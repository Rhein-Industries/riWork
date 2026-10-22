//! Native draft source chips. Editing changes one quote/comment; provenance and
//! provider dispatch snapshots remain separate. Nothing in this module sends.
use super::{
    ChatView, diff,
    display::Row,
    notices::LocalKey,
    widgets::{self, Look},
};
use crate::{
    chat::model::{ItemBody, NoticeLevel},
    chat_drafts::references::{self as data, DraftReference, ReferenceOrigin, ReferenceState},
    text_input::{self, EnterBehavior, InputEvent, TextareaState},
    ui_text,
};
use gpui::{
    AnyElement, Context, Entity, Focusable, ListOffset, Subscription, Window, div, prelude::*, px,
    rgb,
};
use gpui_kit::base::TestSupportExt as _;
use std::collections::HashMap;

type Revisions = HashMap<(String, Option<usize>), String>;

#[cfg(test)]
mod tests;

struct Entry {
    value: DraftReference,
    quote: Entity<TextareaState>,
    comment: Entity<TextareaState>,
    _subscriptions: Vec<Subscription>,
}

#[derive(Default)]
pub(super) struct DraftReferences {
    entries: Vec<Entry>,
    editing: Option<String>,
}
impl DraftReferences {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn contents(&self) -> Vec<DraftReference> {
        self.entries
            .iter()
            .map(|entry| entry.value.clone())
            .collect()
    }
    pub fn clear(&mut self) {
        self.entries.clear();
        self.editing = None;
    }
    fn entry(value: DraftReference, window: &mut Window, cx: &mut Context<ChatView>) -> Entry {
        let quote = text_input::multiline(
            value.quote.clone(),
            "Quoted source",
            1,
            5,
            EnterBehavior::Newline,
            window,
            cx,
        );
        let comment = text_input::multiline(
            value.comment.clone(),
            "Your reply or review comment",
            1,
            4,
            EnterBehavior::Newline,
            window,
            cx,
        );
        let subscriptions = [(true, &quote), (false, &comment)]
            .into_iter()
            .map(|(is_quote, state)| {
                let id = value.id.clone();
                cx.subscribe_in(state, window, move |view, state, event, window, cx| {
                    if matches!(event, InputEvent::Focus) {
                        gpui_kit::base::TextSelection::clear(window, cx);
                    } else if matches!(event, InputEvent::Change) {
                        view.reference_edited(
                            &id,
                            is_quote,
                            state.read(cx).value().to_string(),
                            cx,
                        );
                    }
                })
            })
            .collect();
        Entry {
            value,
            quote,
            comment,
            _subscriptions: subscriptions,
        }
    }
}

impl ChatView {
    pub(super) fn focused_reference_editor(
        &self,
        window: &Window,
        cx: &gpui::App,
    ) -> Option<Entity<TextareaState>> {
        self.references
            .entries
            .iter()
            .flat_map(|entry| [&entry.quote, &entry.comment])
            .find(|editor| editor.read(cx).focus_handle(cx).is_focused(window))
            .cloned()
    }
    pub(super) fn restore_references(
        &mut self,
        values: Vec<DraftReference>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.references.clear();
        self.references.entries = values
            .into_iter()
            .map(|mut value| {
                value.state = self.reference_state(&value);
                DraftReferences::entry(value, window, cx)
            })
            .collect();
    }

    pub(super) fn add_reference(
        &mut self,
        value: DraftReference,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut values = self.references.contents();
        values.push(value.clone());
        if !data::valid(&values) {
            self.notices.set(LocalKey::Reference, NoticeLevel::Warning, "A draft supports up to 16 source references within the recovery limit. Remove an older reference first.");
            cx.notify();
            return false;
        }
        let entry = DraftReferences::entry(value, window, cx);
        entry.comment.read(cx).focus_handle(cx).focus(window, cx);
        self.references.editing = Some(entry.value.id.clone());
        self.references.entries.push(entry);
        self.notices.clear(LocalKey::Reference);
        self.dictation.user_edited();
        self.bump_generation();
        self.remember_draft(cx);
        cx.notify();
        true
    }

    fn reference_edited(&mut self, id: &str, quote: bool, value: String, cx: &mut Context<Self>) {
        let Some(entry) = self
            .references
            .entries
            .iter_mut()
            .find(|entry| entry.value.id == id)
        else {
            return;
        };
        if quote {
            entry.value.quote = value;
        } else {
            entry.value.comment = value;
        }
        self.bump_generation();
        if data::valid(&self.references.contents()) {
            self.notices.clear(LocalKey::Reference);
            self.remember_draft(cx);
        } else {
            self.notices.set(LocalKey::Reference, NoticeLevel::Warning, "This source reference is too large to save or send. Keep each quote/comment within 8 KiB and reduce other references if needed; the previous saved draft is retained.");
        }
        cx.notify();
    }

    fn remove_reference(&mut self, id: &str, cx: &mut Context<Self>) {
        self.references.entries.retain(|entry| entry.value.id != id);
        if self.references.editing.as_deref() == Some(id) {
            self.references.editing = None;
        }
        if data::valid(&self.references.contents()) {
            self.notices.clear(LocalKey::Reference);
        }
        self.bump_generation();
        self.remember_draft(cx);
        cx.notify();
    }

    pub(super) fn close_reference_editor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.references.entries.iter().any(|entry| {
            entry.quote.read(cx).focus_handle(cx).is_focused(window)
                || entry.comment.read(cx).focus_handle(cx).is_focused(window)
        }) {
            return false;
        }
        if self.references.editing.take().is_none() {
            return false;
        }
        self.focus(window, cx);
        cx.notify();
        true
    }

    fn live_reference_origin(&self, reference: &DraftReference) -> Option<ReferenceOrigin> {
        self.live_reference_origin_cached(reference, &mut Revisions::new())
    }

    fn live_reference_origin_cached(
        &self,
        reference: &DraftReference,
        revisions: &mut Revisions,
    ) -> Option<ReferenceOrigin> {
        if reference.chat != self.chat_id {
            return None;
        }
        let item = self
            .model
            .transcript
            .items
            .iter()
            .find(|item| item.id == reference.origin.item())?;
        match (&reference.origin, &item.body) {
            (ReferenceOrigin::Message { turn, .. }, ItemBody::AgentMessage { text })
                if turn == &item.turn_id =>
            {
                Some(ReferenceOrigin::Message {
                    item: item.id.clone(),
                    turn: turn.clone(),
                    revision: revisions
                        .entry((item.id.clone(), None))
                        .or_insert_with(|| data::revision(text))
                        .clone(),
                })
            }
            (
                ReferenceOrigin::Diff {
                    turn,
                    path,
                    kind,
                    change,
                    first,
                    last,
                    old,
                    new,
                    ..
                },
                ItemBody::FileChange { changes },
            ) if turn == &item.turn_id => {
                let matches = |candidate: &&crate::chat::model::FileChange| {
                    candidate.path == *path && candidate.kind == *kind
                };
                let at = if changes.get(*change).filter(matches).is_some() {
                    *change
                } else {
                    let candidates: Vec<_> = changes
                        .iter()
                        .enumerate()
                        .filter(|(_, candidate)| matches(candidate))
                        .collect();
                    if candidates.len() != 1 {
                        return None;
                    }
                    candidates[0].0
                };
                let patch = changes[at].diff.as_deref()?;
                Some(ReferenceOrigin::Diff {
                    item: item.id.clone(),
                    turn: turn.clone(),
                    change: at,
                    path: path.clone(),
                    kind: *kind,
                    revision: revisions
                        .entry((item.id.clone(), Some(at)))
                        .or_insert_with(|| data::revision(patch))
                        .clone(),
                    first: *first,
                    last: *last,
                    old: old.clone(),
                    new: new.clone(),
                })
            }
            _ => None,
        }
    }

    fn reference_state(&self, reference: &DraftReference) -> ReferenceState {
        self.reference_state_cached(reference, &mut Revisions::new())
    }

    fn reference_state_cached(
        &self,
        reference: &DraftReference,
        revisions: &mut Revisions,
    ) -> ReferenceState {
        match self.live_reference_origin_cached(reference, revisions) {
            Some(origin) if origin == reference.origin => ReferenceState::Current,
            Some(_) => ReferenceState::Changed,
            None if !matches!(self.model.link, super::Link::Live) => ReferenceState::Unloaded,
            None => ReferenceState::Missing,
        }
    }

    pub(super) fn refresh_reference_states(&mut self, cx: &mut Context<Self>) {
        // Several comments commonly share one answer or patch. Hash each actual
        // source once for this update, without retaining revisions across deltas.
        let mut revisions = Revisions::new();
        let states: Vec<_> = self
            .references
            .entries
            .iter()
            .map(|entry| self.reference_state_cached(&entry.value, &mut revisions))
            .collect();
        let mut changed = false;
        for (entry, state) in self.references.entries.iter_mut().zip(states) {
            if entry.value.state != state {
                entry.value.state = state;
                changed = true;
            }
        }
        if changed {
            // Automatic source updates never claim another window's saved editor.
            let owned = self.chat_id.as_deref().is_none_or(|chat| {
                crate::chat_drafts::owns(
                    chat,
                    (self.composer.entity_id().as_u64(), self.editor_generation),
                    cx,
                )
            });
            if owned {
                self.bump_generation();
                self.remember_draft(cx);
            }
            cx.notify();
        }
    }

    fn reanchor_reference(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(value) = self
            .references
            .entries
            .iter()
            .find(|entry| entry.value.id == id)
            .map(|entry| entry.value.clone())
        else {
            return;
        };
        let Some(mut origin) = self.live_reference_origin(&value) else {
            return;
        };
        let mut fragments = value.fragments.clone();
        let anchored = match &mut origin {
            ReferenceOrigin::Diff {
                item,
                change,
                first,
                last,
                old,
                new,
                kind,
                ..
            } => {
                let Some(ItemBody::FileChange { changes }) = self
                    .model
                    .transcript
                    .items
                    .iter()
                    .find(|candidate| candidate.id == *item)
                    .map(|item| &item.body)
                else {
                    return;
                };
                let Some(patch) = changes[*change].diff.as_deref() else {
                    return;
                };
                let (lines, _) = diff::display(patch, *kind);
                let excerpt: Vec<_> = value.excerpt.split('\n').collect();
                let count = excerpt.len();
                let candidates: Vec<_> = lines
                    .windows(count)
                    .enumerate()
                    .filter(|(_, lines)| {
                        lines
                            .iter()
                            .map(|line| line.text)
                            .eq(excerpt.iter().copied())
                    })
                    .map(|(at, _)| at)
                    .take(2)
                    .collect();
                if candidates.len() != 1 {
                    false
                } else {
                    *first = candidates[0];
                    *last = *first + count - 1;
                    let numbers = diff::line_numbers(&lines, *kind);
                    *old = super::feedback::line_ranges(
                        numbers[*first..=*last].iter().filter_map(|(old, _)| *old),
                    );
                    *new = super::feedback::line_ranges(
                        numbers[*first..=*last].iter().filter_map(|(_, new)| *new),
                    );
                    fragments.clear();
                    true
                }
            }
            ReferenceOrigin::Message { item, .. } => {
                let source = super::selection_document::search_source(self, Look::of(cx));
                let prefix = format!("{item}/");
                !fragments.is_empty()
                    && fragments.iter_mut().all(|fragment| {
                        let hits: Vec<_> = source
                            .leaves
                            .iter()
                            .filter(|leaf| {
                                leaf.key.starts_with(&prefix)
                                    || leaf.key.starts_with(&format!("code:{prefix}"))
                                    || leaf.key.starts_with(&format!("callout:{prefix}"))
                            })
                            .flat_map(|leaf| {
                                leaf.text
                                    .match_indices(&fragment.text)
                                    .map(move |(at, _)| (leaf.key.clone(), at))
                            })
                            .collect();
                        if hits.len() != 1 {
                            return false;
                        }
                        fragment.key = hits[0].0.clone();
                        fragment.start = hits[0].1;
                        fragment.end = hits[0].1 + fragment.text.len();
                        true
                    })
            }
        };
        if !anchored {
            self.notices.set(LocalKey::Reference, NoticeLevel::Warning, "The original excerpt is missing or ambiguous in the changed source. Keep the saved reference, or select the new passage and add a replacement.");
            cx.notify();
            return;
        }
        let entry = self
            .references
            .entries
            .iter_mut()
            .find(|entry| entry.value.id == id)
            .unwrap();
        entry
            .value
            .original_origin
            .get_or_insert_with(|| value.origin.clone());
        entry.value.origin = origin;
        entry.value.fragments = fragments;
        entry.value.state = ReferenceState::Current;
        self.notices.clear(LocalKey::Reference);
        self.bump_generation();
        self.remember_draft(cx);
        cx.notify();
    }

    fn go_to_reference(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(value) = self
            .references
            .entries
            .iter()
            .find(|entry| entry.value.id == id)
            .map(|entry| entry.value.clone())
        else {
            return;
        };
        let Some(origin) = self.live_reference_origin(&value) else {
            self.notices.set(LocalKey::Reference, NoticeLevel::Warning, "This source is not available in the current transcript. The saved excerpt and comment are retained.");
            cx.notify();
            return;
        };
        if let ReferenceOrigin::Diff {
            item,
            change,
            first,
            ..
        } = origin
        {
            self.open_file_review_at(&item, change, Some(first), window, cx);
            return;
        }
        self.close_file_review(window, cx);
        let Some(at) = self
            .model
            .transcript
            .items
            .iter()
            .position(|item| item.id == origin.item())
        else {
            return;
        };
        if let Some(turn) = self.model.transcript.items[at].turn_id.as_deref() {
            self.open.insert(super::display::activity_key(turn));
        }
        self.refresh_projection(cx);
        if let Some(row) = self
            .visible
            .iter()
            .position(|row| matches!(row, Row::Item(index) | Row::Details(index) if *index == at))
        {
            self.find.cancel_reveal();
            self.list.set_follow_mode(gpui::FollowMode::Normal);
            self.list.scroll_to(ListOffset {
                item_ix: row,
                offset_in_item: px(0.),
            });
            self.focus.focus(window, cx);
            cx.notify();
        }
    }

    pub(super) fn reference_chips(
        &self,
        look: Look,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.references.is_empty() {
            return None;
        }
        Some(
            div()
                .id("chat-source-references")
                .role(gpui::Role::Group)
                .aria_label("Draft source references")
                .w_full()
                .min_w_0()
                .max_h(ui_text::space(340.))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(ui_text::space(6.))
                .children(self.references.entries.iter().map(|entry| {
                    let value = &entry.value;
                    let expanded = self.references.editing.as_deref() == Some(&value.id);
                    let edit_id = value.id.clone();
                    let remove_id = value.id.clone();
                    let go_id = value.id.clone();
                    let anchor_id = value.id.clone();
                    div()
                        .id(format!("reference:{}", value.id))
                        .w_full()
                        .min_w_0()
                        .p(ui_text::space(8.))
                        .border_1()
                        .border_color(rgb(look.colors.divider))
                        .rounded(ui_text::space(4.))
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap(ui_text::space(6.))
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex_1()
                                        .text_size(ui_text::text(11.))
                                        .child(value.origin.label()),
                                )
                                .child(
                                    widgets::button(
                                        format!("reference-go:{}", value.id),
                                        "Go to source",
                                        None,
                                        look,
                                    )
                                    .on_click(cx.listener(
                                        move |view, _, window, cx| {
                                            view.go_to_reference(&go_id, window, cx)
                                        },
                                    )),
                                )
                                .child(
                                    widgets::button(
                                        format!("reference-edit:{}", value.id),
                                        if expanded { "Done" } else { "Edit" },
                                        None,
                                        look,
                                    )
                                    .aria_expanded(expanded)
                                    .on_click(cx.listener(
                                        move |view, _, window, cx| {
                                            if view.references.editing.as_deref() == Some(&edit_id)
                                            {
                                                view.references.editing = None;
                                                view.focus(window, cx);
                                            } else {
                                                view.references.editing = Some(edit_id.clone());
                                                if let Some(entry) = view
                                                    .references
                                                    .entries
                                                    .iter()
                                                    .find(|entry| entry.value.id == edit_id)
                                                {
                                                    entry
                                                        .comment
                                                        .read(cx)
                                                        .focus_handle(cx)
                                                        .focus(window, cx);
                                                }
                                            }
                                            cx.notify();
                                        },
                                    )),
                                )
                                .child(
                                    widgets::button(
                                        format!("reference-remove:{}", value.id),
                                        "Remove",
                                        None,
                                        look,
                                    )
                                    .on_click(cx.listener(
                                        move |view, _, _, cx| view.remove_reference(&remove_id, cx),
                                    )),
                                ),
                        )
                        .child(
                            div()
                                .text_size(ui_text::text(10.))
                                .text_color(rgb(look.colors.muted))
                                .child(value.state.label()),
                        )
                        .children((value.state == ReferenceState::Changed).then(|| {
                            widgets::button(
                                format!("reference-reanchor:{}", value.id),
                                "Re-anchor unchanged excerpt",
                                None,
                                look,
                            )
                            .on_click(cx.listener(
                                move |view, _, _, cx| view.reanchor_reference(&anchor_id, cx),
                            ))
                        }))
                        .when(expanded, |card| {
                            card.child(div().text_size(ui_text::text(10.)).child("Quote"))
                                .child(text_input::textarea(
                                    format!("reference-quote:{}", value.id),
                                    &entry.quote,
                                    window,
                                    cx,
                                ))
                                .child(
                                    div()
                                        .text_size(ui_text::text(10.))
                                        .child("Reply / review comment"),
                                )
                                .child(text_input::textarea(
                                    format!("reference-comment:{}", value.id),
                                    &entry.comment,
                                    window,
                                    cx,
                                ))
                        })
                        .when(!expanded, |card| {
                            card.child(
                                div()
                                    .text_size(ui_text::text(11.))
                                    .child(value.quote.chars().take(140).collect::<String>()),
                            )
                            .children(
                                (!value.comment.is_empty()).then(|| {
                                    div()
                                        .text_size(ui_text::text(11.))
                                        .child(value.comment.chars().take(140).collect::<String>())
                                }),
                            )
                        })
                        .test_support()
                }))
                .test_support()
                .into_any_element(),
        )
    }
}
