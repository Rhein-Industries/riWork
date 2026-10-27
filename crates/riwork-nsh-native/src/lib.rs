//! Phase 1a read side of the frozen `riwork/native` NSH module.
//!
//! The host awaits [`initialize`] before constructing AppViews, then registers
//! `[nsh_base::registry(), registry()]` once. All windows share the same process
//! service; neither subscription registration nor delivery does domain I/O on an
//! engine thread. See the crate README for lifecycle and test instructions.
pub mod backend;
pub mod dto;
pub mod hub;
pub mod service;

use futures_util::future::{AbortHandle, Abortable};
use nsh_core::{element::Cleanup, prelude::*};

use dto::{ProjectInspectionDto, SearchHitDto, SessionsSnapshot, StoreState};
use hub::{Snapshot, TopicSubscription};
pub use service::{ProcessService, initialize};

#[native_module("riwork/native")]
fn native(registry: &mut Registry) {
    registry
        .module::<Store>()
        .module::<Sessions>()
        .class::<Subscription>();
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
        self.close();
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
    fn native_registry_exports_the_frozen_read_surface_and_typed_events() {
        let registry = super::registry();
        assert_eq!(registry.specifier(), Some("riwork/native"));
        let modules = registry.module_specs();
        assert_eq!(
            modules.iter().map(|module| module.name).collect::<Vec<_>>(),
            ["Sessions", "Store"]
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
        ] {
            assert!(
                types.contains(declaration),
                "missing {declaration}: {types}"
            );
        }
    }
}
