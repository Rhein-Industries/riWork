use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::ThreadId,
    time::Duration,
};

use anyhow::{Result, bail};
use riwork_nsh_native::{
    ProcessService,
    backend::{CoreStoreBackend, SessionsBackend, StoreBackend, Unavailable},
    dto::{ProjectInspectionDto, SearchHitDto, SessionsSnapshot, ShellSessionDto, StoreState},
    service::SESSION_SAMPLE_INTERVAL,
};

#[derive(Default)]
struct FakeSessions {
    value: Mutex<SessionsSnapshot>,
    samples: AtomicUsize,
    invalidations: AtomicUsize,
    unavailable: AtomicBool,
}

impl SessionsBackend for FakeSessions {
    fn list(&self) -> Result<SessionsSnapshot> {
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(Unavailable("tmux unavailable in fixture".into()).into());
        }
        Ok(self.value.lock().unwrap().clone())
    }
    fn sample(&self) -> Result<SessionsSnapshot> {
        self.samples.fetch_add(1, Ordering::SeqCst);
        self.list()
    }
    fn invalidate_sample(&self) {
        self.invalidations.fetch_add(1, Ordering::SeqCst);
    }
}

fn session() -> ShellSessionDto {
    let saved: riwork_engine::sessions::ShellSession = serde_json::from_value(serde_json::json!({
        "id": "00000000-0000-4000-8000-000000000001", "kind": "project",
        "cwd": "/isolated/fixture", "created_at_unix": 1
    }))
    .unwrap();
    saved.into()
}

async fn start(home: &Path, sessions: Arc<dyn SessionsBackend>) -> Arc<ProcessService> {
    let home = home.to_path_buf();
    tokio::task::spawn_blocking(move || {
        ProcessService::with_backends(
            home.clone(),
            Arc::new(CoreStoreBackend::new(home)),
            sessions,
        )
    })
    .await
    .unwrap()
    .unwrap()
}

#[tokio::test]
async fn one_sampler_serves_multiple_subscribers_and_emits_only_changes() {
    let fixture = tempfile::tempdir().unwrap();
    let sessions = Arc::new(FakeSessions::default());
    let service = start(fixture.path(), sessions.clone()).await;
    let a = service.subscribe_sessions().await.unwrap();
    let b = service.subscribe_sessions().await.unwrap();
    assert_eq!(a.recv().await.unwrap().rev, 1);
    assert_eq!(b.recv().await.unwrap().rev, 1);
    assert_eq!(sessions.samples.load(Ordering::SeqCst), 1);
    sessions.value.lock().unwrap().sessions.push(session());
    let changed = tokio::time::timeout(SESSION_SAMPLE_INTERVAL + Duration::from_secs(1), a.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(changed.data.sessions.len(), 1);
    assert_eq!(changed, b.recv().await.unwrap());
    assert_eq!(sessions.samples.load(Ordering::SeqCst), 2);
    assert_eq!(sessions.invalidations.load(Ordering::SeqCst), 0);
    // A further fallback reads once but equal state never wakes either listener.
    assert!(
        tokio::time::timeout(
            SESSION_SAMPLE_INTERVAL + Duration::from_millis(100),
            a.recv()
        )
        .await
        .is_err()
    );
    assert_eq!(sessions.samples.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn watcher_wakes_sessions_and_unavailable_sessions_do_not_block_store() {
    let fixture = tempfile::tempdir().unwrap();
    let sessions = Arc::new(FakeSessions::default());
    sessions.unavailable.store(true, Ordering::SeqCst);
    let service = start(fixture.path(), sessions.clone()).await;
    let stores = service.subscribe_store().await.unwrap();
    assert_eq!(stores.recv().await.unwrap().data.schema_version, 1);
    assert!(
        service
            .sessions_list()
            .await
            .unwrap_err()
            .starts_with("unavailable:")
    );
    let listener = service.subscribe_sessions().await.unwrap();
    // No invented empty snapshot while the source is unavailable.
    assert!(
        tokio::time::timeout(Duration::from_millis(30), listener.recv())
            .await
            .is_err()
    );
    sessions.unavailable.store(false, Ordering::SeqCst);
    sessions.value.lock().unwrap().sessions.push(session());
    let path = fixture.path().join("sessions.json");
    tokio::task::spawn_blocking(move || std::fs::write(path, "fixture wake"))
        .await
        .unwrap()
        .unwrap();
    let first = tokio::time::timeout(Duration::from_millis(1500), listener.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.rev, 1);
    assert_eq!(first.data.sessions.len(), 1);
    assert!(sessions.invalidations.load(Ordering::SeqCst) >= 1);
    // Core supports inspecting a not-yet-created project directory.
    let inspection = service
        .inspect_project(fixture.path().join("new-project"))
        .await
        .unwrap();
    assert!(!inspection.exists);
}

struct ThreadCheckedStore {
    forbidden: ThreadId,
}
impl StoreBackend for ThreadCheckedStore {
    fn snapshot(&self) -> Result<StoreState> {
        assert_ne!(std::thread::current().id(), self.forbidden);
        Ok(StoreState::default())
    }
    fn search(&self, _query: &str) -> Result<Vec<SearchHitDto>> {
        self.snapshot()?;
        Ok(Vec::new())
    }
    fn inspect_project(&self, _root: &Path) -> Result<ProjectInspectionDto> {
        bail!("fixture error")
    }
}

#[tokio::test(flavor = "current_thread")]
async fn async_reads_hop_off_the_callers_thread_and_errors_are_coded() {
    let fixture = tempfile::tempdir().unwrap();
    let home = fixture.path().to_path_buf();
    let backend = Arc::new(ThreadCheckedStore {
        forbidden: std::thread::current().id(),
    });
    let service = tokio::task::spawn_blocking(move || {
        ProcessService::with_backends(home, backend, Arc::new(FakeSessions::default()))
    })
    .await
    .unwrap()
    .unwrap();
    assert_eq!(service.store_snapshot().await.unwrap().schema_version, 1);
    assert!(
        service
            .store_search("anything".into())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        service
            .inspect_project("".into())
            .await
            .unwrap_err()
            .starts_with("invalid:")
    );
    assert_eq!(
        service
            .inspect_project("/isolated".into())
            .await
            .unwrap_err(),
        "io: fixture error"
    );
}

#[tokio::test]
async fn one_settings_watcher_pushes_terminal_theme_changes_to_two_windows() {
    let fixture = tempfile::tempdir().unwrap();
    let service = start(fixture.path(), Arc::new(FakeSessions::default())).await;
    let first = service.subscribe_appearance().await.unwrap();
    let second = service.subscribe_appearance().await.unwrap();
    assert_eq!(
        first.recv().await.unwrap().data.terminal_dark["source"],
        "ghostty"
    );
    second.recv().await.unwrap();
    riwork_core::settings::SettingsStore::open(fixture.path())
        .unwrap()
        .update(|settings| {
            settings.theme = riwork_core::theme::ThemeChoice::Catppuccin;
            settings.tab_close_behavior = riwork_core::settings::TabCloseBehavior::Detach;
        })
        .unwrap();
    let changed = tokio::time::timeout(Duration::from_secs(3), first.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(changed.data.terminal_dark["background"], "#1e1e2e");
    assert_eq!(changed.data.tab_close_behavior, "detach");
    assert_eq!(second.recv().await.unwrap(), changed);
    // Equal settings never generate a duplicate projection.
    riwork_core::settings::SettingsStore::open(fixture.path())
        .unwrap()
        .update(|_| {})
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(150), first.recv())
            .await
            .is_err()
    );
}

struct WritableSessions {
    state: Mutex<SessionsSnapshot>,
    forbidden: ThreadId,
    fail_close: AtomicBool,
}
impl SessionsBackend for WritableSessions {
    fn list(&self) -> Result<SessionsSnapshot> {
        assert_ne!(std::thread::current().id(), self.forbidden);
        Ok(self.state.lock().unwrap().clone())
    }
    fn create(
        &self,
        request: riwork_nsh_native::dto::CreateSessionRequest,
    ) -> Result<ShellSessionDto> {
        assert_ne!(std::thread::current().id(), self.forbidden);
        let mut value = session();
        value.project_id = Some(request.project_id);
        value.alive = true;
        self.state.lock().unwrap().sessions.push(value.clone());
        Ok(value)
    }
    fn close(&self, id: &str) -> Result<()> {
        assert_ne!(std::thread::current().id(), self.forbidden);
        if self.fail_close.load(Ordering::SeqCst) {
            bail!("fixture close failed");
        }
        self.state
            .lock()
            .unwrap()
            .sessions
            .retain(|session| session.id != id);
        Ok(())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn local_mutations_hop_off_thread_publish_immediately_and_do_not_hide_failed_exit() {
    let fixture = tempfile::tempdir().unwrap();
    let sessions = Arc::new(WritableSessions {
        state: Mutex::new(SessionsSnapshot::default()),
        forbidden: std::thread::current().id(),
        fail_close: AtomicBool::new(false),
    });
    let service = start(fixture.path(), sessions.clone()).await;
    let subscription = service.subscribe_sessions().await.unwrap();
    assert!(subscription.recv().await.unwrap().data.sessions.is_empty());
    let created = service
        .sessions_create(riwork_nsh_native::dto::CreateSessionRequest {
            project_id: "project-fixture".into(),
            worktree_id: None,
            cwd: None,
            command: None,
        })
        .await
        .unwrap();
    let update = tokio::time::timeout(Duration::from_secs(1), subscription.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(update.data.sessions, [created.clone()]);
    sessions.fail_close.store(true, Ordering::SeqCst);
    assert!(
        service
            .sessions_close(created.id.clone())
            .await
            .unwrap_err()
            .starts_with("io:")
    );
    assert_eq!(
        service.sessions_list().await.unwrap().sessions,
        [created.clone()]
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(150), subscription.recv())
            .await
            .is_err()
    );
    sessions.fail_close.store(false, Ordering::SeqCst);
    service.sessions_close(created.id).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), subscription.recv())
            .await
            .unwrap()
            .unwrap()
            .data
            .sessions
            .is_empty()
    );
}
