//! Deliberate code actions operate on a captured scratch copy, never on the
//! transcript or the chat draft. Exports share the plan's confirmed atomic writer.
use super::{
    ChatView, ChatViewEvent,
    notices::LocalKey,
    plan_document,
    widgets::{self, Look},
};
use crate::{
    chat::model::{ItemStatus, NoticeLevel},
    text_input::{self, EnterBehavior, TextareaState},
    ui_text,
};
use gpui::{
    AnyElement, Context, Entity, Focusable, PromptButton, PromptLevel, SharedString, Window, div,
    prelude::*, rgb,
};
use gpui_kit::base::TestSupportExt as _;
use std::path::PathBuf;

#[cfg(test)]
mod tests;

pub(super) const MAX_SNIPPET: usize = 256 * 1024;
pub(super) struct TerminalPicker {
    text: String,
    targets: Vec<(String, String)>,
    focus: gpui::FocusHandle,
}
pub(super) struct Overlay {
    original: String,
    name: String,
    editor: Entity<TextareaState>,
    closing: bool,
}

fn shell(language: Option<&str>) -> bool {
    language.is_some_and(|language| {
        matches!(
            language.to_ascii_lowercase().as_str(),
            "sh" | "bash" | "zsh" | "shell" | "shellscript" | "fish" | "powershell" | "pwsh"
        )
    })
}

fn snippet_name(title: Option<&str>, language: Option<&str>) -> String {
    if let Some(name) = title
        .and_then(|title| std::path::Path::new(title).file_name())
        .and_then(|name| name.to_str())
    {
        let name: String = name
            .chars()
            .filter(|c| !c.is_control() && !matches!(c, '/' | '\\' | ':'))
            .take(100)
            .collect();
        if !name.is_empty() && name != "." && name != ".." {
            return name;
        }
    }
    let ext = match language.unwrap_or("").to_ascii_lowercase().as_str() {
        "rust" | "rs" => "rs",
        "python" | "py" => "py",
        "javascript" | "js" => "js",
        "typescript" | "ts" => "ts",
        "json" => "json",
        "html" => "html",
        "css" => "css",
        "sh" | "shell" | "bash" | "zsh" => "sh",
        "fish" => "fish",
        "powershell" | "pwsh" => "ps1",
        "swift" => "swift",
        "yaml" | "yml" => "yaml",
        "mermaid" => "mmd",
        "sql" => "sql",
        _ => "txt",
    };
    format!("snippet.{ext}")
}

impl ChatView {
    pub(super) fn focused_output_editor(
        &self,
        window: &Window,
        cx: &gpui::App,
    ) -> Option<Entity<TextareaState>> {
        self.output_overlay
            .as_ref()
            .map(|overlay| &overlay.editor)
            .filter(|editor| editor.read(cx).focus_handle(cx).is_focused(window))
            .cloned()
    }
    pub(crate) fn choose_snippet_terminal(
        &mut self,
        text: String,
        targets: Vec<(String, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if text.len() > MAX_SNIPPET {
            return;
        }
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        self.focus_composer = false;
        self.transcript_selection.clear(self.window_handle, cx);
        self.terminal_picker = Some(TerminalPicker {
            text,
            targets,
            focus,
        });
        cx.notify();
    }
    pub(super) fn completed_code(&self, key: &str) -> bool {
        // Item IDs may themselves contain separators. The most specific source
        // owns the block; a completed prefix item cannot enable a streaming child.
        // An owner is the key up to a separator, so each candidate is looked up
        // rather than every item compared: this runs per code block per frame.
        fn owners<'a>(
            rest: &'a str,
            separator: &'static str,
            prefix: usize,
        ) -> impl Iterator<Item = (&'a str, usize)> {
            rest.match_indices(separator)
                .map(move |(at, _)| (&rest[..at], at + prefix))
        }
        let direct = owners(key, "/", 0);
        let user = key
            .strip_prefix("user:")
            .into_iter()
            .flat_map(|rest| owners(rest, "/", 5));
        let plan = key
            .strip_prefix("plan:")
            .into_iter()
            .flat_map(|rest| owners(rest, ":document/", 14));
        direct
            .chain(user)
            .chain(plan)
            .filter_map(|(id, length)| {
                let (at, item) = self.model.item(id)?;
                // Of equally specific owners, the later item, as a scan would find.
                Some(((length, at), item.status))
            })
            .max_by_key(|(rank, _)| *rank)
            .is_some_and(|(_, status)| status != ItemStatus::InProgress)
    }

    pub(super) fn code_actions(
        &self,
        language: Option<&str>,
        title: Option<&str>,
        code: &str,
        key: &str,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let completed = self.completed_code(key);
        let bounded = code.len() <= MAX_SNIPPET;
        let name = snippet_name(title, language);
        let reader_source = if bounded && completed {
            code.to_owned()
        } else {
            String::new()
        };
        let reader_name = name.clone();
        let save_source = reader_source.as_bytes().to_vec();
        let terminal_source = reader_source.clone();
        div()
            .id(format!("code-actions:{key}"))
            .flex()
            .flex_wrap()
            .items_center()
            .gap(ui_text::space(4.))
            .child(
                widgets::button(
                    SharedString::from(format!("read-code:{key}")),
                    "Reader",
                    None,
                    look,
                )
                .disabled(!completed || !bounded)
                .on_click(cx.listener(move |view, _, window, cx| {
                    view.open_code_reader(reader_name.clone(), reader_source.clone(), window, cx)
                })),
            )
            .child(
                widgets::button(
                    SharedString::from(format!("save-code:{key}")),
                    "Save…",
                    None,
                    look,
                )
                .disabled(!completed || !bounded)
                .on_click(cx.listener(move |view, _, window, cx| {
                    view.export_bytes(save_source.clone(), name.clone(), window, cx)
                })),
            )
            .children(shell(language).then(|| {
                widgets::button(
                    SharedString::from(format!("terminal-code:{key}")),
                    "Copy & open terminal…",
                    None,
                    look,
                )
                .disabled(!completed || !bounded)
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.emit(ChatViewEvent::CodeToTerminal {
                        text: terminal_source.clone(),
                    })
                }))
            }))
            .children((!bounded).then(|| {
                div()
                    .text_size(ui_text::text(10.))
                    .child("Reader/export limit: 256 KiB")
            }))
            .test_support()
            .into_any_element()
    }

    fn open_code_reader(
        &mut self,
        name: String,
        source: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if source.len() > MAX_SNIPPET {
            return;
        }
        let editor = text_input::multiline(
            source.clone(),
            "Snippet",
            12,
            100,
            EnterBehavior::Newline,
            window,
            cx,
        );
        self.notices.clear(LocalKey::Export);
        self.focus_composer = false;
        self.output_overlay = Some(Overlay {
            original: source,
            name,
            editor: editor.clone(),
            closing: false,
        });
        self.transcript_selection.clear(self.window_handle, cx);
        editor.read(cx).focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    pub(super) fn close_output_overlay(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.terminal_picker.take().is_some() {
            self.focus(window, cx);
            cx.notify();
            return true;
        }
        if !self.file_action_targets.is_empty() {
            self.file_action_targets.clear();
            self.file_action_page = 0;
            self.notices.clear(LocalKey::Export);
            self.focus(window, cx);
            cx.notify();
            return true;
        }
        let Some(overlay) = &mut self.output_overlay else {
            return false;
        };
        if overlay.closing {
            return true;
        }
        if overlay.editor.read(cx).value().as_ref() == overlay.original {
            self.output_overlay = None;
            self.focus(window, cx);
            cx.notify();
            return true;
        }
        overlay.closing = true;
        let editor = overlay.editor.clone();
        let answer = window.prompt(
            PromptLevel::Warning,
            "Close this edited snippet?",
            Some(
                "Copy or save the scratch copy to keep your edits. The conversation is unchanged.",
            ),
            &[
                PromptButton::cancel("Keep editing"),
                PromptButton::ok("Close"),
            ],
            cx,
        );
        self.attachment_tasks
            .push(cx.spawn_in(window, async move |this, cx| {
                let close = answer.await == Ok(1);
                // The view may have moved while its old window's prompt was open.
                // Settle entity state independently of that window's lifetime.
                let closed = this.update(cx, |view, cx| {
                    if let Some(overlay) = &mut view.output_overlay
                        && overlay.editor == editor
                    {
                        overlay.closing = false;
                        if close {
                            view.output_overlay = None;
                        }
                        cx.notify();
                        return close;
                    }
                    false
                });
                if closed.is_ok_and(|closed| closed) {
                    let _ = this.update_in(cx, |view, window, cx| view.focus(window, cx));
                }
            }));
        true
    }

    pub(super) fn code_reader(
        &self,
        look: Look,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if let Some(picker) = &self.terminal_picker {
            return Some(div().id("snippet-terminal-picker").track_focus(&picker.focus).absolute().inset_0().occlude().bg(rgb(look.colors.panel))
                .p(ui_text::space(12.)).flex().flex_col().gap(ui_text::space(8.))
                .child(div().flex().child(div().flex_1().child("Copy snippet and open a terminal"))
                    .child(widgets::button("cancel-snippet-terminal","Cancel",None,look).on_click(cx.listener(|view,_,window,cx| {view.close_output_overlay(window,cx); }))))
                .child("Choose a terminal. The snippet will be copied for you to paste when ready.")
                .children(picker.targets.is_empty().then(||div().child("No local terminals are open in this project window.")))
                .child(div().id("snippet-terminal-list").flex_1().min_h_0().overflow_y_scroll().children(picker.targets.iter().enumerate().map(|(at,(shell_id,label))| {
                    let shell_id=shell_id.clone(); let text=picker.text.clone();
                    widgets::button(SharedString::from(format!("snippet-terminal:{at}")),label.clone(),None,look).w_full()
                        .on_click(cx.listener(move |view,_,_,cx| { view.terminal_picker=None; cx.emit(ChatViewEvent::FocusCodeTerminal{shell_id:shell_id.clone(),text:text.clone()}); cx.notify(); }))
                }))).test_support().into_any_element());
        }
        let overlay = self.output_overlay.as_ref()?;
        Some(
            div()
                .id("chat-code-reader")
                .absolute()
                .inset_0()
                .occlude()
                .bg(rgb(look.colors.panel))
                .p(ui_text::space(12.))
                .flex()
                .flex_col()
                .gap(ui_text::space(8.))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(ui_text::space(6.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(format!("{} · editable copy", overlay.name)),
                        )
                        .child(
                            widgets::button(
                                "copy-code-reader",
                                if self.copied.as_deref() == Some("copy-code-reader") {
                                    "Copied"
                                } else {
                                    "Copy"
                                },
                                None,
                                look,
                            )
                            .on_click(cx.listener(|view, _, _, cx| {
                                if let Some(overlay) = &view.output_overlay {
                                    let text = overlay.editor.read(cx).value().to_string();
                                    view.copy("copy-code-reader".into(), text, cx);
                                }
                            })),
                        )
                        .child(
                            widgets::button("save-code-reader", "Save…", None, look).on_click(
                                cx.listener(|view, _, window, cx| {
                                    if let Some(overlay) = &view.output_overlay {
                                        let text =
                                            overlay.editor.read(cx).value().as_bytes().to_vec();
                                        let name = overlay.name.clone();
                                        view.export_bytes(text, name, window, cx);
                                    }
                                }),
                            ),
                        )
                        .child(
                            widgets::button("close-code-reader", "Close", None, look).on_click(
                                cx.listener(|view, _, window, cx| {
                                    view.close_output_overlay(window, cx);
                                }),
                            ),
                        ),
                )
                .children(self.notices.local(LocalKey::Export).map(|notice| {
                    div()
                        .id("code-reader-export-notice")
                        .text_size(ui_text::text(11.))
                        .child(notice.to_owned())
                        .test_support()
                }))
                .child(
                    div()
                        .id("code-reader-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .font_family(ui_text::code_family())
                        .child(
                            text_input::textarea("code-reader-editor", &overlay.editor, window, cx)
                                .w_full(),
                        ),
                )
                .test_support()
                .into_any_element(),
        )
    }

    pub(super) fn export_bytes(
        &mut self,
        bytes: Vec<u8>,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if bytes.len() > 32 * 1024 * 1024 {
            self.notices.set(
                LocalKey::Export,
                NoticeLevel::Error,
                "This export exceeds the 32 MiB limit.",
            );
            cx.notify();
            return;
        }
        let directory = self
            .info()
            .map(|info| info.cwd.clone())
            .unwrap_or_else(std::env::temp_dir);
        let selected = cx.prompt_for_new_path(&directory, Some(&name));
        let executor = cx.background_executor().clone();
        self.attachment_tasks
            .push(cx.spawn_in(window, async move |this, cx| {
                let path = match selected.await {
                    Ok(Ok(Some(path))) => path,
                    Ok(Ok(None)) => return,
                    other => {
                        let _ = this.update(cx, |view, cx| {
                            view.export_result(
                                Err(format!("Cannot choose export destination: {other:?}")),
                                cx,
                            )
                        });
                        return;
                    }
                };
                let saved_path = path.clone();
                let destination = match executor
                    .spawn(async move { plan_document::export_destination(path) })
                    .await
                {
                    Ok(value) => value,
                    Err(error) => {
                        let _ = this.update(cx, |view, cx| view.export_result(Err(error), cx));
                        return;
                    }
                };
                let replace = destination.replaces_file();
                if replace {
                    let Ok(answer) = this.update_in(cx, |_, window, cx| {
                        window.prompt(
                            PromptLevel::Warning,
                            "Replace the existing file?",
                            Some(&saved_path.display().to_string()),
                            &[PromptButton::cancel("Cancel"), PromptButton::ok("Replace")],
                            cx,
                        )
                    }) else {
                        return;
                    };
                    if answer.await != Ok(1) {
                        return;
                    }
                }
                if this.update(cx, |_, _| ()).is_err() {
                    return;
                }
                let result = executor
                    .spawn(async move {
                        plan_document::write_bytes(&destination, &bytes, replace)
                            .map(|()| saved_path)
                    })
                    .await;
                let _ = this.update(cx, |view, cx| view.export_result(result, cx));
            }));
    }

    fn export_result(&mut self, result: Result<PathBuf, String>, cx: &mut Context<Self>) {
        match result {
            Ok(path) => self.notices.set(
                LocalKey::Export,
                NoticeLevel::Info,
                format!("Saved {}", path.display()),
            ),
            Err(error) => self
                .notices
                .set(LocalKey::Export, NoticeLevel::Error, error),
        }
        cx.notify();
    }
}
