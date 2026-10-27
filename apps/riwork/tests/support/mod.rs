#![allow(dead_code)]
use riwork_core::layouts::{
    Axis, Layout, LayoutStore, PanelKind, ProjectLayout, SavedPane, SavedTab, WindowSize,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::Path,
};

pub const PROJECT: &str = "fixture-project";

pub fn fixture() -> ProjectLayout {
    let navigation = [
        PanelKind::Projects,
        PanelKind::Files,
        PanelKind::Worktrees,
        PanelKind::Tasks,
        PanelKind::Shells,
    ]
    .into_iter()
    .map(|panel| SavedTab::Panel { panel })
    .collect();
    let mut layout = Layout::navigation_beside(1, Layout::Pane(2));
    assert!(layout.split_with_ratio(2, Axis::Stacked, 3, false, 0.64));
    let mut project = ProjectLayout {
        layout,
        panes: BTreeMap::from([
            (
                1,
                SavedPane {
                    tabs: navigation,
                    active_tab_key: Some("panel:files".into()),
                    ..Default::default()
                },
            ),
            (
                2,
                SavedPane {
                    tabs: vec![shell("first"), shell("second")],
                    active_tab_key: Some("shell:second".into()),
                    ..Default::default()
                },
            ),
            (3, SavedPane::default()),
        ]),
        active_pane: 2,
        locked_panes: Some(HashSet::from([1, 3])),
        panels_initialized: true,
        detached_shell_ids: HashSet::from(["detached-shell".into()]),
        detached_chat_ids: HashSet::from(["dismissed-chat".into()]),
        chat_dismissal_revision: 7,
        selected_worktree_id: Some("selected-worktree".into()),
        selected_task_id: Some("selected-task".into()),
        sidebar_visible: false,
        window_size: WindowSize::new(1440.0, 900.0),
        main_pane: Some(2),
    };
    project.normalize().unwrap();
    project
}

pub fn shell(id: &str) -> SavedTab {
    SavedTab::Shell {
        shell_id: id.into(),
    }
}

/// GPUI and NSH use this exact production writer. JSON editing below is only a
/// fixture's simulation of a future RiWork release adding an unknown tab kind.
pub fn seed(dir: &Path, with_unknown: bool) -> ProjectLayout {
    let project = fixture();
    LayoutStore::open(dir)
        .unwrap()
        .save_chat_state(PROJECT, &project, &[], None)
        .unwrap();
    if with_unknown {
        let mut file = read(dir);
        file["projects"][PROJECT]["panes"]["3"]["tabs"]
            .as_array_mut()
            .unwrap()
            .push(unknown_tab());
        file["futureTopLevel"] = json!({"preserve": [1, "future", true]});
        write(dir, &file);
    }
    project
}

pub fn unknown_tab() -> Value {
    json!({"kind": "future_browser", "identity": "future-tab", "payload": {"version": 5, "url": "https://example.invalid/"}})
}

pub fn read(dir: &Path) -> Value {
    serde_json::from_slice(&fs::read(dir.join("layouts.json")).unwrap()).unwrap()
}

pub fn write(dir: &Path, value: &Value) {
    fs::write(
        dir.join("layouts.json"),
        serde_json::to_vec_pretty(value).unwrap(),
    )
    .unwrap();
}
