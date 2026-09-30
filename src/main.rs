mod activity;
mod agent_hooks;
mod cli;
mod codex_accounts;
mod cua;
mod dock_menu;
mod file_explorer;
mod file_preview;
mod icons;
mod layouts;
mod mcp;
mod notifications;
mod orca_import;
mod panels;
mod paths;
mod project_creator;
mod project_recency;
mod project_settings;
mod project_sort;
mod remote_cli;
mod runtime;
mod schedule_panel;
mod schedule_service;
mod schedules;
mod session_input;
mod session_keys;
mod session_reload;
mod session_viewport;
mod sessions;
mod settings;
mod status_bar;
mod store;
mod terminal_lifecycle;
mod theme;
mod tooltip;
mod update;
mod usage;

use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet, HashSet},
    env,
    ops::Range,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use activity::{ActivityTracker, AgentActivity};
use file_explorer::{ExplorerRoot, FileExplorer, FileExplorerEvent};
use gpui::{
    AnyElement, App, Bounds, ClipboardItem, Context, DragMoveEvent, Entity, EntityInputHandler,
    FocusHandle, Global, IntoElement, KeyBinding, KeyDownEvent, Menu, MenuItem, MouseButton,
    Pixels, Point, Render, StatefulInteractiveElement, TitlebarOptions, UTF16Selection, Window,
    WindowBounds, WindowHandle, WindowOptions, actions, canvas, div, img, point, prelude::*, px,
    rgb, size,
};
use gpui_libghostty::{TerminalConfiguration, TerminalOptions, TerminalTheme};
use gpui_platform::application;
use icons::Icon;
use layouts::{
    Axis, DIVIDER_THICKNESS, Extent, Layout, LayoutStore, MIN_PANE_EXTENT, PaneId, PanelKind,
    ProjectLayout, SavedPane, SavedTab, TabEdge, WindowSize,
};
use panels::{PanelAction, PanelData};
use project_creator::{ProjectCreationEvent, ProjectCreator};
use project_settings::{
    FolderEditor, FolderEditorEvent, ProjectSettingsEvent, ProjectSettingsPanel,
};
use sessions::{HarnessKind, SessionManager, SessionMetrics, ShellKind, ShellSession};
use settings::{CuaSetupState, Settings, SettingsEvent, SettingsPanel, SettingsStore};
use store::{Project, ProjectCodexAccount, SearchHit, State, Store};
use theme::{Appearance, Palette, ThemeChoice};
use tooltip::Look;
use usage::ProviderUsage;

gpui_libghostty::bind_gpui!(gpui);

actions!(
    riwork,
    [
        NewTab,
        SplitRight,
        SplitDown,
        CloseTab,
        ClosePane,
        NextTab,
        PreviousTab,
        ToggleSidebar,
        FocusSearch,
        OpenOrchestrator,
        OpenProjectOrchestrator,
        NewProjectWindow,
        OpenCodex,
        OpenClaude,
        OpenGrok,
        CreateProject,
        ToggleFocusMode,
        OpenSettings,
        OpenProjectSettings,
        OpenSchedules,
        OpenFiles,
        Quit
    ]
);

type TabId = u64;

#[derive(Clone, Copy)]
enum PaneMenuAction {
    Shell,
    Harness(HarnessKind, bool),
    Orchestrator(bool),
    View(PanelKind),
    Split(Axis),
    Close,
    Lock,
    Focus,
}

const WINDOW_CONTROLS_WIDTH: f32 = 78.0;
const WINDOW_CONTROLS_HEIGHT: f32 = 28.0;
const WINDOW_CONTROLS_CONTENT_INSET: f32 = WINDOW_CONTROLS_WIDTH + 14.0;
const FOCUS_MAX_WIDTH: f32 = 1100.0;
const FOCUS_BOTTOM_MARGIN: f32 = 0.30;
const FOCUS_TOOLBAR_HEIGHT: f32 = 32.0;
const STATUS_BAR_HEIGHT: f32 = 22.0;
/// A window restored fullscreen or zoomed is first drawn at its opening size and animates
/// to the real one. Sizes seen this soon after the first draw are that settling, not the
/// user resizing, so they must not rebalance the saved ratios.
const WINDOW_SETTLE: Duration = Duration::from_secs(2);
/// Attach attempts a tab makes on its own before it waits to be selected.
const ATTACH_RETRIES: u8 = 3;
const ATTACH_RETRY_DELAY: Duration = Duration::from_secs(2);

struct Tab {
    id: TabId,
    title: String,
    content: TabContent,
    /// When the tab last went off screen; `None` while it is shown.
    hidden_since: Option<Instant>,
}

enum TabContent {
    Shell {
        shell_id: String,
        worktree_id: Option<String>,
        /// The Ghostty surface, and with it the tmux client. Absent while the tab is
        /// released (hidden for a while) or has not been shown yet; the tmux session
        /// does not depend on it, and a new terminal attaches to the same session.
        terminal: Option<Entity<Terminal>>,
        /// Why the last attach failed. Kept so a tab that cannot attach is not
        /// retried on every frame; a few timed retries follow, and selecting the tab
        /// tries again.
        attach_error: Option<String>,
        attach_failures: u8,
    },
    Panel(PanelKind),
}

impl Tab {
    fn saved(&self) -> SavedTab {
        match &self.content {
            TabContent::Shell { shell_id, .. } => SavedTab::Shell {
                shell_id: shell_id.clone(),
            },
            TabContent::Panel(panel) => SavedTab::Panel { panel: *panel },
        }
    }

    fn shell_id(&self) -> Option<&str> {
        match &self.content {
            TabContent::Shell { shell_id, .. } => Some(shell_id),
            _ => None,
        }
    }

    fn panel(&self) -> Option<PanelKind> {
        match &self.content {
            TabContent::Panel(panel) => Some(*panel),
            _ => None,
        }
    }

    fn terminal(&self) -> Option<&Entity<Terminal>> {
        match &self.content {
            TabContent::Shell { terminal, .. } => terminal.as_ref(),
            _ => None,
        }
    }

    /// Drop the terminal, which frees its Ghostty surface, the surface's threads and
    /// render targets, and detaches its tmux client. The session keeps running.
    fn release_terminal(&mut self, cx: &mut Context<Workspace>) -> bool {
        let TabContent::Shell { terminal, .. } = &mut self.content else {
            return false;
        };
        let Some(terminal) = terminal.take() else {
            return false;
        };
        terminal.update(cx, |terminal, _| terminal.set_visible(false));
        true
    }

    fn set_visible(&self, visible: bool, cx: &mut Context<Workspace>) {
        if let Some(terminal) = self.terminal() {
            terminal.update(cx, |terminal, _| terminal.set_visible(visible));
        }
    }
}

struct Pane {
    tabs: Vec<Tab>,
    active: usize,
}

#[derive(Clone)]
struct DraggedTab {
    pane_id: PaneId,
    tab_id: TabId,
    project_id: String,
    title: String,
}

impl Render for DraggedTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme::palette(cx);
        div()
            .px(px(12.0))
            .py(px(7.0))
            .bg(rgb(colors.panel_active))
            .border_1()
            .border_color(rgb(colors.cyan))
            .text_color(rgb(colors.text))
            .font_family("Menlo")
            .text_size(px(11.0))
            .child(self.title.clone())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DockSide {
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Clone)]
struct SplitResize {
    path: Vec<bool>,
    axis: Axis,
    bounds: Bounds<Pixels>,
}

#[derive(Default)]
struct AccountUsage {
    codex: BTreeMap<PathBuf, CodexUsageEntry>,
}

#[derive(Default)]
struct CodexUsageEntry {
    codex: Option<ProviderUsage>,
    codex_error: Option<String>,
    pending: bool,
    last_attempt: u64,
    /// Consecutive failed reads; spaces out retries of an account that never works.
    failures: u32,
}
impl Global for AccountUsage {}

struct Workspace {
    project_creator: Option<Entity<ProjectCreator>>,
    folder_editor: Option<Entity<FolderEditor>>,
    collapsed_project_folders: HashSet<String>,
    project_settings_panel: Option<Entity<ProjectSettingsPanel>>,
    schedule_panel: Option<Entity<schedule_panel::SchedulePanel>>,
    file_explorer: Option<Entity<FileExplorer>>,
    locked_panes: Option<HashSet<PaneId>>,
    carry_layout: Option<ProjectLayout>,
    layout: Layout,
    layout_ready: bool,
    /// The size of the area the split ratios were last laid out for. `None` until the
    /// first draw of a layout, and left alone in focus mode, which draws no split tree.
    pane_area: Option<Extent>,
    /// When the window first drew a layout; sizes within `WINDOW_SETTLE` of it are not resizes.
    first_drawn: Option<Instant>,
    /// A pending save of ratios rewritten by a window resize; one write per resize.
    layout_save: Option<gpui::Task<()>>,
    panes: BTreeMap<PaneId, Pane>,
    active_pane: PaneId,
    next_pane_id: PaneId,
    next_tab_id: TabId,
    layouts: LayoutStore,
    restore_layout: Option<ProjectLayout>,
    restore_focus: Option<(bool, bool)>,
    settings_store: SettingsStore,
    settings_panel: Entity<SettingsPanel>,
    settings: Settings,
    appearance: Appearance,
    window_size: Option<WindowSize>,
    /// Last size the user gave this window; it follows the window across project switches.
    window_size_user: Option<WindowSize>,
    /// A user resize waiting out its debounce; promoted to `window_size` on the next save.
    window_size_pending: Option<WindowSize>,
    /// The window's last settled ordinary frame, which is where a zoomed window returns to.
    normal_bounds: Option<Bounds<Pixels>>,
    window_size_save: Option<gpui::Task<()>>,
    store: Store,
    sessions: SessionManager,
    state: State,
    project_id: String,
    selected_worktree_id: Option<String>,
    selected_task_id: Option<String>,
    detached_shell_ids: HashSet<String>,
    shells: Vec<ShellSession>,
    shell_cwds: BTreeMap<String, PathBuf>,
    metrics: BTreeMap<String, SessionMetrics>,
    session_refresh_pending: bool,
    session_refresh_generation: u64,
    agent_activity: BTreeMap<String, AgentActivity>,
    activity_tracker: Option<ActivityTracker>,
    project_last_edits: BTreeMap<String, u64>,
    project_recency_pending: bool,
    project_recency_sampled_at: Option<Instant>,
    project_sort_menu_open: bool,
    claude_usage: BTreeMap<String, ProviderUsage>,
    /// Session usage of Grok tabs, by shell. Read only while shown; see
    /// `refresh_grok_usage`.
    grok_usage: BTreeMap<String, usage::GrokTabUsage>,
    grok_usage_pending: bool,
    refresh_count: u64,
    syncing_project_ids: HashSet<String>,
    sidebar_visible: bool,
    focus_mode: bool,
    focus_centered: bool,
    drop_target: Option<(PaneId, Option<DockSide>)>,
    panel_menu: Option<PaneId>,
    resizing: Option<SplitResize>,
    tab_dragging: bool,
    terminal_snapshots: BTreeMap<TabId, Arc<gpui::RenderImage>>,
    /// The pending pass that releases hidden terminals; replaced whenever the set
    /// of hidden tabs changes.
    terminal_release: Option<gpui::Task<()>>,
    /// The pending retry of tabs whose terminal failed to attach.
    attach_retry: Option<gpui::Task<()>>,
    search_focused: bool,
    search: String,
    search_marked: Option<Range<usize>>,
    cwd: PathBuf,
    shell_name: String,
    notice: Option<String>,
    focus: FocusHandle,
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.1} GiB", bytes as f64 / (1024 * 1024 * 1024) as f64)
    } else if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024 * 1024) as f64)
    } else {
        format!("{} KiB", bytes / 1024)
    }
}

fn utf16_to_byte(text: &str, offset: usize) -> usize {
    let mut units = 0;
    for (byte, character) in text.char_indices() {
        if units >= offset {
            return byte;
        }
        units += character.len_utf16();
    }
    text.len()
}

fn file_explorer_root(
    state: &State,
    project_id: &str,
    selected_worktree_id: Option<&str>,
) -> Option<ExplorerRoot> {
    let project = state
        .projects
        .iter()
        .find(|project| project.id == project_id)?;
    if let Some(id) = selected_worktree_id {
        let worktree = state
            .worktrees
            .iter()
            .find(|worktree| worktree.id == id && worktree.project_id == project_id)?;
        Some(ExplorerRoot {
            path: worktree.path.clone(),
            label: format!("{} · {}", project.name, worktree.branch),
            worktree_id: Some(worktree.id.clone()),
        })
    } else {
        Some(ExplorerRoot {
            path: project.root.clone(),
            label: project.name.clone(),
            worktree_id: None,
        })
    }
}

enum FileEditorTarget {
    Existing(ShellSession),
    New,
}

fn file_editor_target(
    root: &ExplorerRoot,
    path: &Path,
    identity: file_preview::FileIdentity,
    project_id: &str,
    shells: &[ShellSession],
) -> Result<FileEditorTarget, String> {
    // A live editor owns this path already. Saving in Vim invalidates the
    // browser's cached size/mtime; no-follow path validation still applies.
    // The canonical path alone identifies the file: the project root (no
    // worktree id) and its primary worktree are one directory, while another
    // worktree resolves to a different path.
    let editor_path = file_preview::validated_live_editor_path(&root.path, path)?;
    if let Some(shell) = shells.iter().find(|shell| {
        shell.alive
            && shell.project_id.as_deref() == Some(project_id)
            && shell.editor_path.as_ref() == Some(&editor_path)
    }) {
        return Ok(FileEditorTarget::Existing(shell.clone()));
    }
    // Only a fresh launch must match the selection's original file identity.
    file_preview::validated_editor_path(&root.path, path, identity)?;
    Ok(FileEditorTarget::New)
}

/// The pane that inherits the space of `removed` when it closes, so focus can
/// stay next to where the user was working. Nearest to `removed` inside the
/// sibling subtree.
fn pane_inheriting_space(layout: &Layout, removed: PaneId) -> Option<PaneId> {
    let Layout::Split { first, second, .. } = layout else {
        return None;
    };
    match (first.as_ref(), second.as_ref()) {
        (Layout::Pane(id), sibling) if *id == removed => sibling.pane_ids().first().copied(),
        (sibling, Layout::Pane(id)) if *id == removed => sibling.pane_ids().last().copied(),
        _ => {
            pane_inheriting_space(first, removed).or_else(|| pane_inheriting_space(second, removed))
        }
    }
}

/// Left and top offsets of the window-edge docking targets. They stay clear of
/// the window controls island, which native full screen hides.
fn root_dock_insets(controls_visible: bool) -> (f32, f32) {
    if controls_visible {
        (WINDOW_CONTROLS_WIDTH + 2.0, WINDOW_CONTROLS_HEIGHT + 2.0)
    } else {
        // Match the right dock, which yields the corner to the top dock.
        (0.0, 12.0)
    }
}

/// Frees snapshot textures from the window's sprite atlas. `RenderImage` has no
/// `Drop`, so without this every snapshot leaks a surface-sized texture.
fn release_render_images(images: Vec<Arc<gpui::RenderImage>>, cx: &mut App) {
    if images.is_empty() {
        return;
    }
    // `App::drop_image` skips a window that is mid-update, and this runs from
    // event handlers and render. Deferred, the window is back in the app.
    cx.defer(move |cx| {
        for image in images {
            cx.drop_image(image, None);
        }
    });
}

fn project_recency_sources(state: &State) -> BTreeMap<String, Vec<PathBuf>> {
    let mut sources: BTreeMap<_, _> = state
        .projects
        .iter()
        .map(|project| {
            let mut roots = project.repository_roots.clone();
            roots.push(project.root.clone());
            (project.id.clone(), roots)
        })
        .collect();
    for worktree in &state.worktrees {
        if let Some(roots) = sources.get_mut(&worktree.project_id) {
            roots.push(worktree.path.clone());
        }
    }
    for roots in sources.values_mut() {
        roots.sort();
        roots.dedup();
    }
    sources
}

fn contextual_shell_title(
    title: String,
    shell: &ShellSession,
    project_id: &str,
    state: &State,
) -> String {
    match shell.project_id.as_deref().filter(|id| *id != project_id) {
        Some(id) => {
            let name = state
                .projects
                .iter()
                .find(|project| project.id == id)
                .map(|project| project.name.as_str())
                .unwrap_or(id);
            format!("{name} · {title}")
        }
        None => title,
    }
}

/// Account numbers are derived from frozen homes across all live shells in
/// each project, so separate windows and tab layouts agree on the same number.
fn codex_account_numbers(shells: &[ShellSession]) -> BTreeMap<String, usize> {
    let mut homes = BTreeMap::<&str, BTreeSet<&Path>>::new();
    for shell in shells
        .iter()
        .filter(|shell| shell.alive && shell.harness == Some(HarnessKind::Codex))
    {
        if let (Some(project_id), Some(home)) =
            (shell.project_id.as_deref(), shell.codex_home.as_deref())
        {
            homes.entry(project_id).or_default().insert(home);
        }
    }
    let mut numbers = BTreeMap::new();
    for shell in shells
        .iter()
        .filter(|shell| shell.alive && shell.harness == Some(HarnessKind::Codex))
    {
        let (Some(project_id), Some(home)) =
            (shell.project_id.as_deref(), shell.codex_home.as_deref())
        else {
            continue;
        };
        let Some(project_homes) = homes.get(project_id).filter(|homes| homes.len() > 1) else {
            continue;
        };
        if let Some(index) = project_homes
            .iter()
            .position(|candidate| *candidate == home)
        {
            numbers.insert(shell.id.clone(), index + 1);
        }
    }
    numbers
}

fn codex_tab_title(
    title: &str,
    shell: Option<&ShellSession>,
    numbers: &BTreeMap<String, usize>,
) -> String {
    match shell.and_then(|shell| numbers.get(&shell.id)) {
        Some(number) => format!("{title} · A{number}"),
        None => title.to_owned(),
    }
}

fn verified_shell_email(
    shell: &ShellSession,
    snapshot: Option<&codex_accounts::AccountsSnapshot>,
) -> Option<String> {
    shell.codex_account_email.clone().or_else(|| {
        snapshot?
            .accounts
            .iter()
            .find(|account| {
                shell.codex_account_id.as_deref() == Some(account.id.as_str())
                    && shell.codex_home.as_ref() == Some(&account.home)
            })?
            .email
            .clone()
    })
}

fn codex_session_status_label(
    shell: &ShellSession,
    numbers: &BTreeMap<String, usize>,
    snapshot: Option<&codex_accounts::AccountsSnapshot>,
) -> String {
    let email = verified_shell_email(shell, snapshot).unwrap_or_else(|| "Email unknown".to_owned());
    match numbers.get(&shell.id) {
        Some(number) => format!("CODEX A{number} · {email}"),
        None => format!("CODEX · {email}"),
    }
}

fn project_default_account_label(
    project: Option<&Project>,
    app_selected: Option<&str>,
    snapshot: Option<&codex_accounts::AccountsSnapshot>,
) -> String {
    let choice = project.map(|project| &project.codex_account);
    let (source, selected) = match choice {
        Some(ProjectCodexAccount::Saved(id)) => ("PROJECT", Some(id.as_str())),
        Some(ProjectCodexAccount::SystemDefault) => ("SYSTEM", None),
        _ => (
            "APP",
            app_selected.filter(|id| *id != codex_accounts::SYSTEM_DEFAULT_ID),
        ),
    };
    let account =
        selected.and_then(|id| snapshot?.accounts.iter().find(|account| account.id == id));
    let detail = match (selected, account) {
        (None, _) => "System default · email unknown".to_owned(),
        (Some(_), Some(account)) if account.available => account
            .email
            .clone()
            .unwrap_or_else(|| "Email unknown".to_owned()),
        (Some(_), Some(_)) => "Account unavailable".to_owned(),
        (Some(_), None) if snapshot.is_none() => "Email unknown".to_owned(),
        (Some(_), None) => "Account unavailable".to_owned(),
    };
    format!("DEFAULT ({source}) · {detail}")
}

/// Everything `Workspace::new` reads from disk. It is resolved before the
/// window opens, so a broken store or a vanished project folder is an error the
/// caller reports instead of a panic inside window creation.
struct WorkspaceStartup {
    store: Store,
    layouts: LayoutStore,
    settings_store: SettingsStore,
    sessions: SessionManager,
    state: State,
    project: Project,
}

impl WorkspaceStartup {
    fn prepare(startup_path: Option<PathBuf>, fallback_cwd: PathBuf) -> Result<Self, String> {
        sessions::prefetch_login_shell_dirs();
        Self::resolve(
            Store::open_default()?,
            LayoutStore::open_default()?,
            SettingsStore::open_default()?,
            SessionManager::open_default()?,
            startup_path,
            fallback_cwd,
        )
    }

    fn resolve(
        store: Store,
        layouts: LayoutStore,
        settings_store: SettingsStore,
        sessions: SessionManager,
        startup_path: Option<PathBuf>,
        fallback_cwd: PathBuf,
    ) -> Result<Self, String> {
        let initial = store.snapshot()?;
        let project = if let Some(path) = startup_path {
            let root = path.canonicalize().map_err(|error| {
                format!("Cannot open project folder {}: {error}", path.display())
            })?;
            match initial.projects.iter().find(|project| project.root == root) {
                Some(project) => project.clone(),
                None => store.add_project(root, None)?,
            }
        } else if let Some(project) = initial.active_project() {
            project.clone()
        } else {
            store.add_project(fallback_cwd, None)?
        };
        let state = store.snapshot()?;
        Ok(Self {
            store,
            layouts,
            settings_store,
            sessions,
            state,
            project,
        })
    }
}

impl Workspace {
    fn new(
        startup: WorkspaceStartup,
        restore: Option<runtime::RuntimeWindow>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let WorkspaceStartup {
            store,
            layouts,
            settings_store,
            sessions,
            state,
            project,
        } = startup;
        let settings = cx.global::<Settings>().clone();
        let appearance = cx.global::<Appearance>().clone();
        let settings_panel = cx.new(|cx| SettingsPanel::new(settings_store.clone(), cx));
        cx.subscribe(&settings_panel, |workspace, _, event, cx| match event {
            SettingsEvent::OrcaImported => {
                workspace.refresh_project_metadata(cx);
                workspace.notice = Some("Orca import completed".to_owned());
                cx.notify();
            }
        })
        .detach();
        let activity_tracker = ActivityTracker::at(sessions.state_home().to_path_buf());
        window.set_window_title(&format!("RiWork · {}", project.name));
        let selected_worktree_id = state
            .worktrees_for(&project.id)
            .into_iter()
            .find(|worktree| worktree.is_primary)
            .map(|worktree| worktree.id.clone());
        let shell = env::var("SHELL")
            .ok()
            .filter(|value| Path::new(value).is_file())
            .unwrap_or_else(|| "/bin/zsh".to_owned());
        let shell_name = Path::new(&shell)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("shell")
            .to_owned();

        let mut panes = BTreeMap::new();
        panes.insert(
            1,
            Pane {
                tabs: Vec::new(),
                active: 0,
            },
        );
        let mut workspace = Self {
            project_creator: None,
            folder_editor: None,
            collapsed_project_folders: HashSet::new(),
            project_settings_panel: None,
            schedule_panel: None,
            file_explorer: None,
            locked_panes: None,
            carry_layout: None,
            layout: Layout::Pane(1),
            layout_ready: false,
            pane_area: None,
            first_drawn: None,
            layout_save: None,
            panes,
            active_pane: 1,
            next_pane_id: 2,
            next_tab_id: 1,
            layouts,
            restore_layout: restore.as_ref().and_then(|window| window.layout.clone()),
            restore_focus: restore
                .as_ref()
                .map(|window| (window.focus_mode, window.focus_centered)),
            settings_store,
            settings_panel,
            settings,
            appearance,
            window_size: None,
            window_size_user: None,
            window_size_pending: None,
            normal_bounds: normal_window_bounds(window),
            window_size_save: None,
            store,
            sessions,
            state,
            project_id: project.id,
            selected_worktree_id,
            selected_task_id: None,
            detached_shell_ids: HashSet::new(),
            shells: Vec::new(),
            shell_cwds: BTreeMap::new(),
            metrics: BTreeMap::new(),
            session_refresh_pending: false,
            session_refresh_generation: 0,
            agent_activity: BTreeMap::new(),
            activity_tracker: Some(activity_tracker),
            project_last_edits: BTreeMap::new(),
            project_recency_pending: false,
            project_recency_sampled_at: None,
            project_sort_menu_open: false,
            claude_usage: BTreeMap::new(),
            grok_usage: BTreeMap::new(),
            grok_usage_pending: false,
            refresh_count: 0,
            syncing_project_ids: HashSet::new(),
            sidebar_visible: true,
            focus_mode: false,
            focus_centered: true,
            drop_target: None,
            panel_menu: None,
            resizing: None,
            tab_dragging: false,
            terminal_snapshots: BTreeMap::new(),
            terminal_release: None,
            attach_retry: None,
            search_focused: false,
            search: String::new(),
            search_marked: None,
            cwd: project.root,
            shell_name,
            notice: None,
            focus: cx.focus_handle(),
        };
        workspace.load_project(window, cx);
        workspace.refresh_project_recency(cx);
        if cua::CuaManager::open_default()
            .and_then(|manager| manager.driver_path())
            .is_err()
        {
            workspace.open_panel(PanelKind::Settings, workspace.active_pane, window, cx);
        }
        cx.observe_window_bounds(window, |workspace, window, cx| {
            // A hint stays where it opened, so a moved or resized window leaves it behind.
            tooltip::hide(cx);
            workspace.remember_window_size(window, true, cx);
        })
        .detach();
        cx.observe_global_in::<Settings>(window, |workspace, window, cx| {
            workspace.apply_settings(window, cx);
        })
        .detach();
        cx.observe_global_in::<Appearance>(window, |workspace, window, cx| {
            workspace.apply_settings(window, cx);
        })
        .detach();
        cx.observe_global::<settings::CodexAccountsState>(|_, cx| {
            request_codex_usage(false, cx);
            cx.notify();
        })
        .detach();
        cx.on_release(|workspace, _| workspace.save_layout())
            .detach();
        cx.observe_window_activation(window, |_, window, cx| {
            if window.is_window_active() {
                dock_menu::set_frontmost(window.window_handle().window_id().as_u64(), cx);
            } else {
                tooltip::hide(cx);
            }
        })
        .detach();
        if window.is_window_active() {
            dock_menu::set_frontmost(window.window_handle().window_id().as_u64(), cx);
        }
        workspace.announce_to_dock(window, cx);
        request_codex_usage(false, cx);
        for home in workspace
            .shells
            .iter()
            .filter(|shell| shell.alive && shell.harness == Some(HarnessKind::Codex))
            .filter_map(|shell| shell.codex_home.clone())
            .collect::<HashSet<_>>()
        {
            request_codex_usage_at(home, false, cx);
        }
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                if this
                    .update_in(cx, |workspace, window, cx| workspace.refresh(window, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        workspace
    }

    fn terminal_options(
        command: String,
        cwd: PathBuf,
        theme: Option<TerminalTheme>,
    ) -> TerminalOptions {
        let mut options = TerminalOptions::new(command, cwd);
        options.quiet_login = true;
        options.configuration = theme.map_or(
            TerminalConfiguration::UserDefault,
            TerminalConfiguration::UserDefaultWithOverride,
        );
        options
    }

    fn terminal_theme(settings: &Settings, appearance: &Appearance) -> Option<TerminalTheme> {
        appearance.terminal.or_else(|| {
            settings
                .use_riwork_colors
                .then(theme::riwork_terminal_theme)
        })
    }

    fn spawn_tab(
        &mut self,
        pane_id: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let worktree_id = self.selected_worktree_id.clone();
        let cwd = worktree_id
            .as_ref()
            .and_then(|id| {
                self.state
                    .worktrees
                    .iter()
                    .find(|worktree| &worktree.id == id)
            })
            .map(|worktree| worktree.path.clone())
            .unwrap_or_else(|| self.cwd.clone());
        let shell = self
            .sessions
            .create(self.project_id.clone(), worktree_id, cwd, None)?;
        let result = self.attach_session(pane_id, shell, window, cx);
        if let Ok(shells) = self.sessions.list() {
            self.shells = shells;
        }
        self.refresh_agent_activity(cx);
        result
    }

    fn refresh_agent_activity(&mut self, cx: &mut Context<Self>) {
        if let Some(mut tracker) = self.activity_tracker.take() {
            let shells = self.shells.clone();
            let home = self.sessions.state_home().to_path_buf();
            let work = cx.background_executor().spawn(async move {
                let activity = tracker.sample(&shells);
                for completion in tracker.take_completions() {
                    if let Err(error) = notifications::record_completion(
                        &home,
                        &completion.shell_id,
                        &completion.event_id,
                        HarnessKind::Codex,
                    ) {
                        eprintln!("riwork notifications: {error}");
                    }
                }
                (tracker, activity)
            });
            cx.spawn(async move |this, cx| {
                let (tracker, activity) = work.await;
                let _ = this.update(cx, |workspace, cx| {
                    workspace.activity_tracker = Some(tracker);
                    workspace.agent_activity = activity;
                    cx.notify();
                });
            })
            .detach();
        }
    }

    fn refresh_project_recency(&mut self, cx: &mut Context<Self>) {
        if self.project_recency_pending
            || self
                .project_recency_sampled_at
                .is_some_and(|sampled| sampled.elapsed() < Duration::from_secs(30))
        {
            return;
        }
        self.project_recency_pending = true;
        self.project_recency_sampled_at = Some(Instant::now());
        let state = self.state.clone();
        let sources = project_recency_sources(&state);
        let work = cx.background_executor().spawn(async move {
            // One scan at a time for the whole process: the other windows read
            // what it leaves in the shared cache.
            let edits = project_recency::scan(&state);
            (sources, edits)
        });
        cx.spawn(async move |this, cx| {
            let (sources, edits) = work.await;
            let _ = this.update(cx, |workspace, cx| {
                workspace.project_recency_pending = false;
                if project_recency_sources(&workspace.state) == sources {
                    workspace.project_last_edits = edits;
                } else {
                    // A project or worktree moved while scanning; sample its new roots.
                    workspace.project_recency_sampled_at = None;
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// A terminal attached to `shell`'s tmux session. New tabs, tabs coming back from
    /// release and window restores all start theirs here, so size, theme and
    /// environment cannot differ between them.
    fn spawn_terminal(
        &self,
        shell: &ShellSession,
        focus_on_spawn: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Entity<Terminal>, String> {
        // A restoring app leaves these where a hang in the terminal library
        // would otherwise leave no trace (see `runtime::RestoreWatchdog`).
        runtime::note_attach_started(&shell.id, &shell.cwd);
        let spawned = self.sessions.attach_command(&shell.id).and_then(|command| {
            let mut options = Self::terminal_options(
                command,
                shell.cwd.clone(),
                Self::terminal_theme(&self.settings, &self.appearance),
            );
            options.focus_on_spawn = focus_on_spawn;
            Terminal::spawn(options, window, cx)
        });
        runtime::note_attach_finished(&shell.id, spawned.is_ok());
        spawned
    }

    fn shell_tab(&mut self, shell: ShellSession, terminal: Option<Entity<Terminal>>) -> Tab {
        let tab_id = self.next_tab_id;
        self.next_tab_id += 1;
        let worktree_name = shell
            .worktree_id
            .as_ref()
            .and_then(|id| {
                self.state
                    .worktrees
                    .iter()
                    .find(|worktree| &worktree.id == id)
            })
            .map(|worktree| worktree.branch.as_str())
            .unwrap_or("root");
        let title = if let Some(path) = &shell.editor_path {
            format!(
                "VIM · {}",
                path.file_name()
                    .map(|name| name.to_string_lossy())
                    .unwrap_or_default()
            )
        } else if shell.kind == ShellKind::Orchestrator {
            orchestrator_tab_title(&shell)
        } else {
            format!(
                "{} {:02} · {}",
                shell.harness.map(harness_name).unwrap_or(&self.shell_name),
                tab_id,
                worktree_name
            )
        };
        let title = contextual_shell_title(title, &shell, &self.project_id, &self.state);
        Tab {
            id: tab_id,
            title,
            content: TabContent::Shell {
                shell_id: shell.id,
                worktree_id: shell.worktree_id,
                terminal,
                attach_error: None,
                attach_failures: 0,
            },
            hidden_since: None,
        }
    }

    fn attach_session(
        &mut self,
        pane_id: PaneId,
        shell: ShellSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let terminal = self.spawn_terminal(&shell, true, window, cx)?;
        // The new terminal takes focus on spawn.
        self.search_focused = false;
        self.search_marked = None;
        let tab = self.shell_tab(shell, Some(terminal));
        let pane = self
            .panes
            .get_mut(&pane_id)
            .ok_or_else(|| format!("Pane {pane_id} no longer exists"))?;
        for existing in &pane.tabs {
            existing.set_visible(false, cx);
        }
        pane.tabs.push(tab);
        pane.active = pane.tabs.len() - 1;
        self.active_pane = pane_id;
        self.notice = None;
        self.session_refresh_generation = self.session_refresh_generation.wrapping_add(1);
        if self.layout_ready {
            self.remember_active_worktree(cx);
        }
        cx.notify();
        Ok(())
    }

    /// Add a shell's tab without a terminal. Restoring a window opens every saved
    /// tab, but only each pane's active one is ever seen, so the others wait for a
    /// terminal until they are shown.
    fn restore_shell_tab(&mut self, pane_id: PaneId, shell: ShellSession) {
        let tab = self.shell_tab(shell, None);
        let Some(pane) = self.panes.get_mut(&pane_id) else {
            return;
        };
        pane.tabs.push(tab);
        pane.active = pane.tabs.len() - 1;
        self.active_pane = pane_id;
        self.session_refresh_generation = self.session_refresh_generation.wrapping_add(1);
    }

    /// `changed_only` ignores a size the window already settled at, so the frame it
    /// opened with (possibly clamped to the display) or returned to after zooming is
    /// not mistaken for the user's choice. Turning the setting on passes false to
    /// remember the current size right away.
    fn remember_window_size(
        &mut self,
        window: &Window,
        changed_only: bool,
        cx: &mut Context<Self>,
    ) {
        // Fullscreen and zoomed frames must not replace the normal window size.
        let Some(bounds) = normal_window_bounds(window) else {
            self.window_size_pending = None;
            self.window_size_save = None;
            return;
        };
        if changed_only
            && self
                .normal_bounds
                .is_some_and(|normal| normal.size == bounds.size)
        {
            // Only moved, or back at the settled size.
            self.normal_bounds = Some(bounds);
            self.window_size_pending = None;
            self.window_size_save = None;
            return;
        }
        self.window_size_pending = self
            .settings
            .remember_window_size
            .then(|| WindowSize::new(bounds.size.width.as_f32(), bounds.size.height.as_f32()))
            .flatten();
        // Avoid writing for every frame during a drag. Release also saves the final size.
        self.window_size_save = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(250))
                .await;
            let _ = this.update_in(cx, |workspace, window, _| {
                // Zooming animates through ordinary-looking frames; only a window that
                // is still ordinary now has settled on a size.
                let Some(bounds) = normal_window_bounds(window) else {
                    workspace.window_size_pending = None;
                    return;
                };
                workspace.normal_bounds = Some(bounds);
                if workspace.window_size_pending.is_some() {
                    workspace.save_layout();
                }
            });
        }));
    }

    fn apply_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let settings = cx.global::<Settings>().clone();
        if settings.project_order != self.settings.project_order {
            self.project_sort_menu_open = false;
        }
        if cx.global::<Appearance>().selected != settings.theme {
            sync_appearance(cx);
        }
        let appearance = cx.global::<Appearance>().clone();
        let previous_theme = Self::terminal_theme(&self.settings, &self.appearance);
        let next_theme = Self::terminal_theme(&settings, &appearance);
        let colors_changed = previous_theme != next_theme
            || (next_theme.is_none() && self.appearance.ghostty != appearance.ghostty);
        let restore_terminal_focus = next_theme.is_none()
            && !self.modal_open()
            && !self.search_focused
            && self.panel_menu.is_none()
            && !self.tab_dragging
            && self
                .panes
                .get(&self.active_pane)
                .and_then(|pane| pane.tabs.get(pane.active))
                .is_some_and(|tab| tab.shell_id().is_some());
        let remember_changed = settings.remember_window_size != self.settings.remember_window_size;
        self.settings = settings;
        self.appearance = appearance;
        if remember_changed && self.settings.remember_window_size {
            self.remember_window_size(window, false, cx);
        }
        if colors_changed {
            let mut error = None;
            for pane in self.panes.values_mut() {
                for tab in &mut pane.tabs {
                    let TabContent::Shell {
                        shell_id, terminal, ..
                    } = &mut tab.content
                    else {
                        continue;
                    };
                    // A released tab has no client to update; it attaches with the
                    // new colors when it is shown again.
                    let Some(terminal) = terminal else {
                        continue;
                    };
                    if let Some(theme) = next_theme {
                        // Ghostty can apply palette overrides without replacing the client.
                        if let Err(message) =
                            terminal.update(cx, |terminal, _| terminal.update_theme(theme))
                        {
                            error = Some(message);
                        }
                        continue;
                    }
                    // Returning to the native Ghostty config removes every override.
                    // Only display clients reconnect; tmux shells and harnesses stay alive.
                    let replacement = (|| {
                        let shell = self.sessions.get(shell_id)?;
                        if !shell.alive {
                            return Ok(None);
                        }
                        let command = self.sessions.attach_command(shell_id)?;
                        let mut options = Self::terminal_options(command, shell.cwd, None);
                        options.focus_on_spawn = false;
                        Terminal::spawn(options, window, cx).map(Some)
                    })();
                    match replacement {
                        Ok(Some(replacement)) => {
                            terminal.update(cx, |terminal, _| terminal.set_visible(false));
                            replacement.update(cx, |terminal, _| terminal.set_visible(false));
                            *terminal = replacement;
                        }
                        Ok(None) => {}
                        Err(message) => error = Some(message),
                    }
                }
            }
            self.release_snapshots(cx);
            if let Some(error) = error {
                self.notice = Some(error);
            }
            if restore_terminal_focus {
                self.focus_active(window, cx);
            }
        }
        cx.notify();
    }

    fn panel_title(panel: PanelKind) -> &'static str {
        match panel {
            PanelKind::Projects => "PROJECTS",
            PanelKind::Worktrees => "WORKTREES",
            PanelKind::Files => "FILES",
            PanelKind::Tasks => "TASKS",
            PanelKind::Shells => "SHELLS",
            PanelKind::Usage => "USAGE",
            PanelKind::Settings => "SETTINGS",
            PanelKind::Schedules => "SCHEDULES",
            PanelKind::ProjectSettings => "PROJECT SETTINGS",
        }
    }

    fn attach_panel(&mut self, pane_id: PaneId, panel: PanelKind, cx: &mut Context<Self>) {
        if panel == PanelKind::Schedules && self.schedule_panel.is_none() {
            let store = self.store.clone();
            let sessions = self.sessions.clone();
            let project = self.project_id.clone();
            let workspace = self.selected_worktree_id.clone();
            self.schedule_panel = Some(cx.new(|cx| {
                schedule_panel::SchedulePanel::new(store, sessions, project, workspace, cx)
            }));
        }
        if panel == PanelKind::ProjectSettings {
            self.ensure_project_settings(cx);
        }
        if panel == PanelKind::Files {
            self.ensure_file_explorer(cx);
        }
        if let Some(pane) = self.panes.get_mut(&pane_id) {
            for tab in &pane.tabs {
                tab.set_visible(false, cx);
            }
            let id = self.next_tab_id;
            self.next_tab_id += 1;
            pane.tabs.push(Tab {
                id,
                title: Self::panel_title(panel).to_owned(),
                content: TabContent::Panel(panel),
                hidden_since: None,
            });
            pane.active = pane.tabs.len() - 1;
        }
    }

    fn add_navigation_pane(&mut self, cx: &mut Context<Self>) {
        let pane_id = self.next_pane_id;
        self.next_pane_id += 1;
        self.panes.insert(
            pane_id,
            Pane {
                tabs: Vec::new(),
                active: 0,
            },
        );
        for panel in [
            PanelKind::Projects,
            PanelKind::Files,
            PanelKind::Worktrees,
            PanelKind::Tasks,
            PanelKind::Shells,
        ] {
            self.attach_panel(pane_id, panel, cx);
        }
        self.panes.get_mut(&pane_id).unwrap().active = 0;
        self.layout = Layout::Split {
            axis: Axis::SideBySide,
            ratio: 0.27,
            first: Box::new(Layout::Pane(pane_id)),
            second: Box::new(self.layout.clone()),
        };
    }

    fn open_panel(
        &mut self,
        panel: PanelKind,
        pane_id: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.ensure_layout(window, cx) {
            return;
        }
        self.panel_menu = None;
        let existing = self.panes.iter().find_map(|(id, pane)| {
            pane.tabs
                .iter()
                .find(|tab| matches!(tab.content, TabContent::Panel(kind) if kind == panel))
                .map(|tab| (*id, tab.id))
        });
        if let Some((pane_id, tab_id)) = existing {
            self.select_tab(pane_id, tab_id, window, cx);
        } else {
            self.attach_panel(pane_id, panel, cx);
            self.active_pane = pane_id;
            self.focus_active(window, cx);
            self.save_layout();
            cx.notify();
        }
        if panel == PanelKind::Usage {
            // Show Grok's figures now, not on the next tick.
            self.refresh_grok_usage(false, cx);
        }
    }

    fn panel_action(&mut self, action: PanelAction, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            PanelAction::ToggleProjectSortMenu => {
                self.project_sort_menu_open = !self.project_sort_menu_open;
                if self.project_sort_menu_open {
                    self.panel_menu = None;
                    self.finish_tab_drag(cx);
                    self.focus.focus(window, cx);
                } else {
                    self.focus_active(window, cx);
                }
                cx.notify();
            }
            PanelAction::CloseProjectSortMenu => {
                self.project_sort_menu_open = false;
                self.focus_active(window, cx);
                cx.notify();
            }
            PanelAction::SetProjectOrder(order) => {
                self.project_sort_menu_open = false;
                match self
                    .settings_store
                    .update(|settings| settings.project_order = order)
                {
                    Ok(settings) => {
                        self.notice = None;
                        cx.set_global(settings);
                    }
                    Err(error) => self.notice = Some(error),
                }
                self.focus_active(window, cx);
                cx.notify();
            }
            PanelAction::CreateProject => self.begin_project_creation(window, cx),
            PanelAction::CreateFolder => self.begin_folder_edit(None, window, cx),
            PanelAction::CreateSubfolder(id) => {
                self.begin_folder_edit_in(None, Some(&id), window, cx)
            }
            PanelAction::EditFolder(id) => self.begin_folder_edit(Some(&id), window, cx),
            PanelAction::BeginProjectDrag => {
                self.panel_menu = None;
                self.project_sort_menu_open = false;
                self.search_focused = false;
                self.begin_tab_drag(cx);
            }
            PanelAction::MoveProject {
                project_id,
                folder_id,
            } => {
                match self
                    .store
                    .move_project_to_folder(&project_id, folder_id.as_deref())
                {
                    Ok(_) => {
                        self.notice = None;
                        self.refresh_project_metadata(cx);
                        self.expand_project_folder(folder_id.as_deref());
                    }
                    Err(error) => self.notice = Some(error),
                }
                self.finish_tab_drag(cx);
                cx.notify();
            }
            PanelAction::MoveFolder {
                folder_id,
                parent_id,
            } => {
                match self
                    .store
                    .move_project_folder(&folder_id, parent_id.as_deref())
                {
                    Ok(_) => {
                        self.notice = None;
                        self.refresh_project_metadata(cx);
                        self.expand_project_folder(parent_id.as_deref());
                    }
                    Err(error) => self.notice = Some(error),
                }
                self.finish_tab_drag(cx);
                cx.notify();
            }
            PanelAction::RemoveFolder(id) => {
                match self.store.remove_project_folder(&id) {
                    Ok(()) => {
                        self.collapsed_project_folders.remove(&id);
                        self.refresh_project_metadata(cx);
                    }
                    Err(error) => self.notice = Some(error),
                }
                cx.notify();
            }
            PanelAction::ToggleFolder(id) => {
                if !self.collapsed_project_folders.remove(&id) {
                    self.collapsed_project_folders.insert(id);
                }
                cx.notify();
            }
            PanelAction::ProjectSettings(id) => {
                self.activate_project(&id, true, window, cx);
                if self.project_id != id {
                    return;
                }
                let pane_id = self
                    .panes
                    .iter()
                    .rev()
                    .find(|(_, pane)| pane.tabs.iter().any(|tab| tab.shell_id().is_some()))
                    .map(|(id, _)| *id)
                    .unwrap_or(self.active_pane);
                self.open_panel(PanelKind::ProjectSettings, pane_id, window, cx);
            }
            PanelAction::ToggleProjectNotifications(id) => {
                let enabled = match self.store.snapshot().and_then(|state| {
                    state
                        .project(&id)
                        .map(|project| !project.notify_on_agent_done)
                }) {
                    Ok(enabled) => enabled,
                    Err(error) => {
                        self.notice = Some(error);
                        cx.notify();
                        return;
                    }
                };
                match self.store.set_project_notifications(&id, enabled) {
                    Ok(project) => {
                        if enabled {
                            notifications::show_enabled(&project, cx);
                        } else if let Err(error) =
                            notifications::cancel_pending(self.sessions.state_home(), &id)
                        {
                            self.notice = Some(error);
                        }
                        self.refresh_project_metadata(cx);
                        cx.refresh_windows();
                    }
                    Err(error) => {
                        self.notice = Some(error);
                        cx.notify();
                    }
                }
            }
            PanelAction::Project(id) => self.activate_project(&id, true, window, cx),
            PanelAction::OpenProject(id) => self.open_project_window(&id, cx),
            PanelAction::Worktree(id) => self.select_worktree(&id, window, cx),
            PanelAction::Task(id) => self.select_task(&id, window, cx),
            PanelAction::Shell(id) => self.show_shell(&id, window, cx),
            PanelAction::Search => {
                self.search_focused = true;
                self.focus.focus(window, cx);
                cx.notify();
            }
        }
    }

    fn refresh_project_metadata(&mut self, cx: &mut Context<Self>) {
        match self.store.snapshot() {
            Ok(state) => self.state = state,
            Err(error) => self.notice = Some(error),
        }
        self.project_recency_sampled_at = None;
        self.refresh_project_recency(cx);
        if let Some(panel) = &self.project_settings_panel {
            panel.update(cx, |panel, cx| panel.refresh_folders(cx));
        }
        cx.notify();
    }

    fn ensure_project_settings(&mut self, cx: &mut Context<Self>) {
        if self.project_settings_panel.is_some() {
            return;
        }
        let Ok(project) = self.state.project(&self.project_id).cloned() else {
            return;
        };
        let store = self.store.clone();
        let panel = cx.new(|cx| ProjectSettingsPanel::new(store, project, cx));
        cx.subscribe(&panel, |workspace, _, event, cx| {
            if let ProjectSettingsEvent::Saved(project) = event {
                workspace.notice = Some(format!("Saved {}", project.name));
            }
            workspace.refresh_project_metadata(cx);
        })
        .detach();
        self.project_settings_panel = Some(panel);
    }

    fn ensure_file_explorer(&mut self, cx: &mut Context<Self>) {
        if self.file_explorer.is_none() {
            let panel = cx.new(FileExplorer::new);
            cx.subscribe(&panel, |workspace, _, event, cx| match event {
                FileExplorerEvent::Edit {
                    root,
                    path,
                    identity,
                } => {
                    let view = cx.weak_entity();
                    let entity_id = cx.entity_id();
                    let (root, path, identity) = (root.clone(), path.clone(), *identity);
                    cx.defer(move |app| {
                        app.with_window(entity_id, |window, app| {
                            let _ = view.update(app, |workspace, cx| {
                                workspace.open_file_editor(root, path, identity, window, cx);
                            });
                        });
                    });
                }
                FileExplorerEvent::Open(path) => cx.open_with_system(path),
                FileExplorerEvent::Reveal(path) => cx.reveal_path(path),
                FileExplorerEvent::CopyRelativePath(path) => {
                    cx.write_to_clipboard(ClipboardItem::new_string(
                        path.to_string_lossy().into_owned(),
                    ));
                    workspace.notice = Some(format!("Copied {}", path.display()));
                    cx.notify();
                }
            })
            .detach();
            self.file_explorer = Some(panel);
        }
        self.sync_file_explorer(cx);
    }

    fn sync_file_explorer(&mut self, cx: &mut Context<Self>) {
        if let Some(panel) = &self.file_explorer {
            let root = file_explorer_root(
                &self.state,
                &self.project_id,
                self.selected_worktree_id.as_deref(),
            );
            panel.update(cx, |panel, cx| panel.set_root(root, cx));
        }
    }

    fn open_file_editor(
        &mut self,
        root: ExplorerRoot,
        path: PathBuf,
        identity: file_preview::FileIdentity,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Err(error) = self.open_file_editor_inner(root, path, identity, window, cx) {
            self.notice = Some(error);
            cx.notify();
        }
    }

    fn open_file_editor_inner(
        &mut self,
        root: ExplorerRoot,
        path: PathBuf,
        identity: file_preview::FileIdentity,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if !self.ensure_layout(window, cx) {
            return Err("The workspace is still loading.".into());
        }
        if file_explorer_root(
            &self.state,
            &self.project_id,
            self.selected_worktree_id.as_deref(),
        ) != Some(root.clone())
        {
            return Err("The selected worktree changed. Select the file again.".into());
        }
        let shells = self.sessions.list()?;
        if let FileEditorTarget::Existing(shell) =
            file_editor_target(&root, &path, identity, &self.project_id, &shells)?
        {
            if let Some((pane_id, tab_id)) = self.panes.iter().find_map(|(pane_id, pane)| {
                pane.tabs
                    .iter()
                    .find(|tab| tab.shell_id() == Some(&shell.id))
                    .map(|tab| (*pane_id, tab.id))
            }) {
                self.select_tab(pane_id, tab_id, window, cx);
            } else {
                let pane_id = self.editor_pane(cx)?;
                self.attach_session(pane_id, shell, window, cx)?;
                self.focus_active(window, cx);
                self.save_layout();
            }
            return Ok(());
        }
        let shell = self.sessions.create_editor(
            self.project_id.clone(),
            root.worktree_id,
            root.path,
            path,
            identity,
        )?;
        let pane_id = self.editor_pane(cx)?;
        self.attach_session(pane_id, shell, window, cx)?;
        if let Ok(shells) = self.sessions.list() {
            self.shells = shells;
        }
        self.focus_active(window, cx);
        self.save_layout();
        Ok(())
    }

    fn editor_pane(&mut self, cx: &mut Context<Self>) -> Result<PaneId, String> {
        if let Some(pane_id) = self
            .panes
            .iter()
            .find(|(id, pane)| {
                !self.pane_is_locked(**id)
                    && pane
                        .tabs
                        .iter()
                        .any(|tab| matches!(tab.content, TabContent::Shell { .. }))
            })
            .map(|(id, _)| *id)
            .or_else(|| {
                self.panes
                    .keys()
                    .copied()
                    .find(|id| !self.pane_is_locked(*id))
            })
        {
            return Ok(pane_id);
        }
        let new_pane = self.next_pane_id;
        self.next_pane_id += 1;
        let target = self.active_pane;
        if !self.layout.split(target, Axis::SideBySide, new_pane) {
            return Err("Cannot make room for an editor tab.".into());
        }
        self.panes.insert(
            new_pane,
            Pane {
                tabs: Vec::new(),
                active: 0,
            },
        );
        cx.notify();
        Ok(new_pane)
    }

    fn remember_active_worktree(&mut self, cx: &mut Context<Self>) {
        let worktree_id = self
            .panes
            .get(&self.active_pane)
            .and_then(|pane| pane.tabs.get(pane.active))
            .and_then(|tab| match &tab.content {
                TabContent::Shell {
                    shell_id,
                    worktree_id,
                    ..
                } => {
                    let assigned = worktree_id.as_ref().and_then(|id| {
                        self.state
                            .worktrees
                            .iter()
                            .find(|worktree| worktree.id == *id)
                    });
                    if assigned.is_some_and(|worktree| worktree.project_id != self.project_id) {
                        return None;
                    }
                    let project_scoped = self.shells.iter().any(|shell| {
                        shell.id == *shell_id
                            && shell.project_id.as_deref() == Some(self.project_id.as_str())
                    });
                    if let Some(id) = worktree_id.as_ref().filter(|_| assigned.is_none()) {
                        return project_scoped.then(|| id.clone());
                    }
                    if assigned.is_none() && !project_scoped {
                        return None;
                    }
                    // A removed launch worktree remains an explicit unavailable context.
                    let current = self.shell_cwds.get(shell_id).and_then(|cwd| {
                        self.state
                            .worktrees
                            .iter()
                            .filter(|worktree| {
                                worktree.project_id == self.project_id
                                    && cwd.starts_with(&worktree.path)
                            })
                            .max_by_key(|worktree| worktree.path.components().count())
                    });
                    current
                        .map(|worktree| worktree.id.clone())
                        .or_else(|| worktree_id.clone())
                }
                _ => None,
            });
        if let Some(id) = worktree_id {
            self.selected_worktree_id = Some(id);
        }
        self.sync_file_explorer(cx);
    }

    fn begin_folder_edit(&mut self, id: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
        self.begin_folder_edit_in(id, None, window, cx);
    }

    fn expand_project_folder(&mut self, id: Option<&str>) {
        let mut current = id.map(str::to_owned);
        let mut seen = HashSet::new();
        while let Some(id) = current {
            if !seen.insert(id.clone()) {
                break;
            }
            self.collapsed_project_folders.remove(&id);
            current = self
                .state
                .project_folders
                .iter()
                .find(|folder| folder.id == id)
                .and_then(|folder| folder.parent_id.clone());
        }
    }

    fn begin_folder_edit_in(
        &mut self,
        id: Option<&str>,
        parent_id: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.folder_editor.is_some() || self.project_creator.is_some() {
            return;
        }
        let folder = id
            .and_then(|id| self.state.project_folder(id).ok())
            .cloned();
        self.search_focused = false;
        self.panel_menu = None;
        self.notice = None;
        self.begin_tab_drag(cx);
        let store = self.store.clone();
        let parent_id = parent_id.map(str::to_owned);
        let editor = cx.new(|cx| match parent_id {
            Some(parent_id) => FolderEditor::new_in(store, folder, Some(parent_id), cx),
            None => FolderEditor::new(store, folder, cx),
        });
        editor.update(cx, |editor, cx| editor.focus(window, cx));
        cx.subscribe_in(&editor, window, |workspace, _, event, window, cx| {
            workspace.folder_editor = None;
            workspace.finish_tab_drag(cx);
            if let FolderEditorEvent::Saved(folder) = event {
                workspace.refresh_project_metadata(cx);
                workspace.expand_project_folder(Some(&folder.id));
            }
            workspace.focus_active(window, cx);
            cx.notify();
        })
        .detach();
        self.folder_editor = Some(editor);
        cx.notify();
    }

    fn move_tab(
        &mut self,
        drag: &DraggedTab,
        target: PaneId,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if drag.project_id != self.project_id
            || !self.layout_ready
            || !self.panes.contains_key(&target)
        {
            return;
        }
        let Some(source) = self.panes.get_mut(&drag.pane_id) else {
            return;
        };
        let Some(from) = source.tabs.iter().position(|tab| tab.id == drag.tab_id) else {
            return;
        };
        let selected = source.tabs.get(source.active).map(|tab| tab.id);
        let tab = source.tabs.remove(from);
        tab.set_visible(false, cx);
        source.active = selected
            .and_then(|id| source.tabs.iter().position(|tab| tab.id == id))
            .unwrap_or_else(|| from.min(source.tabs.len().saturating_sub(1)));
        let target_index = if drag.pane_id == target && from < index {
            index.saturating_sub(1)
        } else {
            index
        };
        let dest = self.panes.get_mut(&target).unwrap();
        let index = target_index.min(dest.tabs.len());
        dest.tabs.insert(index, tab);
        dest.active = index;
        if drag.pane_id != target
            && self
                .panes
                .get(&drag.pane_id)
                .is_some_and(|pane| pane.tabs.is_empty())
        {
            self.panes.remove(&drag.pane_id);
            if let Some(layout) = self.layout.clone().without(drag.pane_id) {
                self.layout = layout;
            }
        }
        for pane in self.panes.values() {
            for (index, tab) in pane.tabs.iter().enumerate() {
                tab.set_visible(index == pane.active, cx);
            }
        }
        self.active_pane = target;
        self.search_focused = false;
        self.search_marked = None;
        self.drop_target = None;
        self.focus_active(window, cx);
        self.save_layout();
        cx.notify();
    }

    fn dock_tab(
        &mut self,
        drag: &DraggedTab,
        target: Option<PaneId>,
        side: DockSide,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if drag.project_id != self.project_id || !self.layout_ready {
            return;
        }
        if !self
            .panes
            .get(&drag.pane_id)
            .is_some_and(|pane| pane.tabs.iter().any(|tab| tab.id == drag.tab_id))
        {
            return;
        }
        if target == Some(drag.pane_id)
            && self
                .panes
                .get(&drag.pane_id)
                .is_some_and(|pane| pane.tabs.len() == 1)
        {
            return;
        }
        let axis = if matches!(side, DockSide::Left | DockSide::Right) {
            Axis::SideBySide
        } else {
            Axis::Stacked
        };
        let new_first = matches!(side, DockSide::Left | DockSide::Top);
        let id = self.next_pane_id;
        self.next_pane_id += 1;
        let next_layout = if let Some(target) = target {
            let mut layout = self.layout.clone();
            if !layout.split_with(target, axis, id, new_first) {
                return;
            }
            layout
        } else if new_first {
            Layout::Split {
                axis,
                ratio: 0.25,
                first: Box::new(Layout::Pane(id)),
                second: Box::new(self.layout.clone()),
            }
        } else {
            Layout::Split {
                axis,
                ratio: 0.75,
                first: Box::new(self.layout.clone()),
                second: Box::new(Layout::Pane(id)),
            }
        };
        self.layout = next_layout;
        self.panes.insert(
            id,
            Pane {
                tabs: Vec::new(),
                active: 0,
            },
        );
        self.move_tab(drag, id, 0, window, cx);
    }

    fn pane_drop(
        &mut self,
        drag: &DraggedTab,
        pane_id: PaneId,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let position = window.mouse_position();
        let viewport = window.viewport_size();
        let outer = if position.x < px(12.0) {
            Some(DockSide::Left)
        } else if position.x > viewport.width - px(12.0) {
            Some(DockSide::Right)
        } else if position.y < px(12.0) {
            Some(DockSide::Top)
        } else if position.y > viewport.height - px(12.0) {
            Some(DockSide::Bottom)
        } else {
            None
        };
        if let Some(side) = outer {
            self.dock_tab(drag, None, side, window, cx);
            return;
        }
        let local = position - bounds.origin;
        let x = local.x.as_f32() / bounds.size.width.as_f32().max(1.0);
        let y = local.y.as_f32() / bounds.size.height.as_f32().max(1.0);
        let side = if x < 0.23 {
            Some(DockSide::Left)
        } else if x > 0.77 {
            Some(DockSide::Right)
        } else if y < 0.23 {
            Some(DockSide::Top)
        } else if y > 0.77 {
            Some(DockSide::Bottom)
        } else {
            None
        };
        match side {
            Some(side) => self.dock_tab(drag, Some(pane_id), side, window, cx),
            None => {
                let len = self
                    .panes
                    .get(&pane_id)
                    .map(|pane| pane.tabs.len())
                    .unwrap_or(0);
                self.move_tab(drag, pane_id, len, window, cx);
            }
        }
    }

    /// Drops the terminal snapshots and frees their atlas textures.
    fn release_snapshots(&mut self, cx: &mut Context<Self>) {
        let snapshots = std::mem::take(&mut self.terminal_snapshots);
        release_render_images(snapshots.into_values().collect(), cx);
    }

    fn modal_open(&self) -> bool {
        self.project_creator.is_some() || self.folder_editor.is_some()
    }

    fn begin_tab_drag(&mut self, cx: &mut Context<Self>) {
        self.release_snapshots(cx);
        self.tab_dragging = true;
        for (pane_id, pane) in &self.panes {
            if let Some(tab) = pane.tabs.get(pane.active) {
                // Only what is on screen is frozen. Focus mode hides the other panes'
                // terminals, and a released one has nothing to capture.
                if !self.tab_is_shown(*pane_id, pane.active, pane.active) {
                    continue;
                }
                if let Some(terminal) = tab.terminal() {
                    let snapshot = terminal.update(cx, |terminal, _| {
                        let snapshot = terminal.snapshot().ok();
                        terminal.set_visible(false);
                        snapshot
                    });
                    if let Some(snapshot) = snapshot {
                        self.terminal_snapshots.insert(tab.id, snapshot);
                    }
                }
            }
        }
        cx.notify();
    }

    fn finish_tab_drag(&mut self, cx: &mut Context<Self>) {
        // A modal keeps the terminals frozen; showing them would draw the
        // native surfaces above it.
        if self.modal_open() {
            return;
        }
        self.tab_dragging = false;
        self.release_snapshots(cx);
        for pane in self.panes.values() {
            for (index, tab) in pane.tabs.iter().enumerate() {
                tab.set_visible(index == pane.active, cx);
            }
        }
    }

    /// Whether the tab at `index` of a pane is on screen.
    fn tab_is_shown(&self, pane_id: PaneId, pane_active: usize, index: usize) -> bool {
        terminal_lifecycle::is_shown(
            index,
            pane_active,
            pane_id,
            self.active_pane,
            self.focus_mode,
        )
    }

    /// Show the terminals that are on screen, hide the rest, and note when each tab
    /// went off screen. Returns whether any tab changed between shown and hidden.
    fn sync_tab_visibility(&mut self, cx: &mut Context<Self>) -> bool {
        let now = Instant::now();
        let mut changed = false;
        for (pane_id, pane) in &mut self.panes {
            for (index, tab) in pane.tabs.iter_mut().enumerate() {
                let shown = terminal_lifecycle::is_shown(
                    index,
                    pane.active,
                    *pane_id,
                    self.active_pane,
                    self.focus_mode,
                );
                tab.set_visible(shown && !self.tab_dragging, cx);
                if shown {
                    if tab.hidden_since.take().is_some() {
                        changed = true;
                        // Bringing a tab back is asking for its terminal again, by
                        // whatever path it came back, so an old failure is forgotten.
                        if let TabContent::Shell {
                            attach_error,
                            attach_failures,
                            ..
                        } = &mut tab.content
                        {
                            *attach_error = None;
                            *attach_failures = 0;
                        }
                    }
                } else if tab.hidden_since.is_none() {
                    tab.hidden_since = Some(now);
                    changed = true;
                }
            }
        }
        changed
    }

    /// Give a shell tab that has no terminal a new one attached to its session.
    /// Returns whether it has one afterwards. A failure is remembered on the tab
    /// so it is not retried on every frame.
    fn attach_terminal(
        &mut self,
        pane_id: PaneId,
        tab_id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(shell_id) = self
            .panes
            .get(&pane_id)
            .and_then(|pane| pane.tabs.iter().find(|tab| tab.id == tab_id))
            .and_then(|tab| match &tab.content {
                TabContent::Shell {
                    shell_id,
                    terminal: None,
                    attach_error: None,
                    ..
                } => Some(shell_id.clone()),
                _ => None,
            })
        else {
            return false;
        };
        let result = match self.shells.iter().find(|shell| shell.id == shell_id) {
            Some(shell) => Ok(shell.clone()),
            None => self.sessions.get(&shell_id),
        }
        .and_then(|shell| self.spawn_terminal(&shell, false, window, cx));
        let Some(TabContent::Shell {
            terminal,
            attach_error,
            attach_failures,
            ..
        }) = self
            .panes
            .get_mut(&pane_id)
            .and_then(|pane| pane.tabs.iter_mut().find(|tab| tab.id == tab_id))
            .map(|tab| &mut tab.content)
        else {
            return false;
        };
        match result {
            Ok(spawned) => {
                *terminal = Some(spawned);
                *attach_error = None;
                *attach_failures = 0;
                true
            }
            Err(error) => {
                *attach_error = Some(error.clone());
                *attach_failures = attach_failures.saturating_add(1);
                let retry = *attach_failures < ATTACH_RETRIES;
                self.notice = Some(error);
                if retry {
                    self.schedule_attach_retry(cx);
                }
                false
            }
        }
    }

    /// A tab that failed to attach, say because tmux was busy while many windows
    /// opened at once, is tried again shortly, a few times, without any click.
    fn schedule_attach_retry(&mut self, cx: &mut Context<Self>) {
        if self.attach_retry.is_some() {
            return;
        }
        self.attach_retry = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(ATTACH_RETRY_DELAY).await;
            let _ = this.update(cx, |workspace, cx| {
                workspace.attach_retry = None;
                for pane in workspace.panes.values_mut() {
                    for tab in &mut pane.tabs {
                        if let TabContent::Shell {
                            attach_error,
                            attach_failures,
                            ..
                        } = &mut tab.content
                            && *attach_failures < ATTACH_RETRIES
                        {
                            *attach_error = None;
                        }
                    }
                }
                cx.notify();
            });
        }));
    }

    /// Attach a terminal to every tab that is on screen without one: tabs restored
    /// with their window, and tabs coming back from release.
    fn attach_shown_terminals(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let missing = self
            .panes
            .iter()
            .flat_map(|(pane_id, pane)| {
                pane.tabs
                    .iter()
                    .enumerate()
                    .filter(|(index, tab)| {
                        self.tab_is_shown(*pane_id, pane.active, *index)
                            && matches!(
                                tab.content,
                                TabContent::Shell {
                                    terminal: None,
                                    attach_error: None,
                                    ..
                                }
                            )
                    })
                    .map(|(_, tab)| (*pane_id, tab.id))
            })
            .collect::<Vec<_>>();
        for (pane_id, tab_id) in missing {
            self.attach_terminal(pane_id, tab_id, window, cx);
        }
    }

    /// Which hidden terminals to release now and when to look again.
    fn hidden_terminal_plan(&self) -> terminal_lifecycle::Plan {
        let now = Instant::now();
        let panes = self
            .panes
            .iter()
            .map(|(id, pane)| terminal_lifecycle::PaneState {
                id: *id,
                active: pane.active,
                tabs: pane
                    .tabs
                    .iter()
                    .map(|tab| terminal_lifecycle::TabState {
                        id: tab.id,
                        attached: tab.terminal().is_some(),
                        // A session that has ended, or one this window has not
                        // heard of yet, keeps its terminal: attaching again could
                        // not bring its last screen back.
                        pinned: !tab.shell_id().is_some_and(|id| {
                            self.shells
                                .iter()
                                .any(|shell| shell.id == id && shell.alive)
                        }),
                        hidden_for: tab
                            .hidden_since
                            .map_or(Duration::ZERO, |since| now.saturating_duration_since(since)),
                    })
                    .collect(),
            })
            .collect::<Vec<_>>();
        terminal_lifecycle::plan(&terminal_lifecycle::Screen {
            panes: &panes,
            active_pane: self.active_pane,
            focus_mode: self.focus_mode,
            // Dragging a tab, the pane menu and modals all run `begin_tab_drag`, which
            // freezes the shown terminals as snapshots until they end.
            frozen: self.tab_dragging || self.modal_open(),
        })
    }

    /// Drop the terminals the policy gives up, which detaches their tmux clients and
    /// frees their surfaces, then schedule the next look.
    fn release_hidden_terminals(&mut self, cx: &mut Context<Self>) {
        let plan = self.hidden_terminal_plan();
        for tab_id in plan.release {
            for pane in self.panes.values_mut() {
                for tab in pane.tabs.iter_mut().filter(|tab| tab.id == tab_id) {
                    tab.release_terminal(cx);
                }
            }
        }
        self.schedule_terminal_release(plan.recheck, cx);
    }

    /// Replaces any pending pass, so a burst of tab switches leaves a single timer.
    fn schedule_terminal_release(&mut self, after: Option<Duration>, cx: &mut Context<Self>) {
        self.terminal_release = after.map(|after| {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(after).await;
                let _ = this.update(cx, |workspace, cx| workspace.release_hidden_terminals(cx));
            })
        });
    }

    fn resize_at(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(resize) = &self.resizing else {
            return;
        };
        let ratio = if resize.axis == Axis::SideBySide {
            (position.x - resize.bounds.origin.x).as_f32()
                / resize.bounds.size.width.as_f32().max(1.0)
        } else {
            (position.y - resize.bounds.origin.y).as_f32()
                / resize.bounds.size.height.as_f32().max(1.0)
        };
        if self.layout.ratio_at(&resize.path).is_some()
            && self.layout.set_ratio(&resize.path, ratio)
        {
            cx.notify();
        }
    }

    fn end_resize(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Every left mouse-up lands here; only a divider drag changes the layout.
        if self.resizing.is_none() {
            return;
        }
        self.resize_at(window.mouse_position(), cx);
        self.resizing = None;
        self.save_layout();
    }

    fn load_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.session_refresh_generation = self.session_refresh_generation.wrapping_add(1);
        self.project_sort_menu_open = false;
        if self.focus_mode {
            self.set_focus_mode(false, window, cx);
        }
        self.layout_ready = false;
        // A new project's saved ratios are drawn as they are.
        self.pane_area = None;
        for pane in self.panes.values() {
            for tab in &pane.tabs {
                tab.set_visible(false, cx);
            }
        }
        self.layout = Layout::Pane(1);
        self.panes.clear();
        self.tab_dragging = false;
        self.release_snapshots(cx);
        self.drop_target = None;
        self.resizing = None;
        self.panel_menu = None;
        self.panes.insert(
            1,
            Pane {
                tabs: Vec::new(),
                active: 0,
            },
        );
        self.active_pane = 1;
        self.focus.focus(window, cx);
        let shells = match self.sessions.list() {
            Ok(shells) => shells,
            Err(error) => {
                self.notice = Some(error);
                cx.notify();
                return;
            }
        };
        // A layout that cannot be read (corrupt, or saved by a newer build) must not
        // leave the window unusable. Open a default layout and say why; the store keeps
        // the unreadable data as it is.
        let mut layout_notice = None;
        let saved_layout = if let Some(layout) = self.restore_layout.clone() {
            Ok(Some(layout))
        } else {
            self.layouts.load(&self.project_id)
        };
        let mut saved = match saved_layout {
            Ok(saved) => saved,
            Err(error) => {
                layout_notice = Some(format!("{error}; using a default layout"));
                None
            }
        };
        let destination_missing = saved.is_none();
        if let Some(previous) = &self.carry_layout {
            let destination = saved.clone().unwrap_or_else(|| ProjectLayout {
                layout: Layout::Pane(1),
                panes: BTreeMap::from([(1, SavedPane::default())]),
                active_pane: 1,
                panels_initialized: true,
                detached_shell_ids: HashSet::new(),
                selected_worktree_id: self.selected_worktree_id.clone(),
                selected_task_id: None,
                sidebar_visible: true,
                window_size: None,
                locked_panes: None,
            });
            match destination.carry_locked_regions_from(previous) {
                Ok(layout) => saved = Some(layout),
                Err(error) => {
                    layout_notice = Some(format!("{error}; locked regions were not carried over"));
                }
            }
        }
        let locked_ids = saved
            .as_ref()
            .map(ProjectLayout::effective_locked_panes)
            .unwrap_or_default();
        let locked_shell_ids: HashSet<_> = saved
            .as_ref()
            .into_iter()
            .flat_map(|layout| locked_ids.iter().filter_map(|id| layout.panes.get(id)))
            .flat_map(|pane| {
                pane.tabs.iter().filter_map(|tab| match tab {
                    SavedTab::Shell { shell_id } => Some(shell_id.clone()),
                    _ => None,
                })
            })
            .collect();
        self.locked_panes = saved
            .as_ref()
            .and_then(|layout| layout.locked_panes.clone());
        // The window keeps a size the user gave it when the project changes. The frame it
        // opened with (clamped to the display) and zoomed or full-screen frames never
        // become a project's remembered size.
        self.window_size = saved.as_ref().and_then(|saved| saved.window_size);
        if self.settings.remember_window_size {
            self.window_size = self.window_size_user.or(self.window_size);
        }
        let live_shells = shells
            .iter()
            .filter(|shell| {
                (session_belongs_to_workspace(shell, &self.project_id)
                    || locked_shell_ids.contains(&shell.id))
                    && shell.alive
            })
            .map(|shell| (shell.id.clone(), shell.clone()))
            .collect::<BTreeMap<_, _>>();
        self.layout = saved
            .as_ref()
            .map(|saved| saved.layout.clone())
            .unwrap_or(Layout::Pane(1));
        self.panes.clear();
        let pane_ids = self.layout.pane_ids();
        for pane_id in &pane_ids {
            self.panes.insert(
                *pane_id,
                Pane {
                    tabs: Vec::new(),
                    active: 0,
                },
            );
        }
        self.next_pane_id = pane_ids.iter().copied().max().unwrap_or(1) + 1;
        let active_pane = saved
            .as_ref()
            .map(|saved| saved.active_pane)
            .filter(|id| self.panes.contains_key(id))
            .unwrap_or_else(|| self.layout.first_pane());
        self.active_pane = active_pane;
        let shell_pane = (!locked_ids.contains(&active_pane))
            .then_some(active_pane)
            .or_else(|| pane_ids.iter().copied().find(|id| !locked_ids.contains(id)));
        self.detached_shell_ids = saved
            .as_ref()
            .map(|saved| saved.detached_shell_ids.clone())
            .unwrap_or_default();
        self.detached_shell_ids
            .retain(|id| live_shells.contains_key(id));
        self.sidebar_visible = true;
        if let Some(saved) = &saved {
            self.sidebar_visible = saved.sidebar_visible;
            if let Some(worktree_id) = &saved.selected_worktree_id {
                if self
                    .state
                    .worktrees
                    .iter()
                    .find(|worktree| &worktree.id == worktree_id)
                    .is_none_or(|worktree| worktree.project_id == self.project_id)
                {
                    self.selected_worktree_id = Some(worktree_id.clone());
                }
            }
            self.selected_task_id = saved.selected_task_id.clone().filter(|id| {
                self.state
                    .tasks
                    .iter()
                    .any(|task| &task.id == id && task.project_id == self.project_id)
            });
        }
        let mut known_shell_ids = self.detached_shell_ids.clone();
        let mut restore_error = None;
        if let Some(saved) = &saved {
            for pane_id in &pane_ids {
                if let Some(pane) = saved.panes.get(pane_id) {
                    for tab in &pane.tabs {
                        match tab {
                            SavedTab::Shell { shell_id } => {
                                known_shell_ids.insert(shell_id.clone());
                                if let Some(shell) = live_shells.get(shell_id) {
                                    self.restore_shell_tab(*pane_id, shell.clone());
                                }
                            }
                            SavedTab::Panel { panel } => self.attach_panel(*pane_id, *panel, cx),
                        }
                    }
                }
            }
        }
        // Shells created through the CLI since the last save join the active pane.
        for shell in &shells {
            if shell.kind == ShellKind::Project
                && live_shells.contains_key(&shell.id)
                && !known_shell_ids.contains(&shell.id)
            {
                if let Some(shell_pane) = shell_pane {
                    self.restore_shell_tab(shell_pane, shell.clone());
                }
            }
        }
        if destination_missing
            && !live_shells.values().any(|shell| {
                shell.kind == ShellKind::Project
                    && session_belongs_to_workspace(shell, &self.project_id)
            })
        {
            if let Some(shell_pane) = shell_pane {
                if let Err(error) = self.spawn_tab(shell_pane, window, cx) {
                    restore_error = Some(error);
                }
            }
        }
        for (pane_id, pane) in &mut self.panes {
            let selected = saved
                .as_ref()
                .and_then(|saved| saved.panes.get(pane_id))
                .and_then(|pane| pane.active_tab_key.as_ref());
            if let Some(index) =
                selected.and_then(|key| pane.tabs.iter().position(|tab| &tab.saved().key() == key))
            {
                pane.active = index;
            }
            for (index, tab) in pane.tabs.iter().enumerate() {
                tab.set_visible(index == pane.active, cx);
            }
        }
        self.active_pane = active_pane;
        if saved
            .as_ref()
            .map(|saved| !saved.panels_initialized && saved.sidebar_visible)
            .unwrap_or(true)
        {
            self.add_navigation_pane(cx);
            self.active_pane = active_pane;
        }
        self.shells = self.sessions.list().unwrap_or(shells);
        self.focus_active(window, cx);
        if let Some(error) = restore_error {
            // Preserve the saved UUID positions and retry on the next refresh.
            self.notice = Some(error);
        } else {
            self.layout_ready = true;
            self.restore_layout = None;
            self.carry_layout = None;
            if let Some((mode, centered)) = self.restore_focus.take() {
                self.focus_centered = centered;
                self.set_focus_mode(mode, window, cx);
            }
            self.notice = layout_notice;
            self.save_layout();
        }
        cx.notify();
    }

    fn ensure_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.layout_ready {
            self.load_project(window, cx);
        }
        self.layout_ready
    }

    fn layout_snapshot(&self) -> Option<ProjectLayout> {
        if !self.layout_ready {
            return None;
        }
        Some(ProjectLayout {
            layout: self.layout.clone(),
            panes: self
                .panes
                .iter()
                .map(|(id, pane)| {
                    (
                        *id,
                        SavedPane {
                            shell_ids: pane
                                .tabs
                                .iter()
                                .filter_map(|tab| tab.shell_id().map(str::to_owned))
                                .collect(),
                            active_shell_id: pane
                                .tabs
                                .get(pane.active)
                                .and_then(|tab| tab.shell_id().map(str::to_owned)),
                            tabs: pane.tabs.iter().map(Tab::saved).collect(),
                            active_tab_key: pane.tabs.get(pane.active).map(|tab| tab.saved().key()),
                            tab_edge: TabEdge::Top,
                        },
                    )
                })
                .collect(),
            active_pane: self.active_pane,
            detached_shell_ids: self.detached_shell_ids.clone(),
            selected_worktree_id: self.selected_worktree_id.clone(),
            selected_task_id: self.selected_task_id.clone(),
            sidebar_visible: self.sidebar_visible,
            panels_initialized: true,
            window_size: self.window_size,
            locked_panes: self.locked_panes.clone(),
        })
    }

    fn save_layout(&mut self) {
        if let Some(size) = self.window_size_pending.take() {
            self.window_size = Some(size);
            self.window_size_user = Some(size);
        }
        let Some(saved) = self.layout_snapshot() else {
            return;
        };
        if let Err(error) = self.layouts.save(&self.project_id, &saved) {
            self.notice = Some(error);
        }
    }

    /// Tells the Dock menu which project and branch this window shows. Cheap when
    /// nothing changed, so it can follow every refresh.
    fn announce_to_dock(&self, window: &Window, cx: &mut App) {
        let branch =
            self.selected_worktree_id
                .as_ref()
                .and_then(|id| {
                    self.state.worktrees.iter().find(|worktree| {
                        worktree.id == *id && worktree.project_id == self.project_id
                    })
                })
                .map(|worktree| worktree.branch.clone());
        let project = self
            .state
            .project(&self.project_id)
            .map(|project| project.name.clone())
            .unwrap_or_default();
        dock_menu::update_window(
            dock_menu::DockWindow {
                id: window.window_handle().window_id().as_u64(),
                project,
                branch,
            },
            cx,
        );
    }

    fn runtime_window(&self, window: &Window) -> runtime::RuntimeWindow {
        // GPUI reports a zoomed macOS window as Windowed. A zoomed window is restored
        // from its last ordinary frame so that un-zooming still has somewhere to go.
        let (mode, bounds) = match window.window_bounds() {
            WindowBounds::Fullscreen(bounds) => (runtime::WindowMode::Fullscreen, bounds),
            WindowBounds::Maximized(bounds) => (runtime::WindowMode::Maximized, bounds),
            WindowBounds::Windowed(bounds) if window.is_maximized() => (
                runtime::WindowMode::Maximized,
                self.normal_bounds.unwrap_or(bounds),
            ),
            WindowBounds::Windowed(bounds) => (runtime::WindowMode::Windowed, bounds),
        };
        runtime::RuntimeWindow {
            project_id: Some(self.project_id.clone()),
            path: self.cwd.clone(),
            bounds: Some(runtime::WindowGeometry {
                x: bounds.origin.x.as_f32(),
                y: bounds.origin.y.as_f32(),
                width: bounds.size.width.as_f32(),
                height: bounds.size.height.as_f32(),
            }),
            mode,
            layout: self.layout_snapshot(),
            focus_mode: self.focus_mode,
            focus_centered: self.focus_centered,
        }
    }

    fn activate_project(
        &mut self,
        project_id: &str,
        persist: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let project = if persist {
            match self.store.use_project(project_id) {
                Ok(project) => {
                    if let Ok(state) = self.store.snapshot() {
                        self.state = state;
                    }
                    project
                }
                Err(error) => {
                    self.notice = Some(error);
                    cx.notify();
                    return;
                }
            }
        } else {
            match self.state.project(project_id) {
                Ok(project) => project.clone(),
                Err(error) => {
                    self.notice = Some(error);
                    cx.notify();
                    return;
                }
            }
        };
        if self.project_id == project.id {
            return;
        }
        tooltip::hide(cx);
        self.save_layout();
        if let Some(layout) = self.layout_snapshot() {
            self.carry_layout = Some(layout);
        }
        self.restore_layout = None;
        self.project_id = project.id;
        self.project_settings_panel = None;
        self.schedule_panel = None;
        self.file_explorer = None;
        window.set_window_title(&format!("RiWork · {}", project.name));
        self.cwd = project.root;
        self.selected_worktree_id = self
            .state
            .worktrees_for(&self.project_id)
            .into_iter()
            .find(|worktree| worktree.is_primary)
            .map(|worktree| worktree.id.clone());
        self.selected_task_id = None;
        self.search_focused = false;
        self.search.clear();
        self.search_marked = None;
        self.load_project(window, cx);
        self.announce_to_dock(window, cx);
    }

    fn select_worktree(&mut self, worktree_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(worktree) = self
            .state
            .worktrees
            .iter()
            .find(|worktree| worktree.id == worktree_id)
            .cloned()
        else {
            return;
        };
        if self.project_id != worktree.project_id {
            self.activate_project(&worktree.project_id, true, window, cx);
        }
        if !self.ensure_layout(window, cx) {
            return;
        }
        self.selected_worktree_id = Some(worktree.id.clone());
        let existing = self.panes.iter().find_map(|(pane_id, pane)| {
            pane.tabs
                .iter()
                .find(|tab| matches!(&tab.content, TabContent::Shell { worktree_id, .. } if worktree_id.as_deref() == Some(worktree.id.as_str())))
                .map(|tab| (*pane_id, tab.id))
        });
        if let Some((pane_id, tab_id)) = existing {
            self.select_tab(pane_id, tab_id, window, cx);
        } else if let Some(shell_id) = self
            .shells
            .iter()
            .rev()
            .find(|shell| {
                shell.alive
                    && shell.project_id.as_deref() == Some(self.project_id.as_str())
                    && shell.worktree_id.as_deref() == Some(worktree.id.as_str())
            })
            .map(|shell| shell.id.clone())
        {
            self.show_shell(&shell_id, window, cx);
        } else {
            self.add_tab(window, cx);
        }
        self.save_layout();
        cx.notify();
    }

    fn select_task(&mut self, task_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task) = self
            .state
            .tasks
            .iter()
            .find(|task| task.id == task_id)
            .cloned()
        else {
            return;
        };
        if self.project_id != task.project_id {
            self.activate_project(&task.project_id, true, window, cx);
        }
        if !self.ensure_layout(window, cx) {
            return;
        }
        self.selected_worktree_id = task
            .worktree_id
            .clone()
            .or(self.selected_worktree_id.clone());
        self.selected_task_id = Some(task.id);
        self.sync_file_explorer(cx);
        self.save_layout();
        cx.notify();
    }

    fn show_shell(&mut self, shell_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.ensure_layout(window, cx) {
            return;
        }
        let existing = self.panes.iter().find_map(|(pane_id, pane)| {
            pane.tabs
                .iter()
                .find(|tab| tab.shell_id() == Some(shell_id))
                .map(|tab| (*pane_id, tab.id))
        });
        if let Some((pane_id, tab_id)) = existing {
            self.select_tab(pane_id, tab_id, window, cx);
            return;
        }
        let Some(shell) = self
            .shells
            .iter()
            .find(|shell| shell.id == shell_id)
            .cloned()
        else {
            return;
        };
        if !shell.alive {
            self.notice = Some(format!("Shell {} has exited", &shell.id[..8]));
            cx.notify();
            return;
        }
        if shell.project_id.as_deref() != Some(self.project_id.as_str()) {
            if let Some(project_id) = &shell.project_id {
                self.activate_project(project_id, true, window, cx);
            }
        }
        if !self.ensure_layout(window, cx) {
            return;
        }
        // Loading another project may already have restored this shell.
        if let Some((pane_id, tab_id)) = self.panes.iter().find_map(|(pane_id, pane)| {
            pane.tabs
                .iter()
                .find(|tab| tab.shell_id() == Some(shell_id))
                .map(|tab| (*pane_id, tab.id))
        }) {
            self.select_tab(pane_id, tab_id, window, cx);
            return;
        }
        self.detached_shell_ids.remove(shell_id);
        if let Err(error) = self.attach_session(self.active_pane, shell, window, cx) {
            self.notice = Some(error);
        }
        self.save_layout();
        cx.notify();
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Ok(settings) = self.settings_store.load() {
            if &settings != cx.global::<Settings>() {
                cx.set_global(settings);
            }
        }
        if &self.settings != cx.global::<Settings>() {
            self.apply_settings(window, cx);
        }
        self.refresh_count += 1;
        if self.refresh_count % 5 == 0 && self.syncing_project_ids.insert(self.project_id.clone()) {
            let project_id = self.project_id.clone();
            let work_project_id = project_id.clone();
            let store = self.store.clone();
            // Shared with the other windows: it does nothing when one of them
            // just synced this project or nothing on disk changed since.
            let work = cx
                .background_executor()
                .spawn(async move { store.refresh_worktrees(&work_project_id) });
            cx.spawn(async move |this, cx| {
                let result = work.await;
                let _ = this.update(cx, |workspace, cx| {
                    workspace.syncing_project_ids.remove(&project_id);
                    if workspace.project_id == project_id {
                        if let Ok(state) = workspace.store.snapshot() {
                            workspace.state = state;
                        }
                        if let Err(error) = result {
                            workspace.notice = Some(error);
                        }
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        match self.store.snapshot() {
            Ok(state) => self.state = state,
            Err(error) => self.notice = Some(error),
        }
        self.refresh_project_recency(cx);
        if let Ok(project) = self.state.project(&self.project_id) {
            window.set_window_title(&format!("RiWork · {}", project.name));
        }
        self.announce_to_dock(window, cx);
        if let Some(panel) = &self.project_settings_panel {
            panel.update(cx, |panel, cx| panel.refresh_folders(cx));
        }
        if !self.layout_ready {
            self.load_project(window, cx);
        }
        self.refresh_sessions(cx);
        request_codex_usage(false, cx);
        self.remember_active_worktree(cx);
        let files_visible = self.panes.values().any(|pane| {
            pane.tabs
                .get(pane.active)
                .is_some_and(|tab| matches!(tab.content, TabContent::Panel(PanelKind::Files)))
        });
        if files_visible {
            if let Some(panel) = &self.file_explorer {
                panel.update(cx, |panel, cx| panel.refresh(cx));
            }
        }
        self.refresh_shell_titles();
        cx.notify();
    }

    fn refresh_sessions(&mut self, cx: &mut Context<Self>) {
        if self.session_refresh_pending {
            return;
        }
        self.session_refresh_pending = true;
        let project_id = self.project_id.clone();
        let generation = self.session_refresh_generation;
        let sessions = self.sessions.clone();
        let work_project_id = project_id.clone();
        let want_metrics = self.metrics_visible();
        let work = cx.background_executor().spawn(async move {
            // One tmux and `ps` sample serves every window for about a tick.
            let sample = sessions.sample(want_metrics)?;
            let shells = sample.shells;
            let metrics = sample.metrics;
            let cwds = sample.directories;
            let mut claude_usage = BTreeMap::new();
            for shell in &shells {
                if shell.harness == Some(HarnessKind::Claude)
                    && shell.project_id.as_deref() == Some(work_project_id.as_str())
                {
                    if let Ok(Some(snapshot)) =
                        usage::read_claude_usage_at(sessions.state_home(), &shell.id)
                    {
                        claude_usage.insert(shell.id.clone(), snapshot);
                    }
                }
            }
            Ok::<_, String>((shells, metrics, cwds, claude_usage))
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |workspace, cx| {
                workspace.session_refresh_pending = false;
                // Project switches and local launches invalidate an older snapshot,
                // including a switch away from and back to the same project.
                if workspace.project_id != project_id
                    || workspace.session_refresh_generation != generation
                {
                    return;
                }
                let Ok((shells, metrics, cwds, claude_usage)) = result else {
                    return;
                };
                workspace.shells = shells;
                if let Some(metrics) = metrics {
                    workspace.metrics = metrics;
                }
                if let Some(cwds) = cwds {
                    workspace.shell_cwds = cwds;
                }
                workspace.claude_usage = claude_usage;
                for home in workspace
                    .shells
                    .iter()
                    .filter(|shell| shell.alive && shell.harness == Some(HarnessKind::Codex))
                    .filter_map(|shell| shell.codex_home.clone())
                    .collect::<HashSet<_>>()
                {
                    request_codex_usage_at(home, false, cx);
                }
                workspace.refresh_grok_usage(false, cx);
                workspace.refresh_agent_activity(cx);
                workspace.remember_active_worktree(cx);
                workspace.refresh_shell_titles();
                cx.notify();
            });
        })
        .detach();
    }

    /// The bottom bar's usage item is drawn.
    fn usage_chip_visible(&self) -> bool {
        use status_bar::{StatusItemKind, StatusSide};
        !self.focus_mode
            && [StatusSide::Left, StatusSide::Right]
                .into_iter()
                .any(|side| {
                    self.settings
                        .status_bar
                        .visible_items(side)
                        .contains(&StatusItemKind::Usage)
                })
    }

    fn usage_panel_visible(&self) -> bool {
        self.panes.values().any(|pane| {
            pane.tabs
                .get(pane.active)
                .is_some_and(|tab| matches!(tab.content, TabContent::Panel(PanelKind::Usage)))
        })
    }

    fn active_shell(&self) -> Option<&ShellSession> {
        self.panes
            .get(&self.active_pane)
            .and_then(|pane| pane.tabs.get(pane.active))
            .and_then(Tab::shell_id)
            .and_then(|id| self.shells.iter().find(|shell| shell.id == id))
    }

    /// Grok tabs whose session usage this window shows: every live one of the
    /// project while the Usage panel is on screen, and the focused one while the
    /// bottom bar's usage item is. Anything else is not worth a `grok usage`.
    fn grok_usage_targets(&self) -> Vec<String> {
        let panel = self.usage_panel_visible();
        let focused = self
            .usage_chip_visible()
            .then(|| self.active_shell())
            .flatten()
            .map(|shell| shell.id.as_str());
        self.shells
            .iter()
            .filter(|shell| shell.alive && shell.harness == Some(HarnessKind::Grok))
            .filter(|shell| {
                (panel && shell.project_id.as_deref() == Some(self.project_id.as_str()))
                    || focused == Some(shell.id.as_str())
            })
            .map(|shell| shell.id.clone())
            .collect()
    }

    /// Read Grok's per-session usage on a worker for the tabs shown. Grok has no
    /// interface for its account allowance, so this is tokens and cost only. Each
    /// tick asks whether a tab's figures are due (about every 30 s, sooner while
    /// a tab has none); the reads themselves are also cached across windows, so
    /// this never starts `grok` every tick. `force` is the Refresh action.
    fn refresh_grok_usage(&mut self, force: bool, cx: &mut Context<Self>) {
        if self.grok_usage_pending {
            return;
        }
        let shells = &self.shells;
        self.grok_usage
            .retain(|id, _| shells.iter().any(|shell| shell.id == *id && shell.alive));
        let targets = self.grok_usage_targets();
        let now = unix_time();
        let due = force
            || targets
                .iter()
                .any(|id| self.grok_usage.get(id).is_none_or(|tab| tab.is_due(now)));
        if targets.is_empty() || !due {
            return;
        }
        self.grok_usage_pending = true;
        let sessions = self.sessions.clone();
        let previous = self.grok_usage.clone();
        let work = cx.background_executor().spawn(async move {
            let pids = sessions.pane_pids(&targets);
            let targets = targets
                .into_iter()
                .map(|shell_id| usage::GrokTarget {
                    pane_pid: match &pids {
                        Ok(pids) => pids
                            .get(&shell_id)
                            .copied()
                            .ok_or_else(|| "tmux did not report a process for this tab".to_owned()),
                        Err(error) => Err(error.clone()),
                    },
                    shell_id,
                })
                .collect::<Vec<_>>();
            usage::read_grok_usages(&targets, &previous, force)
        });
        cx.spawn(async move |this, cx| {
            let results = work.await;
            let _ = this.update(cx, |workspace, cx| {
                workspace.grok_usage_pending = false;
                workspace.grok_usage.extend(results);
                cx.notify();
            });
        })
        .detach();
    }

    /// CPU and memory feed only the status bar's resources item and the Shells
    /// panel, and sampling them costs a `ps` run. A window showing neither keeps
    /// its last figures and picks them up again on the refresh after either
    /// appears.
    fn metrics_visible(&self) -> bool {
        use status_bar::{StatusItemKind, StatusSide};
        let status = &self.settings.status_bar;
        let in_status_bar = !self.focus_mode
            && [StatusSide::Left, StatusSide::Right]
                .into_iter()
                .any(|side| {
                    status
                        .visible_items(side)
                        .contains(&StatusItemKind::Resources)
                });
        in_status_bar
            || self.panes.values().any(|pane| {
                pane.tabs
                    .get(pane.active)
                    .is_some_and(|tab| matches!(tab.content, TabContent::Panel(PanelKind::Shells)))
            })
    }

    fn refresh_shell_titles(&mut self) {
        for pane in self.panes.values_mut() {
            for tab in &mut pane.tabs {
                if let Some(path) = tab
                    .shell_id()
                    .and_then(|id| self.shells.iter().find(|shell| shell.id == id))
                    .and_then(|shell| shell.editor_path.as_ref())
                {
                    tab.title = format!(
                        "VIM · {}",
                        path.file_name()
                            .map(|name| name.to_string_lossy())
                            .unwrap_or_default()
                    );
                    continue;
                }
                if let Some(shell) = tab
                    .shell_id()
                    .and_then(|id| self.shells.iter().find(|shell| shell.id == id))
                    .filter(|shell| shell.kind == ShellKind::Orchestrator)
                {
                    tab.title = contextual_shell_title(
                        orchestrator_tab_title(shell),
                        shell,
                        &self.project_id,
                        &self.state,
                    );
                    continue;
                }
                if let Some(cwd) = tab.shell_id().and_then(|id| self.shell_cwds.get(id)) {
                    let shell = self
                        .shells
                        .iter()
                        .find(|shell| tab.shell_id() == Some(&shell.id));
                    let project_id = shell
                        .and_then(|shell| shell.project_id.as_deref())
                        .unwrap_or(&self.project_id);
                    let worktree = self
                        .state
                        .worktrees
                        .iter()
                        .filter(|worktree| worktree.project_id == project_id)
                        .filter(|worktree| cwd.starts_with(&worktree.path))
                        .max_by_key(|worktree| worktree.path.as_os_str().len());
                    let label = worktree
                        .map(|worktree| worktree.branch.as_str())
                        .unwrap_or("outside");
                    let program = self
                        .shells
                        .iter()
                        .find(|shell| tab.shell_id() == Some(&shell.id))
                        .and_then(|shell| shell.harness)
                        .map(harness_name)
                        .unwrap_or(&self.shell_name);
                    let title = format!("{} {:02} · {}", program, tab.id, label);
                    tab.title = shell
                        .map(|shell| {
                            contextual_shell_title(
                                title.clone(),
                                shell,
                                &self.project_id,
                                &self.state,
                            )
                        })
                        .unwrap_or(title);
                }
            }
        }
    }

    fn add_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.ensure_layout(window, cx) {
            return;
        }
        if let Err(error) = self.spawn_tab(self.active_pane, window, cx) {
            self.notice = Some(error);
            cx.notify();
        }
        self.save_layout();
    }

    fn add_split(&mut self, axis: Axis, window: &mut Window, cx: &mut Context<Self>) {
        if self.focus_mode {
            self.set_focus_mode(false, window, cx);
        }
        if !self.ensure_layout(window, cx) {
            return;
        }
        let target = self.active_pane;
        let new_pane = self.next_pane_id;
        self.next_pane_id += 1;
        self.panes.insert(
            new_pane,
            Pane {
                tabs: Vec::new(),
                active: 0,
            },
        );
        if let Err(error) = self.spawn_tab(new_pane, window, cx) {
            self.panes.remove(&new_pane);
            self.notice = Some(error);
            cx.notify();
            return;
        }
        self.layout.split(target, axis, new_pane);
        self.active_pane = new_pane;
        self.save_layout();
        cx.notify();
    }

    fn focus_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.remember_active_worktree(cx);
        let files_active = self
            .panes
            .get(&self.active_pane)
            .and_then(|pane| pane.tabs.get(pane.active))
            .is_some_and(|tab| matches!(tab.content, TabContent::Panel(PanelKind::Files)));
        if files_active {
            self.search_focused = false;
            self.ensure_file_explorer(cx);
            if let Some(panel) = &self.file_explorer {
                panel.update(cx, |panel, cx| {
                    panel.refresh(cx);
                    panel.focus(window, cx);
                });
            }
            return;
        }
        let project_settings_active = self
            .panes
            .get(&self.active_pane)
            .and_then(|pane| pane.tabs.get(pane.active))
            .is_some_and(|tab| {
                matches!(tab.content, TabContent::Panel(PanelKind::ProjectSettings))
            });
        if project_settings_active {
            self.search_focused = false;
            self.ensure_project_settings(cx);
            if let Some(panel) = &self.project_settings_panel {
                panel.update(cx, |panel, cx| panel.focus(window, cx));
            }
            return;
        }
        let schedules_active = self
            .panes
            .get(&self.active_pane)
            .and_then(|pane| pane.tabs.get(pane.active))
            .is_some_and(|tab| matches!(tab.content, TabContent::Panel(PanelKind::Schedules)));
        if schedules_active {
            self.search_focused = false;
            if let Some(panel) = &self.schedule_panel {
                panel.update(cx, |panel, cx| panel.focus(window, cx));
            }
            return;
        }
        let settings_active = self
            .panes
            .get(&self.active_pane)
            .and_then(|pane| pane.tabs.get(pane.active))
            .is_some_and(|tab| matches!(tab.content, TabContent::Panel(PanelKind::Settings)));
        if settings_active {
            self.search_focused = false;
            self.settings_panel
                .update(cx, |panel, cx| panel.focus(window, cx));
            return;
        }
        // A tab that was released while hidden gets its terminal back here, before
        // it is asked to take the keys.
        if let Some(tab_id) = self
            .panes
            .get(&self.active_pane)
            .and_then(|pane| pane.tabs.get(pane.active))
            .map(|tab| tab.id)
        {
            self.attach_terminal(self.active_pane, tab_id, window, cx);
        }
        let terminal = self
            .panes
            .get(&self.active_pane)
            .and_then(|pane| pane.tabs.get(pane.active))
            .and_then(|tab| tab.terminal().cloned());
        if let Some(terminal) = terminal {
            // The terminal takes the keys, so a search left open must not also
            // act on them: the adapter does not stop them bubbling to us.
            self.search_focused = false;
            self.search_marked = None;
            terminal.update(cx, |terminal, cx| terminal.focus(window, cx));
        } else {
            self.focus.focus(window, cx);
        }
    }

    fn select_pane(&mut self, pane_id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        if self.panes.contains_key(&pane_id) {
            self.active_pane = pane_id;
            self.focus_active(window, cx);
            self.save_layout();
            cx.notify();
        }
    }

    fn select_tab(
        &mut self,
        pane_id: PaneId,
        tab_id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.ensure_layout(window, cx) {
            return;
        }
        let Some(pane) = self.panes.get_mut(&pane_id) else {
            return;
        };
        let Some(index) = pane.tabs.iter().position(|tab| tab.id == tab_id) else {
            return;
        };
        pane.active = index;
        for (tab_index, tab) in pane.tabs.iter().enumerate() {
            tab.set_visible(tab_index == index, cx);
        }
        // Choosing a tab that could not attach is asking to try again.
        if let TabContent::Shell {
            attach_error,
            attach_failures,
            ..
        } = &mut pane.tabs[index].content
        {
            *attach_error = None;
            *attach_failures = 0;
        }
        self.active_pane = pane_id;
        self.search_focused = false;
        self.search_marked = None;
        self.focus_active(window, cx);
        self.save_layout();
        cx.notify();
    }

    fn cycle_tab(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pane) = self.panes.get(&self.active_pane) else {
            return;
        };
        if pane.tabs.is_empty() {
            return;
        }
        let len = pane.tabs.len() as isize;
        let index = (pane.active as isize + delta).rem_euclid(len) as usize;
        let pane_id = self.active_pane;
        let tab_id = pane.tabs[index].id;
        self.select_tab(pane_id, tab_id, window, cx);
    }

    fn remove_tab(
        &mut self,
        pane_id: PaneId,
        tab_id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.ensure_layout(window, cx) {
            return;
        }
        let Some(pane) = self.panes.get_mut(&pane_id) else {
            return;
        };
        let Some(index) = pane.tabs.iter().position(|tab| tab.id == tab_id) else {
            return;
        };
        let should_focus = self.active_pane == pane_id && pane.active == index;
        let removed = pane.tabs.remove(index);
        if let Some(shell_id) = removed.shell_id() {
            self.detached_shell_ids.insert(shell_id.to_owned());
        }
        removed.set_visible(false, cx);
        if pane.tabs.is_empty() {
            self.remove_pane(pane_id, window, cx);
            return;
        }
        if index < pane.active {
            pane.active -= 1;
        } else if pane.active >= pane.tabs.len() {
            pane.active = pane.tabs.len() - 1;
        }
        for (tab_index, tab) in pane.tabs.iter().enumerate() {
            tab.set_visible(tab_index == pane.active, cx);
        }
        if should_focus {
            self.focus_active(window, cx);
        }
        self.save_layout();
        cx.notify();
    }

    fn remove_pane(&mut self, pane_id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        if !self.ensure_layout(window, cx) {
            return;
        }
        // An open pane menu keeps the terminals frozen as snapshots, and only
        // the render-time guard ends that once the menu is gone.
        self.panel_menu = None;
        self.drop_target = None;
        if self.panes.len() == 1 {
            if let Some(pane) = self.panes.get_mut(&pane_id) {
                for tab in pane.tabs.drain(..) {
                    if let Some(shell_id) = tab.shell_id() {
                        self.detached_shell_ids.insert(shell_id.to_owned());
                    }
                    tab.set_visible(false, cx);
                }
                pane.active = 0;
            }
            if self.tab_dragging {
                self.finish_tab_drag(cx);
            }
            self.focus_active(window, cx);
            self.save_layout();
            cx.notify();
            return;
        }
        if let Some(locked) = &mut self.locked_panes {
            locked.remove(&pane_id);
        }
        let was_active = self.active_pane == pane_id;
        let inheritor = pane_inheriting_space(&self.layout, pane_id);
        if let Some(pane) = self.panes.remove(&pane_id) {
            for tab in pane.tabs {
                if let Some(shell_id) = tab.shell_id() {
                    self.detached_shell_ids.insert(shell_id.to_owned());
                }
                tab.set_visible(false, cx);
            }
        }
        if self.tab_dragging {
            self.finish_tab_drag(cx);
        }
        if let Some(layout) = self.layout.clone().without(pane_id) {
            self.layout = layout;
            if was_active {
                self.active_pane = inheritor
                    .filter(|id| self.panes.contains_key(id))
                    .unwrap_or_else(|| self.layout.first_pane());
                self.focus_active(window, cx);
            }
            self.save_layout();
            cx.notify();
        }
    }

    fn pane_is_locked(&self, pane_id: PaneId) -> bool {
        pane_lock_state(
            self.locked_panes.as_ref(),
            pane_id,
            self.layout.first_pane(),
            || {
                self.panes.get(&pane_id).is_some_and(|pane| {
                    pane.tabs.iter().any(|tab| {
                        matches!(
                            tab.panel(),
                            Some(PanelKind::Projects | PanelKind::Worktrees | PanelKind::Files)
                        )
                    })
                })
            },
        )
    }

    fn toggle_pane_lock(&mut self, pane_id: PaneId, cx: &mut Context<Self>) {
        let mut locked: HashSet<_> = self
            .panes
            .keys()
            .copied()
            .filter(|id| self.pane_is_locked(*id))
            .collect();
        if !locked.remove(&pane_id) {
            locked.insert(pane_id);
        }
        self.locked_panes = Some(locked);
        // The refusal hint is stale once the pane can be closed again.
        if self.notice.as_deref().is_some_and(is_locked_close_hint) {
            self.notice = None;
        }
        self.save_layout();
        cx.notify();
    }

    /// Close a tab because the user asked to. A locked pane refuses; internal
    /// removals (moving a tab, project switches, restore) never come through here.
    fn close_tab_by_user(
        &mut self,
        pane_id: PaneId,
        tab_id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.ensure_layout(window, cx) || self.refuse_locked_close(pane_id, UserClose::Tab, cx)
        {
            return;
        }
        self.remove_tab(pane_id, tab_id, window, cx);
    }

    /// Close a pane, and with it every tab in it, because the user asked to.
    fn close_pane_by_user(&mut self, pane_id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        if !self.ensure_layout(window, cx) || self.refuse_locked_close(pane_id, UserClose::Pane, cx)
        {
            return;
        }
        self.remove_pane(pane_id, window, cx);
    }

    /// Say why nothing was closed. The tab X and the menu's Close pane are not drawn
    /// for a locked pane, so this is what a shortcut reaches.
    fn refuse_locked_close(
        &mut self,
        pane_id: PaneId,
        close: UserClose,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(hint) = user_close_refusal(self.pane_is_locked(pane_id), close) else {
            return false;
        };
        self.notice = Some(hint.to_owned());
        cx.notify();
        true
    }

    fn new_tab_action(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal_open() {
            return;
        }
        self.add_tab(window, cx);
    }

    fn new_project_window_action(
        &mut self,
        _: &NewProjectWindow,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let project_id = self.project_id.clone();
        self.open_project_window(&project_id, cx);
    }

    fn open_project_window(&mut self, id: &str, cx: &mut Context<Self>) {
        let path = match self.state.project(id) {
            Ok(project) => project.root.clone(),
            Err(error) => {
                self.notice = Some(error);
                cx.notify();
                return;
            }
        };
        if let Err(error) = open_workspace_window(Some(path), self.cwd.clone(), None, cx) {
            self.notice = Some(error);
        }
        cx.notify();
    }

    fn open_codex_action(&mut self, _: &OpenCodex, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal_open() {
            return;
        }
        self.add_harness(HarnessKind::Codex, false, window, cx);
    }

    fn open_claude_action(&mut self, _: &OpenClaude, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal_open() {
            return;
        }
        self.add_harness(HarnessKind::Claude, false, window, cx);
    }

    fn open_grok_action(&mut self, _: &OpenGrok, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal_open() {
            return;
        }
        self.add_harness(HarnessKind::Grok, false, window, cx);
    }

    fn add_harness(
        &mut self,
        harness: HarnessKind,
        unrestricted: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.ensure_layout(window, cx) {
            return;
        }
        self.panel_menu = None;
        self.finish_tab_drag(cx);
        let worktree_id = self.selected_worktree_id.clone();
        let cwd = worktree_id
            .as_ref()
            .and_then(|id| {
                self.state
                    .worktrees
                    .iter()
                    .find(|worktree| &worktree.id == id)
            })
            .map(|worktree| worktree.path.clone())
            .unwrap_or_else(|| self.cwd.clone());
        let result = self
            .sessions
            .create_harness(
                self.project_id.clone(),
                worktree_id,
                cwd,
                harness,
                unrestricted,
            )
            .and_then(|shell| self.attach_session(self.active_pane, shell, window, cx));
        if let Err(error) = result {
            self.notice = Some(error);
        }
        if let Ok(shells) = self.sessions.list() {
            self.shells = shells;
        }
        self.focus_active(window, cx);
        self.save_layout();
        request_codex_usage(false, cx);
        cx.notify();
    }

    fn toggle_panel_menu(&mut self, pane_id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        self.project_sort_menu_open = false;
        self.active_pane = pane_id;
        self.panel_menu = if self.panel_menu == Some(pane_id) {
            None
        } else {
            Some(pane_id)
        };
        if self.panel_menu.is_some() {
            self.begin_tab_drag(cx);
            self.focus.focus(window, cx);
        } else {
            self.finish_tab_drag(cx);
            self.focus_active(window, cx);
        }
        cx.notify();
    }

    fn split_right_action(&mut self, _: &SplitRight, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal_open() {
            return;
        }
        self.add_split(Axis::SideBySide, window, cx);
    }

    fn split_down_action(&mut self, _: &SplitDown, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal_open() {
            return;
        }
        self.add_split(Axis::Stacked, window, cx);
    }

    fn close_tab_action(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal_open() {
            return;
        }
        let Some(pane) = self.panes.get(&self.active_pane) else {
            return;
        };
        let Some(tab) = pane.tabs.get(pane.active) else {
            return;
        };
        self.close_tab_by_user(self.active_pane, tab.id, window, cx);
    }

    fn close_pane_action(&mut self, _: &ClosePane, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal_open() {
            return;
        }
        self.close_pane_by_user(self.active_pane, window, cx);
    }

    fn next_tab_action(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal_open() {
            return;
        }
        self.cycle_tab(1, window, cx);
    }

    fn previous_tab_action(
        &mut self,
        _: &PreviousTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal_open() {
            return;
        }
        self.cycle_tab(-1, window, cx);
    }

    fn toggle_sidebar_action(
        &mut self,
        _: &ToggleSidebar,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal_open() {
            return;
        }
        self.open_panel(PanelKind::Projects, self.active_pane, window, cx);
    }

    fn open_schedules_action(
        &mut self,
        _: &OpenSchedules,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal_open() {
            return;
        }
        self.open_panel(PanelKind::Schedules, self.active_pane, window, cx);
    }

    fn open_settings_action(
        &mut self,
        _: &OpenSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal_open() {
            return;
        }
        self.open_panel(PanelKind::Settings, self.active_pane, window, cx);
    }

    fn open_project_settings_action(
        &mut self,
        _: &OpenProjectSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal_open() {
            return;
        }
        self.open_panel(PanelKind::ProjectSettings, self.active_pane, window, cx);
    }

    fn open_files_action(&mut self, _: &OpenFiles, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal_open() {
            return;
        }
        let pane_id = self
            .panes
            .iter()
            .find_map(|(id, pane)| {
                pane.tabs
                    .iter()
                    .any(|tab| {
                        matches!(
                            tab.content,
                            TabContent::Panel(
                                PanelKind::Projects | PanelKind::Worktrees | PanelKind::Files
                            )
                        )
                    })
                    .then_some(*id)
            })
            .unwrap_or(self.active_pane);
        self.open_panel(PanelKind::Files, pane_id, window, cx);
    }

    fn focus_search_action(
        &mut self,
        _: &FocusSearch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal_open() {
            return;
        }
        let files_active = self
            .panes
            .get(&self.active_pane)
            .and_then(|pane| pane.tabs.get(pane.active))
            .is_some_and(|tab| matches!(tab.content, TabContent::Panel(PanelKind::Files)));
        if files_active {
            self.ensure_file_explorer(cx);
            if let Some(panel) = &self.file_explorer {
                panel.update(cx, |panel, cx| panel.focus_search(window, cx));
            }
            return;
        }
        let active_panel = self
            .panes
            .get(&self.active_pane)
            .and_then(|pane| pane.tabs.get(pane.active))
            .is_some_and(|tab| matches!(tab.content, TabContent::Panel(kind) if kind != PanelKind::Settings && kind != PanelKind::ProjectSettings && kind != PanelKind::Usage && kind != PanelKind::Schedules));
        if !active_panel {
            self.open_panel(PanelKind::Projects, self.active_pane, window, cx);
        }
        self.search_focused = true;
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn create_project_action(
        &mut self,
        _: &CreateProject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.begin_project_creation(window, cx);
    }

    fn begin_project_creation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.project_creator.is_some() || self.folder_editor.is_some() {
            return;
        }
        let directory = match paths::ensure_default_projects_directory() {
            Ok(directory) => directory,
            Err(error) => {
                self.notice = Some(error);
                cx.notify();
                return;
            }
        };
        self.search_focused = false;
        self.panel_menu = None;
        self.notice = None;
        self.begin_tab_drag(cx);
        let store = self.store.clone();
        let creator = cx.new(|cx| ProjectCreator::new(store, directory, window, cx));
        cx.subscribe_in(&creator, window, |workspace, _, event, window, cx| {
            workspace.project_creator = None;
            workspace.finish_tab_drag(cx);
            match event {
                ProjectCreationEvent::Created(project) => {
                    workspace.activate_project(&project.id, true, window, cx);
                    workspace.open_panel(PanelKind::Projects, workspace.active_pane, window, cx);
                }
                ProjectCreationEvent::Cancelled => workspace.focus_active(window, cx),
            }
            cx.notify();
        })
        .detach();
        self.project_creator = Some(creator);
        cx.notify();
    }

    fn open_orchestrator_action(
        &mut self,
        _: &OpenOrchestrator,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_orchestrator(window, cx);
    }

    fn open_orchestrator(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_scoped_orchestrator(None, window, cx);
    }

    fn open_project_orchestrator_action(
        &mut self,
        _: &OpenProjectOrchestrator,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_scoped_orchestrator(Some(self.project_id.clone()), window, cx);
    }

    fn open_scoped_orchestrator(
        &mut self,
        project_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal_open() || !self.ensure_layout(window, cx) {
            return;
        }
        self.panel_menu = None;
        self.finish_tab_drag(cx);
        let session = match project_id.as_deref() {
            Some(id) => match self.state.project(id) {
                Ok(project) => self.sessions.orchestrator_create_for_project(
                    project.id.clone(),
                    project.root.clone(),
                    None,
                ),
                Err(error) => Err(error),
            },
            None => self.sessions.orchestrator_create(self.cwd.clone(), None),
        };
        let shell = match session {
            Ok(shell) => shell,
            Err(error) => {
                self.notice = Some(error);
                cx.notify();
                return;
            }
        };
        // A scope has one persistent session. Reopen its existing tab instead
        // of creating another terminal view or a separate window.
        let existing = self.panes.iter().find_map(|(pane_id, pane)| {
            pane.tabs
                .iter()
                .find(|tab| tab.shell_id() == Some(shell.id.as_str()))
                .map(|tab| (*pane_id, tab.id))
        });
        if let Ok(shells) = self.sessions.list() {
            self.shells = shells;
        }
        self.detached_shell_ids.remove(&shell.id);
        if let Some((pane_id, tab_id)) = existing {
            self.select_tab(pane_id, tab_id, window, cx);
        } else if let Err(error) = self.attach_session(self.active_pane, shell, window, cx) {
            self.notice = Some(error);
        } else {
            self.focus_active(window, cx);
        }
        self.save_layout();
        cx.notify();
    }

    fn set_focus_mode(&mut self, enabled: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_mode = enabled;
        self.panel_menu = None;
        self.resizing = None;
        self.drop_target = None;
        self.finish_tab_drag(cx);
        // Keep native window geometry and the saved split tree unchanged.
        self.focus_active(window, cx);
        cx.notify();
    }

    fn toggle_focus_mode_action(
        &mut self,
        _: &ToggleFocusMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal_open() {
            return;
        }
        self.set_focus_mode(!self.focus_mode, window, cx);
    }

    fn replace_search_text(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
    ) -> Range<usize> {
        let marked = self.search_marked.take();
        let range = if let Some(range) = range_utf16 {
            utf16_to_byte(&self.search, range.start)..utf16_to_byte(&self.search, range.end)
        } else {
            marked.unwrap_or(self.search.len()..self.search.len())
        };
        let text = text.replace('\n', " ").replace('\r', " ");
        self.search.replace_range(range.clone(), &text);
        range.start..range.start + text.len()
    }

    fn search_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.key == "escape" && self.project_sort_menu_open {
            self.project_sort_menu_open = false;
            self.focus_active(window, cx);
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if event.keystroke.key == "escape" && cx.has_active_drag() {
            cx.stop_active_drag(window);
            self.finish_tab_drag(cx);
            self.drop_target = None;
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if event.keystroke.key == "escape" && self.panel_menu.is_some() {
            self.panel_menu = None;
            self.finish_tab_drag(cx);
            self.focus_active(window, cx);
            cx.stop_propagation();
            cx.notify();
            return;
        }
        // Keys reach this handler from every focused descendant, including
        // terminals, so the search only owns them while the workspace itself
        // holds focus.
        if !self.search_focused || !self.focus.is_focused(window) {
            return;
        }
        let handled = match event.keystroke.key.as_str() {
            "escape" => {
                self.search_focused = false;
                self.search_marked = None;
                self.focus_active(window, cx);
                true
            }
            "backspace" => {
                self.search.pop();
                self.search_marked = None;
                true
            }
            "enter" | "return" => {
                if let Some(hit) = self.state.search(&self.search).into_iter().next() {
                    match hit {
                        SearchHit::Project(project) => {
                            self.activate_project(&project.id, true, window, cx)
                        }
                        SearchHit::Worktree(worktree) => {
                            self.select_worktree(&worktree.id, window, cx)
                        }
                        SearchHit::Task(task) => self.select_task(&task.id, window, cx),
                    }
                }
                self.search_focused = false;
                self.search_marked = None;
                true
            }
            "a" if event.keystroke.modifiers.platform => {
                self.search.clear();
                self.search_marked = None;
                true
            }
            _ => false,
        };
        if handled {
            cx.stop_propagation();
            cx.notify();
        }
    }

    /// Keeps locked panes at their pixel size when the area the panes fill changes: a
    /// window resize, full screen, or the status bar being switched on or off.
    fn follow_pane_area(&mut self, window: &Window, cx: &mut Context<Self>) {
        // Focus mode draws one pane instead of the tree. Leaving `pane_area` as it was
        // means any change made meanwhile is applied once focus mode ends.
        if self.focus_mode {
            return;
        }
        let viewport = window.viewport_size();
        let footer = if self.settings.status_bar.enabled {
            STATUS_BAR_HEIGHT
        } else {
            0.0
        };
        let area = Extent {
            width: viewport.width.as_f32(),
            height: viewport.height.as_f32() - footer,
        };
        if !self.layout_ready || area.width <= 0.0 || area.height <= 0.0 {
            self.pane_area = None;
            return;
        }
        let settled = *self.first_drawn.get_or_insert_with(Instant::now) + WINDOW_SETTLE;
        // The first area of a layout only records what the saved ratios were drawn at.
        let Some(previous) = self.pane_area.replace(area) else {
            return;
        };
        if previous == area || Instant::now() < settled {
            return;
        }
        let locked: HashSet<PaneId> = self
            .layout
            .pane_ids()
            .into_iter()
            .filter(|id| self.pane_is_locked(*id))
            .collect();
        if self
            .layout
            .preserve_locked(previous, area, &|id| locked.contains(&id), MIN_PANE_EXTENT)
        {
            // One write once the resize pauses, not one per frame of it.
            self.layout_save = Some(cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                let _ = this.update(cx, |workspace, _| workspace.save_layout());
            }));
        }
    }

    fn render_layout(
        &self,
        layout: &Layout,
        path: Vec<bool>,
        width: f32,
        x: f32,
        at_window_top: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme::palette(cx);
        match layout {
            Layout::Pane(id) => self.render_pane(*id, width, x, at_window_top, cx),
            Layout::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let horizontal = *axis == Axis::SideBySide;
                let key = format!("split-{path:?}");
                let split_bounds = Rc::new(Cell::new(Bounds::<Pixels>::default()));
                let measured = split_bounds.clone();
                let drag_path = path.clone();
                let resize_axis = *axis;
                let divider = div()
                    .id(format!("divider-{path:?}"))
                    .flex_none()
                    .bg(rgb(colors.divider))
                    .hover(|style| style.bg(rgb(colors.cyan)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |workspace, _, _, cx| {
                            workspace.resizing = Some(SplitResize {
                                path: drag_path.clone(),
                                axis: resize_axis,
                                bounds: split_bounds.get(),
                            });
                            cx.stop_propagation();
                        }),
                    );
                let divider = if horizontal {
                    divider
                        .w(px(DIVIDER_THICKNESS))
                        .h_full()
                        .cursor_col_resize()
                } else {
                    divider
                        .h(px(DIVIDER_THICKNESS))
                        .w_full()
                        .cursor_row_resize()
                };
                let mut first_path = path.clone();
                first_path.push(false);
                let mut second_path = path;
                second_path.push(true);
                let parent = div()
                    .id(key)
                    .relative()
                    .flex()
                    .size_full()
                    .min_w_0()
                    .min_h_0()
                    .child(
                        canvas(move |bounds, _, _| measured.set(bounds), |_, _, _, _| {})
                            .absolute()
                            .inset_0(),
                    );
                let parent = if horizontal {
                    parent.flex_row()
                } else {
                    parent.flex_col()
                };
                parent
                    .child(
                        div()
                            .flex()
                            .flex_basis(px(0.0))
                            .flex_grow(*ratio)
                            .min_w_0()
                            .min_h_0()
                            .child(self.render_layout(
                                first,
                                first_path,
                                if horizontal {
                                    (width - DIVIDER_THICKNESS).max(0.0) * *ratio
                                } else {
                                    width
                                },
                                x,
                                at_window_top,
                                cx,
                            )),
                    )
                    .child(divider)
                    .child(
                        div()
                            .flex()
                            .flex_basis(px(0.0))
                            .flex_grow(1.0 - *ratio)
                            .min_w_0()
                            .min_h_0()
                            .child(self.render_layout(
                                second,
                                second_path,
                                if horizontal {
                                    (width - DIVIDER_THICKNESS).max(0.0) * (1.0 - *ratio)
                                } else {
                                    width
                                },
                                if horizontal {
                                    x + (width - DIVIDER_THICKNESS).max(0.0) * *ratio
                                        + DIVIDER_THICKNESS
                                } else {
                                    x
                                },
                                at_window_top && horizontal,
                                cx,
                            )),
                    )
                    .into_any_element()
            }
        }
    }

    fn render_pane(
        &self,
        pane_id: PaneId,
        pane_width: f32,
        x: f32,
        at_window_top: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme::palette(cx);
        let Some(pane) = self.panes.get(&pane_id) else {
            return div().into_any_element();
        };
        let selected = self.active_pane == pane_id;
        let window_drag_enabled = at_window_top;
        let control_inset = if at_window_top && x < WINDOW_CONTROLS_CONTENT_INSET {
            // Keep the pane menu and a fixed drag area outside the scrolling tabs.
            (WINDOW_CONTROLS_CONTENT_INSET - x).min((pane_width - 40.0).max(0.0))
        } else {
            0.0
        };
        let header_width = pane_width - control_inset;
        let show_lock = header_width >= 108.0;
        let show_focus = header_width >= 180.0;
        let pane_locked = self.pane_is_locked(pane_id);
        let account_numbers = codex_account_numbers(&self.shells);
        let tabs = pane
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let tab_id = tab.id;
                let active = index == pane.active;
                let panel = tab.panel().is_some();
                // Shell tabs keep their titles; only built-in panels swap in an icon.
                let icon_panel = tab.panel().filter(|_| self.settings.panel_tab_icons);
                // The X is left out, not disabled, so a locked pane's tabs lose no room to it.
                let close_visible =
                    active && user_close_refusal(pane_locked, UserClose::Tab).is_none();
                let (pad_left, pad_right, gap) = match (icon_panel, close_visible) {
                    (None, _) => (8.0, 8.0, 6.0),
                    (Some(_), true) => (0.0, 4.0, 0.0),
                    (Some(_), false) => (0.0, 0.0, 0.0),
                };
                let tab_color = if active {
                    if panel { colors.magenta } else { colors.text }
                } else {
                    colors.muted
                };
                let workspace = cx.entity();
                let shell = tab
                    .shell_id()
                    .and_then(|id| self.shells.iter().find(|shell| shell.id == id));
                let display_title = codex_tab_title(&tab.title, shell, &account_numbers);
                div()
                    .id(("tab", tab_id))
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .h_full()
                    .pl(px(pad_left))
                    .pr(px(pad_right))
                    .gap(px(gap))
                    .border_r_1()
                    .border_b_1()
                    .border_color(rgb(if active { colors.cyan } else { colors.divider }))
                    .bg(rgb(if active {
                        colors.panel_active
                    } else {
                        colors.panel
                    }))
                    .text_color(rgb(tab_color))
                    .text_size(px(if panel { 9.0 } else { 10.0 }))
                    .cursor_grab()
                    .hover(|style| style.bg(rgb(colors.panel_active)))
                    .drag_over::<DraggedTab>(move |style, _, _, _| {
                        style.border_l_2().border_color(rgb(colors.cyan))
                    })
                    .child(match icon_panel {
                        // The tooltip sits on the icon's own box so it does not stack with the X's.
                        Some(kind) => div()
                            .id(("tab-icon", tab_id))
                            .h_full()
                            .min_w(px(32.0))
                            .px(px(8.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icons::icon(Icon::Panel(kind), tab_color))
                            .child(tooltip::anchor(panel_tooltip(kind), Look::Pane))
                            .into_any_element(),
                        None => display_title.clone().into_any_element(),
                    })
                    .children(close_visible.then(|| {
                        div()
                            .id(("close-tab", tab_id))
                            .size(px(18.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .rounded(px(3.0))
                            .hover(|style| style.bg(rgb(colors.divider)))
                            .child(icons::icon(Icon::Close, colors.muted))
                            .child(tooltip::anchor("Close tab · ⌘W", Look::Pane))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |workspace, _, window, cx| {
                                cx.stop_propagation();
                                workspace.close_tab_by_user(pane_id, tab_id, window, cx);
                            }))
                    }))
                    .on_click(cx.listener(move |workspace, _, window, cx| {
                        workspace.select_tab(pane_id, tab_id, window, cx)
                    }))
                    .on_drag(
                        DraggedTab {
                            pane_id,
                            tab_id,
                            project_id: self.project_id.clone(),
                            title: display_title,
                        },
                        move |drag, _, _, cx| {
                            workspace.update(cx, |workspace, cx| {
                                workspace.panel_menu = None;
                                workspace.begin_tab_drag(cx);
                            });
                            cx.new(|_| drag.clone())
                        },
                    )
                    .on_drop(
                        cx.listener(move |workspace, drag: &DraggedTab, window, cx| {
                            workspace.move_tab(drag, pane_id, index, window, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        let header = div()
            .id(("pane-header", pane_id))
            .overflow_hidden()
            .h(px(28.0))
            .flex_none()
            .flex()
            .min_w_0()
            .items_center()
            .bg(rgb(colors.panel))
            .border_b_1()
            .border_color(rgb(if selected {
                colors.cyan
            } else {
                colors.divider
            }))
            .pl(px(control_inset))
            .child(
                div()
                    .id(("tab-strip", pane_id))
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_x_scroll()
                    .children(tabs)
                    .child(
                        div()
                            .id(("window-drag-space", pane_id))
                            .flex_1()
                            .min_w(px(18.0))
                            .h_full()
                            .when(window_drag_enabled, |space| {
                                space
                                    .cursor_grab()
                                    .on_mouse_down(MouseButton::Left, start_window_drag)
                            }),
                    )
                    .on_drop(
                        cx.listener(move |workspace, drag: &DraggedTab, window, cx| {
                            let len = workspace
                                .panes
                                .get(&pane_id)
                                .map(|pane| pane.tabs.len())
                                .unwrap_or(0);
                            workspace.move_tab(drag, pane_id, len, window, cx);
                            cx.stop_propagation();
                        }),
                    ),
            )
            .children(window_drag_enabled.then(|| {
                div()
                    .id(("window-drag-handle", pane_id))
                    .flex_none()
                    .w(px(12.0))
                    .h_full()
                    .cursor_grab()
                    .on_mouse_down(MouseButton::Left, start_window_drag)
            }))
            .child(
                div()
                    .flex()
                    .h_full()
                    .flex_none()
                    .children(show_lock.then(|| {
                        self.pane_button(
                            pane_id,
                            "lock",
                            if pane_locked {
                                Icon::Lock
                            } else {
                                Icon::Unlock
                            },
                            if pane_locked {
                                colors.cyan
                            } else {
                                colors.muted
                            },
                            |workspace, id, _, cx| workspace.toggle_pane_lock(id, cx),
                            cx,
                        )
                    }))
                    .children(show_focus.then(|| {
                        self.pane_button(
                            pane_id,
                            "focus",
                            Icon::Focus,
                            colors.muted,
                            |workspace, id, window, cx| {
                                workspace.active_pane = id;
                                workspace.set_focus_mode(true, window, cx);
                            },
                            cx,
                        )
                    }))
                    .child(self.pane_button(
                        pane_id,
                        "menu",
                        Icon::More,
                        if self.panel_menu == Some(pane_id) {
                            colors.cyan
                        } else {
                            colors.muted
                        },
                        |workspace, id, window, cx| {
                            workspace.toggle_panel_menu(id, window, cx);
                        },
                        cx,
                    )),
            );
        let skill_upgrade = pane
            .tabs
            .get(pane.active)
            .and_then(Tab::shell_id)
            .and_then(|id| self.shells.iter().find(|shell| shell.id == id))
            .filter(|shell| {
                shell.kind == ShellKind::Orchestrator
                    && !self.sessions.orchestrator_skill_is_current(shell)
            })
            .map(|shell| shell.id.clone());
        let content = match pane.tabs.get(pane.active).map(|tab| &tab.content) {
            Some(TabContent::Shell {
                terminal,
                attach_error,
                ..
            }) => {
                let snapshot = pane
                    .tabs
                    .get(pane.active)
                    .and_then(|tab| self.terminal_snapshots.get(&tab.id));
                if self.tab_dragging {
                    match snapshot {
                        Some(snapshot) => img(snapshot.clone()).size_full().into_any_element(),
                        None => div().size_full().bg(rgb(colors.bg)).into_any_element(),
                    }
                } else if let Some(terminal) = terminal {
                    terminal.clone().into_any_element()
                } else {
                    // The terminal attaches during the frame this tab is shown in; this
                    // is what a tab shows if that failed. tmux redraws the screen on
                    // attach, so there is nothing to keep here.
                    div()
                        .size_full()
                        .p(px(14.0))
                        .bg(rgb(colors.bg))
                        .text_color(rgb(colors.muted))
                        .child(
                            attach_error
                                .clone()
                                .unwrap_or_else(|| "Attaching…".to_owned()),
                        )
                        .into_any_element()
                }
            }
            Some(TabContent::Panel(PanelKind::Usage)) => self.render_usage_panel(cx),
            Some(TabContent::Panel(PanelKind::Schedules)) => self
                .schedule_panel
                .as_ref()
                .map(|panel| panel.clone().into_any_element())
                .unwrap_or_else(|| div().into_any_element()),
            Some(TabContent::Panel(PanelKind::Settings)) => {
                self.settings_panel.clone().into_any_element()
            }
            Some(TabContent::Panel(PanelKind::Files)) => self
                .file_explorer
                .as_ref()
                .map(|panel| panel.clone().into_any_element())
                .unwrap_or_else(|| div().into_any_element()),
            Some(TabContent::Panel(PanelKind::ProjectSettings)) => self
                .project_settings_panel
                .as_ref()
                .map(|panel| panel.clone().into_any_element())
                .unwrap_or_else(|| div().into_any_element()),
            Some(TabContent::Panel(panel)) => panels::render_panel(
                *panel,
                PanelData {
                    state: &self.state,
                    project_id: &self.project_id,
                    selected_worktree_id: self.selected_worktree_id.as_deref(),
                    selected_task_id: self.selected_task_id.as_deref(),
                    shells: &self.shells,
                    shell_cwds: &self.shell_cwds,
                    metrics: &self.metrics,
                    activity: &self.agent_activity,
                    query: &self.search,
                    search_focused: self.search_focused && selected,
                    focus: self.focus.clone(),
                    control_inset: 0.0,
                    collapsed_folders: &self.collapsed_project_folders,
                    state_home: self.sessions.state_home(),
                    project_order: self.settings.project_order,
                    project_last_edits: &self.project_last_edits,
                    project_sort_menu_open: self.project_sort_menu_open,
                },
                Self::panel_action,
                cx,
            ),
            None => div()
                .p(px(14.0))
                .text_color(rgb(colors.muted))
                .child("Drop a tab here or use the pane menu")
                .into_any_element(),
        };
        let body_bounds = Rc::new(Cell::new(Bounds::<Pixels>::default()));
        let measured_body = body_bounds.clone();
        let body = div()
            .id(("pane-body", pane_id))
            .relative()
            .flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .bg(rgb(colors.bg))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |workspace, _, window, cx| {
                    if workspace.active_pane != pane_id {
                        workspace.select_pane(pane_id, window, cx);
                    }
                }),
            )
            .on_drag_move(cx.listener(
                move |workspace, event: &DragMoveEvent<DraggedTab>, _, cx| {
                    if event.drag(cx).project_id != workspace.project_id
                        || !event.bounds.contains(&event.event.position)
                    {
                        return;
                    }
                    let local = event.event.position - event.bounds.origin;
                    let x = local.x.as_f32() / event.bounds.size.width.as_f32().max(1.0);
                    let y = local.y.as_f32() / event.bounds.size.height.as_f32().max(1.0);
                    let side = if x < 0.23 {
                        Some(DockSide::Left)
                    } else if x > 0.77 {
                        Some(DockSide::Right)
                    } else if y < 0.23 {
                        Some(DockSide::Top)
                    } else if y > 0.77 {
                        Some(DockSide::Bottom)
                    } else {
                        None
                    };
                    let target = Some((pane_id, side));
                    if workspace.drop_target != target {
                        workspace.drop_target = target;
                        cx.notify();
                    }
                },
            ))
            .on_drop(
                cx.listener(move |workspace, drag: &DraggedTab, window, cx| {
                    workspace.pane_drop(drag, pane_id, body_bounds.get(), window, cx);
                    cx.stop_propagation();
                }),
            )
            .child(div().flex().size_full().min_w_0().min_h_0().child(content))
            .child(
                canvas(
                    move |bounds, _, _| measured_body.set(bounds),
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            );
        let body = if cx.has_active_drag() {
            if let Some((_, side)) = self.drop_target.filter(|(id, _)| *id == pane_id) {
                let overlay = div()
                    .absolute()
                    .bg(rgb(colors.cyan))
                    .opacity(0.13)
                    .border_1()
                    .border_color(rgb(colors.cyan));
                let overlay = match side {
                    Some(DockSide::Left) => {
                        overlay.top_0().bottom_0().left_0().w(gpui::relative(0.5))
                    }
                    Some(DockSide::Right) => {
                        overlay.top_0().bottom_0().right_0().w(gpui::relative(0.5))
                    }
                    Some(DockSide::Top) => {
                        overlay.top_0().left_0().right_0().h(gpui::relative(0.5))
                    }
                    Some(DockSide::Bottom) => {
                        overlay.bottom_0().left_0().right_0().h(gpui::relative(0.5))
                    }
                    None => overlay.inset(px(8.0)),
                };
                body.child(overlay)
            } else {
                body
            }
        } else {
            body
        };
        let container = div()
            .id(("pane", pane_id))
            .overflow_hidden()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .min_h_0();
        let container = container
            .children((!self.focus_mode).then_some(header))
            .children(skill_upgrade.filter(|_| !self.focus_mode).map(|shell_id| {
                div()
                    .h(px(23.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .px(px(10.0))
                    .bg(rgb(colors.panel_active))
                    .border_b_1()
                    .border_color(rgb(colors.divider))
                    .text_color(rgb(colors.muted))
                    .child("ORCHESTRATOR SKILL UPDATE AVAILABLE")
                    .child(div().flex_1())
                    .child(
                        div()
                            .id(("load-orchestrator-skill", pane_id))
                            .text_color(rgb(colors.gold))
                            .cursor_pointer()
                            .child("LOAD SKILL")
                            .on_click(cx.listener(move |workspace, _, _, cx| {
                                match workspace.sessions.load_orchestrator_skill(&shell_id) {
                                    Ok(_) => {
                                        workspace.notice = None;
                                        if let Ok(shells) = workspace.sessions.list() {
                                            workspace.shells = shells;
                                        }
                                    }
                                    Err(error) => workspace.notice = Some(error),
                                }
                                cx.notify();
                            })),
                    )
            }))
            .child(body);
        container
            .children(
                (!self.focus_mode && self.panel_menu == Some(pane_id)).then(|| {
                    let menu = div()
                        .id(("view-menu", pane_id))
                        .absolute()
                        .top(px(30.0))
                        .right(px(6.0))
                        .w(px(248.0_f32.min((pane_width - 12.0).max(0.0))))
                        .max_h(gpui::relative(0.9))
                        .overflow_y_scroll()
                        .bg(rgb(colors.panel_active))
                        .border_1()
                        .border_color(rgb(colors.magenta))
                        .p(px(3.0))
                        .on_mouse_down_out(cx.listener(|workspace, _, window, cx| {
                            workspace.panel_menu = None;
                            workspace.finish_tab_drag(cx);
                            workspace.focus_active(window, cx);
                            cx.notify();
                        }));
                    menu.child(pane_menu_heading("NEW TAB", colors))
                        .children(
                            [
                                ("Shell", "⌘T", Some(Icon::Add), PaneMenuAction::Shell),
                                (
                                    "Codex",
                                    "⌘⇧C",
                                    None,
                                    PaneMenuAction::Harness(HarnessKind::Codex, false),
                                ),
                                (
                                    "Claude",
                                    "⌘⇧L",
                                    None,
                                    PaneMenuAction::Harness(HarnessKind::Claude, false),
                                ),
                                (
                                    "Grok",
                                    "⌘⇧G",
                                    None,
                                    PaneMenuAction::Harness(HarnessKind::Grok, false),
                                ),
                                (
                                    "Codex · unrestricted",
                                    "",
                                    None,
                                    PaneMenuAction::Harness(HarnessKind::Codex, true),
                                ),
                                (
                                    "Claude · unrestricted",
                                    "",
                                    None,
                                    PaneMenuAction::Harness(HarnessKind::Claude, true),
                                ),
                                (
                                    "Grok · unrestricted",
                                    "",
                                    None,
                                    PaneMenuAction::Harness(HarnessKind::Grok, true),
                                ),
                                (
                                    "Global orchestrator",
                                    "⌘⇧O",
                                    None,
                                    PaneMenuAction::Orchestrator(false),
                                ),
                                (
                                    "Project orchestrator",
                                    "⌘⌥O",
                                    None,
                                    PaneMenuAction::Orchestrator(true),
                                ),
                            ]
                            .into_iter()
                            .map(|(label, shortcut, icon, action)| {
                                self.pane_menu_row(pane_id, label, shortcut, icon, action, cx)
                            }),
                        )
                        .child(pane_menu_heading("VIEWS", colors))
                        .children(
                            [
                                PanelKind::Projects,
                                PanelKind::Files,
                                PanelKind::Worktrees,
                                PanelKind::Tasks,
                                PanelKind::Shells,
                                PanelKind::Usage,
                                PanelKind::Schedules,
                                PanelKind::ProjectSettings,
                                PanelKind::Settings,
                            ]
                            .into_iter()
                            .map(|kind| {
                                self.pane_menu_row(
                                    pane_id,
                                    Self::panel_title(kind),
                                    match kind {
                                        PanelKind::Files => "⌘⇧E",
                                        PanelKind::Settings => "⌘,",
                                        PanelKind::Schedules => "⌘⇧S",
                                        _ => "",
                                    },
                                    None,
                                    PaneMenuAction::View(kind),
                                    cx,
                                )
                            }),
                        )
                        .child(pane_menu_heading("PANE", colors))
                        .children(
                            [
                                (
                                    "Split right",
                                    "⌘D",
                                    Icon::SplitRight,
                                    PaneMenuAction::Split(Axis::SideBySide),
                                ),
                                (
                                    "Split down",
                                    "⌘⇧D",
                                    Icon::SplitDown,
                                    PaneMenuAction::Split(Axis::Stacked),
                                ),
                                ("Close pane", "⌘⇧W", Icon::Close, PaneMenuAction::Close),
                            ]
                            .into_iter()
                            // Menus here have no disabled rows, so a locked pane just omits Close pane.
                            .filter(|(_, _, _, action)| {
                                !matches!(action, PaneMenuAction::Close)
                                    || user_close_refusal(pane_locked, UserClose::Pane).is_none()
                            })
                            .map(|(label, shortcut, icon, action)| {
                                self.pane_menu_row(pane_id, label, shortcut, Some(icon), action, cx)
                            }),
                        )
                        .children((!show_lock).then(|| {
                            self.pane_menu_row(
                                pane_id,
                                if pane_locked {
                                    "Unlock pane"
                                } else {
                                    "Lock pane"
                                },
                                "",
                                Some(if pane_locked {
                                    Icon::Lock
                                } else {
                                    Icon::Unlock
                                }),
                                PaneMenuAction::Lock,
                                cx,
                            )
                        }))
                        .children((!show_focus).then(|| {
                            self.pane_menu_row(
                                pane_id,
                                "Focus tab",
                                "⌘⇧F",
                                Some(Icon::Focus),
                                PaneMenuAction::Focus,
                                cx,
                            )
                        }))
                }),
            )
            .into_any_element()
    }

    fn render_status(&self, cx: &mut Context<Self>) -> AnyElement {
        use status_bar::StatusSide;
        let colors = theme::palette(cx);
        let settings = &self.settings.status_bar;
        div()
            .id("status-items")
            .size_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(8.0))
            .px(px(8.0))
            .text_size(px(9.0))
            .text_color(rgb(colors.muted))
            .child(
                div()
                    .id("status-left")
                    .flex()
                    .min_w_0()
                    .items_center()
                    .gap(px(10.0))
                    .overflow_x_scroll()
                    .children(
                        settings
                            .visible_items(StatusSide::Left)
                            .into_iter()
                            .map(|kind| self.render_status_item(kind, cx)),
                    ),
            )
            .child(div().flex_1().min_w(px(4.0)))
            .child(
                div()
                    .id("status-right")
                    .flex()
                    .min_w_0()
                    .items_center()
                    .gap(px(10.0))
                    .overflow_x_scroll()
                    .children(
                        settings
                            .visible_items(StatusSide::Right)
                            .into_iter()
                            .map(|kind| self.render_status_item(kind, cx)),
                    ),
            )
            .into_any_element()
    }

    fn render_status_item(
        &self,
        kind: status_bar::StatusItemKind,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use status_bar::StatusItemKind;
        let colors = theme::palette(cx);
        match kind {
            StatusItemKind::Project => {
                let name = self
                    .state
                    .project(&self.project_id)
                    .map(|project| project.name.clone())
                    .unwrap_or_else(|_| "Project unavailable".to_owned());
                div()
                    .id("status-current-project")
                    .max_w(px(240.0))
                    .min_w_0()
                    .text_ellipsis()
                    .overflow_hidden()
                    .text_color(rgb(colors.cyan))
                    .cursor_pointer()
                    .child(name)
                    .on_click(cx.listener(|workspace, _, window, cx| {
                        workspace.open_panel(
                            PanelKind::ProjectSettings,
                            workspace.active_pane,
                            window,
                            cx,
                        );
                    }))
                    .into_any_element()
            }
            StatusItemKind::Worktree => {
                let branch = self
                    .selected_worktree_id
                    .as_ref()
                    .and_then(|id| {
                        self.state.worktrees.iter().find(|worktree| {
                            &worktree.id == id && worktree.project_id == self.project_id
                        })
                    })
                    .map(|worktree| worktree.branch.clone())
                    .unwrap_or_else(|| "No worktree".to_owned());
                div()
                    .id("status-current-worktree")
                    .max_w(px(220.0))
                    .min_w_0()
                    .text_ellipsis()
                    .overflow_hidden()
                    .cursor_pointer()
                    .child(branch)
                    .on_click(cx.listener(|workspace, _, window, cx| {
                        workspace.open_panel(
                            PanelKind::Worktrees,
                            workspace.active_pane,
                            window,
                            cx,
                        );
                    }))
                    .into_any_element()
            }
            StatusItemKind::AgentActivity => {
                let counts = activity::ActivityCounts::for_project(
                    &self.project_id,
                    &self.shells,
                    &self.agent_activity,
                );
                div()
                    .id("status-agent-activity")
                    .max_w(px(250.0))
                    .min_w_0()
                    .text_ellipsis()
                    .overflow_hidden()
                    .cursor_pointer()
                    .text_color(rgb(if counts.working > 0 {
                        colors.cyan
                    } else {
                        colors.muted
                    }))
                    .child(counts.summary().unwrap_or_else(|| "Agents · —".to_owned()))
                    .on_click(cx.listener(|workspace, _, window, cx| {
                        workspace.open_panel(PanelKind::Shells, workspace.active_pane, window, cx);
                    }))
                    .into_any_element()
            }
            StatusItemKind::LiveSessions => div()
                .flex_none()
                .child(format!(
                    "{} LIVE",
                    self.shells
                        .iter()
                        .filter(
                            |shell| shell.project_id.as_deref() == Some(&self.project_id)
                                && shell.alive
                        )
                        .count()
                ))
                .into_any_element(),
            StatusItemKind::Resources => {
                let (cpu, ram) = self
                    .shells
                    .iter()
                    .filter(|shell| {
                        shell.project_id.as_deref() == Some(&self.project_id) && shell.alive
                    })
                    .fold((0.0_f32, 0_u64), |(cpu, ram), shell| {
                        let metrics = self.metrics.get(&shell.id).copied().unwrap_or_default();
                        (cpu + metrics.cpu_percent, ram + metrics.ram_bytes)
                    });
                div()
                    .flex_none()
                    .child(format!("CPU {cpu:.1}% RAM {}", format_bytes(ram)))
                    .into_any_element()
            }
            StatusItemKind::Usage => self.render_usage_chip(cx),
            StatusItemKind::CodexAccount => self.render_codex_account_status(cx),
            StatusItemKind::SessionId => {
                let id = self
                    .panes
                    .get(&self.active_pane)
                    .and_then(|pane| pane.tabs.get(pane.active))
                    .and_then(Tab::shell_id)
                    .map(str::to_owned);
                div()
                    .id("copy-active-shell-id")
                    .flex_none()
                    .text_color(rgb(colors.cyan))
                    .when_some(id, |item, id| {
                        item.cursor_pointer()
                            .child(id.chars().take(8).collect::<String>())
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(id.clone()))
                            }))
                    })
                    .into_any_element()
            }
            StatusItemKind::GlobalOrchestrator => div()
                .id("top-orchestrator")
                .flex_none()
                .text_color(rgb(colors.magenta))
                .cursor_pointer()
                .child("G·ORCH")
                .on_click(
                    cx.listener(|workspace, _, window, cx| workspace.open_orchestrator(window, cx)),
                )
                .into_any_element(),
            StatusItemKind::ProjectOrchestrator => div()
                .id("project-orchestrator")
                .flex_none()
                .text_color(rgb(colors.cyan))
                .cursor_pointer()
                .child("P·ORCH")
                .on_click(cx.listener(|workspace, _, window, cx| {
                    workspace.open_scoped_orchestrator(
                        Some(workspace.project_id.clone()),
                        window,
                        cx,
                    );
                }))
                .into_any_element(),
        }
    }

    fn render_codex_account_status(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = theme::palette(cx);
        let snapshot = cx
            .global::<settings::CodexAccountsState>()
            .snapshot
            .as_ref();
        let active = self
            .panes
            .get(&self.active_pane)
            .and_then(|pane| pane.tabs.get(pane.active))
            .and_then(Tab::shell_id)
            .and_then(|id| self.shells.iter().find(|shell| shell.id == id))
            .filter(|shell| shell.harness == Some(HarnessKind::Codex));
        let label = if let Some(shell) = active {
            codex_session_status_label(shell, &codex_account_numbers(&self.shells), snapshot)
        } else {
            project_default_account_label(
                self.state
                    .projects
                    .iter()
                    .find(|project| project.id == self.project_id),
                self.settings.selected_codex_account.as_deref(),
                snapshot,
            )
        };
        div()
            .id("status-codex-account")
            .max_w(px(300.0))
            .min_w_0()
            .overflow_hidden()
            .text_ellipsis()
            .text_color(rgb(colors.cyan))
            .cursor_pointer()
            .child(label)
            .on_click(cx.listener(|workspace, _, window, cx| {
                workspace.open_panel(
                    PanelKind::ProjectSettings,
                    workspace.active_pane,
                    window,
                    cx,
                );
            }))
            .into_any_element()
    }

    fn render_usage_chip(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = theme::palette(cx);
        let active_shell = self.active_shell();
        let cache = cx.global::<AccountUsage>();
        let label = if let Some(shell) =
            active_shell.filter(|shell| shell.harness == Some(HarnessKind::Claude))
        {
            self.claude_usage
                .get(&shell.id)
                .map(|snapshot| usage_summary("CLAUDE", snapshot))
                .unwrap_or_else(|| "CLAUDE · WAITING".to_owned())
        } else if let Some(shell) =
            active_shell.filter(|shell| shell.harness == Some(HarnessKind::Grok))
        {
            // Grok's allowance is not readable, so the chip shows this session.
            if shell.alive {
                usage::grok_chip_label(self.grok_usage.get(&shell.id))
            } else {
                "GROK · EXITED".to_owned()
            }
        } else {
            let profile = if let Some(shell) =
                active_shell.filter(|shell| shell.harness == Some(HarnessKind::Codex))
            {
                shell.codex_home.clone()
            } else {
                project_selected_codex_home(self, cx)
            };
            let entry = profile.as_ref().and_then(|home| cache.codex.get(home));
            entry
                .and_then(|entry| entry.codex.as_ref())
                .map(|snapshot| usage_summary("CODEX", snapshot))
                .unwrap_or_else(|| {
                    if profile.is_none() {
                        "CODEX · ACCOUNT UNKNOWN"
                    } else if entry.is_some_and(|entry| entry.pending) {
                        "USAGE · LOADING"
                    } else {
                        "USAGE · —"
                    }
                    .to_owned()
                })
        };
        div()
            .id("usage-chip")
            .max_w(px(220.0))
            .overflow_hidden()
            .text_ellipsis()
            .text_color(rgb(colors.cyan))
            .cursor_pointer()
            .hover(|style| style.text_color(rgb(colors.magenta)))
            .child(label)
            .on_click(cx.listener(|workspace, _, window, cx| {
                workspace.open_panel(PanelKind::Usage, workspace.active_pane, window, cx);
            }))
            .into_any_element()
    }

    fn render_usage_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = theme::palette(cx);
        let cache = cx.global::<AccountUsage>();
        let pending = cache.codex.values().any(|entry| entry.pending);
        let mut cards = Vec::new();
        let mut profiles = codex_usage_profiles(cx);
        for shell in self
            .shells
            .iter()
            .filter(|shell| shell.harness == Some(HarnessKind::Codex))
        {
            if let Some(home) = &shell.codex_home {
                profiles.entry(home.clone()).or_insert_with(|| {
                    shell
                        .codex_account_label
                        .clone()
                        .unwrap_or_else(|| "Session account".to_owned())
                });
            }
        }
        for (home, label) in profiles {
            let entry = cache.codex.get(&home);
            let title = format!("CODEX · {label}");
            if let Some(snapshot) = entry.and_then(|entry| entry.codex.as_ref()) {
                cards.push(render_provider_usage(snapshot, &title, colors));
            } else {
                cards.push(
                    div()
                        .p(px(12.0))
                        .border_t_1()
                        .border_color(rgb(colors.divider))
                        .child(title)
                        .child(div().mt(px(6.0)).text_color(rgb(colors.muted)).child(
                            if entry.is_some_and(|entry| entry.pending) {
                                "Reading account usage…".to_owned()
                            } else {
                                entry
                                    .and_then(|entry| entry.codex_error.clone())
                                    .unwrap_or_else(|| "Account usage is unavailable".to_owned())
                            },
                        ))
                        .into_any_element(),
                );
            }
            if let Some(error) = entry
                .filter(|entry| entry.codex.is_some())
                .and_then(|entry| entry.codex_error.as_ref())
            {
                cards.push(
                    div()
                        .px(px(12.0))
                        .text_color(rgb(colors.gold))
                        .child(format!("Last refresh: {error}"))
                        .into_any_element(),
                );
            }
        }
        let claude_shells = self.shells.iter().filter(|shell| {
            shell.project_id.as_deref() == Some(self.project_id.as_str())
                && shell.harness == Some(HarnessKind::Claude)
        });
        let mut has_claude = false;
        for shell in claude_shells {
            has_claude = true;
            let title = format!("CLAUDE · {}", &shell.id[..8]);
            if let Some(snapshot) = self.claude_usage.get(&shell.id) {
                cards.push(render_provider_usage(snapshot, &title, colors));
            } else {
                cards.push(
                    div()
                        .p(px(12.0))
                        .border_t_1()
                        .border_color(rgb(colors.divider))
                        .child(title)
                        .child(
                            div()
                                .mt(px(6.0))
                                .text_color(rgb(colors.muted))
                                .child("Waiting for Claude usage after its first response"),
                        )
                        .into_any_element(),
                );
            }
        }
        if !has_claude {
            cards.push(
                div()
                    .p(px(12.0))
                    .text_color(rgb(colors.muted))
                    .child("Open a Claude session to see its usage here")
                    .into_any_element(),
            );
        }
        cards.extend(self.render_grok_usage_cards(colors));
        div().size_full().flex().flex_col().min_h_0().bg(rgb(colors.panel))
            .child(div().h(px(32.0)).flex_none().flex().items_center().px(px(10.0)).justify_between()
                .border_b_1().border_color(rgb(colors.divider)).child("ACCOUNT USAGE")
                .child(div().id("refresh-account-usage").text_color(rgb(colors.cyan)).cursor_pointer()
                    .child(if pending || self.grok_usage_pending { "REFRESHING…" } else { "↻ REFRESH" })
                    .on_click(cx.listener(|workspace, _, window, cx| {
                        workspace.refresh_grok_usage(true, cx);
                        request_codex_usage(true, cx);
                        for home in workspace.shells.iter()
                            .filter(|shell| shell.harness == Some(HarnessKind::Codex))
                            .filter_map(|shell| shell.codex_home.clone()).collect::<HashSet<_>>() {
                            request_codex_usage_at(home, true, cx);
                        }
                        workspace.refresh(window, cx);
                    }))))
            .child(div().id("usage-panel-scroll").flex_1().min_h_0().overflow_y_scroll().children(cards))
            .child(div().flex_none().p(px(10.0)).border_t_1().border_color(rgb(colors.divider)).text_color(rgb(colors.muted)).text_size(px(9.0))
                .child("Quota is shared by all sessions on the same account. Missing windows are unavailable. Claude subscription quota requires a supported Pro/Max account. Grok shows each session's tokens and cost only."))
            .into_any_element()
    }

    /// Grok's section of the Usage panel: one card per live Grok tab of the
    /// project, their combined total, and where the account allowance is.
    fn render_grok_usage_cards(&self, colors: Palette) -> Vec<AnyElement> {
        let shells = self.shells.iter().filter(|shell| {
            shell.alive
                && shell.harness == Some(HarnessKind::Grok)
                && shell.project_id.as_deref() == Some(self.project_id.as_str())
        });
        let mut cards = Vec::new();
        let mut usages = Vec::new();
        let mut listed = 0;
        for shell in shells {
            listed += 1;
            let title = self
                .panes
                .values()
                .flat_map(|pane| pane.tabs.iter())
                .find(|tab| tab.shell_id() == Some(shell.id.as_str()))
                .map(|tab| tab.title.to_uppercase())
                .unwrap_or_else(|| format!("GROK · {}", &shell.id[..8]));
            let tab = self.grok_usage.get(&shell.id);
            usages.extend(tab.and_then(|tab| tab.usage.as_ref()));
            cards.push(render_grok_session(&title, tab, colors));
        }
        if listed == 0 {
            cards.push(
                div()
                    .p(px(12.0))
                    .border_t_1()
                    .border_color(rgb(colors.divider))
                    .text_color(rgb(colors.muted))
                    .child("No live Grok sessions")
                    .into_any_element(),
            );
        } else if !usages.is_empty() {
            cards.push(render_grok_total(
                &usage::grok_totals(usages.iter().copied()),
                listed - usages.len(),
                colors,
            ));
        }
        cards.push(
            div()
                .px(px(12.0))
                .pb(px(12.0))
                .pt(px(8.0))
                .border_t_1()
                .border_color(rgb(colors.divider))
                .text_color(rgb(colors.muted))
                .text_size(px(9.0))
                .child("Grok's account allowance and credits are shown only in Grok's own /usage screen. RiWork cannot read them through an official interface.")
                .into_any_element(),
        );
        cards
    }

    fn render_focus(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = theme::palette(cx);
        let show_window_controls = window_controls_visible(window);
        let viewport = window.viewport_size();
        let width = viewport.width.as_f32();
        let height = viewport.height.as_f32();
        let title = self
            .panes
            .get(&self.active_pane)
            .and_then(|pane| pane.tabs.get(pane.active))
            .map(|tab| tab.title.clone())
            .unwrap_or_default();
        let centered = self.focus_centered;
        let toolbar = div()
            .id("focus-toolbar")
            .flex()
            .flex_none()
            .items_center()
            .h(px(FOCUS_TOOLBAR_HEIGHT))
            .pl(px(if show_window_controls {
                WINDOW_CONTROLS_CONTENT_INSET
            } else {
                12.0
            }))
            .pr(px(12.0))
            .gap(px(12.0))
            .bg(rgb(colors.panel))
            .border_b_1()
            .border_color(rgb(colors.divider))
            .child(
                div()
                    .id("focus-window-drag-space")
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .items_center()
                    .gap(px(12.0))
                    .when(show_window_controls, |space| {
                        space
                            .cursor_grab()
                            .on_mouse_down(MouseButton::Left, start_window_drag)
                    })
                    .children((width >= 720.0).then(|| {
                        div()
                            .flex_none()
                            .text_color(rgb(colors.cyan))
                            .child(if centered { "CENTER FOCUS" } else { "FOCUS" })
                    }))
                    .child(
                        div()
                            .text_ellipsis()
                            .text_color(rgb(colors.muted))
                            .child(title),
                    ),
            )
            .child(
                div()
                    .id("focus-layout-toggle")
                    .flex_none()
                    .px(px(8.0))
                    .py(px(5.0))
                    .cursor_pointer()
                    .text_color(rgb(colors.muted))
                    .hover(|style| {
                        style
                            .bg(rgb(colors.panel_active))
                            .text_color(rgb(colors.cyan))
                    })
                    .child(if centered {
                        "⛶ FILL WINDOW"
                    } else {
                        "⊙ CENTER FOCUS"
                    })
                    .on_click(cx.listener(|workspace, _, window, cx| {
                        workspace.focus_centered = !workspace.focus_centered;
                        workspace.focus_active(window, cx);
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .id("restore-workspace")
                    .flex_none()
                    .px(px(10.0))
                    .py(px(5.0))
                    .border_1()
                    .border_color(rgb(colors.divider))
                    .bg(rgb(colors.panel_active))
                    .text_color(rgb(colors.cyan))
                    .cursor_pointer()
                    .hover(|style| style.border_color(rgb(colors.cyan)))
                    .child(if width >= 600.0 {
                        "↙ RESTORE  ⌘⇧F"
                    } else {
                        "↙ RESTORE"
                    })
                    .on_click(cx.listener(|workspace, _, window, cx| {
                        workspace.set_focus_mode(false, window, cx);
                    })),
            );
        let content = if centered {
            let top_gap = 20.0_f32.min((height - FOCUS_TOOLBAR_HEIGHT).max(0.0));
            let content_width = (width - 48.0).max(0.0).min(FOCUS_MAX_WIDTH);
            let content_height =
                (height * (1.0 - FOCUS_BOTTOM_MARGIN) - FOCUS_TOOLBAR_HEIGHT - top_gap).max(0.0);
            div()
                .flex()
                .flex_none()
                .justify_center()
                .w_full()
                .h(px(content_height))
                .mt(px(top_gap))
                .child(
                    div()
                        .w(px(content_width))
                        .h_full()
                        .border_1()
                        .border_color(rgb(colors.divider))
                        .child(self.render_pane(
                            self.active_pane,
                            content_width - 2.0,
                            0.0,
                            false,
                            cx,
                        )),
                )
                .into_any_element()
        } else {
            div()
                .flex()
                .flex_1()
                .min_h_0()
                .child(self.render_pane(self.active_pane, width, 0.0, false, cx))
                .into_any_element()
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(toolbar)
            .child(content)
            .into_any_element()
    }

    fn pane_button(
        &self,
        pane_id: PaneId,
        key: &'static str,
        icon: Icon,
        color: u32,
        action: impl Fn(&mut Self, PaneId, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme::palette(cx);
        div()
            .id(format!("pane-{pane_id}-{key}"))
            .h_full()
            .w(px(28.0))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .when(
                key == "menu" && self.panel_menu == Some(pane_id),
                |button| button.bg(rgb(colors.divider)),
            )
            .hover(|style| style.bg(rgb(colors.divider)))
            .child(icons::icon(icon, color))
            .when(
                key != "menu" || self.panel_menu != Some(pane_id),
                |button| {
                    let label = match key {
                        "lock" if self.pane_is_locked(pane_id) => {
                            "Locked across project switches, tabs can't close · click to unlock"
                        }
                        "lock" => "Lock across project switches and keep tabs open",
                        "focus" => "Focus this tab · ⌘⇧F",
                        _ => "Add tabs and manage this pane",
                    };
                    button.child(tooltip::anchor(label, Look::Pane))
                },
            )
            .on_click(
                cx.listener(move |workspace, _, window, cx| action(workspace, pane_id, window, cx)),
            )
            .into_any_element()
    }

    fn pane_menu_row(
        &self,
        pane_id: PaneId,
        label: &'static str,
        shortcut: &'static str,
        icon: Option<Icon>,
        action: PaneMenuAction,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme::palette(cx);
        div()
            .id(format!("pane-menu-{pane_id}-{label}"))
            .flex()
            .items_center()
            .gap(px(8.0))
            .px(px(8.0))
            .py(px(5.0))
            .text_size(px(10.0))
            .text_color(rgb(colors.text))
            .cursor_pointer()
            .hover(|style| style.bg(rgb(colors.divider)).text_color(rgb(colors.cyan)))
            .child(
                div()
                    .w(px(14.0))
                    .flex_none()
                    .children(icon.map(|icon| icons::icon(icon, colors.muted))),
            )
            .child(div().flex_1().min_w_0().text_ellipsis().child(label))
            .children((!shortcut.is_empty()).then(|| {
                div()
                    .flex_none()
                    .text_size(px(9.0))
                    .text_color(rgb(colors.muted))
                    .child(shortcut)
            }))
            .on_click(cx.listener(move |workspace, _, window, cx| {
                workspace.panel_menu = None;
                workspace.finish_tab_drag(cx);
                workspace.active_pane = pane_id;
                match action {
                    PaneMenuAction::Shell => workspace.add_tab(window, cx),
                    PaneMenuAction::Harness(kind, unrestricted) => {
                        workspace.add_harness(kind, unrestricted, window, cx);
                    }
                    PaneMenuAction::Orchestrator(project_scoped) => {
                        let project_id = project_scoped.then(|| workspace.project_id.clone());
                        workspace.open_scoped_orchestrator(project_id, window, cx);
                    }
                    PaneMenuAction::View(kind) => workspace.open_panel(kind, pane_id, window, cx),
                    PaneMenuAction::Split(axis) => workspace.add_split(axis, window, cx),
                    PaneMenuAction::Close => workspace.close_pane_by_user(pane_id, window, cx),
                    PaneMenuAction::Lock => {
                        workspace.toggle_pane_lock(pane_id, cx);
                        workspace.focus_active(window, cx);
                    }
                    PaneMenuAction::Focus => workspace.set_focus_mode(true, window, cx),
                }
            }))
            .into_any_element()
    }

    fn render_root_dock(
        &self,
        side: DockSide,
        controls_visible: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme::palette(cx);
        let (inset_left, inset_top) = root_dock_insets(controls_visible);
        let footer_height = if self.focus_mode || !self.settings.status_bar.enabled {
            0.0
        } else {
            STATUS_BAR_HEIGHT
        };
        let target = div()
            .id(match side {
                DockSide::Left => "root-left",
                DockSide::Right => "root-right",
                DockSide::Top => "root-top",
                DockSide::Bottom => "root-bottom",
            })
            .absolute()
            .bg(rgb(colors.panel_active))
            .opacity(0.75)
            .drag_over::<DraggedTab>(move |style, _, _, _| style.bg(rgb(colors.cyan)))
            .on_drop(
                cx.listener(move |workspace, drag: &DraggedTab, window, cx| {
                    workspace.dock_tab(drag, None, side, window, cx);
                    cx.stop_propagation();
                }),
            );
        match side {
            DockSide::Left => target
                .left_0()
                .top(px(inset_top))
                .bottom(px(12.0 + footer_height))
                .w(px(12.0)),
            DockSide::Right => target
                .right_0()
                .top(px(12.0))
                .bottom(px(12.0 + footer_height))
                .w(px(12.0)),
            DockSide::Top => target.top_0().left(px(inset_left)).right_0().h(px(12.0)),
            DockSide::Bottom => target
                .bottom(px(footer_height))
                .left_0()
                .right_0()
                .h(px(12.0)),
        }
        .into_any_element()
    }
}

impl EntityInputHandler for Workspace {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let start = utf16_to_byte(&self.search, range_utf16.start);
        let end = utf16_to_byte(&self.search, range_utf16.end);
        let range = start.min(end)..start.max(end);
        *actual_range = Some(
            self.search[..range.start].encode_utf16().count()
                ..self.search[..range.end].encode_utf16().count(),
        );
        Some(self.search[range].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let end = self.search.encode_utf16().count();
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.search_marked.as_ref().map(|range| {
            self.search[..range.start].encode_utf16().count()
                ..self.search[..range.end].encode_utf16().count()
        })
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.search_marked = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.replace_search_text(range_utf16, text);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let inserted = self.replace_search_text(range_utf16, text);
        if !text.is_empty() {
            self.search_marked = Some(inserted);
        }
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(bounds)
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.search.encode_utf16().count())
    }

    fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        Some(self.search.encode_utf16().count())
    }

    fn accepts_text_input(&self, _: &mut Window, _: &mut Context<Self>) -> bool {
        self.search_focused
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme::palette(cx);
        let show_window_controls = window_controls_visible(window);
        if self.search_focused && !self.focus.is_focused(window) {
            // Focus moved elsewhere (a click on a terminal, say) without going
            // through the workspace.
            self.search_focused = false;
            self.search_marked = None;
        }
        if !cx.has_active_drag() {
            self.drop_target = None;
            if self.tab_dragging && self.panel_menu.is_none() && !self.modal_open() {
                self.finish_tab_drag(cx);
            }
        }
        self.follow_pane_area(window, cx);
        // Every tab on screen needs a terminal before the panes are drawn: tabs restored
        // with the window, and tabs that were released while hidden.
        self.attach_shown_terminals(window, cx);
        if self.sync_tab_visibility(cx) {
            // Some tab just went off screen or came back; look at what is hidden.
            self.schedule_terminal_release(Some(terminal_lifecycle::MIN_RECHECK), cx);
        }
        div()
            .id("riwork")
            .relative()
            .key_context("RiWork")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::new_tab_action))
            .on_action(cx.listener(Self::create_project_action))
            .on_action(cx.listener(Self::new_project_window_action))
            .on_action(cx.listener(Self::open_codex_action))
            .on_action(cx.listener(Self::open_claude_action))
            .on_action(cx.listener(Self::open_grok_action))
            .on_action(cx.listener(Self::split_right_action))
            .on_action(cx.listener(Self::split_down_action))
            .on_action(cx.listener(Self::close_tab_action))
            .on_action(cx.listener(Self::close_pane_action))
            .on_action(cx.listener(Self::next_tab_action))
            .on_action(cx.listener(Self::previous_tab_action))
            .on_action(cx.listener(Self::toggle_sidebar_action))
            .on_action(cx.listener(Self::open_settings_action))
            .on_action(cx.listener(Self::open_project_settings_action))
            .on_action(cx.listener(Self::open_schedules_action))
            .on_action(cx.listener(Self::open_files_action))
            .on_action(cx.listener(Self::focus_search_action))
            .on_action(cx.listener(Self::toggle_focus_mode_action))
            .on_action(cx.listener(Self::open_orchestrator_action))
            .on_action(cx.listener(Self::open_project_orchestrator_action))
            .on_key_down(cx.listener(Self::search_key_down))
            .on_mouse_move(
                cx.listener(|workspace, event: &gpui::MouseMoveEvent, _, cx| {
                    workspace.resize_at(event.position, cx)
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|workspace, _, window, cx| workspace.end_resize(window, cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|workspace, _, window, cx| workspace.end_resize(window, cx)),
            )
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(colors.bg))
            .text_color(rgb(colors.text))
            .font_family("Menlo")
            .text_size(px(10.0))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .overflow_hidden()
                    .child(if self.focus_mode {
                        self.render_focus(window, cx)
                    } else {
                        self.render_layout(
                            &self.layout,
                            Vec::new(),
                            window.viewport_size().width.as_f32(),
                            0.0,
                            show_window_controls,
                            cx,
                        )
                    }),
            )
            .children(
                (!self.focus_mode && self.settings.status_bar.enabled).then(|| {
                    div()
                        .id("bottom-status-bar")
                        .w_full()
                        .h(px(STATUS_BAR_HEIGHT))
                        .flex_none()
                        .flex()
                        .items_center()
                        .bg(rgb(colors.panel))
                        .border_t_1()
                        .border_color(rgb(colors.divider))
                        .child(self.render_status(cx))
                }),
            )
            .children(show_window_controls.then(|| window_controls_island(cx)))
            .children(self.project_creator.as_ref().map(|creator| {
                div()
                    .absolute()
                    .inset_0()
                    .size_full()
                    .p(px(16.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(gpui::rgba(0x00000099))
                    .occlude()
                    .child(creator.clone())
            }))
            .children(self.folder_editor.as_ref().map(|editor| {
                div()
                    .absolute()
                    .inset_0()
                    .size_full()
                    .p(px(16.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(gpui::rgba(0x00000099))
                    .occlude()
                    .child(editor.clone())
            }))
            .children(self.notice.as_ref().map(|notice| {
                div()
                    .absolute()
                    .top(px(31.0))
                    .right(px(8.0))
                    .max_w(px(520.0))
                    .p(px(8.0))
                    .bg(rgb(colors.panel_active))
                    .text_color(rgb(colors.gold))
                    .child(notice.clone())
            }))
            .children(
                cx.has_active_drag()
                    .then(|| {
                        [
                            DockSide::Left,
                            DockSide::Right,
                            DockSide::Top,
                            DockSide::Bottom,
                        ]
                        .into_iter()
                        .map(|side| self.render_root_dock(side, show_window_controls, cx))
                        .collect::<Vec<_>>()
                    })
                    .unwrap_or_default(),
            )
    }
}

fn pane_menu_heading(label: &'static str, colors: Palette) -> AnyElement {
    div()
        .px(px(8.0))
        .py(px(6.0))
        .border_t_1()
        .border_color(rgb(colors.divider))
        .text_color(rgb(colors.muted))
        .text_size(px(9.0))
        .child(label)
        .into_any_element()
}

/// Whether a pane is locked. The user's explicit set wins (an empty set unlocks
/// everything); until they choose, the first pane is locked while it holds a
/// navigation panel (Projects, Worktrees or Files).
fn pane_lock_state(
    explicit: Option<&HashSet<PaneId>>,
    pane_id: PaneId,
    first_pane: PaneId,
    holds_navigation_panel: impl FnOnce() -> bool,
) -> bool {
    match explicit {
        Some(locked) => locked.contains(&pane_id),
        None => first_pane == pane_id && holds_navigation_panel(),
    }
}

/// What the user asked to close.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UserClose {
    /// One tab: the tab X or Cmd+W.
    Tab,
    /// A whole pane and every tab in it: Close pane or Cmd+Shift+W.
    Pane,
}

/// Why a close the user asked for must not happen, or `None` when it may. A locked
/// pane keeps its tabs and stays a pane. Only requests from the user ask this:
/// moving a tab to another pane, project switches, restore and layout repair remove
/// tabs on their own terms, and a locked pane never blocks them.
fn user_close_refusal(pane_locked: bool, close: UserClose) -> Option<&'static str> {
    pane_locked.then_some(match close {
        UserClose::Tab => "Unlock the pane to close its tabs",
        UserClose::Pane => "Unlock the pane to close it",
    })
}

fn is_locked_close_hint(notice: &str) -> bool {
    [UserClose::Tab, UserClose::Pane]
        .into_iter()
        .any(|close| user_close_refusal(true, close) == Some(notice))
}

/// The full name of a built-in panel, for the tooltip of its icon-only tab.
fn panel_tooltip(panel: PanelKind) -> &'static str {
    match panel {
        PanelKind::Projects => "Projects · ⌘B",
        PanelKind::Worktrees => "Worktrees",
        PanelKind::Files => "Files · ⌘⇧E",
        PanelKind::Tasks => "Tasks",
        PanelKind::Shells => "Shells",
        PanelKind::Usage => "Usage",
        PanelKind::Settings => "Settings · ⌘,",
        PanelKind::ProjectSettings => "Project Settings · ⌘⌥A",
        PanelKind::Schedules => "Schedules · ⌘⇧S",
    }
}

fn window_controls_visible(window: &Window) -> bool {
    !matches!(window.window_bounds(), WindowBounds::Fullscreen(_))
}

fn window_controls_island(cx: &App) -> impl IntoElement {
    let colors = theme::palette(cx);
    div()
        .id("window-controls-island")
        .absolute()
        .top_0()
        .left_0()
        .w(px(WINDOW_CONTROLS_WIDTH))
        .h(px(WINDOW_CONTROLS_HEIGHT))
        .rounded_br(px(8.0))
        .bg(rgb(colors.panel_active))
        .border_r_1()
        .border_b_1()
        .border_color(rgb(colors.divider))
        .on_mouse_down(MouseButton::Left, start_window_drag)
}

fn start_window_drag(event: &gpui::MouseDownEvent, window: &mut Window, cx: &mut App) {
    cx.stop_propagation();
    if event.click_count == 2 {
        window.titlebar_double_click();
    } else {
        window.start_window_move();
    }
}

fn session_belongs_to_workspace(shell: &ShellSession, project_id: &str) -> bool {
    shell.project_id.as_deref() == Some(project_id)
        || (shell.kind == ShellKind::Orchestrator && shell.project_id.is_none())
}

fn orchestrator_tab_title(shell: &ShellSession) -> String {
    if shell.project_id.is_some() {
        "P·ORCH · PROJECT"
    } else {
        "G·ORCH · GLOBAL"
    }
    .to_owned()
}

fn harness_name(harness: HarnessKind) -> &'static str {
    match harness {
        HarnessKind::Codex => "codex",
        HarnessKind::Claude => "claude",
        HarnessKind::Grok => "grok",
    }
}

fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn request_codex_usage(force: bool, cx: &mut App) {
    for home in codex_usage_profiles(cx).into_keys() {
        request_codex_usage_at(home, force, cx);
    }
}

fn selected_codex_home(cx: &App) -> Option<PathBuf> {
    let selected = cx.global::<Settings>().selected_codex_account.as_deref();
    if let Some(snapshot) = &cx.global::<settings::CodexAccountsState>().snapshot {
        snapshot
            .accounts
            .iter()
            .find(|account| {
                account.available
                    && match selected {
                        Some(id) => account.id == id,
                        None => account.is_system_default,
                    }
            })
            .map(|account| account.home.clone())
    } else if selected.is_none() {
        codex_accounts::default_codex_home().ok()
    } else {
        None
    }
}

fn project_selected_codex_home(workspace: &Workspace, cx: &App) -> Option<PathBuf> {
    match workspace
        .state
        .projects
        .iter()
        .find(|project| project.id == workspace.project_id)
        .map(|project| &project.codex_account)
    {
        Some(ProjectCodexAccount::SystemDefault) => codex_accounts::default_codex_home().ok(),
        Some(ProjectCodexAccount::Saved(id)) => cx
            .global::<settings::CodexAccountsState>()
            .snapshot
            .as_ref()?
            .accounts
            .iter()
            .find(|account| account.available && account.id == *id)
            .map(|account| account.home.clone()),
        _ => selected_codex_home(cx),
    }
}

fn codex_usage_profiles(cx: &App) -> BTreeMap<PathBuf, String> {
    if let Some(snapshot) = &cx.global::<settings::CodexAccountsState>().snapshot {
        snapshot
            .accounts
            .iter()
            .filter(|account| account.available)
            .map(|account| (account.home.clone(), account.label.clone()))
            .collect()
    } else {
        codex_accounts::default_codex_home()
            .ok()
            .map(|home| BTreeMap::from([(home, "System default".to_owned())]))
            .unwrap_or_default()
    }
}

fn request_codex_usage_at(home: PathBuf, force: bool, cx: &mut App) {
    let now = unix_time();
    {
        let cache = cx
            .global_mut::<AccountUsage>()
            .codex
            .entry(home.clone())
            .or_default();
        let interval = usage::codex_refresh_interval(cache.codex.is_some(), cache.failures);
        if cache.pending
            || (!force
                && cache.last_attempt != 0
                && now.saturating_sub(cache.last_attempt) < interval)
        {
            return;
        }
        cache.pending = true;
        cache.last_attempt = now;
    }
    let read_home = home.clone();
    let task = cx
        .background_executor()
        .spawn(async move { usage::read_codex_usage_at(&read_home) });
    cx.spawn(async move |cx| {
        let result = task.await;
        let _ = cx.update(|cx| {
            let cache = cx
                .global_mut::<AccountUsage>()
                .codex
                .entry(home)
                .or_default();
            cache.pending = false;
            match result {
                Ok(snapshot) => {
                    cache.codex = Some(snapshot);
                    cache.codex_error = None;
                    cache.failures = 0;
                }
                Err(error) => {
                    cache.codex_error = Some(error);
                    cache.failures = cache.failures.saturating_add(1);
                }
            }
            cx.refresh_windows();
        });
    })
    .detach();
}

fn usage_summary(provider: &str, snapshot: &ProviderUsage) -> String {
    let stale = unix_time().saturating_sub(snapshot.updated_at_unix) > 15 * 60;
    let limits = snapshot
        .windows
        .iter()
        .take(2)
        .map(|window| {
            format!(
                "{} {:.0}% left",
                window.label,
                (100.0 - window.used_percent).clamp(0.0, 100.0)
            )
        })
        .collect::<Vec<_>>()
        .join(" / ");
    format!(
        "{provider}{} · {}",
        if stale { "*" } else { "" },
        if limits.is_empty() { "—" } else { &limits }
    )
}

fn reset_summary(resets_at: Option<u64>) -> String {
    let Some(deadline) = resets_at else {
        return "Reset unavailable".to_owned();
    };
    let seconds = deadline.saturating_sub(unix_time());
    if seconds == 0 {
        "Awaiting reset update".to_owned()
    } else if seconds >= 86400 {
        format!(
            "Resets in {}d {}h",
            seconds / 86400,
            (seconds % 86400) / 3600
        )
    } else if seconds >= 3600 {
        format!("Resets in {}h {}m", seconds / 3600, (seconds % 3600) / 60)
    } else {
        format!("Resets in {}m", seconds.div_ceil(60))
    }
}

fn ago(seconds: u64) -> String {
    match seconds {
        0..60 => "just now".to_owned(),
        60..3600 => format!("{}m ago", seconds / 60),
        3600..86400 => format!("{}h ago", seconds / 3600),
        _ => format!("{}d ago", seconds / 86400),
    }
}

fn token_breakdown(tokens: &usage::TokenCounts) -> String {
    use usage::format_tokens;
    format!(
        "in {} · out {} · cached {} · cache writes {} · reasoning {}",
        format_tokens(tokens.input),
        format_tokens(tokens.output),
        format_tokens(tokens.cached_read),
        format_tokens(tokens.cache_creation),
        format_tokens(tokens.reasoning),
    )
}

fn render_grok_session(
    title: &str,
    tab: Option<&usage::GrokTabUsage>,
    colors: Palette,
) -> AnyElement {
    let card = div()
        .p(px(12.0))
        .border_t_1()
        .border_color(rgb(colors.divider));
    let Some(tab) = tab else {
        return card
            .child(div().text_color(rgb(colors.cyan)).child(title.to_owned()))
            .child(
                div()
                    .mt(px(6.0))
                    .text_color(rgb(colors.muted))
                    .child("Reading Grok usage…"),
            )
            .into_any_element();
    };
    let Some(session) = &tab.usage else {
        return card
            .child(div().text_color(rgb(colors.cyan)).child(title.to_owned()))
            .child(
                div()
                    .mt(px(6.0))
                    .text_color(rgb(colors.muted))
                    .child(format!(
                        "usage unavailable: {}",
                        tab.error.as_deref().unwrap_or("Grok has not reported it")
                    )),
            )
            .into_any_element();
    };
    let now = unix_time();
    let headline = [
        session.cost_usd.map(usage::format_usd),
        Some(format!(
            "{} tokens",
            usage::format_tokens(session.tokens.total)
        )),
        Some(format!(
            "{} turn{}",
            session.turns,
            if session.turns == 1 { "" } else { "s" }
        )),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ");
    let read_age = now.saturating_sub(tab.fetched_at_unix);
    let mut freshness = format!("Updated {}", ago(read_age));
    if let Some(activity) = session.updated_at_unix {
        freshness.push_str(&format!(
            " · last activity {}",
            ago(now.saturating_sub(activity))
        ));
    }
    card.child(
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(8.0))
            .child(div().text_color(rgb(colors.cyan)).child(title.to_owned()))
            .child(
                div()
                    .text_color(rgb(colors.muted))
                    .text_size(px(9.0))
                    .child(
                        session
                            .primary_model
                            .clone()
                            .unwrap_or_else(|| "model unknown".to_owned()),
                    ),
            ),
    )
    .child(div().mt(px(6.0)).child(headline))
    .child(
        div()
            .mt(px(4.0))
            .text_color(rgb(colors.muted))
            .text_size(px(9.0))
            .child(token_breakdown(&session.tokens)),
    )
    .children((session.models.len() > 1).then(|| {
        div()
            .mt(px(4.0))
            .text_color(rgb(colors.muted))
            .text_size(px(9.0))
            .children(session.models.iter().map(|model| {
                div().child(format!(
                    "{} · {} tokens · {} calls{}",
                    model.model,
                    usage::format_tokens(model.tokens.total),
                    model.model_calls,
                    model
                        .cost_usd
                        .map(|cost| format!(" · {}", usage::format_usd(cost)))
                        .unwrap_or_default()
                ))
            }))
    }))
    .child(
        div()
            .mt(px(4.0))
            .text_color(rgb(if tab.stale { colors.gold } else { colors.muted }))
            .text_size(px(9.0))
            .child(if tab.stale {
                format!(
                    "STALE · {} · {}",
                    freshness,
                    tab.error.as_deref().unwrap_or("latest read failed")
                )
            } else {
                freshness
            }),
    )
    .into_any_element()
}

fn render_grok_total(totals: &usage::GrokTotals, missing: usize, colors: Palette) -> AnyElement {
    div()
        .p(px(12.0))
        .border_t_1()
        .border_color(rgb(colors.divider))
        .child(div().text_color(rgb(colors.cyan)).child("GROK · TOTAL"))
        .child(div().mt(px(6.0)).child(format!(
            "{} · {} tokens · {} session{}",
            usage::format_usd(totals.cost_usd),
            usage::format_tokens(totals.tokens.total),
            totals.sessions,
            if totals.sessions == 1 { "" } else { "s" }
        )))
        .child(
            div()
                .mt(px(4.0))
                .text_color(rgb(colors.muted))
                .text_size(px(9.0))
                .child(token_breakdown(&totals.tokens)),
        )
        .child(
            div()
                .mt(px(4.0))
                .text_color(rgb(colors.muted))
                .text_size(px(9.0))
                .child(
                    "A session resumed or forked from another includes that history, so the total can overcount.",
                ),
        )
        .children((missing > 0).then(|| {
            div()
                .mt(px(4.0))
                .text_color(rgb(colors.muted))
                .text_size(px(9.0))
                .child(format!(
                    "{missing} listed session{} without usage not counted",
                    if missing == 1 { "" } else { "s" }
                ))
        }))
        .into_any_element()
}

fn render_provider_usage(snapshot: &ProviderUsage, title: &str, colors: Palette) -> AnyElement {
    let age = unix_time().saturating_sub(snapshot.updated_at_unix);
    let limits = snapshot
        .windows
        .iter()
        .map(|window| {
            let remaining = (100.0 - window.used_percent).clamp(0.0, 100.0);
            div()
                .mt(px(10.0))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(8.0))
                        .child(format!("{} · {:.0}% left", window.label, remaining))
                        .child(
                            div()
                                .text_color(rgb(colors.muted))
                                .text_size(px(9.0))
                                .child(reset_summary(window.resets_at)),
                        ),
                )
                .child(
                    div().mt(px(5.0)).h(px(3.0)).bg(rgb(colors.divider)).child(
                        div()
                            .h_full()
                            .w(gpui::relative((remaining / 100.0) as f32))
                            .bg(rgb(if remaining < 15.0 {
                                colors.magenta
                            } else {
                                colors.cyan
                            })),
                    ),
                )
                .into_any_element()
        })
        .collect::<Vec<_>>();
    div()
        .p(px(12.0))
        .border_t_1()
        .border_color(rgb(colors.divider))
        .child(div().text_color(rgb(colors.cyan)).child(title.to_owned()))
        .children(snapshot.account_label.as_ref().map(|label| {
            div()
                .mt(px(4.0))
                .text_color(rgb(colors.muted))
                .child(label.clone())
        }))
        .child(
            div()
                .mt(px(4.0))
                .text_color(rgb(if age > 900 { colors.gold } else { colors.muted }))
                .text_size(px(9.0))
                .child(format!(
                    "{}Updated {}m ago",
                    if age > 900 { "STALE · " } else { "" },
                    age / 60
                )),
        )
        .children(limits)
        .children(snapshot.windows.is_empty().then(|| {
            div()
                .mt(px(8.0))
                .text_color(rgb(colors.muted))
                .child("Quota windows unavailable")
        }))
        .children(snapshot.context_used_percent.map(|used| {
            div()
                .mt(px(10.0))
                .text_color(rgb(colors.muted))
                .child(format!("Context · {used:.0}% used"))
        }))
        .children(snapshot.session_cost_usd.map(|cost| {
            div()
                .mt(px(5.0))
                .text_color(rgb(colors.muted))
                .child(format!("Estimated session cost · ${cost:.2}"))
        }))
        .into_any_element()
}

fn sync_appearance(cx: &mut App) {
    let selected = cx.global::<Settings>().theme;
    // Presets are static; native configuration is resolved again to pick up edits,
    // including recursive config files and custom theme files. The parse itself is
    // skipped while none of those files changed.
    if selected != ThemeChoice::Ghostty && cx.global::<Appearance>().selected == selected {
        return;
    }
    let Some(appearance) = theme::refresh_appearance(selected, cx) else {
        return;
    };
    if &appearance != cx.global::<Appearance>() {
        cx.set_global(appearance);
    }
}

fn exit_startup(error: String) -> ! {
    eprintln!("riwork: {error}");
    std::process::exit(2);
}

fn main() {
    let args = env::args().skip(1).collect::<Vec<_>>();
    match cli::run_cli(&args) {
        Ok(true) => return,
        Ok(false) => {}
        Err(error) => {
            if !error.is_empty() {
                eprintln!("riwork: {error}");
            }
            std::process::exit(2);
        }
    }
    if let Err(error) = paths::ensure_default_projects_directory() {
        eprintln!("riwork: {error}");
        std::process::exit(2);
    }
    let startup_path = args.first().map(PathBuf::from).filter(|path| path.is_dir());
    let fallback_cwd = env::current_dir()
        .ok()
        .filter(|path| path != Path::new("/"))
        .or_else(|| {
            env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_dir())
        })
        .unwrap_or_else(|| PathBuf::from("."));
    // None of these can be worked around, but a message beats a panic backtrace.
    let runtime =
        runtime::RuntimeManager::open_default().unwrap_or_else(|error| exit_startup(error));
    let restore = runtime
        .restore_from_env()
        .unwrap_or_else(|error| exit_startup(error));
    let state_home = SessionManager::open_default()
        .unwrap_or_else(|error| exit_startup(error))
        .state_home()
        .to_path_buf();
    let mut registration = runtime
        .register(state_home.clone())
        .unwrap_or_else(|error| exit_startup(error));
    // A replacement that cannot finish restoring ends itself; see the watchdog.
    let watchdog = restore
        .as_ref()
        .map(|snapshot| registration.start_restore_watchdog(snapshot));

    application().run(move |cx: &mut App| {
        cx.set_app_identity("dev.riwork.shell", "RiWork");
        schedules::start(state_home.clone(), cx);
        notifications::start(state_home, cx);
        cx.on_system_notification_response(|response, cx| {
            if response.action_id.as_ref().is_some_and(|action| action.as_ref() != "open") { return; }
            if let Some((project_id, shell_id)) = notifications::response_target(&response.tag) {
                if let Err(error) = open_completed_agent(project_id, shell_id, cx) {
                    eprintln!("riwork notifications: {error}");
                }
            }
        });
        cx.set_global(AccountUsage::default());
        cx.set_global(settings::CodexAccountsState::default());
        cx.set_global(CuaSetupState::default());
        settings::refresh_cua_status(cx);
        let settings = SettingsStore::open_default()
            .and_then(|store| store.load())
            .unwrap_or_else(|error| {
                eprintln!("riwork: {error}");
                Settings::default()
            });
        cx.set_global(Appearance::resolve(settings.theme));
        cx.set_global(settings);
        settings::refresh_codex_accounts(cx);
        cx.spawn(async move |cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                cx.update(sync_appearance);
            }
        })
        .detach();
        cx.spawn(async move |cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_secs(15))
                    .await;
                cx.update(|cx| {
                    let state = cx.global::<CuaSetupState>();
                    if state.status.as_ref().is_none_or(|status| !status.ready) {
                        settings::refresh_cua_status(cx);
                    }
                });
            }
        })
        .detach();
        cx.on_action(|_: &Quit, cx| {
            for handle in cx.windows() {
                if let Some(handle) = handle.downcast::<Workspace>() {
                    let _ = handle.update(cx, |workspace, _, _| workspace.save_layout());
                }
            }
            cx.quit();
        });
        cx.bind_keys([
            KeyBinding::new("cmd-,", OpenSettings, None),
            KeyBinding::new("cmd-alt-a", OpenProjectSettings, None),
            KeyBinding::new("cmd-shift-s", OpenSchedules, None),
            KeyBinding::new("cmd-shift-e", OpenFiles, None),
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-n", CreateProject, None),
            KeyBinding::new("cmd-t", NewTab, None),
            KeyBinding::new("cmd-d", SplitRight, None),
            KeyBinding::new("cmd-shift-d", SplitDown, None),
            KeyBinding::new("cmd-w", CloseTab, None),
            KeyBinding::new("cmd-shift-w", ClosePane, None),
            KeyBinding::new("ctrl-tab", NextTab, None),
            KeyBinding::new("ctrl-shift-tab", PreviousTab, None),
            KeyBinding::new("cmd-b", ToggleSidebar, None),
            KeyBinding::new("cmd-f", FocusSearch, None),
            KeyBinding::new("cmd-shift-f", ToggleFocusMode, None),
            KeyBinding::new("cmd-shift-o", OpenOrchestrator, None),
            KeyBinding::new("cmd-alt-o", OpenProjectOrchestrator, None),
            KeyBinding::new("cmd-shift-n", NewProjectWindow, None),
            KeyBinding::new("cmd-shift-c", OpenCodex, None),
            KeyBinding::new("cmd-shift-l", OpenClaude, None),
            KeyBinding::new("cmd-shift-g", OpenGrok, None),
        ]);
        cx.set_menus([Menu::new("RiWork").items([
            MenuItem::action("Settings…", OpenSettings),
            MenuItem::action("Project Settings…", OpenProjectSettings),
            MenuItem::action("Schedules", OpenSchedules),
            MenuItem::separator(),
            MenuItem::action("Quit RiWork", Quit),
        ])]);
        dock_menu::init(cx);
        tooltip::init(cx);
        cx.on_window_closed(|cx, window| {
            // A hint window is not a reason to stay open.
            if cx.windows().iter().all(tooltip::is_popup) {
                cx.quit();
            } else {
                dock_menu::window_closed(window, cx);
            }
        })
        .detach();
        if let Some(snapshot) = &restore {
            for window in &snapshot.windows {
                if let Err(error) = open_workspace_window(
                    Some(window.path.clone()),
                    fallback_cwd.clone(),
                    Some(window.clone()),
                    cx,
                ) {
                    // The previous app stays open until every window is restored.
                    eprintln!("riwork: cannot restore a window: {error}");
                    cx.quit();
                    return;
                }
            }
        } else if let Err(error) =
            open_startup_window(startup_path.clone(), fallback_cwd.clone(), cx)
        {
            exit_startup(error);
        }
        // The heartbeat below publishes again and reports failures.
        if let Err(error) = registration.publish_windows(runtime_windows(cx, false)) {
            eprintln!("riwork: {error}");
        }
        cx.spawn(async move |cx| {
            let mut reload = None;
            let mut restore = restore;
            let mut watchdog = watchdog;
            let restore_started = Instant::now();
            let mut ticks = 0u32;
            let mut publish_due = false;
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                ticks = ticks.wrapping_add(1);
                let _ = cx.update(|cx| {
                    if let Some(snapshot) = &restore {
                        if restore_started.elapsed() > Duration::from_secs(20) {
                            eprintln!("riwork: window restoration timed out; keeping the previous app open");
                            cx.quit();
                            return;
                        }
                        let windows = runtime_windows(cx, false);
                        if windows.len() == snapshot.windows.len()
                            && windows.iter().all(|window| window.layout.is_some())
                        {
                            // The watchdog may already be ending this process,
                            // which must then not confirm anything.
                            if watchdog.as_ref().is_some_and(|w| !w.begin_confirm()) {
                                return;
                            }
                            if let Err(error) = registration
                                .publish_windows(windows)
                                .and_then(|_| runtime.mark_restore_ready(snapshot, &registration))
                            {
                                eprintln!("riwork: {error}");
                                if let Some(watchdog) = &watchdog {
                                    watchdog.abort_confirm();
                                }
                                cx.quit();
                                return;
                            }
                            if let Some(watchdog) = watchdog.take() {
                                watchdog.finish_confirm();
                            }
                            restore = None;
                        } else {
                            return;
                        }
                    }
                    if let Some(launch) = &reload {
                        match registration.reload_ready(launch) {
                            Ok(true) => cx.quit(),
                            Ok(false) => {}
                            Err(error) => {
                                eprintln!("riwork: {error}");
                                reload = None;
                            }
                        }
                        return;
                    }
                    // The registry only feeds `riwork instances` and reload
                    // reports (a reload snapshots the windows itself), so it is
                    // refreshed every 2 s, written only on change, and never waits
                    // for its lock on this thread: a busy lock stays due for the
                    // next tick.
                    publish_due |= ticks.is_multiple_of(4);
                    if publish_due {
                        match registration.try_publish_windows(runtime_windows(cx, false)) {
                            Ok(true) => publish_due = false,
                            Ok(false) => {}
                            Err(error) => {
                                publish_due = false;
                                eprintln!("riwork: {error}");
                            }
                        }
                    }
                    match registration.pending_reload() {
                        Ok(Some(request)) => {
                            match registration.launch_reload(&request, runtime_windows(cx, true)) {
                                Ok(launch) => reload = Some(launch),
                                Err(error) => {
                                    let _ = registration.fail_reload(&request, &error);
                                    eprintln!("riwork: {error}");
                                }
                            }
                        }
                        Ok(None) => {}
                        Err(error) => eprintln!("riwork: {error}"),
                    }
                });
            }
        })
        .detach();
        cx.activate(true);
    });
}

fn open_completed_agent(project_id: &str, shell_id: &str, cx: &mut App) -> Result<(), String> {
    let project = Store::open_default()?
        .snapshot()?
        .project(project_id)?
        .clone();
    let session = SessionManager::open_default()?.registered_session(shell_id)?;
    if session.project_id.as_deref() != Some(project_id) {
        return Err("This agent no longer belongs to that project".to_owned());
    }
    for handle in cx.windows() {
        let Some(handle) = handle.downcast::<Workspace>() else {
            continue;
        };
        let opened = handle
            .update(cx, |workspace, window, cx| {
                if workspace.project_id != project_id {
                    return false;
                }
                workspace.show_shell(shell_id, window, cx);
                window.activate_window();
                true
            })
            .map_err(|error| error.to_string())?;
        if opened {
            cx.activate(true);
            return Ok(());
        }
    }
    let handle = open_workspace_window(Some(project.root.clone()), project.root, None, cx)?;
    handle
        .update(cx, |workspace, window, cx| {
            workspace.show_shell(shell_id, window, cx);
            window.activate_window();
        })
        .map_err(|error| error.to_string())?;
    cx.activate(true);
    Ok(())
}

fn runtime_windows(cx: &mut App, save: bool) -> Vec<runtime::RuntimeWindow> {
    cx.windows()
        .into_iter()
        .filter_map(|handle| {
            let handle = handle.downcast::<Workspace>()?;
            handle
                .update(cx, |workspace, window, _| {
                    if save {
                        workspace.save_layout();
                    }
                    workspace.runtime_window(window)
                })
                .ok()
        })
        .collect()
}

/// The window's frame when it is an ordinary window, None in full screen or
/// zoomed. GPUI reports a zoomed macOS window as plain Windowed, so it also asks
/// `is_maximized`; that covers the app's own titlebar double-click.
fn normal_window_bounds(window: &Window) -> Option<Bounds<Pixels>> {
    match window.window_bounds() {
        WindowBounds::Windowed(bounds) if !window.is_maximized() => Some(bounds),
        _ => None,
    }
}

/// Opens the first window of an ordinary launch. A folder argument that cannot be
/// opened falls back to the active project, and the window says why.
fn open_startup_window(
    startup_path: Option<PathBuf>,
    fallback_cwd: PathBuf,
    cx: &mut App,
) -> Result<(), String> {
    let error = match open_workspace_window(startup_path.clone(), fallback_cwd.clone(), None, cx) {
        Ok(_) => return Ok(()),
        Err(error) if startup_path.is_some() => error,
        Err(error) => return Err(error),
    };
    eprintln!("riwork: {error}");
    let handle = open_workspace_window(None, fallback_cwd, None, cx)?;
    let _ = handle.update(cx, |workspace, _, cx| {
        workspace.notice = Some(error);
        cx.notify();
    });
    Ok(())
}

fn open_workspace_window(
    startup_path: Option<PathBuf>,
    fallback_cwd: PathBuf,
    restore: Option<runtime::RuntimeWindow>,
    cx: &mut App,
) -> Result<WindowHandle<Workspace>, String> {
    let startup = WorkspaceStartup::prepare(startup_path, fallback_cwd)?;
    // Best effort: an unreadable layout only means the default size.
    let saved_size = cx
        .global::<Settings>()
        .remember_window_size
        .then(|| startup.layouts.window_size(&startup.project.id))
        .flatten()
        .unwrap_or(WindowSize {
            width: 1220.0,
            height: 780.0,
        });
    let cascade = cx
        .windows()
        .iter()
        .filter(|handle| !tooltip::is_popup(handle))
        .count();
    let frame = match cx.primary_display().map(|display| display.visible_bounds()) {
        Some(display) => layouts::WindowFrame::opening(
            saved_size,
            layouts::WindowFrame {
                x: display.origin.x.as_f32(),
                y: display.origin.y.as_f32(),
                width: display.size.width.as_f32(),
                height: display.size.height.as_f32(),
            },
            cascade,
        ),
        None => layouts::WindowFrame {
            x: 0.0,
            y: 0.0,
            width: saved_size.width,
            height: saved_size.height,
        },
    };
    let mut bounds = Bounds::new(
        point(px(frame.x), px(frame.y)),
        size(px(frame.width), px(frame.height)),
    );
    if let Some(saved) = restore.as_ref().and_then(|window| window.bounds.as_ref()) {
        bounds.origin = point(px(saved.x), px(saved.y));
        bounds.size = size(px(saved.width), px(saved.height));
    }
    let window_bounds = match restore
        .as_ref()
        .map(|window| window.mode)
        .unwrap_or_default()
    {
        runtime::WindowMode::Windowed => WindowBounds::Windowed(bounds),
        runtime::WindowMode::Maximized => WindowBounds::Maximized(bounds),
        runtime::WindowMode::Fullscreen => WindowBounds::Fullscreen(bounds),
    };
    cx.open_window(
        WindowOptions {
            window_bounds: Some(window_bounds),
            titlebar: Some(TitlebarOptions {
                appears_transparent: true,
                traffic_light_position: Some(point(px(10.0), px(8.0))),
                ..Default::default()
            }),
            app_owns_titlebar_drag: true,
            window_min_size: Some(size(px(640.0), px(400.0))),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| Workspace::new(startup, restore, window, cx)),
    )
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod startup_tests {
    use super::*;

    fn startup_at(
        home: &Path,
        startup_path: Option<PathBuf>,
        fallback_cwd: PathBuf,
    ) -> Result<WorkspaceStartup, String> {
        WorkspaceStartup::resolve(
            Store::open(home)?,
            LayoutStore::open(home)?,
            SettingsStore::open(home)?,
            SessionManager::at(home.to_path_buf())?,
            startup_path,
            fallback_cwd,
        )
    }

    #[test]
    fn a_vanished_project_root_is_an_error_for_the_caller_not_a_panic() {
        let base = std::env::temp_dir().join(format!("riwork-startup-{}", uuid::Uuid::new_v4()));
        let home = base.join("home");
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let project = project.canonicalize().unwrap();

        let opened = startup_at(&home, Some(project.clone()), base.clone()).unwrap();
        assert_eq!(opened.project.root, project);
        // The same folder opens the already registered project.
        let again = startup_at(&home, Some(project.clone()), base.clone()).unwrap();
        assert_eq!(again.project.id, opened.project.id);

        std::fs::remove_dir_all(&project).unwrap();
        let error = startup_at(&home, Some(project.clone()), base.clone())
            .err()
            .expect("a missing folder cannot be opened");
        assert!(error.contains("Cannot open project folder"), "{error}");

        // Without a folder argument the active project is used, whatever became of it.
        let active = startup_at(&home, None, base.clone()).unwrap();
        assert_eq!(active.project.id, opened.project.id);
        std::fs::remove_dir_all(base).unwrap();
    }
}

#[cfg(test)]
mod workspace_tab_tests {
    use super::*;

    #[test]
    fn a_locked_pane_refuses_user_closes_but_an_unlocked_one_allows_them() {
        for close in [UserClose::Tab, UserClose::Pane] {
            assert_eq!(user_close_refusal(false, close), None);
            assert!(user_close_refusal(true, close).is_some());
        }
        assert_ne!(
            user_close_refusal(true, UserClose::Tab),
            user_close_refusal(true, UserClose::Pane)
        );
        // Only the hints this decision produces are treated as stale on unlock.
        for close in [UserClose::Tab, UserClose::Pane] {
            assert!(is_locked_close_hint(
                user_close_refusal(true, close).unwrap()
            ));
        }
        assert!(!is_locked_close_hint("Copied /tmp/file"));
    }

    #[test]
    fn lock_state_follows_the_users_choice_and_defaults_to_the_navigation_pane() {
        let nav = || true;
        let none = || false;
        // Before any choice the first pane is locked only while it holds a navigation panel.
        assert!(pane_lock_state(None, 1, 1, nav));
        assert!(!pane_lock_state(None, 1, 1, none));
        assert!(!pane_lock_state(None, 2, 1, nav));
        // An explicit set decides on its own, whatever the panes hold.
        let locked = HashSet::from([2]);
        assert!(pane_lock_state(Some(&locked), 2, 1, none));
        assert!(!pane_lock_state(Some(&locked), 1, 1, nav));
        // Unlocking the default navigation pane stores an empty set and stays unlocked.
        assert!(!pane_lock_state(Some(&HashSet::new()), 1, 1, nav));
        // A default-locked pane refuses to be closed; once unlocked it can be.
        let default_locked = pane_lock_state(None, 1, 1, nav);
        assert!(user_close_refusal(default_locked, UserClose::Tab).is_some());
        let unlocked = pane_lock_state(Some(&HashSet::new()), 1, 1, nav);
        assert!(user_close_refusal(unlocked, UserClose::Tab).is_none());
    }

    #[test]
    fn icon_only_panel_tabs_name_their_panel_in_the_tooltip() {
        let panels = [
            (PanelKind::Projects, "Projects"),
            (PanelKind::Worktrees, "Worktrees"),
            (PanelKind::Files, "Files"),
            (PanelKind::Tasks, "Tasks"),
            (PanelKind::Shells, "Shells"),
            (PanelKind::Usage, "Usage"),
            (PanelKind::Settings, "Settings"),
            (PanelKind::ProjectSettings, "Project Settings"),
            (PanelKind::Schedules, "Schedules"),
        ];
        for (panel, name) in panels {
            assert!(panel_tooltip(panel).starts_with(name), "{panel:?}");
            assert_eq!(
                Workspace::panel_title(panel),
                name.to_uppercase(),
                "the tooltip names the tab's own label"
            );
        }
    }

    #[test]
    fn saved_file_reselects_its_live_editor_without_refresh_but_new_launch_stays_strict() {
        use std::{fs, os::unix::fs::symlink};

        let directory =
            std::env::temp_dir().join(format!("riwork-editor-reselect-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("café space.txt");
        fs::write(&path, "before\n").unwrap();
        let old = file_preview::FileIdentity::of(&fs::metadata(&path).unwrap());
        let root = ExplorerRoot {
            path: directory.clone(),
            label: "fixture".into(),
            worktree_id: Some("worktree-a".into()),
        };
        let mut editor = session(ShellKind::Project, Some("project-a"));
        editor.worktree_id = root.worktree_id.clone();
        editor.editor_path = Some(directory.canonicalize().unwrap().join("café space.txt"));

        // Saving updates the file without refreshing the explorer's old row.
        fs::write(&path, "after save, with a different length\n").unwrap();
        assert_ne!(
            old,
            file_preview::FileIdentity::of(&fs::metadata(&path).unwrap())
        );
        assert!(file_preview::validated_editor_path(&directory, &path, old).is_err());
        assert!(matches!(
            file_editor_target(&root, &path, old, "project-a", &[editor.clone()]),
            Ok(FileEditorTarget::Existing(shell)) if shell.id == editor.id
        ));

        // The project root and its primary worktree are one directory, so an
        // editor opened from either is the same editor.
        for editor_worktree in [None, Some("primary".to_owned())] {
            for root_worktree in [None, Some("worktree-a".to_owned())] {
                let root = ExplorerRoot {
                    worktree_id: root_worktree,
                    ..root.clone()
                };
                let shell = ShellSession {
                    worktree_id: editor_worktree.clone(),
                    ..editor.clone()
                };
                assert!(matches!(
                    file_editor_target(&root, &path, old, "project-a", std::slice::from_ref(&shell)),
                    Ok(FileEditorTarget::Existing(found)) if found.id == shell.id
                ));
            }
        }

        // A dead editor, another project, or another worktree's copy of the
        // file cannot bypass the strict identity check for a new session.
        let other_worktree = directory.join("other-worktree");
        fs::create_dir(&other_worktree).unwrap();
        fs::write(other_worktree.join("café space.txt"), "before\n").unwrap();
        for (project_id, shells) in [
            ("project-a", vec![]),
            ("project-b", vec![editor.clone()]),
            (
                "project-a",
                vec![ShellSession {
                    alive: false,
                    ..editor.clone()
                }],
            ),
            (
                "project-a",
                vec![ShellSession {
                    worktree_id: Some("worktree-b".into()),
                    editor_path: Some(
                        other_worktree
                            .canonicalize()
                            .unwrap()
                            .join("café space.txt"),
                    ),
                    ..editor.clone()
                }],
            ),
        ] {
            assert!(file_editor_target(&root, &path, old, project_id, &shells).is_err());
        }

        // Even a forged live-editor match cannot follow a symlink.
        let link = directory.join("link.txt");
        symlink(&path, &link).unwrap();
        editor.editor_path = Some(directory.canonicalize().unwrap().join("link.txt"));
        assert!(file_editor_target(&root, &link, old, "project-a", &[editor]).is_err());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn closing_a_pane_focuses_the_pane_that_takes_its_space() {
        let pane = |id| Box::new(Layout::Pane(id));
        let split = |first: Box<Layout>, second: Box<Layout>| {
            Box::new(Layout::Split {
                axis: Axis::SideBySide,
                ratio: 0.5,
                first,
                second,
            })
        };
        // nav | (terminal | (top / bottom))
        let layout = Layout::Split {
            axis: Axis::SideBySide,
            ratio: 0.27,
            first: pane(1),
            second: split(pane(2), split(pane(3), pane(4))),
        };
        assert_eq!(pane_inheriting_space(&layout, 4), Some(3));
        assert_eq!(pane_inheriting_space(&layout, 3), Some(4));
        assert_eq!(pane_inheriting_space(&layout, 2), Some(3));
        assert_eq!(pane_inheriting_space(&layout, 1), Some(2));
        // The neighbour next to the closed pane, not the far end of a subtree.
        let layout = Layout::Split {
            axis: Axis::SideBySide,
            ratio: 0.5,
            first: split(pane(1), pane(2)),
            second: pane(3),
        };
        assert_eq!(pane_inheriting_space(&layout, 3), Some(2));
        assert_eq!(pane_inheriting_space(&layout, 1), Some(2));
        assert_eq!(pane_inheriting_space(&layout, 9), None);
        assert_eq!(pane_inheriting_space(&Layout::Pane(1), 1), None);
    }

    #[test]
    fn root_docking_targets_clear_the_window_controls_only_while_they_show() {
        assert_eq!(root_dock_insets(true), (80.0, 30.0));
        assert_eq!(root_dock_insets(false), (0.0, 12.0));
    }

    #[test]
    fn files_follow_the_selected_worktree_and_never_fall_back_from_an_unavailable_selection() {
        let state: State = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "active_project_id": "atlas",
            "projects": [
                {"id":"atlas", "name":"Atlas", "root":"/projects/atlas", "created_at":0},
                {"id":"beacon", "name":"Beacon", "root":"/projects/beacon", "created_at":0}
            ],
            "worktrees": [
                {"id":"main", "project_id":"atlas", "branch":"main", "path":"/projects/atlas", "is_primary":true,"created_at":0},
                {"id":"feature", "project_id":"atlas", "branch":"feature", "path":"/worktrees/feature", "created_at":0},
                {"id":"other", "project_id":"beacon", "branch":"main", "path":"/projects/beacon", "created_at":0}
            ],
            "tasks": []
        })).unwrap();
        assert_eq!(
            file_explorer_root(&state, "atlas", Some("feature"))
                .unwrap()
                .path,
            PathBuf::from("/worktrees/feature")
        );
        assert_eq!(
            file_explorer_root(&state, "atlas", Some("feature"))
                .unwrap()
                .worktree_id
                .as_deref(),
            Some("feature")
        );
        assert_eq!(
            file_explorer_root(&state, "beacon", Some("other"))
                .unwrap()
                .label,
            "Beacon · main"
        );
        assert!(file_explorer_root(&state, "atlas", Some("other")).is_none());
        assert!(file_explorer_root(&state, "atlas", Some("removed")).is_none());
        assert!(file_explorer_root(&state, "removed", None).is_none());
        assert_eq!(
            file_explorer_root(&state, "atlas", None).unwrap().path,
            PathBuf::from("/projects/atlas")
        );
        assert!(
            file_explorer_root(&state, "atlas", None)
                .unwrap()
                .worktree_id
                .is_none()
        );
    }

    #[test]
    fn terminal_palette_override_is_opt_in_and_preserves_user_configuration() {
        let original = Workspace::terminal_options("shell".to_owned(), PathBuf::from("/tmp"), None);
        assert_eq!(original.configuration, TerminalConfiguration::UserDefault);
        let themed = Workspace::terminal_options(
            "shell".to_owned(),
            PathBuf::from("/tmp"),
            Some(theme::riwork_terminal_theme()),
        );
        assert!(matches!(
            themed.configuration,
            TerminalConfiguration::UserDefaultWithOverride(_)
        ));
    }

    #[test]
    fn selected_theme_controls_terminals_and_keeps_legacy_preferences() {
        let mut settings = Settings::default();
        for selected in ThemeChoice::ALL
            .into_iter()
            .filter(|theme| *theme != ThemeChoice::Ghostty)
        {
            settings.theme = selected;
            let appearance = Appearance::resolve(selected);
            assert_eq!(
                Workspace::terminal_theme(&settings, &appearance),
                appearance.terminal
            );
            settings.use_riwork_colors = true;
            assert_eq!(
                Workspace::terminal_theme(&settings, &appearance),
                appearance.terminal
            );
        }
        let mut appearance = Appearance::resolve(ThemeChoice::RiWork);
        appearance.selected = ThemeChoice::Ghostty;
        appearance.terminal = None;
        settings.theme = ThemeChoice::Ghostty;
        assert_eq!(
            Workspace::terminal_theme(&settings, &appearance),
            Some(theme::riwork_terminal_theme())
        );
        settings.use_riwork_colors = false;
        assert_eq!(Workspace::terminal_theme(&settings, &appearance), None);
    }

    fn session(kind: ShellKind, project: Option<&str>) -> ShellSession {
        ShellSession {
            id: "scope-test".to_owned(),
            project_id: project.map(str::to_owned),
            worktree_id: None,
            kind,
            cwd: PathBuf::from("/tmp"),
            command: None,
            editor_path: None,
            harness: None,
            codex_account_id: None,
            codex_account_label: None,
            codex_account_email: None,
            codex_home: None,
            unrestricted: false,
            orchestrator_skill_loaded: false,
            orchestrator_skill_version: None,
            orchestrator_project_root: None,
            created_at_unix: 0,
            alive: true,
        }
    }

    #[test]
    fn restored_tabs_keep_global_and_project_orchestrators_in_their_scopes() {
        assert!(session_belongs_to_workspace(
            &session(ShellKind::Orchestrator, None),
            "alpha"
        ));
        assert!(session_belongs_to_workspace(
            &session(ShellKind::Orchestrator, Some("alpha")),
            "alpha"
        ));
        assert!(!session_belongs_to_workspace(
            &session(ShellKind::Orchestrator, Some("beta")),
            "alpha"
        ));
        assert!(session_belongs_to_workspace(
            &session(ShellKind::Project, Some("alpha")),
            "alpha"
        ));
        assert!(!session_belongs_to_workspace(
            &session(ShellKind::Project, Some("beta")),
            "alpha"
        ));
        assert!(!session_belongs_to_workspace(
            &session(ShellKind::Project, None),
            "alpha"
        ));
    }

    #[test]
    fn codex_tabs_and_status_share_deterministic_project_account_numbers() {
        let mut first = session(ShellKind::Project, Some("alpha"));
        first.id = "first".into();
        first.harness = Some(HarnessKind::Codex);
        first.codex_account_id = Some("a".into());
        first.codex_account_email = Some("a@example.test".into());
        first.codex_home = Some(PathBuf::from("/managed/a"));
        let mut second = first.clone();
        second.id = "second".into();
        second.codex_account_id = Some("b".into());
        second.codex_account_email = Some("b@example.test".into());
        second.codex_home = Some(PathBuf::from("/managed/b"));
        let mut same_account = first.clone();
        same_account.id = "same-account".into();
        let mut other_project = second.clone();
        other_project.id = "other-project".into();
        other_project.project_id = Some("beta".into());
        let mut global = first.clone();
        global.id = "global".into();
        global.project_id = None;
        global.kind = ShellKind::Orchestrator;
        let shells = [
            second.clone(),
            global.clone(),
            other_project.clone(),
            same_account.clone(),
            first.clone(),
        ];
        let numbers = codex_account_numbers(&shells);
        assert_eq!(numbers.get("first"), Some(&1));
        assert_eq!(numbers.get("same-account"), Some(&1));
        assert_eq!(numbers.get("second"), Some(&2));
        assert!(!numbers.contains_key("other-project"));
        assert!(!numbers.contains_key("global"));
        assert_eq!(
            codex_tab_title("codex 01 · main", Some(&first), &numbers),
            "codex 01 · main · A1"
        );
        assert_eq!(
            codex_session_status_label(&first, &numbers, None),
            "CODEX A1 · a@example.test"
        );
        assert_eq!(
            codex_session_status_label(&second, &numbers, None),
            "CODEX A2 · b@example.test"
        );
        let mut unknown = first.clone();
        unknown.codex_account_email = None;
        assert_eq!(
            codex_session_status_label(&unknown, &numbers, None),
            "CODEX A1 · Email unknown"
        );
        let remaining = codex_account_numbers(&[first.clone(), same_account]);
        assert!(remaining.is_empty());
        assert_eq!(
            codex_tab_title("claude 03 · main", None, &remaining),
            "claude 03 · main"
        );
    }

    #[test]
    fn status_uses_only_public_verified_email_and_marks_project_defaults() {
        let snapshot = codex_accounts::AccountsSnapshot {
            accounts: vec![codex_accounts::CodexAccount {
                id: "a".into(),
                label: "a@example.test · Workspace".into(),
                email: Some("a@example.test".into()),
                home: PathBuf::from("/managed/a"),
                available: true,
                unavailable_reason: None,
                is_system_default: false,
            }],
            ..Default::default()
        };
        let mut old_shell = session(ShellKind::Project, Some("alpha"));
        old_shell.harness = Some(HarnessKind::Codex);
        old_shell.codex_account_id = Some("a".into());
        old_shell.codex_account_label = Some("Workspace".into());
        old_shell.codex_home = Some(PathBuf::from("/managed/a"));
        assert_eq!(
            verified_shell_email(&old_shell, Some(&snapshot)).as_deref(),
            Some("a@example.test")
        );
        old_shell.codex_home = Some(PathBuf::from("/managed/other"));
        assert_eq!(verified_shell_email(&old_shell, Some(&snapshot)), None);
        let mut project: Project = serde_json::from_value(serde_json::json!({
            "id":"alpha", "name":"Alpha", "root":"/alpha", "created_at":1
        }))
        .unwrap();
        assert_eq!(
            project_default_account_label(Some(&project), Some("a"), Some(&snapshot)),
            "DEFAULT (APP) · a@example.test"
        );
        project.codex_account = ProjectCodexAccount::Saved("a".into());
        assert_eq!(
            project_default_account_label(Some(&project), Some("missing"), Some(&snapshot)),
            "DEFAULT (PROJECT) · a@example.test"
        );
        project.codex_account = ProjectCodexAccount::SystemDefault;
        assert_eq!(
            project_default_account_label(Some(&project), Some("a"), Some(&snapshot)),
            "DEFAULT (SYSTEM) · System default · email unknown"
        );
        project.codex_account = ProjectCodexAccount::Saved("missing".into());
        assert_eq!(
            project_default_account_label(Some(&project), None, Some(&snapshot)),
            "DEFAULT (PROJECT) · Account unavailable"
        );
    }
}
