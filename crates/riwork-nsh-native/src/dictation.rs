//! On-device dictation through the original riwork-speech helper. Creating a handle is inert;
//! only an explicit start opens the microphone. The original machine and subprocess protocol
//! live here without GPUI; the composer owns UTF-16 insertion into its native editor.
use std::{
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{Arc, Mutex, OnceLock, Weak, atomic::{AtomicBool, Ordering}},
};
use nsh_core::{element::Cleanup, prelude::*};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use crate::{hub::Hub, service, Subscription};

/// The shortcut that starts and stops dictation in a chat, as it is shown (main.rs binds it
/// as `ctrl-alt-d`).
pub const SHORTCUT_LABEL: &str = "⌃⌥D";
/// Give the helper's five-second finalisation deadline one second to reach the event pump.
pub const STOPPING_DEADLINE: std::time::Duration = std::time::Duration::from_secs(6);

/// Why dictation could not go on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    MicrophoneDenied,
    RecognitionDenied,
    /// No on-device recognizer for the language (speech is never sent to a server for this).
    Unsupported,
    /// The speech model is not on the Mac yet and could not be fetched.
    ModelUnavailable,
    /// The microphone could not be opened: another app holds it, or there is none.
    MicrophoneBusy(String),
    /// The microphone went away while listening.
    Interrupted,
    /// The helper is not beside the app (a build without Swift).
    Unavailable,
    Failed(String),
}

impl Problem {
    pub fn message(&self) -> String {
        match self {
            Self::MicrophoneDenied => {
                "Microphone access is off for RiWork. Turn it on in System Settings to dictate."
                    .to_owned()
            }
            Self::RecognitionDenied => {
                "Speech recognition is off for RiWork. Turn it on in System Settings to dictate."
                    .to_owned()
            }
            Self::Unsupported => {
                "On-device dictation is not available for this language on this Mac.".to_owned()
            }
            Self::ModelUnavailable => {
                "The speech model could not be installed. Check the connection and try again."
                    .to_owned()
            }
            Self::MicrophoneBusy(detail) if detail.is_empty() => {
                "The microphone could not be opened: another app may be using it.".to_owned()
            }
            Self::MicrophoneBusy(detail) => format!(
                "The microphone could not be opened ({detail}): another app may be using it."
            ),
            Self::Interrupted => {
                "Dictation stopped: the microphone went away. What was heard is kept.".to_owned()
            }
            Self::Unavailable => {
                "Dictation is not available in this build of RiWork (riwork-speech is missing)."
                    .to_owned()
            }
            Self::Failed(reason) => format!("Dictation stopped: {reason}"),
        }
    }

    /// The System Settings pane where the fix is, for a permission that is off.
    pub fn settings_url(&self) -> Option<&'static str> {
        match self {
            Self::MicrophoneDenied => {
                Some("x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone")
            }
            Self::RecognitionDenied => Some(
                "x-apple.systempreferences:com.apple.preference.security?Privacy_SpeechRecognition",
            ),
            _ => None,
        }
    }

    fn from_wire(code: &str, detail: String) -> Self {
        match code {
            "microphone-denied" => Self::MicrophoneDenied,
            "recognition-denied" => Self::RecognitionDenied,
            "unsupported" => Self::Unsupported,
            "model-unavailable" => Self::ModelUnavailable,
            "microphone-busy" => Self::MicrophoneBusy(detail),
            "interrupted" => Self::Interrupted,
            _ if detail.is_empty() => Self::Failed(code.to_owned()),
            _ => Self::Failed(detail),
        }
    }
}

/// Where one dictation is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Idle,
    /// The microphone and the speech model are being got ready. `note` says what takes long
    /// ("Downloading the speech model…").
    Preparing {
        note: Option<String>,
    },
    /// Listening; `text` is what has been heard so far, the last words of it still open to change.
    Listening {
        text: String,
    },
    /// Stopped by the person: the last words are being settled.
    Finishing {
        text: String,
    },
    Failed(Problem),
}

impl Phase {
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            Self::Preparing { .. } | Self::Listening { .. } | Self::Finishing { .. }
        )
    }

    pub fn text(&self) -> &str {
        match self {
            Self::Listening { text } | Self::Finishing { text } => text,
            _ => "",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Start,
    /// Getting ready takes a while for a reason worth saying.
    Note(String),
    /// The microphone is open.
    Ready,
    /// What has been heard so far.
    Heard(String),
    /// The person clicked stop or pressed the shortcut.
    Stop,
    /// The helper did not finish within the settling window. Keep its latest hypothesis.
    FinishTimedOut,
    /// The recognizer has finished; this is all of it.
    Finished(String),
    Cancel,
    Failed(Problem),
    /// The message about a failure was seen.
    Dismiss,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    None,
    BeginEngine,
    /// Stop listening and settle the text.
    FinishEngine,
    CancelEngine,
    /// The text for the field. Never empty.
    Deliver(String),
}

/// The rules of one dictation, apart from audio and recognizers so they can be tested: click to
/// start, click to stop, cancel at any time, and the text is handed over exactly once, when it
/// is final. The phone's `DictationMachine` (ios/Core/DictationMachine.swift), rule for rule.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Machine {
    phase: Phase,
}

impl Machine {
    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    pub fn handle(&mut self, event: Event) -> Effect {
        let active = self.phase.is_active();
        match (&self.phase, event) {
            (Phase::Idle | Phase::Failed(_), Event::Start) => {
                self.phase = Phase::Preparing { note: None };
                Effect::BeginEngine
            }
            (Phase::Preparing { .. }, Event::Note(note)) => {
                self.phase = Phase::Preparing { note: Some(note) };
                Effect::None
            }
            (Phase::Preparing { .. }, Event::Ready) => {
                self.phase = Phase::Listening {
                    text: String::new(),
                };
                Effect::None
            }
            (phase, Event::Heard(text)) if text.trim().is_empty() && !phase.text().is_empty() => {
                Effect::None
            }
            (Phase::Preparing { .. } | Phase::Listening { .. }, Event::Heard(text)) => {
                self.phase = Phase::Listening { text };
                Effect::None
            }
            (Phase::Finishing { .. }, Event::Heard(text)) => {
                self.phase = Phase::Finishing { text };
                Effect::None
            }
            // Nothing can have been heard yet: stopping is cancelling.
            (Phase::Preparing { .. }, Event::Stop) => {
                self.phase = Phase::Idle;
                Effect::CancelEngine
            }
            (Phase::Listening { text }, Event::Stop) => {
                self.phase = Phase::Finishing { text: text.clone() };
                Effect::FinishEngine
            }
            (Phase::Finishing { text }, Event::FinishTimedOut) => {
                let text = text.clone();
                self.handle(Event::Finished(text))
            }
            (phase, Event::Finished(text)) if active => {
                // A final with no words must not erase a volatile hypothesis already shown.
                let text = if text.trim().is_empty() {
                    phase.text().trim().to_owned()
                } else {
                    text.trim().to_owned()
                };
                self.phase = Phase::Idle;
                if text.is_empty() {
                    Effect::None
                } else {
                    Effect::Deliver(text)
                }
            }
            (_, Event::Cancel) if active => {
                self.phase = Phase::Idle;
                Effect::CancelEngine
            }
            // Every failure keeps what was said before it.
            (Phase::Listening { text } | Phase::Finishing { text }, Event::Failed(problem)) => {
                let kept = text.trim().to_owned();
                self.phase = Phase::Failed(problem);
                if kept.is_empty() {
                    Effect::CancelEngine
                } else {
                    Effect::Deliver(kept)
                }
            }
            (_, Event::Failed(problem)) if active => {
                self.phase = Phase::Failed(problem);
                Effect::CancelEngine
            }
            (Phase::Failed(_), Event::Dismiss) => {
                self.phase = Phase::Idle;
                Effect::None
            }
            _ => Effect::None,
        }
    }
}

/// Vocabulary is local to one recognition session, never persisted or sent to a service.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Sources {
    pub names: Vec<String>,
    pub paths: Vec<String>,
    pub text: String,
}

/// What the helper reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EngineEvent {
    Note(String),
    Ready,
    Heard(String),
    Finished(String),
    Failed(Problem),
}

impl EngineEvent {
    /// One line of the helper's output; `None` for one this app does not know.
    pub fn parse(line: &str) -> Option<Self> {
        #[derive(Deserialize)]
        struct Wire {
            event: String,
            #[serde(default)]
            text: String,
            #[serde(default)]
            problem: String,
            #[serde(default)]
            detail: String,
        }
        let wire: Wire = serde_json::from_str(line).ok()?;
        Some(match wire.event.as_str() {
            "note" => Self::Note(wire.text),
            "ready" => Self::Ready,
            "heard" => Self::Heard(wire.text),
            "finished" => Self::Finished(wire.text),
            "failed" => Self::Failed(Problem::from_wire(&wire.problem, wire.detail)),
            _ => return None,
        })
    }

    pub fn into_event(self) -> Event {
        match self {
            Self::Note(note) => Event::Note(note),
            Self::Ready => Event::Ready,
            Self::Heard(text) => Event::Heard(text),
            Self::Finished(text) => Event::Finished(text),
            Self::Failed(problem) => Event::Failed(problem),
        }
    }
}

static BUILT_HELPER: OnceLock<Option<PathBuf>> = OnceLock::new();

/// The desktop host owns the Swift build script. Pass its compile-time helper location
/// once at startup so an NSH development host can use that same helper outside a bundle.
pub fn configure_helper_path(path: Option<&str>) {
    let _ = BUILT_HELPER.set(path.map(PathBuf::from));
}

/// `riwork-speech`: beside the executable (in the bundle, Contents/MacOS), else where build.rs
/// left it. `RIWORK_SPEECH_HELPER` always overrides both installed and development helpers.
pub fn helper_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("RIWORK_SPEECH_HELPER") {
        return Some(PathBuf::from(path));
    }
    let executable = std::env::current_exe().ok()?;
    let beside = executable.parent()?;
    // A test binary is in target/<profile>/deps, one level below the helper.
    let candidates = [
        Some(beside.join("riwork-speech")),
        beside.parent().map(|parent| parent.join("riwork-speech")),
        BUILT_HELPER.get().cloned().flatten(),
        option_env!("RIWORK_SPEECH_HELPER_BUILT").map(PathBuf::from),
    ];
    candidates.into_iter().flatten().find(|path| path.is_file())
}

/// Words the helper "hears" instead of listening, for screenshots and tests without a
/// microphone (`RIWORK_SCRIPTED_DICTATION="words"`, debug builds only), like the phone's
/// `-RiWorkScriptedDictation`.
fn scripted() -> Option<String> {
    if cfg!(debug_assertions) {
        std::env::var("RIWORK_SCRIPTED_DICTATION")
            .ok()
            .filter(|words| !words.trim().is_empty())
    } else {
        None
    }
}

/// One dictation's helper process. Dropping it cancels the dictation and ends the process; the
/// helper also ends by itself when the app's end of its stdin closes, so it never outlives
/// RiWork.
pub struct Engine {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
}

impl Engine {
    /// Starts the helper for `sources`; its events arrive on the receiver, ending with
    /// `Finished` or `Failed`.
    pub fn start(
        sources: &Sources,
    ) -> Result<(Self, async_channel::Receiver<EngineEvent>), Problem> {
        Self::spawn(sources, scripted())
    }

    /// `start`, listening to the microphone or, with `script`, "hearing" those words.
    fn spawn(
        sources: &Sources,
        script: Option<String>,
    ) -> Result<(Self, async_channel::Receiver<EngineEvent>), Problem> {
        let helper = helper_path().ok_or(Problem::Unavailable)?;
        let mut command = Command::new(helper);
        match script {
            Some(words) => command.arg("scripted").arg(words),
            None => command.arg("listen"),
        };
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| Problem::Failed(format!("could not start riwork-speech: {error}")))?;
        let mut stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let line = serde_json::to_string(sources).unwrap_or_else(|_| "{}".to_owned());
        if let Some(stdin) = stdin.as_mut() {
            let _ = writeln!(stdin, "{line}");
        }
        let (sender, receiver) = async_channel::unbounded();
        if let Some(stdout) = stdout {
            if let Err(error) = std::thread::Builder::new()
                .name("riwork-speech".to_owned())
                .spawn(move || read_events(BufReader::new(stdout), &sender))
            {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Problem::Failed(format!("could not read riwork-speech: {error}")));
            }
        }
        Ok((
            Self {
                child: Some(child),
                stdin,
            },
            receiver,
        ))
    }

    /// Stop listening; the helper settles the last words and reports `Finished`.
    pub fn finish(&mut self) {
        if let Some(stdin) = self.stdin.as_mut() {
            let _ = writeln!(stdin, "finish");
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if let Some(mut stdin) = self.stdin.take() {
            let _ = writeln!(stdin, "cancel");
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            // Reap it off the main thread.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }
}

/// Hands the helper's events over until it ends. A helper that ends without its last word
/// (it crashed, or was killed) is a failure.
fn read_events(reader: impl BufRead, sender: &async_channel::Sender<EngineEvent>) {
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let Some(event) = EngineEvent::parse(&line) else {
            continue;
        };
        let last = matches!(event, EngineEvent::Finished(_) | EngineEvent::Failed(_));
        if sender.send_blocking(event).is_err() || last {
            return;
        }
    }
    let _ = sender.send_blocking(EngineEvent::Failed(Problem::Failed(
        "the speech helper stopped".to_owned(),
    )));
}

#[derive(Default)]
struct State {
    machine: Machine,
    engine: Option<Engine>,
    session: u64,
    // Retained in snapshots until the next start so coalescing cannot lose the final edit.
    delivery: Option<Value>,
}
impl State {
    fn snapshot(&self) -> Value {
        let (phase, note, problem, settings_url) = match self.machine.phase() {
            Phase::Idle => ("idle", None, None, None),
            Phase::Preparing { note } => ("preparing", note.clone(), None, None),
            Phase::Listening { .. } => ("listening", None, None, None),
            Phase::Finishing { .. } => ("finishing", None, None, None),
            Phase::Failed(problem) => ("failed", None, Some(problem.message()), problem.settings_url()),
        };
        json!({"session": self.session, "phase": phase, "text": self.machine.phase().text(),
            "note": note, "problem": problem, "settingsUrl": settings_url, "delivery": self.delivery})
    }
    fn handle(&mut self, event: Event) -> Effect {
        let was_active = self.machine.phase().is_active();
        let effect = self.machine.handle(event);
        match &effect {
            Effect::Deliver(text) => self.delivery = Some(json!({"kind": "commit", "text": text})),
            Effect::CancelEngine => self.delivery = Some(json!({"kind": "discard"})),
            _ if was_active && !self.machine.phase().is_active() => {
                self.delivery = Some(json!({"kind": "discard"}));
            }
            _ => {}
        }
        if was_active && !self.machine.phase().is_active() {
            self.engine = None;
        }
        effect
    }
}

struct Owner {
    state: Mutex<State>,
    topic: Hub<Value>,
    closed: AtomicBool,
}
static ACTIVE: OnceLock<Mutex<Weak<Owner>>> = OnceLock::new();

impl Owner {
    fn new() -> Arc<Self> {
        let state = State::default();
        Arc::new(Self {
            topic: Hub::new(state.snapshot()), state: Mutex::new(state), closed: AtomicBool::new(false),
        })
    }
    fn start(self: &Arc<Self>, sources: Sources) -> Result<(), String> {
        // Keep launches ordered across every window: one recognizer owns the microphone.
        let mut active = ACTIVE.get_or_init(|| Mutex::new(Weak::new())).lock().unwrap();
        if self.closed.load(Ordering::Acquire) { return Err("Dictation is closed.".into()); }
        if let Some(other) = active.upgrade().filter(|other| !Arc::ptr_eq(other, self)) {
            other.event(Event::Cancel, None);
        }
        let mut state = self.state.lock().unwrap();
        if state.machine.phase().is_active() { return Ok(()); }
        state.session += 1;
        let session = state.session;
        state.delivery = None;
        state.handle(Event::Start);
        self.topic.publish(state.snapshot());
        *active = Arc::downgrade(self);
        match Engine::start(&sources) {
            Ok((engine, events)) => {
                if self.closed.load(Ordering::Acquire) {
                    state.handle(Event::Cancel);
                    self.topic.publish(state.snapshot());
                    return Ok(());
                }
                state.engine = Some(engine);
                let owner = Arc::downgrade(self);
                let pump = std::thread::Builder::new().name("nsh-dictation".into()).spawn(move || {
                    while let Ok(event) = events.recv_blocking() {
                        let Some(owner) = owner.upgrade() else { return };
                        if !owner.event(event.into_event(), Some(session)) { return; }
                    }
                });
                if let Err(error) = pump {
                    state.handle(Event::Failed(Problem::Failed(error.to_string())));
                    self.topic.publish(state.snapshot());
                }
            }
            Err(problem) => {
                state.handle(Event::Failed(problem));
                self.topic.publish(state.snapshot());
            }
        }
        Ok(())
    }
    fn event(self: &Arc<Self>, event: Event, session: Option<u64>) -> bool {
        if self.closed.load(Ordering::Acquire) { return false; }
        let mut state = self.state.lock().unwrap();
        if session.is_some_and(|session| session != state.session || !state.machine.phase().is_active()) {
            return false;
        }
        if matches!(event, Event::FinishTimedOut) && !matches!(state.machine.phase(), Phase::Finishing { .. }) {
            return state.machine.phase().is_active();
        }
        let effect = state.handle(event);
        if effect == Effect::FinishEngine {
            if let Some(engine) = state.engine.as_mut() { engine.finish(); }
            let owner = Arc::downgrade(self);
            let session = state.session;
            // The helper normally finishes first; the deadline preserves its last hypothesis.
            let _ = std::thread::Builder::new().name("nsh-dictation-finish".into()).spawn(move || {
                std::thread::sleep(STOPPING_DEADLINE);
                if let Some(owner) = owner.upgrade() { owner.event(Event::FinishTimedOut, Some(session)); }
            });
        }
        self.topic.publish(state.snapshot());
        state.machine.phase().is_active()
    }
    fn close(self: &Arc<Self>) {
        self.closed.store(true, Ordering::Release);
        // Teardown never waits for a helper launch. A launch checks closed before retaining it.
        if let Ok(mut state) = self.state.try_lock() {
            state.engine = None;
            state.handle(Event::Cancel);
        } else {
            let owner = self.clone();
            let _ = std::thread::Builder::new().name("nsh-dictation-close".into()).spawn(move || {
                let mut state = owner.state.lock().unwrap();
                state.engine = None;
                state.handle(Event::Cancel);
            });
        }
    }
}

#[class]
pub struct DictationHandle {
    owner: Arc<Owner>,
    cleanup: Option<Cleanup>,
}
impl Drop for DictationHandle {
    fn drop(&mut self) { self.owner.close(); }
}
#[class]
impl DictationHandle {
    pub fn subscribe(this: Instance<Self>, listener: Callback<Value>, cx: CallCx<'_>) -> Subscription {
        let owner = this.with(|handle| handle.owner.clone());
        Subscription::start(cx, async move {
            if let Ok(subscription) = service::blocking(move || Ok(owner.topic.subscribe())).await {
                while let Ok(snapshot) = subscription.recv().await {
                    listener.post(subscription.latest(snapshot).data);
                    next_frame().await;
                }
            }
        })
    }
    pub async fn start(this: Instance<Self>, sources: Value) -> Result<(), String> {
        let owner = this.with(|handle| handle.owner.clone());
        if !crate::settings::Settings::get().await?.dictation_mic {
            return Err("Show microphone buttons for dictation in Settings to dictate.".into());
        }
        let sources: Sources = serde_json::from_value(sources).map_err(|error| format!("invalid: {error}"))?;
        service::blocking(move || owner.start(sources)).await
    }
    pub async fn stop(this: Instance<Self>) -> Result<(), String> {
        let owner = this.with(|handle| handle.owner.clone());
        service::blocking(move || { owner.event(Event::Stop, None); Ok(()) }).await
    }
    pub async fn cancel(this: Instance<Self>) -> Result<(), String> {
        let owner = this.with(|handle| handle.owner.clone());
        service::blocking(move || { owner.event(Event::Cancel, None); Ok(()) }).await
    }
    pub async fn dismiss(this: Instance<Self>) -> Result<(), String> {
        let owner = this.with(|handle| handle.owner.clone());
        service::blocking(move || { owner.event(Event::Dismiss, None); Ok(()) }).await
    }
    pub fn close(&mut self) {
        self.owner.close();
        if let Some(cleanup) = self.cleanup.take() { cleanup.release(); }
    }
}

#[module]
pub struct Dictation;
#[module]
impl Dictation {
    pub fn open(cx: CallCx<'_>) -> DictationHandle {
        let owner = Owner::new();
        let cleanup_owner = owner.clone();
        let cleanup = cx.on_cleanup(move || cleanup_owner.close());
        DictationHandle { owner, cleanup: Some(cleanup) }
    }
}

