//! Durable per-project tabs and split geometry, independent of shell lifetimes.

use std::{
    cmp::Ordering,
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
        self.split_with_ratio(target, axis, new_pane, new_first, default_ratio())
    }

    /// Like `split_with`, giving the first child `ratio` of the split.
    pub fn split_with_ratio(
        &mut self,
        target: PaneId,
        axis: Axis,
        new_pane: PaneId,
        new_first: bool,
        ratio: f32,
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
                    ratio: normalized_ratio(ratio),
                    first: Box::new(Self::Pane(first)),
                    second: Box::new(Self::Pane(second)),
                };
                true
            }
            Self::Pane(_) => false,
            Self::Split { first, second, .. } => {
                first.split_with_ratio(target, axis, new_pane, new_first, ratio)
                    || second.split_with_ratio(target, axis, new_pane, new_first, ratio)
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
                    ratio: stored_ratio(ratio),
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

/// Dragging keeps a divider inside `normalized_ratio`'s range, but a resize can leave a
/// narrow locked pane in a wide window below it. A ratio in this wider range survives a
/// restart; anything outside it is repaired as before.
const MIN_KEPT_RATIO: f32 = 0.02;
const MAX_KEPT_RATIO: f32 = 0.98;

fn stored_ratio(ratio: f32) -> f32 {
    if (MIN_KEPT_RATIO..=MAX_KEPT_RATIO).contains(&ratio) {
        ratio
    } else {
        normalized_ratio(ratio)
    }
}

/// Thickness of the divider between a split's children. The renderer takes it off the
/// split's extent before sharing the rest by ratio, so pixel maths must do the same.
pub const DIVIDER_THICKNESS: f32 = 5.0;

/// The least a resize leaves a pane along an axis (unless it was already smaller).
pub const MIN_PANE_EXTENT: f32 = 100.0;

/// Pixel size of the area a split tree fills.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Extent {
    pub width: f32,
    pub height: f32,
}

impl Extent {
    fn along(self, axis: Axis) -> f32 {
        match axis {
            Axis::SideBySide => self.width,
            Axis::Stacked => self.height,
        }
    }

    fn across(self, axis: Axis) -> f32 {
        match axis {
            Axis::SideBySide => self.height,
            Axis::Stacked => self.width,
        }
    }

    fn from_axis(axis: Axis, along: f32, across: f32) -> Self {
        match axis {
            Axis::SideBySide => Self {
                width: along,
                height: across,
            },
            Axis::Stacked => Self {
                width: across,
                height: along,
            },
        }
    }
}

impl Layout {
    /// Rewrites split ratios for the tree's area changing from `old` to `new` so locked
    /// panes keep their pixel size and unlocked panes absorb the difference. Returns
    /// whether any ratio changed.
    ///
    /// Along a split's axis a child holds its pixels when it is a locked pane, a split
    /// along that axis whose children both hold theirs, or a split across it with a
    /// locked pane inside (its children share that extent). A child that holds beside
    /// one that does not keeps its pixels and the other absorbs the change; two that
    /// hold, or two that do not, keep their ratio, as before. A locked pane with no
    /// neighbour along an axis has nothing to trade with, so it follows the area there.
    /// The one refinement is a locked pane beside a column that only holds because a
    /// locked pane sits in it (a locked nav next to a region with a locked footer): both
    /// cannot be kept, and the column gives way, so the pane beside it stays put.
    /// When the side that gives would fall under `min_pane` the other shrinks by just
    /// enough; if even that cannot fit both minimums the split stays proportional.
    pub fn preserve_locked(
        &mut self,
        old: Extent,
        new: Extent,
        locked: &impl Fn(PaneId) -> bool,
        min_pane: f32,
    ) -> bool {
        self.rebalance(old, new, locked, min_pane)
    }

    fn rebalance(
        &mut self,
        old: Extent,
        new: Extent,
        locked: &dyn Fn(PaneId) -> bool,
        min_pane: f32,
    ) -> bool {
        let Self::Split {
            axis,
            ratio,
            first,
            second,
        } = self
        else {
            return false;
        };
        if old == new {
            return false;
        }
        let axis = *axis;
        let available_old = old.along(axis) - DIVIDER_THICKNESS;
        let available_new = new.along(axis) - DIVIDER_THICKNESS;
        // Also rejects NaN.
        if !(available_old > 0.0 && available_new > 0.0) {
            return false;
        }
        let first_old = available_old * *ratio;
        let second_old = available_old - first_old;
        let mut changed = false;
        let first_new = if available_old == available_new {
            first_old
        } else {
            let kept_first = match first.grip(axis, locked).cmp(&second.grip(axis, locked)) {
                Ordering::Greater => kept_extent(
                    available_new,
                    first_old,
                    second_old,
                    first.min_extent(axis, min_pane),
                    second.min_extent(axis, min_pane),
                ),
                Ordering::Less => kept_extent(
                    available_new,
                    second_old,
                    first_old,
                    second.min_extent(axis, min_pane),
                    first.min_extent(axis, min_pane),
                )
                .map(|kept| available_new - kept),
                Ordering::Equal => None,
            };
            match kept_first {
                Some(first_new) => {
                    // The renderer applies the stored ratio, so continue from that.
                    let next = (first_new / available_new).clamp(MIN_KEPT_RATIO, MAX_KEPT_RATIO);
                    changed = next != *ratio;
                    *ratio = next;
                    available_new * next
                }
                None => available_new * *ratio,
            }
        };
        let second_new = available_new - first_new;
        let (across_old, across_new) = (old.across(axis), new.across(axis));
        changed |= first.rebalance(
            Extent::from_axis(axis, first_old, across_old),
            Extent::from_axis(axis, first_new, across_new),
            locked,
            min_pane,
        );
        changed |= second.rebalance(
            Extent::from_axis(axis, second_old, across_old),
            Extent::from_axis(axis, second_new, across_new),
            locked,
            min_pane,
        );
        changed
    }

    /// How firmly this subtree holds its extent along `axis`.
    fn grip(&self, axis: Axis, locked: &dyn Fn(PaneId) -> bool) -> Grip {
        match self {
            Self::Pane(id) if locked(*id) => Grip::Firm,
            Self::Pane(_) => Grip::Loose,
            Self::Split {
                axis: own,
                first,
                second,
                ..
            } if *own == axis => first.grip(axis, locked).min(second.grip(axis, locked)),
            Self::Split { first, second, .. } => {
                if first.grip(axis, locked).max(second.grip(axis, locked)) == Grip::Loose {
                    Grip::Loose
                } else {
                    Grip::Shared
                }
            }
        }
    }

    /// The least this subtree needs along `axis`.
    fn min_extent(&self, axis: Axis, min_pane: f32) -> f32 {
        match self {
            Self::Pane(_) => min_pane,
            Self::Split {
                axis: own,
                first,
                second,
                ..
            } if *own == axis => {
                first.min_extent(axis, min_pane)
                    + DIVIDER_THICKNESS
                    + second.min_extent(axis, min_pane)
            }
            Self::Split { first, second, .. } => first
                .min_extent(axis, min_pane)
                .max(second.min_extent(axis, min_pane)),
        }
    }
}

/// How firmly a subtree holds its extent along an axis while its parent resizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Grip {
    /// Nothing locked pins it, so it can absorb a change.
    Loose,
    /// Pinned only because a locked pane inside shares the extent with its siblings.
    Shared,
    /// Locked panes, and only them, fill the extent, so it keeps its pixels.
    Firm,
}

/// What the child that holds keeps of `available` pixels shared with a sibling that
/// gives, or `None` when the two minimums do not fit and the split has to stay proportional.
fn kept_extent(
    available: f32,
    kept: f32,
    other: f32,
    kept_min: f32,
    other_min: f32,
) -> Option<f32> {
    // A child already under its minimum is not pushed back up, so growing never
    // shrinks the rigid side.
    let kept_floor = kept_min.min(kept);
    let other_floor = other_min.min(other);
    (available >= kept_floor + other_floor).then(|| kept.min(available - other_floor))
}

/// The width from which a pane is split side by side for the preview. It is the width at
/// which the old Files panel, which held the preview itself, put the two next to each other.
pub const PREVIEW_SIDE_BY_SIDE_MIN_WIDTH: f32 = 620.0;
/// The explorer keeps this share of a side-by-side split, as the old in-panel split did.
pub const PREVIEW_SIDE_BY_SIDE_RATIO: f32 = 0.36;
/// The height from which a narrower pane is split top and bottom. The pane's tab strip and
/// the explorer's header, filter and footer take about 165 px, so each half of an even
/// split keeps a list of six or seven rows and a readable preview.
pub const PREVIEW_STACKED_MIN_HEIGHT: f32 = 700.0;
pub const PREVIEW_STACKED_RATIO: f32 = 0.5;
/// A pane that is split to make room for the preview, rather than the explorer's own, keeps
/// this share of it: the work in it stays the larger half, and the preview still gets 40%,
/// at least 245 px of the narrowest side-by-side split and 275 px of the shortest stacked one.
pub const PREVIEW_BESIDE_OTHER_RATIO: f32 = 0.6;

/// Where a Preview tab goes when the window has none. Files, when a link asks for a folder, is
/// placed the same way (see `Layout::beside_placement`), so these types serve both.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PreviewPlacement {
    /// A new pane split off `target`, holding the Preview tab. `target` keeps the first
    /// child's `ratio` of the split and its selected tab stays on screen.
    Split {
        target: PaneId,
        axis: Axis,
        ratio: f32,
    },
    /// A tab in an existing pane. It is made the pane's selected tab only when that hides
    /// no terminal and no locked pane's selection; otherwise it waits in the tab strip.
    Tab { pane: PaneId, activate: bool },
}

/// The window's tab for a panel (Preview, or Files), if it has one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreviewTab {
    pub pane: PaneId,
    /// Whether it is the selected tab of its pane, so it is on screen.
    pub shown: bool,
    /// Whether the selected tab of its pane is a terminal, which the user is looking at.
    pub behind_shell: bool,
}

/// What the placement rules need to know about the panes besides their size.
pub struct PaneFacts<'a> {
    pub locked: &'a dyn Fn(PaneId) -> bool,
    /// Whether the pane's selected tab is a terminal (a shell, an agent or an editor).
    pub shows_shell: &'a dyn Fn(PaneId) -> bool,
}

/// What selecting a file does to the layout.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PreviewReveal {
    Leave,
    /// Bring the Preview tab of this pane forward.
    Activate(PaneId),
    Open(PreviewPlacement),
}

/// How a pane of `extent` can be split for the preview: side by side when it is wide enough,
/// otherwise top and bottom when it is tall enough.
fn preview_axis(extent: Extent) -> Option<Axis> {
    if extent.width >= PREVIEW_SIDE_BY_SIDE_MIN_WIDTH {
        Some(Axis::SideBySide)
    } else if extent.height >= PREVIEW_STACKED_MIN_HEIGHT {
        Some(Axis::Stacked)
    } else {
        None
    }
}

impl Layout {
    /// Every pane's pixel size, laid out the way the renderer does it.
    pub fn pane_extents(&self, area: Extent) -> BTreeMap<PaneId, Extent> {
        fn walk(layout: &Layout, area: Extent, out: &mut BTreeMap<PaneId, Extent>) {
            match layout {
                Layout::Pane(id) => {
                    out.insert(*id, area);
                }
                Layout::Split {
                    axis,
                    ratio,
                    first,
                    second,
                } => {
                    let available = (area.along(*axis) - DIVIDER_THICKNESS).max(0.0);
                    let first_along = available * ratio;
                    let across = area.across(*axis);
                    walk(first, Extent::from_axis(*axis, first_along, across), out);
                    walk(
                        second,
                        Extent::from_axis(*axis, available - first_along, across),
                        out,
                    );
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(self, area, &mut out);
        out
    }

    /// Where a new Preview tab goes for the explorer in pane `explorer`, in `area` pixels.
    /// The preview gets a pane of its own whenever one can be made without resizing a locked
    /// pane or hiding what the user is looking at:
    ///
    /// 1. Beside the explorer, if its pane is not locked and is wide enough, or below it if
    ///    that is tall enough. The explorer keeps the smaller share.
    /// 2. Otherwise beside or below the roomiest other pane that is not locked and can be
    ///    split the same way. That pane keeps the larger share and its selected tab, so a
    ///    terminal in it stays on screen.
    ///
    /// Splitting shrinks the pane being split, so a locked pane is never split. When no pane
    /// can be, the preview is added as a tab, which resizes nothing. Its best home is the
    /// roomiest unlocked pane that is not showing a terminal, where it is shown. Failing
    /// that it waits, unselected, in an unlocked pane showing a terminal, then in a locked
    /// pane, then in the explorer's own pane, which keeps the tree on screen. Bringing it
    /// forward is then the user's choice.
    pub fn preview_placement(
        &self,
        area: Option<Extent>,
        explorer: PaneId,
        facts: &PaneFacts,
    ) -> Option<PreviewPlacement> {
        if !(facts.locked)(explorer)
            && let Some(axis) = area
                .map(|area| self.pane_extents(area))
                .and_then(|extents| extents.get(&explorer).copied())
                .and_then(preview_axis)
        {
            let ratio = match axis {
                Axis::SideBySide => PREVIEW_SIDE_BY_SIDE_RATIO,
                Axis::Stacked => PREVIEW_STACKED_RATIO,
            };
            return Some(PreviewPlacement::Split {
                target: explorer,
                axis,
                ratio,
            });
        }
        self.placement_beside_work(area, explorer, false, facts)
    }

    /// Where a panel goes when a click in pane `clicked` asks for it and there is no tree to
    /// put it beside. The pane clicked in has a terminal on screen that must stay there, so:
    ///
    /// 1. The roomiest unlocked pane that is wide or tall enough is split, the clicked pane
    ///    included. It keeps `PREVIEW_BESIDE_OTHER_RATIO` of the space and its selected tab,
    ///    and the panel gets the new pane.
    /// 2. Otherwise a tab, which resizes nothing: selected in the roomiest unlocked pane
    ///    that is not showing a terminal, or else waiting, unselected, in another unlocked
    ///    pane, then a locked one, then the clicked pane itself.
    ///
    /// Splitting shrinks the pane being split, so a locked pane is never split.
    pub fn beside_placement(
        &self,
        area: Option<Extent>,
        clicked: PaneId,
        facts: &PaneFacts,
    ) -> Option<PreviewPlacement> {
        self.placement_beside_work(area, clicked, true, facts)
    }

    /// The part of the placement rules that puts a panel beside the work in the window rather
    /// than beside the tree: split the roomiest unlocked pane that can be, else add a tab.
    /// `anchor` is the explorer's pane or the pane clicked in. It is a candidate for the split
    /// only when `anchor_may_split` (the explorer's own pane was tried first and failed), and
    /// is where a waiting tab goes when no other pane can hold it.
    fn placement_beside_work(
        &self,
        area: Option<Extent>,
        anchor: PaneId,
        anchor_may_split: bool,
        facts: &PaneFacts,
    ) -> Option<PreviewPlacement> {
        let extents = area.map(|area| self.pane_extents(area));
        let extent_of = |id: PaneId| {
            extents
                .as_ref()
                .and_then(|extents| extents.get(&id))
                .copied()
        };
        // Without sizes every pane counts the same, so the first one in layout order wins.
        let room = |id: PaneId| extent_of(id).map_or(0.0, |extent| extent.width * extent.height);
        let roomiest = |ids: &[PaneId]| {
            let mut best: Option<(PaneId, f32)> = None;
            for id in ids {
                if best.is_none_or(|(_, most)| room(*id) > most) {
                    best = Some((*id, room(*id)));
                }
            }
            best.map(|(id, _)| id)
        };

        let others: Vec<PaneId> = self
            .pane_ids()
            .into_iter()
            .filter(|id| *id != anchor)
            .collect();
        let splittable: Vec<PaneId> = self
            .pane_ids()
            .into_iter()
            .filter(|id| *id != anchor || anchor_may_split)
            .filter(|id| !(facts.locked)(*id) && extent_of(*id).and_then(preview_axis).is_some())
            .collect();
        if let Some(target) = roomiest(&splittable)
            && let Some(axis) = extent_of(target).and_then(preview_axis)
        {
            return Some(PreviewPlacement::Split {
                target,
                axis,
                ratio: PREVIEW_BESIDE_OTHER_RATIO,
            });
        }

        let unlocked: Vec<PaneId> = others
            .iter()
            .copied()
            .filter(|id| !(facts.locked)(*id))
            .collect();
        let clear: Vec<PaneId> = unlocked
            .iter()
            .copied()
            .filter(|id| !(facts.shows_shell)(*id))
            .collect();
        if let Some(pane) = roomiest(&clear) {
            return Some(PreviewPlacement::Tab {
                pane,
                activate: true,
            });
        }
        let waiting = if unlocked.is_empty() {
            let locked: Vec<PaneId> = others
                .iter()
                .copied()
                .filter(|id| (facts.locked)(*id))
                .collect();
            roomiest(&locked)
        } else {
            roomiest(&unlocked)
        };
        waiting
            .or_else(|| self.pane_ids().contains(&anchor).then_some(anchor))
            .map(|pane| PreviewPlacement::Tab {
                pane,
                activate: false,
            })
    }

    /// What selecting a file in the explorer in pane `explorer` does about the preview.
    ///
    /// With the preference off nothing happens, so a closed preview stays closed. An
    /// existing Preview tab is only brought forward, and never over the explorer's own tab,
    /// which would hide the tree being navigated, or over a terminal, which the user chose
    /// to look at there. The explicit Cmd+Shift+P is how it comes forward in those cases.
    pub fn plan_preview_reveal(
        &self,
        enabled: bool,
        existing: Option<PreviewTab>,
        area: Option<Extent>,
        explorer: PaneId,
        facts: &PaneFacts,
    ) -> PreviewReveal {
        if !enabled {
            return PreviewReveal::Leave;
        }
        match existing {
            Some(tab) if tab.shown || tab.behind_shell || tab.pane == explorer => {
                PreviewReveal::Leave
            }
            Some(tab) => PreviewReveal::Activate(tab.pane),
            None => self
                .preview_placement(area, explorer, facts)
                .map_or(PreviewReveal::Leave, PreviewReveal::Open),
        }
    }

    /// What a click on a link in pane `clicked` does about the panel it asks for (the Preview
    /// of a file, or Files for a folder) when there is no visible tree to put it beside. The
    /// click was a request, so the panel is made, or brought forward, whatever the preference
    /// says; but never in front of a terminal, and above all never in front of the one
    /// clicked: an existing tab is brought forward only where no terminal is selected, and
    /// never in the clicked pane. The keys stay in the terminal either way.
    pub fn plan_beside_reveal(
        &self,
        existing: Option<PreviewTab>,
        area: Option<Extent>,
        clicked: PaneId,
        facts: &PaneFacts,
    ) -> PreviewReveal {
        match existing {
            Some(tab) if tab.shown || tab.behind_shell || tab.pane == clicked => {
                PreviewReveal::Leave
            }
            Some(tab) => PreviewReveal::Activate(tab.pane),
            None => self
                .beside_placement(area, clicked, facts)
                .map_or(PreviewReveal::Leave, PreviewReveal::Open),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelKind {
    Projects,
    Worktrees,
    Files,
    /// The preview of the file selected in the window's Files panel.
    Preview,
    Tasks,
    Shells,
    Usage,
    Settings,
    ProjectSettings,
    Schedules,
}

impl PanelKind {
    /// The name saved layouts and element ids use.
    pub fn name(self) -> &'static str {
        match self {
            Self::Projects => "projects",
            Self::Worktrees => "worktrees",
            Self::Files => "files",
            Self::Preview => "preview",
            Self::Tasks => "tasks",
            Self::Shells => "shells",
            Self::Usage => "usage",
            Self::Settings => "settings",
            Self::ProjectSettings => "project_settings",
            Self::Schedules => "schedules",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SavedTab {
    Shell {
        shell_id: String,
    },
    Panel {
        panel: PanelKind,
    },
    /// A shell of another Mac's RiWork, reached through the paired host `desktop_id`. The
    /// shell belongs to that Mac, so it is never one of this Mac's `shell_ids`.
    RemoteShell {
        desktop_id: String,
        shell_id: String,
    },
}

impl SavedTab {
    pub fn key(&self) -> String {
        match self {
            Self::Shell { shell_id } => format!("shell:{shell_id}"),
            Self::Panel { panel } => format!("panel:{}", panel.name()),
            Self::RemoteShell {
                desktop_id,
                shell_id,
            } => format!("remote:{desktop_id}:{shell_id}"),
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
                    SavedTab::Panel { .. } | SavedTab::RemoteShell { .. } => None,
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
        let mut remote_shells = HashSet::new();
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
                // A host's id is a UUID of this Mac's registry and a shell's of the host's,
                // so only the pair names a tab, and an empty half is no tab at all.
                SavedTab::RemoteShell {
                    desktop_id,
                    shell_id,
                } => {
                    !desktop_id.is_empty()
                        && !shell_id.is_empty()
                        && remote_shells.insert((desktop_id.clone(), shell_id.clone()))
                }
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
                    SavedTab::Panel { .. } | SavedTab::RemoteShell { .. } => None,
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
            SavedTab::Panel { .. } | SavedTab::RemoteShell { .. } => None,
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

/// Tabs of a kind this build does not know, by the pane that held them, exactly as they were
/// read.
type SkippedTabs = BTreeMap<PaneId, Vec<Value>>;

#[derive(Debug)]
enum SavedEntry {
    Layout {
        layout: Box<ProjectLayout>,
        /// Left out of `layout`, which opens without them, and written back after each
        /// pane's own tabs, so an older build cannot erase what a newer one saved.
        skipped: SkippedTabs,
    },
    Unreadable {
        raw: Value,
        reason: String,
    },
}

impl Serialize for SavedEntry {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Layout { layout, skipped } => {
                if skipped.is_empty() {
                    return layout.serialize(serializer);
                }
                let mut value = serde_json::to_value(layout).map_err(serde::ser::Error::custom)?;
                for (id, tabs) in skipped {
                    // A pane that was closed since takes its unknown tabs with it.
                    if let Some(saved) = value
                        .pointer_mut(&format!("/panes/{id}/tabs"))
                        .and_then(Value::as_array_mut)
                    {
                        saved.extend(tabs.iter().cloned());
                    }
                }
                value.serialize(serializer)
            }
            Self::Unreadable { raw, .. } => raw.serialize(serializer),
        }
    }
}

impl SavedEntry {
    fn layout(&self) -> Result<ProjectLayout, String> {
        match self {
            Self::Layout { layout, .. } => {
                let mut layout = (**layout).clone();
                layout.normalize()?;
                Ok(layout)
            }
            Self::Unreadable { reason, .. } => Err(unreadable_entry_message(reason)),
        }
    }

    fn window_size(&self) -> Option<WindowSize> {
        match self {
            Self::Layout { layout, .. } => layout.window_size,
            Self::Unreadable { raw, .. } => raw
                .get("window_size")
                .and_then(|size| WindowSize::deserialize(size).ok()),
        }
        .filter(WindowSize::is_valid)
    }

    /// Read one project's layout. A tab this build cannot read, such as a panel kind that
    /// only a newer one has, is skipped and kept instead of making the whole layout
    /// unreadable. Anything else that does not parse leaves the entry as it is, untouched.
    fn parse(raw: Value) -> Self {
        // Nearly always every tab is known, and then nothing is copied.
        if let Ok(layout) = ProjectLayout::deserialize(&raw) {
            return Self::Layout {
                layout: Box::new(layout),
                skipped: SkippedTabs::new(),
            };
        }
        let mut readable = raw.clone();
        let skipped = take_unreadable_tabs(&mut readable);
        match ProjectLayout::deserialize(&readable) {
            Ok(layout) => Self::Layout {
                layout: Box::new(layout),
                skipped,
            },
            Err(error) => Self::Unreadable {
                reason: error.to_string(),
                raw,
            },
        }
    }
}

/// Move every tab that is not a `SavedTab` out of the layout's panes.
fn take_unreadable_tabs(layout: &mut Value) -> SkippedTabs {
    let mut skipped = SkippedTabs::new();
    let Some(panes) = layout.get_mut("panes").and_then(Value::as_object_mut) else {
        return skipped;
    };
    for (id, pane) in panes {
        let (Ok(id), Some(tabs)) = (
            id.parse::<PaneId>(),
            pane.get_mut("tabs").and_then(Value::as_array_mut),
        ) else {
            continue;
        };
        let (readable, unreadable): (Vec<Value>, Vec<Value>) = std::mem::take(tabs)
            .into_iter()
            .partition(|tab| SavedTab::deserialize(tab).is_ok());
        *tabs = readable;
        if !unreadable.is_empty() {
            skipped.insert(id, unreadable);
        }
    }
    skipped
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
            .map(|(id, raw)| (id, SavedEntry::parse(raw)))
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
        // What a newer build saved in a pane that still exists is carried over.
        let mut skipped = match layouts.projects.remove(project_id) {
            Some(SavedEntry::Layout { skipped, .. }) => skipped,
            _ => SkippedTabs::new(),
        };
        let panes = layout.layout.pane_ids();
        skipped.retain(|id, _| panes.contains(id));
        layouts.projects.insert(
            project_id.to_owned(),
            SavedEntry::Layout {
                layout: Box::new(layout),
                skipped,
            },
        );
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
    fn ratios_a_resize_leaves_on_narrow_locked_panes_survive_a_restart() {
        let directory = TestDirectory::new();
        let mut layout = saved_layout();
        if let Layout::Split { ratio, second, .. } = &mut layout.layout {
            *ratio = 0.05;
            if let Layout::Split { ratio, .. } = second.as_mut() {
                *ratio = 0.97;
            }
        }
        directory.store().save("project-narrow", &layout).unwrap();
        let restored = directory.store().load("project-narrow").unwrap().unwrap();
        assert_eq!(restored.layout.ratio_at(&[]), Some(0.05));
        assert_eq!(restored.layout.ratio_at(&[true]), Some(0.97));
        // Outside what a resize produces, and for dragging, the old range holds.
        assert_eq!(stored_ratio(0.01), 0.1);
        assert_eq!(stored_ratio(0.995), 0.9);
        assert_eq!(stored_ratio(f32::NAN), 0.5);
        assert!(layout.layout.set_ratio(&[], 0.05));
        assert_eq!(layout.layout.ratio_at(&[]), Some(0.1));
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
        // Not a tab this time: a split shape that only a newer build writes.
        let mut reshaped = serde_json::to_value(saved_layout()).unwrap();
        reshaped["layout"] = serde_json::json!({"grid": {"columns": 3}});
        let mut no_active_pane = serde_json::to_value(saved_layout()).unwrap();
        no_active_pane
            .as_object_mut()
            .unwrap()
            .remove("active_pane");
        directory.write(
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "future_top_level": {"keep": [1, 2, 3]},
                "projects": {
                    "old": saved_layout(),
                    "shape": reshaped,
                    "pane": no_active_pane,
                },
            }))
            .unwrap(),
        );

        // Only the affected projects fall back; the rest of the file still loads.
        assert_eq!(store.load("old").unwrap(), Some(saved_layout()));
        for id in ["shape", "pane"] {
            let error = store.load(id).unwrap_err();
            assert!(error.contains("cannot be read"), "{error}");
        }
        assert!(store.load("absent").unwrap().is_none());
        // The window size survives even when the rest of the entry does not parse.
        assert_eq!(store.window_size("shape"), WindowSize::new(1440.0, 900.0));

        // An older build saving its own projects leaves the newer data alone,
        // and refuses to replace an entry it could not read.
        let mut changed = saved_layout();
        changed.selected_task_id = Some("another-task".to_owned());
        store.save("old", &changed).unwrap();
        for id in ["shape", "pane"] {
            let error = store.save(id, &saved_layout()).unwrap_err();
            assert!(error.contains("cannot be read"), "{error}");
        }
        store.save("brand-new", &saved_layout()).unwrap();
        let file = directory.read_value();
        assert_eq!(file["projects"]["shape"], reshaped);
        assert_eq!(file["projects"]["pane"], no_active_pane);
        assert_eq!(
            file["future_top_level"],
            serde_json::json!({"keep": [1, 2, 3]})
        );
        assert_eq!(store.load("old").unwrap(), Some(changed));
        assert_eq!(store.load("brand-new").unwrap(), Some(saved_layout()));
    }

    #[test]
    fn tabs_of_an_unknown_kind_are_skipped_and_the_rest_of_the_layout_still_loads() {
        let directory = TestDirectory::new();
        let store = directory.store();
        let quantum = serde_json::json!({"kind": "panel", "panel": "quantum"});
        let browser = serde_json::json!({"kind": "browser", "url": "https://example.com"});
        let mut pane_four = saved_layout();
        pane_four.panes.get_mut(&4).unwrap().tabs.insert(
            0,
            SavedTab::Panel {
                panel: PanelKind::Files,
            },
        );
        pane_four.normalize().unwrap();
        let mut stored = serde_json::to_value(&pane_four).unwrap();
        // The unknown tabs sit among the known ones, and one of them was the selected tab.
        let tabs = stored["panes"]["4"]["tabs"].as_array_mut().unwrap();
        tabs.insert(0, quantum.clone());
        tabs.push(browser.clone());
        stored["panes"]["4"]["active_tab_key"] = serde_json::json!("panel:quantum");
        directory.write(
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "projects": {"newer": stored},
            }))
            .unwrap(),
        );

        // Nothing unreadable reaches the caller, and a selection that named an unknown tab
        // falls to the first tab the pane can show.
        let loaded = store.load("newer").unwrap().unwrap();
        let tabs = &loaded.panes[&4].tabs;
        assert!(tabs.iter().any(|tab| tab.key() == "panel:files"));
        assert_eq!(tabs.len(), pane_four.panes[&4].tabs.len());
        assert_eq!(loaded.layout, pane_four.layout);
        assert_eq!(
            loaded.panes[&4].active_tab_key.as_deref(),
            Some("panel:files")
        );
        assert_eq!(loaded.panes[&8], pane_four.panes[&8]);
        assert_eq!(store.window_size("newer"), WindowSize::new(1440.0, 900.0));

        // Saving what this build has changed does not erase the tabs it could not read.
        let mut changed = loaded.clone();
        changed.selected_task_id = Some("another-task".to_owned());
        store.save("newer", &changed).unwrap();
        let file = directory.read_value();
        let saved = file["projects"]["newer"]["panes"]["4"]["tabs"]
            .as_array()
            .unwrap();
        assert!(
            saved.contains(&quantum) && saved.contains(&browser),
            "{saved:?}"
        );
        assert_eq!(saved.len(), tabs.len() + 2);
        assert_eq!(store.load("newer").unwrap(), Some(changed.clone()));

        // They are written once, not once more per save, and a closed pane takes its own away.
        store.save("newer", &changed).unwrap();
        store.save("newer", &changed).unwrap();
        let again = directory.read_value();
        assert_eq!(
            again["projects"]["newer"]["panes"]["4"]["tabs"],
            file["projects"]["newer"]["panes"]["4"]["tabs"]
        );
        let mut closed = changed.clone();
        closed.layout = Layout::Pane(8);
        closed.active_pane = 8;
        store.save("newer", &closed).unwrap();
        let file = directory.read_value();
        assert!(file["projects"]["newer"]["panes"].get("4").is_none());
        assert!(!file.to_string().contains("quantum"));
    }

    #[test]
    fn an_unknown_panel_leaves_the_shells_beside_it_untouched() {
        let directory = TestDirectory::new();
        let store = directory.store();
        let stored = newer_layout(serde_json::json!({"kind": "panel", "panel": "quantum"}));
        directory.write(
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "projects": {"newer": stored},
            }))
            .unwrap(),
        );
        let loaded = store.load("newer").unwrap().unwrap();
        assert_eq!(loaded, saved_layout());
    }

    fn remote(desktop: &str, shell_id: &str) -> SavedTab {
        SavedTab::RemoteShell {
            desktop_id: desktop.into(),
            shell_id: shell_id.into(),
        }
    }

    #[test]
    fn a_remote_shell_tab_has_a_fixed_json_shape_and_key() {
        let tab = remote("host-1", "shell-9");
        assert_eq!(tab.key(), "remote:host-1:shell-9");
        let json = serde_json::json!({
            "kind": "remote_shell",
            "desktop_id": "host-1",
            "shell_id": "shell-9",
        });
        assert_eq!(serde_json::to_value(&tab).unwrap(), json);
        assert_eq!(serde_json::from_value::<SavedTab>(json).unwrap(), tab);
        // The same shell id on another host is another tab.
        assert_ne!(tab.key(), remote("host-2", "shell-9").key());
    }

    #[test]
    fn remote_shell_tabs_round_trip_and_are_not_this_macs_shells() {
        let directory = TestDirectory::new();
        let store = directory.store();
        let mut layout = saved_layout();
        layout.panes.insert(
            4,
            pane(
                vec![
                    shell("shell-a"),
                    remote("host-1", "shell-9"),
                    panel(PanelKind::Files),
                    remote("host-2", "shell-9"),
                ],
                1,
            ),
        );
        layout.normalize().unwrap();
        // Only this Mac's own shells are listed as shells; a remote id would otherwise be
        // adopted, detached or looked up in the local session registry.
        assert_eq!(layout.panes[&4].shell_ids, ["shell-a"]);
        assert_eq!(layout.panes[&4].active_shell_id, None);
        assert_eq!(
            layout.panes[&4].active_tab_key.as_deref(),
            Some("remote:host-1:shell-9")
        );

        store.save("project-a", &layout).unwrap();
        let file = directory.read_value();
        assert!(
            file["projects"]["project-a"]["panes"]["4"]["tabs"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!({
                    "kind": "remote_shell",
                    "desktop_id": "host-2",
                    "shell_id": "shell-9",
                }))
        );
        assert_eq!(store.load("project-a").unwrap(), Some(layout));
    }

    #[test]
    fn duplicate_or_empty_remote_tabs_are_dropped_by_normalization() {
        let mut layout = saved_layout();
        layout.panes.insert(
            4,
            pane(
                vec![
                    remote("host-1", "shell-9"),
                    remote("host-1", "shell-9"),
                    remote("", "shell-9"),
                    remote("host-1", ""),
                    remote("host-2", "shell-9"),
                ],
                0,
            ),
        );
        // Another pane showing the same remote shell does not get it twice either.
        layout
            .panes
            .get_mut(&8)
            .unwrap()
            .tabs
            .push(remote("host-2", "shell-9"));
        layout.normalize().unwrap();
        assert_eq!(
            layout.panes[&4]
                .tabs
                .iter()
                .map(SavedTab::key)
                .collect::<Vec<_>>(),
            ["remote:host-1:shell-9", "remote:host-2:shell-9"]
        );
        assert!(
            !layout.panes[&8]
                .tabs
                .iter()
                .any(|tab| matches!(tab, SavedTab::RemoteShell { .. }))
        );
    }

    #[test]
    fn remote_tabs_parse_beside_tabs_of_a_kind_that_is_still_unknown() {
        let browser = serde_json::json!({"kind": "browser", "url": "https://example.com"});
        let remote_json = serde_json::json!({
            "kind": "remote_shell",
            "desktop_id": "host-1",
            "shell_id": "shell-9",
        });
        let mut raw = serde_json::to_value(saved_layout()).unwrap();
        let tabs = raw["panes"]["4"]["tabs"].as_array_mut().unwrap();
        tabs.push(browser.clone());
        tabs.push(remote_json.clone());
        let SavedEntry::Layout { layout, skipped } = SavedEntry::parse(raw) else {
            panic!("a layout with a remote tab must stay readable");
        };
        // The remote tab is a tab of this build; only the unknown one is set aside.
        assert!(layout.panes[&4].tabs.contains(&remote("host-1", "shell-9")));
        assert_eq!(skipped, SkippedTabs::from([(4, vec![browser.clone()])]));
        let written = serde_json::to_value(SavedEntry::Layout { layout, skipped }).unwrap();
        let tabs = written["panes"]["4"]["tabs"].as_array().unwrap();
        assert!(tabs.contains(&remote_json) && tabs.contains(&browser));
    }

    #[test]
    fn a_remote_projects_layout_is_kept_under_its_own_key_beside_the_local_ones() {
        let directory = TestDirectory::new();
        let store = directory.store();
        let key = "remote:host-1:project-9";
        let mut local = saved_layout();
        local.selected_worktree_id = Some("local-worktree".to_owned());
        let mut remote_layout = saved_layout();
        remote_layout.selected_worktree_id = Some("remote-worktree".to_owned());
        remote_layout.panes.insert(
            4,
            pane(
                vec![remote("host-1", "shell-9"), panel(PanelKind::Worktrees)],
                0,
            ),
        );
        remote_layout.normalize().unwrap();
        store.save("project-a", &local).unwrap();
        store.save(key, &remote_layout).unwrap();

        // Each is its own: saving or loading one never reads or replaces the other, and a
        // remote project's id is not a local one's.
        assert_eq!(store.load(key).unwrap(), Some(remote_layout.clone()));
        assert_eq!(store.load("project-a").unwrap(), Some(local.clone()));
        let mut changed = local.clone();
        changed.selected_task_id = Some("another-task".to_owned());
        store.save("project-a", &changed).unwrap();
        assert_eq!(store.load(key).unwrap(), Some(remote_layout));
        let file = directory.read_value();
        let projects = file["projects"].as_object().unwrap();
        assert!(projects.contains_key(key) && projects.contains_key("project-a"));
    }

    #[test]
    fn a_build_that_predates_remote_tabs_keeps_them_verbatim() {
        // What an older build does with the tab: the kind is unknown to it, so the tab is
        // set aside and written back unchanged. Simulated with a kind no build knows.
        let directory = TestDirectory::new();
        let store = directory.store();
        let remote_json = serde_json::json!({
            "kind": "remote_shell_from_the_future",
            "desktop_id": "host-1",
            "shell_id": "shell-9",
        });
        let stored = newer_layout(remote_json.clone());
        directory.write(
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "projects": {"newer": stored},
            }))
            .unwrap(),
        );
        let mut loaded = store.load("newer").unwrap().unwrap();
        loaded.selected_task_id = Some("another-task".to_owned());
        store.save("newer", &loaded).unwrap();
        let file = directory.read_value();
        assert!(
            file["projects"]["newer"]["panes"]["4"]["tabs"]
                .as_array()
                .unwrap()
                .contains(&remote_json)
        );
    }

    #[test]
    fn the_preview_panel_round_trips_beside_files_and_old_layouts_without_it_still_load() {
        let directory = TestDirectory::new();
        let store = directory.store();
        let mut layout = saved_layout();
        layout.panes.insert(
            4,
            pane(
                vec![
                    panel(PanelKind::Files),
                    shell("shell-a"),
                    panel(PanelKind::Preview),
                ],
                2,
            ),
        );
        layout.normalize().unwrap();
        store.save("project-a", &layout).unwrap();

        let file = directory.read_value();
        assert!(
            file["projects"]["project-a"]["panes"]["4"]["tabs"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!({"kind": "panel", "panel": "preview"}))
        );
        assert_eq!(
            file["projects"]["project-a"]["panes"]["4"]["active_tab_key"],
            "panel:preview"
        );
        let restored = store.load("project-a").unwrap().unwrap();
        assert_eq!(restored, layout);
        assert_eq!(
            restored.panes[&4]
                .tabs
                .iter()
                .map(SavedTab::key)
                .collect::<Vec<_>>(),
            ["panel:files", "shell:shell-a", "panel:preview"]
        );

        // A layout saved before the panel existed holds only Files and reads as it did.
        let mut older = layout.clone();
        older
            .panes
            .insert(4, pane(vec![panel(PanelKind::Files), shell("shell-a")], 0));
        older.normalize().unwrap();
        let stored = serde_json::to_value(&older).unwrap();
        assert!(!stored.to_string().contains("preview"));
        directory.write(
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "projects": {"older": stored},
            }))
            .unwrap(),
        );
        assert_eq!(store.load("older").unwrap(), Some(older));
        assert_eq!(PanelKind::Preview.name(), "preview");
        assert_eq!(
            SavedTab::Panel {
                panel: PanelKind::Preview
            }
            .key(),
            "panel:preview"
        );
    }

    #[test]
    fn there_is_only_ever_one_preview_tab_per_window() {
        let mut layout = saved_layout();
        layout.panes.insert(
            4,
            pane(vec![panel(PanelKind::Preview), shell("shell-a")], 0),
        );
        layout
            .panes
            .insert(8, pane(vec![panel(PanelKind::Preview)], 0));
        layout.normalize().unwrap();
        let previews = layout
            .panes
            .values()
            .flat_map(|pane| &pane.tabs)
            .filter(|tab| tab.key() == "panel:preview")
            .count();
        assert_eq!(previews, 1);
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

#[cfg(test)]
mod preserve_locked_tests {
    use super::*;
    use std::collections::BTreeMap;

    fn area(width: f32, height: f32) -> Extent {
        Extent { width, height }
    }

    fn pane(id: PaneId) -> Layout {
        Layout::Pane(id)
    }

    fn split(axis: Axis, ratio: f32, first: Layout, second: Layout) -> Layout {
        Layout::Split {
            axis,
            ratio,
            first: Box::new(first),
            second: Box::new(second),
        }
    }

    fn side(ratio: f32, first: Layout, second: Layout) -> Layout {
        split(Axis::SideBySide, ratio, first, second)
    }

    fn stack(ratio: f32, first: Layout, second: Layout) -> Layout {
        split(Axis::Stacked, ratio, first, second)
    }

    /// The ratio that gives a split's first child `pixels` of `total` along its axis.
    fn ratio_of(pixels: f32, total: f32) -> f32 {
        pixels / (total - DIVIDER_THICKNESS)
    }

    /// Every pane's pixel size, laid out the way the renderer does it.
    fn sizes(layout: &Layout, area: Extent) -> BTreeMap<PaneId, Extent> {
        layout.pane_extents(area)
    }

    fn resize(layout: &mut Layout, old: Extent, new: Extent, locked: &[PaneId]) -> bool {
        layout.preserve_locked(old, new, &|id| locked.contains(&id), MIN_PANE_EXTENT)
    }

    fn assert_size(sizes: &BTreeMap<PaneId, Extent>, id: PaneId, width: f32, height: f32) {
        let actual = sizes[&id];
        assert!(
            (actual.width - width).abs() < 0.01 && (actual.height - height).abs() < 0.01,
            "pane {id}: {}x{} but wanted {width}x{height}",
            actual.width,
            actual.height,
        );
    }

    #[test]
    fn a_locked_left_nav_keeps_its_width_and_its_height_follows_the_window() {
        let start = area(1200.0, 800.0);
        let mut layout = side(
            ratio_of(260.0, 1200.0),
            pane(1),
            stack(0.5, pane(2), pane(3)),
        );
        let mut current = start;
        for next in [
            area(1600.0, 900.0),
            area(900.0, 700.0),
            area(900.0, 500.0),
            start,
        ] {
            resize(&mut layout, current, next, &[1]);
            current = next;
            let panes = sizes(&layout, next);
            // The nav has no vertical neighbour, so it spans the window's height.
            assert_size(&panes, 1, 260.0, next.height);
            let right = next.width - 260.0 - DIVIDER_THICKNESS;
            let each = (next.height - DIVIDER_THICKNESS) / 2.0;
            assert_size(&panes, 2, right, each);
            assert_size(&panes, 3, right, each);
        }
    }

    #[test]
    fn a_locked_pane_with_neighbours_on_both_axes_keeps_width_and_height() {
        let start = area(1200.0, 800.0);
        // Column of 695 x (595 | 200), the locked pane at the bottom right.
        let mut layout = side(
            ratio_of(500.0, 1200.0),
            pane(1),
            stack(ratio_of(595.0, 800.0), pane(2), pane(3)),
        );
        let mut current = start;
        for next in [
            area(1500.0, 1000.0),
            area(900.0, 600.0),
            area(1100.0, 900.0),
        ] {
            resize(&mut layout, current, next, &[3]);
            current = next;
            let panes = sizes(&layout, next);
            assert_size(&panes, 3, 695.0, 200.0);
            // Unlocked panes take up the difference on each axis.
            assert_size(
                &panes,
                1,
                next.width - 695.0 - DIVIDER_THICKNESS,
                next.height,
            );
            assert_size(&panes, 2, 695.0, next.height - 200.0 - DIVIDER_THICKNESS);
        }
    }

    #[test]
    fn a_locked_pane_that_is_alone_along_an_axis_follows_that_axis() {
        let start = area(1200.0, 800.0);
        // A locked top bar spans the whole width but has a pane below it.
        let mut layout = stack(ratio_of(120.0, 800.0), pane(1), side(0.5, pane(2), pane(3)));
        let next = area(1500.0, 700.0);
        assert!(resize(&mut layout, start, next, &[1]));
        let panes = sizes(&layout, next);
        assert_size(&panes, 1, 1500.0, 120.0);
        let below = 700.0 - 120.0 - DIVIDER_THICKNESS;
        assert_size(&panes, 2, (1500.0 - DIVIDER_THICKNESS) / 2.0, below);
        assert_size(&panes, 3, (1500.0 - DIVIDER_THICKNESS) / 2.0, below);

        // With nothing beside or below it, a locked pane fills the window both ways.
        let mut only = pane(1);
        assert!(!resize(&mut only, start, next, &[1]));
        assert_eq!(only, pane(1));
    }

    #[test]
    fn a_locked_footer_keeps_its_height_and_its_width_follows() {
        let start = area(1200.0, 800.0);
        let mut layout = stack(ratio_of(560.0, 800.0), pane(1), pane(2));
        let next = area(1000.0, 1000.0);
        resize(&mut layout, start, next, &[2]);
        let panes = sizes(&layout, next);
        assert_size(&panes, 2, 1000.0, 800.0 - 560.0 - DIVIDER_THICKNESS);
        assert_size(&panes, 1, 1000.0, 1000.0 - 235.0 - DIVIDER_THICKNESS);
    }

    #[test]
    fn a_column_holding_a_locked_pane_keeps_its_width() {
        let start = area(1200.0, 800.0);
        // Locked nav above unlocked files in one column, a terminal beside it.
        let mut layout = side(
            ratio_of(260.0, 1200.0),
            stack(ratio_of(300.0, 800.0), pane(1), pane(2)),
            pane(3),
        );
        let next = area(1500.0, 1000.0);
        resize(&mut layout, start, next, &[1]);
        let panes = sizes(&layout, next);
        assert_size(&panes, 1, 260.0, 300.0);
        assert_size(&panes, 2, 260.0, 1000.0 - 300.0 - DIVIDER_THICKNESS);
        assert_size(&panes, 3, 1500.0 - 260.0 - DIVIDER_THICKNESS, 1000.0);
    }

    #[test]
    fn two_locked_or_two_unlocked_children_stay_proportional() {
        let start = area(1200.0, 800.0);
        let next = area(1800.0, 500.0);
        for locked in [&[1, 2][..], &[][..]] {
            let mut layout = side(0.3, pane(1), pane(2));
            assert!(!resize(&mut layout, start, next, locked));
            assert_eq!(layout.ratio_at(&[]), Some(0.3));
            let panes = sizes(&layout, next);
            let left = (1800.0 - DIVIDER_THICKNESS) * 0.3;
            assert_size(&panes, 1, left, 500.0);
            assert_size(&panes, 2, 1800.0 - left - DIVIDER_THICKNESS, 500.0);
        }
        // Nothing locked anywhere: the whole tree is left exactly as it was.
        let mut tree = side(
            0.3,
            pane(1),
            stack(0.6, pane(2), side(0.4, pane(3), pane(4))),
        );
        let before = tree.clone();
        assert!(!resize(&mut tree, start, next, &[]));
        assert_eq!(tree, before);
    }

    #[test]
    fn a_split_of_only_locked_panes_behaves_as_one_locked_pane() {
        let start = area(1200.0, 800.0);
        let next = area(1600.0, 800.0);
        // Panes 1 and 2 are locked, side by side, next to unlocked pane 3.
        let mut layout = side(
            ratio_of(500.0, 1200.0),
            side(0.4, pane(1), pane(2)),
            pane(3),
        );
        resize(&mut layout, start, next, &[1, 2]);
        let panes = sizes(&layout, next);
        assert_size(&panes, 1, (500.0 - DIVIDER_THICKNESS) * 0.4, 800.0);
        assert_size(&panes, 2, (500.0 - DIVIDER_THICKNESS) * 0.6, 800.0);
        assert_size(&panes, 3, 1600.0 - 500.0 - DIVIDER_THICKNESS, 800.0);

        // With one of them unlocked the pair can give, so the root stays proportional
        // and the locked pane holds its width inside the pair.
        let mut layout = side(
            ratio_of(500.0, 1200.0),
            side(0.4, pane(1), pane(2)),
            pane(3),
        );
        resize(&mut layout, start, next, &[1]);
        let panes = sizes(&layout, next);
        assert_eq!(layout.ratio_at(&[]), Some(ratio_of(500.0, 1200.0)));
        let pair = (1600.0 - DIVIDER_THICKNESS) * ratio_of(500.0, 1200.0);
        assert_size(&panes, 1, (500.0 - DIVIDER_THICKNESS) * 0.4, 800.0);
        assert_size(
            &panes,
            2,
            pair - (500.0 - DIVIDER_THICKNESS) * 0.4 - DIVIDER_THICKNESS,
            800.0,
        );
    }

    #[test]
    fn a_small_window_takes_from_the_locked_pane_only_as_far_as_it_must() {
        let start = area(1200.0, 800.0);
        let mut layout = side(ratio_of(260.0, 1200.0), pane(1), pane(2));
        let narrow = area(300.0, 800.0);
        resize(&mut layout, start, narrow, &[1]);
        let panes = sizes(&layout, narrow);
        assert_size(&panes, 2, MIN_PANE_EXTENT, 800.0);
        assert_size(
            &panes,
            1,
            300.0 - MIN_PANE_EXTENT - DIVIDER_THICKNESS,
            800.0,
        );

        // Growing again gives the extra to the unlocked pane.
        let wide = area(1000.0, 800.0);
        resize(&mut layout, narrow, wide, &[1]);
        let panes = sizes(&layout, wide);
        assert_size(&panes, 1, 195.0, 800.0);
        assert_size(&panes, 2, 1000.0 - 195.0 - DIVIDER_THICKNESS, 800.0);

        // A flexible side made of two panes needs room for both.
        let mut layout = side(
            ratio_of(260.0, 1200.0),
            pane(1),
            side(0.5, pane(2), pane(3)),
        );
        let narrow = area(400.0, 800.0);
        resize(&mut layout, start, narrow, &[1]);
        let panes = sizes(&layout, narrow);
        let needed = 2.0 * MIN_PANE_EXTENT + DIVIDER_THICKNESS;
        assert_size(&panes, 1, 400.0 - needed - DIVIDER_THICKNESS, 800.0);
        assert_size(&panes, 2, MIN_PANE_EXTENT, 800.0);
        assert_size(&panes, 3, MIN_PANE_EXTENT, 800.0);
    }

    #[test]
    fn a_window_too_small_for_every_minimum_stays_proportional() {
        let start = area(1200.0, 800.0);
        let mut layout = side(
            ratio_of(260.0, 1200.0),
            pane(1),
            side(0.5, pane(2), pane(3)),
        );
        let before = layout.clone();
        assert!(!resize(&mut layout, start, area(250.0, 800.0), &[1]));
        assert_eq!(layout, before);
    }

    #[test]
    fn a_pane_already_under_the_minimum_is_not_pushed_back_up() {
        // The unlocked pane starts at 95 pixels, under the 100 pixel minimum.
        let start = area(360.0, 800.0);
        let base = side(ratio_of(260.0, 360.0), pane(1), pane(2));

        let mut layout = base.clone();
        let wider = area(400.0, 800.0);
        resize(&mut layout, start, wider, &[1]);
        let panes = sizes(&layout, wider);
        assert_size(&panes, 1, 260.0, 800.0);
        assert_size(&panes, 2, 135.0, 800.0);

        let mut layout = base;
        let narrower = area(350.0, 800.0);
        resize(&mut layout, start, narrower, &[1]);
        let panes = sizes(&layout, narrower);
        assert_size(&panes, 2, 95.0, 800.0);
        assert_size(&panes, 1, 250.0, 800.0);
    }

    #[test]
    fn a_locked_nav_beside_a_locked_footer_keeps_its_width_and_the_footer_follows() {
        let start = area(1200.0, 800.0);
        // Locked nav | (editor over a locked footer that spans the region's width).
        let fresh = || {
            side(
                ratio_of(260.0, 1200.0),
                pane(1),
                stack(ratio_of(560.0, 800.0), pane(2), pane(3)),
            )
        };
        for next in [area(1600.0, 1000.0), area(800.0, 600.0)] {
            let mut layout = fresh();
            resize(&mut layout, start, next, &[1, 3]);
            let panes = sizes(&layout, next);
            let region = next.width - 260.0 - DIVIDER_THICKNESS;
            assert_size(&panes, 1, 260.0, next.height);
            assert_size(&panes, 3, region, 235.0);
            assert_size(&panes, 2, region, next.height - 235.0 - DIVIDER_THICKNESS);
        }
    }

    #[test]
    fn resizing_back_and_forth_returns_to_the_same_ratios() {
        // A locked nav, then a region with a locked footer over an unlocked pane and a
        // locked side pane, so three levels of splits each rebalance.
        let start = area(1400.0, 900.0);
        let mut layout = side(
            ratio_of(260.0, 1400.0),
            pane(1),
            stack(
                ratio_of(715.0, 900.0),
                side(ratio_of(830.0, 1135.0), pane(2), pane(4)),
                pane(3),
            ),
        );
        let original = layout.clone();
        let locked = [1, 3, 4];
        let route = [area(1900.0, 1100.0), area(1000.0, 700.0), start];

        let mut once = layout.clone();
        assert!(resize(&mut once, start, route[0], &locked));
        let panes = sizes(&once, route[0]);
        assert_size(&panes, 1, 260.0, 1100.0);
        assert_size(&panes, 4, 300.0, 1100.0 - 180.0 - DIVIDER_THICKNESS);
        assert_size(&panes, 3, 1900.0 - 260.0 - DIVIDER_THICKNESS, 180.0);
        assert_ne!(once.ratio_at(&[]), original.ratio_at(&[]));
        assert_ne!(once.ratio_at(&[true]), original.ratio_at(&[true]));
        assert_ne!(
            once.ratio_at(&[true, false]),
            original.ratio_at(&[true, false])
        );

        let mut current = start;
        for _ in 0..100 {
            for next in route {
                resize(&mut layout, current, next, &locked);
                current = next;
            }
        }
        for path in [&[][..], &[true], &[true, false]] {
            let drift = (layout.ratio_at(path).unwrap() - original.ratio_at(path).unwrap()).abs();
            assert!(drift < 1e-4, "{path:?} drifted by {drift}");
        }
        let panes = sizes(&layout, start);
        let before = sizes(&original, start);
        for id in 1..=4 {
            let (now, was) = (panes[&id], before[&id]);
            assert!(
                (now.width - was.width).abs() < 0.1 && (now.height - was.height).abs() < 0.1,
                "pane {id} moved from {was:?} to {now:?}"
            );
        }
    }

    #[test]
    fn only_real_changes_are_reported() {
        let start = area(1200.0, 800.0);
        let mut layout = side(ratio_of(260.0, 1200.0), pane(1), pane(2));
        assert!(!resize(&mut layout, start, start, &[1]));
        // Only the height changed: the nav already spans it, so no ratio moves.
        assert!(!resize(&mut layout, start, area(1200.0, 900.0), &[1]));
        assert!(resize(&mut layout, start, area(1300.0, 800.0), &[1]));
        // Nothing is locked: today's proportional behaviour, nothing rewritten.
        let mut plain = side(0.4, pane(1), pane(2));
        assert!(!resize(&mut plain, start, area(1300.0, 900.0), &[]));
    }

    #[test]
    fn unusable_sizes_leave_the_layout_alone() {
        let start = area(1200.0, 800.0);
        let base = side(ratio_of(260.0, 1200.0), pane(1), pane(2));
        for bad in [
            area(3.0, 800.0),
            area(0.0, 0.0),
            area(f32::NAN, 800.0),
            area(-50.0, 800.0),
        ] {
            let mut layout = base.clone();
            assert!(!resize(&mut layout, start, bad, &[1]));
            assert!(!resize(&mut layout, bad, start, &[1]));
            assert_eq!(layout, base);
        }
    }

    #[test]
    fn a_narrow_locked_pane_in_a_wide_window_may_go_below_the_drag_limit() {
        let start = area(1200.0, 800.0);
        let mut layout = side(ratio_of(260.0, 1200.0), pane(1), pane(2));
        let ultrawide = area(5000.0, 800.0);
        resize(&mut layout, start, ultrawide, &[1]);
        let ratio = layout.ratio_at(&[]).unwrap();
        assert!((MIN_KEPT_RATIO..0.1).contains(&ratio), "{ratio}");
        assert_size(&sizes(&layout, ultrawide), 1, 260.0, 800.0);
    }
}

#[cfg(test)]
mod preview_placement_tests {
    use super::*;

    fn area(width: f32, height: f32) -> Extent {
        Extent { width, height }
    }

    /// What the placement rules are told about a window's panes.
    struct Panes {
        locked: Vec<PaneId>,
        shells: Vec<PaneId>,
    }

    impl Panes {
        fn new(locked: &[PaneId], shells: &[PaneId]) -> Self {
            Self {
                locked: locked.to_vec(),
                shells: shells.to_vec(),
            }
        }

        fn place(
            &self,
            layout: &Layout,
            area: Option<Extent>,
            explorer: PaneId,
        ) -> Option<PreviewPlacement> {
            let locked = |id: PaneId| self.locked.contains(&id);
            let shows_shell = |id: PaneId| self.shells.contains(&id);
            layout.preview_placement(
                area,
                explorer,
                &PaneFacts {
                    locked: &locked,
                    shows_shell: &shows_shell,
                },
            )
        }

        /// Where a panel asked for by a click in pane `clicked` goes, with no tree to go beside.
        fn place_beside(
            &self,
            layout: &Layout,
            area: Option<Extent>,
            clicked: PaneId,
        ) -> Option<PreviewPlacement> {
            let locked = |id: PaneId| self.locked.contains(&id);
            let shows_shell = |id: PaneId| self.shells.contains(&id);
            layout.beside_placement(
                area,
                clicked,
                &PaneFacts {
                    locked: &locked,
                    shows_shell: &shows_shell,
                },
            )
        }

        /// What a click in pane `clicked` does about a panel with no tree to go beside.
        fn reveal_beside(
            &self,
            layout: &Layout,
            existing: Option<PreviewTab>,
            area: Option<Extent>,
            clicked: PaneId,
        ) -> PreviewReveal {
            let locked = |id: PaneId| self.locked.contains(&id);
            let shows_shell = |id: PaneId| self.shells.contains(&id);
            layout.plan_beside_reveal(
                existing,
                area,
                clicked,
                &PaneFacts {
                    locked: &locked,
                    shows_shell: &shows_shell,
                },
            )
        }

        fn reveal(
            &self,
            layout: &Layout,
            enabled: bool,
            existing: Option<PreviewTab>,
            area: Option<Extent>,
            explorer: PaneId,
        ) -> PreviewReveal {
            let locked = |id: PaneId| self.locked.contains(&id);
            let shows_shell = |id: PaneId| self.shells.contains(&id);
            layout.plan_preview_reveal(
                enabled,
                existing,
                area,
                explorer,
                &PaneFacts {
                    locked: &locked,
                    shows_shell: &shows_shell,
                },
            )
        }
    }

    fn open_tab() -> PreviewTab {
        PreviewTab {
            pane: 2,
            shown: false,
            behind_shell: false,
        }
    }

    /// A left navigation pane (1) and the pane to its right (2), as a fresh window has them.
    fn navigation_and_main() -> Layout {
        let mut layout = Layout::Pane(1);
        assert!(layout.split_with_ratio(1, Axis::SideBySide, 2, false, 0.27));
        layout
    }

    fn split_for(placement: Option<PreviewPlacement>) -> (PaneId, Axis, f32) {
        match placement {
            Some(PreviewPlacement::Split {
                target,
                axis,
                ratio,
            }) => (target, axis, ratio),
            other => panic!("expected a split, got {other:?}"),
        }
    }

    fn tab(pane: PaneId, activate: bool) -> Option<PreviewPlacement> {
        Some(PreviewPlacement::Tab { pane, activate })
    }

    /// Apply a split the way the workspace does and return the pane sizes before and after.
    fn applied(
        layout: &Layout,
        size: Extent,
        placement: Option<PreviewPlacement>,
    ) -> (BTreeMap<PaneId, Extent>, BTreeMap<PaneId, Extent>) {
        let (target, axis, ratio) = split_for(placement);
        let mut after = layout.clone();
        assert!(after.split_with_ratio(target, axis, 99, false, ratio));
        (layout.pane_extents(size), after.pane_extents(size))
    }

    #[test]
    fn a_wide_unlocked_explorer_gets_the_preview_beside_it() {
        // Only the right pane is unlocked, and it is 1,000 px wide.
        let layout = navigation_and_main();
        let size = area(1005.0 + 5.0 + 380.0, 900.0);
        let panes = Panes::new(&[1], &[2]);
        let placement = panes.place(&layout, Some(size), 2);
        let (target, axis, ratio) = split_for(placement);
        assert_eq!((target, axis), (2, Axis::SideBySide));
        assert_eq!(ratio, PREVIEW_SIDE_BY_SIDE_RATIO);

        // Applying it with the existing split logic leaves every other pane's size alone and
        // gives both halves of the explorer's old pane room.
        let (before, after) = applied(&layout, size, placement);
        assert_eq!(after[&1], before[&1]);
        assert_eq!(after[&2].height, before[&2].height);
        assert!(
            (after[&2].width + after[&99].width + DIVIDER_THICKNESS - before[&2].width).abs()
                < 0.01
        );
        assert!(after[&2].width >= MIN_PANE_EXTENT && after[&99].width >= MIN_PANE_EXTENT);
        assert!(
            after[&99].width > after[&2].width,
            "the preview gets the larger share"
        );
    }

    #[test]
    fn the_width_threshold_is_where_the_old_in_panel_split_went_side_by_side() {
        let layout = Layout::Pane(1);
        let panes = Panes::new(&[], &[]);
        let beside = |width| panes.place(&layout, Some(area(width, 400.0)), 1);
        assert_eq!(
            split_for(beside(PREVIEW_SIDE_BY_SIDE_MIN_WIDTH)).1,
            Axis::SideBySide
        );
        // 400 px tall is too short to stack, so one pixel narrower has nowhere to go but a
        // tab waiting in the explorer's own pane.
        assert_eq!(beside(PREVIEW_SIDE_BY_SIDE_MIN_WIDTH - 1.0), tab(1, false));
        // Each half of the narrowest split is still comfortably above the drag limit.
        let narrowest = PREVIEW_SIDE_BY_SIDE_MIN_WIDTH - DIVIDER_THICKNESS;
        assert!(narrowest * PREVIEW_SIDE_BY_SIDE_RATIO >= MIN_PANE_EXTENT);
        assert!(narrowest * (1.0 - PREVIEW_SIDE_BY_SIDE_RATIO) >= MIN_PANE_EXTENT);
        assert!(narrowest * (1.0 - PREVIEW_BESIDE_OTHER_RATIO) >= MIN_PANE_EXTENT);
    }

    #[test]
    fn a_narrow_but_tall_explorer_gets_the_preview_below_it() {
        let layout = Layout::Pane(1);
        let panes = Panes::new(&[], &[]);
        let (target, axis, ratio) = split_for(panes.place(
            &layout,
            Some(area(
                PREVIEW_SIDE_BY_SIDE_MIN_WIDTH - 1.0,
                PREVIEW_STACKED_MIN_HEIGHT,
            )),
            1,
        ));
        assert_eq!(
            (target, axis, ratio),
            (1, Axis::Stacked, PREVIEW_STACKED_RATIO)
        );
        let narrow_short = area(
            PREVIEW_SIDE_BY_SIDE_MIN_WIDTH - 1.0,
            PREVIEW_STACKED_MIN_HEIGHT - 1.0,
        );
        assert_eq!(panes.place(&layout, Some(narrow_short), 1), tab(1, false));

        // The new pane is the second child, so it lands under the explorer.
        let size = area(500.0, PREVIEW_STACKED_MIN_HEIGHT);
        let (_, after) = applied(
            &layout,
            size,
            panes.place(&layout, Some(area(500.0, PREVIEW_STACKED_MIN_HEIGHT)), 1),
        );
        assert_eq!(after[&1].width, 500.0);
        assert_eq!(after[&99].width, 500.0);
        // Room for the explorer's own chrome and a list of rows above the preview.
        assert!(after[&1].height >= 330.0 && after[&99].height >= 330.0);
    }

    #[test]
    fn side_by_side_wins_when_both_would_fit() {
        let layout = Layout::Pane(1);
        let panes = Panes::new(&[], &[]);
        let placement = panes.place(&layout, Some(area(1400.0, 900.0)), 1);
        assert_eq!(split_for(placement).1, Axis::SideBySide);
    }

    #[test]
    fn a_locked_nav_pane_next_to_a_wide_shell_pane_splits_the_shell_pane_to_the_right() {
        // The default window: Files in the locked navigation pane, a shell to its right.
        let layout = navigation_and_main();
        let size = area(1600.0, 900.0);
        let panes = Panes::new(&[1], &[2]);
        let placement = panes.place(&layout, Some(size), 1);
        let (target, axis, ratio) = split_for(placement);
        assert_eq!((target, axis), (2, Axis::SideBySide));
        assert_eq!(ratio, PREVIEW_BESIDE_OTHER_RATIO);
        assert!(ratio > 0.5, "the shell keeps the larger share");

        // The locked pane keeps its exact size, the shell pane stays the larger half and
        // keeps its tab on screen (a split never touches a pane's tabs), and the preview
        // pane is usable.
        let (before, after) = applied(&layout, size, placement);
        assert_eq!(after[&1], before[&1]);
        assert!(after[&2].width > after[&99].width);
        assert!(after[&99].width >= 245.0 && after[&2].width >= MIN_PANE_EXTENT);
        assert_eq!(after[&2].height, before[&2].height);
        assert!(
            (after[&2].width + after[&99].width + DIVIDER_THICKNESS - before[&2].width).abs()
                < 0.01
        );
    }

    #[test]
    fn a_tall_narrow_shell_pane_is_split_below() {
        // 800 px wide: the shell pane is about 580 px, too narrow to split side by side.
        let layout = navigation_and_main();
        let size = area(800.0, 900.0);
        assert!(layout.pane_extents(size)[&2].width < PREVIEW_SIDE_BY_SIDE_MIN_WIDTH);
        let panes = Panes::new(&[1], &[2]);
        let placement = panes.place(&layout, Some(size), 1);
        let (target, axis, ratio) = split_for(placement);
        assert_eq!(
            (target, axis, ratio),
            (2, Axis::Stacked, PREVIEW_BESIDE_OTHER_RATIO)
        );

        let (before, after) = applied(&layout, size, placement);
        assert_eq!(after[&1], before[&1]);
        assert_eq!(after[&2].width, before[&2].width);
        assert_eq!(after[&99].width, before[&2].width);
        assert!(after[&2].height > after[&99].height);
        assert!(after[&99].height >= 275.0);

        // One pixel too short for that, and the shell pane is not split.
        let short = area(800.0, PREVIEW_STACKED_MIN_HEIGHT - 1.0);
        assert!(!matches!(
            panes.place(&layout, Some(short), 1),
            Some(PreviewPlacement::Split { .. })
        ));
    }

    #[test]
    fn the_roomiest_pane_that_can_be_split_is_the_one_split_and_locked_ones_never_are() {
        // Navigation (1, locked) | a small column (2) over a large pane (3) | explorer pane (4, locked)
        let mut layout = Layout::Pane(1);
        assert!(layout.split_with_ratio(1, Axis::SideBySide, 4, false, 0.2));
        assert!(layout.split_with_ratio(4, Axis::SideBySide, 2, true, 0.5));
        assert!(layout.split_with_ratio(2, Axis::Stacked, 3, false, 0.2));
        let size = area(2000.0, 1000.0);
        let extents = layout.pane_extents(size);
        assert!(extents[&3].width * extents[&3].height > extents[&2].width * extents[&2].height);
        let panes = Panes::new(&[1, 4], &[2, 3]);
        assert_eq!(split_for(panes.place(&layout, Some(size), 4)).0, 3);
        // A larger pane that is locked is passed over.
        let panes = Panes::new(&[1, 3, 4], &[2, 3]);
        assert_eq!(split_for(panes.place(&layout, Some(size), 4)).0, 2);
        // An explorer pane that is unlocked but too small to split yields to another pane,
        // and is not a candidate itself.
        let small = area(1000.0, 400.0);
        let extents = layout.pane_extents(small);
        assert!(extents[&2].width < PREVIEW_SIDE_BY_SIDE_MIN_WIDTH);
        let panes = Panes::new(&[1], &[3, 4]);
        let placement = panes.place(&layout, Some(small), 2);
        assert!(!matches!(
            placement,
            Some(PreviewPlacement::Split { target: 2, .. })
        ));
    }

    #[test]
    fn when_nothing_can_be_split_the_preview_is_a_tab_that_never_hides_a_terminal() {
        let layout = navigation_and_main();
        let small = Some(area(700.0, 500.0));
        // The shell pane is too small to split: the tab waits, unselected, so the terminal
        // stays on screen.
        let panes = Panes::new(&[1], &[2]);
        assert_eq!(panes.place(&layout, small, 1), tab(2, false));
        // The same without sizes, as before the first frame.
        assert_eq!(panes.place(&layout, None, 1), tab(2, false));
        // If that pane is showing something other than a terminal, the tab is shown there.
        let panes = Panes::new(&[1], &[]);
        assert_eq!(panes.place(&layout, small, 1), tab(2, true));

        // Everything locked: still a tab, never selected, and never in front of a terminal
        // or over the tree.
        let panes = Panes::new(&[1, 2], &[2]);
        assert_eq!(
            panes.place(&layout, Some(area(1600.0, 900.0)), 1),
            tab(2, false)
        );
        let panes = Panes::new(&[1, 2], &[]);
        assert_eq!(
            panes.place(&layout, Some(area(1600.0, 900.0)), 1),
            tab(2, false)
        );
    }

    #[test]
    fn the_tab_prefers_an_unlocked_pane_without_a_terminal_and_then_the_roomier_one() {
        // Three unlocked panes in a row after the locked navigation pane: 2, 3 and 4.
        let mut layout = navigation_and_main();
        assert!(layout.split_with_ratio(2, Axis::SideBySide, 3, false, 0.5));
        assert!(layout.split_with_ratio(3, Axis::SideBySide, 4, false, 0.5));
        let small_area = area(1200.0, 400.0);
        let small = Some(small_area);
        let extents = layout.pane_extents(small_area);
        assert!(
            extents
                .values()
                .all(|extent| extent.width < PREVIEW_SIDE_BY_SIDE_MIN_WIDTH)
        );
        // Terminals in 2 and 3; 4 shows a panel and is the only one the tab can be selected in.
        let panes = Panes::new(&[1], &[2, 3]);
        assert_eq!(panes.place(&layout, small, 1), tab(4, true));
        // With terminals in all three, it waits in the roomiest of them.
        let panes = Panes::new(&[1], &[2, 3, 4]);
        let roomiest = [2, 3, 4]
            .into_iter()
            .max_by(|a, b| {
                let room = |id: &PaneId| extents[id].width * extents[id].height;
                room(a).partial_cmp(&room(b)).unwrap().then(b.cmp(a))
            })
            .unwrap();
        assert_eq!(panes.place(&layout, small, 1), tab(roomiest, false));
        // A locked pane only takes it when no unlocked pane could.
        let panes = Panes::new(&[1, 2, 3], &[2, 3, 4]);
        assert_eq!(panes.place(&layout, small, 1), tab(4, false));
    }

    #[test]
    fn a_lone_explorer_pane_that_cannot_be_split_keeps_the_tab_waiting_beside_the_tree() {
        let alone = Layout::Pane(1);
        let panes = Panes::new(&[], &[]);
        assert_eq!(
            panes.place(&alone, Some(area(500.0, 300.0)), 1),
            tab(1, false)
        );
        let locked = Panes::new(&[1], &[]);
        assert_eq!(
            locked.place(&alone, Some(area(1600.0, 900.0)), 1),
            tab(1, false)
        );
    }

    #[test]
    fn selecting_a_file_reuses_the_preview_tab_and_brings_it_forward_without_hiding_what_matters() {
        let layout = navigation_and_main();
        let size = Some(area(1600.0, 900.0));
        let panes = Panes::new(&[1], &[2]);
        let reveal = |enabled, existing| panes.reveal(&layout, enabled, existing, size, 1);

        // No tab yet: open one where the placement rules say.
        assert_eq!(
            reveal(true, None),
            PreviewReveal::Open(PreviewPlacement::Split {
                target: 2,
                axis: Axis::SideBySide,
                ratio: PREVIEW_BESIDE_OTHER_RATIO,
            })
        );
        // A tab hidden behind another non-terminal tab becomes that pane's selected tab.
        assert_eq!(reveal(true, Some(open_tab())), PreviewReveal::Activate(2));
        // One that is already showing is left alone.
        assert_eq!(
            reveal(
                true,
                Some(PreviewTab {
                    shown: true,
                    ..open_tab()
                })
            ),
            PreviewReveal::Leave
        );
        // One hidden behind a terminal stays hidden: the user chose to look at the shell.
        assert_eq!(
            reveal(
                true,
                Some(PreviewTab {
                    behind_shell: true,
                    ..open_tab()
                })
            ),
            PreviewReveal::Leave
        );
        // One tabbed next to Files in the explorer's own pane is not brought forward, or
        // the tree the user is moving through would disappear.
        assert_eq!(
            reveal(
                true,
                Some(PreviewTab {
                    pane: 1,
                    ..open_tab()
                })
            ),
            PreviewReveal::Leave
        );
        // A Preview tab in a locked pane can still be selected, which is not a resize.
        let locked_preview = Panes::new(&[1, 2], &[]);
        assert_eq!(
            locked_preview.reveal(&layout, true, Some(open_tab()), size, 1),
            PreviewReveal::Activate(2)
        );
    }

    #[test]
    fn a_link_click_with_no_tree_splits_the_clicked_pane_and_it_keeps_sixty_percent() {
        // One wide terminal pane and nothing else: it is the only pane that can be split, and it
        // is the one clicked in.
        let layout = Layout::Pane(1);
        let size = area(1400.0, 900.0);
        let panes = Panes::new(&[], &[1]);
        let placement = panes.place_beside(&layout, Some(size), 1);
        let (target, axis, ratio) = split_for(placement);
        assert_eq!((target, axis), (1, Axis::SideBySide));
        assert_eq!(ratio, PREVIEW_BESIDE_OTHER_RATIO);
        assert_eq!(
            panes.reveal_beside(&layout, None, Some(size), 1),
            PreviewReveal::Open(PreviewPlacement::Split {
                target: 1,
                axis: Axis::SideBySide,
                ratio: PREVIEW_BESIDE_OTHER_RATIO,
            })
        );

        // The terminal's pane is the larger side, with the share the Preview pane uses
        // beside other work, and the new pane is usable.
        let (before, after) = applied(&layout, size, placement);
        let whole = before[&1].width - DIVIDER_THICKNESS;
        assert!((after[&1].width - whole * PREVIEW_BESIDE_OTHER_RATIO).abs() < 0.01);
        assert!(after[&1].width > after[&99].width && after[&99].width >= 245.0);
        assert_eq!(after[&1].height, before[&1].height);

        // A pane too narrow for that but tall enough is split below.
        let tall = area(
            PREVIEW_SIDE_BY_SIDE_MIN_WIDTH - 1.0,
            PREVIEW_STACKED_MIN_HEIGHT,
        );
        let (target, axis, ratio) = split_for(panes.place_beside(&layout, Some(tall), 1));
        assert_eq!(
            (target, axis, ratio),
            (1, Axis::Stacked, PREVIEW_BESIDE_OTHER_RATIO)
        );
    }

    #[test]
    fn the_roomiest_unlocked_pane_is_split_whether_or_not_it_is_the_one_clicked() {
        // Navigation (1, locked) | terminals 2 and 3 side by side, 3 much wider.
        let mut layout = navigation_and_main();
        assert!(layout.split_with_ratio(2, Axis::SideBySide, 3, false, 0.2));
        let size = area(2600.0, 900.0);
        let extents = layout.pane_extents(size);
        assert!(extents[&2].width < PREVIEW_SIDE_BY_SIDE_MIN_WIDTH);
        assert!(extents[&3].width >= PREVIEW_SIDE_BY_SIDE_MIN_WIDTH);
        let panes = Panes::new(&[1], &[2, 3]);
        // Clicked in the small pane: the large one beside it is split, and the one clicked in
        // is left alone.
        let (target, _, _) = split_for(panes.place_beside(&layout, Some(size), 2));
        assert_eq!(target, 3);
        // Clicked in the large pane: that is the one split.
        let (target, axis, ratio) = split_for(panes.place_beside(&layout, Some(size), 3));
        assert_eq!(
            (target, axis, ratio),
            (3, Axis::SideBySide, PREVIEW_BESIDE_OTHER_RATIO)
        );
        // A locked pane is never split, even if it is the roomiest and the one clicked in.
        let panes = Panes::new(&[1, 3], &[2, 3]);
        assert!(!matches!(
            panes.place_beside(&layout, Some(size), 3),
            Some(PreviewPlacement::Split { target: 3, .. })
        ));
    }

    #[test]
    fn with_nothing_to_split_the_tab_prefers_a_pane_that_is_not_the_one_clicked() {
        let mut layout = navigation_and_main();
        assert!(layout.split_with_ratio(2, Axis::SideBySide, 3, false, 0.5));
        let small = Some(area(1200.0, 400.0));
        // Terminals in 2 and 3, which are both too small: it waits, unselected, in the other
        // one, not behind the terminal that was clicked.
        let panes = Panes::new(&[1], &[2, 3]);
        assert_eq!(panes.place_beside(&layout, small, 2), tab(3, false));
        assert_eq!(panes.place_beside(&layout, small, 3), tab(2, false));
        // A pane that shows something else is where it can be selected, since the click asked
        // for it; the clicked pane never counts, because it shows a terminal.
        let panes = Panes::new(&[1], &[2]);
        assert_eq!(panes.place_beside(&layout, small, 2), tab(3, true));
        // Without sizes (before the first frame) nothing is split either.
        let panes = Panes::new(&[1], &[2, 3]);
        assert_eq!(panes.place_beside(&layout, None, 2), tab(3, false));
    }

    #[test]
    fn with_everything_locked_a_link_click_adds_the_tab_unselected_behind_nothing_it_covers() {
        // The default window with both panes locked: the terminal's pane cannot be split.
        let layout = navigation_and_main();
        let size = Some(area(1600.0, 900.0));
        let panes = Panes::new(&[1, 2], &[2]);
        // The navigation pane takes it as a tab that waits: the terminal clicked in stays.
        assert_eq!(panes.place_beside(&layout, size, 2), tab(1, false));
        // Clicked in a lone locked pane, the only place is that pane, unselected.
        let alone = Layout::Pane(1);
        let panes = Panes::new(&[1], &[1]);
        assert_eq!(panes.place_beside(&alone, size, 1), tab(1, false));
        // And in a lone pane too small to split.
        let panes = Panes::new(&[], &[1]);
        assert_eq!(
            panes.place_beside(&alone, Some(area(500.0, 300.0)), 1),
            tab(1, false)
        );
    }

    #[test]
    fn a_link_click_reuses_the_existing_tab_and_never_switches_away_from_the_terminal_clicked() {
        let layout = navigation_and_main();
        let size = Some(area(1600.0, 900.0));
        let panes = Panes::new(&[1], &[2]);
        let reveal = |existing| panes.reveal_beside(&layout, existing, size, 2);
        let waiting = PreviewTab {
            pane: 1,
            shown: false,
            behind_shell: false,
        };
        // Hidden in a pane that shows no terminal: brought forward there.
        assert_eq!(reveal(Some(waiting)), PreviewReveal::Activate(1));
        // Already showing: nothing to do, and nothing new is opened.
        assert_eq!(
            reveal(Some(PreviewTab {
                shown: true,
                ..waiting
            })),
            PreviewReveal::Leave
        );
        // Behind a terminal in another pane: left, so the terminal there stays.
        assert_eq!(
            reveal(Some(PreviewTab {
                pane: 3,
                behind_shell: true,
                ..waiting
            })),
            PreviewReveal::Leave
        );
        // In the clicked pane itself: never selected over the terminal clicked in, even if
        // the pane were showing something else.
        assert_eq!(
            reveal(Some(PreviewTab { pane: 2, ..waiting })),
            PreviewReveal::Leave
        );
        // Without a tab, one is made where the rules say.
        assert!(matches!(reveal(None), PreviewReveal::Open(_)));
    }

    #[test]
    fn with_the_preference_off_selecting_a_file_changes_nothing() {
        let layout = navigation_and_main();
        let size = Some(area(1600.0, 900.0));
        let panes = Panes::new(&[1], &[]);
        for existing in [
            None,
            Some(open_tab()),
            Some(PreviewTab {
                shown: true,
                ..open_tab()
            }),
        ] {
            assert_eq!(
                panes.reveal(&layout, false, existing, size, 1),
                PreviewReveal::Leave
            );
        }
    }

    #[test]
    fn pane_extents_follow_the_renderer_and_never_go_negative() {
        let mut layout = Layout::Pane(1);
        assert!(layout.split_with_ratio(1, Axis::SideBySide, 2, false, 0.25));
        let extents = layout.pane_extents(area(1005.0, 700.0));
        assert_eq!(extents[&1], area(250.0, 700.0));
        assert_eq!(extents[&2], area(750.0, 700.0));
        let degenerate = layout.pane_extents(area(2.0, 700.0));
        assert!(degenerate.values().all(|extent| extent.width >= 0.0));
    }
}
