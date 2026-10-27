//! Blocking domain adapters. The bridge always calls these on its blocking pool.

use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use anyhow::Result;
use riwork_core::store::Store;
use riwork_engine::sessions::SessionManager;

use crate::dto::{
    CreateSessionRequest, FolderDto, ProjectDto, ProjectInspectionDto, ProjectRequest,
    SearchHitDto, SessionsSnapshot, ShellSessionDto, StoreState, TabUpdate, TabsSnapshot, TaskDto,
    WorktreeDto,
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

    /// Periodic fallback and watcher-triggered inventory, once per process.
    fn sample(&self) -> Result<SessionsSnapshot> {
        self.list()
    }

    /// A home-watcher event invalidates the engine's process-wide sample cache.
    fn invalidate_sample(&self) {}
    fn create(&self, _request: CreateSessionRequest) -> Result<ShellSessionDto> {
        unsupported!()
    }
    fn close(&self, _id: &str) -> Result<()> {
        unsupported!()
    }
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
        // The engine reuses its process-wide SessionSample cache here.
        riwork_engine::project_tabs::refresh_snapshot_with_activity(
            &self.home,
            project,
            &Default::default(),
        )
        .map_err(anyhow::Error::msg)?
        .try_into()
    }

    fn update(&self, project: &str, update: TabUpdate) -> Result<TabsSnapshot> {
        let store = riwork_engine::project_tabs::TabStore::at(&self.home, project)
            .map_err(anyhow::Error::msg)?;
        store.update(&update.into()).map_err(anyhow::Error::msg)?;
        store.snapshot().map_err(anyhow::Error::msg)?.try_into()
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
        self.store()?
            .snapshot()
            .map_err(anyhow::Error::msg)?
            .try_into()
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
        self.store()?
            .sync_worktrees(selector)
            .map(|rows| rows.into_iter().map(Into::into).collect())
            .map_err(anyhow::Error::msg)
    }
    fn refresh_worktrees(&self, selector: &str) -> Result<Vec<WorktreeDto>> {
        self.store()?
            .refresh_worktrees(selector)
            .map_err(anyhow::Error::msg)?;
        let state = self.store()?.snapshot().map_err(anyhow::Error::msg)?;
        let project = state.project(selector).map_err(anyhow::Error::msg)?;
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
    fn refresh_presence(&self) -> Result<StoreState> {
        let state = self.store()?.snapshot().map_err(anyhow::Error::msg)?;
        for project in state.projects {
            // Missing/inaccessible projects still need a presence snapshot.
            // Explicit refreshWorktrees reports discovery failures to its
            // caller; a background scan cannot suppress all other projects.
            let _ = self.store()?.refresh_worktrees(&project.id);
        }
        self.snapshot()
    }
}

/// Production session adapter. Its home, and therefore its tmux socket name,
/// are fixed before any window subscribes.
#[derive(Debug)]
pub struct EngineSessionsBackend {
    home: PathBuf,
    manager: OnceLock<SessionManager>,
}

impl EngineSessionsBackend {
    /// Does not launch or query tmux. Missing tmux is reported only when the
    /// sessions service is read, so it cannot prevent store access.
    pub fn new(home: PathBuf) -> Self {
        Self {
            home,
            manager: OnceLock::new(),
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

    fn sample(&self) -> Result<SessionsSnapshot> {
        let sample = self.manager()?.sample(false).map_err(anyhow::Error::msg)?;
        Ok(SessionsSnapshot {
            sessions: sample.shells.into_iter().map(Into::into).collect(),
        })
    }

    fn invalidate_sample(&self) {
        if let Some(manager) = self.manager.get() {
            manager.invalidate_sample();
        }
    }

    fn create(&self, request: CreateSessionRequest) -> Result<ShellSessionDto> {
        self.manager()?
            .for_user()
            .create(
                request.project_id,
                request.worktree_id,
                request.cwd.into(),
                request.command,
            )
            .map(Into::into)
            .map_err(anyhow::Error::msg)
    }

    fn close(&self, id: &str) -> Result<()> {
        self.manager()?.close(id).map_err(anyhow::Error::msg)
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
