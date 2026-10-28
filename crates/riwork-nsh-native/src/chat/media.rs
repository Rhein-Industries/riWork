//! Embedded images use bounded decoding; local previews retain the original chat scope.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use nsh_core::prelude::*;
use riwork_engine::{
    chat::model::{ChatImage, ChatInfo, ImageSource},
    file_preview::{self, PreviewContent},
    store::Store,
};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct ChatImagePreview {
    pub label: String,
    pub data_url: Option<String>,
    pub url: Option<String>,
    pub message: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

pub fn read(home: &Path, info: &ChatInfo, image: ChatImage) -> Result<ChatImagePreview, String> {
    let mut result = ChatImagePreview {
        label: image.label,
        data_url: None,
        url: None,
        message: None,
        width: None,
        height: None,
    };
    match image.source {
        ImageSource::Local { path } => {
            let state = Store::open(home.to_owned())?.snapshot()?;
            let scope = super::image_scope::ImageScope::for_chat(
                info,
                None,
                &state,
                home,
                Some(info.cwd.clone()),
            );
            let resolved = scope.resolve(&path)?;
            set_content(
                &mut result,
                file_preview::load(&resolved.root, &resolved.path, None)?,
            );
        }
        ImageSource::Data { mime: _, base64 } => {
            if base64.len() > 32 * 1024 * 1024 {
                return Err("Image exceeds the preview size limit".into());
            }
            let bytes = STANDARD
                .decode(base64)
                .map_err(|_| "Invalid image encoding")?;
            set_content(&mut result, file_preview::decode_image(&bytes)?);
        }
        ImageSource::Url { url } => {
            if !(url.starts_with("https://") || url.starts_with("http://"))
                || !riwork_engine::terminal_links::is_openable_url(&url)
            {
                return Err("Unsupported image link".into());
            }
            result.url = Some(url);
        }
        ImageSource::Unavailable { reason } => result.message = Some(reason),
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use riwork_engine::chat::{
        log::{self, ChatLog},
        model::{ChatEvent, Item, ItemBody, ItemStatus},
        projection::DisplayMode,
        wire::Envelope,
    };
    use serde_json::json;

    fn info(cwd: &Path) -> ChatInfo {
        serde_json::from_value(json!({"id":"11111111-2222-4333-8444-555555555555",
            "provider":"codex", "cwd":cwd, "title":"Synthetic screenshot", "created_at_unix":1}))
        .unwrap()
    }
    fn png() -> Vec<u8> {
        let mut output = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(12, 8, image::Rgba([200, 40, 10, 255]))
            .write_to(&mut output, image::ImageFormat::Png)
            .unwrap();
        output.into_inner()
    }
    fn decoded(preview: ChatImagePreview) -> image::RgbaImage {
        assert_eq!((preview.width, preview.height), (Some(12), Some(8)));
        let data = preview.data_url.unwrap();
        let bytes = STANDARD
            .decode(data.strip_prefix("data:image/png;base64,").unwrap())
            .unwrap();
        image::load_from_memory(&bytes).unwrap().into_rgba8()
    }

    #[test]
    fn persisted_cua_image_replays_as_an_inline_reference_and_decodable_pixels() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let info = info(home);
        let image = png();
        let item = Item {
            id: "cua-shot".into(),
            turn_id: Some("turn-1".into()),
            status: ItemStatus::Completed,
            presentation: riwork_engine::chat::media::presentation(&json!({"type":"mcpToolCall",
                "result":{"content":[{"type":"image", "mimeType":"image/png", "data":STANDARD.encode(&image)}]}})),
            started_at: None,
            ended_at: None,
            body: ItemBody::ToolCall {
                server: Some("cua-driver".into()),
                tool: "get_window_state".into(),
                input: json!({}),
                output: None,
            },
        };
        let events = [
            ChatEvent::Info { info: info.clone() },
            ChatEvent::ItemCompleted { item },
        ];
        let log = ChatLog::create(&log::chat_dir(home, &info.id).unwrap(), &info).unwrap();
        for (index, event) in events.into_iter().enumerate() {
            let envelope = Envelope {
                chat_id: info.id.clone(),
                seq: index as u64 + 1,
                event,
            };
            let mut line = serde_json::to_vec(&envelope).unwrap();
            line.push(b'\n');
            log.append(&line, true).unwrap();
        }
        drop(log);
        let replay = log::read_envelopes(&log::chat_dir(home, &info.id).unwrap()).unwrap();
        let mut view = super::super::view::ChatView::new(
            &info.id,
            DisplayMode::Normal,
            super::super::view::system_clock(),
        );
        view.apply(&replay);
        let delivery = view.reset();
        let artifact = delivery
            .rows
            .unwrap()
            .into_iter()
            .flat_map(|row| row.artifacts)
            .find(|artifact| artifact.item_id == "cua-shot")
            .unwrap();
        assert!(artifact.inline_image);
        assert_eq!(artifact.image_index, Some(0));
        // Headers carry only a reference; the native image call supplies the bytes.
        assert!(
            !serde_json::to_string(&artifact)
                .unwrap()
                .contains(&STANDARD.encode(&image))
        );
        let source = view.fold.transcript.items[0].presentation.images[0].clone();
        let pixels = decoded(read(home, &info, source).unwrap());
        assert!(pixels.pixels().all(|pixel| pixel.0 == [200, 40, 10, 255]));
    }

    #[test]
    fn embedded_images_reject_bad_encoding_corrupt_payloads_and_oversized_dimensions() {
        let temp = tempfile::tempdir().unwrap();
        for base64 in [
            "not-base64".into(),
            STANDARD.encode(&png()[..40]),
            "A".repeat(32 * 1024 * 1024 + 1),
        ] {
            assert!(
                read(
                    temp.path(),
                    &info(temp.path()),
                    ChatImage {
                        label: "Image".into(),
                        source: ImageSource::Data {
                            mime: "image/png".into(),
                            base64
                        }
                    }
                )
                .is_err()
            );
        }
        let mut huge = std::io::Cursor::new(Vec::new());
        image::RgbaImage::new(6001, 1)
            .write_to(&mut huge, image::ImageFormat::Png)
            .unwrap();
        let preview = read(
            temp.path(),
            &info(temp.path()),
            ChatImage {
                label: "Image".into(),
                source: ImageSource::Data {
                    mime: "image/png".into(),
                    base64: STANDARD.encode(huge.into_inner()),
                },
            },
        )
        .unwrap();
        assert!(preview.data_url.is_none());
        assert!(preview.message.unwrap().contains("too large"));
    }

    #[test]
    fn local_native_access_preserves_scope_missing_file_and_symlink_errors() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let root = temp.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let project = Store::open(&home)
            .unwrap()
            .add_project(&root, Some("Fixture"))
            .unwrap();
        let mut info = info(&root);
        info.project_id = Some(project.id);
        std::fs::write(root.join("shot.png"), png()).unwrap();
        let local = |path: &str| ChatImage {
            label: "Image".into(),
            source: ImageSource::Local { path: path.into() },
        };
        decoded(read(&home, &info, local("shot.png")).unwrap());
        assert!(read(&home, &info, local("missing.png")).is_err());
        assert!(read(&home, &info, local("../outside.png")).is_err());
        std::os::unix::fs::symlink(root.join("shot.png"), root.join("link.png")).unwrap();
        assert!(read(&home, &info, local("link.png")).is_err());
        let unavailable = read(
            &home,
            &info,
            ChatImage {
                label: "Image".into(),
                source: ImageSource::Unavailable {
                    reason: "Image was not retained".into(),
                },
            },
        )
        .unwrap();
        assert_eq!(
            unavailable.message.as_deref(),
            Some("Image was not retained")
        );
        assert!(unavailable.data_url.is_none());
    }
}

fn set_content(result: &mut ChatImagePreview, content: PreviewContent) {
    match content {
        PreviewContent::Image {
            bytes,
            mime,
            width,
            height,
        } => {
            result.data_url = Some(format!("data:{mime};base64,{}", STANDARD.encode(bytes)));
            result.width = Some(width);
            result.height = Some(height);
        }
        PreviewContent::Message(message) => result.message = Some(message),
        _ => result.message = Some("This artifact is not an image".into()),
    }
}
