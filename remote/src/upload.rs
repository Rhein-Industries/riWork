//! Files a phone sends to a shell or a chat of this desktop ("File upload extension" in
//! `docs/remote-protocol.md`): the storage behind `upload.begin`, `upload.chunk`,
//! `upload.finish`, `upload.cancel` and the paths `shell.paste` hands the CLI.
//!
//! - An upload arrives in chunks into `remote/uploads/DEVICE/UPLOAD.part` (mode 600, in the
//!   connector's mode-700 directory). How far it got is the length of that file, so a phone that
//!   lost its connection asks `upload.begin` again and carries on from there.
//! - `upload.finish` checks the size and the SHA-256 the phone announced and only then moves the
//!   file, as a whole, into the inbox of its shell or chat: `RIWORK_HOME/uploads/TARGET/NAME`
//!   (the desktop's `src/upload_inbox.rs` removes that folder when the shell is closed or the chat
//!   deleted). A file that arrived damaged is thrown away: all or nothing.
//! - `NAME` is made here: a tame form of the phone's name (letters, digits, `-` and `_`, at most
//!   40), eight random hex digits and the extension. The phone's name never becomes a path, and
//!   an existing file is never replaced.
//! - Each device has a ledger, `uploads-DEVICE.json` (mode 600), of its uploads, partial and
//!   complete. It bounds what a device may hold (`QUOTA_BYTES` in all, `MAX_ACTIVE` partial at
//!   once, `MAX_FILE_BYTES` a file) and is what the sweep and a revocation remove by.
//! - `sweep` removes a partial upload untouched for `IDLE_SECONDS`, a complete one after
//!   `KEEP_SECONDS`, everything of a device that is no longer paired, and files in the inboxes
//!   that no ledger knows once they are as old. The connector runs it when it starts, every hour
//!   and before every new upload; `forget_device` runs when a device is revoked.
//!
//! Nothing here opens, runs or logs what a file holds.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::config::{options, private_dir, private_read, private_write};

/// The largest file a phone may send.
pub const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
/// The most data one `upload.chunk` carries. Base64 makes 122 880 characters of it, which with
/// the rest of the request stays inside one encrypted frame (`MAX_PLAINTEXT`, 128 KiB).
pub const CHUNK_BYTES: usize = 92_160;
/// What one device may keep here at once, partial and complete uploads together.
pub const QUOTA_BYTES: u64 = 200 * 1024 * 1024;
/// Partial uploads of one device at once.
pub const MAX_ACTIVE: usize = 4;
/// Uploads one device's ledger remembers; the oldest complete ones go first.
pub const MAX_RECORDS: usize = 256;
/// How long a complete upload is kept.
pub const KEEP_SECONDS: u64 = 24 * 60 * 60;
/// How long a partial upload may wait for its next chunk.
pub const IDLE_SECONDS: u64 = 60 * 60;
/// Files one `shell.paste` takes (the CLI's `shell paste` takes as many).
pub const PASTE_MAX_FILES: usize = 16;
/// The folder in `RIWORK_HOME` that holds the inboxes; `src/upload_inbox.rs` names it too.
pub const INBOX: &str = "uploads";
pub const NAME_MAX_BYTES: usize = 255;
pub const TYPE_MAX_BYTES: usize = 127;
/// The most characters of the phone's name kept in the file's name.
const STEM_MAX: usize = 40;

/// What `ready` announces as `features.upload`.
pub fn features() -> Value {
    json!({
        "max_bytes": MAX_FILE_BYTES,
        "chunk_bytes": CHUNK_BYTES,
        "quota_bytes": QUOTA_BYTES,
        "max_files": PASTE_MAX_FILES
    })
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Why a request was refused: a protocol error code and a sentence for the phone.
#[derive(Debug, PartialEq, Eq)]
pub struct Refusal {
    pub code: &'static str,
    pub message: String,
}
fn refuse(code: &'static str, message: impl Into<String>) -> Refusal {
    Refusal {
        code,
        message: message.into(),
    }
}
fn invalid(message: impl Into<String>) -> Refusal {
    refuse("invalid_request", message)
}
/// The desktop's own trouble (a disk that is full, a file it cannot write).
fn failed(error: impl std::fmt::Display) -> Refusal {
    refuse(
        "cli_error",
        format!("the desktop could not store the file: {error}"),
    )
}

/// What an upload is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Shell,
    Chat,
}

/// A validated `upload.begin`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spec {
    pub upload: String,
    pub kind: Kind,
    pub target: String,
    pub name: String,
    pub media_type: Option<String>,
    pub size: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    upload: String,
    kind: Kind,
    target: String,
    /// The file's name in its inbox, made by `file_name`.
    file: String,
    size: u64,
    sha256: String,
    complete: bool,
    created_unix: u64,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    uploads: Vec<Record>,
}

/// Where an upload stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub upload: String,
    pub received: u64,
    /// The file in its inbox, once complete.
    pub path: Option<PathBuf>,
}
impl Status {
    pub fn json(&self) -> Value {
        let mut value = json!({
            "upload": self.upload,
            "status": if self.path.is_some() { "complete" } else { "partial" },
            "received": self.received,
        });
        if let Some(path) = &self.path {
            value["path"] = json!(path.to_string_lossy());
            if let Some(name) = path.file_name() {
                value["name"] = json!(name.to_string_lossy());
            }
        }
        value
    }
}

/// The uploads of every device of one connector. One lock for all: every step is a few file
/// operations, the hash of a finished file at most.
pub struct Uploads {
    /// The connector's private directory (`RIWORK_HOME/remote`).
    remote: PathBuf,
    /// `RIWORK_HOME`, whose `uploads` folder holds the inboxes.
    home: PathBuf,
    lock: Mutex<()>,
}

/// A 64-digit lowercase hex SHA-256.
pub fn valid_sha256(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A file name as the phone gave it: shown nowhere as a path, but bounded and printable.
pub fn valid_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.len() <= NAME_MAX_BYTES
        && !name
            .chars()
            .any(|c| c.is_control() || c == '\u{2028}' || c == '\u{2029}')
}

/// A media type (`image/jpeg`): `type/subtype` of RFC 6838's restricted names.
pub fn valid_media_type(text: &str) -> bool {
    let token = |part: &str| {
        !part.is_empty()
            && part.bytes().all(|b| {
                b.is_ascii_alphanumeric()
                    || matches!(
                        b,
                        b'!' | b'#' | b'$' | b'&' | b'^' | b'_' | b'.' | b'+' | b'-'
                    )
            })
    };
    text.len() <= TYPE_MAX_BYTES
        && text
            .split_once('/')
            .is_some_and(|(kind, subtype)| token(kind) && token(subtype))
}

/// The extension a media type stands for, for a name that has none.
fn extension_for(media_type: &str) -> Option<&'static str> {
    Some(match media_type.to_ascii_lowercase().as_str() {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/heic" => "heic",
        "image/heif" => "heif",
        "image/tiff" => "tiff",
        "application/pdf" => "pdf",
        "text/plain" => "txt",
        "application/json" => "json",
        "video/quicktime" => "mov",
        "video/mp4" => "mp4",
        "application/zip" => "zip",
        _ => return None,
    })
}

/// The stem and extension of the file a phone named `name`: only ASCII letters, digits, `-` and
/// `_` in the stem (at most `STEM_MAX`, "file" if nothing is left), a lowercase extension of
/// letters and digits (from `media_type` when the name has none). The caller adds a random part.
pub fn tame_name(name: &str, media_type: Option<&str>) -> (String, Option<String>) {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name).trim();
    let (stem, extension) = match base.rsplit_once('.') {
        Some((stem, extension)) if !stem.trim_matches('.').is_empty() => (stem, Some(extension)),
        _ => (base, None),
    };
    let mut tame = String::new();
    for c in stem.chars() {
        let c = if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            c
        } else {
            '-'
        };
        if !(c == '-' && tame.ends_with('-')) {
            tame.push(c);
        }
        if tame.len() >= STEM_MAX {
            break;
        }
    }
    let tame = tame.trim_matches(['-', '_']).to_owned();
    let tame = if tame.is_empty() {
        "file".to_owned()
    } else {
        tame
    };
    let extension = extension
        .filter(|e| (1..=10).contains(&e.len()) && e.bytes().all(|b| b.is_ascii_alphanumeric()))
        .map(str::to_ascii_lowercase)
        .or_else(|| media_type.and_then(extension_for).map(str::to_owned));
    (tame, extension)
}

fn file_name(stem: &str, extension: Option<&str>) -> String {
    let random: [u8; 4] = rand::random();
    match extension {
        Some(extension) => format!("{stem}-{}.{extension}", hex::encode(random)),
        None => format!("{stem}-{}", hex::encode(random)),
    }
}

/// A name this module made: never a path, never hidden.
fn plain_file_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn canonical_uuid(text: &str) -> bool {
    crate::crypto::uuid(text).is_ok()
}

impl Uploads {
    /// The uploads kept for the connector whose private directory is `remote`
    /// (`RIWORK_HOME/remote`); the inboxes are beside it.
    pub fn new(remote: PathBuf) -> Self {
        let home = remote
            .parent()
            .map_or_else(|| remote.clone(), Path::to_path_buf);
        Self {
            remote,
            home,
            lock: Mutex::new(()),
        }
    }
    fn guard(&self) -> std::sync::MutexGuard<'_, ()> {
        self.lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
    fn ledger_path(&self, device: &str) -> PathBuf {
        self.remote.join(format!("uploads-{device}.json"))
    }
    fn staging(&self, device: &str) -> PathBuf {
        self.remote.join("uploads").join(device)
    }
    fn part(&self, device: &str, upload: &str) -> PathBuf {
        self.staging(device).join(format!("{upload}.part"))
    }
    /// `RIWORK_HOME/uploads`.
    pub fn inbox_root(&self) -> PathBuf {
        self.home.join(INBOX)
    }
    fn inbox_file(&self, record: &Record) -> Option<PathBuf> {
        (canonical_uuid(&record.target) && plain_file_name(&record.file))
            .then(|| self.inbox_root().join(&record.target).join(&record.file))
    }
    fn read(&self, device: &str) -> Result<Ledger> {
        let path = self.ledger_path(device);
        if !path.exists() {
            return Ok(Ledger::default());
        }
        let mut ledger: Ledger = private_read(&path, 4 * 1024 * 1024)?;
        // Only what this module could have written counts.
        ledger.uploads.retain(|r| {
            canonical_uuid(&r.upload) && canonical_uuid(&r.target) && plain_file_name(&r.file)
        });
        Ok(ledger)
    }
    fn write(&self, device: &str, ledger: &Ledger) -> Result<()> {
        if ledger.uploads.is_empty() {
            match fs::remove_file(self.ledger_path(device)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                _ => return Ok(()),
            }
        }
        private_write(&self.ledger_path(device), ledger)
    }
    /// Remove an upload's data, partial or complete.
    fn discard(&self, device: &str, record: &Record) {
        let _ = fs::remove_file(self.part(device, &record.upload));
        if record.complete
            && let Some(file) = self.inbox_file(record)
        {
            let _ = fs::remove_file(file);
        }
    }
    fn status(&self, device: &str, record: &Record) -> Status {
        if record.complete {
            return Status {
                upload: record.upload.clone(),
                received: record.size,
                path: self.inbox_file(record),
            };
        }
        let received = fs::metadata(self.part(device, &record.upload)).map_or(0, |m| m.len());
        Status {
            upload: record.upload.clone(),
            received: received.min(record.size),
            path: None,
        }
    }
    /// Forget what has had its time in one device's ledger, and what is gone from disk.
    fn expire(&self, device: &str, ledger: &mut Ledger, now: u64) {
        let mut kept = Vec::with_capacity(ledger.uploads.len());
        for record in ledger.uploads.drain(..) {
            let alive = if record.complete {
                now.saturating_sub(record.created_unix) < KEEP_SECONDS
                    && self.inbox_file(&record).is_some_and(|f| f.is_file())
            } else {
                let touched = fs::metadata(self.part(device, &record.upload))
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs());
                touched.is_some_and(|t| now.saturating_sub(t) < IDLE_SECONDS)
            };
            if alive {
                kept.push(record);
            } else {
                self.discard(device, &record);
            }
        }
        ledger.uploads = kept;
    }

    /// Start an upload, or say where one with the same UUID stands.
    pub fn begin(&self, device: &str, spec: &Spec) -> std::result::Result<Status, Refusal> {
        self.begin_at(device, spec, now_unix())
    }
    pub fn begin_at(
        &self,
        device: &str,
        spec: &Spec,
        now: u64,
    ) -> std::result::Result<Status, Refusal> {
        if spec.size == 0 || spec.size > MAX_FILE_BYTES {
            return Err(refuse(
                "upload_limit",
                format!("a file may be 1 to {MAX_FILE_BYTES} bytes"),
            ));
        }
        let _guard = self.guard();
        let mut ledger = self.read(device).map_err(failed)?;
        if let Some(record) = ledger.uploads.iter().find(|r| r.upload == spec.upload) {
            if record.kind != spec.kind
                || record.target != spec.target
                || record.size != spec.size
                || record.sha256 != spec.sha256
            {
                return Err(invalid("this upload UUID was used for another file"));
            }
            let status = self.status(device, record);
            if record.complete && status.path.as_ref().is_none_or(|p| !p.is_file()) {
                return Err(refuse(
                    "not_found",
                    "the uploaded file is gone; send it again",
                ));
            }
            if !record.complete && !self.part(device, &record.upload).is_file() {
                // Its data went (a sweep): start it again under the same UUID.
                ledger.uploads.retain(|r| r.upload != spec.upload);
            } else {
                return Ok(status);
            }
        }
        self.expire(device, &mut ledger, now);
        if ledger.uploads.iter().filter(|r| !r.complete).count() >= MAX_ACTIVE {
            return Err(refuse(
                "upload_limit",
                format!("at most {MAX_ACTIVE} uploads may be under way at once"),
            ));
        }
        // Room for it: the oldest complete uploads go first, partial ones never.
        let used = |ledger: &Ledger| ledger.uploads.iter().map(|r| r.size).sum::<u64>();
        while used(&ledger) + spec.size > QUOTA_BYTES || ledger.uploads.len() >= MAX_RECORDS {
            let Some(oldest) = ledger.uploads.iter().position(|r| r.complete) else {
                break;
            };
            let record = ledger.uploads.remove(oldest);
            self.discard(device, &record);
        }
        if used(&ledger) + spec.size > QUOTA_BYTES || ledger.uploads.len() >= MAX_RECORDS {
            let _ = self.write(device, &ledger);
            return Err(refuse(
                "upload_limit",
                format!(
                    "this phone's uploads under way would pass {} MiB; wait for them or cancel one",
                    QUOTA_BYTES / (1024 * 1024)
                ),
            ));
        }
        let (stem, extension) = tame_name(&spec.name, spec.media_type.as_deref());
        let staging = self.staging(device);
        private_dir(&self.remote.join("uploads")).map_err(failed)?;
        private_dir(&staging).map_err(failed)?;
        let part = self.part(device, &spec.upload);
        let _ = fs::remove_file(&part);
        options()
            .write(true)
            .create_new(true)
            .open(&part)
            .map_err(failed)?;
        ledger.uploads.push(Record {
            upload: spec.upload.clone(),
            kind: spec.kind,
            target: spec.target.clone(),
            file: file_name(&stem, extension.as_deref()),
            size: spec.size,
            sha256: spec.sha256.clone(),
            complete: false,
            created_unix: now,
        });
        if let Err(error) = self.write(device, &ledger) {
            let _ = fs::remove_file(&part);
            return Err(failed(error));
        }
        Ok(Status {
            upload: spec.upload.clone(),
            received: 0,
            path: None,
        })
    }

    /// Add `data` at `offset`. A chunk the desktop has already (its answer was lost) changes
    /// nothing; one that overlaps the end adds only what is new.
    pub fn chunk(
        &self,
        device: &str,
        upload: &str,
        offset: u64,
        data: &[u8],
    ) -> std::result::Result<Status, Refusal> {
        let _guard = self.guard();
        let ledger = self.read(device).map_err(failed)?;
        let record = ledger
            .uploads
            .iter()
            .find(|r| r.upload == upload)
            .ok_or_else(|| refuse("not_found", "unknown upload; begin it again"))?;
        if record.complete {
            return Ok(self.status(device, record));
        }
        let part = self.part(device, upload);
        let have = fs::metadata(&part)
            .map_err(|_| refuse("not_found", "the upload expired; begin it again"))?
            .len();
        let end = offset + data.len() as u64;
        if offset > have {
            return Err(invalid(format!(
                "the desktop has {have} bytes of this upload; send from there"
            )));
        }
        if end > record.size {
            return Err(invalid("the chunk goes past the size the upload announced"));
        }
        if end > have {
            let mut file = options().append(true).open(&part).map_err(failed)?;
            file.write_all(&data[(have - offset) as usize..])
                .map_err(failed)?;
        }
        Ok(self.status(device, record))
    }

    /// Check a fully arrived upload against its SHA-256 and move it into its inbox. Answers the
    /// same for an upload that is complete already. A damaged one is removed.
    pub fn finish(&self, device: &str, upload: &str) -> std::result::Result<Status, Refusal> {
        let _guard = self.guard();
        let mut ledger = self.read(device).map_err(failed)?;
        let index = ledger
            .uploads
            .iter()
            .position(|r| r.upload == upload)
            .ok_or_else(|| refuse("not_found", "unknown upload; begin it again"))?;
        if ledger.uploads[index].complete {
            let status = self.status(device, &ledger.uploads[index]);
            return match &status.path {
                Some(path) if path.is_file() => Ok(status),
                _ => Err(refuse(
                    "not_found",
                    "the uploaded file is gone; send it again",
                )),
            };
        }
        let record = ledger.uploads[index].clone();
        let part = self.part(device, upload);
        let have = fs::metadata(&part)
            .map_err(|_| refuse("not_found", "the upload expired; begin it again"))?
            .len();
        if have != record.size {
            return Err(invalid(format!(
                "{have} of {} bytes arrived; send the rest first",
                record.size
            )));
        }
        if hash_file(&part).map_err(failed)? != record.sha256 {
            ledger.uploads.remove(index);
            self.discard(device, &record);
            let _ = self.write(device, &ledger);
            return Err(invalid(
                "the file arrived damaged (its SHA-256 differs) and was discarded; send it again",
            ));
        }
        let inbox = self.inbox_root().join(&record.target);
        private_dir(&self.inbox_root()).map_err(failed)?;
        private_dir(&inbox).map_err(failed)?;
        let (stem, extension) = match record.file.rsplit_once('.') {
            Some((stem, extension)) => {
                (stem.rsplit_once('-').map_or(stem, |s| s.0), Some(extension))
            }
            None => (
                record
                    .file
                    .rsplit_once('-')
                    .map_or(record.file.as_str(), |s| s.0),
                None,
            ),
        };
        let mut name = record.file.clone();
        let mut placed = None;
        for _ in 0..8 {
            let target = inbox.join(&name);
            // A hard link is made only where nothing is: an existing file is never replaced.
            match fs::hard_link(&part, &target) {
                Ok(()) => {
                    placed = Some(target);
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    name = file_name(stem, extension);
                }
                Err(_) => {
                    copy_new(&part, &target).map_err(failed)?;
                    placed = Some(target);
                    break;
                }
            }
        }
        let Some(placed) = placed else {
            return Err(failed("no free file name"));
        };
        let _ = fs::remove_file(&part);
        let record = &mut ledger.uploads[index];
        record.complete = true;
        record.file = name;
        let status = Status {
            upload: record.upload.clone(),
            received: record.size,
            path: Some(placed.clone()),
        };
        if let Err(error) = self.write(device, &ledger) {
            let _ = fs::remove_file(placed);
            return Err(failed(error));
        }
        Ok(status)
    }

    /// Drop a partial upload. A complete one is left to its inbox; an unknown one is fine.
    pub fn cancel(&self, device: &str, upload: &str) -> std::result::Result<Status, Refusal> {
        let _guard = self.guard();
        let mut ledger = self.read(device).map_err(failed)?;
        if let Some(index) = ledger.uploads.iter().position(|r| r.upload == upload) {
            if ledger.uploads[index].complete {
                return Ok(self.status(device, &ledger.uploads[index]));
            }
            let record = ledger.uploads.remove(index);
            self.discard(device, &record);
            self.write(device, &ledger).map_err(failed)?;
        }
        Ok(Status {
            upload: upload.to_owned(),
            received: 0,
            path: None,
        })
    }

    /// The files of complete uploads of this device for `kind` `target`, in the order asked.
    pub fn paths(
        &self,
        device: &str,
        kind: Kind,
        target: &str,
        uploads: &[String],
    ) -> std::result::Result<Vec<PathBuf>, Refusal> {
        let _guard = self.guard();
        let ledger = self.read(device).map_err(failed)?;
        uploads
            .iter()
            .map(|upload| {
                let record = ledger
                    .uploads
                    .iter()
                    .find(|r| &r.upload == upload)
                    .ok_or_else(|| refuse("not_found", format!("unknown upload {upload}")))?;
                if !record.complete {
                    return Err(invalid(format!("upload {upload} is not finished")));
                }
                if record.kind != kind || record.target != target {
                    return Err(invalid(format!(
                        "upload {upload} was sent for another target"
                    )));
                }
                self.inbox_file(record)
                    .filter(|path| path.is_file())
                    .ok_or_else(|| {
                        refuse(
                            "not_found",
                            format!("upload {upload} is gone; send it again"),
                        )
                    })
            })
            .collect()
    }

    /// Remove what has had its time, and everything of devices that `paired` says are not
    /// (revoked, or unknown). Best effort: what cannot be removed now is tried again next time.
    pub fn sweep(&self, paired: impl Fn(&str) -> bool) {
        self.sweep_at(paired, now_unix());
    }
    pub fn sweep_at(&self, paired: impl Fn(&str) -> bool, now: u64) {
        let _guard = self.guard();
        let mut devices = std::collections::BTreeSet::new();
        for entry in fs::read_dir(&self.remote).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(device) = name
                .strip_prefix("uploads-")
                .and_then(|n| n.strip_suffix(".json"))
            {
                devices.insert(device.to_owned());
            }
        }
        for entry in fs::read_dir(self.remote.join("uploads"))
            .into_iter()
            .flatten()
            .flatten()
        {
            devices.insert(entry.file_name().to_string_lossy().into_owned());
        }
        let mut known = std::collections::HashSet::new();
        for device in devices {
            if !canonical_uuid(&device) {
                continue;
            }
            if !paired(&device) {
                self.forget_locked(&device);
                continue;
            }
            let Ok(mut ledger) = self.read(&device) else {
                continue;
            };
            self.expire(&device, &mut ledger, now);
            let _ = self.write(&device, &ledger);
            // Staged data no record holds.
            let parts: std::collections::HashSet<String> = ledger
                .uploads
                .iter()
                .filter(|r| !r.complete)
                .map(|r| format!("{}.part", r.upload))
                .collect();
            for entry in fs::read_dir(self.staging(&device))
                .into_iter()
                .flatten()
                .flatten()
            {
                if !parts.contains(&*entry.file_name().to_string_lossy()) {
                    let _ = fs::remove_file(entry.path());
                }
            }
            let _ = fs::remove_dir(self.staging(&device));
            for record in ledger.uploads.iter().filter(|r| r.complete) {
                known.extend(self.inbox_file(record));
            }
        }
        // Files in the inboxes that no ledger holds (a ledger lost, a crash between placing a
        // file and writing it down) once they are as old as a kept upload, and empty inboxes.
        for inbox in fs::read_dir(self.inbox_root())
            .into_iter()
            .flatten()
            .flatten()
        {
            let path = inbox.path();
            if !canonical_uuid(&inbox.file_name().to_string_lossy())
                || !fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir())
            {
                continue;
            }
            for file in fs::read_dir(&path).into_iter().flatten().flatten() {
                let file = file.path();
                let old = fs::symlink_metadata(&file)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .is_some_and(|age| now.saturating_sub(age.as_secs()) >= KEEP_SECONDS);
                if old && !known.contains(&file) {
                    let _ = fs::remove_file(&file);
                }
            }
            let _ = fs::remove_dir(&path);
        }
    }

    /// Everything a revoked device sent: its partial and complete uploads and its ledger.
    pub fn forget_device(&self, device: &str) {
        if canonical_uuid(device) {
            let _guard = self.guard();
            self.forget_locked(device);
        }
    }
    fn forget_locked(&self, device: &str) {
        if let Ok(ledger) = self.read(device) {
            for record in &ledger.uploads {
                self.discard(device, record);
                if let Some(file) = self.inbox_file(record) {
                    let _ = file.parent().map(fs::remove_dir);
                }
            }
        }
        let _ = fs::remove_dir_all(self.staging(device));
        let _ = fs::remove_file(self.ledger_path(device));
    }
}

fn hash_file(path: &Path) -> Result<String> {
    let mut file = options().read(true).open(path).context("open the upload")?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// `from` copied to `to`, which must not exist yet (another file system than the staging).
fn copy_new(from: &Path, to: &Path) -> Result<()> {
    let mut source = options().read(true).open(from)?;
    let mut target = options().write(true).create_new(true).open(to)?;
    let copied = std::io::copy(&mut source, &mut target).and_then(|_| target.sync_all());
    if copied.is_err() {
        let _ = fs::remove_file(to);
    }
    Ok(copied?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_phone_name_never_becomes_a_path() {
        let cases = [
            ("IMG_0001.HEIC", None, ("IMG_0001", Some("heic"))),
            ("../../etc/passwd", None, ("passwd", None)),
            ("..", None, ("file", None)),
            (".bashrc", None, ("bashrc", None)),
            ("a b (1).png", None, ("a-b-1", Some("png"))),
            ("Bild ü.jpeg", None, ("Bild", Some("jpeg"))),
            ("C:\\x\\y.txt", None, ("y", Some("txt"))),
            ("photo", Some("image/png"), ("photo", Some("png"))),
            ("photo.", Some("image/jpeg"), ("photo", Some("jpg"))),
            ("x.tar.gz", None, ("x-tar", Some("gz"))),
            ("weird.ex$t", Some("text/plain"), ("weird", Some("txt"))),
            ("-rf .png", None, ("rf", Some("png"))),
            ("$(rm -rf ~)", None, ("rm-rf", None)),
        ];
        for (name, media_type, (stem, extension)) in cases {
            let (tame, ext) = tame_name(name, media_type);
            assert_eq!((tame.as_str(), ext.as_deref()), (stem, extension), "{name}");
            assert!(plain_file_name(&file_name(&tame, ext.as_deref())), "{name}");
        }
        let long = "x".repeat(300);
        assert_eq!(tame_name(&long, None).0.len(), STEM_MAX);
    }

    #[test]
    fn names_types_and_hashes_are_checked() {
        assert!(valid_name("IMG_0001.jpg"));
        assert!(!valid_name(""));
        assert!(!valid_name("  "));
        assert!(!valid_name("a\nb"));
        assert!(!valid_name(&"a".repeat(256)));
        assert!(valid_media_type("image/jpeg"));
        assert!(valid_media_type("application/vnd.ms-excel"));
        assert!(!valid_media_type("image"));
        assert!(!valid_media_type("image/"));
        assert!(!valid_media_type("image/jpeg; q=1"));
        assert!(valid_sha256(&"a".repeat(64)));
        assert!(!valid_sha256(&"A".repeat(64)));
        assert!(!valid_sha256(&"a".repeat(63)));
    }
}
