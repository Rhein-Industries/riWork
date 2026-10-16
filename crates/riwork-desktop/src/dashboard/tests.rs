use super::*;
use crate::{
    activity::ActivityCounts,
    form_input::{test_turn, test_window},
    global_overview::{
        AppOverview, BackendsOverview, ChatHostOverview, CliState, Freshness, ProviderOverview,
        Totals, Work, local_time,
    },
    sessions::HarnessKind,
    text_input::{self, InputEvent},
};
use gpui::{Render, TestAppContext, px, size};
use gpui_kit::test::TestWindowExt;
use std::path::PathBuf;

const NOW: u64 = 1_800_000_000;
/// Native's default interface scale: 11 pt design text drawn at 13 pt.
const NATIVE: f32 = 13.0 / 11.0;
const ALPHA: &str = "aaaaaaaa-0000-4000-8000-000000000001";
const BETA: &str = "bbbbbbbb-0000-4000-8000-000000000002";

fn session(target: Target, agent: &'static str, is_chat: bool, project: &str) -> SessionSummary {
    SessionSummary {
        target,
        agent,
        title: is_chat.then(|| "Fix the build".to_owned()),
        project_id: Some(project.into()),
        project_name: Some(if project == ALPHA { "alpha" } else { "beta" }.into()),
        branch: Some("feat/x".into()),
        orchestrator: None,
        is_chat,
    }
}

fn project(id: &str, name: &str) -> ProjectOverview {
    ProjectOverview {
        id: id.into(),
        name: name.into(),
        folder: None,
        root: PathBuf::from(format!("/work/{name}")),
        agents: ActivityCounts::default(),
        live_shells: 0,
        live_chats: 0,
        tasks: TaskCounts::default(),
        worktrees: 1,
        attention: 0,
        orchestrator: None,
        last_activity: None,
    }
}

/// Five projects in the overview's own order (the busiest first): alpha needs the user and
/// works, beta works with tasks in progress, Delta only runs sessions, gamma has tasks to
/// do and nothing else, quiet has nothing.
fn projects() -> Vec<ProjectOverview> {
    vec![
        ProjectOverview {
            folder: Some("Clients".into()),
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
            orchestrator: Some(ProjectOrchestrator::Chat {
                provider: Provider::Codex,
                state: ChatState::Running,
            }),
            last_activity: Some(NOW - 120),
            ..project(ALPHA, "alpha")
        },
        ProjectOverview {
            agents: ActivityCounts {
                working: 1,
                ..ActivityCounts::default()
            },
            live_shells: 1,
            tasks: TaskCounts {
                in_progress: 3,
                ..TaskCounts::default()
            },
            last_activity: Some(NOW - 30),
            ..project(BETA, "beta")
        },
        ProjectOverview {
            agents: ActivityCounts {
                unknown: 3,
                ..ActivityCounts::default()
            },
            live_shells: 3,
            ..project("delta", "Delta")
        },
        ProjectOverview {
            tasks: TaskCounts {
                todo: 2,
                ..TaskCounts::default()
            },
            last_activity: Some(NOW - 7200),
            ..project("gamma", "gamma")
        },
        project("quiet", "quiet"),
    ]
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
            projects: 5,
            active_projects: 4,
            live_shells: 6,
            live_chats: 1,
            agents: ActivityCounts {
                working: 2,
                waiting: 1,
                unknown: 3,
                ..ActivityCounts::default()
            },
            tasks: TaskCounts {
                todo: 3,
                in_progress: 4,
                done: 2,
            },
            attention: 1,
        },
        attention: vec![Attention {
            kind: AttentionKind::Waiting,
            target: chat.clone(),
            session: Some(session(chat, "Claude", true, ALPHA)),
            project_id: Some(ALPHA.into()),
            project_name: Some("alpha".into()),
            at: Some(NOW - 120),
        }],
        working: vec![Work {
            session: session(shell, "Codex", false, BETA),
            activity: AgentActivity::Working,
            since_unix: Some(NOW - 600),
            subagents: 2,
        }],
        finished: Vec::new(),
        projects: projects(),
        tasks_in_progress: vec![
            TaskOverview {
                id: "task-a".into(),
                title: "Ship it".into(),
                project_id: ALPHA.into(),
                project_name: "alpha".into(),
                branch: None,
                updated_at: NOW - 60,
            },
            TaskOverview {
                id: "task-b".into(),
                title: "Fix the login flow".into(),
                project_id: BETA.into(),
                project_name: "beta".into(),
                branch: Some("feat/login".into()),
                updated_at: NOW - 3600,
            },
        ],
        global,
        automations: Automations::Read {
            total: 2,
            paused: 1,
            next: None,
            items: vec![
                AutomationSummary {
                    id: "nightly".into(),
                    title: "Nightly build".into(),
                    project_id: Some(BETA.into()),
                    project_name: Some("beta".into()),
                    paused: false,
                    next_run: Some(NOW + 3600),
                    last_run: Some((Outcome::Failed, NOW - 7200)),
                },
                AutomationSummary {
                    id: "weekly".into(),
                    title: "Weekly report".into(),
                    project_id: None,
                    project_name: None,
                    paused: true,
                    next_run: None,
                    last_run: None,
                },
            ],
        },
        backends: Backends::Read(backends()),
        chats_known: true,
    }
}

/// The app current, the chat host from before the build on disk, Claude with an agent from
/// before its update and one that cannot be told, Codex current, Muse installed and unused.
fn backends() -> BackendsOverview {
    BackendsOverview {
        app: AppOverview {
            running: 1,
            windows: 3,
            started_at: Some(NOW - 600),
            build_at: Some(NOW - 7200),
            freshness: Some(Freshness::Current),
        },
        chat_host: ChatHostOverview::Running {
            pid: 4242,
            started_at: Some(NOW - 172_800),
            build_at: Some(NOW - 7200),
            freshness: Freshness::Stale,
        },
        providers: vec![
            ProviderOverview {
                provider: HarnessKind::Codex,
                cli: CliState::Found {
                    version: Some("0.47.0".into()),
                    replaced_at: Some(NOW - 86_400),
                },
                shells: 1,
                chats: Some(0),
                oldest_start: Some(NOW - 3600),
                stale: 0,
                unknown: 0,
            },
            ProviderOverview {
                provider: HarnessKind::Claude,
                cli: CliState::Found {
                    version: Some("2.1.301".into()),
                    replaced_at: Some(NOW - 1800),
                },
                shells: 2,
                chats: Some(1),
                oldest_start: Some(NOW - 9 * 86_400),
                stale: 1,
                unknown: 1,
            },
            ProviderOverview {
                provider: HarnessKind::Muse,
                cli: CliState::Found {
                    version: None,
                    replaced_at: Some(NOW - 600_000),
                },
                shells: 0,
                chats: Some(0),
                oldest_start: None,
                stale: 0,
                unknown: 0,
            },
        ],
    }
}

/// The dashboard as the global window holds it: its own search, filter, order and region,
/// which its clicks change and nothing else does.
struct Fixture {
    overview: Overview,
    sampled_at: Option<u64>,
    sample_error: Option<String>,
    search: Entity<InputState>,
    query: String,
    filter: DashboardFilter,
    sort: DashboardSort,
    region: Region,
    actions: Vec<DashboardAction>,
    _events: gpui::Subscription,
}

impl Fixture {
    fn new(
        overview: Overview,
        sampled_at: Option<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = text_input::single_line("", "Search", window, cx);
        let events = cx.subscribe_in(&search, window, |fixture, input, event, _, cx| {
            if matches!(event, InputEvent::Change) {
                fixture.query = input.read(cx).value().to_string();
                cx.notify();
            }
        });
        Self {
            overview,
            sampled_at,
            sample_error: None,
            search,
            query: String::new(),
            filter: DashboardFilter::default(),
            sort: DashboardSort::default(),
            region: Region::default(),
            actions: Vec::new(),
            _events: events,
        }
    }

    fn act(&mut self, action: DashboardAction, window: &mut Window, cx: &mut Context<Self>) {
        match &action {
            DashboardAction::Filter(filter) => self.filter = *filter,
            DashboardAction::Sort(sort) => self.sort = *sort,
            DashboardAction::Region(region) => self.region = *region,
            DashboardAction::ClearSearch => {
                crate::form_input::set_value(&self.search, String::new(), window, cx);
                self.query.clear();
            }
            _ => {}
        }
        self.actions.push(action);
        cx.notify();
    }
}

impl Render for Fixture {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dashboard = render(
            DashboardData {
                overview: &self.overview,
                now: NOW,
                sampled_at: self.sampled_at,
                sample_error: self.sample_error.as_deref(),
                chat_starting: false,
                runs: OrchestratorRuns::chat(Provider::Codex),
                width: window.viewport_size().width,
                search: Some(&self.search),
                query: &self.query,
                filter: self.filter,
                sort: self.sort,
                region: self.region,
            },
            Self::act,
            window,
            cx,
        );
        div().id("dashboard-pane").size_full().child(dashboard)
    }
}

/// The dashboard filling a window `width` by `height`.
fn mount(
    cx: &mut TestAppContext,
    overview: Overview,
    sampled_at: Option<u64>,
    width: f32,
    height: f32,
) -> (gpui::AnyWindowHandle, gpui::Entity<Fixture>) {
    let (window, fixture) = test_window(cx, move |window, cx| {
        Fixture::new(overview, sampled_at, window, cx)
    });
    cx.simulate_window_resize(window, size(px(width), px(height)));
    cx.run_until_parked();
    (window, fixture)
}

fn type_search(cx: &mut TestAppContext, window: gpui::AnyWindowHandle, text: &str) {
    test_turn(cx, window, |window, app| {
        window.click("dashboard-search-icon", app)
    });
    // One character a turn, so each Change is read before the next edit.
    for character in text.chars() {
        let character = character.to_string();
        test_turn(cx, window, |window, app| window.input(&character, app));
    }
}

/// The project rows on screen, top to bottom.
fn listed(window: &Window, overview: &Overview) -> Vec<String> {
    let mut rows: Vec<(gpui::Pixels, String)> = overview
        .projects
        .iter()
        .filter_map(|project| {
            window
                .try_find(format!("dashboard-project-{}", project.id))
                .map(|row| (row.bounds().top(), project.name.clone()))
        })
        .collect();
    rows.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    rows.into_iter().map(|(_, name)| name).collect()
}

fn names(projects: Vec<&ProjectOverview>) -> Vec<&str> {
    projects
        .into_iter()
        .map(|project| project.name.as_str())
        .collect()
}

// ---- Words ----------------------------------------------------------------------------------

#[test]
fn a_terminal_orchestrator_is_offered_as_itself_and_never_as_a_new_chat() {
    let terminal = GlobalOrchestrator::Terminal {
        shell_id: "shell-g".into(),
        activity: Some(AgentActivity::Working),
    };
    for runs in [
        OrchestratorRuns::Automatic,
        OrchestratorRuns::chat(Provider::Claude),
        OrchestratorRuns::Terminal,
    ] {
        let copy = chat_copy(&terminal, &runs, false);
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
    let copy = chat_copy(&chat, &OrchestratorRuns::Terminal, false);
    assert_eq!(
        (copy.action.as_str(), copy.starts, copy.enabled),
        ("Open global chat", false, true)
    );
    assert_eq!(copy.status, "Codex · Idle");
    assert_eq!(copy.provider, Some(Provider::Codex));

    let none = GlobalOrchestrator::None {
        exited_terminal: true,
    };
    let start = chat_copy(&none, &OrchestratorRuns::chat(Provider::Claude), false);
    assert_eq!(start.action, "Start global chat");
    assert!(start.starts);
    assert!(start.status.contains("terminal has exited"));
    assert!(start.note.unwrap().contains("Claude"));
    let opus = OrchestratorRuns::Chat {
        provider: Provider::Claude,
        model: Some("opus".into()),
    };
    assert!(
        chat_copy(&none, &opus, false)
            .note
            .unwrap()
            .contains("Claude chat on opus")
    );
    // Automatic names what it starts first and what it falls back to.
    let automatic = chat_copy(&none, &OrchestratorRuns::Automatic, false);
    assert_eq!(
        (automatic.action.as_str(), automatic.starts),
        ("Start global chat", true)
    );
    assert_eq!(automatic.provider, Some(Provider::Muse));
    let note = automatic.note.unwrap();
    assert!(note.contains("Muse chat on muse-wonder-full-1.0"), "{note}");
    assert!(
        note.contains("Codex chat on gpt-6.1-sol if Muse cannot start"),
        "{note}"
    );
    let terminal = chat_copy(&none, &OrchestratorRuns::Terminal, false);
    assert_eq!(terminal.action, "Start global orchestrator");

    // While one is being found or started, a second click does nothing.
    let busy = chat_copy(&none, &OrchestratorRuns::Terminal, true);
    assert!(!busy.enabled && !busy.starts);
}

#[test]
fn the_menu_summary_is_honest_before_and_after_the_first_read() {
    let overview = overview(GlobalOrchestrator::Unknown);
    assert_eq!(summary(&overview, false), "Reading sessions…");
    assert_eq!(
        summary(&overview, true),
        "2 working · 1 need you · 4 tasks in progress"
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

#[test]
fn a_project_row_reads_out_what_it_shows_and_says_what_is_unknown() {
    let colors = Palette::RIWORK;
    let projects = projects();
    let alpha = project_name_for_reader(&projects[0], true, NOW, colors);
    for part in [
        "Project alpha",
        "in Clients",
        "1 needs you",
        "1 waiting",
        "1 working",
        "3 live · 2 terminals · 1 chat",
        "tasks 2/4 done, 1 in progress · 1 to do",
        "project orchestrator: Codex chat, working",
        "last activity 2m ago",
    ] {
        assert!(alpha.contains(part), "{part:?} in {alpha:?}");
    }
    assert!(alpha.ends_with("; opens the project"));
    // Nothing recorded and nothing run: said as such, never as a zero time.
    let quiet = project_name_for_reader(&projects[4], true, NOW, colors);
    for part in [
        "quiet",
        "No live sessions",
        "no tasks",
        "no recorded activity",
    ] {
        assert!(quiet.contains(part), "{part:?} in {quiet:?}");
    }
    // Before the chats were read, the chat count is not claimed.
    let unread = project_name_for_reader(&projects[0], false, NOW, colors);
    assert!(
        unread.contains("2 terminals · chats not read yet"),
        "{unread}"
    );
    assert!(!unread.contains("live ·"), "{unread}");
}

#[test]
fn automations_say_when_they_run_next_and_how_the_last_run_went() {
    let Automations::Read { items, .. } = overview(GlobalOrchestrator::Unknown).automations else {
        unreachable!()
    };
    assert_eq!(
        automation_detail(&items[0], NOW),
        "beta · next in 1h · last run failed 2h ago"
    );
    assert_eq!(automation_detail(&items[1], NOW), "App · paused");
    let mut due = items[0].clone();
    due.next_run = Some(NOW - 5);
    due.last_run = None;
    assert_eq!(automation_detail(&due, NOW), "beta · due now");
}

#[test]
fn the_toolbar_says_how_old_the_figures_are_and_why_they_may_be() {
    let overview = overview(GlobalOrchestrator::Unknown);
    let data = |sampled_at: Option<u64>, error: Option<&'static str>| DashboardData {
        overview: &overview,
        now: NOW,
        sampled_at,
        sample_error: error,
        chat_starting: false,
        runs: OrchestratorRuns::Terminal,
        width: px(1100.0),
        search: None,
        query: "",
        filter: DashboardFilter::default(),
        sort: DashboardSort::default(),
        region: Region::default(),
    };
    assert_eq!(
        freshness(&data(None, None)),
        ("Reading sessions…".to_owned(), false)
    );
    assert_eq!(
        freshness(&data(None, Some("tmux: no server"))),
        (
            "Sessions could not be read: tmux: no server".to_owned(),
            true
        )
    );
    assert_eq!(
        freshness(&data(Some(NOW - 2), None)),
        ("Live".to_owned(), false)
    );
    assert_eq!(
        freshness(&data(Some(NOW - 600), None)),
        ("Last read 10m ago".to_owned(), true)
    );
    // A failed read after a good one keeps the figures, and says how old they are.
    assert_eq!(
        freshness(&data(Some(NOW - 60), Some("tmux: no server"))),
        (
            "Showing the read from 1m ago · tmux: no server".to_owned(),
            true
        )
    );
}

// ---- Search, filters, order ----------------------------------------------------------------

#[test]
fn a_search_needs_every_word_somewhere_and_ignores_case() {
    assert!(matches("", &["anything"]));
    assert!(matches("   ", &[]));
    assert!(matches("ALPHA cli", &["alpha", "Clients"]));
    assert!(matches("cli alp", &["alpha", "Clients"]), "any order");
    assert!(!matches("alpha beta", &["alpha", "Clients"]));
    assert!(matches("work/alp", &["/work/alpha"]));
}

#[test]
fn each_filter_lists_what_it_names_and_the_counts_follow_the_search() {
    let projects = projects();
    let list = |filter: DashboardFilter, query: &str| {
        names(visible_projects(
            &projects,
            query,
            filter,
            DashboardSort::Priority,
        ))
    };
    assert_eq!(
        list(DashboardFilter::Active, ""),
        ["alpha", "beta", "Delta", "gamma"],
        "the quiet project is not active"
    );
    assert_eq!(list(DashboardFilter::NeedsYou, ""), ["alpha"]);
    assert_eq!(list(DashboardFilter::Working, ""), ["alpha", "beta"]);
    assert_eq!(list(DashboardFilter::InProgress, ""), ["alpha", "beta"]);
    assert_eq!(
        list(DashboardFilter::All, ""),
        ["alpha", "beta", "Delta", "gamma", "quiet"]
    );
    // A project is found by its group and by its folder, not only its name.
    assert_eq!(list(DashboardFilter::All, "clients"), ["alpha"]);
    assert_eq!(list(DashboardFilter::All, "/work/qui"), ["quiet"]);
    assert_eq!(list(DashboardFilter::Active, "quiet"), Vec::<&str>::new());

    // "ta" is in beta and Delta only.
    assert_eq!(
        filter_counts(&projects, "ta"),
        [
            (DashboardFilter::Active, 2),
            (DashboardFilter::NeedsYou, 0),
            (DashboardFilter::Working, 1),
            (DashboardFilter::InProgress, 1),
            (DashboardFilter::All, 2),
        ]
    );
}

#[test]
fn every_order_keeps_the_busiest_first_among_equals() {
    let projects = projects();
    let order =
        |sort: DashboardSort| names(visible_projects(&projects, "", DashboardFilter::All, sort));
    assert_eq!(
        order(DashboardSort::Priority),
        ["alpha", "beta", "Delta", "gamma", "quiet"]
    );
    // The latest first; with no recorded time last, still busiest first.
    assert_eq!(
        order(DashboardSort::Recent),
        ["beta", "alpha", "gamma", "Delta", "quiet"]
    );
    // Most in progress, then most open.
    assert_eq!(
        order(DashboardSort::Tasks),
        ["beta", "alpha", "gamma", "Delta", "quiet"]
    );
    // Alpha's three (two terminals and a chat) tie with Delta's three: alpha is busier.
    assert_eq!(
        order(DashboardSort::Sessions),
        ["alpha", "Delta", "beta", "gamma", "quiet"]
    );
    // By name, whatever the case.
    assert_eq!(
        order(DashboardSort::Name),
        ["alpha", "beta", "Delta", "gamma", "quiet"]
    );
}

// ---- Layout ---------------------------------------------------------------------------------

#[test]
fn the_layout_follows_the_width_it_is_given() {
    // The colorful themes at the default size draw the design sizes as they are; Native's
    // default draws 11 pt text at 13 pt, so everything is 13/11 as wide.
    for (scale, face, width, arrangement, columns) in [
        (
            1.0,
            ui_text::Face::Menlo,
            640.0,
            Arrangement::Compact,
            Columns::Narrow,
        ),
        (
            1.0,
            ui_text::Face::Menlo,
            800.0,
            Arrangement::Compact,
            Columns::Medium,
        ),
        (
            1.0,
            ui_text::Face::Menlo,
            1100.0,
            Arrangement::Normal,
            Columns::Medium,
        ),
        (
            1.0,
            ui_text::Face::Menlo,
            1440.0,
            Arrangement::Normal,
            Columns::Full,
        ),
        (
            1.0,
            ui_text::Face::Menlo,
            1900.0,
            Arrangement::Wide,
            Columns::Full,
        ),
        (
            NATIVE,
            ui_text::Face::System,
            640.0,
            Arrangement::Compact,
            Columns::Narrow,
        ),
        (
            NATIVE,
            ui_text::Face::System,
            800.0,
            Arrangement::Compact,
            Columns::Narrow,
        ),
        (
            NATIVE,
            ui_text::Face::System,
            1100.0,
            Arrangement::Compact,
            Columns::Medium,
        ),
        (
            NATIVE,
            ui_text::Face::System,
            1440.0,
            Arrangement::Normal,
            Columns::Medium,
        ),
        // A 1900-wide Native window keeps both rails; the orchestrator folds away instead.
        (
            NATIVE,
            ui_text::Face::System,
            1900.0,
            Arrangement::Wide,
            Columns::Medium,
        ),
        (
            NATIVE,
            ui_text::Face::System,
            2560.0,
            Arrangement::Wide,
            Columns::Full,
        ),
    ] {
        let before = ui_text::set_for_tests(scale, face);
        let layout = Layout::of(px(width));
        ui_text::set_for_tests(before.0, before.1);
        assert_eq!(
            layout,
            Layout {
                arrangement,
                columns
            },
            "{width} at {scale}"
        );
    }
    // From the smallest window up, the table always has room for the columns it shows, and
    // a rail never squeezes it below the medium columns.
    let mut last = Layout::for_width(640.0);
    for width in (640..=2600).step_by(10) {
        let width = width as f32;
        let layout = Layout::for_width(width);
        let room = width - 2.0 * PAGE - layout.arrangement.rails() * (RAIL + GAP);
        assert!(
            room >= layout.columns.min_table(),
            "{width}: {layout:?} in {room}"
        );
        if layout.arrangement != Arrangement::Compact {
            assert_ne!(layout.columns, Columns::Narrow, "{width}");
        }
        // Growing the window never takes a rail away.
        assert!(
            layout.arrangement.rails() >= last.arrangement.rails(),
            "{width}"
        );
        last = layout;
    }
}

#[test]
fn task_counts_are_said_whole_in_the_room_a_cell_has() {
    let counts = TaskCounts {
        todo: 1,
        in_progress: 12,
        done: 71,
    };
    let lines = |width: f32| {
        let labels = task_labels(counts, width).unwrap();
        (
            labels.done,
            labels.bar_beside,
            labels
                .open
                .into_iter()
                .map(|(line, _)| line)
                .collect::<Vec<_>>(),
        )
    };
    // The full and medium columns, and the narrow one.
    assert_eq!(
        lines(170.0),
        (
            "71/84 done".into(),
            true,
            vec!["12 in progress · 1 to do".into()]
        )
    );
    assert_eq!(
        lines(130.0),
        ("71/84 done".into(), true, vec!["12 doing · 1 to do".into()])
    );
    assert_eq!(
        lines(120.0),
        ("71/84 done".into(), true, vec!["12 doing · 1 to do".into()])
    );
    // Narrower still: one count to a line, then the short word, and the bar under the
    // done count.
    assert_eq!(
        lines(90.0),
        (
            "71/84 done".into(),
            false,
            vec!["12 in progress".into(), "1 to do".into()]
        )
    );
    assert_eq!(
        lines(80.0),
        (
            "71/84 done".into(),
            false,
            vec!["12 doing".into(), "1 to do".into()]
        )
    );
    assert_eq!(task_labels(TaskCounts::default(), 170.0), None);
    assert_eq!(
        task_labels(
            TaskCounts {
                done: 3,
                ..TaskCounts::default()
            },
            120.0
        )
        .unwrap()
        .open,
        [("all done".to_owned(), false)]
    );
    // At any width a column could have, every count is there, whole: no ellipsis, nothing
    // dropped, the in-progress line marked as such.
    let large = TaskCounts {
        todo: 15,
        in_progress: 120,
        done: 1234,
    };
    for width in (100..=220).step_by(2) {
        let labels = task_labels(large, width as f32).unwrap();
        let all = format!(
            "{} {}",
            labels.done,
            labels
                .open
                .iter()
                .map(|(line, _)| line.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        );
        for count in ["1234/1369", "120 ", "15 to do"] {
            assert!(all.contains(count), "{width}: {count:?} in {all:?}");
        }
        assert!(!all.contains('…'), "{width}: {all}");
        assert!(labels.open[0].1 && labels.open[0].0.starts_with("120 "));
    }
}

// ---- Drawn ----------------------------------------------------------------------------------

#[gpui::test]
fn rows_open_their_exact_target_and_drawing_asks_for_nothing(cx: &mut TestAppContext) {
    let (window, fixture) = mount(
        cx,
        overview(GlobalOrchestrator::None {
            exited_terminal: false,
        }),
        Some(NOW - 1),
        1440.0,
        900.0,
    );
    // Drawing the dashboard asked for nothing, a chat least of all.
    test_turn(cx, window, |_, app| {
        assert!(fixture.read(app).actions.is_empty());
    });
    for id in [
        "dashboard-working-0".to_owned(),
        "dashboard-attention-0".to_owned(),
        format!("dashboard-project-{ALPHA}"),
        format!("dashboard-project-{ALPHA}-tasks"),
        format!("dashboard-project-{BETA}-sessions"),
        "dashboard-task-task-b".to_owned(),
        "dashboard-automation-nightly".to_owned(),
        "dashboard-automation-weekly".to_owned(),
        "dashboard-global-chat".to_owned(),
    ] {
        test_turn(cx, window, |window, app| window.click(id, app));
    }
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
                DashboardAction::Open(Target::Project { id: ALPHA.into() }),
                DashboardAction::ProjectPanel {
                    project_id: ALPHA.into(),
                    panel: PanelKind::Tasks
                },
                DashboardAction::ProjectPanel {
                    project_id: BETA.into(),
                    panel: PanelKind::Shells
                },
                DashboardAction::Open(Target::Task {
                    id: "task-b".into()
                }),
                DashboardAction::Open(Target::Automations {
                    project_id: Some(BETA.into())
                }),
                DashboardAction::Open(Target::Automations { project_id: None }),
                DashboardAction::GlobalChat,
            ]
        );
    });
}

#[gpui::test]
fn typing_narrows_every_region_and_clearing_brings_everything_back(cx: &mut TestAppContext) {
    let overview = overview(GlobalOrchestrator::Unknown);
    let (window, fixture) = mount(cx, overview.clone(), Some(NOW - 1), 1440.0, 900.0);
    type_search(cx, window, "beta");
    let shown = overview.clone();
    let typed = fixture.clone();
    test_turn(cx, window, move |window, app| {
        assert_eq!(typed.read(app).query, "beta");
        assert_eq!(listed(window, &shown), ["beta"]);
        // Its task, its working session and its automation stay; alpha's are gone, and so
        // is the app automation.
        assert!(window.try_find("dashboard-task-task-b").is_some());
        assert!(window.try_find("dashboard-task-task-a").is_none());
        assert!(window.try_find("dashboard-working-0").is_some());
        assert!(window.try_find("dashboard-attention-0").is_none());
        assert!(window.try_find("dashboard-automation-nightly").is_some());
        assert!(window.try_find("dashboard-automation-weekly").is_none());
        // The filters count what the search left.
        assert_eq!(
            window.find("dashboard-filter-active").label(),
            Some("Active 1")
        );
        window.click("dashboard-search-clear", app);
    });
    test_turn(cx, window, move |window, app| {
        let fixture = fixture.read(app);
        assert_eq!(fixture.query, "");
        assert_eq!(fixture.actions, [DashboardAction::ClearSearch]);
        assert_eq!(
            listed(window, &overview),
            ["alpha", "beta", "Delta", "gamma"]
        );
        assert!(window.try_find("dashboard-search-clear").is_none());
        assert!(window.try_find("dashboard-attention-0").is_some());
        // The field keeps the keyboard for the next search.
        assert!(
            fixture
                .search
                .read(app)
                .focus_handle(app)
                .is_focused(window)
        );
    });
}

#[gpui::test]
fn filters_and_orders_change_what_the_table_lists_and_only_that(cx: &mut TestAppContext) {
    let overview = overview(GlobalOrchestrator::Unknown);
    let (window, fixture) = mount(cx, overview.clone(), Some(NOW - 1), 1440.0, 900.0);
    let shown = overview.clone();
    test_turn(cx, window, move |window, app| {
        assert_eq!(listed(window, &shown), ["alpha", "beta", "Delta", "gamma"]);
        window.click("dashboard-filter-needs_you", app);
    });
    let shown = overview.clone();
    test_turn(cx, window, move |window, app| {
        assert_eq!(listed(window, &shown), ["alpha"]);
        // The rails do not filter: what is waiting or under way stays in view.
        assert!(window.try_find("dashboard-working-0").is_some());
        assert!(window.try_find("dashboard-task-task-b").is_some());
        window.click("dashboard-filter-all", app);
    });
    test_turn(cx, window, |window, app| {
        window.click("dashboard-sort-recent", app)
    });
    let shown = overview.clone();
    test_turn(cx, window, move |window, app| {
        assert_eq!(
            listed(window, &shown),
            ["beta", "alpha", "gamma", "Delta", "quiet"]
        );
        window.click("dashboard-sort-name", app);
    });
    test_turn(cx, window, move |window, app| {
        assert_eq!(
            listed(window, &overview),
            ["alpha", "beta", "Delta", "gamma", "quiet"]
        );
        assert_eq!(
            fixture.read(app).actions,
            [
                DashboardAction::Filter(DashboardFilter::NeedsYou),
                DashboardAction::Filter(DashboardFilter::All),
                DashboardAction::Sort(DashboardSort::Recent),
                DashboardAction::Sort(DashboardSort::Name),
            ]
        );
    });
}

#[gpui::test]
fn an_empty_table_says_why_and_leads_to_what_is_hidden(cx: &mut TestAppContext) {
    let overview = overview(GlobalOrchestrator::Unknown);
    let (window, fixture) = mount(cx, overview.clone(), Some(NOW - 1), 1440.0, 900.0);
    // The quiet project matches, but Active leaves it out: the table says so and offers it.
    type_search(cx, window, "quiet");
    test_turn(cx, window, |window, app| {
        assert!(window.try_find("dashboard-projects-empty").is_some());
        assert_eq!(
            window.find("dashboard-show-all").label(),
            Some("Show all 1 project")
        );
        window.click("dashboard-show-all", app);
    });
    test_turn(cx, window, move |window, app| {
        assert_eq!(fixture.read(app).filter, DashboardFilter::All);
        assert_eq!(listed(window, &overview), ["quiet"]);
        assert!(window.try_find("dashboard-projects-empty").is_none());
    });
}

/// Forty more active projects, so the table is far taller than any window.
fn crowded() -> Overview {
    let mut overview = overview(GlobalOrchestrator::Chat {
        id: "chat-g".into(),
        provider: Provider::Codex,
        state: ChatState::Stopped,
    });
    // Large task counts: the longest a tasks cell has to say.
    overview.projects.push(ProjectOverview {
        live_shells: 1,
        tasks: TaskCounts {
            todo: 15,
            in_progress: 120,
            done: 1234,
        },
        ..project("counts", "counts")
    });
    for index in 0..40 {
        overview.projects.push(ProjectOverview {
            folder: Some("A group with a rather long name".into()),
            live_shells: 1,
            tasks: TaskCounts {
                todo: 3,
                in_progress: 1,
                done: 40,
            },
            worktrees: 4,
            last_activity: Some(NOW - 86_400),
            orchestrator: Some(ProjectOrchestrator::Terminal {
                activity: Some(AgentActivity::Waiting),
            }),
            ..project(
                &format!("extra-{index}"),
                &format!("a-project-with-a-long-name-{index}"),
            )
        });
    }
    overview
}

#[gpui::test]
fn at_every_size_nothing_crosses_the_edge_and_tasks_are_never_buried(cx: &mut TestAppContext) {
    const HEIGHT: f32 = 800.0;
    let cases = [
        (640.0, 1.0, ui_text::Face::Menlo, Arrangement::Compact),
        (800.0, 1.0, ui_text::Face::System, Arrangement::Compact),
        (800.0, 1.3, ui_text::Face::System, Arrangement::Compact),
        (1100.0, 1.0, ui_text::Face::Menlo, Arrangement::Normal),
        (1100.0, 1.0, ui_text::Face::System, Arrangement::Normal),
        (1440.0, 1.0, ui_text::Face::System, Arrangement::Normal),
        (1900.0, 1.0, ui_text::Face::System, Arrangement::Wide),
        (1900.0, 1.0, ui_text::Face::Menlo, Arrangement::Wide),
        // Native at its default text size.
        (640.0, NATIVE, ui_text::Face::System, Arrangement::Compact),
        (800.0, NATIVE, ui_text::Face::System, Arrangement::Compact),
        (1100.0, NATIVE, ui_text::Face::System, Arrangement::Compact),
        (1440.0, NATIVE, ui_text::Face::System, Arrangement::Normal),
        (1900.0, NATIVE, ui_text::Face::System, Arrangement::Wide),
    ];
    for (width, scale, face, expected) in cases {
        let before = ui_text::set_for_tests(scale, face);
        let layout = Layout::of(px(width));
        assert_eq!(layout.arrangement, expected, "{width} at {scale}");
        let overview = crowded();
        let last = format!("dashboard-project-{}", overview.projects.last().unwrap().id);
        let (window, _) = mount(cx, overview, Some(NOW - 1), width, HEIGHT);
        let inside = move |window: &Window, id: &str| {
            let bounds = window.find(id.to_owned()).bounds();
            assert!(
                bounds.left() >= px(0.0) && bounds.right() <= px(width + 0.5),
                "{width}/{scale}/{face:?}: {id} spans {bounds:?}"
            );
            bounds
        };
        let in_view = move |window: &Window, id: &str| {
            let bounds = inside(window, id);
            assert!(
                bounds.bottom() <= px(HEIGHT),
                "{width}/{scale}/{face:?}: {id} needs scrolling: {bounds:?}"
            );
        };
        let last_row = last.clone();
        test_turn(cx, window, move |window, _| {
            for id in [
                "dashboard-search-icon",
                "dashboard-refresh",
                "dashboard-status",
                "dashboard-filter-in_progress",
                "dashboard-sort-name",
                "dashboard-table-head",
                "dashboard-project-extra-0-sessions",
            ] {
                inside(window, id);
            }
            // The last of 45 rows is in the table, scrolled out of sight but never wider.
            inside(window, &last_row);
            // Within a row, the cells end before its buttons begin, and the project's own
            // column keeps room to be read.
            for project in ["extra-0", "extra-39", ALPHA, "counts"] {
                let row =
                    |part: &str| inside(window, &format!("dashboard-project-{project}{part}"));
                let (name, last, tasks) = (row("-name"), row("-last"), row("-tasks"));
                // Every task count is drawn whole inside its cell: done of all, then each
                // open count, whatever the width.
                let cell = row("-tasks-cell");
                let mut labels = vec![row("-tasks-done"), row("-tasks-open-0")];
                if let Some(todo) =
                    window.try_find(format!("dashboard-project-{project}-tasks-open-1"))
                {
                    labels.push(todo.bounds());
                }
                for label in labels {
                    assert!(
                        label.left() >= cell.left() && label.right() <= cell.right() + px(0.5),
                        "{width}/{scale}/{face:?}: {project}'s task count {label:?} leaves {cell:?}"
                    );
                }
                assert!(
                    last.right() <= tasks.left(),
                    "{width}/{scale}/{face:?}: {project}'s cells run into its buttons: {last:?} {tasks:?}"
                );
                assert!(
                    name.size.width >= px(100.0),
                    "{width}/{scale}/{face:?}: {project}'s name has {:?}",
                    name.size.width
                );
            }
            let projects = inside(window, "dashboard-projects");
            match layout.arrangement {
                Arrangement::Compact => {
                    assert!(window.try_find("dashboard-rail").is_none());
                    assert!(window.try_find("dashboard-rail-work").is_none());
                    in_view(window, "dashboard-region-work");
                    in_view(window, "dashboard-global-chat");
                }
                Arrangement::Normal => {
                    let rail = inside(window, "dashboard-rail");
                    assert!(projects.right() <= rail.left(), "{width}");
                    // However long the project list, the tasks and the automations are in
                    // view beside it.
                    for id in [
                        "dashboard-attention-0",
                        "dashboard-working-0",
                        "dashboard-task-task-b",
                        "dashboard-automation-nightly",
                        "dashboard-global-chat",
                    ] {
                        in_view(window, id);
                    }
                    // The backends come last, below them, and stay inside the rail.
                    for id in ["dashboard-backend-chat-host", "dashboard-backend-claude"] {
                        let row = inside(window, id);
                        assert!(row.left() >= rail.left() && row.right() <= rail.right() + px(0.5));
                    }
                }
                Arrangement::Wide => {
                    let activity = inside(window, "dashboard-rail-activity");
                    let work = inside(window, "dashboard-rail-work");
                    assert!(projects.right() <= activity.left(), "{width}");
                    assert!(activity.right() <= work.left(), "{width}");
                    for id in [
                        "dashboard-attention-0",
                        "dashboard-task-task-b",
                        "dashboard-automation-weekly",
                        "dashboard-global-chat",
                    ] {
                        in_view(window, id);
                    }
                    for id in ["dashboard-backend-chat-host", "dashboard-backend-claude"] {
                        let row = inside(window, id);
                        assert!(row.left() >= work.left() && row.right() <= work.right() + px(0.5));
                    }
                }
            }
        });
        if layout.arrangement == Arrangement::Compact {
            // One click away, each region fills the page, in view.
            test_turn(cx, window, |window, app| {
                window.click("dashboard-region-work", app)
            });
            test_turn(cx, window, move |window, app| {
                assert!(window.try_find(last.clone()).is_none());
                in_view(window, "dashboard-task-task-b");
                in_view(window, "dashboard-automation-nightly");
                inside(window, "dashboard-backend-chat-host");
                inside(window, "dashboard-backend-claude");
                window.click("dashboard-region-activity", app);
            });
            test_turn(cx, window, move |window, _| {
                in_view(window, "dashboard-attention-0");
                in_view(window, "dashboard-working-0");
                assert!(window.try_find("dashboard-task-task-b").is_none());
            });
        }
        ui_text::set_for_tests(before.0, before.1);
    }
}

#[gpui::test]
fn before_the_first_read_it_says_so_and_can_still_refresh(cx: &mut TestAppContext) {
    let (window, fixture) = mount(
        cx,
        overview(GlobalOrchestrator::Unknown),
        None,
        1100.0,
        800.0,
    );
    test_turn(cx, window, |window, app| {
        // No figure and no row is drawn from a read that did not happen.
        assert!(window.try_find("dashboard-summary").is_none());
        assert!(window.try_find("dashboard-projects").is_none());
        assert!(window.try_find("dashboard-working-0").is_none());
        assert!(window.try_find("dashboard-search-icon").is_some());
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
    // A failed first read still shows nothing counted; a later good read shows it all, and
    // keeps it while a read after that fails.
    test_turn(cx, window, |_, app| {
        fixture.update(app, |fixture, cx| {
            fixture.sample_error = Some("tmux: server exited unexpectedly".into());
            cx.notify();
        })
    });
    test_turn(cx, window, |window, _| {
        assert!(window.try_find("dashboard-projects").is_none());
        assert!(window.try_find("dashboard-refresh").is_some());
    });
    test_turn(cx, window, |_, app| {
        fixture.update(app, |fixture, cx| {
            fixture.sampled_at = Some(NOW - 600);
            cx.notify();
        })
    });
    test_turn(cx, window, |window, _| {
        assert!(window.try_find("dashboard-projects").is_some());
        assert!(window.try_find("dashboard-summary").is_some());
        assert!(window.try_find("dashboard-working-0").is_some());
    });
}

// ---- Backends -------------------------------------------------------------------------------

/// The accessible text of backend row `key`, which is what the row shows.
fn backend_label(window: &Window, key: &str) -> String {
    window
        .find(format!("dashboard-backend-{key}"))
        .label()
        .unwrap_or_default()
        .to_owned()
}

#[gpui::test]
fn the_backends_say_what_runs_and_what_a_restart_would_apply_and_offer_no_click(
    cx: &mut TestAppContext,
) {
    let (window, fixture) = mount(
        cx,
        overview(GlobalOrchestrator::None {
            exited_terminal: false,
        }),
        Some(NOW - 1),
        1900.0,
        1000.0,
    );
    let at = |at: u64| local_time(at, NOW);
    test_turn(cx, window, move |window, _| {
        // In the work rail, after the automations.
        let rail = window.find("dashboard-rail-work").bounds();
        let section = window.find("dashboard-backends").bounds();
        assert!(section.left() >= rail.left() && section.right() <= rail.right());
        assert!(window.find("dashboard-automations").bounds().bottom() <= section.top());
        assert_eq!(
            backend_label(window, "app"),
            format!(
                "RiWork, current: running since {} · 3 windows · runs the {} build",
                at(NOW - 600),
                at(NOW - 7200)
            )
        );
        assert_eq!(
            backend_label(window, "chat-host"),
            format!(
                "Chat host, restart to apply: running since {} · pid 4242 · restart to apply the {} build",
                at(NOW - 172_800),
                at(NOW - 7200)
            )
        );
        assert_eq!(
            backend_label(window, "claude"),
            format!(
                "Claude, restart to apply: 2.1.301 on disk · 3 running, oldest {} · 1 predates the installed binary · 1 unknown",
                at(NOW - 9 * 86_400)
            )
        );
        assert_eq!(
            backend_label(window, "codex"),
            format!(
                "Codex, current: 0.47.0 on disk · 1 running, oldest {}",
                at(NOW - 3600)
            )
        );
        assert_eq!(
            backend_label(window, "muse"),
            format!(
                "Muse, not running: version unknown, installed {} · none running",
                at(NOW - 600_000)
            )
        );
        // Grok was neither found nor run: no row.
        assert!(window.try_find("dashboard-backend-grok").is_none());
    });
    // The rows are facts: clicking one asks the window for nothing, a restart least of all.
    for key in ["app", "chat-host", "claude", "muse"] {
        test_turn(cx, window, |window, app| {
            window.click(format!("dashboard-backend-{key}"), app)
        });
    }
    test_turn(cx, window, |_, app| {
        assert!(fixture.read(app).actions.is_empty());
    });
    // The search narrows them like every other list.
    type_search(cx, window, "claude");
    test_turn(cx, window, |window, _| {
        assert!(window.try_find("dashboard-backend-claude").is_some());
        assert!(window.try_find("dashboard-backend-codex").is_none());
        assert!(window.try_find("dashboard-backend-app").is_none());
    });
}

#[gpui::test]
fn a_stopped_chat_host_and_providers_never_used_are_said_as_such(cx: &mut TestAppContext) {
    let mut quiet = overview(GlobalOrchestrator::None {
        exited_terminal: false,
    });
    quiet.backends = Backends::Read(BackendsOverview {
        chat_host: ChatHostOverview::Stopped,
        providers: Vec::new(),
        ..backends()
    });
    let (window, _) = mount(cx, quiet, Some(NOW - 1), 1440.0, 2000.0);
    test_turn(cx, window, |window, _| {
        assert_eq!(
            backend_label(window, "chat-host"),
            "Chat host, not running: not running"
        );
        for provider in ["codex", "claude", "muse", "grok"] {
            assert!(
                window
                    .try_find(format!("dashboard-backend-{provider}"))
                    .is_none()
            );
        }
        // In the one rail, last of all.
        let rail = window.find("dashboard-rail").bounds();
        let section = window.find("dashboard-backends").bounds();
        assert!(section.left() >= rail.left() && section.right() <= rail.right());
        assert!(window.find("dashboard-automations").bounds().bottom() <= section.top());
    });
    // Before the backends were read, no row is claimed.
    let mut unread = overview(GlobalOrchestrator::None {
        exited_terminal: false,
    });
    unread.backends = Backends::NotRead;
    let (window, _) = mount(cx, unread, Some(NOW - 1), 1440.0, 2000.0);
    test_turn(cx, window, |window, _| {
        assert!(window.try_find("dashboard-backends").is_some());
        assert!(window.try_find("dashboard-backend-app").is_none());
        assert!(window.try_find("dashboard-backend-chat-host").is_none());
    });
}

#[gpui::test]
fn a_compact_dashboard_shows_the_backends_with_the_work(cx: &mut TestAppContext) {
    let (window, _) = mount(
        cx,
        overview(GlobalOrchestrator::None {
            exited_terminal: false,
        }),
        Some(NOW - 1),
        640.0,
        2000.0,
    );
    test_turn(cx, window, |window, app| {
        assert!(window.try_find("dashboard-backends").is_none());
        window.click("dashboard-region-work", app)
    });
    test_turn(cx, window, |window, _| {
        let section = window.find("dashboard-backends").bounds();
        assert!(section.left() >= px(0.0) && section.right() <= px(640.5));
        assert!(window.find("dashboard-automations").bounds().bottom() <= section.top());
        let row = window.find("dashboard-backend-chat-host").bounds();
        assert!(row.right() <= px(640.5), "{row:?}");
    });
}
