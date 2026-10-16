//! RiWork's workers in this chat's timeline. The host writes a marker item where a worker
//! was started or given work (`chat::workers`); this draws it as a card whose live state
//! (status, elapsed time, the last lines of its terminal) comes from the inventory through
//! `set_subshells`. A running card keeps one height while its output moves, and its live
//! tail is plain text, never part of a selection. Installing new live state never focuses
//! an editor or changes the chat's draft.
use super::*;
use crate::chat::model::Item;
use crate::chat::workers::{self, CardStatus, LiveWorker, WorkerKind, WorkerRef};
use gpui::{AnyElement, SharedString};
use gpui_kit::base::TestSupportExt as _;
use std::collections::BTreeMap;

/// The lines of output a running card shows, padded so its height never moves.
pub(super) const LIVE_LINES: usize = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShellAction {
    Open,
    Send(String),
    Stop,
}

#[derive(Default)]
pub(super) struct WorkerUi {
    /// What the inventory says now about this chat's workers, by session id.
    pub live: BTreeMap<String, LiveWorker>,
    /// Bounded, generated status text, separate from the authoritative inventory state.
    pub(super) summaries: BTreeMap<String, String>,
    pub(super) shelf: super::worker_shelf::ShelfState,
    /// Why that may be old (a failed or paused check), shown on running cards.
    pub(super) stale: Option<String>,
    /// Each session's latest marker by item index: only that card can be live.
    pub(super) latest: HashMap<String, usize>,
    /// Where each running card was last seen against the viewport (above, below), by
    /// item id: a card waiting to be measured again keeps its last known place.
    places: std::cell::RefCell<HashMap<String, (bool, bool)>>,
    /// The worker the bar above the message box writes to, and what it holds.
    pub(super) message: Option<(String, Entity<InputState>, Subscription)>,
    /// The send or stop on its way, and to which worker.
    pub(super) pending: Option<(String, ShellAction)>,
    error: Option<String>,
}

fn elapsed(seconds: u64) -> String {
    format!("{}m {}s", seconds / 60, seconds % 60)
}

impl ChatView {
    pub fn set_subshells(
        &mut self,
        live: BTreeMap<String, LiveWorker>,
        stale: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.workers.live == live && self.workers.stale == stale {
            // Nothing new, but time goes on: running cards count their time, and a card the
            // inventory has not seen may have ended its grace since it was last drawn.
            let now = crate::activity::unix_now();
            let mut running = false;
            let mut starting = HashSet::new();
            for (session, at) in &self.workers.latest {
                let Some(worker) = self.model.transcript.items.get(*at).and_then(workers::of)
                else {
                    continue;
                };
                running |= self.worker_status(*at, worker).running();
                if worker.end.is_none()
                    && !self.workers.live.contains_key(session)
                    && now.saturating_sub(worker.noted_at)
                        <= workers::STARTING_GRACE
                            + riwork_engine::subshells::MAX_INTERVAL.as_secs()
                {
                    starting.insert(session.clone());
                }
            }
            self.remeasure_workers(&starting);
            if running || !starting.is_empty() {
                cx.notify();
            }
            return;
        }
        let changed: HashSet<String> = live
            .iter()
            .filter(|(id, now)| self.workers.live.get(*id) != Some(*now))
            .map(|(id, _)| id.clone())
            .chain(
                self.workers
                    .live
                    .keys()
                    .filter(|id| !live.contains_key(*id))
                    .cloned(),
            )
            .chain(
                (self.workers.stale != stale)
                    .then(|| self.workers.latest.keys().cloned().collect::<Vec<_>>())
                    .into_iter()
                    .flatten(),
            )
            .collect();
        self.workers.live = live;
        self.workers.stale = stale;
        self.remeasure_workers(&changed);
        self.schedule_worker_summaries(cx);
        cx.notify();
    }

    /// Only the cards of these workers are measured again: the rest of the transcript, a
    /// selection in it and where the reader is all stay as they were.
    fn remeasure_workers(&self, sessions: &HashSet<String>) {
        if sessions.is_empty() {
            return;
        }
        let rows: Vec<usize> = self
            .visible
            .iter()
            .enumerate()
            .filter_map(|(row, shown)| match shown {
                display::Row::Item(at) | display::Row::Details(at) => {
                    let worker = workers::of(&self.model.transcript.items[*at])?;
                    sessions.contains(&worker.session_id).then_some(row)
                }
                _ => None,
            })
            .collect();
        for row in rows {
            self.list.remeasure_items(row..row + 1);
        }
    }

    /// Finds each worker's latest card again after the rows changed; a card that is no
    /// longer the latest becomes a one-line "continued below", so it is measured again.
    pub(super) fn reindex_workers(&mut self) {
        let latest: HashMap<String, usize> = workers::latest(&self.model.transcript.items)
            .into_iter()
            .map(|(id, at)| (id.to_owned(), at))
            .collect();
        let superseded: Vec<usize> = self
            .workers
            .latest
            .iter()
            .filter(|(id, at)| latest.get(*id) != Some(*at))
            .map(|(_, at)| *at)
            .collect();
        self.workers.latest = latest;
        self.reindex_provider_workers();
        for at in superseded {
            if let Some(row) = self.visible.iter().position(
                |row| matches!(row, display::Row::Item(i) | display::Row::Details(i) if *i == at),
            ) {
                self.list.remeasure_items(row..row + 1);
            }
        }
    }

    pub(super) fn worker_status(&self, at: usize, worker: &WorkerRef) -> CardStatus {
        CardStatus::of(
            worker,
            self.workers
                .latest
                .get(&worker.session_id)
                .is_some_and(|latest| *latest != at),
            self.workers
                .live
                .get(&worker.session_id)
                .map(|live| live.status),
            crate::activity::unix_now(),
        )
    }

    /// The rows of running cards outside the viewport, first one first, and whether that
    /// one is above it.
    pub(super) fn workers_out_of_view(&self) -> (usize, Option<(usize, bool)>) {
        let mut count = 0;
        let mut first = None;
        for (row, shown) in self.visible.iter().enumerate() {
            let display::Row::Item(at) = shown else {
                continue;
            };
            let Some(worker) = workers::of(&self.model.transcript.items[*at]) else {
                continue;
            };
            if !self.worker_status(*at, worker).running() {
                continue;
            }
            let item = &self.model.transcript.items[*at].id;
            let mut places = self.workers.places.borrow_mut();
            let known = places.get(item).copied();
            let place = match (
                self.list.item_is_above_viewport(row),
                self.list.item_is_below_viewport(row),
            ) {
                (Some(above), Some(below)) => (above, below),
                (Some(true), None) => (true, false),
                // Never drawn: after the top row it is below, unless the list follows
                // its end and draws it next.
                _ => known.unwrap_or_else(|| {
                    let top = self.list.logical_scroll_top().item_ix;
                    (row < top, row > top && !self.list.is_following_tail())
                }),
            };
            places.insert(item.clone(), place);
            let (above, below) = place;
            if above || below {
                count += 1;
                first.get_or_insert((row, above));
            }
        }
        (count, first)
    }

    pub(super) fn workers_chip(&self, look: Look, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (count, first) = self.workers_out_of_view();
        let (row, above) = first?;
        let label = format!(
            "{count} worker{} running {}",
            if count == 1 { "" } else { "s" },
            if above { "↑" } else { "↓" }
        );
        Some(
            widgets::button("chat-workers-running", label, Some(look.colors.cyan), look)
                .bg(rgb(look.colors.panel_active))
                .on_click(cx.listener(move |view, _, _, cx| {
                    view.list.pause_following_tail();
                    view.list.scroll_to_reveal_item(row);
                    cx.notify();
                }))
                .into_any_element(),
        )
    }

    pub fn subshell_action_finished(
        &mut self,
        id: &str,
        result: Result<(), String>,
        cx: &mut Context<Self>,
    ) {
        // An open has nothing pending, and only says something when it failed.
        let pending = self
            .workers
            .pending
            .take_if(|(worker, _)| worker == id)
            .map(|(_, action)| action);
        match result {
            Err(error) => self.workers.error = Some(error),
            Ok(()) if pending.is_none() => return,
            Ok(()) => {
                // A message that went as it reads is done with; one edited while it went,
                // or left unsent by a stop, stays.
                if let Some(ShellAction::Send(sent)) = &pending
                    && self
                        .workers
                        .message
                        .as_ref()
                        .is_some_and(|(shell, input, _)| {
                            shell == id && input.read(cx).value().as_ref() == sent.as_str()
                        })
                {
                    self.workers.message = None;
                }
                self.workers.error = None;
            }
        }
        cx.notify();
    }

    pub(super) fn worker_action(
        &mut self,
        session_id: String,
        action: ShellAction,
        cx: &mut Context<Self>,
    ) {
        let actionable = self
            .workers
            .live
            .get(&session_id)
            .is_some_and(|live| live.actionable);
        // A worker chat can only be opened from its card.
        if !actionable && action != ShellAction::Open {
            return;
        }
        if action != ShellAction::Open {
            if self.workers.pending.is_some() {
                return;
            }
            self.workers.pending = Some((session_id.clone(), action.clone()));
        }
        self.workers.error = None;
        cx.emit(ChatViewEvent::WorkerShell {
            shell_id: session_id,
            action,
        });
        cx.notify();
    }

    pub(super) fn message_worker(
        &mut self,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

    fn worker_title(&self, session_id: &str) -> String {
        self.model
            .transcript
            .items
            .iter()
            .rev()
            .filter_map(workers::of)
            .find(|worker| worker.session_id == session_id)
            .map_or_else(|| "worker".to_owned(), |worker| worker.title.clone())
    }

    /// The bar above the message box that writes to one worker, outside the transcript so
    /// its input stays mounted however far the list scrolls.
    pub(super) fn worker_bar(
        &self,
        look: Look,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.workers.message.is_none() && self.workers.error.is_none() {
            return None;
        }
        let colors = look.colors;
        Some(
            div()
                .id("chat-worker-bar")
                .flex()
                .flex_col()
                .gap(ui_text::space(4.0))
                .px(ui_text::space(20.0))
                .py(ui_text::space(6.0))
                .border_t_1()
                .border_color(rgb(colors.divider))
                .text_size(ui_text::text(11.0))
                .children(self.workers.error.as_ref().map(|error| {
                    div()
                        .flex()
                        .items_center()
                        .gap(ui_text::space(6.0))
                        .child(
                            div()
                                .id("chat-workers-error")
                                .flex_1()
                                .min_w_0()
                                .text_color(rgb(look.error()))
                                .child(error.clone())
                                .test_support(),
                        )
                        .when(self.workers.message.is_none(), |row| {
                            row.child(
                                widgets::button("worker-dismiss-error", "Dismiss", None, look)
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.workers.error = None;
                                        cx.notify();
                                    })),
                            )
                        })
                }))
                .children(self.workers.message.as_ref().map(|(id, input, _)| {
                    div()
                        .flex()
                        .items_center()
                        .gap(ui_text::space(6.0))
                        .child(
                            div()
                                .id("chat-worker-bar-label")
                                .flex_none()
                                .text_color(rgb(colors.muted))
                                .child(format!("To worker · {}", self.worker_title(id)))
                                .test_support(),
                        )
                        .child(div().flex_1().min_w_0().child(text_input::input(
                            "worker-message-input",
                            input,
                            window,
                            cx,
                        )))
                        .child(
                            widgets::button("worker-send", "Send message", Some(colors.cyan), look)
                                .disabled(self.workers.pending.is_some())
                                .on_click(
                                    cx.listener(|view, _, _, cx| view.send_worker_message(cx)),
                                ),
                        )
                        .child(
                            widgets::button("worker-cancel-message", "Cancel", None, look)
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.workers.message = None;
                                    view.workers.error = None;
                                    cx.notify();
                                })),
                        )
                }))
                .test_support()
                .into_any_element(),
        )
    }

    /// The card of a worker marker at item `at`.
    pub(super) fn worker_card(
        &self,
        at: usize,
        item: &Item,
        worker: &WorkerRef,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = look.colors;
        let status = self.worker_status(at, worker);
        let running = status.running();
        let live = self.workers.live.get(&worker.session_id);
        let tail = worker
            .end
            .as_ref()
            .map(|end| end.tail.clone())
            .filter(|tail| !tail.trim().is_empty());
        let expandable = tail.is_some() && !running;
        let open = expandable && self.open.contains(&item.id);
        let badge = cards::Badge {
            label: status.label().into(),
            tone: match status {
                CardStatus::Live(workers::LiveStatus::Waiting) => cards::Tone::Warning,
                CardStatus::Live(workers::LiveStatus::Failed)
                | CardStatus::Finished(workers::WorkerEnd::Failed) => cards::Tone::Error,
                _ if running => cards::Tone::Accent,
                _ => cards::Tone::Muted,
            },
            live: running,
        };
        let kind = if worker.kind == WorkerKind::Chat {
            "Worker chat"
        } else {
            "Worker"
        };
        let title = format!(
            "{}{kind} · {}",
            if worker.follow_up {
                "Follow-up · "
            } else {
                ""
            },
            worker.title
        );
        let key = item.id.clone();
        let header = div()
            .w_full()
            .flex()
            .items_center()
            .gap(ui_text::space(6.0))
            .px(ui_text::space(10.0))
            .py(ui_text::space(6.0))
            .text_size(ui_text::text(11.0))
            .child(widgets::chevron(open, expandable, colors.muted))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(rgb(colors.text))
                    .child(title.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(rgb(colors.muted))
                    .child(worker.assignment.clone().unwrap_or_default()),
            )
            .child(widgets::badge(
                SharedString::from(format!("badge:{}", item.id)),
                &badge,
                look,
            ));
        // Read out as what the card says: the worker, then how it is doing.
        let header = widgets::content_button(
            SharedString::from(format!("worker-details:{}", item.id)),
            format!("{title} · {}", status.label()),
            header,
            look,
        )
        .disabled(!expandable)
        .aria_expanded(open)
        .on_click(cx.listener(move |view, _, _, cx| {
            // As for images: opening a result never pulls the reader to the end.
            let offset = view.list.logical_scroll_top();
            view.list.pause_following_tail();
            view.toggle(&key, Some(at), cx);
            view.list.scroll_to(offset);
        }));
        let now = crate::activity::unix_now();
        // A worker that is still there keeps its actions on its latest card when that
        // card is no longer running: an idle terminal can be given more, a chat opened.
        let actions = !running
            && self.workers.latest.get(&worker.session_id) == Some(&at)
            && (worker.kind == WorkerKind::Chat || live.is_some_and(|live| live.actionable))
            && live.is_some();
        let body = if running {
            Some(self.live_body(item, worker, live, now, look, cx))
        } else {
            let result = open.then(|| {
                let tail = tail.clone().unwrap_or_default();
                div()
                    .id(SharedString::from(format!("worker-result:{}", item.id)))
                    .px(ui_text::space(10.0))
                    .py(ui_text::space(6.0))
                    .flex()
                    .flex_col()
                    .gap(ui_text::space(4.0))
                    .text_size(ui_text::text(11.0))
                    .children(worker.end.as_ref().map(|end| {
                        div().text_color(rgb(colors.muted)).child(format!(
                            "{} after {}",
                            end.status.label(),
                            elapsed(end.ended_at.saturating_sub(worker.noted_at))
                        ))
                    }))
                    .child(
                        div()
                            .font_family(ui_text::code_family())
                            .child(self.selectable(
                                &format!("worker:{}", item.id),
                                tail,
                                Vec::new(),
                                Vec::new(),
                                look,
                                cx,
                            )),
                    )
                    .test_support()
                    .into_any_element()
            });
            (open || actions).then(|| {
                div()
                    .flex()
                    .flex_col()
                    .children(result)
                    .children(actions.then(|| {
                        div()
                            .px(ui_text::space(10.0))
                            .py(ui_text::space(6.0))
                            .child(self.worker_actions(worker, live, look, cx))
                    }))
                    .into_any_element()
            })
        };
        div()
            .id(SharedString::from(format!("worker:{}", item.id)))
            .w_full()
            .rounded(ui_text::space(4.0))
            .border_1()
            .border_color(rgb(colors.divider))
            .bg(rgb(colors.panel))
            .overflow_hidden()
            .child(header)
            .children(body.map(|body| {
                div()
                    .border_t_1()
                    .border_color(rgb(colors.divider))
                    .child(body)
            }))
            .test_support()
            .into_any_element()
    }

    /// A running card's body: one line about the worker, a fixed block of its last lines,
    /// and its actions. Nothing in it changes height.
    fn live_body(
        &self,
        item: &Item,
        worker: &WorkerRef,
        live: Option<&LiveWorker>,
        now: u64,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = look.colors;
        let session = worker.session_id.clone();
        let mut about = vec![
            worker.harness.clone().unwrap_or_else(|| "Worker".into()),
            live.and_then(|live| live.model.clone())
                .or_else(|| worker.model.clone())
                .unwrap_or_else(|| "Model unavailable".into()),
            elapsed(now.saturating_sub(worker.noted_at)),
        ];
        if worker.late {
            about.push("Started earlier · first seen here".into());
        }
        // The terminal sample's freshness; a worker chat's state comes from its host.
        if worker.kind == WorkerKind::Shell
            && let Some(stale) = &self.workers.stale
        {
            about.push(stale.clone());
        }
        let error = live.and_then(|live| live.output_error.clone());
        let output = live.map(|live| live.output.as_str()).unwrap_or_default();
        let mut lines: Vec<String> = output
            .lines()
            .rev()
            .take(LIVE_LINES)
            .map(str::to_owned)
            .collect();
        lines.reverse();
        lines.resize(LIVE_LINES, String::new());
        div()
            .px(ui_text::space(10.0))
            .py(ui_text::space(6.0))
            .flex()
            .flex_col()
            .gap(ui_text::space(4.0))
            .text_size(ui_text::text(11.0))
            .child(
                div()
                    .truncate()
                    .text_color(rgb(if error.is_some() {
                        look.error()
                    } else {
                        colors.muted
                    }))
                    .child(match &error {
                        Some(error) => format!("Output unavailable · {error}"),
                        None => about.join(" · "),
                    }),
            )
            .child(
                div()
                    .id(SharedString::from(format!("worker-output:{}", item.id)))
                    .flex()
                    .flex_col()
                    .font_family(ui_text::code_family())
                    .text_color(rgb(colors.text))
                    .children(lines.into_iter().map(|line| {
                        // A blank line keeps its height, so the block never moves.
                        div().truncate().child(if line.is_empty() {
                            "\u{00a0}".to_owned()
                        } else {
                            line
                        })
                    }))
                    .test_support(),
            )
            .child(self.worker_actions(worker, live, look, cx))
            .into_any_element()
    }

    /// A worker's actions: open it, and for a terminal message or stop it.
    fn worker_actions(
        &self,
        worker: &WorkerRef,
        live: Option<&LiveWorker>,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let session = worker.session_id.clone();
        let actionable = live.is_some_and(|live| live.actionable);
        let chat = worker.kind == WorkerKind::Chat;
        let (open_id, message_id, stop_id) = (session.clone(), session.clone(), session.clone());
        div()
            .flex()
            .items_center()
            .gap(ui_text::space(6.0))
            .child(
                widgets::button(
                    SharedString::from(format!("worker-open:{session}")),
                    if chat { "Open chat" } else { "Open shell" },
                    None,
                    look,
                )
                .disabled(!actionable && !chat)
                .on_click(cx.listener(move |view, _, _, cx| {
                    view.worker_action(open_id.clone(), ShellAction::Open, cx)
                })),
            )
            .when(!chat, |row| {
                row.child(
                    widgets::button(
                        SharedString::from(format!("worker-message:{session}")),
                        "Message",
                        None,
                        look,
                    )
                    .disabled(!actionable || self.workers.pending.is_some())
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.message_worker(message_id.clone(), window, cx)
                    })),
                )
                .child(
                    widgets::button(
                        SharedString::from(format!("worker-stop:{session}")),
                        "Stop",
                        None,
                        look,
                    )
                    .disabled(!actionable || self.workers.pending.is_some())
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.worker_action(stop_id.clone(), ShellAction::Stop, cx)
                    })),
                )
            })
            .into_any_element()
    }
}
