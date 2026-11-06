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
    opening: HashMap<String, usize>,
}

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Registry::default()))
}

/// Track blocking open jobs before they leave the async caller. A dev remount
/// retains the close marker until stopped engines have no late opener left.
pub struct Opening {
    window_id: String,
}
impl Drop for Opening {
    fn drop(&mut self) {
        let mut entries = registry().lock().unwrap_or_else(|error| error.into_inner());
        if let Some(count) = entries.opening.get_mut(&self.window_id) {
            *count -= 1;
            if *count == 0 {
                entries.opening.remove(&self.window_id);
            }
        }
    }
}
pub fn begin_window_open(window_id: &str) -> Result<Opening, String> {
    let mut entries = registry().lock().unwrap_or_else(|error| error.into_inner());
    if entries.closing.contains(window_id) {
        return Err("unavailable: window is closing".into());
    }
    *entries.opening.entry(window_id.to_owned()).or_default() += 1;
    Ok(Opening {
        window_id: window_id.to_owned(),
    })
}
pub fn pending_window_opens(window_id: &str) -> usize {
    registry()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .opening
        .get(window_id)
        .copied()
        .unwrap_or(0)
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
    // Readers of every generation share the writer's disposal guard. The last
    // owner can release it on any thread without running its destructor there.
    persistence: Arc<OwnedPersistence>,
}

/// A coherent host read that shares the layout instead of copying its tabs.
/// Retaining it also retains the generation's guarded persistence owner.
pub struct ProjectSnapshot(Arc<CurrentLayout>);

impl ProjectSnapshot {
    /// Whether both captures retain the exact same published generation.
    pub fn shares_generation(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    pub fn project_id(&self) -> &str {
        self.0.persistence.project_id()
    }

    pub fn project(&self) -> &ProjectLayout {
        &self.0.project
    }
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
            persistence: Arc::new(OwnedPersistence(Some(persistence))),
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
        let persistence = Arc::clone(&current.load().persistence);
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

    /// Capture identity and layout together without allocating either payload.
    pub fn shared_project_snapshot(&self) -> ProjectSnapshot {
        ProjectSnapshot(self.current.as_ref().expect("live host layout").load_full())
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
                persistence: Arc::new(OwnedPersistence(Some(persistence))),
            }));
        // Older readers retain the outgoing writer's shared disposal guard
        // until their final generation is released.
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

#[cfg(test)]
thread_local! {
    static DISPOSAL_SUBMISSIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(crate) fn defer_persistence(persistence: Arc<dyn LayoutBackend>) {
    #[cfg(test)]
    DISPOSAL_SUBMISSIONS.with(|count| count.set(count.get() + 1));
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

/// Generations share this owner. Its final release transfers the backend Arc
/// to the blocking pool; intermediate generations need no disposal work.
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

    #[test]
    fn stopped_window_tracks_a_blocking_open_until_its_late_registration_is_rejected() {
        let id = "pending-open-remount-contract";
        let opening = begin_window_open(id).unwrap();
        let (release, resume) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _opening = opening;
            resume.recv().unwrap();
            assert!(
                registration(
                    id,
                    Arc::new(FakeLayoutBackend::default()),
                    default_project_layout()
                )
                .is_err()
            );
        });
        begin_window_close(id);
        assert_eq!(pending_window_opens(id), 1);
        assert!(begin_window_open(id).is_err());
        release.send(()).unwrap();
        worker.join().unwrap();
        assert_eq!(pending_window_opens(id), 0);
        assert!(lookup(id).is_none());
        cancel_window_close(id);
        let _replacement = registration(
            id,
            Arc::new(FakeLayoutBackend::default()),
            default_project_layout(),
        )
        .unwrap();
    }
    #[derive(Default)]
    struct FakeLayoutBackend {
        scheduled: AtomicUsize,
        flushed: AtomicUsize,
        project_id: String,
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
        fn project_id(&self) -> &str {
            &self.project_id
        }
    }

    #[test]
    fn borrowed_snapshot_keeps_identity_and_layout_together_during_a_switch() {
        let mut before = default_project_layout();
        before.selected_task_id = Some("before".into());
        let host = registration(
            "borrowed-snapshot-switch",
            Arc::new(FakeLayoutBackend {
                project_id: "before".into(),
                ..Default::default()
            }),
            before.clone(),
        )
        .unwrap();
        let snapshot = host.shared_project_snapshot();
        {
            let writer = Arc::clone(&host);
            std::thread::spawn(move || {
                let mut after = default_project_layout();
                after.selected_task_id = Some("after".into());
                writer
                    .begin_project_switch()
                    .unwrap()
                    .commit(
                        Arc::new(FakeLayoutBackend {
                            project_id: "after".into(),
                            ..Default::default()
                        }),
                        after,
                        false,
                        || (),
                    )
                    .unwrap();
            })
            .join()
            .unwrap();
            assert_eq!(snapshot.project_id(), "before");
            assert_eq!(snapshot.project(), &before);
        }
        let latest = host.shared_project_snapshot();
        assert_eq!(latest.project_id(), "after");
        assert_eq!(
            latest.project().selected_task_id.as_deref(),
            Some(latest.project_id())
        );
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
    fn ordinary_generations_share_one_writer_owner_and_submit_only_its_final_disposal() {
        for with_runtime in [false, true] {
            let runtime = with_runtime.then(|| tokio::runtime::Runtime::new().unwrap());
            let _entered = runtime.as_ref().map(|runtime| runtime.enter());
            let caller = std::thread::current().id();
            let (tx, rx) = std::sync::mpsc::channel();
            DISPOSAL_SUBMISSIONS.with(|count| count.set(0));
            let host = registration(
                &format!("shared-writer-generations-{with_runtime}"),
                Arc::new(DisposalProbe(tx)),
                default_project_layout(),
            )
            .unwrap();
            let mut readers = vec![host.current.as_ref().unwrap().load_full()];
            for index in 0..128 {
                let mut project = default_project_layout();
                project.selected_task_id = Some(format!("generation-{index}"));
                host.publish(project);
                let next = host.current.as_ref().unwrap().load_full();
                assert!(Arc::ptr_eq(&readers[0].persistence, &next.persistence));
                readers.push(next);
            }
            assert_eq!(DISPOSAL_SUBMISSIONS.with(|count| count.get()), 0);
            drop(readers);
            assert_eq!(DISPOSAL_SUBMISSIONS.with(|count| count.get()), 0);
            assert!(rx.try_recv().is_err(), "live host still owns the writer");
            drop(host);
            assert_eq!(DISPOSAL_SUBMISSIONS.with(|count| count.get()), 1);
            assert_ne!(
                rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap(),
                caller
            );
            assert!(rx.try_recv().is_err());
        }
    }

    #[test]
    fn switched_writer_owners_wait_for_their_last_delayed_reader() {
        for with_runtime in [false, true] {
            let runtime = with_runtime.then(|| tokio::runtime::Runtime::new().unwrap());
            let _entered = runtime.as_ref().map(|runtime| runtime.enter());
            let caller = std::thread::current().id();
            let (old_tx, old_rx) = std::sync::mpsc::channel();
            let (new_tx, new_rx) = std::sync::mpsc::channel();
            DISPOSAL_SUBMISSIONS.with(|count| count.set(0));
            let host = registration(
                &format!("shared-writer-switch-{with_runtime}"),
                Arc::new(DisposalProbe(old_tx)),
                default_project_layout(),
            )
            .unwrap();
            let old_first = host.shared_project_snapshot();
            host.publish(default_project_layout());
            let old_last = host.shared_project_snapshot();
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
            let newest = host.shared_project_snapshot();
            assert!(!Arc::ptr_eq(&old_last.0.persistence, &newest.0.persistence));
            drop(host);
            drop(old_first);
            assert_eq!(DISPOSAL_SUBMISSIONS.with(|count| count.get()), 0);
            assert!(old_rx.try_recv().is_err());
            assert!(new_rx.try_recv().is_err());
            drop(old_last);
            assert_eq!(DISPOSAL_SUBMISSIONS.with(|count| count.get()), 1);
            assert_ne!(
                old_rx
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap(),
                caller
            );
            assert!(new_rx.try_recv().is_err());
            drop(newest);
            assert_eq!(DISPOSAL_SUBMISSIONS.with(|count| count.get()), 2);
            assert_ne!(
                new_rx
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap(),
                caller
            );
        }
    }

    #[test]
    fn concurrent_final_readers_submit_one_off_thread_writer_disposal() {
        let (tx, rx) = std::sync::mpsc::channel();
        let host = registration(
            "shared-writer-concurrent",
            Arc::new(DisposalProbe(tx)),
            default_project_layout(),
        )
        .unwrap();
        let first = host.shared_project_snapshot();
        host.publish(default_project_layout());
        let second = host.shared_project_snapshot();
        drop(host);
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let workers = [first, second]
            .into_iter()
            .map(|reader| {
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    DISPOSAL_SUBMISSIONS.with(|count| count.set(0));
                    barrier.wait();
                    drop(reader);
                    (
                        std::thread::current().id(),
                        DISPOSAL_SUBMISSIONS.with(|count| count.get()),
                    )
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        let workers = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(workers.iter().map(|(_, count)| count).sum::<usize>(), 1);
        let disposer = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert!(workers.iter().all(|(caller, _)| *caller != disposer));
        assert_ne!(disposer, std::thread::current().id());
    }

    #[test]
    fn final_writer_disposal_survives_a_shutdown_runtime() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let handle = runtime.handle().clone();
        runtime.shutdown_background();
        let _entered = handle.enter();
        let (tx, rx) = std::sync::mpsc::channel();
        DISPOSAL_SUBMISSIONS.with(|count| count.set(0));
        let host = registration(
            "shared-writer-shutdown",
            Arc::new(DisposalProbe(tx)),
            default_project_layout(),
        )
        .unwrap();
        drop(host);
        assert_eq!(DISPOSAL_SUBMISSIONS.with(|count| count.get()), 1);
        assert_ne!(
            rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap(),
            std::thread::current().id()
        );
    }

    #[test]
    #[ignore = "manual paired retained-generation disposal benchmark"]
    fn shared_writer_disposal_benchmark() {
        use riwork_core::layouts::SavedTab;
        use std::time::{Duration, Instant};

        // Exact pre-change ownership: every retained generation owned a new
        // guard around another Arc to the same writer. Field drop order also
        // matches CurrentLayout (payload first, persistence second).
        struct PreviousGeneration {
            _project: ProjectLayout,
            _persistence: OwnedPersistence,
        }

        for with_runtime in [false, true] {
            let runtime = with_runtime.then(|| tokio::runtime::Runtime::new().unwrap());
            let _entered = runtime.as_ref().map(|runtime| runtime.enter());
            for tabs in [20, 500] {
                let mut project = default_project_layout();
                project.panes.values_mut().next().unwrap().tabs = (0..tabs)
                    .map(|tab| SavedTab::Addin {
                        instance_id: format!("tab-{tab}"),
                        addin: "project/review".into(),
                        view: "files".into(),
                        title: format!("Review {tab}"),
                        args: serde_json::json!({"context": "x".repeat(1024)}),
                    })
                    .collect();
                for generations in [1, 128] {
                    let setup = |shared: bool| {
                        let (tx, rx) = std::sync::mpsc::channel();
                        let backend: Arc<dyn LayoutBackend> = Arc::new(DisposalProbe(tx));
                        let dispose: Box<dyn FnOnce()> = if shared {
                            let host = registration_backend(
                                "shared-writer-benchmark",
                                backend,
                                project.clone(),
                            )
                            .unwrap();
                            let mut readers = vec![host.shared_project_snapshot()];
                            for _ in 1..generations {
                                host.publish(project.clone());
                                readers.push(host.shared_project_snapshot());
                            }
                            Box::new(move || {
                                drop(readers);
                                drop(host);
                            })
                        } else {
                            let readers = (0..generations)
                                .map(|_| {
                                    Arc::new(PreviousGeneration {
                                        _project: project.clone(),
                                        _persistence: OwnedPersistence(Some(Arc::clone(&backend))),
                                    })
                                })
                                .collect::<Vec<_>>();
                            let current = Arc::clone(readers.last().unwrap());
                            drop(backend);
                            Box::new(move || {
                                drop(readers);
                                drop(current);
                            })
                        };
                        (dispose, rx)
                    };
                    let mut samples = [Vec::new(), Vec::new()];
                    for sample in 0..5 {
                        for index in if sample % 2 == 0 { [0, 1] } else { [1, 0] } {
                            let (dispose, final_drop) = setup(index == 1);
                            DISPOSAL_SUBMISSIONS.with(|count| count.set(0));
                            let started = Instant::now();
                            dispose();
                            let submissions = DISPOSAL_SUBMISSIONS.with(|count| count.get());
                            // The final raw backend Arc cannot be destroyed
                            // until every submitted disposal has released it.
                            let disposer =
                                final_drop.recv_timeout(Duration::from_secs(10)).unwrap();
                            samples[index].push(started.elapsed());
                            assert_ne!(disposer, std::thread::current().id());
                            assert_eq!(submissions, if index == 0 { generations } else { 1 });
                        }
                    }
                    for sample in &mut samples {
                        sample.sort();
                    }
                    eprintln!(
                        "retained generation disposal: tokio={with_runtime} tabs={tabs} generations={generations} median_of=5 baseline={:?} shared_owner={:?} submissions={generations}->1; includes payload destruction and final backend destruction, excludes fixture creation/publication",
                        samples[0][2], samples[1][2]
                    );
                }
            }
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
