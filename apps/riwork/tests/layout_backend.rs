//! Exercise the module/class through a fake repository and persistence adapter.
use nsh_core::prelude::Registry;
use riwork_core::layouts::{ProjectLayout, SavedTab};
use riwork_nsh_workspace::{
    LayoutRepository, Layouts, OpenedLayout, install_layout_repository,
    persistence::{LayoutBackend, default_project_layout},
    register_layouts,
};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};

#[derive(Default)]
struct FakeBackend {
    saves: AtomicUsize,
    flushes: AtomicUsize,
}
impl LayoutBackend for FakeBackend {
    fn schedule(&self, _: ProjectLayout) -> Result<(), String> {
        self.saves.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn flush(&self) -> Result<(), String> {
        self.flushes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
struct FakeRepository(Arc<FakeBackend>);
impl LayoutRepository for FakeRepository {
    fn open(
        &self,
        _: PathBuf,
        _: String,
        _: Option<ProjectLayout>,
    ) -> Result<OpenedLayout, String> {
        let mut project = default_project_layout();
        project
            .panes
            .get_mut(&2)
            .unwrap()
            .tabs
            .extend((0..24).map(|index| SavedTab::Shell {
                shell_id: format!("fixture-{index}"),
            }));
        Ok(OpenedLayout {
            backend: self.0.clone(),
            project,
            notice: None,
            unknown_panes: Default::default(),
        })
    }
}

fn fixture_home() -> Result<PathBuf, String> {
    static HOME: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    Ok(HOME
        .get_or_init(|| {
            tempfile::Builder::new()
                .prefix("riwork-layout-backend-")
                .tempdir_in("/tmp")
                .unwrap()
        })
        .path()
        .to_path_buf())
}

#[tokio::test(flavor = "multi_thread")]
async fn layout_module_uses_the_injected_backend_and_sync_calls_do_not_wait_for_it() {
    let backend = Arc::new(FakeBackend::default());
    install_layout_repository(Arc::new(FakeRepository(backend.clone()))).unwrap();
    // Layout state stays in the injected backend; session metadata reads
    // (Store/settings) use a disposable home rather than a real one.
    register_layouts(&mut Registry::new(), fixture_home);
    let mut session = Layouts::open_session(
        json!({"windowId":"fake-layout-window", "projectId":"fake-layout-project"}),
    )
    .await
    .unwrap();
    let mut timings: Vec<(&str, Vec<u128>)> = Vec::new();
    macro_rules! measure {
        ($name:literal, $work:expr) => {{
            let mut values = Vec::new();
            for _ in 0..41 {
                let start = Instant::now();
                $work;
                values.push(start.elapsed().as_micros());
            }
            values.sort();
            timings.push(($name, values));
        }};
    }
    measure!("dock", {
        session.dock(json!({"width":1220,"height":780})).unwrap();
    });
    measure!("snapshot", {
        session.snapshot();
    });
    measure!("panel", {
        session.panel("shell:fixture-0".into());
    });
    measure!("selectPanel", {
        session.select_panel("panel:projects".into()).unwrap();
    });
    measure!("openPanel", {
        session.open_panel("projects".into()).unwrap();
    });
    measure!("setExtent", {
        session
            .set_extent(json!({"width":1220,"height":780}))
            .unwrap();
    });
    measure!("selectPane", {
        session.select_pane(2).unwrap();
    });
    measure!("toggleLock", {
        session.toggle_lock(1).unwrap();
    });
    measure!("toggleMain", {
        session.toggle_main(2).unwrap();
        session.toggle_main(2).unwrap();
    });
    // The pair above restores the existing main pane, independent of parity.
    measure!("gather", {
        session.gather().unwrap();
    });
    measure!("defaultLayout", {
        session.default_layout().unwrap();
    });
    let dock = session.dock(json!({"width":1220,"height":780})).unwrap();
    measure!("applyDock", {
        session
            .apply_dock_model(dock.clone(), json!({"width":1220,"height":780}))
            .unwrap();
    });
    assert!(backend.saves.load(Ordering::SeqCst) > 1);
    for (name, values) in timings {
        println!(
            "sync {name}: median={}us p90={}us max={}us (24 session tabs)",
            values[20], values[36], values[40]
        );
    }
    let host = riwork_nsh_workspace::host_layouts::lookup("fake-layout-window").unwrap();
    host.flush().unwrap();
    assert_eq!(backend.flushes.load(Ordering::SeqCst), 1);
    host.begin_close();
    assert!(session.gather().unwrap_err().starts_with("unavailable:"));
}
