use super::*;
use crate::chat::testkit::{Fake, fold, until};

fn fixture(steps: Vec<Value>) -> String {
    steps
        .into_iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}
fn expect(method: &str, params: Value, replies: Vec<Value>) -> Value {
    json!({"type":"expect","frame":{"jsonrpc":"2.0","method":method,"params":params},"reply":replies})
}
fn reply(value: Value) -> Value {
    json!({"jsonrpc":"2.0","id":"$id","result":value})
}
fn update(value: Value) -> Value {
    json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":value}})
}
fn init() -> Value {
    expect(
        "initialize",
        json!({"protocolVersion":1,"clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false},"terminal":false}}),
        vec![reply(
            json!({"protocolVersion":1,"agentCapabilities":{"loadSession":true}}),
        )],
    )
}
fn new() -> Value {
    expect(
        "session/new",
        json!({"mcpServers":[]}),
        vec![reply(json!({"sessionId":"s1"}))],
    )
}
fn prompt(replies: Vec<Value>) -> Value {
    expect("session/prompt", json!({"sessionId":"s1"}), replies)
}
fn permission() -> Value {
    json!({"jsonrpc":"2.0","id":"native-option-request","method":"session/request_permission","params":{"sessionId":"s1","toolCall":{"toolCallId":"tool1","kind":"execute","title":"Run tests"},"options":[{"optionId":"persistent","kind":"allow_always","name":"Always"},{"optionId":"opaque-allow","kind":"allow_once","name":"Yes"},{"optionId":"opaque-no","kind":"reject_once","name":"No"}]}})
}

#[test]
fn native_lifecycle_tools_and_literal_arguments() {
    let text = fixture(vec![
        init(),
        new(),
        prompt(vec![
            update(
                json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Hello "}}),
            ),
            update(
                json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"world"}}),
            ),
            update(
                json!({"sessionUpdate":"tool_call","toolCallId":"tool1","title":"Read file","status":"pending","rawInput":{"path":"a"}}),
            ),
            update(
                json!({"sessionUpdate":"tool_call_update","toolCallId":"tool1","status":"completed","content":[{"type":"content","content":{"type":"text","text":"contents"}}]}),
            ),
            reply(json!({"stopReason":"end_turn"})),
        ]),
    ]);
    let fake = Fake::new(&[&text]);
    let mut config = fake.config(Provider::Acp);
    config.extra_args = vec!["a b".into(), "$(touch never)".into()];
    let (tx, rx) = mpsc::channel();
    let mut driver = start(config, tx).unwrap();
    assert_eq!(driver.provider_thread_id().as_deref(), Some("s1"));
    driver
        .command(ChatCommand::Send { text: "Hi".into() })
        .unwrap();
    let events = until(&rx, |e| {
        matches!(
            e,
            ChatEvent::TurnCompleted {
                outcome: TurnOutcome::Completed,
                ..
            }
        )
    });
    let view = fold(&events);
    assert!(
        view.items
            .iter()
            .any(|i| matches!(&i.body, ItemBody::AgentMessage{text} if text == "Hello world"))
    );
    assert!(view.items.iter().any(|i| matches!(&i.body, ItemBody::ToolCall{input, output:Some(output),..} if input["path"]=="a" && output=="contents") && i.status==ItemStatus::Completed));
    driver.shutdown();
    assert!(!fake.saw("mismatch"), "{:?}", fake.entries());
    assert_eq!(
        fake.entries()[0]["start"]["argv"],
        json!(["a b", "$(touch never)"])
    );
    assert!(!fake.dir.join("never").exists());
}

#[test]
fn permission_uses_native_once_id_without_persistent_grant() {
    let text = fixture(vec![
        init(),
        new(),
        prompt(vec![permission()]),
        json!({"type":"expect","frame":{"id":"native-option-request","result":{"outcome":{"outcome":"selected","optionId":"opaque-allow"}}},"reply":[{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}]}),
    ]);
    let fake = Fake::new(&[&text]);
    let (tx, rx) = mpsc::channel();
    let mut driver = start(fake.config(Provider::Acp), tx).unwrap();
    driver
        .command(ChatCommand::Send {
            text: "test".into(),
        })
        .unwrap();
    let events = until(&rx, |e| matches!(e, ChatEvent::ApprovalRequested { .. }));
    let approval = events
        .iter()
        .find_map(|e| {
            if let ChatEvent::ApprovalRequested { approval } = e {
                Some(approval)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(
        approval.choices,
        vec![Decision::Accept, Decision::Decline, Decision::Cancel]
    );
    assert!(
        driver
            .command(ChatCommand::Approve {
                request_id: approval.request_id.clone(),
                decision: Decision::AcceptForSession
            })
            .is_err()
    );
    driver
        .command(ChatCommand::Approve {
            request_id: approval.request_id.clone(),
            decision: Decision::Accept,
        })
        .unwrap();
    until(&rx, |e| {
        matches!(
            e,
            ChatEvent::TurnCompleted {
                outcome: TurnOutcome::Completed,
                ..
            }
        )
    });
    driver.shutdown();
    assert!(!fake.saw("mismatch"), "{:?}", fake.entries());
}

#[test]
fn cancel_answers_pending_permissions_then_waits_for_prompt_confirmation() {
    let text = fixture(vec![
        init(),
        new(),
        prompt(vec![permission()]),
        json!({"type":"expect","frame":{"id":"native-option-request","result":{"outcome":{"outcome":"cancelled"}}}}),
        expect(
            "session/cancel",
            json!({"sessionId":"s1"}),
            vec![json!({"jsonrpc":"2.0","id":3,"result":{"stopReason":"cancelled"}})],
        ),
    ]);
    let fake = Fake::new(&[&text]);
    let (tx, rx) = mpsc::channel();
    let mut driver = start(fake.config(Provider::Acp), tx).unwrap();
    driver
        .command(ChatCommand::Send {
            text: "test".into(),
        })
        .unwrap();
    until(&rx, |e| matches!(e, ChatEvent::ApprovalRequested { .. }));
    assert!(
        driver
            .command(ChatCommand::Send {
                text: "no steering".into()
            })
            .is_err()
    );
    driver.command(ChatCommand::Interrupt).unwrap();
    until(&rx, |e| {
        matches!(
            e,
            ChatEvent::TurnCompleted {
                outcome: TurnOutcome::Interrupted,
                ..
            }
        )
    });
    driver.shutdown();
    assert!(!fake.saw("mismatch"), "{:?}", fake.entries());
}

#[test]
fn restart_loads_same_session_without_duplicate_replay_or_prompt_resubmission() {
    let first = fixture(vec![
        init(),
        new(),
        prompt(vec![update(
            json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"partial"}}),
        )]),
        json!({"type":"exit","code":7}),
    ]);
    let second = fixture(vec![
        init(),
        expect(
            "session/load",
            json!({"sessionId":"s1"}),
            vec![
                update(
                    json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"historical replay"}}),
                ),
                reply(json!({})),
            ],
        ),
        prompt(vec![
            update(
                json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"new response"}}),
            ),
            reply(json!({"stopReason":"end_turn"})),
        ]),
    ]);
    let fake = Fake::new(&[&first, &second]);
    let (tx, rx) = mpsc::channel();
    let mut driver = start(fake.config(Provider::Acp), tx).unwrap();
    driver
        .command(ChatCommand::Send {
            text: "first".into(),
        })
        .unwrap();
    until(&rx, |e| {
        matches!(
            e,
            ChatEvent::State {
                state: ChatState::Failed { .. }
            }
        )
    });
    driver.shutdown();
    let mut config = fake.config(Provider::Acp);
    config.resume = Some("s1".into());
    config.resume_has_history = true;
    let (tx, rx) = mpsc::channel();
    let mut driver = start(config, tx).unwrap();
    assert_eq!(
        fake.received_method("session/prompt").len(),
        1,
        "startup never resends the interrupted prompt"
    );
    driver
        .command(ChatCommand::Send {
            text: "next".into(),
        })
        .unwrap();
    let events = until(&rx, |e| matches!(e, ChatEvent::TurnCompleted { .. }));
    assert!(!format!("{events:?}").contains("historical replay"));
    assert!(format!("{events:?}").contains("new response"));
    driver.shutdown();
    assert!(!fake.saw("mismatch"), "{:?}", fake.entries());
    assert_eq!(fake.received_method("session/new").len(), 1);
}

#[test]
fn incompatible_version_and_missing_load_capability_refuse_without_fresh_fallback() {
    for response in [
        json!({"protocolVersion":2}),
        json!({"protocolVersion":1,"agentCapabilities":{}}),
    ] {
        let text = fixture(vec![expect("initialize", json!({}), vec![reply(response)])]);
        let fake = Fake::new(&[&text]);
        let mut config = fake.config(Provider::Acp);
        config.resume = Some("saved-session".into());
        config.resume_has_history = true;
        let (tx, _) = mpsc::channel();
        assert!(start(config, tx).is_err());
        assert!(fake.received_method("session/new").is_empty());
        assert!(fake.received_method("session/load").is_empty());
        assert!(!fake.saw("mismatch"));
    }
}

#[test]
fn requested_model_must_be_advertised_and_set_successfully() {
    let text = fixture(vec![
        init(),
        expect(
            "session/new",
            json!({}),
            vec![reply(
                json!({"sessionId":"s1","models":{"currentModelId":"m1","availableModels":[{"modelId":"m1","name":"One"},{"modelId":"m2","name":"Two"}]}}),
            )],
        ),
        expect(
            "session/set_model",
            json!({"sessionId":"s1","modelId":"m2"}),
            vec![reply(json!({}))],
        ),
    ]);
    let fake = Fake::new(&[&text]);
    let mut config = fake.config(Provider::Acp);
    config.model = Some("m2".into());
    let (tx, rx) = mpsc::channel();
    let mut driver = start(config, tx).unwrap();
    let events = until(&rx, |e| {
        matches!(
            e,
            ChatEvent::State {
                state: ChatState::Idle
            }
        )
    });
    assert!(
        events
            .iter()
            .any(|e| matches!(e,ChatEvent::Models{models} if models.len()==2))
    );
    driver.shutdown();
    assert!(!fake.saw("mismatch"));
}

#[test]
fn client_does_not_execute_unadvertised_file_or_terminal_requests() {
    let text = fixture(vec![
        init(),
        new(),
        prompt(vec![
            json!({"jsonrpc":"2.0","id":99,"method":"fs/write_text_file","params":{"path":"/tmp/should-never-write","content":"bad"}}),
        ]),
        json!({"type":"expect","frame":{"id":99,"error":{"code":-32601}},"reply":[{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}]}),
    ]);
    let fake = Fake::new(&[&text]);
    let (tx, rx) = mpsc::channel();
    let mut driver = start(fake.config(Provider::Acp), tx).unwrap();
    driver
        .command(ChatCommand::Send {
            text: "test".into(),
        })
        .unwrap();
    until(&rx, |e| matches!(e, ChatEvent::TurnCompleted { .. }));
    driver.shutdown();
    assert!(!fake.saw("mismatch"));
}

#[test]
fn malformed_request_identity_fails_without_echoing_or_executing_it() {
    let text = fixture(vec![
        init(),
        new(),
        prompt(vec![
            json!({"jsonrpc":"2.0","id":{"not":"an RPC id"},"method":"session/request_permission","params":{"sessionId":"s1"}}),
        ]),
    ]);
    let fake = Fake::new(&[&text]);
    let (tx, rx) = mpsc::channel();
    let mut driver = start(fake.config(Provider::Acp), tx).unwrap();
    driver
        .command(ChatCommand::Send {
            text: "test".into(),
        })
        .unwrap();
    let events = until(&rx, |e| {
        matches!(
            e,
            ChatEvent::State {
                state: ChatState::Failed { .. }
            }
        )
    });
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, ChatEvent::ApprovalRequested { .. }))
    );
    driver.shutdown();
    assert!(!fake.received().iter().any(|frame| frame["id"].is_object()));
}

#[test]
fn real_host_persists_acp_identity_events_and_loads_after_restart() {
    use crate::chat::{
        host::Providers,
        testing::{TestHost, quick_options},
    };
    let first = fixture(vec![
        init(),
        new(),
        prompt(vec![
            update(
                json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"first answer"}}),
            ),
            reply(json!({"stopReason":"end_turn"})),
        ]),
    ]);
    let second = fixture(vec![
        init(),
        expect(
            "session/load",
            json!({"sessionId":"s1"}),
            vec![
                update(
                    json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"first answer"}}),
                ),
                reply(json!({})),
            ],
        ),
        prompt(vec![reply(json!({"stopReason":"end_turn"}))]),
    ]);
    let fake = Fake::new(&[&first, &second]);
    let mut host = TestHost::with_providers(quick_options(), Providers::system());
    crate::chat::acp_config::save(
        &host.home,
        &crate::chat::acp_config::Config {
            command: fake.program.clone(),
            args: vec![],
        },
    )
    .unwrap();
    let chat = host.create(Provider::Acp);
    host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
    host.client()
        .command(
            &chat.id,
            ChatCommand::Send {
                text: "first".into(),
            },
        )
        .unwrap();
    host.wait_for_log(&chat.id, |log| {
        log.iter()
            .any(|e| matches!(e.event, ChatEvent::TurnCompleted { .. }))
    });
    assert_eq!(
        host.info(&chat.id).provider_thread_id.as_deref(),
        Some("s1")
    );
    host.restart(quick_options());
    // Changing the global executable does not change the old chat's pinned agent.
    crate::chat::acp_config::save(
        &host.home,
        &crate::chat::acp_config::Config {
            command: "/bin/false".into(),
            args: vec![],
        },
    )
    .unwrap();
    host.client()
        .command(
            &chat.id,
            ChatCommand::Send {
                text: "next".into(),
            },
        )
        .unwrap();
    let log = host.wait_for_log(&chat.id, |log| {
        log.iter()
            .filter(|e| matches!(e.event, ChatEvent::TurnCompleted { .. }))
            .count()
            == 2
    });
    assert_eq!(log.iter().filter(|e|matches!(&e.event,ChatEvent::ItemCompleted{item:Item{body:ItemBody::AgentMessage{text},..}} if text=="first answer")).count(),1);
    host.stop();
    assert_eq!(fake.received_method("session/new").len(), 1);
    assert_eq!(fake.received_method("session/load").len(), 1);
    assert!(!fake.saw("mismatch"), "{:?}", fake.entries());
}
