//! Durable per-project tabs and split geometry, independent of shell lifetimes.

use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    fs,
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use fs2::FileExt;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;
use uuid::Uuid;

pub type PaneId = u64;

const MAX_DEPTH: usize = 32;
const MAX_PANES: usize = 256;
const MAX_PANE_ID: PaneId = u64::MAX - MAX_PANES as u64;
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    SideBySide,
    Stacked,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    Pane(PaneId),
    Split {
        axis: Axis,
        #[serde(default = "default_ratio")]
        ratio: f32,
        first: Box<Layout>,
        second: Box<Layout>,
    },
}

impl Layout {
    pub fn split(&mut self, target: PaneId, axis: Axis, new_pane: PaneId) -> bool {
        self.split_with(target, axis, new_pane, false)
    }

    pub fn split_with(
        &mut self,
        target: PaneId,
        axis: Axis,
        new_pane: PaneId,
        new_first: bool,
    ) -> bool {
        match self {
            Self::Pane(id) if *id == target => {
                let (first, second) = if new_first {
                    (new_pane, target)
                } else {
                    (target, new_pane)
                };
                *self = Self::Split {
                    axis,
                    ratio: default_ratio(),
                    first: Box::new(Self::Pane(first)),
                    second: Box::new(Self::Pane(second)),
                };
                true
            }
            Self::Pane(_) => false,
            Self::Split { first, second, .. } => {
                first.split_with(target, axis, new_pane, new_first)
                    || second.split_with(target, axis, new_pane, new_first)
            }
        }
    }

    /// A split path uses false for its first child and true for its second.
    pub fn ratio_at(&self, path: &[bool]) -> Option<f32> {
        match self {
            Self::Pane(_) => None,
            Self::Split {
                ratio,
                first,
                second,
                ..
            } => match path.split_first() {
                None => Some(*ratio),
                Some((false, rest)) => first.ratio_at(rest),
                Some((true, rest)) => second.ratio_at(rest),
            },
        }
    }

    pub fn set_ratio(&mut self, path: &[bool], new_ratio: f32) -> bool {
        match self {
            Self::Pane(_) => false,
            Self::Split {
                ratio,
                first,
                second,
                ..
            } => match path.split_first() {
                None => {
                    *ratio = normalized_ratio(new_ratio);
                    true
                }
                Some((false, rest)) => first.set_ratio(rest, new_ratio),
                Some((true, rest)) => second.set_ratio(rest, new_ratio),
            },
        }
    }

    pub fn without(self, target: PaneId) -> Option<Self> {
        match self {
            Self::Pane(id) if id == target => None,
            Self::Pane(_) => Some(self),
            Self::Split {
                axis,
                ratio,
                first,
                second,
            } => match (first.without(target), second.without(target)) {
                (Some(first), Some(second)) => Some(Self::Split {
                    axis,
                    ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (Some(remaining), None) | (None, Some(remaining)) => Some(remaining),
                (None, None) => None,
            },
        }
    }

    pub fn first_pane(&self) -> PaneId {
        match self {
            Self::Pane(id) => *id,
            Self::Split { first, .. } => first.first_pane(),
        }
    }

    pub fn pane_ids(&self) -> Vec<PaneId> {
        let mut ids = Vec::new();
        let mut pending = vec![self];
        while let Some(layout) = pending.pop() {
            match layout {
                Self::Pane(id) => ids.push(*id),
                Self::Split { first, second, .. } => {
                    pending.push(second);
                    pending.push(first);
                }
            }
        }
        ids
    }

    fn validate_size(&self) -> Result<(), String> {
        let mut pending = vec![(self, 0)];
        let mut pane_count = 0;
        while let Some((layout, depth)) = pending.pop() {
            if depth > MAX_DEPTH {
                return Err(format!("Saved split layout exceeds depth {MAX_DEPTH}"));
            }
            match layout {
                Self::Pane(id) => {
                    if *id >= MAX_PANE_ID {
                        return Err(
                            "Saved pane ID is too large to allocate another pane".to_owned()
                        );
                    }
                    pane_count += 1;
                    if pane_count > MAX_PANES {
                        return Err(format!("Saved split layout exceeds {MAX_PANES} panes"));
                    }
                }
                Self::Split { first, second, .. } => {
                    pending.push((second, depth + 1));
                    pending.push((first, depth + 1));
                }
            }
        }
        Ok(())
    }

    fn unique(self, seen: &mut HashSet<PaneId>) -> Option<Self> {
        match self {
            Self::Pane(id) if id == 0 || !seen.insert(id) => None,
            Self::Pane(id) => Some(Self::Pane(id)),
            Self::Split {
                axis,
                ratio,
                first,
                second,
            } => match (first.unique(seen), second.unique(seen)) {
                (Some(first), Some(second)) => Some(Self::Split {
                    axis,
                    ratio: normalized_ratio(ratio),
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (Some(remaining), None) | (None, Some(remaining)) => Some(remaining),
                (None, None) => None,
            },
        }
    }
}

fn default_ratio() -> f32 {
    0.5
}

fn normalized_ratio(ratio: f32) -> f32 {
    if ratio.is_finite() {
        ratio.clamp(0.1, 0.9)
    } else {
        default_ratio()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelKind {
    Projects,
    Worktrees,
    Files,
    Tasks,
    Shells,
    Usage,
    Settings,
    ProjectSettings,
    Schedules,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SavedTab {
    Shell { shell_id: String },
    Panel { panel: PanelKind },
}

impl SavedTab {
    pub fn key(&self) -> String {
        match self {
            Self::Shell { shell_id } => format!("shell:{shell_id}"),
            Self::Panel { panel } => format!(
                "panel:{}",
                match panel {
                    PanelKind::Projects => "projects",
                    PanelKind::Worktrees => "worktrees",
                    PanelKind::Files => "files",
                    PanelKind::Tasks => "tasks",
                    PanelKind::Shells => "shells",
                    PanelKind::Usage => "usage",
                    PanelKind::Settings => "settings",
                    PanelKind::ProjectSettings => "project_settings",
                    PanelKind::Schedules => "schedules",
                }
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TabEdge {
    #[default]
    Top,
    Bottom,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedPane {
    pub tabs: Vec<SavedTab>,
    pub active_tab_key: Option<String>,
    pub tab_edge: TabEdge,
    /// Kept in sync for layouts written by older RiWork builds.
    pub shell_ids: Vec<String>,
    pub active_shell_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectLayout {
    pub layout: Layout,
    #[serde(default)]
    pub panes: BTreeMap<PaneId, SavedPane>,
    pub active_pane: PaneId,
    /// None chooses the left navigation pane automatically; Some(empty) is
    /// the user's explicit choice to carry no regions across project switches.
    #[serde(default)]
    pub locked_panes: Option<HashSet<PaneId>>,
    #[serde(default)]
    pub panels_initialized: bool,
    #[serde(default)]
    pub detached_shell_ids: HashSet<String>,
    #[serde(default)]
    pub selected_worktree_id: Option<String>,
    #[serde(default)]
    pub selected_task_id: Option<String>,
    #[serde(default = "sidebar_visible_default")]
    pub sidebar_visible: bool,
    #[serde(default)]
    pub window_size: Option<WindowSize>,
}

/// The normal window's content size, independent of terminal rows and columns.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowSize {
    pub width: f32,
    pub height: f32,
}

impl WindowSize {
    pub fn new(width: f32, height: f32) -> Option<Self> {
        let size = Self { width, height };
        size.is_valid().then_some(size)
    }

    fn is_valid(&self) -> bool {
        self.width.is_finite()
            && self.height.is_finite()
            && (320.0..=16_384.0).contains(&self.width)
            && (240.0..=16_384.0).contains(&self.height)
    }
}

/// A window frame in display points with a top-left origin.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowFrame {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl WindowFrame {
    /// Where the `cascade`th open window goes: the remembered `size`, fitted to
    /// `display`, centred and shifted down-right by its place in a five-step
    /// cascade. The shift is applied before the final clamp so no step hangs
    /// off the display.
    pub fn opening(size: WindowSize, display: WindowFrame, cascade: usize) -> Self {
        // A project last opened on a larger monitor must still fit this one.
        let width = size.width.min((display.width - 40.0).max(640.0));
        let height = size.height.min((display.height - 40.0).max(400.0));
        let shift = (cascade % 5) as f32 * 22.0;
        let x = display.x + (display.width - width) / 2.0 + shift;
        let y = display.y + (display.height - height) / 2.0 + shift;
        Self {
            x: x.min(display.x + display.width - width).max(display.x),
            y: y.min(display.y + display.height - height).max(display.y),
            width,
            height,
        }
    }
}

fn sidebar_visible_default() -> bool {
    true
}

impl ProjectLayout {
    pub fn effective_locked_panes(&self) -> HashSet<PaneId> {
        let ids = self.layout.pane_ids().into_iter().collect::<HashSet<_>>();
        if let Some(locked) = &self.locked_panes {
            return locked.intersection(&ids).copied().collect();
        }
        let first = self.layout.first_pane();
        self.panes
            .get(&first)
            .filter(|pane| {
                pane.tabs.iter().any(|tab| {
                    matches!(
                        tab,
                        SavedTab::Panel {
                            panel: PanelKind::Projects | PanelKind::Worktrees | PanelKind::Files
                        }
                    )
                })
            })
            .map(|_| HashSet::from([first]))
            .unwrap_or_default()
    }

    /// Keep the previous window's locked regions while loading this project's
    /// unlocked regions. This changes only saved geometry and tab metadata.
    pub fn carry_locked_regions_from(
        &self,
        previous: &ProjectLayout,
    ) -> Result<ProjectLayout, String> {
        let mut destination = self.clone();
        destination.normalize()?;
        let mut previous = previous.clone();
        previous.normalize()?;
        let locked = previous.effective_locked_panes();
        if locked.is_empty() {
            destination.locked_panes = previous.locked_panes;
            destination.normalize()?;
            return Ok(destination);
        }

        let mut slots = 0;
        let scaffold = LockedScaffold::new(&previous.layout, &locked, &mut slots);
        let mut panes = previous
            .panes
            .iter()
            .filter(|(id, _)| locked.contains(id))
            .map(|(id, pane)| (*id, pane.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut used_ids = locked.clone();
        let mut used_keys = panes
            .values()
            .flat_map(|pane| &pane.tabs)
            .map(SavedTab::key)
            .collect::<HashSet<_>>();
        let destination_active_key = destination
            .panes
            .get(&destination.active_pane)
            .and_then(|pane| pane.active_tab_key.clone());
        let destination_shell_ids = destination
            .panes
            .values()
            .flat_map(|pane| {
                pane.tabs.iter().filter_map(|tab| match tab {
                    SavedTab::Shell { shell_id } => Some(shell_id.clone()),
                    SavedTab::Panel { .. } => None,
                })
            })
            .collect::<HashSet<_>>();

        // The destination's own navigation/locked regions are replaced by the
        // carried ones. Recover their unique tabs rather than losing sessions.
        let destination_locked = destination.effective_locked_panes();
        let mut recovered = Vec::new();
        let mut excluded = destination_locked.clone();
        for id in destination.layout.pane_ids() {
            let pane = destination
                .panes
                .get_mut(&id)
                .expect("normalized pane exists");
            let had_tabs = !pane.tabs.is_empty();
            pane.tabs.retain(|tab| used_keys.insert(tab.key()));
            if destination_locked.contains(&id) {
                recovered.append(&mut pane.tabs);
            } else if had_tabs && pane.tabs.is_empty() {
                excluded.insert(id);
            }
            sync_saved_pane(pane);
        }
        let mut content = without_panes(destination.layout.clone(), &excluded);
        if !recovered.is_empty() {
            let id = content
                .as_ref()
                .map(Layout::first_pane)
                .unwrap_or(destination.layout.first_pane());
            let pane = destination.panes.entry(id).or_default();
            pane.tabs.extend(recovered);
            if destination_active_key
                .as_ref()
                .is_some_and(|key| pane.tabs.iter().any(|tab| tab.key() == *key))
            {
                pane.active_tab_key = destination_active_key.clone();
            }
            sync_saved_pane(pane);
            if content.is_none() {
                content = Some(Layout::Pane(id));
            }
        }

        let mut fragments = content
            .map(|layout| partition_layout(layout, slots.max(1)))
            .unwrap_or_default();
        let mut mapping = BTreeMap::new();
        for fragment in &mut fragments {
            remap_layout(fragment, &mut used_ids, &mut mapping)?;
        }
        for (source, target) in &mapping {
            panes.insert(*target, destination.panes[source].clone());
        }
        let layout = scaffold.fill(&mut fragments.into(), &mut panes, &mut used_ids)?;

        let active_pane = if slots == 0 {
            previous.active_pane
        } else {
            // A removed destination navigation pane may have selected a tab
            // retained by a locked region, or recovered in a different pane.
            destination_active_key
                .as_ref()
                .and_then(|key| {
                    panes
                        .iter()
                        .find(|(_, pane)| pane.active_tab_key.as_ref() == Some(key))
                        .map(|(id, _)| *id)
                })
                .or_else(|| mapping.get(&destination.active_pane).copied())
                .or_else(|| {
                    layout
                        .pane_ids()
                        .into_iter()
                        .find(|id| !locked.contains(id))
                })
                .unwrap_or(layout.first_pane())
        };
        let mut result = destination;
        result.layout = layout;
        result.panes = panes;
        result.active_pane = active_pane;
        result.locked_panes = previous.locked_panes;
        result.panels_initialized = result.panels_initialized || previous.panels_initialized;
        result.sidebar_visible = previous.sidebar_visible;
        result.window_size = previous.window_size;
        // If every region was locked, destination sessions remain recoverable
        // through the shell browser even though no unlocked slot is available.
        if slots == 0 {
            result.detached_shell_ids.extend(destination_shell_ids);
        }
        result.normalize()?;
        Ok(result)
    }

    /// Repair recoverable geometry and selection errors without touching sessions.
    pub fn normalize(&mut self) -> Result<(), String> {
        self.layout.validate_size()?;
        self.window_size = self.window_size.filter(WindowSize::is_valid);
        let mut pane_ids = HashSet::new();
        let layout = std::mem::replace(&mut self.layout, Layout::Pane(1));
        self.layout = layout.unique(&mut pane_ids).unwrap_or(Layout::Pane(1));
        let ordered_pane_ids = self.layout.pane_ids();
        pane_ids = ordered_pane_ids.iter().copied().collect();
        self.panes.retain(|id, _| pane_ids.contains(id));
        if let Some(locked) = &mut self.locked_panes {
            locked.retain(|id| pane_ids.contains(id));
        }

        let mut shell_ids = HashSet::new();
        let mut panel_kinds = HashSet::new();
        for id in ordered_pane_ids {
            let pane = self.panes.entry(id).or_default();
            // Older layouts may have bottom strips; all current strips belong at the top.
            pane.tab_edge = TabEdge::Top;
            if pane.tabs.is_empty() && !pane.shell_ids.is_empty() {
                pane.tabs = pane
                    .shell_ids
                    .iter()
                    .map(|shell_id| SavedTab::Shell {
                        shell_id: shell_id.clone(),
                    })
                    .collect();
                if pane.active_tab_key.is_none() {
                    pane.active_tab_key = pane
                        .active_shell_id
                        .as_ref()
                        .map(|id| format!("shell:{id}"));
                }
            }
            pane.tabs.retain(|tab| match tab {
                SavedTab::Shell { shell_id } => {
                    !shell_id.is_empty() && shell_ids.insert(shell_id.clone())
                }
                SavedTab::Panel { panel } => panel_kinds.insert(*panel),
            });
            if !pane
                .active_tab_key
                .as_ref()
                .is_some_and(|key| pane.tabs.iter().any(|tab| tab.key() == *key))
            {
                pane.active_tab_key = pane.tabs.first().map(SavedTab::key);
            }
            pane.shell_ids = pane
                .tabs
                .iter()
                .filter_map(|tab| match tab {
                    SavedTab::Shell { shell_id } => Some(shell_id.clone()),
                    SavedTab::Panel { .. } => None,
                })
                .collect();
            pane.active_shell_id = pane.tabs.iter().find_map(|tab| match tab {
                SavedTab::Shell { shell_id }
                    if pane.active_tab_key.as_deref() == Some(tab.key().as_str()) =>
                {
                    Some(shell_id.clone())
                }
                _ => None,
            });
        }
        if !pane_ids.contains(&self.active_pane) {
            self.active_pane = self.layout.first_pane();
        }
        self.detached_shell_ids
            .retain(|id| !id.is_empty() && !shell_ids.contains(id));
        Ok(())
    }
}

enum LockedScaffold {
    Locked(PaneId),
    Slot(PaneId),
    Split {
        axis: Axis,
        ratio: f32,
        first: Box<Self>,
        second: Box<Self>,
    },
}

impl LockedScaffold {
    fn new(layout: &Layout, locked: &HashSet<PaneId>, slots: &mut usize) -> Self {
        if !layout.pane_ids().iter().any(|id| locked.contains(id)) {
            *slots += 1;
            return Self::Slot(layout.first_pane());
        }
        match layout {
            Layout::Pane(id) => Self::Locked(*id),
            Layout::Split {
                axis,
                ratio,
                first,
                second,
            } => Self::Split {
                axis: *axis,
                ratio: *ratio,
                first: Box::new(Self::new(first, locked, slots)),
                second: Box::new(Self::new(second, locked, slots)),
            },
        }
    }

    fn fill(
        self,
        fragments: &mut VecDeque<Layout>,
        panes: &mut BTreeMap<PaneId, SavedPane>,
        used: &mut HashSet<PaneId>,
    ) -> Result<Layout, String> {
        match self {
            Self::Locked(id) => Ok(Layout::Pane(id)),
            Self::Slot(preferred) => {
                if let Some(fragment) = fragments.pop_front() {
                    Ok(fragment)
                } else {
                    let id = available_pane_id(preferred, used)?;
                    panes.insert(id, SavedPane::default());
                    Ok(Layout::Pane(id))
                }
            }
            Self::Split {
                axis,
                ratio,
                first,
                second,
            } => Ok(Layout::Split {
                axis,
                ratio,
                first: Box::new(first.fill(fragments, panes, used)?),
                second: Box::new(second.fill(fragments, panes, used)?),
            }),
        }
    }
}

fn without_panes(layout: Layout, removed: &HashSet<PaneId>) -> Option<Layout> {
    match layout {
        Layout::Pane(id) => (!removed.contains(&id)).then_some(Layout::Pane(id)),
        Layout::Split {
            axis,
            ratio,
            first,
            second,
        } => match (
            without_panes(*first, removed),
            without_panes(*second, removed),
        ) {
            (Some(first), Some(second)) => Some(Layout::Split {
                axis,
                ratio,
                first: Box::new(first),
                second: Box::new(second),
            }),
            (Some(remaining), None) | (None, Some(remaining)) => Some(remaining),
            (None, None) => None,
        },
    }
}

/// Cut only outer splits when several unlocked regions surround locked panes.
/// Original axes and ratios inside each resulting fragment remain untouched.
fn partition_layout(layout: Layout, slots: usize) -> Vec<Layout> {
    let mut fragments = vec![layout];
    while fragments.len() < slots {
        let Some(index) = fragments
            .iter()
            .position(|layout| matches!(layout, Layout::Split { .. }))
        else {
            break;
        };
        let Layout::Split { first, second, .. } = fragments.remove(index) else {
            unreachable!()
        };
        fragments.insert(index, *second);
        fragments.insert(index, *first);
    }
    fragments
}

fn available_pane_id(preferred: PaneId, used: &mut HashSet<PaneId>) -> Result<PaneId, String> {
    if preferred > 0 && preferred < MAX_PANE_ID && used.insert(preferred) {
        return Ok(preferred);
    }
    for id in 1..MAX_PANE_ID {
        if used.insert(id) {
            return Ok(id);
        }
    }
    Err("No pane ID is available for the unlocked layout".to_owned())
}

fn remap_layout(
    layout: &mut Layout,
    used: &mut HashSet<PaneId>,
    mapping: &mut BTreeMap<PaneId, PaneId>,
) -> Result<(), String> {
    match layout {
        Layout::Pane(id) => {
            let original = *id;
            *id = available_pane_id(original, used)?;
            mapping.insert(original, *id);
        }
        Layout::Split { first, second, .. } => {
            remap_layout(first, used, mapping)?;
            remap_layout(second, used, mapping)?;
        }
    }
    Ok(())
}

fn sync_saved_pane(pane: &mut SavedPane) {
    if !pane
        .active_tab_key
        .as_ref()
        .is_some_and(|key| pane.tabs.iter().any(|tab| tab.key() == *key))
    {
        pane.active_tab_key = pane.tabs.first().map(SavedTab::key);
    }
    pane.shell_ids = pane
        .tabs
        .iter()
        .filter_map(|tab| match tab {
            SavedTab::Shell { shell_id } => Some(shell_id.clone()),
            SavedTab::Panel { .. } => None,
        })
        .collect();
    pane.active_shell_id = pane.tabs.iter().find_map(|tab| match tab {
        SavedTab::Shell { shell_id }
            if pane.active_tab_key.as_deref() == Some(tab.key().as_str()) =>
        {
            Some(shell_id.clone())
        }
        _ => None,
    });
}

/// Every project's layout, kept entry by entry. An entry this build cannot
/// parse (typically a panel kind a newer build added) stays as raw JSON and is
/// written back unchanged, so an older build never destroys a newer one's data.
#[derive(Debug, Serialize)]
struct SavedLayouts {
    schema_version: u32,
    projects: BTreeMap<String, SavedEntry>,
    /// Top-level fields this build does not know about.
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

impl Default for SavedLayouts {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            projects: BTreeMap::new(),
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Debug)]
enum SavedEntry {
    Layout(Box<ProjectLayout>),
    Unreadable { raw: Value, reason: String },
}

impl Serialize for SavedEntry {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Layout(layout) => layout.serialize(serializer),
            Self::Unreadable { raw, .. } => raw.serialize(serializer),
        }
    }
}

impl SavedEntry {
    fn layout(&self) -> Result<ProjectLayout, String> {
        match self {
            Self::Layout(layout) => {
                let mut layout = (**layout).clone();
                layout.normalize()?;
                Ok(layout)
            }
            Self::Unreadable { reason, .. } => Err(unreadable_entry_message(reason)),
        }
    }

    fn window_size(&self) -> Option<WindowSize> {
        match self {
            Self::Layout(layout) => layout.window_size,
            Self::Unreadable { raw, .. } => raw
                .get("window_size")
                .and_then(|size| WindowSize::deserialize(size).ok()),
        }
        .filter(WindowSize::is_valid)
    }
}

fn unreadable_entry_message(reason: &str) -> String {
    format!(
        "The saved layout for this project cannot be read ({reason}); it may come from a newer \
         RiWork and is left unchanged"
    )
}

/// Why layouts.json could not be used.
enum Unreadable {
    /// Not layout data at all. A save sets the file aside and starts over.
    Corrupt(String),
    /// Possibly a newer build's data, or unreadable right now. Never replaced.
    Kept(String),
}

impl Unreadable {
    fn into_message(self) -> String {
        match self {
            Self::Corrupt(message) | Self::Kept(message) => message,
        }
    }
}

impl SavedLayouts {
    fn parse(data: &[u8], path: &Path) -> Result<Self, Unreadable> {
        if data.iter().all(u8::is_ascii_whitespace) {
            return Ok(Self::default());
        }
        let corrupt = |detail: String| {
            Unreadable::Corrupt(format!(
                "Cannot parse {}: {detail}; it is set aside as layouts.corrupt-*.json on the next save",
                path.display()
            ))
        };
        let Value::Object(mut fields) =
            serde_json::from_slice(data).map_err(|error| corrupt(error.to_string()))?
        else {
            return Err(corrupt("expected a JSON object".to_owned()));
        };
        // Checked before anything else: a newer schema may reshape the rest.
        if let Some(version) = fields.remove("schema_version")
            && version.as_u64() != Some(u64::from(SCHEMA_VERSION))
        {
            return Err(Unreadable::Kept(format!(
                "Unsupported layout schema {version}; this build supports schema {SCHEMA_VERSION}"
            )));
        }
        let projects = match fields.remove("projects") {
            None => serde_json::Map::new(),
            Some(Value::Object(projects)) => projects,
            Some(_) => return Err(corrupt("\"projects\" is not an object".to_owned())),
        };
        let projects = projects
            .into_iter()
            .map(|(id, raw)| {
                let entry = match ProjectLayout::deserialize(&raw) {
                    Ok(layout) => SavedEntry::Layout(Box::new(layout)),
                    Err(error) => SavedEntry::Unreadable {
                        reason: error.to_string(),
                        raw,
                    },
                };
                (id, entry)
            })
            .collect();
        Ok(Self {
            schema_version: SCHEMA_VERSION,
            projects,
            extra: fields.into_iter().collect(),
        })
    }
}

#[derive(Clone, Debug)]
pub struct LayoutStore {
    dir: PathBuf,
}

impl LayoutStore {
    pub fn open_default() -> Result<Self, String> {
        Self::open(crate::paths::riwork_home()?)
    }

    pub fn open(dir: impl Into<PathBuf>) -> Result<Self, String> {
        let dir = dir.into();
        fs::create_dir_all(&dir)
            .map_err(|error| format!("Cannot create {}: {error}", dir.display()))?;
        Ok(Self { dir })
    }

    /// An error means this project's layout, or the file, cannot be used; the
    /// caller falls back to a default layout and the stored data is left alone.
    pub fn load(&self, project_id: &str) -> Result<Option<ProjectLayout>, String> {
        let lock = self.lock_file()?;
        FileExt::lock_shared(&lock).map_err(|error| format!("Cannot lock layouts: {error}"))?;
        let layouts = self.read_layouts().map_err(Unreadable::into_message)?;
        layouts
            .projects
            .get(project_id)
            .map(SavedEntry::layout)
            .transpose()
    }

    /// The remembered window size, or None on any problem. Opening a window
    /// must not depend on the rest of the layout parsing.
    pub fn window_size(&self, project_id: &str) -> Option<WindowSize> {
        let lock = self.lock_file().ok()?;
        FileExt::lock_shared(&lock).ok()?;
        self.read_layouts()
            .ok()?
            .projects
            .get(project_id)?
            .window_size()
    }

    pub fn save(&self, project_id: &str, layout: &ProjectLayout) -> Result<(), String> {
        if project_id.is_empty() {
            return Err("Cannot save a layout without a project UUID".to_owned());
        }
        layout.layout.validate_size()?;
        let mut layout = layout.clone();
        layout.normalize()?;
        let lock = self.lock_file()?;
        FileExt::lock_exclusive(&lock).map_err(|error| format!("Cannot lock layouts: {error}"))?;
        let mut layouts = match self.read_layouts() {
            Ok(layouts) => layouts,
            Err(Unreadable::Corrupt(_)) => {
                self.set_aside_corrupt_file()?;
                SavedLayouts::default()
            }
            Err(Unreadable::Kept(message)) => return Err(message),
        };
        match layouts.projects.get(project_id) {
            Some(SavedEntry::Unreadable { reason, .. }) => {
                return Err(unreadable_entry_message(reason));
            }
            // Common case: nothing changed, so skip the write and its fsync.
            Some(existing) if existing.layout().is_ok_and(|existing| existing == layout) => {
                return Ok(());
            }
            _ => {}
        }
        layouts
            .projects
            .insert(project_id.to_owned(), SavedEntry::Layout(Box::new(layout)));
        self.write_layouts(&layouts)
    }

    fn lock_file(&self) -> Result<File, String> {
        // A pure lock file: its contents never matter, so never truncate it.
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.dir.join("layouts.lock"))
            .map_err(|error| format!("Cannot open layout lock: {error}"))
    }

    fn read_layouts(&self) -> Result<SavedLayouts, Unreadable> {
        let path = self.dir.join("layouts.json");
        match fs::metadata(&path) {
            Ok(metadata) if metadata.len() > MAX_FILE_BYTES => {
                return Err(Unreadable::Kept(format!("{} is too large", path.display())));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(SavedLayouts::default());
            }
            Err(error) => {
                return Err(Unreadable::Kept(format!(
                    "Cannot read {}: {error}",
                    path.display()
                )));
            }
        }
        let data = fs::read(&path).map_err(|error| {
            Unreadable::Kept(format!("Cannot read {}: {error}", path.display()))
        })?;
        SavedLayouts::parse(&data, &path)
    }

    /// Keep an unparseable layouts.json for manual recovery instead of
    /// deleting it, so persistence can resume.
    fn set_aside_corrupt_file(&self) -> Result<(), String> {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        let unique = Uuid::new_v4().simple().to_string();
        let path = self.dir.join("layouts.json");
        let aside = self
            .dir
            .join(format!("layouts.corrupt-{secs}-{}.json", &unique[..8]));
        fs::rename(&path, &aside).map_err(|error| {
            format!(
                "Cannot set aside {} as {}: {error}",
                path.display(),
                aside.display()
            )
        })
    }

    fn write_layouts(&self, layouts: &SavedLayouts) -> Result<(), String> {
        let path = self.dir.join("layouts.json");
        let tmp = self.dir.join(format!(".layouts-{}.tmp", Uuid::new_v4()));
        let write = || -> Result<(), String> {
            // Encode first: pretty printing straight into the file is one
            // syscall per token, and this runs on the UI thread.
            let mut data = serde_json::to_vec_pretty(layouts)
                .map_err(|error| format!("Cannot encode layouts: {error}"))?;
            data.push(b'\n');
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)
                .map_err(|error| format!("Cannot create {}: {error}", tmp.display()))?;
            file.write_all(&data)
                .map_err(|error| format!("Cannot write layouts: {error}"))?;
            file.sync_all()
                .map_err(|error| format!("Cannot sync layouts: {error}"))?;
            // The rename is atomic; a crash before the directory entry reaches
            // disk only loses this one save, so the directory is not synced.
            fs::rename(&tmp, &path)
                .map_err(|error| format!("Cannot replace {}: {error}", path.display()))
        };
        let result = write();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            Self(env::temp_dir().join(format!("riwork-layout-test-{}", Uuid::new_v4())))
        }

        fn store(&self) -> LayoutStore {
            LayoutStore::open(&self.0).unwrap()
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    impl TestDirectory {
        fn file(&self) -> PathBuf {
            self.0.join("layouts.json")
        }

        fn write(&self, contents: impl AsRef<[u8]>) {
            fs::create_dir_all(&self.0).unwrap();
            fs::write(self.file(), contents).unwrap();
        }

        fn read_value(&self) -> Value {
            serde_json::from_slice(&fs::read(self.file()).unwrap()).unwrap()
        }
    }

    fn saved_layout() -> ProjectLayout {
        let mut layout = Layout::Pane(4);
        assert!(layout.split(4, Axis::SideBySide, 8));
        assert!(layout.split(8, Axis::Stacked, 15));
        let mut saved = ProjectLayout {
            layout,
            panes: BTreeMap::from([
                (
                    4,
                    SavedPane {
                        shell_ids: vec!["shell-b".to_owned(), "shell-a".to_owned()],
                        active_shell_id: Some("shell-a".to_owned()),
                        ..SavedPane::default()
                    },
                ),
                (
                    8,
                    SavedPane {
                        shell_ids: vec!["shell-c".to_owned()],
                        active_shell_id: Some("shell-c".to_owned()),
                        ..SavedPane::default()
                    },
                ),
                (15, SavedPane::default()),
            ]),
            active_pane: 8,
            locked_panes: None,
            panels_initialized: false,
            detached_shell_ids: HashSet::from(["shell-detached".to_owned()]),
            selected_worktree_id: Some("worktree-selected".to_owned()),
            selected_task_id: Some("task-selected".to_owned()),
            sidebar_visible: false,
            window_size: Some(WindowSize {
                width: 1440.0,
                height: 900.0,
            }),
        };
        saved.normalize().unwrap();
        saved
    }

    #[test]
    fn nested_layout_round_trip_preserves_tabs_selections_and_detachment() {
        let directory = TestDirectory::new();
        let layout = saved_layout();
        directory.store().save("project-a", &layout).unwrap();
        let restored = directory.store().load("project-a").unwrap().unwrap();
        assert_eq!(restored, layout);
        assert_eq!(restored.layout.pane_ids(), vec![4, 8, 15]);
        assert!(directory.store().load("missing").unwrap().is_none());
    }

    #[test]
    fn settings_restores_as_a_single_selected_workspace_tab() {
        let directory = TestDirectory::new();
        let mut layout = saved_layout();
        let settings = SavedTab::Panel {
            panel: PanelKind::Settings,
        };
        layout
            .panes
            .get_mut(&4)
            .unwrap()
            .tabs
            .push(settings.clone());
        layout.panes.get_mut(&4).unwrap().active_tab_key = Some(settings.key());
        layout.panes.get_mut(&8).unwrap().tabs.push(settings);
        layout.active_pane = 4;
        layout.normalize().unwrap();
        directory.store().save("settings-project", &layout).unwrap();
        let restored = directory.store().load("settings-project").unwrap().unwrap();
        assert_eq!(restored, layout);
        assert_eq!(restored.active_pane, 4);
        assert_eq!(
            restored.panes[&4].active_tab_key.as_deref(),
            Some("panel:settings")
        );
        assert_eq!(
            restored
                .panes
                .values()
                .flat_map(|pane| &pane.tabs)
                .filter(|tab| tab.key() == "panel:settings")
                .count(),
            1
        );
    }

    #[test]
    fn project_settings_tabs_restore_independently_for_each_project() {
        let directory = TestDirectory::new();
        let settings = SavedTab::Panel {
            panel: PanelKind::ProjectSettings,
        };
        for project in ["project-a", "project-b"] {
            let mut layout = saved_layout();
            layout
                .panes
                .get_mut(&4)
                .unwrap()
                .tabs
                .push(settings.clone());
            layout
                .panes
                .get_mut(&8)
                .unwrap()
                .tabs
                .push(settings.clone());
            layout.panes.get_mut(&4).unwrap().active_tab_key = Some(settings.key());
            layout
                .panes
                .get_mut(&4)
                .unwrap()
                .tabs
                .push(SavedTab::Panel {
                    panel: PanelKind::Settings,
                });
            layout.active_pane = 4;
            layout.normalize().unwrap();
            directory.store().save(project, &layout).unwrap();
            let restored = directory.store().load(project).unwrap().unwrap();
            assert_eq!(restored, layout);
            assert_eq!(
                restored.panes[&4].active_tab_key.as_deref(),
                Some("panel:project_settings")
            );
            assert_eq!(
                restored
                    .panes
                    .values()
                    .flat_map(|pane| &pane.tabs)
                    .filter(|tab| tab.key() == "panel:project_settings")
                    .count(),
                1
            );
            assert!(
                restored.panes[&4]
                    .tabs
                    .iter()
                    .any(|tab| tab.key() == "panel:settings")
            );
        }
    }

    #[test]
    fn files_tabs_restore_per_project_with_independent_selected_worktrees() {
        let directory = TestDirectory::new();
        let files = SavedTab::Panel {
            panel: PanelKind::Files,
        };
        let mut expected = BTreeMap::new();
        for (project, worktree, active_pane, duplicate_pane) in [
            ("project-a", "worktree-a", 4, 8),
            ("project-b", "worktree-b", 8, 15),
        ] {
            let mut layout = saved_layout();
            layout.selected_worktree_id = Some(worktree.to_owned());
            layout
                .panes
                .get_mut(&active_pane)
                .unwrap()
                .tabs
                .push(files.clone());
            layout.panes.get_mut(&active_pane).unwrap().active_tab_key = Some(files.key());
            layout
                .panes
                .get_mut(&duplicate_pane)
                .unwrap()
                .tabs
                .push(files.clone());
            layout.active_pane = active_pane;
            layout.normalize().unwrap();
            directory.store().save(project, &layout).unwrap();
            expected.insert(project, layout);
        }
        for (project, layout) in &expected {
            let restored = directory.store().load(project).unwrap().unwrap();
            assert_eq!(&restored, layout);
            assert_eq!(
                restored.panes[&restored.active_pane]
                    .active_tab_key
                    .as_deref(),
                Some("panel:files")
            );
            assert_eq!(
                restored
                    .panes
                    .values()
                    .flat_map(|pane| &pane.tabs)
                    .filter(|tab| matches!(
                        tab,
                        SavedTab::Panel {
                            panel: PanelKind::Files
                        }
                    ))
                    .count(),
                1
            );
        }
        let mut first = expected["project-a"].clone();
        first.selected_worktree_id = Some("worktree-a-next".to_owned());
        directory.store().save("project-a", &first).unwrap();
        assert_eq!(directory.store().load("project-a").unwrap().unwrap(), first);
        assert_eq!(
            directory.store().load("project-b").unwrap().unwrap(),
            expected["project-b"]
        );
    }

    #[test]
    fn legacy_and_invalid_window_sizes_do_not_break_layout_restoration() {
        let saved = saved_layout();
        let mut value = serde_json::to_value(&saved).unwrap();
        value.as_object_mut().unwrap().remove("window_size");
        let mut legacy: ProjectLayout = serde_json::from_value(value).unwrap();
        legacy.normalize().unwrap();
        assert!(legacy.window_size.is_none());
        assert_eq!(legacy.layout, saved.layout);
        legacy.window_size = Some(WindowSize {
            width: -10.0,
            height: 900.0,
        });
        legacy.normalize().unwrap();
        assert!(legacy.window_size.is_none());
        assert!(WindowSize::new(f32::INFINITY, 900.0).is_none());
        assert!(WindowSize::new(1440.0, 0.0).is_none());
        assert_eq!(saved.window_size, WindowSize::new(1440.0, 900.0));
    }

    #[test]
    fn concurrent_project_saves_merge_without_losing_other_projects() {
        let directory = TestDirectory::new();
        let store = directory.store();
        let workers = (0..8)
            .map(|index| {
                let store = store.clone();
                std::thread::spawn(move || {
                    let mut layout = saved_layout();
                    layout.selected_task_id = Some(format!("task-{index}"));
                    store.save(&format!("project-{index}"), &layout).unwrap();
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().unwrap();
        }
        for index in 0..8 {
            let layout = store.load(&format!("project-{index}")).unwrap().unwrap();
            assert_eq!(layout.selected_task_id, Some(format!("task-{index}")));
        }
    }

    #[test]
    fn normalization_repairs_geometry_and_invalid_selections() {
        let mut layout = saved_layout();
        layout.layout = Layout::Split {
            axis: Axis::Stacked,
            ratio: 0.5,
            first: Box::new(Layout::Pane(4)),
            second: Box::new(Layout::Split {
                axis: Axis::SideBySide,
                ratio: 0.5,
                first: Box::new(Layout::Pane(4)),
                second: Box::new(Layout::Pane(22)),
            }),
        };
        layout.active_pane = 999;
        layout.panes.get_mut(&4).unwrap().tabs = ["shell-b", "shell-b", ""]
            .into_iter()
            .map(|id| SavedTab::Shell {
                shell_id: id.to_owned(),
            })
            .collect();
        layout.detached_shell_ids.insert("shell-b".to_owned());
        layout.normalize().unwrap();
        assert_eq!(layout.layout.pane_ids(), vec![4, 22]);
        assert_eq!(layout.panes.len(), 2);
        assert_eq!(layout.panes[&22], SavedPane::default());
        assert_eq!(layout.active_pane, 4);
        assert_eq!(layout.panes[&4].shell_ids, vec!["shell-b"]);
        assert_eq!(layout.panes[&4].active_shell_id.as_deref(), Some("shell-b"));
        assert!(!layout.detached_shell_ids.contains("shell-b"));
        assert!(layout.detached_shell_ids.contains("shell-detached"));
    }

    #[test]
    fn normalization_deduplicates_shells_across_panes() {
        let mut layout = saved_layout();
        layout.panes.get_mut(&8).unwrap().tabs = ["shell-a", "shell-c"]
            .into_iter()
            .map(|id| SavedTab::Shell {
                shell_id: id.to_owned(),
            })
            .collect();
        layout.panes.get_mut(&8).unwrap().active_tab_key = Some("shell:shell-a".to_owned());
        layout.normalize().unwrap();
        assert_eq!(layout.panes[&8].shell_ids, vec!["shell-c"]);
        assert_eq!(layout.panes[&8].active_shell_id.as_deref(), Some("shell-c"));
    }

    #[test]
    fn legacy_shell_layout_migrates_without_changing_tab_order_or_selection() {
        let mut layout: ProjectLayout = serde_json::from_str(
            r#"{
                "layout": {"split": {
                    "axis": "side_by_side",
                    "first": {"pane": 4},
                    "second": {"pane": 8}
                }},
                "panes": {
                    "4": {"shell_ids": ["shell-b", "shell-a"], "active_shell_id": "shell-a"},
                    "8": {"shell_ids": ["shell-a", "shell-c"], "active_shell_id": "shell-a"}
                },
                "active_pane": 8
            }"#,
        )
        .unwrap();
        layout.normalize().unwrap();
        assert_eq!(layout.layout.ratio_at(&[]), Some(0.5));
        assert_eq!(layout.panes[&4].shell_ids, vec!["shell-b", "shell-a"]);
        assert_eq!(
            layout.panes[&4]
                .tabs
                .iter()
                .map(SavedTab::key)
                .collect::<Vec<_>>(),
            vec!["shell:shell-b", "shell:shell-a"]
        );
        assert_eq!(
            layout.panes[&4].active_tab_key.as_deref(),
            Some("shell:shell-a")
        );
        assert_eq!(
            layout.panes[&8].active_tab_key.as_deref(),
            Some("shell:shell-c")
        );
        assert_eq!(layout.panes[&8].tab_edge, TabEdge::Top);
        assert!(!layout.panels_initialized);
    }

    #[test]
    fn mixed_tabs_preserve_order_and_selection_while_migrating_bottom_strips() {
        let directory = TestDirectory::new();
        let mut layout = saved_layout();
        layout.panels_initialized = true;
        layout.panes.get_mut(&4).unwrap().tabs = vec![
            SavedTab::Panel {
                panel: PanelKind::Tasks,
            },
            SavedTab::Shell {
                shell_id: "shell-b".to_owned(),
            },
            SavedTab::Panel {
                panel: PanelKind::Projects,
            },
            SavedTab::Shell {
                shell_id: "shell-a".to_owned(),
            },
        ];
        layout.panes.get_mut(&4).unwrap().active_tab_key = Some("panel:tasks".to_owned());
        layout.panes.get_mut(&4).unwrap().tab_edge = TabEdge::Bottom;
        layout.panes.get_mut(&8).unwrap().tabs = vec![
            SavedTab::Shell {
                shell_id: "shell-b".to_owned(),
            },
            SavedTab::Panel {
                panel: PanelKind::Tasks,
            },
            SavedTab::Shell {
                shell_id: "shell-c".to_owned(),
            },
            SavedTab::Panel {
                panel: PanelKind::Worktrees,
            },
        ];
        layout.panes.get_mut(&8).unwrap().active_tab_key = Some("panel:tasks".to_owned());
        layout.normalize().unwrap();
        directory.store().save("project-mixed", &layout).unwrap();
        let restored = directory.store().load("project-mixed").unwrap().unwrap();
        assert_eq!(restored, layout);
        assert_eq!(restored.panes[&4].tab_edge, TabEdge::Top);
        assert_eq!(
            restored.panes[&4].active_tab_key.as_deref(),
            Some("panel:tasks")
        );
        assert_eq!(restored.panes[&4].active_shell_id, None);
        assert_eq!(restored.panes[&4].shell_ids, vec!["shell-b", "shell-a"]);
        assert_eq!(
            restored.panes[&8]
                .tabs
                .iter()
                .map(SavedTab::key)
                .collect::<Vec<_>>(),
            vec!["shell:shell-c", "panel:worktrees"]
        );
        assert_eq!(
            restored.panes[&8].active_tab_key.as_deref(),
            Some("shell:shell-c")
        );
    }

    #[test]
    fn split_ratios_round_trip_and_survive_insertions_and_collapses() {
        let directory = TestDirectory::new();
        let mut layout = saved_layout();
        assert!(layout.layout.set_ratio(&[], 0.36));
        assert!(layout.layout.set_ratio(&[true], 0.72));
        assert!(!layout.layout.set_ratio(&[false], 0.25));
        assert!(!layout.layout.set_ratio(&[true, true], 0.25));
        assert!(layout.layout.split_with(4, Axis::Stacked, 23, true));
        assert_eq!(layout.layout.pane_ids(), vec![23, 4, 8, 15]);
        assert_eq!(layout.layout.ratio_at(&[]), Some(0.36));
        assert_eq!(layout.layout.ratio_at(&[false]), Some(0.5));
        assert_eq!(layout.layout.ratio_at(&[true]), Some(0.72));
        layout.layout = layout.layout.without(23).unwrap();
        directory.store().save("project-sized", &layout).unwrap();
        let restored = directory.store().load("project-sized").unwrap().unwrap();
        assert_eq!(restored, layout);
        assert_eq!(restored.layout.ratio_at(&[]), Some(0.36));
        assert_eq!(restored.layout.ratio_at(&[true]), Some(0.72));
        assert_eq!(restored.layout.ratio_at(&[false]), None);
        let collapsed = restored.layout.without(4).unwrap();
        assert_eq!(collapsed.ratio_at(&[]), Some(0.72));
        assert_eq!(collapsed.pane_ids(), vec![8, 15]);
    }

    #[test]
    fn invalid_ratios_are_repaired_before_persistence() {
        let directory = TestDirectory::new();
        let mut layout = saved_layout();
        if let Layout::Split { ratio, second, .. } = &mut layout.layout {
            *ratio = f32::NAN;
            if let Layout::Split { ratio, .. } = second.as_mut() {
                *ratio = -2.0;
            }
        }
        directory.store().save("project-ratios", &layout).unwrap();
        let restored = directory.store().load("project-ratios").unwrap().unwrap();
        assert_eq!(restored.layout.ratio_at(&[]), Some(0.5));
        assert_eq!(restored.layout.ratio_at(&[true]), Some(0.1));
        assert!(layout.layout.set_ratio(&[], f32::INFINITY));
        assert_eq!(layout.layout.ratio_at(&[]), Some(0.5));
        assert!(layout.layout.set_ratio(&[true], 2.0));
        assert_eq!(layout.layout.ratio_at(&[true]), Some(0.9));
    }

    #[test]
    fn unreasonable_layout_is_rejected_without_replacing_saved_state() {
        let directory = TestDirectory::new();
        let store = directory.store();
        let original = saved_layout();
        store.save("project-a", &original).unwrap();
        let mut invalid = original.clone();
        invalid.layout = Layout::Pane(1);
        for id in 2..=(MAX_DEPTH as u64 + 3) {
            assert!(invalid.layout.split(id - 1, Axis::Stacked, id));
        }
        assert!(
            store
                .save("project-a", &invalid)
                .unwrap_err()
                .contains("depth")
        );
        assert_eq!(store.load("project-a").unwrap(), Some(original));
    }

    #[test]
    fn unsafe_pane_ids_are_rejected_and_zero_is_repaired() {
        let mut layout = saved_layout();
        layout.layout = Layout::Pane(u64::MAX);
        assert!(layout.normalize().unwrap_err().contains("pane ID"));
        layout.layout = Layout::Pane(0);
        layout.normalize().unwrap();
        assert_eq!(layout.layout, Layout::Pane(1));
        assert_eq!(layout.active_pane, 1);
        assert_eq!(layout.panes, BTreeMap::from([(1, SavedPane::default())]));
    }

    #[test]
    fn excessive_pane_count_is_rejected() {
        fn balanced_layout(first: PaneId, count: usize) -> Layout {
            if count == 1 {
                Layout::Pane(first)
            } else {
                let first_count = count / 2;
                Layout::Split {
                    axis: Axis::SideBySide,
                    ratio: 0.5,
                    first: Box::new(balanced_layout(first, first_count)),
                    second: Box::new(balanced_layout(
                        first + first_count as u64,
                        count - first_count,
                    )),
                }
            }
        }
        let mut layout = saved_layout();
        layout.layout = balanced_layout(1, MAX_PANES + 1);
        assert!(layout.normalize().unwrap_err().contains("panes"));
    }

    fn pane(tabs: Vec<SavedTab>, selected: usize) -> SavedPane {
        let mut pane = SavedPane {
            active_tab_key: tabs.get(selected).map(SavedTab::key),
            tabs,
            ..SavedPane::default()
        };
        sync_saved_pane(&mut pane);
        pane
    }

    fn shell(id: &str) -> SavedTab {
        SavedTab::Shell {
            shell_id: id.into(),
        }
    }
    fn panel(kind: PanelKind) -> SavedTab {
        SavedTab::Panel { panel: kind }
    }

    #[test]
    fn left_navigation_locks_by_default_but_explicit_unlock_survives_round_trip() {
        let directory = TestDirectory::new();
        let mut previous = saved_layout();
        assert!(previous.effective_locked_panes().is_empty());
        previous
            .panes
            .get_mut(&4)
            .unwrap()
            .tabs
            .push(panel(PanelKind::Projects));
        assert_eq!(previous.effective_locked_panes(), HashSet::from([4]));
        previous.locked_panes = Some(HashSet::new());
        assert!(previous.effective_locked_panes().is_empty());
        directory.store().save("unlocked", &previous).unwrap();
        let restored = directory.store().load("unlocked").unwrap().unwrap();
        assert_eq!(restored.locked_panes, Some(HashSet::new()));
        let mut destination = saved_layout();
        destination
            .panes
            .get_mut(&4)
            .unwrap()
            .tabs
            .push(panel(PanelKind::Projects));
        let carried = destination.carry_locked_regions_from(&previous).unwrap();
        assert_eq!(carried.layout, destination.layout);
        assert!(carried.effective_locked_panes().is_empty());
        assert_eq!(carried.locked_panes, Some(HashSet::new()));
    }

    #[test]
    fn carry_preserves_locked_tabs_ratios_and_maps_colliding_destination_ids() {
        let mut previous = saved_layout();
        previous.layout.set_ratio(&[], 0.27);
        previous.panes.insert(
            4,
            pane(
                vec![
                    panel(PanelKind::Projects),
                    panel(PanelKind::Files),
                    shell("global"),
                ],
                1,
            ),
        );
        previous.active_pane = 4;
        let mut destination = saved_layout();
        destination.layout.set_ratio(&[], 0.61);
        destination.layout.set_ratio(&[true], 0.72);
        destination
            .panes
            .insert(4, pane(vec![shell("destination-a")], 0));
        destination.panes.insert(
            8,
            pane(vec![panel(PanelKind::Files), shell("destination-b")], 1),
        );
        destination.active_pane = 4;
        destination.selected_worktree_id = Some("destination-worktree".into());
        let carried = destination.carry_locked_regions_from(&previous).unwrap();
        assert_eq!(carried.panes[&4], previous.panes[&4]);
        assert_eq!(carried.layout.ratio_at(&[]), Some(0.27));
        assert_eq!(carried.layout.ratio_at(&[true]), Some(0.61));
        assert_eq!(carried.layout.ratio_at(&[true, true]), Some(0.72));
        assert_ne!(carried.active_pane, 4);
        assert_eq!(
            carried.panes[&carried.active_pane]
                .active_tab_key
                .as_deref(),
            Some("shell:destination-a")
        );
        assert_eq!(
            carried.selected_worktree_id,
            destination.selected_worktree_id
        );
        let keys = carried
            .panes
            .values()
            .flat_map(|pane| &pane.tabs)
            .map(SavedTab::key)
            .collect::<Vec<_>>();
        assert_eq!(keys.iter().filter(|key| *key == "panel:files").count(), 1);
        assert!(keys.contains(&"shell:destination-b".to_owned()));
    }

    #[test]
    fn destination_locked_pane_sessions_are_recovered_into_unlocked_region() {
        let mut previous = saved_layout();
        previous.panes.insert(
            4,
            pane(vec![panel(PanelKind::Projects), shell("global")], 0),
        );
        let mut destination = saved_layout();
        destination.panes.insert(
            4,
            pane(
                vec![
                    panel(PanelKind::Projects),
                    panel(PanelKind::Files),
                    shell("destination-left"),
                    shell("global"),
                ],
                2,
            ),
        );
        destination
            .panes
            .insert(8, pane(vec![shell("destination-right")], 0));
        destination.active_pane = 4;
        let carried = destination.carry_locked_regions_from(&previous).unwrap();
        assert_eq!(carried.panes[&4], previous.panes[&4]);
        assert_eq!(
            carried.panes[&carried.active_pane]
                .active_tab_key
                .as_deref(),
            Some("shell:destination-left")
        );
        let tabs = carried
            .panes
            .values()
            .flat_map(|pane| &pane.tabs)
            .map(SavedTab::key)
            .collect::<Vec<_>>();
        assert!(tabs.contains(&"shell:destination-left".into()));
        assert!(tabs.contains(&"shell:destination-right".into()));
        assert!(tabs.contains(&"panel:files".into()));
        assert_eq!(tabs.iter().filter(|key| *key == "shell:global").count(), 1);
        assert_eq!(
            tabs.iter().filter(|key| *key == "panel:projects").count(),
            1
        );
    }

    #[test]
    fn multiple_locked_regions_preserve_scaffold_and_destination_active_pane() {
        let mut previous = saved_layout();
        previous.layout = Layout::Split {
            axis: Axis::SideBySide,
            ratio: 0.25,
            first: Box::new(Layout::Pane(4)),
            second: Box::new(Layout::Split {
                axis: Axis::Stacked,
                ratio: 0.4,
                first: Box::new(Layout::Pane(8)),
                second: Box::new(Layout::Split {
                    axis: Axis::SideBySide,
                    ratio: 0.7,
                    first: Box::new(Layout::Pane(15)),
                    second: Box::new(Layout::Pane(21)),
                }),
            }),
        };
        previous.panes.insert(21, SavedPane::default());
        previous.locked_panes = Some(HashSet::from([4, 15]));
        let mut destination = saved_layout();
        destination.layout = Layout::Split {
            axis: Axis::Stacked,
            ratio: 0.62,
            first: Box::new(Layout::Pane(4)),
            second: Box::new(Layout::Pane(15)),
        };
        destination.panes.insert(4, pane(vec![shell("new-a")], 0));
        destination.panes.insert(15, pane(vec![shell("new-b")], 0));
        destination.active_pane = 15;
        let carried = destination.carry_locked_regions_from(&previous).unwrap();
        assert_eq!(carried.layout.ratio_at(&[]), Some(0.25));
        assert_eq!(carried.layout.ratio_at(&[true]), Some(0.4));
        assert_eq!(carried.layout.ratio_at(&[true, true]), Some(0.7));
        assert_eq!(carried.panes[&4], previous.panes[&4]);
        assert_eq!(carried.panes[&15], previous.panes[&15]);
        assert_eq!(
            carried.panes[&carried.active_pane]
                .active_tab_key
                .as_deref(),
            Some("shell:new-b")
        );
        assert_eq!(carried.effective_locked_panes(), HashSet::from([4, 15]));
        assert_eq!(carried.layout.pane_ids().len(), 4);
    }

    #[test]
    fn surplus_unlocked_slots_remain_empty_and_all_locked_keeps_sessions_recoverable() {
        let mut previous = saved_layout();
        previous.locked_panes = Some(HashSet::from([8]));
        let mut destination = saved_layout();
        destination.layout = Layout::Pane(4);
        destination.panes = BTreeMap::from([(4, pane(vec![shell("new")], 0))]);
        destination.active_pane = 4;
        let carried = destination.carry_locked_regions_from(&previous).unwrap();
        assert_eq!(carried.layout.pane_ids().len(), 3);
        assert_eq!(
            carried
                .panes
                .values()
                .filter(|pane| pane.tabs.is_empty())
                .count(),
            1
        );
        assert_eq!(carried.panes[&8], previous.panes[&8]);

        previous.locked_panes = Some(previous.layout.pane_ids().into_iter().collect());
        let carried = destination.carry_locked_regions_from(&previous).unwrap();
        assert_eq!(carried.layout, previous.layout);
        assert_eq!(carried.panes, previous.panes);
        assert_eq!(carried.active_pane, previous.active_pane);
        assert!(carried.detached_shell_ids.contains("new"));
    }

    #[test]
    fn stale_lock_ids_are_removed_without_reenabling_auto_lock() {
        let mut layout = saved_layout();
        layout.locked_panes = Some(HashSet::from([99]));
        layout.normalize().unwrap();
        assert_eq!(layout.locked_panes, Some(HashSet::new()));
        let mut json = serde_json::to_value(&layout).unwrap();
        json.as_object_mut().unwrap().remove("locked_panes");
        let legacy: ProjectLayout = serde_json::from_value(json).unwrap();
        assert_eq!(legacy.locked_panes, None);
    }

    /// A layout as a newer build could write it: a project holding a panel kind
    /// and a tab kind this build has never heard of.
    fn newer_layout(unknown_tab: Value) -> Value {
        let mut value = serde_json::to_value(saved_layout()).unwrap();
        value["panes"]["4"]["tabs"]
            .as_array_mut()
            .unwrap()
            .push(unknown_tab);
        value
    }

    #[test]
    fn unparseable_entries_fall_back_per_project_and_are_kept_verbatim_on_save() {
        let directory = TestDirectory::new();
        let store = directory.store();
        let unknown_panel = newer_layout(serde_json::json!({"kind": "panel", "panel": "quantum"}));
        let unknown_tab = newer_layout(serde_json::json!({"kind": "browser", "url": "x"}));
        directory.write(
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "future_top_level": {"keep": [1, 2, 3]},
                "projects": {
                    "old": saved_layout(),
                    "panel": unknown_panel,
                    "tab": unknown_tab,
                },
            }))
            .unwrap(),
        );

        // Only the affected projects fall back; the rest of the file still loads.
        assert_eq!(store.load("old").unwrap(), Some(saved_layout()));
        for id in ["panel", "tab"] {
            let error = store.load(id).unwrap_err();
            assert!(error.contains("cannot be read"), "{error}");
        }
        assert!(store.load("absent").unwrap().is_none());
        // The window size survives even when the rest of the entry does not parse.
        assert_eq!(store.window_size("panel"), WindowSize::new(1440.0, 900.0));

        // An older build saving its own projects leaves the newer data alone,
        // and refuses to replace an entry it could not read.
        let mut changed = saved_layout();
        changed.selected_task_id = Some("another-task".to_owned());
        store.save("old", &changed).unwrap();
        for id in ["panel", "tab"] {
            let error = store.save(id, &saved_layout()).unwrap_err();
            assert!(error.contains("cannot be read"), "{error}");
        }
        store.save("brand-new", &saved_layout()).unwrap();
        let file = directory.read_value();
        assert_eq!(file["projects"]["panel"], unknown_panel);
        assert_eq!(file["projects"]["tab"], unknown_tab);
        assert_eq!(
            file["future_top_level"],
            serde_json::json!({"keep": [1, 2, 3]})
        );
        assert_eq!(store.load("old").unwrap(), Some(changed));
        assert_eq!(store.load("brand-new").unwrap(), Some(saved_layout()));
    }

    #[test]
    fn corrupt_file_is_set_aside_on_save_instead_of_crashing_or_being_deleted() {
        let directory = TestDirectory::new();
        let store = directory.store();
        let garbage = b"{\"schema_version\": 1, \"projects\": {\"a\": ";
        directory.write(garbage);

        assert!(store.load("a").unwrap_err().contains("Cannot parse"));
        assert_eq!(store.window_size("a"), None);

        store.save("a", &saved_layout()).unwrap();
        assert_eq!(store.load("a").unwrap(), Some(saved_layout()));
        let aside = fs::read_dir(&directory.0)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("layouts.corrupt-"))
            })
            .collect::<Vec<_>>();
        assert_eq!(aside.len(), 1);
        assert_eq!(fs::read(&aside[0]).unwrap(), garbage);

        // An empty file is simply an empty store, not damage worth keeping.
        let empty = TestDirectory::new();
        empty.write("");
        assert!(empty.store().load("a").unwrap().is_none());
        empty.store().save("a", &saved_layout()).unwrap();
        assert_eq!(fs::read_dir(&empty.0).unwrap().count(), 2);
    }

    #[test]
    fn newer_schema_is_neither_loaded_nor_overwritten() {
        let directory = TestDirectory::new();
        let store = directory.store();
        let newer = br#"{"schema_version": 2, "projects": ["reshaped"]}"#;
        directory.write(newer);

        assert!(
            store
                .load("a")
                .unwrap_err()
                .contains("Unsupported layout schema 2")
        );
        assert_eq!(store.window_size("a"), None);
        assert!(store.save("a", &saved_layout()).is_err());
        assert_eq!(fs::read(directory.file()).unwrap(), newer);
        assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 2);
    }

    #[test]
    fn unchanged_layouts_are_not_rewritten_and_writes_leave_no_temporary_files() {
        use std::os::unix::fs::MetadataExt;

        let directory = TestDirectory::new();
        let store = directory.store();
        let layout = saved_layout();
        store.save("a", &layout).unwrap();
        let inode = fs::metadata(directory.file()).unwrap().ino();

        store.save("a", &layout).unwrap();
        assert_eq!(fs::metadata(directory.file()).unwrap().ino(), inode);

        let mut changed = layout.clone();
        changed.sidebar_visible = !changed.sidebar_visible;
        store.save("a", &changed).unwrap();
        assert_ne!(fs::metadata(directory.file()).unwrap().ino(), inode);
        assert_eq!(store.load("a").unwrap(), Some(changed));
        // Only layouts.json and layouts.lock remain.
        assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 2);
    }

    #[test]
    fn window_size_lookup_is_best_effort() {
        let directory = TestDirectory::new();
        let store = directory.store();
        assert_eq!(store.window_size("a"), None);
        store.save("a", &saved_layout()).unwrap();
        assert_eq!(store.window_size("a"), WindowSize::new(1440.0, 900.0));
        assert_eq!(store.window_size("b"), None);
        directory.write("not json");
        assert_eq!(store.window_size("a"), None);
    }

    fn frame(x: f32, y: f32, width: f32, height: f32) -> WindowFrame {
        WindowFrame {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn opening_windows_cascade_inside_the_display_and_clamp_after_cascading() {
        // Below the menu bar, like a laptop's visible frame.
        let display = frame(0.0, 25.0, 1440.0, 875.0);
        let contains = |window: WindowFrame| {
            window.x >= display.x
                && window.y >= display.y
                && window.x + window.width <= display.x + display.width
                && window.y + window.height <= display.y + display.height
        };

        // Room to spare: centred, then stepped 22 points per window, five steps.
        let small = WindowSize::new(1000.0, 600.0).unwrap();
        let first = WindowFrame::opening(small, display, 0);
        assert_eq!(first, frame(220.0, 162.5, 1000.0, 600.0));
        assert_eq!(
            WindowFrame::opening(small, display, 3),
            frame(286.0, 228.5, 1000.0, 600.0)
        );
        assert_eq!(WindowFrame::opening(small, display, 5), first);

        // A size saved on a bigger monitor is fitted, and the later windows of
        // the cascade used to hang off the display by up to 68 x 48 points.
        let huge = WindowSize::new(3000.0, 2000.0).unwrap();
        for cascade in 0..12 {
            let window = WindowFrame::opening(huge, display, cascade);
            assert_eq!((window.width, window.height), (1400.0, 835.0));
            assert!(contains(window), "cascade {cascade}: {window:?}");
        }
        let last = WindowFrame::opening(huge, display, 4);
        assert_eq!(last.x + last.width, display.x + display.width);
        assert_eq!(last.y + last.height, display.y + display.height);
    }
}
