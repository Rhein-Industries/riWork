//! A small, persistent view of this chat's workers, outside the scrolling transcript.
//! Process/provider reports own the badges. Generated text can explain the work, but
//! cannot turn a running worker into a completed one or authorize an action.
use super::*;
use crate::chat::{
    subagents::{self, RunStatus},
    worker_summary::Snapshot,
    workers::{self, CardStatus, LiveStatus, WorkerKind},
};
use gpui::{AnyElement, SharedString};
use gpui_kit::base::TestSupportExt as _;
use std::collections::BTreeMap;

const COMPACT_ROWS: usize = 2;
const RECENT_SECONDS: u64 = 5 * 60;

#[derive(Default)]
pub(super) struct ShelfState {
    pub(super) expanded: bool,
    pub(super) selected: Option<String>,
    revision: Option<(u64, Option<Provider>)>,
    providers: Vec<ProviderWorker>,
}

struct ProviderWorker {
    id: String,
    title: String,
    assignment: String,
    output: String,
    status: RunStatus,
    at: usize,
}

pub(super) struct ShelfWorker {
    pub(super) id: String,
    pub(super) title: String,
    pub(super) assignment: String,
    pub(super) output: String,
    pub(super) label: &'static str,
    pub(super) active: bool,
    priority: u8,
    tone: cards::Tone,
    at: usize,
    session: Option<String>,
    detail: Option<String>,
}

/// Collapse whitespace on a character boundary; terminal prompts and empty decoration do
/// not replace a useful assignment when there is no substantive output yet.
fn compressed(text: &str, limit: usize) -> String {
    let mut compact = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .take(limit + 1)
        .collect::<String>();
    if compact.chars().count() > limit {
        compact = compact.chars().take(limit.saturating_sub(1)).collect();
        compact.push('…');
    }
    compact
}

fn snippet(output: &str, assignment: &str) -> String {
    output
        .lines()
        .rev()
        .find(|line| line.chars().filter(|c| c.is_alphanumeric()).count() >= 3)
        .map(|line| compressed(line, 180))
        .filter(|line| !line.is_empty())
        .unwrap_or_else(|| compressed(assignment, 180))
}

fn provider_state(status: RunStatus) -> (&'static str, bool, u8, cards::Tone) {
    match status {
        RunStatus::Failed => ("Failed", false, 0, cards::Tone::Error),
        RunStatus::Declined => ("Declined", false, 0, cards::Tone::Warning),
        RunStatus::Running => ("Working", true, 2, cards::Tone::Accent),
        RunStatus::Unknown => ("Unreported", false, 3, cards::Tone::Muted),
        RunStatus::Done => ("Done", false, 4, cards::Tone::Muted),
        RunStatus::Interrupted => ("Interrupted", false, 4, cards::Tone::Muted),
    }
}

impl ChatView {
    /// Parse each delegation once per transcript revision. Child IDs, and the event order
    /// of replacements, join spawn/send/wait reports without duplicating the child.
    pub(super) fn reindex_provider_workers(&mut self) {
        let provider = self.info().map(|info| info.provider);
        let revision = (self.model.transcript.revision(), provider);
        if self.workers.shelf.revision == Some(revision) {
            return;
        }
        let mut reports = self
            .model
            .transcript
            .items
            .iter()
            .enumerate()
            .filter_map(|(at, item)| subagents::of(item, provider).map(|run| (at, item, run)))
            .collect::<Vec<_>>();
        reports.sort_by_key(|(_, item, _)| self.model.transcript.replaced_at(&item.id));
        let mut by_id = BTreeMap::<String, ProviderWorker>::new();
        for (at, item, run) in reports {
            if run.children.is_empty() {
                // Codex operations without a reported child do not identify a worker.
                if provider == Some(Provider::Codex) {
                    continue;
                }
                let id = format!("subagent:item:{}", item.id);
                by_id.insert(
                    id.clone(),
                    ProviderWorker {
                        id,
                        title: run.name,
                        assignment: run.purpose.unwrap_or_default(),
                        output: run.summary.unwrap_or_default(),
                        status: run.status,
                        at,
                    },
                );
                continue;
            }
            for child in run.children {
                let id = format!("subagent:{}", child.id);
                let row = by_id.entry(id.clone()).or_insert_with(|| ProviderWorker {
                    id,
                    title: format!("Subagent · {}", compressed(&child.id, 22)),
                    assignment: String::new(),
                    output: String::new(),
                    status: child.status,
                    at,
                });
                if let Some(purpose) = &run.purpose {
                    row.assignment = purpose.clone();
                    row.title = compressed(purpose, 80);
                }
                if child.status == RunStatus::Running && row.status != RunStatus::Running {
                    row.output.clear();
                }
                row.status = child.status;
                row.at = at;
                if let Some(summary) = child.summary {
                    row.output = summary;
                }
            }
        }
        self.workers.shelf.providers = by_id.into_values().collect();
        self.workers.shelf.revision = Some(revision);
    }

    pub(super) fn shelf_workers(&self) -> Vec<ShelfWorker> {
        let now = crate::activity::unix_now();
        let turn = self.model.transcript.turn_id.as_deref().or_else(|| {
            self.model
                .transcript
                .items
                .iter()
                .rev()
                .find_map(|item| item.turn_id.as_deref())
        });
        let in_recent_turn = |at: usize| {
            turn.is_some() && self.model.transcript.items[at].turn_id.as_deref() == turn
        };
        let mut rows = Vec::new();
        for (session, at) in &self.workers.latest {
            let Some(worker) = self.model.transcript.items.get(*at).and_then(workers::of) else {
                continue;
            };
            let live = self.workers.live.get(session);
            // Unlike historical cards, a persistent shelf always describes the inventory
            // now, even when the last recorded assignment had already finished.
            let status = live.map_or_else(
                || self.worker_status(*at, worker),
                |live| CardStatus::Live(live.status),
            );
            let active = status.running();
            let ended = worker
                .end
                .as_ref()
                .map_or(worker.noted_at, |end| end.ended_at);
            if !active && !in_recent_turn(*at) && now.saturating_sub(ended) > RECENT_SECONDS {
                continue;
            }
            let (label, priority, tone) = match status {
                CardStatus::Live(LiveStatus::Waiting) => ("Waiting", 1, cards::Tone::Warning),
                CardStatus::Live(LiveStatus::Failed)
                | CardStatus::Finished(workers::WorkerEnd::Failed) => {
                    ("Failed", 0, cards::Tone::Error)
                }
                CardStatus::Starting => ("Starting", 2, cards::Tone::Accent),
                CardStatus::Live(LiveStatus::Working) => ("Working", 2, cards::Tone::Accent),
                CardStatus::Live(LiveStatus::Unknown) => ("Unreported", 3, cards::Tone::Muted),
                _ => (status.label(), 4, cards::Tone::Muted),
            };
            let output = live
                .map(|live| live.output.clone())
                .filter(|output| !output.trim().is_empty())
                .or_else(|| {
                    (!active)
                        .then(|| worker.end.as_ref().map(|end| end.tail.clone()))
                        .flatten()
                })
                .unwrap_or_default();
            let detail = live
                .and_then(|live| live.output_error.as_ref())
                .map(|error| format!("Output unavailable · {error}"))
                .or_else(|| {
                    (worker.kind == WorkerKind::Shell)
                        .then(|| self.workers.stale.clone())
                        .flatten()
                });
            rows.push(ShelfWorker {
                id: format!("worker:{session}"),
                title: if worker.title.trim().is_empty() {
                    if worker.kind == WorkerKind::Chat {
                        "Worker chat"
                    } else {
                        "Subshell"
                    }
                    .into()
                } else {
                    worker.title.clone()
                },
                assignment: worker.assignment.clone().unwrap_or_default(),
                output,
                label,
                active,
                priority,
                tone,
                at: *at,
                session: Some(session.clone()),
                detail,
            });
        }
        for worker in &self.workers.shelf.providers {
            let (label, active, priority, tone) = provider_state(worker.status);
            if !active && !in_recent_turn(worker.at) {
                continue;
            }
            rows.push(ShelfWorker {
                id: worker.id.clone(),
                title: worker.title.clone(),
                assignment: worker.assignment.clone(),
                output: worker.output.clone(),
                label,
                active,
                priority,
                tone,
                at: worker.at,
                session: None,
                detail: None,
            });
        }
        rows.sort_by(|left, right| {
            left.priority
                .cmp(&right.priority)
                .then_with(|| right.at.cmp(&left.at))
                .then_with(|| left.id.cmp(&right.id))
        });
        rows
    }

    pub(super) fn worker_summary_sources(&self) -> Vec<Snapshot> {
        self.shelf_workers()
            .into_iter()
            .filter(|worker| !worker.output.trim().is_empty())
            .map(|worker| Snapshot {
                id: worker.id,
                assignment: (!worker.assignment.is_empty()).then_some(worker.assignment),
                output: worker.output,
            })
            .collect()
    }

    pub(super) fn worker_shelf(&self, look: Look, cx: &mut Context<Self>) -> Option<AnyElement> {
        let workers = self.shelf_workers();
        if workers.is_empty() {
            return None;
        }
        let active = workers.iter().filter(|worker| worker.active).count();
        let waiting = workers
            .iter()
            .filter(|worker| worker.label == "Waiting")
            .count();
        let header = if waiting > 0 {
            format!("Workers · {active} active · {waiting} waiting")
        } else {
            format!("Workers · {active} active · {} total", workers.len())
        };
        let expanded = self.workers.shelf.expanded;
        let selected = self.workers.shelf.selected.as_ref();
        let limit = if expanded {
            workers.len()
        } else {
            COMPACT_ROWS
        };
        let colors = look.colors;
        let mut body = div()
            .id("chat-worker-shelf-list")
            .flex()
            .flex_col()
            .min_w_0()
            .max_h(ui_text::space(if expanded { 208.0 } else { 168.0 }))
            .overflow_y_scroll();
        for worker in workers.iter().take(limit) {
            let open = selected == Some(&worker.id);
            let key = worker.id.clone();
            let generated = self.workers.summaries.contains_key(&worker.id);
            let summary = self
                .workers
                .summaries
                .get(&worker.id)
                .map(|summary| compressed(summary, 240))
                .unwrap_or_else(|| snippet(&worker.output, &worker.assignment));
            let summary = if summary.is_empty() {
                "Waiting for worker output…".into()
            } else {
                summary
            };
            let badge = cards::Badge {
                label: worker.label.into(),
                tone: worker.tone,
                live: worker.active,
            };
            let row = div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .px(ui_text::space(8.0))
                .py(ui_text::space(3.0))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(ui_text::space(6.0))
                        .child(widgets::chevron(open, true, colors.muted))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(rgb(colors.text))
                                .child(worker.title.clone()),
                        )
                        .child(widgets::badge(
                            SharedString::from(format!("shelf-badge:{}", worker.id)),
                            &badge,
                            look,
                        )),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("shelf-summary:{}", worker.id)))
                        .min_w_0()
                        .truncate()
                        .text_color(rgb(colors.muted))
                        .child(summary.clone())
                        .when(generated, |summary| {
                            summary.child(crate::tooltip::anchor(
                        "AI summary of recent output; lifecycle status comes from the provider.",
                        crate::tooltip::Look::Control,
                    ))
                        })
                        .test_support(),
                );
            body = body.child(
                widgets::content_button(
                    SharedString::from(format!("shelf-worker:{}", worker.id)),
                    format!("{} · {} · {summary}", worker.title, worker.label),
                    row,
                    look,
                )
                .aria_expanded(open)
                .hover(move |style| style.bg(rgb(colors.panel_active)))
                .on_click(cx.listener(move |view, _, _, cx| {
                    if view.workers.shelf.selected.as_ref() == Some(&key) {
                        view.workers.shelf.selected = None;
                    } else {
                        view.workers.shelf.selected = Some(key.clone());
                    }
                    cx.notify();
                })),
            );
            if open {
                let at = worker.at;
                let mut detail = div()
                    .id(SharedString::from(format!("shelf-detail:{}", worker.id)))
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(ui_text::space(4.0))
                    .px(ui_text::space(8.0))
                    .pb(ui_text::space(6.0))
                    .children((!worker.assignment.is_empty()).then(|| {
                        div()
                            .text_color(rgb(colors.muted))
                            .child(compressed(&worker.assignment, 360))
                    }))
                    .children(worker.detail.as_ref().map(|message| {
                        div()
                            .text_color(rgb(colors.gold))
                            .child(compressed(message, 240))
                    }))
                    .child(
                        div()
                            .id(SharedString::from(format!("shelf-output:{}", worker.id)))
                            .max_h(ui_text::space(64.0))
                            .overflow_y_scroll()
                            .font_family(ui_text::code_family())
                            .text_color(rgb(colors.muted))
                            .child(if worker.output.is_empty() {
                                "No output reported yet.".into()
                            } else {
                                worker
                                    .output
                                    .lines()
                                    .rev()
                                    .take(6)
                                    .collect::<Vec<_>>()
                                    .into_iter()
                                    .rev()
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            })
                            .test_support(),
                    );
                let mut actions = div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap(ui_text::space(6.0))
                    .child(
                        widgets::button(
                            SharedString::from(format!("shelf-reveal:{}", worker.id)),
                            "In chat",
                            None,
                            look,
                        )
                        .on_click(cx.listener(move |view, _, _, cx| {
                            let item_id = view.model.transcript.items[at].id.clone();
                            view.reveal_projected_item(&item_id, cx);
                        })),
                    );
                if let Some(session) = &worker.session {
                    let marker = workers::of(&self.model.transcript.items[at]).unwrap();
                    let live = self.workers.live.get(session);
                    let chat = marker.kind == WorkerKind::Chat;
                    let actionable = live.is_some_and(|live| live.actionable);
                    let id = session.clone();
                    actions = actions.child(
                        widgets::button(
                            SharedString::from(format!("shelf-open:{session}")),
                            if chat { "Open chat" } else { "Open shell" },
                            None,
                            look,
                        )
                        .disabled(!actionable && !chat)
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.worker_action(id.clone(), ShellAction::Open, cx)
                        })),
                    );
                    if !chat {
                        let id = session.clone();
                        actions = actions.child(
                            widgets::button(
                                SharedString::from(format!("shelf-message:{session}")),
                                "Message",
                                None,
                                look,
                            )
                            .disabled(!actionable || self.workers.pending.is_some())
                            .on_click(cx.listener(
                                move |view, _, window, cx| {
                                    view.message_worker(id.clone(), window, cx)
                                },
                            )),
                        );
                        let id = session.clone();
                        actions = actions.child(
                            widgets::button(
                                SharedString::from(format!("shelf-stop:{session}")),
                                "Stop",
                                None,
                                look,
                            )
                            .disabled(!actionable || self.workers.pending.is_some())
                            .on_click(cx.listener(
                                move |view, _, _, cx| {
                                    view.worker_action(id.clone(), ShellAction::Stop, cx)
                                },
                            )),
                        );
                    }
                }
                detail = detail.child(actions);
                body = body.child(detail.test_support());
            }
        }
        Some(
            div()
                .id("chat-workers")
                .flex_none()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .border_t_1()
                .border_color(rgb(colors.divider))
                .bg(rgb(colors.panel))
                .px(ui_text::space(12.0))
                .py(ui_text::space(4.0))
                .text_size(ui_text::text(10.0))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(ui_text::space(6.0))
                        .child(
                            div()
                                .id("chat-worker-shelf-count")
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(rgb(colors.muted))
                                .child(header)
                                .test_support(),
                        )
                        .when(workers.len() > COMPACT_ROWS, |header| {
                            header.child(
                                widgets::button(
                                    "chat-worker-shelf-expand",
                                    if expanded {
                                        "Less".into()
                                    } else {
                                        format!("All {}", workers.len())
                                    },
                                    None,
                                    look,
                                )
                                .aria_expanded(expanded)
                                .on_click(cx.listener(
                                    |view, _, _, cx| {
                                        view.workers.shelf.expanded = !view.workers.shelf.expanded;
                                        cx.notify();
                                    },
                                )),
                            )
                        }),
                )
                .child(body)
                .test_support()
                .into_any_element(),
        )
    }
}
