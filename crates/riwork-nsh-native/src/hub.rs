//! Snapshot topics shared by every engine. No engine thread takes the hub lock.
use std::sync::{Mutex, PoisonError};

use async_channel::{Receiver, Sender, TrySendError};
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

struct Listener<T> {
    tx: Sender<Snapshot<T>>,
    // The producer can clear an overflowing listener without blocking it.
    drain: Receiver<Snapshot<T>>,
}

struct State<T> {
    latest: Option<Snapshot<T>>,
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
                data,
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
        TopicSubscription { rx }
    }

    /// Only the process service publishes. Equal values do not wake any engine.
    pub(crate) fn publish(&self, data: T) {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if state
            .latest
            .as_ref()
            .is_some_and(|snapshot| snapshot.data == data)
        {
            return;
        }
        state.rev = state.rev.checked_add(1).expect("topic revision exhausted");
        let snapshot = Snapshot {
            kind: "snapshot".into(),
            rev: state.rev,
            data,
            resync: false,
        };
        state.latest = Some(snapshot.clone());
        state
            .listeners
            .retain(|listener| match listener.tx.try_send(snapshot.clone()) {
                Ok(()) => true,
                Err(TrySendError::Closed(_)) => false,
                Err(TrySendError::Full(mut snapshot)) => {
                    while listener.drain.try_recv().is_ok() {}
                    snapshot.resync = true;
                    listener.tx.try_send(snapshot).is_ok()
                }
            });
    }
}

/// Closing or dropping a subscription closes its queue immediately. Removal from
/// the hub is lazy, avoiding any cross-thread lock during owner cleanup.
pub struct TopicSubscription<T> {
    rx: Receiver<Snapshot<T>>,
}

impl<T> TopicSubscription<T> {
    pub async fn recv(&self) -> Result<Snapshot<T>, async_channel::RecvError> {
        self.rx.recv().await
    }

    /// Snapshot topics can discard intermediate states before one JS delivery.
    pub(crate) fn latest(&self, mut snapshot: Snapshot<T>) -> Snapshot<T> {
        while let Ok(next) = self.rx.try_recv() {
            let resync = snapshot.resync || next.resync;
            snapshot = next;
            snapshot.resync = resync;
        }
        snapshot
    }

    pub fn close(&self) {
        self.rx.close();
    }
}

impl<T> Drop for TopicSubscription<T> {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
