//! The Dock menu: one item per open window of this app, labelled with its project.

use std::collections::{HashMap, HashSet};

use gpui::{Action, App, Global, MenuItem, WindowId};

const UNTITLED: &str = "Untitled project";

/// Raises one window. The id is GPUI's `WindowId` as a `u64`; a window closed since
/// the menu was built matches nothing, so a stale item does nothing.
#[derive(Clone, PartialEq, Action)]
#[action(namespace = riwork, no_json)]
pub struct ActivateWindow {
    pub window_id: u64,
}

/// The Dock's "New Window": Cmd+Shift+N on the frontmost window, but it also brings
/// the app forward and works when no window has focus to handle `NewProjectWindow`.
#[derive(Clone, PartialEq, Action)]
#[action(namespace = riwork, no_json)]
pub struct NewDockWindow;

/// What the menu needs to know about one window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DockWindow {
    pub id: u64,
    pub project: String,
    /// The window's worktree branch; only used to tell same-project windows apart.
    pub branch: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DockItem {
    pub window_id: u64,
    pub label: String,
    pub frontmost: bool,
}

struct Row {
    id: u64,
    name: String,
    branch: Option<String>,
}

fn clean(text: &str) -> Option<String> {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!text.is_empty()).then_some(text)
}

/// Menu items in a fixed order with unique labels. Windows sort alphabetically by
/// project name (case-insensitive), then by branch, then by window id. Alphabetical
/// order is what a list of 6 or 7 projects is scanned by, and GPUI window ids do not
/// follow creation order (a closed window's slot is reused). Windows that show the
/// same project get their branch appended, and any labels still equal are numbered
/// from the second one on: "Project", "Project (2)".
pub fn dock_items(windows: &[DockWindow], frontmost: Option<u64>) -> Vec<DockItem> {
    let mut rows = windows
        .iter()
        .map(|window| Row {
            id: window.id,
            name: clean(&window.project).unwrap_or_else(|| UNTITLED.to_owned()),
            branch: window.branch.as_deref().and_then(clean),
        })
        .collect::<Vec<_>>();
    rows.sort_by_cached_key(|row| {
        (
            row.name.to_lowercase(),
            row.name.clone(),
            row.branch.clone().unwrap_or_default().to_lowercase(),
            row.id,
        )
    });
    let mut projects: HashMap<&str, usize> = HashMap::new();
    for row in &rows {
        *projects.entry(&row.name).or_default() += 1;
    }
    let mut taken = HashSet::new();
    rows.iter()
        .map(|row| {
            let mut label = match &row.branch {
                Some(branch) if projects[row.name.as_str()] > 1 => {
                    format!("{} · {branch}", row.name)
                }
                _ => row.name.clone(),
            };
            let base = label.clone();
            let mut copy = 1;
            while taken.contains(&label) {
                copy += 1;
                label = format!("{base} ({copy})");
            }
            taken.insert(label.clone());
            DockItem {
                window_id: row.id,
                label,
                frontmost: frontmost == Some(row.id),
            }
        })
        .collect()
}

#[derive(Default)]
struct DockMenu {
    windows: Vec<DockWindow>,
    /// The window that was key last; it stays marked while another app is in front.
    frontmost: Option<u64>,
    shown: Vec<DockItem>,
}

impl Global for DockMenu {}

pub fn init(cx: &mut App) {
    cx.on_action(|action: &ActivateWindow, cx| activate_window(action.window_id, cx));
    cx.on_action(|_: &NewDockWindow, cx| new_window(cx));
}

/// A window reports its project on open, on a project switch and on every refresh.
/// Nothing is rebuilt unless what it reports changed.
pub fn update_window(window: DockWindow, cx: &mut App) {
    let menu = cx.try_global::<DockMenu>();
    if menu.is_some_and(|menu| menu.windows.contains(&window)) {
        return;
    }
    let menu = cx.default_global::<DockMenu>();
    match menu.windows.iter_mut().find(|known| known.id == window.id) {
        Some(known) => *known = window,
        None => menu.windows.push(window),
    }
    rebuild(cx);
}

pub fn set_frontmost(id: u64, cx: &mut App) {
    if cx
        .try_global::<DockMenu>()
        .is_some_and(|menu| menu.frontmost == Some(id))
    {
        return;
    }
    cx.default_global::<DockMenu>().frontmost = Some(id);
    rebuild(cx);
}

pub fn window_closed(id: WindowId, cx: &mut App) {
    let id = id.as_u64();
    let menu = cx.default_global::<DockMenu>();
    menu.windows.retain(|window| window.id != id);
    if menu.frontmost == Some(id) {
        menu.frontmost = None;
    }
    rebuild(cx);
}

fn rebuild(cx: &mut App) {
    let menu = cx.default_global::<DockMenu>();
    let items = dock_items(&menu.windows, menu.frontmost);
    if items == menu.shown {
        return;
    }
    let mut entries = items
        .iter()
        .map(|item| {
            MenuItem::action(
                item.label.clone(),
                ActivateWindow {
                    window_id: item.window_id,
                },
            )
            .checked(item.frontmost)
        })
        .collect::<Vec<_>>();
    if !entries.is_empty() {
        entries.push(MenuItem::separator());
    }
    entries.push(MenuItem::action("New Window", NewDockWindow));
    menu.shown = items;
    cx.set_dock_menu(entries);
}

fn activate_window(id: u64, cx: &mut App) {
    // The action runs inside the update of the window that was key, which may be
    // the one to raise, so the raising waits until that update is over.
    cx.defer(move |cx| {
        let Some(handle) = cx
            .windows()
            .into_iter()
            .find(|handle| handle.window_id().as_u64() == id)
        else {
            return;
        };
        if handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
        {
            cx.activate(true);
        }
    });
}

fn new_window(cx: &mut App) {
    cx.defer(|cx| {
        let front = cx.window_stack().unwrap_or_else(|| cx.windows());
        for handle in front {
            let opened = crate::with_workspace(handle, cx, |workspace, _, cx| {
                let project_id = workspace.project_id.clone();
                workspace.open_project_window(&project_id, cx);
            });
            if matches!(opened, Ok(Some(()))) {
                cx.activate(true);
                break;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(id: u64, project: &str, branch: Option<&str>) -> DockWindow {
        DockWindow {
            id,
            project: project.to_owned(),
            branch: branch.map(str::to_owned),
        }
    }

    fn labels(windows: &[DockWindow]) -> Vec<String> {
        dock_items(windows, None)
            .into_iter()
            .map(|item| item.label)
            .collect()
    }

    #[test]
    fn the_order_does_not_depend_on_the_input_order() {
        let mut windows = vec![
            window(4, "Site", Some("main")),
            window(2, "Site", Some("main")),
            window(9, "Site", Some("feature/x")),
            window(1, "api", None),
            window(3, "", None),
            window(8, "Ünï", None),
        ];
        let expected = dock_items(&windows, Some(2));
        for _ in 0..windows.len() {
            windows.rotate_left(1);
            assert_eq!(dock_items(&windows, Some(2)), expected);
        }
        windows.reverse();
        assert_eq!(dock_items(&windows, Some(2)), expected);
    }

    #[test]
    fn labels_stay_unique_even_when_a_project_is_named_like_a_number_suffix() {
        let windows = [
            window(1, "Site", None),
            window(2, "Site", None),
            window(3, "Site (2)", None),
            window(4, "Site · main", None),
            window(5, "Site", Some("main")),
            window(6, "Site", Some("main")),
        ];
        let labels = labels(&windows);
        let unique = labels.iter().collect::<HashSet<_>>();
        assert_eq!(unique.len(), windows.len(), "{labels:?}");
    }
}
