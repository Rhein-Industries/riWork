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

/// A cancelled JS promise can release its result on an engine thread. Keep
/// persistence behind the same deferred-disposal guard as the host handle.
struct PreparedSwitch {
    model: Option<LayoutModel>,
    persistence: Option<Arc<LayoutPersistence>>,
    notice: Option<String>,
    dock: DockAreaLayout,
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

    /// Place or select one existing local shell without creating a session.
    pub fn open_shell(&mut self, shell_id: String) -> Result<Value, String> {
        self.edit(|model| model.open_shell(&shell_id).map(|next| (next, true)))
    }

    /// Remove this window's tab while preserving its persistent tmux session.
    pub fn detach_shell(&mut self, shell_id: String) -> Result<Value, String> {
        self.edit(|model| model.detach_shell(&shell_id).map(|next| (next, true)))
    }

    pub fn default_layout(&mut self) -> Result<Value, String> {
        self.edit(|model| model.default_layout().map(|next| (next, true)))
    }

    /// Open a navigation panel or select its existing stable tab identity.
    pub fn open_panel(&mut self, panel_id: String) -> Result<Value, String> {
        let name = panel_id.strip_prefix("panel:").unwrap_or(&panel_id);
        let panel: PanelKind =
            serde_json::from_value(json!(name)).map_err(|_| "invalid: unknown panel".to_owned())?;
        self.edit(|model| model.open_panel(panel).map(|next| (next, true)))
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
        let size = this.with(|session| session.model.extent());
        let reservation = host.begin_project_switch()?;
        let (mut reservation, mut prepared) = tokio::task::spawn_blocking(move || {
            let loaded = reservation.prepare(&project_id)?;
            let mut model = LayoutModel::new(loaded.project)?;
            model.mark_unknown_panes(&loaded.unknown_panes);
            let dock = model.dock(size.unwrap_or(Extent {
                width: 1220.0,
                height: 780.0,
            }));
            Ok::<_, String>((
                reservation,
                PreparedSwitch {
                    model: Some(model),
                    persistence: Some(Arc::new(loaded.persistence)),
                    notice: loaded.notice,
                    dock,
                },
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
                    json!(prepared.dock)
                })
            },
        )
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
            "projectId": self.host.project_id(),
            "selectedWorktreeId": project.selected_worktree_id,
            "selectedTaskId": project.selected_task_id,
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

    const OUTGOING: &str = "10000000-0000-4000-8000-000000000001";
    const DESTINATION: &str = "10000000-0000-4000-8000-000000000002";
    const TASK: &str = "10000000-0000-4000-8000-000000000003";
    const WORKTREE: &str = "10000000-0000-4000-8000-000000000004";
    const SHELL: &str = "10000000-0000-4000-8000-000000000005";

    fn session(home: &std::path::Path) -> LayoutSession {
        let loaded =
            LayoutPersistence::open(home, OUTGOING, None, SaveMode::LegacyChatState).unwrap();
        let mut model = LayoutModel::new(loaded.project).unwrap();
        model.dock(Extent {
            width: 1400.0,
            height: 900.0,
        });
        let host = host_layouts::registration(
            &home.display().to_string(),
            Arc::new(loaded.persistence),
            model.storage_project(),
        )
        .unwrap();
        LayoutSession {
            model,
            host,
            notice: None,
        }
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
