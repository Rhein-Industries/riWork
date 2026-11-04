//! Remote chat exercises the production view/worker and durable composer receipts
//! over an in-memory RPC peer. No host connection, provider, or real home is used.
use riwork_engine::{
    chat::model::{ChatCommand, Decision},
    remote_hosts::RemoteError,
};
use riwork_nsh_native::chat::{
    ChatBackend, ChatHandle, drafts, dto::SubmitRequest, remote_feed::Target,
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
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

#[test]
fn remote_view_recovers_a_gap_preserves_requests_and_never_resends_an_uncertain_submit() {
    let home = std::env::temp_dir().join(format!("somtus-remote-chat-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
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
    };
    let backend = Arc::new(NoLocalChat(home.clone()));
    let handle = ChatHandle::start_remote_with_backend(target.clone(), backend);
    let receiver = handle.subscribe_channel();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
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
    std::fs::remove_dir_all(home).unwrap();
}
