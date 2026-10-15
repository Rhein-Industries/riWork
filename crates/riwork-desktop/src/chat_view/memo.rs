//! `/memo` in the message box: instead of sending, the chat shows its project's memo above the
//! message box, as it is saved (`riwork_core::project_memo`), with **Insert into composer**,
//! which appends it to the draft and sends nothing, and **Edit**, which opens the memo in the
//! project's settings. A chat of no project says that there is no project memo here; an empty
//! memo offers Edit only. The memo goes into the draft only when Insert is pressed: nothing
//! else of it reaches the chat or its agent.

use gpui::{AnyElement, Context, Window, div, prelude::*, px, rgb};

use riwork_core::project_memo::{self, ProjectMemo};

use super::{
    ChatView, ChatViewEvent,
    widgets::{Look, button},
};
use crate::{store::Store, ui_text};

/// What the preview shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Preview {
    /// The chat belongs to no project (the global orchestrator).
    NoProject,
    /// The project's memo could not be read; nothing can be inserted.
    Unreadable { project_id: String, error: String },
    /// The project's memo as saved, possibly empty.
    Memo {
        project_id: String,
        memo: ProjectMemo,
    },
}

/// What the preview of `project_id`'s memo is, as `store` has it.
pub(crate) fn preview(project_id: Option<&str>, store: Option<&Store>) -> Preview {
    let Some(project_id) = project_id else {
        return Preview::NoProject;
    };
    let read = store
        .ok_or_else(|| "this window cannot read project memos".to_owned())
        .and_then(Store::snapshot);
    match read {
        Ok(state) => Preview::Memo {
            project_id: project_id.to_owned(),
            memo: state.project_memo(project_id),
        },
        Err(error) => Preview::Unreadable {
            project_id: project_id.to_owned(),
            error,
        },
    }
}

impl ChatView {
    /// Where memos are read from: the window's store.
    pub fn set_memo_store(&mut self, store: Store) {
        self.memo_store = Some(store);
    }

    /// The project this chat belongs to, as its host or the window's list says.
    fn memo_project(&self) -> Option<String> {
        let info = self.info().or(self.inventory_info.as_ref())?;
        info.project_id.clone().or_else(|| {
            info.orchestrator
                .as_ref()
                .and_then(|scope| scope.project_id().map(str::to_owned))
        })
    }

    /// When the draft ends in `/memo`, take the command out of it and show the preview
    /// instead of sending. Whether it did.
    pub(super) fn open_memo_preview(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let draft = self.composer_text(cx);
        let Some(rest) = project_memo::command_rest(&draft) else {
            return false;
        };
        let rest = rest.to_owned();
        self.set_composer(rest.clone(), rest.len(), window, cx);
        self.memo_preview = Some(preview(
            self.memo_project().as_deref(),
            self.memo_store.as_ref(),
        ));
        self.menu = None;
        cx.notify();
        true
    }

    pub(super) fn close_memo_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.memo_preview = None;
        self.focus(window, cx);
        cx.notify();
    }

    /// **Insert into composer**: the memo after what the draft holds. Never sends.
    pub(super) fn insert_memo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Preview::Memo { memo, .. }) = self.memo_preview.take() else {
            return;
        };
        if !memo.is_empty() {
            let text = project_memo::append_to_draft(&self.composer_text(cx), &memo.text);
            let end = text.len();
            self.set_composer(text, end, window, cx);
        }
        self.focus(window, cx);
        cx.notify();
    }

    /// **Edit**: the window opens the memo in the project's settings.
    pub(super) fn edit_memo(&mut self, cx: &mut Context<Self>) {
        let project_id = match self.memo_preview.take() {
            Some(Preview::Memo { project_id, .. } | Preview::Unreadable { project_id, .. }) => {
                project_id
            }
            _ => return,
        };
        cx.emit(ChatViewEvent::EditProjectMemo { project_id });
        cx.notify();
    }

    /// Replace what the message box holds, as typing would (it is kept as the chat's draft),
    /// with the caret at `caret`.
    fn set_composer(
        &mut self,
        text: String,
        caret: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.composer.read(cx).value().as_ref() != text.as_str() {
            self.programmatic_changes += 1;
            self.composer
                .update(cx, |state, cx| state.replace_all(text, window, cx));
        }
        self.composer
            .update(cx, |state, cx| state.set_selected_range(caret..caret, cx));
    }

    /// The preview card above the message box, while it is open.
    pub(super) fn memo_preview_card(
        &self,
        look: Look,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let preview = self.memo_preview.as_ref()?;
        let colors = look.colors;
        let note = |text: String| {
            div()
                .px(ui_text::space(10.0))
                .py(ui_text::space(6.0))
                .text_size(ui_text::text(11.0))
                .text_color(rgb(colors.muted))
                .child(text)
        };
        let (title, body, insert, edit) = match preview {
            Preview::NoProject => (
                "No project memo here",
                note("This chat is not part of a project, so it has no project memo. Use /memo in a chat of a project to insert that project's memo.".into())
                    .into_any_element(),
                false,
                false,
            ),
            Preview::Unreadable { error, .. } => (
                "Project memo",
                note(format!("The memo could not be read: {error}")).into_any_element(),
                false,
                true,
            ),
            Preview::Memo { memo, .. } if memo.is_empty() => (
                "Project memo",
                note("This project's memo is empty. Edit it to write notes you can insert into any of its chats.".into())
                    .into_any_element(),
                false,
                true,
            ),
            Preview::Memo { memo, .. } => (
                "Project memo",
                div()
                    .id("chat-memo-preview-text")
                    .max_h(ui_text::space(220.0))
                    .overflow_y_scroll()
                    .px(ui_text::space(10.0))
                    .py(ui_text::space(6.0))
                    .font_family(ui_text::mono_family())
                    .text_size(ui_text::text(11.0))
                    .text_color(rgb(colors.text))
                    .whitespace_normal()
                    .children(memo.text.lines().map(|line| {
                        div().min_h(ui_text::space(14.0)).child(line.to_owned())
                    }))
                    .into_any_element(),
                true,
                true,
            ),
        };
        let actions = div()
            .flex()
            .flex_wrap()
            .justify_end()
            .gap(ui_text::space(6.0))
            .px(ui_text::space(10.0))
            .py(ui_text::space(6.0))
            .border_t_1()
            .border_color(rgb(colors.divider))
            .when(edit, |row| {
                row.child(
                    button("chat-memo-edit", "Edit", None, look)
                        .on_click(cx.listener(|view, _, _, cx| view.edit_memo(cx))),
                )
            })
            .when(insert, |row| {
                row.child(
                    button(
                        "chat-memo-insert",
                        "Insert into composer",
                        Some(colors.cyan),
                        look,
                    )
                    .on_click(cx.listener(|view, _, window, cx| view.insert_memo(window, cx))),
                )
            })
            .when(!insert && !edit, |row| {
                row.child(button("chat-memo-close", "Close", None, look).on_click(
                    cx.listener(|view, _, window, cx| view.close_memo_preview(window, cx)),
                ))
            });
        Some(
            div()
                .id("chat-memo-preview")
                .w_full()
                .flex()
                .flex_col()
                .rounded(px(if look.native { 8.0 } else { 3.0 }))
                .border_1()
                .border_color(rgb(colors.divider))
                .bg(rgb(colors.panel_active))
                .role(gpui::Role::Group)
                .aria_label(title)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(ui_text::space(8.0))
                        .px(ui_text::space(10.0))
                        .py(ui_text::space(4.0))
                        .border_b_1()
                        .border_color(rgb(colors.divider))
                        .text_size(ui_text::text(11.0))
                        .text_color(rgb(colors.text))
                        .when(look.native, |head| {
                            head.font_weight(gpui::FontWeight::SEMIBOLD)
                        })
                        .child(div().flex_1().child(title))
                        .child(
                            button("chat-memo-dismiss", "×", None, look)
                                .accessibility_label("Close the memo preview")
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.close_memo_preview(window, cx)
                                })),
                        ),
                )
                .child(body)
                .child(actions)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chat_of_no_project_has_no_memo_and_an_unreadable_store_says_why() {
        assert_eq!(preview(None, None), Preview::NoProject);
        match preview(Some("p"), None) {
            Preview::Unreadable { project_id, error } => {
                assert_eq!(project_id, "p");
                assert!(error.contains("cannot read"));
            }
            other => panic!("{other:?}"),
        }
    }
}
