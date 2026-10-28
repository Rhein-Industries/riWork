//! Preview layout mechanics with private persistence; no preview worker/GUI.
mod support;
use riwork_core::{
    app_preview::{PreviewMode, PreviewSpec},
    layouts::{LayoutStore, SavedTab},
};
use riwork_nsh_workspace::layout::{Extent, LayoutModel};

#[test]
fn phase6c_preview_spec_persists_and_routing_obeys_main_active_and_focus_mode() {
    for (main, focus, expected) in [(Some(2), false, 2), (None, false, 3), (Some(2), true, 3)] {
        let temp = tempfile::Builder::new()
            .prefix("riwork-preview-layout-")
            .tempdir_in("/tmp")
            .unwrap();
        let mut project = support::fixture();
        project.main_pane = main;
        project.active_pane = 3;
        let mut model = LayoutModel::new(project).unwrap();
        model.dock(Extent {
            width: 1280.,
            height: 800.,
        });
        model.restore_focus(focus, false);
        let spec = PreviewSpec {
            command: String::new(),
            cwd: temp.path().into(),
            mode: PreviewMode::Cooperative,
            ios: None,
        };
        let id = "10000000-0000-4000-8000-000000000060";
        let key = format!("app_preview:{id}");
        model
            .open_tab(SavedTab::AppPreview {
                preview_id: id.into(),
                spec: spec.clone(),
            })
            .unwrap();
        assert_eq!(model.project().active_pane, expected);
        assert!(
            model.project().panes[&expected]
                .tabs
                .iter()
                .any(|tab| tab.key() == key)
        );
        let changed = PreviewSpec {
            command: "fixture command".into(),
            mode: PreviewMode::WindowCapture,
            ..spec
        };
        model.configure_app_preview(&key, changed.clone()).unwrap();
        let store = LayoutStore::open(temp.path()).unwrap();
        store
            .save_chat_state(support::PROJECT, &model.storage_project(), &[], None)
            .unwrap();
        let restored = store.load(support::PROJECT).unwrap().unwrap();
        let preview = restored
            .panes
            .values()
            .flat_map(|pane| &pane.tabs)
            .find(|tab| tab.key() == key)
            .unwrap();
        assert_eq!(
            preview,
            &SavedTab::AppPreview {
                preview_id: id.into(),
                spec: changed
            }
        );
        if model.project().effective_locked_panes().contains(&expected) {
            assert!(model.close_panel(&key, false).is_err());
            model.toggle_lock(expected).unwrap();
        }
        model.close_panel(&key, false).unwrap();
        assert!(
            !model
                .project()
                .panes
                .values()
                .flat_map(|pane| &pane.tabs)
                .any(|tab| tab.key() == key)
        );
    }
}
