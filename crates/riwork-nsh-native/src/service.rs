//! One watcher and one sampler for the process, independent of window count.
use std::{
    collections::BTreeMap,
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
        CoreStoreBackend, EngineSessionsBackend, EngineTabsBackend, InvalidInput, SessionsBackend,
        StoreBackend, TabsBackend, Unavailable,
    },
    dto::{
        CreateSessionRequest, FolderDto, ProjectDto, ProjectInspectionDto, ProjectRequest,
        SearchHitDto, SessionsSnapshot, ShellSessionDto, StoreState, TabUpdate, TabsSnapshot,
        TaskDto, WorktreeDto,
    },
    hub::{Hub, TopicSubscription},
    settings::PreferenceService,
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
    } else if error.is::<InvalidInput>() {
        "invalid"
    } else {
        "io"
    };
    format!("{code}: {error}")
}

fn validate_input_id(id: &str) -> anyhow::Result<()> {
    if uuid::Uuid::parse_str(id).is_ok_and(|value| value.to_string() == id) {
        Ok(())
    } else {
        Err(InvalidInput("expected a canonical UUID".into()).into())
    }
}

fn validate_optional_input_id(id: Option<&str>) -> anyhow::Result<()> {
    id.map(validate_input_id).transpose().map(|_| ())
}

fn validate_folder_name(name: &str) -> anyhow::Result<()> {
    if name.trim().is_empty() {
        Err(InvalidInput("folder name is empty".into()).into())
    } else {
        Ok(())
    }
}

pub(crate) fn validate_uuid(id: &str) -> Result<(), String> {
    validate_input_id(id).map_err(backend_error)
}

pub struct ProcessService {
    home: PathBuf,
    pub(crate) store: Arc<dyn StoreBackend>,
    pub(crate) sessions: Arc<dyn SessionsBackend>,
    pub(crate) preferences: Arc<PreferenceService>,
    store_hub: Arc<Hub<StoreState>>,
    sessions_hub: Arc<Hub<SessionsSnapshot>>,
    tabs: Arc<dyn TabsBackend>,
    tabs_hubs: Arc<Mutex<BTreeMap<String, Arc<Hub<TabsSnapshot>>>>>,
    store_refresh: Arc<Mutex<()>>,
    sessions_refresh: Arc<Mutex<()>>,
    has_worktrees: Arc<AtomicBool>,
    has_shells: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    wake: SyncSender<()>,
    worker: Option<JoinHandle<()>>,
    _watcher: notify::RecommendedWatcher,
    last_error: Arc<Mutex<Option<String>>>,
}

impl ProcessService {
    async fn store_write<T: Send + 'static>(
        self: &Arc<Self>,
        operation: impl FnOnce(&dyn StoreBackend) -> anyhow::Result<T> + Send + 'static,
    ) -> Result<T, String> {
        let service = self.clone();
        blocking(move || {
            let _guard = service
                .store_refresh
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let value = operation(service.store.as_ref()).map_err(backend_error)?;
            // Write and publish are serialized with watcher refreshes, preventing
            // an older read from overwriting a newer mutation in either window.
            let snapshot = service.store.snapshot().map_err(backend_error)?;
            let has_worktrees = !snapshot.worktrees.is_empty();
            service.store_hub.publish(snapshot);
            if service.has_worktrees.swap(has_worktrees, Ordering::AcqRel) != has_worktrees {
                let _ = service.wake.try_send(());
            }
            Ok(value)
        })
        .await
    }

    pub async fn default_project_directory(self: &Arc<Self>) -> Result<String, String> {
        let backend = self.store.clone();
        blocking(move || backend.default_project_directory().map_err(backend_error)).await
    }

    pub async fn create_project(
        self: &Arc<Self>,
        request: ProjectRequest,
    ) -> Result<ProjectDto, String> {
        self.store_write(move |backend| backend.create_project(request))
            .await
    }
    pub async fn create_new_project(
        self: &Arc<Self>,
        request: ProjectRequest,
    ) -> Result<ProjectDto, String> {
        self.store_write(move |backend| backend.create_new_project(request))
            .await
    }
    pub async fn add_project(
        self: &Arc<Self>,
        request: ProjectRequest,
    ) -> Result<ProjectDto, String> {
        self.store_write(move |backend| backend.add_project(request))
            .await
    }
    pub async fn use_project(self: &Arc<Self>, selector: String) -> Result<ProjectDto, String> {
        self.store_write(move |backend| {
            validate_input_id(&selector)?;
            backend.use_project(&selector)
        })
        .await
    }
    pub async fn rename_project(
        self: &Arc<Self>,
        selector: String,
        name: String,
    ) -> Result<ProjectDto, String> {
        self.store_write(move |backend| {
            validate_input_id(&selector)?;
            backend.rename_project(&selector, &name)
        })
        .await
    }
    pub async fn set_project_notifications(
        self: &Arc<Self>,
        selector: String,
        enabled: bool,
    ) -> Result<ProjectDto, String> {
        self.store_write(move |backend| {
            validate_input_id(&selector)?;
            backend.set_project_notifications(&selector, enabled)
        })
        .await
    }
    pub async fn set_project_orchestrator_wakes(
        self: &Arc<Self>,
        selector: String,
        enabled: bool,
    ) -> Result<ProjectDto, String> {
        self.store_write(move |backend| {
            validate_input_id(&selector)?;
            backend.set_project_orchestrator_wakes(&selector, enabled)
        })
        .await
    }
    pub async fn create_folder(
        self: &Arc<Self>,
        name: String,
        parent: Option<String>,
    ) -> Result<FolderDto, String> {
        self.store_write(move |backend| {
            validate_optional_input_id(parent.as_deref())?;
            validate_folder_name(&name)?;
            backend.create_folder(&name, parent.as_deref())
        })
        .await
    }
    pub async fn rename_folder(
        self: &Arc<Self>,
        selector: String,
        name: String,
    ) -> Result<FolderDto, String> {
        self.store_write(move |backend| {
            validate_input_id(&selector)?;
            validate_folder_name(&name)?;
            backend.rename_folder(&selector, &name)
        })
        .await
    }
    pub async fn move_folder(
        self: &Arc<Self>,
        selector: String,
        parent: Option<String>,
    ) -> Result<FolderDto, String> {
        self.store_write(move |backend| {
            validate_input_id(&selector)?;
            validate_optional_input_id(parent.as_deref())?;
            backend.move_folder(&selector, parent.as_deref())
        })
        .await
    }
    pub async fn remove_folder(self: &Arc<Self>, selector: String) -> Result<(), String> {
        self.store_write(move |backend| {
            validate_input_id(&selector)?;
            backend.remove_folder(&selector)
        })
        .await
    }
    pub async fn move_project_to_folder(
        self: &Arc<Self>,
        selector: String,
        folder: Option<String>,
    ) -> Result<ProjectDto, String> {
        self.store_write(move |backend| {
            validate_input_id(&selector)?;
            validate_optional_input_id(folder.as_deref())?;
            backend.move_project_to_folder(&selector, folder.as_deref())
        })
        .await
    }
    pub async fn sync_worktrees(
        self: &Arc<Self>,
        selector: String,
    ) -> Result<Vec<WorktreeDto>, String> {
        self.store_write(move |backend| {
            validate_input_id(&selector)?;
            backend.sync_worktrees(&selector)
        })
        .await
    }
    pub async fn refresh_worktrees(
        self: &Arc<Self>,
        selector: String,
    ) -> Result<Vec<WorktreeDto>, String> {
        self.store_write(move |backend| {
            validate_input_id(&selector)?;
            backend.refresh_worktrees(&selector)
        })
        .await
    }
    pub async fn add_task(
        self: &Arc<Self>,
        project: String,
        title: String,
        details: String,
    ) -> Result<TaskDto, String> {
        self.store_write(move |backend| {
            validate_input_id(&project)?;
            backend.add_task(&project, &title, &details)
        })
        .await
    }
    pub async fn assign_tasks(
        self: &Arc<Self>,
        worktree: String,
        tasks: Vec<String>,
    ) -> Result<Vec<TaskDto>, String> {
        self.store_write(move |backend| {
            validate_input_id(&worktree)?;
            for id in &tasks {
                validate_input_id(id)?;
            }
            backend.assign_tasks(&worktree, &tasks)
        })
        .await
    }
    pub async fn unassign_tasks(
        self: &Arc<Self>,
        tasks: Vec<String>,
    ) -> Result<Vec<TaskDto>, String> {
        self.store_write(move |backend| {
            for id in &tasks {
                validate_input_id(id)?;
            }
            backend.unassign_tasks(&tasks)
        })
        .await
    }
    pub async fn set_task_status(
        self: &Arc<Self>,
        selector: String,
        status: String,
    ) -> Result<TaskDto, String> {
        if !matches!(status.as_str(), "todo" | "in_progress" | "done") {
            return Err("invalid: use todo, in_progress, or done".into());
        }
        self.store_write(move |backend| {
            validate_input_id(&selector)?;
            backend.set_task_status(&selector, &status)
        })
        .await
    }

    async fn session_write<T: Send + 'static>(
        self: &Arc<Self>,
        operation: impl FnOnce(&dyn SessionsBackend) -> anyhow::Result<T> + Send + 'static,
    ) -> Result<T, String> {
        let service = self.clone();
        blocking(move || {
            let _guard = service
                .sessions_refresh
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let result = operation(service.sessions.as_ref()).map_err(backend_error)?;
            service.sessions.invalidate_sample();
            let snapshot = service.sessions.sample().map_err(backend_error)?;
            let has_shells = !snapshot.sessions.is_empty();
            service.sessions_hub.publish(snapshot);
            if service.has_shells.swap(has_shells, Ordering::AcqRel) != has_shells {
                let _ = service.wake.try_send(());
            }
            refresh_tabs(&service.tabs, &service.tabs_hubs, &service.last_error);
            Ok(result)
        })
        .await
    }

    pub async fn create_session(
        self: &Arc<Self>,
        request: CreateSessionRequest,
    ) -> Result<ShellSessionDto, String> {
        self.session_write(move |backend| {
            validate_input_id(&request.project_id)?;
            validate_optional_input_id(request.worktree_id.as_deref())?;
            if request.cwd.is_empty() {
                return Err(InvalidInput("session cwd is empty".into()).into());
            }
            backend.create(request)
        })
        .await
    }
    pub async fn close_session(self: &Arc<Self>, id: String) -> Result<(), String> {
        self.session_write(move |backend| {
            validate_input_id(&id)?;
            backend.close(&id)
        })
        .await
    }

    /// Compatibility names retained for P1 bridge callers and fixtures.
    pub async fn sessions_create(
        self: &Arc<Self>,
        request: CreateSessionRequest,
    ) -> Result<ShellSessionDto, String> {
        self.create_session(request).await
    }
    pub async fn sessions_close(self: &Arc<Self>, id: String) -> Result<(), String> {
        self.close_session(id).await
    }

    pub async fn tabs_update(
        self: &Arc<Self>,
        project: String,
        update: TabUpdate,
    ) -> Result<TabsSnapshot, String> {
        let service = self.clone();
        blocking(move || {
            validate_project_id(&project)?;
            let engine_update: riwork_engine::project_tabs::Update = update.clone().into();
            engine_update
                .validate()
                .map_err(|error| format!("invalid: {error}"))?;
            let mut hubs = service
                .tabs_hubs
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let result = service
                .tabs
                .update(&project, update)
                .map_err(backend_error)?;
            if let Some(hub) = hubs.get_mut(&project) {
                hub.publish(result.clone());
            }
            Ok(result)
        })
        .await
    }

    pub async fn subscribe_tabs(
        self: &Arc<Self>,
        project: String,
    ) -> Result<TopicSubscription<TabsSnapshot>, String> {
        let service = self.clone();
        blocking(move || {
            validate_project_id(&project)?;
            let mut hubs = service
                .tabs_hubs
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let hub = if let Some(hub) = hubs.get(&project) {
                hub.clone()
            } else {
                let hub = Arc::new(match service.tabs.snapshot(&project) {
                    Ok(snapshot) => Hub::new(snapshot),
                    Err(error) => {
                        *service
                            .last_error
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner) =
                            Some(format!("initial tabs snapshot: {error}"));
                        // Keep the subscription registered through a temporary
                        // tmux/inventory outage; the process worker retries it.
                        Hub::pending()
                    }
                });
                hubs.insert(project, hub.clone());
                hub
            };
            Ok(hub.subscribe())
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

    /// Blocking constructor and test seam. Production uses initialize(), whose
    /// OnceLock guarantees one instance. Tests pass fixed isolated homes and fakes.
    pub fn with_backends(
        home: PathBuf,
        store: Arc<dyn StoreBackend>,
        sessions: Arc<dyn SessionsBackend>,
    ) -> Result<Arc<Self>> {
        Self::with_all_backends(
            home.clone(),
            store,
            sessions,
            Arc::new(EngineTabsBackend::new(home)),
        )
    }

    pub fn with_all_backends(
        home: PathBuf,
        store: Arc<dyn StoreBackend>,
        sessions: Arc<dyn SessionsBackend>,
        tabs: Arc<dyn TabsBackend>,
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
        let initial_store = store.snapshot().context("initial store snapshot")?;
        let has_worktrees = Arc::new(AtomicBool::new(!initial_store.worktrees.is_empty()));
        let store_hub = Arc::new(Hub::new(initial_store));
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
        let has_shells = Arc::new(AtomicBool::new(has_shells));
        let stop = Arc::new(AtomicBool::new(false));
        let tabs_hubs = Arc::new(Mutex::new(BTreeMap::new()));
        let store_refresh = Arc::new(Mutex::new(()));
        let sessions_refresh = Arc::new(Mutex::new(()));
        let worker_stop = stop.clone();
        let worker_store = store.clone();
        let worker_sessions = sessions.clone();
        let worker_store_hub = store_hub.clone();
        let worker_sessions_hub = sessions_hub.clone();
        let worker_error = last_error.clone();
        let worker_tabs = tabs.clone();
        let worker_tabs_hubs = tabs_hubs.clone();
        let worker_store_refresh = store_refresh.clone();
        let worker_sessions_refresh = sessions_refresh.clone();
        let worker_has_worktrees = has_worktrees.clone();
        let worker_has_shells = has_shells.clone();
        let worker_preferences = preferences.clone();
        let worker = thread::Builder::new()
            .name("riwork-native-service".into())
            .spawn(move || {
                let mut next_sample = None;
                let mut next_presence = None;
                while !worker_stop.load(Ordering::Acquire) {
                    // The last successful inventories arm separate deadlines.
                    // Explicit CRUD updates these flags and wakes this receive,
                    // so scheduling does not depend on filesystem event timing.
                    let now = Instant::now();
                    if worker_has_shells.load(Ordering::Acquire) {
                        next_sample.get_or_insert(now + SESSION_SAMPLE_INTERVAL);
                    } else {
                        next_sample = None;
                    }
                    if worker_has_worktrees.load(Ordering::Acquire) {
                        next_presence.get_or_insert(now + SESSION_SAMPLE_INTERVAL);
                    } else {
                        next_presence = None;
                    }
                    let deadline = next_sample.into_iter().chain(next_presence).min();
                    if let Some(deadline) = deadline {
                        let _ = receive.recv_timeout(deadline.saturating_duration_since(now));
                    } else {
                        // An empty home has no periodic liveness or presence
                        // work. Watcher events, CRUD transitions and shutdown
                        // still wake this blocking receive.
                        let _ = receive.recv();
                    }
                    if worker_stop.load(Ordering::Acquire) {
                        break;
                    }
                    let flags = dirty.swap(0, Ordering::AcqRel);
                    if flags & SETTINGS != 0 {
                        worker_preferences.refresh();
                    }
                    let now = Instant::now();
                    let sample_due = next_sample.is_some_and(|deadline| now >= deadline);
                    let presence_due = next_presence.is_some_and(|deadline| now >= deadline);
                    if flags & STORE != 0 || presence_due {
                        let _guard = worker_store_refresh
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner);
                        let snapshot = if presence_due {
                            worker_store.refresh_presence()
                        } else {
                            worker_store.snapshot()
                        };
                        match snapshot {
                            Ok(snapshot) => {
                                worker_has_worktrees
                                    .store(!snapshot.worktrees.is_empty(), Ordering::Release);
                                worker_store_hub.publish(snapshot);
                            }
                            Err(error) => {
                                *worker_error.lock().unwrap_or_else(PoisonError::into_inner) =
                                    Some(format!("store snapshot: {error}"))
                            }
                        }
                        if presence_due {
                            // Session watcher wakes may reset their fallback,
                            // but cannot postpone worktree presence forever.
                            next_presence = worker_has_worktrees
                                .load(Ordering::Acquire)
                                .then(|| Instant::now() + SESSION_SAMPLE_INTERVAL);
                        }
                    }
                    if flags & SESSIONS != 0 || sample_due {
                        let _guard = worker_sessions_refresh
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner);
                        if flags & SESSIONS != 0 {
                            worker_sessions.invalidate_sample();
                        }
                        match worker_sessions.sample() {
                            Ok(snapshot) => {
                                worker_has_shells
                                    .store(!snapshot.sessions.is_empty(), Ordering::Release);
                                worker_sessions_hub.publish(snapshot);
                            }
                            Err(error) => {
                                *worker_error.lock().unwrap_or_else(PoisonError::into_inner) =
                                    Some(format!("session sample: {error}"))
                            }
                        }
                        // Only nonempty shell inventories need a liveness
                        // fallback. A transient failure keeps the last known
                        // state, as in M5; empty inventories rely on the watcher.
                        next_sample = worker_has_shells
                            .load(Ordering::Acquire)
                            .then(|| Instant::now() + SESSION_SAMPLE_INTERVAL);
                    }
                    // Presence alone is not a session inventory trigger. Its
                    // durable branch/discovery writes raise STORE watcher
                    // events; a quiet presence check must not query tmux
                    // indirectly through an empty tab/session inventory.
                    if flags & (TABS | SESSIONS | STORE) != 0 || sample_due {
                        refresh_tabs(&worker_tabs, &worker_tabs_hubs, &worker_error);
                    }
                }
            })?;
        Ok(Arc::new(Self {
            home,
            store,
            sessions,
            preferences,
            store_hub,
            sessions_hub,
            tabs,
            tabs_hubs,
            store_refresh,
            sessions_refresh,
            has_worktrees,
            has_shells,
            stop,
            wake,
            worker: Some(worker),
            _watcher: watcher,
            last_error,
        }))
    }

    /// Compatibility constructor: all versions share the same topic coordinator.
    pub fn with_tabs_backend(
        home: PathBuf,
        store: Arc<dyn StoreBackend>,
        sessions: Arc<dyn SessionsBackend>,
        tabs: Arc<dyn TabsBackend>,
    ) -> Result<Arc<Self>> {
        Self::with_all_backends(home, store, sessions, tabs)
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn preferences(&self) -> &Arc<PreferenceService> {
        &self.preferences
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
    } else if path == home.join("settings.json") {
        SETTINGS
    } else if path == home.join("sessions.json")
        || path == home.join("orchestrator-wakes.json")
        || path.starts_with(home.join("agent-activity"))
    {
        SESSIONS
    } else if path.starts_with(home.join("project-tabs"))
        && path
            .extension()
            .is_some_and(|extension| extension == "json")
        || path.starts_with(home.join("chats"))
    {
        TABS
    } else {
        0
    }
}

pub(crate) fn validate_project_id(project: &str) -> Result<(), String> {
    validate_uuid(project)
}

fn refresh_tabs(
    backend: &Arc<dyn TabsBackend>,
    hubs: &Mutex<BTreeMap<String, Arc<Hub<TabsSnapshot>>>>,
    error: &Mutex<Option<String>>,
) {
    let mut hubs = hubs.lock().unwrap_or_else(PoisonError::into_inner);
    hubs.retain(|project, hub| {
        if !hub.has_subscribers() {
            return false;
        }
        match backend.snapshot(project) {
            Ok(snapshot) => hub.publish(snapshot),
            Err(failure) => {
                *error.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some(format!("tabs snapshot: {failure}"))
            }
        }
        true
    });
}
