//! Dictation into the message box: the mic button and ⌃⌥D start and stop it, ⎋ cancels it,
//! the words appear at the caret as they are heard, and it stops by itself after a pause.
//! Dictation never sends the message. The rules are `dictation::Machine`'s; this runs them
//! against the helper process and the box. All of it is there only while Settings shows the
//! mic in chats (`mic_shown`): otherwise the button, the key and the hints are gone, and no
//! helper is started, so nothing asks for the microphone.

use std::time::{Duration, Instant};

use gpui::{App, Context, Focusable, Global, Task, WeakEntity, Window};

use crate::{
    dictation::{self, Effect, Engine, Event, Insertion, Machine, Phase, Sources},
    settings::Settings,
};

use super::{ChatView, state::provider_name};

/// One dictation of a chat tab, and what it runs on.
#[derive(Default)]
pub(super) struct Dictation {
    machine: Machine,
    engine: Option<Engine>,
    insertion: Insertion,
    /// When the words last changed (or the microphone opened), for the silence that ends it.
    last_change: Option<Instant>,
    /// The helper's events, and the silence watch.
    tasks: Vec<Task<()>>,
}

impl Dictation {
    pub fn user_edited(&mut self) {
        self.insertion.user_edited();
    }
    pub fn phase(&self) -> &Phase {
        self.machine.phase()
    }
}

/// Whether chats show the mic: Settings → "Show microphone buttons for dictation", off by
/// default. The phone follows the same setting through `appearance.json`.
pub(super) fn mic_shown(cx: &App) -> bool {
    mic_in(cx.try_global::<Settings>())
}

/// `mic_shown` for these settings; none yet is the default, off.
fn mic_in(settings: Option<&Settings>) -> bool {
    settings.is_some_and(|settings| settings.dictation_mic)
}

/// What becomes of a dictation in `phase` when the mic is hidden: one that runs is cancelled,
/// so what it put in the box goes and the helper ends, and a failure is put away.
fn when_hidden(phase: &Phase) -> Option<Event> {
    match phase {
        phase if phase.is_active() => Some(Event::Cancel),
        Phase::Failed(_) => Some(Event::Dismiss),
        _ => None,
    }
}

/// The chat tab whose dictation runs, app-wide: one at a time, as there is one microphone.
#[derive(Default)]
struct Active(Option<WeakEntity<ChatView>>);

impl Global for Active {}

impl ChatView {
    #[cfg(test)]
    pub(super) fn dictation_fixture(
        &mut self,
        event: Event,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event == Event::Start {
            assert_eq!(self.dictation.machine.handle(event), Effect::BeginEngine);
        } else {
            self.dictate(event, window, cx);
        }
    }
    /// The mic button and ⌃⌥D: start a dictation into the message box, or stop the one that
    /// runs and keep what was heard.
    pub(super) fn toggle_dictation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.accepts_input() || !mic_shown(cx) {
            return;
        }
        if self.dictation.phase().is_active() {
            self.dictate(Event::Stop, window, cx);
        } else {
            self.focus(window, cx);
            self.dictate(Event::Start, window, cx);
        }
    }

    /// ⎋ and closing the tab: stop at once and take out what was dictated.
    pub(super) fn cancel_dictation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dictate(Event::Cancel, window, cx);
    }

    /// The mic setting changed: turned off, a dictation that runs is cancelled (what it put in
    /// the box goes) and a failure it left is put away. Either way the box is drawn again.
    pub(super) fn follow_mic_setting(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !mic_shown(cx)
            && let Some(event) = when_hidden(self.dictation.phase())
        {
            self.dictate(event, window, cx);
        }
        cx.notify();
    }

    /// The failure was seen.
    pub(super) fn dismiss_dictation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dictate(Event::Dismiss, window, cx);
    }

    fn dictate(&mut self, event: Event, window: &mut Window, cx: &mut Context<Self>) {
        let before = self.dictation.phase().text().to_owned();
        if matches!(event, Event::Heard(_) | Event::Ready) {
            self.dictation.last_change = Some(Instant::now());
        }
        let effect = self.dictation.machine.handle(event);
        let phase = self.dictation.phase().clone();
        let focused = self.composer.read(cx).focus_handle(cx).is_focused(window);
        if phase.is_active() && phase.text() != before {
            let state = self.composer.read(cx);
            let edit = self.dictation.insertion.show_snapshot(
                &state.value(),
                state.selected_range(),
                state.cursor(),
                phase.text(),
                focused,
            );
            self.apply_dictation_edit(edit, window, cx);
        }
        let quiet = effect == Effect::None;
        match effect {
            Effect::None => {}
            Effect::BeginEngine => self.begin_dictation(window, cx),
            Effect::FinishEngine => {
                if let Some(engine) = self.dictation.engine.as_mut() {
                    engine.finish();
                }
            }
            Effect::CancelEngine => {
                let state = self.composer.read(cx);
                let edit = self.dictation.insertion.discard_snapshot(
                    &state.value(),
                    state.selected_range(),
                    state.cursor(),
                );
                self.apply_dictation_edit(edit, window, cx);
                self.end_dictation(cx);
            }
            Effect::Deliver(text) => {
                let state = self.composer.read(cx);
                let edit = self.dictation.insertion.commit_snapshot(
                    &state.value(),
                    state.selected_range(),
                    state.cursor(),
                    &text,
                    focused,
                );
                self.apply_dictation_edit(edit, window, cx);
                self.end_dictation(cx);
            }
        }
        // Finished with nothing heard: whatever was shown goes, and so does the helper.
        if !phase.is_active() && self.dictation.engine.is_some() && quiet {
            let state = self.composer.read(cx);
            let edit = self.dictation.insertion.discard_snapshot(
                &state.value(),
                state.selected_range(),
                state.cursor(),
            );
            self.apply_dictation_edit(edit, window, cx);
            self.end_dictation(cx);
        }
        cx.notify();
    }

    fn apply_dictation_edit(
        &mut self,
        (text, selection): (String, std::ops::Range<usize>),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.composer.read(cx).value().as_ref() != text.as_str() {
            self.programmatic_changes += 1;
            self.composer
                .update(cx, |state, cx| state.replace_all(text, window, cx));
        }
        self.composer
            .update(cx, |state, cx| state.set_selected_range(selection, cx));
    }

    fn begin_dictation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Another tab's dictation ends first: there is one microphone.
        let me = cx.entity().downgrade();
        let other = cx.default_global::<Active>().0.replace(me.clone());
        if let Some(other) = other.filter(|other| other != &me) {
            let handle = other.read_with(cx, |view, _| view.window_handle).ok();
            if let Some(handle) = handle {
                if handle == window.window_handle() {
                    let _ = other.update(cx, |view, cx| view.cancel_dictation(window, cx));
                } else {
                    let _ = handle.update(cx, |_, window, cx| {
                        other.update(cx, |view, cx| view.cancel_dictation(window, cx))
                    });
                }
            }
        }
        let sources = self.speech_sources();
        match Engine::start(&sources) {
            Ok((engine, events)) => {
                self.dictation.engine = Some(engine);
                self.dictation.last_change = Some(Instant::now());
                let pump = cx.spawn_in(window, async move |this, cx| {
                    while let Ok(event) = events.recv().await {
                        let event = event.into_event();
                        if this
                            .update_in(cx, |view, window, cx| view.dictate(event, window, cx))
                            .is_err()
                        {
                            return;
                        }
                    }
                });
                let watch = cx.spawn_in(window, async move |this, cx| {
                    loop {
                        cx.background_executor()
                            .timer(Duration::from_millis(250))
                            .await;
                        match this.update_in(cx, |view, window, cx| view.watch_silence(window, cx))
                        {
                            Ok(true) => {}
                            _ => return,
                        }
                    }
                });
                self.dictation.tasks = vec![pump, watch];
            }
            Err(problem) => {
                self.dictate(Event::Failed(problem), window, cx);
            }
        }
    }

    /// Ends a dictation by itself after a pause once something was said, or after a long wait
    /// when nothing was. False once there is nothing to watch.
    fn watch_silence(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Phase::Listening { text } = self.dictation.phase() else {
            return self.dictation.phase().is_active();
        };
        let wait = if text.is_empty() {
            dictation::SILENCE_BEFORE_SPEECH
        } else {
            dictation::SILENCE_AFTER_SPEECH
        };
        if self
            .dictation
            .last_change
            .is_some_and(|at| at.elapsed() >= wait)
        {
            self.dictate(Event::Stop, window, cx);
        }
        true
    }

    fn end_dictation(&mut self, cx: &mut Context<Self>) {
        // Dropping the engine cancels the helper and ends its process.
        self.dictation.engine = None;
        self.dictation.tasks.clear();
        self.dictation.last_change = None;
        let me = cx.entity().downgrade();
        if cx
            .try_global::<Active>()
            .is_some_and(|active| active.0.as_ref() == Some(&me))
        {
            cx.set_global(Active(None));
        }
    }

    /// What the vocabulary is built from: the names around the chat, its agent and model, and
    /// the end of its transcript (the phone's `RemoteModel.speechSources` for a chat).
    fn speech_sources(&self) -> Sources {
        let mut names = self.speech_names.clone();
        let mut paths = self.speech_paths.clone();
        if let Some(info) = &self.model.transcript.info {
            names.push(info.title.clone());
            names.push(provider_name(info.provider).to_owned());
            names.extend(info.model.clone());
            paths.push(info.cwd.display().to_string());
        }
        Sources::of_chat(names, paths, &self.model.transcript.items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dictation::Problem;

    #[test]
    fn the_mic_is_hidden_unless_settings_show_it() {
        assert!(!mic_in(None), "no settings yet");
        assert!(!mic_in(Some(&Settings::default())), "off by default");
        let shown = Settings {
            dictation_mic: true,
            ..Settings::default()
        };
        assert!(mic_in(Some(&shown)));
    }

    #[test]
    fn hiding_the_mic_cancels_a_dictation_and_puts_a_failure_away() {
        // Listening, with words already in the box: cancelled, the helper ends, the words go.
        let mut machine = Machine::default();
        machine.handle(Event::Start);
        machine.handle(Event::Ready);
        machine.handle(Event::Heard("half a sentence".into()));
        let event = when_hidden(machine.phase()).unwrap();
        assert_eq!(event, Event::Cancel);
        assert_eq!(machine.handle(event), Effect::CancelEngine);
        assert_eq!(machine.phase(), &Phase::Idle);
        // Getting ready and settling are cancelled the same way.
        let mut machine = Machine::default();
        machine.handle(Event::Start);
        assert_eq!(when_hidden(machine.phase()), Some(Event::Cancel));
        machine.handle(Event::Ready);
        machine.handle(Event::Heard("done".into()));
        machine.handle(Event::Stop);
        assert!(matches!(machine.phase(), Phase::Finishing { .. }));
        assert_eq!(machine.handle(Event::Cancel), Effect::CancelEngine);
        // A failure's message goes; nothing at all is left alone.
        let mut machine = Machine::default();
        machine.handle(Event::Start);
        machine.handle(Event::Failed(Problem::Interrupted));
        let event = when_hidden(machine.phase()).unwrap();
        assert_eq!(machine.handle(event), Effect::None);
        assert_eq!(machine.phase(), &Phase::Idle);
        assert_eq!(when_hidden(&Phase::Idle), None);
    }
}
