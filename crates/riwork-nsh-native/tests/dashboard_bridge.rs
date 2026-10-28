//! The global dashboard's process read and the orchestrator adapters, against
//! isolated homes. Fake sources count reads; pacing is injected, so nothing here
//! waits on the production 2 s / 10 s intervals.
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};

use riwork_core::store::Store;
use riwork_engine::{
    orchestrators::{Created, Orchestrator, Plan, PlanSource},
    sessions::{ShellKind, ShellSession},
};
use riwork_nsh_native::{
    hub::Snapshot,
    orchestrators::{self, OrchestratorBackend, Orchestrators},
    overview::{DashboardDto, OverviewService, OverviewSource, Pacing, ShellSample},
};
use tempfile::TempDir;

struct Counting {
    home: PathBuf,
    reads: AtomicUsize,
    fail: AtomicBool,
}

impl OverviewSource for Counting {
    fn home(&self) -> &Path {
        &self.home
    }
    fn shells(&self) -> Result<ShellSample, String> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            return Err("tmux did not answer".into());
        }
        Ok(ShellSample::default())
    }
}

fn home_with_project() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let home = dir.path().join("home");
    let root = dir.path().join("alpha");
    std::fs::create_dir_all(&root).unwrap();
    Store::open(home.clone())
        .unwrap()
        .add_project(root, Some("Alpha".into()))
        .unwrap();
    (dir, home)
}

const PACING: Pacing = Pacing {
    change_gap: Duration::from_millis(60),
    extras_every: Duration::from_millis(600),
};

async fn next(
    subscription: &riwork_nsh_native::hub::TopicSubscription<DashboardDto>,
) -> Snapshot<DashboardDto> {
    tokio::time::timeout(Duration::from_secs(10), subscription.recv())
        .await
        .expect("a snapshot within 10 s")
        .unwrap()
}

#[tokio::test]
async fn nothing_is_read_without_a_window_and_one_read_serves_every_window() {
    let (_dir, home) = home_with_project();
    let source = Arc::new(Counting {
        home,
        reads: AtomicUsize::new(0),
        fail: AtomicBool::new(false),
    });
    let (_changes, inbox) = mpsc::sync_channel(1);
    let service =
        OverviewService::start(source.clone(), inbox, Arc::new(|| 1_000), PACING).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        source.reads.load(Ordering::SeqCst),
        0,
        "no subscriber, no read"
    );
    let first = service.subscribe();
    let second = service.subscribe();
    let a = next(&first).await;
    let b = next(&second).await;
    assert_eq!(a, b);
    assert_eq!(source.reads.load(Ordering::SeqCst), 1);
    let dto = a.data;
    assert_eq!((dto.schema, dto.now, dto.read_at), (3, 1_000, Some(1_000)));
    assert_eq!(dto.error, None);
    let wire = dto.wire.unwrap();
    assert_eq!(wire["v"], 1);
    assert_eq!(wire["generated_at"], 1_000);
    assert_eq!(wire["projects"][0]["name"], "Alpha");
    // No host runs, so the saved chats were read (none): zero is a counted zero.
    assert_eq!(wire["chats_known"], true);
    assert_eq!(wire["totals"]["live_chats"], 0);
    // Display only: the saved summary seam is present; nothing asked Haiku.
    assert_eq!(wire["intelligence"]["state"], "disabled");
    assert!(dto.backend_lines.is_some());
}

#[tokio::test]
async fn a_change_is_read_at_the_change_pace_a_refresh_at_once_and_a_quiet_window_gets_heartbeats_only()
 {
    let (_dir, home) = home_with_project();
    let source = Arc::new(Counting {
        home,
        reads: AtomicUsize::new(0),
        fail: AtomicBool::new(false),
    });
    let (changes, inbox) = mpsc::sync_channel(1);
    let clock = Arc::new(AtomicUsize::new(10));
    let now = clock.clone();
    let service = OverviewService::start(
        source.clone(),
        inbox,
        Arc::new(move || now.fetch_add(1, Ordering::SeqCst) as u64),
        PACING,
    )
    .unwrap();
    let subscription = service.subscribe();
    next(&subscription).await;
    changes.try_send(()).unwrap();
    let changed = next(&subscription).await;
    assert_eq!(source.reads.load(Ordering::SeqCst), 2);
    service.refresh();
    let refreshed = next(&subscription).await;
    assert!(refreshed.rev > changed.rev);
    assert_eq!(source.reads.load(Ordering::SeqCst), 3);
    // Quiet: the next read is the heartbeat, not the change pace.
    let before = std::time::Instant::now();
    next(&subscription).await;
    assert!(before.elapsed() >= PACING.extras_every - Duration::from_millis(100));
    drop(subscription);
    tokio::time::sleep(PACING.extras_every * 3).await;
    let after = source.reads.load(Ordering::SeqCst);
    tokio::time::sleep(PACING.extras_every * 2).await;
    assert_eq!(
        source.reads.load(Ordering::SeqCst),
        after,
        "no window, no reads"
    );
}

#[tokio::test]
async fn a_failed_read_keeps_the_figures_it_had_and_says_why() {
    let (_dir, home) = home_with_project();
    let source = Arc::new(Counting {
        home,
        reads: AtomicUsize::new(0),
        fail: AtomicBool::new(false),
    });
    let (_changes, inbox) = mpsc::sync_channel(1);
    let clock = Arc::new(Mutex::new(100u64));
    let now = clock.clone();
    let service = OverviewService::start(
        source.clone(),
        inbox,
        Arc::new(move || *now.lock().unwrap()),
        PACING,
    )
    .unwrap();
    let subscription = service.subscribe();
    let good = next(&subscription).await.data;
    source.fail.store(true, Ordering::SeqCst);
    *clock.lock().unwrap() = 160;
    service.refresh();
    let failed = next(&subscription).await.data;
    assert_eq!(failed.now, 160);
    assert_eq!(failed.read_at, Some(100));
    assert_eq!(failed.error.as_deref(), Some("tmux did not answer"));
    assert_eq!(failed.wire, good.wire);
}

struct FakeOrchestrators(Mutex<Vec<Option<String>>>);

fn terminal(id: &str, project: Option<&str>) -> ShellSession {
    ShellSession {
        user_opened: true,
        parent_id: None,
        id: id.into(),
        project_id: project.map(str::to_owned),
        worktree_id: None,
        kind: ShellKind::Orchestrator,
        cwd: "/tmp".into(),
        command: None,
        editor_path: None,
        harness: None,
        unrestricted: false,
        codex_account_id: None,
        codex_account_label: None,
        codex_account_email: None,
        codex_home: None,
        orchestrator_skill_loaded: false,
        orchestrator_skill_version: None,
        orchestrator_delegation_version: None,
        orchestrator_project_root: None,
        created_at_unix: 0,
        alive: true,
    }
}

impl OrchestratorBackend for FakeOrchestrators {
    fn find(&self, _project: Option<&str>) -> Result<Option<Orchestrator>, String> {
        Ok(None)
    }
    fn start(&self, project: Option<&str>) -> Result<Created, String> {
        self.0.lock().unwrap().push(project.map(str::to_owned));
        Ok(Created {
            orchestrator: Orchestrator::Terminal(terminal(
                "6f0c2b1e-1111-4222-8333-444455556666",
                project,
            )),
            created: true,
            fallback: Some("Muse chat on muse-wonder-full-1.0 did not start".into()),
        })
    }
    fn plan(&self, _project: Option<&str>) -> Result<Plan, String> {
        Ok(Plan {
            runs: riwork_core::settings::OrchestratorRuns::Automatic,
            source: PlanSource::Settings,
            notes: Vec::new(),
        })
    }
}

#[tokio::test]
async fn orchestrators_start_only_on_request_validate_ids_and_name_the_fallback() {
    let fake = Arc::new(FakeOrchestrators(Mutex::new(Vec::new())));
    orchestrators::install_backend(fake.clone()).unwrap();
    assert!(
        Orchestrators::find(Some("not-a-uuid".into()))
            .await
            .unwrap_err()
            .starts_with("invalid:")
    );
    assert_eq!(Orchestrators::find(None).await.unwrap(), None);
    assert!(fake.0.lock().unwrap().is_empty(), "find starts nothing");
    let made = Orchestrators::start(None).await.unwrap();
    assert_eq!(made.kind, "terminal");
    assert_eq!(made.title, "G·ORCH · GLOBAL");
    assert_eq!(made.state, "live");
    assert!(made.created);
    assert_eq!(
        made.fallback.as_deref(),
        Some("Muse chat on muse-wonder-full-1.0 did not start")
    );
    let project = "15198d94-7b9d-4a20-b7b5-1b9b76c9501b";
    let made = Orchestrators::start(Some(project.into())).await.unwrap();
    assert_eq!(made.title, "P·ORCH · PROJECT");
    assert_eq!(made.project_id.as_deref(), Some(project));
    let plan = Orchestrators::plan(None).await.unwrap();
    assert_eq!(plan.mode, "automatic");
    assert_eq!(
        plan.automatic,
        ["muse:muse-wonder-full-1.0", "codex:gpt-6.1-sol"]
    );
    assert_eq!(*fake.0.lock().unwrap(), [None, Some(project.to_owned())]);
}
