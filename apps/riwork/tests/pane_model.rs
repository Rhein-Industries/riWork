//! Deterministic pane/session seams. Real Headless gestures are covered by the
//! gesture suite; these assert the core result and lifecycle preconditions.
use riwork_core::layouts::{Axis, Layout, ProjectLayout, SavedPane, SavedTab};
use riwork_nsh_workspace::layout::{DockNode, Extent, LayoutModel};
use std::collections::{BTreeSet, HashSet};

const AREA: Extent = Extent {
    width: 1280.0,
    height: 800.0,
};
fn shell(id: &str) -> SavedTab {
    SavedTab::Shell {
        shell_id: id.into(),
    }
}
fn fixture() -> LayoutModel {
    let mut project: ProjectLayout = serde_json::from_value(serde_json::json!({"layout":{"split":{"axis":"side_by_side","ratio":0.27,"first":{"pane":1},"second":{"split":{"axis":"stacked","ratio":0.6,"first":{"pane":2},"second":{"pane":3}}}}},"active_pane":3,"main_pane":2,"locked_panes":[1],"panels_initialized":true})).unwrap();
    for (id, tabs) in [(1, vec!["nav"]), (2, vec!["main"]), (3, vec!["a", "b"])] {
        project.panes.insert(
            id,
            SavedPane {
                tabs: tabs.into_iter().map(shell).collect(),
                ..SavedPane::default()
            },
        );
    }
    project.normalize().unwrap();
    let mut model = LayoutModel::new(project).unwrap();
    model.dock(AREA);
    model
}
fn ids(node: &DockNode) -> Vec<String> {
    match node {
        DockNode::Tabs { panels, .. } => panels.clone(),
        DockNode::Split { children, .. } => children.iter().flat_map(ids).collect(),
    }
}

#[test]
fn split_right_adds_the_new_pane_second_at_one_half() {
    let mut model = fixture();
    model.toggle_focus(true).unwrap();
    model.split_shell(shell("new"), Axis::SideBySide).unwrap();
    assert!(!model.focus_mode());
    assert_eq!(model.project().active_pane, 4);
    assert_eq!(
        model.project().panes[&4].active_tab_key.as_deref(),
        Some("shell:new")
    );
    let Layout::Split { second, .. } = &model.project().layout else {
        panic!()
    };
    let Layout::Split { second, .. } = &**second else {
        panic!()
    };
    assert_eq!(
        **second,
        Layout::Split {
            axis: Axis::SideBySide,
            ratio: 0.5,
            first: Box::new(Layout::Pane(3)),
            second: Box::new(Layout::Pane(4))
        }
    );
}

#[test]
fn a_failed_create_undoes_the_split() {
    let mut model = fixture();
    let before = model.storage_project();
    // The bridge creates before committing: failed or duplicate creations can
    // never publish a half-created split, allocate a pane, or schedule a save.
    assert!(model.split_shell(shell(""), Axis::SideBySide).is_err());
    assert_eq!(model.storage_project(), before);
    assert!(model.split_shell(shell("a"), Axis::SideBySide).is_err());
    assert_eq!(model.storage_project(), before);
    model.split_shell(shell("new"), Axis::Stacked).unwrap();
    assert_eq!(model.project().active_pane, 4);
}

#[test]
fn focus_mode_never_changes_the_saved_layout() {
    let mut model = fixture();
    let before = model.storage_project();
    let full = model.dock(AREA);
    assert_eq!(model.toggle_focus(true).unwrap(), full);
    assert!(model.centered_focus());
    model
        .set_extent(Extent {
            width: 1920.0,
            height: 1080.0,
        })
        .unwrap();
    assert_eq!(model.storage_project(), before);
    model.toggle_focus(false).unwrap();
    assert!(!model.focus_mode());
    assert_eq!(model.storage_project(), before);
}

#[test]
fn centered_focus_enter_exit_preserves_the_normal_resize_baseline() {
    let mut model = fixture();
    let before = model.storage_project();
    let focused = Extent {
        width: 1100.0,
        height: 560.0,
    };
    model.toggle_focus(true).unwrap();
    model.set_extent(focused).unwrap();
    model.toggle_focus(false).unwrap();
    // A subscription/thaw can request the latest reported projection before
    // the normal surface's onLayout arrives. It must not replace the baseline.
    model.dock(focused);
    model.set_extent(AREA).unwrap();
    assert_eq!(model.storage_project(), before);

    // A window restored directly into centered focus has no normal baseline.
    let mut restored = LayoutModel::new(before.clone()).unwrap();
    restored.restore_focus(true, true);
    restored.dock(focused);
    restored.toggle_focus(false).unwrap();
    restored.dock(focused);
    restored.set_extent(AREA).unwrap();
    assert_eq!(restored.storage_project(), before);
}

#[test]
fn a_new_tab_routes_to_main_and_focus_mode_routes_to_selected() {
    let mut model = fixture();
    model.open_tab(shell("normal")).unwrap();
    assert_eq!(model.project().active_pane, 2);
    model.select_pane(3).unwrap();
    model.toggle_focus(false).unwrap();
    model.open_tab(shell("focused")).unwrap();
    assert_eq!(model.project().active_pane, 3);
    assert_eq!(
        model.project().panes[&3].active_tab_key.as_deref(),
        Some("shell:focused")
    );
}

#[test]
fn locked_closes_are_refused_but_internal_removals_are_allowed() {
    let mut model = fixture();
    let before = model.storage_project();
    assert_eq!(
        model.close_panel("shell:nav", false).unwrap_err(),
        "Unlock the pane to close its tabs"
    );
    assert_eq!(
        model.close_pane(1).unwrap_err(),
        "Unlock the pane to close it"
    );
    assert_eq!(model.storage_project(), before);
    model.close_panel("shell:nav", true).unwrap();
    assert!(model.panel("shell:nav").is_none());
}

#[test]
fn a_closed_pane_preserves_order_membership_and_main_selection() {
    let mut model = fixture();
    model.close_pane(3).unwrap();
    assert_eq!(model.project().active_pane, 2);
    assert_eq!(
        model.project().panes[&2].tabs,
        vec![shell("main"), shell("a"), shell("b")]
    );
    assert_eq!(
        model.project().panes[&2].active_tab_key.as_deref(),
        Some("shell:main")
    );
    assert!(model.project().detached_shell_ids.is_empty());
}

#[test]
fn an_empty_main_keeps_its_sentinel_through_every_gesture() {
    let mut model = fixture();
    model.close_panel("shell:main", false).unwrap();
    for step in 0..4 {
        let dock = match step {
            0 => model.select_pane(2).unwrap(),
            1 => model.toggle_focus(false).unwrap(),
            2 => model.toggle_focus(false).unwrap(),
            _ => model
                .set_extent(Extent {
                    width: 1920.0,
                    height: 1080.0,
                })
                .unwrap(),
        };
        assert!(ids(&dock.center).contains(&"empty:2".into()));
        model.apply_dock(dock, AREA).unwrap();
        assert!(model.project().panes[&2].tabs.is_empty());
    }
}

#[test]
fn chat_pending_is_never_saved_and_maps_to_chat_id() {
    let mut model = fixture();
    let projected = model
        .open_pending_chat("chat-pending:fixture-uuid")
        .unwrap();
    assert!(ids(&projected.center).contains(&"chat-pending:fixture-uuid".into()));
    assert!(
        model
            .storage_project()
            .panes
            .values()
            .flat_map(|p| &p.tabs)
            .all(|t| !matches!(t, SavedTab::Chat { .. }))
    );
    model.apply_dock(projected, AREA).unwrap();
    let resolved = model
        .resolve_pending_chat("chat-pending:fixture-uuid", "saved-id")
        .unwrap();
    assert!(ids(&resolved.center).contains(&"chat-pending:fixture-uuid".into()));
    assert!(!ids(&resolved.center).contains(&"chat:saved-id".into()));
    assert_eq!(
        model.panel("chat-pending:fixture-uuid"),
        Some(&SavedTab::Chat {
            chat_id: "saved-id".into()
        })
    );
    assert!(
        model.storage_project().panes[&2]
            .tabs
            .contains(&SavedTab::Chat {
                chat_id: "saved-id".into()
            })
    );
    model.apply_dock(resolved, AREA).unwrap();
    model.select_panel("chat-pending:fixture-uuid").unwrap();
    assert_eq!(
        model.project().panes[&2].active_tab_key.as_deref(),
        Some("chat:saved-id")
    );
}

#[test]
fn restore_skips_dead_and_detached_shells() {
    let mut model = fixture();
    let live = vec!["nav".into(), "main".into(), "a".into(), "cli".into()];
    model
        .reconcile_shells(&live, &live, &BTreeSet::from(["a".into()]))
        .unwrap();
    assert!(model.panel("shell:a").is_none());
    assert!(model.panel("shell:b").is_none());
    assert!(model.panel("shell:cli").is_some());
    assert!(model.project().panes.contains_key(&3));
    assert_eq!(
        model.project().panes[&2].active_tab_key.as_deref(),
        Some("shell:main")
    );
}

#[test]
fn restore_adopts_cli_shells_into_main_with_sixteen_per_delivery_and_locked_filter() {
    let mut model = fixture();
    let mut live = vec!["nav".into(), "main".into(), "a".into(), "b".into()];
    let adoptable = (0..20).map(|i| format!("cli-{i}")).collect::<Vec<_>>();
    live.extend(adoptable.clone());
    model
        .reconcile_shells(&live, &adoptable, &BTreeSet::new())
        .unwrap();
    assert_eq!(model.project().panes[&2].tabs.len(), 17);
    model
        .reconcile_shells(&live, &adoptable, &BTreeSet::new())
        .unwrap();
    assert_eq!(model.project().panes[&2].tabs.len(), 21);
    assert_eq!(model.project().panes[&1].tabs, vec![shell("nav")]);
    assert_eq!(model.project().effective_locked_panes(), HashSet::from([1]));
}

#[test]
fn default_and_gather_exit_focus_and_report_the_exact_result() {
    let mut model = fixture();
    model.toggle_focus(true).unwrap();
    model.gather().unwrap();
    assert!(!model.focus_mode());
    assert_eq!(model.notice(), Some("Gathered 2 tabs into the main pane"));
    model.gather().unwrap();
    assert_eq!(
        model.notice(),
        Some("No tabs to gather: every unlocked tab is in the main pane already")
    );
    model.default_layout().unwrap();
    assert!(
        model
            .notice()
            .unwrap()
            .starts_with("Applied the default layout")
    );
    let before = model.storage_project();
    model.default_layout().unwrap();
    assert_eq!(
        model.notice(),
        Some("The default layout is already in place")
    );
    assert_eq!(model.storage_project(), before);
}

#[test]
fn closing_the_last_pane_empties_its_tabs_without_removing_the_pane() {
    let mut model = fixture();
    model.toggle_lock(1).unwrap();
    model.close_pane(1).unwrap();
    model.close_pane(3).unwrap();
    assert_eq!(model.project().panes.len(), 1);
    model.close_pane(2).unwrap();
    assert_eq!(model.project().layout, Layout::Pane(2));
    assert!(model.project().panes[&2].tabs.is_empty());
    assert_eq!(model.project().detached_shell_ids.len(), 4);
}

#[test]
fn adoption_uses_active_unlocked_pane_when_main_is_locked_and_normalizes_hidden_keys() {
    let mut model = fixture();
    model.toggle_lock(2).unwrap();
    let live = vec![
        "nav".into(),
        "main".into(),
        "a".into(),
        "b".into(),
        "cli".into(),
        "hidden".into(),
    ];
    model
        .reconcile_shells(&live, &live, &BTreeSet::from(["shell:hidden".into()]))
        .unwrap();
    assert!(model.project().panes[&3].tabs.contains(&shell("cli")));
    assert!(model.panel("shell:hidden").is_none());
    assert_eq!(
        model.project().panes[&3].active_tab_key.as_deref(),
        Some("shell:a")
    );
}

#[test]
fn reopening_an_existing_navigation_panel_selects_it_where_it_lives() {
    let mut model = fixture();
    let mut project = model.project().clone();
    project
        .panes
        .get_mut(&3)
        .unwrap()
        .tabs
        .push(SavedTab::Panel {
            panel: riwork_core::layouts::PanelKind::Files,
        });
    model.replace_project(project).unwrap();
    model
        .open_tab(SavedTab::Panel {
            panel: riwork_core::layouts::PanelKind::Files,
        })
        .unwrap();
    assert_eq!(model.project().active_pane, 3);
    assert_eq!(
        model.project().panes[&3].active_tab_key.as_deref(),
        Some("panel:files")
    );
}

#[test]
fn same_project_updates_preserve_focus_and_opaque_aliases_across_default_layout() {
    const UNSAFE: u64 = 9_007_199_254_740_992;
    let mut project = fixture().project().clone();
    project.layout = Layout::Split {
        axis: Axis::SideBySide,
        ratio: 0.4,
        first: Box::new(Layout::Pane(UNSAFE)),
        second: Box::new(Layout::Pane(2)),
    };
    let source = project.panes.remove(&3).unwrap();
    project.panes.clear();
    project.panes.insert(UNSAFE, source);
    project.panes.insert(2, SavedPane::default());
    project.active_pane = UNSAFE;
    project.locked_panes = Some(HashSet::new());
    let mut model = LayoutModel::new(project).unwrap();
    let safe = model.pane_id_remap()[&UNSAFE];
    model.mark_unknown_panes(&BTreeSet::from([UNSAFE]));
    model.toggle_focus(true).unwrap();
    model.update_same_project(model.project().clone()).unwrap();
    assert!(model.focus_mode());
    assert!(model.centered_focus());
    assert_eq!(model.storage_id_remap()[&UNSAFE], safe);

    model.default_layout().unwrap();
    let main = model.project().main_pane.unwrap();
    assert_eq!(model.storage_id_remap()[&UNSAFE], main);
    assert_eq!(model.storage_id_remap()[&safe], main);
    model.update_same_project(model.project().clone()).unwrap();
    assert_eq!(model.storage_id_remap()[&UNSAFE], main);
    assert_eq!(model.storage_id_remap()[&safe], main);
    assert!(!model.storage_project().panes.contains_key(&UNSAFE));
}

#[test]
fn editor_tabs_reuse_existing_placement_and_make_room_when_every_pane_is_locked() {
    let mut model = fixture();
    model.open_editor_shell("vim").unwrap();
    assert_eq!(model.project().active_pane, 2, "main receives a new editor");
    model.select_pane(3).unwrap();
    model.open_editor_shell("vim").unwrap();
    assert_eq!(
        model.project().active_pane,
        2,
        "reopening selects the existing editor"
    );
    assert_eq!(
        model
            .project()
            .panes
            .values()
            .flat_map(|pane| &pane.tabs)
            .filter(|tab| tab.key() == "shell:vim")
            .count(),
        1
    );

    let mut project = model.project().clone();
    project.main_pane = None;
    project.locked_panes = Some(HashSet::from([1, 2, 3]));
    model.replace_project(project).unwrap();
    model.open_editor_shell("vim-other").unwrap();
    let added = model.project().active_pane;
    assert!(!model.project().effective_locked_panes().contains(&added));
    assert_eq!(
        model.project().panes[&added].active_tab_key.as_deref(),
        Some("shell:vim-other")
    );
    assert_eq!(model.project().panes.len(), 4);
}

#[test]
fn focused_editor_routing_uses_unlocked_terminal_pane_instead_of_main() {
    let mut model = fixture();
    let mut project = model.project().clone();
    project.locked_panes = Some(HashSet::from([1, 2]));
    model.replace_project(project).unwrap();
    model.toggle_focus(false).unwrap();
    model.open_editor_shell("focused-vim").unwrap();
    assert_eq!(model.project().active_pane, 3);
    assert!(model.focus_mode());
}
