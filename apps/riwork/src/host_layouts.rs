//! Host-side handoff for a window's current layout and persistence barrier.
//!
//! The registry is consulted only while opening a session or from the host.
//! Publishing an edit never takes the registry mutex or waits on a file lock.
use crate::persistence::{LayoutBackend, LoadedLayout};
use arc_swap::ArcSwap;
use riwork_core::layouts::{PaneId, ProjectLayout, WindowSize};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering},
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

fn pack_size(size: Option<WindowSize>) -> u64 {
    size.map_or(0, |size| {
        (u64::from(size.width.to_bits()) << 32) | u64::from(size.height.to_bits())
    })
}

fn unpack_size(packed: u64) -> Option<WindowSize> {
    (packed != 0).then(|| WindowSize {
        width: f32::from_bits((packed >> 32) as u32),
        height: f32::from_bits(packed as u32),
    })
}

/// A thread-safe host handle, independent of NSH's engine-owned class instance.
/// The host may retain it through AppView teardown to flush before terminating.
pub struct HostLayout {
    current: Option<ArcSwap<CurrentLayout>>,
    // One atomic publishes a coherent focus-mode/centering pair for reload.
    focus: AtomicU8,
    // Host geometry survives later model edits whose projection predates a
    // resize. Packed f32 bits (zero is none) keep engine publication lock-free.
    window_size: AtomicU64,
    closing: AtomicBool,
    switching: AtomicBool,
    editing: AtomicUsize,
}

struct CurrentLayout {
    project: ProjectLayout,
    // ArcSwap readers can release the final generation on any thread. Its
    // writer therefore owns the same disposal guard as a cancelled switch.
    persistence: OwnedPersistence,
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
        focus: AtomicU8::new(0),
        window_size: AtomicU64::new(pack_size(snapshot.window_size)),
        current: Some(ArcSwap::from_pointee(CurrentLayout {
            project: snapshot,
            persistence: OwnedPersistence(Some(persistence)),
        })),
        closing: AtomicBool::new(false),
        switching: AtomicBool::new(false),
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

    /// Store the presentation flags from inside an already admitted edit.
    pub fn publish_focus(&self, focused: bool, centered: bool) {
        self.focus.store(
            u8::from(focused) | (u8::from(centered) << 1),
            Ordering::Release,
        );
    }

    pub fn focus_state(&self) -> (bool, bool) {
        let flags = self.focus.load(Ordering::Acquire);
        (flags & 1 != 0, flags & 2 != 0)
    }

    /// Publish the storage projection after each committed in-memory edit.
    /// Call before scheduling that edit's persistence snapshot.
    pub fn publish(&self, mut project: ProjectLayout) {
        project.window_size = self.window_size();
        let current = self.current.as_ref().expect("live host layout");
        let persistence = current.load().persistence.clone();
        current.store(Arc::new(CurrentLayout {
            project,
            persistence: OwnedPersistence(Some(persistence)),
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
        self.edit_with_remap(|| {
            operation()
                .map(|(value, project)| (value, project.map(|project| (project, BTreeMap::new()))))
        })
    }

    /// Atomically publish a layout edit together with opaque-tab relocations.
    pub fn edit_with_remap<T>(
        &self,
        operation: impl FnOnce()
            -> Result<(T, Option<(ProjectLayout, BTreeMap<PaneId, PaneId>)>), String>,
    ) -> Result<T, String> {
        let _admission = self.admit(false)?;
        let (result, project) = operation()?;
        if let Some((project, remap)) = project {
            self.publish(project);
            self.current
                .as_ref()
                .expect("live host layout")
                .load()
                .persistence
                .schedule_with_remap(self.snapshot(), remap)?;
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
            // Only the host service thread writes this value; edits admitted
            // here are serialized against close by the edit admission.
            if self.window_size() == Some(size) {
                return Ok(((), None));
            }
            self.window_size
                .store(pack_size(Some(size)), Ordering::Release);
            let mut project = self.snapshot();
            project.window_size = Some(size);
            Ok(((), Some(project)))
        })
    }

    /// The latest ordinary size captured from host geometry or the saved layout.
    pub fn window_size(&self) -> Option<WindowSize> {
        unpack_size(self.window_size.load(Ordering::Acquire))
    }

    /// Stop accepting edits before the host queues its persistence barrier.
    /// An edit already publishing/enqueueing completes before this returns.
    pub fn begin_close(&self) {
        self.closing.store(true, Ordering::SeqCst);
        // The host service thread waits; the engine never takes a host mutex.
        while self.editing.load(Ordering::SeqCst) != 0 {
            std::thread::yield_now();
        }
    }

    pub fn cancel_close(&self) {
        self.closing.store(false, Ordering::SeqCst);
    }

    fn admit(&self, switching_owner: bool) -> Result<EditAdmission<'_>, String> {
        let check = || -> Result<(), String> {
            if self.closing.load(Ordering::SeqCst) {
                return Err("unavailable: window is closing".into());
            }
            if !switching_owner && self.switching.load(Ordering::SeqCst) {
                return Err("conflict: project switch is in progress".into());
            }
            Ok(())
        };
        check()?;
        self.editing.fetch_add(1, Ordering::SeqCst);
        let admission = EditAdmission(&self.editing);
        check()?;
        Ok(admission)
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
        if self.closing.load(Ordering::SeqCst) {
            return Err("unavailable: window is closing".into());
        }
        if self
            .switching
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err("conflict: project switch is in progress".into());
        }
        if self.closing.load(Ordering::SeqCst) {
            self.switching.store(false, Ordering::SeqCst);
            return Err("unavailable: window is closing".into());
        }
        // Never wait for an overlapping host edit on the engine thread. Its
        // admission recheck also rejects an edit racing this reservation.
        if self.editing.load(Ordering::SeqCst) != 0 {
            self.switching.store(false, Ordering::SeqCst);
            return Err("conflict: layout edit is in progress".into());
        }
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
        persistence: Arc<dyn LayoutBackend>,
        mut project: ProjectLayout,
        save: bool,
        operation: impl FnOnce() -> T,
    ) -> Result<T, String> {
        let _admission = match self.host.admit(true) {
            Ok(admission) => admission,
            Err(error) => {
                defer_persistence(persistence);
                return Err(error);
            }
        };
        // The window keeps its captured ordinary size across a project switch.
        if let Some(size) = self.host.window_size() {
            project.window_size = Some(size);
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
                persistence: OwnedPersistence(Some(persistence)),
            }));
        // Each generation owns its disposal guard, including any generation
        // still held by an ArcSwap reader after this replacement.
        drop(previous);
        self.host.switching.store(false, Ordering::SeqCst);
        self.finished = true;
        Ok(result)
    }
}

impl Drop for ProjectSwitch {
    fn drop(&mut self) {
        if !self.finished {
            self.host.switching.store(false, Ordering::SeqCst);
        }
    }
}

impl Drop for HostLayout {
    fn drop(&mut self) {
        let Some(current) = self.current.take() else {
            return;
        };
        drop(current.into_inner());
    }
}

pub(crate) fn defer_persistence(persistence: Arc<dyn LayoutBackend>) {
    let deferred = DeferredPersistence(Some(persistence));
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        // Cancellation drops the capture through the guarded fallback.
        runtime.spawn_blocking(move || deferred.finish());
    } else {
        drop(deferred);
    }
}

/// Release an admitted edit without taking a host lock on the engine thread.
struct EditAdmission<'a>(&'a AtomicUsize);
impl Drop for EditAdmission<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Every generation transfers its owned Arc, rather than a temporary clone, to
/// the blocking pool. Ordinary edits reuse the pool instead of spawning threads.
struct OwnedPersistence(Option<Arc<dyn LayoutBackend>>);

impl std::ops::Deref for OwnedPersistence {
    type Target = Arc<dyn LayoutBackend>;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref().expect("owned layout writer")
    }
}

impl Drop for OwnedPersistence {
    fn drop(&mut self) {
        if let Some(persistence) = self.0.take() {
            defer_persistence(persistence);
        }
    }
}

/// Cancellation of the blocking task transfers final ownership to a fallback
/// thread so a persistence destructor can never flush on the engine.
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

    struct DisposalProbe(std::sync::mpsc::Sender<std::thread::ThreadId>);

    impl LayoutBackend for DisposalProbe {
        fn schedule(&self, _: ProjectLayout) -> Result<(), String> {
            Ok(())
        }
        fn flush(&self) -> Result<(), String> {
            Ok(())
        }
    }

    impl Drop for DisposalProbe {
        fn drop(&mut self) {
            self.0.send(std::thread::current().id()).unwrap();
        }
    }

    #[test]
    fn switched_and_closed_generations_release_their_final_writer_off_the_engine() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _entered = runtime.enter();
        let engine_thread = std::thread::current().id();
        let (old_tx, old_rx) = std::sync::mpsc::channel();
        let (new_tx, new_rx) = std::sync::mpsc::channel();
        let host = registration(
            "deferred-generation-test",
            Arc::new(DisposalProbe(old_tx)),
            default_project_layout(),
        )
        .unwrap();
        // A host reader may retain the outgoing generation beyond the commit.
        let reader = host.current.as_ref().unwrap().load_full();
        let mut switch = host.begin_project_switch().unwrap();
        switch
            .commit(
                Arc::new(DisposalProbe(new_tx)),
                default_project_layout(),
                false,
                || (),
            )
            .unwrap();
        drop(switch);
        assert!(old_rx.try_recv().is_err());
        drop(reader);
        let timeout = std::time::Duration::from_secs(5);
        assert_ne!(old_rx.recv_timeout(timeout).unwrap(), engine_thread);
        drop(host);
        assert_ne!(new_rx.recv_timeout(timeout).unwrap(), engine_thread);
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
