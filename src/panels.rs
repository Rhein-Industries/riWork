//! Searchable navigation views that share the same movable tab surface as shells.

use std::{
    cell::Cell,
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
    sync::{LazyLock, Mutex, PoisonError},
    time::{Duration, Instant},
};

use gpui::{
    AnyElement, Bounds, Context, Div, Entity, FocusHandle, Focusable, FontWeight, IntoElement,
    MouseButton, Pixels, Render, Stateful, Window, canvas, div, prelude::*, px, rgb,
};
use gpui_kit::TestSupportExt;

use crate::{
    activity::{ActivityCounts, AgentActivity, AgentState, ChatActivity},
    behavior_controls as behavior,
    chat::model::ChatInfo,
    controls,
    icons::{self, ActionGlyph, Icon},
    layouts::PanelKind,
    project_sort::{ProjectOrder, ProjectSort, sorted_project_indices},
    remote_tree::{FolderView, Link, Listing, ProjectView, RemoteShell, SelectedView, folder_key},
    session_catalog::{self, Filter},
    sessions::{SessionMetrics, ShellKind, ShellSession},
    store::{State, TaskStatus},
    theme::{self, Palette},
    tooltip::{self, Look},
    ui_text,
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
    Shell {
        project: String,
        generation: u64,
        id: String,
    },
    Chat {
        project: String,
        generation: u64,
        id: String,
    },
    SessionFilter(Filter),
    RefreshSessions,
    Search,
    /// Empty the search field, from its clear button.
    ClearSearch,
    ToggleProjectSortMenu,
    /// Controlled request from the visible popup owner; never invert a shared boolean.
    SetProjectSortMenuOpen(bool),
    CloseProjectSortMenu,
    SetProjectOrder(ProjectOrder),
    Remote(RemoteAction),
}

/// What a row that belongs to another Mac does when clicked, beyond what a local row of the
/// same kind does (selecting a project, worktree or task goes through the ordinary actions).
#[derive(Clone)]
pub enum RemoteAction {
    /// Open a shell of the selected remote project in a tab.
    OpenShell { host: String, shell: RemoteShell },
    /// Ask for a name, then create a project on the host.
    NewProject(String),
    /// Dismiss what a failed project creation left under a host's folder.
    DismissProject(String),
    /// Dismiss what a failed shell creation left in the Shells panel.
    DismissShell { host: String, project: String },
}

/// How long a worktree folder check stays fresh; the workspace redraws about
/// this often anyway.
const PRESENCE_INTERVAL: Duration = Duration::from_secs(2);

struct PresenceProbe {
    missing: bool,
    at: Instant,
}

/// Which registered worktree folders are gone. Checking is a `stat`, which can
/// block for a long time on a sleeping external or network volume, so render
/// only reads this cache and background tasks keep it fresh. Each folder has
/// its own check, so one stuck volume cannot delay the others or stack up
/// more blocked threads.
#[derive(Default)]
struct WorktreePresence {
    probes: HashMap<PathBuf, PresenceProbe>,
    in_flight: HashSet<PathBuf>,
}

impl WorktreePresence {
    /// A folder nobody has checked yet counts as present.
    fn missing(&self, path: &Path) -> bool {
        self.probes.get(path).is_some_and(|probe| probe.missing)
    }

    /// The folders whose check is due, now marked as being checked.
    fn due<'a>(&mut self, paths: impl Iterator<Item = &'a Path>, now: Instant) -> Vec<PathBuf> {
        let mut due = Vec::new();
        for path in paths {
            let fresh = self
                .probes
                .get(path)
                .is_some_and(|probe| now.duration_since(probe.at) < PRESENCE_INTERVAL);
            if !fresh && !self.in_flight.contains(path) && !due.iter().any(|due| due == path) {
                due.push(path.to_owned());
            }
        }
        self.in_flight.extend(due.iter().cloned());
        due
    }

    /// Record a check; returns whether it changed what the panel shows.
    fn finish(&mut self, path: PathBuf, missing: bool, now: Instant) -> bool {
        self.in_flight.remove(&path);
        let changed = self.missing(&path) != missing;
        self.probes.insert(path, PresenceProbe { missing, at: now });
        changed
    }
}

fn folder_missing(path: &Path) -> bool {
    !path.is_dir()
}

static WORKTREE_PRESENCE: LazyLock<Mutex<WorktreePresence>> = LazyLock::new(Mutex::default);

fn worktree_presence() -> std::sync::MutexGuard<'static, WorktreePresence> {
    // The lock is never held across a filesystem call, so poisoning is harmless.
    WORKTREE_PRESENCE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Start background checks for worktree folders whose cached state is stale,
/// and repaint if one turns out to have appeared or vanished.
fn refresh_worktree_presence<'a, V: 'static>(
    paths: impl Iterator<Item = &'a Path>,
    cx: &mut Context<V>,
) {
    let due = worktree_presence().due(paths, Instant::now());
    for path in due {
        let probe = path.clone();
        let work = cx
            .background_executor()
            .spawn(async move { folder_missing(&probe) });
        cx.spawn(async move |this, cx| {
            let missing = work.await;
            if worktree_presence().finish(path, missing, Instant::now()) {
                let _ = this.update(cx, |_, cx| cx.notify());
            }
        })
        .detach();
    }
}

pub struct PanelData<'a> {
    pub state: &'a State,
    pub project_id: &'a str,
    pub selected_worktree_id: Option<&'a str>,
    pub selected_task_id: Option<&'a str>,
    pub shells: &'a [ShellSession],
    pub shell_cwds: &'a BTreeMap<String, PathBuf>,
    pub metrics: &'a BTreeMap<String, SessionMetrics>,
    pub activity: &'a BTreeMap<String, AgentState>,
    /// The open chat tabs, counted among the agents beside the shells.
    pub chats: &'a [ChatActivity],
    pub native_chats: &'a [ChatInfo],
    pub session_catalog_note: Option<&'a str>,
    pub session_catalog_loading: bool,
    pub session_catalog_generation: u64,
    pub session_filter: Filter,
    pub query: &'a str,
    pub search_focused: bool,
    pub search_input: Option<&'a Entity<crate::text_input::InputState>>,
    pub control_inset: f32,
    pub collapsed_folders: &'a HashSet<String>,
    pub state_home: &'a Path,
    pub project_order: ProjectOrder,
    pub project_last_edits: &'a BTreeMap<String, u64>,
    pub project_sort_menu_open: bool,
    /// Workspace retains one owner per visible Projects surface and removes stale owners.
    pub project_sort_ui: Option<&'a ProjectSortUi>,
    /// The hosts' folders, already filtered by the search. Only the Projects panel draws them.
    pub remote_folders: &'a [FolderView],
    /// The selected project when it is on another Mac. The Worktrees, Tasks and Shells panels
    /// then draw its lists instead of the local project's.
    pub selected_remote: Option<&'a SelectedView>,
}

/// Persistent Base popover state for one visible Projects surface. Creation belongs
/// to Workspace's surface lifecycle, not render. Closing a peer never steals focus.
pub struct ProjectSortUi {
    pub state: Entity<gpui_kit::base::PopoverState>,
    pub trigger_focus: FocusHandle,
}
impl ProjectSortUi {
    pub fn new(cx: &mut gpui::App) -> Self {
        Self {
            state: cx.new(|cx| gpui_kit::base::PopoverState::new(false, cx)),
            trigger_focus: cx.focus_handle(),
        }
    }
    pub fn dismiss(&self, window: &mut Window, cx: &mut gpui::App) {
        self.state.update(cx, |state, cx| state.dismiss(window, cx));
    }
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
            .gap(ui_text::space(6.0))
            .px(ui_text::space(10.0))
            .py(ui_text::space(6.0))
            .max_w(ui_text::space(280.0))
            .bg(rgb(colors.panel_active))
            .border_1()
            .border_color(rgb(colors.cyan))
            .text_color(rgb(colors.text))
            .font_family(ui_text::ui_family())
            .text_size(ui_text::text(11.0))
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

pub fn render_panel<V: Render + 'static>(
    kind: PanelKind,
    data: PanelData<'_>,
    on_action: impl Fn(&mut V, PanelAction, &mut Window, &mut Context<V>) + Clone + 'static,
    window: &mut Window,
    cx: &mut Context<V>,
) -> AnyElement {
    if !data.project_sort_menu_open
        && let Some(ui) = data.project_sort_ui
    {
        ui.dismiss(window, cx);
    }
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
    let name = kind.name();

    match kind {
        PanelKind::Files
        | PanelKind::Preview
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
                                .pr(ui_text::space(8.0))
                                .py(ui_text::space(6.0))
                                .text_size(ui_text::text(10.0))
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
                            ActivityCounts::for_project(&project.id, data.shells, data.activity)
                                .with_chats_in_project(&project.id, data.chats);
                        let selected = data.project_id == project.id;
                        // Keyed by the visible surface and immutable project identity.
                        // Base receives the same persistent handle; refreshing/reordering never focuses it.
                        let scope = data
                            .project_sort_ui
                            .expect("Projects surface requires persistent sort UI")
                            .state
                            .entity_id();
                        let row_focus = window
                            .use_keyed_state(
                                format!("project-row-focus-{scope}-{}", project.id),
                                cx,
                                |_, cx| cx.focus_handle(),
                            )
                            .read(cx)
                            .clone();
                        let controls = project_action_strip(
                            ui_text::is_native(),
                            selected,
                            &row_focus,
                            window,
                            cx,
                        )
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
                        let title = div()
                            .flex()
                            .items_center()
                            .gap(ui_text::space(4.0))
                            .child(div().flex_1().min_w_0().child(line(
                                project.name.clone(),
                                colors.text,
                                controls::ROW_TITLE_TEXT,
                            )))
                            .child(controls);
                        rows.push(project_row(
                            &project.id,
                            &project.name,
                            Some(DraggedProjectItem {
                                kind: ProjectDragKind::Project(project.id.clone()),
                                label: project.name.clone(),
                                state_home: data.state_home.to_path_buf(),
                            }),
                            ProjectRowLook {
                                selected,
                                depth,
                                dimmed: false,
                                focus: Some(row_focus),
                            },
                            vec![
                                title.into_any_element(),
                                line(
                                    format!(
                                        "{}{worktrees} trees · {done}/{} tasks · {shells} live",
                                        activity
                                            .summary()
                                            .map(|summary| format!("Agents {summary} · "))
                                            .unwrap_or_default(),
                                        tasks.len()
                                    ),
                                    if activity.working > 0 {
                                        colors.working
                                    } else {
                                        colors.muted
                                    },
                                    if ui_text::is_native() {
                                        controls::ROW_DETAIL_TEXT
                                    } else {
                                        9.0
                                    },
                                ),
                            ],
                            PanelAction::Project(project.id.clone()),
                            on_action.clone(),
                            cx,
                        ));
                    }
                }
            }
            // Each paired Mac is one more folder, below the local ones.
            for folder in data.remote_folders {
                total += folder.count;
                matched_project_count += folder.projects.len();
                push_remote_folder(&mut rows, folder, on_action.clone(), cx);
            }
        }
        PanelKind::Worktrees | PanelKind::Tasks | PanelKind::Shells
            if data.selected_remote.is_some() =>
        {
            if let Some(remote) = data.selected_remote {
                total = push_remote_panel(
                    &mut rows,
                    kind,
                    remote,
                    &data,
                    &matches,
                    on_action.clone(),
                    cx,
                );
            }
        }
        PanelKind::Worktrees => {
            let worktrees = data.state.worktrees_for(data.project_id);
            refresh_worktree_presence(worktrees.iter().map(|worktree| worktree.path.as_path()), cx);
            for worktree in worktrees {
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
                let missing = worktree_presence().missing(&worktree.path);
                let activity = ActivityCounts::for_worktree(
                    &worktree.id,
                    data.state,
                    data.shells,
                    data.shell_cwds,
                    data.activity,
                )
                .with_chats_in_worktree(&worktree.id, data.chats);
                if ui_text::is_native() {
                    let lines = native_worktree_lines(
                        NativeWorktree {
                            primary: worktree.is_primary,
                            branch: &worktree.branch,
                            repository: worktree
                                .repository_root
                                .as_ref()
                                .and_then(|root| root.file_name())
                                .map(|name| name.to_string_lossy().into_owned()),
                            missing,
                            path: path.into_owned(),
                            counts: format!(
                                "{}{done}/{} tasks",
                                activity
                                    .summary()
                                    .map(|summary| format!("Agents {summary} · "))
                                    .unwrap_or_default(),
                                tasks.len(),
                            ),
                            id: short_id(&worktree.id),
                            selected,
                            working: activity.working > 0,
                        },
                        colors,
                    );
                    rows.push(row(
                        format!("worktree-{}", worktree.id),
                        worktree.branch.clone(),
                        selected,
                        colors.magenta,
                        lines,
                        PanelAction::Worktree(worktree.id.clone()),
                        on_action.clone(),
                        cx,
                    ));
                    continue;
                }
                rows.push(row(
                    format!("worktree-{}", worktree.id),
                    worktree.branch.clone(),
                    selected,
                    colors.magenta,
                    vec![
                        title_line(
                            format!(
                                "{} {}{}{}",
                                if worktree.is_primary { "◆" } else { "◇" },
                                worktree.branch,
                                if missing { "  [missing]" } else { "" },
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
                        mono_line(path.into_owned(), colors.muted, 10.0),
                        quiet_line(
                            format!(
                                "{}{done}/{} TASKS · {}",
                                activity
                                    .summary()
                                    .map(|summary| format!("Agents {summary} · "))
                                    .unwrap_or_default(),
                                tasks.len(),
                                short_id(&worktree.id)
                            ),
                            if activity.working > 0 {
                                colors.working
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
                    task.title.clone(),
                    data.selected_task_id == Some(task.id.as_str()),
                    color,
                    vec![
                        div()
                            .flex()
                            .gap(ui_text::space(6.0))
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
            for chat in data
                .native_chats
                .iter()
                .filter(|chat| chat.project_id.as_deref() == Some(data.project_id))
            {
                total += 1;
                if !data.session_filter.accepts_chat(chat.provider) {
                    continue;
                }
                let provider = session_catalog::provider_label(chat.provider);
                let status = session_catalog::status_label(&chat.state);
                let title = if chat.title.trim().is_empty() {
                    "Untitled chat"
                } else {
                    &chat.title
                };
                let worktree = data
                    .state
                    .worktrees_for(data.project_id)
                    .into_iter()
                    .find(|tree| chat.worktree_id.as_deref() == Some(tree.id.as_str()))
                    .or_else(|| {
                        data.state
                            .worktrees_for(data.project_id)
                            .into_iter()
                            .filter(|tree| chat.cwd.starts_with(&tree.path))
                            .max_by_key(|tree| tree.path.as_os_str().len())
                    });
                let context = worktree.map(|tree| tree.branch.clone()).unwrap_or_else(|| {
                    chat.worktree_id
                        .as_deref()
                        .map(|id| format!("Worktree {}", short_id(id)))
                        .unwrap_or_else(|| "Project root".into())
                });
                let path = chat.cwd.to_string_lossy();
                if !matches(&[
                    &chat.id,
                    title,
                    provider,
                    status,
                    &context,
                    &path,
                    chat.worktree_id.as_deref().unwrap_or(""),
                ]) {
                    continue;
                }
                rows.push(row(
                    format!("session-chat-{}", chat.id),
                    format!(
                        "{provider} chat · {title} · {status} · {context} · {}",
                        chat.id
                    ),
                    false,
                    colors.cyan,
                    vec![
                        line(format!("{provider} · {title}"), colors.text, 11.0),
                        line(format!("{status} · {context}"), colors.muted, 10.0),
                        mono_line(path.into_owned(), colors.muted, 10.0),
                        mono_line(chat.id.clone(), colors.muted, 10.0),
                    ],
                    PanelAction::Chat {
                        project: data.project_id.into(),
                        generation: data.session_catalog_generation,
                        id: chat.id.clone(),
                    },
                    on_action.clone(),
                    cx,
                ));
            }
            for shell in data.shells.iter().filter(|shell| {
                shell.kind == ShellKind::Project
                    && shell.project_id.as_deref() == Some(data.project_id)
            }) {
                total += 1;
                if !data.session_filter.accepts_shell() {
                    continue;
                }
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
                    "shell",
                    shell.harness.map(|harness| harness.program()).unwrap_or(""),
                    &shell.id,
                    label,
                    &path,
                    command,
                    if shell.alive { "live" } else { "exited" },
                ]) {
                    continue;
                }
                let metrics = data.metrics.get(&shell.id).copied().unwrap_or_default();
                let agent = data.activity.get(&shell.id);
                let status = ui_text::quiet(shell_status_label(shell.alive, agent)).to_string();
                let working = agent.is_some_and(|state| state.activity == AgentActivity::Working);
                if ui_text::is_native() {
                    let lines = native_shell_lines(
                        NativeShell {
                            title: format!("Shell · {label}"),
                            status: native_shell_status(shell.alive, agent),
                            path: path.into_owned(),
                            detail: format!(
                                "CPU {:.1}% · RAM {} · {command}",
                                metrics.cpu_percent,
                                format_bytes(metrics.ram_bytes)
                            ),
                            id: short_id(&shell.id),
                        },
                        colors,
                    );
                    rows.push(row(
                        format!("shell-{}", shell.id),
                        format!("Shell · {label} · {status} · {}", shell.id),
                        false,
                        colors.cyan,
                        lines,
                        PanelAction::Shell {
                            project: data.project_id.into(),
                            generation: data.session_catalog_generation,
                            id: shell.id.clone(),
                        },
                        on_action.clone(),
                        cx,
                    ));
                    continue;
                }
                rows.push(row(
                    format!("shell-{}", shell.id),
                    format!("Shell · {label} · {status} · {}", shell.id),
                    false,
                    colors.cyan,
                    vec![
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(ui_text::space(6.0))
                            .child(line(
                                format!("Shell · {} · {label}", short_id(&shell.id)),
                                colors.text,
                                11.0,
                            ))
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(rgb(if !shell.alive {
                                        colors.magenta
                                    } else if working {
                                        colors.working
                                    } else {
                                        colors.cyan
                                    }))
                                    .child(status),
                            )
                            .into_any_element(),
                        mono_line(path.into_owned(), colors.muted, 10.0),
                        mono_line(
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
                        mono_line(shell.id.clone(), colors.muted, 10.0),
                    ],
                    PanelAction::Shell {
                        project: data.project_id.into(),
                        generation: data.session_catalog_generation,
                        id: shell.id.clone(),
                    },
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
    let native = ui_text::is_native();
    if rows.is_empty() {
        let empty = if query.is_empty() {
            ui_text::cased(format!("No {}", panel_noun(kind)))
        } else {
            ui_text::cased("No matches")
        };
        rows.push(if native {
            controls::empty_state(
                if query.is_empty() {
                    Icon::Panel(kind).symbol()
                } else {
                    "magnifyingglass"
                },
                empty,
                colors,
            )
            .into_any_element()
        } else {
            div()
                .p(ui_text::space(10.0))
                .text_color(rgb(colors.muted))
                .child(empty)
                .into_any_element()
        });
    }
    let mut session_controls = Vec::new();
    if kind == PanelKind::Shells && data.selected_remote.is_none() {
        let mut toolbar = div()
            .flex()
            .flex_wrap()
            .gap(ui_text::space(4.0))
            .p(ui_text::space(6.0));
        for filter in Filter::ALL {
            let handler = on_action.clone();
            toolbar = toolbar.child(
                behavior::button(
                    format!("sessions-filter-{}", filter.label()),
                    filter.label(),
                    if data.session_filter == filter {
                        controls::Button::Primary
                    } else {
                        controls::Button::Secondary
                    },
                    colors,
                )
                .aria_selected(data.session_filter == filter)
                .on_click(cx.listener(move |view, _, window, cx| {
                    handler(view, PanelAction::SessionFilter(filter), window, cx);
                })),
            );
        }
        let handler = on_action.clone();
        toolbar = toolbar.child(
            behavior::button(
                "sessions-refresh",
                "Refresh",
                controls::Button::Secondary,
                colors,
            )
            .disabled(data.session_catalog_loading)
            .on_click(cx.listener(move |view, _, window, cx| {
                handler(view, PanelAction::RefreshSessions, window, cx)
            })),
        );
        session_controls.push(toolbar.flex_none().into_any_element());
        if let Some(note) = data.session_catalog_note {
            session_controls.push(
                div()
                    .flex_none()
                    .p(ui_text::space(8.0))
                    .text_size(ui_text::text(10.0))
                    .text_color(rgb(colors.muted))
                    .child(note.to_owned())
                    .into_any_element(),
            );
        } else if data.session_catalog_loading {
            session_controls.push(line("Loading chat history…".to_owned(), colors.muted, 10.0));
        }
    }
    if native {
        let panel = native_panel(
            kind,
            &data,
            NativeChrome {
                count,
                total,
                rows,
                session_controls,
                sort_selector_bounds: Rc::new(Cell::new(Bounds::<Pixels>::default())),
            },
            on_action,
            window,
            cx,
        );
        return panel;
    }

    let as_icons = icons::labels_as_icons(cx);
    let sort_selector_bounds = Rc::new(Cell::new(Bounds::<Pixels>::default()));
    let panel =
        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .min_h_0()
            .bg(rgb(colors.panel))
            .text_size(ui_text::text(11.0))
            .child(
                div()
                    .min_h(ui_text::space(30.0))
                    .flex_none()
                    .flex()
                    .flex_wrap()
                    .border_b_1()
                    .border_color(rgb(if data.search_focused {
                        colors.cyan
                    } else {
                        colors.divider
                    }))
                    // Native's search field draws its own edge; the strip needs no rule.
                    .when(ui_text::is_native(), |strip| strip.border_b_0())
                    .overflow_hidden()
                    .child(div().flex_none().w(px((data.control_inset - 8.0).max(0.0))))
                    .child(panel_search(
                        kind,
                        data.search_input,
                        data.query,
                        false,
                        on_action.clone(),
                        window,
                        cx,
                    ))
                    .children((kind == PanelKind::Projects).then(|| {
                        project_header_button(
                            "new-project-folder",
                            HeaderButton {
                                label: "+ Folder",
                                tooltip: "New folder",
                                glyph: ActionGlyph::NewFolder,
                                color: colors.magenta,
                                pad: 6.0,
                                action: PanelAction::CreateFolder,
                            },
                            as_icons,
                            on_action.clone(),
                            cx,
                        )
                    }))
                    .children((kind == PanelKind::Projects).then(|| {
                        project_header_button(
                            "new-project",
                            HeaderButton {
                                label: "+ Project",
                                tooltip: "New project",
                                glyph: ActionGlyph::NewProject,
                                color: colors.cyan,
                                pad: 8.0,
                                action: PanelAction::CreateProject,
                            },
                            as_icons,
                            on_action.clone(),
                            cx,
                        )
                    })),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(ui_text::space(6.0))
                    .px(ui_text::space(8.0))
                    .py(ui_text::space(4.0))
                    .text_size(ui_text::text(10.0))
                    .text_color(rgb(colors.muted))
                    .child(div().flex_1().min_w_0().text_ellipsis().child(
                        if ui_text::is_native() {
                            native_count(count, total, panel_noun(kind))
                        } else {
                            format!("{count:02} / {total:02} {}", name.to_uppercase())
                        },
                    ))
                    .children((kind == PanelKind::Projects).then(|| {
                        project_sort_controls(
                            data.project_order,
                            data.project_sort_ui
                                .expect("Projects surface requires persistent sort UI"),
                            sort_selector_bounds.clone(),
                            on_action.clone(),
                            cx,
                        )
                    })),
            )
            .children(session_controls)
            .child(
                div()
                    .id(format!("{name}-rows"))
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(rows),
            );

    finish_panel(panel, kind, &data, sort_selector_bounds, on_action, cx)
}

/// What every panel ends with: the selected task's detail under the Tasks list, and the
/// Projects sort menu while it is open.
fn finish_panel<V: 'static>(
    mut panel: Div,
    kind: PanelKind,
    data: &PanelData<'_>,
    _sort_selector_bounds: Rc<Cell<Bounds<Pixels>>>,
    _on_action: impl Fn(&mut V, PanelAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    if kind == PanelKind::Tasks {
        if let Some(remote) = data.selected_remote {
            if let Some(task) = data
                .selected_task_id
                .and_then(|id| remote.tasks.items.iter().find(|task| task.id == id))
            {
                panel = panel.child(task_detail(
                    task.status,
                    &task.title,
                    &remote_worktree_label(remote, task.worktree_id.as_deref()),
                    &task.details,
                    &task.id,
                    colors,
                ));
            }
        } else if let Some(task) = data.selected_task_id.and_then(|id| {
            data.state
                .tasks
                .iter()
                .find(|task| task.id == id && task.project_id == data.project_id)
        }) {
            panel = panel.child(task_detail(
                task.status,
                &task.title,
                worktree_label(data.state, task.worktree_id.as_deref()),
                &task.details,
                &task.id,
                colors,
            ));
        }
    }
    panel.into_any_element()
}

/// The chrome owns layout, while the retained Base child owns all editing.
/// Pointer focus targets this surface's state instead of the active-pane search.
/// Once something is typed, the field ends in its clear button, which empties every
/// search and keeps this field's editor focused.
#[allow(clippy::too_many_arguments)]
fn panel_search<V: 'static>(
    kind: PanelKind,
    input: Option<&Entity<crate::text_input::InputState>>,
    query: &str,
    native: bool,
    on_action: impl Fn(&mut V, PanelAction, &mut Window, &mut Context<V>) + 'static,
    window: &Window,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let name = kind.name();
    let focused = input.is_some_and(|input| input.read(cx).focus_handle(cx).is_focused(window));
    let search = div()
        .id(format!("{name}-search"))
        .relative()
        .cursor_text()
        .flex()
        .items_center()
        .min_w_0();
    let search = if native {
        controls::search_field(search, focused, colors)
    } else {
        search
            .flex_1()
            .min_w(ui_text::space(128.0))
            .h(ui_text::space(30.0))
            .px(ui_text::space(8.0))
            .gap(ui_text::space(5.0))
            .text_color(rgb(if focused { colors.text } else { colors.muted }))
    };
    search
        .child(
            div()
                .id(format!("{name}-search-icon"))
                .flex_none()
                .w(ui_text::space(12.0))
                .h(ui_text::space(12.0))
                .flex()
                .items_center()
                .justify_center()
                .child(icons::mark("⌕", 10.0, colors.muted))
                .test_support(),
        )
        .children(input.map(|input| {
            crate::form_input::search_frame(format!("{name}-search-input"), input, window, cx)
                .accessibility_label(format!("Search {}", panel_noun(kind)))
        }))
        .children((!query.is_empty()).then(|| {
            let input = input.cloned();
            search_clear_button(name, colors).on_click(cx.listener(move |view, _, window, cx| {
                cx.stop_propagation();
                on_action(view, PanelAction::ClearSearch, window, cx);
                if let Some(input) = &input {
                    input.read(cx).focus_handle(cx).focus(window, cx);
                }
            }))
        }))
        .when_some(input.cloned(), |search, input| {
            search.on_mouse_down(MouseButton::Left, move |_, window, cx| {
                let state = input.read(cx);
                if !state.presentation().is_disabled() {
                    state.focus_handle(cx).focus(window, cx);
                    // The text child already hit-tests the caret. Only forward
                    // icon/chrome focus and protect it from ancestor defaults.
                    window.prevent_default();
                }
            })
        })
        .test_support()
        .into_any_element()
}

/// The numbers a Native panel's header and list are drawn from.
struct NativeChrome {
    count: usize,
    total: usize,
    rows: Vec<AnyElement>,
    session_controls: Vec<AnyElement>,
    sort_selector_bounds: Rc<Cell<Bounds<Pixels>>>,
}

/// The clear button inside a search field: a muted filled cross, brought to the text color
/// under the pointer or with keyboard focus. The caller adds the click.
fn search_clear_button(name: &str, colors: theme::Palette) -> crate::behavior_controls::Button {
    crate::behavior_controls::button_content(
        format!("{name}-search-clear"),
        "Clear the search",
        if ui_text::is_native() {
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
    .child(crate::tooltip::anchor(
        "Clear the search",
        crate::tooltip::Look::Control,
    ))
}

/// A list panel under Native: the shared header (its name, a count and, for Projects, the
/// sort and create buttons), the search field, and the inset rows.
fn native_panel<V: Render + 'static>(
    kind: PanelKind,
    data: &PanelData<'_>,
    chrome: NativeChrome,
    on_action: impl Fn(&mut V, PanelAction, &mut Window, &mut Context<V>) + Clone + 'static,
    window: &Window,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let NativeChrome {
        count,
        total,
        rows,
        session_controls,
        sort_selector_bounds,
    } = chrome;
    let mut meta = native_count(count, total, panel_noun(kind));
    // The lists of one project say which.
    if kind != PanelKind::Projects {
        let project = data
            .selected_remote
            .map(|remote| remote.project_name.clone())
            .or_else(|| {
                data.state
                    .projects
                    .iter()
                    .find(|project| project.id == data.project_id)
                    .map(|project| project.name.clone())
            });
        if let Some(project) = project {
            meta = format!("{meta} · {project}");
        }
    }
    let mut actions = Vec::new();
    if kind == PanelKind::Projects {
        actions.push(project_sort_button(
            data.project_order,
            data.project_sort_ui
                .expect("Projects surface requires persistent sort UI"),
            sort_selector_bounds.clone(),
            on_action.clone(),
            cx,
        ));
        for (id, button) in [
            (
                "new-project-folder",
                HeaderButton {
                    label: "+ Folder",
                    tooltip: "New folder",
                    glyph: ActionGlyph::NewFolder,
                    color: colors.magenta,
                    pad: 6.0,
                    action: PanelAction::CreateFolder,
                },
            ),
            (
                "new-project",
                HeaderButton {
                    label: "+ Project",
                    tooltip: "New project",
                    glyph: ActionGlyph::NewProject,
                    color: colors.cyan,
                    pad: 8.0,
                    action: PanelAction::CreateProject,
                },
            ),
        ] {
            actions.push(project_header_button(
                id,
                button,
                true,
                on_action.clone(),
                cx,
            ));
        }
    }
    let name = kind.name();
    // One field: the magnifier, the text and, once something is typed, its clear button, all
    // on the field's own fill. The text box inside draws no box of its own.
    let search = panel_search(
        kind,
        data.search_input,
        data.query,
        true,
        on_action.clone(),
        window,
        cx,
    );
    let panel = controls::panel(colors)
        .relative()
        .child(controls::panel_header(
            kind.label(),
            Some(meta.into()),
            actions,
            colors,
        ))
        .child(search)
        .children(session_controls)
        .child(
            div()
                .id(format!("{name}-rows"))
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .flex_col()
                .gap(ui_text::space(1.0))
                .pb(ui_text::space(controls::LIST_MARGIN))
                .overflow_y_scroll()
                .children(rows),
        );
    finish_panel(panel, kind, data, sort_selector_bounds, on_action, cx)
}

/// Native's sort control for Projects: a header button whose menu picks the order and
/// its direction, as Finder's View menu does.
fn project_sort_button<V: 'static>(
    order: ProjectOrder,
    ui: &ProjectSortUi,
    selector_bounds: Rc<Cell<Bounds<Pixels>>>,
    on_action: impl Fn(&mut V, PanelAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let popup_bounds = selector_bounds.clone();
    let popup_action = on_action.clone();
    let state = ui.state.clone();
    let open = state.read(cx).is_open();
    let button = crate::project_settings::kit_toolbar_button(
        "project-sort-selector",
        "arrow.up.arrow.down",
        format!(
            "Sort by {} · {}",
            order.by.label().to_lowercase(),
            order.direction_label()
        ),
        true,
        colors,
    );
    let trigger = if open {
        button
            .bg(rgb(colors.panel_active))
            .text_color(rgb(colors.text))
    } else {
        button
    }
    .track_focus(&ui.trigger_focus)
    .aria_expanded(open)
    .relative()
    .child(
        canvas(
            move |bounds, _, _| selector_bounds.set(bounds),
            |_, _, _, _| {},
        )
        .absolute()
        .inset_0(),
    )
    .on_click(cx.listener(move |view, _, window, cx| {
        state.update(cx, |state, cx| state.sync_open(!open, window, cx));
        on_action(view, PanelAction::SetProjectSortMenuOpen(!open), window, cx);
    }))
    .into_any_element();
    anchored_sort_menu(trigger, order, ui, popup_bounds, popup_action, cx)
}

/// A create button in the Projects header.
struct HeaderButton {
    label: &'static str,
    tooltip: &'static str,
    glyph: ActionGlyph,
    color: u32,
    /// Side padding of the text button; the icon button has a fixed width instead.
    pad: f32,
    action: PanelAction,
}

/// The header keeps its words unless icons are on, when the glyph takes their place
/// and the tooltip carries the name. The button keeps its colour and full height.
fn project_header_button<V: 'static>(
    id: &'static str,
    button: HeaderButton,
    as_icon: bool,
    on_action: impl Fn(&mut V, PanelAction, &mut Window, &mut Context<V>) + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let HeaderButton {
        label,
        tooltip,
        glyph,
        color,
        pad,
        action,
    } = button;
    // Native draws both as bare symbols with their names in tooltips, like the pane's own
    // buttons on the bar above: muted, in a round hover, turning primary under the pointer.
    if ui_text::is_native() {
        return crate::project_settings::kit_toolbar_button(
            id,
            Icon::Action(glyph).symbol(),
            tooltip,
            true,
            colors,
        )
        .on_click(cx.listener(move |view, _, window, cx| {
            on_action(view, action.clone(), window, cx);
        }))
        .into_any_element();
    }
    behavior::button_content(
        id,
        tooltip,
        if as_icon {
            icons::icon(Icon::Action(glyph), color)
        } else {
            div().child(ui_text::cased(label)).into_any_element()
        },
    )
    .flex_none()
    .h(ui_text::space(30.0))
    .flex()
    .items_center()
    .text_color(rgb(color))
    .hover(|style| style.bg(rgb(colors.panel_active)))
    .map(|button| {
        if as_icon {
            button
                .w(ui_text::space(28.0))
                .justify_center()
                .child(tooltip::anchor(tooltip, Look::Control))
        } else {
            button.px(ui_text::space(pad))
        }
    })
    .on_click(cx.listener(move |view, _, window, cx| {
        on_action(view, action.clone(), window, cx);
    }))
    .into_any_element()
}

fn project_sort_controls<V: 'static>(
    order: ProjectOrder,
    ui: &ProjectSortUi,
    selector_bounds: Rc<Cell<Bounds<Pixels>>>,
    on_action: impl Fn(&mut V, PanelAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let popup_bounds = selector_bounds.clone();
    let popup_action = on_action.clone();
    let state = ui.state.clone();
    let open = state.read(cx).is_open();
    let choose = on_action.clone();
    let trigger = div()
        .flex()
        .flex_none()
        .items_center()
        .gap(ui_text::space(2.0))
        .text_size(ui_text::text(9.0))
        .child(
            behavior::button_content("project-sort-selector", "Sort projects", order.by.label())
                .track_focus(&ui.trigger_focus)
                .aria_expanded(open)
                .relative()
                .flex()
                .items_center()
                .gap(ui_text::space(5.0))
                .px(ui_text::space(5.0))
                .h(ui_text::space(20.0))
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
                .map(|control| {
                    controls::native(control, |control| {
                        control.rounded_full().px(ui_text::space(7.0))
                    })
                })
                .child(icons::text_mark(if open { "▴" } else { "▾" }, 8.0))
                .child(
                    canvas(
                        move |bounds, _, _| selector_bounds.set(bounds),
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .inset_0(),
                )
                .when(!open, |control| {
                    control.child(tooltip::anchor(
                        "Sort projects within each folder",
                        Look::Control,
                    ))
                })
                .on_click(cx.listener(move |view, _, window, cx| {
                    state.update(cx, |state, cx| state.sync_open(!open, window, cx));
                    choose(view, PanelAction::SetProjectSortMenuOpen(!open), window, cx);
                })),
        )
        .child(
            behavior::toggle_content(
                "project-sort-direction",
                order.direction_label(),
                icons::text_mark(if order.descending { "↓" } else { "↑" }, 9.0),
                order.descending,
            )
            .flex()
            .items_center()
            .justify_center()
            .w(ui_text::space(22.0))
            .h(ui_text::space(20.0))
            .text_color(rgb(colors.cyan))
            .hover(move |style| {
                let style = style.bg(rgb(colors.panel_active));
                if ui_text::is_native() {
                    style.text_color(rgb(colors.text))
                } else {
                    style
                }
            })
            // Native's arrow is muted like the order beside it, primary under the pointer.
            .map(|control| {
                controls::native(control, |control| {
                    control.rounded_full().text_color(rgb(colors.muted))
                })
            })
            .when(!open, |control| {
                control.child(tooltip::anchor(order.direction_label(), Look::Control))
            })
            .on_change({
                let listener = cx.listener(move |view, _, window, cx| {
                    on_action(
                        view,
                        PanelAction::SetProjectOrder(order.toggled()),
                        window,
                        cx,
                    );
                });
                move |_, event, window, cx| listener(event, window, cx)
            }),
        )
        .into_any_element();
    anchored_sort_menu(trigger, order, ui, popup_bounds, popup_action, cx)
}

/// Base Popup owns measurement/deferred layering, Base PopoverState owns focus.
/// The button owns activation exactly once; no enclosing Confirm handler competes.
fn anchored_sort_menu<V: 'static>(
    trigger: AnyElement,
    order: ProjectOrder,
    ui: &ProjectSortUi,
    bounds: Rc<Cell<Bounds<Pixels>>>,
    action: impl Fn(&mut V, PanelAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let state = ui.state.clone();
    let open = state.read(cx).is_open();
    gpui_kit::base::Popup::new("project-sort-popup", trigger)
        .anchor(gpui::Anchor::TopRight)
        .offset(ui_text::space(4.0))
        .when(open, |popup| {
            let close_state = state.clone();
            popup.content(project_sort_menu(
                order,
                bounds,
                state,
                move |view, event, window, cx| {
                    close_state.update(cx, |state, cx| state.dismiss(window, cx));
                    action(view, event, window, cx);
                },
                cx,
            ))
        })
        .into_any_element()
}

fn project_sort_menu<V: 'static>(
    order: ProjectOrder,
    selector_bounds: Rc<Cell<Bounds<Pixels>>>,
    state: Entity<gpui_kit::base::PopoverState>,
    on_action: impl Fn(&mut V, PanelAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let dismiss = on_action.clone();
    let escape = on_action.clone();
    let escape_state = state.clone();
    let focus = state.read(cx).focus_handle(cx);
    let menu = div()
        .id("project-sort-menu")
        .w(ui_text::space(220.0))
        .max_w(gpui::relative(0.9))
        .max_h(ui_text::space(128.0))
        .overflow_y_scroll()
        .bg(rgb(colors.panel_active))
        .border_1()
        .border_color(rgb(colors.magenta))
        .p(ui_text::space(3.0))
        .map(|menu| {
            controls::native(menu, |menu| {
                controls::menu(menu, colors)
                    .w(ui_text::space(200.0))
                    .bottom_auto()
                    .max_h(ui_text::space(220.0))
            })
        })
        .occlude()
        .on_key_down(
            cx.listener(move |view, event: &gpui::KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "escape" => {
                        escape_state.update(cx, |state, cx| state.dismiss(window, cx));
                        escape(view, PanelAction::CloseProjectSortMenu, window, cx);
                    }
                    "tab" | "up" | "down" => {
                        crate::project_settings::modal_tab(
                            event.keystroke.key == "up" || event.keystroke.modifiers.shift,
                            window,
                            cx,
                        );
                    }
                    _ => return,
                }
                window.prevent_default();
                cx.stop_propagation();
            }),
        )
        .on_mouse_down_out(
            cx.listener(move |view, event: &gpui::MouseDownEvent, window, cx| {
                if selector_bounds.get().contains(&event.position) {
                    return;
                }
                state.update(cx, |state, cx| state.dismiss(window, cx));
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
                let selected = by == order.by;
                let next = if selected {
                    order
                } else {
                    ProjectOrder::for_sort(by)
                };
                sort_menu_row(
                    format!("project-sort-{}", by.label()),
                    by.label(),
                    selected,
                    next,
                    on_action.clone(),
                    cx,
                )
            }),
        )
        // Native's menu also holds the direction, as Finder's sort menus do.
        .when(ui_text::is_native(), |menu| {
            menu.child(controls::menu_separator(colors))
                .children([false, true].map(|descending| {
                    let next = ProjectOrder {
                        descending,
                        ..order
                    };
                    sort_menu_row(
                        format!("project-sort-descending-{descending}"),
                        next.direction_label(),
                        order.descending == descending,
                        next,
                        on_action.clone(),
                        cx,
                    )
                }))
        });
    behavior::focus_scope(menu, "project-sort-focus-trap", &focus).into_any_element()
}

/// One choice in the Projects sort menu, ticked when it is the current one.
fn sort_menu_row<V: 'static>(
    id: String,
    label: &'static str,
    selected: bool,
    next: ProjectOrder,
    action: impl Fn(&mut V, PanelAction, &mut Window, &mut Context<V>) + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    behavior::button_content(
        id,
        label,
        div()
            .flex_none()
            .w(ui_text::space(12.0))
            .when(selected, |check| {
                check.child(if ui_text::is_native() {
                    icons::text_mark("✓", 10.0)
                } else {
                    "✓".into_any_element()
                })
            }),
    )
    .aria_selected(selected)
    .flex()
    .items_center()
    .gap(ui_text::space(7.0))
    .focus_visible(move |style| {
        style
            .bg(rgb(controls::menu_row_hover(colors)))
            .text_color(rgb(colors.cyan))
    })
    .px(ui_text::space(8.0))
    .py(ui_text::space(7.0))
    .text_size(ui_text::text(10.0))
    .text_color(rgb(if selected { colors.cyan } else { colors.text }))
    .hover(move |style| {
        controls::hovered(style, controls::menu_row_hover(colors), |style| {
            style.bg(rgb(colors.divider)).text_color(rgb(colors.cyan))
        })
    })
    .map(|row| controls::native(row, |row| controls::menu_row(row, colors)))
    .child(div().flex_1().min_w_0().text_ellipsis().child(label))
    .on_click(cx.listener(move |view, _, window, cx| {
        action(view, PanelAction::SetProjectOrder(next), window, cx);
    }))
    .into_any_element()
}

/// Preserve Native's pointer hover policy and reveal the action strip whenever
/// keyboard focus is on the row or one of its nested Base controls.
fn project_action_strip(
    native: bool,
    selected: bool,
    row_focus: &FocusHandle,
    window: &Window,
    cx: &gpui::App,
) -> Div {
    let keyboard_within =
        window.last_input_was_keyboard() && row_focus.contains_focused(window, cx);
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(ui_text::space(4.0))
        .when(native && !selected && !keyboard_within, |strip| {
            strip
                .invisible()
                .group_hover(PROJECT_ROW_GROUP, |style| style.visible())
        })
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
    behavior::button_content(
        format!("{name}-project-{project_id}"),
        tooltip,
        icons::text_mark(mark, 10.0),
    )
    .flex_none()
    .flex()
    .items_center()
    .justify_center()
    .w(ui_text::space(20.0))
    .h(ui_text::space(18.0))
    .text_color(rgb(colors.cyan))
    .focus_visible(move |style| style.bg(rgb(colors.divider)))
    .hover(move |style| {
        let style = style.bg(rgb(colors.divider));
        if ui_text::is_native() {
            style.text_color(rgb(colors.text))
        } else {
            style.text_color(rgb(colors.magenta))
        }
    })
    // Native's row controls are muted symbols in a round hover, like the bar's buttons.
    .map(|control| {
        controls::native(control, |control| {
            control
                .size(ui_text::space(20.0))
                .rounded_full()
                .text_color(rgb(colors.muted))
        })
    })
    .child(tooltip::anchor(tooltip, Look::Control))
    // Let Base/GPUI transfer pointer focus before isolating activation. A
    // mouse-down stop skips that earlier listener in reverse bubble dispatch.
    .on_click(cx.listener(move |view, _, window, cx| {
        cx.stop_propagation();
        on_action(view, action.clone(), window, cx);
    }))
    .map(|control| {
        crate::form_input::control_element(format!("{name}-project-{project_id}"), control)
    })
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
    behavior::toggle_content(
        format!("notifications-project-{project_id}"),
        "Agent completion notifications",
        icons::text_icon(
            if enabled { Icon::Bell } else { Icon::BellOff },
            9.6,
            if enabled { colors.cyan } else { colors.muted },
        ),
        enabled,
    )
    .flex_none()
    .flex()
    .items_center()
    .justify_center()
    .w(ui_text::space(20.0))
    .h(ui_text::space(18.0))
    .focus_visible(move |style| style.bg(rgb(colors.divider)))
    .hover(move |style| {
        let style = style.bg(rgb(colors.divider));
        if ui_text::is_native() {
            style.text_color(rgb(colors.text))
        } else {
            style
        }
    })
    // Native draws the bell as its row's other controls: the size of their symbols, in
    // a round hover, primary while notifications are on.
    .map(|control| {
        controls::native(control, |control| {
            control
                .size(ui_text::space(20.0))
                .rounded_full()
                .text_color(rgb(if enabled { colors.text } else { colors.muted }))
        })
    })
    .child(tooltip::anchor(
        if enabled {
            "Agent completion notifications on · click to disable"
        } else {
            "Agent completion notifications off · click to enable"
        },
        Look::Control,
    ))
    // Keep Base's pointer focus transfer; the domain activation stays isolated.
    .on_change({
        let listener = cx.listener(move |view, _, window, cx| {
            cx.stop_propagation();
            on_action(
                view,
                PanelAction::ToggleProjectNotifications(project_id.clone()),
                window,
                cx,
            );
        });
        move |_, event, window, cx| listener(event, window, cx)
    })
    .into_any_element()
}

/// The detail of the selected task under the Tasks list.
fn task_detail(
    status: TaskStatus,
    title: &str,
    worktree: &str,
    details: &str,
    id: &str,
    colors: Palette,
) -> AnyElement {
    let (_, color) = task_mark(status, colors);
    div()
        .id("task-detail")
        .flex_none()
        .min_h_0()
        .max_h(ui_text::space(200.0))
        .overflow_y_scroll()
        .border_t_1()
        .border_color(rgb(colors.divider))
        .p(ui_text::space(8.0))
        .child(if ui_text::is_native() {
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(colors.muted))
                .child("Task detail")
        } else {
            div().text_color(rgb(colors.gold)).child("TASK DETAIL")
        })
        .child(
            div()
                .pt(ui_text::space(5.0))
                .text_color(rgb(colors.text))
                .child(title.to_owned()),
        )
        .child(
            div()
                .pt(ui_text::space(4.0))
                .text_color(rgb(color))
                .child(format!("{} · @ {worktree}", status.as_str())),
        )
        .child(
            div()
                .pt(ui_text::space(6.0))
                .text_color(rgb(colors.muted))
                .child(if details.is_empty() {
                    "No details".to_owned()
                } else {
                    details.to_owned()
                }),
        )
        .child(line(id.to_owned(), colors.muted, 10.0))
        .into_any_element()
}

/// The dot beside a host: its color is the link state, and its hint says it in words.
fn link_color(link: Option<Link>, colors: Palette) -> u32 {
    match link {
        Some(Link::Online) => colors.cyan,
        Some(Link::Offline) => colors.muted,
        Some(Link::Connecting) | None => colors.gold,
    }
}

/// A paired Mac as a folder: its heading, what is wrong with it if anything, and its
/// projects as ordinary rows. It cannot be renamed, moved or removed here; that is done in
/// Settings.
fn push_remote_folder<V: 'static>(
    rows: &mut Vec<AnyElement>,
    folder: &FolderView,
    on_action: impl Fn(&mut V, PanelAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) {
    let colors = theme::palette(cx);
    let toggle_action = on_action.clone();
    let toggle = PanelAction::ToggleFolder(folder_key(&folder.host));
    let online = folder.link == Some(Link::Online);
    let offline = folder.link == Some(Link::Offline);
    let hint = folder.link.map_or("Connecting…", Link::text);
    rows.push(
        folder_bar(
            format!("remote-folder-{}", folder.host),
            &folder.label,
            if folder.collapsed { "▸" } else { "▾" },
            0,
            colors,
        )
        .aria_expanded(!folder.collapsed)
        .child(
            div()
                .relative()
                .flex_none()
                .text_color(rgb(link_color(folder.link, colors)))
                .child(if offline { "○" } else { "●" })
                .child(tooltip::anchor(hint, Look::Control)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_ellipsis()
                .overflow_hidden()
                .child(folder.label.clone()),
        )
        .children(folder.creating.then(|| {
            div()
                .text_size(ui_text::text(9.0))
                .text_color(rgb(colors.muted))
                .child("creating…")
        }))
        .child(
            div()
                .text_size(ui_text::text(9.0))
                .text_color(rgb(colors.muted))
                .child(format!("{:02}", folder.count)),
        )
        .children(online.then(|| {
            project_control(
                &folder.host,
                "remote-new-project",
                "+",
                "New project on this Mac",
                PanelAction::Remote(RemoteAction::NewProject(folder.host.clone())),
                on_action.clone(),
                cx,
            )
        }))
        .child(tooltip::anchor(
            "A paired Mac. Its projects open here like local ones.",
            Look::Control,
        ))
        .on_click(cx.listener(move |view, _, window, cx| {
            toggle_action(view, toggle.clone(), window, cx);
        }))
        .into_any_element(),
    );
    if let Some(message) = &folder.failure {
        rows.push(failure_row(
            format!("remote-failure-{}", folder.host),
            message,
            1,
            PanelAction::Remote(RemoteAction::DismissProject(folder.host.clone())),
            on_action.clone(),
            cx,
        ));
    }
    if folder.collapsed {
        return;
    }
    if let Some(note) = &folder.note {
        rows.push(
            div()
                .pl(px(project_indent(1)))
                .pr(ui_text::space(8.0))
                .py(ui_text::space(5.0))
                .text_size(ui_text::text(10.0))
                .text_color(rgb(if offline { colors.gold } else { colors.muted }))
                .child(note.clone())
                .into_any_element(),
        );
    }
    for project in &folder.projects {
        rows.push(remote_project_row(project, on_action.clone(), cx));
    }
}

/// A project of a paired Mac, drawn as a local one is: name, then trees, tasks and live
/// shells. Choosing it makes it the window's project.
fn remote_project_row<V: 'static>(
    project: &ProjectView,
    on_action: impl Fn(&mut V, PanelAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    project_row(
        &project.key,
        &project.name,
        None,
        ProjectRowLook {
            selected: project.selected,
            depth: 1,
            dimmed: project.dimmed,
            focus: None,
        },
        vec![
            div()
                .flex()
                .items_center()
                .gap(ui_text::space(4.0))
                .child(div().flex_1().min_w_0().child(line(
                    project.name.clone(),
                    colors.text,
                    11.0,
                )))
                .into_any_element(),
            line(project.stats.line(), colors.muted, 9.0),
        ],
        PanelAction::Project(project.key.clone()),
        on_action,
        cx,
    )
}

/// What a refused or lost creation left, with a way to dismiss it.
fn failure_row<V: 'static>(
    id: String,
    message: &str,
    depth: usize,
    dismiss: PanelAction,
    on_action: impl Fn(&mut V, PanelAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    div()
        .id(id.clone())
        .flex()
        .items_start()
        .gap(ui_text::space(6.0))
        .pl(px(project_indent(depth)))
        .pr(ui_text::space(8.0))
        .py(ui_text::space(4.0))
        .text_size(ui_text::text(10.0))
        .text_color(rgb(colors.gold))
        .child(div().flex_1().min_w_0().child(message.to_owned()))
        .child(
            behavior::button_content(format!("{id}-dismiss"), "Dismiss failure", "×")
                .flex_none()
                .px(ui_text::space(4.0))
                .text_color(rgb(colors.muted))
                .hover(|style| style.text_color(rgb(colors.text)))
                .child(tooltip::anchor("Dismiss", Look::Control))
                .on_click(cx.listener(move |view, _, window, cx| {
                    on_action(view, dismiss.clone(), window, cx);
                })),
        )
        .into_any_element()
}

/// The branch of a remote worktree, as a local one is named, or the word for none.
fn remote_worktree_label(remote: &SelectedView, id: Option<&str>) -> String {
    id.and_then(|id| {
        remote
            .worktrees
            .items
            .iter()
            .find(|worktree| worktree.id == id)
    })
    .map_or_else(
        || ui_text::cased("Unassigned").to_string(),
        |worktree| worktree.branch.clone(),
    )
}

/// A line of the Worktrees, Tasks or Shells panel that says why the list is not (yet) full.
fn listing_note<V: 'static, T>(
    rows: &mut Vec<AnyElement>,
    listing: &Listing<T>,
    colors: Palette,
    _: &mut Context<V>,
) {
    let (text, color) = match (&listing.error, listing.loading) {
        (Some(error), _) => (error.clone(), colors.gold),
        (None, true) => ("Loading…".to_owned(), colors.muted),
        (None, false) => return,
    };
    rows.push(
        div()
            .px(ui_text::space(8.0))
            .py(ui_text::space(3.0))
            .text_size(ui_text::text(10.0))
            .text_color(rgb(color))
            .child(text)
            .into_any_element(),
    );
}

/// Dims what may be out of date, as a project's lists while its host cannot be reached.
fn dimmed_if(element: AnyElement, dimmed: bool) -> AnyElement {
    if dimmed {
        div().opacity(0.5).child(element).into_any_element()
    } else {
        element
    }
}

/// The Worktrees, Tasks or Shells panel of a project on another Mac, drawn like the local
/// one from the host's lists. Returns how many entries the list has before the search.
fn push_remote_panel<V: 'static>(
    rows: &mut Vec<AnyElement>,
    kind: PanelKind,
    remote: &SelectedView,
    data: &PanelData<'_>,
    matches: &dyn Fn(&[&str]) -> bool,
    on_action: impl Fn(&mut V, PanelAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> usize {
    let colors = theme::palette(cx);
    let note = |text: String, color: u32| {
        div()
            .px(ui_text::space(8.0))
            .py(ui_text::space(3.0))
            .text_size(ui_text::text(10.0))
            .text_color(rgb(color))
            .child(text)
            .into_any_element()
    };
    if remote.unpaired {
        rows.push(note(
            "This Mac is no longer paired. Add it again in Settings.".to_owned(),
            colors.gold,
        ));
        return 0;
    }
    let dimmed = remote.link == Some(Link::Offline);
    match remote.link {
        Some(Link::Online) => {}
        Some(Link::Offline) => rows.push(note(Link::Offline.text().to_owned(), colors.gold)),
        Some(Link::Connecting) | None => rows.push(note(
            format!("Connecting to {}…", remote.host_label),
            colors.muted,
        )),
    }
    let mut total = 0;
    match kind {
        PanelKind::Worktrees => {
            listing_note(rows, &remote.worktrees, colors, cx);
            for worktree in &remote.worktrees.items {
                total += 1;
                if !matches(&[&worktree.id, &worktree.branch, &worktree.path]) {
                    continue;
                }
                let tasks = remote
                    .tasks
                    .items
                    .iter()
                    .filter(|task| task.worktree_id.as_deref() == Some(worktree.id.as_str()))
                    .collect::<Vec<_>>();
                let done = tasks
                    .iter()
                    .filter(|task| task.status == TaskStatus::Done)
                    .count();
                let selected = data.selected_worktree_id == Some(worktree.id.as_str());
                let lines = if ui_text::is_native() {
                    native_worktree_lines(
                        NativeWorktree {
                            primary: worktree.primary,
                            branch: &worktree.branch,
                            repository: None,
                            missing: false,
                            path: worktree.path.clone(),
                            counts: format!("{done}/{} tasks", tasks.len()),
                            id: short_id(&worktree.id),
                            selected,
                            working: false,
                        },
                        colors,
                    )
                } else {
                    vec![
                        title_line(
                            format!(
                                "{} {}",
                                if worktree.primary { "◆" } else { "◇" },
                                worktree.branch
                            ),
                            if selected { colors.cyan } else { colors.text },
                            11.0,
                        ),
                        mono_line(worktree.path.clone(), colors.muted, 10.0),
                        quiet_line(
                            format!("{done}/{} TASKS · {}", tasks.len(), short_id(&worktree.id)),
                            colors.muted,
                            10.0,
                        ),
                    ]
                };
                rows.push(dimmed_if(
                    row(
                        format!("remote-worktree-{}", worktree.id),
                        worktree.branch.clone(),
                        selected,
                        colors.magenta,
                        lines,
                        PanelAction::Worktree(worktree.id.clone()),
                        on_action.clone(),
                        cx,
                    ),
                    dimmed,
                ));
            }
        }
        PanelKind::Tasks => {
            listing_note(rows, &remote.tasks, colors, cx);
            for task in &remote.tasks.items {
                total += 1;
                let worktree = remote_worktree_label(remote, task.worktree_id.as_deref());
                if !matches(&[
                    &task.id,
                    &task.title,
                    &task.details,
                    task.status.as_str(),
                    &worktree,
                ]) {
                    continue;
                }
                let (mark, color) = task_mark(task.status, colors);
                rows.push(dimmed_if(
                    row(
                        format!("remote-task-{}", task.id),
                        task.title.clone(),
                        data.selected_task_id == Some(task.id.as_str()),
                        color,
                        vec![
                            div()
                                .flex()
                                .gap(ui_text::space(6.0))
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
                    ),
                    dimmed,
                ));
            }
        }
        _ => {
            rows.push(note("Native Codex and Claude chat history is unavailable on remote hosts. Showing remote shells.".to_owned(), colors.muted));
            if let Some(message) = &remote.failure {
                rows.push(failure_row(
                    format!("remote-shell-failure-{}", remote.project),
                    message,
                    0,
                    PanelAction::Remote(RemoteAction::DismissShell {
                        host: remote.host.clone(),
                        project: remote.project.clone(),
                    }),
                    on_action.clone(),
                    cx,
                ));
            }
            if remote.creating {
                rows.push(note("Starting a terminal…".to_owned(), colors.cyan));
            }
            listing_note(rows, &remote.shells, colors, cx);
            for shell in &remote.shells.items {
                total += 1;
                let label = match (shell.orchestrator, shell.project_id.is_some()) {
                    (true, true) => "P·ORCH".to_owned(),
                    (true, false) => "G·ORCH".to_owned(),
                    (false, _) => remote_worktree_label(remote, shell.worktree_id.as_deref()),
                };
                let command = shell.harness.as_deref().unwrap_or("shell");
                if !matches(&[
                    &shell.id,
                    &label,
                    &shell.cwd,
                    command,
                    if shell.alive { "live" } else { "exited" },
                ]) {
                    continue;
                }
                let lines = if ui_text::is_native() {
                    native_shell_lines(
                        NativeShell {
                            title: label.clone(),
                            status: native_shell_status(shell.alive, None),
                            path: shell.cwd.clone(),
                            detail: command.to_owned(),
                            id: short_id(&shell.id),
                        },
                        colors,
                    )
                } else {
                    vec![
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(ui_text::space(6.0))
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
                                    .child(ui_text::quiet(if shell.alive {
                                        "● LIVE"
                                    } else {
                                        "× EXITED"
                                    })),
                            )
                            .into_any_element(),
                        mono_line(shell.cwd.clone(), colors.muted, 10.0),
                        mono_line(
                            command.to_owned(),
                            if shell.alive {
                                colors.cyan
                            } else {
                                colors.muted
                            },
                            10.0,
                        ),
                        mono_line(shell.id.clone(), colors.muted, 10.0),
                    ]
                };
                rows.push(dimmed_if(
                    row(
                        format!("remote-shell-{}", shell.id),
                        format!("Shell · {label} · {status} · {}", shell.id),
                        false,
                        colors.cyan,
                        lines,
                        PanelAction::Remote(RemoteAction::OpenShell {
                            host: remote.host.clone(),
                            shell: shell.clone(),
                        }),
                        on_action.clone(),
                        cx,
                    ),
                    dimmed,
                ));
            }
        }
    }
    total
}

/// What a panel shows while the window's project is on another Mac.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteSupport {
    /// The host's own lists: worktrees, tasks and shells.
    Lists,
    /// Not about a project at all: the folders of projects, and the settings.
    Global,
    /// The host has no API for it yet. The panel says so rather than show local data.
    Unavailable,
}

pub fn remote_support(kind: PanelKind) -> RemoteSupport {
    match kind {
        PanelKind::Worktrees | PanelKind::Tasks | PanelKind::Shells => RemoteSupport::Lists,
        PanelKind::Projects | PanelKind::Settings => RemoteSupport::Global,
        PanelKind::Files
        | PanelKind::Preview
        | PanelKind::ProjectSettings
        | PanelKind::Schedules
        | PanelKind::Usage => RemoteSupport::Unavailable,
    }
}

/// What a panel with nothing behind it for a project on another Mac says instead of
/// showing the local project's data.
pub fn unavailable<V: 'static>(kind: PanelKind, host: &str, cx: &mut Context<V>) -> AnyElement {
    let colors = theme::palette(cx);
    let what = match kind {
        PanelKind::Files => "Files",
        PanelKind::Preview => "Preview",
        PanelKind::ProjectSettings => "Project settings",
        PanelKind::Schedules => "Schedules",
        PanelKind::Usage => "Usage",
        PanelKind::Projects => "Projects",
        PanelKind::Worktrees => "Worktrees",
        PanelKind::Tasks => "Tasks",
        PanelKind::Shells => "Sessions",
        PanelKind::Settings => "Settings",
    };
    div()
        .id(format!("unavailable-{}", kind.name()))
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(ui_text::space(6.0))
        .p(ui_text::space(16.0))
        .bg(rgb(colors.panel))
        .child(
            div()
                .text_size(ui_text::text(11.0))
                .text_color(rgb(colors.text))
                .child(format!("Not available for a project on {host}")),
        )
        .child(
            div()
                .max_w(ui_text::space(360.0))
                .text_size(ui_text::text(10.0))
                .text_color(rgb(colors.muted))
                .child(format!(
                    "{what} works with this Mac's own projects. Select one of them to use it."
                )),
        )
        .into_any_element()
}

fn project_indent(depth: usize) -> f32 {
    8.0 + depth.min(16) as f32 * 12.0
}

/// The bar of a folder heading, shared by the local folders and the ones that stand for
/// another Mac so the two read as one list.
fn folder_bar(
    id: String,
    name: &str,
    content: impl IntoElement,
    depth: usize,
    colors: Palette,
) -> behavior::Button {
    behavior::button_content(id, name.to_owned(), content)
        .focus_visible(move |style| style.border_color(rgb(colors.focus)))
        .flex()
        .items_center()
        .gap(ui_text::space(5.0))
        .min_w_0()
        .h(ui_text::space(27.0))
        .pl(px(project_indent(depth)))
        .pr(ui_text::space(8.0))
        .mt(ui_text::space(3.0))
        .border_b_1()
        .border_color(rgb(colors.divider))
        .bg(rgb(colors.panel_active))
        .text_color(rgb(colors.magenta))
        // Native: a sidebar section heading, as Finder's, with no bar behind it.
        .map(|bar| {
            controls::native(bar, |bar| {
                bar.mt(ui_text::space(8.0))
                    .h(ui_text::space(22.0))
                    .pl(ui_text::space(
                        controls::PANEL_INSET + NATIVE_LEVEL * depth.min(16) as f32,
                    ))
                    .pr(ui_text::space(controls::PANEL_INSET - 4.0))
                    .border_b_0()
                    .bg(gpui::transparent_black())
                    .text_color(rgb(colors.muted))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_size(ui_text::text(10.0))
                    .focus_visible(move |style| {
                        style.bg(rgb(colors.divider)).text_color(rgb(colors.text))
                    })
            })
        })
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
    folder_bar(
        format!("project-folder-{}", id.unwrap_or("unfiled")),
        name,
        icons::mark(if collapsed { "▸" } else { "▾" }, 9.0, colors.muted),
        depth,
        colors,
    )
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
    .aria_expanded(!collapsed)
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
            .text_size(ui_text::text(9.0))
            .text_color(rgb(colors.muted))
            .child(counter(count)),
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
    .child(tooltip::anchor(
        if editable {
            "Drag this folder to move it; drop projects or folders here"
        } else {
            "Drop projects here to unfile them, or folders to move them to the root"
        },
        Look::Control,
    ))
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

/// How a project row is drawn.
#[derive(Clone)]
struct ProjectRowLook {
    selected: bool,
    depth: usize,
    /// What the row says may be out of date, as when its host cannot be reached.
    dimmed: bool,
    focus: Option<FocusHandle>,
}

/// A project of a folder. A local project can be dragged to another folder; one that belongs
/// to another Mac (`drag` is `None`) stays where its host puts it.
fn project_row<V: 'static>(
    id: &str,
    accessible_name: &str,
    drag: Option<DraggedProjectItem>,
    look: ProjectRowLook,
    children: Vec<AnyElement>,
    action: PanelAction,
    on_action: impl Fn(&mut V, PanelAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let ProjectRowLook {
        selected,
        depth,
        dimmed,
        focus,
    } = look;
    let drag_view = cx.entity();
    let drag_action = on_action.clone();
    let mut children = children.into_iter();
    let first = children.next().unwrap_or_else(|| div().into_any_element());
    behavior::button_content(format!("project-{id}"), accessible_name.to_owned(), first)
        .when_some(focus, |row, focus| row.track_focus(&focus))
        .aria_selected(selected)
        .items_stretch()
        .focus_visible(move |style| style.border_color(rgb(colors.focus)))
        .flex()
        .flex_col()
        .gap(ui_text::space(3.0))
        .min_w_0()
        .pl(px(project_indent(depth)))
        .pr(ui_text::space(8.0))
        .py(ui_text::space(6.0))
        .border_l_1()
        .border_color(rgb(if selected { colors.cyan } else { colors.panel }))
        .bg(rgb(if selected {
            colors.panel_active
        } else {
            colors.panel
        }))
        .when(dimmed, |row| row.opacity(0.5))
        .hover(move |element| {
            controls::hovered(element, controls::row_hover(selected, colors), |element| {
                element.bg(rgb(colors.panel_active))
            })
        })
        .group(PROJECT_ROW_GROUP)
        .map(|row| {
            controls::native(row, |row| {
                sidebar_row(row, selected, colors).pl(native_row_indent(depth))
            })
        })
        .children(children)
        .on_click(cx.listener(move |view, _, window, cx| {
            on_action(view, action.clone(), window, cx);
        }))
        .when_some(drag, |row, drag| {
            row.on_drag(drag, move |drag, _, window, cx| {
                drag_view.update(cx, |view, cx| {
                    drag_action(view, PanelAction::BeginProjectDrag, window, cx);
                });
                cx.new(|_| drag.clone())
            })
        })
        .map(|control| crate::form_input::control_element(format!("project-{id}"), control))
        .into_any_element()
}

/// The group a project row's hover reveals its buttons in.
const PROJECT_ROW_GROUP: &str = "project-row";

/// Native's sidebar row: inset from the panel's edges and rounded, filled when chosen,
/// with no bar along its side.
fn sidebar_row<E: gpui::Styled + gpui::InteractiveElement>(
    row: E,
    selected: bool,
    colors: Palette,
) -> E {
    controls::list_row(row, selected, colors)
        .mx(ui_text::space(controls::LIST_MARGIN))
        .border_l_0()
        .border_0()
        // The shared ring uses a border; Native deliberately removes it.
        // A keyboard-only fill shows focus without changing layout or hover.
        .focus_visible(move |style| style.bg(rgb(colors.divider)))
}

/// How far Native indents a project row's text at `depth`: a level's step is a disclosure
/// chevron and its gap, so a project's name lines up with its folder's.
fn native_row_indent(depth: usize) -> Pixels {
    ui_text::space(
        controls::PANEL_INSET - controls::LIST_MARGIN + NATIVE_LEVEL * depth.min(16) as f32,
    )
}

/// One level of Native's project tree: a chevron and the gap after it.
const NATIVE_LEVEL: f32 = 16.0;

fn row<V: 'static>(
    id: String,
    accessible_name: String,
    selected: bool,
    accent: u32,
    children: Vec<AnyElement>,
    action: PanelAction,
    on_action: impl Fn(&mut V, PanelAction, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let mut children = children.into_iter();
    let first = children.next().unwrap_or_else(|| div().into_any_element());
    behavior::button_content(id, accessible_name, first)
        .aria_selected(selected)
        .items_stretch()
        .focus_visible(move |style| style.border_color(rgb(colors.focus)))
        .flex()
        .flex_col()
        .gap(ui_text::space(3.0))
        .min_w_0()
        .px(ui_text::space(8.0))
        .py(ui_text::space(6.0))
        .border_l_1()
        .border_color(rgb(if selected { accent } else { colors.panel }))
        .bg(rgb(if selected {
            colors.panel_active
        } else {
            colors.panel
        }))
        .hover(move |element| {
            controls::hovered(element, controls::row_hover(selected, colors), |element| {
                element.bg(rgb(colors.panel_active))
            })
        })
        .map(|row| controls::native(row, |row| sidebar_row(row, selected, colors)))
        .children(children)
        .on_click(cx.listener(move |view, _, window, cx| {
            on_action(view, action.clone(), window, cx);
        }))
        .into_any_element()
}

/// What a panel lists, for its count and its empty state: "projects", "shells".
fn panel_noun(kind: PanelKind) -> &'static str {
    match kind {
        PanelKind::ProjectSettings => "project settings",
        PanelKind::Shells => "sessions",
        kind => kind.name(),
    }
}

/// A count as the colorful themes print it, two digits; Native prints the number.
fn counter(count: usize) -> String {
    if ui_text::is_native() {
        count.to_string()
    } else {
        format!("{count:02}")
    }
}

/// Native's panel count: "3 projects", or "2 of 3 projects" while a search hides some.
fn native_count(count: usize, total: usize, noun: &str) -> String {
    // "1 project", not "1 projects": every noun here is a plural in -s.
    let noun = if total == 1 {
        noun.strip_suffix('s').unwrap_or(noun)
    } else {
        noun
    };
    if count == total {
        format!("{total} {noun}")
    } else {
        format!("{count} of {total} {noun}")
    }
}

fn line(text: String, color: u32, size: f32) -> AnyElement {
    line_box(text, color, size).into_any_element()
}

/// A line of technical text, a path, a branch, an id or a command, in the
/// interface's monospace accent face.
fn mono_line(text: String, color: u32, size: f32) -> AnyElement {
    line_box(text, color, size)
        .font_family(ui_text::mono_family())
        .into_any_element()
}

/// A row's title: in the interface face under Native, like a project's name, and in the
/// colorful themes' monospace face elsewhere.
fn title_line(text: String, color: u32, size: f32) -> AnyElement {
    if ui_text::is_native() {
        line(text, color, size)
    } else {
        mono_line(text, color, size)
    }
}

/// A line of composed text with capitalized words, in the theme's case.
fn quiet_line(text: String, color: u32, size: f32) -> AnyElement {
    line(ui_text::quiet(text).to_string(), color, size)
}

fn line_box(text: String, color: u32, size: f32) -> Div {
    div()
        .min_w_0()
        .overflow_hidden()
        .text_ellipsis()
        .text_size(ui_text::text(size))
        .text_color(rgb(color))
        .child(text)
}

/// What Native's worktree row shows.
struct NativeWorktree<'a> {
    primary: bool,
    branch: &'a str,
    repository: Option<String>,
    missing: bool,
    path: String,
    /// The agents and tasks: "Agents 1 working · 2/3 tasks".
    counts: String,
    id: &'a str,
    selected: bool,
    working: bool,
}

/// The width a Native row's detail lines are indented by, so they line up with the
/// title's words after its symbol.
const NATIVE_ROW_SYMBOL: f32 = 10.0;
const NATIVE_ROW_SYMBOL_GAP: f32 = 5.0;

fn native_detail_indent() -> Pixels {
    ui_text::space((NATIVE_ROW_SYMBOL * 1.3).max(14.0) + NATIVE_ROW_SYMBOL_GAP)
}

/// Native's worktree row, set like the other navigation lists: a branch symbol (a folder
/// for the repository's own checkout) and the branch in the interface face, then the path
/// and the counts, with the path and the short id in the monospace accent.
fn native_worktree_lines(worktree: NativeWorktree<'_>, colors: Palette) -> Vec<AnyElement> {
    let symbol = if worktree.primary {
        "folder.fill"
    } else {
        "arrow.triangle.branch"
    };
    let title = div()
        .flex()
        .items_center()
        .gap(ui_text::space(NATIVE_ROW_SYMBOL_GAP))
        .min_w_0()
        .child(icons::symbol(
            symbol,
            NATIVE_ROW_SYMBOL,
            Some(if worktree.selected {
                colors.cyan
            } else {
                colors.muted
            }),
        ))
        .child(
            div().flex_1().min_w_0().child(
                line_box(
                    worktree.branch.to_owned(),
                    colors.text,
                    controls::ROW_TITLE_TEXT,
                )
                .when(worktree.selected, |title| {
                    title.font_weight(FontWeight::MEDIUM)
                })
                .children(worktree.repository.map(|repository| {
                    div()
                        .text_color(rgb(colors.muted))
                        .child(format!(" · {repository}"))
                }))
                .flex(),
            ),
        )
        .children(worktree.missing.then(|| {
            div()
                .flex_none()
                .text_size(ui_text::text(controls::ROW_DETAIL_TEXT))
                .text_color(rgb(colors.gold))
                .child("Missing")
        }));
    let counts = div()
        .flex()
        .items_center()
        .gap(ui_text::space(3.0))
        .min_w_0()
        .child(
            line_box(
                format!("{} ·", worktree.counts),
                if worktree.working {
                    colors.working
                } else {
                    colors.muted
                },
                controls::ROW_DETAIL_TEXT,
            )
            .flex_shrink(1.0),
        )
        .child(
            line_box(
                worktree.id.to_owned(),
                colors.muted,
                controls::ROW_DETAIL_TEXT,
            )
            .flex_none()
            .font_family(ui_text::mono_family()),
        );
    vec![
        title.into_any_element(),
        native_details(vec![
            mono_line(worktree.path, colors.muted, controls::ROW_DETAIL_TEXT),
            counts.into_any_element(),
        ]),
    ]
}

/// A Native row's detail lines, lined up with the title's words after its symbol.
fn native_details(lines: Vec<AnyElement>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(ui_text::space(2.0))
        .min_w_0()
        .pl(native_detail_indent())
        .children(lines)
        .into_any_element()
}

/// What Native's shell row shows.
struct NativeShell<'a> {
    /// The worktree's branch the shell is in.
    title: String,
    status: NativeShellStatus,
    path: String,
    /// "CPU 0.0% · RAM 6M · zsh", or the command alone.
    detail: String,
    id: &'a str,
}

/// A shell's state as Native words it: a small symbol and a muted word, orange only while
/// an agent is working.
struct NativeShellStatus {
    symbol: &'static str,
    points: f32,
    text: String,
    working: bool,
}

fn native_shell_status(alive: bool, state: Option<&AgentState>) -> NativeShellStatus {
    let status = |symbol, points, text: &str, working| NativeShellStatus {
        symbol,
        points,
        text: text.to_owned(),
        working,
    };
    if !alive {
        return status("xmark", 7.0, "Exited", false);
    }
    let Some(state) = state else {
        return status("circle.fill", 6.0, "Live", false);
    };
    let mut status = match state.activity {
        AgentActivity::Working => status("circle.fill", 6.0, "Working", true),
        AgentActivity::Done => status("checkmark", 7.0, "Done", false),
        AgentActivity::Waiting => status("circle", 6.0, "Waiting", false),
        AgentActivity::Unknown | AgentActivity::Exited => {
            return status("circle.fill", 6.0, "Live", false);
        }
    };
    if let Some(subagents) = state.subagents.label() {
        status.text = format!("{} · {subagents}", status.text);
    }
    status
}

/// Native's shell row: the shell's branch, its state on the right, then its folder and
/// its load, with the folder and the short id in the monospace accent.
fn native_shell_lines(shell: NativeShell<'_>, colors: Palette) -> Vec<AnyElement> {
    let status_color = if shell.status.working {
        colors.working
    } else {
        colors.muted
    };
    let title = div()
        .flex()
        .items_center()
        .gap(ui_text::space(NATIVE_ROW_SYMBOL_GAP))
        .min_w_0()
        .child(icons::symbol(
            "terminal",
            NATIVE_ROW_SYMBOL,
            Some(colors.muted),
        ))
        .child(div().flex_1().min_w_0().child(line(
            shell.title,
            colors.text,
            controls::ROW_TITLE_TEXT,
        )))
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .text_size(ui_text::text(controls::ROW_DETAIL_TEXT))
                .text_color(rgb(status_color))
                .child(icons::symbol(
                    shell.status.symbol,
                    shell.status.points,
                    Some(status_color),
                ))
                .child(shell.status.text),
        );
    let detail = div()
        .flex()
        .items_center()
        .gap(ui_text::space(3.0))
        .min_w_0()
        .child(
            line_box(
                format!("{} ·", shell.detail),
                colors.muted,
                controls::ROW_DETAIL_TEXT,
            )
            .flex_shrink(1.0),
        )
        .child(
            line_box(shell.id.to_owned(), colors.muted, controls::ROW_DETAIL_TEXT)
                .flex_none()
                .font_family(ui_text::mono_family()),
        );
    vec![
        title.into_any_element(),
        native_details(vec![
            mono_line(shell.path, colors.muted, controls::ROW_DETAIL_TEXT),
            detail.into_any_element(),
        ]),
    ]
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

/// The right-hand label of a shell row: whether it lives, and for an agent what
/// it is doing, with the subagents it has running. "● LIVE" when the agent's
/// state is not known, as for a plain shell.
fn shell_status_label(alive: bool, state: Option<&AgentState>) -> String {
    if !alive {
        return "× EXITED".to_owned();
    }
    let Some(state) = state else {
        return "● LIVE".to_owned();
    };
    let mark = match state.activity {
        AgentActivity::Working => "● WORKING",
        AgentActivity::Done => "✓ DONE",
        AgentActivity::Waiting => "◌ WAITING",
        AgentActivity::Unknown | AgentActivity::Exited => return "● LIVE".to_owned(),
    };
    match state.subagents.label() {
        Some(subagents) => format!("{mark} · {}", subagents.to_uppercase()),
        None => mark.to_owned(),
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

    #[test]
    fn panels_without_a_remote_api_say_so_and_the_lists_are_the_hosts() {
        use PanelKind::*;
        for kind in [Worktrees, Tasks, Shells] {
            assert_eq!(remote_support(kind), RemoteSupport::Lists, "{kind:?}");
        }
        for kind in [Files, Preview, ProjectSettings, Schedules, Usage] {
            assert_eq!(remote_support(kind), RemoteSupport::Unavailable, "{kind:?}");
        }
        // The folders of projects and the settings are not about one project.
        for kind in [Projects, Settings] {
            assert_eq!(remote_support(kind), RemoteSupport::Global, "{kind:?}");
        }
    }

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
            codex_account: crate::store::ProjectCodexAccount::default(),
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

    #[test]
    fn worktree_presence_is_cached_and_never_probed_twice_at_once() {
        let (a, b) = (Path::new("/w/a"), Path::new("/w/b"));
        let start = Instant::now();
        let mut presence = WorktreePresence::default();
        // Unchecked folders count as present, and render only asks for checks.
        assert!(!presence.missing(a));
        let mut due = presence.due([a, b, a].into_iter(), start);
        due.sort();
        assert_eq!(due, [a.to_owned(), b.to_owned()]);
        // Nothing is re-probed while a check (possibly stuck on a dead volume)
        // is still running.
        let later = start + Duration::from_secs(60);
        assert!(presence.due([a, b].into_iter(), later).is_empty());
        assert!(presence.finish(a.to_owned(), true, later));
        assert!(presence.missing(a));
        assert!(!presence.missing(b));
        // Fresh results are reused; stale ones are checked again, per folder.
        assert!(
            presence
                .due([a].into_iter(), later + Duration::from_secs(1))
                .is_empty()
        );
        let stale = later + PRESENCE_INTERVAL;
        assert_eq!(presence.due([a, b].into_iter(), stale), [a.to_owned()]);
        // Confirming the same state does not ask for a repaint; a change does.
        assert!(!presence.finish(a.to_owned(), true, stale));
        assert_eq!(
            presence.due([a].into_iter(), stale + PRESENCE_INTERVAL),
            [a.to_owned()]
        );
        assert!(presence.finish(a.to_owned(), false, stale + PRESENCE_INTERVAL));
        assert!(!presence.missing(a));
        // A folder found present on its first check needs no repaint.
        assert!(!presence.finish(b.to_owned(), false, later));
    }

    #[test]
    fn a_worktree_folder_is_missing_unless_it_is_a_directory() {
        let directory = std::env::temp_dir().join(format!("riwork-panels-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let file = directory.join("file");
        std::fs::write(&file, "").unwrap();
        assert!(!folder_missing(&directory));
        assert!(folder_missing(&file));
        assert!(folder_missing(&directory.join("gone")));
        std::fs::remove_dir_all(&directory).unwrap();
    }
}

#[cfg(test)]
mod kit_control_tests {
    use super::*;
    use crate::form_input::{test_turn, test_window};
    use gpui::InputEvent as _;
    use gpui::TestAppContext;
    use gpui_kit::test::TestWindowExt;

    struct Fixture {
        sort: ProjectSortUi,
        order: ProjectOrder,
        actions: Vec<PanelAction>,
        row_focus: FocusHandle,
        native_reveal: bool,
    }
    impl Fixture {
        fn action(&mut self, action: PanelAction, window: &mut Window, cx: &mut Context<Self>) {
            match &action {
                PanelAction::CloseProjectSortMenu => self.sort.dismiss(window, cx),
                PanelAction::SetProjectOrder(order) => {
                    self.order = *order;
                    self.sort.dismiss(window, cx);
                }
                _ => {}
            }
            self.actions.push(action);
            cx.notify();
        }
    }
    impl Render for Fixture {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .flex()
                .flex_col()
                .child(project_sort_controls(
                    self.order,
                    &self.sort,
                    Rc::new(Cell::new(Bounds::default())),
                    Self::action,
                    cx,
                ))
                .child(project_row(
                    "synthetic-project-id",
                    "Same title",
                    None,
                    ProjectRowLook {
                        selected: !self.native_reveal,
                        depth: 0,
                        dimmed: false,
                        focus: Some(self.row_focus.clone()),
                    },
                    vec![
                        div()
                            .flex()
                            .items_center()
                            .w_full()
                            .child(div().flex_1().child("Same title"))
                            .child(
                                project_action_strip(
                                    self.native_reveal,
                                    !self.native_reveal,
                                    &self.row_focus,
                                    window,
                                    cx,
                                )
                                .child(project_control(
                                    "synthetic-project-id",
                                    "settings",
                                    "⚙",
                                    "Project settings",
                                    PanelAction::ProjectSettings("synthetic-project-id".into()),
                                    Self::action,
                                    cx,
                                )),
                            )
                            .into_any_element(),
                    ],
                    PanelAction::Project("synthetic-project-id".into()),
                    Self::action,
                    cx,
                ))
        }
    }
    fn mount(cx: &mut TestAppContext) -> (gpui::AnyWindowHandle, Entity<Fixture>) {
        test_window(cx, |_, cx| Fixture {
            sort: ProjectSortUi::new(cx),
            order: ProjectOrder::default(),
            actions: Vec::new(),
            row_focus: cx.focus_handle(),
            native_reveal: false,
        })
    }

    #[gpui::test]
    fn panel_rows_keyboard_ax_and_nested_actions_keep_exact_project_identity(
        cx: &mut TestAppContext,
    ) {
        let (window, owner) = mount(cx);
        test_turn(cx, window, |window, app| {
            window.click("settings-project-synthetic-project-id", app)
        });
        test_turn(cx, window, |window, app| {
            assert!(
                matches!(owner.read(app).actions.as_slice(), [PanelAction::ProjectSettings(id)] if id == "synthetic-project-id")
            );
            assert_eq!(
                window
                    .find("settings-project-synthetic-project-id")
                    .focused(),
                Some(true)
            );
            assert!(owner.read(app).row_focus.contains_focused(window, app));
            assert!(!owner.read(app).row_focus.is_focused(window));
            // Rebuild the real parent and child controls before keyboard
            // activation; their semantic IDs must retain the nested focus.
            owner.update(app, |_, cx| cx.notify());
        });
        test_turn(cx, window, |window, _| {
            assert_eq!(
                window
                    .find("settings-project-synthetic-project-id")
                    .focused(),
                Some(true)
            );
        });
        test_turn(cx, window, |window, app| window.press("space", app));
        test_turn(cx, window, |window, app| {
            let actions = &owner.read(app).actions;
            assert_eq!(actions.len(), 2);
            assert!(actions.iter().all(
                |a| matches!(a, PanelAction::ProjectSettings(id) if id == "synthetic-project-id")
            ));
            let row = crate::form_input::test_ax_node(window, app, "project-synthetic-project-id");
            assert_eq!(row.role(), gpui::Role::Button);
            assert_eq!(row.label(), Some("Same title"));
            assert_eq!(row.is_selected(), Some(true));
        });
        test_turn(cx, window, |window, app| {
            window.click("project-synthetic-project-id", app);
        });
        test_turn(cx, window, |window, app| {
            let owner = owner.read(app);
            assert_eq!(owner.actions.len(), 3);
            assert!(
                matches!(&owner.actions[2], PanelAction::Project(id) if id == "synthetic-project-id")
            );
            assert!(owner.row_focus.is_focused(window));
        });
        test_turn(cx, window, |window, app| window.press("enter", app));
        test_turn(cx, window, |_, app| {
            let actions = &owner.read(app).actions;
            assert_eq!(actions.len(), 4);
            assert!(
                actions[2..]
                    .iter()
                    .all(|a| matches!(a, PanelAction::Project(id) if id == "synthetic-project-id"))
            );
            assert!(
                !actions
                    .iter()
                    .any(|a| matches!(a, PanelAction::OpenProject(_)))
            );
        });
    }

    #[gpui::test]
    fn native_project_actions_reveal_for_keyboard_scope_and_keep_pointer_hover(
        cx: &mut TestAppContext,
    ) {
        let (window, owner) = mount(cx);
        // Exercise Native's actual visibility policy without loading a native
        // theme, Ghostty settings, StateStore, Workspace or any service.
        test_turn(cx, window, |window, app| {
            owner.update(app, |owner, cx| {
                owner.native_reveal = true;
                cx.notify();
            });
            window.dispatch_event(
                gpui::MouseMoveEvent {
                    position: gpui::point(
                        window.viewport_size().width - px(2.),
                        window.viewport_size().height - px(2.),
                    ),
                    pressed_button: None,
                    modifiers: Default::default(),
                }
                .to_platform_input(),
                app,
            );
        });
        test_turn(cx, window, |window, app| {
            assert!(
                !window
                    .find("settings-project-synthetic-project-id")
                    .visible()
            );
            let position = window
                .find("project-synthetic-project-id")
                .bounds()
                .center();
            window.dispatch_event(
                gpui::MouseMoveEvent {
                    position,
                    pressed_button: None,
                    modifiers: Default::default(),
                }
                .to_platform_input(),
                app,
            );
        });
        test_turn(cx, window, |window, app| {
            assert!(
                window
                    .find("settings-project-synthetic-project-id")
                    .visible()
            );
            assert!(owner.read(app).actions.is_empty());
            window.dispatch_event(
                gpui::MouseMoveEvent {
                    position: gpui::point(
                        window.viewport_size().width - px(2.),
                        window.viewport_size().height - px(2.),
                    ),
                    pressed_button: None,
                    modifiers: Default::default(),
                }
                .to_platform_input(),
                app,
            );
        });
        test_turn(cx, window, |window, app| {
            assert!(
                !window
                    .find("settings-project-synthetic-project-id")
                    .visible()
            );
            let focus = owner.read(app).row_focus.clone();
            focus.focus(window, app);
            // Programmatic focus preserves pointer modality. A non-activating
            // key establishes keyboard focus and lets the row reveal its child
            // before Root's next Tab enumerates the actually painted tab stops.
            window.press("right", app);
        });
        test_turn(cx, window, |window, app| {
            assert!(owner.read(app).row_focus.is_focused(window));
            assert!(
                window
                    .find("settings-project-synthetic-project-id")
                    .visible()
            );
            assert!(owner.read(app).actions.is_empty());
            window.press("tab", app);
        });
        test_turn(cx, window, |window, app| {
            let nested = window.find("settings-project-synthetic-project-id");
            assert_eq!(nested.focused(), Some(true));
            assert!(
                nested.visible(),
                "keyboard focus within the row reveals Native actions"
            );
            let node = crate::form_input::test_ax_node(
                window,
                app,
                "settings-project-synthetic-project-id",
            );
            assert_eq!(node.role(), gpui::Role::Button);
            assert_eq!(node.label(), Some("Project settings"));
            assert!(!node.is_disabled());
            assert!(node.supports_action(gpui::accesskit::Action::Click));
            window.press("space", app);
        });
        test_turn(cx, window, |window, app| {
            assert!(
                matches!(owner.read(app).actions.as_slice(), [PanelAction::ProjectSettings(id)] if id == "synthetic-project-id")
            );
            let focus = owner.read(app).sort.trigger_focus.clone();
            focus.focus(window, app);
        });
        test_turn(cx, window, |window, app| {
            assert!(
                !window
                    .find("settings-project-synthetic-project-id")
                    .visible()
            );
            assert!(owner.read(app).sort.trigger_focus.is_focused(window));
        });
    }

    #[gpui::test]
    fn sort_popover_keyboard_reselect_dismissal_and_refresh_preserve_focus(
        cx: &mut TestAppContext,
    ) {
        let (window, owner) = mount(cx);
        let identity = owner.read_with(cx, |owner, _| owner.sort.state.entity_id());
        test_turn(cx, window, |window, app| {
            window.click("project-sort-selector", app)
        });
        test_turn(cx, window, |window, app| {
            let owner = owner.read(app);
            assert!(owner.sort.state.read(app).is_open());
            assert!(
                owner
                    .sort
                    .state
                    .read(app)
                    .focus_handle(app)
                    .contains_focused(window, app)
            );
            assert!(matches!(
                owner.actions.as_slice(),
                [PanelAction::SetProjectSortMenuOpen(true)]
            ));
            window.press("tab", app);
        });
        test_turn(cx, window, |window, app| window.press("enter", app));
        test_turn(cx, window, |window, app| {
            let owner = owner.read(app);
            assert!(!owner.sort.state.read(app).is_open());
            assert_eq!(
                owner
                    .actions
                    .iter()
                    .filter(|a| matches!(a, PanelAction::SetProjectOrder(_)))
                    .count(),
                1
            );
            assert!(owner.sort.trigger_focus.is_focused(window));
            window.press("space", app);
        });
        test_turn(cx, window, |window, app| window.press("escape", app));
        test_turn(cx, window, |window, app| {
            assert!(!owner.read(app).sort.state.read(app).is_open());
            assert!(owner.read(app).sort.trigger_focus.is_focused(window));
            owner.update(app, |_, cx| cx.notify());
        });
        test_turn(cx, window, |window, app| {
            assert_eq!(owner.read(app).sort.state.entity_id(), identity);
            assert!(owner.read(app).sort.trigger_focus.is_focused(window));
            assert_eq!(
                owner
                    .read(app)
                    .actions
                    .iter()
                    .filter(|a| matches!(a, PanelAction::CloseProjectSortMenu))
                    .count(),
                1
            );
        });
    }
}

#[cfg(test)]
mod search_regression_tests {
    use super::*;
    use crate::{
        form_input::test_turn,
        text_input::{self, InputEvent, InputState},
    };
    use gpui::{Subscription, TestAppContext};
    use gpui_kit::test::TestWindowExt;

    // Real Projects panels and retained Base states; no Workspace, Store,
    // SessionManager, filesystem fixture, terminal or backend is constructed.
    struct Fixture {
        state: State,
        kind: PanelKind,
        native_chats: Vec<ChatInfo>,
        shells: Vec<ShellSession>,
        session_filter: Filter,
        inputs: Vec<(u64, Entity<InputState>)>,
        queries: BTreeMap<u64, String>,
        sorts: Vec<ProjectSortUi>,
        changes: Vec<(u64, String)>,
        focused: Option<u64>,
        actions: Vec<PanelAction>,
        width: f32,
        two_panels: bool,
        _subscriptions: Vec<Subscription>,
    }
    impl Fixture {
        fn action(&mut self, action: PanelAction, window: &mut Window, cx: &mut Context<Self>) {
            if let PanelAction::SessionFilter(filter) = &action {
                self.session_filter = *filter;
            }
            if matches!(
                action,
                PanelAction::ClearSearch | PanelAction::SessionFilter(_)
            ) {
                for (id, input) in &self.inputs {
                    crate::form_input::set_value(input, String::new(), window, cx);
                    self.queries.insert(*id, String::new());
                }
            }
            self.actions.push(action);
            cx.notify();
        }
    }
    impl Render for Fixture {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().size_full().flex().items_start().gap(px(8.)).children(
                self.inputs
                    .iter()
                    .take(if self.two_panels { 2 } else { 1 })
                    .enumerate()
                    .map(|(index, (id, input))| {
                        div()
                            .id(("search-fixture-panel", *id))
                            .test_support()
                            .flex_none()
                            .w(px(self.width))
                            .h(px(if self.kind == PanelKind::Shells {
                                600.
                            } else {
                                360.
                            }))
                            .child(render_panel(
                                self.kind,
                                PanelData {
                                    state: &self.state,
                                    project_id: "alpha-id",
                                    selected_worktree_id: None,
                                    selected_task_id: None,
                                    shells: &self.shells,
                                    shell_cwds: &BTreeMap::new(),
                                    metrics: &BTreeMap::new(),
                                    activity: &BTreeMap::new(),
                                    chats: &[],
                                    native_chats: &self.native_chats,
                                    session_catalog_note: None,
                                    session_catalog_loading: false,
                                    session_catalog_generation: 7,
                                    session_filter: self.session_filter,
                                    query: &self.queries[id],
                                    search_focused: input
                                        .read(cx)
                                        .focus_handle(cx)
                                        .is_focused(window),
                                    search_input: Some(input),
                                    control_inset: 0.,
                                    collapsed_folders: &HashSet::new(),
                                    state_home: Path::new("/synthetic-only/no-state"),
                                    project_order: ProjectOrder::default(),
                                    project_last_edits: &BTreeMap::new(),
                                    project_sort_menu_open: false,
                                    project_sort_ui: Some(&self.sorts[index]),
                                    remote_folders: &[],
                                    selected_remote: None,
                                },
                                Self::action,
                                window,
                                cx,
                            ))
                    }),
            )
        }
    }
    fn mount(cx: &mut TestAppContext, native: bool) -> (gpui::AnyWindowHandle, Entity<Fixture>) {
        let (handle, owner): (gpui::AnyWindowHandle, Entity<Fixture>) = cx.update(|app| {
            let mut settings = crate::settings::Settings::default();
            settings.ui_text_matches_terminal = false;
            settings.theme = if native {
                theme::ThemeChoice::Native
            } else {
                theme::ThemeChoice::RiWork
            };
            let choice = settings.theme;
            app.set_global(settings);
            app.set_global(theme::Appearance {
                selected: choice,
                palette: if native {
                    Palette::NATIVE_LIGHT
                } else {
                    Palette::RIWORK
                },
                terminal: None,
                ghostty: None,
                error: None,
            });
            text_input::init(app);
            crate::behavior_controls::init(app);
            // Actual native/colorful face and scaling, with terminal matching
            // explicitly off: init returns before all Ghostty config reads.
            ui_text::init(app);
            let created = Rc::new(std::cell::RefCell::new(None));
            let retained = created.clone();
            let handle = app
                .open_window(
                    gpui::WindowOptions {
                        window_bounds: Some(gpui::WindowBounds::Windowed(Bounds::new(
                            gpui::point(px(0.), px(0.)),
                            gpui::size(px(1000.), px(600.)),
                        ))),
                        ..Default::default()
                    },
                    move |window, app| {
                        let owner = app.new(|cx| {
                            let inputs: Vec<_> = [101, 202]
                                .into_iter()
                                .map(|id| {
                                    (id, text_input::single_line("", "Search  ⌘F", window, cx))
                                })
                                .collect();
                            let subscriptions = inputs
                                .iter()
                                .map(|(id, input)| {
                                    let id = *id;
                                    cx.subscribe_in(
                                        input,
                                        window,
                                        move |owner: &mut Fixture, input, event, _, cx| {
                                            if !owner.inputs.iter().any(|(live_id, live)| {
                                                *live_id == id
                                                    && live.entity_id() == input.entity_id()
                                            }) {
                                                return;
                                            }
                                            match event {
                                                InputEvent::Change => {
                                                    let value = input.read(cx).value().to_string();
                                                    owner.queries.insert(id, value.clone());
                                                    owner.changes.push((id, value));
                                                }
                                                InputEvent::Focus => owner.focused = Some(id),
                                                _ => {}
                                            }
                                            cx.notify();
                                        },
                                    )
                                })
                                .collect();
                            let projects = [("alpha-id", "Alpha"), ("beta-id", "Beta")]
                                .into_iter()
                                .map(|(id, name)| crate::store::Project {
                                    id: id.into(),
                                    name: name.into(),
                                    root: PathBuf::from("/synthetic-only/no-project"),
                                    repository_roots: Vec::new(),
                                    folder_id: None,
                                    notify_on_agent_done: false,
                                    codex_account: Default::default(),
                                    created_at: 0,
                                })
                                .collect();
                            Fixture {
                                kind: PanelKind::Projects,
                                native_chats: Vec::new(),
                                shells: Vec::new(),
                                session_filter: Filter::All,
                                state: State {
                                    projects,
                                    ..Default::default()
                                },
                                inputs,
                                queries: BTreeMap::from([
                                    (101, String::new()),
                                    (202, String::new()),
                                ]),
                                sorts: vec![ProjectSortUi::new(cx), ProjectSortUi::new(cx)],
                                changes: Vec::new(),
                                focused: None,
                                actions: Vec::new(),
                                width: 160.,
                                two_panels: false,
                                _subscriptions: subscriptions,
                            }
                        });
                        *retained.borrow_mut() = Some(owner.clone());
                        app.new(|cx| gpui_kit::base::Root::new(owner, window, cx))
                    },
                )
                .unwrap();
            let owner = created.borrow_mut().take().unwrap();
            (handle.into(), owner)
        });
        test_turn(cx, handle, |window, _| window.activate_window());
        (handle, owner)
    }
    fn history(id: u128, provider: &str, state: serde_json::Value, project: &str) -> ChatInfo {
        serde_json::from_value(serde_json::json!({
            "id": uuid::Uuid::from_u128(id).to_string(), "provider": provider,
            "project_id": project, "worktree_id": "saved-tree", "cwd": "/synthetic-only/history",
            "title": "Same title", "created_at_unix": id as u64, "state": state,
        }))
        .unwrap()
    }

    // A mounted Root and an in-memory action recorder only. There is no chat feed,
    // Workspace, StateStore, SessionManager, filesystem or provider in this fixture.
    fn sessions_controls(cx: &mut TestAppContext, native: bool) {
        let (handle, owner) = mount(cx, native);
        let claude = uuid::Uuid::from_u128(1).to_string();
        let codex = uuid::Uuid::from_u128(2).to_string();
        test_turn(cx, handle, |_, app| {
            owner.update(app, |owner, cx| {
                owner.kind = PanelKind::Shells;
                owner.width = 480.;
                owner.native_chats = vec![
                    history(
                        1,
                        "claude",
                        serde_json::json!({"state":"stopped"}),
                        "alpha-id",
                    ),
                    history(
                        2,
                        "codex",
                        serde_json::json!({"state":"failed", "message":"offline"}),
                        "alpha-id",
                    ),
                    history(
                        3,
                        "claude",
                        serde_json::json!({"state":"idle"}),
                        "other-project",
                    ),
                ];
                owner.shells = vec![
                    serde_json::from_value(serde_json::json!({
                        "id": uuid::Uuid::from_u128(4).to_string(), "project_id":"alpha-id",
                        "kind":"project", "cwd":"/synthetic-only/shell", "created_at_unix":4,
                    }))
                    .unwrap(),
                ];
                cx.notify();
            })
        });
        test_turn(cx, handle, |window, app| {
            let node =
                crate::form_input::test_ax_node(window, app, format!("session-chat-{claude}"));
            assert_eq!(node.role(), gpui::Role::Button);
            assert!(
                node.label()
                    .unwrap()
                    .contains("Claude chat · Same title · Stopped")
            );
            let node =
                crate::form_input::test_ax_node(window, app, format!("session-chat-{codex}"));
            assert!(
                node.label()
                    .unwrap()
                    .contains("Codex chat · Same title · Failed")
            );
            assert!(
                window
                    .try_find(format!("session-chat-{}", uuid::Uuid::from_u128(3)))
                    .is_none()
            );
            // Provider filters are real Base buttons, usable by mouse and keyboard.
            window.click("sessions-filter-Claude chats", app);
        });
        test_turn(cx, handle, |window, app| window.press("space", app));
        test_turn(cx, handle, |window, app| {
            assert_eq!(owner.read(app).session_filter, Filter::Claude);
            assert!(window.try_find(format!("session-chat-{codex}")).is_none());
            assert!(
                window
                    .try_find(format!("shell-{}", uuid::Uuid::from_u128(4)))
                    .is_none()
            );
            assert!(
                owner
                    .read(app)
                    .actions
                    .iter()
                    .all(|a| matches!(a, PanelAction::SessionFilter(Filter::Claude)))
            );
            window.click(format!("session-chat-{claude}"), app);
        });
        test_turn(cx, handle, |window, app| window.press("enter", app));
        test_turn(cx, handle, |_, app| {
            let actions = &owner.read(app).actions;
            assert_eq!(actions.len(), 4);
            assert!(actions[2..].iter().all(|action| matches!(action,
                PanelAction::Chat { project, generation: 7, id } if project == "alpha-id" && id == &claude)));
        });
        test_turn(cx, handle, |window, app| {
            window.click("sessions-filter-Shells", app);
        });
        test_turn(cx, handle, |window, app| {
            window.click(format!("shell-{}", uuid::Uuid::from_u128(4)), app);
        });
        test_turn(cx, handle, |_, app| {
            assert!(
                matches!(owner.read(app).actions.last(), Some(PanelAction::Shell { project, generation: 7, id }) if project == "alpha-id" && id == &uuid::Uuid::from_u128(4).to_string())
            );
        });
        test_turn(cx, handle, |window, app| {
            window.click("sessions-filter-All sessions", app)
        });
        test_turn(cx, handle, |window, app| {
            window.click("shells-search-icon", app)
        });
        for character in "failed".chars() {
            test_turn(cx, handle, |window, app| {
                window.input(&character.to_string(), app)
            });
        }
        test_turn(cx, handle, |window, app| {
            assert_eq!(owner.read(app).queries[&101], "failed");
            assert!(window.try_find(format!("session-chat-{claude}")).is_none());
            assert!(
                window
                    .try_find(format!("shell-{}", uuid::Uuid::from_u128(4)))
                    .is_none()
            );
            window.click(format!("session-chat-{codex}"), app);
        });
        test_turn(cx, handle, |_, app| {
            assert!(
                matches!(owner.read(app).actions.last(), Some(PanelAction::Chat { id, .. }) if id == &codex)
            );
        });
    }

    #[gpui::test]
    fn sessions_native_controls_search_and_exact_uuid_selection(cx: &mut TestAppContext) {
        sessions_controls(cx, true);
    }

    #[gpui::test]
    fn sessions_colorful_controls_search_and_exact_uuid_selection(cx: &mut TestAppContext) {
        sessions_controls(cx, false);
    }

    fn geometry(cx: &mut TestAppContext, native: bool) {
        let (handle, owner) = mount(cx, native);
        let entity = owner.read_with(cx, |owner, _| owner.inputs[0].1.entity_id());
        for width in [160., 240., 480.] {
            test_turn(cx, handle, |_, app| {
                owner.update(app, |owner, cx| {
                    owner.width = width;
                    cx.notify();
                })
            });
            test_turn(cx, handle, |window, app| {
                assert_eq!(ui_text::is_native(), native);
                let mut panel = window.within(("search-fixture-panel", 101u64));
                let search = panel.find("projects-search");
                let icon = panel.find("projects-search-icon");
                let editor = panel.find("projects-search-input");
                assert!(search.visible() && icon.visible() && editor.visible());
                assert_eq!(editor.role(), Some(gpui::Role::TextInput));
                assert_eq!(editor.label(), Some("Search projects"));
                assert!(
                    editor.bounds().size.width >= px(80.),
                    "readable editor at pane width {width}: {:?}",
                    editor.bounds()
                );
                assert!(editor.bounds().size.height >= ui_text::space(18.));
                assert!(editor.bounds().left() >= icon.bounds().right());
                assert!(editor.bounds().right() <= search.bounds().right());
                assert!(editor.bounds().top() >= search.bounds().top());
                assert!(editor.bounds().bottom() <= search.bounds().bottom());
                let input = owner.read(app).inputs[0].1.clone();
                assert_eq!(input.entity_id(), entity);
                assert_eq!(
                    input.read(app).presentation().placeholder().as_ref(),
                    "Search  ⌘F"
                );
                let caret = input
                    .read(app)
                    .range_to_bounds(&(0..0))
                    .expect("the empty placeholder has real editor layout");
                assert!(caret.size.height > px(0.));
                assert!(
                    caret.left() >= editor.bounds().left()
                        && caret.right() <= editor.bounds().right()
                );
                // An icon press must focus this real field, including the
                // padding/background path outside Base's glyph hit target.
                panel.click("projects-search-icon", app);
            });
            test_turn(cx, handle, |window, app| {
                assert!(
                    owner.read(app).inputs[0]
                        .1
                        .read(app)
                        .focus_handle(app)
                        .is_focused(window)
                );
                assert_eq!(owner.read(app).focused, Some(101));
                assert!(owner.read(app).actions.is_empty());
            });
        }
    }
    #[gpui::test]
    fn projects_search_has_readable_bounds_and_placeholder_in_narrow_colorful_panels(
        cx: &mut TestAppContext,
    ) {
        geometry(cx, false);
    }
    #[gpui::test]
    fn projects_search_has_readable_bounds_and_placeholder_in_narrow_native_panels(
        cx: &mut TestAppContext,
    ) {
        geometry(cx, true);
    }
    fn editing_and_binding(cx: &mut TestAppContext, native: bool) {
        let (handle, owner) = mount(cx, native);
        test_turn(cx, handle, |_, app| {
            owner.update(app, |owner, cx| {
                owner.two_panels = true;
                owner.width = 240.;
                cx.notify();
            })
        });
        test_turn(cx, handle, |window, app| {
            window
                .within(("search-fixture-panel", 101u64))
                .click("projects-search-icon", app)
        });
        // Input dispatches per-character keystrokes; drain each Change effect
        // before the next character so its callback reads that edit's value.
        for character in "Alpha".chars() {
            let character = character.to_string();
            test_turn(cx, handle, |window, app| window.input(&character, app));
        }
        test_turn(cx, handle, |window, app| {
            assert_eq!(owner.read(app).queries[&101], "Alpha");
            assert_eq!(owner.read(app).queries[&202], "");
            let panel = window.within(("search-fixture-panel", 101u64));
            assert!(panel.try_find("project-alpha-id").is_some());
            assert!(panel.try_find("project-beta-id").is_none());
            window.press("cmd-a", app);
        });
        test_turn(cx, handle, |window, app| window.press("cmd-c", app));
        test_turn(cx, handle, |window, app| {
            assert_eq!(
                app.read_from_clipboard()
                    .and_then(|item| item.text())
                    .as_deref(),
                Some("Alpha")
            );
            let input = owner.read(app).inputs[0].1.clone();
            assert_eq!(input.read(app).selected_range(), 0..5);
            let caret = input.read(app).range_to_bounds(&(2..2)).unwrap().center();
            let bounds = window
                .within(("search-fixture-panel", 101u64))
                .find("projects-search-input")
                .bounds();
            window.within(("search-fixture-panel", 101u64)).click_at(
                "projects-search-input",
                caret - bounds.origin,
                app,
            );
        });
        test_turn(cx, handle, |window, app| {
            assert_eq!(owner.read(app).inputs[0].1.read(app).cursor(), 2);
            window
                .within(("search-fixture-panel", 202u64))
                .click("projects-search-icon", app);
        });
        for character in "Beta".chars() {
            let character = character.to_string();
            test_turn(cx, handle, |window, app| window.input(&character, app));
        }
        test_turn(cx, handle, |window, app| {
            let fixture = owner.read(app);
            assert_eq!(fixture.focused, Some(202));
            assert_eq!(
                fixture.changes,
                [
                    (101u64, String::from("A")),
                    (101u64, String::from("Al")),
                    (101u64, String::from("Alp")),
                    (101u64, String::from("Alph")),
                    (101u64, String::from("Alpha")),
                    (202u64, String::from("B")),
                    (202u64, String::from("Be")),
                    (202u64, String::from("Bet")),
                    (202u64, String::from("Beta"))
                ]
            );
            assert_eq!(fixture.inputs[0].1.read(app).value(), "Alpha");
            assert_eq!(fixture.inputs[0].1.read(app).cursor(), 2);
            assert_eq!(fixture.inputs[1].1.read(app).value(), "Beta");
            assert!(
                fixture.inputs[1]
                    .1
                    .read(app)
                    .focus_handle(app)
                    .is_focused(window)
            );
            assert!(
                fixture.actions.is_empty(),
                "search clicks must not dispatch through the active-pane fallback"
            );
            let panel = window.within(("search-fixture-panel", 202u64));
            assert!(panel.try_find("project-beta-id").is_some());
            assert!(panel.try_find("project-alpha-id").is_none());
        });
    }
    #[gpui::test]
    fn projects_search_pointer_editing_copy_cursor_and_sibling_tab_binding_colorful(
        cx: &mut TestAppContext,
    ) {
        editing_and_binding(cx, false);
    }
    #[gpui::test]
    fn projects_search_pointer_editing_copy_cursor_and_sibling_tab_binding_native(
        cx: &mut TestAppContext,
    ) {
        editing_and_binding(cx, true);
    }
    fn clearing(cx: &mut TestAppContext, native: bool) {
        let (handle, owner) = mount(cx, native);
        test_turn(cx, handle, |_, app| {
            owner.update(app, |owner, cx| {
                owner.width = 240.;
                cx.notify();
            })
        });
        test_turn(cx, handle, |window, app| {
            let mut panel = window.within(("search-fixture-panel", 101u64));
            assert!(panel.try_find("projects-search-clear").is_none());
            panel.click("projects-search-icon", app)
        });
        for character in "Al".chars() {
            let character = character.to_string();
            test_turn(cx, handle, |window, app| window.input(&character, app));
        }
        test_turn(cx, handle, |window, app| {
            let mut panel = window.within(("search-fixture-panel", 101u64));
            let search = panel.find("projects-search");
            let editor = panel.find("projects-search-input");
            let clear = panel.find("projects-search-clear");
            // Inside the one field, after the text.
            assert!(clear.visible());
            assert_eq!(clear.label(), Some("Clear the search"));
            assert!(clear.bounds().left() >= editor.bounds().right());
            assert!(clear.bounds().right() <= search.bounds().right());
            panel.click("projects-search-clear", app);
        });
        test_turn(cx, handle, |window, app| {
            let fixture = owner.read(app);
            assert!(matches!(
                fixture.actions.as_slice(),
                [PanelAction::ClearSearch]
            ));
            for (id, input) in &fixture.inputs {
                assert_eq!(input.read(app).value(), "");
                assert_eq!(input.read(app).cursor(), 0);
                assert_eq!(fixture.queries[id], "");
            }
            let panel = window.within(("search-fixture-panel", 101u64));
            assert!(panel.try_find("projects-search-clear").is_none());
            assert!(panel.try_find("project-alpha-id").is_some());
            assert!(panel.try_find("project-beta-id").is_some());
            // The pressed field keeps its own editor focused, not the active pane's.
            assert!(
                fixture.inputs[0]
                    .1
                    .read(app)
                    .focus_handle(app)
                    .is_focused(window)
            );
        });
    }
    #[gpui::test]
    fn projects_search_clear_button_sits_in_the_field_and_keeps_its_editor_focused_colorful(
        cx: &mut TestAppContext,
    ) {
        clearing(cx, false);
    }
    #[gpui::test]
    fn projects_search_clear_button_sits_in_the_field_and_keeps_its_editor_focused_native(
        cx: &mut TestAppContext,
    ) {
        clearing(cx, true);
    }
}
