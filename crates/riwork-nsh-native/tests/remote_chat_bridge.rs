//! Remote chat exercises the production view/worker and durable composer receipts
//! over an in-memory RPC peer. No host connection, provider, or real home is used.
use riwork_engine::{
    chat::model::{ChatCommand, Decision},
    remote_hosts::RemoteError,
};
use riwork_nsh_native::chat::{
    ChatBackend, ChatHandle, drafts,
    dto::{ChatDelivery, ChatPanelDto, SubmitRequest},
    remote_feed::Target,
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

struct NoLocalChat(PathBuf);
impl ChatBackend for NoLocalChat {
    fn home(&self) -> Result<PathBuf, String> {
        Ok(self.0.clone())
    }
    fn existing(&self) -> Result<PathBuf, String> {
        panic!("remote read tried the local chat socket")
    }
    fn ensure(&self) -> Result<PathBuf, String> {
        panic!("remote mutation tried the local chat socket")
    }
}

/// The drafts store is one per process, so every test in this binary shares one home. It stays
/// behind in the build's own scratch folder, as no test can know it is the last one using it.
fn shared_home() -> PathBuf {
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    HOME.get_or_init(|| {
        let home = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("somtus-remote-chat-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        home
    })
    .clone()
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn remote_view_recovers_a_gap_preserves_requests_and_never_resends_an_uncertain_submit() {
    let home = shared_home();
    let chat = uuid::Uuid::new_v4().to_string();
    let reads = Arc::new(AtomicUsize::new(0));
    let calls = Arc::new(Mutex::new(Vec::<(String, String, Value)>::new()));
    let recorded = calls.clone();
    let attempts = reads.clone();
    let expected_chat = chat.clone();
    let target = Target {
        host: "host-a".into(),
        chat: chat.clone(),
        rpc: Arc::new(move |host, method, params, _| {
            assert_eq!(params["chat_id"], expected_chat);
            recorded
                .lock()
                .unwrap()
                .push((host.into(), method.into(), params.clone()));
            if method == "chat.command" {
                return if params["command"]["command"] == "send" {
                    Err(RemoteError::Timeout)
                } else {
                    Ok(json!({"status":"ok"}))
                };
            }
            assert_eq!(method, "chat.events");
            let attempt = attempts.fetch_add(1, Ordering::SeqCst);
            let approval = json!({"event":"approval_requested","approval":{"request_id":"allow-1","kind":"command","title":"host command","choices":["accept","decline"]}});
            let question = json!({"event":"question_requested","question":{"request_id":"question-1","questions":[{"question":"Which branch?","options":[{"label":"main"}]}]}});
            if attempt < 2 {
                assert_eq!(
                    params["since"], 0,
                    "a malformed page must not advance even its valid prefix"
                );
                return Ok(
                    json!({"chat_id":expected_chat,"events":[{"seq":1,"event":approval},{"seq":if attempt == 0 {3} else {2},"event":question}],"next":if attempt == 0 {3} else {2},"more":false}),
                );
            }
            assert_eq!(params["since"], 2);
            Ok(json!({"chat_id":expected_chat,"events":[],"next":2,"more":false}))
        }),
        route_removed: Arc::new(|_| panic!("nothing here makes the daemon unreachable")),
        removed: Default::default(),
    };
    let backend = Arc::new(NoLocalChat(home.clone()));
    let handle = ChatHandle::start_remote_with_backend(target.clone(), backend);
    let receiver = handle.subscribe_channel();
    let runtime = runtime();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let delivery = runtime.block_on(async {
            tokio::time::timeout(
                deadline.saturating_duration_since(Instant::now()),
                receiver.recv(),
            )
            .await
            .unwrap()
            .unwrap()
        });
        if let Some(panel) = delivery.panel {
            if panel.link == "live" {
                assert_eq!(panel.approvals.len(), 1);
                assert_eq!(panel.questions.len(), 1);
                break;
            }
        }
    }
    let parts = handle.parts();
    runtime
        .block_on(parts.exchange(
            ChatCommand::Approve {
                request_id: "allow-1".into(),
                decision: Decision::Accept,
            },
            false,
        ))
        .unwrap();
    runtime
        .block_on(parts.exchange(
            ChatCommand::Answer {
                request_id: "question-1".into(),
                answers: vec![vec!["main".into()]],
            },
            false,
        ))
        .unwrap();
    let receipt = runtime.block_on(parts.submit(SubmitRequest {
        text: "only once".into(),
        attachments: vec![],
        editor: drafts::new_editor(),
        generation: 1,
    }));
    assert_eq!(receipt.status, "uncertain");
    assert!(!receipt.clear);
    assert!(receipt.key.is_some());
    drafts::flush_pending(Duration::from_secs(5)).unwrap();
    let restored = runtime.block_on(parts.restore_draft()).unwrap().unwrap();
    assert_eq!(restored.submissions.len(), 1);
    assert_eq!(restored.submissions[0].status, "uncertain");
    let other = Target {
        host: "host-b".into(),
        ..target.clone()
    };
    assert_ne!(target.draft_key(), other.draft_key());
    assert_eq!(
        uuid::Uuid::parse_str(&target.draft_key())
            .unwrap()
            .to_string(),
        target.draft_key()
    );
    assert!(home.join("chat-drafts").join(target.draft_key()).is_file());
    assert!(!home.join("chat-drafts").join(other.draft_key()).exists());
    let recorded = calls.lock().unwrap();
    let commands: Vec<_> = recorded
        .iter()
        .filter(|(_, method, _)| method == "chat.command")
        .collect();
    assert_eq!(commands.len(), 3);
    assert!(commands.iter().all(|(host, _, _)| host == "host-a"));
    assert_eq!(
        commands
            .iter()
            .map(|(_, _, p)| p["command"]["command"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["approve", "answer", "send"]
    );
    drop(recorded);
    drop(parts);
    drop(handle);
}

/// A failure the feed's events do not share: only commands meet it.
#[test]
fn a_command_that_never_left_is_refused_and_one_that_may_have_left_is_uncertain() {
    let runtime = runtime();
    for (error, status) in [
        (
            RemoteError::Rpc {
                code: "offline".into(),
                message: "host is offline: relay closed".into(),
            },
            "refused",
        ),
        (
            RemoteError::NeedsSetup {
                cause: "identity_changed".into(),
                message: "This host's identity changed. Remove and add it again.".into(),
            },
            "refused",
        ),
        (
            RemoteError::Unreachable("Cannot start the connection to this Mac".into()),
            "refused",
        ),
        (RemoteError::Lost("the session ended".into()), "uncertain"),
        (RemoteError::Timeout, "uncertain"),
    ] {
        let chat = uuid::Uuid::new_v4().to_string();
        let exchanges = Arc::new(AtomicUsize::new(0));
        let target = Target {
            host: "host-refusing".into(),
            chat: chat.clone(),
            rpc: {
                let (error, exchanges) = (error.clone(), exchanges.clone());
                Arc::new(move |_, method, params, _| {
                    if method == "chat.command" {
                        exchanges.fetch_add(1, Ordering::SeqCst);
                        return Err(error.clone());
                    }
                    Ok(json!({"chat_id":chat,"events":[],"next":params["since"],"more":false}))
                })
            },
            route_removed: Arc::new(|_| panic!("the chat's own events never failed")),
            removed: Default::default(),
        };
        let backend = Arc::new(NoLocalChat(shared_home()));
        let handle = ChatHandle::start_remote_with_backend(target, backend);
        let parts = handle.parts();

        let receipt = runtime.block_on(parts.submit(SubmitRequest {
            text: "only once".into(),
            attachments: vec![],
            editor: drafts::new_editor(),
            generation: 1,
        }));
        assert_eq!(receipt.status, status, "{error:?}: {receipt:?}");
        assert!(!receipt.clear, "{error:?}: the draft stays either way");
        assert_eq!(exchanges.load(Ordering::SeqCst), 1, "{error:?}");
        drafts::flush_pending(Duration::from_secs(5)).unwrap();
        let restored = runtime.block_on(parts.restore_draft()).unwrap().unwrap();
        assert_eq!(restored.submissions[0].status, status, "{error:?}");

        // The approval bar and Stop are not held back by the link state, and go by the same rule.
        let approved = runtime
            .block_on(parts.exchange(
                ChatCommand::Approve {
                    request_id: "allow-1".into(),
                    decision: Decision::Accept,
                },
                false,
            ))
            .unwrap_err();
        let word = if status == "refused" {
            "refused:"
        } else {
            "broken:"
        };
        assert!(approved.starts_with(word), "{error:?}: {approved}");
        assert_eq!(
            exchanges.load(Ordering::SeqCst),
            2,
            "{error:?}: never retried"
        );
    }
}

/// Deliveries until `done`, given each new panel and every failure notice so far, says stop.
fn panel_until(
    runtime: &tokio::runtime::Runtime,
    receiver: &async_channel::Receiver<ChatDelivery>,
    done: impl Fn(&ChatPanelDto, &[String]) -> bool,
) -> (ChatPanelDto, Vec<String>) {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut notices = Vec::new();
    loop {
        let delivery = runtime
            .block_on(async {
                tokio::time::timeout(
                    deadline.saturating_duration_since(Instant::now()),
                    receiver.recv(),
                )
                .await
            })
            .unwrap_or_else(|_| panic!("nothing more came, after the notices {notices:?}"))
            .unwrap();
        if let Some(panel) = delivery.panel {
            notices.extend(panel.failures.iter().cloned());
            if done(&panel, &notices) {
                return (panel, notices);
            }
        }
    }
}

#[test]
fn a_removed_host_explains_itself_refuses_commands_and_the_chat_resumes_from_its_cursor() {
    const REMOVED: &str = "This host was removed from this Mac";
    let runtime = runtime();
    let chat = uuid::Uuid::new_v4().to_string();
    let gone = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
    let target = Target {
        host: "host-removed".into(),
        chat: chat.clone(),
        rpc: {
            let (gone, calls) = (gone.clone(), calls.clone());
            Arc::new(move |_, method, params, _| {
                calls.lock().unwrap().push((method.into(), params.clone()));
                if gone.load(Ordering::SeqCst) {
                    return Err(RemoteError::Unreachable(
                        "Cannot start the connection to this Mac: no host".into(),
                    ));
                }
                if method == "chat.command" {
                    return Ok(json!({"status":"ok"}));
                }
                let since = params["since"].as_u64().unwrap();
                Ok(if since == 0 {
                    json!({"chat_id":chat,"next":1,"more":false,"events":[
                        {"seq":1,"event":{"event":"state","state":{"state":"stopped"}}},
                    ]})
                } else {
                    json!({"chat_id":chat,"next":since,"more":false,"events":[]})
                })
            })
        },
        route_removed: {
            let gone = gone.clone();
            Arc::new(move |host| {
                assert_eq!(host, "host-removed");
                gone.load(Ordering::SeqCst)
            })
        },
        removed: Default::default(),
    };
    let backend = Arc::new(NoLocalChat(shared_home()));
    let handle = ChatHandle::start_remote_with_backend(target, backend);
    let receiver = handle.subscribe_channel();
    panel_until(&runtime, &receiver, |panel, _| panel.link == "live");

    // Told once and in plain words, while the chat itself is not called deleted.
    gone.store(true, Ordering::SeqCst);
    let (panel, notices) = panel_until(&runtime, &receiver, |panel, notices| {
        panel.status.as_deref() == Some(REMOVED) && !notices.is_empty()
    });
    assert_eq!(
        panel.link, "reconnecting",
        "the composer waits, as while reconnecting"
    );
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(
        notices[0].ends_with(
            "This host was removed from this Mac. Add it again, then open this chat from its \
             folder if it does not come back."
        ),
        "{notices:?}"
    );

    // From here on nothing asks the host, or starts a daemon to do it.
    let asked = calls.lock().unwrap().len();
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(calls.lock().unwrap().len(), asked);

    // A command is told at once, nothing is sent, and the composer's copy is kept.
    let parts = handle.parts();
    let receipt = runtime.block_on(parts.submit(SubmitRequest {
        text: "while it is gone".into(),
        attachments: vec![],
        editor: drafts::new_editor(),
        generation: 1,
    }));
    assert_eq!(receipt.status, "refused", "{receipt:?}");
    assert!(!receipt.clear);
    assert!(
        receipt.error.as_deref().unwrap().contains(REMOVED),
        "{receipt:?}"
    );
    let approved = runtime
        .block_on(parts.exchange(
            ChatCommand::Approve {
                request_id: "allow-1".into(),
                decision: Decision::Accept,
            },
            false,
        ))
        .unwrap_err();
    assert!(approved.starts_with("refused:"), "{approved}");
    assert_eq!(calls.lock().unwrap().len(), asked);
    drafts::flush_pending(Duration::from_secs(5)).unwrap();
    let restored = runtime.block_on(parts.restore_draft()).unwrap().unwrap();
    assert_eq!(restored.submissions[0].text, "while it is gone");
    assert_eq!(restored.submissions[0].status, "refused");

    // Paired again under the same id: on from the same cursor, with nothing more to say.
    gone.store(false, Ordering::SeqCst);
    let (panel, notices) = panel_until(&runtime, &receiver, |panel, _| panel.link == "live");
    assert_ne!(panel.status.as_deref(), Some(REMOVED));
    assert!(notices.is_empty(), "{notices:?}");
    {
        let calls = calls.lock().unwrap();
        assert_eq!(calls[asked].0, "chat.events");
        assert_eq!(calls[asked].1["since"], 1);
    }
    runtime
        .block_on(parts.exchange(ChatCommand::Interrupt, false))
        .unwrap();
    let commands = calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(method, _)| method == "chat.command")
        .count();
    assert_eq!(commands, 1);
}
