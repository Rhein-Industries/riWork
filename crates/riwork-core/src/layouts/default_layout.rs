//! Saved-state successor of the desktop's default-layout placement rule.
use super::*;

const NAVIGATION: [PanelKind; 5] = [
    PanelKind::Projects,
    PanelKind::Files,
    PanelKind::Worktrees,
    PanelKind::Tasks,
    PanelKind::Shells,
];

/// Restore navigation and main panes without closing tabs or resetting a
/// navigation width that already has the default arrangement.
pub fn apply_default_layout(project: &mut ProjectLayout) -> Result<(), String> {
    let locks = project.effective_locked_panes();
    if let Some((left, right)) = project.layout.two_panes_side_by_side()
        && project.panes.len() == 2
        && project.main_pane == Some(right)
        && locks.contains(&left)
        && !locks.contains(&right)
        && project.panes[&left].tabs.iter().eq(NAVIGATION
            .iter()
            .map(|panel| SavedTab::Panel { panel: *panel })
            .collect::<Vec<_>>()
            .iter())
    {
        return Ok(());
    }
    let mut order = project.layout.pane_ids();
    order.extend(
        project
            .panes
            .keys()
            .filter(|id| !project.layout.pane_ids().contains(id))
            .copied(),
    );
    let holds_navigation = |id: &PaneId| {
        project.panes.get(id).is_some_and(|pane| {
            pane.tabs
                .iter()
                .any(|tab| matches!(tab, SavedTab::Panel { panel } if NAVIGATION.contains(panel)))
        })
    };
    let navigation = order
        .iter()
        .copied()
        .find(|id| Some(*id) != project.main_pane && holds_navigation(id));
    let right = project
        .main_pane
        .filter(|id| project.panes.contains_key(id))
        .or_else(|| order.iter().copied().find(|id| Some(*id) != navigation));
    let mut used = project.panes.keys().copied().collect();
    let navigation = match navigation {
        Some(id) => id,
        None => available_pane_id(0, &mut used)?,
    };
    let right = match right {
        Some(id) => id,
        None => available_pane_id(0, &mut used)?,
    };
    let mut slots: [Option<SavedTab>; 5] = Default::default();
    let mut others = Vec::new();
    let (
        mut navigation_shown,
        mut navigation_active,
        mut others_shown,
        mut others_in_main,
        mut others_active,
    ) = (None, None, None, None, None);
    for id in order {
        let Some(pane) = project.panes.remove(&id) else {
            continue;
        };
        for tab in pane.tabs {
            let selected = pane.active_tab_key.as_deref() == Some(tab.key().as_str());
            let slot = match &tab {
                SavedTab::Panel { panel } => NAVIGATION.iter().position(|kind| kind == panel),
                _ => None,
            }
            .filter(|slot| slots[*slot].is_none());
            if let Some(slot) = slot {
                if selected {
                    navigation_shown.get_or_insert(slot);
                    if id == project.active_pane {
                        navigation_active = Some(slot);
                    }
                }
                slots[slot] = Some(tab);
            } else {
                if selected {
                    let position = others.len();
                    others_shown.get_or_insert(position);
                    if Some(id) == project.main_pane {
                        others_in_main.get_or_insert(position);
                    }
                    if id == project.active_pane {
                        others_active = Some(position);
                    }
                }
                others.push(tab);
            }
        }
    }
    let navigation_tabs: Vec<_> = NAVIGATION
        .into_iter()
        .zip(slots)
        .map(|(panel, tab)| tab.unwrap_or(SavedTab::Panel { panel }))
        .collect();
    let (active, navigation_selected, other_selected) = if let Some(slot) = navigation_active {
        (navigation, slot, others_in_main.or(others_shown))
    } else {
        (
            right,
            navigation_shown.unwrap_or(0),
            others_active.or(others_in_main).or(others_shown),
        )
    };
    let selected_other = other_selected
        .and_then(|index| others.get(index))
        .map(SavedTab::key);
    project.panes.insert(
        navigation,
        SavedPane {
            active_tab_key: Some(navigation_tabs[navigation_selected].key()),
            tabs: navigation_tabs,
            ..Default::default()
        },
    );
    project.panes.insert(
        right,
        SavedPane {
            tabs: others,
            active_tab_key: selected_other,
            ..Default::default()
        },
    );
    project.layout = Layout::navigation_beside(navigation, Layout::Pane(right));
    project.main_pane = Some(right);
    project.active_pane = active;
    project.locked_panes = Some(HashSet::from([navigation]));
    project.panels_initialized = true;
    project.normalize()
}
