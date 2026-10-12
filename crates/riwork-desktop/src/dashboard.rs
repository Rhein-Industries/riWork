//! The global dashboard: what every project on this Mac is doing, what waits for the user,
//! and how far the tasks have come, with each row opening the exact session, task, project
//! or automation it names. It draws `global_overview::Overview`, which the window builds
//! from the sessions, activity and chats it samples anyway; drawing or refreshing it never
//! starts an agent, a chat or a chat host. Only the Chat controls do, when clicked.

use gpui::{AnyElement, Context, Div, FontWeight, IntoElement, Window, div, prelude::*, rgb};

use crate::{
    activity::AgentActivity,
    behavior_controls as behavior,
    chat::model::{ChatState, OrchestratorScope, Provider},
    controls,
    global_overview::{
        Attention, AttentionKind, Automations, GlobalOrchestrator, Overview, ProjectOrchestrator,
        ProjectOverview, SessionSummary, Target, TaskCounts, TaskOverview, Work, short_duration,
    },
    layouts::PanelKind,
    schedules::Outcome,
    session_catalog,
    settings::OrchestratorRuns,
    theme::{self, Palette},
    tooltip::{self, Look},
    ui_text,
};

/// Rows a list shows before it says how many more there are.
const LIST_LIMIT: usize = 12;
/// A session sample older than this is called out as stale.
const STALE_AFTER_SECS: u64 = 30;

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
    ToggleQuietProjects,
}

/// Everything the dashboard draws, from the window's latest samples.
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
    pub show_quiet: bool,
}

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

/// What an attention row says: a headline, a detail line, and its mark.
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

fn agents_line(project: &ProjectOverview, chats_known: bool) -> String {
    let mut parts = Vec::new();
    if let Some(summary) = project.agents.summary() {
        parts.push(summary);
    }
    let live = if chats_known {
        project.live_shells + project.live_chats
    } else {
        project.live_shells
    };
    parts.push(format!("{live} live"));
    parts.push(format!(
        "{}/{} tasks",
        project.tasks.done,
        project.tasks.total()
    ));
    if project.tasks.in_progress > 0 {
        parts.push(format!("{} in progress", project.tasks.in_progress));
    }
    match &project.orchestrator {
        Some(ProjectOrchestrator::Terminal { activity }) => {
            parts.push(format!("P·Orch {}", activity_word(*activity)))
        }
        Some(ProjectOrchestrator::Chat { state, .. }) => parts.push(format!(
            "P·Orch {}",
            session_catalog::status_label(state).to_lowercase()
        )),
        None => {}
    }
    parts.join(" · ")
}

/// Draw the dashboard. `on_action` gets every click.
pub fn render<V: 'static>(
    data: DashboardData<'_>,
    on_action: impl Fn(&mut V, DashboardAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let overview = data.overview;
    let native = ui_text::is_native();
    let chat = chat_copy(&overview.global, data.runs, data.chat_starting);

    let meta = match (data.sampled_at, data.sample_error) {
        (_, Some(error)) => format!("Sessions could not be read: {}", one_line(error)),
        (None, None) => "Reading sessions…".to_owned(),
        (Some(at), None) => {
            let projects = format!(
                "{} · {} active",
                plural(overview.totals.projects, "project"),
                overview.totals.active_projects
            );
            let age = data.now.saturating_sub(at);
            if age > STALE_AFTER_SECS {
                format!("{projects} · last read {} ago", short_duration(age))
            } else {
                format!("{projects} · live")
            }
        }
    };
    let meta_warning = data.sample_error.is_some()
        || data
            .sampled_at
            .is_some_and(|at| data.now.saturating_sub(at) > STALE_AFTER_SECS);

    let header = header(&meta, meta_warning, &chat, on_action.clone(), cx);

    let body = if data.sampled_at.is_none() {
        // Nothing has been read yet: no figure is shown rather than a zero that was not
        // counted. The header says why when the read failed.
        div()
            .flex_1()
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
        let left = div()
            .id("dashboard-column-now")
            .flex()
            .flex_col()
            .flex_grow(1.0)
            .flex_basis(ui_text::space(300.0))
            .min_w(ui_text::space(240.0))
            .gap(ui_text::space(4.0))
            .child(attention_section(&data, on_action.clone(), cx))
            .child(work_section(&data, on_action.clone(), cx))
            .children(finished_section(&data, on_action.clone(), cx))
            .child(chat_section(&chat, on_action.clone(), cx));
        let right = div()
            .id("dashboard-column-progress")
            .flex()
            .flex_col()
            .flex_grow(1.0)
            .flex_basis(ui_text::space(300.0))
            .min_w(ui_text::space(240.0))
            .gap(ui_text::space(4.0))
            .child(projects_section(&data, on_action.clone(), cx))
            .child(tasks_section(&data, on_action.clone(), cx))
            .child(automations_section(&data, on_action.clone(), cx));
        div()
            .flex()
            .flex_col()
            .gap(ui_text::space(10.0))
            .child(tiles(&data, colors))
            .child(
                div()
                    .id("dashboard-columns")
                    .flex()
                    .flex_wrap()
                    .items_start()
                    .gap(ui_text::space(12.0))
                    .child(left)
                    .child(right),
            )
            .into_any_element()
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
        .child(header)
        .child(
            div()
                .id("dashboard-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .px(ui_text::space(controls::LIST_MARGIN))
                .pt(ui_text::space(if native { 2.0 } else { 8.0 }))
                .pb(ui_text::space(16.0))
                .child(body),
        )
        .into_any_element()
}

fn header<V: 'static>(
    meta: &str,
    warning: bool,
    chat: &ChatCopy,
    on_action: impl Fn(&mut V, DashboardAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let native = ui_text::is_native();
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
    let nav = navigation(chat, on_action, cx);
    if native {
        // A stale or failed read says so in the signal color, where the meta line is.
        let mut header_colors = colors;
        if warning {
            header_colors.muted = colors.gold;
        }
        return controls::panel_header(
            "Global dashboard",
            Some(meta.to_owned().into()),
            [nav, refresh],
            header_colors,
        )
        .into_any_element();
    }
    div()
        .flex_none()
        .min_h(ui_text::space(32.0))
        .flex()
        .flex_wrap()
        .items_center()
        .gap(ui_text::space(8.0))
        .px(ui_text::space(10.0))
        .py(ui_text::space(4.0))
        .border_b_1()
        .border_color(rgb(colors.divider))
        .child(
            div()
                .flex_1()
                .min_w(ui_text::space(140.0))
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_color(rgb(colors.magenta))
                        .child(ui_text::cased("Global dashboard")),
                )
                .child(
                    div()
                        .text_size(ui_text::text(9.0))
                        .text_color(rgb(if warning { colors.gold } else { colors.muted }))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(meta.to_owned()),
                ),
        )
        .child(nav)
        .child(refresh)
        .into_any_element()
}

/// Dashboard | Chat: the two places the global control leads, the dashboard being this one.
fn navigation<V: 'static>(
    chat: &ChatCopy,
    on_action: impl Fn(&mut V, DashboardAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let native = ui_text::is_native();
    let segment = |selected: bool, label: &'static str| {
        div()
            .flex_none()
            .px(ui_text::space(8.0))
            .py(ui_text::space(2.0))
            .text_size(ui_text::text(if native { 11.0 } else { 9.0 }))
            .when(!native, |segment| {
                segment
                    .border_1()
                    .border_color(rgb(if selected {
                        colors.cyan
                    } else {
                        colors.divider
                    }))
                    .text_color(rgb(if selected { colors.cyan } else { colors.text }))
            })
            .when(native, |segment| {
                segment
                    .rounded_full()
                    .bg(rgb(if selected {
                        controls::raised(colors)
                    } else {
                        colors.panel_active
                    }))
                    .text_color(rgb(if selected { colors.text } else { colors.muted }))
                    .when(selected, |segment| segment.shadow_sm())
            })
            .child(ui_text::cased(label))
    };
    let chat_button = behavior::action(
        "dashboard-nav-chat",
        format!("Chat: {}", chat.action),
        colors,
    )
    .disabled(!chat.enabled)
    .flex_none()
    .when(native, |button| button.rounded_full())
    .hover(move |style| {
        controls::hovered(style, colors.divider, |style| {
            style.bg(rgb(colors.panel_active))
        })
    })
    .child(segment(false, "Chat"))
    .child(tooltip::anchor(chat.action.clone(), Look::Control))
    .on_click(cx.listener(move |view, _, window, cx| {
        on_action(view, DashboardAction::GlobalChat, window, cx)
    }));
    div()
        .id("dashboard-navigation")
        .flex_none()
        .flex()
        .items_center()
        .gap(ui_text::space(2.0))
        .when(native, |track| {
            track
                .p(ui_text::space(2.0))
                .rounded_full()
                .bg(rgb(colors.panel_active))
        })
        // This page: shown as the selected segment, with nothing to click.
        .child(segment(true, "Dashboard"))
        .child(crate::form_input::control_element(
            "dashboard-nav-chat",
            chat_button,
        ))
        .into_any_element()
}

fn tiles(data: &DashboardData<'_>, colors: Palette) -> AnyElement {
    let totals = &data.overview.totals;
    let chats_known = data.overview.chats_known;
    let waiting = data
        .overview
        .attention
        .iter()
        .filter(|item| matches!(item.kind, AttentionKind::Waiting))
        .count();
    let failed = totals.attention - waiting;
    let working_detail = match (totals.agents.working, totals.agents.subagents) {
        (0, _) => "No agent is working".to_owned(),
        (_, 0) => match data
            .overview
            .projects
            .iter()
            .filter(|project| project.agents.working > 0)
            .count()
        {
            // Only the global orchestrator, which belongs to no project.
            0 => "Global scope".to_owned(),
            projects => format!("In {}", plural(projects, "project")),
        },
        (_, subagents) => format!("With {}", plural(subagents, "subagent")),
    };
    let attention_detail = match (waiting, failed) {
        (0, 0) => "Nothing waits for you".to_owned(),
        (waiting, 0) => format!("{waiting} waiting"),
        (0, failed) => format!("{failed} failed"),
        (waiting, failed) => format!("{waiting} waiting · {failed} failed"),
    };
    let live_detail = if chats_known {
        format!(
            "{} · {}",
            plural(totals.live_shells, "terminal"),
            plural(totals.live_chats, "chat")
        )
    } else {
        format!("{} · chats unknown", plural(totals.live_shells, "terminal"))
    };
    div()
        .id("dashboard-tiles")
        .flex()
        .flex_wrap()
        .gap(ui_text::space(8.0))
        .px(ui_text::space(
            controls::PANEL_INSET - controls::LIST_MARGIN,
        ))
        .child(tile(
            "dashboard-tile-working",
            "Working",
            totals.agents.working.to_string(),
            working_detail,
            (totals.agents.working > 0).then_some(colors.working),
            None,
            colors,
        ))
        .child(tile(
            "dashboard-tile-attention",
            "Needs you",
            totals.attention.to_string(),
            attention_detail,
            (totals.attention > 0).then_some(colors.gold),
            None,
            colors,
        ))
        .child(tile(
            "dashboard-tile-live",
            "Live sessions",
            (totals.live_shells + if chats_known { totals.live_chats } else { 0 }).to_string(),
            live_detail,
            None,
            None,
            colors,
        ))
        .child(tile(
            "dashboard-tile-tasks",
            "Tasks",
            format!("{}/{}", totals.tasks.done, totals.tasks.total()),
            format!(
                "{} in progress · {} to do",
                totals.tasks.in_progress, totals.tasks.todo
            ),
            None,
            Some(totals.tasks),
            colors,
        ))
        .into_any_element()
}

fn tile(
    id: &'static str,
    label: &'static str,
    value: String,
    detail: String,
    accent: Option<u32>,
    progress: Option<TaskCounts>,
    colors: Palette,
) -> AnyElement {
    let native = ui_text::is_native();
    div()
        .id(id)
        .flex()
        .flex_col()
        .flex_grow(1.0)
        .flex_basis(ui_text::space(130.0))
        .min_w(ui_text::space(110.0))
        .gap(ui_text::space(2.0))
        .px(ui_text::space(10.0))
        .py(ui_text::space(8.0))
        .when(native, |tile| {
            tile.rounded(controls::radius(controls::ROW_RADIUS))
                .bg(rgb(colors.panel_active))
        })
        .when(!native, |tile| {
            tile.border_1()
                .border_color(rgb(accent.unwrap_or(colors.divider)))
        })
        .child(
            div()
                .text_size(ui_text::text(9.0))
                .text_color(rgb(colors.muted))
                .when(native, |label| label.font_weight(FontWeight::SEMIBOLD))
                .child(ui_text::cased(label)),
        )
        .child(
            div()
                .text_size(ui_text::text(20.0))
                .font_family(ui_text::mono_family())
                .text_color(rgb(accent.unwrap_or(colors.text)))
                .child(value),
        )
        .children(progress.map(|tasks| progress_bar(tasks, colors)))
        .child(
            div()
                .text_size(ui_text::text(9.0))
                .text_color(rgb(colors.muted))
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(detail),
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
        .my(ui_text::space(2.0))
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
            .children(count.map(|count| div().font_weight(FontWeight::NORMAL).child(count)))
            .into_any_element()
    } else {
        div()
            .flex()
            .items_center()
            .gap(ui_text::space(6.0))
            .px(ui_text::space(
                controls::PANEL_INSET - controls::LIST_MARGIN,
            ))
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
    div().id(id).flex().flex_col().min_w_0().child(heading)
}

fn note(text: impl Into<gpui::SharedString>, color: u32) -> AnyElement {
    div()
        .px(ui_text::space(
            controls::PANEL_INSET - controls::LIST_MARGIN,
        ))
        .py(ui_text::space(4.0))
        .text_size(ui_text::text(10.0))
        .text_color(rgb(color))
        .child(text.into())
        .into_any_element()
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
    detail_color: Option<u32>,
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
    let line = |text: String, color: u32, size: f32| {
        div()
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .text_size(ui_text::text(size))
            .text_color(rgb(color))
            .child(text)
    };
    let control = behavior::action(row.id.clone(), row.name, colors)
        .w_full()
        .min_w_0()
        .flex()
        .items_start()
        .gap(ui_text::space(6.0))
        .px(ui_text::space(
            controls::PANEL_INSET - controls::LIST_MARGIN,
        ))
        .py(ui_text::space(4.0))
        .when(!native, |row| {
            row.border_b_1().border_color(rgb(colors.divider))
        })
        .hover(move |style| {
            controls::hovered(style, controls::row_hover(false, colors), |style| {
                style.bg(rgb(colors.panel_active))
            })
        })
        .map(|row| controls::native(row, |row| controls::list_row(row, false, colors)))
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
                .children(row.detail.map(|detail| {
                    line(
                        detail,
                        row.detail_color.unwrap_or(colors.muted),
                        controls::ROW_DETAIL_TEXT,
                    )
                })),
        )
        .on_click(
            cx.listener(move |view, _, window, cx| on_action(view, action.clone(), window, cx)),
        );
    crate::form_input::control_element(row.id, control)
}

fn more(count: usize, colors: Palette) -> Option<AnyElement> {
    (count > LIST_LIMIT).then(|| note(format!("{} more", count - LIST_LIMIT), colors.muted))
}

fn session_target_action(target: &Target) -> DashboardAction {
    DashboardAction::Open(target.clone())
}

fn attention_section<V: 'static>(
    data: &DashboardData<'_>,
    on_action: impl Fn(&mut V, DashboardAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let items = &data.overview.attention;
    let mut section = section(
        "dashboard-attention",
        "Needs attention",
        Some(items.len().to_string()),
        colors,
    );
    if items.is_empty() {
        section = section.child(note(quiet_attention(data.overview), colors.muted));
    }
    // Every one is listed: these are what the dashboard is for.
    for (index, item) in items.iter().enumerate() {
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
                detail_color: None,
                action: session_target_action(&item.target),
            },
            on_action.clone(),
            cx,
        ));
    }
    section.into_any_element()
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
            detail_color: None,
            action: session_target_action(&work.session.target),
        },
        on_action,
        cx,
    )
}

fn work_section<V: 'static>(
    data: &DashboardData<'_>,
    on_action: impl Fn(&mut V, DashboardAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let working = &data.overview.working;
    let mut section = section(
        "dashboard-working",
        "Working now",
        Some(working.len().to_string()),
        colors,
    );
    if working.is_empty() {
        section = section.child(note("No agent is working right now.", colors.muted));
    }
    for (index, work) in working.iter().take(LIST_LIMIT).enumerate() {
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
    on_action: impl Fn(&mut V, DashboardAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> Option<AnyElement> {
    let colors = theme::palette(cx);
    let finished = &data.overview.finished;
    if finished.is_empty() {
        return None;
    }
    let mut section = section(
        "dashboard-finished",
        "Finished in the last hour",
        Some(finished.len().to_string()),
        colors,
    );
    for (index, work) in finished.iter().take(LIST_LIMIT).enumerate() {
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

fn chat_section<V: 'static>(
    chat: &ChatCopy,
    on_action: impl Fn(&mut V, DashboardAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let kind = if chat.starts {
        controls::Button::Primary
    } else {
        controls::Button::Secondary
    };
    let button = behavior::button(
        "dashboard-global-chat",
        chat.action.clone(),
        if chat.enabled {
            kind
        } else {
            controls::Button::Disabled
        },
        colors,
    )
    .flex_none()
    .text_size(ui_text::text(10.0))
    .on_click(cx.listener(move |view, _, window, cx| {
        on_action(view, DashboardAction::GlobalChat, window, cx)
    }));
    section("dashboard-chat", "Global chat", None, colors)
        .child(note(chat.status.clone(), colors.text))
        .children(chat.note.clone().map(|text| note(text, colors.muted)))
        .child(
            div()
                .px(ui_text::space(
                    controls::PANEL_INSET - controls::LIST_MARGIN,
                ))
                .py(ui_text::space(4.0))
                .flex()
                .child(crate::form_input::control_element(
                    "dashboard-global-chat",
                    button,
                )),
        )
        .into_any_element()
}

fn projects_section<V: 'static>(
    data: &DashboardData<'_>,
    on_action: impl Fn(&mut V, DashboardAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let projects = &data.overview.projects;
    let quiet = projects.iter().filter(|project| project.is_quiet()).count();
    let shown: Vec<&ProjectOverview> = projects
        .iter()
        .filter(|project| data.show_quiet || !project.is_quiet())
        .collect();
    let mut section = section(
        "dashboard-projects",
        "Projects",
        Some(format!(
            "{} active of {}",
            data.overview.totals.active_projects,
            projects.len()
        )),
        colors,
    );
    if projects.is_empty() {
        section = section.child(note(
            "No projects yet. Add one from the Projects panel.",
            colors.muted,
        ));
    } else if shown.is_empty() {
        section = section.child(note(
            "No project has a live session or an open task.",
            colors.muted,
        ));
    }
    for project in shown {
        section = section.child(project_row(
            project,
            data.overview.chats_known,
            on_action.clone(),
            cx,
        ));
    }
    if quiet > 0 {
        let toggle = on_action.clone();
        let label = if data.show_quiet {
            "Hide quiet projects".to_owned()
        } else {
            format!("Show {} quiet", plural(quiet, "project"))
        };
        let button = behavior::action("dashboard-toggle-quiet", label.clone(), colors)
            .px(ui_text::space(
                controls::PANEL_INSET - controls::LIST_MARGIN,
            ))
            .py(ui_text::space(4.0))
            .text_size(ui_text::text(10.0))
            .text_color(rgb(colors.cyan))
            .hover(move |style| style.text_color(rgb(colors.text)))
            .child(ui_text::cased(label))
            .on_click(cx.listener(move |view, _, window, cx| {
                toggle(view, DashboardAction::ToggleQuietProjects, window, cx)
            }));
        section = section.child(div().flex().child(crate::form_input::control_element(
            "dashboard-toggle-quiet",
            button,
        )));
    }
    section.into_any_element()
}

fn project_row<V: 'static>(
    project: &ProjectOverview,
    chats_known: bool,
    on_action: impl Fn(&mut V, DashboardAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let native = ui_text::is_native();
    let detail = agents_line(project, chats_known);
    let open = on_action.clone();
    let id = project.id.clone();
    let title = match &project.folder {
        Some(folder) => format!("{} · {folder}", project.name),
        None => project.name.clone(),
    };
    let main = behavior::action(
        format!("dashboard-project-{}", project.id),
        format!("Project {}, {detail}; opens the project", project.name),
        colors,
    )
    .flex_1()
    .min_w_0()
    .flex()
    .flex_col()
    .gap(ui_text::space(2.0))
    .py(ui_text::space(2.0))
    .child(
        div()
            .flex()
            .items_center()
            .gap(ui_text::space(6.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(ui_text::text(controls::ROW_TITLE_TEXT))
                    .text_color(rgb(colors.text))
                    .child(title),
            )
            .children((project.attention > 0).then(|| {
                div()
                    .flex_none()
                    .text_size(ui_text::text(9.0))
                    .text_color(rgb(colors.gold))
                    .child(format!("{} need you", project.attention))
            })),
    )
    .child(
        div()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .text_size(ui_text::text(controls::ROW_DETAIL_TEXT))
            .text_color(rgb(if project.agents.working > 0 {
                colors.working
            } else {
                colors.muted
            }))
            .child(detail),
    )
    .children((project.tasks.total() > 0).then(|| progress_bar(project.tasks, colors)))
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
        .gap(ui_text::space(6.0))
        .px(ui_text::space(
            controls::PANEL_INSET - controls::LIST_MARGIN,
        ))
        .py(ui_text::space(3.0))
        .when(!native, |row| {
            row.border_b_1().border_color(rgb(colors.divider))
        })
        .child(crate::form_input::control_element(
            format!("dashboard-project-{}", project.id),
            main,
        ))
        .child(tasks)
        .child(sessions)
        .into_any_element()
}

fn tasks_section<V: 'static>(
    data: &DashboardData<'_>,
    on_action: impl Fn(&mut V, DashboardAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let tasks = &data.overview.tasks_in_progress;
    let mut section = section(
        "dashboard-tasks",
        "Tasks in progress",
        Some(tasks.len().to_string()),
        colors,
    );
    if tasks.is_empty() {
        section = section.child(note("No task is in progress.", colors.muted));
    }
    for task in tasks.iter().take(LIST_LIMIT) {
        section = section.child(task_row(task, on_action.clone(), cx));
    }
    section
        .children(more(tasks.len(), colors))
        .into_any_element()
}

fn task_row<V: 'static>(
    task: &TaskOverview,
    on_action: impl Fn(&mut V, DashboardAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let detail = place(
        Some(&task.project_id),
        Some(&task.project_name),
        task.branch.as_deref(),
    );
    row(
        Row {
            id: format!("dashboard-task-{}", task.id),
            name: format!("Task {}, in progress, {detail}; opens it", task.title),
            mark: Some(("◐", colors.working)),
            title: task.title.clone(),
            detail: Some(detail),
            detail_color: None,
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
    on_action: impl Fn(&mut V, DashboardAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let section = section("dashboard-automations", "Automations", None, colors);
    match &data.overview.automations {
        Automations::NotRead => section
            .child(note("Reading automations…", colors.muted))
            .into_any_element(),
        Automations::Unreadable(error) => section
            .child(note(
                format!("Automations could not be read: {}", one_line(error)),
                colors.gold,
            ))
            .into_any_element(),
        Automations::Read { total: 0, .. } => section
            .child(note("No automations.", colors.muted))
            .into_any_element(),
        Automations::Read {
            total,
            paused,
            next,
        } => {
            let mut detail = vec![plural(*total, "automation")];
            if *paused > 0 {
                detail.push(format!("{paused} paused"));
            }
            let (title, project_id) = match next {
                Some(next) => (
                    format!(
                        "Next: “{}” in {}",
                        next.title,
                        short_duration(next.at.saturating_sub(data.now))
                    ),
                    next.project_id.clone(),
                ),
                None => ("Nothing is due".to_owned(), None),
            };
            section
                .child(row(
                    Row {
                        id: "dashboard-automations-next".to_owned(),
                        name: format!("{title}, {}; opens Automations", detail.join(", ")),
                        mark: Some(("◷", colors.muted)),
                        title,
                        detail: Some(detail.join(" · ")),
                        detail_color: None,
                        action: DashboardAction::Open(Target::Automations { project_id }),
                    },
                    on_action,
                    cx,
                ))
                .into_any_element()
        }
    }
}

#[cfg(test)]
mod tests;
