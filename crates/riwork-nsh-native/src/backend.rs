//! Blocking domain adapters. The bridge always calls these on its blocking pool.

use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

use anyhow::Result;
use riwork_core::store::Store;
use riwork_engine::{
    project_tabs::{TabStore, Update},
    sessions::{SessionManager, registered_shell},
};
use serde_json::Value;

use crate::dto::{
    CreateSessionRequest, ProjectInspectionDto, SearchHitDto, SessionsSnapshot, ShellSessionDto,
    StoreState,
};

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

/// The read side of the durable project/worktree/task store.
///
/// Implementations may touch disk or run Git. They must never be invoked on an
/// NSH engine thread. Keeping the interface synchronous makes that boundary
/// explicit and lets headless tests supply a deterministic in-memory backend.
pub trait StoreBackend: Send + Sync + 'static {
    fn snapshot(&self) -> Result<StoreState>;
    fn search(&self, query: &str) -> Result<Vec<SearchHitDto>>;
    fn inspect_project(&self, root: &Path) -> Result<ProjectInspectionDto>;
}

/// Read side of the session inventory, shared by the process service.
pub trait SessionsBackend: Send + Sync + 'static {
    fn list(&self) -> Result<SessionsSnapshot>;

    fn create(&self, _request: CreateSessionRequest) -> Result<ShellSessionDto> {
        Err(Unavailable("shell creation is unavailable in this backend".into()).into())
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

    /// A home-watcher event invalidates the engine's process-wide sample cache.
    fn invalidate_sample(&self) {}
}

/// Production store adapter bound to the home selected by the host once.
///
/// Construction is lazy: opening an inventory subscription never causes domain
/// I/O on an engine thread. No call re-reads `RIWORK_HOME` from the environment.
#[derive(Debug)]
pub struct CoreStoreBackend {
    home: PathBuf,
    store: OnceLock<Store>,
}

impl CoreStoreBackend {
    pub fn new(home: PathBuf) -> Self {
        Self {
            home,
            store: OnceLock::new(),
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
        Store::inspect_project(root)
            .map(Into::into)
            .map_err(anyhow::Error::msg)
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
        Ok(SessionsSnapshot {
            sessions: sample.shells.into_iter().map(Into::into).collect(),
        })
    }

    fn invalidate_sample(&self) {
        if let Some(manager) = self.manager.get() {
            manager.invalidate_sample();
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
    }
}
