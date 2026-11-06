//! Host snapshot tail coalescing with ordered commands and receiver-owned retirement.
//! Sender publishing and merging are serialized by a gate; receiver waiting uses
//! std mpsc. Only a stable, uninterrupted snapshot segment can be coalesced; this
//! does not bound all queued commands or changed layout generations.
use std::sync::{mpsc, Arc, Mutex, MutexGuard, Weak, atomic::{AtomicUsize, Ordering}};
use std::time::Duration;

/// Short bursts already fit in one consumer materialization pass.
pub const SNAPSHOT_BATCH_LIMIT: usize = 32;

/// Only snapshot messages are mergeable. Successful merging must retain every
/// potentially large owner in `self` until all duplicate owners in `next` drop.
/// A changed identity returns `next` intact; commands are never mergeable.
pub trait MergeMessage: Sized {
    fn mergeable(&self) -> bool;
    fn try_merge(&mut self, next: Self) -> Result<(), Self>;
}

type Slot<T> = Mutex<Option<T>>;

enum Queued<T> {
    OwnedSnapshot(T),
    Snapshot(Arc<Slot<T>>),
    Command(T),
}

struct SendState<T> {
    closed: bool,
    tail: Option<Weak<Slot<T>>>,
}

pub struct Sender<T> {
    sender: mpsc::Sender<Queued<T>>,
    state: Arc<Mutex<SendState<T>>>,
    pending: Arc<AtomicUsize>,
    #[cfg(test)]
    hooks: Arc<Hooks>,
}

pub struct Receiver<T> {
    receiver: mpsc::Receiver<Queued<T>>,
    state: Arc<Mutex<SendState<T>>>,
    pending: Arc<AtomicUsize>,
    #[cfg(test)]
    hooks: Arc<Hooks>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let (sender, receiver) = mpsc::channel();
    let state = Arc::new(Mutex::new(SendState { closed: false, tail: None }));
    let pending = Arc::new(AtomicUsize::new(0));
    #[cfg(test)]
    let hooks = Arc::new(Hooks::default());
    (
        Sender { sender, state: Arc::clone(&state), pending: Arc::clone(&pending), #[cfg(test)] hooks: Arc::clone(&hooks) },
        Receiver { receiver, state, pending, #[cfg(test)] hooks },
    )
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            state: Arc::clone(&self.state),
            pending: Arc::clone(&self.pending),
            #[cfg(test)]
            hooks: Arc::clone(&self.hooks),
        }
    }
}

impl<T: MergeMessage> Sender<T> {
    pub fn send(&self, mut message: T) -> Result<(), mpsc::SendError<T>> {
        #[cfg(test)]
        self.hooks.call(Point::BeforeSendGate);
        // This gate linearizes snapshot replacement and every sender's command
        // enqueue. It also excludes receiver teardown while a producer has a
        // temporary strong reference to an occupied snapshot slot.
        let mut state = lock(&self.state);
        if state.closed {
            return Err(mpsc::SendError(message));
        }
        if message.mergeable() {
            if self.pending.load(Ordering::Acquire) >= SNAPSHOT_BATCH_LIMIT
                && let Some(slot) = state.tail.as_ref().and_then(Weak::upgrade)
            {
                #[cfg(test)]
                self.hooks.call(Point::Upgraded);
                let mut value = lock(&slot);
                if let Some(previous) = value.as_mut() {
                    match previous.try_merge(message) {
                        Ok(()) => {
                            #[cfg(test)]
                            self.hooks.call(Point::Merged);
                            drop(value);
                            drop(slot);
                            // No producer strong slot reference outlives gate.
                            return Ok(());
                        }
                        Err(next) => message = next,
                    }
                }
                drop(value);
                drop(slot);
            }
            // Increment before publishing to mpsc, so a concurrent receiver
            // cannot decrement an envelope whose increment is still pending.
            // This counts enqueued or sender-in-flight envelopes until owned
            // extraction, not receiver-held payloads or retained layout memory.
            let pending = self.pending.fetch_add(1, Ordering::AcqRel) + 1;
            #[cfg(test)]
            self.hooks.call(Point::CountedBeforeEnqueue);
            let queued = if pending < SNAPSHOT_BATCH_LIMIT {
                // A plain enqueue seals any prior tail, even if that tail was
                // created at high backlog before concurrent receive lowered it.
                state.tail = None;
                Queued::OwnedSnapshot(message)
            } else {
                let slot = Arc::new(Mutex::new(Some(message)));
                state.tail = Some(Arc::downgrade(&slot));
                Queued::Snapshot(slot)
            };
            // Move, not clone: the producer no longer owns this new slot after
            // successful enqueue, before releasing the sender gate.
            match self.sender.send(queued) {
                Ok(()) => {
                    #[cfg(test)]
                    self.hooks.call(Point::EnqueuedSnapshot);
                    Ok(())
                }
                Err(mpsc::SendError(Queued::Snapshot(slot))) => {
                    self.pending.fetch_sub(1, Ordering::AcqRel);
                    state.tail = None;
                    let value = lock(&slot).take().expect("failed snapshot owns its payload");
                    drop(slot);
                    Err(mpsc::SendError(value))
                }
                Err(mpsc::SendError(Queued::OwnedSnapshot(value))) => {
                    self.pending.fetch_sub(1, Ordering::AcqRel);
                    Err(mpsc::SendError(value))
                }
                Err(mpsc::SendError(Queued::Command(_))) => unreachable!(),
            }
        } else {
            state.tail = None;
            #[cfg(test)]
            self.hooks.call(Point::BeforeCommandEnqueue);
            self.sender.send(Queued::Command(message)).map_err(|error| match error.0 {
                Queued::Command(message) => mpsc::SendError(message),
                Queued::Snapshot(_) | Queued::OwnedSnapshot(_) => unreachable!(),
            })
        }
    }
}

impl<T> Receiver<T> {
    fn owned(&self, queued: Queued<T>) -> T {
        match queued {
            Queued::Command(message) => message,
            Queued::OwnedSnapshot(message) => {
                self.consumed_snapshot();
                message
            }
            Queued::Snapshot(slot) => {
                #[cfg(test)]
                self.hooks.call(Point::DequeuedBeforeTake);
                let message = lock(&slot).take().expect("snapshot received once");
                self.consumed_snapshot();
                #[cfg(test)]
                self.hooks.call(Point::Taken);
                // Even if a producer still holds the slot Arc, it is empty.
                drop(slot);
                message
            }
        }
    }

    fn consumed_snapshot(&self) {
        // No sender-order gate is acquired from the receive path. The slot lock
        // (if any) has already been released before this count changes.
        let previous = self.pending.fetch_sub(1, Ordering::AcqRel);
        assert!(previous > 0, "received snapshot has a published pending count");
    }

    pub fn recv(&self) -> Result<T, mpsc::RecvError> {
        self.receiver.recv().map(|queued| self.owned(queued))
    }

    pub fn recv_timeout(&self, duration: Duration) -> Result<T, mpsc::RecvTimeoutError> {
        self.receiver.recv_timeout(duration).map(|queued| self.owned(queued))
    }

    pub fn try_recv(&self) -> Result<T, mpsc::TryRecvError> {
        self.receiver.try_recv().map(|queued| self.owned(queued))
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        #[cfg(test)]
        self.hooks.call(Point::BeforeCloseGate);
        {
            let mut state = lock(&self.state);
            state.closed = true;
            state.tail = None;
        }
        // Every producer strong slot reference ended before the gate above was
        // acquired. Future sends see closed. Queued payloads can now be retired
        // on this receiver's thread without holding the sender gate.
        #[cfg(test)]
        self.hooks.call(Point::Closed);
        // Explicitly retire every remaining owned message here rather than
        // depending on std mpsc's internal queued-value destruction strategy.
        // Closure above prevents new enqueues, so this drain is finite; it never
        // holds the sender gate while payload destructors run.
        while let Ok(queued) = self.receiver.try_recv() {
            drop(self.owned(queued));
        }
    }
}

// Scheduling hooks are compiled only for deterministic unit tests.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Point {
    BeforeSendGate, BeforeCommandEnqueue, CountedBeforeEnqueue, Upgraded, Merged, EnqueuedSnapshot, DequeuedBeforeTake, Taken, BeforeCloseGate, Closed,
}

#[cfg(test)]
#[derive(Default)]
pub struct Hooks {
    callback: Mutex<Option<Arc<dyn Fn(Point) + Send + Sync>>>,
}

#[cfg(test)]
impl Hooks {
    fn call(&self, point: Point) {
        let callback = lock(&self.callback).clone();
        if let Some(callback) = callback { callback(point); }
    }
}

#[cfg(test)]
impl<T> Sender<T> {
    pub fn pending_snapshots(&self) -> usize { self.pending.load(Ordering::Acquire) }
    pub fn hook(&self, callback: impl Fn(Point) + Send + Sync + 'static) {
        *lock(&self.hooks.callback) = Some(Arc::new(callback));
    }
}

#[cfg(test)]
impl<T> Receiver<T> {
    pub fn disconnect_empty_transport_for_test(&mut self) {
        assert_eq!(self.pending.load(Ordering::Acquire), 0);
        let (_, disconnected) = mpsc::channel();
        // Fault injection exercises the SendError rollback branch. Production
        // cannot disconnect mpsc separately from the receiver's closed gate.
        self.receiver = disconnected;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::atomic::{AtomicBool, Ordering}, thread};

    struct Payload(mpsc::Sender<thread::ThreadId>);
    impl Drop for Payload {
        fn drop(&mut self) { self.0.send(thread::current().id()).unwrap(); }
    }
    struct Snapshot(Arc<Payload>);
    impl MergeMessage for Snapshot {
        fn mergeable(&self) -> bool { true }
        fn try_merge(&mut self, next: Self) -> Result<(), Self> {
            if Arc::ptr_eq(&self.0, &next.0) { drop(next); Ok(()) } else { Err(next) }
        }
    }

    #[test]
    fn threshold_resets_after_receive_and_failed_send_returns_ownership() {
        let (destroyed, destruction) = mpsc::channel();
        let owner = Arc::new(Payload(destroyed));
        let (sender, mut receiver) = channel();
        for _ in 0..(SNAPSHOT_BATCH_LIMIT - 1) {
            assert!(sender.send(Snapshot(Arc::clone(&owner))).is_ok());
        }
        assert_eq!(sender.pending_snapshots(), SNAPSHOT_BATCH_LIMIT - 1);
        assert!(sender.send(Snapshot(Arc::clone(&owner))).is_ok());
        assert!(sender.send(Snapshot(Arc::clone(&owner))).is_ok());
        assert_eq!(sender.pending_snapshots(), SNAPSHOT_BATCH_LIMIT);
        for _ in 0..(SNAPSHOT_BATCH_LIMIT - 1) { drop(receiver.recv().unwrap()); }
        let received = receiver.recv().unwrap();
        assert!(Arc::ptr_eq(&received.0, &owner));
        // A receiver-held payload is no longer counted as queued work.
        assert_eq!(sender.pending_snapshots(), 0);
        assert!(sender.send(Snapshot(Arc::clone(&owner))).is_ok());
        assert_eq!(sender.pending_snapshots(), 1);
        drop(receiver.recv().unwrap());
        assert_eq!(sender.pending_snapshots(), 0);

        receiver.disconnect_empty_transport_for_test();
        let Err(mpsc::SendError(returned)) = sender.send(Snapshot(Arc::clone(&owner))) else {
            panic!("disconnected send must return its original payload");
        };
        assert!(Arc::ptr_eq(&returned.0, &owner));
        assert_eq!(sender.pending_snapshots(), 0);
        drop(returned); drop(received); drop(receiver); drop(sender); drop(owner);
        assert_eq!(destruction.recv().unwrap(), thread::current().id());
        assert!(destruction.try_recv().is_err());
    }

    #[test]
    fn receiver_teardown_retires_payload_after_an_upgraded_producer_slot() {
        let (destroyed, destruction) = mpsc::channel();
        let owner = Arc::new(Payload(destroyed));
        let (sender, receiver) = channel();
        for _ in 0..SNAPSHOT_BATCH_LIMIT {
            assert!(sender.send(Snapshot(Arc::clone(&owner))).is_ok());
        }
        let next = Snapshot(owner);
        let (upgraded, wait_upgraded) = mpsc::channel();
        let (resume, wait_resume) = mpsc::channel();
        let wait_resume = Mutex::new(wait_resume);
        let (closing, wait_closing) = mpsc::channel();
        let closed = Arc::new(AtomicBool::new(false));
        let observed_closed = Arc::clone(&closed);
        sender.hook(move |point| match point {
            Point::Upgraded => {
                upgraded.send(()).unwrap();
                wait_resume.lock().unwrap().recv().unwrap();
            }
            Point::BeforeCloseGate => { closing.send(()).unwrap(); }
            Point::Closed => { observed_closed.store(true, Ordering::SeqCst); }
            _ => {}
        });
        let producer = thread::spawn(move || assert!(sender.send(next).is_ok()));
        wait_upgraded.recv().unwrap();
        let worker = thread::spawn(move || { drop(receiver); thread::current().id() });
        wait_closing.recv().unwrap();
        // The producer holds the gate at a known suspension point. The receiver
        // cannot finish closure or retire the payload until that reference ends.
        assert!(!closed.load(Ordering::SeqCst));
        assert!(destruction.try_recv().is_err());
        resume.send(()).unwrap();
        producer.join().unwrap();
        let worker = worker.join().unwrap();
        assert!(closed.load(Ordering::SeqCst));
        assert_eq!(destruction.recv().unwrap(), worker);
        assert!(destruction.try_recv().is_err());
    }
}
