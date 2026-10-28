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
        Self::start_with_queue(bundle, id, layout, None)
    }
    fn start_with_queue(
        bundle: &Path,
        id: &str,
        layout: &riwork_core::layouts::ProjectLayout,
        queue: Option<mpsc::Sender<Main>>,
    ) -> Self {
        let host = HostChannel::new();
        host.set_params(json!({"schema":1,"windowId":id,"projectId":id,"kind":"project","path":"/fixture/Recovery","restore":{"layout":layout}}).as_object().unwrap().clone());
        let (events, receive) = mpsc::channel();
        let ready = Arc::new(AtomicUsize::new(0));
        for name in ["app.restoreReady", "window.setDockInfo", "app.fixture.pong"] {
            let events = events.clone();
            let ready = ready.clone();
            let queue = queue.clone();
            let id = id.to_owned();
            host.handle(name, move |call| {
                if name != "app.fixture.pong" {
                    if let Some(queue) = &queue {
                        queue.send(Main::Call(id.clone(), name, call)).unwrap();
                        return;
                    }
                    if let Err(error) = admit_host_call(Some(&window(&id).1), false, &id, name) {
                        call.reject(error);
                        return;
                    }
                }
                if name == "app.restoreReady" {
                    ready.fetch_add(1, Ordering::SeqCst);
                }
                events.send(name.into()).unwrap();
                call.resolve(json!({}));
            });
        }
        let id = id.to_owned();
        host.handle("system.fonts", move |call| {
            if let Some(queue) = &queue {
                queue
                    .send(Main::Call(id.clone(), "system.fonts", call))
                    .unwrap();
            } else {
                call.resolve(json!({"families":[]}));
            }
        });
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
    ordered_failure_completion(&bundle, &home, &repo, &layout);
}

/// The queue replaces only AppKit's exec_async scheduling and window lookup.
/// Admission and recovery dispatch are the production functions used by Host.
struct QueuedHost {
    quitting: bool,
    window: (String, RuntimeWindow),
    channel: HostChannel,
    ready_attempts: usize,
    ready_accepted: usize,
    ready_rejected: usize,
    failure_completed: bool,
    cancellations: usize,
}
impl QueuedHost {
    fn handle(&mut self, message: Main) {
        match message {
            Main::Recovery(recovery) => {
                if matches!(&recovery, Recovery::QuitFailed(..)) {
                    self.failure_completed = true;
                }
                recovery.handle(&mut self.quitting, |id| {
                    if id == self.window.0 {
                        self.cancellations += 1;
                        self.channel
                            .send("window.closeCancelled", json!({}))
                            .unwrap();
                    }
                });
            }
            Main::Call(id, name, call) => {
                let ready = name == "app.restoreReady";
                self.ready_attempts += usize::from(ready);
                let window = (id == self.window.0).then_some(&self.window.1);
                if let Err(error) = admit_host_call(window, self.quitting, &id, name) {
                    if ready {
                        self.ready_rejected += 1;
                        println!(
                            "READY REJECTED: {error}; failure_completed={}",
                            self.failure_completed
                        );
                    }
                    call.reject(error);
                    return;
                }
                if ready {
                    self.ready_accepted += 1;
                    println!(
                        "READY ACCEPTED: failure_completed={}",
                        self.failure_completed
                    );
                }
                call.resolve(if name == "system.fonts" {
                    json!({"families":[]})
                } else {
                    json!({})
                });
            }
            _ => panic!("unexpected fixture main message"),
        }
    }
    fn drain(&mut self, queue: &mpsc::Receiver<Main>) {
        while let Ok(message) = queue.try_recv() {
            self.handle(message);
        }
    }
    fn until(&mut self, queue: &mpsc::Receiver<Main>, done: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + WAIT;
        while !done(self) {
            self.handle(
                queue
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .expect("queued host progress"),
            );
        }
    }
}

fn ordered_failure_completion(
    bundle: &Path,
    home: &Path,
    repo: &Repository,
    layout: &riwork_core::layouts::ProjectLayout,
) {
    let mut failures = Vec::new();
    // All three real service failure sites use complete_failed_quit. The last
    // case fails replacement readiness after a successful persistence barrier.
    for reason in ["quit", "restart-launch", "restart-ready"] {
        let id = format!("ordered-{reason}-{}", Uuid::new_v4());
        let other_id = format!("other-{id}");
        let attempt = Arc::new(Attempt::default());
        repo.0.lock().unwrap().insert(id.clone(), attempt.clone());
        let other_writer = Arc::new(OtherWriter {
            fail: AtomicBool::new(reason != "restart-ready"),
        });
        let other =
            host_layouts::registration(&other_id, other_writer.clone(), layout.clone()).unwrap();
        let (queue, messages) = mpsc::channel();
        let mut app = AppProbe::start_with_queue(bundle, &id, layout, Some(queue.clone()));
        let mut main = QueuedHost {
            quitting: false,
            window: window(&id),
            channel: app.host.clone(),
            ready_attempts: 0,
            ready_accepted: 0,
            ready_rejected: 0,
            failure_completed: false,
            cancellations: 0,
        };
        attempt.entered.wait();
        app.ping();
        main.drain(&messages);
        main.quitting = true;
        app.host
            .send(
                "window.willClose",
                json!({"reason": if reason == "quit" { "quit" } else { "restart" }}),
            )
            .unwrap();
        let batch = ClosingWindows::new(vec![window(&id), window(&other_id)]);
        attempt.release_rejection.release();
        attempt.release_open.release();
        app.wait("closing-error");
        let error = if reason == "restart-ready" {
            batch.flush(home).unwrap();
            "fixture: replacement readiness failed".to_owned()
        } else {
            batch.flush(home).unwrap_err()
        };
        let completing = Arc::new(Gate::default());
        let finish = Arc::new(Gate::default());
        let worker = {
            let completing = completing.clone();
            let finish = finish.clone();
            std::thread::spawn(move || {
                complete_failed_quit(
                    batch,
                    error,
                    || {
                        completing.release();
                        finish.wait();
                    },
                    |message| queue.send(message).unwrap(),
                )
            })
        };
        completing.wait();
        // The service is deliberately held inside fail_reload/resume_forwarding.
        // Old ordering already queued cancellation. Let App fully retry and post
        // readiness now, before releasing completion, to expose the lost ack.
        main.drain(&messages);
        if main.cancellations != 0 {
            main.until(&messages, |host| host.ready_attempts == 1);
            assert_eq!(
                main.ready_rejected, 1,
                "old ordering must expose the closed admission gate"
            );
            println!("{reason}: readiness lost while failure completion is deliberately blocked");
        } else {
            assert_eq!(attempt.calls.load(Ordering::SeqCst), 1);
            assert!(main.quitting);
            println!("{reason}: recovery correctly withheld during delayed failure completion");
        }
        finish.release();
        worker.join().unwrap();
        main.until(&messages, |host| {
            host.failure_completed && host.ready_attempts == 1
        });
        app.ping();
        main.drain(&messages);
        assert!(!main.quitting);
        assert_eq!(main.cancellations, 1);
        assert_eq!(attempt.calls.load(Ordering::SeqCst), 2);
        if (
            main.ready_attempts,
            main.ready_accepted,
            main.ready_rejected,
        ) != (1, 1, 0)
        {
            failures.push(format!(
                "{reason}: attempts={}, accepted={}, rejected={}",
                main.ready_attempts, main.ready_accepted, main.ready_rejected
            ));
        } else {
            println!(
                "PASS ordered {reason}: app.restoreReady accepted exactly once after failure completion"
            );
        }
        // A later successful teardown still keeps the native closing marker and
        // releases the recovered writer. No live desktop lifecycle is involved.
        other_writer.fail.store(false, Ordering::SeqCst);
        let successful = ClosingWindows::new(vec![window(&id), window(&other_id)]);
        successful.flush(home).unwrap();
        app.stop();
        drop(successful);
        drop(other);
        eventually("ordered case left an orphan writer", || {
            attempt.writers.load(Ordering::SeqCst) == 0
        });
        assert!(host_layouts::lookup(&id).is_none());
    }
    assert!(
        failures.is_empty(),
        "lost ready acknowledgement: {}",
        failures.join("; ")
    );
}
