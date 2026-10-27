//! Named acceptance fixtures. Valid layouts always use the production GPUI
//! LayoutStore writer; legacy/future payloads are the core test literals.
#![allow(dead_code)]
use riwork_core::{
    app_preview::{PreviewMode, PreviewSpec},
    layouts::{Axis, Layout, LayoutStore, PanelKind, ProjectLayout, SavedPane, SavedTab},
};
use riwork_nsh_workspace::persistence::default_project_layout;
use serde_json::Value;
use std::{collections::HashSet, fs, path::Path};

pub const NAMES: &[&str] = &[
    "L-NAV",
    "L-NESTED",
    "L-EMPTY",
    "L-UNKNOWN",
    "L-PREVIEW-REMOTE",
    "L-LEGACY",
    "L-GLOBAL-TABS",
    "L-UNLOCK",
    "L-BIGID",
    "L-RATIO",
    "L-CARRY",
    "L-CARRY-DEST",
    "L-MAIN",
];

pub fn shell(id: impl Into<String>) -> SavedTab {
    SavedTab::Shell {
        shell_id: id.into(),
    }
}
fn pane(tabs: Vec<SavedTab>) -> SavedPane {
    SavedPane {
        active_tab_key: tabs.last().map(SavedTab::key),
        tabs,
        ..SavedPane::default()
    }
}
pub fn layout(name: &str) -> ProjectLayout {
    let mut p = default_project_layout();
    p.locked_panes = None;
    p.panes.insert(
        2,
        pane(vec![
            shell(format!("{name}-S1")),
            shell(format!("{name}-S2")),
            SavedTab::Chat {
                chat_id: format!("{name}-chat"),
            },
            SavedTab::RemoteShell {
                desktop_id: "desk".into(),
                shell_id: format!("{name}-remote"),
            },
        ]),
    );
    match name {
        "L-NAV" => {}
        "L-NESTED" => {
            p.layout = Layout::Split {
                axis: Axis::SideBySide,
                ratio: 0.30,
                first: Box::new(Layout::Pane(1)),
                second: Box::new(Layout::Split {
                    axis: Axis::Stacked,
                    ratio: 0.62,
                    first: Box::new(Layout::Pane(2)),
                    second: Box::new(Layout::Split {
                        axis: Axis::Stacked,
                        ratio: 0.50,
                        first: Box::new(Layout::Pane(3)),
                        second: Box::new(Layout::Pane(4)),
                    }),
                }),
            };
            p.panes.insert(3, pane(vec![shell("L-NESTED-S3")]));
            p.panes.insert(4, pane(vec![shell("L-NESTED-S4")]));
            p.active_pane = 3;
        }
        "L-EMPTY" => {
            p.layout.split_with_ratio(2, Axis::Stacked, 3, false, 0.64);
            p.panes.insert(2, SavedPane::default());
            p.panes.insert(3, SavedPane::default());
            p.locked_panes = Some(HashSet::from([1, 3]));
        }
        "L-UNKNOWN" => {
            p.panes
                .insert(2, pane(vec![shell("L-UNKNOWN-S1"), shell("L-UNKNOWN-S2")]));
        }
        "L-PREVIEW-REMOTE" => {
            p.panes.insert(
                2,
                pane(vec![
                    SavedTab::RemoteShell {
                        desktop_id: "desk".into(),
                        shell_id: "remote-shell".into(),
                    },
                    SavedTab::AppPreview {
                        preview_id: "12345678-1234-4234-8234-123456789012".into(),
                        spec: PreviewSpec {
                            command: "fixture command must never run".into(),
                            cwd: "/tmp/riwork-preview-fixture".into(),
                            mode: PreviewMode::Cooperative,
                            ios: None,
                        },
                    },
                ]),
            );
        }
        "L-LEGACY" => return serde_json::from_str(LEGACY).unwrap(),
        "L-GLOBAL-TABS" => {
            p.panes.insert(
                2,
                pane(vec![
                    SavedTab::Panel {
                        panel: PanelKind::Usage,
                    },
                    SavedTab::Panel {
                        panel: PanelKind::Dashboard,
                    },
                    SavedTab::Chat {
                        chat_id: "global-chat".into(),
                    },
                    shell("global-orchestrator"),
                ]),
            );
        }
        "L-UNLOCK" => {
            p.locked_panes = Some(HashSet::new());
            p.main_pane = None;
        }
        "L-BIGID" => {
            let id = (1_u64 << 53) + 5;
            p.layout = Layout::navigation_beside(1, Layout::Pane(id));
            let tabs = p.panes.remove(&2).unwrap();
            p.panes.insert(id, tabs);
            p.active_pane = id;
            p.main_pane = Some(id);
            p.locked_panes = Some(HashSet::from([1, id]));
        }
        "L-RATIO" => {
            p.layout = Layout::Split {
                axis: Axis::SideBySide,
                ratio: 0.02,
                first: Box::new(Layout::Pane(1)),
                second: Box::new(Layout::Split {
                    axis: Axis::Stacked,
                    ratio: 0.98,
                    first: Box::new(Layout::Pane(2)),
                    second: Box::new(Layout::Pane(3)),
                }),
            };
            p.panes.insert(3, pane(vec![shell("L-RATIO-S3")]));
            p.locked_panes = Some(HashSet::from([1, 3]));
        }
        "L-CARRY" | "L-CARRY-DEST" => {
            p.layout.split_with_ratio(2, Axis::Stacked, 3, false, 0.62);
            p.panes.insert(
                1,
                pane(vec![
                    SavedTab::Panel {
                        panel: PanelKind::Files,
                    },
                    shell(format!("{name}-locked")),
                ]),
            );
            p.panes.insert(3, pane(vec![shell(format!("{name}-S3"))]));
            p.locked_panes = Some(HashSet::from([1]));
        }
        "L-MAIN" => {
            p.layout.split_with_ratio(2, Axis::Stacked, 3, false, 0.62);
            p.panes.insert(3, pane(vec![shell("L-MAIN-S3")]));
            p.main_pane = Some(3);
        }
        _ => panic!("unknown layout fixture: {name}"),
    }
    p.normalize().unwrap();
    p
}

// legacy_shell_layout_migrates_without_changing_tab_order_or_selection and
// mixed_tabs_preserve_order_and_selection_while_migrating_bottom_strips.
pub const LEGACY: &str = r#"{
    "layout": {"split": {"axis":"side_by_side", "first":{"pane":4}, "second":{"pane":8}}},
    "panes": {
        "4":{"shell_ids":["shell-b","shell-a"],"active_shell_id":"shell-a","tab_edge":"bottom"},
        "8":{"shell_ids":["shell-a","shell-c"],"active_shell_id":"shell-a"}
    }, "active_pane":8
}"#;
pub const UNKNOWN: &str = r#"{ "kind": "browser", "url": "https://example.com" }"#;
pub const FUTURE: &str = r#"{ "keep": [1, 2, 3], "escaped": "\u0061" }"#;
pub const UNREADABLE: &str =
    r#"{ "layout": {"future_grid": [1,2]}, "panes": {}, "active_pane": 1 }"#;
pub const CORRUPT: &[u8] = b"{\"schema_version\": 1, \"projects\": {";
pub const NEWER: &[u8] = br#"{"schema_version":2,"projects":["future format"]}"#;

pub fn seed(home: &Path, project: &str, name: &str) -> ProjectLayout {
    let store = LayoutStore::open(home).unwrap();
    let input = layout(name);
    store.save_chat_state(project, &input, &[], None).unwrap();
    if matches!(name, "L-LEGACY" | "L-UNKNOWN") {
        let mut file: Value =
            serde_json::from_slice(&fs::read(home.join("layouts.json")).unwrap()).unwrap();
        if name == "L-LEGACY" {
            file["projects"][project] = serde_json::from_str(LEGACY).unwrap();
        } else {
            file["projects"][project]["panes"]["2"]["tabs"]
                .as_array_mut()
                .unwrap()
                .insert(1, serde_json::from_str(UNKNOWN).unwrap());
            file["future_top_level"] = serde_json::from_str(FUTURE).unwrap();
        }
        fs::write(
            home.join("layouts.json"),
            serde_json::to_vec_pretty(&file).unwrap(),
        )
        .unwrap();
    }
    store.load(project).unwrap().unwrap()
}

pub fn damaged(home: &Path, project: &str, name: &str) {
    fs::create_dir_all(home).unwrap();
    let bytes = match name {
        "D-CORRUPT" => CORRUPT.to_vec(),
        "D-NEWER" => NEWER.to_vec(),
        "D-ENTRY" => format!(
            "{{\"schema_version\":1,\"projects\":{{{}:{UNREADABLE},\"good\":{}}}}}",
            serde_json::to_string(project).unwrap(),
            serde_json::to_string(&layout("L-NAV")).unwrap()
        )
        .into_bytes(),
        _ => panic!("unknown damage fixture"),
    };
    fs::write(home.join("layouts.json"), bytes).unwrap();
}
