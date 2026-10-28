//! Composer attachments use the chat host's immutable snapshots for every source.
use std::{fs, io::Write, os::unix::fs::{DirBuilderExt, OpenOptionsExt}, path::Path};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use riwork_engine::chat::{attachments::{self, Attachment, Preview}, client::Client};
use super::{dto::AttachmentDto, service::ChatBackend};

pub fn stage_path(backend: &dyn ChatBackend, chat: &str, source: &str) -> Result<AttachmentDto, String> {
    let path = if let Some(url) = source.strip_prefix("file://") {
        if !url.starts_with('/') { return Err("invalid: choose a local file".into()); }
        std::path::PathBuf::from(super::links::percent_decode(url)?)
    } else { std::path::PathBuf::from(source) };
    if !path.is_absolute() || source.contains('\0') { return Err("invalid: choose a local file".into()); }
    let socket = backend.ensure()?;
    Client::connect(&socket).map_err(|error| format!("unavailable: {error}"))?
        .stage_attachment(chat, &path).map(AttachmentDto).map_err(|error| error.to_string())
}

pub fn stage_image(backend: &dyn ChatBackend, chat: &str, name: &str, encoded: &str) -> Result<AttachmentDto, String> {
    if encoded.len() > ((attachments::RAW_TIFF_BYTES + 2) / 3 * 4) as usize {
        return Err("Clipboard image exceeds 64 MiB".into());
    }
    let bytes = STANDARD.decode(encoded).map_err(|_| "invalid: clipboard image bytes".to_owned())?;
    let bytes = attachments::normalize_clipboard_image(&bytes)?;
    let directory = std::env::temp_dir().join(format!("riwork-chat-paste-{}", uuid::Uuid::new_v4()));
    fs::DirBuilder::new().mode(0o700).create(&directory).map_err(|error| error.to_string())?;
    let name = Path::new(name).file_name().and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && !name.chars().any(char::is_control)).unwrap_or("Clipboard image.png");
    let source = directory.join(name);
    let result = (|| {
        let mut file = fs::OpenOptions::new().create_new(true).write(true).mode(0o600)
            .custom_flags(libc::O_NOFOLLOW).open(&source).map_err(|error| error.to_string())?;
        file.write_all(&bytes).and_then(|()| file.sync_all()).map_err(|error| error.to_string())?;
        stage_path(backend, chat, &source.to_string_lossy())
    })();
    let _ = fs::remove_dir_all(directory);
    result
}

pub fn preview(backend: &dyn ChatBackend, chat: &str, attachment: &Attachment) -> Result<Option<String>, String> {
    let directory = backend.home()?.join("chats").join(chat);
    super::drafts::validate_attachment(&directory, attachment)?;
    match &attachment.preview {
        Preview::Text { .. } => Ok(None),
        Preview::Image { path } => {
            let bytes = fs::read(path).map_err(|error| error.to_string())?;
            if bytes.len() > attachments::FILE_BYTES as usize { return Err("Attachment preview is too large".into()); }
            Ok(Some(format!("data:image/png;base64,{}", STANDARD.encode(bytes))))
        }
    }
}
