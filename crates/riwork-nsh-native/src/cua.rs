//! Shared Cua setup state. Setup and access requests are explicit; the
//! process-wide 15 s Check retains driver startup and survives Settings closing.
use crate::{Subscription, hub::Hub, service};
use nsh_core::prelude::*;
use riwork_engine::cua::{CuaManager, CuaStatus};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
};

trait Backend: Send + Sync {
    fn status(&self) -> Result<Value, String>;
    fn action(&self, action: &str) -> Result<Value, String>;
}
struct Core(CuaManager);
fn status_value(status: CuaStatus) -> Value {
    let mut value = serde_json::to_value(&status).expect("Cua status serializes");
    value["skills"]["setup_needed"] = json!(status.skills.setup_needed());
    for (value, target) in value["skills"]["targets"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .zip(&status.skills.targets)
    {
        value["label"] = json!(target.state.label());
    }
    value
}
impl Backend for Core {
    fn status(&self) -> Result<Value, String> {
        self.0.status().map(status_value)
    }
    fn action(&self, action: &str) -> Result<Value, String> {
        match action {
            "check" => {
                if self.0.driver_path().is_ok() {
                    self.0.ensure_started()?;
                }
                self.status()
            }
            "install" => self.0.setup().map(status_value),
            "permissions" => self.0.request_permissions().map(status_value),
            _ => Err("invalid: unknown Cua action".into()),
        }
    }
}
struct Observer {
    backend: Arc<dyn Backend>,
    busy: AtomicBool,
    state: Mutex<Value>,
    topic: Hub<Value>,
}
impl Observer {
    fn new(backend: Arc<dyn Backend>) -> Self {
        let state = json!({"status":null,"pending":null,"error":null});
        Self {
            backend,
            busy: AtomicBool::new(false),
            state: Mutex::new(state.clone()),
            topic: Hub::new(state),
        }
    }
    fn snapshot(&self) -> Value {
        self.state.lock().unwrap().clone()
    }
    fn run(&self, action: Option<&str>) -> Result<Value, String> {
        let pending = match action {
            None | Some("check") => "Checking Cua.ai…",
            Some("install") => "Setting up Cua.ai…",
            Some("permissions") => "Opening macOS access settings…",
            _ => return Err("invalid: unknown Cua action".into()),
        };
        if self.busy.swap(true, Ordering::AcqRel) {
            return Ok(self.snapshot());
        }
        {
            let mut state = self.state.lock().unwrap();
            state["pending"] = json!(pending);
            state["error"] = Value::Null;
            self.topic.publish(state.clone());
        }
        let result = match action {
            Some(action) => self.backend.action(action),
            None => self.backend.status(),
        };
        let mut state = self.state.lock().unwrap();
        match result {
            Ok(status) => {
                state["status"] = status;
                state["error"] = Value::Null;
            }
            Err(error) => {
                if let Ok(status) = self.backend.status() {
                    state["status"] = status;
                }
                state["error"] = json!(error);
            }
        }
        state["pending"] = Value::Null;
        self.topic.publish(state.clone());
        self.busy.store(false, Ordering::Release);
        Ok(state.clone())
    }
    fn needs_poll(&self) -> bool {
        self.snapshot()["status"]["ready"] != true
    }
}

/// Fixture status never probes or invokes a driver, even when buttons are pressed.
struct Fixture(std::path::PathBuf);
impl Backend for Fixture {
    fn status(&self) -> Result<Value, String> {
        match std::fs::read(self.0.join("cua-fixture.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| e.to_string()),
            Err(_) => Ok(
                json!({"installed":true,"ready":true,"running":true,"version":"fixture",
                "accessibility":true,"screen_recording":true,"direct_capture_verified":true,
                "message":"Fixture driver ready (no driver invoked).",
                "skills":{"setup_needed":false,"message":"Fixture: no driver or agent homes probed.","targets":[]}}),
            ),
        }
    }
    fn action(&self, _: &str) -> Result<Value, String> {
        self.status()
    }
}

// Match the other native fixture services: debug/test homes under /tmp always
// use fakes. This also covers the existing windowless recovery acceptance app.
pub(crate) fn fixture_home(home: &std::path::Path) -> bool {
    cfg!(any(debug_assertions, feature = "test-support"))
        && (std::env::var_os("RIWORK_NSH_FAKE_SESSIONS").is_some()
            || home.starts_with("/tmp")
            || home.starts_with("/private/tmp"))
}
static OBSERVER: OnceLock<Arc<Observer>> = OnceLock::new();
static START: Mutex<()> = Mutex::new(());
fn observer() -> Result<Arc<Observer>, String> {
    let _start = START.lock().unwrap();
    if let Some(observer) = OBSERVER.get() {
        return Ok(observer.clone());
    }
    let home = service::process_home()?;
    let backend: Arc<dyn Backend> = if fixture_home(&home) {
        Arc::new(Fixture(home))
    } else {
        Arc::new(Core(CuaManager::at(home)?))
    };
    let observer = Arc::new(Observer::new(backend));
    let worker = observer.clone();
    std::thread::Builder::new()
        .name("nsh-cua-status".into())
        .spawn(move || {
            loop {
                if worker.needs_poll() {
                    let _ = worker.run(Some("check"));
                }
                std::thread::sleep(std::time::Duration::from_secs(15));
            }
        })
        .map_err(|e| e.to_string())?;
    let _ = OBSERVER.set(observer.clone());
    Ok(observer)
}

#[module]
pub struct Cua;
#[module]
impl Cua {
    pub async fn snapshot() -> Result<Value, String> {
        service::initialize().await?;
        service::blocking(|| Ok(observer()?.snapshot())).await
    }
    pub async fn run(action: String) -> Result<Value, String> {
        service::initialize().await?;
        service::blocking(move || observer()?.run(Some(&action))).await
    }
    pub fn subscribe(listener: Callback<Value>, cx: CallCx<'_>) -> Subscription {
        Subscription::start(cx, async move {
            if service::initialize().await.is_ok() {
                if let Ok(subscription) =
                    service::blocking(|| Ok(observer()?.topic.subscribe())).await
                {
                    while let Ok(snapshot) = subscription.recv().await {
                        listener.post(
                            serde_json::to_value(subscription.latest(snapshot))
                                .expect("Cua event serializes"),
                        );
                        next_frame().await;
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod phase6c_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    struct Fake {
        reads: AtomicUsize,
        actions: AtomicUsize,
        gate: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    }
    impl Backend for Fake {
        fn status(&self) -> Result<Value, String> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            Ok(json!({"ready":false,"installed":false}))
        }
        fn action(&self, _: &str) -> Result<Value, String> {
            self.actions.fetch_add(1, Ordering::SeqCst);
            if let Some(gate) = self.gate.lock().unwrap().take() {
                gate.recv().unwrap();
            }
            Err("fixture permission failure".into())
        }
    }
    #[test]
    fn phase6c_cua_read_only_poll_and_single_flight_actions_preserve_error_status() {
        let (tx, rx) = std::sync::mpsc::channel();
        let backend = Arc::new(Fake {
            reads: AtomicUsize::new(0),
            actions: AtomicUsize::new(0),
            gate: Mutex::new(Some(rx)),
        });
        let observer = Arc::new(Observer::new(backend.clone()));
        observer.run(None).unwrap();
        assert_eq!(backend.actions.load(Ordering::SeqCst), 0);
        assert!(observer.needs_poll());
        let worker = observer.clone();
        let join = std::thread::spawn(move || worker.run(Some("permissions")));
        while backend.actions.load(Ordering::SeqCst) == 0 {
            std::thread::yield_now();
        }
        assert_eq!(
            observer.run(Some("install")).unwrap()["pending"],
            "Opening macOS access settings…"
        );
        assert_eq!(backend.actions.load(Ordering::SeqCst), 1);
        tx.send(()).unwrap();
        let state = join.join().unwrap().unwrap();
        assert_eq!(state["error"], "fixture permission failure");
        assert!(state["pending"].is_null());
        assert_eq!(state["status"]["installed"], false);
        observer.state.lock().unwrap()["status"]["ready"] = json!(true);
        assert!(!observer.needs_poll());
        assert!(observer.run(Some("unknown")).is_err());
    }
}
