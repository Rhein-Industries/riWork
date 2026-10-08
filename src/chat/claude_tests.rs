use super::*;
use crate::chat::model::{Provider, Transcript};
use crate::chat::testkit::{Fake, fixture, fold, until};
use std::sync::mpsc::Receiver;

#[test]
fn attachment_ordinary_user_echo_retains_text_and_images_with_replay_identity() {
    let (sender, events) = mpsc::channel();
    let mut core = Core::new(&bare_config(), sender, "session".into());
    let frame = json!({"type":"user","uuid":"replayed-user","message":{"content":[
        {"type":"text","text":"actual echoed text 🦀"},
        {"type":"image","source":{"type":"base64","media_type":"image/png","data":"aGVsbG8="}}
    ]}});
    core.on_user(&frame);
    core.on_user(&frame);
    let transcript = fold(&events.try_iter().collect::<Vec<_>>());
    assert_eq!(transcript.items.len(), 1);
    assert!(
        matches!(&transcript.items[0].body, ItemBody::UserMessage { text } if text.contains("actual echoed text 🦀"))
    );
    assert_eq!(transcript.items[0].presentation.images.len(), 1);
}

/// Short waits, so the escalation paths take milliseconds.
fn fast() -> Tuning {
    Tuning {
        handshake: Duration::from_secs(20),
        interrupt_grace: Duration::from_millis(150),
        interrupt_kill: Duration::from_millis(150),
        inactivity: Duration::from_secs(600),
        tick: Duration::from_millis(20),
        stop_grace: Duration::from_secs(1),
        term_wait: Duration::from_millis(300),
    }
}

/// A driver running against the fake `claude`, and the events it has sent.
struct Rig {
    // The driver goes first, so the process is gone before its directory.
    driver: Box<dyn Driver>,
    fake: Fake,
    events: Receiver<ChatEvent>,
    seen: Vec<ChatEvent>,
}

impl Rig {
    fn new(fixtures: &[&str]) -> Self {
        Self::with(fixtures, fast(), |_| {})
    }

    fn with(fixtures: &[&str], tuning: Tuning, adjust: impl FnOnce(&mut DriverConfig)) -> Self {
        let texts: Vec<String> = fixtures
            .iter()
            .map(|name| fixture(&format!("claude/{name}.ndjson")))
            .collect();
        let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
        let fake = Fake::new(&texts);
        let mut config = fake.config(Provider::Claude);
        config.resume = Some("sess-1".into());
        adjust(&mut config);
        let (sender, events) = mpsc::channel();
        let driver = start_with(config, sender, tuning).expect("the fake claude starts");
        let mut rig = Rig {
            driver,
            fake,
            events,
            seen: Vec::new(),
        };
        rig.until(is_idle);
        rig
    }

    fn until(&mut self, done: impl Fn(&ChatEvent) -> bool) -> Vec<ChatEvent> {
        let events = until(&self.events, done);
        self.seen.extend(events.iter().cloned());
        events
    }

    /// Everything up to the next time the chat is idle.
    fn until_idle(&mut self) -> Vec<ChatEvent> {
        self.until(is_idle)
    }

    fn send(&mut self, text: &str) {
        self.command(ChatCommand::Send { text: text.into() });
    }

    fn command(&mut self, command: ChatCommand) {
        self.driver
            .command(command)
            .expect("the command is accepted");
    }

    fn transcript(&self) -> Transcript {
        fold(&self.seen)
    }

    fn states(&self) -> Vec<ChatState> {
        states(&self.seen)
    }
}

fn is_idle(event: &ChatEvent) -> bool {
    matches!(
        event,
        ChatEvent::State {
            state: ChatState::Idle
        }
    )
}

fn states(events: &[ChatEvent]) -> Vec<ChatState> {
    events
        .iter()
        .filter_map(|event| match event {
            ChatEvent::State { state } => Some(state.clone()),
            _ => None,
        })
        .collect()
}

fn wait_for(what: &str, condition: impl Fn() -> bool) {
    for _ in 0..500 {
        if condition() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for {what}");
}

fn argv(start: &Value) -> Vec<String> {
    start["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|arg| arg.as_str().unwrap().to_owned())
        .collect()
}

fn has_pair(args: &[String], flag: &str, value: &str) -> bool {
    args.windows(2)
        .any(|pair| pair[0] == flag && pair[1] == value)
}

fn item<'a>(transcript: &'a Transcript, id: &str) -> &'a Item {
    transcript
        .items
        .iter()
        .find(|item| item.id == id)
        .unwrap_or_else(|| panic!("no item {id} in {:#?}", transcript.items))
}

fn agent_texts(transcript: &Transcript) -> Vec<&str> {
    transcript
        .items
        .iter()
        .filter_map(|item| match &item.body {
            ItemBody::AgentMessage { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn notices(events: &[ChatEvent]) -> Vec<(NoticeLevel, String)> {
    events
        .iter()
        .filter_map(|event| match event {
            ChatEvent::ItemCompleted {
                item:
                    Item {
                        body: ItemBody::Notice { level, text, .. },
                        ..
                    },
            } => Some((*level, text.clone())),
            _ => None,
        })
        .collect()
}

fn outcomes(events: &[ChatEvent]) -> Vec<TurnOutcome> {
    events
        .iter()
        .filter_map(|event| match event {
            ChatEvent::TurnCompleted { outcome, .. } => Some(outcome.clone()),
            _ => None,
        })
        .collect()
}

fn approvals(events: &[ChatEvent]) -> Vec<Approval> {
    events
        .iter()
        .filter_map(|event| match event {
            ChatEvent::ApprovalRequested { approval } => Some(approval.clone()),
            _ => None,
        })
        .collect()
}

fn is_approval(event: &ChatEvent) -> bool {
    matches!(event, ChatEvent::ApprovalRequested { .. })
}

// The whole driver, against the fake.

#[test]
fn a_turn_streams_text_runs_tools_and_asks_for_approval() {
    let mut rig = Rig::new(&["turn"]);
    assert_eq!(rig.driver.provider_thread_id().as_deref(), Some("sess-1"));
    assert_eq!(
        rig.seen,
        vec![
            ChatEvent::State {
                state: ChatState::Starting
            },
            ChatEvent::State {
                state: ChatState::Idle
            }
        ]
    );

    rig.send("Fix the greeting");
    rig.until(is_approval);
    let bash = approvals(&rig.seen).remove(0);
    assert_eq!(bash.request_id, "perm-1");
    assert_eq!(bash.kind, ApprovalKind::Command);
    assert_eq!(bash.title, "printf 'hi\\n' > out.txt && cat out.txt");
    assert_eq!(bash.item_id.as_deref(), Some("toolu_bash"));
    assert_eq!(
        bash.choices,
        vec![
            Decision::Accept,
            Decision::AcceptForSession,
            Decision::Decline
        ]
    );
    rig.command(ChatCommand::Approve {
        request_id: "perm-1".into(),
        decision: Decision::Accept,
    });

    rig.until(|event| {
        matches!(event, ChatEvent::ApprovalRequested { approval } if approval.request_id == "perm-2")
    });
    let edit = approvals(&rig.seen).remove(1);
    assert_eq!(edit.kind, ApprovalKind::FileChange);
    assert_eq!(edit.title, "/work/greeting.txt");
    assert!(
        edit.detail.contains("-hello\n+hello, world"),
        "{}",
        edit.detail
    );
    rig.command(ChatCommand::Approve {
        request_id: "perm-2".into(),
        decision: Decision::Decline,
    });
    rig.until_idle();

    // What the CLI was told.
    let sent = rig.fake.received();
    assert_eq!(sent[1]["message"]["content"], "Fix the greeting");
    let answer = |id: &str| {
        sent.iter()
            .find(|frame| frame["response"]["request_id"] == id)
            .unwrap_or_else(|| panic!("no answer to {id}"))["response"]["response"]
            .clone()
    };
    assert_eq!(answer("perm-1")["behavior"], "allow");
    assert_eq!(answer("perm-1")["toolUseID"], "toolu_bash");
    assert_eq!(answer("perm-2")["behavior"], "deny");

    // The states a tab shows.
    assert_eq!(
        rig.states(),
        vec![
            ChatState::Starting,
            ChatState::Idle,
            ChatState::Running,
            ChatState::Waiting,
            ChatState::Running,
            ChatState::Waiting,
            ChatState::Running,
            ChatState::Idle,
        ]
    );

    // The text arrived as deltas before it was complete.
    let deltas: Vec<&str> = rig
        .seen
        .iter()
        .filter_map(|event| match event {
            ChatEvent::ItemDelta {
                item_id,
                delta: crate::chat::model::Delta::Text(text),
            } if item_id == "msg_a:1" => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(deltas, ["I'll ", "check the file."]);
    let started = rig
        .seen
        .iter()
        .position(|event| matches!(event, ChatEvent::ItemStarted { item } if item.id == "msg_a:1"))
        .unwrap();
    let first_delta = rig
        .seen
        .iter()
        .position(
            |event| matches!(event, ChatEvent::ItemDelta { item_id, .. } if item_id == "msg_a:1"),
        )
        .unwrap();
    assert!(started < first_delta);

    // What a tab draws.
    let transcript = rig.transcript();
    let kinds: Vec<&str> = transcript
        .items
        .iter()
        .map(|item| match &item.body {
            ItemBody::UserMessage { .. } => "user",
            ItemBody::AgentMessage { .. } => "agent",
            ItemBody::Reasoning { .. } => "reasoning",
            ItemBody::ToolCall { .. } => "tool",
            ItemBody::Command { .. } => "command",
            ItemBody::FileChange { .. } => "file",
            _ => "other",
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "user",
            "reasoning",
            "agent",
            "tool",
            "command",
            "file",
            "agent",
            "agent"
        ]
    );
    assert!(transcript.items.iter().all(|item| item.turn_id.is_some()));
    assert_eq!(
        item(&transcript, "msg_a:0").body,
        ItemBody::Reasoning {
            text: "Look at the file".into()
        }
    );
    assert_eq!(
        item(&transcript, "msg_a:1").body,
        ItemBody::AgentMessage {
            text: "I'll check the file.".into()
        }
    );
    let read = item(&transcript, "toolu_read");
    assert_eq!(read.status, ItemStatus::Completed);
    assert_eq!(
        read.body,
        ItemBody::ToolCall {
            server: None,
            tool: "Read".into(),
            input: json!({"file_path": "/work/greeting.txt"}),
            output: Some("     1\thello\n".into()),
        }
    );
    let command = item(&transcript, "toolu_bash");
    assert_eq!(command.status, ItemStatus::Completed);
    assert_eq!(
        command.body,
        ItemBody::Command {
            command: "printf 'hi\\n' > out.txt && cat out.txt".into(),
            cwd: None,
            output: "hi\n".into(),
            exit_code: Some(0),
        }
    );
    let edit = item(&transcript, "toolu_edit");
    assert_eq!(edit.status, ItemStatus::Declined);
    assert!(matches!(&edit.body, ItemBody::FileChange { changes }
        if changes[0].kind == ChangeKind::Modify && changes[0].path == "/work/greeting.txt"));
    assert!(transcript.approvals.is_empty());
    assert_eq!(transcript.state, ChatState::Idle);
    assert_eq!(outcomes(&rig.seen), vec![TurnOutcome::Completed]);
    // Everything the turn left open is closed.
    assert!(
        transcript
            .items
            .iter()
            .all(|item| item.status != ItemStatus::InProgress)
    );

    // The usage of the turn: tokens as the CLI counts them, cost as it reports it.
    assert_eq!(
        transcript.usage,
        Some(Usage {
            input_tokens: 7 + 100 + 900,
            output_tokens: 61,
            cached_input_tokens: 900,
            context_window: Some(200_000),
            context_used: Some(7 + 100 + 900 + 61),
            cost_usd: Some(0.0123),
        })
    );
}

#[test]
fn accepting_for_the_session_sends_the_clis_own_suggestion_back() {
    let mut rig = Rig::new(&["session_approval"]);
    rig.send("Write a file");
    rig.until(is_approval);
    let approval = approvals(&rig.seen).remove(0);
    assert_eq!(approval.kind, ApprovalKind::FileChange);
    assert!(
        approval.detail.contains("--- /dev/null"),
        "{}",
        approval.detail
    );
    rig.command(ChatCommand::Approve {
        request_id: "perm-9".into(),
        decision: Decision::AcceptForSession,
    });
    // The fake only goes on if the reply carried `updatedPermissions`.
    rig.until_idle();
    let transcript = rig.transcript();
    assert!(matches!(
        &item(&transcript, "toolu_w").body,
        ItemBody::FileChange { changes } if changes[0].kind == ChangeKind::Add
    ));
    assert!(rig.seen.iter().any(|event| matches!(
        event,
        ChatEvent::ApprovalResolved { request_id, decision: Decision::AcceptForSession }
            if request_id == "perm-9"
    )));
    assert!(!rig.fake.saw("mismatch"));
}

#[test]
fn a_question_is_asked_and_the_answers_go_back_keyed_by_question() {
    let mut rig = Rig::new(&["question"]);
    rig.send("Pick a library");
    let events = rig.until(|event| matches!(event, ChatEvent::QuestionRequested { .. }));
    let Some(ChatEvent::QuestionRequested { question }) = events.last() else {
        unreachable!()
    };
    assert_eq!(question.request_id, "ask-1");
    assert_eq!(question.questions.len(), 2);
    assert_eq!(question.questions[0].header.as_deref(), Some("Library"));
    assert_eq!(question.questions[0].question, "Which date library?");
    assert_eq!(
        question.questions[0].options,
        vec![
            QuestionOption {
                label: "chrono".into(),
                description: "The usual one".into()
            },
            QuestionOption {
                label: "time".into(),
                description: "Smaller".into()
            },
        ]
    );
    assert!(!question.questions[0].multi_select && question.questions[1].multi_select);
    rig.until(|event| {
        matches!(
            event,
            ChatEvent::State {
                state: ChatState::Waiting
            }
        )
    });
    // An approval id is not a question and the other way round.
    assert!(
        rig.driver
            .command(ChatCommand::Approve {
                request_id: "ask-1".into(),
                decision: Decision::Accept
            })
            .is_err()
    );
    rig.command(ChatCommand::Answer {
        request_id: "ask-1".into(),
        answers: vec![vec!["chrono".into()], vec!["serde".into(), "clock".into()]],
    });
    rig.until_idle();
    assert!(rig.transcript().questions.is_empty());
    assert!(rig.seen.contains(&ChatEvent::QuestionResolved {
        request_id: "ask-1".into()
    }));
    assert!(!rig.fake.saw("mismatch"));
}

#[test]
fn a_withdrawn_prompt_is_resolved_and_an_unknown_request_gets_an_error() {
    let mut rig = Rig::new(&["withdrawn"]);
    rig.send("go");
    rig.until_idle();
    assert!(rig.seen.contains(&ChatEvent::ApprovalResolved {
        request_id: "perm-5".into(),
        decision: Decision::Cancel
    }));
    assert!(
        rig.driver
            .command(ChatCommand::Approve {
                request_id: "perm-5".into(),
                decision: Decision::Accept
            })
            .is_err()
    );

    let mut rig = Rig::new(&["unsupported_request"]);
    rig.send("go");
    rig.until_idle();
    assert!(!rig.fake.saw("mismatch"));
}

#[test]
fn an_interrupt_the_cli_honours_ends_the_turn_as_interrupted() {
    let mut rig = Rig::new(&["interrupt_ack"]);
    rig.send("Run for a while");
    rig.until(|event| matches!(event, ChatEvent::ItemDelta { .. }));
    rig.command(ChatCommand::Interrupt);
    rig.until_idle();
    assert_eq!(outcomes(&rig.seen), vec![TurnOutcome::Interrupted]);
    // No failure notice for the error result an interrupt produces.
    assert!(notices(&rig.seen).is_empty());
    let transcript = rig.transcript();
    assert!(
        transcript
            .items
            .iter()
            .all(|item| item.status != ItemStatus::InProgress)
    );
    assert_eq!(
        rig.fake.received()[2],
        json!({"type": "control_request", "request_id": rig.fake.received()[2]["request_id"],
               "request": {"subtype": "interrupt", "cancel_queued": true}})
    );
    // Nothing is running now, so another interrupt is a no-op.
    rig.command(ChatCommand::Interrupt);
    assert_eq!(rig.fake.received().len(), 3);
}

#[test]
fn an_interrupt_the_cli_ignores_escalates_to_sigint_and_a_restart_that_resumes() {
    let tuning = Tuning {
        stop_grace: Duration::from_millis(200),
        ..fast()
    };
    let mut rig = Rig::with(
        &["interrupt_ignored_1", "interrupt_ignored_2"],
        tuning,
        |_| {},
    );
    rig.send("Run for a while");
    rig.command(ChatCommand::Interrupt);
    rig.until_idle();
    assert_eq!(outcomes(&rig.seen), vec![TurnOutcome::Interrupted]);
    assert_eq!(
        rig.states(),
        vec![
            ChatState::Starting,
            ChatState::Idle,
            ChatState::Running,
            ChatState::Starting,
            ChatState::Idle
        ]
    );

    let entries = rig.fake.entries();
    assert!(
        entries.contains(&json!({"signal": "INT", "ignored": true})),
        "{entries:?}"
    );
    let starts = rig.fake.starts();
    assert_eq!(starts.len(), 2);
    assert!(has_pair(&argv(&starts[1]), "--resume", "sess-1"));
    assert!(!argv(&starts[1]).contains(&"--session-id".to_owned()));

    // The chat works on the new process.
    rig.send("Carry on");
    rig.until_idle();
    assert!(agent_texts(&rig.transcript()).contains(&"Back again."));
    assert_eq!(
        rig.fake.received().last().unwrap()["message"]["content"],
        "Carry on"
    );
}

#[test]
fn cancelling_at_a_prompt_declines_and_interrupts_in_one_reply() {
    let mut rig = Rig::new(&["cancel"]);
    rig.send("go");
    rig.until(is_approval);
    rig.command(ChatCommand::Approve {
        request_id: "perm-1".into(),
        decision: Decision::Cancel,
    });
    rig.until_idle();
    assert!(!rig.fake.saw("mismatch"));
    assert_eq!(outcomes(&rig.seen), vec![TurnOutcome::Interrupted]);
    assert_eq!(
        item(&rig.transcript(), "toolu_b").status,
        ItemStatus::Declined
    );
}

#[test]
fn an_interrupt_at_a_prompt_answers_the_prompt_before_asking_to_stop() {
    let mut rig = Rig::new(&["interrupt_at_prompt"]);
    rig.send("go");
    rig.until(is_approval);
    rig.command(ChatCommand::Interrupt);
    rig.until_idle();
    assert!(!rig.fake.saw("mismatch"));
    assert_eq!(outcomes(&rig.seen), vec![TurnOutcome::Interrupted]);
    assert!(rig.seen.contains(&ChatEvent::ApprovalResolved {
        request_id: "perm-1".into(),
        decision: Decision::Cancel
    }));
    assert_eq!(
        item(&rig.transcript(), "toolu_b").status,
        ItemStatus::Interrupted
    );
}

#[test]
fn a_message_sent_while_the_process_restarts_waits_for_the_new_one() {
    let mut rig = Rig::new(&["idle_only", "queued_2"]);
    rig.command(ChatCommand::Configure {
        model: None,
        effort: Some("low".into()),
        approval_mode: None,
        fast: None,
    });
    // The restart is claimed by now, so this does not go to the old process.
    rig.send("after");
    rig.command(ChatCommand::Compact);
    rig.until_idle();
    rig.until_idle();
    assert_eq!(rig.fake.starts().len(), 2);
    let transcript = rig.transcript();
    assert_eq!(agent_texts(&transcript), ["Received.", "done"]);
    // The message is echoed once it is sent; the queued /compact never is.
    let said: Vec<&str> = transcript
        .items
        .iter()
        .filter_map(|item| match &item.body {
            ItemBody::UserMessage { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(said, ["after"]);
    wait_for("the queued /compact", || rig.fake.received().len() == 4);
    let received = rig.fake.received();
    assert_eq!(received[2]["message"]["content"], "after");
    assert_eq!(received[3]["message"]["content"], "/compact");
    // Nothing reached the first process but its handshake.
    assert_eq!(
        received
            .iter()
            .filter(|frame| frame["message"]["content"] == "after")
            .count(),
        1
    );
}

#[test]
fn a_setting_goes_to_the_running_process_as_a_control_request() {
    let mut rig = Rig::new(&["configure_live"]);
    rig.command(ChatCommand::Configure {
        model: Some("opus".into()),
        effort: None,
        approval_mode: Some(ApprovalMode::AutoEdit),
        fast: None,
    });
    wait_for("both requests", || rig.fake.received().len() == 3);
    let sent = rig.fake.received();
    assert_eq!(sent[1]["request"]["subtype"], "set_permission_mode");
    assert_eq!(sent[1]["request"]["mode"], "acceptEdits");
    assert_eq!(sent[2]["request"]["subtype"], "set_model");
    assert_eq!(sent[2]["request"]["model"], "opus");
    assert_eq!(rig.fake.starts().len(), 1);
    assert!(!rig.fake.saw("mismatch"));
    // The same value again asks for nothing.
    rig.command(ChatCommand::Configure {
        model: Some("opus".into()),
        effort: None,
        approval_mode: Some(ApprovalMode::AutoEdit),
        fast: None,
    });
    thread::sleep(Duration::from_millis(100));
    assert_eq!(rig.fake.received().len(), 3);
}

#[test]
fn a_setting_the_cli_cannot_change_restarts_it_with_the_new_flags() {
    let mut rig = Rig::new(&["configure_unsupported_1", "configure_unsupported_2"]);
    rig.command(ChatCommand::Configure {
        model: Some("opus".into()),
        effort: None,
        approval_mode: None,
        fast: None,
    });
    rig.until(|event| {
        matches!(
            event,
            ChatEvent::State {
                state: ChatState::Starting
            }
        )
    });
    rig.until_idle();
    let starts = rig.fake.starts();
    assert_eq!(starts.len(), 2);
    assert!(!argv(&starts[0]).contains(&"--model".to_owned()));
    assert!(has_pair(&argv(&starts[1]), "--model", "opus"));
    assert!(has_pair(&argv(&starts[1]), "--resume", "sess-1"));
    // The first process was asked to leave by closing its input.
    wait_for("the old process to see its input close", || {
        rig.fake.saw("eof")
    });
}

#[test]
fn an_effort_change_restarts_the_process_and_a_refused_model_does_not() {
    let mut rig = Rig::new(&["idle_only", "idle_only_2"]);
    rig.command(ChatCommand::Configure {
        model: None,
        effort: Some("high".into()),
        approval_mode: None,
        fast: None,
    });
    rig.until(|event| {
        matches!(
            event,
            ChatEvent::State {
                state: ChatState::Starting
            }
        )
    });
    rig.until_idle();
    let starts = rig.fake.starts();
    assert!(!argv(&starts[0]).contains(&"--effort".to_owned()));
    assert!(has_pair(&argv(&starts[1]), "--effort", "high"));

    let mut rig = Rig::new(&["configure_refused"]);
    rig.command(ChatCommand::Configure {
        model: Some("nonsense".into()),
        effort: None,
        approval_mode: None,
        fast: None,
    });
    let events = rig.until(|event| {
        matches!(event, ChatEvent::ItemCompleted { item } if matches!(item.body, ItemBody::Notice { .. }))
    });
    assert_notice(
        &completed_notices(&events)[0],
        notice_kind::SETTING_REFUSED,
        false,
    );
    let notices = notices(&events);
    assert_eq!(notices[0].0, NoticeLevel::Warning);
    assert!(
        notices[0].1.contains("Unknown model: nonsense"),
        "{notices:?}"
    );
    assert_eq!(rig.fake.starts().len(), 1);
    // The refusal put the old value back: asking again asks again.
    rig.command(ChatCommand::Configure {
        model: Some("nonsense".into()),
        effort: None,
        approval_mode: None,
        fast: None,
    });
    wait_for("the second request", || rig.fake.received().len() == 3);
}

fn models_events(events: &[ChatEvent]) -> Vec<Vec<ModelOption>> {
    events
        .iter()
        .filter_map(|event| match event {
            ChatEvent::Models { models } => Some(models.clone()),
            _ => None,
        })
        .collect()
}

fn configure_fast(fast: bool) -> ChatCommand {
    ChatCommand::Configure {
        model: None,
        effort: None,
        approval_mode: None,
        fast: Some(fast),
    }
}

fn is_notice(event: &ChatEvent) -> bool {
    matches!(event, ChatEvent::ItemCompleted { item } if matches!(item.body, ItemBody::Notice { .. }))
}

#[test]
fn the_models_in_the_initialize_answer_become_a_models_event() {
    let rig = Rig::with(&["models_fast"], fast(), |_| {});
    let lists = models_events(&rig.seen);
    assert_eq!(lists.len(), 1, "{:?}", rig.seen);
    let models = &lists[0];
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["default", "opus", "fable", "sonnet", "haiku"]);
    assert_eq!(
        models[0],
        ModelOption {
            id: "default".into(),
            name: "Default (recommended)".into(),
            description: "Use the default model (currently Opus 5.5) \u{b7} $4/$20 per Mtok".into(),
            efforts: ["low", "medium", "high", "xhigh", "max"]
                .map(str::to_owned)
                .into(),
            default_effort: None,
            supports_fast: true,
            is_default: true,
        }
    );
    // Fast mode is the CLI's word for two models; a model without the field has none.
    let fast_models: Vec<&str> = models
        .iter()
        .filter(|m| m.supports_fast)
        .map(|m| m.id.as_str())
        .collect();
    assert_eq!(fast_models, ["default", "opus"]);
    assert_eq!(models.iter().filter(|m| m.is_default).count(), 1);
    // Haiku takes no effort at all.
    assert!(models[4].efforts.is_empty());
    assert_eq!(models[3].name, "Sonnet");
    // A CLI that names no models says nothing.
    let rig = Rig::new(&["idle_only"]);
    assert!(models_events(&rig.seen).is_empty());
}

#[test]
fn fast_mode_is_in_the_launch_settings_only_when_asked_for() {
    let on = Rig::with(&["models_fast"], fast(), |config| config.fast = true);
    let args = argv(&on.fake.starts()[0]);
    assert!(
        has_pair(&args, "--settings", r#"{"fastMode":true}"#),
        "{args:?}"
    );
    let off = Rig::new(&["idle_only"]);
    assert!(!argv(&off.fake.starts()[0]).contains(&"--settings".to_owned()));
    // Nothing is said when it is on, as asked.
    assert!(notices(&on.seen).is_empty());
}

#[test]
fn fast_mode_is_changed_in_the_running_process_and_a_state_that_is_not_on_is_said_once() {
    let mut rig = Rig::with(&["models_fast"], fast(), |config| config.fast = true);
    rig.command(configure_fast(false));
    wait_for("the state to be asked for", || {
        rig.fake.received().len() == 3
    });
    let sent = rig.fake.received();
    assert_eq!(sent[1]["request"]["subtype"], "apply_flag_settings");
    assert_eq!(sent[1]["request"]["settings"], json!({"fastMode": false}));
    assert_eq!(sent[2]["request"]["subtype"], "initialize");
    // The same again asks for nothing.
    rig.command(configure_fast(false));
    // Off was asked for, so "off" is no news. On is asked for, and the CLI says it is cooling
    // down after a rate limit.
    rig.command(configure_fast(true));
    let events = rig.until(is_notice);
    let said = notices(&events);
    assert_eq!(said.len(), 1, "{said:?}");
    assert_eq!(said[0].0, NoticeLevel::Warning);
    assert!(
        said[0].1.contains("cooling down after a rate limit"),
        "{said:?}"
    );
    assert_eq!(rig.fake.received().len(), 5);
    assert_eq!(
        rig.fake.received()[3]["request"]["settings"],
        json!({"fastMode": true})
    );
    // A turn repeats the state in `system/init` and in `result`: nothing new. Then it is on again.
    rig.send("go");
    rig.until_idle();
    assert_eq!(notices(&rig.seen).len(), 1, "{:?}", notices(&rig.seen));
    rig.send("again");
    rig.until_idle();
    let said = notices(&rig.seen);
    assert_eq!(said.len(), 2, "{said:?}");
    assert_eq!(
        (said[1].0, said[1].1.as_str()),
        (NoticeLevel::Info, "Fast mode is on again.")
    );
    // The model list was not repeated by the questions asked on the way.
    assert_eq!(models_events(&rig.seen).len(), 1);
    assert!(!rig.fake.saw("mismatch"));
}

#[test]
fn a_cli_that_cannot_change_fast_mode_is_restarted_with_it_in_the_settings() {
    let mut rig = Rig::new(&["fast_unsupported_1", "fast_unsupported_2"]);
    rig.command(configure_fast(true));
    rig.until(|event| {
        matches!(
            event,
            ChatEvent::State {
                state: ChatState::Starting
            }
        )
    });
    rig.until_idle();
    let starts = rig.fake.starts();
    assert_eq!(starts.len(), 2);
    assert!(!argv(&starts[0]).contains(&"--settings".to_owned()));
    assert!(has_pair(
        &argv(&starts[1]),
        "--settings",
        r#"{"fastMode":true}"#
    ));
    assert!(has_pair(&argv(&starts[1]), "--resume", "sess-1"));
    assert!(!rig.fake.saw("mismatch"));
}

#[test]
fn a_model_without_fast_mode_does_not_make_the_chat_complain_about_it() {
    let mut rig = Rig::with(&["fast_model_without"], fast(), |config| config.fast = true);
    rig.command(ChatCommand::Configure {
        model: Some("sonnet".into()),
        effort: None,
        approval_mode: None,
        fast: None,
    });
    rig.send("go");
    rig.until_idle();
    assert!(notices(&rig.seen).is_empty(), "{:?}", notices(&rig.seen));
    assert!(!rig.fake.saw("mismatch"));
}

#[test]
fn every_reason_fast_mode_is_off_has_its_own_words() {
    let said = |state, reason| fast_off_text(state, reason);
    assert!(said("cooldown", None).contains("cooling down after a rate limit"));
    // A cooldown is a state, not a reason: whatever reason comes with it, it is a cooldown.
    assert!(said("cooldown", Some("unknown")).contains("cooling down"));
    for (reason, words) in [
        ("free", "paid Claude subscription"),
        ("preference", "organization has turned off"),
        ("extra_usage_disabled", "extra usage"),
        ("network_error", "network"),
        ("not_first_party", "Anthropic API"),
        ("disabled_by_env", "environment"),
        ("model_not_allowed", "does not allow this model"),
        ("sdk_opt_in_required", "did not turn on"),
        ("unknown", "unavailable"),
    ] {
        assert!(said("off", Some(reason)).contains(words), "{reason}");
    }
    assert!(said("off", None).contains("for this model"));
    assert_eq!(
        said("off", Some("brand_new")),
        "Fast mode is off (brand_new)"
    );
}

#[test]
fn a_session_starts_with_its_own_id_or_resumes_one_and_never_sees_an_api_key() {
    // A new chat.
    let text = fixture("claude/idle_only.ndjson");
    let fake = Fake::new(&[text.as_str()]);
    let (sender, _events) = mpsc::channel();
    let mut config = fake.config(Provider::Claude);
    config.env = vec![("ANTHROPIC_API_KEY".into(), "sk-ant-test".into())];
    let mut driver = start_with(config, sender, fast()).unwrap();
    let id = driver.provider_thread_id().unwrap();
    assert!(Uuid::parse_str(&id).is_ok(), "{id}");
    let start = fake.starts().remove(0);
    let args = argv(&start);
    assert!(has_pair(&args, "--session-id", &id));
    assert!(!args.contains(&"--resume".to_owned()));
    assert!(has_pair(&args, "--permission-mode", "default"));
    assert_eq!(start["env"]["CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS"], "1");
    assert!(start["env"].get("ANTHROPIC_API_KEY").is_none(), "{start}");
    assert_eq!(start["cwd"], fake.dir.to_str().unwrap());
    driver.shutdown();

    // A resumed one, with everything configured.
    let fake = Fake::new(&[text.as_str()]);
    let (sender, _events) = mpsc::channel();
    let mut config = fake.config(Provider::Claude);
    config.resume = Some("sess-9".into());
    config.approval_mode = ApprovalMode::Full;
    config.model = Some("sonnet".into());
    config.effort = Some("max".into());
    config.extra_args = vec!["--mcp-config".into(), "{}".into()];
    config.env = vec![("CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS".into(), "0".into())];
    let mut driver = start_with(config, sender, fast()).unwrap();
    assert_eq!(driver.provider_thread_id().as_deref(), Some("sess-9"));
    let start = fake.starts().remove(0);
    assert_eq!(
        argv(&start),
        [
            "--mcp-config",
            "{}",
            "--output-format",
            "stream-json",
            "--verbose",
            "--input-format",
            "stream-json",
            "--permission-prompt-tool",
            "stdio",
            "--include-partial-messages",
            "--allow-dangerously-skip-permissions",
            "--permission-mode",
            "bypassPermissions",
            "--model",
            "sonnet",
            "--effort",
            "max",
            "--resume",
            "sess-9",
        ]
    );
    assert_eq!(start["env"]["CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS"], "1");
    driver.shutdown();
}

#[test]
fn the_cost_is_the_latest_total_and_the_tokens_add_up_per_turn() {
    let mut rig = Rig::new(&["usage"]);
    rig.send("one");
    rig.until_idle();
    rig.send("two");
    rig.until_idle();
    let usages: Vec<&Usage> = rig
        .seen
        .iter()
        .filter_map(|event| match event {
            ChatEvent::Usage { usage } => Some(usage),
            _ => None,
        })
        .collect();
    assert_eq!(usages.len(), 2);
    assert_eq!(usages[0].cost_usd, Some(0.01));
    assert_eq!(usages[1].cost_usd, Some(0.015));
    assert_eq!(usages[0].input_tokens, 3 + 50 + 100);
    assert_eq!(usages[1].input_tokens, 3 + 50 + 100 + 4 + 200);
    assert_eq!(usages[1].output_tokens, 22);
    assert_eq!(usages[1].cached_input_tokens, 300);
    assert_eq!(usages[1].context_used, Some(4 + 200 + 12));
    assert_eq!(rig.transcript().usage.as_ref(), Some(usages[1]));
}

#[test]
fn an_oversized_line_is_skipped_with_a_notice_and_the_turn_goes_on() {
    let mut rig = Rig::new(&["oversized"]);
    rig.send("go");
    rig.until_idle();
    let notices = notices(&rig.seen);
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].0, NoticeLevel::Warning);
    assert!(notices[0].1.contains("oversized"), "{notices:?}");
    assert!(notices[0].1.contains("17."), "{notices:?}");
    let transcript = rig.transcript();
    assert_eq!(agent_texts(&transcript).len(), 3);
    assert_eq!(outcomes(&rig.seen), vec![TurnOutcome::Completed]);
    assert_notice(
        &completed_notices(&rig.seen)[0],
        notice_kind::OVERSIZED_LINE,
        false,
    );
}

#[test]
fn noise_and_unknown_frames_are_ignored() {
    let mut rig = Rig::new(&["noise"]);
    rig.send("go");
    rig.until_idle();
    assert_eq!(agent_texts(&rig.transcript()), ["Still here.", "done"]);
    assert!(notices(&rig.seen).is_empty());
    assert_eq!(outcomes(&rig.seen), vec![TurnOutcome::Completed]);
}

#[test]
fn a_crash_mid_turn_fails_the_turn_and_the_chat_with_the_end_of_stderr() {
    let mut rig = Rig::new(&["crash"]);
    rig.send("go");
    let events = rig.until(|event| {
        matches!(
            event,
            ChatEvent::State {
                state: ChatState::Failed { .. }
            }
        )
    });
    let Some(TurnOutcome::Failed { message }) = outcomes(&events).pop() else {
        panic!("the turn did not fail: {events:#?}");
    };
    assert!(message.contains("exit status: 3"), "{message}");
    assert!(message.contains("session store is corrupt"), "{message}");
    assert_eq!(
        events.last(),
        Some(&ChatEvent::State {
            state: ChatState::Failed { message }
        })
    );
    // The text that streamed before the crash does not stay open.
    let transcript = rig.transcript();
    assert!(
        transcript
            .items
            .iter()
            .all(|item| item.status != ItemStatus::InProgress)
    );
    assert_eq!(transcript.turn_id, None);
    // A dead chat takes no commands, and stopping it leaves the failure standing.
    let error = rig
        .driver
        .command(ChatCommand::Send {
            text: "again".into(),
        })
        .unwrap_err();
    assert!(error.contains("not running"), "{error}");
    rig.driver.shutdown();
    assert!(rig.events.try_recv().is_err());
}

#[test]
fn a_failed_result_fails_the_turn_without_saying_the_same_thing_twice() {
    let mut rig = Rig::new(&["failed_result"]);
    rig.send("go");
    rig.until_idle();
    assert_eq!(
        outcomes(&rig.seen),
        vec![TurnOutcome::Failed {
            message: "Failed to authenticate. API Error: 401".into()
        }]
    );
    assert_eq!(
        agent_texts(&rig.transcript()),
        ["Failed to authenticate. API Error: 401"]
    );
    assert_eq!(
        notices(&rig.seen),
        vec![(
            NoticeLevel::Error,
            "Claude stopped because the turn failed.".into()
        )]
    );
    assert_notice(
        &completed_notices(&rig.seen)[0],
        notice_kind::TURN_FAILED,
        false,
    );
}

#[test]
fn compact_sends_the_slash_command_and_the_boundary_becomes_a_compaction() {
    let mut rig = Rig::new(&["compact"]);
    rig.command(ChatCommand::Compact);
    rig.until_idle();
    assert_eq!(rig.fake.received()[1]["message"]["content"], "/compact");
    let transcript = rig.transcript();
    assert_eq!(
        transcript
            .items
            .iter()
            .map(|item| &item.body)
            .collect::<Vec<_>>(),
        vec![
            &ItemBody::Compaction,
            &ItemBody::AgentMessage {
                text: "done".into()
            }
        ]
    );
    assert_eq!(outcomes(&rig.seen), vec![TurnOutcome::Completed]);
}

#[test]
fn retries_become_notices_but_rate_warnings_are_structured_windows() {
    let mut rig = Rig::new(&["notices"]);
    rig.send("go");
    rig.until_idle();
    let notices = notices(&rig.seen);
    assert_eq!(notices.len(), 2, "{notices:?}");
    assert_eq!(notices[0].0, NoticeLevel::Warning);
    assert!(notices[0].1.contains("status 529"), "{notices:?}");
    assert!(notices[0].1.contains("attempt 1 of 5"), "{notices:?}");
    let transcript = rig.transcript();
    let ids: Vec<&str> = transcript
        .items
        .iter()
        .filter(|item| matches!(item.body, ItemBody::Notice { .. }))
        .map(|item| item.id.as_str())
        .collect();
    assert_eq!(transcript.rate_limits[0].id, "five_hour");
    assert_eq!(transcript.rate_limits[0].used_percent, 90.0);
    // Unique to this driver, so that the ids of a resumed chat's notices do not
    // replace the earlier ones' in the host's log.
    let prefix = &ids[0]["notice-".len()..ids[0].len() - 2];
    assert_eq!(prefix.len(), 8);
    assert_eq!(ids, [format!("notice-{prefix}-1")]);
    let other = {
        let (sender, _events) = mpsc::channel();
        let mut core = Core::new(&bare_config(), sender, "s".into());
        core.notice(NoticeLevel::Info, notice_kind::PROVIDER_WARNING, "x");
        core.notice_prefix
    };
    assert_ne!(other, prefix);
}

#[test]
fn silence_during_a_turn_is_a_notice_and_the_process_is_left_alone() {
    let tuning = Tuning {
        inactivity: Duration::from_millis(250),
        ..fast()
    };
    let mut rig = Rig::with(&["silence"], tuning, |_| {});
    rig.send("go");
    rig.until_idle();
    let notices = notices(&rig.seen);
    assert_eq!(notices.len(), 2, "{notices:?}");
    assert!(notices[0].1.contains("silent"), "{notices:?}");
    assert_eq!(outcomes(&rig.seen), vec![TurnOutcome::Completed]);
    assert_eq!(rig.fake.starts().len(), 1);
    assert!(!rig.fake.saw("signal"));
}

#[test]
fn tools_become_the_items_their_kind_calls_for() {
    let mut rig = Rig::new(&["tools"]);
    rig.send("plan it");
    rig.until_idle();
    let transcript = rig.transcript();
    assert_eq!(
        item(&transcript, "toolu_todo").body,
        ItemBody::Todo {
            items: vec![
                Step {
                    text: "Read code".into(),
                    status: StepStatus::Completed
                },
                Step {
                    text: "Writing the fix".into(),
                    status: StepStatus::InProgress
                },
            ]
        }
    );
    assert_eq!(
        item(&transcript, "toolu_mcp").body,
        ItemBody::ToolCall {
            server: Some("cua-driver".into()),
            tool: "screenshot".into(),
            input: json!({"window": 1}),
            output: Some("a screenshot\n[image]".into()),
        }
    );
    let failed = item(&transcript, "toolu_fail");
    assert_eq!(failed.status, ItemStatus::Failed);
    assert!(
        matches!(&failed.body, ItemBody::Command { exit_code: Some(1), output, .. } if output == "boom\n")
    );
    assert_eq!(
        item(&transcript, "toolu_plan").body,
        ItemBody::Plan {
            explanation: Some("1. Do the thing\n2. Test it".into()),
            steps: vec![]
        }
    );
    assert_eq!(
        item(&transcript, "toolu_search").body,
        ItemBody::WebSearch {
            query: "rust process groups".into()
        }
    );
    // A subagent's own text stays out of the transcript.
    assert_eq!(agent_texts(&transcript), ["done"]);
}

#[test]
fn without_state_events_the_result_ends_the_turn() {
    let mut rig = Rig::new(&["no_state_events"]);
    rig.send("go");
    rig.until_idle();
    assert_eq!(outcomes(&rig.seen), vec![TurnOutcome::Completed]);
    assert_eq!(agent_texts(&rig.transcript()), ["Hi.", "done"]);
}

#[test]
fn stopping_closes_stdin_and_says_stopped_once() {
    let mut rig = Rig::new(&["idle_only"]);
    rig.driver.shutdown();
    assert_eq!(
        rig.events.recv_timeout(Duration::from_secs(5)),
        Ok(ChatEvent::State {
            state: ChatState::Stopped
        })
    );
    assert!(rig.fake.saw("eof"));
    assert!(!rig.fake.saw("signal"));
    let error = rig
        .driver
        .command(ChatCommand::Send {
            text: "late".into(),
        })
        .unwrap_err();
    assert!(error.contains("stopped"), "{error}");
    rig.driver.shutdown();
    assert!(rig.events.try_recv().is_err());
}

#[test]
fn stopping_in_the_middle_of_a_turn_closes_it_as_interrupted() {
    let mut rig = Rig::new(&["interrupt_ack"]);
    rig.send("Run for a while");
    rig.until(|event| matches!(event, ChatEvent::ItemDelta { .. }));
    rig.command(ChatCommand::Stop);
    let events = rig.until(|event| {
        matches!(
            event,
            ChatEvent::State {
                state: ChatState::Stopped
            }
        )
    });
    assert_eq!(outcomes(&events), vec![TurnOutcome::Interrupted]);
    assert!(rig.driver.command(ChatCommand::Interrupt).is_err());
}

#[test]
fn start_reports_a_process_that_dies_or_stays_silent() {
    // Every failure ends the events with the same message the error carries.
    let text = fixture("claude/silent_start.ndjson");
    let fake = Fake::new(&[text.as_str()]);
    let (sender, events) = mpsc::channel();
    let tuning = Tuning {
        handshake: Duration::from_millis(300),
        ..fast()
    };
    let error = start_with(fake.config(Provider::Claude), sender, tuning)
        .err()
        .unwrap();
    assert!(error.contains("did not answer initialize"), "{error}");
    assert_eq!(
        states(&events.try_iter().collect::<Vec<_>>()),
        [
            ChatState::Starting,
            ChatState::Failed {
                message: error.clone()
            }
        ]
    );

    let (sender, events) = mpsc::channel();
    let mut config = fake.config(Provider::Claude);
    config.program = fake.dir.join("missing");
    let error = start(config, sender).err().unwrap();
    assert!(error.contains("cannot start"), "{error}");
    assert_eq!(
        states(&events.try_iter().collect::<Vec<_>>()),
        [ChatState::Failed { message: error }]
    );
}

#[test]
fn resuming_a_session_claude_never_saved_starts_it_again_under_the_same_id() {
    let rig = Rig::new(&["fails_at_start", "idle_only"]);
    assert_eq!(rig.driver.provider_thread_id().as_deref(), Some("sess-1"));
    let starts = rig.fake.starts();
    assert_eq!(starts.len(), 2);
    assert!(has_pair(&argv(&starts[0]), "--resume", "sess-1"));
    assert!(has_pair(&argv(&starts[1]), "--session-id", "sess-1"));
    assert!(!argv(&starts[1]).contains(&"--resume".to_owned()));
    // The first attempt's failure is not announced; the notice is.
    assert_eq!(
        rig.states(),
        [ChatState::Starting, ChatState::Idle],
        "{:#?}",
        rig.seen
    );
    let notices = notices(&rig.seen);
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].0, NoticeLevel::Info);
    assert!(
        notices[0].1.contains("no saved conversation sess-1"),
        "{notices:?}"
    );
    assert!(rig.seen.iter().all(|event| !matches!(
        event,
        ChatEvent::Usage { .. } | ChatEvent::TurnStarted { .. }
    )));
    assert_notice(
        &completed_notices(&rig.seen)[0],
        notice_kind::RESUMED_FRESH,
        false,
    );
}

#[test]
fn resuming_fails_for_good_when_the_second_attempt_fails_too() {
    let text = fixture("claude/fails_at_start.ndjson");
    let fake = Fake::new(&[text.as_str()]);
    let (sender, events) = mpsc::channel();
    let mut config = fake.config(Provider::Claude);
    config.resume = Some("sess-1".into());
    let error = start_with(config, sender, fast()).err().unwrap();
    assert!(error.contains("No conversation found"), "{error}");
    assert!(error.contains("exited"), "{error}");
    assert_eq!(fake.starts().len(), 2);
    let events: Vec<ChatEvent> = events.try_iter().collect();
    assert_eq!(
        states(&events),
        [ChatState::Starting, ChatState::Failed { message: error }]
    );
    assert!(notices(&events).is_empty());
}

#[test]
fn a_resume_that_fails_otherwise_is_not_retried() {
    // Not the missing-session message: no second attempt.
    let fake = Fake::new(&["{\"type\":\"exit\",\"code\":2,\"stderr\":\"claude: bad settings\"}\n"]);
    let (sender, events) = mpsc::channel();
    let mut config = fake.config(Provider::Claude);
    config.resume = Some("sess-1".into());
    let error = start_with(config, sender, fast()).err().unwrap();
    assert!(error.contains("bad settings"), "{error}");
    assert_eq!(fake.starts().len(), 1);
    assert_eq!(
        states(&events.try_iter().collect::<Vec<_>>()),
        [ChatState::Starting, ChatState::Failed { message: error }]
    );
}

#[test]
fn a_made_up_error_message_does_not_overwrite_the_streamed_one_before_it() {
    let mut rig = Rig::new(&["synthetic_after_stream"]);
    rig.send("go");
    rig.until_idle();
    let transcript = rig.transcript();
    assert_eq!(
        agent_texts(&transcript),
        ["Streamed.", "Request failed.", "done"]
    );
    assert_eq!(
        item(&transcript, "M1:0").body,
        ItemBody::AgentMessage {
            text: "Streamed.".into()
        }
    );
    assert!(transcript.items.iter().any(|item| item.id == "synth-1:0"));
}

#[test]
fn a_cancel_whose_interrupt_the_cli_ignores_is_escalated_like_an_interrupt() {
    let tuning = Tuning {
        stop_grace: Duration::from_millis(200),
        ..fast()
    };
    let mut rig = Rig::with(&["cancel_ignored_1", "idle_only_2"], tuning, |_| {});
    rig.send("go");
    rig.until(is_approval);
    rig.command(ChatCommand::Approve {
        request_id: "perm-1".into(),
        decision: Decision::Cancel,
    });
    rig.until_idle();
    assert!(!rig.fake.saw("mismatch"));
    assert_eq!(outcomes(&rig.seen), vec![TurnOutcome::Interrupted]);
    let entries = rig.fake.entries();
    assert!(
        entries.contains(&json!({"signal": "INT", "ignored": true})),
        "{entries:?}"
    );
    let starts = rig.fake.starts();
    assert_eq!(starts.len(), 2);
    assert!(has_pair(&argv(&starts[1]), "--resume", "sess-1"));
}

#[test]
fn a_process_that_leaves_after_sigint_is_restarted_even_if_the_turn_had_ended() {
    let tuning = Tuning {
        interrupt_grace: Duration::from_millis(100),
        interrupt_kill: Duration::from_secs(5),
        stop_grace: Duration::from_millis(200),
        ..fast()
    };
    let mut rig = Rig::with(&["sigint_then_idle_1", "idle_only_2"], tuning, |_| {});
    rig.send("Run for a while");
    rig.command(ChatCommand::Interrupt);
    // The turn ends (the process ignored SIGINT but finished a little later) ...
    rig.until_idle();
    assert_eq!(outcomes(&rig.seen), vec![TurnOutcome::Interrupted]);
    // ... and the process exits right after: a new one, not a failure.
    rig.until(|event| {
        matches!(
            event,
            ChatEvent::State {
                state: ChatState::Starting
            }
        )
    });
    rig.until_idle();
    assert!(
        rig.states()
            .iter()
            .all(|state| !matches!(state, ChatState::Failed { .. })),
        "{:?}",
        rig.states()
    );
    let entries = rig.fake.entries();
    assert!(
        entries.contains(&json!({"signal": "INT", "ignored": true})),
        "{entries:?}"
    );
    let starts = rig.fake.starts();
    assert_eq!(starts.len(), 2);
    assert!(has_pair(&argv(&starts[1]), "--resume", "sess-1"));
}

#[test]
fn time_spent_on_a_prompt_is_not_silence() {
    let tuning = Tuning {
        inactivity: Duration::from_millis(300),
        ..fast()
    };
    let mut rig = Rig::with(&["slow_answer"], tuning, |_| {});
    rig.send("go");
    rig.until(is_approval);
    // Longer than the limit, with the prompt open.
    thread::sleep(Duration::from_millis(700));
    rig.command(ChatCommand::Approve {
        request_id: "perm-1".into(),
        decision: Decision::Accept,
    });
    // The CLI takes 150 ms to answer; the clock started with the answer.
    rig.until_idle();
    assert!(notices(&rig.seen).is_empty(), "{:?}", notices(&rig.seen));
}

#[test]
fn compaction_resets_the_context_size_to_what_it_left() {
    let mut rig = Rig::new(&["compact_after_turn"]);
    rig.send("hello");
    rig.until_idle();
    assert_eq!(
        rig.transcript().usage.and_then(|usage| usage.context_used),
        Some(3 + 1000 + 5)
    );
    rig.command(ChatCommand::Compact);
    rig.until_idle();
    assert_eq!(
        rig.transcript().usage.and_then(|usage| usage.context_used),
        Some(400)
    );
}

#[test]
fn stopping_during_a_restart_stops_the_process_being_replaced_too() {
    let tuning = Tuning {
        stop_grace: Duration::from_millis(400),
        term_wait: Duration::from_millis(400),
        ..fast()
    };
    let mut rig = Rig::with(&["hangs", "idle_only_2"], tuning, |_| {});
    let dir = rig.fake.dir.to_str().unwrap().to_owned();
    let alive = || {
        std::process::Command::new("pgrep")
            .args(["-f", &dir])
            .output()
            .unwrap()
            .status
            .success()
    };
    assert!(alive());
    rig.command(ChatCommand::Configure {
        model: None,
        effort: Some("low".into()),
        approval_mode: None,
        fast: None,
    });
    // The restart has begun stopping the old process, which will not leave
    // by itself for a while.
    thread::sleep(Duration::from_millis(100));
    rig.driver.shutdown();
    assert!(!alive(), "a process outlived shutdown");
    assert_eq!(rig.fake.starts().len(), 1, "a new one was started");
    let events: Vec<ChatEvent> = rig.events.try_iter().collect();
    assert_eq!(states(&events).last(), Some(&ChatState::Stopped));
    assert!(
        states(&events)
            .iter()
            .all(|state| !matches!(state, ChatState::Failed { .. }))
    );
}

#[test]
fn messages_queued_for_a_restart_that_fails_are_reported_not_lost() {
    let mut rig = Rig::new(&["idle_only", "fails_at_start"]);
    rig.command(ChatCommand::Configure {
        model: None,
        effort: Some("low".into()),
        approval_mode: None,
        fast: None,
    });
    rig.send("hello?");
    rig.command(ChatCommand::Compact);
    let events = rig.until(|event| {
        matches!(
            event,
            ChatEvent::State {
                state: ChatState::Failed { .. }
            }
        )
    });
    let notices = notices(&events);
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0].0, NoticeLevel::Warning);
    assert!(
        notices[0].1.contains("2 messages") && notices[0].1.contains("not delivered"),
        "{notices:?}"
    );
    // Nothing was echoed for what never reached the CLI.
    assert!(
        !rig.transcript()
            .items
            .iter()
            .any(|item| matches!(item.body, ItemBody::UserMessage { .. }))
    );
    assert!(rig.driver.command(ChatCommand::Compact).is_err());
    assert_notice(
        &completed_notices(&events)[0],
        notice_kind::UNDELIVERED,
        false,
    );
}

// The pure parts.

#[test]
fn diffs_show_the_replaced_and_the_new_text() {
    assert_eq!(
        unified_diff("/a.txt", false, &[("one\ntwo", "one\n2\nthree")]),
        "--- /a.txt\n+++ /a.txt\n@@ -1,2 +1,3 @@\n-one\n-two\n+one\n+2\n+three\n"
    );
    assert_eq!(
        unified_diff("/new.txt", true, &[("", "x\n")]),
        "--- /dev/null\n+++ /new.txt\n@@ -0,0 +1,1 @@\n+x\n"
    );
}

#[test]
fn file_tools_become_file_changes() {
    let edit = tool_body(
        "Edit",
        &json!({"file_path": "/x/y.rs", "old_string": "a", "new_string": "b"}),
    );
    assert_eq!(
        edit,
        ItemBody::FileChange {
            changes: vec![FileChange {
                path: "/x/y.rs".into(),
                kind: ChangeKind::Modify,
                diff: Some("--- /x/y.rs\n+++ /x/y.rs\n@@ -1,1 +1,1 @@\n-a\n+b\n".into()),
            }]
        }
    );
    let multi = tool_body(
        "MultiEdit",
        &json!({"file_path": "/x/y.rs", "edits": [
            {"old_string": "a", "new_string": "b"}, {"old_string": "c", "new_string": "d\ne"}]}),
    );
    let ItemBody::FileChange { changes } = multi else {
        panic!()
    };
    assert_eq!(changes.len(), 1);
    assert_eq!(
        changes[0].diff.as_deref(),
        Some("--- /x/y.rs\n+++ /x/y.rs\n@@ -1,1 +1,1 @@\n-a\n+b\n@@ -1,1 +1,2 @@\n-c\n+d\n+e\n")
    );
    let write = tool_body(
        "Write",
        &json!({"file_path": "/definitely/not/here.txt", "content": "hello"}),
    );
    assert!(matches!(&write, ItemBody::FileChange { changes }
        if changes[0].kind == ChangeKind::Add
            && changes[0].diff.as_deref() == Some("--- /dev/null\n+++ /definitely/not/here.txt\n@@ -0,0 +1,1 @@\n+hello\n")));
    // A file that exists is modified, not added.
    let existing = std::env::current_exe().unwrap();
    let write = tool_body(
        "Write",
        &json!({"file_path": existing.to_str().unwrap(), "content": "x"}),
    );
    assert!(
        matches!(&write, ItemBody::FileChange { changes } if changes[0].kind == ChangeKind::Modify)
    );
    let notebook = tool_body(
        "NotebookEdit",
        &json!({"notebook_path": "/n.ipynb", "new_source": "print(1)", "edit_mode": "replace"}),
    );
    assert!(matches!(&notebook, ItemBody::FileChange { changes }
        if changes[0].path == "/n.ipynb" && changes[0].diff.as_deref().unwrap().contains("+print(1)")));
    let deleted = tool_body(
        "NotebookEdit",
        &json!({"notebook_path": "/n.ipynb", "new_source": "", "edit_mode": "delete"}),
    );
    assert!(matches!(&deleted, ItemBody::FileChange { changes } if changes[0].diff.is_none()));
    // Without a path there is nothing to show but the call.
    assert!(matches!(
        tool_body("Edit", &json!({})),
        ItemBody::ToolCall { .. }
    ));
}

#[test]
fn other_tools_are_classified_by_name() {
    assert!(matches!(
        tool_body("Bash", &json!({"command": "ls"})),
        ItemBody::Command { command, .. } if command == "ls"
    ));
    assert!(matches!(
        tool_body("ExitPlanMode", &json!({})),
        ItemBody::ToolCall { tool, .. } if tool == "ExitPlanMode"
    ));
    assert!(matches!(
        tool_body("mcp__a_b__c__d", &json!({})),
        ItemBody::ToolCall { server: Some(server), tool, .. } if server == "a_b" && tool == "c__d"
    ));
    assert!(matches!(
        tool_body("Grep", &json!({"pattern": "x"})),
        ItemBody::ToolCall { server: None, tool, .. } if tool == "Grep"
    ));
    assert_eq!(approval_kind("Bash"), ApprovalKind::Command);
    for tool in ["Edit", "Write", "MultiEdit", "NotebookEdit"] {
        assert_eq!(approval_kind(tool), ApprovalKind::FileChange);
    }
    assert_eq!(approval_kind("WebFetch"), ApprovalKind::Tool);
    assert_eq!(approval_kind("mcp__x__y"), ApprovalKind::Tool);
}

#[test]
fn results_fill_in_output_exit_code_and_text() {
    let mut command = tool_body("Bash", &json!({"command": "x"}));
    apply_result(&mut command, "Exit code 2\nout\nerr", true, None);
    assert!(
        matches!(&command, ItemBody::Command { exit_code: Some(2), output, .. } if output == "out\nerr")
    );
    let mut command = tool_body("Bash", &json!({"command": "x"}));
    apply_result(
        &mut command,
        "(Bash completed with no output)",
        false,
        Some(&json!({"stdout": "", "stderr": ""})),
    );
    assert!(
        matches!(&command, ItemBody::Command { exit_code: Some(0), output, .. } if output.is_empty())
    );
    let mut command = tool_body("Bash", &json!({"command": "x"}));
    apply_result(
        &mut command,
        "ignored",
        false,
        Some(&json!({"stdout": "a", "stderr": "b"})),
    );
    assert!(matches!(&command, ItemBody::Command { output, .. } if output == "a\nb"));
    // A failure that says nothing about an exit code has none.
    let mut command = tool_body("Bash", &json!({"command": "x"}));
    apply_result(
        &mut command,
        "Request interrupted",
        true,
        Some(&json!("Error: x")),
    );
    assert!(matches!(
        &command,
        ItemBody::Command {
            exit_code: None,
            ..
        }
    ));

    assert_eq!(split_exit_code("Exit code 12\nrest"), (Some(12), "rest"));
    assert_eq!(
        split_exit_code("Exit code x\nrest"),
        (None, "Exit code x\nrest")
    );
    assert_eq!(split_exit_code("plain"), (None, "plain"));
    assert_eq!(
        content_text(
            &json!([{"type": "text", "text": "a"}, {"type": "image"}, {"type": "other"}, {"type": "text", "text": "b"}])
        ),
        "a\n[image]\nb"
    );
    assert_eq!(content_text(&json!("s")), "s");
    assert_eq!(content_text(&Value::Null), "");
}

#[test]
fn long_text_is_cut_at_a_character_boundary() {
    let long = "é".repeat(TEXT_LIMIT);
    let head = cap_text(&long, false);
    assert!(head.ends_with("\n[output cut]") && head.len() < TEXT_LIMIT + 20);
    let tail = cap_text(&long, true);
    assert!(tail.starts_with("[output cut]\n") && tail.len() < TEXT_LIMIT + 20);
    assert_eq!(cap_text("short", true), "short");
}

#[test]
fn approvals_describe_what_is_asked_and_offer_what_the_cli_allows() {
    let request = json!({
        "subtype": "can_use_tool", "tool_name": "Bash", "tool_use_id": "t1",
        "input": {"command": "cd x\nrm -rf build", "description": "Clean"},
        "decision_reason": "\u{1b}[1mDangerous\u{1b}[0m command",
        "blocked_path": "/etc",
    });
    let Some(Pending::Approval { approval, .. }) = build_approval("r1", &request) else {
        panic!()
    };
    assert_eq!(approval.title, "cd x");
    assert_eq!(approval.kind, ApprovalKind::Command);
    assert_eq!(approval.item_id.as_deref(), Some("t1"));
    assert_eq!(
        approval.detail,
        "Dangerous command\n\nNeeds access to /etc\n\nClean\n\ncd x\nrm -rf build"
    );
    assert_eq!(
        approval.choices,
        [
            Decision::Accept,
            Decision::AcceptForSession,
            Decision::Decline
        ]
    );

    let mut request = request;
    request["suppress_always_allow_rule"] = json!(true);
    let Some(Pending::Approval { approval, .. }) = build_approval("r1", &request) else {
        panic!()
    };
    assert_eq!(approval.choices, [Decision::Accept, Decision::Decline]);

    let tool = json!({"subtype": "can_use_tool", "tool_name": "WebFetch", "tool_use_id": "t2",
        "display_name": "Fetch", "input": {"url": "https://example.com"}});
    let Some(Pending::Approval { approval, .. }) = build_approval("r2", &tool) else {
        panic!()
    };
    assert_eq!(approval.kind, ApprovalKind::Tool);
    assert_eq!(approval.title, "Fetch");
    assert!(approval.detail.contains("https://example.com"));

    let plan = json!({"subtype": "can_use_tool", "tool_name": "ExitPlanMode", "tool_use_id": "t3",
        "input": {"plan": "Step 1"}});
    let Some(Pending::Approval { approval, .. }) = build_approval("r3", &plan) else {
        panic!()
    };
    assert_eq!(
        (approval.title.as_str(), approval.detail.as_str()),
        ("Approve the plan", "Step 1")
    );
    assert!(build_approval("r4", &json!({"subtype": "can_use_tool"})).is_none());
    assert!(build_question("r5", &json!({"input": {"questions": []}})).is_none());
}

#[test]
fn allowing_for_the_session_uses_the_suggestion_or_a_rule_for_what_was_asked() {
    let suggestions = vec![
        json!({"type": "setMode", "mode": "acceptEdits", "destination": "session"}),
        json!({"type": "addDirectories", "directories": ["/x"], "destination": "userSettings"}),
        json!({"type": "addRules", "rules": [], "behavior": "allow"}),
        json!("not an update"),
    ];
    // Whatever the CLI suggests stays in this session; what is no update goes.
    assert_eq!(
        session_permissions("Edit", &json!({}), &suggestions),
        json!([
            {"type": "setMode", "mode": "acceptEdits", "destination": "session"},
            {"type": "addDirectories", "directories": ["/x"], "destination": "session"},
            {"type": "addRules", "rules": [], "behavior": "allow", "destination": "session"},
        ])
    );
    assert_eq!(
        session_permissions("Edit", &json!({}), &[json!(5)])[0]["destination"],
        "session"
    );
    assert_eq!(
        session_permissions("Bash", &json!({"command": "ls -l"}), &[]),
        json!([{"type": "addRules", "rules": [{"toolName": "Bash", "ruleContent": "ls -l"}],
                "behavior": "allow", "destination": "session"}])
    );
    assert_eq!(
        session_permissions("WebFetch", &json!({"url": "u"}), &[]),
        json!([{"type": "addRules", "rules": [{"toolName": "WebFetch"}],
                "behavior": "allow", "destination": "session"}])
    );
    assert_eq!(
        allow_body(&json!({"a": 1}), "t", None),
        json!({"behavior": "allow", "updatedInput": {"a": 1}, "toolUseID": "t"})
    );
    assert_eq!(
        deny_body("t", true),
        json!({"behavior": "deny", "message": DECLINED, "toolUseID": "t", "interrupt": true})
    );
}

#[test]
fn small_helpers() {
    assert_eq!(permission_mode(ApprovalMode::Supervised), "default");
    assert_eq!(permission_mode(ApprovalMode::AutoEdit), "acceptEdits");
    assert_eq!(permission_mode(ApprovalMode::Full), "bypassPermissions");
    assert_eq!(permission_mode(ApprovalMode::Plan), "plan");
    assert_eq!(strip_ansi("a\u{1b}[31;1mb\u{1b}[0mc"), "abc");
    assert_eq!(first_line(&"x".repeat(300)).chars().count(), 201);
    assert_eq!(describe_duration(Duration::from_secs(90)), "90 seconds");
    assert_eq!(describe_duration(Duration::from_secs(300)), "5 minutes");
    assert_eq!(
        context_tokens(&json!({"input_tokens": 1, "output_tokens": 2})),
        Some(3)
    );
    assert_eq!(context_tokens(&json!({})), None);
    assert_eq!(split_mcp("mcp__server__tool"), Some(("server", "tool")));
    assert_eq!(split_mcp("Bash"), None);
}

fn bare_config() -> DriverConfig {
    DriverConfig {
        provider: Provider::Claude,
        program: "claude".into(),
        cwd: std::env::temp_dir(),
        approval_mode: ApprovalMode::Plan,
        model: None,
        effort: None,
        fast: false,
        resume: None,
        outstanding_notices: Default::default(),
        extra_args: Vec::new(),
        env: Vec::new(),
        env_remove: Vec::new(),
    }
}

/// Starts the real `claude` with a trivial prompt. It spends tokens and needs a
/// logged-in CLI, so it only runs on request:
/// `RIWORK_LIVE_CLAUDE=1 cargo test chat::claude -- --ignored live`
/// (`RIWORK_LIVE_CLAUDE_PROGRAM` names the executable, `claude` by default).
#[test]
#[ignore = "starts the real claude; set RIWORK_LIVE_CLAUDE=1"]
fn live_claude_answers_a_trivial_prompt() {
    if std::env::var("RIWORK_LIVE_CLAUDE").as_deref() != Ok("1") {
        return;
    }
    let dir = std::env::temp_dir().join(format!("riwork-claude-live-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let config = DriverConfig {
        provider: Provider::Claude,
        program: std::env::var("RIWORK_LIVE_CLAUDE_PROGRAM")
            .unwrap_or_else(|_| "claude".into())
            .into(),
        cwd: dir.clone(),
        approval_mode: ApprovalMode::Plan,
        model: None,
        effort: None,
        fast: false,
        resume: None,
        outstanding_notices: Default::default(),
        extra_args: Vec::new(),
        env: Vec::new(),
        env_remove: Vec::new(),
    };
    let (sender, events) = mpsc::channel();
    let mut driver = start(config, sender).expect("claude starts and answers initialize");
    assert!(driver.provider_thread_id().is_some());
    driver
        .command(ChatCommand::Send {
            text: "Reply with the single word pong.".into(),
        })
        .unwrap();
    let mut seen = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        let Ok(event) = events.recv_timeout(Duration::from_secs(5)) else {
            continue;
        };
        let finished = matches!(event, ChatEvent::TurnCompleted { .. });
        seen.push(event);
        if finished {
            break;
        }
    }
    driver.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
    let transcript = fold(&seen);
    assert!(
        !agent_texts(&transcript).is_empty(),
        "no answer in {seen:#?}"
    );
    assert!(
        outcomes(&seen).contains(&TurnOutcome::Completed),
        "{seen:#?}"
    );
}

#[test]
fn result_marks_only_authoritative_text_final_and_tool_images_survive() {
    let fake = Fake::new(&[]);
    let (sender, receiver) = mpsc::channel();
    let mut core = Core::new(&fake.config(Provider::Claude), sender, "session".into());
    core.on_assistant(&json!({"message":{"id":"comment","content":[{"type":"text","text":"Checking"}]},"parent_tool_use_id":null}));
    core.on_assistant(&json!({"message":{"id":"tool","content":[{"type":"tool_use","id":"picture","name":"mcp__cua__screenshot","input":{}}]},"parent_tool_use_id":null}));
    core.on_user(&json!({"message":{"content":[{"type":"tool_result","tool_use_id":"picture","content":[{"type":"image","source":{"type":"base64","media_type":"image/png","data":"aGVsbG8="}}]}]}}));
    core.on_result(&json!({"type":"result","subtype":"success","is_error":false,"result":"The completed result"}));
    let transcript = fold(&receiver.try_iter().collect::<Vec<_>>());
    assert_eq!(transcript.items[0].presentation.phase, None);
    assert_eq!(transcript.items[1].presentation.images.len(), 1);
    let final_item = transcript
        .items
        .iter()
        .find(|item| item.presentation.phase == Some(crate::chat::model::MessagePhase::Final))
        .unwrap();
    assert!(
        matches!(&final_item.body, ItemBody::AgentMessage { text } if text == "The completed result")
    );
}

fn completed_notices(events: &[ChatEvent]) -> Vec<Item> {
    events
        .iter()
        .filter_map(|event| match event {
            ChatEvent::ItemCompleted { item } if matches!(item.body, ItemBody::Notice { .. }) => {
                Some(item.clone())
            }
            _ => None,
        })
        .collect()
}

fn assert_notice(item: &Item, expected: &str, resolved: bool) {
    assert!(
        matches!(&item.body, ItemBody::Notice { kind: Some(kind), resolved: actual, .. }
        if kind == expected && *actual == resolved),
        "{item:?}"
    );
}

#[test]
fn rate_limit_status_is_remembered_across_turns_and_allowed_resolves_the_last_item() {
    let (sender, events) = mpsc::channel();
    let mut core = Core::new(&bare_config(), sender, "session".into());
    core.ready = true;
    let warning = json!({"type":"rate_limit_event", "rate_limit_info": {
        "status":"allowed_warning", "rateLimitType":"seven_day", "resetsAt":1767225600000u64
    }});
    core.ensure_turn();
    core.on_frame(&warning);
    core.on_result(&json!({"subtype":"success"}));
    core.ensure_turn();
    core.on_frame(&warning);
    let first = completed_notices(&events.try_iter().collect::<Vec<_>>());
    assert!(first.is_empty(), "allowed_warning never creates a notice");
    core.on_frame(&json!({"type":"rate_limit_event","rate_limit_info":{
        "status":"rejected","rateLimitType":"seven_day","resetsAt":1767225700
    }}));
    core.on_frame(&json!({"type":"rate_limit_event","rate_limit_info":{
        "status":"allowed","rateLimitType":"seven_day"
    }}));
    core.on_frame(&json!({"type":"rate_limit_event","rate_limit_info":{
        "status":"allowed","rateLimitType":"seven_day"
    }}));
    let updates = completed_notices(&events.try_iter().collect::<Vec<_>>());
    assert_eq!(updates.len(), 2);
    assert_eq!(updates[0].id, updates[1].id);
    assert_notice(&updates[1], "rate_limit:seven_day", true);
    assert!(
        matches!(&updates[0].body, ItemBody::Notice { text, resets_at: Some(1767225700), level: NoticeLevel::Error, .. }
        if text == "This account has reached the weekly usage limit.")
    );
    for window in [Some("new_provider_window"), None] {
        core.on_frame(&json!({"type":"rate_limit_event","rate_limit_info":{
            "status":"rejected","rateLimitType":window
        }}));
        let items = completed_notices(&events.try_iter().collect::<Vec<_>>());
        assert_eq!(items.len(), 1);
        assert_notice(
            &items[0],
            &notice_kind::rate_limit(window.unwrap_or("unknown")),
            false,
        );
    }
}

#[test]
fn api_retries_update_one_item_per_turn_and_resolve_on_assistant_or_result() {
    let (sender, events) = mpsc::channel();
    let mut core = Core::new(&bare_config(), sender, "session".into());
    core.ready = true;
    for attempt in [1, 2] {
        core.on_frame(
            &json!({"type":"system","subtype":"api_retry","attempt":attempt,"max_retries":5}),
        );
    }
    core.on_frame(&json!({"type":"assistant","message":{"id":"answer","content":[{"type":"text","text":"hello"}]}}));
    let items = completed_notices(&events.try_iter().collect::<Vec<_>>());
    assert_eq!(items.len(), 3);
    assert!(items.iter().all(|item| item.id == items[0].id));
    assert_notice(&items[0], notice_kind::API_RETRY, false);
    assert!(
        matches!(&items[1].body, ItemBody::Notice {text, ..} if text.contains("attempt 2 of 5"))
    );
    assert_notice(&items[2], notice_kind::API_RETRY, true);
    // A further retry in this turn still updates its original item.
    core.on_frame(&json!({"type":"system","subtype":"api_retry","attempt":3}));
    core.on_frame(&json!({"type":"result","subtype":"success"}));
    let more = completed_notices(&events.try_iter().collect::<Vec<_>>());
    assert_eq!(more.len(), 2);
    assert!(more.iter().all(|item| item.id == items[0].id));
    assert_notice(&more[1], notice_kind::API_RETRY, true);
    core.on_frame(&json!({"type":"system","subtype":"api_retry","attempt":1}));
    let next = completed_notices(&events.try_iter().collect::<Vec<_>>());
    assert_ne!(next[0].id, items[0].id);
}

#[test]
fn silence_and_fast_mode_resolve_their_warning_ids() {
    let (sender, events) = mpsc::channel();
    let mut core = Core::new(&bare_config(), sender, "session".into());
    core.ready = true;
    core.ensure_turn();
    core.last_activity = Instant::now() - Duration::from_secs(601);
    core.watch(&fast());
    core.on_frame(&json!({"type":"assistant","message":{"content":[]}}));
    core.settings.fast = true;
    core.note_fast_state(&json!({"fast_mode_state":"off"}));
    core.note_fast_state(&json!({"fast_mode_state":"on"}));
    let items = completed_notices(&events.try_iter().collect::<Vec<_>>());
    assert_eq!(items.len(), 4);
    for (pair, kind) in items
        .chunks(2)
        .zip([notice_kind::SILENCE, notice_kind::FAST_MODE])
    {
        assert_eq!(pair[0].id, pair[1].id);
        assert_notice(&pair[0], kind, false);
        assert_notice(&pair[1], kind, true);
        assert!(matches!(
            pair[1].body,
            ItemBody::Notice {
                level: NoticeLevel::Info,
                ..
            }
        ));
    }
}

#[test]
fn authentication_errors_are_not_agent_messages_and_clear_after_a_successful_turn() {
    let (sender, events) = mpsc::channel();
    let mut core = Core::new(&bare_config(), sender, "session".into());
    core.ready = true;
    core.on_frame(
        &json!({"type":"assistant","error":"authentication_failed","message":{
            "model":"<synthetic>","content":[{"type":"text","text":"Please log in."}]
        }}),
    );
    core.on_frame(&json!({"type":"result","is_error":true,"result":"Please log in."}));
    let failed = events.try_iter().collect::<Vec<_>>();
    let items = completed_notices(&failed);
    assert_eq!(items.len(), 1);
    assert_notice(&items[0], notice_kind::AUTH_REQUIRED, false);
    assert!(agent_texts(&fold(&failed)).is_empty());
    core.ensure_turn();
    core.on_frame(&json!({"type":"result","subtype":"success","result":"done"}));
    let resolved = completed_notices(&events.try_iter().collect::<Vec<_>>());
    assert_eq!(resolved.len(), 1);
    assert_eq!(items[0].id, resolved[0].id);
    assert_notice(&resolved[0], notice_kind::AUTH_REQUIRED, true);
}

#[test]
fn failed_results_use_readable_subtypes_or_provider_errors() {
    for (subtype, errors, expected) in [
        (
            "error_max_turns",
            json!([]),
            "Claude stopped: it reached the maximum number of turns.",
        ),
        (
            "error_during_execution",
            json!([]),
            "Claude stopped because of an error during execution.",
        ),
        (
            "error_during_execution",
            json!(["first", "second"]),
            "first\nsecond",
        ),
    ] {
        let (sender, events) = mpsc::channel();
        let mut core = Core::new(&bare_config(), sender, "session".into());
        core.ensure_turn();
        core.on_result(&json!({"subtype":subtype,"errors":errors}));
        let items = completed_notices(&events.try_iter().collect::<Vec<_>>());
        assert_eq!(items.len(), 1);
        assert_notice(&items[0], notice_kind::TURN_FAILED, false);
        assert!(matches!(&items[0].body, ItemBody::Notice {text, ..} if text == expected));
    }
    for error in ["billing_error", "rate_limit"] {
        let (sender, events) = mpsc::channel();
        let mut core = Core::new(&bare_config(), sender, "session".into());
        core.on_assistant(&json!({"error":error,"message":{"model":"<synthetic>","content":[{"type":"text","text":"provider failed"}]}}));
        core.on_result(&json!({"is_error":true,"result":"provider failed"}));
        let seen = events.try_iter().collect::<Vec<_>>();
        assert_eq!(agent_texts(&fold(&seen)), ["provider failed"]);
        let items = completed_notices(&seen);
        assert_eq!(items.len(), 1);
        assert_notice(&items[0], notice_kind::TURN_FAILED, false);
        assert!(matches!(&items[0].body, ItemBody::Notice {text, ..} if text != "provider failed"));
    }
}

#[test]
fn a_refused_fast_mode_change_is_a_setting_refused_notice() {
    let text = format!(
        "{}\n{}",
        fixture("claude/idle_only.ndjson"),
        json!({
            "type":"expect", "frame":{"type":"control_request","request":{"subtype":"apply_flag_settings"}},
            "reply":[{"type":"control_response","response":{"subtype":"error","request_id":"$request_id","error":"Fast mode refused"}}]
        })
    );
    let fake = Fake::new(&[&text]);
    let (sender, events) = mpsc::channel();
    let mut driver = start_with(fake.config(Provider::Claude), sender, fast()).unwrap();
    until(&events, is_idle);
    driver.command(configure_fast(true)).unwrap();
    let seen = until(&events, is_notice);
    let items = completed_notices(&seen);
    assert_eq!(items.len(), 1);
    assert_notice(&items[0], notice_kind::SETTING_REFUSED, false);
    assert!(
        matches!(&items[0].body, ItemBody::Notice {text, ..} if text.contains("Fast mode refused"))
    );
    assert!(!fake.saw("mismatch"));
}

#[test]
fn repeated_failed_results_emit_one_turn_failed_notice() {
    let (sender, events) = mpsc::channel();
    let mut core = Core::new(&bare_config(), sender, "session".into());
    core.state_events = true;
    core.ensure_turn();
    for _ in 0..2 {
        core.on_result(&json!({"is_error":true,"errors":["failed"]}));
    }
    let items = completed_notices(&events.try_iter().collect::<Vec<_>>());
    assert_eq!(items.len(), 1);
    assert_notice(&items[0], notice_kind::TURN_FAILED, false);
}

#[test]
fn restored_auth_notice_resolves_on_the_first_successful_turn() {
    let (sender, events) = mpsc::channel();
    let mut config = bare_config();
    let mut previous = Core::new(&config, sender.clone(), "session".into());
    previous.notice(
        NoticeLevel::Error,
        notice_kind::AUTH_REQUIRED,
        "Please log in.",
    );
    config.outstanding_notices = previous.open_notices.clone();
    let original = completed_notices(&events.try_iter().collect::<Vec<_>>());
    let mut core = Core::new(&config, sender, "session".into());
    core.ensure_turn();
    core.on_result(&json!({"subtype":"success","result":"done"}));
    let resolved = completed_notices(&events.try_iter().collect::<Vec<_>>());
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].id, original[0].id);
    assert_notice(&resolved[0], notice_kind::AUTH_REQUIRED, true);
}

#[test]
fn restored_rate_limits_remember_the_status_and_allowed_resolves_the_saved_item() {
    for (level, status) in [
        (NoticeLevel::Warning, "allowed_warning"),
        (NoticeLevel::Error, "rejected"),
    ] {
        let (sender, events) = mpsc::channel();
        let mut config = bare_config();
        let mut previous = Core::new(&config, sender.clone(), "session".into());
        previous.notice(level, "rate_limit:seven_day", "Weekly usage limit.");
        config.outstanding_notices = previous.open_notices.clone();
        let original = completed_notices(&events.try_iter().collect::<Vec<_>>());
        let mut core = Core::new(&config, sender, "session".into());
        core.ready = true;
        assert_eq!(core.rate_status["seven_day"], status);
        core.on_frame(&json!({"type":"rate_limit_event","rate_limit_info":{
            "status":status,"rateLimitType":"seven_day"
        }}));
        assert!(events.try_iter().next().is_none());
        core.on_frame(&json!({"type":"rate_limit_event","rate_limit_info":{
            "status":"allowed","rateLimitType":"seven_day"
        }}));
        let resolved = completed_notices(&events.try_iter().collect::<Vec<_>>());
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].id, original[0].id);
        assert_notice(&resolved[0], "rate_limit:seven_day", true);
    }
}

#[test]
fn rate_windows_include_low_utilization_and_unified_windows_without_warning_notices() {
    let (sender, events) = mpsc::channel();
    let mut core = Core::new(&bare_config(), sender, "session".into());
    core.ready = true;
    let warning = json!({"type":"rate_limit_event", "rate_limit_info":{
        "status":"allowed_warning", "rateLimitType":"five_hour", "utilization":0.3, "resetsAt":1767225600000u64,
        "unifiedWindows":{
            "five_hour":{"utilization":0.3,"resetsAt":1767225600},
            "seven_day":{"utilization":0.87,"resetsAt":1767300000},
            "seven_day_opus":{"utilization":0.7}, "seven_day_sonnet":{"utilization":0.69},
            "overage":{"utilization":0.1}
        }
    }});
    core.on_frame(&warning);
    let seen = events.try_iter().collect::<Vec<_>>();
    assert!(completed_notices(&seen).is_empty());
    let t = fold(&seen);
    assert_eq!(t.rate_limits.len(), 5);
    let five = t.rate_limits.iter().find(|w| w.id == "five_hour").unwrap();
    assert_eq!(
        (
            five.label.as_str(),
            five.used_percent,
            five.resets_at,
            five.warn_at
        ),
        ("5h", 30.0, Some(1767225600), 70.0)
    );
    assert!(t.rate_limits.iter().all(|w| w.warn_at == 70.0));
    assert_eq!(
        t.rate_limits
            .iter()
            .find(|w| w.id == "seven_day_opus")
            .unwrap()
            .label,
        "weekly Opus"
    );
    core.on_frame(&warning);
    assert!(
        events.try_iter().next().is_none(),
        "unchanged windows do not repeat"
    );
    core.on_frame(&json!({"type":"rate_limit_event","rate_limit_info":{
        "status":"allowed_warning","rateLimitType":"five_hour","utilization":0.7
    }}));
    let seen = events.try_iter().collect::<Vec<_>>();
    assert!(completed_notices(&seen).is_empty());
    assert!(
        matches!(&seen[0], ChatEvent::RateLimits { windows } if windows.iter().any(|w| w.id == "five_hour" && w.used_percent == w.warn_at))
    );
    core.on_frame(&json!({"type":"rate_limit_event","rate_limit_info":{
        "status":"rejected","rateLimitType":"overage","utilization":1.0,"resetsAt":1767300000000u64
    }}));
    let seen = events.try_iter().collect::<Vec<_>>();
    let notices = completed_notices(&seen);
    assert_eq!(notices.len(), 1);
    assert!(
        matches!(&notices[0].body, ItemBody::Notice { level: NoticeLevel::Error, kind: Some(kind), resets_at: Some(1767300000), .. } if kind == "rate_limit:overage")
    );
    // A repeated rejection with a later reset is a new occurrence, even if status is unchanged.
    core.on_frame(&json!({"type":"rate_limit_event","rate_limit_info":{
        "status":"rejected","rateLimitType":"overage","resetsAt":1767400000
    }}));
    let later = completed_notices(&events.try_iter().collect::<Vec<_>>());
    assert_eq!(later.len(), 1);
    assert_ne!(later[0].id, notices[0].id);
}

#[test]
fn system_frames_report_only_hashed_login_identity_to_the_host() {
    let (sender, events) = mpsc::channel();
    let mut core = Core::new(&bare_config(), sender, "session".into());
    core.on_system(&json!({"type":"system","subtype":"init","apiKeySource":"none","oauthAccount":{"accountUuid":"account-a","emailAddress":"private@example.test"}}));
    let first = events
        .try_iter()
        .find_map(|event| match event {
            ChatEvent::ProviderAccountIdentity { identity } => identity,
            _ => None,
        })
        .unwrap();
    core.on_system(
        &json!({"type":"system","subtype":"status","account":{"email":"another@example.test"}}),
    );
    let second = events
        .try_iter()
        .find_map(|event| match event {
            ChatEvent::ProviderAccountIdentity { identity } => identity,
            _ => None,
        })
        .unwrap();
    assert_ne!(first, second);
    assert!(first.scope.starts_with("sha256:"));
    assert!(!first.scope.contains("private"));
}
