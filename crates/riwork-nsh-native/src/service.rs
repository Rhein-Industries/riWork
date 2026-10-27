//! One watcher and one sampler for the process, independent of window count.
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock, PoisonError,
        atomic::{AtomicBool, AtomicU8, Ordering},
        mpsc::{self, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use notify::{EventKind, RecursiveMode, Watcher};

use crate::{
    backend::{
        CoreStoreBackend, EngineSessionsBackend, SessionsBackend, StoreBackend, Unavailable,
    },
    dto::{ProjectInspectionDto, SearchHitDto, SessionsSnapshot, StoreState},
    hub::{Hub, TopicSubscription},
};

pub const SESSION_SAMPLE_INTERVAL: Duration = Duration::from_secs(2);
const STORE: u8 = 1;
const SESSIONS: u8 = 2;

// Kept strongly for the life of the process, never once per AppView. The home's
// resolution and all setup happen inside initialize's blocking-pool hop.
static PROCESS: OnceLock<Arc<ProcessService>> = OnceLock::new();
static PROCESS_HOME: OnceLock<Result<PathBuf, String>> = OnceLock::new();
static START: Mutex<()> = Mutex::new(());

/// Install a host-supplied service before creating any engines. This is also the
/// fake-backend seam used by headless tests of the real `riwork/native` registry.
/// It cannot replace an existing process service or change its fixed home.
pub fn install_process_service(service: Arc<ProcessService>) -> Result<(), String> {
    let _start = START.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(home) = PROCESS_HOME.get() {
        let home = home.as_ref().map_err(Clone::clone)?;
        if home.canonicalize().map_err(io_error)? != service.home() {
            return Err("conflict: native process home is already fixed".into());
        }
    }
    let _ = PROCESS_HOME.set(Ok(service.home().to_owned()));
    PROCESS
        .set(service)
        .map_err(|_| "conflict: native process service is already initialized".into())
}

/// Initialize before the first AppView. Calls from multiple windows also converge
/// here; RIWORK_HOME is resolved just once and cannot be supplied from JavaScript.
pub async fn initialize() -> Result<Arc<ProcessService>, String> {
    blocking(|| {
        let _start = START.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(service) = PROCESS.get() {
            return Ok(service.clone());
        }
        let home = resolved_home()?;
        let store = Arc::new(CoreStoreBackend::new(home.clone()));
        let sessions = Arc::new(EngineSessionsBackend::new(home.clone()));
        let service = ProcessService::with_backends(home, store, sessions).map_err(io_error)?;
        let _ = PROCESS.set(service.clone());
        Ok(service)
    })
    .await
}

/// Shared by layout IO and service startup. Layout-only headless runners do not
/// need to start the session sampler, but still resolve exactly the same fixed
/// process home as Store/Sessions and host-supplied fake backends.
pub(crate) fn process_home() -> Result<PathBuf, String> {
    let _start = START.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(service) = PROCESS.get() {
        return Ok(service.home().to_owned());
    }
    resolved_home()
}

fn resolved_home() -> Result<PathBuf, String> {
    PROCESS_HOME
        .get_or_init(|| riwork_core::paths::riwork_home().map_err(io_error))
        .clone()
}

pub(crate) fn initialized() -> Result<Arc<ProcessService>, String> {
    PROCESS.get().cloned().ok_or_else(|| {
        "unavailable: call riwork_nsh_native::initialize().await before creating AppViews".into()
    })
}

/// Move every core/engine call, including service setup and hub registration, off
/// both the AppKit thread and the NSH engine thread.
pub(crate) async fn blocking<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| format!("broken: native worker failed: {error}"))?
}

pub(crate) fn io_error(error: impl std::fmt::Display) -> String {
    format!("io: {error}")
}

fn backend_error(error: anyhow::Error) -> String {
    let code = if error.is::<Unavailable>() {
        "unavailable"
    } else {
        "io"
    };
    format!("{code}: {error}")
}

pub struct ProcessService {
    home: PathBuf,
    pub(crate) store: Arc<dyn StoreBackend>,
    pub(crate) sessions: Arc<dyn SessionsBackend>,
    store_hub: Arc<Hub<StoreState>>,
    sessions_hub: Arc<Hub<SessionsSnapshot>>,
    stop: Arc<AtomicBool>,
    wake: SyncSender<()>,
    worker: Option<JoinHandle<()>>,
    _watcher: notify::RecommendedWatcher,
    last_error: Arc<Mutex<Option<String>>>,
}

impl ProcessService {
    pub async fn store_snapshot(self: &Arc<Self>) -> Result<StoreState, String> {
        let backend = self.store.clone();
        blocking(move || backend.snapshot().map_err(backend_error)).await
    }

    pub async fn store_search(
        self: &Arc<Self>,
        query: String,
    ) -> Result<Vec<SearchHitDto>, String> {
        let backend = self.store.clone();
        blocking(move || backend.search(&query).map_err(backend_error)).await
    }

    pub async fn inspect_project(
        self: &Arc<Self>,
        root: PathBuf,
    ) -> Result<ProjectInspectionDto, String> {
        let backend = self.store.clone();
        blocking(move || {
            if root.as_os_str().is_empty() {
                return Err("invalid: project root is empty".into());
            }
            backend.inspect_project(&root).map_err(backend_error)
        })
        .await
    }

    pub async fn sessions_list(self: &Arc<Self>) -> Result<SessionsSnapshot, String> {
        let backend = self.sessions.clone();
        blocking(move || backend.list().map_err(backend_error)).await
    }

    /// Blocking constructor and test seam. Production uses initialize(), whose
    /// OnceLock guarantees one instance. Tests pass fixed isolated homes and fakes.
    pub fn with_backends(
        home: PathBuf,
        store: Arc<dyn StoreBackend>,
        sessions: Arc<dyn SessionsBackend>,
    ) -> Result<Arc<Self>> {
        riwork_core::paths::create_private_dir(&home)?;
        let home = home.canonicalize()?;
        let (wake, receive) = mpsc::sync_channel(1);
        let dirty = Arc::new(AtomicU8::new(0));
        let watch_dirty = dirty.clone();
        let watch_wake = wake.clone();
        let watch_home = home.clone();
        let last_error = Arc::new(Mutex::new(None));
        let watch_error = last_error.clone();
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                let flags = match event {
                    Ok(event) if event.need_rescan() => STORE | SESSIONS,
                    Ok(event) if !matches!(event.kind, EventKind::Access(_)) => event
                        .paths
                        .iter()
                        .fold(0, |flags, path| flags | relevant(path, &watch_home)),
                    Ok(_) => 0,
                    Err(error) => {
                        *watch_error.lock().unwrap_or_else(PoisonError::into_inner) =
                            Some(format!("home watcher: {error}"));
                        STORE | SESSIONS
                    }
                };
                if flags != 0 {
                    watch_dirty.fetch_or(flags, Ordering::Release);
                    let _ = watch_wake.try_send(());
                }
            })?;
        // Register before loading snapshots: no external write can fall in a gap
        // between the initial read and installation of the push source.
        watcher.watch(&home, RecursiveMode::Recursive)?;
        let store_hub = Arc::new(Hub::new(
            store.snapshot().context("initial store snapshot")?,
        ));
        let mut has_shells = false;
        let sessions_hub = Arc::new(match sessions.sample() {
            Ok(snapshot) => {
                has_shells = !snapshot.sessions.is_empty();
                Hub::new(snapshot)
            }
            Err(error) => {
                *last_error.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some(format!("initial session sample: {error}"));
                Hub::pending()
            }
        });
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker_store = store.clone();
        let worker_sessions = sessions.clone();
        let worker_store_hub = store_hub.clone();
        let worker_sessions_hub = sessions_hub.clone();
        let worker_error = last_error.clone();
        let worker = thread::Builder::new()
            .name("riwork-native-service".into())
            .spawn(move || {
                let mut next_sample = has_shells.then(|| Instant::now() + SESSION_SAMPLE_INTERVAL);
                while !worker_stop.load(Ordering::Acquire) {
                    if let Some(deadline) = next_sample {
                        let _ = receive
                            .recv_timeout(deadline.saturating_duration_since(Instant::now()));
                    } else {
                        // An empty inventory needs no liveness timer. The home
                        // watcher wakes us when another process creates a shell;
                        // shutdown also wakes this blocking receive.
                        let _ = receive.recv();
                    }
                    if worker_stop.load(Ordering::Acquire) {
                        break;
                    }
                    let flags = dirty.swap(0, Ordering::AcqRel);
                    if flags & STORE != 0 {
                        match worker_store.snapshot() {
                            Ok(snapshot) => worker_store_hub.publish(snapshot),
                            Err(error) => {
                                *worker_error.lock().unwrap_or_else(PoisonError::into_inner) =
                                    Some(format!("store snapshot: {error}"))
                            }
                        }
                    }
                    if flags & SESSIONS != 0
                        || next_sample.is_some_and(|deadline| Instant::now() >= deadline)
                    {
                        if flags & SESSIONS != 0 {
                            worker_sessions.invalidate_sample();
                        }
                        match worker_sessions.sample() {
                            Ok(snapshot) => {
                                has_shells = !snapshot.sessions.is_empty();
                                worker_sessions_hub.publish(snapshot);
                            }
                            Err(error) => {
                                *worker_error.lock().unwrap_or_else(PoisonError::into_inner) =
                                    Some(format!("session sample: {error}"))
                            }
                        }
                        // Only this thread performs the two-second tmux fallback,
                        // and only while the last successful inventory has shells.
                        // A transient read failure preserves that last known state.
                        next_sample = has_shells.then(|| Instant::now() + SESSION_SAMPLE_INTERVAL);
                    }
                }
            })?;
        Ok(Arc::new(Self {
            home,
            store,
            sessions,
            store_hub,
            sessions_hub,
            stop,
            wake,
            worker: Some(worker),
            _watcher: watcher,
            last_error,
        }))
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    /// For host diagnostics, called on a background thread.
    pub fn last_error(&self) -> Option<String> {
        self.last_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub async fn subscribe_store(
        self: &Arc<Self>,
    ) -> Result<TopicSubscription<StoreState>, String> {
        let hub = self.store_hub.clone();
        blocking(move || Ok(hub.subscribe())).await
    }

    pub async fn subscribe_sessions(
        self: &Arc<Self>,
    ) -> Result<TopicSubscription<SessionsSnapshot>, String> {
        let hub = self.sessions_hub.clone();
        blocking(move || Ok(hub.subscribe())).await
    }
}

impl Drop for ProcessService {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.wake.try_send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn relevant(path: &Path, home: &Path) -> u8 {
    if path == home.join("state.json") {
        STORE
    } else if path == home.join("sessions.json")
        || path == home.join("orchestrator-wakes.json")
        || path.starts_with(home.join("agent-activity"))
    {
        SESSIONS
    } else {
        0
    }
}
