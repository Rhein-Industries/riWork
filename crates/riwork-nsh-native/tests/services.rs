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
    // The liveness fallback exists only while an inventory contains shells.
    // Discovery into an empty inventory is driven by the home watcher.
    sessions.value.lock().unwrap().sessions.push(session());
    let service = start(fixture.path(), sessions.clone()).await;
    let a = service.subscribe_sessions().await.unwrap();
    let b = service.subscribe_sessions().await.unwrap();
    assert_eq!(a.recv().await.unwrap().rev, 1);
    assert_eq!(b.recv().await.unwrap().rev, 1);
    assert_eq!(sessions.samples.load(Ordering::SeqCst), 1);
    sessions.value.lock().unwrap().sessions[0].alive = true;
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
async fn an_empty_inventory_sleeps_until_a_watcher_event_and_disarms_when_empty_again() {
    let fixture = tempfile::tempdir().unwrap();
    let sessions = Arc::new(FakeSessions::default());
    let service = start(fixture.path(), sessions.clone()).await;
    let listener = service.subscribe_sessions().await.unwrap();
    assert!(listener.recv().await.unwrap().data.sessions.is_empty());
    tokio::time::sleep(SESSION_SAMPLE_INTERVAL + Duration::from_millis(150)).await;
    assert_eq!(
        sessions.samples.load(Ordering::SeqCst),
        1,
        "an empty inventory must not run the tmux liveness fallback"
    );

    sessions.value.lock().unwrap().sessions.push(session());
    let path = fixture.path().join("sessions.json");
    std::fs::write(&path, "a fixture shell appeared").unwrap();
    let first = tokio::time::timeout(Duration::from_secs(2), listener.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.data.sessions.len(), 1);
    let reads = sessions.samples.load(Ordering::SeqCst);
    tokio::time::sleep(SESSION_SAMPLE_INTERVAL + Duration::from_millis(150)).await;
    assert!(
        sessions.samples.load(Ordering::SeqCst) > reads,
        "the liveness fallback must run after a watcher discovers a shell"
    );

    sessions.value.lock().unwrap().sessions.clear();
    std::fs::write(&path, "the last fixture shell disappeared").unwrap();
    let empty = tokio::time::timeout(Duration::from_secs(2), listener.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(empty.data.sessions.is_empty());
    // Let any coalesced filesystem notification settle before counting.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let reads = sessions.samples.load(Ordering::SeqCst);
    tokio::time::sleep(SESSION_SAMPLE_INTERVAL + Duration::from_millis(150)).await;
    assert_eq!(
        sessions.samples.load(Ordering::SeqCst),
        reads,
        "removing the last shell must disarm the fallback"
    );
}

#[tokio::test]
async fn a_quiet_home_produces_no_deliveries_for_60_s() {
    let fixture = tempfile::tempdir().unwrap();
    let sessions = Arc::new(FakeSessions::default());
    let service = start(fixture.path(), sessions.clone()).await;
    let stores = service.subscribe_store().await.unwrap();
    let shells = service.subscribe_sessions().await.unwrap();
    stores.recv().await.unwrap();
    shells.recv().await.unwrap();
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_secs(60)) => {},
        event = stores.recv() => panic!("quiet Store delivered {event:?}"),
        event = shells.recv() => panic!("quiet Sessions delivered {event:?}"),
    }
    assert_eq!(sessions.samples.load(Ordering::SeqCst), 1);
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
