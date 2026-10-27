//! Host-side handoff for a window's current layout and persistence barrier.
//!
//! The registry is consulted only while opening a session or from the host.
//! Publishing an edit never takes the registry mutex or waits on a file lock.
use crate::persistence::LayoutPersistence;
use arc_swap::ArcSwap;
use riwork_core::layouts::{ProjectLayout, WindowSize};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicU8, Ordering},
    },
};

#[derive(Default)]
struct Registry {
    sessions: HashMap<String, Weak<HostLayout>>,
    // Window IDs are unique for the process. Keep a close marker so an async
    // openSession that finishes after teardown cannot register an orphan writer.
    closing: HashSet<String>,
}

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Registry::default()))
}

/// A thread-safe host handle, independent of NSH's engine-owned class instance.
/// The host may retain it through AppView teardown to flush before terminating.
pub struct HostLayout {
    current: ArcSwap<ProjectLayout>,
    // One atomic publishes a coherent focus-mode/centering pair for reload.
    focus: AtomicU8,
    // Host geometry survives later model edits whose projection predates a resize.
    window_size: Mutex<Option<WindowSize>>,
    persistence: Option<Arc<LayoutPersistence>>,
    // Serializes the short publish/enqueue operation against a host close barrier.
    // No filesystem work takes this mutex on the engine thread.
    closing: Mutex<bool>,
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
    if entries.closing.contains(window_id) {
        return Err("unavailable: window is closing".into());
    }
    entries
        .sessions
        .retain(|_, entry| entry.strong_count() != 0);
    if entries
        .sessions
        .get(window_id)
        .and_then(Weak::upgrade)
        .is_some()
    {
        return Err("conflict: a layout session is already registered for this window".into());
    }
    let handle = Arc::new(HostLayout {
        focus: AtomicU8::new(0),
        window_size: Mutex::new(snapshot.window_size),
        current: ArcSwap::from_pointee(snapshot),
        persistence: Some(persistence),
        closing: Mutex::new(false),
    });
    entries
        .sessions
        .insert(window_id.to_owned(), Arc::downgrade(&handle));
    Ok(handle)
}

/// Look up before closing an engine/window or publishing a restart snapshot.
/// Expired entries are removed without retaining sessions after their owner dies.
pub fn lookup(window_id: &str) -> Option<Arc<HostLayout>> {
    let mut entries = registry().lock().unwrap_or_else(|error| error.into_inner());
    let handle = entries.sessions.get(window_id).and_then(Weak::upgrade);
    if handle.is_none() {
        entries.sessions.remove(window_id);
    }
    handle
}

/// Atomically stop new sessions and edits for a closing window, including a
/// layout open that is still running on a blocking worker.
pub fn begin_window_close(window_id: &str) -> Option<Arc<HostLayout>> {
    let mut entries = registry().lock().unwrap_or_else(|error| error.into_inner());
    entries.closing.insert(window_id.to_owned());
    let handle = entries.sessions.get(window_id).and_then(Weak::upgrade);
    if let Some(handle) = &handle {
        handle.begin_close();
    }
    handle
}

/// A failed persistence barrier leaves the same window available for retry.
pub fn cancel_window_close(window_id: &str) {
    let mut entries = registry().lock().unwrap_or_else(|error| error.into_inner());
    entries.closing.remove(window_id);
    if let Some(handle) = entries.sessions.get(window_id).and_then(Weak::upgrade) {
        handle.cancel_close();
    }
}

impl HostLayout {
    /// Runtime-only presentation flags, carried by restart rather than layouts.json.
    pub fn set_focus_state(&self, focused: bool, centered: bool) -> Result<(), String> {
        self.edit(|| {
            self.focus.store(
                u8::from(focused) | (u8::from(centered) << 1),
                Ordering::Release,
            );
            Ok(((), None))
        })
    }

    pub fn focus_state(&self) -> (bool, bool) {
        let flags = self.focus.load(Ordering::Acquire);
        (flags & 1 != 0, flags & 2 != 0)
    }

    /// Publish the storage projection after each committed in-memory edit.
    /// Call before scheduling that edit's persistence snapshot.
    pub fn publish(&self, mut project: ProjectLayout) {
        project.window_size = *self
            .window_size
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.current.store(Arc::new(project));
    }

    /// Publish and enqueue an edited storage projection without waiting for IO.
    /// The host snapshot remains current even if the save worker has failed.
    pub fn schedule(&self, project: ProjectLayout) -> Result<(), String> {
        self.edit(|| Ok(((), Some(project))))
    }

    /// Admit a whole in-memory edit and its publication as one short operation.
    /// Closing cannot reject an edit after its model was already changed.
    /// The operation must never perform IO or wait on another thread.
    pub fn edit<T>(
        &self,
        operation: impl FnOnce() -> Result<(T, Option<ProjectLayout>), String>,
    ) -> Result<T, String> {
        let closing = self
            .closing
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if *closing {
            return Err("unavailable: window is closing".into());
        }
        let (result, project) = operation()?;
        if let Some(project) = project {
            self.publish(project);
            self.persistence
                .as_ref()
                .ok_or_else(|| "unavailable: host layout is closed".to_owned())?
                .schedule(self.snapshot())?;
        }
        Ok(result)
    }

    /// Rust host geometry bridge. The service thread supplies the current
    /// settings value; no IO or filesystem lock runs on the AppKit thread.
    /// LayoutPersistence owns the trailing 250 ms timer. A return to the saved
    /// size replaces the pending snapshot; LayoutStore then skips its write.
    pub fn observe_window_frame(
        &self,
        width: f32,
        height: f32,
        ordinary: bool,
        remember: bool,
    ) -> Result<(), String> {
        if !ordinary || !remember {
            return Ok(());
        }
        let Some(size) = WindowSize::new(width, height) else {
            return Ok(());
        };
        self.edit(|| {
            let mut captured = self
                .window_size
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if *captured == Some(size) {
                return Ok(((), None));
            }
            *captured = Some(size);
            let mut project = self.snapshot();
            project.window_size = Some(size);
            Ok(((), Some(project)))
        })
    }

    /// Stop accepting edits before the host queues its persistence barrier.
    /// An edit already publishing/enqueueing completes before this returns.
    pub fn begin_close(&self) {
        *self
            .closing
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = true;
    }

    /// Restore editing when a close/quit flush failed and the window stays open.
    pub fn cancel_close(&self) {
        *self
            .closing
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = false;
    }

    /// Frozen P1c → P1b Rust handoff name (acceptance §4.1).
    pub fn project_layout(&self) -> ProjectLayout {
        self.snapshot()
    }

    /// Frozen P1c → P1b persistence barrier; call on the host service thread.
    pub fn flush_blocking(&self) -> Result<(), String> {
        self.flush()
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
