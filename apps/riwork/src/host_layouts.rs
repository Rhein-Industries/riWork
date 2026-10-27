//! Host-side handoff for a window's current layout and persistence barrier.
//!
//! The registry is consulted only while opening a session or from the host.
//! Publishing an edit never takes the registry mutex or waits on a file lock.
use crate::persistence::LayoutPersistence;
use arc_swap::ArcSwap;
use riwork_core::layouts::ProjectLayout;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock, Weak},
};

type Registry = HashMap<String, Weak<HostLayout>>;

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// A thread-safe host handle, independent of NSH's engine-owned class instance.
/// The host may retain it through AppView teardown to flush before terminating.
pub struct HostLayout {
    current: ArcSwap<ProjectLayout>,
    persistence: Option<Arc<LayoutPersistence>>,
}

/// Register after opening persistence on a blocking thread. A second live
/// session for the same window is a conflict rather than an implicit takeover.
pub fn registration(
    window_id: &str,
    persistence: Arc<LayoutPersistence>,
    snapshot: ProjectLayout,
) -> Result<Arc<HostLayout>, String> {
    if window_id.is_empty() {
        return Err("invalid: a host layout requires a windowId".into());
    }
    let mut entries = registry().lock().unwrap_or_else(|error| error.into_inner());
    entries.retain(|_, entry| entry.strong_count() != 0);
    if entries.get(window_id).and_then(Weak::upgrade).is_some() {
        return Err("conflict: a layout session is already registered for this window".into());
    }
    let handle = Arc::new(HostLayout {
        current: ArcSwap::from_pointee(snapshot),
        persistence: Some(persistence),
    });
    entries.insert(window_id.to_owned(), Arc::downgrade(&handle));
    Ok(handle)
}

/// Look up before closing an engine/window or publishing a restart snapshot.
/// Expired entries are removed without retaining sessions after their owner dies.
pub fn lookup(window_id: &str) -> Option<Arc<HostLayout>> {
    let mut entries = registry().lock().unwrap_or_else(|error| error.into_inner());
    let handle = entries.get(window_id).and_then(Weak::upgrade);
    if handle.is_none() {
        entries.remove(window_id);
    }
    handle
}

impl HostLayout {
    /// Publish the storage projection after each committed in-memory edit.
    /// Call before scheduling that edit's persistence snapshot.
    pub fn publish(&self, project: ProjectLayout) {
        self.current.store(Arc::new(project));
    }

    /// Publish and enqueue an edited storage projection without waiting for IO.
    /// The host snapshot remains current even if the save worker has failed.
    pub fn schedule(&self, project: ProjectLayout) -> Result<(), String> {
        self.publish(project.clone());
        self.persistence
            .as_ref()
            .ok_or_else(|| "unavailable: host layout is closed".to_owned())?
            .schedule(project)
    }

    /// A consistent core snapshot for the host's RuntimeWindow reload payload.
    pub fn snapshot(&self) -> ProjectLayout {
        self.current.load_full().as_ref().clone()
    }

    /// Wait for already-scheduled saves. The host calls this off the AppKit and
    /// NSH engine threads, after it stops accepting edits and before teardown.
    /// Dropping the handle is deliberately not a synchronous persistence barrier.
    pub fn flush(&self) -> Result<(), String> {
        self.persistence
            .as_ref()
            .ok_or_else(|| "unavailable: host layout is closed".to_owned())?
            .flush()
    }
}

impl Drop for HostLayout {
    fn drop(&mut self) {
        let Some(persistence) = self.persistence.take() else {
            return;
        };
        let deferred = DeferredPersistence(Some(persistence));
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            // If shutdown rejects/cancels this task before it starts, dropping
            // its capture still defers the last Arc through the guarded fallback.
            runtime.spawn_blocking(move || deferred.finish());
        } else {
            drop(deferred);
        }
    }
}

/// Ensures even cancellation of the blocking task cannot run the persistence
/// destructor (which flushes) on the thread releasing the host handle.
struct DeferredPersistence(Option<Arc<LayoutPersistence>>);

impl DeferredPersistence {
    fn finish(mut self) {
        drop(self.0.take());
    }
}

impl Drop for DeferredPersistence {
    fn drop(&mut self) {
        let Some(persistence) = self.0.take() else {
            return;
        };
        let slot = Arc::new(Mutex::new(Some(persistence)));
        let worker_slot = Arc::clone(&slot);
        let spawned = std::thread::Builder::new()
            .name("riwork-layout-dispose".into())
            .spawn(move || {
                let persistence = worker_slot
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take();
                drop(persistence);
            });
        if let Err(error) = spawned {
            // Resource exhaustion must not unexpectedly perform filesystem IO
            // on the engine. Retain the Arc; the host's explicit flush remains
            // the durability barrier before process exit.
            eprintln!("layout.dispose: could not start background disposal: {error}");
            std::mem::forget(slot);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::SaveMode;

    #[test]
    fn host_lookup_tracks_the_latest_snapshot_without_retaining_the_session() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let _entered = runtime.enter();
        let home = tempfile::Builder::new()
            .prefix("riwork-host-layout-")
            .tempdir_in("/tmp")
            .unwrap();
        let loaded =
            LayoutPersistence::open(home.path(), "host-project", None, SaveMode::LegacyChatState)
                .unwrap();
        let persistence = Arc::new(loaded.persistence);
        let mut project = loaded.project;
        let handle = registration(
            "host-layout-test-window",
            Arc::clone(&persistence),
            project.clone(),
        )
        .unwrap();
        let peer = lookup("host-layout-test-window").unwrap();
        assert!(Arc::ptr_eq(&handle, &peer));
        assert_eq!(peer.snapshot(), project);
        assert!(
            registration(
                "host-layout-test-window",
                Arc::clone(&persistence),
                project.clone()
            )
            .is_err()
        );
        project.selected_task_id = Some("latest-in-memory-task".into());
        handle.publish(project.clone());
        assert_eq!(peer.snapshot(), project);
        assert!(
            !home.path().join("layouts.json").exists(),
            "publishing must not write layouts.json"
        );
        handle.flush().unwrap();
        drop(peer);
        drop(handle);
        assert!(lookup("host-layout-test-window").is_none());
        let reopened = registration("host-layout-test-window", persistence, project).unwrap();
        assert!(lookup("host-layout-test-window").is_some());
        drop(reopened);
    }
}
