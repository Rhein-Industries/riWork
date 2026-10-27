//! Phase 1 services and layout classes in the frozen `riwork/native` NSH module.
//!
//! The host awaits [`initialize`] before constructing AppViews, then registers
//! `[nsh_base::registry(), registry()]` once. All windows share the same process
//! service; neither subscription registration nor delivery does domain I/O on an
//! engine thread. See the crate README for lifecycle and test instructions.
pub mod appearance;
pub mod backend;
pub mod dto;
pub mod hub;
pub mod service;

use futures_util::future::{AbortHandle, Abortable};
use nsh_core::{element::Cleanup, prelude::*};

use dto::{
    CreateSessionRequest, ProjectInspectionDto, SearchHitDto, SessionsSnapshot, ShellSessionDto,
    StoreState,
};
use hub::{Snapshot, TopicSubscription};
pub use service::{ProcessService, initialize};

#[native_module("riwork/native")]
fn native(registry: &mut Registry) {
    registry
        .module::<Appearance>()
        .module::<Store>()
        .module::<Sessions>()
        .module::<Tabs>()
        .class::<Subscription>();
    riwork_nsh_workspace::register_layouts(registry, service::process_home);
}

/// Durable project, folder, worktree and task reads.
#[module]
pub struct Store;

#[module]
impl Store {
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
        let service = service::initialized()?;
        Ok(Subscription::start(cx, async move {
            if let Ok(subscription) = service.subscribe_store().await {
                deliver(subscription, listener).await;
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

    /// Explicit Exit only. Closing/detaching a tab must never call this method.
    pub async fn close(id: String) -> Result<(), String> {
        initialize().await?.sessions_close(id).await
    }

    pub fn subscribe(
        listener: Callback<Snapshot<SessionsSnapshot>>,
        cx: CallCx<'_>,
    ) -> Result<Subscription, String> {
        let service = service::initialized()?;
        Ok(Subscription::start(cx, async move {
            if let Ok(subscription) = service.subscribe_sessions().await {
                deliver(subscription, listener).await;
            }
        }))
    }
}

/// Shared local shell membership. Projection subscriptions remain a later slice.
#[module]
pub struct Tabs;

#[module]
impl Tabs {
    /// Hide/unhide/order/title edits through the existing engine TabStore.
    pub async fn update(
        project_id: String,
        update: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        initialize().await?.tabs_update(project_id, update).await
    }
}

/// Settings-backed terminal theme and close preference, pushed once per process.
#[module]
pub struct Appearance;

#[module]
impl Appearance {
    pub async fn snapshot() -> Result<appearance::AppearanceSnapshot, String> {
        initialize().await?.appearance_snapshot().await
    }

    pub fn subscribe(
        listener: Callback<Snapshot<appearance::AppearanceSnapshot>>,
        cx: CallCx<'_>,
    ) -> Result<Subscription, String> {
        let service = service::initialized()?;
        Ok(Subscription::start(cx, async move {
            if let Ok(subscription) = service.subscribe_appearance().await {
                deliver(subscription, listener).await;
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
            ["Appearance", "Layouts", "Sessions", "Store", "Tabs"]
        );
        for module in modules {
            for function in &module.functions {
                let expected = if function.name == "subscribe" {
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
            "inspectProject(root: string): Promise<ProjectInspectionDto>",
            "StoreEvent",
            "SessionsEvent",
            "resync?: boolean",
            "projectFolders",
            "openSession(request: unknown): Promise<LayoutSession>",
            "class LayoutSession",
            "class Subscription",
        ] {
            assert!(
                types.contains(declaration),
                "missing {declaration}: {types}"
            );
        }
    }
}
