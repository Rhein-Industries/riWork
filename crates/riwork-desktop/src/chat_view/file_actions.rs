//! Scoped local-link actions. Validation runs off the UI thread, against the
//! chat's registered roots, with the same no-follow regular-file walk as Files.
use super::{
    ChatView, ChatViewEvent,
    image_scope::ImageScope,
    notices::LocalKey,
    widgets::{self, Look},
};
use crate::{chat::model::NoticeLevel, ui_text};
use gpui::{AnyElement, ClipboardItem, Context, SharedString, Window, div, prelude::*, rgb};
use gpui_kit::base::TestSupportExt as _;
use std::path::{Path, PathBuf};

const PAGE_SIZE: usize = 20;
#[derive(Clone, Copy)]
enum Action {
    Relative,
    Full,
    Reveal,
    Editor,
    Preview,
}

fn page_range(count: usize, page: usize) -> std::ops::Range<usize> {
    let start = page.min(count.saturating_sub(1) / PAGE_SIZE) * PAGE_SIZE;
    start..count.min(start.saturating_add(PAGE_SIZE))
}

fn relative_path(base: &Path, path: &Path) -> PathBuf {
    let base: Vec<_> = base.components().collect();
    let path: Vec<_> = path.components().collect();
    let shared = base.iter().zip(&path).take_while(|(a, b)| a == b).count();
    if shared == 0 {
        return path.iter().collect();
    }
    let mut relative = PathBuf::new();
    for _ in shared..base.len() {
        relative.push("..");
    }
    for part in &path[shared..] {
        relative.push(part);
    }
    if relative.as_os_str().is_empty() {
        relative.push(".");
    }
    relative
}

fn validated(
    scope: &ImageScope,
    cwd: Option<&Path>,
    target: &str,
    action: Action,
) -> Result<PathBuf, String> {
    let resolved = scope.resolve(target)?;
    let path = crate::file_preview::validated_live_editor_path(&resolved.root, &resolved.path)?;
    if matches!(action, Action::Editor) && crate::terminal_links::open_refusal(&path).is_some() {
        return Err("This file type cannot be opened from chat. Use Reveal in Finder.".into());
    }
    if matches!(action, Action::Preview) && !scope.in_files(target) {
        return Err("Open in Files is only available inside this chat’s Files workspace.".into());
    }
    if matches!(action, Action::Relative) {
        // Compare canonical forms, including macOS's /var alias.
        let base = cwd
            .ok_or("This chat has no working directory for relative paths.")?
            .canonicalize()
            .map_err(|_| "The chat's working directory is no longer available.")?;
        return Ok(relative_path(&base, &path));
    }
    Ok(path)
}

impl ChatView {
    fn open_file_actions(
        &mut self,
        mut targets: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        targets.retain(|target| !crate::terminal_links::is_openable_url(target));
        targets.sort();
        targets.dedup();
        if targets.is_empty() {
            return;
        }
        self.file_action_targets = targets;
        self.file_action_generation = self.file_action_generation.wrapping_add(1);
        self.file_action_page = 0;
        self.focus_composer = false;
        self.transcript_selection.clear(self.window_handle, cx);
        self.notices.clear(LocalKey::Export);
        self.file_action_focus
            .get_or_insert_with(|| cx.focus_handle())
            .focus(window, cx);
        cx.notify();
    }

    pub(super) fn file_actions_button(
        &self,
        key: &str,
        mut targets: Vec<String>,
        look: Look,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        targets.retain(|target| !crate::terminal_links::is_openable_url(target));
        targets.sort();
        targets.dedup();
        if targets.is_empty() {
            return None;
        }
        Some(
            widgets::button(
                SharedString::from(format!("file-actions:{key}")),
                "File actions…",
                None,
                look,
            )
            .on_click(cx.listener(move |view, _, window, cx| {
                view.open_file_actions(targets.clone(), window, cx)
            }))
            .into_any_element(),
        )
    }

    pub(super) fn file_actions_panel(
        &self,
        look: Look,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.file_action_targets.is_empty() {
            return None;
        }
        let range = page_range(self.file_action_targets.len(), self.file_action_page);
        let start = range.start;
        let end = range.end;
        let count = self.file_action_targets.len();
        let page_label = format!("{}–{end} of {count}", start + 1);
        Some(
            div()
                .id("chat-file-actions")
                .absolute()
                .inset_0()
                .occlude()
                .bg(rgb(look.colors.panel))
                .p(ui_text::space(12.))
                .min_w_0()
                .flex()
                .flex_col()
                .gap(ui_text::space(8.))
                .when_some(self.file_action_focus.as_ref(), |panel, focus| {
                    panel.track_focus(focus)
                })
                .child(
                    div()
                        .flex()
                        .min_w_0()
                        .items_center()
                        .gap(ui_text::space(8.))
                        .child(div().flex_1().min_w_0().child("File actions"))
                        .child(
                            widgets::button("close-file-actions", "Close", None, look).on_click(
                                cx.listener(|view, _, window, cx| {
                                    view.close_output_overlay(window, cx);
                                }),
                            ),
                        ),
                )
                .child(
                    div()
                        .id(SharedString::from(format!(
                            "file-action-list:{}",
                            self.file_action_page
                        )))
                        .flex_1()
                        .min_h_0()
                        .min_w_0()
                        .overflow_y_scroll()
                        .children(range.map(|at| {
                            let target = &self.file_action_targets[at];
                            div()
                                .id(SharedString::from(format!("file-action-row:{at}")))
                                .w_full()
                                .min_w_0()
                                .p(ui_text::space(8.))
                                .border_b_1()
                                .border_color(rgb(look.colors.divider))
                                .flex()
                                .flex_col()
                                .gap(ui_text::space(5.))
                                .child(
                                    div()
                                        .id(SharedString::from(format!("file-action-path:{at}")))
                                        .w_full()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(ui_text::text(12.))
                                        .child(target.clone())
                                        .test_support(),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .w_full()
                                        .min_w_0()
                                        .flex_wrap()
                                        .gap(ui_text::space(5.))
                                        .children(
                                            [
                                                (
                                                    Action::Relative,
                                                    "relative",
                                                    "Copy relative path",
                                                ),
                                                (Action::Full, "full", "Copy full path"),
                                                (Action::Reveal, "reveal", "Reveal in Finder"),
                                                (Action::Editor, "editor", "Open in default app"),
                                                (Action::Preview, "preview", "Open in Files"),
                                            ]
                                            .into_iter()
                                            .map(
                                                |(action, key, label)| {
                                                    let target = target.clone();
                                                    widgets::button(
                                                        SharedString::from(format!(
                                                            "file-action-{key}:{at}"
                                                        )),
                                                        label,
                                                        None,
                                                        look,
                                                    )
                                                    .max_w_full()
                                                    .whitespace_normal()
                                                    .on_click(cx.listener(
                                                        move |view, _, window, cx| {
                                                            view.file_action(
                                                                target.clone(),
                                                                action,
                                                                window,
                                                                cx,
                                                            )
                                                        },
                                                    ))
                                                },
                                            ),
                                        ),
                                )
                                .test_support()
                        })),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(ui_text::space(8.))
                        .child(
                            widgets::button("file-actions-previous", "Previous", None, look)
                                .disabled(start == 0)
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.file_action_page = view.file_action_page.saturating_sub(1);
                                    cx.notify();
                                })),
                        )
                        .child(
                            div()
                                .id("file-actions-page")
                                .role(gpui::Role::Label)
                                .aria_label(page_label.clone())
                                .text_size(ui_text::text(11.))
                                .child(page_label)
                                .test_support(),
                        )
                        .child(
                            widgets::button("file-actions-next", "Next", None, look)
                                .disabled(end >= count)
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.file_action_page =
                                        view.file_action_page.saturating_add(1).min(
                                            view.file_action_targets.len().saturating_sub(1)
                                                / PAGE_SIZE,
                                        );
                                    cx.notify();
                                })),
                        ),
                )
                .children(self.notices.local(LocalKey::Export).map(|message| {
                    div()
                        .id("file-actions-notice")
                        .text_size(ui_text::text(11.))
                        .child(message.to_owned())
                        .test_support()
                }))
                .test_support()
                .into_any_element(),
        )
    }

    fn file_action(
        &mut self,
        target: String,
        action: Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let scope = self.image_scope.clone();
        let expected_scope = scope.clone();
        let targets = self.file_action_targets.clone();
        let generation = self.file_action_generation;
        let cwd = self.info().map(|info| info.cwd.clone());
        let preview = target.clone();
        let work = cx
            .background_executor()
            .spawn(async move { validated(&scope, cwd.as_deref(), &target, action) });
        self.attachment_tasks
            .push(cx.spawn_in(window, async move |this, cx| {
                let result = work.await;
                let _ = this.update_in(cx, |view, window, cx| {
                    // Closing/switching the panel or changing chat scope retires pending actions.
                    if view.image_scope != expected_scope
                        || view.file_action_targets != targets
                        || view.file_action_generation != generation
                    {
                        return;
                    }
                    match result {
                        Ok(path) => match action {
                            Action::Relative | Action::Full => {
                                cx.write_to_clipboard(ClipboardItem::new_string(
                                    path.display().to_string(),
                                ));
                                view.notices.set(
                                    LocalKey::Export,
                                    NoticeLevel::Info,
                                    "Path copied.",
                                );
                            }
                            Action::Reveal => cx.reveal_path(&path),
                            Action::Editor => cx.open_with_system(&path),
                            Action::Preview => {
                                view.close_output_overlay(window, cx);
                                cx.emit(ChatViewEvent::OpenFile { target: preview });
                            }
                        },
                        Err(error) => view.notices.set(
                            LocalKey::Export,
                            NoticeLevel::Error,
                            format!("Cannot use this file: {error}"),
                        ),
                    }
                    cx.notify();
                });
            }));
    }
}

#[cfg(test)]
mod tests;
