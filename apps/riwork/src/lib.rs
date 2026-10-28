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
use riwork_engine::project_tabs::{Kind, Status, Update};
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

/// Rust-only backend injection; JavaScript never supplies a storage path or
/// implementation. The default adapter is the production core LayoutStore.
pub trait LayoutRepository: Send + Sync {
    fn open(
        &self,
        home: PathBuf,
        project_id: String,
        restore: Option<ProjectLayout>,
    ) -> Result<OpenedLayout, String>;
    /// Shared-tab homes save through the tab layout writer; others keep the
    /// legacy chat-state writer. Fakes may ignore the mode.
    fn open_with_mode(
        &self,
        home: PathBuf,
        project_id: String,
        restore: Option<ProjectLayout>,
        _mode: SaveMode,
    ) -> Result<OpenedLayout, String> {
        self.open(home, project_id, restore)
    }
}

pub struct OpenedLayout {
    pub backend: Arc<dyn persistence::LayoutBackend>,
    pub project: ProjectLayout,
    pub notice: Option<String>,
    pub unknown_panes: BTreeSet<u64>,
}

pub struct CoreLayoutRepository;
impl LayoutRepository for CoreLayoutRepository {
    fn open(
        &self,
        home: PathBuf,
        project_id: String,
        restore: Option<ProjectLayout>,
    ) -> Result<OpenedLayout, String> {
        self.open_with_mode(home, project_id, restore, SaveMode::LegacyChatState)
    }
    fn open_with_mode(
        &self,
        home: PathBuf,
        project_id: String,
        restore: Option<ProjectLayout>,
        mode: SaveMode,
    ) -> Result<OpenedLayout, String> {
        let loaded = LayoutPersistence::open(home, project_id, restore, mode)?;
        Ok(OpenedLayout {
            backend: Arc::new(loaded.persistence),
            project: loaded.project,
            notice: loaded.notice,
            unknown_panes: loaded.unknown_panes,
        })
    }
}

static LAYOUT_REPOSITORY: OnceLock<Arc<dyn LayoutRepository>> = OnceLock::new();

pub fn install_layout_repository(backend: Arc<dyn LayoutRepository>) -> Result<(), String> {
    LAYOUT_REPOSITORY
        .set(backend)
        .map_err(|_| "conflict: layout repository is already initialized".into())
}

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

fn layout_error(error: String) -> String {
    if error.split_once(": ").is_some_and(|(code, _)| {
        matches!(
            code,
            "refused" | "broken" | "not_found" | "invalid" | "conflict" | "unavailable" | "io"
        )
    }) {
        error
    } else {
        format!("io: {error}")
    }
}

fn validate_id(id: &str) -> Result<(), String> {
    if !uuid::Uuid::parse_str(id).is_ok_and(|value| value.to_string() == id) {
        return Err("invalid: expected a canonical UUID".into());
    }
    Ok(())
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
        tokio::task::spawn_blocking(move || {
            let request: OpenSession =
                serde_json::from_value(request).map_err(|e| format!("invalid: {e}"))?;
            if request.window_id.is_empty() || request.project_id.is_empty() {
                return Err("invalid: windowId and projectId are required".into());
            }
            open_at(process_home()?, request)
        })
        .await
        .map_err(|e| format!("broken: {e}"))?
        .map_err(layout_error)
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
    let loaded = LAYOUT_REPOSITORY
        .get_or_init(|| Arc::new(CoreLayoutRepository))
        .open_with_mode(
            home.clone(),
            request.project_id.clone(),
            request.restore,
            if data.tabs.is_some() {
                SaveMode::SharedTabs
            } else {
                SaveMode::LegacyChatState
            },
        )?;
    let mut model = LayoutModel::new(loaded.project)?;
    model.mark_unknown_panes(&loaded.unknown_panes);
    let host = host_layouts::registration_backend(
        &request.window_id,
        loaded.backend,
        model.storage_project(),
    )?;
    let mut session = LayoutSession {
        model,
        host,
        notice: loaded.notice,
        session_notice: None,
        home,
        data,
        inventory_initialized: false,
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
        .publish_focus(request.focus_mode, request.focus_centered);
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
    /// Initial restore filters dead saved tabs; later inventories retain an
    /// already-open ended shell's last surface until explicit Hide/removal.
    inventory_initialized: bool,
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

/// A cancelled JS promise can release its result on an engine thread. Keep
/// persistence behind the same deferred-disposal guard as the host handle.
struct PreparedSwitch {
    model: Option<LayoutModel>,
    persistence: Option<Arc<LayoutPersistence>>,
    notice: Option<String>,
}

impl Drop for PreparedSwitch {
    fn drop(&mut self) {
        if let Some(persistence) = self.persistence.take() {
            host_layouts::defer_persistence(persistence);
        }
    }
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
            host.publish_focus(self.model.focus_mode(), self.model.centered_focus());
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
                    .filter(|e| e.hidden || (e.kind != Kind::Shell && e.status == Status::Stopped))
                    .map(|e| e.key.clone()),
            );
        }
        if let Some(shells) = &self.data.shells {
            // An ended registered shell retains its tab and last surface. The
            // engine prunes exited Vim rows, so missing editor rows still leave
            // the layout. Only live sessions are eligible for new adoption.
            let retained = shells
                .iter()
                .filter(|s| s.alive || self.inventory_initialized)
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
            self.model
                .reconcile_shells(&retained, &adoptable, &hidden)?;
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
        self.inventory_initialized = true;
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
        .map_err(layout_error)
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

    /// Rust-only gesture replay; returns the shared Move its caller must apply.
    pub fn apply_dock_model(
        &mut self,
        next: Value,
        size: Value,
    ) -> Result<(Value, Option<(PathBuf, String, Update)>), String> {
        let next: DockAreaLayout =
            serde_json::from_value(next).map_err(|e| format!("invalid: {e}"))?;
        let size = extent(size)?;
        let previous = self.model.project().clone();
        let result = self.edit(|model| model.apply_dock(next, size).map(|next| (next, true)))?;
        let project = self.model.project();
        let mut update = None;
        if let Some(tabs) = &self.data.tabs
            && let Some(pane) = project.panes.get(&project.active_pane)
            && let Some(key) = &pane.active_tab_key
        {
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
                update = Some((
                    self.home.clone(),
                    self.source_project(key),
                    Update::Move {
                        key: key.clone(),
                        before,
                    },
                ));
            }
        }
        Ok((result, update))
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
    /// A shared-strip move is written to the owning project's TabStore off-engine.
    pub fn apply_dock(
        &mut self,
        next: Value,
        size: Value,
        cx: CallCx<'_>,
    ) -> Result<Value, String> {
        let (result, update) = self.apply_dock_model(next, size)?;
        if let Some((home, owner, update)) = update {
            let notice = self.io_notice.clone();
            cx.start(async move {
                let outcome =
                    tokio::task::spawn_blocking(move || session_io::update(&home, &owner, update))
                        .await;
                if let Err(error) = outcome.unwrap_or_else(|e| Err(format!("broken: {e}"))) {
                    *notice.lock().unwrap() = Some(format!("Could not move tab: {error}"));
                    session_io::changed();
                }
            });
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

    /// Place or select one existing local shell without creating a session.
    pub fn open_shell(&mut self, shell_id: String) -> Result<Value, String> {
        self.edit(|model| model.open_shell(&shell_id).map(|next| (next, true)))
    }

    /// Place an existing dedicated Vim session using the editor routing policy.
    pub fn open_editor_shell(&mut self, shell_id: String) -> Result<Value, String> {
        self.edit(|model| model.open_editor_shell(&shell_id).map(|next| (next, true)))
    }

    /// Remove this window's tab while preserving its persistent tmux session.
    pub fn detach_shell(&mut self, shell_id: String) -> Result<Value, String> {
        self.edit(|model| model.detach_shell(&shell_id).map(|next| (next, true)))
    }

    /// Open a navigation panel or select its existing stable tab identity.
    pub fn open_panel(&mut self, panel_id: String) -> Result<Value, String> {
        let name = panel_id.strip_prefix("panel:").unwrap_or(&panel_id);
        let panel: PanelKind =
            serde_json::from_value(json!(name)).map_err(|_| "invalid: unknown panel".to_owned())?;
        self.edit(|model| model.open_panel(panel).map(|next| (next, true)))
    }

    /// Reveal Files or Preview using core placement while retaining keyboard focus.
    pub fn reveal_panel(
        &mut self,
        panel_id: String,
        asked: bool,
        enabled: bool,
    ) -> Result<Value, String> {
        let panel: PanelKind = serde_json::from_value(json!(panel_id))
            .map_err(|_| "invalid: unknown panel".to_owned())?;
        self.edit(|model| {
            model
                .reveal_panel(panel, asked, enabled)
                .map(|next| (next, true))
        })
    }

    /// Place or select a shell chosen by the navigation panels.
    pub fn show_shell(&mut self, shell_id: String) -> Result<Value, String> {
        validate_id(&shell_id)?;
        self.edit(|model| model.show_shell(&shell_id).map(|next| (next, true)))
    }

    /// Persist window-local navigation selection through LayoutStore.
    pub fn set_selected_worktree(&mut self, id: Option<String>) -> Result<(), String> {
        if let Some(id) = &id {
            validate_id(id)?;
        }
        let host = self.host.clone();
        host.edit(|| {
            self.model.set_selected_worktree(id);
            Ok(((), Some(self.model.storage_project())))
        })
    }

    pub fn set_selected_task(&mut self, id: Option<String>) -> Result<(), String> {
        if let Some(id) = &id {
            validate_id(id)?;
        }
        let host = self.host.clone();
        host.edit(|| {
            self.model.set_selected_task(id);
            Ok(((), Some(self.model.storage_project())))
        })
    }

    /// Flush the outgoing project and prepare locked-region carry away from the
    /// engine. A close, disposal or load failure keeps the old session intact.
    pub async fn switch_project(this: Instance<Self>, project_id: String) -> Result<Value, String> {
        validate_id(&project_id)?;
        if this.is_disposed() {
            return Err("unavailable: layout session is disposed".into());
        }
        this.with(|session| session.ensure_editable())?;
        let host = this.with(|session| session.host.clone());
        if host.project_id() == project_id {
            return this.with_mut(|session| {
                let size = session.model.extent().unwrap_or(Extent {
                    width: 1220.0,
                    height: 780.0,
                });
                Ok(json!(session.model.dock(size)))
            });
        }
        let (size, home) = this.with(|session| (session.model.extent(), session.home.clone()));
        let reservation = host.begin_project_switch()?;
        // Incoming session/tab deliveries queue while the switch is pending, and
        // in-flight shell or close operations observe the new generation.
        this.with_mut(|session| {
            session.switching = true;
            session.project_generation += 1;
            session.shell_creates = 0;
            session.initial_shell_in_flight = false;
        });
        let outcome = async {
            let (mut reservation, mut prepared, data) = tokio::task::spawn_blocking(move || {
                let loaded = reservation.prepare(&project_id)?;
                let data = session_io::load(&home, &project_id)?;
                let mut model = LayoutModel::new(loaded.project)?;
                model.mark_unknown_panes(&loaded.unknown_panes);
                model.dock(size.unwrap_or(Extent {
                    width: 1220.0,
                    height: 780.0,
                }));
                Ok::<_, String>((
                    reservation,
                    PreparedSwitch {
                        model: Some(model),
                        persistence: Some(Arc::new(loaded.persistence)),
                        notice: loaded.notice,
                    },
                    data,
                ))
            })
            .await
            .map_err(|error| format!("broken: {error}"))??;
            if this.is_disposed() {
                return Err("unavailable: layout session is disposed".into());
            }
            let project = prepared
                .model
                .as_ref()
                .expect("prepared layout")
                .storage_project();
            reservation.commit(
                prepared
                    .persistence
                    .as_ref()
                    .expect("prepared persistence")
                    .clone(),
                project,
                prepared.notice.is_none(),
                || {
                    this.with_mut(|session| {
                        session.model = prepared.model.take().expect("prepared layout");
                        session.notice = prepared.notice.take();
                        session.host.publish_focus(false, false);
                        session.data = data;
                        session.inventory_initialized = false;
                        session.initial_shell_pending = false;
                        session.session_notice = None;
                        session.queued = None;
                        session.first_draw = Some(Instant::now());
                    })
                },
            )
        }
        .await;
        if this.is_disposed() {
            return outcome.and(Err("unavailable: layout session is disposed".into()));
        }
        this.with_mut(|session| {
            session.switching = false;
            // Adoption, hidden-tab filtering and shared order apply to the
            // destination after the commit, as ordinary persisted edits.
            let result = outcome.and_then(|()| session.reconcile(true));
            // A failed switch resumes incoming snapshots for the source.
            let dock = session.apply_queued();
            result.and(dock)
        })
    }

    /// Activate a pane without changing its selected tab (including empty panes).
    pub fn select_pane(&mut self, pane_id: u64) -> Result<Value, String> {
        self.edit(|model| model.select_pane(pane_id).map(|next| (next, true)))
    }

    /// Split the active pane scaffold without creating a terminal process.
    pub fn split_active(&mut self, down: bool) -> Result<Value, String> {
        self.edit(|model| model.split_active(down).map(|next| (next, true)))
    }

    /// Close a placeholder in this layout; shared sessions remain running.
    pub fn close_active(&mut self, whole_pane: bool) -> Result<Value, String> {
        self.edit(|model| model.close_active(whole_pane).map(|next| (next, true)))
    }

    /// Set runtime-only focus presentation; the host restart snapshot carries it.
    pub fn set_focus_mode(&mut self, focused: bool, centered: bool) -> Result<Value, String> {
        let size = self.last_extent;
        self.edit(|model| {
            model.restore_focus(focused, centered);
            Ok((model.dock(size), false))
        })
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
    /// Place or select an existing chat by the main-pane rules (SHELL-215, SHELL-219).
    pub fn open_chat(&mut self, chat_id: String) -> Result<Value, String> {
        self.edit(|m| m.open_chat(&chat_id).map(|d| (d, true)))
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
            "projectId": self.host.project_id(),
            "selectedWorktreeId": project.selected_worktree_id,
            "selectedTaskId": project.selected_task_id,
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
            .map_err(layout_error)
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

    const OUTGOING: &str = "10000000-0000-4000-8000-000000000001";
    const DESTINATION: &str = "10000000-0000-4000-8000-000000000002";
    const TASK: &str = "10000000-0000-4000-8000-000000000003";
    const WORKTREE: &str = "10000000-0000-4000-8000-000000000004";
    const SHELL: &str = "10000000-0000-4000-8000-000000000005";

    fn session(home: &std::path::Path) -> LayoutSession {
        let mut session = open_fixture(
            home.to_owned(),
            json!({"windowId": home.display().to_string(), "projectId": OUTGOING}),
        )
        .unwrap();
        session.model.dock(Extent {
            width: 1400.0,
            height: 900.0,
        });
        session
    }

    #[test]
    fn project_switch_keeps_the_host_handle_and_flushes_both_project_writers() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _entered = runtime.enter();
        let fixture = tempfile::Builder::new()
            .prefix("riwork-layout-project-switch-")
            .tempdir_in("/tmp")
            .unwrap();
        let store = riwork_core::layouts::LayoutStore::open(fixture.path()).unwrap();
        let mut destination = persistence::default_project_layout();
        destination
            .panes
            .get_mut(&2)
            .unwrap()
            .tabs
            .push(SavedTab::Shell {
                shell_id: "destination-saved-shell".into(),
            });
        destination.normalize().unwrap();
        store
            .save_chat_state(DESTINATION, &destination, &[], None)
            .unwrap();
        let mut session = session(fixture.path());
        session.open_panel("tasks".into()).unwrap();
        session.set_selected_task(Some(TASK.into())).unwrap();
        session
            .set_selected_worktree(Some(WORKTREE.into()))
            .unwrap();
        let previous = session.project_from_host();
        let expected = destination.carry_locked_regions_from(&previous).unwrap();
        let host = session.host.clone();
        let instance = Instance::new(session);
        let dock = runtime
            .block_on(LayoutSession::switch_project(
                instance.clone(),
                DESTINATION.into(),
            ))
            .unwrap();
        assert!(!dock.is_null());
        assert!(Arc::ptr_eq(
            &host,
            &host_layouts::lookup(&fixture.path().display().to_string()).unwrap()
        ));
        assert_eq!(host.project_id(), DESTINATION);
        assert_eq!(host.snapshot(), expected);
        assert_eq!(store.load(OUTGOING).unwrap(), Some(previous.clone()));
        instance.with_mut(|session| {
            assert_eq!(session.snapshot()["projectId"], DESTINATION);
            session.set_selected_task(Some(TASK.into())).unwrap();
            session.show_shell(SHELL.into()).unwrap();
            session.show_shell(SHELL.into()).unwrap();
        });
        let committed = host.snapshot();
        host.begin_close();
        assert!(
            instance
                .with_mut(|session| session.set_selected_task(None))
                .is_err()
        );
        host.flush_blocking().unwrap();
        assert_eq!(store.load(DESTINATION).unwrap(), Some(committed.clone()));
        assert_eq!(store.load(OUTGOING).unwrap(), Some(previous));
        assert_eq!(
            committed
                .panes
                .values()
                .flat_map(|pane| &pane.tabs)
                .filter(|tab| tab.key() == format!("shell:{SHELL}"))
                .count(),
            1
        );
        instance.dispose();
    }

    #[test]
    fn failed_project_preparation_preserves_the_prior_editable_session() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _entered = runtime.enter();
        let fixture = tempfile::Builder::new()
            .prefix("riwork-layout-switch-failure-")
            .tempdir_in("/tmp")
            .unwrap();
        let session = session(fixture.path());
        let host = session.host.clone();
        let previous = session.project_from_host();
        let instance = Instance::new(session);
        // This prevents the next load from opening its shared lock without
        // relying on permissions (the test can also run as an elevated user).
        std::fs::remove_file(fixture.path().join("layouts.lock")).unwrap();
        std::fs::create_dir(fixture.path().join("layouts.lock")).unwrap();
        assert!(
            runtime
                .block_on(LayoutSession::switch_project(
                    instance.clone(),
                    DESTINATION.into()
                ))
                .is_err()
        );
        assert_eq!(host.project_id(), OUTGOING);
        assert_eq!(host.snapshot(), previous);
        std::fs::remove_dir(fixture.path().join("layouts.lock")).unwrap();
        instance
            .with_mut(|session| session.set_selected_task(Some(TASK.into())))
            .unwrap();
        host.flush_blocking().unwrap();
        assert_eq!(
            riwork_core::layouts::LayoutStore::open(fixture.path())
                .unwrap()
                .load(OUTGOING)
                .unwrap()
                .unwrap()
                .selected_task_id
                .as_deref(),
            Some(TASK)
        );
        instance.dispose();
    }

    #[test]
    fn disposed_session_rejects_project_switch_without_panicking() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _entered = runtime.enter();
        let fixture = tempfile::Builder::new()
            .prefix("riwork-layout-switch-disposed-")
            .tempdir_in("/tmp")
            .unwrap();
        let instance = Instance::new(session(fixture.path()));
        instance.dispose();
        assert_eq!(
            runtime
                .block_on(LayoutSession::switch_project(instance, DESTINATION.into()))
                .unwrap_err(),
            "unavailable: layout session is disposed"
        );
    }

    #[test]
    fn disposal_while_project_io_is_pending_never_commits_the_prepared_writer() {
        use std::{future::Future, task::Poll};

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _entered = runtime.enter();
        let fixture = tempfile::Builder::new()
            .prefix("riwork-layout-switch-cancelled-")
            .tempdir_in("/tmp")
            .unwrap();
        let session = session(fixture.path());
        let host = session.host.clone();
        let previous = host.snapshot();
        let instance = Instance::new(session);
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(fixture.path().join("layouts.lock"))
            .unwrap();
        fs2::FileExt::lock_exclusive(&lock).unwrap();
        let mut switch = Box::pin(LayoutSession::switch_project(
            instance.clone(),
            DESTINATION.into(),
        ));
        // The real store file lock keeps the blocking IO pending after the
        // engine has reserved the switch, with no timing-dependent sleeps.
        runtime.block_on(std::future::poll_fn(|cx| match switch.as_mut().poll(cx) {
            Poll::Pending => Poll::Ready(()),
            Poll::Ready(_) => panic!("the store lock must keep preparation pending"),
        }));
        instance.dispose();
        fs2::FileExt::unlock(&lock).unwrap();
        assert_eq!(
            runtime.block_on(switch).unwrap_err(),
            "unavailable: layout session is disposed"
        );
        assert_eq!(host.project_id(), OUTGOING);
        assert_eq!(host.snapshot(), previous.clone());
        host.flush_blocking().unwrap();
        let store = riwork_core::layouts::LayoutStore::open(fixture.path()).unwrap();
        assert_eq!(store.load(OUTGOING).unwrap(), Some(previous));
        assert!(store.load(DESTINATION).unwrap().is_none());
        // A cancelled preparation also releases the short edit admission gate.
        host.schedule(host.snapshot()).unwrap();
        host.flush_blocking().unwrap();
    }

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
    fn ended_registered_shells_keep_their_tabs_until_explicit_hide_or_registry_pruning() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _entered = runtime.enter();
        let (_home, mut session) = isolated_session();
        session.open_shell(SHELL.into()).unwrap();
        let ended_data = |session: &LayoutSession, revision, alive, hidden| {
            let mut data = snapshot_data(session, "ended", revision);
            data.shells = Some(vec![session_io::Shell {
                id: SHELL.into(),
                project_id: Some("delivery-project".into()),
                worktree_id: None,
                kind: "project".into(),
                cwd: session.home.clone(),
                alive,
                user_opened: true,
                parent_id: Some(OUTGOING.into()),
                title: "Nested shell".into(),
                status: Some(Status::Stopped),
                appear_after_ms: 0,
            }]);
            data.tabs.as_mut().unwrap().entries = vec![serde_json::from_value(json!({
                "key":format!("shell:{SHELL}"),"kind":"shell","title":"Nested shell", "status":if alive { "idle" } else { "stopped" },
                "hidden":hidden,"worker":true,"order":0,"parent":null,"children":[],"child_count":0
            })).unwrap()];
            data
        };
        session
            .accept_data(ended_data(&session, 1, true, false))
            .unwrap();
        session
            .accept_data(ended_data(&session, 2, false, false))
            .unwrap();
        assert!(
            session.model.panel(&format!("shell:{SHELL}")).is_some(),
            "a live-to-ended nested tab keeps its last surface"
        );
        session
            .accept_data(ended_data(&session, 3, false, true))
            .unwrap();
        assert!(
            session.model.panel(&format!("shell:{SHELL}")).is_none(),
            "explicit Hide still removes the tab"
        );
        session.open_shell(SHELL.into()).unwrap();
        let mut pruned = snapshot_data(&session, "ended", 4);
        pruned.shells = Some(Vec::new());
        session.accept_data(pruned).unwrap();
        assert!(
            session.model.panel(&format!("shell:{SHELL}")).is_none(),
            "a pruned editor registry row leaves the layout"
        );

        let (_other_home, mut restoring) = isolated_session();
        restoring.open_shell(SHELL.into()).unwrap();
        restoring.inventory_initialized = false;
        restoring
            .accept_data(ended_data(&restoring, 1, false, false))
            .unwrap();
        assert!(
            restoring.model.panel(&format!("shell:{SHELL}")).is_none(),
            "initial restore still prunes already-dead saved tabs"
        );
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
            inventory_initialized: false,
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
        session.set_focus_mode(true, true).unwrap();
        assert_eq!(host.focus_state(), (true, true));
        host.begin_close();
        assert_eq!(
            session.set_focus_mode(false, false).unwrap_err(),
            "unavailable: window is closing"
        );
        assert_eq!(host.focus_state(), (true, true));
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
