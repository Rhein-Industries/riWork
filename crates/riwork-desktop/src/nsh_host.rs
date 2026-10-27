//! AppKit owns windows; every window owns an independent NSH engine/channel.
//! Disk, runtime registry and project resolution live on the service thread.
use crate::{
    nsh_chrome::{create_window, position_controls},
    nsh_menus::{DockWindow, HostMenus, MenuAction},
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
    global_window::GlobalWindowStore,
    layouts::{WindowFrame, WindowSize},
    paths,
    store::Store,
};
use riwork_engine::runtime::{RuntimeWindow, WindowGeometry, WindowKind, WindowMode};
use riwork_nsh_native as native;
use riwork_nsh_workspace::host_layouts::{self, HostLayout};
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
    "unavailable: NSH restart handoff is not enabled in the Phase 1 integration subset";

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
    Close(String, ClosingWindows, Option<HostCall>),
    Quit(ClosingWindows),
    Finished,
    Wake,
}
enum Main {
    Call(String, &'static str, HostCall),
    Open(PreparedWindow, Option<HostCall>, Option<Value>, bool),
    State(String),
    Close(String),
    Closed(String, Option<HostCall>, Result<(), String>),
    QuitFailed(String),
    Menu(MenuAction),
    Quit,
    Finish,
    DrainTerminals,
    #[cfg(feature = "terminal-fixture")]
    Fixture(Value),
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
    background: bool,
    forced_appearance: Option<Appearance>,
}
impl Host {
    fn open(&mut self, prepared: PreparedWindow) -> Result<String, String> {
        let id = Uuid::new_v4().to_string();
        let channel = HostChannel::new();
        let appearance = self.forced_appearance.unwrap_or(Appearance::System);
        let mut params = prepared.params(&id, self.windows.len());
        params["appearance"] = json!(appearance_name(appearance));
        channel.set_params(params.as_object().unwrap().clone());
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
            "system.openWith",
            "system.revealInFinder",
            "system.fonts",
        ] {
            let id = id.clone();
            channel.handle(name, move |call| send(Main::Call(id.clone(), name, call)));
        }
        // All other v1 operations fail explicitly until their owning feature lands.
        for name in [
            "system.quickLook",
            "system.openPanel",
            "system.savePanel",
            "system.readPasteboard",
            "system.notify",
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
        let frame = opening_frame(self.mtm, prepared.runtime.bounds, self.windows.len());
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
        for window in self.windows.values() {
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
                let mut runtime = window.runtime.clone();
                let screen = window
                    .window
                    .screen()
                    .or_else(|| NSScreen::mainScreen(self.mtm))
                    .map(|screen| screen.frame())
                    .unwrap_or(window.normal);
                runtime.bounds = Some(runtime_geometry(window.normal, screen));
                runtime.mode = mode(&window.window);
                apply_current_layout(id, &mut runtime);
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
        if self.quitting || window.closing {
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
                batch.cancel();
                self.windows.get_mut(id).unwrap().closing = false;
                if let Some(call) = call {
                    call.reject("unavailable: host service stopped");
                }
            }
        }
    }
    fn quit(&mut self) {
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
            if let Service::Quit(batch) = error.0 {
                batch.cancel();
            }
            self.quitting = false;
            eprintln!("riwork: unavailable: host service stopped");
        }
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
                        .filter(|(id, w)| {
                            current_project_id(id, &w.runtime) == prepared.runtime.project_id
                        })
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
                        if global {
                            if let Some(state) = self
                                .windows
                                .get_mut(&id)
                                .and_then(|window| window.runtime.global.as_mut())
                            {
                                state.view = riwork_core::global_window::GlobalView::from(
                                    show["view"].as_str().unwrap_or("dashboard").to_owned(),
                                );
                            }
                            // Every project status menu follows the shown view;
                            // only the global window receives the start action.
                            self.publish_global_selection();
                            self.publish();
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
            Main::Close(id) => self.close(&id, "user", None),
            Main::Closed(id, call, result) => match result {
                Ok(()) => {
                    if let Some(call) = call {
                        call.resolve(json!({}));
                    }
                    self.teardown(&id);
                    self.publish();
                    if self.windows.is_empty() || self.quit_pending {
                        self.quit();
                    }
                }
                Err(error) => {
                    if let Some(window) = self.windows.get_mut(&id) {
                        window.closing = false;
                    }
                    self.quit_pending = false;
                    if let Some(call) = call {
                        call.reject(error);
                    } else {
                        eprintln!("riwork: close cancelled: {error}");
                    }
                }
            },
            Main::QuitFailed(error) => {
                self.quitting = false;
                eprintln!("riwork: quit cancelled: {error}");
            }
            Main::Menu(action) => self.menu(action),
            Main::Quit => self.quit(),
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
                        ("command", json!({"id":request["id"]}))
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
                    .iter()
                    .filter(|(_, w)| !w.runtime.is_global())
                    .max_by_key(|(_, w)| w.focus_serial)
                    .and_then(|(id, w)| current_project_id(id, &w.runtime));
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
                "windowId":id,"kind":w.runtime.kind,"projectId":current_project_id(id, &w.runtime),
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
                    Some(target) if self.windows.contains_key(target) => { let target = target.to_owned(); self.close(&target,"user",Some(call)); }
                    _ => call.reject("not_found: window"),
                }
            }
            "window.setDockInfo" => { self.windows.get_mut(&id).unwrap().dock_info = data; call.resolve(json!({})); }
            "app.restoreReady" => {
                if !self.windows[&id].runtime.is_global() && host_layouts::lookup(&id).is_none() {
                    call.reject("unavailable: project layout is not ready");
                } else {
                    self.publish();
                    let _ = self.service.send(Service::Ready(id));
                    call.resolve(json!({}));
                }
            }
            "app.quit" => { call.resolve(json!({})); self.quit(); }
            "app.restart" => call.reject(RESTART_PENDING),
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
    // Initialization and bundle IO stay on this startup worker, before any AppView.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("Cannot initialize native runtime: {error}"))?;
    runtime.block_on(native::initialize())?;
    let (bundle, directory) = crate::nsh_bundle::load()?;
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
            }
            if let Some(global) = &window.global {
                if let Err(error) = GlobalWindowStore::at(home).save(global) {
                    errors.push(format!("global window {id}: {error}"));
                }
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
        for (id, _) in &self.windows {
            host_layouts::cancel_window_close(id);
        }
    }
}

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
                match startup
                    .runtime
                    .mark_restore_ready(&id, current_layouts(&startup.home, &snapshots))
                {
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
            Ok(Service::Close(id, batch, call)) => {
                let result = batch.flush(&startup.home).map(|_| ());
                if result.is_err() {
                    batch.cancel();
                }
                send(Main::Closed(id, call, result));
            }
            Ok(Service::Quit(batch)) => {
                let result = batch
                    .flush(&startup.home)
                    .and_then(|windows| startup.runtime.publish(windows));
                if let Err(error) = result {
                    batch.cancel();
                    send(Main::QuitFailed(error));
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
        let now = Instant::now();
        if runtime_tick.is_due(now) {
            match startup
                .runtime
                .try_publish(current_layouts(&startup.home, &snapshots))
            {
                Ok(completed) => runtime_tick.published(now, completed),
                Err(e) => eprintln!("riwork: {e}"),
            }
            // Restart handoff remains outside this integration subset.
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
        background,
        forced_appearance,
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
    struct LayoutFixture {
        root: PathBuf,
    }
    impl LayoutFixture {
        fn new() -> Self {
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
        fn window(&self, id: &str) -> (String, RuntimeWindow) {
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
        let mut global = riwork_core::global_window::GlobalWindowState::default();
        global.window_size = Some(WindowSize {
            width: 900.,
            height: 650.,
        });
        windows.push((
            "global".into(),
            RuntimeWindow::global(&fixture.root, None, WindowMode::Windowed, global.clone()),
        ));
        let closing = ClosingWindows::new(windows);
        let published = closing.flush(&fixture.root).unwrap();
        assert_eq!(published.len(), 3);
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
