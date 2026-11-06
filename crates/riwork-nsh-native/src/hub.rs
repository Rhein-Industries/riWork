//! Snapshot topics shared by every engine. No engine thread takes the hub lock.
use std::sync::{Arc, Mutex, PoisonError};

use async_channel::{Receiver, Sender, TrySendError};
use nsh_core::element::{JsHost, JsIn, JsOwned, JsValue, PropError, TsTypes};
use serde::{Deserialize, Serialize};

pub const SUBSCRIPTION_CAPACITY: usize = 64;

/// A complete, authoritative value of one process-wide topic.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot<T> {
    pub kind: String,
    pub rev: u64,
    pub data: T,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub resync: bool,
}

/// A shared Rust delivery payload with exactly the wrapped value's JS shape.
/// Queues and posted callbacks keep this handle; conversion borrows the value.
#[doc(hidden)]
#[derive(Debug, PartialEq)]
pub struct Shared<T>(Arc<T>);

impl<T> Shared<T> {
    fn new(value: T) -> Self {
        Self(Arc::new(value))
    }

    fn into_owned(self, clone: fn(&T) -> T) -> T {
        Arc::try_unwrap(self.0).unwrap_or_else(|shared| clone(&shared))
    }
}

impl<T> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> std::ops::Deref for Shared<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T: Serialize> Serialize for Shared<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.as_ref().serialize(serializer)
    }
}

impl<T: JsValue> JsValue for Shared<T> {
    fn ts_type(types: &mut TsTypes) -> String {
        T::ts_type(types)
    }
    fn from_js(value: JsIn<'_>) -> Result<Self, PropError> {
        T::from_js(value).map(Self::new)
    }
    fn to_js<'a>(&self, host: &'a dyn JsHost) -> Result<JsOwned<'a>, PropError> {
        self.0.to_js(host)
    }
}

/// Typed callback adapter; its declaration and decoding remain Snapshot<T>'s.
#[doc(hidden)]
pub struct SharedSnapshot<T>(pub(crate) Snapshot<Shared<T>>);

impl<T> From<Snapshot<T>> for SharedSnapshot<T> {
    fn from(snapshot: Snapshot<T>) -> Self {
        Self(Snapshot {
            kind: snapshot.kind,
            rev: snapshot.rev,
            data: Shared::new(snapshot.data),
            resync: snapshot.resync,
        })
    }
}

impl<T: Serialize> JsValue for SharedSnapshot<T>
where
    Snapshot<T>: JsValue,
{
    fn ts_type(types: &mut TsTypes) -> String {
        Snapshot::<T>::ts_type(types)
    }
    fn from_js(value: JsIn<'_>) -> Result<Self, PropError> {
        Snapshot::<T>::from_js(value).map(Self::from)
    }
    fn to_js<'a>(&self, host: &'a dyn JsHost) -> Result<JsOwned<'a>, PropError> {
        // Snapshot's existing serde shape, including omitted false resync. No
        // owned T is made before the same JSON-to-JS conversion as owned events.
        serde_json::to_value(&self.0)
            .map_err(|error| PropError(error.to_string()))?
            .to_js(host)
    }
}

struct Listener<T> {
    tx: Sender<Snapshot<Shared<T>>>,
    // The producer can clear an overflowing listener without blocking it.
    drain: Receiver<Snapshot<Shared<T>>>,
}

struct State<T> {
    latest: Option<Snapshot<Shared<T>>>,
    rev: u64,
    listeners: Vec<Listener<T>>,
}

pub(crate) struct Hub<T>(Mutex<State<T>>);

impl<T: Clone + PartialEq> Hub<T> {
    pub(crate) fn new(data: T) -> Self {
        Self(Mutex::new(State {
            latest: Some(Snapshot {
                kind: "snapshot".into(),
                rev: 1,
                data: Shared::new(data),
                resync: false,
            }),
            rev: 1,
            listeners: Vec::new(),
        }))
    }

    /// A source that is temporarily unavailable must not invent an empty
    /// snapshot. Its first successful read becomes the first delivery.
    pub(crate) fn pending() -> Self {
        Self(Mutex::new(State {
            latest: None,
            rev: 0,
            listeners: Vec::new(),
        }))
    }

    /// The process can stop refreshing a project once every window has closed
    /// its subscription. Cleanup never performs domain I/O on an engine.
    pub(crate) fn has_subscribers(&self) -> bool {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        state.listeners.retain(|listener| !listener.tx.is_closed());
        !state.listeners.is_empty()
    }

    /// Called from the blocking pool, so first delivery and registration are atomic
    /// with publication. A change cannot slip between the snapshot and the listener.
    pub(crate) fn subscribe(&self) -> TopicSubscription<T> {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let (tx, rx) = async_channel::bounded(SUBSCRIPTION_CAPACITY);
        if let Some(snapshot) = &state.latest {
            tx.try_send(snapshot.clone())
                .expect("new topic queue has room");
        }
        state.listeners.retain(|listener| !listener.tx.is_closed());
        state.listeners.push(Listener {
            tx,
            drain: rx.clone(),
        });
        TopicSubscription {
            rx,
            clone_data: T::clone,
        }
    }

    /// Only the process service publishes. Equal values do not wake any engine.
    /// Returns whether the value changed.
    pub(crate) fn publish(&self, data: T) -> bool {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if state
            .latest
            .as_ref()
            .is_some_and(|snapshot| *snapshot.data == data)
        {
            return false;
        }
        state.rev = state.rev.checked_add(1).expect("topic revision exhausted");
        let snapshot = Snapshot {
            kind: "snapshot".into(),
            rev: state.rev,
            data: Shared::new(data),
            resync: false,
        };
        state.listeners.retain(|listener| {
            if listener.tx.is_closed() {
                return false;
            }
            match listener.tx.try_send(snapshot.clone()) {
                Ok(()) => true,
                Err(TrySendError::Closed(_)) => false,
                Err(TrySendError::Full(mut snapshot)) => {
                    while listener.drain.try_recv().is_ok() {}
                    snapshot.resync = true;
                    listener.tx.try_send(snapshot).is_ok()
                }
            }
        });
        // Every queued envelope and the cache share one immutable payload.
        state.latest = Some(snapshot);
        true
    }
}

/// Closing or dropping a subscription closes its queue immediately. Removal from
/// the hub is lazy, avoiding any cross-thread lock during owner cleanup.
pub struct TopicSubscription<T> {
    rx: Receiver<Snapshot<Shared<T>>>,
    // Retain the owned recv API without imposing a new Clone bound on callers
    // generic over TopicSubscription<T>. The producer already requires Clone.
    clone_data: fn(&T) -> T,
}

impl<T> TopicSubscription<T> {
    pub async fn recv(&self) -> Result<Snapshot<T>, async_channel::RecvError> {
        self.recv_shared()
            .await
            .map(|snapshot| self.owned_snapshot(snapshot))
    }

    pub(crate) async fn recv_shared(
        &self,
    ) -> Result<Snapshot<Shared<T>>, async_channel::RecvError> {
        self.rx.recv().await
    }

    fn owned_snapshot(&self, snapshot: Snapshot<Shared<T>>) -> Snapshot<T> {
        Snapshot {
            kind: snapshot.kind,
            rev: snapshot.rev,
            data: snapshot.data.into_owned(self.clone_data),
            resync: snapshot.resync,
        }
    }

    /// Snapshot topics can discard intermediate states before one JS delivery.
    pub(crate) fn latest(&self, snapshot: Snapshot<T>) -> Snapshot<T> {
        let Ok(mut next) = self.rx.try_recv() else {
            return snapshot;
        };
        next.resync |= snapshot.resync;
        self.owned_snapshot(self.latest_shared(next))
    }

    pub(crate) fn latest_shared(&self, snapshot: Snapshot<Shared<T>>) -> Snapshot<Shared<T>> {
        coalesce(snapshot, std::iter::from_fn(|| self.rx.try_recv().ok()))
    }

    pub fn close(&self) {
        self.rx.close();
    }
}

fn coalesce<T>(
    mut snapshot: Snapshot<T>,
    queued: impl Iterator<Item = Snapshot<T>>,
) -> Snapshot<T> {
    // A producer may refill the queue while it is drained. Bound one delivery's
    // work so continuous publication cannot prevent the engine's next frame.
    for next in queued.take(SUBSCRIPTION_CAPACITY) {
        let resync = snapshot.resync || next.resync;
        snapshot = next;
        snapshot.resync = resync;
    }
    snapshot
}

impl<T> Drop for TopicSubscription<T> {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
#[path = "hub_tests.rs"]
mod delivery_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn shared_publication_never_clones_data_and_owned_receive_stays_independent() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };

        #[derive(Debug, Serialize)]
        struct Tracked {
            value: usize,
            #[serde(skip)]
            clones: Arc<AtomicUsize>,
        }
        impl Clone for Tracked {
            fn clone(&self) -> Self {
                self.clones.fetch_add(1, Ordering::SeqCst);
                Self {
                    value: self.value,
                    clones: self.clones.clone(),
                }
            }
        }
        impl PartialEq for Tracked {
            fn eq(&self, other: &Self) -> bool {
                self.value == other.value
            }
        }
        let clones = Arc::new(AtomicUsize::new(0));
        let tracked = |value| Tracked {
            value,
            clones: clones.clone(),
        };
        let hub = Hub::new(tracked(0));
        assert!(hub.publish(tracked(1)));
        assert_eq!(clones.load(Ordering::SeqCst), 0);
        let first = hub.subscribe();
        let closed = hub.subscribe();
        let second = hub.subscribe();
        let first_value = first.recv_shared().await.unwrap();
        let second_value = second.recv_shared().await.unwrap();
        assert_eq!(first_value.data.value, 1);
        assert!(Arc::ptr_eq(&first_value.data.0, &second_value.data.0));
        assert_eq!(
            serde_json::to_value(&first_value).unwrap()["data"]["value"],
            1
        );
        assert_eq!(clones.load(Ordering::SeqCst), 0);
        closed.close();
        clones.store(0, Ordering::SeqCst);
        assert!(hub.publish(tracked(2)));
        assert_eq!(clones.load(Ordering::SeqCst), 0);
        let mut owned = first.recv().await.unwrap();
        assert_eq!(owned.data.value, 2);
        owned.data.value = 999;
        assert_eq!(second.recv().await.unwrap().data.value, 2);
        assert_eq!(clones.load(Ordering::SeqCst), 2);
        assert!(!hub.publish(tracked(2)));
        assert_eq!(clones.load(Ordering::SeqCst), 2);
        drop(first);
        drop(second);
        assert!(hub.publish(tracked(3)));
        assert_eq!(clones.load(Ordering::SeqCst), 2);
        let latest = hub.subscribe().recv().await.unwrap();
        assert_eq!(
            (latest.rev, latest.data.value, latest.resync),
            (4, 3, false)
        );
    }

    #[tokio::test]
    async fn first_snapshot_deduplication_overflow_and_close() {
        let hub = Hub::new(0);
        let listener = hub.subscribe();
        let initial = listener.recv().await.unwrap();
        assert_eq!((initial.rev, initial.data, initial.resync), (1, 0, false));
        hub.publish(0);
        assert!(listener.rx.is_empty());
        for value in 1..=SUBSCRIPTION_CAPACITY + 1 {
            hub.publish(value as i32);
        }
        let recovered = listener.recv().await.unwrap();
        assert_eq!(recovered.data, 65);
        assert_eq!(recovered.rev, 66);
        assert!(recovered.resync);
        assert!(listener.rx.is_empty());
        listener.close();
        assert!(listener.recv().await.is_err());
        hub.publish(100);
        assert!(hub.0.lock().unwrap().listeners.is_empty());
    }

    #[tokio::test]
    async fn coalescing_keeps_overflow_resync_and_releases_obsolete_payloads() {
        let hub = Hub::new(vec![0u8; 1024]);
        let listener = hub.subscribe();
        let initial = listener.recv_shared().await.unwrap();
        let obsolete = Arc::downgrade(&initial.data.0);
        drop(initial);
        for value in 1..=SUBSCRIPTION_CAPACITY + 2 {
            hub.publish(vec![value as u8; 1024]);
        }
        assert!(obsolete.upgrade().is_none());
        let latest = listener.latest_shared(listener.recv_shared().await.unwrap());
        assert_eq!((latest.rev, latest.data[0], latest.resync), (67, 66, true));
        assert!(listener.rx.is_empty());
        // Overflow belongs to the affected listener, never the hub's cache.
        let fresh = hub.subscribe().recv_shared().await.unwrap();
        assert!(!fresh.resync);
        assert!(Arc::ptr_eq(&latest.data.0, &fresh.data.0));
        assert!(
            serde_json::to_value(&latest).unwrap()["resync"]
                .as_bool()
                .unwrap()
        );
        assert!(
            serde_json::to_value(&fresh)
                .unwrap()
                .get("resync")
                .is_none()
        );
    }

    #[tokio::test]
    async fn pending_close_drop_and_hub_teardown_preserve_queue_lifetimes() {
        let hub = Hub::pending();
        let first = hub.subscribe();
        let second = hub.subscribe();
        assert!(first.rx.is_empty());
        hub.publish(vec![7u8; 4096]);
        let held = first.recv_shared().await.unwrap();
        let weak = Arc::downgrade(&held.data.0);
        assert_eq!(held.rev, 1);
        second.close();
        // Closing still permits draining already queued data, as owned recv did.
        assert_eq!(second.recv().await.unwrap().data, vec![7u8; 4096]);
        assert!(second.recv().await.is_err());
        drop(first);
        assert!(!hub.has_subscribers());
        drop(hub);
        assert!(
            weak.upgrade().is_some(),
            "a held delivery survives hub teardown"
        );
        drop(held);
        assert!(
            weak.upgrade().is_none(),
            "the final delivery releases its payload"
        );
    }

    #[test]
    fn subscribe_and_publish_cannot_lose_the_first_change() {
        for _ in 0..64 {
            let hub = Arc::new(Hub::new(0));
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let publisher = {
                let hub = hub.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    hub.publish(1);
                })
            };
            barrier.wait();
            let listener = hub.subscribe();
            publisher.join().unwrap();
            let first = listener.rx.try_recv().unwrap();
            match first.rev {
                1 => assert_eq!(listener.rx.try_recv().unwrap().rev, 2),
                2 => assert_eq!(*first.data, 1),
                rev => panic!("unexpected revision {rev}"),
            }
            assert!(listener.rx.is_empty());
        }
    }

    #[test]
    fn coalescing_yields_when_the_producer_keeps_replenishing() {
        let event = |rev| Snapshot {
            kind: "snapshot".into(),
            rev,
            data: rev,
            resync: rev == 3,
        };
        let mut reads = 0;
        let always_ready = std::iter::from_fn(|| {
            reads += 1;
            assert!(reads <= SUBSCRIPTION_CAPACITY + 1, "unbounded queue drain");
            Some(event(reads as u64))
        });
        let latest = coalesce(event(0), always_ready);
        assert_eq!(reads, SUBSCRIPTION_CAPACITY);
        assert_eq!(latest.rev, SUBSCRIPTION_CAPACITY as u64);
        assert!(
            latest.resync,
            "overflow in a consumed intermediate survives"
        );
    }
}
