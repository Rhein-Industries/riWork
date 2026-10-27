//! AppKit owns windows; every window owns an independent NSH engine/channel.
//! Disk, runtime registry and project resolution live on the service thread.
use crate::{
    nsh_menus::{DockWindow, HostMenus, MenuAction},
    nsh_native_stub as native,
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
        NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate,
        NSApplicationTerminateReply, NSBackingStoreType, NSEvent, NSEventModifierFlags,
        NSEventType, NSMenu, NSScreen, NSWindow, NSWindowButton, NSWindowDelegate,
        NSWindowOcclusionState, NSWindowStyleMask, NSWindowTitleVisibility,
    },
    objc2_foundation::{
        NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
    },
    set_ui_sets,
};
use riwork_core::{
    layouts::{WindowFrame, WindowSize},
    paths,
    store::Store,
};
use riwork_engine::runtime::{RuntimeWindow, WindowGeometry, WindowKind, WindowMode};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

const APP_ID: &str = "dev.riwork.shell";
const CAPABILITIES: &str = r#"{"host":["window.*","app.*","system.*"]}"#;
const RESTART_PENDING: &str =
    "unavailable: NSH restart awaits the P1a Rust layout and draft flush hooks";

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

// Service commands contain plain data only. No AppKit object leaves the main thread.
enum Service {
    Snapshot(Vec<(String, RuntimeWindow)>),
    Ready(String),
    Open(OpenOptions, Option<HostCall>, Option<Value>, bool),
    Quit,
    Finished,
    Wake,
}
enum Main {
    Call(String, &'static str, HostCall),
    Open(PreparedWindow, Option<HostCall>, Option<Value>, bool),
    State(String),
    Close(String),
    Menu(MenuAction),
    Quit,
    Finish,
    Error(String),
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
}
struct Host {
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
    background: bool,
}
impl Host {
    fn open(&mut self, prepared: PreparedWindow) -> Result<String, String> {
        let id = Uuid::new_v4().to_string();
        let channel = HostChannel::new();
        channel.set_params(
            prepared
                .params(&id, self.windows.len())
                .as_object()
                .unwrap()
                .clone(),
        );
        for name in [
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
        ] {
            let id = id.clone();
            channel.handle(name, move |call| send(Main::Call(id.clone(), name, call)));
        }
        // All other v1 operations fail explicitly until their owning feature lands.
        for name in [
            "system.revealInFinder",
            "system.openWith",
            "system.quickLook",
            "system.openPanel",
            "system.savePanel",
            "system.readPasteboard",
            "system.notify",
            "system.share",
            "system.fonts",
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
        let frame = opening_frame(self.mtm, prepared.runtime.bounds, self.windows.len());
        let view = AppView::new(
            self.mtm,
            AppOptions {
                app_id: APP_ID.into(),
                title: "RiWork".into(),
                size: (frame.size.width as i32, frame.size.height as i32),
                appearance: Appearance::System,
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
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(self.mtm),
                frame,
                NSWindowStyleMask::Titled
                    | NSWindowStyleMask::Closable
                    | NSWindowStyleMask::Miniaturizable
                    | NSWindowStyleMask::Resizable
                    | NSWindowStyleMask::FullSizeContentView,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe {
            window.setReleasedWhenClosed(false);
        }
        window.setContentMinSize(NSSize::new(640., 400.));
        window.setTitlebarAppearsTransparent(true);
        window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
        window.setTitle(&NSString::from_str(if prepared.runtime.is_global() {
            "RiWork — Global"
        } else {
            "RiWork — Project"
        }));
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
            },
        );
        self.state(&id);
        self.publish();
        Ok(id)
    }
    fn snapshot(&self) -> Vec<(String, RuntimeWindow)> {
        self.windows
            .iter()
            .map(|(id, window)| {
                let mut runtime = window.runtime.clone();
                let screen = window
                    .window
                    .screen()
                    .or_else(|| NSScreen::mainScreen(self.mtm))
                    .map(|screen| screen.frame())
                    .unwrap_or(window.normal);
                runtime.bounds = Some(runtime_geometry(window.normal, screen));
                runtime.mode = mode(&window.window);
                (id.clone(), runtime)
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
            if mode(&w.window) == WindowMode::Windowed && !w.window.isMiniaturized() {
                w.normal = w.window.frame();
            }
            let state = json!({"key": w.window.isKeyWindow(), "visible": w.window.isVisible(),
                "occluded": !w.window.occlusionState().contains(NSWindowOcclusionState::Visible),
                "mode": mode(&w.window), "backingScale": w.window.backingScaleFactor(),
                "frame": geometry(w.window.frame()), "appActive": self.app.isActive()});
            if state != w.last_state {
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
    fn close(&mut self, id: &str, reason: &str) {
        if let Some(w) = self.windows.remove(id) {
            let _ = w
                .view
                .host()
                .send("window.willClose", json!({"reason":reason}));
            // P1a: flush the Rust LayoutSession here, before stopping the engine.
            w.view.stop();
            w.window.setDelegate(None);
            w.window.close();
            self.publish();
        }
        if self.windows.is_empty() && !self.quitting {
            self.quit();
        }
    }
    fn quit(&mut self) {
        if self.quitting {
            return;
        }
        self.quitting = true;
        self.publish();
        // Service acknowledges publication; future Drafts/LayoutSession flushes belong there.
        let _ = self.service.send(Service::Quit);
    }
    fn handle(&mut self, message: Main) {
        match message {
            Main::Call(id, name, call) => self.call(id, name, call),
            Main::Open(prepared, call, route, forwarded) => {
                if self.quitting {
                    if let Some(call) = call {
                        call.reject("unavailable: host is quitting");
                    }
                    return;
                }
                let existing = if prepared.runtime.is_global() {
                    self.windows
                        .iter()
                        .find(|(_, w)| w.runtime.is_global())
                        .map(|(id, _)| id.clone())
                } else if route.is_some() || forwarded {
                    self.windows
                        .iter()
                        .filter(|(_, w)| w.runtime.project_id == prepared.runtime.project_id)
                        .max_by_key(|(_, w)| w.focus_serial)
                        .map(|(id, _)| id.clone())
                } else {
                    None
                };
                let global = prepared.runtime.is_global();
                let show = json!({"view": prepared.options.view.as_deref().unwrap_or("dashboard"), "start": prepared.options.start.unwrap_or(false)});
                let project = json!({"projectId": prepared.runtime.project_id, "path": prepared.runtime.path});
                let created = existing.is_none();
                let result = existing.map(Ok).unwrap_or_else(|| self.open(prepared));
                match result {
                    Ok(id) => {
                        if !created {
                            let _ = self.focus(&id);
                        }
                        if let Some(w) = self.windows.get(&id) {
                            if global {
                                let _ = w.view.host().send("global.show", show);
                            }
                            if forwarded {
                                let _ = w.view.host().send("project.open", project);
                            }
                            if let Some(step) = &route {
                                let _ = w.view.host().send("route", json!({"step":step}));
                            }
                        }
                        if let Some(call) = call {
                            call.resolve(if route.is_some() {
                                json!({"windowId":id})
                            } else {
                                json!({"windowId":id,"created":created})
                            });
                        }
                    }
                    Err(error) => {
                        if let Some(call) = call {
                            call.reject(error);
                        } else {
                            eprintln!("riwork: {error}");
                        }
                    }
                }
            }
            Main::State(id) => self.state(&id),
            Main::Close(id) => self.close(&id, "user"),
            Main::Menu(action) => self.menu(action),
            Main::Quit => self.quit(),
            Main::Finish => {
                for id in self.windows.keys().cloned().collect::<Vec<_>>() {
                    self.close(&id, "quit");
                }
                stop(&self.app);
            }
            Main::Error(error) => {
                eprintln!("riwork: {error}");
                self.quit();
            }
        }
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
                let mut options = OpenOptions::global();
                options.view = Some(
                    if action.command == "openGlobalChat" {
                        "chat"
                    } else {
                        "dashboard"
                    }
                    .into(),
                );
                options.start = Some(action.command == "openGlobalChat");
                let _ = self.service.send(Service::Open(options, None, None, false));
            }
            "newWindow" | "newProjectWindow" => {
                let mut options = OpenOptions::project(None);
                options.project_id = self
                    .windows
                    .values()
                    .filter(|w| !w.runtime.is_global())
                    .max_by_key(|w| w.focus_serial)
                    .and_then(|w| w.runtime.project_id.clone());
                let _ = self.service.send(Service::Open(options, None, None, false));
            }
            command => {
                let selected = self
                    .windows
                    .values()
                    .find(|w| Some(w.window.windowNumber()) == action.window_number)
                    .or_else(|| self.windows.values().find(|w| w.window.isKeyWindow()));
                if let Some(w) = selected {
                    let _ = w.view.host().send("command", json!({"id":command}));
                }
            }
        }
    }
    fn call(&mut self, id: String, name: &str, call: HostCall) {
        if !self.windows.contains_key(&id) {
            call.reject("not_found: calling window");
            return;
        }
        if self.quitting {
            call.reject("unavailable: host is quitting");
            return;
        }
        let data = call.data().clone();
        match name {
            "window.open" => match serde_json::from_value::<OpenOptions>(data).map_err(|e| format!("invalid: {e}")).and_then(|o| { o.validate()?; Ok(o) }) {
                Ok(options) => { let _ = self.service.send(Service::Open(options, Some(call), None, false)); }
                Err(error) => call.reject(error),
            },
            "window.route" => {
                let project = data["projectId"].as_str();
                if project.is_none() || !valid_route(&data["step"]) { call.reject("invalid: projectId and a valid route step are required"); return; }
                let mut options = OpenOptions::project(None); options.project_id = project.map(str::to_owned);
                let _ = self.service.send(Service::Open(options, Some(call), Some(data["step"].clone()), false));
            }
            "window.list" => call.resolve(Value::Array(self.windows.iter().map(|(id,w)| json!({
                "windowId":id,"kind":w.runtime.kind,"projectId":w.runtime.project_id,
                "title":w.window.title().to_string(),"key":w.window.isKeyWindow(),"mode":mode(&w.window)})).collect())),
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
                    Some(target) if self.windows.contains_key(target) => { let target = target.to_owned(); call.resolve(json!({})); self.close(&target,"user"); }
                    _ => call.reject("not_found: window"),
                }
            }
            "window.setDockInfo" => { self.windows.get_mut(&id).unwrap().dock_info = data; call.resolve(json!({})); }
            "app.restoreReady" => { let _ = self.service.send(Service::Ready(id)); call.resolve(json!({})); }
            "app.quit" => { call.resolve(json!({})); self.quit(); }
            "app.restart" => call.reject(RESTART_PENDING),
            "app.setAppearance" => call.reject("unavailable: process appearance service awaits P1a"),
            _ => call.reject("invalid: unregistered host operation"),
        }
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
    let kind = step
        .get("kind")
        .or_else(|| step.get("type"))
        .and_then(Value::as_str);
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
        Some("apiKeySettings" | "projectMemo") => true,
        _ => false,
    }
}
fn position_controls(window: &NSWindow) {
    for (index, kind) in [
        NSWindowButton::CloseButton,
        NSWindowButton::MiniaturizeButton,
        NSWindowButton::ZoomButton,
    ]
    .into_iter()
    .enumerate()
    {
        if let Some(button) = window.standardWindowButton(kind)
            && let Some(parent) = (unsafe { button.superview() })
        {
            let y = if parent.isFlipped() {
                8.
            } else {
                parent.bounds().size.height - button.frame().size.height - 8.
            };
            button.setFrameOrigin(NSPoint::new(10. + index as f64 * 20., y));
        }
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
    if window.styleMask().contains(NSWindowStyleMask::FullScreen) {
        WindowMode::Fullscreen
    } else if window.isZoomed() {
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
fn opening_frame(mtm: MainThreadMarker, bounds: Option<WindowGeometry>, cascade: usize) -> NSRect {
    if let Some(mut b) = bounds {
        b.width = b.width.max(640.);
        b.height = b.height.max(400.);
        let screen = NSScreen::mainScreen(mtm)
            .map(|s| s.frame())
            .unwrap_or_else(|| visible_frame(mtm, None));
        return appkit_frame(b, screen);
    }
    let screen = visible_frame(mtm, None);
    let frame = WindowFrame::opening(
        WindowSize {
            width: 1220.,
            height: 780.,
        },
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
        RuntimeWindow::global(home, None, WindowMode::Windowed, Default::default())
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
        } else if let Some(project) = state.active_project() {
            project.clone()
        } else {
            store.add_project(std::env::current_dir().map_err(|e| e.to_string())?, None)?
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
        options,
        runtime,
        restore: false,
        notice: None,
    })
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
    if restore.is_some() && std::env::var_os("RIWORK_NSH_BUNDLE").is_none() {
        return Err(
            "unavailable: restoring layouts requires the NSH application bundle and P1a services"
                .into(),
        );
    }
    let (bundle, directory) = if let Some(path) = std::env::var_os("RIWORK_NSH_BUNDLE") {
        let path = PathBuf::from(path)
            .canonicalize()
            .map_err(|e| format!("NSH bundle: {e}"))?;
        (
            Bundle::read(&path).map_err(|e| format!("NSH bundle: {e}"))?,
            path.parent().map(Path::to_path_buf),
        )
    } else {
        (
            Bundle::source(include_str!("nsh_bootstrap.js").into()),
            None,
        )
    };
    let bundle = bundle
        .with_app_id(Some(APP_ID))
        .with_capabilities_json(Some(CAPABILITIES))?;
    let windows = if let Some(snapshot) = &restore {
        snapshot
            .windows
            .iter()
            .map(|runtime| PreparedWindow {
                options: if runtime.is_global() {
                    OpenOptions::global()
                } else {
                    OpenOptions::project(Some(runtime.path.clone()))
                },
                runtime: runtime.clone(),
                restore: true,
                notice: None,
            })
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
fn services(mut startup: Startup, rx: mpsc::Receiver<Service>, tx: mpsc::Sender<Service>) {
    startup.instance.set_request_waker(move || {
        let _ = tx.send(Service::Wake);
    });
    let mut snapshots = Vec::new();
    let mut initial = true;
    let mut confirmed = false;
    let mut publish_at = Instant::now();
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
                snapshots = windows.into_iter().map(|(_, w)| w).collect::<Vec<_>>();
            }
            Ok(Service::Ready(id)) => {
                match startup.runtime.mark_restore_ready(&id, snapshots.clone()) {
                    Ok(done) => confirmed |= done,
                    Err(e) => send(Main::Error(e)),
                }
            }
            Ok(Service::Open(options, call, route, forwarded)) => {
                match prepare(&startup.home, options) {
                    Ok(window) => send(Main::Open(window, call, route, forwarded)),
                    Err(error) => {
                        if let Some(call) = call {
                            call.reject(error);
                        } else {
                            eprintln!("riwork: {error}");
                        }
                    }
                }
            }
            Ok(Service::Quit) => {
                if let Err(e) = startup.runtime.publish(snapshots.clone()) {
                    eprintln!("riwork: {e}");
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
            Ok(Service::Finished) => break,
            Ok(Service::Wake) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        for request in startup.instance.drain_requests() {
            match prepare(&startup.home, OpenOptions::project(request.path)) {
                Ok(window) => send(Main::Open(window, None, None, true)),
                Err(e) => eprintln!("riwork: forwarded open failed: {e}"),
            }
        }
        if confirmed && let Err(e) = startup.instance.try_complete_handoff() {
            send(Main::Error(e));
        }
        if Instant::now() >= publish_at {
            publish_at = Instant::now() + Duration::from_secs(2);
            if let Err(e) = startup.runtime.try_publish(snapshots.clone()) {
                eprintln!("riwork: {e}");
            }
            // Do not launch a replacement before native persistence is wired.
            if let Ok(Some(request)) = startup.runtime.pending_reload() {
                let _ = startup.runtime.fail_reload(&request, RESTART_PENDING);
            }
        }
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
    let mut host = Host {
        mtm,
        app: app.clone(),
        home: startup.home.clone(),
        bundle: startup.bundle.clone(),
        asset_base: startup.directory.clone().map(AssetBase::Directory),
        chunks: startup.directory.clone(),
        components: Components::default(),
        menus,
        windows: BTreeMap::new(),
        service: tx.clone(),
        focus_serial: 0,
        quitting: false,
        background,
    };
    // Queue the complete initial snapshot before the worker starts, not one partial set per open.
    let windows = std::mem::take(&mut startup.windows);
    for window in windows {
        host.open(window)?;
    }
    while rx.try_recv().is_ok() {}
    host.publish();
    HOST.with(|state| *state.borrow_mut() = Some(host));
    let worker_tx = tx.clone();
    let worker = thread::spawn(move || services(startup, rx, worker_tx));
    app.finishLaunching();
    if !background {
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
    }
    app.run();
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
    fn params_follow_frozen_schema_and_omit_restore_on_new_windows() {
        let home = Path::new("/isolated");
        let window = PreparedWindow {
            options: OpenOptions::global(),
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
        let mut global = OpenOptions::global();
        global.project_id = Some("bad".into());
        assert!(global.validate().is_err());
    }
}
