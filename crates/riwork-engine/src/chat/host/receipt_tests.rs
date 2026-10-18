use super::*;
use crate::chat::client::CallError;
use crate::chat::receipts::{self, CommandStatus, Reservation};
use crate::chat::testing::*;

fn message(text: &str) -> ChatCommand {
    ChatCommand::Send { text: text.into() }
}

#[test]
fn identified_command_duplicate_send_and_key_collisions_do_not_dispatch_again() {
    let mut host = TestHost::new();
    let chat = host.create(Provider::Codex);
    let first = host
        .client()
        .command_identified(&chat.id, "send-1", message("once"))
        .unwrap();
    assert_eq!(first.status, CommandStatus::Accepted);
    assert_eq!(
        host.client()
            .command_identified(&chat.id, "send-1", message("once"))
            .unwrap(),
        first
    );
    assert_eq!(host.fake().commands(), vec![message("once")]);
    let different = host
        .client()
        .command_identified(&chat.id, "send-1", message("different"))
        .unwrap_err();
    assert!(
        matches!(different, CallError::Refused(error) if error.starts_with(receipts::OPERATION_CONFLICT))
    );
    let other = host.create(Provider::Claude);
    let different_chat = host
        .client()
        .command_identified(&other.id, "send-1", message("once"))
        .unwrap_err();
    assert!(
        matches!(different_chat, CallError::Refused(error) if error.starts_with(receipts::OPERATION_CONFLICT))
    );
    host.restart(quick_options());
    let starts = host.fake().start_count();
    assert_eq!(
        host.client().command_receipt(&chat.id, "send-1").unwrap(),
        Some(first.clone())
    );
    assert_eq!(
        host.client()
            .command_identified(&chat.id, "send-1", message("once"))
            .unwrap(),
        first
    );
    assert_eq!(
        host.fake().start_count(),
        starts,
        "a retry must not resume the provider"
    );
    assert_eq!(host.fake().commands(), vec![message("once")]);
}

#[test]
fn identified_command_uncertain_crash_windows_never_replay_after_restart() {
    // Exercise the two durable crash boundaries, with and without an external
    // side effect. Deliberately omit settlement, as a terminated process would.
    for sent_before_crash in [false, true] {
        let mut host = TestHost::new();
        let chat = host.create(Provider::Codex);
        let command = message("possibly received");
        assert!(matches!(
            receipts::reserve(&host.home, &chat.id, "crash", &command, false).unwrap(),
            Reservation::New(_)
        ));
        if sent_before_crash {
            host.client()
                .command_checked(&chat.id, command.clone())
                .unwrap();
        }
        host.restart(quick_options());
        let starts = host.fake().start_count();
        let receipt = host
            .client()
            .command_identified(&chat.id, "crash", command.clone())
            .unwrap();
        assert_eq!(receipt.status, CommandStatus::Uncertain);
        assert_eq!(
            host.client().command_receipt(&chat.id, "crash").unwrap(),
            Some(receipt)
        );
        assert_eq!(host.fake().commands().len(), usize::from(sent_before_crash));
        assert_eq!(host.fake().start_count(), starts);
    }
}

#[test]
fn identified_command_noop_stop_retry_cannot_stop_a_future_turn() {
    let host = TestHost::new();
    let chat = host.create(Provider::Codex);
    host.client().close(&chat.id).unwrap();
    let first = host
        .client()
        .command_identified(&chat.id, "old-stop", ChatCommand::Stop)
        .unwrap();
    assert_eq!(first.status, CommandStatus::Accepted);
    host.client().command(&chat.id, message("hang")).unwrap();
    host.wait_for_state(&chat.id, |state| *state == ChatState::Running);
    let shutdowns = host.fake().shutdowns.load(Ordering::SeqCst);
    assert_eq!(
        host.client()
            .command_identified(&chat.id, "old-stop", ChatCommand::Stop)
            .unwrap(),
        first
    );
    assert_eq!(host.info(&chat.id).state, ChatState::Running);
    assert_eq!(host.fake().shutdowns.load(Ordering::SeqCst), shutdowns);
}

#[test]
fn identified_command_rejection_and_deleted_chat_receipts_remain_durable() {
    let mut host = TestHost::new();
    let absent = Uuid::new_v4().to_string();
    let rejected = host
        .client()
        .command_identified(&absent, "unknown-chat", message("never"))
        .unwrap();
    assert_eq!(rejected.status, CommandStatus::Rejected);
    assert!(rejected.error.as_deref().unwrap().contains("unknown chat"));
    assert_eq!(host.fake().start_count(), 0);
    let chat = host.create(Provider::Codex);
    let accepted = host
        .client()
        .command_identified(&chat.id, "completed", message("once"))
        .unwrap();
    host.client().delete(&chat.id).unwrap();
    host.restart(quick_options());
    assert_eq!(
        host.client()
            .command_receipt(&absent, "unknown-chat")
            .unwrap(),
        Some(rejected)
    );
    assert_eq!(
        host.client()
            .command_identified(&chat.id, "completed", message("once"))
            .unwrap(),
        accepted
    );
    assert_eq!(host.fake().commands(), vec![message("once")]);
}

#[test]
fn identified_command_dispatch_error_is_uncertain_and_does_not_retry() {
    let host = TestHost::new();
    let chat = host.create(Provider::Codex);
    *host.fake().fail_command.lock().unwrap() = Some("connection lost after request write".into());
    let receipt = host
        .client()
        .command_identified(&chat.id, "ambiguous", message("once"))
        .unwrap();
    assert_eq!(receipt.status, CommandStatus::Uncertain);
    assert_eq!(
        receipt.error.as_deref(),
        Some("connection lost after request write")
    );
    assert_eq!(
        host.client()
            .command_identified(&chat.id, "ambiguous", message("once"))
            .unwrap(),
        receipt
    );
    assert_eq!(host.fake().commands(), vec![message("once")]);
}

#[test]
fn identified_command_opt_in_preserves_legacy_repeated_sends() {
    let host = TestHost::new();
    let chat = host.create(Provider::Codex);
    for _ in 0..2 {
        host.client().command(&chat.id, message("again")).unwrap();
    }
    assert_eq!(
        host.fake().commands(),
        vec![message("again"), message("again")]
    );
    assert_eq!(
        host.client().command_receipt(&chat.id, "not-used").unwrap(),
        None
    );
    assert!(!host.home.join("chat-command-receipts").exists());
}

#[test]
fn identified_command_running_guard_cannot_resume_or_be_reused_without_guard() {
    let host = TestHost::new();
    let chat = host.create(Provider::Codex);
    host.client().close(&chat.id).unwrap();
    let before = host.fake().start_count();
    let receipt = host
        .client()
        .command_identified_running(&chat.id, "guarded", message("never"))
        .unwrap();
    assert_eq!(receipt.status, CommandStatus::Rejected);
    assert_eq!(host.fake().start_count(), before);
    assert!(host.fake().commands().is_empty());
    assert!(
        matches!(host.client().command_identified(&chat.id, "guarded", message("never")), Err(CallError::Refused(error)) if error.starts_with(receipts::OPERATION_CONFLICT))
    );
}
