//! AppKit owns windows; every window owns an independent NSH engine/channel.
//! Disk, runtime registry and project resolution live on the service thread.
use crate::{
    nsh_chrome::{create_window, position_controls},
    nsh_menus::{DockWindow, HostMenus, MenuAction},
    nsh_notifications::{self as notices, Notification, Notifier},
    nsh_runtime::{DesktopInstance, HostRuntime, InstanceStart, OpenRequest},
};
use nsh_engine::{HostCall, HostChannel};
use nsh_platform_macos::{
    App, AppOptions, AppView, Appearance, AssetBase, Bundle, CacheLocation, CacheOptions,
    Components,
    objc2::{
        DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, rc::Retained,
        runtime::ProtocolObject,
    },
    objc2_app_kit::{
        NSAppearance, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSApplication,
        NSApplicationActivationPolicy, NSApplicationDelegate, NSApplicationTerminateReply, NSEvent,
        NSEventModifierFlags, NSEventType, NSFontManager, NSMenu, NSScreen, NSWindow,
        NSWindowDelegate, NSWindowOcclusionState, NSWindowStyleMask,
    },
    objc2_foundation::{
        NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
    },
    set_ui_sets,
};
use riwork_core::{
    global_window::{GlobalView, GlobalWindowState, GlobalWindowStore},
    layouts::{LayoutStore, WindowFrame, WindowSize},
    paths,
    settings::SettingsStore,
    store::Store,
};
use riwork_engine::runtime::{
    ReloadLaunch, ReloadRequest, RuntimeWindow, WindowGeometry, WindowKind, WindowMode,
};
use riwork_nsh_native as native;
use riwork_nsh_workspace::host_layouts::{self, HostLayout};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

const APP_ID: &str = "dev.riwork.shell";
const CAPABILITIES: &str = r#"{"host":["window.*","app.*","system.*"]}"#;

fn parse_appearance(value: &str) -> Result<Appearance, String> {
    match value {
        "light" => Ok(Appearance::Light),
        "dark" => Ok(Appearance::Dark),
        "system" => Ok(Appearance::System),
        _ => Err("invalid: appearance must be light, dark or system".into()),
    }
}

fn forced_appearance(value: Option<&str>) -> Result<Option<Appearance>, String> {
    value
        .filter(|value| !value.is_empty())
        .map(parse_appearance)
        .transpose()
}

fn appearance_name(value: Appearance) -> &'static str {
    match value {
        Appearance::Light => "light",
        Appearance::Dark => "dark",
        Appearance::System => "system",
    }
}

fn pin_application(app: &NSApplication, appearance: Appearance) {
    let name = match appearance {
        Appearance::Light => Some(unsafe { NSAppearanceNameAqua }),
        Appearance::Dark => Some(unsafe { NSAppearanceNameDarkAqua }),
        Appearance::System => None,
    };
    let value = name.and_then(NSAppearance::appearanceNamed);
    app.setAppearance(value.as_deref());
}

// Domain persistence belongs to riwork/native. In particular, two AppViews
// must never race to write the same nsh/storage storage.json.
struct TransientStorage;
impl nsh_core::storage::StorageBackend for TransientStorage {
    fn load(&self) -> Result<BTreeMap<String, String>, String> {
        Ok(BTreeMap::new())
    }
    fn write(&self, _change: nsh_core::storage::StorageChange) {}
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OpenOptions {
    kind: WindowKind,
    project_id: Option<String>,
    path: Option<PathBuf>,
    view: Option<String>,
    start: Option<bool>,
    activate: Option<bool>,
}
impl OpenOptions {
    fn project(path: Option<PathBuf>) -> Self {
        Self {
            kind: WindowKind::Project,
            project_id: None,
            path,
            view: None,
            start: None,
            activate: None,
        }
    }
    fn global() -> Self {
        Self {
            kind: WindowKind::Global,
            ..Self::project(None)
        }
    }
    fn validate(&self) -> Result<(), String> {
        if self.kind == WindowKind::Global && (self.project_id.is_some() || self.path.is_some()) {
            return Err("invalid: global windows do not take a project or path".into());
        }
        if self
            .view
            .as_deref()
            .is_some_and(|v| !matches!(v, "dashboard" | "chat"))
        {
            return Err("invalid: global view must be dashboard or chat".into());
        }
        if self.kind == WindowKind::Project && (self.view.is_some() || self.start.is_some()) {
            return Err("invalid: view and start belong to the global window".into());
        }
        Ok(())
    }
}

struct PreparedWindow {
    options: OpenOptions,
    runtime: RuntimeWindow,
    opening_size: WindowSize,
    restore: bool,
    notice: Option<String>,
}
impl PreparedWindow {
    fn params(&self, id: &str, cascade: usize) -> Value {
        let mut params = json!({
            "schema": 1, "windowId": id, "kind": self.runtime.kind,
            "projectId": self.runtime.project_id, "path": self.runtime.path,
            "cascade": cascade, "notice": self.notice, "appearance": "system",
            "build": {"version": env!("CARGO_PKG_VERSION"),
                "commit": option_env!("RIWORK_NSH_BUILD_COMMIT").unwrap_or("unknown"),
                "debug": cfg!(debug_assertions)}
        });
        if self.runtime.is_global() {
            params["global"] = json!({"view": self.options.view.as_deref().unwrap_or("dashboard"),
                "start": self.options.start.unwrap_or(false)});
        }
        if self.restore {
            params["restore"] = json!({"layout": self.runtime.layout,
                "focusMode": self.runtime.focus_mode, "focusCentered": self.runtime.focus_centered,
                "global": self.runtime.global});
        }
        params
    }
}

/// Why a window is opened or reused, and what it is told afterwards.
#[derive(Default)]
struct OpenIntent {
    /// The `window.open` / `window.route` call to answer.
    call: Option<HostCall>,
    /// `window.route`: deliver `route {step}`; the call answers `{windowId}`.
    route: Option<Value>,
    /// `riwork open` (possibly forwarded): deliver `project.open`.
    forwarded: bool,
    /// A notification click: deliver `notification.activate`.
    activate: Option<Value>,
}
impl OpenIntent {
    fn call(call: HostCall) -> Self {
        Self {
            call: Some(call),
            ..Self::default()
        }
    }
    fn forwarded() -> Self {
        Self {
            forwarded: true,
            ..Self::default()
        }
    }
    /// These go to a window already showing the project; a plain open makes a new one.
    fn reuses_project_window(&self) -> bool {
        self.route.is_some() || self.forwarded || self.activate.is_some()
    }
}

/// What the registry knows about one open window, for the pure decisions below.
#[derive(Clone, Debug, PartialEq)]
struct WindowFacts {
    id: String,
    kind: WindowKind,
    /// The project the window shows now (it follows in-window project switches).
    project_id: Option<String>,
    /// Focus recency, the z-order proxy: higher was key more recently; 0 = never key.
    focus_serial: u64,
    minimized: bool,
    on_active_space: bool,
    /// Closed but still being released: no longer counts as open.
    closing: bool,
}

/// Windows in search order (SHELL-062): on-screen windows front to back, then
/// minimized windows and windows on other Spaces, also most recent first.
fn search_order(windows: &[WindowFacts]) -> Vec<&WindowFacts> {
    let mut order: Vec<_> = windows.iter().filter(|w| !w.closing).collect();
    order.sort_by_key(|w| {
        (
            w.minimized || !w.on_active_space,
            std::cmp::Reverse(w.focus_serial),
        )
    });
    order
}

/// `window.route`, forwarded opens and notification clicks: the first project
/// window showing `project_id` in search order. The global window never is one.
fn route_target(windows: &[WindowFacts], project_id: &str) -> Option<String> {
    search_order(windows)
        .into_iter()
        .find(|w| w.kind == WindowKind::Project && w.project_id.as_deref() == Some(project_id))
        .map(|w| w.id.clone())
}

/// The one global window, if open.
fn global_window(windows: &[WindowFacts]) -> Option<String> {
    windows
        .iter()
        .find(|w| w.kind == WindowKind::Global && !w.closing)
        .map(|w| w.id.clone())
}

#[derive(Debug, PartialEq)]
enum OpenTarget {
    Reuse(String),
    Create,
}

/// Main::Open: the global window is a singleton (focus it, never a second);
/// routed, forwarded and notification opens reuse the project's window; any
/// other project open makes a new window. Valid with no window at all.
fn open_target(
    runtime: &RuntimeWindow,
    reuse_project_window: bool,
    windows: &[WindowFacts],
) -> OpenTarget {
    let existing = if runtime.is_global() {
        global_window(windows)
    } else if reuse_project_window {
        runtime
            .project_id
            .as_deref()
            .and_then(|project| route_target(windows, project))
    } else {
        None
    };
    existing.map_or(OpenTarget::Create, OpenTarget::Reuse)
}

fn global_view(options: &OpenOptions) -> &str {
    options.view.as_deref().unwrap_or("dashboard")
}

/// The events the opened or reused window receives, in order.
fn open_events(prepared: &PreparedWindow, intent: &OpenIntent) -> Vec<(&'static str, Value)> {
    let mut events = Vec::new();
    if prepared.runtime.is_global() {
        events.push((
            "global.show",
            json!({"view": global_view(&prepared.options),
                "start": prepared.options.start.unwrap_or(false)}),
        ));
    }
    if intent.forwarded {
        events.push((
            "project.open",
            json!({"projectId": prepared.runtime.project_id, "path": prepared.runtime.path}),
        ));
    }
    if let Some(step) = &intent.route {
        events.push(("route", json!({"step": step})));
    }
    if let Some(activate) = &intent.activate {
        events.push(("notification.activate", activate.clone()));
    }
    events
}

/// `window.open` answers `{windowId, created}`; `window.route` answers `{windowId}`.
fn open_result(id: &str, created: bool, routed: bool) -> Value {
    if routed {
        json!({"windowId": id})
    } else {
        json!({"windowId": id, "created": created})
    }
}

/// View ▸ Global Dashboard / Global Chat, ⌘⌥G and ⌘⇧O: these need no window.
fn global_open_options(command: &str) -> Option<OpenOptions> {
    let chat = match command {
        "openGlobalDashboard" => false,
        "openGlobalChat" => true,
        _ => return None,
    };
    let mut options = OpenOptions::global();
    options.view = Some(if chat { "chat" } else { "dashboard" }.into());
    options.start = Some(chat);
    Some(options)
}

/// A local project id is a UUID; a remote project's key is not.
fn is_local_project(id: &str) -> bool {
    Uuid::parse_str(id).is_ok()
}

#[derive(Debug, PartialEq)]
enum NewWindow {
    /// Open a window on the project of the most recently focused local project window.
    Project(String),
    /// Only the global window is open: its TSX resolves the frontmost project
    /// window, else the active project, else its notice (SHELL-044).
    AskGlobal(String),
    /// No window: the active project, else the cwd fallback (`prepare`).
    Default,
}

/// Dock "New Window" (and reopen). Remote-project windows are skipped.
fn new_window_plan(windows: &[WindowFacts]) -> NewWindow {
    let project = windows
        .iter()
        .filter(|w| w.kind == WindowKind::Project && !w.closing)
        .filter_map(|w| {
            w.project_id
                .as_deref()
                .filter(|id| is_local_project(id))
                .map(|id| (w.focus_serial, id))
        })
        .max_by_key(|(serial, _)| *serial);
    match (project, global_window(windows)) {
        (Some((_, project)), _) => NewWindow::Project(project.to_owned()),
        (None, Some(global)) => NewWindow::AskGlobal(global),
        (None, None) => NewWindow::Default,
    }
}

fn new_window_command() -> Value {
    json!({"id": "newWindow"})
}

fn window_title(runtime: &RuntimeWindow) -> &'static str {
    if runtime.is_global() {
        "RiWork · Global"
    } else {
        "RiWork"
    }
}

/// A `system.notify` post, remembered so its click reaches the right window.
#[derive(Clone)]
struct PostedNotice {
    tag: String,
    window: String,
    project_id: Option<String>,
    target: Option<Value>,
}
const POSTED_NOTICES: usize = 64;

// Service commands contain plain data only. No AppKit object leaves the main thread.
enum Service {
    Snapshot(Vec<(String, RuntimeWindow)>),
    Ready(String),
    Open(OpenOptions, OpenIntent),
    /// A finished-agent notification was clicked: verify, then route `showShell`.
    OpenAgent {
        tag: String,
        project_id: String,
        shell_id: String,
    },
    Close(String, ClosingWindows, Option<HostCall>),
    Quit(ClosingWindows),
    PrepareQuit,
    QueueRestart(HostCall),
    Restart(ReloadRequest, ClosingWindows),
    RestartSkipped,
    TextSize(&'static str),
    Finished,
    Wake,
}
enum Main {
    Call(String, &'static str, HostCall),
    Open(PreparedWindow, OpenIntent),
    Notify(Notification),
    NotificationResponse(String, Option<String>),
    State(String),
    Close(String),
    Closed(
        String,
        Option<HostCall>,
        Result<(), (String, ClosingWindows)>,
    ),
    Recovery(Recovery),
    Restart(ReloadRequest),
    SettingsChanged(f32),
    Menu(MenuAction),
    Quit,
    QuitPrepared,
    Finish,
    DrainTerminals,
    #[cfg(feature = "terminal-fixture")]
    Fixture(Value),
    Error(String),
}
// Kept independent of AppKit so headless fixtures exercise the same queued
// recovery dispatch and call admission as the real main-thread host.
enum Recovery {
    CloseCancelled(String),
    QuitFailed(String, ClosingWindows),
}
impl Recovery {
    fn handle(self, quitting: &mut bool, mut notify: impl FnMut(&str)) {
        match self {
            Self::CloseCancelled(id) => notify(&id),
            Self::QuitFailed(error, batch) => {
                // One main-thread transition: admit calls before reopening any
                // native gate or waking a surviving App's pending opener.
                *quitting = false;
                batch.cancel_with(&mut notify);
                eprintln!("riwork: quit cancelled: {error}");
            }
        }
    }
}

fn admit_host_call(
    window: Option<&RuntimeWindow>,
    quitting: bool,
    id: &str,
    name: &str,
) -> Result<(), &'static str> {
    let window = window.ok_or("not_found: calling window")?;
    if quitting {
        return Err("unavailable: host is quitting");
    }
    if name == "app.restoreReady" && !window.is_global() && host_layouts::lookup(id).is_none() {
        return Err("unavailable: project layout is not ready");
    }
    Ok(())
}

// The closure contains failure bookkeeping/forwarding IO on the service thread.
fn complete_failed_quit(
    batch: ClosingWindows,
    error: String,
    complete: impl FnOnce(),
    dispatch: impl Fn(Main),
) {
    complete();
    // Keep the native close guards and retained layouts until the main thread
    // can reopen call admission and notify Apps in the same turn.
    dispatch(Main::Recovery(Recovery::QuitFailed(error, batch)));
}

thread_local! { static HOST: RefCell<Option<Host>> = const { RefCell::new(None) }; }
fn send(message: Main) {
    dispatch2::DispatchQueue::main().exec_async(move || {
        HOST.with(|host| {
            if let Some(host) = host.borrow_mut().as_mut() {
                host.handle(message);
            }
        });
    });
}

struct DelegateState {
    id: Option<String>,
}
define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = DelegateState]
    #[name = "RiWorkNshHostDelegate"]
    struct Delegate;
    unsafe impl NSObjectProtocol for Delegate {}
    unsafe impl NSApplicationDelegate for Delegate {
        #[unsafe(method_id(applicationDockMenu:))]
        fn dock_menu(&self, _app: &NSApplication) -> Option<Retained<NSMenu>> {
            HOST.with(|host| host.borrow().as_ref().map(Host::dock_menu))
        }
        #[unsafe(method(applicationShouldTerminate:))]
        fn terminate(&self, _app: &NSApplication) -> NSApplicationTerminateReply {
            send(Main::Quit);
            NSApplicationTerminateReply::TerminateCancel
        }
        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn after_last(&self, _app: &NSApplication) -> bool {
            false
        }
        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn reopen(&self, _app: &NSApplication, _visible: bool) -> bool {
            send(Main::Menu(MenuAction {
                window_number: None,
                command: "newWindow",
            }));
            false
        }
        #[unsafe(method(applicationDidBecomeActive:))]
        fn active(&self, _note: &NSNotification) {
            send(Main::State(String::new()));
        }
        #[unsafe(method(applicationDidResignActive:))]
        fn inactive(&self, _note: &NSNotification) {
            send(Main::State(String::new()));
        }
    }
    unsafe impl NSWindowDelegate for Delegate {
        #[unsafe(method(windowShouldClose:))]
        fn close(&self, _window: &NSWindow) -> bool {
            if let Some(id) = &self.ivars().id {
                send(Main::Close(id.clone()));
            }
            false
        }
        #[unsafe(method(windowWillClose:))]
        fn will_close(&self, _note: &NSNotification) {
            if let Some(id) = &self.ivars().id {
                send(Main::Close(id.clone()));
            }
        }
        #[unsafe(method(windowDidBecomeKey:))]
        fn key(&self, _note: &NSNotification) {
            self.changed();
        }
        #[unsafe(method(windowDidResignKey:))]
        fn resigned(&self, _note: &NSNotification) {
            self.changed();
        }
        #[unsafe(method(windowDidResize:))]
        fn resized(&self, _note: &NSNotification) {
            self.changed();
        }
        #[unsafe(method(windowDidUpdate:))]
        fn updated(&self, note: &NSNotification) {
            // AppKit's titlebar layout can reset standard-button positions
            // after ordering or resizing. Correct chrome only: queueing a
            // host state event on every AppKit update can cause idle work.
            if let Some(object) = note.object()
                && let Some(window) = object.downcast_ref::<NSWindow>()
            {
                position_controls(window);
            }
        }
        #[unsafe(method(windowDidMove:))]
        fn moved(&self, _note: &NSNotification) {
            self.changed();
        }
        #[unsafe(method(windowDidChangeOcclusionState:))]
        fn occlusion(&self, _note: &NSNotification) {
            self.changed();
        }
        #[unsafe(method(windowDidChangeBackingProperties:))]
        fn backing(&self, _note: &NSNotification) {
            self.changed();
        }
        #[unsafe(method(windowDidMiniaturize:))]
        fn miniaturized(&self, _note: &NSNotification) {
            self.changed();
        }
        #[unsafe(method(windowDidDeminiaturize:))]
        fn deminiaturized(&self, _note: &NSNotification) {
            self.changed();
        }
        #[unsafe(method(windowDidEnterFullScreen:))]
        fn fullscreen(&self, _note: &NSNotification) {
            self.changed();
        }
        #[unsafe(method(windowDidExitFullScreen:))]
        fn windowed(&self, _note: &NSNotification) {
            self.changed();
        }
    }
);
impl Delegate {
    fn new(mtm: MainThreadMarker, id: Option<String>) -> Retained<Self> {
        let object = Self::alloc(mtm).set_ivars(DelegateState { id });
        unsafe { msg_send![super(object), init] }
    }
    fn changed(&self) {
        if let Some(id) = &self.ivars().id {
            send(Main::State(id.clone()));
        }
    }
}

struct Window {
    window: Retained<NSWindow>,
    view: Rc<AppView>,
    _delegate: Retained<Delegate>,
    runtime: RuntimeWindow,
    normal: NSRect,
    last_state: Value,
    focus_serial: u64,
    dock_info: Value,
    closing: bool,
}
struct Host {
    #[cfg(feature = "terminal-fixture")]
    fixture: Option<crate::nsh_terminal_fixture::Fixture>,
    mtm: MainThreadMarker,
    app: Retained<NSApplication>,
    home: PathBuf,
    bundle: Bundle,
    asset_base: Option<AssetBase>,
    chunks: Option<PathBuf>,
    components: Components,
    menus: HostMenus,
    windows: BTreeMap<String, Window>,
    service: mpsc::Sender<Service>,
    focus_serial: u64,
    quitting: bool,
    quit_pending: bool,
    quit_request_pending: bool,
    background: bool,
    forced_appearance: Option<Appearance>,
    /// `None` when this process must not post (`notices::Delivery::Off`).
    notifier: Option<Arc<dyn Notifier>>,
    /// The production poster; only in an app-bundle process.
    notification_center: Option<notices::NotificationCenter>,
    posted: VecDeque<PostedNotice>,
}
impl Host {
    fn open(&mut self, prepared: PreparedWindow) -> Result<String, String> {
        let id = Uuid::new_v4().to_string();
        let channel = HostChannel::new();
        let appearance = self.forced_appearance.unwrap_or(Appearance::System);
        let mut params = prepared.params(&id, window_count(&self.windows));
        params["appearance"] = json!(appearance_name(appearance));
        channel.set_params(params.as_object().unwrap().clone());
        for &name in host_names::OPERATIONS {
            let id = id.clone();
            channel.handle(name, move |call| send(Main::Call(id.clone(), name, call)));
        }
        // All other v1 operations fail explicitly until their owning feature lands.
        for name in [
            "system.quickLook",
            "system.openPanel",
            "system.savePanel",
            "system.readPasteboard",
            "system.share",
            "system.requestPermission",
        ] {
            channel.handle(name, move |call| {
                call.reject(format!(
                    "unavailable: {name} is not implemented in the Phase 1b host"
                ))
            });
        }
        channel.handle_open_url(|request| {
            if allowed_url(request.url()) {
                request.decline();
            } else {
                request.handled();
            }
        });
        let frame = opening_frame(
            self.mtm,
            prepared.runtime.bounds,
            prepared.opening_size,
            window_count(&self.windows),
        );
        let view = AppView::new(
            self.mtm,
            AppOptions {
                app_id: APP_ID.into(),
                title: "RiWork".into(),
                size: (frame.size.width as i32, frame.size.height as i32),
                appearance,
                data_dir: Some(self.home.join("nsh")),
                cache: CacheOptions::at(CacheLocation::Dir(self.home.join("nsh/cache"))),
                storage: Some(Arc::new(TransientStorage)),
                asset_base: self.asset_base.clone(),
                chunks: self.chunks.clone(),
                host: channel,
                components: self.components.clone(),
                ..Default::default()
            },
        );
        let window = create_window(self.mtm, frame);
        window.setTitle(&NSString::from_str(window_title(&prepared.runtime)));
        window.setContentView(Some(&view.view()));
        position_controls(&window);
        let delegate = Delegate::new(self.mtm, Some(id.clone()));
        window.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        let activate = prepared.options.activate.unwrap_or(true) && !self.background;
        if activate {
            window.makeKeyAndOrderFront(None);
        } else {
            window.orderFront(None);
        }
        match prepared.runtime.mode {
            WindowMode::Maximized => window.zoom(None),
            WindowMode::Fullscreen => window.toggleFullScreen(None),
            WindowMode::Windowed => {}
        }
        view.load(App::Js(self.bundle.clone()));
        self.windows.insert(
            id.clone(),
            Window {
                window,
                view,
                _delegate: delegate,
                runtime: prepared.runtime,
                normal: frame,
                last_state: Value::Null,
                focus_serial: 0,
                dock_info: Value::Null,
                closing: false,
            },
        );
        self.state(&id);
        self.publish_global_selection();
        self.publish();
        Ok(id)
    }

    fn publish_global_selection(&self) {
        let view = self
            .windows
            .values()
            .find(|window| window.runtime.is_global())
            .and_then(|window| window.runtime.global.as_ref())
            .map(|state| state.view.name());
        // The global window owns its view once open (TSX saves it); only an
        // explicit open (`open_events`) may switch it. Re-sending the host's
        // open-time view here would undo a switch made inside the window.
        for window in self.windows.values().filter(|w| !w.runtime.is_global()) {
            let _ = window
                .view
                .host()
                .send("global.show", json!({"view": view, "start": false}));
        }
    }
    fn snapshot(&self) -> Vec<(String, RuntimeWindow)> {
        self.windows
            .iter()
            .map(|(id, window)| {
                let screen = window
                    .window
                    .screen()
                    .or_else(|| NSScreen::mainScreen(self.mtm))
                    .map(|screen| screen.frame())
                    .unwrap_or(window.normal);
                let mut runtime =
                    recorded_runtime(&window.runtime, window.normal, screen, mode(&window.window));
                apply_current_layout(id, &mut runtime);
                (id.clone(), runtime)
            })
            .collect()
    }
    fn facts(&self) -> Vec<WindowFacts> {
        self.windows
            .iter()
            .map(|(id, w)| WindowFacts {
                id: id.clone(),
                kind: w.runtime.kind,
                project_id: current_project_id(id, &w.runtime),
                focus_serial: w.focus_serial,
                minimized: w.window.isMiniaturized(),
                on_active_space: w.window.isOnActiveSpace(),
                closing: w.closing,
            })
            .collect()
    }
    fn publish(&self) {
        let _ = self.service.send(Service::Snapshot(self.snapshot()));
    }
    fn state(&mut self, id: &str) {
        if id.is_empty() {
            for id in self.windows.keys().cloned().collect::<Vec<_>>() {
                self.state(&id);
            }
            return;
        }
        if let Some(w) = self.windows.get_mut(id) {
            position_controls(&w.window);
            w.normal = normal_frame(
                w.normal,
                w.window.frame(),
                mode(&w.window),
                w.window.isMiniaturized(),
            );
            let state = json!({"key": w.window.isKeyWindow(), "visible": w.window.isVisible(),
                "occluded": !w.window.occlusionState().contains(NSWindowOcclusionState::Visible),
                "mode": mode(&w.window), "backingScale": w.window.backingScaleFactor(),
                "frame": geometry(w.window.frame()), "appActive": self.app.isActive()});
            if changed_state(&w.last_state, &state) {
                if w.window.isKeyWindow() && w.last_state["key"] != true {
                    self.focus_serial += 1;
                    w.focus_serial = self.focus_serial;
                }
                let _ = w.view.host().send("window.state", state.clone());
                w.last_state = state;
                self.publish();
            }
        }
    }
    fn focus(&self, id: &str) -> Result<(), String> {
        let w = self.windows.get(id).ok_or("not_found: window")?;
        if w.window.isMiniaturized() {
            w.window.deminiaturize(None);
        }
        if self.background {
            w.window.orderFront(None);
        } else {
            w.window.makeKeyAndOrderFront(None);
            #[allow(deprecated)]
            self.app.activateIgnoringOtherApps(true);
        }
        Ok(())
    }
    fn teardown(&mut self, id: &str) {
        if let Some(window) = self.windows.remove(id) {
            window.view.stop();
            window.window.setDelegate(None);
            window.window.close();
            self.publish_global_selection();
        }
    }
    fn close(&mut self, id: &str, reason: &str, call: Option<HostCall>) {
        let Some(window) = self.windows.get_mut(id) else {
            if let Some(call) = call {
                call.reject("not_found: window");
            }
            return;
        };
        // A quit preparation already paused forwarding. Do not introduce a
        // separate close barrier whose failure could abandon that quit gate.
        if self.quitting || self.quit_request_pending || window.closing {
            if let Some(call) = call {
                call.reject("unavailable: window is closing");
            }
            return;
        }
        window.closing = true;
        let _ = window
            .view
            .host()
            .send("window.willClose", json!({"reason":reason}));
        let batch = ClosingWindows::new(
            self.snapshot()
                .into_iter()
                .filter(|(window_id, _)| window_id == id)
                .collect(),
        );
        if let Err(error) = self
            .service
            .send(Service::Close(id.to_owned(), batch, call))
        {
            if let Service::Close(_, batch, call) = error.0 {
                self.windows.get_mut(id).unwrap().closing = false;
                batch.cancel();
                if let Some(call) = call {
                    call.reject("unavailable: host service stopped");
                }
            }
        }
    }
    fn quit(&mut self) {
        if self.quitting || self.quit_request_pending {
            return;
        }
        self.quit_request_pending = true;
        if self.service.send(Service::PrepareQuit).is_err() {
            self.quit_request_pending = false;
        }
    }
    fn finish_quit(&mut self) {
        self.quit_request_pending = false;
        if self.quitting {
            return;
        }
        if self.windows.values().any(|window| window.closing) {
            self.quit_pending = true;
            return;
        }
        self.quitting = true;
        self.quit_pending = false;
        for window in self.windows.values() {
            let _ = window
                .view
                .host()
                .send("window.willClose", json!({"reason":"quit"}));
        }
        let batch = ClosingWindows::new(self.snapshot());
        if let Err(error) = self.service.send(Service::Quit(batch)) {
            self.quitting = false;
            if let Service::Quit(batch) = error.0 {
                batch.cancel();
            }
            eprintln!("riwork: unavailable: host service stopped");
        }
    }
    fn restart(&mut self, request: ReloadRequest) {
        if self.quitting || self.windows.values().any(|w| w.closing) {
            // The service will retry the pending request after the close turn.
            let _ = self.service.send(Service::RestartSkipped);
            return;
        }
        self.quitting = true;
        for window in self.windows.values() {
            let _ = window
                .view
                .host()
                .send("window.willClose", json!({"reason":"restart"}));
        }
        let batch = ClosingWindows::new(self.snapshot());
        if let Err(error) = self.service.send(Service::Restart(request, batch)) {
            self.quitting = false;
            if let Service::Restart(_, batch) = error.0 {
                batch.cancel();
            }
        }
    }
    fn open_requested(&mut self, prepared: PreparedWindow, intent: OpenIntent) {
        if self.quitting {
            if let Some(call) = intent.call {
                call.reject("unavailable: host is quitting");
            }
            return;
        }
        let target = open_target(
            &prepared.runtime,
            intent.reuses_project_window(),
            &self.facts(),
        );
        let events = open_events(&prepared, &intent);
        let view = prepared
            .runtime
            .is_global()
            .then(|| global_view(&prepared.options).to_owned());
        let routed = intent.route.is_some();
        let (id, created) = match target {
            OpenTarget::Reuse(id) => {
                let _ = self.focus(&id);
                (id, false)
            }
            OpenTarget::Create => match self.open(prepared) {
                Ok(id) => (id, true),
                Err(error) => {
                    match intent.call {
                        Some(call) => call.reject(error),
                        None => eprintln!("riwork: {error}"),
                    }
                    return;
                }
            },
        };
        if let Some(view) = view {
            if let Some(state) = self
                .windows
                .get_mut(&id)
                .and_then(|window| window.runtime.global.as_mut())
            {
                state.view = GlobalView::from(view);
            }
            // Every project status menu follows the shown view;
            // only the global window receives the start action.
            self.publish_global_selection();
            self.publish();
        }
        if let Some(window) = self.windows.get(&id) {
            for (name, data) in events {
                let _ = window.view.host().send(name, data);
            }
        }
        if let Some(call) = intent.call {
            call.resolve(open_result(&id, created, routed));
        }
    }
    /// A click on a posted notification or one of its actions (any thread → main).
    fn notification_response(&mut self, tag: String, action: Option<String>) {
        if let Some(posted) = self.posted.iter().rev().find(|p| p.tag == tag).cloned() {
            let activate = notices::activation(&tag, action.as_deref(), posted.target);
            if let Some(project_id) = posted.project_id {
                let mut options = OpenOptions::project(None);
                options.project_id = Some(project_id);
                let intent = OpenIntent {
                    activate: Some(activate),
                    ..OpenIntent::default()
                };
                let _ = self.service.send(Service::Open(options, intent));
            } else if let Some(window) = self.windows.get(&posted.window) {
                let _ = window.view.host().send("notification.activate", activate);
                let _ = self.focus(&posted.window);
            }
            return;
        }
        if let Some((project_id, shell_id)) = notices::completion_response(&tag, action.as_deref())
        {
            let _ = self.service.send(Service::OpenAgent {
                tag,
                project_id,
                shell_id,
            });
        }
    }
    fn handle(&mut self, message: Main) {
        match message {
            Main::Call(id, name, call) => self.call(id, name, call),
            Main::Open(prepared, intent) => self.open_requested(prepared, intent),
            Main::Notify(notification) => {
                if let Some(center) = &self.notification_center {
                    center.post(&notification);
                }
            }
            Main::NotificationResponse(tag, action) => self.notification_response(tag, action),
            Main::State(id) => self.state(&id),
            Main::Close(id) => self.close(&id, "user", None),
            Main::Closed(id, call, result) => match result {
                Ok(()) => {
                    if let Some(call) = call {
                        call.resolve(json!({}));
                    }
                    self.teardown(&id);
                    self.publish();
                    if last_close_quits(window_count(&self.windows)) || self.quit_pending {
                        self.quit();
                    }
                }
                Err((error, batch)) => {
                    if let Some(window) = self.windows.get_mut(&id) {
                        window.closing = false;
                    }
                    self.quit_pending = false;
                    batch.cancel();
                    if let Some(call) = call {
                        call.reject(error);
                    } else {
                        eprintln!("riwork: close cancelled: {error}");
                    }
                }
            },
            Main::Recovery(recovery) => recovery.handle(&mut self.quitting, |id| {
                if let Some(window) = self.windows.get(id) {
                    let _ = window.view.host().send("window.closeCancelled", json!({}));
                }
            }),
            Main::Restart(request) => self.restart(request),
            Main::SettingsChanged(size) => {
                for window in self.windows.values() {
                    let _ = window
                        .view
                        .host()
                        .send("settings.changed", json!({"uiTextSize":size}));
                }
            }
            Main::Menu(action) => self.menu(action),
            Main::Quit => self.quit(),
            Main::QuitPrepared => self.finish_quit(),
            Main::Finish => {
                for id in self.windows.keys().cloned().collect::<Vec<_>>() {
                    self.teardown(&id);
                }
                // AppView::stop drops the components, but their native views
                // remain parked until exact-tty detach and child exit complete.
                // Keep AppKit/Ghostty wakeups alive through the final free.
                self.handle(Main::DrainTerminals);
            }
            Main::DrainTerminals => {
                if riwork_nsh_terminal::pending_count() == 0 {
                    stop(&self.app);
                } else {
                    thread::spawn(|| {
                        thread::sleep(Duration::from_millis(20));
                        send(Main::DrainTerminals);
                    });
                }
            }
            Main::Error(error) => {
                eprintln!("riwork: {error}");
                self.quit();
            }
            #[cfg(feature = "terminal-fixture")]
            Main::Fixture(request) => self.fixture_call(request),
        }
    }
    #[cfg(feature = "terminal-fixture")]
    fn fixture_call(&mut self, request: Value) {
        if self.fixture.is_none() {
            return;
        }
        if request["op"] == "heartbeat" {
            riwork_nsh_terminal::fixture_heartbeat();
            return;
        }
        let seq = request["seq"].as_u64().unwrap_or(0);
        let id = request["windowId"].as_str().unwrap_or_default();
        let result = match request["op"].as_str() {
            Some("snapshot") => json!({"ok":true,"windows":self.windows.iter().map(|(id,w)|json!({
                "windowId":id,"projectId":w.runtime.project_id,"windowNumber":w.window.windowNumber(),
                "layout":host_layouts::lookup(id).map(|l|l.project_layout())
            })).collect::<Vec<_>>(),"terminalStates":if request["includeTrace"].as_bool().unwrap_or(false) {riwork_nsh_terminal::fixture_snapshot()} else {riwork_nsh_terminal::fixture_status()}}),
            Some("route" | "command") => {
                if let Some(window) = self.windows.get(id) {
                    let (name, data) = if request["op"] == "route" {
                        ("route", json!({"step":request["step"]}))
                    } else {
                        (
                            "command",
                            json!({"id":request["id"],"args":request["args"]}),
                        )
                    };
                    let _ = window.view.host().send(name, data);
                    json!({"ok":true})
                } else {
                    json!({"ok":false,"error":"window not found"})
                }
            }
            Some("closeWindow") => {
                self.close(id, "user", None);
                json!({"ok":true})
            }
            Some("quit") => {
                self.quit();
                json!({"ok":true})
            }
            Some("show") => {
                // Only this marker-guarded fixture transport may arrange its
                // own windows. Bring both into view without changing the key
                // app/window, so real occlusion notifications drive Ghostty.
                let available = visible_frame(self.mtm, None);
                let count = self.windows.len().max(1) as f64;
                let gap = 16.;
                let width = ((available.size.width - gap * (count + 1.)) / count).min(1220.);
                let height = (available.size.height - 2. * gap).min(900.);
                if width < 640. || height < 400. {
                    json!({"ok":false,"error":"screen too small for isolated side-by-side fixture windows"})
                } else {
                    let start = available.origin.x
                        + (available.size.width - count * width - (count - 1.) * gap) / 2.;
                    let frames = self
                        .windows
                        .iter()
                        .enumerate()
                        .map(|(index, (id, window))| {
                            let frame = NSRect::new(
                                NSPoint::new(
                                    start + index as f64 * (width + gap),
                                    available.origin.y + available.size.height - height - gap,
                                ),
                                NSSize::new(width, height),
                            );
                            window.window.setFrame_display(frame, true);
                            window.window.orderFrontRegardless();
                            json!({"windowId":id,"windowNumber":window.window.windowNumber(),
                            "x":frame.origin.x,"y":frame.origin.y,"width":width,"height":height})
                        })
                        .collect::<Vec<_>>();
                    json!({"ok":true,"frames":frames,"activation":false})
                }
            }
            Some("resize") => {
                if let Some(window) = self.windows.get(id) {
                    let mut frame = window.window.frame();
                    frame.size.width = request["width"]
                        .as_f64()
                        .unwrap_or(1000.)
                        .clamp(640., 2000.);
                    frame.size.height = request["height"]
                        .as_f64()
                        .unwrap_or(700.)
                        .clamp(400., 1400.);
                    window.window.setFrame_display(frame, true);
                    json!({"ok":true})
                } else {
                    json!({"ok":false})
                }
            }
            Some("terminalProbe") => match riwork_nsh_terminal::fixture_probe(
                request["shellId"].as_str().unwrap_or_default(),
                request["kind"].as_str().unwrap_or_default(),
            ) {
                Ok(()) => json!({"ok":true}),
                Err(e) => json!({"ok":false,"error":e}),
            },
            Some("terminalInput") => match riwork_nsh_terminal::fixture_input(
                request["shellId"].as_str().unwrap_or_default(),
                request["text"].as_str().unwrap_or_default(),
            ) {
                Ok(()) => json!({"ok":true}),
                Err(e) => json!({"ok":false,"error":e}),
            },
            Some("terminalCommand") => match riwork_nsh_terminal::fixture_command(
                request["shellId"].as_str().unwrap_or_default(),
                request["name"].as_str().unwrap_or_default(),
                request["args"].clone(),
            ) {
                Ok(()) => json!({"ok":true}),
                Err(e) => json!({"ok":false,"error":e}),
            },
            Some("terminalPolicy") => match riwork_nsh_terminal::fixture_policy(
                request["shellId"].as_str().unwrap_or_default(),
                &request["args"],
            ) {
                Ok(value) => json!({"ok":true,"policy":value}),
                Err(e) => json!({"ok":false,"error":e}),
            },
            _ => json!({"ok":false,"error":"unknown fixture operation"}),
        };
        self.fixture.as_ref().unwrap().reply(seq, result);
    }
    fn dock_menu(&self) -> Retained<NSMenu> {
        let windows = self
            .windows
            .iter()
            .map(|(id, w)| DockWindow {
                id: id.clone(),
                window_number: w.window.windowNumber(),
                project: w.dock_info["project"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| w.window.title().to_string()),
                branch: w.dock_info["branch"].as_str().map(str::to_owned),
            })
            .collect::<Vec<_>>();
        let frontmost = self
            .windows
            .values()
            .filter(|w| w.focus_serial > 0)
            .max_by_key(|w| w.focus_serial)
            .map(|w| w.window.windowNumber());
        self.menus.dock_menu(&windows, frontmost)
    }
    fn menu(&mut self, action: MenuAction) {
        match action.command {
            "biggerText" | "smallerText" | "actualSize" => {
                let _ = self.service.send(Service::TextSize(action.command));
            }
            "quit" => self.quit(),
            "focusWindow" => {
                let id = self
                    .windows
                    .iter()
                    .find(|(_, w)| Some(w.window.windowNumber()) == action.window_number)
                    .map(|(id, _)| id.clone());
                if let Some(id) = id {
                    let _ = self.focus(&id);
                }
            }
            "openGlobalDashboard" | "openGlobalChat" => {
                if let Some(options) = global_open_options(action.command) {
                    let _ = self
                        .service
                        .send(Service::Open(options, OpenIntent::default()));
                }
            }
            "newWindow" => match new_window_plan(&self.facts()) {
                NewWindow::Project(project_id) => {
                    let mut options = OpenOptions::project(None);
                    options.project_id = Some(project_id);
                    let _ = self
                        .service
                        .send(Service::Open(options, OpenIntent::default()));
                }
                NewWindow::AskGlobal(id) => {
                    if let Some(window) = self.windows.get(&id) {
                        let _ = window.view.host().send("command", new_window_command());
                    }
                }
                NewWindow::Default => {
                    let _ = self.service.send(Service::Open(
                        OpenOptions::project(None),
                        OpenIntent::default(),
                    ));
                }
            },
            command => {
                let number = crate::nsh_menus::command_window(
                    action.window_number,
                    self.windows
                        .values()
                        .find(|w| w.window.isKeyWindow())
                        .map(|w| w.window.windowNumber()),
                    &self
                        .windows
                        .values()
                        .map(|w| w.window.windowNumber())
                        .collect::<Vec<_>>(),
                );
                let selected = self
                    .windows
                    .values()
                    .find(|w| Some(w.window.windowNumber()) == number);
                if let Some(w) = selected {
                    let _ = w.view.host().send("command", json!({"id":command}));
                }
            }
        }
    }
    fn call(&mut self, id: String, name: &str, call: HostCall) {
        if let Err(error) = admit_host_call(
            self.windows.get(&id).map(|window| &window.runtime),
            self.quitting,
            &id,
            name,
        ) {
            call.reject(error);
            return;
        }
        let data = call.data().clone();
        match name {
            "window.open" => match serde_json::from_value::<OpenOptions>(data).map_err(|e| format!("invalid: {e}")).and_then(|o| { o.validate()?; Ok(o) }) {
                Ok(options) => { let _ = self.service.send(Service::Open(options, OpenIntent::call(call))); }
                Err(error) => call.reject(error),
            },
            "window.route" => {
                let project = data["projectId"].as_str();
                if project.is_none() || !valid_route(&data["step"]) { call.reject("invalid: projectId and a valid route step are required"); return; }
                let mut options = OpenOptions::project(None); options.project_id = project.map(str::to_owned);
                let intent = OpenIntent { call: Some(call), route: Some(data["step"].clone()), ..OpenIntent::default() };
                let _ = self.service.send(Service::Open(options, intent));
            }
            "window.list" => call.resolve(Value::Array(self.windows.iter().map(|(id,w)| json!({
                "windowId":id,"kind":w.runtime.kind,"projectId":current_project_id(id, &w.runtime),
                "title":w.window.title().to_string(),"key":w.window.isKeyWindow(),"mode":mode(&w.window),
                "focusSerial":w.focus_serial})).collect())),
            "window.placement" => {
                let w = &self.windows[&id];
                call.resolve(json!({"mode":mode(&w.window),"frame":geometry(w.window.frame()),
                    "normalFrame":geometry(w.normal),"visibleFrame":geometry(visible_frame(self.mtm, Some(&w.window)))}));
            }
            "window.focus" => match data["windowId"].as_str().ok_or("invalid: windowId").and_then(|id| self.focus(id).map_err(|_| "not_found: window")) {
                Ok(()) => call.resolve(json!({})), Err(error) => call.reject(error),
            },
            "window.close" => {
                let target = data.get("windowId").map(Value::as_str).unwrap_or(Some(&id));
                match target {
                    Some(target) if self.windows.contains_key(target) => { let target = target.to_owned(); self.close(&target,"user",Some(call)); }
                    _ => call.reject("not_found: window"),
                }
            }
            "window.setDockInfo" => { self.windows.get_mut(&id).unwrap().dock_info = data; call.resolve(json!({})); }
            "app.restoreReady" => {
                self.publish();
                let _ = self.service.send(Service::Ready(id));
                call.resolve(json!({}));
            }
            "app.quit" => { call.resolve(json!({})); self.quit(); }
            "app.restart" => { let _ = self.service.send(Service::QueueRestart(call)); }
            "app.setAppearance" => match data["appearance"].as_str().ok_or_else(|| "invalid: appearance is required".to_owned()).and_then(parse_appearance) {
                Ok(requested) => {
                    let appearance = self.forced_appearance.unwrap_or(requested);
                    pin_application(&self.app, appearance);
                    for window in self.windows.values() {
                        let _ = window.view.host().send("appearance", json!({"appearance": appearance_name(appearance)}));
                    }
                    call.resolve(json!({}));
                }
                Err(error) => call.reject(error),
            },
            "system.notify" => match (notices::Notification::from_call(&data), self.notifier.clone()) {
                (Err(error), _) => call.reject(error),
                (Ok(_), None) => call.reject("unavailable: system notifications need the RiWork app bundle"),
                (Ok(notification), Some(notifier)) => match notifier.post(&notification) {
                    Ok(()) => {
                        if self.posted.len() >= POSTED_NOTICES { self.posted.pop_front(); }
                        self.posted.push_back(PostedNotice { tag: notification.tag, window: id,
                            project_id: notification.project_id, target: notification.target });
                        call.resolve(json!({}));
                    }
                    Err(error) => call.reject(error),
                },
            },
            "system.fonts" => {
                let families = NSFontManager::sharedFontManager(self.mtm).availableFontFamilies();
                call.resolve(json!({"families": families.iter().map(|family| family.to_string()).collect::<Vec<_>>()}));
            },
            "system.openWith" | "system.revealInFinder" => {
                // The opt-in isolated fixture verifies the complete native ->
                // TSX -> host event path without opening any external app.
                #[cfg(feature = "terminal-fixture")]
                if self.fixture.is_some() { call.resolve(json!({"fixture":true})); return; }
                let Some(path) = data["path"].as_str().filter(|p| !p.is_empty() && !p.contains('\0')) else { call.reject("invalid: path"); return; };
                let path = path.to_owned();
                let app = data.get("app").and_then(Value::as_str).map(str::to_owned);
                let reveal = name == "system.revealInFinder";
                thread::spawn(move || {
                    let result = open_terminal_link(&path, app.as_deref(), reveal);
                    match result { Ok(()) => call.resolve(json!({})), Err(error) => call.reject(error) }
                });
            }
            _ => call.reject("invalid: unregistered host operation"),
        }
    }
}

fn open_terminal_link(path: &str, app: Option<&str>, reveal: bool) -> Result<(), String> {
    if !Path::new(path).is_absolute() && !riwork_engine::terminal_links::is_openable_url(path) {
        return Err("invalid: expected an absolute path or http/https/mailto URL".into());
    }
    if Path::new(path).is_absolute() {
        if let Some(reason) = riwork_engine::terminal_links::open_refusal(Path::new(path)) {
            return Err(format!("refused: {reason}"));
        }
    } else if reveal {
        return Err("invalid: Finder requires a local path".into());
    }
    let mut command = std::process::Command::new("/usr/bin/open");
    if reveal {
        command.arg("-R");
    }
    if let Some(app) = app {
        command.arg("-a").arg(app);
    }
    let status = command
        .arg("--")
        .arg(path)
        .status()
        .map_err(|e| format!("io: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("io: open failed ({status})"))
    }
}

fn allowed_url(url: &str) -> bool {
    url.split_once(':').is_some_and(|(scheme, _)| {
        matches!(
            scheme.to_ascii_lowercase().as_str(),
            "http" | "https" | "mailto" | "x-apple.systempreferences"
        )
    })
}
fn valid_route(step: &Value) -> bool {
    let kind = match step.get("kind").or_else(|| step.get("type")) {
        Some(kind) => kind.as_str(),
        // Contracts §2.3 write a panel step as `{panel}`; the kind is implied.
        None if step.get("panel").is_some() => Some("panel"),
        None => None,
    };
    match kind {
        Some("showShell") => step["shellId"]
            .as_str()
            .is_some_and(|id| Uuid::parse_str(id).is_ok()),
        Some("chat") => step["chatId"]
            .as_str()
            .is_some_and(|id| Uuid::parse_str(id).is_ok()),
        Some("task") => step["taskId"]
            .as_str()
            .is_some_and(|id| Uuid::parse_str(id).is_ok()),
        Some("panel") => step["panel"].as_str().is_some_and(|s| !s.is_empty()),
        Some("openEditor") => {
            step["path"]
                .as_str()
                .is_some_and(|s| Path::new(s).is_absolute())
                && step
                    .get("line")
                    .is_none_or(|n| n.as_u64().is_some_and(|n| n <= u32::MAX as u64))
        }
        Some("newSubshell") => step
            .get("parentShellId")
            .is_none_or(|id| id.as_str().is_some_and(|id| Uuid::parse_str(id).is_ok())),
        Some("apiKeySettings" | "projectMemo") => true,
        _ => false,
    }
}
fn geometry(frame: NSRect) -> WindowGeometry {
    WindowGeometry {
        x: frame.origin.x as f32,
        y: frame.origin.y as f32,
        width: frame.size.width as f32,
        height: frame.size.height as f32,
    }
}
// Preserve the existing GPUI runtime file convention: display-local, top-left
// origin. Host window.placement uses AppKit coordinates like nsh/desktop.
fn runtime_geometry(frame: NSRect, screen: NSRect) -> WindowGeometry {
    WindowGeometry {
        x: (frame.origin.x - screen.origin.x) as f32,
        y: (screen.origin.y + screen.size.height - frame.origin.y - frame.size.height) as f32,
        width: frame.size.width as f32,
        height: frame.size.height as f32,
    }
}
fn appkit_frame(bounds: WindowGeometry, screen: NSRect) -> NSRect {
    NSRect::new(
        NSPoint::new(
            screen.origin.x + bounds.x as f64,
            screen.origin.y + screen.size.height - (bounds.y + bounds.height) as f64,
        ),
        NSSize::new(bounds.width as f64, bounds.height as f64),
    )
}
fn mode(window: &NSWindow) -> WindowMode {
    window_mode(
        window.styleMask().contains(NSWindowStyleMask::FullScreen),
        window.isZoomed(),
    )
}
fn window_mode(fullscreen: bool, zoomed: bool) -> WindowMode {
    if fullscreen {
        WindowMode::Fullscreen
    } else if zoomed {
        WindowMode::Maximized
    } else {
        WindowMode::Windowed
    }
}
fn visible_frame(mtm: MainThreadMarker, window: Option<&NSWindow>) -> NSRect {
    window
        .and_then(NSWindow::screen)
        .or_else(|| NSScreen::mainScreen(mtm))
        .map(|s| s.visibleFrame())
        .unwrap_or(NSRect::new(NSPoint::new(0., 0.), NSSize::new(1280., 800.)))
}
fn opening_frame(
    mtm: MainThreadMarker,
    bounds: Option<WindowGeometry>,
    size: WindowSize,
    cascade: usize,
) -> NSRect {
    if let Some(b) = bounds {
        let screen = NSScreen::mainScreen(mtm)
            .map(|s| s.frame())
            .unwrap_or_else(|| visible_frame(mtm, None));
        return appkit_frame(b, screen);
    }
    let screen = visible_frame(mtm, None);
    let frame = WindowFrame::opening(
        size,
        WindowFrame {
            x: screen.origin.x as f32,
            y: 0.,
            width: screen.size.width as f32,
            height: screen.size.height as f32,
        },
        cascade,
    );
    NSRect::new(
        NSPoint::new(
            frame.x as f64,
            screen.origin.y + screen.size.height - (frame.y + frame.height) as f64,
        ),
        NSSize::new(frame.width as f64, frame.height as f64),
    )
}
fn normal_frame(previous: NSRect, frame: NSRect, mode: WindowMode, minimized: bool) -> NSRect {
    if mode == WindowMode::Windowed && !minimized {
        frame
    } else {
        previous
    }
}
fn changed_state(previous: &Value, next: &Value) -> bool {
    previous != next
}
// The registry only admits the frozen Project and Global kinds. NSH tooltip
// overlays are elements in an AppView and never entries in this registry.
fn window_count<T>(windows: &BTreeMap<String, T>) -> usize {
    windows.len()
}
fn last_close_quits(remaining: usize) -> bool {
    remaining == 0
}

/// What the registry records for one window at a publish, close, quit or
/// reload: its launch RuntimeWindow with the current frame and mode, and for
/// the global window its captured size. Layouts are applied by the caller.
fn recorded_runtime(
    runtime: &RuntimeWindow,
    normal: NSRect,
    screen: NSRect,
    mode: WindowMode,
) -> RuntimeWindow {
    let mut runtime = runtime.clone();
    runtime.bounds = Some(runtime_geometry(normal, screen));
    runtime.mode = mode;
    if let Some(global) = &mut runtime.global {
        global.window_size = WindowSize::new(normal.size.width as f32, normal.size.height as f32);
    }
    runtime
}

/// A window a reload hands over. The global window reopens as itself and
/// shows the handed-over view.
fn restored_window(home: &Path, runtime: &RuntimeWindow) -> PreparedWindow {
    let options = if runtime.is_global() {
        let mut options = OpenOptions::global();
        options.view = runtime
            .global
            .as_ref()
            .map(|state| state.view.name().to_owned());
        options
    } else {
        OpenOptions::project(Some(runtime.path.clone()))
    };
    PreparedWindow {
        options,
        opening_size: preferred_opening_size(home, runtime),
        runtime: runtime.clone(),
        restore: true,
        notice: None,
    }
}

/// TSX owns the global window's view, `show_quiet`, filter and sort and saves
/// them itself (`Dashboard.saveGlobalWindow`); the host owns only the size it
/// captured. Every host-side write or publish merges onto the file, so a view
/// switched inside the window is never overwritten by the one from open time.
/// Service thread only (disk IO).
fn merged_global_state(home: &Path, host: &GlobalWindowState) -> GlobalWindowState {
    let mut state = GlobalWindowStore::at(home).load();
    // An impossible size was already dropped; keep the last ordinary one.
    state.window_size = host.window_size.or(state.window_size);
    state
}

/// Called on the startup/service worker, never while constructing an AppView.
fn preferred_opening_size(home: &Path, runtime: &RuntimeWindow) -> WindowSize {
    if runtime.is_global() {
        return runtime
            .global
            .as_ref()
            .and_then(|state| state.window_size)
            .unwrap_or(WindowSize {
                width: 1100.,
                height: 760.,
            });
    }
    let remember = SettingsStore::open(home)
        .and_then(|store| store.load())
        .map(|settings| settings.remember_window_size)
        .unwrap_or(true);
    if remember {
        if let Some(size) = runtime
            .project_id
            .as_deref()
            .and_then(|id| LayoutStore::open(home).ok()?.window_size(id))
        {
            return WindowSize {
                width: size.width.max(640.),
                height: size.height.max(400.),
            };
        }
    }
    WindowSize {
        width: 1220.,
        height: 780.,
    }
}

fn remember_window_frames(home: &Path, windows: &[(String, RuntimeWindow)]) {
    let Ok(settings) = SettingsStore::open(home).and_then(|store| store.load()) else {
        return;
    };
    for (id, window) in windows {
        if let (Some(layout), Some(frame)) = (host_layouts::lookup(id), window.bounds) {
            // A close gate intentionally rejects late geometry. The flush owns
            // the last accepted edit and will retain the window if it fails.
            let _ = layout.observe_window_frame(
                frame.width,
                frame.height,
                window.mode == WindowMode::Windowed,
                settings.remember_window_size,
            );
        }
    }
}

fn stop(app: &NSApplication) {
    app.stop(None);
    if let Some(event) = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
        NSEventType::ApplicationDefined, NSPoint::new(0.,0.), NSEventModifierFlags::empty(),0.,0,None,0,0,0) {
        app.postEvent_atStart(&event,true);
    }
}

fn prepare(home: &Path, options: OpenOptions) -> Result<PreparedWindow, String> {
    options.validate()?;
    let runtime = if options.kind == WindowKind::Global {
        RuntimeWindow::global(
            home,
            None,
            WindowMode::Windowed,
            GlobalWindowStore::at(home).load(),
        )
    } else {
        let store = Store::open(home.to_path_buf())?;
        let state = store.snapshot()?;
        let project = if let Some(id) = &options.project_id {
            Uuid::parse_str(id)
                .map_err(|_| "invalid: projectId must be a UUID (remote projects await P1a)")?;
            state.project(id)?.clone()
        } else if let Some(path) = &options.path {
            let root = path
                .canonicalize()
                .map_err(|e| format!("Cannot open project folder {}: {e}", path.display()))?;
            match state.projects.iter().find(|p| p.root == root) {
                Some(project) => project.clone(),
                None => store.add_project(root, None)?,
            }
        } else if let Some(project) = state.active_project().filter(|p| p.root.is_dir()) {
            project.clone()
        } else {
            let root = fallback_directory(
                std::env::current_dir().ok(),
                std::env::var_os("HOME").map(PathBuf::from),
            );
            match state.projects.iter().find(|p| p.root == root) {
                Some(project) => project.clone(),
                None => store.add_project(root, None)?,
            }
        };
        if !project.root.is_dir() {
            return Err(format!(
                "not_found: project folder {}",
                project.root.display()
            ));
        }
        RuntimeWindow {
            project_id: Some(project.id),
            path: project.root,
            bounds: None,
            mode: WindowMode::Windowed,
            layout: None,
            focus_mode: false,
            focus_centered: false,
            kind: WindowKind::Project,
            global: None,
        }
    };
    Ok(PreparedWindow {
        opening_size: preferred_opening_size(home, &runtime),
        options,
        runtime,
        restore: false,
        notice: None,
    })
}

fn fallback_directory(cwd: Option<PathBuf>, home: Option<PathBuf>) -> PathBuf {
    cwd.into_iter()
        .chain(home)
        .chain([PathBuf::from(".")])
        .find(|p| p.is_dir())
        .unwrap_or_else(|| PathBuf::from("."))
}

struct Startup {
    home: PathBuf,
    bundle: Bundle,
    directory: Option<PathBuf>,
    windows: Vec<PreparedWindow>,
    instance: DesktopInstance,
    runtime: HostRuntime,
}
fn startup(args: Vec<String>) -> Result<Option<Startup>, String> {
    let home = paths::riwork_home()?;
    paths::create_private_dir(&home).map_err(|e| e.to_string())?;
    let home = home.canonicalize().map_err(|e| e.to_string())?;
    let restore = HostRuntime::read_restore(&home)?;
    let path = args
        .into_iter()
        .find(|arg| !arg.starts_with("-psn_"))
        .map(PathBuf::from);
    let instance = match DesktopInstance::acquire_or_forward(
        &home,
        &OpenRequest { path: path.clone() },
        restore.as_ref(),
    )? {
        InstanceStart::Forwarded => return Ok(None),
        InstanceStart::Primary(instance) | InstanceStart::Replacement(instance) => instance,
    };
    paths::ensure_default_projects_directory()?;
    // Initialization and bundle IO stay on this startup worker, before any AppView.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("Cannot initialize native runtime: {error}"))?;
    runtime.block_on(native::initialize())?;
    // This executable is RiWork: `chat ensure` re-enters `run_cli` in nsh_main.
    // Test runners never call this, so they can never start a chat host.
    native::chat::allow_host_start();
    let (bundle, directory) = crate::nsh_bundle::load()?;
    let bundle = bundle
        .with_app_id(Some(APP_ID))
        .with_capabilities_json(Some(CAPABILITIES))?;
    let windows = if let Some(snapshot) = &restore {
        snapshot
            .windows
            .iter()
            .map(|runtime| restored_window(&home, runtime))
            .collect()
    } else {
        let first = match prepare(&home, OpenOptions::project(path.clone())) {
            Ok(window) => window,
            Err(error) if path.is_some() => {
                let mut window = prepare(&home, OpenOptions::project(None))?;
                window.notice = Some(error);
                window
            }
            Err(error) => return Err(error),
        };
        vec![first]
    };
    if windows.is_empty() {
        return Err("Cannot restore an empty window snapshot".into());
    }
    let runtime = HostRuntime::register(&home, restore)?;
    Ok(Some(Startup {
        home,
        bundle,
        directory,
        windows,
        instance,
        runtime,
    }))
}
/// The registered layout follows project switches without an AppKit event or
/// a second UI-to-host protocol. Loading windows retain their launch identity.
fn current_project_id(id: &str, window: &RuntimeWindow) -> Option<String> {
    if window.is_global() {
        return None;
    }
    host_layouts::lookup(id)
        .map(|layout| layout.project_id())
        .or_else(|| window.project_id.clone())
}

fn apply_current_layout(id: &str, window: &mut RuntimeWindow) {
    if !window.is_global()
        && let Some(layout) = host_layouts::lookup(id)
    {
        let (project_id, project) = layout.project_snapshot();
        window.project_id = Some(project_id);
        window.layout = Some(project);
        (window.focus_mode, window.focus_centered) = layout.focus_state();
    }
}

/// Refresh identity and layout at every publish, including heartbeats without
/// a geometry event. This capture runs on the service thread.
fn current_layouts(home: &Path, windows: &[(String, RuntimeWindow)]) -> Vec<RuntimeWindow> {
    let mut windows: Vec<_> = windows
        .iter()
        .map(|(id, window)| {
            let mut window = window.clone();
            apply_current_layout(id, &mut window);
            if let Some(global) = &mut window.global {
                *global = merged_global_state(home, global);
            }
            window
        })
        .collect();
    refresh_project_paths(home, &mut windows);
    windows
}

/// Runtime snapshots also need the switched project's root for restoration.
/// This helper is called only by the service worker, never AppKit/engine code.
fn refresh_project_paths(home: &Path, windows: &mut [RuntimeWindow]) {
    if let Ok(store) = Store::open(home.to_path_buf())
        && let Ok(state) = store.snapshot()
    {
        for window in windows.iter_mut().filter(|window| !window.is_global()) {
            if let Some(project) = state
                .projects
                .iter()
                .find(|project| Some(project.id.as_str()) == window.project_id.as_deref())
            {
                window.path = project.root.clone();
            }
        }
    }
}

/// Retain the in-process handles across the main/service-thread handoff. The
/// close gate orders the last accepted edit before the blocking flush command.
struct ClosingWindows {
    windows: Vec<(String, RuntimeWindow)>,
    layouts: Vec<(String, Arc<HostLayout>)>,
}
impl ClosingWindows {
    fn new(windows: Vec<(String, RuntimeWindow)>) -> Self {
        let layouts = windows
            .iter()
            .filter_map(|(id, _)| {
                let layout = host_layouts::begin_window_close(id)?;
                Some((id.clone(), layout))
            })
            .collect();
        Self { windows, layouts }
    }

    /// Called only by the service worker. Attempt every barrier even when one
    /// fails; a failure keeps all corresponding AppViews alive and editable.
    fn flush(&self, home: &Path) -> Result<Vec<RuntimeWindow>, String> {
        let mut errors = Vec::new();
        if let Err(error) = native::host_flush::flush_blocking() {
            errors.push(format!("drafts: {error}"));
        }
        for (id, layout) in &self.layouts {
            if let Err(error) = layout.flush_blocking() {
                errors.push(format!("window {id}: {error}"));
            }
        }
        let mut windows = self.windows.clone();
        for (id, window) in &mut windows {
            if let Some((_, layout)) = self.layouts.iter().find(|(candidate, _)| candidate == id) {
                let (project_id, project) = layout.project_snapshot();
                window.project_id = Some(project_id);
                window.layout = Some(project);
                (window.focus_mode, window.focus_centered) = layout.focus_state();
            }
            if let Some(global) = &mut window.global {
                let merged = merged_global_state(home, global);
                if let Err(error) = GlobalWindowStore::at(home).save(&merged) {
                    errors.push(format!("global window {id}: {error}"));
                }
                *global = merged;
            }
        }
        let mut windows: Vec<_> = windows.into_iter().map(|(_, window)| window).collect();
        refresh_project_paths(home, &mut windows);
        if errors.is_empty() {
            Ok(windows)
        } else {
            Err(errors.join("; "))
        }
    }

    fn cancel(&self) {
        self.cancel_with(|id| send(Main::Recovery(Recovery::CloseCancelled(id.to_owned()))));
    }

    // Reopen every native gate before notifying the surviving engines. The
    // pending open may have rejected even when this batch retained no layout.
    fn cancel_with(&self, mut notify: impl FnMut(&str)) {
        for (id, _) in &self.windows {
            host_layouts::cancel_window_close(id);
        }
        for (id, _) in &self.windows {
            notify(id);
        }
    }
}

#[cfg(test)]
#[path = "nsh_pending_open_tests.rs"]
mod pending_open_tests;

struct RuntimeTick {
    due_at: Instant,
}

impl RuntimeTick {
    fn new(now: Instant) -> Self {
        Self { due_at: now }
    }
    fn is_due(&self, now: Instant) -> bool {
        now >= self.due_at
    }
    fn published(&mut self, now: Instant, completed: bool) {
        if completed {
            self.due_at = now + Duration::from_secs(2);
        }
    }
}

fn services(mut startup: Startup, rx: mpsc::Receiver<Service>, tx: mpsc::Sender<Service>) {
    startup.instance.set_request_waker(move || {
        let _ = tx.send(Service::Wake);
    });
    let mut snapshots: Vec<(String, RuntimeWindow)> = Vec::new();
    let mut initial = true;
    let mut confirmed = false;
    let mut runtime_tick = RuntimeTick::new(Instant::now());
    let mut reload: Option<(ReloadLaunch, ClosingWindows)> = None;
    let mut restart_queued = false;
    loop {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(Service::Snapshot(windows)) => {
                if initial {
                    initial = false;
                    if let Err(e) = startup
                        .runtime
                        .set_restore_window_ids(windows.iter().map(|(id, _)| id.clone()))
                    {
                        send(Main::Error(e));
                    }
                }
                snapshots = windows;
            }
            Ok(Service::Ready(id)) => {
                match startup.runtime.mark_restore_ready(
                    &id,
                    current_layouts(&startup.home, &startup.runtime.restore_windows(&snapshots)),
                ) {
                    Ok(done) => confirmed |= done,
                    Err(e) => send(Main::Error(e)),
                }
            }
            Ok(Service::Open(options, intent)) => match prepare(&startup.home, options) {
                Ok(window) => send(Main::Open(window, intent)),
                Err(error) => {
                    if let Some(call) = intent.call {
                        call.reject(error);
                    } else {
                        eprintln!("riwork: {error}");
                    }
                }
            },
            Ok(Service::OpenAgent {
                tag,
                project_id,
                shell_id,
            }) => {
                // SHELL-271: show the shell in a window of its project, opening
                // one by id if none shows it; the active project is unchanged.
                let result =
                    notices::verify_agent(&startup.home, &project_id, &shell_id).and_then(|()| {
                        let mut options = OpenOptions::project(None);
                        options.project_id = Some(project_id.clone());
                        prepare(&startup.home, options)
                    });
                match result {
                    Ok(window) => {
                        let target = json!({"projectId": project_id, "shellId": shell_id});
                        let intent = OpenIntent {
                            route: Some(json!({"kind": "showShell", "shellId": shell_id})),
                            activate: Some(notices::activation(&tag, None, Some(target))),
                            ..OpenIntent::default()
                        };
                        send(Main::Open(window, intent));
                    }
                    Err(error) => eprintln!("riwork notifications: {error}"),
                }
            }
            Ok(Service::Close(id, batch, call)) => {
                let result = batch
                    .flush(&startup.home)
                    .map(|_| ())
                    .map_err(|error| (error, batch));
                // Failed single-close recovery waits for Main::Closed to clear
                // the window's closing state before reopening native gates.
                send(Main::Closed(id, call, result));
            }
            Ok(Service::Quit(batch)) => {
                let result = batch
                    .flush(&startup.home)
                    .and_then(|windows| startup.runtime.publish(windows));
                if let Err(error) = result {
                    complete_failed_quit(
                        batch,
                        error,
                        || {
                            if let Err(error) = startup.instance.resume_forwarding() {
                                eprintln!("riwork: {error}");
                            }
                        },
                        send,
                    );
                    continue;
                }
                startup.instance.stop_forwarding();
                send(Main::Finish);
                // Retain the ownership lock until AppKit has stopped every view.
                while let Ok(message) = rx.recv() {
                    if matches!(message, Service::Finished) {
                        break;
                    }
                }
                break;
            }
            Ok(Service::PrepareQuit) => {
                startup.instance.stop_forwarding();
                for open in startup.instance.drain_requests() {
                    match prepare(&startup.home, OpenOptions::project(open.path)) {
                        Ok(window) => send(Main::Open(window, OpenIntent::forwarded())),
                        Err(error) => eprintln!("riwork: forwarded open failed: {error}"),
                    }
                }
                send(Main::QuitPrepared);
            }
            Ok(Service::QueueRestart(call)) => match startup.runtime.queue_restart() {
                Ok(()) => call.resolve(json!({})),
                Err(error) => call.reject(error),
            },
            Ok(Service::Restart(request, batch)) => {
                restart_queued = false;
                match batch
                    .flush(&startup.home)
                    .and_then(|windows| startup.runtime.launch_reload(&request, windows))
                {
                    Ok(launch) => reload = Some((launch, batch)),
                    Err(error) => {
                        complete_failed_quit(
                            batch,
                            format!("restart: {error}"),
                            || {
                                let _ = startup.runtime.fail_reload(&request, &error);
                                if let Err(error) = startup.instance.resume_forwarding() {
                                    eprintln!("riwork: {error}");
                                }
                            },
                            send,
                        );
                    }
                }
            }
            Ok(Service::RestartSkipped) => {
                restart_queued = false;
                if let Err(error) = startup.instance.resume_forwarding() {
                    eprintln!("riwork: {error}");
                }
            }
            Ok(Service::TextSize(command)) => match change_text_size(&startup.home, command) {
                Ok(size) => send(Main::SettingsChanged(size)),
                Err(error) => eprintln!("riwork: {error}"),
            },
            Ok(Service::Finished) => break,
            Ok(Service::Wake) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if reload.is_none() {
            remember_window_frames(&startup.home, &snapshots);
        }
        if reload.is_none() {
            for request in startup.instance.drain_requests() {
                match prepare(&startup.home, OpenOptions::project(request.path)) {
                    Ok(window) => send(Main::Open(window, OpenIntent::forwarded())),
                    Err(e) => eprintln!("riwork: forwarded open failed: {e}"),
                }
            }
        }
        if let Err(error) = startup.runtime.check_restore_deadline() {
            // Do not confirm late. The engine watchdog is the final backstop;
            // normal host progress closes this failed replacement immediately.
            eprintln!("riwork: {error}");
            send(Main::Finish);
            while let Ok(message) = rx.recv() {
                if matches!(message, Service::Finished) {
                    break;
                }
            }
            break;
        }
        if let Some((launch, _)) = &reload {
            match startup.runtime.reload_ready(launch) {
                Ok(true) => {
                    startup.instance.stop_forwarding();
                    send(Main::Finish);
                    while let Ok(message) = rx.recv() {
                        if matches!(message, Service::Finished) {
                            break;
                        }
                    }
                    break;
                }
                Err(error) => {
                    if let Some((_, batch)) = reload.take() {
                        complete_failed_quit(
                            batch,
                            format!("restart: {error}"),
                            || {
                                if let Err(error) = startup.instance.resume_forwarding() {
                                    eprintln!("riwork: {error}");
                                }
                            },
                            send,
                        );
                    }
                }
                Ok(false) => {}
            }
        }
        if confirmed && let Err(e) = startup.instance.try_complete_handoff() {
            send(Main::Error(e));
        }
        let now = Instant::now();
        if runtime_tick.is_due(now) {
            match startup
                .runtime
                .try_publish(current_layouts(&startup.home, &snapshots))
            {
                Ok(completed) => runtime_tick.published(now, completed),
                Err(e) => eprintln!("riwork: {e}"),
            }
            if reload.is_none()
                && !restart_queued
                && let Ok(Some(request)) = startup.runtime.pending_reload()
            {
                // Stop ACKing opens before capturing the restart snapshot.
                // Drain every already-acknowledged request ahead of Restart on
                // the same main queue. New forwarders retry the replacement.
                startup.instance.stop_forwarding();
                for open in startup.instance.drain_requests() {
                    match prepare(&startup.home, OpenOptions::project(open.path)) {
                        Ok(window) => send(Main::Open(window, OpenIntent::forwarded())),
                        Err(error) => eprintln!("riwork: forwarded open failed: {error}"),
                    }
                }
                restart_queued = true;
                send(Main::Restart(request));
            }
        }
    }
}

fn change_text_size(home: &Path, command: &str) -> Result<f32, String> {
    use riwork_core::{
        settings::SettingsStore,
        ui_text::{GHOSTTY_DEFAULT_FONT_SIZE, SizeChange},
    };
    let change = match command {
        "biggerText" => SizeChange::Bigger,
        "smallerText" => SizeChange::Smaller,
        "actualSize" => SizeChange::Reset,
        _ => return Err("invalid: text size command".into()),
    };
    let settings = SettingsStore::open(home)?
        .update(|settings| change.apply(settings, GHOSTTY_DEFAULT_FONT_SIZE))?;
    Ok(settings.ui_text_size.points())
}

/// Who posts notifications in this process (see `notices::Delivery`). The
/// production center is created at startup so a click reaches its delegate.
fn notification_delivery(
    home: &Path,
) -> (
    Option<Arc<dyn Notifier>>,
    Option<notices::NotificationCenter>,
) {
    let executable = std::env::current_exe().ok();
    let flag = std::env::var_os(notices::FIXTURE_ENV);
    match notices::delivery(home, flag.as_deref(), executable.as_deref()) {
        notices::Delivery::Fixture => (
            notices::FixtureNotifier::at(home).map(|n| Arc::new(n) as Arc<dyn Notifier>),
            None,
        ),
        notices::Delivery::System => {
            let center = notices::NotificationCenter::new(Arc::new(|tag, action| {
                send(Main::NotificationResponse(tag, action))
            }));
            let notifier = center.is_some().then(|| {
                Arc::new(notices::MainThreadNotifier::new(|notification| {
                    send(Main::Notify(notification))
                })) as Arc<dyn Notifier>
            });
            (notifier, center)
        }
        notices::Delivery::Off => (None, None),
    }
}

pub fn run(args: Vec<String>) -> Result<(), String> {
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .try_init();
    let Some(mut startup) = thread::spawn(move || startup(args))
        .join()
        .map_err(|_| "NSH startup worker panicked")??
    else {
        return Ok(());
    };
    let mtm = MainThreadMarker::new().ok_or("AppKit host must run on the main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    let forced_appearance = forced_appearance(std::env::var("RIWORK_APPEARANCE").ok().as_deref())?;
    if let Some(appearance) = forced_appearance {
        pin_application(&app, appearance);
    }
    let background =
        std::env::var_os("RIWORK_NSH_BACKGROUND").as_deref() == Some(std::ffi::OsStr::new("1"));
    app.setActivationPolicy(if background {
        NSApplicationActivationPolicy::Accessory
    } else {
        NSApplicationActivationPolicy::Regular
    });
    let delegate = Delegate::new(mtm, None);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    set_ui_sets(vec![nsh_base::registry(), native::registry()]);
    let menus = HostMenus::install(mtm, Arc::new(|action| send(Main::Menu(action))));
    let (tx, rx) = mpsc::channel();
    let components = Components::default();
    riwork_nsh_terminal::register(&components, startup.home.clone());
    let (notifier, notification_center) = notification_delivery(&startup.home);
    let mut host = Host {
        #[cfg(feature = "terminal-fixture")]
        fixture: if std::env::var_os("RIWORK_NSH_TERMINAL_FIXTURE").is_some() {
            Some(crate::nsh_terminal_fixture::start(
                startup.home.clone(),
                |request| send(Main::Fixture(request)),
            )?)
        } else {
            None
        },
        mtm,
        app: app.clone(),
        home: startup.home.clone(),
        bundle: startup.bundle.clone(),
        asset_base: startup.directory.clone().map(AssetBase::Directory),
        chunks: startup.directory.clone(),
        components,
        menus,
        windows: BTreeMap::new(),
        service: tx.clone(),
        focus_serial: 0,
        quitting: false,
        quit_pending: false,
        quit_request_pending: false,
        background,
        forced_appearance,
        notifier: notifier.clone(),
        notification_center,
        posted: VecDeque::new(),
    };
    // Queue the complete initial snapshot before the worker starts, not one partial set per open.
    let windows = std::mem::take(&mut startup.windows);
    for window in windows {
        host.open(window)?;
    }
    while rx.try_recv().is_ok() {}
    host.publish();
    HOST.with(|state| *state.borrow_mut() = Some(host));
    // Process-wide, once (not per window): notification claims and orchestrator
    // wakes. Their main-thread posts queue behind app.run().
    let _background = notices::start(startup.home.clone(), notifier);
    let worker_tx = tx.clone();
    let worker = thread::spawn(move || services(startup, rx, worker_tx));
    app.finishLaunching();
    if !background {
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
    }
    app.run();
    #[cfg(feature = "terminal-fixture")]
    HOST.with(|state| {
        if let Some(host) = state.borrow().as_ref() {
            if let Some(fixture) = &host.fixture {
                let _ = fixture
                    .finish(json!({"terminalStates":riwork_nsh_terminal::fixture_snapshot()}));
            }
        }
    });
    let _ = tx.send(Service::Finished);
    HOST.with(|state| {
        state.borrow_mut().take();
    });
    app.setDelegate(None);
    worker.join().map_err(|_| "NSH service worker panicked")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forced_appearance_from_env() {
        for (value, expected) in [
            ("light", Appearance::Light),
            ("dark", Appearance::Dark),
            ("system", Appearance::System),
        ] {
            assert_eq!(forced_appearance(Some(value)).unwrap(), Some(expected));
            assert_eq!(appearance_name(expected), value);
        }
        assert_eq!(forced_appearance(None).unwrap(), None);
        assert_eq!(forced_appearance(Some("")).unwrap(), None);
        assert!(
            forced_appearance(Some("sepia"))
                .unwrap_err()
                .starts_with("invalid:")
        );
    }

    #[test]
    fn text_size_commands_persist_without_any_window() {
        use riwork_core::settings::SettingsStore;
        let fixture = LayoutFixture::new();
        let store = SettingsStore::open(&fixture.root).unwrap();
        let initial = store.load().unwrap().ui_text_size.points();
        change_text_size(&fixture.root, "biggerText").unwrap();
        assert_eq!(store.load().unwrap().ui_text_size.points(), initial + 1.);
        change_text_size(&fixture.root, "smallerText").unwrap();
        assert_eq!(store.load().unwrap().ui_text_size.points(), initial);
        change_text_size(&fixture.root, "biggerText").unwrap();
        change_text_size(&fixture.root, "actualSize").unwrap();
        assert_eq!(store.load().unwrap().ui_text_size.points(), initial);
    }

    #[test]
    fn fallback_directory_uses_home_when_the_cwd_is_gone() {
        let fixture = LayoutFixture::new();
        assert_eq!(
            fallback_directory(
                Some(fixture.root.join("missing")),
                Some(fixture.root.clone())
            ),
            fixture.root
        );
        assert_eq!(fallback_directory(None, None), PathBuf::from("."));
    }

    #[test]
    fn a_busy_registry_lock_stays_due() {
        let start = Instant::now();
        let mut tick = RuntimeTick::new(start);
        assert!(tick.is_due(start));
        tick.published(start, false);
        assert!(tick.is_due(start + Duration::from_millis(500)));
        let retry = start + Duration::from_millis(500);
        tick.published(retry, true);
        assert!(!tick.is_due(retry + Duration::from_millis(1999)));
        assert!(tick.is_due(retry + Duration::from_secs(2)));
    }

    #[test]
    fn folder_arg_reuses_existing_project() {
        let fixture = LayoutFixture::new();
        let project_root = fixture.root.join("Alpha");
        std::fs::create_dir(&project_root).unwrap();
        let store = Store::open(fixture.root.clone()).unwrap();
        let project = store
            .add_project(project_root.clone(), Some("Alpha".into()))
            .unwrap();
        let prepared = prepare(
            &fixture.root,
            OpenOptions::project(Some(project_root.join("."))),
        )
        .unwrap();
        assert_eq!(
            prepared.runtime.project_id.as_deref(),
            Some(project.id.as_str())
        );
        assert_eq!(prepared.runtime.path, project_root.canonicalize().unwrap());
        assert_eq!(store.snapshot().unwrap().projects.len(), 1);
    }

    #[test]
    fn a_vanished_project_root_is_an_error_for_the_caller_not_a_panic() {
        let fixture = LayoutFixture::new();
        let project_root = fixture.root.join("Alpha");
        std::fs::create_dir(&project_root).unwrap();
        let store = Store::open(fixture.root.clone()).unwrap();
        let project = store.add_project(project_root.clone(), None).unwrap();
        std::fs::remove_dir(&project_root).unwrap();
        let mut options = OpenOptions::project(None);
        options.project_id = Some(project.id);
        let error = prepare(&fixture.root, options).err().unwrap();
        assert!(error.starts_with("not_found: project folder "));
    }

    #[test]
    fn no_active_project_registers_the_cwd_fallback() {
        let fixture = LayoutFixture::new();
        let prepared = prepare(&fixture.root, OpenOptions::project(None)).unwrap();
        let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
        assert_eq!(prepared.runtime.path, cwd);
        assert_eq!(
            Store::open(fixture.root.clone())
                .unwrap()
                .snapshot()
                .unwrap()
                .projects
                .len(),
            1
        );
    }
    #[test]
    fn params_follow_frozen_schema_and_omit_restore_on_new_windows() {
        let home = Path::new("/isolated");
        let window = PreparedWindow {
            options: OpenOptions::global(),
            opening_size: WindowSize {
                width: 1100.,
                height: 760.,
            },
            runtime: RuntimeWindow::global(home, None, WindowMode::Windowed, Default::default()),
            restore: false,
            notice: None,
        };
        let a = window.params("a", 0);
        let b = window.params("b", 1);
        assert_eq!(a["schema"], 1);
        assert_eq!(a["projectId"], Value::Null);
        assert_eq!(a["global"]["view"], "dashboard");
        assert!(a.get("restore").is_none());
        assert_ne!(a["windowId"], b["windowId"]);
        assert_eq!(b["cascade"], 1);
    }
    #[test]
    fn runtime_frames_keep_the_existing_display_local_top_left_coordinates() {
        let screen = NSRect::new(NSPoint::new(-1600., 80.), NSSize::new(1600., 1000.));
        let frame = NSRect::new(NSPoint::new(-1500., 200.), NSSize::new(800., 600.));
        let bounds = runtime_geometry(frame, screen);
        assert_eq!(
            bounds,
            WindowGeometry {
                x: 100.,
                y: 280.,
                width: 800.,
                height: 600.
            }
        );
        assert_eq!(appkit_frame(bounds, screen), frame);
    }
    #[test]
    fn url_routing_and_window_arguments_are_bounded() {
        assert!(allowed_url("https://example.com"));
        assert!(allowed_url("mailto:a@example.com"));
        assert!(!allowed_url("file:///etc/passwd"));
        assert!(!allowed_url("javascript:alert(1)"));
        assert!(!valid_route(&json!({"kind":"showShell","shellId":"bad"})));
        assert!(valid_route(&json!({"kind":"projectMemo"})));
        // Contracts §2.3 `panel {panel}`: the kind may be implied.
        assert!(valid_route(&json!({"panel":"settings"})));
        assert!(valid_route(&json!({"kind":"panel","panel":"settings"})));
        assert!(!valid_route(&json!({"panel":""})));
        assert!(!valid_route(&json!({"panel":1})));
        assert!(!valid_route(&json!({"kind":"unknown","panel":"settings"})));
        assert!(!valid_route(&json!({})));
        let id = Uuid::new_v4().to_string();
        assert!(valid_route(&json!({"kind":"chat","chatId":id})));
        assert!(valid_route(&json!({"kind":"task","taskId":id})));
        assert!(valid_route(&json!({"kind":"showShell","shellId":id})));
        let mut global = OpenOptions::global();
        global.project_id = Some("bad".into());
        assert!(global.validate().is_err());
    }
    pub(super) struct LayoutFixture {
        pub(super) root: PathBuf,
    }
    /// Registry facts for the pure window decisions: on screen, never closing.
    pub(super) fn facts(
        id: &str,
        kind: WindowKind,
        project_id: Option<&str>,
        focus_serial: u64,
    ) -> WindowFacts {
        WindowFacts {
            id: id.into(),
            kind,
            project_id: project_id.map(str::to_owned),
            focus_serial,
            minimized: false,
            on_active_space: true,
            closing: false,
        }
    }
    impl LayoutFixture {
        pub(super) fn new() -> Self {
            let root = PathBuf::from("/tmp")
                .join(format!("riwork-host-flush-{}", Uuid::new_v4().simple()));
            std::fs::create_dir_all(&root).unwrap();
            Self { root }
        }
        fn session(&self, id: &str) -> Arc<HostLayout> {
            use riwork_nsh_workspace::persistence::{LayoutPersistence, SaveMode};
            let loaded =
                LayoutPersistence::open(&self.root, id, None, SaveMode::LegacyChatState).unwrap();
            host_layouts::registration(id, Arc::new(loaded.persistence), loaded.project).unwrap()
        }
        pub(super) fn window(&self, id: &str) -> (String, RuntimeWindow) {
            (
                id.into(),
                RuntimeWindow {
                    project_id: Some(id.into()),
                    path: self.root.join(id),
                    bounds: None,
                    mode: WindowMode::Windowed,
                    layout: None,
                    focus_mode: false,
                    focus_centered: false,
                    kind: WindowKind::Project,
                    global: None,
                },
            )
        }
    }
    impl Drop for LayoutFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn close_flushes_the_last_accepted_edit_before_the_window_goes() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _entered = runtime.enter();
        let fixture = LayoutFixture::new();
        let id = format!("close-{}", Uuid::new_v4());
        let layout = fixture.session(&id);
        let mut edited = layout.project_layout();
        edited.selected_task_id = Some("last-accepted".into());
        layout.schedule(edited.clone()).unwrap();
        let closing = ClosingWindows::new(vec![fixture.window(&id)]);
        let mut rejected = edited.clone();
        rejected.selected_task_id = Some("after-close".into());
        assert!(layout.schedule(rejected).is_err());
        let published = closing.flush(&fixture.root).unwrap();
        assert_eq!(published[0].layout.as_ref(), Some(&edited));
        let saved = riwork_core::layouts::LayoutStore::open(&fixture.root)
            .unwrap()
            .load(&id)
            .unwrap()
            .unwrap();
        assert_eq!(saved.selected_task_id, edited.selected_task_id);
    }

    #[test]
    fn quit_flushes_every_layout_session_and_the_global_state_from_rust() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _entered = runtime.enter();
        let fixture = LayoutFixture::new();
        let ids = [
            format!("quit-a-{}", Uuid::new_v4()),
            format!("quit-b-{}", Uuid::new_v4()),
        ];
        let sessions = ids.iter().map(|id| fixture.session(id)).collect::<Vec<_>>();
        for (index, layout) in sessions.iter().enumerate() {
            let mut edited = layout.project_layout();
            edited.selected_task_id = Some(format!("edited-{index}"));
            layout.schedule(edited).unwrap();
        }
        let mut windows = ids.iter().map(|id| fixture.window(id)).collect::<Vec<_>>();
        // TSX saved its view and filter itself (Dashboard.saveGlobalWindow) after
        // the window opened; the host's copy is the one from open time.
        let mut saved = GlobalWindowState {
            view: GlobalView::Chat,
            ..GlobalWindowState::default()
        };
        saved.set_filter(riwork_core::global_window::DashboardFilter::All);
        GlobalWindowStore::at(&fixture.root).save(&saved).unwrap();
        let host = GlobalWindowState {
            window_size: Some(WindowSize {
                width: 900.,
                height: 650.,
            }),
            ..GlobalWindowState::default()
        };
        windows.push((
            format!("quit-global-{}", Uuid::new_v4()),
            RuntimeWindow::global(&fixture.root, None, WindowMode::Windowed, host.clone()),
        ));
        let closing = ClosingWindows::new(windows);
        let published = closing.flush(&fixture.root).unwrap();
        assert_eq!(published.len(), 3);
        // The size is the host's; everything else is what TSX saved.
        let global = GlobalWindowState {
            window_size: host.window_size,
            ..saved
        };
        assert_eq!(published[2].global.as_ref(), Some(&global));
        let store = riwork_core::layouts::LayoutStore::open(&fixture.root).unwrap();
        for (index, id) in ids.iter().enumerate() {
            assert_eq!(
                store.load(id).unwrap().unwrap().selected_task_id,
                Some(format!("edited-{index}"))
            );
        }
        assert_eq!(GlobalWindowStore::at(&fixture.root).load(), global);
    }

    #[test]
    fn a_view_tsx_saved_survives_a_host_flush_and_the_runtime_publish() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _entered = runtime.enter();
        let fixture = LayoutFixture::new();
        let home = &fixture.root;
        // The host opened the global window on the dashboard ...
        let prepared = prepare(home, OpenOptions::global()).unwrap();
        assert_eq!(
            prepared.runtime.global.as_ref().unwrap().view,
            GlobalView::Dashboard
        );
        // ... and the user switched it to Chat inside the window, saved by TSX.
        let saved = GlobalWindowState {
            view: GlobalView::Chat,
            ..GlobalWindowState::default()
        };
        GlobalWindowStore::at(home).save(&saved).unwrap();
        let screen = NSRect::new(NSPoint::new(0., 0.), NSSize::new(1800., 1100.));
        let normal = NSRect::new(NSPoint::new(10., 10.), NSSize::new(1000., 700.));
        let id = format!("tsx-view-{}", Uuid::new_v4());
        let recorded = recorded_runtime(&prepared.runtime, normal, screen, WindowMode::Windowed);
        let expected = GlobalWindowState {
            window_size: Some(WindowSize {
                width: 1000.,
                height: 700.,
            }),
            ..saved
        };
        let published = current_layouts(home, &[(id.clone(), recorded.clone())]);
        assert_eq!(published[0].global.as_ref(), Some(&expected));
        let closing = ClosingWindows::new(vec![(id, recorded)]);
        let flushed = closing.flush(home).unwrap();
        closing.cancel();
        assert_eq!(flushed[0].global.as_ref(), Some(&expected));
        assert_eq!(GlobalWindowStore::at(home).load(), expected);
    }

    #[test]
    fn failed_flush_keeps_the_snapshot_and_cancel_restores_editing() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _entered = runtime.enter();
        let fixture = LayoutFixture::new();
        let id = format!("failed-close-{}", Uuid::new_v4());
        let layout = fixture.session(&id);
        let mut edited = layout.project_layout();
        edited.selected_task_id = Some("not-lost".into());
        let lock = fixture.root.join("layouts.lock");
        std::fs::remove_file(&lock).unwrap();
        std::fs::create_dir(&lock).unwrap();
        layout.schedule(edited.clone()).unwrap();
        let closing = ClosingWindows::new(vec![fixture.window(&id)]);
        assert!(closing.flush(&fixture.root).is_err());
        assert_eq!(layout.project_layout(), edited);
        closing.cancel();
        std::fs::remove_dir(&lock).unwrap();
        layout.schedule(edited.clone()).unwrap();
        layout.flush_blocking().unwrap();
        assert_eq!(
            riwork_core::layouts::LayoutStore::open(&fixture.root)
                .unwrap()
                .load(&id)
                .unwrap()
                .unwrap()
                .selected_task_id,
            edited.selected_task_id
        );
    }

    #[test]
    fn closing_a_loading_window_rejects_late_layout_registration() {
        use riwork_nsh_workspace::persistence::{LayoutPersistence, SaveMode};
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _entered = runtime.enter();
        let fixture = LayoutFixture::new();
        let id = format!("loading-close-{}", Uuid::new_v4());
        let loaded =
            LayoutPersistence::open(&fixture.root, &id, None, SaveMode::LegacyChatState).unwrap();
        let persistence = Arc::new(loaded.persistence);
        let closing = ClosingWindows::new(vec![fixture.window(&id)]);
        assert!(
            host_layouts::registration(&id, persistence.clone(), loaded.project.clone()).is_err()
        );
        closing.flush(&fixture.root).unwrap();
        closing.cancel();
        assert!(host_layouts::registration(&id, persistence, loaded.project).is_ok());
    }

    #[test]
    fn runtime_publication_reads_current_layout_without_an_appkit_state_event() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _entered = runtime.enter();
        let fixture = LayoutFixture::new();
        let id = format!("snapshot-{}", Uuid::new_v4());
        let layout = fixture.session(&id);
        let windows = vec![fixture.window(&id)];
        let mut edited = layout.project_layout();
        edited.selected_task_id = Some("newest".into());
        layout.publish(edited.clone());
        assert_eq!(
            current_layouts(&fixture.root, &windows)[0].layout.as_ref(),
            Some(&edited)
        );
    }

    #[test]
    fn switched_layout_identity_drives_routing_publication_and_close_restore_metadata() {
        use riwork_nsh_workspace::persistence::{LayoutPersistence, SaveMode};

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _entered = runtime.enter();
        let fixture = LayoutFixture::new();
        let store = Store::open(fixture.root.clone()).unwrap();
        let original = store
            .create_project(fixture.root.join("original"), Some("Original"), false)
            .unwrap();
        let destination = store
            .create_project(fixture.root.join("destination"), Some("Destination"), false)
            .unwrap();
        let id = format!("switched-{}", Uuid::new_v4());
        // Simulate the state after LayoutSession switched its registered writer:
        // AppKit still holds the original launch RuntimeWindow.
        let loaded = LayoutPersistence::open(
            &fixture.root,
            &destination.id,
            None,
            SaveMode::LegacyChatState,
        )
        .unwrap();
        let layout =
            host_layouts::registration(&id, Arc::new(loaded.persistence), loaded.project).unwrap();
        let mut launched = fixture.window(&id);
        launched.1.project_id = Some(original.id.clone());
        launched.1.path = original.root;
        assert_eq!(
            current_project_id(&id, &launched.1),
            Some(destination.id.clone())
        );
        assert_ne!(current_project_id(&id, &launched.1), Some(original.id));
        let published = current_layouts(&fixture.root, &[launched.clone()]);
        assert_eq!(
            published[0].project_id.as_deref(),
            Some(destination.id.as_str())
        );
        assert_eq!(published[0].path, destination.root);
        assert_eq!(published[0].layout.as_ref(), Some(&layout.project_layout()));
        let closing = ClosingWindows::new(vec![launched]);
        let restored = closing.flush(&fixture.root).unwrap();
        assert_eq!(
            restored[0].project_id.as_deref(),
            Some(destination.id.as_str())
        );
        assert_eq!(restored[0].path, destination.root);
        assert_eq!(restored[0].layout.as_ref(), Some(&layout.project_layout()));
        closing.cancel();
    }
}

#[cfg(test)]
mod placement {
    use super::*;
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn the_cascade_counts_open_non_tooltip_windows() {
            let mut registry = BTreeMap::new();
            registry.insert("alpha".into(), WindowKind::Project);
            registry.insert("global".into(), WindowKind::Global);
            assert_eq!(window_count(&registry), 2);
            assert!(serde_json::from_value::<OpenOptions>(json!({"kind":"tooltip"})).is_err());
            let size = WindowSize {
                width: 1220.,
                height: 780.,
            };
            let display = WindowFrame {
                x: 0.,
                y: 0.,
                width: 1800.,
                height: 1200.,
            };
            let first = WindowFrame::opening(size, display, 0);
            let second = WindowFrame::opening(size, display, window_count(&registry) - 1);
            assert_eq!((second.x - first.x, second.y - first.y), (22., 22.));
            let fifth = WindowFrame::opening(size, display, 5);
            assert_eq!(first, fifth);
        }
        #[test]
        fn zoomed_counts_as_maximized_and_keeps_its_normal_frame() {
            let normal = NSRect::new(NSPoint::new(100., 150.), NSSize::new(800., 600.));
            let zoomed = NSRect::new(NSPoint::new(0., 0.), NSSize::new(1800., 1100.));
            assert_eq!(window_mode(false, true), WindowMode::Maximized);
            assert_eq!(
                normal_frame(normal, zoomed, window_mode(false, true), false),
                normal
            );
            assert_eq!(
                normal_frame(normal, zoomed, WindowMode::Windowed, true),
                normal
            );
            assert_eq!(
                normal_frame(normal, zoomed, WindowMode::Windowed, false),
                zoomed
            );
        }
        #[test]
        fn fullscreen_counts_as_fullscreen() {
            assert_eq!(window_mode(true, false), WindowMode::Fullscreen);
            assert_eq!(window_mode(true, true), WindowMode::Fullscreen);
            let normal = NSRect::new(NSPoint::new(40., 30.), NSSize::new(640., 400.));
            let fullscreen = NSRect::new(NSPoint::new(0., 0.), NSSize::new(1800., 1200.));
            assert_eq!(
                normal_frame(normal, fullscreen, window_mode(true, true), false),
                normal
            );
        }
    }
}
#[cfg(test)]
mod window_registry {
    #[cfg(test)]
    mod tests {
        use super::super::*;
        #[test]
        fn the_last_close_quits() {
            let mut registry = BTreeMap::from([
                ("alpha".into(), WindowKind::Project),
                ("global".into(), WindowKind::Global),
            ]);
            registry.remove("alpha");
            assert!(!last_close_quits(window_count(&registry)));
            registry.remove("global");
            assert!(last_close_quits(window_count(&registry)));
        }
        #[test]
        fn tooltips_do_not_count() {
            assert!(serde_json::from_value::<OpenOptions>(json!({"kind":"tooltip"})).is_err());
            let registry = BTreeMap::<String, WindowKind>::new();
            assert_eq!(window_count(&registry), 0);
            assert!(last_close_quits(window_count(&registry)));
        }

        use super::super::tests::{LayoutFixture, facts};

        #[test]
        fn the_global_window_is_a_singleton_and_opens_with_no_window() {
            let fixture = LayoutFixture::new();
            // View ▸ Global Chat / ⌘⇧O and Global Dashboard / ⌘⌥G have no calling
            // window: Host::menu sends exactly these options to the service.
            let options = global_open_options("openGlobalChat").unwrap();
            options.validate().unwrap();
            assert_eq!(
                (options.kind, options.view.as_deref(), options.start),
                (WindowKind::Global, Some("chat"), Some(true))
            );
            let dashboard = global_open_options("openGlobalDashboard").unwrap();
            assert_eq!(
                (dashboard.view.as_deref(), dashboard.start),
                (Some("dashboard"), Some(false))
            );
            assert!(global_open_options("newWindow").is_none());
            // TSX window.open {kind: "global"} is the same request.
            let from_tsx: OpenOptions =
                serde_json::from_value(json!({"kind":"global","view":"chat","start":true}))
                    .unwrap();
            from_tsx.validate().unwrap();
            // The service prepares it from the home alone.
            let prepared = prepare(&fixture.root, options).unwrap();
            assert!(prepared.runtime.is_global());
            assert_eq!(prepared.runtime.project_id, None);
            let intent = OpenIntent::default();
            let reuse = intent.reuses_project_window();
            // No window at all: it is created.
            assert_eq!(
                open_target(&prepared.runtime, reuse, &[]),
                OpenTarget::Create
            );
            // Project windows only: still created.
            let project = Uuid::new_v4().to_string();
            let alpha = facts("alpha", WindowKind::Project, Some(&project), 3);
            assert_eq!(
                open_target(&prepared.runtime, reuse, std::slice::from_ref(&alpha)),
                OpenTarget::Create
            );
            // Once it exists, every way in reuses it (focus) — minimised or on
            // another Space too — and it gets `global.show`; created: false.
            let global = facts("global", WindowKind::Global, None, 1);
            for existing in [
                global.clone(),
                WindowFacts {
                    minimized: true,
                    ..global.clone()
                },
                WindowFacts {
                    on_active_space: false,
                    ..global.clone()
                },
            ] {
                let windows = [alpha.clone(), existing];
                assert_eq!(
                    open_target(&prepared.runtime, reuse, &windows),
                    OpenTarget::Reuse("global".into())
                );
                assert_eq!(
                    open_target(&prepared.runtime, true, &windows),
                    OpenTarget::Reuse("global".into())
                );
            }
            assert_eq!(
                open_events(&prepared, &intent),
                vec![("global.show", json!({"view":"chat","start":true}))]
            );
            assert_eq!(
                open_result("global", false, false),
                json!({"windowId":"global","created":false})
            );
            assert_eq!(
                open_result("new", true, false),
                json!({"windowId":"new","created":true})
            );
            // A global window being released no longer counts.
            let closing = WindowFacts {
                closing: true,
                ..global
            };
            assert_eq!(
                open_target(&prepared.runtime, reuse, &[alpha, closing]),
                OpenTarget::Create
            );
        }

        #[test]
        fn route_prefers_frontmost_then_minimised_then_opens() {
            let fixture = LayoutFixture::new();
            let store = Store::open(fixture.root.clone()).unwrap();
            let alpha = store
                .create_project(fixture.root.join("alpha"), Some("Alpha"), false)
                .unwrap();
            let beta = store
                .create_project(fixture.root.join("beta"), Some("Beta"), false)
                .unwrap();
            let active = store.snapshot().unwrap().active_project_id;
            let p = beta.id.as_str();
            let global = facts("global", WindowKind::Global, None, 20);
            let other = facts("other", WindowKind::Project, Some(&alpha.id), 19);
            let front = facts("front", WindowKind::Project, Some(p), 3);
            let behind = facts("behind", WindowKind::Project, Some(p), 2);
            let minimized = WindowFacts {
                minimized: true,
                ..facts("minimized", WindowKind::Project, Some(p), 9)
            };
            let away = WindowFacts {
                on_active_space: false,
                ..facts("away", WindowKind::Project, Some(p), 7)
            };
            let mut windows = vec![
                global.clone(),
                minimized,
                away,
                other.clone(),
                behind,
                front,
            ];
            // On-screen windows front to back first, even before a minimised
            // window that was key more recently.
            assert_eq!(route_target(&windows, p).as_deref(), Some("front"));
            windows.retain(|w| w.id != "front");
            assert_eq!(route_target(&windows, p).as_deref(), Some("behind"));
            windows.retain(|w| w.id != "behind");
            // Then minimised windows and windows on other Spaces, most recent first.
            assert_eq!(route_target(&windows, p).as_deref(), Some("minimized"));
            windows.retain(|w| w.id != "minimized");
            assert_eq!(route_target(&windows, p).as_deref(), Some("away"));
            // A window being released is no target.
            for window in &mut windows {
                window.closing |= window.id == "away";
            }
            assert_eq!(route_target(&windows, p), None);
            // The global window is searched first (most recent) but never chosen.
            assert_eq!(search_order(&windows)[0].id, "global");
            // None shows the project: the existing open-by-id path. TSX sends the
            // contracts' `{panel}` step shape; the host validates and forwards it.
            let step = json!({"panel": "settings"});
            assert!(valid_route(&step));
            let mut options = OpenOptions::project(None);
            options.project_id = Some(beta.id.clone());
            let prepared = prepare(&fixture.root, options).unwrap();
            assert_eq!(prepared.runtime.project_id.as_deref(), Some(p));
            assert_eq!(prepared.runtime.path, beta.root);
            let intent = OpenIntent {
                route: Some(step.clone()),
                ..OpenIntent::default()
            };
            assert!(intent.reuses_project_window());
            assert_eq!(
                open_target(&prepared.runtime, true, &windows),
                OpenTarget::Create
            );
            // Only `route`: the global window gets no `global.show` and keeps its view.
            assert_eq!(
                open_events(&prepared, &intent),
                vec![("route", json!({"step": step}))]
            );
            assert_eq!(open_result("new", true, true), json!({"windowId":"new"}));
            // A window showing it now is reused (and raised) instead.
            windows.push(WindowFacts {
                minimized: true,
                ..facts("beta-window", WindowKind::Project, Some(p), 1)
            });
            assert_eq!(
                open_target(&prepared.runtime, true, &windows),
                OpenTarget::Reuse("beta-window".into())
            );
            // The store's active project is unchanged.
            assert_eq!(store.snapshot().unwrap().active_project_id, active);
        }

        #[test]
        fn the_global_window_opens_at_its_own_size() {
            let fixture = LayoutFixture::new();
            let prepared = prepare(&fixture.root, OpenOptions::global()).unwrap();
            assert_eq!(
                prepared.opening_size,
                WindowSize {
                    width: 1100.,
                    height: 760.
                }
            );
            assert_eq!(window_title(&prepared.runtime), "RiWork · Global");
            assert_eq!(window_title(&fixture.window("project").1), "RiWork");
            let saved = GlobalWindowState {
                window_size: WindowSize::new(900., 650.),
                ..GlobalWindowState::default()
            };
            GlobalWindowStore::at(&fixture.root).save(&saved).unwrap();
            let prepared = prepare(&fixture.root, OpenOptions::global()).unwrap();
            assert_eq!(
                prepared.opening_size,
                WindowSize {
                    width: 900.,
                    height: 650.
                }
            );
            // An impossible saved size is dropped: back to 1100×760.
            std::fs::write(
                GlobalWindowStore::at(&fixture.root).path(),
                r#"{"window_size":{"width":10.0,"height":10.0}}"#,
            )
            .unwrap();
            assert_eq!(
                prepare(&fixture.root, OpenOptions::global())
                    .unwrap()
                    .opening_size,
                WindowSize {
                    width: 1100.,
                    height: 760.
                }
            );
        }
    }
}

#[cfg(test)]
mod runtime_window {
    #[cfg(test)]
    mod tests {
        use super::super::tests::LayoutFixture;
        use super::super::*;

        #[test]
        fn the_global_window_is_recorded_as_global() {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let _entered = runtime.enter();
            let fixture = LayoutFixture::new();
            let home = &fixture.root;
            let prepared = prepare(home, OpenOptions::global()).unwrap();
            // TSX saved the Chat view after the window opened.
            let saved = GlobalWindowState {
                view: GlobalView::Chat,
                ..GlobalWindowState::default()
            };
            GlobalWindowStore::at(home).save(&saved).unwrap();
            let screen = NSRect::new(NSPoint::new(0., 0.), NSSize::new(1800., 1100.));
            let normal = NSRect::new(NSPoint::new(100., 200.), NSSize::new(900., 650.));
            // What snapshot() records for it (publish, close, quit, reload).
            let recorded =
                recorded_runtime(&prepared.runtime, normal, screen, WindowMode::Windowed);
            assert_eq!(recorded.kind, WindowKind::Global);
            assert_eq!(recorded.project_id, None);
            assert_eq!(recorded.layout, None);
            assert_eq!(recorded.path, home.join("global-window"));
            assert_eq!(
                recorded.bounds,
                Some(WindowGeometry {
                    x: 100.,
                    y: 250.,
                    width: 900.,
                    height: 650.
                })
            );
            let encoded = serde_json::to_string(&recorded).unwrap();
            assert!(encoded.contains(r#""kind":"global""#), "{encoded}");
            // The quit/reload flush hands it over as itself.
            let id = format!("runtime-global-{}", Uuid::new_v4());
            let closing = ClosingWindows::new(vec![(id, recorded)]);
            let handed = closing.flush(home).unwrap().remove(0);
            closing.cancel();
            let expected = GlobalWindowState {
                window_size: Some(WindowSize {
                    width: 900.,
                    height: 650.,
                }),
                ..saved
            };
            assert_eq!(handed.kind, WindowKind::Global);
            assert_eq!(handed.project_id, None);
            assert_eq!(handed.layout, None);
            assert_eq!(handed.path, home.join("global-window"));
            assert_eq!(handed.global.as_ref(), Some(&expected));
            let encoded = serde_json::to_string(&handed).unwrap();
            assert!(encoded.contains(r#""kind":"global""#), "{encoded}");
            let handed: RuntimeWindow = serde_json::from_str(&encoded).unwrap();
            // The file changes after the handover: the handed-over state wins.
            GlobalWindowStore::at(home)
                .save(&GlobalWindowState::default())
                .unwrap();
            let restored = restored_window(home, &handed);
            assert!(restored.restore);
            assert!(restored.options.validate().is_ok());
            assert_eq!(
                restored.opening_size,
                WindowSize {
                    width: 900.,
                    height: 650.
                }
            );
            let params = restored.params("restored", 0);
            assert_eq!(params["kind"], "global");
            assert_eq!(params["projectId"], Value::Null);
            assert_eq!(params["path"], json!(home.join("global-window")));
            assert_eq!(params["restore"]["layout"], Value::Null);
            assert_eq!(
                params["restore"]["global"],
                serde_json::to_value(&expected).unwrap()
            );
            assert_eq!(params["restore"]["global"]["view"], "chat");
            assert_eq!(params["global"]["view"], "chat");
            // `<home>/global-window` is a name, never a directory or file.
            assert!(!home.join("global-window").exists());
        }
    }
}

#[cfg(test)]
mod menu {
    #[cfg(test)]
    mod tests {
        use super::super::tests::{LayoutFixture, facts};
        use super::super::*;

        #[test]
        fn dock_new_window() {
            let (a, b) = (Uuid::new_v4().to_string(), Uuid::new_v4().to_string());
            let alpha = facts("alpha", WindowKind::Project, Some(&a), 2);
            let beta = WindowFacts {
                minimized: true,
                ..facts("beta", WindowKind::Project, Some(&b), 5)
            };
            let remote = facts("remote", WindowKind::Project, Some("ssh:devbox/repo"), 9);
            let global = facts("global", WindowKind::Global, None, 10);
            // 1. A local project window exists: the most recently focused one's
            //    project; remote-project windows and the global window are skipped.
            assert_eq!(
                new_window_plan(&[alpha.clone(), beta.clone(), remote.clone(), global.clone()]),
                NewWindow::Project(b.clone())
            );
            assert_eq!(
                new_window_plan(&[alpha.clone(), remote.clone()]),
                NewWindow::Project(a.clone())
            );
            let closing_beta = WindowFacts {
                closing: true,
                ..beta
            };
            assert_eq!(
                new_window_plan(&[alpha, closing_beta.clone(), global.clone()]),
                NewWindow::Project(a)
            );
            // 2. Only the global window: its TSX gets `command {id: "newWindow"}`
            //    and resolves frontmost project window / active project / notice.
            assert_eq!(
                new_window_plan(std::slice::from_ref(&global)),
                NewWindow::AskGlobal("global".into())
            );
            assert_eq!(
                new_window_plan(&[remote.clone(), closing_beta, global]),
                NewWindow::AskGlobal("global".into())
            );
            assert_eq!(new_window_command(), json!({"id": "newWindow"}));
            // 3. No window at all (or only a remote one): today's prepare(None),
            //    which opens the store's active project.
            assert_eq!(new_window_plan(&[]), NewWindow::Default);
            assert_eq!(new_window_plan(&[remote]), NewWindow::Default);
            let fixture = LayoutFixture::new();
            let store = Store::open(fixture.root.clone()).unwrap();
            store
                .create_project(fixture.root.join("first"), Some("First"), false)
                .unwrap();
            let active = store
                .create_project(fixture.root.join("second"), Some("Second"), false)
                .unwrap();
            store.use_project(&active.id).unwrap();
            let prepared = prepare(&fixture.root, OpenOptions::project(None)).unwrap();
            assert_eq!(
                prepared.runtime.project_id.as_deref(),
                Some(active.id.as_str())
            );
        }
    }
}
#[cfg(test)]
mod window_state {
    #[cfg(test)]
    mod tests {
        use super::super::*;
        #[test]
        fn events_only_on_change() {
            let baseline = json!({"key":false,"visible":true,"occluded":false,"mode":"windowed","backingScale":2.,"frame":{"x":100.,"y":100.,"width":800.,"height":600.},"appActive":true});
            assert!(changed_state(&Value::Null, &baseline));
            assert!(!changed_state(&baseline, &baseline.clone()));
            for (key, value) in [
                ("key", json!(true)),
                ("visible", json!(false)),
                ("occluded", json!(true)),
                ("mode", json!("fullscreen")),
                ("backingScale", json!(1.)),
                (
                    "frame",
                    json!({"x":122.,"y":122.,"width":800.,"height":600.}),
                ),
                ("appActive", json!(false)),
            ] {
                let mut next = baseline.clone();
                next[key] = value;
                assert!(changed_state(&baseline, &next), "{key}");
                assert!(!changed_state(&next, &next));
            }
        }
    }
}

/// The actual registration list deliberately excludes cross-window tab transfer.
mod host_names {
    pub(super) const OPERATIONS: &[&str] = &[
        "window.open",
        "window.route",
        "window.placement",
        "window.list",
        "window.focus",
        "window.close",
        "window.setDockInfo",
        "app.restart",
        "app.quit",
        "app.restoreReady",
        "app.setAppearance",
        "system.openWith",
        "system.revealInFinder",
        "system.fonts",
        "system.notify",
    ];
    #[cfg(test)]
    mod tests {
        #[test]
        fn tab_transfer_is_refused() {
            for reserved in ["tab.transfer", "tab.receive"] {
                assert!(!super::OPERATIONS.contains(&reserved));
            }
        }
    }
}
