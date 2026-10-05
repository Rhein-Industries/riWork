//! Dictation into the message box: the mic button and ⌘⇧Space start and stop it, ⎋ cancels it,
//! the words appear at the caret as they are heard, and it stops by itself after a pause.
//! Dictation never sends the message. The rules are `dictation::Machine`'s; this runs them
//! against the helper process and the box.

use std::time::{Duration, Instant};

use gpui::{Context, Global, Task, WeakEntity};

use crate::dictation::{self, Effect, Engine, Event, Insertion, Machine, Phase, Sources};

use super::{ChatView, Field, state::provider_name};

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
    pub fn phase(&self) -> &Phase {
        self.machine.phase()
    }
}

/// The chat tab whose dictation runs, app-wide: one at a time, as there is one microphone.
#[derive(Default)]
struct Active(Option<WeakEntity<ChatView>>);

impl Global for Active {}

impl ChatView {
    /// The mic button and ⌘⇧Space: start a dictation into the message box, or stop the one that
    /// runs and keep what was heard.
    pub(super) fn toggle_dictation(&mut self, cx: &mut Context<Self>) {
        if !self.accepts_input() {
            return;
        }
        if self.dictation.phase().is_active() {
            self.dictate(Event::Stop, cx);
        } else {
            self.field = Field::Composer;
            self.dictate(Event::Start, cx);
        }
    }

    /// ⎋ and closing the tab: stop at once and take out what was dictated.
    pub(super) fn cancel_dictation(&mut self, cx: &mut Context<Self>) {
        self.dictate(Event::Cancel, cx);
    }

    /// The failure was seen.
    pub(super) fn dismiss_dictation(&mut self, cx: &mut Context<Self>) {
        self.dictate(Event::Dismiss, cx);
    }

    fn dictate(&mut self, event: Event, cx: &mut Context<Self>) {
        let before = self.dictation.phase().text().to_owned();
        if matches!(event, Event::Heard(_) | Event::Ready) {
            self.dictation.last_change = Some(Instant::now());
        }
        let effect = self.dictation.machine.handle(event);
        let phase = self.dictation.phase().clone();
        let focused = self.has_focus && self.field == Field::Composer;
        if phase.is_active() && phase.text() != before {
            self.dictation
                .insertion
                .show(&mut self.composer, phase.text(), focused);
            self.keep_cursor_in_view();
        }
        let quiet = effect == Effect::None;
        match effect {
            Effect::None => {}
            Effect::BeginEngine => self.begin_dictation(cx),
            Effect::FinishEngine => {
                if let Some(engine) = self.dictation.engine.as_mut() {
                    engine.finish();
                }
            }
            Effect::CancelEngine => {
                self.dictation.insertion.discard(&mut self.composer);
                self.end_dictation(cx);
            }
            Effect::Deliver(text) => {
                self.dictation
                    .insertion
                    .commit(&mut self.composer, &text, focused);
                self.keep_cursor_in_view();
                self.end_dictation(cx);
            }
        }
        // Finished with nothing heard: whatever was shown goes, and so does the helper.
        if !phase.is_active() && self.dictation.engine.is_some() && quiet {
            self.dictation.insertion.discard(&mut self.composer);
            self.end_dictation(cx);
        }
        cx.notify();
    }

    fn begin_dictation(&mut self, cx: &mut Context<Self>) {
        // Another tab's dictation ends first: there is one microphone.
        let me = cx.entity().downgrade();
        let other = cx.default_global::<Active>().0.replace(me.clone());
        if let Some(other) = other.filter(|other| other != &me) {
            let _ = other.update(cx, |view, cx| view.cancel_dictation(cx));
        }
        let sources = self.speech_sources();
        match Engine::start(&sources) {
            Ok((engine, events)) => {
                self.dictation.engine = Some(engine);
                self.dictation.last_change = Some(Instant::now());
                let pump = cx.spawn(async move |this, cx| {
                    while let Ok(event) = events.recv().await {
                        let event = event.into_event();
                        if this.update(cx, |view, cx| view.dictate(event, cx)).is_err() {
                            return;
                        }
                    }
                });
                let watch = cx.spawn(async move |this, cx| {
                    loop {
                        cx.background_executor()
                            .timer(Duration::from_millis(250))
                            .await;
                        match this.update(cx, |view, cx| view.watch_silence(cx)) {
                            Ok(true) => {}
                            _ => return,
                        }
                    }
                });
                self.dictation.tasks = vec![pump, watch];
            }
            Err(problem) => {
                self.dictate(Event::Failed(problem), cx);
            }
        }
    }

    /// Ends a dictation by itself after a pause once something was said, or after a long wait
    /// when nothing was. False once there is nothing to watch.
    fn watch_silence(&mut self, cx: &mut Context<Self>) -> bool {
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
            self.dictate(Event::Stop, cx);
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
