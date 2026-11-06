//! Actual QuickJS ChatHandle search dispatch with a paused worker. No provider,
//! sockets, filesystem home, platform backend, or native window is involved.
use nsh_core::{ApproxTextEngine, MountBatch, prelude::*};
use nsh_engine::{Bundle, Engine, EngineConfig, EngineControl, HostChannel, MountSink};
use riwork_engine::chat::{
    model::{ChatEvent, Item, ItemBody, ItemStatus},
    projection::DisplayMode,
    wire::Envelope,
};
use riwork_nsh_native::chat::{ChatBackend, ChatHandle, feed::FeedMsg, worker::WorkerMsg};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock, mpsc},
    time::Duration,
};

const WAIT: Duration = Duration::from_secs(20);
static HANDLE: Mutex<Option<ChatHandle>> = Mutex::new(None);

#[module]
struct Fixture;
#[module]
impl Fixture {
    pub fn open() -> ChatHandle {
        HANDLE.lock().unwrap().take().expect("one fixture handle")
    }
}
#[native_module("chat-search-fixture")]
fn fixture(registry: &mut Registry) {
    registry.module::<Fixture>();
}

struct NoIo;
impl ChatBackend for NoIo {
    fn home(&self) -> Result<PathBuf, String> {
        Err("fixture has no home".into())
    }
    fn existing(&self) -> Result<PathBuf, String> {
        Err("fixture has no socket".into())
    }
    fn ensure(&self) -> Result<PathBuf, String> {
        panic!("search must not start a host")
    }
}
struct Sink {
    control: Arc<OnceLock<EngineControl>>,
    events: mpsc::Sender<(String, Value)>,
}
impl MountSink for Sink {
    fn submit(&self, _: MountBatch) {}
    fn report_error(&self, message: &str) {
        let _ = self.events.send(("error".into(), json!(message)));
    }
    fn request_frame(&self) {
        self.control.get().unwrap().frame();
    }
}

#[test]
fn latest_search_and_cancel_cross_quickjs_without_cancelling_ordinary_search() {
    let declarations = nsh_core::element::declarations(&riwork_nsh_native::registry(), &[]);
    assert!(
        declarations.contains("searchLatest(query: string): Promise<(ChatSearchHit)[] | null>;")
    );
    assert!(declarations.contains("cancelSearch(): void;"));
    assert!(matches!(
        <ChatHandle as nsh_core::element::Class>::DEF
            .members
            .iter()
            .find(|member| member.name == "searchLatest")
            .unwrap()
            .call,
        nsh_core::element::MemberCall::Start(_)
    ));
    let chat = "11111111-2222-4333-8444-555555555555";
    let handle = ChatHandle::start(Arc::new(NoIo), chat.into(), true, Some(DisplayMode::Normal));
    handle.post(WorkerMsg::Feed(FeedMsg::Events(vec![Envelope {
        chat_id: chat.into(),
        seq: 1,
        event: ChatEvent::ItemCompleted {
            item: Item {
                id: "needle-item".into(),
                turn_id: None,
                status: ItemStatus::Completed,
                presentation: Default::default(),
                started_at: None,
                ended_at: None,
                body: ItemBody::AgentMessage {
                    text: "A searchable needle.".into(),
                },
            },
        },
    }])));
    let (blocked, entered) = mpsc::channel();
    let (release, released) = mpsc::channel();
    handle.post(WorkerMsg::Run(Box::new(move |_| {
        blocked.send(()).unwrap();
        // A failing assertion in the harness must not leak a parked worker.
        let _ = released.recv_timeout(WAIT);
    })));
    entered.recv_timeout(WAIT).unwrap();
    *HANDLE.lock().unwrap() = Some(handle);

    let host = HostChannel::new();
    let (events, receive) = mpsc::channel();
    for name in ["cancelled", "done", "error"] {
        let events = events.clone();
        host.handle(name, move |mut call| {
            let _ = events.send((call.name().to_owned(), call.take_data()));
        });
    }
    let source = r#"
        import { host } from 'nsh';
        import { Fixture } from 'chat-search-fixture';
        const handle = Fixture.open();
        async function run() {
            const ordinary = handle.search('needle');
            const first = handle.searchLatest('needle');
            if (typeof first.then !== 'function') throw new Error('searchLatest must return a Promise');
            const second = handle.searchLatest('needle');
            // First settling proves the second request has reached native code;
            // both remain behind the Rust worker barrier.
            const superseded = await first;
            handle.cancelSearch();
            const cancelled = await second;
            const sameTurn = handle.searchLatest('needle');
            handle.cancelSearch();
            const immediate = await sameTurn;
            host.post('cancelled', { superseded, cancelled, immediate });
            const ordinaryHits = await ordinary;
            const latestHits = await handle.searchLatest('needle');
            const empty = await handle.searchLatest('  ');
            handle.close();
            host.post('done', { ordinaryHits, latestHits, empty });
        }
        run().catch(error => host.post('error', String(error)));
    "#;
    let bundle = Bundle::source(source.into())
        .with_capabilities_json(Some(r#"{"host":["cancelled","done","error"]}"#))
        .unwrap();
    let mut config = EngineConfig::new(bundle);
    config.ui_sets = vec![
        nsh_base::registry(),
        riwork_nsh_native::registry(),
        registry(),
    ];
    config.host = host;
    let control = Arc::new(OnceLock::new());
    let setup = control.clone();
    let _engine = Engine::start_with(
        config,
        || ApproxTextEngine,
        Sink { control, events },
        move |control| {
            assert!(setup.set(control.clone()).is_ok());
        },
    )
    .unwrap();
    let (name, value) = receive.recv_timeout(WAIT).unwrap();
    assert_eq!(name, "cancelled", "{value}");
    assert_eq!(
        value,
        json!({"superseded":null,"cancelled":null,"immediate":null})
    );
    release.send(()).unwrap();
    let (name, value) = receive.recv_timeout(WAIT).unwrap();
    assert_eq!(name, "done", "{value}");
    assert_eq!(value["ordinaryHits"].as_array().unwrap().len(), 1);
    assert_eq!(value["ordinaryHits"][0]["id"], "needle-item");
    assert_eq!(value["latestHits"], value["ordinaryHits"]);
    assert_eq!(value["empty"], json!([]));
}
