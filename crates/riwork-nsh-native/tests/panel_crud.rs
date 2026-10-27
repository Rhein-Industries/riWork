//! Isolated persisted panel adapters; no production home, tmux or windows.
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::Result;
use riwork_engine::project_tabs::{Kind, Session, Status, TabStore};
use riwork_nsh_native::{
    ProcessService,
    backend::{CoreStoreBackend, SessionsBackend, TabsBackend},
    dto::{
        CreateSessionRequest, ProjectRequest, SessionsSnapshot, ShellSessionDto, StoreState,
        TabUpdate, TabsSnapshot,
    },
    hub::TopicSubscription,
};

#[derive(Default)]
struct FixtureSessions(Mutex<SessionsSnapshot>);
impl SessionsBackend for FixtureSessions {
    fn list(&self) -> Result<SessionsSnapshot> {
        Ok(self.0.lock().unwrap().clone())
    }
    fn create(&self, request: CreateSessionRequest) -> Result<ShellSessionDto> {
        let saved: riwork_engine::sessions::ShellSession =
            serde_json::from_value(serde_json::json!({
                "id": "00000000-0000-4000-8000-000000000002", "project_id": request.project_id,
                "worktree_id": request.worktree_id, "kind": "project", "cwd": request.cwd,
                "command": request.command, "created_at_unix": 1, "user_opened": true
            }))?;
        let mut dto = ShellSessionDto::from(saved);
        dto.alive = true;
        self.0.lock().unwrap().sessions.push(dto.clone());
        Ok(dto)
    }
    fn close(&self, id: &str) -> Result<()> {
        self.0
            .lock()
            .unwrap()
            .sessions
            .retain(|session| session.id != id);
        Ok(())
    }
}

struct FixtureTabs(std::path::PathBuf);
impl TabsBackend for FixtureTabs {
    fn snapshot(&self, project: &str) -> Result<TabsSnapshot> {
        TabStore::at(&self.0, project)
            .map_err(anyhow::Error::msg)?
            .snapshot()
            .map_err(anyhow::Error::msg)?
            .try_into()
    }
    fn update(&self, project: &str, update: TabUpdate) -> Result<TabsSnapshot> {
        let store = TabStore::at(&self.0, project).map_err(anyhow::Error::msg)?;
        store.update(&update.into()).map_err(anyhow::Error::msg)?;
        self.snapshot(project)
    }
}

async fn start(home: &Path) -> Arc<ProcessService> {
    let home = home.to_path_buf();
    tokio::task::spawn_blocking(move || {
        ProcessService::with_all_backends(
            home.clone(),
            Arc::new(CoreStoreBackend::with_default_project_directory(
                home.clone(),
                home.join("fixture-projects"),
            )),
            Arc::new(FixtureSessions::default()),
            Arc::new(FixtureTabs(home)),
        )
    })
    .await
    .unwrap()
    .unwrap()
}

async fn observe(
    subscription: &TopicSubscription<StoreState>,
    predicate: impl Fn(&StoreState) -> bool,
) -> StoreState {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let event = subscription.recv().await.unwrap();
            if predicate(&event.data) {
                return event.data;
            }
        }
    })
    .await
    .expect("second service receives durable store watcher event")
}

#[tokio::test]
async fn project_folder_and_task_lifecycle_round_trip_real_files_and_second_window_subscription() {
    let fixture = tempfile::tempdir().unwrap();
    let home = fixture.path().join("RIWORK_HOME");
    let first = start(&home).await;
    let second = start(&home).await;
    let observer = second.subscribe_store().await.unwrap();
    assert!(observer.recv().await.unwrap().data.projects.is_empty());
    let project = first
        .create_new_project(ProjectRequest {
            root: fixture
                .path()
                .join("projects/App")
                .to_string_lossy()
                .into_owned(),
            name: Some("App".into()),
            init_git: Some(false),
        })
        .await
        .unwrap();
    let state = observe(&observer, |state| {
        state.projects.iter().any(|p| p.id == project.id)
    })
    .await;
    assert_eq!(
        state.active_project_id.as_deref(),
        Some(project.id.as_str())
    );
    let folder = first.create_folder("Clients".into(), None).await.unwrap();
    let child = first
        .create_folder("Nested".into(), Some(folder.id.clone()))
        .await
        .unwrap();
    first
        .move_project_to_folder(project.id.clone(), Some(child.id.clone()))
        .await
        .unwrap();
    first
        .rename_folder(child.id.clone(), "Renamed".into())
        .await
        .unwrap();
    first.move_folder(child.id.clone(), None).await.unwrap();
    first.remove_folder(folder.id).await.unwrap();
    first
        .rename_project(project.id.clone(), "App renamed".into())
        .await
        .unwrap();
    first.use_project(project.id.clone()).await.unwrap();
    assert!(
        first
            .set_project_notifications(project.id.clone(), true)
            .await
            .unwrap()
            .notify_on_agent_done
    );
    assert!(
        first
            .set_project_orchestrator_wakes(project.id.clone(), true)
            .await
            .unwrap()
            .wake_orchestrators_on_agent_done
    );
    let worktree = first
        .store_snapshot()
        .await
        .unwrap()
        .worktrees
        .into_iter()
        .find(|w| w.project_id == project.id)
        .expect("plain project has primary worktree");
    assert!(worktree.present);
    let task = first
        .add_task(
            project.id.clone(),
            "Ship panels".into(),
            "Persist the lifecycle".into(),
        )
        .await
        .unwrap();
    assert_eq!(task.status, "todo");
    let assigned = first
        .assign_tasks(worktree.id.clone(), vec![task.id.clone()])
        .await
        .unwrap();
    assert_eq!(
        assigned[0].worktree_id.as_deref(),
        Some(worktree.id.as_str())
    );
    assert_eq!(
        first
            .set_task_status(task.id.clone(), "in_progress".into())
            .await
            .unwrap()
            .status,
        "in_progress"
    );
    assert_eq!(
        first
            .set_task_status(task.id.clone(), "done".into())
            .await
            .unwrap()
            .status,
        "done"
    );
    assert_eq!(
        first.unassign_tasks(vec![task.id.clone()]).await.unwrap()[0].worktree_id,
        None
    );
    assert!(
        first
            .set_task_status(task.id.clone(), "broken".into())
            .await
            .unwrap_err()
            .starts_with("invalid:")
    );
    let updated = observe(&observer, |state| {
        state
            .tasks
            .iter()
            .any(|t| t.id == task.id && t.status == "done" && t.worktree_id.is_none())
    })
    .await;
    assert_eq!(
        updated.projects[0].folder_id.as_deref(),
        Some(child.id.as_str())
    );
    let persisted = riwork_core::store::Store::open(home.clone())
        .unwrap()
        .snapshot()
        .unwrap();
    assert_eq!(persisted.tasks[0].id, task.id);
    assert_eq!(persisted.tasks[0].status.as_str(), "done");
    assert_eq!(persisted.tasks[0].worktree_id, None);
    assert!(persisted.projects[0].notify_on_agent_done);
    assert!(persisted.projects[0].wake_orchestrators_on_agent_done);
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(home.join("state.json")).unwrap()).unwrap();
    assert!(json.get("schema_version").is_some());
    assert!(json.get("schemaVersion").is_none());
    assert!(
        tokio::time::timeout(Duration::from_millis(120), observer.recv())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn tabs_mutations_and_external_writer_reach_both_services_without_live_inventory() {
    let fixture = tempfile::tempdir().unwrap();
    let project = "00000000-0000-4000-8000-000000000001";
    let key = "shell:00000000-0000-4000-8000-000000000002";
    let tabs = TabStore::at(fixture.path(), project).unwrap();
    tabs.reconcile_inventory(&[Session {
        title_priority: 0,
        key: key.into(),
        kind: Kind::Shell,
        title: "Shell".into(),
        parent_id: None,
        status: Status::Idle,
        orchestrator: false,
        worker: false,
        created: 1,
    }])
    .unwrap();
    let a = start(fixture.path()).await;
    let b = start(fixture.path()).await;
    let first = a.subscribe_tabs(project.into()).await.unwrap();
    let second = b.subscribe_tabs(project.into()).await.unwrap();
    let initial = first.recv().await.unwrap();
    assert_eq!(initial.data, second.recv().await.unwrap().data);
    let changed = a
        .tabs_update(
            project.into(),
            TabUpdate::Rename {
                key: key.into(),
                title: "Named shell".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(changed.entries[0].title, "Named shell");
    assert_eq!(first.recv().await.unwrap().data, changed);
    let observed = tokio::time::timeout(Duration::from_secs(4), second.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(observed.data, changed);
    assert!(observed.rev > initial.rev);
    tabs.update(&riwork_engine::project_tabs::Update::Hide { key: key.into() })
        .unwrap();
    for subscriber in [&first, &second] {
        let observed = tokio::time::timeout(Duration::from_secs(4), subscriber.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(observed.data.entries[0].hidden);
    }
    assert!(a.subscribe_tabs("not-a-uuid".into()).await.is_err());
    assert!(
        tokio::time::timeout(Duration::from_millis(120), second.recv())
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn session_create_close_adapters_publish_to_all_window_subscribers() {
    let fixture = tempfile::tempdir().unwrap();
    let service = start(fixture.path()).await;
    let a = service.subscribe_sessions().await.unwrap();
    let b = service.subscribe_sessions().await.unwrap();
    a.recv().await.unwrap();
    b.recv().await.unwrap();
    let shell = service
        .create_session(CreateSessionRequest {
            project_id: "00000000-0000-4000-8000-000000000001".into(),
            worktree_id: None,
            cwd: fixture.path().to_string_lossy().into_owned(),
            command: None,
        })
        .await
        .unwrap();
    assert!(shell.user_opened);
    assert!(shell.parent_id.is_none());
    assert_eq!(a.recv().await.unwrap().data.sessions, vec![shell.clone()]);
    assert_eq!(b.recv().await.unwrap().data.sessions, vec![shell.clone()]);
    service.close_session(shell.id).await.unwrap();
    assert!(a.recv().await.unwrap().data.sessions.is_empty());
    assert!(b.recv().await.unwrap().data.sessions.is_empty());
}

#[tokio::test]
async fn process_sampler_pushes_worktree_presence_without_ui_polling_or_state_write() {
    let fixture = tempfile::tempdir().unwrap();
    let home = fixture.path().join("RIWORK_HOME");
    let service = start(&home).await;
    let root = fixture.path().join("projects/presence");
    let project = service
        .create_project(ProjectRequest {
            root: root.to_string_lossy().into_owned(),
            name: None,
            init_git: Some(false),
        })
        .await
        .unwrap();
    let subscription = service.subscribe_store().await.unwrap();
    let initial = subscription.recv().await.unwrap();
    assert!(
        initial
            .data
            .worktrees
            .iter()
            .any(|worktree| worktree.project_id == project.id && worktree.present)
    );
    let bytes_before = std::fs::read(home.join("state.json")).unwrap();
    std::fs::remove_dir(root).unwrap();
    // Session watcher wakes keep resetting the tmux fallback. Presence must
    // still arrive on its independent deadline while those wakes continue.
    // This strengthens the existing presence wait without another timed test.
    let stop = Arc::new(AtomicBool::new(false));
    let writer_stop = stop.clone();
    let wake_path = home.join("sessions.json");
    let writer = std::thread::spawn(move || {
        while !writer_stop.load(Ordering::Acquire) {
            std::fs::write(&wake_path, "isolated fixture session wake").unwrap();
            std::thread::sleep(Duration::from_millis(100));
        }
    });
    let missing = tokio::time::timeout(Duration::from_secs(6), async {
        loop {
            let event = subscription.recv().await.unwrap();
            if event
                .data
                .worktrees
                .iter()
                .any(|worktree| worktree.project_id == project.id && !worktree.present)
            {
                break event.data;
            }
        }
    })
    .await;
    stop.store(true, Ordering::Release);
    writer.join().unwrap();
    let missing = missing.expect("session activity must not postpone worktree presence");
    assert!(
        missing
            .worktrees
            .iter()
            .any(|worktree| worktree.project_id == project.id && worktree.is_primary)
    );
    assert_eq!(
        std::fs::read(home.join("state.json")).unwrap(),
        bytes_before
    );
}
