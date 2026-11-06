use super::*;
use nsh_core::prelude::*;
use serde_json::{Value, json};

#[module]
struct Parity;

#[module]
impl Parity {
    pub fn typed(
        resync: bool,
        owned: Callback<Snapshot<crate::dto::SessionsSnapshot>>,
        shared: Callback<SharedSnapshot<crate::dto::SessionsSnapshot>>,
    ) {
        let event = Snapshot {
            kind: "snapshot".into(),
            rev: 42,
            data: crate::dto::SessionsSnapshot::default(),
            resync,
        };
        shared.post(SharedSnapshot(Snapshot {
            kind: event.kind.clone(),
            rev: event.rev,
            data: Shared::new(event.data.clone()),
            resync,
        }));
        owned.post(event);
    }

    pub fn raw(owned: Callback<Value>, shared: Callback<Shared<Value>>) {
        let value = json!({"unicode":"Σ🦀\n\"", "empty":{}, "list":[null,false,0,u64::MAX]});
        shared.post(Shared::new(value.clone()));
        owned.post(value);
    }
}

#[native_module("test/hub")]
fn native(registry: &mut Registry) {
    registry.module::<Parity>();
}

#[test]
fn shared_callback_values_have_exact_owned_js_shape() {
    use nsh_core::{ApproxTextEngine, MountBatch};
    use nsh_engine::{Bundle, Engine, EngineConfig, EngineControl, HostChannel, MountSink};
    use std::{
        sync::{OnceLock, mpsc},
        time::Duration,
    };
    struct Sink {
        control: Arc<OnceLock<EngineControl>>,
        output: mpsc::Sender<Value>,
    }
    impl MountSink for Sink {
        fn submit(&self, _: MountBatch) {}
        fn report_error(&self, error: &str) {
            let _ = self.output.send(json!({"error":error}));
        }
        fn request_frame(&self) {
            if let Some(control) = self.control.get() {
                control.frame();
            }
        }
    }
    let (tx, rx) = mpsc::channel();
    let host = HostChannel::new();
    let output = tx.clone();
    host.handle("result", move |mut call| {
        let _ = output.send(call.take_data());
    });
    let bundle = Bundle::source(
        r#"
        import { host } from 'nsh';
        import { Parity } from 'test/hub';
        const values = {};
        let remaining = 6;
        const receive = key => value => {
            values[key] = value;
            if (--remaining === 0) host.post('result', values);
        };
        Parity.typed(false, receive('owned'), receive('shared'));
        Parity.typed(true, receive('ownedResync'), receive('sharedResync'));
        Parity.raw(receive('rawOwned'), receive('rawShared'));
    "#
        .into(),
    )
    .with_capabilities_json(Some(r#"{"host":["result"]}"#))
    .unwrap();
    let mut config = EngineConfig::new(bundle);
    config.ui_sets = vec![nsh_base::registry(), registry()];
    config.host = host;
    let control = Arc::new(OnceLock::new());
    let setup = control.clone();
    let engine = Engine::start_with(
        config,
        || ApproxTextEngine,
        Sink {
            control,
            output: tx,
        },
        move |control| {
            setup.set(control.clone()).ok();
        },
    )
    .unwrap();
    let values = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(values.get("error").is_none(), "{values}");
    assert_eq!(values["owned"], values["shared"]);
    assert_eq!(values["ownedResync"], values["sharedResync"]);
    assert_eq!(values["rawOwned"], values["rawShared"]);
    assert_eq!(values["shared"]["rev"], 42);
    assert!(values["shared"].get("resync").is_none());
    assert_eq!(values["sharedResync"]["resync"], true);
    let declarations = nsh_core::element::declarations(&registry(), &[]);
    assert!(declarations.contains("SessionsEvent"));
    assert!(!declarations.contains("Shared"));
    drop(engine);
}

#[derive(Default)]
struct Memory {
    bytes: std::sync::atomic::AtomicUsize,
    clones: std::sync::atomic::AtomicUsize,
}

struct Payload {
    revision: usize,
    text: String,
    memory: Arc<Memory>,
}
impl Payload {
    fn new(revision: usize, bytes: usize, memory: &Arc<Memory>) -> Self {
        memory
            .bytes
            .fetch_add(bytes, std::sync::atomic::Ordering::SeqCst);
        Self {
            revision,
            text: "x".repeat(bytes),
            memory: memory.clone(),
        }
    }
}
impl Clone for Payload {
    fn clone(&self) -> Self {
        self.memory
            .clones
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.memory
            .bytes
            .fetch_add(self.text.len(), std::sync::atomic::Ordering::SeqCst);
        Self {
            revision: self.revision,
            text: self.text.clone(),
            memory: self.memory.clone(),
        }
    }
}
impl PartialEq for Payload {
    fn eq(&self, other: &Self) -> bool {
        self.revision == other.revision && self.text == other.text
    }
}
impl Drop for Payload {
    fn drop(&mut self) {
        self.memory
            .bytes
            .fetch_sub(self.text.len(), std::sync::atomic::Ordering::SeqCst);
    }
}

// Previous owned-queue algorithm, including the hub lock, deduplication,
// capacity/overflow behavior and moving the source snapshot into the cache.
struct OwnedHub(
    Mutex<(
        Snapshot<Payload>,
        Vec<(Sender<Snapshot<Payload>>, Receiver<Snapshot<Payload>>)>,
    )>,
);
impl OwnedHub {
    fn new(data: Payload) -> Self {
        Self(Mutex::new((
            Snapshot {
                kind: "snapshot".into(),
                rev: 1,
                data,
                resync: false,
            },
            Vec::new(),
        )))
    }
    fn subscribe(&self) -> Receiver<Snapshot<Payload>> {
        let mut state = self.0.lock().unwrap();
        let (tx, rx) = async_channel::bounded(SUBSCRIPTION_CAPACITY);
        tx.try_send(state.0.clone()).ok().unwrap();
        state.1.retain(|(tx, _)| !tx.is_closed());
        state.1.push((tx, rx.clone()));
        rx
    }
    fn publish(&self, data: Payload) {
        let mut state = self.0.lock().unwrap();
        if state.0.data == data {
            return;
        }
        let snapshot = Snapshot {
            kind: "snapshot".into(),
            rev: state.0.rev + 1,
            data,
            resync: false,
        };
        state.1.retain(|(tx, drain)| {
            if tx.is_closed() {
                return false;
            }
            match tx.try_send(snapshot.clone()) {
                Ok(()) => true,
                Err(TrySendError::Closed(_)) => false,
                Err(TrySendError::Full(mut snapshot)) => {
                    while drain.try_recv().is_ok() {}
                    snapshot.resync = true;
                    tx.try_send(snapshot).is_ok()
                }
            }
        });
        state.0 = snapshot;
    }
}

fn fanout(
    shared: bool,
    bytes: usize,
    listeners: usize,
) -> (std::time::Duration, std::time::Duration, usize, usize) {
    use std::{hint::black_box, sync::atomic::Ordering, time::Instant};
    let memory = Arc::new(Memory::default());
    let initial = Payload::new(0, bytes, &memory);
    let values = (1..=SUBSCRIPTION_CAPACITY)
        .map(|rev| Payload::new(rev, bytes, &memory))
        .collect::<Vec<_>>();
    let (publish, drain, retained, clones) = if shared {
        let hub = Hub::new(initial);
        let subscriptions = (0..listeners)
            .map(|_| {
                let subscription = hub.subscribe();
                drop(subscription.rx.try_recv().unwrap());
                subscription
            })
            .collect::<Vec<_>>();
        memory.clones.store(0, Ordering::SeqCst);
        let started = Instant::now();
        for value in values {
            hub.publish(value);
        }
        let publish = started.elapsed();
        let retained = memory.bytes.load(Ordering::SeqCst);
        let started = Instant::now();
        for subscription in &subscriptions {
            let latest = subscription.latest_shared(subscription.rx.try_recv().unwrap());
            assert_eq!(latest.data.revision, SUBSCRIPTION_CAPACITY);
            black_box(latest);
        }
        (
            publish,
            started.elapsed(),
            retained,
            memory.clones.load(Ordering::SeqCst),
        )
    } else {
        let hub = OwnedHub::new(initial);
        let subscriptions = (0..listeners)
            .map(|_| {
                let subscription = hub.subscribe();
                drop(subscription.try_recv().unwrap());
                subscription
            })
            .collect::<Vec<_>>();
        memory.clones.store(0, Ordering::SeqCst);
        let started = Instant::now();
        for value in values {
            hub.publish(value);
        }
        let publish = started.elapsed();
        let retained = memory.bytes.load(Ordering::SeqCst);
        let started = Instant::now();
        for subscription in &subscriptions {
            let mut latest = subscription.try_recv().unwrap();
            while let Ok(next) = subscription.try_recv() {
                let resync = latest.resync || next.resync;
                latest = next;
                latest.resync = resync;
            }
            assert_eq!(latest.data.revision, SUBSCRIPTION_CAPACITY);
            black_box(latest);
        }
        (
            publish,
            started.elapsed(),
            retained,
            memory.clones.load(Ordering::SeqCst),
        )
    };
    assert_eq!(
        memory.bytes.load(Ordering::SeqCst),
        0,
        "all payloads released"
    );
    (publish, drain, retained, clones)
}

#[test]
fn queued_listeners_share_each_payload_once_until_coalesced() {
    let shared = fanout(true, 1024, 4);
    let owned = fanout(false, 1024, 4);
    assert_eq!((shared.2, shared.3), (SUBSCRIPTION_CAPACITY * 1024, 0));
    assert_eq!(
        (owned.2, owned.3),
        (
            (SUBSCRIPTION_CAPACITY * 4 + 1) * 1024,
            SUBSCRIPTION_CAPACITY * 4
        )
    );
}

#[test]
#[ignore = "manual paired hub publication/coalescing and retained payload measurement"]
fn measure_shared_hub_fanout() {
    for (bytes, listeners) in [(4096, 4), (256 * 1024, 8)] {
        let mut runs = [Vec::new(), Vec::new()];
        for pass in 0..7 {
            for shared in if pass % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                runs[usize::from(shared)].push(fanout(shared, bytes, listeners));
            }
        }
        for (shared, values) in runs.iter_mut().enumerate() {
            values.sort_by_key(|run| run.0);
            let publish = values[3].0;
            values.sort_by_key(|run| run.1);
            let (_, drain, retained, clones) = values[3];
            eprintln!(
                "hub shared={shared}: {bytes} bytes, {listeners} listeners, 64 events: publish {publish:?}, coalesce {drain:?}, retained_payload_bytes {retained}, clones {clones}"
            );
        }
    }
}
