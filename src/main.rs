mod cli;
mod layouts;
mod mcp;
mod panels;
mod paths;
mod project_creator;
mod sessions;
mod settings;
mod store;
mod usage;

use std::{
    cell::Cell,
    collections::{BTreeMap, HashSet},
    env,
    ops::Range,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use gpui::{
    AnyElement, App, Bounds, ClipboardItem, Context, DragMoveEvent, Entity, EntityInputHandler,
    FocusHandle, Global, IntoElement, KeyBinding, KeyDownEvent, Menu, MenuItem, MouseButton,
    Pixels, Point, Render, StatefulInteractiveElement, TitlebarOptions, UTF16Selection, Window,
    WindowBounds, WindowHandle, WindowOptions, actions, canvas, div, img, point, prelude::*, px,
    rgb, size,
};
use gpui_libghostty::{TerminalColor, TerminalConfiguration, TerminalOptions, TerminalTheme};
use gpui_platform::application;
use layouts::{
    Axis, Layout, LayoutStore, PaneId, PanelKind, ProjectLayout, SavedPane, SavedTab, TabEdge,
    WindowSize,
};
use panels::{PanelAction, PanelData};
use project_creator::{ProjectCreationEvent, ProjectCreator};
use sessions::{HarnessKind, SessionManager, SessionMetrics, ShellKind, ShellSession};
use settings::{Settings, SettingsPanel, SettingsStore};
use store::{SearchHit, State, Store};
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
        CreateProject,
        ToggleFocusMode,
        OpenSettings,
        Quit
    ]
);

type TabId = u64;

const BG: u32 = 0x090d14;
const PANEL: u32 = 0x101720;
const PANEL_ACTIVE: u32 = 0x14212a;
const DIVIDER: u32 = 0x253c45;
const CYAN: u32 = 0x55e6dc;
const MAGENTA: u32 = 0xce78ef;
const GOLD: u32 = 0xf4bf75;
const TEXT: u32 = 0xd3e1e6;
const MUTED: u32 = 0x708993;
const WINDOW_CONTROLS_WIDTH: f32 = 78.0;
const WINDOW_CONTROLS_CONTENT_INSET: f32 = WINDOW_CONTROLS_WIDTH + 14.0;
const FOCUS_MAX_WIDTH: f32 = 1100.0;
const FOCUS_BOTTOM_MARGIN: f32 = 0.30;
const FOCUS_TOOLBAR_HEIGHT: f32 = 32.0;

struct Tab {
    id: TabId,
    title: String,
    content: TabContent,
}

enum TabContent {
    Shell {
        shell_id: String,
        worktree_id: Option<String>,
        terminal: Entity<Terminal>,
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

    fn terminal(&self) -> Option<&Entity<Terminal>> {
        match &self.content {
            TabContent::Shell { terminal, .. } => Some(terminal),
            _ => None,
        }
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
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(12.0))
            .py(px(7.0))
            .bg(rgb(PANEL_ACTIVE))
            .border_1()
            .border_color(rgb(CYAN))
            .text_color(rgb(TEXT))
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
    codex: Option<ProviderUsage>,
    codex_error: Option<String>,
    pending: bool,
    last_attempt: u64,
}
impl Global for AccountUsage {}

struct Workspace {
    project_creator: Option<Entity<ProjectCreator>>,
    layout: Layout,
    layout_ready: bool,
    panes: BTreeMap<PaneId, Pane>,
    active_pane: PaneId,
    next_pane_id: PaneId,
    next_tab_id: TabId,
    layouts: LayoutStore,
    settings_store: SettingsStore,
    settings_panel: Entity<SettingsPanel>,
    settings: Settings,
    window_size: Option<WindowSize>,
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
    claude_usage: BTreeMap<String, ProviderUsage>,
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

impl Workspace {
    fn new(
        startup_path: Option<PathBuf>,
        fallback_cwd: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let store = Store::open_default().expect("open RiWork project store");
        let layouts = LayoutStore::open_default().expect("open RiWork layout store");
        let settings_store = SettingsStore::open_default().expect("open RiWork settings store");
        let settings = cx.global::<Settings>().clone();
        let settings_panel = cx.new(|cx| SettingsPanel::new(settings_store.clone(), cx));
        let sessions = SessionManager::open_default().expect("open RiWork shell registry");
        let initial = store.snapshot().expect("read RiWork project store");
        let project = if let Some(path) = startup_path {
            let root = path.canonicalize().expect("resolve project path");
            initial
                .projects
                .iter()
                .find(|project| project.root == root)
                .cloned()
                .unwrap_or_else(|| store.add_project(root, None).expect("register project"))
        } else if let Some(project) = initial.active_project() {
            project.clone()
        } else {
            store
                .add_project(fallback_cwd, None)
                .expect("register initial project")
        };
        let state = store.snapshot().expect("read selected project");
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
            layout: Layout::Pane(1),
            layout_ready: false,
            panes,
            active_pane: 1,
            next_pane_id: 2,
            next_tab_id: 1,
            layouts,
            settings_store,
            settings_panel,
            settings,
            window_size: None,
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
            claude_usage: BTreeMap::new(),
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
            search_focused: false,
            search: String::new(),
            search_marked: None,
            cwd: project.root,
            shell_name,
            notice: None,
            focus: cx.focus_handle(),
        };
        workspace.load_project(window, cx);
        cx.observe_window_bounds(window, |workspace, window, cx| {
            workspace.remember_window_size(window, cx);
        })
        .detach();
        cx.observe_global_in::<Settings>(window, |workspace, window, cx| {
            workspace.apply_settings(window, cx);
        })
        .detach();
        cx.on_release(|workspace, _| workspace.save_layout())
            .detach();
        request_codex_usage(false, cx);
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

    fn terminal_options(command: String, cwd: PathBuf, use_riwork_colors: bool) -> TerminalOptions {
        let mut options = TerminalOptions::new(command, cwd);
        options.quiet_login = true;
        options.configuration = if use_riwork_colors {
            TerminalConfiguration::UserDefaultWithOverride(TerminalTheme::new(
                TerminalColor::new(0x09, 0x0d, 0x14),
                TerminalColor::new(0xd3, 0xe1, 0xe6),
                [
                    TerminalColor::new(0x13, 0x1b, 0x25),
                    TerminalColor::new(0xf0, 0x73, 0x8b),
                    TerminalColor::new(0x61, 0xd5, 0xae),
                    TerminalColor::new(0xf4, 0xbf, 0x75),
                    TerminalColor::new(0x78, 0xa9, 0xff),
                    TerminalColor::new(0xce, 0x78, 0xef),
                    TerminalColor::new(0x55, 0xe6, 0xdc),
                    TerminalColor::new(0xd3, 0xe1, 0xe6),
                    TerminalColor::new(0x58, 0x70, 0x7b),
                    TerminalColor::new(0xff, 0x8b, 0xa0),
                    TerminalColor::new(0x83, 0xeb, 0xc3),
                    TerminalColor::new(0xff, 0xd1, 0x91),
                    TerminalColor::new(0x9b, 0xc0, 0xff),
                    TerminalColor::new(0xdf, 0xa3, 0xf7),
                    TerminalColor::new(0x84, 0xf3, 0xea),
                    TerminalColor::new(0xff, 0xff, 0xff),
                ],
            ))
        } else {
            TerminalConfiguration::UserDefault
        };
        options
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
        result
    }

    fn attach_session(
        &mut self,
        pane_id: PaneId,
        shell: ShellSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let command = self.sessions.attach_command(&shell.id)?;
        let terminal = Terminal::spawn(
            Self::terminal_options(command, shell.cwd.clone(), self.settings.use_riwork_colors),
            window,
            cx,
        )?;
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
        let pane = self
            .panes
            .get_mut(&pane_id)
            .ok_or_else(|| format!("Pane {pane_id} no longer exists"))?;
        for tab in &pane.tabs {
            tab.set_visible(false, cx);
        }
        pane.tabs.push(Tab {
            id: tab_id,
            title: if shell.kind == ShellKind::Orchestrator {
                orchestrator_tab_title(&shell)
            } else {
                format!(
                    "{} {:02} · {}",
                    shell.harness.map(harness_name).unwrap_or(&self.shell_name),
                    tab_id,
                    worktree_name
                )
            },
            content: TabContent::Shell {
                shell_id: shell.id,
                worktree_id: shell.worktree_id,
                terminal,
            },
        });
        pane.active = pane.tabs.len() - 1;
        self.active_pane = pane_id;
        self.notice = None;
        cx.notify();
        Ok(())
    }

    fn remember_window_size(&mut self, window: &Window, cx: &mut Context<Self>) {
        if !self.settings.remember_window_size {
            return;
        }
        // Fullscreen and maximized frames must not replace the normal window size.
        let WindowBounds::Windowed(bounds) = window.window_bounds() else {
            return;
        };
        let size = WindowSize::new(bounds.size.width.as_f32(), bounds.size.height.as_f32());
        if size.is_none() || size == self.window_size {
            return;
        }
        self.window_size = size;
        // Avoid writing for every frame during a drag. Release also saves the final size.
        self.window_size_save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(250))
                .await;
            let _ = this.update(cx, |workspace, _| workspace.save_layout());
        }));
    }

    fn apply_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let settings = cx.global::<Settings>().clone();
        let colors_changed = settings.use_riwork_colors != self.settings.use_riwork_colors;
        let remember_changed = settings.remember_window_size != self.settings.remember_window_size;
        self.settings = settings;
        if remember_changed && self.settings.remember_window_size {
            self.remember_window_size(window, cx);
        }
        if colors_changed {
            // Reconnect only the display clients; the tmux shells and harnesses stay alive.
            // Replacing a client's configuration also removes old palette overrides completely.
            let colors = self.settings.use_riwork_colors;
            let mut error = None;
            for pane in self.panes.values_mut() {
                for tab in &mut pane.tabs {
                    let TabContent::Shell {
                        shell_id, terminal, ..
                    } = &mut tab.content
                    else {
                        continue;
                    };
                    let replacement = (|| {
                        let shell = self.sessions.get(shell_id)?;
                        if !shell.alive {
                            return Ok(None);
                        }
                        let command = self.sessions.attach_command(shell_id)?;
                        let mut options = Self::terminal_options(command, shell.cwd, colors);
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
            self.terminal_snapshots.clear();
            if let Some(error) = error {
                self.notice = Some(error);
            }
            self.focus_active(window, cx);
        }
        cx.notify();
    }

    fn panel_title(panel: PanelKind) -> &'static str {
        match panel {
            PanelKind::Projects => "PROJECTS",
            PanelKind::Worktrees => "WORKTREES",
            PanelKind::Tasks => "TASKS",
            PanelKind::Shells => "SHELLS",
            PanelKind::Usage => "USAGE",
            PanelKind::Settings => "SETTINGS",
        }
    }

    fn attach_panel(&mut self, pane_id: PaneId, panel: PanelKind, cx: &mut Context<Self>) {
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
    }

    fn panel_action(&mut self, action: PanelAction, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            PanelAction::CreateProject => self.begin_project_creation(window, cx),
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

    fn begin_tab_drag(&mut self, cx: &mut Context<Self>) {
        self.terminal_snapshots.clear();
        self.tab_dragging = true;
        for pane in self.panes.values() {
            if let Some(tab) = pane.tabs.get(pane.active) {
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
        self.tab_dragging = false;
        self.terminal_snapshots.clear();
        for pane in self.panes.values() {
            for (index, tab) in pane.tabs.iter().enumerate() {
                tab.set_visible(index == pane.active, cx);
            }
        }
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
        self.resize_at(window.mouse_position(), cx);
        self.resizing = None;
        self.save_layout();
    }

    fn load_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.focus_mode {
            self.set_focus_mode(false, window, cx);
        }
        self.layout_ready = false;
        for pane in self.panes.values() {
            for tab in &pane.tabs {
                tab.set_visible(false, cx);
            }
        }
        self.layout = Layout::Pane(1);
        self.panes.clear();
        self.tab_dragging = false;
        self.terminal_snapshots.clear();
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
        let saved = match self.layouts.load(&self.project_id) {
            Ok(saved) => saved,
            Err(error) => {
                self.notice = Some(error);
                cx.notify();
                return;
            }
        };
        self.window_size = if self.settings.remember_window_size {
            match window.window_bounds() {
                WindowBounds::Windowed(bounds) => {
                    WindowSize::new(bounds.size.width.as_f32(), bounds.size.height.as_f32())
                }
                _ => saved.as_ref().and_then(|saved| saved.window_size),
            }
        } else {
            saved.as_ref().and_then(|saved| saved.window_size)
        };
        let live_shells = shells
            .iter()
            .filter(|shell| session_belongs_to_workspace(shell, &self.project_id) && shell.alive)
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
                if self.state.worktrees.iter().any(|worktree| {
                    &worktree.id == worktree_id && worktree.project_id == self.project_id
                }) {
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
                                    if let Err(error) =
                                        self.attach_session(*pane_id, shell.clone(), window, cx)
                                    {
                                        restore_error = Some(error);
                                    }
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
                if let Err(error) = self.attach_session(active_pane, shell.clone(), window, cx) {
                    restore_error = Some(error);
                }
            }
        }
        if saved.is_none()
            && !live_shells
                .values()
                .any(|shell| shell.kind == ShellKind::Project)
        {
            if let Err(error) = self.spawn_tab(active_pane, window, cx) {
                restore_error = Some(error);
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
            self.notice = None;
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

    fn save_layout(&mut self) {
        if !self.layout_ready {
            return;
        }
        let saved = ProjectLayout {
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
        };
        if let Err(error) = self.layouts.save(&self.project_id, &saved) {
            self.notice = Some(error);
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
        self.save_layout();
        self.project_id = project.id;
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
            let work = cx
                .background_executor()
                .spawn(async move { store.sync_worktrees(&work_project_id) });
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
        if !self.layout_ready {
            self.load_project(window, cx);
        }
        if let Ok(metrics) = self.sessions.metrics_snapshot() {
            self.metrics = metrics;
        }
        if let Ok(shells) = self.sessions.list() {
            self.shells = shells;
        }
        request_codex_usage(false, cx);
        self.claude_usage.clear();
        for shell in &self.shells {
            if shell.harness == Some(HarnessKind::Claude)
                && shell.project_id.as_deref() == Some(self.project_id.as_str())
            {
                if let Ok(Some(snapshot)) = usage::read_claude_usage(&shell.id) {
                    self.claude_usage.insert(shell.id.clone(), snapshot);
                }
            }
        }
        self.shell_cwds.clear();
        for shell in &self.shells {
            if shell.alive {
                if let Ok(cwd) = self.sessions.current_directory(&shell.id) {
                    self.shell_cwds.insert(shell.id.clone(), cwd);
                }
            }
        }
        for pane in self.panes.values_mut() {
            for tab in &mut pane.tabs {
                if let Some(shell) = tab
                    .shell_id()
                    .and_then(|id| self.shells.iter().find(|shell| shell.id == id))
                    .filter(|shell| shell.kind == ShellKind::Orchestrator)
                {
                    tab.title = orchestrator_tab_title(shell);
                    continue;
                }
                if let Some(cwd) = tab.shell_id().and_then(|id| self.shell_cwds.get(id)) {
                    let worktree = self
                        .state
                        .worktrees
                        .iter()
                        .filter(|worktree| worktree.project_id == self.project_id)
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
                    tab.title = format!("{} {:02} · {}", program, tab.id, label);
                }
            }
        }
        cx.notify();
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
        let terminal = self
            .panes
            .get(&self.active_pane)
            .and_then(|pane| pane.tabs.get(pane.active))
            .and_then(|tab| tab.terminal().cloned());
        if let Some(terminal) = terminal {
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
            self.focus_active(window, cx);
            self.save_layout();
            cx.notify();
            return;
        }
        let was_active = self.active_pane == pane_id;
        if let Some(pane) = self.panes.remove(&pane_id) {
            for tab in pane.tabs {
                if let Some(shell_id) = tab.shell_id() {
                    self.detached_shell_ids.insert(shell_id.to_owned());
                }
                tab.set_visible(false, cx);
            }
        }
        if let Some(layout) = self.layout.clone().without(pane_id) {
            self.layout = layout;
            if was_active {
                self.active_pane = self.layout.first_pane();
                self.focus_active(window, cx);
            }
            self.save_layout();
            cx.notify();
        }
    }

    fn new_tab_action(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
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
        if let Err(error) = open_workspace_window(Some(path), self.cwd.clone(), cx) {
            self.notice = Some(error);
        }
        cx.notify();
    }

    fn open_codex_action(&mut self, _: &OpenCodex, window: &mut Window, cx: &mut Context<Self>) {
        self.add_harness(HarnessKind::Codex, false, window, cx);
    }

    fn open_claude_action(&mut self, _: &OpenClaude, window: &mut Window, cx: &mut Context<Self>) {
        self.add_harness(HarnessKind::Claude, false, window, cx);
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
        self.add_split(Axis::SideBySide, window, cx);
    }

    fn split_down_action(&mut self, _: &SplitDown, window: &mut Window, cx: &mut Context<Self>) {
        self.add_split(Axis::Stacked, window, cx);
    }

    fn close_tab_action(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pane) = self.panes.get(&self.active_pane) else {
            return;
        };
        let Some(tab) = pane.tabs.get(pane.active) else {
            return;
        };
        self.remove_tab(self.active_pane, tab.id, window, cx);
    }

    fn close_pane_action(&mut self, _: &ClosePane, window: &mut Window, cx: &mut Context<Self>) {
        self.remove_pane(self.active_pane, window, cx);
    }

    fn next_tab_action(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        self.cycle_tab(1, window, cx);
    }

    fn previous_tab_action(
        &mut self,
        _: &PreviousTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cycle_tab(-1, window, cx);
    }

    fn toggle_sidebar_action(
        &mut self,
        _: &ToggleSidebar,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_panel(PanelKind::Projects, self.active_pane, window, cx);
    }

    fn open_settings_action(
        &mut self,
        _: &OpenSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_panel(PanelKind::Settings, self.active_pane, window, cx);
    }

    fn focus_search_action(
        &mut self,
        _: &FocusSearch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let active_panel = self
            .panes
            .get(&self.active_pane)
            .and_then(|pane| pane.tabs.get(pane.active))
            .is_some_and(|tab| matches!(tab.content, TabContent::Panel(kind) if kind != PanelKind::Settings && kind != PanelKind::Usage));
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
        if self.project_creator.is_some() {
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
        if self.project_creator.is_some() || !self.ensure_layout(window, cx) {
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
        if self.project_creator.is_none() {
            self.set_focus_mode(!self.focus_mode, window, cx);
        }
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
        if !self.search_focused {
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

    fn render_layout(
        &self,
        layout: &Layout,
        path: Vec<bool>,
        width: f32,
        x: f32,
        at_window_top: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
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
                    .bg(rgb(DIVIDER))
                    .hover(|style| style.bg(rgb(CYAN)))
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
                    divider.w(px(5.0)).h_full().cursor_col_resize()
                } else {
                    divider.h(px(5.0)).w_full().cursor_row_resize()
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
                                    (width - 5.0).max(0.0) * *ratio
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
                                    (width - 5.0).max(0.0) * (1.0 - *ratio)
                                } else {
                                    width
                                },
                                if horizontal {
                                    x + (width - 5.0).max(0.0) * *ratio + 5.0
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
        let Some(pane) = self.panes.get(&pane_id) else {
            return div().into_any_element();
        };
        let selected = self.active_pane == pane_id;
        let active_is_panel = pane
            .tabs
            .get(pane.active)
            .is_some_and(|tab| matches!(tab.content, TabContent::Panel(_)));
        let compact = pane_width < 260.0;
        let window_drag_enabled = at_window_top;
        let control_inset = if at_window_top && x < WINDOW_CONTROLS_CONTENT_INSET {
            (WINDOW_CONTROLS_CONTENT_INSET - x).min(pane_width)
        } else {
            0.0
        };
        let last_pane = self.layout.pane_ids().last() == Some(&pane_id);
        let tabs = pane
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let tab_id = tab.id;
                let active = index == pane.active;
                let panel = matches!(tab.content, TabContent::Panel(_));
                let workspace = cx.entity();
                div()
                    .id(("tab", tab_id))
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .h_full()
                    .px(px(8.0))
                    .gap(px(6.0))
                    .border_r_1()
                    .border_b_1()
                    .border_color(rgb(if active { CYAN } else { DIVIDER }))
                    .bg(rgb(if active { PANEL_ACTIVE } else { PANEL }))
                    .text_color(rgb(if active {
                        if panel { MAGENTA } else { TEXT }
                    } else {
                        MUTED
                    }))
                    .text_size(px(if panel { 9.0 } else { 10.0 }))
                    .cursor_grab()
                    .hover(|style| style.bg(rgb(PANEL_ACTIVE)))
                    .drag_over::<DraggedTab>(|style, _, _, _| {
                        style.border_l_2().border_color(rgb(CYAN))
                    })
                    .child(tab.title.clone())
                    .children(active.then(|| {
                        div()
                            .id(("close-tab", tab_id))
                            .text_color(rgb(MUTED))
                            .hover(|style| style.text_color(rgb(MAGENTA)))
                            .child("×")
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |workspace, _, window, cx| {
                                cx.stop_propagation();
                                workspace.remove_tab(pane_id, tab_id, window, cx);
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
                            title: tab.title.clone(),
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
            .bg(rgb(PANEL))
            .border_b_1()
            .border_color(rgb(if selected { CYAN } else { DIVIDER }))
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
                    .id(("window-drag-grip", pane_id))
                    .flex_none()
                    .w(px(16.0))
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(rgb(MUTED))
                    .cursor_grab()
                    .hover(|style| style.bg(rgb(PANEL_ACTIVE)).text_color(rgb(CYAN)))
                    .child("⋮")
                    .on_mouse_down(MouseButton::Left, start_window_drag)
            }))
            .child(
                div()
                    .flex()
                    .h_full()
                    .flex_none()
                    .children((!compact).then(|| {
                        self.pane_button(
                            pane_id,
                            "new",
                            "+",
                            CYAN,
                            |workspace, id, window, cx| {
                                workspace.toggle_panel_menu(id, window, cx);
                            },
                            cx,
                        )
                    }))
                    .child(self.pane_button(
                        pane_id,
                        "focus",
                        "⛶",
                        CYAN,
                        |workspace, id, window, cx| {
                            workspace.active_pane = id;
                            workspace.set_focus_mode(true, window, cx);
                        },
                        cx,
                    ))
                    .child(self.pane_button(
                        pane_id,
                        "views",
                        "▤",
                        MAGENTA,
                        |workspace, id, window, cx| {
                            workspace.toggle_panel_menu(id, window, cx);
                        },
                        cx,
                    ))
                    .children((!active_is_panel && !compact).then(|| {
                        self.pane_button(
                            pane_id,
                            "split",
                            "║",
                            MUTED,
                            |workspace, id, window, cx| {
                                workspace.active_pane = id;
                                workspace.add_split(Axis::SideBySide, window, cx);
                            },
                            cx,
                        )
                    }))
                    .children((!active_is_panel && !compact).then(|| {
                        self.pane_button(
                            pane_id,
                            "stack",
                            "═",
                            MUTED,
                            |workspace, id, window, cx| {
                                workspace.active_pane = id;
                                workspace.add_split(Axis::Stacked, window, cx);
                            },
                            cx,
                        )
                    }))
                    .children((!compact).then(|| {
                        self.pane_button(
                            pane_id,
                            "close",
                            "×",
                            MUTED,
                            |workspace, id, window, cx| {
                                workspace.remove_pane(id, window, cx);
                            },
                            cx,
                        )
                    })),
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
            Some(TabContent::Shell { terminal, .. }) => {
                let snapshot = pane
                    .tabs
                    .get(pane.active)
                    .and_then(|tab| self.terminal_snapshots.get(&tab.id));
                if self.tab_dragging {
                    match snapshot {
                        Some(snapshot) => img(snapshot.clone()).size_full().into_any_element(),
                        None => div().size_full().bg(rgb(BG)).into_any_element(),
                    }
                } else {
                    terminal.clone().into_any_element()
                }
            }
            Some(TabContent::Panel(PanelKind::Usage)) => self.render_usage_panel(cx),
            Some(TabContent::Panel(PanelKind::Settings)) => {
                self.settings_panel.clone().into_any_element()
            }
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
                    query: &self.search,
                    search_focused: self.search_focused && selected,
                    focus: self.focus.clone(),
                    control_inset: 0.0,
                },
                Self::panel_action,
                cx,
            ),
            None => div()
                .p(px(14.0))
                .text_color(rgb(MUTED))
                .child("DROP A TAB HERE  ·  + SHELL  ·  ▤ VIEWS")
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
            .bg(rgb(BG))
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
                    .bg(rgb(CYAN))
                    .opacity(0.13)
                    .border_1()
                    .border_color(rgb(CYAN));
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
                    .bg(rgb(PANEL_ACTIVE))
                    .border_b_1()
                    .border_color(rgb(DIVIDER))
                    .text_color(rgb(MUTED))
                    .child("ORCHESTRATOR SKILL UPDATE AVAILABLE")
                    .child(div().flex_1())
                    .child(
                        div()
                            .id(("load-orchestrator-skill", pane_id))
                            .text_color(rgb(GOLD))
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
        let container = if last_pane && !self.focus_mode {
            container.child(
                div()
                    .id(("bottom-status-space", pane_id))
                    .h(px(22.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .bg(rgb(PANEL))
                    .border_t_1()
                    .border_color(rgb(DIVIDER))
                    .child(
                        div()
                            .id(("bottom-status-fill", pane_id))
                            .flex_1()
                            .min_w_0()
                            .h_full(),
                    )
                    .child(self.render_status(pane_width, cx)),
            )
        } else {
            container
        };
        container
            .children(
                (!self.focus_mode && self.panel_menu == Some(pane_id)).then(|| {
                    let menu = div()
                        .id(("view-menu", pane_id))
                        .absolute()
                        .top(px(30.0))
                        .right(px(12.0))
                        .w(px(185.0))
                        .max_h(gpui::relative(0.9))
                        .overflow_y_scroll()
                        .bg(rgb(PANEL_ACTIVE))
                        .border_1()
                        .border_color(rgb(MAGENTA))
                        .p(px(3.0))
                        .on_mouse_down_out(cx.listener(|workspace, _, _, cx| {
                            workspace.panel_menu = None;
                            workspace.finish_tab_drag(cx);
                            cx.notify();
                        }));
                    menu.children(
                        [
                            PanelKind::Projects,
                            PanelKind::Worktrees,
                            PanelKind::Tasks,
                            PanelKind::Shells,
                            PanelKind::Usage,
                            PanelKind::Settings,
                        ]
                        .into_iter()
                        .map(|kind| {
                            div()
                                .id(format!("open-{}-{pane_id}", Self::panel_title(kind)))
                                .px(px(10.0))
                                .py(px(7.0))
                                .text_color(rgb(TEXT))
                                .hover(|style| style.bg(rgb(DIVIDER)))
                                .child(Self::panel_title(kind))
                                .on_click(cx.listener(move |workspace, _, window, cx| {
                                    workspace.open_panel(kind, pane_id, window, cx)
                                }))
                        }),
                    )
                    .children(
                        [
                            ("+ SHELL", 0),
                            ("CODEX", 1),
                            ("CLAUDE", 2),
                            ("CODEX · UNRESTRICTED", 3),
                            ("CLAUDE · UNRESTRICTED", 4),
                            ("GLOBAL ORCHESTRATOR", 6),
                            ("PROJECT ORCHESTRATOR", 7),
                            ("FOCUS TAB · ⌘⇧F", 8),
                            ("CLOSE TAB", 5),
                        ]
                        .into_iter()
                        .map(|(label, action)| {
                            div()
                                .id(format!("pane-menu-{pane_id}-{action}"))
                                .px(px(10.0))
                                .py(px(7.0))
                                .border_t_1()
                                .border_color(rgb(DIVIDER))
                                .text_color(rgb(MUTED))
                                .hover(|style| style.bg(rgb(DIVIDER)).text_color(rgb(CYAN)))
                                .child(label)
                                .on_click(cx.listener(move |workspace, _, window, cx| {
                                    workspace.panel_menu = None;
                                    workspace.active_pane = pane_id;
                                    match action {
                                        0 => workspace.add_tab(window, cx),
                                        1 => workspace.add_harness(
                                            HarnessKind::Codex,
                                            false,
                                            window,
                                            cx,
                                        ),
                                        2 => workspace.add_harness(
                                            HarnessKind::Claude,
                                            false,
                                            window,
                                            cx,
                                        ),
                                        3 => workspace.add_harness(
                                            HarnessKind::Codex,
                                            true,
                                            window,
                                            cx,
                                        ),
                                        4 => workspace.add_harness(
                                            HarnessKind::Claude,
                                            true,
                                            window,
                                            cx,
                                        ),
                                        5 => {
                                            if let Some(tab_id) = workspace
                                                .panes
                                                .get(&pane_id)
                                                .and_then(|pane| pane.tabs.get(pane.active))
                                                .map(|tab| tab.id)
                                            {
                                                workspace.remove_tab(pane_id, tab_id, window, cx);
                                            }
                                        }
                                        6 => workspace.open_scoped_orchestrator(None, window, cx),
                                        7 => workspace.open_scoped_orchestrator(
                                            Some(workspace.project_id.clone()),
                                            window,
                                            cx,
                                        ),
                                        8 => workspace.set_focus_mode(true, window, cx),
                                        _ => {}
                                    }
                                }))
                        }),
                    )
                }),
            )
            .into_any_element()
    }

    fn render_status(&self, width: f32, cx: &mut Context<Self>) -> AnyElement {
        let (live, cpu, ram) = self
            .shells
            .iter()
            .filter(|shell| {
                shell.project_id.as_deref() == Some(self.project_id.as_str()) && shell.alive
            })
            .fold((0_usize, 0.0_f32, 0_u64), |(count, cpu, ram), shell| {
                let metrics = self.metrics.get(&shell.id).copied().unwrap_or_default();
                (
                    count + 1,
                    cpu + metrics.cpu_percent,
                    ram + metrics.ram_bytes,
                )
            });
        let active_shell_id = self
            .panes
            .get(&self.active_pane)
            .and_then(|pane| pane.tabs.get(pane.active))
            .and_then(|tab| tab.shell_id())
            .map(str::to_owned);
        let label = if width > 480.0 {
            format!("{live} LIVE CPU {cpu:.1}% RAM {}", format_bytes(ram))
        } else {
            format!("{cpu:.0}% {}M", ram / (1024 * 1024))
        };
        div()
            .id("status-island")
            .h(px(20.0))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.0))
            .px(px(7.0))
            .rounded(px(4.0))
            .bg(rgb(PANEL_ACTIVE))
            .text_size(px(9.0))
            .text_color(rgb(MUTED))
            .child(label)
            .children((width > 360.0).then(|| self.render_usage_chip(cx)))
            .children(
                (width > 600.0)
                    .then_some(active_shell_id)
                    .flatten()
                    .map(|id| {
                        div()
                            .id("copy-active-shell-id")
                            .text_color(rgb(CYAN))
                            .child(id[..8].to_owned())
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(id.clone()))
                            }))
                    }),
            )
            .child(
                div()
                    .id("top-orchestrator")
                    .text_color(rgb(MAGENTA))
                    .child("G·ORCH")
                    .on_click(cx.listener(|workspace, _, window, cx| {
                        workspace.open_orchestrator(window, cx)
                    })),
            )
            .child(
                div()
                    .id("project-orchestrator")
                    .text_color(rgb(CYAN))
                    .child("P·ORCH")
                    .on_click(cx.listener(|workspace, _, window, cx| {
                        workspace.open_scoped_orchestrator(
                            Some(workspace.project_id.clone()),
                            window,
                            cx,
                        )
                    })),
            )
            .into_any_element()
    }

    fn render_usage_chip(&self, cx: &mut Context<Self>) -> AnyElement {
        let active_shell = self
            .panes
            .get(&self.active_pane)
            .and_then(|pane| pane.tabs.get(pane.active))
            .and_then(Tab::shell_id)
            .and_then(|id| self.shells.iter().find(|shell| shell.id == id));
        let cache = cx.global::<AccountUsage>();
        let label = if let Some(shell) =
            active_shell.filter(|shell| shell.harness == Some(HarnessKind::Claude))
        {
            self.claude_usage
                .get(&shell.id)
                .map(|snapshot| usage_summary("CLAUDE", snapshot))
                .unwrap_or_else(|| "CLAUDE · WAITING".to_owned())
        } else {
            cache
                .codex
                .as_ref()
                .map(|snapshot| usage_summary("CODEX", snapshot))
                .unwrap_or_else(|| {
                    if cache.pending {
                        "USAGE · LOADING"
                    } else {
                        "USAGE · —"
                    }
                    .to_owned()
                })
        };
        div()
            .id("usage-chip")
            .max_w(px(180.0))
            .overflow_hidden()
            .text_ellipsis()
            .text_color(rgb(CYAN))
            .cursor_pointer()
            .hover(|style| style.text_color(rgb(MAGENTA)))
            .child(label)
            .on_click(cx.listener(|workspace, _, window, cx| {
                workspace.open_panel(PanelKind::Usage, workspace.active_pane, window, cx);
            }))
            .into_any_element()
    }

    fn render_usage_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let cache = cx.global::<AccountUsage>();
        let codex = cache.codex.clone();
        let error = cache.codex_error.clone();
        let pending = cache.pending;
        let mut cards = Vec::new();
        if let Some(snapshot) = codex.as_ref() {
            cards.push(render_provider_usage(snapshot, "CODEX"));
        } else {
            cards.push(
                div()
                    .p(px(12.0))
                    .text_color(rgb(MUTED))
                    .child(if pending {
                        "Reading Codex usage…".to_owned()
                    } else {
                        error
                            .clone()
                            .unwrap_or_else(|| "Codex usage is unavailable".to_owned())
                    })
                    .into_any_element(),
            );
        }
        if codex.is_some()
            && let Some(error) = error
        {
            cards.push(
                div()
                    .px(px(12.0))
                    .text_color(rgb(GOLD))
                    .child(format!("Last refresh: {error}"))
                    .into_any_element(),
            );
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
                cards.push(render_provider_usage(snapshot, &title));
            } else {
                cards.push(
                    div()
                        .p(px(12.0))
                        .border_t_1()
                        .border_color(rgb(DIVIDER))
                        .child(title)
                        .child(
                            div()
                                .mt(px(6.0))
                                .text_color(rgb(MUTED))
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
                    .text_color(rgb(MUTED))
                    .child("Open a Claude session to see its usage here")
                    .into_any_element(),
            );
        }
        div().size_full().flex().flex_col().min_h_0().bg(rgb(PANEL))
            .child(div().h(px(32.0)).flex_none().flex().items_center().px(px(10.0)).justify_between()
                .border_b_1().border_color(rgb(DIVIDER)).child("ACCOUNT USAGE")
                .child(div().id("refresh-account-usage").text_color(rgb(CYAN)).cursor_pointer()
                    .child(if pending { "REFRESHING…" } else { "↻ REFRESH" })
                    .on_click(cx.listener(|workspace, _, window, cx| {
                        request_codex_usage(true, cx);
                        workspace.refresh(window, cx);
                    }))))
            .child(div().id("usage-panel-scroll").flex_1().min_h_0().overflow_y_scroll().children(cards))
            .child(div().flex_none().p(px(10.0)).border_t_1().border_color(rgb(DIVIDER)).text_color(rgb(MUTED)).text_size(px(9.0))
                .child("Quota is shared by all sessions on the same account. Missing windows are unavailable. Claude subscription quota requires a supported Pro/Max account."))
            .into_any_element()
    }

    fn render_focus(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
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
            .pl(px(WINDOW_CONTROLS_CONTENT_INSET))
            .pr(px(12.0))
            .gap(px(12.0))
            .bg(rgb(PANEL))
            .border_b_1()
            .border_color(rgb(DIVIDER))
            .child(
                div()
                    .id("focus-window-drag-space")
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .items_center()
                    .gap(px(12.0))
                    .cursor_grab()
                    .on_mouse_down(MouseButton::Left, start_window_drag)
                    .children((width >= 720.0).then(|| {
                        div().flex_none().text_color(rgb(CYAN)).child(if centered {
                            "CENTER FOCUS"
                        } else {
                            "FOCUS"
                        })
                    }))
                    .child(div().text_ellipsis().text_color(rgb(MUTED)).child(title)),
            )
            .child(
                div()
                    .id("focus-layout-toggle")
                    .flex_none()
                    .px(px(8.0))
                    .py(px(5.0))
                    .cursor_pointer()
                    .text_color(rgb(MUTED))
                    .hover(|style| style.bg(rgb(PANEL_ACTIVE)).text_color(rgb(CYAN)))
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
                    .border_color(rgb(DIVIDER))
                    .bg(rgb(PANEL_ACTIVE))
                    .text_color(rgb(CYAN))
                    .cursor_pointer()
                    .hover(|style| style.border_color(rgb(CYAN)))
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
                        .border_color(rgb(DIVIDER))
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
        label: &str,
        color: u32,
        action: impl Fn(&mut Self, PaneId, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id(format!("pane-{pane_id}-{key}"))
            .h_full()
            .w(px(20.0))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .text_color(rgb(color))
            .hover(|style| style.bg(rgb(DIVIDER)).text_color(rgb(CYAN)))
            .child(label.to_owned())
            .when(key == "focus", |button| {
                button.tooltip(|_, cx| cx.new(|_| FocusModeTooltip).into())
            })
            .on_click(
                cx.listener(move |workspace, _, window, cx| action(workspace, pane_id, window, cx)),
            )
            .into_any_element()
    }

    fn render_root_dock(&self, side: DockSide, cx: &mut Context<Self>) -> AnyElement {
        let target = div()
            .id(match side {
                DockSide::Left => "root-left",
                DockSide::Right => "root-right",
                DockSide::Top => "root-top",
                DockSide::Bottom => "root-bottom",
            })
            .absolute()
            .bg(rgb(PANEL_ACTIVE))
            .opacity(0.75)
            .drag_over::<DraggedTab>(|style, _, _, _| style.bg(rgb(CYAN)))
            .on_drop(
                cx.listener(move |workspace, drag: &DraggedTab, window, cx| {
                    workspace.dock_tab(drag, None, side, window, cx);
                    cx.stop_propagation();
                }),
            );
        match side {
            DockSide::Left => target.left_0().top(px(30.0)).bottom(px(12.0)).w(px(12.0)),
            DockSide::Right => target.right_0().top(px(12.0)).bottom(px(12.0)).w(px(12.0)),
            DockSide::Top => target.top_0().left(px(80.0)).right_0().h(px(12.0)),
            DockSide::Bottom => target.bottom_0().left_0().right_0().h(px(12.0)),
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
        if !cx.has_active_drag() {
            self.drop_target = None;
            if self.tab_dragging && self.panel_menu.is_none() && self.project_creator.is_none() {
                self.finish_tab_drag(cx);
            }
        }
        for (pane_id, pane) in &self.panes {
            for (index, tab) in pane.tabs.iter().enumerate() {
                tab.set_visible(
                    !self.tab_dragging
                        && index == pane.active
                        && (!self.focus_mode || *pane_id == self.active_pane),
                    cx,
                );
            }
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
            .on_action(cx.listener(Self::split_right_action))
            .on_action(cx.listener(Self::split_down_action))
            .on_action(cx.listener(Self::close_tab_action))
            .on_action(cx.listener(Self::close_pane_action))
            .on_action(cx.listener(Self::next_tab_action))
            .on_action(cx.listener(Self::previous_tab_action))
            .on_action(cx.listener(Self::toggle_sidebar_action))
            .on_action(cx.listener(Self::open_settings_action))
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
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .font_family("Menlo")
            .text_size(px(10.0))
            .child(if self.focus_mode {
                self.render_focus(window, cx)
            } else {
                self.render_layout(
                    &self.layout,
                    Vec::new(),
                    window.viewport_size().width.as_f32(),
                    0.0,
                    true,
                    cx,
                )
            })
            .child(window_controls_island())
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
            .children(self.notice.as_ref().map(|notice| {
                div()
                    .absolute()
                    .top(px(31.0))
                    .right(px(8.0))
                    .max_w(px(520.0))
                    .p(px(8.0))
                    .bg(rgb(PANEL_ACTIVE))
                    .text_color(rgb(GOLD))
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
                        .map(|side| self.render_root_dock(side, cx))
                        .collect::<Vec<_>>()
                    })
                    .unwrap_or_default(),
            )
    }
}

struct FocusModeTooltip;
impl Render for FocusModeTooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(9.0))
            .py(px(6.0))
            .bg(rgb(PANEL_ACTIVE))
            .border_1()
            .border_color(rgb(DIVIDER))
            .text_color(rgb(TEXT))
            .text_size(px(11.0))
            .child("Center this tab · ⌘⇧F to focus or restore")
    }
}

fn window_controls_island() -> impl IntoElement {
    div()
        .id("window-controls-island")
        .absolute()
        .top_0()
        .left_0()
        .w(px(WINDOW_CONTROLS_WIDTH))
        .h(px(28.0))
        .rounded_br(px(8.0))
        .bg(rgb(PANEL_ACTIVE))
        .border_r_1()
        .border_b_1()
        .border_color(rgb(DIVIDER))
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
    }
}

fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn request_codex_usage(force: bool, cx: &mut App) {
    let now = unix_time();
    {
        let cache = cx.global_mut::<AccountUsage>();
        let interval = if cache.codex.is_some() { 15 * 60 } else { 60 };
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
    let task = cx
        .background_executor()
        .spawn(async { usage::read_codex_usage() });
    cx.spawn(async move |cx| {
        let result = task.await;
        let _ = cx.update(|cx| {
            let cache = cx.global_mut::<AccountUsage>();
            cache.pending = false;
            match result {
                Ok(snapshot) => {
                    cache.codex = Some(snapshot);
                    cache.codex_error = None;
                }
                Err(error) => cache.codex_error = Some(error),
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

fn render_provider_usage(snapshot: &ProviderUsage, title: &str) -> AnyElement {
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
                                .text_color(rgb(MUTED))
                                .text_size(px(9.0))
                                .child(reset_summary(window.resets_at)),
                        ),
                )
                .child(
                    div().mt(px(5.0)).h(px(3.0)).bg(rgb(DIVIDER)).child(
                        div()
                            .h_full()
                            .w(gpui::relative((remaining / 100.0) as f32))
                            .bg(rgb(if remaining < 15.0 { MAGENTA } else { CYAN })),
                    ),
                )
                .into_any_element()
        })
        .collect::<Vec<_>>();
    div()
        .p(px(12.0))
        .border_t_1()
        .border_color(rgb(DIVIDER))
        .child(div().text_color(rgb(CYAN)).child(title.to_owned()))
        .children(snapshot.account_label.as_ref().map(|label| {
            div()
                .mt(px(4.0))
                .text_color(rgb(MUTED))
                .child(label.clone())
        }))
        .child(
            div()
                .mt(px(4.0))
                .text_color(rgb(if age > 900 { GOLD } else { MUTED }))
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
                .text_color(rgb(MUTED))
                .child("Quota windows unavailable")
        }))
        .children(snapshot.context_used_percent.map(|used| {
            div()
                .mt(px(10.0))
                .text_color(rgb(MUTED))
                .child(format!("Context · {used:.0}% used"))
        }))
        .children(snapshot.session_cost_usd.map(|cost| {
            div()
                .mt(px(5.0))
                .text_color(rgb(MUTED))
                .child(format!("Estimated session cost · ${cost:.2}"))
        }))
        .into_any_element()
}

fn main() {
    let args = env::args().skip(1).collect::<Vec<_>>();
    match cli::run_cli(&args) {
        Ok(true) => return,
        Ok(false) => {}
        Err(error) => {
            eprintln!("riwork: {error}");
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

    application().run(move |cx: &mut App| {
        cx.set_global(AccountUsage::default());
        let settings = SettingsStore::open_default()
            .and_then(|store| store.load())
            .unwrap_or_else(|error| {
                eprintln!("riwork: {error}");
                Settings::default()
            });
        cx.set_global(settings);
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
        ]);
        cx.set_menus([Menu::new("RiWork").items([
            MenuItem::action("Settings…", OpenSettings),
            MenuItem::separator(),
            MenuItem::action("Quit RiWork", Quit),
        ])]);
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        open_workspace_window(startup_path.clone(), fallback_cwd.clone(), cx)
            .expect("open RiWork window");
        cx.activate(true);
    });
}

fn open_workspace_window(
    startup_path: Option<PathBuf>,
    fallback_cwd: PathBuf,
    cx: &mut App,
) -> Result<WindowHandle<Workspace>, String> {
    let saved_size = if cx.global::<Settings>().remember_window_size {
        let state = Store::open_default()?.snapshot()?;
        let project = match startup_path.as_ref() {
            Some(path) => {
                let root = path
                    .canonicalize()
                    .map_err(|error| format!("Cannot resolve project: {error}"))?;
                state.projects.iter().find(|project| project.root == root)
            }
            None => state.active_project(),
        };
        match project {
            Some(project) => LayoutStore::open_default()?
                .load(&project.id)?
                .and_then(|layout| layout.window_size),
            None => None,
        }
    } else {
        None
    };
    let saved_size = saved_size.unwrap_or(WindowSize {
        width: 1220.0,
        height: 780.0,
    });
    // A project last opened on a larger monitor must still fit the current display.
    let available = cx.primary_display().map(|display| display.bounds().size);
    let width = available.map_or(saved_size.width, |size| {
        saved_size
            .width
            .min((size.width.as_f32() - 40.0).max(640.0))
    });
    let height = available.map_or(saved_size.height, |size| {
        saved_size
            .height
            .min((size.height.as_f32() - 80.0).max(400.0))
    });
    let mut bounds = Bounds::centered(None, size(px(width), px(height)), cx);
    let offset = px((cx.windows().len() % 5) as f32 * 22.0);
    bounds.origin.x += offset;
    bounds.origin.y += offset;
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                appears_transparent: true,
                traffic_light_position: Some(point(px(10.0), px(8.0))),
                ..Default::default()
            }),
            app_owns_titlebar_drag: true,
            window_min_size: Some(size(px(640.0), px(400.0))),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| Workspace::new(startup_path, fallback_cwd, window, cx)),
    )
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod workspace_tab_tests {
    use super::*;

    #[test]
    fn terminal_palette_override_is_opt_in_and_preserves_user_configuration() {
        let original =
            Workspace::terminal_options("shell".to_owned(), PathBuf::from("/tmp"), false);
        assert_eq!(original.configuration, TerminalConfiguration::UserDefault);
        let themed = Workspace::terminal_options("shell".to_owned(), PathBuf::from("/tmp"), true);
        assert!(matches!(
            themed.configuration,
            TerminalConfiguration::UserDefaultWithOverride(_)
        ));
    }

    fn session(kind: ShellKind, project: Option<&str>) -> ShellSession {
        ShellSession {
            id: "scope-test".to_owned(),
            project_id: project.map(str::to_owned),
            worktree_id: None,
            kind,
            cwd: PathBuf::from("/tmp"),
            command: None,
            harness: None,
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
}
