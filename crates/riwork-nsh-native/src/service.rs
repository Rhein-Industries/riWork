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
    dto::{
        CreateSessionRequest, FolderDto, ProjectDto, ProjectInspectionDto, SearchHitDto,
        SessionsSnapshot, ShellSessionDto, StoreState,
    },
    hub::{Hub, TopicSubscription},
    settings::PreferenceService,
    tabs::{EngineTabsBackend, TabsBackend, TabsService},
};

pub const SESSION_SAMPLE_INTERVAL: Duration = Duration::from_secs(2);
const STORE: u8 = 1;
const SESSIONS: u8 = 2;
const TABS: u8 = 4;
const SETTINGS: u8 = 8;

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

pub(crate) fn backend_error(error: anyhow::Error) -> String {
    let code = if error.is::<Unavailable>() {
        "unavailable"
    } else {
        "io"
    };
    format!("{code}: {error}")
}

/// JS identifiers must be canonical UUIDs, just like the CLI's tab/session IDs.
pub(crate) fn validate_uuid(id: &str) -> Result<(), String> {
    if uuid::Uuid::parse_str(id).is_ok_and(|value| value.to_string() == id) {
        Ok(())
    } else {
        Err("invalid: expected a canonical UUID".into())
    }
}

fn validate_optional_uuid(id: Option<&str>) -> Result<(), String> {
    id.map(validate_uuid).transpose().map(|_| ())
}

pub struct ProcessService {
    home: PathBuf,
    pub(crate) store: Arc<dyn StoreBackend>,
    pub(crate) sessions: Arc<dyn SessionsBackend>,
    pub(crate) tabs: Arc<TabsService>,
    pub(crate) preferences: Arc<PreferenceService>,
    store_hub: Arc<Hub<StoreState>>,
    store_updates: Arc<Mutex<()>>,
    sessions_hub: Arc<Hub<SessionsSnapshot>>,
    stop: Arc<AtomicBool>,
    wake: SyncSender<()>,
    dirty: Arc<AtomicU8>,
    worker: Option<JoinHandle<()>>,
    _watcher: notify::RecommendedWatcher,
    last_error: Arc<Mutex<Option<String>>>,
}

impl ProcessService {
    async fn mutate_store<T: Send + 'static>(
        self: &Arc<Self>,
        mutation: impl FnOnce(&dyn StoreBackend) -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        let backend = self.store.clone();
        let hub = self.store_hub.clone();
        let updates = self.store_updates.clone();
        blocking(move || {
            let _update = updates.lock().unwrap_or_else(PoisonError::into_inner);
            let result = mutation(backend.as_ref())?;
            // Publish before resolving. The watcher also observes this write,
            // but Hub deduplication prevents a second delivery.
            hub.publish(backend.snapshot().map_err(backend_error)?);
            Ok(result)
        })
        .await
    }

    pub async fn create_folder(
        self: &Arc<Self>,
        name: String,
        parent_id: Option<String>,
    ) -> Result<FolderDto, String> {
        self.mutate_store(move |backend| {
            validate_optional_uuid(parent_id.as_deref())?;
            if name.trim().is_empty() {
                return Err("invalid: folder name is empty".into());
            }
            backend
                .create_folder(&name, parent_id.as_deref())
                .map_err(backend_error)
        })
        .await
    }
    pub async fn rename_folder(
        self: &Arc<Self>,
        id: String,
        name: String,
    ) -> Result<FolderDto, String> {
        self.mutate_store(move |backend| {
            validate_uuid(&id)?;
            if name.trim().is_empty() {
                return Err("invalid: folder name is empty".into());
            }
            backend.rename_folder(&id, &name).map_err(backend_error)
        })
        .await
    }
    pub async fn move_folder(
        self: &Arc<Self>,
        id: String,
        parent_id: Option<String>,
    ) -> Result<FolderDto, String> {
        self.mutate_store(move |backend| {
            validate_uuid(&id)?;
            validate_optional_uuid(parent_id.as_deref())?;
            backend
                .move_folder(&id, parent_id.as_deref())
                .map_err(backend_error)
        })
        .await
    }
    pub async fn remove_folder(self: &Arc<Self>, id: String) -> Result<(), String> {
        self.mutate_store(move |backend| {
            validate_uuid(&id)?;
            backend.remove_folder(&id).map_err(backend_error)
        })
        .await
    }
    pub async fn move_project_to_folder(
        self: &Arc<Self>,
        project_id: String,
        folder_id: Option<String>,
    ) -> Result<ProjectDto, String> {
        self.mutate_store(move |backend| {
            validate_uuid(&project_id)?;
            validate_optional_uuid(folder_id.as_deref())?;
            backend
                .move_project_to_folder(&project_id, folder_id.as_deref())
                .map_err(backend_error)
        })
        .await
    }
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

    pub async fn sessions_create(
        self: &Arc<Self>,
        request: CreateSessionRequest,
    ) -> Result<ShellSessionDto, String> {
        let backend = self.sessions.clone();
        let wake = self.wake.clone();
        let dirty = self.dirty.clone();
        blocking(move || {
            validate_uuid(&request.project_id)?;
            validate_optional_uuid(request.worktree_id.as_deref())?;
            if request.cwd.is_empty() {
                return Err("invalid: session cwd is empty".into());
            }
            let session = backend.create(request).map_err(backend_error)?;
            dirty.fetch_or(SESSIONS | TABS, Ordering::Release);
            let _ = wake.try_send(());
            Ok(session)
        })
        .await
    }

    pub async fn sessions_close(self: &Arc<Self>, id: String) -> Result<(), String> {
        let backend = self.sessions.clone();
        let wake = self.wake.clone();
        let dirty = self.dirty.clone();
        blocking(move || {
            validate_uuid(&id)?;
            backend.close(&id).map_err(backend_error)?;
            dirty.fetch_or(SESSIONS | TABS, Ordering::Release);
            let _ = wake.try_send(());
            Ok(())
        })
        .await
    }

    /// Blocking constructor and test seam. Production uses initialize(), whose
    /// OnceLock guarantees one instance. Tests pass fixed isolated homes and fakes.
    pub fn with_backends(
        home: PathBuf,
        store: Arc<dyn StoreBackend>,
        sessions: Arc<dyn SessionsBackend>,
    ) -> Result<Arc<Self>> {
        let tabs = Arc::new(EngineTabsBackend::new(home.clone()));
        Self::with_tabs_backend(home, store, sessions, tabs)
    }

    /// Full adapter seam for tests of shared membership without tmux or chats.
    pub fn with_tabs_backend(
        home: PathBuf,
        store: Arc<dyn StoreBackend>,
        sessions: Arc<dyn SessionsBackend>,
        tabs: Arc<dyn TabsBackend>,
    ) -> Result<Arc<Self>> {
        riwork_core::paths::create_private_dir(&home)?;
        let home = home.canonicalize()?;
        let tabs = TabsService::new(tabs);
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
                    Ok(event) if event.need_rescan() => STORE | SESSIONS | TABS | SETTINGS,
                    Ok(event) if !matches!(event.kind, EventKind::Access(_)) => event
                        .paths
                        .iter()
                        .fold(0, |flags, path| flags | relevant(path, &watch_home)),
                    Ok(_) => 0,
                    Err(error) => {
                        *watch_error.lock().unwrap_or_else(PoisonError::into_inner) =
                            Some(format!("home watcher: {error}"));
                        STORE | SESSIONS | TABS | SETTINGS
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
        let preferences = PreferenceService::new(home.clone())?;
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
        let store_updates = Arc::new(Mutex::new(()));
        let worker_store_updates = store_updates.clone();
        let worker_sessions_hub = sessions_hub.clone();
        let worker_error = last_error.clone();
        let worker_tabs = tabs.clone();
        let worker_preferences = preferences.clone();
        let worker_dirty = dirty.clone();
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
                    let flags = worker_dirty.swap(0, Ordering::AcqRel);
                    if flags & SETTINGS != 0 {
                        worker_preferences.refresh();
                    }
                    if flags & STORE != 0 {
                        let _update = worker_store_updates
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner);
                        match worker_store.snapshot() {
                            Ok(snapshot) => worker_store_hub.publish(snapshot),
                            Err(error) => {
                                *worker_error.lock().unwrap_or_else(PoisonError::into_inner) =
                                    Some(format!("store snapshot: {error}"))
                            }
                        }
                    }
                    if flags & TABS != 0 {
                        if let Err(error) = worker_tabs.refresh() {
                            *worker_error.lock().unwrap_or_else(PoisonError::into_inner) =
                                Some(error);
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
            tabs,
            preferences,
            store_hub,
            store_updates,
            sessions_hub,
            stop,
            wake,
            dirty,
            worker: Some(worker),
            _watcher: watcher,
            last_error,
        }))
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn preferences(&self) -> &Arc<PreferenceService> {
        &self.preferences
    }

    pub fn tabs(&self) -> &Arc<TabsService> {
        &self.tabs
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
        STORE | TABS
    } else if path == home.join("settings.json") {
        SETTINGS
    } else if path == home.join("sessions.json")
        || path == home.join("orchestrator-wakes.json")
        || path.starts_with(home.join("agent-activity"))
    {
        SESSIONS | TABS
    } else if path.starts_with(home.join("project-tabs"))
        && path.extension().is_some_and(|ext| ext == "json")
        || path.starts_with(home.join("chats"))
            && path.file_name().is_some_and(|name| name == "info.json")
    {
        TABS
    } else {
        0
    }
}
