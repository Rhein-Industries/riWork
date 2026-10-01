//! One FSEvents stream over a set of directories, with an event for every file
//! that changes below them.
//!
//! This binds the few CoreServices calls it needs directly instead of through
//! the `notify` or `fsevent-sys` crates. `notify` creates its stream with a
//! latency of zero and `NoDefer`, so a build that writes ten thousand files is
//! ten thousand wake-ups, folds the drop and overflow flags into one rescan
//! hint, and brings five crates that are not built today. `fsevent-sys` does
//! not bind the dispatch queue calls, so it needs a thread parked in a run
//! loop. `core-foundation-sys` is already built for the window libraries, so
//! this adds no crate.
//!
//! The stream delivers on a private serial dispatch queue. Nothing here owns a
//! thread, and a stream with no events costs nothing in this process.

use std::{
    ffi::{CStr, c_char, c_void},
    marker::PhantomData,
    os::unix::ffi::OsStrExt,
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
    ptr,
    sync::Arc,
    time::Duration,
};

use core_foundation_sys::{
    array::{CFArrayCreate, CFArrayRef, kCFTypeArrayCallBacks},
    base::{CFIndex, CFRelease, CFTypeRef},
    string::{CFStringCreateWithBytes, CFStringRef, kCFStringEncodingUTF8},
};

/// `FSEventStreamEventFlags`, the ones the recency scan reads.
pub(super) mod flag {
    pub const MUST_SCAN_SUBDIRS: u32 = 0x1;
    pub const USER_DROPPED: u32 = 0x2;
    pub const KERNEL_DROPPED: u32 = 0x4;
    pub const EVENT_IDS_WRAPPED: u32 = 0x8;
    pub const HISTORY_DONE: u32 = 0x10;
    pub const ROOT_CHANGED: u32 = 0x20;
    pub const MOUNT: u32 = 0x40;
    pub const UNMOUNT: u32 = 0x80;
    pub const ITEM_CREATED: u32 = 0x100;
    pub const ITEM_REMOVED: u32 = 0x200;
    pub const ITEM_INODE_META_MOD: u32 = 0x400;
    pub const ITEM_RENAMED: u32 = 0x800;
    pub const ITEM_MODIFIED: u32 = 0x1000;
    pub const ITEM_IS_DIR: u32 = 0x2_0000;
    pub const ITEM_CLONED: u32 = 0x40_0000;
}

#[cfg(test)]
thread_local! {
    /// Makes the streams started on this thread fail, as when the system refuses.
    pub(super) static REFUSE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// `kFSEventStreamCreateFlagWatchRoot`: tell us when a watched directory itself
/// is renamed or deleted.
const WATCH_ROOT: u32 = 0x4;
/// `kFSEventStreamCreateFlagFileEvents`: one event per file, not per directory.
const FILE_EVENTS: u32 = 0x10;
/// `kFSEventStreamEventIdSinceNow`.
const SINCE_NOW: u64 = u64::MAX;

type FSEventStreamRef = *mut c_void;
type DispatchQueue = *mut c_void;
type Retain = extern "C" fn(*const c_void) -> *const c_void;
type Release = extern "C" fn(*const c_void);
type Callback = extern "C" fn(
    stream: *const c_void,
    info: *mut c_void,
    count: usize,
    paths: *mut c_void,
    flags: *const u32,
    ids: *const u64,
);

#[repr(C)]
struct Context {
    version: CFIndex,
    info: *mut c_void,
    retain: Option<Retain>,
    release: Option<Release>,
    copy_description: Option<extern "C" fn(*const c_void) -> CFStringRef>,
}

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn FSEventStreamCreate(
        allocator: *const c_void,
        callback: Callback,
        context: *mut Context,
        paths: CFArrayRef,
        since: u64,
        latency: f64,
        flags: u32,
    ) -> FSEventStreamRef;
    fn FSEventStreamSetDispatchQueue(stream: FSEventStreamRef, queue: DispatchQueue);
    fn FSEventStreamStart(stream: FSEventStreamRef) -> u8;
    fn FSEventStreamStop(stream: FSEventStreamRef);
    fn FSEventStreamInvalidate(stream: FSEventStreamRef);
    fn FSEventStreamRelease(stream: FSEventStreamRef);
}

// libdispatch is part of libSystem, which every binary links.
unsafe extern "C" {
    fn dispatch_queue_create(label: *const c_char, attr: *const c_void) -> DispatchQueue;
    fn dispatch_sync_f(
        queue: DispatchQueue,
        context: *mut c_void,
        work: extern "C" fn(*mut c_void),
    );
    fn dispatch_release(object: DispatchQueue);
}

/// One changed path and what happened to it.
#[derive(Clone, Copy)]
pub(super) struct Event<'a> {
    pub path: &'a [u8],
    pub flags: u32,
}

/// The events of one callback.
pub(super) struct Events<'a> {
    paths: *const *const c_char,
    flags: *const u32,
    count: usize,
    next: usize,
    batch: PhantomData<&'a ()>,
}

impl<'a> Iterator for Events<'a> {
    type Item = Event<'a>;

    fn next(&mut self) -> Option<Event<'a>> {
        if self.next >= self.count {
            return None;
        }
        // SAFETY: FSEvents hands the callback `count` NUL-terminated paths and
        // as many flags, valid until the callback returns, which `'a` ends at.
        let (path, flags) = unsafe {
            (
                CStr::from_ptr(*self.paths.add(self.next)),
                *self.flags.add(self.next),
            )
        };
        self.next += 1;
        Some(Event {
            path: path.to_bytes(),
            flags,
        })
    }
}

/// What a stream tells. `heard` runs on the stream's queue, never twice at once.
pub(super) trait Listener: Send + Sync + 'static {
    fn heard(&self, events: Events<'_>);

    /// `heard` panicked, so some of the events were not taken in.
    fn lost(&self);
}

/// A running stream. Dropping it stops the stream and waits for a callback that
/// is already under way, so nothing is delivered after the drop returns.
pub(super) struct Stream {
    stream: FSEventStreamRef,
    queue: DispatchQueue,
    started: bool,
}

// SAFETY: the pointers name CoreServices and libdispatch objects that may be
// used from any thread. A stream is only driven by whoever owns it.
unsafe impl Send for Stream {}

extern "C" fn retain<L: Listener>(info: *const c_void) -> *const c_void {
    // SAFETY: `info` is the pointer `Arc::into_raw` made of an `Arc<L>`.
    unsafe { Arc::increment_strong_count(info.cast::<L>()) };
    info
}

extern "C" fn release<L: Listener>(info: *const c_void) {
    // SAFETY: balances a `retain` (or the count `start` handed over).
    unsafe { Arc::decrement_strong_count(info.cast::<L>()) };
}

extern "C" fn deliver<L: Listener>(
    _stream: *const c_void,
    info: *mut c_void,
    count: usize,
    paths: *mut c_void,
    flags: *const u32,
    _ids: *const u64,
) {
    // SAFETY: the stream keeps a count on the listener until it is released.
    let listener = unsafe { &*info.cast::<L>() };
    let events = Events {
        paths: paths.cast_const().cast(),
        flags,
        count,
        next: 0,
        batch: PhantomData,
    };
    // A panic must not unwind into CoreServices.
    if catch_unwind(AssertUnwindSafe(|| listener.heard(events))).is_err() {
        listener.lost();
    }
}

extern "C" fn nothing(_: *mut c_void) {}

impl Stream {
    /// Watch `paths` and everything below them. Events are delivered once
    /// `latency` has passed since the first of a burst, folded into one call.
    /// None when the system refuses the stream, or a path is not UTF-8.
    pub(super) fn start<L: Listener>(
        paths: &[&Path],
        latency: Duration,
        listener: &Arc<L>,
    ) -> Option<Self> {
        #[cfg(test)]
        if REFUSE.with(std::cell::Cell::get) {
            return None;
        }
        let mut strings: Vec<CFStringRef> = Vec::with_capacity(paths.len());
        for path in paths {
            let bytes = path.as_os_str().as_bytes();
            // SAFETY: `bytes` is valid for its length; the result is ours to release.
            let string = unsafe {
                CFStringCreateWithBytes(
                    ptr::null(),
                    bytes.as_ptr(),
                    bytes.len() as CFIndex,
                    kCFStringEncodingUTF8,
                    0,
                )
            };
            strings.push(string);
        }
        let array = if strings.iter().any(|string| string.is_null()) {
            ptr::null()
        } else {
            // SAFETY: `strings` holds `len` valid CFStrings, which the array retains.
            unsafe {
                CFArrayCreate(
                    ptr::null(),
                    strings.as_ptr().cast(),
                    strings.len() as CFIndex,
                    &kCFTypeArrayCallBacks,
                )
            }
        };
        for string in strings.into_iter().filter(|string| !string.is_null()) {
            // SAFETY: created above and not released yet.
            unsafe { CFRelease(string as CFTypeRef) };
        }
        if array.is_null() {
            return None;
        }
        let info = Arc::into_raw(listener.clone());
        let mut context = Context {
            version: 0,
            info: info.cast_mut().cast(),
            retain: Some(retain::<L>),
            release: Some(release::<L>),
            copy_description: None,
        };
        // SAFETY: `context` is copied by the call, which also retains `info`.
        let stream = unsafe {
            FSEventStreamCreate(
                ptr::null(),
                deliver::<L>,
                &mut context,
                array,
                SINCE_NOW,
                latency.as_secs_f64(),
                FILE_EVENTS | WATCH_ROOT,
            )
        };
        // SAFETY: gives back the count `into_raw` took; the stream has its own.
        // If it could not be made, this is the last one and frees the listener.
        drop(unsafe { Arc::from_raw(info) });
        // SAFETY: the stream holds its own reference to the array.
        unsafe { CFRelease(array as CFTypeRef) };
        if stream.is_null() {
            return None;
        }
        // SAFETY: a plain serial queue; the label is a NUL-terminated literal.
        let queue = unsafe {
            dispatch_queue_create(c"riwork.project-recency.fsevents".as_ptr(), ptr::null())
        };
        if queue.is_null() {
            // SAFETY: never scheduled, so only released.
            unsafe { FSEventStreamRelease(stream) };
            return None;
        }
        let mut this = Self {
            stream,
            queue,
            started: false,
        };
        // SAFETY: both are live; the stream retains the queue.
        unsafe { FSEventStreamSetDispatchQueue(stream, queue) };
        // SAFETY: scheduled above.
        this.started = unsafe { FSEventStreamStart(stream) } != 0;
        this.started.then_some(this)
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // SAFETY: the stream and queue are ours and still live. After
        // invalidation no further callback is queued, and the empty
        // synchronous block returns once one already queued has finished, so
        // nothing is running when the stream is let go. The listener it held is
        // released a moment later, by the system, on a queue of its own.
        unsafe {
            if self.started {
                FSEventStreamStop(self.stream);
            }
            FSEventStreamInvalidate(self.stream);
            dispatch_sync_f(self.queue, ptr::null_mut(), nothing);
            FSEventStreamRelease(self.stream);
            dispatch_release(self.queue);
        }
    }
}
