//! Selected output becomes an editable, durable source reference. Source validation happens
//! again at activation: a streaming replacement cannot silently quote another passage.
use super::{
    ChatView, diff,
    select::SelectionFragment,
    widgets::{self, Look},
};
use crate::chat_drafts::references::{
    DraftReference, ReferenceFragment, ReferenceOrigin as Origin, ReferenceState, revision,
};
use crate::{
    chat::model::{ChangeKind, FileChange, Item, ItemBody},
    ui_text,
};
use gpui::{AnyElement, Context, MouseButton, SharedString, Window, div, prelude::*, rgb};
use gpui_kit::base::TestSupportExt as _;

pub(super) const MAX_QUOTE_BYTES: usize = 8 * 1024;
pub(super) const MAX_DIFF_LINES: usize = 80;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SelectionFeedback {
    chat: Option<String>,
    fragments: Vec<SelectionFragment>,
    origin: Origin,
    snippet: String,
    diff_key: Option<String>,
}

fn quoted(value: &str) -> String {
    // Paths and provider IDs are labels, including when they contain newlines.
    serde_json::to_string(value).expect("a string is serializable")
}

pub(super) fn line_ranges(values: impl IntoIterator<Item = usize>) -> Vec<(usize, usize)> {
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for line in values {
        if let Some((_, end)) = ranges.last_mut()
            && end.checked_add(1) == Some(line)
        {
            *end = line;
        } else {
            ranges.push((line, line));
        }
    }
    ranges
}

fn format_ranges(ranges: &[(usize, usize)]) -> String {
    if ranges.is_empty() {
        return "none".into();
    }
    ranges
        .iter()
        .map(|(first, last)| {
            if first == last {
                first.to_string()
            } else {
                format!("{first}–{last}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

impl SelectionFeedback {
    fn label(&self) -> &'static str {
        if self.diff_key.is_some() {
            "Comment on selected lines"
        } else {
            "Reply to selection"
        }
    }

    fn draft_context(&self) -> String {
        let (header, prompt) = match &self.origin {
            Origin::Message { .. } => ("Regarding this answer:".to_owned(), "Reply: "),
            Origin::Diff { path, old, new, .. } => (
                format!(
                    "Review comment on {}:\nOld lines {}; new lines {}.",
                    quoted(path),
                    format_ranges(old),
                    format_ranges(new)
                ),
                "Comment: ",
            ),
        };
        let excerpt = self
            .snippet
            .split('\n')
            .map(|line| format!("> {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        format!("{header}\n{excerpt}\n\n{prompt}")
    }
}

impl ChatView {
    /// Render near the transcript/composer. This only offers an action for one answer
    /// or one patch; selection across messages is still available to ordinary Copy.
    pub(super) fn selection_feedback(
        &self,
        look: Look,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let fragments = match self
            .transcript_selection
            .selected_fragments(cx, MAX_QUOTE_BYTES)
        {
            Ok(fragments) => fragments,
            Err(()) => {
                return Some(
                    div()
                        .px(ui_text::space(20.))
                        .py(ui_text::space(4.))
                        .text_size(ui_text::text(11.))
                        .text_color(rgb(look.colors.muted))
                        .child("Select a shorter passage to reply")
                        .into_any_element(),
                );
            }
        };
        let feedback = self.feedback_with_pending(fragments)?;
        Some(
            div()
                .id("chat-selection-feedback")
                .w_full()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(ui_text::space(8.))
                .px(ui_text::space(20.))
                .py(ui_text::space(4.))
                .child(self.feedback_button("chat-selection-reply".into(), feedback, look, cx))
                .child(
                    div()
                        .id("chat-selection-feedback-hint")
                        .min_w_0()
                        .text_size(ui_text::text(10.))
                        .text_color(rgb(look.colors.muted))
                        .child("Adds an editable source reference to your draft")
                        .test_support(),
                )
                .test_support()
                .into_any_element(),
        )
    }

    pub(super) fn diff_feedback_action(
        &self,
        key: &str,
        look: Look,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let feedback = self.current_selection_feedback(cx)?;
        (feedback.diff_key.as_deref() == Some(key))
            .then(|| self.feedback_button(format!("comment-diff:{key}").into(), feedback, look, cx))
    }

    fn feedback_button(
        &self,
        id: SharedString,
        feedback: SelectionFeedback,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let pressed_feedback = feedback.clone();
        widgets::button(id, feedback.label(), None, look)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |view, _, window, cx| {
                    // Base clears pointer selection in the capture phase. Retain the
                    // validated action through this press so it stays mounted until click.
                    view.feedback_pending = Some(pressed_feedback.clone());
                    window.prevent_default();
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|view, _, _, cx| {
                    view.feedback_pending = None;
                    cx.notify();
                }),
            )
            .on_click(cx.listener(move |view, _, window, cx| {
                view.stage_feedback(&feedback, window, cx);
            }))
            .into_any_element()
    }

    pub(super) fn current_selection_feedback(&self, cx: &gpui::App) -> Option<SelectionFeedback> {
        self.feedback_with_pending(
            self.transcript_selection
                .selected_fragments(cx, MAX_QUOTE_BYTES)
                .ok()?,
        )
    }

    fn feedback_with_pending(
        &self,
        fragments: Vec<SelectionFragment>,
    ) -> Option<SelectionFeedback> {
        if fragments.is_empty() {
            let pending = self.feedback_pending.as_ref()?;
            let current = self.feedback_for_fragments(pending.fragments.clone())?;
            (current == *pending).then_some(current)
        } else {
            self.feedback_for_fragments(fragments)
        }
    }

    pub(super) fn validate_pending_feedback(&mut self) {
        if self.feedback_pending.as_ref().is_some_and(|pending| {
            self.feedback_for_fragments(pending.fragments.clone())
                .as_ref()
                != Some(pending)
        }) {
            self.feedback_pending = None;
        }
    }

    fn feedback_for_fragments(
        &self,
        fragments: Vec<SelectionFragment>,
    ) -> Option<SelectionFeedback> {
        let first = fragments.first()?;
        let rows = self.transcript_selection.with_selection_source(|source| {
            fragments
                .iter()
                .map(|fragment| {
                    let leaf = source.iter().find(|leaf| leaf.key == fragment.key)?;
                    (leaf.text.get(fragment.range.clone())? == fragment.text).then_some(leaf.row)
                })
                .collect::<Option<Vec<_>>>()
        })?;
        if first.key.starts_with("diff:") {
            return self.diff_selection(fragments);
        }
        if !rows.iter().all(|row| *row == rows[0]) {
            return None;
        }
        let (_, item) = self.projected_item(&self.list_rows.get(rows[0])?.row.id)?;
        let ItemBody::AgentMessage { text } = &item.body else {
            return None;
        };
        // The same virtual row can also contain its changed-files section. It is not
        // part of the answer's rendered prose, even though it shares that row number.
        let prose = format!("{}/", item.id);
        if !fragments.iter().all(|fragment| {
            fragment.key.starts_with(&prose)
                || fragment.key.starts_with(&format!("code:{prose}"))
                || fragment.key.starts_with(&format!("callout:{prose}"))
        }) {
            return None;
        }
        let snippet = fragments
            .iter()
            .map(|part| part.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        Some(SelectionFeedback {
            chat: self.chat_id.clone(),
            fragments,
            origin: Origin::Message {
                item: item.id.clone(),
                turn: item.turn_id.clone(),
                revision: revision(text),
            },
            snippet,
            diff_key: None,
        })
    }

    pub(super) fn diff_source(&self, key: &str) -> Option<(&Item, usize, &FileChange)> {
        if let Some(key) = key.strip_prefix("answer-file:") {
            let (answer, at) = key.rsplit_once(':')?;
            let answer = self
                .model
                .transcript
                .items
                .iter()
                .find(|item| item.id == answer)?;
            let change = *self.answer_files(answer).get(at.parse::<usize>().ok()?)?;
            // The answer review holds references into the transcript. Recover the edit
            // item's stable identity instead of treating the sorted UI index as a source.
            self.model.transcript.items.iter().find_map(|item| {
                let ItemBody::FileChange { changes } = &item.body else {
                    return None;
                };
                changes
                    .iter()
                    .enumerate()
                    .find(|(_, candidate)| std::ptr::eq(*candidate, change))
                    .map(|(at, change)| (item, at, change))
            })
        } else {
            let (id, at) = key.rsplit_once('#')?;
            let item = self
                .model
                .transcript
                .items
                .iter()
                .find(|item| item.id == id)?;
            let ItemBody::FileChange { changes } = &item.body else {
                return None;
            };
            let at = at.parse().ok()?;
            Some((item, at, changes.get(at)?))
        }
    }

    fn diff_selection(&self, fragments: Vec<SelectionFragment>) -> Option<SelectionFeedback> {
        let (key, _) = fragments
            .first()?
            .key
            .strip_prefix("diff:")?
            .rsplit_once(':')?;
        let indices = fragments
            .iter()
            .map(|fragment| {
                let (group, at) = fragment.key.strip_prefix("diff:")?.rsplit_once(':')?;
                (group == key).then(|| at.parse::<usize>().ok()).flatten()
            })
            .collect::<Option<Vec<_>>>()?;
        if indices
            .windows(2)
            .any(|pair| pair[1] != pair[0].checked_add(1).unwrap_or(usize::MAX))
        {
            return None;
        }
        let first = *indices.first()?;
        let last = *indices.last()?;
        if last.checked_sub(first)? >= MAX_DIFF_LINES {
            return None;
        }
        let (item, change_at, change) = self.diff_source(key)?;
        let patch = change.diff.as_deref()?;
        let (lines, _) = diff::display(patch, change.kind);
        let selected = lines.get(first..=last)?;
        let numbers = diff::line_numbers(&lines, change.kind);
        let range = |old: bool| {
            line_ranges(
                numbers[first..=last]
                    .iter()
                    .filter_map(|(a, b)| if old { *a } else { *b }),
            )
        };
        let snippet = selected
            .iter()
            .map(|line| line.text)
            .collect::<Vec<_>>()
            .join("\n");
        if snippet.len() > MAX_QUOTE_BYTES {
            return None;
        }
        Some(SelectionFeedback {
            chat: self.chat_id.clone(),
            origin: Origin::Diff {
                item: item.id.clone(),
                turn: item.turn_id.clone(),
                change: change_at,
                path: change.path.clone(),
                kind: change.kind,
                revision: revision(patch),
                first,
                last,
                old: range(true),
                new: range(false),
            },
            diff_key: Some(key.to_owned()),
            snippet,
            fragments,
        })
    }

    pub(super) fn stage_feedback(
        &mut self,
        feedback: &SelectionFeedback,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.feedback_pending = None;
        self.refresh_selection_document(Look::of(cx), cx);
        // Recheck exact selected bytes plus whole-source revision, even if a queued click
        // outlived a streaming update or a changed answer's sorted file list.
        if self
            .feedback_for_fragments(feedback.fragments.clone())
            .as_ref()
            != Some(feedback)
        {
            cx.notify();
            return false;
        }
        let reference = DraftReference {
            id: uuid::Uuid::new_v4().to_string(),
            chat: feedback.chat.clone(),
            origin: feedback.origin.clone(),
            original_origin: None,
            excerpt: feedback.snippet.clone(),
            quote: feedback.snippet.clone(),
            comment: String::new(),
            state: ReferenceState::Current,
            fragments: feedback
                .fragments
                .iter()
                .map(|fragment| ReferenceFragment {
                    key: fragment.key.clone(),
                    start: fragment.range.start,
                    end: fragment.range.end,
                    text: fragment.text.clone(),
                })
                .collect(),
        };
        if !self.add_reference(reference, window, cx) {
            return false;
        }
        self.transcript_selection.clear(self.window_handle, cx);
        cx.notify();
        true
    }

    /// Focused review uses real reported edit identity and displayed patch line
    /// indices; no sorted UI index or workspace file content becomes provenance.
    pub(super) fn add_review_reference(
        &mut self,
        item_id: &str,
        change_index: usize,
        first: usize,
        last: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if last < first || last - first >= MAX_DIFF_LINES {
            return false;
        }
        let Some(item) = self
            .model
            .transcript
            .items
            .iter()
            .find(|item| item.id == item_id)
        else {
            return false;
        };
        let ItemBody::FileChange { changes } = &item.body else {
            return false;
        };
        let Some(change) = changes.get(change_index) else {
            return false;
        };
        let Some(patch) = change.diff.as_deref() else {
            return false;
        };
        let (lines, _) = diff::display(patch, change.kind);
        let Some(selected) = lines.get(first..=last) else {
            return false;
        };
        let snippet = selected
            .iter()
            .map(|line| line.text)
            .collect::<Vec<_>>()
            .join("\n");
        if snippet.len() > MAX_QUOTE_BYTES {
            return false;
        }
        let numbers = diff::line_numbers(&lines, change.kind);
        let reference = DraftReference {
            id: uuid::Uuid::new_v4().to_string(),
            chat: self.chat_id.clone(),
            original_origin: None,
            origin: Origin::Diff {
                item: item.id.clone(),
                turn: item.turn_id.clone(),
                change: change_index,
                path: change.path.clone(),
                kind: change.kind,
                revision: revision(patch),
                first,
                last,
                old: line_ranges(numbers[first..=last].iter().filter_map(|(old, _)| *old)),
                new: line_ranges(numbers[first..=last].iter().filter_map(|(_, new)| *new)),
            },
            excerpt: snippet.clone(),
            quote: snippet,
            comment: String::new(),
            fragments: Vec::new(),
            state: ReferenceState::Current,
        };
        self.add_reference(reference, window, cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_comments_describe_disjoint_hunks_without_claiming_the_gap() {
        let patch = "@@ -1,2 +1,2 @@\n keep\n-before\n+after\n@@ -500 +600 @@\n-old\n+new";
        let (lines, _) = diff::display(patch, ChangeKind::Modify);
        let coordinates = diff::line_numbers(&lines, ChangeKind::Modify);
        let feedback = SelectionFeedback {
            chat: None,
            fragments: Vec::new(),
            origin: Origin::Diff {
                item: "private-source-id".into(),
                turn: Some("private-turn-id".into()),
                change: 0,
                path: "src/main.rs".into(),
                kind: ChangeKind::Modify,
                revision: revision(patch),
                first: 0,
                last: lines.len() - 1,
                old: line_ranges(coordinates.iter().filter_map(|(old, _)| *old)),
                new: line_ranges(coordinates.iter().filter_map(|(_, new)| *new)),
            },
            snippet: patch.into(),
            diff_key: Some("render-only-key".into()),
        };
        let draft = feedback.draft_context();
        assert!(
            draft.contains("Old lines 1–2, 500; new lines 1–2, 600."),
            "{draft}"
        );
        assert!(!draft.contains("1–500"));
        assert!(!draft.contains("private-source-id"));
        assert!(!draft.contains("render-only-key"));
        assert!(draft.ends_with("> +new\n\nComment: "));
        assert_eq!(format_ranges(&line_ranges([])), "none");
    }
}
