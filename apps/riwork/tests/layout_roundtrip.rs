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
    let safe_id = model.pane_id_remap()[&unsafe_id];
    assert!(
        read(temp.path())["projects"][PROJECT]["panes"]
            .get(unsafe_id.to_string())
            .is_none()
    );
    assert_eq!(
        read(temp.path())["projects"][PROJECT]["panes"][safe_id.to_string()]["tabs"],
        json!([unknown_tab()])
    );
}

#[path = "support/fixtures.rs"]
mod named;

fn named_oracle(name: &str) {
    let rt = runtime();
    let _entered = rt.enter();
    let temp = scratch();
    let input = named::seed(temp.path(), PROJECT, name);
    let loaded =
        LayoutPersistence::open(temp.path(), PROJECT, None, SaveMode::LegacyChatState).unwrap();
    // The oracle itself is the pure layout mapping. Session opening separately
    // reconstructs the sidebar for pre-panels legacy saves.
    let mut model = LayoutModel::new(input).unwrap();
    model.mark_unknown_panes(&loaded.unknown_panes);
    let expected = model.project().clone();
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
        let projection = model.dock(extent);
        let returned = model.apply_dock(projection.clone(), extent).unwrap();
        assert_eq!(returned, projection, "{name}");
        assert_eq!(model.project(), &expected, "{name}");
        assert_eq!(
            model.project().layout.pane_extents(extent),
            expected.layout.pane_extents(extent)
        );
    }
}

fn reopen_named(name: &str) {
    let rt = runtime();
    let _entered = rt.enter();
    let temp = scratch();
    named::seed(temp.path(), PROJECT, name);
    let path = temp.path().join("layouts.json");
    let before = fs::read(&path).unwrap();
    let mtime = fs::metadata(&path).unwrap().modified().unwrap();
    {
        let loaded =
            LayoutPersistence::open(temp.path(), PROJECT, None, SaveMode::LegacyChatState).unwrap();
        let mut model = LayoutModel::new(loaded.project).unwrap();
        model.mark_unknown_panes(&loaded.unknown_panes);
        let projection = model.dock(area());
        fn assert_groups(node: &DockNode, expected: &ProjectLayout) {
            match node {
                DockNode::Split { children, .. } => {
                    for child in children {
                        assert_groups(child, expected);
                    }
                }
                DockNode::Tabs {
                    id,
                    panels,
                    active_index,
                } => {
                    let id = id.expect("projected group has its stable pane id");
                    let pane = &expected.panes[&id];
                    let keys: Vec<_> = pane.tabs.iter().map(SavedTab::key).collect();
                    if keys.is_empty() {
                        assert_eq!(panels, &vec![format!("empty:{id}")]);
                    } else {
                        let visible: Vec<_> = panels
                            .iter()
                            .filter(|key| !key.starts_with("empty:"))
                            .cloned()
                            .collect();
                        assert_eq!(visible, keys);
                        assert_eq!(panels.get(*active_index), pane.active_tab_key.as_ref());
                    }
                }
            }
        }
        assert_groups(&projection.center, model.project());
        loaded.persistence.flush().unwrap();
    }
    assert_eq!(
        fs::read(&path).unwrap(),
        before,
        "{name} changed bytes without an edit"
    );
    assert_eq!(
        fs::metadata(&path).unwrap().modified().unwrap(),
        mtime,
        "{name} changed mtime without an edit"
    );
}

fn edit_named(name: &str) {
    let rt = runtime();
    let _entered = rt.enter();
    let temp = scratch();
    let core = scratch();
    named::seed(temp.path(), PROJECT, name);
    let other = named::seed(temp.path(), "other-project", "L-NESTED");
    fs::copy(
        temp.path().join("layouts.json"),
        core.path().join("layouts.json"),
    )
    .unwrap();
    let loaded =
        LayoutPersistence::open(temp.path(), PROJECT, None, SaveMode::LegacyChatState).unwrap();
    let mut model = LayoutModel::new(loaded.project).unwrap();
    model.mark_unknown_panes(&loaded.unknown_panes);
    model.dock(area());
    let target = model
        .project()
        .layout
        .pane_ids()
        .into_iter()
        .find(|id| *id != model.project().active_pane)
        .unwrap();
    model.select_pane(target).unwrap();
    let changed = model.storage_project();
    let map = model
        .pane_id_remap()
        .iter()
        .filter(|(old, _)| **old > (1_u64 << 53) - 1)
        .map(|(&old, &new)| (old, new))
        .collect();
    let expected = LayoutStore::open(core.path())
        .unwrap()
        .save_chat_state_remapped(PROJECT, &changed, &map)
        .unwrap();
    loaded.persistence.schedule(changed).unwrap();
    loaded.persistence.flush().unwrap();
    assert_eq!(
        read(temp.path()),
        read(core.path()),
        "{name}: NSH save differs from GPUI writer"
    );
    let store = LayoutStore::open(temp.path()).unwrap();
    assert_eq!(store.load(PROJECT).unwrap(), Some(expected));
    assert_eq!(store.load("other-project").unwrap(), Some(other));
}

macro_rules! named_cases {
    ($($oracle:ident, $reopen:ident, $edit:ident => $name:literal;)*) => {$ (
        #[test] fn $oracle() { named_oracle($name); }
        #[test] fn $reopen() { reopen_named($name); }
        #[test] fn $edit() { edit_named($name); }
    )*};
}
named_cases! {
    oracle_l_nav, reopen_l_nav_renders_golden_and_writes_nothing, edit_l_nav_saves_what_riwork_core_saves => "L-NAV";
    oracle_l_nested, reopen_l_nested_renders_golden_and_writes_nothing, edit_l_nested_saves_what_riwork_core_saves => "L-NESTED";
    oracle_l_empty, reopen_l_empty_renders_golden_and_writes_nothing, edit_l_empty_saves_what_riwork_core_saves => "L-EMPTY";
    oracle_l_unknown, reopen_l_unknown_renders_golden_and_writes_nothing, edit_l_unknown_saves_what_riwork_core_saves => "L-UNKNOWN";
    oracle_l_preview_remote, reopen_l_preview_remote_renders_golden_and_writes_nothing, edit_l_preview_remote_saves_what_riwork_core_saves => "L-PREVIEW-REMOTE";
    oracle_l_legacy, reopen_l_legacy_renders_golden_and_writes_nothing, edit_l_legacy_saves_what_riwork_core_saves => "L-LEGACY";
    oracle_l_global_tabs, reopen_l_global_tabs_renders_golden_and_writes_nothing, edit_l_global_tabs_saves_what_riwork_core_saves => "L-GLOBAL-TABS";
    oracle_l_unlock, reopen_l_unlock_renders_golden_and_writes_nothing, edit_l_unlock_saves_what_riwork_core_saves => "L-UNLOCK";
    oracle_l_bigid, reopen_l_bigid_renders_golden_and_writes_nothing, edit_l_bigid_saves_what_riwork_core_saves => "L-BIGID";
    oracle_l_ratio, reopen_l_ratio_renders_golden_and_writes_nothing, edit_l_ratio_saves_what_riwork_core_saves => "L-RATIO";
    oracle_l_carry, reopen_l_carry_renders_golden_and_writes_nothing, edit_l_carry_saves_what_riwork_core_saves => "L-CARRY";
    oracle_l_carry_dest, reopen_l_carry_dest_renders_golden_and_writes_nothing, edit_l_carry_dest_saves_what_riwork_core_saves => "L-CARRY-DEST";
    oracle_l_main, reopen_l_main_renders_golden_and_writes_nothing, edit_l_main_saves_what_riwork_core_saves => "L-MAIN";
}
macro_rules! core_oracles {
    ($($test:ident => $name:literal;)*) => {$ (#[test] fn $test() {named_oracle($name);})*};
}
core_oracles! {
    oracle_nested_layout_round_trip_preserves_tabs_selections_and_detachment => "L-NESTED";
    oracle_split_ratios_round_trip_and_survive_insertions_and_collapses => "L-NESTED";
    oracle_ratios_a_resize_leaves_on_narrow_locked_panes_survive_a_restart => "L-RATIO";
    oracle_left_navigation_locks_by_default_but_explicit_unlock_survives_round_trip => "L-UNLOCK";
    oracle_surplus_unlocked_slots_remain_empty_and_all_locked_keeps_sessions_recoverable => "L-EMPTY";
    oracle_tabs_of_an_unknown_kind_are_skipped_and_the_rest_of_the_layout_still_loads => "L-UNKNOWN";
    oracle_app_preview_launch_settings_round_trip_and_deduplicate_by_identity => "L-PREVIEW-REMOTE";
    oracle_remote_shell_tabs_round_trip_and_are_not_this_macs_shells => "L-PREVIEW-REMOTE";
    oracle_the_main_pane_is_kept_per_project_and_comes_back_with_its_layout => "L-MAIN";
}

#[test]
fn opaque_entries_and_unknown_fields_keep_raw_whitespace_and_escapes_after_a_gesture() {
    let rt = runtime();
    let _entered = rt.enter();
    let temp = scratch();
    let known = serde_json::to_string(&named::layout("L-NAV")).unwrap();
    let data = format!(
        "{{\"schema_version\":1,\"future\":{},\"projects\":{{\"{PROJECT}\":{known},\"unreadable\":{}}}}}",
        named::FUTURE,
        named::UNREADABLE
    );
    fs::write(temp.path().join("layouts.json"), data).unwrap();
    let loaded =
        LayoutPersistence::open(temp.path(), PROJECT, None, SaveMode::LegacyChatState).unwrap();
    let mut changed = loaded.project;
    changed.selected_task_id = Some("gesture".into());
    loaded.persistence.schedule(changed).unwrap();
    loaded.persistence.flush().unwrap();
    let after = fs::read_to_string(temp.path().join("layouts.json")).unwrap();
    assert!(after.contains(named::FUTURE));
    assert!(after.contains(named::UNREADABLE));
}

fn damaged_open(name: &str) {
    let rt = runtime();
    let _entered = rt.enter();
    let temp = scratch();
    named::damaged(temp.path(), PROJECT, name);
    let before = fs::read(temp.path().join("layouts.json")).unwrap();
    let loaded =
        LayoutPersistence::open(temp.path(), PROJECT, None, SaveMode::LegacyChatState).unwrap();
    assert_eq!(loaded.project, default_project_layout());
    assert!(
        loaded
            .notice
            .as_deref()
            .unwrap()
            .ends_with("; using a default layout")
    );
    drop(loaded);
    assert_eq!(fs::read(temp.path().join("layouts.json")).unwrap(), before);
}
#[test]
fn open_session_on_d_newer() {
    damaged_open("D-NEWER");
}
#[test]
fn open_session_on_d_corrupt() {
    damaged_open("D-CORRUPT");
}
#[test]
fn open_session_on_d_entry() {
    damaged_open("D-ENTRY");
}
#[test]
fn oracle_newer_schema_is_neither_loaded_nor_overwritten() {
    newer_schema_and_unreadable_entries_show_notice_and_are_never_overwritten();
}

#[test]
fn an_older_layout_s_global_tabs_are_the_global_window_s() {
    use riwork_nsh_workspace::persistence::prune_global_tabs;
    let mut p = named::layout("L-GLOBAL-TABS");
    p.panes.get_mut(&2).unwrap().tabs.extend([
        named::shell("ended-orchestrator"),
        SavedTab::RemoteShell {
            desktop_id: "desk".into(),
            shell_id: "global-orchestrator".into(),
        },
        named::layout("L-PREVIEW-REMOTE").panes[&2].tabs[1].clone(),
    ]);
    assert!(prune_global_tabs(
        &mut p,
        &HashSet::from(["global-chat".into()]),
        &HashSet::from(["global-orchestrator".into()])
    ));
    let keys: Vec<_> = p.panes[&2].tabs.iter().map(SavedTab::key).collect();
    assert_eq!(
        keys,
        vec![
            "panel:usage",
            "shell:ended-orchestrator",
            "remote:desk:global-orchestrator",
            "app_preview:12345678-1234-4234-8234-123456789012"
        ]
    );
    assert!(!prune_global_tabs(
        &mut p,
        &HashSet::from(["global-chat".into()]),
        &HashSet::from(["global-orchestrator".into()])
    ));
}

#[test]
fn an_older_layout_loads_without_its_global_tabs_and_saves_without_them() {
    use riwork_nsh_workspace::persistence::{MOVED_NOTICE, prune_global_tabs};
    let rt = runtime();
    let _entered = rt.enter();
    let temp = scratch();
    named::seed(temp.path(), PROJECT, "L-GLOBAL-TABS");
    let loaded =
        LayoutPersistence::open(temp.path(), PROJECT, None, SaveMode::LegacyChatState).unwrap();
    let mut changed = loaded.project;
    assert!(prune_global_tabs(
        &mut changed,
        &HashSet::from(["global-chat".into()]),
        &HashSet::from(["global-orchestrator".into()])
    ));
    assert_eq!(
        MOVED_NOTICE,
        "The dashboard and the global chat now have their own window: View › Global Dashboard (⌘⌥G) or Global Chat (⌘⇧O)."
    );
    loaded.persistence.schedule(changed.clone()).unwrap();
    loaded.persistence.flush().unwrap();
    assert_eq!(
        LayoutStore::open(temp.path())
            .unwrap()
            .load(PROJECT)
            .unwrap(),
        Some(changed)
    );
}

#[cfg(debug_assertions)]
#[test]
fn each_trigger_saves_once_after_250_ms() {
    use riwork_core::layouts::{PanelKind, WindowSize};
    let cases: &[(&str, fn(&mut ProjectLayout))] = &[
        ("tab selection", |p| {
            p.panes.get_mut(&2).unwrap().active_tab_key = Some("shell:first".into())
        }),
        ("pane selection", |p| p.active_pane = 1),
        ("tab move", |p| {
            let tab = p.panes.get_mut(&2).unwrap().tabs.remove(0);
            p.panes.get_mut(&3).unwrap().tabs.push(tab);
        }),
        ("tab close", |p| {
            p.panes.get_mut(&2).unwrap().tabs.pop();
        }),
        ("pane split", |p| {
            p.layout.split_with_ratio(2, Axis::Stacked, 4, false, 0.5);
            p.panes.insert(4, SavedPane::default());
        }),
        ("pane close", |p| {
            p.layout = p.layout.clone().without(3).unwrap();
            p.panes.remove(&3);
        }),
        ("lock toggle", |p| p.locked_panes = Some(HashSet::new())),
        ("main toggle", |p| p.main_pane = Some(3)),
        ("divider mouse up", |p| {
            if let Layout::Split { ratio, .. } = &mut p.layout {
                *ratio = 0.4;
            }
        }),
        ("gather", |p| {
            p.locked_panes = Some(HashSet::from([1]));
            p.panes
                .get_mut(&3)
                .unwrap()
                .tabs
                .push(named::shell("gathered"));
            riwork_core::layouts::workspace::gather(p, &Default::default()).unwrap();
        }),
        ("default layout", |p| {
            riwork_core::layouts::workspace::default_layout(p, &Default::default()).unwrap();
        }),
        ("worktree selection", |p| {
            p.selected_worktree_id = Some("other-worktree".into())
        }),
        ("task selection", |p| {
            p.selected_task_id = Some("other-task".into())
        }),
        ("panel open", |p| {
            p.panes.get_mut(&2).unwrap().tabs.push(SavedTab::Panel {
                panel: PanelKind::Usage,
            })
        }),
        ("window size debounce", |p| {
            p.window_size = WindowSize::new(1000.0, 700.0)
        }),
        ("pane area debounce", |p| {
            p.layout.preserve_locked(
                area(),
                Extent {
                    width: 1500.0,
                    height: 900.0,
                },
                &|id| id == 1,
                100.0,
            );
        }),
        ("successful restored load", |p| p.active_pane = 3),
    ];
    let rt = runtime();
    let _entered = rt.enter();
    for (trigger, change) in cases {
        let temp = scratch();
        let original = seed(temp.path(), false);
        let path = temp.path().join("layouts.json");
        let bytes = fs::read(&path).unwrap();
        let stamp = fs::metadata(&path).unwrap().modified().unwrap();
        let loaded = LayoutPersistence::open_with_manual_clock(
            temp.path(),
            PROJECT,
            None,
            SaveMode::LegacyChatState,
        )
        .unwrap();
        let mut edited = original.clone();
        change(&mut edited);
        edited.normalize().unwrap();
        assert_ne!(
            edited, original,
            "{trigger} must cause a real layout mutation"
        );
        loaded.persistence.schedule(edited.clone()).unwrap();
        loaded
            .persistence
            .advance_manual_clock(Duration::from_millis(200))
            .unwrap();
        loaded.persistence.schedule(edited.clone()).unwrap();
        loaded
            .persistence
            .advance_manual_clock(Duration::from_millis(249))
            .unwrap();
        assert_eq!(
            fs::read(&path).unwrap(),
            bytes,
            "{trigger}: saved before trailing deadline"
        );
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), stamp);
        loaded
            .persistence
            .advance_manual_clock(Duration::from_millis(1))
            .unwrap();
        assert_eq!(
            LayoutStore::open(temp.path())
                .unwrap()
                .load(PROJECT)
                .unwrap(),
            Some(edited),
            "{trigger}"
        );
        let saved = fs::read(&path).unwrap();
        let stamp = fs::metadata(&path).unwrap().modified().unwrap();
        loaded
            .persistence
            .advance_manual_clock(Duration::from_secs(100))
            .unwrap();
        loaded.persistence.flush().unwrap();
        assert_eq!(fs::read(&path).unwrap(), saved);
        assert_eq!(
            fs::metadata(&path).unwrap().modified().unwrap(),
            stamp,
            "{trigger}: idle/flush saved again"
        );
    }
}

#[cfg(debug_assertions)]
#[test]
fn flush_points_write_synchronously() {
    let rt = runtime();
    let _entered = rt.enter();
    for point in [
        "window.close",
        "app.quit",
        "app.restart",
        "project switch",
        "session drop",
    ] {
        let temp = scratch();
        seed(temp.path(), false);
        let mut loaded = LayoutPersistence::open_with_manual_clock(
            temp.path(),
            PROJECT,
            None,
            SaveMode::LegacyChatState,
        )
        .unwrap();
        let mut edited = loaded.project.clone();
        edited.selected_task_id = Some(point.into());
        loaded.persistence.schedule(edited.clone()).unwrap();
        match point {
            "session drop" => drop(loaded.persistence),
            "project switch" => {
                loaded
                    .persistence
                    .switch_project("destination", &edited, None)
                    .unwrap();
            }
            _ => {
                loaded.persistence.flush().unwrap();
            }
        }
        assert_eq!(
            LayoutStore::open(temp.path())
                .unwrap()
                .load(PROJECT)
                .unwrap(),
            Some(edited),
            "{point}"
        );
    }
}

#[cfg(debug_assertions)]
#[test]
fn nothing_is_saved_without_an_edit() {
    let rt = runtime();
    let _entered = rt.enter();
    let temp = scratch();
    seed(temp.path(), false);
    let path = temp.path().join("layouts.json");
    let before = fs::read(&path).unwrap();
    let stamp = fs::metadata(&path).unwrap().modified().unwrap();
    let loaded =
        LayoutPersistence::open_with_manual_clock(temp.path(), PROJECT, None, SaveMode::SharedTabs)
            .unwrap();
    loaded
        .persistence
        .advance_manual_clock(Duration::from_secs(86400))
        .unwrap();
    loaded.persistence.flush().unwrap();
    drop(loaded);
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), stamp);
}

#[test]
fn legacy_sidebar_reconstruction_keeps_shell_positions_selection_and_hidden_choice() {
    use riwork_nsh_workspace::persistence::initialize_navigation;
    let mut legacy = named::layout("L-LEGACY");
    legacy.normalize().unwrap();
    let original = legacy.clone();
    assert!(initialize_navigation(&mut legacy).unwrap());
    assert_eq!(legacy.active_pane, original.active_pane);
    for (id, pane) in &original.panes {
        assert_eq!(&legacy.panes[id], pane);
    }
    assert!(legacy.panels_initialized);
    assert!(!initialize_navigation(&mut legacy).unwrap());
    let mut hidden = original.clone();
    hidden.sidebar_visible = false;
    let before = hidden.clone();
    assert!(!initialize_navigation(&mut hidden).unwrap());
    assert_eq!(hidden, before);
}

fn preserved_resize_oracle(case: u8) {
    let rt = runtime();
    let _entered = rt.enter();
    fn split(axis: Axis, ratio: f32, a: Layout, b: Layout) -> Layout {
        Layout::Split {
            axis,
            ratio,
            first: Box::new(a),
            second: Box::new(b),
        }
    }
    let start = Extent {
        width: 1200.0,
        height: 800.0,
    };
    let right = match case {
        1 | 2 => split(Axis::SideBySide, 0.5, Layout::Pane(2), Layout::Pane(3)),
        3 | 4 => split(
            Axis::Stacked,
            560.0 / 795.0,
            Layout::Pane(2),
            Layout::Pane(3),
        ),
        _ => Layout::Pane(2),
    };
    let mut project = default_project_layout();
    project.layout = split(Axis::SideBySide, 260.0 / 1195.0, Layout::Pane(1), right);
    for id in project.layout.pane_ids() {
        project.panes.entry(id).or_default();
    }
    project.locked_panes = Some(if case == 3 || case == 4 {
        HashSet::from([1, 3])
    } else {
        HashSet::from([1])
    });
    project.normalize().unwrap();
    let mut current = start;
    let route = match case {
        1 => vec![
            Extent {
                width: 400.0,
                height: 800.0,
            },
            Extent {
                width: 1000.0,
                height: 800.0,
            },
        ],
        2 => vec![Extent {
            width: 250.0,
            height: 800.0,
        }],
        5 => vec![
            start,
            Extent {
                width: 1200.0,
                height: 900.0,
            },
            Extent {
                width: 1300.0,
                height: 800.0,
            },
        ],
        6 => vec![
            Extent {
                width: 0.0,
                height: 0.0,
            },
            Extent {
                width: f32::NAN,
                height: 800.0,
            },
            Extent {
                width: -50.0,
                height: 800.0,
            },
        ],
        7 => vec![Extent {
            width: 5000.0,
            height: 800.0,
        }],
        _ => vec![
            Extent {
                width: 1600.0,
                height: 1000.0,
            },
            Extent {
                width: 800.0,
                height: 600.0,
            },
            start,
        ],
    };
    for _ in 0..if case == 4 { 10 } else { 1 } {
        for next in &route {
            let locks = project.effective_locked_panes();
            project
                .layout
                .preserve_locked(current, *next, &|id| locks.contains(&id), 100.0);
            oracle(project.clone());
            if case != 6 {
                current = *next;
            }
        }
    }
}
#[test]
fn oracle_a_locked_left_nav_keeps_its_width_and_its_height_follows_the_window() {
    preserved_resize_oracle(0);
}
#[test]
fn oracle_a_small_window_takes_from_the_locked_pane_only_as_far_as_it_must() {
    preserved_resize_oracle(1);
}
#[test]
fn oracle_a_window_too_small_for_every_minimum_stays_proportional() {
    preserved_resize_oracle(2);
}
#[test]
fn oracle_a_locked_nav_beside_a_locked_footer_keeps_its_width_and_the_footer_follows() {
    preserved_resize_oracle(3);
}
#[test]
fn oracle_resizing_back_and_forth_returns_to_the_same_ratios() {
    preserved_resize_oracle(4);
}
#[test]
fn oracle_only_real_changes_are_reported() {
    preserved_resize_oracle(5);
}
#[test]
fn oracle_unusable_sizes_leave_the_layout_alone() {
    preserved_resize_oracle(6);
}
#[test]
fn oracle_a_narrow_locked_pane_in_a_wide_window_may_go_below_the_drag_limit() {
    preserved_resize_oracle(7);
}

#[test]
fn default_layout_relocates_opaque_tabs_and_keeps_their_raw_payload() {
    let rt = runtime();
    let _entered = rt.enter();
    let temp = scratch();
    seed(temp.path(), false);
    let raw = r#"{ "kind": "future_browser", "url": "\u0061", "payload": [1,  2] }"#;
    let unknown: serde_json::Value = serde_json::from_str(raw).unwrap();
    let mut value = read(temp.path());
    value["projects"][PROJECT]["panes"]["3"]["tabs"] = json!([unknown]);
    let text = serde_json::to_string(&value)
        .unwrap()
        .replace(&serde_json::to_string(&unknown).unwrap(), raw);
    fs::write(temp.path().join("layouts.json"), text).unwrap();
    let loaded =
        LayoutPersistence::open(temp.path(), PROJECT, None, SaveMode::LegacyChatState).unwrap();
    let mut model = LayoutModel::new(loaded.project).unwrap();
    model.mark_unknown_panes(&loaded.unknown_panes);
    model.dock(area());
    model.default_layout().unwrap();
    let main = model.project().main_pane.unwrap();
    assert!(!model.project().panes.contains_key(&3));
    loaded
        .persistence
        .schedule_with_remap(model.storage_project(), model.storage_id_remap())
        .unwrap();
    loaded.persistence.flush().unwrap();
    let saved = read(temp.path());
    assert!(
        saved["projects"][PROJECT]["panes"][main.to_string()]["tabs"]
            .as_array()
            .unwrap()
            .contains(&unknown)
    );
    assert!(
        fs::read_to_string(temp.path().join("layouts.json"))
            .unwrap()
            .contains(raw)
    );
    // The accumulated mapping cannot append the same opaque tab twice.
    let mut edited = model.storage_project();
    edited.selected_task_id = Some("next-edit".into());
    loaded
        .persistence
        .schedule_with_remap(edited, model.storage_id_remap())
        .unwrap();
    loaded.persistence.flush().unwrap();
    assert_eq!(
        read(temp.path())["projects"][PROJECT]["panes"][main.to_string()]["tabs"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|tab| **tab == unknown)
            .count(),
        1
    );
}

#[test]
fn safe_reload_snapshot_carries_opaque_tabs_from_the_unsaved_unsafe_disk_generation() {
    let rt = runtime();
    let _entered = rt.enter();
    let temp = scratch();
    let input = named::seed(temp.path(), PROJECT, "L-BIGID");
    let old = input.main_pane.unwrap();
    let mut raw = read(temp.path());
    raw["projects"][PROJECT]["panes"][old.to_string()]["tabs"]
        .as_array_mut()
        .unwrap()
        .push(unknown_tab());
    write(temp.path(), &raw);
    let first =
        LayoutPersistence::open(temp.path(), PROJECT, None, SaveMode::LegacyChatState).unwrap();
    let mut model = LayoutModel::new(first.project).unwrap();
    model.mark_unknown_panes(&first.unknown_panes);
    let snapshot = model.storage_project();
    let safe = snapshot.main_pane.unwrap();
    assert_ne!(safe, old);
    drop(first.persistence);
    let restored = LayoutPersistence::open(
        temp.path(),
        PROJECT,
        Some(snapshot),
        SaveMode::LegacyChatState,
    )
    .unwrap();
    assert_eq!(restored.unknown_panes, [safe].into_iter().collect());
    let mut edited = restored.project;
    edited.selected_task_id = Some("restored-edit".into());
    restored.persistence.schedule(edited).unwrap();
    restored.persistence.flush().unwrap();
    let saved = read(temp.path());
    assert!(
        saved["projects"][PROJECT]["panes"]
            .get(old.to_string())
            .is_none()
    );
    assert!(
        saved["projects"][PROJECT]["panes"][safe.to_string()]["tabs"]
            .as_array()
            .unwrap()
            .contains(&unknown_tab())
    );
}
