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
use gpui::{AnyElement, Context, SharedString, div, img, prelude::*, px, rgb};
use gpui_kit::base::{ElementExt as _, TestSupportExt as _};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::Read,
    sync::Arc,
    time::Duration,
};

const CONCURRENT: usize = 2;
const LOADED: usize = 8;
const OPEN: usize = 16;
const THUMBNAILS: usize = 64;
const THUMBNAIL_EDGE: u32 = 360;
const OPEN_LIMIT: &str = "Fold an image before opening more.";
const NETWORK_BYTES: u64 = 8 * 1024 * 1024;
pub(super) const ARTIFACT_PAGE: usize = 32;

pub(super) fn artifact_range(count: usize, page: usize) -> std::ops::Range<usize> {
    let start = page.min(count.saturating_sub(1) / ARTIFACT_PAGE) * ARTIFACT_PAGE;
    start..count.min(start.saturating_add(ARTIFACT_PAGE))
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
                self.expanded.remove(key);
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
            let slot = self.thumbnails.slots.remove(&key).unwrap();
            self.thumbnails.lru.retain(|cached| cached != &key);
            self.thumbnails.queue.retain(|(queued, _)| queued != &key);
            self.generation += 1;
            released.extend(self.thumbnails.prepare(
                key,
                ChatImage {
                    label: String::new(),
                    source: slot.source.clone(),
                },
                self.generation,
            ));
            released.push(slot);
        }
        released
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn decoded_image(width: u32, height: u32) -> PreviewContent {
        PreviewContent::Image {
            image: Arc::new(gpui::RenderImage::new([image::Frame::new(
                image::RgbaImage::from_pixel(width, height, image::Rgba([17, 31, 47, 255])),
            )])),
            description: format!("{width} × {height} pixels"),
        }
    }

    #[test]
    fn thumbnail_cache_evicts_lru_and_rejects_a_late_result_for_an_evicted_entry() {
        let image = ChatImage {
            label: "Thumbnail".into(),
            source: ImageSource::Unavailable {
                reason: "fixture".into(),
            },
        };
        let mut cache = ThumbnailCache::default();
        for n in 0..THUMBNAILS {
            assert!(
                cache
                    .prepare(format!("{n}"), image.clone(), n as u64)
                    .is_empty()
            );
        }
        cache.prepare("0".into(), image.clone(), 100);
        let released = cache.prepare("new".into(), image.clone(), 101);
        assert_eq!(released.len(), 1);
        assert!(
            cache.slots.contains_key("0"),
            "a reused thumbnail is most recent"
        );
        assert!(!cache.slots.contains_key("1"));
        assert_eq!(cache.slots.len(), THUMBNAILS);
        assert_eq!(cache.queue.len(), THUMBNAILS);
        assert!(
            cache
                .complete("1", 1, &image.source, Ok(decoded_image(20, 10)), false)
                .is_some()
        );
        cache.prepare("1".into(), image.clone(), 102);
        assert!(
            cache
                .complete("1", 1, &image.source, Ok(decoded_image(20, 10)), false)
                .is_some()
        );
        assert!(cache.slots["1"].content.is_none());
        assert!(
            cache
                .complete("1", 102, &image.source, Ok(decoded_image(20, 10)), false)
                .is_none()
        );
    }

    #[test]
    fn thumbnails_keep_only_downscaled_pixels_and_preserve_their_channel_order() {
        let large = thumbnail_content(decoded_image(1600, 900));
        let image = large.render_image().unwrap();
        let size = image.size(0);
        assert_eq!(size.width.0, THUMBNAIL_EDGE as i32);
        assert!(size.height.0 <= THUMBNAIL_EDGE as i32);
        assert_eq!(&image.as_bytes(0).unwrap()[..4], &[17, 31, 47, 255]);
        let small = thumbnail_content(decoded_image(40, 20));
        assert_eq!(small.render_image().unwrap().size(0).width.0, 40);
    }

    #[test]
    fn rescoping_requeues_local_thumbnails_without_accepting_the_old_scope_result() {
        let source = ImageSource::Local {
            path: "shot.png".into(),
        };
        let mut media = MediaState {
            generation: 1,
            ..Default::default()
        };
        media.thumbnails.prepare(
            "shot:image:0".into(),
            ChatImage {
                label: "Shot".into(),
                source: source.clone(),
            },
            1,
        );
        media.thumbnails.queue.clear(); // the old scope's worker is in flight
        assert_eq!(media.rescope(&HashSet::new()).len(), 1);
        let generation = media.thumbnails.slots["shot:image:0"].generation;
        assert_ne!(generation, 1);
        assert_eq!(media.thumbnails.queue.len(), 1);
        assert!(
            media
                .thumbnails
                .complete("shot:image:0", 1, &source, Ok(decoded_image(20, 10)), false)
                .is_some()
        );
        assert!(media.thumbnails.slots["shot:image:0"].content.is_none());
        assert!(
            media
                .thumbnails
                .complete(
                    "shot:image:0",
                    generation,
                    &source,
                    Ok(decoded_image(20, 10)),
                    false
                )
                .is_none()
        );
    }

    #[test]
    fn image_canvas_caps_height_and_never_upscales_small_images() {
        assert_eq!(fitted_image_size(40.0, 20.0, 800.0), (40.0, 20.0));
        assert_eq!(fitted_image_size(1000.0, 500.0, 400.0), (400.0, 200.0));
        assert_eq!(fitted_image_size(640.0, 360.0, 800.0).1, 320.0);
        assert_eq!(fitted_image_size(100.0, 1000.0, 800.0), (32.0, 320.0));
    }

    #[gpui::test]
    fn full_image_completion_invalidates_prose_and_hidden_artifact_measurements(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::chat::model::{ItemStatus, Presentation};
        use gpui_kit::test::TestWindowExt;
        let (handle, view, _) = super::super::editor_tests::mount_selection(cx);
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |view, cx| {
                view.model.link = super::super::Link::Live;
                view.model.transcript.items = ["message-a", "message-b"]
                    .into_iter()
                    .map(|id| Item {
                        id: id.into(),
                        turn_id: None,
                        started_at: None,
                        ended_at: None,
                        status: ItemStatus::Completed,
                        presentation: Presentation::default(),
                        body: ItemBody::AgentMessage { text: id.into() },
                    })
                    .collect();
                view.refresh_projection(cx);
            });
            for key in [
                "message-a:image:0",
                "user:message-a/0/image/hash/0",
                "hidden-artifact-call:image:0",
            ] {
                window.render_frame(cx);
                view.update(cx, |view, _| {
                    assert!(view.list.bounds_for_item(0).is_some());
                    assert!(view.list.bounds_for_item(1).is_some());
                    view.remeasure_completed_image(key);
                    assert!(view.list.bounds_for_item(0).is_none());
                    assert_eq!(
                        view.list.bounds_for_item(1).is_some(),
                        key == "message-a:image:0",
                        "{key} must invalidate every row if its rendered owner is unknown"
                    );
                });
            }
        })
        .unwrap();
    }

    #[gpui::test]
    fn tool_images_keep_compact_headers_and_only_unshown_artifact_chips(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::chat::model::{ItemStatus, PathRef, Presentation, Semantics, ToolKind};
        use gpui_kit::test::TestWindowExt;
        let old = ui_text::set_for_tests(1.0, ui_text::Face::Menlo);
        let (handle, view, _) = super::super::editor_tests::mount_selection(cx);
        let paths = ["/tmp/polish-first.png", "/tmp/polish-second.png"];
        let images: Vec<_> = paths
            .iter()
            .map(|path| ChatImage {
                label: "Image".into(),
                source: ImageSource::Local {
                    path: (*path).into(),
                },
            })
            .collect();
        view.update(cx, |view, cx| {
            view.model.link = super::super::Link::Live;
            view.model.transcript.items = vec![Item {
                id: "shot".into(),
                turn_id: Some("turn".into()),
                started_at: None,
                ended_at: None,
                status: ItemStatus::Completed,
                presentation: Presentation {
                    semantics: Some(Semantics {
                        kind: ToolKind::Read,
                        target: Some(paths[0].into()),
                        produced: paths
                            .iter()
                            .copied()
                            .chain(["/tmp/polish-notes.txt"])
                            .map(|path| PathRef {
                                path: path.into(),
                                line: None,
                            })
                            .collect(),
                        ..Default::default()
                    }),
                    images: images.clone(),
                    ..Default::default()
                },
                body: ItemBody::ToolCall {
                    server: None,
                    tool: "Read".into(),
                    input: serde_json::json!({ "file_path": paths[0] }),
                    output: Some(" [image]\n\n[IMAGE] ".into()),
                },
            }];
            view.display_mode = super::super::DisplayMode::Verbose;
            view.refresh_projection(cx);
            // Decoded fixtures keep this test independent of disk and image workers.
            for (index, image) in images.iter().enumerate() {
                view.media.thumbnails.slots.insert(
                    format!("shot:image:{index}"),
                    Slot {
                        generation: 0,
                        source: image.source.clone(),
                        content: Some(Ok(decoded_image(120, 60))),
                        in_files: false,
                    },
                );
            }
            view.list.remeasure();
            cx.notify();
        });
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("card:shot").bounds().size.height, px(24.0));
            let thumb = window.find("header-thumb:shot:image:0").bounds();
            assert_eq!(
                window.find("header-thumb:shot:image:0").role(),
                Some(gpui::Role::Image)
            );
            assert!(thumb.size.height > px(0.0));
            assert!(thumb.size.height <= px(20.0), "{thumb:?}");
            assert!(window.try_find("chip:artifact:shot:image:0").is_none());
            assert!(
                window
                    .try_find("chip:artifact:shot:path:/tmp/polish-first.png")
                    .is_none()
            );
            assert!(window.find("chip:artifact:shot:image:1").visible());
            assert!(
                window
                    .find("chip:artifact:shot:path:/tmp/polish-second.png")
                    .visible()
            );
            assert!(
                window
                    .find("chip:artifact:shot:path:/tmp/polish-notes.txt")
                    .visible()
            );

            view.update(cx, |view, cx| {
                for (index, image) in images.iter().enumerate() {
                    view.media.slots.insert(
                        format!("shot:image:{index}"),
                        Slot {
                            generation: 0,
                            source: image.source.clone(),
                            content: Some(Ok(decoded_image(120, 60))),
                            in_files: false,
                        },
                    );
                }
                view.list.remeasure();
                cx.notify();
            });
            window.click("header-thumb:shot:image:0", cx);
            assert!(
                super::super::display::find(&view.read(cx).visible, "shot")
                    .unwrap()
                    .open,
                "a thumbnail is row decoration, so clicking it toggles the row"
            );
            window.render_frame(cx);
            assert!(window.try_find("header-thumb:shot:image:0").is_none());
            assert!(window.try_find("transcript:tool-output:shot").is_none());
            assert!(window.try_find("copy:shot").is_none());
            for index in 0..2 {
                assert!(
                    window
                        .find(SharedString::from(format!("view:shot:image:{index}")))
                        .visible()
                );
                assert!(
                    window
                        .try_find(SharedString::from(format!(
                            "chip:artifact:shot:image:{index}"
                        )))
                        .is_none()
                );
            }
            for path in paths {
                assert!(
                    window
                        .try_find(SharedString::from(format!(
                            "chip:artifact:shot:path:{path}"
                        )))
                        .is_none()
                );
            }
            assert!(
                window
                    .find("chip:artifact:shot:path:/tmp/polish-notes.txt")
                    .visible()
            );

            view.update(cx, |view, cx| {
                let ItemBody::ToolCall { output, .. } = &mut view.model.transcript.items[0].body
                else {
                    unreachable!();
                };
                *output = Some("Image caption\n[image]".into());
                view.refresh_projection(cx);
                view.list.remeasure();
                cx.notify();
            });
            window.render_frame(cx);
            assert_eq!(
                window.find("transcript:tool-output:shot:0").label(),
                Some("Image caption")
            );
            assert!(window.try_find("transcript:tool-output:shot:1").is_none());
            assert!(
                window
                    .try_find("output-placeholder:tool-output:shot")
                    .is_none()
            );
            window.hover("output-block:shot", cx);
            assert!(window.find("copy:shot").visible());

            view.update(cx, |view, cx| {
                view.apply_display_mode(super::super::DisplayMode::Normal, cx)
            });
            window.render_frame(cx);
            assert_eq!(window.find("card:activity:turn").expanded(), Some(false));
            assert!(window.find("chip:artifact:shot:image:0").visible());
            assert!(window.find("chip:artifact:shot:image:1").visible());
            assert!(
                window
                    .find("chip:artifact:shot:path:/tmp/polish-notes.txt")
                    .visible()
            );
        })
        .unwrap();
        ui_text::set_for_tests(old.0, old.1);
    }

    #[gpui::test]
    fn one_hundred_image_message_loads_visible_thumbnails_without_starvation_or_manual_opens(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::chat::model::{ChatEvent, ItemStatus, Presentation};
        use gpui_kit::test::TestWindowExt;
        let (handle, view, _) = super::super::editor_tests::mount_selection(cx);
        let images: Vec<_> = (0..100)
            .map(|n| ChatImage {
                label: format!("Image {n}"),
                source: ImageSource::Unavailable {
                    reason: format!("fixture {n}"),
                },
            })
            .collect();
        view.update(cx, |view, cx| {
            view.model.apply(&[crate::chat::wire::Envelope {
                chat_id: "fixture-chat".into(),
                seq: 1,
                event: ChatEvent::ItemCompleted {
                    item: Item {
                        id: "history".into(),
                        turn_id: Some("turn".into()),
                        started_at: None,
                        ended_at: None,
                        status: ItemStatus::Completed,
                        presentation: Presentation {
                            images: images.clone(),
                            ..Default::default()
                        },
                        body: ItemBody::UserMessage {
                            text: "All screenshots".into(),
                        },
                    },
                },
            }]);
            view.refresh_projection(cx);
            view.list.pause_following_tail();
            view.list.scroll_to(gpui::ListOffset {
                item_ix: 0,
                offset_in_item: px(0.0),
            });
            cx.notify();
        });
        for _ in 0..3 {
            cx.update_window(handle.into(), |_, window, cx| window.render_frame(cx))
                .unwrap();
            cx.run_until_parked();
        }
        view.read_with(cx, |view, _| {
            assert!(view.open.is_empty());
            assert!(view.media.slots.is_empty());
            assert!(
                view.media.thumbnails.slots.len() < THUMBNAILS,
                "offscreen grid cells must not evict visible thumbnails"
            );
            assert!(
                view.media.thumbnails.slots["history:image:0"]
                    .content
                    .is_some()
            );
        });
        // Walk the same long grid, exceeding its decoded cache, then return to
        // the first image. A missing/evicted visible image must be queued again.
        for offset in (0..=4200).step_by(600).chain([0]) {
            view.update(cx, |view, cx| {
                view.list.scroll_to(gpui::ListOffset {
                    item_ix: 0,
                    offset_in_item: px(offset as f32),
                });
                cx.notify();
            });
            cx.update_window(handle.into(), |_, window, cx| window.render_frame(cx))
                .unwrap();
            cx.run_until_parked();
        }
        view.update(cx, |view, cx| {
            assert!(view.media.thumbnails.slots.len() <= THUMBNAILS);
            assert!(view.media.thumbnails.queue.is_empty());
            assert!(
                view.media.thumbnails.slots["history:image:0"]
                    .content
                    .is_some()
            );
            view.toggle_image("manual:image:0".into(), images[0].clone(), cx);
            assert_eq!(view.open, HashSet::from(["manual:image:0".to_owned()]));
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    fn opened_image_layout_is_unscaled_and_small_pixels_stay_at_natural_size(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::chat::model::{ChatEvent, ItemStatus, Presentation};
        use gpui_kit::test::TestWindowExt;
        let (handle, view, _) = super::super::editor_tests::mount_selection(cx);
        let old = ui_text::set_for_tests(1.5, ui_text::Face::SystemMono);
        let image = ChatImage {
            label: "Shot".into(),
            source: ImageSource::Unavailable {
                reason: "fixture".into(),
            },
        };
        view.update(cx, |view, cx| {
            view.model.apply(&[crate::chat::wire::Envelope {
                chat_id: "fixture-chat".into(),
                seq: 1,
                event: ChatEvent::ItemCompleted {
                    item: Item {
                        id: "shot".into(),
                        turn_id: Some("turn".into()),
                        started_at: None,
                        ended_at: None,
                        status: ItemStatus::Completed,
                        presentation: Presentation {
                            images: vec![image.clone()],
                            ..Default::default()
                        },
                        body: ItemBody::ToolCall {
                            server: None,
                            tool: "capture_image".into(),
                            input: serde_json::Value::Null,
                            output: None,
                        },
                    },
                },
            }]);
            view.display_mode = super::super::display::DisplayMode::Verbose;
            view.disclosure.overrides.insert("shot".into(), true);
            view.refresh_projection(cx);
        });
        for (width, height) in [(640, 360), (40, 20)] {
            view.update(cx, |view, cx| {
                view.media.slots.insert(
                    "shot:image:0".into(),
                    Slot {
                        generation: 0,
                        source: image.source.clone(),
                        content: Some(Ok(decoded_image(width, height))),
                        in_files: false,
                    },
                );
                view.list.remeasure();
                cx.notify();
            });
            cx.run_until_parked();
            cx.update_window(handle.into(), |_, window, cx| {
                window.render_frame(cx);
                let bounds = window.find("view:shot:image:0").bounds();
                assert!(bounds.size.height > px(0.0));
                assert!(
                    bounds.size.height <= px(320.0),
                    "font scale must not enlarge the image cap: {bounds:?}"
                );
                let frame = window.find("tool-image:shot:image:0").bounds();
                assert!(
                    frame.size.width <= px(width as f32),
                    "image frame never upscales"
                );
                assert!(frame.size.height <= px(320.0));
                if width == 40 {
                    assert_eq!(frame.size.width, px(40.0));
                    assert_eq!(frame.size.height, px(20.0));
                }
                window.hover("tool-image:shot:image:0", cx);
                window.click("copy-image:shot:image:0", cx);
                let clipboard = cx.read_from_clipboard().expect("copied raster");
                let gpui::ClipboardEntry::Image(copied_image) = &clipboard.entries()[0] else {
                    panic!("copy image writes image bytes, not a path");
                };
                let decoded = image::load_from_memory(copied_image.bytes())
                    .unwrap()
                    .into_rgba8();
                assert_eq!(decoded.dimensions(), (width, height));
                assert_eq!(decoded.get_pixel(0, 0).0, [47, 31, 17, 255]);
                assert_eq!(
                    view.read(cx).copied.as_deref(),
                    Some("copy-image:shot:image:0")
                );
            })
            .unwrap();
        }
        // Loading and failed images keep the same compact placeholder geometry.
        for content in [None, Some(Err("Image could not be loaded".into()))] {
            view.update(cx, |view, cx| {
                view.copied = None;
                view.media.slots.insert(
                    "shot:image:0".into(),
                    Slot {
                        generation: 0,
                        source: image.source.clone(),
                        content,
                        in_files: false,
                    },
                );
                view.list.remeasure();
                cx.notify();
            });
            cx.update_window(handle.into(), |_, window, cx| {
                window.render_frame(cx);
                assert_eq!(
                    window.find("tool-image:shot:image:0").bounds().size.height,
                    px(72.0)
                );
                assert_eq!(
                    window.find("copy-image:shot:image:0").label(),
                    Some("Copy image")
                );
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                    "unavailable sentinel".into(),
                ));
                window.hover("tool-image:shot:image:0", cx);
                window.click("copy-image:shot:image:0", cx);
                assert_eq!(
                    cx.read_from_clipboard().and_then(|item| item.text()),
                    Some("unavailable sentinel".into())
                );
                assert_eq!(
                    window.find("open-image:shot:image:0").bounds().size,
                    gpui::size(px(20.0), px(20.0))
                );
            })
            .unwrap();
        }
        ui_text::set_for_tests(old.0, old.1);
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
        for pixel in rgba.chunks_exact_mut(4) {
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
        if !global
            || context.is_some_and(|item| {
                !matches!(
                    item.body,
                    ItemBody::UserMessage { .. } | ItemBody::AgentMessage { .. }
                )
            })
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
                                    view.media.viewer = Some(viewer_key.clone());
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
    pub(super) fn preview_image_reference(
        &mut self,
        image: &ChatImage,
        key: &str,
        _owner: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        if !self.open.contains(key) {
            self.toggle_image(key.to_owned(), image.clone(), cx);
        }
        self.media.viewer = Some(key.to_owned());
        cx.notify();
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
    pub(super) fn image_viewer(&self, look: Look, cx: &mut Context<Self>) -> Option<AnyElement> {
        let key = self.media.viewer.as_ref()?;
        let slot = self.media.slots.get(key)?;
        let Some(Ok(PreviewContent::Image { image, .. })) = &slot.content else {
            return None;
        };
        Some(
            div()
                .absolute()
                .inset_0()
                .occlude()
                .bg(rgb(look.colors.panel))
                .p(ui_text::space(16.0))
                .flex()
                .flex_col()
                .child(
                    button("close-chat-image", "Close image", None, look).on_click(cx.listener(
                        |view, _, _, cx| {
                            view.media.viewer = None;
                            cx.notify();
                        },
                    )),
                )
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .relative()
                        .overflow_hidden()
                        .child(
                            img(Arc::clone(image))
                                .absolute()
                                .inset_0()
                                .size_full()
                                .aspect_square(),
                        ),
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

    /// Loading follows clipped thumbnail bounds, so even one message containing
    /// hundreds of images cannot evict its first images before they are drawn.
    fn thumbnail(
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
                .child(self.thumbnail(&key, &image, gpui::ObjectFit::Contain, look, cx))
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
                div()
                    .size(ui_text::space(24.0))
                    .child(self.thumbnail(cache_key, image, gpui::ObjectFit::Cover, look, cx))
                    .into_any_element()
            })
            .unwrap_or_else(|| {
                crate::icons::symbol(
                    if is_image { "photo" } else { "doc" },
                    11.0,
                    Some(look.colors.muted),
                )
            });
        let image = image.cloned();
        let image_key = key.strip_prefix("artifact:").unwrap_or(&key).to_owned();
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
            if let Some(image) = &image {
                v.preview_image_reference(image, &image_key, Some(&owner), cx);
            } else {
                v.preview_reference(&target, Some(&owner), cx);
            }
        }))
        .into_any_element()
    }

    pub(super) fn render_artifact_refs(
        &self,
        refs: &[riwork_engine::chat::projection::ArtifactRef],
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(ui_text::space(4.0))
            .children(refs.iter().filter_map(|artifact| {
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
            }))
            .into_any_element()
    }

    pub(super) fn sent_attachment_strip(
        &self,
        item: &Item,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use crate::chat::attachments::AttachmentKind;
        let mut images: Vec<(String, ChatImage)> = item
            .presentation
            .attachments
            .iter()
            .enumerate()
            .filter_map(|(i, a)| {
                matches!(a.kind, AttachmentKind::Image { .. }).then(|| {
                    (
                        format!("attachment:{}:image:{i}", item.id),
                        ChatImage {
                            label: a.name.clone(),
                            source: ImageSource::Local {
                                path: a.path.to_string_lossy().into_owned(),
                            },
                        },
                    )
                })
            })
            .collect();
        for (i, image) in item.presentation.images.iter().enumerate() {
            if !images.iter().any(|(_, other)| other.source == image.source) {
                images.push((format!("{}:image:{i}", item.id), image.clone()));
            }
        }
        let files: Vec<_> = item
            .presentation
            .attachments
            .iter()
            .enumerate()
            .filter(|(_, a)| !matches!(a.kind, AttachmentKind::Image { .. }))
            .collect();
        if images.is_empty() && files.is_empty() {
            return div().into_any_element();
        }
        let image_count = images.len();
        let file_count = files.len();
        div()
            .mt(ui_text::space(8.0))
            .flex()
            .flex_col()
            .gap(ui_text::space(5.0))
            .child(
                div()
                    .text_size(ui_text::text(11.0))
                    .text_color(rgb(look.colors.muted))
                    .child(format!(
                        "{image_count} {}, {file_count} {}",
                        if image_count == 1 { "image" } else { "images" },
                        if file_count == 1 { "file" } else { "files" }
                    )),
            )
            .child(
                div()
                    .max_w(ui_text::space(310.0))
                    .flex()
                    .flex_wrap()
                    .gap(ui_text::space(5.0))
                    .children(images.iter().map(|(key, image)| {
                        let image = image.clone();
                        let key = key.clone();
                        let owner = item.id.clone();
                        crate::behavior_controls::button_content(
                            SharedString::from(format!("thumb:{key}")),
                            format!("Preview {}", image_label(&image, Some(item))),
                            self.thumbnail(&key, &image, gpui::ObjectFit::Cover, look, cx),
                        )
                        .w(ui_text::space(if image_count == 2 { 150.0 } else { 100.0 }))
                        .aspect_square()
                        .border_1()
                        .border_color(rgb(look.colors.divider))
                        .rounded(ui_text::space(6.0))
                        .bg(rgb(look.colors.panel))
                        .overflow_hidden()
                        .on_click(cx.listener(move |v, _, _, cx| {
                            cx.stop_propagation();
                            v.preview_image_reference(&image, &key, Some(&owner), cx);
                        }))
                        .into_any_element()
                    })),
            )
            .child(div().flex().flex_wrap().gap(ui_text::space(4.0)).children(
                files.into_iter().map(|(i, a)| {
                    self.file_chip(
                        format!("attachment:{}:{i}", item.id),
                        a.name.clone(),
                        Some(a.bytes),
                        a.path.to_string_lossy().into_owned(),
                        item.id.clone(),
                        None,
                        look,
                        cx,
                    )
                }),
            ))
            .into_any_element()
    }
}

/// A collapsed header shows its first image; an open call shows every image.
/// Extra images and files still need their chips. Activity summaries do not use this filter.
pub(super) fn artifact_shown_inline(
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
