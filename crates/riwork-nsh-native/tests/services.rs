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
    backend::{CoreStoreBackend, SessionsBackend, StoreBackend, TabsBackend, Unavailable},
    dto::{
        ProjectInspectionDto, SearchHitDto, SessionsSnapshot, ShellSessionDto, StoreState,
        TabUpdate, TabsSnapshot,
    },
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
    fn create(
        &self,
        request: riwork_nsh_native::dto::CreateSessionRequest,
    ) -> Result<ShellSessionDto> {
        assert_ne!(std::thread::current().name(), Some("nsh-engine"));
        let mut created = session();
        created.project_id = Some(request.project_id);
        created.worktree_id = request.worktree_id;
        if let Some(cwd) = request.cwd {
            created.cwd = cwd;
        }
        created.command = request.command;
        self.value.lock().unwrap().sessions.push(created.clone());
        Ok(created)
    }
    fn close(&self, id: &str) -> Result<()> {
        self.value
            .lock()
            .unwrap()
            .sessions
            .retain(|session| session.id != id);
        Ok(())
    }
    fn create_subshell(
        &self,
        request: riwork_nsh_native::dto::CreateSubshellRequest,
    ) -> Result<ShellSessionDto> {
        assert_ne!(std::thread::current().name(), Some("nsh-engine"));
        let mut created = session();
        created.project_id = Some(request.project_id);
        created.parent_id = Some(request.parent_shell_id);
        self.value.lock().unwrap().sessions.push(created.clone());
        Ok(created)
    }
    fn open_editor(
        &self,
        request: riwork_nsh_native::dto::OpenEditorRequest,
    ) -> Result<ShellSessionDto> {
        assert_ne!(std::thread::current().name(), Some("nsh-engine"));
        let mut created = session();
        created.project_id = Some(request.project_id);
        created.worktree_id = request.worktree_id;
        created.editor_path = Some(request.path);
        self.value.lock().unwrap().sessions.push(created.clone());
        Ok(created)
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

struct CountedStore {
    inner: CoreStoreBackend,
    presence_refreshes: AtomicUsize,
}

impl StoreBackend for CountedStore {
    fn snapshot(&self) -> Result<StoreState> {
        self.inner.snapshot()
    }
    fn search(&self, query: &str) -> Result<Vec<SearchHitDto>> {
        self.inner.search(query)
    }
    fn inspect_project(&self, root: &Path) -> Result<ProjectInspectionDto> {
        self.inner.inspect_project(root)
    }
    fn refresh_presence(&self) -> Result<StoreState> {
        self.presence_refreshes.fetch_add(1, Ordering::SeqCst);
        self.inner.refresh_presence()
    }
}

#[derive(Default)]
struct CountedTabs(AtomicUsize);

impl TabsBackend for CountedTabs {
    fn snapshot(&self, _project: &str) -> Result<TabsSnapshot> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(TabsSnapshot {
            epoch: "fixture".into(),
            revision: 1,
            entries: vec![],
        })
    }
    fn update(&self, _project: &str, _update: TabUpdate) -> Result<TabsSnapshot> {
        bail!("fixture does not mutate tabs")
    }
}

#[tokio::test]
async fn worktree_presence_has_a_deadline_without_restarting_empty_session_fallback() {
    let fixture = tempfile::tempdir().unwrap();
    let home = fixture.path().join("RIWORK_HOME");
    let project = riwork_core::store::Store::open(&home)
        .unwrap()
        .create_project(fixture.path().join("project"), None, false)
        .unwrap();
    let sessions = Arc::new(FakeSessions::default());
    let tabs = Arc::new(CountedTabs::default());
    let store = Arc::new(CountedStore {
        inner: CoreStoreBackend::new(home.clone()),
        presence_refreshes: AtomicUsize::new(0),
    });
    let service = tokio::task::spawn_blocking({
        let store = store.clone();
        let sessions = sessions.clone();
        let tabs = tabs.clone();
        move || ProcessService::with_all_backends(home, store, sessions, tabs)
    })
    .await
    .unwrap()
    .unwrap();
    let listener = service.subscribe_store().await.unwrap();
    let tab_listener = service.subscribe_tabs(project.id).await.unwrap();
    tab_listener.recv().await.unwrap();
    assert_eq!(listener.recv().await.unwrap().data.worktrees.len(), 1);
    tokio::time::sleep(SESSION_SAMPLE_INTERVAL + Duration::from_millis(200)).await;
    assert!(
        store.presence_refreshes.load(Ordering::SeqCst) >= 1,
        "worktrees still need their independent presence refresh"
    );
    assert_eq!(
        sessions.samples.load(Ordering::SeqCst),
        1,
        "presence refresh must not query the empty session inventory"
    );
    assert_eq!(
        tabs.0.load(Ordering::SeqCst),
        1,
        "presence refresh must not indirectly query sessions through tab inventory"
    );
}

#[tokio::test]
async fn a_quiet_home_produces_no_deliveries_for_60_s() {
    let fixture = tempfile::tempdir().unwrap();
    let sessions = Arc::new(FakeSessions::default());
    let home = fixture.path().to_path_buf();
    let store = Arc::new(CountedStore {
        inner: CoreStoreBackend::new(home.clone()),
        presence_refreshes: AtomicUsize::new(0),
    });
    let service = tokio::task::spawn_blocking({
        let store = store.clone();
        let sessions = sessions.clone();
        move || {
            ProcessService::with_all_backends(home, store, sessions, Arc::new(FakeTabs::default()))
        }
    })
    .await
    .unwrap()
    .unwrap();
    let stores = service.subscribe_store().await.unwrap();
    let shells = service.subscribe_sessions().await.unwrap();
    let preferences = service.preferences().clone();
    let tabs = service
        .subscribe_tabs("00000000-0000-4000-8000-000000000001".into())
        .await
        .unwrap();
    let (settings, appearance) = tokio::task::spawn_blocking(move || {
        (
            preferences.subscribe_settings(),
            preferences.subscribe_appearance(),
        )
    })
    .await
    .unwrap();
    stores.recv().await.unwrap();
    shells.recv().await.unwrap();
    settings.recv().await.unwrap();
    appearance.recv().await.unwrap();
    tabs.recv().await.unwrap();
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_secs(60)) => {},
        event = stores.recv() => panic!("quiet Store delivered {event:?}"),
        event = shells.recv() => panic!("quiet Sessions delivered {event:?}"),
        event = settings.recv() => panic!("quiet Settings delivered {event:?}"),
        event = appearance.recv() => panic!("quiet Appearance delivered {event:?}"),
        event = tabs.recv() => panic!("quiet Tabs delivered {event:?}"),
    }
    assert_eq!(sessions.samples.load(Ordering::SeqCst), 1);
    assert_eq!(
        store.presence_refreshes.load(Ordering::SeqCst),
        0,
        "an empty home must not arm a presence timer either"
    );
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
    assert!(first.recv().await.unwrap().data.terminal_dark.is_none());
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
    assert_eq!(
        changed.data.terminal_dark.as_ref().unwrap().background,
        "#1e1e2e"
    );
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
    fail_next_sample: AtomicBool,
}
impl SessionsBackend for WritableSessions {
    fn sample(&self) -> Result<SessionsSnapshot> {
        if self.fail_next_sample.swap(false, Ordering::SeqCst) {
            bail!("fixture follow-up inventory failed");
        }
        self.list()
    }
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
        fail_next_sample: AtomicBool::new(false),
    });
    let service = start(fixture.path(), sessions.clone()).await;
    let subscription = service.subscribe_sessions().await.unwrap();
    assert!(subscription.recv().await.unwrap().data.sessions.is_empty());
    let created = service
        .sessions_create(riwork_nsh_native::dto::CreateSessionRequest {
            project_id: "00000000-0000-4000-8000-000000000003".into(),
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

#[tokio::test(flavor = "current_thread")]
async fn successful_session_mutations_survive_failed_follow_up_inventory_and_recover() {
    let fixture = tempfile::tempdir().unwrap();
    let sessions = Arc::new(WritableSessions {
        state: Mutex::new(SessionsSnapshot::default()),
        forbidden: std::thread::current().id(),
        fail_close: AtomicBool::new(false),
        fail_next_sample: AtomicBool::new(false),
    });
    let service = start(fixture.path(), sessions.clone()).await;
    let subscription = service.subscribe_sessions().await.unwrap();
    assert!(subscription.recv().await.unwrap().data.sessions.is_empty());
    sessions.fail_next_sample.store(true, Ordering::SeqCst);
    let created = service
        .create_session(riwork_nsh_native::dto::CreateSessionRequest {
            project_id: "00000000-0000-4000-8000-000000000003".into(),
            worktree_id: None,
            cwd: None,
            command: None,
        })
        .await
        .expect("a successful create must not invite a duplicate retry");
    assert!(
        service
            .last_error()
            .unwrap()
            .contains("follow-up inventory failed")
    );
    let recovered = tokio::time::timeout(
        SESSION_SAMPLE_INTERVAL + Duration::from_secs(1),
        subscription.recv(),
    )
    .await
    .expect("a failed inventory arms the otherwise quiet sampler")
    .unwrap();
    assert_eq!(recovered.data.sessions, [created.clone()]);
    sessions.fail_next_sample.store(true, Ordering::SeqCst);
    service.close_session(created.id).await.unwrap();
    let closed = tokio::time::timeout(
        SESSION_SAMPLE_INTERVAL + Duration::from_secs(1),
        subscription.recv(),
    )
    .await
    .expect("close also recovers its follow-up inventory")
    .unwrap();
    assert!(closed.data.sessions.is_empty());
}

#[derive(Default)]
struct FakeTabs {
    calls: AtomicUsize,
    value: Mutex<Option<riwork_nsh_native::tabs::TabsSnapshot>>,
}
impl riwork_nsh_native::tabs::TabsBackend for FakeTabs {
    fn snapshot(&self, _project: &str) -> Result<riwork_nsh_native::tabs::TabsSnapshot> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.value.lock().unwrap().clone().unwrap_or_else(|| {
            riwork_nsh_native::tabs::TabsSnapshot {
                epoch: "fixture".into(),
                revision: 1,
                entries: Vec::new(),
            }
        }))
    }
    fn update(
        &self,
        project: &str,
        _update: TabUpdate,
    ) -> Result<riwork_nsh_native::tabs::TabsSnapshot> {
        let mut snapshot = self.snapshot(project)?;
        snapshot.revision += 1;
        *self.value.lock().unwrap() = Some(snapshot.clone());
        Ok(snapshot)
    }
}

#[tokio::test]
async fn tabs_update_rejects_invalid_keys() {
    use serde_json::json;
    let fixture = tempfile::tempdir().unwrap();
    let home = fixture.path().to_path_buf();
    let backend = Arc::new(FakeTabs::default());
    let test_tabs = backend.clone();
    let service = tokio::task::spawn_blocking(move || {
        ProcessService::with_all_backends(
            home.clone(),
            Arc::new(CoreStoreBackend::new(home)),
            Arc::new(FakeSessions::default()),
            test_tabs,
        )
    })
    .await
    .unwrap()
    .unwrap();
    let project = "00000000-0000-4000-8000-000000000001";
    for update in [
        json!({"action":"hide","key":"shell:../../sessions"}),
        json!({"action":"unhide","key":"chat:NOT-A-UUID"}),
        json!({"action":"move","key":"shell:00000000-0000-4000-8000-000000000001","before":"shell:bad"}),
        json!({"action":"rename","key":"shell:00000000-0000-4000-8000-000000000001","title":"bad\nname"}),
    ] {
        let update: TabUpdate = serde_json::from_value(update).unwrap();
        assert!(
            service
                .tabs_update(project.into(), update)
                .await
                .unwrap_err()
                .starts_with("invalid:")
        );
    }
    assert!(serde_json::from_value::<TabUpdate>(json!({"action":"hide","key":"shell:00000000-0000-4000-8000-000000000001","surprise":true})).is_err());
    assert!(
        service
            .tabs_update(
                "not-a-project".into(),
                TabUpdate::Hide {
                    key: "shell:00000000-0000-4000-8000-000000000001".into()
                }
            )
            .await
            .unwrap_err()
            .starts_with("invalid:")
    );
    assert_eq!(
        backend.calls.load(Ordering::SeqCst),
        0,
        "validation must precede backend calls"
    );
}

#[tokio::test]
async fn store_rejects_non_uuid_ids() {
    let fixture = tempfile::tempdir().unwrap();
    let service = start(fixture.path(), Arc::new(FakeSessions::default())).await;
    for id in [
        "../state",
        "a folder name",
        "00000000-0000-4000-8000-00000000000A",
    ] {
        assert!(
            service
                .rename_folder(id.into(), "Name".into())
                .await
                .unwrap_err()
                .starts_with("invalid:")
        );
        assert!(
            service
                .move_folder(id.into(), None)
                .await
                .unwrap_err()
                .starts_with("invalid:")
        );
        assert!(
            service
                .remove_folder(id.into())
                .await
                .unwrap_err()
                .starts_with("invalid:")
        );
        assert!(
            service
                .create_folder("Name".into(), Some(id.into()))
                .await
                .unwrap_err()
                .starts_with("invalid:")
        );
        assert!(
            service
                .move_project_to_folder(id.into(), None)
                .await
                .unwrap_err()
                .starts_with("invalid:")
        );
    }
    assert!(
        !fixture.path().join("state.json").exists(),
        "invalid bridge IDs must not write state"
    );
}

#[tokio::test]
async fn folder_mutations_publish_once_to_every_subscriber_and_preserve_disk_format() {
    let fixture = tempfile::tempdir().unwrap();
    let service = start(fixture.path(), Arc::new(FakeSessions::default())).await;
    let first = service.subscribe_store().await.unwrap();
    let second = service.subscribe_store().await.unwrap();
    assert_eq!(first.recv().await.unwrap(), second.recv().await.unwrap());
    let parent = service.create_folder("Parent".into(), None).await.unwrap();
    let event = first.recv().await.unwrap();
    assert_eq!(event, second.recv().await.unwrap());
    assert_eq!(event.data.project_folders, vec![parent.clone()]);
    assert!(
        tokio::time::timeout(Duration::from_millis(150), first.recv())
            .await
            .is_err()
    );
    let child = service
        .create_folder("Child".into(), Some(parent.id.clone()))
        .await
        .unwrap();
    assert_eq!(child.parent_id, Some(parent.id.clone()));
    let renamed = service
        .rename_folder(child.id.clone(), "Renamed".into())
        .await
        .unwrap();
    assert_eq!(renamed.name, "Renamed");
    let moved = service.move_folder(child.id.clone(), None).await.unwrap();
    assert_eq!(moved.parent_id, None);
    service.remove_folder(parent.id).await.unwrap();
    let disk: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fixture.path().join("state.json")).unwrap()).unwrap();
    assert!(disk.get("project_folders").is_some());
    assert!(disk.get("projectFolders").is_none());
    assert_eq!(disk["project_folders"][0]["id"], child.id);
}

#[tokio::test]
async fn tabs_watcher_and_bridge_updates_share_the_topic_without_idle_sampling() {
    use serde_json::json;
    let fixture = tempfile::tempdir().unwrap();
    let home = fixture.path().to_path_buf();
    let backend = Arc::new(FakeTabs::default());
    let test_tabs = backend.clone();
    let service = tokio::task::spawn_blocking(move || {
        ProcessService::with_tabs_backend(
            home.clone(),
            Arc::new(CoreStoreBackend::new(home)),
            Arc::new(FakeSessions::default()),
            test_tabs,
        )
    })
    .await
    .unwrap()
    .unwrap();
    let project = "00000000-0000-4000-8000-000000000001";
    let first = service.subscribe_tabs(project.into()).await.unwrap();
    let second = service.subscribe_tabs(project.into()).await.unwrap();
    assert_eq!(first.recv().await.unwrap(), second.recv().await.unwrap());
    assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
    service
        .tabs_update(
            project.into(),
            serde_json::from_value(
                json!({"action":"hide","key":"shell:00000000-0000-4000-8000-000000000001"}),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let event = first.recv().await.unwrap();
    assert_eq!(event.data.revision, 2);
    assert_eq!(event, second.recv().await.unwrap());
    backend.value.lock().unwrap().as_mut().unwrap().revision = 3;
    std::fs::create_dir_all(fixture.path().join("project-tabs")).unwrap();
    std::fs::write(
        fixture
            .path()
            .join("project-tabs")
            .join(format!("{project}.json")),
        "external fixture tab write",
    )
    .unwrap();
    let event = tokio::time::timeout(Duration::from_secs(2), first.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(event.data.revision, 3);
    assert_eq!(event, second.recv().await.unwrap());
    assert!(
        tokio::time::timeout(Duration::from_millis(100), first.recv())
            .await
            .is_err()
    );
    let reads = backend.calls.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(backend.calls.load(Ordering::SeqCst), reads);
}

#[tokio::test]
async fn errors_use_only_the_frozen_codes() {
    const CODES: [&str; 7] = [
        "refused",
        "broken",
        "not_found",
        "invalid",
        "conflict",
        "unavailable",
        "io",
    ];
    let fixture = tempfile::tempdir().unwrap();
    let sessions = Arc::new(FakeSessions::default());
    sessions.unavailable.store(true, Ordering::SeqCst);
    let service = start(fixture.path(), sessions).await;
    let errors = [
        service.sessions_list().await.unwrap_err(),
        service.inspect_project("".into()).await.unwrap_err(),
        service.remove_folder("bad-id".into()).await.unwrap_err(),
        service
            .rename_folder(
                "00000000-0000-4000-8000-000000000001".into(),
                "Missing".into(),
            )
            .await
            .unwrap_err(),
        service
            .sessions_create(riwork_nsh_native::dto::CreateSessionRequest {
                project_id: "bad-project".into(),
                worktree_id: None,
                cwd: Some("/fixture".into()),
                command: None,
            })
            .await
            .unwrap_err(),
    ];
    for error in errors {
        assert!(
            CODES.contains(&error.split_once(": ").expect("coded error").0),
            "unexpected code in {error}"
        );
    }
}

#[tokio::test]
async fn sessions_create_and_close_wake_the_single_sampler() {
    use riwork_nsh_native::dto::CreateSessionRequest;
    let fixture = tempfile::tempdir().unwrap();
    let sessions = Arc::new(FakeSessions::default());
    let service = start(fixture.path(), sessions.clone()).await;
    let first = service.subscribe_sessions().await.unwrap();
    let second = service.subscribe_sessions().await.unwrap();
    assert_eq!(first.recv().await.unwrap(), second.recv().await.unwrap());
    let created = service
        .sessions_create(CreateSessionRequest {
            project_id: "00000000-0000-4000-8000-000000000002".into(),
            worktree_id: None,
            cwd: Some("/isolated/requested".into()),
            command: Some("echo fixture".into()),
        })
        .await
        .unwrap();
    assert_eq!(created.cwd, "/isolated/requested");
    let changed = tokio::time::timeout(Duration::from_secs(1), first.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(changed, second.recv().await.unwrap());
    assert_eq!(changed.data.sessions, vec![created.clone()]);
    service.sessions_close(created.id).await.unwrap();
    let changed = tokio::time::timeout(Duration::from_secs(1), first.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(changed, second.recv().await.unwrap());
    assert!(changed.data.sessions.is_empty());
    assert!(
        service
            .sessions_close("bad".into())
            .await
            .unwrap_err()
            .starts_with("invalid:")
    );
    assert_eq!(sessions.samples.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn subshell_and_vim_mutations_validate_before_backend_and_publish_inventory() {
    use riwork_nsh_native::dto::{CreateSubshellRequest, OpenEditorRequest};
    let fixture = tempfile::tempdir().unwrap();
    let sessions = Arc::new(FakeSessions::default());
    let service = start(fixture.path(), sessions.clone()).await;
    let updates = service.subscribe_sessions().await.unwrap();
    assert!(updates.recv().await.unwrap().data.sessions.is_empty());
    let project = "00000000-0000-4000-8000-000000000002";
    assert!(
        service
            .create_subshell(CreateSubshellRequest {
                project_id: project.into(),
                parent_shell_id: "bad".into()
            })
            .await
            .unwrap_err()
            .starts_with("invalid:")
    );
    assert!(
        service
            .open_editor(OpenEditorRequest {
                project_id: project.into(),
                worktree_id: None,
                path: String::new(),
                line: None
            })
            .await
            .unwrap_err()
            .starts_with("invalid:")
    );
    assert!(sessions.value.lock().unwrap().sessions.is_empty());
    let parent = "00000000-0000-4000-8000-000000000003";
    let child = service
        .create_subshell(CreateSubshellRequest {
            project_id: project.into(),
            parent_shell_id: parent.into(),
        })
        .await
        .unwrap();
    assert_eq!(child.parent_id.as_deref(), Some(parent));
    let first = tokio::time::timeout(Duration::from_secs(1), updates.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.data.sessions, [child]);
    let editor = service
        .open_editor(OpenEditorRequest {
            project_id: project.into(),
            worktree_id: None,
            path: "/fixture/a spaced 🦀 file.txt".into(),
            line: Some(4),
        })
        .await
        .unwrap();
    assert_eq!(
        editor.editor_path.as_deref(),
        Some("/fixture/a spaced 🦀 file.txt")
    );
    let second = tokio::time::timeout(Duration::from_secs(1), updates.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second.data.sessions.len(), 2);
}
