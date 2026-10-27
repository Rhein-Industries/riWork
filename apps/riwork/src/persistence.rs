//! LayoutStore is the only writer of layouts.json. Call `open`, `switch_project`, and
//! `flush` from Rust's blocking pool; the TSX mutation path only sends snapshots.
use fs2::FileExt;
use riwork_core::layouts::{
    Layout, LayoutStore, PaneId, PanelKind, ProjectLayout, SavedPane, SavedTab,
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
    time::{Duration, Instant},
};

pub const SAVE_DELAY: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, Default)]
pub enum SaveMode {
    #[default]
    LegacyChatState,
    SharedTabs,
}

pub struct ProjectLoad {
    pub project: ProjectLayout,
    pub notice: Option<String>,
    /// Original, on-disk pane IDs. LayoutModel maps these to JS-safe IDs.
    pub unknown_panes: BTreeSet<PaneId>,
}

pub struct LoadedLayout {
    pub persistence: LayoutPersistence,
    pub project: ProjectLayout,
    pub notice: Option<String>,
    pub unknown_panes: BTreeSet<PaneId>,
}

enum Command {
    Schedule(Box<ProjectLayout>),
    Flush(Sender<Result<(), String>>),
    Stop(Sender<Result<(), String>>),
}

/// One blocking-pool worker per open layout session. It owns the debounce clock
/// and performs all file locking, normalization and atomic writes through core.
pub struct LayoutPersistence {
    dir: PathBuf,
    project_id: String,
    mode: SaveMode,
    commands: Sender<Command>,
}

impl LayoutPersistence {
    pub fn open(
        dir: impl Into<PathBuf>,
        project_id: impl Into<String>,
        restore: Option<ProjectLayout>,
        mode: SaveMode,
    ) -> Result<LoadedLayout, String> {
        let dir = dir.into();
        let project_id = project_id.into();
        if project_id.is_empty() {
            return Err("invalid: a layout session requires a project ID".into());
        }
        let loaded = load_project(&dir, &project_id, restore)?;
        let persistence = Self::start(dir, project_id, mode)?;
        Ok(LoadedLayout {
            persistence,
            project: loaded.project,
            notice: loaded.notice,
            unknown_panes: loaded.unknown_panes,
        })
    }

    fn start(dir: PathBuf, project_id: String, mode: SaveMode) -> Result<Self, String> {
        let runtime = tokio::runtime::Handle::try_current().map_err(|error| {
            format!("unavailable: layout worker needs a Tokio runtime: {error}")
        })?;
        let store = LayoutStore::open(&dir)?;
        let (commands, receiver) = mpsc::channel();
        let worker_project = project_id.clone();
        runtime.spawn_blocking(move || save_worker(store, worker_project, mode, receiver));
        Ok(Self {
            dir,
            project_id,
            mode,
            commands,
        })
    }

    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    /// Prepare a replacement without changing this writer. The caller can
    /// reject the prepared layout if its window closed while disk IO ran.
    /// All reads, flushes and worker construction happen on a blocking thread.
    pub fn prepare_project_switch(
        &self,
        project_id: &str,
        previous: &ProjectLayout,
    ) -> Result<LoadedLayout, String> {
        if project_id.is_empty() {
            return Err("invalid: a layout session requires a project ID".into());
        }
        self.schedule(previous.clone())?;
        self.flush()?;
        let mut loaded = Self::open(&self.dir, project_id, None, self.mode)?;
        match loaded.project.carry_locked_regions_from(previous) {
            Ok(project) => loaded.project = project,
            Err(error) => {
                loaded.notice = Some(format!("{error}; locked regions were not carried over"));
            }
        }
        Ok(loaded)
    }

    /// No filesystem access or waiting on another thread. The snapshot must use
    /// storage IDs (`LayoutModel::storage_project`), not projected NodeIds. Core
    /// cannot remap its private skipped-tab map; preserving those IDs avoids
    /// erasing unknown tabs when a JS-unsafe ID was projected to a safe one.
    pub fn schedule(&self, project: ProjectLayout) -> Result<(), String> {
        self.commands
            .send(Command::Schedule(Box::new(project)))
            .map_err(|_| "unavailable: layout save worker stopped".into())
    }

    /// Synchronous Rust lifecycle hook for close/quit/restart/project-switch.
    /// Never expose this as a synchronous filesystem operation to JavaScript.
    pub fn flush(&self) -> Result<(), String> {
        let (reply, result) = mpsc::channel();
        self.commands
            .send(Command::Flush(reply))
            .map_err(|_| "unavailable: layout save worker stopped".to_owned())?;
        result
            .recv()
            .map_err(|_| "unavailable: layout save worker stopped".to_owned())?
    }

    /// Save the outgoing project before reading the destination, then delegate
    /// locked-region carry (including ID collisions) to the GPUI core rule.
    pub fn switch_project(
        &mut self,
        project_id: impl Into<String>,
        previous: &ProjectLayout,
        restore: Option<ProjectLayout>,
    ) -> Result<ProjectLoad, String> {
        let project_id = project_id.into();
        if project_id.is_empty() {
            return Err("invalid: a layout session requires a project ID".into());
        }
        self.schedule(previous.clone())?;
        self.flush()?;
        let mut loaded = load_project(&self.dir, &project_id, restore)?;
        loaded.project = loaded.project.carry_locked_regions_from(previous)?;
        let next = Self::start(self.dir.clone(), project_id, self.mode)?;
        *self = next;
        Ok(loaded)
    }
}

impl Drop for LayoutPersistence {
    fn drop(&mut self) {
        let (reply, result) = mpsc::channel();
        if self.commands.send(Command::Stop(reply)).is_ok()
            && let Ok(Err(error)) = result.recv()
        {
            eprintln!("layout.save: {error}");
        }
    }
}

fn save_worker(
    store: LayoutStore,
    project_id: String,
    mode: SaveMode,
    commands: Receiver<Command>,
) {
    let mut pending = None;
    let mut deadline: Option<Instant> = None;
    loop {
        let command = match deadline {
            Some(deadline) => {
                commands.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            }
            None => commands.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match command {
            Ok(Command::Schedule(project)) => {
                pending = Some(project);
                deadline = Some(Instant::now() + SAVE_DELAY);
            }
            Ok(Command::Flush(reply)) => {
                deadline = None;
                let _ = reply.send(save_pending(&store, &project_id, mode, &mut pending));
            }
            Ok(Command::Stop(reply)) => {
                let _ = reply.send(save_pending(&store, &project_id, mode, &mut pending));
                break;
            }
            Err(RecvTimeoutError::Timeout) => {
                deadline = None;
                if let Err(error) = save_pending(&store, &project_id, mode, &mut pending) {
                    // Keep the snapshot for an explicit flush/retry. A failed
                    // filesystem must not turn into an unbounded retry loop.
                    eprintln!("layout.save: {error}");
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                let _ = save_pending(&store, &project_id, mode, &mut pending);
                break;
            }
        }
    }
}

fn save_pending(
    store: &LayoutStore,
    project_id: &str,
    mode: SaveMode,
    pending: &mut Option<Box<ProjectLayout>>,
) -> Result<(), String> {
    let Some(project) = pending.as_ref() else {
        return Ok(());
    };
    match mode {
        SaveMode::LegacyChatState => store.save_chat_state(project_id, project, &[], None),
        SaveMode::SharedTabs => store.save_tab_layout(project_id, project),
    }?;
    *pending = None;
    Ok(())
}

fn load_project(
    dir: &Path,
    project_id: &str,
    restore: Option<ProjectLayout>,
) -> Result<ProjectLoad, String> {
    let store = LayoutStore::open(dir)?;
    // Keep load and opaque-tab discovery on the same generation. This is the
    // core store's existing shared lock, never a second persistence mechanism.
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.join("layouts.lock"))
        .map_err(|error| format!("Cannot open layout lock: {error}"))?;
    FileExt::lock_shared(&lock).map_err(|error| format!("Cannot lock layouts: {error}"))?;
    let has_restore = restore.is_some();
    let loaded = match restore {
        Some(mut restore) => restore.normalize().map(|_| Some(restore)),
        None => store.load(project_id),
    };
    let (project, notice) = match loaded {
        Ok(Some(project)) => (project, None),
        Ok(None) => (default_project_layout(), None),
        Err(error) => (default_project_layout(), Some(error)),
    };
    // This is metadata inspection only: LayoutStore remains the parser and sole
    // writer. The shared lock covers both reads. No raw tab
    // payload ever crosses into TSX or gets serialized by this crate.
    let unknown_panes = if notice.is_none() {
        match unknown_tab_panes(dir, project_id) {
            Ok(panes) => panes,
            // Reload data wins even if the saved file cannot be read. Core will
            // still protect a newer schema or set aside corrupt data on save.
            Err(_) if has_restore => BTreeSet::new(),
            Err(error) => return Err(error),
        }
    } else {
        BTreeSet::new()
    };
    Ok(ProjectLoad {
        project,
        notice,
        unknown_panes,
    })
}

fn unknown_tab_panes(dir: &Path, project_id: &str) -> Result<BTreeSet<PaneId>, String> {
    let path = dir.join("layouts.json");
    let metadata = match fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(error) => return Err(error.to_string()),
    };
    if metadata.len() > 16 * 1024 * 1024 {
        return Err("layout file exceeds the core 16 MiB limit".into());
    }
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(BTreeSet::new());
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    let mut result = BTreeSet::new();
    if let Some(panes) = value
        .get("projects")
        .and_then(|p| p.get(project_id))
        .and_then(|p| p.get("panes"))
        .and_then(Value::as_object)
    {
        for (id, pane) in panes {
            if let Ok(id) = id.parse()
                && pane
                    .get("tabs")
                    .and_then(Value::as_array)
                    .is_some_and(|tabs| {
                        tabs.iter()
                            .any(|tab| serde_json::from_value::<SavedTab>(tab.clone()).is_err())
                    })
            {
                result.insert(id);
            }
        }
    }
    Ok(result)
}

pub fn default_project_layout() -> ProjectLayout {
    let tabs = [
        PanelKind::Projects,
        PanelKind::Files,
        PanelKind::Worktrees,
        PanelKind::Tasks,
        PanelKind::Shells,
    ]
    .into_iter()
    .map(|panel| SavedTab::Panel { panel })
    .collect();
    let mut project = ProjectLayout {
        layout: Layout::navigation_beside(1, Layout::Pane(2)),
        panes: BTreeMap::from([
            (
                1,
                SavedPane {
                    tabs,
                    active_tab_key: Some("panel:projects".into()),
                    ..Default::default()
                },
            ),
            (2, SavedPane::default()),
        ]),
        active_pane: 2,
        locked_panes: Some(HashSet::from([1])),
        panels_initialized: true,
        detached_shell_ids: HashSet::new(),
        detached_chat_ids: HashSet::new(),
        chat_dismissal_revision: 0,
        selected_worktree_id: None,
        selected_task_id: None,
        sidebar_visible: true,
        window_size: None,
        main_pane: Some(2),
    };
    project.normalize().expect("the default layout is valid");
    project
}
