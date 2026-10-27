//! Compatibility oracle: fixtures are written by the same LayoutStore the GPUI
//! build calls. Every test owns an explicit directory below /tmp; no live home.
mod support;

use riwork_core::{
    app_preview::{PreviewMode, PreviewSpec},
    layouts::{Axis, Extent, Layout, LayoutStore, ProjectLayout, SavedPane, SavedTab},
};
use riwork_nsh_workspace::{
    layout::{DockNode, LayoutModel},
    persistence::{LayoutPersistence, SAVE_DELAY, SaveMode, default_project_layout},
};
use serde_json::json;
use std::{
    collections::HashSet,
    fs,
    time::{Duration, Instant},
};
use support::{PROJECT, fixture, read, seed, unknown_tab, write};

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

fn scratch() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("riwork-p1c-layout-")
        .tempdir_in("/tmp")
        .unwrap()
}

fn area() -> Extent {
    Extent {
        width: 1280.0,
        height: 800.0,
    }
}

#[test]
fn the_default_layout_closes_nothing_and_preserves_selection_and_navigation_width() {
    let mut project = fixture();
    let original_tabs: HashSet<_> = project
        .panes
        .values()
        .flat_map(|pane| pane.tabs.iter().map(SavedTab::key))
        .collect();
    riwork_core::layouts::apply_default_layout(&mut project).unwrap();
    let (navigation, main) = project.layout.two_panes_side_by_side().unwrap();
    assert_eq!(project.main_pane, Some(main));
    assert_eq!(project.active_pane, main);
    assert_eq!(
        project.panes[&main].active_tab_key.as_deref(),
        Some("shell:second")
    );
    assert_eq!(
        project.effective_locked_panes(),
        HashSet::from([navigation])
    );
    let final_tabs: HashSet<_> = project
        .panes
        .values()
        .flat_map(|pane| pane.tabs.iter().map(SavedTab::key))
        .collect();
    assert!(original_tabs.is_subset(&final_tabs));
    if let Layout::Split { ratio, .. } = &mut project.layout {
        *ratio = 0.36;
    }
    let applied = project.clone();
    riwork_core::layouts::apply_default_layout(&mut project).unwrap();
    assert_eq!(project, applied);
    let mut model = LayoutModel::new(project).unwrap();
    model
        .open_panel(riwork_core::layouts::PanelKind::Usage)
        .unwrap();
    assert_eq!(
        model.project().panes[&navigation].active_tab_key.as_deref(),
        Some("panel:usage")
    );
}

#[test]
fn default_layout_allocates_navigation_below_the_core_pane_id_ceiling() {
    let mut project = fixture();
    let high = u64::MAX - 257;
    project.layout = Layout::Pane(high);
    project.panes = std::collections::BTreeMap::from([(high, SavedPane::default())]);
    project.active_pane = high;
    project.main_pane = Some(high);
    project.locked_panes = Some(HashSet::new());
    project.normalize().unwrap();
    riwork_core::layouts::apply_default_layout(&mut project).unwrap();
    let (navigation, main) = project.layout.two_panes_side_by_side().unwrap();
    assert_eq!(main, high);
    assert!(navigation < high);
    assert_eq!(project.panes[&navigation].tabs.len(), 5);
}

fn oracle(input: ProjectLayout) {
    let temp = scratch();
    let store = LayoutStore::open(temp.path()).unwrap();
    let expected = store.save_chat_state(PROJECT, &input, &[], None).unwrap();
    let original_json = read(temp.path());
    let loaded =
        LayoutPersistence::open(temp.path(), PROJECT, None, SaveMode::LegacyChatState).unwrap();
    assert!(loaded.notice.is_none());
    assert_eq!(loaded.project, expected);
    let mut model = LayoutModel::new(loaded.project).unwrap();
    for extent in [
        area(),
        Extent {
            width: 1920.0,
            height: 1080.0,
        },
        Extent {
            width: 640.0,
            height: 400.0,
        },
    ] {
        let dock = model.dock(extent);
        model.apply_dock(dock, extent).unwrap();
        assert_eq!(model.project(), &expected);
        assert_eq!(
            model.project().layout.pane_extents(extent),
            expected.layout.pane_extents(extent)
        );
    }
    loaded
        .persistence
        .schedule(model.storage_project())
        .unwrap();
    loaded.persistence.flush().unwrap();
    assert_eq!(store.load(PROJECT).unwrap(), Some(expected));
    assert_eq!(read(temp.path()), original_json);
}

#[test]
fn gpui_written_fixtures_open_identically_and_resave_without_structure_or_metadata_loss() {
    let rt = runtime();
    let _entered = rt.enter();
    oracle(fixture());
    let mut automatic = fixture();
    automatic.locked_panes = None;
    oracle(automatic);
    let mut explicitly_unlocked = fixture();
    explicitly_unlocked.locked_panes = Some(HashSet::new());
    oracle(explicitly_unlocked);
    let mut all_locked = fixture();
    all_locked.locked_panes = Some(HashSet::from([1, 2, 3]));
    oracle(all_locked);
    let mut narrow = fixture();
    if let Layout::Split { ratio, second, .. } = &mut narrow.layout {
        *ratio = 0.02;
        if let Layout::Split { ratio, .. } = &mut **second {
            *ratio = 0.98;
        }
    }
    oracle(narrow);
    let mut same_axis = fixture();
    same_axis
        .layout
        .split_with_ratio(2, Axis::Stacked, 4, false, 0.4);
    same_axis.panes.insert(4, SavedPane::default());
    oracle(same_axis);
    let mut every_tab_kind = fixture();
    every_tab_kind.panes.get_mut(&2).unwrap().tabs.extend([
        SavedTab::RemoteShell {
            desktop_id: "other-mac".into(),
            shell_id: "remote-shell".into(),
        },
        SavedTab::Chat {
            chat_id: "chat-existing".into(),
        },
        SavedTab::AppPreview {
            preview_id: "12345678-1234-4234-8234-123456789012".into(),
            spec: PreviewSpec {
                command: "never execute this fixture command".into(),
                cwd: "/tmp/fixture-preview".into(),
                mode: PreviewMode::Cooperative,
                ios: None,
            },
        },
    ]);
    oracle(every_tab_kind);
}

#[test]
fn empty_locked_panes_unknown_tabs_and_unknown_top_level_fields_survive_an_actual_save() {
    let rt = runtime();
    let _entered = rt.enter();
    let temp = scratch();
    let mut expected = seed(temp.path(), true);
    let loaded =
        LayoutPersistence::open(temp.path(), PROJECT, None, SaveMode::LegacyChatState).unwrap();
    assert_eq!(loaded.unknown_panes, [3].into_iter().collect());
    let mut model = LayoutModel::new(loaded.project).unwrap();
    model.mark_unknown_panes(&loaded.unknown_panes);
    let dock = model.dock(area());
    assert!(serde_json::to_string(&dock).unwrap().contains("empty:3"));
    model.apply_dock(dock, area()).unwrap();
    assert_eq!(model.project(), &expected);
    let mut edited = model.storage_project();
    edited.selected_task_id = Some("task-after-nsh-edit".into());
    expected.selected_task_id = edited.selected_task_id.clone();
    loaded.persistence.schedule(edited).unwrap();
    loaded.persistence.flush().unwrap();
    let stored = read(temp.path());
    assert_eq!(
        stored["projects"][PROJECT]["panes"]["3"]["tabs"],
        json!([unknown_tab()])
    );
    assert_eq!(
        stored["futureTopLevel"],
        json!({"preserve": [1, "future", true]})
    );
    assert_eq!(
        LayoutStore::open(temp.path())
            .unwrap()
            .load(PROJECT)
            .unwrap(),
        Some(expected)
    );
}

#[test]
fn dragging_the_last_known_tab_out_keeps_its_unknown_tab_and_empty_sentinel() {
    let rt = runtime();
    let _entered = rt.enter();
    let temp = scratch();
    seed(temp.path(), true);
    let mut raw = read(temp.path());
    raw["projects"][PROJECT]["panes"]["3"]["tabs"]
        .as_array_mut()
        .unwrap()
        .insert(0, json!({"kind": "shell", "shell_id": "movable"}));
    write(temp.path(), &raw);
    let loaded =
        LayoutPersistence::open(temp.path(), PROJECT, None, SaveMode::LegacyChatState).unwrap();
    let mut model = LayoutModel::new(loaded.project).unwrap();
    model.mark_unknown_panes(&loaded.unknown_panes);
    let mut next = model.dock(area());
    fn move_panel(node: &mut DockNode) {
        match node {
            DockNode::Split { children, .. } => children.iter_mut().for_each(move_panel),
            DockNode::Tabs {
                id,
                panels,
                active_index,
            } if *id == Some(3) => {
                panels.retain(|id| id != "shell:movable");
                *active_index = 0;
            }
            DockNode::Tabs {
                id,
                panels,
                active_index,
            } if *id == Some(2) => {
                panels.push("shell:movable".into());
                *active_index = panels.len() - 1;
            }
            _ => {}
        }
    }
    move_panel(&mut next.center);
    let projection = model.apply_dock(next, area()).unwrap();
    assert!(model.project().panes[&3].tabs.is_empty());
    assert!(
        serde_json::to_string(&projection)
            .unwrap()
            .contains("empty:3")
    );
    loaded
        .persistence
        .schedule(model.storage_project())
        .unwrap();
    loaded.persistence.flush().unwrap();
    assert_eq!(
        read(temp.path())["projects"][PROJECT]["panes"]["3"]["tabs"],
        json!([unknown_tab()])
    );
    assert_eq!(
        LayoutStore::open(temp.path())
            .unwrap()
            .load(PROJECT)
            .unwrap()
            .unwrap()
            .panes[&2]
            .active_tab_key
            .as_deref(),
        Some("shell:movable")
    );
}

#[test]
fn newer_schema_and_unreadable_entries_show_notice_and_are_never_overwritten() {
    let rt = runtime();
    let _entered = rt.enter();
    for input in [
        json!({"schema_version": 2, "projects": ["future format"]}),
        json!({"schema_version": 1, "projects": {PROJECT: {"layout": "unreadable future tree"}}}),
    ] {
        let temp = scratch();
        write(temp.path(), &input);
        let bytes = fs::read(temp.path().join("layouts.json")).unwrap();
        let loaded =
            LayoutPersistence::open(temp.path(), PROJECT, None, SaveMode::LegacyChatState).unwrap();
        assert!(loaded.notice.is_some());
        assert_eq!(loaded.project, default_project_layout());
        loaded.persistence.schedule(loaded.project).unwrap();
        assert!(loaded.persistence.flush().is_err());
        drop(loaded.persistence);
        assert_eq!(fs::read(temp.path().join("layouts.json")).unwrap(), bytes);
    }
}

#[test]
fn restore_snapshot_wins_and_missing_projects_do_not_write_before_an_edit() {
    let rt = runtime();
    let _entered = rt.enter();
    let temp = scratch();
    seed(temp.path(), false);
    let before = read(temp.path());
    let mut restore = fixture();
    restore.selected_task_id = Some("reload-supplied-task".into());
    let loaded = LayoutPersistence::open(
        temp.path(),
        PROJECT,
        Some(restore.clone()),
        SaveMode::LegacyChatState,
    )
    .unwrap();
    assert_eq!(loaded.project, restore);
    loaded.persistence.flush().unwrap();
    assert_eq!(read(temp.path()), before);
    let missing = LayoutPersistence::open(
        temp.path(),
        "missing-project",
        None,
        SaveMode::LegacyChatState,
    )
    .unwrap();
    assert!(missing.notice.is_none());
    assert_eq!(missing.project, default_project_layout());
    drop(missing);
    assert_eq!(read(temp.path()), before);
    // A reload snapshot also outranks a corrupt file. Merely opening it never
    // performs the corrupt-file recovery write.
    let corrupt = b"invalid saved JSON";
    fs::write(temp.path().join("layouts.json"), corrupt).unwrap();
    let restored = LayoutPersistence::open(
        temp.path(),
        PROJECT,
        Some(restore.clone()),
        SaveMode::LegacyChatState,
    )
    .unwrap();
    assert_eq!(restored.project, restore);
    assert!(restored.notice.is_none());
    drop(restored);
    assert_eq!(fs::read(temp.path().join("layouts.json")).unwrap(), corrupt);
    fs::write(temp.path().join("layouts.json"), b"  \n\t").unwrap();
    let whitespace =
        LayoutPersistence::open(temp.path(), PROJECT, None, SaveMode::LegacyChatState).unwrap();
    assert_eq!(whitespace.project, default_project_layout());
    assert!(whitespace.notice.is_none());
}

#[test]
fn corrupt_layout_is_untouched_until_an_edit_then_core_sets_it_aside() {
    let rt = runtime();
    let _entered = rt.enter();
    let temp = scratch();
    let corrupt = b"{\"schema_version\": 1, \"projects\": {";
    fs::write(temp.path().join("layouts.json"), corrupt).unwrap();
    let loaded =
        LayoutPersistence::open(temp.path(), PROJECT, None, SaveMode::LegacyChatState).unwrap();
    assert!(loaded.notice.is_some());
    assert_eq!(fs::read(temp.path().join("layouts.json")).unwrap(), corrupt);
    loaded.persistence.schedule(loaded.project).unwrap();
    loaded.persistence.flush().unwrap();
    let backups: Vec<_> = fs::read_dir(temp.path())
        .unwrap()
        .map(Result::unwrap)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("layouts.corrupt-")
        })
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(fs::read(backups[0].path()).unwrap(), corrupt);
}

#[test]
fn trailing_save_coalesces_edits_and_flush_and_drop_do_not_wait_for_the_deadline() {
    let rt = runtime();
    let _entered = rt.enter();
    let temp = scratch();
    seed(temp.path(), false);
    let store = LayoutStore::open(temp.path()).unwrap();
    let loaded =
        LayoutPersistence::open(temp.path(), PROJECT, None, SaveMode::LegacyChatState).unwrap();
    let mut first = loaded.project.clone();
    first.selected_task_id = Some("first-edit".into());
    loaded.persistence.schedule(first).unwrap();
    std::thread::sleep(Duration::from_millis(80));
    let mut latest = loaded.project;
    latest.selected_task_id = Some("last-edit".into());
    loaded.persistence.schedule(latest.clone()).unwrap();
    std::thread::sleep(Duration::from_millis(80));
    assert_eq!(
        store
            .load(PROJECT)
            .unwrap()
            .unwrap()
            .selected_task_id
            .as_deref(),
        Some("selected-task")
    );
    let start = Instant::now();
    while store.load(PROJECT).unwrap().as_ref() != Some(&latest) {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "trailing save did not finish"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(SAVE_DELAY, Duration::from_millis(250));
    latest.selected_task_id = Some("flush-edit".into());
    loaded.persistence.schedule(latest.clone()).unwrap();
    loaded.persistence.flush().unwrap();
    assert_eq!(store.load(PROJECT).unwrap(), Some(latest.clone()));
    latest.selected_task_id = Some("drop-edit".into());
    loaded.persistence.schedule(latest.clone()).unwrap();
    drop(loaded.persistence);
    assert_eq!(store.load(PROJECT).unwrap(), Some(latest));
}

#[test]
fn switching_flushes_the_outgoing_project_and_uses_core_locked_region_carry() {
    let rt = runtime();
    let _entered = rt.enter();
    let temp = scratch();
    seed(temp.path(), false);
    let store = LayoutStore::open(temp.path()).unwrap();
    let mut destination = fixture();
    destination.selected_worktree_id = Some("destination-worktree".into());
    destination.panes.get_mut(&2).unwrap().tabs = vec![support::shell("destination-shell")];
    destination.normalize().unwrap();
    store
        .save_chat_state("destination", &destination, &[], None)
        .unwrap();
    let mut loaded =
        LayoutPersistence::open(temp.path(), PROJECT, None, SaveMode::LegacyChatState).unwrap();
    let mut previous = loaded.project;
    previous.selected_task_id = Some("outgoing-final-task".into());
    let expected = destination.carry_locked_regions_from(&previous).unwrap();
    let next = loaded
        .persistence
        .switch_project("destination", &previous, None)
        .unwrap();
    assert_eq!(store.load(PROJECT).unwrap(), Some(previous));
    assert_eq!(next.project, expected);
    assert_eq!(
        next.project.selected_worktree_id.as_deref(),
        Some("destination-worktree")
    );
    assert_eq!(loaded.persistence.project_id(), "destination");
}

#[test]
fn legacy_saves_merge_chat_dismissals_while_shared_membership_saves_its_projection() {
    let rt = runtime();
    let _entered = rt.enter();
    let temp = scratch();
    seed(temp.path(), false);
    let store = LayoutStore::open(temp.path()).unwrap();
    let legacy =
        LayoutPersistence::open(temp.path(), PROJECT, None, SaveMode::LegacyChatState).unwrap();
    let mut peer = legacy.project.clone();
    peer.detached_chat_ids.insert("peer-dismissal".into());
    store.save_chat_state(PROJECT, &peer, &[], None).unwrap();
    let mut stale = legacy.project;
    stale.selected_task_id = Some("stale-window-layout-edit".into());
    legacy.persistence.schedule(stale).unwrap();
    legacy.persistence.flush().unwrap();
    assert!(
        store
            .load(PROJECT)
            .unwrap()
            .unwrap()
            .detached_chat_ids
            .contains("peer-dismissal")
    );
    let shared = LayoutPersistence::open(temp.path(), PROJECT, None, SaveMode::SharedTabs).unwrap();
    let mut projected = shared.project;
    projected.detached_chat_ids.clear();
    shared.persistence.schedule(projected).unwrap();
    shared.persistence.flush().unwrap();
    assert!(
        store
            .load(PROJECT)
            .unwrap()
            .unwrap()
            .detached_chat_ids
            .is_empty()
    );
}

#[test]
fn storage_preserves_unknown_tabs_when_projection_remaps_a_js_unsafe_pane_id() {
    let rt = runtime();
    let _entered = rt.enter();
    let temp = scratch();
    let unsafe_id = (1_u64 << 53) + 10;
    let mut input = fixture();
    input.layout = Layout::navigation_beside(1, Layout::Pane(unsafe_id));
    let empty = input.panes.remove(&3).unwrap();
    input.panes.insert(unsafe_id, empty);
    input.active_pane = unsafe_id;
    input.main_pane = Some(unsafe_id);
    input.locked_panes = Some(HashSet::from([1, unsafe_id]));
    input.normalize().unwrap();
    LayoutStore::open(temp.path())
        .unwrap()
        .save_chat_state(PROJECT, &input, &[], None)
        .unwrap();
    let mut disk = read(temp.path());
    disk["projects"][PROJECT]["panes"][unsafe_id.to_string()]["tabs"] = json!([unknown_tab()]);
    write(temp.path(), &disk);
    let loaded =
        LayoutPersistence::open(temp.path(), PROJECT, None, SaveMode::LegacyChatState).unwrap();
    let mut model = LayoutModel::new(loaded.project).unwrap();
    model.mark_unknown_panes(&loaded.unknown_panes);
    assert!(
        model
            .project()
            .layout
            .pane_ids()
            .iter()
            .all(|id| *id <= (1_u64 << 53) - 1)
    );
    let dock = model.dock(area());
    model.apply_dock(dock, area()).unwrap();
    let mut stored = model.storage_project();
    stored.selected_task_id = Some("force-real-write".into());
    loaded.persistence.schedule(stored).unwrap();
    loaded.persistence.flush().unwrap();
    // Known limitation: core's private skipped-tab map has no remap API, so
    // storage keeps the original ID rather than losing future tab payloads.
    assert_eq!(
        read(temp.path())["projects"][PROJECT]["panes"][unsafe_id.to_string()]["tabs"],
        json!([unknown_tab()])
    );
}
