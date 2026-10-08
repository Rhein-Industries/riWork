//! Picker, native paste, and external drop share host staging. Source paths never become inputs.
use super::{
    ChatView,
    attachment_draft::Status,
    widgets::{self, Look, button},
};
use crate::{
    behavior_controls as behavior,
    chat::{
        attachments::{
            Attachment, AttachmentKind, FILE_BYTES, Preview, SEND_COUNT, image_thumbnail,
        },
        client::Client,
    },
    icons, ui_text,
};
use gpui::{
    AnyElement, ClipboardEntry, ClipboardItem, Context, ExternalPaths, Image, ImageFormat,
    PathPromptOptions, SharedString, Window, div, img, prelude::*, rgb,
};
use gpui_kit::base::TestSupportExt as _;
use std::{
    fs,
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::PathBuf,
    sync::Arc,
};
use uuid::Uuid;

#[cfg(test)]
mod tests;

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
    local_preview: Option<Arc<Image>>,
}
impl Chip {
    fn release_local_preview(&mut self, cx: &mut gpui::App) {
        if let Some(image) = self.local_preview.take() {
            // Retire after the old frame; also release GPUI's decoded asset cache.
            cx.defer(move |cx| gpui::ImageSource::Image(image).remove_asset(cx));
        }
    }
    pub fn ready(attachment: Attachment) -> Self {
        Self {
            id: attachment.id.clone(),
            name: attachment.name.clone(),
            state: Stage::Ready(attachment),
            open: false,
            source: None,
            local_preview: None,
        }
    }
    pub fn attachment(&self) -> Option<&Attachment> {
        if let Stage::Ready(a) = &self.state {
            Some(a)
        } else {
            None
        }
    }
    fn image_preview(&self) -> Option<&PathBuf> {
        match &self.attachment()?.preview {
            // Only the validated, bounded host thumbnail, never Source or content.
            Preview::Image { path } => Some(path),
            Preview::Text { .. } => None,
        }
    }
    fn preview_source(&self) -> Option<gpui::ImageSource> {
        if let Stage::Ready(_) = self.state {
            return self.image_preview().cloned().map(Into::into);
        }
        self.local_preview.clone().map(Into::into)
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

/// A regular file of at most `FILE_BYTES`, for its card's thumbnail. Nonblocking, so a
/// FIFO in its place cannot hang the preview task.
fn read_preview_bytes(path: &std::path::Path) -> Option<Vec<u8>> {
    use std::io::Read as _;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > FILE_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(FILE_BYTES + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= FILE_BYTES).then_some(bytes)
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
                        view.notices.set(
                            super::notices::LocalKey::Attachment,
                            crate::chat::model::NoticeLevel::Error,
                            format!("Attachment picker: {other:?}"),
                        );
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
            self.notices.set(
                super::notices::LocalKey::Attachment,
                crate::chat::model::NoticeLevel::Info,
                "Pasted attachments; clipboard text was not inserted.",
            );
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
        // A picker that failed before has worked now.
        self.notices.clear(super::notices::LocalKey::Attachment);
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
                local_preview: None,
            });
            self.bump_generation();
            self.start_local_preview(id.clone(), &source, window, cx);
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
    fn start_local_preview(
        &mut self,
        id: String,
        source: &Source,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A pasted image's bytes, or a dropped/picked PNG or JPEG file read for the card
        // only: the host still stages from the path itself.
        enum Local {
            Image(Arc<Image>),
            File(PathBuf),
        }
        let local = match source {
            Source::Image { image, .. } => Local::Image(image.clone()),
            Source::File(path)
                if matches!(
                    path.file_name()
                        .and_then(|n| extension(&n.to_string_lossy()))
                        .as_deref(),
                    Some("png" | "jpg" | "jpeg")
                ) =>
            {
                Local::File(path.clone())
            }
            Source::File(_) => return,
        };
        // Independent task: never waits for Ensure, a socket or host staging.
        let work = cx.background_executor().spawn(async move {
            let bytes = match local {
                Local::Image(image) => {
                    if !matches!(image.format(), ImageFormat::Png | ImageFormat::Jpeg) {
                        return None;
                    }
                    image.bytes().to_vec()
                }
                Local::File(path) => read_preview_bytes(&path)?,
            };
            image_thumbnail(&bytes).ok().map(|bytes| {
                let mut preview = Image::from_bytes(ImageFormat::Png, bytes);
                // Own this asset independently of identical images in other
                // chips/windows, so retirement cannot evict their cache.
                preview.id = Uuid::new_v4().as_u128() as u64;
                Arc::new(preview)
            })
        });
        self.attachment_tasks
            .push(cx.spawn_in(window, async move |this, cx| {
                let preview = work.await;
                let _ = this.update(cx, |view, cx| view.local_preview_finished(&id, preview, cx));
            }));
    }
    fn local_preview_finished(
        &mut self,
        id: &str,
        preview: Option<Arc<Image>>,
        cx: &mut Context<Self>,
    ) {
        let Some(chip) = self
            .attachments
            .iter_mut()
            .find(|chip| chip.id == id && !matches!(chip.state, Stage::Ready(_)))
        else {
            return;
        };
        chip.release_local_preview(cx);
        chip.local_preview = preview;
        // Presentation only: never mark Ready or change the editor generation.
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
                    view.staging_finished(&id, result, cx);
                });
            }));
    }
    fn staging_finished(
        &mut self,
        id: &str,
        result: Result<Attachment, String>,
        cx: &mut Context<Self>,
    ) {
        // Removal/newer attempt UUIDs never resurrect or overwrite a chip.
        let Some(chip) = self
            .attachments
            .iter_mut()
            .find(|chip| chip.id == id && matches!(chip.state, Stage::Pending))
        else {
            return;
        };
        chip.state = match result {
            Ok(attachment) => {
                chip.source = None;
                // Quick Look shows it from here on, not the inline preview.
                chip.open = false;
                chip.release_local_preview(cx);
                Stage::Ready(attachment)
            }
            Err(error) => Stage::Failed(error),
        };
        self.bump_generation();
        cx.notify();
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
        let needs_preview = chip.local_preview.is_none();
        self.bump_generation();
        if needs_preview {
            self.start_local_preview(next.clone(), &source, window, cx);
        }
        self.start_staging(next, source, window, cx);
        cx.notify();
    }
    fn remove_attachment(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(chip) = self.attachments.iter_mut().find(|chip| chip.id == id) {
            chip.release_local_preview(cx);
        }
        self.attachments.retain(|c| c.id != id);
        self.bump_generation();
        cx.notify();
    }
    pub(super) fn release_attachment_previews(&mut self, cx: &mut gpui::App) {
        for chip in &mut self.attachments {
            chip.release_local_preview(cx);
        }
    }
    /// A card's click: Quick Look on the staged copy once the host has it; until then (or
    /// if that copy is gone) the inline preview below the cards, as for a pasted image
    /// still staging.
    fn open_attachment(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(chip) = self.attachments.iter_mut().find(|c| c.id == id) else {
            return;
        };
        if chip.attachment().is_some_and(quick_look) {
            chip.open = false;
        } else if chip.attachment().is_some() || chip.local_preview.is_some() {
            chip.open = !chip.open;
        }
        cx.notify();
    }
    /// The draft's attachments as a wrapping row of cards above the message box, then the
    /// inline previews of the ones opened without Quick Look.
    pub(super) fn attachment_chips(&self, look: Look, cx: &mut Context<Self>) -> AnyElement {
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(ui_text::space(6.))
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_wrap()
                    .gap(ui_text::space(8.))
                    .children(
                        self.attachments
                            .iter()
                            .map(|chip| attachment_card(chip, look, cx)),
                    ),
            )
            .children(
                self.attachments
                    .iter()
                    .filter(|chip| chip.open)
                    .map(|chip| expanded_preview(chip, look)),
            )
            .into_any_element()
    }
    pub(super) fn submission_cards(&self, look: Look, cx: &mut Context<Self>) -> AnyElement {
        div().w_full().flex().flex_col().gap(ui_text::space(4.)).children(self.submissions.iter().map(|saved| {
            let id = saved.id;
            let label = match &saved.status { Status::Pending => "Submitting; draft retained.".into(), Status::Submitted => "Submitted".into(), Status::Refused(e) => format!("Refused: {e}"), Status::Uncertain(e) => format!("Outcome uncertain: {e}. Inspect the transcript before explicitly resending.") };
            let review_key = format!("submission:{id}"); let expanded = self.open.contains(&review_key);
            div().id(format!("chat-submission-{id}")).flex().flex_col().gap(ui_text::space(3.))
                .child(div().text_size(ui_text::text(10.)).text_color(rgb(look.colors.gold)).child(label))
                .child(button(format!("chat-review-{id}"), if expanded {"▾ Saved draft"} else {"▸ Saved draft"}, None, look).aria_expanded(expanded).on_click(cx.listener(move |view, _, _, cx| view.toggle(&review_key, None, cx))))
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

/// A square image card's side, and a file card's height.
const CARD_SIDE: f32 = 64.;
/// A file card's narrowest and widest.
const FILE_CARD_MIN: f32 = 132.;
const FILE_CARD_MAX: f32 = 200.;
/// The most characters of a file name a file card shows before it cuts out the middle.
const NAME_CHARS: usize = 22;

fn card_radius(look: Look) -> f32 {
    if look.native { 10. } else { 4. }
}

/// One attachment: an image as its thumbnail filling a square card, any other file as a
/// card with its type, name and size; a × in the corner on hover or keyboard focus.
fn attachment_card(chip: &Chip, look: Look, cx: &mut Context<ChatView>) -> AnyElement {
    let colors = look.colors;
    let key = chip.id.clone();
    let failed = matches!(chip.state, Stage::Failed(_));
    let error = look.error();
    let radius = ui_text::space(card_radius(look));
    let side = ui_text::space(CARD_SIDE);
    let (status, tooltip) = match &chip.state {
        Stage::Pending => ("Staging…".to_owned(), format!("{} · staging…", chip.name)),
        Stage::Ready(a) => {
            let size = size_text(a.bytes);
            (size.clone(), format!("{} · {size}", chip.name))
        }
        Stage::Failed(e) => (e.clone(), e.clone()),
    };
    let previewable = chip.attachment().is_some() || chip.local_preview.is_some();
    let label = if failed {
        format!("Retry staging {}", chip.name)
    } else {
        format!("Preview {}", chip.name)
    };
    let thumbnail = chip.preview_source();
    let image = thumbnail.is_some();
    let content = match thumbnail {
        Some(source) => div()
            .relative()
            .size_full()
            .child(
                div()
                    .id(format!("attachment-thumbnail-{key}"))
                    .role(gpui::Role::Image)
                    .aria_label(format!("Image preview: {}", chip.name))
                    .size_full()
                    .overflow_hidden()
                    .child(
                        img(source)
                            .size_full()
                            .rounded(radius)
                            .object_fit(gpui::ObjectFit::Cover),
                    )
                    .test_support(),
            )
            .children(
                matches!(chip.state, Stage::Pending | Stage::Failed(_)).then(|| {
                    // A scrim over the thumbnail while it stages, tinted when it failed.
                    let scrim = if failed {
                        (look.tint(error, 0.5) << 8) | 0x99
                    } else {
                        (colors.panel << 8) | 0x8c
                    };
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(gpui::rgba(scrim))
                        .child(
                            status_label(&key, &status, status_mark(&chip.state, &key, look))
                                .text_size(ui_text::text(14.))
                                .text_color(rgb(colors.text))
                                .test_support(),
                        )
                }),
            )
            .into_any_element(),
        None => {
            let visible = match &chip.state {
                Stage::Pending => div()
                    .flex()
                    .items_center()
                    .gap(ui_text::space(4.))
                    .child(widgets::pulse(
                        format!("attachment-pulse-{key}"),
                        colors.muted,
                    ))
                    .child(status.clone())
                    .into_any_element(),
                Stage::Failed(_) => "Failed · retry".into_any_element(),
                Stage::Ready(_) => status.clone().into_any_element(),
            };
            div()
                .size_full()
                .flex()
                .items_center()
                .gap(ui_text::space(8.))
                .pl(ui_text::space(8.))
                // Room for the × in the corner, so it never covers the name's end.
                .pr(ui_text::space(26.))
                .child(file_tile(&chip.name, look))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .gap(ui_text::space(2.))
                        .child(
                            div()
                                .text_size(ui_text::text(10.5))
                                .text_color(rgb(colors.text))
                                .truncate()
                                .child(middle_truncate(&chip.name, NAME_CHARS)),
                        )
                        .child(
                            status_label(&key, &status, visible)
                                .text_size(ui_text::text(9.5))
                                .text_color(rgb(if failed { error } else { colors.muted }))
                                .truncate()
                                .test_support(),
                        ),
                )
                .into_any_element()
        }
    };
    let action = key.clone();
    let card = behavior::button_content(format!("attachment-card-{key}"), label, content)
        .when(!failed && chip.attachment().is_none(), |card| {
            card.aria_expanded(chip.open)
        })
        .disabled(!failed && !previewable)
        .relative()
        .flex_none()
        .h(side)
        .map(|card| {
            if image {
                card.w(side)
            } else {
                card.min_w(ui_text::space(FILE_CARD_MIN))
                    .max_w(ui_text::space(FILE_CARD_MAX))
            }
        })
        .rounded(radius)
        .overflow_hidden()
        .border_1()
        .border_color(rgb(if failed { error } else { colors.divider }))
        .bg(rgb(if failed {
            look.tint(error, 0.16)
        } else {
            colors.panel_active
        }))
        .cursor_pointer()
        .hover(move |style| style.border_color(rgb(if failed { error } else { colors.muted })))
        .focus_visible(move |style| style.border_color(rgb(colors.focus)))
        .child(crate::tooltip::anchor(
            tooltip,
            crate::tooltip::Look::Control,
        ))
        .on_click(cx.listener(move |view, _, window, cx| {
            if view
                .attachments
                .iter()
                .any(|c| c.id == action && matches!(c.state, Stage::Failed(_)))
            {
                view.retry_staging(&action, window, cx)
            } else {
                view.open_attachment(&action, cx)
            }
        }));
    let remove = key.clone();
    let group = SharedString::from(format!("attachment-group-{key}"));
    let badge = behavior::button_content(
        format!("attachment-remove-{key}"),
        format!("Remove {}", chip.name),
        icons::text_mark("×", 10.),
    )
    .flex_none()
    .size(ui_text::space(18.))
    .flex()
    .items_center()
    .justify_center()
    .rounded_full()
    .border_1()
    .border_color(rgb(colors.divider))
    .bg(rgb(colors.panel))
    .text_size(ui_text::text(11.))
    .text_color(rgb(colors.text))
    .cursor_pointer()
    .hover(move |style| style.bg(rgb(colors.panel_active)))
    .focus_visible(move |style| style.border_color(rgb(colors.focus)))
    .on_click(cx.listener(move |view, _, _, cx| view.remove_attachment(&remove, cx)));
    div()
        .id(format!("attachment-{key}"))
        .group(group.clone())
        .relative()
        .flex_none()
        .child(card)
        .child(
            div()
                .absolute()
                .top(ui_text::space(4.))
                .right(ui_text::space(4.))
                .child(behavior::tab_close_boundary(
                    format!("attachment-remove-boundary-{key}"),
                    behavior::tab_close_reveal(badge, group),
                )),
        )
        .into_any_element()
}

/// What a thumbnail's scrim shows: a pulse while it stages, a "!" when it failed.
fn status_mark(state: &Stage, key: &str, look: Look) -> AnyElement {
    match state {
        Stage::Failed(_) => "!".into_any_element(),
        _ => widgets::pulse(format!("attachment-pulse-{key}"), look.colors.text),
    }
}

/// The card's state for assistive technology in full ("Staging…", the size, the error),
/// whatever short form it shows.
fn status_label(key: &str, status: &str, visible: AnyElement) -> gpui::Stateful<gpui::Div> {
    div()
        .id(format!("attachment-status-{key}"))
        .role(gpui::Role::Label)
        .aria_label(status.to_owned())
        .child(visible)
}

/// The file's type on a small tile: its SF Symbol under Native, its extension elsewhere.
fn file_tile(name: &str, look: Look) -> AnyElement {
    let colors = look.colors;
    let kind = FileKind::of(name);
    let ink = match kind {
        FileKind::Image => colors.cyan,
        FileKind::Pdf => colors.magenta,
        FileKind::Code => colors.cyan,
        FileKind::Archive => colors.gold,
        FileKind::Document | FileKind::Other => colors.muted,
    };
    let tile = div()
        .flex_none()
        .size(ui_text::space(32.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(ui_text::space(if look.native { 7. } else { 3. }))
        .bg(rgb(look.tint(ink, 0.18)));
    if look.native {
        tile.child(icons::symbol(kind.symbol(), 14., Some(ink)))
            .into_any_element()
    } else {
        tile.border_1()
            .border_color(rgb(ink))
            .text_size(ui_text::text(8.))
            .text_color(rgb(ink))
            .child(extension_label(name))
            .into_any_element()
    }
}

/// The inline preview of an opened card that Quick Look cannot show.
fn expanded_preview(chip: &Chip, look: Look) -> AnyElement {
    let key = &chip.id;
    match (chip.preview_source(), chip.attachment().map(|a| &a.preview)) {
        (None, Some(Preview::Text { excerpt })) => div()
            .id(format!("attachment-expanded-{key}"))
            .max_h(ui_text::space(140.))
            .overflow_hidden()
            .font_family(ui_text::code_family())
            .text_size(ui_text::text(10.))
            .child(excerpt.clone())
            .test_support()
            .into_any_element(),
        (Some(source), _) => div()
            .id(format!("attachment-expanded-{key}"))
            .role(gpui::Role::Image)
            .aria_label(format!("Expanded image preview: {}", chip.name))
            .max_w(ui_text::space(256.))
            .max_h(ui_text::space(256.))
            .overflow_hidden()
            .rounded(ui_text::space(card_radius(look)))
            .child(
                img(source)
                    .max_w(ui_text::space(256.))
                    .max_h(ui_text::space(256.))
                    .object_fit(gpui::ObjectFit::Contain),
            )
            .test_support()
            .into_any_element(),
        _ => div()
            .text_color(rgb(look.colors.muted))
            .child("No preview until the file is staged.")
            .into_any_element(),
    }
}

#[cfg(test)]
thread_local! {
    /// The staged copies and content types Quick Look was asked for; tests spawn nothing.
    pub(super) static QUICK_LOOKS: std::cell::RefCell<Vec<(PathBuf, &'static str)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// The Uniform Type Identifier Quick Look should read a staged copy as: the host stores it
/// as `content`, without the name's extension.
pub(super) fn content_type(kind: &AttachmentKind) -> &'static str {
    match kind {
        AttachmentKind::Image { mime } if mime == "image/jpeg" => "public.jpeg",
        AttachmentKind::Image { .. } => "public.png",
        AttachmentKind::Text => "public.plain-text",
    }
}

/// Show the host's staged copy in a Quick Look panel (`qlmanage -p`), from a private
/// temporary copy under the attachment's own name so the panel titles it so; the copy goes
/// when the panel closes. False when there is no staged copy to show.
fn quick_look(attachment: &Attachment) -> bool {
    if !attachment.path.is_file() {
        return false;
    }
    let content_type = content_type(&attachment.kind);
    #[cfg(test)]
    {
        QUICK_LOOKS.with(|q| q.borrow_mut().push((attachment.path.clone(), content_type)));
        true
    }
    #[cfg(not(test))]
    {
        use std::process::{Command, Stdio};
        let source = attachment.path.clone();
        let name = quick_look_name(&attachment.name);
        std::thread::Builder::new()
            .name("attachment-quick-look".into())
            .spawn(move || {
                let dir =
                    std::env::temp_dir().join(format!("riwork-quick-look-{}", Uuid::new_v4()));
                let copy = fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&dir)
                    .ok()
                    .map(|()| dir.join(name))
                    .filter(|copy| fs::copy(&source, copy).is_ok());
                if let Ok(mut child) = Command::new("/usr/bin/qlmanage")
                    .arg("-p")
                    .arg("-c")
                    .arg(content_type)
                    .arg(copy.as_ref().unwrap_or(&source))
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                {
                    let _ = child.wait();
                }
                // Only this freshly allocated directory, never the staged copy.
                let _ = fs::remove_dir_all(&dir);
            })
            .is_ok()
    }
}

/// A file name safe to create in the Quick Look directory: the name's last component.
pub(super) fn quick_look_name(name: &str) -> String {
    std::path::Path::new(name)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "attachment".into())
}

/// What a file card shows a file as, by its name's extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FileKind {
    Image,
    Pdf,
    Code,
    Archive,
    Document,
    Other,
}
impl FileKind {
    pub(super) fn of(name: &str) -> Self {
        let Some(ext) = extension(name) else {
            return Self::Other;
        };
        match ext.as_str() {
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "heic" | "tiff" | "bmp" | "svg" => {
                Self::Image
            }
            "pdf" => Self::Pdf,
            "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "7z" | "rar" | "zst" => Self::Archive,
            "txt" | "md" | "markdown" | "rtf" | "doc" | "docx" | "pages" | "csv" | "tsv"
            | "log" | "odt" => Self::Document,
            "rs" | "py" | "js" | "mjs" | "ts" | "tsx" | "jsx" | "swift" | "go" | "c" | "h"
            | "cc" | "cpp" | "hpp" | "m" | "mm" | "java" | "kt" | "rb" | "php" | "sh" | "zsh"
            | "bash" | "fish" | "json" | "toml" | "yaml" | "yml" | "xml" | "html" | "css"
            | "scss" | "sql" | "lua" | "zig" | "nix" => Self::Code,
            _ => Self::Other,
        }
    }
    /// Its SF Symbol, for Native.
    pub(super) fn symbol(self) -> &'static str {
        match self {
            Self::Image => "photo",
            Self::Pdf => "doc.richtext",
            Self::Code => "chevron.left.forwardslash.chevron.right",
            Self::Archive => "doc.zipper",
            Self::Document => "doc.text",
            Self::Other => "doc",
        }
    }
}

/// The name's extension, lowercase: a short alphanumeric run after the last dot of a name
/// that has something before it.
pub(super) fn extension(name: &str) -> Option<String> {
    let (stem, ext) = name.rsplit_once('.')?;
    (!stem.is_empty()
        && !ext.is_empty()
        && ext.chars().count() <= 8
        && ext.chars().all(|c| c.is_ascii_alphanumeric()))
    .then(|| ext.to_ascii_lowercase())
}

/// The extension as the colorful themes' file tile shows it: "PNG", at most four letters,
/// or "FILE".
pub(super) fn extension_label(name: &str) -> String {
    extension(name).map_or_else(
        || "FILE".into(),
        |ext| ext.to_ascii_uppercase().chars().take(4).collect(),
    )
}

/// `name` in at most `max_chars` characters, its middle replaced by "…" so the start and
/// the end, extension included, stay readable: "Screensho…22.14.23.png".
pub(super) fn middle_truncate(name: &str, max_chars: usize) -> String {
    let chars: Vec<char> = name.chars().collect();
    if chars.len() <= max_chars {
        return name.to_owned();
    }
    if max_chars == 0 {
        return String::new();
    }
    let keep = max_chars - 1;
    let ext_len = extension(name).map_or(0, |ext| ext.chars().count() + 1);
    // The extension stays whole when it leaves room for a few characters of the stem.
    let ext_len = if ext_len + 4 <= keep { ext_len } else { 0 };
    let stem = &chars[..chars.len() - ext_len];
    let available = keep - ext_len;
    let head = available.div_ceil(2);
    let tail = available - head;
    let mut out: String = stem[..head].iter().collect();
    out.push('…');
    out.extend(&stem[stem.len() - tail..]);
    out.extend(&chars[chars.len() - ext_len..]);
    out
}

/// A size as a file card shows it: "512 B", "36 KB", "4.5 KB", "1.2 MB" (1024-based, one
/// decimal under ten).
pub(super) fn size_text(bytes: u64) -> String {
    const KB: f64 = 1024.;
    fn scaled(value: f64, unit: &str) -> String {
        if value < 9.95 {
            let text = format!("{value:.1}");
            format!("{} {unit}", text.strip_suffix(".0").unwrap_or(&text))
        } else {
            format!("{} {unit}", value.round() as u64)
        }
    }
    let bytes_f = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes_f / KB < 1023.5 {
        scaled(bytes_f / KB, "KB")
    } else if bytes_f / (KB * KB) < 1023.5 {
        scaled(bytes_f / (KB * KB), "MB")
    } else {
        scaled(bytes_f / (KB * KB * KB), "GB")
    }
}
