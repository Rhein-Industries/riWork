mod activity;
mod agent_hooks;
mod appearance_file;
mod appearance_sync;
mod cli;
mod cli_agents;
mod codex_accounts;
mod controls;
mod cua;
mod dock_menu;
mod file_explorer;
mod file_preview;
mod icons;
mod layouts;
mod mcp;
mod metal_layer;
mod notifications;
mod orca_import;
mod panels;
mod paths;
mod project_creator;
mod project_recency;
mod project_settings;
mod project_sort;
mod recency_file;
mod remote_cache;
mod remote_cli;
mod remote_hosts;
mod remote_prompt;
mod remote_service;
mod remote_tree;
mod runtime;
mod schedule_panel;
mod schedule_service;
mod schedules;
mod session_input;
mod session_keys;
mod session_reload;
mod session_viewport;
mod sessions;
mod settings;
mod sgr;
mod status_bar;
mod store;
mod symbols;
mod terminal_drop;
mod terminal_lifecycle;
mod terminal_links;
mod theme;
mod tooltip;
mod ui_text;
mod update;
mod usage;

use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet, HashSet},
    env,
    ops::Range,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use activity::{ActivityTracker, AgentState};
use file_explorer::{ExplorerRoot, FileExplorer, FileExplorerEvent, FilePreview};
use gpui::{
    AnyElement, App, Bounds, ClipboardItem, Context, Div, DragMoveEvent, Entity,
    EntityInputHandler, FocusHandle, Global, IntoElement, KeyBinding, KeyDownEvent, Menu, MenuItem,
    MouseButton, Pixels, Point, Render, Stateful, StatefulInteractiveElement, TitlebarOptions,
    UTF16Selection, Window, WindowBounds, WindowHandle, WindowOptions, actions, canvas, div, img,
    point, prelude::*, px, rgb, size,
};
use gpui_libghostty::{TerminalConfiguration, TerminalOptions, TerminalTheme};
use gpui_platform::application;
use icons::Icon;
use layouts::{
    Axis, DIVIDER_THICKNESS, Extent, Layout, LayoutStore, MIN_PANE_EXTENT, NAVIGATION_PANELS,
    OpenTabReuse, PaneFacts, PaneId, PanelKind, PreviewPlacement, PreviewReveal, PreviewTab,
    ProjectLayout, SavedPane, SavedTab, TabEdge, WindowSize,
};
use panels::{PanelAction, PanelData};
use project_creator::{ProjectCreationEvent, ProjectCreator};
use project_settings::{
    FolderEditor, FolderEditorEvent, ProjectSettingsEvent, ProjectSettingsPanel,
};
use remote_prompt::{PromptKind, RemotePrompt, RemotePromptEvent};
use remote_service::RemoteState;
use remote_tree::{NewShellKind, RemoteShell};
use sessions::{HarnessKind, SessionManager, SessionMetrics, ShellKind, ShellSession};
use settings::{CuaSetupState, Settings, SettingsEvent, SettingsPanel, SettingsStore};
use store::{Project, ProjectCodexAccount, SearchHit, State, Store};
use theme::{Appearance, Palette, ThemeChoice};
use tooltip::Look;
use usage::ProviderUsage;

gpui_libghostty::bind_gpui!(gpui);

actions!(
    riwork,
    [
        NewTab,
        SplitRight,
        SplitDown,
        CloseTab,
        ClosePane,
        NextTab,
        PreviousTab,
        ToggleSidebar,
        FocusSearch,
        OpenOrchestrator,
        OpenProjectOrchestrator,
        NewProjectWindow,
        OpenCodex,
        OpenClaude,
        OpenGrok,
        CreateProject,
        ToggleFocusMode,
        OpenSettings,
        OpenProjectSettings,
        OpenSchedules,
        OpenFiles,
        OpenPreview,
        GatherTabs,
        ApplyDefaultLayout,
        BiggerText,
        SmallerText,
        ActualSizeText,
        Quit
    ]
);

type TabId = u64;

#[derive(Clone, Copy)]
enum PaneMenuAction {
    Shell,
    Harness(HarnessKind, bool),
    Orchestrator(bool),
    View(PanelKind),
    Split(Axis),
    Close,
    Main,
    Gather,
    Lock,
    Focus,
}

/// What a row of the status bar's layout menu does when clicked.
#[derive(Clone, Copy)]
enum LayoutMenuAction {
    Default,
    Gather,
}

/// One row of the layout menu. A row with no action only says something.
struct LayoutMenuRow {
    id: &'static str,
    icon: Option<Icon>,
    label: String,
    shortcut: &'static str,
    /// A second, smaller line under the label.
    detail: Option<&'static str>,
    action: Option<LayoutMenuAction>,
}

const WINDOW_CONTROLS_WIDTH: f32 = 78.0;
const WINDOW_CONTROLS_HEIGHT: f32 = 28.0;
const WINDOW_CONTROLS_CONTENT_INSET: f32 = WINDOW_CONTROLS_WIDTH + 14.0;
const FOCUS_MAX_WIDTH: f32 = 1100.0;
const FOCUS_BOTTOM_MARGIN: f32 = 0.30;
const FOCUS_TOOLBAR_HEIGHT: f32 = 32.0;
/// Design sizes at 100 % interface text; they grow with it (`ui_text::space`).
const STATUS_BAR_HEIGHT: f32 = 22.0;
const PANE_HEADER_HEIGHT: f32 = 28.0;
/// A window restored fullscreen or zoomed is first drawn at its opening size and animates
/// to the real one. Sizes seen this soon after the first draw are that settling, not the
/// user resizing, so they must not rebalance the saved ratios.
const WINDOW_SETTLE: Duration = Duration::from_secs(2);
/// Attach attempts a tab makes on its own before it waits to be selected.
const ATTACH_RETRIES: u8 = 3;
const ATTACH_RETRY_DELAY: Duration = Duration::from_secs(2);

struct Tab {
    id: TabId,
    title: String,
    content: TabContent,
    /// When the tab last went off screen; `None` while it is shown.
    hidden_since: Option<Instant>,
}

enum TabContent {
    Shell {
        shell_id: String,
        worktree_id: Option<String>,
        /// The Ghostty surface, and with it the tmux client. Absent while the tab is
        /// released (hidden for a while) or has not been shown yet; the tmux session
        /// does not depend on it, and a new terminal attaches to the same session.
        terminal: Option<Entity<Terminal>>,
        /// Why the last attach failed. Kept so a tab that cannot attach is not
        /// retried on every frame; a few timed retries follow, and selecting the tab
        /// tries again.
        attach_error: Option<String>,
        attach_failures: u8,
    },
    /// A shell of another Mac's RiWork. The terminal runs `riwork-remote attach`, a bridge
    /// that carries the host's tmux client over the relay, so releasing a hidden terminal
    /// and attaching a new one work as they do for a local shell.
    RemoteShell {
        desktop_id: String,
        shell_id: String,
        terminal: Option<Entity<Terminal>>,
        attach_error: Option<String>,
        attach_failures: u8,
    },
    Panel(PanelKind),
}

/// The bookkeeping every terminal tab has, whatever it attaches to.
struct AttachState<'a> {
    terminal: &'a mut Option<Entity<Terminal>>,
    error: &'a mut Option<String>,
    failures: &'a mut u8,
}

/// What a tab without a terminal attaches to.
enum AttachTarget {
    Local(String),
    Remote(String, String),
}

impl TabContent {
    fn attach_state(&mut self) -> Option<AttachState<'_>> {
        match self {
            Self::Shell {
                terminal,
                attach_error,
                attach_failures,
                ..
            }
            | Self::RemoteShell {
                terminal,
                attach_error,
                attach_failures,
                ..
            } => Some(AttachState {
                terminal,
                error: attach_error,
                failures: attach_failures,
            }),
            Self::Panel(_) => None,
        }
    }
}

impl Tab {
    fn saved(&self) -> SavedTab {
        match &self.content {
            TabContent::Shell { shell_id, .. } => SavedTab::Shell {
                shell_id: shell_id.clone(),
            },
            TabContent::RemoteShell {
                desktop_id,
                shell_id,
                ..
            } => SavedTab::RemoteShell {
                desktop_id: desktop_id.clone(),
                shell_id: shell_id.clone(),
            },
            TabContent::Panel(panel) => SavedTab::Panel { panel: *panel },
        }
    }

    /// The host and shell id of a tab on another Mac's shell.
    fn remote(&self) -> Option<(&str, &str)> {
        match &self.content {
            TabContent::RemoteShell {
                desktop_id,
                shell_id,
                ..
            } => Some((desktop_id, shell_id)),
            _ => None,
        }
    }

    /// Whether the tab shows a terminal: a local shell, an agent, an editor or a remote shell.
    fn is_terminal(&self) -> bool {
        matches!(
            self.content,
            TabContent::Shell { .. } | TabContent::RemoteShell { .. }
        )
    }

    fn shell_id(&self) -> Option<&str> {
        match &self.content {
            TabContent::Shell { shell_id, .. } => Some(shell_id),
            _ => None,
        }
    }

    fn panel(&self) -> Option<PanelKind> {
        match &self.content {
            TabContent::Panel(panel) => Some(*panel),
            _ => None,
        }
    }

    fn terminal(&self) -> Option<&Entity<Terminal>> {
        match &self.content {
            TabContent::Shell { terminal, .. } | TabContent::RemoteShell { terminal, .. } => {
                terminal.as_ref()
            }
            TabContent::Panel(_) => None,
        }
    }

    /// Drop the terminal, which frees its Ghostty surface, the surface's threads and
    /// render targets, and detaches its tmux client. The session keeps running.
    fn release_terminal(&mut self, cx: &mut Context<Workspace>) -> bool {
        let Some(AttachState { terminal, .. }) = self.content.attach_state() else {
            return false;
        };
        let Some(terminal) = terminal.take() else {
            return false;
        };
        terminal.update(cx, |terminal, _| terminal.set_visible(false));
        true
    }

    fn set_visible(&self, visible: bool, cx: &mut Context<Workspace>) {
        if let Some(terminal) = self.terminal() {
            terminal.update(cx, |terminal, _| terminal.set_visible(visible));
        }
    }
}

struct Pane {
    tabs: Vec<Tab>,
    active: usize,
}

/// Add a tab for `panel` to the pane. With `select` it becomes the pane's selected tab;
/// without, the tab waits in the strip and what the pane shows is left as it is.
fn push_panel_tab(pane: &mut Pane, id: TabId, title: &str, panel: PanelKind, select: bool) {
    pane.tabs.push(new_panel_tab(id, title, panel));
    if select {
        pane.active = pane.tabs.len() - 1;
    }
}

fn new_panel_tab(id: TabId, title: &str, panel: PanelKind) -> Tab {
    Tab {
        id,
        title: title.to_owned(),
        content: TabContent::Panel(panel),
        hidden_since: None,
    }
}

/// Apply a placement for a panel to the window's panes: for a split, make the new pane and put
/// it in the layout (the pane split keeps its tabs and its selected one); then add the panel's
/// tab, selected in a new pane or where the placement says so. Returns the pane and whether the
/// tab is its selected tab. Terminals are not told they were hidden; see `place_panel`.
fn apply_panel_placement(
    layout: &mut Layout,
    panes: &mut BTreeMap<PaneId, Pane>,
    next_pane_id: &mut PaneId,
    next_tab_id: &mut TabId,
    panel: PanelKind,
    title: &str,
    placement: PreviewPlacement,
) -> Option<(PaneId, bool)> {
    let (pane_id, select) = match placement {
        PreviewPlacement::Tab { pane, activate } => (pane, activate),
        PreviewPlacement::Split {
            target,
            axis,
            ratio,
        } => {
            if !panes.contains_key(&target) {
                return None;
            }
            let new_pane = *next_pane_id;
            if !layout.split_with_ratio(target, axis, new_pane, false, ratio) {
                return None;
            }
            *next_pane_id += 1;
            panes.insert(
                new_pane,
                Pane {
                    tabs: Vec::new(),
                    active: 0,
                },
            );
            (new_pane, true)
        }
    };
    let pane = panes.get_mut(&pane_id)?;
    let id = *next_tab_id;
    *next_tab_id += 1;
    push_panel_tab(pane, id, title, panel, select);
    Some((pane_id, select))
}

/// Apply what the placement rules decided about a panel to the window's panes. The pane that
/// changed, and whether its selected tab did (so that terminals in it must be told which tabs
/// are hidden); `None` when nothing changed. A split also changes the layout.
fn apply_panel_reveal_to_panes(
    layout: &mut Layout,
    panes: &mut BTreeMap<PaneId, Pane>,
    next_pane_id: &mut PaneId,
    next_tab_id: &mut TabId,
    panel: PanelKind,
    title: &str,
    reveal: PreviewReveal,
) -> Option<(PaneId, bool)> {
    match reveal {
        PreviewReveal::Leave => None,
        PreviewReveal::Activate(pane_id) => {
            select_panel_tab(panes.get_mut(&pane_id)?, panel).then_some((pane_id, true))
        }
        PreviewReveal::Open(placement) => apply_panel_placement(
            layout,
            panes,
            next_pane_id,
            next_tab_id,
            panel,
            title,
            placement,
        ),
        PreviewReveal::Move { from, to, activate } => {
            let tab_id = panes
                .get(&from)?
                .tabs
                .iter()
                .find(|tab| tab.panel() == Some(panel))?
                .id;
            move_tab_between_panes(panes, from, tab_id, to, activate).then_some((to, activate))
        }
    }
}

/// Take the tab `tab_id` out of pane `from` and add it after the tabs of pane `to`, selected
/// there if `select`. `from` keeps showing the tab it showed, or a neighbour if it showed this
/// one. Whether the tab moved: it does not when a pane or the tab is missing.
fn move_tab_between_panes(
    panes: &mut BTreeMap<PaneId, Pane>,
    from: PaneId,
    tab_id: TabId,
    to: PaneId,
    select: bool,
) -> bool {
    if from == to || !panes.contains_key(&to) {
        return false;
    }
    let Some(source) = panes.get_mut(&from) else {
        return false;
    };
    let Some(index) = source.tabs.iter().position(|tab| tab.id == tab_id) else {
        return false;
    };
    let shown = source.tabs.get(source.active).map(|tab| tab.id);
    let tab = source.tabs.remove(index);
    source.active = shown
        .and_then(|id| source.tabs.iter().position(|tab| tab.id == id))
        .unwrap_or_else(|| index.min(source.tabs.len().saturating_sub(1)));
    let Some(destination) = panes.get_mut(&to) else {
        return false;
    };
    destination.tabs.push(tab);
    if select {
        destination.active = destination.tabs.len() - 1;
    }
    true
}

/// The pane that takes the tabs of `closing` when it closes: the main pane. Not when `closing`
/// is the main pane itself, whose tabs close with it as they always did, nor when it is
/// locked, which nothing is taken from or added to by someone else's decision.
fn aggregation_target(
    main: Option<PaneId>,
    closing: PaneId,
    closing_locked: bool,
) -> Option<PaneId> {
    main.filter(|main| *main != closing && !closing_locked)
}

/// Take pane `closed` out of the panes. Its tabs move to the end of the pane `main`, when there
/// is one to take them (see `aggregation_target`), in their order and without changing what
/// that pane shows; otherwise they are returned, to be closed.
fn close_pane_tabs(
    panes: &mut BTreeMap<PaneId, Pane>,
    closed: PaneId,
    main: Option<PaneId>,
) -> Vec<Tab> {
    let Some(mut pane) = panes.remove(&closed) else {
        return Vec::new();
    };
    match main.and_then(|main| panes.get_mut(&main)) {
        Some(main) => {
            main.tabs.append(&mut pane.tabs);
            Vec::new()
        }
        None => pane.tabs,
    }
}

/// What a gather did.
#[derive(Debug, Default, PartialEq, Eq)]
struct Gathered {
    /// Tabs moved into the main pane.
    moved: usize,
    /// Panes that were emptied and removed.
    removed: Vec<PaneId>,
    /// Panes that were emptied and stay, because their space would have gone to a locked pane.
    kept: usize,
}

/// Move every tab of every unlocked pane but `main` to the end of `main`, pane by pane in
/// layout order and tab by tab, and remove the panes this empties. What `main` shows does not
/// change. Locked panes are not touched: not their tabs, their selection or their place in the
/// tree. A pane whose space would go to a sibling holding a locked pane stays, empty, so that
/// the locked pane keeps its size.
fn gather_panes(
    layout: &mut Layout,
    panes: &mut BTreeMap<PaneId, Pane>,
    main: PaneId,
    locked: &dyn Fn(PaneId) -> bool,
) -> Gathered {
    let mut gathered = Gathered::default();
    if !panes.contains_key(&main) {
        return gathered;
    }
    for source in layout.gather_sources(main, locked) {
        let Some(from) = panes.get_mut(&source) else {
            continue;
        };
        let mut tabs = std::mem::take(&mut from.tabs);
        from.active = 0;
        gathered.moved += tabs.len();
        if let Some(main) = panes.get_mut(&main) {
            main.tabs.append(&mut tabs);
        }
        if layout.removal_resizes_locked(source, locked) {
            gathered.kept += 1;
        } else if let Some(rest) = layout.clone().without(source) {
            *layout = rest;
            panes.remove(&source);
            gathered.removed.push(source);
        }
    }
    gathered
}

/// The panes as a project's layout saves them: their tabs in order and the selected one.
fn saved_panes(panes: &BTreeMap<PaneId, Pane>) -> BTreeMap<PaneId, SavedPane> {
    panes
        .iter()
        .map(|(id, pane)| {
            (
                *id,
                SavedPane {
                    shell_ids: pane
                        .tabs
                        .iter()
                        .filter_map(|tab| tab.shell_id().map(str::to_owned))
                        .collect(),
                    active_shell_id: pane
                        .tabs
                        .get(pane.active)
                        .and_then(|tab| tab.shell_id().map(str::to_owned)),
                    tabs: pane.tabs.iter().map(Tab::saved).collect(),
                    active_tab_key: pane.tabs.get(pane.active).map(|tab| tab.saved().key()),
                    tab_edge: TabEdge::Top,
                },
            )
        })
        .collect()
}

/// What `default_layout_panes` did.
#[derive(Debug, PartialEq, Eq)]
struct DefaultLayout {
    /// The locked navigation pane on the left.
    navigation: PaneId,
    /// The pane on the right, which holds every other tab and is the main pane.
    main: PaneId,
    /// The pane that holds the tab that was selected in the selected pane.
    active_pane: PaneId,
    /// Tabs that are in another pane than they were.
    moved: usize,
    /// Navigation panels that were not open and now are.
    opened: Vec<PanelKind>,
}

/// Whether the window already is what `default_layout_panes` makes of it: exactly two panes, the
/// navigation panels in a locked pane on the left, in order and with no other tab, and the pane
/// on the right the unlocked main pane. The width of the two is not part of it: a navigation pane
/// the user dragged wider is not undone.
fn is_default_layout(
    layout: &Layout,
    panes: &BTreeMap<PaneId, Pane>,
    main: Option<PaneId>,
    locked: &dyn Fn(PaneId) -> bool,
) -> bool {
    let Some((navigation, rest)) = layout.two_panes_side_by_side() else {
        return false;
    };
    panes.len() == 2
        && main == Some(rest)
        && locked(navigation)
        && !locked(rest)
        && panes.get(&navigation).is_some_and(|pane| {
            pane.tabs
                .iter()
                .map(Tab::panel)
                .eq(NAVIGATION_PANELS.map(Some))
        })
}

/// Lay the window out as the default layout: a locked navigation pane on the left holding exactly
/// Projects, Files, Worktrees, Tasks and Shells, in that order and at the width a new project's
/// navigation pane starts with, and one pane on the right holding every other tab, which is the
/// main pane. The caller locks the first and makes the second the main pane; see `DefaultLayout`.
///
/// Nothing is closed: every tab of every pane, a locked one's included, is moved. Panes are read
/// in layout order and tabs in their own order, so the right pane has them in the order the
/// window showed them. A navigation panel is moved to the left pane wherever it was, and one that
/// was not open is opened there; a second tab for the same panel counts as any other tab. Both
/// panes keep the id of a pane that was there where there is one (the first pane other than
/// the main pane that holds a navigation panel, and the main pane), so that what a pane's id
/// names stays.
///
/// What is on screen stays as far as two panes allow: the tab selected in the selected pane is
/// selected in its new pane, and that pane is the selected one. Otherwise each pane shows what
/// the window showed in it, the main pane's first. Returns `None`, changing nothing, when the
/// window is laid out like this already.
fn default_layout_panes(
    layout: &mut Layout,
    panes: &mut BTreeMap<PaneId, Pane>,
    main: Option<PaneId>,
    active_pane: PaneId,
    locked: &dyn Fn(PaneId) -> bool,
    next_pane_id: &mut PaneId,
    next_tab_id: &mut TabId,
) -> Option<DefaultLayout> {
    if is_default_layout(layout, panes, main, locked) {
        return None;
    }
    // Every pane, in layout order; a pane the layout does not name would be a bug, but its tabs
    // are not left behind for it.
    let mut order = layout.pane_ids();
    let strays: Vec<PaneId> = panes
        .keys()
        .copied()
        .filter(|id| !order.contains(id))
        .collect();
    order.extend(strays);

    let holds_navigation = |id: &PaneId| {
        panes.get(id).is_some_and(|pane| {
            pane.tabs.iter().any(|tab| {
                tab.panel()
                    .is_some_and(|kind| NAVIGATION_PANELS.contains(&kind))
            })
        })
    };
    let navigation = order
        .iter()
        .copied()
        .find(|id| Some(*id) != main && holds_navigation(id));
    let right = main
        .filter(|id| panes.contains_key(id))
        .or_else(|| order.iter().copied().find(|id| Some(*id) != navigation));
    let mut fresh = || {
        let id = *next_pane_id;
        *next_pane_id += 1;
        id
    };
    let navigation = navigation.unwrap_or_else(&mut fresh);
    let right = right.unwrap_or_else(&mut fresh);

    let mut slots: [Option<Tab>; NAVIGATION_PANELS.len()] = Default::default();
    let mut others: Vec<Tab> = Vec::new();
    let mut moved = 0;
    // The tab each pane will show, by where it was selected.
    let mut navigation_shown: Option<usize> = None;
    let mut navigation_active: Option<usize> = None;
    let mut others_shown: Option<usize> = None;
    let mut others_in_main: Option<usize> = None;
    let mut others_active: Option<usize> = None;
    for id in order {
        let Some(Pane { tabs, active }) = panes.remove(&id) else {
            continue;
        };
        for (index, tab) in tabs.into_iter().enumerate() {
            let selected = index == active;
            let slot = tab
                .panel()
                .and_then(|kind| NAVIGATION_PANELS.iter().position(|panel| *panel == kind))
                .filter(|slot| slots[*slot].is_none());
            if let Some(slot) = slot {
                moved += usize::from(id != navigation);
                if selected {
                    navigation_shown.get_or_insert(slot);
                    if id == active_pane {
                        navigation_active = Some(slot);
                    }
                }
                slots[slot] = Some(tab);
            } else {
                moved += usize::from(id != right);
                if selected {
                    let position = others.len();
                    others_shown.get_or_insert(position);
                    if Some(id) == main {
                        others_in_main.get_or_insert(position);
                    }
                    if id == active_pane {
                        others_active = Some(position);
                    }
                }
                others.push(tab);
            }
        }
    }

    let mut opened = Vec::new();
    let navigation_tabs: Vec<Tab> = NAVIGATION_PANELS
        .iter()
        .zip(slots)
        .map(|(kind, slot)| {
            slot.unwrap_or_else(|| {
                opened.push(*kind);
                let id = *next_tab_id;
                *next_tab_id += 1;
                new_panel_tab(id, Workspace::panel_title(*kind), *kind)
            })
        })
        .collect();
    // The selected pane is the navigation pane only when its selected tab is one of its panels.
    let (active_pane, navigation_shown, others_shown) = if let Some(slot) = navigation_active {
        (navigation, slot, others_in_main.or(others_shown))
    } else {
        (
            right,
            navigation_shown.unwrap_or(0),
            others_active.or(others_in_main).or(others_shown),
        )
    };
    panes.insert(
        navigation,
        Pane {
            tabs: navigation_tabs,
            active: navigation_shown,
        },
    );
    panes.insert(
        right,
        Pane {
            tabs: others,
            active: others_shown.unwrap_or(0),
        },
    );
    *layout = Layout::navigation_beside(navigation, Layout::Pane(right));
    Some(DefaultLayout {
        navigation,
        main: right,
        active_pane,
        moved,
        opened,
    })
}

/// What the default layout says it did.
fn default_layout_notice(applied: &DefaultLayout, foreign_shells: usize) -> String {
    let mut notice = match applied.moved {
        0 => "Applied the default layout".to_owned(),
        1 => "Applied the default layout: moved 1 tab".to_owned(),
        count => format!("Applied the default layout: moved {count} tabs"),
    };
    if !applied.opened.is_empty() {
        let names: Vec<String> = applied
            .opened
            .iter()
            .map(|panel| panel_display_name(*panel))
            .collect();
        notice.push_str(if applied.moved == 0 {
            ": opened "
        } else {
            ", opened "
        });
        notice.push_str(&names.join(", "));
    }
    notice.push_str(". Nothing was closed");
    // Only a locked pane takes a shell of another project along when the project changes; one
    // that left it stays with that project's layout, and is no longer in this one's.
    match foreign_shells {
        0 => {}
        1 => notice.push_str(
            ". A shell of another project left the lock and is not part of this project's layout",
        ),
        count => notice.push_str(&format!(
            ". {count} shells of other projects left the lock and are not part of this project's layout"
        )),
    }
    notice
}

/// Said when the default layout is asked for and the window has it.
const DEFAULT_LAYOUT_PRESENT_HINT: &str = "The default layout is already in place";

/// The main pane as the layout menu names it: what it shows, and how many tabs it holds, or
/// `none` when the window has no main pane.
fn main_pane_summary(pane: Option<&Pane>) -> String {
    let Some(pane) = pane else {
        return "none".to_owned();
    };
    match (pane.tabs.get(pane.active), pane.tabs.len()) {
        (None, _) => "empty".to_owned(),
        (Some(tab), 1) => tab.title.clone(),
        (Some(tab), count) => format!("{} · {count} tabs", tab.title),
    }
}

/// The left edge of the layout menu, in window points. The menu is as wide as `menu` and ends
/// where the status bar item ends, as far as the window lets it: at least `MARGIN` from either
/// edge. An item that has not been laid out yet (zero width) puts the menu against the right edge.
fn layout_menu_left(item_right: f32, item_width: f32, menu: f32, window: f32) -> f32 {
    const MARGIN: f32 = 6.0;
    let right = if item_width > 0.0 {
        item_right
    } else {
        window - MARGIN
    };
    (right - menu).min(window - menu - MARGIN).max(MARGIN)
}

/// The pane, tab and visibility of the tab for `kind` among `panes`, if there is one.
fn panel_tab_in(panes: &BTreeMap<PaneId, Pane>, kind: PanelKind) -> Option<(PaneId, TabId, bool)> {
    panes.iter().find_map(|(pane_id, pane)| {
        pane.tabs
            .iter()
            .enumerate()
            .find(|(_, tab)| tab.panel() == Some(kind))
            .map(|(index, tab)| (*pane_id, tab.id, index == pane.active))
    })
}

/// Whether the pane's selected tab is a terminal: a shell, an agent or an editor.
fn pane_shows_shell_in(panes: &BTreeMap<PaneId, Pane>, pane_id: PaneId) -> bool {
    panes
        .get(&pane_id)
        .and_then(|pane| pane.tabs.get(pane.active))
        .is_some_and(Tab::is_terminal)
}

/// What a click on a link in pane `clicked` does about `panel`, the Preview or Files, when
/// there is no tree on screen to put it beside. See `Layout::plan_beside_reveal`.
fn plan_link_panel(
    layout: &Layout,
    panes: &BTreeMap<PaneId, Pane>,
    area: Option<Extent>,
    locked: &dyn Fn(PaneId) -> bool,
    panel: PanelKind,
    clicked: PaneId,
) -> PreviewReveal {
    let existing = panel_tab_in(panes, panel).map(|(pane, _, shown)| PreviewTab {
        pane,
        shown,
        behind_shell: pane_shows_shell_in(panes, pane),
    });
    let shows_shell = |id: PaneId| pane_shows_shell_in(panes, id);
    layout.plan_beside_reveal(
        existing,
        area,
        clicked,
        &PaneFacts {
            locked,
            shows_shell: &shows_shell,
        },
    )
}

/// The pane that takes what is opened (a new tab, a clicked shell, a preview) when the window has
/// a main pane. In focus mode only one pane is on screen, and a tab sent to another would open
/// out of sight, so everything stays where it is. A main pane that has gone counts for nothing.
fn routing_target(
    main: Option<PaneId>,
    panes: &BTreeMap<PaneId, Pane>,
    focus_mode: bool,
) -> Option<PaneId> {
    main.filter(|id| !focus_mode && panes.contains_key(id))
}

/// What choosing a file, or clicking a link in pane `clicked`, does about `panel` when the window
/// has the main pane `main`. Two panes keep their selected tab in front: the pane clicked in,
/// where the terminal keeps the keys, and, for the Preview, the pane with the tree on screen,
/// which the Preview would hide (see `layouts::plan_main_reveal`).
fn plan_main_panel(
    panes: &BTreeMap<PaneId, Pane>,
    main: PaneId,
    panel: PanelKind,
    clicked: Option<PaneId>,
) -> PreviewReveal {
    let existing = panel_tab_in(panes, panel).map(|(pane, _, shown)| PreviewTab {
        pane,
        shown,
        behind_shell: pane_shows_shell_in(panes, pane),
    });
    let tree = match panel_tab_in(panes, PanelKind::Files) {
        Some((pane, _, true)) if panel == PanelKind::Preview => Some(pane),
        _ => None,
    };
    let keep: Vec<PaneId> = clicked.into_iter().chain(tree).collect();
    layouts::plan_main_reveal(main, existing, &keep)
}

/// The pane whose tree a file's preview goes beside when the file was selected. A link is
/// clicked in a terminal; a tree that is a tab in that very pane is behind the terminal and not
/// on screen, so there is none to go beside.
fn tree_pane_for_reveal(explorer: Option<PaneId>, asked: bool, clicked: PaneId) -> Option<PaneId> {
    explorer.filter(|pane| !asked || *pane != clicked)
}

/// Make the tab for `panel` the selected tab of its pane. Whether that changed anything.
fn select_panel_tab(pane: &mut Pane, panel: PanelKind) -> bool {
    match pane.tabs.iter().position(|tab| tab.panel() == Some(panel)) {
        Some(index) if pane.active != index => {
            pane.active = index;
            true
        }
        _ => false,
    }
}

/// A panel's name in a sentence: `Files`, `Project settings`.
fn panel_display_name(panel: PanelKind) -> String {
    let title = Workspace::panel_title(panel);
    title
        .chars()
        .take(1)
        .chain(title.chars().skip(1).flat_map(char::to_lowercase))
        .collect()
}

/// What the notice says when a click asked for a panel that is only in a tab strip, not on
/// screen, so that the click does not seem to have done nothing. In the pane clicked in, the
/// selected tab is the terminal.
fn hidden_panel_notice(panel: PanelKind, in_clicked_pane: bool) -> String {
    let name = panel_display_name(panel);
    if in_clicked_pane {
        format!("{name} is in this pane's tab strip, behind the terminal.")
    } else {
        format!("{name} is in the tab strip of another pane.")
    }
}

#[derive(Clone)]
struct DraggedTab {
    pane_id: PaneId,
    tab_id: TabId,
    project_id: String,
    title: String,
}

impl Render for DraggedTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme::palette(cx);
        div()
            .px(ui_text::space(12.0))
            .py(ui_text::space(7.0))
            .bg(rgb(colors.panel_active))
            .border_1()
            .border_color(rgb(colors.cyan))
            .text_color(rgb(colors.text))
            .font_family(ui_text::ui_family())
            .text_size(ui_text::text(11.0))
            .child(self.title.clone())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DockSide {
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Clone)]
struct SplitResize {
    path: Vec<bool>,
    axis: Axis,
    bounds: Bounds<Pixels>,
}

#[derive(Default)]
struct AccountUsage {
    codex: BTreeMap<PathBuf, CodexUsageEntry>,
}

#[derive(Default)]
struct CodexUsageEntry {
    codex: Option<ProviderUsage>,
    codex_error: Option<String>,
    pending: bool,
    last_attempt: u64,
    /// Consecutive failed reads; spaces out retries of an account that never works.
    failures: u32,
}
impl Global for AccountUsage {}

struct Workspace {
    project_creator: Option<Entity<ProjectCreator>>,
    folder_editor: Option<Entity<FolderEditor>>,
    /// The modal for adding a host, pairing another Mac or naming a project on a host.
    remote_prompt: Option<Entity<RemotePrompt>>,
    /// Remote tabs whose bridge process has exited: the shell ended on its host, or the
    /// bridge could not stay. They keep their last screen until closed.
    remote_ended: HashSet<TabId>,
    /// The relay and name last typed to pair another Mac, so a second pairing starts there.
    remote_pair_defaults: (String, String),
    collapsed_project_folders: HashSet<String>,
    project_settings_panel: Option<Entity<ProjectSettingsPanel>>,
    schedule_panel: Option<Entity<schedule_panel::SchedulePanel>>,
    file_explorer: Option<Entity<FileExplorer>>,
    /// The Preview panel's view of `file_explorer`. Created and dropped with it.
    file_preview: Option<Entity<FilePreview>>,
    locked_panes: Option<HashSet<PaneId>>,
    /// The pane new tabs, clicked shells and opened previews go to. Saved with the layout.
    /// Read through `main_pane`, which ignores one whose pane has gone.
    main_pane: Option<PaneId>,
    carry_layout: Option<ProjectLayout>,
    layout: Layout,
    layout_ready: bool,
    /// The size of the area the split ratios were last laid out for. `None` until the
    /// first draw of a layout, and left alone in focus mode, which draws no split tree.
    pane_area: Option<Extent>,
    /// When the window first drew a layout; sizes within `WINDOW_SETTLE` of it are not resizes.
    first_drawn: Option<Instant>,
    /// A pending save of ratios rewritten by a window resize; one write per resize.
    layout_save: Option<gpui::Task<()>>,
    panes: BTreeMap<PaneId, Pane>,
    active_pane: PaneId,
    next_pane_id: PaneId,
    next_tab_id: TabId,
    layouts: LayoutStore,
    restore_layout: Option<ProjectLayout>,
    restore_focus: Option<(bool, bool)>,
    settings_store: SettingsStore,
    settings_panel: Entity<SettingsPanel>,
    settings: Settings,
    appearance: Appearance,
    window_size: Option<WindowSize>,
    /// Last size the user gave this window; it follows the window across project switches.
    window_size_user: Option<WindowSize>,
    /// A user resize waiting out its debounce; promoted to `window_size` on the next save.
    window_size_pending: Option<WindowSize>,
    /// The window's last settled ordinary frame, which is where a zoomed window returns to.
    normal_bounds: Option<Bounds<Pixels>>,
    window_size_save: Option<gpui::Task<()>>,
    store: Store,
    sessions: SessionManager,
    state: State,
    project_id: String,
    selected_worktree_id: Option<String>,
    selected_task_id: Option<String>,
    detached_shell_ids: HashSet<String>,
    shells: Vec<ShellSession>,
    shell_cwds: BTreeMap<String, PathBuf>,
    metrics: BTreeMap<String, SessionMetrics>,
    session_refresh_pending: bool,
    session_refresh_generation: u64,
    agent_activity: BTreeMap<String, AgentState>,
    activity_tracker: Option<ActivityTracker>,
    project_last_edits: BTreeMap<String, u64>,
    project_recency_pending: bool,
    project_recency_sampled_at: Option<Instant>,
    project_sort_menu_open: bool,
    claude_usage: BTreeMap<String, ProviderUsage>,
    /// Session usage of Grok tabs, by shell. Read only while shown; see
    /// `refresh_grok_usage`.
    grok_usage: BTreeMap<String, usage::GrokTabUsage>,
    grok_usage_pending: bool,
    refresh_count: u64,
    syncing_project_ids: HashSet<String>,
    sidebar_visible: bool,
    focus_mode: bool,
    focus_centered: bool,
    drop_target: Option<(PaneId, Option<DockSide>)>,
    panel_menu: Option<PaneId>,
    /// The layout menu of the status bar is open. Like a pane's menu it freezes the terminals
    /// as snapshots, so that it is drawn over them.
    layout_menu_open: bool,
    /// Where the status bar's layout item was last drawn, so that the menu opens above it and
    /// a click on the item itself closes the menu instead of closing and reopening it.
    layout_item_bounds: Rc<Cell<Bounds<Pixels>>>,
    resizing: Option<SplitResize>,
    tab_dragging: bool,
    terminal_snapshots: BTreeMap<TabId, Arc<gpui::RenderImage>>,
    /// ⌘-clicking URLs and files in local terminals.
    terminal_links: terminal_links::LinkState,
    terminal_drop: terminal_drop::DropState,
    /// The pending pass that releases hidden terminals; replaced whenever the set
    /// of hidden tabs changes.
    terminal_release: Option<gpui::Task<()>>,
    /// The pending retry of tabs whose terminal failed to attach.
    attach_retry: Option<gpui::Task<()>>,
    search_focused: bool,
    search: String,
    search_marked: Option<Range<usize>>,
    cwd: PathBuf,
    shell_name: String,
    notice: Option<String>,
    /// The title last given to the window. Setting a title makes AppKit update
    /// the window menu and lay out the title bar again, so an unchanged one is
    /// not set on every refresh.
    window_title: String,
    /// Whether the platform is presenting this window's frames. A window that is
    /// covered, minimized, on another Space or on a sleeping display shows
    /// nothing, so work that only feeds the screen waits for it.
    window_visible: bool,
    focus: FocusHandle,
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.1} GiB", bytes as f64 / (1024 * 1024 * 1024) as f64)
    } else if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024 * 1024) as f64)
    } else {
        format!("{} KiB", bytes / 1024)
    }
}

fn utf16_to_byte(text: &str, offset: usize) -> usize {
    let mut units = 0;
    for (byte, character) in text.char_indices() {
        if units >= offset {
            return byte;
        }
        units += character.len_utf16();
    }
    text.len()
}

fn file_explorer_root(
    state: &State,
    project_id: &str,
    selected_worktree_id: Option<&str>,
) -> Option<ExplorerRoot> {
    let project = state
        .projects
        .iter()
        .find(|project| project.id == project_id)?;
    if let Some(id) = selected_worktree_id {
        let worktree = state
            .worktrees
            .iter()
            .find(|worktree| worktree.id == id && worktree.project_id == project_id)?;
        Some(ExplorerRoot {
            path: worktree.path.clone(),
            label: format!("{} · {}", project.name, worktree.branch),
            worktree_id: Some(worktree.id.clone()),
        })
    } else {
        Some(ExplorerRoot {
            path: project.root.clone(),
            label: project.name.clone(),
            worktree_id: None,
        })
    }
}

enum FileEditorTarget {
    Existing(ShellSession),
    New,
}

fn file_editor_target(
    root: &ExplorerRoot,
    path: &Path,
    identity: file_preview::FileIdentity,
    project_id: &str,
    shells: &[ShellSession],
) -> Result<FileEditorTarget, String> {
    // A live editor owns this path already. Saving in Vim invalidates the
    // browser's cached size/mtime; no-follow path validation still applies.
    // The canonical path alone identifies the file: the project root (no
    // worktree id) and its primary worktree are one directory, while another
    // worktree resolves to a different path.
    let editor_path = file_preview::validated_live_editor_path(&root.path, path)?;
    if let Some(shell) = shells.iter().find(|shell| {
        shell.alive
            && shell.project_id.as_deref() == Some(project_id)
            && shell.editor_path.as_ref() == Some(&editor_path)
    }) {
        return Ok(FileEditorTarget::Existing(shell.clone()));
    }
    // Only a fresh launch must match the selection's original file identity.
    file_preview::validated_editor_path(&root.path, path, identity)?;
    Ok(FileEditorTarget::New)
}

/// The pane that inherits the space of `removed` when it closes, so focus can
/// stay next to where the user was working. Nearest to `removed` inside the
/// sibling subtree.
fn pane_inheriting_space(layout: &Layout, removed: PaneId) -> Option<PaneId> {
    let Layout::Split { first, second, .. } = layout else {
        return None;
    };
    match (first.as_ref(), second.as_ref()) {
        (Layout::Pane(id), sibling) if *id == removed => sibling.pane_ids().first().copied(),
        (sibling, Layout::Pane(id)) if *id == removed => sibling.pane_ids().last().copied(),
        _ => {
            pane_inheriting_space(first, removed).or_else(|| pane_inheriting_space(second, removed))
        }
    }
}

/// Left and top offsets of the window-edge docking targets. They stay clear of
/// the window controls island, which native full screen hides.
fn root_dock_insets(controls_visible: bool) -> (f32, f32) {
    if controls_visible {
        (WINDOW_CONTROLS_WIDTH + 2.0, WINDOW_CONTROLS_HEIGHT + 2.0)
    } else {
        // Match the right dock, which yields the corner to the top dock.
        (0.0, 12.0)
    }
}

/// Frees snapshot textures from the window's sprite atlas. `RenderImage` has no
/// `Drop`, so without this every snapshot leaks a surface-sized texture.
fn release_render_images(images: Vec<Arc<gpui::RenderImage>>, cx: &mut App) {
    if images.is_empty() {
        return;
    }
    // `App::drop_image` skips a window that is mid-update, and this runs from
    // event handlers and render. Deferred, the window is back in the app.
    cx.defer(move |cx| {
        for image in images {
            cx.drop_image(image, None);
        }
    });
}

/// Refresh ticks (2 s each) between redraws of a window that is otherwise
/// unchanged. Ages in the window ("just now", "3m ago") move by the minute.
const IDLE_REDRAW_TICKS: u64 = 5;

/// Give `tab` a new title. Returns whether it differs from the one it had.
fn retitle(tab: &mut Tab, title: String) -> bool {
    if tab.title == title {
        return false;
    }
    tab.title = title;
    true
}

/// Whether a hidden tab keeps its terminal however long it stays hidden.
///
/// A session that has ended, or one this window has not heard of yet, keeps its terminal:
/// attaching again could not bring its last screen back. A remote tab is live until its
/// bridge has exited; until then a new bridge makes the host's tmux repaint the screen.
fn keeps_terminal(tab: &Tab, remote_ended: &HashSet<TabId>, shells: &[ShellSession]) -> bool {
    if tab.remote().is_some() {
        return remote_ended.contains(&tab.id);
    }
    !tab.shell_id()
        .is_some_and(|id| shells.iter().any(|shell| shell.id == id && shell.alive))
}

/// The command a remote tab's terminal runs: the bridge to one shell of one host.
fn remote_attach_command(
    cli: &remote_hosts::RemoteCli,
    desktop_id: &str,
    shell_id: &str,
) -> String {
    std::iter::once(cli.binary().to_string_lossy().into_owned())
        .chain(remote_hosts::attach_args(desktop_id, shell_id))
        .map(|argument| sessions::quote_arg(&argument))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The title of a remote tab: the host's mark, then what the host's lists say about the
/// shell, or the start of its id before any list has loaded.
fn remote_title(desktop_id: &str, shell_id: &str, cx: &App) -> String {
    cx.try_global::<RemoteState>()
        .map(|state| state.tree().tab_title(desktop_id, shell_id))
        .unwrap_or_else(|| {
            remote_tree::remote_tab_title(
                remote_tree::short_id(desktop_id),
                remote_tree::short_id(shell_id),
            )
        })
}

fn window_title_for(project_name: &str) -> String {
    format!("RiWork · {project_name}")
}

fn project_recency_sources(state: &State) -> BTreeMap<String, Vec<PathBuf>> {
    let mut sources: BTreeMap<_, _> = state
        .projects
        .iter()
        .map(|project| {
            let mut roots = project.repository_roots.clone();
            roots.push(project.root.clone());
            (project.id.clone(), roots)
        })
        .collect();
    for worktree in &state.worktrees {
        if let Some(roots) = sources.get_mut(&worktree.project_id) {
            roots.push(worktree.path.clone());
        }
    }
    for roots in sources.values_mut() {
        roots.sort();
        roots.dedup();
    }
    sources
}

fn contextual_shell_title(
    title: String,
    shell: &ShellSession,
    project_id: &str,
    state: &State,
) -> String {
    match shell.project_id.as_deref().filter(|id| *id != project_id) {
        Some(id) => {
            let name = state
                .projects
                .iter()
                .find(|project| project.id == id)
                .map(|project| project.name.as_str())
                .unwrap_or(id);
            format!("{name} · {title}")
        }
        None => title,
    }
}

/// Account numbers are derived from frozen homes across all live shells in
/// each project, so separate windows and tab layouts agree on the same number.
fn codex_account_numbers(shells: &[ShellSession]) -> BTreeMap<String, usize> {
    let mut homes = BTreeMap::<&str, BTreeSet<&Path>>::new();
    for shell in shells
        .iter()
        .filter(|shell| shell.alive && shell.harness == Some(HarnessKind::Codex))
    {
        if let (Some(project_id), Some(home)) =
            (shell.project_id.as_deref(), shell.codex_home.as_deref())
        {
            homes.entry(project_id).or_default().insert(home);
        }
    }
    let mut numbers = BTreeMap::new();
    for shell in shells
        .iter()
        .filter(|shell| shell.alive && shell.harness == Some(HarnessKind::Codex))
    {
        let (Some(project_id), Some(home)) =
            (shell.project_id.as_deref(), shell.codex_home.as_deref())
        else {
            continue;
        };
        let Some(project_homes) = homes.get(project_id).filter(|homes| homes.len() > 1) else {
            continue;
        };
        if let Some(index) = project_homes
            .iter()
            .position(|candidate| *candidate == home)
        {
            numbers.insert(shell.id.clone(), index + 1);
        }
    }
    numbers
}

fn codex_tab_title(
    title: &str,
    shell: Option<&ShellSession>,
    numbers: &BTreeMap<String, usize>,
) -> String {
    match shell.and_then(|shell| numbers.get(&shell.id)) {
        Some(number) => format!("{title} · A{number}"),
        None => title.to_owned(),
    }
}

fn verified_shell_email(
    shell: &ShellSession,
    snapshot: Option<&codex_accounts::AccountsSnapshot>,
) -> Option<String> {
    shell.codex_account_email.clone().or_else(|| {
        snapshot?
            .accounts
            .iter()
            .find(|account| {
                shell.codex_account_id.as_deref() == Some(account.id.as_str())
                    && shell.codex_home.as_ref() == Some(&account.home)
            })?
            .email
            .clone()
    })
}

fn codex_session_status_label(
    shell: &ShellSession,
    numbers: &BTreeMap<String, usize>,
    snapshot: Option<&codex_accounts::AccountsSnapshot>,
) -> String {
    let email = verified_shell_email(shell, snapshot).unwrap_or_else(|| "Email unknown".to_owned());
    match numbers.get(&shell.id) {
        Some(number) => format!("CODEX A{number} · {email}"),
        None => format!("CODEX · {email}"),
    }
}

fn project_default_account_label(
    project: Option<&Project>,
    app_selected: Option<&str>,
    snapshot: Option<&codex_accounts::AccountsSnapshot>,
) -> String {
    let choice = project.map(|project| &project.codex_account);
    let (source, selected) = match choice {
        Some(ProjectCodexAccount::Saved(id)) => ("PROJECT", Some(id.as_str())),
        Some(ProjectCodexAccount::SystemDefault) => ("SYSTEM", None),
        _ => (
            "APP",
            app_selected.filter(|id| *id != codex_accounts::SYSTEM_DEFAULT_ID),
        ),
    };
    let account =
        selected.and_then(|id| snapshot?.accounts.iter().find(|account| account.id == id));
    let detail = match (selected, account) {
        (None, _) => "System default · email unknown".to_owned(),
        (Some(_), Some(account)) if account.available => account
            .email
            .clone()
            .unwrap_or_else(|| "Email unknown".to_owned()),
        (Some(_), Some(_)) => "Account unavailable".to_owned(),
        (Some(_), None) if snapshot.is_none() => "Email unknown".to_owned(),
        (Some(_), None) => "Account unavailable".to_owned(),
    };
    format!("DEFAULT ({source}) · {detail}")
}

/// Everything `Workspace::new` reads from disk. It is resolved before the
/// window opens, so a broken store or a vanished project folder is an error the
/// caller reports instead of a panic inside window creation.
struct WorkspaceStartup {
    store: Store,
    layouts: LayoutStore,
    settings_store: SettingsStore,
    sessions: SessionManager,
    state: State,
    /// The local project the window starts on, which also gives it its working directory.
    project: Project,
    /// The project on another Mac to open instead, as the last launch left it.
    remote: Option<String>,
}

impl WorkspaceStartup {
    fn prepare(startup_path: Option<PathBuf>, fallback_cwd: PathBuf) -> Result<Self, String> {
        sessions::prefetch_login_shell_dirs();
        Self::resolve(
            Store::open_default()?,
            LayoutStore::open_default()?,
            SettingsStore::open_default()?,
            SessionManager::open_default()?,
            startup_path,
            fallback_cwd,
        )
    }

    fn resolve(
        store: Store,
        layouts: LayoutStore,
        settings_store: SettingsStore,
        sessions: SessionManager,
        startup_path: Option<PathBuf>,
        fallback_cwd: PathBuf,
    ) -> Result<Self, String> {
        let initial = store.snapshot()?;
        let project = if let Some(path) = startup_path {
            let root = path.canonicalize().map_err(|error| {
                format!("Cannot open project folder {}: {error}", path.display())
            })?;
            match initial.projects.iter().find(|project| project.root == root) {
                Some(project) => project.clone(),
                None => store.add_project(root, None)?,
            }
        } else if let Some(project) = initial.active_project() {
            project.clone()
        } else {
            store.add_project(fallback_cwd, None)?
        };
        let state = store.snapshot()?;
        Ok(Self {
            store,
            layouts,
            settings_store,
            sessions,
            state,
            project,
            remote: None,
        })
    }
}

impl Workspace {
    fn new(
        startup: WorkspaceStartup,
        restore: Option<runtime::RuntimeWindow>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let WorkspaceStartup {
            store,
            layouts,
            settings_store,
            sessions,
            state,
            project,
            remote: remote_start,
        } = startup;
        let settings = cx.global::<Settings>().clone();
        let appearance = cx.global::<Appearance>().clone();
        let settings_panel = cx.new(|cx| SettingsPanel::new(settings_store.clone(), cx));
        cx.subscribe_in(
            &settings_panel,
            window,
            |workspace, _, event, window, cx| match event {
                SettingsEvent::OrcaImported => {
                    workspace.refresh_project_metadata(cx);
                    workspace.notice = Some("Orca import completed".to_owned());
                    cx.notify();
                }
                SettingsEvent::AddHost => {
                    workspace.begin_remote_prompt(PromptKind::AddHost, window, cx)
                }
                SettingsEvent::PairMac => {
                    let (relay, name) = workspace.remote_pair_defaults.clone();
                    let routes = paths::riwork_home()
                        .map(|home| remote_prompt::default_routes_file(&home))
                        .unwrap_or_default();
                    workspace.begin_remote_prompt(
                        PromptKind::PairMac {
                            relay,
                            name,
                            routes,
                        },
                        window,
                        cx,
                    );
                }
            },
        )
        .detach();
        let activity_tracker = ActivityTracker::at(sessions.state_home().to_path_buf());
        let window_title = window_title_for(&project.name);
        window.set_window_title(&window_title);
        let selected_worktree_id = state
            .worktrees_for(&project.id)
            .into_iter()
            .find(|worktree| worktree.is_primary)
            .map(|worktree| worktree.id.clone());
        let shell = env::var("SHELL")
            .ok()
            .filter(|value| Path::new(value).is_file())
            .unwrap_or_else(|| "/bin/zsh".to_owned());
        let shell_name = Path::new(&shell)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("shell")
            .to_owned();

        let mut panes = BTreeMap::new();
        panes.insert(
            1,
            Pane {
                tabs: Vec::new(),
                active: 0,
            },
        );
        // A window restored on, or launched with, a project on another Mac opens that one.
        let remote_start = remote_start_project(restore.as_ref(), remote_start.as_deref());
        let mut workspace = Self {
            project_creator: None,
            folder_editor: None,
            remote_prompt: None,
            remote_ended: HashSet::new(),
            remote_pair_defaults: (String::new(), String::new()),
            collapsed_project_folders: HashSet::new(),
            project_settings_panel: None,
            schedule_panel: None,
            file_explorer: None,
            file_preview: None,
            locked_panes: None,
            main_pane: None,
            carry_layout: None,
            layout: Layout::Pane(1),
            layout_ready: false,
            pane_area: None,
            first_drawn: None,
            layout_save: None,
            panes,
            active_pane: 1,
            next_pane_id: 2,
            next_tab_id: 1,
            layouts,
            restore_layout: restore.as_ref().and_then(|window| window.layout.clone()),
            restore_focus: restore
                .as_ref()
                .map(|window| (window.focus_mode, window.focus_centered)),
            settings_store,
            settings_panel,
            settings,
            appearance,
            window_size: None,
            window_size_user: None,
            window_size_pending: None,
            normal_bounds: normal_window_bounds(window),
            window_size_save: None,
            store,
            sessions,
            state,
            project_id: project.id,
            selected_worktree_id,
            selected_task_id: None,
            detached_shell_ids: HashSet::new(),
            shells: Vec::new(),
            shell_cwds: BTreeMap::new(),
            metrics: BTreeMap::new(),
            session_refresh_pending: false,
            session_refresh_generation: 0,
            agent_activity: BTreeMap::new(),
            activity_tracker: Some(activity_tracker),
            project_last_edits: BTreeMap::new(),
            project_recency_pending: false,
            project_recency_sampled_at: None,
            project_sort_menu_open: false,
            claude_usage: BTreeMap::new(),
            grok_usage: BTreeMap::new(),
            grok_usage_pending: false,
            refresh_count: 0,
            syncing_project_ids: HashSet::new(),
            sidebar_visible: true,
            focus_mode: false,
            focus_centered: true,
            drop_target: None,
            panel_menu: None,
            layout_menu_open: false,
            layout_item_bounds: Rc::new(Cell::new(Bounds::default())),
            resizing: None,
            tab_dragging: false,
            terminal_snapshots: BTreeMap::new(),
            terminal_links: terminal_links::LinkState::default(),
            terminal_drop: terminal_drop::DropState::default(),
            terminal_release: None,
            attach_retry: None,
            search_focused: false,
            search: String::new(),
            search_marked: None,
            cwd: project.root,
            shell_name,
            notice: None,
            window_title,
            window_visible: window.is_visible(),
            focus: cx.focus_handle(),
        };
        if let Some(key) = remote_start {
            workspace.project_id = key.clone();
            workspace.selected_worktree_id = None;
            remote_service::select(Some(key), cx);
            workspace.refresh_window_title(window, cx);
        }
        workspace.load_project(window, cx);
        workspace.refresh_project_recency(cx);
        if cua::CuaManager::open_default()
            .and_then(|manager| manager.driver_path())
            .is_err()
        {
            workspace.open_panel(PanelKind::Settings, workspace.active_pane, window, cx);
        }
        cx.observe_window_bounds(window, |workspace, window, cx| {
            // A hint stays where it opened, so a moved or resized window leaves it behind.
            tooltip::hide(cx);
            // Moving to a display of another scale also lands here, before the
            // terminals are resized for it.
            metal_layer::sync_terminal_scale(window);
            workspace.remember_window_size(window, true, cx);
        })
        .detach();
        cx.observe_global_in::<Settings>(window, |workspace, window, cx| {
            workspace.apply_settings(window, cx);
        })
        .detach();
        cx.observe_global_in::<Appearance>(window, |workspace, window, cx| {
            workspace.apply_settings(window, cx);
        })
        .detach();
        // Native follows macOS light and dark mode as it switches, not at the next poll.
        cx.observe_window_appearance(window, |_, _, cx| sync_appearance(cx))
            .detach();
        cx.observe_global::<settings::CodexAccountsState>(|_, cx| {
            request_codex_usage(false, cx);
            cx.notify();
        })
        .detach();
        cx.on_release(|workspace, _| workspace.save_layout())
            .detach();
        cx.observe_window_activation(window, |_, window, cx| {
            if window.is_window_active() {
                dock_menu::set_frontmost(window.window_handle().window_id().as_u64(), cx);
            } else {
                tooltip::hide(cx);
            }
        })
        .detach();
        if window.is_window_active() {
            dock_menu::set_frontmost(window.window_handle().window_id().as_u64(), cx);
        }
        workspace.announce_to_dock(window, cx);
        request_codex_usage(false, cx);
        for home in workspace
            .shells
            .iter()
            .filter(|shell| shell.alive && shell.harness == Some(HarnessKind::Codex))
            .filter_map(|shell| shell.codex_home.clone())
            .collect::<HashSet<_>>()
        {
            request_codex_usage_at(home, false, cx);
        }
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                if this
                    .update_in(cx, |workspace, window, cx| workspace.refresh(window, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        workspace
    }

    fn terminal_options(
        command: String,
        cwd: PathBuf,
        theme: Option<TerminalTheme>,
    ) -> TerminalOptions {
        let mut options = TerminalOptions::new(command, cwd);
        options.quiet_login = true;
        options.configuration = theme.map_or(
            TerminalConfiguration::UserDefault,
            TerminalConfiguration::UserDefaultWithOverride,
        );
        options
    }

    fn terminal_theme(settings: &Settings, appearance: &Appearance) -> Option<TerminalTheme> {
        appearance.terminal_override(settings.terminal_colors_forced())
    }

    fn spawn_tab(
        &mut self,
        pane_id: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        self.local_only(cx)?;
        let worktree_id = self.selected_worktree_id.clone();
        let cwd = worktree_id
            .as_ref()
            .and_then(|id| {
                self.state
                    .worktrees
                    .iter()
                    .find(|worktree| &worktree.id == id)
            })
            .map(|worktree| worktree.path.clone())
            .unwrap_or_else(|| self.cwd.clone());
        let shell = self
            .sessions
            .create(self.project_id.clone(), worktree_id, cwd, None)?;
        let result = self.attach_session(pane_id, shell, window, cx);
        if let Ok(shells) = self.sessions.list() {
            self.shells = shells;
        }
        self.refresh_agent_activity(cx);
        result
    }

    fn refresh_agent_activity(&mut self, cx: &mut Context<Self>) {
        if let Some(mut tracker) = self.activity_tracker.take() {
            let shells = self.shells.clone();
            let home = self.sessions.state_home().to_path_buf();
            let work = cx.background_executor().spawn(async move {
                let activity = tracker.sample_states(&shells, activity::unix_now());
                for completion in tracker.take_completions() {
                    if let Err(error) = notifications::record_completion(
                        &home,
                        &completion.shell_id,
                        &completion.event_id,
                        HarnessKind::Codex,
                    ) {
                        eprintln!("riwork notifications: {error}");
                    }
                }
                (tracker, activity)
            });
            cx.spawn(async move |this, cx| {
                let (tracker, activity) = work.await;
                let _ = this.update(cx, |workspace, cx| {
                    workspace.activity_tracker = Some(tracker);
                    if workspace.agent_activity != activity {
                        workspace.agent_activity = activity;
                        cx.notify();
                    }
                });
            })
            .detach();
        }
    }

    fn refresh_project_recency(&mut self, cx: &mut Context<Self>) {
        if self.project_recency_pending
            || self
                .project_recency_sampled_at
                .is_some_and(|sampled| sampled.elapsed() < project_recency::ASK_EVERY)
        {
            return;
        }
        self.project_recency_pending = true;
        self.project_recency_sampled_at = Some(Instant::now());
        let state = self.state.clone();
        let sources = project_recency_sources(&state);
        let home = self.sessions.state_home().to_path_buf();
        let work = cx.background_executor().spawn(async move {
            // One scan at a time for the whole process: the other windows read
            // what it leaves in the shared cache.
            let edits = project_recency::scan(&state);
            // `riwork project list --json`, and so the phone, sort by these
            // dates, and only this process knows them.
            let known = state.projects.iter().map(|project| project.id.as_str());
            if let Err(error) = recency_file::publish(&home, &edits, known, activity::unix_now()) {
                eprintln!("riwork project recency: {error}");
            }
            (sources, edits)
        });
        cx.spawn(async move |this, cx| {
            let (sources, edits) = work.await;
            let _ = this.update(cx, |workspace, cx| {
                workspace.project_recency_pending = false;
                if project_recency_sources(&workspace.state) == sources {
                    workspace.project_last_edits = edits;
                } else {
                    // A project or worktree moved while scanning; sample its new roots.
                    workspace.project_recency_sampled_at = None;
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// A terminal attached to `shell`'s tmux session. New tabs, tabs coming back from
    /// release and window restores all start theirs here, so size, theme and
    /// environment cannot differ between them.
    fn spawn_terminal(
        &self,
        shell: &ShellSession,
        focus_on_spawn: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Entity<Terminal>, String> {
        // A restoring app leaves these where a hang in the terminal library
        // would otherwise leave no trace (see `runtime::RestoreWatchdog`).
        runtime::note_attach_started(&shell.id, &shell.cwd);
        let spawned = self.sessions.attach_command(&shell.id).and_then(|command| {
            let mut options = Self::terminal_options(
                command,
                shell.cwd.clone(),
                Self::terminal_theme(&self.settings, &self.appearance),
            );
            options.focus_on_spawn = focus_on_spawn;
            Terminal::spawn(options, window, cx)
        });
        runtime::note_attach_finished(&shell.id, spawned.is_ok());
        spawned
    }

    /// A terminal running the bridge to `shell_id` on the paired host `desktop_id`. The
    /// bridge is the Ghostty child; the host's tmux client travels through it, so Ghostty
    /// draws what it would draw for a local tmux client.
    fn spawn_remote_terminal(
        &self,
        desktop_id: &str,
        shell_id: &str,
        focus_on_spawn: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Entity<Terminal>, String> {
        let command =
            remote_attach_command(&remote_hosts::RemoteCli::locate()?, desktop_id, shell_id);
        let attach_id = format!("remote:{desktop_id}:{shell_id}");
        runtime::note_attach_started(&attach_id, &self.cwd);
        let mut options = Self::terminal_options(
            command,
            self.cwd.clone(),
            Self::terminal_theme(&self.settings, &self.appearance),
        );
        options.focus_on_spawn = focus_on_spawn;
        let spawned = Terminal::spawn(options, window, cx);
        runtime::note_attach_finished(&attach_id, spawned.is_ok());
        spawned
    }

    /// A tab on a shell of another Mac, titled from what the host's lists say about it.
    fn remote_tab(
        &mut self,
        desktop_id: String,
        shell_id: String,
        terminal: Option<Entity<Terminal>>,
        cx: &App,
    ) -> Tab {
        let tab_id = self.next_tab_id;
        self.next_tab_id += 1;
        Tab {
            id: tab_id,
            title: remote_title(&desktop_id, &shell_id, cx),
            content: TabContent::RemoteShell {
                desktop_id,
                shell_id,
                terminal,
                attach_error: None,
                attach_failures: 0,
            },
            hidden_since: None,
        }
    }

    fn shell_tab(&mut self, shell: ShellSession, terminal: Option<Entity<Terminal>>) -> Tab {
        claim_shell(&shell.id);
        let tab_id = self.next_tab_id;
        self.next_tab_id += 1;
        let worktree_name = shell
            .worktree_id
            .as_ref()
            .and_then(|id| {
                self.state
                    .worktrees
                    .iter()
                    .find(|worktree| &worktree.id == id)
            })
            .map(|worktree| worktree.branch.as_str())
            .unwrap_or("root");
        let title = if let Some(path) = &shell.editor_path {
            format!(
                "VIM · {}",
                path.file_name()
                    .map(|name| name.to_string_lossy())
                    .unwrap_or_default()
            )
        } else if shell.kind == ShellKind::Orchestrator {
            orchestrator_tab_title(&shell)
        } else {
            format!(
                "{} {:02} · {}",
                shell.harness.map(harness_name).unwrap_or(&self.shell_name),
                tab_id,
                worktree_name
            )
        };
        let title = contextual_shell_title(title, &shell, &self.project_id, &self.state);
        Tab {
            id: tab_id,
            title,
            content: TabContent::Shell {
                shell_id: shell.id,
                worktree_id: shell.worktree_id,
                terminal,
                attach_error: None,
                attach_failures: 0,
            },
            hidden_since: None,
        }
    }

    fn attach_session(
        &mut self,
        pane_id: PaneId,
        shell: ShellSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let terminal = self.spawn_terminal(&shell, true, window, cx)?;
        let tab = self.shell_tab(shell, Some(terminal));
        self.place_new_tab(pane_id, tab, cx)
    }

    /// Add a tab whose terminal has just taken focus, and make it the pane's selected tab.
    fn place_new_tab(
        &mut self,
        pane_id: PaneId,
        tab: Tab,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        // The new terminal takes focus on spawn.
        self.search_focused = false;
        self.search_marked = None;
        let pane = self
            .panes
            .get_mut(&pane_id)
            .ok_or_else(|| format!("Pane {pane_id} no longer exists"))?;
        for existing in &pane.tabs {
            existing.set_visible(false, cx);
        }
        pane.tabs.push(tab);
        pane.active = pane.tabs.len() - 1;
        self.active_pane = pane_id;
        self.notice = None;
        self.session_refresh_generation = self.session_refresh_generation.wrapping_add(1);
        if self.layout_ready {
            self.remember_active_worktree(cx);
        }
        cx.notify();
        Ok(())
    }

    /// Add a shell's tab without a terminal. Restoring a window opens every saved
    /// tab, but only each pane's active one is ever seen, so the others wait for a
    /// terminal until they are shown.
    fn restore_shell_tab(&mut self, pane_id: PaneId, shell: ShellSession) {
        let tab = self.shell_tab(shell, None);
        let Some(pane) = self.panes.get_mut(&pane_id) else {
            return;
        };
        pane.tabs.push(tab);
        pane.active = pane.tabs.len() - 1;
        self.active_pane = pane_id;
        self.session_refresh_generation = self.session_refresh_generation.wrapping_add(1);
    }

    /// Add a remote shell's tab without a terminal, as `restore_shell_tab` does for a
    /// local one: it attaches when the tab is first shown.
    fn restore_remote_tab(
        &mut self,
        pane_id: PaneId,
        desktop_id: String,
        shell_id: String,
        cx: &App,
    ) {
        let tab = self.remote_tab(desktop_id, shell_id, None, cx);
        let Some(pane) = self.panes.get_mut(&pane_id) else {
            return;
        };
        pane.tabs.push(tab);
        pane.active = pane.tabs.len() - 1;
        self.active_pane = pane_id;
    }

    /// `changed_only` ignores a size the window already settled at, so the frame it
    /// opened with (possibly clamped to the display) or returned to after zooming is
    /// not mistaken for the user's choice. Turning the setting on passes false to
    /// remember the current size right away.
    fn remember_window_size(
        &mut self,
        window: &Window,
        changed_only: bool,
        cx: &mut Context<Self>,
    ) {
        // Fullscreen and zoomed frames must not replace the normal window size.
        let Some(bounds) = normal_window_bounds(window) else {
            self.window_size_pending = None;
            self.window_size_save = None;
            return;
        };
        if changed_only
            && self
                .normal_bounds
                .is_some_and(|normal| normal.size == bounds.size)
        {
            // Only moved, or back at the settled size.
            self.normal_bounds = Some(bounds);
            self.window_size_pending = None;
            self.window_size_save = None;
            return;
        }
        self.window_size_pending = self
            .settings
            .remember_window_size
            .then(|| WindowSize::new(bounds.size.width.as_f32(), bounds.size.height.as_f32()))
            .flatten();
        // Avoid writing for every frame during a drag. Release also saves the final size.
        self.window_size_save = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(250))
                .await;
            let _ = this.update_in(cx, |workspace, window, _| {
                // Zooming animates through ordinary-looking frames; only a window that
                // is still ordinary now has settled on a size.
                let Some(bounds) = normal_window_bounds(window) else {
                    workspace.window_size_pending = None;
                    return;
                };
                workspace.normal_bounds = Some(bounds);
                if workspace.window_size_pending.is_some() {
                    workspace.save_layout();
                }
            });
        }));
    }

    fn apply_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let settings = cx.global::<Settings>().clone();
        if settings.project_order != self.settings.project_order {
            self.project_sort_menu_open = false;
        }
        if cx.global::<Appearance>().selected != settings.theme {
            sync_appearance(cx);
        }
        let appearance = cx.global::<Appearance>().clone();
        let previous_theme = Self::terminal_theme(&self.settings, &self.appearance);
        let next_theme = Self::terminal_theme(&settings, &appearance);
        let colors_changed = previous_theme != next_theme
            || (next_theme.is_none() && self.appearance.ghostty != appearance.ghostty);
        let restore_terminal_focus = next_theme.is_none()
            && !self.modal_open()
            && !self.search_focused
            && self.panel_menu.is_none()
            && !self.tab_dragging
            && self
                .panes
                .get(&self.active_pane)
                .and_then(|pane| pane.tabs.get(pane.active))
                .is_some_and(Tab::is_terminal);
        let remember_changed = settings.remember_window_size != self.settings.remember_window_size;
        self.settings = settings;
        self.appearance = appearance;
        if remember_changed && self.settings.remember_window_size {
            self.remember_window_size(window, false, cx);
        }
        if colors_changed {
            let mut error = None;
            for pane in self.panes.values_mut() {
                for tab in &mut pane.tabs {
                    let target = match &tab.content {
                        TabContent::Shell { shell_id, .. } => AttachTarget::Local(shell_id.clone()),
                        TabContent::RemoteShell {
                            desktop_id,
                            shell_id,
                            ..
                        } => AttachTarget::Remote(desktop_id.clone(), shell_id.clone()),
                        TabContent::Panel(_) => continue,
                    };
                    let Some(AttachState { terminal, .. }) = tab.content.attach_state() else {
                        continue;
                    };
                    // A released tab has no client to update; it attaches with the
                    // new colors when it is shown again.
                    let Some(terminal) = terminal else {
                        continue;
                    };
                    if let Some(theme) = next_theme {
                        // Ghostty can apply palette overrides without replacing the client.
                        if let Err(message) =
                            terminal.update(cx, |terminal, _| terminal.update_theme(theme))
                        {
                            error = Some(message);
                        }
                        continue;
                    }
                    // Returning to the native Ghostty config removes every override.
                    // Only display clients reconnect; tmux shells, harnesses and remote
                    // shells stay alive.
                    let ended = self.remote_ended.contains(&tab.id);
                    let replacement = (|| {
                        let (command, cwd) = match &target {
                            AttachTarget::Local(shell_id) => {
                                let shell = self.sessions.get(shell_id)?;
                                if !shell.alive {
                                    return Ok(None);
                                }
                                (self.sessions.attach_command(shell_id)?, shell.cwd)
                            }
                            // A bridge that has exited would only exit again.
                            AttachTarget::Remote(..) if ended => return Ok(None),
                            AttachTarget::Remote(desktop_id, shell_id) => (
                                remote_attach_command(
                                    &remote_hosts::RemoteCli::locate()?,
                                    desktop_id,
                                    shell_id,
                                ),
                                self.cwd.clone(),
                            ),
                        };
                        let mut options = Self::terminal_options(command, cwd, None);
                        options.focus_on_spawn = false;
                        Terminal::spawn(options, window, cx).map(Some)
                    })();
                    match replacement {
                        Ok(Some(replacement)) => {
                            terminal.update(cx, |terminal, _| terminal.set_visible(false));
                            replacement.update(cx, |terminal, _| terminal.set_visible(false));
                            *terminal = replacement;
                        }
                        Ok(None) => {}
                        Err(message) => error = Some(message),
                    }
                }
            }
            self.release_snapshots(cx);
            if let Some(error) = error {
                self.notice = Some(error);
            }
            if restore_terminal_focus {
                self.focus_active(window, cx);
            }
        }
        cx.notify();
    }

    /// A panel's name in sentence case, as Native writes labels.
    fn panel_label(panel: PanelKind) -> &'static str {
        match panel {
            PanelKind::Projects => "Projects",
            PanelKind::Worktrees => "Worktrees",
            PanelKind::Files => "Files",
            PanelKind::Preview => "Preview",
            PanelKind::Tasks => "Tasks",
            PanelKind::Shells => "Shells",
            PanelKind::Usage => "Usage",
            PanelKind::Settings => "Settings",
            PanelKind::Schedules => "Schedules",
            PanelKind::ProjectSettings => "Project settings",
        }
    }

    fn panel_title(panel: PanelKind) -> &'static str {
        match panel {
            PanelKind::Projects => "PROJECTS",
            PanelKind::Worktrees => "WORKTREES",
            PanelKind::Files => "FILES",
            PanelKind::Preview => "PREVIEW",
            PanelKind::Tasks => "TASKS",
            PanelKind::Shells => "SHELLS",
            PanelKind::Usage => "USAGE",
            PanelKind::Settings => "SETTINGS",
            PanelKind::Schedules => "SCHEDULES",
            PanelKind::ProjectSettings => "PROJECT SETTINGS",
        }
    }

    fn attach_panel(&mut self, pane_id: PaneId, panel: PanelKind, cx: &mut Context<Self>) {
        self.attach_panel_as(pane_id, panel, true, cx);
    }

    /// Make what a tab for `panel` shows: the window's schedules, project settings or file
    /// explorer, for the panels that have one. Opening the tab needs it; the tab itself is made
    /// by `attach_panel_as` or, for the default layout, by `default_layout_panes`.
    fn prepare_panel(&mut self, panel: PanelKind, cx: &mut Context<Self>) {
        if panel == PanelKind::Schedules && self.schedule_panel.is_none() && !self.is_remote() {
            let store = self.store.clone();
            let sessions = self.sessions.clone();
            let project = self.project_id.clone();
            let workspace = self.selected_worktree_id.clone();
            self.schedule_panel = Some(cx.new(|cx| {
                schedule_panel::SchedulePanel::new(store, sessions, project, workspace, cx)
            }));
        }
        if panel == PanelKind::ProjectSettings {
            self.ensure_project_settings(cx);
        }
        if matches!(panel, PanelKind::Files | PanelKind::Preview) {
            self.ensure_file_explorer(cx);
        }
    }

    /// Add a tab for `panel` to the pane. With `select` it becomes the pane's selected tab;
    /// without, the tab waits in the strip and what the pane shows is left as it is.
    fn attach_panel_as(
        &mut self,
        pane_id: PaneId,
        panel: PanelKind,
        select: bool,
        cx: &mut Context<Self>,
    ) {
        self.prepare_panel(panel, cx);
        if let Some(pane) = self.panes.get_mut(&pane_id) {
            if select {
                for tab in &pane.tabs {
                    tab.set_visible(false, cx);
                }
            }
            let id = self.next_tab_id;
            self.next_tab_id += 1;
            push_panel_tab(pane, id, Self::panel_title(panel), panel, select);
        }
    }

    fn add_navigation_pane(&mut self, cx: &mut Context<Self>) {
        let pane_id = self.next_pane_id;
        self.next_pane_id += 1;
        self.panes.insert(
            pane_id,
            Pane {
                tabs: Vec::new(),
                active: 0,
            },
        );
        for panel in NAVIGATION_PANELS {
            self.attach_panel(pane_id, panel, cx);
        }
        self.panes.get_mut(&pane_id).unwrap().active = 0;
        self.layout = Layout::navigation_beside(pane_id, self.layout.clone());
    }

    fn open_panel(
        &mut self,
        panel: PanelKind,
        pane_id: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.ensure_layout(window, cx) {
            return;
        }
        self.panel_menu = None;
        let existing = self.panes.iter().find_map(|(id, pane)| {
            pane.tabs
                .iter()
                .find(|tab| matches!(tab.content, TabContent::Panel(kind) if kind == panel))
                .map(|tab| (*id, tab.id))
        });
        if let Some((pane_id, tab_id)) = existing {
            self.select_tab(pane_id, tab_id, window, cx);
        } else {
            self.attach_panel(pane_id, panel, cx);
            self.active_pane = pane_id;
            self.focus_active(window, cx);
            self.save_layout();
            cx.notify();
        }
        if panel == PanelKind::Usage {
            // Show Grok's figures now, not on the next tick.
            self.refresh_grok_usage(false, cx);
        }
    }

    fn panel_action(&mut self, action: PanelAction, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            PanelAction::ToggleProjectSortMenu => {
                self.project_sort_menu_open = !self.project_sort_menu_open;
                if self.project_sort_menu_open {
                    self.panel_menu = None;
                    self.finish_tab_drag(cx);
                    self.focus.focus(window, cx);
                } else {
                    self.focus_active(window, cx);
                }
                cx.notify();
            }
            PanelAction::CloseProjectSortMenu => {
                self.project_sort_menu_open = false;
                self.focus_active(window, cx);
                cx.notify();
            }
            PanelAction::SetProjectOrder(order) => {
                self.project_sort_menu_open = false;
                match self
                    .settings_store
                    .update(|settings| settings.project_order = order)
                {
                    Ok(settings) => {
                        self.notice = None;
                        cx.set_global(settings);
                    }
                    Err(error) => self.notice = Some(error),
                }
                self.focus_active(window, cx);
                cx.notify();
            }
            PanelAction::CreateProject => self.begin_project_creation(window, cx),
            PanelAction::CreateFolder => self.begin_folder_edit(None, window, cx),
            PanelAction::CreateSubfolder(id) => {
                self.begin_folder_edit_in(None, Some(&id), window, cx)
            }
            PanelAction::EditFolder(id) => self.begin_folder_edit(Some(&id), window, cx),
            PanelAction::BeginProjectDrag => {
                self.panel_menu = None;
                self.project_sort_menu_open = false;
                self.search_focused = false;
                self.begin_tab_drag(cx);
            }
            PanelAction::MoveProject {
                project_id,
                folder_id,
            } => {
                match self
                    .store
                    .move_project_to_folder(&project_id, folder_id.as_deref())
                {
                    Ok(_) => {
                        self.notice = None;
                        self.refresh_project_metadata(cx);
                        self.expand_project_folder(folder_id.as_deref());
                    }
                    Err(error) => self.notice = Some(error),
                }
                self.finish_tab_drag(cx);
                cx.notify();
            }
            PanelAction::MoveFolder {
                folder_id,
                parent_id,
            } => {
                match self
                    .store
                    .move_project_folder(&folder_id, parent_id.as_deref())
                {
                    Ok(_) => {
                        self.notice = None;
                        self.refresh_project_metadata(cx);
                        self.expand_project_folder(parent_id.as_deref());
                    }
                    Err(error) => self.notice = Some(error),
                }
                self.finish_tab_drag(cx);
                cx.notify();
            }
            PanelAction::RemoveFolder(id) => {
                match self.store.remove_project_folder(&id) {
                    Ok(()) => {
                        self.collapsed_project_folders.remove(&id);
                        self.refresh_project_metadata(cx);
                    }
                    Err(error) => self.notice = Some(error),
                }
                cx.notify();
            }
            PanelAction::ToggleFolder(id) => {
                let host_folder = remote_tree::parse_folder_key(&id).is_some();
                if !self.collapsed_project_folders.remove(&id) {
                    self.collapsed_project_folders.insert(id);
                }
                if host_folder {
                    // Opening a host's folder lists its projects now, not at the next tick.
                    remote_service::tick(&self.remote_wants(), cx);
                }
                cx.notify();
            }
            PanelAction::ProjectSettings(id) => {
                self.activate_project(&id, true, window, cx);
                if self.project_id != id {
                    return;
                }
                let pane_id = self
                    .panes
                    .iter()
                    .rev()
                    .find(|(_, pane)| pane.tabs.iter().any(Tab::is_terminal))
                    .map(|(id, _)| *id)
                    .unwrap_or(self.active_pane);
                self.open_panel(PanelKind::ProjectSettings, pane_id, window, cx);
            }
            PanelAction::ToggleProjectNotifications(id) => {
                let enabled = match self.store.snapshot().and_then(|state| {
                    state
                        .project(&id)
                        .map(|project| !project.notify_on_agent_done)
                }) {
                    Ok(enabled) => enabled,
                    Err(error) => {
                        self.notice = Some(error);
                        cx.notify();
                        return;
                    }
                };
                match self.store.set_project_notifications(&id, enabled) {
                    Ok(project) => {
                        if enabled {
                            notifications::show_enabled(&project, cx);
                        } else if let Err(error) =
                            notifications::cancel_pending(self.sessions.state_home(), &id)
                        {
                            self.notice = Some(error);
                        }
                        self.refresh_project_metadata(cx);
                        cx.refresh_windows();
                    }
                    Err(error) => {
                        self.notice = Some(error);
                        cx.notify();
                    }
                }
            }
            PanelAction::Project(id) => self.activate_project(&id, true, window, cx),
            PanelAction::OpenProject(id) => self.open_project_window(&id, cx),
            PanelAction::Worktree(id) => self.select_worktree(&id, window, cx),
            PanelAction::Task(id) => self.select_task(&id, window, cx),
            PanelAction::Shell(id) => self.show_shell(&id, window, cx),
            PanelAction::Search => {
                self.search_focused = true;
                self.focus.focus(window, cx);
                cx.notify();
            }
            PanelAction::Remote(action) => self.remote_action(action, window, cx),
        }
    }

    fn refresh_project_metadata(&mut self, cx: &mut Context<Self>) {
        match self.store.snapshot() {
            Ok(state) => self.state = state,
            Err(error) => self.notice = Some(error),
        }
        self.project_recency_sampled_at = None;
        self.refresh_project_recency(cx);
        if let Some(panel) = &self.project_settings_panel {
            panel.update(cx, |panel, cx| panel.refresh_folders(cx));
        }
        cx.notify();
    }

    fn ensure_project_settings(&mut self, cx: &mut Context<Self>) {
        if self.project_settings_panel.is_some() {
            return;
        }
        let Ok(project) = self.state.project(&self.project_id).cloned() else {
            return;
        };
        let store = self.store.clone();
        let panel = cx.new(|cx| ProjectSettingsPanel::new(store, project, cx));
        cx.subscribe(&panel, |workspace, _, event, cx| {
            if let ProjectSettingsEvent::Saved(project) = event {
                workspace.notice = Some(format!("Saved {}", project.name));
            }
            workspace.refresh_project_metadata(cx);
        })
        .detach();
        self.project_settings_panel = Some(panel);
    }

    fn ensure_file_explorer(&mut self, cx: &mut Context<Self>) {
        // Another Mac's files are not browsable from here, and this Mac's would be the wrong ones.
        if self.is_remote() {
            return;
        }
        if self.file_explorer.is_none() {
            let panel = cx.new(FileExplorer::new);
            cx.subscribe(&panel, |workspace, _, event, cx| match event {
                FileExplorerEvent::Edit {
                    root,
                    path,
                    identity,
                } => {
                    let view = cx.weak_entity();
                    let entity_id = cx.entity_id();
                    let (root, path, identity) = (root.clone(), path.clone(), *identity);
                    cx.defer(move |app| {
                        app.with_window(entity_id, |window, app| {
                            let _ = view.update(app, |workspace, cx| {
                                workspace.open_file_editor(root, path, identity, None, window, cx);
                            });
                        });
                    });
                }
                FileExplorerEvent::Open(path) => cx.open_with_system(path),
                FileExplorerEvent::Reveal(path) => cx.reveal_path(path),
                FileExplorerEvent::CopyRelativePath(path) => {
                    cx.write_to_clipboard(ClipboardItem::new_string(
                        path.to_string_lossy().into_owned(),
                    ));
                    workspace.notice = Some(format!("Copied {}", path.display()));
                    cx.notify();
                }
                FileExplorerEvent::Selected => workspace.reveal_preview(false, cx),
                FileExplorerEvent::Revealed => workspace.reveal_preview(true, cx),
                FileExplorerEvent::NotListed(path) => workspace.open_unlisted(path.clone(), cx),
            })
            .detach();
            self.file_preview = Some(cx.new(|cx| FilePreview::new(panel.clone(), cx)));
            self.file_explorer = Some(panel);
        }
        self.sync_file_explorer(cx);
    }

    fn sync_file_explorer(&mut self, cx: &mut Context<Self>) {
        if let Some(panel) = &self.file_explorer {
            let root = file_explorer_root(
                &self.state,
                &self.project_id,
                self.selected_worktree_id.as_deref(),
            );
            panel.update(cx, |panel, cx| panel.set_root(root, cx));
        }
    }

    /// Open `path` in a Vim tab, at `line` if one is given.
    fn open_file_editor(
        &mut self,
        root: ExplorerRoot,
        path: PathBuf,
        identity: file_preview::FileIdentity,
        line: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Err(error) = self.open_file_editor_inner(root, path, identity, line, window, cx) {
            self.notice = Some(error);
            cx.notify();
        }
    }

    fn open_file_editor_inner(
        &mut self,
        root: ExplorerRoot,
        path: PathBuf,
        identity: file_preview::FileIdentity,
        line: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if !self.ensure_layout(window, cx) {
            return Err("The workspace is still loading.".into());
        }
        if file_explorer_root(
            &self.state,
            &self.project_id,
            self.selected_worktree_id.as_deref(),
        ) != Some(root.clone())
        {
            return Err("The selected worktree changed. Select the file again.".into());
        }
        let shells = self.sessions.list()?;
        if let FileEditorTarget::Existing(shell) =
            file_editor_target(&root, &path, identity, &self.project_id, &shells)?
        {
            if let Some((pane_id, tab_id)) = self.panes.iter().find_map(|(pane_id, pane)| {
                pane.tabs
                    .iter()
                    .find(|tab| tab.shell_id() == Some(&shell.id))
                    .map(|tab| (*pane_id, tab.id))
            }) {
                self.bring_up_tab(pane_id, tab_id, window, cx);
            } else {
                let pane_id = self.editor_pane(cx)?;
                self.attach_session(pane_id, shell.clone(), window, cx)?;
                self.focus_active(window, cx);
                self.save_layout();
            }
            if let Some(line) = line {
                self.jump_editor_to_line(shell.id, line);
            }
            return Ok(());
        }
        let shell = self.sessions.create_editor(
            self.project_id.clone(),
            root.worktree_id,
            root.path,
            path,
            identity,
            line,
        )?;
        let pane_id = self.editor_pane(cx)?;
        self.attach_session(pane_id, shell, window, cx)?;
        if let Ok(shells) = self.sessions.list() {
            self.shells = shells;
        }
        self.focus_active(window, cx);
        self.save_layout();
        Ok(())
    }

    /// Take a Vim that is already open to `line`: leave whatever mode it is in, then `:LINE`.
    /// The keys go in from a worker, because a key that follows text is sent after a pause.
    fn jump_editor_to_line(&self, shell_id: String, line: u32) {
        use session_keys::{Item, Key};
        let sessions = self.sessions.clone();
        std::thread::spawn(move || {
            let _ = sessions.send_keys(
                &shell_id,
                &[
                    Item::Key(Key::Escape),
                    Item::Text(format!(":{line}")),
                    Item::Key(Key::Enter),
                ],
            );
        });
    }

    fn editor_pane(&mut self, cx: &mut Context<Self>) -> Result<PaneId, String> {
        if let Some(main) = self.routing_pane() {
            return Ok(main);
        }
        if let Some(pane_id) = self
            .panes
            .iter()
            .find(|(id, pane)| !self.pane_is_locked(**id) && pane.tabs.iter().any(Tab::is_terminal))
            .map(|(id, _)| *id)
            .or_else(|| {
                self.panes
                    .keys()
                    .copied()
                    .find(|id| !self.pane_is_locked(*id))
            })
        {
            return Ok(pane_id);
        }
        let new_pane = self.next_pane_id;
        self.next_pane_id += 1;
        let target = self.active_pane;
        if !self.layout.split(target, Axis::SideBySide, new_pane) {
            return Err("Cannot make room for an editor tab.".into());
        }
        self.panes.insert(
            new_pane,
            Pane {
                tabs: Vec::new(),
                active: 0,
            },
        );
        cx.notify();
        Ok(new_pane)
    }

    /// Follow the active tab to its worktree. Returns whether the selection moved.
    fn remember_active_worktree(&mut self, cx: &mut Context<Self>) -> bool {
        let worktree_id = self
            .panes
            .get(&self.active_pane)
            .and_then(|pane| pane.tabs.get(pane.active))
            .and_then(|tab| match &tab.content {
                TabContent::Shell {
                    shell_id,
                    worktree_id,
                    ..
                } => {
                    let assigned = worktree_id.as_ref().and_then(|id| {
                        self.state
                            .worktrees
                            .iter()
                            .find(|worktree| worktree.id == *id)
                    });
                    if assigned.is_some_and(|worktree| worktree.project_id != self.project_id) {
                        return None;
                    }
                    let project_scoped = self.shells.iter().any(|shell| {
                        shell.id == *shell_id
                            && shell.project_id.as_deref() == Some(self.project_id.as_str())
                    });
                    if let Some(id) = worktree_id.as_ref().filter(|_| assigned.is_none()) {
                        return project_scoped.then(|| id.clone());
                    }
                    if assigned.is_none() && !project_scoped {
                        return None;
                    }
                    // A removed launch worktree remains an explicit unavailable context.
                    let current = self.shell_cwds.get(shell_id).and_then(|cwd| {
                        self.state
                            .worktrees
                            .iter()
                            .filter(|worktree| {
                                worktree.project_id == self.project_id
                                    && cwd.starts_with(&worktree.path)
                            })
                            .max_by_key(|worktree| worktree.path.components().count())
                    });
                    current
                        .map(|worktree| worktree.id.clone())
                        .or_else(|| worktree_id.clone())
                }
                _ => None,
            });
        let moved = worktree_id
            .as_ref()
            .is_some_and(|id| self.selected_worktree_id.as_ref() != Some(id));
        if let Some(id) = worktree_id {
            self.selected_worktree_id = Some(id);
        }
        self.sync_file_explorer(cx);
        moved
    }

    fn begin_folder_edit(&mut self, id: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
        self.begin_folder_edit_in(id, None, window, cx);
    }

    fn expand_project_folder(&mut self, id: Option<&str>) {
        let mut current = id.map(str::to_owned);
        let mut seen = HashSet::new();
        while let Some(id) = current {
            if !seen.insert(id.clone()) {
                break;
            }
            self.collapsed_project_folders.remove(&id);
            current = self
                .state
                .project_folders
                .iter()
                .find(|folder| folder.id == id)
                .and_then(|folder| folder.parent_id.clone());
        }
    }

    fn begin_folder_edit_in(
        &mut self,
        id: Option<&str>,
        parent_id: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal_open() {
            return;
        }
        let folder = id
            .and_then(|id| self.state.project_folder(id).ok())
            .cloned();
        self.search_focused = false;
        self.panel_menu = None;
        self.notice = None;
        self.begin_tab_drag(cx);
        let store = self.store.clone();
        let parent_id = parent_id.map(str::to_owned);
        let editor = cx.new(|cx| match parent_id {
            Some(parent_id) => FolderEditor::new_in(store, folder, Some(parent_id), cx),
            None => FolderEditor::new(store, folder, cx),
        });
        editor.update(cx, |editor, cx| editor.focus(window, cx));
        cx.subscribe_in(&editor, window, |workspace, _, event, window, cx| {
            workspace.folder_editor = None;
            workspace.finish_tab_drag(cx);
            if let FolderEditorEvent::Saved(folder) = event {
                workspace.refresh_project_metadata(cx);
                workspace.expand_project_folder(Some(&folder.id));
            }
            workspace.focus_active(window, cx);
            cx.notify();
        })
        .detach();
        self.folder_editor = Some(editor);
        cx.notify();
    }

    fn move_tab(
        &mut self,
        drag: &DraggedTab,
        target: PaneId,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if drag.project_id != self.project_id
            || !self.layout_ready
            || !self.panes.contains_key(&target)
        {
            return;
        }
        let Some(source) = self.panes.get_mut(&drag.pane_id) else {
            return;
        };
        let Some(from) = source.tabs.iter().position(|tab| tab.id == drag.tab_id) else {
            return;
        };
        let selected = source.tabs.get(source.active).map(|tab| tab.id);
        let tab = source.tabs.remove(from);
        tab.set_visible(false, cx);
        source.active = selected
            .and_then(|id| source.tabs.iter().position(|tab| tab.id == id))
            .unwrap_or_else(|| from.min(source.tabs.len().saturating_sub(1)));
        let target_index = if drag.pane_id == target && from < index {
            index.saturating_sub(1)
        } else {
            index
        };
        let dest = self.panes.get_mut(&target).unwrap();
        let index = target_index.min(dest.tabs.len());
        dest.tabs.insert(index, tab);
        dest.active = index;
        if drag.pane_id != target
            && self.main_pane != Some(drag.pane_id)
            && self
                .panes
                .get(&drag.pane_id)
                .is_some_and(|pane| pane.tabs.is_empty())
        {
            self.panes.remove(&drag.pane_id);
            if let Some(layout) = self.layout.clone().without(drag.pane_id) {
                self.layout = layout;
            }
        }
        for pane in self.panes.values() {
            for (index, tab) in pane.tabs.iter().enumerate() {
                tab.set_visible(index == pane.active, cx);
            }
        }
        self.active_pane = target;
        self.search_focused = false;
        self.search_marked = None;
        self.drop_target = None;
        self.focus_active(window, cx);
        self.save_layout();
        cx.notify();
    }

    fn dock_tab(
        &mut self,
        drag: &DraggedTab,
        target: Option<PaneId>,
        side: DockSide,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if drag.project_id != self.project_id || !self.layout_ready {
            return;
        }
        if !self
            .panes
            .get(&drag.pane_id)
            .is_some_and(|pane| pane.tabs.iter().any(|tab| tab.id == drag.tab_id))
        {
            return;
        }
        if target == Some(drag.pane_id)
            && self
                .panes
                .get(&drag.pane_id)
                .is_some_and(|pane| pane.tabs.len() == 1)
        {
            return;
        }
        let axis = if matches!(side, DockSide::Left | DockSide::Right) {
            Axis::SideBySide
        } else {
            Axis::Stacked
        };
        let new_first = matches!(side, DockSide::Left | DockSide::Top);
        let id = self.next_pane_id;
        self.next_pane_id += 1;
        let next_layout = if let Some(target) = target {
            let mut layout = self.layout.clone();
            if !layout.split_with(target, axis, id, new_first) {
                return;
            }
            layout
        } else if new_first {
            Layout::Split {
                axis,
                ratio: 0.25,
                first: Box::new(Layout::Pane(id)),
                second: Box::new(self.layout.clone()),
            }
        } else {
            Layout::Split {
                axis,
                ratio: 0.75,
                first: Box::new(self.layout.clone()),
                second: Box::new(Layout::Pane(id)),
            }
        };
        self.layout = next_layout;
        self.panes.insert(
            id,
            Pane {
                tabs: Vec::new(),
                active: 0,
            },
        );
        self.move_tab(drag, id, 0, window, cx);
    }

    fn pane_drop(
        &mut self,
        drag: &DraggedTab,
        pane_id: PaneId,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let position = window.mouse_position();
        let viewport = window.viewport_size();
        let outer = if position.x < px(12.0) {
            Some(DockSide::Left)
        } else if position.x > viewport.width - px(12.0) {
            Some(DockSide::Right)
        } else if position.y < px(12.0) {
            Some(DockSide::Top)
        } else if position.y > viewport.height - px(12.0) {
            Some(DockSide::Bottom)
        } else {
            None
        };
        if let Some(side) = outer {
            self.dock_tab(drag, None, side, window, cx);
            return;
        }
        let local = position - bounds.origin;
        let x = local.x.as_f32() / bounds.size.width.as_f32().max(1.0);
        let y = local.y.as_f32() / bounds.size.height.as_f32().max(1.0);
        let side = if x < 0.23 {
            Some(DockSide::Left)
        } else if x > 0.77 {
            Some(DockSide::Right)
        } else if y < 0.23 {
            Some(DockSide::Top)
        } else if y > 0.77 {
            Some(DockSide::Bottom)
        } else {
            None
        };
        match side {
            Some(side) => self.dock_tab(drag, Some(pane_id), side, window, cx),
            None => {
                let len = self
                    .panes
                    .get(&pane_id)
                    .map(|pane| pane.tabs.len())
                    .unwrap_or(0);
                self.move_tab(drag, pane_id, len, window, cx);
            }
        }
    }

    /// Drops the terminal snapshots and frees their atlas textures.
    fn release_snapshots(&mut self, cx: &mut Context<Self>) {
        let snapshots = std::mem::take(&mut self.terminal_snapshots);
        release_render_images(snapshots.into_values().collect(), cx);
    }

    fn modal_open(&self) -> bool {
        self.project_creator.is_some()
            || self.folder_editor.is_some()
            || self.remote_prompt.is_some()
    }

    fn begin_tab_drag(&mut self, cx: &mut Context<Self>) {
        self.release_snapshots(cx);
        self.tab_dragging = true;
        for (pane_id, pane) in &self.panes {
            if let Some(tab) = pane.tabs.get(pane.active) {
                // Only what is on screen is frozen. Focus mode hides the other panes'
                // terminals, and a released one has nothing to capture.
                if !self.tab_is_shown(*pane_id, pane.active, pane.active) {
                    continue;
                }
                if let Some(terminal) = tab.terminal() {
                    let snapshot = terminal.update(cx, |terminal, _| {
                        let snapshot = terminal.snapshot().ok();
                        terminal.set_visible(false);
                        snapshot
                    });
                    if let Some(snapshot) = snapshot {
                        self.terminal_snapshots.insert(tab.id, snapshot);
                    }
                }
            }
        }
        cx.notify();
    }

    fn finish_tab_drag(&mut self, cx: &mut Context<Self>) {
        // A modal keeps the terminals frozen; showing them would draw the
        // native surfaces above it.
        if self.modal_open() {
            return;
        }
        self.tab_dragging = false;
        self.release_snapshots(cx);
        for pane in self.panes.values() {
            for (index, tab) in pane.tabs.iter().enumerate() {
                tab.set_visible(index == pane.active, cx);
            }
        }
    }

    /// Whether the tab at `index` of a pane is on screen.
    fn tab_is_shown(&self, pane_id: PaneId, pane_active: usize, index: usize) -> bool {
        terminal_lifecycle::is_shown(
            index,
            pane_active,
            pane_id,
            self.active_pane,
            self.focus_mode,
        )
    }

    /// Show the terminals that are on screen, hide the rest, and note when each tab
    /// went off screen. Returns whether any tab changed between shown and hidden.
    fn sync_tab_visibility(&mut self, cx: &mut Context<Self>) -> bool {
        let now = Instant::now();
        let mut changed = false;
        for (pane_id, pane) in &mut self.panes {
            for (index, tab) in pane.tabs.iter_mut().enumerate() {
                let shown = terminal_lifecycle::is_shown(
                    index,
                    pane.active,
                    *pane_id,
                    self.active_pane,
                    self.focus_mode,
                );
                tab.set_visible(shown && !self.tab_dragging, cx);
                if shown {
                    if tab.hidden_since.take().is_some() {
                        changed = true;
                        // Bringing a tab back is asking for its terminal again, by
                        // whatever path it came back, so an old failure is forgotten.
                        if let Some(attach) = tab.content.attach_state() {
                            *attach.error = None;
                            *attach.failures = 0;
                        }
                    }
                } else if tab.hidden_since.is_none() {
                    tab.hidden_since = Some(now);
                    changed = true;
                }
            }
        }
        changed
    }

    /// Give a shell tab that has no terminal a new one attached to its session.
    /// Returns whether it has one afterwards. A failure is remembered on the tab
    /// so it is not retried on every frame.
    fn attach_terminal(
        &mut self,
        pane_id: PaneId,
        tab_id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(target) = self
            .panes
            .get_mut(&pane_id)
            .and_then(|pane| pane.tabs.iter_mut().find(|tab| tab.id == tab_id))
            .and_then(|tab| match &mut tab.content {
                TabContent::Shell {
                    shell_id,
                    terminal: None,
                    attach_error: None,
                    ..
                } => Some(AttachTarget::Local(shell_id.clone())),
                TabContent::RemoteShell {
                    desktop_id,
                    shell_id,
                    terminal: None,
                    attach_error: None,
                    ..
                } => Some(AttachTarget::Remote(desktop_id.clone(), shell_id.clone())),
                _ => None,
            })
        else {
            return false;
        };
        let result = match &target {
            AttachTarget::Local(shell_id) => {
                match self.shells.iter().find(|shell| &shell.id == shell_id) {
                    Some(shell) => Ok(shell.clone()),
                    None => self.sessions.get(shell_id),
                }
                .and_then(|shell| self.spawn_terminal(&shell, false, window, cx))
            }
            AttachTarget::Remote(desktop_id, shell_id) => {
                self.spawn_remote_terminal(desktop_id, shell_id, false, window, cx)
            }
        };
        let Some(attach) = self
            .panes
            .get_mut(&pane_id)
            .and_then(|pane| pane.tabs.iter_mut().find(|tab| tab.id == tab_id))
            .and_then(|tab| tab.content.attach_state())
        else {
            return false;
        };
        match result {
            Ok(spawned) => {
                *attach.terminal = Some(spawned);
                *attach.error = None;
                *attach.failures = 0;
                true
            }
            Err(error) => {
                *attach.error = Some(error.clone());
                *attach.failures = attach.failures.saturating_add(1);
                let retry = *attach.failures < ATTACH_RETRIES;
                self.notice = Some(error);
                if retry {
                    self.schedule_attach_retry(cx);
                }
                false
            }
        }
    }

    /// A tab that failed to attach, say because tmux was busy while many windows
    /// opened at once, is tried again shortly, a few times, without any click.
    fn schedule_attach_retry(&mut self, cx: &mut Context<Self>) {
        if self.attach_retry.is_some() {
            return;
        }
        self.attach_retry = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(ATTACH_RETRY_DELAY).await;
            let _ = this.update(cx, |workspace, cx| {
                workspace.attach_retry = None;
                for pane in workspace.panes.values_mut() {
                    for tab in &mut pane.tabs {
                        if let Some(attach) = tab.content.attach_state()
                            && *attach.failures < ATTACH_RETRIES
                        {
                            *attach.error = None;
                        }
                    }
                }
                cx.notify();
            });
        }));
    }

    /// Attach a terminal to every tab that is on screen without one: tabs restored
    /// with their window, and tabs coming back from release.
    fn attach_shown_terminals(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let missing = self
            .panes
            .iter()
            .flat_map(|(pane_id, pane)| {
                pane.tabs
                    .iter()
                    .enumerate()
                    .filter(|(index, tab)| {
                        self.tab_is_shown(*pane_id, pane.active, *index)
                            && matches!(
                                tab.content,
                                TabContent::Shell {
                                    terminal: None,
                                    attach_error: None,
                                    ..
                                } | TabContent::RemoteShell {
                                    terminal: None,
                                    attach_error: None,
                                    ..
                                }
                            )
                    })
                    .map(|(_, tab)| (*pane_id, tab.id))
            })
            .collect::<Vec<_>>();
        for (pane_id, tab_id) in missing {
            self.attach_terminal(pane_id, tab_id, window, cx);
        }
    }

    /// Which hidden terminals to release now and when to look again.
    fn hidden_terminal_plan(&self) -> terminal_lifecycle::Plan {
        let now = Instant::now();
        let panes = self
            .panes
            .iter()
            .map(|(id, pane)| terminal_lifecycle::PaneState {
                id: *id,
                active: pane.active,
                tabs: pane
                    .tabs
                    .iter()
                    .map(|tab| terminal_lifecycle::TabState {
                        id: tab.id,
                        attached: tab.terminal().is_some(),
                        pinned: keeps_terminal(tab, &self.remote_ended, &self.shells),
                        hidden_for: tab
                            .hidden_since
                            .map_or(Duration::ZERO, |since| now.saturating_duration_since(since)),
                    })
                    .collect(),
            })
            .collect::<Vec<_>>();
        terminal_lifecycle::plan(&terminal_lifecycle::Screen {
            panes: &panes,
            active_pane: self.active_pane,
            focus_mode: self.focus_mode,
            // Dragging a tab, the pane menu and modals all run `begin_tab_drag`, which
            // freezes the shown terminals as snapshots until they end.
            frozen: self.tab_dragging || self.modal_open(),
        })
    }

    /// Drop the terminals the policy gives up, which detaches their tmux clients and
    /// frees their surfaces, then schedule the next look.
    fn release_hidden_terminals(&mut self, cx: &mut Context<Self>) {
        let plan = self.hidden_terminal_plan();
        for tab_id in plan.release {
            for pane in self.panes.values_mut() {
                for tab in pane.tabs.iter_mut().filter(|tab| tab.id == tab_id) {
                    tab.release_terminal(cx);
                }
            }
        }
        self.schedule_terminal_release(plan.recheck, cx);
    }

    /// Replaces any pending pass, so a burst of tab switches leaves a single timer.
    fn schedule_terminal_release(&mut self, after: Option<Duration>, cx: &mut Context<Self>) {
        self.terminal_release = after.map(|after| {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(after).await;
                let _ = this.update(cx, |workspace, cx| workspace.release_hidden_terminals(cx));
            })
        });
    }

    fn resize_at(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(resize) = &self.resizing else {
            return;
        };
        let ratio = if resize.axis == Axis::SideBySide {
            (position.x - resize.bounds.origin.x).as_f32()
                / resize.bounds.size.width.as_f32().max(1.0)
        } else {
            (position.y - resize.bounds.origin.y).as_f32()
                / resize.bounds.size.height.as_f32().max(1.0)
        };
        if self.layout.ratio_at(&resize.path).is_some()
            && self.layout.set_ratio(&resize.path, ratio)
        {
            cx.notify();
        }
    }

    fn end_resize(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Every left mouse-up lands here; only a divider drag changes the layout.
        if self.resizing.is_none() {
            return;
        }
        self.resize_at(window.mouse_position(), cx);
        self.resizing = None;
        self.save_layout();
    }

    fn load_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.session_refresh_generation = self.session_refresh_generation.wrapping_add(1);
        // The tabs below are rebuilt from the layout, which claims what is kept again.
        self.release_tab_claims();
        self.project_sort_menu_open = false;
        if self.focus_mode {
            self.set_focus_mode(false, window, cx);
        }
        self.layout_ready = false;
        // A new project's saved ratios are drawn as they are.
        self.pane_area = None;
        for pane in self.panes.values() {
            for tab in &pane.tabs {
                tab.set_visible(false, cx);
            }
        }
        self.layout = Layout::Pane(1);
        self.main_pane = None;
        self.panes.clear();
        self.tab_dragging = false;
        self.layout_menu_open = false;
        self.release_snapshots(cx);
        self.drop_target = None;
        self.resizing = None;
        self.panel_menu = None;
        self.panes.insert(
            1,
            Pane {
                tabs: Vec::new(),
                active: 0,
            },
        );
        self.active_pane = 1;
        self.focus.focus(window, cx);
        let shells = match self.sessions.list() {
            Ok(shells) => shells,
            Err(error) => {
                self.notice = Some(error);
                cx.notify();
                return;
            }
        };
        // A layout that cannot be read (corrupt, or saved by a newer build) must not
        // leave the window unusable. Open a default layout and say why; the store keeps
        // the unreadable data as it is.
        let mut layout_notice = None;
        let saved_layout = if let Some(layout) = self.restore_layout.clone() {
            Ok(Some(layout))
        } else {
            self.layouts.load(&self.project_id)
        };
        let mut saved = match saved_layout {
            Ok(saved) => saved,
            Err(error) => {
                layout_notice = Some(format!("{error}; using a default layout"));
                None
            }
        };
        let destination_missing = saved.is_none();
        if let Some(previous) = &self.carry_layout {
            let destination = saved.clone().unwrap_or_else(|| ProjectLayout {
                layout: Layout::Pane(1),
                panes: BTreeMap::from([(1, SavedPane::default())]),
                active_pane: 1,
                panels_initialized: true,
                detached_shell_ids: HashSet::new(),
                selected_worktree_id: self.selected_worktree_id.clone(),
                selected_task_id: None,
                sidebar_visible: true,
                window_size: None,
                locked_panes: None,
                main_pane: None,
            });
            match destination.carry_locked_regions_from(previous) {
                Ok(layout) => saved = Some(layout),
                Err(error) => {
                    layout_notice = Some(format!("{error}; locked regions were not carried over"));
                }
            }
        }
        let locked_ids = saved
            .as_ref()
            .map(ProjectLayout::effective_locked_panes)
            .unwrap_or_default();
        let locked_shell_ids: HashSet<_> = saved
            .as_ref()
            .into_iter()
            .flat_map(|layout| locked_ids.iter().filter_map(|id| layout.panes.get(id)))
            .flat_map(|pane| {
                pane.tabs.iter().filter_map(|tab| match tab {
                    SavedTab::Shell { shell_id } => Some(shell_id.clone()),
                    SavedTab::Panel { .. } | SavedTab::RemoteShell { .. } => None,
                })
            })
            .collect();
        self.locked_panes = saved
            .as_ref()
            .and_then(|layout| layout.locked_panes.clone());
        // The window keeps a size the user gave it when the project changes. The frame it
        // opened with (clamped to the display) and zoomed or full-screen frames never
        // become a project's remembered size.
        self.window_size = saved.as_ref().and_then(|saved| saved.window_size);
        if self.settings.remember_window_size {
            self.window_size = self.window_size_user.or(self.window_size);
        }
        let live_shells = shells
            .iter()
            .filter(|shell| {
                (session_belongs_to_workspace(shell, &self.project_id)
                    || locked_shell_ids.contains(&shell.id))
                    && shell.alive
            })
            .map(|shell| (shell.id.clone(), shell.clone()))
            .collect::<BTreeMap<_, _>>();
        self.layout = saved
            .as_ref()
            .map(|saved| saved.layout.clone())
            .unwrap_or(Layout::Pane(1));
        self.panes.clear();
        let pane_ids = self.layout.pane_ids();
        for pane_id in &pane_ids {
            self.panes.insert(
                *pane_id,
                Pane {
                    tabs: Vec::new(),
                    active: 0,
                },
            );
        }
        self.next_pane_id = pane_ids.iter().copied().max().unwrap_or(1) + 1;
        self.main_pane = saved
            .as_ref()
            .and_then(|saved| saved.main_pane)
            .filter(|id| self.panes.contains_key(id));
        let active_pane = saved
            .as_ref()
            .map(|saved| saved.active_pane)
            .filter(|id| self.panes.contains_key(id))
            .unwrap_or_else(|| self.layout.first_pane());
        self.active_pane = active_pane;
        // Shells a project had before it was opened join its main pane, unless that is a locked
        // pane: nothing puts a shell into a locked pane that nobody asked for.
        let shell_pane = self
            .main_pane
            .filter(|id| !locked_ids.contains(id))
            .or_else(|| {
                (!locked_ids.contains(&active_pane))
                    .then_some(active_pane)
                    .or_else(|| pane_ids.iter().copied().find(|id| !locked_ids.contains(id)))
            });
        self.detached_shell_ids = saved
            .as_ref()
            .map(|saved| saved.detached_shell_ids.clone())
            .unwrap_or_default();
        self.detached_shell_ids
            .retain(|id| live_shells.contains_key(id));
        self.sidebar_visible = true;
        if let Some(saved) = &saved {
            self.sidebar_visible = saved.sidebar_visible;
            if let Some(worktree_id) = &saved.selected_worktree_id {
                if self
                    .state
                    .worktrees
                    .iter()
                    .find(|worktree| &worktree.id == worktree_id)
                    .is_none_or(|worktree| worktree.project_id == self.project_id)
                {
                    self.selected_worktree_id = Some(worktree_id.clone());
                }
            }
            // A remote project's tasks are the host's: the saved choice stands until its list says
            // otherwise.
            let remote = self.is_remote();
            self.selected_task_id = saved.selected_task_id.clone().filter(|id| {
                remote
                    || self
                        .state
                        .tasks
                        .iter()
                        .any(|task| &task.id == id && task.project_id == self.project_id)
            });
        }
        let mut known_shell_ids = self.detached_shell_ids.clone();
        let mut restore_error = None;
        if let Some(saved) = &saved {
            for pane_id in &pane_ids {
                if let Some(pane) = saved.panes.get(pane_id) {
                    for tab in &pane.tabs {
                        match tab {
                            SavedTab::Shell { shell_id } => {
                                known_shell_ids.insert(shell_id.clone());
                                if let Some(shell) = live_shells.get(shell_id) {
                                    self.restore_shell_tab(*pane_id, shell.clone());
                                }
                            }
                            SavedTab::Panel { panel } => self.attach_panel(*pane_id, *panel, cx),
                            SavedTab::RemoteShell {
                                desktop_id,
                                shell_id,
                            } => self.restore_remote_tab(
                                *pane_id,
                                desktop_id.clone(),
                                shell_id.clone(),
                                cx,
                            ),
                        }
                    }
                }
            }
        }
        // Shells created through the CLI since the last save join the active pane.
        for shell in &shells {
            if shell.kind == ShellKind::Project
                && live_shells.contains_key(&shell.id)
                && !known_shell_ids.contains(&shell.id)
            {
                if let Some(shell_pane) = shell_pane {
                    self.restore_shell_tab(shell_pane, shell.clone());
                }
            }
        }
        // A project on another Mac starts with no terminal: none is made on this Mac for it.
        if destination_missing
            && !self.is_remote()
            && !live_shells.values().any(|shell| {
                shell.kind == ShellKind::Project
                    && session_belongs_to_workspace(shell, &self.project_id)
            })
        {
            if let Some(shell_pane) = shell_pane {
                if let Err(error) = self.spawn_tab(shell_pane, window, cx) {
                    restore_error = Some(error);
                }
            }
        }
        for (pane_id, pane) in &mut self.panes {
            let selected = saved
                .as_ref()
                .and_then(|saved| saved.panes.get(pane_id))
                .and_then(|pane| pane.active_tab_key.as_ref());
            if let Some(index) =
                selected.and_then(|key| pane.tabs.iter().position(|tab| &tab.saved().key() == key))
            {
                pane.active = index;
            }
            for (index, tab) in pane.tabs.iter().enumerate() {
                tab.set_visible(index == pane.active, cx);
            }
        }
        self.active_pane = active_pane;
        if saved
            .as_ref()
            .map(|saved| !saved.panels_initialized && saved.sidebar_visible)
            .unwrap_or(true)
        {
            self.add_navigation_pane(cx);
            self.active_pane = active_pane;
        }
        self.shells = self.sessions.list().unwrap_or(shells);
        self.focus_active(window, cx);
        if let Some(error) = restore_error {
            // Preserve the saved UUID positions and retry on the next refresh.
            self.notice = Some(error);
        } else {
            self.layout_ready = true;
            self.restore_layout = None;
            self.carry_layout = None;
            if let Some((mode, centered)) = self.restore_focus.take() {
                self.focus_centered = centered;
                self.set_focus_mode(mode, window, cx);
            }
            self.notice = layout_notice;
            self.save_layout();
        }
        cx.notify();
    }

    fn ensure_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.layout_ready {
            self.load_project(window, cx);
        }
        self.layout_ready
    }

    fn layout_snapshot(&self) -> Option<ProjectLayout> {
        if !self.layout_ready {
            return None;
        }
        Some(ProjectLayout {
            layout: self.layout.clone(),
            panes: saved_panes(&self.panes),
            active_pane: self.active_pane,
            detached_shell_ids: self.detached_shell_ids.clone(),
            selected_worktree_id: self.selected_worktree_id.clone(),
            selected_task_id: self.selected_task_id.clone(),
            sidebar_visible: self.sidebar_visible,
            panels_initialized: true,
            window_size: self.window_size,
            locked_panes: self.locked_panes.clone(),
            main_pane: self.main_pane(),
        })
    }

    fn save_layout(&mut self) {
        if let Some(size) = self.window_size_pending.take() {
            self.window_size = Some(size);
            self.window_size_user = Some(size);
        }
        let Some(saved) = self.layout_snapshot() else {
            return;
        };
        if let Err(error) = self.layouts.save(&self.project_id, &saved) {
            self.notice = Some(error);
        }
    }

    /// Title the window for `project_name`, unless it already has that title.
    fn set_window_title(&mut self, project_name: &str, window: &mut Window) {
        let title = window_title_for(project_name);
        if self.window_title != title {
            window.set_window_title(&title);
            self.window_title = title;
        }
    }

    /// Tells the Dock menu which project and branch this window shows. Cheap when
    /// nothing changed, so it can follow every refresh.
    fn announce_to_dock(&self, window: &Window, cx: &mut App) {
        let branch = match self.remote_project() {
            Some((host, project)) => self.selected_worktree_id.as_deref().and_then(|id| {
                cx.global::<RemoteState>()
                    .tree()
                    .worktree_branch(host, project, id)
                    .map(str::to_owned)
            }),
            None => self
                .selected_worktree_id
                .as_ref()
                .and_then(|id| {
                    self.state.worktrees.iter().find(|worktree| {
                        worktree.id == *id && worktree.project_id == self.project_id
                    })
                })
                .map(|worktree| worktree.branch.clone()),
        };
        let project = self.project_display_name(cx).unwrap_or_default();
        dock_menu::update_window(
            dock_menu::DockWindow {
                id: window.window_handle().window_id().as_u64(),
                project,
                branch,
            },
            cx,
        );
    }

    fn runtime_window(&self, window: &Window) -> runtime::RuntimeWindow {
        // GPUI reports a zoomed macOS window as Windowed. A zoomed window is restored
        // from its last ordinary frame so that un-zooming still has somewhere to go.
        let (mode, bounds) = match window.window_bounds() {
            WindowBounds::Fullscreen(bounds) => (runtime::WindowMode::Fullscreen, bounds),
            WindowBounds::Maximized(bounds) => (runtime::WindowMode::Maximized, bounds),
            WindowBounds::Windowed(bounds) if window.is_maximized() => (
                runtime::WindowMode::Maximized,
                self.normal_bounds.unwrap_or(bounds),
            ),
            WindowBounds::Windowed(bounds) => (runtime::WindowMode::Windowed, bounds),
        };
        runtime::RuntimeWindow {
            project_id: Some(self.project_id.clone()),
            path: self.cwd.clone(),
            bounds: Some(runtime::WindowGeometry {
                x: bounds.origin.x.as_f32(),
                y: bounds.origin.y.as_f32(),
                width: bounds.size.width.as_f32(),
                height: bounds.size.height.as_f32(),
            }),
            mode,
            layout: self.layout_snapshot(),
            focus_mode: self.focus_mode,
            focus_centered: self.focus_centered,
        }
    }

    fn activate_project(
        &mut self,
        project_id: &str,
        persist: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A project on another Mac is never looked up in, or recorded by, the local store.
        if remote_tree::parse_project_key(project_id).is_some() {
            self.activate_remote_project(project_id, window, cx);
            return;
        }
        let project = if persist {
            match self.store.use_project(project_id) {
                Ok(project) => {
                    if let Ok(state) = self.store.snapshot() {
                        self.state = state;
                    }
                    project
                }
                Err(error) => {
                    self.notice = Some(error);
                    cx.notify();
                    return;
                }
            }
        } else {
            match self.state.project(project_id) {
                Ok(project) => project.clone(),
                Err(error) => {
                    self.notice = Some(error);
                    cx.notify();
                    return;
                }
            }
        };
        if self.project_id == project.id {
            return;
        }
        tooltip::hide(cx);
        self.save_layout();
        if let Some(layout) = self.layout_snapshot() {
            self.carry_layout = Some(layout);
        }
        self.restore_layout = None;
        if self.is_remote() {
            // Back on one of this Mac's projects: the next launch opens it, not the remote one.
            remote_service::select(None, cx);
        }
        self.project_id = project.id;
        self.project_settings_panel = None;
        self.schedule_panel = None;
        self.file_explorer = None;
        self.file_preview = None;
        self.set_window_title(&project.name, window);
        self.cwd = project.root;
        self.selected_worktree_id = self
            .state
            .worktrees_for(&self.project_id)
            .into_iter()
            .find(|worktree| worktree.is_primary)
            .map(|worktree| worktree.id.clone());
        self.selected_task_id = None;
        self.search_focused = false;
        self.search.clear();
        self.search_marked = None;
        self.load_project(window, cx);
        self.announce_to_dock(window, cx);
    }

    /// Make a project on another Mac the window's project, as selecting a local one does:
    /// the layout is that project's own, and the Worktrees, Tasks and Shells panels draw its
    /// lists. Nothing is created or looked up in this Mac's store for it, and the host does
    /// not have to be reachable: what its folder listed last stays on screen, dimmed, while
    /// the connection is made in the background.
    fn activate_remote_project(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.project_id == key || remote_tree::parse_project_key(key).is_none() {
            return;
        }
        tooltip::hide(cx);
        self.save_layout();
        if let Some(layout) = self.layout_snapshot() {
            self.carry_layout = Some(layout);
        }
        self.restore_layout = None;
        self.project_id = key.to_owned();
        self.project_settings_panel = None;
        self.schedule_panel = None;
        self.file_explorer = None;
        self.file_preview = None;
        self.selected_worktree_id = None;
        self.selected_task_id = None;
        self.search_focused = false;
        self.search.clear();
        self.search_marked = None;
        remote_service::select(Some(key.to_owned()), cx);
        self.refresh_window_title(window, cx);
        self.load_project(window, cx);
        remote_service::tick(&self.remote_wants(), cx);
        self.announce_to_dock(window, cx);
    }

    /// Title the window for the selected project, with the Mac it is on if it is on another.
    fn refresh_window_title(&mut self, window: &mut Window, cx: &App) {
        if let Some(name) = self.project_display_name(cx) {
            self.set_window_title(&name, window);
        }
    }

    fn select_worktree(&mut self, worktree_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_remote() {
            self.select_remote_worktree(worktree_id, window, cx);
            return;
        }
        let Some(worktree) = self
            .state
            .worktrees
            .iter()
            .find(|worktree| worktree.id == worktree_id)
            .cloned()
        else {
            return;
        };
        if self.project_id != worktree.project_id {
            self.activate_project(&worktree.project_id, true, window, cx);
        }
        if !self.ensure_layout(window, cx) {
            return;
        }
        self.selected_worktree_id = Some(worktree.id.clone());
        let existing = self.panes.iter().find_map(|(pane_id, pane)| {
            pane.tabs
                .iter()
                .find(|tab| matches!(&tab.content, TabContent::Shell { worktree_id, .. } if worktree_id.as_deref() == Some(worktree.id.as_str())))
                .map(|tab| (*pane_id, tab.id))
        });
        if let Some((pane_id, tab_id)) = existing {
            self.bring_up_tab(pane_id, tab_id, window, cx);
        } else if let Some(shell_id) = self
            .shells
            .iter()
            .rev()
            .find(|shell| {
                shell.alive
                    && shell.project_id.as_deref() == Some(self.project_id.as_str())
                    && shell.worktree_id.as_deref() == Some(worktree.id.as_str())
            })
            .map(|shell| shell.id.clone())
        {
            self.show_shell(&shell_id, window, cx);
        } else {
            self.add_tab(window, cx);
        }
        self.save_layout();
        cx.notify();
    }

    /// A click on a worktree of a project on another Mac does what `select_worktree` does for a
    /// local one, step for step: the worktree becomes the selected one; a tab already open on a
    /// shell started in it is shown; else the project's newest live shell of that worktree is
    /// opened (`show_shell`); else a plain shell is started there (`add_tab`), in the pane
    /// that is selected, with `shell.create` on the host. The choice itself is
    /// `RemoteTree::click_worktree`. The creation is sent once, a second click while it is out
    /// does nothing, and nothing is ever sent again for the person.
    fn select_remote_worktree(
        &mut self,
        worktree_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((host, project)) = self
            .remote_project()
            .map(|(host, project)| (host.to_owned(), project.to_owned()))
        else {
            return;
        };
        if !self.ensure_layout(window, cx) {
            return;
        }
        self.selected_worktree_id = Some(worktree_id.to_owned());
        let open_tabs = self
            .panes
            .values()
            .flat_map(|pane| pane.tabs.iter().filter_map(Tab::remote))
            .filter(|(tab_host, _)| *tab_host == host)
            .map(|(_, shell)| shell)
            .collect::<Vec<_>>();
        let click = cx.global::<RemoteState>().tree().click_worktree(
            &host,
            &project,
            worktree_id,
            &open_tabs,
        );
        match click {
            remote_tree::WorktreeClick::Tab(shell_id) => {
                let found = self.panes.iter().find_map(|(pane_id, pane)| {
                    pane.tabs
                        .iter()
                        .find(|tab| tab.remote() == Some((host.as_str(), shell_id.as_str())))
                        .map(|tab| (*pane_id, tab.id))
                });
                if let Some((pane_id, tab_id)) = found {
                    self.bring_up_tab(pane_id, tab_id, window, cx);
                }
            }
            remote_tree::WorktreeClick::Live(shell) => {
                self.open_remote_shell(host, shell, window, cx);
            }
            remote_tree::WorktreeClick::Start => {
                self.create_remote_shell(NewShellKind::Shell, false, cx);
            }
            remote_tree::WorktreeClick::Unknown => {
                // Listed within a few seconds while the Worktrees panel shows; a click before
                // that must not start a second shell beside one not heard of yet.
                self.notice = Some(format!(
                    "Reading the terminals on {}; click again in a moment",
                    cx.global::<RemoteState>().tree().host_name(&host)
                ));
            }
        }
        self.save_layout();
        cx.notify();
    }

    fn select_task(&mut self, task_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_remote() {
            // The host has no task writes: a task can only be read.
            if self.ensure_layout(window, cx) {
                self.selected_task_id = Some(task_id.to_owned());
                self.save_layout();
                cx.notify();
            }
            return;
        }
        let Some(task) = self
            .state
            .tasks
            .iter()
            .find(|task| task.id == task_id)
            .cloned()
        else {
            return;
        };
        if self.project_id != task.project_id {
            self.activate_project(&task.project_id, true, window, cx);
        }
        if !self.ensure_layout(window, cx) {
            return;
        }
        self.selected_worktree_id = task
            .worktree_id
            .clone()
            .or(self.selected_worktree_id.clone());
        self.selected_task_id = Some(task.id);
        self.sync_file_explorer(cx);
        self.save_layout();
        cx.notify();
    }

    fn show_shell(&mut self, shell_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.ensure_layout(window, cx) {
            return;
        }
        let existing = self.panes.iter().find_map(|(pane_id, pane)| {
            pane.tabs
                .iter()
                .find(|tab| tab.shell_id() == Some(shell_id))
                .map(|tab| (*pane_id, tab.id))
        });
        if let Some((pane_id, tab_id)) = existing {
            self.bring_up_tab(pane_id, tab_id, window, cx);
            return;
        }
        let Some(shell) = self
            .shells
            .iter()
            .find(|shell| shell.id == shell_id)
            .cloned()
        else {
            return;
        };
        if !shell.alive {
            self.notice = Some(format!("Shell {} has exited", &shell.id[..8]));
            cx.notify();
            return;
        }
        if shell.project_id.as_deref() != Some(self.project_id.as_str()) {
            if let Some(project_id) = &shell.project_id {
                self.activate_project(project_id, true, window, cx);
            }
        }
        if !self.ensure_layout(window, cx) {
            return;
        }
        // Loading another project may already have restored this shell.
        if let Some((pane_id, tab_id)) = self.panes.iter().find_map(|(pane_id, pane)| {
            pane.tabs
                .iter()
                .find(|tab| tab.shell_id() == Some(shell_id))
                .map(|tab| (*pane_id, tab.id))
        }) {
            self.bring_up_tab(pane_id, tab_id, window, cx);
            return;
        }
        self.detached_shell_ids.remove(shell_id);
        if let Err(error) = self.attach_session(self.new_tab_pane(), shell, window, cx) {
            self.notice = Some(error);
        }
        self.save_layout();
        cx.notify();
    }

    /// The selected project's place on another Mac, if that is where it is: the host's id and
    /// the project's id on it.
    fn remote_project(&self) -> Option<(&str, &str)> {
        remote_tree::parse_project_key(&self.project_id)
    }

    fn is_remote(&self) -> bool {
        self.remote_project().is_some()
    }

    /// The name of the Mac the selected project is on.
    fn remote_host_name(&self, cx: &App) -> Option<String> {
        let (host, _) = self.remote_project()?;
        Some(cx.global::<RemoteState>().tree().host_name(host))
    }

    /// Why something that only works on this Mac's own projects cannot run now, when the
    /// selected project is on another Mac. Nothing local is ever done for it instead.
    fn local_only(&self, cx: &App) -> Result<(), String> {
        match self.remote_host_name(cx) {
            Some(host) => Err(format!("Not available for a project on {host}")),
            None => Ok(()),
        }
    }

    /// The selected project's name for the window title and the status bar: with the Mac it is
    /// on, if it is on another one.
    fn project_display_name(&self, cx: &App) -> Option<String> {
        match self.remote_project() {
            Some((host, project)) => {
                let tree = cx.global::<RemoteState>().tree();
                Some(format!(
                    "{} (on {})",
                    tree.project_name(host, project),
                    tree.host_name(host)
                ))
            }
            None => self
                .state
                .project(&self.project_id)
                .ok()
                .map(|project| project.name.clone()),
        }
    }

    /// What of the remote machinery someone is looking at: the hosts' folders in a Projects
    /// panel, a Settings panel listing the hosts, the lists of the selected remote project
    /// that its panels draw, and the hosts that shown tabs are on.
    fn remote_wants(&self) -> remote_tree::Wants {
        let on_screen = |kind: PanelKind| {
            self.panes.iter().any(|(pane_id, pane)| {
                self.tab_is_shown(*pane_id, pane.active, pane.active)
                    && pane.tabs.get(pane.active).is_some_and(
                        |tab| matches!(tab.content, TabContent::Panel(panel) if panel == kind),
                    )
            })
        };
        let (worktrees, tasks, shells) = (
            on_screen(PanelKind::Worktrees),
            on_screen(PanelKind::Tasks),
            on_screen(PanelKind::Shells),
        );
        remote_tree::Wants {
            folders: on_screen(PanelKind::Projects),
            collapsed: self.collapsed_project_folders.clone(),
            settings: on_screen(PanelKind::Settings),
            tab_hosts: self
                .panes
                .iter()
                .flat_map(|(pane_id, pane)| {
                    pane.tabs
                        .iter()
                        .enumerate()
                        .filter(|(index, _)| self.tab_is_shown(*pane_id, pane.active, *index))
                        .filter_map(|(_, tab)| tab.remote().map(|(host, _)| host.to_owned()))
                })
                .collect(),
            // Each panel names things by worktree, and the worktrees' rows count tasks.
            selected: self
                .remote_project()
                .map(|(host, project)| remote_tree::SelectedWants {
                    host: host.to_owned(),
                    project: project.to_owned(),
                    worktrees: worktrees || tasks || shells,
                    tasks: tasks || worktrees,
                    // A click on a worktree decides from the host's shells.
                    shells: shells || worktrees,
                }),
        }
    }

    /// Keep what remote tabs and the REMOTE section show current: ask the hosts for what is
    /// due, retitle tabs from what the lists now say, and notice bridges that have exited.
    /// Returns whether the window needs drawing again.
    fn refresh_remote(&mut self, cx: &mut Context<Self>) -> bool {
        remote_service::tick(&self.remote_wants(), cx);
        let mut changed = false;
        let mut ended = HashSet::new();
        for pane in self.panes.values_mut() {
            for tab in &mut pane.tabs {
                let Some((host, shell)) = tab.remote().map(|(h, s)| (h.to_owned(), s.to_owned()))
                else {
                    continue;
                };
                if tab
                    .terminal()
                    .is_some_and(|terminal| !terminal.read(cx).is_alive())
                {
                    ended.insert(tab.id);
                }
                changed |= retitle(tab, remote_title(&host, &shell, cx));
            }
        }
        if ended != self.remote_ended {
            self.remote_ended = ended;
            changed = true;
        }
        changed
    }

    /// The line above a remote tab's terminal: its shell ended on the host, or the link to
    /// the host is down. `None` while all is well.
    fn remote_strip(
        &self,
        pane_id: PaneId,
        tab_id: Option<TabId>,
        desktop_id: &str,
        ended: bool,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let colors = theme::palette(cx);
        let tree = cx.global::<RemoteState>().tree();
        let label = tree
            .host_label(desktop_id)
            .unwrap_or_else(|| remote_tree::short_id(desktop_id))
            .to_owned();
        let (text, color) = if ended {
            (format!("Ended on {label}"), colors.magenta)
        } else {
            (tree.strip_for(desktop_id)?, colors.gold)
        };
        let reconnect = tab_id.filter(|_| ended).map(|tab_id| {
            div()
                .id(("remote-reconnect", tab_id))
                .px(ui_text::space(8.0))
                .py(ui_text::space(2.0))
                .border_1()
                .border_color(rgb(colors.divider))
                .text_color(rgb(colors.text))
                .cursor_pointer()
                .hover(|style| style.bg(rgb(colors.divider)))
                .child(ui_text::cased("Reconnect"))
                .on_click(cx.listener(move |workspace, _, window, cx| {
                    workspace.reconnect_remote_tab(pane_id, tab_id, window, cx);
                }))
        });
        Some(
            div()
                .flex_none()
                .h(ui_text::space(24.0))
                .px(ui_text::space(10.0))
                .flex()
                .items_center()
                .gap(ui_text::space(10.0))
                .bg(rgb(colors.panel_active))
                .border_b_1()
                .border_color(rgb(color))
                .text_size(ui_text::text(10.0))
                .text_color(rgb(color))
                .child(div().flex_1().min_w_0().text_ellipsis().child(text))
                .children(reconnect)
                .into_any_element(),
        )
    }

    /// Replace the bridge of an ended remote tab with a new one. If the shell has really
    /// ended, the new bridge says so and exits, and the strip comes back.
    fn reconnect_remote_tab(
        &mut self,
        pane_id: PaneId,
        tab_id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self
            .panes
            .get_mut(&pane_id)
            .and_then(|pane| pane.tabs.iter_mut().find(|tab| tab.id == tab_id))
        else {
            return;
        };
        tab.release_terminal(cx);
        if let Some(attach) = tab.content.attach_state() {
            *attach.error = None;
            *attach.failures = 0;
        }
        self.remote_ended.remove(&tab_id);
        self.attach_terminal(pane_id, tab_id, window, cx);
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Show a shell of another Mac the way `show_shell` shows a local one: select its tab if
    /// one is open, otherwise open one in the active pane.
    fn open_remote_shell(
        &mut self,
        desktop_id: String,
        shell: RemoteShell,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pane_id = self.new_tab_pane();
        self.open_remote_shell_in(pane_id, desktop_id, shell, window, cx);
    }

    /// `open_remote_shell` for a pane chosen earlier: a shell made on the host arrives some
    /// time after it was asked for, and opens where the person asked, as a local one made at
    /// once would. If that pane has been closed meanwhile it opens where a new tab does.
    fn open_remote_shell_in(
        &mut self,
        pane_id: PaneId,
        desktop_id: String,
        shell: RemoteShell,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.ensure_layout(window, cx) {
            return;
        }
        let existing = self.panes.iter().find_map(|(pane_id, pane)| {
            pane.tabs
                .iter()
                .find(|tab| tab.remote() == Some((desktop_id.as_str(), shell.id.as_str())))
                .map(|tab| (*pane_id, tab.id))
        });
        if let Some((pane_id, tab_id)) = existing {
            self.bring_up_tab(pane_id, tab_id, window, cx);
            return;
        }
        if !shell.alive {
            let host = cx
                .global::<RemoteState>()
                .tree()
                .host_label(&desktop_id)
                .unwrap_or("the host")
                .to_owned();
            self.notice = Some(format!("{} has ended on {host}", shell.display()));
            cx.notify();
            return;
        }
        let pane_id = if self.panes.contains_key(&pane_id) {
            pane_id
        } else {
            self.new_tab_pane()
        };
        let result = self
            .spawn_remote_terminal(&desktop_id, &shell.id, true, window, cx)
            .and_then(|terminal| {
                let tab = self.remote_tab(desktop_id, shell.id, Some(terminal), cx);
                self.place_new_tab(pane_id, tab, cx)
            });
        if let Err(error) = result {
            self.notice = Some(error);
        }
        self.save_layout();
        cx.notify();
    }

    fn remote_action(
        &mut self,
        action: panels::RemoteAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use panels::RemoteAction;
        match action {
            RemoteAction::OpenShell { host, shell } => {
                self.open_remote_shell(host, shell, window, cx)
            }
            RemoteAction::NewProject(host) => {
                let label = cx.global::<RemoteState>().tree().host_name(&host);
                self.begin_remote_prompt(
                    PromptKind::NewProject {
                        host_id: host,
                        host_label: label,
                    },
                    window,
                    cx,
                );
            }
            RemoteAction::DismissProject(host) => {
                cx.global_mut::<RemoteState>()
                    .tree_mut()
                    .dismiss_project_failure(&host);
                cx.notify();
            }
            RemoteAction::DismissShell { host, project } => {
                cx.global_mut::<RemoteState>()
                    .tree_mut()
                    .dismiss_shell_failure(&host, &project);
                cx.notify();
            }
        }
    }

    /// Ask the host of the selected remote project for a new terminal, once: the New Tab menu,
    /// Cmd+T and the agent shortcuts all come here for a project on another Mac. It starts in
    /// the selected worktree, or the project's root, and opens as a tab when the host
    /// answers. Whatever happens is recorded on the project; nothing is sent again unless
    /// the person asks again.
    fn create_remote_shell(
        &mut self,
        kind: NewShellKind,
        unrestricted: bool,
        cx: &mut Context<Self>,
    ) {
        let Some((host, project)) = self
            .remote_project()
            .map(|(host, project)| (host.to_owned(), project.to_owned()))
        else {
            return;
        };
        if !cx
            .global_mut::<RemoteState>()
            .tree_mut()
            .begin_shell(&host, &project)
        {
            return;
        }
        cx.notify();
        // The shell opens in the pane new tabs open in when it was asked for.
        let pane_id = self.new_tab_pane();
        // The worktree the person chose, if the host still lists it.
        let scope = match self.selected_worktree_id.clone().filter(|id| {
            cx.global::<RemoteState>()
                .tree()
                .worktree_branch(&host, &project, id)
                .is_some()
        }) {
            Some(worktree) => remote_tree::ShellScope::Worktree(worktree),
            None => remote_tree::ShellScope::Project(project.clone()),
        };
        let backend = cx.global_mut::<RemoteState>().backend();
        let work = cx.background_executor().spawn({
            let host = host.clone();
            async move {
                backend
                    .map_err(remote_hosts::RemoteError::Unreachable)?
                    .create_shell(&host, &scope, kind, unrestricted)
            }
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let entity_id = this.entity_id();
            cx.update(|app| {
                // Settled even if this window has closed meanwhile, so the project does not
                // stay "creating" in the others.
                let failure = result
                    .as_ref()
                    .err()
                    .map(remote_hosts::RemoteError::message);
                let shell = app
                    .global_mut::<RemoteState>()
                    .tree_mut()
                    .finish_shell(&host, &project, result);
                app.refresh_windows();
                match shell {
                    Some(shell) => {
                        app.with_window(entity_id, |window, app| {
                            let _ = this.update(app, |workspace, cx| {
                                workspace.open_remote_shell_in(pane_id, host, shell, window, cx);
                            });
                        });
                    }
                    None => {
                        let _ = this.update(app, |workspace, cx| {
                            workspace.notice = failure;
                            cx.notify();
                        });
                    }
                }
            });
        })
        .detach();
    }

    /// Open the host's orchestrator for the selected remote project (or its global one), if
    /// it has one running. Orchestrators cannot be started from here.
    fn open_remote_orchestrator(
        &mut self,
        project_scoped: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((host, project)) = self
            .remote_project()
            .map(|(host, project)| (host.to_owned(), project.to_owned()))
        else {
            return;
        };
        let wanted = project_scoped.then_some(project);
        let known = cx
            .global::<RemoteState>()
            .tree()
            .orchestrator(&host, wanted.as_deref())
            .cloned();
        if let Some(shell) = known {
            self.open_remote_shell(host, shell, window, cx);
            return;
        }
        // Not listed yet (the Shells panel is not showing): ask the host once.
        let backend = match cx.global_mut::<RemoteState>().backend() {
            Ok(backend) => backend,
            Err(error) => {
                self.notice = Some(error);
                cx.notify();
                return;
            }
        };
        let work = cx.background_executor().spawn({
            let host = host.clone();
            async move { backend.orchestrators(&host) }
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let entity_id = this.entity_id();
            cx.update(|app| {
                let request = remote_tree::Request::Orchestrators { host: host.clone() };
                let listed = result.is_ok();
                let tree = app.global_mut::<RemoteState>().tree_mut();
                tree.apply(&request, remote_tree::Reply::Orchestrators(result));
                let shell = tree.orchestrator(&host, wanted.as_deref()).cloned();
                let name = tree.host_name(&host);
                app.refresh_windows();
                let scope = if wanted.is_some() {
                    "project"
                } else {
                    "global"
                };
                app.with_window(entity_id, |window, app| {
                    let _ = this.update(app, |workspace, cx| match shell {
                        Some(shell) => workspace.open_remote_shell(host, shell, window, cx),
                        None => {
                            workspace.notice = Some(if listed {
                                format!("{name} has no {scope} orchestrator running")
                            } else {
                                format!("Cannot reach {name}")
                            });
                            cx.notify();
                        }
                    });
                });
            });
        })
        .detach();
    }

    /// Ask a host to make a project, once, like `create_remote_shell`. It joins the host's
    /// folder when the host answers.
    fn create_remote_project(&mut self, host: String, name: String, cx: &mut Context<Self>) {
        if !cx
            .global_mut::<RemoteState>()
            .tree_mut()
            .begin_project(&host)
        {
            return;
        }
        cx.notify();
        let backend = cx.global_mut::<RemoteState>().backend();
        let work = cx.background_executor().spawn({
            let (host, name) = (host.clone(), name.clone());
            async move {
                backend
                    .map_err(remote_hosts::RemoteError::Unreachable)?
                    .create_project(&host, &name)
            }
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            cx.update(|app| {
                let created = app
                    .global_mut::<RemoteState>()
                    .tree_mut()
                    .finish_project(&host, result);
                app.refresh_windows();
                if let Some(project) = created {
                    let _ = this.update(app, |workspace, cx| {
                        workspace.notice = Some(format!("Created {}", project.name));
                        cx.notify();
                    });
                }
            });
        })
        .detach();
    }

    fn begin_remote_prompt(
        &mut self,
        kind: PromptKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal_open() {
            return;
        }
        self.search_focused = false;
        self.panel_menu = None;
        self.notice = None;
        self.begin_tab_drag(cx);
        let backend = cx.global_mut::<RemoteState>().backend();
        let prompt = cx.new(|cx| RemotePrompt::new(kind, backend, cx));
        prompt.update(cx, |prompt, cx| prompt.focus(window, cx));
        cx.subscribe_in(&prompt, window, |workspace, prompt, event, window, cx| {
            if let Some(defaults) = prompt.read(cx).pair_defaults() {
                workspace.remote_pair_defaults = defaults;
            }
            workspace.remote_prompt = None;
            workspace.finish_tab_drag(cx);
            match event {
                RemotePromptEvent::Closed => {}
                RemotePromptEvent::HostAdded { label } => {
                    cx.global_mut::<RemoteState>().tree_mut().invalidate_hosts();
                    remote_service::tick(&workspace.remote_wants(), cx);
                    workspace.notice = Some(if label.is_empty() {
                        "Host added".to_owned()
                    } else {
                        format!("Added host {label}")
                    });
                }
                RemotePromptEvent::NewProject { host_id, name } => {
                    workspace.create_remote_project(host_id.clone(), name.clone(), cx);
                }
            }
            workspace.focus_active(window, cx);
            cx.notify();
        })
        .detach();
        self.remote_prompt = Some(prompt);
        cx.notify();
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.window_visible = window.is_visible();
        if let Ok(settings) = self.settings_store.load() {
            if &settings != cx.global::<Settings>() {
                cx.set_global(settings);
            }
        }
        // What this tick changed that the window draws. Most ticks change nothing,
        // and a window drawn again for nothing costs a frame of GPU work and a
        // full-size surface for the window server to composite.
        let mut changed = false;
        if &self.settings != cx.global::<Settings>() {
            self.apply_settings(window, cx);
            changed = true;
        }
        self.refresh_count += 1;
        if self.refresh_count % 5 == 0
            && !self.is_remote()
            && self.syncing_project_ids.insert(self.project_id.clone())
        {
            let project_id = self.project_id.clone();
            let work_project_id = project_id.clone();
            let store = self.store.clone();
            // Shared with the other windows: it does nothing when one of them
            // just synced this project or nothing on disk changed since.
            let work = cx
                .background_executor()
                .spawn(async move { store.refresh_worktrees(&work_project_id) });
            cx.spawn(async move |this, cx| {
                let result = work.await;
                let _ = this.update(cx, |workspace, cx| {
                    workspace.syncing_project_ids.remove(&project_id);
                    if workspace.project_id == project_id {
                        let mut changed = false;
                        if let Ok(state) = workspace.store.snapshot() {
                            changed |= workspace.state != state;
                            workspace.state = state;
                        }
                        if let Err(error) = result {
                            changed |= workspace.notice.as_ref() != Some(&error);
                            workspace.notice = Some(error);
                        }
                        if changed {
                            cx.notify();
                        }
                    }
                });
            })
            .detach();
        }
        match self.store.snapshot() {
            Ok(state) => {
                if self.state != state {
                    self.state = state;
                    changed = true;
                }
            }
            Err(error) => {
                if self.notice.as_ref() != Some(&error) {
                    self.notice = Some(error);
                    changed = true;
                }
            }
        }
        // The order of projects is for the eye: a window nobody can see catches up
        // on the first tick after it shows again.
        if self.window_visible {
            self.refresh_project_recency(cx);
        }
        self.refresh_window_title(window, cx);
        self.announce_to_dock(window, cx);
        // A panel that is open in a tab but not on screen is drawn by nobody, so
        // it is brought up to date when its tab comes forward.
        let panel_on_screen = |kind: PanelKind| {
            self.panes.values().any(|pane| {
                pane.tabs.get(pane.active).is_some_and(
                    |tab| matches!(tab.content, TabContent::Panel(panel) if panel == kind),
                )
            })
        };
        // The preview needs the explorer's listings to notice its file changing, so a
        // Preview on screen keeps the explorer polling even when the tree is hidden.
        let files_visible =
            panel_on_screen(PanelKind::Files) || panel_on_screen(PanelKind::Preview);
        let project_settings_visible = panel_on_screen(PanelKind::ProjectSettings);
        if project_settings_visible && let Some(panel) = &self.project_settings_panel {
            panel.update(cx, |panel, cx| panel.refresh_folders(cx));
        }
        if !self.layout_ready {
            self.load_project(window, cx);
            changed = true;
        }
        self.refresh_sessions(cx);
        request_codex_usage(false, cx);
        changed |= self.remember_active_worktree(cx);
        if files_visible {
            if let Some(panel) = &self.file_explorer {
                panel.update(cx, |panel, cx| panel.refresh(cx));
            }
        }
        changed |= self.refresh_shell_titles();
        changed |= self.refresh_remote(cx);
        // The panels draw what they were last told; the open ones are asked again
        // each tick and redraw themselves when it differs. The rest of the window
        // has a few ages ("3m ago") that move with the clock, so it is drawn every
        // few ticks even when nothing else changed.
        let heartbeat = self.refresh_count.is_multiple_of(IDLE_REDRAW_TICKS);
        let panels_open = project_settings_visible || files_visible;
        if changed || heartbeat || panels_open {
            cx.notify();
        }
    }

    fn refresh_sessions(&mut self, cx: &mut Context<Self>) {
        if self.session_refresh_pending {
            return;
        }
        self.session_refresh_pending = true;
        let project_id = self.project_id.clone();
        let generation = self.session_refresh_generation;
        let sessions = self.sessions.clone();
        let work_project_id = project_id.clone();
        let want_metrics = self.window_visible && self.metrics_visible();
        let work = cx.background_executor().spawn(async move {
            // One tmux and `ps` sample serves every window for about a tick.
            let sample = sessions.sample(want_metrics)?;
            let shells = sample.shells;
            let metrics = sample.metrics;
            let cwds = sample.directories;
            let mut claude_usage = BTreeMap::new();
            for shell in &shells {
                if shell.harness == Some(HarnessKind::Claude)
                    && shell.project_id.as_deref() == Some(work_project_id.as_str())
                {
                    if let Ok(Some(snapshot)) =
                        usage::read_claude_usage_at(sessions.state_home(), &shell.id)
                    {
                        claude_usage.insert(shell.id.clone(), snapshot);
                    }
                }
            }
            Ok::<_, String>((shells, metrics, cwds, claude_usage))
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |workspace, cx| {
                workspace.session_refresh_pending = false;
                // Project switches and local launches invalidate an older snapshot,
                // including a switch away from and back to the same project.
                if workspace.project_id != project_id
                    || workspace.session_refresh_generation != generation
                {
                    return;
                }
                let Ok((shells, metrics, cwds, claude_usage)) = result else {
                    return;
                };
                // Most ticks find everything as it was; the window is drawn again
                // only when something it shows is different.
                let mut changed = workspace.shells != shells
                    || workspace.claude_usage != claude_usage
                    || metrics
                        .as_ref()
                        .is_some_and(|metrics| workspace.metrics != *metrics)
                    || cwds
                        .as_ref()
                        .is_some_and(|cwds| workspace.shell_cwds != *cwds);
                workspace.shells = shells;
                if let Some(metrics) = metrics {
                    workspace.metrics = metrics;
                }
                if let Some(cwds) = cwds {
                    workspace.shell_cwds = cwds;
                }
                workspace.claude_usage = claude_usage;
                workspace.adopt_new_shells(cx);
                for home in workspace
                    .shells
                    .iter()
                    .filter(|shell| shell.alive && shell.harness == Some(HarnessKind::Codex))
                    .filter_map(|shell| shell.codex_home.clone())
                    .collect::<HashSet<_>>()
                {
                    request_codex_usage_at(home, false, cx);
                }
                workspace.refresh_grok_usage(false, cx);
                workspace.refresh_agent_activity(cx);
                changed |= workspace.remember_active_worktree(cx);
                changed |= workspace.refresh_shell_titles();
                if changed {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Terminals started outside this window while it is open (`riwork shell
    /// create`, the phone companion, an agent) get a tab in the active pane, as
    /// they would when the project is next loaded. The tab is added behind the
    /// current one: nothing changes focus, the selected tab or the active pane,
    /// and the terminal view attaches when the tab is first shown. (A pane the
    /// user emptied has no current tab, so the new one is shown there, still
    /// without focus.)
    fn adopt_new_shells(&mut self, cx: &mut Context<Self>) {
        // A terminal is claimed only once there is a pane to put it in, and not
        // while a tab is being dragged; the next refresh looks again.
        if self.tab_dragging {
            return;
        }
        let Some(pane_id) = self.adoption_pane().filter(|_| self.layout_ready) else {
            return;
        };
        let shown = self
            .panes
            .values()
            .flat_map(|pane| pane.tabs.iter().filter_map(Tab::shell_id))
            .collect::<HashSet<_>>();
        let new = shells_to_adopt(
            &self.shells,
            &self.project_id,
            &shown,
            &self.detached_shell_ids,
        )
        .into_iter()
        .filter(|shell| claim_shell(&shell.id))
        .cloned()
        .collect::<Vec<_>>();
        if new.is_empty() {
            return;
        }
        for shell in new {
            let tab = self.shell_tab(shell, None);
            if let Some(pane) = self.panes.get_mut(&pane_id) {
                pane.tabs.push(tab);
            }
        }
        self.save_layout();
        cx.notify();
    }

    /// Lets go of the terminals this window has tabs for, so a window that
    /// still shows the project can pick up the ones this one no longer holds.
    fn release_tab_claims(&self) {
        for shell_id in self
            .panes
            .values()
            .flat_map(|pane| pane.tabs.iter().filter_map(Tab::shell_id))
        {
            release_shell(shell_id);
        }
    }

    /// The pane a terminal that appeared by itself joins: the main pane, if there is one, else
    /// the active one, as when a project is loaded, unless it is locked.
    fn adoption_pane(&self) -> Option<PaneId> {
        if let Some(main) = self.main_pane().filter(|id| !self.pane_is_locked(*id)) {
            return Some(main);
        }
        std::iter::once(self.active_pane)
            .chain(self.layout.pane_ids())
            .find(|id| self.panes.contains_key(id) && !self.pane_is_locked(*id))
    }

    /// The bottom bar's usage item is drawn.
    fn usage_chip_visible(&self) -> bool {
        use status_bar::{StatusItemKind, StatusSide};
        !self.focus_mode
            && [StatusSide::Left, StatusSide::Right]
                .into_iter()
                .any(|side| {
                    self.settings
                        .status_bar
                        .visible_items(side)
                        .contains(&StatusItemKind::Usage)
                })
    }

    fn usage_panel_visible(&self) -> bool {
        self.panes.values().any(|pane| {
            pane.tabs
                .get(pane.active)
                .is_some_and(|tab| matches!(tab.content, TabContent::Panel(PanelKind::Usage)))
        })
    }

    fn active_shell(&self) -> Option<&ShellSession> {
        self.panes
            .get(&self.active_pane)
            .and_then(|pane| pane.tabs.get(pane.active))
            .and_then(Tab::shell_id)
            .and_then(|id| self.shells.iter().find(|shell| shell.id == id))
    }

    /// Grok tabs whose session usage this window shows: every live one of the
    /// project while the Usage panel is on screen, and the focused one while the
    /// bottom bar's usage item is. Anything else is not worth a `grok usage`.
    fn grok_usage_targets(&self) -> Vec<String> {
        let panel = self.usage_panel_visible();
        let focused = self
            .usage_chip_visible()
            .then(|| self.active_shell())
            .flatten()
            .map(|shell| shell.id.as_str());
        self.shells
            .iter()
            .filter(|shell| shell.alive && shell.harness == Some(HarnessKind::Grok))
            .filter(|shell| {
                (panel && shell.project_id.as_deref() == Some(self.project_id.as_str()))
                    || focused == Some(shell.id.as_str())
            })
            .map(|shell| shell.id.clone())
            .collect()
    }

    /// Read Grok's per-session usage on a worker for the tabs shown. Grok has no
    /// interface for its account allowance, so this is tokens and cost only. Each
    /// tick asks whether a tab's figures are due (about every 30 s, sooner while
    /// a tab has none); the reads themselves are also cached across windows, so
    /// this never starts `grok` every tick. `force` is the Refresh action.
    fn refresh_grok_usage(&mut self, force: bool, cx: &mut Context<Self>) {
        if self.grok_usage_pending {
            return;
        }
        let shells = &self.shells;
        self.grok_usage
            .retain(|id, _| shells.iter().any(|shell| shell.id == *id && shell.alive));
        let targets = self.grok_usage_targets();
        let now = unix_time();
        let due = force
            || targets
                .iter()
                .any(|id| self.grok_usage.get(id).is_none_or(|tab| tab.is_due(now)));
        if targets.is_empty() || !due {
            return;
        }
        self.grok_usage_pending = true;
        let sessions = self.sessions.clone();
        let previous = self.grok_usage.clone();
        let work = cx.background_executor().spawn(async move {
            let pids = sessions.pane_pids(&targets);
            let targets = targets
                .into_iter()
                .map(|shell_id| usage::GrokTarget {
                    pane_pid: match &pids {
                        Ok(pids) => pids
                            .get(&shell_id)
                            .copied()
                            .ok_or_else(|| "tmux did not report a process for this tab".to_owned()),
                        Err(error) => Err(error.clone()),
                    },
                    shell_id,
                })
                .collect::<Vec<_>>();
            usage::read_grok_usages(&targets, &previous, force)
        });
        cx.spawn(async move |this, cx| {
            let results = work.await;
            let _ = this.update(cx, |workspace, cx| {
                workspace.grok_usage_pending = false;
                workspace.grok_usage.extend(results);
                cx.notify();
            });
        })
        .detach();
    }

    /// CPU and memory feed only the status bar's resources item and the Shells
    /// panel, and sampling them costs a `ps` run. A window showing neither keeps
    /// its last figures and picks them up again on the refresh after either
    /// appears.
    fn metrics_visible(&self) -> bool {
        use status_bar::{StatusItemKind, StatusSide};
        let status = &self.settings.status_bar;
        let in_status_bar = !self.focus_mode
            && [StatusSide::Left, StatusSide::Right]
                .into_iter()
                .any(|side| {
                    status
                        .visible_items(side)
                        .contains(&StatusItemKind::Resources)
                });
        in_status_bar
            || self.panes.values().any(|pane| {
                pane.tabs
                    .get(pane.active)
                    .is_some_and(|tab| matches!(tab.content, TabContent::Panel(PanelKind::Shells)))
            })
    }

    /// Retitle the shell tabs from their sessions. Returns whether any title changed.
    fn refresh_shell_titles(&mut self) -> bool {
        let mut changed = false;
        for pane in self.panes.values_mut() {
            for tab in &mut pane.tabs {
                if let Some(path) = tab
                    .shell_id()
                    .and_then(|id| self.shells.iter().find(|shell| shell.id == id))
                    .and_then(|shell| shell.editor_path.as_ref())
                {
                    changed |= retitle(
                        tab,
                        format!(
                            "VIM · {}",
                            path.file_name()
                                .map(|name| name.to_string_lossy())
                                .unwrap_or_default()
                        ),
                    );
                    continue;
                }
                if let Some(shell) = tab
                    .shell_id()
                    .and_then(|id| self.shells.iter().find(|shell| shell.id == id))
                    .filter(|shell| shell.kind == ShellKind::Orchestrator)
                {
                    changed |= retitle(
                        tab,
                        contextual_shell_title(
                            orchestrator_tab_title(shell),
                            shell,
                            &self.project_id,
                            &self.state,
                        ),
                    );
                    continue;
                }
                if let Some(cwd) = tab.shell_id().and_then(|id| self.shell_cwds.get(id)) {
                    let shell = self
                        .shells
                        .iter()
                        .find(|shell| tab.shell_id() == Some(&shell.id));
                    let project_id = shell
                        .and_then(|shell| shell.project_id.as_deref())
                        .unwrap_or(&self.project_id);
                    let worktree = self
                        .state
                        .worktrees
                        .iter()
                        .filter(|worktree| worktree.project_id == project_id)
                        .filter(|worktree| cwd.starts_with(&worktree.path))
                        .max_by_key(|worktree| worktree.path.as_os_str().len());
                    let label = worktree
                        .map(|worktree| worktree.branch.as_str())
                        .unwrap_or("outside");
                    let program = self
                        .shells
                        .iter()
                        .find(|shell| tab.shell_id() == Some(&shell.id))
                        .and_then(|shell| shell.harness)
                        .map(harness_name)
                        .unwrap_or(&self.shell_name);
                    let title = format!("{} {:02} · {}", program, tab.id, label);
                    let title = shell
                        .map(|shell| {
                            contextual_shell_title(
                                title.clone(),
                                shell,
                                &self.project_id,
                                &self.state,
                            )
                        })
                        .unwrap_or(title);
                    changed |= retitle(tab, title);
                }
            }
        }
        changed
    }

    fn add_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.ensure_layout(window, cx) {
            return;
        }
        if self.is_remote() {
            self.create_remote_shell(NewShellKind::Shell, false, cx);
            return;
        }
        if let Err(error) = self.spawn_tab(self.new_tab_pane(), window, cx) {
            self.notice = Some(error);
            cx.notify();
        }
        self.save_layout();
    }

    fn add_split(&mut self, axis: Axis, window: &mut Window, cx: &mut Context<Self>) {
        if self.focus_mode {
            self.set_focus_mode(false, window, cx);
        }
        if !self.ensure_layout(window, cx) {
            return;
        }
        let target = self.active_pane;
        let new_pane = self.next_pane_id;
        self.next_pane_id += 1;
        self.panes.insert(
            new_pane,
            Pane {
                tabs: Vec::new(),
                active: 0,
            },
        );
        // A pane next to a remote project's starts empty: a terminal there is made on the other
        // Mac, and only when asked for, from the New Tab menu.
        if !self.is_remote()
            && let Err(error) = self.spawn_tab(new_pane, window, cx)
        {
            self.panes.remove(&new_pane);
            self.notice = Some(error);
            cx.notify();
            return;
        }
        self.layout.split(target, axis, new_pane);
        self.active_pane = new_pane;
        self.save_layout();
        cx.notify();
    }

    fn focus_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.remember_active_worktree(cx);
        let files_active = self
            .panes
            .get(&self.active_pane)
            .and_then(|pane| pane.tabs.get(pane.active))
            .is_some_and(|tab| matches!(tab.content, TabContent::Panel(PanelKind::Files)));
        if files_active {
            self.search_focused = false;
            self.ensure_file_explorer(cx);
            if let Some(panel) = &self.file_explorer {
                panel.update(cx, |panel, cx| {
                    panel.refresh(cx);
                    panel.focus(window, cx);
                });
            }
            return;
        }
        let preview_active = self
            .panes
            .get(&self.active_pane)
            .and_then(|pane| pane.tabs.get(pane.active))
            .is_some_and(|tab| matches!(tab.content, TabContent::Panel(PanelKind::Preview)));
        if preview_active {
            self.search_focused = false;
            self.ensure_file_explorer(cx);
            if let Some(panel) = &self.file_explorer {
                panel.update(cx, |panel, cx| panel.refresh(cx));
            }
            if let Some(panel) = &self.file_preview {
                panel.update(cx, |panel, cx| panel.focus(window, cx));
            }
            return;
        }
        let project_settings_active = self
            .panes
            .get(&self.active_pane)
            .and_then(|pane| pane.tabs.get(pane.active))
            .is_some_and(|tab| {
                matches!(tab.content, TabContent::Panel(PanelKind::ProjectSettings))
            });
        if project_settings_active {
            self.search_focused = false;
            self.ensure_project_settings(cx);
            if let Some(panel) = &self.project_settings_panel {
                panel.update(cx, |panel, cx| panel.focus(window, cx));
            }
            return;
        }
        let schedules_active = self
            .panes
            .get(&self.active_pane)
            .and_then(|pane| pane.tabs.get(pane.active))
            .is_some_and(|tab| matches!(tab.content, TabContent::Panel(PanelKind::Schedules)));
        if schedules_active {
            self.search_focused = false;
            if let Some(panel) = &self.schedule_panel {
                panel.update(cx, |panel, cx| panel.focus(window, cx));
            }
            return;
        }
        let settings_active = self
            .panes
            .get(&self.active_pane)
            .and_then(|pane| pane.tabs.get(pane.active))
            .is_some_and(|tab| matches!(tab.content, TabContent::Panel(PanelKind::Settings)));
        if settings_active {
            self.search_focused = false;
            self.settings_panel
                .update(cx, |panel, cx| panel.focus(window, cx));
            return;
        }
        // A tab that was released while hidden gets its terminal back here, before
        // it is asked to take the keys.
        if let Some(tab_id) = self
            .panes
            .get(&self.active_pane)
            .and_then(|pane| pane.tabs.get(pane.active))
            .map(|tab| tab.id)
        {
            self.attach_terminal(self.active_pane, tab_id, window, cx);
        }
        let terminal = self
            .panes
            .get(&self.active_pane)
            .and_then(|pane| pane.tabs.get(pane.active))
            .and_then(|tab| tab.terminal().cloned());
        if let Some(terminal) = terminal {
            // The terminal takes the keys, so a search left open must not also
            // act on them: the adapter does not stop them bubbling to us.
            self.search_focused = false;
            self.search_marked = None;
            terminal.update(cx, |terminal, cx| terminal.focus(window, cx));
        } else {
            self.focus.focus(window, cx);
        }
    }

    fn select_pane(&mut self, pane_id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        if self.panes.contains_key(&pane_id) {
            self.active_pane = pane_id;
            self.focus_active(window, cx);
            self.save_layout();
            cx.notify();
        }
    }

    fn select_tab(
        &mut self,
        pane_id: PaneId,
        tab_id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.ensure_layout(window, cx) {
            return;
        }
        let Some(pane) = self.panes.get_mut(&pane_id) else {
            return;
        };
        let Some(index) = pane.tabs.iter().position(|tab| tab.id == tab_id) else {
            return;
        };
        pane.active = index;
        for (tab_index, tab) in pane.tabs.iter().enumerate() {
            tab.set_visible(tab_index == index, cx);
        }
        // Choosing a tab that could not attach is asking to try again.
        if let Some(attach) = pane.tabs[index].content.attach_state() {
            *attach.error = None;
            *attach.failures = 0;
        }
        self.active_pane = pane_id;
        self.search_focused = false;
        self.search_marked = None;
        self.focus_active(window, cx);
        self.save_layout();
        cx.notify();
    }

    fn cycle_tab(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pane) = self.panes.get(&self.active_pane) else {
            return;
        };
        if pane.tabs.is_empty() {
            return;
        }
        let len = pane.tabs.len() as isize;
        let index = (pane.active as isize + delta).rem_euclid(len) as usize;
        let pane_id = self.active_pane;
        let tab_id = pane.tabs[index].id;
        self.select_tab(pane_id, tab_id, window, cx);
    }

    fn remove_tab(
        &mut self,
        pane_id: PaneId,
        tab_id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.ensure_layout(window, cx) {
            return;
        }
        let Some(pane) = self.panes.get_mut(&pane_id) else {
            return;
        };
        let Some(index) = pane.tabs.iter().position(|tab| tab.id == tab_id) else {
            return;
        };
        let should_focus = self.active_pane == pane_id && pane.active == index;
        let removed = pane.tabs.remove(index);
        if let Some(shell_id) = removed.shell_id() {
            self.detached_shell_ids.insert(shell_id.to_owned());
        }
        removed.set_visible(false, cx);
        // The main pane is where new tabs open, so it stays when its last tab closes.
        if pane.tabs.is_empty() && self.main_pane != Some(pane_id) {
            self.remove_pane(pane_id, window, cx);
            return;
        }
        if index < pane.active {
            pane.active -= 1;
        } else if pane.active >= pane.tabs.len() {
            pane.active = pane.tabs.len().saturating_sub(1);
        }
        for (tab_index, tab) in pane.tabs.iter().enumerate() {
            tab.set_visible(tab_index == pane.active, cx);
        }
        if should_focus {
            self.focus_active(window, cx);
        }
        self.save_layout();
        cx.notify();
    }

    fn remove_pane(&mut self, pane_id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        if !self.ensure_layout(window, cx) {
            return;
        }
        // An open pane menu keeps the terminals frozen as snapshots, and only
        // the render-time guard ends that once the menu is gone.
        self.panel_menu = None;
        self.drop_target = None;
        if self.panes.len() == 1 {
            if let Some(pane) = self.panes.get_mut(&pane_id) {
                for tab in pane.tabs.drain(..) {
                    if let Some(shell_id) = tab.shell_id() {
                        self.detached_shell_ids.insert(shell_id.to_owned());
                    }
                    tab.set_visible(false, cx);
                }
                pane.active = 0;
            }
            if self.tab_dragging {
                self.finish_tab_drag(cx);
            }
            self.focus_active(window, cx);
            self.save_layout();
            cx.notify();
            return;
        }
        let was_locked = self.pane_is_locked(pane_id);
        if let Some(locked) = &mut self.locked_panes {
            locked.remove(&pane_id);
        }
        let was_active = self.active_pane == pane_id;
        let inheritor = pane_inheriting_space(&self.layout, pane_id);
        // The tabs of a pane that closes move into the main pane, after its own, instead of
        // closing with it. The main pane's own tabs close with it, as they always did, and a
        // locked pane's are left alone.
        let into_main = aggregation_target(self.main_pane(), pane_id, was_locked);
        for tab in close_pane_tabs(&mut self.panes, pane_id, into_main) {
            if let Some(shell_id) = tab.shell_id() {
                self.detached_shell_ids.insert(shell_id.to_owned());
            }
            tab.set_visible(false, cx);
        }
        if let Some(pane) = into_main.and_then(|main| self.panes.get(&main)) {
            for (index, tab) in pane.tabs.iter().enumerate() {
                tab.set_visible(index == pane.active, cx);
            }
        }
        if self.main_pane == Some(pane_id) {
            self.main_pane = None;
        }
        if self.tab_dragging {
            self.finish_tab_drag(cx);
        }
        if let Some(layout) = self.layout.clone().without(pane_id) {
            self.layout = layout;
            if was_active {
                self.active_pane = inheritor
                    .filter(|id| self.panes.contains_key(id))
                    .unwrap_or_else(|| self.layout.first_pane());
                self.focus_active(window, cx);
            }
            self.save_layout();
            cx.notify();
        }
    }

    fn pane_is_locked(&self, pane_id: PaneId) -> bool {
        pane_lock_state(
            self.locked_panes.as_ref(),
            pane_id,
            self.layout.first_pane(),
            || {
                self.panes.get(&pane_id).is_some_and(|pane| {
                    pane.tabs.iter().any(|tab| {
                        matches!(
                            tab.panel(),
                            Some(PanelKind::Projects | PanelKind::Worktrees | PanelKind::Files)
                        )
                    })
                })
            },
        )
    }

    fn toggle_pane_lock(&mut self, pane_id: PaneId, cx: &mut Context<Self>) {
        let mut locked: HashSet<_> = self
            .panes
            .keys()
            .copied()
            .filter(|id| self.pane_is_locked(*id))
            .collect();
        if !locked.remove(&pane_id) {
            locked.insert(pane_id);
        }
        self.locked_panes = Some(locked);
        // The refusal hint is stale once the pane can be closed again.
        if self.notice.as_deref().is_some_and(is_locked_close_hint) {
            self.notice = None;
        }
        self.save_layout();
        cx.notify();
    }

    /// The main pane, if the window has one.
    fn main_pane(&self) -> Option<PaneId> {
        self.main_pane.filter(|id| self.panes.contains_key(id))
    }

    /// The main pane while it takes what is opened.
    fn routing_pane(&self) -> Option<PaneId> {
        routing_target(self.main_pane, &self.panes, self.focus_mode)
    }

    /// The pane a new tab opens in: the main pane, else the selected one.
    fn new_tab_pane(&self) -> PaneId {
        self.routing_pane().unwrap_or(self.active_pane)
    }

    /// Make `pane_id` the main pane, or none if it already is. Another pane that was the main
    /// pane stops being one: a layout has at most one. A locked pane may be the main pane when
    /// the user says so; nothing makes it one on its own.
    fn toggle_main_pane(&mut self, pane_id: PaneId, cx: &mut Context<Self>) {
        if !self.panes.contains_key(&pane_id) {
            return;
        }
        self.main_pane = (self.main_pane() != Some(pane_id)).then_some(pane_id);
        // The hint to choose one is stale once there is one.
        if self.notice.as_deref() == Some(NO_MAIN_PANE_HINT) {
            self.notice = None;
        }
        self.save_layout();
        cx.notify();
    }

    /// Bring up a tab that is already open, for a click on its shell, worktree or orchestrator.
    /// With a main pane it is selected where it is, or moved into the main pane when it is
    /// behind a terminal (see `layouts::reuse_open_tab`); without one it is selected where it is.
    fn bring_up_tab(
        &mut self,
        pane_id: PaneId,
        tab_id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(main) = self.routing_pane()
            && let Some(pane) = self.panes.get(&pane_id)
            && let Some(index) = pane.tabs.iter().position(|tab| tab.id == tab_id)
        {
            let tab = PreviewTab {
                pane: pane_id,
                shown: pane.active == index,
                behind_shell: pane_shows_shell_in(&self.panes, pane_id),
            };
            if layouts::reuse_open_tab(main, tab, self.pane_is_locked(pane_id))
                == OpenTabReuse::IntoMain
                && move_tab_between_panes(&mut self.panes, pane_id, tab_id, main, true)
            {
                self.show_pane_tabs(pane_id, cx);
                self.select_tab(main, tab_id, window, cx);
                return;
            }
        }
        self.select_tab(pane_id, tab_id, window, cx);
    }

    /// A tab has left `pane_id` for the main pane. What the pane shows now is shown. It is never
    /// left empty by this: only a tab that is not the pane's selected one moves.
    fn show_pane_tabs(&self, pane_id: PaneId, cx: &mut Context<Self>) {
        if let Some(pane) = self.panes.get(&pane_id) {
            for (index, tab) in pane.tabs.iter().enumerate() {
                tab.set_visible(index == pane.active, cx);
            }
        }
    }

    /// Move the tabs of every unlocked pane into the main pane and remove the panes that
    /// empties. Shells keep running: a tab moves, it is not closed.
    fn gather_tabs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.ensure_layout(window, cx) {
            return;
        }
        self.panel_menu = None;
        self.finish_tab_drag(cx);
        let Some(main) = self.main_pane() else {
            self.notice = Some(NO_MAIN_PANE_HINT.to_owned());
            cx.notify();
            return;
        };
        if self.focus_mode {
            self.set_focus_mode(false, window, cx);
        }
        let locked: HashSet<PaneId> = self
            .panes
            .keys()
            .copied()
            .filter(|id| self.pane_is_locked(*id))
            .collect();
        let gathered = gather_panes(&mut self.layout, &mut self.panes, main, &|id| {
            locked.contains(&id)
        });
        self.notice = Some(gather_notice(&gathered));
        if gathered.moved > 0 || !gathered.removed.is_empty() {
            // With no lock chosen yet, the first pane is locked while it holds a navigation panel.
            // A gather can change which pane is first or what it holds, so pin the locks as they
            // were if it would change them.
            if self.locked_panes.is_none()
                && self
                    .panes
                    .keys()
                    .any(|id| self.pane_is_locked(*id) != locked.contains(id))
            {
                self.locked_panes = Some(locked.clone());
            }
            self.drop_target = None;
            self.active_pane = main;
            for pane in self.panes.values() {
                for (index, tab) in pane.tabs.iter().enumerate() {
                    tab.set_visible(index == pane.active, cx);
                }
            }
            self.focus_active(window, cx);
            self.save_layout();
        }
        cx.notify();
    }

    fn gather_tabs_action(&mut self, _: &GatherTabs, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal_open() {
            return;
        }
        self.gather_tabs(window, cx);
    }

    /// Lay the window out as the default layout (see `default_layout_panes`): a locked navigation
    /// pane on the left and every other tab in the main pane on the right. Tabs move, none
    /// closes, so shells keep running. A window that is laid out like this already is left
    /// alone. The result is saved as the project's layout like any other change.
    fn apply_default_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.ensure_layout(window, cx) {
            return;
        }
        self.panel_menu = None;
        self.layout_menu_open = false;
        self.finish_tab_drag(cx);
        // Focus mode shows one pane, where the new arrangement could not be seen.
        if self.focus_mode {
            self.set_focus_mode(false, window, cx);
        }
        let locked: HashSet<PaneId> = self
            .panes
            .keys()
            .copied()
            .filter(|id| self.pane_is_locked(*id))
            .collect();
        let main = self.main_pane();
        let active_pane = self.active_pane;
        let foreign_shells = self
            .panes
            .values()
            .flat_map(|pane| &pane.tabs)
            .filter_map(Tab::shell_id)
            .filter(|id| {
                self.shells.iter().any(|shell| {
                    shell.id == *id && !session_belongs_to_workspace(shell, &self.project_id)
                })
            })
            .count();
        let Some(applied) = default_layout_panes(
            &mut self.layout,
            &mut self.panes,
            main,
            active_pane,
            &|id| locked.contains(&id),
            &mut self.next_pane_id,
            &mut self.next_tab_id,
        ) else {
            self.notice = Some(DEFAULT_LAYOUT_PRESENT_HINT.to_owned());
            // A click in the layout menu had moved the keys to the workspace itself.
            self.focus_active(window, cx);
            cx.notify();
            return;
        };
        for panel in &applied.opened {
            self.prepare_panel(*panel, cx);
        }
        // The lock is chosen, not inferred: the navigation pane stays the locked one wherever it
        // goes, and the main pane is not.
        self.locked_panes = Some(HashSet::from([applied.navigation]));
        self.main_pane = Some(applied.main);
        self.active_pane = applied.active_pane;
        self.drop_target = None;
        self.resizing = None;
        for pane in self.panes.values() {
            for (index, tab) in pane.tabs.iter().enumerate() {
                tab.set_visible(index == pane.active, cx);
            }
        }
        self.notice = Some(default_layout_notice(&applied, foreign_shells));
        self.focus_active(window, cx);
        self.save_layout();
        cx.notify();
    }

    fn apply_default_layout_action(
        &mut self,
        _: &ApplyDefaultLayout,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal_open() {
            return;
        }
        self.apply_default_layout(window, cx);
    }

    /// Whether the status bar shows the layout item, which the layout menu hangs from.
    fn layout_item_shown(&self) -> bool {
        use status_bar::{StatusItemKind, StatusSide};
        !self.focus_mode
            && [StatusSide::Left, StatusSide::Right]
                .into_iter()
                .any(|side| {
                    self.settings
                        .status_bar
                        .visible_items(side)
                        .contains(&StatusItemKind::Layout)
                })
    }

    fn toggle_layout_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.layout_menu_open {
            self.close_layout_menu(window, cx);
            return;
        }
        self.project_sort_menu_open = false;
        self.panel_menu = None;
        self.layout_menu_open = true;
        // Terminals are native views drawn above everything of ours; the menu is drawn over
        // their snapshots instead, as a pane's menu is.
        if !self.tab_dragging {
            self.begin_tab_drag(cx);
        }
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn close_layout_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.layout_menu_open {
            return;
        }
        self.layout_menu_open = false;
        self.finish_tab_drag(cx);
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Close a tab because the user asked to. A locked pane refuses; internal
    /// removals (moving a tab, project switches, restore) never come through here.
    fn close_tab_by_user(
        &mut self,
        pane_id: PaneId,
        tab_id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.ensure_layout(window, cx) || self.refuse_locked_close(pane_id, UserClose::Tab, cx)
        {
            return;
        }
        self.remove_tab(pane_id, tab_id, window, cx);
    }

    /// Close a pane, and with it every tab in it, because the user asked to.
    fn close_pane_by_user(&mut self, pane_id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        if !self.ensure_layout(window, cx) || self.refuse_locked_close(pane_id, UserClose::Pane, cx)
        {
            return;
        }
        self.remove_pane(pane_id, window, cx);
    }

    /// Say why nothing was closed. The tab X and the menu's Close pane are not drawn
    /// for a locked pane, so this is what a shortcut reaches.
    fn refuse_locked_close(
        &mut self,
        pane_id: PaneId,
        close: UserClose,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(hint) = user_close_refusal(self.pane_is_locked(pane_id), close) else {
            return false;
        };
        self.notice = Some(hint.to_owned());
        cx.notify();
        true
    }

    fn new_tab_action(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal_open() {
            return;
        }
        self.add_tab(window, cx);
    }

    fn new_project_window_action(
        &mut self,
        _: &NewProjectWindow,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let project_id = self.project_id.clone();
        if remote_tree::parse_project_key(&project_id).is_some() {
            // A project on another Mac opens as itself in a new window.
            let window = runtime::RuntimeWindow {
                project_id: Some(project_id),
                path: self.cwd.clone(),
                bounds: None,
                mode: runtime::WindowMode::Windowed,
                layout: None,
                focus_mode: false,
                focus_centered: false,
            };
            if let Err(error) = open_workspace_window(None, self.cwd.clone(), Some(window), cx) {
                self.notice = Some(error);
            }
            cx.notify();
            return;
        }
        self.open_project_window(&project_id, cx);
    }

    fn open_project_window(&mut self, id: &str, cx: &mut Context<Self>) {
        let path = match self.state.project(id) {
            Ok(project) => project.root.clone(),
            Err(error) => {
                self.notice = Some(error);
                cx.notify();
                return;
            }
        };
        if let Err(error) = open_workspace_window(Some(path), self.cwd.clone(), None, cx) {
            self.notice = Some(error);
        }
        cx.notify();
    }

    fn open_codex_action(&mut self, _: &OpenCodex, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal_open() {
            return;
        }
        self.add_harness(HarnessKind::Codex, false, window, cx);
    }

    fn open_claude_action(&mut self, _: &OpenClaude, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal_open() {
            return;
        }
        self.add_harness(HarnessKind::Claude, false, window, cx);
    }

    fn open_grok_action(&mut self, _: &OpenGrok, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal_open() {
            return;
        }
        self.add_harness(HarnessKind::Grok, false, window, cx);
    }

    fn add_harness(
        &mut self,
        harness: HarnessKind,
        unrestricted: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.ensure_layout(window, cx) {
            return;
        }
        self.panel_menu = None;
        self.finish_tab_drag(cx);
        if self.is_remote() {
            self.create_remote_shell(remote_shell_kind(harness), unrestricted, cx);
            return;
        }
        let worktree_id = self.selected_worktree_id.clone();
        let cwd = worktree_id
            .as_ref()
            .and_then(|id| {
                self.state
                    .worktrees
                    .iter()
                    .find(|worktree| &worktree.id == id)
            })
            .map(|worktree| worktree.path.clone())
            .unwrap_or_else(|| self.cwd.clone());
        let result = self
            .sessions
            .create_harness(
                self.project_id.clone(),
                worktree_id,
                cwd,
                harness,
                unrestricted,
            )
            .and_then(|shell| self.attach_session(self.new_tab_pane(), shell, window, cx));
        if let Err(error) = result {
            self.notice = Some(error);
        }
        if let Ok(shells) = self.sessions.list() {
            self.shells = shells;
        }
        self.focus_active(window, cx);
        self.save_layout();
        request_codex_usage(false, cx);
        cx.notify();
    }

    fn toggle_panel_menu(&mut self, pane_id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        self.project_sort_menu_open = false;
        self.layout_menu_open = false;
        self.active_pane = pane_id;
        self.panel_menu = if self.panel_menu == Some(pane_id) {
            None
        } else {
            Some(pane_id)
        };
        if self.panel_menu.is_some() {
            self.begin_tab_drag(cx);
            self.focus.focus(window, cx);
        } else {
            self.finish_tab_drag(cx);
            self.focus_active(window, cx);
        }
        cx.notify();
    }

    fn split_right_action(&mut self, _: &SplitRight, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal_open() {
            return;
        }
        self.add_split(Axis::SideBySide, window, cx);
    }

    fn split_down_action(&mut self, _: &SplitDown, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal_open() {
            return;
        }
        self.add_split(Axis::Stacked, window, cx);
    }

    fn close_tab_action(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal_open() {
            return;
        }
        let Some(pane) = self.panes.get(&self.active_pane) else {
            return;
        };
        let Some(tab) = pane.tabs.get(pane.active) else {
            return;
        };
        self.close_tab_by_user(self.active_pane, tab.id, window, cx);
    }

    fn close_pane_action(&mut self, _: &ClosePane, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal_open() {
            return;
        }
        self.close_pane_by_user(self.active_pane, window, cx);
    }

    fn next_tab_action(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal_open() {
            return;
        }
        self.cycle_tab(1, window, cx);
    }

    fn previous_tab_action(
        &mut self,
        _: &PreviousTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal_open() {
            return;
        }
        self.cycle_tab(-1, window, cx);
    }

    fn toggle_sidebar_action(
        &mut self,
        _: &ToggleSidebar,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal_open() {
            return;
        }
        self.open_panel(PanelKind::Projects, self.active_pane, window, cx);
    }

    fn open_schedules_action(
        &mut self,
        _: &OpenSchedules,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal_open() {
            return;
        }
        self.open_panel(PanelKind::Schedules, self.active_pane, window, cx);
    }

    fn open_settings_action(
        &mut self,
        _: &OpenSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal_open() {
            return;
        }
        self.open_panel(PanelKind::Settings, self.active_pane, window, cx);
    }

    fn open_project_settings_action(
        &mut self,
        _: &OpenProjectSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal_open() {
            return;
        }
        self.open_panel(PanelKind::ProjectSettings, self.active_pane, window, cx);
    }

    fn open_files_action(&mut self, _: &OpenFiles, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal_open() {
            return;
        }
        let pane_id = self
            .panes
            .iter()
            .find_map(|(id, pane)| {
                pane.tabs
                    .iter()
                    .any(|tab| {
                        matches!(
                            tab.content,
                            TabContent::Panel(
                                PanelKind::Projects | PanelKind::Worktrees | PanelKind::Files
                            )
                        )
                    })
                    .then_some(*id)
            })
            .unwrap_or(self.active_pane);
        self.open_panel(PanelKind::Files, pane_id, window, cx);
    }

    fn open_preview_action(
        &mut self,
        _: &OpenPreview,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal_open() {
            return;
        }
        self.open_preview(window, cx);
    }

    /// The pane, tab and visibility of the window's tab for `kind`, if it has one.
    fn panel_tab(&self, kind: PanelKind) -> Option<(PaneId, TabId, bool)> {
        panel_tab_in(&self.panes, kind)
    }

    /// The pane the file explorer's tree is in, which is where its preview goes beside.
    fn explorer_pane(&self) -> Option<PaneId> {
        self.panel_tab(PanelKind::Files)
            .map(|(pane_id, _, _)| pane_id)
    }

    /// Open the Preview panel and give it the keys. A window with a Files panel and no
    /// Preview gets one where a file selection would put it. Without Files there is nothing
    /// to place it beside, so it opens as a tab of the active pane like any other panel.
    fn open_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.ensure_layout(window, cx) {
            return;
        }
        let mut pane_id = self.active_pane;
        if self.panel_tab(PanelKind::Preview).is_none()
            && let Some(main) = self.routing_pane()
        {
            // A main pane takes the preview as a tab, whatever is beside it.
            let placement = PreviewPlacement::Tab {
                pane: main,
                activate: true,
            };
            if let Some(placed) = self.place_panel(PanelKind::Preview, placement, cx) {
                pane_id = placed;
            }
        } else if self.panel_tab(PanelKind::Preview).is_none()
            && !self.focus_mode
            && let Some(explorer) = self.explorer_pane()
        {
            let locked = |id: PaneId| self.pane_is_locked(id);
            let shows_shell = |id: PaneId| self.pane_shows_shell(id);
            let placement = self.layout.preview_placement(
                self.pane_area,
                explorer,
                &PaneFacts {
                    locked: &locked,
                    shows_shell: &shows_shell,
                },
            );
            if let Some(placed) =
                placement.and_then(|placement| self.place_panel(PanelKind::Preview, placement, cx))
            {
                pane_id = placed;
            }
        }
        // Asked for by name, it comes forward and takes the keys wherever it was put.
        self.open_panel(PanelKind::Preview, pane_id, window, cx);
    }

    /// A file was selected in the explorer: show its preview, if the user wants that, or
    /// whatever they want when `asked` (the selection was a link they clicked). The keys stay
    /// where they are, in the tree, so the arrow keys keep moving the selection while the
    /// preview follows it.
    ///
    /// A link is clicked in a terminal, and the terminal keeps the keys and stays on screen. If
    /// the tree is in some other pane the preview goes beside it as usual; if there is no tree
    /// on screen, or it is a tab behind the terminal clicked, the preview goes beside the work
    /// instead (see `Layout::beside_placement`).
    fn reveal_preview(&mut self, asked: bool, cx: &mut Context<Self>) {
        // Focus mode shows one pane; rearranging the others behind it would be unseen.
        if self.focus_mode || !self.layout_ready {
            return;
        }
        let clicked = self.active_pane;
        if let Some(main) = self.main_pane() {
            // The main pane takes the preview as a tab; no pane is split for it. It leaves the
            // tree in front, and for a link the terminal clicked.
            if asked || cx.global::<Settings>().open_preview_on_select {
                let reveal = plan_main_panel(
                    &self.panes,
                    main,
                    PanelKind::Preview,
                    asked.then_some(clicked),
                );
                self.apply_panel_reveal(PanelKind::Preview, reveal, cx);
                if asked {
                    self.note_hidden_panel(PanelKind::Preview, clicked);
                }
            }
            return;
        }
        let explorer = tree_pane_for_reveal(self.explorer_pane(), asked, clicked);
        let Some(explorer) = explorer else {
            if asked {
                self.reveal_panel_for_link(PanelKind::Preview, cx);
            }
            return;
        };
        let existing = self
            .panel_tab(PanelKind::Preview)
            .map(|(pane, _, shown)| PreviewTab {
                pane,
                shown,
                behind_shell: self.pane_shows_shell(pane),
            });
        let locked = |id: PaneId| self.pane_is_locked(id);
        let shows_shell = |id: PaneId| self.pane_shows_shell(id);
        let reveal = self.layout.plan_preview_reveal(
            asked || cx.global::<Settings>().open_preview_on_select,
            existing,
            self.pane_area,
            explorer,
            &PaneFacts {
                locked: &locked,
                shows_shell: &shows_shell,
            },
        );
        self.apply_panel_reveal(PanelKind::Preview, reveal, cx);
        if asked {
            self.note_hidden_panel(PanelKind::Preview, clicked);
        }
    }

    /// Show `panel` (the Preview, or Files) for a link clicked in the active pane, beside the
    /// work and never over it. The keys stay in the terminal that was clicked.
    fn reveal_panel_for_link(&mut self, panel: PanelKind, cx: &mut Context<Self>) {
        if self.focus_mode || !self.layout_ready {
            return;
        }
        let clicked = self.active_pane;
        let reveal = match self.main_pane() {
            Some(main) => plan_main_panel(&self.panes, main, panel, Some(clicked)),
            None => {
                let locked = |id: PaneId| self.pane_is_locked(id);
                plan_link_panel(
                    &self.layout,
                    &self.panes,
                    self.pane_area,
                    &locked,
                    panel,
                    clicked,
                )
            }
        };
        self.apply_panel_reveal(panel, reveal, cx);
        self.note_hidden_panel(panel, clicked);
    }

    /// Do what the placement rules decided about `panel`. Nothing else changes: the active
    /// pane and the keys stay put. Returns the pane the panel's tab was added to or brought
    /// forward in.
    fn apply_panel_reveal(
        &mut self,
        panel: PanelKind,
        reveal: PreviewReveal,
        cx: &mut Context<Self>,
    ) -> Option<PaneId> {
        if reveal != PreviewReveal::Leave && matches!(panel, PanelKind::Files | PanelKind::Preview)
        {
            self.ensure_file_explorer(cx);
        }
        let (pane_id, selected) = apply_panel_reveal_to_panes(
            &mut self.layout,
            &mut self.panes,
            &mut self.next_pane_id,
            &mut self.next_tab_id,
            panel,
            Self::panel_title(panel),
            reveal,
        )?;
        if selected && let Some(pane) = self.panes.get(&pane_id) {
            // The tab now selected shows; the ones it covers, terminals first, are hidden.
            for (index, tab) in pane.tabs.iter().enumerate() {
                tab.set_visible(index == pane.active, cx);
            }
        }
        if let PreviewReveal::Move { from, .. } = reveal {
            self.show_pane_tabs(from, cx);
        }
        self.save_layout();
        cx.notify();
        Some(pane_id)
    }

    /// A click asked for `panel` and it is in a tab strip with a terminal selected over it:
    /// say so, or the click would seem to have done nothing.
    fn note_hidden_panel(&mut self, panel: PanelKind, clicked: PaneId) {
        if let Some((pane, _, false)) = self.panel_tab(panel) {
            self.notice = Some(hidden_panel_notice(panel, pane == clicked));
        }
    }

    /// Whether the pane's selected tab is a terminal: a shell, an agent or an editor.
    fn pane_shows_shell(&self, pane_id: PaneId) -> bool {
        pane_shows_shell_in(&self.panes, pane_id)
    }

    /// Add a tab for `panel` as `placement` says: in a new pane or where the placement says to
    /// select it, it becomes its pane's selected tab; otherwise it waits in the tab strip and
    /// the pane keeps showing what it showed.
    fn place_panel(
        &mut self,
        panel: PanelKind,
        placement: PreviewPlacement,
        cx: &mut Context<Self>,
    ) -> Option<PaneId> {
        self.apply_panel_reveal(panel, PreviewReveal::Open(placement), cx)
    }

    fn focus_search_action(
        &mut self,
        _: &FocusSearch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal_open() {
            return;
        }
        let files_active = self
            .panes
            .get(&self.active_pane)
            .and_then(|pane| pane.tabs.get(pane.active))
            .is_some_and(|tab| matches!(tab.content, TabContent::Panel(PanelKind::Files)));
        if files_active {
            self.ensure_file_explorer(cx);
            if let Some(panel) = &self.file_explorer {
                panel.update(cx, |panel, cx| panel.focus_search(window, cx));
            }
            return;
        }
        let preview_active = self
            .panes
            .get(&self.active_pane)
            .and_then(|pane| pane.tabs.get(pane.active))
            .is_some_and(|tab| matches!(tab.content, TabContent::Panel(PanelKind::Preview)));
        if preview_active {
            // The filter belongs to the Files pane: use it if that is on screen, and leave
            // the preview alone if not. Searching Projects from here would be a surprise.
            if let Some((pane_id, _, true)) = self.panel_tab(PanelKind::Files) {
                self.active_pane = pane_id;
                self.ensure_file_explorer(cx);
                if let Some(panel) = &self.file_explorer {
                    panel.update(cx, |panel, cx| panel.focus_search(window, cx));
                }
                cx.notify();
            }
            return;
        }
        let active_panel = self
            .panes
            .get(&self.active_pane)
            .and_then(|pane| pane.tabs.get(pane.active))
            .is_some_and(|tab| matches!(tab.content, TabContent::Panel(kind) if kind != PanelKind::Settings && kind != PanelKind::ProjectSettings && kind != PanelKind::Usage && kind != PanelKind::Schedules));
        if !active_panel {
            self.open_panel(PanelKind::Projects, self.active_pane, window, cx);
        }
        self.search_focused = true;
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn create_project_action(
        &mut self,
        _: &CreateProject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.begin_project_creation(window, cx);
    }

    fn begin_project_creation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal_open() {
            return;
        }
        let directory = match paths::ensure_default_projects_directory() {
            Ok(directory) => directory,
            Err(error) => {
                self.notice = Some(error);
                cx.notify();
                return;
            }
        };
        self.search_focused = false;
        self.panel_menu = None;
        self.notice = None;
        self.begin_tab_drag(cx);
        let store = self.store.clone();
        let creator = cx.new(|cx| ProjectCreator::new(store, directory, window, cx));
        cx.subscribe_in(&creator, window, |workspace, _, event, window, cx| {
            workspace.project_creator = None;
            workspace.finish_tab_drag(cx);
            match event {
                ProjectCreationEvent::Created(project) => {
                    workspace.activate_project(&project.id, true, window, cx);
                    workspace.open_panel(PanelKind::Projects, workspace.active_pane, window, cx);
                }
                ProjectCreationEvent::Cancelled => workspace.focus_active(window, cx),
            }
            cx.notify();
        })
        .detach();
        self.project_creator = Some(creator);
        cx.notify();
    }

    fn open_orchestrator_action(
        &mut self,
        _: &OpenOrchestrator,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_orchestrator(window, cx);
    }

    fn open_orchestrator(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_scoped_orchestrator(None, window, cx);
    }

    fn open_project_orchestrator_action(
        &mut self,
        _: &OpenProjectOrchestrator,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_scoped_orchestrator(Some(self.project_id.clone()), window, cx);
    }

    fn open_scoped_orchestrator(
        &mut self,
        project_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal_open() || !self.ensure_layout(window, cx) {
            return;
        }
        self.panel_menu = None;
        self.finish_tab_drag(cx);
        if self.is_remote() {
            // The other Mac's orchestrator, if it has one: this Mac makes none for it.
            self.open_remote_orchestrator(project_id.is_some(), window, cx);
            return;
        }
        let session = match project_id.as_deref() {
            Some(id) => match self.state.project(id) {
                Ok(project) => self.sessions.orchestrator_create_for_project(
                    project.id.clone(),
                    project.root.clone(),
                    None,
                ),
                Err(error) => Err(error),
            },
            None => self.sessions.orchestrator_create(self.cwd.clone(), None),
        };
        let shell = match session {
            Ok(shell) => shell,
            Err(error) => {
                self.notice = Some(error);
                cx.notify();
                return;
            }
        };
        // A scope has one persistent session. Reopen its existing tab instead
        // of creating another terminal view or a separate window.
        let existing = self.panes.iter().find_map(|(pane_id, pane)| {
            pane.tabs
                .iter()
                .find(|tab| tab.shell_id() == Some(shell.id.as_str()))
                .map(|tab| (*pane_id, tab.id))
        });
        if let Ok(shells) = self.sessions.list() {
            self.shells = shells;
        }
        self.detached_shell_ids.remove(&shell.id);
        if let Some((pane_id, tab_id)) = existing {
            self.bring_up_tab(pane_id, tab_id, window, cx);
        } else if let Err(error) = self.attach_session(self.new_tab_pane(), shell, window, cx) {
            self.notice = Some(error);
        } else {
            self.focus_active(window, cx);
        }
        self.save_layout();
        cx.notify();
    }

    fn set_focus_mode(&mut self, enabled: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_mode = enabled;
        self.panel_menu = None;
        self.layout_menu_open = false;
        self.resizing = None;
        self.drop_target = None;
        self.finish_tab_drag(cx);
        // Keep native window geometry and the saved split tree unchanged.
        self.focus_active(window, cx);
        cx.notify();
    }

    fn toggle_focus_mode_action(
        &mut self,
        _: &ToggleFocusMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal_open() {
            return;
        }
        self.set_focus_mode(!self.focus_mode, window, cx);
    }

    fn replace_search_text(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
    ) -> Range<usize> {
        let marked = self.search_marked.take();
        let range = if let Some(range) = range_utf16 {
            utf16_to_byte(&self.search, range.start)..utf16_to_byte(&self.search, range.end)
        } else {
            marked.unwrap_or(self.search.len()..self.search.len())
        };
        let text = text.replace('\n', " ").replace('\r', " ");
        self.search.replace_range(range.clone(), &text);
        range.start..range.start + text.len()
    }

    fn search_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A text-size key gets here only when no binding took it, which means a
        // terminal had focus and Ghostty already zoomed its font. Handling it keeps
        // AppKit from offering it to the View menu's item, which would also resize
        // RiWork's text.
        if ui_text::is_size_keystroke(&event.keystroke) {
            cx.stop_propagation();
            return;
        }
        if event.keystroke.key == "escape" && self.project_sort_menu_open {
            self.project_sort_menu_open = false;
            self.focus_active(window, cx);
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if event.keystroke.key == "escape" && cx.has_active_drag() {
            cx.stop_active_drag(window);
            self.finish_tab_drag(cx);
            self.drop_target = None;
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if event.keystroke.key == "escape" && self.layout_menu_open {
            self.close_layout_menu(window, cx);
            cx.stop_propagation();
            return;
        }
        if event.keystroke.key == "escape" && self.panel_menu.is_some() {
            self.panel_menu = None;
            self.finish_tab_drag(cx);
            self.focus_active(window, cx);
            cx.stop_propagation();
            cx.notify();
            return;
        }
        // Keys reach this handler from every focused descendant, including
        // terminals, so the search only owns them while the workspace itself
        // holds focus.
        if !self.search_focused || !self.focus.is_focused(window) {
            return;
        }
        let handled = match event.keystroke.key.as_str() {
            "escape" => {
                self.search_focused = false;
                self.search_marked = None;
                self.focus_active(window, cx);
                true
            }
            "backspace" => {
                self.search.pop();
                self.search_marked = None;
                true
            }
            "enter" | "return" => {
                if let Some(hit) = self.state.search(&self.search).into_iter().next() {
                    match hit {
                        SearchHit::Project(project) => {
                            self.activate_project(&project.id, true, window, cx)
                        }
                        SearchHit::Worktree(worktree) => {
                            self.select_worktree(&worktree.id, window, cx)
                        }
                        SearchHit::Task(task) => self.select_task(&task.id, window, cx),
                    }
                }
                self.search_focused = false;
                self.search_marked = None;
                true
            }
            "a" if event.keystroke.modifiers.platform => {
                self.search.clear();
                self.search_marked = None;
                true
            }
            _ => false,
        };
        if handled {
            cx.stop_propagation();
            cx.notify();
        }
    }

    /// Keeps locked panes at their pixel size when the area the panes fill changes: a
    /// window resize, full screen, or the status bar being switched on or off.
    fn follow_pane_area(&mut self, window: &Window, cx: &mut Context<Self>) {
        // Focus mode draws one pane instead of the tree. Leaving `pane_area` as it was
        // means any change made meanwhile is applied once focus mode ends.
        if self.focus_mode {
            return;
        }
        let viewport = window.viewport_size();
        let footer = if self.settings.status_bar.enabled {
            ui_text::space_f32(STATUS_BAR_HEIGHT)
        } else {
            0.0
        };
        let area = Extent {
            width: viewport.width.as_f32(),
            height: viewport.height.as_f32() - footer,
        };
        if !self.layout_ready || area.width <= 0.0 || area.height <= 0.0 {
            self.pane_area = None;
            return;
        }
        let settled = *self.first_drawn.get_or_insert_with(Instant::now) + WINDOW_SETTLE;
        // The first area of a layout only records what the saved ratios were drawn at.
        let Some(previous) = self.pane_area.replace(area) else {
            return;
        };
        if previous == area || Instant::now() < settled {
            return;
        }
        let locked: HashSet<PaneId> = self
            .layout
            .pane_ids()
            .into_iter()
            .filter(|id| self.pane_is_locked(*id))
            .collect();
        if self
            .layout
            .preserve_locked(previous, area, &|id| locked.contains(&id), MIN_PANE_EXTENT)
        {
            // One write once the resize pauses, not one per frame of it.
            self.layout_save = Some(cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                let _ = this.update(cx, |workspace, _| workspace.save_layout());
            }));
        }
    }

    fn render_layout(
        &self,
        layout: &Layout,
        path: Vec<bool>,
        width: f32,
        x: f32,
        at_window_top: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme::palette(cx);
        match layout {
            Layout::Pane(id) => self.render_pane(*id, width, x, at_window_top, cx),
            Layout::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let horizontal = *axis == Axis::SideBySide;
                let key = format!("split-{path:?}");
                let split_bounds = Rc::new(Cell::new(Bounds::<Pixels>::default()));
                let measured = split_bounds.clone();
                let drag_path = path.clone();
                let resize_axis = *axis;
                let divider = div()
                    .id(format!("divider-{path:?}"))
                    .flex_none()
                    .bg(rgb(colors.divider))
                    .hover(move |style| {
                        controls::hovered(style, colors.divider, |style| style.bg(rgb(colors.cyan)))
                    })
                    // Native: a hairline in the middle of the grip, which fills on hover.
                    .map(|divider| {
                        controls::native(divider, |divider| {
                            divider
                                .bg(rgb(colors.panel))
                                .flex()
                                .justify_center()
                                .items_center()
                                .when(horizontal, |divider| {
                                    divider.child(div().w(px(1.0)).h_full().bg(rgb(colors.divider)))
                                })
                                .when(!horizontal, |divider| {
                                    divider.child(div().h(px(1.0)).w_full().bg(rgb(colors.divider)))
                                })
                        })
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |workspace, _, _, cx| {
                            workspace.resizing = Some(SplitResize {
                                path: drag_path.clone(),
                                axis: resize_axis,
                                bounds: split_bounds.get(),
                            });
                            cx.stop_propagation();
                        }),
                    );
                let divider = if horizontal {
                    divider
                        .w(px(DIVIDER_THICKNESS))
                        .h_full()
                        .cursor_col_resize()
                } else {
                    divider
                        .h(px(DIVIDER_THICKNESS))
                        .w_full()
                        .cursor_row_resize()
                };
                let mut first_path = path.clone();
                first_path.push(false);
                let mut second_path = path;
                second_path.push(true);
                let parent = div()
                    .id(key)
                    .relative()
                    .flex()
                    .size_full()
                    .min_w_0()
                    .min_h_0()
                    .child(
                        canvas(move |bounds, _, _| measured.set(bounds), |_, _, _, _| {})
                            .absolute()
                            .inset_0(),
                    );
                let parent = if horizontal {
                    parent.flex_row()
                } else {
                    parent.flex_col()
                };
                parent
                    .child(
                        div()
                            .flex()
                            .flex_basis(px(0.0))
                            .flex_grow(*ratio)
                            .min_w_0()
                            .min_h_0()
                            .child(self.render_layout(
                                first,
                                first_path,
                                if horizontal {
                                    (width - DIVIDER_THICKNESS).max(0.0) * *ratio
                                } else {
                                    width
                                },
                                x,
                                at_window_top,
                                cx,
                            )),
                    )
                    .child(divider)
                    .child(
                        div()
                            .flex()
                            .flex_basis(px(0.0))
                            .flex_grow(1.0 - *ratio)
                            .min_w_0()
                            .min_h_0()
                            .child(self.render_layout(
                                second,
                                second_path,
                                if horizontal {
                                    (width - DIVIDER_THICKNESS).max(0.0) * (1.0 - *ratio)
                                } else {
                                    width
                                },
                                if horizontal {
                                    x + (width - DIVIDER_THICKNESS).max(0.0) * *ratio
                                        + DIVIDER_THICKNESS
                                } else {
                                    x
                                },
                                at_window_top && horizontal,
                                cx,
                            )),
                    )
                    .into_any_element()
            }
        }
    }

    fn render_pane(
        &self,
        pane_id: PaneId,
        pane_width: f32,
        x: f32,
        at_window_top: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme::palette(cx);
        let Some(pane) = self.panes.get(&pane_id) else {
            return div().into_any_element();
        };
        let selected = self.active_pane == pane_id;
        let window_drag_enabled = at_window_top;
        // The menu button and the drag handle at their scaled widths. A pane too
        // narrow for both drops the handle, so the menu button is never clipped.
        let (menu_width, handle_width) = (ui_text::space_f32(28.0), ui_text::space_f32(12.0));
        let drag_handle = window_drag_enabled && pane_width >= menu_width + handle_width;
        let control_inset = if at_window_top && x < WINDOW_CONTROLS_CONTENT_INSET {
            // Keep the pane menu and the drag handle outside the scrolling tabs; in a
            // pane too narrow for all of it, the tabs give way to the controls.
            let reserved = menu_width + if drag_handle { handle_width } else { 0.0 };
            (WINDOW_CONTROLS_CONTENT_INSET - x).min((pane_width - reserved).max(0.0))
        } else {
            0.0
        };
        let header_width = pane_width - control_inset;
        let show_lock = header_width >= ui_text::space_f32(108.0);
        let show_focus = header_width >= ui_text::space_f32(180.0);
        let pane_locked = self.pane_is_locked(pane_id);
        let has_main = self.main_pane().is_some();
        let is_main = self.main_pane() == Some(pane_id);
        // The marker is an icon button like the others, or its word; a narrower pane leaves
        // it to the menu's check mark.
        let show_main = is_main
            && header_width
                >= ui_text::space_f32(if self.settings.panel_tab_icons {
                    136.0
                } else {
                    152.0
                });
        let account_numbers = codex_account_numbers(&self.shells);
        let tabs = pane
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let tab_id = tab.id;
                let active = index == pane.active;
                let panel = tab.panel().is_some();
                // Shell tabs keep their titles; only built-in panels swap in an icon.
                let icon_panel = tab.panel().filter(|_| self.settings.panel_tab_icons);
                // The X is left out, not disabled, so a locked pane's tabs lose no room to it.
                let close_visible =
                    active && user_close_refusal(pane_locked, UserClose::Tab).is_none();
                let (pad_left, pad_right, gap) = match (icon_panel, close_visible) {
                    (None, _) => (8.0, 8.0, 6.0),
                    (Some(_), true) => (0.0, 4.0, 0.0),
                    (Some(_), false) => (0.0, 0.0, 0.0),
                };
                let tab_color = if active {
                    if panel && !colors.plain_tabs {
                        colors.magenta
                    } else {
                        colors.text
                    }
                } else {
                    colors.muted
                };
                // Native's selected tab is a full-height cell in the content's own
                // background with medium weight, brightest in the selected pane.
                // Other tabs are muted words on the bar; hairlines part the cells.
                let tab_fill = if selected {
                    colors.bg
                } else {
                    colors.panel_active
                };
                let workspace = cx.entity();
                let shell = tab
                    .shell_id()
                    .and_then(|id| self.shells.iter().find(|shell| shell.id == id));
                // A panel's tab says its name as the theme writes labels; a saved
                // layout keeps whatever title the tab was created with.
                let display_title = match tab.panel() {
                    Some(kind) if ui_text::is_native() => Self::panel_label(kind).to_owned(),
                    _ => codex_tab_title(&tab.title, shell, &account_numbers),
                };
                let activity_hint = tab
                    .shell_id()
                    .and_then(|id| self.agent_activity.get(id))
                    .and_then(AgentState::hint);
                div()
                    .id(("tab", tab_id))
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .h_full()
                    .pl(ui_text::space(pad_left))
                    .pr(ui_text::space(pad_right))
                    .gap(ui_text::space(gap))
                    .map(|style| {
                        if !colors.plain_tabs {
                            return style
                                .border_r_1()
                                .border_b_1()
                                .border_color(rgb(if active {
                                    colors.cyan
                                } else {
                                    colors.divider
                                }))
                                .bg(rgb(if active {
                                    colors.panel_active
                                } else {
                                    colors.panel
                                }));
                        }
                        let style = style
                            .h_full()
                            .border_r_1()
                            .border_color(rgb(colors.divider));
                        if active {
                            style
                                .bg(rgb(tab_fill))
                                .font_weight(gpui::FontWeight::MEDIUM)
                        } else {
                            style
                        }
                    })
                    .text_color(rgb(tab_color))
                    .text_size(ui_text::text(if panel && !colors.plain_tabs {
                        9.0
                    } else {
                        10.0
                    }))
                    .cursor_grab()
                    .hover(move |style| {
                        if active && colors.plain_tabs {
                            style
                        } else {
                            style.bg(rgb(colors.panel_active))
                        }
                    })
                    .drag_over::<DraggedTab>(move |style, _, _, _| {
                        style.border_l_2().border_color(rgb(colors.cyan))
                    })
                    .child(match icon_panel {
                        // The tooltip sits on the icon's own box so it does not stack with the X's.
                        Some(kind) => div()
                            .id(("tab-icon", tab_id))
                            .h_full()
                            .min_w(ui_text::space(32.0))
                            .px(ui_text::space(8.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icons::icon(Icon::Panel(kind), tab_color))
                            .child(tooltip::anchor(panel_tooltip(kind), Look::Pane))
                            .into_any_element(),
                        // Another Mac's tabs wear its name in the remote accent.
                        None => match remote_tree::split_remote_title(&display_title)
                            .filter(|_| tab.remote().is_some())
                        {
                            Some((mark, rest)) => div()
                                .flex()
                                .child(div().text_color(rgb(colors.magenta)).child(mark.to_owned()))
                                .child(rest.to_owned())
                                .into_any_element(),
                            // What the agent in the tab is doing, on hover.
                            None => match activity_hint {
                                Some(hint) => div()
                                    .child(display_title.clone())
                                    .child(tooltip::anchor(hint, Look::Pane))
                                    .into_any_element(),
                                None => display_title.clone().into_any_element(),
                            },
                        },
                    })
                    .children(close_visible.then(|| {
                        div()
                            .id(("close-tab", tab_id))
                            .size(ui_text::space(18.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .rounded(px(3.0))
                            .hover(|style| style.bg(rgb(colors.divider)))
                            .child(icons::icon(Icon::Close, colors.muted))
                            .child(tooltip::anchor("Close tab · ⌘W", Look::Pane))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |workspace, _, window, cx| {
                                cx.stop_propagation();
                                workspace.close_tab_by_user(pane_id, tab_id, window, cx);
                            }))
                    }))
                    .on_click(cx.listener(move |workspace, _, window, cx| {
                        workspace.select_tab(pane_id, tab_id, window, cx)
                    }))
                    .on_drag(
                        DraggedTab {
                            pane_id,
                            tab_id,
                            project_id: self.project_id.clone(),
                            title: display_title,
                        },
                        move |drag, _, _, cx| {
                            workspace.update(cx, |workspace, cx| {
                                workspace.panel_menu = None;
                                workspace.begin_tab_drag(cx);
                            });
                            cx.new(|_| drag.clone())
                        },
                    )
                    .on_drop(
                        cx.listener(move |workspace, drag: &DraggedTab, window, cx| {
                            workspace.move_tab(drag, pane_id, index, window, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        let header = div()
            .id(("pane-header", pane_id))
            .overflow_hidden()
            .h(ui_text::space(PANE_HEADER_HEIGHT))
            .flex_none()
            .flex()
            .min_w_0()
            .items_center()
            .bg(rgb(colors.panel))
            .border_b_1()
            // Native marks the selected pane by its brighter tab, not by a line.
            .border_color(rgb(if selected && !colors.plain_tabs {
                colors.cyan
            } else {
                colors.divider
            }))
            .pl(px(control_inset))
            .child(
                div()
                    .id(("tab-strip", pane_id))
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_x_scroll()
                    .children(tabs)
                    .child(
                        div()
                            .id(("window-drag-space", pane_id))
                            .flex_1()
                            .min_w(ui_text::space(18.0))
                            .h_full()
                            .when(window_drag_enabled, |space| {
                                space
                                    .cursor_grab()
                                    .on_mouse_down(MouseButton::Left, start_window_drag)
                            }),
                    )
                    .on_drop(
                        cx.listener(move |workspace, drag: &DraggedTab, window, cx| {
                            let len = workspace
                                .panes
                                .get(&pane_id)
                                .map(|pane| pane.tabs.len())
                                .unwrap_or(0);
                            workspace.move_tab(drag, pane_id, len, window, cx);
                            cx.stop_propagation();
                        }),
                    ),
            )
            .children(drag_handle.then(|| {
                div()
                    .id(("window-drag-handle", pane_id))
                    .flex_none()
                    .w(px(handle_width))
                    .h_full()
                    .cursor_grab()
                    .on_mouse_down(MouseButton::Left, start_window_drag)
            }))
            .child(
                div()
                    .flex()
                    .h_full()
                    .flex_none()
                    // Native groups the pane's buttons in one capsule, as a toolbar does.
                    .map(|group| {
                        controls::native(group, |group| {
                            group
                                .h(ui_text::space(24.0))
                                .items_center()
                                .mx(ui_text::space(6.0))
                                .px(ui_text::space(2.0))
                                .rounded_full()
                                .bg(rgb(colors.panel_active))
                        })
                    })
                    .children(show_main.then(|| self.main_marker(pane_id, cx)))
                    .children(show_lock.then(|| {
                        self.pane_button(
                            pane_id,
                            "lock",
                            if pane_locked {
                                Icon::Lock
                            } else {
                                Icon::Unlock
                            },
                            if pane_locked {
                                colors.cyan
                            } else {
                                colors.muted
                            },
                            |workspace, id, _, cx| workspace.toggle_pane_lock(id, cx),
                            cx,
                        )
                    }))
                    .children(show_focus.then(|| {
                        self.pane_button(
                            pane_id,
                            "focus",
                            Icon::Focus,
                            colors.muted,
                            |workspace, id, window, cx| {
                                workspace.active_pane = id;
                                workspace.set_focus_mode(true, window, cx);
                            },
                            cx,
                        )
                    }))
                    .child(self.pane_button(
                        pane_id,
                        "menu",
                        Icon::More,
                        if self.panel_menu == Some(pane_id) {
                            colors.cyan
                        } else {
                            colors.muted
                        },
                        |workspace, id, window, cx| {
                            workspace.toggle_panel_menu(id, window, cx);
                        },
                        cx,
                    )),
            );
        let skill_upgrade = pane
            .tabs
            .get(pane.active)
            .and_then(Tab::shell_id)
            .and_then(|id| self.shells.iter().find(|shell| shell.id == id))
            .filter(|shell| {
                shell.kind == ShellKind::Orchestrator
                    && !self.sessions.orchestrator_skill_is_current(shell)
            })
            .map(|shell| shell.id.clone());
        let active_panel = match pane.tabs.get(pane.active).map(|tab| &tab.content) {
            Some(TabContent::Panel(panel)) => Some(*panel),
            _ => None,
        };
        let remote_tree = cx.global::<RemoteState>().tree();
        // The hosts' folders, for the Projects panel.
        let remote_folders = if active_panel == Some(PanelKind::Projects) {
            remote_tree.folders(
                &self.search.trim().to_lowercase(),
                self.settings.project_order,
                &self.collapsed_project_folders,
                self.remote_project().map(|_| self.project_id.as_str()),
            )
        } else {
            Vec::new()
        };
        // The selected remote project's lists, for the panels that draw them.
        let remote_selected = active_panel
            .filter(|panel| {
                matches!(
                    panel,
                    PanelKind::Worktrees | PanelKind::Tasks | PanelKind::Shells
                )
            })
            .and_then(|_| remote_tree.selected_view(&self.project_id));
        // What a panel without an implementation for another Mac's project says instead.
        let remote_host_label = self.remote_host_name(cx);
        let content = match pane.tabs.get(pane.active).map(|tab| &tab.content) {
            Some(TabContent::RemoteShell {
                desktop_id,
                terminal,
                attach_error,
                ..
            }) => {
                let tab_id = pane.tabs.get(pane.active).map(|tab| tab.id);
                let snapshot = tab_id.and_then(|id| self.terminal_snapshots.get(&id));
                let body = if self.tab_dragging {
                    match snapshot {
                        Some(snapshot) => img(snapshot.clone()).size_full().into_any_element(),
                        None => div().size_full().bg(rgb(colors.bg)).into_any_element(),
                    }
                } else if let Some(terminal) = terminal {
                    terminal.clone().into_any_element()
                } else {
                    // The bridge starts during the frame this tab is shown in.
                    div()
                        .size_full()
                        .p(ui_text::space(14.0))
                        .bg(rgb(colors.bg))
                        .text_color(rgb(colors.muted))
                        .child(
                            attach_error
                                .clone()
                                .unwrap_or_else(|| "Connecting…".to_owned()),
                        )
                        .into_any_element()
                };
                let ended = tab_id.is_some_and(|id| self.remote_ended.contains(&id));
                match self.remote_strip(pane_id, tab_id, desktop_id, ended, cx) {
                    // The strip sits beside the terminal, not over it: the native surface
                    // would draw above anything laid on top.
                    Some(strip) => div()
                        .size_full()
                        .flex()
                        .flex_col()
                        .child(strip)
                        .child(div().flex_1().min_h_0().child(body))
                        .into_any_element(),
                    None => body,
                }
            }
            Some(TabContent::Shell {
                terminal,
                attach_error,
                ..
            }) => {
                let snapshot = pane
                    .tabs
                    .get(pane.active)
                    .and_then(|tab| self.terminal_snapshots.get(&tab.id));
                if self.tab_dragging {
                    match snapshot {
                        Some(snapshot) => img(snapshot.clone()).size_full().into_any_element(),
                        None => div().size_full().bg(rgb(colors.bg)).into_any_element(),
                    }
                } else if let Some(terminal) = terminal {
                    match pane.tabs.get(pane.active) {
                        // The links read the screen from this shell's tmux session.
                        Some(tab) if tab.shell_id().is_some() => self.terminal_link_layer(
                            pane_id,
                            tab.id,
                            terminal.clone().into_any_element(),
                            cx,
                        ),
                        _ => terminal.clone().into_any_element(),
                    }
                } else {
                    // The terminal attaches during the frame this tab is shown in; this
                    // is what a tab shows if that failed. tmux redraws the screen on
                    // attach, so there is nothing to keep here.
                    div()
                        .size_full()
                        .p(ui_text::space(14.0))
                        .bg(rgb(colors.bg))
                        .text_color(rgb(colors.muted))
                        .child(
                            attach_error
                                .clone()
                                .unwrap_or_else(|| "Attaching…".to_owned()),
                        )
                        .into_any_element()
                }
            }
            Some(TabContent::Panel(kind))
                if remote_host_label.is_some()
                    && panels::remote_support(*kind) == panels::RemoteSupport::Unavailable =>
            {
                panels::unavailable(*kind, remote_host_label.as_deref().unwrap_or_default(), cx)
            }
            Some(TabContent::Panel(PanelKind::Usage)) => self.render_usage_panel(cx),
            Some(TabContent::Panel(PanelKind::Schedules)) => self
                .schedule_panel
                .as_ref()
                .map(|panel| panel.clone().into_any_element())
                .unwrap_or_else(|| div().into_any_element()),
            Some(TabContent::Panel(PanelKind::Settings)) => {
                self.settings_panel.clone().into_any_element()
            }
            Some(TabContent::Panel(PanelKind::Files)) => self
                .file_explorer
                .as_ref()
                .map(|panel| panel.clone().into_any_element())
                .unwrap_or_else(|| div().into_any_element()),
            Some(TabContent::Panel(PanelKind::Preview)) => self
                .file_preview
                .as_ref()
                .map(|panel| panel.clone().into_any_element())
                .unwrap_or_else(|| div().into_any_element()),
            Some(TabContent::Panel(PanelKind::ProjectSettings)) => self
                .project_settings_panel
                .as_ref()
                .map(|panel| panel.clone().into_any_element())
                .unwrap_or_else(|| div().into_any_element()),
            Some(TabContent::Panel(panel)) => panels::render_panel(
                *panel,
                PanelData {
                    remote_folders: &remote_folders,
                    selected_remote: remote_selected.as_ref(),
                    state: &self.state,
                    project_id: &self.project_id,
                    selected_worktree_id: self.selected_worktree_id.as_deref(),
                    selected_task_id: self.selected_task_id.as_deref(),
                    shells: &self.shells,
                    shell_cwds: &self.shell_cwds,
                    metrics: &self.metrics,
                    activity: &self.agent_activity,
                    query: &self.search,
                    search_focused: self.search_focused && selected,
                    focus: self.focus.clone(),
                    control_inset: 0.0,
                    collapsed_folders: &self.collapsed_project_folders,
                    state_home: self.sessions.state_home(),
                    project_order: self.settings.project_order,
                    project_last_edits: &self.project_last_edits,
                    project_sort_menu_open: self.project_sort_menu_open,
                },
                Self::panel_action,
                cx,
            ),
            None => div()
                .p(ui_text::space(14.0))
                .text_color(rgb(colors.muted))
                .child("Drop a tab here or use the pane menu")
                .into_any_element(),
        };
        let body_bounds = Rc::new(Cell::new(Bounds::<Pixels>::default()));
        let measured_body = body_bounds.clone();
        let body = div()
            .id(("pane-body", pane_id))
            .relative()
            .flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .bg(rgb(colors.bg))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |workspace, _, window, cx| {
                    if workspace.active_pane != pane_id {
                        workspace.select_pane(pane_id, window, cx);
                    }
                }),
            )
            .on_drag_move(cx.listener(
                move |workspace, event: &DragMoveEvent<DraggedTab>, _, cx| {
                    if event.drag(cx).project_id != workspace.project_id
                        || !event.bounds.contains(&event.event.position)
                    {
                        return;
                    }
                    let local = event.event.position - event.bounds.origin;
                    let x = local.x.as_f32() / event.bounds.size.width.as_f32().max(1.0);
                    let y = local.y.as_f32() / event.bounds.size.height.as_f32().max(1.0);
                    let side = if x < 0.23 {
                        Some(DockSide::Left)
                    } else if x > 0.77 {
                        Some(DockSide::Right)
                    } else if y < 0.23 {
                        Some(DockSide::Top)
                    } else if y > 0.77 {
                        Some(DockSide::Bottom)
                    } else {
                        None
                    };
                    let target = Some((pane_id, side));
                    if workspace.drop_target != target {
                        workspace.drop_target = target;
                        cx.notify();
                    }
                },
            ))
            .on_drop(
                cx.listener(move |workspace, drag: &DraggedTab, window, cx| {
                    workspace.pane_drop(drag, pane_id, body_bounds.get(), window, cx);
                    cx.stop_propagation();
                }),
            )
            .child(div().flex().size_full().min_w_0().min_h_0().child(content))
            .child(
                canvas(
                    move |bounds, _, _| measured_body.set(bounds),
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            );
        let body = if cx.has_active_drag() {
            if let Some((_, side)) = self.drop_target.filter(|(id, _)| *id == pane_id) {
                let overlay = div()
                    .absolute()
                    .bg(rgb(colors.cyan))
                    .opacity(0.13)
                    .border_1()
                    .border_color(rgb(colors.cyan));
                let overlay = match side {
                    Some(DockSide::Left) => {
                        overlay.top_0().bottom_0().left_0().w(gpui::relative(0.5))
                    }
                    Some(DockSide::Right) => {
                        overlay.top_0().bottom_0().right_0().w(gpui::relative(0.5))
                    }
                    Some(DockSide::Top) => {
                        overlay.top_0().left_0().right_0().h(gpui::relative(0.5))
                    }
                    Some(DockSide::Bottom) => {
                        overlay.bottom_0().left_0().right_0().h(gpui::relative(0.5))
                    }
                    None => overlay.inset(px(8.0)),
                };
                body.child(overlay)
            } else {
                body
            }
        } else {
            body
        };
        let container = div()
            .id(("pane", pane_id))
            .overflow_hidden()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .min_h_0();
        let container = container
            .children((!self.focus_mode).then_some(header))
            .children(skill_upgrade.filter(|_| !self.focus_mode).map(|shell_id| {
                div()
                    .h(ui_text::space(23.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .px(ui_text::space(10.0))
                    .bg(rgb(colors.panel_active))
                    .border_b_1()
                    .border_color(rgb(colors.divider))
                    .text_color(rgb(colors.muted))
                    .child(ui_text::cased("Orchestrator skill update available"))
                    .child(div().flex_1())
                    .child(
                        div()
                            .id(("load-orchestrator-skill", pane_id))
                            // The colorful themes call it out; Native's link is the primary color.
                            .text_color(rgb(if ui_text::is_native() {
                                colors.cyan
                            } else {
                                colors.gold
                            }))
                            .cursor_pointer()
                            .child(ui_text::cased("Load skill"))
                            .on_click(cx.listener(move |workspace, _, _, cx| {
                                match workspace.sessions.load_orchestrator_skill(&shell_id) {
                                    Ok(_) => {
                                        workspace.notice = None;
                                        if let Ok(shells) = workspace.sessions.list() {
                                            workspace.shells = shells;
                                        }
                                    }
                                    Err(error) => workspace.notice = Some(error),
                                }
                                cx.notify();
                            })),
                    )
            }))
            .child(body);
        container
            .children(
                (!self.focus_mode && self.panel_menu == Some(pane_id)).then(|| {
                    let menu = div()
                        .id(("view-menu", pane_id))
                        .absolute()
                        .top(ui_text::space(PANE_HEADER_HEIGHT) + px(2.0))
                        .right(px(6.0))
                        .w(px(
                            ui_text::space_f32(248.0).min((pane_width - 12.0).max(0.0))
                        ))
                        .max_h(gpui::relative(0.9))
                        .overflow_y_scroll()
                        .bg(rgb(colors.panel_active))
                        .border_1()
                        .border_color(rgb(colors.magenta))
                        .p(ui_text::space(3.0))
                        .on_mouse_down_out(cx.listener(|workspace, _, window, cx| {
                            workspace.panel_menu = None;
                            workspace.finish_tab_drag(cx);
                            workspace.focus_active(window, cx);
                            cx.notify();
                        }));
                    menu.child(pane_menu_heading("New tab", colors))
                        .children(
                            [
                                ("Shell", "⌘T", Some(Icon::Add), PaneMenuAction::Shell),
                                (
                                    "Codex",
                                    "⌘⇧C",
                                    None,
                                    PaneMenuAction::Harness(HarnessKind::Codex, false),
                                ),
                                (
                                    "Claude",
                                    "⌘⇧L",
                                    None,
                                    PaneMenuAction::Harness(HarnessKind::Claude, false),
                                ),
                                (
                                    "Grok",
                                    "⌘⇧G",
                                    None,
                                    PaneMenuAction::Harness(HarnessKind::Grok, false),
                                ),
                                (
                                    "Codex · unrestricted",
                                    "",
                                    None,
                                    PaneMenuAction::Harness(HarnessKind::Codex, true),
                                ),
                                (
                                    "Claude · unrestricted",
                                    "",
                                    None,
                                    PaneMenuAction::Harness(HarnessKind::Claude, true),
                                ),
                                (
                                    "Grok · unrestricted",
                                    "",
                                    None,
                                    PaneMenuAction::Harness(HarnessKind::Grok, true),
                                ),
                                (
                                    "Global orchestrator",
                                    "⌘⇧O",
                                    None,
                                    PaneMenuAction::Orchestrator(false),
                                ),
                                (
                                    "Project orchestrator",
                                    "⌘⌥O",
                                    None,
                                    PaneMenuAction::Orchestrator(true),
                                ),
                            ]
                            .into_iter()
                            .map(|(label, shortcut, icon, action)| {
                                self.pane_menu_row(pane_id, label, shortcut, icon, action, cx)
                            }),
                        )
                        .child(pane_menu_heading("Views", colors))
                        .children(
                            [
                                PanelKind::Projects,
                                PanelKind::Files,
                                PanelKind::Preview,
                                PanelKind::Worktrees,
                                PanelKind::Tasks,
                                PanelKind::Shells,
                                PanelKind::Usage,
                                PanelKind::Schedules,
                                PanelKind::ProjectSettings,
                                PanelKind::Settings,
                            ]
                            .into_iter()
                            .map(|kind| {
                                self.pane_menu_row(
                                    pane_id,
                                    Self::panel_title(kind),
                                    match kind {
                                        PanelKind::Files => "⌘⇧E",
                                        PanelKind::Preview => "⌘⇧P",
                                        PanelKind::Settings => "⌘,",
                                        PanelKind::Schedules => "⌘⇧S",
                                        _ => "",
                                    },
                                    None,
                                    PaneMenuAction::View(kind),
                                    cx,
                                )
                            }),
                        )
                        .child(pane_menu_heading("Pane", colors))
                        .children(
                            [
                                (
                                    "Split right",
                                    "⌘D",
                                    Icon::SplitRight,
                                    PaneMenuAction::Split(Axis::SideBySide),
                                ),
                                (
                                    "Split down",
                                    "⌘⇧D",
                                    Icon::SplitDown,
                                    PaneMenuAction::Split(Axis::Stacked),
                                ),
                                ("Close pane", "⌘⇧W", Icon::Close, PaneMenuAction::Close),
                            ]
                            .into_iter()
                            // Menus here have no disabled rows, so a locked pane just omits Close pane.
                            .filter(|(_, _, _, action)| {
                                !matches!(action, PaneMenuAction::Close)
                                    || user_close_refusal(pane_locked, UserClose::Pane).is_none()
                            })
                            .map(|(label, shortcut, icon, action)| {
                                self.pane_menu_row(pane_id, label, shortcut, Some(icon), action, cx)
                            }),
                        )
                        .child(self.pane_menu_row(
                            pane_id,
                            "Main pane",
                            "",
                            is_main.then_some(Icon::Check),
                            PaneMenuAction::Main,
                            cx,
                        ))
                        .children(has_main.then(|| {
                            self.pane_menu_row(
                                pane_id,
                                "Gather tabs into main pane",
                                "⌘⇧M",
                                None,
                                PaneMenuAction::Gather,
                                cx,
                            )
                        }))
                        .children((!show_lock).then(|| {
                            self.pane_menu_row(
                                pane_id,
                                if pane_locked {
                                    "Unlock pane"
                                } else {
                                    "Lock pane"
                                },
                                "",
                                Some(if pane_locked {
                                    Icon::Lock
                                } else {
                                    Icon::Unlock
                                }),
                                PaneMenuAction::Lock,
                                cx,
                            )
                        }))
                        .children((!show_focus).then(|| {
                            self.pane_menu_row(
                                pane_id,
                                "Focus tab",
                                "⌘⇧F",
                                Some(Icon::Focus),
                                PaneMenuAction::Focus,
                                cx,
                            )
                        }))
                }),
            )
            .into_any_element()
    }

    fn render_status(&self, cx: &mut Context<Self>) -> AnyElement {
        use status_bar::StatusSide;
        let colors = theme::palette(cx);
        let settings = &self.settings.status_bar;
        // Native parts its items by space alone, and a little more of it.
        let status_gap = if ui_text::is_native() { 16.0 } else { 10.0 };
        div()
            .id("status-items")
            .size_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap(ui_text::space(8.0))
            .px(ui_text::space(if ui_text::is_native() {
                12.0
            } else {
                8.0
            }))
            .text_size(ui_text::text(9.0))
            .text_color(rgb(colors.muted))
            // Items keep one line; at larger text the right side gives way first and
            // the left (the project, by default) keeps up to 40 % of the bar, or all
            // of it when nothing sits on the right.
            .whitespace_nowrap()
            .child(
                div()
                    .id("status-left")
                    .flex()
                    .flex_shrink_0()
                    .max_w(gpui::relative(
                        if settings.visible_items(StatusSide::Right).is_empty() {
                            1.0
                        } else {
                            0.4
                        },
                    ))
                    .min_w_0()
                    .items_center()
                    .gap(ui_text::space(status_gap))
                    .overflow_x_scroll()
                    .children(
                        settings
                            .visible_items(StatusSide::Left)
                            .into_iter()
                            .map(|kind| self.render_status_item(kind, cx)),
                    ),
            )
            .child(div().flex_1().min_w(ui_text::space(4.0)))
            .child(
                div()
                    .id("status-right")
                    .flex()
                    .min_w_0()
                    .items_center()
                    .gap(ui_text::space(status_gap))
                    .overflow_x_scroll()
                    .children(
                        settings
                            .visible_items(StatusSide::Right)
                            .into_iter()
                            .map(|kind| self.render_status_item(kind, cx)),
                    ),
            )
            .into_any_element()
    }

    fn render_status_item(
        &self,
        kind: status_bar::StatusItemKind,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use status_bar::StatusItemKind;
        let colors = theme::palette(cx);
        match kind {
            StatusItemKind::Project => {
                let name = self
                    .project_display_name(cx)
                    .unwrap_or_else(|| "Project unavailable".to_owned());
                div()
                    .id("status-current-project")
                    .max_w(ui_text::space(240.0))
                    .min_w_0()
                    .text_ellipsis()
                    .overflow_hidden()
                    .text_color(rgb(colors.cyan))
                    .cursor_pointer()
                    .child(name)
                    .on_click(cx.listener(|workspace, _, window, cx| {
                        workspace.open_panel(
                            PanelKind::ProjectSettings,
                            workspace.active_pane,
                            window,
                            cx,
                        );
                    }))
                    .into_any_element()
            }
            StatusItemKind::Worktree => {
                let branch = match self.remote_project() {
                    Some((host, project)) => self.selected_worktree_id.as_deref().and_then(|id| {
                        cx.global::<RemoteState>()
                            .tree()
                            .worktree_branch(host, project, id)
                            .map(str::to_owned)
                    }),
                    None => self
                        .selected_worktree_id
                        .as_ref()
                        .and_then(|id| {
                            self.state.worktrees.iter().find(|worktree| {
                                &worktree.id == id && worktree.project_id == self.project_id
                            })
                        })
                        .map(|worktree| worktree.branch.clone()),
                }
                .unwrap_or_else(|| "No worktree".to_owned());
                div()
                    .id("status-current-worktree")
                    .max_w(ui_text::space(220.0))
                    .min_w_0()
                    .font_family(ui_text::mono_family())
                    .text_ellipsis()
                    .overflow_hidden()
                    .cursor_pointer()
                    .child(branch)
                    .on_click(cx.listener(|workspace, _, window, cx| {
                        workspace.open_panel(
                            PanelKind::Worktrees,
                            workspace.active_pane,
                            window,
                            cx,
                        );
                    }))
                    .into_any_element()
            }
            // Agent activity and resource use are read from this Mac's own sessions; another
            // Mac's are not sampled, so these say so rather than show the local project's.
            StatusItemKind::AgentActivity if self.is_remote() => div()
                .flex_none()
                .text_color(rgb(colors.muted))
                .child("Agents · —")
                .into_any_element(),
            StatusItemKind::Resources if self.is_remote() => div()
                .flex_none()
                .text_color(rgb(colors.muted))
                .child("CPU — RAM —")
                .into_any_element(),
            StatusItemKind::Usage | StatusItemKind::CodexAccount if self.is_remote() => {
                div().into_any_element()
            }
            StatusItemKind::LiveSessions if self.is_remote() => {
                let live = self.remote_project().and_then(|(host, project)| {
                    cx.global::<RemoteState>().tree().live_shells(host, project)
                });
                div()
                    .flex_none()
                    .child(ui_text::quiet(format!(
                        "{} LIVE",
                        live.map_or("—".to_owned(), |live| live.to_string())
                    )))
                    .into_any_element()
            }
            StatusItemKind::AgentActivity => {
                let counts = activity::ActivityCounts::for_project(
                    &self.project_id,
                    &self.shells,
                    &self.agent_activity,
                );
                div()
                    .id("status-agent-activity")
                    .max_w(ui_text::space(250.0))
                    .min_w_0()
                    .text_ellipsis()
                    .overflow_hidden()
                    .cursor_pointer()
                    .text_color(rgb(if counts.working > 0 {
                        colors.working
                    } else {
                        colors.muted
                    }))
                    .child(counts.summary().unwrap_or_else(|| "Agents · —".to_owned()))
                    .on_click(cx.listener(|workspace, _, window, cx| {
                        workspace.open_panel(PanelKind::Shells, workspace.active_pane, window, cx);
                    }))
                    .into_any_element()
            }
            StatusItemKind::LiveSessions => div()
                .flex_none()
                .font_family(ui_text::mono_family())
                .child(ui_text::quiet(format!(
                    "{} LIVE",
                    self.shells
                        .iter()
                        .filter(
                            |shell| shell.project_id.as_deref() == Some(&self.project_id)
                                && shell.alive
                        )
                        .count()
                )))
                .into_any_element(),
            StatusItemKind::Resources => {
                let (cpu, ram) = self
                    .shells
                    .iter()
                    .filter(|shell| {
                        shell.project_id.as_deref() == Some(&self.project_id) && shell.alive
                    })
                    .fold((0.0_f32, 0_u64), |(cpu, ram), shell| {
                        let metrics = self.metrics.get(&shell.id).copied().unwrap_or_default();
                        (cpu + metrics.cpu_percent, ram + metrics.ram_bytes)
                    });
                div()
                    .flex_none()
                    .font_family(ui_text::mono_family())
                    .child(format!("CPU {cpu:.1}% RAM {}", format_bytes(ram)))
                    .into_any_element()
            }
            StatusItemKind::Usage => self.render_usage_chip(cx),
            StatusItemKind::CodexAccount => self.render_codex_account_status(cx),
            StatusItemKind::SessionId => {
                let id = self
                    .panes
                    .get(&self.active_pane)
                    .and_then(|pane| pane.tabs.get(pane.active))
                    .and_then(|tab| {
                        tab.shell_id()
                            .or_else(|| tab.remote().map(|(_, shell)| shell))
                    })
                    .map(str::to_owned);
                div()
                    .id("copy-active-shell-id")
                    .flex_none()
                    .font_family(ui_text::mono_family())
                    .text_color(rgb(status_accent(colors.cyan, colors)))
                    .when_some(id, |item, id| {
                        item.cursor_pointer()
                            .child(id.chars().take(8).collect::<String>())
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(id.clone()))
                            }))
                    })
                    .into_any_element()
            }
            StatusItemKind::GlobalOrchestrator => div()
                .id("top-orchestrator")
                .flex_none()
                .text_color(rgb(status_accent(colors.magenta, colors)))
                .cursor_pointer()
                .child(ui_text::quiet("G·ORCH"))
                .on_click(
                    cx.listener(|workspace, _, window, cx| workspace.open_orchestrator(window, cx)),
                )
                .into_any_element(),
            StatusItemKind::ProjectOrchestrator => div()
                .id("project-orchestrator")
                .flex_none()
                .text_color(rgb(status_accent(colors.cyan, colors)))
                .cursor_pointer()
                .child(ui_text::quiet("P·ORCH"))
                .on_click(cx.listener(|workspace, _, window, cx| {
                    workspace.open_scoped_orchestrator(
                        Some(workspace.project_id.clone()),
                        window,
                        cx,
                    );
                }))
                .into_any_element(),
            StatusItemKind::Layout => self.render_layout_status(cx),
        }
    }

    /// The layout menu's place in the status bar: its icon, and the word LAYOUT beside it with
    /// icons instead of labels off. A click opens the menu above it, and another closes it.
    fn render_layout_status(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = theme::palette(cx);
        let open = self.layout_menu_open;
        let color = if open { colors.cyan } else { colors.muted };
        let bounds = self.layout_item_bounds.clone();
        div()
            .id("status-layout")
            .relative()
            .flex_none()
            .flex()
            .items_center()
            .gap(ui_text::space(4.0))
            .px(ui_text::space(4.0))
            .py(ui_text::space(1.0))
            .cursor_pointer()
            .text_color(rgb(color))
            .when(open, |item| item.bg(rgb(colors.panel_active)))
            .hover(|style| {
                style
                    .bg(rgb(colors.panel_active))
                    .text_color(rgb(colors.cyan))
            })
            .child(icons::icon(Icon::Layout, color))
            .when(ui_text::is_native(), |item| {
                item.rounded_full().px(ui_text::space(6.0))
            })
            .children(
                (!self.settings.panel_tab_icons).then(|| div().child(ui_text::cased("Layout"))),
            )
            .child(
                canvas(move |area, _, _| bounds.set(area), |_, _, _, _| {})
                    .absolute()
                    .inset_0(),
            )
            .when(!open, |item| {
                item.child(tooltip::anchor(
                    "Layout menu · default layout ⌘⌥L",
                    Look::Status,
                ))
            })
            .on_click(cx.listener(|workspace, _, window, cx| {
                workspace.toggle_layout_menu(window, cx);
            }))
            .into_any_element()
    }

    /// The layout menu, above the status bar at its layout item: the default layout, a gather
    /// into the main pane, and which pane is the main one.
    fn render_layout_menu(&self, window_width: f32, cx: &mut Context<Self>) -> AnyElement {
        let colors = theme::palette(cx);
        let width = ui_text::space_f32(296.0).min((window_width - 12.0).max(0.0));
        let item = self.layout_item_bounds.get();
        let left = layout_menu_left(
            item.right().as_f32(),
            item.size.width.as_f32(),
            width,
            window_width,
        );
        let main = self.main_pane().and_then(|id| self.panes.get(&id));
        let rows = [
            LayoutMenuRow {
                id: "layout-menu-default",
                icon: Some(Icon::Layout),
                label: "Default layout".to_owned(),
                shortcut: "⌘⌥L",
                detail: Some("Locked navigation left, everything else right"),
                action: Some(LayoutMenuAction::Default),
            },
            LayoutMenuRow {
                id: "layout-menu-gather",
                icon: None,
                label: "Gather into main pane".to_owned(),
                shortcut: "⌘⇧M",
                detail: Some(if main.is_some() {
                    "Unlocked panes' tabs move into it"
                } else {
                    "Needs a main pane: pane … menu → Main pane"
                }),
                action: main.map(|_| LayoutMenuAction::Gather),
            },
            LayoutMenuRow {
                id: "layout-menu-main",
                icon: Some(Icon::Main),
                label: format!("Main pane: {}", main_pane_summary(main)),
                shortcut: "",
                detail: main
                    .is_none()
                    .then_some("Choose Main pane in a pane's … menu"),
                action: None,
            },
        ];
        div()
            .id("layout-menu")
            .absolute()
            .left(px(left))
            .bottom(ui_text::space(STATUS_BAR_HEIGHT) + px(3.0))
            .w(px(width))
            .bg(rgb(colors.panel_active))
            .border_1()
            .border_color(rgb(colors.magenta))
            .p(ui_text::space(3.0))
            .occlude()
            .on_mouse_down_out(cx.listener(
                |workspace, event: &gpui::MouseDownEvent, window, cx| {
                    // A click on the item itself is its own toggle.
                    if !workspace.layout_item_bounds.get().contains(&event.position) {
                        workspace.close_layout_menu(window, cx);
                    }
                },
            ))
            .children(rows.into_iter().map(|row| self.layout_menu_row(row, cx)))
            .into_any_element()
    }

    /// A row has no hover hint, as the rows of the other menus have none: a hint is a window of
    /// its own that opens below the row and would cover the next one.
    fn layout_menu_row(&self, row: LayoutMenuRow, cx: &mut Context<Self>) -> AnyElement {
        let colors = theme::palette(cx);
        let enabled = row.action.is_some();
        div()
            .id(row.id)
            .flex()
            .flex_col()
            .gap(ui_text::space(2.0))
            .px(ui_text::space(8.0))
            .py(ui_text::space(5.0))
            .text_size(ui_text::text(10.0))
            .text_color(rgb(if enabled { colors.text } else { colors.muted }))
            .when(enabled, |item| {
                item.cursor_pointer()
                    .hover(|style| style.bg(rgb(colors.divider)).text_color(rgb(colors.cyan)))
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(ui_text::space(8.0))
                    .child(
                        div()
                            .w(ui_text::space(14.0))
                            .flex_none()
                            .children(row.icon.map(|icon| icons::icon(icon, colors.muted))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(row.label),
                    )
                    .children((!row.shortcut.is_empty()).then(|| {
                        div()
                            .flex_none()
                            .text_size(ui_text::text(9.0))
                            .text_color(rgb(colors.muted))
                            .child(row.shortcut)
                    })),
            )
            .children(row.detail.map(|detail| {
                div()
                    .pl(ui_text::space(22.0))
                    .text_size(ui_text::text(9.0))
                    .text_color(rgb(colors.muted))
                    .child(detail)
            }))
            .when_some(row.action, |item, action| {
                item.on_click(cx.listener(move |workspace, _, window, cx| {
                    workspace.layout_menu_open = false;
                    match action {
                        LayoutMenuAction::Default => workspace.apply_default_layout(window, cx),
                        LayoutMenuAction::Gather => {
                            workspace.gather_tabs(window, cx);
                            workspace.focus_active(window, cx);
                        }
                    }
                }))
            })
            .into_any_element()
    }

    fn render_codex_account_status(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = theme::palette(cx);
        let snapshot = cx
            .global::<settings::CodexAccountsState>()
            .snapshot
            .as_ref();
        let active = self
            .panes
            .get(&self.active_pane)
            .and_then(|pane| pane.tabs.get(pane.active))
            .and_then(Tab::shell_id)
            .and_then(|id| self.shells.iter().find(|shell| shell.id == id))
            .filter(|shell| shell.harness == Some(HarnessKind::Codex));
        let label = if let Some(shell) = active {
            codex_session_status_label(shell, &codex_account_numbers(&self.shells), snapshot)
        } else {
            project_default_account_label(
                self.state
                    .projects
                    .iter()
                    .find(|project| project.id == self.project_id),
                self.settings.selected_codex_account.as_deref(),
                snapshot,
            )
        };
        div()
            .id("status-codex-account")
            .max_w(ui_text::space(300.0))
            .min_w_0()
            .overflow_hidden()
            .text_ellipsis()
            .text_color(rgb(status_accent(colors.cyan, colors)))
            .cursor_pointer()
            .child(ui_text::quiet(label))
            .on_click(cx.listener(|workspace, _, window, cx| {
                workspace.open_panel(
                    PanelKind::ProjectSettings,
                    workspace.active_pane,
                    window,
                    cx,
                );
            }))
            .into_any_element()
    }

    fn render_usage_chip(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = theme::palette(cx);
        let active_shell = self.active_shell();
        let cache = cx.global::<AccountUsage>();
        let label = if let Some(shell) =
            active_shell.filter(|shell| shell.harness == Some(HarnessKind::Claude))
        {
            self.claude_usage
                .get(&shell.id)
                .map(|snapshot| usage_summary("CLAUDE", snapshot))
                .unwrap_or_else(|| "CLAUDE · WAITING".to_owned())
        } else if let Some(shell) =
            active_shell.filter(|shell| shell.harness == Some(HarnessKind::Grok))
        {
            // Grok's allowance is not readable, so the chip shows this session.
            if shell.alive {
                usage::grok_chip_label(self.grok_usage.get(&shell.id))
            } else {
                "GROK · EXITED".to_owned()
            }
        } else {
            let profile = if let Some(shell) =
                active_shell.filter(|shell| shell.harness == Some(HarnessKind::Codex))
            {
                shell.codex_home.clone()
            } else {
                project_selected_codex_home(self, cx)
            };
            let entry = profile.as_ref().and_then(|home| cache.codex.get(home));
            entry
                .and_then(|entry| entry.codex.as_ref())
                .map(|snapshot| usage_summary("CODEX", snapshot))
                .unwrap_or_else(|| {
                    if profile.is_none() {
                        "CODEX · ACCOUNT UNKNOWN"
                    } else if entry.is_some_and(|entry| entry.pending) {
                        "USAGE · LOADING"
                    } else {
                        "USAGE · —"
                    }
                    .to_owned()
                })
        };
        div()
            .id("usage-chip")
            .max_w(ui_text::space(220.0))
            .overflow_hidden()
            .text_ellipsis()
            .text_color(rgb(status_accent(colors.cyan, colors)))
            .cursor_pointer()
            .hover(|style| style.text_color(rgb(colors.magenta)))
            .child(ui_text::quiet(label))
            .on_click(cx.listener(|workspace, _, window, cx| {
                workspace.open_panel(PanelKind::Usage, workspace.active_pane, window, cx);
            }))
            .into_any_element()
    }

    fn render_usage_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = theme::palette(cx);
        let cache = cx.global::<AccountUsage>();
        let pending = cache.codex.values().any(|entry| entry.pending);
        let mut cards = Vec::new();
        let mut profiles = codex_usage_profiles(cx);
        for shell in self
            .shells
            .iter()
            .filter(|shell| shell.harness == Some(HarnessKind::Codex))
        {
            if let Some(home) = &shell.codex_home {
                profiles.entry(home.clone()).or_insert_with(|| {
                    shell
                        .codex_account_label
                        .clone()
                        .unwrap_or_else(|| "Session account".to_owned())
                });
            }
        }
        for (home, label) in profiles {
            let entry = cache.codex.get(&home);
            let title = format!("{} · {label}", ui_text::cased("Codex"));
            if let Some(snapshot) = entry.and_then(|entry| entry.codex.as_ref()) {
                cards.push(render_provider_usage(snapshot, &title, colors));
            } else {
                cards.push(
                    div()
                        .p(ui_text::space(12.0))
                        .border_t_1()
                        .border_color(rgb(colors.divider))
                        .child(title)
                        .child(
                            div()
                                .mt(ui_text::space(6.0))
                                .text_color(rgb(colors.muted))
                                .child(if entry.is_some_and(|entry| entry.pending) {
                                    "Reading account usage…".to_owned()
                                } else {
                                    entry
                                        .and_then(|entry| entry.codex_error.clone())
                                        .unwrap_or_else(|| {
                                            "Account usage is unavailable".to_owned()
                                        })
                                }),
                        )
                        .into_any_element(),
                );
            }
            if let Some(error) = entry
                .filter(|entry| entry.codex.is_some())
                .and_then(|entry| entry.codex_error.as_ref())
            {
                cards.push(
                    div()
                        .px(ui_text::space(12.0))
                        .text_color(rgb(colors.gold))
                        .child(format!("Last refresh: {error}"))
                        .into_any_element(),
                );
            }
        }
        let claude_shells = self.shells.iter().filter(|shell| {
            shell.project_id.as_deref() == Some(self.project_id.as_str())
                && shell.harness == Some(HarnessKind::Claude)
        });
        let mut has_claude = false;
        for shell in claude_shells {
            has_claude = true;
            let title = format!("{} · {}", ui_text::cased("Claude"), &shell.id[..8]);
            if let Some(snapshot) = self.claude_usage.get(&shell.id) {
                cards.push(render_provider_usage(snapshot, &title, colors));
            } else {
                cards.push(
                    div()
                        .p(ui_text::space(12.0))
                        .border_t_1()
                        .border_color(rgb(colors.divider))
                        .child(title)
                        .child(
                            div()
                                .mt(ui_text::space(6.0))
                                .text_color(rgb(colors.muted))
                                .child("Waiting for Claude usage after its first response"),
                        )
                        .into_any_element(),
                );
            }
        }
        if !has_claude {
            cards.push(
                div()
                    .p(ui_text::space(12.0))
                    .text_color(rgb(colors.muted))
                    .child("Open a Claude session to see its usage here")
                    .into_any_element(),
            );
        }
        cards.extend(self.render_grok_usage_cards(colors));
        div().size_full().flex().flex_col().min_h_0().bg(rgb(colors.panel))
            .child(div().h(ui_text::space(32.0)).flex_none().flex().items_center().px(ui_text::space(10.0)).justify_between()
                .border_b_1().border_color(rgb(colors.divider)).child(ui_text::cased("Account usage"))
                .child(div().id("refresh-account-usage").text_color(rgb(colors.cyan)).cursor_pointer()
                    .child(ui_text::cased(if pending || self.grok_usage_pending { "Refreshing…" } else { "↻ Refresh" }))
                    .on_click(cx.listener(|workspace, _, window, cx| {
                        workspace.refresh_grok_usage(true, cx);
                        request_codex_usage(true, cx);
                        for home in workspace.shells.iter()
                            .filter(|shell| shell.harness == Some(HarnessKind::Codex))
                            .filter_map(|shell| shell.codex_home.clone()).collect::<HashSet<_>>() {
                            request_codex_usage_at(home, true, cx);
                        }
                        workspace.refresh(window, cx);
                    }))))
            .child(div().id("usage-panel-scroll").flex_1().min_h_0().overflow_y_scroll().children(cards))
            .child(div().flex_none().p(ui_text::space(10.0)).border_t_1().border_color(rgb(colors.divider)).text_color(rgb(colors.muted)).text_size(ui_text::text(9.0))
                .child("Quota is shared by all sessions on the same account. Missing windows are unavailable. Claude subscription quota requires a supported Pro/Max account. Grok shows each session's tokens and cost only."))
            .into_any_element()
    }

    /// Grok's section of the Usage panel: one card per live Grok tab of the
    /// project, their combined total, and where the account allowance is.
    fn render_grok_usage_cards(&self, colors: Palette) -> Vec<AnyElement> {
        let shells = self.shells.iter().filter(|shell| {
            shell.alive
                && shell.harness == Some(HarnessKind::Grok)
                && shell.project_id.as_deref() == Some(self.project_id.as_str())
        });
        let mut cards = Vec::new();
        let mut usages = Vec::new();
        let mut listed = 0;
        for shell in shells {
            listed += 1;
            let title = self
                .panes
                .values()
                .flat_map(|pane| pane.tabs.iter())
                .find(|tab| tab.shell_id() == Some(shell.id.as_str()))
                .map(|tab| ui_text::cased(tab.title.clone()).to_string())
                .unwrap_or_else(|| format!("{} · {}", ui_text::cased("Grok"), &shell.id[..8]));
            let tab = self.grok_usage.get(&shell.id);
            usages.extend(tab.and_then(|tab| tab.usage.as_ref()));
            cards.push(render_grok_session(&title, tab, colors));
        }
        if listed == 0 {
            cards.push(
                div()
                    .p(ui_text::space(12.0))
                    .border_t_1()
                    .border_color(rgb(colors.divider))
                    .text_color(rgb(colors.muted))
                    .child("No live Grok sessions")
                    .into_any_element(),
            );
        } else if !usages.is_empty() {
            cards.push(render_grok_total(
                &usage::grok_totals(usages.iter().copied()),
                listed - usages.len(),
                colors,
            ));
        }
        cards.push(
            div()
                .px(ui_text::space(12.0))
                .pb(ui_text::space(12.0))
                .pt(ui_text::space(8.0))
                .border_t_1()
                .border_color(rgb(colors.divider))
                .text_color(rgb(colors.muted))
                .text_size(ui_text::text(9.0))
                .child("Grok's account allowance and credits are shown only in Grok's own /usage screen. RiWork cannot read them through an official interface.")
                .into_any_element(),
        );
        cards
    }

    fn render_focus(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = theme::palette(cx);
        let show_window_controls = window_controls_visible(window);
        let viewport = window.viewport_size();
        let width = viewport.width.as_f32();
        let height = viewport.height.as_f32();
        let title = self
            .panes
            .get(&self.active_pane)
            .and_then(|pane| pane.tabs.get(pane.active))
            .map(|tab| tab.title.clone())
            .unwrap_or_default();
        let centered = self.focus_centered;
        let toolbar = div()
            .id("focus-toolbar")
            .flex()
            .flex_none()
            .items_center()
            .h(ui_text::space(FOCUS_TOOLBAR_HEIGHT))
            .pl(px(if show_window_controls {
                WINDOW_CONTROLS_CONTENT_INSET
            } else {
                12.0
            }))
            .pr(ui_text::space(12.0))
            .gap(ui_text::space(12.0))
            .bg(rgb(colors.panel))
            .border_b_1()
            .border_color(rgb(colors.divider))
            .child(
                div()
                    .id("focus-window-drag-space")
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .items_center()
                    .gap(ui_text::space(12.0))
                    .when(show_window_controls, |space| {
                        space
                            .cursor_grab()
                            .on_mouse_down(MouseButton::Left, start_window_drag)
                    })
                    .children((width >= 720.0).then(|| {
                        div()
                            .flex_none()
                            .text_color(rgb(colors.cyan))
                            .child(ui_text::cased(if centered {
                                "Center focus"
                            } else {
                                "Focus"
                            }))
                    }))
                    .child(
                        div()
                            .text_ellipsis()
                            .text_color(rgb(colors.muted))
                            .child(title),
                    ),
            )
            .child(
                div()
                    .id("focus-layout-toggle")
                    .flex_none()
                    .px(ui_text::space(8.0))
                    .py(ui_text::space(5.0))
                    .cursor_pointer()
                    .text_color(rgb(colors.muted))
                    .hover(|style| {
                        style
                            .bg(rgb(colors.panel_active))
                            .text_color(rgb(colors.cyan))
                    })
                    .child(if centered {
                        "⛶ FILL WINDOW"
                    } else {
                        "⊙ CENTER FOCUS"
                    })
                    .on_click(cx.listener(|workspace, _, window, cx| {
                        workspace.focus_centered = !workspace.focus_centered;
                        workspace.focus_active(window, cx);
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .id("restore-workspace")
                    .flex_none()
                    .px(ui_text::space(10.0))
                    .py(ui_text::space(5.0))
                    .border_1()
                    .border_color(rgb(colors.divider))
                    .bg(rgb(colors.panel_active))
                    .text_color(rgb(colors.cyan))
                    .cursor_pointer()
                    .hover(|style| style.border_color(rgb(colors.cyan)))
                    .child(if width >= 600.0 {
                        "↙ RESTORE  ⌘⇧F"
                    } else {
                        "↙ RESTORE"
                    })
                    .on_click(cx.listener(|workspace, _, window, cx| {
                        workspace.set_focus_mode(false, window, cx);
                    })),
            );
        let content = if centered {
            let toolbar_height = ui_text::space_f32(FOCUS_TOOLBAR_HEIGHT);
            let top_gap = 20.0_f32.min((height - toolbar_height).max(0.0));
            let content_width = (width - 48.0).max(0.0).min(FOCUS_MAX_WIDTH);
            let content_height =
                (height * (1.0 - FOCUS_BOTTOM_MARGIN) - toolbar_height - top_gap).max(0.0);
            div()
                .flex()
                .flex_none()
                .justify_center()
                .w_full()
                .h(px(content_height))
                .mt(px(top_gap))
                .child(
                    div()
                        .w(px(content_width))
                        .h_full()
                        .border_1()
                        .border_color(rgb(colors.divider))
                        .child(self.render_pane(
                            self.active_pane,
                            content_width - 2.0,
                            0.0,
                            false,
                            cx,
                        )),
                )
                .into_any_element()
        } else {
            div()
                .flex()
                .flex_1()
                .min_h_0()
                .child(self.render_pane(self.active_pane, width, 0.0, false, cx))
                .into_any_element()
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(toolbar)
            .child(content)
            .into_any_element()
    }

    fn pane_button(
        &self,
        pane_id: PaneId,
        key: &'static str,
        icon: Icon,
        color: u32,
        action: impl Fn(&mut Self, PaneId, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme::palette(cx);
        div()
            .id(format!("pane-{pane_id}-{key}"))
            .h_full()
            .w(ui_text::space(28.0))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .when(
                key == "menu" && self.panel_menu == Some(pane_id),
                |button| button.bg(rgb(colors.divider)),
            )
            .hover(|style| style.bg(rgb(colors.divider)))
            .map(|button| controls::native(button, toolbar_button))
            .child(icons::icon(icon, color))
            .when(
                key != "menu" || self.panel_menu != Some(pane_id),
                |button| {
                    let label = match key {
                        "lock" if self.pane_is_locked(pane_id) => {
                            "Locked across project switches, tabs can't close · click to unlock"
                        }
                        "lock" => "Lock across project switches and keep tabs open",
                        "focus" => "Focus this tab · ⌘⇧F",
                        _ => "Add tabs and manage this pane",
                    };
                    button.child(tooltip::anchor(label, Look::Pane))
                },
            )
            .on_click(
                cx.listener(move |workspace, _, window, cx| action(workspace, pane_id, window, cx)),
            )
            .into_any_element()
    }

    /// The mark on the main pane's header: a star, or the word MAIN with icons off, in the colour
    /// a pane lock has when it is on. Clicking it makes the pane an ordinary one again.
    fn main_marker(&self, pane_id: PaneId, cx: &mut Context<Self>) -> AnyElement {
        let colors = theme::palette(cx);
        let marker = div()
            .id(format!("pane-{pane_id}-main"))
            .h_full()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|style| style.bg(rgb(colors.divider)))
            .map(|marker| controls::native(marker, toolbar_button));
        let marker = if self.settings.panel_tab_icons {
            marker
                .w(ui_text::space(28.0))
                .child(icons::icon(Icon::Main, colors.cyan))
        } else {
            marker
                .px(ui_text::space(6.0))
                .text_size(ui_text::text(9.0))
                .text_color(rgb(colors.cyan))
                .min_w(ui_text::space(0.0))
                .px(ui_text::space(8.0))
                .child(ui_text::cased("Main"))
        };
        marker
            .child(tooltip::anchor(
                "Main pane: new tabs, clicked shells and previews open here · click to unset",
                Look::Pane,
            ))
            .on_click(cx.listener(move |workspace, _, _, cx| {
                workspace.toggle_main_pane(pane_id, cx);
            }))
            .into_any_element()
    }

    fn pane_menu_row(
        &self,
        pane_id: PaneId,
        label: &'static str,
        shortcut: &'static str,
        icon: Option<Icon>,
        action: PaneMenuAction,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme::palette(cx);
        div()
            .id(format!("pane-menu-{pane_id}-{label}"))
            .flex()
            .items_center()
            .gap(ui_text::space(8.0))
            .px(ui_text::space(8.0))
            .py(ui_text::space(5.0))
            .text_size(ui_text::text(10.0))
            .text_color(rgb(colors.text))
            .cursor_pointer()
            .hover(|style| style.bg(rgb(colors.divider)).text_color(rgb(colors.cyan)))
            .child(
                div()
                    .w(ui_text::space(14.0))
                    .flex_none()
                    .children(icon.map(|icon| {
                        icons::icon(
                            icon,
                            if matches!(icon, Icon::Check) {
                                colors.cyan
                            } else {
                                colors.muted
                            },
                        )
                    })),
            )
            .child(div().flex_1().min_w_0().text_ellipsis().child(label))
            .children((!shortcut.is_empty()).then(|| {
                div()
                    .flex_none()
                    .text_size(ui_text::text(9.0))
                    .text_color(rgb(colors.muted))
                    .child(shortcut)
            }))
            .on_click(cx.listener(move |workspace, _, window, cx| {
                workspace.panel_menu = None;
                workspace.finish_tab_drag(cx);
                workspace.active_pane = pane_id;
                match action {
                    PaneMenuAction::Shell => workspace.add_tab(window, cx),
                    PaneMenuAction::Harness(kind, unrestricted) => {
                        workspace.add_harness(kind, unrestricted, window, cx);
                    }
                    PaneMenuAction::Orchestrator(project_scoped) => {
                        let project_id = project_scoped.then(|| workspace.project_id.clone());
                        workspace.open_scoped_orchestrator(project_id, window, cx);
                    }
                    PaneMenuAction::View(kind) => workspace.open_panel(kind, pane_id, window, cx),
                    PaneMenuAction::Split(axis) => workspace.add_split(axis, window, cx),
                    PaneMenuAction::Close => workspace.close_pane_by_user(pane_id, window, cx),
                    PaneMenuAction::Main => {
                        workspace.toggle_main_pane(pane_id, cx);
                        workspace.focus_active(window, cx);
                    }
                    PaneMenuAction::Gather => workspace.gather_tabs(window, cx),
                    PaneMenuAction::Lock => {
                        workspace.toggle_pane_lock(pane_id, cx);
                        workspace.focus_active(window, cx);
                    }
                    PaneMenuAction::Focus => workspace.set_focus_mode(true, window, cx),
                }
            }))
            .into_any_element()
    }

    fn render_root_dock(
        &self,
        side: DockSide,
        controls_visible: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme::palette(cx);
        let (inset_left, inset_top) = root_dock_insets(controls_visible);
        let footer_height = if self.focus_mode || !self.settings.status_bar.enabled {
            0.0
        } else {
            ui_text::space_f32(STATUS_BAR_HEIGHT)
        };
        let target = div()
            .id(match side {
                DockSide::Left => "root-left",
                DockSide::Right => "root-right",
                DockSide::Top => "root-top",
                DockSide::Bottom => "root-bottom",
            })
            .absolute()
            .bg(rgb(colors.panel_active))
            .opacity(0.75)
            .drag_over::<DraggedTab>(move |style, _, _, _| style.bg(rgb(colors.cyan)))
            .on_drop(
                cx.listener(move |workspace, drag: &DraggedTab, window, cx| {
                    workspace.dock_tab(drag, None, side, window, cx);
                    cx.stop_propagation();
                }),
            );
        match side {
            DockSide::Left => target
                .left_0()
                .top(px(inset_top))
                .bottom(px(12.0 + footer_height))
                .w(px(12.0)),
            DockSide::Right => target
                .right_0()
                .top(px(12.0))
                .bottom(px(12.0 + footer_height))
                .w(px(12.0)),
            DockSide::Top => target.top_0().left(px(inset_left)).right_0().h(px(12.0)),
            DockSide::Bottom => target
                .bottom(px(footer_height))
                .left_0()
                .right_0()
                .h(px(12.0)),
        }
        .into_any_element()
    }
}

impl EntityInputHandler for Workspace {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let start = utf16_to_byte(&self.search, range_utf16.start);
        let end = utf16_to_byte(&self.search, range_utf16.end);
        let range = start.min(end)..start.max(end);
        *actual_range = Some(
            self.search[..range.start].encode_utf16().count()
                ..self.search[..range.end].encode_utf16().count(),
        );
        Some(self.search[range].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let end = self.search.encode_utf16().count();
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.search_marked.as_ref().map(|range| {
            self.search[..range.start].encode_utf16().count()
                ..self.search[..range.end].encode_utf16().count()
        })
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.search_marked = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.replace_search_text(range_utf16, text);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let inserted = self.replace_search_text(range_utf16, text);
        if !text.is_empty() {
            self.search_marked = Some(inserted);
        }
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(bounds)
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.search.encode_utf16().count())
    }

    fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        Some(self.search.encode_utf16().count())
    }

    fn accepts_text_input(&self, _: &mut Window, _: &mut Context<Self>) -> bool {
        self.search_focused
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme::palette(cx);
        let show_window_controls = window_controls_visible(window);
        if self.search_focused && !self.focus.is_focused(window) {
            // Focus moved elsewhere (a click on a terminal, say) without going
            // through the workspace.
            self.search_focused = false;
            self.search_marked = None;
        }
        // The menu belongs to a status bar item; without the item it has nothing to hang from.
        // It also needs the terminals frozen: a shortcut that unfroze them (a new tab, a gather,
        // a modal closing) ends it, rather than leave it under the native terminals.
        if self.layout_menu_open && (!self.layout_item_shown() || !self.tab_dragging) {
            self.layout_menu_open = false;
        }
        self.settle_terminal_drop(cx);
        if !cx.has_active_drag() {
            self.drop_target = None;
            if self.tab_dragging
                && self.panel_menu.is_none()
                && !self.layout_menu_open
                && !self.modal_open()
            {
                self.finish_tab_drag(cx);
            }
        }
        self.follow_pane_area(window, cx);
        // Every tab on screen needs a terminal before the panes are drawn: tabs restored
        // with the window, and tabs that were released while hidden.
        self.attach_shown_terminals(window, cx);
        if self.sync_tab_visibility(cx) {
            // Some tab just went off screen or came back; look at what is hidden.
            self.schedule_terminal_release(Some(terminal_lifecycle::MIN_RECHECK), cx);
        }
        div()
            .id("riwork")
            .relative()
            .key_context("RiWork")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::new_tab_action))
            .on_action(cx.listener(Self::create_project_action))
            .on_action(cx.listener(Self::new_project_window_action))
            .on_action(cx.listener(Self::open_codex_action))
            .on_action(cx.listener(Self::open_claude_action))
            .on_action(cx.listener(Self::open_grok_action))
            .on_action(cx.listener(Self::split_right_action))
            .on_action(cx.listener(Self::split_down_action))
            .on_action(cx.listener(Self::close_tab_action))
            .on_action(cx.listener(Self::close_pane_action))
            .on_action(cx.listener(Self::next_tab_action))
            .on_action(cx.listener(Self::previous_tab_action))
            .on_action(cx.listener(Self::toggle_sidebar_action))
            .on_action(cx.listener(Self::open_settings_action))
            .on_action(cx.listener(Self::open_project_settings_action))
            .on_action(cx.listener(Self::open_schedules_action))
            .on_action(cx.listener(Self::open_files_action))
            .on_action(cx.listener(Self::open_preview_action))
            .on_action(cx.listener(Self::gather_tabs_action))
            .on_action(cx.listener(Self::apply_default_layout_action))
            .on_action(cx.listener(Self::focus_search_action))
            .on_action(cx.listener(Self::toggle_focus_mode_action))
            .on_action(cx.listener(Self::open_orchestrator_action))
            .on_action(cx.listener(Self::open_project_orchestrator_action))
            .on_key_down(cx.listener(Self::search_key_down))
            .on_modifiers_changed(cx.listener(
                |workspace, event: &gpui::ModifiersChangedEvent, window, cx| {
                    workspace.terminal_link_modifiers(event.modifiers, window, cx);
                },
            ))
            .on_mouse_move(
                cx.listener(|workspace, event: &gpui::MouseMoveEvent, _, cx| {
                    workspace.resize_at(event.position, cx)
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|workspace, _, window, cx| workspace.end_resize(window, cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|workspace, _, window, cx| workspace.end_resize(window, cx)),
            )
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(colors.bg))
            .text_color(rgb(colors.text))
            .font_family(ui_text::ui_family())
            .text_size(ui_text::text(10.0))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .overflow_hidden()
                    .child(if self.focus_mode {
                        self.render_focus(window, cx)
                    } else {
                        self.render_layout(
                            &self.layout,
                            Vec::new(),
                            window.viewport_size().width.as_f32(),
                            0.0,
                            show_window_controls,
                            cx,
                        )
                    }),
            )
            .children(
                (!self.focus_mode && self.settings.status_bar.enabled).then(|| {
                    div()
                        .id("bottom-status-bar")
                        .w_full()
                        .h(ui_text::space(STATUS_BAR_HEIGHT))
                        .flex_none()
                        .flex()
                        .items_center()
                        .bg(rgb(colors.panel))
                        .border_t_1()
                        .border_color(rgb(colors.divider))
                        .child(self.render_status(cx))
                }),
            )
            .children(
                self.layout_menu_open
                    .then(|| self.render_layout_menu(window.viewport_size().width.as_f32(), cx)),
            )
            .children(show_window_controls.then(|| window_controls_island(cx)))
            .children(self.project_creator.as_ref().map(|creator| {
                div()
                    .absolute()
                    .inset_0()
                    .size_full()
                    .p(ui_text::space(16.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(gpui::rgba(0x00000099))
                    .occlude()
                    .child(creator.clone())
            }))
            .children(self.folder_editor.as_ref().map(|editor| {
                div()
                    .absolute()
                    .inset_0()
                    .size_full()
                    .p(ui_text::space(16.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(gpui::rgba(0x00000099))
                    .occlude()
                    .child(editor.clone())
            }))
            .children(self.remote_prompt.as_ref().map(|prompt| {
                div()
                    .absolute()
                    .inset_0()
                    .size_full()
                    .p(ui_text::space(16.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(gpui::rgba(0x00000099))
                    .occlude()
                    .child(prompt.clone())
            }))
            .children(self.notice.as_ref().map(|notice| {
                div()
                    .absolute()
                    .top(ui_text::space(PANE_HEADER_HEIGHT) + px(3.0))
                    .right(px(8.0))
                    .max_w(ui_text::space(520.0))
                    .p(ui_text::space(8.0))
                    .bg(rgb(colors.panel_active))
                    .text_color(rgb(colors.gold))
                    .child(notice.clone())
            }))
            .children(
                cx.has_active_drag()
                    .then(|| {
                        [
                            DockSide::Left,
                            DockSide::Right,
                            DockSide::Top,
                            DockSide::Bottom,
                        ]
                        .into_iter()
                        .map(|side| self.render_root_dock(side, show_window_controls, cx))
                        .collect::<Vec<_>>()
                    })
                    .unwrap_or_default(),
            )
            // Last, so that the terminals have painted the underline of a hovered link.
            .child(self.terminal_link_underline())
            .child(self.terminal_drop_outline(cx))
    }
}

/// An accent the colorful themes give a status bar item; Native keeps the bar muted.
fn status_accent(accent: u32, colors: Palette) -> u32 {
    if ui_text::is_native() {
        colors.muted
    } else {
        accent
    }
}

/// Native's toolbar button: a round hover in its capsule, not a full-height cell.
fn toolbar_button(button: Stateful<Div>) -> Stateful<Div> {
    button
        .h(ui_text::space(20.0))
        .min_w(ui_text::space(24.0))
        .w_auto()
        .rounded_full()
}

fn pane_menu_heading(label: &'static str, colors: Palette) -> AnyElement {
    div()
        .px(ui_text::space(8.0))
        .py(ui_text::space(6.0))
        .border_t_1()
        .border_color(rgb(colors.divider))
        .text_color(rgb(colors.muted))
        .text_size(ui_text::text(9.0))
        .when(ui_text::is_native(), |heading| {
            heading.font_weight(gpui::FontWeight::SEMIBOLD)
        })
        .child(ui_text::cased(label))
        .into_any_element()
}

/// Whether a pane is locked. The user's explicit set wins (an empty set unlocks
/// everything); until they choose, the first pane is locked while it holds a
/// navigation panel (Projects, Worktrees or Files).
fn pane_lock_state(
    explicit: Option<&HashSet<PaneId>>,
    pane_id: PaneId,
    first_pane: PaneId,
    holds_navigation_panel: impl FnOnce() -> bool,
) -> bool {
    match explicit {
        Some(locked) => locked.contains(&pane_id),
        None => first_pane == pane_id && holds_navigation_panel(),
    }
}

/// What the user asked to close.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UserClose {
    /// One tab: the tab X or Cmd+W.
    Tab,
    /// A whole pane and every tab in it: Close pane or Cmd+Shift+W.
    Pane,
}

/// Why a close the user asked for must not happen, or `None` when it may. A locked
/// pane keeps its tabs and stays a pane. Only requests from the user ask this:
/// moving a tab to another pane, project switches, restore and layout repair remove
/// tabs on their own terms, and a locked pane never blocks them.
fn user_close_refusal(pane_locked: bool, close: UserClose) -> Option<&'static str> {
    pane_locked.then_some(match close {
        UserClose::Tab => "Unlock the pane to close its tabs",
        UserClose::Pane => "Unlock the pane to close it",
    })
}

/// What a gather says it did.
fn gather_notice(gathered: &Gathered) -> String {
    let tabs = match gathered.moved {
        0 => "No tabs to gather: every unlocked tab is in the main pane already".to_owned(),
        1 => "Gathered 1 tab into the main pane".to_owned(),
        count => format!("Gathered {count} tabs into the main pane"),
    };
    match gathered.kept {
        0 => tabs,
        1 => format!("{tabs}. An empty pane stays beside a locked one so that it keeps its size"),
        count => format!(
            "{tabs}. {count} empty panes stay beside locked ones so that they keep their size"
        ),
    }
}

/// Said when a gather is asked for and no pane is the main pane.
const NO_MAIN_PANE_HINT: &str = "No main pane. Choose Main pane in a pane's … menu first";

fn is_locked_close_hint(notice: &str) -> bool {
    [UserClose::Tab, UserClose::Pane]
        .into_iter()
        .any(|close| user_close_refusal(true, close) == Some(notice))
}

/// The full name of a built-in panel, for the tooltip of its icon-only tab.
fn panel_tooltip(panel: PanelKind) -> &'static str {
    match panel {
        PanelKind::Projects => "Projects · ⌘B",
        PanelKind::Worktrees => "Worktrees",
        PanelKind::Files => "Files · ⌘⇧E",
        PanelKind::Preview => "Preview · ⌘⇧P",
        PanelKind::Tasks => "Tasks",
        PanelKind::Shells => "Shells",
        PanelKind::Usage => "Usage",
        PanelKind::Settings => "Settings · ⌘,",
        PanelKind::ProjectSettings => "Project Settings · ⌘⌥A",
        PanelKind::Schedules => "Schedules · ⌘⇧S",
    }
}

fn window_controls_visible(window: &Window) -> bool {
    !matches!(window.window_bounds(), WindowBounds::Fullscreen(_))
}

fn window_controls_island(cx: &App) -> impl IntoElement {
    let colors = theme::palette(cx);
    // Native keeps the area (it still drags the window) but draws nothing, so the
    // traffic lights sit on the bar like a Mac app's.
    div()
        .id("window-controls-island")
        .absolute()
        .top_0()
        .left_0()
        .w(px(WINDOW_CONTROLS_WIDTH))
        .h(px(WINDOW_CONTROLS_HEIGHT))
        .when(colors.controls_island, |island| {
            island
                .rounded_br(px(8.0))
                .bg(rgb(colors.panel_active))
                .border_r_1()
                .border_b_1()
                .border_color(rgb(colors.divider))
        })
        .on_mouse_down(MouseButton::Left, start_window_drag)
}

fn start_window_drag(event: &gpui::MouseDownEvent, window: &mut Window, cx: &mut App) {
    cx.stop_propagation();
    if event.click_count == 2 {
        window.titlebar_double_click();
    } else {
        window.start_window_move();
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        self.release_tab_claims();
    }
}

fn session_belongs_to_workspace(shell: &ShellSession, project_id: &str) -> bool {
    // This Mac's sessions are never a project of another Mac's, not even the global
    // orchestrator that every local project shows.
    remote_tree::parse_project_key(project_id).is_none()
        && (shell.project_id.as_deref() == Some(project_id)
            || (shell.kind == ShellKind::Orchestrator && shell.project_id.is_none()))
}

/// The project on another Mac a window opens on: the one its restored state names, else the
/// one the last launch left selected. A restored window that names a local project, or any
/// id that is not a remote project's, opens that local project instead.
fn remote_start_project(
    restored: Option<&runtime::RuntimeWindow>,
    kept: Option<&str>,
) -> Option<String> {
    let key = match restored {
        Some(window) => window.project_id.as_deref(),
        None => kept,
    }?;
    remote_tree::parse_project_key(key).map(|_| key.to_owned())
}

/// What the New Tab menu's agent entries ask another Mac's `shell.create` for.
fn remote_shell_kind(harness: HarnessKind) -> NewShellKind {
    match harness {
        HarnessKind::Codex => NewShellKind::Codex,
        HarnessKind::Claude => NewShellKind::Claude,
        HarnessKind::Grok => NewShellKind::Grok,
    }
}

/// Shells that have a tab in some window of this process (a window gives its
/// claims up when it loads another project or closes). A window never adds a
/// tab for a terminal another window already has, so one started in a window is
/// not also added to the others that show the same project; of several windows
/// that show the project, the first to notice a new terminal takes it.
fn shell_claims() -> &'static Mutex<HashSet<String>> {
    static CLAIMS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    CLAIMS.get_or_init(Default::default)
}

/// Records that a window has a tab for `shell_id`. True the first time, false
/// if a window had one already.
fn claim_shell(shell_id: &str) -> bool {
    shell_claims()
        .lock()
        .is_ok_and(|mut claims| claims.insert(shell_id.to_owned()))
}

/// Gives up the claim on `shell_id`: no window holds a tab for it any more.
fn release_shell(shell_id: &str) {
    if let Ok(mut claims) = shell_claims().lock() {
        claims.remove(shell_id);
    }
}

/// The live terminals of `project_id` that nothing in this window accounts
/// for: no tab, and not one the user closed (those are `detached`).
fn shells_to_adopt<'a>(
    shells: &'a [ShellSession],
    project_id: &str,
    shown: &HashSet<&str>,
    detached: &HashSet<String>,
) -> Vec<&'a ShellSession> {
    shells
        .iter()
        .filter(|shell| {
            shell.kind == ShellKind::Project
                && shell.alive
                && shell.project_id.as_deref() == Some(project_id)
                && !shown.contains(shell.id.as_str())
                && !detached.contains(&shell.id)
        })
        .collect()
}

fn orchestrator_tab_title(shell: &ShellSession) -> String {
    if shell.project_id.is_some() {
        "P·ORCH · PROJECT"
    } else {
        "G·ORCH · GLOBAL"
    }
    .to_owned()
}

fn harness_name(harness: HarnessKind) -> &'static str {
    match harness {
        HarnessKind::Codex => "codex",
        HarnessKind::Claude => "claude",
        HarnessKind::Grok => "grok",
    }
}

fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn request_codex_usage(force: bool, cx: &mut App) {
    for home in codex_usage_profiles(cx).into_keys() {
        request_codex_usage_at(home, force, cx);
    }
}

fn selected_codex_home(cx: &App) -> Option<PathBuf> {
    let selected = cx.global::<Settings>().selected_codex_account.as_deref();
    if let Some(snapshot) = &cx.global::<settings::CodexAccountsState>().snapshot {
        snapshot
            .accounts
            .iter()
            .find(|account| {
                account.available
                    && match selected {
                        Some(id) => account.id == id,
                        None => account.is_system_default,
                    }
            })
            .map(|account| account.home.clone())
    } else if selected.is_none() {
        codex_accounts::default_codex_home().ok()
    } else {
        None
    }
}

fn project_selected_codex_home(workspace: &Workspace, cx: &App) -> Option<PathBuf> {
    match workspace
        .state
        .projects
        .iter()
        .find(|project| project.id == workspace.project_id)
        .map(|project| &project.codex_account)
    {
        Some(ProjectCodexAccount::SystemDefault) => codex_accounts::default_codex_home().ok(),
        Some(ProjectCodexAccount::Saved(id)) => cx
            .global::<settings::CodexAccountsState>()
            .snapshot
            .as_ref()?
            .accounts
            .iter()
            .find(|account| account.available && account.id == *id)
            .map(|account| account.home.clone()),
        _ => selected_codex_home(cx),
    }
}

fn codex_usage_profiles(cx: &App) -> BTreeMap<PathBuf, String> {
    if let Some(snapshot) = &cx.global::<settings::CodexAccountsState>().snapshot {
        snapshot
            .accounts
            .iter()
            .filter(|account| account.available)
            .map(|account| (account.home.clone(), account.label.clone()))
            .collect()
    } else {
        codex_accounts::default_codex_home()
            .ok()
            .map(|home| BTreeMap::from([(home, "System default".to_owned())]))
            .unwrap_or_default()
    }
}

fn request_codex_usage_at(home: PathBuf, force: bool, cx: &mut App) {
    let now = unix_time();
    {
        let cache = cx
            .global_mut::<AccountUsage>()
            .codex
            .entry(home.clone())
            .or_default();
        let interval = usage::codex_refresh_interval(cache.codex.is_some(), cache.failures);
        if cache.pending
            || (!force
                && cache.last_attempt != 0
                && now.saturating_sub(cache.last_attempt) < interval)
        {
            return;
        }
        cache.pending = true;
        cache.last_attempt = now;
    }
    let read_home = home.clone();
    let task = cx
        .background_executor()
        .spawn(async move { usage::read_codex_usage_at(&read_home) });
    cx.spawn(async move |cx| {
        let result = task.await;
        let _ = cx.update(|cx| {
            let cache = cx
                .global_mut::<AccountUsage>()
                .codex
                .entry(home)
                .or_default();
            cache.pending = false;
            match result {
                Ok(snapshot) => {
                    cache.codex = Some(snapshot);
                    cache.codex_error = None;
                    cache.failures = 0;
                }
                Err(error) => {
                    cache.codex_error = Some(error);
                    cache.failures = cache.failures.saturating_add(1);
                }
            }
            cx.refresh_windows();
        });
    })
    .detach();
}

fn usage_summary(provider: &str, snapshot: &ProviderUsage) -> String {
    let stale = unix_time().saturating_sub(snapshot.updated_at_unix) > 15 * 60;
    let limits = snapshot
        .windows
        .iter()
        .take(2)
        .map(|window| {
            format!(
                "{} {:.0}% left",
                window.label,
                (100.0 - window.used_percent).clamp(0.0, 100.0)
            )
        })
        .collect::<Vec<_>>()
        .join(" / ");
    format!(
        "{provider}{} · {}",
        if stale { "*" } else { "" },
        if limits.is_empty() { "—" } else { &limits }
    )
}

fn reset_summary(resets_at: Option<u64>) -> String {
    let Some(deadline) = resets_at else {
        return "Reset unavailable".to_owned();
    };
    let seconds = deadline.saturating_sub(unix_time());
    if seconds == 0 {
        "Awaiting reset update".to_owned()
    } else if seconds >= 86400 {
        format!(
            "Resets in {}d {}h",
            seconds / 86400,
            (seconds % 86400) / 3600
        )
    } else if seconds >= 3600 {
        format!("Resets in {}h {}m", seconds / 3600, (seconds % 3600) / 60)
    } else {
        format!("Resets in {}m", seconds.div_ceil(60))
    }
}

fn ago(seconds: u64) -> String {
    match seconds {
        0..60 => "just now".to_owned(),
        60..3600 => format!("{}m ago", seconds / 60),
        3600..86400 => format!("{}h ago", seconds / 3600),
        _ => format!("{}d ago", seconds / 86400),
    }
}

fn token_breakdown(tokens: &usage::TokenCounts) -> String {
    use usage::format_tokens;
    format!(
        "in {} · out {} · cached {} · cache writes {} · reasoning {}",
        format_tokens(tokens.input),
        format_tokens(tokens.output),
        format_tokens(tokens.cached_read),
        format_tokens(tokens.cache_creation),
        format_tokens(tokens.reasoning),
    )
}

fn render_grok_session(
    title: &str,
    tab: Option<&usage::GrokTabUsage>,
    colors: Palette,
) -> AnyElement {
    let card = div()
        .p(ui_text::space(12.0))
        .border_t_1()
        .border_color(rgb(colors.divider));
    let Some(tab) = tab else {
        return card
            .child(div().text_color(rgb(colors.cyan)).child(title.to_owned()))
            .child(
                div()
                    .mt(ui_text::space(6.0))
                    .text_color(rgb(colors.muted))
                    .child("Reading Grok usage…"),
            )
            .into_any_element();
    };
    let Some(session) = &tab.usage else {
        return card
            .child(div().text_color(rgb(colors.cyan)).child(title.to_owned()))
            .child(
                div()
                    .mt(ui_text::space(6.0))
                    .text_color(rgb(colors.muted))
                    .child(format!(
                        "usage unavailable: {}",
                        tab.error.as_deref().unwrap_or("Grok has not reported it")
                    )),
            )
            .into_any_element();
    };
    let now = unix_time();
    let headline = [
        session.cost_usd.map(usage::format_usd),
        Some(format!(
            "{} tokens",
            usage::format_tokens(session.tokens.total)
        )),
        Some(format!(
            "{} turn{}",
            session.turns,
            if session.turns == 1 { "" } else { "s" }
        )),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ");
    let read_age = now.saturating_sub(tab.fetched_at_unix);
    let mut freshness = format!("Updated {}", ago(read_age));
    if let Some(activity) = session.updated_at_unix {
        freshness.push_str(&format!(
            " · last activity {}",
            ago(now.saturating_sub(activity))
        ));
    }
    card.child(
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap(ui_text::space(8.0))
            .child(div().text_color(rgb(colors.cyan)).child(title.to_owned()))
            .child(
                div()
                    .text_color(rgb(colors.muted))
                    .text_size(ui_text::text(9.0))
                    .child(
                        session
                            .primary_model
                            .clone()
                            .unwrap_or_else(|| "model unknown".to_owned()),
                    ),
            ),
    )
    .child(div().mt(ui_text::space(6.0)).child(headline))
    .child(
        div()
            .mt(ui_text::space(4.0))
            .text_color(rgb(colors.muted))
            .text_size(ui_text::text(9.0))
            .child(token_breakdown(&session.tokens)),
    )
    .children((session.models.len() > 1).then(|| {
        div()
            .mt(ui_text::space(4.0))
            .text_color(rgb(colors.muted))
            .text_size(ui_text::text(9.0))
            .children(session.models.iter().map(|model| {
                div().child(format!(
                    "{} · {} tokens · {} calls{}",
                    model.model,
                    usage::format_tokens(model.tokens.total),
                    model.model_calls,
                    model
                        .cost_usd
                        .map(|cost| format!(" · {}", usage::format_usd(cost)))
                        .unwrap_or_default()
                ))
            }))
    }))
    .child(
        div()
            .mt(ui_text::space(4.0))
            .text_color(rgb(if tab.stale { colors.gold } else { colors.muted }))
            .text_size(ui_text::text(9.0))
            .child(if tab.stale {
                format!(
                    "STALE · {} · {}",
                    freshness,
                    tab.error.as_deref().unwrap_or("latest read failed")
                )
            } else {
                freshness
            }),
    )
    .into_any_element()
}

fn render_grok_total(totals: &usage::GrokTotals, missing: usize, colors: Palette) -> AnyElement {
    div()
        .p(ui_text::space(12.0))
        .border_t_1()
        .border_color(rgb(colors.divider))
        .child(div().text_color(rgb(colors.cyan)).child(ui_text::cased("Grok · Total")))
        .child(div().mt(ui_text::space(6.0)).child(format!(
            "{} · {} tokens · {} session{}",
            usage::format_usd(totals.cost_usd),
            usage::format_tokens(totals.tokens.total),
            totals.sessions,
            if totals.sessions == 1 { "" } else { "s" }
        )))
        .child(
            div()
                .mt(ui_text::space(4.0))
                .text_color(rgb(colors.muted))
                .text_size(ui_text::text(9.0))
                .child(token_breakdown(&totals.tokens)),
        )
        .child(
            div()
                .mt(ui_text::space(4.0))
                .text_color(rgb(colors.muted))
                .text_size(ui_text::text(9.0))
                .child(
                    "A session resumed or forked from another includes that history, so the total can overcount.",
                ),
        )
        .children((missing > 0).then(|| {
            div()
                .mt(ui_text::space(4.0))
                .text_color(rgb(colors.muted))
                .text_size(ui_text::text(9.0))
                .child(format!(
                    "{missing} listed session{} without usage not counted",
                    if missing == 1 { "" } else { "s" }
                ))
        }))
        .into_any_element()
}

fn render_provider_usage(snapshot: &ProviderUsage, title: &str, colors: Palette) -> AnyElement {
    let age = unix_time().saturating_sub(snapshot.updated_at_unix);
    let limits = snapshot
        .windows
        .iter()
        .map(|window| {
            let remaining = (100.0 - window.used_percent).clamp(0.0, 100.0);
            div()
                .mt(ui_text::space(10.0))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(ui_text::space(8.0))
                        .child(format!("{} · {:.0}% left", window.label, remaining))
                        .child(
                            div()
                                .text_color(rgb(colors.muted))
                                .text_size(ui_text::text(9.0))
                                .child(reset_summary(window.resets_at)),
                        ),
                )
                .child(
                    div()
                        .mt(ui_text::space(5.0))
                        .h(ui_text::space(3.0))
                        .bg(rgb(colors.divider))
                        .child(
                            div()
                                .h_full()
                                .w(gpui::relative((remaining / 100.0) as f32))
                                .bg(rgb(if remaining < 15.0 {
                                    colors.magenta
                                } else {
                                    colors.cyan
                                })),
                        ),
                )
                .into_any_element()
        })
        .collect::<Vec<_>>();
    div()
        .p(ui_text::space(12.0))
        .border_t_1()
        .border_color(rgb(colors.divider))
        .child(div().text_color(rgb(colors.cyan)).child(title.to_owned()))
        .children(snapshot.account_label.as_ref().map(|label| {
            div()
                .mt(ui_text::space(4.0))
                .text_color(rgb(colors.muted))
                .child(label.clone())
        }))
        .child(
            div()
                .mt(ui_text::space(4.0))
                .text_color(rgb(if age > 900 { colors.gold } else { colors.muted }))
                .text_size(ui_text::text(9.0))
                .child(format!(
                    "{}Updated {}m ago",
                    if age > 900 { "Stale · " } else { "" },
                    age / 60
                )),
        )
        .children(limits)
        .children(snapshot.windows.is_empty().then(|| {
            div()
                .mt(ui_text::space(8.0))
                .text_color(rgb(colors.muted))
                .child("Quota windows unavailable")
        }))
        .children(snapshot.context_used_percent.map(|used| {
            div()
                .mt(ui_text::space(10.0))
                .text_color(rgb(colors.muted))
                .child(format!("Context · {used:.0}% used"))
        }))
        .children(snapshot.session_cost_usd.map(|cost| {
            div()
                .mt(ui_text::space(5.0))
                .text_color(rgb(colors.muted))
                .child(format!("Estimated session cost · ${cost:.2}"))
        }))
        .into_any_element()
}

fn sync_appearance(cx: &mut App) {
    // Text matching the terminal follows edits to Ghostty's font-size, whatever the theme.
    ui_text::refresh_terminal_font_size(cx);
    let selected = cx.global::<Settings>().theme;
    // Presets are static and Native changes only with macOS's light or dark mode;
    // native configuration is resolved again to pick up edits, including recursive
    // config files and custom theme files. The parse itself is skipped while none
    // of those files changed.
    let current = cx.global::<Appearance>();
    if selected != ThemeChoice::Ghostty
        && current.selected == selected
        && !current.is_stale_for(theme::system_is_dark(cx))
    {
        return;
    }
    let Some(appearance) = theme::refresh_appearance(selected, cx) else {
        return;
    };
    if &appearance != cx.global::<Appearance>() {
        cx.set_global(appearance);
    }
}

fn exit_startup(error: String) -> ! {
    eprintln!("riwork: {error}");
    std::process::exit(2);
}

fn main() {
    let args = env::args().skip(1).collect::<Vec<_>>();
    match cli::run_cli(&args) {
        Ok(true) => return,
        Ok(false) => {}
        Err(error) => {
            if !error.is_empty() {
                eprintln!("riwork: {error}");
            }
            std::process::exit(2);
        }
    }
    if let Err(error) = paths::ensure_default_projects_directory() {
        eprintln!("riwork: {error}");
        std::process::exit(2);
    }
    let startup_path = args.first().map(PathBuf::from).filter(|path| path.is_dir());
    let fallback_cwd = env::current_dir()
        .ok()
        .filter(|path| path != Path::new("/"))
        .or_else(|| {
            env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_dir())
        })
        .unwrap_or_else(|| PathBuf::from("."));
    // None of these can be worked around, but a message beats a panic backtrace.
    let runtime =
        runtime::RuntimeManager::open_default().unwrap_or_else(|error| exit_startup(error));
    let restore = runtime
        .restore_from_env()
        .unwrap_or_else(|error| exit_startup(error));
    let state_home = SessionManager::open_default()
        .unwrap_or_else(|error| exit_startup(error))
        .state_home()
        .to_path_buf();
    let mut registration = runtime
        .register(state_home.clone())
        .unwrap_or_else(|error| exit_startup(error));
    // A replacement that cannot finish restoring ends itself; see the watchdog.
    let watchdog = restore
        .as_ref()
        .map(|snapshot| registration.start_restore_watchdog(snapshot));

    application().run(move |cx: &mut App| {
        cx.set_app_identity("dev.riwork.shell", "RiWork");
        schedules::start(state_home.clone(), cx);
        notifications::start(state_home.clone(), cx);
        cx.on_system_notification_response(|response, cx| {
            if response.action_id.as_ref().is_some_and(|action| action.as_ref() != "open") { return; }
            if let Some((project_id, shell_id)) = notifications::response_target(&response.tag) {
                if let Err(error) = open_completed_agent(project_id, shell_id, cx) {
                    eprintln!("riwork notifications: {error}");
                }
            }
        });
        cx.set_global(AccountUsage::default());
        cx.set_global(settings::CodexAccountsState::default());
        cx.set_global(CuaSetupState::default());
        remote_service::init(cx);
        settings::refresh_cua_status(cx);
        let settings = SettingsStore::open_default()
            .and_then(|store| store.load())
            .unwrap_or_else(|error| {
                eprintln!("riwork: {error}");
                Settings::default()
            });
        theme::force_system_appearance(cx);
        cx.set_global(Appearance::resolve(
            settings.theme,
            theme::system_is_dark(cx),
        ));
        cx.set_global(settings);
        ui_text::init(cx);
        // After both globals exist: publishes now and again on every change.
        appearance_sync::start(state_home, cx);
        settings::refresh_codex_accounts(cx);
        cx.spawn(async move |cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                cx.update(sync_appearance);
            }
        })
        .detach();
        cx.spawn(async move |cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_secs(15))
                    .await;
                cx.update(|cx| {
                    let state = cx.global::<CuaSetupState>();
                    if state.status.as_ref().is_none_or(|status| !status.ready) {
                        settings::refresh_cua_status(cx);
                    }
                });
            }
        })
        .detach();
        cx.on_action(|_: &Quit, cx| {
            for handle in cx.windows() {
                if let Some(handle) = handle.downcast::<Workspace>() {
                    let _ = handle.update(cx, |workspace, _, _| workspace.save_layout());
                }
            }
            cx.quit();
        });
        cx.on_action(|_: &BiggerText, cx| ui_text::change(ui_text::SizeChange::Bigger, cx));
        cx.on_action(|_: &SmallerText, cx| ui_text::change(ui_text::SizeChange::Smaller, cx));
        cx.on_action(|_: &ActualSizeText, cx| ui_text::change(ui_text::SizeChange::Reset, cx));
        // RiWork's text size, except inside a terminal: there GPUI finds no binding,
        // so the key reaches Ghostty for its own font zoom (see `search_key_down`).
        // `!Terminal` fails when any focused ancestor is gpui-libghostty's
        // `key_context("Terminal")`. The first binding is the one the menu shows.
        let outside_terminals = Some("!Terminal");
        cx.bind_keys([
            KeyBinding::new("cmd-=", BiggerText, outside_terminals),
            KeyBinding::new("cmd-+", BiggerText, outside_terminals),
            KeyBinding::new("cmd-shift-=", BiggerText, outside_terminals),
            KeyBinding::new("cmd--", SmallerText, outside_terminals),
            KeyBinding::new("cmd-0", ActualSizeText, outside_terminals),
        ]);
        cx.bind_keys([
            KeyBinding::new("cmd-,", OpenSettings, None),
            KeyBinding::new("cmd-alt-a", OpenProjectSettings, None),
            KeyBinding::new("cmd-shift-s", OpenSchedules, None),
            KeyBinding::new("cmd-shift-e", OpenFiles, None),
            KeyBinding::new("cmd-shift-p", OpenPreview, None),
            KeyBinding::new("cmd-shift-m", GatherTabs, None),
            KeyBinding::new("cmd-alt-l", ApplyDefaultLayout, None),
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-n", CreateProject, None),
            KeyBinding::new("cmd-t", NewTab, None),
            KeyBinding::new("cmd-d", SplitRight, None),
            KeyBinding::new("cmd-shift-d", SplitDown, None),
            KeyBinding::new("cmd-w", CloseTab, None),
            KeyBinding::new("cmd-shift-w", ClosePane, None),
            KeyBinding::new("ctrl-tab", NextTab, None),
            KeyBinding::new("ctrl-shift-tab", PreviousTab, None),
            KeyBinding::new("cmd-b", ToggleSidebar, None),
            KeyBinding::new("cmd-f", FocusSearch, None),
            KeyBinding::new("cmd-shift-f", ToggleFocusMode, None),
            KeyBinding::new("cmd-shift-o", OpenOrchestrator, None),
            KeyBinding::new("cmd-alt-o", OpenProjectOrchestrator, None),
            KeyBinding::new("cmd-shift-n", NewProjectWindow, None),
            KeyBinding::new("cmd-shift-c", OpenCodex, None),
            KeyBinding::new("cmd-shift-l", OpenClaude, None),
            KeyBinding::new("cmd-shift-g", OpenGrok, None),
        ]);
        cx.set_menus([
            Menu::new("RiWork").items([
                MenuItem::action("Settings…", OpenSettings),
                MenuItem::action("Project Settings…", OpenProjectSettings),
                MenuItem::action("Schedules", OpenSchedules),
                MenuItem::separator(),
                MenuItem::action("Quit RiWork", Quit),
            ]),
            // RiWork's own text; terminals zoom with the same keys while focused.
            Menu::new("View").items([
                MenuItem::action("Default Layout", ApplyDefaultLayout),
                MenuItem::action("Gather Tabs into Main Pane", GatherTabs),
                MenuItem::separator(),
                MenuItem::action("Bigger Text", BiggerText),
                MenuItem::action("Smaller Text", SmallerText),
                MenuItem::action("Actual Size", ActualSizeText),
            ]),
        ]);
        dock_menu::init(cx);
        tooltip::init(cx);
        cx.on_window_closed(|cx, window| {
            // A hint window is not a reason to stay open.
            if cx.windows().iter().all(tooltip::is_popup) {
                cx.quit();
            } else {
                dock_menu::window_closed(window, cx);
            }
        })
        .detach();
        if let Some(snapshot) = &restore {
            for window in &snapshot.windows {
                if let Err(error) = open_workspace_window(
                    Some(window.path.clone()),
                    fallback_cwd.clone(),
                    Some(window.clone()),
                    cx,
                ) {
                    // The previous app stays open until every window is restored.
                    eprintln!("riwork: cannot restore a window: {error}");
                    cx.quit();
                    return;
                }
            }
        } else if let Err(error) =
            open_startup_window(startup_path.clone(), fallback_cwd.clone(), cx)
        {
            exit_startup(error);
        }
        // The heartbeat below publishes again and reports failures.
        if let Err(error) = registration.publish_windows(runtime_windows(cx, false)) {
            eprintln!("riwork: {error}");
        }
        cx.spawn(async move |cx| {
            let mut reload = None;
            let mut restore = restore;
            let mut watchdog = watchdog;
            let restore_started = Instant::now();
            let mut ticks = 0u32;
            let mut publish_due = false;
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                ticks = ticks.wrapping_add(1);
                let _ = cx.update(|cx| {
                    if let Some(snapshot) = &restore {
                        if restore_started.elapsed() > Duration::from_secs(20) {
                            eprintln!("riwork: window restoration timed out; keeping the previous app open");
                            cx.quit();
                            return;
                        }
                        let windows = runtime_windows(cx, false);
                        if windows.len() == snapshot.windows.len()
                            && windows.iter().all(|window| window.layout.is_some())
                        {
                            // The watchdog may already be ending this process,
                            // which must then not confirm anything.
                            if watchdog.as_ref().is_some_and(|w| !w.begin_confirm()) {
                                return;
                            }
                            if let Err(error) = registration
                                .publish_windows(windows)
                                .and_then(|_| runtime.mark_restore_ready(snapshot, &registration))
                            {
                                eprintln!("riwork: {error}");
                                if let Some(watchdog) = &watchdog {
                                    watchdog.abort_confirm();
                                }
                                cx.quit();
                                return;
                            }
                            if let Some(watchdog) = watchdog.take() {
                                watchdog.finish_confirm();
                            }
                            restore = None;
                        } else {
                            return;
                        }
                    }
                    if let Some(launch) = &reload {
                        match registration.reload_ready(launch) {
                            Ok(true) => cx.quit(),
                            Ok(false) => {}
                            Err(error) => {
                                eprintln!("riwork: {error}");
                                reload = None;
                            }
                        }
                        return;
                    }
                    // The registry only feeds `riwork instances` and reload
                    // reports (a reload snapshots the windows itself), so it is
                    // refreshed every 2 s, written only on change, and never waits
                    // for its lock on this thread: a busy lock stays due for the
                    // next tick.
                    publish_due |= ticks.is_multiple_of(4);
                    if publish_due {
                        match registration.try_publish_windows(runtime_windows(cx, false)) {
                            Ok(true) => publish_due = false,
                            Ok(false) => {}
                            Err(error) => {
                                publish_due = false;
                                eprintln!("riwork: {error}");
                            }
                        }
                    }
                    match registration.pending_reload() {
                        Ok(Some(request)) => {
                            match registration.launch_reload(&request, runtime_windows(cx, true)) {
                                Ok(launch) => reload = Some(launch),
                                Err(error) => {
                                    let _ = registration.fail_reload(&request, &error);
                                    eprintln!("riwork: {error}");
                                }
                            }
                        }
                        Ok(None) => {}
                        Err(error) => eprintln!("riwork: {error}"),
                    }
                });
            }
        })
        .detach();
        cx.activate(true);
    });
}

fn open_completed_agent(project_id: &str, shell_id: &str, cx: &mut App) -> Result<(), String> {
    let project = Store::open_default()?
        .snapshot()?
        .project(project_id)?
        .clone();
    let session = SessionManager::open_default()?.registered_session(shell_id)?;
    if session.project_id.as_deref() != Some(project_id) {
        return Err("This agent no longer belongs to that project".to_owned());
    }
    for handle in cx.windows() {
        let Some(handle) = handle.downcast::<Workspace>() else {
            continue;
        };
        let opened = handle
            .update(cx, |workspace, window, cx| {
                if workspace.project_id != project_id {
                    return false;
                }
                workspace.show_shell(shell_id, window, cx);
                window.activate_window();
                true
            })
            .map_err(|error| error.to_string())?;
        if opened {
            cx.activate(true);
            return Ok(());
        }
    }
    let handle = open_workspace_window(Some(project.root.clone()), project.root, None, cx)?;
    handle
        .update(cx, |workspace, window, cx| {
            workspace.show_shell(shell_id, window, cx);
            window.activate_window();
        })
        .map_err(|error| error.to_string())?;
    cx.activate(true);
    Ok(())
}

fn runtime_windows(cx: &mut App, save: bool) -> Vec<runtime::RuntimeWindow> {
    cx.windows()
        .into_iter()
        .filter_map(|handle| {
            let handle = handle.downcast::<Workspace>()?;
            handle
                .update(cx, |workspace, window, _| {
                    if save {
                        workspace.save_layout();
                    }
                    workspace.runtime_window(window)
                })
                .ok()
        })
        .collect()
}

/// The window's frame when it is an ordinary window, None in full screen or
/// zoomed. GPUI reports a zoomed macOS window as plain Windowed, so it also asks
/// `is_maximized`; that covers the app's own titlebar double-click.
fn normal_window_bounds(window: &Window) -> Option<Bounds<Pixels>> {
    match window.window_bounds() {
        WindowBounds::Windowed(bounds) if !window.is_maximized() => Some(bounds),
        _ => None,
    }
}

/// Opens the first window of an ordinary launch. A folder argument that cannot be
/// opened falls back to the active project, and the window says why.
fn open_startup_window(
    startup_path: Option<PathBuf>,
    fallback_cwd: PathBuf,
    cx: &mut App,
) -> Result<(), String> {
    if startup_path.is_some() {
        // Asking for a folder is choosing a project of this Mac: the next plain launch opens it.
        remote_service::select(None, cx);
    }
    let error = match open_workspace_window(startup_path.clone(), fallback_cwd.clone(), None, cx) {
        Ok(_) => return Ok(()),
        Err(error) if startup_path.is_some() => error,
        Err(error) => return Err(error),
    };
    eprintln!("riwork: {error}");
    let handle = open_workspace_window(None, fallback_cwd, None, cx)?;
    let _ = handle.update(cx, |workspace, _, cx| {
        workspace.notice = Some(error);
        cx.notify();
    });
    Ok(())
}

fn open_workspace_window(
    startup_path: Option<PathBuf>,
    fallback_cwd: PathBuf,
    restore: Option<runtime::RuntimeWindow>,
    cx: &mut App,
) -> Result<WindowHandle<Workspace>, String> {
    let ordinary = startup_path.is_none() && restore.is_none();
    let mut startup = WorkspaceStartup::prepare(startup_path, fallback_cwd)?;
    if ordinary {
        startup.remote = cx.global::<RemoteState>().selected().map(str::to_owned);
    }
    // Best effort: an unreadable layout only means the default size.
    let saved_size = cx
        .global::<Settings>()
        .remember_window_size
        .then(|| startup.layouts.window_size(&startup.project.id))
        .flatten()
        .unwrap_or(WindowSize {
            width: 1220.0,
            height: 780.0,
        });
    let cascade = cx
        .windows()
        .iter()
        .filter(|handle| !tooltip::is_popup(handle))
        .count();
    let frame = match cx.primary_display().map(|display| display.visible_bounds()) {
        Some(display) => layouts::WindowFrame::opening(
            saved_size,
            layouts::WindowFrame {
                x: display.origin.x.as_f32(),
                y: display.origin.y.as_f32(),
                width: display.size.width.as_f32(),
                height: display.size.height.as_f32(),
            },
            cascade,
        ),
        None => layouts::WindowFrame {
            x: 0.0,
            y: 0.0,
            width: saved_size.width,
            height: saved_size.height,
        },
    };
    let mut bounds = Bounds::new(
        point(px(frame.x), px(frame.y)),
        size(px(frame.width), px(frame.height)),
    );
    if let Some(saved) = restore.as_ref().and_then(|window| window.bounds.as_ref()) {
        bounds.origin = point(px(saved.x), px(saved.y));
        bounds.size = size(px(saved.width), px(saved.height));
    }
    let window_bounds = match restore
        .as_ref()
        .map(|window| window.mode)
        .unwrap_or_default()
    {
        runtime::WindowMode::Windowed => WindowBounds::Windowed(bounds),
        runtime::WindowMode::Maximized => WindowBounds::Maximized(bounds),
        runtime::WindowMode::Fullscreen => WindowBounds::Fullscreen(bounds),
    };
    cx.open_window(
        WindowOptions {
            window_bounds: Some(window_bounds),
            titlebar: Some(TitlebarOptions {
                appears_transparent: true,
                traffic_light_position: Some(point(px(10.0), px(8.0))),
                ..Default::default()
            }),
            app_owns_titlebar_drag: true,
            window_min_size: Some(size(px(640.0), px(400.0))),
            ..Default::default()
        },
        |window, cx| {
            metal_layer::limit_drawables(window);
            cx.new(|cx| Workspace::new(startup, restore, window, cx))
        },
    )
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod startup_tests {
    use super::*;

    fn startup_at(
        home: &Path,
        startup_path: Option<PathBuf>,
        fallback_cwd: PathBuf,
    ) -> Result<WorkspaceStartup, String> {
        WorkspaceStartup::resolve(
            Store::open(home)?,
            LayoutStore::open(home)?,
            SettingsStore::open(home)?,
            SessionManager::at(home.to_path_buf())?,
            startup_path,
            fallback_cwd,
        )
    }

    #[test]
    fn a_vanished_project_root_is_an_error_for_the_caller_not_a_panic() {
        let base = std::env::temp_dir().join(format!("riwork-startup-{}", uuid::Uuid::new_v4()));
        let home = base.join("home");
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let project = project.canonicalize().unwrap();

        let opened = startup_at(&home, Some(project.clone()), base.clone()).unwrap();
        assert_eq!(opened.project.root, project);
        // The same folder opens the already registered project.
        let again = startup_at(&home, Some(project.clone()), base.clone()).unwrap();
        assert_eq!(again.project.id, opened.project.id);

        std::fs::remove_dir_all(&project).unwrap();
        let error = startup_at(&home, Some(project.clone()), base.clone())
            .err()
            .expect("a missing folder cannot be opened");
        assert!(error.contains("Cannot open project folder"), "{error}");

        // Without a folder argument the active project is used, whatever became of it.
        let active = startup_at(&home, None, base.clone()).unwrap();
        assert_eq!(active.project.id, opened.project.id);
        std::fs::remove_dir_all(base).unwrap();
    }
}

#[cfg(test)]
mod workspace_tab_tests {
    use super::*;

    fn remote_tab(id: TabId, host: &str, shell: &str) -> Tab {
        Tab {
            id,
            title: remote_tree::remote_tab_title("Studio", &remote_tree::short_id(shell)),
            content: TabContent::RemoteShell {
                desktop_id: host.to_owned(),
                shell_id: shell.to_owned(),
                terminal: None,
                attach_error: None,
                attach_failures: 0,
            },
            hidden_since: None,
        }
    }

    fn local_tab(id: TabId, shell: &str) -> Tab {
        Tab {
            id,
            title: "zsh 01 · main".to_owned(),
            content: TabContent::Shell {
                shell_id: shell.to_owned(),
                worktree_id: None,
                terminal: None,
                attach_error: None,
                attach_failures: 0,
            },
            hidden_since: None,
        }
    }

    #[test]
    fn a_remote_tab_saves_under_its_host_and_is_not_one_of_this_macs_shells() {
        let tab = remote_tab(4, "host-1", "shell-9");
        assert_eq!(
            tab.saved(),
            SavedTab::RemoteShell {
                desktop_id: "host-1".to_owned(),
                shell_id: "shell-9".to_owned(),
            }
        );
        assert_eq!(tab.saved().key(), "remote:host-1:shell-9");
        // It is a terminal, but never a local session: nothing that looks sessions up by
        // id, adopts, detaches or kills may see it.
        assert!(tab.is_terminal());
        assert_eq!(tab.shell_id(), None);
        assert_eq!(tab.remote(), Some(("host-1", "shell-9")));
        assert!(tab.panel().is_none());

        let local = local_tab(5, "shell-9");
        assert!(local.is_terminal() && local.remote().is_none());
        assert_eq!(local.shell_id(), Some("shell-9"));
        let panel = Tab {
            id: 6,
            title: "SETTINGS".to_owned(),
            content: TabContent::Panel(PanelKind::Settings),
            hidden_since: None,
        };
        assert!(!panel.is_terminal() && panel.remote().is_none());
    }

    #[test]
    fn remote_tab_titles_start_with_the_hosts_mark() {
        let tab = remote_tab(4, "host-1", "0123456789abcdef");
        assert_eq!(tab.title, "⇄ Studio · 01234567");
        assert!(tab.title.starts_with("⇄ Studio · "));
    }

    #[test]
    fn a_failed_attach_is_remembered_and_forgotten_for_remote_tabs_as_for_local_ones() {
        for mut tab in [remote_tab(4, "host-1", "shell-9"), local_tab(5, "shell-9")] {
            let attach = tab.content.attach_state().expect("a terminal tab");
            *attach.error = Some("could not start".to_owned());
            *attach.failures = 2;
            // Selecting the tab, or showing it again, clears both.
            let attach = tab.content.attach_state().expect("a terminal tab");
            *attach.error = None;
            *attach.failures = 0;
            assert!(matches!(
                tab.content,
                TabContent::Shell {
                    attach_error: None,
                    attach_failures: 0,
                    ..
                } | TabContent::RemoteShell {
                    attach_error: None,
                    attach_failures: 0,
                    ..
                }
            ));
        }
        let mut panel = TabContent::Panel(PanelKind::Files);
        assert!(panel.attach_state().is_none());
    }

    fn live_shell(id: &str) -> ShellSession {
        ShellSession {
            id: id.to_owned(),
            project_id: None,
            worktree_id: None,
            kind: ShellKind::Project,
            cwd: PathBuf::from("/tmp"),
            command: None,
            editor_path: None,
            harness: None,
            codex_account_id: None,
            codex_account_label: None,
            codex_account_email: None,
            codex_home: None,
            unrestricted: false,
            orchestrator_skill_loaded: false,
            orchestrator_skill_version: None,
            orchestrator_project_root: None,
            created_at_unix: 0,
            alive: true,
        }
    }

    #[test]
    fn hidden_remote_terminals_are_released_like_local_ones_until_the_bridge_exits() {
        let remote = remote_tab(4, "host-1", "shell-9");
        let none = HashSet::new();
        // A live bridge can be dropped and attached again: the host repaints it.
        assert!(!keeps_terminal(&remote, &none, &[]));
        // An exited one keeps its last screen, and its strip, until the tab is closed.
        assert!(keeps_terminal(&remote, &HashSet::from([4]), &[]));
        // The local rules are unchanged: a live session is released, an ended or unknown
        // one is kept, and the remote shell's id is never looked up among local sessions.
        let local = local_tab(5, "shell-9");
        assert!(!keeps_terminal(&local, &none, &[live_shell("shell-9")]));
        let mut ended = live_shell("shell-9");
        ended.alive = false;
        assert!(keeps_terminal(&local, &none, &[ended]));
        assert!(keeps_terminal(&local, &none, &[]));
        assert!(!keeps_terminal(&remote, &none, &[live_shell("shell-9")]));
    }

    fn restored(project_id: Option<&str>) -> runtime::RuntimeWindow {
        runtime::RuntimeWindow {
            project_id: project_id.map(str::to_owned),
            path: PathBuf::from("/Users/me/app"),
            bounds: None,
            mode: runtime::WindowMode::Windowed,
            layout: None,
            focus_mode: false,
            focus_centered: false,
        }
    }

    #[test]
    fn a_window_restores_on_the_remote_project_it_was_on() {
        let key = remote_tree::project_key("h1", "p1");
        // A reload restores each window as it was, whatever the last launch left selected.
        assert_eq!(
            remote_start_project(Some(&restored(Some(&key))), None),
            Some(key.clone())
        );
        assert_eq!(
            remote_start_project(Some(&restored(Some(&key))), Some("remote:h2:p2")),
            Some(key.clone())
        );
        // A window that was on a local project (or none) opens that one.
        let local = uuid::Uuid::new_v4().to_string();
        assert_eq!(
            remote_start_project(Some(&restored(Some(&local))), Some(&key)),
            None
        );
        assert_eq!(
            remote_start_project(Some(&restored(None)), Some(&key)),
            None
        );
        // An ordinary launch opens what the last one left selected.
        assert_eq!(remote_start_project(None, Some(&key)), Some(key));
        assert_eq!(remote_start_project(None, None), None);
        // Nothing but a remote project's id counts; a damaged one opens the local project.
        for odd in ["", "p1", "remote:h1", "remote::p1", "remote:h1:"] {
            assert_eq!(remote_start_project(None, Some(odd)), None, "{odd:?}");
            assert_eq!(remote_start_project(Some(&restored(Some(odd))), None), None);
        }
    }

    #[test]
    fn no_local_session_belongs_to_a_project_on_another_mac() {
        let key = remote_tree::project_key("h1", "p1");
        let mut global = session(ShellKind::Orchestrator, None);
        global.id = "global".to_owned();
        let project = session(ShellKind::Project, Some("p1"));
        // The global orchestrator is part of every local project, and of no remote one.
        assert!(session_belongs_to_workspace(&global, "p1"));
        assert!(!session_belongs_to_workspace(&global, &key));
        assert!(!session_belongs_to_workspace(&project, &key));
        // Even a session that somehow carried the remote id is not claimed by it.
        let odd = session(ShellKind::Project, Some(&key));
        assert!(!session_belongs_to_workspace(&odd, &key));
        // And nothing local is adopted into a remote project's window.
        assert!(shells_to_adopt(&[project], &key, &HashSet::new(), &HashSet::new()).is_empty());
    }

    #[test]
    fn the_local_store_has_nothing_for_a_remote_projects_key() {
        // Selecting a remote project never goes through the store, and a lookup that did
        // would find nothing rather than another project.
        let key = remote_tree::project_key("h1", "p1");
        let state = State::default();
        assert!(state.project(&key).is_err());
        assert!(state.worktrees_for(&key).is_empty());
        assert!(state.tasks_for_project(&key).is_empty());
        assert!(file_explorer_root(&state, &key, None).is_none());
    }

    #[test]
    fn the_new_tab_menus_agents_map_to_the_hosts_shell_kinds() {
        assert_eq!(remote_shell_kind(HarnessKind::Codex), NewShellKind::Codex);
        assert_eq!(remote_shell_kind(HarnessKind::Claude), NewShellKind::Claude);
        assert_eq!(remote_shell_kind(HarnessKind::Grok), NewShellKind::Grok);
    }

    #[test]
    fn the_bridge_command_is_quoted_and_names_the_host_and_shell() {
        let cli = remote_hosts::RemoteCli::at(PathBuf::from("/Applications/My App/riwork-remote"));
        assert_eq!(
            remote_attach_command(&cli, "h-1", "s-1"),
            "'/Applications/My App/riwork-remote' attach --desktop h-1 --shell s-1"
        );
        let plain = remote_hosts::RemoteCli::at(PathBuf::from("/opt/riwork-remote"));
        assert_eq!(
            remote_attach_command(&plain, "h", "it's"),
            "/opt/riwork-remote attach --desktop h --shell 'it'\\''s'"
        );
    }

    #[test]
    fn a_locked_pane_refuses_user_closes_but_an_unlocked_one_allows_them() {
        for close in [UserClose::Tab, UserClose::Pane] {
            assert_eq!(user_close_refusal(false, close), None);
            assert!(user_close_refusal(true, close).is_some());
        }
        assert_ne!(
            user_close_refusal(true, UserClose::Tab),
            user_close_refusal(true, UserClose::Pane)
        );
        // Only the hints this decision produces are treated as stale on unlock.
        for close in [UserClose::Tab, UserClose::Pane] {
            assert!(is_locked_close_hint(
                user_close_refusal(true, close).unwrap()
            ));
        }
        assert!(!is_locked_close_hint("Copied /tmp/file"));
    }

    #[test]
    fn lock_state_follows_the_users_choice_and_defaults_to_the_navigation_pane() {
        let nav = || true;
        let none = || false;
        // Before any choice the first pane is locked only while it holds a navigation panel.
        assert!(pane_lock_state(None, 1, 1, nav));
        assert!(!pane_lock_state(None, 1, 1, none));
        assert!(!pane_lock_state(None, 2, 1, nav));
        // An explicit set decides on its own, whatever the panes hold.
        let locked = HashSet::from([2]);
        assert!(pane_lock_state(Some(&locked), 2, 1, none));
        assert!(!pane_lock_state(Some(&locked), 1, 1, nav));
        // Unlocking the default navigation pane stores an empty set and stays unlocked.
        assert!(!pane_lock_state(Some(&HashSet::new()), 1, 1, nav));
        // A default-locked pane refuses to be closed; once unlocked it can be.
        let default_locked = pane_lock_state(None, 1, 1, nav);
        assert!(user_close_refusal(default_locked, UserClose::Tab).is_some());
        let unlocked = pane_lock_state(Some(&HashSet::new()), 1, 1, nav);
        assert!(user_close_refusal(unlocked, UserClose::Tab).is_none());
    }

    #[test]
    fn icon_only_panel_tabs_name_their_panel_in_the_tooltip() {
        let panels = [
            (PanelKind::Projects, "Projects"),
            (PanelKind::Worktrees, "Worktrees"),
            (PanelKind::Files, "Files"),
            (PanelKind::Preview, "Preview"),
            (PanelKind::Tasks, "Tasks"),
            (PanelKind::Shells, "Shells"),
            (PanelKind::Usage, "Usage"),
            (PanelKind::Settings, "Settings"),
            (PanelKind::ProjectSettings, "Project Settings"),
            (PanelKind::Schedules, "Schedules"),
        ];
        for (panel, name) in panels {
            assert!(panel_tooltip(panel).starts_with(name), "{panel:?}");
            assert_eq!(
                Workspace::panel_title(panel),
                name.to_uppercase(),
                "the tooltip names the tab's own label"
            );
        }
    }

    #[test]
    fn saved_file_reselects_its_live_editor_without_refresh_but_new_launch_stays_strict() {
        use std::{fs, os::unix::fs::symlink};

        let directory =
            std::env::temp_dir().join(format!("riwork-editor-reselect-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("café space.txt");
        fs::write(&path, "before\n").unwrap();
        let old = file_preview::FileIdentity::of(&fs::metadata(&path).unwrap());
        let root = ExplorerRoot {
            path: directory.clone(),
            label: "fixture".into(),
            worktree_id: Some("worktree-a".into()),
        };
        let mut editor = session(ShellKind::Project, Some("project-a"));
        editor.worktree_id = root.worktree_id.clone();
        editor.editor_path = Some(directory.canonicalize().unwrap().join("café space.txt"));

        // Saving updates the file without refreshing the explorer's old row.
        fs::write(&path, "after save, with a different length\n").unwrap();
        assert_ne!(
            old,
            file_preview::FileIdentity::of(&fs::metadata(&path).unwrap())
        );
        assert!(file_preview::validated_editor_path(&directory, &path, old).is_err());
        assert!(matches!(
            file_editor_target(&root, &path, old, "project-a", &[editor.clone()]),
            Ok(FileEditorTarget::Existing(shell)) if shell.id == editor.id
        ));

        // The project root and its primary worktree are one directory, so an
        // editor opened from either is the same editor.
        for editor_worktree in [None, Some("primary".to_owned())] {
            for root_worktree in [None, Some("worktree-a".to_owned())] {
                let root = ExplorerRoot {
                    worktree_id: root_worktree,
                    ..root.clone()
                };
                let shell = ShellSession {
                    worktree_id: editor_worktree.clone(),
                    ..editor.clone()
                };
                assert!(matches!(
                    file_editor_target(&root, &path, old, "project-a", std::slice::from_ref(&shell)),
                    Ok(FileEditorTarget::Existing(found)) if found.id == shell.id
                ));
            }
        }

        // A dead editor, another project, or another worktree's copy of the
        // file cannot bypass the strict identity check for a new session.
        let other_worktree = directory.join("other-worktree");
        fs::create_dir(&other_worktree).unwrap();
        fs::write(other_worktree.join("café space.txt"), "before\n").unwrap();
        for (project_id, shells) in [
            ("project-a", vec![]),
            ("project-b", vec![editor.clone()]),
            (
                "project-a",
                vec![ShellSession {
                    alive: false,
                    ..editor.clone()
                }],
            ),
            (
                "project-a",
                vec![ShellSession {
                    worktree_id: Some("worktree-b".into()),
                    editor_path: Some(
                        other_worktree
                            .canonicalize()
                            .unwrap()
                            .join("café space.txt"),
                    ),
                    ..editor.clone()
                }],
            ),
        ] {
            assert!(file_editor_target(&root, &path, old, project_id, &shells).is_err());
        }

        // Even a forged live-editor match cannot follow a symlink.
        let link = directory.join("link.txt");
        symlink(&path, &link).unwrap();
        editor.editor_path = Some(directory.canonicalize().unwrap().join("link.txt"));
        assert!(file_editor_target(&root, &link, old, "project-a", &[editor]).is_err());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn closing_a_pane_focuses_the_pane_that_takes_its_space() {
        let pane = |id| Box::new(Layout::Pane(id));
        let split = |first: Box<Layout>, second: Box<Layout>| {
            Box::new(Layout::Split {
                axis: Axis::SideBySide,
                ratio: 0.5,
                first,
                second,
            })
        };
        // nav | (terminal | (top / bottom))
        let layout = Layout::Split {
            axis: Axis::SideBySide,
            ratio: 0.27,
            first: pane(1),
            second: split(pane(2), split(pane(3), pane(4))),
        };
        assert_eq!(pane_inheriting_space(&layout, 4), Some(3));
        assert_eq!(pane_inheriting_space(&layout, 3), Some(4));
        assert_eq!(pane_inheriting_space(&layout, 2), Some(3));
        assert_eq!(pane_inheriting_space(&layout, 1), Some(2));
        // The neighbour next to the closed pane, not the far end of a subtree.
        let layout = Layout::Split {
            axis: Axis::SideBySide,
            ratio: 0.5,
            first: split(pane(1), pane(2)),
            second: pane(3),
        };
        assert_eq!(pane_inheriting_space(&layout, 3), Some(2));
        assert_eq!(pane_inheriting_space(&layout, 1), Some(2));
        assert_eq!(pane_inheriting_space(&layout, 9), None);
        assert_eq!(pane_inheriting_space(&Layout::Pane(1), 1), None);
    }

    #[test]
    fn root_docking_targets_clear_the_window_controls_only_while_they_show() {
        assert_eq!(root_dock_insets(true), (80.0, 30.0));
        assert_eq!(root_dock_insets(false), (0.0, 12.0));
    }

    #[test]
    fn files_follow_the_selected_worktree_and_never_fall_back_from_an_unavailable_selection() {
        let state: State = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "active_project_id": "atlas",
            "projects": [
                {"id":"atlas", "name":"Atlas", "root":"/projects/atlas", "created_at":0},
                {"id":"beacon", "name":"Beacon", "root":"/projects/beacon", "created_at":0}
            ],
            "worktrees": [
                {"id":"main", "project_id":"atlas", "branch":"main", "path":"/projects/atlas", "is_primary":true,"created_at":0},
                {"id":"feature", "project_id":"atlas", "branch":"feature", "path":"/worktrees/feature", "created_at":0},
                {"id":"other", "project_id":"beacon", "branch":"main", "path":"/projects/beacon", "created_at":0}
            ],
            "tasks": []
        })).unwrap();
        assert_eq!(
            file_explorer_root(&state, "atlas", Some("feature"))
                .unwrap()
                .path,
            PathBuf::from("/worktrees/feature")
        );
        assert_eq!(
            file_explorer_root(&state, "atlas", Some("feature"))
                .unwrap()
                .worktree_id
                .as_deref(),
            Some("feature")
        );
        assert_eq!(
            file_explorer_root(&state, "beacon", Some("other"))
                .unwrap()
                .label,
            "Beacon · main"
        );
        assert!(file_explorer_root(&state, "atlas", Some("other")).is_none());
        assert!(file_explorer_root(&state, "atlas", Some("removed")).is_none());
        assert!(file_explorer_root(&state, "removed", None).is_none());
        assert_eq!(
            file_explorer_root(&state, "atlas", None).unwrap().path,
            PathBuf::from("/projects/atlas")
        );
        assert!(
            file_explorer_root(&state, "atlas", None)
                .unwrap()
                .worktree_id
                .is_none()
        );
    }

    #[test]
    fn terminal_palette_override_is_opt_in_and_preserves_user_configuration() {
        let original = Workspace::terminal_options("shell".to_owned(), PathBuf::from("/tmp"), None);
        assert_eq!(original.configuration, TerminalConfiguration::UserDefault);
        let themed = Workspace::terminal_options(
            "shell".to_owned(),
            PathBuf::from("/tmp"),
            Some(theme::riwork_terminal_theme()),
        );
        assert!(matches!(
            themed.configuration,
            TerminalConfiguration::UserDefaultWithOverride(_)
        ));
    }

    #[test]
    fn selected_theme_controls_terminals_and_keeps_legacy_preferences() {
        let mut settings = Settings::default();
        for selected in ThemeChoice::ALL
            .into_iter()
            .filter(|theme| *theme != ThemeChoice::Ghostty)
        {
            settings.theme = selected;
            let appearance = Appearance::resolve(selected, false);
            assert_eq!(
                Workspace::terminal_theme(&settings, &appearance),
                appearance.terminal
            );
            settings.use_riwork_colors = true;
            assert_eq!(
                Workspace::terminal_theme(&settings, &appearance),
                appearance.terminal
            );
        }
        // Native forces its colors only while "Terminals match the theme" is on,
        // whatever the RiWork option says.
        settings.theme = ThemeChoice::Native;
        let native = Appearance::resolve(ThemeChoice::Native, true);
        settings.native_terminal_colors = false;
        assert_eq!(Workspace::terminal_theme(&settings, &native), None);
        settings.native_terminal_colors = true;
        settings.use_riwork_colors = false;
        assert_eq!(
            Workspace::terminal_theme(&settings, &native),
            Some(theme::native_terminal_theme(true))
        );
        settings.use_riwork_colors = true;
        let mut appearance = Appearance::resolve(ThemeChoice::RiWork, false);
        appearance.selected = ThemeChoice::Ghostty;
        appearance.terminal = None;
        settings.theme = ThemeChoice::Ghostty;
        assert_eq!(
            Workspace::terminal_theme(&settings, &appearance),
            Some(theme::riwork_terminal_theme())
        );
        settings.use_riwork_colors = false;
        assert_eq!(Workspace::terminal_theme(&settings, &appearance), None);
    }

    fn session(kind: ShellKind, project: Option<&str>) -> ShellSession {
        ShellSession {
            id: "scope-test".to_owned(),
            project_id: project.map(str::to_owned),
            worktree_id: None,
            kind,
            cwd: PathBuf::from("/tmp"),
            command: None,
            editor_path: None,
            harness: None,
            codex_account_id: None,
            codex_account_label: None,
            codex_account_email: None,
            codex_home: None,
            unrestricted: false,
            orchestrator_skill_loaded: false,
            orchestrator_skill_version: None,
            orchestrator_project_root: None,
            created_at_unix: 0,
            alive: true,
        }
    }

    fn project_shell(id: &str, project: &str) -> ShellSession {
        let mut shell = session(ShellKind::Project, Some(project));
        shell.id = id.to_owned();
        shell
    }

    #[test]
    fn a_terminal_started_outside_the_window_is_adopted_unless_it_was_closed_or_is_not_live() {
        let mut exited = project_shell("exited", "alpha");
        exited.alive = false;
        let shells = vec![
            project_shell("open", "alpha"),
            project_shell("new", "alpha"),
            project_shell("closed-by-user", "alpha"),
            exited,
            project_shell("other-project", "beta"),
            session(ShellKind::Orchestrator, Some("alpha")),
            session(ShellKind::Orchestrator, None),
        ];
        let shown = HashSet::from(["open"]);
        let detached = HashSet::from(["closed-by-user".to_owned()]);
        let ids = |found: Vec<&ShellSession>| {
            found
                .iter()
                .map(|shell| shell.id.clone())
                .collect::<Vec<_>>()
        };
        // Only the live project terminal nothing accounts for. Orchestrators
        // open through their own commands and never as a side effect.
        assert_eq!(
            ids(shells_to_adopt(&shells, "alpha", &shown, &detached)),
            ["new"]
        );
        // Once it has a tab it is not adopted again.
        let shown = HashSet::from(["open", "new"]);
        assert!(shells_to_adopt(&shells, "alpha", &shown, &detached).is_empty());
        // The other project's window adopts the other project's terminal.
        assert_eq!(
            ids(shells_to_adopt(&shells, "beta", &HashSet::new(), &detached)),
            ["other-project"]
        );
    }

    #[test]
    fn a_terminal_that_some_window_already_has_is_claimed_once() {
        let id = "claim-test-4f6c1d2e-0001";
        assert!(claim_shell(id));
        assert!(!claim_shell(id));
        assert!(!claim_shell(id));
        assert!(claim_shell("claim-test-4f6c1d2e-0002"));
    }

    #[test]
    fn a_terminal_a_window_let_go_of_can_be_taken_by_another() {
        let id = "claim-test-4f6c1d2e-0003";
        assert!(claim_shell(id));
        release_shell(id);
        assert!(claim_shell(id));
        // Letting go of what was never claimed is harmless.
        release_shell("claim-test-4f6c1d2e-never");
        assert!(claim_shell("claim-test-4f6c1d2e-never"));
    }

    #[test]
    fn restored_tabs_keep_global_and_project_orchestrators_in_their_scopes() {
        assert!(session_belongs_to_workspace(
            &session(ShellKind::Orchestrator, None),
            "alpha"
        ));
        assert!(session_belongs_to_workspace(
            &session(ShellKind::Orchestrator, Some("alpha")),
            "alpha"
        ));
        assert!(!session_belongs_to_workspace(
            &session(ShellKind::Orchestrator, Some("beta")),
            "alpha"
        ));
        assert!(session_belongs_to_workspace(
            &session(ShellKind::Project, Some("alpha")),
            "alpha"
        ));
        assert!(!session_belongs_to_workspace(
            &session(ShellKind::Project, Some("beta")),
            "alpha"
        ));
        assert!(!session_belongs_to_workspace(
            &session(ShellKind::Project, None),
            "alpha"
        ));
    }

    #[test]
    fn codex_tabs_and_status_share_deterministic_project_account_numbers() {
        let mut first = session(ShellKind::Project, Some("alpha"));
        first.id = "first".into();
        first.harness = Some(HarnessKind::Codex);
        first.codex_account_id = Some("a".into());
        first.codex_account_email = Some("a@example.test".into());
        first.codex_home = Some(PathBuf::from("/managed/a"));
        let mut second = first.clone();
        second.id = "second".into();
        second.codex_account_id = Some("b".into());
        second.codex_account_email = Some("b@example.test".into());
        second.codex_home = Some(PathBuf::from("/managed/b"));
        let mut same_account = first.clone();
        same_account.id = "same-account".into();
        let mut other_project = second.clone();
        other_project.id = "other-project".into();
        other_project.project_id = Some("beta".into());
        let mut global = first.clone();
        global.id = "global".into();
        global.project_id = None;
        global.kind = ShellKind::Orchestrator;
        let shells = [
            second.clone(),
            global.clone(),
            other_project.clone(),
            same_account.clone(),
            first.clone(),
        ];
        let numbers = codex_account_numbers(&shells);
        assert_eq!(numbers.get("first"), Some(&1));
        assert_eq!(numbers.get("same-account"), Some(&1));
        assert_eq!(numbers.get("second"), Some(&2));
        assert!(!numbers.contains_key("other-project"));
        assert!(!numbers.contains_key("global"));
        assert_eq!(
            codex_tab_title("codex 01 · main", Some(&first), &numbers),
            "codex 01 · main · A1"
        );
        assert_eq!(
            codex_session_status_label(&first, &numbers, None),
            "CODEX A1 · a@example.test"
        );
        assert_eq!(
            codex_session_status_label(&second, &numbers, None),
            "CODEX A2 · b@example.test"
        );
        let mut unknown = first.clone();
        unknown.codex_account_email = None;
        assert_eq!(
            codex_session_status_label(&unknown, &numbers, None),
            "CODEX A1 · Email unknown"
        );
        let remaining = codex_account_numbers(&[first.clone(), same_account]);
        assert!(remaining.is_empty());
        assert_eq!(
            codex_tab_title("claude 03 · main", None, &remaining),
            "claude 03 · main"
        );
    }

    #[test]
    fn status_uses_only_public_verified_email_and_marks_project_defaults() {
        let snapshot = codex_accounts::AccountsSnapshot {
            accounts: vec![codex_accounts::CodexAccount {
                id: "a".into(),
                label: "a@example.test · Workspace".into(),
                email: Some("a@example.test".into()),
                home: PathBuf::from("/managed/a"),
                available: true,
                unavailable_reason: None,
                is_system_default: false,
            }],
            ..Default::default()
        };
        let mut old_shell = session(ShellKind::Project, Some("alpha"));
        old_shell.harness = Some(HarnessKind::Codex);
        old_shell.codex_account_id = Some("a".into());
        old_shell.codex_account_label = Some("Workspace".into());
        old_shell.codex_home = Some(PathBuf::from("/managed/a"));
        assert_eq!(
            verified_shell_email(&old_shell, Some(&snapshot)).as_deref(),
            Some("a@example.test")
        );
        old_shell.codex_home = Some(PathBuf::from("/managed/other"));
        assert_eq!(verified_shell_email(&old_shell, Some(&snapshot)), None);
        let mut project: Project = serde_json::from_value(serde_json::json!({
            "id":"alpha", "name":"Alpha", "root":"/alpha", "created_at":1
        }))
        .unwrap();
        assert_eq!(
            project_default_account_label(Some(&project), Some("a"), Some(&snapshot)),
            "DEFAULT (APP) · a@example.test"
        );
        project.codex_account = ProjectCodexAccount::Saved("a".into());
        assert_eq!(
            project_default_account_label(Some(&project), Some("missing"), Some(&snapshot)),
            "DEFAULT (PROJECT) · a@example.test"
        );
        project.codex_account = ProjectCodexAccount::SystemDefault;
        assert_eq!(
            project_default_account_label(Some(&project), Some("a"), Some(&snapshot)),
            "DEFAULT (SYSTEM) · System default · email unknown"
        );
        project.codex_account = ProjectCodexAccount::Saved("missing".into());
        assert_eq!(
            project_default_account_label(Some(&project), None, Some(&snapshot)),
            "DEFAULT (PROJECT) · Account unavailable"
        );
    }
}

/// What a click on a link does to the window's panes when there is no tree on screen: the
/// Preview or Files is put beside the work, and the terminal that was clicked stays on screen.
#[cfg(test)]
mod link_panel_tests {
    use super::*;

    fn shell(id: TabId) -> Tab {
        Tab {
            id,
            title: "zsh".to_owned(),
            content: TabContent::Shell {
                shell_id: format!("shell-{id}"),
                worktree_id: None,
                terminal: None,
                attach_error: None,
                attach_failures: 0,
            },
            hidden_since: None,
        }
    }

    fn panel(id: TabId, kind: PanelKind) -> Tab {
        Tab {
            id,
            title: Workspace::panel_title(kind).to_owned(),
            content: TabContent::Panel(kind),
            hidden_since: None,
        }
    }

    fn pane(tabs: Vec<Tab>, active: usize) -> Pane {
        Pane { tabs, active }
    }

    /// A window's panes without the GPUI parts: what the placement rules read and change.
    struct Window {
        layout: Layout,
        panes: BTreeMap<PaneId, Pane>,
        next_pane_id: PaneId,
        next_tab_id: TabId,
        size: Extent,
    }

    impl Window {
        fn new(layout: Layout, panes: Vec<(PaneId, Pane)>, size: (f32, f32)) -> Self {
            let next_pane_id = panes.iter().map(|(id, _)| *id).max().unwrap_or(0) + 1;
            let next_tab_id = panes
                .iter()
                .flat_map(|(_, pane)| pane.tabs.iter().map(|tab| tab.id))
                .max()
                .unwrap_or(0)
                + 1;
            Self {
                layout,
                panes: panes.into_iter().collect(),
                next_pane_id,
                next_tab_id,
                size: Extent {
                    width: size.0,
                    height: size.1,
                },
            }
        }

        /// One terminal pane with two tabs, the second selected, as a window that has been used.
        fn alone(size: (f32, f32)) -> Self {
            Self::new(
                Layout::Pane(1),
                vec![(1, pane(vec![shell(1), shell(2)], 1))],
                size,
            )
        }

        /// A click on a link in pane `clicked`, as `Workspace::reveal_panel_for_link` handles it
        /// but for the parts that need a window. The pane changed, and whether its selected tab did.
        fn click(
            &mut self,
            kind: PanelKind,
            clicked: PaneId,
            locked: &[PaneId],
        ) -> Option<(PaneId, bool)> {
            let is_locked = |id: PaneId| locked.contains(&id);
            let reveal = plan_link_panel(
                &self.layout,
                &self.panes,
                Some(self.size),
                &is_locked,
                kind,
                clicked,
            );
            apply_panel_reveal_to_panes(
                &mut self.layout,
                &mut self.panes,
                &mut self.next_pane_id,
                &mut self.next_tab_id,
                kind,
                Workspace::panel_title(kind),
                reveal,
            )
        }

        fn tab_ids(&self, pane: PaneId) -> Vec<TabId> {
            self.panes[&pane].tabs.iter().map(|tab| tab.id).collect()
        }

        fn panels(&self, kind: PanelKind) -> usize {
            self.panes
                .values()
                .flat_map(|pane| &pane.tabs)
                .filter(|tab| tab.panel() == Some(kind))
                .count()
        }
    }

    #[test]
    fn a_link_click_splits_the_only_terminal_pane_and_the_terminal_keeps_its_tab() {
        for kind in [PanelKind::Preview, PanelKind::Files] {
            let mut window = Window::alone((1400.0, 900.0));
            let before = window.layout.pane_extents(window.size)[&1];
            assert_eq!(window.click(kind, 1, &[]), Some((2, true)), "{kind:?}");

            // The terminal pane is split, keeps 60% of its space, and keeps its tabs and the
            // selected one; the keys were never moved off it.
            let after = window.layout.pane_extents(window.size);
            let whole = before.width - DIVIDER_THICKNESS;
            assert!(
                (after[&1].width - whole * layouts::PREVIEW_BESIDE_OTHER_RATIO).abs() < 0.01,
                "{after:?}"
            );
            assert_eq!(after[&1].height, before.height);
            assert!(after[&2].width > 245.0);
            assert_eq!(window.tab_ids(1), [1, 2]);
            assert_eq!(window.panes[&1].active, 1);
            assert!(pane_shows_shell_in(&window.panes, 1));

            // The new pane holds the panel, selected, and nothing else.
            assert_eq!(window.tab_ids(2), [3]);
            assert_eq!(panel_tab_in(&window.panes, kind), Some((2, 3, true)));
            assert_eq!(window.panels(kind), 1);
        }
    }

    #[test]
    fn a_narrow_tall_terminal_pane_is_split_below() {
        let mut window = Window::alone((layouts::PREVIEW_SIDE_BY_SIDE_MIN_WIDTH - 1.0, 900.0));
        assert_eq!(window.click(PanelKind::Preview, 1, &[]), Some((2, true)));
        let after = window.layout.pane_extents(window.size);
        assert_eq!(after[&1].width, after[&2].width);
        assert!(after[&1].height > after[&2].height);
        assert_eq!(window.panes[&1].active, 1);
    }

    #[test]
    fn with_everything_locked_the_panel_is_added_unselected_and_the_terminal_stays_shown() {
        // The default window: a locked navigation pane (showing Projects) and a locked terminal
        // pane, in a window big enough to split if nothing were locked.
        let mut layout = Layout::Pane(1);
        assert!(layout.split_with_ratio(1, Axis::SideBySide, 2, false, 0.27));
        let navigation = pane(vec![panel(1, PanelKind::Projects)], 0);
        let terminal = pane(vec![shell(2)], 0);
        let mut window = Window::new(
            layout,
            vec![(1, navigation), (2, terminal)],
            (1600.0, 900.0),
        );
        let extents = window.layout.pane_extents(window.size);

        assert_eq!(
            window.click(PanelKind::Preview, 2, &[1, 2]),
            Some((1, false))
        );
        // No pane was made, the terminal is untouched, and the navigation pane still shows
        // Projects: the Preview waits in its tab strip.
        assert_eq!(window.layout.pane_extents(window.size), extents);
        assert_eq!(window.tab_ids(2), [2]);
        assert!(pane_shows_shell_in(&window.panes, 2));
        assert_eq!(window.tab_ids(1), [1, 3]);
        assert_eq!(window.panes[&1].active, 0);
        assert_eq!(
            panel_tab_in(&window.panes, PanelKind::Preview),
            Some((1, 3, false))
        );

        // A lone locked terminal pane: the panel is a tab behind the terminal, still unselected.
        let mut alone = Window::alone((1600.0, 900.0));
        assert_eq!(alone.click(PanelKind::Files, 1, &[1]), Some((1, false)));
        assert_eq!(alone.tab_ids(1), [1, 2, 3]);
        assert_eq!(alone.panes[&1].active, 1);
        assert!(pane_shows_shell_in(&alone.panes, 1));
        assert_eq!(
            panel_tab_in(&alone.panes, PanelKind::Files),
            Some((1, 3, false))
        );
    }

    #[test]
    fn an_existing_tab_is_reused_and_never_brought_over_a_terminal() {
        let mut window = Window::alone((1400.0, 900.0));
        assert_eq!(window.click(PanelKind::Preview, 1, &[]), Some((2, true)));

        // Clicking again while it is showing changes nothing, and makes no second one.
        let panes_before = window.layout.pane_ids();
        assert_eq!(window.click(PanelKind::Preview, 1, &[]), None);
        assert_eq!(window.layout.pane_ids(), panes_before);
        assert_eq!(window.panels(PanelKind::Preview), 1);

        // Hidden behind another panel in its pane: brought forward there, not duplicated.
        let preview_pane = window.panes.get_mut(&2).unwrap();
        push_panel_tab(preview_pane, 10, "USAGE", PanelKind::Usage, true);
        assert_eq!(
            panel_tab_in(&window.panes, PanelKind::Preview),
            Some((2, 3, false))
        );
        assert_eq!(window.click(PanelKind::Preview, 1, &[]), Some((2, true)));
        assert_eq!(
            panel_tab_in(&window.panes, PanelKind::Preview),
            Some((2, 3, true))
        );
        assert_eq!(window.panels(PanelKind::Preview), 1);
        assert_eq!(window.panes[&1].active, 1);

        // Hidden behind a terminal: left there, so that the terminal stays on screen.
        let behind = window.panes.get_mut(&2).unwrap();
        behind.tabs.push(shell(12));
        behind.active = behind.tabs.len() - 1;
        assert_eq!(window.click(PanelKind::Preview, 1, &[]), None);
        assert_eq!(
            panel_tab_in(&window.panes, PanelKind::Preview),
            Some((2, 3, false))
        );
        assert!(pane_shows_shell_in(&window.panes, 2));

        // In the pane that was clicked, behind the terminal clicked in: left as well.
        let mut window = Window::alone((1400.0, 900.0));
        push_panel_tab(
            window.panes.get_mut(&1).unwrap(),
            5,
            "PREVIEW",
            PanelKind::Preview,
            false,
        );
        assert_eq!(window.click(PanelKind::Preview, 1, &[]), None);
        assert_eq!(window.panes[&1].active, 1);
    }

    #[test]
    fn an_existing_files_tab_is_brought_forward_beside_a_terminal_not_over_it() {
        // Files is a tab in a navigation pane behind Projects: a folder link selects it there.
        let mut layout = Layout::Pane(1);
        assert!(layout.split_with_ratio(1, Axis::SideBySide, 2, false, 0.27));
        let navigation = pane(
            vec![panel(1, PanelKind::Projects), panel(2, PanelKind::Files)],
            0,
        );
        let mut window = Window::new(
            layout,
            vec![(1, navigation), (2, pane(vec![shell(3)], 0))],
            (1600.0, 900.0),
        );
        assert_eq!(window.click(PanelKind::Files, 2, &[1, 2]), Some((1, true)));
        assert_eq!(window.panes[&1].active, 1);
        assert_eq!(window.panels(PanelKind::Files), 1);
        assert!(pane_shows_shell_in(&window.panes, 2));

        // Files is a tab in the terminal's own pane, behind the terminal: it stays there.
        let mut window = Window::alone((1400.0, 900.0));
        push_panel_tab(
            window.panes.get_mut(&1).unwrap(),
            5,
            "FILES",
            PanelKind::Files,
            false,
        );
        assert_eq!(window.click(PanelKind::Files, 1, &[]), None);
        assert_eq!(window.panels(PanelKind::Files), 1);
    }

    #[test]
    fn a_tree_behind_the_terminal_clicked_is_not_a_tree_to_go_beside() {
        // A file chosen in the tree has its preview beside the tree, wherever the tree is.
        assert_eq!(tree_pane_for_reveal(Some(3), false, 3), Some(3));
        assert_eq!(tree_pane_for_reveal(Some(3), false, 1), Some(3));
        // A link is clicked in a terminal. A tree in another pane is on screen; one that is a tab
        // in the clicked pane's own strip is behind the terminal, and the preview goes beside the
        // terminal rather than shrinking it to the tree's narrow share.
        assert_eq!(tree_pane_for_reveal(Some(3), true, 1), Some(3));
        assert_eq!(tree_pane_for_reveal(Some(3), true, 3), None);
        assert_eq!(tree_pane_for_reveal(None, true, 1), None);
    }

    #[test]
    fn the_notice_says_where_a_hidden_panel_is() {
        assert_eq!(
            hidden_panel_notice(PanelKind::Preview, true),
            "Preview is in this pane's tab strip, behind the terminal."
        );
        assert_eq!(
            hidden_panel_notice(PanelKind::Files, false),
            "Files is in the tab strip of another pane."
        );
    }

    #[test]
    fn a_placement_that_names_a_missing_pane_changes_nothing() {
        let mut window = Window::alone((1400.0, 900.0));
        let missing = PreviewPlacement::Split {
            target: 9,
            axis: Axis::SideBySide,
            ratio: 0.6,
        };
        let result = apply_panel_placement(
            &mut window.layout,
            &mut window.panes,
            &mut window.next_pane_id,
            &mut window.next_tab_id,
            PanelKind::Preview,
            "PREVIEW",
            missing,
        );
        assert_eq!(result, None);
        let tab = PreviewPlacement::Tab {
            pane: 9,
            activate: true,
        };
        assert_eq!(
            apply_panel_placement(
                &mut window.layout,
                &mut window.panes,
                &mut window.next_pane_id,
                &mut window.next_tab_id,
                PanelKind::Preview,
                "PREVIEW",
                tab,
            ),
            None
        );
        assert_eq!(window.layout.pane_ids(), [1]);
        assert_eq!(window.panels(PanelKind::Preview), 0);
    }
}

#[cfg(test)]
mod main_pane_tests {
    use super::*;

    fn shell(id: TabId) -> Tab {
        Tab {
            id,
            title: "zsh".to_owned(),
            content: TabContent::Shell {
                shell_id: format!("shell-{id}"),
                worktree_id: None,
                terminal: None,
                attach_error: None,
                attach_failures: 0,
            },
            hidden_since: None,
        }
    }

    fn panel(id: TabId, kind: PanelKind) -> Tab {
        Tab {
            id,
            title: Workspace::panel_title(kind).to_owned(),
            content: TabContent::Panel(kind),
            hidden_since: None,
        }
    }

    fn pane(tabs: Vec<Tab>, active: usize) -> Pane {
        Pane { tabs, active }
    }

    fn side(ratio: f32, first: Layout, second: Layout) -> Layout {
        Layout::Split {
            axis: Axis::SideBySide,
            ratio,
            first: Box::new(first),
            second: Box::new(second),
        }
    }

    fn stack(ratio: f32, first: Layout, second: Layout) -> Layout {
        Layout::Split {
            axis: Axis::Stacked,
            ratio,
            first: Box::new(first),
            second: Box::new(second),
        }
    }

    const AREA: Extent = Extent {
        width: 1600.0,
        height: 1000.0,
    };

    /// A window's panes without the GPUI parts, with its main pane and its locks.
    struct Window {
        layout: Layout,
        panes: BTreeMap<PaneId, Pane>,
        main: Option<PaneId>,
        locked: Vec<PaneId>,
        next_pane_id: PaneId,
        next_tab_id: TabId,
    }

    impl Window {
        fn new(
            layout: Layout,
            panes: Vec<(PaneId, Pane)>,
            main: Option<PaneId>,
            locked: &[PaneId],
        ) -> Self {
            let next_pane_id = panes.iter().map(|(id, _)| *id).max().unwrap_or(0) + 1;
            let next_tab_id = panes
                .iter()
                .flat_map(|(_, pane)| pane.tabs.iter().map(|tab| tab.id))
                .max()
                .unwrap_or(0)
                + 1;
            Self {
                layout,
                panes: panes.into_iter().collect(),
                main,
                locked: locked.to_vec(),
                next_pane_id,
                next_tab_id,
            }
        }

        /// The window the user described: a locked navigation pane (1) showing Files, the main
        /// pane (2) with two shells, the second selected, and another pane (3) with a shell.
        fn navigation_and_work() -> Self {
            Self::new(
                side(
                    0.27,
                    Layout::Pane(1),
                    side(0.6, Layout::Pane(2), Layout::Pane(3)),
                ),
                vec![
                    (
                        1,
                        pane(
                            vec![
                                panel(1, PanelKind::Projects),
                                panel(2, PanelKind::Files),
                                panel(3, PanelKind::Shells),
                            ],
                            1,
                        ),
                    ),
                    (2, pane(vec![shell(4), shell(5)], 1)),
                    (3, pane(vec![shell(6)], 0)),
                ],
                Some(2),
                &[1],
            )
        }

        fn is_locked(&self, id: PaneId) -> bool {
            self.locked.contains(&id)
        }

        /// A file chosen in the tree (`asked` false), or a link clicked in pane `clicked`, as
        /// `Workspace::reveal_preview` and `reveal_panel_for_link` handle it but for the parts
        /// that need a window. The pane that changed, and whether its selected tab did.
        fn reveal(
            &mut self,
            panel: PanelKind,
            asked: bool,
            clicked: PaneId,
        ) -> Option<(PaneId, bool)> {
            let main = self.main?;
            let reveal = plan_main_panel(&self.panes, main, panel, asked.then_some(clicked));
            apply_panel_reveal_to_panes(
                &mut self.layout,
                &mut self.panes,
                &mut self.next_pane_id,
                &mut self.next_tab_id,
                panel,
                Workspace::panel_title(panel),
                reveal,
            )
        }

        fn tab_ids(&self, pane: PaneId) -> Vec<TabId> {
            self.panes[&pane].tabs.iter().map(|tab| tab.id).collect()
        }

        fn shown(&self, pane: PaneId) -> TabId {
            let pane = &self.panes[&pane];
            pane.tabs[pane.active].id
        }

        fn all_tab_ids(&self) -> BTreeSet<TabId> {
            self.panes
                .values()
                .flat_map(|pane| pane.tabs.iter().map(|tab| tab.id))
                .collect()
        }

        fn gather(&mut self) -> Gathered {
            let locked = self.locked.clone();
            gather_panes(
                &mut self.layout,
                &mut self.panes,
                self.main.unwrap(),
                &|id| locked.contains(&id),
            )
        }
    }

    #[test]
    fn only_a_main_pane_that_exists_outside_focus_mode_takes_what_is_opened() {
        let panes = BTreeMap::from([(1, pane(vec![], 0)), (2, pane(vec![], 0))]);
        assert_eq!(routing_target(Some(2), &panes, false), Some(2));
        // No main pane: nothing is routed, and new tabs go to the selected pane as before.
        assert_eq!(routing_target(None, &panes, false), None);
        // Focus mode shows one pane, and a tab sent to another would be out of sight.
        assert_eq!(routing_target(Some(2), &panes, true), None);
        // A main pane that has since been closed counts for nothing.
        assert_eq!(routing_target(Some(9), &panes, false), None);
    }

    #[test]
    fn a_file_chosen_in_the_tree_opens_the_preview_in_the_main_pane_and_splits_nothing() {
        let mut window = Window::navigation_and_work();
        let panes_before = window.layout.pane_ids();
        let sizes_before = window.layout.pane_extents(AREA);

        // The preview is a selected tab of the main pane, after its shells.
        assert_eq!(window.reveal(PanelKind::Preview, false, 1), Some((2, true)));
        assert_eq!(window.tab_ids(2), [4, 5, 7]);
        assert_eq!(
            panel_tab_in(&window.panes, PanelKind::Preview),
            Some((2, 7, true))
        );
        // No pane was made or resized; the tree and the other pane are exactly as they were.
        assert_eq!(window.layout.pane_ids(), panes_before);
        assert_eq!(window.layout.pane_extents(AREA), sizes_before);
        assert_eq!(window.tab_ids(1), [1, 2, 3]);
        assert_eq!(window.shown(1), 2);
        assert_eq!(window.tab_ids(3), [6]);

        // The next file finds it on screen and leaves everything alone.
        assert_eq!(window.reveal(PanelKind::Preview, false, 1), None);
        assert_eq!(window.tab_ids(2), [4, 5, 7]);
    }

    #[test]
    fn with_every_other_pane_locked_the_preview_still_opens_in_the_main_pane() {
        // Two locked panes beside the main pane, in a window big enough to split anything:
        // before, the preview would have had to be squeezed in; now it is a tab.
        let layout = side(
            0.2,
            Layout::Pane(1),
            stack(0.7, Layout::Pane(2), Layout::Pane(3)),
        );
        let mut window = Window::new(
            layout,
            vec![
                (1, pane(vec![panel(1, PanelKind::Files)], 0)),
                (2, pane(vec![shell(2)], 0)),
                (3, pane(vec![shell(3), shell(4)], 1)),
            ],
            Some(2),
            &[1, 3],
        );
        let sizes = window.layout.pane_extents(AREA);
        assert_eq!(window.reveal(PanelKind::Preview, false, 1), Some((2, true)));
        assert_eq!(window.layout.pane_extents(AREA), sizes);
        assert_eq!(window.tab_ids(1), [1]);
        assert_eq!(window.tab_ids(3), [3, 4]);
        assert_eq!(window.tab_ids(2), [2, 5]);

        // A locked pane may itself be the main pane, when the user chose that.
        window.main = Some(3);
        window.panes.get_mut(&2).unwrap().tabs.pop();
        assert_eq!(window.reveal(PanelKind::Preview, false, 1), Some((3, true)));
        assert_eq!(window.tab_ids(3), [3, 4, 6]);
        assert_eq!(window.shown(3), 6);
    }

    #[test]
    fn a_link_clicked_in_a_terminal_never_covers_that_terminal() {
        // Clicked in the main pane's own terminal: the preview waits in its strip, and the
        // terminal stays in front and keeps the keys.
        let mut window = Window::navigation_and_work();
        assert_eq!(window.reveal(PanelKind::Preview, true, 2), Some((2, false)));
        assert_eq!(window.tab_ids(2), [4, 5, 7]);
        assert_eq!(window.shown(2), 5);
        assert_eq!(
            panel_tab_in(&window.panes, PanelKind::Preview),
            Some((2, 7, false))
        );
        // Clicking again leaves it there; it is not brought over the terminal.
        assert_eq!(window.reveal(PanelKind::Preview, true, 2), None);
        assert_eq!(window.shown(2), 5);

        // Clicked in a terminal of another pane: the preview is the selected tab of the main
        // pane, and the clicked terminal is where it was.
        let mut window = Window::navigation_and_work();
        assert_eq!(window.reveal(PanelKind::Preview, true, 3), Some((2, true)));
        assert_eq!(window.shown(2), 7);
        assert_eq!(window.tab_ids(3), [6]);
        assert!(pane_shows_shell_in(&window.panes, 3));

        // A folder link asks for Files the same way; with the tree already on screen in the
        // locked pane nothing moves.
        assert_eq!(window.reveal(PanelKind::Files, true, 3), None);
        assert_eq!(window.tab_ids(1), [1, 2, 3]);
    }

    #[test]
    fn a_preview_tab_that_is_open_is_reused_and_never_copied() {
        // Hidden behind a terminal of another unlocked pane: it moves into the main pane,
        // selected, and the pane it left keeps showing its terminal.
        let mut window = Window::navigation_and_work();
        window
            .panes
            .get_mut(&3)
            .unwrap()
            .tabs
            .insert(0, panel(8, PanelKind::Preview));
        window.panes.get_mut(&3).unwrap().active = 1;
        assert_eq!(window.reveal(PanelKind::Preview, false, 1), Some((2, true)));
        assert_eq!(window.tab_ids(2), [4, 5, 8]);
        assert_eq!(window.shown(2), 8);
        assert_eq!(window.tab_ids(3), [6]);
        assert_eq!(window.shown(3), 6);
        assert_eq!(
            window
                .panes
                .values()
                .flat_map(|pane| &pane.tabs)
                .filter(|tab| tab.panel() == Some(PanelKind::Preview))
                .count(),
            1
        );

        // Behind the tree in the locked navigation pane: it leaves for the main pane, since
        // bringing it forward there would hide the tree being navigated. The navigation pane
        // keeps its other tabs and shows the tree.
        let mut window = Window::navigation_and_work();
        window
            .panes
            .get_mut(&1)
            .unwrap()
            .tabs
            .push(panel(8, PanelKind::Preview));
        assert_eq!(window.reveal(PanelKind::Preview, false, 1), Some((2, true)));
        assert_eq!(window.tab_ids(1), [1, 2, 3]);
        assert_eq!(window.shown(1), 2);
        assert_eq!(window.tab_ids(2), [4, 5, 8]);
        assert_eq!(window.shown(2), 8);

        // Behind another panel in a pane that shows no terminal: brought forward where it is.
        let mut window = Window::navigation_and_work();
        window.panes.insert(
            3,
            pane(
                vec![panel(8, PanelKind::Preview), panel(9, PanelKind::Usage)],
                1,
            ),
        );
        assert_eq!(window.reveal(PanelKind::Preview, false, 1), Some((3, true)));
        assert_eq!(window.shown(3), 8);
        assert_eq!(window.tab_ids(2), [4, 5]);

        // Already in the main pane behind a shell: a file chosen brings it forward.
        let mut window = Window::navigation_and_work();
        window
            .panes
            .get_mut(&2)
            .unwrap()
            .tabs
            .insert(0, panel(8, PanelKind::Preview));
        window.panes.get_mut(&2).unwrap().active = 2;
        assert_eq!(window.reveal(PanelKind::Preview, false, 1), Some((2, true)));
        assert_eq!(window.shown(2), 8);
    }

    #[test]
    fn a_link_clicked_elsewhere_does_not_bring_a_preview_over_the_tree_either() {
        // The tree is on screen in the locked navigation pane with the Preview behind it. A
        // link clicked in the terminal of pane 3 asks for the Preview: it moves into the main
        // pane, where it is the selected tab, instead of replacing the tree.
        let mut window = Window::navigation_and_work();
        window
            .panes
            .get_mut(&1)
            .unwrap()
            .tabs
            .push(panel(8, PanelKind::Preview));
        assert_eq!(window.reveal(PanelKind::Preview, true, 3), Some((2, true)));
        assert_eq!(window.tab_ids(1), [1, 2, 3]);
        assert_eq!(window.shown(1), 2);
        assert_eq!(window.tab_ids(2), [4, 5, 8]);
        assert_eq!(window.shown(2), 8);

        // Files itself is not kept back by the tree it is: a folder link brings it forward in
        // its pane when that hides no terminal.
        let mut window = Window::navigation_and_work();
        window.panes.get_mut(&1).unwrap().active = 0;
        assert_eq!(window.reveal(PanelKind::Files, true, 3), Some((1, true)));
        assert_eq!(window.shown(1), 2);
    }

    #[test]
    fn a_main_pane_that_holds_the_tree_keeps_it_in_front() {
        // Files and the shells share the main pane. Choosing a file adds the preview behind the
        // tree rather than hiding the tree being navigated.
        let mut window = Window::new(
            Layout::Pane(1),
            vec![(1, pane(vec![shell(1), panel(2, PanelKind::Files)], 1))],
            Some(1),
            &[],
        );
        assert_eq!(
            window.reveal(PanelKind::Preview, false, 1),
            Some((1, false))
        );
        assert_eq!(window.tab_ids(1), [1, 2, 3]);
        assert_eq!(window.shown(1), 2);
    }

    #[test]
    fn a_tab_moves_between_panes_and_the_pane_it_left_keeps_what_it_showed() {
        let mut window = Window::navigation_and_work();
        window.panes.get_mut(&3).unwrap().tabs.push(shell(7));
        window.panes.get_mut(&3).unwrap().tabs.push(shell(8));
        window.panes.get_mut(&3).unwrap().active = 1;

        // The tab shown leaves: a neighbour is shown; one that is not shown leaves unnoticed.
        assert!(move_tab_between_panes(&mut window.panes, 3, 7, 2, true));
        assert_eq!(window.tab_ids(3), [6, 8]);
        assert_eq!(window.shown(3), 8);
        assert_eq!(window.tab_ids(2), [4, 5, 7]);
        assert_eq!(window.shown(2), 7);
        assert!(move_tab_between_panes(&mut window.panes, 3, 6, 2, false));
        assert_eq!(window.tab_ids(3), [8]);
        assert_eq!(window.shown(3), 8);
        assert_eq!(window.shown(2), 7);
        assert_eq!(window.tab_ids(2), [4, 5, 7, 6]);

        // Nothing moves for a missing pane or tab, or into the pane it is in.
        assert!(!move_tab_between_panes(&mut window.panes, 3, 99, 2, true));
        assert!(!move_tab_between_panes(&mut window.panes, 3, 8, 9, true));
        assert!(!move_tab_between_panes(&mut window.panes, 9, 8, 2, true));
        assert!(!move_tab_between_panes(&mut window.panes, 2, 4, 2, true));
        assert_eq!(window.tab_ids(3), [8]);
    }

    #[test]
    fn a_closed_pane_hands_its_tabs_to_the_main_pane_after_its_own() {
        let mut window = Window::navigation_and_work();
        window
            .panes
            .get_mut(&3)
            .unwrap()
            .tabs
            .push(panel(7, PanelKind::Usage));
        window.panes.get_mut(&3).unwrap().tabs.push(shell(8));

        let target = aggregation_target(window.main, 3, window.is_locked(3));
        assert_eq!(target, Some(2));
        let closed = close_pane_tabs(&mut window.panes, 3, target);
        // Nothing is closed: the tabs are in the main pane, in their order, behind its own,
        // and the main pane shows what it showed.
        assert!(closed.is_empty());
        assert!(!window.panes.contains_key(&3));
        assert_eq!(window.tab_ids(2), [4, 5, 6, 7, 8]);
        assert_eq!(window.shown(2), 5);
        // The shells in them are still tabs of the window: none was detached.
        assert_eq!(
            window.panes[&2]
                .tabs
                .iter()
                .filter_map(Tab::shell_id)
                .collect::<Vec<_>>(),
            ["shell-4", "shell-5", "shell-6", "shell-8"]
        );
        // The locked pane was not touched.
        assert_eq!(window.tab_ids(1), [1, 2, 3]);
        assert_eq!(window.shown(1), 2);

        // An empty main pane shows the first tab that arrives.
        let mut window = Window::navigation_and_work();
        window.panes.get_mut(&2).unwrap().tabs.clear();
        window.panes.get_mut(&2).unwrap().active = 0;
        assert!(close_pane_tabs(&mut window.panes, 3, Some(2)).is_empty());
        assert_eq!(window.tab_ids(2), [6]);
        assert_eq!(window.shown(2), 6);
    }

    #[test]
    fn closing_the_main_pane_or_a_locked_one_or_having_no_main_pane_closes_the_tabs_as_before() {
        // The main pane itself: its tabs are closed with it, and no pane takes them.
        assert_eq!(aggregation_target(Some(2), 2, false), None);
        let mut window = Window::navigation_and_work();
        let closed = close_pane_tabs(&mut window.panes, 2, None);
        assert_eq!(closed.iter().map(|tab| tab.id).collect::<Vec<_>>(), [4, 5]);
        assert_eq!(window.tab_ids(3), [6]);
        assert_eq!(window.tab_ids(1), [1, 2, 3]);

        // A locked pane's tabs are not moved; no main pane means no one to take them.
        assert_eq!(aggregation_target(Some(2), 1, true), None);
        assert_eq!(aggregation_target(None, 3, false), None);
        let mut window = Window::navigation_and_work();
        let closed = close_pane_tabs(&mut window.panes, 3, None);
        assert_eq!(closed.len(), 1);
        assert_eq!(window.tab_ids(2), [4, 5]);

        // A main pane that has gone takes nothing either.
        let mut window = Window::navigation_and_work();
        let closed = close_pane_tabs(&mut window.panes, 3, Some(9));
        assert_eq!(closed.len(), 1);
    }

    /// nav 1 (locked) | (main 2 | 3) over 4: the shape the status bar's layout makes, with
    /// every other pane to the right of the navigation.
    fn right_of_a_locked_navigation_pane() -> Window {
        Window::new(
            side(
                0.27,
                Layout::Pane(1),
                stack(
                    0.75,
                    side(0.6, Layout::Pane(2), Layout::Pane(3)),
                    Layout::Pane(4),
                ),
            ),
            vec![
                (
                    1,
                    pane(
                        vec![
                            panel(1, PanelKind::Projects),
                            panel(2, PanelKind::Files),
                            panel(3, PanelKind::Shells),
                        ],
                        2,
                    ),
                ),
                (2, pane(vec![shell(4), shell(5)], 0)),
                (3, pane(vec![shell(6), panel(7, PanelKind::Usage)], 1)),
                (4, pane(vec![shell(8)], 0)),
            ],
            Some(2),
            &[1],
        )
    }

    #[test]
    fn a_gather_moves_every_unlocked_tab_into_the_main_pane_and_leaves_the_locked_pane_alone() {
        let mut window = right_of_a_locked_navigation_pane();
        let tabs = window.all_tab_ids();
        let navigation = window.layout.pane_extents(AREA)[&1];

        let gathered = window.gather();
        assert_eq!(
            gathered,
            Gathered {
                moved: 3,
                removed: vec![3, 4],
                kept: 0
            }
        );
        // The emptied panes are gone, from the layout and the panes.
        assert_eq!(window.layout.pane_ids(), [1, 2]);
        assert_eq!(window.panes.keys().copied().collect::<Vec<_>>(), [1, 2]);
        // Tabs arrive pane by pane in layout order, each pane's in its own order, after the main
        // pane's, and the main pane keeps showing what it showed.
        assert_eq!(window.tab_ids(2), [4, 5, 6, 7, 8]);
        assert_eq!(window.shown(2), 4);
        // No tab was closed, lost or copied.
        assert_eq!(window.all_tab_ids(), tabs);
        // The locked pane has its tabs, its selection and its size.
        assert_eq!(window.tab_ids(1), [1, 2, 3]);
        assert_eq!(window.shown(1), 3);
        assert_eq!(window.layout.pane_extents(AREA)[&1], navigation);
        assert_eq!(window.layout.first_pane(), 1);

        // A second gather has nothing left to do.
        let again = window.gather();
        assert_eq!(again, Gathered::default());
        assert_eq!(window.tab_ids(2), [4, 5, 6, 7, 8]);
    }

    #[test]
    fn a_gather_never_touches_any_locked_pane_among_several() {
        // nav 1 | (main 2 | 3), with a locked pane 4 along the bottom and 5 beside the main one.
        let mut window = Window::new(
            stack(
                0.8,
                side(
                    0.2,
                    Layout::Pane(1),
                    side(0.6, Layout::Pane(2), Layout::Pane(3)),
                ),
                side(0.5, Layout::Pane(4), Layout::Pane(5)),
            ),
            vec![
                (1, pane(vec![panel(1, PanelKind::Files)], 0)),
                (2, pane(vec![shell(2)], 0)),
                (3, pane(vec![shell(3), shell(4)], 1)),
                (4, pane(vec![shell(5), shell(6)], 1)),
                (5, pane(vec![shell(7)], 0)),
            ],
            Some(2),
            &[1, 4],
        );
        let sizes = window.layout.pane_extents(AREA);
        let gathered = window.gather();

        // Pane 5 sits beside locked pane 4: removing it would widen 4, so it is emptied and kept.
        assert_eq!(
            gathered,
            Gathered {
                moved: 3,
                removed: vec![3],
                kept: 1
            }
        );
        assert_eq!(window.tab_ids(2), [2, 3, 4, 7]);
        assert!(window.tab_ids(5).is_empty());
        for locked in [1, 4] {
            assert_eq!(window.layout.pane_extents(AREA)[&locked], sizes[&locked]);
        }
        assert_eq!(window.tab_ids(4), [5, 6]);
        assert_eq!(window.shown(4), 6);
        assert_eq!(window.tab_ids(1), [1]);
    }

    #[test]
    fn a_gather_with_nothing_to_gather_changes_nothing() {
        // Only the main pane and a locked one.
        let mut window = Window::new(
            side(0.27, Layout::Pane(1), Layout::Pane(2)),
            vec![
                (1, pane(vec![panel(1, PanelKind::Files)], 0)),
                (2, pane(vec![shell(2)], 0)),
            ],
            Some(2),
            &[1],
        );
        assert_eq!(window.gather(), Gathered::default());
        assert_eq!(window.layout.pane_ids(), [1, 2]);

        // A main pane that is not in the window gathers nothing and removes nothing.
        let mut window = Window::navigation_and_work();
        window.main = Some(9);
        assert_eq!(window.gather(), Gathered::default());
        assert_eq!(window.layout.pane_ids(), [1, 2, 3]);
        assert_eq!(window.tab_ids(3), [6]);
    }

    #[test]
    fn the_gather_says_what_it_did() {
        let gathered = |moved, kept| Gathered {
            moved,
            removed: Vec::new(),
            kept,
        };
        assert_eq!(
            gather_notice(&gathered(3, 0)),
            "Gathered 3 tabs into the main pane"
        );
        assert_eq!(
            gather_notice(&gathered(1, 0)),
            "Gathered 1 tab into the main pane"
        );
        assert!(gather_notice(&gathered(0, 0)).starts_with("No tabs to gather"));
        assert!(gather_notice(&gathered(2, 1)).contains("An empty pane stays beside a locked one"));
        assert!(gather_notice(&gathered(2, 2)).contains("2 empty panes stay"));
    }

    /// The tree, each pane's tabs and its selected one, and the main pane.
    type Picture = (Layout, Vec<(PaneId, Vec<TabId>, TabId)>, Option<PaneId>);

    impl Window {
        /// `Workspace::apply_default_layout`, but for the parts that need a window: the lock
        /// and the main pane it then chooses. `active_pane` is the selected pane.
        fn default_layout(&mut self, active_pane: PaneId) -> Option<DefaultLayout> {
            let locked = self.locked.clone();
            let applied = default_layout_panes(
                &mut self.layout,
                &mut self.panes,
                self.main,
                active_pane,
                &|id| locked.contains(&id),
                &mut self.next_pane_id,
                &mut self.next_tab_id,
            )?;
            self.locked = vec![applied.navigation];
            self.main = Some(applied.main);
            Some(applied)
        }

        fn all_tab_ids_of(&self, pane: PaneId) -> BTreeSet<TabId> {
            self.tab_ids(pane).into_iter().collect()
        }

        fn panels(&self, pane: PaneId) -> Vec<Option<PanelKind>> {
            self.panes[&pane].tabs.iter().map(Tab::panel).collect()
        }

        /// Everything about the window that a person can see or that is saved with it.
        fn picture(&self) -> Picture {
            (
                self.layout.clone(),
                self.panes
                    .iter()
                    .map(|(id, pane)| (*id, self.tab_ids(*id), pane.tabs[pane.active].id))
                    .collect(),
                self.main,
            )
        }
    }

    /// The five navigation panels as tabs, in order.
    fn navigation_tabs() -> Vec<Tab> {
        NAVIGATION_PANELS
            .into_iter()
            .map(|kind| panel(kind as TabId + 1, kind))
            .collect()
    }

    /// A window nobody arranged: six panes in nested splits. Pane 1 is locked and holds Files with
    /// a shell and the Preview; pane 2 is the main pane; panels and shells are scattered over the
    /// rest, and Worktrees is not open at all.
    fn messy_window() -> Window {
        Window::new(
            stack(
                0.7,
                side(
                    0.2,
                    Layout::Pane(1),
                    side(
                        0.5,
                        Layout::Pane(2),
                        stack(0.5, Layout::Pane(3), Layout::Pane(4)),
                    ),
                ),
                side(0.5, Layout::Pane(5), Layout::Pane(6)),
            ),
            vec![
                (
                    1,
                    pane(
                        vec![
                            panel(1, PanelKind::Files),
                            shell(2),
                            panel(3, PanelKind::Preview),
                        ],
                        1,
                    ),
                ),
                (
                    2,
                    pane(vec![shell(4), panel(5, PanelKind::Tasks), shell(6)], 2),
                ),
                (
                    3,
                    pane(
                        vec![panel(7, PanelKind::Usage), panel(8, PanelKind::Projects)],
                        0,
                    ),
                ),
                (4, pane(vec![shell(9)], 0)),
                (
                    5,
                    pane(
                        vec![
                            panel(10, PanelKind::Settings),
                            shell(11),
                            panel(12, PanelKind::Shells),
                            shell(13),
                        ],
                        3,
                    ),
                ),
                (6, pane(vec![panel(14, PanelKind::Schedules)], 0)),
            ],
            Some(2),
            &[1],
        )
    }

    #[test]
    fn a_messy_window_becomes_a_locked_navigation_pane_beside_the_main_pane() {
        let mut window = messy_window();
        // The shell selected in pane 5, which is the selected pane.
        let applied = window
            .default_layout(5)
            .expect("the window is not laid out yet");

        // The tree: the navigation pane at its usual width, left of one pane, and nothing else.
        assert_eq!(window.layout, Layout::navigation_beside(1, Layout::Pane(2)));
        assert_eq!(window.panes.keys().copied().collect::<Vec<_>>(), [1, 2]);
        assert_eq!(
            applied,
            DefaultLayout {
                navigation: 1,
                main: 2,
                active_pane: 2,
                moved: 11,
                opened: vec![PanelKind::Worktrees],
            }
        );

        // Left: exactly the five panels in the order a new project gets, the open ones as they
        // were (tab ids 8, 1, 5 and 12) and Worktrees, which was not open, as a new tab (15).
        assert_eq!(
            window.panels(1),
            NAVIGATION_PANELS.map(Some).to_vec(),
            "the left pane holds the navigation panels and nothing else"
        );
        assert_eq!(window.tab_ids(1), [8, 1, 15, 5, 12]);
        // Right: every other tab, pane by pane in layout order and each pane's own order. The
        // locked pane's shell and the Preview come first, being in the first pane.
        assert_eq!(window.tab_ids(2), [2, 3, 4, 6, 7, 9, 10, 11, 13, 14]);
        // The shell that was selected in the selected pane still is, and its pane is selected.
        assert_eq!(window.shown(2), 13);
        // The navigation pane shows its first panel: none was selected in any pane.
        assert_eq!(window.shown(1), 8);

        // The caller's side: the left pane is locked, the right one is not and is the main pane.
        assert_eq!(window.locked, [1]);
        assert_eq!(window.main, Some(2));
        assert!(window.is_locked(1) && !window.is_locked(2));
        assert!(is_default_layout(
            &window.layout,
            &window.panes,
            window.main,
            &|id| window.is_locked(id)
        ));
    }

    #[test]
    fn the_default_layout_closes_nothing_and_a_locked_panes_tabs_are_moved_not_dropped() {
        let mut window = messy_window();
        let before = window.all_tab_ids();
        let sessions: BTreeSet<String> = window
            .panes
            .values()
            .flat_map(|pane| pane.tabs.iter())
            .filter_map(|tab| tab.shell_id().map(str::to_owned))
            .collect();
        let locked_tabs = window.tab_ids(1);
        window.default_layout(5).unwrap();

        // Every tab there was is still there, once. The only new one is the panel it opened.
        let after = window.all_tab_ids();
        assert!(after.is_superset(&before));
        assert_eq!(after.len(), before.len() + 1);
        assert_eq!(
            window
                .panes
                .values()
                .map(|pane| pane.tabs.len())
                .sum::<usize>(),
            after.len()
        );
        // Every shell session is in a tab of the window.
        let kept: BTreeSet<String> = window
            .panes
            .values()
            .flat_map(|pane| pane.tabs.iter())
            .filter_map(|tab| tab.shell_id().map(str::to_owned))
            .collect();
        assert_eq!(kept, sessions);
        // The locked pane's tabs went to the pane they belong to by kind: Files to the left,
        // its shell and the Preview to the right.
        for tab in locked_tabs {
            assert!(
                window.all_tab_ids().contains(&tab),
                "tab {tab} of the locked pane was lost"
            );
        }
        assert!(window.tab_ids(1).contains(&1));
        assert!(window.tab_ids(2).contains(&2) && window.tab_ids(2).contains(&3));
    }

    #[test]
    fn a_window_that_is_laid_out_already_is_left_exactly_as_it_is() {
        let mut window = messy_window();
        window.default_layout(5).unwrap();
        // The user then drags the navigation pane wider and selects another panel and shell.
        assert!(window.layout.set_ratio(&[], 0.4));
        window.panes.get_mut(&1).unwrap().active = 3;
        window.panes.get_mut(&2).unwrap().active = 0;
        let picture = window.picture();

        assert_eq!(window.default_layout(1), None);
        assert_eq!(window.default_layout(2), None);
        assert_eq!(window.picture(), picture);
        assert_eq!(window.layout.ratio_at(&[]), Some(0.4));
        assert_eq!(window.next_pane_id, 7);
        assert_eq!(window.next_tab_id, 16);
    }

    #[test]
    fn a_window_that_nearly_matches_is_finished_without_moving_a_pane_or_a_tab() {
        // The panes and the tabs in each, whatever their order.
        let contents = |window: &Window| {
            let sorted = |pane| window.all_tab_ids_of(pane);
            (window.layout.pane_ids(), sorted(1), sorted(2))
        };
        let finished = |window: &mut Window, active: PaneId| {
            let ids = contents(window);
            let applied = window.default_layout(active).expect("not complete yet");
            assert_eq!(applied.moved, 0);
            assert!(applied.opened.is_empty());
            assert_eq!(contents(window), ids);
            assert_eq!(window.locked, [1]);
            assert_eq!(window.main, Some(2));
            assert!(is_default_layout(
                &window.layout,
                &window.panes,
                window.main,
                &|id| window.is_locked(id)
            ));
        };
        let complete = || {
            let mut window = messy_window();
            window.default_layout(5).unwrap();
            window
        };

        // The navigation pane was unlocked, or never locked.
        let mut window = complete();
        window.locked.clear();
        finished(&mut window, 2);
        // No main pane was chosen.
        let mut window = complete();
        window.main = None;
        finished(&mut window, 2);
        // The main pane was locked too, which a person may choose.
        let mut window = complete();
        window.locked = vec![1, 2];
        finished(&mut window, 2);
        // The panels are there but not in the order a new project has them.
        let mut window = complete();
        window.panes.get_mut(&1).unwrap().tabs.reverse();
        finished(&mut window, 2);
        assert_eq!(window.panels(1), NAVIGATION_PANELS.map(Some).to_vec());
    }

    #[test]
    fn what_the_default_layout_needs_to_have_to_count_as_in_place() {
        let in_place = |window: &Window| {
            is_default_layout(&window.layout, &window.panes, window.main, &|id| {
                window.is_locked(id)
            })
        };
        let mut window = messy_window();
        window.default_layout(2).unwrap();
        assert!(in_place(&window));

        // A third pane, a navigation pane with another tab in it, a navigation pane without one
        // of its panels, a stacked pair, a main pane on the left: each is something else.
        let mut third = Window::new(
            side(
                0.27,
                Layout::Pane(1),
                side(0.5, Layout::Pane(2), Layout::Pane(3)),
            ),
            vec![
                (1, pane(navigation_tabs(), 0)),
                (2, pane(vec![shell(20)], 0)),
                (3, pane(vec![], 0)),
            ],
            Some(2),
            &[1],
        );
        assert!(!in_place(&third));
        third.panes.remove(&3);
        third.layout = Layout::navigation_beside(1, Layout::Pane(2));
        assert!(in_place(&third));

        let mut extra = Window::new(
            Layout::navigation_beside(1, Layout::Pane(2)),
            vec![
                (1, pane(vec![panel(1, PanelKind::Preview)], 0)),
                (2, pane(vec![shell(2)], 0)),
            ],
            Some(2),
            &[1],
        );
        assert!(!in_place(&extra));
        extra.panes.insert(1, pane(navigation_tabs(), 0));
        assert!(in_place(&extra));
        extra.panes.get_mut(&1).unwrap().tabs.pop();
        assert!(!in_place(&extra));
        extra
            .panes
            .get_mut(&1)
            .unwrap()
            .tabs
            .push(panel(99, PanelKind::Shells));
        assert!(in_place(&extra));
        extra.layout = stack(0.27, Layout::Pane(1), Layout::Pane(2));
        assert!(!in_place(&extra));
        extra.layout = side(0.27, Layout::Pane(2), Layout::Pane(1));
        assert!(!in_place(&extra));
    }

    #[test]
    fn the_selected_tab_stays_selected_in_whichever_pane_it_lands() {
        // nav 1 (locked): Projects, Files, Shells, with Files selected | main 2: two shells, the
        // second selected | pane 3: a shell.
        let window = || {
            Window::new(
                side(
                    0.27,
                    Layout::Pane(1),
                    side(0.6, Layout::Pane(2), Layout::Pane(3)),
                ),
                vec![
                    (
                        1,
                        pane(
                            vec![
                                panel(1, PanelKind::Projects),
                                panel(2, PanelKind::Files),
                                panel(3, PanelKind::Shells),
                            ],
                            1,
                        ),
                    ),
                    (2, pane(vec![shell(4), shell(5)], 1)),
                    (3, pane(vec![shell(6), shell(7)], 0)),
                ],
                Some(2),
                &[1],
            )
        };

        // Working in the main pane: its selected shell stays selected, and so does the Files
        // panel the navigation pane showed.
        let mut main_pane = window();
        let applied = main_pane.default_layout(2).unwrap();
        assert_eq!(
            (applied.navigation, applied.main, applied.active_pane),
            (1, 2, 2)
        );
        assert_eq!(main_pane.tab_ids(2), [4, 5, 6, 7]);
        assert_eq!(main_pane.shown(2), 5);
        assert_eq!(main_pane.shown(1), 2);

        // Working in another pane: its shell is the one on screen, and its pane the selected one.
        let mut other = window();
        let applied = other.default_layout(3).unwrap();
        assert_eq!(applied.active_pane, 2);
        assert_eq!(other.shown(2), 6);

        // Working in the Files panel: the navigation pane is the selected one and shows Files,
        // while the main pane keeps what it showed.
        let mut files = window();
        let applied = files.default_layout(1).unwrap();
        assert_eq!(applied.active_pane, 1);
        assert_eq!(files.shown(1), 2);
        assert_eq!(files.shown(2), 5);

        // Working in a panel that is not a navigation panel: it goes right and stays selected.
        let mut usage = window();
        usage
            .panes
            .get_mut(&3)
            .unwrap()
            .tabs
            .insert(0, panel(8, PanelKind::Usage));
        usage.panes.get_mut(&3).unwrap().active = 0;
        let applied = usage.default_layout(3).unwrap();
        assert_eq!(applied.active_pane, 2);
        assert_eq!(usage.shown(2), 8);

        // A selected pane with nothing in it: the main pane keeps what it showed.
        let mut empty = window();
        empty.panes.insert(3, pane(vec![], 0));
        let applied = empty.default_layout(3).unwrap();
        assert_eq!(applied.active_pane, 2);
        assert_eq!(empty.shown(2), 5);
    }

    #[test]
    fn panes_keep_their_ids_and_a_new_one_is_made_only_when_there_is_none_to_keep() {
        // The main pane, alone, holding Files and a shell: it stays the main pane and keeps its
        // id; the navigation pane is the new one.
        let mut one = Window::new(
            Layout::Pane(1),
            vec![(1, pane(vec![panel(1, PanelKind::Files), shell(2)], 1))],
            Some(1),
            &[],
        );
        let applied = one.default_layout(1).unwrap();
        assert_eq!((applied.navigation, applied.main), (2, 1));
        assert_eq!(one.next_pane_id, 3);
        assert_eq!(one.layout, Layout::navigation_beside(2, Layout::Pane(1)));
        assert_eq!(one.tab_ids(1), [2]);
        assert_eq!(one.tab_ids(2), [3, 1, 4, 5, 6]);
        // Its shell was selected, in the pane that stays the selected one.
        assert_eq!(applied.active_pane, 1);

        // Without a main pane, the pane that holds a navigation panel is the left one, and the
        // right is new.
        let mut unmarked = Window::new(
            Layout::Pane(1),
            vec![(1, pane(vec![panel(1, PanelKind::Files), shell(2)], 1))],
            None,
            &[],
        );
        let applied = unmarked.default_layout(1).unwrap();
        assert_eq!((applied.navigation, applied.main), (1, 2));
        assert_eq!(
            unmarked.layout,
            Layout::navigation_beside(1, Layout::Pane(2))
        );

        // A main pane on the left that holds the tree is not mistaken for the navigation pane.
        let mut tree_in_main = Window::new(
            side(0.3, Layout::Pane(1), Layout::Pane(2)),
            vec![
                (1, pane(vec![shell(1), panel(2, PanelKind::Files)], 0)),
                (2, pane(vec![panel(3, PanelKind::Projects), shell(4)], 0)),
            ],
            Some(1),
            &[2],
        );
        let applied = tree_in_main.default_layout(1).unwrap();
        assert_eq!((applied.navigation, applied.main), (2, 1));
        assert_eq!(tree_in_main.tab_ids(1), [1, 4]);
        assert_eq!(tree_in_main.layout.pane_ids(), [2, 1]);

        // A pane of shells alone keeps the right, and the navigation is new.
        let mut shells = Window::new(
            Layout::Pane(1),
            vec![(1, pane(vec![shell(1), shell(2)], 1))],
            None,
            &[],
        );
        let applied = shells.default_layout(1).unwrap();
        assert_eq!((applied.navigation, applied.main), (2, 1));
        assert_eq!(shells.tab_ids(1), [1, 2]);
        assert_eq!(shells.shown(1), 2);
        assert_eq!(applied.opened, NAVIGATION_PANELS.to_vec());
        assert_eq!(shells.layout, Layout::navigation_beside(2, Layout::Pane(1)));

        // The main pane that was somewhere else on the right is the one that stays.
        let mut spread = Window::new(
            side(
                0.3,
                Layout::Pane(1),
                side(0.5, Layout::Pane(2), Layout::Pane(3)),
            ),
            vec![
                (1, pane(vec![shell(1)], 0)),
                (2, pane(vec![shell(2)], 0)),
                (3, pane(vec![panel(3, PanelKind::Tasks), shell(4)], 0)),
            ],
            Some(2),
            &[],
        );
        let applied = spread.default_layout(2).unwrap();
        assert_eq!((applied.navigation, applied.main), (3, 2));
        assert_eq!(spread.tab_ids(2), [1, 2, 4]);
        assert_eq!(spread.layout.pane_ids(), [3, 2]);
    }

    #[test]
    fn a_window_with_only_navigation_panels_leaves_an_empty_main_pane_and_a_second_copy_is_kept() {
        let mut window = Window::new(
            side(0.3, Layout::Pane(1), Layout::Pane(2)),
            vec![
                (
                    1,
                    pane(
                        vec![
                            panel(1, PanelKind::Shells),
                            panel(2, PanelKind::Projects),
                            panel(3, PanelKind::Files),
                        ],
                        0,
                    ),
                ),
                (2, pane(vec![panel(4, PanelKind::Files)], 0)),
            ],
            None,
            &[],
        );
        let applied = window.default_layout(1).unwrap();
        assert_eq!(applied.opened, [PanelKind::Worktrees, PanelKind::Tasks]);
        assert_eq!(window.panels(1), NAVIGATION_PANELS.map(Some).to_vec());
        // The first Files tab is the panel; the second is an ordinary tab, kept on the right.
        assert_eq!(window.tab_ids(1), [2, 3, 5, 6, 1]);
        assert_eq!(window.tab_ids(2), [4]);
        // It was on the right already; only the panels that were on the left stayed there.
        assert_eq!(applied.moved, 0);

        // With no tab to put there, the main pane is an empty pane, where new tabs still open.
        let mut only = Window::new(
            Layout::Pane(1),
            vec![(1, pane(navigation_tabs(), 2))],
            None,
            &[],
        );
        let applied = only.default_layout(1).unwrap();
        assert_eq!((applied.navigation, applied.main), (1, 2));
        assert!(only.panes[&2].tabs.is_empty());
        assert_eq!(only.panes[&2].active, 0);
        assert_eq!(applied.active_pane, 1);
        // The third panel, Worktrees (tab 2), was the selected one and still is.
        assert_eq!(only.shown(1), 2);
    }

    #[test]
    fn the_default_layout_is_saved_as_the_projects_layout_and_reads_back_as_it_was() {
        let mut window = messy_window();
        let applied = window.default_layout(5).unwrap();
        let saved = ProjectLayout {
            layout: window.layout.clone(),
            panes: saved_panes(&window.panes),
            active_pane: applied.active_pane,
            locked_panes: Some(window.locked.iter().copied().collect()),
            panels_initialized: true,
            detached_shell_ids: HashSet::new(),
            selected_worktree_id: None,
            selected_task_id: None,
            sidebar_visible: true,
            window_size: None,
            main_pane: window.main,
        };
        let directory =
            std::env::temp_dir().join(format!("riwork-default-layout-{}", uuid::Uuid::new_v4()));
        let store = LayoutStore::open(&directory).unwrap();
        store.save("project-a", &saved).unwrap();
        let loaded = LayoutStore::open(&directory)
            .unwrap()
            .load("project-a")
            .unwrap()
            .expect("the layout was saved");

        // The tree, the tabs in order with each pane's selection, the lock and the main pane.
        assert_eq!(loaded.layout, window.layout);
        assert_eq!(loaded.active_pane, 2);
        assert_eq!(loaded.main_pane, Some(2));
        assert_eq!(loaded.locked_panes, Some(HashSet::from([1])));
        assert_eq!(loaded.effective_locked_panes(), HashSet::from([1]));
        assert_eq!(loaded.panes[&1].tabs, saved.panes[&1].tabs);
        assert_eq!(loaded.panes[&2].tabs, saved.panes[&2].tabs);
        assert_eq!(
            loaded.panes[&1].active_tab_key.as_deref(),
            Some("panel:projects")
        );
        assert_eq!(
            loaded.panes[&2].active_tab_key.as_deref(),
            Some("shell:shell-13")
        );
        assert_eq!(
            loaded.panes[&1].tabs,
            NAVIGATION_PANELS.map(|panel| SavedTab::Panel { panel })
        );
        // Every shell of the window is in the saved layout.
        let saved_shells: BTreeSet<_> = loaded
            .panes
            .values()
            .flat_map(|pane| pane.shell_ids.iter().cloned())
            .collect();
        assert_eq!(
            saved_shells,
            [
                "shell-11", "shell-13", "shell-2", "shell-4", "shell-6", "shell-9"
            ]
            .map(str::to_owned)
            .into()
        );

        // Saving the layout again changes nothing, which is what a second apply amounts to.
        store.save("project-a", &loaded).unwrap();
        assert_eq!(store.load("project-a").unwrap(), Some(loaded));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn the_layout_menu_says_which_pane_is_main_and_stays_inside_the_window() {
        let pane_of = |titles: &[&str], active: usize| {
            pane(
                titles
                    .iter()
                    .enumerate()
                    .map(|(index, title)| {
                        let mut tab = shell(index as TabId + 1);
                        tab.title = (*title).to_owned();
                        tab
                    })
                    .collect(),
                active,
            )
        };
        assert_eq!(main_pane_summary(None), "none");
        assert_eq!(main_pane_summary(Some(&pane(vec![], 0))), "empty");
        assert_eq!(main_pane_summary(Some(&pane_of(&["zsh"], 0))), "zsh");
        assert_eq!(
            main_pane_summary(Some(&pane_of(&["zsh", "codex", "claude"], 1))),
            "codex · 3 tabs"
        );

        // The menu ends where the item does, and never leaves the window.
        assert_eq!(layout_menu_left(1500.0, 70.0, 296.0, 1600.0), 1204.0);
        assert_eq!(layout_menu_left(1600.0, 70.0, 296.0, 1600.0), 1298.0);
        assert_eq!(layout_menu_left(100.0, 70.0, 296.0, 1600.0), 6.0);
        // An item not laid out yet puts the menu at the right edge.
        assert_eq!(layout_menu_left(0.0, 0.0, 296.0, 1600.0), 1298.0);
        // A window narrower than the menu keeps its left edge.
        assert_eq!(layout_menu_left(300.0, 70.0, 296.0, 300.0), 6.0);
    }

    #[test]
    fn the_default_layout_says_what_it_did() {
        let applied = |moved, opened: &[PanelKind]| DefaultLayout {
            navigation: 1,
            main: 2,
            active_pane: 2,
            moved,
            opened: opened.to_vec(),
        };
        assert_eq!(
            default_layout_notice(&applied(11, &[PanelKind::Worktrees]), 0),
            "Applied the default layout: moved 11 tabs, opened Worktrees. Nothing was closed"
        );
        assert_eq!(
            default_layout_notice(&applied(1, &[]), 0),
            "Applied the default layout: moved 1 tab. Nothing was closed"
        );
        assert_eq!(
            default_layout_notice(&applied(0, &[PanelKind::Tasks, PanelKind::Shells]), 0),
            "Applied the default layout: opened Tasks, Shells. Nothing was closed"
        );
        assert_eq!(
            default_layout_notice(&applied(0, &[]), 0),
            "Applied the default layout. Nothing was closed"
        );
        // A shell of another project only stays in a window with a locked pane that carries it.
        assert!(default_layout_notice(&applied(3, &[]), 1).ends_with(
            ". A shell of another project left the lock and is not part of this project's layout"
        ));
        assert!(
            default_layout_notice(&applied(3, &[]), 2)
                .contains(". 2 shells of other projects left the lock")
        );
        assert_eq!(
            DEFAULT_LAYOUT_PRESENT_HINT,
            "The default layout is already in place"
        );
    }
}
