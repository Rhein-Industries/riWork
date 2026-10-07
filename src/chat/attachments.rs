//! Owned attachment snapshots. Provider inputs are built from bytes, never filenames alone.
//! No eviction: drafts, submitted turns and uncertain submissions retain their files until
//! chat deletion. Quotas bound this retention and refuse further staging explicitly.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::{ImageFormat, ImageReader, Limits};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::{Cursor, Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
use uuid::Uuid;

pub const FILE_BYTES: u64 = 4 << 20;
pub const TEXT_BYTES: usize = 1 << 20;
pub const SEND_BYTES: u64 = 8 << 20;
pub const SEND_COUNT: usize = 8;
pub const STORE_BYTES: u64 = 64 << 20;
pub const STORE_COUNT: usize = 128;
pub const TEXT_PREVIEW_CHARS: usize = 512;
pub const UNKNOWN_SUBMISSION: &str = "attachment_submission_unknown:";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AttachmentKind {
    Text,
    Image { mime: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Preview {
    Text { excerpt: String },
    Image { path: PathBuf },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    pub kind: AttachmentKind,
    pub bytes: u64,
    /// FNV-1a detects accidental stale edits; this is not an authentication boundary.
    pub fingerprint: String,
    pub preview: Preview,
}

fn fingerprint(bytes: &[u8]) -> String {
    let hash = bytes.iter().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
    });
    format!("{hash:016x}")
}
fn error(name: &str, reason: impl std::fmt::Display) -> String {
    format!("Attachment {name}: {reason}")
}

fn read(path: &Path, cap: u64) -> Result<Vec<u8>, String> {
    // Nonblocking also prevents a replaced FIFO from hanging the host.
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| e.to_string())?;
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("choose a regular file (no symlinks or directories)".into());
    }
    if meta.len() > cap {
        return Err(format!("exceeds the {cap} byte limit"));
    }
    let mut bytes = Vec::new();
    file.take(cap + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > cap {
        return Err(format!("exceeds the {cap} byte limit"));
    }
    Ok(bytes)
}
fn text(bytes: &[u8]) -> Result<&str, String> {
    if bytes.len() > TEXT_BYTES {
        return Err("text exceeds the 1 MiB limit".into());
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| "unsupported binary file; use UTF-8 text, PNG or JPEG")?;
    if text
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err("unsupported binary/control characters; use UTF-8 text, PNG or JPEG".into());
    }
    Ok(text)
}
fn decoded(bytes: &[u8]) -> Result<image::DynamicImage, String> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 << 20);
    let (width, height) = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .into_dimensions()
        .map_err(|e| e.to_string())?;
    if u64::from(width) * u64::from(height) > 16 * 1024 * 1024 {
        return Err("image exceeds 16 megapixels".into());
    }
    reader.limits(limits);
    reader
        .decode()
        .map_err(|e| format!("invalid or oversized image: {e}"))
}
fn static_image_mime(bytes: &[u8]) -> Result<&'static str, String> {
    if bytes.len() as u64 > FILE_BYTES {
        return Err("image exceeds 4 MiB".into());
    }
    let format = image::guess_format(bytes).map_err(|e| format!("invalid image: {e}"))?;
    let mime = match format {
        ImageFormat::Png => "image/png",
        ImageFormat::Jpeg => "image/jpeg",
        _ => return Err("unsupported image format; use static PNG or JPEG".into()),
    };
    // APNG animation would otherwise silently lose frames.
    if format == ImageFormat::Png
        && image::codecs::png::PngDecoder::new(Cursor::new(bytes))
            .map_err(|e| e.to_string())?
            .is_apng()
            .map_err(|e| e.to_string())?
    {
        return Err("animated PNG is unsupported; use a static image".into());
    }
    Ok(mime)
}

/// Pure bounded PNG thumbnail for clipboard previews and owned host previews.
/// Call off the UI thread. Uses the same static-format, APNG, size, dimension,
/// pixel and allocation admission as staged images; grants no staging authority.
pub fn image_thumbnail(bytes: &[u8]) -> Result<Vec<u8>, String> {
    static_image_mime(bytes)?;
    let mut thumbnail = Vec::new();
    decoded(bytes)?
        .thumbnail(256, 256)
        .write_to(&mut Cursor::new(&mut thumbnail), ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(thumbnail)
}

fn classify(name: &str, bytes: &[u8]) -> Result<AttachmentKind, String> {
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if image::guess_format(bytes).is_ok() {
        let mime = static_image_mime(bytes)?;
        decoded(bytes)?;
        return Ok(AttachmentKind::Image { mime: mime.into() });
    }
    if matches!(
        ext.as_str(),
        "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "webp"
            | "pdf"
            | "zip"
            | "gz"
            | "mp4"
            | "mp3"
            | "wav"
            | "heic"
            | "svg"
    ) {
        return Err("unsupported or corrupt file; use UTF-8 text, static PNG or JPEG".into());
    }
    text(bytes)?;
    Ok(AttachmentKind::Text)
}
fn private_dir(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())
        }
        Ok(_) => Err("attachment storage is not a real directory".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => fs::DirBuilder::new()
            .mode(0o700)
            .create(path)
            .map_err(|e| e.to_string()),
        Err(e) => Err(e.to_string()),
    }
}
fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| e.to_string())
}
fn usage(root: &Path) -> Result<(usize, u64), String> {
    let mut count = 0;
    let mut bytes = 0u64;
    for entry in fs::read_dir(root).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let meta = fs::symlink_metadata(entry.path()).map_err(|e| e.to_string())?;
        if !meta.is_dir() {
            return Err("unexpected attachment storage entry".into());
        }
        count += 1;
        for file in fs::read_dir(entry.path()).map_err(|e| e.to_string())? {
            let meta = fs::symlink_metadata(file.map_err(|e| e.to_string())?.path())
                .map_err(|e| e.to_string())?;
            if !meta.is_file() {
                return Err("unexpected attachment storage file".into());
            }
            bytes = bytes
                .checked_add(meta.len())
                .ok_or("attachment storage size overflow")?;
        }
    }
    Ok((count, bytes))
}

/// Call under the host's per-chat lock so concurrent staging cannot bypass quota.
pub fn stage(chat_dir: &Path, source: &Path) -> Result<Attachment, String> {
    let name = source
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("Attachment needs a UTF-8 filename")?
        .to_owned();
    if name.chars().any(char::is_control) || name.len() > 240 {
        return Err(error(&name, "unsupported filename"));
    }
    let run = || {
        let bytes = read(source, FILE_BYTES)?;
        let kind = classify(&name, &bytes)?;
        let root = chat_dir.join("attachments");
        private_dir(&root)?;
        let (count, used) = usage(&root)?;
        if count >= STORE_COUNT {
            return Err(format!(
                "chat attachment storage reached its {STORE_COUNT} file limit"
            ));
        }
        let id = Uuid::new_v4().to_string();
        let dir = root.join(&id);
        let path = dir.join("content");
        let mut thumbnail = Vec::new();
        let preview = match &kind {
            AttachmentKind::Text => Preview::Text {
                excerpt: text(&bytes)?.chars().take(TEXT_PREVIEW_CHARS).collect(),
            },
            AttachmentKind::Image { .. } => {
                thumbnail = image_thumbnail(&bytes)?;
                Preview::Image {
                    path: dir.join("preview.png"),
                }
            }
        };
        let attachment = Attachment {
            id,
            name: name.clone(),
            path,
            kind,
            bytes: bytes.len() as u64,
            fingerprint: fingerprint(&bytes),
            preview,
        };
        let meta = serde_json::to_vec(&attachment).map_err(|e| e.to_string())?;
        if used + bytes.len() as u64 + thumbnail.len() as u64 + meta.len() as u64 > STORE_BYTES {
            return Err("chat attachment storage exceeds 64 MiB; delete an unneeded chat to free its snapshots".into());
        }
        private_dir(&dir)?;
        let result = (|| {
            write(&attachment.path, &bytes)?;
            if let Preview::Image { path } = &attachment.preview {
                write(path, &thumbnail)?;
            }
            write(&dir.join("attachment.json"), &meta)?;
            Ok(attachment)
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&dir);
        }
        result
    };
    run().map_err(|e| error(&name, e))
}

pub fn validate_owned(chat_dir: &Path, attachments: &[Attachment]) -> Result<(), String> {
    check_set(attachments)?;
    let root = chat_dir.join("attachments");
    for a in attachments {
        let id = Uuid::parse_str(&a.id).map_err(|_| error(&a.name, "invalid snapshot id"))?;
        if id.to_string() != a.id {
            return Err(error(&a.name, "invalid snapshot id"));
        }
        let dir = root.join(&a.id);
        if a.path != dir.join("content") {
            return Err(error(&a.name, "snapshot belongs to another chat"));
        }
        for path in [&root, &dir] {
            if !fs::symlink_metadata(path).is_ok_and(|m| m.is_dir()) {
                return Err(error(&a.name, "missing or unsafe snapshot directory"));
            }
        }
        let meta = read(&dir.join("attachment.json"), 8192).map_err(|e| error(&a.name, e))?;
        let stored: Attachment = serde_json::from_slice(&meta).map_err(|e| error(&a.name, e))?;
        if &stored != a {
            return Err(error(
                &a.name,
                "snapshot metadata changed; attach the file again",
            ));
        }
        load(a)?;
    }
    Ok(())
}
fn check_set(attachments: &[Attachment]) -> Result<(), String> {
    if attachments.is_empty() || attachments.len() > SEND_COUNT {
        return Err(format!("send between 1 and {SEND_COUNT} attachments"));
    }
    let mut ids = HashSet::new();
    let mut size = 0u64;
    for a in attachments {
        if !ids.insert(&a.id) {
            return Err(error(&a.name, "duplicate attachment"));
        }
        if a.bytes > FILE_BYTES {
            return Err(error(&a.name, "exceeds 4 MiB"));
        }
        size += a.bytes;
    }
    if size > SEND_BYTES {
        return Err("attachments exceed the 8 MiB send limit".into());
    }
    Ok(())
}
fn load(a: &Attachment) -> Result<Vec<u8>, String> {
    let run = || -> Result<Vec<u8>, String> {
        let bytes = read(&a.path, FILE_BYTES)?;
        if bytes.len() as u64 != a.bytes || fingerprint(&bytes) != a.fingerprint {
            return Err("snapshot changed; attach the file again".into());
        }
        if classify(&a.name, &bytes)? != a.kind {
            return Err("snapshot content type changed".into());
        }
        Ok(bytes)
    };
    run().map_err(|e| error(&a.name, e))
}

/// Assemble all blocks before sending any bytes. Text file contents are explicit input,
/// not provider-native arbitrary-file objects (neither CLI promises those).
pub fn inputs(text: &str, attachments: &[Attachment], claude: bool) -> Result<Value, String> {
    check_set(attachments)?;
    if text.len() > TEXT_BYTES {
        return Err("attachment message text exceeds 1 MiB".into());
    }
    let mut blocks = Vec::new();
    if !text.is_empty() {
        blocks.push(json!({"type":"text","text":text}));
    }
    for a in attachments {
        let bytes = load(a)?;
        match &a.kind {
            AttachmentKind::Text => blocks.push(json!({"type":"text","text":format!("Attached file: {}\n{}", a.name, self::text(&bytes)?)})),
            AttachmentKind::Image { mime } => {
                blocks.push(json!({"type":"text","text":format!("Attached image: {}", a.name)}));
                let data = STANDARD.encode(&bytes);
                blocks.push(if claude { json!({"type":"image","source":{"type":"base64","media_type":mime,"data":data}}) }
                    else { json!({"type":"image","url":format!("data:{mime};base64,{data}")}) });
            }
        }
    }
    let input = Value::Array(blocks);
    if serde_json::to_vec(&input).map_err(|e| e.to_string())?.len() > 12 << 20 {
        return Err("encoded attachment input exceeds 12 MiB; attach fewer/smaller files".into());
    }
    Ok(input)
}

/// User-visible echo without inlining multi-megabyte file/image payloads in history.
pub fn summary(text: &str, attachments: &[Attachment]) -> String {
    let mut text = text.to_owned();
    for a in attachments {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&format!("[Attached: {} ({} bytes)]", a.name, a.bytes));
    }
    text
}

#[cfg(test)]
mod tests;
