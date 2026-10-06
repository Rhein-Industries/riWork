//! Picker, native paste, and external drop share host staging. Source paths never become inputs.
use super::{
    ChatView,
    attachment_draft::Status,
    widgets::{Look, button},
};
use crate::{
    chat::{
        attachments::{Attachment, FILE_BYTES, Preview, SEND_COUNT},
        client::Client,
    },
    ui_text,
};
use gpui::{
    AnyElement, ClipboardEntry, ClipboardItem, Context, ExternalPaths, Image, ImageFormat,
    PathPromptOptions, Window, div, img, prelude::*, rgb,
};
use std::{
    fs,
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::PathBuf,
    sync::Arc,
};
use uuid::Uuid;

#[derive(Clone)]
pub(super) enum Source {
    File(PathBuf),
    Image { name: String, image: Arc<Image> },
}
impl Source {
    fn name(&self) -> String {
        match self {
            Self::File(path) => path
                .file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy()
                .into_owned(),
            Self::Image { name, .. } => name.clone(),
        }
    }
}
pub(super) enum Stage {
    Pending,
    Ready(Attachment),
    Failed(String),
}
pub(super) struct Chip {
    pub id: String,
    pub name: String,
    pub state: Stage,
    pub open: bool,
    source: Option<Source>,
}
impl Chip {
    pub fn ready(attachment: Attachment) -> Self {
        Self {
            id: attachment.id.clone(),
            name: attachment.name.clone(),
            state: Stage::Ready(attachment),
            open: false,
            source: None,
        }
    }
    pub fn attachment(&self) -> Option<&Attachment> {
        if let Stage::Ready(a) = &self.state {
            Some(a)
        } else {
            None
        }
    }
}

/// Files win over image previews/filename text. Otherwise all images win over text.
/// Ordinary text is left to Kit. Never insert clipboard filename text as a second message.
pub(super) fn clipboard_sources(item: &ClipboardItem) -> Option<Vec<Source>> {
    let files = item
        .entries
        .iter()
        .filter_map(|e| match e {
            ClipboardEntry::ExternalPaths(paths) => Some(paths.paths()),
            _ => None,
        })
        .flatten()
        .cloned()
        .map(Source::File)
        .collect::<Vec<_>>();
    if !files.is_empty() {
        return Some(files);
    }
    let images = item
        .entries
        .iter()
        .filter_map(|e| match e {
            ClipboardEntry::Image(image) => {
                let extension = match image.format() {
                    ImageFormat::Png => "png",
                    ImageFormat::Jpeg => "jpg",
                    _ => "unsupported",
                };
                Some(Source::Image {
                    name: format!("Clipboard image {}.{extension}", Uuid::new_v4()),
                    image: Arc::new(image.clone()),
                })
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    (!images.is_empty()).then_some(images)
}

fn stage_source(
    ensure: &super::feed::Ensure,
    chat: &str,
    source: &Source,
) -> Result<Attachment, String> {
    let name = source.name();
    let socket = ensure().map_err(|e| format!("Attachment {name}: {e}"))?;
    let mut client = Client::connect(&socket).map_err(|e| format!("Attachment {name}: {e}"))?;
    match source {
        Source::File(path) => client
            .stage_attachment(chat, path)
            .map_err(|e| format!("Attachment {name}: {e}")),
        Source::Image { name, image } => {
            if !matches!(image.format(), ImageFormat::Png | ImageFormat::Jpeg) {
                return Err(format!(
                    "Attachment {name}: clipboard format is unsupported; use static PNG or JPEG"
                ));
            }
            if image.bytes().len() as u64 > FILE_BYTES {
                return Err(format!("Attachment {name}: exceeds 4 MiB"));
            }
            // Child-owned scratch storage, no inherited RIWORK_HOME or credential access.
            // Host copies/validates the bytes before this source is removed.
            let dir = std::env::temp_dir().join(format!("riwork-chat-paste-{}", Uuid::new_v4()));
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&dir)
                .map_err(|e| format!("Attachment {name}: {e}"))?;
            let path = dir.join(name);
            let result = (|| {
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&path)
                    .map_err(|e| e.to_string())?;
                file.write_all(image.bytes())
                    .and_then(|()| file.sync_all())
                    .map_err(|e| e.to_string())?;
                client
                    .stage_attachment(chat, &path)
                    .map_err(|e| e.to_string())
            })();
            // Only this freshly allocated source directory, never an attachment or fixture home.
            let _ = fs::remove_dir_all(dir);
            result.map_err(|e| format!("Attachment {name}: {e}"))
        }
    }
}

impl ChatView {
    pub(super) fn attach_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.accepts_input() {
            return;
        }
        let selected = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach UTF-8 text, PNG or JPEG".into()),
        });
        self.attachment_tasks.push(cx.spawn_in(
            window,
            async move |this, cx| match selected.await {
                Ok(Ok(Some(paths))) => {
                    let _ = this.update_in(cx, |view, window, cx| {
                        view.stage_sources(
                            paths.into_iter().map(Source::File).collect(),
                            window,
                            cx,
                        )
                    });
                }
                Ok(Ok(None)) => {}
                other => {
                    let _ = this.update(cx, |view, cx| {
                        view.notice = Some(format!("Attachment picker: {other:?}"));
                        cx.notify();
                    });
                }
            },
        ));
    }
    pub(super) fn paste_attachments(
        &mut self,
        item: &ClipboardItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(sources) = clipboard_sources(item) else {
            return false;
        };
        if !self.accepts_input() {
            return false;
        }
        let mixed = item
            .entries
            .iter()
            .any(|e| matches!(e, ClipboardEntry::String(text) if !text.text().is_empty()));
        self.stage_sources(sources, window, cx);
        if mixed {
            self.notice = Some("Pasted attachments; clipboard text was not inserted.".into());
        }
        true
    }
    pub(super) fn dropped_attachments(
        &mut self,
        paths: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.stage_sources(
            paths.paths().iter().cloned().map(Source::File).collect(),
            window,
            cx,
        );
        cx.stop_propagation();
    }
    pub(super) fn stage_sources(
        &mut self,
        sources: Vec<Source>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.accepts_input() {
            return;
        }
        for source in sources {
            let id = Uuid::new_v4().to_string();
            let name = source.name();
            let ready_or_pending = self
                .attachments
                .iter()
                .filter(|c| !matches!(c.state, Stage::Failed(_)))
                .count();
            let error = (ready_or_pending >= SEND_COUNT).then(|| format!("Attachment {name}: a message can include at most {SEND_COUNT} files; remove one before retrying"));
            self.attachments.push(Chip {
                id: id.clone(),
                name,
                state: error.map_or(Stage::Pending, Stage::Failed),
                open: false,
                source: Some(source.clone()),
            });
            self.bump_generation();
            if self
                .attachments
                .last()
                .is_some_and(|c| matches!(c.state, Stage::Pending))
            {
                self.start_staging(id, source, window, cx);
            }
        }
        self.focus(window, cx);
        cx.notify();
    }
    fn start_staging(
        &mut self,
        id: String,
        source: Source,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(chat) = self.chat_id.clone() else {
            return;
        };
        let ensure = self.config.ensure.clone();
        let work = cx
            .background_executor()
            .spawn(async move { stage_source(&ensure, &chat, &source) });
        self.attachment_tasks
            .push(cx.spawn_in(window, async move |this, cx| {
                let result = work.await;
                let _ = this.update_in(cx, |view, _, cx| {
                    // Removal/newer staging never resurrects a discarded chip.
                    let Some(chip) = view
                        .attachments
                        .iter_mut()
                        .find(|chip| chip.id == id && matches!(chip.state, Stage::Pending))
                    else {
                        return;
                    };
                    chip.state = match result {
                        Ok(attachment) => {
                            chip.source = None;
                            Stage::Ready(attachment)
                        }
                        Err(error) => Stage::Failed(error),
                    };
                    view.bump_generation();
                    cx.notify();
                });
            }));
    }
    fn retry_staging(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .attachments
            .iter()
            .filter(|c| !matches!(c.state, Stage::Failed(_)))
            .count()
            >= SEND_COUNT
        {
            return;
        }
        let Some(chip) = self
            .attachments
            .iter_mut()
            .find(|c| c.id == id && matches!(c.state, Stage::Failed(_)))
        else {
            return;
        };
        let Some(source) = chip.source.clone() else {
            return;
        };
        // Each explicit retry has a new identity, so any old completion is harmless.
        chip.id = Uuid::new_v4().to_string();
        chip.state = Stage::Pending;
        let next = chip.id.clone();
        self.bump_generation();
        self.start_staging(next, source, window, cx);
        cx.notify();
    }
    fn remove_attachment(&mut self, id: &str, cx: &mut Context<Self>) {
        self.attachments.retain(|c| c.id != id);
        self.bump_generation();
        cx.notify();
    }
    pub(super) fn attachment_chips(&self, look: Look, cx: &mut Context<Self>) -> AnyElement {
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(ui_text::space(4.))
            .children(self.attachments.iter().map(|chip| {
                let key = chip.id.clone();
                let toggle = key.clone();
                let remove = key.clone();
                let status = match &chip.state {
                    Stage::Pending => "Staging…".into(),
                    Stage::Ready(a) => format!("{} bytes", a.bytes),
                    Stage::Failed(error) => error.clone(),
                };
                let row = div()
                    .id(format!("attachment-{key}"))
                    .flex()
                    .flex_col()
                    .gap(ui_text::space(3.))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(ui_text::space(6.))
                            .child(
                                button(
                                    format!("attachment-preview-{key}"),
                                    format!("{} {}", if chip.open { "▾" } else { "▸" }, chip.name),
                                    Some(look.colors.cyan),
                                    look,
                                )
                                .on_click(cx.listener(
                                    move |view, _, _, cx| {
                                        if let Some(c) =
                                            view.attachments.iter_mut().find(|c| c.id == toggle)
                                        {
                                            c.open = !c.open;
                                        }
                                        cx.notify();
                                    },
                                )),
                            )
                            .child(
                                div()
                                    .text_size(ui_text::text(10.))
                                    .text_color(rgb(if matches!(chip.state, Stage::Failed(_)) {
                                        look.colors.gold
                                    } else {
                                        look.colors.muted
                                    }))
                                    .child(status),
                            )
                            .children(matches!(chip.state, Stage::Failed(_)).then(|| {
                                button(
                                    format!("attachment-retry-{key}"),
                                    "Retry staging",
                                    None,
                                    look,
                                )
                                .on_click(cx.listener(
                                    move |view, _, window, cx| view.retry_staging(&key, window, cx),
                                ))
                            }))
                            .child(
                                button(format!("attachment-remove-{remove}"), "Remove", None, look)
                                    .on_click(cx.listener(move |view, _, _, cx| {
                                        view.remove_attachment(&remove, cx)
                                    })),
                            ),
                    );
                row.children(chip.open.then(|| {
                    match chip.attachment().map(|a| &a.preview) {
                        Some(Preview::Text { excerpt }) => div()
                            .max_h(ui_text::space(140.))
                            .overflow_hidden()
                            .font_family(ui_text::code_family())
                            .text_size(ui_text::text(10.))
                            .child(excerpt.clone())
                            .into_any_element(),
                        Some(Preview::Image { path }) => div()
                            .child(
                                img(path.clone())
                                    .max_w(ui_text::space(256.))
                                    .max_h(ui_text::space(256.))
                                    .object_fit(gpui::ObjectFit::Contain),
                            )
                            .into_any_element(),
                        None => div()
                            .child("No preview until the file is staged.")
                            .into_any_element(),
                    }
                }))
                .into_any_element()
            }))
            .into_any_element()
    }
    pub(super) fn submission_cards(&self, look: Look, cx: &mut Context<Self>) -> AnyElement {
        div().w_full().flex().flex_col().gap(ui_text::space(4.)).children(self.submissions.iter().map(|saved| {
            let id = saved.id;
            let label = match &saved.status { Status::Pending => "Submitting; draft retained.".into(), Status::Submitted => "Submitted".into(), Status::Refused(e) => format!("Refused: {e}"), Status::Uncertain(e) => format!("Outcome uncertain: {e}. Inspect the transcript before explicitly resending.") };
            let review_key = format!("submission:{id}"); let expanded = self.open.contains(&review_key);
            div().id(format!("chat-submission-{id}")).flex().flex_col().gap(ui_text::space(3.))
                .child(div().text_size(ui_text::text(10.)).text_color(rgb(look.colors.gold)).child(label))
                .child(button(format!("chat-review-{id}"), if expanded {"▾ Saved draft"} else {"▸ Saved draft"}, None, look).on_click(cx.listener(move |view, _, _, cx| view.toggle(&review_key, None, cx))))
                .children(expanded.then(|| div().flex().flex_col().gap(ui_text::space(3.))
                    .child(div().id(format!("chat-saved-text-{id}")).max_h(ui_text::space(160.)).overflow_y_scroll().child(saved.snapshot.text.clone()))
                    .children(saved.snapshot.attachments.iter().map(|a| div().child(format!("{} ({} bytes)",a.name,a.bytes))))
                    .child(button(format!("chat-copy-saved-{id}"), "Copy saved text", None, look).on_click(cx.listener(move |view, _, _, cx| { if let Some(s) = view.submissions.iter().find(|s| s.id == id) { cx.write_to_clipboard(ClipboardItem::new_string(s.snapshot.text.clone())); } })))
                ))
                .children((saved.status != Status::Pending && self.pending_submission.is_none()).then(|| div().flex().flex_wrap().gap(ui_text::space(6.))
                    .child(button(format!("chat-resend-{id}"), "Resend saved draft", Some(look.colors.cyan), look).on_click(cx.listener(move |view, _, window, cx| view.resend_snapshot(id, window, cx))))
                    .child(button(format!("chat-restore-{id}"), "Replace current draft with saved", None, look).on_click(cx.listener(move |view, _, window, cx| view.restore_snapshot(id, window, cx))))
                )).into_any_element()
        })).into_any_element()
    }
}
