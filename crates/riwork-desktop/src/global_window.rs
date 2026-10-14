//! The global window: the dashboard and the global chat, in one window of their own that
//! belongs to no project.
//!
//! There is at most one. Every way in (the status bar's global menu, View › Global Dashboard
//! and Global Chat, ⌘⌥G and ⌘⇧O, the dashboard's Chat control, and any entry that would show
//! the global orchestrator) brings it up and switches it to the view asked for; asking again
//! from any project window raises the same window. It has no project: it reads the store and
//! the sessions like any window, but it never registers, selects or switches a project, never
//! writes a project's layout, tabs or chat requests, and claims no project's shell. What it
//! last showed (its view, and its dashboard's filter and order) is kept in
//! `global-window.json` (`riwork_core::global_window`) and, for a reload, in its own entry of
//! the runtime registry (`runtime::WindowKind::Global`). The dashboard's search lives as long
//! as the window and is not kept.
//!
//! The dashboard draws what the window samples, as project windows do; opening, drawing or
//! refreshing it starts nothing. The Chat view shows the global orchestrator there is: its
//! chat (`ChatView`, with its own transcript, draft, provider, permission and model controls,
//! all kept by the chat host and the chat's id) or, when it runs in a terminal, that terminal,
//! labeled as one. Only an explicit request for Chat (the shortcut, the menu rows, the
//! dashboard's Chat control or the Start button) may make an orchestrator, through
//! `orchestrators::create` and as Settings say. Closing the window drops its views only: the
//! chat, its provider and the terminal's session run on, and the window finds them again.
//!
//! The dashboard's rows open what they name in that project's window, raising one that shows
//! the project or opening one for it; the global window stays what it is.

use super::*;
use dashboard::{DashboardAction, DashboardData};
use global_overview::{GlobalOrchestrator, Target};
use riwork_core::global_window::{
    DashboardFilter, DashboardSort, GlobalWindowState, GlobalWindowStore,
};

pub(crate) const TITLE: &str = "RiWork · Global";

/// What a project window says once it has left out the global tabs of an older layout.
pub(crate) const MOVED_NOTICE: &str = "The dashboard and the global chat now have their own window: View › Global Dashboard (⌘⌥G) or Global Chat (⌘⇧O).";

const TERMINAL_ELSEWHERE: &str = "The global orchestrator's terminal is open in another window.";

/// The default size of a new global window.
const DEFAULT_SIZE: WindowSize = WindowSize {
    width: 1100.0,
    height: 760.0,
};

/// Ticks between redraws of a dashboard on screen whose figures did not change: its ages move
/// with the clock.
const DASHBOARD_HEARTBEAT_TICKS: u64 = 5;

/// The open global window, while there is one.
#[derive(Default)]
struct Open {
    window: Option<(gpui::AnyWindowHandle, gpui::WeakEntity<GlobalWindow>)>,
}

impl gpui::Global for Open {}

/// The global window and its view, if one is open.
pub(crate) fn current(cx: &App) -> Option<(gpui::AnyWindowHandle, Entity<GlobalWindow>)> {
    let (handle, view) = cx.try_global::<Open>()?.window.as_ref()?;
    // A closed window is no window to raise, even while its view is still being let go.
    if !cx.windows().contains(handle) {
        return None;
    }
    Some((*handle, view.upgrade()?))
}

/// What the global window shows, if one is open.
pub(crate) fn shown_view(cx: &App) -> Option<GlobalView> {
    current(cx).map(|(_, view)| view.read(cx).view)
}

/// Whether the global window is finding or making the global orchestrator.
pub(crate) fn chat_starting(cx: &App) -> bool {
    current(cx).is_some_and(|(_, view)| view.read(cx).starting)
}

/// Bring the global window up on `view`, opening it when none is open. `start` is an explicit
/// request for Chat: the global orchestrator there is, else a new one as Settings say. It runs
/// once the update that asked is over, since that is often another window's.
pub(crate) fn open(view: GlobalView, start: bool, cx: &mut App) {
    cx.defer(move |cx| {
        if let Err(error) = bring_up(view, start, cx) {
            eprintln!("riwork: cannot open the global window: {error}");
        }
    });
}

fn bring_up(view: GlobalView, start: bool, cx: &mut App) -> Result<(), String> {
    let handle = open_window(None, cx)?;
    with_global_window(handle, cx, |global, window, cx| {
        global.show(view, start, window, cx);
        window.activate_window();
    })?;
    cx.activate(true);
    Ok(())
}

/// The global window: the open one, else a new one, as `restore` (a reload) or the saved
/// state says. A new one starts nothing: its Chat view looks for the orchestrator there is.
pub(crate) fn open_window(
    restore: Option<runtime::RuntimeWindow>,
    cx: &mut App,
) -> Result<gpui::AnyWindowHandle, String> {
    if let Some((handle, _)) = current(cx) {
        return Ok(handle);
    }
    let startup = GlobalStartup::prepare()?;
    let saved = opening_state(restore.as_ref(), &startup.saved);
    let window_bounds = opening_bounds(
        Some(saved.window_size.unwrap_or(DEFAULT_SIZE)),
        restore.as_ref(),
        cx,
    );
    let handle = cx
        .open_window(window_options(window_bounds), |window, cx| {
            metal_layer::limit_drawables(window);
            let view = cx.new(|cx| GlobalWindow::new(startup, saved, window, cx, true));
            cx.new(|cx| gpui_kit::base::Root::new(view, window, cx))
        })
        .map_err(|error| error.to_string())?;
    Ok(handle.into())
}

/// What a new global window shows: what a reload handed over, else what it last showed.
fn opening_state(
    restore: Option<&runtime::RuntimeWindow>,
    saved: &GlobalWindowStore,
) -> GlobalWindowState {
    restore
        .and_then(|window| window.global.clone())
        .unwrap_or_else(|| saved.load())
}

pub(crate) fn with_global_window<R>(
    handle: gpui::AnyWindowHandle,
    cx: &mut App,
    update: impl FnOnce(&mut GlobalWindow, &mut Window, &mut Context<GlobalWindow>) -> R,
) -> Result<Option<R>, String> {
    handle
        .update(cx, |view, window, cx| {
            let content = behavior_controls::content_view::<GlobalWindow>(view, cx)?;
            Some(content.update(cx, |global, cx| update(global, window, cx)))
        })
        .map_err(|error| error.to_string())
}

/// How a new global orchestrator would run, as Settings say now (the global scope has
/// no project choice over it). For the words only: creating reads the files again
/// (`orchestrators::plan`).
pub(crate) fn orchestrator_runs(settings: &Settings) -> settings::OrchestratorRuns {
    settings.orchestrator_runs()
}

// ---- What the dashboard reads beside the sessions, in any window ---------------------------

impl DashboardReads {
    /// Whether to read again: at once with `force`, else once the last read is
    /// `DASHBOARD_READ_EVERY` old. Never while a read runs.
    pub(crate) fn due(&self, force: bool) -> bool {
        !self.pending
            && (force
                || self
                    .read_at
                    .is_none_or(|at| at.elapsed() >= DASHBOARD_READ_EVERY))
    }

    /// Take a finished read; whether it changed anything drawn.
    pub(crate) fn finish(
        &mut self,
        schedules: Result<Vec<schedules::Schedule>, String>,
        written: BTreeMap<String, u64>,
    ) -> bool {
        self.pending = false;
        self.read_at = Some(Instant::now());
        let changed = self.schedules.as_ref() != Some(&schedules) || self.chat_activity != written;
        self.schedules = Some(schedules);
        self.chat_activity = written;
        changed
    }
}

/// The chats whose log times are read, and the reading itself (the automations and those
/// times), off the window: the engine's, which `riwork dashboard` uses too, so the phone's
/// dashboard is sampled as this one is. It starts no agent, chat or chat host.
pub(crate) use global_overview::{read_extras, timed_chats};

// ---- Where a dashboard row goes ------------------------------------------------------------

/// What a click on a dashboard row asks for, worked out from what the window last sampled.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Route {
    /// The global orchestrator: this window's Chat view.
    GlobalChat,
    /// Something of a project: that project's window, and then `step` in it.
    Project { project_id: String, step: Step },
    /// Something of no project, shown in a project window of this Mac: the frontmost one,
    /// else the store's active project.
    AnyProject(Step),
    /// What the row named is gone; the window says so instead.
    Gone(&'static str),
}

/// What a project window does once it is up.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Step {
    Nothing,
    Shell(String),
    Chat(ChatInfo),
    Task(String),
    Panel(PanelKind),
    /// Settings, at the Anthropic API key field.
    ApiKeySettings,
    /// Project Settings, at the project's memo.
    ProjectMemo,
}

pub(crate) fn route(
    target: &Target,
    state: &State,
    shells: &[ShellSession],
    chats: Option<&[ChatInfo]>,
) -> Route {
    match target {
        Target::Shell { id } => match shells.iter().find(|shell| &shell.id == id) {
            None => Route::Gone("That session has ended."),
            Some(shell) if is_global_orchestrator_shell(shell) => Route::GlobalChat,
            Some(shell) => match &shell.project_id {
                Some(project_id) => Route::Project {
                    project_id: project_id.clone(),
                    step: Step::Shell(id.clone()),
                },
                None => Route::Gone("That session belongs to no project."),
            },
        },
        Target::Chat { id, .. } => match chats.into_iter().flatten().find(|chat| &chat.id == id) {
            None => Route::Gone("That chat is gone."),
            Some(chat) if chat.orchestrator == Some(OrchestratorScope::Global) => Route::GlobalChat,
            Some(chat) => match &chat.project_id {
                Some(project_id) => Route::Project {
                    project_id: project_id.clone(),
                    step: Step::Chat(chat.clone()),
                },
                None => Route::Gone("This chat belongs to no project; open it with riwork chat."),
            },
        },
        Target::Project { id } => {
            if state.projects.iter().any(|project| &project.id == id) {
                Route::Project {
                    project_id: id.clone(),
                    step: Step::Nothing,
                }
            } else {
                Route::Gone("That project is gone.")
            }
        }
        Target::Task { id } => match state.tasks.iter().find(|task| &task.id == id) {
            Some(task) => Route::Project {
                project_id: task.project_id.clone(),
                step: Step::Task(id.clone()),
            },
            None => Route::Gone("That task is gone."),
        },
        Target::Automations {
            project_id: Some(project_id),
        } => Route::Project {
            project_id: project_id.clone(),
            step: Step::Panel(PanelKind::Schedules),
        },
        Target::Automations { project_id: None } => {
            Route::AnyProject(Step::Panel(PanelKind::Schedules))
        }
    }
}

impl Workspace {
    /// Do what a dashboard row asked for, in this window of its project.
    fn take_step(&mut self, step: Step, window: &mut Window, cx: &mut Context<Self>) {
        match step {
            Step::Nothing => self.focus_active(window, cx),
            Step::Shell(id) => self.show_shell(&id, window, cx),
            Step::Chat(chat) if chat.orchestrator.is_some() => {
                self.show_orchestrator_chat(chat, window, cx)
            }
            Step::Chat(chat) => self.show_existing_chat(chat, window, cx),
            Step::Task(id) => {
                self.select_task(&id, window, cx);
                self.open_panel(PanelKind::Tasks, self.active_pane, window, cx);
            }
            Step::Panel(panel) => self.open_panel(panel, self.active_pane, window, cx),
            Step::ApiKeySettings => self.reveal_api_key_settings(window, cx),
            Step::ProjectMemo => self.reveal_project_memo(window, cx),
        }
        cx.notify();
    }
}

/// Every window, the frontmost first. The window stack can leave out a minimized window or
/// one on another Space; those follow, so that a project's window is always found.
fn windows_front_first(cx: &App) -> Vec<gpui::AnyWindowHandle> {
    let mut windows = cx.window_stack().unwrap_or_default();
    for handle in cx.windows() {
        if !windows.contains(&handle) {
            windows.push(handle);
        }
    }
    windows
}

/// Run `step` in a window of `project_id` (a window of this Mac's showing it, the frontmost
/// first), or in a new window opened for it by its id. No window changes project for it, and
/// the store's active project stays what it is.
fn in_project_window(project_id: &str, step: Step, cx: &mut App) -> Result<(), String> {
    for handle in windows_front_first(cx) {
        let mut pending = Some(step.clone());
        let done = with_workspace(handle, cx, |workspace, window, cx| {
            if workspace.project_id != project_id {
                return false;
            }
            if let Some(step) = pending.take() {
                workspace.take_step(step, window, cx);
            }
            window.activate_window();
            true
        });
        if matches!(done, Ok(Some(true))) {
            cx.activate(true);
            return Ok(());
        }
    }
    let handle = open_project_window_by_id(project_id, cx)?;
    with_workspace(handle.into(), cx, |workspace, window, cx| {
        workspace.take_step(step, window, cx);
        window.activate_window();
    })?;
    cx.activate(true);
    Ok(())
}

/// Run `step` in the frontmost window of one of this Mac's projects, else in a window of the
/// store's active project. With no project registered there is nowhere to go.
fn in_any_project_window(step: Step, cx: &mut App) -> Result<(), String> {
    for handle in windows_front_first(cx) {
        let project = with_workspace(handle, cx, |workspace, _, _| {
            (!workspace.is_remote()).then(|| workspace.project_id.clone())
        });
        if let Ok(Some(Some(project))) = project {
            return in_project_window(&project, step, cx);
        }
    }
    let active = Store::open_default()?
        .snapshot()?
        .active_project_id
        .ok_or_else(|| "Open one of this Mac's projects first.".to_owned())?;
    in_project_window(&active, step, cx)
}

/// A new window on the project of the frontmost project window, else on the store's active
/// project: ⌘⇧N, and the Dock's New Window, while the global window is the only one. The project
/// is found by its id; nothing is registered for it.
pub(crate) fn open_new_project_window(cx: &mut App) -> Result<(), String> {
    let mut project = None;
    for handle in windows_front_first(cx) {
        if let Ok(Some(Some(id))) = with_workspace(handle, cx, |workspace, _, _| {
            (!workspace.is_remote()).then(|| workspace.project_id.clone())
        }) {
            project = Some(id);
            break;
        }
    }
    let project = match project {
        Some(project) => project,
        None => Store::open_default()?
            .snapshot()?
            .active_project_id
            .ok_or_else(|| "Open one of this Mac's projects first.".to_owned())?,
    };
    let handle = open_project_window_by_id(&project, cx)?;
    let _ = with_workspace(handle.into(), cx, |_, window, _| window.activate_window());
    cx.activate(true);
    Ok(())
}

// ---- The window ----------------------------------------------------------------------------

/// What the global window reads from disk before it opens.
pub(crate) struct GlobalStartup {
    store: Store,
    sessions: SessionManager,
    settings_store: SettingsStore,
    saved: GlobalWindowStore,
    state: State,
    /// How the chat's view reaches the chat host.
    host: HostConfig,
    /// How an explicit Start reaches it (`orchestrators::system_ensure`).
    ensure_host: EnsureHost,
}

/// Makes sure the chat host of a data directory runs and says where it listens.
type EnsureHost = fn(&Path) -> Result<PathBuf, String>;

impl GlobalStartup {
    fn prepare() -> Result<Self, String> {
        let store = Store::open_default()?;
        let sessions = SessionManager::open_default()?;
        let state = store.snapshot()?;
        let home = sessions.state_home().to_path_buf();
        Ok(Self {
            store,
            settings_store: SettingsStore::open_default()?,
            saved: GlobalWindowStore::at(&home),
            state,
            host: HostConfig::for_home(home),
            ensure_host: orchestrators::system_ensure,
            sessions,
        })
    }
}

/// What one read of the sessions and chats found: the sessions, their folders, the chats.
type Sample = (
    Vec<ShellSession>,
    Option<BTreeMap<String, PathBuf>>,
    Option<Vec<ChatInfo>>,
);

/// The global orchestrator the Chat view shows.
enum Shown {
    /// None yet: the view says what there is, and offers to start one when there is none.
    Nothing,
    Chat {
        id: String,
        view: Entity<ChatView>,
    },
    /// A terminal orchestrator. Its session is the orchestrator; the terminal is only this
    /// window's client of it.
    Terminal {
        shell_id: String,
        terminal: Option<Entity<Terminal>>,
        error: Option<String>,
    },
}

pub(crate) struct GlobalWindow {
    pub(crate) view: GlobalView,
    /// The dashboard's own filter and order of the projects, kept with the window's state.
    pub(crate) filter: DashboardFilter,
    pub(crate) sort: DashboardSort,
    /// What a compact dashboard shows; not kept.
    pub(crate) region: dashboard::Region,
    /// The dashboard's search field, kept for the window's life, and its text.
    pub(crate) search: Entity<InputState>,
    pub(crate) query: String,
    _search_events: gpui::Subscription,
    store: Store,
    sessions: SessionManager,
    settings_store: SettingsStore,
    saved: GlobalWindowStore,
    host: HostConfig,
    ensure_host: EnsureHost,
    pub(crate) state: State,
    shells: Vec<ShellSession>,
    shell_cwds: BTreeMap<String, PathBuf>,
    agent_activity: BTreeMap<String, AgentState>,
    activity_tracker: Option<ActivityTracker>,
    /// Every chat, as the chat host or the saved chats say; `None` until first read.
    chats: Option<Vec<ChatInfo>>,
    sampled_at: Option<u64>,
    sample_error: Option<String>,
    sample_pending: bool,
    /// Moves on when the window learns of an orchestrator made after a read began, so that
    /// read, older than what the window knows, is let go instead of taken.
    sample_generation: u64,
    dashboard: DashboardReads,
    shown: Shown,
    /// The global orchestrator is being found or made, after an explicit request.
    starting: bool,
    /// This window's claims on the chat it shows, so no project window adopts it as a tab.
    chat_tabs: chat_tabs::Coordinator,
    settings: Settings,
    appearance: Appearance,
    pub(crate) notice: Option<String>,
    window_size: Option<WindowSize>,
    normal_bounds: Option<Bounds<Pixels>>,
    window_visible: bool,
    ticks: u64,
    /// Off in tests: no sampling, no timers and no terminals.
    services: bool,
    focus: FocusHandle,
}

impl GlobalWindow {
    pub(crate) fn new(
        startup: GlobalStartup,
        saved: GlobalWindowState,
        window: &mut Window,
        cx: &mut Context<Self>,
        services: bool,
    ) -> Self {
        let GlobalStartup {
            store,
            sessions,
            settings_store,
            saved: saved_store,
            state,
            host,
            ensure_host,
        } = startup;
        window.set_window_title(TITLE);
        let activity_tracker = ActivityTracker::at(sessions.state_home().to_path_buf());
        let search =
            text_input::single_line("", "Search projects, tasks and sessions  ⌘F", window, cx);
        // The search only narrows what the dashboard draws: it reads and changes nothing.
        let search_events = cx.subscribe_in(&search, window, |global, input, event, _, cx| {
            if matches!(event, InputEvent::Change) {
                let query = input.read(cx).value().to_string();
                if global.query != query {
                    global.query = query;
                    cx.notify();
                }
            }
        });
        cx.set_global(Open {
            window: Some((window.window_handle(), cx.weak_entity())),
        });
        let id = cx.entity_id();
        cx.on_release(move |global: &mut Self, cx| {
            global.save_state();
            let this = cx
                .try_global::<Open>()
                .and_then(|open| open.window.as_ref())
                .is_some_and(|(_, view)| view.entity_id() == id);
            if this {
                cx.set_global(Open::default());
            }
        })
        .detach();
        let mut global = Self {
            view: saved.view,
            filter: saved.filter(),
            sort: saved.dashboard_sort,
            region: dashboard::Region::default(),
            search,
            query: String::new(),
            _search_events: search_events,
            store,
            sessions,
            settings_store,
            saved: saved_store,
            host,
            ensure_host,
            state,
            shells: Vec::new(),
            shell_cwds: BTreeMap::new(),
            agent_activity: BTreeMap::new(),
            activity_tracker: Some(activity_tracker),
            chats: None,
            sampled_at: None,
            sample_error: None,
            sample_pending: false,
            sample_generation: 0,
            dashboard: DashboardReads::default(),
            shown: Shown::Nothing,
            starting: false,
            chat_tabs: chat_tabs::Coordinator::new(),
            settings: cx.global::<Settings>().clone(),
            appearance: cx.global::<Appearance>().clone(),
            notice: None,
            window_size: saved.window_size,
            normal_bounds: normal_window_bounds(window),
            window_visible: window.is_visible(),
            ticks: 0,
            services,
            focus: cx.focus_handle(),
        };
        cx.observe_global_in::<Settings>(window, |global, window, cx| {
            global.apply_settings(window, cx)
        })
        .detach();
        cx.observe_global_in::<Appearance>(window, |global, window, cx| {
            global.apply_settings(window, cx)
        })
        .detach();
        // Its own keys (⌘W and the rest) reach it before anything in it is clicked.
        global.focus.focus(window, cx);
        if !services {
            return global;
        }
        cx.observe_window_bounds(window, |global, window, cx| {
            tooltip::hide(cx);
            metal_layer::sync_terminal_scale(window);
            if let Some(bounds) = normal_window_bounds(window) {
                global.normal_bounds = Some(bounds);
                global.window_size =
                    WindowSize::new(bounds.size.width.as_f32(), bounds.size.height.as_f32());
            }
        })
        .detach();
        cx.observe_window_appearance(window, |_, _, cx| sync_appearance(cx))
            .detach();
        cx.observe_window_activation(window, |_, window, cx| {
            if window.is_window_active() {
                dock_menu::set_frontmost(window.window_handle().window_id().as_u64(), cx);
            } else {
                tooltip::hide(cx);
            }
        })
        .detach();
        dock_menu::update_window(
            dock_menu::DockWindow {
                id: window.window_handle().window_id().as_u64(),
                project: "Global".to_owned(),
                branch: None,
            },
            cx,
        );
        if window.is_window_active() {
            dock_menu::set_frontmost(window.window_handle().window_id().as_u64(), cx);
        }
        global.refresh(window, cx);
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                if this
                    .update_in(cx, |global, window, cx| global.refresh(window, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        global
    }

    fn state_to_save(&self) -> GlobalWindowState {
        let mut state = GlobalWindowState {
            view: self.view,
            dashboard_sort: self.sort,
            window_size: self.window_size,
            ..GlobalWindowState::default()
        };
        state.set_filter(self.filter);
        state
    }

    /// Keep what the window shows in its own file, never in a project's layout.
    pub(crate) fn save_state(&mut self) {
        let state = self.state_to_save();
        if self.saved.load() != state
            && let Err(error) = self.saved.save(&state)
        {
            self.notice = Some(error);
        }
    }

    pub(crate) fn runtime_window(&self, window: &Window) -> runtime::RuntimeWindow {
        let (mode, bounds) = window_placement(window, self.normal_bounds);
        runtime::RuntimeWindow::global(
            self.sessions.state_home(),
            Some(bounds),
            mode,
            self.state_to_save(),
        )
    }

    /// Switch to `view`. `start` asks for Chat explicitly; see `open`.
    pub(crate) fn show(
        &mut self,
        view: GlobalView,
        start: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.view != view {
            self.view = view;
            self.save_state();
        }
        match view {
            GlobalView::Dashboard => self.refresh_dashboard(true, cx),
            GlobalView::Chat if start => self.start_chat(window, cx),
            GlobalView::Chat => self.sync_chat(window, cx),
        }
        self.sync_terminal_visibility(cx);
        self.focus_view(window, cx);
        cx.notify();
    }

    fn focus_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match (&self.shown, self.view) {
            (Shown::Chat { view, .. }, GlobalView::Chat) => {
                view.update(cx, |view, cx| view.focus(window, cx))
            }
            (
                Shown::Terminal {
                    terminal: Some(terminal),
                    ..
                },
                GlobalView::Chat,
            ) => terminal.update(cx, |terminal, cx| terminal.focus(window, cx)),
            _ => self.focus.focus(window, cx),
        }
    }

    /// A terminal is a native view drawn over the window: it is shown only with the Chat view.
    fn sync_terminal_visibility(&self, cx: &mut Context<Self>) {
        if let Shown::Terminal {
            terminal: Some(terminal),
            ..
        } = &self.shown
        {
            let visible = self.view == GlobalView::Chat;
            terminal.update(cx, |terminal, _| terminal.set_visible(visible));
        }
    }

    fn apply_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let settings = cx.global::<Settings>().clone();
        let appearance = cx.global::<Appearance>().clone();
        let previous = Workspace::terminal_theme(&self.settings, &self.appearance);
        let next = Workspace::terminal_theme(&settings, &appearance);
        let colors_changed =
            previous != next || (next.is_none() && self.appearance.ghostty != appearance.ghostty);
        self.settings = settings;
        self.appearance = appearance;
        let attached = match &self.shown {
            Shown::Terminal {
                shell_id,
                terminal: Some(terminal),
                ..
            } => Some((shell_id.clone(), terminal.clone())),
            _ => None,
        };
        if colors_changed && let Some((shell_id, terminal)) = attached {
            match next {
                Some(theme) => {
                    if let Err(error) =
                        terminal.update(cx, |terminal, _| terminal.update_theme(theme))
                    {
                        self.notice = Some(error);
                    }
                }
                // Back to the native Ghostty config: a new client of the same session.
                None => {
                    let shell = self
                        .shells
                        .iter()
                        .find(|shell| shell.id == shell_id)
                        .cloned();
                    if let Some(shell) = shell {
                        self.show_terminal(shell, window, cx);
                    }
                }
            }
        }
        cx.notify();
    }

    // ---- Sampling --------------------------------------------------------------------------

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ticks += 1;
        self.window_visible = window.is_visible();
        // As a project window does, so that Settings changed elsewhere reach this window
        // when it is the only one open.
        if let Ok(settings) = self.settings_store.load()
            && &settings != cx.global::<Settings>()
        {
            cx.set_global(settings);
        }
        let mut changed = false;
        match self.store.snapshot() {
            Ok(state) if state != self.state => {
                self.state = state;
                changed = true;
            }
            Ok(_) => {}
            Err(error) => {
                changed |= self.notice.as_ref() != Some(&error);
                self.notice = Some(error);
            }
        }
        self.sample(window, cx);
        if self.view == GlobalView::Dashboard && self.window_visible {
            self.refresh_dashboard(false, cx);
        }
        self.refresh_image_scope(cx);
        let heartbeat = self.view == GlobalView::Dashboard
            && self.window_visible
            && self.ticks.is_multiple_of(DASHBOARD_HEARTBEAT_TICKS);
        if changed || heartbeat {
            cx.notify();
        }
    }

    /// The sessions and chats, read off the window. Reading starts no chat host.
    fn sample(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sample_pending {
            return;
        }
        self.sample_pending = true;
        let generation = self.sample_generation;
        let sessions = self.sessions.clone();
        let work = cx.background_executor().spawn(async move {
            let sample = sessions.sample(false)?;
            let home = sessions.state_home();
            let host = orchestrators::ChatHost {
                home,
                ensure: &orchestrators::system_ensure,
            };
            let chats = orchestrators::running_chats(&host)
                .or_else(|| chat::log::read_infos_checked(home).ok());
            Ok::<_, String>((sample.shells, sample.directories, chats))
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = work.await;
            let _ = this.update_in(cx, |global, window, cx| {
                global.sample_pending = false;
                global.sampled(generation, result, window, cx)
            });
        })
        .detach();
    }

    /// A read that began at `generation` is done.
    fn sampled(
        &mut self,
        generation: u64,
        result: Result<Sample, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if generation != self.sample_generation {
            // It began before an orchestrator was made, and would take that one for gone.
            return;
        }
        match result {
            Ok((shells, cwds, chats)) => self.take_sample(shells, cwds, chats, window, cx),
            Err(error) => {
                if self.sample_error.as_ref() != Some(&error) {
                    self.sample_error = Some(error);
                    cx.notify();
                }
            }
        }
    }

    fn take_sample(
        &mut self,
        shells: Vec<ShellSession>,
        cwds: Option<BTreeMap<String, PathBuf>>,
        chats: Option<Vec<ChatInfo>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let fresh = self.sampled_at.is_none() || self.sample_error.is_some();
        self.sampled_at = Some(activity::unix_now());
        self.sample_error = None;
        let mut changed = fresh || self.shells != shells;
        self.shells = shells;
        if let Some(cwds) = cwds {
            changed |= self.shell_cwds != cwds;
            self.shell_cwds = cwds;
        }
        if let Some(chats) = chats {
            changed |= self.chats.as_ref() != Some(&chats);
            self.chats = Some(chats);
        }
        self.refresh_agent_activity(cx);
        self.sync_chat(window, cx);
        self.refresh_image_scope(cx);
        if changed {
            cx.notify();
        }
    }

    fn refresh_agent_activity(&mut self, cx: &mut Context<Self>) {
        let Some(mut tracker) = self.activity_tracker.take() else {
            return;
        };
        let shells = self.shells.clone();
        let home = self.sessions.state_home().to_path_buf();
        let work = cx.background_executor().spawn(async move {
            let activity = tracker.sample_states(&shells, activity::unix_now());
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
            let _ = this.update(cx, |global, cx| {
                global.activity_tracker = Some(tracker);
                if global.agent_activity != activity {
                    global.agent_activity = activity;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Read the automations and the times of failed or waiting chats, off the window.
    fn refresh_dashboard(&mut self, force: bool, cx: &mut Context<Self>) {
        if !self.services || !self.dashboard.due(force) {
            return;
        }
        self.dashboard.pending = true;
        let home = self.sessions.state_home().to_path_buf();
        let chats = timed_chats(self.chats.as_deref());
        let work = cx
            .background_executor()
            .spawn(async move { read_extras(&home, chats) });
        cx.spawn(async move |this, cx| {
            let (schedules, written) = work.await;
            let _ = this.update(cx, |global, cx| {
                if global.dashboard.finish(schedules, written) {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(crate) fn overview(&self) -> global_overview::Overview {
        global_overview::build(&global_overview::Inputs {
            state: &self.state,
            shells: &self.shells,
            cwds: &self.shell_cwds,
            activity: &self.agent_activity,
            chats: self.chats.as_deref(),
            chat_activity: &self.dashboard.chat_activity,
            schedules: self.dashboard.schedules.as_ref(),
            now: activity::unix_now(),
        })
    }

    fn global_orchestrator(&self) -> GlobalOrchestrator {
        global_overview::global_orchestrator(
            &self.shells,
            &self.agent_activity,
            self.chats.as_deref(),
        )
    }

    // ---- The global orchestrator -----------------------------------------------------------

    /// Show the global orchestrator there is, as the sessions and chats last read say: never
    /// one made for it. Nothing happens while the Dashboard is shown.
    fn sync_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.view != GlobalView::Chat || self.starting {
            return;
        }
        match self.global_orchestrator() {
            GlobalOrchestrator::Unknown => {}
            GlobalOrchestrator::Chat { id, .. } => {
                if !matches!(&self.shown, Shown::Chat { id: shown, .. } if *shown == id) {
                    self.show_chat(id, window, cx);
                }
            }
            GlobalOrchestrator::Terminal { shell_id, .. } => {
                let shown = matches!(&self.shown, Shown::Terminal { shell_id: shown, .. } if *shown == shell_id);
                let shell = self
                    .shells
                    .iter()
                    .find(|shell| shell.id == shell_id)
                    .cloned();
                if !shown && let Some(shell) = shell {
                    self.show_terminal(shell, window, cx);
                }
            }
            GlobalOrchestrator::None { .. } => {
                if !matches!(self.shown, Shown::Nothing) {
                    self.forget_shown(cx);
                    cx.notify();
                }
            }
        }
    }

    /// An explicit request for Chat: the global orchestrator there is, else a new one made as
    /// Settings say (`orchestrators::create`), off the window. A second request while it runs
    /// waits for the first.
    fn start_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.starting {
            return;
        }
        // The one there is, as last read, needs no trip to the chat host.
        self.sync_chat(window, cx);
        let live = match &self.shown {
            Shown::Chat { .. } => true,
            Shown::Terminal { shell_id, .. } => self
                .shells
                .iter()
                .any(|shell| &shell.id == shell_id && shell.alive),
            Shown::Nothing => false,
        };
        if live {
            return;
        }
        self.starting = true;
        let sessions = self.sessions.clone();
        let ensure = self.ensure_host;
        let work = cx.background_executor().spawn(async move {
            let home = sessions.state_home().to_path_buf();
            let host = orchestrators::ChatHost {
                home: &home,
                ensure: &ensure,
            };
            let plan = orchestrators::plan(&home, None)?;
            let cwd = sessions::orchestrator_context(&home, None);
            orchestrators::create(
                &sessions,
                &host,
                &OrchestratorScope::Global,
                None,
                cwd,
                None,
                &plan.runs,
            )
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = work.await;
            let _ = this.update_in(cx, |global, window, cx| {
                global.orchestrator_ready(result, window, cx)
            });
        })
        .detach();
        cx.notify();
    }

    fn orchestrator_ready(
        &mut self,
        result: Result<orchestrators::Created, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.starting = false;
        if result.is_ok() {
            self.sample_generation = self.sample_generation.wrapping_add(1);
        }
        // Automatic ran its second choice: say what could not start and why.
        if let Ok(orchestrators::Created {
            fallback: Some(fallback),
            ..
        }) = &result
        {
            self.notice = Some(fallback.clone());
        }
        match result.map(|made| made.orchestrator) {
            Err(error) => self.notice = Some(error),
            Ok(Orchestrator::Chat(chat)) => {
                // Known at once, so the next sample does not take it for gone.
                let chats = self.chats.get_or_insert_with(Vec::new);
                chats
                    .retain(|known| known.id != chat.id && known.orchestrator != chat.orchestrator);
                chats.push(chat.clone());
                if self.view == GlobalView::Chat {
                    self.show_chat(chat.id, window, cx);
                }
            }
            Ok(Orchestrator::Terminal(shell)) => {
                self.shells.retain(|known| known.id != shell.id);
                self.shells.push(shell.clone());
                if self.view == GlobalView::Chat {
                    self.show_terminal(shell, window, cx);
                }
            }
        }
        self.focus_view(window, cx);
        cx.notify();
    }

    fn forget_shown(&mut self, cx: &mut Context<Self>) {
        match std::mem::replace(&mut self.shown, Shown::Nothing) {
            Shown::Nothing => {}
            Shown::Chat { id, .. } => self.chat_tabs.release(&id),
            Shown::Terminal {
                shell_id, terminal, ..
            } => {
                if let Some(terminal) = terminal {
                    terminal.update(cx, |terminal, _| terminal.set_visible(false));
                }
                release_shell(&shell_id);
            }
        }
    }

    /// The global orchestrator's chat, as it is: its own view of the host's chat, which keeps
    /// the transcript, the draft, the provider, permission and model choices by the chat's id.
    fn show_chat(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.forget_shown(cx);
        // Explicit: the chat is this window's. No project window adopts it as a tab.
        self.chat_tabs.claim(&id, true);
        let home = self.sessions.state_home().to_path_buf();
        let config = self.host.clone();
        let view = cx.new(|cx| ChatView::open(id.clone(), config, window, cx));
        let (names, paths) = global_speech_context(&self.state);
        let store = self.store.clone();
        view.update(cx, |view, _| {
            view.set_speech_context(names, paths);
            view.set_catalog_home(home);
            view.set_memo_store(store);
        });
        cx.subscribe_in(
            &view,
            window,
            |global, view, event: &ChatViewEvent, window, cx| {
                global.chat_event(view, event, window, cx)
            },
        )
        .detach();
        self.shown = Shown::Chat { id, view };
        self.refresh_image_scope(cx);
        if self.view == GlobalView::Chat {
            self.focus_view(window, cx);
        }
        cx.notify();
    }

    /// The terminal the global orchestrator runs in: a client of its session, which RiWork
    /// keeps as it is. It is never turned into a chat or ended here.
    fn show_terminal(&mut self, shell: ShellSession, window: &mut Window, cx: &mut Context<Self>) {
        self.forget_shown(cx);
        if !claim_shell(&shell.id) {
            self.notice = Some(TERMINAL_ELSEWHERE.into());
            cx.notify();
            return;
        }
        if self.notice.as_deref() == Some(TERMINAL_ELSEWHERE) {
            self.notice = None;
        }
        let (terminal, error) = if self.services {
            match self.spawn_terminal(&shell, window, cx) {
                Ok(terminal) => (Some(terminal), None),
                Err(error) => (None, Some(error)),
            }
        } else {
            (None, None)
        };
        self.shown = Shown::Terminal {
            shell_id: shell.id,
            terminal,
            error,
        };
        self.sync_terminal_visibility(cx);
        if self.view == GlobalView::Chat {
            self.focus_view(window, cx);
        }
        cx.notify();
    }

    fn spawn_terminal(
        &self,
        shell: &ShellSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Entity<Terminal>, String> {
        runtime::note_attach_started(&shell.id, &shell.cwd);
        let spawned = self.sessions.attach_command(&shell.id).and_then(|command| {
            let mut options = Workspace::terminal_options(
                command,
                shell.cwd.clone(),
                Workspace::terminal_theme(&self.settings, &self.appearance),
            );
            options.focus_on_spawn = self.view == GlobalView::Chat;
            Terminal::spawn(options, window, cx)
        });
        runtime::note_attach_finished(&shell.id, spawned.is_ok());
        spawned
    }

    /// The image cards of the global chat read from every registered project and the global
    /// orchestrator's folders, worked out from the chat's identity as project windows do.
    fn refresh_image_scope(&mut self, cx: &mut Context<Self>) {
        let Shown::Chat { view, .. } = &self.shown else {
            return;
        };
        let view = view.clone();
        let scope = self.image_scope(&view, cx);
        if view.read(cx).image_scope() != &scope {
            view.update(cx, |view, cx| view.set_image_scope(scope, cx));
        }
    }

    fn image_scope(&self, view: &Entity<ChatView>, cx: &App) -> chat_view::ImageScope {
        let Some(info) = view.read(cx).info() else {
            return chat_view::ImageScope::default();
        };
        let listed = self.chats.iter().flatten().find(|chat| chat.id == info.id);
        // It has no Files of its own: Open in Files preview is a project window's.
        chat_view::ImageScope::for_chat(info, listed, &self.state, self.sessions.state_home(), None)
    }

    fn chat_event(
        &mut self,
        view: &Entity<ChatView>,
        event: &ChatViewEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ours = matches!(&self.shown, Shown::Chat { view: shown, .. } if shown == view);
        if !ours {
            return;
        }
        match event {
            ChatViewEvent::Changed => {
                self.refresh_image_scope(cx);
                cx.notify();
            }
            // Only a chat being made says so; this one exists.
            ChatViewEvent::Created(_) => {}
            ChatViewEvent::Close => {
                // Deleted: the window offers to start one again.
                self.forget_shown(cx);
                self.focus_view(window, cx);
                cx.notify();
            }
            ChatViewEvent::HandOff | ChatViewEvent::NewChat | ChatViewEvent::WorkerShell { .. } => {
                self.notice = Some("That works on a project's chats, in its window.".into());
                cx.notify();
            }
            ChatViewEvent::OpenFile { .. } => {
                self.notice = Some(
                    "The global chat has no Files; open the file in its project's window.".into(),
                );
                cx.notify();
            }
            ChatViewEvent::ShowUsage => {
                self.go(Route::AnyProject(Step::Panel(PanelKind::Usage)), cx)
            }
            ChatViewEvent::OpenApiKeySettings => {
                self.go(Route::AnyProject(Step::ApiKeySettings), cx)
            }
            // The global chat has no project, so its preview offers no Edit; a project's
            // chat shown here edits in that project's window.
            ChatViewEvent::EditProjectMemo { project_id } => self.go(
                Route::Project {
                    project_id: project_id.clone(),
                    step: Step::ProjectMemo,
                },
                cx,
            ),
        }
    }

    // ---- Clicks ----------------------------------------------------------------------------

    pub(crate) fn dashboard_action(
        &mut self,
        action: DashboardAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            DashboardAction::Refresh => {
                self.refresh_dashboard(true, cx);
                if self.services {
                    self.sample(window, cx);
                }
                cx.notify();
            }
            DashboardAction::Filter(filter) => {
                if self.filter != filter {
                    self.filter = filter;
                    self.save_state();
                }
                cx.notify();
            }
            DashboardAction::Sort(sort) => {
                if self.sort != sort {
                    self.sort = sort;
                    self.save_state();
                }
                cx.notify();
            }
            DashboardAction::Region(region) => {
                self.region = region;
                cx.notify();
            }
            DashboardAction::ClearSearch => {
                crate::form_input::set_value(&self.search, String::new(), window, cx);
                self.query.clear();
                cx.notify();
            }
            DashboardAction::GlobalChat => self.show(GlobalView::Chat, true, window, cx),
            DashboardAction::ProjectPanel { project_id, panel } => self.go(
                Route::Project {
                    project_id,
                    step: Step::Panel(panel),
                },
                cx,
            ),
            DashboardAction::Open(target) => {
                let route = route(&target, &self.state, &self.shells, self.chats.as_deref());
                match route {
                    Route::GlobalChat => self.show(GlobalView::Chat, false, window, cx),
                    route => self.go(route, cx),
                }
            }
        }
    }

    /// Go where a click asked, in a project window; this window stays as it is.
    fn go(&mut self, route: Route, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        match route {
            Route::Gone(notice) => {
                self.notice = Some(notice.to_owned());
                cx.notify();
            }
            Route::GlobalChat => open(GlobalView::Chat, false, cx),
            Route::Project { project_id, step } => cx.defer(move |cx| {
                if let Err(error) = in_project_window(&project_id, step, cx) {
                    let _ = this.update(cx, |global, cx| {
                        global.notice = Some(error);
                        cx.notify();
                    });
                }
            }),
            Route::AnyProject(step) => cx.defer(move |cx| {
                if let Err(error) = in_any_project_window(step, cx) {
                    let _ = this.update(cx, |global, cx| {
                        global.notice = Some(error);
                        cx.notify();
                    });
                }
            }),
        }
    }

    /// ⌘F: the dashboard's search. In the Chat view the key is the chat's.
    fn focus_search_action(
        &mut self,
        _: &FocusSearch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.view != GlobalView::Dashboard {
            cx.propagate();
            return;
        }
        self.search.read(cx).focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn close_action(&mut self, _: &CloseTab, window: &mut Window, _: &mut Context<Self>) {
        // The window has no tabs: ⌘W closes it. The chat and the terminal session run on.
        window.remove_window();
    }

    fn new_project_window_action(
        &mut self,
        _: &NewProjectWindow,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let this = cx.entity().downgrade();
        cx.defer(move |cx| {
            if let Err(error) = open_new_project_window(cx) {
                let _ = this.update(cx, |global, cx| {
                    global.notice = Some(error);
                    cx.notify();
                });
            }
        });
    }

    fn open_settings_action(&mut self, _: &OpenSettings, _: &mut Window, cx: &mut Context<Self>) {
        self.go(Route::AnyProject(Step::Panel(PanelKind::Settings)), cx);
    }

    fn open_schedules_action(&mut self, _: &OpenSchedules, _: &mut Window, cx: &mut Context<Self>) {
        self.go(Route::AnyProject(Step::Panel(PanelKind::Schedules)), cx);
    }

    // ---- Drawing ---------------------------------------------------------------------------

    fn render_bar(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = theme::palette(cx);
        let native = ui_text::is_native();
        let inset = if window_controls_visible(window) {
            WINDOW_CONTROLS_CONTENT_INSET
        } else {
            10.0
        };
        let segment =
            |id: &'static str, label: &'static str, view: GlobalView, cx: &mut Context<Self>| {
                let selected = self.view == view;
                behavior_controls::segment(id, label, selected, colors)
                    .text_size(ui_text::text(10.0))
                    .on_change({
                        let listener =
                            cx.listener(move |global, _: &gpui::ClickEvent, window, cx| {
                                global.show(view, false, window, cx);
                            });
                        move |_, event, window, cx| listener(event, window, cx)
                    })
            };
        div()
            .id("global-bar")
            .flex()
            .flex_none()
            .items_center()
            .gap(ui_text::space(10.0))
            .h(px(WINDOW_CONTROLS_HEIGHT + 6.0))
            .pl(px(inset))
            .pr(ui_text::space(10.0))
            .bg(rgb(colors.panel))
            .border_b_1()
            .border_color(rgb(colors.divider))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(ui_text::space(5.0))
                    .text_color(rgb(colors.magenta))
                    .child(icons::text_icon(
                        Icon::Panel(PanelKind::Dashboard),
                        10.0,
                        colors.magenta,
                    ))
                    .child(if native {
                        div().child("Global")
                    } else {
                        div().child(ui_text::cased("Global"))
                    }),
            )
            .child(
                behavior_controls::segments("global-views", "Global views", colors)
                    .flex_none()
                    .child(segment(
                        "global-view-dashboard",
                        "Dashboard",
                        GlobalView::Dashboard,
                        cx,
                    ))
                    .child(segment("global-view-chat", "Chat", GlobalView::Chat, cx)),
            )
            // The rest of the bar moves the window, as a title bar does.
            .child(
                div()
                    .id("global-bar-drag")
                    .flex_1()
                    .h_full()
                    .on_mouse_down(MouseButton::Left, start_window_drag),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(ui_text::text(9.0))
                    .text_color(rgb(colors.muted))
                    .child("This Mac · every project"),
            )
            .into_any_element()
    }

    fn render_notice(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let notice = self.notice.clone()?;
        let colors = theme::palette(cx);
        Some(
            div()
                .id("global-notice")
                .flex()
                .flex_none()
                .items_center()
                .gap(ui_text::space(8.0))
                .px(ui_text::space(10.0))
                .py(ui_text::space(4.0))
                .bg(rgb(colors.panel_active))
                .border_t_1()
                .border_color(rgb(colors.divider))
                .text_size(ui_text::text(10.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(rgb(colors.gold))
                        .child(notice),
                )
                .child(crate::form_input::control_element(
                    "global-notice-dismiss",
                    behavior_controls::button(
                        "global-notice-dismiss",
                        "Dismiss",
                        controls::Button::Secondary,
                        colors,
                    )
                    .text_size(ui_text::text(9.0))
                    .on_click(cx.listener(|global, _, _, cx| {
                        global.notice = None;
                        cx.notify();
                    })),
                ))
                .into_any_element(),
        )
    }

    fn render_dashboard(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let overview = self.overview();
        dashboard::render(
            DashboardData {
                overview: &overview,
                now: activity::unix_now(),
                sampled_at: self.sampled_at,
                sample_error: self.sample_error.as_deref(),
                chat_starting: self.starting,
                runs: orchestrator_runs(&self.settings),
                width: window.viewport_size().width,
                search: Some(&self.search),
                query: &self.query,
                filter: self.filter,
                sort: self.sort,
                region: self.region,
            },
            Self::dashboard_action,
            window,
            cx,
        )
    }

    fn render_chat(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = theme::palette(cx);
        match &self.shown {
            Shown::Chat { view, .. } => div()
                .id("global-chat")
                .size_full()
                .child(view.clone())
                .into_any_element(),
            Shown::Terminal {
                terminal, error, ..
            } => {
                let body = match terminal {
                    Some(terminal) => terminal.clone().into_any_element(),
                    None => div()
                        .size_full()
                        .p(ui_text::space(14.0))
                        .bg(rgb(colors.bg))
                        .text_color(rgb(colors.muted))
                        .child(error.clone().unwrap_or_else(|| "Attaching…".to_owned()))
                        .into_any_element(),
                };
                div()
                    .id("global-terminal")
                    .size_full()
                    .flex()
                    .flex_col()
                    // The label sits above the terminal, not over it: the native surface would
                    // draw above anything laid on top.
                    .child(
                        div()
                            .id("global-terminal-label")
                            .flex_none()
                            .px(ui_text::space(10.0))
                            .py(ui_text::space(4.0))
                            .bg(rgb(colors.panel))
                            .border_b_1()
                            .border_color(rgb(colors.divider))
                            .text_size(ui_text::text(10.0))
                            .child(
                                div()
                                    .text_color(rgb(colors.text))
                                    .child("Terminal orchestrator: the global orchestrator runs in a terminal"),
                            )
                            .child(div().text_color(rgb(colors.muted)).child(
                                "RiWork keeps this terminal as it is; a chat takes over only after you end it.",
                            )),
                    )
                    .child(div().flex_1().min_h_0().child(body))
                    .into_any_element()
            }
            Shown::Nothing => self.render_start(cx),
        }
    }

    /// No orchestrator shown: what there is, and the one control that may start one.
    fn render_start(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = theme::palette(cx);
        let copy = dashboard::chat_copy(
            &self.global_orchestrator(),
            &orchestrator_runs(&self.settings),
            self.starting,
        );
        let kind = match (copy.enabled, copy.starts) {
            (false, _) => controls::Button::Disabled,
            (true, true) => controls::Button::Primary,
            (true, false) => controls::Button::Secondary,
        };
        let button =
            behavior_controls::button("global-chat-start", copy.action.clone(), kind, colors)
                .text_size(ui_text::text(11.0))
                .on_click(cx.listener(|global, _, window, cx| {
                    global.show(GlobalView::Chat, true, window, cx)
                }));
        div()
            .id("global-chat-start-card")
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(ui_text::space(6.0))
            .p(ui_text::space(24.0))
            .bg(rgb(colors.panel))
            .text_size(ui_text::text(11.0))
            .child(
                div()
                    .text_color(rgb(colors.text))
                    .child(copy.status.clone()),
            )
            .children(
                copy.note
                    .clone()
                    .map(|note| div().text_color(rgb(colors.muted)).child(note)),
            )
            .child(crate::form_input::control_element(
                "global-chat-start",
                button,
            ))
            .into_any_element()
    }
}

impl Drop for GlobalWindow {
    fn drop(&mut self) {
        // Its claims go with it; the chat and the terminal session do not.
        self.chat_tabs.release_all();
        if let Shown::Terminal { shell_id, .. } = &self.shown {
            release_shell(shell_id);
        }
    }
}

/// Dictation hears the names and folders of every project: the global chat works across them.
fn global_speech_context(state: &State) -> (Vec<String>, Vec<String>) {
    let mut names = Vec::new();
    let mut paths = Vec::new();
    for project in state.projects.iter().take(64) {
        let (project_names, project_paths) = speech_context(state, &project.id);
        names.extend(project_names);
        paths.extend(project_paths);
    }
    (names, paths)
}

impl Render for GlobalWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme::palette(cx);
        let body = match self.view {
            GlobalView::Dashboard => self.render_dashboard(window, cx),
            GlobalView::Chat => self.render_chat(cx),
        };
        div()
            .id("riwork-global")
            .relative()
            .key_context("RiWorkGlobal")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::close_action))
            .on_action(cx.listener(Self::focus_search_action))
            .on_action(cx.listener(Self::new_project_window_action))
            .on_action(cx.listener(Self::open_settings_action))
            .on_action(cx.listener(Self::open_schedules_action))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(colors.bg))
            .text_color(rgb(colors.text))
            .font_family(ui_text::ui_family())
            .child(self.render_bar(window, cx))
            .child(div().flex_1().min_h_0().child(body))
            .children(self.render_notice(cx))
            .children(window_controls_visible(window).then(|| window_controls_island(cx)))
    }
}

#[cfg(test)]
pub(crate) mod tests;
