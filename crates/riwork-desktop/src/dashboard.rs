//! The global dashboard: what every project on this Mac is doing, what waits for the user,
//! and how far the tasks have come, with each row opening the exact session, task, project
//! or automation it names. It draws `global_overview::Overview`, which the window builds
//! from the sessions, activity and chats it samples anyway; drawing, searching, filtering,
//! sorting or refreshing it never starts an agent, a chat or a chat host. Only the global
//! chat control does, when clicked.
//!
//! The page is a toolbar (the search, the figures in a line, how fresh they are), then the
//! projects as a table beside rails for what needs the user, the work under way, the tasks
//! in progress and the automations. The search narrows every list; the filters and the
//! order apply to the projects. How much sits side by side follows the width the window
//! gives the dashboard (`Layout`): two rails when wide, one at an ordinary size, and one
//! region at a time, chosen above it, when compact. Columns that do not fit fold into the
//! project's own lines rather than being cut off.

use std::cmp::Reverse;

use gpui::{
    AnyElement, Context, Div, Entity, Focusable, FontWeight, IntoElement, MouseButton, Pixels,
    Window, div, prelude::*, rgb,
};
use gpui_kit::TestSupportExt;
use riwork_core::{
    codex_accounts::display_home,
    global_window::{DashboardFilter, DashboardSort},
};

use crate::{
    activity::AgentActivity,
    behavior_controls as behavior,
    chat::model::{ChatState, OrchestratorScope, Provider},
    controls,
    global_overview::{
        Attention, AttentionKind, AutomationSummary, Automations, GlobalOrchestrator, Overview,
        ProjectOrchestrator, ProjectOverview, SessionSummary, Target, TaskCounts, TaskOverview,
        Work, short_duration,
    },
    icons,
    layouts::PanelKind,
    schedules::Outcome,
    session_catalog,
    settings::OrchestratorRuns,
    text_input::InputState,
    theme::{self, Palette},
    tooltip::{self, Look},
    ui_text,
};

/// Rows a rail's list shows before it says how many more there are.
const LIST_LIMIT: usize = 12;
/// A session sample older than this is called out as stale.
const STALE_AFTER_SECS: u64 = 30;

// The layout's measures, in design pixels (`ui_text::space` grows them with the text).
/// The page's inset from the window's edges.
const PAGE: f32 = 10.0;
/// A rail's width, and the gap beside it.
const RAIL: f32 = 340.0;
const GAP: f32 = 12.0;
/// The status mark at the start of a project row.
const MARK: f32 = 10.0;
const CELL_GAP: f32 = 10.0;
const ROW_PAD: f32 = 8.0;
/// The project column is never planned narrower than this.
const PROJECT_MIN: f32 = 180.0;

/// What a click on the dashboard asks the window to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DashboardAction {
    Open(Target),
    /// Open one of a project's panels (Tasks, Sessions) in that project.
    ProjectPanel {
        project_id: String,
        panel: PanelKind,
    },
    /// Open the global orchestrator: the one there is, or, when there is none, start one as
    /// the settings say. Only an explicit click sends this.
    GlobalChat,
    /// Read the sessions, chats and automations again now.
    Refresh,
    /// List these projects. It is the dashboard's own: no project changes.
    Filter(DashboardFilter),
    Sort(DashboardSort),
    /// Show this region of a compact dashboard.
    Region(Region),
    /// Empty the search.
    ClearSearch,
}

/// What a compact dashboard shows, one at a time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Region {
    #[default]
    Projects,
    /// What needs the user, what works, what finished.
    Activity,
    /// Tasks in progress and automations.
    Work,
}

/// Everything the dashboard draws, from the window's latest samples and its own state.
pub struct DashboardData<'a> {
    pub overview: &'a Overview,
    pub now: u64,
    /// When the sessions were last read; `None` before the first read finished.
    pub sampled_at: Option<u64>,
    /// Why the last read of the sessions failed, if it did.
    pub sample_error: Option<&'a str>,
    /// The global orchestrator is being found or started.
    pub chat_starting: bool,
    /// How a new global orchestrator would run.
    pub runs: OrchestratorRuns,
    /// The width the dashboard is drawn in.
    pub width: Pixels,
    /// The search field, which the window keeps; its text is `query`.
    pub search: Option<&'a Entity<InputState>>,
    pub query: &'a str,
    pub filter: DashboardFilter,
    pub sort: DashboardSort,
    pub region: Region,
}

// ---- Layout --------------------------------------------------------------------------------

/// How much of the dashboard sits side by side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Arrangement {
    /// The projects, then a rail for activity and one for tasks and automations.
    Wide,
    /// The projects and one rail.
    Normal,
    /// One region at a time.
    Compact,
}

impl Arrangement {
    fn rails(self) -> f32 {
        match self {
            Self::Wide => 2.0,
            Self::Normal => 1.0,
            Self::Compact => 0.0,
        }
    }
}

/// Which columns the projects table has. What a narrower table drops moves into the
/// project's own lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Columns {
    /// Project, activity, tasks, orchestrator, last activity.
    Full,
    /// The orchestrator moves under the project's name.
    Medium,
    /// So do the activity and the sessions.
    Narrow,
}

/// The fixed columns' widths; the project column takes the rest.
struct Widths {
    activity: Option<f32>,
    tasks: f32,
    orchestrator: Option<f32>,
    last: f32,
    actions: f32,
}

impl Columns {
    fn widths(self) -> Widths {
        match self {
            Self::Full => Widths {
                activity: Some(180.0),
                tasks: 170.0,
                orchestrator: Some(150.0),
                last: 76.0,
                actions: 124.0,
            },
            Self::Medium => Widths {
                activity: Some(150.0),
                tasks: 130.0,
                orchestrator: None,
                last: 66.0,
                actions: 120.0,
            },
            Self::Narrow => Widths {
                activity: None,
                tasks: 110.0,
                orchestrator: None,
                last: 64.0,
                actions: 120.0,
            },
        }
    }

    /// The narrowest table these columns fit, with `PROJECT_MIN` left for the project.
    fn min_table(self) -> f32 {
        let widths = self.widths();
        let fixed = [
            Some(MARK),
            widths.activity,
            Some(widths.tasks),
            widths.orchestrator,
            Some(widths.last),
            Some(widths.actions),
        ];
        let cells = fixed.iter().flatten().count() + 1;
        fixed.iter().flatten().sum::<f32>()
            + PROJECT_MIN
            + (cells - 1) as f32 * CELL_GAP
            + 2.0 * ROW_PAD
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Layout {
    pub arrangement: Arrangement,
    pub columns: Columns,
}

impl Layout {
    /// The layout for a dashboard `width` design pixels wide: the rails a table with room
    /// for its columns leaves space for, then the most columns that fit beside them.
    pub(crate) fn for_width(width: f32) -> Self {
        let table =
            |arrangement: Arrangement| width - 2.0 * PAGE - arrangement.rails() * (RAIL + GAP);
        let arrangement = if table(Arrangement::Wide) >= Columns::Full.min_table() {
            Arrangement::Wide
        } else if table(Arrangement::Normal) >= Columns::Medium.min_table() {
            Arrangement::Normal
        } else {
            Arrangement::Compact
        };
        let room = table(arrangement);
        let columns = [Columns::Full, Columns::Medium]
            .into_iter()
            .find(|columns| room >= columns.min_table())
            .unwrap_or(Columns::Narrow);
        Self {
            arrangement,
            columns,
        }
    }

    fn of(width: Pixels) -> Self {
        Self::for_width(width.as_f32() / ui_text::scale().max(1.0))
    }
}

// ---- Search, filters and order ------------------------------------------------------------

/// Every word of the query is in one of `fields`, ignoring case. An empty query matches.
pub(crate) fn matches(query: &str, fields: &[&str]) -> bool {
    let fields: Vec<String> = fields.iter().map(|field| field.to_lowercase()).collect();
    query.split_whitespace().all(|word| {
        let word = word.to_lowercase();
        fields.iter().any(|field| field.contains(&word))
    })
}

fn project_path(project: &ProjectOverview) -> String {
    display_home(&project.root)
}

fn project_matches(project: &ProjectOverview, query: &str) -> bool {
    matches(
        query,
        &[
            &project.name,
            project.folder.as_deref().unwrap_or(""),
            &project_path(project),
        ],
    )
}

/// Whether `filter` lists `project`.
pub(crate) fn passes(project: &ProjectOverview, filter: DashboardFilter) -> bool {
    match filter {
        DashboardFilter::Active => !project.is_quiet(),
        DashboardFilter::NeedsYou => project.attention > 0,
        DashboardFilter::Working => project.agents.working > 0,
        DashboardFilter::InProgress => project.tasks.in_progress > 0,
        DashboardFilter::All => true,
    }
}

/// The projects the table lists, in its order. `projects` come in the overview's order,
/// the busiest first, which every other order keeps among equals.
pub(crate) fn visible_projects<'a>(
    projects: &'a [ProjectOverview],
    query: &str,
    filter: DashboardFilter,
    sort: DashboardSort,
) -> Vec<&'a ProjectOverview> {
    let mut shown: Vec<&ProjectOverview> = projects
        .iter()
        .filter(|project| passes(project, filter) && project_matches(project, query))
        .collect();
    match sort {
        DashboardSort::Priority => {}
        // Unknown times last: nothing recorded is not "long ago".
        DashboardSort::Recent => shown.sort_by_key(|project| {
            (
                project.last_activity.is_none(),
                Reverse(project.last_activity),
            )
        }),
        DashboardSort::Tasks => shown.sort_by_key(|project| {
            (
                Reverse(project.tasks.in_progress),
                Reverse(project.tasks.open()),
            )
        }),
        DashboardSort::Sessions => {
            shown.sort_by_key(|project| Reverse(project.live_shells + project.live_chats))
        }
        DashboardSort::Name => shown.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.id.cmp(&b.id))
        }),
    }
    shown
}

/// How many projects each filter lists under the query.
pub(crate) fn filter_counts(
    projects: &[ProjectOverview],
    query: &str,
) -> Vec<(DashboardFilter, usize)> {
    let found: Vec<&ProjectOverview> = projects
        .iter()
        .filter(|project| project_matches(project, query))
        .collect();
    DashboardFilter::ALL
        .into_iter()
        .map(|filter| {
            (
                filter,
                found
                    .iter()
                    .filter(|project| passes(project, filter))
                    .count(),
            )
        })
        .collect()
}

fn filter_label(filter: DashboardFilter) -> &'static str {
    match filter {
        DashboardFilter::Active => "Active",
        DashboardFilter::NeedsYou => "Needs you",
        DashboardFilter::Working => "Working",
        DashboardFilter::InProgress => "In progress",
        DashboardFilter::All => "All",
    }
}

fn sort_label(sort: DashboardSort) -> &'static str {
    match sort {
        DashboardSort::Priority => "Priority",
        DashboardSort::Recent => "Recent",
        DashboardSort::Tasks => "Tasks",
        DashboardSort::Sessions => "Sessions",
        DashboardSort::Name => "Name",
    }
}

/// What a filter that lists nothing says, without a search.
fn nothing_listed(filter: DashboardFilter) -> &'static str {
    match filter {
        DashboardFilter::Active => "No project has a live session, a live chat or an open task.",
        DashboardFilter::NeedsYou => "No project needs you right now.",
        DashboardFilter::Working => "No agent is working in any project.",
        DashboardFilter::InProgress => "No project has a task in progress.",
        DashboardFilter::All => "No projects yet. Add one from a project window's Projects panel.",
    }
}

// ---- Words --------------------------------------------------------------------------------

/// What the Chat controls say and do, in the dashboard and in the status bar's menu alike.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatCopy {
    /// The button's or menu row's label.
    pub action: String,
    /// The global orchestrator's state, in a line.
    pub status: String,
    /// What else to know before clicking.
    pub note: Option<String>,
    /// A click may start a new agent.
    pub starts: bool,
    pub enabled: bool,
    pub provider: Option<Provider>,
}

fn activity_word(activity: Option<AgentActivity>) -> &'static str {
    match activity {
        Some(AgentActivity::Working) => "working",
        Some(AgentActivity::Waiting) => "waiting for you",
        Some(AgentActivity::Done) => "finished its turn",
        Some(AgentActivity::Exited) => "agent exited",
        Some(AgentActivity::Unknown) | None => "running",
    }
}

/// The Chat controls' words for the global orchestrator as it is. A terminal one is named as
/// one and opened as it is: nothing converts it into a chat or ends it.
pub fn chat_copy(global: &GlobalOrchestrator, runs: OrchestratorRuns, starting: bool) -> ChatCopy {
    let copy = |action: &str, status: String, note: Option<String>, starts: bool| ChatCopy {
        action: action.to_owned(),
        status,
        note,
        starts,
        enabled: !starting,
        provider: None,
    };
    if starting {
        return copy(
            "Opening global chat…",
            "Finding the global orchestrator".to_owned(),
            None,
            false,
        );
    }
    match global {
        GlobalOrchestrator::Chat {
            provider, state, ..
        } => ChatCopy {
            provider: Some(*provider),
            ..copy(
                "Open global chat",
                format!(
                    "{} · {}",
                    session_catalog::provider_label(*provider),
                    session_catalog::status_label(state)
                ),
                match state {
                    ChatState::Failed { message } => Some(format!(
                        "Its agent failed: {message}. The next message resumes it."
                    )),
                    ChatState::Stopped => Some("Stopped; the next message resumes it.".to_owned()),
                    _ => None,
                },
                false,
            )
        },
        GlobalOrchestrator::Terminal { activity, .. } => copy(
            "Open terminal orchestrator",
            format!("Runs in a terminal · {}", activity_word(*activity)),
            Some(
                "RiWork keeps this terminal as it is; a chat takes over only after you end it."
                    .to_owned(),
            ),
            false,
        ),
        GlobalOrchestrator::None { exited_terminal } => {
            let status = if *exited_terminal {
                "No global orchestrator · its last terminal has exited".to_owned()
            } else {
                "No global orchestrator yet".to_owned()
            };
            match runs {
                OrchestratorRuns::Chat(provider) => ChatCopy {
                    provider: Some(provider),
                    ..copy(
                        "Start global chat",
                        status,
                        Some(format!(
                            "Starts a {} chat that asks before commands and edits.",
                            session_catalog::provider_label(provider)
                        )),
                        true,
                    )
                },
                OrchestratorRuns::Terminal => copy(
                    "Start global orchestrator",
                    status,
                    Some("Settings run new orchestrators in a terminal.".to_owned()),
                    true,
                ),
            }
        }
        GlobalOrchestrator::Unknown => copy(
            "Open global chat",
            "Reading chats…".to_owned(),
            Some("Opens the global orchestrator there is, or starts one as Settings say.".into()),
            true,
        ),
    }
}

/// The dashboard in a line, for the status bar's menu.
pub fn summary(overview: &Overview, sampled: bool) -> String {
    if !sampled {
        return "Reading sessions…".to_owned();
    }
    let totals = &overview.totals;
    let mut parts = Vec::new();
    if totals.agents.working > 0 {
        parts.push(format!("{} working", totals.agents.working));
    }
    if totals.attention > 0 {
        parts.push(format!("{} need you", totals.attention));
    }
    if totals.tasks.in_progress > 0 {
        parts.push(format!("{} tasks in progress", totals.tasks.in_progress));
    }
    if parts.is_empty() {
        format!("All quiet across {}", plural(totals.projects, "project"))
    } else {
        parts.join(" · ")
    }
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// "alpha · feat/x", "Global", or the project's id when the store no longer has it.
fn place(project_id: Option<&str>, project_name: Option<&str>, branch: Option<&str>) -> String {
    let project = match (project_id, project_name) {
        (_, Some(name)) => name.to_owned(),
        (None, None) => "Global".to_owned(),
        (Some(id), None) => format!("Project {}", id.chars().take(8).collect::<String>()),
    };
    match branch {
        Some(branch) => format!("{project} · {branch}"),
        None => project,
    }
}

fn session_name(session: &SessionSummary) -> String {
    let what = match &session.orchestrator {
        Some(OrchestratorScope::Global) => format!("Global orchestrator ({})", session.agent),
        Some(OrchestratorScope::Project { .. }) => {
            format!("Project orchestrator ({})", session.agent)
        }
        None if session.is_chat => format!("{} chat", session.agent),
        None => session.agent.to_owned(),
    };
    match &session.title {
        Some(title) if session.orchestrator.is_none() => format!("{what} · {title}"),
        _ => what,
    }
}

fn session_place(session: &SessionSummary) -> String {
    place(
        session.project_id.as_deref(),
        session.project_name.as_deref(),
        session.branch.as_deref(),
    )
}

fn age(now: u64, at: Option<u64>) -> Option<String> {
    let at = at?;
    Some(match short_duration(now.saturating_sub(at)).as_str() {
        "now" => "just now".to_owned(),
        short => format!("{short} ago"),
    })
}

/// What an attention row says: a headline and a detail line.
fn attention_copy(item: &Attention, now: u64) -> (String, String) {
    let when = age(now, item.at);
    let place = place(
        item.project_id.as_deref(),
        item.project_name.as_deref(),
        item.session
            .as_ref()
            .and_then(|session| session.branch.as_deref()),
    );
    let join = |parts: Vec<Option<String>>| {
        parts
            .into_iter()
            .flatten()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" · ")
    };
    match &item.kind {
        AttentionKind::Waiting => (
            format!(
                "{} waits for you",
                item.session
                    .as_ref()
                    .map_or("An agent".to_owned(), session_name)
            ),
            join(vec![Some(place), when.map(|when| format!("since {when}"))]),
        ),
        AttentionKind::ChatFailed { message } => (
            format!(
                "{} failed",
                item.session
                    .as_ref()
                    .map_or("A chat".to_owned(), session_name)
            ),
            join(vec![Some(place), Some(one_line(message)), when]),
        ),
        AttentionKind::Automation {
            title,
            outcome,
            message,
        } => (
            format!(
                "Automation “{title}” {}",
                match outcome {
                    Outcome::Missed => "missed its run",
                    Outcome::Uncertain => "may not have run",
                    _ => "failed",
                }
            ),
            join(vec![
                Some(if item.project_id.is_none() {
                    "App".to_owned()
                } else {
                    place
                }),
                Some(one_line(message)),
                when,
            ]),
        ),
    }
}

fn one_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() > 140 {
        format!("{}…", line.chars().take(139).collect::<String>())
    } else {
        line.to_owned()
    }
}

/// A project's agents by what they do, as colored parts; nothing when it has none.
fn agent_parts(project: &ProjectOverview, colors: Palette) -> Vec<(String, u32)> {
    let agents = &project.agents;
    let mut parts = Vec::new();
    if agents.waiting > 0 {
        parts.push((format!("◌ {} waiting", agents.waiting), colors.gold));
    }
    if agents.working > 0 {
        parts.push((format!("● {} working", agents.working), colors.working));
    }
    if agents.done > 0 {
        parts.push((format!("✓ {} done", agents.done), colors.cyan));
    }
    if agents.unknown > 0 {
        parts.push((format!("? {} unknown", agents.unknown), colors.muted));
    }
    parts
}

/// Its live sessions: terminals and, once the chats were read, chats.
fn sessions_line(project: &ProjectOverview, chats_known: bool) -> String {
    let terminals = plural(project.live_shells, "terminal");
    if !chats_known {
        return format!("{terminals} · chats not read yet");
    }
    match project.live_shells + project.live_chats {
        0 => "No live sessions".to_owned(),
        live => format!(
            "{live} live · {terminals} · {}",
            plural(project.live_chats, "chat")
        ),
    }
}

fn orchestrator_words(orchestrator: &Option<ProjectOrchestrator>) -> Option<(String, String)> {
    Some(match orchestrator.as_ref()? {
        ProjectOrchestrator::Terminal { activity } => {
            ("Terminal".to_owned(), activity_word(*activity).to_owned())
        }
        ProjectOrchestrator::Chat { provider, state } => (
            format!("{} chat", session_catalog::provider_label(*provider)),
            session_catalog::status_label(state).to_lowercase(),
        ),
    })
}

/// "71/85 done", then what is open, or `None` without tasks.
fn tasks_words(tasks: TaskCounts) -> Option<(String, String)> {
    if tasks.total() == 0 {
        return None;
    }
    let open = match (tasks.in_progress, tasks.todo) {
        (0, 0) => "all done".to_owned(),
        (0, todo) => format!("{todo} to do"),
        (in_progress, 0) => format!("{in_progress} in progress"),
        (in_progress, todo) => format!("{in_progress} in progress · {todo} to do"),
    };
    Some((format!("{}/{} done", tasks.done, tasks.total()), open))
}

/// The project's mark: its most pressing state, as a color and a word.
fn project_state(project: &ProjectOverview, colors: Palette) -> (&'static str, u32, &'static str) {
    if project.attention > 0 {
        ("●", colors.gold, "needs you")
    } else if project.agents.working > 0 {
        ("●", colors.working, "working")
    } else if !project.is_quiet() {
        ("●", theme::mix(colors.muted, colors.panel, 0.2), "active")
    } else {
        ("○", theme::mix(colors.muted, colors.panel, 0.4), "quiet")
    }
}

fn needs_you(count: usize) -> String {
    if count == 1 {
        "1 needs you".to_owned()
    } else {
        format!("{count} need you")
    }
}

/// A project row's accessible name: everything its cells say, then where it goes.
fn project_name_for_reader(
    project: &ProjectOverview,
    chats_known: bool,
    now: u64,
    colors: Palette,
) -> String {
    let mut parts = vec![format!("Project {}", project.name)];
    if let Some(folder) = &project.folder {
        parts.push(format!("in {folder}"));
    }
    parts.push(if project.attention > 0 {
        needs_you(project.attention)
    } else {
        project_state(project, colors).2.to_owned()
    });
    parts.extend(agent_parts(project, colors).into_iter().map(|(part, _)| {
        part.trim_start_matches(['◌', '●', '✓', '?', ' '])
            .to_owned()
    }));
    parts.push(sessions_line(project, chats_known));
    match tasks_words(project.tasks) {
        Some((done, open)) => parts.push(format!("tasks {done}, {open}")),
        None => parts.push("no tasks".to_owned()),
    }
    if let Some((orchestrator, state)) = orchestrator_words(&project.orchestrator) {
        parts.push(format!("project orchestrator: {orchestrator}, {state}"));
    }
    parts.push(match age(now, project.last_activity) {
        Some(when) => format!("last activity {when}"),
        None => "no recorded activity".to_owned(),
    });
    format!("{}; opens the project", parts.join(", "))
}

/// What an empty attention list can honestly say: only what was read.
fn quiet_attention(overview: &Overview) -> String {
    let mut text = "Nothing waits for you: no agent asks a question".to_owned();
    let mut unknown = Vec::new();
    if overview.chats_known {
        text.push_str(", no chat failed");
    } else {
        unknown.push("Chats are still being read.");
    }
    match overview.automations {
        Automations::Read { .. } => text.push_str(", no automation failed"),
        Automations::NotRead => unknown.push("Automations are still being read."),
        Automations::Unreadable(_) => unknown.push("Automations could not be read."),
    }
    text.push('.');
    for part in unknown {
        text.push(' ');
        text.push_str(part);
    }
    text
}

fn automation_detail(item: &AutomationSummary, now: u64) -> String {
    let mut parts = vec![if item.project_id.is_none() {
        "App".to_owned()
    } else {
        place(
            item.project_id.as_deref(),
            item.project_name.as_deref(),
            None,
        )
    }];
    if item.paused {
        parts.push("paused".to_owned());
    } else {
        match item.next_run {
            Some(at) if at > now => parts.push(format!("next in {}", short_duration(at - now))),
            Some(_) => parts.push("due now".to_owned()),
            None => parts.push("no next run".to_owned()),
        }
    }
    if let Some((outcome, at)) = &item.last_run {
        let word = match outcome {
            Outcome::Failed => "last run failed",
            Outcome::Missed => "last run missed",
            Outcome::Uncertain => "last run uncertain",
            Outcome::Deferred => "last run deferred",
            Outcome::Dispatching => "running",
            Outcome::Submitted => "last run sent",
        };
        parts.push(match age(now, Some(*at)) {
            Some(when) => format!("{word} {when}"),
            None => word.to_owned(),
        });
    }
    parts.join(" · ")
}

// ---- What is listed ------------------------------------------------------------------------

/// The rows each region lists under the search, each with its index in the overview's list.
struct Lists<'a> {
    projects: Vec<&'a ProjectOverview>,
    counts: Vec<(DashboardFilter, usize)>,
    attention: Vec<(usize, &'a Attention)>,
    working: Vec<(usize, &'a Work)>,
    finished: Vec<(usize, &'a Work)>,
    tasks: Vec<&'a TaskOverview>,
    automations: Vec<&'a AutomationSummary>,
}

impl<'a> Lists<'a> {
    fn new(data: &DashboardData<'a>) -> Self {
        let overview = data.overview;
        let query = data.query;
        let work = |list: &'a [Work]| {
            list.iter()
                .enumerate()
                .filter(|(_, work)| {
                    matches(
                        query,
                        &[&session_name(&work.session), &session_place(&work.session)],
                    )
                })
                .collect::<Vec<_>>()
        };
        Self {
            projects: visible_projects(&overview.projects, query, data.filter, data.sort),
            counts: filter_counts(&overview.projects, query),
            attention: overview
                .attention
                .iter()
                .enumerate()
                .filter(|(_, item)| {
                    let (title, detail) = attention_copy(item, data.now);
                    matches(query, &[&title, &detail])
                })
                .collect(),
            working: work(&overview.working),
            finished: work(&overview.finished),
            tasks: overview
                .tasks_in_progress
                .iter()
                .filter(|task| {
                    matches(
                        query,
                        &[
                            &task.title,
                            &task.project_name,
                            task.branch.as_deref().unwrap_or(""),
                        ],
                    )
                })
                .collect(),
            automations: match &overview.automations {
                Automations::Read { items, .. } => items
                    .iter()
                    .filter(|item| {
                        matches(
                            query,
                            &[&item.title, item.project_name.as_deref().unwrap_or("App")],
                        )
                    })
                    .collect(),
                _ => Vec::new(),
            },
        }
    }

    fn count(&self, filter: DashboardFilter) -> usize {
        self.counts
            .iter()
            .find(|(each, _)| *each == filter)
            .map_or(0, |(_, count)| *count)
    }
}

// ---- Drawing -------------------------------------------------------------------------------

/// Draw the dashboard. `on_action` gets every click.
pub fn render<V: 'static>(
    data: DashboardData<'_>,
    on_action: impl Fn(&mut V, DashboardAction, &mut Window, &mut Context<V>) + Clone + 'static,
    window: &Window,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let native = ui_text::is_native();
    let layout = Layout::of(data.width);
    let toolbar = toolbar(&data, layout, on_action.clone(), window, cx);

    let body = if data.sampled_at.is_none() {
        // Nothing has been read yet: no figure is shown rather than a zero that was not
        // counted. The toolbar says why when the read failed.
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(match data.sample_error {
                None => empty_state(
                    "hourglass",
                    "Reading the sessions, chats and tasks of every project…",
                    colors,
                ),
                Some(_) => empty_state(
                    "exclamationmark.triangle",
                    "The sessions could not be read, so nothing can be counted yet. RiWork tries again every two seconds; Refresh tries now.",
                    colors,
                ),
            })
            .into_any_element()
    } else {
        body(&data, layout, on_action, cx)
    };

    let page = if native {
        controls::panel(colors)
    } else {
        div()
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(rgb(colors.panel))
            .text_color(rgb(colors.text))
            .font_family(ui_text::ui_family())
            .text_size(ui_text::text(11.0))
    };
    page.id("global-dashboard")
        .overflow_hidden()
        .child(toolbar)
        .child(body)
        .into_any_element()
}

fn body<V: 'static>(
    data: &DashboardData<'_>,
    layout: Layout,
    on_action: impl Fn(&mut V, DashboardAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let lists = Lists::new(data);
    let activity = |cx: &mut Context<V>| {
        vec![
            attention_section(data, &lists, on_action.clone(), cx),
            work_section(data, &lists, on_action.clone(), cx),
        ]
        .into_iter()
        .chain(finished_section(data, &lists, on_action.clone(), cx))
        .collect::<Vec<_>>()
    };
    let work = |cx: &mut Context<V>| {
        vec![
            tasks_section(data, &lists, on_action.clone(), cx),
            automations_section(data, &lists, on_action.clone(), cx),
        ]
    };
    let main = div()
        .id("dashboard-main")
        .flex_1()
        .min_h_0()
        .min_w_0()
        .flex()
        .items_stretch()
        .gap(ui_text::space(GAP))
        .px(ui_text::space(PAGE));
    let main = match layout.arrangement {
        Arrangement::Wide => {
            let activity = activity(cx);
            let work = work(cx);
            main.child(projects_region(data, &lists, layout, on_action.clone(), cx))
                .child(rail("dashboard-rail-activity", activity, false, colors))
                .child(rail("dashboard-rail-work", work, false, colors))
        }
        Arrangement::Normal => {
            // The pressing first: what needs the user and what works, then the tasks and
            // the automations, and what finished last of all.
            let mut sections = vec![
                attention_section(data, &lists, on_action.clone(), cx),
                work_section(data, &lists, on_action.clone(), cx),
            ];
            sections.extend(work(cx));
            sections.extend(finished_section(data, &lists, on_action.clone(), cx));
            main.child(projects_region(data, &lists, layout, on_action.clone(), cx))
                .child(rail("dashboard-rail", sections, false, colors))
        }
        Arrangement::Compact => match data.region {
            Region::Projects => {
                main.child(projects_region(data, &lists, layout, on_action.clone(), cx))
            }
            Region::Activity => {
                let activity = activity(cx);
                main.child(rail("dashboard-rail-activity", activity, true, colors))
            }
            Region::Work => {
                let work = work(cx);
                main.child(rail("dashboard-rail-work", work, true, colors))
            }
        },
    };
    let compact = layout.arrangement == Arrangement::Compact;
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .children(compact.then(|| {
            div()
                .flex_none()
                .px(ui_text::space(PAGE))
                .pt(ui_text::space(6.0))
                .child(summary_strip(data, on_action.clone(), cx))
                .into_any_element()
        }))
        .children(compact.then(|| region_switcher(data, &lists, on_action.clone(), cx)))
        .child(main.pb(ui_text::space(8.0)))
        .into_any_element()
}

/// A column of sections that scrolls on its own, so the projects never push it away.
fn rail(id: &'static str, sections: Vec<AnyElement>, fill: bool, colors: Palette) -> AnyElement {
    div()
        .id(id)
        .when(fill, |rail| rail.flex_1().min_w_0())
        .when(!fill, |rail| {
            rail.flex_none()
                .w(ui_text::space(RAIL))
                .pl(ui_text::space(4.0))
                .border_l_1()
                .border_color(rgb(colors.divider))
        })
        .min_h_0()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap(ui_text::space(4.0))
        .pt(ui_text::space(4.0))
        .children(sections)
        .test_support()
        .into_any_element()
}

fn toolbar<V: 'static>(
    data: &DashboardData<'_>,
    layout: Layout,
    on_action: impl Fn(&mut V, DashboardAction, &mut Window, &mut Context<V>) + Clone + 'static,
    window: &Window,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let native = ui_text::is_native();
    let compact = layout.arrangement == Arrangement::Compact;
    let refresh = {
        let on_action = on_action.clone();
        let button = if native {
            behavior::toolbar_button(
                "dashboard-refresh",
                "arrow.clockwise",
                "Read everything again now",
                true,
                colors,
            )
        } else {
            behavior::action("dashboard-refresh", "Read everything again now", colors)
                .flex_none()
                .px(ui_text::space(6.0))
                .py(ui_text::space(2.0))
                .text_size(ui_text::text(9.0))
                .text_color(rgb(colors.cyan))
                .hover(move |style| style.bg(rgb(colors.panel_active)))
                .child(ui_text::cased("↻ Refresh"))
        };
        crate::form_input::control_element(
            "dashboard-refresh",
            button.on_click(cx.listener(move |view, _, window, cx| {
                on_action(view, DashboardAction::Refresh, window, cx)
            })),
        )
    };
    let (status, warning) = freshness(data);
    div()
        .id("dashboard-toolbar")
        .flex_none()
        .flex()
        .items_center()
        .gap(ui_text::space(10.0))
        .px(ui_text::space(PAGE))
        .py(ui_text::space(6.0))
        .border_b_1()
        .border_color(rgb(colors.divider))
        .children(
            data.search.map(|search| {
                search_field(search, data.query, compact, on_action.clone(), window, cx)
            }),
        )
        .child(if compact || data.sampled_at.is_none() {
            div().flex_1().into_any_element()
        } else {
            div()
                .flex_1()
                .min_w_0()
                .child(summary_strip(data, on_action.clone(), cx))
                .into_any_element()
        })
        .child(
            div()
                .id("dashboard-status")
                .flex_none()
                .max_w(ui_text::space(if compact { 180.0 } else { 260.0 }))
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_size(ui_text::text(10.0))
                .text_color(rgb(if warning { colors.gold } else { colors.muted }))
                .child(status)
                .test_support(),
        )
        .child(refresh)
        .into_any_element()
}

/// How fresh the figures are, and whether that is worth a warning.
fn freshness(data: &DashboardData<'_>) -> (String, bool) {
    match (data.sampled_at, data.sample_error) {
        (None, None) => ("Reading sessions…".to_owned(), false),
        (None, Some(error)) => (
            format!("Sessions could not be read: {}", one_line(error)),
            true,
        ),
        (Some(at), Some(error)) => (
            format!(
                "Showing the read from {} ago · {}",
                short_duration(data.now.saturating_sub(at)),
                one_line(error)
            ),
            true,
        ),
        (Some(at), None) => {
            let age = data.now.saturating_sub(at);
            if age > STALE_AFTER_SECS {
                (format!("Last read {} ago", short_duration(age)), true)
            } else {
                ("Live".to_owned(), false)
            }
        }
    }
}

/// The search: the window's retained field, its magnifier, and a clear button once
/// something is typed.
fn search_field<V: 'static>(
    input: &Entity<InputState>,
    query: &str,
    compact: bool,
    on_action: impl Fn(&mut V, DashboardAction, &mut Window, &mut Context<V>) + Clone + 'static,
    window: &Window,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let native = ui_text::is_native();
    let focused = input.read(cx).focus_handle(cx).is_focused(window);
    let field = div()
        .id("dashboard-search")
        .relative()
        .cursor_text()
        .h(ui_text::space(26.0))
        .flex()
        .items_center()
        .gap(ui_text::space(5.0))
        .px(ui_text::space(8.0))
        .min_w(ui_text::space(160.0))
        .when(compact, |field| field.flex_1())
        .when(!compact, |field| field.flex_none().w(ui_text::space(300.0)))
        .border_1()
        .border_color(rgb(if focused {
            colors.focus
        } else {
            colors.divider
        }))
        .when(native, |field| {
            field
                .rounded_full()
                .bg(rgb(colors.panel_active))
                .border_color(if focused {
                    rgb(colors.focus).into()
                } else {
                    gpui::transparent_black()
                })
        })
        .text_color(rgb(if focused { colors.text } else { colors.muted }));
    let clear = (!query.is_empty()).then(|| {
        let input = input.clone();
        let button = behavior::button_content(
            "dashboard-search-clear",
            "Clear the search",
            if native {
                icons::symbol("xmark.circle.fill", 10.0, None)
            } else {
                div().child("×").into_any_element()
            },
        )
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .text_color(rgb(colors.muted))
        .hover(move |style| style.text_color(rgb(colors.text)))
        .focus_visible(move |style| style.text_color(rgb(colors.focus)))
        .on_click(cx.listener(move |view, _, window, cx| {
            cx.stop_propagation();
            on_action(view, DashboardAction::ClearSearch, window, cx);
            input.read(cx).focus_handle(cx).focus(window, cx);
        }));
        crate::form_input::control_element("dashboard-search-clear", button)
    });
    let focus = input.clone();
    field
        .child(
            div()
                .id("dashboard-search-icon")
                .flex_none()
                .w(ui_text::space(12.0))
                .flex()
                .items_center()
                .justify_center()
                .child(icons::mark("⌕", 10.0, colors.muted))
                .test_support(),
        )
        .child(
            crate::form_input::search_frame("dashboard-search-input", input, window, cx)
                .accessibility_label("Search projects, tasks and sessions"),
        )
        .children(clear)
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            let state = focus.read(cx);
            if !state.presentation().is_disabled() {
                state.focus_handle(cx).focus(window, cx);
                window.prevent_default();
            }
        })
        .into_any_element()
}

/// The figures in a line: who works, what needs the user, what runs, how far the tasks
/// are, and the global chat's state with its one control.
fn summary_strip<V: 'static>(
    data: &DashboardData<'_>,
    on_action: impl Fn(&mut V, DashboardAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let overview = data.overview;
    let totals = &overview.totals;
    let chats_known = overview.chats_known;
    let stat = |id: &'static str, parts: Vec<(String, u32)>| {
        div()
            .id(id)
            .flex_none()
            .flex()
            .items_center()
            .gap(ui_text::space(4.0))
            .whitespace_nowrap()
            .children(
                parts
                    .into_iter()
                    .map(|(text, color)| div().text_color(rgb(color)).child(text)),
            )
            .into_any_element()
    };
    let working = match (totals.agents.working, totals.agents.subagents) {
        (0, _) => vec![("No agent working".to_owned(), colors.muted)],
        (working, 0) => vec![(format!("● {working} working"), colors.working)],
        (working, subagents) => vec![
            (format!("● {working} working"), colors.working),
            (format!("· {}", plural(subagents, "subagent")), colors.muted),
        ],
    };
    let attention = match totals.attention {
        0 => vec![("Nothing waits for you".to_owned(), colors.muted)],
        count => vec![(format!("◌ {}", needs_you(count)), colors.gold)],
    };
    let live = if chats_known {
        vec![
            (
                format!("{} live", totals.live_shells + totals.live_chats),
                colors.text,
            ),
            (
                format!(
                    "· {} · {}",
                    plural(totals.live_shells, "terminal"),
                    plural(totals.live_chats, "chat")
                ),
                colors.muted,
            ),
        ]
    } else {
        vec![
            (plural(totals.live_shells, "terminal"), colors.text),
            ("· chats not read yet".to_owned(), colors.muted),
        ]
    };
    let tasks = div()
        .id("dashboard-summary-tasks")
        .flex_none()
        .flex()
        .items_center()
        .gap(ui_text::space(4.0))
        .whitespace_nowrap()
        .children(match tasks_words(totals.tasks) {
            None => vec![
                div()
                    .text_color(rgb(colors.muted))
                    .child("No tasks")
                    .into_any_element(),
            ],
            Some((done, open)) => vec![
                div()
                    .text_color(rgb(colors.text))
                    .child(format!("Tasks {done}"))
                    .into_any_element(),
                div()
                    .w(ui_text::space(56.0))
                    .child(progress_bar(totals.tasks, colors))
                    .into_any_element(),
                div()
                    .text_color(rgb(colors.muted))
                    .child(open)
                    .into_any_element(),
            ],
        })
        .into_any_element();
    let chat = chat_copy(&overview.global, data.runs, data.chat_starting);
    let kind = match (chat.enabled, chat.starts) {
        (false, _) => controls::Button::Disabled,
        (true, true) => controls::Button::Primary,
        (true, false) => controls::Button::Secondary,
    };
    let note = chat.note.clone();
    let button = behavior::button("dashboard-global-chat", chat.action.clone(), kind, colors)
        .flex_none()
        .relative()
        .px(ui_text::space(8.0))
        .py(ui_text::space(1.0))
        .text_size(ui_text::text(10.0))
        .children(note.map(|note| tooltip::anchor(note, Look::Control)))
        .on_click(cx.listener(move |view, _, window, cx| {
            on_action(view, DashboardAction::GlobalChat, window, cx)
        }));
    let chat = div()
        .id("dashboard-summary-chat")
        .flex_none()
        .flex()
        .items_center()
        .gap(ui_text::space(6.0))
        .whitespace_nowrap()
        .child(div().text_color(rgb(colors.muted)).child("Global chat"))
        .child(
            div()
                .text_color(rgb(colors.text))
                .child(chat.status.clone()),
        )
        .child(crate::form_input::control_element(
            "dashboard-global-chat",
            button,
        ))
        .into_any_element();
    let dot = || {
        div()
            .flex_none()
            .text_color(rgb(colors.divider))
            .child("|")
            .into_any_element()
    };
    div()
        .id("dashboard-summary")
        .min_w_0()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_x(ui_text::space(10.0))
        .gap_y(ui_text::space(4.0))
        .text_size(ui_text::text(10.0))
        .child(stat("dashboard-summary-working", working))
        .child(dot())
        .child(stat("dashboard-summary-attention", attention))
        .child(dot())
        .child(stat("dashboard-summary-live", live))
        .child(dot())
        .child(tasks)
        .child(dot())
        .child(chat)
        .test_support()
        .into_any_element()
}

/// Projects | Activity | Tasks: what a compact dashboard shows.
fn region_switcher<V: 'static>(
    data: &DashboardData<'_>,
    lists: &Lists<'_>,
    on_action: impl Fn(&mut V, DashboardAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let segment = |id: &'static str, label: String, region: Region, cx: &mut Context<V>| {
        let on_action = on_action.clone();
        let listener = cx.listener(move |view, _: &gpui::ClickEvent, window, cx| {
            on_action(view, DashboardAction::Region(region), window, cx)
        });
        behavior::segment(id, label, data.region == region, colors)
            .text_size(ui_text::text(10.0))
            .on_change(move |_, event, window, cx| listener(event, window, cx))
    };
    let activity = lists.attention.len() + lists.working.len();
    let projects = segment(
        "dashboard-region-projects",
        format!("Projects {}", lists.projects.len()),
        Region::Projects,
        cx,
    );
    let activity = segment(
        "dashboard-region-activity",
        if lists.attention.is_empty() {
            format!("Activity {activity}")
        } else {
            format!("Activity · {}", needs_you(lists.attention.len()))
        },
        Region::Activity,
        cx,
    );
    let work = segment(
        "dashboard-region-work",
        format!("Tasks {}", lists.tasks.len()),
        Region::Work,
        cx,
    );
    div()
        .flex_none()
        .flex()
        .px(ui_text::space(PAGE))
        .py(ui_text::space(6.0))
        .child(
            behavior::segments("dashboard-regions", "Dashboard sections", colors)
                .flex_none()
                .child(projects)
                .child(activity)
                .child(work),
        )
        .into_any_element()
}

/// The projects: their heading with the filters and the order, the column heads, and the
/// rows, which scroll under the heads.
fn projects_region<V: 'static>(
    data: &DashboardData<'_>,
    lists: &Lists<'_>,
    layout: Layout,
    on_action: impl Fn(&mut V, DashboardAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let native = ui_text::is_native();
    let total = data.overview.projects.len();
    let heading = {
        let filter = |filter: DashboardFilter, cx: &mut Context<V>| {
            let on_action = on_action.clone();
            let listener = cx.listener(move |view, _: &gpui::ClickEvent, window, cx| {
                on_action(view, DashboardAction::Filter(filter), window, cx)
            });
            behavior::segment(
                format!("dashboard-filter-{}", filter.name()),
                format!("{} {}", filter_label(filter), lists.count(filter)),
                data.filter == filter,
                colors,
            )
            .text_size(ui_text::text(10.0))
            .on_change(move |_, event, window, cx| listener(event, window, cx))
        };
        let sort = |sort: DashboardSort, cx: &mut Context<V>| {
            let on_action = on_action.clone();
            let listener = cx.listener(move |view, _: &gpui::ClickEvent, window, cx| {
                on_action(view, DashboardAction::Sort(sort), window, cx)
            });
            behavior::segment(
                format!("dashboard-sort-{}", sort.name()),
                sort_label(sort),
                data.sort == sort,
                colors,
            )
            .text_size(ui_text::text(10.0))
            .on_change(move |_, event, window, cx| listener(event, window, cx))
        };
        let mut filters =
            behavior::segments("dashboard-filter", "Show projects", colors).flex_none();
        for each in DashboardFilter::ALL {
            filters = filters.child(filter(each, cx));
        }
        let mut sorts =
            behavior::segments("dashboard-sort", "Sort projects by", colors).flex_none();
        for each in DashboardSort::ALL {
            sorts = sorts.child(sort(each, cx));
        }
        div()
            .id("dashboard-projects-heading")
            .flex_none()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x(ui_text::space(10.0))
            .gap_y(ui_text::space(4.0))
            .px(ui_text::space(ROW_PAD))
            .pt(ui_text::space(6.0))
            .pb(ui_text::space(4.0))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_baseline()
                    .gap(ui_text::space(6.0))
                    .text_size(ui_text::text(controls::META_TEXT))
                    .child(
                        div()
                            .text_color(rgb(if native { colors.text } else { colors.magenta }))
                            .when(native, |title| title.font_weight(FontWeight::SEMIBOLD))
                            .child(ui_text::cased("Projects")),
                    )
                    .child(
                        div()
                            .font_family(ui_text::mono_family())
                            .text_color(rgb(colors.muted))
                            .child(format!("{} of {total}", lists.projects.len())),
                    ),
            )
            .child(filters)
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(ui_text::space(6.0))
                    .child(
                        div()
                            .text_size(ui_text::text(10.0))
                            .text_color(rgb(colors.muted))
                            .child(ui_text::cased("Sort")),
                    )
                    .child(sorts),
            )
    };
    let rows: Vec<AnyElement> = if lists.projects.is_empty() {
        vec![no_projects(data, lists, on_action.clone(), cx)]
    } else {
        lists
            .projects
            .iter()
            .map(|project| project_row(project, data, layout, on_action.clone(), cx))
            .collect()
    };
    div()
        .id("dashboard-projects")
        .flex_1()
        .min_w_0()
        .min_h_0()
        .flex()
        .flex_col()
        .child(heading.test_support())
        .child(table_head(layout, colors))
        .child(
            div()
                .id("dashboard-projects-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .children(rows),
        )
        .test_support()
        .into_any_element()
}

/// What an empty table says: why nothing is listed, and the way to what is hidden.
fn no_projects<V: 'static>(
    data: &DashboardData<'_>,
    lists: &Lists<'_>,
    on_action: impl Fn(&mut V, DashboardAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let query = data.query.trim();
    let elsewhere = lists.count(DashboardFilter::All);
    let text = if data.overview.projects.is_empty() {
        nothing_listed(DashboardFilter::All).to_owned()
    } else if query.is_empty() {
        nothing_listed(data.filter).to_owned()
    } else if data.filter == DashboardFilter::All || elsewhere == 0 {
        format!("No project matches “{query}”.")
    } else {
        format!(
            "No project in {} matches “{query}”; {} in All {}.",
            filter_label(data.filter),
            plural(elsewhere, "project"),
            if elsewhere == 1 { "does" } else { "do" }
        )
    };
    let show_all = (data.filter != DashboardFilter::All && elsewhere > 0).then(|| {
        let label = format!("Show all {}", plural(elsewhere, "project"));
        let button = behavior::action("dashboard-show-all", label.clone(), colors)
            .flex_none()
            .px(ui_text::space(6.0))
            .py(ui_text::space(2.0))
            .text_size(ui_text::text(10.0))
            .text_color(rgb(colors.cyan))
            .hover(move |style| style.text_color(rgb(colors.text)))
            .child(ui_text::cased(label))
            .on_click(cx.listener(move |view, _, window, cx| {
                on_action(
                    view,
                    DashboardAction::Filter(DashboardFilter::All),
                    window,
                    cx,
                )
            }));
        crate::form_input::control_element("dashboard-show-all", button)
    });
    div()
        .id("dashboard-projects-empty")
        .flex()
        .flex_col()
        .items_start()
        .gap(ui_text::space(4.0))
        .px(ui_text::space(ROW_PAD))
        .py(ui_text::space(10.0))
        .child(
            div()
                .text_size(ui_text::text(10.0))
                .text_color(rgb(colors.muted))
                .child(text),
        )
        .children(show_all)
        .test_support()
        .into_any_element()
}

fn cell(width: f32) -> Div {
    div()
        .flex_none()
        .w(ui_text::space(width))
        .min_w_0()
        .overflow_hidden()
        .flex()
        .flex_col()
        .gap(ui_text::space(2.0))
}

fn line(text: impl Into<gpui::SharedString>, color: u32, size: f32) -> Div {
    div()
        .w_full()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
        .text_size(ui_text::text(size))
        .text_color(rgb(color))
        .child(text.into())
}

/// The column heads, on the same measures as the rows.
fn table_head(layout: Layout, colors: Palette) -> AnyElement {
    let widths = layout.columns.widths();
    let native = ui_text::is_native();
    let head = |width: Option<f32>, label: &'static str| {
        width.map(|width| {
            cell(width).child(line(
                ui_text::cased(label),
                colors.muted,
                controls::ROW_DETAIL_TEXT - 1.0,
            ))
        })
    };
    div()
        .id("dashboard-table-head")
        .flex_none()
        .flex()
        .items_center()
        .gap(ui_text::space(CELL_GAP))
        .pr(ui_text::space(ROW_PAD))
        .py(ui_text::space(3.0))
        .border_b_1()
        .border_color(rgb(colors.divider))
        .when(native, |head| head.font_weight(FontWeight::SEMIBOLD))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap(ui_text::space(CELL_GAP))
                .pl(ui_text::space(ROW_PAD + 1.0))
                .child(div().flex_none().w(ui_text::space(MARK)))
                .child(div().flex_1().min_w_0().child(line(
                    ui_text::cased("Project"),
                    colors.muted,
                    controls::ROW_DETAIL_TEXT - 1.0,
                )))
                .children(head(widths.activity, "Activity"))
                .children(head(Some(widths.tasks), "Tasks"))
                .children(head(widths.orchestrator, "Orchestrator"))
                .children(head(Some(widths.last), "Last active")),
        )
        .child(div().flex_none().w(ui_text::space(widths.actions)))
        .test_support()
        .into_any_element()
}

fn project_row<V: 'static>(
    project: &ProjectOverview,
    data: &DashboardData<'_>,
    layout: Layout,
    on_action: impl Fn(&mut V, DashboardAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let native = ui_text::is_native();
    let chats_known = data.overview.chats_known;
    let widths = layout.columns.widths();
    let (mark, mark_color, _) = project_state(project, colors);
    let agents = agent_parts(project, colors);
    let orchestrator = orchestrator_words(&project.orchestrator);

    // The project's own lines: its name, group and what needs the user, then where it is
    // and, when the table has no column for them, what it runs.
    let name_line = div()
        .w_full()
        .min_w_0()
        .flex()
        .items_center()
        .gap(ui_text::space(6.0))
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_size(ui_text::text(controls::ROW_TITLE_TEXT))
                .text_color(rgb(colors.text))
                .when(native, |name| name.font_weight(FontWeight::MEDIUM))
                .child(project.name.clone()),
        )
        .children(project.folder.clone().map(|folder| {
            div()
                .min_w_0()
                .max_w(ui_text::space(140.0))
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .px(ui_text::space(5.0))
                .text_size(ui_text::text(9.0))
                .text_color(rgb(colors.muted))
                .when(native, |tag| {
                    tag.rounded_full().bg(rgb(colors.panel_active))
                })
                .when(!native, |tag| {
                    tag.border_1().border_color(rgb(colors.divider))
                })
                .child(folder)
        }))
        .children((project.attention > 0).then(|| {
            div()
                .flex_none()
                .text_size(ui_text::text(9.0))
                .text_color(rgb(colors.gold))
                .child(needs_you(project.attention))
        }));
    let mut where_parts = Vec::new();
    if layout.columns == Columns::Narrow {
        where_parts.extend(agents.iter().map(|(part, _)| part.clone()));
        where_parts.push(sessions_line(project, chats_known));
    }
    if layout.columns != Columns::Full
        && let Some((orchestrator, state)) = &orchestrator
    {
        where_parts.push(format!("P·Orch {orchestrator} {state}"));
    }
    if project.worktrees > 1 {
        where_parts.push(plural(project.worktrees, "worktree"));
    }
    where_parts.push(project_path(project));
    let project_cell = div()
        .id(format!("dashboard-project-{}-name", project.id))
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .flex()
        .flex_col()
        .gap(ui_text::space(2.0))
        .child(name_line)
        .child(line(
            where_parts.join(" · "),
            colors.muted,
            controls::ROW_DETAIL_TEXT,
        ))
        .test_support();

    let activity_cell = widths.activity.map(|width| {
        let first = if agents.is_empty() {
            line("No agents", colors.muted, controls::ROW_DETAIL_TEXT).into_any_element()
        } else {
            // One line of whole parts: one that does not fit wraps out of sight rather than
            // being cut in the middle.
            div()
                .w_full()
                .min_w_0()
                .h(ui_text::space(14.0))
                .line_height(ui_text::space(14.0))
                .overflow_hidden()
                .whitespace_nowrap()
                .flex()
                .flex_wrap()
                .gap_x(ui_text::space(6.0))
                .text_size(ui_text::text(controls::ROW_DETAIL_TEXT))
                .children(agents.iter().map(|(text, color)| {
                    div()
                        .flex_none()
                        .text_color(rgb(*color))
                        .child(text.clone())
                }))
                .into_any_element()
        };
        cell(width).child(first).child(line(
            sessions_line(project, chats_known),
            colors.muted,
            controls::ROW_DETAIL_TEXT,
        ))
    });
    let tasks_cell = cell(widths.tasks).children(match tasks_words(project.tasks) {
        None => vec![line("No tasks", colors.muted, controls::ROW_DETAIL_TEXT).into_any_element()],
        Some((done, open)) => vec![
            line(
                format!("{done} · {open}"),
                if project.tasks.in_progress > 0 {
                    colors.text
                } else {
                    colors.muted
                },
                controls::ROW_DETAIL_TEXT,
            )
            .into_any_element(),
            progress_bar(project.tasks, colors),
        ],
    });
    let orchestrator_cell = widths.orchestrator.map(|width| match &orchestrator {
        None => cell(width).child(line("None", colors.muted, controls::ROW_DETAIL_TEXT)),
        Some((what, state)) => cell(width)
            .child(line(what.clone(), colors.text, controls::ROW_DETAIL_TEXT))
            .child(line(
                state.clone(),
                if matches!(
                    project.orchestrator,
                    Some(ProjectOrchestrator::Terminal {
                        activity: Some(AgentActivity::Working)
                    }) | Some(ProjectOrchestrator::Chat {
                        state: ChatState::Running,
                        ..
                    })
                ) {
                    colors.working
                } else {
                    colors.muted
                },
                controls::ROW_DETAIL_TEXT,
            )),
    });
    let last_cell = cell(widths.last)
        .id(format!("dashboard-project-{}-last", project.id))
        .child(line(
            age(data.now, project.last_activity).unwrap_or_else(|| "—".to_owned()),
            colors.muted,
            controls::ROW_DETAIL_TEXT,
        ))
        .test_support();

    let open = on_action.clone();
    let id = project.id.clone();
    let main = behavior::action(
        format!("dashboard-project-{}", project.id),
        project_name_for_reader(project, chats_known, data.now, colors),
        colors,
    )
    .flex_1()
    .min_w_0()
    .flex()
    .items_center()
    .justify_start()
    .gap(ui_text::space(CELL_GAP))
    .pl(ui_text::space(ROW_PAD))
    .py(ui_text::space(4.0))
    .when(native, |row| {
        row.rounded(controls::radius(controls::LIST_ROW_RADIUS))
    })
    .hover(move |style| {
        controls::hovered(style, controls::row_hover(false, colors), |style| {
            style.bg(rgb(colors.panel_active))
        })
    })
    .child(
        div()
            .flex_none()
            .w(ui_text::space(MARK))
            .text_size(ui_text::text(8.0))
            .text_color(rgb(mark_color))
            .child(mark),
    )
    .child(project_cell)
    .children(activity_cell)
    .child(tasks_cell)
    .children(orchestrator_cell)
    .child(last_cell)
    .on_click(cx.listener(move |view, _, window, cx| {
        open(
            view,
            DashboardAction::Open(Target::Project { id: id.clone() }),
            window,
            cx,
        )
    }));
    let small =
        |suffix: &'static str, label: &'static str, panel: PanelKind, cx: &mut Context<V>| {
            let on_action = on_action.clone();
            let project_id = project.id.clone();
            let id = format!("dashboard-project-{}-{suffix}", project.id);
            let button =
                behavior::action(id.clone(), format!("{label} of {}", project.name), colors)
                    .flex_none()
                    .px(ui_text::space(6.0))
                    .py(ui_text::space(1.0))
                    .text_size(ui_text::text(9.0))
                    .text_color(rgb(colors.muted))
                    .when(native, |button| button.rounded_full())
                    .when(!native, |button| {
                        button.border_1().border_color(rgb(colors.divider))
                    })
                    .hover(move |style| {
                        style
                            .text_color(rgb(colors.text))
                            .bg(rgb(colors.panel_active))
                    })
                    .child(ui_text::cased(label))
                    .on_click(cx.listener(move |view, _, window, cx| {
                        on_action(
                            view,
                            DashboardAction::ProjectPanel {
                                project_id: project_id.clone(),
                                panel,
                            },
                            window,
                            cx,
                        )
                    }));
            crate::form_input::control_element(id, button)
        };
    let tasks = small("tasks", "Tasks", PanelKind::Tasks, cx);
    let sessions = small("sessions", "Sessions", PanelKind::Shells, cx);
    div()
        .w_full()
        .min_w_0()
        .flex()
        .items_center()
        .gap(ui_text::space(CELL_GAP))
        .pr(ui_text::space(ROW_PAD))
        .border_b_1()
        .border_color(rgb(if native {
            theme::mix(colors.divider, colors.panel, 0.5)
        } else {
            colors.divider
        }))
        .child(crate::form_input::control_element(
            format!("dashboard-project-{}", project.id),
            main,
        ))
        .child(
            div()
                .flex_none()
                .w(ui_text::space(widths.actions))
                .flex()
                .items_center()
                .justify_end()
                .gap(ui_text::space(4.0))
                .child(tasks)
                .child(sessions),
        )
        .into_any_element()
}

/// Done, then in progress, then to do, across a thin track.
fn progress_bar(tasks: TaskCounts, colors: Palette) -> AnyElement {
    let total = tasks.total().max(1) as f32;
    let part = |count: usize, color: u32| {
        div()
            .h_full()
            .w(gpui::relative(count as f32 / total))
            .bg(rgb(color))
    };
    div()
        .w_full()
        .h(ui_text::space(4.0))
        .my(ui_text::space(1.0))
        .flex()
        .overflow_hidden()
        .bg(rgb(colors.divider))
        .when(ui_text::is_native(), |track| track.rounded_full())
        .child(part(tasks.done, colors.cyan))
        .child(part(
            tasks.in_progress,
            theme::mix(colors.working, colors.divider, 0.35),
        ))
        .into_any_element()
}

fn section(
    id: &'static str,
    title: &str,
    count: Option<String>,
    colors: Palette,
) -> gpui::Stateful<Div> {
    let heading = if ui_text::is_native() {
        controls::section_heading(ui_text::cased(title.to_owned()), colors)
            .px(ui_text::space(ROW_PAD))
            .children(count.map(|count| div().font_weight(FontWeight::NORMAL).child(count)))
            .into_any_element()
    } else {
        div()
            .flex()
            .items_center()
            .gap(ui_text::space(6.0))
            .px(ui_text::space(ROW_PAD))
            .pt(ui_text::space(8.0))
            .pb(ui_text::space(4.0))
            .text_size(ui_text::text(9.0))
            .text_color(rgb(colors.magenta))
            .child(ui_text::cased(title.to_owned()))
            .children(count.map(|count| {
                div()
                    .font_family(ui_text::mono_family())
                    .text_color(rgb(colors.muted))
                    .child(count)
            }))
            .into_any_element()
    };
    div()
        .id(id)
        .flex_none()
        .flex()
        .flex_col()
        .min_w_0()
        .child(heading)
}

/// "3", or "1 of 3" while a search hides some.
fn count_of(shown: usize, total: usize) -> String {
    if shown == total {
        total.to_string()
    } else {
        format!("{shown} of {total}")
    }
}

fn note(text: impl Into<gpui::SharedString>, color: u32) -> AnyElement {
    div()
        .px(ui_text::space(ROW_PAD))
        .py(ui_text::space(4.0))
        .text_size(ui_text::text(10.0))
        .text_color(rgb(color))
        .child(text.into())
        .into_any_element()
}

/// What a rail list says when the search hid every row it had.
fn no_match(query: &str) -> String {
    format!("Nothing here matches “{}”.", query.trim())
}

fn empty_state(symbol: &'static str, text: &'static str, colors: Palette) -> AnyElement {
    if ui_text::is_native() {
        return controls::empty_state(symbol, text, colors).into_any_element();
    }
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .p(ui_text::space(24.0))
        .text_color(rgb(colors.muted))
        .child(text)
        .into_any_element()
}

struct Row {
    id: String,
    /// The row's accessible name: what it is and where clicking goes.
    name: String,
    mark: Option<(&'static str, u32)>,
    title: String,
    detail: Option<String>,
    action: DashboardAction,
}

fn row<V: 'static>(
    row: Row,
    on_action: impl Fn(&mut V, DashboardAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let native = ui_text::is_native();
    let action = row.action.clone();
    let control = behavior::action(row.id.clone(), row.name, colors)
        .w_full()
        .min_w_0()
        .flex()
        .items_start()
        .justify_start()
        .gap(ui_text::space(6.0))
        .px(ui_text::space(ROW_PAD))
        .py(ui_text::space(3.0))
        .when(!native, |row| {
            row.border_b_1().border_color(rgb(colors.divider))
        })
        .when(native, |row| {
            row.rounded(controls::radius(controls::LIST_ROW_RADIUS))
        })
        .hover(move |style| {
            controls::hovered(style, controls::row_hover(false, colors), |style| {
                style.bg(rgb(colors.panel_active))
            })
        })
        .children(row.mark.map(|(mark, color)| {
            controls::on_first_line(
                div()
                    .w(ui_text::space(10.0))
                    .text_size(ui_text::text(controls::ROW_TITLE_TEXT))
                    .text_color(rgb(color))
                    .child(mark),
                15.0,
            )
        }))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(ui_text::space(1.0))
                .child(line(row.title, colors.text, controls::ROW_TITLE_TEXT))
                .children(
                    row.detail
                        .map(|detail| line(detail, colors.muted, controls::ROW_DETAIL_TEXT)),
                ),
        )
        .on_click(
            cx.listener(move |view, _, window, cx| on_action(view, action.clone(), window, cx)),
        );
    crate::form_input::control_element(row.id, control)
}

fn more(count: usize, colors: Palette) -> Option<AnyElement> {
    (count > LIST_LIMIT).then(|| note(format!("{} more", count - LIST_LIMIT), colors.muted))
}

fn attention_section<V: 'static>(
    data: &DashboardData<'_>,
    lists: &Lists<'_>,
    on_action: impl Fn(&mut V, DashboardAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let all = &data.overview.attention;
    let items = &lists.attention;
    let mut section = section(
        "dashboard-attention",
        "Needs you",
        Some(count_of(items.len(), all.len())),
        colors,
    );
    if all.is_empty() {
        section = section.child(note(quiet_attention(data.overview), colors.muted));
    } else if items.is_empty() {
        section = section.child(note(no_match(data.query), colors.muted));
    }
    // Every one is listed: these are what the dashboard is for.
    for (index, item) in items {
        let (title, detail) = attention_copy(item, data.now);
        let (mark, color) = match item.kind {
            AttentionKind::Waiting => ("◌", colors.gold),
            AttentionKind::ChatFailed { .. } => ("✕", colors.gold),
            AttentionKind::Automation { .. } => ("!", colors.gold),
        };
        let destination = match &item.target {
            Target::Automations { .. } => "opens Automations",
            _ => "opens it",
        };
        section = section.child(row(
            Row {
                id: format!("dashboard-attention-{index}"),
                name: format!("{title}, {detail}; {destination}"),
                mark: Some((mark, color)),
                title,
                detail: Some(detail),
                action: DashboardAction::Open(item.target.clone()),
            },
            on_action.clone(),
            cx,
        ));
    }
    section.into_any_element()
}

fn work_row<V: 'static>(
    id: String,
    work: &Work,
    finished: bool,
    now: u64,
    on_action: impl Fn(&mut V, DashboardAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let title = session_name(&work.session);
    let mut detail = vec![session_place(&work.session)];
    if finished {
        if let Some(when) = age(now, work.since_unix) {
            detail.push(format!("finished {when}"));
        }
    } else {
        if let Some(since) = work.since_unix {
            detail.push(format!("for {}", short_duration(now.saturating_sub(since))));
        }
        if work.subagents > 0 {
            detail.push(plural(work.subagents, "subagent"));
        }
    }
    let detail = detail.join(" · ");
    row(
        Row {
            id,
            name: format!(
                "{title}, {}, {detail}; opens it",
                if finished { "finished" } else { "working" }
            ),
            mark: Some(if finished {
                ("✓", colors.cyan)
            } else {
                ("●", colors.working)
            }),
            title,
            detail: Some(detail),
            action: DashboardAction::Open(work.session.target.clone()),
        },
        on_action,
        cx,
    )
}

fn work_section<V: 'static>(
    data: &DashboardData<'_>,
    lists: &Lists<'_>,
    on_action: impl Fn(&mut V, DashboardAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let all = &data.overview.working;
    let working = &lists.working;
    let mut section = section(
        "dashboard-working",
        "Working now",
        Some(count_of(working.len(), all.len())),
        colors,
    );
    if all.is_empty() {
        section = section.child(note("No agent is working right now.", colors.muted));
    } else if working.is_empty() {
        section = section.child(note(no_match(data.query), colors.muted));
    }
    for (index, work) in working.iter().take(LIST_LIMIT) {
        section = section.child(work_row(
            format!("dashboard-working-{index}"),
            work,
            false,
            data.now,
            on_action.clone(),
            cx,
        ));
    }
    section
        .children(more(working.len(), colors))
        .into_any_element()
}

fn finished_section<V: 'static>(
    data: &DashboardData<'_>,
    lists: &Lists<'_>,
    on_action: impl Fn(&mut V, DashboardAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> Option<AnyElement> {
    let colors = theme::palette(cx);
    let all = &data.overview.finished;
    let finished = &lists.finished;
    if finished.is_empty() {
        return None;
    }
    let mut section = section(
        "dashboard-finished",
        "Finished in the last hour",
        Some(count_of(finished.len(), all.len())),
        colors,
    );
    for (index, work) in finished.iter().take(LIST_LIMIT) {
        section = section.child(work_row(
            format!("dashboard-finished-{index}"),
            work,
            true,
            data.now,
            on_action.clone(),
            cx,
        ));
    }
    Some(
        section
            .children(more(finished.len(), colors))
            .into_any_element(),
    )
}

fn tasks_section<V: 'static>(
    data: &DashboardData<'_>,
    lists: &Lists<'_>,
    on_action: impl Fn(&mut V, DashboardAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let all = &data.overview.tasks_in_progress;
    let tasks = &lists.tasks;
    let mut section = section(
        "dashboard-tasks",
        "Tasks in progress",
        Some(count_of(tasks.len(), all.len())),
        colors,
    );
    if all.is_empty() {
        section = section.child(note("No task is in progress.", colors.muted));
    } else if tasks.is_empty() {
        section = section.child(note(no_match(data.query), colors.muted));
    }
    for task in tasks.iter().take(LIST_LIMIT) {
        section = section.child(task_row(task, data.now, on_action.clone(), cx));
    }
    section
        .children(more(tasks.len(), colors))
        .into_any_element()
}

fn task_row<V: 'static>(
    task: &TaskOverview,
    now: u64,
    on_action: impl Fn(&mut V, DashboardAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let mut detail = place(
        Some(&task.project_id),
        Some(&task.project_name),
        task.branch.as_deref(),
    );
    if let Some(when) = age(now, Some(task.updated_at)) {
        detail = format!("{detail} · updated {when}");
    }
    row(
        Row {
            id: format!("dashboard-task-{}", task.id),
            name: format!("Task {}, in progress, {detail}; opens it", task.title),
            mark: Some(("◐", colors.working)),
            title: task.title.clone(),
            detail: Some(detail),
            action: DashboardAction::Open(Target::Task {
                id: task.id.clone(),
            }),
        },
        on_action,
        cx,
    )
}

fn automations_section<V: 'static>(
    data: &DashboardData<'_>,
    lists: &Lists<'_>,
    on_action: impl Fn(&mut V, DashboardAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    match &data.overview.automations {
        Automations::NotRead => section("dashboard-automations", "Automations", None, colors)
            .child(note("Reading automations…", colors.muted))
            .into_any_element(),
        Automations::Unreadable(error) => {
            section("dashboard-automations", "Automations", None, colors)
                .child(note(
                    format!("Automations could not be read: {}", one_line(error)),
                    colors.gold,
                ))
                .into_any_element()
        }
        Automations::Read { total: 0, .. } => section(
            "dashboard-automations",
            "Automations",
            Some("0".into()),
            colors,
        )
        .child(note("No automations.", colors.muted))
        .into_any_element(),
        Automations::Read { total, paused, .. } => {
            let shown = &lists.automations;
            let mut count = count_of(shown.len(), *total);
            if *paused > 0 {
                count = format!("{count} · {paused} paused");
            }
            let mut section = section("dashboard-automations", "Automations", Some(count), colors);
            if shown.is_empty() {
                section = section.child(note(no_match(data.query), colors.muted));
            }
            for item in shown.iter().take(LIST_LIMIT) {
                let detail = automation_detail(item, data.now);
                let failed = matches!(
                    item.last_run,
                    Some((Outcome::Failed | Outcome::Missed | Outcome::Uncertain, _))
                );
                let mark = if item.paused {
                    ("‖", colors.muted)
                } else if failed {
                    ("!", colors.gold)
                } else {
                    ("◷", colors.muted)
                };
                section = section.child(row(
                    Row {
                        id: format!("dashboard-automation-{}", item.id),
                        name: format!("Automation {}, {detail}; opens Automations", item.title),
                        mark: Some(mark),
                        title: item.title.clone(),
                        detail: Some(detail),
                        action: DashboardAction::Open(Target::Automations {
                            project_id: item.project_id.clone(),
                        }),
                    },
                    on_action.clone(),
                    cx,
                ));
            }
            section
                .children(more(shown.len(), colors))
                .into_any_element()
        }
    }
}

#[cfg(test)]
mod tests;
