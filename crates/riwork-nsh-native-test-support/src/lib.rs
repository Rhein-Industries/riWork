//! The acceptance probe uses the production registry and adapters with in-memory
//! domain backends. Its home is always a fresh tempfile, never the user's home.
use anyhow::Result;
use nsh_core::prelude::*;
use riwork_nsh_native::{
    ProcessService,
    backend::{SessionsBackend, StoreBackend},
    dto::{FolderDto, ProjectInspectionDto, SearchHitDto, SessionsSnapshot, StoreState},
    tabs::{TabsBackend, TabsSnapshot},
};
use std::{
    path::Path,
    sync::{Arc, Mutex, OnceLock},
};

static FIXTURE: OnceLock<(tempfile::TempDir, Arc<ProcessService>)> = OnceLock::new();

#[native_module("riwork/native")]
fn native(registry: &mut Registry) {
    FIXTURE.get_or_init(|| {
        let home = tempfile::Builder::new()
            .prefix("riwork-native-probe-")
            .tempdir()
            .expect("isolated probe home");
        let service = ProcessService::with_tabs_backend(
            home.path().to_path_buf(),
            Arc::new(MemoryStore::default()),
            Arc::new(MemorySessions),
            Arc::new(MemoryTabs),
        )
        .expect("fake native service");
        riwork_nsh_native::service::install_process_service(service.clone())
            .expect("one probe service");
        (home, service)
    });
    *registry = riwork_nsh_native::registry();
}

#[derive(Default)]
struct MemoryStore(Mutex<StoreState>);
impl StoreBackend for MemoryStore {
    fn snapshot(&self) -> Result<StoreState> {
        Ok(self.0.lock().unwrap().clone())
    }
    fn search(&self, _query: &str) -> Result<Vec<SearchHitDto>> {
        Ok(Vec::new())
    }
    fn inspect_project(&self, root: &Path) -> Result<ProjectInspectionDto> {
        Ok(ProjectInspectionDto {
            root: root.to_string_lossy().into_owned(),
            exists: false,
            repository_roots: Vec::new(),
            repository_count: 0,
            can_init_git: false,
            discovery_complete: true,
            warning: None,
        })
    }
    fn create_folder(&self, name: &str, parent: Option<&str>) -> Result<FolderDto> {
        let folder = FolderDto {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            parent_id: parent.map(str::to_owned),
            created_at: 1,
        };
        self.0.lock().unwrap().project_folders.push(folder.clone());
        Ok(folder)
    }
}
struct MemorySessions;
impl SessionsBackend for MemorySessions {
    fn list(&self) -> Result<SessionsSnapshot> {
        Ok(SessionsSnapshot::default())
    }
}
struct MemoryTabs;
impl TabsBackend for MemoryTabs {
    fn snapshot(&self, _project: &str) -> Result<TabsSnapshot> {
        Ok(TabsSnapshot {
            epoch: "probe".into(),
            revision: 1,
            entries: Vec::new(),
        })
    }
    fn update(
        &self,
        project: &str,
        _update: &riwork_engine::project_tabs::Update,
    ) -> Result<TabsSnapshot> {
        self.snapshot(project)
    }
}
