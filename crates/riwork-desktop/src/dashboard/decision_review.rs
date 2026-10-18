//! Review exact provider requests from the global inbox. Sending always checks a fresh
//! snapshot and the live host; a model's summary can never supply an approval or answer.
use std::path::{Path, PathBuf};

use gpui::{Context, Entity, EventEmitter, FocusHandle, Render, Window, div, prelude::*, rgb};

use crate::{
    behavior_controls as behavior,
    chat::{
        client::{CallError, Client, socket_path},
        model::{ChatCommand, ChatEvent, ChatState, Decision as Choice},
    },
    controls::{self, Button},
    global_overview::observed::{Decision, Request},
    text_input::{self, InputState},
    theme, ui_text,
};

pub(crate) struct DecisionReview {
    home: PathBuf,
    decision: Decision,
    inputs: Vec<Entity<InputState>>,
    selected: Vec<Vec<bool>>,
    options_focus: Vec<Vec<FocusHandle>>,
    actions_focus: Vec<FocusHandle>,
    close_focus: FocusHandle,
    focus: FocusHandle,
    dialog: gpui_kit::base::DialogHandle,
    return_focus: Option<FocusHandle>,
    busy: bool,
    sent: bool,
    uncertain: bool,
    error: Option<String>,
}
impl EventEmitter<()> for DecisionReview {}

impl DecisionReview {
    pub(crate) fn new(
        home: PathBuf,
        decision: Decision,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let prompts = match &decision.request {
            Request::Question { question } => question.questions.as_slice(),
            _ => &[],
        };
        let inputs: Vec<_> = prompts
            .iter()
            .map(|_| text_input::single_line("", "Or type your answer", window, cx))
            .collect();
        // Typing changes which options are shown chosen.
        for input in &inputs {
            cx.observe(input, |_, _, cx| cx.notify()).detach();
        }
        let selected = prompts
            .iter()
            .map(|prompt| vec![false; prompt.options.len()])
            .collect();
        let options_focus = prompts
            .iter()
            .map(|prompt| prompt.options.iter().map(|_| cx.focus_handle()).collect())
            .collect();
        let actions = match &decision.request {
            Request::Approval { approval } => approval.choices.len(),
            _ => 1,
        };
        Self {
            home,
            decision,
            inputs,
            selected,
            options_focus,
            actions_focus: (0..actions).map(|_| cx.focus_handle()).collect(),
            close_focus: cx.focus_handle(),
            focus: cx.focus_handle(),
            dialog: gpui_kit::base::DialogHandle::new(true),
            return_focus: window.focused(cx),
            busy: false,
            sent: false,
            uncertain: false,
            error: None,
        }
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_focus.focus(window, cx);
    }
    fn close(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        crate::project_settings::close_modal(
            &self.dialog,
            &self.focus,
            &self.return_focus,
            window,
            cx,
        );
        cx.emit(());
    }
    fn answers(&self, cx: &gpui::App) -> Vec<Vec<String>> {
        let Request::Question { question } = &self.decision.request else {
            return vec![];
        };
        question
            .questions
            .iter()
            .enumerate()
            // What the dialog shows, as the phone answers: the options picked, then the text
            // typed; with one choice only, typed text takes the place of the option.
            .map(|(index, prompt)| {
                let typed = self.typed(index, cx);
                let mut answer: Vec<String> = if prompt.multi_select || typed.is_empty() {
                    prompt
                        .options
                        .iter()
                        .enumerate()
                        .filter(|(at, _)| self.selected[index][*at])
                        .map(|(_, option)| option.label.clone())
                        .collect()
                } else {
                    Vec::new()
                };
                if !typed.is_empty() {
                    answer.push(typed);
                }
                answer
            })
            .collect()
    }
    fn typed(&self, index: usize, cx: &gpui::App) -> String {
        self.inputs[index].read(cx).value().trim().to_owned()
    }
    fn send(&mut self, command: ChatCommand, cx: &mut Context<Self>) {
        if self.busy || self.sent || self.uncertain {
            return;
        }
        self.busy = true;
        self.error = None;
        let home = self.home.clone();
        let decision = self.decision.clone();
        let work = cx
            .background_executor()
            .spawn(async move { send_checked(&home, &decision, command) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |view, cx| {
                view.busy = false;
                match result {
                    Ok(()) => view.sent = true,
                    Err(error) => {
                        view.uncertain = error.uncertain;
                        view.error = Some(error.message);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

fn validate(
    expected: &Request,
    controls: &[ChatEvent],
    command: &ChatCommand,
) -> Result<(), String> {
    let matches = controls.iter().any(|event| match (expected, event) {
        (Request::Approval { approval: expected }, ChatEvent::ApprovalRequested { approval }) => {
            expected == approval
        }
        (Request::Question { question: expected }, ChatEvent::QuestionRequested { question }) => {
            expected == question
        }
        _ => false,
    });
    if !matches {
        return Err(
            "This request changed or closed. Refresh the dashboard and review it again.".into(),
        );
    }
    match (expected, command) {
        (
            Request::Approval { approval },
            ChatCommand::Approve {
                request_id,
                decision,
            },
        ) if request_id == &approval.request_id && approval.choices.contains(decision) => Ok(()),
        (
            Request::Question { question },
            ChatCommand::Answer {
                request_id,
                answers,
            },
        ) if request_id == &question.request_id
            && answers.len() == question.questions.len()
            && answers.iter().all(|answer| {
                !answer.is_empty() && answer.iter().all(|text| !text.trim().is_empty())
            }) =>
        {
            Ok(())
        }
        _ => Err("Choose one of the provider's decisions or answer every question.".into()),
    }
}

struct SendFailure {
    message: String,
    uncertain: bool,
}
impl From<String> for SendFailure {
    fn from(message: String) -> Self {
        Self {
            message,
            uncertain: false,
        }
    }
}
impl From<&str> for SendFailure {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}

fn send_checked(home: &Path, expected: &Decision, command: ChatCommand) -> Result<(), SendFailure> {
    // Connecting and listing cannot start a host or resurrect an ended provider session.
    let mut client = Client::connect(&socket_path(home))?;
    let info = client
        .list()?
        .into_iter()
        .find(|chat| chat.id == expected.chat_id)
        .ok_or("This chat is no longer available.")?;
    if info.state != ChatState::Waiting {
        return Err("This chat no longer waits for an answer. Refresh the dashboard.".into());
    }
    let fresh =
        crate::chat::log::read_snapshot(home, &expected.chat_id, None, u64::MAX, 1, 8 << 20, &[])
            .map_err(|error| {
            format!("This chat could not be checked again ({error}). Answer it in the chat.")
        })?;
    validate(&expected.request, &fresh.controls, &command)?;
    // The same id asked again (a restarted provider reuses them) is another request.
    let asked =
        crate::global_overview::observed::open_request(home, &expected.chat_id, &expected.request);
    if asked.as_ref() != Some(&expected.source) {
        return Err(
            "This request changed or closed. Refresh the dashboard and review it again.".into(),
        );
    }
    client.command_checked(&expected.chat_id, command).map_err(|error| match error {
        CallError::Refused(message) => message.into(),
        CallError::Broken(message) => SendFailure { message: format!("The response may have gone through. Check the chat before sending again. {message}"), uncertain: true },
    })
}

impl Render for DecisionReview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme::palette(cx);
        let disabled = self.busy || self.sent || self.uncertain;
        let mut content = div()
            .id("dashboard-decision-content")
            .max_h(ui_text::space(460.0))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(ui_text::space(10.0));
        match self.decision.request.clone() {
            Request::Approval { approval } => {
                content = content
                    .child(
                        div()
                            .text_size(ui_text::text(13.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(approval.title),
                    )
                    .child(div().text_size(ui_text::text(11.0)).child(approval.detail));
                let buttons = approval
                    .choices
                    .into_iter()
                    .enumerate()
                    .map(|(index, choice)| {
                        let label = match choice {
                            Choice::Accept => "Allow",
                            Choice::AcceptForSession => "Allow for session",
                            Choice::Decline => "Deny",
                            Choice::Cancel => "Stop turn",
                        };
                        let request_id = self.decision.request_id.clone();
                        behavior::button(
                            ("dashboard-decision-choice", index),
                            label,
                            if disabled {
                                Button::Disabled
                            } else {
                                Button::Secondary
                            },
                            colors,
                        )
                        .track_focus(&self.actions_focus[index])
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.send(
                                ChatCommand::Approve {
                                    request_id: request_id.clone(),
                                    decision: choice,
                                },
                                cx,
                            )
                        }))
                        .map(|button| {
                            crate::form_input::control_element(
                                ("dashboard-decision-choice", index),
                                button,
                            )
                        })
                    })
                    .collect::<Vec<_>>();
                content = content.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(ui_text::space(8.0))
                        .children(buttons),
                );
            }
            Request::Question { question } => {
                for (index, prompt) in question.questions.into_iter().enumerate() {
                    let mut block = div()
                        .flex()
                        .flex_col()
                        .gap(ui_text::space(6.0))
                        .children(
                            prompt
                                .header
                                .map(|header| div().text_color(rgb(colors.muted)).child(header)),
                        )
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(prompt.question),
                        );
                    if prompt.multi_select {
                        block = block.child(
                            div()
                                .text_color(rgb(colors.muted))
                                .child("Choose one or more, or type an answer."),
                        );
                    }
                    for (option_index, option) in prompt.options.into_iter().enumerate() {
                        let multi = prompt.multi_select;
                        // Typed text replaces a single choice, so it is not shown chosen.
                        let selected = self.selected[index][option_index]
                            && (multi || self.typed(index, cx).is_empty());
                        let button = behavior::button(
                            ("decision-question-option", index * 1024 + option_index),
                            format!("{} {}", if selected { "●" } else { "○" }, option.label),
                            if disabled {
                                Button::Disabled
                            } else if selected {
                                Button::Primary
                            } else {
                                Button::Secondary
                            },
                            colors,
                        )
                        .track_focus(&self.options_focus[index][option_index])
                        .on_click(cx.listener(move |view, _, _, cx| {
                            if view.busy || view.sent {
                                return;
                            }
                            if multi {
                                view.selected[index][option_index] =
                                    !view.selected[index][option_index];
                            } else {
                                view.selected[index].fill(false);
                                view.selected[index][option_index] = true;
                            }
                            cx.notify();
                        }));
                        block = block
                            .child(crate::form_input::control_element(
                                ("decision-question-option", index * 1024 + option_index),
                                button,
                            ))
                            .children((!option.description.is_empty()).then(|| {
                                div()
                                    .text_color(rgb(colors.muted))
                                    .child(option.description)
                            }));
                    }
                    block = block.child(crate::form_input::frame(
                        ("decision-answer", index),
                        &self.inputs[index],
                        disabled,
                        window,
                        cx,
                    ));
                    content = content.child(block);
                }
                content = content.child(
                    behavior::button(
                        "dashboard-send-answer",
                        "Send answer",
                        if disabled {
                            Button::Disabled
                        } else {
                            Button::Primary
                        },
                        colors,
                    )
                    .track_focus(&self.actions_focus[0])
                    .on_click(cx.listener(|view, _, _, cx| {
                        let answers = view.answers(cx);
                        let request_id = view.decision.request_id.clone();
                        if answers.iter().any(Vec::is_empty) {
                            view.error = Some("Answer every question before sending.".into());
                            cx.notify();
                            return;
                        }
                        view.send(
                            ChatCommand::Answer {
                                request_id,
                                answers,
                            },
                            cx,
                        );
                    })),
                );
            }
        }
        let panel = div()
            .id("dashboard-decision-review")
            .role(gpui::Role::Dialog)
            .aria_label("Review decision")
            .occlude()
            .w(ui_text::space(620.0))
            .max_w_full()
            .p(ui_text::space(18.0))
            .flex()
            .flex_col()
            .gap(ui_text::space(12.0))
            .font_family(ui_text::ui_family())
            .text_size(ui_text::text(11.0))
            .text_color(rgb(colors.text))
            .bg(rgb(colors.panel))
            .border_1()
            .border_color(rgb(colors.divider))
            .map(|panel| {
                controls::native(panel, |panel| {
                    panel.rounded(controls::radius(12.0)).shadow_lg()
                })
            })
            .child(div().text_color(rgb(colors.muted)).child(format!(
                    "{} · {} · event {}",
                    self.decision.session.agent,
                    self.decision
                        .session
                        .project_name
                        .as_deref()
                        .unwrap_or("Global"),
                    self.decision.source.event_seq
                )))
            .child(content)
            .children(self.busy.then(|| {
                div()
                    .text_color(rgb(colors.muted))
                    .child("Checking the live request and sending…")
            }))
            .children(self.sent.then(|| {
                div()
                    .text_color(rgb(colors.cyan))
                    .child("Sent. The inbox updates when the provider confirms the answer.")
            }))
            .children(
                self.error
                    .clone()
                    .map(|error| div().text_color(rgb(colors.gold)).child(error)),
            )
            .child(
                behavior::button(
                    "dashboard-review-close",
                    "Close",
                    if self.busy {
                        Button::Disabled
                    } else {
                        Button::Secondary
                    },
                    colors,
                )
                .track_focus(&self.close_focus)
                .on_click(cx.listener(|view, _, window, cx| view.close(window, cx))),
            );
        let can_close = !self.busy;
        let close = cx.listener(|view, _: &gpui::ClickEvent, window, cx| view.close(window, cx));
        gpui_kit::base::Dialog::new(cx)
            .handle(self.dialog.clone())
            .focus_handle(self.focus.clone())
            .close_on_backdrop_press(false)
            .on_ok(|_, _, _| false)
            .on_cancel(move |event, window, cx| {
                if !can_close {
                    return false;
                }
                close(event, window, cx);
                true
            })
            .popup(panel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::model::{Approval, ApprovalKind};
    use gpui_kit::test::TestWindowExt;

    fn request() -> Request {
        Request::Approval {
            approval: Approval {
                request_id: "r".into(),
                item_id: None,
                kind: ApprovalKind::Command,
                title: "git push".into(),
                detail: "Publish".into(),
                choices: vec![Choice::Accept, Choice::Decline],
            },
        }
    }

    fn card(request: Request) -> Decision {
        Decision {
            id: "chat:question:r".into(),
            chat_id: "chat".into(),
            request_id: "r".into(),
            observed_at: 100,
            source: crate::global_overview::observed::Source {
                chat_id: "chat".into(),
                event_seq: 7,
                event_id: "chat:chat:seq:7".into(),
            },
            session: crate::global_overview::SessionSummary {
                target: crate::global_overview::Target::Chat {
                    id: "chat".into(),
                    project_id: None,
                    orchestrator: None,
                },
                agent: "Claude",
                title: Some("Review".into()),
                project_id: None,
                project_name: None,
                branch: None,
                orchestrator: None,
                is_chat: true,
            },
            request,
        }
    }

    #[gpui::test]
    fn question_options_and_typed_answers_keep_exact_provider_labels_without_sending(
        cx: &mut gpui::TestAppContext,
    ) {
        let request: Request = serde_json::from_value(serde_json::json!({"kind":"question","question":{"request_id":"r","questions":[{"question":"Choose","options":[{"label":"Staging, exactly","description":"Test"},{"label":"Production","description":"Live"}],"multi_select":false}]}})).unwrap();
        let (handle, dialog) = crate::form_input::test_window(cx, move |window, cx| {
            DecisionReview::new(
                PathBuf::from("/nonexistent/inert-dashboard-test"),
                card(request),
                window,
                cx,
            )
        });
        crate::form_input::test_turn(cx, handle, |window, cx| {
            assert!(dialog.read(cx).answers(cx).iter().all(Vec::is_empty));
            window.click(("decision-question-option", 0usize), cx);
        });
        crate::form_input::test_turn(cx, handle, |window, cx| {
            assert_eq!(
                dialog.read(cx).answers(cx),
                vec![vec!["Staging, exactly".to_owned()]]
            );
            window.click(("decision-question-option", 1usize), cx);
        });
        crate::form_input::test_turn(cx, handle, |window, cx| {
            assert_eq!(
                dialog.read(cx).answers(cx),
                vec![vec!["Production".to_owned()]]
            );
            window.click(("decision-answer", 0usize), cx);
            window.input("Custom response", cx);
        });
        crate::form_input::test_turn(cx, handle, |_, cx| {
            assert_eq!(
                dialog.read(cx).answers(cx),
                vec![vec!["Custom response".to_owned()]]
            );
            assert!(!dialog.read(cx).busy && !dialog.read(cx).sent);
        });
    }

    #[gpui::test]
    fn several_choices_keep_their_labels_beside_typed_text(cx: &mut gpui::TestAppContext) {
        let request: Request = serde_json::from_value(serde_json::json!({"kind":"question","question":{"request_id":"r","questions":[{"question":"Where?","options":[{"label":"Staging","description":""},{"label":"Production","description":""}],"multi_select":true}]}})).unwrap();
        let (handle, dialog) = crate::form_input::test_window(cx, move |window, cx| {
            DecisionReview::new(
                PathBuf::from("/nonexistent/inert-dashboard-test"),
                card(request),
                window,
                cx,
            )
        });
        crate::form_input::test_turn(cx, handle, |window, cx| {
            window.click(("decision-question-option", 0usize), cx);
        });
        crate::form_input::test_turn(cx, handle, |window, cx| {
            window.click(("decision-question-option", 1usize), cx);
        });
        crate::form_input::test_turn(cx, handle, |window, cx| {
            window.click(("decision-answer", 0usize), cx);
            window.input("also run smoke tests", cx);
        });
        crate::form_input::test_turn(cx, handle, |_, cx| {
            assert_eq!(
                dialog.read(cx).answers(cx),
                vec![vec![
                    "Staging".to_owned(),
                    "Production".to_owned(),
                    "also run smoke tests".to_owned()
                ]]
            );
        });
    }

    #[gpui::test]
    fn escape_does_not_dismiss_an_answer_in_flight(cx: &mut gpui::TestAppContext) {
        let (handle, dialog) = crate::form_input::test_window(cx, |window, cx| {
            DecisionReview::new(
                PathBuf::from("/nonexistent/inert-dashboard-test"),
                card(request()),
                window,
                cx,
            )
        });
        crate::form_input::test_turn(cx, handle, |window, cx| {
            dialog.update(cx, |view, cx| {
                view.busy = true;
                view.focus(window, cx);
                cx.notify();
            });
        });
        crate::form_input::test_turn(cx, handle, |window, cx| {
            window.press("escape", cx);
        });
        crate::form_input::test_turn(cx, handle, |_, cx| {
            assert!(dialog.read(cx).dialog.is_open());
        });
    }
    #[test]
    fn changed_and_resolved_requests_cannot_be_answered_from_old_cards() {
        let expected = request();
        let command = ChatCommand::Approve {
            request_id: "r".into(),
            decision: Choice::Accept,
        };
        assert!(validate(&expected, &[], &command).is_err());
        let Request::Approval { mut approval } = expected.clone() else {
            unreachable!()
        };
        approval.detail = "Changed command".into();
        assert!(
            validate(
                &expected,
                &[ChatEvent::ApprovalRequested { approval }],
                &command
            )
            .is_err()
        );
    }
    #[test]
    fn only_a_current_exact_request_and_an_offered_choice_can_be_sent() {
        let expected = request();
        let Request::Approval { approval } = expected.clone() else {
            unreachable!()
        };
        let controls = [ChatEvent::ApprovalRequested { approval }];
        assert!(
            validate(
                &expected,
                &controls,
                &ChatCommand::Approve {
                    request_id: "r".into(),
                    decision: Choice::Accept
                }
            )
            .is_ok()
        );
        assert!(
            validate(
                &expected,
                &controls,
                &ChatCommand::Approve {
                    request_id: "r".into(),
                    decision: Choice::AcceptForSession
                }
            )
            .is_err()
        );
        assert!(
            validate(
                &expected,
                &controls,
                &ChatCommand::Approve {
                    request_id: "other".into(),
                    decision: Choice::Accept
                }
            )
            .is_err()
        );
    }
    #[test]
    fn a_question_needs_one_non_blank_answer_per_prompt_of_the_unchanged_request() {
        let expected: Request = serde_json::from_value(serde_json::json!({"kind":"question","question":{"request_id":"r","questions":[{"question":"Where?","options":[{"label":"Staging","description":""}],"multi_select":false},{"question":"When?","options":[],"multi_select":false}]}})).unwrap();
        let Request::Question { question } = expected.clone() else {
            unreachable!()
        };
        let controls = [ChatEvent::QuestionRequested {
            question: question.clone(),
        }];
        let answer = |answers: &[&[&str]]| ChatCommand::Answer {
            request_id: "r".into(),
            answers: answers
                .iter()
                .map(|answer| answer.iter().map(|text| (*text).to_owned()).collect())
                .collect(),
        };
        assert!(validate(&expected, &controls, &answer(&[&["Staging"], &["now"]])).is_ok());
        for wrong in [
            answer(&[&["Staging"]]),
            answer(&[&["Staging"], &["  "]]),
            answer(&[&["Staging"], &[]]),
        ] {
            assert!(validate(&expected, &controls, &wrong).is_err());
        }
        let mut changed = question;
        changed.questions[0].options[0].label = "Staging!".into();
        assert!(
            validate(
                &expected,
                &[ChatEvent::QuestionRequested { question: changed }],
                &answer(&[&["Staging"], &["now"]])
            )
            .is_err()
        );
    }
}
