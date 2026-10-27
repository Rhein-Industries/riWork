//! Workspace layout classes composed into `riwork-nsh-native`'s sole UI set.
//! The native crate owns the process home; this crate owns layout state and IO.
pub mod host_layouts;
pub mod layout;
pub mod persistence;

use layout::{DockAreaLayout, Extent, LayoutModel};
use nsh_core::prelude::*;
use persistence::{LayoutPersistence, SaveMode};
use riwork_core::layouts::{PanelKind, ProjectLayout, SavedTab};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{Arc, OnceLock},
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
        let loaded = LayoutPersistence::open(home, project_id, restore, SaveMode::LegacyChatState)?;
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
    registry.module::<Layouts>().class::<LayoutSession>();
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenSession {
    window_id: String,
    project_id: String,
    restore: Option<ProjectLayout>,
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

/// Layout IO runs in Rust's blocking pool. JS never receives a filesystem path.
#[module]
pub struct Layouts;

#[module]
impl Layouts {
    /// Open the project in the process's fixed home, honoring a reload snapshot.
    pub async fn open_session(request: Value) -> Result<LayoutSession, String> {
        tokio::task::spawn_blocking(move || {
            let request: OpenSession =
                serde_json::from_value(request).map_err(|e| format!("invalid: {e}"))?;
            if request.window_id.is_empty() || request.project_id.is_empty() {
                return Err("invalid: windowId and projectId are required".into());
            }
            let loaded = LAYOUT_REPOSITORY
                .get_or_init(|| Arc::new(CoreLayoutRepository))
                .open(process_home()?, request.project_id, request.restore)?;
            let mut model = LayoutModel::new(loaded.project)?;
            model.mark_unknown_panes(&loaded.unknown_panes);
            let host = host_layouts::registration_backend(
                &request.window_id,
                loaded.backend,
                model.storage_project(),
            )?;
            // Successful loads, including a reload payload that supersedes disk,
            // become durable even if the user makes no edit before closing.
            // Recovery defaults must not silently replace unreadable data.
            if loaded.notice.is_none() {
                host.schedule(model.storage_project())?;
            }
            Ok(LayoutSession {
                model,
                host,
                notice: loaded.notice,
            })
        })
        .await
        .map_err(|e| format!("broken: {e}"))?
        .map_err(layout_error)
    }
}

/// Window-owned in-memory authority; DockArea receives only its projections.
#[class]
pub struct LayoutSession {
    model: LayoutModel,
    host: Arc<host_layouts::HostLayout>,
    notice: Option<String>,
}

impl LayoutSession {
    fn edit(
        &mut self,
        operation: impl FnOnce(&mut LayoutModel) -> Result<(DockAreaLayout, bool), String>,
    ) -> Result<Value, String> {
        let host = Arc::clone(&self.host);
        host.edit(|| {
            let (next, persist) = operation(&mut self.model)?;
            Ok((json!(next), persist.then(|| self.model.storage_project())))
        })
        .map_err(layout_error)
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
        self.edit(|model| Ok((model.dock(size), false)))
    }

    /// Replay one DockArea gesture using core rules, then return its projection.
    pub fn apply_dock(&mut self, next: Value, size: Value) -> Result<Value, String> {
        let next: DockAreaLayout =
            serde_json::from_value(next).map_err(|e| format!("invalid: {e}"))?;
        let size = extent(size)?;
        self.edit(|model| model.apply_dock(next, size).map(|next| (next, true)))
    }

    /// Preserve locked pixel extents through the core's window-resize algorithm.
    pub fn set_extent(&mut self, size: Value) -> Result<Value, String> {
        let size = extent(size)?;
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

    pub fn open_panel(&mut self, panel: String) -> Result<Value, String> {
        let panel: PanelKind = serde_json::from_value(json!(panel.trim_start_matches("panel:")))
            .map_err(|error| format!("invalid: {error}"))?;
        self.edit(|model| model.open_panel(panel).map(|next| (next, true)))
    }

    pub fn default_layout(&mut self) -> Result<Value, String> {
        self.edit(|model| model.default_layout().map(|next| (next, true)))
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

    /// Tab/pane presentation metadata. Persistence uses the separate core snapshot.
    pub fn snapshot(&self) -> Value {
        let project = self.model.project();
        let locked = project.effective_locked_panes();
        let ids = project.layout.pane_ids();
        json!({
            "activePane": project.active_pane,
            "selectedWorktreeId": project.selected_worktree_id,
            "notice": self.notice,
            "panes": ids.into_iter().map(|id| {
                let pane = &project.panes[&id];
                json!({ "id": id, "locked": locked.contains(&id), "main": project.main_pane == Some(id),
                    "active": project.active_pane == id, "activeTab": pane.active_tab_key,
                    "tabs": pane.tabs.iter().map(|tab| json!({ "id": tab.key(),
                        "kind": match tab { SavedTab::Shell{..} => "shell", SavedTab::Panel{..} => "panel",
                            SavedTab::RemoteShell{..} => "remote_shell", SavedTab::Chat{..} => "chat", SavedTab::AppPreview{..} => "app_preview" },
                        "title": tab_title(tab)
                    })).collect::<Vec<_>>() })
            }).collect::<Vec<_>>()
        })
    }

    /// Frozen panel lookup; empty sentinels have no persisted SavedTab.
    pub fn panel(&self, panel_id: String) -> Value {
        self.model
            .project()
            .panes
            .values()
            .flat_map(|pane| &pane.tabs)
            .find(|tab| tab.key() == panel_id)
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
