//! `chat-drafts/<chat id>`: each chat's unsent draft and unresolved submissions, under the RiWork
//! data directory, so a draft outlives its tab being closed and the app restarting.
//! Switching tabs needs none of this: a hidden chat tab keeps its view and with it the
//! message box.
//!
//! A chat's file is replaced as its draft changes and removed once its draft and saved
//! submissions are empty. Only a canonical UUID becomes a file name. Keep the newest 64
//! chats for 30 days, with 256 KiB of text per draft/submission and eight saved submissions.
//! Plain-text records from older builds remain readable.
//!
//! Ready attachments already belong to the host until chat deletion. Save their identities,
//! not source paths. Pending dispatches are separate recovery records, never unsent drafts.
//!
//! NSH port of the legacy `riwork-desktop/src/chat_drafts.rs`: the store and its on-disk
//! format are unchanged; the GPUI global is replaced by one process-wide store ([`init`])
//! shared by every window and engine.
use riwork_engine::chat::attachments::{self, Attachment, Preview, SEND_COUNT};
use serde::{Deserialize, Serialize};
use std::os::unix::fs::OpenOptionsExt;
use std::sync::mpsc;
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError},
    thread::{self, JoinHandle},
    time::{Duration, SystemTime},
};
use uuid::Uuid;

pub const DIR_NAME: &str = "chat-drafts";
pub const MAX_DRAFTS: usize = 64;
pub const MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
pub const MAX_BYTES: usize = 256 * 1024;
pub const MAX_SUBMISSIONS: usize = 8;
const MAX_RECORD_BYTES: usize = (MAX_BYTES + 64 * 1024) * (MAX_SUBMISSIONS + 1);
const RECORD_PREFIX: &str = "RIWORK_DRAFT_V1\n";

pub mod references;
pub use references::DraftReference;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contents {
    pub text: String,
    pub attachments: Vec<Attachment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<DraftReference>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubmissionStatus {
    Pending,
    Submitted,
    Refused(String),
    Uncertain(String),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedSubmission {
    pub key: String,
    pub snapshot: Contents,
    pub status: SubmissionStatus,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub draft: Contents,
    pub submissions: Vec<SavedSubmission>,
    #[serde(default)]
    pub unready_attachments: usize,
}
impl Record {
    fn empty(&self) -> bool {
        self.draft.text.is_empty()
            && self.draft.attachments.is_empty()
            && self.draft.references.is_empty()
            && self.submissions.is_empty()
            && self.unready_attachments == 0
    }
    fn encode(&self) -> Result<String, String> {
        if self.empty() {
            return Ok(String::new());
        }
        if self.draft.text.len() > MAX_BYTES {
            let mut saved = self.clone();
            saved.draft.text.clear();
            return saved.encode();
        }
        if self.draft.attachments.is_empty()
            && self.draft.references.is_empty()
            && self.submissions.is_empty()
            && self.unready_attachments == 0
            && !self.draft.text.starts_with(RECORD_PREFIX)
        {
            return Ok(self.draft.text.clone());
        }
        let encoded = serde_json::to_string(self)
            .map(|json| format!("{RECORD_PREFIX}{json}"))
            .map_err(|error| error.to_string())?;
        if encoded.len() > MAX_RECORD_BYTES {
            return Err("The saved submissions exceed the recovery storage limit; dismiss an older submission first".into());
        }
        Ok(encoded)
    }
    fn decode(text: String) -> Option<Self> {
        let record = if let Some(json) = text.strip_prefix(RECORD_PREFIX) {
            serde_json::from_str::<Self>(json).ok()?
        } else {
            Self {
                draft: Contents {
                    text,
                    attachments: Vec::new(),
                    references: Vec::new(),
                },
                submissions: Vec::new(),
                unready_attachments: 0,
            }
        };
        let valid = |draft: &Contents| {
            draft.text.len() <= MAX_BYTES
                && draft.attachments.len() <= SEND_COUNT
                && references::valid(&draft.references)
        };
        if !valid(&record.draft) || record.submissions.len() > MAX_SUBMISSIONS {
            return None;
        }
        let mut keys = HashSet::new();
        for saved in &record.submissions {
            if !valid(&saved.snapshot) || !is_chat_id(&saved.key) || !keys.insert(&saved.key) {
                return None;
            }
        }
        Some(record)
    }
}

/// Local editor identity plus monotonic edit generation. Equal text is not proof
/// that an acknowledgment still owns a draft another window edited meanwhile.
pub type Owner = (u64, u64);

/// The drafts of one RiWork data directory, with writes queued off the UI thread.
pub struct Drafts {
    records: HashMap<String, Record>,
    owners: HashMap<String, Owner>,
    home: PathBuf,
    /// Chats deleted in this run: a late write for one must not bring its draft back.
    deleted: HashSet<String>,
    queue: Arc<Queue>,
    thread: Option<JoinHandle<()>>,
}

/// What waits for the writer: the newest text of each chat, so however fast the drafts
/// change it holds one entry per chat, never a backlog.
#[derive(Default)]
struct Queue {
    state: Mutex<Waiting>,
    wake: Condvar,
}

#[derive(Default)]
struct Waiting {
    texts: HashMap<String, String>,
    closed: bool,
    acknowledgments: Vec<mpsc::Sender<Result<(), String>>>,
    #[cfg(test)]
    flushes: Vec<mpsc::Sender<()>>,
}

impl Queue {
    fn lock(&self) -> std::sync::MutexGuard<'_, Waiting> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Drafts {
    /// The drafts under `home`, dropping the ones over the bounds as of `now`.
    pub fn load(home: &Path, now: SystemTime) -> Self {
        let dir = home.join(DIR_NAME);
        let mut found = Vec::new();
        for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(metadata) = fs::symlink_metadata(&path).ok().filter(|m| m.is_file()) else {
                continue;
            };
            let changed = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            let fresh = now.duration_since(changed).unwrap_or_default() <= MAX_AGE;
            let record = (is_chat_id(&name) && fresh && metadata.len() <= MAX_RECORD_BYTES as u64)
                .then(|| read_text(&path))
                .flatten()
                .and_then(Record::decode)
                .filter(|record| !record.empty());
            match record {
                Some(record) => found.push((changed, name, record)),
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
        let records: HashMap<_, _> = found
            .into_iter()
            .map(|(_, id, record)| (id, record))
            .collect();
        let persisted = records
            .iter()
            .filter_map(|(id, record)| record.encode().ok().map(|encoded| (id.clone(), encoded)))
            .collect();
        let queue = Arc::new(Queue::default());
        let thread = {
            let queue = queue.clone();
            thread::Builder::new()
                .name("chat-drafts".into())
                .spawn(move || write_drafts(dir, persisted, &queue))
                .expect("start the chat draft writer")
        };
        Self {
            records,
            owners: HashMap::new(),
            home: home.to_owned(),
            deleted: HashSet::new(),
            queue,
            thread: Some(thread),
        }
    }

    /// The whole record, so a text-only editor keeps the attachments and
    /// references another build saved with it.
    pub fn record(&self, chat_id: &str) -> Option<&Record> {
        self.records.get(chat_id)
    }

    pub fn get(&self, chat_id: &str) -> Option<&str> {
        self.records
            .get(chat_id)
            .map(|record| record.draft.text.as_str())
            .filter(|text| !text.is_empty())
    }

    /// Keep `text` as the draft of `chat_id` and queue it for the writer. An id that
    /// is not a UUID, or a draft over the size bound, is kept for this run only: an older
    /// copy on disk is removed rather than come back after a restart.
    pub fn set(&mut self, chat_id: &str, text: &str) -> Result<(), String> {
        self.set_draft(
            chat_id,
            Contents {
                text: text.into(),
                attachments: Vec::new(),
                references: Vec::new(),
            },
            None,
        )
    }

    pub fn set_draft(
        &mut self,
        chat_id: &str,
        draft: Contents,
        owner: Option<Owner>,
    ) -> Result<(), String> {
        self.set_draft_with_unready(chat_id, draft, owner, 0)
    }

    pub fn set_draft_with_unready(
        &mut self,
        chat_id: &str,
        draft: Contents,
        owner: Option<Owner>,
        unready: usize,
    ) -> Result<(), String> {
        if self.deleted.contains(chat_id) {
            return Ok(());
        }
        if !references::valid(&draft.references) {
            return Err("The draft's source references exceed the recovery limits".into());
        }
        let record = self.records.entry(chat_id.into()).or_default();
        record.draft = draft;
        record.unready_attachments = unready.min(SEND_COUNT);
        if let Some(owner) = owner {
            self.owners.insert(chat_id.into(), owner);
        }
        self.enqueue(chat_id)
    }

    fn enqueue(&mut self, chat_id: &str) -> Result<(), String> {
        self.enqueue_with_barrier(chat_id, None)
    }

    fn enqueue_with_barrier(
        &mut self,
        chat_id: &str,
        acknowledgment: Option<mpsc::Sender<Result<(), String>>>,
    ) -> Result<(), String> {
        let record = self.records.get(chat_id).cloned().unwrap_or_default();
        if record.empty() {
            self.records.remove(chat_id);
        }
        let mut waiting = self.queue.lock();
        if waiting.closed {
            return Err("The chat draft writer has stopped".to_owned());
        }
        waiting.texts.insert(chat_id.to_owned(), record.encode()?);
        if let Some(acknowledgment) = acknowledgment {
            waiting.acknowledgments.push(acknowledgment);
        }
        drop(waiting);
        self.queue.wake.notify_one();
        Ok(())
    }

    /// `sent` went out: the draft goes, unless it is no longer what was sent (another
    /// window of the chat has saved newer text since).
    pub fn clear_sent(&mut self, chat_id: &str, sent: &str) -> Result<(), String> {
        if self.records.get(chat_id).is_some_and(|record| {
            !record.draft.references.is_empty() || !record.draft.attachments.is_empty()
        }) {
            // A legacy text-only receipt cannot acknowledge a later rich draft.
            return Ok(());
        }
        if self.get(chat_id).is_some_and(|kept| kept != sent) {
            return Ok(());
        }
        self.set(chat_id, "")
    }

    /// The chat is gone: its draft goes, and stays gone for the rest of the run.
    pub fn forget(&mut self, chat_id: &str) -> Result<(), String> {
        self.records.remove(chat_id);
        self.owners.remove(chat_id);
        let result = self.enqueue(chat_id);
        self.deleted.insert(chat_id.to_owned());
        result
    }

    pub fn begin_submission(
        &mut self,
        chat_id: &str,
        owner: Owner,
        saved: SavedSubmission,
    ) -> Result<mpsc::Receiver<Result<(), String>>, String> {
        if !is_chat_id(chat_id) {
            return Err("This chat has no valid recovery identity".into());
        }
        if self.deleted.contains(chat_id) {
            return Err("This chat was deleted".into());
        }
        let before = self.records.get(chat_id).cloned();
        let record = self.records.entry(chat_id.into()).or_default();
        if record.submissions.len() >= MAX_SUBMISSIONS {
            return Err("Dismiss an older saved submission before sending another message".into());
        }
        if saved.snapshot.text.len() > MAX_BYTES {
            return Err("The draft exceeds the 256 KiB recovery limit".into());
        }
        if !references::valid(&saved.snapshot.references) {
            return Err("The source references exceed the recovery limits".into());
        }
        if self.owners.get(chat_id) == Some(&owner)
            || (self.owners.get(chat_id).is_none() && record.draft == saved.snapshot)
        {
            record.draft = Contents::default();
        }
        record.submissions.push(saved);
        // Queue the record and its barrier atomically, so this acknowledgment
        // always observes the write attempt that admitted this submission.
        let (sender, receiver) = mpsc::channel();
        if let Err(error) = self.enqueue_with_barrier(chat_id, Some(sender)) {
            match before {
                Some(record) => {
                    self.records.insert(chat_id.into(), record);
                }
                None => {
                    self.records.remove(chat_id);
                }
            }
            return Err(error);
        }
        // A provider must not see the command before its recovery snapshot is durable.
        Ok(receiver)
    }

    pub fn pending_submission(&self, chat_id: &str, key: &str) -> bool {
        !self.deleted.contains(chat_id)
            && self.records.get(chat_id).is_some_and(|record| {
                record
                    .submissions
                    .iter()
                    .any(|saved| saved.key == key && saved.status == SubmissionStatus::Pending)
            })
    }

    /// Explicit app reloads wait for the queued drafts before starting the replacement.
    /// This bounded wait is deliberately separate from the asynchronous send barrier.
    /// The process-wide [`flush_pending`] is the same wait without holding the store.
    #[cfg(test)]
    fn flush_pending(&self, timeout: Duration) -> Result<(), String> {
        wait_flush(self.request_flush()?, timeout)
    }

    /// The first half of [`Self::flush_pending`]: queue the barrier. The process-wide
    /// [`flush_pending`] waits for it without holding the store.
    fn request_flush(&self) -> Result<mpsc::Receiver<Result<(), String>>, String> {
        let (sender, receiver) = mpsc::channel();
        let mut waiting = self.queue.lock();
        if waiting.closed {
            return Err("The chat draft writer has stopped".into());
        }
        waiting.acknowledgments.push(sender);
        drop(waiting);
        self.queue.wake.notify_one();
        Ok(receiver)
    }

    pub fn settle_submission(
        &mut self,
        chat_id: &str,
        key: &str,
        status: SubmissionStatus,
    ) -> Result<(), String> {
        if self.deleted.contains(chat_id) {
            return Ok(());
        }
        let Some(record) = self.records.get_mut(chat_id) else {
            return Ok(());
        };
        if status == SubmissionStatus::Submitted {
            record.submissions.retain(|saved| saved.key != key);
        } else if let Some(saved) = record.submissions.iter_mut().find(|saved| saved.key == key) {
            if matches!(saved.status, SubmissionStatus::Pending) {
                saved.status = status;
            }
        }
        self.enqueue(chat_id)
    }

    pub fn dismiss_submission(&mut self, chat_id: &str, key: &str) -> Result<(), String> {
        if let Some(record) = self.records.get_mut(chat_id) {
            record.submissions.retain(|saved| saved.key != key);
        }
        self.enqueue(chat_id)
    }

    pub fn recover(&self, chat_id: &str) -> Option<(Record, Vec<String>)> {
        let mut record = self.records.get(chat_id)?.clone();
        let mut warnings = Vec::new();
        if record.unready_attachments > 0 {
            warnings.push(format!(
                "{} attachments had not finished staging. Attach them again before sending.",
                record.unready_attachments
            ));
        }
        // Recovery is metadata-only. The view restores non-sendable Checking chips
        // and validates host-owned bytes on its background executor. Keep the
        // identities durable even if the tab closes before validation finishes.
        for saved in &mut record.submissions {
            // Retain invalid submission attachment metadata for honest review, but
            // do not make it a Ready chip or allow a partial resend.
            if saved.status == SubmissionStatus::Pending {
                saved.status = SubmissionStatus::Uncertain(
                    "The view closed before delivery was confirmed".into(),
                );
            }
        }
        Some((record, warnings))
    }

    fn finish(&mut self) {
        if let Some(thread) = self.stop_writer() {
            let _ = thread.join();
        }
    }

    /// Close the queue and hand back the writer to join. [`shutdown`] joins it without
    /// holding the store, so the quit drain never blocks another thread's store call.
    fn stop_writer(&mut self) -> Option<JoinHandle<()>> {
        self.queue.lock().closed = true;
        self.queue.wake.notify_one();
        self.thread.take()
    }

    #[cfg(test)]
    pub fn flush(&self) {
        let (sender, receiver) = mpsc::channel();
        self.queue.lock().flushes.push(sender);
        self.queue.wake.notify_one();
        receiver.recv().unwrap();
    }
}

impl Drop for Drafts {
    fn drop(&mut self) {
        self.finish();
    }
}

fn wait_flush(
    receiver: mpsc::Receiver<Result<(), String>>,
    timeout: Duration,
) -> Result<(), String> {
    receiver
        .recv_timeout(timeout)
        .map_err(|_| "Timed out saving chat drafts; reload was cancelled".to_owned())?
}

/// The first wait before a failed write is tried again; it doubles up to `RETRY_MAX`.
const RETRY_FIRST: Duration = Duration::from_millis(250);
const RETRY_MAX: Duration = Duration::from_secs(30);
/// Tries left for what still fails when the app quits.
const RETRIES_AT_QUIT: u32 = 3;
/// The most failed writes kept for another try; past it the oldest is given up.
pub const MAX_FAILED: usize = MAX_DRAFTS;

fn write_drafts(dir: PathBuf, mut persisted: HashMap<String, String>, queue: &Queue) {
    // What failed to be written, oldest first, until it is written or newer text replaces it.
    let mut failed: Vec<(String, String)> = Vec::new();
    let mut wait = RETRY_FIRST;
    loop {
        let mut waiting = queue.lock();
        #[cfg(test)]
        let ready = |w: &Waiting| {
            !w.texts.is_empty()
                || w.closed
                || !w.flushes.is_empty()
                || !w.acknowledgments.is_empty()
        };
        #[cfg(not(test))]
        let ready = |w: &Waiting| !w.texts.is_empty() || w.closed || !w.acknowledgments.is_empty();
        if failed.is_empty() {
            while !ready(&waiting) {
                waiting = queue.wake.wait(waiting).unwrap_or_else(|e| e.into_inner());
            }
        } else if !ready(&waiting) {
            waiting = queue
                .wake
                .wait_timeout(waiting, wait)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        let newer = std::mem::take(&mut waiting.texts);
        let closed = waiting.closed;
        let acknowledgments = std::mem::take(&mut waiting.acknowledgments);
        #[cfg(test)]
        let flushes = std::mem::take(&mut waiting.flushes);
        drop(waiting);
        failed.retain(|(id, _)| !newer.contains_key(id));
        let pending = std::mem::take(&mut failed).into_iter().chain(newer);
        failed = write_pending(&dir, &mut persisted, pending);
        for acknowledgment in acknowledgments {
            let result = if failed.is_empty() {
                Ok(())
            } else {
                Err("Could not save the submission recovery snapshot; nothing was sent".into())
            };
            let _ = acknowledgment.send(result);
        }
        cap_failed(&mut failed);
        wait = if failed.is_empty() {
            RETRY_FIRST
        } else {
            (wait * 2).min(RETRY_MAX)
        };
        #[cfg(test)]
        for sender in flushes {
            let _ = sender.send(());
        }
        if closed {
            break;
        }
    }
    // The app quits: what still fails gets a few last tries.
    for _ in 0..RETRIES_AT_QUIT {
        if failed.is_empty() {
            break;
        }
        thread::sleep(RETRY_FIRST);
        failed = write_pending(
            &dir,
            &mut persisted,
            std::mem::take(&mut failed).into_iter(),
        );
    }
}

/// Keep at most `MAX_FAILED` failed writes, giving up the oldest.
fn cap_failed(failed: &mut Vec<(String, String)>) {
    if failed.len() > MAX_FAILED {
        for (id, _) in failed.drain(..failed.len() - MAX_FAILED) {
            eprintln!("riwork: gave up writing the draft of chat {id}: too many drafts failed");
        }
    }
}

/// Write each chat's newest text; what fails comes back, in order, to be tried again.
fn write_pending(
    dir: &Path,
    persisted: &mut HashMap<String, String>,
    pending: impl Iterator<Item = (String, String)>,
) -> Vec<(String, String)> {
    let mut failed = Vec::new();
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
                failed.push((id, text));
            }
        }
    }
    failed
}

fn persist(dir: &Path, chat_id: &str, text: &str) -> Result<(), String> {
    let path = dir.join(chat_id);
    if text.len() > MAX_RECORD_BYTES {
        return Err("Draft recovery record exceeds its storage limit".into());
    }
    if text.is_empty() {
        return match fs::remove_file(&path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                Err(format!("Cannot remove {}: {error}", path.display()))
            }
            _ => Ok(()),
        };
    }
    riwork_core::paths::create_private_dir(dir)
        .map_err(|error| format!("Cannot create {}: {error}", dir.display()))?;
    write_private_file(dir, &path, text.as_bytes())
}

/// A canonical UUID, which is how the chat host names chats: nothing else becomes a path.
fn is_chat_id(id: &str) -> bool {
    Uuid::parse_str(id).is_ok_and(|uuid| uuid.to_string() == id)
}

fn read_text(path: &Path) -> Option<String> {
    let mut bytes = Vec::new();
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .ok()?
        .take(MAX_RECORD_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= MAX_RECORD_BYTES).then_some(())?;
    String::from_utf8(bytes).ok()
}

pub fn validate_attachment(chat_dir: &Path, attachment: &Attachment) -> Result<(), String> {
    if !is_chat_id(&attachment.id) {
        return Err("Recovered attachment has an invalid identity".into());
    }
    if attachment.name.len() > 240 || attachment.name.chars().any(char::is_control) {
        return Err("Recovered attachment has an invalid name".into());
    }
    // Refuse a symlink at every owned directory before reading metadata/content.
    for path in [
        chat_dir.parent().unwrap_or(chat_dir).to_owned(),
        chat_dir.to_owned(),
        chat_dir.join("attachments"),
        chat_dir.join("attachments").join(&attachment.id),
    ] {
        if !fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir()) {
            return Err(format!(
                "Attachment {} is missing or unsafe; attach it again",
                attachment.name
            ));
        }
    }
    if let Preview::Image { path } = &attachment.preview {
        let expected = chat_dir
            .join("attachments")
            .join(&attachment.id)
            .join("preview.png");
        if path != &expected
            || !fs::symlink_metadata(path)
                .is_ok_and(|m| m.is_file() && m.len() <= attachments::FILE_BYTES)
        {
            return Err(format!(
                "Attachment {} has no safe preview; attach it again",
                attachment.name
            ));
        }
    }
    attachments::validate_owned(chat_dir, std::slice::from_ref(attachment))
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
        file.sync_all()
            .map_err(|error| format!("Cannot sync the chat draft: {error}"))?;
        fs::rename(&temporary, path)
            .map_err(|error| format!("Cannot replace {}: {error}", path.display()))?;
        File::open(dir)
            .and_then(|file| file.sync_all())
            .map_err(|error| format!("Cannot sync draft directory: {error}"))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// The process's drafts: one store shared by every NSH window and engine. Without it (a
/// process that never called [`init`]) a chat keeps no draft past its view.
struct Shared {
    home: PathBuf,
    drafts: Mutex<Drafts>,
}

static SHARED: OnceLock<Shared> = OnceLock::new();

fn store(shared: &Shared) -> MutexGuard<'_, Drafts> {
    shared.drafts.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Load the drafts under `home` once per process and register the host's blocking draft
/// flush (close, quit, reload). Calling it again with the same home does nothing; another
/// home is refused, since every window must share one store.
pub fn init(home: &Path) -> Result<(), String> {
    static INIT: Mutex<()> = Mutex::new(());
    let _init = INIT.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(shared) = SHARED.get() {
        return if shared.home == home {
            Ok(())
        } else {
            Err(format!(
                "Chat drafts are already loaded from {}",
                shared.home.display()
            ))
        };
    }
    let drafts = Drafts::load(home, SystemTime::now());
    let _ = SHARED.set(Shared {
        home: home.to_owned(),
        drafts: Mutex::new(drafts),
    });
    crate::host_flush::register_draft_flush(|| flush_pending(Duration::from_secs(5)));
    Ok(())
}

/// Run `f` on the process's store; `None` before [`init`]. `f` runs under the store lock:
/// it must not block (the writer thread does the disk IO).
pub fn with<R>(f: impl FnOnce(&mut Drafts) -> R) -> Option<R> {
    SHARED.get().map(|shared| f(&mut store(shared)))
}

pub fn draft(chat_id: &str) -> Option<String> {
    with(|drafts| drafts.get(chat_id).map(str::to_owned)).flatten()
}

pub fn recover(chat_id: &str) -> Option<(Record, Vec<String>)> {
    with(|drafts| drafts.recover(chat_id)).flatten()
}

pub fn remember_complete(chat_id: &str, contents: Contents, owner: Owner, unready: usize) {
    if let Some(Err(error)) =
        with(|drafts| drafts.set_draft_with_unready(chat_id, contents, Some(owner), unready))
    {
        eprintln!("riwork: {error}");
    }
}

pub fn owns(chat_id: &str, owner: Owner) -> bool {
    with(|drafts| {
        drafts
            .owners
            .get(chat_id)
            .is_none_or(|current| current == &owner)
    })
    .unwrap_or(true)
}

/// Admit `saved` and queue its durable write. A provider must not see the command before
/// the returned receiver yields `Ok(())` and [`pending_submission`] still holds. Before
/// [`init`] there is nothing to wait for (the GPUI helper's `Ok(None)`): the receiver
/// already holds `Ok(())`.
pub fn begin_submission(
    chat_id: &str,
    owner: Owner,
    saved: SavedSubmission,
) -> Result<mpsc::Receiver<Result<(), String>>, String> {
    match with(|drafts| drafts.begin_submission(chat_id, owner, saved)) {
        Some(result) => result,
        None => {
            let (sender, receiver) = mpsc::channel();
            let _ = sender.send(Ok(()));
            Ok(receiver)
        }
    }
}

pub fn pending_submission(chat_id: &str, key: &str) -> bool {
    with(|drafts| drafts.pending_submission(chat_id, key)).unwrap_or(true)
}

/// Explicit app reloads wait for the queued drafts, at most `timeout`. The wait holds no
/// store lock.
pub fn flush_pending(timeout: Duration) -> Result<(), String> {
    match with(|drafts| drafts.request_flush()) {
        Some(receiver) => wait_flush(receiver?, timeout),
        None => Ok(()),
    }
}

pub fn settle_submission(chat_id: &str, key: &str, status: SubmissionStatus) {
    if let Some(Err(error)) = with(|drafts| drafts.settle_submission(chat_id, key, status)) {
        eprintln!("riwork: {error}");
    }
}

pub fn dismiss_submission(chat_id: &str, key: &str) {
    if let Some(Err(error)) = with(|drafts| drafts.dismiss_submission(chat_id, key)) {
        eprintln!("riwork: {error}");
    }
}

/// Where a recovered attachment of `chat_id` must live; `None` before [`init`] (the
/// caller then fails closed: the recovery store is unavailable).
pub fn recovery_directory(chat_id: &str) -> Option<PathBuf> {
    if !is_chat_id(chat_id) {
        return None;
    }
    with(|drafts| drafts.home.join("chats").join(chat_id))
}

/// The chat's draft goes once `sent` went out, unless it has changed since.
pub fn clear_sent(chat_id: &str, sent: &str) {
    if let Some(Err(error)) = with(|drafts| drafts.clear_sent(chat_id, sent)) {
        eprintln!("riwork: {error}");
    }
}

/// The chat was deleted: its draft goes for good.
pub fn forget(chat_id: &str) {
    if let Some(Err(error)) = with(|drafts| drafts.forget(chat_id)) {
        eprintln!("riwork: {error}");
    }
}

pub fn remember(chat_id: &str, text: &str) {
    if let Some(Err(error)) = with(|drafts| drafts.set(chat_id, text)) {
        eprintln!("riwork: {error}");
    }
}

/// The app quits: the writer drains the newest record of every chat, gives what still
/// fails its last tries, and stops; this returns once it has. Later saves are refused
/// ("The chat draft writer has stopped"). A concurrent call waits for the same drain.
pub fn shutdown() {
    static DRAIN: Mutex<()> = Mutex::new(());
    let _drain = DRAIN.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(Some(thread)) = with(Drafts::stop_writer) {
        let _ = thread.join();
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

    fn saved(text: &str) -> SavedSubmission {
        SavedSubmission {
            key: id(),
            snapshot: Contents {
                text: text.into(),
                attachments: Vec::new(),
                references: Vec::new(),
            },
            status: SubmissionStatus::Pending,
        }
    }

    fn staged(home: &Path, chat: &str) -> Attachment {
        let dir = home.join("chats").join(chat);
        fs::create_dir_all(&dir).unwrap();
        let source = home.join("notes.txt");
        fs::write(&source, "A durable attachment 🦀").unwrap();
        attachments::stage(&dir, &source).unwrap()
    }

    #[test]
    fn ready_attachment_identity_survives_restart_and_changed_bytes_are_rejected() {
        let home = home();
        let chat = id();
        let attachment = staged(&home, &chat);
        let contents = Contents {
            text: "please review".into(),
            attachments: vec![attachment.clone()],
            references: Vec::new(),
        };
        let mut drafts = Drafts::load(&home, SystemTime::now());
        drafts
            .set_draft(&chat, contents.clone(), Some((1, 1)))
            .unwrap();
        drafts.flush_pending(Duration::from_secs(2)).unwrap();
        drop(drafts);
        let reloaded = Drafts::load(&home, SystemTime::now());
        let (record, warnings) = reloaded.recover(&chat).unwrap();
        assert_eq!(record.draft, contents);
        assert!(warnings.is_empty());
        fs::write(&attachment.path, "changed after staging").unwrap();
        let (record, warnings) = reloaded.recover(&chat).unwrap();
        assert_eq!(record.draft.attachments, vec![attachment.clone()]);
        assert_eq!(record.draft.text, "please review");
        assert!(
            warnings.is_empty(),
            "metadata recovery performs no file reads"
        );
        assert!(validate_attachment(&home.join("chats").join(&chat), &attachment).is_err());
        drop(reloaded);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn pending_submission_recovers_as_uncertain_and_receipt_preserves_new_owner() {
        let home = home();
        let chat = id();
        let mut snapshot = saved("same text");
        snapshot.snapshot.attachments.push(staged(&home, &chat));
        let mut drafts = Drafts::load(&home, SystemTime::now());
        drafts
            .set_draft(&chat, snapshot.snapshot.clone(), Some((1, 1)))
            .unwrap();
        drafts
            .begin_submission(&chat, (1, 1), snapshot.clone())
            .unwrap()
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        let reloaded = Drafts::load(&home, SystemTime::now());
        let (record, _) = reloaded.recover(&chat).unwrap();
        assert_eq!(record.draft, Contents::default());
        assert_eq!(record.submissions[0].snapshot, snapshot.snapshot);
        assert!(matches!(
            record.submissions[0].status,
            SubmissionStatus::Uncertain(_)
        ));
        drop(reloaded);
        // Identical text from another editor is nevertheless a distinct unsent draft.
        drafts
            .set_draft(&chat, snapshot.snapshot.clone(), Some((2, 1)))
            .unwrap();
        drafts
            .settle_submission(&chat, &snapshot.key, SubmissionStatus::Submitted)
            .unwrap();
        drafts.flush();
        drop(drafts);
        let reloaded = Drafts::load(&home, SystemTime::now());
        let (record, _) = reloaded.recover(&chat).unwrap();
        assert_eq!(record.draft, snapshot.snapshot);
        assert!(record.submissions.is_empty());
        drop(reloaded);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn oversized_current_text_cannot_delete_retained_submissions() {
        let home = home();
        let chat = id();
        let snapshot = saved("retained intent");
        let mut drafts = Drafts::load(&home, SystemTime::now());
        drafts
            .begin_submission(&chat, (1, 1), snapshot.clone())
            .unwrap()
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        drafts.set(&chat, &"x".repeat(MAX_BYTES + 1)).unwrap();
        drafts.flush();
        drop(drafts);
        let reloaded = Drafts::load(&home, SystemTime::now());
        let (record, _) = reloaded.recover(&chat).unwrap();
        assert!(record.draft.text.is_empty());
        assert_eq!(record.submissions[0].snapshot, snapshot.snapshot);
        drop(reloaded);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn escaped_record_overflow_refuses_admission_without_losing_prior_snapshot() {
        let home = home();
        let chat = id();
        let snapshot = saved(&"\0".repeat(MAX_BYTES));
        let mut drafts = Drafts::load(&home, SystemTime::now());
        drafts
            .begin_submission(&chat, (1, 1), snapshot)
            .unwrap()
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        let before = drafts.records.get(&chat).unwrap().clone();
        let path = home.join(DIR_NAME).join(&chat);
        let disk_before = fs::read(&path).unwrap();
        let error = drafts
            .begin_submission(&chat, (1, 2), saved(&"\0".repeat(MAX_BYTES)))
            .unwrap_err();
        assert!(error.contains("storage limit"));
        assert_eq!(drafts.records.get(&chat), Some(&before));
        drafts.flush();
        assert_eq!(fs::read(&path).unwrap(), disk_before);
        assert!(
            persist(
                &home.join(DIR_NAME),
                &chat,
                &"x".repeat(MAX_RECORD_BYTES + 1)
            )
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), disk_before);
        drop(drafts);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn write_failure_reports_not_durable_and_keeps_intent_for_explicit_recovery() {
        let home = home();
        let chat = id();
        let dir = home.join(DIR_NAME);
        fs::write(&dir, "blocks draft storage").unwrap();
        let snapshot = saved("never dispatched");
        let mut drafts = Drafts::load(&home, SystemTime::now());
        let receiver = drafts
            .begin_submission(&chat, (1, 1), snapshot.clone())
            .unwrap();
        assert!(
            receiver
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .is_err()
        );
        assert_eq!(
            drafts.records[&chat].submissions[0].snapshot,
            snapshot.snapshot
        );
        assert!(drafts.flush_pending(Duration::from_secs(2)).is_err());
        fs::remove_file(dir).unwrap();
        drafts
            .settle_submission(
                &chat,
                &snapshot.key,
                SubmissionStatus::Refused("nothing sent".into()),
            )
            .unwrap();
        drafts.flush_pending(Duration::from_secs(2)).unwrap();
        drop(drafts);
        let reloaded = Drafts::load(&home, SystemTime::now());
        let (record, _) = reloaded.recover(&chat).unwrap();
        assert!(matches!(
            record.submissions[0].status,
            SubmissionStatus::Refused(_)
        ));
        drop(reloaded);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn dismissal_and_chat_deletion_revoke_pending_dispatch_without_deleting_staged_files() {
        let home = home();
        let chat = id();
        let mut snapshot = saved("keep staged bytes");
        snapshot.snapshot.attachments.push(staged(&home, &chat));
        let path = snapshot.snapshot.attachments[0].path.clone();
        let mut drafts = Drafts::load(&home, SystemTime::now());
        drafts
            .begin_submission(&chat, (1, 1), snapshot.clone())
            .unwrap()
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        assert!(drafts.pending_submission(&chat, &snapshot.key));
        drafts.dismiss_submission(&chat, &snapshot.key).unwrap();
        assert!(!drafts.pending_submission(&chat, &snapshot.key));
        drafts.flush();
        assert!(!home.join(DIR_NAME).join(&chat).exists());
        assert!(path.is_file());
        drafts
            .begin_submission(&chat, (1, 1), snapshot.clone())
            .unwrap()
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        drafts.forget(&chat).unwrap();
        drafts
            .settle_submission(
                &chat,
                &snapshot.key,
                SubmissionStatus::Refused("late".into()),
            )
            .unwrap();
        drafts.flush();
        assert!(!drafts.pending_submission(&chat, &snapshot.key));
        assert!(!home.join(DIR_NAME).join(&chat).exists());
        drop(drafts);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn unfinished_staging_is_reported_without_restoring_arbitrary_sources() {
        let home = home();
        let chat = id();
        let mut drafts = Drafts::load(&home, SystemTime::now());
        drafts
            .set_draft_with_unready(&chat, Contents::default(), Some((1, 1)), 2)
            .unwrap();
        drafts.flush();
        drop(drafts);
        let reloaded = Drafts::load(&home, SystemTime::now());
        let (record, warnings) = reloaded.recover(&chat).unwrap();
        assert!(record.draft.attachments.is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("2 attachments had not finished staging"));
        drop(reloaded);
        fs::remove_dir_all(home).unwrap();
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
        assert_eq!(reloaded.records.len(), MAX_DRAFTS);
        assert_eq!(reloaded.get(&ids[0]), None);
        assert_eq!(reloaded.get(&ids[1]), None);
        assert_eq!(reloaded.get(&ids[2]), Some("draft"));
        assert_eq!(
            fs::read_dir(home.join(DIR_NAME)).unwrap().count(),
            MAX_DRAFTS
        );
        // A month later every one is stale.
        let later = SystemTime::now() + MAX_AGE + Duration::from_secs(60);
        assert!(Drafts::load(&home, later).records.is_empty());
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

    #[test]
    fn failed_writes_keep_only_the_newest_ones() {
        let mut failed: Vec<_> = (0..MAX_FAILED + 5)
            .map(|n| (format!("chat-{n}"), "text".to_owned()))
            .collect();
        cap_failed(&mut failed);
        assert_eq!(failed.len(), MAX_FAILED);
        assert_eq!(failed[0].0, "chat-5", "the oldest five are given up");
        assert_eq!(failed.last().unwrap().0, format!("chat-{}", MAX_FAILED + 4));
    }

    #[test]
    fn rapid_changes_wait_as_one_entry_per_chat() {
        let home = home();
        let (a, b) = (id(), id());
        let dir = home.join(DIR_NAME);
        // Blocked, so nothing is written while the changes pile up.
        fs::write(&dir, "blocks the directory").unwrap();
        let mut drafts = Drafts::load(&home, SystemTime::now());
        for n in 0..10_000 {
            drafts.set(&a, &n.to_string()).unwrap();
            drafts.set(&b, "b").unwrap();
            assert!(drafts.queue.lock().texts.len() <= 2);
        }
        drafts.flush();
        fs::remove_file(&dir).unwrap();
        drafts.set(&a, "last").unwrap();
        drafts.flush();
        assert_eq!(fs::read_to_string(dir.join(&a)).unwrap(), "last");
        drop(drafts);
        let _ = fs::remove_dir_all(home);
    }

    // Golden bytes: what the legacy GPUI build wrote, spelled out from its serde shapes.
    const GOLDEN_KEY: &str = "5e2c9a71-3b4d-4f8e-a1c6-7d9e0f2b3a45";

    fn contents(text: &str) -> Contents {
        Contents {
            text: text.into(),
            attachments: Vec::new(),
            references: Vec::new(),
        }
    }

    #[test]
    fn text_and_one_submission_encode_to_the_legacy_bytes() {
        let record = Record {
            draft: contents("Half a thought 🦀\n  \"quoted\""),
            submissions: vec![SavedSubmission {
                key: GOLDEN_KEY.into(),
                snapshot: contents("sent text"),
                status: SubmissionStatus::Pending,
            }],
            unready_attachments: 0,
        };
        let expected = concat!(
            "RIWORK_DRAFT_V1\n",
            r#"{"draft":{"text":"Half a thought 🦀\n  \"quoted\"","attachments":[]},"#,
            r#""submissions":[{"key":"5e2c9a71-3b4d-4f8e-a1c6-7d9e0f2b3a45","#,
            r#""snapshot":{"text":"sent text","attachments":[]},"status":"Pending"}],"#,
            r#""unready_attachments":0}"#,
        );
        assert_eq!(record.encode().unwrap(), expected);
        assert_eq!(Record::decode(expected.into()), Some(record.clone()));
        // Through the writer: another editor's draft stays beside the admitted submission.
        let home = home();
        let chat = id();
        let mut drafts = Drafts::load(&home, SystemTime::now());
        drafts
            .set_draft(&chat, record.draft.clone(), Some((1, 1)))
            .unwrap();
        drafts
            .begin_submission(&chat, (2, 1), record.submissions[0].clone())
            .unwrap()
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        assert_eq!(
            fs::read(home.join(DIR_NAME).join(&chat)).unwrap(),
            expected.as_bytes()
        );
        drop(drafts);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn a_text_only_draft_is_written_as_raw_text() {
        let home = home();
        let chat = id();
        let path = home.join(DIR_NAME).join(&chat);
        let mut drafts = Drafts::load(&home, SystemTime::now());
        drafts.set(&chat, "half a thought 🦀\n  ").unwrap();
        drafts.flush();
        assert_eq!(fs::read(&path).unwrap(), "half a thought 🦀\n  ".as_bytes());
        // Text that looks like a record is the one text-only draft saved as a record.
        let lookalike = format!("{RECORD_PREFIX}{{}}");
        drafts.set(&chat, &lookalike).unwrap();
        drafts.flush();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            concat!(
                "RIWORK_DRAFT_V1\n",
                r#"{"draft":{"text":"RIWORK_DRAFT_V1\n{}","attachments":[]},"#,
                r#""submissions":[],"unready_attachments":0}"#,
            )
        );
        assert_eq!(
            Drafts::load(&home, SystemTime::now()).get(&chat),
            Some(lookalike.as_str())
        );
        // A plain-text file of an older build is a draft.
        let legacy = id();
        fs::write(home.join(DIR_NAME).join(&legacy), "older build\n").unwrap();
        assert_eq!(
            Drafts::load(&home, SystemTime::now()).get(&legacy),
            Some("older build\n")
        );
        drop(drafts);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn rich_records_keep_the_legacy_serde_shape() {
        use references::{ReferenceFragment, ReferenceOrigin, ReferenceState, revision};
        use riwork_engine::chat::{attachments::AttachmentKind, model::ChangeKind};
        let attachment = Attachment {
            id: "a1b2c3d4-0000-4000-8000-000000000001".into(),
            name: "notes.txt".into(),
            path: PathBuf::from("/data/content"),
            kind: AttachmentKind::Text,
            bytes: 5,
            fingerprint: "a430d84680aabd0b".into(),
            preview: Preview::Text {
                excerpt: "hello".into(),
            },
        };
        let message = DraftReference {
            id: "c0ffee00-0000-4000-8000-000000000002".into(),
            chat: Some("0b4a8f0e-6a3c-4d38-9f43-5a8c2e1d7b90".into()),
            origin: ReferenceOrigin::Message {
                item: "answer".into(),
                turn: None,
                revision: revision("abc"),
            },
            original_origin: None,
            excerpt: "Original answer".into(),
            quote: "Original answer".into(),
            comment: "Reply 🦀".into(),
            fragments: Vec::new(),
            state: ReferenceState::Current,
        };
        let diff = DraftReference {
            id: "c0ffee00-0000-4000-8000-000000000003".into(),
            chat: None,
            origin: ReferenceOrigin::Diff {
                item: "edit".into(),
                turn: Some("turn".into()),
                change: 0,
                path: "src/main.rs".into(),
                kind: ChangeKind::Modify,
                revision: revision("abc"),
                first: 1,
                last: 3,
                old: vec![(1, 2)],
                new: vec![(1, 2), (600, 600)],
            },
            original_origin: None,
            excerpt: "-old\n+new".into(),
            quote: "+new".into(),
            comment: String::new(),
            fragments: vec![ReferenceFragment {
                key: "k".into(),
                start: 0,
                end: 4,
                text: "+new".into(),
            }],
            state: ReferenceState::Changed,
        };
        let record = Record {
            draft: Contents {
                text: "see attached".into(),
                attachments: vec![attachment.clone()],
                references: vec![message, diff],
            },
            submissions: vec![
                SavedSubmission {
                    key: GOLDEN_KEY.into(),
                    snapshot: Contents {
                        text: "first".into(),
                        attachments: vec![attachment],
                        references: Vec::new(),
                    },
                    status: SubmissionStatus::Refused("no".into()),
                },
                SavedSubmission {
                    key: "5e2c9a71-3b4d-4f8e-a1c6-7d9e0f2b3a46".into(),
                    snapshot: contents("second"),
                    status: SubmissionStatus::Uncertain("lost".into()),
                },
                SavedSubmission {
                    key: "5e2c9a71-3b4d-4f8e-a1c6-7d9e0f2b3a47".into(),
                    snapshot: contents("third"),
                    status: SubmissionStatus::Submitted,
                },
            ],
            unready_attachments: 2,
        };
        let attachment = concat!(
            r#"{"id":"a1b2c3d4-0000-4000-8000-000000000001","name":"notes.txt","#,
            r#""path":"/data/content","kind":{"kind":"text"},"bytes":5,"#,
            r#""fingerprint":"a430d84680aabd0b","preview":{"kind":"text","excerpt":"hello"}}"#,
        );
        let sha = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let expected = [
            "RIWORK_DRAFT_V1\n",
            r#"{"draft":{"text":"see attached","attachments":["#,
            attachment,
            r#"],"references":[{"id":"c0ffee00-0000-4000-8000-000000000002","#,
            r#""chat":"0b4a8f0e-6a3c-4d38-9f43-5a8c2e1d7b90","#,
            r#""origin":{"Message":{"item":"answer","turn":null,"revision":""#,
            sha,
            r#""}},"original_origin":null,"excerpt":"Original answer","#,
            r#""quote":"Original answer","comment":"Reply 🦀","fragments":[],"state":"Current"},"#,
            r#"{"id":"c0ffee00-0000-4000-8000-000000000003","chat":null,"#,
            r#""origin":{"Diff":{"item":"edit","turn":"turn","change":0,"path":"src/main.rs","#,
            r#""kind":"modify","revision":""#,
            sha,
            r#"","first":1,"last":3,"old":[[1,2]],"new":[[1,2],[600,600]]}},"#,
            r#""original_origin":null,"excerpt":"-old\n+new","quote":"+new","comment":"","#,
            r#""fragments":[{"key":"k","start":0,"end":4,"text":"+new"}],"state":"Changed"}]},"#,
            r#""submissions":[{"key":"5e2c9a71-3b4d-4f8e-a1c6-7d9e0f2b3a45","#,
            r#""snapshot":{"text":"first","attachments":["#,
            attachment,
            r#"]},"status":{"Refused":"no"}},"#,
            r#"{"key":"5e2c9a71-3b4d-4f8e-a1c6-7d9e0f2b3a46","#,
            r#""snapshot":{"text":"second","attachments":[]},"status":{"Uncertain":"lost"}},"#,
            r#"{"key":"5e2c9a71-3b4d-4f8e-a1c6-7d9e0f2b3a47","#,
            r#""snapshot":{"text":"third","attachments":[]},"status":"Submitted"}],"#,
            r#""unready_attachments":2}"#,
        ]
        .concat();
        assert_eq!(record.encode().unwrap(), expected);
        assert_eq!(Record::decode(expected), Some(record));
    }

    // Store-level halves of riwork-desktop's chat_view/draft_tests.rs. An editor is
    // (editor id, generation); each composer change saves the whole draft under it.

    fn submit(drafts: &mut Drafts, chat: &str, owner: Owner, snapshot: Contents) -> String {
        let saved = SavedSubmission {
            key: id(),
            snapshot,
            status: SubmissionStatus::Pending,
        };
        let key = saved.key.clone();
        drafts
            .begin_submission(chat, owner, saved)
            .unwrap()
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        assert!(drafts.pending_submission(chat, &key));
        key
    }

    /// What the process-wide `owns` answers for this store.
    fn owns_in(drafts: &Drafts, chat: &str, owner: Owner) -> bool {
        drafts
            .owners
            .get(chat)
            .is_none_or(|current| current == &owner)
    }

    #[test]
    fn a_closed_tab_comes_back_with_its_draft_and_another_chat_without() {
        let home = home();
        let (a, b) = (id(), id());
        let mut drafts = Drafts::load(&home, SystemTime::now());
        drafts
            .set_draft_with_unready(&a, contents("half a thought\n  indented"), Some((1, 1)), 0)
            .unwrap();
        // Closing the tab saves nothing more; reopening recovers what was typed.
        let (record, warnings) = drafts.recover(&a).unwrap();
        assert_eq!(record.draft, contents("half a thought\n  indented"));
        assert!(record.submissions.is_empty() && warnings.is_empty());
        assert!(drafts.recover(&b).is_none());
        drop(drafts);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn a_draft_survives_a_restart_until_it_is_sent() {
        let home = home();
        let a = id();
        let mut drafts = Drafts::load(&home, SystemTime::now());
        drafts
            .set_draft_with_unready(&a, contents("before the restart"), Some((1, 1)), 0)
            .unwrap();
        drop(drafts);
        // A new process reads only what is on disk; the reopened editor saves it as its own.
        let mut drafts = Drafts::load(&home, SystemTime::now());
        let (record, _) = drafts.recover(&a).unwrap();
        assert_eq!(record.draft, contents("before the restart"));
        drafts
            .set_draft_with_unready(&a, record.draft.clone(), Some((2, 1)), 0)
            .unwrap();
        let key = submit(&mut drafts, &a, (2, 1), record.draft);
        assert_eq!(drafts.get(&a), None);
        assert_eq!(Drafts::load(&home, SystemTime::now()).get(&a), None);
        drafts
            .settle_submission(&a, &key, SubmissionStatus::Submitted)
            .unwrap();
        drafts.flush();
        assert!(Drafts::load(&home, SystemTime::now()).recover(&a).is_none());
        assert!(!home.join(DIR_NAME).join(&a).exists());
        drop(drafts);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn closing_a_tab_after_dispatch_does_not_restore_the_sent_draft() {
        let home = home();
        let a = id();
        let mut drafts = Drafts::load(&home, SystemTime::now());
        drafts
            .set_draft_with_unready(&a, contents("send and close"), Some((1, 1)), 0)
            .unwrap();
        submit(&mut drafts, &a, (1, 1), contents("send and close"));
        // The tab closes before the receipt; reopening, now or after a restart, never resends.
        let reloaded = Drafts::load(&home, SystemTime::now());
        for (record, _) in [drafts.recover(&a).unwrap(), reloaded.recover(&a).unwrap()] {
            assert_eq!(record.draft, Contents::default());
            assert_eq!(record.submissions.len(), 1);
            assert_eq!(record.submissions[0].snapshot.text, "send and close");
            assert_eq!(
                record.submissions[0].status,
                SubmissionStatus::Uncertain("The view closed before delivery was confirmed".into())
            );
        }
        drop((drafts, reloaded));
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn refused_and_uncertain_drafts_are_persisted_again() {
        let home = home();
        let mut drafts = Drafts::load(&home, SystemTime::now());
        for status in [
            SubmissionStatus::Refused("no".into()),
            SubmissionStatus::Uncertain("reply lost".into()),
        ] {
            let a = id();
            let draft = contents("  keep this draft 🦀");
            drafts
                .set_draft_with_unready(&a, draft.clone(), Some((1, 1)), 0)
                .unwrap();
            let key = submit(&mut drafts, &a, (1, 1), draft.clone());
            assert_eq!(drafts.get(&a), None);
            // The failed receipt settles; the editor still owns the draft and saves it again.
            drafts.settle_submission(&a, &key, status.clone()).unwrap();
            assert!(owns_in(&drafts, &a, (1, 1)));
            drafts
                .set_draft_with_unready(&a, draft.clone(), Some((1, 1)), 0)
                .unwrap();
            drafts.flush();
            let reloaded = Drafts::load(&home, SystemTime::now());
            assert_eq!(reloaded.get(&a), Some("  keep this draft 🦀"));
            assert_eq!(
                reloaded.recover(&a).unwrap().0.submissions[0].status,
                status
            );
            drop(reloaded);
            // Typing on, then explicitly restoring the saved snapshot, saves the snapshot.
            drafts
                .set_draft_with_unready(&a, contents("  keep this draft 🦀 newer"), Some((1, 2)), 0)
                .unwrap();
            drafts
                .set_draft_with_unready(&a, draft, Some((1, 3)), 0)
                .unwrap();
            drafts.flush();
            assert_eq!(
                Drafts::load(&home, SystemTime::now()).get(&a),
                Some("  keep this draft 🦀")
            );
        }
        drop(drafts);
        fs::remove_dir_all(home).unwrap();
    }

    /// Also `deleting_a_chat_forgets_its_draft_only_on_success`: a refused delete calls
    /// nothing, a deleted link or a successful delete calls `forget`.
    #[test]
    fn a_deleted_link_forgets_the_chat_draft() {
        let home = home();
        let a = id();
        let mut drafts = Drafts::load(&home, SystemTime::now());
        drafts
            .set_draft_with_unready(&a, contents("delete this draft"), Some((1, 1)), 0)
            .unwrap();
        drafts.flush();
        assert_eq!(
            Drafts::load(&home, SystemTime::now()).get(&a),
            Some("delete this draft")
        );
        drafts.forget(&a).unwrap();
        assert_eq!(drafts.get(&a), None);
        drafts.flush();
        assert_eq!(Drafts::load(&home, SystemTime::now()).get(&a), None);
        drop(drafts);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn a_send_in_one_window_keeps_the_newer_draft_of_another() {
        let home = home();
        let a = id();
        let mut drafts = Drafts::load(&home, SystemTime::now());
        drafts
            .set_draft_with_unready(&a, contents("sent from the first window"), Some((1, 1)), 0)
            .unwrap();
        let key = submit(
            &mut drafts,
            &a,
            (1, 1),
            contents("sent from the first window"),
        );
        // The second window saves newer text before the first hears its send went through.
        drafts
            .set_draft_with_unready(&a, contents("newer in the second window"), Some((2, 1)), 0)
            .unwrap();
        // The receipt settles; the first window clears its composer without saving.
        drafts
            .settle_submission(&a, &key, SubmissionStatus::Submitted)
            .unwrap();
        assert_eq!(drafts.get(&a), Some("newer in the second window"));
        drafts.flush();
        assert_eq!(
            Drafts::load(&home, SystemTime::now()).get(&a),
            Some("newer in the second window")
        );
        drop(drafts);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn a_failed_send_after_the_chat_was_deleted_does_not_bring_its_draft_back() {
        let home = home();
        let a = id();
        let mut drafts = Drafts::load(&home, SystemTime::now());
        drafts
            .set_draft_with_unready(&a, contents("lost with the chat"), Some((1, 1)), 0)
            .unwrap();
        let key = submit(&mut drafts, &a, (1, 1), contents("lost with the chat"));
        // The deletion is heard before the send's failure.
        drafts.forget(&a).unwrap();
        drafts
            .settle_submission(&a, &key, SubmissionStatus::Refused("gone".into()))
            .unwrap();
        // No owner is left, so the editor saves again; nor does that late save bring it back.
        assert!(owns_in(&drafts, &a, (1, 1)));
        drafts
            .set_draft_with_unready(&a, contents("lost with the chat"), Some((1, 1)), 0)
            .unwrap();
        assert_eq!(drafts.get(&a), None);
        assert!(drafts.recover(&a).is_none());
        assert!(!drafts.pending_submission(&a, &key));
        drafts.flush();
        assert!(Drafts::load(&home, SystemTime::now()).recover(&a).is_none());
        drop(drafts);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn ready_attachment_draft_survives_restart_and_acknowledgment_clears_only_recovery() {
        let home = home();
        let chat = id();
        let attachment = staged(&home, &chat);
        let draft = Contents {
            text: "with attachment".into(),
            attachments: vec![attachment.clone()],
            references: Vec::new(),
        };
        let mut drafts = Drafts::load(&home, SystemTime::now());
        drafts
            .set_draft_with_unready(&chat, draft.clone(), Some((1, 2)), 0)
            .unwrap();
        drop(drafts);
        let mut drafts = Drafts::load(&home, SystemTime::now());
        let (record, warnings) = drafts.recover(&chat).unwrap();
        assert_eq!(record.draft, draft);
        assert!(warnings.is_empty());
        drafts
            .set_draft_with_unready(&chat, record.draft.clone(), Some((2, 1)), 0)
            .unwrap();
        let key = submit(&mut drafts, &chat, (2, 1), record.draft);
        drafts
            .settle_submission(&chat, &key, SubmissionStatus::Submitted)
            .unwrap();
        drafts.flush();
        assert!(
            Drafts::load(&home, SystemTime::now())
                .recover(&chat)
                .is_none()
        );
        assert!(
            attachment.path.is_file(),
            "the host retains submitted attachment bytes"
        );
        drop(drafts);
        fs::remove_dir_all(home).unwrap();
    }

    /// Also `closing_during_attachment_check_keeps_metadata_for_the_next_restore`.
    #[test]
    fn recovered_checks_allow_text_edits_block_send_and_remain_durable_before_completion() {
        let home = home();
        let chat = id();
        let attachment = staged(&home, &chat);
        let mut drafts = Drafts::load(&home, SystemTime::now());
        drafts
            .set_draft(
                &chat,
                Contents {
                    text: "original".into(),
                    attachments: vec![attachment.clone()],
                    references: Vec::new(),
                },
                None,
            )
            .unwrap();
        drafts.flush();
        // Closed while its chips were still Checking: the metadata comes back.
        let (record, _) = Drafts::load(&home, SystemTime::now())
            .recover(&chat)
            .unwrap();
        assert_eq!(record.draft.attachments, vec![attachment.clone()]);
        // A Checking chip is recoverable but not sendable; typing saves it with the text.
        drafts
            .set_draft_with_unready(
                &chat,
                Contents {
                    text: "typed while checking".into(),
                    attachments: vec![attachment.clone()],
                    references: Vec::new(),
                },
                Some((1, 2)),
                0,
            )
            .unwrap();
        drafts.flush();
        let restarted = Drafts::load(&home, SystemTime::now());
        let (record, warnings) = restarted.recover(&chat).unwrap();
        assert_eq!(record.draft.text, "typed while checking");
        assert_eq!(record.draft.attachments, vec![attachment.clone()]);
        assert_eq!(record.unready_attachments, 0);
        assert!(warnings.is_empty());
        assert!(validate_attachment(&home.join("chats").join(&chat), &attachment).is_ok());
        drop((drafts, restarted));
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn checking_completion_never_resurrects_removed_chips_or_overwrites_another_editor() {
        let home = home();
        let chat = id();
        let attachment = staged(&home, &chat);
        let with_chip = Contents {
            text: String::new(),
            attachments: vec![attachment],
            references: Vec::new(),
        };
        let mut drafts = Drafts::load(&home, SystemTime::now());
        drafts
            .set_draft_with_unready(&chat, with_chip.clone(), Some((1, 1)), 0)
            .unwrap();
        drafts
            .set_draft_with_unready(&chat, Contents::default(), Some((1, 2)), 0)
            .unwrap();
        assert!(
            drafts.recover(&chat).is_none(),
            "the removed chip stays gone"
        );
        drafts
            .set_draft_with_unready(&chat, with_chip, Some((1, 3)), 0)
            .unwrap();
        // Another live editor takes ownership before the validation continuation.
        drafts
            .set_draft(&chat, contents("newer owner"), Some((u64::MAX, 1)))
            .unwrap();
        assert!(!owns_in(&drafts, &chat, (1, 3)));
        assert_eq!(drafts.get(&chat), Some("newer owner"));
        drop(drafts);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn corrupt_recovered_snapshot_stays_unavailable_and_preserves_metadata_for_review() {
        let home = home();
        let chat = id();
        let attachment = staged(&home, &chat);
        let directory = home.join("chats").join(&chat);
        fs::write(&attachment.path, "corrupt").unwrap();
        assert!(validate_attachment(&directory, &attachment).is_err());
        let mut drafts = Drafts::load(&home, SystemTime::now());
        drafts
            .set_draft_with_unready(
                &chat,
                Contents {
                    text: "exact saved text".into(),
                    attachments: vec![attachment.clone()],
                    references: Vec::new(),
                },
                Some((1, 1)),
                0,
            )
            .unwrap();
        let (record, warnings) = drafts.recover(&chat).unwrap();
        assert_eq!(record.draft.attachments, vec![attachment.clone()]);
        assert!(
            warnings.is_empty(),
            "metadata recovery performs no file reads"
        );
        fs::write(&attachment.path, "A durable attachment 🦀").unwrap();
        assert!(validate_attachment(&directory, &attachment).is_ok());
        drop(drafts);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn admission_refusal_in_stale_window_preserves_the_newer_windows_draft() {
        let home = home();
        let chat = id();
        let mut drafts = Drafts::load(&home, SystemTime::now());
        drafts
            .set_draft_with_unready(&chat, contents("stale first window"), Some((1, 1)), 0)
            .unwrap();
        drafts
            .set_draft_with_unready(&chat, contents("newer second window"), Some((2, 1)), 0)
            .unwrap();
        for _ in 0..MAX_SUBMISSIONS {
            drafts
                .begin_submission(&chat, (u64::MAX, 1), saved("existing recovery"))
                .unwrap();
        }
        let refused = saved("stale first window");
        let error = drafts
            .begin_submission(&chat, (1, 1), refused.clone())
            .unwrap_err();
        assert_eq!(
            error,
            "Dismiss an older saved submission before sending another message"
        );
        // The view settles its refused send and saves only if it still owns the draft.
        drafts
            .settle_submission(&chat, &refused.key, SubmissionStatus::Refused(error))
            .unwrap();
        assert!(!owns_in(&drafts, &chat, (1, 1)));
        assert_eq!(drafts.get(&chat), Some("newer second window"));
        drafts.flush();
        assert_eq!(
            Drafts::load(&home, SystemTime::now()).get(&chat),
            Some("newer second window")
        );
        drop(drafts);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn dismissing_snapshot_before_durability_continuation_prevents_dispatch() {
        let home = home();
        let chat = id();
        let draft = contents("not sent after dismissal");
        let mut drafts = Drafts::load(&home, SystemTime::now());
        drafts
            .set_draft_with_unready(&chat, draft.clone(), Some((1, 1)), 0)
            .unwrap();
        let snapshot = SavedSubmission {
            key: id(),
            snapshot: draft.clone(),
            status: SubmissionStatus::Pending,
        };
        let receiver = drafts
            .begin_submission(&chat, (1, 1), snapshot.clone())
            .unwrap();
        // Another window dismisses the recovered card while this send awaits disk.
        drafts.dismiss_submission(&chat, &snapshot.key).unwrap();
        // The barrier resolves, but nothing is pending any more: nothing is sent.
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(2)).unwrap(),
            Ok(())
        );
        assert!(!drafts.pending_submission(&chat, &snapshot.key));
        // The refused receipt finds no saved submission; the owner saves its text again.
        drafts
            .settle_submission(
                &chat,
                &snapshot.key,
                SubmissionStatus::Refused("dismissed".into()),
            )
            .unwrap();
        assert!(owns_in(&drafts, &chat, (1, 1)));
        drafts
            .set_draft_with_unready(&chat, draft, Some((1, 1)), 0)
            .unwrap();
        drafts.flush();
        let (record, _) = Drafts::load(&home, SystemTime::now())
            .recover(&chat)
            .unwrap();
        assert_eq!(record.draft.text, "not sent after dismissal");
        assert!(record.submissions.is_empty());
        drop(drafts);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn disk_failure_keeps_editor_and_refuses_dispatch() {
        let home = home();
        fs::write(home.join(DIR_NAME), "blocks storage").unwrap();
        let chat = id();
        let draft = contents("not sent without recovery");
        let mut drafts = Drafts::load(&home, SystemTime::now());
        drafts
            .set_draft_with_unready(&chat, draft.clone(), Some((1, 1)), 0)
            .unwrap();
        let snapshot = SavedSubmission {
            key: id(),
            snapshot: draft.clone(),
            status: SubmissionStatus::Pending,
        };
        let error = drafts
            .begin_submission(&chat, (1, 1), snapshot.clone())
            .unwrap()
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap_err();
        assert_eq!(
            error,
            "Could not save the submission recovery snapshot; nothing was sent"
        );
        // Nothing is dispatched: the view settles Refused and, still owning, saves again.
        drafts
            .settle_submission(&chat, &snapshot.key, SubmissionStatus::Refused(error))
            .unwrap();
        assert!(owns_in(&drafts, &chat, (1, 1)));
        drafts
            .set_draft_with_unready(&chat, draft, Some((1, 1)), 0)
            .unwrap();
        fs::remove_file(home.join(DIR_NAME)).unwrap();
        drafts.flush();
        let reloaded = Drafts::load(&home, SystemTime::now());
        assert_eq!(reloaded.get(&chat), Some("not sent without recovery"));
        assert!(matches!(
            reloaded.recover(&chat).unwrap().0.submissions[0].status,
            SubmissionStatus::Refused(_)
        ));
        drop((drafts, reloaded));
        fs::remove_dir_all(home).unwrap();
    }

    /// The only test that touches the process-wide store, so its before-`init` half is
    /// deterministic. Covers `missing_recovery_store_never_marks_unchecked_attachment_ready`
    /// (no store, no recovery directory: the view fails closed).
    #[test]
    fn one_process_store_mirrors_the_gpui_helpers_before_and_after_init() {
        let chat = id();
        let before = saved("no store");
        assert!(with(|_| ()).is_none());
        assert_eq!(draft(&chat), None);
        assert!(recover(&chat).is_none());
        assert!(owns(&chat, (1, 1)));
        assert!(pending_submission(&chat, &before.key));
        assert_eq!(recovery_directory(&chat), None);
        assert_eq!(flush_pending(Duration::from_secs(1)), Ok(()));
        assert_eq!(
            begin_submission(&chat, (1, 1), before.clone())
                .unwrap()
                .try_recv(),
            Ok(Ok(())),
            "no store, no barrier to wait for"
        );
        remember_complete(&chat, contents("dropped"), (1, 1), 0);
        remember(&chat, "dropped");
        settle_submission(&chat, &before.key, SubmissionStatus::Submitted);
        dismiss_submission(&chat, &before.key);
        clear_sent(&chat, "dropped");
        forget(&chat);
        shutdown();

        let home = home();
        let dir = home.join(DIR_NAME);
        init(&home).unwrap();
        init(&home).unwrap();
        assert!(init(&home.join("elsewhere")).is_err());
        assert_eq!(draft(&chat), None);
        assert_eq!(
            recovery_directory(&chat),
            Some(home.join("chats").join(&chat))
        );
        assert_eq!(recovery_directory("../escape"), None);

        remember_complete(&chat, contents("typed"), (1, 1), 0);
        assert_eq!(draft(&chat).as_deref(), Some("typed"));
        assert!(owns(&chat, (1, 1)));
        assert!(!owns(&chat, (2, 1)));
        let sent = SavedSubmission {
            key: id(),
            snapshot: contents("typed"),
            status: SubmissionStatus::Pending,
        };
        let durable = begin_submission(&chat, (1, 1), sent.clone()).unwrap();
        assert_eq!(durable.recv_timeout(Duration::from_secs(2)), Ok(Ok(())));
        assert!(pending_submission(&chat, &sent.key));
        assert_eq!(
            draft(&chat),
            None,
            "the submitting owner's draft became the submission"
        );
        let on_disk = Record::decode(fs::read_to_string(dir.join(&chat)).unwrap()).unwrap();
        assert_eq!(on_disk.submissions, vec![sent.clone()]);
        settle_submission(&chat, &sent.key, SubmissionStatus::Refused("no".into()));
        assert!(!pending_submission(&chat, &sent.key));
        assert_eq!(
            recover(&chat).unwrap().0.submissions[0].status,
            SubmissionStatus::Refused("no".into())
        );
        dismiss_submission(&chat, &sent.key);
        assert!(recover(&chat).is_none());
        remember(&chat, "sent");
        clear_sent(&chat, "sent");
        assert_eq!(draft(&chat), None);
        remember_complete(&chat, contents("kept"), (1, 2), 0);
        // The host's close/quit/reload barrier reaches this store.
        crate::host_flush::flush_blocking().unwrap();
        assert_eq!(fs::read_to_string(dir.join(&chat)).unwrap(), "kept");
        forget(&chat);
        remember_complete(&chat, contents("late"), (1, 3), 0);
        assert_eq!(draft(&chat), None);
        flush_pending(Duration::from_secs(2)).unwrap();
        assert!(!dir.join(&chat).exists());

        // Quit: the newest text of each chat is drained before shutdown returns.
        let other = id();
        for n in 0..100 {
            remember(&other, &n.to_string());
        }
        shutdown();
        assert_eq!(fs::read_to_string(dir.join(&other)).unwrap(), "99");
        shutdown();
        assert_eq!(
            flush_pending(Duration::from_secs(1)),
            Err("The chat draft writer has stopped".into())
        );
        assert_eq!(
            begin_submission(&other, (1, 1), saved("after quit")).unwrap_err(),
            "The chat draft writer has stopped"
        );
        assert_eq!(draft(&other).as_deref(), Some("99"));
        fs::remove_dir_all(home).unwrap();
    }
}
