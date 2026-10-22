//! A chat's files on disk, under `RIWORK_HOME/chats/<id>/`:
//!
//! - `info.json` is the chat's `ChatInfo`, replaced atomically on every change.
//! - `events.jsonl` is its log: one `wire::Envelope` per line, append-only, the
//!   line number being the envelope's `seq` (1-based, gapless). A subscriber's
//!   replay is a copy of these lines, so the file format is the wire format.
//!
//! A crash can leave half a line at the end of the log. `ChatLog::open` cuts
//! it off, so the next event continues the numbering, and it checks that the
//! numbering is what the replay relies on: line N is event N.
//!
//! No file stays open between events: a host with hundreds of chats would run
//! out of descriptors (a macOS app's soft limit is 256).

use super::model::{ChatEvent, ChatInfo, ItemBody};
use super::wire::Envelope;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

const INFO: &str = "info.json";
const EVENTS: &str = "events.jsonl";
/// What a chat that switched provider passes on (`ChatLog::save_context`).
const CONTEXT: &str = "context.md";

/// Where all chats live.
pub fn chats_dir(home: &Path) -> PathBuf {
    home.join("chats")
}

/// The directory of chat `id`, which must be a canonical UUID: an id from the
/// wire never becomes a path component otherwise.
pub fn chat_dir(home: &Path, id: &str) -> Option<PathBuf> {
    Uuid::parse_str(id)
        .ok()
        .filter(|uuid| uuid.to_string() == id)
        .map(|_| chats_dir(home).join(id))
}

pub struct ChatLog {
    dir: PathBuf,
}

impl ChatLog {
    pub(super) fn dir(&self) -> &Path {
        &self.dir
    }

    /// A provider switch starts a new binding, including its alias history. Leaving the
    /// old history under the new ChatInfo would misattribute it during later migrations.
    pub(super) fn reset_account_identity(&self) -> Result<(), String> {
        self.replace(
            "account-identity.json",
            br#"{"v":2,"active":null,"known":[]}"#,
        )
        .map(drop)
    }

    pub(super) fn save_account_identity(
        &self,
        identity: Option<&super::account_identity::Identity>,
    ) -> Result<(), String> {
        write_account_identity(&self.dir, identity)
    }

    /// A new chat: its directory (owner-only), `info.json`, and an empty log.
    pub fn create(dir: &Path, info: &ChatInfo) -> Result<Self, String> {
        crate::paths::create_private_dir(dir)
            .map_err(|error| format!("Cannot create {}: {error}", dir.display()))?;
        open_events(dir)
            .map_err(|error| format!("Cannot create {}: {error}", dir.join(EVENTS).display()))?;
        let log = Self {
            dir: dir.to_owned(),
        };
        log.save_info(info)?;
        Ok(log)
    }

    /// An existing chat: its info, its log ready to append to, and the `seq`
    /// of the last event in it (0 for an empty log).
    pub fn open(dir: &Path) -> Result<(Self, ChatInfo, u64), String> {
        let text = fs::read_to_string(dir.join(INFO))
            .map_err(|error| format!("Cannot read {}: {error}", dir.join(INFO).display()))?;
        let info: ChatInfo = serde_json::from_str(&text)
            .map_err(|error| format!("Cannot parse {}: {error}", dir.join(INFO).display()))?;
        open_events(dir)
            .map_err(|error| format!("Cannot open {}: {error}", dir.join(EVENTS).display()))?;
        let path = dir.join(EVENTS);
        let last = repair_tail(&path)
            .and_then(|last| verify_numbering(&path, last))
            .map_err(|error| format!("Cannot read {}: {error}", path.display()))?;
        Ok((
            Self {
                dir: dir.to_owned(),
            },
            info,
            last,
        ))
    }

    /// Appends one complete line (ending in a newline) with a single write, so
    /// a reader never sees half of it while the host runs. With `sync`, what
    /// was appended so far survives a power loss.
    pub fn append(&self, line: &[u8], sync: bool) -> io::Result<()> {
        debug_assert!(line.ends_with(b"\n"));
        let mut events = open_events(&self.dir)?;
        events.write_all(line)?;
        if sync {
            events.sync_data()?;
        }
        Ok(())
    }

    /// Replaces `info.json`: the old file or the whole new one is there, never
    /// a partial write.
    pub fn save_info(&self, info: &ChatInfo) -> Result<(), String> {
        let data = serde_json::to_vec_pretty(info).map_err(|error| error.to_string())?;
        self.replace(INFO, &data).map(drop)
    }

    /// Whether the current provider session has ever been used. Scan the whole
    /// log as a stream: a tail or `first_user_message` alone would confuse an
    /// empty switched session with an old conversation. Legacy records without
    /// binding metadata belong to the only known session. An unreadable record
    /// is an error, not evidence that a conversation is safe to replace.
    pub(super) fn provider_session_has_history(&self, info: &ChatInfo) -> Result<bool, String> {
        let path = self.dir.join(EVENTS);
        let read = |error: io::Error| format!("Cannot read {}: {error}", path.display());
        let mut reader = BufReader::new(File::open(&path).map_err(read)?);
        let mut binding: Option<(super::model::Provider, Option<String>)> = None;
        let mut history = false;
        let mut any_history = false;
        let mut boundary = false;
        let mut turn = None;
        let mut seq = 0;
        let mut line = Vec::new();
        loop {
            line.clear();
            if reader.read_until(b'\n', &mut line).map_err(read)? == 0 {
                break;
            }
            let envelope: Envelope = serde_json::from_slice(&line)
                .map_err(|error| format!("Cannot parse {}: {error}", path.display()))?;
            if line.last() != Some(&b'\n') || envelope.chat_id != info.id || envelope.seq != seq + 1
            {
                return Err(format!("Invalid session history in {}", path.display()));
            }
            seq = envelope.seq;
            if let ChatEvent::Info { info: said } = &envelope.event {
                let changed = binding.as_ref().is_some_and(|(provider, thread)| {
                    *provider != said.provider
                        || matches!((thread, &said.provider_thread_id), (Some(a), Some(b)) if a != b)
                });
                if changed {
                    // The provider can adopt its real identity in system/init
                    // after the host has logged the first accepted input.
                    // Activity in the still-open turn follows that identity;
                    // a switch or an idle /clear starts a new history segment.
                    let continuing = binding
                        .as_ref()
                        .is_some_and(|(provider, _)| *provider == said.provider)
                        && (turn.is_some()
                            || matches!(
                                said.state,
                                super::model::ChatState::Running | super::model::ChatState::Waiting
                            ));
                    if !continuing {
                        history = false;
                        turn = None;
                    }
                    boundary = true;
                }
                binding = Some((said.provider, said.provider_thread_id.clone()));
                if matches!(
                    said.state,
                    super::model::ChatState::Running | super::model::ChatState::Waiting
                ) {
                    history = true;
                    any_history = true;
                }
            } else if is_session_history(&envelope.event) {
                history = true;
                any_history = true;
            }
            match envelope.event {
                ChatEvent::TurnStarted { turn_id, .. } => turn = Some(turn_id),
                ChatEvent::TurnCompleted { turn_id, .. } if turn.as_ref() == Some(&turn_id) => {
                    turn = None
                }
                _ => {}
            }
        }
        if binding.is_none() && info.provider_thread_id.is_some() {
            // Metadata-only legacy chats (including a missing/recreated log)
            // cannot prove that their provider session was never used.
            return Ok(true);
        }
        if binding.as_ref().is_some_and(|(provider, thread)| {
            *provider != info.provider
                || matches!((thread, &info.provider_thread_id), (Some(a), Some(b)) if a != b)
        }) {
            // Metadata can be saved just before a crash prevented its Info.
            // With ambiguous ownership, never assert that a used chat is empty.
            return Ok(any_history || info.first_user_message.is_some());
        }
        Ok(history || (!boundary && info.first_user_message.is_some()))
    }

    /// Writes the conversation a chat passes on when it switches provider
    /// (`ChatInfo::carried_over`), replacing the one an earlier switch wrote, and returns
    /// where it is. It goes with the chat when the chat is deleted.
    pub fn save_context(&self, text: &str) -> Result<PathBuf, String> {
        self.replace(CONTEXT, text.as_bytes())
    }

    /// Replaces the file `name` of the chat's folder whole, owner-only.
    fn replace(&self, name: &str, data: &[u8]) -> Result<PathBuf, String> {
        let path = self.dir.join(name);
        let temporary = self.dir.join(format!(".{name}-{}.tmp", Uuid::new_v4()));
        let result = (|| {
            let mut file = private_options().create_new(true).open(&temporary)?;
            file.write_all(data)?;
            file.sync_all()?;
            fs::rename(&temporary, &path)
        })();
        if let Err(error) = result {
            let _ = fs::remove_file(&temporary);
            return Err(format!("Cannot write {}: {error}", path.display()));
        }
        Ok(path)
    }
}

fn is_session_history(event: &ChatEvent) -> bool {
    match event {
        ChatEvent::State {
            state: super::model::ChatState::Running | super::model::ChatState::Waiting,
        }
        | ChatEvent::TurnStarted { .. }
        | ChatEvent::TurnCompleted { .. }
        | ChatEvent::ItemDelta { .. }
        | ChatEvent::ApprovalRequested { .. }
        | ChatEvent::QuestionRequested { .. } => true,
        // A worker marker is the host's record of a terminal or chat, never the
        // provider's own session: it cannot make a new provider look resumable, even in a
        // shape this build cannot read.
        ChatEvent::ItemStarted { item } | ChatEvent::ItemCompleted { item } => {
            !matches!(item.body, ItemBody::Notice { .. }) && !super::workers::is_marker(item)
        }
        _ => false,
    }
}

pub(super) fn write_account_identity(
    dir: &Path,
    identity: Option<&super::account_identity::Identity>,
) -> Result<(), String> {
    let temporary = dir.join(format!(".account-identity-{}.tmp", Uuid::new_v4()));
    let result = (|| -> io::Result<()> {
        let mut file = private_options().create_new(true).open(&temporary)?;
        file.write_all(&serde_json::to_vec(&super::account_identity::record(
            dir, identity,
        ))?)?;
        file.sync_all()?;
        fs::rename(&temporary, dir.join("account-identity.json"))
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        return Err(error.to_string());
    }
    Ok(())
}

fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

fn open_events(dir: &Path) -> io::Result<File> {
    private_options()
        .create(true)
        .append(true)
        .open(dir.join(EVENTS))
}

/// Writes the log's events `since + 1 ..= last` to `out`, enriching legacy metadata
/// only in the response. Durable bytes and event sequence numbers never change. The caller
/// knows `last` from the moment it registered for live events, so lines that
/// are written afterwards are not part of the replay.
pub fn replay(dir: &Path, since: u64, last: u64, out: &mut impl Write) -> io::Result<()> {
    let truncated = || {
        io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "the event log is shorter than its numbering",
        )
    };
    let mut reader = BufReader::with_capacity(256 * 1024, File::open(dir.join(EVENTS))?);
    for _ in 0..since {
        if reader.skip_until(b'\n')? == 0 {
            return Err(truncated());
        }
    }
    let mut line = Vec::new();
    for _ in since..last {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return Err(truncated());
        }
        if let Ok(mut envelope) = serde_json::from_slice::<Envelope>(&line) {
            envelope.enrich();
            serde_json::to_writer(&mut *out, &envelope)?;
            out.write_all(b"\n")?;
        } else {
            // Future event/body shapes remain forwardable to tolerant clients.
            out.write_all(&line)?;
        }
    }
    out.flush()
}

// ---- Reading a chat's files without the host ----------------------------------------
//
// The files are the chat's public record (the README describes them), and the
// host only ever appends whole lines to the log and replaces `info.json` atomically,
// so a reader that never writes can look at a chat whether the host runs or not.

/// The most of a log `read_transcript` reads: its end. A transcript is read to
/// be shown, and the end of a chat is what is wanted of it.
const TRANSCRIPT_TAIL: u64 = 32 << 20;

/// The saved info of every chat, for a caller that finds no host running. A
/// chat that cannot be read is left out. Nothing runs without a host, so a chat
/// that was at work when its host ended is `Stopped` here, as the next host
/// will make it.
pub fn read_infos(home: &Path) -> Vec<ChatInfo> {
    let Ok(entries) = fs::read_dir(chats_dir(home)) else {
        return Vec::new();
    };
    let mut infos: Vec<ChatInfo> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let dir = chat_dir(home, &name)?;
            let text = fs::read_to_string(dir.join(INFO)).ok()?;
            let mut info: ChatInfo = serde_json::from_str(&text).ok()?;
            if info.id != name {
                return None;
            }
            if !matches!(
                info.state,
                super::model::ChatState::Stopped | super::model::ChatState::Failed { .. }
            ) {
                info.state = super::model::ChatState::Stopped;
            }
            Some(info)
        })
        .collect();
    infos.sort_by(|a, b| (a.created_at_unix, &a.id).cmp(&(b.created_at_unix, &b.id)));
    infos
}

/// Missing history is empty; unreadable history is unknown, never authoritative.
pub fn read_infos_checked(home: &Path) -> Result<Vec<ChatInfo>, String> {
    let entries = match fs::read_dir(chats_dir(home)) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
    let mut infos = Vec::new();
    for entry in entries {
        let read = (|| -> Result<Option<ChatInfo>, String> {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(dir) = chat_dir(home, &name) else {
                return Ok(None);
            };
            if !entry.file_type().map_err(|e| e.to_string())?.is_dir() {
                return Ok(None);
            }
            let text = fs::read_to_string(dir.join(INFO))
                .map_err(|e| format!("{}: {e}", dir.display()))?;
            let mut info: ChatInfo =
                serde_json::from_str(&text).map_err(|e| format!("{}: {e}", dir.display()))?;
            if info.id != name {
                return Err(format!(
                    "{}: chat metadata identity mismatch",
                    dir.display()
                ));
            }
            if !matches!(
                info.state,
                super::model::ChatState::Stopped | super::model::ChatState::Failed { .. }
            ) {
                info.state = super::model::ChatState::Stopped;
            }
            Ok(Some(info))
        })();
        match read {
            Ok(Some(info)) => infos.push(info),
            Ok(None) => {}
            Err(error) => eprintln!("riwork tabs: skipping unreadable chat metadata: {error}"),
        }
    }
    infos.sort_by(|a, b| (a.created_at_unix, &a.id).cmp(&(b.created_at_unix, &b.id)));
    Ok(infos)
}

/// The transcript the log of chat `id` builds, read from the end of the file.
pub fn read_transcript(home: &Path, id: &str) -> Result<super::model::Transcript, String> {
    read_transcript_tail(home, id, TRANSCRIPT_TAIL)
}

/// Fold complete events from a bounded tail without contacting the chat host.
/// A small observer such as the worker shelf need not replay a long conversation.
pub fn read_transcript_tail(
    home: &Path,
    id: &str,
    max_bytes: u64,
) -> Result<super::model::Transcript, String> {
    let max_bytes = max_bytes.min(TRANSCRIPT_TAIL);
    let dir = chat_dir(home, id).ok_or("invalid chat id")?;
    let path = dir.join(EVENTS);
    let mut file =
        File::open(&path).map_err(|error| format!("Cannot read {}: {error}", path.display()))?;
    let length = file
        .metadata()
        .map_err(|error| format!("Cannot read {}: {error}", path.display()))?
        .len();
    let start = length.saturating_sub(max_bytes);
    let from = start.saturating_sub(1);
    file.seek(SeekFrom::Start(from))
        .map_err(|error| format!("Cannot read {}: {error}", path.display()))?;
    let mut reader = BufReader::new(file.take(length - from));
    let read = |error: io::Error| format!("Cannot read {}: {error}", path.display());
    if start > 0 {
        // Only discard a fragment: the byte before the tail may end a whole record.
        let mut previous = [0];
        reader.read_exact(&mut previous).map_err(read)?;
        if previous[0] != b'\n' {
            reader.skip_until(b'\n').map_err(read)?;
        }
    }
    let mut transcript = super::model::Transcript::default();
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line).map_err(read)? == 0 {
            break;
        }
        // A line the host is still writing has no newline yet.
        if line.last() != Some(&b'\n') {
            break;
        }
        if let Ok(envelope) = serde_json::from_slice::<Envelope>(&line) {
            transcript.apply_sequenced(envelope.seq, &envelope.event);
        }
    }
    Ok(transcript)
}

/// How far back a starting driver looks for notices it must be able to resolve.
pub const NOTICE_TAIL: u64 = 2 << 20;

/// The notices in the last `max` bytes of the log in `dir`, folded oldest first, so a
/// starting driver finds what is still outstanding without reading a long chat whole.
/// A log that cannot be read has none.
pub fn notice_tail(dir: &Path, max: u64) -> super::model::Transcript {
    let mut transcript = super::model::Transcript::default();
    let Ok(mut file) = File::open(dir.join(EVENTS)) else {
        return transcript;
    };
    let Ok(length) = file.metadata().map(|metadata| metadata.len()) else {
        return transcript;
    };
    let start = length.saturating_sub(max);
    let from = start.saturating_sub(1);
    if file.seek(SeekFrom::Start(from)).is_err() {
        return transcript;
    }
    let mut reader = BufReader::new(file.take(length - from));
    let mut line = Vec::new();
    if start > 0 {
        let mut previous = [0];
        if reader.read_exact(&mut previous).is_err()
            || (previous[0] != b'\n' && reader.skip_until(b'\n').is_err())
        {
            return transcript;
        }
    }
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        // Only notice lines are parsed; the rest of a long chat is skipped as bytes.
        if line.last() != Some(&b'\n') || !contains(&line, br#""type":"notice""#) {
            continue;
        }
        if let Ok(envelope) = serde_json::from_slice::<Envelope>(&line) {
            transcript.apply_sequenced(envelope.seq, &envelope.event);
        }
    }
    transcript
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

// Snapshot v1 reads one immutable prefix without contacting or upgrading the host.
// Explicit limits fail closed: never return a partial item or advance past omitted state.
const SNAPSHOT_FILE_MAX: u64 = 128 << 20;
const SNAPSHOT_LINE_MAX: u64 = 8 << 20;
const SNAPSHOT_EVENTS_MAX: u64 = 1_000_000;

#[derive(serde::Serialize, serde::Deserialize)]
pub struct SnapshotItem {
    pub order: u64,
    pub item: super::model::Item,
}
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Snapshot {
    pub v: u8,
    pub chat_id: String,
    pub cursor: String,
    pub next: u64,
    pub before: u64,
    pub more: bool,
    pub items: Vec<SnapshotItem>,
    pub controls: Vec<super::model::ChatEvent>,
    #[serde(default)]
    pub requests: Vec<super::model::RequestRecord>,
    /// Captured turn wall-clock times for the turns represented by this page.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub turn_timing: std::collections::HashMap<String, super::model::TurnTiming>,
    /// First event sequence of each represented turn, for lossless page alignment.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub turn_starts: std::collections::HashMap<String, u64>,
    /// A transport limit required a partial turn; clients remap anchors by item id.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub provisional: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dismissed_notices: Vec<String>,
}

pub fn read_snapshot(
    home: &Path,
    id: &str,
    cursor: Option<&str>,
    before: u64,
    limit: usize,
    max_bytes: usize,
    item_ids: &[String],
) -> Result<Snapshot, String> {
    read_snapshot_with_features(home, id, cursor, before, limit, max_bytes, item_ids, false)
}

pub fn read_snapshot_with_features(
    home: &Path,
    id: &str,
    cursor: Option<&str>,
    before: u64,
    limit: usize,
    max_bytes: usize,
    item_ids: &[String],
    rate_limits: bool,
) -> Result<Snapshot, String> {
    use super::model::ChatEvent;
    use std::collections::HashMap;
    if cursor.is_some_and(|c| c.len() > 80 || c.is_empty())
        || (!item_ids.is_empty() && cursor.is_none())
        || (cursor.is_none() && before != u64::MAX)
        || item_ids.len() > 100
        || item_ids.iter().any(|id| id.is_empty() || id.len() > 512)
        || !(1..=100).contains(&limit)
        || !(4096..=8 << 20).contains(&max_bytes)
    {
        return Err("invalid snapshot limits".into());
    }
    let dir = chat_dir(home, id).ok_or("invalid chat id")?;
    let file = File::open(dir.join(EVENTS)).map_err(|_| "chat not found")?;
    let length = file.metadata().map_err(|e| e.to_string())?.len();
    let end = match cursor {
        Some(c) => c
            .split('-')
            .next()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v <= length)
            .ok_or("snapshot expired")?,
        None => length,
    };
    if end > SNAPSHOT_FILE_MAX {
        return Err("snapshot file limit exceeded".into());
    }
    let started = std::time::Instant::now();
    let mut reader = BufReader::new(file.take(end));
    let mut transcript = super::model::Transcript::default();
    let mut orders = HashMap::new();
    let mut turn_starts = HashMap::new();
    let (mut next, mut bytes, mut hash) = (0u64, 0u64, 0xcbf29ce484222325u64);
    let mut line = Vec::new();
    loop {
        line.clear();
        // Take is per line, preventing an untrusted unterminated record allocating the file.
        let n = reader
            .by_ref()
            .take(SNAPSHOT_LINE_MAX + 1)
            .read_until(b'\n', &mut line)
            .map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        if n as u64 > SNAPSHOT_LINE_MAX {
            return Err("snapshot record limit exceeded".into());
        }
        if line.last() != Some(&b'\n') {
            break;
        }
        if next >= SNAPSHOT_EVENTS_MAX || started.elapsed().as_secs() >= 5 {
            return Err("snapshot work limit exceeded".into());
        }
        let envelope: Envelope =
            serde_json::from_slice(&line).map_err(|_| "invalid snapshot record")?;
        if envelope.chat_id != id || envelope.seq != next + 1 {
            return Err("invalid snapshot sequence".into());
        }
        for b in &line {
            hash = (hash ^ u64::from(*b)).wrapping_mul(0x100000001b3);
        }
        bytes += n as u64;
        next = envelope.seq;
        if let ChatEvent::TurnStarted { turn_id, .. } = &envelope.event {
            turn_starts.entry(turn_id.clone()).or_insert(next);
        }
        if let ChatEvent::ItemStarted { item } | ChatEvent::ItemCompleted { item } = &envelope.event
        {
            orders.entry(item.id.clone()).or_insert(next);
            if let Some(turn_id) = &item.turn_id {
                turn_starts.entry(turn_id.clone()).or_insert(next);
            }
        }
        transcript.apply_sequenced(envelope.seq, &envelope.event);
    }
    let mark = format!("{bytes}-{next}-{hash:016x}");
    if cursor.is_some_and(|c| c != mark) {
        return Err("snapshot expired".into());
    }
    let mut controls = Vec::new();
    if cursor.is_none() {
        if let Some(info) = transcript.info.clone() {
            controls.push(ChatEvent::Info { info });
        }
        controls.push(ChatEvent::State {
            state: transcript.state.clone(),
        });
        if let Some(turn_id) = transcript.turn_id.clone() {
            let started_at = transcript
                .turn_timing
                .get(&turn_id)
                .and_then(|t| t.started_at);
            controls.push(ChatEvent::TurnStarted {
                turn_id,
                started_at,
            });
        }
        for approval in &transcript.approvals {
            controls.push(ChatEvent::ApprovalRequested {
                approval: approval.clone(),
            });
        }
        for question in &transcript.questions {
            controls.push(ChatEvent::QuestionRequested {
                question: question.clone(),
            });
        }
        if let Some(usage) = transcript.usage.clone() {
            controls.push(ChatEvent::Usage { usage });
        }
        if rate_limits && !transcript.rate_limits.is_empty() {
            controls.push(ChatEvent::RateLimits {
                windows: transcript.rate_limits.clone(),
            });
        }
        controls.push(ChatEvent::Models {
            models: transcript.models.clone(),
        });
        // No `Account`: a phone reads the controls as one list and refuses a reply with
        // an event it does not know. It follows the live events, which skip one alone.
    }
    let clipped_turns: std::collections::HashSet<_> = transcript
        .items
        .iter()
        .filter(|item| orders[&item.id] >= before)
        .filter_map(|item| item.turn_id.clone())
        .collect();
    let mut items: Vec<_> = transcript
        .items
        .into_iter()
        .filter_map(|item| {
            let order = orders[&item.id];
            (order < before && (item_ids.is_empty() || item_ids.contains(&item.id)))
                .then_some(SnapshotItem { order, item })
        })
        .collect();
    let mut cut = if item_ids.is_empty() {
        items.len().saturating_sub(limit)
    } else {
        0
    };
    let mut page_start = items
        .get(cut)
        .map_or(0, |row| snapshot_item_boundary(row, &turn_starts));
    // Pages extend back to a turn boundary. Only the response byte limit below
    // may produce a partial oversized turn, explicitly marked provisional.
    if item_ids.is_empty() && cut < items.len() {
        loop {
            page_start = page_start.min(snapshot_item_boundary(&items[cut], &turn_starts));
            let previous_cut = cut;
            // A turn may contain notices without a turn id, or late rows from
            // another turn. Every row at/after its boundary belongs on this page.
            while cut > 0 && items[cut - 1].order >= page_start {
                cut -= 1;
            }
            if cut == previous_cut {
                break;
            }
        }
    }
    items.drain(..cut);
    let provisional = items
        .iter()
        .filter_map(|row| row.item.turn_id.as_ref())
        .any(|turn| clipped_turns.contains(turn));
    let requests = if item_ids.is_empty() {
        transcript
            .requests
            .into_iter()
            .filter(|record| record.requested_seq >= page_start && record.requested_seq < before)
            .collect()
    } else {
        Vec::new()
    };
    let dismissed_notices =
        super::notice_dismissals::snapshot_keys(home, transcript.info.as_ref())?;
    let mut snapshot = Snapshot {
        v: 1,
        chat_id: id.into(),
        cursor: mark,
        next,
        before: page_start,
        more: item_ids.is_empty() && cut > 0,
        items,
        controls,
        requests,
        turn_timing: transcript.turn_timing,
        turn_starts,
        provisional,
        dismissed_notices,
    };
    retain_snapshot_turns(&mut snapshot);
    // Reduce by whole oldest items only. Oversized controls or one item are explicit errors.
    while serde_json::to_vec(&snapshot)
        .map_err(|e| e.to_string())?
        .len()
        > max_bytes
    {
        if !item_ids.is_empty() || snapshot.items.len() <= 1 {
            return Err("snapshot response limit exceeded".into());
        }
        let removed = snapshot.items.remove(0);
        let boundary = snapshot_item_boundary(&snapshot.items[0], &snapshot.turn_starts);
        // If a byte cap cuts through a turn interval, item-exact pagination is
        // required: an older page must still be able to recover every dropped row.
        let partial_start = removed.order >= boundary;
        snapshot.before = if partial_start {
            snapshot.items[0].order
        } else {
            boundary
        };
        snapshot
            .requests
            .retain(|record| record.requested_seq >= snapshot.before);
        snapshot.provisional |= partial_start
            || snapshot.items.iter().any(|row| {
                row.item.turn_id.as_ref().is_some_and(|turn| {
                    snapshot
                        .turn_starts
                        .get(turn)
                        .is_some_and(|start| *start <= removed.order)
                })
            });
        snapshot.more = true;
        retain_snapshot_turns(&mut snapshot);
    }
    Ok(snapshot)
}

fn snapshot_item_boundary(
    row: &SnapshotItem,
    turn_starts: &std::collections::HashMap<String, u64>,
) -> u64 {
    row.item
        .turn_id
        .as_ref()
        .and_then(|turn| turn_starts.get(turn))
        .copied()
        .unwrap_or(row.order)
        .min(row.order)
}

fn retain_snapshot_turns(snapshot: &mut Snapshot) {
    let turns: std::collections::HashSet<_> = snapshot
        .items
        .iter()
        .filter_map(|row| row.item.turn_id.as_deref())
        .chain(
            snapshot
                .requests
                .iter()
                .filter_map(|request| request.turn_id.as_deref()),
        )
        .chain(snapshot.controls.iter().filter_map(|event| match event {
            ChatEvent::TurnStarted { turn_id, .. } => Some(turn_id.as_str()),
            _ => None,
        }))
        .collect();
    snapshot
        .turn_timing
        .retain(|turn, _| turns.contains(turn.as_str()));
    snapshot
        .turn_starts
        .retain(|turn, _| turns.contains(turn.as_str()));
}

/// How long the log of chat `id` is, in bytes: a mark that `read_after` reads
/// everything after.
pub fn mark(home: &Path, id: &str) -> Result<u64, String> {
    let dir = chat_dir(home, id).ok_or("invalid chat id")?;
    let path = dir.join(EVENTS);
    fs::metadata(&path)
        .map(|meta| meta.len())
        .map_err(|error| format!("Cannot read {}: {error}", path.display()))
}

/// The whole events appended to the log of chat `id` after `mark`, and the mark
/// that follows the last of them.
pub fn read_after(home: &Path, id: &str, mark: u64) -> Result<(Vec<Envelope>, u64), String> {
    let dir = chat_dir(home, id).ok_or("invalid chat id")?;
    let path = dir.join(EVENTS);
    let fail = |error: io::Error| format!("Cannot read {}: {error}", path.display());
    let mut file = File::open(&path).map_err(fail)?;
    file.seek(SeekFrom::Start(mark)).map_err(fail)?;
    let mut reader = BufReader::new(file);
    let (mut envelopes, mut next) = (Vec::new(), mark);
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line).map_err(fail)? == 0 || line.last() != Some(&b'\n') {
            break;
        }
        next += line.len() as u64;
        if let Ok(mut envelope) = serde_json::from_slice::<Envelope>(&line) {
            envelope.enrich();
            envelopes.push(envelope);
        }
    }
    Ok((envelopes, next))
}

/// Every event of the log, for a chat that has to be examined whole.
pub fn read_envelopes(dir: &Path) -> io::Result<Vec<Envelope>> {
    let reader = BufReader::new(File::open(dir.join(EVENTS))?);
    let mut envelopes = Vec::new();
    for line in reader.lines() {
        match serde_json::from_str::<Envelope>(&line?) {
            Ok(mut envelope) if envelope.seq == envelopes.len() as u64 + 1 => {
                envelope.enrich();
                envelopes.push(envelope);
            }
            _ => break,
        }
    }
    Ok(envelopes)
}

/// Cuts a half-written last line off the log and returns the `seq` of the last
/// whole one. A last line that does not parse is cut off too.
fn repair_tail(path: &Path) -> io::Result<u64> {
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    loop {
        let length = file.metadata()?.len();
        if length == 0 {
            return Ok(0);
        }
        let mut last_byte = [0];
        file.seek(SeekFrom::Start(length - 1))?;
        file.read_exact(&mut last_byte)?;
        if last_byte[0] != b'\n' {
            let whole = line_start(&mut file, length)?;
            file.set_len(whole)?;
            continue;
        }
        let start = line_start(&mut file, length - 1)?;
        file.seek(SeekFrom::Start(start))?;
        let mut line = Vec::new();
        BufReader::new(&mut file).read_until(b'\n', &mut line)?;
        match serde_json::from_slice::<Envelope>(&line) {
            Ok(envelope) => return Ok(envelope.seq),
            Err(_) => file.set_len(start)?,
        }
    }
}

/// Checks that line N of the log is event N, as far as `last` (the number of
/// the last line's event) says: the replay finds events by counting lines. A
/// log where that does not hold (edited, or pieced together from another)
/// keeps its longest gapless beginning, and the rest is cut off. Returns the
/// number of events that are left.
fn verify_numbering(path: &Path, last: u64) -> io::Result<u64> {
    let mut reader = BufReader::with_capacity(1 << 20, File::open(path)?);
    let mut lines = 0u64;
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            break;
        }
        lines += buffer.iter().filter(|byte| **byte == b'\n').count() as u64;
        let length = buffer.len();
        reader.consume(length);
    }
    if lines == last {
        return Ok(last);
    }
    let mut reader = BufReader::new(File::open(path)?);
    let (mut kept_lines, mut kept_bytes) = (0u64, 0u64);
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        match serde_json::from_slice::<Envelope>(&line) {
            Ok(envelope) if envelope.seq == kept_lines + 1 && line.ends_with(b"\n") => {
                kept_lines += 1;
                kept_bytes += line.len() as u64;
            }
            _ => break,
        }
    }
    OpenOptions::new()
        .write(true)
        .open(path)?
        .set_len(kept_bytes)?;
    eprintln!(
        "riwork chat: {} was numbered irregularly; kept its first {kept_lines} events",
        path.display()
    );
    Ok(kept_lines)
}

/// The offset just after the last newline that lies before `end`; 0 if none.
fn line_start(file: &mut File, end: u64) -> io::Result<u64> {
    const CHUNK: u64 = 64 * 1024;
    let mut position = end;
    while position > 0 {
        let from = position.saturating_sub(CHUNK);
        let mut buffer = vec![0; (position - from) as usize];
        file.seek(SeekFrom::Start(from))?;
        file.read_exact(&mut buffer)?;
        if let Some(index) = buffer.iter().rposition(|byte| *byte == b'\n') {
            return Ok(from + index as u64 + 1);
        }
        position = from;
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::model::{ApprovalMode, ChatEvent, ChatState, Provider};

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rwlog-{}", &Uuid::new_v4().to_string()[..8]));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn info(dir: &Path) -> ChatInfo {
        ChatInfo {
            parent_id: None,
            user_title: None,
            first_user_message: None,
            provider_title: None,
            id: Uuid::new_v4().to_string(),
            provider: Provider::Codex,
            project_id: None,
            worktree_id: None,
            cwd: dir.to_owned(),
            title: "Codex chat".into(),
            created_at_unix: 1,
            provider_thread_id: None,
            model: None,
            effort: None,
            approval_mode: ApprovalMode::Supervised,
            codex_account_id: None,
            state: ChatState::Starting,
            orchestrator: None,
            fast: false,
            contributor_mode: Default::default(),
            contributor_opt_out: false,
            claude_billing: None,
            carried_over: None,
        }
    }

    fn line(seq: u64) -> String {
        let envelope = Envelope {
            chat_id: "c".into(),
            seq,
            event: ChatEvent::State {
                state: ChatState::Idle,
            },
        };
        format!("{}\n", serde_json::to_string(&envelope).unwrap())
    }

    #[test]
    fn a_chat_is_written_and_read_back_with_private_modes() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch();
        let dir = root.join("chat");
        let info = info(&root);
        let log = ChatLog::create(&dir, &info).unwrap();
        log.append(line(1).as_bytes(), false).unwrap();
        log.append(line(2).as_bytes(), true).unwrap();
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join(INFO)), 0o600);
        assert_eq!(mode(&dir.join(EVENTS)), 0o600);
        drop(log);
        let (_, read, last) = ChatLog::open(&dir).unwrap();
        assert_eq!((read, last), (info, 2));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn saving_info_leaves_no_temporary_files_and_replaces_the_old_one() {
        let root = scratch();
        let dir = root.join("chat");
        let mut info = info(&root);
        let log = ChatLog::create(&dir, &info).unwrap();
        info.title = "Renamed".into();
        info.state = ChatState::Idle;
        log.save_info(&info).unwrap();
        let mut names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, [EVENTS, INFO]);
        assert_eq!(ChatLog::open(&dir).unwrap().1, info);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_half_written_last_line_is_cut_off_and_numbering_continues() {
        let root = scratch();
        let dir = root.join("chat");
        let info = info(&root);
        let log = ChatLog::create(&dir, &info).unwrap();
        for seq in 1..=3 {
            log.append(line(seq).as_bytes(), false).unwrap();
        }
        let whole = fs::read(dir.join(EVENTS)).unwrap();
        // Half of a fourth line, as a crash in the middle of a write leaves it.
        let mut cut = whole.clone();
        cut.extend_from_slice(&line(4).as_bytes()[..20]);
        fs::write(dir.join(EVENTS), &cut).unwrap();
        let (log, _, last) = ChatLog::open(&dir).unwrap();
        assert_eq!(last, 3);
        assert_eq!(fs::read(dir.join(EVENTS)).unwrap(), whole);
        log.append(line(4).as_bytes(), false).unwrap();
        assert_eq!(read_envelopes(&dir).unwrap().len(), 4);
        // A whole line that is not an event is cut off too.
        let mut garbled = fs::read(dir.join(EVENTS)).unwrap();
        garbled.extend_from_slice(b"{\"oops\":true}\n");
        fs::write(dir.join(EVENTS), &garbled).unwrap();
        assert_eq!(ChatLog::open(&dir).unwrap().2, 4);
        // No newline anywhere: nothing to keep.
        fs::write(dir.join(EVENTS), b"{\"chat_id\"").unwrap();
        assert_eq!(ChatLog::open(&dir).unwrap().2, 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_log_whose_numbering_is_irregular_keeps_its_gapless_beginning() {
        let root = scratch();
        let dir = root.join("chat");
        let log = ChatLog::create(&dir, &info(&root)).unwrap();
        for seq in [1, 2, 4, 5] {
            log.append(line(seq).as_bytes(), false).unwrap();
        }
        let (log, _, last) = ChatLog::open(&dir).unwrap();
        assert_eq!(last, 2);
        assert_eq!(read_envelopes(&dir).unwrap().len(), 2);
        log.append(line(3).as_bytes(), false).unwrap();
        assert_eq!(ChatLog::open(&dir).unwrap().2, 3);
        // A last event with an absurd number is not taken at its word.
        log.append(line(u64::MAX).as_bytes(), false).unwrap();
        assert_eq!(ChatLog::open(&dir).unwrap().2, 3);
        // Neither is a last line that is not even text.
        let mut bytes = fs::read(dir.join(EVENTS)).unwrap();
        bytes.extend_from_slice(b"\xff\xfe\n");
        fs::write(dir.join(EVENTS), bytes).unwrap();
        assert_eq!(ChatLog::open(&dir).unwrap().2, 3);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn appending_holds_no_file_open() {
        let root = scratch();
        let dir = root.join("chat");
        let log = ChatLog::create(&dir, &info(&root)).unwrap();
        // Removing the directory under a log is seen at the next append.
        fs::remove_dir_all(&dir).unwrap();
        assert!(log.append(line(1).as_bytes(), false).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_replay_copies_the_requested_lines_as_stored() {
        let root = scratch();
        let dir = root.join("chat");
        let log = ChatLog::create(&dir, &info(&root)).unwrap();
        for seq in 1..=5 {
            log.append(line(seq).as_bytes(), false).unwrap();
        }
        let mut out = Vec::new();
        replay(&dir, 2, 4, &mut out).unwrap();
        assert_eq!(out, [line(3), line(4)].concat().as_bytes());
        let mut nothing = Vec::new();
        replay(&dir, 5, 5, &mut nothing).unwrap();
        assert!(nothing.is_empty());
        assert!(replay(&dir, 0, 6, &mut Vec::new()).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    fn user_line(seq: u64, text: &str) -> String {
        let envelope = Envelope {
            chat_id: "c".into(),
            seq,
            event: ChatEvent::ItemCompleted {
                item: crate::chat::model::Item {
                    started_at: None,
                    ended_at: None,
                    presentation: Default::default(),
                    id: format!("u{seq}"),
                    turn_id: None,
                    status: crate::chat::model::ItemStatus::Completed,
                    body: crate::chat::model::ItemBody::UserMessage { text: text.into() },
                },
            },
        };
        format!("{}\n", serde_json::to_string(&envelope).unwrap())
    }

    #[test]
    fn a_small_transcript_tail_skips_old_partial_events_and_keeps_recent_output() {
        let home = scratch();
        let chat = info(&home);
        let directory = chat_dir(&home, &chat.id).unwrap();
        let log = ChatLog::create(&directory, &chat).unwrap();
        log.append(user_line(1, &"old output ".repeat(1000)).as_bytes(), false)
            .unwrap();
        let recent = user_line(2, "fresh worker finding");
        log.append(recent.as_bytes(), false).unwrap();
        let transcript = read_transcript_tail(&home, &chat.id, recent.len() as u64 + 1).unwrap();
        assert_eq!(transcript.items.len(), 1);
        assert_eq!(transcript.items[0].id, "u2");
        let aligned = read_transcript_tail(&home, &chat.id, recent.len() as u64).unwrap();
        assert_eq!(
            aligned.items.len(),
            1,
            "an exactly aligned tail keeps its first record"
        );
        assert_eq!(aligned.items[0].id, "u2");
        assert!(
            read_transcript_tail(&home, &chat.id, 0)
                .unwrap()
                .items
                .is_empty()
        );
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn a_reader_follows_a_log_without_the_host_and_waits_for_a_line_that_is_not_whole() {
        let home = scratch();
        let mut chat = info(&home);
        chat.id = Uuid::new_v4().to_string();
        let dir = chat_dir(&home, &chat.id).unwrap();
        let log = ChatLog::create(&dir, &chat).unwrap();
        let mark = mark(&home, &chat.id).unwrap();
        assert_eq!(mark, 0);
        log.append(user_line(1, "one").as_bytes(), false).unwrap();
        // The host is in the middle of writing the second line.
        let second = user_line(2, "two");
        let (head, tail) = second.as_bytes().split_at(second.len() / 2);
        let mut events = OpenOptions::new()
            .append(true)
            .open(dir.join(EVENTS))
            .unwrap();
        events.write_all(head).unwrap();
        let (found, next) = read_after(&home, &chat.id, mark).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(next, user_line(1, "one").len() as u64);
        events.write_all(tail).unwrap();
        let (found, after) = read_after(&home, &chat.id, next).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].seq, 2);
        assert_eq!(after, (user_line(1, "one").len() + second.len()) as u64);
        assert!(read_after(&home, &chat.id, after).unwrap().0.is_empty());
        // The transcript is what the events build; an unfinished line is not in it.
        events.write_all(b"{\"chat_id\":\"c\",\"seq\":3").unwrap();
        let transcript = read_transcript(&home, &chat.id).unwrap();
        assert_eq!(transcript.items.len(), 2);
        assert!(read_transcript(&home, "../x").is_err());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn only_canonical_uuids_name_a_chat_directory() {
        let home = Path::new("/home");
        let id = Uuid::new_v4().to_string();
        assert_eq!(chat_dir(home, &id), Some(home.join("chats").join(&id)));
        for bad in ["", "..", "../x", &id.to_uppercase(), &id[..8], "a/b"] {
            assert_eq!(chat_dir(home, bad), None, "{bad:?}");
        }
    }
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;
    use crate::chat::model::{ItemBody, NoticeLevel};
    use serde_json::json;
    struct Fixture {
        home: PathBuf,
        id: String,
        dir: PathBuf,
        seq: u64,
    }
    impl Fixture {
        fn new() -> Self {
            let home = std::env::temp_dir().join(format!("riwork-snapshot-{}", Uuid::new_v4()));
            let id = Uuid::new_v4().to_string();
            let dir = chat_dir(&home, &id).unwrap();
            fs::create_dir_all(&dir).unwrap();
            File::create(dir.join(EVENTS)).unwrap();
            Self {
                home,
                id,
                dir,
                seq: 0,
            }
        }
        fn event(&mut self, event: serde_json::Value) {
            self.seq += 1;
            let line =
                serde_json::to_vec(&json!({"chat_id": self.id, "seq": self.seq, "event": event}))
                    .unwrap();
            let mut file = OpenOptions::new()
                .append(true)
                .open(self.dir.join(EVENTS))
                .unwrap();
            file.write_all(&line).unwrap();
            file.write_all(b"\n").unwrap();
        }
        fn row(&mut self, id: &str, text: &str) {
            self.event(json!({"event":"item_started","item":{"id":id,"status":"in_progress","body":{"type":"agent_message","text":text}}}));
        }
        fn read(&self, cursor: Option<&str>, before: u64, count: usize) -> Snapshot {
            read_snapshot(&self.home, &self.id, cursor, before, count, 120_000, &[]).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.home);
        }
    }
    #[test]
    fn a_notice_at_the_exact_tail_boundary_is_kept() {
        let mut f = Fixture::new();
        f.row("old", "before the tail");
        let start = fs::metadata(f.dir.join(EVENTS)).unwrap().len();
        f.event(
            json!({"event":"item_completed","item":{"id":"notice","status":"completed","body":{
            "type":"notice","level":"error","text":"needs attention","kind":"auth_required"}}}),
        );
        let length = fs::metadata(f.dir.join(EVENTS)).unwrap().len() - start;
        let tail = notice_tail(&f.dir, length);
        assert_eq!(tail.items.len(), 1);
        assert_eq!(tail.items[0].id, "notice");
        assert!(notice_tail(&f.dir, length - 1).items.is_empty());
        assert!(notice_tail(&f.dir, 0).items.is_empty());
    }

    #[test]
    fn snapshot_pages_keep_whole_turns_and_their_request_boundaries() {
        let mut f = Fixture::new();
        for turn in ["first", "second"] {
            f.event(json!({"event":"turn_started","turn_id":turn}));
            f.event(json!({"event":"approval_requested","approval":{
                "request_id":turn,"kind":"command","title":"npm test","choices":["accept"]
            }}));
            f.event(json!({"event":"approval_resolved","request_id":turn,"decision":"accept"}));
            for suffix in ["a", "b"] {
                f.event(json!({"event":"item_completed","item":{
                    "id":format!("{turn}-{suffix}"),"turn_id":turn,"status":"completed",
                    "body":{"type":"tool_call","tool":"Read","input":{"file_path":suffix}}
                }}));
            }
            f.event(
                json!({"event":"turn_completed","turn_id":turn,"outcome":{"outcome":"completed"}}),
            );
        }
        let newest = f.read(None, u64::MAX, 1);
        assert_eq!(
            newest.items.len(),
            2,
            "one-item preference extends to turn start"
        );
        assert_eq!(newest.before, 7);
        assert!(newest.more);
        assert!(!newest.provisional);
        assert_eq!(newest.requests.len(), 1);
        assert_eq!(newest.requests[0].request_id, "second");
        assert_eq!(newest.requests[0].requested_seq, 8);
        assert_eq!(newest.requests[0].resolved_seq, Some(9));
        assert_eq!(newest.requests[0].turn_id.as_deref(), Some("second"));
        assert_eq!(
            newest.items[0]
                .item
                .presentation
                .semantics
                .as_ref()
                .unwrap()
                .kind,
            super::super::model::ToolKind::Read
        );
        let older = f.read(Some(&newest.cursor), newest.before, 1);
        assert_eq!(older.items.len(), 2);
        assert_eq!(older.before, 1);
        assert!(!older.more);
        assert_eq!(older.requests.len(), 1);
        assert_eq!(older.requests[0].request_id, "first");
    }

    #[test]
    fn a_starting_driver_reads_only_the_notices_in_the_tail_of_a_large_log() {
        let mut f = Fixture::new();
        let notice = |id: &str, text: &str| {
            json!({"event":"item_completed","item":{"id":id,"status":"completed","body":{
                "type":"notice","level":"error","text":text,"kind":"auth_required"}}})
        };
        f.event(notice("early", "beyond the tail"));
        // About 8 MB of ordinary rows, four times the tail.
        let filler = "x".repeat(4000);
        for n in 0..2000 {
            f.row(&format!("row-{n}"), &filler);
        }
        f.event(notice("late", "in the tail"));
        f.row("after", "the last row");
        assert!(fs::metadata(f.dir.join(EVENTS)).unwrap().len() > 3 * NOTICE_TAIL);
        let tail = notice_tail(&f.dir, NOTICE_TAIL);
        let ids: Vec<_> = tail.items.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, ["late"], "only notices, only from the tail");
        let full = read_notice_transcript(&f.dir).unwrap();
        assert_eq!(
            full.items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["early", "late"],
            "dismissal must also find notices outside the driver resume tail"
        );
        // A missing log has none.
        assert!(
            notice_tail(&f.dir.join("missing"), NOTICE_TAIL)
                .items
                .is_empty()
        );
    }

    #[test]
    fn notice_kinds_survive_the_log_the_transcript_and_the_snapshot() {
        let mut f = Fixture::new();
        // As a driver writes it, and as a log from before `kind` has it.
        f.event(
            json!({"event":"item_completed","item":{"id":"n1","status":"completed","body":{
            "type":"notice","level":"warning","text":"close","kind":"rate_limit:seven_day",
            "resolved":true,"resets_at":1767225600}}}),
        );
        f.event(
            json!({"event":"item_completed","item":{"id":"n0","status":"completed","body":{
            "type":"notice","level":"error","text":"old"}}}),
        );
        let transcript = read_transcript(&f.home, &f.id).unwrap();
        assert_eq!(
            transcript.items[0].body,
            ItemBody::Notice {
                level: NoticeLevel::Warning,
                text: "close".into(),
                kind: Some("rate_limit:seven_day".into()),
                resolved: true,
                dismissed: false,
                resets_at: Some(1767225600),
            }
        );
        assert_eq!(
            transcript.items[1].body,
            ItemBody::notice(NoticeLevel::Error, "old", None)
        );
        // The phone gets what `riwork chat snapshot` prints; the relay passes it on as is.
        let snapshot = serde_json::to_value(f.read(None, u64::MAX, 10)).unwrap();
        let bodies: Vec<_> = snapshot["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["item"]["body"].clone())
            .collect();
        assert!(
            bodies.contains(&json!({"type":"notice","level":"warning","text":"close",
            "kind":"rate_limit:seven_day","resolved":true,"resets_at":1767225600}))
        );
        // Nothing new is written for a notice without them.
        assert!(bodies.contains(&json!({"type":"notice","level":"error","text":"old"})));
    }

    #[test]
    fn recent_full_items_controls_and_fixed_history_survive_live_writes() {
        let mut f = Fixture::new();
        f.event(json!({"event":"approval_requested","approval":{"request_id":"old-approval","kind":"command","title":"fixture","choices":["accept","decline"]}}));
        f.event(json!({"event":"question_requested","question":{"request_id":"old-question","questions":[]}}));
        f.event(json!({"event":"models","models":[]}));
        f.event(json!({"event":"usage","usage":{"input_tokens":123}}));
        for n in 0..8000 {
            f.row(&format!("row-{n}"), "start");
        }
        f.event(json!({"event":"item_delta","item_id":"row-7999","delta":{"kind":"text","text":"-complete"}}));
        f.event(json!({"event":"state","state":{"state":"waiting"}}));
        let recent = f.read(None, u64::MAX, 50);
        assert_eq!(recent.next, f.seq);
        assert_eq!(recent.items.len(), 50);
        assert_eq!(recent.items[0].item.id, "row-7950");
        assert!(
            serde_json::to_string(&recent.items[49].item)
                .unwrap()
                .contains("start-complete")
        );
        let controls = serde_json::to_value(&recent.controls).unwrap();
        assert!(controls.to_string().contains("old-approval"));
        assert!(controls.to_string().contains("old-question"));
        assert!(controls.to_string().contains("123"));
        assert!(controls.to_string().contains("waiting"));
        f.event(
            json!({"event":"approval_resolved","request_id":"old-approval","decision":"accept"}),
        );
        f.event(json!({"event":"item_delta","item_id":"row-7900","delta":{"kind":"text","text":"LIVE"}}));
        let old = f.read(Some(&recent.cursor), recent.before, 50);
        assert_eq!(old.next, recent.next);
        assert!(old.controls.is_empty());
        assert_eq!(old.items.last().unwrap().item.id, "row-7949");
        assert!(!serde_json::to_string(&old).unwrap().contains("LIVE"));
        let again = f.read(Some(&recent.cursor), recent.before, 50);
        assert_eq!(
            serde_json::to_value(old).unwrap(),
            serde_json::to_value(again).unwrap()
        );
        let targeted = read_snapshot(
            &f.home,
            &f.id,
            Some(&recent.cursor),
            u64::MAX,
            50,
            120_000,
            &["row-1".into()],
        )
        .unwrap();
        assert_eq!(targeted.items[0].item.id, "row-1");
        assert!(!targeted.more);
    }
    #[test]
    fn newest_200kb_and_aggregate_bases_or_controls_report_limits_without_partial_state() {
        let mut f = Fixture::new();
        f.row("newest", &"x".repeat(200_000));
        assert_eq!(
            read_snapshot(&f.home, &f.id, None, u64::MAX, 50, 120_000, &[])
                .err()
                .unwrap(),
            "snapshot response limit exceeded"
        );
        let mut f = Fixture::new();
        f.row("a", &"a".repeat(70_000));
        f.row("b", &"b".repeat(70_000));
        let initial = f.read(None, u64::MAX, 50);
        assert_eq!(initial.items.len(), 1);
        assert!(
            read_snapshot(
                &f.home,
                &f.id,
                Some(&initial.cursor),
                u64::MAX,
                50,
                120_000,
                &["a".into(), "b".into()]
            )
            .is_err()
        );
        let mut f = Fixture::new();
        for n in 0..200 {
            f.event(json!({"event":"approval_requested","approval":{"request_id":format!("request-{n}"),"kind":"command","title":"x".repeat(1000),"choices":["accept","decline"]}}));
        }
        assert_eq!(
            read_snapshot(&f.home, &f.id, None, u64::MAX, 50, 120_000, &[])
                .err()
                .unwrap(),
            "snapshot response limit exceeded"
        );
    }
    #[test]
    fn partial_tail_validation_replacement_and_resource_limits_fail_closed() {
        let mut f = Fixture::new();
        f.row("a", "hello");
        let before = f.read(None, u64::MAX, 50);
        OpenOptions::new()
            .append(true)
            .open(f.dir.join(EVENTS))
            .unwrap()
            .write_all(b"{partial")
            .unwrap();
        let after = f.read(None, u64::MAX, 50);
        assert_eq!(after.cursor, before.cursor);
        assert!(read_snapshot(&f.home, "../escape", None, u64::MAX, 50, 120_000, &[]).is_err());
        assert!(read_snapshot(&f.home, &f.id, None, u64::MAX, 101, 120_000, &[]).is_err());
        assert!(
            read_snapshot(
                &f.home,
                &f.id,
                Some(&before.cursor),
                u64::MAX,
                50,
                120_000,
                &[String::new()]
            )
            .is_err()
        );
        fs::write(f.dir.join(EVENTS), b"").unwrap();
        assert!(read_snapshot(&f.home, &f.id, Some(&before.cursor), 10, 50, 120_000, &[]).is_err());
        f.seq = 0;
        f.row("b", &"x".repeat(5000));
        assert!(read_snapshot(&f.home, &f.id, None, u64::MAX, 50, 4096, &[]).is_err());
        OpenOptions::new()
            .write(true)
            .open(f.dir.join(EVENTS))
            .unwrap()
            .set_len(SNAPSHOT_FILE_MAX + 1)
            .unwrap();
        assert!(read_snapshot(&f.home, &f.id, None, u64::MAX, 50, 120_000, &[]).is_err());
    }
}

/// Full notice history for account-wide dismissal, including notices older than the
/// bounded driver-resume tail. Stream past conversation bodies without retaining them.
pub(super) fn read_notice_transcript(dir: &Path) -> Result<super::model::Transcript, String> {
    let file = File::open(dir.join(EVENTS)).map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(file);
    let mut transcript = super::model::Transcript::default();
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader
            .read_until(b'\n', &mut line)
            .map_err(|e| e.to_string())?
            == 0
        {
            break;
        }
        if line.last() != Some(&b'\n') || !contains(&line, br#""type":"notice""#) {
            continue;
        }
        let envelope: Envelope = serde_json::from_slice(&line).map_err(|e| e.to_string())?;
        transcript.apply_sequenced(envelope.seq, &envelope.event);
    }
    Ok(transcript)
}

/// The worker markers in the log in `dir` (`workers`), in the order they were written; a
/// later line for the same id is the same marker's newer form. Only marker lines are
/// parsed; the rest of a long chat is skipped as bytes.
pub(super) fn read_worker_markers(dir: &Path) -> Result<Vec<super::model::Item>, String> {
    let file = match File::open(dir.join(EVENTS)) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.to_string()),
    };
    let mut reader = BufReader::new(file);
    let mut markers: Vec<super::model::Item> = Vec::new();
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader
            .read_until(b'\n', &mut line)
            .map_err(|e| e.to_string())?
            == 0
        {
            break;
        }
        if line.last() != Some(&b'\n') || !contains(&line, br#""tool":"worker""#) {
            continue;
        }
        let Ok(envelope) = serde_json::from_slice::<Envelope>(&line) else {
            continue;
        };
        if let ChatEvent::ItemStarted { item } | ChatEvent::ItemCompleted { item } = envelope.event
            && item.presentation.worker.is_some()
        {
            match markers.iter_mut().find(|marker| marker.id == item.id) {
                Some(marker) => *marker = item,
                None => markers.push(item),
            }
        }
    }
    Ok(markers)
}

#[cfg(test)]
mod rate_snapshot_tests {
    use super::*;

    #[test]
    fn a_worker_marker_is_not_the_provider_sessions_history() {
        use super::super::model::{ChatEvent, Item, ItemBody, ItemStatus};
        use super::super::workers::{WorkerKind, WorkerRef};
        let marker = super::super::workers::item(
            "worker:s".into(),
            None,
            WorkerRef {
                session_id: "s".into(),
                kind: WorkerKind::Shell,
                title: "t".into(),
                harness: None,
                model: None,
                worktree_id: None,
                assignment: None,
                noted_at: 1,
                after: None,
                late: false,
                follow_up: false,
                end: None,
            },
        );
        assert!(!is_session_history(&ChatEvent::ItemCompleted {
            item: marker.clone()
        }));
        let mut unreadable = marker.clone();
        unreadable.presentation = Default::default();
        assert!(!is_session_history(&ChatEvent::ItemCompleted {
            item: unreadable
        }));
        let mut tool = marker;
        tool.presentation = Default::default();
        tool.body = ItemBody::ToolCall {
            server: Some("riwork".into()),
            tool: "riwork_shell_list".into(),
            input: Default::default(),
            output: None,
        };
        assert!(is_session_history(&ChatEvent::ItemCompleted { item: tool }));
        let message = Item {
            started_at: None,
            ended_at: None,
            presentation: Default::default(),
            id: "m".into(),
            turn_id: None,
            status: ItemStatus::Completed,
            body: ItemBody::AgentMessage { text: "x".into() },
        };
        assert!(is_session_history(&ChatEvent::ItemStarted {
            item: message
        }));
    }

    #[test]
    fn worker_markers_are_read_back_newest_form_first_place() {
        let dir = std::env::temp_dir().join(format!("riwork-markers-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let worker = |end: bool| super::super::workers::WorkerRef {
            session_id: "s".into(),
            kind: super::super::workers::WorkerKind::Shell,
            title: "t".into(),
            harness: None,
            model: None,
            worktree_id: None,
            assignment: None,
            noted_at: 1,
            after: None,
            late: false,
            follow_up: false,
            end: end.then(|| super::super::workers::WorkerFinal {
                status: super::super::workers::WorkerEnd::Done,
                ended_at: 2,
                tail: String::new(),
                turn: None,
                turn_end: None,
                done_since: None,
            }),
        };
        let lines = [
            super::super::workers::item("worker:s".into(), None, worker(false)),
            super::super::workers::item("worker:s:2".into(), None, worker(false)),
            super::super::workers::item("worker:s".into(), None, worker(true)),
        ]
        .into_iter()
        .enumerate()
        .map(|(n, item)| {
            serde_json::to_string(&Envelope {
                chat_id: "c".into(),
                seq: n as u64 + 1,
                event: ChatEvent::ItemCompleted { item },
            })
            .unwrap()
                + "\n"
        })
        .collect::<String>();
        std::fs::write(dir.join(EVENTS), lines).unwrap();
        let markers = read_worker_markers(&dir).unwrap();
        assert_eq!(
            markers.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["worker:s", "worker:s:2"]
        );
        assert!(
            markers[0]
                .presentation
                .worker
                .as_ref()
                .unwrap()
                .end
                .is_some()
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn old_snapshot_without_dismissed_keys_or_rate_limits_still_decodes() {
        let snapshot: Snapshot = serde_json::from_value(serde_json::json!({
            "v":1,"chat_id":"old","cursor":"0-0-0000000000000000","next":0,
            "before":0,"more":false,"items":[],"controls":[]
        }))
        .unwrap();
        assert!(snapshot.dismissed_notices.is_empty());
        assert!(snapshot.turn_timing.is_empty());
        assert!(snapshot.turn_starts.is_empty());
        let mut transcript = super::super::model::Transcript::default();
        for event in snapshot.controls {
            transcript.apply(&event);
        }
        assert!(transcript.rate_limits.is_empty());
    }
}
