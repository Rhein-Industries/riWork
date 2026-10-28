use super::*;
use crate::{LayoutSession, open_fixture, session_io};
use nsh_core::prelude::Instance;
use riwork_core::{
    layouts::LayoutStore, settings::SettingsStore, store::Store, theme::ThemeChoice,
};
use serde_json::json;

const HOST: &str = "paired-mac";
const PROJECT: &str = "host-project";
const SHELL: &str = "host-shell";

#[test]
fn remote_preferences_skip_local_store_and_session_backends() {
    let home = tempfile::Builder::new()
        .prefix("riwork-remote-dispatch-")
        .tempdir_in("/tmp")
        .unwrap();
    SettingsStore::open(home.path())
        .unwrap()
        .update(|settings| settings.panel_tab_icons = true)
        .unwrap();
    std::fs::write(home.path().join("state.json"), b"unreadable local store").unwrap();
    let key = riwork_engine::remote_tree::project_key(HOST, PROJECT);
    let data = session_io::load(home.path(), &key).unwrap();
    assert_eq!(data.project_id, key);
    assert_eq!(data.theme, ThemeChoice::Native);
    assert!(data.panel_tab_icons);
    assert!(data.root.is_none() && data.shells.is_none() && data.tabs.is_none());
    assert_eq!(
        std::fs::read(home.path().join("state.json")).unwrap(),
        b"unreadable local store"
    );
    for name in ["sessions.json", "project-tabs", "tmux"] {
        assert!(
            !home.path().join(name).exists(),
            "unexpected backend: {name}"
        );
    }
    let untouched = home.path().join("must-not-create");
    assert!(session_io::create(&untouched, &key, None, None).is_err());
    assert!(session_io::tab_snapshot(&untouched, &key).is_err());
    assert!(
        session_io::update(
            &untouched,
            &key,
            riwork_engine::project_tabs::Update::Hide {
                key: "shell:any".into()
            }
        )
        .is_err()
    );
    assert!(!untouched.exists());
}

#[test]
fn local_remote_local_switch_and_remote_restore_keep_identity_and_tabs() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _entered = runtime.enter();
    let home = tempfile::Builder::new()
        .prefix("riwork-remote-roundtrip-")
        .tempdir_in("/tmp")
        .unwrap();
    let root = home.path().join("project");
    std::fs::create_dir(&root).unwrap();
    let store = Store::open(home.path().to_owned()).unwrap();
    let project = store.add_project(root, None).unwrap();
    std::fs::write(
        home.path().join("fake-sessions.json"),
        br#"{"sessions":[]}"#,
    )
    .unwrap();
    session_io::register_fixture(home.path()).unwrap();
    let session = open_fixture(
        home.path().to_owned(),
        json!({"windowId":home.path().display().to_string(),"projectId":project.id}),
    )
    .unwrap();
    let local = session.project_from_host();
    let host = session.host.clone();
    let instance = Instance::new(session);
    let key = riwork_engine::remote_tree::project_key(HOST, PROJECT);
    let state_before = std::fs::read(home.path().join("state.json")).unwrap();
    let sessions_before = std::fs::read(home.path().join("fake-sessions.json")).unwrap();
    let tabs_path = home
        .path()
        .join("project-tabs")
        .join(format!("{}.json", project.id));
    let tabs_before = std::fs::read(&tabs_path).unwrap();
    runtime
        .block_on(LayoutSession::switch_project(instance.clone(), key.clone()))
        .unwrap();
    assert_eq!(host.project_id(), key);
    instance.with_mut(|session| {
        assert_eq!(session.snapshot()["remoteProject"], true);
        assert!(session.open_shell("local-adoption".into()).is_err());
        assert!(session.open_editor_shell("local-editor".into()).is_err());
        assert!(session.show_shell("local-navigation".into()).is_err());
        session
            .set_selected_worktree(Some("host-worktree".into()))
            .unwrap();
        session.set_selected_task(Some("host-task".into())).unwrap();
        session
            .open_remote_shell(HOST.into(), SHELL.into())
            .unwrap();
        session
            .open_remote_shell(HOST.into(), SHELL.into())
            .unwrap();
        assert_eq!(
            session
                .model
                .project()
                .panes
                .values()
                .flat_map(|pane| &pane.tabs)
                .filter(|tab| tab.key() == format!("remote:{HOST}:{SHELL}"))
                .count(),
            1
        );
        session.set_focus_mode(true, true).unwrap();
    });
    let count = instance.with(|session| session.model.project().layout.pane_ids().len());
    runtime
        .block_on(LayoutSession::new_shell(
            instance.clone(),
            json!({"split":"right"}),
        ))
        .unwrap();
    assert_eq!(
        instance.with(|session| session.model.project().layout.pane_ids().len()),
        count + 1
    );
    instance.with(|session| {
        let project = session.model.project();
        assert!(project.panes[&project.active_pane].tabs.is_empty());
        assert!(!session.model.focus_mode());
    });
    assert!(
        runtime
            .block_on(LayoutSession::new_shell(instance.clone(), json!({})))
            .is_err()
    );
    runtime
        .block_on(LayoutSession::close_panel(
            instance.clone(),
            format!("remote:{HOST}:{SHELL}"),
            true,
        ))
        .unwrap();
    runtime
        .block_on(LayoutSession::refresh_tabs(instance.clone()))
        .unwrap();
    instance.with_mut(|session| {
        session
            .open_remote_shell(HOST.into(), SHELL.into())
            .unwrap();
    });
    host.flush().unwrap();
    let remote = host.snapshot();
    assert_eq!(
        std::fs::read(home.path().join("state.json")).unwrap(),
        state_before
    );
    assert_eq!(
        std::fs::read(home.path().join("fake-sessions.json")).unwrap(),
        sessions_before
    );
    assert_eq!(std::fs::read(&tabs_path).unwrap(), tabs_before);
    assert!(
        !home
            .path()
            .join("project-tabs")
            .join(format!("{key}.json"))
            .exists()
    );
    runtime
        .block_on(LayoutSession::switch_project(
            instance.clone(),
            project.id.clone(),
        ))
        .unwrap();
    assert_eq!(host.project_id(), project.id);
    assert_eq!(host.snapshot(), local);
    assert_eq!(
        instance.with(|session| session.snapshot()["remoteProject"].clone()),
        false
    );
    assert!(
        instance
            .with_mut(|session| session.set_selected_task(Some("host-task".into())))
            .is_err()
    );
    instance.dispose();
    let restored = open_fixture(home.path().to_owned(), json!({"windowId":format!("{}-restore", home.path().display()),"projectId":key,"restore":remote})).unwrap();
    assert_eq!(restored.snapshot()["remoteProject"], true);
    assert_eq!(restored.host.project_id(), key);
    assert_eq!(restored.snapshot()["selectedWorktreeId"], "host-worktree");
    assert_eq!(restored.snapshot()["selectedTaskId"], "host-task");
    assert!(
        restored
            .model
            .project()
            .panes
            .values()
            .flat_map(|pane| &pane.tabs)
            .any(|tab| tab.key() == format!("remote:{HOST}:{SHELL}"))
    );
    restored.host.flush().unwrap();
    assert_eq!(
        LayoutStore::open(home.path())
            .unwrap()
            .load(&key)
            .unwrap()
            .unwrap(),
        remote
    );
}

#[test]
fn remote_keys_follow_engine_parser_and_bad_dispatch_never_reads_home() {
    for key in ["remote:host:project", "remote:host:project:detail"] {
        assert!(is_project(key));
        validate_project(key).unwrap();
    }
    for key in ["remote:", "remote:host", "remote::project", "remote:host:"] {
        assert!(!is_project(key));
        assert!(validate_project(key).is_err());
        assert!(load(Path::new("/must-not-read"), key).is_err());
    }
    assert!(shell(HOST.into(), "with:separator".into()).is_err());
}
