//! Controlled DockArea projection. The core's binary tree remains authoritative;
//! reported DockArea trees are gestures to replay, never replacement persistence.

use std::collections::{BTreeMap, BTreeSet, HashSet};

pub use riwork_core::layouts::Extent;
use riwork_core::layouts::workspace;
use riwork_core::layouts::{
    Axis, DIVIDER_THICKNESS, Layout, MIN_PANE_EXTENT, PaneFacts, PaneId, PanelKind,
    PreviewPlacement, PreviewReveal, PreviewTab, ProjectLayout, SavedPane, SavedTab,
};
use serde::{Deserialize, Serialize};

pub const MAX_SAFE_PANE_ID: PaneId = (1_u64 << 53) - 1;

/// Loading a JS-safe ID near MAX_SAFE would still poison NSH's process-wide
/// allocator: it adopts that ID, then allocates split containers above it.
/// Keep initial group IDs below this defensive ceiling, leaving ample room for
/// native split/group allocations. These additional aliases are session-only;
/// JS-safe on-disk IDs retain their identity when the project is saved.
pub const MAX_INITIAL_DOCK_PANE_ID: PaneId = u32::MAX as PaneId;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DockAxis {
    Horizontal,
    Vertical,
}

impl From<Axis> for DockAxis {
    fn from(axis: Axis) -> Self {
        match axis {
            Axis::SideBySide => Self::Horizontal,
            Axis::Stacked => Self::Vertical,
        }
    }
}

impl From<DockAxis> for Axis {
    fn from(axis: DockAxis) -> Self {
        match axis {
            DockAxis::Horizontal => Self::SideBySide,
            DockAxis::Vertical => Self::Stacked,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum DockNode {
    Split {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<PaneId>,
        axis: DockAxis,
        children: Vec<DockNode>,
        #[serde(default)]
        sizes: Vec<Option<f32>>,
    },
    Tabs {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<PaneId>,
        panels: Vec<String>,
        #[serde(default, rename = "activeIndex")]
        active_index: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DockRegion {
    pub layout: DockNode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collapsible: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DockAreaLayout {
    pub center: DockNode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left: Option<DockRegion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right: Option<DockRegion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bottom: Option<DockRegion>,
}

impl DockAreaLayout {
    pub fn new(center: DockNode) -> Self {
        Self {
            center,
            left: None,
            right: None,
            bottom: None,
        }
    }
}

/// Pure per-window state, with no IO, workers, timers or session side effects.
pub struct LayoutModel {
    project: ProjectLayout,
    extent: Option<Extent>,
    last: Option<DockAreaLayout>,
    pane_id_remap: BTreeMap<PaneId, PaneId>,
    allocated_pane_ids: BTreeSet<PaneId>,
    unknown_panes: BTreeSet<PaneId>,
    /// Original stored opaque-tab pane -> current in-memory pane. Unlike known
    /// tabs, their payload stays private to LayoutStore throughout a move.
    opaque_pane_remap: BTreeMap<PaneId, PaneId>,
    diagnostic: Option<String>,
    detached_shell_panes: BTreeMap<String, PaneId>,
    focus_mode: bool,
    centered_focus: bool,
    // The capped/margined focused surface is presentation, not a resize of
    // the saved split area. Restore this baseline before leaving focus.
    unfocused_extent: Option<Extent>,
    skip_next_preserve: bool,
    notice: Option<String>,
    /// Persisted key -> component identity. A pending chat keeps its first id.
    panel_ids: BTreeMap<String, String>,
    pending_chats: BTreeSet<String>,
}

impl LayoutModel {
    pub fn new(mut project: ProjectLayout) -> Result<Self, String> {
        project.normalize()?;
        let occupied: BTreeSet<_> = project.layout.pane_ids().into_iter().collect();
        let mut next = 1;
        let mut remap = BTreeMap::new();
        for &id in &occupied {
            if id > MAX_INITIAL_DOCK_PANE_ID {
                while occupied.contains(&next) || remap.values().any(|&id| id == next) {
                    next += 1;
                }
                remap.insert(id, next);
                next += 1;
            }
        }
        remap_project(&mut project, &remap);
        let allocated_pane_ids = project.layout.pane_ids().into_iter().collect();
        Ok(Self {
            project,
            extent: None,
            last: None,
            pane_id_remap: remap,
            allocated_pane_ids,
            unknown_panes: BTreeSet::new(),
            opaque_pane_remap: BTreeMap::new(),
            diagnostic: None,
            detached_shell_panes: BTreeMap::new(),
            focus_mode: false,
            centered_focus: false,
            unfocused_extent: None,
            skip_next_preserve: false,
            notice: None,
            panel_ids: BTreeMap::new(),
            pending_chats: BTreeSet::new(),
        })
    }

    pub fn project(&self) -> &ProjectLayout {
        &self.project
    }

    pub fn extent(&self) -> Option<Extent> {
        self.extent
    }

    /// Reuse an existing panel in place. A new panel follows the GPUI routing
    /// rule (main, else the active pane); Files joins the navigation pane.
    /// Dashboard is a global-window route and cannot become a project tab.
    pub fn open_panel(&mut self, panel: PanelKind) -> Result<DockAreaLayout, String> {
        if panel == PanelKind::Dashboard {
            return Err("refused: Dashboard belongs to the global window".into());
        }
        let navigation = (panel == PanelKind::Files)
            .then(|| {
                self.project.panes.iter().find_map(|(id, pane)| {
                    pane.tabs
                        .iter()
                        .any(|tab| {
                            matches!(
                                tab,
                                SavedTab::Panel {
                                    panel: PanelKind::Projects
                                        | PanelKind::Worktrees
                                        | PanelKind::Files
                                }
                            )
                        })
                        .then_some(*id)
                })
            })
            .flatten();
        match navigation {
            Some(target) => self.open_tab_in(SavedTab::Panel { panel }, target),
            None => self.open_tab(SavedTab::Panel { panel }),
        }
    }

    /// File selection and terminal links reveal panels without moving keyboard
    /// focus. Core owns the main-pane, locked-pane and terminal visibility rules.
    pub fn reveal_panel(
        &mut self,
        panel: PanelKind,
        asked: bool,
        enabled: bool,
    ) -> Result<DockAreaLayout, String> {
        if !matches!(panel, PanelKind::Files | PanelKind::Preview) {
            return Err("invalid: only Files and Preview support reveal".into());
        }
        if self.focus_mode || (!asked && !enabled) {
            return Ok(self.dock(self.extent.unwrap_or(DEFAULT_EXTENT)));
        }
        let key = SavedTab::Panel { panel }.key();
        let shows_shell = |id| {
            self.project.panes.get(&id).is_some_and(|pane| {
                pane.tabs.iter().any(|tab| {
                    Some(tab.key()) == pane.active_tab_key
                        && matches!(tab, SavedTab::Shell { .. } | SavedTab::RemoteShell { .. })
                })
            })
        };
        let existing = self.project.panes.iter().find_map(|(&id, pane)| {
            pane.tabs
                .iter()
                .any(|tab| tab.key() == key)
                .then_some(PreviewTab {
                    pane: id,
                    shown: pane.active_tab_key.as_deref() == Some(key.as_str()),
                    behind_shell: shows_shell(id),
                })
        });
        let clicked = self.project.active_pane;
        let explorer = self
            .project
            .panes
            .iter()
            .find_map(|(&id, pane)| {
                (pane.active_tab_key.as_deref() == Some("panel:files")).then_some(id)
            })
            .filter(|id| !asked || *id != clicked);
        let locks = self.project.effective_locked_panes();
        let locked = |id| locks.contains(&id);
        let facts = PaneFacts {
            locked: &locked,
            shows_shell: &shows_shell,
        };
        let reveal = if let Some(main) = self.project.main_pane {
            let keep: Vec<_> = asked
                .then_some(clicked)
                .into_iter()
                .chain((panel == PanelKind::Preview).then_some(explorer).flatten())
                .collect();
            riwork_core::layouts::plan_main_reveal(main, existing, &keep)
        } else if panel == PanelKind::Preview && explorer.is_some() {
            self.project.layout.plan_preview_reveal(
                true,
                existing,
                self.extent,
                explorer.unwrap(),
                &facts,
            )
        } else if asked {
            self.project
                .layout
                .plan_beside_reveal(existing, self.extent, clicked, &facts)
        } else {
            PreviewReveal::Leave
        };
        match reveal {
            PreviewReveal::Leave => {}
            PreviewReveal::Activate(id) => {
                self.project
                    .panes
                    .get_mut(&id)
                    .ok_or("not_found: pane")?
                    .active_tab_key = Some(key.clone())
            }
            PreviewReveal::Move { from, to, activate } => {
                let source = self.project.panes.get_mut(&from).ok_or("not_found: pane")?;
                source.tabs.retain(|tab| tab.key() != key);
                if source.active_tab_key.as_deref() == Some(key.as_str()) {
                    source.active_tab_key = source.tabs.first().map(SavedTab::key);
                }
                let target = self.project.panes.get_mut(&to).ok_or("not_found: pane")?;
                target.tabs.push(SavedTab::Panel { panel });
                if activate {
                    target.active_tab_key = Some(key.clone());
                }
            }
            PreviewReveal::Open(placement) => {
                let (id, activate) = match placement {
                    PreviewPlacement::Tab { pane, activate } => (pane, activate),
                    PreviewPlacement::Split {
                        target,
                        axis,
                        ratio,
                    } => {
                        let id = (1..=MAX_INITIAL_DOCK_PANE_ID)
                            .find(|id| !self.allocated_pane_ids.contains(id))
                            .ok_or("unavailable: no pane identifiers remain")?;
                        if !self
                            .project
                            .layout
                            .split_with_ratio(target, axis, id, false, ratio)
                        {
                            return Err("not_found: split target".into());
                        }
                        self.allocated_pane_ids.insert(id);
                        self.project.panes.insert(id, SavedPane::default());
                        (id, true)
                    }
                };
                let target = self.project.panes.get_mut(&id).ok_or("not_found: pane")?;
                target.tabs.push(SavedTab::Panel { panel });
                if activate {
                    target.active_tab_key = Some(key);
                }
            }
        }
        self.project.normalize()?;
        Ok(self.dock(self.extent.unwrap_or(DEFAULT_EXTENT)))
    }

    /// Keep the panel API on the terminal placement path, including remembered
    /// detached panes and refusal to add tabs when every pane is locked.
    pub fn show_shell(&mut self, shell_id: &str) -> Result<DockAreaLayout, String> {
        if shell_id.is_empty() || shell_id.chars().any(char::is_control) || shell_id.contains(':') {
            return Err("invalid: shell ID is empty or contains a reserved character".into());
        }
        self.open_shell(shell_id)
    }

    fn open_tab_in(&mut self, tab: SavedTab, target: PaneId) -> Result<DockAreaLayout, String> {
        let key = tab.key();
        if self
            .project
            .panes
            .values()
            .any(|pane| pane.tabs.iter().any(|tab| tab.key() == key))
        {
            return self.select_panel(&key);
        }
        let pane = self
            .project
            .panes
            .get_mut(&target)
            .ok_or("not_found: pane")?;
        pane.tabs.push(tab);
        pane.active_tab_key = Some(key);
        self.project.active_pane = target;
        self.project.normalize()?;
        Ok(self.dock(self.extent.unwrap_or(DEFAULT_EXTENT)))
    }

    pub fn set_selected_worktree(&mut self, id: Option<String>) {
        self.project.selected_worktree_id = id;
    }

    pub fn set_selected_task(&mut self, id: Option<String>) {
        self.project.selected_task_id = id;
    }

    pub fn pane_id_remap(&self) -> &BTreeMap<PaneId, PaneId> {
        &self.pane_id_remap
    }

    /// Reverse session-only headroom aliases for already JS-safe disk IDs.
    /// Genuinely unsafe IDs persist their safe remap. LayoutStore receives the
    /// same mapping for its opaque tabs through `storage_id_remap`.
    pub fn storage_project(&self) -> ProjectLayout {
        let mut project = self.project.clone();
        let reverse = self
            .pane_id_remap
            .iter()
            .filter(|(old, _)| **old <= MAX_SAFE_PANE_ID)
            .map(|(&old, &new)| (new, old))
            .collect();
        remap_project(&mut project, &reverse);
        for pane in project.panes.values_mut() {
            pane.tabs
                .retain(|tab| !self.pending_chats.contains(&tab.key()));
            pane.shell_ids.clear();
            pane.active_shell_id = None;
        }
        // Pending chats have no durable session and cannot become saved tabs.
        project
            .normalize()
            .expect("removing pending chats preserves valid geometry");
        project
    }

    pub fn storage_id_remap(&self) -> BTreeMap<PaneId, PaneId> {
        let mut mapping: BTreeMap<_, _> = self
            .pane_id_remap
            .iter()
            .filter(|(old, _)| **old > MAX_SAFE_PANE_ID)
            .map(|(&old, &new)| (old, new))
            .collect();
        for (&original, &current) in &self.opaque_pane_remap {
            let stored = self
                .pane_id_remap
                .iter()
                .find_map(|(&old, &alias)| {
                    (old <= MAX_SAFE_PANE_ID && alias == current).then_some(old)
                })
                .unwrap_or(current);
            if original != stored {
                mapping.insert(original, stored);
            }
        }
        mapping
    }

    pub fn focus_mode(&self) -> bool {
        self.focus_mode
    }
    pub fn centered_focus(&self) -> bool {
        self.centered_focus
    }
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }
    pub fn panel_id(&self, tab: &SavedTab) -> String {
        self.panel_ids
            .get(&tab.key())
            .cloned()
            .unwrap_or_else(|| tab.key())
    }
    pub fn panel(&self, id: &str) -> Option<&SavedTab> {
        self.project
            .panes
            .values()
            .flat_map(|pane| &pane.tabs)
            .find(|tab| self.panel_id(tab) == id || tab.key() == id)
    }
    fn tab_key(&self, id: &str) -> String {
        self.panel_ids
            .iter()
            .find(|(_, alias)| alias.as_str() == id)
            .map(|(key, _)| key.clone())
            .unwrap_or_else(|| id.to_owned())
    }

    /// Project switch retains component ids for carried tabs while replacing the
    /// core layout atomically. No projection is published on invalid input.
    pub fn replace_project(&mut self, project: ProjectLayout) -> Result<DockAreaLayout, String> {
        let replacement = Self::new(project)?;
        self.project = replacement.project;
        self.pane_id_remap = replacement.pane_id_remap;
        self.allocated_pane_ids
            .extend(replacement.allocated_pane_ids);
        self.unknown_panes.clear();
        self.opaque_pane_remap.clear();
        self.last = None;
        self.restore_focus(false, false);
        Ok(self.dock(self.extent.unwrap_or(DEFAULT_EXTENT)))
    }

    /// Reconcile data for the current project, whose pane ids are already the
    /// model's projected ids. Preserve focus, opaque-tab mappings and aliases.
    pub fn update_same_project(
        &mut self,
        mut project: ProjectLayout,
    ) -> Result<DockAreaLayout, String> {
        project.normalize()?;
        if project
            .layout
            .pane_ids()
            .iter()
            .any(|id| *id > MAX_INITIAL_DOCK_PANE_ID)
        {
            return Err("invalid: same-project update requires projected pane ids".into());
        }
        self.allocated_pane_ids.extend(project.layout.pane_ids());
        self.project = project;
        Ok(self.dock(self.extent.unwrap_or(DEFAULT_EXTENT)))
    }

    pub fn toggle_focus(&mut self, centered: bool) -> Result<DockAreaLayout, String> {
        self.restore_focus(!self.focus_mode, centered);
        Ok(self.dock(self.extent.unwrap_or(DEFAULT_EXTENT)))
    }

    pub fn restore_focus(&mut self, focused: bool, centered: bool) {
        if focused && !self.focus_mode {
            self.unfocused_extent = self.unfocused_extent.or(self.extent);
        } else if !focused && self.focus_mode {
            self.extent = self.unfocused_extent;
            // A window restored directly into focus may never have measured
            // its normal split area. Its first normal measurement establishes
            // a baseline rather than resizing from the focused presentation.
            self.skip_next_preserve = self.extent.is_none();
        }
        self.focus_mode = focused;
        self.centered_focus = focused && centered;
    }

    pub fn split_shell(&mut self, tab: SavedTab, axis: Axis) -> Result<DockAreaLayout, String> {
        if !matches!(&tab, SavedTab::Shell { shell_id } if !shell_id.is_empty()) {
            return Err("invalid: split requires a created shell".into());
        }
        if self.panel(&tab.key()).is_some() {
            return Err("invalid: split shell is already open".into());
        }
        let id = (1..=MAX_INITIAL_DOCK_PANE_ID)
            .find(|id| !self.allocated_pane_ids.contains(id))
            .ok_or("invalid: no unused pane id")?;
        let mut candidate = self.project.clone();
        if !candidate.layout.split(candidate.active_pane, axis, id) {
            return Err("not_found: active pane".into());
        }
        candidate.panes.insert(
            id,
            SavedPane {
                active_tab_key: Some(tab.key()),
                tabs: vec![tab],
                ..SavedPane::default()
            },
        );
        candidate.active_pane = id;
        candidate.normalize()?;
        self.project = candidate;
        self.allocated_pane_ids.insert(id);
        self.restore_focus(false, false);
        Ok(self.dock(self.extent.unwrap_or(DEFAULT_EXTENT)))
    }

    pub fn open_tab(&mut self, tab: SavedTab) -> Result<DockAreaLayout, String> {
        let key = tab.key();
        if let Some(source) = self
            .project
            .panes
            .iter()
            .find_map(|(&id, pane)| pane.tabs.iter().any(|tab| tab.key() == key).then_some(id))
        {
            if let Some(target) = (!matches!(tab, SavedTab::Panel { .. }))
                .then(|| workspace::bring_up_target(&self.project, self.focus_mode, source, &key))
                .flatten()
            {
                let from = self.project.panes.get_mut(&source).unwrap();
                let index = from.tabs.iter().position(|tab| tab.key() == key).unwrap();
                let tab = from.tabs.remove(index);
                from.shell_ids.clear();
                from.active_shell_id = None;
                self.project.panes.get_mut(&target).unwrap().tabs.push(tab);
                remove_emptied(&mut self.project, source, &self.unknown_panes);
            }
            return self.select_panel(&key);
        }
        let target = workspace::new_tab_target(
            self.project.main_pane,
            &self.project.panes,
            self.focus_mode,
            self.project.active_pane,
        );
        let mut candidate = self.project.clone();
        let pane = candidate
            .panes
            .get_mut(&target)
            .ok_or("not_found: target pane")?;
        pane.tabs.push(tab);
        pane.active_tab_key = Some(key);
        candidate.active_pane = target;
        candidate.normalize()?;
        self.project = candidate;
        Ok(self.dock(self.extent.unwrap_or(DEFAULT_EXTENT)))
    }

    pub fn open_pending_chat(&mut self, id: &str) -> Result<DockAreaLayout, String> {
        let uuid = id.strip_prefix("chat-pending:").unwrap_or(id);
        if uuid.is_empty() {
            return Err("invalid: pending chat id is empty".into());
        }
        let tab = SavedTab::Chat {
            chat_id: format!("pending:{uuid}"),
        };
        let key = tab.key();
        self.pending_chats.insert(key.clone());
        self.panel_ids.insert(key, format!("chat-pending:{uuid}"));
        self.open_tab(tab)
    }

    pub fn resolve_pending_chat(
        &mut self,
        panel_id: &str,
        chat_id: &str,
    ) -> Result<DockAreaLayout, String> {
        if chat_id.is_empty() {
            return Err("invalid: chat id is empty".into());
        }
        let key = self.tab_key(panel_id);
        if !self.pending_chats.contains(&key) {
            return Err("not_found: pending chat".into());
        }
        let resolved = SavedTab::Chat {
            chat_id: chat_id.to_owned(),
        };
        let resolved_key = resolved.key();
        if self.panel(&resolved_key).is_some() {
            return Err("invalid: chat is already open".into());
        }
        for pane in self.project.panes.values_mut() {
            if let Some(tab) = pane.tabs.iter_mut().find(|tab| tab.key() == key) {
                *tab = resolved.clone();
                if pane.active_tab_key.as_deref() == Some(&key) {
                    pane.active_tab_key = Some(resolved_key.clone());
                }
            }
        }
        let stable = self
            .panel_ids
            .remove(&key)
            .unwrap_or_else(|| panel_id.to_owned());
        self.pending_chats.remove(&key);
        self.panel_ids.insert(resolved_key, stable);
        self.project.normalize()?;
        Ok(self.dock(self.extent.unwrap_or(DEFAULT_EXTENT)))
    }

    pub fn close_panel(&mut self, id: &str, internal: bool) -> Result<DockAreaLayout, String> {
        let key = self.tab_key(id);
        let source = self
            .project
            .panes
            .iter()
            .find_map(|(&id, pane)| pane.tabs.iter().any(|tab| tab.key() == key).then_some(id))
            .ok_or("not_found: panel")?;
        if !internal && self.project.effective_locked_panes().contains(&source) {
            return Err("Unlock the pane to close its tabs".into());
        }
        let pane = self.project.panes.get_mut(&source).unwrap();
        let index = pane.tabs.iter().position(|tab| tab.key() == key).unwrap();
        let tab = pane.tabs.remove(index);
        if pane.active_tab_key.as_deref() == Some(&key) {
            pane.active_tab_key = pane
                .tabs
                .get(index.min(pane.tabs.len().saturating_sub(1)))
                .map(SavedTab::key);
        }
        pane.shell_ids.clear();
        pane.active_shell_id = None;
        if !internal {
            match tab {
                SavedTab::Shell { shell_id } => {
                    // An explicit reopen returns to this pane while it exists.
                    self.detached_shell_panes.insert(shell_id.clone(), source);
                    self.project.detached_shell_ids.insert(shell_id);
                }
                SavedTab::Chat { chat_id } if !self.pending_chats.contains(&key) => {
                    self.project.detached_chat_ids.insert(chat_id);
                }
                _ => {}
            }
        }
        let inheritor = workspace::pane_inheriting_space(&self.project.layout, source);
        remove_emptied(&mut self.project, source, &self.unknown_panes);
        if self.project.active_pane == source && !self.project.panes.contains_key(&source) {
            if let Some(id) = inheritor {
                self.project.active_pane = id;
            }
        }
        self.pending_chats.remove(&key);
        self.panel_ids.remove(&key);
        self.project.normalize()?;
        Ok(self.dock(self.extent.unwrap_or(DEFAULT_EXTENT)))
    }

    pub fn close_pane(&mut self, id: PaneId) -> Result<DockAreaLayout, String> {
        if !self.project.panes.contains_key(&id) {
            return Err("not_found: pane".into());
        }
        if self.project.effective_locked_panes().contains(&id) {
            return Err("Unlock the pane to close it".into());
        }
        let Some(rest) = self.project.layout.clone().without(id) else {
            let pane = self.project.panes.get_mut(&id).unwrap();
            for tab in std::mem::take(&mut pane.tabs) {
                match tab {
                    SavedTab::Shell { shell_id } => {
                        self.project.detached_shell_ids.insert(shell_id);
                    }
                    SavedTab::Chat { chat_id } => {
                        self.project.detached_chat_ids.insert(chat_id);
                    }
                    _ => {}
                }
            }
            pane.shell_ids.clear();
            pane.active_shell_id = None;
            pane.active_tab_key = None;
            self.project.normalize()?;
            return Ok(self.dock(self.extent.unwrap_or(DEFAULT_EXTENT)));
        };
        let target = workspace::aggregation_target(self.project.main_pane, id, false)
            .filter(|main| self.project.panes.contains_key(main));
        let inherit = target.or_else(|| workspace::pane_inheriting_space(&self.project.layout, id));
        let closed = workspace::close_pane_tabs(&mut self.project.panes, id, target);
        for tab in closed {
            match tab {
                SavedTab::Shell { shell_id } => {
                    self.project.detached_shell_ids.insert(shell_id);
                }
                SavedTab::Chat { chat_id } => {
                    self.project.detached_chat_ids.insert(chat_id);
                }
                _ => {}
            }
        }
        self.project.layout = rest;
        if let Some(inherit) = inherit {
            self.project.active_pane = inherit;
        }
        self.project.normalize()?;
        Ok(self.dock(self.extent.unwrap_or(DEFAULT_EXTENT)))
    }

    pub fn default_layout(&mut self) -> Result<DockAreaLayout, String> {
        let mut result = workspace::default_layout(&mut self.project, &self.allocated_pane_ids)?;
        let main = self
            .project
            .main_pane
            .expect("default layout has a main pane");
        let opaque_moved = self.unknown_panes.iter().any(|id| *id != main);
        if opaque_moved {
            // A previous save may already have replaced an unsafe disk id with
            // its safe alias. Keep both generations in the cumulative map.
            let previous_stored: Vec<_> = self
                .opaque_pane_remap
                .values()
                .map(|&current| {
                    self.pane_id_remap
                        .iter()
                        .find_map(|(&old, &alias)| {
                            (old <= MAX_SAFE_PANE_ID && alias == current).then_some(old)
                        })
                        .unwrap_or(current)
                })
                .collect();
            for target in self.opaque_pane_remap.values_mut() {
                *target = main;
            }
            for source in previous_stored {
                self.opaque_pane_remap.insert(source, main);
            }
            self.unknown_panes = BTreeSet::from([main]);
            if result.is_none() {
                result = Some(workspace::DefaultLayout {
                    navigation: self.project.layout.first_pane(),
                    main,
                    active_pane: self.project.active_pane,
                    moved: 0,
                    opened: Vec::new(),
                });
            }
        }
        self.notice = Some(
            result
                .as_ref()
                .map(workspace::default_layout_notice)
                .unwrap_or_else(|| "The default layout is already in place".into()),
        );
        self.allocated_pane_ids
            .extend(self.project.layout.pane_ids());
        self.restore_focus(false, false);
        Ok(self.dock(self.extent.unwrap_or(DEFAULT_EXTENT)))
    }

    pub fn reconcile_shells(
        &mut self,
        live: &[String],
        adoptable: &[String],
        hidden: &BTreeSet<String>,
    ) -> Result<DockAreaLayout, String> {
        let live: BTreeSet<_> = live.iter().collect();
        let hidden: BTreeSet<_> = hidden
            .iter()
            .map(|id| id.strip_prefix("shell:").unwrap_or(id).to_owned())
            .collect();
        // Keep scaffolding during restore so a later successful delivery can
        // populate the saved position, including selected empty main panes.
        for pane in self.project.panes.values_mut() {
            pane.tabs.retain(|tab| match tab {
                SavedTab::Shell { shell_id } => {
                    live.contains(shell_id)
                        && !hidden.contains(shell_id)
                        && !self.project.detached_shell_ids.contains(shell_id)
                }
                _ => true,
            });
            pane.shell_ids.clear();
            pane.active_shell_id = None;
        }
        let locks = self.project.effective_locked_panes();
        let target = self
            .project
            .main_pane
            .filter(|id| !locks.contains(id))
            .or_else(|| {
                (!locks.contains(&self.project.active_pane)).then_some(self.project.active_pane)
            })
            .or_else(|| {
                self.project
                    .layout
                    .pane_ids()
                    .into_iter()
                    .find(|id| !locks.contains(id))
            });
        if let Some(target) = target {
            let existing: BTreeSet<_> = self
                .project
                .panes
                .values()
                .flat_map(|pane| &pane.tabs)
                .filter_map(|tab| {
                    if let SavedTab::Shell { shell_id } = tab {
                        Some(shell_id.clone())
                    } else {
                        None
                    }
                })
                .collect();
            let mut added = BTreeSet::new();
            for id in adoptable
                .iter()
                .filter(|id| {
                    live.contains(id)
                        && !existing.contains(*id)
                        && !hidden.contains(*id)
                        && !self.project.detached_shell_ids.contains(*id)
                })
                .take(16)
            {
                if added.insert(id.clone()) {
                    self.project
                        .panes
                        .get_mut(&target)
                        .unwrap()
                        .tabs
                        .push(SavedTab::Shell {
                            shell_id: id.clone(),
                        });
                }
            }
        }
        self.project.normalize()?;
        Ok(self.dock(self.extent.unwrap_or(DEFAULT_EXTENT)))
    }

    /// Supplied by the persistence adapter's read-only inspection; contents stay
    /// opaque and only LayoutStore writes them. IDs here are the on-disk IDs.
    pub fn mark_unknown_panes(&mut self, panes: &BTreeSet<PaneId>) {
        self.opaque_pane_remap = panes
            .iter()
            .map(|id| (*id, self.pane_id_remap.get(id).copied().unwrap_or(*id)))
            .collect();
        self.unknown_panes = panes
            .iter()
            .map(|id| self.pane_id_remap.get(id).copied().unwrap_or(*id))
            .collect();
        self.last = None;
    }

    pub fn diagnostic(&self) -> Option<&str> {
        self.diagnostic.as_deref()
    }

    pub fn dock(&mut self, extent: Extent) -> DockAreaLayout {
        // A zero-size first layout is not a baseline for locked geometry.
        let extent = valid_extent(extent).unwrap_or_else(|| self.extent.unwrap_or(DEFAULT_EXTENT));
        self.extent = Some(extent);
        let extents = self.project.layout.pane_extents(extent);
        let locks = self.project.effective_locked_panes();
        let dock = DockAreaLayout::new(project_node(
            &self.project.layout,
            &self.project,
            &extents,
            &locks,
            &self.unknown_panes,
        ));
        self.last = Some(dock.clone());
        let mut visible = dock;
        remap_panel_keys(&mut visible.center, &self.panel_ids);
        visible
    }

    pub fn set_extent(&mut self, extent: Extent) -> Result<DockAreaLayout, String> {
        let extent =
            valid_extent(extent).ok_or("invalid: pane extent must be finite and positive")?;
        let previous = if self.focus_mode {
            None
        } else {
            self.unfocused_extent.take().or(self.extent)
        };
        if let Some(old) = previous.filter(|_| !self.skip_next_preserve) {
            let locks = self.project.effective_locked_panes();
            self.project.layout.preserve_locked(
                old,
                extent,
                &|id| locks.contains(&id),
                MIN_PANE_EXTENT,
            );
        }
        if !self.focus_mode {
            self.skip_next_preserve = false;
        }
        Ok(self.dock(extent))
    }

    pub fn select_panel(&mut self, key: &str) -> Result<DockAreaLayout, String> {
        let key = self.tab_key(key);
        let pane = self
            .project
            .panes
            .iter_mut()
            .find(|(_, pane)| pane.tabs.iter().any(|tab| tab.key() == key))
            .ok_or("not_found: panel is not in this layout")?;
        pane.1.active_tab_key = Some(key.to_owned());
        self.project.active_pane = *pane.0;
        self.project.normalize()?;
        Ok(self.dock(self.extent.unwrap_or(DEFAULT_EXTENT)))
    }

    /// Opening an existing session changes only placement. Engine creation and
    /// liveness checks belong to Sessions/the native terminal, never this model.
    pub fn open_shell(&mut self, shell_id: &str) -> Result<DockAreaLayout, String> {
        if shell_id.trim().is_empty() {
            return Err("invalid: shellId is required".into());
        }
        let key = format!("shell:{shell_id}");
        if self
            .project
            .panes
            .values()
            .any(|pane| pane.tabs.iter().any(|tab| tab.key() == key))
        {
            return self.select_panel(&key);
        }
        let locks = self.project.effective_locked_panes();
        let destination = self
            .detached_shell_panes
            .get(shell_id)
            .copied()
            .into_iter()
            .chain(self.project.main_pane)
            .chain(std::iter::once(self.project.active_pane))
            .chain(self.project.layout.pane_ids())
            .find(|id| self.project.panes.contains_key(id) && !locks.contains(id))
            .ok_or("refused: unlock a pane before opening a shell")?;
        let pane = self.project.panes.get_mut(&destination).unwrap();
        pane.tabs.push(SavedTab::Shell {
            shell_id: shell_id.to_owned(),
        });
        pane.active_tab_key = Some(key);
        self.project.active_pane = destination;
        self.project.detached_shell_ids.retain(|id| id != shell_id);
        self.detached_shell_panes.remove(shell_id);
        self.project.normalize()?;
        Ok(self.dock(self.extent.unwrap_or(DEFAULT_EXTENT)))
    }

    /// Vim reuses its existing tab. A new editor goes to main outside focus;
    /// otherwise prefer an unlocked terminal pane, then any unlocked pane. If
    /// all panes are locked, make an unlocked pane rather than rejecting Edit.
    pub fn open_editor_shell(&mut self, shell_id: &str) -> Result<DockAreaLayout, String> {
        if shell_id.trim().is_empty() {
            return Err("invalid: shellId is required".into());
        }
        let key = format!("shell:{shell_id}");
        if self.panel(&key).is_some() {
            return self.select_panel(&key);
        }
        let locks = self.project.effective_locked_panes();
        let target = self
            .project
            .main_pane
            .filter(|_| !self.focus_mode)
            .or_else(|| {
                self.project
                    .panes
                    .iter()
                    .find(|(id, pane)| {
                        !locks.contains(id)
                            && pane.tabs.iter().any(|tab| {
                                matches!(tab, SavedTab::Shell { .. } | SavedTab::RemoteShell { .. })
                            })
                    })
                    .map(|(id, _)| *id)
            })
            .or_else(|| {
                self.project
                    .layout
                    .pane_ids()
                    .into_iter()
                    .find(|id| !locks.contains(id))
            });
        if let Some(id) = target {
            self.project.active_pane = id;
        } else {
            self.split_active(false)?;
        }
        let pane = self
            .project
            .panes
            .get_mut(&self.project.active_pane)
            .unwrap();
        pane.tabs.push(SavedTab::Shell {
            shell_id: shell_id.to_owned(),
        });
        pane.active_tab_key = Some(key);
        self.project.detached_shell_ids.retain(|id| id != shell_id);
        self.detached_shell_panes.remove(shell_id);
        self.project.normalize()?;
        Ok(self.dock(self.extent.unwrap_or(DEFAULT_EXTENT)))
    }

    /// Detach removes this window's tab and records its identity for restore.
    /// It deliberately has no SessionManager, and never kills a tmux session.
    pub fn detach_shell(&mut self, shell_id: &str) -> Result<DockAreaLayout, String> {
        let key = format!("shell:{shell_id}");
        let (pane_id, pane) = self
            .project
            .panes
            .iter_mut()
            .find(|(_, pane)| pane.tabs.iter().any(|tab| tab.key() == key))
            .ok_or("not_found: shell tab is not in this layout")?;
        self.detached_shell_panes
            .insert(shell_id.to_owned(), *pane_id);
        pane.tabs.retain(|tab| tab.key() != key);
        // Clear legacy identity before normalize, which can migrate shell-only
        // saves into tabs when the final tab was removed.
        pane.shell_ids.retain(|id| id != shell_id);
        if pane.active_shell_id.as_deref() == Some(shell_id) {
            pane.active_shell_id = None;
        }
        if pane.active_tab_key.as_deref() == Some(&key) {
            pane.active_tab_key = pane.tabs.first().map(SavedTab::key);
        }
        if !self
            .project
            .detached_shell_ids
            .iter()
            .any(|id| id == shell_id)
        {
            self.project.detached_shell_ids.insert(shell_id.to_owned());
        }
        self.project.normalize()?;
        Ok(self.dock(self.extent.unwrap_or(DEFAULT_EXTENT)))
    }

    /// Split only the layout scaffold. Terminal creation is owned by Sessions.
    pub fn split_active(&mut self, down: bool) -> Result<DockAreaLayout, String> {
        let id = (1..=MAX_INITIAL_DOCK_PANE_ID)
            .find(|id| !self.allocated_pane_ids.contains(id))
            .ok_or("unavailable: no pane identifiers remain")?;
        let axis = if down {
            Axis::Stacked
        } else {
            Axis::SideBySide
        };
        if !self
            .project
            .layout
            .split(self.project.active_pane, axis, id)
        {
            return Err("not_found: active pane".into());
        }
        self.allocated_pane_ids.insert(id);
        self.project.panes.insert(id, SavedPane::default());
        self.project.active_pane = id;
        self.project.normalize()?;
        Ok(self.dock(self.extent.unwrap_or(DEFAULT_EXTENT)))
    }

    /// Remove a placeholder from this window only; never end its session.
    pub fn close_active(&mut self, whole_pane: bool) -> Result<DockAreaLayout, String> {
        let id = self.project.active_pane;
        if self.project.effective_locked_panes().contains(&id) {
            return Err("refused: unlock the pane before closing its tabs".into());
        }
        let pane = self
            .project
            .panes
            .get_mut(&id)
            .ok_or("not_found: active pane")?;
        if whole_pane {
            pane.tabs.clear();
        } else if let Some(key) = &pane.active_tab_key {
            pane.tabs.retain(|tab| &tab.key() != key);
        }
        pane.shell_ids.clear();
        pane.active_shell_id = None;
        pane.active_tab_key = None;
        remove_emptied(&mut self.project, id, &self.unknown_panes);
        self.project.normalize()?;
        Ok(self.dock(self.extent.unwrap_or(DEFAULT_EXTENT)))
    }

    pub fn select_pane(&mut self, id: PaneId) -> Result<DockAreaLayout, String> {
        if !self.project.panes.contains_key(&id) {
            return Err("not_found: pane".into());
        }
        self.project.active_pane = id;
        Ok(self.dock(self.extent.unwrap_or(DEFAULT_EXTENT)))
    }

    pub fn toggle_lock(&mut self, id: PaneId) -> Result<DockAreaLayout, String> {
        if !self.project.panes.contains_key(&id) {
            return Err("not_found: pane".into());
        }
        let mut locks = self.project.effective_locked_panes();
        if !locks.remove(&id) {
            locks.insert(id);
        }
        self.project.locked_panes = Some(locks);
        Ok(self.dock(self.extent.unwrap_or(DEFAULT_EXTENT)))
    }

    pub fn toggle_main(&mut self, id: PaneId) -> Result<DockAreaLayout, String> {
        if !self.project.panes.contains_key(&id) {
            return Err("not_found: pane".into());
        }
        self.project.main_pane = (self.project.main_pane != Some(id)).then_some(id);
        Ok(self.dock(self.extent.unwrap_or(DEFAULT_EXTENT)))
    }

    /// Gather in the core's tree order. No tab is closed and locked sources,
    /// main selection, and scaffolds that protect locked geometry survive.
    pub fn gather(&mut self) -> Result<DockAreaLayout, String> {
        self.restore_focus(false, false);
        let result = workspace::gather(&mut self.project, &self.unknown_panes)?;
        self.notice = Some(workspace::gather_notice(&result));
        Ok(self.dock(self.extent.unwrap_or(DEFAULT_EXTENT)))
    }

    pub fn apply_dock(
        &mut self,
        mut next: DockAreaLayout,
        extent: Extent,
    ) -> Result<DockAreaLayout, String> {
        let extent =
            valid_extent(extent).ok_or("invalid: pane extent must be finite and positive")?;
        if self.last.is_none() {
            self.dock(extent);
        }
        let previous = self.last.clone().unwrap();
        let reverse = self
            .panel_ids
            .iter()
            .map(|(key, id)| (id.clone(), key.clone()))
            .collect();
        remap_panel_keys(&mut next.center, &reverse);
        for region in [&mut next.left, &mut next.right, &mut next.bottom]
            .into_iter()
            .flatten()
        {
            remap_panel_keys(&mut region.layout, &reverse);
        }
        self.diagnostic = None;
        let previous_node = canonical(&previous.center)?;
        let has_docks = next.left.is_some() || next.right.is_some() || next.bottom.is_some();
        let next_node = canonical(&center_with_docks(next, extent))?;
        let old_groups = groups(&previous_node)?;
        let next_groups = groups(&next_node)?;
        let old_locations = locations(&old_groups)?;
        let next_locations = locations(&next_groups)?;
        if old_locations.keys().ne(next_locations.keys()) {
            return Err(
                "refused: DockArea cannot create or close tabs; use the app tab flow".into(),
            );
        }
        for (&id, group) in &next_groups {
            for key in &group.panels {
                if key.starts_with("empty:") && key != &format!("empty:{id}") {
                    return Err("refused: empty-pane placeholders cannot move".into());
                }
            }
        }
        if !has_docks && equivalent(&previous_node, &next_node, true, true) {
            // NSH assigns split IDs and may wrap the root in a one-child split.
            // Neither changes the authoritative binary shape or stored ratios.
            return Ok(self.dock(extent));
        }

        let mut candidate = self.project.clone();
        let same_tree = equivalent(&previous_node, &next_node, false, false);
        let moved: Vec<_> = old_locations
            .iter()
            .filter_map(|(key, (id, _))| {
                let (target, _) = next_locations[key];
                (id != &target).then_some((key.clone(), *id, target))
            })
            .collect();
        let added: Vec<_> = next_groups
            .keys()
            .filter(|id| !old_groups.contains_key(id))
            .copied()
            .collect();
        let removed: Vec<_> = old_groups
            .keys()
            .filter(|id| !next_groups.contains_key(id))
            .copied()
            .collect();
        let same_order = old_groups.iter().all(|(id, old)| {
            next_groups
                .get(id)
                .is_some_and(|new| real_panels(old) == real_panels(new))
        });
        let size_changed = same_tree && !equivalent_sizes(&previous_node, &next_node);
        let selected_changed: Vec<_> = old_groups
            .iter()
            .filter_map(|(&id, old)| {
                next_groups
                    .get(&id)
                    .filter(|new| active_real(old) != active_real(new))
                    .map(|_| id)
            })
            .collect();
        let edge = added
            .first()
            .and_then(|&new_id| edge_split(&next_node, new_id, &candidate, extent));
        let single_move_shape = if moved.len() == 1 && added.len() <= 1 && removed.len() <= 1 {
            let mut expected = candidate.layout.clone();
            let split_ok = match (added.first(), edge) {
                (Some(&new_id), Some((anchor, axis, new_first, ratio))) => {
                    expected.split_with_ratio(anchor, axis, new_id, new_first, ratio)
                }
                (Some(_), None) => false,
                (None, _) => true,
            };
            // NSH removes an emptied group immediately. Compare its report to
            // that topology; RiWork may restore a protected empty group later.
            for &id in &removed {
                if let Some(tree) = expected.clone().without(id) {
                    expected = tree;
                }
            }
            split_ok && equivalent(&shape_node(&expected), &next_node, false, false)
        } else {
            false
        };

        if !has_docks && same_tree && same_order && !size_changed && selected_changed.len() <= 1 {
            sync_selection(&mut candidate, &next_groups, &selected_changed);
        } else if !has_docks && same_tree && same_order && selected_changed.is_empty() {
            let reported = dock_extents(&next_node, extent)?;
            replay_ratios(&mut candidate.layout, &reported, &mut Vec::new());
        } else if !has_docks
            && same_tree
            && moved.is_empty()
            && !size_changed
            && reordered_count(&old_groups, &next_groups) == 1
        {
            apply_groups(&mut candidate, &next_groups, &old_groups, None)?;
        } else if !has_docks
            && single_move_shape
            && single_tab_move(&old_groups, &next_groups, &moved[0].0)
        {
            let (key, source, target) = &moved[0];
            if let Some(&new_id) = added.first() {
                if new_id != *target {
                    return Err("invalid: new group is not the move destination".into());
                }
                let (anchor, axis, new_first, ratio) =
                    edge.ok_or("invalid: new group must split an existing pane edge")?;
                if !candidate
                    .layout
                    .split_with_ratio(anchor, axis, new_id, new_first, ratio)
                {
                    return Err("invalid: split target is absent".into());
                }
                candidate.panes.insert(new_id, SavedPane::default());
            }
            apply_groups(
                &mut candidate,
                &next_groups,
                &old_groups,
                Some((*source, *target, key)),
            )?;
            remove_emptied(&mut candidate, *source, &self.unknown_panes);
        } else {
            self.diagnostic =
                Some("layout.fallback: compound DockArea edit rebuilt from reported tree".into());
            eprintln!("{}", self.diagnostic.as_deref().unwrap());
            candidate.layout = rebuild(&next_node, extent)?;
            apply_groups(&mut candidate, &next_groups, &old_groups, None)?;
            // A compound gesture may not silently discard an opaque tab pane or
            // the main pane. Simple moves above restore them in their old scaffold.
            for id in &removed {
                if self.unknown_panes.contains(id)
                    || self.project.main_pane == Some(*id)
                    || self.project.layout.removal_resizes_locked(*id, &|pane| {
                        self.project.effective_locked_panes().contains(&pane)
                    })
                {
                    return Err(
                        "refused: compound layout edit would remove a protected empty pane".into(),
                    );
                }
            }
        }
        // Normalize only the candidate: validation failure never replaces live state.
        candidate.normalize()?;
        // A native allocation above the ceiling must not collide on disk with
        // one of our loaded, session-only aliases. Such a new group receives a
        // fresh low identity; unlike a load alias it has no old disk ID to restore.
        let incoming_ids = candidate.layout.pane_ids();
        let occupied: BTreeSet<_> = incoming_ids.iter().copied().collect();
        let mut incoming_remap = BTreeMap::new();
        let mut free = 1;
        for id in incoming_ids {
            if id > MAX_INITIAL_DOCK_PANE_ID {
                while self.allocated_pane_ids.contains(&free)
                    || occupied.contains(&free)
                    || incoming_remap.values().any(|&id| id == free)
                {
                    free += 1;
                }
                if free > MAX_INITIAL_DOCK_PANE_ID {
                    return Err("invalid: no unused DockArea pane identity remains".into());
                }
                incoming_remap.insert(id, free);
            }
        }
        remap_project(&mut candidate, &incoming_remap);
        self.allocated_pane_ids.extend(candidate.layout.pane_ids());
        self.project = candidate;
        Ok(self.dock(extent))
    }
}

const DEFAULT_EXTENT: Extent = Extent {
    width: 1280.0,
    height: 800.0,
};

fn remap_panel_keys(node: &mut DockNode, mapping: &BTreeMap<String, String>) {
    match node {
        DockNode::Tabs { panels, .. } => {
            for panel in panels {
                if let Some(id) = mapping.get(panel) {
                    *panel = id.clone();
                }
            }
        }
        DockNode::Split { children, .. } => {
            for child in children {
                remap_panel_keys(child, mapping);
            }
        }
    }
}

fn valid_extent(extent: Extent) -> Option<Extent> {
    (extent.width.is_finite()
        && extent.height.is_finite()
        && extent.width > 0.0
        && extent.height > 0.0)
        .then_some(extent)
}

fn remap_project(project: &mut ProjectLayout, mapping: &BTreeMap<PaneId, PaneId>) {
    fn tree(layout: &mut Layout, mapping: &BTreeMap<PaneId, PaneId>) {
        match layout {
            Layout::Pane(id) => *id = mapping.get(id).copied().unwrap_or(*id),
            Layout::Split { first, second, .. } => {
                tree(first, mapping);
                tree(second, mapping);
            }
        }
    }
    tree(&mut project.layout, mapping);
    project.panes = std::mem::take(&mut project.panes)
        .into_iter()
        .map(|(id, pane)| (mapping.get(&id).copied().unwrap_or(id), pane))
        .collect();
    project.active_pane = mapping
        .get(&project.active_pane)
        .copied()
        .unwrap_or(project.active_pane);
    project.main_pane = project
        .main_pane
        .map(|id| mapping.get(&id).copied().unwrap_or(id));
    if let Some(locked) = &mut project.locked_panes {
        *locked = locked
            .iter()
            .map(|id| mapping.get(id).copied().unwrap_or(*id))
            .collect();
    }
}

fn along(extent: Extent, axis: Axis) -> f32 {
    match axis {
        Axis::SideBySide => extent.width,
        Axis::Stacked => extent.height,
    }
}

fn with_along(mut extent: Extent, axis: Axis, value: f32) -> Extent {
    match axis {
        Axis::SideBySide => extent.width = value,
        Axis::Stacked => extent.height = value,
    }
    extent
}

fn subtree_extent(layout: &Layout, leaves: &BTreeMap<PaneId, Extent>) -> Extent {
    match layout {
        Layout::Pane(id) => leaves[id],
        Layout::Split {
            axis,
            first,
            second,
            ..
        } => {
            let a = subtree_extent(first, leaves);
            let b = subtree_extent(second, leaves);
            match axis {
                Axis::SideBySide => Extent {
                    width: a.width + DIVIDER_THICKNESS + b.width,
                    height: a.height.max(b.height),
                },
                Axis::Stacked => Extent {
                    width: a.width.max(b.width),
                    height: a.height + DIVIDER_THICKNESS + b.height,
                },
            }
        }
    }
}

// Same holding predicate as Layout::preserve_locked's private grip rule. The
// actual resize always goes through preserve_locked (including its minima).
fn holds(layout: &Layout, axis: Axis, locked: &HashSet<PaneId>) -> bool {
    match layout {
        Layout::Pane(id) => locked.contains(id),
        Layout::Split {
            axis: own,
            first,
            second,
            ..
        } if *own == axis => holds(first, axis, locked) && holds(second, axis, locked),
        Layout::Split { first, second, .. } => {
            holds(first, axis, locked) || holds(second, axis, locked)
        }
    }
}

fn flatten_axis<'a>(layout: &'a Layout, axis: Axis, children: &mut Vec<&'a Layout>) {
    match layout {
        Layout::Split {
            axis: own,
            first,
            second,
            ..
        } if *own == axis => {
            flatten_axis(first, axis, children);
            flatten_axis(second, axis, children);
        }
        _ => children.push(layout),
    }
}

fn shape_node(layout: &Layout) -> DockNode {
    match layout {
        Layout::Pane(id) => DockNode::Tabs {
            id: Some(*id),
            panels: Vec::new(),
            active_index: 0,
        },
        Layout::Split { axis, .. } => {
            let mut children = Vec::new();
            flatten_axis(layout, *axis, &mut children);
            DockNode::Split {
                id: None,
                axis: (*axis).into(),
                children: children.into_iter().map(shape_node).collect(),
                sizes: Vec::new(),
            }
        }
    }
}

fn project_node(
    layout: &Layout,
    project: &ProjectLayout,
    extents: &BTreeMap<PaneId, Extent>,
    locked: &HashSet<PaneId>,
    unknown: &BTreeSet<PaneId>,
) -> DockNode {
    match layout {
        Layout::Pane(id) => {
            let pane = &project.panes[id];
            let mut panels: Vec<_> = pane.tabs.iter().map(SavedTab::key).collect();
            let active_index = pane
                .active_tab_key
                .as_ref()
                .and_then(|key| panels.iter().position(|p| p == key))
                .unwrap_or(0);
            if panels.is_empty() || unknown.contains(id) {
                panels.push(format!("empty:{id}"));
            }
            DockNode::Tabs {
                id: Some(*id),
                panels,
                active_index,
            }
        }
        Layout::Split { axis, .. } => {
            let mut parts = Vec::new();
            flatten_axis(layout, *axis, &mut parts);
            let holding: Vec<_> = parts
                .iter()
                .map(|child| holds(child, *axis, locked))
                .collect();
            let one_absorber = holding.iter().any(|&holds| holds)
                && holding.iter().filter(|&&holds| !holds).count() == 1;
            let sizes = parts
                .iter()
                .zip(holding)
                .map(|(child, holds)| {
                    if one_absorber && !holds {
                        None
                    } else {
                        Some(along(subtree_extent(child, extents), *axis))
                    }
                })
                .collect();
            let children = parts
                .iter()
                .map(|child| project_node(child, project, extents, locked, unknown))
                .collect();
            DockNode::Split {
                id: None,
                axis: (*axis).into(),
                children,
                sizes,
            }
        }
    }
}

#[derive(Clone, Debug)]
struct Group {
    panels: Vec<String>,
    active_index: usize,
}

fn groups(node: &DockNode) -> Result<BTreeMap<PaneId, Group>, String> {
    fn visit(
        node: &DockNode,
        depth: usize,
        result: &mut BTreeMap<PaneId, Group>,
    ) -> Result<(), String> {
        if depth > 32 {
            return Err("invalid: DockArea exceeds depth 32".into());
        }
        match node {
            DockNode::Tabs {
                id,
                panels,
                active_index,
            } => {
                let id = id
                    .filter(|id| *id > 0 && *id <= MAX_SAFE_PANE_ID)
                    .ok_or("invalid: missing or unsafe pane id")?;
                if result
                    .insert(
                        id,
                        Group {
                            panels: panels.clone(),
                            active_index: *active_index,
                        },
                    )
                    .is_some()
                {
                    return Err("invalid: duplicate pane id".into());
                }
                if result.len() > 256 {
                    return Err("invalid: DockArea exceeds 256 panes".into());
                }
            }
            DockNode::Split { children, .. } => {
                for child in children {
                    visit(child, depth + 1, result)?;
                }
            }
        }
        Ok(())
    }
    let mut result = BTreeMap::new();
    visit(node, 0, &mut result)?;
    Ok(result)
}

fn real_panels(group: &Group) -> Vec<&str> {
    group
        .panels
        .iter()
        .filter(|key| !key.starts_with("empty:"))
        .map(String::as_str)
        .collect()
}

fn active_real(group: &Group) -> Option<&str> {
    group
        .panels
        .get(group.active_index)
        .filter(|key| !key.starts_with("empty:"))
        .map(String::as_str)
}

fn locations(
    groups: &BTreeMap<PaneId, Group>,
) -> Result<BTreeMap<String, (PaneId, usize)>, String> {
    let mut result = BTreeMap::new();
    for (&id, group) in groups {
        for (index, key) in real_panels(group).into_iter().enumerate() {
            if result.insert(key.to_owned(), (id, index)).is_some() {
                return Err("invalid: duplicate panel id".into());
            }
        }
    }
    Ok(result)
}

fn canonical(node: &DockNode) -> Result<DockNode, String> {
    fn walk(node: &DockNode, depth: usize) -> Result<DockNode, String> {
        if depth > 32 {
            return Err("invalid: DockArea exceeds depth 32".into());
        }
        match node {
            DockNode::Tabs { .. } => Ok(node.clone()),
            DockNode::Split {
                axis,
                children,
                sizes,
                ..
            } => {
                if children.is_empty() {
                    return Err("invalid: split has no children".into());
                }
                if children.len() > 256 {
                    return Err("invalid: split exceeds 256 children".into());
                }
                let mut children: Vec<_> = children
                    .iter()
                    .map(|child| walk(child, depth + 1))
                    .collect::<Result<_, _>>()?;
                if children.len() == 1 {
                    return Ok(children.remove(0));
                }
                if sizes
                    .iter()
                    .flatten()
                    .any(|size| !size.is_finite() || *size < 0.0)
                {
                    return Err("invalid: split size must be finite and nonnegative".into());
                }
                let sizes = (0..children.len())
                    .map(|i| sizes.get(i).copied().flatten())
                    .collect();
                Ok(DockNode::Split {
                    id: None,
                    axis: *axis,
                    children,
                    sizes,
                })
            }
        }
    }
    walk(node, 0)
}

fn equivalent(a: &DockNode, b: &DockNode, panels: bool, sizes: bool) -> bool {
    match (a, b) {
        (
            DockNode::Tabs {
                id: a,
                panels: ap,
                active_index: ai,
            },
            DockNode::Tabs {
                id: b,
                panels: bp,
                active_index: bi,
            },
        ) => a == b && (!panels || (ap == bp && ai == bi)),
        (
            DockNode::Split {
                axis: a,
                children: ac,
                sizes: az,
                ..
            },
            DockNode::Split {
                axis: b,
                children: bc,
                sizes: bz,
                ..
            },
        ) => {
            a == b
                && ac.len() == bc.len()
                && (!sizes || sizes_equal(az, bz))
                && ac
                    .iter()
                    .zip(bc)
                    .all(|(a, b)| equivalent(a, b, panels, sizes))
        }
        _ => false,
    }
}

fn sizes_equal(a: &[Option<f32>], b: &[Option<f32>]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(a, b)| match (a, b) {
            (Some(a), Some(b)) => (a - b).abs() < 0.001,
            (None, None) => true,
            _ => false,
        })
}

fn equivalent_sizes(a: &DockNode, b: &DockNode) -> bool {
    equivalent(a, b, false, true)
}

/// Match NSH's basis/flex distribution after subtracting core's divider widths.
fn resolve_sizes(sizes: &[Option<f32>], count: usize, available: f32) -> Vec<f32> {
    let available = (available - DIVIDER_THICKNESS * count.saturating_sub(1) as f32).max(0.0);
    let fixed: f32 = sizes.iter().take(count).flatten().sum();
    let flex_count = (0..count)
        .filter(|&i| sizes.get(i).copied().flatten().is_none())
        .count();
    (0..count)
        .map(|i| match sizes.get(i).copied().flatten() {
            Some(size) if flex_count > 0 && fixed <= available => size,
            Some(size) if fixed > 0.0 => size / fixed * available,
            Some(_) => available / count as f32,
            None if flex_count > 0 => (available - fixed).max(0.0) / flex_count as f32,
            None => 0.0,
        })
        .collect()
}

fn dock_extents(node: &DockNode, extent: Extent) -> Result<BTreeMap<PaneId, Extent>, String> {
    fn walk(
        node: &DockNode,
        extent: Extent,
        out: &mut BTreeMap<PaneId, Extent>,
    ) -> Result<(), String> {
        match node {
            DockNode::Tabs { id, .. } => {
                out.insert(id.ok_or("invalid: missing pane id")?, extent);
            }
            DockNode::Split {
                axis,
                children,
                sizes,
                ..
            } => {
                let axis = (*axis).into();
                let sizes = resolve_sizes(sizes, children.len(), along(extent, axis));
                for (child, size) in children.iter().zip(sizes) {
                    walk(child, with_along(extent, axis, size), out)?;
                }
            }
        }
        Ok(())
    }
    let mut out = BTreeMap::new();
    walk(node, extent, &mut out)?;
    Ok(out)
}

fn replay_ratios(layout: &mut Layout, extents: &BTreeMap<PaneId, Extent>, path: &mut Vec<bool>) {
    fn ratios(
        layout: &Layout,
        extents: &BTreeMap<PaneId, Extent>,
        path: &mut Vec<bool>,
        out: &mut Vec<(Vec<bool>, f32)>,
    ) {
        if let Layout::Split {
            axis,
            ratio,
            first,
            second,
        } = layout
        {
            let a = along(subtree_extent(first, extents), *axis);
            let b = along(subtree_extent(second, extents), *axis);
            if a + b > 0.0 && (a / (a + b) - ratio).abs() > 0.00001 {
                out.push((path.clone(), a / (a + b)));
            }
            path.push(false);
            ratios(first, extents, path, out);
            path.pop();
            path.push(true);
            ratios(second, extents, path, out);
            path.pop();
        }
    }
    let mut changes = Vec::new();
    ratios(layout, extents, path, &mut changes);
    for (path, ratio) in changes {
        layout.set_ratio(&path, ratio);
    }
}

fn sync_selection(
    project: &mut ProjectLayout,
    groups: &BTreeMap<PaneId, Group>,
    changed: &[PaneId],
) {
    for &id in changed {
        if let (Some(pane), Some(key)) = (project.panes.get_mut(&id), active_real(&groups[&id])) {
            pane.active_tab_key = Some(key.to_owned());
            project.active_pane = id;
        }
    }
}

fn reordered_count(old: &BTreeMap<PaneId, Group>, new: &BTreeMap<PaneId, Group>) -> usize {
    old.iter()
        .filter(|(id, group)| {
            new.get(id)
                .is_some_and(|new| real_panels(group) != real_panels(new))
        })
        .count()
}

fn single_tab_move(
    old: &BTreeMap<PaneId, Group>,
    new: &BTreeMap<PaneId, Group>,
    moved: &str,
) -> bool {
    old.keys().chain(new.keys()).all(|id| {
        let without = |groups: &BTreeMap<PaneId, Group>| {
            groups
                .get(id)
                .map(|g| {
                    real_panels(g)
                        .into_iter()
                        .filter(|&key| key != moved)
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        without(old) == without(new)
    })
}

fn apply_groups(
    project: &mut ProjectLayout,
    groups: &BTreeMap<PaneId, Group>,
    old: &BTreeMap<PaneId, Group>,
    moved: Option<(PaneId, PaneId, &str)>,
) -> Result<(), String> {
    let tabs: BTreeMap<_, _> = project
        .panes
        .values()
        .flat_map(|p| p.tabs.iter().map(|t| (t.key(), t.clone())))
        .collect();
    // Clear legacy fields too: otherwise normalize would restore a moved shell.
    for pane in project.panes.values_mut() {
        pane.tabs.clear();
        pane.shell_ids.clear();
        pane.active_shell_id = None;
    }
    for (&id, group) in groups {
        let pane = project.panes.entry(id).or_default();
        pane.tabs = real_panels(group)
            .iter()
            .map(|key| {
                tabs.get(*key)
                    .cloned()
                    .ok_or_else(|| format!("invalid: unknown panel {key}"))
            })
            .collect::<Result<_, _>>()?;
        let prior = pane.active_tab_key.clone();
        pane.active_tab_key = active_real(group).map(str::to_owned);
        if let Some((source, target, key)) = moved {
            if id == source
                && prior
                    .as_ref()
                    .is_some_and(|k| pane.tabs.iter().any(|t| t.key() == *k))
            {
                pane.active_tab_key = prior;
            }
            if id == target {
                pane.active_tab_key = Some(key.to_owned());
                project.active_pane = id;
            }
        } else if old
            .get(&id)
            .is_some_and(|before| active_real(before) != active_real(group))
        {
            project.active_pane = id;
        }
    }
    Ok(())
}

fn remove_emptied(project: &mut ProjectLayout, source: PaneId, unknown: &BTreeSet<PaneId>) {
    if !project
        .panes
        .get(&source)
        .is_some_and(|p| p.tabs.is_empty())
    {
        return;
    }
    let locks = project.effective_locked_panes();
    let keep = project.main_pane == Some(source)
        || unknown.contains(&source)
        || project
            .layout
            .removal_resizes_locked(source, &|id| locks.contains(&id));
    if !keep {
        if let Some(layout) = project.layout.clone().without(source) {
            project.layout = layout;
            project.panes.remove(&source);
        }
    }
}

fn edge_split(
    node: &DockNode,
    new: PaneId,
    project: &ProjectLayout,
    extent: Extent,
) -> Option<(PaneId, Axis, bool, f32)> {
    fn candidates(node: &DockNode, new: PaneId, out: &mut Vec<(PaneId, Axis, bool)>) {
        if let DockNode::Split { axis, children, .. } = node {
            if let Some(index) = children.iter().position(
                |child| matches!(child, DockNode::Tabs { id: Some(id), .. } if *id == new),
            ) {
                for (neighbor, child) in children.iter().enumerate() {
                    if neighbor.abs_diff(index) == 1 {
                        if let DockNode::Tabs { id: Some(id), .. } = child {
                            out.push((*id, (*axis).into(), index < neighbor));
                        }
                    }
                }
            }
            for child in children {
                candidates(child, new, out);
            }
        }
    }
    let reported = dock_extents(node, extent).ok()?;
    let before = project.layout.pane_extents(extent);
    let mut options = Vec::new();
    candidates(node, new, &mut options);
    options
        .into_iter()
        .filter(|(id, _, _)| project.panes.contains_key(id))
        .map(|(id, axis, new_first)| {
            let new_size = along(reported[&new], axis);
            let target_size = along(reported[&id], axis);
            let ratio =
                if new_first { new_size } else { target_size } / (new_size + target_size).max(1.0);
            // Flattening erases the split's binary parent. The untouched neighbor's
            // former extent identifies which adjacent pane supplied the new space.
            let error =
                (along(before[&id], axis) - new_size - target_size - DIVIDER_THICKNESS).abs();
            (error, id, axis, new_first, ratio)
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, id, axis, first, ratio)| (id, axis, first, ratio))
}

fn rebuild(node: &DockNode, extent: Extent) -> Result<Layout, String> {
    match node {
        DockNode::Tabs { id, .. } => Ok(Layout::Pane(id.ok_or("invalid: missing pane id")?)),
        DockNode::Split {
            axis,
            children,
            sizes,
            ..
        } => {
            let axis: Axis = (*axis).into();
            let lengths = resolve_sizes(sizes, children.len(), along(extent, axis));
            let mut children: Vec<_> = children
                .iter()
                .zip(&lengths)
                .map(|(child, &size)| rebuild(child, with_along(extent, axis, size)))
                .collect::<Result<_, _>>()?;
            let mut tree = children.pop().ok_or("invalid: empty split")?;
            let mut remaining = *lengths.last().unwrap();
            for (child, &size) in children.into_iter().zip(lengths.iter()).rev() {
                let ratio = if size + remaining > 0.0 {
                    size / (size + remaining)
                } else {
                    0.5
                };
                tree = Layout::Split {
                    axis,
                    ratio,
                    first: Box::new(child),
                    second: Box::new(tree),
                };
                tree.set_ratio(&[], ratio);
                remaining += size + DIVIDER_THICKNESS;
            }
            Ok(tree)
        }
    }
}

fn center_with_docks(next: DockAreaLayout, extent: Extent) -> DockNode {
    let mut center = next.center;
    let mut horizontal = Vec::new();
    let mut sizes = Vec::new();
    if let Some(left) = next.left {
        horizontal.push(left.layout);
        sizes.push(left.size);
    }
    horizontal.push(center);
    sizes.push(None);
    if let Some(right) = next.right {
        horizontal.push(right.layout);
        sizes.push(right.size);
    }
    center = if horizontal.len() == 1 {
        horizontal.remove(0)
    } else {
        DockNode::Split {
            id: None,
            axis: DockAxis::Horizontal,
            children: horizontal,
            sizes,
        }
    };
    if let Some(bottom) = next.bottom {
        let size = bottom.size.unwrap_or(extent.height * 0.25);
        center = DockNode::Split {
            id: None,
            axis: DockAxis::Vertical,
            children: vec![center, bottom.layout],
            sizes: vec![None, Some(size)],
        };
    }
    center
}

#[cfg(test)]
mod tests {
    use super::*;
    use nsh_base::dock::{
        DockLayout, DockNode as NativeNode, InsertTarget, NodeId, PaneTree, RootKind,
    };
    use nsh_core::{
        element::Placement,
        geometry::{Axis as NativeAxis, px},
    };
    use riwork_core::layouts::{PanelKind, TabEdge};
    use serde_json::json;

    fn fixture(layout: Layout, tabs: &[(PaneId, &[&str])]) -> ProjectLayout {
        let mut project: ProjectLayout = serde_json::from_value(json!({
            "layout": layout, "active_pane": layout.first_pane(),
            "locked_panes": [], "selected_worktree_id": "worktree-1",
            "selected_task_id": "task-1", "window_size": {"width": 1220, "height": 780},
            "detached_shell_ids": ["detached"], "detached_chat_ids": ["closed-chat"],
            "chat_dismissal_revision": 7, "sidebar_visible": false, "panels_initialized": true
        }))
        .unwrap();
        for &(id, keys) in tabs {
            project.panes.insert(
                id,
                SavedPane {
                    tabs: keys
                        .iter()
                        .map(|key| SavedTab::Shell {
                            shell_id: (*key).into(),
                        })
                        .collect(),
                    active_tab_key: keys.last().map(|key| format!("shell:{key}")),
                    ..Default::default()
                },
            );
        }
        project.normalize().unwrap();
        project
    }

    #[test]
    fn files_preview_reveal_uses_main_without_moving_focus_or_splitting() {
        let mut model = LayoutModel::new(crate::persistence::default_project_layout()).unwrap();
        model.open_panel(PanelKind::Files).unwrap();
        model
            .set_extent(Extent {
                width: 1220.0,
                height: 780.0,
            })
            .unwrap();
        let before = model.project().clone();
        model.reveal_panel(PanelKind::Preview, false, true).unwrap();
        assert_eq!(model.project().layout, before.layout);
        assert_eq!(model.project().active_pane, before.active_pane);
        assert_eq!(
            model.project().panes[&1].active_tab_key.as_deref(),
            Some("panel:files")
        );
        assert_eq!(
            model.project().panes[&2].active_tab_key.as_deref(),
            Some("panel:preview")
        );
        model.reveal_panel(PanelKind::Preview, false, true).unwrap();
        assert_eq!(
            model.project().panes[&2]
                .tabs
                .iter()
                .filter(|tab| tab.key() == "panel:preview")
                .count(),
            1
        );
    }

    #[test]
    fn file_link_reveal_keeps_the_clicked_terminal_and_keys() {
        let mut project = fixture(Layout::Pane(7), &[(7, &["work"])]);
        project.main_pane = Some(7);
        let mut model = LayoutModel::new(project).unwrap();
        model.reveal_panel(PanelKind::Preview, true, false).unwrap();
        assert_eq!(model.project().active_pane, 7);
        assert_eq!(
            model.project().panes[&7].active_tab_key.as_deref(),
            Some("shell:work")
        );
        assert!(
            model.project().panes[&7]
                .tabs
                .iter()
                .any(|tab| tab.key() == "panel:preview")
        );
    }

    #[test]
    fn preview_without_main_splits_beside_tree_and_obeys_preference() {
        let mut project = fixture(Layout::Pane(1), &[(1, &[])]);
        project.panes.get_mut(&1).unwrap().tabs = vec![SavedTab::Panel {
            panel: PanelKind::Files,
        }];
        project.panes.get_mut(&1).unwrap().active_tab_key = Some("panel:files".into());
        project.locked_panes = Some(HashSet::new());
        let mut model = LayoutModel::new(project).unwrap();
        model
            .set_extent(Extent {
                width: 1000.0,
                height: 800.0,
            })
            .unwrap();
        model
            .reveal_panel(PanelKind::Preview, false, false)
            .unwrap();
        assert_eq!(model.project().layout.pane_ids(), vec![1]);
        model.reveal_panel(PanelKind::Preview, false, true).unwrap();
        assert_eq!(model.project().layout.pane_ids().len(), 2);
        assert_eq!(model.project().active_pane, 1);
        match &model.project().layout {
            Layout::Split { ratio, axis, .. } => {
                assert_eq!(*ratio, riwork_core::layouts::PREVIEW_SIDE_BY_SIDE_RATIO);
                assert_eq!(*axis, Axis::SideBySide);
            }
            _ => panic!("expected split"),
        }
    }

    #[test]
    fn panel_and_shell_intents_reuse_identity_and_select_their_own_panes() {
        let project = crate::persistence::default_project_layout();
        let mut model = LayoutModel::new(project).unwrap();
        model.open_panel(PanelKind::Tasks).unwrap();
        assert_eq!(model.project().active_pane, 1);
        assert_eq!(
            model.project().panes[&1].active_tab_key.as_deref(),
            Some("panel:tasks")
        );
        model.select_pane(2).unwrap();
        model.open_panel(PanelKind::Usage).unwrap();
        assert_eq!(
            model.project().panes[&2].active_tab_key.as_deref(),
            Some("panel:usage")
        );
        model.select_pane(1).unwrap();
        model.open_panel(PanelKind::Usage).unwrap();
        assert_eq!(model.project().active_pane, 2);
        assert_eq!(
            model
                .project()
                .panes
                .values()
                .flat_map(|pane| &pane.tabs)
                .filter(|tab| tab.key() == "panel:usage")
                .count(),
            1
        );
        model
            .project
            .detached_shell_ids
            .insert("saved-shell".into());
        model.select_pane(1).unwrap();
        model.show_shell("saved-shell").unwrap();
        model.show_shell("saved-shell").unwrap();
        assert_eq!(
            model.project().active_pane,
            2,
            "new shell intents use the main pane"
        );
        assert!(!model.project().detached_shell_ids.contains("saved-shell"));
        assert_eq!(
            model.project().panes[&2]
                .tabs
                .iter()
                .filter(|tab| tab.key() == "shell:saved-shell")
                .count(),
            1
        );
        let before = model.project().clone();
        assert!(model.show_shell("bad:shell").is_err());
        assert!(model.open_panel(PanelKind::Dashboard).is_err());
        assert_eq!(model.project(), &before);
    }

    #[test]
    fn window_panel_commands_reuse_identity_and_files_find_the_navigation_pane() {
        let mut model = LayoutModel::new(fixture(Layout::Pane(1), &[(1, &["first"])])).unwrap();
        model.open_panel(PanelKind::Projects).unwrap();
        model.split_active(false).unwrap();
        let work = model.project().active_pane;
        assert_ne!(work, 1);
        model.open_panel(PanelKind::Files).unwrap();
        assert_eq!(model.project().active_pane, 1);
        assert_eq!(
            model.project().panes[&1].active_tab_key.as_deref(),
            Some("panel:files")
        );
        model.select_pane(work).unwrap();
        model.open_panel(PanelKind::Files).unwrap();
        assert_eq!(
            model
                .project()
                .panes
                .values()
                .flat_map(|pane| &pane.tabs)
                .filter(|tab| tab.key() == "panel:files")
                .count(),
            1
        );
        model.open_panel(PanelKind::Settings).unwrap();
        model.close_active(false).unwrap();
        assert!(
            model
                .project()
                .panes
                .values()
                .flat_map(|pane| &pane.tabs)
                .any(|tab| tab.key() == "shell:first")
        );
        assert!(
            !model
                .project()
                .panes
                .values()
                .flat_map(|pane| &pane.tabs)
                .any(|tab| tab.key() == "panel:settings")
        );
        model.toggle_lock(1).unwrap();
        let before = model.storage_project();
        assert!(
            model
                .close_active(true)
                .unwrap_err()
                .starts_with("refused:")
        );
        assert_eq!(model.storage_project(), before);
        assert!(model.open_panel(PanelKind::Dashboard).is_err());
    }

    #[test]
    fn default_layout_command_keeps_sessions_selection_and_is_idempotent() {
        let mut model =
            LayoutModel::new(fixture(Layout::Pane(7), &[(7, &["first", "second"])])).unwrap();
        model.open_panel(PanelKind::Settings).unwrap();
        model.select_panel("shell:second").unwrap();
        model.default_layout().unwrap();
        let project = model.storage_project();
        let navigation = project.layout.first_pane();
        let main = project.main_pane.unwrap();
        assert_eq!(main, 7);
        assert_eq!(project.active_pane, main);
        assert_eq!(
            project.panes[&main].active_tab_key.as_deref(),
            Some("shell:second")
        );
        assert_eq!(
            project.panes[&main]
                .tabs
                .iter()
                .map(SavedTab::key)
                .collect::<Vec<_>>(),
            ["shell:first", "shell:second", "panel:settings"]
        );
        assert_eq!(
            project.panes[&navigation]
                .tabs
                .iter()
                .map(SavedTab::key)
                .collect::<Vec<_>>(),
            [
                "panel:projects",
                "panel:files",
                "panel:worktrees",
                "panel:tasks",
                "panel:shells"
            ]
        );
        assert_eq!(
            project.effective_locked_panes(),
            HashSet::from([navigation])
        );
        model.default_layout().unwrap();
        assert_eq!(model.storage_project(), project);
    }

    fn split(axis: Axis, ratio: f32, first: Layout, second: Layout) -> Layout {
        Layout::Split {
            axis,
            ratio,
            first: Box::new(first),
            second: Box::new(second),
        }
    }

    fn from_native(node: &NativeNode) -> DockNode {
        match node {
            NativeNode::Tabs {
                id,
                panels,
                active_index,
            } => DockNode::Tabs {
                id: id.map(NodeId::as_u64),
                panels: panels.iter().map(|p| p.as_str().to_owned()).collect(),
                active_index: *active_index,
            },
            NativeNode::Split {
                id,
                axis,
                children,
                sizes,
            } => DockNode::Split {
                id: id.map(NodeId::as_u64),
                axis: match axis {
                    NativeAxis::Horizontal => DockAxis::Horizontal,
                    NativeAxis::Vertical => DockAxis::Vertical,
                },
                children: children.iter().map(from_native).collect(),
                sizes: sizes.iter().map(|s| s.map(|v| v.0)).collect(),
            },
        }
    }

    fn to_native(node: &DockNode, known: &BTreeMap<PaneId, NodeId>) -> NativeNode {
        match node {
            DockNode::Tabs {
                id,
                panels,
                active_index,
            } => NativeNode::Tabs {
                id: id.and_then(|id| known.get(&id).copied()),
                panels: panels.iter().cloned().map(Into::into).collect(),
                active_index: *active_index,
            },
            DockNode::Split {
                axis,
                children,
                sizes,
                ..
            } => NativeNode::Split {
                id: None,
                axis: match axis {
                    DockAxis::Horizontal => NativeAxis::Horizontal,
                    DockAxis::Vertical => NativeAxis::Vertical,
                },
                children: children
                    .iter()
                    .map(|child| to_native(child, known))
                    .collect(),
                sizes: sizes.iter().map(|s| s.map(px)).collect(),
            },
        }
    }

    /// Use the real NSH 0.3 PaneTree normalizer and gesture implementation,
    /// headlessly. No desktop window, runtime home, terminal, or app is opened.
    fn native_fixture(layout: DockLayout) -> (LayoutModel, PaneTree) {
        let tree = PaneTree::from_layout(layout, RootKind::Split);
        let node = canonical(&from_native(&tree.to_node())).unwrap();
        let layout = rebuild(&node, DEFAULT_EXTENT).unwrap();
        let mut project = fixture(layout, &[]);
        for (id, group) in groups(&node).unwrap() {
            project.panes.insert(
                id,
                SavedPane {
                    tabs: group
                        .panels
                        .iter()
                        .map(|key| SavedTab::Shell {
                            shell_id: key.strip_prefix("shell:").unwrap().into(),
                        })
                        .collect(),
                    active_tab_key: active_real(&group).map(str::to_owned),
                    ..Default::default()
                },
            );
        }
        let mut model = LayoutModel::new(project).unwrap();
        let known = tree
            .node_ids()
            .into_iter()
            .map(|id| (id.as_u64(), id))
            .collect();
        let projected = model.dock(DEFAULT_EXTENT);
        let tree = PaneTree::from_node(&to_native(&projected.center, &known), RootKind::Split);
        (model, tree)
    }

    fn two_native() -> (LayoutModel, PaneTree) {
        native_fixture(
            DockLayout::h_split()
                .child(
                    DockLayout::tabs().panel("shell:a").panel("shell:b"),
                    Some(px(400.0)),
                )
                .child(DockLayout::tabs().panel("shell:c"), None),
        )
    }

    fn report(model: &mut LayoutModel, tree: &PaneTree) -> DockAreaLayout {
        model
            .apply_dock(
                DockAreaLayout::new(from_native(&tree.to_node())),
                DEFAULT_EXTENT,
            )
            .unwrap()
    }

    #[test]
    fn unchanged_projection_keeps_binary_shape_and_every_project_field_at_three_extents() {
        let tree = split(
            Axis::SideBySide,
            0.42,
            split(Axis::SideBySide, 0.37, Layout::Pane(1), Layout::Pane(2)),
            split(Axis::Stacked, 0.06, Layout::Pane(3), Layout::Pane(4)),
        );
        let mut project = fixture(
            tree,
            &[(1, &["a", "b"]), (2, &[]), (3, &["c"]), (4, &["d"])],
        );
        project.main_pane = Some(2);
        project.locked_panes = Some(HashSet::from([3]));
        project.panes.get_mut(&4).unwrap().tabs.extend([
            SavedTab::Panel {
                panel: PanelKind::Files,
            },
            SavedTab::RemoteShell {
                desktop_id: "other-mac".into(),
                shell_id: "remote-shell".into(),
            },
            SavedTab::Chat {
                chat_id: "chat-a".into(),
            },
        ]);
        project.normalize().unwrap();
        for extent in [
            DEFAULT_EXTENT,
            Extent {
                width: 1920.0,
                height: 1080.0,
            },
            Extent {
                width: 640.0,
                height: 400.0,
            },
        ] {
            let mut model = LayoutModel::new(project.clone()).unwrap();
            let projected = model.dock(extent);
            let extents = dock_extents(&projected.center, extent).unwrap();
            for (id, expected) in project.layout.pane_extents(extent) {
                assert!((extents[&id].width - expected.width).abs() < 0.001);
                assert!((extents[&id].height - expected.height).abs() < 0.001);
            }
            model.apply_dock(projected, extent).unwrap();
            assert_eq!(model.project(), &project);
        }
    }

    #[test]
    fn real_nsh_normalization_and_assigned_split_ids_do_not_rewrite_the_core_tree() {
        let (mut model, tree) = two_native();
        let before = model.project().clone();
        report(&mut model, &tree);
        assert_eq!(model.project(), &before);
        let (mut model, tree) = native_fixture(DockLayout::tabs().panel("shell:single"));
        let before = model.project().clone();
        report(&mut model, &tree);
        assert_eq!(model.project(), &before);
    }

    #[test]
    fn native_active_index_selects_tab_and_pane_only() {
        let (mut model, mut tree) = two_native();
        let pane = tree.find_panel_node("shell:a").unwrap();
        let before_tree = model.project().layout.clone();
        assert!(tree.set_active(pane, 1).changed());
        report(&mut model, &tree);
        assert_eq!(model.project().active_pane, pane.as_u64());
        assert_eq!(
            model.project().panes[&pane.as_u64()]
                .active_tab_key
                .as_deref(),
            Some("shell:b")
        );
        assert_eq!(model.project().layout, before_tree);
    }

    #[test]
    fn native_divider_resize_replays_core_ratio_clamp() {
        let (mut model, mut tree) = two_native();
        let before_panes = model.project().panes.clone();
        assert!(
            tree.set_sizes(tree.root().id(), vec![Some(px(1.0)), Some(px(1274.0))])
                .changed()
        );
        report(&mut model, &tree);
        assert_eq!(model.project().layout.ratio_at(&[]), Some(0.1));
        assert_eq!(model.project().panes, before_panes);
    }

    #[test]
    fn native_same_group_reorder_preserves_identity_and_changes_selection() {
        let (mut model, mut tree) = two_native();
        let pane = tree.find_panel_node("shell:a").unwrap();
        assert!(
            tree.move_panel(
                "shell:b",
                InsertTarget::Tabs {
                    node: pane,
                    ix: Some(0),
                    activate: true
                }
            )
            .changed()
        );
        report(&mut model, &tree);
        let saved = &model.project().panes[&pane.as_u64()];
        assert_eq!(
            saved.tabs.iter().map(SavedTab::key).collect::<Vec<_>>(),
            ["shell:b", "shell:a"]
        );
        assert_eq!(saved.shell_ids, ["b", "a"]);
        assert_eq!(saved.active_tab_key.as_deref(), Some("shell:b"));
    }

    #[test]
    fn native_move_to_existing_group_keeps_source_selection_and_selects_target() {
        let (mut model, mut tree) = two_native();
        let source = tree.find_panel_node("shell:a").unwrap();
        let target = tree.find_panel_node("shell:c").unwrap();
        tree.set_active(source, 1);
        report(&mut model, &tree);
        assert!(
            tree.move_panel(
                "shell:a",
                InsertTarget::Tabs {
                    node: target,
                    ix: Some(0),
                    activate: true
                }
            )
            .changed()
        );
        report(&mut model, &tree);
        assert_eq!(
            model.project().panes[&source.as_u64()]
                .active_tab_key
                .as_deref(),
            Some("shell:b")
        );
        assert_eq!(
            model.project().panes[&target.as_u64()]
                .tabs
                .iter()
                .map(SavedTab::key)
                .collect::<Vec<_>>(),
            ["shell:a", "shell:c"]
        );
        assert_eq!(model.project().active_pane, target.as_u64());
        assert!(model.diagnostic().is_none());
    }

    #[test]
    fn native_edge_move_splits_through_core_in_all_four_directions() {
        for placement in [
            Placement::Left,
            Placement::Right,
            Placement::Top,
            Placement::Bottom,
        ] {
            let (mut model, mut tree) = two_native();
            let source = tree.find_panel_node("shell:a").unwrap();
            let target = tree.find_panel_node("shell:c").unwrap();
            assert!(
                tree.move_panel(
                    "shell:a",
                    InsertTarget::Split {
                        node: target,
                        placement,
                        size: None
                    }
                )
                .changed()
            );
            let new = tree.find_panel_node("shell:a").unwrap().as_u64();
            report(&mut model, &tree);
            assert_eq!(model.project().active_pane, new);
            assert_eq!(model.project().layout.pane_ids().len(), 3);
            assert_eq!(
                model.project().panes[&source.as_u64()].tabs[0].key(),
                "shell:b"
            );
            assert!(model.diagnostic().is_none());
            assert_eq!(model.project().panes[&new].tabs[0].key(), "shell:a");
        }
    }

    #[test]
    fn native_last_panel_move_removes_ordinary_source_but_restores_main_sentinel() {
        for keep_main in [false, true] {
            let (mut model, mut tree) = two_native();
            let source = tree.find_panel_node("shell:c").unwrap();
            let target = tree.find_panel_node("shell:a").unwrap();
            if keep_main {
                model.toggle_main(source.as_u64()).unwrap();
            }
            tree.move_panel(
                "shell:c",
                InsertTarget::Tabs {
                    node: target,
                    ix: None,
                    activate: true,
                },
            );
            let projected = report(&mut model, &tree);
            assert_eq!(
                model.project().panes.contains_key(&source.as_u64()),
                keep_main
            );
            if keep_main {
                assert!(model.project().panes[&source.as_u64()].tabs.is_empty());
                assert_eq!(
                    groups(&projected.center).unwrap()[&source.as_u64()].panels,
                    [format!("empty:{}", source.as_u64())]
                );
            }
        }
    }

    #[test]
    fn native_last_panel_move_keeps_empty_scaffold_beside_locked_pane() {
        let (mut model, mut tree) = two_native();
        let source = tree.find_panel_node("shell:c").unwrap();
        let target = tree.find_panel_node("shell:a").unwrap();
        model.toggle_lock(target.as_u64()).unwrap();
        let before_tree = model.project().layout.clone();
        tree.move_panel(
            "shell:c",
            InsertTarget::Tabs {
                node: target,
                ix: None,
                activate: true,
            },
        );
        report(&mut model, &tree);
        assert_eq!(model.project().layout, before_tree);
        assert!(model.project().panes[&source.as_u64()].tabs.is_empty());
    }

    #[test]
    fn unknown_tabs_pin_a_sentinel_even_while_known_tabs_move_out() {
        let (mut model, tree) = two_native();
        let source = tree.find_panel_node("shell:c").unwrap();
        let target = tree.find_panel_node("shell:a").unwrap();
        model.mark_unknown_panes(&BTreeSet::from([source.as_u64()]));
        let known = tree
            .node_ids()
            .into_iter()
            .map(|id| (id.as_u64(), id))
            .collect();
        let before = model.dock(DEFAULT_EXTENT);
        assert_eq!(
            groups(&before.center).unwrap()[&source.as_u64()]
                .panels
                .len(),
            2
        );
        let mut tree = PaneTree::from_node(&to_native(&before.center, &known), RootKind::Split);
        tree.move_panel(
            "shell:c",
            InsertTarget::Tabs {
                node: target,
                ix: None,
                activate: true,
            },
        );
        let after = report(&mut model, &tree);
        assert_eq!(
            groups(&after.center).unwrap()[&source.as_u64()].panels,
            [format!("empty:{}", source.as_u64())]
        );
        assert!(model.project().panes[&source.as_u64()].tabs.is_empty());
    }

    #[test]
    fn dropping_real_tab_into_an_empty_pane_never_persists_the_sentinel() {
        let mut model = LayoutModel::new(fixture(
            split(Axis::SideBySide, 0.5, Layout::Pane(1), Layout::Pane(2)),
            &[(1, &["a"]), (2, &[])],
        ))
        .unwrap();
        model.project.main_pane = Some(1);
        let mut next = model.dock(DEFAULT_EXTENT);
        if let DockNode::Split { children, .. } = &mut next.center {
            children[0] = DockNode::Tabs {
                id: Some(1),
                panels: vec!["empty:1".into()],
                active_index: 0,
            };
            children[1] = DockNode::Tabs {
                id: Some(2),
                panels: vec!["empty:2".into(), "shell:a".into()],
                active_index: 1,
            };
        }
        let next = model.apply_dock(next, DEFAULT_EXTENT).unwrap();
        assert_eq!(groups(&next.center).unwrap()[&2].panels, ["shell:a"]);
        assert_eq!(model.project().panes[&2].tabs[0].key(), "shell:a");
    }

    #[test]
    fn compound_native_edit_uses_logged_right_nested_fallback_without_data_loss() {
        let (mut model, mut tree) = two_native();
        let first = tree.find_panel_node("shell:a").unwrap();
        let second = tree.find_panel_node("shell:c").unwrap();
        tree.move_panel(
            "shell:a",
            InsertTarget::Tabs {
                node: second,
                ix: None,
                activate: true,
            },
        );
        tree.move_panel(
            "shell:c",
            InsertTarget::Tabs {
                node: first,
                ix: None,
                activate: true,
            },
        );
        report(&mut model, &tree);
        assert!(model.diagnostic().unwrap().starts_with("layout.fallback"));
        assert_eq!(
            model
                .project()
                .panes
                .values()
                .map(|p| p.tabs.len())
                .sum::<usize>(),
            3
        );
        assert_eq!(
            model.project().panes[&first.as_u64()]
                .tabs
                .iter()
                .map(SavedTab::key)
                .collect::<Vec<_>>(),
            ["shell:b", "shell:c"]
        );
    }

    #[test]
    fn edge_dock_fallback_moves_back_into_center_only() {
        let (mut model, tree) = two_native();
        let source = tree.find_panel_node("shell:c").unwrap().as_u64();
        let old = model.dock(DEFAULT_EXTENT);
        let mut next = old.clone();
        if let DockNode::Split { children, .. } = next.center {
            next.center = children[0].clone();
        }
        next.bottom = Some(DockRegion {
            layout: DockNode::Tabs {
                id: Some(source),
                panels: vec!["shell:c".into()],
                active_index: 0,
            },
            size: Some(240.0),
            open: Some(true),
            collapsible: None,
        });
        let projected = model.apply_dock(next, DEFAULT_EXTENT).unwrap();
        assert!(projected.bottom.is_none());
        assert!(matches!(
            model.project().layout,
            Layout::Split {
                axis: Axis::Stacked,
                ..
            }
        ));
        assert!(model.diagnostic().is_some());
    }

    #[test]
    fn resizing_uses_core_locked_minima_and_returns_to_the_original_geometry() {
        let mut project = fixture(
            split(
                Axis::SideBySide,
                0.27,
                Layout::Pane(1),
                split(Axis::Stacked, 0.7, Layout::Pane(2), Layout::Pane(3)),
            ),
            &[(1, &["a"]), (2, &["b"]), (3, &["c"])],
        );
        project.locked_panes = Some(HashSet::from([1, 3]));
        let mut model = LayoutModel::new(project.clone()).unwrap();
        model.dock(DEFAULT_EXTENT);
        let next = Extent {
            width: 1920.0,
            height: 1080.0,
        };
        project.layout.preserve_locked(
            DEFAULT_EXTENT,
            next,
            &|id| id == 1 || id == 3,
            MIN_PANE_EXTENT,
        );
        model.set_extent(next).unwrap();
        assert_eq!(model.project().layout, project.layout);
        model.set_extent(DEFAULT_EXTENT).unwrap();
        let sizes = model.project().layout.pane_extents(DEFAULT_EXTENT);
        assert!((sizes[&1].width - 1275.0 * 0.27).abs() < 0.001);
        assert!(
            model
                .set_extent(Extent {
                    width: f32::NAN,
                    height: 1.0
                })
                .is_err()
        );
    }

    #[test]
    fn unsafe_ids_remap_all_references_and_supply_opaque_tab_persistence_mapping() {
        let unsafe_id = MAX_SAFE_PANE_ID + 10;
        let mut project = fixture(
            split(
                Axis::SideBySide,
                0.5,
                Layout::Pane(1),
                Layout::Pane(unsafe_id),
            ),
            &[(1, &["a"]), (unsafe_id, &["b"])],
        );
        project.active_pane = unsafe_id;
        project.main_pane = Some(unsafe_id);
        project.locked_panes = Some(HashSet::from([unsafe_id]));
        let mut model = LayoutModel::new(project.clone()).unwrap();
        model.mark_unknown_panes(&BTreeSet::from([unsafe_id]));
        let projected = model.dock(DEFAULT_EXTENT);
        assert_eq!(model.pane_id_remap()[&unsafe_id], 2);
        assert_eq!(model.project().active_pane, 2);
        assert_eq!(model.project().main_pane, Some(2));
        assert_eq!(model.project().effective_locked_panes(), HashSet::from([2]));
        assert!(
            groups(&projected.center).unwrap()[&2]
                .panels
                .contains(&"empty:2".into())
        );
        assert_eq!(model.storage_project(), *model.project());
        assert_eq!(model.storage_id_remap(), BTreeMap::from([(unsafe_id, 2)]));
    }

    #[test]
    fn invalid_or_destructive_reports_are_transactional() {
        let (mut model, tree) = two_native();
        let before = model.project().clone();
        let mut deleted = tree.clone();
        deleted.remove_panel("shell:a");
        assert!(
            model
                .apply_dock(
                    DockAreaLayout::new(from_native(&deleted.to_node())),
                    DEFAULT_EXTENT
                )
                .is_err()
        );
        assert_eq!(model.project(), &before);
        let mut next = model.dock(DEFAULT_EXTENT);
        if let DockNode::Split { sizes, .. } = &mut next.center {
            sizes[0] = Some(f32::NAN);
        }
        assert!(model.apply_dock(next, DEFAULT_EXTENT).is_err());
        assert_eq!(model.project(), &before);
    }

    #[test]
    fn implicit_navigation_lock_and_explicit_unlocked_state_survive_no_op() {
        for locked in [None, Some(HashSet::new())] {
            let mut project = fixture(Layout::navigation_beside(1, Layout::Pane(2)), &[]);
            project.locked_panes = locked;
            project.panes.get_mut(&1).unwrap().tabs = vec![SavedTab::Panel {
                panel: PanelKind::Projects,
            }];
            project.panes.get_mut(&1).unwrap().tab_edge = TabEdge::Bottom;
            project.normalize().unwrap();
            let mut model = LayoutModel::new(project.clone()).unwrap();
            let projected = model.dock(DEFAULT_EXTENT);
            model.apply_dock(projected, DEFAULT_EXTENT).unwrap();
            assert_eq!(model.project(), &project);
        }
    }

    #[test]
    fn flattened_resize_reconstructs_binary_ratios_including_inner_divider() {
        let tree = split(
            Axis::SideBySide,
            0.4,
            split(Axis::SideBySide, 0.3, Layout::Pane(1), Layout::Pane(2)),
            Layout::Pane(3),
        );
        let mut model =
            LayoutModel::new(fixture(tree, &[(1, &["a"]), (2, &["b"]), (3, &["c"])])).unwrap();
        let mut next = model.dock(DEFAULT_EXTENT);
        if let DockNode::Split { sizes, .. } = &mut next.center {
            *sizes = vec![Some(200.0), Some(300.0), Some(770.0)];
        }
        model.apply_dock(next, DEFAULT_EXTENT).unwrap();
        let sizes = model.project().layout.pane_extents(DEFAULT_EXTENT);
        for (id, width) in [(1, 200.0), (2, 300.0), (3, 770.0)] {
            assert!((sizes[&id].width - width).abs() < 0.001);
        }
        assert!((model.project().layout.ratio_at(&[]).unwrap() - 505.0 / 1275.0).abs() < 0.00001);
    }

    #[test]
    fn ordinary_unsafe_ids_persist_the_safe_remap() {
        let unsafe_id = MAX_SAFE_PANE_ID + 11;
        let project = fixture(Layout::Pane(unsafe_id), &[(unsafe_id, &["a"])]);
        let model = LayoutModel::new(project).unwrap();
        assert_eq!(model.storage_project().layout, Layout::Pane(1));
        assert_eq!(model.storage_project(), *model.project());
    }

    #[test]
    fn max_safe_persisted_id_uses_session_alias_without_poisoning_native_allocator() {
        let high_id = MAX_SAFE_PANE_ID;
        let mut project = fixture(
            split(
                Axis::SideBySide,
                0.27,
                Layout::Pane(1),
                Layout::Pane(high_id),
            ),
            &[(1, &["a"]), (high_id, &["b"])],
        );
        project.main_pane = Some(high_id);
        project.active_pane = high_id;
        project.locked_panes = Some(HashSet::from([high_id]));
        let mut model = LayoutModel::new(project.clone()).unwrap();
        let projected = model.dock(DEFAULT_EXTENT);
        assert_eq!(model.pane_id_remap()[&high_id], 2);
        assert!(
            groups(&projected.center)
                .unwrap()
                .keys()
                .all(|&id| id <= MAX_INITIAL_DOCK_PANE_ID)
        );
        assert_eq!(model.project().active_pane, 2);
        assert_eq!(model.project().main_pane, Some(2));
        assert_eq!(model.project().effective_locked_panes(), HashSet::from([2]));
        model.apply_dock(projected, DEFAULT_EXTENT).unwrap();
        assert_eq!(model.storage_project(), project);
        // Do not feed the original high ID to a real PaneTree even in a test:
        // its allocator is shared with every other NSH test in this process.
    }

    #[test]
    fn headroom_aliases_leave_free_ids_for_new_groups_and_opaque_sentinels() {
        let high_id = MAX_INITIAL_DOCK_PANE_ID + 1;
        let project = fixture(
            split(
                Axis::SideBySide,
                0.5,
                Layout::Pane(1),
                Layout::Pane(high_id),
            ),
            &[(1, &["a", "b"]), (high_id, &["c"])],
        );
        let mut model = LayoutModel::new(project).unwrap();
        model.mark_unknown_panes(&BTreeSet::from([high_id]));
        let mut next = model.dock(DEFAULT_EXTENT);
        assert!(
            groups(&next.center).unwrap()[&2]
                .panels
                .contains(&"empty:2".into())
        );
        if let DockNode::Split {
            children, sizes, ..
        } = &mut next.center
        {
            children[0] = DockNode::Tabs {
                id: Some(1),
                panels: vec!["shell:b".into()],
                active_index: 0,
            };
            children.insert(
                1,
                DockNode::Tabs {
                    id: Some(3),
                    panels: vec!["shell:a".into()],
                    active_index: 0,
                },
            );
            *sizes = vec![Some(317.5), Some(317.5), Some(635.0)];
        }
        model.apply_dock(next, DEFAULT_EXTENT).unwrap();
        let stored = model.storage_project();
        assert!(stored.panes.contains_key(&high_id));
        assert!(stored.panes.contains_key(&3));
        assert!(!stored.panes.contains_key(&2));
        assert_eq!(stored.panes[&3].tabs[0].key(), "shell:a");
        let reloaded = LayoutModel::new(stored).unwrap();
        assert_eq!(reloaded.pane_id_remap()[&high_id], 2);
        assert!(reloaded.project().panes.contains_key(&3));
    }

    #[test]
    fn native_id_matching_an_aliased_disk_id_cannot_overwrite_either_pane_on_save() {
        let native_ceiling = MAX_INITIAL_DOCK_PANE_ID;
        let original_high = native_ceiling + 3;
        let project = fixture(
            split(
                Axis::SideBySide,
                0.5,
                Layout::Pane(native_ceiling),
                Layout::Pane(original_high),
            ),
            &[(native_ceiling, &["a", "b"]), (original_high, &["c"])],
        );
        let mut model = LayoutModel::new(project).unwrap();
        assert_eq!(model.pane_id_remap()[&original_high], 1);
        let mut next = model.dock(DEFAULT_EXTENT);
        if let DockNode::Split {
            children, sizes, ..
        } = &mut next.center
        {
            children[0] = DockNode::Tabs {
                id: Some(native_ceiling),
                panels: vec!["shell:b".into()],
                active_index: 0,
            };
            // The native allocator can reach a disk ID hidden behind an alias.
            children.insert(
                1,
                DockNode::Tabs {
                    id: Some(original_high),
                    panels: vec!["shell:a".into()],
                    active_index: 0,
                },
            );
            *sizes = vec![Some(317.5), Some(317.5), Some(635.0)];
        }
        let projected = model.apply_dock(next, DEFAULT_EXTENT).unwrap();
        assert!(
            groups(&projected.center)
                .unwrap()
                .keys()
                .all(|&id| id <= native_ceiling)
        );
        assert_eq!(model.project().active_pane, 2);
        let saved = model.storage_project();
        assert_eq!(saved.panes.len(), 3);
        assert_eq!(saved.panes[&original_high].tabs[0].key(), "shell:c");
        assert_eq!(saved.panes[&2].tabs[0].key(), "shell:a");
        assert_eq!(saved.panes[&native_ceiling].tabs[0].key(), "shell:b");
    }

    #[test]
    fn local_shell_detach_reopen_preserves_identity_and_does_not_resurrect_legacy_tab() {
        let mut model =
            LayoutModel::new(fixture(Layout::Pane(1), &[(1, &["first", "second"])])).unwrap();
        model.select_panel("shell:first").unwrap();
        model.detach_shell("first").unwrap();
        assert!(model.project().detached_shell_ids.contains("first"));
        assert_eq!(
            model.project().panes[&1].active_tab_key.as_deref(),
            Some("shell:second")
        );
        model.detach_shell("second").unwrap();
        assert!(model.project().panes[&1].tabs.is_empty());
        assert!(model.project().panes[&1].shell_ids.is_empty());
        let stored = model.storage_project();
        let mut restored = LayoutModel::new(stored).unwrap();
        restored.open_shell("first").unwrap();
        restored.open_shell("first").unwrap();
        assert_eq!(restored.project().panes[&1].tabs.len(), 1);
        assert_eq!(restored.project().panes[&1].tabs[0].key(), "shell:first");
        assert!(!restored.project().detached_shell_ids.contains("first"));
        assert!(restored.project().detached_shell_ids.contains("second"));
    }

    #[test]
    fn explicit_reopen_remembers_surviving_unlocked_pane_and_falls_back_if_locked_or_removed() {
        let mut project = fixture(
            split(Axis::SideBySide, 0.5, Layout::Pane(1), Layout::Pane(2)),
            &[(1, &["main"]), (2, &["other"])],
        );
        project.main_pane = Some(1);
        project.locked_panes = Some(HashSet::new());
        let mut model = LayoutModel::new(project.clone()).unwrap();
        model.detach_shell("other").unwrap();
        model.open_shell("other").unwrap();
        assert_eq!(model.project().panes[&2].tabs[0].key(), "shell:other");
        model.detach_shell("other").unwrap();
        model.toggle_lock(2).unwrap();
        model.open_shell("other").unwrap();
        assert_eq!(
            model.project().panes[&1].tabs.last().unwrap().key(),
            "shell:other"
        );
        let mut model = LayoutModel::new(project).unwrap();
        model.detach_shell("other").unwrap();
        // A real controlled DockArea gesture removes the emptied former pane.
        model
            .apply_dock(
                DockAreaLayout::new(DockNode::Tabs {
                    id: Some(1),
                    panels: vec!["shell:main".into()],
                    active_index: 0,
                }),
                DEFAULT_EXTENT,
            )
            .unwrap();
        assert!(!model.project().panes.contains_key(&2));
        model.open_shell("other").unwrap();
        assert_eq!(
            model.project().panes[&1].tabs.last().unwrap().key(),
            "shell:other"
        );
    }

    #[test]
    fn opening_local_shell_uses_unlocked_main_and_refuses_all_locked_transactionally() {
        let mut project = fixture(
            split(Axis::SideBySide, 0.27, Layout::Pane(1), Layout::Pane(2)),
            &[],
        );
        project.locked_panes = Some(HashSet::from([1]));
        project.main_pane = Some(2);
        project.active_pane = 1;
        let mut model = LayoutModel::new(project).unwrap();
        model.open_shell("existing").unwrap();
        assert_eq!(model.project().active_pane, 2);
        assert_eq!(model.project().panes[&2].tabs[0].key(), "shell:existing");
        model.toggle_lock(2).unwrap();
        let before = model.project().clone();
        assert!(
            model
                .open_shell("another")
                .unwrap_err()
                .starts_with("refused:")
        );
        assert_eq!(model.project(), &before);
        // A lock prevents routing new tabs, not selecting an existing one.
        model.open_shell("existing").unwrap();
    }

    #[test]
    fn panel_shell_selection_preserves_terminal_reopen_and_lock_policy() {
        let mut project = fixture(
            split(Axis::SideBySide, 0.5, Layout::Pane(1), Layout::Pane(2)),
            &[(1, &["main"]), (2, &["other"])],
        );
        project.main_pane = Some(1);
        project.locked_panes = Some(HashSet::new());
        let mut model = LayoutModel::new(project).unwrap();
        model.detach_shell("other").unwrap();
        model.show_shell("other").unwrap();
        assert_eq!(model.project().active_pane, 2);
        model.detach_shell("other").unwrap();
        model.toggle_lock(2).unwrap();
        model.show_shell("other").unwrap();
        assert_eq!(model.project().active_pane, 1);
        model.toggle_lock(1).unwrap();
        let before = model.project().clone();
        assert!(model.show_shell("new").unwrap_err().starts_with("refused:"));
        assert_eq!(model.project(), &before);
        model.show_shell("other").unwrap();
    }

    #[test]
    fn gather_uses_core_sources_keeps_main_selection_and_locked_geometry() {
        let mut project = fixture(
            split(
                Axis::SideBySide,
                0.27,
                Layout::Pane(1),
                split(Axis::Stacked, 0.6, Layout::Pane(2), Layout::Pane(3)),
            ),
            &[(1, &["navigation"]), (2, &["main"]), (3, &["a", "b"])],
        );
        project.locked_panes = Some(HashSet::from([1]));
        project.main_pane = Some(2);
        let mut model = LayoutModel::new(project).unwrap();
        model.dock(DEFAULT_EXTENT);
        let width = model.project().layout.pane_extents(DEFAULT_EXTENT)[&1].width;
        model.gather().unwrap();
        assert_eq!(
            model.project().panes[&2]
                .tabs
                .iter()
                .map(SavedTab::key)
                .collect::<Vec<_>>(),
            ["shell:main", "shell:a", "shell:b"]
        );
        assert_eq!(
            model.project().panes[&2].active_tab_key.as_deref(),
            Some("shell:main")
        );
        assert_eq!(model.project().panes[&1].tabs[0].key(), "shell:navigation");
        assert!(!model.project().panes.contains_key(&3));
        assert_eq!(
            model.project().layout.pane_extents(DEFAULT_EXTENT)[&1].width,
            width
        );
    }

    #[test]
    fn compound_topology_change_is_not_misclassified_as_a_simple_move() {
        let mut model = LayoutModel::new(fixture(
            split(Axis::SideBySide, 0.5, Layout::Pane(1), Layout::Pane(2)),
            &[(1, &["a", "b"]), (2, &["c"])],
        ))
        .unwrap();
        model.dock(DEFAULT_EXTENT);
        let next = DockAreaLayout::new(DockNode::Split {
            id: None,
            axis: DockAxis::Vertical,
            sizes: vec![None, None],
            children: vec![
                DockNode::Tabs {
                    id: Some(1),
                    panels: vec!["shell:b".into()],
                    active_index: 0,
                },
                DockNode::Tabs {
                    id: Some(2),
                    panels: vec!["shell:c".into(), "shell:a".into()],
                    active_index: 1,
                },
            ],
        });
        model.apply_dock(next, DEFAULT_EXTENT).unwrap();
        assert!(model.diagnostic().is_some());
        assert!(matches!(
            model.project().layout,
            Layout::Split {
                axis: Axis::Stacked,
                ..
            }
        ));
    }
}
