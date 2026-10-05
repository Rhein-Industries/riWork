//! Dictation into the chat's message box, recognized on this Mac.
//!
//! Recognition runs in `riwork-speech` (macos/speech, built by build.rs), a small Swift helper
//! beside the executable: Apple's on-device `SpeechAnalyzer` is a Swift-only API. The helper
//! builds the session's vocabulary and rewrites what it hears onto it with the phone's own code
//! (ios/Core/SpeechVocabulary.swift, compiled in place), so "key bar view" comes back as
//! `KeyBarView` on both. This file holds the app's side, which needs no window and is tested
//! alone:
//!
//! - `Machine`, the rules of one dictation, the phone's `DictationMachine` in Rust with its tests;
//! - `insert` and `Insertion`, the text put at the caret as it is heard and taken out on cancel,
//!   the phone's `DictatedText.insert` and `TextInsertion`;
//! - `Sources`, what the chat offers the vocabulary;
//! - `Engine`, the helper process and the events it reports.

use std::{
    io::{BufRead, BufReader, Write},
    ops::Range,
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    time::Duration,
};

use serde::{Deserialize, Serialize};

use crate::{
    chat::model::{Item, ItemBody},
    project_settings::Input,
};

/// Stop by itself once speech has been followed by this much quiet.
pub const SILENCE_AFTER_SPEECH: Duration = Duration::from_millis(2_500);
/// Stop by itself if nothing at all has been heard for this long.
pub const SILENCE_BEFORE_SPEECH: Duration = Duration::from_secs(10);

/// The shortcut that starts and stops dictation in a chat, as it is shown (main.rs binds it
/// as `ctrl-alt-d`).
pub const SHORTCUT_LABEL: &str = "⌃⌥D";

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
    /// The person clicked stop, or the silence after speech was long enough.
    Stop,
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
            (
                Phase::Preparing { .. } | Phase::Listening { .. } | Phase::Finishing { .. },
                Event::Finished(text),
            ) => {
                self.phase = Phase::Idle;
                let text = text.trim();
                if text.is_empty() {
                    Effect::None
                } else {
                    Effect::Deliver(text.to_owned())
                }
            }
            (_, Event::Cancel) if active => {
                self.phase = Phase::Idle;
                Effect::CancelEngine
            }
            // The microphone going away keeps what was said before it.
            (
                Phase::Listening { text } | Phase::Finishing { text },
                Event::Failed(Problem::Interrupted),
            ) => {
                let kept = text.trim().to_owned();
                self.phase = Phase::Failed(Problem::Interrupted);
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

/// `dictated` put into `text` in place of the byte range `at`: a space is added between it and a
/// word right before or after, so dictating at the end of a sentence does not glue words
/// together. Returns the new text and where the caret goes. The phone's `DictatedText.insert`,
/// in byte offsets.
pub fn insert(dictated: &str, text: &str, at: Range<usize>) -> (String, usize) {
    let boundary = |mut offset: usize| {
        offset = offset.min(text.len());
        while !text.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
    };
    let start = boundary(at.start);
    let end = boundary(at.end.max(start)).max(start);
    let mut piece = dictated.trim().to_owned();
    if piece.is_empty() {
        return (text.to_owned(), start);
    }
    if let Some(before) = text[..start].chars().next_back()
        && !before.is_whitespace()
        && !"([{\"'`".contains(before)
    {
        piece.insert(0, ' ');
    }
    if let Some(after) = text[end..].chars().next()
        && !after.is_whitespace()
        && !".,;:!?)]}".contains(after)
    {
        piece.push(' ');
    }
    let result = format!("{}{piece}{}", &text[..start], &text[end..]);
    (result, start + piece.len())
}

/// Dictation into an input at its caret: the words appear as they are heard, and the final text
/// replaces them. Cancelling takes them out again. Typing in between starts over from what the
/// input then holds, so nothing typed is lost. The phone's `TextInsertion`.
#[derive(Debug, Default)]
pub struct Insertion {
    /// The input's text before dictation showed anything, and the range it replaces.
    base: Option<(String, Range<usize>)>,
    /// What this wrote last, to tell whether the input was edited since.
    written: Option<String>,
}

impl Insertion {
    /// Shows `heard` at the caret in place of what was shown before; "" takes it out. `focused`
    /// says whether the input has the keys: one that never had them has its caret at the start,
    /// and dictation then adds to the end.
    pub fn show(&mut self, input: &mut Input, heard: &str, focused: bool) {
        if self.base.is_none() || self.written.as_deref() != Some(input.text.as_str()) {
            let anchor = if !focused && input.cursor() == 0 && !input.text.is_empty() {
                input.text.len()..input.text.len()
            } else {
                input.selection.clone()
            };
            self.base = Some((input.text.clone(), anchor));
        }
        let Some((base, anchor)) = &self.base else {
            return;
        };
        let (text, caret) = if heard.trim().is_empty() {
            (base.clone(), anchor.end)
        } else {
            insert(heard, base, anchor.clone())
        };
        if input.text != text || input.selection != (caret..caret) {
            input.text = text;
            input.selection = caret..caret;
            input.reversed = false;
            input.marked = None;
        }
        self.written = Some(input.text.clone());
    }

    /// The final text, at the caret. The next dictation starts from where this one left it.
    pub fn commit(&mut self, input: &mut Input, text: &str, focused: bool) {
        self.show(input, text, focused);
        *self = Self::default();
    }

    /// Takes out what dictation showed.
    pub fn discard(&mut self, input: &mut Input) {
        if self.base.is_some() {
            self.show(input, "", true);
        }
        *self = Self::default();
    }
}

/// What the chat on screen offers dictation to listen for: the same as the phone's
/// `SpeechVocabularySources`. Read fresh each time dictation starts and handed to the helper,
/// never stored or sent anywhere else.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Sources {
    /// Names that matter most: project, worktree branches, chat title, agent, model.
    pub names: Vec<String>,
    /// Working directories and roots: their last components are names too.
    pub paths: Vec<String>,
    /// The chat's recent messages, commands, files and tools, oldest first.
    pub text: String,
}

/// How many of the latest transcript items the vocabulary reads, as the phone does.
pub const RECENT_ITEMS: usize = 40;

impl Sources {
    /// The sources of a chat: `names` and `paths` around it, then the end of its transcript.
    pub fn of_chat(names: Vec<String>, paths: Vec<String>, items: &[Item]) -> Self {
        let recent = &items[items.len().saturating_sub(RECENT_ITEMS)..];
        let text = recent
            .iter()
            .map(|item| item_text(&item.body))
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        let keep = |list: Vec<String>| {
            let mut kept: Vec<String> = Vec::new();
            for entry in list {
                let entry = entry.trim().to_owned();
                if !entry.is_empty() && !kept.contains(&entry) {
                    kept.push(entry);
                }
            }
            kept
        };
        Self {
            names: keep(names),
            paths: keep(paths),
            text,
        }
    }
}

/// The words of a transcript item that a person might say back: messages, commands, file names
/// and tools. The phone's `RemoteModel.speechText`.
pub fn item_text(body: &ItemBody) -> String {
    match body {
        ItemBody::UserMessage { text } | ItemBody::AgentMessage { text } => text.clone(),
        ItemBody::Command { command, .. } => command.clone(),
        ItemBody::FileChange { changes } => changes
            .iter()
            .map(|change| change.path.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        ItemBody::ToolCall { tool, .. } => tool.clone(),
        ItemBody::Plan { steps, .. } | ItemBody::Todo { items: steps } => steps
            .iter()
            .map(|step| step.text.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        ItemBody::Reasoning { .. }
        | ItemBody::WebSearch { .. }
        | ItemBody::Compaction
        | ItemBody::Notice { .. } => String::new(),
    }
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

/// `riwork-speech`: beside the executable (in the bundle, Contents/MacOS), else where build.rs
/// left it. `RIWORK_SPEECH_HELPER` names another one.
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
            std::thread::Builder::new()
                .name("riwork-speech".to_owned())
                .spawn(move || read_events(BufReader::new(stdout), &sender))
                .ok();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn run(events: Vec<Event>) -> (Machine, Vec<Effect>) {
        let mut machine = Machine::default();
        let effects = events
            .into_iter()
            .map(|event| machine.handle(event))
            .collect();
        (machine, effects)
    }

    fn heard(text: &str) -> Event {
        Event::Heard(text.to_owned())
    }

    fn finished(text: &str) -> Event {
        Event::Finished(text.to_owned())
    }

    // The phone's DictationMachineTests, one for one.

    #[test]
    fn click_to_start_click_to_stop_delivers_the_final_text_once() {
        let (machine, effects) = run(vec![
            Event::Start,
            Event::Ready,
            heard("git"),
            heard("git status"),
            Event::Stop,
            heard("git status."),
            finished("git status."),
        ]);
        assert_eq!(
            effects,
            [
                Effect::BeginEngine,
                Effect::None,
                Effect::None,
                Effect::None,
                Effect::FinishEngine,
                Effect::None,
                Effect::Deliver("git status.".to_owned()),
            ]
        );
        assert_eq!(machine.phase(), &Phase::Idle);
    }

    #[test]
    fn the_phases_carry_the_text_heard_so_far() {
        let mut machine = Machine::default();
        machine.handle(Event::Start);
        assert_eq!(machine.phase(), &Phase::Preparing { note: None });
        assert!(machine.phase().is_active());
        machine.handle(Event::Note(
            "Downloading the speech model (once)…".to_owned(),
        ));
        assert_eq!(
            machine.phase(),
            &Phase::Preparing {
                note: Some("Downloading the speech model (once)…".to_owned())
            }
        );
        machine.handle(Event::Ready);
        assert_eq!(
            machine.phase(),
            &Phase::Listening {
                text: String::new()
            }
        );
        machine.handle(heard("hello"));
        assert_eq!(machine.phase().text(), "hello");
        machine.handle(Event::Stop);
        assert_eq!(
            machine.phase(),
            &Phase::Finishing {
                text: "hello".to_owned()
            }
        );
    }

    #[test]
    fn cancel_drops_everything_from_any_active_phase() {
        for prefix in [
            vec![Event::Start],
            vec![Event::Start, Event::Ready, heard("abc")],
            vec![Event::Start, Event::Ready, heard("abc"), Event::Stop],
        ] {
            let mut events = prefix;
            events.extend([Event::Cancel, finished("abc")]);
            let (machine, effects) = run(events);
            assert_eq!(
                effects[effects.len() - 2..],
                [Effect::CancelEngine, Effect::None],
                "nothing is delivered after a cancel"
            );
            assert_eq!(machine.phase(), &Phase::Idle);
        }
    }

    #[test]
    fn stopping_before_the_microphone_is_open_cancels() {
        assert_eq!(
            run(vec![Event::Start, Event::Stop]).1.last(),
            Some(&Effect::CancelEngine)
        );
    }

    #[test]
    fn nothing_heard_delivers_nothing() {
        let (machine, effects) = run(vec![
            Event::Start,
            Event::Ready,
            Event::Stop,
            finished("  "),
        ]);
        assert_eq!(effects.last(), Some(&Effect::None));
        assert_eq!(machine.phase(), &Phase::Idle);
    }

    #[test]
    fn the_recognizer_may_finish_by_itself() {
        assert_eq!(
            run(vec![
                Event::Start,
                Event::Ready,
                heard("ls"),
                finished("ls")
            ])
            .1
            .last(),
            Some(&Effect::Deliver("ls".to_owned()))
        );
    }

    #[test]
    fn a_permission_refused_is_a_failure_until_seen() {
        let (mut machine, effects) =
            run(vec![Event::Start, Event::Failed(Problem::MicrophoneDenied)]);
        assert_eq!(effects.last(), Some(&Effect::CancelEngine));
        assert_eq!(machine.phase(), &Phase::Failed(Problem::MicrophoneDenied));
        assert!(!machine.phase().is_active());
        assert!(Problem::MicrophoneDenied.settings_url().is_some());
        assert_eq!(
            machine.handle(Event::Stop),
            Effect::None,
            "the mic shows the failure; stop does nothing"
        );
        machine.handle(Event::Dismiss);
        assert_eq!(machine.phase(), &Phase::Idle);
        assert_eq!(machine.handle(Event::Start), Effect::BeginEngine);
    }

    #[test]
    fn starting_again_after_a_failure_is_allowed() {
        let (mut machine, _) = run(vec![Event::Start, Event::Failed(Problem::ModelUnavailable)]);
        assert_eq!(machine.handle(Event::Start), Effect::BeginEngine);
    }

    #[test]
    fn an_interruption_keeps_what_was_said() {
        let (machine, effects) = run(vec![
            Event::Start,
            Event::Ready,
            heard("deploy to staging"),
            Event::Failed(Problem::Interrupted),
        ]);
        assert_eq!(
            effects.last(),
            Some(&Effect::Deliver("deploy to staging".to_owned()))
        );
        assert_eq!(machine.phase(), &Phase::Failed(Problem::Interrupted));
        assert_eq!(
            run(vec![
                Event::Start,
                Event::Ready,
                Event::Failed(Problem::Interrupted)
            ])
            .1
            .last(),
            Some(&Effect::CancelEngine)
        );
    }

    #[test]
    fn events_out_of_turn_are_ignored() {
        let (machine, effects) = run(vec![
            Event::Ready,
            heard("x"),
            Event::Stop,
            finished("x"),
            Event::Cancel,
            Event::Dismiss,
        ]);
        assert_eq!(effects, vec![Effect::None; 6]);
        assert_eq!(machine.phase(), &Phase::Idle);
        assert_eq!(
            run(vec![Event::Start, Event::Start]).1,
            [Effect::BeginEngine, Effect::None],
            "a second start while one runs does nothing"
        );
    }

    #[test]
    fn every_problem_says_something_and_only_permissions_open_settings() {
        for problem in [
            Problem::MicrophoneDenied,
            Problem::RecognitionDenied,
            Problem::Unsupported,
            Problem::ModelUnavailable,
            Problem::MicrophoneBusy(String::new()),
            Problem::MicrophoneBusy("busy".to_owned()),
            Problem::Interrupted,
            Problem::Unavailable,
            Problem::Failed("x".to_owned()),
        ] {
            assert!(!problem.message().is_empty());
            let permission = matches!(
                problem,
                Problem::MicrophoneDenied | Problem::RecognitionDenied
            );
            assert_eq!(problem.settings_url().is_some(), permission, "{problem:?}");
        }
    }

    // The phone's SpeechVocabularyTests.testInsertingAtTheCaretKeepsWordsApart, in byte offsets
    // (the same numbers: the texts are ASCII).

    #[test]
    fn inserting_at_the_caret_keeps_words_apart() {
        assert_eq!(
            insert("fix the build", "Please", 6..6),
            ("Please fix the build".to_owned(), 20)
        );
        assert_eq!(
            insert("quickly", "Do it now.", 3..3).0,
            "Do quickly it now."
        );
        assert_eq!(insert("Hello", "", 0..0), ("Hello".to_owned(), 5));
        assert_eq!(insert("there", "Hi .", 3..3).0, "Hi there.");
        assert_eq!(
            insert("new", "replace old words", 8..11).0,
            "replace new words"
        );
        assert_eq!(
            insert("x", "abc", 99..104).0,
            "abc x",
            "a range past the end is clipped to it"
        );
        assert_eq!(insert("  ", "abc", 1..1).0, "abc");
    }

    #[test]
    fn inserting_never_splits_a_character() {
        // "é" is two bytes; an offset inside it moves back to its start.
        let (text, caret) = insert("ok", "café", 4..4);
        assert_eq!(text, "caf ok é");
        assert_eq!(&text[..caret], "caf ok ");
    }

    fn input(text: &str, caret: usize) -> Input {
        Input {
            text: text.to_owned(),
            selection: caret..caret,
            ..Default::default()
        }
    }

    #[test]
    fn heard_words_replace_the_last_ones_shown_and_the_final_text_stays() {
        let mut field = input("Please", 6);
        let mut insertion = Insertion::default();
        insertion.show(&mut field, "fix", true);
        assert_eq!(field.text, "Please fix");
        insertion.show(&mut field, "fix the bild", true);
        insertion.show(&mut field, "fix the build", true);
        assert_eq!(field.text, "Please fix the build");
        assert_eq!(field.selection, 20..20, "the caret follows the words");
        insertion.commit(&mut field, "fix the build.", true);
        assert_eq!(field.text, "Please fix the build.");
        assert_eq!(field.cursor(), field.text.len());
        // The next dictation starts from where this one left the caret.
        insertion.show(&mut field, "Then test", true);
        assert_eq!(field.text, "Please fix the build. Then test");
    }

    #[test]
    fn cancelling_takes_out_what_was_dictated_and_keeps_what_was_typed() {
        let mut field = input("Do it now.", 3);
        let mut insertion = Insertion::default();
        insertion.show(&mut field, "quickly", true);
        assert_eq!(field.text, "Do quickly it now.");
        insertion.discard(&mut field);
        assert_eq!(field.text, "Do it now.");
        assert_eq!(field.selection, 3..3);
        // Discarding with nothing shown changes nothing.
        let mut untouched = input("abc", 1);
        Insertion::default().discard(&mut untouched);
        assert_eq!(
            (untouched.text.as_str(), untouched.selection),
            ("abc", 1..1)
        );
    }

    #[test]
    fn typing_during_dictation_is_kept_as_the_new_base() {
        let mut field = input("", 0);
        let mut insertion = Insertion::default();
        insertion.show(&mut field, "hello", true);
        field.replace_lines(None, "!");
        assert_eq!(field.text, "hello!");
        insertion.show(&mut field, "world", true);
        assert_eq!(field.text, "hello! world");
        insertion.discard(&mut field);
        assert_eq!(field.text, "hello!");
    }

    #[test]
    fn a_box_without_the_keys_gets_the_words_at_its_end() {
        let mut field = input("Draft", 0);
        let mut insertion = Insertion::default();
        insertion.show(&mut field, "more", false);
        assert_eq!(field.text, "Draft more");
        // With the keys, the caret at the start is where the words go.
        let mut focused = input("Draft", 0);
        Insertion::default().show(&mut focused, "A", true);
        assert_eq!(focused.text, "A Draft");
    }

    #[test]
    fn a_selection_is_replaced_by_the_words() {
        let mut field = Input {
            text: "replace old words".to_owned(),
            selection: 8..11,
            ..Default::default()
        };
        let mut insertion = Insertion::default();
        insertion.commit(&mut field, "new", true);
        assert_eq!(field.text, "replace new words");
    }

    #[test]
    fn the_helpers_lines_become_events() {
        assert_eq!(
            EngineEvent::parse(r#"{"event":"heard","text":"KeyBarView"}"#),
            Some(EngineEvent::Heard("KeyBarView".to_owned()))
        );
        assert_eq!(
            EngineEvent::parse(r#"{"event":"ready"}"#),
            Some(EngineEvent::Ready)
        );
        assert_eq!(
            EngineEvent::parse(r#"{"event":"note","text":"Downloading"}"#),
            Some(EngineEvent::Note("Downloading".to_owned()))
        );
        assert_eq!(
            EngineEvent::parse(r#"{"event":"finished","text":"done."}"#),
            Some(EngineEvent::Finished("done.".to_owned()))
        );
        assert_eq!(
            EngineEvent::parse(r#"{"event":"failed","problem":"microphone-denied","detail":""}"#),
            Some(EngineEvent::Failed(Problem::MicrophoneDenied))
        );
        assert_eq!(
            EngineEvent::parse(
                r#"{"event":"failed","problem":"microphone-busy","detail":"in use"}"#
            ),
            Some(EngineEvent::Failed(Problem::MicrophoneBusy(
                "in use".to_owned()
            )))
        );
        assert_eq!(
            EngineEvent::parse(r#"{"event":"failed","problem":"failed","detail":"boom"}"#),
            Some(EngineEvent::Failed(Problem::Failed("boom".to_owned())))
        );
        assert_eq!(EngineEvent::parse(r#"{"event":"level","value":0.5}"#), None);
        assert_eq!(EngineEvent::parse("not json"), None);
    }

    #[test]
    fn a_helper_that_ends_without_its_last_word_failed() {
        let (sender, receiver) = async_channel::unbounded();
        read_events(
            "{\"event\":\"ready\"}\n{\"event\":\"heard\",\"text\":\"a\"}\n".as_bytes(),
            &sender,
        );
        let events: Vec<_> = std::iter::from_fn(|| receiver.try_recv().ok()).collect();
        assert_eq!(
            events[..2],
            [EngineEvent::Ready, EngineEvent::Heard("a".to_owned())]
        );
        assert!(matches!(events[2], EngineEvent::Failed(Problem::Failed(_))));
        // One that finished stops there.
        let (sender, receiver) = async_channel::unbounded();
        read_events(
            "{\"event\":\"finished\",\"text\":\"a\"}\n{\"event\":\"heard\",\"text\":\"b\"}\n"
                .as_bytes(),
            &sender,
        );
        let events: Vec<_> = std::iter::from_fn(|| receiver.try_recv().ok()).collect();
        assert_eq!(events, [EngineEvent::Finished("a".to_owned())]);
    }

    fn item(body: ItemBody) -> Item {
        serde_json::from_value(serde_json::json!({ "id": "x", "body": body })).unwrap()
    }

    #[test]
    fn a_chats_sources_are_its_names_and_the_end_of_its_transcript() {
        let mut items: Vec<Item> = (0..50)
            .map(|n| {
                item(ItemBody::UserMessage {
                    text: format!("old{n}"),
                })
            })
            .collect();
        items.push(item(ItemBody::Command {
            command: "cargo test --bin riwork".to_owned(),
            cwd: None,
            output: "lots of output".to_owned(),
            exit_code: Some(0),
        }));
        items.push(item(ItemBody::Reasoning {
            text: "private thoughts".to_owned(),
        }));
        let sources = Sources::of_chat(
            vec![
                "riWork".to_owned(),
                " ".to_owned(),
                "riWork".to_owned(),
                "mac-chat-mic".to_owned(),
            ],
            vec!["/work/riWork".to_owned()],
            &items,
        );
        assert_eq!(sources.names, ["riWork", "mac-chat-mic"]);
        assert!(sources.text.ends_with("cargo test --bin riwork"));
        assert!(!sources.text.contains("old11"), "only the last 40 items");
        assert!(sources.text.contains("old12"));
        assert!(
            !sources.text.contains("lots of output"),
            "a command, not its output"
        );
        assert!(!sources.text.contains("private thoughts"));
        let json = serde_json::to_value(&sources).unwrap();
        assert_eq!(json["paths"][0], "/work/riWork");
    }

    /// The helper's vocabulary and rewrite are the phone's (ios/Core/SpeechVocabulary.swift),
    /// compiled into it in place. Run them as the Mac builds them, with the phone's vectors
    /// (SpeechVocabularyTests): the rewrite as the app gets it, through the helper.
    #[test]
    fn the_helper_rewrites_with_the_phones_vocabulary() {
        let Some(helper) = helper_path() else {
            eprintln!("riwork-speech was not built (no Swift compiler); skipping");
            return;
        };
        let rewrite = |names: &[&str], heard: &str| -> serde_json::Value {
            let mut child = Command::new(&helper)
                .arg("rewrite")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            let request = serde_json::json!({ "names": names, "heard": heard });
            child
                .stdin
                .take()
                .unwrap()
                .write_all(request.to_string().as_bytes())
                .unwrap();
            let output = child.wait_with_output().unwrap();
            serde_json::from_slice(&output.stdout).unwrap()
        };
        // The phone's session: its terms come after the agents, so they are all in the vocabulary.
        let session = [
            "KeyBarView",
            "ios-speech-input",
            "RemoteModel+Keys.swift",
            "SessionVocabulary",
            "RiWork",
            "ChatComposer",
            "Codex",
            "git",
            "iOS",
        ];
        for (heard, expected) in [
            (
                "Ask Claude to refactor the key bar view capsule inset.",
                "Ask Claude to refactor the KeyBarView capsule inset.",
            ),
            (
                "Check out IOS speech input, then run swift test.",
                "Check out ios-speech-input, then run swift test.",
            ),
            (
                "open remote model plus keys.swift and fix it",
                "open RemoteModel+Keys.swift and fix it",
            ),
            (
                "rename it to session vocabulary",
                "rename it to SessionVocabulary",
            ),
            (
                "grep for chat composer in the iOS folder",
                "grep for ChatComposer in the iOS folder",
            ),
            ("open riwork", "open RiWork"),
            ("ask codex", "ask codex"),
            ("Git status", "Git status"),
            (
                "Please look at the bar and the view, then tell me what you think.",
                "Please look at the bar and the view, then tell me what you think.",
            ),
            ("", ""),
        ] {
            assert_eq!(rewrite(&session, heard)["text"], expected, "{heard}");
        }
        // The vocabulary is bounded and starts with the agents.
        let many: Vec<String> = (0..300).map(|n| format!("someIdentifier{n}Name")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        let terms = rewrite(&many, "")["terms"].as_array().unwrap().clone();
        assert!(terms.len() <= 100, "{}", terms.len());
        assert_eq!(terms[0], "Claude");
    }

    fn process_alive(pid: u32) -> bool {
        // Signal 0 checks for the process without touching it.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }

    fn wait_until(mut done: impl FnMut() -> bool) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if done() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn a_scripted_dictation_runs_through_the_helper_with_the_sessions_names() {
        if helper_path().is_none() {
            eprintln!("riwork-speech was not built (no Swift compiler); skipping");
            return;
        }
        let sources = Sources {
            names: vec!["KeyBarView".to_owned()],
            ..Default::default()
        };
        let (mut engine, events) =
            Engine::spawn(&sources, Some("fix the key bar view".to_owned())).unwrap();
        let next = || events.recv_blocking().unwrap();
        assert_eq!(next(), EngineEvent::Ready);
        let mut last = String::new();
        while last != "fix the KeyBarView" {
            match next() {
                EngineEvent::Heard(text) => last = text,
                other => panic!("{other:?}"),
            }
        }
        engine.finish();
        assert_eq!(
            next(),
            EngineEvent::Finished("fix the KeyBarView".to_owned())
        );
    }

    #[test]
    fn the_helper_ends_when_its_dictation_is_dropped_or_the_app_goes() {
        if helper_path().is_none() {
            eprintln!("riwork-speech was not built (no Swift compiler); skipping");
            return;
        }
        let (engine, _events) =
            Engine::spawn(&Sources::default(), Some("a b c d e f g h".to_owned())).unwrap();
        let pid = engine.child.as_ref().unwrap().id();
        assert!(process_alive(pid));
        drop(engine);
        assert!(wait_until(|| !process_alive(pid)), "dropping kills it");

        // An app that dies closes the helper's stdin: the helper ends by itself.
        let mut child = Command::new(helper_path().unwrap())
            .args(["scripted", "a b c d e f g h"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        writeln!(child.stdin.as_mut().unwrap(), "{{}}").unwrap();
        drop(child.stdin.take());
        assert!(
            wait_until(|| child.try_wait().unwrap().is_some()),
            "stdin closing ends it"
        );
    }
}
