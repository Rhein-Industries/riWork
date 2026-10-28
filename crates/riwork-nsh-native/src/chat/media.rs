//! Deliberate image preview uses the original chat identity and registered project scope.
use std::{io::Cursor, path::Path};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use nsh_core::prelude::*;
use serde::{Serialize, Deserialize};
use riwork_engine::{chat::model::{ChatImage, ChatInfo, ImageSource}, file_preview::{self, PreviewContent}, store::Store};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct ChatImagePreview { pub label: String, pub data_url: Option<String>, pub url: Option<String>, pub message: Option<String> }

pub fn read(home: &Path, info: &ChatInfo, image: ChatImage) -> Result<ChatImagePreview, String> {
    let mut result = ChatImagePreview { label: image.label, data_url: None, url: None, message: None };
    match image.source {
        ImageSource::Local { path } => {
            let state = Store::open(home.to_owned())?.snapshot()?;
            let scope = super::image_scope::ImageScope::for_chat(info, None, &state, home, Some(info.cwd.clone()));
            let resolved = scope.resolve(&path)?;
            match file_preview::load(&resolved.root, &resolved.path, None)? {
                PreviewContent::Image { bytes, mime, .. } => result.data_url = Some(format!("data:{mime};base64,{}", STANDARD.encode(bytes))),
                PreviewContent::Message(message) => result.message = Some(message),
                _ => result.message = Some("This artifact is not an image".into()),
            }
        }
        ImageSource::Data { mime: _, base64 } => {
            if base64.len() > 32 * 1024 * 1024 { return Err("Image exceeds the preview size limit".into()); }
            let bytes = STANDARD.decode(base64).map_err(|_| "Invalid image encoding")?;
            let format = image::guess_format(&bytes).map_err(|_| "Unsupported image format")?;
            let (width, height) = image::ImageReader::with_format(Cursor::new(&bytes), format).into_dimensions().map_err(|error| error.to_string())?;
            if width > 6000 || height > 6000 || u64::from(width) * u64::from(height) > 24_000_000 { return Err("Image exceeds the preview dimensions".into()); }
            result.data_url = Some(format!("data:{};base64,{}", format.to_mime_type(), STANDARD.encode(bytes)));
        }
        ImageSource::Url { url } => {
            if !riwork_engine::terminal_links::is_openable_url(&url) { return Err("Unsupported image link".into()); }
            result.url = Some(url);
        }
        ImageSource::Unavailable { reason } => result.message = Some(reason),
    }
    Ok(result)
}
