//! Phase 1c mixed-app entry. P1a owns the final `riwork-nsh-native` registry.
//! This temporary registry exports the frozen specifier for isolated development;
//! integration must register Layouts/LayoutSession in that ONE registry, not both.
pub mod host_layouts;
pub mod layout;
pub mod persistence;

use layout::{DockAreaLayout, Extent, LayoutModel};
use nsh_core::prelude::*;
use persistence::{LayoutPersistence, SaveMode};
use riwork_core::layouts::{ProjectLayout, SavedTab};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, OnceLock},
};

#[native_module("riwork/native")]
fn native(registry: &mut Registry) {
    register_layouts(registry);
}

/// P1a calls this from its frozen `riwork-nsh-native` registry when integrated.
/// Register one `riwork/native` UI set; do not install the temporary one too.
pub fn register_layouts(registry: &mut Registry) {
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
    static HOME_DIR: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    HOME_DIR
        .get_or_init(|| {
            // P1a will supply its process service's fixed home. Until then, require
            // explicit launch isolation instead of silently using a user's live home.
            std::env::var_os("RIWORK_HOME")
                .filter(|p| !p.is_empty())
                .map(PathBuf::from)
                .ok_or_else(|| {
                    "unavailable: P1a registry pending; set an explicit RIWORK_HOME".into()
                })
        })
        .clone()
}

/// Layout IO runs in Rust's blocking pool. JS never receives a filesystem path.
#[module]
pub struct Layouts;

#[module]
impl Layouts {
    /// Open the project in the process's fixed home, honoring a reload snapshot.
    pub async fn open_session(request: Value) -> Result<LayoutSession, String> {
        let request: OpenSession =
            serde_json::from_value(request).map_err(|e| format!("invalid: {e}"))?;
        if request.window_id.is_empty() || request.project_id.is_empty() {
            return Err("invalid: windowId and projectId are required".into());
        }
        let home = process_home()?;
        tokio::task::spawn_blocking(move || {
            let loaded = LayoutPersistence::open(
                home,
                request.project_id,
                request.restore,
                SaveMode::LegacyChatState,
            )?;
            let mut model = LayoutModel::new(loaded.project)?;
            model.mark_unknown_panes(&loaded.unknown_panes);
            let host = host_layouts::registration(
                &request.window_id,
                Arc::new(loaded.persistence),
                model.storage_project(),
            )?;
            Ok(LayoutSession {
                model,
                host,
                notice: loaded.notice,
            })
        })
        .await
        .map_err(|e| format!("broken: {e}"))?
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
    fn schedule(&self) -> Result<(), String> {
        self.host.schedule(self.model.storage_project())
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
        Ok(json!(self.model.dock(extent(size)?)))
    }

    /// Replay one DockArea gesture using core rules, then return its projection.
    pub fn apply_dock(&mut self, next: Value, size: Value) -> Result<Value, String> {
        let next: DockAreaLayout =
            serde_json::from_value(next).map_err(|e| format!("invalid: {e}"))?;
        let next = self.model.apply_dock(next, extent(size)?)?;
        self.schedule()?;
        Ok(json!(next))
    }

    /// Preserve locked pixel extents through the core's window-resize algorithm.
    pub fn set_extent(&mut self, size: Value) -> Result<Value, String> {
        let before = self.model.project().clone();
        let next = self.model.set_extent(extent(size)?)?;
        if before != *self.model.project() {
            self.schedule()?;
        }
        Ok(json!(next))
    }

    /// Select a stable panel identity and its owning pane.
    pub fn select_panel(&mut self, panel_id: String) -> Result<Value, String> {
        let next = self.model.select_panel(&panel_id)?;
        self.schedule()?;
        Ok(json!(next))
    }

    /// Activate a pane without changing its selected tab (including empty panes).
    pub fn select_pane(&mut self, pane_id: u64) -> Result<Value, String> {
        let next = self.model.select_pane(pane_id)?;
        self.schedule()?;
        Ok(json!(next))
    }

    /// Toggle the explicit lock set, preserving the difference from automatic locks.
    pub fn toggle_lock(&mut self, pane_id: u64) -> Result<Value, String> {
        let next = self.model.toggle_lock(pane_id)?;
        self.schedule()?;
        Ok(json!(next))
    }

    /// Select or clear the project's unique main pane.
    pub fn toggle_main(&mut self, pane_id: u64) -> Result<Value, String> {
        let next = self.model.toggle_main(pane_id)?;
        self.schedule()?;
        Ok(json!(next))
    }

    /// Gather unlocked sources into main without closing any session.
    pub fn gather(&mut self) -> Result<Value, String> {
        let next = self.model.gather()?;
        self.schedule()?;
        Ok(json!(next))
    }

    /// Tab/pane presentation metadata. Persistence uses the separate core snapshot.
    pub fn snapshot(&self) -> Value {
        let project = self.model.project();
        let locked = project.effective_locked_panes();
        let ids = project.layout.pane_ids();
        json!({
            "activePane": project.active_pane,
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
