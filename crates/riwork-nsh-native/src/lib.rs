//! Shared domain services and layout classes in the frozen `riwork/native` NSH module.
//!
//! The host awaits [`initialize`] before constructing AppViews, then registers
//! `[nsh_base::registry(), registry()]` once. All windows share the same process
//! service; neither subscription registration nor delivery does domain I/O on an
//! engine thread. See the crate README for lifecycle and test instructions.
pub mod app_preview;
pub mod appearance;
pub mod backend;
pub mod chat;
pub mod cua;
pub mod dashboard;
pub mod dto;
pub mod files;
pub mod host_flush;
pub mod hub;
mod memo;
pub mod orchestrators;
pub mod overview;
mod presence;
mod preview_input;
#[cfg(target_os = "macos")]
pub mod preview_macos;
pub mod preview_surface;
pub mod project_settings;
pub mod remote;
pub mod schedules;
pub mod service;
pub mod settings;
pub mod settings_aux;
pub mod tabs;
#[cfg(feature = "test-support")]
pub mod testing;
pub mod usage;

use futures_util::future::{AbortHandle, Abortable};
use nsh_core::{element::Cleanup, prelude::*};

use dto::{
    CreateSessionRequest, CreateSubshellRequest, FolderDto, OpenEditorRequest, ProjectDto,
    ProjectInspectionDto, ProjectMemoDto, ProjectRequest, SearchHitDto, SessionsSnapshot,
    ShellSessionDto, StoreState, TabUpdate, TabsSnapshot, TaskDto, WorktreeDto,
};
use hub::{Snapshot, TopicSubscription};
pub use service::{ProcessService, initialize};

#[native_module("riwork/native")]
fn native(registry: &mut Registry) {
    registry
        .module::<Store>()
        .module::<Sessions>()
        .module::<Tabs>()
        .module::<files::Files>()
        .module::<settings::Settings>()
        .module::<project_settings::ProjectSettings>()
        .module::<settings_aux::ApiKey>()
        .module::<settings_aux::OrcaImport>()
        .module::<appearance::Appearance>()
        .module::<dashboard::Dashboard>()
        .module::<orchestrators::Orchestrators>()
        .module::<chat::Chat>()
        .module::<usage::Usage>()
        .module::<usage::Accounts>()
        .module::<cua::Cua>()
        .module::<app_preview::Previews>()
        .class::<app_preview::PreviewHandle>()
        .module::<schedules::Schedules>()
        .module::<remote::Remote>()
        .class::<chat::ChatHandle>()
        .class::<Subscription>();
    riwork_nsh_workspace::register_layouts(registry, service::process_home);
}

/// Durable project, folder, worktree and task operations.
#[module]
pub struct Store;

#[module]
impl Store {
    pub async fn default_project_directory() -> Result<String, String> {
        initialize().await?.default_project_directory().await
    }
    pub async fn create_project(request: ProjectRequest) -> Result<ProjectDto, String> {
        initialize().await?.create_project(request).await
    }
    pub async fn create_new_project(request: ProjectRequest) -> Result<ProjectDto, String> {
        initialize().await?.create_new_project(request).await
    }
    pub async fn add_project(request: ProjectRequest) -> Result<ProjectDto, String> {
        initialize().await?.add_project(request).await
    }
    pub async fn use_project(selector: String) -> Result<ProjectDto, String> {
        initialize().await?.use_project(selector).await
    }
    pub async fn rename_project(selector: String, name: String) -> Result<ProjectDto, String> {
        initialize().await?.rename_project(selector, name).await
    }
    pub fn stage_project_memo(project: String, text: String) -> Result<u32, String> {
        memo::stage(&project, &text)
    }
    pub async fn set_project_memo(
        project: String,
        text: String,
        revision: Option<u32>,
    ) -> Result<ProjectMemoDto, String> {
        initialize()
            .await?
            .set_project_memo(project, text, revision)
            .await
    }
    pub async fn update_project_metadata(
        project: String,
        name: String,
        folder: Option<String>,
    ) -> Result<ProjectDto, String> {
        initialize()
            .await?
            .update_project_metadata(project, name, folder)
            .await
    }
    pub async fn set_project_codex_account(
        project: String,
        choice: dto::ProjectCodexAccountDto,
    ) -> Result<ProjectDto, String> {
        initialize()
            .await?
            .set_project_codex_account(project, choice)
            .await
    }
    pub async fn set_project_orchestrator(
        project: String,
        choice: nsh_core::JsonValue,
    ) -> Result<ProjectDto, String> {
        initialize()
            .await?
            .set_project_orchestrator(project, choice)
            .await
    }
    pub async fn set_project_delegation_policy(
        project: String,
        patch: settings::DelegationPatch,
    ) -> Result<ProjectDto, String> {
        initialize()
            .await?
            .set_project_delegation_policy(project, patch)
            .await
    }
    pub async fn set_project_notifications(
        selector: String,
        enabled: bool,
    ) -> Result<ProjectDto, String> {
        initialize()
            .await?
            .set_project_notifications(selector, enabled)
            .await
    }
    pub async fn set_project_orchestrator_wakes(
        selector: String,
        enabled: bool,
    ) -> Result<ProjectDto, String> {
        initialize()
            .await?
            .set_project_orchestrator_wakes(selector, enabled)
            .await
    }
    pub async fn create_folder(name: String, parent: Option<String>) -> Result<FolderDto, String> {
        initialize().await?.create_folder(name, parent).await
    }
    pub async fn rename_folder(selector: String, name: String) -> Result<FolderDto, String> {
        initialize().await?.rename_folder(selector, name).await
    }
    pub async fn move_folder(
        selector: String,
        parent: Option<String>,
    ) -> Result<FolderDto, String> {
        initialize().await?.move_folder(selector, parent).await
    }
    pub async fn remove_folder(selector: String) -> Result<(), String> {
        initialize().await?.remove_folder(selector).await
    }
    pub async fn move_project_to_folder(
        selector: String,
        folder: Option<String>,
    ) -> Result<ProjectDto, String> {
        initialize()
            .await?
            .move_project_to_folder(selector, folder)
            .await
    }
    pub async fn sync_worktrees(selector: String) -> Result<Vec<WorktreeDto>, String> {
        initialize().await?.sync_worktrees(selector).await
    }
    pub async fn refresh_worktrees(selector: String) -> Result<Vec<WorktreeDto>, String> {
        initialize().await?.refresh_worktrees(selector).await
    }
    pub async fn add_task(
        project: String,
        title: String,
        details: String,
    ) -> Result<TaskDto, String> {
        initialize().await?.add_task(project, title, details).await
    }
    pub async fn assign_tasks(
        worktree: String,
        tasks: Vec<String>,
    ) -> Result<Vec<TaskDto>, String> {
        initialize().await?.assign_tasks(worktree, tasks).await
    }
    pub async fn unassign_tasks(tasks: Vec<String>) -> Result<Vec<TaskDto>, String> {
        initialize().await?.unassign_tasks(tasks).await
    }
    pub async fn set_task_status(selector: String, status: String) -> Result<TaskDto, String> {
        initialize().await?.set_task_status(selector, status).await
    }
    pub async fn snapshot() -> Result<StoreState, String> {
        initialize().await?.store_snapshot().await
    }

    pub async fn search(query: String) -> Result<Vec<SearchHitDto>, String> {
        initialize().await?.store_search(query).await
    }

    pub async fn inspect_project(root: String) -> Result<ProjectInspectionDto, String> {
        initialize().await?.inspect_project(root.into()).await
    }

    /// First delivers a snapshot, then pushes changed snapshots until closed or
    /// the rendering owner goes. The injected context is invisible in TypeScript.
    pub fn subscribe(
        listener: Callback<Snapshot<StoreState>>,
        cx: CallCx<'_>,
    ) -> Result<Subscription, String> {
        Ok(Subscription::start(cx, async move {
            if let Ok(service) = initialize().await {
                if let Ok(subscription) = service.subscribe_store().await {
                    deliver(subscription, listener).await;
                }
            }
        }))
    }
}

/// Persistent shell inventory; attaching terminal views belongs to the host.
#[module]
pub struct Sessions;

#[module]
impl Sessions {
    pub async fn list() -> Result<SessionsSnapshot, String> {
        initialize().await?.sessions_list().await
    }

    pub async fn create(request: CreateSessionRequest) -> Result<ShellSessionDto, String> {
        initialize().await?.sessions_create(request).await
    }

    pub async fn create_subshell(
        request: CreateSubshellRequest,
    ) -> Result<ShellSessionDto, String> {
        initialize().await?.create_subshell(request).await
    }

    pub async fn open_editor(request: OpenEditorRequest) -> Result<ShellSessionDto, String> {
        initialize().await?.open_editor(request).await
    }

    /// Explicit Exit only. Closing/detaching a tab must never call this method.
    pub async fn close(id: String) -> Result<(), String> {
        initialize().await?.sessions_close(id).await
    }

    pub fn subscribe(
        listener: Callback<Snapshot<SessionsSnapshot>>,
        cx: CallCx<'_>,
    ) -> Result<Subscription, String> {
        Ok(Subscription::start(cx, async move {
            if let Ok(service) = initialize().await {
                if let Ok(subscription) = service.subscribe_sessions().await {
                    deliver(subscription, listener).await;
                }
            }
        }))
    }
}

/// Process-shared tab membership. Window-local selection stays in TSX/layouts.
#[module]
pub struct Tabs;

#[module]
impl Tabs {
    pub async fn update(project_id: String, update: TabUpdate) -> Result<TabsSnapshot, String> {
        initialize().await?.tabs_update(project_id, update).await
    }

    pub fn subscribe(
        project_id: String,
        listener: Callback<Snapshot<TabsSnapshot>>,
        cx: CallCx<'_>,
    ) -> Result<Subscription, String> {
        service::validate_project_id(&project_id)?;
        Ok(Subscription::start(cx, async move {
            if let Ok(service) = initialize().await {
                if let Ok(subscription) = service.subscribe_tabs(project_id).await {
                    deliver(subscription, listener).await;
                }
            }
        }))
    }
}

/// A live subscription. Closing, disposal, collection and owner teardown end its
/// engine task, dropping and closing the bounded topic queue.
#[class]
pub struct Subscription {
    abort: AbortHandle,
    cleanup: Option<Cleanup>,
}

impl Subscription {
    fn start(cx: CallCx<'_>, work: impl std::future::Future<Output = ()> + 'static) -> Self {
        let (abort, registration) = AbortHandle::new_pair();
        let owner_abort = abort.clone();
        let cleanup = cx.on_cleanup(move || owner_abort.abort());
        // A synchronous module call is outside the core's ambient runtime.
        // CallCx queues work in order and binds cleanup to the actual JS owner.
        cx.start(async move {
            let _ = Abortable::new(work, registration).await;
        });
        Self {
            abort,
            cleanup: Some(cleanup),
        }
    }
}

#[class]
impl Subscription {
    pub fn close(&mut self) {
        self.abort.abort();
        if let Some(cleanup) = self.cleanup.take() {
            cleanup.release();
        }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        // VM teardown clears NSH's context before finalizing objects retained
        // by reactive cleanup closures. Cleanup::release reaches that context;
        // it is valid only for explicit close() while JS is running. Dropping
        // Cleanup keeps its owner callback registered and safely drops the
        // unregister closure. Abort independently in either teardown order.
        self.abort.abort();
    }
}

async fn deliver<T: 'static>(subscription: TopicSubscription<T>, listener: Callback<Snapshot<T>>)
where
    Snapshot<T>: JsValue,
{
    while let Ok(snapshot) = subscription.recv().await {
        listener.post(subscription.latest(snapshot));
        // A single requested frame gates the next delivery to another engine
        // turn. Once quiet we await the channel; no idle frame loop or polling.
        // A plain yield would allow multiple posts in one core task-drain turn.
        next_frame().await;
    }
}

#[cfg(test)]
mod tests {
    use nsh_core::element::{FunctionKind, declarations};

    #[test]
    fn native_registry_composes_services_and_layouts_in_one_frozen_module() {
        let registry = super::registry();
        assert_eq!(registry.specifier(), Some("riwork/native"));
        let modules = registry.module_specs();
        assert_eq!(
            modules.iter().map(|module| module.name).collect::<Vec<_>>(),
            [
                "Accounts",
                "ApiKey",
                "Appearance",
                "Chat",
                "Cua",
                "Dashboard",
                "Files",
                "Layouts",
                "OrcaImport",
                "Orchestrators",
                "Previews",
                "ProjectSettings",
                "Remote",
                "Schedules",
                "Sessions",
                "Settings",
                "Store",
                "Tabs",
                "Usage"
            ]
        );
        for module in modules {
            for function in &module.functions {
                let expected = if matches!(
                    function.name,
                    "subscribe"
                        | "sentenceCase"
                        | "cased"
                        | "open"
                        | "stageProjectMemo"
                        | "newEditor"
                ) {
                    FunctionKind::Sync
                } else {
                    FunctionKind::Async
                };
                assert_eq!(function.kind, expected, "{}.{}", module.name, function.name);
                assert!(function.params.iter().all(|(name, _, _)| *name != "cx"));
            }
        }
        let types = declarations(&registry, &[]);
        for declaration in [
            "snapshot(): Promise<StoreState>",
            "createSubshell(request: CreateSubshellRequest): Promise<ShellSessionDto>",
            "openEditor(request: OpenEditorRequest): Promise<ShellSessionDto>",
            "inspectProject(root: string): Promise<ProjectInspectionDto>",
            "StoreEvent",
            "SessionsEvent",
            "resync?: boolean",
            "projectFolders",
            "openSession(request: unknown): Promise<LayoutSession>",
            "class LayoutSession",
            "newEditor(): number",
            "class Subscription",
        ] {
            assert!(
                types.contains(declaration),
                "missing {declaration}: {types}"
            );
        }
    }
}
