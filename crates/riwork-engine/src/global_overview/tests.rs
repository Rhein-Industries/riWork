//! The dashboard's arrangement of fixed records: no tmux, no chat host, no files.
use super::*;
use crate::{
    activity::Subagents,
    store::{Project, ProjectFolder, Task, Worktree},
};
use serde_json::json;

const ALPHA: &str = "aaaaaaaa-0000-4000-8000-000000000001";
const BETA: &str = "bbbbbbbb-0000-4000-8000-000000000002";
const QUIET: &str = "cccccccc-0000-4000-8000-000000000003";
const NOW: u64 = 1_800_000_000;

fn project(id: &str, name: &str, folder: Option<&str>) -> Project {
    Project {
        id: id.into(),
        name: name.into(),
        root: PathBuf::from(format!("/work/{name}")),
        repository_roots: Vec::new(),
        folder_id: folder.map(str::to_owned),
        notify_on_agent_done: false,
        wake_orchestrators_on_agent_done: false,
        orchestrator: Default::default(),
        codex_account: Default::default(),
        created_at: 0,
    }
}

fn worktree(id: &str, project: &str, branch: &str, path: &str, primary: bool) -> Worktree {
    Worktree {
        id: id.into(),
        project_id: project.into(),
        branch: branch.into(),
        path: PathBuf::from(path),
        is_primary: primary,
        repository_root: None,
        created_at: 0,
    }
}

fn task(id: &str, project: &str, status: TaskStatus, worktree: Option<&str>, updated: u64) -> Task {
    Task {
        id: id.into(),
        project_id: project.into(),
        title: format!("Task {id}"),
        details: String::new(),
        status,
        worktree_id: worktree.map(str::to_owned),
        created_at: 0,
        updated_at: updated,
    }
}

fn state() -> State {
    State {
        projects: vec![
            project(ALPHA, "alpha", Some("f1")),
            project(BETA, "beta", None),
            project(QUIET, "quiet", None),
        ],
        project_folders: vec![ProjectFolder {
            id: "f1".into(),
            name: "Clients".into(),
            parent_id: None,
            created_at: 0,
        }],
        worktrees: vec![
            worktree("wa-main", ALPHA, "main", "/work/alpha", true),
            worktree("wa-feat", ALPHA, "feat/x", "/work/alpha-feat", false),
            worktree("wb-main", BETA, "main", "/work/beta", true),
        ],
        tasks: vec![
            task("t1", ALPHA, TaskStatus::InProgress, Some("wa-feat"), 20),
            task("t2", ALPHA, TaskStatus::Done, None, 10),
            task("t3", BETA, TaskStatus::Todo, None, 5),
            task("t4", BETA, TaskStatus::InProgress, None, 30),
            // A task of a project the store no longer lists counts nowhere.
            task("t5", "gone", TaskStatus::InProgress, None, 99),
        ],
        ..State::default()
    }
}

fn shell(
    id: &str,
    project: Option<&str>,
    kind: &str,
    harness: Option<&str>,
    alive: bool,
) -> ShellSession {
    let mut shell: ShellSession = serde_json::from_value(json!({
        "id": id,
        "project_id": project,
        "worktree_id": null,
        "kind": kind,
        "cwd": "/work",
        "command": null,
        "harness": harness,
        "created_at_unix": 0
    }))
    .unwrap();
    shell.alive = alive;
    shell
}

fn chat(
    id: &str,
    project: Option<&str>,
    state: serde_json::Value,
    orchestrator: Option<serde_json::Value>,
) -> ChatInfo {
    let mut value = json!({
        "id": id,
        "provider": "claude",
        "project_id": project,
        "cwd": "/work",
        "title": format!("Chat {id}"),
        "created_at_unix": 0,
        "state": state,
    });
    if let Some(scope) = orchestrator {
        value["orchestrator"] = scope;
    }
    serde_json::from_value(value).unwrap()
}

fn agent(activity: AgentActivity, since: Option<u64>, subagents: usize) -> AgentState {
    AgentState {
        activity,
        since_unix: since,
        subagents: Subagents {
            working: subagents,
            kinds: Vec::new(),
        },
    }
}

struct Fixture {
    state: State,
    shells: Vec<ShellSession>,
    cwds: BTreeMap<String, PathBuf>,
    activity: BTreeMap<String, AgentState>,
    chats: Option<Vec<ChatInfo>>,
    chat_activity: BTreeMap<String, u64>,
    schedules: Option<Result<Vec<Schedule>, String>>,
}

impl Fixture {
    fn overview(&self) -> Overview {
        build(&Inputs {
            state: &self.state,
            shells: &self.shells,
            cwds: &self.cwds,
            activity: &self.activity,
            chats: self.chats.as_deref(),
            chat_activity: &self.chat_activity,
            schedules: self.schedules.as_ref(),
            now: NOW,
        })
    }
}

/// Two busy projects and a quiet one, a global orchestrator chat, and a few things to do.
fn busy() -> Fixture {
    Fixture {
        state: state(),
        shells: vec![
            shell("s-alpha-codex", Some(ALPHA), "project", Some("codex"), true),
            shell(
                "s-alpha-claude",
                Some(ALPHA),
                "project",
                Some("claude"),
                true,
            ),
            shell("s-alpha-plain", Some(ALPHA), "project", None, true),
            shell("s-beta-codex", Some(BETA), "project", Some("codex"), true),
            shell(
                "s-beta-orch",
                Some(BETA),
                "orchestrator",
                Some("codex"),
                true,
            ),
            // Ended sessions are history: not live, not working, not waiting.
            shell("s-beta-old", Some(BETA), "project", Some("codex"), false),
        ],
        cwds: BTreeMap::from([(
            "s-alpha-codex".to_owned(),
            PathBuf::from("/work/alpha-feat/src"),
        )]),
        activity: BTreeMap::from([
            (
                "s-alpha-codex".to_owned(),
                agent(AgentActivity::Working, Some(NOW - 600), 2),
            ),
            (
                "s-alpha-claude".to_owned(),
                agent(AgentActivity::Waiting, Some(NOW - 120), 0),
            ),
            (
                "s-beta-codex".to_owned(),
                agent(AgentActivity::Done, Some(NOW - 300), 0),
            ),
            (
                "s-beta-orch".to_owned(),
                agent(AgentActivity::Working, Some(NOW - 60), 0),
            ),
            (
                "s-beta-old".to_owned(),
                agent(AgentActivity::Waiting, Some(NOW - 30), 0),
            ),
        ]),
        chats: Some(vec![
            chat(
                "c-global",
                None,
                json!({"state": "idle"}),
                Some(json!({"scope": "global"})),
            ),
            chat("c-beta-run", Some(BETA), json!({"state": "running"}), None),
            chat(
                "c-alpha-stopped",
                Some(ALPHA),
                json!({"state": "stopped"}),
                None,
            ),
        ]),
        chat_activity: BTreeMap::new(),
        schedules: None,
    }
}

#[test]
fn every_project_is_counted_from_its_own_sessions_chats_and_tasks() {
    let overview = busy().overview();
    let row = |id: &str| {
        overview
            .projects
            .iter()
            .find(|project| project.id == id)
            .unwrap()
            .clone()
    };

    let alpha = row(ALPHA);
    assert_eq!(alpha.folder.as_deref(), Some("Clients"));
    assert_eq!(alpha.live_shells, 3);
    assert_eq!(alpha.live_chats, 0, "a stopped chat is not live");
    assert_eq!((alpha.agents.working, alpha.agents.waiting), (1, 1));
    assert_eq!(alpha.agents.subagents, 2);
    assert_eq!(
        alpha.tasks,
        TaskCounts {
            todo: 0,
            in_progress: 1,
            done: 1
        }
    );
    assert_eq!(alpha.worktrees, 2);
    assert_eq!(alpha.attention, 1);
    assert_eq!(alpha.orchestrator, None);

    let beta = row(BETA);
    assert_eq!(beta.live_shells, 2, "the ended session is not live");
    assert_eq!(beta.live_chats, 1);
    // Its orchestrator's terminal and its running chat both work; its Codex is done.
    assert_eq!((beta.agents.working, beta.agents.done), (2, 1));
    assert_eq!(
        beta.orchestrator,
        Some(ProjectOrchestrator::Terminal {
            activity: Some(AgentActivity::Working)
        })
    );

    let quiet = row(QUIET);
    assert!(quiet.is_quiet());
    assert_eq!(quiet.agents, ActivityCounts::default());

    let totals = overview.totals;
    assert_eq!(totals.projects, 3);
    assert_eq!(totals.active_projects, 2);
    assert_eq!(totals.live_shells, 5);
    // The global orchestrator chat is idle and so live; the stopped one is not.
    assert_eq!(totals.live_chats, 2);
    // Alpha's Codex, beta's orchestrator and beta's chat work; the global chat has finished
    // a turn, as an orchestrator that is idle has.
    assert_eq!(totals.agents.working, 3);
    assert_eq!(totals.agents.waiting, 1);
    assert_eq!(totals.agents.done, 2);
    assert_eq!(
        totals.tasks,
        TaskCounts {
            todo: 1,
            in_progress: 2,
            done: 1
        },
        "the removed project's task is not counted"
    );
    assert_eq!(totals.attention, 1);
}

#[test]
fn each_project_says_where_it_is_and_when_something_last_happened_in_it() {
    let mut fixture = busy();
    // Beta's running chat wrote to its log ten seconds ago, as far as that was read.
    fixture
        .chat_activity
        .insert("c-beta-run".to_owned(), NOW - 10);
    let overview = fixture.overview();
    let row = |id: &str| {
        overview
            .projects
            .iter()
            .find(|project| project.id == id)
            .unwrap()
            .clone()
    };
    let alpha = row(ALPHA);
    assert_eq!(alpha.root, PathBuf::from("/work/alpha"));
    // Its waiting Claude began waiting two minutes ago: later than its tasks changed and
    // than its Codex began working.
    assert_eq!(alpha.last_activity, Some(NOW - 120));
    // The chat's log is the latest of beta's; its ended session counts for nothing.
    assert_eq!(row(BETA).last_activity, Some(NOW - 10));
    fixture.chat_activity.clear();
    assert_eq!(
        fixture
            .overview()
            .projects
            .iter()
            .find(|project| project.id == BETA)
            .unwrap()
            .last_activity,
        Some(NOW - 60),
        "its orchestrator began working a minute ago"
    );
    // Nothing recorded a time for the quiet project: unknown, not zero.
    assert_eq!(row(QUIET).last_activity, None);
    // A task's change is a time of its own.
    let mut tasks_only = busy();
    tasks_only.activity.clear();
    let alpha = tasks_only
        .overview()
        .projects
        .into_iter()
        .find(|project| project.id == ALPHA)
        .unwrap();
    assert_eq!(alpha.last_activity, Some(20));
}

#[test]
fn the_busiest_projects_come_first_and_quiet_ones_last() {
    let overview = busy().overview();
    let order: Vec<&str> = overview.projects.iter().map(|p| p.name.as_str()).collect();
    // Alpha has something waiting; beta works without anything waiting; quiet is quiet.
    assert_eq!(order, ["alpha", "beta", "quiet"]);
}

#[test]
fn work_lists_name_the_exact_session_its_project_and_worktree() {
    let overview = busy().overview();
    assert_eq!(overview.working.len(), 3);
    // The longest-running first; the chat has no start time and goes last.
    let first = &overview.working[0];
    assert_eq!(
        first.session.target,
        Target::Shell {
            id: "s-alpha-codex".into()
        }
    );
    assert_eq!(first.session.agent, "Codex");
    assert_eq!(first.session.project_name.as_deref(), Some("alpha"));
    // Its directory is in the feature worktree, whichever it started in.
    assert_eq!(first.session.branch.as_deref(), Some("feat/x"));
    assert_eq!(first.subagents, 2);
    let orchestrator = &overview.working[1];
    assert_eq!(
        orchestrator.session.orchestrator,
        Some(OrchestratorScope::Project {
            project_id: BETA.into()
        })
    );
    let chat = &overview.working[2];
    assert!(chat.session.is_chat);
    assert_eq!(
        chat.session.target,
        Target::Chat {
            id: "c-beta-run".into(),
            project_id: Some(BETA.into()),
            orchestrator: None
        }
    );
    assert_eq!(chat.session.title.as_deref(), Some("Chat c-beta-run"));

    assert_eq!(overview.finished.len(), 1);
    assert_eq!(
        overview.finished[0].session.target,
        Target::Shell {
            id: "s-beta-codex".into()
        }
    );
}

#[test]
fn an_agent_that_finished_long_ago_is_not_recent_work() {
    let mut fixture = busy();
    fixture.activity.insert(
        "s-beta-codex".into(),
        agent(AgentActivity::Done, Some(NOW - FINISHED_WINDOW_SECS - 1), 0),
    );
    assert!(fixture.overview().finished.is_empty());
}

#[test]
fn the_global_orchestrator_is_found_as_orchestrators_find_would() {
    let mut fixture = busy();
    assert_eq!(
        fixture.overview().global,
        GlobalOrchestrator::Chat {
            id: "c-global".into(),
            provider: Provider::Claude,
            state: ChatState::Idle
        }
    );

    // A project's orchestrator, in either mode, is never the global one.
    fixture.chats = Some(vec![chat(
        "c-alpha-orch",
        Some(ALPHA),
        json!({"state": "idle"}),
        Some(json!({"scope": "project", "project_id": ALPHA})),
    )]);
    assert_eq!(
        fixture.overview().global,
        GlobalOrchestrator::None {
            exited_terminal: false
        }
    );
    let alpha = fixture
        .overview()
        .projects
        .into_iter()
        .find(|project| project.id == ALPHA)
        .unwrap();
    assert_eq!(
        alpha.orchestrator,
        Some(ProjectOrchestrator::Chat {
            provider: Provider::Claude,
            state: ChatState::Idle
        })
    );

    // A live terminal is the global orchestrator even beside a chat that claims the scope:
    // it is kept and shown, never converted.
    fixture.chats = busy().chats;
    fixture
        .shells
        .push(shell("s-global", None, "orchestrator", Some("codex"), true));
    fixture.activity.insert(
        "s-global".into(),
        agent(AgentActivity::Waiting, Some(NOW - 5), 0),
    );
    let overview = fixture.overview();
    assert_eq!(
        overview.global,
        GlobalOrchestrator::Terminal {
            shell_id: "s-global".into(),
            activity: Some(AgentActivity::Waiting)
        }
    );
    // Its waiting counts in the totals and the attention list, but in no project.
    assert_eq!(overview.totals.agents.waiting, 2);
    let waiting = overview
        .attention
        .iter()
        .find(|item| {
            item.target
                == Target::Shell {
                    id: "s-global".into(),
                }
        })
        .unwrap();
    assert_eq!(waiting.project_id, None);
    assert_eq!(
        waiting.session.as_ref().unwrap().orchestrator,
        Some(OrchestratorScope::Global)
    );
    assert!(
        overview
            .projects
            .iter()
            .all(|project| project.agents.waiting <= 1)
    );

    // A terminal that ended leaves room for the chat, as `find` says.
    fixture.shells.last_mut().unwrap().alive = false;
    assert!(matches!(
        fixture.overview().global,
        GlobalOrchestrator::Chat { .. }
    ));
    fixture.chats = Some(Vec::new());
    assert_eq!(
        fixture.overview().global,
        GlobalOrchestrator::None {
            exited_terminal: true
        }
    );
}

#[test]
fn before_the_chats_are_read_nothing_about_them_is_claimed() {
    let mut fixture = busy();
    fixture.chats = None;
    let overview = fixture.overview();
    assert!(!overview.chats_known);
    assert_eq!(overview.global, GlobalOrchestrator::Unknown);
    assert_eq!(overview.totals.live_chats, 0);
    assert!(overview.working.iter().all(|work| !work.session.is_chat));

    // A live terminal orchestrator is known from the sessions alone.
    fixture
        .shells
        .push(shell("s-global", None, "orchestrator", None, true));
    assert!(matches!(
        fixture.overview().global,
        GlobalOrchestrator::Terminal { .. }
    ));
}

#[test]
fn attention_lists_waiting_agents_first_then_recent_failures() {
    let mut fixture = busy();
    let chats = fixture.chats.as_mut().unwrap();
    chats.push(chat(
        "c-alpha-waits",
        Some(ALPHA),
        json!({"state": "waiting"}),
        None,
    ));
    chats.push(chat(
        "c-beta-failed-now",
        Some(BETA),
        json!({"state": "failed", "message": "provider exited"}),
        None,
    ));
    chats.push(chat(
        "c-beta-failed-long-ago",
        Some(BETA),
        json!({"state": "failed", "message": "old"}),
        None,
    ));
    chats.push(chat(
        "c-failed-unread",
        Some(BETA),
        json!({"state": "failed", "message": "not yet stat-ed"}),
        None,
    ));
    chats.push(chat(
        "c-beta-orch-failed",
        Some(BETA),
        json!({"state": "failed", "message": "orchestrator crashed"}),
        Some(json!({"scope": "project", "project_id": BETA})),
    ));
    fixture.chat_activity = BTreeMap::from([
        ("c-alpha-waits".to_owned(), NOW - 10),
        ("c-beta-failed-now".to_owned(), NOW - 60),
        (
            "c-beta-failed-long-ago".to_owned(),
            NOW - FAILED_CHAT_WINDOW_SECS - 1,
        ),
    ]);
    let overview = fixture.overview();
    let targets: Vec<String> = overview
        .attention
        .iter()
        .map(|item| match &item.target {
            Target::Shell { id } | Target::Chat { id, .. } => id.clone(),
            other => format!("{other:?}"),
        })
        .collect();
    assert_eq!(
        targets,
        [
            // Waiting, the newest first.
            "c-alpha-waits",
            "s-alpha-claude",
            // Failures: the recent one, then the orchestrator whose time is unknown.
            "c-beta-failed-now",
            "c-beta-orch-failed",
        ]
    );
    assert_eq!(
        overview.attention[2].kind,
        AttentionKind::ChatFailed {
            message: "provider exited".into()
        }
    );
    assert_eq!(overview.totals.attention, 4);
    let beta = overview.projects.iter().find(|p| p.id == BETA).unwrap();
    assert_eq!(beta.attention, 2);
}

fn schedule(id: &str, scope: serde_json::Value, paused: bool, last: Option<&str>) -> Schedule {
    serde_json::from_value(json!({
        "id": id,
        "revision": 1,
        "title": format!("Automation {id}"),
        "prompt": "check",
        "target": {
            "scope": scope,
            "shell_id": "00000000-0000-4000-8000-000000000000",
            "created_at": 0,
            "command": null,
            "harness": "codex",
            "codex_home": null,
            "pane_identity": "",
            "provider_session": ""
        },
        "timing": {"kind": "interval", "first": NOW - 3600, "seconds": 3600},
        "paused": paused,
        "next_run": NOW + 600,
        "last_run": last.map(|outcome| json!({
            "due_at": NOW - 3600,
            "observed_at": NOW - 3500,
            "outcome": outcome,
            "message": format!("{outcome} at the last run")
        })),
        "check_after": 0
    }))
    .unwrap()
}

#[test]
fn automations_that_did_not_run_need_attention_unless_paused() {
    let mut fixture = busy();
    assert_eq!(fixture.overview().automations, Automations::NotRead);
    fixture.schedules = Some(Err("Invalid schedule store".into()));
    assert_eq!(
        fixture.overview().automations,
        Automations::Unreadable("Invalid schedule store".into())
    );

    fixture.schedules = Some(Ok(vec![
        schedule(
            "a1",
            json!({"scope": "project", "project_id": BETA}),
            false,
            Some("failed"),
        ),
        schedule("a2", json!({"scope": "app"}), false, Some("submitted")),
        schedule("a3", json!({"scope": "app"}), true, Some("missed")),
    ]));
    let overview = fixture.overview();
    let automation = overview
        .attention
        .iter()
        .find(|item| matches!(item.kind, AttentionKind::Automation { .. }))
        .unwrap();
    assert_eq!(
        automation.target,
        Target::Automations {
            project_id: Some(BETA.into())
        }
    );
    assert_eq!(automation.project_name.as_deref(), Some("beta"));
    assert_eq!(automation.at, Some(NOW - 3500));
    assert_eq!(
        overview
            .attention
            .iter()
            .filter(|item| matches!(item.kind, AttentionKind::Automation { .. }))
            .count(),
        1,
        "a submitted run and a paused automation need nothing"
    );
    match overview.automations {
        Automations::Read {
            total,
            paused,
            next: Some(next),
            items,
        } => {
            assert_eq!((total, paused), (3, 1));
            assert_eq!(next.at, NOW + 600);
            assert_eq!(next.title, "Automation a1");
            // Each one, as its row says it: the paused one runs at no time, and comes last.
            let rows: Vec<_> = items
                .iter()
                .map(|item| {
                    (
                        item.id.as_str(),
                        item.project_name.as_deref(),
                        item.paused,
                        item.next_run,
                        item.last_run.clone(),
                    )
                })
                .collect();
            assert_eq!(
                rows,
                [
                    (
                        "a1",
                        Some("beta"),
                        false,
                        Some(NOW + 600),
                        Some((Outcome::Failed, NOW - 3500))
                    ),
                    (
                        "a2",
                        None,
                        false,
                        Some(NOW + 600),
                        Some((Outcome::Submitted, NOW - 3500))
                    ),
                    ("a3", None, true, None, Some((Outcome::Missed, NOW - 3500))),
                ]
            );
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn automations_are_listed_by_their_next_run() {
    let mut fixture = busy();
    let mut later = schedule("later", json!({"scope": "app"}), false, None);
    later.next_run = Some(NOW + 7200);
    let mut never = schedule("never", json!({"scope": "app"}), false, None);
    never.next_run = None;
    let soon = schedule("soon", json!({"scope": "app"}), false, None);
    let paused = schedule("paused", json!({"scope": "app"}), true, None);
    fixture.schedules = Some(Ok(vec![paused, never, later, soon]));
    let Automations::Read { items, .. } = fixture.overview().automations else {
        panic!("read");
    };
    let order: Vec<&str> = items.iter().map(|item| item.id.as_str()).collect();
    assert_eq!(order, ["soon", "later", "never", "paused"]);
}

#[test]
fn tasks_in_progress_name_their_project_and_worktree_newest_first() {
    let overview = busy().overview();
    let tasks: Vec<(&str, &str, Option<&str>)> = overview
        .tasks_in_progress
        .iter()
        .map(|task| {
            (
                task.id.as_str(),
                task.project_name.as_str(),
                task.branch.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        tasks,
        [("t4", "beta", None), ("t1", "alpha", Some("feat/x"))],
        "a removed project's task is left out"
    );
}

#[test]
fn an_empty_mac_has_an_empty_honest_overview() {
    let fixture = Fixture {
        state: State::default(),
        shells: Vec::new(),
        cwds: BTreeMap::new(),
        activity: BTreeMap::new(),
        chats: Some(Vec::new()),
        chat_activity: BTreeMap::new(),
        schedules: Some(Ok(Vec::new())),
    };
    let overview = fixture.overview();
    assert_eq!(overview.totals, Totals::default());
    assert!(overview.attention.is_empty() && overview.working.is_empty());
    assert!(overview.projects.is_empty() && overview.tasks_in_progress.is_empty());
    assert_eq!(
        overview.global,
        GlobalOrchestrator::None {
            exited_terminal: false
        }
    );
    assert_eq!(
        overview.automations,
        Automations::Read {
            total: 0,
            paused: 0,
            next: None,
            items: Vec::new(),
        }
    );
}

#[test]
fn durations_are_short() {
    assert_eq!(short_duration(0), "now");
    assert_eq!(short_duration(42), "42s");
    assert_eq!(short_duration(600), "10m");
    assert_eq!(short_duration(7200), "2h");
    assert_eq!(short_duration(3 * 86400), "3d");
}

// ---- The phone's dashboard (`wire`) ------------------------------------------------------------

/// The fixture the phone's tests decode too (`ios/Tests`): written by
/// `RIWORK_UPDATE_FIXTURES=1 cargo test -p riwork-engine global_overview`, checked otherwise.
const SHARED_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../ios/Tests/Fixtures/dashboard.json"
);

// Canonical ids for the shared fixture: the wire carries UUIDs, as the desktop's own records do.
const S_ALPHA_CODEX: &str = "5e110000-0000-4000-8000-000000000001";
const S_ALPHA_CLAUDE: &str = "5e110000-0000-4000-8000-000000000002";
const S_ALPHA_PLAIN: &str = "5e110000-0000-4000-8000-000000000003";
const S_BETA_CODEX: &str = "5e110000-0000-4000-8000-000000000004";
const S_BETA_ORCH: &str = "5e110000-0000-4000-8000-000000000005";
const S_BETA_OLD: &str = "5e110000-0000-4000-8000-000000000006";
const S_GLOBAL: &str = "5e110000-0000-4000-8000-000000000007";
const C_GLOBAL: &str = "c4a70000-0000-4000-8000-000000000001";
const C_BETA_RUN: &str = "c4a70000-0000-4000-8000-000000000002";
const C_ALPHA_STOPPED: &str = "c4a70000-0000-4000-8000-000000000003";
const C_ALPHA_WAITS: &str = "c4a70000-0000-4000-8000-000000000004";
const C_BETA_WAITS: &str = "c4a70000-0000-4000-8000-000000000005";
const C_BETA_FAILED: &str = "c4a70000-0000-4000-8000-000000000006";
const C_BETA_FAILED_OLD: &str = "c4a70000-0000-4000-8000-000000000007";
const C_ALPHA_ORCH: &str = "c4a70000-0000-4000-8000-000000000008";
const A_BETA_FAILED: &str = "5c4e0000-0000-4000-8000-000000000001";
const A_APP_NEXT: &str = "5c4e0000-0000-4000-8000-000000000002";
const A_APP_PAUSED: &str = "5c4e0000-0000-4000-8000-000000000003";
const A_ALPHA_MISSED: &str = "5c4e0000-0000-4000-8000-000000000004";
/// Two quiet projects whose names differ only in case: their order is their ids'.
const DELTA_LOWER: &str = "dddddddd-0000-4000-8000-000000000004";
const DELTA_UPPER: &str = "dddddddd-0000-4000-8000-000000000005";

/// `fixture` with its short shell, chat and task ids replaced by canonical ones.
fn renamed(mut fixture: Fixture, names: &[(&str, &str)]) -> Fixture {
    let rename = |id: &mut String| {
        if let Some((_, new)) = names.iter().find(|(old, _)| old == id) {
            *id = (*new).to_owned();
        }
    };
    for shell in &mut fixture.shells {
        rename(&mut shell.id);
    }
    for chat in fixture.chats.iter_mut().flatten() {
        rename(&mut chat.id);
    }
    for task in &mut fixture.state.tasks {
        rename(&mut task.id);
    }
    fn rekey<T>(map: &mut BTreeMap<String, T>, rename: impl Fn(&mut String)) {
        *map = std::mem::take(map)
            .into_iter()
            .map(|(mut id, value)| {
                rename(&mut id);
                (id, value)
            })
            .collect();
    }
    rekey(&mut fixture.activity, rename);
    rekey(&mut fixture.cwds, rename);
    rekey(&mut fixture.chat_activity, rename);
    fixture
}

fn with(mut chat: ChatInfo, provider: Provider, user_title: Option<&str>) -> ChatInfo {
    chat.provider = provider;
    chat.user_title = user_title.map(str::to_owned);
    chat
}

fn timed_schedule(
    id: &str,
    scope: serde_json::Value,
    paused: bool,
    last: Option<(&str, u64)>,
    next: Option<u64>,
    title: &str,
) -> Schedule {
    let mut schedule = schedule(id, scope, paused, last.map(|(outcome, _)| outcome));
    if let (Some(run), Some((_, at))) = (schedule.last_run.as_mut(), last) {
        run.observed_at = at;
    }
    schedule.next_run = next;
    schedule.title = title.to_owned();
    schedule
}

/// `busy()`, with canonical ids and everything the phone's dashboard tells apart: waiting
/// agents at known and unknown times, a recent and an orchestrator failure (and an old one
/// that is history), automations that failed and missed, with and without a next run and
/// paused, Codex, Claude and Muse chats, and two quiet projects only their ids order.
fn golden_busy() -> Fixture {
    let mut fixture = busy();
    fixture
        .state
        .projects
        .push(project(DELTA_UPPER, "Delta", None));
    fixture
        .state
        .projects
        .push(project(DELTA_LOWER, "delta", None));
    let chats = fixture.chats.as_mut().unwrap();
    for chat in chats.iter_mut() {
        if chat.id == "c-beta-run" {
            *chat = with(chat.clone(), Provider::Codex, Some("Fix the login flow"));
        }
    }
    chats.push(chat(
        "c-alpha-waits",
        Some(ALPHA),
        json!({"state": "waiting"}),
        None,
    ));
    chats.push(with(
        chat(
            "c-beta-waits",
            Some(BETA),
            json!({"state": "waiting"}),
            None,
        ),
        Provider::Muse,
        None,
    ));
    chats.push(with(
        chat(
            "c-beta-failed",
            Some(BETA),
            json!({"state": "failed", "message": "provider exited with status 1"}),
            None,
        ),
        Provider::Codex,
        None,
    ));
    chats.push(chat(
        "c-beta-failed-old",
        Some(BETA),
        json!({"state": "failed", "message": "old"}),
        None,
    ));
    chats.push(chat(
        "c-alpha-orch",
        Some(ALPHA),
        json!({"state": "failed", "message": "orchestrator crashed"}),
        Some(json!({"scope": "project", "project_id": ALPHA})),
    ));
    fixture.chat_activity = BTreeMap::from([
        ("c-alpha-waits".to_owned(), NOW - 10),
        ("c-beta-failed".to_owned(), NOW - 60),
        (
            "c-beta-failed-old".to_owned(),
            NOW - FAILED_CHAT_WINDOW_SECS - 1,
        ),
    ]);
    fixture.schedules = Some(Ok(vec![
        timed_schedule(
            A_APP_PAUSED,
            json!({"scope": "app"}),
            true,
            Some(("missed", NOW - 9000)),
            Some(NOW + 300),
            "Weekly cleanup",
        ),
        timed_schedule(
            A_ALPHA_MISSED,
            json!({"scope": "project", "project_id": ALPHA}),
            false,
            Some(("missed", NOW - 7000)),
            None,
            "Release notes",
        ),
        timed_schedule(
            A_APP_NEXT,
            json!({"scope": "app"}),
            false,
            Some(("submitted", NOW - 3600)),
            Some(NOW + 7200),
            "Morning summary",
        ),
        timed_schedule(
            A_BETA_FAILED,
            json!({"scope": "project", "project_id": BETA}),
            false,
            Some(("failed", NOW - 3500)),
            Some(NOW + 600),
            "Nightly tests",
        ),
    ]));
    renamed(
        fixture,
        &[
            ("s-alpha-codex", S_ALPHA_CODEX),
            ("s-alpha-claude", S_ALPHA_CLAUDE),
            ("s-alpha-plain", S_ALPHA_PLAIN),
            ("s-beta-codex", S_BETA_CODEX),
            ("s-beta-orch", S_BETA_ORCH),
            ("s-beta-old", S_BETA_OLD),
            ("c-global", C_GLOBAL),
            ("c-beta-run", C_BETA_RUN),
            ("c-alpha-stopped", C_ALPHA_STOPPED),
            ("c-alpha-waits", C_ALPHA_WAITS),
            ("c-beta-waits", C_BETA_WAITS),
            ("c-beta-failed", C_BETA_FAILED),
            ("c-beta-failed-old", C_BETA_FAILED_OLD),
            ("c-alpha-orch", C_ALPHA_ORCH),
            ("t1", "7a5c0000-0000-4000-8000-000000000001"),
            ("t2", "7a5c0000-0000-4000-8000-000000000002"),
            ("t3", "7a5c0000-0000-4000-8000-000000000003"),
            ("t4", "7a5c0000-0000-4000-8000-000000000004"),
            ("t5", "7a5c0000-0000-4000-8000-000000000005"),
        ],
    )
}

fn empty_mac() -> Fixture {
    Fixture {
        state: State::default(),
        shells: Vec::new(),
        cwds: BTreeMap::new(),
        activity: BTreeMap::new(),
        chats: Some(Vec::new()),
        chat_activity: BTreeMap::new(),
        schedules: Some(Ok(Vec::new())),
    }
}

/// Every case of the shared fixture, by name.
fn shared_cases() -> serde_json::Value {
    let mut chats_unknown = golden_busy();
    chats_unknown.chats = None;
    // A global orchestrator in a terminal whose agent was not sampled, and automations that
    // could not be read.
    let mut global_terminal = golden_busy();
    global_terminal
        .shells
        .push(shell(S_GLOBAL, None, "orchestrator", Some("codex"), true));
    global_terminal.schedules = Some(Err(
        "Invalid schedule store: expected value at line 1 column 1".into(),
    ));
    // A global terminal that ended and no chat in its place, before the automations were read.
    let mut global_exited = empty_mac();
    global_exited.state = state();
    global_exited
        .shells
        .push(shell(S_GLOBAL, None, "orchestrator", None, false));
    global_exited.schedules = None;
    json!({
        "busy": wire(&golden_busy().overview(), NOW),
        "chats_unknown": wire(&chats_unknown.overview(), NOW),
        "empty": wire(&empty_mac().overview(), NOW),
        "global_terminal": wire(&global_terminal.overview(), NOW),
        "global_exited": wire(&global_exited.overview(), NOW),
    })
}

/// `value` with every object's keys in order, whatever map serde_json was built with.
fn sorted(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            serde_json::Value::Object(
                keys.into_iter()
                    .map(|key| (key.clone(), sorted(&map[key])))
                    .collect(),
            )
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(sorted).collect())
        }
        other => other.clone(),
    }
}

#[test]
fn the_wire_is_the_shared_fixture_the_phone_decodes() {
    let cases = shared_cases();
    let mut expected = serde_json::to_string_pretty(&sorted(&cases)).unwrap();
    expected.push('\n');
    let path = Path::new(SHARED_FIXTURE);
    if std::env::var_os("RIWORK_UPDATE_FIXTURES").is_some_and(|flag| flag == "1") {
        std::fs::write(path, &expected).unwrap();
        return;
    }
    let saved = std::fs::read_to_string(path).unwrap_or_else(|error| {
        panic!("{SHARED_FIXTURE}: {error}; write it with RIWORK_UPDATE_FIXTURES=1")
    });
    let decoded: serde_json::Value = serde_json::from_str(&saved).unwrap();
    assert_eq!(
        decoded, cases,
        "the wire changed; if that is intended, rewrite {SHARED_FIXTURE} with \
         RIWORK_UPDATE_FIXTURES=1 and update the phone's decoder and docs/remote-protocol.md"
    );
    assert_eq!(
        saved, expected,
        "{SHARED_FIXTURE} is not in its canonical form"
    );
}

/// What the phone's tests check again in the shared fixture, so it can never lose it.
#[test]
fn the_shared_fixture_has_what_the_phone_tells_apart() {
    let cases = shared_cases();
    let busy = &cases["busy"];
    let ids = |list: &str| -> Vec<String> {
        busy[list]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                let target = row.get("target").unwrap_or(&row["session"]["target"]);
                target["id"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("automations:{}", target["project_id"]))
            })
            .collect()
    };
    // Waiting first, the newest first and the unknown time last; then failures and
    // automations, the newest first and the unknown time last.
    assert_eq!(
        ids("attention"),
        [
            C_ALPHA_WAITS,
            S_ALPHA_CLAUDE,
            C_BETA_WAITS,
            C_BETA_FAILED,
            &format!("automations:\"{BETA}\""),
            &format!("automations:\"{ALPHA}\""),
            C_ALPHA_ORCH,
        ]
    );
    let kinds: Vec<&str> = busy["attention"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "waiting",
            "waiting",
            "waiting",
            "chat_failed",
            "automation",
            "automation",
            "chat_failed"
        ]
    );
    assert!(busy["attention"][2].get("at").is_none());
    assert_eq!(busy["attention"][4]["outcome"], "failed");
    assert_eq!(busy["attention"][5]["outcome"], "missed");
    // The longest-running first; a chat has no start time and goes last.
    assert_eq!(ids("working"), [S_ALPHA_CODEX, S_BETA_ORCH, C_BETA_RUN]);
    assert!(busy["working"][2].get("since_unix").is_none());
    assert_eq!(ids("finished"), [S_BETA_CODEX]);
    let projects: Vec<&str> = busy["projects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|project| project["id"].as_str().unwrap())
        .collect();
    assert_eq!(projects, [ALPHA, BETA, DELTA_LOWER, DELTA_UPPER, QUIET]);
    assert_eq!(busy["projects"][0]["orchestrator"]["mode"], "chat");
    assert_eq!(busy["projects"][1]["orchestrator"]["mode"], "terminal");
    assert_eq!(busy["tasks_in_progress_total"], 2);
    assert_eq!(busy["global"]["kind"], "chat");
    let automations = &busy["automations"];
    assert_eq!(automations["state"], "read");
    let paused: Vec<bool> = automations["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["paused"].as_bool().unwrap())
        .collect();
    assert_eq!(paused, [false, false, false, true]);
    assert!(automations["items"][2].get("next_run").is_none());

    assert_eq!(cases["chats_unknown"]["global"], json!({"kind": "unknown"}));
    assert_eq!(cases["global_terminal"]["global"]["kind"], "terminal");
    assert_eq!(
        cases["global_exited"]["global"],
        json!({"kind": "none", "exited_terminal": true})
    );
}

/// Every `null` in `value`, by path.
fn nulls(value: &serde_json::Value, path: &str, found: &mut Vec<String>) {
    match value {
        serde_json::Value::Null => found.push(path.to_owned()),
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                nulls(value, &format!("{path}.{key}"), found);
            }
        }
        serde_json::Value::Array(items) => {
            for (index, value) in items.iter().enumerate() {
                nulls(value, &format!("{path}[{index}]"), found);
            }
        }
        _ => {}
    }
}

#[test]
fn the_wire_leaves_out_what_is_not_known_and_never_writes_null() {
    for (name, case) in shared_cases().as_object().unwrap() {
        let mut found = Vec::new();
        nulls(case, name, &mut found);
        assert!(found.is_empty(), "{found:?}");
        assert_eq!(case["v"], WIRE_VERSION, "{name}");
        assert_eq!(case["generated_at"], NOW, "{name}");
    }
    let mut fixture = golden_busy();
    let known = wire(&fixture.overview(), NOW);
    assert_eq!(known["chats_known"], true);
    // The idle global chat, the running one and the two waiting ones.
    assert_eq!(known["totals"]["live_chats"], 4);
    assert!(
        known["projects"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["live_chats"].is_u64())
    );

    fixture.chats = None;
    let unknown = wire(&fixture.overview(), NOW);
    assert_eq!(unknown["chats_known"], false);
    assert!(unknown["totals"].get("live_chats").is_none());
    for project in unknown["projects"].as_array().unwrap() {
        assert!(project.get("live_chats").is_none(), "{project}");
        assert!(project["live_shells"].is_u64());
    }
    assert_eq!(unknown["global"], json!({"kind": "unknown"}));
    // A project nothing recorded a time for has no `last_activity`, not zero.
    let quiet = unknown["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|project| project["id"] == QUIET)
        .unwrap();
    assert!(quiet.get("last_activity").is_none());
    assert!(quiet.get("orchestrator").is_none());
    assert!(quiet.get("folder").is_none());
}

#[test]
fn the_wire_cuts_each_list_after_ordering_and_says_how_long_it_was() {
    let mut overview = golden_busy().overview();
    let first = |list: &[serde_json::Value]| list.first().cloned();
    let attention = overview.attention[0].clone();
    overview.attention = (0..WIRE_MAX_ATTENTION as u64 + 5)
        .map(|n| Attention {
            at: Some(NOW - n),
            ..attention.clone()
        })
        .collect();
    let work = overview.working[0].clone();
    overview.working = (0..WIRE_MAX_WORKING as u64 + 1)
        .map(|n| Work {
            since_unix: Some(NOW - 1000 + n),
            ..work.clone()
        })
        .collect();
    overview.finished = (0..WIRE_MAX_FINISHED as u64 + 7)
        .map(|n| Work {
            since_unix: Some(NOW - n),
            ..work.clone()
        })
        .collect();
    let project = overview.projects[0].clone();
    overview.projects = (0..WIRE_MAX_PROJECTS + 50)
        .map(|n| ProjectOverview {
            id: format!("{n:08x}-0000-4000-8000-000000000000"),
            name: format!("project {n:03}"),
            ..project.clone()
        })
        .collect();
    let task = overview.tasks_in_progress[0].clone();
    overview.tasks_in_progress = (0..WIRE_MAX_TASKS_IN_PROGRESS as u64 + 10)
        .map(|n| TaskOverview {
            updated_at: NOW - n,
            ..task.clone()
        })
        .collect();
    let Automations::Read { items, .. } = &mut overview.automations else {
        panic!("read");
    };
    let item = items[0].clone();
    *items = (0..WIRE_MAX_AUTOMATIONS + 3)
        .map(|n| AutomationSummary {
            title: format!("automation {n:02}"),
            ..item.clone()
        })
        .collect();

    let value = wire(&overview, NOW);
    for (list, cap, total) in [
        ("attention", WIRE_MAX_ATTENTION, WIRE_MAX_ATTENTION + 5),
        ("working", WIRE_MAX_WORKING, WIRE_MAX_WORKING + 1),
        ("finished", WIRE_MAX_FINISHED, WIRE_MAX_FINISHED + 7),
        ("projects", WIRE_MAX_PROJECTS, WIRE_MAX_PROJECTS + 50),
        (
            "tasks_in_progress",
            WIRE_MAX_TASKS_IN_PROGRESS,
            WIRE_MAX_TASKS_IN_PROGRESS + 10,
        ),
    ] {
        assert_eq!(value[list].as_array().unwrap().len(), cap, "{list}");
        assert_eq!(value[format!("{list}_total")], total, "{list}");
    }
    // The rows kept are the first ones, in the overview's order: the tail is what is cut.
    assert_eq!(value["attention"][0]["at"], NOW);
    assert_eq!(value["attention"][WIRE_MAX_ATTENTION - 1]["at"], NOW - 39);
    assert_eq!(value["projects"][149]["name"], "project 149");
    assert_eq!(
        first(value["finished"].as_array().unwrap()).unwrap()["since_unix"],
        NOW
    );
    let automations = &value["automations"];
    assert_eq!(
        automations["items"].as_array().unwrap().len(),
        WIRE_MAX_AUTOMATIONS
    );
    assert_eq!(automations["items_total"], WIRE_MAX_AUTOMATIONS + 3);
    assert_eq!(automations["items"][29]["title"], "automation 29");
    // The totals and counts are the whole Mac's, never the cut lists'.
    assert_eq!(automations["total"], 4);
    assert_eq!(value["totals"]["projects"], 5);
}

#[test]
fn the_wire_bounds_text_by_unicode_scalars_and_never_splits_one() {
    let mut overview = golden_busy().overview();
    let long_name: String = "é🙂x".repeat(100);
    let long_message: String = "日本🙂".repeat(200);
    overview.projects[0].name = long_name.clone();
    overview.projects[0].folder = Some(long_name.clone());
    overview.tasks_in_progress[0].title = long_name.clone();
    overview.tasks_in_progress[0].branch = Some(long_name.clone());
    overview.working[0].session.title = Some(long_name.clone());
    overview.attention[0].kind = AttentionKind::ChatFailed {
        message: long_message.clone(),
    };
    overview.automations = Automations::Unreadable(long_message.clone());
    let short = "naïve 🙂".to_owned();
    overview.projects[1].name = short.clone();

    let value = wire(&overview, NOW);
    let scalars = |value: &serde_json::Value| value.as_str().unwrap().chars().count();
    let name = &value["projects"][0]["name"];
    assert_eq!(scalars(name), WIRE_MAX_NAME_CHARS);
    assert!(long_name.starts_with(name.as_str().unwrap()));
    assert_eq!(
        scalars(&value["projects"][0]["folder"]),
        WIRE_MAX_NAME_CHARS
    );
    assert_eq!(
        scalars(&value["tasks_in_progress"][0]["title"]),
        WIRE_MAX_NAME_CHARS
    );
    assert_eq!(
        scalars(&value["tasks_in_progress"][0]["branch"]),
        WIRE_MAX_NAME_CHARS
    );
    assert_eq!(
        scalars(&value["working"][0]["session"]["title"]),
        WIRE_MAX_NAME_CHARS
    );
    let message = &value["attention"][0]["message"];
    assert_eq!(scalars(message), WIRE_MAX_MESSAGE_CHARS);
    assert!(long_message.starts_with(message.as_str().unwrap()));
    assert_eq!(
        scalars(&value["automations"]["message"]),
        WIRE_MAX_MESSAGE_CHARS
    );
    // Short text is passed on as it is.
    assert_eq!(value["projects"][1]["name"], short);
}

#[test]
fn the_dashboard_reads_the_automations_and_the_times_of_failed_and_waiting_chats_only() {
    let home = std::env::temp_dir().join(format!("riwork-overview-{}", uuid::Uuid::new_v4()));
    let waiting = uuid::Uuid::new_v4().to_string();
    let failed = uuid::Uuid::new_v4().to_string();
    let unlogged = uuid::Uuid::new_v4().to_string();
    let log = crate::chat::log::chat_dir(&home, &waiting).unwrap();
    std::fs::create_dir_all(&log).unwrap();
    std::fs::write(log.join("events.jsonl"), "{}\n").unwrap();
    let log = crate::chat::log::chat_dir(&home, &failed).unwrap();
    std::fs::create_dir_all(&log).unwrap();
    std::fs::write(log.join("events.jsonl"), "{}\n").unwrap();

    let mut chats = vec![
        chat(&waiting, None, json!({"state": "waiting"}), None),
        chat(
            &failed,
            None,
            json!({"state": "failed", "message": "x"}),
            None,
        ),
        chat(
            &unlogged,
            None,
            json!({"state": "failed", "message": "x"}),
            None,
        ),
        chat("c-running", None, json!({"state": "running"}), None),
        chat("c-idle", None, json!({"state": "idle"}), None),
    ];
    assert_eq!(timed_chats(None), Vec::<String>::new());
    let timed = timed_chats(Some(&chats));
    assert_eq!(timed, [waiting.clone(), failed.clone(), unlogged.clone()]);
    // A few hundred at most, however many chats failed.
    chats.extend((0..TIMED_CHATS_MAX + 10).map(|n| {
        chat(
            &format!("c-failed-{n}"),
            None,
            json!({"state": "failed", "message": "x"}),
            None,
        )
    }));
    assert_eq!(timed_chats(Some(&chats)).len(), TIMED_CHATS_MAX);

    let (schedules, written) = read_extras(&home, [timed, vec!["../escape".into()]].concat());
    // No ledger yet: no automations, which is known, not unread.
    assert_eq!(schedules, Ok(Vec::new()));
    let now = crate::activity::unix_now();
    assert_eq!(
        written.keys().cloned().collect::<Vec<_>>(),
        {
            let mut logged = vec![waiting.clone(), failed.clone()];
            logged.sort();
            logged
        },
        "a chat with no log, or an id that is not a UUID, has no time"
    );
    assert!(written.values().all(|at| now.abs_diff(*at) < 60));

    // A damaged ledger is unreadable, and says why.
    std::fs::write(home.join("schedules.json"), "not json").unwrap();
    let (schedules, _) = read_extras(&home, Vec::new());
    assert!(
        schedules
            .as_ref()
            .is_err_and(|error| error.starts_with("Invalid schedule store")),
        "{schedules:?}"
    );
    std::fs::remove_dir_all(&home).unwrap();
}
