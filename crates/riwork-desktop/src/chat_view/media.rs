//! User-triggered, bounded image loading. Folding uses stable message/image keys.
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
use gpui_kit::base::{ElementExt as _, TestSupportExt as _};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::Read,
    sync::Arc,
    time::Duration,
};

const CONCURRENT: usize = 4;
const LOADED: usize = 8;
const OPEN: usize = 16;
const THUMBNAILS: usize = 64;
const THUMBNAIL_EDGE: u32 = 360;
const OPEN_LIMIT: &str = "Fold an image before opening more.";
const NETWORK_BYTES: u64 = 8 * 1024 * 1024;
pub(super) const ARTIFACT_PAGE: usize = 32;
mod actions;

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
pub(super) fn image_label(image: &ChatImage, context: Option<&Item>) -> String {
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
    thumbnails: ThumbnailCache,
    expanded: HashSet<String>,
    slots: HashMap<String, Slot>,
    queue: VecDeque<(String, ChatImage)>,
    active: usize,
    generation: u64,
    pub viewer: Option<String>,
    /// None means fit; numeric zoom is relative to the image's natural pixel dimensions.
    viewer_zoom: Option<f32>,
    viewer_scroll: ScrollHandle,
    viewer_drag: Option<(Point<Pixels>, Point<Pixels>)>,
    action_pending: bool,
}

/// Small decoded images have their own demand, queue and LRU. Scrolling through
/// attachments must neither open cards nor consume the manual-image allowance.
#[derive(Default)]
struct ThumbnailCache {
    slots: HashMap<String, Slot>,
    queue: VecDeque<(String, ChatImage)>,
    lru: VecDeque<String>,
}

impl ThumbnailCache {
    fn prepare(&mut self, key: String, image: ChatImage, generation: u64) -> Vec<Slot> {
        let mut released = Vec::new();
        self.lru.retain(|cached| cached != &key);
        self.lru.push_back(key.clone());
        if self
            .slots
            .get(&key)
            .is_some_and(|slot| slot.source == image.source)
        {
            if let Some(index) = self.queue.iter().position(|(queued, _)| queued == &key)
                && let Some(request) = self.queue.remove(index)
            {
                self.queue.push_front(request);
            }
            return released;
        }
        self.queue.retain(|(queued, _)| queued != &key);
        if let Some(stale) = self.slots.remove(&key) {
            released.push(stale);
        }
        self.slots.insert(
            key.clone(),
            Slot {
                generation,
                source: image.source.clone(),
                content: None,
                in_files: false,
            },
        );
        self.queue.push_front((key, image));
        while self.slots.len() > THUMBNAILS {
            let Some(oldest) = self.lru.pop_front() else {
                break;
            };
            self.queue.retain(|(key, _)| key != &oldest);
            if let Some(slot) = self.slots.remove(&oldest) {
                released.push(slot);
            }
        }
        released
    }

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
            if (open.contains(&key) || self.expanded.contains(&key))
                && !self.queue.iter().any(|(queued, _)| *queued == key)
            {
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
        let stale: Vec<_> = self
            .thumbnails
            .slots
            .iter()
            .filter(|(_, slot)| matches!(slot.source, ImageSource::Local { .. }))
            .map(|(key, _)| key.clone())
            .collect();
        for key in stale {
            if let Some(slot) = self.thumbnails.slots.remove(&key) {
                self.thumbnails.lru.retain(|cached| cached != &key);
                self.thumbnails.queue.retain(|(queued, _)| queued != &key);
                released.push(slot);
            }
        }
        released
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::model::{ItemStatus, Presentation};
    use gpui::TestAppContext;
    use gpui_kit::test::TestWindowExt;
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
            started_at: None,
            ended_at: None,
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
                // Projection retires slots no row asks for; these stand for opened cards.
                view.refresh_projection(cx);
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

    fn image_item(n: usize) -> Item {
        Item {
            id: format!("tool-{n}"),
            turn_id: Some("images".into()),
            status: ItemStatus::Completed,
            body: ItemBody::Command {
                command: "cat result.txt".into(),
                cwd: None,
                output: String::new(),
                exit_code: Some(0),
            },
            presentation: Presentation {
                images: vec![ChatImage {
                    label: format!("result-{n}.png"),
                    source: ImageSource::Local {
                        path: format!("result-{n}.png"),
                    },
                }],
                ..Presentation::default()
            },
            started_at: None,
            ended_at: None,
        }
    }

    #[gpui::test]
    fn artifact_pages_bound_rendering_and_keep_all_images_reachable(cx: &mut TestAppContext) {
        // The projected Activity's artifact strip keeps upstream's bounded pages;
        // opening an image also exposes the complete shared preview sequence.
        assert_eq!(artifact_range(9, 0), 0..9);
        assert_eq!(artifact_range(0, 100), 0..0);
        let pages = (0..313)
            .flat_map(|page| artifact_range(10_000, page))
            .collect::<Vec<_>>();
        assert_eq!(pages, (0..10_000).collect::<Vec<_>>());
        assert_eq!(artifact_range(33, 99), 32..33);
        assert_eq!(artifact_range(10_000, 20), 640..672);
        let (handle, view, _feed) = super::super::editor_tests::mount_selection(cx);
        view.update(cx, |view, cx| {
            view.model.transcript.items = (0..10_000).map(image_item).collect();
            view.refresh_projection(cx);
            assert_eq!(view.items_in_list, 1);
            assert_eq!(view.visible[0].artifacts.len(), 10_000);
            cx.notify();

            let mut sequence = view.file_preview_sequence("result-0.png");
            assert_eq!(sequence.entries.len(), 10_000);
            assert!(!sequence.step(-1));
            for n in 0..10_000 {
                let current = sequence.current().unwrap();
                assert_eq!(current.target, format!("result-{n}.png"));
                assert_eq!(
                    current.image,
                    Some(image_item(n).presentation.images.remove(0))
                );
                assert_eq!(sequence.step(1), n < 9999);
            }
            assert_eq!(sequence.index, 9999);
        });
        for page in [0, 1, 312] {
            view.update(cx, |view, cx| {
                view.media
                    .artifact_pages
                    .insert("activity:images".into(), page);
                view.list.remeasure();
                cx.notify();
            });
            cx.update_window(handle.into(), |_, window, cx| {
                window.render_frame(cx);
                let range = artifact_range(10_000, page);
                for n in range.clone() {
                    assert!(
                        window
                            .try_find(format!("chip:artifact:tool-{n}:image:0"))
                            .is_some()
                    );
                }
                if range.start > 0 {
                    assert!(
                        window
                            .try_find(format!("chip:artifact:tool-{}:image:0", range.start - 1))
                            .is_none()
                    );
                }
                assert!(
                    window
                        .try_find(format!("chip:artifact:tool-{}:image:0", range.end))
                        .is_none()
                );
                if page == 0 {
                    assert!(
                        window
                            .try_find("previous:artifacts:activity:images")
                            .is_none()
                    );
                    window.click("next:artifacts:activity:images", cx);
                    assert_eq!(view.read(cx).media.artifact_pages["activity:images"], 1);
                } else {
                    window.click("previous:artifacts:activity:images", cx);
                    assert_eq!(
                        view.read(cx).media.artifact_pages["activity:images"],
                        page - 1
                    );
                }
            })
            .unwrap();
        }
    }

    #[gpui::test]
    fn collapsing_group_frees_hidden_expansions_and_rejects_late_results_after_reopening(
        cx: &mut TestAppContext,
    ) {
        // Projected calls now own automatic image demand independently of manually
        // opened image cards. Collapsing their parent must retire that demand and
        // its resources, while leaving unrelated manual disclosures alone.
        let (_, view, _feed) = super::super::editor_tests::mount_selection(cx);
        view.update(cx, |view, cx| {
            view.model.transcript.items = (0..15).map(image_item).collect();
            view.apply_display_mode(super::super::DisplayMode::Verbose, cx);
            view.expand_transcript(true, cx);
            let source = ImageSource::Local {
                path: "shot.png".into(),
            };
            let image = ChatImage {
                label: "Image".into(),
                source: source.clone(),
            };
            let keys: Vec<String> = (0..15).map(|n| format!("tool-{n}:image:0")).collect();
            let other = "user:image:0".to_owned();
            view.open.insert(other.clone());
            view.media = MediaState {
                expanded: keys.iter().cloned().collect(),
                active: 2,
                generation: 10,
                viewer: Some(keys[0].clone()),
                ..Default::default()
            };
            for (key, generation, content) in [
                (
                    keys[0].clone(),
                    8,
                    Some(Ok(PreviewContent::Message("Loaded".into()))),
                ),
                (keys[1].clone(), 9, None),
                (other.clone(), 10, None),
            ] {
                view.media.slots.insert(
                    key,
                    Slot {
                        generation,
                        source: source.clone(),
                        content,
                        in_files: false,
                    },
                );
            }
            view.media.queue.extend(
                keys[2..]
                    .iter()
                    .cloned()
                    .chain([other.clone()])
                    .map(|key| (key, image.clone())),
            );

            view.toggle_projected("g:images:tool-0", cx);
            assert!(view.media.expanded.is_empty());
            assert_eq!(view.media.slots.len(), 1);
            assert!(view.media.slots.contains_key(&other));
            assert_eq!(view.media.queue.len(), 1);
            assert_eq!(view.media.queue[0].0, other);
            assert!(view.media.viewer.is_none());
            assert_eq!(view.media.active, 2); // In-flight workers still occupy their slots.
            assert!(view.open.contains(&other));
            assert!(
                view.media
                    .complete(
                        &keys[1],
                        9,
                        &source,
                        Ok(PreviewContent::Message("Late".into())),
                        false,
                    )
                    .is_some()
            );
            assert!(!view.media.slots.contains_key(&keys[1]));

            view.toggle_projected("g:images:tool-0", cx);
            view.media.expanded.insert(keys[1].clone());
            view.media.generation += 1;
            view.media.slots.insert(
                keys[1].clone(),
                Slot {
                    generation: view.media.generation,
                    source: source.clone(),
                    content: None,
                    in_files: false,
                },
            );
            assert!(
                view.media
                    .complete(
                        &keys[1],
                        9,
                        &source,
                        Ok(PreviewContent::Message("Old generation".into())),
                        false,
                    )
                    .is_some()
            );
            assert!(view.media.slots[&keys[1]].content.is_none());
            assert!(
                view.media
                    .complete(
                        &keys[1],
                        11,
                        &source,
                        Ok(PreviewContent::Message("Reopened".into())),
                        false,
                    )
                    .is_none()
            );
            assert!(
                matches!(&view.media.slots[&keys[1]].content, Some(Ok(PreviewContent::Message(text))) if text == "Reopened")
            );
            assert!(view.open.contains(&other));
            // This fixture supplies in-flight slots itself, without background reads.
            // Retire it before the test harness draws the reopened calls.
            view.model.transcript.items.clear();
            view.open.clear();
            view.media = MediaState::default();
            view.refresh_projection(cx);
        });
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
pub(crate) fn load(
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

/// Called exclusively by the background loader. Retain at most a thumbnail's
/// pixels in its cache, rather than a full preview bitmap for every file chip.
fn thumbnail_content(content: PreviewContent) -> PreviewContent {
    let PreviewContent::Image { image, description } = content else {
        return content;
    };
    let size = image.size(0);
    let (width, height) = (size.width.0 as u32, size.height.0 as u32);
    if width <= THUMBNAIL_EDGE && height <= THUMBNAIL_EDGE {
        return PreviewContent::Image { image, description };
    }
    let pixels = image.as_bytes(0).unwrap_or_default().to_vec();
    let Some(buffer) = image::RgbaImage::from_raw(width, height, pixels) else {
        return PreviewContent::Message("Cannot prepare this image thumbnail.".into());
    };
    // RenderImage's bytes are BGRA. Resizing preserves their channel order.
    let buffer = image::imageops::thumbnail(&buffer, THUMBNAIL_EDGE, THUMBNAIL_EDGE);
    PreviewContent::Image {
        image: Arc::new(gpui::RenderImage::new([image::Frame::new(buffer)])),
        description,
    }
}

/// Design-point bounds, independent of the UI font scale. Never upscale pixels.
fn fitted_image_size(width: f32, height: f32, available: f32) -> (f32, f32) {
    let scale = 1.0f32
        .min(available.max(0.0) / width.max(1.0))
        .min(320.0 / height.max(1.0));
    (width * scale, height * scale)
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
    /// Already decoded, offline generated content uses the normal bounded gallery
    /// and the same save/copy/open actions as a PNG supplied by the agent.
    pub(super) fn show_generated_diagram(
        &mut self,
        key: String,
        image: Arc<RenderImage>,
        png: Arc<str>,
        cx: &mut Context<Self>,
    ) {
        if !self.media.slots.contains_key(&key) && self.media.slots.len() >= LOADED {
            let oldest = self
                .media
                .slots
                .keys()
                .find(|candidate| self.media.viewer.as_ref() != Some(*candidate))
                .cloned();
            if let Some(oldest) = oldest {
                self.open.remove(&oldest);
                if let Some(slot) = self.media.slots.remove(&oldest) {
                    release(slot, cx);
                }
            }
        }
        self.media.generation += 1;
        let previous = self.media.slots.insert(
            key.clone(),
            Slot {
                generation: self.media.generation,
                source: ImageSource::Data {
                    mime: "image/png".into(),
                    base64: png.to_string(),
                },
                content: Some(Ok(PreviewContent::Image {
                    image,
                    description: "Mermaid diagram".into(),
                })),
                in_files: false,
            },
        );
        if let Some(previous) = previous {
            release(previous, cx);
        }
        self.media.show(key);
        cx.notify();
    }

    /// A new image scope: what the old one let a card read is forgotten and read again.
    pub(super) fn rescope_images(&mut self, cx: &mut Context<Self>) {
        for slot in self.media.rescope(&self.open) {
            release(slot, cx);
        }
        self.start_images(cx);
        self.list.remeasure();
        cx.notify();
    }

    /// `demand` contains `<item-id>:image:<index>` for opened projected rows.
    /// Manual/global image cards keep their own disclosure keys in `open`.
    pub(super) fn retain_opened_images(
        &mut self,
        demand: &HashSet<String>,
        cx: &mut Context<Self>,
    ) {
        self.media.expanded.retain(|key| demand.contains(key));
        self.media
            .queue
            .retain(|(key, _)| demand.contains(key) || self.open.contains(key));
        let stale: Vec<_> = self
            .media
            .slots
            .keys()
            .filter(|key| !demand.contains(*key) && !self.open.contains(*key))
            .cloned()
            .collect();
        for key in stale {
            if self.media.viewer.as_ref() == Some(&key) {
                self.media.viewer = None;
            }
            if let Some(slot) = self.media.slots.remove(&key) {
                release(slot, cx);
            }
        }
    }

    /// What a card's slot holds, for tests: `Ok(in_files)` once an image or a message was
    /// read, the error once it failed, nothing while it loads or when nothing was asked.
    #[cfg(test)]
    pub(super) fn image_slot(&self, key: &str) -> Option<Result<bool, String>> {
        let slot = self
            .media
            .slots
            .get(key)
            .or_else(|| self.media.thumbnails.slots.get(key))?;
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
        for (_, slot) in self
            .media
            .slots
            .drain()
            .chain(self.media.thumbnails.slots.drain())
        {
            if let Some(Ok(content)) = slot.content
                && let Some(image) = content.render_image()
            {
                let image = image.clone();
                cx.defer(move |cx| cx.drop_image(image, None));
            }
        }
    }

    /// Copy the displayed raster as PNG, converting GPUI's BGRA bytes back to RGBA.
    fn copy_tool_image(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(Ok(PreviewContent::Image { image, .. })) = self
            .media
            .slots
            .get(key)
            .and_then(|slot| slot.content.as_ref())
        else {
            return;
        };
        let Some(bytes) = image.as_bytes(0) else {
            return;
        };
        let size = image.size(0);
        let mut rgba = bytes.to_vec();
        for pixel in rgba.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
        }
        let Some(buffer) =
            image::RgbaImage::from_raw(size.width.0 as u32, size.height.0 as u32, rgba)
        else {
            return;
        };
        let mut encoded = std::io::Cursor::new(Vec::new());
        if image::DynamicImage::ImageRgba8(buffer)
            .write_to(&mut encoded, image::ImageFormat::Png)
            .is_err()
        {
            return;
        }
        let clipboard = gpui::Image::from_bytes(gpui::ImageFormat::Png, encoded.into_inner());
        cx.write_to_clipboard(gpui::ClipboardItem::new_image(&clipboard));
        self.mark_copied(format!("copy-image:{key}"), cx);
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
        let global = self.info().is_some_and(|info| {
            matches!(
                info.orchestrator,
                Some(crate::chat::model::OrchestratorScope::Global)
            )
        });
        // Old user messages without owned snapshot metadata keep their explicit
        // image disclosure. New sent files use the owned-snapshot grid instead.
        let legacy_user =
            context.is_some_and(|item| matches!(item.body, ItemBody::UserMessage { .. }));
        if !legacy_user
            && (!global
                || context.is_some_and(|item| {
                    !matches!(
                        item.body,
                        ItemBody::UserMessage { .. } | ItemBody::AgentMessage { .. }
                    )
                }))
        {
            let owner = context.map(|item| item.id.clone());
            let preview_image = image.clone();
            let preview_key = key.to_owned();
            let content = self
                .media
                .slots
                .get(key)
                .filter(|slot| slot.source == image.source)
                .and_then(|slot| slot.content.as_ref());
            let group = SharedString::from(format!("tc:{}", owner.as_deref().unwrap_or(key)));
            let copied_key = format!("copy-image:{key}");
            let copied = self.copied.as_deref() == Some(copied_key.as_str());
            let show_actions = copied || owner.as_deref().is_some_and(|id| self.row_is_focused(id));
            let ready = matches!(content, Some(Ok(PreviewContent::Image { .. })));
            let (width, aspect) = match content {
                Some(Ok(PreviewContent::Image { image, .. })) => {
                    let size = image.size(0);
                    let aspect = size.width.0 as f32 / (size.height.0 as f32).max(1.0);
                    let (width, _) =
                        fitted_image_size(size.width.0 as f32, size.height.0 as f32, f32::MAX);
                    (Some(width), Some(aspect))
                }
                _ => (None, None),
            };
            let body = match content {
                Some(Ok(PreviewContent::Image { image, .. })) => img(image.clone())
                    .size_full()
                    .object_fit(gpui::ObjectFit::Contain)
                    .into_any_element(),
                _ => div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(6.0))
                    .text_size(ui_text::text(11.0))
                    .text_color(rgb(look.colors.muted))
                    .child(crate::icons::symbol("photo", 12.0, Some(look.colors.muted)))
                    .child(match content {
                        Some(Err(error)) | Some(Ok(PreviewContent::Message(error))) => {
                            error.clone()
                        }
                        _ => "Loading image…".into(),
                    })
                    .into_any_element(),
            };
            let copy_key = key.to_owned();
            let open_owner = owner.clone();
            let open_image = preview_image.clone();
            let open_key = preview_key.clone();
            let backing = gpui::rgba((look.colors.panel << 8) | 0xd9);
            return div()
                .id(SharedString::from(format!("tool-image:{key}")))
                .cursor_text()
                .when(context.is_none(), |frame| frame.group(group.clone()))
                .relative()
                .flex_none()
                .when_some(width, |frame, width| frame.w(px(width)))
                .when(width.is_none(), |frame| frame.w_full().h(px(72.0)))
                .max_w_full()
                .max_h(px(320.0))
                .when_some(aspect, |frame, aspect| frame.aspect_ratio(aspect))
                .bg(rgb(look.colors.panel))
                .border_1()
                .border_color(rgb(look.colors.divider))
                .rounded(px(4.0))
                .overflow_hidden()
                .child(
                    crate::behavior_controls::button_content(
                        SharedString::from(format!("view:{key}")),
                        "Preview image",
                        body,
                    )
                    .absolute()
                    .inset_0()
                    .size_full()
                    .p_0()
                    .cursor_text()
                    .on_click(cx.listener(move |v, _, _, cx| {
                        cx.stop_propagation();
                        v.preview_image_reference(
                            &preview_image,
                            &preview_key,
                            owner.as_deref(),
                            cx,
                        );
                    })),
                )
                .child(
                    div()
                        .absolute()
                        .top(px(4.0))
                        .right(px(4.0))
                        .flex()
                        .gap(px(2.0))
                        .child(
                            super::widgets::tool_icon_action(
                                SharedString::from(format!("copy-image:{key}")),
                                if copied { "checkmark" } else { "doc.on.doc" },
                                if copied { "Copied" } else { "Copy image" },
                                ready,
                                look,
                            )
                            .bg(backing)
                            .opacity(if show_actions { 1.0 } else { 0.0 })
                            .group_hover(group.clone(), |style| style.opacity(1.0))
                            .on_click(cx.listener(
                                move |v, _, _, cx| {
                                    cx.stop_propagation();
                                    v.copy_tool_image(&copy_key, cx);
                                },
                            )),
                        )
                        .child(
                            super::widgets::tool_icon_action(
                                SharedString::from(format!("open-image:{key}")),
                                "arrow.up.right.square",
                                "Open in viewer",
                                true,
                                look,
                            )
                            .bg(backing)
                            .opacity(if show_actions { 1.0 } else { 0.0 })
                            .group_hover(group, |style| style.opacity(1.0))
                            .on_click(cx.listener(
                                move |v, _, _, cx| {
                                    cx.stop_propagation();
                                    v.preview_image_reference(
                                        &open_image,
                                        &open_key,
                                        open_owner.as_deref(),
                                        cx,
                                    );
                                },
                            )),
                        ),
                )
                .test_support()
                .into_any_element();
        }

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
            .children(
                open.then(|| self.image_actions(key, "card", look, cx))
                    .flatten(),
            )
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
            let (thumbnail, queued) = if let Some(queued) = self.media.queue.pop_front() {
                (false, Some(queued))
            } else {
                (true, self.media.thumbnails.queue.pop_front())
            };
            let Some((key, image)) = queued else {
                break;
            };
            if !thumbnail && !self.open.contains(&key) && !self.media.expanded.contains(&key) {
                continue;
            }
            if thumbnail && !self.media.thumbnails.slots.contains_key(&key) {
                continue;
            }
            if !thumbnail
                && self
                    .media
                    .slots
                    .get(&key)
                    .is_some_and(|slot| slot.source == image.source)
            {
                continue;
            }
            if !thumbnail && self.media.slots.len() >= LOADED {
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
            let work = cx.background_executor().spawn(async move {
                let (content, in_files) = load(&owned, &scope);
                (
                    content.map(|content| {
                        if thumbnail {
                            thumbnail_content(content)
                        } else {
                            content
                        }
                    }),
                    in_files,
                )
            });
            self.media.active += 1;
            let generation = if thumbnail {
                self.media.thumbnails.slots[&key].generation
            } else {
                self.media.generation += 1;
                self.media.generation
            };
            let completion_key = key.clone();
            let source = image.source.clone();
            cx.spawn(async move |this, cx| {
                let (content, in_files) = work.await;
                let _ = this.update(cx, |view, cx| {
                    view.media.active = view.media.active.saturating_sub(1);
                    let stale = if thumbnail {
                        view.media.thumbnails.complete(
                            &completion_key,
                            generation,
                            &source,
                            content,
                            in_files,
                        )
                    } else {
                        view.media
                            .complete(&completion_key, generation, &source, content, in_files)
                    };
                    if let Some(content) = stale
                        && let Some(image) = content.render_image()
                    {
                        let image = image.clone();
                        cx.defer(move |cx| cx.drop_image(image, None));
                    }
                    view.start_images(cx);
                    // Thumbnails have fixed geometry. Full images can also live
                    // in prose or an Activity artifact strip, outside the call's
                    // own list entry; those keys need a conservative fallback.
                    if !thumbnail {
                        view.remeasure_completed_image(&completion_key);
                    }
                    cx.notify();
                });
            })
            .detach();
            if !thumbnail {
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
    }

    fn remeasure_completed_image(&self, key: &str) {
        let row = key.rsplit_once(":image:").and_then(|(owner, _)| {
            self.list_rows
                .iter()
                .position(|entry| entry.row.id == owner)
        });
        if let Some(at) = row {
            self.list.remeasure_items(at..at + 1);
        } else {
            self.list.remeasure();
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
                .children(self.image_actions(key, "viewer", look, cx))
                .children(
                    self.notices
                        .local(super::notices::LocalKey::Export)
                        .map(|message| {
                            div()
                                .text_size(ui_text::text(11.))
                                .child(message.to_owned())
                        }),
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

fn size_label(bytes: u64) -> String {
    crate::file_preview::format_size(bytes)
}

impl ChatView {
    pub(super) fn prepare_opened_images(
        &mut self,
        owner: &str,
        images: &[ChatImage],
        cx: &mut Context<Self>,
    ) {
        for (i, image) in images.iter().enumerate() {
            let key = format!("{owner}:image:{i}");
            self.media.expanded.insert(key.clone());
            if self
                .media
                .slots
                .get(&key)
                .is_some_and(|slot| slot.source != image.source)
                && let Some(slot) = self.media.slots.remove(&key)
            {
                release(slot, cx);
            }
            if !self.media.slots.contains_key(&key)
                && !self.media.queue.iter().any(|(queued, _)| queued == &key)
            {
                self.media.queue.push_back((key, image.clone()));
            }
        }
        self.start_images(cx);
    }

    /// The owned snapshot preview and tool thumbnails share one bounded cache.
    pub(super) fn thumbnail(
        &self,
        key: &str,
        path: &str,
    ) -> Option<Result<&Arc<gpui::RenderImage>, &str>> {
        let source = ImageSource::Local {
            path: path.to_owned(),
        };
        let content = self
            .media
            .thumbnails
            .slots
            .get(key)
            .filter(|slot| slot.source == source)?
            .content
            .as_ref()?;
        Some(match content {
            Ok(PreviewContent::Image { image, .. }) => Ok(image),
            Ok(PreviewContent::Message(message)) | Err(message) => Err(message.as_str()),
            Ok(_) => Err("This reference is not a supported image."),
        })
    }

    #[cfg(test)]
    pub(super) fn thumbnail_reads(&self) -> (usize, HashMap<String, u64>) {
        let slots = &self.media.thumbnails.slots;
        (
            slots.values().filter(|slot| slot.content.is_none()).count(),
            slots
                .iter()
                .map(|(key, slot)| (key.clone(), slot.generation))
                .collect(),
        )
    }

    /// Loading follows clipped thumbnail bounds, so even one message containing
    /// hundreds of images cannot evict its first images before they are drawn.
    pub(super) fn render_thumbnail(
        &self,
        key: &str,
        image: &ChatImage,
        fit: gpui::ObjectFit,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let content = self
            .media
            .thumbnails
            .slots
            .get(key)
            .filter(|slot| slot.source == image.source)
            .and_then(|slot| slot.content.as_ref())
            .and_then(|content| content.as_ref().ok())
            .and_then(PreviewContent::render_image);
        let view = cx.entity().downgrade();
        let key = key.to_owned();
        let image = image.clone();
        div()
            .size_full()
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .child(
                content
                    .map(|image| {
                        img(image.clone())
                            .size_full()
                            .object_fit(fit)
                            .into_any_element()
                    })
                    .unwrap_or_else(|| {
                        crate::icons::symbol("photo", 12.0, Some(look.colors.muted))
                    }),
            )
            .on_prepaint(move |bounds, window, cx| {
                if bounds.intersects(&window.content_mask().bounds) {
                    let _ = view.update(cx, |view, cx| {
                        if !view.media.thumbnails.slots.contains_key(&key)
                            && view.media.active >= CONCURRENT
                        {
                            return;
                        }
                        view.media.generation += 1;
                        for slot in view
                            .media
                            .thumbnails
                            .prepare(key, image, view.media.generation)
                        {
                            release(slot, cx);
                        }
                        view.start_images(cx);
                    });
                }
            })
            .into_any_element()
    }

    /// The first image remains recognizable while its call is collapsed.
    pub(super) fn image_header_thumbnail(
        &self,
        item: &Item,
        look: Look,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let image = item.presentation.images.first()?;
        let key = format!("{}:image:0", item.id);
        let image = image.clone();
        Some(
            div()
                .id(SharedString::from(format!("header-thumb:{key}")))
                .role(gpui::Role::Image)
                .aria_label(image_label(&image, Some(item)))
                .child(self.render_thumbnail(&key, &image, gpui::ObjectFit::Contain, look, cx))
                .w(px(28.0))
                .h(px(20.0))
                .flex_none()
                .rounded(px(4.0))
                .border_1()
                .border_color(rgb(look.colors.divider))
                .bg(rgb(look.colors.panel))
                .overflow_hidden()
                .test_support()
                .into_any_element(),
        )
    }

    fn file_chip(
        &self,
        key: String,
        name: String,
        size: Option<u64>,
        target: String,
        owner: String,
        image: Option<&ChatImage>,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let visible = size.map_or_else(
            || name.clone(),
            |size| format!("{name} {}", size_label(size)),
        );
        let highlights = size
            .map(|_| {
                (
                    name.len() + 1..visible.len(),
                    gpui::HighlightStyle {
                        color: Some(rgb(look.colors.muted).into()),
                        ..Default::default()
                    },
                )
            })
            .into_iter()
            .collect();
        let text = self.selectable(&key, visible, highlights, Vec::new(), look, cx);
        let is_image = key
            .rsplit_once(":image:")
            .is_some_and(|(_, index)| index.parse::<usize>().is_ok());
        let icon = image
            .map(|image| {
                let cache_key = key.strip_prefix("artifact:").unwrap_or(&key);
                // Smaller than the 11 pt text line at every scale, so an image chip is
                // exactly as tall as a file chip; a 24 px thumbnail made it 6 px taller.
                div()
                    .flex_none()
                    .size(ui_text::text(16.0))
                    .child(self.render_thumbnail(
                        cache_key,
                        image,
                        gpui::ObjectFit::Cover,
                        look,
                        cx,
                    ))
                    .into_any_element()
            })
            .unwrap_or_else(|| {
                crate::icons::symbol(
                    if is_image { "photo" } else { "doc" },
                    11.0,
                    Some(look.colors.muted),
                )
            });
        crate::behavior_controls::button_content(
            SharedString::from(format!("chip:{key}")),
            format!("Preview {name}"),
            div()
                .flex()
                .items_center()
                .gap(ui_text::space(5.0))
                .child(icon)
                .child(text),
        )
        .px(ui_text::space(7.0))
        .py(ui_text::space(3.0))
        .rounded(ui_text::space(6.0))
        .border_1()
        .border_color(rgb(look.colors.divider))
        .bg(rgb(look.colors.panel))
        .text_size(ui_text::text(11.0))
        .text_color(rgb(look.colors.text))
        .on_click(cx.listener(move |v, _, _, cx| {
            cx.stop_propagation();
            v.preview_reference(&target, Some(&owner), cx);
        }))
        .into_any_element()
    }

    pub(super) fn render_artifact_refs(
        &self,
        owner: &str,
        refs: &[riwork_engine::chat::projection::ArtifactRef],
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let range = artifact_range(
            refs.len(),
            self.media.artifact_pages.get(owner).copied().unwrap_or(0),
        );
        let page = range.start / ARTIFACT_PAGE;
        let previous_owner = owner.to_owned();
        let next_owner = owner.to_owned();
        let chips = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(ui_text::space(4.0))
            .children(refs[range.clone()].iter().filter_map(|artifact| {
                let (_, item) = self.projected_item(&artifact.item_id)?;
                if let Some(path) = &artifact.path {
                    let target = path
                        .line
                        .map_or_else(|| path.path.clone(), |line| format!("{}:{line}", path.path));
                    let name = std::path::Path::new(&path.path)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.path.clone());
                    Some(self.file_chip(
                        format!("artifact:{}:path:{}", item.id, path.path),
                        name,
                        None,
                        target,
                        item.id.clone(),
                        None,
                        look,
                        cx,
                    ))
                } else {
                    let n = artifact.image_index?;
                    let image = item.presentation.images.get(n)?;
                    if self.info().is_some_and(|info| {
                        matches!(
                            info.orchestrator,
                            Some(crate::chat::model::OrchestratorScope::Global)
                        )
                    }) {
                        return Some(self.image_card(
                            image,
                            &format!("{}:image:{n}", item.id),
                            Some(item),
                            None,
                            look,
                            cx,
                        ));
                    }
                    let target = match &image.source {
                        ImageSource::Local { path } => path.clone(),
                        ImageSource::Url { url } => url.clone(),
                        _ => format!("image:{}:{n}", item.id),
                    };
                    Some(self.file_chip(
                        format!("artifact:{}:image:{n}", item.id),
                        image_label(image, Some(item)),
                        None,
                        target,
                        item.id.clone(),
                        Some(image),
                        look,
                        cx,
                    ))
                }
            }));
        div()
            .flex()
            .flex_col()
            .gap(ui_text::space(4.0))
            .child(chips)
            .when(refs.len() > ARTIFACT_PAGE, |body| {
                body.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(ui_text::space(6.0))
                        .children((range.start > 0).then(|| {
                            button(
                                SharedString::from(format!("previous:artifacts:{owner}")),
                                "Previous files",
                                None,
                                look,
                            )
                            .on_click(cx.listener(
                                move |view, _, _, cx| {
                                    view.set_artifact_page(
                                        &previous_owner,
                                        page.saturating_sub(1),
                                        cx,
                                    );
                                },
                            ))
                        }))
                        .child(format!(
                            "{}–{} of {}",
                            range.start + 1,
                            range.end,
                            refs.len()
                        ))
                        .children((range.end < refs.len()).then(|| {
                            button(
                                SharedString::from(format!("next:artifacts:{owner}")),
                                "Next files",
                                None,
                                look,
                            )
                            .on_click(cx.listener(
                                move |view, _, _, cx| {
                                    view.set_artifact_page(&next_owner, page + 1, cx);
                                },
                            ))
                        })),
                )
            })
            .into_any_element()
    }

    fn set_artifact_page(&mut self, owner: &str, page: usize, cx: &mut Context<Self>) {
        let offset = self.list.logical_scroll_top();
        self.list.pause_following_tail();
        self.media.artifact_pages.insert(owner.to_owned(), page);
        if let Some(at) = self
            .list_rows
            .iter()
            .position(|entry| entry.row.id == owner)
        {
            self.list.remeasure_items(at..at + 1);
        }
        self.list.scroll_to(offset);
        cx.notify();
    }
}

/// Whether a row lists `artifact` as a chip. Verbose lists every file a call produced,
/// including an image the call already shows; only a Normal call row folds the chip
/// of an image it shows inline. Messages and activity summaries list every chip.
pub(super) fn artifact_chip_shown(
    mode: super::display::DisplayMode,
    kind: riwork_engine::chat::projection::RowKind,
    artifact: &riwork_engine::chat::projection::ArtifactRef,
    item: Option<&Item>,
    open: bool,
) -> bool {
    use riwork_engine::chat::projection::RowKind;
    mode == super::display::DisplayMode::Verbose
        || matches!(
            kind,
            RowKind::User
                | RowKind::Final
                | RowKind::Commentary
                | RowKind::Compaction
                | RowKind::Notice
        )
        || item.is_none_or(|item| !artifact_shown_inline(artifact, item, open))
}

/// A collapsed header shows its first image; an open call shows every image.
/// Extra images and files still need their chips.
fn artifact_shown_inline(
    artifact: &riwork_engine::chat::projection::ArtifactRef,
    item: &Item,
    open: bool,
) -> bool {
    if artifact.item_id != item.id {
        return false;
    }
    let shown = if open {
        item.presentation.images.len()
    } else {
        1
    };
    if let Some(index) = artifact.image_index {
        return index < shown && index < item.presentation.images.len();
    }
    let Some(path) = &artifact.path else {
        return false;
    };
    item.presentation.images.iter().take(shown).any(|image| match &image.source {
        ImageSource::Local { path: source } => source == &path.path,
        ImageSource::Url { url } => url == &path.path,
        ImageSource::Data { .. } => item.presentation.images.len() == 1 && (
            item.presentation.semantics.as_ref().and_then(|s| s.target.as_deref()) == Some(path.path.as_str())
            || matches!(&item.body, ItemBody::ToolCall { input, .. } if ["file_path", "path", "target"].iter().any(|key| input.get(key).and_then(serde_json::Value::as_str) == Some(path.path.as_str())))
        ),
        ImageSource::Unavailable { .. } => false,
    })
}
