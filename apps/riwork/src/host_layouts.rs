//! Host-side handoff for a window's current layout and persistence barrier.
//!
//! The registry is consulted only while opening a session or from the host.
//! Publishing an edit never takes the registry mutex or waits on a file lock.
use crate::persistence::{LayoutPersistence, LoadedLayout};
use arc_swap::ArcSwap;
use riwork_core::layouts::ProjectLayout;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, OnceLock, Weak},
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
    current: Option<ArcSwap<CurrentLayout>>,
    // Serializes the short publish/enqueue operation against a host close barrier.
    // No filesystem work takes this mutex on the engine thread.
    admission: Mutex<Admission>,
}

struct CurrentLayout {
    project: ProjectLayout,
    persistence: Arc<LayoutPersistence>,
}

#[derive(Default)]
struct Admission {
    closing: bool,
    switching: bool,
}

/// Reserves the window while a replacement is prepared off the engine thread.
/// Dropping a cancelled switch makes the previous session editable again.
pub(crate) struct ProjectSwitch {
    host: Arc<HostLayout>,
    previous: ProjectLayout,
    finished: bool,
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
        current: Some(ArcSwap::from_pointee(CurrentLayout {
            project: snapshot,
            persistence,
        })),
        admission: Mutex::new(Admission::default()),
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
        let current = self.current.as_ref().expect("live host layout");
        let persistence = current.load().persistence.clone();
        current.store(Arc::new(CurrentLayout {
            project,
            persistence,
        }));
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
        let admission = self
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if admission.closing {
            return Err("unavailable: window is closing".into());
        }
        if admission.switching {
            return Err("conflict: project switch is in progress".into());
        }
        let (result, project) = operation()?;
        if let Some(project) = project {
            self.publish(project.clone());
            self.current
                .as_ref()
                .expect("live host layout")
                .load()
                .persistence
                .schedule(project)?;
        }
        Ok(result)
    }

    /// Stop accepting edits before the host queues its persistence barrier.
    /// An edit already publishing/enqueueing completes before this returns.
    pub fn begin_close(&self) {
        self.admission
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .closing = true;
    }

    /// Restore editing when a close/quit flush failed and the window stays open.
    pub fn cancel_close(&self) {
        self.admission
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .closing = false;
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
        self.current
            .as_ref()
            .expect("live host layout")
            .load()
            .project
            .clone()
    }

    pub fn project_id(&self) -> String {
        self.current
            .as_ref()
            .expect("live host layout")
            .load()
            .persistence
            .project_id()
            .to_owned()
    }

    /// Capture identity and layout from one atomic generation for host restore
    /// snapshots; a concurrent project switch cannot mix the two projects.
    pub fn project_snapshot(&self) -> (String, ProjectLayout) {
        let current = self.current.as_ref().expect("live host layout").load();
        (
            current.persistence.project_id().to_owned(),
            current.project.clone(),
        )
    }

    pub(crate) fn begin_project_switch(self: &Arc<Self>) -> Result<ProjectSwitch, String> {
        let mut admission = self
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if admission.closing {
            return Err("unavailable: window is closing".into());
        }
        if admission.switching {
            return Err("conflict: project switch is in progress".into());
        }
        admission.switching = true;
        Ok(ProjectSwitch {
            host: Arc::clone(self),
            previous: self.snapshot(),
            finished: false,
        })
    }

    /// Wait for already-scheduled saves. The host calls this off the AppKit and
    /// NSH engine threads, after it stops accepting edits and before teardown.
    /// Dropping the handle is deliberately not a synchronous persistence barrier.
    pub fn flush(&self) -> Result<(), String> {
        self.current
            .as_ref()
            .expect("live host layout")
            .load_full()
            .persistence
            .flush()
    }
}

impl ProjectSwitch {
    /// Blocking preparation retains the old writer until an admitted commit.
    pub(crate) fn prepare(&self, project_id: &str) -> Result<LoadedLayout, String> {
        self.host
            .current
            .as_ref()
            .expect("live host layout")
            .load_full()
            .persistence
            .prepare_project_switch(project_id, &self.previous)
    }

    /// Commit model, host snapshot and writer under the same short close gate.
    /// The operation only replaces already-prepared in-memory state.
    pub(crate) fn commit<T>(
        &mut self,
        persistence: Arc<LayoutPersistence>,
        project: ProjectLayout,
        save: bool,
        operation: impl FnOnce() -> T,
    ) -> Result<T, String> {
        let mut admission = self
            .host
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if admission.closing {
            defer_persistence(persistence);
            return Err("unavailable: window is closing".into());
        }
        if save {
            if let Err(error) = persistence.schedule(project.clone()) {
                defer_persistence(persistence);
                return Err(error);
            }
        }
        let result = operation();
        let previous = self
            .host
            .current
            .as_ref()
            .expect("live host layout")
            .swap(Arc::new(CurrentLayout {
                project,
                persistence,
            }));
        // A persistence destructor flushes. Never release its final Arc here.
        defer_persistence(previous.persistence.clone());
        drop(previous);
        admission.switching = false;
        self.finished = true;
        Ok(result)
    }
}

impl Drop for ProjectSwitch {
    fn drop(&mut self) {
        if !self.finished {
            self.host
                .admission
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .switching = false;
        }
    }
}

impl Drop for HostLayout {
    fn drop(&mut self) {
        let Some(current) = self.current.take() else {
            return;
        };
        let current = current.into_inner();
        defer_persistence(current.persistence.clone());
    }
}

pub(crate) fn defer_persistence(persistence: Arc<LayoutPersistence>) {
    let deferred = DeferredPersistence(Some(persistence));
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        // Cancellation drops the capture through the guarded fallback.
        runtime.spawn_blocking(move || deferred.finish());
    } else {
        drop(deferred);
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
    fn close_during_switch_rejects_commit_and_flushes_the_old_project_only() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _entered = runtime.enter();
        let home = tempfile::Builder::new()
            .prefix("riwork-host-switch-close-")
            .tempdir_in("/tmp")
            .unwrap();
        let loaded =
            LayoutPersistence::open(home.path(), "outgoing", None, SaveMode::LegacyChatState)
                .unwrap();
        let previous = loaded.project;
        let host = registration(
            &home.path().display().to_string(),
            Arc::new(loaded.persistence),
            previous.clone(),
        )
        .unwrap();
        let mut switch = host.begin_project_switch().unwrap();
        assert_eq!(
            host.schedule(previous.clone()).unwrap_err(),
            "conflict: project switch is in progress"
        );
        let prepared = switch.prepare("destination").unwrap();
        let mut changed = false;
        host.begin_close();
        assert_eq!(
            switch
                .commit(
                    Arc::new(prepared.persistence),
                    prepared.project,
                    true,
                    || changed = true
                )
                .unwrap_err(),
            "unavailable: window is closing"
        );
        assert!(
            !changed,
            "the engine model must not change after close wins admission"
        );
        host.flush_blocking().unwrap();
        let store = riwork_core::layouts::LayoutStore::open(home.path()).unwrap();
        assert_eq!(store.load("outgoing").unwrap(), Some(previous.clone()));
        assert!(store.load("destination").unwrap().is_none());
        assert_eq!(host.project_id(), "outgoing");
        assert_eq!(host.snapshot(), previous);
        drop(switch);
        host.cancel_close();
        host.schedule(previous).unwrap();
        host.flush_blocking().unwrap();
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
