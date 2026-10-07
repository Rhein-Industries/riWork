//! Persistent Kit editors and application policy. Kit alone edits their text/selection.
use super::{
    ChatView,
    attachment_draft::{Draft, Status, Submission},
    notices, panels,
};
use crate::{
    chat::{
        client::CallError,
        model::{ChatCommand, NoticeLevel, Question},
    },
    text_input::{self, EnterBehavior, InputEvent, InputState, TextareaState},
};
use gpui::{Context, Entity, EntityInputHandler, Focusable, Subscription, Window};
use gpui_kit::base::input::{Enter, Escape};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct AnswerKey {
    pub request: String,
    pub prompt: usize,
    // Providers currently expose a prompt index, not a prompt id. Include its text so
    // a changed prompt at the same position cannot inherit a different answer.
    pub question: String,
}
impl AnswerKey {
    pub fn of(question: &Question, prompt: usize) -> Self {
        Self {
            request: question.request_id.clone(),
            prompt,
            question: question.questions[prompt].question.clone(),
        }
    }
    fn still_in(&self, question: &Question) -> bool {
        self.request == question.request_id
            && question
                .questions
                .get(self.prompt)
                .is_some_and(|p| p.question == self.question)
    }
}
pub(super) struct AnswerEditor {
    pub state: Entity<InputState>,
    pub _subscription: Subscription,
}

#[derive(Clone)]
pub(super) struct AnswerSnapshot {
    pub question: Question,
    pub command: ChatCommand,
}
impl AnswerSnapshot {
    fn matches_question(&self, question: &Question) -> bool {
        self.question.request_id == question.request_id
            && self.question.questions.len() == question.questions.len()
            && self
                .question
                .questions
                .iter()
                .zip(&question.questions)
                .all(|(old, new)| {
                    old.question == new.question
                        && old.multi_select == new.multi_select
                        && old
                            .options
                            .iter()
                            .map(|o| &o.label)
                            .eq(new.options.iter().map(|o| &o.label))
                })
    }
}
#[derive(Clone)]
pub(super) struct AnswerFailure {
    pub snapshot: AnswerSnapshot,
    pub error: CallError,
}

impl ChatView {
    pub(super) fn composer_text(&self, cx: &gpui::App) -> String {
        self.composer.read(cx).value().to_string()
    }
    pub(super) fn draft_empty(&self, cx: &gpui::App) -> bool {
        self.composer.read(cx).value().trim().is_empty() && self.attachments.is_empty()
    }
    fn current_draft(&self, cx: &gpui::App) -> Draft {
        Draft {
            text: self.composer_text(cx),
            attachments: self
                .attachments
                .iter()
                .filter_map(|chip| chip.attachment().cloned())
                .collect(),
        }
    }
    /// Bring back what the chat's message box held when its tab closed or the app quit
    /// (`chat_drafts`), unless something was typed meanwhile.
    pub(super) fn restore_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = self
            .chat_id
            .as_deref()
            .and_then(|id| crate::chat_drafts::draft(id, cx))
        else {
            return;
        };
        if self.composer_text(cx).is_empty() {
            self.composer
                .update(cx, |state, cx| state.set_value(text, window, cx));
            self.bump_generation();
        }
    }
    /// Keep the message box's text as the chat's draft. `set_value` sends no Change, so
    /// the code that sets the composer calls this too.
    pub(super) fn remember_draft(&self, cx: &mut Context<Self>) {
        if let Some(id) = self.chat_id.clone() {
            let text = self.composer_text(cx);
            crate::chat_drafts::remember(&id, &text, cx);
        }
    }
    pub(super) fn bump_generation(&mut self) {
        self.editor_generation = self
            .editor_generation
            .checked_add(1)
            .expect("editor generation exhausted");
    }

    pub(super) fn composer_event(
        &mut self,
        _: &Entity<TextareaState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                self.bump_generation();
                if self.programmatic_changes > 0 {
                    self.programmatic_changes -= 1;
                } else {
                    self.dictation.user_edited();
                }
                self.remember_draft(cx);
                cx.notify();
            }
            InputEvent::Focus => {
                gpui_kit::base::TextSelection::clear(window, cx);
                cx.notify();
            }
            _ if text_input::is_submit(event, EnterBehavior::Submit) => {
                if !self.accepts_input() || self.enter_repeated {
                    return;
                }
                let offered = if self.enter_repeated
                    || self
                        .sent_at
                        .is_some_and(|at| at.elapsed() < Duration::from_millis(700))
                {
                    Vec::new()
                } else {
                    self.offered()
                };
                match super::composer::enter(false, false, false, self.draft_empty(cx), &offered) {
                    super::composer::Enter::Approve(decision) => {
                        if let Some((approval, _)) = self.pending_approval() {
                            self.approve(approval.request_id.clone(), decision, cx);
                        }
                    }
                    super::composer::Enter::Send => self.send_message(window, cx),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    pub(super) fn model_event(
        &mut self,
        state: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(event, InputEvent::Focus) {
            gpui_kit::base::TextSelection::clear(window, cx);
        }
        if text_input::is_submit(event, EnterBehavior::Submit)
            && self.menu == Some(super::Menu::Model)
            && self.model.transcript.models.is_empty()
        {
            let model = state.read(cx).value().trim().to_owned();
            if !model.is_empty() {
                self.configure(Some(model), None, None, None, cx);
            }
        }
    }
    /// Observe Enter only to distinguish a held key from a new approval gesture.
    /// Submission remains in the one InputEvent subscription. KeyUp rearms this guard.
    pub(super) fn capture_enter(&mut self, _: &Enter, _: &mut Window, _: &mut Context<Self>) {
        self.enter_repeated = self.enter_down;
        self.enter_down = true;
    }
    pub(super) fn escape_action(
        &mut self,
        _: &Escape,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let repeated = self.escape_down;
        self.escape_down = true;
        // Composition confirmation/cancellation stays with the active native handler.
        let composing = if self.composer.read(cx).focus_handle(cx).is_focused(window) {
            self.composer.update(cx, |state, cx| {
                state.marked_text_range(window, cx).is_some()
            })
        } else if self
            .model_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window)
        {
            self.model_input.update(cx, |state, cx| {
                state.marked_text_range(window, cx).is_some()
            })
        } else {
            let active = self
                .answers
                .values()
                .find(|e| e.state.read(cx).focus_handle(cx).is_focused(window))
                .map(|e| e.state.clone());
            active.is_some_and(|state| {
                state.update(cx, |state, cx| {
                    state.marked_text_range(window, cx).is_some()
                })
            })
        };
        if composing {
            return;
        }
        let handled = if self.media.viewer.take().is_some() {
            true
        } else if self.menu.is_some() {
            self.close_menu(cx);
            true
        } else if self.dictation.phase().is_active() {
            self.cancel_dictation(window, cx);
            true
        } else if self.composer.read(cx).focus_handle(cx).is_focused(window) {
            match super::composer::escape(
                self.draft_empty(cx),
                &if repeated
                    || self
                        .sent_at
                        .is_some_and(|at| at.elapsed() < Duration::from_millis(700))
                {
                    Vec::new()
                } else {
                    self.offered()
                },
            ) {
                Some(decision) => {
                    if let Some((approval, _)) = self.pending_approval() {
                        self.approve(approval.request_id.clone(), decision, cx);
                    }
                    true
                }
                None => false,
            }
        } else {
            false
        };
        if handled {
            cx.stop_propagation();
            cx.notify();
        }
    }

    pub(super) fn send_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.accepts_input() || self.pending_submission.is_some() {
            return;
        }
        if self.dictation.phase().is_active() {
            self.notices.set(
                notices::LocalKey::Send,
                NoticeLevel::Warning,
                "Finish dictation before sending the draft.",
            );
            cx.notify();
            return;
        }
        if self
            .attachments
            .iter()
            .any(|chip| chip.attachment().is_none())
        {
            self.notices.set(
                notices::LocalKey::Send,
                NoticeLevel::Warning,
                "Wait for staging, or remove the attachment with an error before sending.",
            );
            cx.notify();
            return;
        }
        if self.submissions.iter().any(|s| {
            s.editor == self.composer.entity_id().as_u64()
                && s.generation == self.editor_generation
                && matches!(s.status, Status::Refused(_) | Status::Uncertain(_))
        }) {
            self.notices.set(notices::LocalKey::Send, NoticeLevel::Warning, "Review the saved submission below. Inspect the transcript before explicitly resending.");
            cx.notify();
            return;
        }
        self.dispatch_draft(
            self.current_draft(cx),
            self.composer.entity_id().as_u64(),
            self.editor_generation,
            window,
            cx,
        );
    }
    fn dispatch_draft(
        &mut self,
        draft: Draft,
        editor: u64,
        generation: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending_submission.is_some() || !self.accepts_input() {
            return;
        }
        self.next_submission = self
            .next_submission
            .checked_add(1)
            .expect("submission identity exhausted");
        let id = self.next_submission;
        let Ok(mut submission) = Submission::begin(id, editor, generation, draft) else {
            return;
        };
        let command = submission
            .command()
            .expect("a new snapshot is undispatched");
        self.submissions.push(submission);
        self.pending_submission = Some(id);
        self.sent_at = Some(Instant::now());
        // What went wrong with the last draft is over once a new one goes out.
        self.notices.clear(notices::LocalKey::Send);
        self.notices.clear(notices::LocalKey::Attachment);
        self.list.set_follow_mode(gpui::FollowMode::Tail);
        let result = self
            .feed
            .as_ref()
            .ok_or_else(|| CallError::Refused("Not connected to the chat yet.".into()))
            .and_then(|feed| feed.submit(id, command.clone()));
        if let Err(error) = result {
            self.submission_receipt(id, command, Err(error), window, cx);
        }
        cx.notify();
    }
    pub(super) fn resend_snapshot(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(saved) = self
            .submissions
            .iter()
            .find(|s| s.id == id && matches!(s.status, Status::Refused(_) | Status::Uncertain(_)))
            .cloned()
        else {
            return;
        };
        // Retrying an old snapshot cannot clear a newer composer generation.
        self.dispatch_draft(saved.snapshot, saved.editor, saved.generation, window, cx);
    }
    pub(super) fn restore_snapshot(
        &mut self,
        id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending_submission.is_some() || self.dictation.phase().is_active() {
            return;
        }
        let Some(saved) = self.submissions.iter().find(|s| s.id == id).cloned() else {
            return;
        };
        // This is an explicit Replace draft action, never automatic error recovery.
        self.composer.update(cx, |state, cx| {
            state.set_value(saved.snapshot.text, window, cx)
        });
        self.remember_draft(cx);
        self.release_attachment_previews(cx);
        self.attachments = saved
            .snapshot
            .attachments
            .into_iter()
            .map(super::attachment_ui::Chip::ready)
            .collect();
        self.bump_generation();
        self.focus(window, cx);
        cx.notify();
    }
    pub(super) fn submission_receipt(
        &mut self,
        id: u64,
        command: ChatCommand,
        result: Result<(), CallError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(saved) = self.answer_submissions.get(&id).cloned() {
            if command != saved.command {
                return;
            }
            let request_id = saved.question.request_id.clone();
            self.answer_submissions.remove(&id);
            if !self
                .model
                .transcript
                .questions
                .iter()
                .any(|q| saved.matches_question(q))
                || self
                    .answer_submissions
                    .values()
                    .any(|s| s.question.request_id == request_id)
            {
                return;
            }
            if let Err(error) = result {
                self.answered.remove(&request_id);
                self.answer_failures.insert(
                    request_id,
                    AnswerFailure {
                        snapshot: saved,
                        error: error.clone(),
                    },
                );
                self.notices.set(
                    notices::LocalKey::Answer,
                    NoticeLevel::Error,
                    format!("Answer submission: {error}"),
                );
            }
            // Answers and choices are retained until RequestResolved, even on success.
            cx.notify();
            return;
        }
        let Some(submission) = self.submissions.iter_mut().find(|s| s.id == id) else {
            return;
        };
        if submission.status != Status::Pending || command != submission.expected_command() {
            return;
        }
        let clear = submission.settle(
            self.composer.entity_id().as_u64(),
            self.editor_generation,
            result.clone(),
        );
        if self.pending_submission == Some(id) {
            self.pending_submission = None;
        }
        if clear {
            self.composer
                .update(cx, |state, cx| state.set_value("", window, cx));
            self.remember_draft(cx);
            self.attachments.clear();
            self.bump_generation();
        }
        if let Err(error) = result {
            self.notices.set(notices::LocalKey::Send, NoticeLevel::Error, match error {
                CallError::Refused(error) => {
                    format!("Submission refused: {error}. Your draft is retained.")
                }
                CallError::Broken(error) => format!(
                    "Submission outcome uncertain: {error}. Inspect the transcript before explicitly resending. Your draft is retained."
                ),
            });
        }
        self.submissions.retain(|s| s.status != Status::Submitted);
        cx.notify();
    }

    pub(super) fn ready_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let questions = self.model.transcript.questions.clone();
        for saved in self.answer_submissions.values() {
            if !questions.iter().any(|q| saved.matches_question(q))
                && !self.answer_submissions.values().any(|current| {
                    current.question.request_id == saved.question.request_id
                        && questions.iter().any(|q| current.matches_question(q))
                })
            {
                self.answered.remove(&saved.question.request_id);
            }
        }
        self.answers
            .retain(|key, _| questions.iter().any(|q| key.still_in(q)));
        self.answer_failures.retain(|_, failure| {
            questions
                .iter()
                .any(|q| failure.snapshot.matches_question(q))
        });
        for question in questions {
            for at in 0..question.questions.len() {
                let key = AnswerKey::of(&question, at);
                if !self.answers.contains_key(&key) {
                    let state = text_input::single_line("", "or type an answer", window, cx);
                    let request_key = key.clone();
                    let subscription =
                        cx.subscribe_in(&state, window, move |view, _, event, window, cx| {
                            if matches!(event, InputEvent::Focus) {
                                gpui_kit::base::TextSelection::clear(window, cx);
                            } else if text_input::is_submit(event, EnterBehavior::Submit)
                                && view
                                    .pending_question()
                                    .is_some_and(|q| request_key.still_in(q))
                            {
                                view.submit_answers(cx);
                            } else if matches!(event, InputEvent::Change) {
                                cx.notify();
                            }
                        });
                    self.answers.insert(
                        key.clone(),
                        AnswerEditor {
                            state,
                            _subscription: subscription,
                        },
                    );
                }
                let state = &self.answers[&key].state;
                let disabled = self.answered.contains(&question.request_id);
                state.update(cx, |state, cx| {
                    if state.presentation().is_disabled() != disabled {
                        state.set_disabled(disabled, cx);
                    }
                });
            }
            let draft = self.drafts.entry(question.request_id.clone()).or_default();
            draft.picked.resize_with(question.questions.len(), Vec::new);
            // Keep choices on identical refreshes, but don't transfer an old option index
            // into a replaced/reordered prompt. Free text has its own stable identity.
            for (at, prompt) in question.questions.iter().enumerate() {
                if draft.prompts.get(at).is_some_and(|old| {
                    old.question != prompt.question
                        || old.multi_select != prompt.multi_select
                        || old
                            .options
                            .iter()
                            .map(|o| &o.label)
                            .ne(prompt.options.iter().map(|o| &o.label))
                }) {
                    draft.picked[at].clear();
                }
            }
            draft.prompts = question.questions;
        }
        let disabled = !self.accepts_input();
        self.composer.update(cx, |state, cx| {
            if state.presentation().is_disabled() != disabled {
                state.set_disabled(disabled, cx);
            }
        });
    }
    pub(super) fn typed_answers(&self, question: &Question, cx: &gpui::App) -> Vec<String> {
        (0..question.questions.len())
            .map(|at| {
                self.answers
                    .get(&AnswerKey::of(question, at))
                    .map(|e| e.state.read(cx).value().to_string())
                    .unwrap_or_default()
            })
            .collect()
    }
    pub(super) fn submit_answers(&mut self, cx: &mut Context<Self>) {
        let Some(question) = self.pending_question() else {
            return;
        };
        let request_id = question.request_id.clone();
        if self.answered.contains(&request_id) {
            return;
        }
        let answers = panels::answers_of(
            question,
            self.drafts.get(&request_id),
            &self.typed_answers(question, cx),
        );
        if answers.iter().any(Vec::is_empty) {
            self.notices.set(
                notices::LocalKey::Answer,
                NoticeLevel::Warning,
                "Answer every question first.",
            );
            cx.notify();
            return;
        }
        let command = ChatCommand::Answer {
            request_id: request_id.clone(),
            answers,
        };
        if self
            .answer_failures
            .get(&request_id)
            .is_some_and(|failure| {
                failure.snapshot.command == command && matches!(failure.error, CallError::Broken(_))
            })
        {
            self.notices.set(
                notices::LocalKey::Answer,
                NoticeLevel::Warning,
                "Inspect the transcript before explicitly resending the saved answers.",
            );
            cx.notify();
            return;
        }
        self.dispatch_answers(request_id, command, cx);
    }
    fn dispatch_answers(&mut self, request: String, command: ChatCommand, cx: &mut Context<Self>) {
        let Some(question) = self
            .pending_question()
            .filter(|q| q.request_id == request)
            .cloned()
        else {
            return;
        };
        let snapshot = AnswerSnapshot {
            question,
            command: command.clone(),
        };
        self.next_submission = self
            .next_submission
            .checked_add(1)
            .expect("submission identity exhausted");
        let id = self.next_submission;
        let result = self
            .feed
            .as_ref()
            .ok_or_else(|| CallError::Refused("Not connected to the chat yet.".into()))
            .and_then(|feed| feed.submit(id, command.clone()));
        match result {
            Ok(()) => {
                self.notices.clear(notices::LocalKey::Answer);
                self.answered.insert(request.clone());
                self.answer_submissions.insert(id, snapshot);
                self.answer_failures.remove(&request);
            }
            Err(error) => {
                self.notices.set(
                    notices::LocalKey::Answer,
                    NoticeLevel::Error,
                    error.to_string(),
                );
                self.answer_failures
                    .insert(request, AnswerFailure { snapshot, error });
            }
        }
        cx.notify();
    }
    pub(super) fn resend_answers(&mut self, request: &str, cx: &mut Context<Self>) {
        if self
            .pending_question()
            .is_none_or(|q| q.request_id != request)
            || self.answered.contains(request)
        {
            return;
        }
        if let Some(failure) = self.answer_failures.get(request).cloned() {
            if self
                .pending_question()
                .is_some_and(|q| failure.snapshot.matches_question(q))
            {
                self.dispatch_answers(request.into(), failure.snapshot.command, cx);
            }
        }
    }
}
