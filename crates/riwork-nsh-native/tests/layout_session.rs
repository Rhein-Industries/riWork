//! The production workspace bridge, backed by explicit disposable sessions.
use futures_util::FutureExt;
use nsh_core::prelude::Instance;
use riwork_core::{
    layouts::{Axis, LayoutStore, SavedTab},
    store::Store,
};
use riwork_nsh_workspace::{
    LayoutSession, open_fixture, persistence::default_project_layout, session_io,
};
use serde_json::{Value, json};
use std::{fs, path::PathBuf};

struct Fixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
    project: String,
    root: PathBuf,
    runtime: tokio::runtime::Runtime,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::Builder::new()
            .prefix("riwork-w1-session-")
            .tempdir_in("/tmp")
            .unwrap();
        let home = temp.path().join("home");
        let root = temp.path().join("repo");
        fs::create_dir(&root).unwrap();
        let project = Store::open(&home)
            .unwrap()
            .add_project(&root, Some("Fixture"))
            .unwrap()
            .id;
        fs::write(home.join("fake-sessions.json"), br#"{"sessions":[]}"#).unwrap();
        session_io::register_fixture(&home).unwrap();
        Self {
            _temp: temp,
            home,
            project,
            root,
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .max_blocking_threads(4)
                .build()
                .unwrap(),
        }
    }
    fn fake(&self, value: Value) {
        fs::write(
            self.home.join("fake-sessions.json"),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
    }
    fn shell(&self, n: u64, alive: bool) -> Value {
        json!({"id":format!("00000000-0000-4000-8000-{n:012}"),"project_id":self.project,"cwd":self.root,"kind":"project","alive":alive,"user_opened":true})
    }
    fn open(&self, name: &str, restore: Option<Value>) -> LayoutSession {
        let _entered = self.runtime.enter();
        open_fixture(self.home.clone(),json!({"windowId":format!("{}-{name}",self.project),"projectId":self.project,"restore":restore})).unwrap()
    }
    fn id(n: u64) -> String {
        format!("00000000-0000-4000-8000-{n:012}")
    }
}

#[test]
fn no_layout_opens_riwork_cores_default() {
    let f = Fixture::new();
    let session = f.open("default", None);
    let p = session.project_from_host();
    let default = default_project_layout();
    assert_eq!(p.layout, default.layout);
    assert_eq!(p.panes[&1].tabs, default.panes[&1].tabs);
    assert_eq!(p.effective_locked_panes(), default.effective_locked_panes());
    assert_eq!(p.panes[&2].tabs.len(), 1);
    let fake: Value =
        serde_json::from_slice(&fs::read(f.home.join("fake-sessions.json")).unwrap()).unwrap();
    assert_eq!(
        p.panes[&2].tabs[0].key(),
        format!("shell:{}", fake["sessions"][0]["id"].as_str().unwrap())
    );
}

#[test]
fn split_right_adds_the_new_pane_second_at_one_half() {
    let f = Fixture::new();
    let session = Instance::new(f.open("split", None));
    let before = session.with(|s| s.project_from_host());
    f.runtime
        .block_on(LayoutSession::new_shell(
            session.clone(),
            json!({"split":"right"}),
        ))
        .unwrap();
    let after = session.with(|s| s.project_from_host());
    assert_eq!(
        after.layout.pane_ids().len(),
        before.layout.pane_ids().len() + 1
    );
    assert_ne!(after.active_pane, before.active_pane);
    let mut expected = before.layout.clone();
    assert!(expected.split(before.active_pane, Axis::SideBySide, after.active_pane));
    assert_eq!(after.layout, expected);
    assert_eq!(after.panes[&after.active_pane].tabs.len(), 1);
}

#[test]
fn a_failed_create_undoes_the_split() {
    let f = Fixture::new();
    let session = Instance::new(f.open("failure", None));
    let before = session.with(|s| s.project_from_host());
    f.fake(json!({"sessions":[],"create_errors":{&f.project:"Fixture shell creation failed"}}));
    let error = f
        .runtime
        .block_on(LayoutSession::new_shell(
            session.clone(),
            json!({"split":"right"}),
        ))
        .unwrap_err();
    assert_eq!(error, "Fixture shell creation failed");
    assert_eq!(session.with(|s| s.project_from_host()), before);
}

#[test]
fn new_shell_uses_the_selected_worktree_else_the_root() {
    let f = Fixture::new();
    let _entered = f.runtime.enter();
    let root = session_io::create(&f.home, &f.project, None, None).unwrap();
    assert_eq!(root.cwd, f.root.canonicalize().unwrap());
    let worktree = f._temp.path().join("worktree");
    fs::create_dir(&worktree).unwrap();
    let selected = session_io::create(&f.home, &f.project, Some(worktree.clone()), None).unwrap();
    assert_eq!(selected.cwd, worktree);
}

#[test]
fn restore_skips_dead_and_detached_shells() {
    let f = Fixture::new();
    let mut p = default_project_layout();
    p.panes.get_mut(&2).unwrap().tabs = (1..=2)
        .map(|n| SavedTab::Shell {
            shell_id: Fixture::id(n),
        })
        .collect();
    p.detached_shell_ids.insert(Fixture::id(3));
    p.normalize().unwrap();
    f.fake(json!({"sessions":[f.shell(1,true),f.shell(2,false),f.shell(3,true)]}));
    let s = f.open("reconcile", Some(json!(p)));
    let p = s.project_from_host();
    assert_eq!(
        p.panes[&2].tabs,
        vec![SavedTab::Shell {
            shell_id: Fixture::id(1)
        }]
    );
}

#[test]
fn restore_adopts_cli_shells_into_main() {
    let f = Fixture::new();
    let p = default_project_layout();
    f.fake(json!({"sessions":[f.shell(1,true),f.shell(2,true)]}));
    let s = f.open("adopt", Some(json!(p.clone())));
    let actual = s.project_from_host();
    assert_eq!(actual.active_pane, p.active_pane);
    assert_eq!(actual.panes[&2].tabs.len(), 2);
}

#[test]
fn adoption_caps_at_16_per_apply() {
    let f = Fixture::new();
    f.fake(json!({"sessions":(1..=24).map(|n|f.shell(n,true)).collect::<Vec<_>>()}));
    let s = f.open("cap", Some(json!(default_project_layout())));
    assert_eq!(s.project_from_host().panes[&2].tabs.len(), 16);
}

#[test]
fn all_locked_adopts_nothing() {
    let f = Fixture::new();
    let mut p = default_project_layout();
    p.locked_panes = Some([1, 2].into());
    f.fake(json!({"sessions":[f.shell(1,true)]}));
    let s = f.open("locked", Some(json!(p)));
    assert!(s.project_from_host().panes[&2].tabs.is_empty());
}

#[test]
fn a_failed_restore_retries_on_the_next_sessions_delivery_and_keeps_positions() {
    let f = Fixture::new();
    let mut p = default_project_layout();
    p.panes.get_mut(&2).unwrap().tabs = vec![SavedTab::Shell {
        shell_id: Fixture::id(1),
    }];
    p.normalize().unwrap();
    f.fake(json!({"sessions":[],"list_errors":{&f.project:"Fixture inventory failed"}}));
    let s = Instance::new(f.open("retry", Some(json!(p.clone()))));
    assert_eq!(s.with(|s| s.project_from_host()), p);
    assert_eq!(
        s.with(|s| s.snapshot())["notice"],
        "Fixture inventory failed"
    );
    f.fake(json!({"sessions":[f.shell(1,true)]}));
    f.runtime
        .block_on(LayoutSession::refresh_sessions(s.clone()))
        .unwrap();
    assert_eq!(s.with(|s| s.project_from_host()), p);
    assert!(s.with(|s| s.snapshot())["notice"].is_null());
}

#[test]
fn chat_pending_is_never_saved_and_maps_to_chat_id() {
    let f = Fixture::new();
    let mut s = f.open("pending", None);
    s.open_pending_chat("pending-one".into()).unwrap();
    assert!(
        s.project_from_host()
            .panes
            .values()
            .all(|p| p.tabs.iter().all(|t| !t.key().contains("pending")))
    );
    s.resolve_pending_chat("chat-pending:pending-one".into(), "chat-one".into())
        .unwrap();
    assert_eq!(
        s.panel("chat-pending:pending-one".into()),
        json!({"kind":"chat","chat_id":"chat-one"})
    );
    assert!(s.snapshot()["panes"].as_array().unwrap().iter().any(|p| {
        p["tabs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["id"] == "chat-pending:pending-one")
    }));
    s.flush_from_host().unwrap();
    let loaded = LayoutStore::open(&f.home)
        .unwrap()
        .load(&f.project)
        .unwrap()
        .unwrap();
    assert!(
        loaded
            .panes
            .values()
            .any(|p| p.tabs.iter().any(|t| t.key() == "chat:chat-one"))
    );
}

#[test]
fn focus_mode_never_changes_the_saved_layout() {
    let f = Fixture::new();
    let mut s = f.open("focus", None);
    let before = s.project_from_host();
    s.toggle_focus().unwrap();
    s.set_extent(json!({"width":1920,"height":1080})).unwrap();
    assert_eq!(s.project_from_host(), before);
    assert_eq!(s.snapshot()["focusMode"], true);
    s.toggle_focus().unwrap();
    assert_eq!(s.project_from_host(), before);
}

#[test]
fn two_windows_detach_without_stopping_then_exit_records_close() {
    let f = Fixture::new();
    let one = Instance::new(f.open("one", None));
    let two = Instance::new(f.open("two", None));
    let id = one.with(|s| s.project_from_host().panes[&2].tabs[0].key());
    f.runtime
        .block_on(LayoutSession::close_panel(one.clone(), id.clone(), false))
        .unwrap();
    f.runtime
        .block_on(LayoutSession::refresh_tabs(two.clone()))
        .unwrap();
    assert!(two.with(|s| s.panel(id.clone())).is_null());
    let _entered = f.runtime.enter();
    assert!(session_io::list(&f.home).unwrap()[0].alive);
    f.runtime
        .block_on(LayoutSession::new_shell(one.clone(), json!({})))
        .unwrap();
    let next = one.with(|s| s.project_from_host().panes[&2].tabs[0].key());
    f.runtime
        .block_on(LayoutSession::refresh_tabs(one.clone()))
        .unwrap();
    f.runtime
        .block_on(LayoutSession::close_panel(one, next.clone(), true))
        .unwrap();
    let fake: Value =
        serde_json::from_slice(&fs::read(f.home.join("fake-sessions.json")).unwrap()).unwrap();
    assert_eq!(
        fake["close_calls"],
        json!([next.trim_start_matches("shell:")])
    );
}

#[test]
fn a_restored_empty_main_pane_does_not_create_a_shell() {
    let f = Fixture::new();
    let p = default_project_layout();
    LayoutStore::open(&f.home)
        .unwrap()
        .save_chat_state(&f.project, &p, &[], None)
        .unwrap();
    let s = f.open("empty", None);
    assert!(s.project_from_host().panes[&2].tabs.is_empty());
    let _entered = f.runtime.enter();
    assert!(session_io::list(&f.home).unwrap().is_empty());
}

// The session owns one long-lived persistence worker. Occupy the remaining
// blocking slots until each async bridge request has captured its generation.
fn block_background_io() -> Vec<(std::sync::mpsc::Sender<()>, tokio::task::JoinHandle<()>)> {
    (0..3)
        .map(|_| {
            let (release, wait) = std::sync::mpsc::channel();
            let worker = tokio::task::spawn_blocking(move || wait.recv().unwrap());
            (release, worker)
        })
        .collect()
}

#[test]
fn a_failed_default_shell_retry_is_single_flight_and_clears_its_notice() {
    let f = Fixture::new();
    f.fake(json!({"sessions":[],"create_errors":{&f.project:"Fixture create failed"}}));
    let s = Instance::new(f.open("initial-retry", None));
    assert_eq!(s.with(|s| s.snapshot())["notice"], "Fixture create failed");
    f.fake(json!({"sessions":[]}));
    f.runtime.block_on(async {
        let blockers = block_background_io();
        let mut one = Box::pin(LayoutSession::refresh_tabs(s.clone()));
        let mut two = Box::pin(LayoutSession::refresh_tabs(s.clone()));
        assert!(one.as_mut().now_or_never().is_none());
        assert!(two.as_mut().now_or_never().is_none());
        for (release, blocker) in blockers {
            release.send(()).unwrap();
            blocker.await.unwrap();
        }
        one.await.unwrap();
        two.await.unwrap();
    });
    let fake: Value =
        serde_json::from_slice(&fs::read(f.home.join("fake-sessions.json")).unwrap()).unwrap();
    assert_eq!(fake["sessions"].as_array().unwrap().len(), 1);
    assert!(s.with(|s| s.snapshot())["notice"].is_null());
    assert_eq!(s.with(|s| s.project_from_host()).panes[&2].tabs.len(), 1);
}

fn destination(f: &Fixture) -> String {
    let root = f._temp.path().join("destination");
    fs::create_dir(&root).unwrap();
    let project = Store::open(&f.home)
        .unwrap()
        .add_project(&root, Some("Destination"))
        .unwrap()
        .id;
    LayoutStore::open(&f.home)
        .unwrap()
        .save_chat_state(&project, &default_project_layout(), &[], None)
        .unwrap();
    project
}

#[test]
fn a_shell_completion_cannot_cross_a_project_switch() {
    let f = Fixture::new();
    let s = Instance::new(f.open("create-switch", None));
    let destination = destination(&f);
    f.runtime.block_on(async {
        let blockers = block_background_io();
        let mut create = Box::pin(LayoutSession::new_shell(
            s.clone(),
            json!({"split":"right"}),
        ));
        assert!(create.as_mut().now_or_never().is_none());
        let mut switch = Box::pin(LayoutSession::switch_project(
            s.clone(),
            destination.clone(),
        ));
        assert!(switch.as_mut().now_or_never().is_none());
        assert_eq!(
            s.with_mut(|s| s.toggle_lock(2)).unwrap_err(),
            "conflict: a project switch is in progress"
        );
        assert_eq!(
            LayoutSession::switch_project(s.clone(), f.project.clone())
                .await
                .unwrap_err(),
            "conflict: a project switch is in progress"
        );
        for (release, blocker) in blockers {
            release.send(()).unwrap();
            blocker.await.unwrap();
        }
        switch.await.unwrap();
        create.await.unwrap();
    });
    assert_eq!(s.with(|s| s.snapshot())["projectId"], destination);
    assert_eq!(s.with(|s| s.project_from_host()), default_project_layout());
    let fake: Value =
        serde_json::from_slice(&fs::read(f.home.join("fake-sessions.json")).unwrap()).unwrap();
    assert_eq!(fake["sessions"].as_array().unwrap().len(), 2);
    assert!(
        fake["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|shell| shell["project_id"] == f.project)
    );
}

#[test]
fn a_pane_close_completion_cannot_close_the_destination_pane() {
    let f = Fixture::new();
    let s = Instance::new(f.open("close-switch", None));
    let destination = destination(&f);
    f.runtime.block_on(async {
        let blockers = block_background_io();
        let mut close = Box::pin(LayoutSession::close_pane(s.clone(), 2));
        assert!(close.as_mut().now_or_never().is_none());
        let mut switch = Box::pin(LayoutSession::switch_project(s.clone(), destination));
        assert!(switch.as_mut().now_or_never().is_none());
        for (release, blocker) in blockers {
            release.send(()).unwrap();
            blocker.await.unwrap();
        }
        switch.await.unwrap();
        close.await.unwrap();
    });
    assert_eq!(s.with(|s| s.project_from_host()), default_project_layout());
}

#[test]
fn a_failed_switch_releases_its_edit_guard() {
    let f = Fixture::new();
    let s = Instance::new(f.open("switch-error", None));
    let destination = destination(&f);
    fs::write(f.home.join("settings.json"), b"{").unwrap();
    assert!(
        f.runtime
            .block_on(LayoutSession::switch_project(s.clone(), destination))
            .is_err()
    );
    assert_eq!(s.with(|s| s.snapshot())["projectId"], f.project);
    s.with_mut(|s| s.toggle_lock(2)).unwrap();
}

#[test]
fn fake_chats_keep_runtime_status_and_workers_keep_parent_identity() {
    let f = Fixture::new();
    let mut worker = f.shell(2, true);
    worker["user_opened"] = json!(false);
    worker["parent_id"] = json!(Fixture::id(1));
    let chat = Fixture::id(3);
    f.fake(json!({"sessions":[f.shell(1,true),worker],"chats":[{"id":chat,"project_id":f.project,"title":"Project chat","status":"waiting"}]}));
    let mut p = default_project_layout();
    p.panes.get_mut(&2).unwrap().tabs.push(SavedTab::Chat {
        chat_id: chat.clone(),
    });
    p.normalize().unwrap();
    let s = f.open("chat-status", Some(json!(p)));
    assert!(!s.panel(format!("chat:{chat}")).is_null());
    let _entered = f.runtime.enter();
    let tabs = session_io::load(&f.home, &f.project).unwrap().tabs.unwrap();
    assert_eq!(
        tabs.entries
            .iter()
            .find(|e| e.key == format!("chat:{chat}"))
            .unwrap()
            .status,
        riwork_engine::project_tabs::Status::Waiting
    );
    let worker = tabs
        .entries
        .iter()
        .find(|e| e.key == format!("shell:{}", Fixture::id(2)))
        .unwrap();
    assert_eq!(worker.parent, Some(format!("shell:{}", Fixture::id(1))));
    assert!(worker.worker);
}
