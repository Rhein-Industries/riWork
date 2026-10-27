//! Workspace layout classes composed into `riwork-nsh-native`'s sole UI set.
//! The native crate owns the process home; this crate owns layout state and IO.
pub mod host_layouts;
pub mod layout;
pub mod persistence;
pub mod session_io;

use futures_util::future::{AbortHandle, Abortable};
use layout::{DockAreaLayout, Extent, LayoutModel};
use nsh_core::element::Cleanup;
use nsh_core::prelude::*;
use persistence::{LayoutPersistence, SaveMode};
use riwork_core::layouts::{Axis, PanelKind, ProjectLayout, SavedTab};
use riwork_engine::project_tabs::{Status, Update};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
};

type ProcessHome = fn() -> Result<PathBuf, String>;
static PROCESS_HOME: OnceLock<ProcessHome> = OnceLock::new();

/// Compose layout exports into the single `riwork/native` registry. The home
/// resolver is a Rust-only dependency inversion: it comes from the native
/// process service, runs on the blocking pool, and is never supplied by JS.
pub fn register_layouts(registry: &mut Registry, process_home: ProcessHome) {
    PROCESS_HOME.get_or_init(|| process_home);
    registry
        .module::<Layouts>()
        .class::<LayoutSession>()
        .class::<LayoutSubscription>();
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenSession {
    window_id: String,
    project_id: String,
    restore: Option<ProjectLayout>,
    #[serde(default)]
    focus_mode: bool,
    #[serde(default)]
    focus_centered: bool,
}

#[derive(Deserialize)]
struct AreaSize {
    width: f32,
    height: f32,
}

fn extent(value: Value) -> Result<Extent, String> {
    let size: AreaSize = serde_json::from_value(value).map_err(|e| format!("invalid: {e}"))?;
    if !size.width.is_finite()
        || !size.height.is_finite()
        || size.width <= 0.0
        || size.height <= 0.0
    {
        return Err("invalid: the dock extent must be positive and finite".into());
    }
    Ok(Extent {
        width: size.width,
        height: size.height,
    })
}

fn process_home() -> Result<PathBuf, String> {
    PROCESS_HOME
        .get()
        .ok_or_else(|| "unavailable: native registry is not initialized".to_owned())?()
}

/// Layout IO runs in Rust's blocking pool. JS never receives a filesystem path.
#[module]
pub struct Layouts;

#[module]
impl Layouts {
    /// Read-only metadata for generated, explicitly isolated acceptance homes.
    pub async fn fixture_manifest() -> Result<Value, String> {
        tokio::task::spawn_blocking(|| {
            if !cfg!(debug_assertions) {
                return Err("unavailable: acceptance fixtures".into());
            }
            let home = process_home()?.canonicalize().map_err(|e| e.to_string())?;
            let scratch = std::path::Path::new("/tmp")
                .canonicalize()
                .map_err(|e| e.to_string())?;
            if !home.starts_with(scratch) {
                return Err("invalid: fixtures require an isolated home".into());
            }
            serde_json::from_slice(
                &std::fs::read(home.join("fixtures.json")).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| format!("broken: {e}"))?
    }
    /// Open the project in the process's fixed home, honoring a reload snapshot.
    pub async fn open_session(request: Value) -> Result<LayoutSession, String> {
        let request: OpenSession =
            serde_json::from_value(request).map_err(|e| format!("invalid: {e}"))?;
        if request.window_id.is_empty() || request.project_id.is_empty() {
            return Err("invalid: windowId and projectId are required".into());
        }
        tokio::task::spawn_blocking(move || {
            let home = process_home()?;
            open_at(home, request)
        })
        .await
        .map_err(|e| format!("broken: {e}"))?
    }
}

fn open_at(home: PathBuf, request: OpenSession) -> Result<LayoutSession, String> {
    let has_restore = request.restore.is_some();
    let data = session_io::load(&home, &request.project_id)?;
    let needs_initial_shell = request.restore.is_none()
        && riwork_core::layouts::LayoutStore::open(&home)?
            .load(&request.project_id)
            .ok()
            .flatten()
            .is_none();
    let loaded = LayoutPersistence::open(
        home.clone(),
        &request.project_id,
        request.restore,
        if data.tabs.is_some() {
            SaveMode::SharedTabs
        } else {
            SaveMode::LegacyChatState
        },
    )?;
    let mut model = LayoutModel::new(loaded.project)?;
    model.mark_unknown_panes(&loaded.unknown_panes);
    let host = host_layouts::registration(
        &request.window_id,
        Arc::new(loaded.persistence),
        model.storage_project(),
    )?;
    let mut session = LayoutSession {
        model,
        host,
        notice: loaded.notice,
        session_notice: None,
        home,
        data,
        first_draw: None,
        last_extent: Extent {
            width: 1220.0,
            height: 730.0,
        },
        interaction: false,
        queued: None,
        initial_shell_pending: needs_initial_shell,
        initial_shell_in_flight: false,
        shell_creates: 0,
        project_generation: 0,
        switching: false,
        io_notice: Arc::new(Mutex::new(None)),
    };
    session.reconcile(false)?;
    session.host.publish(session.model.storage_project());
    if has_restore && session.notice.is_none() {
        session.host.schedule(session.model.storage_project())?;
    }
    if needs_initial_shell
        && session.data.root.is_some()
        && session.data.shells.is_some()
        && session.model.project().panes.values().all(|pane| {
            !pane
                .tabs
                .iter()
                .any(|t| matches!(t, SavedTab::Shell { .. }))
        })
        && session.data.shells.as_ref().unwrap().iter().all(|shell| {
            !shell.alive || shell.project_id.as_deref() != Some(&session.data.project_id)
        })
    {
        match session_io::create(&session.home, &session.data.project_id, None, None) {
            Ok(shell) => {
                session.initial_shell_pending = false;
                session
                    .model
                    .open_tab(SavedTab::Shell { shell_id: shell.id })?;
                session.data = session_io::load(&session.home, &session.data.project_id)?;
                if session.notice.is_none() {
                    session.host.schedule(session.model.storage_project())?;
                }
            }
            Err(error) => session.session_notice = Some(error),
        }
    }
    session
        .model
        .restore_focus(request.focus_mode, request.focus_centered);
    session
        .host
        .set_focus(request.focus_mode, request.focus_centered);
    Ok(session)
}
/// Rust-only isolated bridge harness; no path-taking API is exported to JS.
#[cfg(debug_assertions)]
pub fn open_fixture(home: PathBuf, request: Value) -> Result<LayoutSession, String> {
    let scratch = std::path::Path::new("/tmp")
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let home = home.canonicalize().map_err(|e| e.to_string())?;
    if !home.starts_with(scratch) {
        return Err("invalid: expected isolated fixture home".into());
    }
    open_at(
        home,
        serde_json::from_value(request).map_err(|e| format!("invalid: {e}"))?,
    )
}

/// Window-owned in-memory authority; DockArea receives only its projections.
#[class]
pub struct LayoutSession {
    model: LayoutModel,
    host: Arc<host_layouts::HostLayout>,
    notice: Option<String>,
    session_notice: Option<String>,
    home: PathBuf,
    data: session_io::Data,
    first_draw: Option<Instant>,
    last_extent: Extent,
    interaction: bool,
    queued: Option<session_io::Data>,
    initial_shell_pending: bool,
    initial_shell_in_flight: bool,
    shell_creates: usize,
    project_generation: u64,
    switching: bool,
    io_notice: Arc<Mutex<Option<String>>>,
}

impl LayoutSession {
    fn edit(
        &mut self,
        operation: impl FnOnce(&mut LayoutModel) -> Result<(DockAreaLayout, bool), String>,
    ) -> Result<Value, String> {
        self.ensure_editable()?;
        let host = Arc::clone(&self.host);
        host.edit_with_remap(|| {
            let before = self.model.storage_project();
            let before_remap = self.model.storage_id_remap();
            let (next, persist) = operation(&mut self.model)?;
            host.set_focus(self.model.focus_mode(), self.model.centered_focus());
            let persist = persist
                && (before != self.model.storage_project()
                    || before_remap != self.model.storage_id_remap());
            Ok((
                json!(next),
                persist.then(|| (self.model.storage_project(), self.model.storage_id_remap())),
            ))
        })
    }

    fn current_dock(&mut self) -> Value {
        json!(self.model.dock(self.last_extent))
    }
    fn ensure_editable(&self) -> Result<(), String> {
        if self.switching {
            Err("conflict: a project switch is in progress".into())
        } else {
            Ok(())
        }
    }
    fn apply_queued(&mut self) -> Result<Value, String> {
        if !self.interaction && self.shell_creates == 0 && !self.switching {
            if let Some(data) = self.queued.take() {
                return self.accept_data(data);
            }
        }
        Ok(self.current_dock())
    }
    fn reconcile(&mut self, persist: bool) -> Result<Value, String> {
        let before = self.model.storage_project();
        self.session_notice = self.data.notice.clone();
        if let Some(error) = &self.data.notice {
            self.session_notice = Some(error.clone());
            return Ok(self.current_dock());
        }
        let mut migrated = self.model.project().clone();
        let global_shells = self
            .data
            .shells
            .as_ref()
            .map(|shells| {
                shells
                    .iter()
                    .filter(|s| s.alive && s.kind == "orchestrator" && s.project_id.is_none())
                    .map(|s| s.id.clone())
                    .collect()
            })
            .unwrap_or_default();
        if persistence::prune_global_tabs(&mut migrated, &self.data.global_chats, &global_shells) {
            self.model.update_same_project(migrated)?;
            self.notice = Some(persistence::MOVED_NOTICE.into());
        }
        let mut hidden = BTreeSet::new();
        if let Some(tabs) = &self.data.tabs {
            hidden.extend(
                tabs.entries
                    .iter()
                    .filter(|e| e.hidden || e.status == Status::Stopped)
                    .map(|e| e.key.clone()),
            );
        }
        if let Some(shells) = &self.data.shells {
            let live = shells
                .iter()
                .filter(|s| s.alive)
                .map(|s| s.id.clone())
                .collect::<Vec<_>>();
            let adoptable = shells
                .iter()
                .filter(|s| {
                    s.alive
                        && s.kind == "project"
                        && s.project_id.as_deref() == Some(&self.data.project_id)
                        && !s.worker()
                })
                .map(|s| s.id.clone())
                .collect::<Vec<_>>();
            self.model.reconcile_shells(&live, &adoptable, &hidden)?;
        }
        if let Some(tabs) = &self.data.tabs {
            let mut project = self.model.project().clone();
            for pane in project.panes.values_mut() {
                pane.tabs.retain(|tab| !hidden.contains(&tab.key()));
                pane.shell_ids.clear();
                pane.active_shell_id = None;
            }
            if let Some(main) = project.main_pane.and_then(|id| project.panes.get_mut(&id)) {
                main.tabs.sort_by_key(|tab| match tab {
                    SavedTab::Panel { .. } => (0, 0),
                    _ => tabs
                        .entries
                        .iter()
                        .position(|e| e.key == tab.key())
                        .map_or((2, 0), |i| (1, i)),
                });
            }
            if project != *self.model.project() {
                let focus = self.model.focus_mode();
                let centered = self.model.centered_focus();
                self.model.update_same_project(project)?;
                self.model.restore_focus(focus, centered);
            }
        }
        if persist && before != self.model.storage_project() {
            self.host.schedule(self.model.storage_project())?;
        }
        Ok(self.current_dock())
    }
    fn accept_data(&mut self, data: session_io::Data) -> Result<Value, String> {
        if self.data.project_id != data.project_id {
            return Ok(self.current_dock());
        }
        if let (Some(known), Some(next)) = (&self.data.tabs, &data.tabs) {
            if known.epoch == next.epoch && next.revision < known.revision {
                return Ok(self.current_dock());
            }
        }
        if self.interaction || self.shell_creates > 0 || self.switching {
            if let (Some(known), Some(next)) = (
                self.queued.as_ref().and_then(|queued| queued.tabs.as_ref()),
                &data.tabs,
            ) {
                if known.epoch == next.epoch && next.revision < known.revision {
                    return Ok(self.current_dock());
                }
            }
            self.queued = Some(data);
            return Ok(self.current_dock());
        }
        let host = self.host.clone();
        host.edit_with_remap(|| {
            let before = self.model.storage_project();
            if data.shells.as_ref().is_some_and(|shells| {
                shells.iter().any(|shell| {
                    shell.alive && shell.project_id.as_deref() == Some(&data.project_id)
                })
            }) {
                self.initial_shell_pending = false;
            }
            self.data = data;
            let dock = self.reconcile(false)?;
            let project = self.model.storage_project();
            Ok((
                dock,
                (before != project).then(|| (project, self.model.storage_id_remap())),
            ))
        })
    }
    fn source_project(&self, id: &str) -> String {
        self.data
            .owners
            .get(id)
            .cloned()
            .unwrap_or_else(|| self.data.project_id.clone())
    }

    fn panel_model(&self, tab: &SavedTab) -> Value {
        let key = tab.key();
        let entry = self
            .data
            .tabs
            .as_ref()
            .and_then(|tabs| tabs.entries.iter().find(|e| e.key == key));
        let shell = key
            .strip_prefix("shell:")
            .and_then(|id| self.data.shells.as_ref()?.iter().find(|s| s.id == id));
        json!({"id":self.model.panel_id(tab),"key":key,
            "kind":match tab {SavedTab::Shell{..}=>"shell",SavedTab::Panel{..}=>"panel",SavedTab::RemoteShell{..}=>"remote_shell",SavedTab::Chat{..}=>"chat",SavedTab::AppPreview{..}=>"app_preview"},
            "title":entry.map(|e|e.title.clone()).unwrap_or_else(||tab_title(tab)),
            "shared":entry.is_some(),"status":entry.map(|e|e.status.clone()),"worker":entry.is_some_and(|e|e.worker)||shell.is_some_and(|s|s.worker()),
            "parent":entry.and_then(|e|e.parent.clone())})
    }

    /// P1b's Rust lifecycle hook: flush before publishing/closing/restarting.
    pub fn flush_from_host(&self) -> Result<(), String> {
        self.host.flush()
    }

    /// P1b reads the core snapshot directly, without a JS round-trip.
    pub fn project_from_host(&self) -> ProjectLayout {
        self.model.storage_project()
    }
}

#[class]
impl LayoutSession {
    /// Generate a controlled DockAreaLayout from the authoritative core layout.
    pub fn dock(&mut self, size: Value) -> Result<Value, String> {
        let size = extent(size)?;
        self.last_extent = size;
        self.first_draw.get_or_insert_with(Instant::now);
        self.edit(|model| Ok((model.dock(size), false)))
    }

    /// Replay one DockArea gesture using core rules, then return its projection.
    pub fn apply_dock(
        &mut self,
        next: Value,
        size: Value,
        cx: CallCx<'_>,
    ) -> Result<Value, String> {
        let next: DockAreaLayout =
            serde_json::from_value(next).map_err(|e| format!("invalid: {e}"))?;
        let size = extent(size)?;
        let previous = self.model.project().clone();
        let result = self.edit(|model| model.apply_dock(next, size).map(|next| (next, true)))?;
        let project = self.model.project();
        if let Some(tabs) = &self.data.tabs {
            if let Some(pane) = project.panes.get(&project.active_pane) {
                if let Some(key) = &pane.active_tab_key {
                    let current = pane.tabs.iter().map(SavedTab::key).collect::<Vec<_>>();
                    let prior = previous
                        .panes
                        .get(&project.active_pane)
                        .map(|p| p.tabs.iter().map(SavedTab::key).collect::<Vec<_>>())
                        .unwrap_or_default();
                    if current != prior && tabs.entries.iter().any(|e| &e.key == key) {
                        let position = current.iter().position(|id| id == key).unwrap();
                        let before = current[position + 1..]
                            .iter()
                            .find(|id| tabs.entries.iter().any(|e| &e.key == *id))
                            .cloned();
                        let update = Update::Move {
                            key: key.clone(),
                            before,
                        };
                        let home = self.home.clone();
                        let owner = self.source_project(key);
                        let notice = self.io_notice.clone();
                        cx.start(async move {
                            let outcome = tokio::task::spawn_blocking(move || {
                                session_io::update(&home, &owner, update)
                            })
                            .await;
                            if let Err(error) =
                                outcome.unwrap_or_else(|e| Err(format!("broken: {e}")))
                            {
                                *notice.lock().unwrap() =
                                    Some(format!("Could not move tab: {error}"));
                                session_io::changed();
                            }
                        });
                    }
                }
            }
        }
        Ok(result)
    }

    /// Preserve locked pixel extents through the core's window-resize algorithm.
    pub fn set_extent(&mut self, size: Value) -> Result<Value, String> {
        let size = extent(size)?;
        self.last_extent = size;
        if self
            .first_draw
            .is_some_and(|first| first.elapsed() < Duration::from_secs(2))
        {
            return Ok(self.current_dock());
        }
        self.edit(|model| {
            let before = model.project().clone();
            let next = model.set_extent(size)?;
            Ok((next, before != *model.project()))
        })
    }

    /// Select a stable panel identity and its owning pane.
    pub fn select_panel(&mut self, panel_id: String) -> Result<Value, String> {
        self.edit(|model| model.select_panel(&panel_id).map(|next| (next, true)))
    }

    /// Activate a pane without changing its selected tab (including empty panes).
    pub fn select_pane(&mut self, pane_id: u64) -> Result<Value, String> {
        self.edit(|model| model.select_pane(pane_id).map(|next| (next, true)))
    }

    /// Toggle the explicit lock set, preserving the difference from automatic locks.
    pub fn toggle_lock(&mut self, pane_id: u64) -> Result<Value, String> {
        self.edit(|model| model.toggle_lock(pane_id).map(|next| (next, true)))
    }

    /// Select or clear the project's unique main pane.
    pub fn toggle_main(&mut self, pane_id: u64) -> Result<Value, String> {
        self.edit(|model| model.toggle_main(pane_id).map(|next| (next, true)))
    }

    /// Gather unlocked sources into main without closing any session.
    pub fn gather(&mut self) -> Result<Value, String> {
        self.edit(|model| model.gather().map(|next| (next, true)))
    }

    pub fn open_panel(&mut self, panel: String) -> Result<Value, String> {
        let panel: PanelKind =
            serde_json::from_value(json!(panel)).map_err(|e| format!("invalid: {e}"))?;
        if panel == PanelKind::Dashboard {
            return Err("invalid: Dashboard belongs to the global window".into());
        }
        self.edit(|model| {
            model
                .open_tab(SavedTab::Panel { panel })
                .map(|dock| (dock, true))
        })
    }
    pub async fn close_pane(this: Instance<Self>, pane_id: u64) -> Result<Value, String> {
        let (home, updates, generation) = this.with(|s| {
            s.ensure_editable()?;
            let project = s.model.project();
            if project.effective_locked_panes().contains(&pane_id) {
                return Err("Unlock the pane to close it".to_owned());
            }
            let pane = project.panes.get(&pane_id).ok_or("not_found: pane")?;
            let inherits = project
                .main_pane
                .is_some_and(|main| main != pane_id && project.panes.contains_key(&main));
            let updates = if inherits {
                Vec::new()
            } else {
                pane.tabs
                    .iter()
                    .filter(|tab| matches!(tab, SavedTab::Shell { .. } | SavedTab::Chat { .. }))
                    .map(|tab| (s.source_project(&tab.key()), tab.key()))
                    .collect()
            };
            Ok((s.home.clone(), updates, s.project_generation))
        })?;
        tokio::task::spawn_blocking(move || {
            for (project, key) in updates {
                if let Ok(store) = riwork_engine::project_tabs::TabStore::at(&home, &project) {
                    if store
                        .snapshot()
                        .is_ok_and(|tabs| tabs.entries.iter().any(|e| e.key == key))
                    {
                        session_io::update(&home, &project, Update::Hide { key })?;
                    }
                }
            }
            Ok::<_, String>(())
        })
        .await
        .map_err(|e| format!("broken: {e}"))??;
        if this.is_disposed() {
            return Err("unavailable: layout session is closed".into());
        }
        this.with_mut(|s| {
            if s.project_generation != generation {
                Ok(s.current_dock())
            } else {
                s.edit(|m| m.close_pane(pane_id).map(|d| (d, true)))
            }
        })
    }

    pub fn default_layout(&mut self) -> Result<Value, String> {
        self.edit(|model| model.default_layout().map(|dock| (dock, true)))
    }
    pub fn toggle_focus(&mut self) -> Result<Value, String> {
        self.edit(|model| model.toggle_focus(false).map(|dock| (dock, false)))
    }
    pub fn toggle_centered(&mut self) -> Result<Value, String> {
        let size = self.last_extent;
        self.edit(|model| {
            model.restore_focus(true, !model.centered_focus());
            Ok((model.dock(size), false))
        })
    }
    pub fn set_interaction(&mut self, active: bool) -> Result<Value, String> {
        self.interaction = active;
        self.apply_queued()
    }
    pub fn open_pending_chat(&mut self, id: String) -> Result<Value, String> {
        self.edit(|m| m.open_pending_chat(&id).map(|d| (d, false)))
    }
    pub fn resolve_pending_chat(&mut self, id: String, chat_id: String) -> Result<Value, String> {
        self.edit(|m| m.resolve_pending_chat(&id, &chat_id).map(|d| (d, true)))
    }

    pub async fn new_shell(this: Instance<Self>, request: Value) -> Result<Value, String> {
        let split = request
            .get("split")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let cwd = request
            .get("cwd")
            .and_then(Value::as_str)
            .map(PathBuf::from);
        let worktree = request
            .get("worktreeId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if split
            .as_deref()
            .is_some_and(|axis| axis != "right" && axis != "down")
        {
            return Err("invalid: split direction".into());
        }
        let (home, project, generation) = this.with_mut(|s| {
            s.ensure_editable()?;
            s.shell_creates += 1;
            Ok::<_, String>((
                s.home.clone(),
                s.data.project_id.clone(),
                s.project_generation,
            ))
        })?;
        let shell = tokio::task::spawn_blocking(move || {
            session_io::create(&home, &project, cwd, worktree)
                .map(|shell| (shell, session_io::load(&home, &project).ok()))
        })
        .await
        .unwrap_or_else(|e| Err(format!("broken: {e}")));
        if this.is_disposed() {
            return Err("unavailable: layout session is closed".into());
        }
        this.with_mut(|s| {
            if s.project_generation != generation {
                return shell.map(|_| s.current_dock());
            }
            s.shell_creates -= 1;
            let outcome = shell.and_then(|(shell, data)| {
                s.initial_shell_pending = false;
                let dock = s.edit(|model| {
                    let tab = SavedTab::Shell { shell_id: shell.id };
                    match split.as_deref() {
                        Some("right") => model.split_shell(tab, Axis::SideBySide),
                        Some("down") => model.split_shell(tab, Axis::Stacked),
                        None => model.open_tab(tab),
                        _ => Err("invalid: split direction".into()),
                    }
                    .map(|dock| (dock, true))
                })?;
                // A watcher snapshot taken before creation must not immediately
                // prune the shell we just inserted. Refresh with the post-create
                // inventory before draining queued delivery.
                s.queued = None;
                if let Some(data) = data {
                    s.accept_data(data)
                } else {
                    Ok(dock)
                }
            });
            let queued = s.apply_queued();
            outcome.and(queued)
        })
    }
    pub async fn close_panel(
        this: Instance<Self>,
        id: String,
        exit: bool,
    ) -> Result<Value, String> {
        let (home, project, key, worker, generation) = this
            .with(|s| {
                s.ensure_editable()
                    .map_err(|_| "conflict: a project switch is in progress")?;
                let tab = s.model.panel(&id).ok_or("not_found: panel")?;
                let pane = s
                    .model
                    .project()
                    .panes
                    .iter()
                    .find(|(_, p)| p.tabs.iter().any(|t| t.key() == tab.key()))
                    .unwrap()
                    .0;
                if s.model.project().effective_locked_panes().contains(pane) {
                    return Err("Unlock the pane to close its tabs");
                }
                Ok((
                    s.home.clone(),
                    s.source_project(&tab.key()),
                    tab.key(),
                    s.panel_model(tab)["worker"].as_bool().unwrap_or(false),
                    s.project_generation,
                ))
            })
            .map_err(str::to_owned)?;
        let shared = this.with(|s| {
            s.data.root.is_some() && (key.starts_with("shell:") || key.starts_with("chat:"))
        });
        tokio::task::spawn_blocking(move || {
            if shared {
                if let Ok(store) = riwork_engine::project_tabs::TabStore::at(&home, &project) {
                    if store
                        .snapshot()
                        .is_ok_and(|tabs| tabs.entries.iter().any(|entry| entry.key == key))
                    {
                        session_io::update(&home, &project, Update::Hide { key: key.clone() })?;
                    }
                }
            }
            if exit && !worker {
                session_io::exit_key(&home, &key)?;
            }
            Ok::<_, String>(())
        })
        .await
        .map_err(|e| format!("broken: {e}"))??;
        if this.is_disposed() {
            return Err("unavailable: layout session is closed".into());
        }
        this.with_mut(|s| {
            if s.project_generation != generation || s.model.panel(&id).is_none() {
                Ok(s.current_dock())
            } else {
                s.edit(|m| m.close_panel(&id, false).map(|d| (d, true)))
            }
        })
    }
    pub async fn refresh_sessions(this: Instance<Self>) -> Result<Value, String> {
        Self::refresh_tabs(this).await
    }
    pub async fn refresh_tabs(this: Instance<Self>) -> Result<Value, String> {
        let (home, project, pending, generation) = this.with_mut(|s| {
            let pending = s.initial_shell_pending && !s.initial_shell_in_flight && !s.switching;
            s.initial_shell_in_flight |= pending;
            (
                s.home.clone(),
                s.data.project_id.clone(),
                pending,
                s.project_generation,
            )
        });
        let data = tokio::task::spawn_blocking(move || {
            let mut data = session_io::load(&home, &project)?;
            if pending
                && data.notice.is_none()
                && data.root.is_some()
                && data.shells.as_ref().is_some_and(|shells| {
                    shells
                        .iter()
                        .all(|s| !s.alive || s.project_id.as_deref() != Some(&project))
                })
            {
                match session_io::create(&home, &project, None, None) {
                    Ok(_) => data = session_io::load(&home, &project)?,
                    Err(error) => data.notice = Some(error),
                }
            }
            Ok::<_, String>(data)
        })
        .await
        .unwrap_or_else(|e| Err(format!("broken: {e}")));
        if this.is_disposed() {
            return Err("unavailable: layout session is closed".into());
        }
        this.with_mut(|s| {
            if s.project_generation != generation {
                return Ok(s.current_dock());
            }
            if pending {
                s.initial_shell_in_flight = false;
            }
            s.accept_data(data?)
        })
    }
    pub async fn rename_tab(
        this: Instance<Self>,
        id: String,
        title: String,
    ) -> Result<Value, String> {
        let (home, project, key) = this.with(|s| {
            (
                s.home.clone(),
                s.source_project(&id),
                s.model.panel(&id).map_or(id.clone(), SavedTab::key),
            )
        });
        tokio::task::spawn_blocking(move || {
            session_io::update(&home, &project, Update::Rename { key, title })
        })
        .await
        .map_err(|e| format!("broken: {e}"))??;
        Self::refresh_tabs(this).await
    }
    pub async fn move_tab(
        this: Instance<Self>,
        id: String,
        direction: i32,
    ) -> Result<Value, String> {
        let request = this
            .with(|s| {
                let tabs = s.data.tabs.as_ref().ok_or("unavailable: shared tabs")?;
                let entry = tabs
                    .entries
                    .iter()
                    .find(|e| e.key == id)
                    .ok_or("not_found: tab")?;
                let siblings = tabs
                    .entries
                    .iter()
                    .filter(|e| e.parent == entry.parent && !e.hidden)
                    .collect::<Vec<_>>();
                let at = siblings.iter().position(|e| e.key == id).unwrap();
                if (direction < 0 && at == 0) || (direction > 0 && at + 1 == siblings.len()) {
                    return Ok(None);
                }
                let before = if direction < 0 {
                    Some(siblings[at - 1].key.clone())
                } else {
                    siblings.get(at + 2).map(|e| e.key.clone())
                };
                Ok(Some((
                    s.home.clone(),
                    s.source_project(&id),
                    Update::Move {
                        key: id.clone(),
                        before,
                    },
                )))
            })
            .map_err(str::to_owned)?;
        if let Some((home, project, update)) = request {
            tokio::task::spawn_blocking(move || session_io::update(&home, &project, update))
                .await
                .map_err(|e| format!("broken: {e}"))??;
        }
        Self::refresh_tabs(this).await
    }
    pub async fn switch_project(this: Instance<Self>, project_id: String) -> Result<Value, String> {
        this.with(|s| s.ensure_editable())?;
        if this.with(|s| s.data.project_id == project_id) {
            return this.with_mut(|s| Ok(s.current_dock()));
        }
        let (home, previous, remap, host) = this.with_mut(|s| {
            s.switching = true;
            s.project_generation += 1;
            s.shell_creates = 0;
            s.initial_shell_in_flight = false;
            (
                s.home.clone(),
                s.model.storage_project(),
                s.model.storage_id_remap(),
                s.host.clone(),
            )
        });
        let outcome = async {
            let (loaded, data) = tokio::task::spawn_blocking(move || {
                host.edit_with_remap(|| Ok(((), Some((previous.clone(), remap)))))?;
                host.flush()?;
                let data = session_io::load(&home, &project_id)?;
                let mut loaded = LayoutPersistence::open(
                    home,
                    &project_id,
                    None,
                    if data.tabs.is_some() {
                        SaveMode::SharedTabs
                    } else {
                        SaveMode::LegacyChatState
                    },
                )?;
                match loaded.project.carry_locked_regions_from(&previous) {
                    Ok(carried) => loaded.project = carried,
                    Err(error) => {
                        loaded.notice =
                            Some(format!("{error}; locked regions were not carried over"))
                    }
                }
                Ok::<_, String>((loaded, data))
            })
            .await
            .map_err(|e| format!("broken: {e}"))??;
            if this.is_disposed() {
                return Err("unavailable: layout session is closed".into());
            }
            let host = this.with(|s| s.host.clone());
            let project = loaded.project.clone();
            let persistence = Arc::new(loaded.persistence);
            tokio::task::spawn_blocking(move || host.retarget(persistence, project))
                .await
                .map_err(|e| format!("broken: {e}"))??;
            this.with_mut(|s| {
                s.model.replace_project(loaded.project)?;
                s.model.mark_unknown_panes(&loaded.unknown_panes);
                s.host.set_focus(false, false);
                s.data = data;
                s.initial_shell_pending = false;
                s.notice = loaded.notice;
                s.session_notice = None;
                s.queued = None;
                s.first_draw = Some(Instant::now());
                s.reconcile(true)
            })
        }
        .await;
        if !this.is_disposed() {
            this.with_mut(|s| {
                s.switching = false;
                // A failed switch resumes incoming snapshots for the source.
                // On success the source queue was discarded with its model.
                s.apply_queued()
            })?;
        }
        outcome
    }
    /// A subscription is driven by the process watcher and adapter writes. It
    /// never starts a timer or performs IO on the rendering thread.
    pub fn subscribe(
        this: Instance<Self>,
        listener: Callback<Value>,
        cx: CallCx<'_>,
    ) -> LayoutSubscription {
        let (abort, registration) = AbortHandle::new_pair();
        let owner = abort.clone();
        let cleanup = cx.on_cleanup(move || owner.abort());
        let mut receive = session_io::changes().subscribe();
        cx.start(async move {
            let _ = Abortable::new(
                async move {
                    loop {
                        match receive.recv().await {
                            Ok(()) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                        if this.is_disposed() {
                            break;
                        }
                        match Self::refresh_tabs(this.clone()).await {
                            Ok(dock) => listener.post(dock),
                            Err(error) => {
                                if this.is_disposed() {
                                    break;
                                }
                                this.with_mut(|s| s.session_notice = Some(error));
                            }
                        }
                    }
                },
                registration,
            )
            .await;
        });
        LayoutSubscription {
            abort,
            cleanup: Some(cleanup),
        }
    }

    /// Tab/pane presentation metadata. Persistence uses the separate core snapshot.
    pub fn snapshot(&self) -> Value {
        let project = self.model.project();
        let locked = project.effective_locked_panes();
        let ids = project.layout.pane_ids();
        json!({
            "activePane": project.active_pane,
            "notice": self.io_notice.lock().unwrap().clone().or_else(||self.model.notice().map(str::to_owned)).or_else(||self.session_notice.clone()).or_else(||self.notice.clone()),
            "projectId": self.data.project_id, "focusMode":self.model.focus_mode(), "focusCentered":self.model.centered_focus(),
            "closeBehavior":self.data.close_behavior,"theme":self.data.theme,"uiTextSize":self.data.ui_text_size,"panelTabIcons":self.data.panel_tab_icons,
            "sharedTabs": self.data.tabs.as_ref().map(|tabs| {
                let mut drawn=ids.iter().flat_map(|id|&project.panes[id].tabs).map(|tab|self.panel_model(tab)).collect::<Vec<_>>();
                drawn.sort_by_key(|tab| { let key=tab["key"].as_str().unwrap_or_default(); if key.starts_with("panel:"){(0,0)}else{tabs.entries.iter().position(|e|e.key==key).map_or((2,0),|i|(1,i))} }); drawn
            }),
            "panes": ids.into_iter().map(|id| {
                let pane = &project.panes[&id];
                json!({ "id": id, "locked": locked.contains(&id), "main": project.main_pane == Some(id),
                    "active": project.active_pane == id, "activeTab": pane.active_tab_key.as_ref().map(|key|pane.tabs.iter().find(|tab|&tab.key()==key).map(|tab|self.model.panel_id(tab)).unwrap_or_else(||key.clone())),
                    "tabs": pane.tabs.iter().map(|tab|self.panel_model(tab)).collect::<Vec<_>>() })
            }).collect::<Vec<_>>()
        })
    }

    /// Frozen panel lookup; empty sentinels have no persisted SavedTab.
    pub fn panel(&self, panel_id: String) -> Value {
        self.model
            .panel(&panel_id)
            .map_or(Value::Null, |tab| json!(tab))
    }

    /// Await persistence off the engine thread; the host has its own Rust barrier.
    pub async fn flush(this: Instance<Self>) -> Result<(), String> {
        let host = this.with(|value| value.host.clone());
        tokio::task::spawn_blocking(move || host.flush())
            .await
            .map_err(|e| format!("broken: {e}"))?
    }
}

#[class]
pub struct LayoutSubscription {
    abort: AbortHandle,
    cleanup: Option<Cleanup>,
}
#[class]
impl LayoutSubscription {
    pub fn close(&mut self) {
        self.abort.abort();
        self.cleanup.take();
    }
}
impl Drop for LayoutSubscription {
    fn drop(&mut self) {
        self.abort.abort();
    }
}

fn tab_title(tab: &SavedTab) -> String {
    match tab {
        SavedTab::Panel { panel } => panel.label().into(),
        SavedTab::Shell { shell_id } => format!("Shell · {shell_id}"),
        SavedTab::RemoteShell {
            desktop_id,
            shell_id,
        } => format!("{shell_id} · {desktop_id}"),
        SavedTab::Chat { chat_id } => format!("Chat · {chat_id}"),
        SavedTab::AppPreview { .. } => "App preview".into(),
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;

    fn isolated_session() -> (tempfile::TempDir, LayoutSession) {
        let home = tempfile::Builder::new()
            .prefix("riwork-layout-delivery-")
            .tempdir_in("/tmp")
            .unwrap();
        let session = open_fixture(
            home.path().to_owned(),
            json!({
                "windowId":home.path().display().to_string(),
                "projectId":"delivery-project",
                "restore":persistence::default_project_layout()
            }),
        )
        .unwrap();
        (home, session)
    }
    fn snapshot_data(session: &LayoutSession, epoch: &str, revision: u64) -> session_io::Data {
        let mut data = session_io::load(&session.home, &session.data.project_id).unwrap();
        data.tabs = Some(riwork_engine::project_tabs::Snapshot {
            epoch: epoch.into(),
            revision,
            known_keys: Default::default(),
            entries: Vec::new(),
        });
        data
    }

    #[test]
    fn the_first_2_s_after_first_draw_are_ignored() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _entered = runtime.enter();
        let (_home, mut session) = isolated_session();
        session.dock(json!({"width":1280,"height":800})).unwrap();
        let before = session.project_from_host();
        session.first_draw = Some(Instant::now());
        session
            .set_extent(json!({"width":1920,"height":1080}))
            .unwrap();
        assert_eq!(session.project_from_host(), before);
        session.first_draw = Some(Instant::now() - Duration::from_secs(3));
        let mut expected = before.clone();
        let locks = expected.effective_locked_panes();
        expected.layout.preserve_locked(
            Extent {
                width: 1920.0,
                height: 1080.0,
            },
            Extent {
                width: 640.0,
                height: 400.0,
            },
            &|pane| locks.contains(&pane),
            100.0,
        );
        session
            .set_extent(json!({"width":640,"height":400}))
            .unwrap();
        assert_eq!(session.project_from_host().layout, expected.layout);
        assert_ne!(session.project_from_host().layout, before.layout);
    }

    #[test]
    fn stale_revisions_are_dropped_but_a_new_epoch_is_accepted() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _entered = runtime.enter();
        let (_home, mut session) = isolated_session();
        session
            .accept_data(snapshot_data(&session, "first", 9))
            .unwrap();
        session
            .accept_data(snapshot_data(&session, "first", 4))
            .unwrap();
        assert_eq!(session.data.tabs.as_ref().unwrap().revision, 9);
        session
            .accept_data(snapshot_data(&session, "replacement", 1))
            .unwrap();
        assert_eq!(session.data.tabs.as_ref().unwrap().epoch, "replacement");
        assert_eq!(session.data.tabs.as_ref().unwrap().revision, 1);
    }

    #[test]
    fn frozen_delivery_keeps_the_latest_revision_until_interaction_ends() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _entered = runtime.enter();
        let (_home, mut session) = isolated_session();
        session
            .accept_data(snapshot_data(&session, "same", 2))
            .unwrap();
        session.set_interaction(true).unwrap();
        session
            .accept_data(snapshot_data(&session, "same", 9))
            .unwrap();
        session
            .accept_data(snapshot_data(&session, "same", 4))
            .unwrap();
        assert_eq!(session.data.tabs.as_ref().unwrap().revision, 2);
        session.set_interaction(false).unwrap();
        assert_eq!(session.data.tabs.as_ref().unwrap().revision, 9);
        assert!(session.queued.is_none());
    }

    #[test]
    fn rejected_close_time_edits_do_not_change_the_session_before_a_cancelled_close() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _entered = runtime.enter();
        let fixture = tempfile::Builder::new()
            .prefix("riwork-layout-close-gate-")
            .tempdir_in("/tmp")
            .unwrap();
        let loaded = LayoutPersistence::open(
            fixture.path(),
            "close-gate-project",
            None,
            SaveMode::LegacyChatState,
        )
        .unwrap();
        let model = LayoutModel::new(loaded.project).unwrap();
        let host = host_layouts::registration(
            &fixture.path().display().to_string(),
            Arc::new(loaded.persistence),
            model.storage_project(),
        )
        .unwrap();
        let mut session = LayoutSession {
            model,
            host: host.clone(),
            notice: None,
            session_notice: None,
            home: fixture.path().to_owned(),
            data: session_io::Data {
                project_id: "close-gate-project".into(),
                root: None,
                shells: None,
                tabs: None,
                close_behavior: Default::default(),
                theme: riwork_core::theme::ThemeChoice::Native,
                ui_text_size: 12.0,
                panel_tab_icons: false,
                notice: None,
                global_chats: Default::default(),
                owners: Default::default(),
            },
            first_draw: None,
            last_extent: Extent {
                width: 1220.0,
                height: 730.0,
            },
            interaction: false,
            queued: None,
            initial_shell_pending: false,
            initial_shell_in_flight: false,
            shell_creates: 0,
            project_generation: 0,
            switching: false,
            io_notice: Arc::new(Mutex::new(None)),
        };
        let before = session.project_from_host();
        host.begin_close();
        assert_eq!(
            session.toggle_lock(before.active_pane).unwrap_err(),
            "unavailable: window is closing"
        );
        assert_eq!(session.project_from_host(), before);
        assert_eq!(host.snapshot(), before);
        assert!(!fixture.path().join("layouts.json").exists());

        host.cancel_close();
        session.toggle_lock(before.active_pane).unwrap();
        let accepted = session.project_from_host();
        assert_ne!(accepted, before);
        host.flush_blocking().unwrap();
        assert_eq!(
            riwork_core::layouts::LayoutStore::open(fixture.path())
                .unwrap()
                .load("close-gate-project")
                .unwrap(),
            Some(accepted)
        );
    }
}
