//! A user-selected snippet export. The AppKit save sheet owns replacement
//! confirmation; disk work follows the original plan export's atomic writer.
use std::{
    fs::{self, File, Metadata, OpenOptions},
    io::Write,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::PathBuf,
};

pub const MAX_SNIPPET: usize = 256 * 1024;

#[derive(PartialEq, Eq)]
struct Identity(u64, u64, u64, i64, i64);
impl Identity {
    fn of(metadata: &Metadata) -> Self {
        Self(metadata.dev(), metadata.ino(), metadata.len(), metadata.mtime(), metadata.mtime_nsec())
    }
}

pub fn suggested_name(value: &str) -> String {
    let name: String = value.rsplit(['/', '\\']).next().unwrap_or("").chars()
        .filter(|character| !character.is_control() && *character != ':').take(100).collect();
    if name.is_empty() || name == "." || name == ".." { "snippet.txt".into() } else { name }
}

/// Called only after the Save sheet accepted its destination (and confirmed
/// any replacement). New files use create-only publication; an existing file
/// must retain its identity through writing the captured scratch copy.
pub fn write(path: PathBuf, text: &str) -> Result<String, String> {
    if text.len() > MAX_SNIPPET { return Err("Reader/export limit: 256 KiB".into()); }
    if !path.is_absolute() { return Err("Choose an absolute file destination".into()); }
    let name = path.file_name().ok_or("Choose a filename")?;
    let parent = path.parent().ok_or("Choose a destination folder")?.canonicalize()
        .map_err(|error| format!("Cannot open the destination folder: {error}"))?;
    let path = parent.join(name);
    let existing = match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() => Some(Identity::of(&metadata)),
        Ok(_) => return Err("The destination is not a regular file; choose another filename".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("Cannot inspect the destination: {error}")),
    };
    let temporary = parent.join(format!(".riwork-snippet-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temporary)
            .map_err(|error| format!("Cannot create the snippet: {error}"))?;
        file.write_all(text.as_bytes()).and_then(|_| file.sync_all())
            .map_err(|error| format!("Cannot write the snippet: {error}"))?;
        if let Some(expected) = existing {
            let current = fs::symlink_metadata(&path).map_err(|error| format!("The destination changed; choose it again: {error}"))?;
            if !current.is_file() || Identity::of(&current) != expected {
                return Err("The destination changed while saving; choose it again".into());
            }
            fs::set_permissions(&temporary, current.permissions()).map_err(|error| error.to_string())?;
            fs::rename(&temporary, &path).map_err(|error| format!("Cannot replace the snippet: {error}"))?;
        } else {
            fs::hard_link(&temporary, &path).map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    "A file now exists at this destination; choose it again to confirm replacement".into()
                } else { format!("Cannot save the snippet: {error}") }
            })?;
        }
        File::open(&parent).and_then(|file| file.sync_all())
            .map_err(|error| format!("Could not confirm the saved snippet on disk: {error}"))?;
        Ok(path.to_string_lossy().into_owned())
    })();
    let _ = fs::remove_file(temporary);
    result
}
