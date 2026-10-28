//! Production App + native Layouts + real close gates; no AppKit/desktop process.
use super::*;
use nsh_core::{ApproxTextEngine, MountBatch};
use nsh_engine::{Bundle, Engine, EngineConfig, EngineControl, HostChannel, MountSink};
use riwork_nsh_workspace::{LayoutRepository, OpenedLayout, persistence::LayoutBackend};
use std::collections::HashMap;
use std::sync::{
    Condvar, Mutex, OnceLock,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

const WAIT: Duration = Duration::from_secs(10);

#[derive(Default)]
struct Gate(Mutex<bool>, Condvar);
impl Gate {
    fn release(&self) {
        *self.0.lock().unwrap() = true;
        self.1.notify_all();
    }
    fn wait(&self) {
        let (ready, timeout) = self
            .1
            .wait_timeout_while(self.0.lock().unwrap(), WAIT, |ready| !*ready)
            .unwrap();
        assert!(*ready && !timeout.timed_out(), "fixture gate timed out");
    }
}
#[derive(Default)]
struct Attempt {
    calls: AtomicUsize,
    writers: AtomicUsize,
    writes: AtomicUsize,
    entered: Gate,
    release_open: Gate,
    rejected: Gate,
    release_rejection: Gate,
}
struct Writer {
    attempt: Arc<Attempt>,
    first: bool,
}
impl LayoutBackend for Writer {
    fn schedule(&self, _: riwork_core::layouts::ProjectLayout) -> Result<(), String> {
        self.attempt.writes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn flush(&self) -> Result<(), String> {
        Ok(())
    }
}
impl Drop for Writer {
    fn drop(&mut self) {
        if self.first {
            // registration_backend has already rejected and dropped this writer;
            // hold the error here to exercise cancellation-before-JS-rejection.
            self.attempt.rejected.release();
            self.attempt.release_rejection.wait();
        }
        self.attempt.writers.fetch_sub(1, Ordering::SeqCst);
    }
}
#[derive(Default)]
struct Repository(Mutex<HashMap<String, Arc<Attempt>>>);
impl LayoutRepository for Repository {
    fn open(
        &self,
        _: PathBuf,
        id: String,
        restore: Option<riwork_core::layouts::ProjectLayout>,
    ) -> Result<OpenedLayout, String> {
        let attempt = self
            .0
            .lock()
            .unwrap()
            .get(&id)
            .expect("registered fixture")
            .clone();
        let first = attempt.calls.fetch_add(1, Ordering::SeqCst) == 0;
        if first {
            attempt.entered.release();
            attempt.release_open.wait();
        }
        attempt.writers.fetch_add(1, Ordering::SeqCst);
        Ok(OpenedLayout {
            backend: Arc::new(Writer { attempt, first }),
            project: restore.unwrap(),
            notice: None,
            unknown_panes: Default::default(),
        })
    }
}
struct OtherWriter {
    fail: AtomicBool,
}
impl LayoutBackend for OtherWriter {
    fn schedule(&self, _: riwork_core::layouts::ProjectLayout) -> Result<(), String> {
        Ok(())
    }
    fn flush(&self) -> Result<(), String> {
        if self.fail.load(Ordering::SeqCst) {
            Err("fixture: another window failed to flush".into())
        } else {
            Ok(())
        }
    }
}
#[derive(Default)]
struct Sessions;
impl native::backend::SessionsBackend for Sessions {
    fn list(&self) -> anyhow::Result<native::dto::SessionsSnapshot> {
        Ok(Default::default())
    }
}

struct Sink {
    control: Arc<OnceLock<EngineControl>>,
    events: mpsc::Sender<String>,
}
impl MountSink for Sink {
    fn submit(&self, batch: MountBatch) {
        // Observe the real App's rendered error, not an imitation of its promise.
        if format!("{batch:?}").contains("unavailable: window is closing") {
            let _ = self.events.send("closing-error".into());
        }
    }
    fn report_error(&self, message: &str) {
        let _ = self.events.send(format!("engine-error: {message}"));
    }
    fn request_frame(&self) {
        self.control.get().unwrap().frame();
    }
}
struct AppProbe {
    engine: Option<Engine>,
    host: HostChannel,
    events: mpsc::Receiver<String>,
    ready: Arc<AtomicUsize>,
}
impl AppProbe {
    fn start(bundle: &Path, id: &str, layout: &riwork_core::layouts::ProjectLayout) -> Self {
        let host = HostChannel::new();
        host.set_params(json!({"schema":1,"windowId":id,"projectId":id,"kind":"project","path":"/fixture/Recovery","restore":{"layout":layout}}).as_object().unwrap().clone());
        let (events, receive) = mpsc::channel();
        let ready = Arc::new(AtomicUsize::new(0));
        for name in ["app.restoreReady", "window.setDockInfo", "app.fixture.pong"] {
            let events = events.clone();
            let ready = ready.clone();
            host.handle(name, move |call| {
                if name == "app.restoreReady" {
                    ready.fetch_add(1, Ordering::SeqCst);
                }
                events.send(name.into()).unwrap();
                call.resolve(json!({}));
            });
        }
        host.handle("system.fonts", |call| call.resolve(json!({"families":[]})));
        let mut config = EngineConfig::new(Bundle::read(bundle).unwrap());
        config.ui_sets = vec![nsh_base::registry(), native::registry()];
        config.host = host.clone();
        let control = Arc::new(OnceLock::new());
        let setup = control.clone();
        let engine = Engine::start_with(
            config,
            || ApproxTextEngine,
            Sink { control, events },
            move |value| {
                assert!(setup.set(value.clone()).is_ok());
            },
        )
        .unwrap();
        Self {
            engine: Some(engine),
            host,
            events: receive,
            ready,
        }
    }
    fn wait(&self, name: &str) {
        let deadline = Instant::now() + WAIT;
        loop {
            let event = self
                .events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|error| panic!("waiting for {name}: {error}"));
            assert!(!event.starts_with("engine-error:"), "{event}");
            if event == name {
                return;
            }
        }
    }
    fn ping(&self) {
        self.host.send("app.fixture.ping", json!({})).unwrap();
        self.wait("app.fixture.pong");
    }
    fn stop(&mut self) {
        drop(self.engine.take());
    }
}
fn eventually(message: &str, predicate: impl Fn() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !predicate() {
        assert!(Instant::now() < deadline, "{message}");
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn window(id: &str) -> (String, RuntimeWindow) {
    (
        id.into(),
        RuntimeWindow {
            project_id: Some(id.into()),
            path: "/fixture/Recovery".into(),
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

#[test]
#[ignore = "requires the production App fixture bundle; run verification/phase1_pending_open.py"]
fn production_app_recovers_pending_open_after_failed_quit_and_restart() {
    let bundle =
        PathBuf::from(std::env::var_os("RIWORK_PENDING_OPEN_TEST_BUNDLE").expect("fixture bundle"));
    let home = PathBuf::from(std::env::var_os("RIWORK_HOME").expect("isolated fixture home"));
    assert!(
        home.starts_with(std::env::temp_dir().canonicalize().unwrap()) || home.starts_with("/tmp")
    );
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _entered = runtime.enter();
    let service = native::ProcessService::with_backends(
        home.clone(),
        Arc::new(native::backend::CoreStoreBackend::new(home.clone())),
        Arc::new(Sessions),
    )
    .unwrap();
    native::service::install_process_service(service).unwrap();
    let repo = Arc::new(Repository::default());
    riwork_nsh_workspace::install_layout_repository(repo.clone()).unwrap();
    let layout: riwork_core::layouts::ProjectLayout = serde_json::from_value(json!({
        "layout":{"pane":1},"active_pane":1,"panels_initialized":true,"locked_panes":[],
        "panes":{"1":{"tabs":[{"kind":"panel","panel":"projects"}],"active_tab_key":"panel:projects"}}
    })).unwrap();
    for reason in ["quit", "restart"] {
        for cancel_before_rejection in [false, true] {
            let id = format!("{reason}-{cancel_before_rejection}-{}", Uuid::new_v4());
            let other_id = format!("other-{id}");
            let attempt = Arc::new(Attempt::default());
            repo.0.lock().unwrap().insert(id.clone(), attempt.clone());
            let other_writer = Arc::new(OtherWriter {
                fail: AtomicBool::new(true),
            });
            let other = host_layouts::registration(&other_id, other_writer.clone(), layout.clone())
                .unwrap();
            let mut app = AppProbe::start(&bundle, &id, &layout);
            attempt.entered.wait();
            app.host
                .send("window.willClose", json!({"reason":reason}))
                .unwrap();
            let closing = ClosingWindows::new(vec![window(&id), window(&other_id)]);
            assert!(host_layouts::lookup(&id).is_none());
            attempt.release_open.release();
            attempt.rejected.wait();
            if !cancel_before_rejection {
                attempt.release_rejection.release();
                app.wait("closing-error");
            }
            assert!(
                closing
                    .flush(&home)
                    .unwrap_err()
                    .contains("another window failed")
            );
            closing.cancel_with(|window_id| {
                if window_id == id {
                    app.host.send("window.closeCancelled", json!({})).unwrap();
                }
            });
            if cancel_before_rejection {
                app.ping();
                attempt.release_rejection.release();
            }
            app.wait("app.restoreReady");
            assert!(
                host_layouts::lookup(&id).is_some(),
                "{reason}: App must install a live native session"
            );
            assert_eq!(
                attempt.calls.load(Ordering::SeqCst),
                2,
                "App itself must retry exactly once"
            );
            app.host.send("window.closeCancelled", json!({})).unwrap();
            app.ping();
            assert_eq!(attempt.calls.load(Ordering::SeqCst), 2);
            assert_eq!(app.ready.load(Ordering::SeqCst), 1);
            other_writer.fail.store(false, Ordering::SeqCst);
            let successful = ClosingWindows::new(vec![window(&id), window(&other_id)]);
            successful.flush(&home).unwrap();
            app.stop();
            drop(successful);
            drop(closing);
            drop(other);
            eventually("no writer after successful teardown", || {
                attempt.writers.load(Ordering::SeqCst) == 0
            });
            assert!(host_layouts::lookup(&id).is_none());
            println!(
                "PASS failed {reason}, cancellation-before-rejection={cancel_before_rejection}; restored once; no writer after teardown"
            );
        }
        let id = format!("successful-{reason}-{}", Uuid::new_v4());
        let attempt = Arc::new(Attempt::default());
        repo.0.lock().unwrap().insert(id.clone(), attempt.clone());
        let mut app = AppProbe::start(&bundle, &id, &layout);
        attempt.entered.wait();
        app.host
            .send("window.willClose", json!({"reason":reason}))
            .unwrap();
        let successful = ClosingWindows::new(vec![window(&id)]);
        successful.flush(&home).unwrap();
        app.stop();
        attempt.release_rejection.release();
        attempt.release_open.release();
        attempt.rejected.wait();
        eventually("late open dropped its writer", || {
            attempt.writers.load(Ordering::SeqCst) == 0
        });
        assert!(host_layouts::lookup(&id).is_none());
        assert_eq!(attempt.calls.load(Ordering::SeqCst), 1);
        assert_eq!(attempt.writes.load(Ordering::SeqCst), 0);
        assert_eq!(app.ready.load(Ordering::SeqCst), 0);
        println!(
            "PASS successful {reason} with pending open: no resurrection, write or ready acknowledgement"
        );
    }
}
