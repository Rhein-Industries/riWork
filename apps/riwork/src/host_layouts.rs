//! Host-side handoff for a window's current layout and persistence barrier.
//!
//! The registry is consulted only while opening a session or from the host.
//! Publishing an edit never takes the registry mutex or waits on a file lock.
use crate::persistence::LayoutBackend;
use arc_swap::ArcSwap;
use riwork_core::layouts::ProjectLayout;
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, AtomicUsize, Ordering},
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
    persistence: Option<Arc<dyn LayoutBackend>>,
    closing: AtomicBool,
    editing: AtomicUsize,
}

/// Register after opening persistence on a blocking thread. A second live
/// session for the same window is a conflict rather than an implicit takeover.
pub fn registration<B: LayoutBackend + 'static>(
    window_id: &str,
    persistence: Arc<B>,
    snapshot: ProjectLayout,
) -> Result<Arc<HostLayout>, String> {
    registration_backend(window_id, persistence, snapshot)
}

pub fn registration_backend(
    window_id: &str,
    persistence: Arc<dyn LayoutBackend>,
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
        current: ArcSwap::from_pointee(snapshot),
        persistence: Some(persistence),
        closing: AtomicBool::new(false),
        editing: AtomicUsize::new(0),
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
    /// Publish the storage projection after each committed in-memory edit.
    /// Call before scheduling that edit's persistence snapshot.
    pub fn publish(&self, project: ProjectLayout) {
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
        if self.closing.load(Ordering::SeqCst) {
            return Err("unavailable: window is closing".into());
        }
        self.editing.fetch_add(1, Ordering::SeqCst);
        let _admission = EditAdmission(&self.editing);
        // A close may have started between the first check and admission. The
        // host waits for admitted work; late calls never change the model.
        if self.closing.load(Ordering::SeqCst) {
            return Err("unavailable: window is closing".into());
        }
        let (result, project) = operation()?;
        if let Some(project) = project {
            self.publish(project.clone());
            self.persistence
                .as_ref()
                .ok_or_else(|| "unavailable: host layout is closed".to_owned())?
                .schedule(project)?;
        }
        Ok(result)
    }

    /// Stop accepting edits before the host queues its persistence barrier.
    /// An edit already publishing/enqueueing completes before this returns.
    pub fn begin_close(&self) {
        self.closing.store(true, Ordering::SeqCst);
        // Only the host's service thread waits. The JS edit path uses atomics
        // and never acquires a lock held by the host or persistence worker.
        while self.editing.load(Ordering::SeqCst) != 0 {
            std::thread::yield_now();
        }
    }

    /// Restore editing when a close/quit flush failed and the window stays open.
    pub fn cancel_close(&self) {
        self.closing.store(false, Ordering::SeqCst);
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
struct EditAdmission<'a>(&'a AtomicUsize);

impl Drop for EditAdmission<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

struct DeferredPersistence(Option<Arc<dyn LayoutBackend>>);

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
    use crate::persistence::{LayoutPersistence, SaveMode, default_project_layout};

    #[derive(Default)]
    struct FakeLayoutBackend {
        scheduled: AtomicUsize,
        flushed: AtomicUsize,
    }

    impl LayoutBackend for FakeLayoutBackend {
        fn schedule(&self, _: ProjectLayout) -> Result<(), String> {
            self.scheduled.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn flush(&self) -> Result<(), String> {
            self.flushed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn a_host_close_waits_for_an_admitted_edit_and_rejects_late_edits_without_a_lock() {
        let backend = Arc::new(FakeLayoutBackend::default());
        let handle = registration(
            "atomic-close-test",
            Arc::clone(&backend),
            default_project_layout(),
        )
        .unwrap();
        let editor = Arc::clone(&handle);
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let edit = std::thread::spawn(move || {
            editor.edit(|| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(((), Some(default_project_layout())))
            })
        });
        entered_rx.recv().unwrap();
        let closer = Arc::clone(&handle);
        let close = std::thread::spawn(move || {
            closer.begin_close();
            closer.flush().unwrap();
        });
        while !handle.closing.load(Ordering::SeqCst) {
            std::thread::yield_now();
        }
        let rejected = handle.edit::<()>(|| panic!("a late edit must never enter the model"));
        assert!(rejected.unwrap_err().starts_with("unavailable:"));
        assert_eq!(backend.flushed.load(Ordering::SeqCst), 0);
        release_tx.send(()).unwrap();
        edit.join().unwrap().unwrap();
        close.join().unwrap();
        assert_eq!(backend.scheduled.load(Ordering::SeqCst), 1);
        assert_eq!(backend.flushed.load(Ordering::SeqCst), 1);
        handle.cancel_close();
        handle.schedule(default_project_layout()).unwrap();
        assert_eq!(backend.scheduled.load(Ordering::SeqCst), 2);
    }

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
