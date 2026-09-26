//! Searchable navigation views that share the same movable tab surface as shells.

use std::{collections::BTreeMap, path::PathBuf};

use gpui::{
    AnyElement, Context, ElementInputHandler, EntityInputHandler, FocusHandle, IntoElement,
    MouseButton, Render, Window, canvas, div, prelude::*, px, rgb,
};

use crate::{
    CYAN, DIVIDER, GOLD, MAGENTA, MUTED, PANEL, PANEL_ACTIVE, TEXT,
    layouts::PanelKind,
    sessions::{SessionMetrics, ShellKind, ShellSession},
    store::{State, TaskStatus},
};

#[derive(Clone)]
pub enum PanelAction {
    CreateProject,
    Project(String),
    OpenProject(String),
    Worktree(String),
    Task(String),
    Shell(String),
    Search,
}

pub struct PanelData<'a> {
    pub state: &'a State,
    pub project_id: &'a str,
    pub selected_worktree_id: Option<&'a str>,
    pub selected_task_id: Option<&'a str>,
    pub shells: &'a [ShellSession],
    pub shell_cwds: &'a BTreeMap<String, PathBuf>,
    pub metrics: &'a BTreeMap<String, SessionMetrics>,
    pub query: &'a str,
    pub search_focused: bool,
    pub focus: FocusHandle,
    pub control_inset: f32,
}

pub fn render_panel<V: Render + EntityInputHandler + 'static>(
    kind: PanelKind,
    data: PanelData<'_>,
    on_action: impl Fn(&mut V, PanelAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let query = data.query.trim().to_lowercase();
    let matches = |values: &[&str]| {
        query.is_empty()
            || values
                .iter()
                .any(|value| value.to_lowercase().contains(&query))
    };
    let mut rows = Vec::new();
    let mut total = 0;
    let name = match kind {
        PanelKind::Projects => "projects",
        PanelKind::Worktrees => "worktrees",
        PanelKind::Tasks => "tasks",
        PanelKind::Shells => "shells",
        PanelKind::Usage => "usage",
        PanelKind::Settings => "settings",
    };

    match kind {
        PanelKind::Usage | PanelKind::Settings => {}
        PanelKind::Projects => {
            total = data.state.projects.len();
            for project in &data.state.projects {
                let path = project.root.to_string_lossy();
                if !matches(&[&project.id, &project.name, &path]) {
                    continue;
                }
                let worktrees = data.state.worktrees_for(&project.id).len();
                let repositories = project.repository_roots.len();
                let tasks = data.state.tasks_for_project(&project.id);
                let done = tasks
                    .iter()
                    .filter(|task| task.status == TaskStatus::Done)
                    .count();
                let shells = data
                    .shells
                    .iter()
                    .filter(|shell| {
                        shell.project_id.as_deref() == Some(project.id.as_str()) && shell.alive
                    })
                    .count();
                let project_id = project.id.clone();
                let open_action = on_action.clone();
                let title = div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(line(project.name.clone(), TEXT, 11.0)),
                    )
                    .child(
                        div()
                            .id(format!("open-project-{}", project.id))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .w(px(22.0))
                            .h(px(18.0))
                            .text_color(rgb(CYAN))
                            .cursor_pointer()
                            .hover(|style| style.bg(rgb(DIVIDER)))
                            .child("↗")
                            .tooltip(|_, cx| cx.new(|_| OpenProjectTooltip).into())
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |view, _, window, cx| {
                                cx.stop_propagation();
                                open_action(
                                    view,
                                    PanelAction::OpenProject(project_id.clone()),
                                    window,
                                    cx,
                                );
                            })),
                    )
                    .into_any_element();
                rows.push(row(
                    format!("project-{}", project.id),
                    data.project_id == project.id,
                    CYAN,
                    vec![
                        title,
                        line(path.into_owned(), MUTED, 10.0),
                        line(
                            format!(
                                "{repositories} REPOS · {worktrees} WORKTREES · {done}/{} TASKS · {shells} LIVE",
                                tasks.len()
                            ),
                            MUTED,
                            10.0,
                        ),
                    ],
                    PanelAction::Project(project.id.clone()),
                    on_action.clone(),
                    cx,
                ));
            }
        }
        PanelKind::Worktrees => {
            for worktree in data.state.worktrees_for(data.project_id) {
                total += 1;
                let path = worktree.path.to_string_lossy();
                if !matches(&[&worktree.id, &worktree.branch, &path]) {
                    continue;
                }
                let tasks = data.state.tasks_for_worktree(&worktree.id);
                let done = tasks
                    .iter()
                    .filter(|task| task.status == TaskStatus::Done)
                    .count();
                let selected = data.selected_worktree_id == Some(worktree.id.as_str());
                rows.push(row(
                    format!("worktree-{}", worktree.id),
                    selected,
                    MAGENTA,
                    vec![
                        line(
                            format!(
                                "{} {}{}{}",
                                if worktree.is_primary { "◆" } else { "◇" },
                                worktree.branch,
                                if worktree.path.is_dir() {
                                    ""
                                } else {
                                    "  [missing]"
                                },
                                worktree
                                    .repository_root
                                    .as_ref()
                                    .and_then(|root| root.file_name())
                                    .map(|name| format!(" · {}", name.to_string_lossy()))
                                    .unwrap_or_default(),
                            ),
                            if selected { CYAN } else { TEXT },
                            11.0,
                        ),
                        line(path.into_owned(), MUTED, 10.0),
                        line(
                            format!("{done}/{} TASKS · {}", tasks.len(), short_id(&worktree.id)),
                            MUTED,
                            10.0,
                        ),
                    ],
                    PanelAction::Worktree(worktree.id.clone()),
                    on_action.clone(),
                    cx,
                ));
            }
        }
        PanelKind::Tasks => {
            for task in data.state.tasks_for_project(data.project_id) {
                total += 1;
                let worktree = worktree_label(data.state, task.worktree_id.as_deref());
                if !matches(&[
                    &task.id,
                    &task.title,
                    &task.details,
                    task.status.as_str(),
                    worktree,
                ]) {
                    continue;
                }
                let (mark, color) = task_mark(task.status);
                rows.push(row(
                    format!("task-{}", task.id),
                    data.selected_task_id == Some(task.id.as_str()),
                    color,
                    vec![
                        div()
                            .flex()
                            .gap(px(6.0))
                            .child(div().text_color(rgb(color)).child(mark))
                            .child(line(task.title.clone(), TEXT, 11.0))
                            .into_any_element(),
                        line(
                            format!("{} · @ {worktree}", task.status.as_str()),
                            MUTED,
                            10.0,
                        ),
                    ],
                    PanelAction::Task(task.id.clone()),
                    on_action.clone(),
                    cx,
                ));
            }
        }
        PanelKind::Shells => {
            for shell in data.shells.iter().filter(|shell| {
                shell.kind == ShellKind::Project
                    && shell.project_id.as_deref() == Some(data.project_id)
            }) {
                total += 1;
                let cwd = data.shell_cwds.get(&shell.id).unwrap_or(&shell.cwd);
                let current_worktree = data
                    .state
                    .worktrees_for(data.project_id)
                    .into_iter()
                    .filter(|worktree| cwd.starts_with(&worktree.path))
                    .max_by_key(|worktree| worktree.path.as_os_str().len());
                let label = current_worktree
                    .map(|worktree| worktree.branch.as_str())
                    .unwrap_or_else(|| worktree_label(data.state, shell.worktree_id.as_deref()));
                let path = cwd.to_string_lossy();
                let command = shell.command.as_deref().unwrap_or("shell");
                if !matches(&[
                    &shell.id,
                    label,
                    &path,
                    command,
                    if shell.alive { "live" } else { "exited" },
                ]) {
                    continue;
                }
                let metrics = data.metrics.get(&shell.id).copied().unwrap_or_default();
                rows.push(row(
                    format!("shell-{}", shell.id),
                    false,
                    CYAN,
                    vec![
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(px(6.0))
                            .child(line(
                                format!("{} · {label}", short_id(&shell.id)),
                                TEXT,
                                11.0,
                            ))
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(rgb(if shell.alive { CYAN } else { MAGENTA }))
                                    .child(if shell.alive { "● LIVE" } else { "× EXITED" }),
                            )
                            .into_any_element(),
                        line(path.into_owned(), MUTED, 10.0),
                        line(
                            format!(
                                "CPU {:.1}% · RAM {} · {command}",
                                metrics.cpu_percent,
                                format_bytes(metrics.ram_bytes)
                            ),
                            if shell.alive { CYAN } else { MUTED },
                            10.0,
                        ),
                        line(shell.id.clone(), MUTED, 10.0),
                    ],
                    PanelAction::Shell(shell.id.clone()),
                    on_action.clone(),
                    cx,
                ));
            }
        }
    }

    let count = rows.len();
    if rows.is_empty() {
        rows.push(
            div()
                .p(px(10.0))
                .text_color(rgb(MUTED))
                .child(if query.is_empty() {
                    format!("NO {}", name.to_uppercase())
                } else {
                    "NO MATCHES".to_owned()
                })
                .into_any_element(),
        );
    }

    let search_label = if data.query.is_empty() {
        "⌕ SEARCH  [CMD+F]".to_owned()
    } else {
        format!(
            "⌕ {}{}",
            data.query,
            if data.search_focused { "▌" } else { "" }
        )
    };
    let search_input = data.search_focused.then(|| {
        let focus = data.focus.clone();
        let view = cx.entity();
        canvas(
            |_, _, _| {},
            move |bounds, _, window, cx| {
                window.handle_input(&focus, ElementInputHandler::new(bounds, view.clone()), cx);
            },
        )
        .absolute()
        .inset_0()
        .into_any_element()
    });
    let search_action = on_action.clone();
    let create_action = on_action;
    let mut panel = div()
        .flex()
        .flex_col()
        .size_full()
        .min_w_0()
        .min_h_0()
        .bg(rgb(PANEL))
        .text_size(px(11.0))
        .child(
            div()
                .h(px(30.0))
                .flex_none()
                .flex()
                .border_b_1()
                .border_color(rgb(if data.search_focused { CYAN } else { DIVIDER }))
                .overflow_hidden()
                .child(div().flex_none().w(px((data.control_inset - 8.0).max(0.0))))
                .child(
                    div()
                        .id(format!("{name}-search"))
                        .relative()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .px(px(8.0))
                        .flex()
                        .items_center()
                        .text_color(rgb(if data.search_focused { TEXT } else { MUTED }))
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(search_label)
                        .children(search_input)
                        .on_click(cx.listener(move |view, _, window, cx| {
                            search_action(view, PanelAction::Search, window, cx);
                        })),
                )
                .children((kind == PanelKind::Projects).then(|| {
                    div()
                        .id("new-project")
                        .flex_none()
                        .h_full()
                        .px(px(8.0))
                        .flex()
                        .items_center()
                        .text_color(rgb(CYAN))
                        .cursor_pointer()
                        .hover(|style| style.bg(rgb(PANEL_ACTIVE)))
                        .child("+ PROJECT")
                        .on_click(cx.listener(move |view, _, window, cx| {
                            create_action(view, PanelAction::CreateProject, window, cx);
                        }))
                })),
        )
        .child(
            div()
                .flex_none()
                .px(px(8.0))
                .py(px(4.0))
                .text_size(px(10.0))
                .text_color(rgb(MUTED))
                .child(format!("{count:02} / {total:02} {}", name.to_uppercase())),
        )
        .child(
            div()
                .id(format!("{name}-rows"))
                .flex_1()
                .min_w_0()
                .min_h_0()
                .overflow_y_scroll()
                .children(rows),
        );

    if kind == PanelKind::Tasks
        && let Some(task) = data.selected_task_id.and_then(|id| {
            data.state
                .tasks
                .iter()
                .find(|task| task.id == id && task.project_id == data.project_id)
        })
    {
        let (_, color) = task_mark(task.status);
        panel =
            panel.child(
                div()
                    .id("task-detail")
                    .flex_none()
                    .min_h_0()
                    .max_h(px(200.0))
                    .overflow_y_scroll()
                    .border_t_1()
                    .border_color(rgb(DIVIDER))
                    .p(px(8.0))
                    .child(div().text_color(rgb(GOLD)).child("TASK DETAIL"))
                    .child(
                        div()
                            .pt(px(5.0))
                            .text_color(rgb(TEXT))
                            .child(task.title.clone()),
                    )
                    .child(div().pt(px(4.0)).text_color(rgb(color)).child(format!(
                        "{} · @ {}",
                        task.status.as_str(),
                        worktree_label(data.state, task.worktree_id.as_deref())
                    )))
                    .child(div().pt(px(6.0)).text_color(rgb(MUTED)).child(
                        if task.details.is_empty() {
                            "No details".to_owned()
                        } else {
                            task.details.clone()
                        },
                    ))
                    .child(line(task.id.clone(), MUTED, 10.0)),
            );
    }
    panel.into_any_element()
}

struct OpenProjectTooltip;

impl Render for OpenProjectTooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(8.0))
            .py(px(5.0))
            .bg(rgb(PANEL_ACTIVE))
            .border_1()
            .border_color(rgb(DIVIDER))
            .text_size(px(10.0))
            .text_color(rgb(TEXT))
            .child("Open project in another window")
    }
}

fn row<V: 'static>(
    id: String,
    selected: bool,
    accent: u32,
    children: Vec<AnyElement>,
    action: PanelAction,
    on_action: impl Fn(&mut V, PanelAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    div()
        .id(id)
        .flex()
        .flex_col()
        .gap(px(3.0))
        .min_w_0()
        .px(px(8.0))
        .py(px(6.0))
        .border_l_1()
        .border_color(rgb(if selected { accent } else { PANEL }))
        .bg(rgb(if selected { PANEL_ACTIVE } else { PANEL }))
        .hover(|element| element.bg(rgb(PANEL_ACTIVE)))
        .children(children)
        .on_click(cx.listener(move |view, _, window, cx| {
            on_action(view, action.clone(), window, cx);
        }))
        .into_any_element()
}

fn line(text: String, color: u32, size: f32) -> AnyElement {
    div()
        .min_w_0()
        .overflow_hidden()
        .text_ellipsis()
        .text_size(px(size))
        .text_color(rgb(color))
        .child(text)
        .into_any_element()
}

fn worktree_label<'a>(state: &'a State, id: Option<&str>) -> &'a str {
    id.and_then(|id| state.worktrees.iter().find(|worktree| worktree.id == id))
        .map(|worktree| worktree.branch.as_str())
        .unwrap_or("UNASSIGNED")
}

fn task_mark(status: TaskStatus) -> (&'static str, u32) {
    match status {
        TaskStatus::Todo => ("□", MUTED),
        TaskStatus::InProgress => ("◧", GOLD),
        TaskStatus::Done => ("■", CYAN),
    }
}

fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

fn format_bytes(bytes: u64) -> String {
    let mebibytes = bytes as f64 / (1024.0 * 1024.0);
    if mebibytes >= 1024.0 {
        format!("{:.1}G", mebibytes / 1024.0)
    } else {
        format!("{mebibytes:.0}M")
    }
}
