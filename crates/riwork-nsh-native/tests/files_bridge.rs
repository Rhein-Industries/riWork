//! Actual QuickJS Files callbacks and owner cancellation, with filesystem
//! mutations restricted to a fresh temporary repository; no native windows.
use nsh_core::{ApproxTextEngine, MountBatch};
use nsh_engine::{Bundle, Engine, EngineConfig, EngineControl, HostChannel, MountSink};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    time::{Duration, Instant},
};

struct Sink {
    control: Arc<OnceLock<EngineControl>>,
    tx: Sender<(String, Value)>,
    frames: Arc<AtomicUsize>,
}
impl MountSink for Sink {
    fn submit(&self, _: MountBatch) {}
    fn report_error(&self, message: &str) {
        let _ = self.tx.send(("error".into(), json!(message)));
    }
    fn request_frame(&self) {
        self.frames.fetch_add(1, Ordering::Relaxed);
        self.control.get().unwrap().frame();
    }
}
struct App {
    _engine: Engine,
    host: HostChannel,
    events: Receiver<(String, Value)>,
    pending: VecDeque<(String, Value)>,
    frames: Arc<AtomicUsize>,
}
impl App {
    fn start() -> Self {
        let host = HostChannel::new();
        let (tx, events) = mpsc::channel();
        for name in [
            "ready", "files", "closed", "disposed", "probe", "edited", "error",
        ] {
            let tx = tx.clone();
            host.handle(name, move |mut call| {
                let _ = tx.send((call.name().to_owned(), call.take_data()));
            });
        }
        let source = r#"
            import { mount, cleanup, host } from 'nsh';
            import { Files, Sessions, Layouts } from 'riwork/native';
            let subscription, dispose, deliveries = 0;
            host.on('start', root => {
                dispose = mount(() => {
                    subscription = Files.subscribe(root, root, false, event => {
                        deliveries++; host.post('files', event);
                    });
                    cleanup(() => subscription.close());
                    return null;
                });
            });
            host.on('close', () => { subscription.close(); subscription.close(); host.post('closed', deliveries); });
            host.on('dispose', () => { dispose(); host.post('disposed', deliveries); });
            host.on('probe', () => host.post('probe', deliveries));
            host.on('editSelected', async request => {
                try {
                    const layout = await Layouts.openSession({ windowId: 'files-editor-proof', projectId: request.projectId, restore: request.restore });
                    const before = layout.snapshot();
                    const listing = await Files.listDirectory(request.root, request.root, false);
                    const selected = listing.entries.find(entry => entry.name === request.name);
                    const editor = await Sessions.openEditor({ projectId: request.projectId, path: selected.path, identity: selected.identity, line: 2 });
                    const dock = layout.openEditorShell(editor.id);
                    // Reusing the same returned editor identity selects its tab
                    // without inserting a second shell tab.
                    layout.openEditorShell(editor.id);
                    await layout.refreshTabs();
                    await layout.flush();
                    host.post('edited', { selected, editor, before, dock, after: layout.snapshot() });
                } catch (error) { host.post('error', String(error)); }
            });
            host.post('ready', true);
        "#;
        let bundle = Bundle::source(source.into())
            .with_capabilities_json(Some(
                r#"{"host":["ready","files","closed","disposed","probe","edited","error"]}"#,
            ))
            .unwrap();
        let mut config = EngineConfig::new(bundle);
        config.ui_sets = vec![nsh_base::registry(), riwork_nsh_native::registry()];
        config.host = host.clone();
        let control = Arc::new(OnceLock::new());
        let setup = control.clone();
        let frames = Arc::new(AtomicUsize::new(0));
        let engine = Engine::start_with(
            config,
            || ApproxTextEngine,
            Sink {
                control,
                tx,
                frames: frames.clone(),
            },
            move |control| {
                assert!(setup.set(control.clone()).is_ok());
            },
        )
        .unwrap();
        let mut app = Self {
            _engine: engine,
            host,
            events,
            pending: VecDeque::new(),
            frames,
        };
        app.recv("ready", |_| true);
        app
    }
    fn send(&self, name: &str, data: Value) {
        self.host.send(name, data).unwrap();
    }
    fn recv(&mut self, name: &str, matches: impl Fn(&Value) -> bool) -> Value {
        if let Some(index) = self
            .pending
            .iter()
            .position(|(key, value)| key == name && matches(value))
        {
            return self.pending.remove(index).unwrap().1;
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let (key, value) = self
                .events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|error| {
                    panic!("waiting {name}: {error}; pending {:?}", self.pending)
                });
            assert_ne!(key, "error", "headless engine: {value}");
            if key == name && matches(&value) {
                return value;
            }
            self.pending.push_back((key, value));
        }
    }
}
fn contains(event: &Value, name: &str) -> bool {
    event["data"]["entries"]
        .as_array()
        .is_some_and(|entries| entries.iter().any(|entry| entry["name"] == name))
}

#[test]
fn files_subscriptions_cross_two_engines_publish_without_polling_and_cancel_by_owner() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::write(root.join("initial 日本語.txt"), "initial").unwrap();
    let mut first = App::start();
    let mut second = App::start();
    first.send("start", json!(root));
    second.send("start", json!(root));
    let a = first.recv("files", |event| contains(event, "initial 日本語.txt"));
    let b = second.recv("files", |event| contains(event, "initial 日本語.txt"));
    assert_eq!(a, b);
    assert_eq!(a["data"]["schema"], 3);
    std::fs::write(root.join("new spaced 🦀.md"), "# new\n").unwrap();
    let a = first.recv("files", |event| contains(event, "new spaced 🦀.md"));
    let b = second.recv("files", |event| contains(event, "new spaced 🦀.md"));
    assert_eq!(a, b);
    assert!(a["rev"].as_u64().unwrap() > 1);
    // Delivery posts to the host immediately before requesting its one frame
    // gate. Let that in-flight gate settle, then measure an idle interval.
    std::thread::sleep(Duration::from_millis(100));
    let frames = first.frames.load(Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        first.frames.load(Ordering::Relaxed),
        frames,
        "quiet Files must not poll frames"
    );
    first.send("dispose", Value::Null);
    let deliveries = first.recv("disposed", |_| true);
    std::fs::write(root.join("after disposal"), "new").unwrap();
    second.recv("files", |event| contains(event, "after disposal"));
    first.send("probe", Value::Null);
    assert_eq!(
        first.recv("probe", |_| true),
        deliveries,
        "disposed owner retained handle but received a Files update"
    );
    second.send("close", Value::Null);
    let deliveries = second.recv("closed", |_| true);
    std::fs::write(root.join("after close"), "new").unwrap();
    std::thread::sleep(Duration::from_millis(150));
    second.send("probe", Value::Null);
    assert_eq!(second.recv("probe", |_| true), deliveries);
}

#[test]
fn failed_watch_setup_delivers_an_explicit_error_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().canonicalize().unwrap().join("missing");
    let mut app = App::start();
    app.send("start", json!(missing));
    let error = app.recv("files", |event| event["data"]["error"].is_string());
    assert_eq!(error["kind"], "snapshot");
    assert_eq!(error["data"]["entries"], json!([]));
}

#[test]
fn listed_selection_reaches_editor_factory_and_reuses_its_main_pane_tab_without_tmux() {
    const CHILD: &str = "RIWORK_FILES_EDITOR_BRIDGE_CHILD";
    if std::env::var_os(CHILD).is_none() {
        // Process globals (the fixed service home and layout fixture catalog)
        // remain local to this one proof, including when the suite is parallel.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "listed_selection_reaches_editor_factory_and_reuses_its_main_pane_tab_without_tmux",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "child proof failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    use riwork_nsh_native::{
        ProcessService,
        backend::{CoreStoreBackend, SessionsBackend},
        dto::{OpenEditorRequest, SessionsSnapshot, ShellSessionDto},
        service::install_process_service,
    };
    use std::{path::PathBuf, sync::Mutex};
    struct EditorFactory {
        home: PathBuf,
        root: PathBuf,
        requests: Mutex<Vec<OpenEditorRequest>>,
        sessions: Mutex<SessionsSnapshot>,
    }
    impl SessionsBackend for EditorFactory {
        fn list(&self) -> anyhow::Result<SessionsSnapshot> {
            Ok(self.sessions.lock().unwrap().clone())
        }
        fn open_editor(&self, request: OpenEditorRequest) -> anyhow::Result<ShellSessionDto> {
            assert_ne!(std::thread::current().name(), Some("nsh-engine"));
            let identity = serde_json::from_str(
                request
                    .identity
                    .as_deref()
                    .expect("selected row identity reached factory"),
            )?;
            let path = riwork_engine::file_preview::validated_editor_path(
                &self.root,
                std::path::Path::new(&request.path),
                identity,
            )
            .map_err(anyhow::Error::msg)?;
            let shell: riwork_engine::sessions::ShellSession = serde_json::from_value(json!({
                "id": "00000000-0000-4000-8000-000000000085", "project_id": request.project_id,
                "kind": "project", "cwd": self.root, "editor_path": path, "user_opened": true,
                "alive": true, "created_at_unix": 1,
            }))?;
            let shell: ShellSessionDto = shell.into();
            std::fs::write(
                self.home.join("fake-sessions.json"),
                serde_json::to_vec(&json!({"sessions":[{
                    "id": shell.id, "project_id": request.project_id, "kind":"project", "cwd": self.root,
                    "alive": true, "user_opened": true,
                }]}))?,
            )?;
            self.requests.lock().unwrap().push(request);
            self.sessions.lock().unwrap().sessions = vec![shell.clone()];
            Ok(shell)
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let home = base.join("home");
    let root = base.join("nested spaced 日本語 repo");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("selected 🦀.txt"), "line 1\nline 2\n").unwrap();
    let project = riwork_core::store::Store::open(&home)
        .unwrap()
        .add_project(&root, Some("Files editor proof"))
        .unwrap();
    std::fs::write(home.join("fake-sessions.json"), b"{\"sessions\":[]}").unwrap();
    riwork_nsh_workspace::session_io::register_fixture(&home).unwrap();
    let factory = Arc::new(EditorFactory {
        home: home.clone(),
        root: root.clone(),
        requests: Mutex::new(Vec::new()),
        sessions: Mutex::new(SessionsSnapshot::default()),
    });
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let service = runtime.block_on({
        let home = home.clone();
        let factory = factory.clone();
        async move {
            tokio::task::spawn_blocking(move || {
                ProcessService::with_backends(
                    home.clone(),
                    Arc::new(CoreStoreBackend::new(home)),
                    factory,
                )
            })
            .await
            .unwrap()
            .unwrap()
        }
    });
    install_process_service(service).unwrap();
    let mut restore = riwork_nsh_workspace::persistence::default_project_layout();
    restore.active_pane = 1;
    let mut app = App::start();
    app.send(
        "editSelected",
        json!({"root":root,"projectId":project.id,"name":"selected 🦀.txt","restore":restore}),
    );
    let edited = app.recv("edited", |_| true);
    let requests = factory.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, edited["selected"]["path"]);
    assert_eq!(
        requests[0].identity.as_deref(),
        edited["selected"]["identity"].as_str()
    );
    assert_eq!(requests[0].line, Some(2));
    let id = edited["editor"]["id"].as_str().unwrap();
    let panes = edited["after"]["panes"].as_array().unwrap();
    let main = panes.iter().find(|pane| pane["main"] == true).unwrap();
    assert_eq!(edited["after"]["activePane"], main["id"]);
    let key = format!("shell:{id}");
    assert!(
        main["tabs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tab| tab["key"] == key)
    );
    assert_eq!(
        panes
            .iter()
            .flat_map(|pane| pane["tabs"].as_array().unwrap())
            .filter(|tab| tab["key"] == key)
            .count(),
        1
    );
    assert_eq!(edited["before"]["activePane"], 1);
    assert_eq!(
        panes.len(),
        edited["before"]["panes"].as_array().unwrap().len(),
        "editor routing should reuse the main pane"
    );
}
