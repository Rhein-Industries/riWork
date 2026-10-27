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
    Schedule(Box<ProjectLayout>, BTreeMap<PaneId, PaneId>),
    Flush(Sender<Result<(), String>>),
    Stop(Sender<Result<(), String>>),
    #[cfg(debug_assertions)]
    Advance(Duration, Sender<Result<(), String>>),
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
        Self::open_clock(dir.into(), project_id.into(), restore, mode, false)
    }

    #[cfg(debug_assertions)]
    pub fn open_with_manual_clock(
        dir: impl Into<PathBuf>,
        project_id: impl Into<String>,
        restore: Option<ProjectLayout>,
        mode: SaveMode,
    ) -> Result<LoadedLayout, String> {
        Self::open_clock(dir.into(), project_id.into(), restore, mode, true)
    }

    fn open_clock(
        dir: PathBuf,
        project_id: String,
        restore: Option<ProjectLayout>,
        mode: SaveMode,
        manual: bool,
    ) -> Result<LoadedLayout, String> {
        if project_id.is_empty() {
            return Err("invalid: a layout session requires a project ID".into());
        }
        let loaded = load_project(&dir, &project_id, restore)?;
        let persistence = Self::start(
            dir,
            project_id,
            mode,
            storage_id_remap(&loaded.project)?,
            manual,
        )?;
        Ok(LoadedLayout {
            persistence,
            project: loaded.project,
            notice: loaded.notice,
            unknown_panes: loaded.unknown_panes,
        })
    }

    fn start(
        dir: PathBuf,
        project_id: String,
        mode: SaveMode,
        remap: BTreeMap<PaneId, PaneId>,
        manual: bool,
    ) -> Result<Self, String> {
        let runtime = tokio::runtime::Handle::try_current().map_err(|error| {
            format!("unavailable: layout worker needs a Tokio runtime: {error}")
        })?;
        let store = LayoutStore::open(&dir)?;
        // A reload snapshot already uses JS-safe identities while the last
        // no-edit disk generation may still use its original unsafe IDs.
        let mut remap = remap;
        if let Ok(Some(saved)) = store.load(&project_id) {
            for (old, new) in storage_id_remap(&saved)? {
                remap.entry(old).or_insert(new);
            }
        }
        let (commands, receiver) = mpsc::channel();
        let worker_project = project_id.clone();
        runtime.spawn_blocking(move || {
            save_worker(store, worker_project, mode, remap, receiver, manual)
        });
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

    /// No filesystem access or waiting on another thread. The snapshot must use
    /// storage IDs (`LayoutModel::storage_project`), not projected NodeIds.
    /// The worker passes the original unsafe-ID map to LayoutStore so opaque
    /// tabs migrate together with their pane on the first actual edit.
    pub fn schedule(&self, project: ProjectLayout) -> Result<(), String> {
        self.schedule_with_remap(project, BTreeMap::new())
    }

    pub fn schedule_with_remap(
        &self,
        project: ProjectLayout,
        remap: BTreeMap<PaneId, PaneId>,
    ) -> Result<(), String> {
        self.commands
            .send(Command::Schedule(Box::new(project), remap))
            .map_err(|_| "unavailable: layout save worker stopped".into())
    }

    /// Deterministic acceptance clock; the production worker uses monotonic time.
    #[cfg(debug_assertions)]
    pub fn advance_manual_clock(&self, elapsed: Duration) -> Result<(), String> {
        let (reply, result) = mpsc::channel();
        self.commands
            .send(Command::Advance(elapsed, reply))
            .map_err(|_| "layout worker stopped".to_owned())?;
        result
            .recv()
            .map_err(|_| "layout worker stopped".to_owned())?
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
        if project_id == self.project_id {
            return Ok(ProjectLoad {
                project: previous.clone(),
                notice: None,
                unknown_panes: unknown_tab_panes(&self.dir, &project_id)?,
            });
        }
        self.schedule(previous.clone())?;
        self.flush()?;
        let mut loaded = load_project(&self.dir, &project_id, restore)?;
        match loaded.project.carry_locked_regions_from(previous) {
            Ok(carried) => loaded.project = carried,
            Err(error) => {
                loaded.notice = Some(format!("{error}; locked regions were not carried over"));
            }
        }
        let next = Self::start(
            self.dir.clone(),
            project_id,
            self.mode,
            storage_id_remap(&loaded.project)?,
            false,
        )?;
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
    mut remap: BTreeMap<PaneId, PaneId>,
    commands: Receiver<Command>,
    manual: bool,
) {
    let started = Instant::now();
    let mut manual_now = Duration::ZERO;
    let mut pending = None;
    let mut deadline: Option<Duration> = None;
    loop {
        let command = match deadline {
            Some(deadline) if !manual => {
                commands.recv_timeout(deadline.saturating_sub(started.elapsed()))
            }
            _ => commands.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match command {
            Ok(Command::Schedule(project, moved)) => {
                remap.extend(moved);
                pending = Some(project);
                deadline = Some(
                    (if manual {
                        manual_now
                    } else {
                        started.elapsed()
                    }) + SAVE_DELAY,
                );
            }
            #[cfg(debug_assertions)]
            Ok(Command::Advance(elapsed, reply)) => {
                if !manual {
                    let _ = reply.send(Err("manual clock is not enabled".into()));
                    continue;
                }
                manual_now += elapsed;
                let result = if deadline.is_some_and(|due| manual_now >= due) {
                    deadline = None;
                    save_pending(&store, &project_id, mode, &remap, &mut pending)
                } else {
                    Ok(())
                };
                let _ = reply.send(result);
            }
            Ok(Command::Flush(reply)) => {
                deadline = None;
                let _ = reply.send(save_pending(
                    &store,
                    &project_id,
                    mode,
                    &remap,
                    &mut pending,
                ));
            }
            Ok(Command::Stop(reply)) => {
                let _ = reply.send(save_pending(
                    &store,
                    &project_id,
                    mode,
                    &remap,
                    &mut pending,
                ));
                break;
            }
            Err(RecvTimeoutError::Timeout) => {
                deadline = None;
                if let Err(error) = save_pending(&store, &project_id, mode, &remap, &mut pending) {
                    // Keep the snapshot for an explicit flush/retry. A failed
                    // filesystem must not turn into an unbounded retry loop.
                    eprintln!("layout.save: {error}");
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                let _ = save_pending(&store, &project_id, mode, &remap, &mut pending);
                break;
            }
        }
    }
}

fn save_pending(
    store: &LayoutStore,
    project_id: &str,
    mode: SaveMode,
    remap: &BTreeMap<PaneId, PaneId>,
    pending: &mut Option<Box<ProjectLayout>>,
) -> Result<(), String> {
    let Some(project) = pending.as_ref() else {
        return Ok(());
    };
    match mode {
        SaveMode::LegacyChatState => store.save_chat_state_remapped(project_id, project, remap),
        SaveMode::SharedTabs => store.save_tab_layout_remapped(project_id, project, remap),
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
    let (mut project, notice) = match loaded {
        Ok(Some(project)) => (project, None),
        Ok(None) => (default_project_layout(), None),
        Err(error) => (
            default_project_layout(),
            Some(format!("{error}; using a default layout")),
        ),
    };
    initialize_navigation(&mut project)?;
    // This is metadata inspection only: LayoutStore remains the parser and sole
    // writer. The shared lock covers both reads. No raw tab
    // payload ever crosses into TSX or gets serialized by this crate.
    let mut unknown_panes = if notice.is_none() {
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
    if has_restore && let Ok(Some(saved)) = store.load(project_id) {
        let remap = storage_id_remap(&saved)?;
        unknown_panes = unknown_panes
            .into_iter()
            .map(|id| {
                if !project.panes.contains_key(&id) {
                    remap.get(&id).copied().unwrap_or(id)
                } else {
                    id
                }
            })
            .collect();
    }
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

fn storage_id_remap(project: &ProjectLayout) -> Result<BTreeMap<PaneId, PaneId>, String> {
    let model = crate::layout::LayoutModel::new(project.clone())?;
    Ok(model
        .pane_id_remap()
        .iter()
        .filter(|(old, _)| **old > (1_u64 << 53) - 1)
        .map(|(&old, &new)| (old, new))
        .collect())
}

/// The GPUI global-nav migration copy; showing this notice never opens a window.
pub const MOVED_NOTICE: &str = "The dashboard and the global chat now have their own window: View › Global Dashboard (⌘⌥G) or Global Chat (⌘⇧O).";

/// Leave global-window tabs out of an older project's layout. Unknown/dead
/// shell IDs are not classified as global: only a live Sessions delivery can
/// identify a global orchestrator. Remote and preview tabs always remain.
pub fn prune_global_tabs(
    project: &mut ProjectLayout,
    global_chats: &HashSet<String>,
    live_global_shells: &HashSet<String>,
) -> bool {
    let mut changed = false;
    for pane in project.panes.values_mut() {
        let old_len = pane.tabs.len();
        pane.tabs.retain(|tab| !match tab {
            SavedTab::Panel { panel } => *panel == PanelKind::Dashboard,
            SavedTab::Chat { chat_id } => global_chats.contains(chat_id),
            SavedTab::Shell { shell_id } => live_global_shells.contains(shell_id),
            SavedTab::RemoteShell { .. } | SavedTab::AppPreview { .. } => false,
        });
        changed |= old_len != pane.tabs.len();
        // Prevent normalize from importing a removed legacy shell again.
        pane.shell_ids.retain(|id| !live_global_shells.contains(id));
        if pane
            .active_shell_id
            .as_ref()
            .is_some_and(|id| live_global_shells.contains(id))
        {
            pane.active_shell_id = None;
        }
    }
    if changed {
        project
            .normalize()
            .expect("removing tabs preserves a valid project layout");
    }
    changed
}

/// Old shell-only saves had no panel initialization flag. Rebuild their visible
/// navigation region beside the saved tree, retaining shell order and selection.
/// This is an in-memory migration; opening/closing alone never writes it.
pub fn initialize_navigation(project: &mut ProjectLayout) -> Result<bool, String> {
    if project.panels_initialized || !project.sidebar_visible {
        return Ok(false);
    }
    let occupied = project.layout.pane_ids();
    let next = (1..)
        .find(|id| !occupied.contains(id))
        .expect("bounded pane tree");
    let mut candidate = project.clone();
    candidate.layout = Layout::navigation_beside(next, candidate.layout.clone());
    candidate.panes.insert(
        next,
        SavedPane {
            tabs: riwork_core::layouts::NAVIGATION_PANELS
                .into_iter()
                .map(|panel| SavedTab::Panel { panel })
                .collect(),
            active_tab_key: Some("panel:projects".into()),
            ..Default::default()
        },
    );
    candidate.panels_initialized = true;
    // Explicit unlock stays explicit; default navigation lock stays inferred.
    candidate.normalize()?;
    *project = candidate;
    Ok(true)
}
