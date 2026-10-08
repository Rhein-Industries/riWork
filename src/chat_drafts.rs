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
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender},
    thread::{self, JoinHandle},
    time::{Duration, SystemTime},
};
use uuid::Uuid;

pub const DIR_NAME: &str = "chat-drafts";
pub const MAX_DRAFTS: usize = 64;
pub const MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
pub const MAX_BYTES: usize = 256 * 1024;

/// The drafts of one RiWork data directory, with writes queued off the UI thread.
pub struct Drafts {
    texts: HashMap<String, String>,
    /// Chats deleted in this run: a late write for one must not bring its draft back.
    deleted: HashSet<String>,
    writer: Option<Sender<WriteDraft>>,
    thread: Option<JoinHandle<()>>,
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
        let texts: HashMap<_, _> = found.into_iter().map(|(_, id, text)| (id, text)).collect();
        let persisted = texts.clone();
        let (writer, receiver) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("chat-drafts".into())
            .spawn(move || write_drafts(dir, persisted, receiver))
            .expect("start the chat draft writer");
        Self {
            texts,
            deleted: HashSet::new(),
            writer: Some(writer),
            thread: Some(thread),
        }
    }

    pub fn get(&self, chat_id: &str) -> Option<&str> {
        self.texts.get(chat_id).map(String::as_str)
    }

    /// Keep `text` as the draft of `chat_id` and queue it for the writer. An id that
    /// is not a UUID, or a draft over the size bound, is kept for this run only: an older
    /// copy on disk is removed rather than come back after a restart.
    pub fn set(&mut self, chat_id: &str, text: &str) -> Result<(), String> {
        if self.deleted.contains(chat_id) {
            return Ok(());
        }
        if text.is_empty() {
            self.texts.remove(chat_id);
        } else {
            self.texts.insert(chat_id.to_owned(), text.to_owned());
        }
        self.writer
            .as_ref()
            .ok_or_else(|| "The chat draft writer has stopped".to_owned())?
            .send(WriteDraft::Set(chat_id.to_owned(), text.to_owned()))
            .map_err(|error| format!("Cannot queue the chat draft: {error}"))
    }

    /// `sent` went out: the draft goes, unless it is no longer what was sent (another
    /// window of the chat has saved newer text since).
    pub fn clear_sent(&mut self, chat_id: &str, sent: &str) -> Result<(), String> {
        if self.texts.get(chat_id).is_some_and(|kept| kept != sent) {
            return Ok(());
        }
        self.set(chat_id, "")
    }

    /// The chat is gone: its draft goes, and stays gone for the rest of the run.
    pub fn forget(&mut self, chat_id: &str) -> Result<(), String> {
        let result = self.set(chat_id, "");
        self.deleted.insert(chat_id.to_owned());
        result
    }

    fn finish(&mut self) {
        self.writer.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }

    #[cfg(test)]
    pub fn flush(&self) {
        let (sender, receiver) = mpsc::channel();
        self.writer
            .as_ref()
            .unwrap()
            .send(WriteDraft::Flush(sender))
            .unwrap();
        receiver.recv().unwrap();
    }
}

impl Drop for Drafts {
    fn drop(&mut self) {
        self.finish();
    }
}

enum WriteDraft {
    Set(String, String),
    #[cfg(test)]
    Flush(Sender<()>),
}

/// The first wait before a failed write is tried again; it doubles up to `RETRY_MAX`.
const RETRY_FIRST: Duration = Duration::from_millis(250);
const RETRY_MAX: Duration = Duration::from_secs(30);
/// Tries left for what still fails when the app quits.
const RETRIES_AT_QUIT: u32 = 3;

fn write_drafts(
    dir: PathBuf,
    mut persisted: HashMap<String, String>,
    receiver: Receiver<WriteDraft>,
) {
    // What failed to be written, kept until it is written or replaced by newer text.
    let mut failed: HashMap<String, String> = HashMap::new();
    let mut wait = RETRY_FIRST;
    loop {
        let first = if failed.is_empty() {
            match receiver.recv() {
                Ok(message) => Some(message),
                Err(_) => break,
            }
        } else {
            match receiver.recv_timeout(wait) {
                Ok(message) => Some(message),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        };
        let mut pending = std::mem::take(&mut failed);
        #[cfg(test)]
        let mut flush = None;
        let mut next = first;
        while let Some(message) = next {
            match message {
                WriteDraft::Set(id, text) => {
                    pending.insert(id, text);
                }
                #[cfg(test)]
                WriteDraft::Flush(sender) => {
                    flush = Some(sender);
                    break;
                }
            }
            next = receiver.try_recv().ok();
        }
        failed = write_pending(&dir, &mut persisted, pending);
        wait = if failed.is_empty() {
            RETRY_FIRST
        } else {
            (wait * 2).min(RETRY_MAX)
        };
        #[cfg(test)]
        if let Some(sender) = flush {
            let _ = sender.send(());
        }
    }
    // The app quits: what still fails gets a few last tries.
    for _ in 0..RETRIES_AT_QUIT {
        if failed.is_empty() {
            break;
        }
        failed = write_pending(&dir, &mut persisted, failed);
        if !failed.is_empty() {
            thread::sleep(RETRY_FIRST);
        }
    }
}

/// Write each chat's newest text; what fails comes back, to be tried again.
fn write_pending(
    dir: &Path,
    persisted: &mut HashMap<String, String>,
    pending: HashMap<String, String>,
) -> HashMap<String, String> {
    let mut failed = HashMap::new();
    for (id, text) in pending {
        if !is_chat_id(&id) || persisted.get(&id).is_some_and(|kept| kept == &text) {
            continue;
        }
        match persist(dir, &id, &text) {
            Ok(()) => {
                if text.is_empty() {
                    persisted.remove(&id);
                } else {
                    persisted.insert(id, text);
                }
            }
            Err(error) => {
                eprintln!("riwork: {error}");
                failed.insert(id, text);
            }
        }
    }
    failed
}

fn persist(dir: &Path, chat_id: &str, text: &str) -> Result<(), String> {
    let path = dir.join(chat_id);
    if text.is_empty() || text.len() > MAX_BYTES {
        return match fs::remove_file(&path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                Err(format!("Cannot remove {}: {error}", path.display()))
            }
            _ => Ok(()),
        };
    }
    crate::paths::create_private_dir(dir)
        .map_err(|error| format!("Cannot create {}: {error}", dir.display()))?;
    write_private_file(dir, &path, text.as_bytes())
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
    cx.on_app_quit(|cx| {
        cx.global_mut::<ChatDrafts>().0.finish();
        async {}
    })
    .detach();
}

pub fn draft(chat_id: &str, cx: &App) -> Option<String> {
    cx.try_global::<ChatDrafts>()?
        .0
        .get(chat_id)
        .map(str::to_owned)
}

/// The chat's draft goes once `sent` went out, unless it has changed since.
pub fn clear_sent(chat_id: &str, sent: &str, cx: &mut App) {
    if !cx.has_global::<ChatDrafts>() {
        return;
    }
    if let Err(error) = cx.global_mut::<ChatDrafts>().0.clear_sent(chat_id, sent) {
        eprintln!("riwork: {error}");
    }
}

/// The chat was deleted: its draft goes for good.
pub fn forget(chat_id: &str, cx: &mut App) {
    if !cx.has_global::<ChatDrafts>() {
        return;
    }
    if let Err(error) = cx.global_mut::<ChatDrafts>().0.forget(chat_id) {
        eprintln!("riwork: {error}");
    }
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
        drafts.flush();
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
        drafts.flush();
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
        drafts.flush();
        // The older copy must not come back after a restart.
        assert_eq!(Drafts::load(&home, SystemTime::now()).get(&a), None);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn failed_writes_and_removals_retry_identical_text() {
        let home = home();
        let a = id();
        let dir = home.join(DIR_NAME);
        fs::write(&dir, "blocks the directory").unwrap();
        let mut drafts = Drafts::load(&home, SystemTime::now());
        drafts.set(&a, "retry me").unwrap();
        assert_eq!(drafts.get(&a), Some("retry me"));
        drafts.flush();
        fs::remove_file(&dir).unwrap();
        drafts.set(&a, "retry me").unwrap();
        drafts.flush();
        assert_eq!(fs::read_to_string(dir.join(&a)).unwrap(), "retry me");
        fs::remove_file(dir.join(&a)).unwrap();
        fs::create_dir(dir.join(&a)).unwrap();
        drafts.set(&a, "").unwrap();
        drafts.flush();
        assert_eq!(drafts.get(&a), None);
        fs::remove_dir(dir.join(&a)).unwrap();
        fs::write(dir.join(&a), "old").unwrap();
        drafts.set(&a, "").unwrap();
        drafts.flush();
        assert!(!dir.join(&a).exists());
        drop(drafts);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn dropping_the_writer_drains_the_latest_text_for_each_chat() {
        let home = home();
        let (a, b) = (id(), id());
        let mut drafts = Drafts::load(&home, SystemTime::now());
        for index in 0..1000 {
            drafts.set(&a, &index.to_string()).unwrap();
            drafts.set(&b, "another chat").unwrap();
        }
        assert_eq!(drafts.get(&a), Some("999"));
        drop(drafts);
        let reloaded = Drafts::load(&home, SystemTime::now());
        assert_eq!(reloaded.get(&a), Some("999"));
        assert_eq!(reloaded.get(&b), Some("another chat"));
        drop(reloaded);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn loading_drops_stale_and_surplus_drafts_and_stray_files() {
        let home = home();
        let mut drafts = Drafts::load(&home, SystemTime::now());
        let ids: Vec<String> = (0..MAX_DRAFTS + 2).map(|_| id()).collect();
        for (index, id) in ids.iter().enumerate() {
            drafts.set(id, "draft").unwrap();
            drafts.flush();
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
        drafts.flush();
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

    #[test]
    fn clearing_a_sent_draft_keeps_newer_text_and_a_deleted_chat_keeps_none() {
        let home = home();
        let (a, b) = (id(), id());
        let mut drafts = Drafts::load(&home, SystemTime::now());
        drafts.set(&a, "newer").unwrap();
        drafts.clear_sent(&a, "sent").unwrap();
        assert_eq!(drafts.get(&a), Some("newer"));
        drafts.clear_sent(&a, "newer").unwrap();
        assert_eq!(drafts.get(&a), None);
        drafts.set(&b, "gone with the chat").unwrap();
        drafts.forget(&b).unwrap();
        drafts.set(&b, "a late write").unwrap();
        assert_eq!(drafts.get(&b), None);
        drafts.flush();
        assert!(!home.join(DIR_NAME).join(&b).exists());
        drop(drafts);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn a_failed_write_is_tried_again_without_another_edit() {
        let home = home();
        let a = id();
        let dir = home.join(DIR_NAME);
        fs::write(&dir, "blocks the directory").unwrap();
        let mut drafts = Drafts::load(&home, SystemTime::now());
        drafts.set(&a, "written later").unwrap();
        drafts.flush();
        assert!(dir.is_file(), "the first write failed");
        fs::remove_file(&dir).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !dir.join(&a).exists() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(fs::read_to_string(dir.join(&a)).unwrap(), "written later");
        drop(drafts);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn what_still_fails_at_quit_gets_last_tries() {
        let home = home();
        let a = id();
        let dir = home.join(DIR_NAME);
        fs::write(&dir, "blocks the directory").unwrap();
        let mut drafts = Drafts::load(&home, SystemTime::now());
        drafts.set(&a, "saved at quit").unwrap();
        drafts.flush();
        let unblock = {
            let dir = dir.clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(100));
                fs::remove_file(&dir).unwrap();
            })
        };
        // Quitting before the next scheduled retry.
        drop(drafts);
        unblock.join().unwrap();
        assert_eq!(fs::read_to_string(dir.join(&a)).unwrap(), "saved at quit");
        let _ = fs::remove_dir_all(home);
    }
}
