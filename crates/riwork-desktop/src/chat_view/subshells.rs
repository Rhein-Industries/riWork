//! A bounded worker section inside the chat, independent of transcript virtualization.
//! Snapshot installs never focus an editor or modify the chat draft.
use super::*;
use gpui::{AnyElement, SharedString};
use gpui_kit::base::TestSupportExt as _;
use riwork_engine::subshells::ShellCard;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShellAction {
    Open,
    Send(String),
    Stop,
}

#[derive(Default)]
pub(super) struct WorkerUi {
    pub cards: Vec<ShellCard>,
    poll: String,
    collapsed: HashSet<String>,
    pub(super) message: Option<(String, Entity<InputState>, Subscription)>,
    pending: Option<String>,
    error: Option<String>,
}

impl ChatView {
    pub fn set_subshells(&mut self, cards: Vec<ShellCard>, poll: String, cx: &mut Context<Self>) {
        if self.workers.cards == cards && self.workers.poll == poll {
            return;
        }
        for card in &cards {
            if card.finished()
                && self
                    .workers
                    .cards
                    .iter()
                    .find(|old| old.id == card.id)
                    .is_none_or(|old| !old.finished())
            {
                self.workers.collapsed.insert(card.id.clone());
            } else if !card.finished()
                && self
                    .workers
                    .cards
                    .iter()
                    .find(|old| old.id == card.id)
                    .is_some_and(|old| old.finished())
            {
                self.workers.collapsed.remove(&card.id);
            }
        }
        self.workers.cards = cards;
        self.workers.poll = poll;
        cx.notify();
    }

    pub fn subshell_action_finished(
        &mut self,
        id: &str,
        result: Result<(), String>,
        cx: &mut Context<Self>,
    ) {
        if self.workers.pending.as_deref() != Some(id) {
            return;
        }
        self.workers.pending = None;
        match result {
            Ok(()) => {
                if self
                    .workers
                    .message
                    .as_ref()
                    .is_some_and(|(shell, _, _)| shell == id)
                {
                    self.workers.message = None;
                }
                self.workers.error = None;
            }
            Err(error) => self.workers.error = Some(error),
        }
        cx.notify();
    }

    fn worker_action(&mut self, shell_id: String, action: ShellAction, cx: &mut Context<Self>) {
        if !self
            .workers
            .cards
            .iter()
            .any(|card| card.id == shell_id && card.alive)
        {
            return;
        }
        if action != ShellAction::Open {
            if self.workers.pending.is_some() {
                return;
            }
            self.workers.pending = Some(shell_id.clone());
        }
        self.workers.error = None;
        cx.emit(ChatViewEvent::WorkerShell { shell_id, action });
        cx.notify();
    }

    fn message_worker(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.workers.pending.is_some() {
            return;
        }
        let input = text_input::single_line("", "Message this worker…", window, cx);
        let subscription = cx.subscribe_in(&input, window, |view, _, event, _, cx| {
            if text_input::is_submit(event, EnterBehavior::Submit) {
                view.send_worker_message(cx);
            }
        });
        input.read(cx).focus_handle(cx).focus(window, cx);
        self.workers.message = Some((id, input, subscription));
        cx.notify();
    }

    fn send_worker_message(&mut self, cx: &mut Context<Self>) {
        let Some((id, input, _)) = &self.workers.message else {
            return;
        };
        let text = input.read(cx).value().to_string();
        if text.trim().is_empty() {
            return;
        }
        self.worker_action(id.clone(), ShellAction::Send(text), cx);
    }

    pub(super) fn worker_section(
        &self,
        look: Look,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.workers.cards.is_empty() {
            return None;
        }
        Some(
            div()
                .id("chat-workers")
                .flex()
                .flex_col()
                .max_h(ui_text::space(240.0))
                .overflow_y_scroll()
                .px(ui_text::space(20.0))
                .py(ui_text::space(6.0))
                .gap(ui_text::space(4.0))
                .text_size(ui_text::text(11.0))
                .child(
                    div()
                        .id("chat-workers-poll")
                        .text_color(rgb(look.colors.muted))
                        .child(format!("Worker shells · {}", self.workers.poll))
                        .test_support(),
                )
                .children(self.workers.error.as_ref().map(|error| {
                    div()
                        .id("chat-workers-error")
                        .text_color(rgb(look.error()))
                        .child(error.clone())
                        .test_support()
                }))
                .children(
                    self.workers
                        .cards
                        .iter()
                        .map(|card| self.worker_card(card, look, window, cx)),
                )
                .into_any_element(),
        )
    }

    fn worker_card(
        &self,
        card: &ShellCard,
        look: Look,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = !self.workers.collapsed.contains(&card.id);
        let toggle = card.id.clone();
        let focus_id = card.id.clone();
        let message_id = card.id.clone();
        let stop_id = card.id.clone();
        let elapsed = card.elapsed(crate::activity::unix_now());
        let heading = format!(
            "{} · {} · {}m {}s",
            card.title(),
            card.status(),
            elapsed / 60,
            elapsed % 60
        );
        let message = self
            .workers
            .message
            .as_ref()
            .filter(|(id, _, _)| id == &card.id);
        div()
            .id(SharedString::from(format!("worker:{}", card.id)))
            .flex()
            .flex_col()
            .gap(ui_text::space(4.0))
            .p(ui_text::space(6.0))
            .border_1()
            .border_color(rgb(look.colors.divider))
            .rounded(ui_text::space(4.0))
            .child(
                widgets::button(
                    SharedString::from(format!("worker-details:{}", card.id)),
                    heading,
                    None,
                    look,
                )
                .aria_expanded(open)
                .on_click(cx.listener(move |view, _, _, cx| {
                    if !view.workers.collapsed.remove(&toggle) {
                        view.workers.collapsed.insert(toggle.clone());
                    }
                    cx.notify();
                })),
            )
            .when(open, |row| {
                row.child(div().text_color(rgb(look.colors.muted)).child(format!(
                    "{} · {}",
                    card.harness,
                    card.model.as_deref().unwrap_or("Model unavailable")
                )))
                .children(
                    card.tasks
                        .iter()
                        .skip(1)
                        .map(|task| div().child(task.title.clone())),
                )
                .children((!card.output.is_empty()).then(|| {
                    div()
                        .id(SharedString::from(format!("worker-output:{}", card.id)))
                        .font_family(ui_text::code_family())
                        .whitespace_normal()
                        .child(card.output.clone())
                        .test_support()
                }))
                .children(card.output_error.as_ref().map(|error| {
                    div()
                        .text_color(rgb(look.error()))
                        .child(format!("Output unavailable · {error}"))
                }))
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(ui_text::space(6.0))
                    .child(
                        widgets::button(
                            SharedString::from(format!("worker-open:{}", card.id)),
                            "Open shell",
                            None,
                            look,
                        )
                        .disabled(!card.alive)
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.worker_action(focus_id.clone(), ShellAction::Open, cx)
                        })),
                    )
                    .child(
                        widgets::button(
                            SharedString::from(format!("worker-message:{}", card.id)),
                            "Message",
                            None,
                            look,
                        )
                        .disabled(!card.alive || self.workers.pending.is_some())
                        .on_click(cx.listener(
                            move |view, _, window, cx| {
                                view.message_worker(message_id.clone(), window, cx)
                            },
                        )),
                    )
                    .child(
                        widgets::button(
                            SharedString::from(format!("worker-stop:{}", card.id)),
                            "Stop",
                            None,
                            look,
                        )
                        .disabled(!card.alive || self.workers.pending.is_some())
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.worker_action(stop_id.clone(), ShellAction::Stop, cx)
                        })),
                    ),
            )
            .children(message.map(|(_, input, _)| {
                div()
                    .flex()
                    .gap(ui_text::space(6.0))
                    .child(text_input::input("worker-message-input", input, window, cx))
                    .child(
                        widgets::button(
                            "worker-send",
                            "Send message",
                            Some(look.colors.cyan),
                            look,
                        )
                        .disabled(self.workers.pending.is_some())
                        .on_click(cx.listener(|view, _, _, cx| view.send_worker_message(cx))),
                    )
                    .child(
                        widgets::button("worker-cancel-message", "Cancel", None, look).on_click(
                            cx.listener(|view, _, _, cx| {
                                view.workers.message = None;
                                cx.notify();
                            }),
                        ),
                    )
            }))
            .test_support()
            .into_any_element()
    }
}
