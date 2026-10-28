//! Real QuickJS/native-component-event/keymap wiring, with a fake mount sink.
//! No native UI, preview process, Cua call or platform event is instantiated.
use nsh_core::{
    ApproxTextEngine, EventKind, EventMask, EventPayload, MountBatch, MountItem, NodeId,
    PayloadValue, Points, ROOT_ID, Size,
    keymap::{KeyMatch, Keymap, Keystroke},
    platform::Os,
};
use nsh_engine::{Bundle, Engine, EngineConfig, EngineControl, HostChannel};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock, mpsc},
    time::Duration,
};

#[derive(Default)]
struct Observed {
    names: HashMap<NodeId, String>,
    masks: HashMap<NodeId, EventMask>,
    parents: HashMap<NodeId, NodeId>,
    keymap: Arc<Keymap>,
    snapshot: Value,
}
impl Observed {
    fn attached(&self, mut id: NodeId) -> bool {
        while id != ROOT_ID {
            let Some(parent) = self.parents.get(&id) else {
                return false;
            };
            id = *parent;
        }
        true
    }
    fn named(&self, name: &str) -> Vec<NodeId> {
        self.names
            .iter()
            .filter(|(id, value)| value.as_str() == name && self.attached(**id))
            .map(|(id, _)| *id)
            .collect()
    }
}
struct Sink {
    control: Arc<OnceLock<EngineControl>>,
    observed: Arc<Mutex<Observed>>,
    events: mpsc::Sender<String>,
}
impl nsh_engine::MountSink for Sink {
    fn submit(&self, batch: MountBatch) {
        let mut seen = self.observed.lock().unwrap();
        for item in batch.items {
            match item {
                MountItem::ViewProps { id, props } => {
                    seen.names.remove(&id);
                    if let Some(name) = props.test_id {
                        seen.names.insert(id, name.to_string());
                    }
                }
                MountItem::Insert { parent, child, .. } => {
                    seen.parents.insert(child, parent);
                }
                MountItem::Remove { id } => {
                    seen.parents.remove(&id);
                }
                MountItem::Delete { id } => {
                    seen.names.remove(&id);
                    seen.parents.remove(&id);
                    seen.masks.remove(&id);
                }
                MountItem::Events { id, events } => {
                    seen.masks.insert(id, events);
                }
                MountItem::Keymap(keymap) => seen.keymap = keymap,
                _ => {}
            }
        }
    }
    fn request_frame(&self) {
        self.control.get().unwrap().frame();
    }
    fn report_error(&self, message: &str) {
        let _ = self.events.send(format!("error: {message}"));
    }
}
struct App {
    _engine: Engine,
    control: Arc<OnceLock<EngineControl>>,
    host: HostChannel,
    observed: Arc<Mutex<Observed>>,
    events: mpsc::Receiver<String>,
}
impl App {
    fn start() -> Self {
        let path =
            PathBuf::from(std::env::var_os("RIWORK_PREVIEW_FOCUS_BUNDLE").expect("fixture bundle"));
        let host = HostChannel::new();
        let observed = Arc::new(Mutex::new(Observed::default()));
        let state = observed.clone();
        host.handle("app.fixture.state", move |call| {
            state.lock().unwrap().snapshot = call.data().clone();
            call.resolve(json!({}));
        });
        let (events, receive) = mpsc::channel();
        let settled = events.clone();
        host.handle("app.fixture.settled", move |call| {
            let _ = settled.send("settled".into());
            call.resolve(json!({}));
        });
        for name in [
            "window.setTitle",
            "window.setDockInfo",
            "window.setSize",
            "window.open",
        ] {
            host.handle(name, |call| call.resolve(json!({})));
        }
        let mut config = EngineConfig::new(Bundle::read(&path).unwrap());
        config.viewport = Size::new(Points(1280.0), Points(850.0));
        config.environment.os = Os::MacOs;
        config.ui_sets = vec![nsh_base::registry(), riwork_nsh_native::registry()];
        config.host = host.clone();
        let control = Arc::new(OnceLock::new());
        let setup = control.clone();
        let engine = Engine::start_with(
            config,
            || ApproxTextEngine,
            Sink {
                control: control.clone(),
                observed: observed.clone(),
                events,
            },
            move |value| {
                assert!(setup.set(value.clone()).is_ok());
            },
        )
        .unwrap();
        let app = Self {
            _engine: engine,
            control,
            host,
            observed,
            events: receive,
        };
        for _ in 0..50 {
            app.settle();
            if app.observed.lock().unwrap().named("preview-frame").len() == 1 {
                return app;
            }
        }
        panic!("preview fixture did not mount");
    }
    fn settle(&self) {
        self.host.send("app.fixture.settle", json!({})).unwrap();
        loop {
            let event = self
                .events
                .recv_timeout(Duration::from_secs(10))
                .expect("fixture settled");
            assert!(!event.starts_with("error:"), "{event}");
            if event == "settled" {
                break;
            }
        }
    }
    fn send(&self, name: &str, value: Value) {
        self.host.send(name, value).unwrap();
        self.settle();
    }
    fn native_focus(&self, focused: bool) {
        let seen = self.observed.lock().unwrap();
        let ids = seen.named("preview-frame");
        assert_eq!(ids.len(), 1, "one attached native preview");
        let id = ids[0];
        assert!(seen.masks[&id].contains(EventKind::NativeEvent));
        drop(seen);
        self.control.get().unwrap().dispatch_event(
            id,
            EventKind::NativeEvent,
            EventPayload::Native {
                name: "focusChange".into(),
                data: json!({"focused":focused}),
            },
        );
        self.settle();
    }
    fn binding(&self, key: &str) -> KeyMatch {
        self.observed.lock().unwrap().keymap.find(
            Keystroke::parse(key, Os::MacOs).unwrap(),
            true,
            false,
        )
    }
    fn key(&self, key: &str) {
        let KeyMatch::Fire(id) = self.binding(key) else {
            panic!("expected a host binding for {key}")
        };
        self.control.get().unwrap().dispatch_event(
            ROOT_ID,
            EventKind::KeyCommand,
            EventPayload::Value(PayloadValue::Number(id as f64)),
        );
        self.settle();
    }
    fn escape_is(&self, intercepted: bool, label: &str) {
        let result = self.binding("escape");
        eprintln!("PREVIEW_FOCUS {label}: Escape {result:?}");
        assert_eq!(result.consumed(), intercepted, "{label}");
    }
}

#[test]
#[ignore = "requires private fixture HOME/RIWORK_HOME and RIWORK_PREVIEW_FOCUS_BUNDLE"]
fn native_preview_focus_routes_pane_and_releases_escape_without_losing_global_keys() {
    let app = App::start();
    assert_eq!(app.observed.lock().unwrap().snapshot["activePane"], 2);
    app.escape_is(true, "before native focus");
    app.native_focus(true);
    assert_eq!(
        app.observed.lock().unwrap().snapshot["activePane"],
        1,
        "native focus selects owning pane"
    );
    app.escape_is(
        false,
        "native focus true traverses component, tab, workspace",
    );
    for key in ["ctrl+tab", "ctrl+shift+tab", "mod+shift+]", "mod+shift+["] {
        assert!(
            app.binding(key).consumed(),
            "global tab key retained: {key}"
        );
    }
    app.native_focus(false);
    app.escape_is(true, "explicit native blur");
    app.native_focus(true);
    app.send("window.state", json!({"visible":false, "appActive":false}));
    app.escape_is(true, "window hiding clears native focus");
    app.send("window.state", json!({"visible":true, "appActive":true}));
    app.escape_is(true, "window showing does not invent focus");
    app.native_focus(true);
    app.key("ctrl+tab");
    app.escape_is(true, "hidden tab clears native focus");
    app.key("ctrl+shift+tab");
    app.escape_is(true, "reshown tab does not invent focus");
    app.native_focus(true);
    app.send("command", json!({"id":"openVim"}));
    app.escape_is(true, "modal regains Escape");
    app.key("escape");
    app.escape_is(false, "modal dismiss restores preview Escape");
    app.native_focus(false);
    app.escape_is(true, "blur after modal");
    app.native_focus(true);
    app.send("command", json!({"id":"closeTab"}));
    for _ in 0..20 {
        if app
            .observed
            .lock()
            .unwrap()
            .named("preview-frame")
            .is_empty()
        {
            break;
        }
        app.settle();
    }
    assert!(
        app.observed
            .lock()
            .unwrap()
            .named("preview-frame")
            .is_empty()
    );
    app.escape_is(true, "unmount releases preview Escape");
    eprintln!(
        "PREVIEW_FOCUS PASS: native event delivery, pane selection, blur/hide/unmount, modal and four global tab keys"
    );
}
