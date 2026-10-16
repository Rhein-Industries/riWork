//! What a user message sent with it, in the transcript. The host logs such a message as what
//! the user typed followed by an `[Attached: name (N bytes)]` line per file
//! (`attachments::summary`), and keeps the snapshots themselves under its `presentation`. The
//! message shows the files first, each image as its thumbnail and any other file as a card
//! naming it, as the composer showed them before it was sent, then what the user typed. What
//! is logged and what the provider received are unchanged; a message logged before the
//! snapshots were kept shows as it always did.
use super::{
    Attachment, AttachmentKind, CARD_SIDE, ChatView, FILE_CARD_MAX, NAME_SIZE, Preview,
    card_radius, file_tile, quick_look, size_text, split_name,
};
use crate::{
    behavior_controls as behavior,
    chat::{
        attachments,
        model::{ChatImage, ImageSource, Item, ItemBody, NoticeLevel},
    },
    chat_view::{ImageScope, notices::LocalKey, widgets::Look},
    ui_text,
};
use gpui::{AnyElement, Context, div, prelude::*, rgb};
use gpui_kit::base::TestSupportExt as _;
use std::path::{Path, PathBuf};

/// A thumbnail's side in the transcript, in design points: the host's preview is at most 256
/// pixels on a side, so at 2x it is drawn at its own size.
const THUMB_SIDE: f32 = 128.;

/// What a sent image's card says when its preview cannot be read.
const UNAVAILABLE: &str = "Image unavailable";

/// The text a user message shows: what the user typed, without the lines the host logged for
/// the files it sent, or the whole text when it is not what the host logs. Nothing when the
/// message sent files alone.
pub(in crate::chat_view) fn shown_text<'a>(item: &Item, text: &'a str) -> Option<&'a str> {
    let sent = &item.presentation.attachments;
    if sent.is_empty() {
        return Some(text);
    }
    let typed = attachments::typed(text, sent).unwrap_or(text);
    (!typed.trim().is_empty()).then_some(typed)
}

/// Whether `image` is one of the files a user message shows as its own card, and so needs
/// no image card below it.
pub(in crate::chat_view) fn covered(item: &Item, image: &ChatImage) -> bool {
    matches!(item.body, ItemBody::UserMessage { .. })
        && matches!(&image.source, ImageSource::Local { path }
            if item.presentation.attachments.iter().any(|a| Path::new(path) == a.path))
}

fn sent_key(item: &Item, a: &Attachment) -> String {
    format!("{}:sent:{}", item.id, a.id)
}

/// The folder of a snapshot this chat holds, when `a` names its `content` there, exactly as
/// the host stages it (`attachments::validate_owned`); never another place a log names.
fn own_snapshot(scope: &ImageScope, a: &Attachment) -> Option<PathBuf> {
    let folder = scope.snapshot(&a.id)?;
    (a.path == folder.join("content")).then_some(folder)
}

/// The host's thumbnail of an image this chat holds, beside its snapshot.
fn preview(scope: &ImageScope, a: &Attachment) -> Option<String> {
    let (AttachmentKind::Image { .. }, Preview::Image { path }) = (&a.kind, &a.preview) else {
        return None;
    };
    let folder = own_snapshot(scope, a)?;
    (*path == folder.join("preview.png")).then(|| path.display().to_string())
}

/// What a sent file's card shows.
enum Face<'a> {
    Thumbnail,
    /// An image whose thumbnail is still being read.
    Reading,
    /// A card naming the file, with this status below its name.
    File(&'a str),
}

impl ChatView {
    /// The files a user message sent, as a wrapping row of cards; nothing when it sent none.
    pub(in crate::chat_view) fn sent_attachments(
        &self,
        item: &Item,
        look: Look,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !matches!(item.body, ItemBody::UserMessage { .. })
            || item.presentation.attachments.is_empty()
        {
            return None;
        }
        Some(
            div()
                .w_full()
                .max_w(ui_text::space(400.))
                .flex()
                .flex_wrap()
                .gap(ui_text::space(8.))
                .children(
                    item.presentation
                        .attachments
                        .iter()
                        .map(|a| self.sent_card(item, a, look, cx)),
                )
                .into_any_element(),
        )
    }

    /// One file a message sent: an image as its thumbnail in a square that has its size
    /// before the thumbnail is read, any other file (and an image whose thumbnail cannot be
    /// read) as a card with its type, name and size. Its name and size are its tooltip; a
    /// click shows the snapshot in Quick Look.
    fn sent_card(
        &self,
        item: &Item,
        a: &Attachment,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = look.colors;
        let key = sent_key(item, a);
        let radius = ui_text::space(card_radius(look));
        let size = size_text(a.bytes);
        let preview = preview(&self.image_scope, a);
        let thumbnail = preview.as_ref().map(|path| ChatImage {
            label: a.name.clone(),
            source: ImageSource::Local { path: path.clone() },
        });
        let face = match a.kind {
            AttachmentKind::Text => Face::File(&size),
            AttachmentKind::Image { .. } => match preview {
                Some(path) => match self.thumbnail(&key, &path) {
                    None => Face::Reading,
                    Some(Ok(_)) => Face::Thumbnail,
                    Some(Err(_)) => Face::File(UNAVAILABLE),
                },
                None => Face::File(UNAVAILABLE),
            },
        };
        let thumbnail_element = thumbnail
            .as_ref()
            .map(|image| self.render_thumbnail(&key, image, gpui::ObjectFit::Contain, look, cx));
        let attachment = a.clone();
        let card = |content: AnyElement| {
            behavior::button_content(
                format!("sent-attachment:{key}"),
                format!("Preview {}", a.name),
                content,
            )
            .relative()
            .flex_none()
            .max_w_full()
            .rounded(radius)
            .overflow_hidden()
            .border_1()
            .border_color(rgb(colors.divider))
            .bg(rgb(colors.panel))
            .cursor_pointer()
            .hover(move |style| style.border_color(rgb(colors.muted)))
            .focus_visible(move |style| style.border_color(rgb(colors.focus)))
            .child(crate::tooltip::anchor(
                format!("{} · {size}", a.name),
                crate::tooltip::Look::Control,
            ))
            .on_click(cx.listener(move |view, _, _, cx| view.look_at_sent(&attachment, cx)))
        };
        match face {
            Face::Thumbnail => card(
                div()
                    .id(format!("sent-thumbnail:{key}"))
                    .role(gpui::Role::Image)
                    .aria_label(format!("Sent image: {}", a.name))
                    .size_full()
                    .child(thumbnail_element.expect("owned preview"))
                    .test_support()
                    .into_any_element(),
            )
            .size(ui_text::space(THUMB_SIDE)),
            // The square the thumbnail will fill.
            Face::Reading => {
                card(thumbnail_element.expect("owned preview")).size(ui_text::space(THUMB_SIDE))
            }
            Face::File(status) => card(file_card(&key, &a.name, status, look))
                .h(ui_text::space(CARD_SIDE))
                .w(ui_text::space(FILE_CARD_MAX)),
        }
        .into_any_element()
    }

    /// Show a sent file's snapshot in Quick Look, as the composer shows a staged one: only a
    /// regular file in this chat's own snapshot folder, through no link. Say so when it is
    /// gone.
    fn look_at_sent(&mut self, a: &Attachment, cx: &mut Context<Self>) {
        let real = |path: &Path, folder: bool| {
            std::fs::symlink_metadata(path).is_ok_and(|meta| {
                if folder {
                    meta.is_dir()
                } else {
                    meta.is_file()
                }
            })
        };
        let held = own_snapshot(&self.image_scope, a).is_some_and(|folder| {
            folder.parent().is_some_and(|root| real(root, true))
                && real(&folder, true)
                && real(&a.path, false)
        });
        if !(held && quick_look(a)) {
            self.notices.set(
                LocalKey::Media,
                NoticeLevel::Info,
                format!("{} is no longer available to preview.", a.name),
            );
            cx.notify();
        }
    }
}

/// A file card's face: its type's tile, its name with the extension always whole, and a
/// short status below in the secondary text style.
fn file_card(key: &str, name: &str, status: &str, look: Look) -> AnyElement {
    let colors = look.colors;
    let (stem, ext) = split_name(name);
    div()
        .id(format!("sent-file:{key}"))
        .size_full()
        .flex()
        .items_center()
        .gap(ui_text::space(8.))
        .px(ui_text::space(8.))
        .child(file_tile(name, look))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(ui_text::space(2.))
                .child(
                    div()
                        .flex()
                        .min_w_0()
                        .font_family(ui_text::ui_family())
                        .text_size(ui_text::text(NAME_SIZE))
                        .text_color(rgb(colors.text))
                        .whitespace_nowrap()
                        .child(div().min_w_0().truncate().child(stem))
                        .children(ext.map(|ext| div().flex_none().child(ext))),
                )
                .child(
                    div()
                        .id(format!("sent-status:{key}"))
                        .role(gpui::Role::Label)
                        .aria_label(status.to_owned())
                        .text_size(ui_text::text(9.5))
                        .text_color(rgb(colors.muted))
                        .truncate()
                        .child(status.to_owned())
                        .test_support(),
                ),
        )
        .test_support()
        .into_any_element()
}
