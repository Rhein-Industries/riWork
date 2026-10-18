//! User-triggered, bounded image loading. Folding uses stable message/image keys. The
//! thumbnails of images the user sent are read as their messages are drawn, a few at a time,
//! from the chat's own snapshot folder only (`want_thumbnails`).
use super::{
    ChatView, ChatViewEvent,
    image_scope::ImageScope,
    widgets::{Look, button},
};
use crate::{
    chat::model::{ChatImage, ImageSource, Item, ItemBody, MessagePhase},
    file_preview::{self, PreviewContent},
    ui_text,
};
use base64::Engine;
use gpui::{
    AnyElement, Context, MouseButton, Pixels, Point, RenderImage, ScrollHandle, SharedString, div,
    img, point, prelude::*, px, rgb,
};
use gpui_kit::base::TestSupportExt as _;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::Read,
    sync::Arc,
    time::Duration,
};

const CONCURRENT: usize = 2;
const LOADED: usize = 8;
const OPEN: usize = 16;
const OPEN_LIMIT: &str = "Fold an image before opening more.";
const NETWORK_BYTES: u64 = 8 * 1024 * 1024;
/// Thumbnails of sent images read at once; the rest are asked for as those come back.
const THUMB_READS: usize = 4;
pub(super) const ARTIFACT_PAGE: usize = 32;

pub(super) fn artifact_range(count: usize, page: usize) -> std::ops::Range<usize> {
    let start = page.min(count.saturating_sub(1) / ARTIFACT_PAGE) * ARTIFACT_PAGE;
    start..count.min(start.saturating_add(ARTIFACT_PAGE))
}

fn image_key_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let mut a = a.split(['/', ':']);
    let mut b = b.split(['/', ':']);
    loop {
        let order = match (a.next(), b.next()) {
            (Some(a), Some(b)) => match (a.parse::<usize>(), b.parse::<usize>()) {
                (Ok(a), Ok(b)) => a.cmp(&b),
                _ => a.cmp(b),
            },
            (None, None) => return std::cmp::Ordering::Equal,
            (None, _) => return std::cmp::Ordering::Less,
            (_, None) => return std::cmp::Ordering::Greater,
        };
        if order != std::cmp::Ordering::Equal {
            return order;
        }
    }
}

fn fitted_scale(width: f32, height: f32, viewport: gpui::Size<Pixels>) -> f32 {
    (viewport.width.as_f32().max(1.0) / width.max(1.0))
        .min(viewport.height.as_f32().max(1.0) / height.max(1.0))
        .min(1.0)
}

fn readable_label(label: &str) -> Option<String> {
    let label = label.trim();
    if label.is_empty()
        || label.eq_ignore_ascii_case("image")
        || label.to_ascii_lowercase().contains("data:")
        || (label.len() >= 64
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"+/=_-".contains(&b)))
    {
        return None;
    }
    Some(
        label
            .chars()
            .filter(|c| !c.is_control())
            .take(160)
            .collect(),
    )
}

fn filename(path: &str) -> Option<String> {
    let (path, _, _) = crate::terminal_links::split_position(path);
    let name = path.rsplit('/').next()?;
    let decoded = super::links::percent_decode(name).unwrap_or_else(|_| name.to_owned());
    readable_label(&decoded)
}

/// Pure renderer fallback: old logs and resident hosts may still label every image "Image".
fn image_label(image: &ChatImage, context: Option<&Item>) -> String {
    if let Some(label) = readable_label(&image.label) {
        // URL labels can contain credentials/query tokens; display only their source name.
        if !label.starts_with("https://") && !label.starts_with("http://") {
            return label;
        }
    }
    match &image.source {
        ImageSource::Local { path } => {
            if let Some(name) = filename(path) {
                return name;
            }
        }
        ImageSource::Url { url } => {
            if let Some(rest) = url
                .strip_prefix("https://")
                .or_else(|| url.strip_prefix("http://"))
            {
                let clean = rest.split(['?', '#']).next().unwrap_or(rest);
                if let Some((_, path)) = clean.split_once('/')
                    && let Some(name) = filename(path)
                {
                    return name;
                }
                if let Some(host) = readable_label(
                    clean
                        .split('/')
                        .next()
                        .unwrap_or_default()
                        .rsplit('@')
                        .next()
                        .unwrap_or_default(),
                ) {
                    return format!("Image from {host}");
                }
            }
        }
        _ => {}
    }
    let origin = match context.map(|item| &item.body) {
        Some(ItemBody::ToolCall { tool, input, .. }) => {
            for field in [
                "path",
                "file_path",
                "filename",
                "savedPath",
                "output_file",
                "screenshot_path",
                "image_path",
            ] {
                if let Some(name) = input[field].as_str().and_then(filename) {
                    return name;
                }
            }
            readable_label(tool.rsplit("__").next().unwrap_or(tool))
                .map(|tool| format!("{} image", tool.replace('_', " ")))
                .unwrap_or_else(|| "Tool image".into())
        }
        Some(ItemBody::UserMessage { .. }) => "Uploaded image".into(),
        Some(ItemBody::AgentMessage { .. })
            if context.is_some_and(|item| item.presentation.phase == Some(MessagePhase::Final)) =>
        {
            "Final answer image".into()
        }
        Some(ItemBody::AgentMessage { .. }) => "Assistant image".into(),
        Some(ItemBody::Command { .. }) => "Command image".into(),
        _ => "Attached image".into(),
    };
    let format = match &image.source {
        ImageSource::Data { mime, .. } => match mime.as_str() {
            "image/png" => Some("PNG"),
            "image/jpeg" => Some("JPEG"),
            "image/webp" => Some("WebP"),
            "image/gif" => Some("GIF"),
            _ => None,
        },
        _ => None,
    };
    format.map_or(origin.clone(), |format| format!("{origin} · {format}"))
}

fn card_label(
    image: &ChatImage,
    context: Option<&Item>,
    ordinal: Option<(usize, usize)>,
) -> String {
    let label = image_label(image, context);
    ordinal.map_or(label.clone(), |(index, count)| {
        format!("{} of {count} · {label}", index + 1)
    })
}
#[derive(Default)]
pub(super) struct MediaState {
    pub artifact_pages: HashMap<String, usize>,
    slots: HashMap<String, Slot>,
    queue: VecDeque<(String, ChatImage)>,
    active: usize,
    generation: u64,
    pub viewer: Option<String>,
    /// None means fit; numeric zoom is relative to the image's natural pixel dimensions.
    viewer_zoom: Option<f32>,
    viewer_scroll: ScrollHandle,
    viewer_drag: Option<(Point<Pixels>, Point<Pixels>)>,
    /// The thumbnails of the files users sent, by the keys `attachment_ui::thumbnails` gives.
    thumbs: HashMap<String, Thumb>,
}

/// A sent attachment's thumbnail: the host's bounded preview of it, read as soon as its
/// message is drawn rather than on a click, as only a small picture the user sent is.
struct Thumb {
    generation: u64,
    path: String,
    /// Nothing while it is read; then the picture, or why there is none.
    content: Option<Result<Arc<RenderImage>, String>>,
}

impl MediaState {
    fn show(&mut self, key: String) {
        self.viewer = Some(key);
        self.viewer_zoom = None;
        self.viewer_drag = None;
        self.viewer_scroll.set_offset(Point::default());
    }

    /// Only decoded images from this turn participate. Navigation never starts a read or
    /// evicts another card, and order follows transcript items then numeric image keys.
    fn gallery(&self, items: &[Item], current: &str) -> Vec<String> {
        let owner = |key: &str| {
            items.iter().position(|item| {
                key.starts_with(&format!("{}:image:", item.id))
                    || key.starts_with(&format!("{}/", item.id))
                    || key.starts_with(&format!("user:{}/", item.id))
            })
        };
        let Some(current_item) = owner(current) else {
            return vec![current.to_owned()];
        };
        let turn = items[current_item].turn_id.as_deref();
        let mut keys: Vec<_> = self
            .slots
            .iter()
            .filter_map(|(key, slot)| {
                let index = owner(key)?;
                let same_turn = turn.map_or(index == current_item, |turn| {
                    items[index].turn_id.as_deref() == Some(turn)
                });
                (same_turn && matches!(slot.content, Some(Ok(PreviewContent::Image { .. }))))
                    .then(|| (index, key.clone()))
            })
            .collect();
        keys.sort_by(|(left, a), (right, b)| left.cmp(right).then_with(|| image_key_cmp(a, b)));
        keys.into_iter().map(|(_, key)| key).collect()
    }

    /// Explicitly collapsing a disclosure folds its images on every page.
    /// Removing slots invalidates in-flight generations; active workers still
    /// drain normally and release their results without reviving hidden cards.
    fn fold_images(&mut self, keys: &[String], open: &mut HashSet<String>) -> Vec<Slot> {
        let hidden: HashSet<&str> = keys.iter().map(String::as_str).collect();
        self.queue.retain(|(key, _)| !hidden.contains(key.as_str()));
        if self
            .viewer
            .as_deref()
            .is_some_and(|key| hidden.contains(key))
        {
            self.viewer = None;
        }
        keys.iter()
            .filter_map(|key| {
                open.remove(key);
                self.slots.remove(key)
            })
            .collect()
    }

    /// Return stale decoded content to the caller for resource release.
    fn complete(
        &mut self,
        key: &str,
        generation: u64,
        source: &ImageSource,
        content: Result<PreviewContent, String>,
        in_files: bool,
    ) -> Option<PreviewContent> {
        if let Some(slot) = self
            .slots
            .get_mut(key)
            .filter(|slot| slot.generation == generation && &slot.source == source)
        {
            slot.content = Some(content);
            slot.in_files = in_files;
            None
        } else {
            content.ok()
        }
    }

    /// Forget every local image read under a scope that has changed: their slots go (to be
    /// released by the caller), a load still running for one finds no slot of its generation
    /// and is dropped on arrival, and each one still open is queued to be read again under the
    /// new scope. Returns the forgotten slots.
    fn rescope(&mut self, open: &HashSet<String>) -> Vec<Slot> {
        let stale: Vec<String> = self
            .slots
            .iter()
            .filter(|(_, slot)| matches!(slot.source, ImageSource::Local { .. }))
            .map(|(key, _)| key.clone())
            .collect();
        let mut released = Vec::new();
        for key in stale {
            let Some(slot) = self.slots.remove(&key) else {
                continue;
            };
            if self.viewer.as_ref() == Some(&key) {
                self.viewer = None;
            }
            if open.contains(&key) && !self.queue.iter().any(|(queued, _)| *queued == key) {
                self.queue.push_back((
                    key,
                    ChatImage {
                        label: String::new(),
                        source: slot.source.clone(),
                    },
                ));
            }
            released.push(slot);
        }
        released
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn decoded(width: u32, height: u32) -> PreviewContent {
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(width, height)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        file_preview::decode_image(png.get_ref()).unwrap()
    }

    fn gallery_item(id: &str, turn: &str) -> Item {
        Item {
            id: id.into(),
            turn_id: Some(turn.into()),
            status: crate::chat::model::ItemStatus::Completed,
            presentation: crate::chat::model::Presentation {
                phase: Some(MessagePhase::Final),
                ..Default::default()
            },
            body: ItemBody::AgentMessage {
                text: "Image results".into(),
            },
        }
    }

    fn loaded_slot() -> Slot {
        Slot {
            generation: 1,
            source: ImageSource::Url {
                url: "https://example.invalid/test.png".into(),
            },
            content: Some(Ok(decoded(4, 3))),
            in_files: false,
        }
    }

    #[test]
    fn gallery_orders_loaded_images_numerically_and_stays_in_its_turn() {
        let items = vec![
            gallery_item("first", "turn"),
            gallery_item("second", "turn"),
            gallery_item("other", "elsewhere"),
        ];
        let mut media = MediaState::default();
        for key in [
            "first:image:10",
            "first:image:2",
            "second:image:0",
            "other:image:0",
        ] {
            media.slots.insert(key.into(), loaded_slot());
        }
        media.slots.insert(
            "first:image:1".into(),
            Slot {
                content: None,
                ..loaded_slot()
            },
        );
        assert_eq!(
            media.gallery(&items, "first:image:10"),
            ["first:image:2", "first:image:10", "second:image:0"]
        );
        media.viewer_zoom = Some(2.0);
        media.viewer_scroll.set_offset(point(px(-20.0), px(-30.0)));
        media.show("second:image:0".into());
        assert_eq!(media.viewer_zoom, None);
        assert_eq!(media.viewer_scroll.offset(), Point::default());
    }

    #[test]
    fn fit_preserves_aspect_and_does_not_upscale_small_images() {
        assert_eq!(
            fitted_scale(1200.0, 600.0, gpui::size(px(600.0), px(600.0))),
            0.5
        );
        assert_eq!(
            fitted_scale(100.0, 200.0, gpui::size(px(600.0), px(600.0))),
            1.0
        );
    }

    #[gpui::test]
    fn image_controls_zoom_pan_and_navigate_without_loading(cx: &mut gpui::TestAppContext) {
        use gpui_kit::test::TestWindowExt;
        let (handle, view, recording) = super::super::editor_tests::mount_selection(cx);
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |view, cx| {
                view.model.link = super::super::Link::Live;
                view.model.transcript.items = vec![
                    gallery_item("first", "turn"),
                    gallery_item("second", "turn"),
                ];
                for key in ["first:image:0", "second:image:0"] {
                    view.media.slots.insert(
                        key.into(),
                        Slot {
                            content: Some(Ok(decoded(1200, 900))),
                            ..loaded_slot()
                        },
                    );
                }
                view.media.show("first:image:0".into());
                view.refresh_projection(cx);
                cx.notify();
            });
            window.render_frame(cx);
            window.click("actual-size-chat-image", cx);
            window.render_frame(cx);
            assert_eq!(view.read(cx).media.viewer_zoom, Some(1.0));
            let bounds = window.find("chat-image-viewport").bounds();
            let center = bounds.center();
            window.drag(center, point(center.x - px(100.0), center.y - px(80.0)), cx);
            assert!(view.read(cx).media.viewer_scroll.offset().x < px(0.0));
            window.click("zoom-in-chat-image", cx);
            window.render_frame(cx);
            assert_eq!(view.read(cx).media.viewer_zoom, Some(1.5));
            window.click("next-chat-image", cx);
            window.render_frame(cx);
            assert_eq!(
                view.read(cx).media.viewer.as_deref(),
                Some("second:image:0")
            );
            assert_eq!(view.read(cx).media.viewer_zoom, None);
            assert!(view.read(cx).media.queue.is_empty());
        })
        .unwrap();
        assert!(recording.try_recv().is_err());
    }

    #[test]
    fn collapsing_group_frees_hidden_expansions_and_rejects_late_results_after_reopening() {
        let source = ImageSource::Local {
            path: "shot.png".into(),
        };
        let image = ChatImage {
            label: "Image".into(),
            source: source.clone(),
        };
        let keys: Vec<String> = (0..15).map(|n| format!("tool-{n}:image:0")).collect();
        let other = "user:image:0".to_owned();
        let mut open: HashSet<String> = keys.iter().cloned().chain([other.clone()]).collect();
        let mut media = MediaState {
            active: 2,
            generation: 10,
            viewer: Some(keys[0].clone()),
            ..Default::default()
        };
        media.slots.insert(
            keys[0].clone(),
            Slot {
                generation: 8,
                source: source.clone(),
                content: Some(Ok(PreviewContent::Message("Loaded".into()))),
                in_files: false,
            },
        );
        media.slots.insert(
            keys[1].clone(),
            Slot {
                generation: 9,
                source: source.clone(),
                content: None,
                in_files: false,
            },
        );
        media.slots.insert(
            other.clone(),
            Slot {
                generation: 10,
                source: source.clone(),
                content: None,
                in_files: false,
            },
        );
        media.queue.extend(
            keys[2..]
                .iter()
                .cloned()
                .chain([other.clone()])
                .map(|key| (key, image.clone())),
        );
        assert_eq!(open.len(), 16);
        let released = media.fold_images(&keys, &mut open);
        assert_eq!(released.len(), 2);
        assert!(released.iter().any(|slot| slot.content.is_some()));
        assert_eq!(open, HashSet::from([other.clone()]));
        assert_eq!(media.slots.len(), 1);
        assert!(media.slots.contains_key(&other));
        assert_eq!(media.queue.len(), 1);
        assert_eq!(media.queue[0].0, other);
        assert!(media.viewer.is_none());
        assert_eq!(media.active, 2); // running workers keep the concurrency bound until they drain
        assert!(
            media
                .complete(
                    &keys[1],
                    9,
                    &source,
                    Ok(PreviewContent::Message("Late".into())),
                    false
                )
                .is_some()
        );
        assert!(!media.slots.contains_key(&keys[1]));

        open.insert(keys[1].clone());
        media.generation += 1;
        media.slots.insert(
            keys[1].clone(),
            Slot {
                generation: media.generation,
                source: source.clone(),
                content: None,
                in_files: false,
            },
        );
        assert!(
            media
                .complete(
                    &keys[1],
                    9,
                    &source,
                    Ok(PreviewContent::Message("Old generation".into())),
                    false
                )
                .is_some()
        );
        assert!(media.slots[&keys[1]].content.is_none());
        assert!(
            media
                .complete(
                    &keys[1],
                    11,
                    &source,
                    Ok(PreviewContent::Message("Reopened".into())),
                    false
                )
                .is_none()
        );
        assert!(
            matches!(&media.slots[&keys[1]].content, Some(Ok(PreviewContent::Message(text))) if text == "Reopened")
        );
        assert_eq!(open.len(), 2);
    }

    #[test]
    fn artifact_pages_bound_rendering_and_keep_all_images_reachable() {
        assert_eq!(artifact_range(9, 0), 0..9);
        assert_eq!(artifact_range(0, 100), 0..0);
        let pages = (0..313)
            .flat_map(|page| artifact_range(10_000, page))
            .collect::<Vec<_>>();
        assert_eq!(pages, (0..10_000).collect::<Vec<_>>());
        assert_eq!(artifact_range(33, 99), 32..33);
        assert_eq!(artifact_range(10_000, 20), 640..672);
    }

    #[test]
    fn downloads_stop_at_the_byte_limit_and_invalid_inline_images_fail() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/image", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = [0; 4096];
            let _ = stream.read(&mut request);
            let length = NETWORK_BYTES + 1024;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            let block = [0; 1024];
            for _ in 0..length / 1024 {
                if stream.write_all(&block).is_err() {
                    break;
                }
            }
        });
        let scope = ImageScope::default();
        let remote = ChatImage {
            label: "Image".into(),
            source: ImageSource::Url { url },
        };
        assert!(load(&remote, &scope).0.err().unwrap().contains("8 MiB"));
        server.join().unwrap();
        let invalid = ChatImage {
            label: "Image".into(),
            source: ImageSource::Data {
                mime: "image/png".into(),
                base64: "aGVsbG8=".into(),
            },
        };
        assert!(load(&invalid, &scope).0.is_err());
    }
}
struct Slot {
    generation: u64,
    source: ImageSource,
    content: Option<Result<PreviewContent, String>>,
    /// Open in Files preview reaches this local image: it is in the chat's own Files root.
    in_files: bool,
}

/// Read an image a card shows, and whether Open in Files preview reaches it. A local file is
/// read only when `scope` allows it, and only as an image: the scope reaches further than
/// Files does, so it shows nothing else. A name with another extension is refused unread; a
/// name without one (an attachment snapshot is `content`) is decoded by what it holds.
pub(super) fn load(
    image: &ChatImage,
    scope: &ImageScope,
) -> (Result<PreviewContent, String>, bool) {
    match &image.source {
        ImageSource::Local { path } => {
            let content = scope.resolve(path).and_then(|resolved| {
                if resolved.path.extension().is_some()
                    && !file_preview::is_image_path(&resolved.path)
                {
                    return Err("This reference is not a supported image.".into());
                }
                match file_preview::load_image(&resolved.root, &resolved.path)? {
                    image @ PreviewContent::Image { .. } | image @ PreviewContent::Message(_) => {
                        Ok(image)
                    }
                    _ => Err("This reference is not a supported image.".into()),
                }
            });
            let in_files = content.is_ok() && scope.in_files(path);
            (content, in_files)
        }
        _ => (load_remote(image), false),
    }
}

fn load_remote(image: &ChatImage) -> Result<PreviewContent, String> {
    match &image.source {
        ImageSource::Local { .. } => Err("This reference is not a supported image.".into()),
        ImageSource::Data { base64, .. } => {
            if base64.len() > crate::chat::media::IMAGE_BYTES {
                return Err("Image exceeds the encoded size limit.".into());
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(base64)
                .map_err(|_| "Invalid image data")?;
            file_preview::decode_image(&bytes)
        }
        ImageSource::Url { url } => {
            if !(url.starts_with("https://") || url.starts_with("http://")) {
                return Err("Unsupported image URL".into());
            }
            let agent = ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(10)))
                .build()
                .new_agent();
            let mut response = agent
                .get(url)
                .call()
                .map_err(|error| format!("Cannot load image: {error}"))?;
            let mut bytes = Vec::new();
            response
                .body_mut()
                .as_reader()
                .take(NETWORK_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|error| format!("Cannot read image: {error}"))?;
            if bytes.len() as u64 > NETWORK_BYTES {
                return Err("Image exceeds the 8 MiB download limit".into());
            }
            file_preview::decode_image(&bytes)
        }
        ImageSource::Unavailable { reason } => Err(reason.clone()),
    }
}
fn release_thumb(thumb: Thumb, cx: &mut gpui::App) {
    if let Some(Ok(image)) = thumb.content {
        cx.defer(move |cx| cx.drop_image(image, None));
    }
}
fn release(slot: Slot, cx: &mut Context<ChatView>) {
    if let Some(Ok(content)) = slot.content
        && let Some(image) = content.render_image()
    {
        let image = image.clone();
        cx.defer(move |cx| cx.drop_image(image, None));
    }
}
impl ChatView {
    pub(super) fn fold_group_images(&mut self, keys: &[String], cx: &mut Context<Self>) {
        for slot in self.media.fold_images(keys, &mut self.open) {
            release(slot, cx);
        }
        if self.open.iter().filter(|id| id.contains("image")).count() < OPEN {
            self.notices.clear(super::notices::LocalKey::Media);
        }
    }

    /// A new image scope: what the old one let a card read is forgotten and read again.
    pub(super) fn rescope_images(&mut self, cx: &mut Context<Self>) {
        for slot in self.media.rescope(&self.open) {
            release(slot, cx);
        }
        // Thumbnails are read again under the new scope when their messages are drawn.
        for (_, thumb) in self.media.thumbs.drain() {
            release_thumb(thumb, cx);
        }
        self.start_images(cx);
        self.list.remeasure();
        cx.notify();
    }

    /// What a card's slot holds, for tests: `Ok(in_files)` once an image or a message was
    /// read, the error once it failed, nothing while it loads or when nothing was asked.
    #[cfg(test)]
    pub(super) fn image_slot(&self, key: &str) -> Option<Result<bool, String>> {
        let slot = self.media.slots.get(key)?;
        match slot.content.as_ref()? {
            Ok(PreviewContent::Image { .. }) => Some(Ok(slot.in_files)),
            Ok(other) => Some(Err(format!(
                "not an image: {}",
                match other {
                    PreviewContent::Message(message) => message.as_str(),
                    _ => "other content",
                }
            ))),
            Err(error) => Some(Err(error.clone())),
        }
    }

    pub(super) fn release_images(&mut self, cx: &mut gpui::App) {
        for (_, slot) in self.media.slots.drain() {
            if let Some(Ok(content)) = slot.content
                && let Some(image) = content.render_image()
            {
                let image = image.clone();
                cx.defer(move |cx| cx.drop_image(image, None));
            }
        }
        for (_, thumb) in self.media.thumbs.drain() {
            release_thumb(thumb, cx);
        }
    }

    /// Start reading the thumbnails of what the user message at `at` sent that are not read
    /// or being read: each as an image card reads a local image, within the chat's scope, a
    /// few at a time. One that could not be read is not read again until the scope changes.
    pub(super) fn want_thumbnails(&mut self, at: usize, cx: &mut Context<Self>) {
        let Some(item) = self.model.transcript.items.get(at) else {
            return;
        };
        let reading = self
            .media
            .thumbs
            .values()
            .filter(|thumb| thumb.content.is_none())
            .count();
        let wanted: Vec<(String, String)> =
            super::attachment_ui::thumbnails(item, &self.image_scope)
                .filter(|(key, path)| {
                    !self
                        .media
                        .thumbs
                        .get(key)
                        .is_some_and(|thumb| thumb.path == *path)
                })
                .take(THUMB_READS.saturating_sub(reading))
                .collect();
        for (key, path) in wanted {
            self.media.generation += 1;
            let generation = self.media.generation;
            let image = ChatImage {
                label: String::new(),
                source: ImageSource::Local { path: path.clone() },
            };
            let scope = self.image_scope.clone();
            let work = cx
                .background_executor()
                .spawn(async move { load(&image, &scope).0 });
            if let Some(old) = self.media.thumbs.insert(
                key.clone(),
                Thumb {
                    generation,
                    path,
                    content: None,
                },
            ) {
                release_thumb(old, cx);
            }
            cx.spawn(async move |this, cx| {
                let content = match work.await {
                    Ok(PreviewContent::Image { image, .. }) => Ok(image),
                    Ok(PreviewContent::Message(message)) | Err(message) => Err(message),
                    // `load` reads nothing else; a picture never drawn holds no atlas tiles.
                    Ok(_) => Err("This reference is not a supported image.".into()),
                };
                let _ = this.update(cx, |view, cx| {
                    match view
                        .media
                        .thumbs
                        .get_mut(&key)
                        .filter(|thumb| thumb.generation == generation)
                    {
                        Some(thumb) => {
                            thumb.content = Some(content);
                            cx.notify();
                        }
                        // Forgotten while it was read: its scope changed or the view let it go.
                        None => {
                            if let Ok(image) = content {
                                cx.defer(move |cx| cx.drop_image(image, None));
                            }
                        }
                    }
                });
            })
            .detach();
        }
    }

    /// For tests: how many thumbnails are being read, and when each one was last asked for.
    #[cfg(test)]
    pub(super) fn thumbnail_reads(&self) -> (usize, HashMap<String, u64>) {
        (
            self.media
                .thumbs
                .values()
                .filter(|thumb| thumb.content.is_none())
                .count(),
            self.media
                .thumbs
                .iter()
                .map(|(key, thumb)| (key.clone(), thumb.generation))
                .collect(),
        )
    }

    /// The thumbnail read for `key` from `path`: nothing while it is read or before.
    pub(super) fn thumbnail(
        &self,
        key: &str,
        path: &str,
    ) -> Option<&Result<Arc<RenderImage>, String>> {
        self.media
            .thumbs
            .get(key)
            .filter(|thumb| thumb.path == path)?
            .content
            .as_ref()
    }

    pub(super) fn image_card(
        &self,
        image: &ChatImage,
        key: &str,
        context: Option<&Item>,
        ordinal: Option<(usize, usize)>,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.open.contains(key);
        let label = card_label(image, context, ordinal);
        let owned = image.clone();
        let toggle_key = key.to_owned();
        let content = self
            .media
            .slots
            .get(key)
            .filter(|slot| slot.source == image.source)
            .and_then(|slot| slot.content.as_ref());
        let body = if open {
            Some(match content {
                Some(Ok(PreviewContent::Image { image, description })) => {
                    let viewer_key = key.to_owned();
                    div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap(ui_text::space(4.0))
                        .child(
                            crate::behavior_controls::button_content(
                                SharedString::from(format!("view:{key}")),
                                format!("View {description}"),
                                img(image.clone())
                                    .absolute()
                                    .inset_0()
                                    .size_full()
                                    .aspect_square(),
                            )
                            .w_full()
                            .h(ui_text::space(240.0))
                            .flex_none()
                            .relative()
                            .overflow_hidden()
                            .cursor_pointer()
                            .focus_visible(move |style| {
                                style.border_1().border_color(rgb(look.colors.focus))
                            })
                            .on_click(cx.listener(
                                move |view, _, _, cx| {
                                    view.media.show(viewer_key.clone());
                                    cx.notify();
                                },
                            )),
                        )
                        .child(
                            div()
                                .text_size(ui_text::text(10.0))
                                .child(format!("{description} · Click to view")),
                        )
                        .into_any_element()
                }
                Some(Ok(PreviewContent::Message(message))) | Some(Err(message)) => div()
                    .text_size(ui_text::text(11.0))
                    .child(message.clone())
                    .into_any_element(),
                _ => div()
                    .text_size(ui_text::text(11.0))
                    .child("Loading image…")
                    .into_any_element(),
            })
        } else {
            None
        };
        // Open in Files preview shows the file in the chat's own worktree; an image of another
        // worktree or project is viewed here only, rather than offered an action that fails.
        let local = match &image.source {
            ImageSource::Local { path }
                if self
                    .media
                    .slots
                    .get(key)
                    .is_some_and(|slot| slot.source == image.source && slot.in_files) =>
            {
                Some(path.clone())
            }
            _ => None,
        };
        div()
            .w_full()
            .border_1()
            .border_color(rgb(look.colors.divider))
            .rounded(ui_text::space(4.0))
            .p(ui_text::space(6.0))
            .flex()
            .flex_col()
            .gap(ui_text::space(6.0))
            .child(
                button(
                    SharedString::from(format!("fold:{key}")),
                    format!("{} {label}", if open { "▾" } else { "▸" }),
                    None,
                    look,
                )
                .aria_expanded(open)
                .on_click(cx.listener(move |view, _, _, cx| {
                    view.toggle_image(toggle_key.clone(), owned.clone(), cx)
                })),
            )
            .children(body)
            .children(local.filter(|_| open).map(|path| {
                button(
                    SharedString::from(format!("preview:{key}")),
                    "Open in Files preview",
                    None,
                    look,
                )
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.emit(ChatViewEvent::OpenFile {
                        target: path.clone(),
                    })
                }))
            }))
            .into_any_element()
    }
    pub(super) fn toggle_image(&mut self, key: String, image: ChatImage, cx: &mut Context<Self>) {
        let offset = self.list.logical_scroll_top();
        self.list.pause_following_tail();
        if !self.open.remove(&key) {
            if self.open.iter().filter(|id| id.contains("image")).count() >= OPEN {
                self.notices.set(
                    super::notices::LocalKey::Media,
                    crate::chat::model::NoticeLevel::Info,
                    OPEN_LIMIT,
                );
            } else {
                self.open.insert(key.clone());
                self.media.queue.push_back((key, image));
                self.start_images(cx);
            }
        } else if let Some(slot) = self.media.slots.remove(&key) {
            release(slot, cx);
        }
        self.list.remeasure();
        self.list.scroll_to(offset);
        cx.notify();
    }
    fn start_images(&mut self, cx: &mut Context<Self>) {
        while self.media.active < CONCURRENT {
            let Some((key, image)) = self.media.queue.pop_front() else {
                break;
            };
            if !self.open.contains(&key) {
                continue;
            }
            if self
                .media
                .slots
                .get(&key)
                .is_some_and(|slot| slot.source == image.source)
            {
                continue;
            }
            if self.media.slots.len() >= LOADED {
                // Only retain a bounded decoded cache; evicted expanded cards can be folded/reopened.
                let oldest = self
                    .media
                    .slots
                    .keys()
                    .find(|id| self.media.viewer.as_ref() != Some(*id))
                    .cloned();
                if let Some(oldest) = oldest {
                    self.open.remove(&oldest);
                    if let Some(slot) = self.media.slots.remove(&oldest) {
                        release(slot, cx);
                    }
                }
            }
            // Local references resolve against the chat's own working directory, within the
            // folders its identity allows (`ImageScope`), never the selected UI worktree.
            let scope = self.image_scope.clone();
            let owned = image.clone();
            let work = cx
                .background_executor()
                .spawn(async move { load(&owned, &scope) });
            self.media.active += 1;
            self.media.generation += 1;
            let generation = self.media.generation;
            let completion_key = key.clone();
            let source = image.source.clone();
            cx.spawn(async move |this, cx| {
                let (content, in_files) = work.await;
                let _ = this.update(cx, |view, cx| {
                    view.media.active = view.media.active.saturating_sub(1);
                    if let Some(content) =
                        view.media
                            .complete(&completion_key, generation, &source, content, in_files)
                        && let Some(image) = content.render_image()
                    {
                        let image = image.clone();
                        cx.defer(move |cx| cx.drop_image(image, None));
                    }
                    view.start_images(cx);
                    view.list.remeasure();
                    cx.notify();
                });
            })
            .detach();
            self.media.slots.insert(
                key,
                Slot {
                    generation,
                    source: image.source,
                    content: None,
                    in_files: false,
                },
            );
        }
    }
    fn set_image_zoom(&mut self, zoom: Option<f32>, cx: &mut Context<Self>) {
        self.media.viewer_zoom = zoom.map(|value| value.clamp(0.01, 8.0));
        self.media.viewer_drag = None;
        self.media.viewer_scroll.set_offset(Point::default());
        cx.notify();
    }

    fn navigate_image(&mut self, direction: isize, cx: &mut Context<Self>) {
        let Some(current) = self.media.viewer.as_deref() else {
            return;
        };
        let gallery = self.media.gallery(&self.model.transcript.items, current);
        let Some(at) = gallery.iter().position(|key| key == current) else {
            return;
        };
        if let Some(next) = at
            .checked_add_signed(direction)
            .and_then(|next| gallery.get(next))
        {
            self.media.show(next.clone());
            cx.notify();
        }
    }

    pub(super) fn image_viewer(&self, look: Look, cx: &mut Context<Self>) -> Option<AnyElement> {
        let key = self.media.viewer.as_ref()?;
        let slot = self.media.slots.get(key)?;
        let Some(Ok(PreviewContent::Image { image, description })) = &slot.content else {
            return None;
        };
        let size = image.size(0);
        let (width, height) = (size.width.0 as f32, size.height.0 as f32);
        let gallery = self.media.gallery(&self.model.transcript.items, key);
        let at = gallery
            .iter()
            .position(|candidate| candidate == key)
            .unwrap_or(0);
        let zoom = self.media.viewer_zoom;
        let handle = self.media.viewer_scroll.clone();
        let zoom_in = cx.listener(move |view: &mut Self, _, _, cx| {
            let current = view.media.viewer_zoom.unwrap_or_else(|| {
                fitted_scale(width, height, view.media.viewer_scroll.bounds().size)
            });
            view.set_image_zoom(Some(current * 1.5), cx);
        });
        let zoom_out = cx.listener(move |view: &mut Self, _, _, cx| {
            let current = view.media.viewer_zoom.unwrap_or_else(|| {
                fitted_scale(width, height, view.media.viewer_scroll.bounds().size)
            });
            view.set_image_zoom(Some(current / 1.5), cx);
        });
        let content = match zoom {
            None => div()
                .absolute()
                .inset_0()
                .child(
                    img(Arc::clone(image))
                        .size_full()
                        .object_fit(gpui::ObjectFit::ScaleDown),
                )
                .into_any_element(),
            Some(zoom) => div()
                .w(px(width * zoom))
                .h(px(height * zoom))
                .min_w_full()
                .min_h_full()
                .flex()
                .items_center()
                .justify_center()
                .flex_none()
                .child(
                    img(Arc::clone(image))
                        .flex_none()
                        .w(px(width * zoom))
                        .h(px(height * zoom)),
                )
                .into_any_element(),
        };
        Some(
            div()
                .id("chat-image-viewer")
                .absolute()
                .inset_0()
                .occlude()
                .bg(rgb(look.colors.panel))
                .p(ui_text::space(16.0))
                .flex()
                .flex_col()
                .gap(ui_text::space(8.0))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(ui_text::space(6.0))
                        .child(
                            button("previous-chat-image", "Previous", None, look)
                                .disabled(at == 0)
                                .on_click(
                                    cx.listener(|view, _, _, cx| view.navigate_image(-1, cx)),
                                ),
                        )
                        .child(div().text_size(ui_text::text(11.0)).child(format!(
                            "{} / {}",
                            at + 1,
                            gallery.len().max(1)
                        )))
                        .child(
                            button("next-chat-image", "Next", None, look)
                                .disabled(at + 1 >= gallery.len())
                                .on_click(cx.listener(|view, _, _, cx| view.navigate_image(1, cx))),
                        )
                        .child(div().flex_1())
                        .child(button("zoom-out-chat-image", "−", None, look).on_click(zoom_out))
                        .child(div().text_size(ui_text::text(11.0)).child(
                            zoom.map_or_else(
                                || "Fit".into(),
                                |zoom| format!("{:.0}%", zoom * 100.0),
                            ),
                        ))
                        .child(button("zoom-in-chat-image", "+", None, look).on_click(zoom_in))
                        .child(
                            button("fit-chat-image", "Fit", None, look).on_click(
                                cx.listener(|view, _, _, cx| view.set_image_zoom(None, cx)),
                            ),
                        )
                        .child(
                            button("actual-size-chat-image", "100%", None, look).on_click(
                                cx.listener(|view, _, _, cx| view.set_image_zoom(Some(1.0), cx)),
                            ),
                        )
                        .child(button("close-chat-image", "Close", None, look).on_click(
                            cx.listener(|view, _, _, cx| {
                                view.media.viewer = None;
                                view.media.viewer_drag = None;
                                cx.notify();
                            }),
                        )),
                )
                .child(
                    div()
                        .id("chat-image-viewport")
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .relative()
                        .overflow_scroll()
                        .track_scroll(&handle)
                        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|view, event: &gpui::MouseDownEvent, _, cx| {
                                if view.media.viewer_zoom.is_some() {
                                    view.media.viewer_drag =
                                        Some((event.position, view.media.viewer_scroll.offset()));
                                    cx.stop_propagation();
                                }
                            }),
                        )
                        .on_mouse_move(cx.listener(|view, event: &gpui::MouseMoveEvent, _, cx| {
                            if event.pressed_button != Some(MouseButton::Left) {
                                view.media.viewer_drag = None;
                                return;
                            }
                            if let Some((start, offset)) = view.media.viewer_drag {
                                let next = offset + event.position - start;
                                let max = view.media.viewer_scroll.max_offset();
                                view.media.viewer_scroll.set_offset(point(
                                    next.x.clamp(-max.x, px(0.0)),
                                    next.y.clamp(-max.y, px(0.0)),
                                ));
                                cx.stop_propagation();
                                cx.notify();
                            }
                        }))
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|view, _, _, _| view.media.viewer_drag = None),
                        )
                        .on_mouse_up_out(
                            MouseButton::Left,
                            cx.listener(|view, _, _, _| view.media.viewer_drag = None),
                        )
                        .child(content)
                        .test_support(),
                )
                .child(
                    div()
                        .text_size(ui_text::text(10.0))
                        .text_color(rgb(look.colors.muted))
                        .child(format!(
                            "{description} · Scroll or drag to pan · {} loaded images in this turn",
                            gallery.len().max(1)
                        )),
                )
                .into_any_element(),
        )
    }
}
