use super::*;
use crate::chat::driver::{Driver, DriverConfig};
use crate::chat::model::{Item, ItemStatus};
use crate::chat::testing::{TestHost, fake_for, fake_providers, quick_options};
use std::sync::mpsc::Sender;

// Each driver owns its event sender. The general fixture's newest-sender slot
// cannot model parent and child providers running in the same working directory.
struct TaskDriver {
    config: DriverConfig,
    events: Sender<ChatEvent>,
    session: String,
    turns: usize,
}

impl Driver for TaskDriver {
    fn command(&mut self, command: ChatCommand) -> Result<(), String> {
        fake_for(&self.config.cwd)
            .commands
            .lock()
            .unwrap()
            .push(command.clone());
        let ChatCommand::Send { text } = command else {
            return Ok(());
        };
        self.turns += 1;
        let turn = format!("{}-{}", self.session, self.turns);
        self.events
            .send(ChatEvent::TurnStarted {
                turn_id: turn.clone(),
                started_at: None,
            })
            .unwrap();
        self.events
            .send(ChatEvent::State {
                state: ChatState::Running,
            })
            .unwrap();
        if text == "hang" {
            return Ok(());
        }
        if text == "uncertain" {
            return Err("provider response was lost".into());
        }
        self.events
            .send(ChatEvent::ItemCompleted {
                item: Item {
                    started_at: None,
                    ended_at: None,
                    presentation: Default::default(),
                    id: format!("reply-{turn}"),
                    turn_id: Some(turn.clone()),
                    status: ItemStatus::Completed,
                    body: ItemBody::AgentMessage {
                        text: format!("result: {text}"),
                    },
                },
            })
            .unwrap();
        self.events
            .send(ChatEvent::TurnCompleted {
                turn_id: turn,
                outcome: TurnOutcome::Completed,
                ended_at: None,
            })
            .unwrap();
        self.events
            .send(ChatEvent::State {
                state: ChatState::Idle,
            })
            .unwrap();
        Ok(())
    }
    fn provider_thread_id(&self) -> Option<String> {
        Some(self.session.clone())
    }
    fn shutdown(&mut self) {}
}

fn start(config: DriverConfig, events: Sender<ChatEvent>) -> Result<Box<dyn Driver>, String> {
    fake_for(&config.cwd)
        .starts
        .lock()
        .unwrap()
        .push(config.clone());
    let session = config
        .resume
        .clone()
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    events
        .send(ChatEvent::State {
            state: ChatState::Idle,
        })
        .unwrap();
    Ok(Box::new(TaskDriver {
        config,
        events,
        session,
        turns: 0,
    }))
}

fn host() -> TestHost {
    let mut providers = fake_providers();
    providers.codex = start;
    providers.claude = start;
    providers.muse = start;
    TestHost::with_providers(quick_options(), providers)
}

fn parent(host: &TestHost, mode: ApprovalMode) -> ChatInfo {
    let mut request = host.new_chat(Provider::Codex);
    request.approval_mode = mode;
    let parent = host.client().create(request).unwrap();
    host.client()
        .command(
            &parent.id,
            ChatCommand::Send {
                text: "hang".into(),
            },
        )
        .unwrap();
    host.wait_for_log(&parent.id, |events| {
        events
            .iter()
            .any(|event| matches!(event.event, ChatEvent::TurnStarted { .. }))
    });
    host.wait_for_state(&parent.id, |state| *state == ChatState::Running)
}

fn request(id: &str, prompt: &str) -> DelegateRequest {
    DelegateRequest {
        request_id: id.into(),
        prompt: prompt.into(),
        title: Some("Delegated work".into()),
        provider: None,
        model: None,
        effort: None,
        approval_mode: None,
    }
}

#[test]
fn delegated_task_late_dispatch_receipt_cannot_overwrite_completion_or_cancellation() {
    let host = host();
    let parent = parent(&host, ApprovalMode::Supervised);
    let service = DelegationService::at(&host.home, &parent.id).unwrap();
    let task = service
        .delegate(
            request("finished-before-ack", "done"),
            Duration::from_secs(2),
        )
        .unwrap();
    assert_eq!(task.status, TaskStatus::Completed);
    let receipt = receipts::read(
        &host.home,
        &task.child_chat_id,
        &send_operation(&task.task_id),
    )
    .unwrap()
    .unwrap();
    let commands = host.fake().commands().len();

    // Reproduce the original launch receiving its acknowledgment after another
    // observer has durably saved the completed result. The locked merge must
    // preserve that result, including against a stale cancellation snapshot.
    let saved = service
        .update(&task.task_id, |stored| {
            stored.settle_dispatch(&receipt);
            stored.record_error("late transport failure".into());
            assert!(!stored.request_cancel());
            Ok(())
        })
        .unwrap();
    assert_eq!(saved.task, task);
    assert!(!saved.cancel_requested);
    assert_eq!(service.cancel(&task.task_id).unwrap(), task);
    assert_eq!(host.fake().commands().len(), commands);

    let waiting = service
        .delegate(request("cancel-before-ack", "hang"), Duration::ZERO)
        .unwrap();
    let receipt = receipts::read(
        &host.home,
        &waiting.child_chat_id,
        &send_operation(&waiting.task_id),
    )
    .unwrap()
    .unwrap();
    let saved = service
        .update(&waiting.task_id, |stored| {
            assert!(stored.request_cancel());
            stored.settle_dispatch(&receipt);
            Ok(())
        })
        .unwrap();
    assert_eq!(saved.task.status, TaskStatus::Cancelling);
    assert!(saved.cancel_requested);
}

#[test]
fn delegated_task_keeps_global_parent_result_and_deduplicates_across_restart() {
    let mut host = host();
    let parent = parent(&host, ApprovalMode::AutoEdit);
    let service = DelegationService::at(&host.home, &parent.id).unwrap();
    let request = request("review", "review this");
    let task = service
        .delegate(request.clone(), Duration::from_secs(2))
        .unwrap();
    assert_eq!(task.status, TaskStatus::Completed);
    assert_eq!(task.parent_chat_id, parent.id);
    assert_eq!(task.approval_mode, ApprovalMode::AutoEdit);
    let child = host.info(&task.child_chat_id);
    assert_eq!(
        child.parent_id.as_deref(),
        Some(parent.id.as_str()),
        "global native parents retain worker lineage"
    );
    assert_eq!(child.project_id, None);
    assert_eq!(child.cwd, parent.cwd);
    assert_eq!(task.result.as_ref().unwrap().text, "result: review this");
    assert!(task.result.as_ref().unwrap().completion_seq > 0);
    let starts = host.fake().start_count();
    let sends = host.fake().commands().len();
    assert_eq!(
        service.delegate(request.clone(), Duration::ZERO).unwrap(),
        task
    );
    host.restart(quick_options());
    assert_eq!(service.delegate(request, Duration::ZERO).unwrap(), task);
    assert_eq!(host.fake().start_count(), starts);
    assert_eq!(host.fake().commands().len(), sends);
    host.client().delete(&task.child_chat_id).unwrap();
    assert_eq!(
        service.status(&task.task_id).unwrap(),
        task,
        "saved result survives deletion of child history"
    );
}

#[test]
fn delegated_task_unaccepted_assignment_cannot_claim_another_child_turn() {
    let host = host();
    let parent = parent(&host, ApprovalMode::Supervised);
    let service = DelegationService::at(&host.home, &parent.id).unwrap();
    let completed = service
        .delegate(
            request("other-input", "another assignment"),
            Duration::from_secs(2),
        )
        .unwrap();
    assert_eq!(completed.status, TaskStatus::Completed);
    let template = service.load(&completed.task_id).unwrap();
    let commands = host.fake().commands().len();
    for (key, receipt_status) in [
        ("not-sent", None),
        ("unknown-send", Some(CommandStatus::Uncertain)),
        ("refused-send", Some(CommandStatus::Rejected)),
    ] {
        // A task has reserved a child whose first log turn was produced by
        // somebody else's input before this assignment could be admitted.
        let mut stored = template.clone();
        stored.request = request(key, "my assignment");
        stored.task.task_id = task_id(&parent.id, key);
        stored.task.request_id = key.into();
        stored.task.assignment = "my assignment".into();
        stored.task.status = TaskStatus::Uncertain;
        stored.task.result = None;
        stored.cursor = 0;
        stored.last_seq = 0;
        stored.turn_id = None;
        stored.output.clear();
        write(&host.home, &stored).unwrap();
        if let Some(status) = receipt_status {
            let operation = send_operation(&stored.task.task_id);
            let receipts::Reservation::New(reserved) = receipts::reserve(
                &host.home,
                &stored.task.child_chat_id,
                &operation,
                &ChatCommand::Send {
                    text: "my assignment".into(),
                },
                true,
            )
            .unwrap() else {
                panic!()
            };
            if status == CommandStatus::Rejected {
                receipts::settle(
                    &host.home,
                    &reserved.outcome(status, Some("child already received another input".into())),
                )
                .unwrap();
            }
        }
        let task = service.status(&stored.task.task_id).unwrap();
        assert_eq!(
            task.status,
            if receipt_status == Some(CommandStatus::Rejected) {
                TaskStatus::Failed
            } else {
                TaskStatus::Uncertain
            }
        );
        assert!(
            task.result.is_none(),
            "an unaccepted assignment must not claim another result"
        );
        assert_eq!(service.load(&stored.task.task_id).unwrap().cursor, 0);
    }
    assert_eq!(host.fake().commands().len(), commands);
}

#[test]
fn delegated_task_rejects_permission_escalation_and_changed_request() {
    let host = host();
    let parent = parent(&host, ApprovalMode::Supervised);
    let service = DelegationService::at(&host.home, &parent.id).unwrap();
    let mut elevated = request("elevated", "do work");
    elevated.approval_mode = Some(ApprovalMode::Full);
    assert!(
        service
            .delegate(elevated, Duration::ZERO)
            .unwrap_err()
            .contains("broader permissions")
    );
    assert_eq!(host.fake().start_count(), 1);
    let initial = request("stable", "done");
    service.delegate(initial, Duration::from_secs(2)).unwrap();
    let starts = host.fake().start_count();
    assert!(
        service
            .delegate(request("stable", "different"), Duration::ZERO)
            .unwrap_err()
            .contains("DELEGATION_REQUEST_CONFLICT")
    );
    assert_eq!(host.fake().start_count(), starts);
}

#[test]
fn delegated_task_plan_parent_cannot_grant_execution_and_child_can_be_restricted() {
    let host = host();
    let parent = parent(&host, ApprovalMode::Plan);
    let service = DelegationService::at(&host.home, &parent.id).unwrap();
    let mut elevated = request("not-allowed", "do work");
    elevated.approval_mode = Some(ApprovalMode::Supervised);
    assert!(service.delegate(elevated, Duration::ZERO).is_err());
    let task = service
        .delegate(request("plan", "only plan"), Duration::from_secs(2))
        .unwrap();
    assert_eq!(
        host.info(&task.child_chat_id).approval_mode,
        ApprovalMode::Plan
    );
}

#[test]
fn delegated_task_parent_scope_and_stopped_parent_are_enforced() {
    let host = host();
    let first = parent(&host, ApprovalMode::Full);
    let second = parent(&host, ApprovalMode::Full);
    let service = DelegationService::at(&host.home, &first.id).unwrap();
    let other = DelegationService::at(&host.home, &second.id).unwrap();
    let task = service
        .delegate(request("owned", "hang"), Duration::ZERO)
        .unwrap();
    assert!(
        other
            .status(&task.task_id)
            .unwrap_err()
            .contains("another parent")
    );
    assert!(
        other
            .cancel(&task.task_id)
            .unwrap_err()
            .contains("another parent")
    );
    host.client().close(&first.id).unwrap();
    let before = host.fake().start_count();
    assert!(
        service
            .delegate(request("new", "must not start"), Duration::ZERO)
            .unwrap_err()
            .contains("no active turn")
    );
    assert_eq!(host.fake().start_count(), before);
    // Observation and explicit cancellation still work for this parent's child.
    let cancelled = service.cancel(&task.task_id).unwrap();
    assert_eq!(cancelled.status, TaskStatus::Cancelled);
    assert_eq!(service.cancel(&task.task_id).unwrap(), cancelled);
    assert_eq!(host.fake().start_count(), before);
}

#[test]
fn delegated_task_uncertain_send_and_wait_never_resend_or_restart() {
    let mut host = host();
    let parent = parent(&host, ApprovalMode::Supervised);
    let service = DelegationService::at(&host.home, &parent.id).unwrap();
    let request = request("lost-reply", "uncertain");
    let task = service
        .delegate(request.clone(), Duration::from_secs(2))
        .unwrap();
    assert_eq!(task.status, TaskStatus::Uncertain);
    let starts = host.fake().start_count();
    let sends = host.fake().commands().len();
    assert_eq!(
        service
            .delegate(request.clone(), Duration::ZERO)
            .unwrap()
            .status,
        TaskStatus::Uncertain
    );
    host.restart(quick_options());
    let recovered = service.delegate(request, Duration::ZERO).unwrap();
    assert!(matches!(
        recovered.status,
        TaskStatus::Uncertain | TaskStatus::Interrupted
    ));
    assert_eq!(host.fake().start_count(), starts);
    assert_eq!(host.fake().commands().len(), sends);
}

#[test]
fn delegated_task_wait_is_bounded_and_cancellation_does_not_touch_later_turns() {
    let host = host();
    let parent = parent(&host, ApprovalMode::Supervised);
    let service = DelegationService::at(&host.home, &parent.id).unwrap();
    let task = service
        .delegate(request("long", "hang"), Duration::ZERO)
        .unwrap();
    let began = Instant::now();
    assert_eq!(
        service
            .wait(&task.task_id, Duration::from_millis(75))
            .unwrap()
            .status,
        TaskStatus::Running
    );
    assert!(began.elapsed() < Duration::from_secs(1));
    let cancelled = service.cancel(&task.task_id).unwrap();
    assert_eq!(cancelled.status, TaskStatus::Cancelled);
    host.client()
        .command(
            &task.child_chat_id,
            ChatCommand::Send {
                text: "hang".into(),
            },
        )
        .unwrap();
    host.wait_for_state(&task.child_chat_id, |state| *state == ChatState::Running);
    assert_eq!(service.cancel(&task.task_id).unwrap(), cancelled);
    assert_eq!(host.info(&task.child_chat_id).state, ChatState::Running);
}

#[test]
fn delegated_task_completed_result_remains_bound_to_its_first_turn() {
    let host = host();
    let parent = parent(&host, ApprovalMode::Supervised);
    let service = DelegationService::at(&host.home, &parent.id).unwrap();
    let task = service
        .delegate(request("first", "first answer"), Duration::from_secs(2))
        .unwrap();
    host.client()
        .command(
            &task.child_chat_id,
            ChatCommand::Send {
                text: "a later answer".into(),
            },
        )
        .unwrap();
    assert_eq!(service.status(&task.task_id).unwrap(), task);
    assert_eq!(service.cancel(&task.task_id).unwrap(), task);
}

#[test]
fn delegated_task_global_parent_attribution_rejects_cross_project_children() {
    let host = host();
    let global = parent(&host, ApprovalMode::Supervised);
    let mut child = host.new_chat(Provider::Codex);
    child.parent_id = Some(global.id.clone());
    child.project_id = Some(Uuid::new_v4().to_string());
    let child = host.client().create(child).unwrap();
    assert_eq!(child.parent_id, None);
    let mut other = host.new_chat(Provider::Codex);
    other.parent_id = Some(child.id);
    assert_eq!(host.client().create(other).unwrap().parent_id, None);
}
