//! Production TSX -> QuickJS -> native mount commands, with a fake platform.
//! No window, Cua driver, account discovery or Keychain. The stock NSH 0.3 JS
//! test driver ignores Focus commands for pressables, so inspect those commands
//! here and deliver only synthetic native focus/blur events for containment.
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock, mpsc},
    time::Duration,
};

use nsh_core::{
    ApproxTextEngine, Command, EventKind, EventMask, EventPayload, MountBatch, MountItem, NodeId,
    ROOT_ID,
};
use nsh_engine::{Bundle, Engine, EngineConfig, EngineControl, HostChannel};
use serde_json::json;

#[derive(Default)]
struct Observed {
    names: HashMap<NodeId, String>,
    masks: HashMap<NodeId, EventMask>,
    parents: HashMap<NodeId, NodeId>,
    focus: Vec<String>,
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
    fn named(&self, name: &str) -> NodeId {
        let ids: Vec<_> = self
            .names
            .iter()
            .filter(|(id, value)| value.as_str() == name && self.attached(**id))
            .map(|(id, _)| *id)
            .collect();
        assert_eq!(ids.len(), 1, "one attached node named {name}: {ids:?}");
        ids[0]
    }
}
struct Sink {
    control: Arc<OnceLock<EngineControl>>,
    observed: Arc<Mutex<Observed>>,
    events: mpsc::Sender<String>,
}
impl nsh_engine::MountSink for Sink {
    fn submit(&self, batch: MountBatch) {
        let mut observed = self.observed.lock().unwrap();
        for item in batch.items {
            match item {
                MountItem::ViewProps { id, props } => {
                    observed.names.remove(&id);
                    if let Some(name) = props.test_id {
                        observed.names.insert(id, name.to_string());
                    }
                }
                MountItem::Insert { parent, child, .. } => {
                    observed.parents.insert(child, parent);
                }
                MountItem::Remove { id } => {
                    observed.parents.remove(&id);
                }
                MountItem::Delete { id } => {
                    observed.names.remove(&id);
                    observed.masks.remove(&id);
                    observed.parents.remove(&id);
                }
                MountItem::Events { id, events } => {
                    observed.masks.insert(id, events);
                }
                MountItem::Command {
                    id,
                    command: Command::Focus,
                } => {
                    let name = observed
                        .names
                        .get(&id)
                        .cloned()
                        .unwrap_or_else(|| format!("node:{id}"));
                    observed.focus.push(name);
                }
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
    fn start(ready: bool, pending: bool) -> Self {
        let bundle = PathBuf::from(
            std::env::var_os("RIWORK_SETTINGS_FOCUS_BUNDLE").expect("fixture bundle"),
        );
        let host = HostChannel::new();
        host.set_params(
            json!({"ready": ready, "pending": pending})
                .as_object()
                .unwrap()
                .clone(),
        );
        let (events, receive) = mpsc::channel();
        for name in [
            "app.fixture.settled",
            "app.fixture.state",
            "app.fixture.cua-action",
        ] {
            let events = events.clone();
            host.handle(name, move |call| {
                let _ = events.send(call.name().into());
                call.resolve(json!({}));
            });
        }
        let mut config = EngineConfig::new(Bundle::read(&bundle).unwrap());
        config.ui_sets = vec![nsh_base::registry(), riwork_nsh_native::registry()];
        config.host = host.clone();
        let control = Arc::new(OnceLock::new());
        let setup = control.clone();
        let observed = Arc::new(Mutex::new(Observed::default()));
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
            if app
                .observed
                .lock()
                .unwrap()
                .names
                .values()
                .any(|name| name == "theme-choice:native")
            {
                break;
            }
        }
        assert!(
            app.observed
                .lock()
                .unwrap()
                .names
                .values()
                .any(|name| name == "theme-choice:native")
        );
        app
    }
    fn settle(&self) {
        self.host.send("app.fixture.settle", json!({})).unwrap();
        loop {
            let event = self
                .events
                .recv_timeout(Duration::from_secs(10))
                .expect("fixture settled");
            assert!(!event.starts_with("error:"), "{event}");
            if event == "app.fixture.settled" {
                break;
            }
        }
    }
    fn focus_event(&self, name: &str, kind: EventKind) {
        let id = self.observed.lock().unwrap().named(name);
        assert!(
            self.observed.lock().unwrap().masks[&id].contains(kind),
            "{name} listens for {kind:?}"
        );
        self.control
            .get()
            .unwrap()
            .dispatch_event(id, kind, EventPayload::None);
        self.settle();
    }
    fn send(&self, event: &str, value: serde_json::Value) -> Vec<String> {
        self.observed.lock().unwrap().focus.clear();
        self.host.send(event, value).unwrap();
        self.settle();
        self.observed.lock().unwrap().focus.clone()
    }
    fn show(&self) -> Vec<String> {
        self.send("app.fixture.show", json!(true))
    }
}

#[test]
#[ignore = "requires the isolated Phase 6c focus runner and compiled TSX fixture"]
fn combined_settings_cua_focus_commands() {
    let mut failures = Vec::new();
    for ready in [false, true] {
        for inside in [
            None,
            Some("claude-api-key-input"),
            Some("theme-choice:native"),
            Some("cua-primary"),
            Some("cua-check"),
            Some("panel-tab-icons"),
            Some("interface-font-system"),
            Some("delegation-parallel-shells"),
            Some("delegation-parallel-shells-increase"),
            Some("orchestrator-other-model"),
        ] {
            let app = App::start(ready, false);
            if let Some(name) = inside {
                app.focus_event(name, EventKind::Focus);
            }
            let actual = app.show();
            let expected: Vec<String> = if inside.is_some() {
                vec![]
            } else {
                vec![
                    if ready {
                        "theme-choice:native"
                    } else {
                        "cua-primary"
                    }
                    .into(),
                ]
            };
            println!(
                "ready={ready} focus_before={inside:?}: expected={expected:?} actual={actual:?}"
            );
            if actual != expected {
                failures.push((ready, inside, actual, expected));
            }
        }
    }
    assert!(failures.is_empty(), "focus regressions: {failures:?}");
}

#[test]
#[ignore = "requires the isolated Phase 6c focus runner and compiled TSX fixture"]
fn delayed_checks_and_focus_leaving_the_panel() {
    for ready in [false, true] {
        let expected = vec![
            if ready {
                "theme-choice:native"
            } else {
                "cua-primary"
            }
            .to_owned(),
        ];
        for inside in [
            None,
            Some("claude-api-key-input"),
            Some("cua-check"),
            Some("panel-tab-icons"),
        ] {
            let app = App::start(ready, true);
            // Focus may enter while the asynchronous Cua check is outstanding.
            assert!(app.show().is_empty());
            if let Some(name) = inside {
                app.focus_event(name, EventKind::Focus);
            }
            let actual = app.send("app.fixture.cua-ready", json!(ready));
            let wanted = if inside.is_some() {
                vec![]
            } else {
                expected.clone()
            };
            println!(
                "delayed ready={ready} focus_before={inside:?}: expected={wanted:?} actual={actual:?}"
            );
            assert_eq!(actual, wanted);
            assert!(
                app.show().is_empty(),
                "same visibility must not focus twice"
            );
            assert!(
                app.send("app.fixture.cua-ready", json!(!ready)).is_empty(),
                "background status must not steal focus"
            );
        }
        for name in ["claude-api-key-input", "cua-check", "panel-tab-icons"] {
            let app = App::start(ready, false);
            app.focus_event(name, EventKind::Focus);
            assert!(app.show().is_empty());
            app.focus_event(name, EventKind::Blur);
            assert!(app.send("app.fixture.show", json!(false)).is_empty());
            let actual = app.show();
            println!(
                "blur/reopen ready={ready} previous={name}: expected={expected:?} actual={actual:?}"
            );
            assert_eq!(actual, expected);
        }
    }
}
