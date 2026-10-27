//! Blocking domain adapters. The bridge always calls these on its blocking pool.

use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

use anyhow::Result;
use riwork_core::store::Store;
use riwork_engine::sessions::SessionManager;

use crate::dto::{ProjectInspectionDto, SearchHitDto, SessionsSnapshot, StoreState};

/// A required backend/tool is unavailable, as distinct from a failed disk read.
#[derive(Debug)]
pub struct Unavailable(pub String);

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unavailable {}

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
}
