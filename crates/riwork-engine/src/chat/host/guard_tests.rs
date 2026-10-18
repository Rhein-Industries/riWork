use super::*;
use crate::chat::model::ApprovalMode;
use crate::chat::receipts::CommandStatus;
use crate::chat::testing::{fake_providers, quick_options, short_home};

// Exercise admission and durable receipts with no socket or provider process.
// Drivers deliberately emit no turn events, making queued-input races exact.
struct GuardHost {
    shared: Shared,
    _notes: Mutex<Receiver<ParentNote>>,
}

impl GuardHost {
    fn new() -> Self {
        let home = short_home();
        fs::create_dir_all(home.join("chats")).unwrap();
        let dismissals = Arc::new(Mutex::new(
            crate::chat::notice_dismissals::Dismissals::open(&home).unwrap(),
        ));
        let (worker_notes, notes) = mpsc::channel();
        Self {
            shared: Shared {
                paths: Paths::new(&home).unwrap(),
                home,
                dismissals,
                providers: fake_providers(),
                options: quick_options(),
                chats: Mutex::new(HashMap::new()),
                orchestrator_creation: Mutex::new(()),
                connections: AtomicUsize::new(0),
                activity: Mutex::new(Instant::now()),
                quit: AtomicBool::new(false),
                worker_notes,
            },
            _notes: Mutex::new(notes),
        }
    }

    fn chat(&self, parent: Option<&Arc<Chat>>, driver: Box<dyn Driver>) -> Arc<Chat> {
        let info: ChatInfo = serde_json::from_value(serde_json::json!({
            "id": Uuid::new_v4().to_string(), "provider": "codex",
            "parent_id": parent.map(|chat| chat.info().id),
            "cwd": self.shared.home, "title": "guard fixture", "created_at_unix": 0,
            "approval_mode": "full", "state": {"state": "idle"}
        }))
        .unwrap();
        let dir = log::chat_dir(&self.shared.home, &info.id).unwrap();
        let chat_log = ChatLog::create(&dir, &info).unwrap();
        let mut inner = Inner::new(info.clone(), chat_log, 0, self.shared.dismissals.clone());
        inner.append(ChatEvent::Info { info: info.clone() });
        inner.generations = 1;
        let mut run = Run::starting(1);
        run.cancel_io = driver.cancel_io();
        run.driver = Some(Arc::new(Mutex::new(driver)));
        run.reader_done.store(true, Ordering::SeqCst);
        inner.run = Some(run);
        let chat = Arc::new(Chat {
            dir,
            lifecycle: Mutex::new(()),
            inner: Mutex::new(inner),
        });
        lock(&self.shared.chats).insert(info.id, chat.clone());
        chat
    }

    fn parent(&self) -> Arc<Chat> {
        let parent = self.chat(None, Box::new(RecordingDriver::default()));
        let mut inner = lock(&parent.inner);
        inner.take_driver_event(ChatEvent::TurnStarted {
            turn_id: "parent-turn".into(),
        });
        inner.set_state(ChatState::Running);
        drop(inner);
        parent
    }

    fn send(
        &self,
        parent: &Chat,
        child: &Chat,
        operation: &str,
    ) -> super::super::receipts::CommandReceipt {
        run_identified_command(
            &self.shared,
            &child.info().id,
            operation,
            ChatCommand::Send {
                text: "assignment".into(),
            },
            true,
            Some(CommandGuard::DelegatedSend {
                parent_chat_id: parent.info().id,
                parent_turn_id: "parent-turn".into(),
            }),
        )
        .unwrap()
    }
}

impl Drop for GuardHost {
    fn drop(&mut self) {
        for chat in self.shared.all() {
            stop(&chat);
        }
        let _ = fs::remove_dir_all(&self.shared.home);
    }
}

#[derive(Default)]
struct RecordingDriver {
    commands: Arc<Mutex<Vec<ChatCommand>>>,
    shutdowns: Arc<AtomicUsize>,
}

impl Driver for RecordingDriver {
    fn command(&mut self, command: ChatCommand) -> Result<(), String> {
        lock(&self.commands).push(command);
        Ok(())
    }
    fn configure(&mut self, command: ChatCommand, reporter: DriverReporter) -> Result<(), String> {
        if let ChatCommand::Configure { approval_mode, .. } = &command {
            reporter.report(DriverReport {
                approval_mode: *approval_mode,
                ..Default::default()
            });
        }
        self.command(command)
    }
    fn provider_thread_id(&self) -> Option<String> {
        Some("guard-session".into())
    }
    fn shutdown(&mut self) {
        self.shutdowns.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn delegated_send_rechecks_parent_permission_after_child_creation() {
    let host = GuardHost::new();
    let parent = host.parent();
    let driver = RecordingDriver::default();
    let commands = driver.commands.clone();
    let child = host.chat(Some(&parent), Box::new(driver));
    run_command(
        &host.shared,
        &parent,
        ChatCommand::Configure {
            model: None,
            effort: None,
            approval_mode: Some(ApprovalMode::Supervised),
            fast: None,
            contributor_mode: None,
            contributor_opt_out: None,
            claude_billing: None,
        },
    )
    .unwrap();
    assert_eq!(parent.info().approval_mode, ApprovalMode::Supervised);
    let receipt = host.send(&parent, &child, "assignment-after-downgrade");
    assert_eq!(receipt.status, CommandStatus::Rejected);
    assert!(receipt.error.unwrap().contains("permission ceiling"));
    assert!(
        lock(&commands).is_empty(),
        "rejected assignment reached provider"
    );
    assert_eq!(lock(&child.inner).run.as_ref().unwrap().inputs, 0);
}

#[test]
fn delegated_cancel_cannot_stop_later_legacy_input_before_turn_started() {
    let host = GuardHost::new();
    let parent = host.parent();
    let driver = RecordingDriver::default();
    let shutdowns = driver.shutdowns.clone();
    let commands = driver.commands.clone();
    let child = host.chat(Some(&parent), Box::new(driver));
    assert_eq!(
        host.send(&parent, &child, "owned-assignment").status,
        CommandStatus::Accepted
    );
    run_command(
        &host.shared,
        &child,
        ChatCommand::Send {
            text: "later user input".into(),
        },
    )
    .unwrap();
    assert!(
        lock(&child.inner).open.turn.is_none(),
        "fixture must withhold TurnStarted"
    );
    let receipt = run_identified_command(
        &host.shared,
        &child.info().id,
        "stale-cancellation",
        ChatCommand::Stop,
        false,
        Some(CommandGuard::DelegatedCancel {
            parent_chat_id: parent.info().id,
            send_operation_id: "owned-assignment".into(),
            turn_id: None,
            allow_unstarted: true,
        }),
    )
    .unwrap();
    assert_eq!(receipt.status, CommandStatus::Rejected);
    assert!(receipt.error.unwrap().contains("another input"));
    assert!(lock(&child.inner).run.as_ref().unwrap().live());
    assert_eq!(shutdowns.load(Ordering::SeqCst), 0);
    assert_eq!(lock(&commands).len(), 2);
}

#[test]
fn delegated_send_cannot_claim_a_child_used_before_its_current_run() {
    let host = GuardHost::new();
    let parent = host.parent();
    let child = host.chat(Some(&parent), Box::new(RecordingDriver::default()));
    run_command(
        &host.shared,
        &child,
        ChatCommand::Send {
            text: "earlier user input".into(),
        },
    )
    .unwrap();
    // No TurnStarted was emitted. Restarting the run resets its input counter,
    // but must retain the chat-level evidence that another input owned it.
    stop(&child);
    let replacement = RecordingDriver::default();
    let commands = replacement.commands.clone();
    {
        let mut inner = lock(&child.inner);
        inner.generations = 2;
        let mut run = Run::starting(2);
        run.driver = Some(Arc::new(Mutex::new(Box::new(replacement))));
        run.reader_done.store(true, Ordering::SeqCst);
        inner.run = Some(run);
        inner.set_state(ChatState::Idle);
    }
    let receipt = host.send(&parent, &child, "assignment-after-child-resume");
    assert_eq!(receipt.status, CommandStatus::Rejected);
    assert!(
        lock(&commands).is_empty(),
        "assignment claimed a previously used child"
    );
    let cleanup = run_identified_command(
        &host.shared,
        &child.info().id,
        "cleanup-after-child-resume",
        ChatCommand::Stop,
        false,
        Some(CommandGuard::DelegatedCancel {
            parent_chat_id: parent.info().id,
            send_operation_id: "assignment-after-child-resume".into(),
            turn_id: None,
            allow_unstarted: true,
        }),
    )
    .unwrap();
    assert_eq!(cleanup.status, CommandStatus::Rejected);
    assert!(lock(&child.inner).run.as_ref().unwrap().live());
}

#[test]
fn parent_stop_does_not_wait_for_delegated_child_provider_write() {
    struct BlockedSend {
        entered: mpsc::Sender<()>,
        released: Receiver<()>,
    }
    impl Driver for BlockedSend {
        fn command(&mut self, _: ChatCommand) -> Result<(), String> {
            self.entered.send(()).unwrap();
            self.released
                .recv_timeout(Duration::from_secs(5))
                .map_err(|_| "test write timed out".into())
        }
        fn provider_thread_id(&self) -> Option<String> {
            Some("blocked-child".into())
        }
        fn shutdown(&mut self) {}
    }
    let host = Arc::new(GuardHost::new());
    let parent = host.parent();
    let (entered, entry) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let child = host.chat(Some(&parent), Box::new(BlockedSend { entered, released }));
    let sending = {
        let (host, parent, child) = (host.clone(), parent.clone(), child.clone());
        thread::spawn(move || host.send(&parent, &child, "blocked-assignment"))
    };
    entry.recv_timeout(Duration::from_secs(2)).unwrap();
    let (stopped, stopped_rx) = mpsc::channel();
    let stopping = {
        let parent = parent.clone();
        thread::spawn(move || {
            stop(&parent);
            let _ = stopped.send(());
        })
    };
    let stopped_before_child_release = stopped_rx.recv_timeout(Duration::from_secs(1)).is_ok();
    // Always release the fixture before asserting, including the failure case.
    release.send(()).unwrap();
    let receipt = sending.join().unwrap();
    stopping.join().unwrap();
    assert!(
        stopped_before_child_release,
        "parent Stop waited on a blocked child write"
    );
    assert_eq!(parent.info().state, ChatState::Stopped);
    assert_eq!(receipt.status, CommandStatus::Accepted);
    assert!(lock(&child.inner).run.as_ref().unwrap().live());
}
