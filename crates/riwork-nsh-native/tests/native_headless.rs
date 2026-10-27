//! Real QuickJS engines and native callbacks, with no platform host or windows.
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    time::{Duration, Instant},
};

use nsh_core::{ApproxTextEngine, MountBatch};
use nsh_engine::{Bundle, Engine, EngineConfig, EngineControl, HostChannel, MountSink};
use riwork_nsh_native::{
    ProcessService,
    backend::{CoreStoreBackend, SessionsBackend},
    dto::SessionsSnapshot,
    service::install_process_service,
};
use serde_json::{Value, json};

const DEADLINE: Duration = Duration::from_secs(10);

#[derive(Default)]
struct FakeSessions {
    reads: AtomicUsize,
}

impl SessionsBackend for FakeSessions {
    fn list(&self) -> anyhow::Result<SessionsSnapshot> {
        assert_ne!(std::thread::current().name(), Some("nsh-engine"));
        self.reads.fetch_add(1, Ordering::Relaxed);
        Ok(SessionsSnapshot::default())
    }
}

#[derive(Debug)]
struct Event {
    name: String,
    data: Value,
}

struct HeadlessSink {
    control: Arc<OnceLock<EngineControl>>,
    events: Sender<Event>,
    frames: Arc<AtomicUsize>,
}

impl MountSink for HeadlessSink {
    fn submit(&self, _batch: MountBatch) {}

    fn report_error(&self, message: &str) {
        let _ = self.events.send(Event {
            name: "error".into(),
            data: json!(message),
        });
    }

    fn request_frame(&self) {
        // Fulfill only a requested frame. There is no display timer or polling
        // loop: quiet subscriptions await their channel without asking again.
        self.frames.fetch_add(1, Ordering::Relaxed);
        self.control
            .get()
            .expect("setup precedes engine start")
            .frame();
    }
}

struct HeadlessApp {
    engine: Option<Engine>,
    host: HostChannel,
    events: Receiver<Event>,
    pending: VecDeque<Event>,
    frames: Arc<AtomicUsize>,
}

impl HeadlessApp {
    fn start() -> Self {
        let host = HostChannel::new();
        let (events, receive) = mpsc::channel();
        for name in [
            "store",
            "sessions",
            "snapshot",
            "list",
            "layout",
            "closed",
            "disposed",
            "probe",
            "error",
            "folderCreated",
            "preferences",
        ] {
            let events = events.clone();
            host.handle(name, move |mut call| {
                let _ = events.send(Event {
                    name: call.name().to_owned(),
                    data: call.take_data(),
                });
            });
        }
        let source = r#"
            import { mount, host } from "nsh";
            import { Store, Sessions, Layouts, Settings, Appearance } from "riwork/native";
            let storeSubscription, sessionsSubscription, layout, deliveries = 0;
            const listen = event => { deliveries++; host.post("store", event); };
            const dispose = mount(() => {
                storeSubscription = Store.subscribe(listen);
                sessionsSubscription = Sessions.subscribe(event => host.post("sessions", event));
                return null;
            });
            host.on("read", async () => {
                try {
                    host.post("snapshot", await Store.snapshot());
                    host.post("list", await Sessions.list());
                } catch (error) { host.post("error", String(error)); }
            });
            host.on("createFolder", async name => {
                try { host.post("folderCreated", await Store.createFolder(name)); }
                catch (error) { host.post("error", String(error)); }
            });
            host.on("preferences", async () => {
                try {
                    const saved = await Settings.set({theme: "native"});
                    const bigger = await Settings.changeTextSize("bigger");
                    const appearance = await Appearance.resolve("native", true);
                    let unknown = "accepted";
                    try { await Settings.set({futureSetting: true}); }
                    catch (error) { unknown = String(error); }
                    host.post("preferences", {saved, bigger, appearance, unknown});
                } catch (error) { host.post("error", String(error)); }
            });
            host.on("layout", async request => {
                try {
                    layout = await Layouts.openSession(request);
                    if (request.edit !== false) {
                        layout.toggleLock(layout.snapshot().activePane);
                        await layout.flush();
                    }
                    host.post("layout", layout.snapshot());
                } catch (error) { host.post("error", String(error)); }
            });
            host.on("close", () => {
                storeSubscription.close();
                storeSubscription.close();
                sessionsSubscription.close();
                host.post("closed", deliveries);
            });
            host.on("dispose", () => {
                dispose();
                // Retain both handles: cancellation must come from the owner,
                // even though their JS objects have not been collected.
                host.post("disposed", deliveries);
            });
            host.on("resubscribe", () => {
                storeSubscription = Store.subscribe(listen);
            });
            host.on("probe", () => host.post("probe", deliveries));
        "#;
        let bundle = Bundle::source(source.to_owned()).with_capabilities_json(Some(
            r#"{"host":["store","sessions","snapshot","list","layout","closed","disposed","probe","error","folderCreated","preferences"]}"#,
        )).unwrap();
        let mut config = EngineConfig::new(bundle);
        config.ui_sets = vec![nsh_base::registry(), riwork_nsh_native::registry()];
        config.host = host.clone();
        // EngineConfig's defaults keep storage and caches entirely in memory.
        let control = Arc::new(OnceLock::new());
        let setup_control = control.clone();
        let frames = Arc::new(AtomicUsize::new(0));
        let sink = HeadlessSink {
            control,
            events,
            frames: frames.clone(),
        };
        let engine = Engine::start_with(
            config,
            || ApproxTextEngine,
            sink,
            move |control| {
                assert!(setup_control.set(control.clone()).is_ok());
            },
        )
        .unwrap();
        Self {
            engine: Some(engine),
            host,
            events: receive,
            pending: VecDeque::new(),
            frames,
        }
    }

    fn send(&self, name: &str) {
        self.host.send(name, Value::Null).unwrap();
    }

    fn receive(&mut self, name: &str, predicate: impl Fn(&Value) -> bool) -> Value {
        if let Some(index) = self
            .pending
            .iter()
            .position(|event| event.name == name && predicate(&event.data))
        {
            return self.pending.remove(index).unwrap().data;
        }
        let until = Instant::now() + DEADLINE;
        loop {
            let event = self
                .events
                .recv_timeout(until.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|error| {
                    panic!("waiting for {name}: {error}; pending {:?}", self.pending)
                });
            assert_ne!(event.name, "error", "headless engine: {}", event.data);
            if event.name == name && predicate(&event.data) {
                return event.data;
            }
            self.pending.push_back(event);
        }
    }

    fn assert_no_store_delivery(&mut self) {
        assert!(!self.pending.iter().any(|event| event.name == "store"));
        match self.events.recv_timeout(Duration::from_millis(100)) {
            Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => {}
            Ok(event) => panic!("unexpected event after subscription teardown: {event:?}"),
        }
    }

    fn stop(&mut self) {
        drop(self.engine.take());
    }
}

fn has_folder(snapshot: &Value, name: &str) -> bool {
    snapshot["projectFolders"]
        .as_array()
        .unwrap()
        .iter()
        .any(|folder| folder["name"] == name)
}

#[test]
fn native_pushes_one_service_to_two_engines_and_stops_on_close_owner_and_engine_drop() {
    // The verification command supplies a disposable RIWORK_HOME. Each test
    // still gets its own child, without changing the process environment.
    let fixture_parent = std::env::var_os("RIWORK_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&fixture_parent).unwrap();
    let fixture = tempfile::Builder::new()
        .prefix("native-headless-")
        .tempdir_in(fixture_parent)
        .unwrap();
    let home = fixture.path().canonicalize().unwrap();
    let sessions = Arc::new(FakeSessions::default());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let service = runtime.block_on({
        let home = home.clone();
        let sessions = sessions.clone();
        async move {
            tokio::task::spawn_blocking(move || {
                ProcessService::with_backends(
                    home.clone(),
                    Arc::new(CoreStoreBackend::new(home)),
                    sessions,
                )
            })
            .await
            .unwrap()
            .unwrap()
        }
    });
    install_process_service(service.clone()).unwrap();
    assert!(Arc::ptr_eq(
        &service,
        &runtime.block_on(riwork_nsh_native::initialize()).unwrap()
    ));
    let mut first = HeadlessApp::start();
    let mut second = HeadlessApp::start();
    let initial = first.receive("store", |_| true);
    assert_eq!(initial, second.receive("store", |_| true));
    assert_eq!(initial["kind"], "snapshot");
    assert_eq!(initial["rev"], 1);
    assert_eq!(initial["data"]["projectFolders"], json!([]));
    for app in [&mut first, &mut second] {
        assert_eq!(
            app.receive("sessions", |_| true)["data"],
            json!({"sessions": []})
        );
        app.send("read");
        assert_eq!(app.receive("snapshot", |_| true), initial["data"]);
        assert_eq!(app.receive("list", |_| true), json!({"sessions": []}));
        app.send("probe");
        assert_eq!(app.receive("probe", |_| true), json!(1));
        let frames = app.frames.load(Ordering::Relaxed);
        app.assert_no_store_delivery();
        assert_eq!(
            app.frames.load(Ordering::Relaxed),
            frames,
            "an idle subscription must not request another frame"
        );
    }

    first.send("preferences");
    let preferences = first.receive("preferences", |_| true);
    assert_eq!(preferences["saved"]["theme"], "native");
    assert_eq!(preferences["bigger"]["text"]["points"], 14.0);
    assert_eq!(preferences["appearance"]["palette"]["bg"], "#000000");
    assert!(
        preferences["unknown"]
            .as_str()
            .unwrap()
            .contains("invalid:")
    );

    // The same native import exposes the P1c classes as well as P1a services.
    // No RIWORK_HOME mutation is needed: layouts must use the service's injected
    // fixture home, even though each AppView owns its own LayoutSession.
    for (app, id) in [
        (&mut first, "combined-registry-first"),
        (&mut second, "combined-registry-second"),
    ] {
        app.host
            .send("layout", json!({"windowId": id, "projectId": id}))
            .unwrap();
        let visible = app.receive("layout", |_| true);
        let host = riwork_nsh_workspace::host_layouts::lookup(id).unwrap();
        let project = host.snapshot();
        assert_eq!(visible["activePane"], project.active_pane);
        assert_eq!(
            riwork_core::layouts::LayoutStore::open(&home)
                .unwrap()
                .load(id)
                .unwrap(),
            Some(project),
            "the sole registry must persist layouts in the process service's home"
        );
    }

    // A reload payload wins over disk and must enter the host's flush barrier
    // on successful open, before any UI mutation or JS flush call.
    let restored_id = "combined-registry-restored";
    let layouts = riwork_core::layouts::LayoutStore::open(&home).unwrap();
    let mut restored = riwork_nsh_workspace::persistence::default_project_layout();
    layouts
        .save_chat_state(restored_id, &restored, &[], None)
        .unwrap();
    restored.selected_task_id = Some("restored-task-without-an-edit".into());
    let mut restored_app = HeadlessApp::start();
    restored_app
        .host
        .send(
            "layout",
            json!({"windowId": restored_id, "projectId": restored_id,
                "restore": restored, "edit": false}),
        )
        .unwrap();
    restored_app.receive("layout", |_| true);
    let restored_host = riwork_nsh_workspace::host_layouts::lookup(restored_id).unwrap();
    assert_eq!(restored_host.snapshot(), restored);
    restored_host.flush_blocking().unwrap();
    assert_eq!(layouts.load(restored_id).unwrap(), Some(restored));
    drop(restored_host);
    restored_app.stop();
    assert!(riwork_nsh_workspace::host_layouts::lookup(restored_id).is_none());

    let disk = riwork_core::store::Store::open(home).unwrap();
    first
        .host
        .send("createFolder", json!("Delivered to both engines"))
        .unwrap();
    assert_eq!(
        first.receive("folderCreated", |_| true)["name"],
        "Delivered to both engines"
    );
    let changed = first.receive("store", |event| {
        has_folder(&event["data"], "Delivered to both engines")
    });
    assert_eq!(
        changed,
        second.receive("store", |event| has_folder(
            &event["data"],
            "Delivered to both engines"
        ))
    );
    assert!(changed["rev"].as_u64().unwrap() > initial["rev"].as_u64().unwrap());
    assert!(changed["data"].get("schema_version").is_none());
    assert!(sessions.reads.load(Ordering::Relaxed) >= 3);

    first.send("close");
    let first_count = first.receive("closed", |_| true);
    second.send("dispose");
    let second_count = second.receive("disposed", |_| true);
    let observer = runtime.block_on(service.subscribe_store()).unwrap();
    runtime.block_on(observer.recv()).unwrap();
    disk.create_project_folder("After close and owner disposal")
        .unwrap();
    runtime.block_on(async {
        loop {
            let snapshot = tokio::time::timeout(DEADLINE, observer.recv())
                .await
                .unwrap()
                .unwrap();
            if snapshot
                .data
                .project_folders
                .iter()
                .any(|folder| folder.name == "After close and owner disposal")
            {
                break;
            }
        }
    });
    for (app, count) in [(&mut first, first_count), (&mut second, second_count)] {
        app.send("probe");
        assert_eq!(app.receive("probe", |_| true), count);
        app.assert_no_store_delivery();
    }

    // An active subscription outside a mounted owner is owned by the VM.
    // Stopping its engine must release it while the process service lives on.
    second.send("resubscribe");
    second.receive("store", |event| {
        has_folder(&event["data"], "After close and owner disposal")
    });
    second.stop();
    first.stop();
    assert!(riwork_nsh_workspace::host_layouts::lookup("combined-registry-first").is_none());
    assert!(riwork_nsh_workspace::host_layouts::lookup("combined-registry-second").is_none());
    disk.create_project_folder("After engine shutdown").unwrap();
    runtime.block_on(async {
        loop {
            let snapshot = tokio::time::timeout(DEADLINE, observer.recv())
                .await
                .unwrap()
                .unwrap();
            if snapshot
                .data
                .project_folders
                .iter()
                .any(|folder| folder.name == "After engine shutdown")
            {
                break;
            }
        }
    });
    second.assert_no_store_delivery();
    assert!(first.frames.load(Ordering::Relaxed) > 0);
    assert!(second.frames.load(Ordering::Relaxed) > 0);
    assert_eq!(service.last_error(), None);
}
