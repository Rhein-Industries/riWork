use super::*;
use crate::{
    activity::ActivityCounts,
    form_input::{test_turn, test_window},
    global_overview::{Totals, Work},
};
use gpui::{Render, TestAppContext};
use gpui_kit::test::TestWindowExt;

const NOW: u64 = 1_800_000_000;
const ALPHA: &str = "aaaaaaaa-0000-4000-8000-000000000001";

fn session(target: Target, agent: &'static str, is_chat: bool) -> SessionSummary {
    SessionSummary {
        target,
        agent,
        title: is_chat.then(|| "Fix the build".to_owned()),
        project_id: Some(ALPHA.into()),
        project_name: Some("alpha".into()),
        branch: Some("feat/x".into()),
        orchestrator: None,
        is_chat,
    }
}

fn overview(global: GlobalOrchestrator) -> Overview {
    let shell = Target::Shell {
        id: "shell-1".into(),
    };
    let chat = Target::Chat {
        id: "chat-1".into(),
        project_id: Some(ALPHA.into()),
        orchestrator: None,
    };
    Overview {
        totals: Totals {
            projects: 2,
            active_projects: 1,
            live_shells: 2,
            live_chats: 1,
            agents: ActivityCounts {
                working: 1,
                waiting: 1,
                ..ActivityCounts::default()
            },
            tasks: TaskCounts {
                todo: 1,
                in_progress: 1,
                done: 2,
            },
            attention: 1,
        },
        attention: vec![Attention {
            kind: AttentionKind::Waiting,
            target: chat.clone(),
            session: Some(session(chat, "Claude", true)),
            project_id: Some(ALPHA.into()),
            project_name: Some("alpha".into()),
            at: Some(NOW - 120),
        }],
        working: vec![Work {
            session: session(shell, "Codex", false),
            activity: AgentActivity::Working,
            since_unix: Some(NOW - 600),
            subagents: 2,
        }],
        finished: Vec::new(),
        projects: vec![
            ProjectOverview {
                id: ALPHA.into(),
                name: "alpha".into(),
                folder: None,
                agents: ActivityCounts {
                    working: 1,
                    waiting: 1,
                    ..ActivityCounts::default()
                },
                live_shells: 2,
                live_chats: 1,
                tasks: TaskCounts {
                    todo: 1,
                    in_progress: 1,
                    done: 2,
                },
                worktrees: 2,
                attention: 1,
                orchestrator: None,
            },
            ProjectOverview {
                id: "quiet".into(),
                name: "quiet".into(),
                folder: None,
                agents: ActivityCounts::default(),
                live_shells: 0,
                live_chats: 0,
                tasks: TaskCounts::default(),
                worktrees: 1,
                attention: 0,
                orchestrator: None,
            },
        ],
        tasks_in_progress: vec![TaskOverview {
            id: "task-1".into(),
            title: "Ship it".into(),
            project_id: ALPHA.into(),
            project_name: "alpha".into(),
            branch: None,
            updated_at: NOW,
        }],
        global,
        automations: Automations::Read {
            total: 0,
            paused: 0,
            next: None,
        },
        chats_known: true,
    }
}

struct Fixture {
    overview: Overview,
    sampled_at: Option<u64>,
    sample_error: Option<String>,
    show_quiet: bool,
    actions: Vec<DashboardAction>,
    /// The pane's width; the window is wider.
    width: f32,
}

impl Render for Fixture {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dashboard = render(
            DashboardData {
                overview: &self.overview,
                now: NOW,
                sampled_at: self.sampled_at,
                sample_error: self.sample_error.as_deref(),
                chat_starting: false,
                runs: OrchestratorRuns::Chat(Provider::Codex),
                remote_host: None,
                show_quiet: self.show_quiet,
            },
            |fixture: &mut Self, action, _, cx| {
                if action == DashboardAction::ToggleQuietProjects {
                    fixture.show_quiet = !fixture.show_quiet;
                }
                fixture.actions.push(action);
                cx.notify();
            },
            cx,
        );
        div()
            .id("dashboard-pane")
            .w(gpui::px(self.width))
            .h(gpui::px(2000.0))
            .child(dashboard)
    }
}

fn mount(
    cx: &mut TestAppContext,
    global: GlobalOrchestrator,
    sampled_at: Option<u64>,
) -> (gpui::AnyWindowHandle, gpui::Entity<Fixture>) {
    mount_at(cx, global, sampled_at, 900.0)
}

fn mount_at(
    cx: &mut TestAppContext,
    global: GlobalOrchestrator,
    sampled_at: Option<u64>,
    width: f32,
) -> (gpui::AnyWindowHandle, gpui::Entity<Fixture>) {
    test_window(cx, move |_, _| Fixture {
        overview: overview(global),
        sampled_at,
        sample_error: None,
        show_quiet: false,
        actions: Vec::new(),
        width,
    })
}

#[test]
fn a_terminal_orchestrator_is_offered_as_itself_and_never_as_a_new_chat() {
    let terminal = GlobalOrchestrator::Terminal {
        shell_id: "shell-g".into(),
        activity: Some(AgentActivity::Working),
    };
    for runs in [
        OrchestratorRuns::Chat(Provider::Claude),
        OrchestratorRuns::Terminal,
    ] {
        let copy = chat_copy(&terminal, runs, false, None);
        assert_eq!(copy.action, "Open terminal orchestrator");
        assert!(!copy.starts, "the terminal is opened, not replaced");
        assert!(copy.status.contains("terminal"));
        assert!(copy.note.unwrap().contains("keeps this terminal"));
    }
}

#[test]
fn chat_controls_say_whether_a_click_opens_or_starts() {
    let chat = GlobalOrchestrator::Chat {
        id: "chat-g".into(),
        provider: Provider::Codex,
        state: ChatState::Idle,
    };
    let copy = chat_copy(&chat, OrchestratorRuns::Terminal, false, None);
    assert_eq!(
        (copy.action.as_str(), copy.starts, copy.enabled),
        ("Open global chat", false, true)
    );
    assert_eq!(copy.status, "Codex · Idle");
    assert_eq!(copy.provider, Some(Provider::Codex));

    let none = GlobalOrchestrator::None {
        exited_terminal: true,
    };
    let start = chat_copy(&none, OrchestratorRuns::Chat(Provider::Claude), false, None);
    assert_eq!(start.action, "Start global chat");
    assert!(start.starts);
    assert!(start.status.contains("terminal has exited"));
    assert!(start.note.unwrap().contains("Claude"));
    let terminal = chat_copy(&none, OrchestratorRuns::Terminal, false, None);
    assert_eq!(terminal.action, "Start global orchestrator");

    // While one is being found or started, a second click does nothing.
    let busy = chat_copy(&none, OrchestratorRuns::Terminal, true, None);
    assert!(!busy.enabled && !busy.starts);
    // Another Mac's project leads to that Mac's orchestrator; this one starts none for it.
    let remote = chat_copy(&none, OrchestratorRuns::Terminal, false, Some("Studio"));
    assert_eq!(remote.action, "Open chat on Studio");
    assert!(!remote.starts);
}

#[test]
fn the_menu_summary_is_honest_before_and_after_the_first_read() {
    let overview = overview(GlobalOrchestrator::Unknown);
    assert_eq!(summary(&overview, false), "Reading sessions…");
    assert_eq!(
        summary(&overview, true),
        "1 working · 1 need you · 1 tasks in progress"
    );
    let mut quiet = overview.clone();
    quiet.totals = Totals {
        projects: 2,
        ..Totals::default()
    };
    assert_eq!(summary(&quiet, true), "All quiet across 2 projects");
}

#[test]
fn attention_rows_name_the_session_and_where_it_is() {
    let overview = overview(GlobalOrchestrator::Unknown);
    let (title, detail) = attention_copy(&overview.attention[0], NOW);
    assert_eq!(title, "Claude chat · Fix the build waits for you");
    assert_eq!(detail, "alpha · feat/x · since 2m ago");
}

#[gpui::test]
fn rows_open_their_exact_target_and_chat_is_only_a_click(cx: &mut TestAppContext) {
    let (window, fixture) = mount(
        cx,
        GlobalOrchestrator::None {
            exited_terminal: false,
        },
        Some(NOW - 1),
    );
    // Drawing the dashboard asked for nothing, a chat least of all.
    test_turn(cx, window, |_, app| {
        assert!(fixture.read(app).actions.is_empty());
    });
    test_turn(cx, window, |window, app| {
        window.click("dashboard-working-0", app)
    });
    test_turn(cx, window, |window, app| {
        window.click("dashboard-attention-0", app)
    });
    test_turn(cx, window, |window, app| {
        window.click(format!("dashboard-project-{ALPHA}-tasks"), app)
    });
    test_turn(cx, window, |window, app| {
        window.click("dashboard-task-task-1", app)
    });
    test_turn(cx, window, |window, app| {
        window.click("dashboard-nav-chat", app)
    });
    test_turn(cx, window, |window, app| {
        window.click("dashboard-global-chat", app)
    });
    test_turn(cx, window, |_, app| {
        assert_eq!(
            fixture.read(app).actions,
            [
                DashboardAction::Open(Target::Shell {
                    id: "shell-1".into()
                }),
                DashboardAction::Open(Target::Chat {
                    id: "chat-1".into(),
                    project_id: Some(ALPHA.into()),
                    orchestrator: None
                }),
                DashboardAction::ProjectPanel {
                    project_id: ALPHA.into(),
                    panel: PanelKind::Tasks
                },
                DashboardAction::Open(Target::Task {
                    id: "task-1".into()
                }),
                DashboardAction::GlobalChat,
                DashboardAction::GlobalChat,
            ]
        );
    });
}

#[gpui::test]
fn quiet_projects_fold_away_and_come_back_on_request(cx: &mut TestAppContext) {
    let (window, fixture) = mount(cx, GlobalOrchestrator::Unknown, Some(NOW - 1));
    test_turn(cx, window, |window, app| {
        window.click(format!("dashboard-project-{ALPHA}"), app)
    });
    test_turn(cx, window, |window, app| {
        window.click("dashboard-toggle-quiet", app)
    });
    // Shown now: the quiet project's row opens it like any other.
    test_turn(cx, window, |window, app| {
        window.click("dashboard-project-quiet", app)
    });
    test_turn(cx, window, |_, app| {
        let fixture = fixture.read(app);
        assert!(fixture.show_quiet);
        assert_eq!(
            fixture.actions,
            [
                DashboardAction::Open(Target::Project { id: ALPHA.into() }),
                DashboardAction::ToggleQuietProjects,
                DashboardAction::Open(Target::Project { id: "quiet".into() }),
            ]
        );
    });
}

#[gpui::test]
fn before_the_first_read_it_says_so_and_can_still_refresh(cx: &mut TestAppContext) {
    let (window, fixture) = mount(cx, GlobalOrchestrator::Unknown, None);
    test_turn(cx, window, |window, app| {
        let node = crate::form_input::test_ax_node(window, app, "dashboard-refresh");
        assert_eq!(node.role(), gpui::Role::Button);
        window.click("dashboard-refresh", app);
    });
    // The keyboard reaches the same control: Enter on the focused button refreshes again.
    test_turn(cx, window, |window, app| window.press("enter", app));
    test_turn(cx, window, |_, app| {
        assert_eq!(
            fixture.read(app).actions,
            [DashboardAction::Refresh, DashboardAction::Refresh]
        );
    });
}

#[gpui::test]
fn columns_sit_side_by_side_when_wide_and_stack_when_narrow(cx: &mut TestAppContext) {
    let project = format!("dashboard-project-{ALPHA}");
    // Both looks, and Native at a larger interface text size, which grows every space.
    let looks = [
        (1.0, ui_text::Face::Menlo, 900.0, true),
        (1.0, ui_text::Face::Menlo, 340.0, false),
        (1.0, ui_text::Face::System, 900.0, true),
        (1.0, ui_text::Face::System, 340.0, false),
        (1.3, ui_text::Face::System, 340.0, false),
    ];
    for (scale, face, width, side_by_side) in looks {
        let before = ui_text::set_for_tests(scale, face);
        let (window, _) = mount_at(cx, GlobalOrchestrator::Unknown, Some(NOW - 1), width);
        let project = project.clone();
        test_turn(cx, window, move |window, _| {
            // The first column ends with the chat; the second starts with the projects.
            let working = window.find("dashboard-working-0").bounds();
            let chat = window.find("dashboard-global-chat").bounds();
            let first_project = window.find(project.clone()).bounds();
            if side_by_side {
                assert!(
                    working.right() <= first_project.left(),
                    "{width}: {working:?} {first_project:?}"
                );
            } else {
                assert!(
                    chat.bottom() <= first_project.top(),
                    "{width}: {chat:?} {first_project:?}"
                );
            }
            // Nothing reaches past the pane's right edge.
            for id in [
                "dashboard-refresh".to_owned(),
                "dashboard-nav-chat".to_owned(),
                "dashboard-attention-0".to_owned(),
                "dashboard-working-0".to_owned(),
                "dashboard-global-chat".to_owned(),
                project.clone(),
                format!("{project}-sessions"),
                "dashboard-task-task-1".to_owned(),
                "dashboard-toggle-quiet".to_owned(),
            ] {
                let right = window.find(id.clone()).bounds().right();
                assert!(
                    right <= gpui::px(width + 0.5),
                    "{width}: {id} ends at {right:?}"
                );
            }
        });
        ui_text::set_for_tests(before.0, before.1);
    }
}

#[gpui::test]
fn a_failed_first_read_shows_no_figures_and_a_stale_one_says_so(cx: &mut TestAppContext) {
    let (window, fixture) = mount(cx, GlobalOrchestrator::Unknown, None);
    test_turn(cx, window, |_, app| {
        fixture.update(app, |fixture, cx| {
            fixture.sample_error = Some("tmux: server exited unexpectedly".into());
            cx.notify();
        })
    });
    test_turn(cx, window, |window, _| {
        // No row and no tile is drawn from a read that did not happen; Refresh still is.
        assert!(window.try_find("dashboard-working-0").is_none());
        assert!(window.try_find("dashboard-global-chat").is_none());
        assert!(window.try_find("dashboard-refresh").is_some());
    });
    // Once read, the figures stay on screen while a later read fails, and the header says so.
    test_turn(cx, window, |_, app| {
        fixture.update(app, |fixture, cx| {
            fixture.sampled_at = Some(NOW - 600);
            cx.notify();
        })
    });
    test_turn(cx, window, |window, _| {
        assert!(window.try_find("dashboard-working-0").is_some());
    });
}

#[test]
fn an_empty_attention_list_claims_only_what_was_read() {
    let mut quiet = overview(GlobalOrchestrator::Unknown);
    quiet.attention.clear();
    assert_eq!(
        quiet_attention(&quiet),
        "Nothing waits for you: no agent asks a question, no chat failed, no automation failed."
    );
    quiet.chats_known = false;
    quiet.automations = Automations::NotRead;
    assert_eq!(
        quiet_attention(&quiet),
        "Nothing waits for you: no agent asks a question. Chats are still being read. \
         Automations are still being read."
    );
    quiet.automations = Automations::Unreadable("bad ledger".into());
    assert!(quiet_attention(&quiet).ends_with("Automations could not be read."));
}
