//! Bounded provider image retention. Never serialize raw bitmap JSON as tool text.
use super::model::{ChatImage, ImageSource, MessagePhase, Presentation};
use serde_json::Value;

// Incoming echoes must retain every accepted outbound raw-byte set after base64
// expansion. Still bounded below the provider's 16 MiB frame cap.
pub const IMAGE_BYTES: usize = (super::attachments::FILE_BYTES as usize).div_ceil(3) * 4;
pub const ITEM_IMAGE_BYTES: usize =
    (super::attachments::SEND_BYTES as usize).div_ceil(3) * 4 + IMAGE_COUNT * 4;
pub const IMAGE_COUNT: usize = 8;

pub fn reference(label: &str, target: &str) -> ChatImage {
    let source = if let Some(data) = target.strip_prefix("data:") {
        match data.split_once(";base64,") {
            Some((mime, data)) => encoded(mime, data),
            None => unavailable("Unsupported image data URL"),
        }
    } else if target.starts_with("https://") || target.starts_with("http://") {
        ImageSource::Url {
            url: target.to_owned(),
        }
    } else {
        ImageSource::Local {
            path: target.to_owned(),
        }
    };
    ChatImage {
        label: label.chars().take(160).collect(),
        source,
    }
}
fn unavailable(reason: &str) -> ImageSource {
    ImageSource::Unavailable {
        reason: reason.into(),
    }
}
fn encoded(mime: &str, data: &str) -> ImageSource {
    if !matches!(
        mime,
        "image/png"
            | "image/jpeg"
            | "image/gif"
            | "image/webp"
            | "image/bmp"
            | "image/tiff"
            | "image/x-icon"
    ) {
        return unavailable("Unsupported image media type");
    }
    if data.len() > IMAGE_BYTES {
        return unavailable("Image exceeds the encoded attachment limit");
    }
    ImageSource::Data {
        mime: mime.into(),
        base64: data.into(),
    }
}

/// Known Codex app-server and Claude stream-json image shapes only.
pub fn presentation(value: &Value) -> Presentation {
    let phase = match value["phase"].as_str() {
        Some("final_answer" | "final") => Some(MessagePhase::Final),
        Some("commentary") => Some(MessagePhase::Commentary),
        _ => None,
    };
    let mut images = Vec::new();
    let mut bytes = 0;
    collect(value, 0, &mut images, &mut bytes);
    Presentation {
        phase,
        images,
        ..Default::default()
    }
}
fn collect(value: &Value, depth: usize, images: &mut Vec<ChatImage>, bytes: &mut usize) {
    if depth > 6 || images.len() >= IMAGE_COUNT {
        return;
    }
    if let Some(array) = value.as_array() {
        for part in array.iter().take(256) {
            collect(part, depth + 1, images, bytes);
        }
        return;
    }
    let kind = value["type"].as_str().unwrap_or_default();
    let source = match kind {
        "image" => {
            let source = value.get("source").unwrap_or(value);
            if let Some(data) = source["data"].as_str() {
                Some(encoded(
                    source["media_type"]
                        .as_str()
                        .or(value["mimeType"].as_str())
                        .unwrap_or("image/png"),
                    data,
                ))
            } else {
                source["url"]
                    .as_str()
                    .or(value["url"].as_str())
                    .map(|url| reference("Image", url).source)
            }
        }
        "inputImage" | "input_image" | "image_url" | "imageView" => value["path"]
            .as_str()
            .or(value["imageUrl"].as_str())
            .or(value["image_url"].as_str())
            .or(value["image_url"]["url"].as_str())
            .or(value["url"].as_str())
            .map(|target| reference("Image", target).source),
        "localImage" => value["path"]
            .as_str()
            .map(|path| reference("Image", path).source),
        "imageGeneration" => value["result"]
            .as_str()
            .filter(|data| !data.is_empty())
            .map(|data| {
                if data.starts_with("data:") {
                    reference("Generated image", data).source
                } else {
                    encoded("image/png", data)
                }
            })
            .or_else(|| {
                value["savedPath"]
                    .as_str()
                    .map(|path| reference("Generated image", path).source)
            }),
        _ => None,
    };
    if let Some(mut source) = source {
        if let ImageSource::Data { base64, .. } = &source {
            if *bytes + base64.len() > ITEM_IMAGE_BYTES {
                source = unavailable("Images exceed the encoded attachment item limit");
            } else {
                *bytes += base64.len();
            }
        }
        images.push(ChatImage {
            label: "Image".into(),
            source,
        });
        return;
    }
    for key in ["content", "contentItems", "result", "message"] {
        if let Some(child) = value.get(key) {
            collect(child, depth + 1, images, bytes);
        }
    }
}

/// Replace image payloads before serializing provider metadata as tool output/input.
pub fn without_payloads(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(without_payloads).collect()),
        Value::Object(fields) => {
            let image = matches!(value["type"].as_str(), Some("image" | "imageGeneration"));
            Value::Object(
                fields
                    .iter()
                    .map(|(key, child)| {
                        let replacement =
                            if image && matches!(key.as_str(), "data" | "source" | "result") {
                                Value::String("[image retained in presentation]".into())
                            } else {
                                without_payloads(child)
                            };
                        (key.clone(), replacement)
                    })
                    .collect(),
            )
        }
        _ => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn accepted_attachment_image_echoes_fit_encoded_retention_bounds() {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        let bytes = vec![0x5a; super::super::attachments::FILE_BYTES as usize];
        let encoded = STANDARD.encode(&bytes);
        assert!(
            encoded.len() > 4 << 20,
            "exercise the former encoded limit mismatch"
        );
        let echo = json!({"type":"userMessage", "content":[
            {"type":"image","url":format!("data:image/png;base64,{encoded}")},
            {"type":"image","url":format!("data:image/jpeg;base64,{encoded}")}
        ]});
        let kept = presentation(&echo);
        assert_eq!(kept.images.len(), 2);
        for image in kept.images {
            let ImageSource::Data { base64, .. } = image.source else {
                panic!("accepted image was lost");
            };
            assert_eq!(STANDARD.decode(base64).unwrap(), bytes);
        }
    }
    #[test]
    fn real_provider_content_is_retained_with_bounds_and_phase() {
        let codex = json!({"type":"mcpToolCall","result":{"content":[{"type":"image","mimeType":"image/png","data":"aGVsbG8="}]}});
        assert!(matches!(
            presentation(&codex).images[0].source,
            ImageSource::Data { .. }
        ));
        let claude = json!({"type":"tool_result","content":[{"type":"image","source":{"type":"base64","media_type":"image/jpeg","data":"aGVsbG8="}}]});
        assert!(
            matches!(presentation(&claude).images[0].source, ImageSource::Data { ref mime, .. } if mime == "image/jpeg")
        );
        assert!(!without_payloads(&claude).to_string().contains("aGVsbG8="));
        assert_eq!(
            presentation(&json!({"phase":"commentary"})).phase,
            Some(MessagePhase::Commentary)
        );
        let huge = json!({"type":"image","data":"a".repeat(IMAGE_BYTES+1),"mimeType":"image/png"});
        assert!(matches!(
            presentation(&huge).images[0].source,
            ImageSource::Unavailable { .. }
        ));
        assert_eq!(
            presentation(&json!({"content":vec![codex; 32]}))
                .images
                .len(),
            IMAGE_COUNT
        );
    }
}
