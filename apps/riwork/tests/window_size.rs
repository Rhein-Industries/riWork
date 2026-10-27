//! Real host geometry → Rust layout → debounced LayoutStore regression route.
use riwork_core::layouts::{LayoutStore, WindowSize};
use riwork_nsh_workspace::{
    host_layouts,
    persistence::{LayoutPersistence, SaveMode},
};
use std::{sync::Arc, time::Duration};

struct Fixture {
    handle: Arc<host_layouts::HostLayout>,
    dir: tempfile::TempDir,
    _runtime: tokio::runtime::Runtime,
}
impl Fixture {
    fn new() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let entered = runtime.enter();
        let dir = tempfile::Builder::new()
            .prefix("riwork-window-size-")
            .tempdir_in("/tmp")
            .unwrap();
        let mut loaded =
            LayoutPersistence::open(dir.path(), "project", None, SaveMode::LegacyChatState)
                .unwrap();
        loaded.project.window_size = WindowSize::new(800., 600.);
        let handle = host_layouts::registration(
            &format!("window-size-{}", dir.path().display()),
            Arc::new(loaded.persistence),
            loaded.project.clone(),
        )
        .unwrap();
        handle.schedule(loaded.project).unwrap();
        handle.flush().unwrap();
        drop(entered);
        Self {
            handle,
            dir,
            _runtime: runtime,
        }
    }
    fn observed(&self, width: f32, height: f32, ordinary: bool, remember: bool) {
        self.handle
            .observe_window_frame(width, height, ordinary, remember)
            .unwrap();
    }
    fn saved(&self) -> Option<WindowSize> {
        LayoutStore::open(self.dir.path())
            .unwrap()
            .window_size("project")
    }
    fn stamp(&self) -> std::time::SystemTime {
        self.dir
            .path()
            .join("layouts.json")
            .metadata()
            .unwrap()
            .modified()
            .unwrap()
    }
}

#[test]
fn an_ordinary_resize_is_saved_after_250_ms() {
    let fixture = Fixture::new();
    fixture.observed(960., 720., true, true);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(fixture.saved(), WindowSize::new(800., 600.));
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(fixture.saved(), WindowSize::new(960., 720.));
    // An older engine model cannot erase accepted host geometry on its next edit.
    let mut stale = fixture.handle.project_layout();
    stale.window_size = WindowSize::new(800., 600.);
    stale.selected_task_id = Some("edit-after-resize".into());
    fixture.handle.schedule(stale).unwrap();
    fixture.handle.flush().unwrap();
    assert_eq!(fixture.saved(), WindowSize::new(960., 720.));
}
#[test]
fn zoomed_and_fullscreen_frames_are_ignored() {
    let fixture = Fixture::new();
    let stamp = fixture.stamp();
    fixture.observed(1440., 900., false, true);
    fixture.observed(1600., 1000., false, true);
    fixture.handle.flush().unwrap();
    assert_eq!(fixture.saved(), WindowSize::new(800., 600.));
    assert_eq!(fixture.stamp(), stamp);
}
#[test]
fn a_move_or_a_return_to_the_settled_size_writes_nothing() {
    let fixture = Fixture::new();
    let stamp = fixture.stamp();
    fixture.observed(800., 600., true, true);
    fixture.observed(950., 700., true, true);
    fixture.observed(800., 600., true, true);
    std::thread::sleep(Duration::from_millis(300));
    fixture.handle.flush().unwrap();
    assert_eq!(fixture.stamp(), stamp);
}
#[test]
fn out_of_range_sizes_are_rejected() {
    let fixture = Fixture::new();
    let stamp = fixture.stamp();
    for (width, height) in [
        (319., 600.),
        (800., 239.),
        (16385., 600.),
        (800., 16385.),
        (f32::NAN, 600.),
        (800., f32::INFINITY),
    ] {
        fixture.observed(width, height, true, true);
    }
    fixture.handle.flush().unwrap();
    assert_eq!(fixture.stamp(), stamp);
    fixture.observed(320., 240., true, true);
    fixture.handle.flush().unwrap();
    assert_eq!(fixture.saved(), WindowSize::new(320., 240.));
}
#[test]
fn remember_off_writes_nothing() {
    let fixture = Fixture::new();
    let stamp = fixture.stamp();
    fixture.observed(1000., 750., true, false);
    fixture.handle.flush().unwrap();
    assert_eq!(fixture.stamp(), stamp);
}
#[test]
fn turning_remember_on_captures_the_current_size() {
    let fixture = Fixture::new();
    fixture.observed(1000., 750., true, false);
    fixture.observed(1000., 750., true, true);
    fixture.handle.flush().unwrap();
    assert_eq!(fixture.saved(), WindowSize::new(1000., 750.));
}
#[test]
fn the_size_follows_the_window_across_a_project_switch() {
    let fixture = Fixture::new();
    fixture.observed(1000., 750., true, true);
    fixture.handle.flush().unwrap();
    let _entered = fixture._runtime.enter();
    let mut loaded = LayoutPersistence::open(
        fixture.dir.path(),
        "project",
        None,
        SaveMode::LegacyChatState,
    )
    .unwrap();
    let next = loaded
        .persistence
        .switch_project("next-project", &loaded.project, None)
        .unwrap();
    assert_eq!(next.project.window_size, WindowSize::new(1000., 750.));
    loaded.persistence.schedule(next.project).unwrap();
    loaded.persistence.flush().unwrap();
    assert_eq!(
        LayoutStore::open(fixture.dir.path())
            .unwrap()
            .window_size("next-project"),
        WindowSize::new(1000., 750.)
    );
}
