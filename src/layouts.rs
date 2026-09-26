//! Durable per-project tabs and split geometry, independent of shell lifetimes.

use std::{
    collections::{BTreeMap, HashSet},
    env, fs,
    fs::{File, OpenOptions},
    io::Write,
    path::PathBuf,
};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type PaneId = u64;

const MAX_DEPTH: usize = 32;
const MAX_PANES: usize = 256;
const MAX_PANE_ID: PaneId = u64::MAX - MAX_PANES as u64;
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

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
    Tasks,
    Shells,
    Usage,
    Settings,
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
                    PanelKind::Tasks => "tasks",
                    PanelKind::Shells => "shells",
                    PanelKind::Usage => "usage",
                    PanelKind::Settings => "settings",
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

fn sidebar_visible_default() -> bool {
    true
}

impl ProjectLayout {
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

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
struct SavedLayouts {
    schema_version: u32,
    projects: BTreeMap<String, ProjectLayout>,
}

impl Default for SavedLayouts {
    fn default() -> Self {
        Self {
            schema_version: 1,
            projects: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct LayoutStore {
    dir: PathBuf,
}

impl LayoutStore {
    pub fn open_default() -> Result<Self, String> {
        let dir = if let Some(value) = env::var_os("RIWORK_HOME") {
            PathBuf::from(value)
        } else {
            let home = env::var_os("HOME").ok_or("HOME is unset; set RIWORK_HOME")?;
            PathBuf::from(home).join(".local/share/riwork")
        };
        Self::open(dir)
    }

    pub fn open(dir: impl Into<PathBuf>) -> Result<Self, String> {
        let dir = dir.into();
        fs::create_dir_all(&dir)
            .map_err(|error| format!("Cannot create {}: {error}", dir.display()))?;
        Ok(Self { dir })
    }

    pub fn load(&self, project_id: &str) -> Result<Option<ProjectLayout>, String> {
        let lock = self.lock_file()?;
        FileExt::lock_shared(&lock).map_err(|error| format!("Cannot lock layouts: {error}"))?;
        let mut layout = self.read_layouts()?.projects.remove(project_id);
        if let Some(layout) = &mut layout {
            layout.normalize()?;
        }
        Ok(layout)
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
        let mut layouts = self.read_layouts()?;
        if layouts.projects.get(project_id) == Some(&layout) {
            return Ok(());
        }
        layouts.projects.insert(project_id.to_owned(), layout);
        self.write_layouts(&layouts)
    }

    fn lock_file(&self) -> Result<File, String> {
        OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(self.dir.join("layouts.lock"))
            .map_err(|error| format!("Cannot open layout lock: {error}"))
    }

    fn read_layouts(&self) -> Result<SavedLayouts, String> {
        let path = self.dir.join("layouts.json");
        match fs::metadata(&path) {
            Ok(metadata) if metadata.len() > MAX_FILE_BYTES => {
                return Err(format!("{} is too large", path.display()));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(SavedLayouts::default());
            }
            Err(error) => return Err(format!("Cannot read {}: {error}", path.display())),
        }
        let data =
            fs::read(&path).map_err(|error| format!("Cannot read {}: {error}", path.display()))?;
        let layouts: SavedLayouts = serde_json::from_slice(&data)
            .map_err(|error| format!("Cannot parse {}: {error}", path.display()))?;
        if layouts.schema_version != 1 {
            return Err(format!(
                "Unsupported layout schema {}; this build supports schema 1",
                layouts.schema_version
            ));
        }
        Ok(layouts)
    }

    fn write_layouts(&self, layouts: &SavedLayouts) -> Result<(), String> {
        let path = self.dir.join("layouts.json");
        let tmp = self.dir.join(format!(".layouts-{}.tmp", Uuid::new_v4()));
        let write = || -> Result<(), String> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)
                .map_err(|error| format!("Cannot create {}: {error}", tmp.display()))?;
            serde_json::to_writer_pretty(&mut file, layouts)
                .map_err(|error| format!("Cannot encode layouts: {error}"))?;
            file.write_all(b"\n")
                .map_err(|error| format!("Cannot write layouts: {error}"))?;
            file.sync_all()
                .map_err(|error| format!("Cannot sync layouts: {error}"))?;
            fs::rename(&tmp, &path)
                .map_err(|error| format!("Cannot replace {}: {error}", path.display()))?;
            File::open(&self.dir)
                .and_then(|dir| dir.sync_all())
                .map_err(|error| format!("Cannot sync {}: {error}", self.dir.display()))
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
            for name in ["layouts.json", "layouts.lock"] {
                let _ = fs::remove_file(self.0.join(name));
            }
            let _ = fs::remove_dir(&self.0);
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
}
