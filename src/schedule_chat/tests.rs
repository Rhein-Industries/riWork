//! Scheduled delivery into orchestrators that run as chats, against a chat host
//! with fake drivers (and, for the exchanges a host does not normally break,
//! a stand-in that speaks the wire protocol by hand), on throwaway homes.
use super::*;
use crate::chat::client::socket_path;
use crate::chat::model::{
    ApprovalMode, ChatEvent, ChatInfo, Item, ItemStatus, OrchestratorScope, Provider,
};
use crate::chat::testing::*;
use crate::chat::wire::{Request, Response};
use crate::orchestrators::{self, Orchestrator};
use crate::schedule_service::{CreateRequest, ScheduleService, ScopeInput};
use crate::schedules::{ChatDelivery, Outcome, Schedule, ScheduleStore, Scope, Timing};
use crate::sessions::{HarnessKind, SessionManager};
use crate::settings::OrchestratorRuns;
use crate::store::Store;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

const PROMPT: &str = "Check the open tasks and report";
const DUE: u64 = 100;

fn in_process(home: &Path) -> Result<PathBuf, String> {
    Ok(socket_path(home))
}

fn unreachable_host(_: &Path) -> Result<PathBuf, String> {
    Err("no socket".into())
}

fn delivery(ensure: crate::schedules::ChatEnsure) -> ChatDelivery {
    ChatDelivery {
        ensure,
        proof: Proof {
            wait: Duration::from_secs(5),
            poll: Duration::from_millis(5),
        },
    }
}

/// A host with an orchestrator chat, and a schedule store that reaches it.
struct Rig {
    host: TestHost,
    sessions: SessionManager,
    store: ScheduleStore,
    chat: ChatInfo,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.sessions.kill_server();
    }
}

impl Rig {
    fn new() -> Self {
        let host = TestHost::new();
        let sessions = SessionManager::at(host.home.clone()).unwrap();
        let chat_host = orchestrators::ChatHost {
            home: &host.home,
            ensure: &in_process,
        };
        let (Orchestrator::Chat(chat), _) = orchestrators::create(
            &sessions,
            &chat_host,
            &OrchestratorScope::Global,
            None,
            host.home.clone(),
            None,
            OrchestratorRuns::Chat(Provider::Codex),
        )
        .unwrap() else {
            panic!("a chat was asked for");
        };
        let chat = host.wait_for_state(&chat.id, |state| *state == ChatState::Idle);
        let store = ScheduleStore::at(host.home.clone())
            .unwrap()
            .with_chat(delivery(in_process));
        Self {
            host,
            sessions,
            store,
            chat,
        }
    }

    fn target(&self) -> Target {
        let state = Store::open(self.host.home.clone())
            .unwrap()
            .snapshot()
            .unwrap();
        Target::bind_chat(Scope::App, &state, &self.chat).unwrap()
    }

    fn schedule(&self, timing: Timing) -> Schedule {
        self.store
            .save(
                None,
                "Orchestrator check".into(),
                PROMPT.into(),
                self.target(),
                timing,
                DUE - 1,
            )
            .unwrap()
    }

    fn row(&self) -> Schedule {
        self.store.list().unwrap().remove(0)
    }

    /// The prompts the driver was sent, and the startup message before them.
    fn sent(&self) -> Vec<String> {
        fake_for(&self.chat.cwd)
            .commands()
            .into_iter()
            .filter_map(|command| match command {
                ChatCommand::Send { text } => Some(text),
                _ => None,
            })
            .collect()
    }

    fn prompts(&self) -> usize {
        self.sent().iter().filter(|text| *text == PROMPT).count()
    }

    fn emit(&self, state: ChatState) {
        fake_for(&self.chat.cwd).emit(ChatEvent::State {
            state: state.clone(),
        });
        self.host.wait_for_state(&self.chat.id, |now| *now == state);
    }

    fn outcome(&self) -> Outcome {
        self.row().last_run.expect("an attempt was made").outcome
    }
}

#[test]
fn an_idle_orchestrator_chat_gets_the_prompt_once_and_its_log_shows_it() {
    let rig = Rig::new();
    rig.schedule(Timing::Once { at: DUE });
    rig.store.tick(DUE).unwrap();
    let row = rig.row();
    let run = row.last_run.as_ref().unwrap();
    assert_eq!(run.outcome, Outcome::Submitted, "{}", run.message);
    assert_eq!(rig.prompts(), 1);
    // The proof is a user message in the chat's log, after the startup turn.
    let log = rig.host.log(&rig.chat.id);
    let delivered: Vec<_> = log
        .iter()
        .filter(|envelope| is_user_message(&envelope.event, PROMPT))
        .collect();
    assert_eq!(delivered.len(), 1);
    // A one-time schedule has nothing more to run, and asking again changes nothing.
    assert_eq!(row.next_run, None);
    rig.store.tick(DUE + 1).unwrap();
    rig.store.tick(DUE + 60).unwrap();
    assert_eq!(rig.prompts(), 1);
    assert_eq!(rig.row().revision, row.revision);
}

#[test]
fn a_chat_at_work_defers_without_sending_and_is_asked_again() {
    let rig = Rig::new();
    rig.schedule(Timing::Once { at: DUE });
    // A turn under way.
    rig.host
        .client()
        .command(
            &rig.chat.id,
            ChatCommand::Send {
                text: "hang".into(),
            },
        )
        .unwrap();
    rig.host
        .wait_for_state(&rig.chat.id, |state| *state == ChatState::Running);
    rig.store.tick(DUE).unwrap();
    assert_eq!(rig.outcome(), Outcome::Deferred);
    assert!(
        rig.row()
            .last_run
            .unwrap()
            .message
            .contains("working on a turn")
    );
    // Not again before the check interval, and the state waits for the user.
    rig.emit(ChatState::Waiting);
    rig.store.tick(DUE + 5).unwrap();
    assert!(
        rig.row()
            .last_run
            .unwrap()
            .message
            .contains("working on a turn")
    );
    rig.store.tick(DUE + 20).unwrap();
    assert!(
        rig.row()
            .last_run
            .unwrap()
            .message
            .contains("approval or an answer")
    );
    assert_eq!(rig.prompts(), 0);
    assert!(!rig.row().paused);
    assert_eq!(rig.row().next_run, Some(DUE));

    // Idle at last: delivered, once.
    rig.emit(ChatState::Idle);
    rig.store.tick(DUE + 40).unwrap();
    assert_eq!(rig.outcome(), Outcome::Submitted);
    assert_eq!(rig.prompts(), 1);
    rig.store.tick(DUE + 60).unwrap();
    assert_eq!(rig.prompts(), 1);
}

#[test]
fn a_stopped_chat_is_resumed_by_the_delivery_and_a_failed_one_is_not_written_to() {
    let rig = Rig::new();
    rig.schedule(Timing::Once { at: DUE });
    // An agent that would not start leaves the chat failed.
    rig.host.client().close(&rig.chat.id).unwrap();
    *fake_for(&rig.chat.cwd).fail_start.lock().unwrap() = Some("codex is not installed".into());
    rig.host
        .client()
        .command(
            &rig.chat.id,
            ChatCommand::Send {
                text: "anyone?".into(),
            },
        )
        .unwrap_err();
    rig.host.wait_for_state(&rig.chat.id, |state| {
        matches!(state, ChatState::Failed { .. })
    });
    rig.store.tick(DUE).unwrap();
    assert_eq!(rig.outcome(), Outcome::Deferred);
    let message = rig.row().last_run.unwrap().message;
    assert!(
        message.contains("failed") && message.contains("codex is not installed"),
        "{message}"
    );
    assert_eq!(rig.prompts(), 0);

    // Stopped is not failed: the message starts the agent again.
    rig.host.client().close(&rig.chat.id).unwrap();
    assert_eq!(rig.host.info(&rig.chat.id).state, ChatState::Stopped);
    let starts = fake_for(&rig.chat.cwd).start_count();
    rig.store.tick(DUE + 20).unwrap();
    assert_eq!(rig.outcome(), Outcome::Submitted);
    assert_eq!(rig.prompts(), 1);
    assert_eq!(fake_for(&rig.chat.cwd).start_count(), starts + 1);
}

#[test]
fn a_recurring_schedule_delivers_each_occurrence_once_however_often_it_is_asked() {
    let rig = Rig::new();
    rig.schedule(Timing::Interval {
        first: DUE,
        seconds: 300,
    });
    for now in DUE..DUE + 40 {
        rig.store.tick(now).unwrap();
    }
    assert_eq!(rig.prompts(), 1);
    rig.host
        .wait_for_state(&rig.chat.id, |state| *state == ChatState::Idle);
    for now in DUE + 40..DUE + 300 {
        rig.store.tick(now).unwrap();
    }
    assert_eq!(rig.prompts(), 1);
    rig.store.tick(DUE + 300).unwrap();
    assert_eq!(rig.prompts(), 2);
    for now in DUE + 301..DUE + 340 {
        rig.store.tick(now).unwrap();
    }
    assert_eq!(rig.prompts(), 2);
    assert_eq!(rig.row().next_run, Some(DUE + 600));
}

#[test]
fn schedulers_that_race_for_one_occurrence_deliver_it_once() {
    let rig = Rig::new();
    rig.schedule(Timing::Once { at: DUE });
    std::thread::scope(|scope| {
        for _ in 0..6 {
            scope.spawn(|| rig.store.clone().tick(DUE).unwrap());
        }
    });
    assert_eq!(rig.prompts(), 1);
    assert_eq!(rig.outcome(), Outcome::Submitted);
}

#[test]
fn a_host_that_refuses_the_message_fails_the_schedule_after_the_claim_with_nothing_sent() {
    let rig = Rig::new();
    rig.schedule(Timing::Interval {
        first: DUE,
        seconds: 300,
    });
    rig.host.client().close(&rig.chat.id).unwrap();
    *fake_for(&rig.chat.cwd).fail_start.lock().unwrap() = Some("codex is not installed".into());
    rig.store.tick(DUE).unwrap();
    let row = rig.row();
    let run = row.last_run.as_ref().unwrap();
    assert_eq!(run.outcome, Outcome::Failed);
    assert!(
        run.message.contains("refused") && run.message.contains("nothing was delivered"),
        "{}",
        run.message
    );
    assert!(row.paused && row.review_required);
    assert_eq!(rig.prompts(), 0);
    // It is claimed and paused: nothing more is tried, not even once the
    // provider works again.
    *fake_for(&rig.chat.cwd).fail_start.lock().unwrap() = None;
    for now in DUE + 1..DUE + 400 {
        rig.store.tick(now).unwrap();
    }
    assert_eq!(rig.prompts(), 0);
    assert_eq!(rig.row().revision, row.revision);
}

/// A chat host's stand-in on `home`'s socket: it answers `List` with `chat`,
/// and what it does with a `Send` is what the test wants to see happen.
struct StandIn {
    thread: Option<std::thread::JoinHandle<()>>,
}

enum OnSend {
    /// Closes the connection without an answer.
    Drop,
    /// Says yes and writes nothing to the log.
    AcknowledgeOnly,
}

impl StandIn {
    fn start(home: &Path, chat: &ChatInfo, on_send: OnSend) -> Self {
        std::fs::create_dir_all(home.join("run")).unwrap();
        let listener = UnixListener::bind(socket_path(home)).unwrap();
        let chat = chat.clone();
        let thread = std::thread::spawn(move || {
            // The connection for `deliver`: a list, then the message.
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            for _ in 0..2 {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 {
                    return;
                }
                let request: Request = serde_json::from_str(&line).unwrap();
                let (id, result) = match request {
                    Request::List { id } => (id, Some(serde_json::to_value(vec![&chat]).unwrap())),
                    Request::Command { id, .. } => match on_send {
                        OnSend::Drop => return,
                        OnSend::AcknowledgeOnly => (id, None),
                    },
                    other => panic!("unexpected {other:?}"),
                };
                let response = Response {
                    id,
                    ok: true,
                    result,
                    error: None,
                };
                writeln!(writer, "{}", serde_json::to_string(&response).unwrap()).unwrap();
            }
        });
        Self {
            thread: Some(thread),
        }
    }
}

impl Drop for StandIn {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A rig on a home whose host is only a stand-in.
fn standin_rig(on_send: OnSend, wait: Duration) -> (PathBuf, ScheduleStore, ChatInfo, StandIn) {
    let home = short_home();
    let chat = ChatInfo {
        parent_id: None,
        user_title: None,
        first_user_message: None,

        id: Uuid::new_v4().to_string(),
        provider: Provider::Codex,
        project_id: None,
        worktree_id: None,
        cwd: home.clone(),
        title: "G·ORCH · GLOBAL".into(),
        created_at_unix: 7,
        provider_thread_id: None,
        model: None,
        effort: None,
        approval_mode: ApprovalMode::Supervised,
        codex_account_id: None,
        state: ChatState::Idle,
        orchestrator: Some(OrchestratorScope::Global),
        fast: false,
    };
    chat::log::ChatLog::create(&chat::log::chat_dir(&home, &chat.id).unwrap(), &chat).unwrap();
    let stand_in = StandIn::start(&home, &chat, on_send);
    let store = ScheduleStore::at(home.clone())
        .unwrap()
        .with_chat(ChatDelivery {
            ensure: in_process,
            proof: Proof {
                wait,
                poll: Duration::from_millis(5),
            },
        });
    (home, store, chat, stand_in)
}

fn bound(home: &Path, store: &ScheduleStore, chat: &ChatInfo) -> Schedule {
    let state = Store::open(home.to_path_buf()).unwrap().snapshot().unwrap();
    store
        .save(
            None,
            "Orchestrator check".into(),
            PROMPT.into(),
            Target::bind_chat(Scope::App, &state, chat).unwrap(),
            Timing::Interval {
                first: DUE,
                seconds: 300,
            },
            DUE - 1,
        )
        .unwrap()
}

#[test]
fn an_exchange_that_breaks_after_the_claim_is_uncertain_and_never_retried() {
    let (home, store, chat, stand_in) = standin_rig(OnSend::Drop, Duration::from_secs(1));
    bound(&home, &store, &chat);
    store.tick(DUE).unwrap();
    let row = store.list().unwrap().remove(0);
    let run = row.last_run.as_ref().unwrap();
    assert_eq!(run.outcome, Outcome::Uncertain);
    assert!(
        run.message.contains("no automatic retry"),
        "{}",
        run.message
    );
    assert!(row.paused && row.review_required);
    drop(stand_in);
    // No host is left to hear a second attempt, and none is made.
    for now in DUE + 1..DUE + 30 {
        store.tick(now).unwrap();
    }
    assert_eq!(store.list().unwrap()[0].revision, row.revision);
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn an_acknowledged_message_the_log_never_shows_is_uncertain() {
    let (home, store, chat, stand_in) =
        standin_rig(OnSend::AcknowledgeOnly, Duration::from_millis(80));
    bound(&home, &store, &chat);
    let started = Instant::now();
    store.tick(DUE).unwrap();
    assert!(started.elapsed() >= Duration::from_millis(80));
    let row = store.list().unwrap().remove(0);
    let run = row.last_run.as_ref().unwrap();
    assert_eq!(run.outcome, Outcome::Uncertain);
    assert!(run.message.contains("did not show it"), "{}", run.message);
    assert!(row.paused);
    drop(stand_in);
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn only_a_message_with_the_prompts_own_text_after_the_mark_is_the_proof() {
    let host = TestHost::new();
    let rig_chat = host.create(Provider::Codex);
    host.wait_for_state(&rig_chat.id, |state| *state == ChatState::Idle);
    let chat_host = orchestrators::ChatHost {
        home: &host.home,
        ensure: &in_process,
    };
    // A message that is not there is waited for briefly; one that is there is waited
    // for as long as a loaded machine needs.
    let impatient = Proof {
        wait: Duration::from_millis(60),
        poll: Duration::from_millis(5),
    };
    let patient = Proof {
        wait: Duration::from_secs(10),
        poll: Duration::from_millis(5),
    };
    // The prompt was said before the mark: it proves nothing.
    host.client()
        .command(
            &rig_chat.id,
            ChatCommand::Send {
                text: PROMPT.into(),
            },
        )
        .unwrap();
    host.wait_for_log(&rig_chat.id, |log| {
        log.iter()
            .any(|envelope| is_user_message(&envelope.event, PROMPT))
    });
    host.wait_for_state(&rig_chat.id, |state| *state == ChatState::Idle);
    let mark = chat::log::mark(&host.home, &rig_chat.id).unwrap();
    assert!(!seen_in_log(
        &chat_host,
        &rig_chat.id,
        mark,
        PROMPT,
        impatient
    ));
    // Other words are not it.
    host.client()
        .command(
            &rig_chat.id,
            ChatCommand::Send {
                text: "something else".into(),
            },
        )
        .unwrap();
    assert!(seen_in_log(
        &chat_host,
        &rig_chat.id,
        mark,
        "something else",
        patient
    ));
    assert!(!seen_in_log(
        &chat_host,
        &rig_chat.id,
        mark,
        PROMPT,
        impatient
    ));
    // The right words after the mark are.
    host.client()
        .command(
            &rig_chat.id,
            ChatCommand::Send {
                text: PROMPT.into(),
            },
        )
        .unwrap();
    assert!(seen_in_log(&chat_host, &rig_chat.id, mark, PROMPT, patient));
    // An item that only quotes the prompt is not a user message.
    let quoted = ChatEvent::ItemCompleted {
        item: Item {
            presentation: Default::default(),
            id: "a".into(),
            turn_id: None,
            status: ItemStatus::Completed,
            body: ItemBody::AgentMessage {
                text: PROMPT.into(),
            },
        },
    };
    assert!(!is_user_message(&quoted, PROMPT));
}

#[test]
fn a_target_that_is_not_the_pinned_chat_fails_instead_of_retargeting() {
    // The chat is gone.
    let rig = Rig::new();
    rig.schedule(Timing::Once { at: DUE });
    rig.host.client().delete(&rig.chat.id).unwrap();
    rig.store.tick(DUE).unwrap();
    let row = rig.row();
    assert_eq!(row.last_run.as_ref().unwrap().outcome, Outcome::Failed);
    assert!(
        row.last_run
            .as_ref()
            .unwrap()
            .message
            .contains("no longer exists")
    );
    assert!(row.paused);

    // Another chat of the same scope is another chat.
    let rig = Rig::new();
    rig.schedule(Timing::Once { at: DUE });
    let chat_host = orchestrators::ChatHost {
        home: &rig.host.home,
        ensure: &in_process,
    };
    orchestrators::close(&chat_host, &rig.chat).unwrap();
    let (Orchestrator::Chat(again), true) = orchestrators::create(
        &rig.sessions,
        &chat_host,
        &OrchestratorScope::Global,
        None,
        rig.host.home.clone(),
        None,
        OrchestratorRuns::Chat(Provider::Codex),
    )
    .unwrap() else {
        panic!("a new chat");
    };
    assert_ne!(again.id, rig.chat.id);
    rig.store.tick(DUE).unwrap();
    assert_eq!(rig.outcome(), Outcome::Failed);
    assert!(rig.sent().iter().all(|text| text != PROMPT));

    // A chat of the id but under another account is not it either.
    let rig = Rig::new();
    let mut target = rig.target();
    target.chat.as_mut().unwrap().codex_account_id = Some("another-account".into());
    rig.store
        .save(
            None,
            "Orchestrator check".into(),
            PROMPT.into(),
            target,
            Timing::Once { at: DUE },
            DUE - 1,
        )
        .unwrap();
    rig.store.tick(DUE).unwrap();
    assert_eq!(rig.outcome(), Outcome::Failed);
    assert!(
        rig.row()
            .last_run
            .unwrap()
            .message
            .contains("identity changed")
    );
    assert_eq!(rig.prompts(), 0);
}

#[test]
fn a_host_that_cannot_be_reached_defers_and_the_schedule_goes_on() {
    let rig = Rig::new();
    let store = rig.store.clone().with_chat(delivery(unreachable_host));
    store
        .save(
            None,
            "Orchestrator check".into(),
            PROMPT.into(),
            rig.target(),
            Timing::Once { at: DUE },
            DUE - 1,
        )
        .unwrap();
    store.tick(DUE).unwrap();
    let row = store.list().unwrap().remove(0);
    assert_eq!(row.last_run.as_ref().unwrap().outcome, Outcome::Deferred);
    assert!(
        row.last_run
            .as_ref()
            .unwrap()
            .message
            .contains("chat host is unavailable")
    );
    assert!(!row.paused);
    // The host is back.
    rig.store.tick(DUE + 20).unwrap();
    assert_eq!(rig.outcome(), Outcome::Submitted);
    assert_eq!(rig.prompts(), 1);
}

#[test]
fn a_chat_orchestrator_is_a_target_of_its_own_scope_only() {
    let rig = Rig::new();
    let store = Store::open(rig.host.home.clone()).unwrap();
    let project = store.add_project(rig.host.work(), Some("demo")).unwrap();
    let state = store.snapshot().unwrap();
    assert!(Scope::App.matches_chat(&state, &rig.chat));
    let scope = Scope::Project {
        project_id: project.id.clone(),
    };
    assert!(!scope.matches_chat(&state, &rig.chat));
    assert!(
        Target::bind_chat(scope, &state, &rig.chat)
            .unwrap_err()
            .contains("in this scope")
    );
    let other = Scope::Project {
        project_id: Uuid::new_v4().to_string(),
    };
    let mut project_chat = rig.chat.clone();
    project_chat.orchestrator = Some(OrchestratorScope::Project {
        project_id: project.id.clone(),
    });
    project_chat.project_id = Some(project.id.clone());
    assert!(
        Scope::Project {
            project_id: project.id
        }
        .matches_chat(&state, &project_chat)
    );
    assert!(!other.matches_chat(&state, &project_chat));
    assert!(
        !Scope::Workspace {
            project_id: String::new(),
            worktree_id: String::new()
        }
        .matches_chat(&state, &rig.chat)
    );
    // The pin is a chat's, and the terminal checks leave it alone.
    let target = rig.target();
    assert_eq!(target.harness, HarnessKind::Codex);
    assert_eq!(target.shell_id, rig.chat.id);
    assert_eq!(target.created_at, rig.chat.created_at_unix);
    assert!(target.matches_chat(&state, &rig.chat));
    assert!(rig.sessions.orchestrator_get().unwrap().is_none());
}

#[test]
fn the_service_binds_a_chat_orchestrator_by_its_id_and_the_ledger_keeps_terminal_targets_as_they_were()
 {
    let rig = Rig::new();
    let service = ScheduleService::at(rig.host.home.clone())
        .unwrap()
        .with_chat(delivery(in_process));
    let schedule = service
        .create(CreateRequest {
            scope: ScopeInput {
                scope: "app".into(),
                project_id: None,
                worktree_id: None,
            },
            shell_id: rig.chat.id.clone(),
            title: "Morning check".into(),
            prompt: PROMPT.into(),
            at: "2999-10-05T09:00:00+02:00".into(),
            every_minutes: None,
        })
        .unwrap();
    assert_eq!(schedule.target.shell_id, rig.chat.id);
    assert!(schedule.target.chat.is_some());
    assert_eq!(schedule.target.harness, HarnessKind::Codex);
    let json = serde_json::to_value(&schedule.target).unwrap();
    assert_eq!(
        json["chat"],
        serde_json::json!({"codex_account_id": "account-a"})
    );
    // A terminal target has no such key, so older builds read the ledger as before.
    let mut terminal = schedule.target.clone();
    terminal.chat = None;
    assert!(
        serde_json::to_value(&terminal)
            .unwrap()
            .get("chat")
            .is_none()
    );
    let older: Target = serde_json::from_value(serde_json::to_value(&terminal).unwrap()).unwrap();
    assert_eq!(older, terminal);
    // An id that is neither a session nor a chat is refused as before.
    let error = service
        .create(CreateRequest {
            scope: ScopeInput {
                scope: "app".into(),
                project_id: None,
                worktree_id: None,
            },
            shell_id: Uuid::new_v4().to_string(),
            title: "Nothing".into(),
            prompt: PROMPT.into(),
            at: "2999-10-05T09:00:00+02:00".into(),
            every_minutes: None,
        })
        .unwrap_err();
    assert_eq!(error.code, "binding_failed");
    // The chat's scope is the app's, not a project's.
    let project = Store::open(rig.host.home.clone())
        .unwrap()
        .add_project(rig.host.work(), Some("demo"))
        .unwrap();
    let error = service
        .create(CreateRequest {
            scope: ScopeInput {
                scope: "project".into(),
                project_id: Some(project.id),
                worktree_id: None,
            },
            shell_id: rig.chat.id.clone(),
            title: "Wrong scope".into(),
            prompt: PROMPT.into(),
            at: "2999-10-05T09:00:00+02:00".into(),
            every_minutes: None,
        })
        .unwrap_err();
    assert_eq!(error.code, "binding_failed");
}
