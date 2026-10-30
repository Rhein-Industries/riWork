//! On-device dictation through the original riwork-speech helper. Creating a handle is inert;
//! only an explicit start opens the microphone. The original machine and subprocess protocol
//! live here without GPUI; the composer owns UTF-16 insertion into its native editor.
//!
//! With a local MLX model chosen (`dictation_model`), the helper only captures the microphone
//! (`riwork-speech capture`) and the speech worker (speech.rs) does the recognizing; both feed
//! the same events, so the machine and the composer do not know which engine listened.
use std::{
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{Arc, Mutex, OnceLock, Weak, atomic::{AtomicBool, Ordering}},
    time::Duration,
};
use nsh_core::{element::Cleanup, prelude::*};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use crate::{hub::Hub, service, speech, Subscription};

/// The shortcut that starts and stops dictation in a chat, as it is shown (main.rs binds it
/// as `ctrl-alt-d`).
pub const SHORTCUT_LABEL: &str = "⌃⌥D";
/// Give the helper's five-second finalisation deadline one second to reach the event pump.
pub const STOPPING_DEADLINE: Duration = Duration::from_secs(6);

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
    /// The chosen local speech model (named) is not downloaded, or its runtime is not set up.
    ModelMissing(String),
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
            Self::ModelMissing(label) => format!(
                "{label} is not on this Mac yet. Download it in Settings → Agent sessions, or choose another dictation model there."
            ),
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

/// A recording the capture helper plays instead of the microphone, for a local model's whole
/// path in tests (`RIWORK_DICTATION_RECORDING=/path.wav`, debug builds only).
fn recording() -> Option<String> {
    if cfg!(debug_assertions) {
        std::env::var("RIWORK_DICTATION_RECORDING").ok().filter(|path| !path.trim().is_empty())
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
    /// With a local model: its dictation in the speech worker, fed by the capturing helper. The
    /// engine keeps it after the helper is done, until the worker's final words are in.
    session: Option<Arc<speech::ModelSession>>,
    deadline: Duration,
}

impl Engine {
    /// Starts dictation for `sources` with the model `model` names (Apple's speech for a key this
    /// build does not know); its events arrive on the receiver, ending with `Finished` or `Failed`.
    pub fn start(
        sources: &Sources,
        model: &str,
    ) -> Result<(Self, async_channel::Receiver<EngineEvent>), Problem> {
        let spec = speech::effective(model);
        if spec.is_apple() {
            Self::spawn(sources, scripted())
        } else {
            Self::spawn_model(spec)
        }
    }

    /// How long a stopped dictation may take to settle its last words.
    pub fn finish_deadline(&self) -> Duration {
        self.deadline
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
                session: None,
                deadline: STOPPING_DEADLINE,
            },
            receiver,
        ))
    }

    /// A local model: the helper captures the microphone and the speech worker recognizes.
    /// It is ready as soon as the microphone is open: the worker keeps what is said while its
    /// model loads, so push-to-talk can talk at once. Its "Loading …" note shows meanwhile.
    fn spawn_model(
        spec: &'static speech::ModelSpec,
    ) -> Result<(Self, async_channel::Receiver<EngineEvent>), Problem> {
        // Before the model: a build without the helper must not load one for nothing.
        let helper = helper_path().ok_or(Problem::Unavailable)?;
        let (session, worker_events) = speech::begin(spec).map_err(|error| match error {
            speech::NotReady::Missing => Problem::ModelMissing(spec.label.clone()),
            speech::NotReady::Failed(reason) => Problem::Failed(reason),
        })?;
        let session = Arc::new(session);
        let mut command = Command::new(helper);
        match recording() {
            Some(path) => command.arg("capture-file").arg(path),
            None => command.arg("capture"),
        };
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| Problem::Failed(format!("could not start riwork-speech: {error}")))?;
        let mut stdin = child.stdin.take();
        let stdout = child.stdout.take();
        if let Some(stdin) = stdin.as_mut() {
            let _ = writeln!(stdin, "{{}}");
        }
        let (sender, receiver) = async_channel::unbounded();
        let gate = Arc::new(Mutex::new(Gate::default()));
        let threads = [
            stdout.map(|stdout| {
                let (session, sender, gate) = (session.clone(), sender.clone(), gate.clone());
                std::thread::Builder::new()
                    .name("riwork-speech-capture".to_owned())
                    .spawn(move || relay_capture(BufReader::new(stdout), &session, &sender, &gate))
            }),
            Some(std::thread::Builder::new().name("riwork-speech-model".to_owned()).spawn({
                let (sender, gate) = (sender.clone(), gate.clone());
                move || relay_model(&worker_events, &sender, &gate, &spec.label)
            })),
        ];
        if let Some(Err(error)) = threads.into_iter().flatten().find(Result::is_err) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Problem::Failed(format!("could not read riwork-speech: {error}")));
        }
        Ok((
            Self {
                child: Some(child),
                stdin,
                session: Some(session),
                deadline: spec.finish_deadline(),
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
        // Unless it finished, the worker drops this dictation too.
        self.session.take();
    }
}

/// A local model's dictation is ready once the microphone is open. Until its model is loaded,
/// the worker's note stays up, into listening too; an empty note takes it down.
#[derive(Default)]
struct Gate {
    microphone: bool,
    loaded: bool,
    loading: Option<String>,
}

impl Gate {
    fn microphone(&mut self, sender: &async_channel::Sender<EngineEvent>) {
        if self.microphone {
            return;
        }
        self.microphone = true;
        let _ = sender.send_blocking(EngineEvent::Ready);
        if let Some(note) = self.loading.clone() {
            let _ = sender.send_blocking(EngineEvent::Note(note));
        }
    }
    fn note(&mut self, note: String, sender: &async_channel::Sender<EngineEvent>) {
        if !self.loaded {
            self.loading = Some(note.clone());
        }
        let _ = sender.send_blocking(EngineEvent::Note(note));
    }
    fn loaded(&mut self, sender: &async_channel::Sender<EngineEvent>) {
        self.loaded = true;
        if self.loading.take().is_some() && self.microphone {
            let _ = sender.send_blocking(EngineEvent::Note(String::new()));
        }
    }
}

/// The capturing helper's output: audio goes to the worker; its `finished` means the audio has
/// ended, so the worker settles the last words. Problems with the microphone end the dictation.
fn relay_capture(
    reader: impl BufRead,
    session: &speech::ModelSession,
    sender: &async_channel::Sender<EngineEvent>,
    gate: &Mutex<Gate>,
) {
    #[derive(Deserialize)]
    struct Wire {
        event: String,
        #[serde(default)]
        data: String,
    }
    for line in reader.lines() {
        let Ok(line) = line else { break };
        // Audio is most of the traffic: hand its base64 over without decoding it.
        if let Ok(wire) = serde_json::from_str::<Wire>(&line) {
            match wire.event.as_str() {
                "audio" => { session.audio(&wire.data); continue; }
                "ready" => { gate.lock().unwrap().microphone(sender); continue; }
                "finished" => { session.finish(); return; }
                _ => {}
            }
        }
        match EngineEvent::parse(&line) {
            Some(EngineEvent::Note(note)) => { let _ = sender.send_blocking(EngineEvent::Note(note)); }
            Some(EngineEvent::Failed(problem)) => { let _ = sender.send_blocking(EngineEvent::Failed(problem)); return; }
            _ => {}
        }
    }
    let _ = sender.send_blocking(EngineEvent::Failed(Problem::Failed(
        "the speech helper stopped".to_owned(),
    )));
}

/// The worker's events for this dictation, as the machine's. `label` names the model for a
/// problem with its files.
fn relay_model(
    events: &async_channel::Receiver<speech::WorkerEvent>,
    sender: &async_channel::Sender<EngineEvent>,
    gate: &Mutex<Gate>,
    label: &str,
) {
    while let Ok(event) = events.recv_blocking() {
        let event = match event {
            speech::WorkerEvent::Note(note) => { gate.lock().unwrap().note(note, sender); continue; }
            speech::WorkerEvent::Loaded => { gate.lock().unwrap().loaded(sender); continue; }
            speech::WorkerEvent::Heard(text) => EngineEvent::Heard(text),
            speech::WorkerEvent::Finished(text) => EngineEvent::Finished(text),
            speech::WorkerEvent::Failed { problem, .. } if problem == "model-unavailable" => {
                EngineEvent::Failed(Problem::ModelMissing(label.to_owned()))
            }
            speech::WorkerEvent::Failed { problem, detail } => {
                EngineEvent::Failed(Problem::Failed(if detail.is_empty() { problem } else { detail }))
            }
        };
        let last = matches!(event, EngineEvent::Finished(_) | EngineEvent::Failed(_));
        if sender.send_blocking(event).is_err() || last {
            return;
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
    /// What the engine says while listening ("Loading …: keep talking").
    note: Option<String>,
    // Retained in snapshots until the next start so coalescing cannot lose the final edit.
    delivery: Option<Value>,
}
impl State {
    fn snapshot(&self) -> Value {
        let (phase, note, problem, settings_url) = match self.machine.phase() {
            Phase::Idle => ("idle", None, None, None),
            Phase::Preparing { note } => ("preparing", note.clone(), None, None),
            Phase::Listening { .. } => ("listening", self.note.clone(), None, None),
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
    fn start(self: &Arc<Self>, sources: Sources, model: String) -> Result<(), String> {
        // Keep launches ordered across every window: one recognizer owns the microphone.
        let mut active = ACTIVE.get_or_init(|| Mutex::new(Weak::new())).lock().unwrap();
        if self.closed.load(Ordering::Acquire) { return Err("Dictation is closed.".into()); }
        if let Some(other) = active.upgrade().filter(|other| !Arc::ptr_eq(other, self)) {
            // One settling its last words has let go of the microphone: it hands over what it
            // heard instead of losing it (a large local model can take seconds to settle).
            let finishing = matches!(other.state.lock().unwrap().machine.phase(), Phase::Finishing { .. });
            other.event(if finishing { Event::FinishTimedOut } else { Event::Cancel }, None);
        }
        let mut state = self.state.lock().unwrap();
        if state.machine.phase().is_active() { return Ok(()); }
        state.session += 1;
        let session = state.session;
        state.delivery = None;
        state.note = None;
        state.handle(Event::Start);
        self.topic.publish(state.snapshot());
        *active = Arc::downgrade(self);
        match Engine::start(&sources, &model) {
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
        // A note while listening is the engine's own (a local model still loading); empty clears it.
        if let (Event::Note(note), Phase::Listening { .. }) = (&event, state.machine.phase()) {
            state.note = (!note.is_empty()).then(|| note.clone());
            self.topic.publish(state.snapshot());
            return true;
        }
        if matches!(event, Event::Ready) {
            state.note = None;
        }
        let effect = state.handle(event);
        if effect == Effect::FinishEngine {
            let deadline = state.engine.as_ref().map_or(STOPPING_DEADLINE, Engine::finish_deadline);
            if let Some(engine) = state.engine.as_mut() { engine.finish(); }
            let owner = Arc::downgrade(self);
            let session = state.session;
            // The helper normally finishes first; the deadline preserves its last hypothesis.
            let _ = std::thread::Builder::new().name("nsh-dictation-finish".into()).spawn(move || {
                std::thread::sleep(deadline);
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
        let settings = crate::settings::Settings::get().await?;
        if !settings.dictation_mic {
            return Err("Show microphone buttons for dictation in Settings to dictate.".into());
        }
        let sources: Sources = serde_json::from_value(sources).map_err(|error| format!("invalid: {error}"))?;
        let model = settings.dictation_model;
        service::blocking(move || owner.start(sources, model)).await
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


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_local_model_listens_at_once_and_says_it_is_loading_until_it_is() {
        let (sender, receiver) = async_channel::unbounded();
        let next = || receiver.try_recv().ok();
        let mut gate = Gate::default();
        gate.note("Loading Parakeet…".into(), &sender);
        assert_eq!(next(), Some(EngineEvent::Note("Loading Parakeet…".into())));
        gate.microphone(&sender);
        assert_eq!(next(), Some(EngineEvent::Ready));
        // Ready ends preparing; the note is said again for listening.
        assert_eq!(next(), Some(EngineEvent::Note("Loading Parakeet…".into())));
        gate.microphone(&sender);
        assert_eq!(next(), None, "ready is said once");
        gate.loaded(&sender);
        assert_eq!(next(), Some(EngineEvent::Note(String::new())));
        // A model already loaded says nothing more.
        let mut warm = Gate::default();
        warm.loaded(&sender);
        warm.microphone(&sender);
        assert_eq!(next(), Some(EngineEvent::Ready));
        assert_eq!(next(), None);
    }

    #[test]
    fn a_note_while_listening_shows_until_an_empty_one() {
        let owner = Owner::new();
        let note = || owner.state.lock().unwrap().snapshot()["note"].clone();
        owner.event(Event::Start, None);
        owner.event(Event::Ready, None);
        owner.event(Event::Note("Loading Nemotron…".into()), None);
        assert_eq!(note(), "Loading Nemotron…");
        owner.event(Event::Heard("hello".into()), None);
        assert_eq!(note(), "Loading Nemotron…");
        owner.event(Event::Note(String::new()), None);
        assert_eq!(note(), Value::Null);
        assert!(matches!(owner.state.lock().unwrap().machine.phase(), Phase::Listening { .. }));
    }

    #[test]
    fn a_missing_model_names_itself_and_where_to_get_it() {
        let message = Problem::ModelMissing("Parakeet TDT 0.6B v3".into()).message();
        assert!(message.starts_with("Parakeet TDT 0.6B v3 is not on this Mac yet."), "{message}");
        assert!(message.contains("Settings"), "{message}");
    }

    /// A recording through the capture helper and a local model, start to delivered text. Needs
    /// a disposable RIWORK_HOME (the runtime is set up there), the model in the Hugging Face
    /// cache, and a built helper:
    ///
    /// RIWORK_HOME=$(mktemp -d) RIWORK_SPEECH_HELPER=target/debug/riwork-speech
    /// RIWORK_DICTATION_RECORDING=speech.wav RIWORK_TEST_SPEECH_MODEL=parakeet-v3
    /// RIWORK_TEST_SPEECH_EXPECT="words it says" cargo test -p riwork-nsh-native --lib -- --ignored local_model
    #[test]
    #[ignore = "needs the speech runtime, a downloaded model, the helper and a recording"]
    fn a_local_model_dictates_a_recording_end_to_end() {
        let model = std::env::var("RIWORK_TEST_SPEECH_MODEL").unwrap_or_else(|_| "parakeet-v3".into());
        let expect = std::env::var("RIWORK_TEST_SPEECH_EXPECT").unwrap_or_default();
        assert!(recording().is_some(), "set RIWORK_DICTATION_RECORDING");
        let wait = |what: &str, seconds: u64, done: &dyn Fn() -> bool| {
            let deadline = std::time::Instant::now() + Duration::from_secs(seconds);
            while !done() {
                assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
                std::thread::sleep(Duration::from_millis(100));
            }
        };
        let status = || {
            speech::catalog().iter().position(|spec| spec.key == model).and_then(|index| {
                let snapshot = speech::test_snapshot();
                snapshot["models"][index]["status"].as_str().map(str::to_owned)
            })
        };
        // The first dictation with a model sets the runtime up in this home (the model is cached).
        speech::test_download(&model).unwrap();
        wait("the runtime", 900, &|| !matches!(status().as_deref(), Some("installing" | "downloading")));
        assert_eq!(status().as_deref(), Some("ready"), "{}", speech::test_snapshot());

        let owner = Owner::new();
        owner.start(Sources::default(), model.clone()).unwrap();
        let phase = || owner.state.lock().unwrap().machine.phase().clone();
        wait("listening", 120, &|| matches!(phase(), Phase::Listening { .. } | Phase::Failed(_)));
        assert!(matches!(phase(), Phase::Listening { .. }), "{:?}", phase());
        // The recording plays at its own pace; its words arrive while it does.
        wait("words", 60, &|| !phase().text().is_empty());
        std::thread::sleep(Duration::from_secs(8));
        owner.event(Event::Stop, None);
        wait("the final text", 60, &|| !phase().is_active());
        let delivery = owner.state.lock().unwrap().delivery.clone().expect("a delivery");
        assert_eq!(delivery["kind"], "commit", "{delivery}");
        let text = delivery["text"].as_str().unwrap().to_owned();
        eprintln!("dictated: {text}");
        let normalize = |text: &str| text.to_lowercase().chars().filter(|c| c.is_alphanumeric() || c.is_whitespace()).collect::<String>();
        assert!(normalize(&text).contains(&normalize(&expect)), "{text:?} does not contain {expect:?}");
    }
}
