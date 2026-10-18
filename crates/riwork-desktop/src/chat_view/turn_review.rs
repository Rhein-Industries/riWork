//! A completed answer's reported file edits. Repeated edits are not added together:
//! without workspace checkpoints their patch counts are not a net turn diff.
use super::{
    ChatView, ChatViewEvent, DisplayMode,
    widgets::{self, Look},
};
use crate::{
    chat::model::{FileChange, Item, ItemBody, ItemStatus, MessagePhase},
    ui_text,
};
use gpui::{AnyElement, Context, SharedString, div, prelude::*, rgb};
use std::collections::BTreeMap;

pub(super) fn reported_files<'a>(items: &'a [Item], answer: usize) -> Vec<&'a FileChange> {
    let Some(item) = items.get(answer) else {
        return Vec::new();
    };
    let Some(turn) = item.turn_id.as_deref() else {
        return Vec::new();
    };
    let mut files = BTreeMap::new();
    for item in items {
        if item.turn_id.as_deref() != Some(turn) || item.status != ItemStatus::Completed {
            continue;
        }
        if let ItemBody::FileChange { changes } = &item.body {
            for change in changes {
                files.insert(change.path.as_str(), change);
            }
        }
    }
    files.into_values().collect()
}

pub(super) fn file_key(answer: &str, at: usize) -> String {
    format!("answer-file:{answer}:{at}")
}

impl ChatView {
    pub(super) fn review_depends_on(
        &self,
        at: usize,
        touched: &std::collections::BTreeSet<usize>,
    ) -> bool {
        let Some(item) = self.model.transcript.items.get(at) else {
            return false;
        };
        if !matches!(item.body, ItemBody::AgentMessage { .. })
            || self.display_mode != DisplayMode::Normal
        {
            return false;
        }
        let Some(turn) = item.turn_id.as_deref() else {
            return false;
        };
        touched.iter().any(|at| {
            self.model.transcript.items.get(*at).is_some_and(|changed| {
                changed.turn_id.as_deref() == Some(turn)
                    && matches!(changed.body, ItemBody::FileChange { .. })
            })
        })
    }

    pub(super) fn answer_files(&self, item: &Item) -> Vec<&FileChange> {
        if self.display_mode != DisplayMode::Normal {
            return Vec::new();
        }
        let Some(turn) = item.turn_id.as_deref() else {
            return Vec::new();
        };
        let Some((_, _, end)) = self.model.completed.iter().find(|(id, _, _)| id == turn) else {
            return Vec::new();
        };
        let Some(last) = self.model.transcript.items
            [..(*end).min(self.model.transcript.items.len())]
            .iter()
            .rposition(|candidate| {
                candidate.turn_id.as_deref() == Some(turn)
                    && matches!(candidate.body, ItemBody::AgentMessage { .. })
                    && candidate.presentation.phase != Some(MessagePhase::Commentary)
            })
        else {
            return Vec::new();
        };
        if self.model.transcript.items[last].id != item.id {
            return Vec::new();
        }
        reported_files(
            &self.model.transcript.items[..(*end).min(self.model.transcript.items.len())],
            last,
        )
    }

    pub(super) fn answer_review(
        &self,
        item: &Item,
        look: Look,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let files = self.answer_files(item);
        if files.is_empty() {
            return None;
        }
        let count = files.len();
        let rows = files.into_iter().enumerate().map(|(at, change)| {
            let key = file_key(&item.id, at);
            let path = change.path.clone();
            let target = path.clone();
            let can_diff = change
                .diff
                .as_ref()
                .is_some_and(|diff| !diff.trim().is_empty());
            let expanded = self.open.contains(&key);
            let toggle_key = key.clone();
            let copy_key = format!("copy:{key}");
            div()
                .w_full()
                .min_w_0()
                .border_t_1()
                .border_color(rgb(look.colors.divider))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(ui_text::space(6.))
                        .px(ui_text::space(10.))
                        .py(ui_text::space(6.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(ui_text::text(11.))
                                .font_family(ui_text::code_family())
                                .child(self.selectable(
                                    &format!("path:{key}"),
                                    path.clone(),
                                    Vec::new(),
                                    vec![(0..path.len(), path.clone())],
                                    look,
                                    cx,
                                )),
                        )
                        .child(
                            widgets::button(
                                SharedString::from(format!("open:{key}")),
                                "Open",
                                None,
                                look,
                            )
                            .on_click(cx.listener(
                                move |_, _, _, cx| {
                                    cx.emit(ChatViewEvent::OpenFile {
                                        target: target.clone(),
                                    })
                                },
                            )),
                        )
                        .child(
                            widgets::copy_button(
                                SharedString::from(copy_key.clone()),
                                self.copied.as_deref() == Some(copy_key.as_str()),
                                "Copy path",
                                "Copy file path",
                                look,
                            )
                            .on_click(cx.listener(
                                move |view, _, _, cx| view.copy(copy_key.clone(), path.clone(), cx),
                            )),
                        )
                        .when(can_diff, |row| {
                            row.child(
                                widgets::button(
                                    SharedString::from(format!("edit:{key}")),
                                    if expanded { "Hide edit" } else { "Show edit" },
                                    None,
                                    look,
                                )
                                .aria_expanded(expanded)
                                .on_click(cx.listener(
                                    move |view, _, _, cx| {
                                        view.toggle(&toggle_key, None, cx);
                                        view.list.remeasure();
                                    },
                                )),
                            )
                        }),
                )
                .children(
                    change
                        .diff
                        .as_ref()
                        .filter(|_| expanded)
                        .map(|text| self.diff_lines(&key, text, change.kind, look, cx)),
                )
        });
        Some(
            div()
                .mt(ui_text::space(14.))
                .w_full()
                .min_w_0()
                .border_1()
                .border_color(rgb(look.colors.divider))
                .rounded(ui_text::space(6.))
                .child(
                    div()
                        .px(ui_text::space(10.))
                        .py(ui_text::space(7.))
                        .text_size(ui_text::text(11.))
                        .child(format!("Reported file changes · {count}")),
                )
                .children(rows)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::model::{ChangeKind, Presentation};
    use gpui::TestAppContext;
    use gpui_kit::test::TestWindowExt;

    #[gpui::test]
    fn native_answer_review_opens_wraps_and_copies_the_complete_patch(cx: &mut TestAppContext) {
        let (handle, view, recording) = super::super::editor_tests::mount_selection(cx);
        let patch = format!("@@ -1 +1 @@\n-old\n+{}", "long new text ".repeat(25));
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |view, cx| {
                view.model.link = super::super::Link::Live;
                view.display_mode = DisplayMode::Normal;
                view.model.transcript.items = vec![
                    Item {
                        id: "edit".into(),
                        turn_id: Some("turn".into()),
                        status: ItemStatus::Completed,
                        presentation: Presentation::default(),
                        body: ItemBody::FileChange {
                            changes: vec![FileChange {
                                path: "src/main.rs".into(),
                                kind: ChangeKind::Modify,
                                diff: Some(patch.clone()),
                            }],
                        },
                    },
                    Item {
                        id: "answer".into(),
                        turn_id: Some("turn".into()),
                        status: ItemStatus::Completed,
                        presentation: Presentation {
                            phase: Some(MessagePhase::Final),
                            ..Presentation::default()
                        },
                        body: ItemBody::AgentMessage {
                            text: "Updated the file.".into(),
                        },
                    },
                ];
                view.model.completed =
                    vec![("turn".into(), crate::chat::model::TurnOutcome::Completed, 2)];
                view.refresh_projection(cx);
                view.list.remeasure();
                cx.notify();
            });
            window.render_frame(cx);
            assert_eq!(
                window.find("transcript:path:answer-file:answer:0").label(),
                Some("src/main.rs")
            );
            window.click("edit:answer-file:answer:0", cx);
            window.render_frame(cx);
            let before = window
                .find("transcript:diff:answer-file:answer:0:2")
                .bounds()
                .size
                .height;
            window.click("wrap-diff:answer-file:answer:0", cx);
            window.render_frame(cx);
            let after = window
                .find("transcript:diff:answer-file:answer:0:2")
                .bounds()
                .size
                .height;
            assert!(after > before, "long patch line must reflow");
            window.click("copy-diff:answer-file:answer:0", cx);
            assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), patch);
        })
        .unwrap();
        assert!(recording.try_recv().is_err());
    }
    #[test]
    fn latest_reported_edit_is_retained_without_summing_repeated_patches() {
        let edit = |id: &str, turn: &str, patch: &str, status| Item {
            id: id.into(),
            turn_id: Some(turn.into()),
            status,
            presentation: Presentation::default(),
            body: ItemBody::FileChange {
                changes: vec![FileChange {
                    path: "src/main.rs".into(),
                    kind: ChangeKind::Modify,
                    diff: Some(patch.into()),
                }],
            },
        };
        let mut items = vec![
            edit("a", "t", "+first", ItemStatus::Completed),
            edit("b", "t", "+second", ItemStatus::Completed),
            edit("c", "other", "+other", ItemStatus::Completed),
            edit("d", "t", "+failed", ItemStatus::Failed),
        ];
        items.push(Item {
            id: "answer".into(),
            turn_id: Some("t".into()),
            status: ItemStatus::Completed,
            presentation: Presentation::default(),
            body: ItemBody::AgentMessage {
                text: "Done".into(),
            },
        });
        let files = reported_files(&items, 4);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].diff.as_deref(), Some("+second"));
        // Some providers finish an edit after emitting their final message.
        items.push(edit("late", "t", "+late", ItemStatus::Completed));
        assert_eq!(reported_files(&items, 4)[0].diff.as_deref(), Some("+late"));
    }
}
