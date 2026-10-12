//! `global-window.json`: what the global window last showed, and how its dashboard filters
//! and orders the projects. The global window holds the dashboard and the global chat, and
//! it belongs to no project, so its state is kept here and never in a project's layout
//! (`layouts.json`), the store's selection or a project's tabs.
//!
//! The file is small and only ever read back by this app. A missing, unreadable or foreign
//! file is the default state (the dashboard), never an error a window has to show, and a view
//! a newer build named is read as the dashboard.

use crate::layouts::WindowSize;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};
use uuid::Uuid;

pub const FILE_NAME: &str = "global-window.json";
/// A larger file is not one this app wrote.
const MAX_BYTES: u64 = 64 * 1024;

/// What the global window shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum GlobalView {
    /// Every project's work on this Mac.
    #[default]
    Dashboard,
    /// The global orchestrator: its chat, or the terminal it runs in.
    Chat,
}

impl GlobalView {
    pub fn name(self) -> &'static str {
        match self {
            Self::Dashboard => "dashboard",
            Self::Chat => "chat",
        }
    }
}

impl From<String> for GlobalView {
    fn from(name: String) -> Self {
        match name.as_str() {
            "chat" => Self::Chat,
            _ => Self::Dashboard,
        }
    }
}

impl From<GlobalView> for String {
    fn from(view: GlobalView) -> Self {
        view.name().to_owned()
    }
}

/// Which projects the dashboard's table lists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum DashboardFilter {
    /// With a live session or chat, or an open task.
    #[default]
    Active,
    /// With an agent waiting for the user, a failed chat or a failed automation.
    NeedsYou,
    /// With an agent at work.
    Working,
    /// With a task in progress.
    InProgress,
    /// Every project, the quiet ones too.
    All,
}

impl DashboardFilter {
    pub const ALL: [Self; 5] = [
        Self::Active,
        Self::NeedsYou,
        Self::Working,
        Self::InProgress,
        Self::All,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::NeedsYou => "needs_you",
            Self::Working => "working",
            Self::InProgress => "in_progress",
            Self::All => "all",
        }
    }
}

impl From<String> for DashboardFilter {
    fn from(name: String) -> Self {
        Self::ALL
            .into_iter()
            .find(|filter| filter.name() == name)
            .unwrap_or_default()
    }
}

impl From<DashboardFilter> for String {
    fn from(filter: DashboardFilter) -> Self {
        filter.name().to_owned()
    }
}

/// The order of the dashboard's table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum DashboardSort {
    /// What needs the user, then what works, then what runs, then open tasks.
    #[default]
    Priority,
    /// The latest recorded activity first.
    Recent,
    /// The most tasks in progress, then the most open ones, first.
    Tasks,
    /// The most live sessions and chats first.
    Sessions,
    Name,
}

impl DashboardSort {
    pub const ALL: [Self; 5] = [
        Self::Priority,
        Self::Recent,
        Self::Tasks,
        Self::Sessions,
        Self::Name,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Priority => "priority",
            Self::Recent => "recent",
            Self::Tasks => "tasks",
            Self::Sessions => "sessions",
            Self::Name => "name",
        }
    }
}

impl From<String> for DashboardSort {
    fn from(name: String) -> Self {
        Self::ALL
            .into_iter()
            .find(|sort| sort.name() == name)
            .unwrap_or_default()
    }
}

impl From<DashboardSort> for String {
    fn from(sort: DashboardSort) -> Self {
        sort.name().to_owned()
    }
}

/// The global window's own state. Every field has a default, so a file from another build
/// still reads.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GlobalWindowState {
    pub view: GlobalView,
    /// The dashboard lists the projects with nothing going on as well. Builds before the
    /// dashboard's filters read only this; it is kept true exactly when the filter is All.
    pub show_quiet: bool,
    /// The dashboard's project filter; a file without one has `show_quiet`'s meaning.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dashboard_filter: Option<DashboardFilter>,
    pub dashboard_sort: DashboardSort,
    /// The last ordinary size the user gave the window.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_size: Option<WindowSize>,
}

impl GlobalWindowState {
    /// The filter the dashboard opens with: the saved one, else what `show_quiet` meant.
    pub fn filter(&self) -> DashboardFilter {
        self.dashboard_filter.unwrap_or(if self.show_quiet {
            DashboardFilter::All
        } else {
            DashboardFilter::Active
        })
    }

    /// Set the dashboard's filter, and `show_quiet` as an older build would take it.
    pub fn set_filter(&mut self, filter: DashboardFilter) {
        self.dashboard_filter = Some(filter);
        self.show_quiet = filter == DashboardFilter::All;
    }
}

#[derive(Clone, Debug)]
pub struct GlobalWindowStore {
    dir: PathBuf,
}

impl GlobalWindowStore {
    pub fn open_default() -> Result<Self, String> {
        Ok(Self::at(crate::paths::riwork_home()?))
    }

    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join(FILE_NAME)
    }

    /// The saved state, or the default one when there is none that can be read.
    pub fn load(&self) -> GlobalWindowState {
        let mut state = read(&self.path()).unwrap_or_default();
        // A size no window can have is no size.
        state.window_size = state
            .window_size
            .and_then(|size| WindowSize::new(size.width, size.height));
        state
    }

    /// Replace the saved state: readers see the old file or the whole new one.
    pub fn save(&self, state: &GlobalWindowState) -> Result<(), String> {
        fs::create_dir_all(&self.dir)
            .map_err(|error| format!("Cannot create {}: {error}", self.dir.display()))?;
        let mut data = serde_json::to_vec_pretty(state)
            .map_err(|error| format!("Cannot encode the global window: {error}"))?;
        data.push(b'\n');
        let path = self.path();
        let temporary = self
            .dir
            .join(format!(".global-window-{}.tmp", Uuid::new_v4()));
        let result = (|| {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options
                .open(&temporary)
                .map_err(|error| format!("Cannot create {}: {error}", temporary.display()))?;
            file.write_all(&data)
                .map_err(|error| format!("Cannot write the global window: {error}"))?;
            fs::rename(&temporary, &path)
                .map_err(|error| format!("Cannot replace {}: {error}", path.display()))
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

fn read(path: &Path) -> Option<GlobalWindowState> {
    // A named pipe would block the open, so look before opening.
    let metadata = fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    File::open(path)
        .ok()?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> PathBuf {
        let path = std::env::temp_dir().join(format!("riwork-global-window-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn the_state_is_saved_in_its_own_file_and_read_back() {
        let home = home();
        let store = GlobalWindowStore::at(&home);
        assert_eq!(store.load(), GlobalWindowState::default());
        let mut state = GlobalWindowState {
            view: GlobalView::Chat,
            dashboard_sort: DashboardSort::Recent,
            window_size: WindowSize::new(900.0, 640.0),
            ..GlobalWindowState::default()
        };
        state.set_filter(DashboardFilter::All);
        store.save(&state).unwrap();
        assert_eq!(store.load(), state);
        let names = fs::read_dir(&home)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect::<Vec<_>>();
        // One file of its own: no project layout, no leftovers.
        assert_eq!(names, [FILE_NAME]);
        let text = fs::read_to_string(store.path()).unwrap();
        assert!(text.contains("\"view\": \"chat\""), "{text}");
        // An older build, which knows only `show_quiet`, still lists every project.
        assert!(text.contains("\"show_quiet\": true"), "{text}");
        assert!(text.contains("\"dashboard_filter\": \"all\""), "{text}");
        assert!(text.contains("\"dashboard_sort\": \"recent\""), "{text}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(store.path()).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn anything_unreadable_is_the_dashboard() {
        let home = home();
        let store = GlobalWindowStore::at(&home);
        for text in [
            "",
            "not json",
            "[]",
            r#"{"view":"timeline","show_quiet":true}"#,
            r#"{"view":7}"#,
        ] {
            fs::write(store.path(), text).unwrap();
            let state = store.load();
            assert_eq!(state.view, GlobalView::Dashboard, "{text:?}");
        }
        // A view a newer build added reads as the dashboard; the rest of the file still counts.
        fs::write(
            store.path(),
            r#"{"view":"timeline","show_quiet":true,"later":1}"#,
        )
        .unwrap();
        assert!(store.load().show_quiet);
        // Not a regular file, or far too large: the default.
        fs::remove_file(store.path()).unwrap();
        fs::create_dir(store.path()).unwrap();
        assert_eq!(store.load(), GlobalWindowState::default());
        fs::remove_dir(store.path()).unwrap();
        fs::write(store.path(), vec![b' '; MAX_BYTES as usize + 1]).unwrap();
        assert_eq!(store.load(), GlobalWindowState::default());
        fs::write(
            store.path(),
            r#"{"view":"chat","window_size":{"width":-1.0,"height":1e9}}"#,
        )
        .unwrap();
        let state = store.load();
        assert_eq!((state.view, state.window_size), (GlobalView::Chat, None));
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn the_dashboards_filter_and_order_read_from_files_of_every_build() {
        let home = home();
        let store = GlobalWindowStore::at(&home);
        // Before the filters: `show_quiet` alone says whether quiet projects are listed.
        for (text, filter) in [
            (r#"{"view":"dashboard"}"#, DashboardFilter::Active),
            (r#"{"show_quiet":false}"#, DashboardFilter::Active),
            (r#"{"show_quiet":true}"#, DashboardFilter::All),
            // A saved filter wins over `show_quiet`.
            (
                r#"{"show_quiet":true,"dashboard_filter":"needs_you"}"#,
                DashboardFilter::NeedsYou,
            ),
            // One a newer build named is the default one.
            (
                r#"{"show_quiet":true,"dashboard_filter":"starred"}"#,
                DashboardFilter::Active,
            ),
        ] {
            fs::write(store.path(), text).unwrap();
            assert_eq!(store.load().filter(), filter, "{text}");
        }
        for (text, sort) in [
            (r#"{}"#, DashboardSort::Priority),
            (r#"{"dashboard_sort":"name"}"#, DashboardSort::Name),
            (r#"{"dashboard_sort":"size"}"#, DashboardSort::Priority),
        ] {
            fs::write(store.path(), text).unwrap();
            assert_eq!(store.load().dashboard_sort, sort, "{text}");
        }
        // Every filter and order is written and read back as itself.
        for filter in DashboardFilter::ALL {
            for sort in DashboardSort::ALL {
                let mut state = GlobalWindowState {
                    dashboard_sort: sort,
                    ..GlobalWindowState::default()
                };
                state.set_filter(filter);
                store.save(&state).unwrap();
                let read = store.load();
                assert_eq!((read.filter(), read.dashboard_sort), (filter, sort));
                assert_eq!(read.show_quiet, filter == DashboardFilter::All);
            }
        }
        fs::remove_dir_all(home).unwrap();
    }
}
