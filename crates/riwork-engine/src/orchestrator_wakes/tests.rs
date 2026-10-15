//! Orchestrator wakes against a chat host with fake drivers and a throwaway
//! `RIWORK_HOME`. A terminal orchestrator's pane is played by `PaneStandIn`,
//! which goes through the same claim as tmux delivery does.
use super::*;
use crate::{
    chat::{model::Provider, testing::*},
    sessions::ShellKind,
    settings::OrchestratorRuns,
};
use std::sync::{Arc, Barrier};

const T: u64 = 1_000_000;
const PROOF: Proof = Proof {
    wait: std::time::Duration::from_secs(5),
    poll: std::time::Duration::from_millis(5),
};
const CHECK_IN: &str = "RiWork check-in:";

fn in_process(home: &Path) -> Result<PathBuf, String> {
    Ok(socket_path(home))
}

/// A home with one project whose wakes are on, a worktree in it, and a worker:
/// a Claude shell that an orchestrator opened there.
struct Fixture {
    host: TestHost,
    root: PathBuf,
    project_id: String,
    worktree_id: String,
    worker: ShellSession,
}

impl Fixture {
    fn new() -> Self {
        let host = TestHost::new();
        let root = host.home.join("project");
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let store = Store::open(host.home.clone()).unwrap();
        let project = store.add_project(&root, Some("Wake project")).unwrap();
        store
            .set_project_orchestrator_wakes(&project.id, true)
            .unwrap();
        let worktree_id = Uuid::new_v4().to_string();
        let path = host.home.join("state.json");
        let mut state: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        state["worktrees"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "id": worktree_id, "project_id": project.id, "branch": "feature/wake",
                "path": root, "created_at": 1
            }));
        fs::write(&path, state.to_string()).unwrap();
        let worker = shell(
            &project.id,
            ShellKind::Project,
            Some(HarnessKind::Claude),
            false,
            Some(&Uuid::new_v4().to_string()),
        );
        let worker = ShellSession {
            worktree_id: Some(worktree_id.clone()),
            ..worker
        };
        let fixture = Self {
            host,
            root,
            project_id: project.id,
            worktree_id,
            worker,
        };
        fixture.register(&[]);
        fixture
    }

    fn home(&self) -> &Path {
        &self.host.home
    }

    /// The worker and `others` are the registered shells.
    fn register(&self, others: &[ShellSession]) {
        let mut shells = vec![self.worker.clone()];
        shells.extend_from_slice(others);
        fs::write(
            self.home().join("sessions.json"),
            serde_json::json!({ "sessions": shells }).to_string(),
        )
        .unwrap();
    }

    fn wakes(&self, enabled: bool) {
        Store::open(self.home().to_path_buf())
            .unwrap()
            .set_project_orchestrator_wakes(&self.project_id, enabled)
            .unwrap();
    }

    fn record(&self, event: &str, time: u64) -> usize {
        record_at(
            self.home(),
            &self.worker.id,
            event,
            HarnessKind::Claude,
            time,
        )
        .unwrap()
    }

    fn chat_host(&self) -> ChatHost<'_> {
        ChatHost {
            home: self.home(),
            ensure: &in_process,
        }
    }

    fn chat_orchestrator(&self, project: bool) -> ChatInfo {
        let sessions = SessionManager::at(self.home().to_path_buf()).unwrap();
        let scope = orchestrators::scope_of(project.then_some(self.project_id.as_str()));
        let root = project.then(|| self.root.clone());
        let created = orchestrators::create(
            &sessions,
            &self.chat_host(),
            &scope,
            root.clone(),
            root.unwrap_or_else(|| self.home().to_path_buf()),
            None,
            &OrchestratorRuns::chat(Provider::Codex),
        )
        .map(|made| (made.orchestrator, made.created))
        .unwrap();
        let (Orchestrator::Chat(chat), true) = created else {
            panic!("expected a new chat orchestrator, got {created:?}");
        };
        self.host
            .wait_for_state(&chat.id, |state| *state == ChatState::Idle)
    }

    fn tick(&self, terminals: &mut dyn Terminals, time: u64) -> Result<(), String> {
        deliver_at(self.home(), &self.chat_host(), PROOF, terminals, time)
    }

    fn tick_chats(&self, time: u64) {
        self.tick(&mut NoTerminals, time).unwrap();
    }

    fn expected(&self) -> String {
        format!(
            "RiWork check-in: worker shell {shell} (claude) finished a turn in project Wake project ({project}), \
             worktree feature/wake ({worktree}). Read its output with `riwork shell output {shell} --lines 120` \
             and its task state with `riwork worktree tasks {worktree}`, then decide its next step.",
            shell = self.worker.id,
            project = self.project_id,
            worktree = self.worktree_id,
        )
    }
}

fn shell(
    project_id: &str,
    kind: ShellKind,
    harness: Option<HarnessKind>,
    user_opened: bool,
    parent_id: Option<&str>,
) -> ShellSession {
    ShellSession {
        user_opened,
        parent_id: parent_id.map(str::to_owned),
        id: Uuid::new_v4().to_string(),
        project_id: Some(project_id.to_owned()),
        worktree_id: None,
        kind,
        cwd: PathBuf::from("/"),
        command: None,
        editor_path: None,
        harness,
        unrestricted: false,
        codex_account_id: None,
        codex_account_label: None,
        codex_account_email: None,
        codex_home: None,
        orchestrator_skill_loaded: false,
        orchestrator_skill_version: None,
        orchestrator_delegation_version: None,
        orchestrator_project_root: None,
        created_at_unix: 1,
        alive: false,
    }
}

/// The check-ins a chat orchestrator's agent was sent.
fn check_ins(chat: &ChatInfo) -> Vec<String> {
    fake_for(&chat.cwd)
        .commands()
        .into_iter()
        .filter_map(|command| match command {
            ChatCommand::Send { text } if text.starts_with(CHECK_IN) => Some(text),
            _ => None,
        })
        .collect()
}

fn scoped(home: &Path, scope: WakeScope) -> Vec<WakeState> {
    states(home)
        .into_iter()
        .filter(|(wake, ..)| *wake == scope)
        .map(|(_, state, _)| state)
        .collect()
}

/// For chat-only homes: there is no terminal orchestrator to reach.
struct NoTerminals;

impl Terminals for NoTerminals {
    fn live(&mut self, _: &ShellSession) -> bool {
        false
    }
    fn idle_token(&mut self, _: &ShellSession) -> Option<String> {
        None
    }
    fn submit(
        &mut self,
        _: &ShellSession,
        _: &str,
        _: &str,
        _: &mut dyn FnMut() -> Result<bool, String>,
    ) -> Submission {
        panic!("no terminal orchestrator here")
    }
}

/// A terminal orchestrator's pane: idle while `token` is set, and what was
/// typed into it once the claim agreed. `before_claim` runs where a toggle in
/// another window could race the delivery.
#[derive(Default)]
struct PaneStandIn {
    token: Option<String>,
    typed: Vec<(String, String)>,
    before_claim: Option<Box<dyn FnMut()>>,
}

impl Terminals for PaneStandIn {
    fn live(&mut self, _: &ShellSession) -> bool {
        true
    }
    fn idle_token(&mut self, _: &ShellSession) -> Option<String> {
        self.token.clone()
    }
    fn submit(
        &mut self,
        shell: &ShellSession,
        text: &str,
        token: &str,
        claim: &mut dyn FnMut() -> Result<bool, String>,
    ) -> Submission {
        if self.token.as_deref() != Some(token) {
            return Submission::NotSent;
        }
        if let Some(before) = &mut self.before_claim {
            before();
        }
        if !claim().unwrap() {
            return Submission::NotSent;
        }
        self.typed.push((shell.id.clone(), text.to_owned()));
        Submission::Sent
    }
}

#[test]
fn a_completion_queues_one_wake_per_scope_and_a_replay_queues_none() {
    let fixture = Fixture::new();
    let barrier = Arc::new(Barrier::new(4));
    let workers: Vec<_> = (0..4)
        .map(|_| {
            let (home, shell, barrier) = (
                fixture.home().to_path_buf(),
                fixture.worker.id.clone(),
                barrier.clone(),
            );
            std::thread::spawn(move || {
                barrier.wait();
                record_at(&home, &shell, "session:turn-1", HarnessKind::Claude, T).unwrap()
            })
        })
        .collect();
    let queued: usize = workers.into_iter().map(|w| w.join().unwrap()).sum();
    assert_eq!(queued, 2);
    assert_eq!(fixture.record("session:turn-1", T + 1), 0);
    assert_eq!(
        states(fixture.home()),
        vec![
            (WakeScope::Project, WakeState::Pending, None),
            (WakeScope::Global, WakeState::Pending, None),
        ]
    );
    assert_eq!(fixture.record("session:turn-2", T + 2), 2);
    // Identifiers only: no names, no message, no agent text.
    let saved = fs::read_to_string(fixture.home().join(LEDGER)).unwrap();
    assert!(!saved.contains("Wake project") && !saved.contains("feature/wake"));
    assert!(!saved.contains(CHECK_IN));
    assert!(
        record_at(
            fixture.home(),
            &fixture.worker.id,
            "body\ntext",
            HarnessKind::Claude,
            T
        )
        .is_err()
    );
}

#[test]
fn only_worker_harness_shells_wake_orchestrators() {
    let fixture = Fixture::new();
    let project = fixture.project_id.as_str();
    let orchestrator = shell(
        project,
        ShellKind::Orchestrator,
        Some(HarnessKind::Codex),
        false,
        None,
    );
    let plain = shell(project, ShellKind::Project, None, false, None);
    let user_opened = shell(
        project,
        ShellKind::Project,
        Some(HarnessKind::Claude),
        true,
        None,
    );
    let automation = shell(
        project,
        ShellKind::Project,
        Some(HarnessKind::Codex),
        false,
        None,
    );
    fixture.register(&[
        orchestrator.clone(),
        plain.clone(),
        user_opened.clone(),
        automation.clone(),
    ]);
    assert!(!orchestrator.is_worker() && !user_opened.is_worker());
    let record = |shell: &ShellSession, provider| {
        record_at(fixture.home(), &shell.id, "event", provider, T).unwrap()
    };
    assert_eq!(record(&orchestrator, HarnessKind::Codex), 0);
    assert_eq!(record(&plain, HarnessKind::Codex), 0);
    assert_eq!(record(&plain, HarnessKind::Claude), 0);
    assert_eq!(record(&user_opened, HarnessKind::Claude), 0);
    // The wrong provider for a worker is no completion of it.
    assert_eq!(record(&fixture.worker, HarnessKind::Codex), 0);
    assert!(!fixture.home().join(LEDGER).exists());
    // Parented, and opened by automation without a parent.
    assert_eq!(record(&fixture.worker, HarnessKind::Claude), 2);
    assert_eq!(record(&automation, HarnessKind::Codex), 2);
}

#[test]
fn disabled_projects_queue_nothing_and_turning_wakes_off_cancels_pending() {
    let fixture = Fixture::new();
    let chat = fixture.chat_orchestrator(true);
    fixture.wakes(false);
    assert_eq!(fixture.record("turn-a", T), 0);
    assert!(!fixture.home().join(LEDGER).exists());
    fixture.wakes(true);
    assert_eq!(fixture.record("turn-b", T), 2);
    fixture.wakes(false);
    cancel_pending(fixture.home(), &fixture.project_id).unwrap();
    fixture.wakes(true);
    fixture.tick_chats(T + 1);
    assert_eq!(
        scoped(fixture.home(), WakeScope::Project),
        vec![WakeState::Cancelled]
    );
    assert!(check_ins(&chat).is_empty());
    // Off without the cancel (another writer of the setting): the tick settles it.
    assert_eq!(fixture.record("turn-c", T + 2), 2);
    fixture.wakes(false);
    fixture.tick_chats(T + 3);
    assert_eq!(
        scoped(fixture.home(), WakeScope::Project),
        vec![WakeState::Cancelled, WakeState::Cancelled]
    );
    assert!(check_ins(&chat).is_empty());
}

#[test]
fn idle_chat_orchestrators_each_get_exactly_one_identifiers_only_line() {
    let fixture = Fixture::new();
    let project = fixture.chat_orchestrator(true);
    let global = fixture.chat_orchestrator(false);
    assert_eq!(fixture.record("session:turn-1", T), 2);
    fixture.tick_chats(T + 1);
    let expected = fixture.expected();
    assert!(!expected.contains('\n') && !expected.contains('\r'));
    assert_eq!(check_ins(&project), vec![expected.clone()]);
    assert_eq!(check_ins(&global), vec![expected.clone()]);
    for chat in [&project, &global] {
        let shown = fixture
            .host
            .log(&chat.id)
            .into_iter()
            .filter(|envelope| match &envelope.event {
                ChatEvent::ItemStarted { item } => {
                    matches!(&item.body, chat::model::ItemBody::UserMessage { text } if *text == expected)
                }
                _ => false,
            })
            .count();
        assert_eq!(shown, 1);
    }
    assert_eq!(
        states(fixture.home()),
        vec![
            (
                WakeScope::Project,
                WakeState::Delivered,
                Some(project.id.clone())
            ),
            (
                WakeScope::Global,
                WakeState::Delivered,
                Some(global.id.clone())
            ),
        ]
    );
    // A replayed completion and later ticks send nothing more.
    assert_eq!(fixture.record("session:turn-1", T + 2), 0);
    fixture.tick_chats(T + 3);
    assert_eq!(check_ins(&project).len(), 1);
    assert_eq!(check_ins(&global).len(), 1);
}

#[test]
fn a_busy_chat_keeps_its_wake_until_idle_and_lets_it_expire_otherwise() {
    let fixture = Fixture::new();
    let project = fixture.chat_orchestrator(true);
    let global = fixture.chat_orchestrator(false);
    let busy = |chat: &ChatInfo| {
        fixture
            .host
            .client()
            .command(
                &chat.id,
                ChatCommand::Send {
                    text: "hang".into(),
                },
            )
            .unwrap();
        fixture
            .host
            .wait_for_state(&chat.id, |state| *state == ChatState::Running);
    };
    busy(&project);
    assert_eq!(fixture.record("turn-1", T), 2);
    fixture.tick_chats(T + 1);
    assert!(
        check_ins(&project).is_empty(),
        "a running turn is never steered"
    );
    assert_eq!(check_ins(&global).len(), 1);
    assert_eq!(
        scoped(fixture.home(), WakeScope::Project),
        vec![WakeState::Pending]
    );
    // Idle within the wake's lifetime: it lands, once.
    fake_for(&project.cwd).emit(ChatEvent::State {
        state: ChatState::Idle,
    });
    fixture
        .host
        .wait_for_state(&project.id, |state| *state == ChatState::Idle);
    fixture.tick_chats(T + 60);
    fixture.tick_chats(T + 61);
    assert_eq!(check_ins(&project), vec![fixture.expected()]);
    // Busy for the whole lifetime: it expires and is never sent. The global
    // orchestrator finished the turn its last wake started, so it takes the next.
    busy(&project);
    assert_eq!(fixture.record("turn-2", T + 100), 2);
    fixture.tick_chats(T + 101);
    assert_eq!(check_ins(&global).len(), 2);
    fixture.tick_chats(T + 100 + WAKE_LIFETIME + 1);
    assert_eq!(
        scoped(fixture.home(), WakeScope::Project),
        vec![WakeState::Delivered, WakeState::Expired]
    );
    fake_for(&project.cwd).emit(ChatEvent::State {
        state: ChatState::Idle,
    });
    fixture
        .host
        .wait_for_state(&project.id, |state| *state == ChatState::Idle);
    fixture.tick_chats(T + 100 + WAKE_LIFETIME + 2);
    assert_eq!(check_ins(&project).len(), 1);
}

#[test]
fn a_chat_is_woken_again_only_after_it_finished_a_turn() {
    let fixture = Fixture::new();
    let chat = fixture.chat_orchestrator(true);
    let mark = chat::log::mark(fixture.home(), &chat.id).unwrap();
    let token = format!("log:{mark}");
    assert!(!chat_turn_since(fixture.home(), &chat.id, &token));
    fixture
        .host
        .client()
        .command(
            &chat.id,
            ChatCommand::Send {
                text: "hang".into(),
            },
        )
        .unwrap();
    fixture
        .host
        .wait_for_state(&chat.id, |state| *state == ChatState::Running);
    // A host that reports idle before the turn is over does not get a second wake.
    fake_for(&chat.cwd).emit(ChatEvent::State {
        state: ChatState::Idle,
    });
    fixture
        .host
        .wait_for_state(&chat.id, |state| *state == ChatState::Idle);
    assert!(!chat_turn_since(fixture.home(), &chat.id, &token));
    let completed = |log: &[chat::wire::Envelope]| {
        log.iter()
            .filter(|envelope| matches!(envelope.event, ChatEvent::TurnCompleted { .. }))
            .count()
    };
    let before = completed(&fixture.host.log(&chat.id));
    fake_for(&chat.cwd).emit(ChatEvent::TurnCompleted {
        turn_id: "turn".into(),
        outcome: chat::model::TurnOutcome::Completed,
    });
    fixture
        .host
        .wait_for_log(&chat.id, |log| completed(log) > before);
    assert!(chat_turn_since(fixture.home(), &chat.id, &token));
}

#[test]
fn a_missing_orchestrator_is_skipped_and_none_is_created() {
    let mut fixture = Fixture::new();
    assert_eq!(fixture.record("turn-1", T), 2);
    fixture.tick_chats(T + 1);
    assert_eq!(
        states(fixture.home()),
        vec![
            (WakeScope::Project, WakeState::Skipped, None),
            (WakeScope::Global, WakeState::Skipped, None),
        ]
    );
    assert!(orchestrators::chat_orchestrators(&fixture.chat_host()).is_empty());
    let sessions = SessionManager::at(fixture.home().to_path_buf()).unwrap();
    assert!(sessions.orchestrator_get().unwrap().is_none());
    assert!(
        sessions
            .orchestrator_get_for_project(&fixture.project_id)
            .unwrap()
            .is_none()
    );
    // With the chat host gone too, a later wake is skipped the same way.
    fixture.host.stop();
    assert_eq!(fixture.record("turn-2", T + 2), 2);
    fixture.tick_chats(T + 3);
    assert_eq!(
        scoped(fixture.home(), WakeScope::Global),
        vec![WakeState::Skipped, WakeState::Skipped]
    );
}

#[test]
fn a_stopped_chat_host_holds_the_wake_until_it_expires() {
    let mut fixture = Fixture::new();
    let chat = fixture.chat_orchestrator(true);
    fixture.host.stop();
    assert_eq!(fixture.record("turn-1", T), 2);
    fixture.tick_chats(T + 1);
    assert_eq!(
        scoped(fixture.home(), WakeScope::Project),
        vec![WakeState::Pending]
    );
    fixture.tick_chats(T + WAKE_LIFETIME + 1);
    assert_eq!(
        scoped(fixture.home(), WakeScope::Project),
        vec![WakeState::Expired]
    );
    assert!(check_ins(&chat).is_empty());
}

#[test]
fn a_terminal_orchestrator_is_typed_into_only_at_a_fresh_idle_prompt() {
    let fixture = Fixture::new();
    let orchestrator = shell(
        &fixture.project_id,
        ShellKind::Orchestrator,
        Some(HarnessKind::Codex),
        false,
        None,
    );
    fixture.register(&[orchestrator.clone()]);
    let mut pane = PaneStandIn::default();
    assert_eq!(fixture.record("turn-1", T), 2);
    fixture.tick(&mut pane, T + 1).unwrap();
    assert!(
        pane.typed.is_empty(),
        "a working terminal is never typed into"
    );
    assert_eq!(
        states(fixture.home()),
        vec![
            (WakeScope::Project, WakeState::Pending, None),
            (WakeScope::Global, WakeState::Skipped, None),
        ]
    );
    pane.token = Some("thread:turn-a".into());
    fixture.tick(&mut pane, T + 2).unwrap();
    fixture.tick(&mut pane, T + 3).unwrap();
    assert_eq!(
        pane.typed,
        vec![(orchestrator.id.clone(), fixture.expected())]
    );
    assert_eq!(
        scoped(fixture.home(), WakeScope::Project),
        vec![WakeState::Delivered]
    );
    // Still showing the turn before the last wake: it has not taken that one up yet.
    assert_eq!(fixture.record("turn-2", T + 4), 2);
    fixture.tick(&mut pane, T + 5).unwrap();
    assert_eq!(pane.typed.len(), 1);
    pane.token = Some("thread:turn-b".into());
    fixture.tick(&mut pane, T + 6).unwrap();
    assert_eq!(pane.typed.len(), 2);
    // Turned off while the pane was being checked: the claim refuses, nothing is typed.
    assert_eq!(fixture.record("turn-3", T + 7), 2);
    pane.token = Some("thread:turn-c".into());
    let (home, project) = (fixture.home().to_path_buf(), fixture.project_id.clone());
    pane.before_claim = Some(Box::new(move || {
        cancel_pending(&home, &project).unwrap();
    }));
    fixture.tick(&mut pane, T + 8).unwrap();
    assert_eq!(pane.typed.len(), 2);
    // Turned off without the cancel reaching the ledger first: the claim reads
    // the setting itself.
    assert_eq!(fixture.record("turn-4", T + 9), 2);
    pane.token = Some("thread:turn-d".into());
    let (home, project) = (fixture.home().to_path_buf(), fixture.project_id.clone());
    pane.before_claim = Some(Box::new(move || {
        Store::open(home.clone())
            .unwrap()
            .set_project_orchestrator_wakes(&project, false)
            .unwrap();
    }));
    fixture.tick(&mut pane, T + 10).unwrap();
    assert_eq!(pane.typed.len(), 2);
    assert_eq!(
        scoped(fixture.home(), WakeScope::Project),
        vec![
            WakeState::Delivered,
            WakeState::Delivered,
            WakeState::Cancelled,
            WakeState::Cancelled
        ]
    );
}

#[test]
fn a_damaged_entry_does_not_disable_the_others() {
    let fixture = Fixture::new();
    assert_eq!(fixture.record("turn-1", T), 2);
    let mut ledger: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.home().join(LEDGER)).unwrap()).unwrap();
    let entries = ledger["entries"].as_array_mut().unwrap();
    entries.push(serde_json::json!({"wake_id": "not-a-uuid"}));
    entries.push(serde_json::json!("not even an object"));
    fs::write(fixture.home().join(LEDGER), ledger.to_string()).unwrap();
    assert_eq!(states(fixture.home()).len(), 2);
    fs::write(fixture.home().join(LEDGER), "{ truncated").unwrap();
    assert!(read(fixture.home()).is_err());
}

/// Stops the tmux server a real-pane test started, even when it panics.
struct Server(SessionManager);

impl Drop for Server {
    fn drop(&mut self) {
        self.0.kill_server();
    }
}

/// Plays a Codex at its prompt: draws `›`, and appends each submitted line to
/// `received-SHELL` in the home, as the schedule tests' harness does.
const PLAY_CODEX: &str = r#"import os,sys,tty,select
from pathlib import Path
tty.setraw(0)
draw=lambda: os.write(1,b'\x1b[2J\x1b[H\xe2\x80\xba ')
draw()
buf=b''
received=Path(os.environ['RIWORK_HOME'])/('received-'+os.environ['RIWORK_SHELL_ID'])
while True:
 if not select.select([0],[],[],0.02)[0]: continue
 b=os.read(0,1)
 if b in (b'\r',b'\n'):
  with open(received,'ab') as f: f.write(buf+b'\n')
  buf=b''
  draw()
 else: buf+=b
"#;

/// A Codex rollout for the orchestrator in a new thread, at `turn`, done or
/// not, which the pane's process (pid 4242 to the fake `lsof`) holds open.
fn codex_turn(home: &Path, shell_id: &str, turn: &str, done: bool) {
    let thread = Uuid::new_v4().to_string();
    let log_home = home.join("codex");
    let logs = log_home.join("sessions/2026/09/27");
    fs::create_dir_all(&logs).unwrap();
    crate::activity::bind_codex_thread(home, shell_id, &thread, &log_home).unwrap();
    let mut records = format!(
        "{}\n{}\n",
        serde_json::json!({"type":"session_meta","payload":{"id":thread,"source":"cli"}}),
        serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":turn}})
    );
    if done {
        records += &format!(
            "{}\n",
            serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":turn}})
        );
    }
    let rollout = logs.join(format!("rollout-fixture-{thread}.jsonl"));
    fs::write(&rollout, records).unwrap();
    fs::write(
        home.join("lsof-4242"),
        format!(
            "p4242\nfcwd\nn/\nf3\nn/dev/null\nf9\nn{}\n",
            rollout.display()
        ),
    )
    .unwrap();
}

/// The schedule tests' fake `lsof`: prints the listing written for the PID.
const FAKE_LSOF: &str = "#!/bin/sh\ncat \"$(dirname \"$0\")/lsof-$4\"\n";

#[test]
#[ignore = "slow: real tmux pane played by a script"]
fn a_real_terminal_orchestrator_gets_one_line_at_its_idle_prompt() {
    let fixture = Fixture::new();
    let home = fixture.home().to_path_buf();
    let script = home.join("play-codex.py");
    fs::write(&script, PLAY_CODEX).unwrap();
    let sessions = SessionManager::at(home.clone()).unwrap();
    let _server = Server(sessions.clone());
    let orchestrator = sessions
        .orchestrator_create_for_project(
            fixture.project_id.clone(),
            fixture.root.clone(),
            Some(format!("/usr/bin/python3 '{}'", script.display())),
        )
        .unwrap();
    // RiWork's own terminal orchestrator is a Codex; the script stands in for
    // it, and the probe reports the pane's process as `codex` with its rollout
    // open, so the real identity proof runs.
    let registry = home.join("sessions.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&registry).unwrap()).unwrap();
    for shell in value["sessions"].as_array_mut().unwrap() {
        if shell["id"] == orchestrator.id.as_str() {
            shell["harness"] = "codex".into();
            shell["codex_home"] = home.join("codex").to_string_lossy().as_ref().into();
        }
    }
    fs::write(&registry, value.to_string()).unwrap();
    let lsof = home.join("fake-lsof");
    fs::write(&lsof, FAKE_LSOF).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&lsof, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let foreground = Arc::new(std::sync::Mutex::new(String::from("codex")));
    let _probe = crate::sessions::schedule_probe::install(
        &home,
        {
            let foreground = foreground.clone();
            Arc::new(move |_: &str| Ok(format!("4242|{}", foreground.lock().unwrap())))
        },
        lsof,
    );
    let received = home.join(format!("received-{}", orchestrator.id));
    let wait = |done: &dyn Fn() -> bool| {
        for _ in 0..250 {
            if done() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!(
            "timed out: {}",
            sessions.capture(&orchestrator.id, 5).unwrap()
        );
    };
    wait(&|| {
        sessions
            .capture(&orchestrator.id, 1)
            .is_ok_and(|screen| screen.lines().any(|line| line.trim() == "›"))
    });
    let lines = || fs::read_to_string(&received).unwrap_or_default();
    let mut runner = Runner::at(home.clone());
    // Working: the wake waits.
    codex_turn(&home, &orchestrator.id, "turn-1", false);
    assert_eq!(fixture.record("turn-a", now()), 2);
    runner.tick().unwrap();
    assert_eq!(lines(), "");
    assert_eq!(scoped(&home, WakeScope::Project), vec![WakeState::Pending]);
    // Idle, but Codex is not what holds the pane: the idle turn is not proven its own.
    codex_turn(&home, &orchestrator.id, "turn-1", true);
    *foreground.lock().unwrap() = "zsh".into();
    runner.tick().unwrap();
    assert_eq!(lines(), "");
    // Idle at its prompt: exactly one line, once.
    *foreground.lock().unwrap() = "codex".into();
    runner.tick().unwrap();
    runner.tick().unwrap();
    wait(&|| !lines().is_empty());
    assert_eq!(lines(), format!("{}\n", fixture.expected()));
    assert_eq!(
        states(&home),
        vec![
            (
                WakeScope::Project,
                WakeState::Delivered,
                Some(orchestrator.id.clone())
            ),
            (WakeScope::Global, WakeState::Skipped, None),
        ]
    );
    // The same completed turn is still showing: the next wake waits for a new one.
    assert_eq!(fixture.record("turn-b", now()), 2);
    runner.tick().unwrap();
    assert_eq!(lines().lines().count(), 1);
    codex_turn(&home, &orchestrator.id, "turn-2", true);
    runner.tick().unwrap();
    wait(&|| lines().lines().count() == 2);
    assert!(lines().lines().all(|line| line == fixture.expected()));
}
