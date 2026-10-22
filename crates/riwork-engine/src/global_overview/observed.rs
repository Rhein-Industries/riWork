//! Bounded observations for the dashboard's decision inbox. Provider events supply every
//! actionable request; text summaries never create, resolve or answer one. A private cache
//! is invalidated by the exact log's inode, size and modification time on every read.
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::Path,
};

use serde::{Deserialize, Serialize};

use super::{Names, SessionSummary, chat_summary};
use crate::{
    chat::{
        model::{Approval, ChatEvent, ChatInfo, ChatState, ItemBody, Question, TurnOutcome},
        wire::Envelope,
    },
    store::State,
};

pub const MAX_DECISIONS: usize = 40;
pub const MAX_RECENT: usize = 40;
const MAX_CHATS: usize = 128;
const TAIL_BYTES: u64 = 2 * 1024 * 1024;
const TOTAL_BYTES: u64 = 12 * 1024 * 1024;
const CACHE_BYTES: u64 = 256 * 1024;
const REQUEST_BYTES: usize = 24 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub chat_id: String,
    pub event_seq: u64,
    pub event_id: String,
}
impl Source {
    fn new(chat_id: &str, event_seq: u64) -> Self {
        Self {
            chat_id: chat_id.into(),
            event_seq,
            event_id: format!("chat:{chat_id}:seq:{event_seq}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Request {
    Approval { approval: Approval },
    Question { question: Question },
}
impl Request {
    fn id(&self) -> &str {
        match self {
            Self::Approval { approval } => &approval.request_id,
            Self::Question { question } => &question.request_id,
        }
    }
    fn kind(&self) -> &'static str {
        match self {
            Self::Approval { .. } => "approval",
            Self::Question { .. } => "question",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Decision {
    pub id: String,
    pub chat_id: String,
    pub request_id: String,
    pub session: SessionSummary,
    pub source: Source,
    pub observed_at: u64,
    pub request: Request,
}

#[derive(Clone, Debug)]
pub struct Activity {
    pub session: SessionSummary,
    pub source: Source,
    pub observed_at: u64,
    pub kind: String,
    pub text: String,
    pub status: String,
    /// Log modification time, not an invented event timestamp.
    pub log_updated_at: u64,
}

#[derive(Default, Debug)]
pub struct Observations {
    pub sampled_at: u64,
    pub chats_total: usize,
    pub chats_read: usize,
    pub chats_complete: usize,
    pub decisions: Vec<Decision>,
    pub recent_activity: Vec<Activity>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Fingerprint {
    inode: u64,
    length: u64,
    seconds: i64,
    nanos: i64,
}
impl Fingerprint {
    fn from(meta: &fs::Metadata) -> Self {
        Self {
            inode: meta.ino(),
            length: meta.len(),
            seconds: meta.mtime(),
            nanos: meta.mtime_nsec(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Pending {
    source: Source,
    request: Request,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Recent {
    source: Source,
    kind: String,
    text: String,
    status: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Cached {
    v: u8,
    fingerprint: Fingerprint,
    complete: bool,
    pending: Vec<Pending>,
    recent: Option<Recent>,
    /// This exact log could not be folded (a malformed record, a gap, too many requests):
    /// it is not read again until it changes.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    unreadable: bool,
}

/// Observe existing logs only. Waiting chats are read first; failed and running chats next.
/// A truncated suffix may prove some pending requests, but is explicitly incomplete. The
/// absence of an old request in a bounded tail never claims that it has been resolved.
pub fn read(home: &Path, state: &State, chats: Option<&[ChatInfo]>, now: u64) -> Observations {
    if let Some(chats) = chats {
        prune_cache(home, chats);
    }
    let chats = chats.unwrap_or_default();
    let mut result = Observations {
        sampled_at: now,
        chats_total: chats.len(),
        ..Default::default()
    };
    let mut order: Vec<_> = chats.iter().collect();
    order.sort_by_key(|chat| {
        (
            match chat.state {
                ChatState::Waiting => 0,
                ChatState::Failed { .. } => 1,
                ChatState::Running => 2,
                ChatState::Starting => 3,
                _ => 4,
            },
            std::cmp::Reverse(chat.created_at_unix),
            &chat.id,
        )
    });
    let names = Names::new(state);
    let mut remaining = TOTAL_BYTES;
    for chat in order.into_iter().take(MAX_CHATS) {
        let Some(cached) = observe(home, &chat.id, &mut remaining) else {
            continue;
        };
        result.chats_read += 1;
        result.chats_complete += usize::from(cached.complete);
        let session = chat_summary(&names, chat);
        // A restored/stopped provider must never acquire actionable controls from its old
        // log. Request identities are still checked by the live provider when answered.
        if chat.state == ChatState::Waiting {
            for pending in cached.pending {
                let request_id = pending.request.id().to_owned();
                result.decisions.push(Decision {
                    id: format!("{}:{}:{}", chat.id, pending.request.kind(), request_id),
                    chat_id: chat.id.clone(),
                    request_id,
                    session: session.clone(),
                    source: pending.source,
                    observed_at: now,
                    request: pending.request,
                });
            }
        }
        if let Some(recent) = cached.recent {
            result.recent_activity.push(Activity {
                session,
                source: recent.source,
                observed_at: now,
                kind: recent.kind,
                text: recent.text,
                status: recent.status,
                log_updated_at: cached.fingerprint.seconds.max(0) as u64,
            });
        }
    }
    result.recent_activity.sort_by_key(|activity| {
        (
            std::cmp::Reverse(activity.log_updated_at),
            activity.source.chat_id.clone(),
        )
    });
    result
}

/// The event that last asked `request` in the chat's bounded log tail, if it is still open
/// there. A provider that restarted can ask a new request under an old id: answering a card
/// needs this to be the card's own event.
pub fn open_request(home: &Path, chat_id: &str, request: &Request) -> Option<Source> {
    let mut remaining = TAIL_BYTES;
    observe(home, chat_id, &mut remaining)?
        .pending
        .into_iter()
        .find(|pending| {
            pending.request.kind() == request.kind() && pending.request.id() == request.id()
        })
        .map(|pending| pending.source)
}

/// Drop the cached observations of chats that no longer exist: they hold the request
/// payloads and activity text of deleted conversations.
fn prune_cache(home: &Path, chats: &[ChatInfo]) {
    let dir = home.join("dashboard-observations");
    // As `write_cache`: never follow a link and delete files somewhere else.
    if !fs::symlink_metadata(&dir).is_ok_and(|meta| meta.file_type().is_dir()) {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if let Some(id) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.strip_suffix(".json"))
            && !chats.iter().any(|chat| chat.id == id)
        {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn private_file(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

fn observe(home: &Path, chat_id: &str, remaining: &mut u64) -> Option<Cached> {
    let dir = crate::chat::log::chat_dir(home, chat_id)?;
    let mut file = private_file(&dir.join("events.jsonl")).ok()?;
    let fingerprint = Fingerprint::from(&file.metadata().ok()?);
    let cache_dir = home.join("dashboard-observations");
    let cache_path = cache_dir.join(format!("{chat_id}.json"));
    if let Ok(cache) = private_file(&cache_path) {
        let mut bytes = Vec::new();
        if cache.take(CACHE_BYTES + 1).read_to_end(&mut bytes).is_ok()
            && bytes.len() as u64 <= CACHE_BYTES
        {
            if let Ok(cached) = serde_json::from_slice::<Cached>(&bytes) {
                if cached.v == 1 && cached.fingerprint == fingerprint {
                    return (!cached.unreadable).then_some(cached);
                }
            }
        }
    }
    let size = fingerprint.length.min(TAIL_BYTES).min(*remaining);
    if size == 0 && fingerprint.length != 0 {
        return None;
    }
    *remaining = remaining.saturating_sub(size);
    let start = fingerprint.length.saturating_sub(size);
    // A suffix also reads the byte before it: `fold` drops everything up to the first
    // newline, which is then only that byte when the suffix starts on a record boundary.
    let from = start.saturating_sub(1);
    file.seek(SeekFrom::Start(from)).ok()?;
    let mut bytes = Vec::with_capacity((size + start - from) as usize);
    Read::by_ref(&mut file)
        .take(size + start - from)
        .read_to_end(&mut bytes)
        .ok()?;
    // Never attach a snapshot to the fingerprint of a log that changed while read. The
    // records read are still this append-only log's, so they are used, just not cached.
    let unchanged = file
        .metadata()
        .is_ok_and(|meta| Fingerprint::from(&meta) == fingerprint);
    let cacheable = unchanged && size != 0 && size == fingerprint.length.min(TAIL_BYTES);
    let Some(cached) = fold(chat_id, fingerprint.clone(), &bytes, start == 0) else {
        if cacheable {
            let unreadable = Cached {
                v: 1,
                fingerprint,
                complete: false,
                pending: Vec::new(),
                recent: None,
                unreadable: true,
            };
            write_cache(&cache_dir, &cache_path, &unreadable);
        }
        return None;
    };
    if cacheable {
        write_cache(&cache_dir, &cache_path, &cached);
    }
    Some(cached)
}

fn write_cache(dir: &Path, path: &Path, cached: &Cached) {
    let Ok(bytes) = serde_json::to_vec(cached) else {
        return;
    };
    if bytes.len() as u64 > CACHE_BYTES {
        return;
    }
    if !dir.exists() && fs::DirBuilder::new().mode(0o700).create(dir).is_err() {
        return;
    }
    if !fs::symlink_metadata(dir)
        .is_ok_and(|meta| meta.file_type().is_dir() && !meta.file_type().is_symlink())
    {
        return;
    }
    let temporary = dir.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let result = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temporary)
        .and_then(|mut file| file.write_all(&bytes))
        .and_then(|_| fs::rename(&temporary, path));
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
}

fn fold(chat_id: &str, fingerprint: Fingerprint, bytes: &[u8], at_head: bool) -> Option<Cached> {
    let mut pending = BTreeMap::new();
    let mut recent: Option<Recent> = None;
    // The item `recent` describes, so a later record of the same item can update it.
    let mut recent_item: Option<String> = None;
    let mut complete = at_head;
    let mut previous = None;
    let mut lines = bytes.split_inclusive(|byte| *byte == b'\n');
    if !at_head {
        lines.next();
    }
    for line in lines {
        if line.last() != Some(&b'\n') {
            complete = false;
            break;
        }
        let envelope: Envelope = serde_json::from_slice(line).ok()?;
        if envelope.chat_id != chat_id
            || envelope.seq == 0
            || (at_head && previous.is_none() && envelope.seq != 1)
            || previous.is_some_and(|seq| envelope.seq != seq + 1)
        {
            return None;
        }
        previous = Some(envelope.seq);
        let source = Source::new(chat_id, envelope.seq);
        match envelope.event {
            ChatEvent::ApprovalRequested { approval } => {
                complete &= remember(&mut pending, source, Request::Approval { approval });
            }
            ChatEvent::QuestionRequested { question } => {
                complete &= remember(&mut pending, source, Request::Question { question });
            }
            ChatEvent::ApprovalResolved { request_id, .. } => {
                pending.remove(&("approval", request_id));
            }
            ChatEvent::QuestionResolved { request_id, .. } => {
                pending.remove(&("question", request_id));
            }
            ChatEvent::TurnStarted { .. } => {
                recent = None;
                recent_item = None;
            }
            ChatEvent::TurnCompleted { outcome, .. } => {
                pending.clear();
                complete = true;
                // Keep the concrete finding or final response, with its own source event,
                // rather than replacing useful evidence with a generic completion label.
                // Whatever the turn left open is over, as the transcript closes it.
                if outcome == TurnOutcome::Completed
                    && let Some(recent) = &mut recent
                {
                    if recent.status == "in_progress" {
                        recent.status = "completed".into();
                    }
                    continue;
                }
                recent_item = None;
                let (kind, text, status) = match outcome {
                    TurnOutcome::Completed => {
                        ("turn_completed", "Turn completed".into(), "completed")
                    }
                    TurnOutcome::Interrupted => {
                        ("turn_interrupted", "Turn interrupted".into(), "interrupted")
                    }
                    TurnOutcome::Failed { message } => {
                        ("turn_failed", compact(&message, 300), "failed")
                    }
                };
                recent = Some(Recent {
                    source,
                    kind: kind.into(),
                    text,
                    status: status.into(),
                });
            }
            ChatEvent::ItemStarted { item } | ChatEvent::ItemCompleted { item } => {
                let description = item
                    .presentation
                    .worker
                    .as_ref()
                    .map(|worker| {
                        let detail = worker
                            .assignment
                            .as_deref()
                            .filter(|text| !text.is_empty())
                            .unwrap_or(&worker.title);
                        ("worker", compact(detail, 300))
                    })
                    .or_else(|| {
                        item.presentation.subagent.as_ref().map(|subagent| {
                            let detail = subagent
                                .summary
                                .as_deref()
                                .or(subagent.purpose.as_deref())
                                .unwrap_or(&subagent.name);
                            (
                                "subagent",
                                compact(
                                    &format!(
                                        "{} · {} · {}",
                                        subagent.name,
                                        subagent.status.label(),
                                        detail
                                    ),
                                    300,
                                ),
                            )
                        })
                    })
                    .or_else(|| describe(&item.body))
                    .filter(|(_, text)| !text.is_empty());
                if let Some((kind, text)) = description {
                    let status = serde_json::to_value(item.status).ok()?.as_str()?.to_owned();
                    recent = Some(Recent {
                        source,
                        kind: kind.into(),
                        text,
                        status,
                    });
                    recent_item = Some(item.id);
                } else if let ItemBody::Notice {
                    text,
                    resolved,
                    dismissed,
                    ..
                } = &item.body
                    && recent_item.as_deref() == Some(item.id.as_str())
                    && let Some(recent) = &mut recent
                {
                    // The notice shown has gone: say so, in its resolution's words if any.
                    recent.source = source;
                    if *resolved {
                        recent.status = "resolved".into();
                        let text = compact(text, 300);
                        if !text.is_empty() {
                            recent.text = text;
                        }
                    } else if *dismissed {
                        recent.status = "dismissed".into();
                    }
                }
            }
            _ => {}
        }
        // Keep cache and response memory bounded even for hostile provider streams.
        if pending.len() > MAX_DECISIONS {
            return None;
        }
    }
    // In the order the provider asked, not by kind and request id.
    let mut pending: Vec<_> = pending.into_values().collect();
    pending.sort_by_key(|pending| pending.source.event_seq);
    Some(Cached {
        v: 1,
        fingerprint,
        complete,
        pending,
        recent,
        unreadable: false,
    })
}

/// Keep a request's latest payload. One too large to keep must not leave the request's
/// older payload answerable: it is dropped, and the log reads as incomplete.
fn remember(
    pending: &mut BTreeMap<(&'static str, String), Pending>,
    source: Source,
    request: Request,
) -> bool {
    let key = (request.kind(), request.id().to_owned());
    if valid_request(&request) {
        pending.insert(key, Pending { source, request });
        true
    } else {
        pending.remove(&key);
        false
    }
}

fn valid_request(request: &Request) -> bool {
    !request.id().is_empty()
        && request.id().len() <= 512
        && serde_json::to_vec(request).is_ok_and(|bytes| bytes.len() <= REQUEST_BYTES)
}

fn compact(text: &str, limit: usize) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(limit)
        .collect()
}
fn describe(body: &ItemBody) -> Option<(&'static str, String)> {
    let (kind, text) = match body {
        ItemBody::AgentMessage { text } if !text.trim().is_empty() => {
            ("message", compact(text, 300))
        }
        ItemBody::Command {
            command, exit_code, ..
        } => (
            "command",
            compact(
                &match exit_code {
                    Some(code) => format!("{command} (exit {code})"),
                    None => command.clone(),
                },
                300,
            ),
        ),
        ItemBody::FileChange { changes } => (
            "file_change",
            compact(
                &changes
                    .iter()
                    .map(|change| change.path.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                300,
            ),
        ),
        ItemBody::ToolCall { tool, .. } => ("tool", compact(tool, 300)),
        ItemBody::WebSearch { query } => ("web_search", compact(query, 300)),
        ItemBody::Plan { explanation, steps } => (
            "plan",
            compact(
                &explanation
                    .clone()
                    .filter(|explanation| !explanation.trim().is_empty())
                    .unwrap_or_else(|| {
                        steps
                            .iter()
                            .map(|step| step.text.as_str())
                            .collect::<Vec<_>>()
                            .join(" · ")
                    }),
                300,
            ),
        ),
        ItemBody::Notice {
            text,
            resolved: false,
            dismissed: false,
            ..
        } => ("notice", compact(text, 300)),
        _ => return None,
    };
    Some((kind, text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::{os::unix::fs::PermissionsExt, path::PathBuf};

    const CHAT: &str = "aaaaaaaa-1111-4111-8111-111111111111";
    fn event(seq: u64, event: Value) -> Vec<u8> {
        let mut result =
            serde_json::to_vec(&json!({"chat_id":CHAT,"seq":seq,"event":event})).unwrap();
        result.push(b'\n');
        result
    }
    fn approval(seq: u64, request: &str) -> Vec<u8> {
        event(
            seq,
            json!({"event":"approval_requested","approval":{"request_id":request,"kind":"command","title":"git push","detail":"Publish changes","choices":["accept","decline"]}}),
        )
    }
    fn question(seq: u64, request: &str) -> Vec<u8> {
        event(
            seq,
            json!({"event":"question_requested","question":{"request_id":request,"questions":[{"header":"Target","question":"Where?","options":[{"label":"Staging, exactly","description":"Private deployment"}],"multi_select":false}]}}),
        )
    }
    fn fingerprint() -> Fingerprint {
        Fingerprint {
            inode: 1,
            length: 10,
            seconds: 100,
            nanos: 2,
        }
    }
    fn folded(lines: &[Vec<u8>], at_head: bool) -> Cached {
        fold(CHAT, fingerprint(), &lines.concat(), at_head).unwrap()
    }
    fn chat(state: &str) -> ChatInfo {
        serde_json::from_value(json!({"id":CHAT,"provider":"claude","cwd":"/work","title":"Publish","created_at_unix":1,"state":{"state":state}})).unwrap()
    }
    struct Fixture {
        home: PathBuf,
    }
    impl Fixture {
        fn new(bytes: &[u8]) -> Self {
            let home = std::env::temp_dir().join(format!(
                "riwork-dashboard-observed-{}",
                uuid::Uuid::new_v4()
            ));
            let dir = home.join("chats").join(CHAT);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("events.jsonl"), bytes).unwrap();
            Self { home }
        }
        fn path(&self) -> PathBuf {
            self.home.join("chats").join(CHAT).join("events.jsonl")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.home);
        }
    }

    #[test]
    fn pending_controls_keep_exact_options_and_resolve_by_kind_and_identity() {
        let cached = folded(
            &[
                approval(1, "same"),
                question(2, "same"),
                event(
                    3,
                    json!({"event":"approval_resolved","request_id":"same","decision":"decline"}),
                ),
            ],
            true,
        );
        assert!(cached.complete);
        assert_eq!(cached.pending.len(), 1);
        let pending = &cached.pending[0];
        assert_eq!(pending.source.event_seq, 2);
        assert_eq!(pending.source.event_id, format!("chat:{CHAT}:seq:2"));
        let Request::Question { question } = &pending.request else {
            panic!("wrong request")
        };
        assert_eq!(question.questions[0].options[0].label, "Staging, exactly");
        assert_eq!(question.request_id, "same");
    }

    #[test]
    fn completion_closes_controls_and_proves_a_suffix_complete() {
        let cached = folded(
            &[
                b"cut prefix\n".to_vec(),
                approval(8, "old"),
                event(
                    9,
                    json!({"event":"turn_completed","turn_id":"t","outcome":{"outcome":"completed"}}),
                ),
                question(10, "new"),
            ],
            false,
        );
        assert!(cached.complete);
        assert_eq!(cached.pending.len(), 1);
        assert_eq!(cached.pending[0].request.id(), "new");
        assert_eq!(cached.recent.unwrap().kind, "turn_completed");
    }

    #[test]
    fn partial_tails_report_incompleteness_and_never_parse_a_partial_final_event() {
        let mut partial = approval(12, "not-complete");
        partial.pop();
        let cached = folded(
            &[b"cut prefix\n".to_vec(), question(11, "observed"), partial],
            false,
        );
        assert!(!cached.complete);
        assert_eq!(cached.pending.len(), 1);
        assert_eq!(cached.pending[0].request.id(), "observed");
    }

    #[test]
    fn malformed_and_cross_chat_events_never_create_actionable_requests() {
        assert!(
            fold(
                CHAT,
                fingerprint(),
                &[approval(1, "old"), b"bad\n".to_vec()].concat(),
                true
            )
            .is_none()
        );
        assert!(
            fold(
                CHAT,
                fingerprint(),
                &[approval(1, "old"), question(3, "gap")].concat(),
                true
            )
            .is_none()
        );
        let other = event(2, json!({"event":"state","state":{"state":"waiting"}}));
        assert!(fold("other", fingerprint(), &other, true).is_none());
    }

    #[test]
    fn stopped_chats_keep_evidence_without_exposing_old_actionable_requests() {
        let f = Fixture::new(&approval(1, "old"));
        let state = State::default();
        let waiting = read(&f.home, &state, Some(&[chat("waiting")]), 100);
        assert_eq!(waiting.decisions.len(), 1);
        assert_eq!(waiting.decisions[0].observed_at, 100);
        let stopped = read(&f.home, &state, Some(&[chat("stopped")]), 101);
        assert!(stopped.decisions.is_empty());
    }

    #[test]
    fn cache_revalidates_log_identity_and_unchanged_reads_spend_no_replay_budget() {
        let f = Fixture::new(&approval(1, "old"));
        let mut budget = TOTAL_BYTES;
        assert_eq!(
            observe(&f.home, CHAT, &mut budget).unwrap().pending.len(),
            1
        );
        assert!(budget < TOTAL_BYTES);
        let mut no_budget = 0;
        assert_eq!(
            observe(&f.home, CHAT, &mut no_budget)
                .unwrap()
                .pending
                .len(),
            1
        );
        let cache = f
            .home
            .join("dashboard-observations")
            .join(format!("{CHAT}.json"));
        assert_eq!(
            fs::metadata(&cache).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let replacement = f.path().with_extension("replacement");
        fs::write(
            &replacement,
            event(1, json!({"event":"state","state":{"state":"idle"}})),
        )
        .unwrap();
        fs::rename(replacement, f.path()).unwrap();
        assert!(observe(&f.home, CHAT, &mut no_budget).is_none());
        let mut budget = TOTAL_BYTES;
        assert!(
            observe(&f.home, CHAT, &mut budget)
                .unwrap()
                .pending
                .is_empty()
        );
    }

    #[test]
    fn oversized_requests_are_explicitly_incomplete_instead_of_truncated_answer_labels() {
        let oversized = event(
            1,
            json!({"event":"question_requested","question":{"request_id":"large","questions":[{"question":"Choose","options":[{"label":"x".repeat(REQUEST_BYTES),"description":""}],"multi_select":false}]}}),
        );
        let cached = folded(&[oversized], true);
        assert!(!cached.complete);
        assert!(cached.pending.is_empty());
    }

    #[test]
    fn a_budget_limited_read_does_not_cache_away_the_next_complete_read() {
        let f = Fixture::new(&approval(1, "pending"));
        let mut small = 8;
        assert!(!observe(&f.home, CHAT, &mut small).unwrap().complete);
        assert!(!f.home.join("dashboard-observations").exists());
        let mut full = TOTAL_BYTES;
        let observed = observe(&f.home, CHAT, &mut full).unwrap();
        assert!(observed.complete);
        assert_eq!(observed.pending.len(), 1);
    }

    #[test]
    fn a_suffix_that_starts_on_a_record_boundary_keeps_that_record() {
        let last = approval(2, "last");
        let f = Fixture::new(&[approval(1, "first"), last.clone()].concat());
        let mut exact = last.len() as u64;
        let observed = observe(&f.home, CHAT, &mut exact).unwrap();
        assert!(!observed.complete);
        assert_eq!(observed.pending.len(), 1);
        assert_eq!(observed.pending[0].request.id(), "last");
    }

    #[test]
    fn a_resolved_notice_and_a_finished_turn_close_the_current_activity() {
        let notice = |seq, event: &str, resolved: bool, text: &str| {
            self::event(
                seq,
                json!({"event":event,"item":{"id":"notice-1","status":"completed","body":{"type":"notice","level":"warning","text":text,"kind":"auth_required","resolved":resolved}}}),
            )
        };
        let open = notice(1, "item_started", false, "Sign in to Claude");
        let cached = folded(
            &[
                open.clone(),
                notice(2, "item_completed", true, "Claude is signed in again."),
            ],
            true,
        );
        let recent = cached.recent.unwrap();
        assert_eq!(
            (recent.status.as_str(), recent.text.as_str()),
            ("resolved", "Claude is signed in again.")
        );
        assert_eq!(recent.source.event_seq, 2);
        // Resolved without new words (a limit reset, a provider switch): the words stay.
        let cached = folded(
            &[open, notice(2, "item_completed", true, "Sign in to Claude")],
            true,
        );
        let recent = cached.recent.unwrap();
        assert_eq!(
            (recent.status.as_str(), recent.text.as_str()),
            ("resolved", "Sign in to Claude")
        );
        // An item a completed turn left open is over, and empty words never replace a row.
        let cached = folded(
            &[
                event(
                    1,
                    json!({"event":"item_started","item":{"id":"m","status":"in_progress","body":{"type":"agent_message","text":"Checking the build"}}}),
                ),
                event(
                    2,
                    json!({"event":"item_completed","item":{"id":"p","status":"completed","body":{"type":"file_change","changes":[]}}}),
                ),
                event(
                    3,
                    json!({"event":"turn_completed","turn_id":"t","outcome":{"outcome":"completed"}}),
                ),
            ],
            true,
        );
        let recent = cached.recent.unwrap();
        assert_eq!(
            (recent.status.as_str(), recent.text.as_str()),
            ("completed", "Checking the build")
        );
        let plan = folded(
            &[event(
                1,
                json!({"event":"item_completed","item":{"id":"p","status":"completed","body":{"type":"plan","explanation":"","steps":[{"text":"Run tests"}]}}}),
            )],
            true,
        );
        assert_eq!(plan.recent.unwrap().text, "Run tests");
    }

    #[test]
    fn deleted_chats_lose_their_cached_observations() {
        let f = Fixture::new(&approval(1, "old"));
        let state = State::default();
        let cache = f
            .home
            .join("dashboard-observations")
            .join(format!("{CHAT}.json"));
        read(&f.home, &state, Some(&[chat("waiting")]), 100);
        assert!(cache.exists());
        read(&f.home, &state, None, 101);
        assert!(cache.exists(), "an unknown chat list removes nothing");
        read(&f.home, &state, Some(&[]), 102);
        assert!(!cache.exists());
    }

    #[test]
    fn large_decision_cards_stop_at_the_reply_budget_and_keep_their_total() {
        let large = |seq, id: &str| {
            event(
                seq,
                json!({"event":"question_requested","question":{"request_id":id,"questions":[{"question":"Choose","options":[{"label":"x".repeat(20 * 1024),"description":""}],"multi_select":false}]}}),
            )
        };
        let f =
            Fixture::new(&[large(1, "a"), large(2, "b"), large(3, "c"), large(4, "d")].concat());
        let state = State::default();
        let chats = vec![chat("waiting")];
        let observed = read(&f.home, &state, Some(&chats), 100);
        assert_eq!(observed.decisions.len(), 4);
        let overview = super::super::build(&super::super::Inputs {
            state: &state,
            shells: &[],
            cwds: &BTreeMap::new(),
            activity: &BTreeMap::new(),
            chats: Some(&chats),
            chat_activity: &BTreeMap::new(),
            schedules: None,
            backends: None,
            now: 100,
        });
        let wire = super::super::wire_with_observations(&overview, 100, &observed);
        assert_eq!(wire["decisions_total"], 4);
        assert_eq!(wire["decisions"].as_array().unwrap().len(), 3);
        assert!(
            serde_json::to_vec(&wire["decisions"]).unwrap().len()
                <= super::super::wire::WIRE_DECISION_BYTES + 8
        );
    }

    #[test]
    fn the_whole_reply_stays_under_its_budget_and_activity_is_cut_before_cards() {
        let large = |seq, id: &str| {
            event(
                seq,
                json!({"event":"question_requested","question":{"request_id":id,"questions":[{"question":"Choose","options":[{"label":"x".repeat(20 * 1024),"description":""}],"multi_select":false}]}}),
            )
        };
        let f =
            Fixture::new(&[large(1, "a"), large(2, "b"), large(3, "c"), large(4, "d")].concat());
        let state = State::default();
        let chats = vec![chat("waiting")];
        let mut observed = read(&f.home, &state, Some(&chats), 100);
        let session = observed.decisions[0].session.clone();
        // Every activity row full, in two-byte characters.
        observed.recent_activity = (0..MAX_RECENT as u64)
            .map(|seq| Activity {
                session: session.clone(),
                source: Source::new(CHAT, seq + 10),
                observed_at: 100,
                kind: "message".into(),
                text: "é".repeat(300),
                status: "completed".into(),
                log_updated_at: 90,
            })
            .collect();
        let overview = super::super::build(&super::super::Inputs {
            state: &state,
            shells: &[],
            cwds: &BTreeMap::new(),
            activity: &BTreeMap::new(),
            chats: Some(&chats),
            chat_activity: &BTreeMap::new(),
            schedules: None,
            backends: None,
            now: 100,
        });
        let wire = super::super::wire_with_observations(&overview, 100, &observed);
        assert!(serde_json::to_vec(&wire).unwrap().len() <= super::super::wire::WIRE_REPLY_BYTES);
        assert_eq!(wire["decisions"].as_array().unwrap().len(), 3);
        assert_eq!(wire["recent_activity_total"], MAX_RECENT);
        let activity = wire["recent_activity"].as_array().unwrap().len();
        assert!((1..MAX_RECENT).contains(&activity), "{activity}");
    }

    #[test]
    fn a_reasked_request_too_large_to_keep_drops_its_older_payload_and_cards_keep_ask_order() {
        let oversized = event(
            3,
            json!({"event":"approval_requested","approval":{"request_id":"r","kind":"command","title":"x".repeat(REQUEST_BYTES),"detail":"","choices":["accept"]}}),
        );
        let cached = folded(&[question(1, "q"), approval(2, "r"), oversized], true);
        assert!(!cached.complete);
        assert_eq!(cached.pending.len(), 1);
        assert_eq!(cached.pending[0].request.id(), "q");
        // Approvals sort before questions by key; the provider asked the question first.
        let cached = folded(
            &[question(1, "b"), approval(2, "a"), question(3, "10")],
            true,
        );
        let order: Vec<_> = cached.pending.iter().map(|p| p.source.event_seq).collect();
        assert_eq!(order, [1, 2, 3]);
    }

    #[test]
    fn a_request_asked_again_under_its_id_is_found_at_its_new_event() {
        let request = folded(&[approval(1, "r")], true).pending[0].request.clone();
        let f = Fixture::new(&[approval(1, "r"), approval(2, "r")].concat());
        let asked = open_request(&f.home, CHAT, &request).unwrap();
        assert_eq!(asked.event_seq, 2);
        let other = folded(&[approval(1, "other")], true).pending[0]
            .request
            .clone();
        assert!(open_request(&f.home, CHAT, &other).is_none());
    }

    #[test]
    fn an_unreadable_log_is_not_replayed_until_it_changes() {
        let f = Fixture::new(&[approval(1, "a"), b"not json\n".to_vec()].concat());
        let state = State::default();
        let chats = vec![chat("waiting")];
        let observed = read(&f.home, &state, Some(&chats), 100);
        assert_eq!(observed.chats_read, 0);
        let cache = f
            .home
            .join("dashboard-observations")
            .join(format!("{CHAT}.json"));
        let cached: Cached = serde_json::from_slice(&fs::read(&cache).unwrap()).unwrap();
        assert!(cached.unreadable);
        // A rewritten log is read again.
        fs::write(f.path(), approval(1, "a")).unwrap();
        let observed = read(&f.home, &state, Some(&chats), 101);
        assert_eq!(observed.chats_read, 1);
        assert_eq!(observed.decisions.len(), 1);
    }

    #[test]
    fn the_additive_wire_exposes_exact_controls_and_coverage_with_event_sources() {
        let f = Fixture::new(&[approval(1, "approval"), question(2, "question")].concat());
        let state = State::default();
        let chats = vec![chat("waiting")];
        let observed = read(&f.home, &state, Some(&chats), 100);
        let overview = super::super::build(&super::super::Inputs {
            state: &state,
            shells: &[],
            cwds: &BTreeMap::new(),
            activity: &BTreeMap::new(),
            chats: Some(&chats),
            chat_activity: &BTreeMap::new(),
            schedules: None,
            backends: None,
            now: 100,
        });
        let wire = super::super::wire_with_observations(&overview, 100, &observed);
        assert_eq!(wire["v"], 1);
        assert_eq!(wire["decisions_total"], 2);
        assert_eq!(wire["decision_sources"]["state"], "read");
        assert_eq!(
            wire["decisions"][0]["approval"]["choices"],
            json!(["accept", "decline"])
        );
        assert_eq!(
            wire["decisions"][1]["question"]["questions"][0]["options"][0]["label"],
            "Staging, exactly"
        );
        assert_eq!(
            wire["decisions"][1]["source"]["event_id"],
            format!("chat:{CHAT}:seq:2")
        );
        assert_eq!(wire["decisions"][1]["session"]["target"]["id"], CHAT);
    }
}
