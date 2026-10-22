//! Dictation into the message box: the mic button and ⌃⌥D toggle it; ⎋ stops and keeps its words.
//! the words appear at the caret as they are heard, until the person stops it.
//! Send/Return settle speech before sending once. The rules are `dictation::Machine`'s; this runs them
//! against the helper process and the box. All of it is there only while Settings shows the
//! mic in chats (`mic_shown`): otherwise the button, the key and the hints are gone, and no
//! helper is started, so nothing asks for the microphone.

use gpui::{App, Context, Focusable, Global, Task, WeakEntity, Window};

use crate::{
    dictation::{Effect, Engine, Event, Insertion, Machine, Phase, STOPPING_DEADLINE, Sources},
    settings::Settings,
};

use super::{
    ChatView,
    attachment_draft::{Status, Submission},
    state::provider_name,
};

/// One dictation of a chat tab, and what it runs on.
#[derive(Default)]
pub(super) struct Dictation {
    machine: Machine,
    engine: Option<Engine>,
    insertion: Insertion,
    /// The helper's events.
    tasks: Vec<Task<()>>,
    stopping: Option<Task<()>>,
    send_after_finish: bool,
    /// Keep the starting viewport until the terminal event's next frame.
    height_locked: bool,
    session: u64,
    /// The send button already signals a pending dictation send; avoid adding a review row.
    pending_send: Option<u64>,
}

impl Dictation {
    pub fn user_edited(&mut self) {
        self.insertion.user_edited();
    }
    pub fn phase(&self) -> &Phase {
        self.machine.phase()
    }
    pub fn show_submission(&self, submission: &Submission) -> bool {
        self.pending_send != Some(submission.id) || submission.status != Status::Pending
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
            self.lock_dictation_height(cx);
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

    /// Closing the tab: stop at once and take out what was dictated.
    pub(super) fn cancel_dictation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dictate(Event::Cancel, window, cx);
    }

    /// Escape only stops the microphone, including when a send was waiting for final words.
    pub(super) fn stop_dictation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dictation.send_after_finish = false;
        self.dictate(Event::Stop, window, cx);
    }

    pub(super) fn send_after_dictation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dictation.send_after_finish = true;
        self.dictate(Event::Stop, window, cx);
    }

    fn lock_dictation_height(&mut self, cx: &mut Context<Self>) {
        self.dictation.session += 1;
        let state = self.composer.read(cx);
        let rows = state.line_height().map_or(1, |line| {
            (f32::from(state.input_bounds().size.height) / f32::from(line))
                .round()
                .max(1.) as usize
        });
        self.composer
            .update(cx, |state, cx| state.set_auto_grow(rows, rows, cx));
        self.dictation.height_locked = true;
    }

    pub(super) fn dictation_user_edited(&mut self, cx: &mut Context<Self>) {
        self.dictation.user_edited();
        if !self.dictation.phase().is_active() {
            self.release_dictation_height(cx);
        }
    }

    pub(super) fn release_dictation_height(&mut self, cx: &mut Context<Self>) {
        if self.dictation.height_locked {
            self.dictation.height_locked = false;
            self.composer
                .update(cx, |state, cx| state.set_auto_grow(1, 8, cx));
        }
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
        if matches!(event, Event::Cancel | Event::Failed(_)) {
            self.dictation.send_after_finish = false;
        }
        let was_active = self.dictation.phase().is_active();
        let heard = matches!(event, Event::Heard(_));
        let before = self.dictation.phase().text().to_owned();
        let effect = self.dictation.machine.handle(event);
        let phase = self.dictation.phase().clone();
        let focused = self.composer.read(cx).focus_handle(cx).is_focused(window);
        if phase.is_active() && (heard || phase.text() != before) {
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
                self.dictation.stopping = Some(cx.spawn_in(window, async move |this, cx| {
                    cx.background_executor().timer(STOPPING_DEADLINE).await;
                    let _ = this.update_in(cx, |view, window, cx| {
                        view.dictate(Event::FinishTimedOut, window, cx);
                    });
                }));
            }
            Effect::CancelEngine => {
                let state = self.composer.read(cx);
                let edit = self.dictation.insertion.discard_snapshot(
                    &state.value(),
                    state.selected_range(),
                    state.cursor(),
                );
                self.apply_dictation_edit(edit, window, cx);
                self.end_dictation(window, cx);
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
                self.end_dictation(window, cx);
            }
        }
        // Finished with nothing heard: whatever was shown goes, and so does the helper.
        if was_active && !phase.is_active() && quiet {
            let state = self.composer.read(cx);
            let edit = self.dictation.insertion.discard_snapshot(
                &state.value(),
                state.selected_range(),
                state.cursor(),
            );
            self.apply_dictation_edit(edit, window, cx);
            self.end_dictation(window, cx);
        }
        if was_active && !phase.is_active() && std::mem::take(&mut self.dictation.send_after_finish)
        {
            // Kit emits Change after this update. Let it advance the draft generation before
            // snapshotting the send, so its receipt can clear the final text exactly once.
            cx.defer_in(window, |view, window, cx| {
                let already_pending = view.pending_submission.is_some();
                view.send_message(window, cx);
                if !already_pending {
                    view.dictation.pending_send = view.pending_submission;
                }
            });
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
        self.lock_dictation_height(cx);
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
                self.pump_dictation(events, window, cx);
            }
            Err(problem) => {
                self.dictate(Event::Failed(problem), window, cx);
            }
        }
    }

    fn pump_dictation(
        &mut self,
        events: async_channel::Receiver<crate::dictation::EngineEvent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let session = self.dictation.session;
        let pump = cx.spawn_in(window, async move |this, cx| {
            while let Ok(event) = events.recv().await {
                let event = event.into_event();
                if this
                    .update_in(cx, |view, window, cx| {
                        if view.dictation.session != session || !view.dictation.phase().is_active()
                        {
                            return false;
                        }
                        view.dictate(event, window, cx);
                        view.dictation.phase().is_active()
                    })
                    .ok()
                    != Some(true)
                {
                    return;
                }
            }
        });
        self.dictation.tasks = vec![pump];
    }

    #[cfg(test)]
    pub(super) fn dictation_pump_fixture(
        &mut self,
        events: async_channel::Receiver<crate::dictation::EngineEvent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pump_dictation(events, window, cx);
    }

    fn end_dictation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Dropping the engine cancels the helper and ends its process.
        self.dictation.engine = None;
        self.dictation.tasks.clear();
        self.dictation.stopping = None;
        let session = self.dictation.session;
        cx.on_next_frame(window, move |view, _, cx| {
            if view.dictation.session == session && !view.dictation.phase().is_active() {
                view.release_dictation_height(cx);
            }
        });
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
