//! Workspace pane operations shared by desktop renderers. These preserve tab
//! identity and contain no UI, session lifecycle or filesystem side effects.

use super::{
    Layout, NAVIGATION_PANELS, OpenTabReuse, PaneId, PanelKind, PreviewTab, ProjectLayout,
    SavedPane, SavedTab, reuse_open_tab,
};
use std::collections::{BTreeMap, BTreeSet, HashSet};

pub fn routing_target(
    main: Option<PaneId>,
    panes: &BTreeMap<PaneId, SavedPane>,
    focus_mode: bool,
) -> Option<PaneId> {
    main.filter(|id| !focus_mode && panes.contains_key(id))
}

pub fn new_tab_target(
    main: Option<PaneId>,
    panes: &BTreeMap<PaneId, SavedPane>,
    focus_mode: bool,
    selected: PaneId,
) -> PaneId {
    routing_target(main, panes, focus_mode).unwrap_or(selected)
}

pub fn bring_up_target(
    project: &ProjectLayout,
    focus_mode: bool,
    pane_id: PaneId,
    key: &str,
) -> Option<PaneId> {
    let main = routing_target(project.main_pane, &project.panes, focus_mode)?;
    let pane = project.panes.get(&pane_id)?;
    if !pane.tabs.iter().any(|tab| tab.key() == key) {
        return None;
    }
    let shown = pane.active_tab_key.as_deref() == Some(key);
    let behind_shell = pane.tabs.iter().any(|tab| {
        pane.active_tab_key.as_deref() == Some(tab.key().as_str())
            && matches!(tab, SavedTab::Shell { .. } | SavedTab::RemoteShell { .. })
    });
    (reuse_open_tab(
        main,
        PreviewTab {
            pane: pane_id,
            shown,
            behind_shell,
        },
        project.effective_locked_panes().contains(&pane_id),
    ) == OpenTabReuse::IntoMain)
        .then_some(main)
}

pub fn aggregation_target(
    main: Option<PaneId>,
    closing: PaneId,
    closing_locked: bool,
) -> Option<PaneId> {
    main.filter(|main| *main != closing && !closing_locked)
}

pub fn close_pane_tabs(
    panes: &mut BTreeMap<PaneId, SavedPane>,
    closed: PaneId,
    main: Option<PaneId>,
) -> Vec<SavedTab> {
    let Some(mut pane) = panes.remove(&closed) else {
        return Vec::new();
    };
    if let Some(target) = main.and_then(|id| panes.get_mut(&id)) {
        target.tabs.append(&mut pane.tabs);
        if target.active_tab_key.is_none() {
            target.active_tab_key = target.tabs.first().map(SavedTab::key);
        }
        Vec::new()
    } else {
        pane.tabs
    }
}

pub fn pane_inheriting_space(layout: &Layout, closed: PaneId) -> Option<PaneId> {
    match layout {
        Layout::Pane(_) => None,
        Layout::Split { first, second, .. } => match (&**first, &**second) {
            (Layout::Pane(id), rest) if *id == closed => Some(rest.first_pane()),
            (rest, Layout::Pane(id)) if *id == closed => rest.pane_ids().last().copied(),
            _ => pane_inheriting_space(first, closed)
                .or_else(|| pane_inheriting_space(second, closed)),
        },
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Gathered {
    pub moved: usize,
    pub removed: Vec<PaneId>,
    pub kept: usize,
}

pub fn gather_notice(gathered: &Gathered) -> String {
    let tabs = match gathered.moved {
        0 => "No tabs to gather: every unlocked tab is in the main pane already".to_owned(),
        1 => "Gathered 1 tab into the main pane".to_owned(),
        n => format!("Gathered {n} tabs into the main pane"),
    };
    match gathered.kept {
        0 => tabs,
        1 => format!("{tabs}. An empty pane stays beside a locked one so that it keeps its size"),
        n => {
            format!("{tabs}. {n} empty panes stay beside locked ones so that they keep their size")
        }
    }
}

pub fn gather(
    project: &mut ProjectLayout,
    opaque_panes: &BTreeSet<PaneId>,
) -> Result<Gathered, String> {
    let main = project
        .main_pane
        .filter(|id| project.panes.contains_key(id))
        .ok_or("No main pane. Choose Main pane in a pane's … menu first")?;
    let locks = project.effective_locked_panes();
    let selected = project.panes[&main].active_tab_key.clone();
    let mut result = Gathered::default();
    for source in project
        .layout
        .gather_sources(main, &|id| locks.contains(&id))
    {
        let pane = project.panes.get_mut(&source).unwrap();
        let tabs = std::mem::take(&mut pane.tabs);
        result.moved += tabs.len();
        pane.shell_ids.clear();
        pane.active_shell_id = None;
        pane.active_tab_key = None;
        project.panes.get_mut(&main).unwrap().tabs.extend(tabs);
        if opaque_panes.contains(&source)
            || project
                .layout
                .removal_resizes_locked(source, &|id| locks.contains(&id))
        {
            result.kept += 1;
        } else if let Some(rest) = project.layout.clone().without(source) {
            project.layout = rest;
            project.panes.remove(&source);
            result.removed.push(source);
        }
    }
    project.panes.get_mut(&main).unwrap().active_tab_key = selected;
    project.locked_panes = Some(locks);
    project.active_pane = main;
    project.normalize()?;
    Ok(result)
}

#[derive(Debug, PartialEq, Eq)]
pub struct DefaultLayout {
    pub navigation: PaneId,
    pub main: PaneId,
    pub active_pane: PaneId,
    pub moved: usize,
    pub opened: Vec<PanelKind>,
}

pub fn is_default_layout(project: &ProjectLayout) -> bool {
    let Some((navigation, main)) = project.layout.two_panes_side_by_side() else {
        return false;
    };
    let locked = project.effective_locked_panes();
    project.panes.len() == 2
        && project.main_pane == Some(main)
        && locked.contains(&navigation)
        && !locked.contains(&main)
        && project.panes.get(&navigation).is_some_and(|pane| {
            pane.tabs
                .iter()
                .map(|tab| match tab {
                    SavedTab::Panel { panel } => Some(*panel),
                    _ => None,
                })
                .eq(NAVIGATION_PANELS.map(Some))
        })
}

pub fn default_layout(
    project: &mut ProjectLayout,
    reserved: &BTreeSet<PaneId>,
) -> Result<Option<DefaultLayout>, String> {
    if is_default_layout(project) {
        return Ok(None);
    }
    let order = project.layout.pane_ids();
    let navigation = order.iter().copied().find(|id| {
        Some(*id) != project.main_pane
            && project.panes[id].tabs.iter().any(
                |tab| matches!(tab, SavedTab::Panel { panel } if NAVIGATION_PANELS.contains(panel)),
            )
    });
    let main = project
        .main_pane
        .filter(|id| project.panes.contains_key(id))
        .or_else(|| order.iter().copied().find(|id| Some(*id) != navigation));
    let mut used = reserved.clone();
    used.extend(order.iter());
    let mut fresh = || {
        let id = (1..).find(|id| !used.contains(id)).unwrap();
        used.insert(id);
        id
    };
    let navigation = navigation.unwrap_or_else(&mut fresh);
    let main = main.unwrap_or_else(&mut fresh);
    let mut slots: [Option<SavedTab>; 5] = Default::default();
    let mut others = Vec::new();
    let mut moved = 0;
    let (mut nav_shown, mut nav_active, mut others_shown, mut others_main, mut others_active) =
        (None, None, None, None, None);
    for id in order {
        let pane = &project.panes[&id];
        for tab in &pane.tabs {
            let key = tab.key();
            let selected = pane.active_tab_key.as_deref() == Some(key.as_str());
            let slot = match tab {
                SavedTab::Panel { panel } => NAVIGATION_PANELS
                    .iter()
                    .position(|kind| kind == panel)
                    .filter(|slot| slots[*slot].is_none()),
                _ => None,
            };
            if let Some(slot) = slot {
                moved += usize::from(id != navigation);
                if selected {
                    nav_shown.get_or_insert(key.clone());
                    if id == project.active_pane {
                        nav_active = Some(key);
                    }
                }
                slots[slot] = Some(tab.clone());
            } else {
                moved += usize::from(id != main);
                if selected {
                    others_shown.get_or_insert(key.clone());
                    if Some(id) == project.main_pane {
                        others_main = Some(key.clone());
                    }
                    if id == project.active_pane {
                        others_active = Some(key);
                    }
                }
                others.push(tab.clone());
            }
        }
    }
    let mut opened = Vec::new();
    let navigation_tabs = NAVIGATION_PANELS
        .into_iter()
        .zip(slots)
        .map(|(panel, slot)| {
            slot.unwrap_or_else(|| {
                opened.push(panel);
                SavedTab::Panel { panel }
            })
        })
        .collect();
    let (active_pane, nav_selected, main_selected) = if let Some(key) = nav_active {
        (navigation, Some(key), others_main.or(others_shown))
    } else {
        (
            main,
            nav_shown,
            others_active.or(others_main).or(others_shown),
        )
    };
    project.panes = BTreeMap::from([
        (
            navigation,
            SavedPane {
                tabs: navigation_tabs,
                active_tab_key: nav_selected,
                ..SavedPane::default()
            },
        ),
        (
            main,
            SavedPane {
                tabs: others,
                active_tab_key: main_selected,
                ..SavedPane::default()
            },
        ),
    ]);
    project.layout = Layout::navigation_beside(navigation, Layout::Pane(main));
    project.main_pane = Some(main);
    project.active_pane = active_pane;
    project.locked_panes = Some(HashSet::from([navigation]));
    project.panels_initialized = true;
    project.normalize()?;
    Ok(Some(DefaultLayout {
        navigation,
        main,
        active_pane,
        moved,
        opened,
    }))
}

pub fn default_layout_notice(applied: &DefaultLayout) -> String {
    let mut notice = match applied.moved {
        0 => "Applied the default layout".to_owned(),
        1 => "Applied the default layout: moved 1 tab".to_owned(),
        n => format!("Applied the default layout: moved {n} tabs"),
    };
    if !applied.opened.is_empty() {
        notice.push_str(if applied.moved == 0 {
            ": opened "
        } else {
            ", opened "
        });
        notice.push_str(
            &applied
                .opened
                .iter()
                .map(|panel| panel.label())
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    notice.push_str(". Nothing was closed");
    notice
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layouts::{Axis, Extent, LayoutStore};

    fn side(a: Layout, b: Layout) -> Layout {
        Layout::Split {
            axis: Axis::SideBySide,
            ratio: 0.5,
            first: Box::new(a),
            second: Box::new(b),
        }
    }
    fn shell(id: &str) -> SavedTab {
        SavedTab::Shell {
            shell_id: id.into(),
        }
    }
    fn panel(panel: PanelKind) -> SavedTab {
        SavedTab::Panel { panel }
    }
    fn pane(tabs: Vec<SavedTab>, selected: usize) -> SavedPane {
        SavedPane {
            active_tab_key: tabs.get(selected).map(SavedTab::key),
            tabs,
            ..SavedPane::default()
        }
    }
    fn project() -> ProjectLayout {
        let mut result: ProjectLayout = serde_json::from_value(serde_json::json!({"layout":{"pane":1},"panes":{},"active_pane":3,"main_pane":2,"locked_panes":[1],"panels_initialized":true})).unwrap();
        result.layout = Layout::navigation_beside(1, side(Layout::Pane(2), Layout::Pane(3)));
        result.panes = BTreeMap::from([
            (
                1,
                pane(vec![panel(PanelKind::Projects), panel(PanelKind::Files)], 1),
            ),
            (2, pane(vec![shell("a"), shell("b")], 1)),
            (3, pane(vec![shell("c"), shell("d")], 0)),
        ]);
        result.normalize().unwrap();
        result
    }
    fn keys(project: &ProjectLayout, pane: PaneId) -> Vec<String> {
        project.panes[&pane]
            .tabs
            .iter()
            .map(SavedTab::key)
            .collect()
    }
    fn all_keys(project: &ProjectLayout) -> BTreeSet<String> {
        project
            .panes
            .values()
            .flat_map(|pane| pane.tabs.iter().map(SavedTab::key))
            .collect()
    }

    #[test]
    fn only_a_main_pane_that_exists_outside_focus_mode_takes_what_is_opened() {
        let p = project();
        assert_eq!(routing_target(Some(2), &p.panes, false), Some(2));
        assert_eq!(routing_target(None, &p.panes, false), None);
        assert_eq!(routing_target(Some(2), &p.panes, true), None);
        assert_eq!(routing_target(Some(9), &p.panes, false), None);
    }

    #[test]
    fn a_new_tab_opens_in_the_main_pane_whichever_pane_is_selected() {
        let p = project();
        for selected in [1, 2, 3] {
            assert_eq!(new_tab_target(p.main_pane, &p.panes, false, selected), 2);
        }
        assert_eq!(new_tab_target(p.main_pane, &p.panes, true, 3), 3);
        assert_eq!(new_tab_target(None, &p.panes, false, 3), 3);
        assert_eq!(new_tab_target(Some(1), &p.panes, false, 3), 1);
    }

    #[test]
    fn an_open_chat_orchestrator_tab_comes_to_the_front_by_the_main_pane_rules() {
        let mut p = project();
        p.panes.get_mut(&3).unwrap().tabs.push(SavedTab::Chat {
            chat_id: "orchestrator".into(),
        });
        assert_eq!(bring_up_target(&p, false, 3, "chat:orchestrator"), Some(2));
        assert_eq!(bring_up_target(&p, true, 3, "chat:orchestrator"), None);
        p.locked_panes = Some(HashSet::from([1, 3]));
        assert_eq!(bring_up_target(&p, false, 3, "chat:orchestrator"), None);
        p.locked_panes = Some(HashSet::new());
        p.panes.get_mut(&3).unwrap().active_tab_key = Some("chat:orchestrator".into());
        assert_eq!(bring_up_target(&p, false, 3, "chat:orchestrator"), None);
    }

    #[test]
    fn closing_a_pane_focuses_the_pane_that_takes_its_space() {
        let layout = side(
            Layout::Pane(1),
            side(Layout::Pane(2), side(Layout::Pane(3), Layout::Pane(4))),
        );
        for (closed, inherits) in [(4, 3), (3, 4), (2, 3), (1, 2)] {
            assert_eq!(pane_inheriting_space(&layout, closed), Some(inherits));
        }
        let layout = side(side(Layout::Pane(1), Layout::Pane(2)), Layout::Pane(3));
        assert_eq!(pane_inheriting_space(&layout, 3), Some(2));
        assert_eq!(pane_inheriting_space(&layout, 9), None);
        assert_eq!(pane_inheriting_space(&Layout::Pane(1), 1), None);
    }

    #[test]
    fn a_closed_pane_hands_its_tabs_to_the_main_pane_after_its_own() {
        let mut p = project();
        let nav = p.panes[&1].clone();
        assert!(close_pane_tabs(&mut p.panes, 3, Some(2)).is_empty());
        assert_eq!(keys(&p, 2), ["shell:a", "shell:b", "shell:c", "shell:d"]);
        assert_eq!(p.panes[&2].active_tab_key.as_deref(), Some("shell:b"));
        assert_eq!(p.panes[&1], nav);
        let mut p = project();
        p.panes.insert(2, SavedPane::default());
        close_pane_tabs(&mut p.panes, 3, Some(2));
        assert_eq!(p.panes[&2].active_tab_key.as_deref(), Some("shell:c"));
    }

    #[test]
    fn closing_the_main_pane_or_a_locked_one_or_having_no_main_pane_closes_the_tabs_as_before() {
        assert_eq!(aggregation_target(Some(2), 2, false), None);
        assert_eq!(aggregation_target(Some(2), 1, true), None);
        assert_eq!(aggregation_target(None, 3, false), None);
        for target in [None, Some(9)] {
            let mut p = project();
            assert_eq!(
                close_pane_tabs(&mut p.panes, 3, target),
                vec![shell("c"), shell("d")]
            );
            assert_eq!(keys(&p, 2), ["shell:a", "shell:b"]);
        }
    }

    #[test]
    fn a_gather_moves_every_unlocked_tab_into_the_main_pane_and_leaves_the_locked_pane_alone() {
        let mut p = project();
        let before = all_keys(&p);
        let nav = p.panes[&1].clone();
        let extent = Extent {
            width: 1280.0,
            height: 800.0,
        };
        let size = p.layout.pane_extents(extent)[&1];
        assert_eq!(
            gather(&mut p, &BTreeSet::new()).unwrap(),
            Gathered {
                moved: 2,
                removed: vec![3],
                kept: 0
            }
        );
        assert_eq!(keys(&p, 2), ["shell:a", "shell:b", "shell:c", "shell:d"]);
        assert_eq!(p.panes[&2].active_tab_key.as_deref(), Some("shell:b"));
        assert_eq!(p.panes[&1], nav);
        assert_eq!(p.layout.pane_extents(extent)[&1], size);
        assert_eq!(all_keys(&p), before);
        assert_eq!(
            gather(&mut p, &BTreeSet::new()).unwrap(),
            Gathered::default()
        );
    }

    #[test]
    fn a_gather_never_touches_any_locked_pane_among_several() {
        let mut p = project();
        p.layout = side(p.layout, side(Layout::Pane(4), Layout::Pane(5)));
        p.panes.insert(4, pane(vec![shell("locked")], 0));
        p.panes.insert(5, pane(vec![shell("unlocked")], 0));
        p.locked_panes = Some(HashSet::from([1, 4]));
        p.normalize().unwrap();
        let before = p.clone();
        let result = gather(&mut p, &BTreeSet::new()).unwrap();
        assert_eq!(
            result,
            Gathered {
                moved: 3,
                removed: vec![3],
                kept: 1
            }
        );
        assert!(p.panes[&5].tabs.is_empty());
        let extent = Extent {
            width: 1280.0,
            height: 800.0,
        };
        for id in [1, 4] {
            assert_eq!(p.panes[&id], before.panes[&id]);
            assert_eq!(
                p.layout.pane_extents(extent)[&id],
                before.layout.pane_extents(extent)[&id]
            );
        }
    }

    #[test]
    fn a_messy_window_becomes_a_locked_navigation_pane_beside_the_main_pane() {
        let mut p = project();
        p.panes.get_mut(&1).unwrap().tabs.push(shell("locked"));
        p.panes
            .get_mut(&3)
            .unwrap()
            .tabs
            .push(panel(PanelKind::Tasks));
        p.normalize().unwrap();
        let applied = default_layout(&mut p, &BTreeSet::new()).unwrap().unwrap();
        assert_eq!((applied.navigation, applied.main), (1, 2));
        assert_eq!(p.layout, Layout::navigation_beside(1, Layout::Pane(2)));
        assert!(is_default_layout(&p));
        assert_eq!(
            keys(&p, 1),
            NAVIGATION_PANELS.map(|panel| SavedTab::Panel { panel }.key())
        );
        assert_eq!(
            keys(&p, 2),
            ["shell:locked", "shell:a", "shell:b", "shell:c", "shell:d"]
        );
        assert_eq!(p.panes[&2].active_tab_key.as_deref(), Some("shell:c"));
    }

    #[test]
    fn the_default_layout_closes_nothing_and_a_locked_panes_tabs_are_moved_not_dropped() {
        let mut p = project();
        p.panes.get_mut(&1).unwrap().tabs.push(shell("locked"));
        let before = all_keys(&p);
        default_layout(&mut p, &BTreeSet::new()).unwrap();
        assert!(all_keys(&p).is_superset(&before));
        assert_eq!(
            all_keys(&p).len(),
            p.panes.values().map(|pane| pane.tabs.len()).sum::<usize>()
        );
        assert!(keys(&p, 2).contains(&"shell:locked".into()));
    }

    #[test]
    fn a_window_that_is_laid_out_already_is_left_exactly_as_it_is() {
        let mut p = project();
        default_layout(&mut p, &BTreeSet::new()).unwrap();
        p.layout.set_ratio(&[], 0.4);
        p.active_pane = 1;
        p.panes.get_mut(&1).unwrap().active_tab_key = Some("panel:tasks".into());
        let before = p.clone();
        assert_eq!(default_layout(&mut p, &BTreeSet::new()).unwrap(), None);
        assert_eq!(p, before);
    }

    #[test]
    fn panes_keep_their_ids_and_a_new_one_is_made_only_when_there_is_none_to_keep() {
        let mut p = project();
        p.layout = Layout::Pane(2);
        p.panes.retain(|id, _| *id == 2);
        p.normalize().unwrap();
        let applied = default_layout(&mut p, &BTreeSet::from([1, 2, 3]))
            .unwrap()
            .unwrap();
        assert_eq!((applied.navigation, applied.main), (4, 2));
        let mut p = project();
        let applied = default_layout(&mut p, &BTreeSet::new()).unwrap().unwrap();
        assert_eq!((applied.navigation, applied.main), (1, 2));
    }

    #[test]
    fn the_default_layout_is_saved_as_the_projects_layout_and_reads_back_as_it_was() {
        let home = std::env::temp_dir().join(format!(
            "riwork-core-default-layout-{}",
            uuid::Uuid::new_v4()
        ));
        let store = LayoutStore::open(&home).unwrap();
        let mut p = project();
        default_layout(&mut p, &BTreeSet::new()).unwrap();
        store.save("project", &p).unwrap();
        assert_eq!(store.load("project").unwrap(), Some(p));
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn lock_state_follows_the_users_choice_and_defaults_to_the_navigation_pane() {
        let mut p = project();
        p.locked_panes = None;
        assert_eq!(p.effective_locked_panes(), HashSet::from([1]));
        p.locked_panes = Some(HashSet::new());
        assert!(p.effective_locked_panes().is_empty());
        p.locked_panes = Some(HashSet::from([2]));
        assert_eq!(p.effective_locked_panes(), HashSet::from([2]));
        p.locked_panes = None;
        p.panes.get_mut(&1).unwrap().tabs = vec![shell("only-shell")];
        assert!(p.effective_locked_panes().is_empty());
    }
}
