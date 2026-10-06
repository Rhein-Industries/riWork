//! User-triggered, bounded image loading. Folding uses stable message/image keys.
use super::{
    ChatView, ChatViewEvent,
    widgets::{Look, button},
};
use crate::{
    chat::model::{ChatImage, ImageSource, Item, ItemBody, MessagePhase},
    file_preview::{self, PreviewContent},
    ui_text,
};
use base64::Engine;
use gpui::{AnyElement, Context, SharedString, div, img, prelude::*, rgb};
use std::{
    collections::{HashMap, VecDeque},
    io::Read,
    sync::Arc,
    time::Duration,
};

const CONCURRENT: usize = 2;
const LOADED: usize = 8;
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
#[derive(Default)]
pub(super) struct MediaState {
    pub artifact_pages: HashMap<String, usize>,
    slots: HashMap<String, Slot>,
    queue: VecDeque<(String, ChatImage)>,
    active: usize,
    generation: u64,
    pub viewer: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

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

    fn context(body: ItemBody) -> Item {
        Item {
            id: "item".into(),
            turn_id: Some("turn".into()),
            status: crate::chat::model::ItemStatus::Completed,
            presentation: Default::default(),
            body,
        }
    }

    #[test]
    fn generic_image_labels_use_filenames_tool_context_and_deliverable_roles_without_payloads() {
        let inline = ChatImage {
            label: "Image".into(),
            source: ImageSource::Data {
                mime: "image/png".into(),
                base64: "SECRET_PAYLOAD".repeat(40),
            },
        };
        let user = context(ItemBody::UserMessage {
            text: "Upload".into(),
        });
        let mut final_answer = context(ItemBody::AgentMessage {
            text: "Result".into(),
        });
        final_answer.presentation.phase = Some(MessagePhase::Final);
        let mut tool = context(ItemBody::ToolCall {
            server: Some("cua".into()),
            tool: "mcp__cua_driver__get_window_state".into(),
            input: serde_json::json!({"pid": 123}),
            output: None,
        });
        assert_eq!(image_label(&inline, Some(&user)), "Uploaded image · PNG");
        assert_eq!(
            image_label(&inline, Some(&final_answer)),
            "Final answer image · PNG"
        );
        assert_eq!(
            image_label(&inline, Some(&tool)),
            "get window state image · PNG"
        );
        if let ItemBody::ToolCall { input, .. } = &mut tool.body {
            *input = serde_json::json!({"screenshot_path":"/tmp/Zażółć wide shot.png"});
        }
        assert_eq!(image_label(&inline, Some(&tool)), "Zażółć wide shot.png");
        let local = ChatImage {
            label: "Image".into(),
            source: ImageSource::Local {
                path: "docs/日本語 image.png".into(),
            },
        };
        assert_eq!(image_label(&local, None), "日本語 image.png");
        let url = ChatImage {
            label: "Image".into(),
            source: ImageSource::Url {
                url: "https://user:secret@example.test/screens/wide%20shot.png?token=private#x"
                    .into(),
            },
        };
        assert_eq!(image_label(&url, None), "wide shot.png");
        let host = ChatImage {
            label: "Image".into(),
            source: ImageSource::Url {
                url: "https://example.test/?secret=hidden".into(),
            },
        };
        assert_eq!(image_label(&host, None), "Image from example.test");
        for label in [
            "data:image/png;base64,SECRET_PAYLOAD".into(),
            "a".repeat(500),
        ] {
            let unsafe_label = ChatImage {
                label,
                ..inline.clone()
            };
            assert_eq!(
                image_label(&unsafe_label, Some(&user)),
                "Uploaded image · PNG"
            );
        }
        assert_eq!(
            image_label(
                &ChatImage {
                    label: "Architecture diagram".into(),
                    ..inline.clone()
                },
                None
            ),
            "Architecture diagram"
        );
        assert_eq!(inline.label, "Image");
        assert!(
            matches!(inline.source, ImageSource::Data { ref base64, .. } if base64.contains("SECRET_PAYLOAD"))
        );
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
        let root = std::path::Path::new("");
        let remote = ChatImage {
            label: "Image".into(),
            source: ImageSource::Url { url },
        };
        assert!(load(&remote, root, root).err().unwrap().contains("8 MiB"));
        server.join().unwrap();
        let invalid = ChatImage {
            label: "Image".into(),
            source: ImageSource::Data {
                mime: "image/png".into(),
                base64: "aGVsbG8=".into(),
            },
        };
        assert!(load(&invalid, root, root).is_err());
    }
}
struct Slot {
    generation: u64,
    source: ImageSource,
    content: Option<Result<PreviewContent, String>>,
}

fn load(
    image: &ChatImage,
    root: &std::path::Path,
    cwd: &std::path::Path,
) -> Result<PreviewContent, String> {
    match &image.source {
        ImageSource::Local { path } => {
            let (path, _) = super::links::resolve(root, cwd, path)?;
            match file_preview::load(root, &path, None)? {
                image @ PreviewContent::Image { .. } | image @ PreviewContent::Message(_) => {
                    Ok(image)
                }
                _ => Err("This reference is not a supported image.".into()),
            }
        }
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
fn release(slot: Slot, cx: &mut Context<ChatView>) {
    if let Some(Ok(content)) = slot.content
        && let Some(image) = content.render_image()
    {
        let image = image.clone();
        cx.defer(move |cx| cx.drop_image(image, None));
    }
}
impl ChatView {
    pub(super) fn release_images(&mut self, cx: &mut gpui::App) {
        for (_, slot) in self.media.slots.drain() {
            if let Some(Ok(content)) = slot.content
                && let Some(image) = content.render_image()
            {
                let image = image.clone();
                cx.defer(move |cx| cx.drop_image(image, None));
            }
        }
    }

    pub(super) fn image_card(
        &self,
        image: &ChatImage,
        key: &str,
        context: Option<&Item>,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.open.contains(key);
        let label = image_label(image, context);
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
                            div()
                                .id(SharedString::from(format!("view:{key}")))
                                .w_full()
                                .h(ui_text::space(240.0))
                                .flex_none()
                                .relative()
                                .overflow_hidden()
                                .child(
                                    img(image.clone())
                                        .absolute()
                                        .inset_0()
                                        .size_full()
                                        .aspect_square(),
                                )
                                .cursor_pointer()
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    view.media.viewer = Some(viewer_key.clone());
                                    cx.notify();
                                })),
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
        let local = if let ImageSource::Local { path } = &image.source {
            Some(path.clone())
        } else {
            None
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
    fn toggle_image(&mut self, key: String, image: ChatImage, cx: &mut Context<Self>) {
        let offset = self.list.logical_scroll_top();
        self.list.pause_following_tail();
        if !self.open.remove(&key) {
            if self.open.iter().filter(|id| id.contains("image")).count() >= 16 {
                self.notice = Some("Fold an image before opening more.".into());
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
            let info = self.model.transcript.info.as_ref();
            let cwd = info.map(|info| info.cwd.clone()).unwrap_or_default();
            // Provider local references are scoped to the chat's actual cwd, never the selected UI worktree.
            let root = self.preview_root.clone();
            let owned = image.clone();
            let work = cx.background_executor().spawn(async move {
                if matches!(owned.source, ImageSource::Local { .. }) && root.is_none() {
                    return Err(
                        "This chat’s worktree is unavailable for local image preview.".into(),
                    );
                }
                load(&owned, &root.unwrap_or_default(), &cwd)
            });
            self.media.active += 1;
            self.media.generation += 1;
            let generation = self.media.generation;
            let completion_key = key.clone();
            let source = image.source.clone();
            cx.spawn(async move |this, cx| {
                let content = work.await;
                let _ = this.update(cx, |view, cx| {
                    view.media.active = view.media.active.saturating_sub(1);
                    if let Some(slot) = view
                        .media
                        .slots
                        .get_mut(&completion_key)
                        .filter(|slot| slot.generation == generation && slot.source == source)
                    {
                        slot.content = Some(content);
                    } else if let Ok(content) = content
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
                },
            );
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
