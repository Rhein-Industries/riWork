//! `chat-drafts/<chat id>`: the unsent text of each chat's message box, under the RiWork
//! data directory, so a draft outlives its tab being closed and the app restarting.
//! Switching tabs needs none of this: a hidden chat tab keeps its view and with it the
//! message box.
//!
//! A chat's file is replaced as its draft changes and removed once the draft is empty
//! (sent, or cleared by hand). Only a canonical UUID becomes a file name. Bounded like the
//! phone's drafts (`ios/Core/ChatDrafts.swift`): the newest 64 chats, 30 days since the last
//! change, 256 KB each; what is over the bounds is dropped when the drafts are loaded.
//!
//! The text alone is kept: attachments are staged files that a restart does not keep.
use gpui::{App, Global};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};
use uuid::Uuid;

pub const DIR_NAME: &str = "chat-drafts";
pub const MAX_DRAFTS: usize = 64;
pub const MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
pub const MAX_BYTES: usize = 256 * 1024;

/// The drafts of one RiWork data directory, as they are on disk.
pub struct Drafts {
    dir: PathBuf,
    texts: HashMap<String, String>,
}

impl Drafts {
    /// The drafts under `home`, dropping the ones over the bounds as of `now`.
    pub fn load(home: &Path, now: SystemTime) -> Self {
        let dir = home.join(DIR_NAME);
        let mut found = Vec::new();
        for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(metadata) = entry.metadata().ok().filter(|m| m.is_file()) else {
                continue;
            };
            let changed = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            let fresh = now.duration_since(changed).unwrap_or_default() <= MAX_AGE;
            let text = (is_chat_id(&name) && fresh && metadata.len() <= MAX_BYTES as u64)
                .then(|| read_text(&path))
                .flatten()
                .filter(|text| !text.is_empty());
            match text {
                Some(text) => found.push((changed, name, text)),
                // A leftover temporary file, a stale draft or one that is not text.
                None => {
                    let _ = fs::remove_file(&path);
                }
            }
        }
        found.sort_by(|a, b| b.0.cmp(&a.0));
        for (_, name, _) in found.drain(MAX_DRAFTS.min(found.len())..) {
            let _ = fs::remove_file(dir.join(name));
        }
        Self {
            texts: found.into_iter().map(|(_, id, text)| (id, text)).collect(),
            dir,
        }
    }

    pub fn get(&self, chat_id: &str) -> Option<&str> {
        self.texts.get(chat_id).map(String::as_str)
    }

    /// Keep `text` as the draft of `chat_id`, writing it only when it changed. An id that
    /// is not a UUID, or a draft over the size bound, is kept for this run only: an older
    /// copy on disk is removed rather than come back after a restart.
    pub fn set(&mut self, chat_id: &str, text: &str) -> Result<(), String> {
        if self
            .texts
            .get(chat_id)
            .map_or(text.is_empty(), |kept| kept == text)
        {
            return Ok(());
        }
        if text.is_empty() {
            self.texts.remove(chat_id);
        } else {
            self.texts.insert(chat_id.to_owned(), text.to_owned());
        }
        if !is_chat_id(chat_id) {
            return Ok(());
        }
        let path = self.dir.join(chat_id);
        if text.is_empty() || text.len() > MAX_BYTES {
            return match fs::remove_file(&path) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    Err(format!("Cannot remove {}: {error}", path.display()))
                }
                _ => Ok(()),
            };
        }
        crate::paths::create_private_dir(&self.dir)
            .map_err(|error| format!("Cannot create {}: {error}", self.dir.display()))?;
        write_private_file(&self.dir, &path, text.as_bytes())
    }
}

/// A canonical UUID, which is how the chat host names chats: nothing else becomes a path.
fn is_chat_id(id: &str) -> bool {
    Uuid::parse_str(id).is_ok_and(|uuid| uuid.to_string() == id)
}

fn read_text(path: &Path) -> Option<String> {
    let mut bytes = Vec::new();
    File::open(path)
        .ok()?
        .take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= MAX_BYTES).then_some(())?;
    String::from_utf8(bytes).ok()
}

/// A new private (0600) file renamed into place, so a crash leaves the old draft or the
/// whole new one.
fn write_private_file(dir: &Path, path: &Path, data: &[u8]) -> Result<(), String> {
    let temporary = dir.join(format!(".draft-{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|error| format!("Cannot create {}: {error}", temporary.display()))?;
        file.write_all(data)
            .map_err(|error| format!("Cannot write the chat draft: {error}"))?;
        fs::rename(&temporary, path)
            .map_err(|error| format!("Cannot replace {}: {error}", path.display()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// The app's drafts. Without it (a test that sets none) a chat keeps no draft past its view.
pub struct ChatDrafts(pub Drafts);
impl Global for ChatDrafts {}

pub fn init(home: &Path, cx: &mut App) {
    cx.set_global(ChatDrafts(Drafts::load(home, SystemTime::now())));
}

pub fn draft(chat_id: &str, cx: &App) -> Option<String> {
    cx.try_global::<ChatDrafts>()?
        .0
        .get(chat_id)
        .map(str::to_owned)
}

pub fn remember(chat_id: &str, text: &str, cx: &mut App) {
    if !cx.has_global::<ChatDrafts>() {
        return;
    }
    if let Err(error) = cx.global_mut::<ChatDrafts>().0.set(chat_id, text) {
        eprintln!("riwork: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> PathBuf {
        let path = std::env::temp_dir().join(format!("riwork-drafts-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn id() -> String {
        Uuid::new_v4().to_string()
    }

    #[test]
    fn a_draft_survives_a_reload_in_a_private_file_and_goes_when_emptied() {
        let home = home();
        let (a, b) = (id(), id());
        let mut drafts = Drafts::load(&home, SystemTime::now());
        drafts.set(&a, "half a thought 🦀\n  ").unwrap();
        drafts.set(&b, "another chat").unwrap();
        let reloaded = Drafts::load(&home, SystemTime::now());
        assert_eq!(reloaded.get(&a), Some("half a thought 🦀\n  "));
        assert_eq!(reloaded.get(&b), Some("another chat"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(home.join(DIR_NAME).join(&a))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        drafts.set(&a, "").unwrap();
        assert!(!home.join(DIR_NAME).join(&a).exists());
        let reloaded = Drafts::load(&home, SystemTime::now());
        assert_eq!(
            (reloaded.get(&a), reloaded.get(&b)),
            (None, Some("another chat"))
        );
        let leftovers = fs::read_dir(home.join(DIR_NAME))
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn only_a_uuid_becomes_a_file_and_an_oversized_draft_stays_in_memory() {
        let home = home();
        let mut drafts = Drafts::load(&home, SystemTime::now());
        drafts.set("../escape", "kept for this run").unwrap();
        assert_eq!(drafts.get("../escape"), Some("kept for this run"));
        assert!(!home.join("escape").exists());
        let a = id();
        drafts.set(&a, "older").unwrap();
        let big = "x".repeat(MAX_BYTES + 1);
        drafts.set(&a, &big).unwrap();
        assert_eq!(drafts.get(&a), Some(big.as_str()));
        // The older copy must not come back after a restart.
        assert_eq!(Drafts::load(&home, SystemTime::now()).get(&a), None);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn loading_drops_stale_and_surplus_drafts_and_stray_files() {
        let home = home();
        let mut drafts = Drafts::load(&home, SystemTime::now());
        let ids: Vec<String> = (0..MAX_DRAFTS + 2).map(|_| id()).collect();
        for (index, id) in ids.iter().enumerate() {
            drafts.set(id, "draft").unwrap();
            // Oldest first, a second apart.
            let at = SystemTime::now() - Duration::from_secs((ids.len() - index) as u64);
            File::options()
                .write(true)
                .open(home.join(DIR_NAME).join(id))
                .unwrap()
                .set_modified(at)
                .unwrap();
        }
        fs::write(home.join(DIR_NAME).join(".draft-x.tmp"), "partial").unwrap();
        let reloaded = Drafts::load(&home, SystemTime::now());
        assert_eq!(reloaded.texts.len(), MAX_DRAFTS);
        assert_eq!(reloaded.get(&ids[0]), None);
        assert_eq!(reloaded.get(&ids[1]), None);
        assert_eq!(reloaded.get(&ids[2]), Some("draft"));
        assert_eq!(
            fs::read_dir(home.join(DIR_NAME)).unwrap().count(),
            MAX_DRAFTS
        );
        // A month later every one is stale.
        let later = SystemTime::now() + MAX_AGE + Duration::from_secs(60);
        assert!(Drafts::load(&home, later).texts.is_empty());
        assert_eq!(fs::read_dir(home.join(DIR_NAME)).unwrap().count(), 0);
        let _ = fs::remove_dir_all(home);
    }
}
