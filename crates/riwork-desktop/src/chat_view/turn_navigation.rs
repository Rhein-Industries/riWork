//! A bounded conversation index over the visible transcript. Targets are row
//! anchors, never raw item indices: collapsed activity changes that mapping.
use super::{
    ChatView, display,
    widgets::{self, Look},
};
use crate::{
    chat::model::{Item, ItemBody, MessagePhase},
    ui_text,
};
use gpui::{
    AnyElement, Context, FocusHandle, ListOffset, SharedString, Window, deferred, div, prelude::*,
    px, rgb,
};
use gpui_kit::base::TestSupportExt as _;
use std::collections::HashMap;

const PAGE_SIZE: usize = 8;
const PREVIEW_CHARS: usize = 140;

#[derive(Default)]
pub(super) struct Navigation {
    pub(super) expanded: bool,
    page: usize,
    indexed: bool,
    turns: Vec<Turn>,
    /// A requested row may be clamped backwards when its remaining content is
    /// shorter than the viewport. Keep the user's turn selection in that case.
    requested: Option<String>,
    picker_focus: Option<FocusHandle>,
}

impl Navigation {
    pub(super) fn invalidate(&mut self) {
        self.indexed = false;
    }

    pub(super) fn manual_scroll(&mut self) {
        self.requested = None;
    }

    fn refresh(&mut self, items: &[Item], visible: &[display::Row]) {
        if self.indexed {
            return;
        }
        self.turns = index(items, visible);
        self.page = self
            .page
            .min(self.turns.len().saturating_sub(1) / PAGE_SIZE);
        self.indexed = true;
    }
}

#[derive(Debug)]
struct Turn {
    key: String,
    prompt: Option<usize>,
    answer: Option<usize>,
    followups: usize,
    row: usize,
}

/// Build in O(items + visible rows), grouping steering messages with their real
/// turn. Legacy messages without a turn id get a stable prompt-item boundary.
fn index(items: &[Item], visible: &[display::Row]) -> Vec<Turn> {
    let mut turns: Vec<Turn> = Vec::new();
    let mut by_key = HashMap::<String, usize>::new();
    let mut owners = vec![None; items.len()];
    let mut latest = None;
    for (at, item) in items.iter().enumerate() {
        let key = item
            .turn_id
            .as_ref()
            .map(|id| format!("turn:{id}"))
            .or_else(|| {
                matches!(item.body, ItemBody::UserMessage { .. })
                    .then(|| format!("message:{}", item.id))
            });
        let owner = if let Some(key) = key {
            *by_key.entry(key.clone()).or_insert_with(|| {
                turns.push(Turn {
                    key,
                    prompt: None,
                    answer: None,
                    followups: 0,
                    row: usize::MAX,
                });
                turns.len() - 1
            })
        } else if let Some(owner) = latest {
            owner
        } else if matches!(item.body, ItemBody::AgentMessage { .. }) {
            let owner = turns.len();
            turns.push(Turn {
                key: format!("message:{}", item.id),
                prompt: None,
                answer: None,
                followups: 0,
                row: usize::MAX,
            });
            owner
        } else {
            continue;
        };
        owners[at] = Some(owner);
        // A late marker for an older turn must not move the legacy boundary back.
        latest = Some(latest.map_or(owner, |old: usize| old.max(owner)));
        let turn = &mut turns[owner];
        match &item.body {
            ItemBody::UserMessage { .. } => {
                if turn.prompt.is_none() {
                    turn.prompt = Some(at);
                } else {
                    turn.followups += 1;
                }
            }
            ItemBody::AgentMessage { .. } => {
                let rank = |item: &Item| match item.presentation.phase {
                    Some(MessagePhase::Final) => 2,
                    Some(MessagePhase::Commentary) => 0,
                    None => 1,
                };
                if turn
                    .answer
                    .is_none_or(|previous| rank(item) >= rank(&items[previous]))
                {
                    turn.answer = Some(at);
                }
            }
            _ => {}
        }
    }
    for (row, entry) in visible.iter().enumerate() {
        let (owner, prompt) = match entry {
            display::Row::Item(at) | display::Row::Details(at) => {
                let owner = owners.get(*at).copied().flatten();
                (
                    owner,
                    owner.is_some_and(|owner| turns[owner].prompt == Some(*at)),
                )
            }
            display::Row::Activity { turn_id, .. }
            | display::Row::Artifacts { turn_id, .. }
            | display::Row::Outcome(turn_id, _) => {
                (by_key.get(&format!("turn:{turn_id}")).copied(), false)
            }
        };
        if let Some(owner) = owner {
            let turn = &mut turns[owner];
            if turn.row == usize::MAX || prompt {
                turn.row = row;
            }
        }
    }
    // Entirely hidden entries have no honest jump target. A visible activity,
    // artifact or outcome is a valid fallback when no prompt is available.
    turns.retain(|turn| turn.row != usize::MAX);
    turns.sort_by_key(|turn| turn.row);
    turns
}

fn current(turns: &[Turn], row: usize) -> Option<usize> {
    (!turns.is_empty()).then(|| {
        turns
            .partition_point(|turn| turn.row <= row)
            .saturating_sub(1)
    })
}

/// Bound work as well as output: huge pasted prompts and whitespace cannot make
/// opening the navigator scan or allocate their full contents.
fn preview(text: &str) -> String {
    let mut result = String::new();
    let mut count = 0;
    let mut space = false;
    for ch in text.chars().take(4096) {
        if ch.is_whitespace() {
            space = !result.is_empty();
            continue;
        }
        if ch.is_control() {
            continue;
        }
        if count >= PREVIEW_CHARS {
            result.push('…');
            break;
        }
        if space {
            result.push(' ');
            count += 1;
            space = false;
        }
        result.push(ch);
        count += 1;
    }
    result
}

fn previews(turn: &Turn, items: &[Item]) -> (String, String) {
    let text = |at: Option<usize>| {
        at.and_then(|at| items.get(at))
            .map(|item| match &item.body {
                ItemBody::UserMessage { text } | ItemBody::AgentMessage { text } => preview(text),
                _ => String::new(),
            })
            .unwrap_or_default()
    };
    let mut prompt = text(turn.prompt);
    if prompt.is_empty() {
        prompt = "Turn without a text prompt".into();
    }
    if turn.followups > 0 {
        prompt.push_str(&format!(" (+{} follow-up)", turn.followups));
    }
    let mut answer = text(turn.answer);
    if answer.is_empty() {
        answer = "No answer text yet".into();
    }
    (prompt, answer)
}

impl ChatView {
    fn refresh_turn_navigation(&mut self) {
        self.turn_navigation
            .refresh(&self.model.transcript.items, &self.visible);
        if self.list.is_following_tail() || self.list.is_scrollbar_dragging() {
            self.turn_navigation.manual_scroll();
        } else if let Some(key) = &self.turn_navigation.requested {
            let top = self.list.logical_scroll_top().item_ix;
            let still_selected = self.turn_navigation.turns.iter().any(|turn| {
                turn.key == *key
                    && (turn.row == top || (turn.row > top && self.turn_navigation_at_end()))
            });
            if !still_selected {
                self.turn_navigation.manual_scroll();
            }
        }
    }

    fn turn_navigation_at_end(&self) -> bool {
        // ListState returns None for a non-scrollable list. Its final footer
        // being inside the viewport also covers that all-content-fits case.
        self.list.is_scrolled_to_end() == Some(true)
            || self.list.item_count().checked_sub(1).is_some_and(|last| {
                self.list.bounds_for_item(last).is_some_and(|bounds| {
                    let viewport = self.list.viewport_bounds();
                    viewport.size.height > px(0.)
                        && bounds.bottom() <= viewport.bottom() + px(1.)
                        && bounds.bottom() >= viewport.top()
                })
            })
    }

    fn current_turn(&self) -> Option<usize> {
        if self.list.is_following_tail() {
            return self.turn_navigation.turns.len().checked_sub(1);
        }
        if self.turn_navigation_at_end()
            && let Some(key) = &self.turn_navigation.requested
            && let Some(at) = self
                .turn_navigation
                .turns
                .iter()
                .position(|turn| turn.key == *key)
            && self.turn_navigation.turns[at].row >= self.list.logical_scroll_top().item_ix
        {
            return Some(at);
        }
        current(
            &self.turn_navigation.turns,
            self.list.logical_scroll_top().item_ix,
        )
    }

    fn jump_to_turn(&mut self, key: &str, cx: &mut Context<Self>) {
        self.refresh_turn_navigation();
        let Some(turn) = self
            .turn_navigation
            .turns
            .iter()
            .find(|turn| turn.key == key)
        else {
            return;
        };
        self.find.cancel_reveal();
        self.turn_navigation.requested = Some(turn.key.clone());
        // A plain scroll_to only pauses Tail. ListState immediately resumes it
        // when the remaining content fits, losing the selected turn on layout
        // (or the next streamed row). Explicit navigation is a reading anchor;
        // the existing Latest action restores automatic tail following.
        self.list.set_follow_mode(gpui::FollowMode::Normal);
        self.list.scroll_to(ListOffset {
            item_ix: turn.row,
            offset_in_item: px(0.),
        });
        self.turn_navigation.expanded = false;
        cx.notify();
    }

    fn step_turn(&mut self, next: bool, cx: &mut Context<Self>) {
        self.refresh_turn_navigation();
        let Some(at) = self.current_turn() else {
            return;
        };
        let target = if next {
            at.checked_add(1)
        } else {
            at.checked_sub(1)
        };
        let Some(key) = target
            .and_then(|at| self.turn_navigation.turns.get(at))
            .map(|turn| turn.key.clone())
        else {
            return;
        };
        self.jump_to_turn(&key, cx);
    }

    fn focus_turn_picker(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(focus) = &self.turn_navigation.picker_focus {
            focus.focus(window, cx);
        }
    }

    pub(super) fn turn_navigator(
        &mut self,
        look: Look,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        self.refresh_turn_navigation();
        let at = self.current_turn()?;
        let picker_focus = self
            .turn_navigation
            .picker_focus
            .get_or_insert_with(|| cx.focus_handle())
            .clone();
        let count = self.turn_navigation.turns.len();
        let expanded = self.turn_navigation.expanded;
        let colors = look.colors;
        let control =
            |id: &'static str, mark: &'static str, label: &'static str, disabled: bool| {
                crate::behavior_controls::button_content(id, label, mark)
                    .disabled(disabled)
                    .flex_none()
                    .size(ui_text::space(24.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.))
                    .text_color(rgb(colors.muted))
                    .when(disabled, |button| button.opacity(0.4))
                    .when(!disabled, |button| {
                        button
                            .cursor_pointer()
                            .hover(move |style| style.bg(rgb(colors.divider)))
                    })
                    .focus_visible(move |style| {
                        style.bg(rgb(colors.divider)).text_color(rgb(colors.focus))
                    })
                    .child(crate::tooltip::anchor(label, crate::tooltip::Look::Control))
            };
        let (prompt, _) = previews(
            &self.turn_navigation.turns[at],
            &self.model.transcript.items,
        );
        let bar = div()
            .id("chat-turn-navigation")
            .relative()
            .flex_none()
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap(ui_text::space(4.))
            .px(ui_text::space(8.))
            .py(ui_text::space(2.))
            .border_b_1()
            .border_color(rgb(colors.divider))
            .bg(rgb(colors.panel))
            .text_size(ui_text::text(11.))
            .font_family(look.chat_family())
            .child(
                widgets::button(
                    "chat-turn-picker",
                    format!("Turns {}/{}", at + 1, count),
                    None,
                    look,
                )
                .track_focus(&picker_focus)
                .aria_label(format!(
                    "Conversation turns. Current turn {} of {count}",
                    at + 1
                ))
                .aria_expanded(expanded)
                .on_click(cx.listener(|view, _, _, cx| {
                    view.refresh_turn_navigation();
                    view.turn_navigation.expanded = !view.turn_navigation.expanded;
                    if view.turn_navigation.expanded {
                        view.turn_navigation.page = view.current_turn().unwrap_or(0) / PAGE_SIZE;
                    }
                    cx.notify();
                })),
            )
            .child(
                control(
                    "chat-turn-previous",
                    "↑",
                    "Previous conversation turn",
                    at == 0,
                )
                .on_click(cx.listener(|view, _, _, cx| view.step_turn(false, cx))),
            )
            .child(
                control(
                    "chat-turn-next",
                    "↓",
                    "Next conversation turn",
                    at + 1 == count,
                )
                .on_click(cx.listener(|view, _, _, cx| view.step_turn(true, cx))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(rgb(colors.muted))
                    .child(prompt),
            )
            .on_key_down(cx.listener(|view, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" && view.turn_navigation.expanded {
                    view.turn_navigation.expanded = false;
                    view.focus_turn_picker(window, cx);
                    cx.stop_propagation();
                    cx.notify();
                }
            }));
        let panel = expanded.then(|| {
            let start = self.turn_navigation.page * PAGE_SIZE;
            let end = (start + PAGE_SIZE).min(count);
            let mut rows = div()
                .id("chat-turn-choices")
                .max_h(ui_text::space(300.))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .w_full()
                .min_w_0();
            for (offset, turn) in self.turn_navigation.turns[start..end].iter().enumerate() {
                let ordinal = start + offset;
                let key = turn.key.clone();
                let (prompt, answer) = previews(turn, &self.model.transcript.items);
                let body = div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(ui_text::space(2.))
                    .px(ui_text::space(8.))
                    .py(ui_text::space(5.))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(rgb(colors.text))
                            .child(format!("{}. {prompt}", ordinal + 1)),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(rgb(colors.muted))
                            .child(answer.clone()),
                    );
                rows = rows.child(
                    widgets::content_button(
                        SharedString::from(format!("chat-turn-{ordinal}")),
                        format!("Turn {}. Prompt: {prompt}. Answer: {answer}", ordinal + 1),
                        body,
                        look,
                    )
                    .w_full()
                    .min_w_0()
                    .when(ordinal == at, |button| button.bg(rgb(colors.panel_active)))
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.jump_to_turn(&key, cx);
                        view.focus_turn_picker(window, cx);
                    })),
                );
            }
            let page = self.turn_navigation.page;
            let panel = div()
                .id("chat-turn-panel")
                .absolute()
                .top(ui_text::space(31.))
                .left_0()
                .right_0()
                .occlude()
                .border_1()
                .border_color(rgb(colors.divider))
                .rounded(px(5.))
                .shadow_md()
                .bg(rgb(colors.panel))
                .role(gpui::Role::Group)
                .aria_label("Conversation turn navigation")
                .child(rows)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(ui_text::space(5.))
                        .p(ui_text::space(5.))
                        .children((count > PAGE_SIZE).then(|| {
                            widgets::button("chat-turn-page-previous", "Earlier", None, look)
                                .disabled(page == 0)
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.turn_navigation.page =
                                        view.turn_navigation.page.saturating_sub(1);
                                    cx.notify();
                                }))
                        }))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_color(rgb(colors.muted))
                                .child(format!("{}–{end} of {count}", start + 1)),
                        )
                        .children((count > PAGE_SIZE).then(|| {
                            widgets::button("chat-turn-page-next", "Later", None, look)
                                .disabled(end == count)
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.turn_navigation.page = (view.turn_navigation.page + 1)
                                        .min(
                                            view.turn_navigation.turns.len().saturating_sub(1)
                                                / PAGE_SIZE,
                                        );
                                    cx.notify();
                                }))
                        }))
                        .child(
                            widgets::button("chat-turn-close", "Close", None, look).on_click(
                                cx.listener(|view, _, window, cx| {
                                    view.turn_navigation.expanded = false;
                                    view.focus_turn_picker(window, cx);
                                    cx.notify();
                                }),
                            ),
                        ),
                )
                .test_support();
            deferred(panel).with_priority(5)
        });
        Some(bar.children(panel).test_support().into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::model::{ItemStatus, Presentation, Transcript, TurnOutcome};
    use gpui_kit::test::TestWindowExt as _;

    fn message(id: &str, turn: Option<&str>, user: bool, text: &str) -> Item {
        Item {
            id: id.into(),
            turn_id: turn.map(str::to_owned),
            status: ItemStatus::Completed,
            presentation: Presentation {
                phase: (!user).then_some(MessagePhase::Final),
                ..Default::default()
            },
            body: if user {
                ItemBody::UserMessage { text: text.into() }
            } else {
                ItemBody::AgentMessage { text: text.into() }
            },
        }
    }

    #[test]
    fn turn_index_maps_collapsed_activity_and_groups_steering_messages() {
        let mut transcript = Transcript::default();
        transcript.items = vec![
            message("q1", Some("t1"), true, "First question"),
            Item {
                id: "tool".into(),
                turn_id: Some("t1".into()),
                status: ItemStatus::Completed,
                presentation: Default::default(),
                body: ItemBody::Reasoning {
                    text: "hidden".into(),
                },
            },
            message("steer", Some("t1"), true, "Also check the edge"),
            message("a1", Some("t1"), false, "First answer"),
            message("q2", Some("t2"), true, "Second question"),
            message("a2", Some("t2"), false, "Streaming answer"),
        ];
        let rows = vec![
            display::Row::Item(0),
            display::Row::Activity {
                turn_id: "t1".into(),
                items: vec![1],
            },
            display::Row::Item(2),
            display::Row::Item(3),
            display::Row::Item(4),
            display::Row::Item(5),
        ];
        let turns = index(&transcript.items, &rows);
        assert_eq!(turns.len(), 2);
        assert_eq!((turns[0].row, turns[1].row), (0, 4));
        assert_eq!(turns[0].followups, 1);
        assert_eq!(previews(&turns[0], &transcript.items).1, "First answer");
        assert_eq!(current(&turns, 3), Some(0));
        assert_eq!(current(&turns, 6), Some(1));
        // The second turn moves when a detail is collapsed. Recompute the row,
        // never reuse its raw item position (4).
        let compact = vec![
            display::Row::Item(0),
            display::Row::Item(3),
            display::Row::Item(4),
            display::Row::Item(5),
        ];
        assert_eq!(index(&transcript.items, &compact)[1].row, 2);
    }

    #[test]
    fn turn_index_handles_legacy_empty_hidden_and_outcome_only_turns() {
        assert!(index(&[], &[]).is_empty());
        let items = vec![
            message("legacy-q", None, true, "old prompt"),
            message("legacy-a", None, false, "old answer"),
            message("hidden", Some("hidden"), false, "not visible"),
            message("interrupted", Some("stopped"), false, ""),
        ];
        let rows = vec![
            display::Row::Item(0),
            display::Row::Item(1),
            display::Row::Outcome("stopped".into(), TurnOutcome::Interrupted),
        ];
        let turns = index(&items, &rows);
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].answer, Some(1));
        assert_eq!(turns[1].row, 2);
        assert_eq!(previews(&turns[1], &items).1, "No answer text yet");
    }

    #[test]
    fn navigator_previews_are_unicode_safe_bounded_and_read_live_text() {
        let mut items = vec![
            message("prompt", Some("turn"), true, "   🦀\n two\t words "),
            message("answer", Some("turn"), false, "before"),
        ];
        let turns = index(&items, &[display::Row::Item(0), display::Row::Item(1)]);
        assert_eq!(previews(&turns[0], &items).0, "🦀 two words");
        if let ItemBody::AgentMessage { text } = &mut items[1].body {
            *text = "after 🦀".into();
        }
        assert_eq!(previews(&turns[0], &items).1, "after 🦀");
        assert!(preview(&"🦀".repeat(100_000)).chars().count() <= PREVIEW_CHARS + 1);
        assert!(preview(&" ".repeat(100_000)).is_empty());
    }

    #[gpui::test]
    fn navigator_clicks_jump_to_visible_turn_rows_without_touching_the_draft(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui_kit::test::TestWindowExt;
        let (handle, view, _recording) = super::super::editor_tests::mount(cx);
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |view, cx| {
                view.model.link = super::super::state::Link::Live;
                view.composer.update(cx, |input, cx| {
                    input.set_value("Keep this draft", window, cx)
                });
                for i in 0..20 {
                    let turn = format!("t{i}");
                    view.model.transcript.items.push(message(
                        &format!("q{i}"),
                        Some(&turn),
                        true,
                        &format!("Question {i}"),
                    ));
                    view.model.transcript.items.push(message(
                        &format!("a{i}"),
                        Some(&turn),
                        false,
                        &"Answer with enough text to fill the viewport. ".repeat(30),
                    ));
                    view.model.completed.push((
                        turn,
                        TurnOutcome::Completed,
                        view.model.transcript.items.len(),
                    ));
                }
                view.refresh_projection(cx);
                view.list.scroll_to(ListOffset {
                    item_ix: 0,
                    offset_in_item: px(0.),
                });
                cx.notify();
            });
            window.render_frame(cx);
            let before = view.read(cx).list.logical_scroll_top();
            window.click("chat-turn-picker", cx);
            window.render_frame(cx);
            let after = view.read(cx).list.logical_scroll_top();
            assert_eq!(
                (after.item_ix, after.offset_in_item),
                (before.item_ix, before.offset_in_item),
                "opening the index must preserve the reader's anchor"
            );
            assert!(
                window
                    .find("chat-turn-0")
                    .label()
                    .unwrap()
                    .contains("Question 0")
            );
            assert!(
                window.try_find("chat-turn-8").is_none(),
                "only one bounded page is rendered"
            );
            window.click("chat-turn-page-next", cx);
            window.render_frame(cx);
            assert!(window.try_find("chat-turn-0").is_none());
            assert!(window.find("chat-turn-8").visible());
            window.click("chat-turn-8", cx);
            window.render_frame(cx);
            assert!(
                view.read(cx)
                    .turn_navigation
                    .picker_focus
                    .as_ref()
                    .unwrap()
                    .is_focused(window)
            );
            view.read_with(cx, |view, cx| {
                assert_eq!(view.current_turn(), Some(8));
                assert!(!view.list.is_following_tail());
                assert_eq!(view.composer_text(cx), "Keep this draft");
                assert!(!view.turn_navigation.expanded);
            });
            window.click("chat-turn-next", cx);
            window.render_frame(cx);
            assert_eq!(view.read(cx).current_turn(), Some(9));
            window.click("chat-turn-previous", cx);
            window.render_frame(cx);
            assert_eq!(view.read(cx).current_turn(), Some(8));
            window.click("chat-turn-picker", cx);
            window.render_frame(cx);
            window.click("chat-turn-close", cx);
            window.render_frame(cx);
            assert!(!view.read(cx).turn_navigation.expanded);
            assert!(
                view.read(cx)
                    .turn_navigation
                    .picker_focus
                    .as_ref()
                    .unwrap()
                    .is_focused(window)
            );
            window.click("chat-turn-picker", cx);
            window.render_frame(cx);
            window.press("escape", cx);
            window.render_frame(cx);
            assert!(!view.read(cx).turn_navigation.expanded);
            assert!(
                view.read(cx)
                    .turn_navigation
                    .picker_focus
                    .as_ref()
                    .unwrap()
                    .is_focused(window)
            );
        })
        .unwrap();
    }

    #[gpui::test]
    fn navigator_keeps_turn_selection_when_all_turns_fit_in_the_viewport(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui_kit::test::TestWindowExt;
        let (handle, view, _) = super::super::editor_tests::mount(cx);
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |view, cx| {
                view.model.link = super::super::state::Link::Live;
                for i in 0..2 {
                    let turn = format!("t{i}");
                    view.model.transcript.items.push(message(
                        &format!("q{i}"),
                        Some(&turn),
                        true,
                        "Short question",
                    ));
                    view.model.transcript.items.push(message(
                        &format!("a{i}"),
                        Some(&turn),
                        false,
                        "Short answer",
                    ));
                    view.model.completed.push((
                        turn,
                        TurnOutcome::Completed,
                        view.model.transcript.items.len(),
                    ));
                }
                view.refresh_projection(cx);
                cx.notify();
            });
            window.render_frame(cx);
            assert!(view.read(cx).turn_navigation_at_end());
            assert_eq!(
                view.read(cx).current_turn(),
                Some(1),
                "Latest selects the last turn even when every row fits"
            );
            window.click("chat-turn-picker", cx);
            window.render_frame(cx);
            window.click("chat-turn-1", cx);
            window.render_frame(cx);
            assert_eq!(
                view.read(cx).list.logical_scroll_top().item_ix,
                0,
                "layout clamps the requested second turn back to the start"
            );
            assert_eq!(view.read(cx).current_turn(), Some(1));
            assert!(!view.read(cx).list.is_following_tail());
            assert_eq!(window.find("chat-turn-picker").label(), Some("Turns 2/2"));
            window.click("chat-turn-previous", cx);
            window.render_frame(cx);
            assert_eq!(view.read(cx).current_turn(), Some(0));
            assert!(!view.read(cx).list.is_following_tail());
            window.click("chat-turn-next", cx);
            window.render_frame(cx);
            assert_eq!(view.read(cx).current_turn(), Some(1));
            view.update(cx, |view, cx| {
                view.turn_navigation.manual_scroll();
                view.list.scroll_to(ListOffset {
                    item_ix: 0,
                    offset_in_item: px(0.),
                });
                cx.notify();
            });
            window.render_frame(cx);
            assert_eq!(
                view.read(cx).current_turn(),
                Some(0),
                "manual scrolling restores the visible row as the current turn"
            );
            window.click("chat-latest", cx);
            window.render_frame(cx);
            assert_eq!(view.read(cx).current_turn(), Some(1));
            assert!(view.read(cx).list.is_following_tail());
            assert!(view.read(cx).turn_navigation.requested.is_none());

            // Make the first answer tall: the second turn now clamps near the
            // end of a genuinely scrollable list. Latest must still be offered
            // even when ListState reports Some(true), not the all-fit None.
            view.update(cx, |view, cx| {
                view.model.transcript.items[1].body = ItemBody::AgentMessage {
                    text: "Long first answer. ".repeat(400),
                };
                view.sync_list(
                    &super::super::state::Applied {
                        touched: [1].into_iter().collect(),
                        ..Default::default()
                    },
                    cx,
                );
                // Four fixture rows: know the total height before asserting
                // ListState's public end condition.
                view.list.clone().measure_all();
                view.jump_to_turn("turn:t1", cx);
            });
            window.render_frame(cx);
            assert_eq!(view.read(cx).list.is_scrolled_to_end(), Some(true));
            assert_eq!(view.read(cx).current_turn(), Some(1));
            assert!(view.read(cx).list.logical_scroll_top().item_ix < 2);
            assert!(!view.read(cx).list.is_following_tail());
            assert!(window.find("chat-latest").visible());
            window.click("chat-latest", cx);
            window.render_frame(cx);
            assert!(view.read(cx).list.is_following_tail());
            assert_eq!(view.read(cx).current_turn(), Some(1));
            assert!(view.read(cx).turn_navigation.requested.is_none());
        })
        .unwrap();
    }

    #[gpui::test]
    fn navigator_jump_cancels_pending_find_reveal_but_keeps_the_query(
        cx: &mut gpui::TestAppContext,
    ) {
        let (handle, view, _) = super::super::editor_tests::mount(cx);
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |view, cx| {
                view.model.link = super::super::state::Link::Live;
                for i in 0..20 {
                    let turn = format!("t{i}");
                    view.model.transcript.items.push(message(
                        &format!("q{i}"),
                        Some(&turn),
                        true,
                        &format!("Question {i}"),
                    ));
                    view.model.transcript.items.push(message(
                        &format!("a{i}"),
                        Some(&turn),
                        false,
                        &format!("{} needle", "Long answer text. ".repeat(100)),
                    ));
                    view.model.completed.push((
                        turn,
                        TurnOutcome::Completed,
                        view.model.transcript.items.len(),
                    ));
                }
                view.refresh_projection(cx);
                view.list.scroll_to(ListOffset {
                    item_ix: 0,
                    offset_in_item: px(0.),
                });
                view.find
                    .input
                    .update(cx, |input, cx| input.set_value("needle", window, cx));
                view.open_find(window, cx);
                cx.notify();
            });
            window.render_frame(cx);
        })
        .unwrap();
        // Deliver the input change before testing an explicit Find step and
        // navigation in the same frame. No later input event should restart Find.
        cx.run_until_parked();
        for _ in 0..30 {
            let ran = cx
                .update_window(handle.into(), |_, window, cx| {
                    window.render_frame(cx);
                    window.simulate_next_frame(cx)
                })
                .unwrap();
            cx.run_until_parked();
            if ran == 0 {
                break;
            }
        }
        let status = cx
            .update_window(handle.into(), |_, window, cx| {
                view.update(cx, |view, cx| {
                    view.find_next_action(&super::super::FindNext, window, cx);
                    assert!(
                        view.find.revealing(),
                        "the off-screen match needs a later frame"
                    );
                    let status = view.find.status();
                    view.jump_to_turn("turn:t0", cx);
                    assert!(!view.find.revealing());
                    status
                })
            })
            .unwrap();
        for _ in 0..6 {
            cx.update_window(handle.into(), |_, window, cx| {
                window.render_frame(cx);
                window.simulate_next_frame(cx);
            })
            .unwrap();
            cx.run_until_parked();
        }
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |view, cx| {
                assert_eq!(view.current_turn(), Some(0));
                assert_eq!(view.list.logical_scroll_top().item_ix, 0);
                assert_eq!(view.find.status(), status);
                assert_eq!(view.find.input.read(cx).value().as_ref(), "needle");
                assert!(view.find.open);
                view.find_next_action(&super::super::FindNext, window, cx);
                assert!(
                    view.turn_navigation.requested.is_none(),
                    "a new Find operation takes navigation authority"
                );
            });
        })
        .unwrap();
    }
}
