//! Searchable navigation views that share the same movable tab surface as shells.

use std::{
    cell::Cell,
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
};

use gpui::{
    AnyElement, Bounds, Context, ElementInputHandler, EntityInputHandler, FocusHandle, IntoElement,
    MouseButton, Pixels, Render, Window, canvas, div, prelude::*, px, rgb,
};

use crate::{
    activity::{ActivityCounts, AgentActivity},
    icons::{self, Icon},
    layouts::PanelKind,
    project_sort::{ProjectOrder, ProjectSort, sorted_project_indices},
    sessions::{SessionMetrics, ShellKind, ShellSession},
    store::{State, TaskStatus},
    theme::{self, Palette},
};

#[derive(Clone)]
pub enum PanelAction {
    CreateProject,
    CreateFolder,
    CreateSubfolder(String),
    EditFolder(String),
    RemoveFolder(String),
    ToggleFolder(String),
    BeginProjectDrag,
    MoveProject {
        project_id: String,
        folder_id: Option<String>,
    },
    MoveFolder {
        folder_id: String,
        parent_id: Option<String>,
    },
    ProjectSettings(String),
    ToggleProjectNotifications(String),
    Project(String),
    OpenProject(String),
    Worktree(String),
    Task(String),
    Shell(String),
    Search,
    ToggleProjectSortMenu,
    CloseProjectSortMenu,
    SetProjectOrder(ProjectOrder),
}

pub struct PanelData<'a> {
    pub state: &'a State,
    pub project_id: &'a str,
    pub selected_worktree_id: Option<&'a str>,
    pub selected_task_id: Option<&'a str>,
    pub shells: &'a [ShellSession],
    pub shell_cwds: &'a BTreeMap<String, PathBuf>,
    pub metrics: &'a BTreeMap<String, SessionMetrics>,
    pub activity: &'a BTreeMap<String, AgentActivity>,
    pub query: &'a str,
    pub search_focused: bool,
    pub focus: FocusHandle,
    pub control_inset: f32,
    pub collapsed_folders: &'a HashSet<String>,
    pub state_home: &'a Path,
    pub project_order: ProjectOrder,
    pub project_last_edits: &'a BTreeMap<String, u64>,
    pub project_sort_menu_open: bool,
}

#[derive(Clone, Debug)]
pub enum ProjectDragKind {
    Project(String),
    Folder(String),
}

#[derive(Clone, Debug)]
pub struct DraggedProjectItem {
    pub kind: ProjectDragKind,
    pub label: String,
    pub state_home: PathBuf,
}

impl Render for DraggedProjectItem {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme::palette(cx);
        div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .px(px(10.0))
            .py(px(6.0))
            .max_w(px(280.0))
            .bg(rgb(colors.panel_active))
            .border_1()
            .border_color(rgb(colors.cyan))
            .text_color(rgb(colors.text))
            .font_family("Menlo")
            .text_size(px(11.0))
            .child(match self.kind {
                ProjectDragKind::Project(_) => "◇",
                ProjectDragKind::Folder(_) => "▱",
            })
            .child(
                div()
                    .text_ellipsis()
                    .overflow_hidden()
                    .child(self.label.clone()),
            )
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ProjectTreeRow {
    Folder {
        index: usize,
        depth: usize,
        count: usize,
        collapsed: bool,
    },
    Project {
        index: usize,
        depth: usize,
    },
    Unfiled {
        count: usize,
        collapsed: bool,
    },
    Empty {
        depth: usize,
    },
}

struct ProjectTree {
    rows: Vec<ProjectTreeRow>,
    matched_projects: usize,
}

/// Recover a deterministic forest even if an older state has missing parents or cycles.
fn project_tree(
    state: &State,
    query: &str,
    collapsed: &HashSet<String>,
    project_order: &[usize],
) -> ProjectTree {
    let query = query.trim().to_lowercase();
    let folders = &state.project_folders;
    let indices: BTreeMap<_, _> = folders
        .iter()
        .enumerate()
        .map(|(index, folder)| (folder.id.as_str(), index))
        .collect();
    let mut order: Vec<_> = (0..folders.len()).collect();
    order.sort_by_key(|&index| {
        let folder = &folders[index];
        (
            folder.name.to_lowercase(),
            folder.name.clone(),
            folder.id.clone(),
        )
    });
    let mut rank = vec![0; folders.len()];
    for (position, &index) in order.iter().enumerate() {
        rank[index] = position;
    }
    let mut parents: Vec<_> = folders
        .iter()
        .enumerate()
        .map(|(index, folder)| {
            folder
                .parent_id
                .as_deref()
                .and_then(|id| indices.get(id).copied())
                .filter(|parent| *parent != index)
        })
        .collect();
    for &start in &order {
        let mut path = Vec::new();
        let mut positions = BTreeMap::new();
        let mut current = Some(start);
        while let Some(index) = current {
            if let Some(&cycle_start) = positions.get(&index) {
                let root = path[cycle_start..]
                    .iter()
                    .copied()
                    .min_by_key(|&index| rank[index])
                    .expect("a repeated folder forms a nonempty cycle");
                parents[root] = None;
                break;
            }
            positions.insert(index, path.len());
            path.push(index);
            current = parents[index];
        }
    }
    let mut children = vec![Vec::new(); folders.len()];
    let mut roots = Vec::new();
    for &index in &order {
        match parents[index] {
            Some(parent) => children[parent].push(index),
            None => roots.push(index),
        }
    }
    let mut traversal = Vec::with_capacity(folders.len());
    let mut depths = vec![0; folders.len()];
    let mut stack: Vec<_> = roots
        .iter()
        .rev()
        .copied()
        .map(|index| (index, 0))
        .collect();
    while let Some((index, depth)) = stack.pop() {
        depths[index] = depth;
        traversal.push(index);
        stack.extend(
            children[index]
                .iter()
                .rev()
                .copied()
                .map(|child| (child, depth + 1)),
        );
    }
    let mut projects = vec![Vec::new(); folders.len()];
    let mut unfiled = Vec::new();
    for &index in project_order {
        let project = &state.projects[index];
        match project.folder_id.as_deref().and_then(|id| indices.get(id)) {
            Some(&folder) => projects[folder].push(index),
            None => unfiled.push(index),
        }
    }
    let project_matches = |index: usize| {
        let project = &state.projects[index];
        query.is_empty()
            || [
                project.name.as_str(),
                project.id.as_str(),
                project.root.to_string_lossy().as_ref(),
            ]
            .iter()
            .any(|value| value.to_lowercase().contains(&query))
    };
    let mut include_subtree = vec![false; folders.len()];
    let mut visible = vec![false; folders.len()];
    let mut matching_projects = vec![Vec::new(); folders.len()];
    let mut counts: Vec<_> = projects.iter().map(Vec::len).collect();
    for &index in &traversal {
        let folder = &folders[index];
        include_subtree[index] = query.is_empty()
            || folder.name.to_lowercase().contains(&query)
            || folder.id.to_lowercase().contains(&query)
            || parents[index].is_some_and(|parent| include_subtree[parent]);
        matching_projects[index] = projects[index]
            .iter()
            .copied()
            .filter(|&project| include_subtree[index] || project_matches(project))
            .collect();
        visible[index] = include_subtree[index] || !matching_projects[index].is_empty();
    }
    for &index in traversal.iter().rev() {
        if let Some(parent) = parents[index] {
            counts[parent] += counts[index];
            visible[parent] |= visible[index];
        }
    }
    let matching_unfiled: Vec<_> = unfiled
        .iter()
        .copied()
        .filter(|&index| "unfiled".contains(&query) || project_matches(index))
        .collect();
    let matched_projects =
        matching_unfiled.len() + matching_projects.iter().map(Vec::len).sum::<usize>();
    let unfiled_collapsed = query.is_empty() && collapsed.contains("unfiled");
    let mut rows = vec![ProjectTreeRow::Unfiled {
        count: unfiled.len(),
        collapsed: unfiled_collapsed,
    }];
    if !unfiled_collapsed {
        rows.extend(
            matching_unfiled
                .into_iter()
                .map(|index| ProjectTreeRow::Project { index, depth: 1 }),
        );
        if unfiled.is_empty() && query.is_empty() {
            rows.push(ProjectTreeRow::Empty { depth: 1 });
        }
    }
    let mut hidden_below = None;
    for &index in &traversal {
        let depth = depths[index];
        if hidden_below.is_some_and(|hidden| depth > hidden) {
            continue;
        }
        hidden_below = None;
        if !visible[index] {
            continue;
        }
        let is_collapsed = query.is_empty() && collapsed.contains(&folders[index].id);
        rows.push(ProjectTreeRow::Folder {
            index,
            depth,
            count: counts[index],
            collapsed: is_collapsed,
        });
        if is_collapsed {
            hidden_below = Some(depth);
            continue;
        }
        rows.extend(matching_projects[index].iter().copied().map(|index| {
            ProjectTreeRow::Project {
                index,
                depth: depth + 1,
            }
        }));
        if counts[index] == 0 && children[index].is_empty() && query.is_empty() {
            rows.push(ProjectTreeRow::Empty { depth: depth + 1 });
        }
    }
    ProjectTree {
        rows,
        matched_projects,
    }
}

#[derive(Clone)]
struct ProjectDropContext {
    state_home: PathBuf,
    parents: BTreeMap<String, Option<String>>,
    names: BTreeMap<String, String>,
    projects: BTreeMap<String, Option<String>>,
}

impl ProjectDropContext {
    fn new(data: &PanelData<'_>) -> Self {
        Self {
            state_home: data.state_home.to_path_buf(),
            parents: data
                .state
                .project_folders
                .iter()
                .map(|folder| (folder.id.clone(), folder.parent_id.clone()))
                .collect(),
            names: data
                .state
                .project_folders
                .iter()
                .map(|folder| (folder.id.clone(), folder.name.to_lowercase()))
                .collect(),
            projects: data
                .state
                .projects
                .iter()
                .map(|project| (project.id.clone(), project.folder_id.clone()))
                .collect(),
        }
    }

    fn accepts(&self, drag: &DraggedProjectItem, destination: Option<&str>) -> bool {
        if drag.state_home != self.state_home
            || destination.is_some_and(|id| !self.parents.contains_key(id))
        {
            return false;
        }
        match &drag.kind {
            ProjectDragKind::Project(id) => self
                .projects
                .get(id)
                .is_some_and(|current| current.as_deref() != destination),
            ProjectDragKind::Folder(id) => {
                let Some(parent) = self.parents.get(id) else {
                    return false;
                };
                if parent.as_deref() == destination {
                    return false;
                }
                if self.parents.iter().any(|(other, parent)| {
                    other != id
                        && parent.as_deref() == destination
                        && self.names.get(other) == self.names.get(id)
                }) {
                    return false;
                }
                let mut current = destination;
                let mut seen = HashSet::new();
                while let Some(candidate) = current {
                    if candidate == id || !seen.insert(candidate) {
                        return false;
                    }
                    current = self
                        .parents
                        .get(candidate)
                        .and_then(|parent| parent.as_deref());
                }
                true
            }
        }
    }
}

pub fn render_panel<V: Render + EntityInputHandler + 'static>(
    kind: PanelKind,
    data: PanelData<'_>,
    on_action: impl Fn(&mut V, PanelAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let query = data.query.trim().to_lowercase();
    let matches = |values: &[&str]| {
        query.is_empty()
            || values
                .iter()
                .any(|value| value.to_lowercase().contains(&query))
    };
    let mut rows = Vec::new();
    let mut total = 0;
    let mut matched_project_count = 0;
    let name = match kind {
        PanelKind::Projects => "projects",
        PanelKind::Worktrees => "worktrees",
        PanelKind::Files => "files",
        PanelKind::Tasks => "tasks",
        PanelKind::Shells => "shells",
        PanelKind::Usage => "usage",
        PanelKind::Settings => "settings",
        PanelKind::ProjectSettings => "project_settings",
        PanelKind::Schedules => "schedules",
    };

    match kind {
        PanelKind::Files
        | PanelKind::Usage
        | PanelKind::Settings
        | PanelKind::ProjectSettings
        | PanelKind::Schedules => {}
        PanelKind::Projects => {
            total = data.state.projects.len();
            let order = sorted_project_indices(
                data.state,
                data.project_order,
                data.project_last_edits,
                data.shells,
            );
            let tree = project_tree(data.state, &query, data.collapsed_folders, &order);
            matched_project_count = tree.matched_projects;
            let drop_context = ProjectDropContext::new(&data);
            for item in tree.rows {
                match item {
                    ProjectTreeRow::Folder {
                        index,
                        depth,
                        count,
                        collapsed,
                    } => {
                        let folder = &data.state.project_folders[index];
                        rows.push(folder_header(
                            Some(&folder.id),
                            &folder.name,
                            count,
                            collapsed,
                            depth,
                            &drop_context,
                            on_action.clone(),
                            cx,
                        ));
                    }
                    ProjectTreeRow::Unfiled { count, collapsed } => {
                        rows.push(folder_header(
                            None,
                            "Unfiled",
                            count,
                            collapsed,
                            0,
                            &drop_context,
                            on_action.clone(),
                            cx,
                        ));
                    }
                    ProjectTreeRow::Empty { depth } => {
                        rows.push(
                            div()
                                .pl(px(project_indent(depth)))
                                .pr(px(8.0))
                                .py(px(6.0))
                                .text_size(px(10.0))
                                .text_color(rgb(colors.muted))
                                .child("Drop projects here")
                                .into_any_element(),
                        );
                    }
                    ProjectTreeRow::Project { index, depth } => {
                        let project = &data.state.projects[index];
                        let worktrees = data.state.worktrees_for(&project.id).len();
                        let tasks = data.state.tasks_for_project(&project.id);
                        let done = tasks
                            .iter()
                            .filter(|task| task.status == TaskStatus::Done)
                            .count();
                        let shells = data
                            .shells
                            .iter()
                            .filter(|shell| {
                                shell.project_id.as_deref() == Some(project.id.as_str())
                                    && shell.alive
                            })
                            .count();
                        let activity =
                            ActivityCounts::for_project(&project.id, data.shells, data.activity);
                        let title = div()
                            .flex()
                            .items_center()
                            .gap(px(4.0))
                            .child(div().flex_1().min_w_0().child(line(
                                project.name.clone(),
                                colors.text,
                                11.0,
                            )))
                            .child(project_notification_control(
                                &project.id,
                                project.notify_on_agent_done,
                                on_action.clone(),
                                cx,
                            ))
                            .child(project_control(
                                &project.id,
                                "settings",
                                "⚙",
                                "Project settings",
                                PanelAction::ProjectSettings(project.id.clone()),
                                on_action.clone(),
                                cx,
                            ))
                            .child(project_control(
                                &project.id,
                                "open",
                                "↗",
                                "Open project in another window",
                                PanelAction::OpenProject(project.id.clone()),
                                on_action.clone(),
                                cx,
                            ));
                        rows.push(project_row(
                            DraggedProjectItem {
                                kind: ProjectDragKind::Project(project.id.clone()),
                                label: project.name.clone(),
                                state_home: data.state_home.to_path_buf(),
                            },
                            data.project_id == project.id,
                            depth,
                            vec![
                                title.into_any_element(),
                                line(
                                    format!(
                                        "{}{worktrees} trees · {done}/{} tasks · {shells} live",
                                        activity
                                            .summary()
                                            .map(|summary| format!("Codex {summary} · "))
                                            .unwrap_or_default(),
                                        tasks.len()
                                    ),
                                    if activity.working > 0 {
                                        colors.cyan
                                    } else {
                                        colors.muted
                                    },
                                    9.0,
                                ),
                            ],
                            PanelAction::Project(project.id.clone()),
                            on_action.clone(),
                            cx,
                        ));
                    }
                }
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
                let activity = ActivityCounts::for_worktree(
                    &worktree.id,
                    data.state,
                    data.shells,
                    data.shell_cwds,
                    data.activity,
                );
                rows.push(row(
                    format!("worktree-{}", worktree.id),
                    selected,
                    colors.magenta,
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
                            if selected { colors.cyan } else { colors.text },
                            11.0,
                        ),
                        line(path.into_owned(), colors.muted, 10.0),
                        line(
                            format!(
                                "{}{done}/{} TASKS · {}",
                                activity
                                    .summary()
                                    .map(|summary| format!("Codex {summary} · "))
                                    .unwrap_or_default(),
                                tasks.len(),
                                short_id(&worktree.id)
                            ),
                            if activity.working > 0 {
                                colors.cyan
                            } else {
                                colors.muted
                            },
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
                let (mark, color) = task_mark(task.status, colors);
                rows.push(row(
                    format!("task-{}", task.id),
                    data.selected_task_id == Some(task.id.as_str()),
                    color,
                    vec![
                        div()
                            .flex()
                            .gap(px(6.0))
                            .child(div().text_color(rgb(color)).child(mark))
                            .child(line(task.title.clone(), colors.text, 11.0))
                            .into_any_element(),
                        line(
                            format!("{} · @ {worktree}", task.status.as_str()),
                            colors.muted,
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
                    colors.cyan,
                    vec![
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(px(6.0))
                            .child(line(
                                format!("{} · {label}", short_id(&shell.id)),
                                colors.text,
                                11.0,
                            ))
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(rgb(if shell.alive {
                                        colors.cyan
                                    } else {
                                        colors.magenta
                                    }))
                                    .child(if shell.alive { "● LIVE" } else { "× EXITED" }),
                            )
                            .into_any_element(),
                        line(path.into_owned(), colors.muted, 10.0),
                        line(
                            format!(
                                "CPU {:.1}% · RAM {} · {command}",
                                metrics.cpu_percent,
                                format_bytes(metrics.ram_bytes)
                            ),
                            if shell.alive {
                                colors.cyan
                            } else {
                                colors.muted
                            },
                            10.0,
                        ),
                        line(shell.id.clone(), colors.muted, 10.0),
                    ],
                    PanelAction::Shell(shell.id.clone()),
                    on_action.clone(),
                    cx,
                ));
            }
        }
    }

    let count = if kind == PanelKind::Projects {
        matched_project_count
    } else {
        rows.len()
    };
    if rows.is_empty() {
        rows.push(
            div()
                .p(px(10.0))
                .text_color(rgb(colors.muted))
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
    let folder_action = on_action.clone();
    let create_action = on_action.clone();
    let sort_selector_bounds = Rc::new(Cell::new(Bounds::<Pixels>::default()));
    let mut panel = div()
        .relative()
        .flex()
        .flex_col()
        .size_full()
        .min_w_0()
        .min_h_0()
        .bg(rgb(colors.panel))
        .text_size(px(11.0))
        .child(
            div()
                .h(px(30.0))
                .flex_none()
                .flex()
                .border_b_1()
                .border_color(rgb(if data.search_focused {
                    colors.cyan
                } else {
                    colors.divider
                }))
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
                        .text_color(rgb(if data.search_focused {
                            colors.text
                        } else {
                            colors.muted
                        }))
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
                        .id("new-project-folder")
                        .flex_none()
                        .h_full()
                        .px(px(6.0))
                        .flex()
                        .items_center()
                        .text_color(rgb(colors.magenta))
                        .cursor_pointer()
                        .hover(|style| style.bg(rgb(colors.panel_active)))
                        .child("+ FOLDER")
                        .on_click(cx.listener(move |view, _, window, cx| {
                            folder_action(view, PanelAction::CreateFolder, window, cx);
                        }))
                }))
                .children((kind == PanelKind::Projects).then(|| {
                    div()
                        .id("new-project")
                        .flex_none()
                        .h_full()
                        .px(px(8.0))
                        .flex()
                        .items_center()
                        .text_color(rgb(colors.cyan))
                        .cursor_pointer()
                        .hover(|style| style.bg(rgb(colors.panel_active)))
                        .child("+ PROJECT")
                        .on_click(cx.listener(move |view, _, window, cx| {
                            create_action(view, PanelAction::CreateProject, window, cx);
                        }))
                })),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(6.0))
                .px(px(8.0))
                .py(px(4.0))
                .text_size(px(10.0))
                .text_color(rgb(colors.muted))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_ellipsis()
                        .child(format!("{count:02} / {total:02} {}", name.to_uppercase())),
                )
                .children((kind == PanelKind::Projects).then(|| {
                    project_sort_controls(
                        data.project_order,
                        data.project_sort_menu_open,
                        sort_selector_bounds.clone(),
                        on_action.clone(),
                        cx,
                    )
                })),
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
        let (_, color) = task_mark(task.status, colors);
        panel = panel.child(
            div()
                .id("task-detail")
                .flex_none()
                .min_h_0()
                .max_h(px(200.0))
                .overflow_y_scroll()
                .border_t_1()
                .border_color(rgb(colors.divider))
                .p(px(8.0))
                .child(div().text_color(rgb(colors.gold)).child("TASK DETAIL"))
                .child(
                    div()
                        .pt(px(5.0))
                        .text_color(rgb(colors.text))
                        .child(task.title.clone()),
                )
                .child(div().pt(px(4.0)).text_color(rgb(color)).child(format!(
                    "{} · @ {}",
                    task.status.as_str(),
                    worktree_label(data.state, task.worktree_id.as_deref())
                )))
                .child(div().pt(px(6.0)).text_color(rgb(colors.muted)).child(
                    if task.details.is_empty() {
                        "No details".to_owned()
                    } else {
                        task.details.clone()
                    },
                ))
                .child(line(task.id.clone(), colors.muted, 10.0)),
        );
    }
    panel
        .children(
            (kind == PanelKind::Projects && data.project_sort_menu_open).then(|| {
                project_sort_menu(
                    data.project_order,
                    sort_selector_bounds.clone(),
                    on_action.clone(),
                    cx,
                )
            }),
        )
        .into_any_element()
}

fn project_sort_controls<V: 'static>(
    order: ProjectOrder,
    open: bool,
    selector_bounds: Rc<Cell<Bounds<Pixels>>>,
    on_action: impl Fn(&mut V, PanelAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let choose = on_action.clone();
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(2.0))
        .text_size(px(9.0))
        .child(
            div()
                .id("project-sort-selector")
                .relative()
                .flex()
                .items_center()
                .gap(px(5.0))
                .px(px(5.0))
                .h(px(20.0))
                .cursor_pointer()
                .bg(rgb(if open {
                    colors.panel_active
                } else {
                    colors.panel
                }))
                .text_color(rgb(if open { colors.cyan } else { colors.muted }))
                .hover(|style| {
                    style
                        .bg(rgb(colors.panel_active))
                        .text_color(rgb(colors.cyan))
                })
                .child(order.by.label())
                .child(if open { "▴" } else { "▾" })
                .child(
                    canvas(
                        move |bounds, _, _| selector_bounds.set(bounds),
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .inset_0(),
                )
                .when(!open, |control| {
                    control.tooltip(|_, cx| {
                        cx.new(|_| ControlTooltip("Sort projects within each folder"))
                            .into()
                    })
                })
                .on_click(cx.listener(move |view, _, window, cx| {
                    choose(view, PanelAction::ToggleProjectSortMenu, window, cx);
                })),
        )
        .child(
            div()
                .id("project-sort-direction")
                .flex()
                .items_center()
                .justify_center()
                .w(px(22.0))
                .h(px(20.0))
                .cursor_pointer()
                .text_color(rgb(colors.cyan))
                .hover(|style| style.bg(rgb(colors.panel_active)))
                .child(if order.descending { "↓" } else { "↑" })
                .when(!open, |control| {
                    control.tooltip(move |_, cx| {
                        cx.new(|_| ControlTooltip(order.direction_label())).into()
                    })
                })
                .on_click(cx.listener(move |view, _, window, cx| {
                    on_action(
                        view,
                        PanelAction::SetProjectOrder(order.toggled()),
                        window,
                        cx,
                    );
                })),
        )
        .into_any_element()
}

fn project_sort_menu<V: 'static>(
    order: ProjectOrder,
    selector_bounds: Rc<Cell<Bounds<Pixels>>>,
    on_action: impl Fn(&mut V, PanelAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let dismiss = on_action.clone();
    div()
        .id("project-sort-menu")
        .absolute()
        .top(px(56.0))
        .right(px(8.0))
        .w(px(220.0))
        .max_w(gpui::relative(0.9))
        .bottom(px(8.0))
        .max_h(px(128.0))
        .overflow_y_scroll()
        .bg(rgb(colors.panel_active))
        .border_1()
        .border_color(rgb(colors.magenta))
        .p(px(3.0))
        .occlude()
        .on_mouse_down_out(
            cx.listener(move |view, event: &gpui::MouseDownEvent, window, cx| {
                if selector_bounds.get().contains(&event.position) {
                    return;
                }
                dismiss(view, PanelAction::CloseProjectSortMenu, window, cx);
            }),
        )
        .children(
            [
                ProjectSort::Name,
                ProjectSort::LastEdited,
                ProjectSort::DateAdded,
                ProjectSort::LiveSessions,
            ]
            .into_iter()
            .map(|by| {
                let action = on_action.clone();
                let selected = by == order.by;
                let next = if selected {
                    order
                } else {
                    ProjectOrder::for_sort(by)
                };
                div()
                    .id(format!("project-sort-{}", by.label()))
                    .flex()
                    .items_center()
                    .gap(px(7.0))
                    .px(px(8.0))
                    .py(px(7.0))
                    .cursor_pointer()
                    .text_size(px(10.0))
                    .text_color(rgb(if selected { colors.cyan } else { colors.text }))
                    .hover(|style| style.bg(rgb(colors.divider)).text_color(rgb(colors.cyan)))
                    .child(
                        div()
                            .flex_none()
                            .w(px(12.0))
                            .child(if selected { "✓" } else { "" }),
                    )
                    .child(div().flex_1().min_w_0().text_ellipsis().child(by.label()))
                    .on_click(cx.listener(move |view, _, window, cx| {
                        action(view, PanelAction::SetProjectOrder(next), window, cx);
                    }))
            }),
        )
        .into_any_element()
}

struct ControlTooltip(&'static str);
impl Render for ControlTooltip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme::palette(cx);
        div()
            .px(px(8.0))
            .py(px(5.0))
            .bg(rgb(colors.panel_active))
            .border_1()
            .border_color(rgb(colors.divider))
            .text_color(rgb(colors.text))
            .font_family("Menlo")
            .text_size(px(10.0))
            .child(self.0)
    }
}

fn project_control<V: 'static>(
    project_id: &str,
    name: &str,
    mark: &'static str,
    tooltip: &'static str,
    action: PanelAction,
    on_action: impl Fn(&mut V, PanelAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    div()
        .id(format!("{name}-project-{project_id}"))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .w(px(20.0))
        .h(px(18.0))
        .text_color(rgb(colors.cyan))
        .cursor_pointer()
        .hover(|style| {
            style
                .bg(rgb(colors.divider))
                .text_color(rgb(colors.magenta))
        })
        .child(mark)
        .tooltip(move |_, cx| cx.new(|_| ControlTooltip(tooltip)).into())
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(move |view, _, window, cx| {
            cx.stop_propagation();
            on_action(view, action.clone(), window, cx);
        }))
        .into_any_element()
}

fn project_notification_control<V: 'static>(
    project_id: &str,
    enabled: bool,
    on_action: impl Fn(&mut V, PanelAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let project_id = project_id.to_owned();
    div()
        .id(format!("notifications-project-{project_id}"))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .w(px(20.0))
        .h(px(18.0))
        .cursor_pointer()
        .hover(|style| style.bg(rgb(colors.divider)))
        .child(icons::icon(
            if enabled { Icon::Bell } else { Icon::BellOff },
            if enabled { colors.cyan } else { colors.muted },
        ))
        .tooltip(move |_, cx| {
            cx.new(|_| {
                ControlTooltip(if enabled {
                    "Agent completion notifications on · click to disable"
                } else {
                    "Agent completion notifications off · click to enable"
                })
            })
            .into()
        })
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(move |view, _, window, cx| {
            cx.stop_propagation();
            on_action(
                view,
                PanelAction::ToggleProjectNotifications(project_id.clone()),
                window,
                cx,
            );
        }))
        .into_any_element()
}

fn project_indent(depth: usize) -> f32 {
    8.0 + depth.min(16) as f32 * 12.0
}

fn folder_header<V: 'static>(
    id: Option<&str>,
    name: &str,
    count: usize,
    collapsed: bool,
    depth: usize,
    drop_context: &ProjectDropContext,
    on_action: impl Fn(&mut V, PanelAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let folder_id = id.map(str::to_owned);
    let toggle_id = id.unwrap_or("unfiled").to_owned();
    let toggle_action = on_action.clone();
    let drop_action = on_action.clone();
    let drag_action = on_action.clone();
    let drag_folder_id = folder_id.clone();
    let drag_label = name.to_owned();
    let drag_state_home = drop_context.state_home.clone();
    let drag_view = cx.entity();
    let hover_destination = folder_id.clone();
    let drop_destination = folder_id.clone();
    let hover_context = drop_context.clone();
    let drop_context = drop_context.clone();
    let editable = id.is_some();
    div()
        .id(format!("project-folder-{}", id.unwrap_or("unfiled")))
        .flex()
        .items_center()
        .gap(px(5.0))
        .min_w_0()
        .h(px(27.0))
        .pl(px(project_indent(depth)))
        .pr(px(8.0))
        .mt(px(3.0))
        .border_b_1()
        .border_color(rgb(colors.divider))
        .bg(rgb(colors.panel_active))
        .text_color(rgb(colors.magenta))
        .cursor_grab()
        .drag_over::<DraggedProjectItem>(move |style, drag, _, _| {
            if hover_context.accepts(drag, hover_destination.as_deref()) {
                style
                    .bg(rgb(colors.divider))
                    .border_b_2()
                    .border_color(rgb(colors.cyan))
            } else {
                style
            }
        })
        .child(if collapsed { "▸" } else { "▾" })
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_ellipsis()
                .overflow_hidden()
                .child(name.to_owned()),
        )
        .child(
            div()
                .text_size(px(9.0))
                .text_color(rgb(colors.muted))
                .child(format!("{count:02}")),
        )
        .children(id.map(|id| {
            project_control(
                id,
                "add-subfolder",
                "+",
                "Create subfolder",
                PanelAction::CreateSubfolder(id.into()),
                on_action.clone(),
                cx,
            )
        }))
        .children(id.map(|id| {
            project_control(
                id,
                "edit-folder",
                "✎",
                "Rename folder",
                PanelAction::EditFolder(id.into()),
                on_action.clone(),
                cx,
            )
        }))
        .children(id.map(|id| {
            project_control(
                id,
                "remove-folder",
                "×",
                "Remove folder; keep its projects and subfolders",
                PanelAction::RemoveFolder(id.into()),
                on_action,
                cx,
            )
        }))
        .tooltip(move |_, cx| {
            cx.new(|_| {
                ControlTooltip(if editable {
                    "Drag this folder to move it; drop projects or folders here"
                } else {
                    "Drop projects here to unfile them, or folders to move them to the root"
                })
            })
            .into()
        })
        .on_click(cx.listener(move |view, _, window, cx| {
            toggle_action(
                view,
                PanelAction::ToggleFolder(toggle_id.clone()),
                window,
                cx,
            );
        }))
        .when(editable, move |element| {
            element.on_drag(
                DraggedProjectItem {
                    kind: ProjectDragKind::Folder(
                        drag_folder_id.expect("an editable folder has an ID"),
                    ),
                    label: drag_label,
                    state_home: drag_state_home,
                },
                move |drag, _, window, cx| {
                    drag_view.update(cx, |view, cx| {
                        drag_action(view, PanelAction::BeginProjectDrag, window, cx);
                    });
                    cx.new(|_| drag.clone())
                },
            )
        })
        .on_drop(
            cx.listener(move |view, drag: &DraggedProjectItem, window, cx| {
                if !drop_context.accepts(drag, drop_destination.as_deref()) {
                    return;
                }
                let action = match &drag.kind {
                    ProjectDragKind::Project(project_id) => PanelAction::MoveProject {
                        project_id: project_id.clone(),
                        folder_id: drop_destination.clone(),
                    },
                    ProjectDragKind::Folder(folder_id) => PanelAction::MoveFolder {
                        folder_id: folder_id.clone(),
                        parent_id: drop_destination.clone(),
                    },
                };
                cx.stop_propagation();
                drop_action(view, action, window, cx);
            }),
        )
        .into_any_element()
}

fn project_row<V: 'static>(
    drag: DraggedProjectItem,
    selected: bool,
    depth: usize,
    children: Vec<AnyElement>,
    action: PanelAction,
    on_action: impl Fn(&mut V, PanelAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let ProjectDragKind::Project(ref project_id) = drag.kind else {
        unreachable!("project rows only drag projects");
    };
    let drag_view = cx.entity();
    let drag_action = on_action.clone();
    div()
        .id(format!("project-{project_id}"))
        .flex()
        .flex_col()
        .gap(px(3.0))
        .min_w_0()
        .pl(px(project_indent(depth)))
        .pr(px(8.0))
        .py(px(6.0))
        .border_l_1()
        .border_color(rgb(if selected { colors.cyan } else { colors.panel }))
        .bg(rgb(if selected {
            colors.panel_active
        } else {
            colors.panel
        }))
        .cursor_grab()
        .hover(|element| element.bg(rgb(colors.panel_active)))
        .children(children)
        .on_click(cx.listener(move |view, _, window, cx| {
            on_action(view, action.clone(), window, cx);
        }))
        .on_drag(drag, move |drag, _, window, cx| {
            drag_view.update(cx, |view, cx| {
                drag_action(view, PanelAction::BeginProjectDrag, window, cx);
            });
            cx.new(|_| drag.clone())
        })
        .into_any_element()
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
    let colors = theme::palette(cx);
    div()
        .id(id)
        .flex()
        .flex_col()
        .gap(px(3.0))
        .min_w_0()
        .px(px(8.0))
        .py(px(6.0))
        .border_l_1()
        .border_color(rgb(if selected { accent } else { colors.panel }))
        .bg(rgb(if selected {
            colors.panel_active
        } else {
            colors.panel
        }))
        .hover(|element| element.bg(rgb(colors.panel_active)))
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

fn task_mark(status: TaskStatus, colors: Palette) -> (&'static str, u32) {
    match status {
        TaskStatus::Todo => ("□", colors.muted),
        TaskStatus::InProgress => ("◧", colors.gold),
        TaskStatus::Done => ("■", colors.cyan),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Project, ProjectFolder};

    fn project_tree(state: &State, query: &str, collapsed: &HashSet<String>) -> ProjectTree {
        super::project_tree(
            state,
            query,
            collapsed,
            &sorted_project_indices(state, ProjectOrder::default(), &BTreeMap::new(), &[]),
        )
    }

    fn folder(id: &str, name: &str, parent: Option<&str>) -> ProjectFolder {
        ProjectFolder {
            id: id.into(),
            name: name.into(),
            parent_id: parent.map(str::to_owned),
            created_at: 0,
        }
    }

    fn project(id: &str, name: &str, folder: Option<&str>) -> Project {
        Project {
            id: id.into(),
            name: name.into(),
            root: PathBuf::from(format!("/projects/{id}")),
            repository_roots: Vec::new(),
            folder_id: folder.map(str::to_owned),
            created_at: 0,
            notify_on_agent_done: false,
        }
    }

    fn nested_state() -> State {
        State {
            project_folders: vec![
                folder("work", "Work", None),
                folder("tools", "Tools", Some("work")),
                folder("scripts", "Scripts", Some("tools")),
                folder("personal", "Personal", None),
            ],
            projects: vec![
                project("one", "One", Some("work")),
                project("needle", "Needle", Some("scripts")),
                project("outside", "Outside", None),
            ],
            ..State::default()
        }
    }

    fn labels(state: &State, tree: &ProjectTree) -> Vec<String> {
        tree.rows
            .iter()
            .filter_map(|row| match row {
                ProjectTreeRow::Folder { index, .. } => {
                    Some(state.project_folders[*index].id.clone())
                }
                ProjectTreeRow::Project { index, .. } => Some(state.projects[*index].id.clone()),
                ProjectTreeRow::Unfiled { .. } => Some("unfiled".into()),
                ProjectTreeRow::Empty { .. } => None,
            })
            .collect()
    }

    #[test]
    fn nested_projects_have_subtree_counts_and_depths() {
        let state = nested_state();
        let tree = project_tree(&state, "", &HashSet::new());
        assert_eq!(tree.matched_projects, 3);
        assert_eq!(
            labels(&state, &tree),
            [
                "unfiled", "outside", "personal", "work", "one", "tools", "scripts", "needle"
            ]
        );
        assert!(tree.rows.contains(&ProjectTreeRow::Folder {
            index: 0,
            depth: 0,
            count: 2,
            collapsed: false,
        }));
        assert!(tree.rows.contains(&ProjectTreeRow::Folder {
            index: 1,
            depth: 1,
            count: 1,
            collapsed: false,
        }));
        assert!(
            tree.rows
                .contains(&ProjectTreeRow::Project { index: 1, depth: 3 })
        );
    }

    #[test]
    fn collapsing_a_parent_hides_its_complete_subtree() {
        let state = nested_state();
        let collapsed = HashSet::from(["work".into()]);
        let tree = project_tree(&state, "", &collapsed);
        assert_eq!(
            labels(&state, &tree),
            ["unfiled", "outside", "personal", "work"]
        );
        assert_eq!(tree.matched_projects, 3);
        assert!(tree.rows.contains(&ProjectTreeRow::Folder {
            index: 0,
            depth: 0,
            count: 2,
            collapsed: true,
        }));
    }

    #[test]
    fn project_search_keeps_ancestors_and_opens_collapsed_folders() {
        let state = nested_state();
        let tree = project_tree(
            &state,
            "Needle",
            &HashSet::from(["work".into(), "tools".into()]),
        );
        assert_eq!(
            labels(&state, &tree),
            ["unfiled", "work", "tools", "scripts", "needle"]
        );
        assert_eq!(tree.matched_projects, 1);
        assert!(!tree.rows.iter().any(|row| matches!(
            row,
            ProjectTreeRow::Folder {
                collapsed: true,
                ..
            }
        )));
    }

    #[test]
    fn folder_search_includes_descendants_but_excludes_ancestor_projects() {
        let state = nested_state();
        let tree = project_tree(&state, "Tools", &HashSet::new());
        assert_eq!(
            labels(&state, &tree),
            ["unfiled", "work", "tools", "scripts", "needle"]
        );
        assert_eq!(tree.matched_projects, 1);
        let tree = project_tree(&state, "Work", &HashSet::new());
        assert_eq!(
            labels(&state, &tree),
            ["unfiled", "work", "one", "tools", "scripts", "needle"]
        );
        assert_eq!(tree.matched_projects, 2);
    }

    #[test]
    fn orphan_and_cyclic_folders_remain_visible_once() {
        let state = State {
            project_folders: vec![
                folder("a", "A", Some("b")),
                folder("b", "B", Some("a")),
                folder("c", "C", Some("missing")),
                folder("d", "D", Some("d")),
            ],
            projects: vec![
                project("p", "P", Some("a")),
                project("q", "Q", Some("missing")),
            ],
            ..State::default()
        };
        let tree = project_tree(&state, "", &HashSet::new());
        assert_eq!(
            labels(&state, &tree),
            ["unfiled", "q", "a", "p", "b", "c", "d"]
        );
        assert_eq!(tree.matched_projects, 2);
        assert!(tree.rows.contains(&ProjectTreeRow::Folder {
            index: 0,
            depth: 0,
            count: 1,
            collapsed: false
        }));
        assert!(tree.rows.contains(&ProjectTreeRow::Folder {
            index: 1,
            depth: 1,
            count: 0,
            collapsed: false
        }));
    }

    #[test]
    fn project_order_applies_within_groups_without_reordering_folders() {
        let state = State {
            project_folders: vec![
                folder("z", "Zulu", None),
                folder("a", "Alpha", None),
                folder("nested", "Nested", Some("a")),
            ],
            projects: vec![
                project("a-old", "Alpha old", Some("a")),
                project("a-new", "Zulu new", Some("a")),
                project("u-old", "Alpha unfiled", None),
                project("u-new", "Zulu unfiled", None),
                project("z", "Zulu folder project", Some("z")),
                project("n", "Nested project", Some("nested")),
            ],
            ..State::default()
        };
        let edits = BTreeMap::from([
            ("a-old".into(), 10),
            ("a-new".into(), 20),
            ("u-old".into(), 10),
            ("u-new".into(), 20),
            ("z".into(), 50),
            ("n".into(), 100),
        ]);
        let order = sorted_project_indices(&state, ProjectOrder::default(), &edits, &[]);
        let tree = super::project_tree(&state, "", &HashSet::new(), &order);
        assert_eq!(
            labels(&state, &tree),
            [
                "unfiled", "u-new", "u-old", "a", "a-new", "a-old", "nested", "n", "z", "z"
            ]
        );
        let collapsed = HashSet::from(["a".into()]);
        let tree = super::project_tree(&state, "", &collapsed, &order);
        assert_eq!(
            labels(&state, &tree),
            ["unfiled", "u-new", "u-old", "a", "z", "z"]
        );
        assert_eq!(tree.matched_projects, 6);
        let tree = super::project_tree(&state, "Alpha", &collapsed, &order);
        assert_eq!(
            labels(&state, &tree),
            ["unfiled", "u-old", "a", "a-new", "a-old", "nested", "n"]
        );
        assert_eq!(tree.matched_projects, 4);
        assert!(!tree.rows.iter().any(|row| matches!(
            row,
            ProjectTreeRow::Folder {
                collapsed: true,
                ..
            }
        )));
    }

    fn drop_context() -> ProjectDropContext {
        ProjectDropContext {
            state_home: PathBuf::from("/state"),
            parents: BTreeMap::from([
                ("work".into(), None),
                ("tools".into(), Some("work".into())),
                ("scripts".into(), Some("tools".into())),
                ("other-tools".into(), None),
            ]),
            names: BTreeMap::from([
                ("work".into(), "work".into()),
                ("tools".into(), "tools".into()),
                ("scripts".into(), "scripts".into()),
                ("other-tools".into(), "tools".into()),
            ]),
            projects: BTreeMap::from([("one".into(), Some("work".into()))]),
        }
    }

    fn drag(kind: ProjectDragKind) -> DraggedProjectItem {
        DraggedProjectItem {
            kind,
            label: String::new(),
            state_home: PathBuf::from("/state"),
        }
    }

    #[test]
    fn drop_targets_reject_cycles_self_duplicates_and_foreign_stores() {
        let context = drop_context();
        let work = drag(ProjectDragKind::Folder("work".into()));
        assert!(!context.accepts(&work, Some("work")));
        assert!(!context.accepts(&work, Some("tools")));
        assert!(!context.accepts(&work, Some("scripts")));
        assert!(!context.accepts(&work, None));
        let scripts = drag(ProjectDragKind::Folder("scripts".into()));
        assert!(context.accepts(&scripts, Some("work")));
        assert!(context.accepts(&scripts, None));
        assert!(!context.accepts(&scripts, Some("tools")));
        let tools = drag(ProjectDragKind::Folder("tools".into()));
        assert!(!context.accepts(&tools, None));
        let project = drag(ProjectDragKind::Project("one".into()));
        assert!(context.accepts(&project, Some("tools")));
        assert!(context.accepts(&project, None));
        assert!(!context.accepts(&project, Some("work")));
        assert!(!context.accepts(&project, Some("missing")));
        let foreign = DraggedProjectItem {
            state_home: PathBuf::from("/another-state"),
            ..project
        };
        assert!(!context.accepts(&foreign, Some("tools")));
    }
}
