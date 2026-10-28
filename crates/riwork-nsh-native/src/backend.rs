//! Blocking domain adapters. The bridge always calls these on its blocking pool.

use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock, PoisonError},
};

use anyhow::Result;
use riwork_core::store::Store;
use riwork_engine::{
    activity::{ActivityTracker, unix_now},
    notifications::record_completion_outcome,
    project_tabs::{TabStore, Update},
    sessions::{HarnessKind, SessionManager, registered_shell},
};
use serde_json::Value;

use crate::dto::{
    CreateSessionRequest, CreateSubshellRequest, FolderDto, OpenEditorRequest, ProjectDto,
    ProjectInspectionDto, ProjectMemoDto, ProjectRequest, SearchHitDto, SessionMetricsSnapshot,
    SessionsSnapshot, ShellMetricsDto, ShellSessionDto, StoreState, TabUpdate, TabsSnapshot,
    TaskDto, WorktreeDto,
};

macro_rules! unsupported {
    () => {
        Err(Unavailable("operation is not supported by this backend".into()).into())
    };
}

/// A required backend/tool is unavailable, as distinct from a failed disk read.
#[derive(Debug)]
pub struct Unavailable(pub String);

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unavailable {}

/// A safe refusal before any process/session side effect.
#[derive(Debug)]
pub struct Refused(pub String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Refused {}

#[derive(Debug)]
pub struct InvalidInput(pub String);
impl std::fmt::Display for InvalidInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for InvalidInput {}

/// The durable project/folder/worktree/task domain adapter.
///
/// Implementations may touch disk or run Git. They must never be invoked on an
/// NSH engine thread. Keeping the interface synchronous makes that boundary
/// explicit and lets headless tests supply a deterministic in-memory backend.
pub trait StoreBackend: Send + Sync + 'static {
    fn snapshot(&self) -> Result<StoreState>;
    fn search(&self, query: &str) -> Result<Vec<SearchHitDto>>;
    fn inspect_project(&self, root: &Path) -> Result<ProjectInspectionDto>;
    fn default_project_directory(&self) -> Result<String> {
        unsupported!()
    }
    fn create_project(&self, _request: ProjectRequest) -> Result<ProjectDto> {
        unsupported!()
    }
    fn create_new_project(&self, _request: ProjectRequest) -> Result<ProjectDto> {
        unsupported!()
    }
    fn add_project(&self, _request: ProjectRequest) -> Result<ProjectDto> {
        unsupported!()
    }
    fn use_project(&self, _selector: &str) -> Result<ProjectDto> {
        unsupported!()
    }
    fn rename_project(&self, _selector: &str, _name: &str) -> Result<ProjectDto> {
        unsupported!()
    }
    fn update_project_metadata(
        &self,
        _selector: &str,
        _name: &str,
        _folder: Option<&str>,
    ) -> Result<ProjectDto> {
        unsupported!()
    }
    fn set_project_codex_account(
        &self,
        _selector: &str,
        _choice: riwork_core::store::ProjectCodexAccount,
    ) -> Result<ProjectDto> {
        unsupported!()
    }
    fn set_project_orchestrator(
        &self,
        _selector: &str,
        _choice: riwork_core::store::ProjectOrchestrator,
    ) -> Result<ProjectDto> {
        unsupported!()
    }
    fn set_project_delegation_policy(
        &self,
        _selector: &str,
        _patch: crate::settings::DelegationPatch,
    ) -> Result<ProjectDto> {
        unsupported!()
    }
    fn set_project_memo(&self, _selector: &str, _text: &str) -> Result<ProjectMemoDto> {
        unsupported!()
    }
    fn set_project_notifications(&self, _selector: &str, _enabled: bool) -> Result<ProjectDto> {
        unsupported!()
    }
    fn set_project_orchestrator_wakes(
        &self,
        _selector: &str,
        _enabled: bool,
    ) -> Result<ProjectDto> {
        unsupported!()
    }
    fn create_folder(&self, _name: &str, _parent: Option<&str>) -> Result<FolderDto> {
        unsupported!()
    }
    fn rename_folder(&self, _selector: &str, _name: &str) -> Result<FolderDto> {
        unsupported!()
    }
    fn move_folder(&self, _selector: &str, _parent: Option<&str>) -> Result<FolderDto> {
        unsupported!()
    }
    fn remove_folder(&self, _selector: &str) -> Result<()> {
        unsupported!()
    }
    fn move_project_to_folder(&self, _selector: &str, _folder: Option<&str>) -> Result<ProjectDto> {
        unsupported!()
    }
    fn sync_worktrees(&self, _selector: &str) -> Result<Vec<WorktreeDto>> {
        unsupported!()
    }
    fn refresh_worktrees(&self, _selector: &str) -> Result<Vec<WorktreeDto>> {
        unsupported!()
    }
    fn add_task(&self, _project: &str, _title: &str, _details: &str) -> Result<TaskDto> {
        unsupported!()
    }
    fn assign_tasks(&self, _worktree: &str, _tasks: &[String]) -> Result<Vec<TaskDto>> {
        unsupported!()
    }
    fn unassign_tasks(&self, _tasks: &[String]) -> Result<Vec<TaskDto>> {
        unsupported!()
    }
    fn set_task_status(&self, _selector: &str, _status: &str) -> Result<TaskDto> {
        unsupported!()
    }
    fn refresh_presence(&self) -> Result<StoreState> {
        self.snapshot()
    }
}

/// Session inventory and explicit creation/close, shared by the process service.
pub trait SessionsBackend: Send + Sync + 'static {
    fn list(&self) -> Result<SessionsSnapshot>;

    fn create(&self, _request: CreateSessionRequest) -> Result<ShellSessionDto> {
        Err(Unavailable("shell creation is unavailable in this backend".into()).into())
    }

    fn create_harness(&self, _request: CreateSessionRequest, _harness: HarnessKind) -> Result<ShellSessionDto> {
        unsupported!()
    }

    fn create_subshell(&self, _request: CreateSubshellRequest) -> Result<ShellSessionDto> {
        unsupported!()
    }

    fn open_editor(&self, _request: OpenEditorRequest) -> Result<ShellSessionDto> {
        unsupported!()
    }

    fn close(&self, _id: &str) -> Result<()> {
        Err(Unavailable("shell exit is unavailable in this backend".into()).into())
    }

    fn update_tabs(&self, _project_id: &str, _update: &Update) -> Result<Value> {
        Err(Unavailable("shared session membership is unavailable in this backend".into()).into())
    }

    /// Periodic fallback and watcher-triggered inventory, once per process.
    fn sample(&self) -> Result<SessionsSnapshot> {
        self.list()
    }

    /// CPU and memory of the live shells; costs a `ps` run, so only taken
    /// while a window shows them.
    fn metrics(&self) -> Result<SessionMetricsSnapshot> {
        Ok(SessionMetricsSnapshot::default())
    }

    /// A home-watcher event invalidates the engine's process-wide sample cache.
    fn invalidate_sample(&self) {}
}

/// Shared membership/order/title inventory. Implementations may block on disk,
/// engine inventory and the project tab lock, so every call uses the pool.
pub trait TabsBackend: Send + Sync + 'static {
    fn snapshot(&self, project: &str) -> Result<TabsSnapshot>;
    fn update(&self, project: &str, update: TabUpdate) -> Result<TabsSnapshot>;
}

pub struct EngineTabsBackend {
    home: PathBuf,
}

impl EngineTabsBackend {
    pub fn new(home: PathBuf) -> Self {
        Self { home }
    }
}

impl TabsBackend for EngineTabsBackend {
    fn snapshot(&self, project: &str) -> Result<TabsSnapshot> {
        // The engine reuses its process-wide SessionSample cache here. A debug
        // fixture home shares the layout sessions' fake inventory instead.
        riwork_nsh_workspace::session_io::tab_snapshot(&self.home, project)
            .map_err(anyhow::Error::msg)?
            .try_into()
    }

    fn update(&self, project: &str, update: TabUpdate) -> Result<TabsSnapshot> {
        let update: Update = update.into();
        update.validate().map_err(InvalidInput)?;
        // Panels can update all supported tab kinds. Local-shell membership
        // retains the terminal adapter's registered project identity check.
        if let Some(id) = update.key().strip_prefix("shell:") {
            let shell = registered_shell(&self.home, id).map_err(anyhow::Error::msg)?;
            if shell.project_id.as_deref() != Some(project) {
                return Err(Refused("session does not belong to this project".into()).into());
            }
        }
        let store = TabStore::at(&self.home, project).map_err(anyhow::Error::msg)?;
        store.update(&update).map_err(Unavailable)?;
        store.snapshot().map_err(Unavailable)?.try_into()
    }
}

/// Production store adapter bound to the home selected by the host once.
///
/// Construction is lazy: opening an inventory subscription never causes domain
/// I/O on an engine thread. No call re-reads `RIWORK_HOME` from the environment.
#[derive(Debug)]
pub struct CoreStoreBackend {
    home: PathBuf,
    store: OnceLock<Store>,
    default_directory: Option<PathBuf>,
}

impl CoreStoreBackend {
    pub fn new(home: PathBuf) -> Self {
        Self {
            home,
            store: OnceLock::new(),
            default_directory: None,
        }
    }

    /// Isolated host/test configuration, never supplied by JavaScript.
    pub fn with_default_project_directory(home: PathBuf, directory: PathBuf) -> Self {
        Self {
            home,
            store: OnceLock::new(),
            default_directory: Some(directory),
        }
    }

    fn store(&self) -> Result<&Store> {
        if let Some(store) = self.store.get() {
            return Ok(store);
        }
        // Failed construction is not cached: permission or filesystem recovery
        // must be visible to the next read. Concurrent successful opens refer
        // to the same fixed home and share the core's parse cache.
        let store = Store::open(self.home.clone()).map_err(anyhow::Error::msg)?;
        Ok(self.store.get_or_init(|| store))
    }
}

impl StoreBackend for CoreStoreBackend {
    fn snapshot(&self) -> Result<StoreState> {
        let mut state: StoreState = self.store()?
            .snapshot()
            .map_err(anyhow::Error::msg)?
            .try_into()?;
        if let Some(cached) = riwork_core::recency_file::read(&self.home) {
            state.project_recency = crate::dto::StringMap(cached.projects.into_iter()
                .filter(|(id, _)| state.projects.iter().any(|project| &project.id == id))
                .collect());
        }
        Ok(state)
    }

    fn search(&self, query: &str) -> Result<Vec<SearchHitDto>> {
        self.store()?
            .snapshot()
            .map_err(anyhow::Error::msg)?
            .search(query)
            .into_iter()
            .map(TryInto::try_into)
            .collect()
    }

    fn inspect_project(&self, root: &Path) -> Result<ProjectInspectionDto> {
        let root = resolve_folder(&root.to_string_lossy(), std::env::var_os("HOME").as_deref())
            .map_err(InvalidInput)?;
        Store::inspect_project(root)
            .map(Into::into)
            .map_err(anyhow::Error::msg)
    }

    fn default_project_directory(&self) -> Result<String> {
        let directory = if let Some(directory) = &self.default_directory {
            fs::create_dir_all(directory)?;
            directory.clone()
        } else {
            riwork_core::paths::ensure_default_projects_directory().map_err(anyhow::Error::msg)?
        };
        Ok(directory.to_string_lossy().into_owned())
    }
    fn create_project(&self, request: ProjectRequest) -> Result<ProjectDto> {
        let root = resolve_folder(&request.root, std::env::var_os("HOME").as_deref())
            .map_err(InvalidInput)?;
        let created = missing_directories(&root);
        self.store()?
            .create_project(
                &root,
                request.name.as_deref(),
                request.init_git.unwrap_or(true),
            )
            .inspect_err(|_| remove_created_directories(&created))
            .map_err(anyhow::Error::msg)?
            .try_into()
    }
    fn create_new_project(&self, request: ProjectRequest) -> Result<ProjectDto> {
        let root = resolve_folder(&request.root, std::env::var_os("HOME").as_deref())
            .map_err(InvalidInput)?;
        let created = missing_directories(&root);
        self.store()?
            .create_new_project(
                &root,
                request.name.as_deref(),
                request.init_git.unwrap_or(true),
            )
            .inspect_err(|_| remove_created_directories(&created))
            .map_err(anyhow::Error::msg)?
            .try_into()
    }
    fn add_project(&self, request: ProjectRequest) -> Result<ProjectDto> {
        let root = resolve_folder(&request.root, std::env::var_os("HOME").as_deref())
            .map_err(InvalidInput)?;
        self.store()?
            .add_project(&root, request.name.as_deref())
            .map_err(anyhow::Error::msg)?
            .try_into()
    }
    fn use_project(&self, selector: &str) -> Result<ProjectDto> {
        self.store()?
            .use_project(selector)
            .map_err(anyhow::Error::msg)?
            .try_into()
    }
    fn rename_project(&self, selector: &str, name: &str) -> Result<ProjectDto> {
        self.store()?
            .rename_project(selector, name)
            .map_err(anyhow::Error::msg)?
            .try_into()
    }
    fn update_project_metadata(
        &self,
        selector: &str,
        name: &str,
        folder: Option<&str>,
    ) -> Result<ProjectDto> {
        self.store()?
            .update_project_metadata(selector, name, folder)
            .map_err(Refused)?
            .try_into()
    }
    fn set_project_codex_account(
        &self,
        selector: &str,
        choice: riwork_core::store::ProjectCodexAccount,
    ) -> Result<ProjectDto> {
        self.store()?
            .set_project_codex_account(selector, choice)
            .map_err(Refused)?
            .try_into()
    }
    fn set_project_orchestrator(
        &self,
        selector: &str,
        choice: riwork_core::store::ProjectOrchestrator,
    ) -> Result<ProjectDto> {
        self.store()?
            .set_project_orchestrator(selector, choice)
            .map_err(Refused)?
            .try_into()
    }
    fn set_project_delegation_policy(
        &self,
        selector: &str,
        patch: crate::settings::DelegationPatch,
    ) -> Result<ProjectDto> {
        patch.validate().map_err(InvalidInput)?;
        self.store()?
            .set_project_delegation_policy(selector, move |policy| patch.apply(policy))
            .map_err(Refused)?
            .try_into()
    }
    fn set_project_memo(&self, selector: &str, text: &str) -> Result<ProjectMemoDto> {
        self.store()?
            .set_project_memo(selector, text)
            .map(|(_, memo)| memo.into())
            .map_err(anyhow::Error::msg)
    }
    fn set_project_notifications(&self, selector: &str, enabled: bool) -> Result<ProjectDto> {
        self.store()?
            .set_project_notifications(selector, enabled)
            .map_err(anyhow::Error::msg)?
            .try_into()
    }
    fn set_project_orchestrator_wakes(&self, selector: &str, enabled: bool) -> Result<ProjectDto> {
        self.store()?
            .set_project_orchestrator_wakes(selector, enabled)
            .map_err(anyhow::Error::msg)?
            .try_into()
    }
    fn create_folder(&self, name: &str, parent: Option<&str>) -> Result<FolderDto> {
        self.store()?
            .create_project_folder_in(name, parent)
            .map(Into::into)
            .map_err(anyhow::Error::msg)
    }
    fn rename_folder(&self, selector: &str, name: &str) -> Result<FolderDto> {
        self.store()?
            .rename_project_folder(selector, name)
            .map(Into::into)
            .map_err(anyhow::Error::msg)
    }
    fn move_folder(&self, selector: &str, parent: Option<&str>) -> Result<FolderDto> {
        self.store()?
            .move_project_folder(selector, parent)
            .map(Into::into)
            .map_err(anyhow::Error::msg)
    }
    fn remove_folder(&self, selector: &str) -> Result<()> {
        self.store()?
            .remove_project_folder(selector)
            .map_err(anyhow::Error::msg)
    }
    fn move_project_to_folder(&self, selector: &str, folder: Option<&str>) -> Result<ProjectDto> {
        self.store()?
            .move_project_to_folder(selector, folder)
            .map_err(anyhow::Error::msg)?
            .try_into()
    }
    fn sync_worktrees(&self, selector: &str) -> Result<Vec<WorktreeDto>> {
        let rows = self.store()?
            .sync_worktrees(selector)
            .map_err(anyhow::Error::msg)?;
        crate::presence::refresh(rows.iter().map(|row| row.path.as_path()));
        Ok(rows.into_iter().map(Into::into).collect())
    }
    fn refresh_worktrees(&self, selector: &str) -> Result<Vec<WorktreeDto>> {
        self.store()?
            .refresh_worktrees(selector)
            .map_err(anyhow::Error::msg)?;
        let state = self.store()?.snapshot().map_err(anyhow::Error::msg)?;
        let project = state.project(selector).map_err(anyhow::Error::msg)?;
        crate::presence::refresh(state.worktrees_for(&project.id).into_iter().map(|row| row.path.as_path()));
        Ok(state
            .worktrees_for(&project.id)
            .into_iter()
            .cloned()
            .map(Into::into)
            .collect())
    }
    fn add_task(&self, project: &str, title: &str, details: &str) -> Result<TaskDto> {
        self.store()?
            .add_task(project, title, details)
            .map(Into::into)
            .map_err(anyhow::Error::msg)
    }
    fn assign_tasks(&self, worktree: &str, tasks: &[String]) -> Result<Vec<TaskDto>> {
        self.store()?
            .assign_tasks(worktree, tasks)
            .map(|rows| rows.into_iter().map(Into::into).collect())
            .map_err(anyhow::Error::msg)
    }
    fn unassign_tasks(&self, tasks: &[String]) -> Result<Vec<TaskDto>> {
        self.store()?
            .unassign_tasks(tasks)
            .map(|rows| rows.into_iter().map(Into::into).collect())
            .map_err(anyhow::Error::msg)
    }
    fn set_task_status(&self, selector: &str, status: &str) -> Result<TaskDto> {
        let status = match status {
            "todo" => riwork_core::store::TaskStatus::Todo,
            "in_progress" => riwork_core::store::TaskStatus::InProgress,
            "done" => riwork_core::store::TaskStatus::Done,
            _ => anyhow::bail!("Unknown task status; use todo, in_progress, or done"),
        };
        self.store()?
            .set_task_status(selector, status)
            .map(Into::into)
            .map_err(anyhow::Error::msg)
    }
}

/// Production session adapter. Its home, and therefore its tmux socket name,
/// are fixed before any window subscribes.
pub struct EngineSessionsBackend {
    home: PathBuf,
    manager: OnceLock<SessionManager>,
    /// Kept across samples: it reads Codex rollouts incrementally and notices
    /// finished turns, which become completion alerts and orchestrator wakes.
    activity: Mutex<Option<ActivityTracker>>,
}

impl std::fmt::Debug for EngineSessionsBackend {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EngineSessionsBackend")
            .field("home", &self.home)
            .finish_non_exhaustive()
    }
}

impl EngineSessionsBackend {
    /// Does not launch or query tmux. Missing tmux is reported only when the
    /// sessions service is read, so it cannot prevent store access.
    pub fn new(home: PathBuf) -> Self {
        Self {
            home,
            manager: OnceLock::new(),
            activity: Mutex::new(None),
        }
    }

    /// As every GPUI window's sample did, once per process here: record the
    /// outcome of each Codex turn that finished since the last sample. The
    /// ledger dedupes by event id across processes; opted-out projects add
    /// nothing. Claude, Grok and Muse report completions through their hooks.
    fn record_codex_completions(&self, shells: &[riwork_engine::sessions::ShellSession]) {
        let mut tracker = self.activity.lock().unwrap_or_else(PoisonError::into_inner);
        let tracker = tracker.get_or_insert_with(|| ActivityTracker::at(self.home.clone()));
        tracker.sample_states(shells, unix_now());
        for completion in tracker.take_completions() {
            if let Err(error) = record_completion_outcome(
                &self.home,
                &completion.shell_id,
                &completion.event_id,
                HarnessKind::Codex,
                completion.outcome,
            ) {
                eprintln!("riwork notifications: {error}");
            }
        }
    }

    fn manager(&self) -> Result<&SessionManager> {
        if let Some(manager) = self.manager.get() {
            return Ok(manager);
        }
        // For example, installing tmux after a failed read can recover without
        // restarting the bridge. Only a successful manager is retained.
        let manager = SessionManager::at(self.home.clone()).map_err(Unavailable)?;
        Ok(self.manager.get_or_init(|| manager))
    }

    fn project_root(&self, project_id: &str, worktree_id: Option<&str>) -> Result<PathBuf> {
        let state = Store::open(self.home.clone())
            .map_err(anyhow::Error::msg)?
            .snapshot()
            .map_err(anyhow::Error::msg)?;
        let project = state
            .projects
            .iter()
            .find(|project| project.id == project_id)
            .ok_or_else(|| Refused("unknown project".into()))?;
        if let Some(id) = worktree_id {
            state
                .worktrees
                .iter()
                .find(|tree| tree.id == id && tree.project_id == project_id)
                .map(|tree| tree.path.clone())
                .ok_or_else(|| Refused("worktree does not belong to project".into()).into())
        } else {
            Ok(project.root.clone())
        }
    }
}

impl SessionsBackend for EngineSessionsBackend {
    fn list(&self) -> Result<SessionsSnapshot> {
        let (sessions, _activity) = self
            .manager()?
            .list_with_activity()
            .map_err(anyhow::Error::msg)?;
        Ok(SessionsSnapshot {
            sessions: sessions.into_iter().map(Into::into).collect(),
        })
    }

    fn create(&self, request: CreateSessionRequest) -> Result<ShellSessionDto> {
        let state = Store::open(self.home.clone())
            .map_err(anyhow::Error::msg)?
            .snapshot()
            .map_err(anyhow::Error::msg)?;
        let project = state
            .projects
            .iter()
            .find(|project| project.id == request.project_id)
            .ok_or_else(|| anyhow::anyhow!("unknown project {}", request.project_id))?;
        let root = if let Some(id) = request.worktree_id.as_deref() {
            state
                .worktrees
                .iter()
                .find(|worktree| worktree.id == id && worktree.project_id == project.id)
                .map(|worktree| worktree.path.clone())
                .ok_or_else(|| anyhow::anyhow!("worktree does not belong to project"))?
        } else {
            project.root.clone()
        };
        let cwd = request.cwd.map(PathBuf::from).unwrap_or(root);
        if !cwd.is_absolute() || !cwd.is_dir() {
            anyhow::bail!("shell cwd must be an existing absolute directory");
        }
        self.manager()?
            .for_user()
            .create(
                request.project_id,
                request.worktree_id,
                cwd,
                request.command,
            )
            .map(Into::into)
            .map_err(anyhow::Error::msg)
    }

    fn create_harness(&self, request: CreateSessionRequest, harness: HarnessKind) -> Result<ShellSessionDto> {
        if request.command.is_some() {
            return Err(InvalidInput("a harness cannot also have a custom command".into()).into());
        }
        let root = self.project_root(&request.project_id, request.worktree_id.as_deref())?;
        let cwd = request.cwd.map(PathBuf::from).unwrap_or(root);
        if !cwd.is_absolute() || !cwd.is_dir() {
            return Err(InvalidInput("agent cwd must be an existing absolute directory".into()).into());
        }
        let unrestricted = riwork_core::settings::agent_terminals_unrestricted(&self.home);
        self.manager()?
            .for_user()
            .create_harness(request.project_id, request.worktree_id, cwd, harness, unrestricted)
            .map(Into::into)
            .map_err(anyhow::Error::msg)
    }

    fn close(&self, id: &str) -> Result<()> {
        let session = registered_shell(&self.home, id).map_err(anyhow::Error::msg)?;
        if let Some(project) = session.project_id {
            self.update_tabs(
                &project,
                &Update::Hide {
                    key: format!("shell:{id}"),
                },
            )
            .map_err(|error| {
                Refused(format!(
                    "cannot hide the shared session; Exit was not performed: {error}"
                ))
            })?;
        }
        self.manager()?.close(id).map_err(anyhow::Error::msg)
    }

    fn create_subshell(&self, request: CreateSubshellRequest) -> Result<ShellSessionDto> {
        let parent =
            registered_shell(&self.home, &request.parent_shell_id).map_err(anyhow::Error::msg)?;
        if parent.project_id.as_deref() != Some(&request.project_id) {
            return Err(Refused("parent shell does not belong to this project".into()).into());
        }
        self.project_root(&request.project_id, parent.worktree_id.as_deref())?;
        self.manager()?
            .create_subshell(request.project_id, &request.parent_shell_id)
            .map(Into::into)
            .map_err(anyhow::Error::msg)
    }

    fn open_editor(&self, request: OpenEditorRequest) -> Result<ShellSessionDto> {
        use riwork_engine::{
            file_preview::{self, FileIdentity},
            session_keys::{Item, Key},
        };
        let root = self.project_root(&request.project_id, request.worktree_id.as_deref())?;
        let requested = Path::new(&request.path);
        let requested = if requested.is_absolute() {
            requested.to_owned()
        } else {
            root.join(requested)
        };
        let path = file_preview::validated_live_editor_path(&root, &requested)
            .map_err(|error| Refused(error))?;
        let selected_identity = request
            .identity
            .as_deref()
            .map(crate::files::decode_identity)
            .transpose()
            .map_err(Refused)?;
        let manager = self.manager()?.for_user();
        if let Some(editor) = manager
            .list()
            .map_err(anyhow::Error::msg)?
            .into_iter()
            .find(|shell| {
                shell.alive
                    && shell.project_id.as_deref() == Some(&request.project_id)
                    && shell.editor_path.as_deref() == Some(path.as_path())
            })
        {
            if let Some(line) = request.line.filter(|line| *line > 0) {
                manager
                    .send_keys(
                        &editor.id,
                        &[
                            Item::Key(Key::Escape),
                            Item::Text(format!(":{line}")),
                            Item::Key(Key::Enter),
                        ],
                    )
                    .map_err(anyhow::Error::msg)?;
            }
            return Ok(editor.into());
        }
        // Capture identity at selection/open time and revalidate inside the
        // engine launch. A replacement between validation and launch is refused.
        let expected = match selected_identity {
            Some(identity) => identity,
            None => FileIdentity::of(&fs::symlink_metadata(&path)?),
        };
        manager
            .create_editor(
                request.project_id,
                request.worktree_id,
                root,
                requested,
                expected,
                request.line,
            )
            .map(Into::into)
            .map_err(anyhow::Error::msg)
    }

    fn update_tabs(&self, project_id: &str, update: &Update) -> Result<Value> {
        update.validate().map_err(anyhow::Error::msg)?;
        let id = update.key().strip_prefix("shell:").ok_or_else(|| {
            Refused("only local shell membership is available in this slice".into())
        })?;
        let shell = registered_shell(&self.home, id).map_err(anyhow::Error::msg)?;
        if shell.project_id.as_deref() != Some(project_id) {
            return Err(Refused("session does not belong to this project".into()).into());
        }
        let store = TabStore::at(&self.home, project_id).map_err(anyhow::Error::msg)?;
        let entries = store.update(update).map_err(Unavailable)?;
        Ok(Value::Array(entries.iter().map(tab_entry).collect()))
    }

    fn sample(&self) -> Result<SessionsSnapshot> {
        let sample = self.manager()?.sample(false).map_err(anyhow::Error::msg)?;
        self.record_codex_completions(&sample.shells);
        Ok(SessionsSnapshot {
            sessions: sample.shells.into_iter().map(Into::into).collect(),
        })
    }

    fn metrics(&self) -> Result<SessionMetricsSnapshot> {
        // Within the cache's lifetime this adds metrics to the sample just taken.
        let sample = self.manager()?.sample(true).map_err(anyhow::Error::msg)?;
        let metrics = sample
            .metrics
            .ok_or_else(|| anyhow::anyhow!("shell metrics are unavailable"))?;
        Ok(SessionMetricsSnapshot {
            metrics: metrics
                .into_iter()
                .map(|(id, metrics)| ShellMetricsDto {
                    id,
                    cpu_percent: f64::from(metrics.cpu_percent),
                    ram_bytes: metrics.ram_bytes,
                })
                .collect(),
        })
    }

    fn invalidate_sample(&self) {
        if let Some(manager) = self.manager.get() {
            manager.invalidate_sample();
        }
    }
}

fn resolve_folder(text: &str, home: Option<&OsStr>) -> std::result::Result<PathBuf, String> {
    let text = text.trim();
    let expand = |tail: &str| {
        let home = home
            .filter(|home| !home.is_empty())
            .ok_or("Cannot expand ~: HOME is not set. Enter an absolute folder path.")?;
        Ok::<_, String>(PathBuf::from(home).join(tail))
    };
    let path = if text == "~" {
        expand("")?
    } else if let Some(tail) = text.strip_prefix("~/") {
        expand(tail)?
    } else {
        PathBuf::from(text)
    };
    if !path.is_absolute() {
        return Err("Use an absolute folder path, starting with / or ~/".into());
    }
    if path
        .components()
        .any(|part| part.as_os_str().eq_ignore_ascii_case(".git"))
    {
        return Err("A project folder cannot be named .git or live inside it".into());
    }
    Ok(path)
}

fn missing_directories(root: &Path) -> Vec<PathBuf> {
    root.ancestors().take_while(|directory| matches!(fs::symlink_metadata(directory), Err(error) if error.kind() == std::io::ErrorKind::NotFound)).map(Path::to_path_buf).collect()
}

fn remove_created_directories(created: &[PathBuf]) {
    for directory in created {
        if fs::remove_dir(directory).is_err() {
            break;
        }
    }
}

/// Only bridge field names change; opaque identities and core files do not.
fn tab_entry(entry: &riwork_engine::project_tabs::Entry) -> Value {
    serde_json::json!({ "key": entry.key, "kind": entry.kind, "title": entry.title,
        "baseTitle": entry.base_title, "titlePriority": entry.title_priority,
        "status": entry.status, "hidden": entry.hidden, "worker": entry.worker,
        "legacyUserOpened": entry.legacy_user_opened, "legacyDismissed": entry.legacy_dismissed,
        "order": entry.order, "parent": entry.parent,
        "children": entry.children.iter().map(tab_entry).collect::<Vec<_>>(),
        "childCount": entry.child_count, "rename": entry.rename, "created": entry.created })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_store_initialization_can_recover_in_the_same_adapter() {
        let isolated = tempfile::tempdir().unwrap();
        let home = isolated.path().join("RIWORK_HOME");
        std::fs::write(&home, "not a directory").unwrap();
        let backend = CoreStoreBackend::new(home.clone());
        assert!(backend.snapshot().is_err());
        std::fs::remove_file(&home).unwrap();
        let recovered = backend.snapshot().unwrap();
        assert_eq!(recovered.schema_version, 1);
        assert!(recovered.projects.is_empty());
    }

    #[test]
    fn vim_refuses_outside_and_symlink_files_before_initializing_tmux() {
        let isolated = tempfile::tempdir().unwrap();
        let home = isolated.path().join("home");
        let root = isolated.path().join("project");
        fs::create_dir_all(&root).unwrap();
        let store = CoreStoreBackend::new(home.clone());
        let project = store
            .create_project(ProjectRequest {
                root: root.to_string_lossy().into_owned(),
                name: None,
                init_git: Some(false),
            })
            .unwrap();
        let outside = isolated.path().join("outside.txt");
        fs::write(&outside, "outside").unwrap();
        let link = root.join("linked.txt");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let backend = EngineSessionsBackend::new(home);
        for path in [outside, link, root.join("missing.txt")] {
            let error = backend
                .open_editor(OpenEditorRequest {
                    project_id: project.id.clone(),
                    worktree_id: None,
                    path: path.to_string_lossy().into_owned(),
                    line: None,
                    identity: None,
                })
                .unwrap_err();
            assert!(error.is::<Refused>(), "{error}");
            assert!(
                backend.manager.get().is_none(),
                "invalid files must not launch/query tmux"
            );
        }
    }

    #[test]
    fn unreadable_membership_refuses_exit_before_initializing_tmux_or_changing_registry() {
        let fixture = tempfile::tempdir().unwrap();
        let id = "00000000-0000-4000-8000-000000000001";
        let project = "00000000-0000-4000-8000-000000000002";
        let registry = serde_json::json!({ "sessions": [{ "id": id, "project_id": project,
            "kind": "project", "cwd": fixture.path(), "created_at_unix": 1 }] })
        .to_string();
        std::fs::write(fixture.path().join("sessions.json"), &registry).unwrap();
        let tabs_dir = fixture.path().join("project-tabs");
        std::fs::create_dir(&tabs_dir).unwrap();
        let tabs_file = tabs_dir.join(format!("{project}.json"));
        std::fs::write(&tabs_file, "unreadable membership").unwrap();
        let backend = EngineSessionsBackend::new(fixture.path().to_owned());
        let error = backend.close(id).unwrap_err();
        assert!(error.is::<Refused>());
        assert!(
            backend.manager.get().is_none(),
            "no tmux query/kill before successful hide"
        );
        assert_eq!(
            std::fs::read_to_string(fixture.path().join("sessions.json")).unwrap(),
            registry
        );
        assert_eq!(
            std::fs::read_to_string(tabs_file).unwrap(),
            "unreadable membership"
        );
        assert!(
            backend
                .update_tabs(
                    project,
                    &Update::Hide {
                        key: format!("shell:{id}")
                    }
                )
                .unwrap_err()
                .is::<Unavailable>()
        );
        // The unified typed Tabs surface preserves legacy local Detach's
        // unavailable classification as well as Exit's refusal above.
        assert!(
            EngineTabsBackend::new(fixture.path().to_owned())
                .update(
                    project,
                    TabUpdate::Hide {
                        key: format!("shell:{id}")
                    }
                )
                .unwrap_err()
                .is::<Unavailable>()
        );
    }

    #[test]
    fn creator_resolves_home_requires_absolute_and_rejects_git_metadata() {
        assert_eq!(
            resolve_folder(" ~/work/app ", Some(OsStr::new("/fixture"))).unwrap(),
            PathBuf::from("/fixture/work/app")
        );
        for path in ["app", "./app", "../app", "~someone/app", ""] {
            assert!(resolve_folder(path, None).is_err());
        }
        for path in ["/fixture/.git", "/fixture/.GIT/hooks", "~/.git/app"] {
            assert!(
                resolve_folder(path, Some(OsStr::new("/fixture")))
                    .unwrap_err()
                    .contains(".git")
            );
        }
        assert!(resolve_folder("~/app", None).is_err());
        assert!(resolve_folder("/fixture/.github", None).is_ok());
    }

    #[test]
    fn creator_failure_cleans_only_new_empty_directories() {
        let fixture = tempfile::tempdir().unwrap();
        let home = fixture.path().join("state");
        let backend = CoreStoreBackend::new(home.clone());
        backend.snapshot().unwrap();
        fs::write(home.join("state.json"), "not json").unwrap();
        let request = |path: &Path, git| ProjectRequest {
            root: path.to_string_lossy().into_owned(),
            name: None,
            init_git: Some(git),
        };
        let root = fixture.path().join("empty/a/b");
        assert!(backend.create_project(request(&root, false)).is_err());
        assert!(!fixture.path().join("empty").exists());
        let existing = fixture.path().join("existing");
        fs::create_dir(&existing).unwrap();
        assert!(backend.create_project(request(&existing, false)).is_err());
        assert!(existing.is_dir());
        let git = fixture.path().join("filled/a");
        assert!(backend.create_project(request(&git, true)).is_err());
        assert!(git.join(".git").is_dir());
    }

    #[test]
    fn default_directory_is_created_only_inside_injected_fixture() {
        let fixture = tempfile::tempdir().unwrap();
        let directory = fixture.path().join("Documents/riwork");
        let backend = CoreStoreBackend::with_default_project_directory(
            fixture.path().join("state"),
            directory.clone(),
        );
        assert!(!directory.exists());
        assert_eq!(
            backend.default_project_directory().unwrap(),
            directory.to_string_lossy()
        );
        assert!(directory.is_dir());
    }
}
