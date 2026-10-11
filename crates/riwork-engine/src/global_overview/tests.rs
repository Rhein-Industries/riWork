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
        } => {
            assert_eq!((total, paused), (3, 1));
            assert_eq!(next.at, NOW + 600);
            assert_eq!(next.title, "Automation a1");
        }
        other => panic!("{other:?}"),
    }
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
            next: None
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
