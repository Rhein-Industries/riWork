//! Codex activity from exact pane/thread bindings and structured lifecycle events.
//! No CPU, terminal output, prompts, tool results, or message bodies are used.

use std::{
    collections::BTreeMap,
    env, fs,
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize, de::IgnoredAny};
use uuid::Uuid;

use crate::{
    sessions::{HarnessKind, ShellSession},
    store::State,
};

pub(crate) const MAX_POLL_BYTES: usize = 8 * 1024 * 1024;
const MAX_RECORD_BYTES: usize = 32 * 1024 * 1024;
const MAX_BINDING_BYTES: u64 = 16 * 1024;
/// A large rollout is opened from its first record plus the smallest tail that
/// contains a turn start, so opening it never costs a scan from byte zero.
const SEED_WINDOWS: [u64; 4] = [1 << 20, 4 << 20, 16 << 20, 64 << 20];
const LOOKUP_BACKOFF_CAP: Duration = Duration::from_secs(120);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AgentActivity {
    Working,
    Done,
    Waiting,
    #[default]
    Unknown,
    Exited,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActivityCounts {
    pub working: usize,
    pub done: usize,
    pub waiting: usize,
    pub unknown: usize,
    pub exited: usize,
}

impl ActivityCounts {
    fn add(&mut self, activity: AgentActivity) {
        match activity {
            AgentActivity::Working => self.working += 1,
            AgentActivity::Done => self.done += 1,
            AgentActivity::Waiting => self.waiting += 1,
            AgentActivity::Unknown => self.unknown += 1,
            AgentActivity::Exited => self.exited += 1,
        }
    }

    pub fn for_project(
        project_id: &str,
        shells: &[ShellSession],
        activity: &BTreeMap<String, AgentActivity>,
    ) -> Self {
        let mut counts = Self::default();
        for shell in shells
            .iter()
            .filter(|shell| shell.project_id.as_deref() == Some(project_id))
        {
            if let Some(status) = activity.get(&shell.id) {
                counts.add(*status);
            }
        }
        counts
    }

    pub fn for_worktree(
        worktree_id: &str,
        state: &State,
        shells: &[ShellSession],
        cwds: &BTreeMap<String, PathBuf>,
        activity: &BTreeMap<String, AgentActivity>,
    ) -> Self {
        let mut counts = Self::default();
        let Some(target) = state
            .worktrees
            .iter()
            .find(|worktree| worktree.id == worktree_id)
        else {
            return counts;
        };
        for shell in shells
            .iter()
            .filter(|shell| shell.project_id.as_deref() == Some(&target.project_id))
        {
            let current = cwds
                .get(&shell.id)
                .and_then(|cwd| {
                    state
                        .worktrees
                        .iter()
                        .filter(|worktree| {
                            worktree.project_id == target.project_id
                                && cwd.starts_with(&worktree.path)
                        })
                        .max_by_key(|worktree| worktree.path.as_os_str().len())
                        .map(|worktree| worktree.id.as_str())
                })
                .or(shell.worktree_id.as_deref());
            if current == Some(worktree_id)
                && let Some(status) = activity.get(&shell.id)
            {
                counts.add(*status);
            }
        }
        counts
    }

    /// Unknown sessions stay neutral; plain shells never acquire a done label.
    pub fn summary(&self) -> Option<String> {
        let mut labels = Vec::new();
        if self.working > 0 {
            labels.push(format!("● {} working", self.working));
        }
        if self.done > 0 {
            labels.push(format!("✓ {} done", self.done));
        }
        if self.waiting > 0 {
            labels.push(format!("◌ {} waiting", self.waiting));
        }
        if self.unknown > 0 {
            labels.push(format!("? {} unknown", self.unknown));
        }
        (!labels.is_empty()).then(|| labels.join(" · "))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct Binding {
    shell_id: String,
    thread_id: String,
    codex_home: PathBuf,
}

fn valid_uuid(value: &str) -> bool {
    Uuid::parse_str(value).is_ok_and(|id| id.to_string() == value)
}

fn codex_home() -> Option<PathBuf> {
    env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
}

/// Used only when a launch notification or authenticated thread environment
/// identifies the exact shell and thread. Stores identifiers, never content.
pub fn bind_codex_thread(
    home: &Path,
    shell_id: &str,
    thread_id: &str,
    log_home: &Path,
) -> Result<(), String> {
    if !valid_uuid(shell_id) || !valid_uuid(thread_id) {
        return Err("Activity binding requires exact canonical shell and thread UUIDs".into());
    }
    let binding = Binding {
        shell_id: shell_id.into(),
        thread_id: thread_id.into(),
        codex_home: log_home.to_path_buf(),
    };
    let directory = home.join("agent-activity");
    fs::create_dir_all(&directory)
        .map_err(|error| format!("Cannot create activity bindings: {error}"))?;
    let path = directory.join(format!("{shell_id}.json"));
    let temporary = directory.join(format!(".{}.tmp", Uuid::new_v4()));
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
            .map_err(|error| error.to_string())?;
        serde_json::to_writer(&mut file, &binding).map_err(|error| error.to_string())?;
        file.write_all(b"\n").map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        fs::rename(&temporary, path).map_err(|error| error.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[derive(Deserialize)]
struct Notification {
    #[serde(rename = "type")]
    kind: String,
    #[serde(rename = "thread-id")]
    thread_id: String,
    #[serde(rename = "turn-id", default)]
    turn_id: Option<String>,
}

pub fn record_codex_notification(home: &Path, shell_id: &str, input: &str) -> Result<(), String> {
    if input.len() > MAX_RECORD_BYTES {
        return Err("Codex notification exceeds the activity limit".into());
    }
    let notification: Notification =
        serde_json::from_str(input).map_err(|_| "Invalid Codex activity notification")?;
    if notification.kind != "agent-turn-complete" {
        return Ok(());
    }
    let log_home = codex_home().ok_or("Cannot locate the Codex log directory")?;
    record_codex_notification_at(home, shell_id, notification, &log_home)
}

fn record_codex_notification_at(
    home: &Path,
    shell_id: &str,
    notification: Notification,
    log_home: &Path,
) -> Result<(), String> {
    // A delegated agent must never replace its parent's pane binding.
    let path = find_rollout(log_home, &notification.thread_id)
        .ok_or("Cannot identify this exact Codex thread")?;
    let mut cursor = RolloutCursor::new(notification.thread_id.clone());
    let status = cursor.read(&path)?;
    if !cursor.valid_session {
        return Ok(());
    }
    bind_codex_thread(home, shell_id, &notification.thread_id, log_home)?;
    if home.join("sessions.json").is_file() {
        crate::sessions::SessionManager::at(home.to_path_buf())?
            .freeze_codex_home_if_unknown(shell_id, log_home)?;
    }
    if let Some(turn) = notified_turn(&cursor, status, notification.turn_id)
        && home.join("sessions.json").is_file()
    {
        let event_id = codex_completion_event_id(log_home, &notification.thread_id, &turn);
        crate::notifications::record_completion(home, shell_id, &event_id, HarnessKind::Codex)?;
    }
    Ok(())
}

/// The turn to alert for. A rollout that shows a different completed turn or a
/// newer running one vetoes the payload. When the rollout cannot be read
/// conclusively (a damaged or oversized record inside the turn, or a log too
/// large to place within one read), the ids Codex itself put in the payload are
/// trusted once `session_meta` has been checked.
fn notified_turn(
    cursor: &RolloutCursor,
    status: AgentActivity,
    payload_turn: Option<String>,
) -> Option<String> {
    match status {
        AgentActivity::Done => {
            let turn = cursor.completed_turn.clone()?;
            payload_turn
                .is_none_or(|expected| expected == turn)
                .then_some(turn)
        }
        AgentActivity::Unknown => payload_turn
            .filter(|turn| (1..=128).contains(&turn.len()) && !turn.chars().any(char::is_control)),
        _ => None,
    }
}

fn codex_completion_event_id(log_home: &Path, thread_id: &str, turn_id: &str) -> String {
    serde_json::to_string(&("codex", log_home.to_string_lossy(), thread_id, turn_id))
        .expect("serializing identifiers cannot fail")
}

#[derive(Clone, Debug)]
pub struct CodexCompletion {
    pub shell_id: String,
    pub event_id: String,
}

pub struct ActivityTracker {
    home: PathBuf,
    default_codex_home: Option<PathBuf>,
    cursors: BTreeMap<String, BoundCursor>,
    completions: Vec<CodexCompletion>,
}

struct BoundCursor {
    binding: Binding,
    path: Option<PathBuf>,
    rollout: RolloutCursor,
    primed: bool,
    last_completed_turn: Option<String>,
    /// A missing rollout is not searched for again until this instant.
    lookup_after: Option<Instant>,
    lookup_misses: u32,
}

impl BoundCursor {
    fn new(binding: Binding) -> Self {
        Self {
            rollout: RolloutCursor::new(binding.thread_id.clone()),
            binding,
            path: None,
            primed: false,
            last_completed_turn: None,
            lookup_after: None,
            lookup_misses: 0,
        }
    }

    /// Walking the sessions tree is the expensive part of a miss (it is also
    /// capped, so a very large tree never resolves), hence the exponential wait.
    fn missed_lookup(&mut self) {
        self.lookup_misses = self.lookup_misses.saturating_add(1);
        let wait =
            Duration::from_secs(2u64 << (self.lookup_misses - 1).min(6)).min(LOOKUP_BACKOFF_CAP);
        self.lookup_after = Some(Instant::now() + wait);
    }
}

impl ActivityTracker {
    pub fn at(home: PathBuf) -> Self {
        Self {
            home,
            default_codex_home: codex_home(),
            cursors: BTreeMap::new(),
            completions: Vec::new(),
        }
    }

    /// Synchronous bounded I/O; call this from the background executor.
    pub fn sample(&mut self, shells: &[ShellSession]) -> BTreeMap<String, AgentActivity> {
        self.cursors
            .retain(|id, _| shells.iter().any(|shell| &shell.id == id));
        let mut result = BTreeMap::new();
        for shell in shells {
            let binding = self.read_binding(&shell.id).or_else(|| {
                let thread_id = resume_thread(shell)?;
                Some(Binding {
                    shell_id: shell.id.clone(),
                    thread_id,
                    codex_home: shell
                        .codex_home
                        .clone()
                        .or_else(|| self.default_codex_home.clone())?,
                })
            });
            if binding.is_none() && shell.harness != Some(HarnessKind::Codex) {
                continue;
            }
            if !shell.alive {
                result.insert(shell.id.clone(), AgentActivity::Exited);
                continue;
            }
            let Some(binding) = binding else {
                result.insert(shell.id.clone(), AgentActivity::Unknown);
                continue;
            };
            let cursor = self
                .cursors
                .entry(shell.id.clone())
                .or_insert_with(|| BoundCursor::new(binding.clone()));
            if cursor.binding != binding {
                *cursor = BoundCursor::new(binding.clone());
            }
            if cursor.path.is_none() && cursor.lookup_after.is_none_or(|at| Instant::now() >= at) {
                cursor.path = find_rollout(&binding.codex_home, &binding.thread_id);
                if cursor.path.is_none() {
                    cursor.missed_lookup();
                }
            }
            let status = match cursor.path.as_ref().map(|path| cursor.rollout.read(path)) {
                Some(Ok(status)) => {
                    cursor.lookup_misses = 0;
                    status
                }
                Some(Err(_)) => {
                    // The file moved or vanished: resolve it again, but not on
                    // every poll.
                    cursor.path = None;
                    cursor.missed_lookup();
                    AgentActivity::Unknown
                }
                None => AgentActivity::Unknown,
            };
            if cursor.rollout.restarted {
                cursor.primed = false;
                cursor.last_completed_turn = None;
            }
            if cursor.rollout.caught_up && cursor.rollout.valid_session {
                if cursor.primed && status == AgentActivity::Done {
                    if let Some(turn) = &cursor.rollout.completed_turn {
                        if cursor.last_completed_turn.as_ref() != Some(turn) {
                            self.completions.push(CodexCompletion {
                                shell_id: shell.id.clone(),
                                event_id: codex_completion_event_id(
                                    &binding.codex_home,
                                    &binding.thread_id,
                                    turn,
                                ),
                            });
                        }
                    }
                }
                // First EOF establishes a baseline, including any historical
                // Done. Backlogged initial reads never produce startup alerts.
                cursor.primed = true;
                cursor.last_completed_turn = cursor.rollout.completed_turn.clone();
            }
            result.insert(shell.id.clone(), status);
        }
        result
    }

    pub fn take_completions(&mut self) -> Vec<CodexCompletion> {
        std::mem::take(&mut self.completions)
    }

    /// Bind only a rollout proven open by the selected pane's own Codex PID.
    /// This preserves custom notification hooks; no directory-wide discovery.
    pub fn bind_schedule_rollout(
        &self,
        shell: &ShellSession,
        path: &Path,
    ) -> Result<String, String> {
        let log_home = shell
            .codex_home
            .as_ref()
            .ok_or("Codex account home is unknown")?;
        if !path.starts_with(log_home.join("sessions")) {
            return Err("Rollout is outside the pinned Codex home".into());
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("Invalid rollout filename")?;
        let stem = name
            .strip_suffix(".jsonl")
            .ok_or("Invalid rollout filename")?;
        let thread = stem
            .get(stem.len().saturating_sub(36)..)
            .filter(|id| valid_uuid(id))
            .ok_or("Invalid rollout thread UUID")?;
        let mut cursor = RolloutCursor::new(thread.into());
        cursor.read(path)?;
        if !cursor.valid_session {
            return Err("Rollout is not a primary interactive Codex session".into());
        }
        bind_codex_thread(&self.home, &shell.id, thread, log_home)?;
        Ok(thread.into())
    }

    pub fn schedule_identity(&self, shell: &ShellSession) -> Option<String> {
        self.read_binding(&shell.id)
            .map(|b| b.thread_id)
            .or_else(|| resume_thread(shell))
    }

    pub fn schedule_idle_token(
        &mut self,
        shell: &ShellSession,
        expected_session: &str,
    ) -> Option<String> {
        if self.sample(std::slice::from_ref(shell)).get(&shell.id) != Some(&AgentActivity::Done) {
            return None;
        }
        self.completions.clear();
        let cursor = self.cursors.get(&shell.id)?;
        (cursor.binding.thread_id == expected_session
            && cursor.rollout.caught_up
            && cursor.rollout.valid_session)
            .then(|| cursor.rollout.completed_turn.clone())
            .flatten()
    }

    fn read_binding(&self, shell_id: &str) -> Option<Binding> {
        if !valid_uuid(shell_id) {
            return None;
        }
        let mut file = File::open(
            self.home
                .join("agent-activity")
                .join(format!("{shell_id}.json")),
        )
        .ok()?;
        if file.metadata().ok()?.len() > MAX_BINDING_BYTES {
            return None;
        }
        let mut data = Vec::new();
        file.read_to_end(&mut data).ok()?;
        let binding: Binding = serde_json::from_slice(&data).ok()?;
        (binding.shell_id == shell_id && valid_uuid(&binding.thread_id)).then_some(binding)
    }
}

// Saved launch argv is parsed, never evaluated as shell code. A CWD match is
// deliberately insufficient: multiple panes and agents often share a folder.
fn resume_thread(shell: &ShellSession) -> Option<String> {
    if shell.harness != Some(HarnessKind::Codex) {
        return None;
    }
    let command = shell.command.as_deref()?;
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut escaped = false;
    for character in command.chars() {
        if escaped {
            word.push(character);
            escaped = false;
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if let Some(delimiter) = quote {
            if character == delimiter {
                quote = None;
            } else {
                word.push(character);
            }
        } else if character == '\'' || character == '"' {
            quote = Some(character);
        } else if character.is_whitespace() {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        } else if matches!(character, ';' | '|' | '&' | '`' | '$') {
            return None;
        } else {
            word.push(character);
        }
    }
    if quote.is_some() || escaped {
        return None;
    }
    if !word.is_empty() {
        words.push(word);
    }
    let index = words.iter().position(|word| {
        Path::new(word)
            .file_name()
            .is_some_and(|name| name == "codex")
    })?;
    let mut ids = words[index + 1..]
        .windows(2)
        .filter(|pair| pair[0] == "resume" && valid_uuid(&pair[1]))
        .map(|pair| pair[1].clone());
    let id = ids.next()?;
    ids.next().is_none().then_some(id)
}

fn find_rollout(home: &Path, thread_id: &str) -> Option<PathBuf> {
    if !valid_uuid(thread_id) {
        return None;
    }
    let suffix = format!("-{thread_id}.jsonl");
    let mut pending = vec![(home.join("sessions"), 0)];
    let mut matched = None;
    let mut inspected = 0;
    while let Some((directory, depth)) = pending.pop() {
        for entry in fs::read_dir(directory).ok()? {
            inspected += 1;
            if inspected > 20_000 {
                return None;
            }
            let entry = entry.ok()?;
            let kind = entry.file_type().ok()?;
            if kind.is_dir() && depth < 4 {
                pending.push((entry.path(), depth + 1));
            } else if kind.is_file() && entry.file_name().to_string_lossy().ends_with(&suffix) {
                if matched.is_some() {
                    return None;
                }
                matched = Some(entry.path());
            }
        }
    }
    matched
}

#[derive(Deserialize)]
struct LogRecord {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    payload: LogPayload,
}

#[derive(Default, Deserialize)]
struct LogPayload {
    #[serde(rename = "type", default)]
    kind: String,
    id: Option<String>,
    turn_id: Option<String>,
    source: Option<LogSource>,
    parent_thread_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum LogSource {
    Name(String),
    Details { subagent: Option<IgnoredAny> },
}

#[derive(Clone)]
struct RolloutCursor {
    thread_id: String,
    offset: u64,
    partial: Vec<u8>,
    skipping: bool,
    identity: Option<(u64, u64)>,
    valid_session: bool,
    malformed: bool,
    active_turn: Option<String>,
    completed_turn: Option<String>,
    caught_up: bool,
    restarted: bool,
    status: AgentActivity,
}

impl RolloutCursor {
    fn new(thread_id: String) -> Self {
        Self {
            thread_id,
            offset: 0,
            partial: Vec::new(),
            skipping: false,
            identity: None,
            valid_session: false,
            malformed: false,
            active_turn: None,
            completed_turn: None,
            caught_up: false,
            restarted: false,
            status: AgentActivity::Unknown,
        }
    }

    fn read(&mut self, path: &Path) -> Result<AgentActivity, String> {
        self.read_with(path, &SEED_WINDOWS)
    }

    fn read_with(&mut self, path: &Path, windows: &[u64]) -> Result<AgentActivity, String> {
        self.restarted = false;
        let mut file = File::open(path).map_err(|error| error.to_string())?;
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        #[cfg(unix)]
        let identity = {
            use std::os::unix::fs::MetadataExt;
            (metadata.dev(), metadata.ino())
        };
        #[cfg(not(unix))]
        let identity = (0, 0);
        if metadata.len() < self.offset || self.identity.is_some_and(|old| old != identity) {
            *self = Self::new(self.thread_id.clone());
            self.restarted = true;
        }
        self.identity = Some(identity);
        if self.offset == 0
            && metadata.len() > MAX_POLL_BYTES as u64
            && let Some(mut seeded) = self.seeded(&mut file, metadata.len(), windows)?
        {
            seeded.identity = self.identity;
            seeded.restarted = self.restarted;
            *self = seeded;
        }
        file.seek(SeekFrom::Start(self.offset))
            .map_err(|error| error.to_string())?;
        let mut buffer = [0; 64 * 1024];
        let mut remaining = MAX_POLL_BYTES;
        while remaining > 0 {
            let length = file
                .read(&mut buffer[..remaining.min(64 * 1024)])
                .map_err(|error| error.to_string())?;
            if length == 0 {
                break;
            }
            remaining -= length;
            self.offset += length as u64;
            for byte in &buffer[..length] {
                if *byte == b'\n' {
                    if !self.skipping {
                        self.record();
                    }
                    self.partial.clear();
                    self.skipping = false;
                } else if !self.skipping {
                    if self.partial.len() >= MAX_RECORD_BYTES {
                        self.partial.clear();
                        self.skipping = true;
                        self.malformed = true;
                    } else {
                        self.partial.push(*byte);
                    }
                }
            }
        }
        self.caught_up = self.offset >= metadata.len();
        if !self.valid_session || self.malformed || !self.caught_up {
            return Ok(AgentActivity::Unknown);
        }
        Ok(self.status)
    }

    /// State of a rollout too large to replay: the first record identifies the
    /// session, and everything from the last turn start onward decides the
    /// activity. Returns `None` when this shape cannot be proven, so the caller
    /// keeps the incremental scan from byte zero.
    fn seeded(&self, file: &mut File, len: u64, windows: &[u64]) -> Result<Option<Self>, String> {
        file.seek(SeekFrom::Start(0))
            .map_err(|error| error.to_string())?;
        let mut head = Vec::new();
        BufReader::new(Read::by_ref(file).take(MAX_RECORD_BYTES as u64))
            .read_until(b'\n', &mut head)
            .map_err(|error| error.to_string())?;
        if head.pop() != Some(b'\n') {
            return Ok(None);
        }
        let head_end = head.len() as u64 + 1;
        let mut base = Self::new(self.thread_id.clone());
        if base.apply(&head) != Applied::Meta
            || len.saturating_sub(head_end) <= MAX_POLL_BYTES as u64
        {
            return Ok(None);
        }
        for &window in windows {
            let start = len.saturating_sub(window);
            let at_head = start <= head_end;
            // Begin one byte early so a window that starts exactly on a record
            // boundary keeps that record instead of discarding it as a partial.
            let from = if at_head { head_end } else { start - 1 };
            file.seek(SeekFrom::Start(from))
                .map_err(|error| error.to_string())?;
            let mut bytes = Vec::with_capacity((len - from) as usize);
            Read::by_ref(file)
                .take(len - from)
                .read_to_end(&mut bytes)
                .map_err(|error| error.to_string())?;
            if bytes.len() as u64 != len - from {
                return Ok(None);
            }
            let mut rest = &bytes[..];
            if !at_head {
                let Some(boundary) = rest.iter().position(|byte| *byte == b'\n') else {
                    continue;
                };
                rest = &rest[boundary + 1..];
            }
            let mut trial = base.clone();
            let mut started = false;
            while let Some(end) = rest.iter().position(|byte| *byte == b'\n') {
                let line = &rest[..end];
                rest = &rest[end + 1..];
                if line.len() > MAX_RECORD_BYTES {
                    trial.malformed = true;
                } else {
                    started |= trial.apply(line) == Applied::Started;
                }
            }
            // Turn ends and aborts only mean something after their start, so a
            // window without one proves nothing unless it reaches the head.
            if !at_head && !started {
                continue;
            }
            if rest.len() > MAX_RECORD_BYTES {
                trial.skipping = true;
                trial.malformed = true;
            } else {
                trial.partial = rest.to_vec();
            }
            trial.offset = len;
            return Ok(Some(trial));
        }
        Ok(None)
    }

    fn record(&mut self) {
        let line = std::mem::take(&mut self.partial);
        self.apply(&line);
        self.partial = line;
    }

    fn apply(&mut self, line: &[u8]) -> Applied {
        let Ok(record) = serde_json::from_slice::<LogRecord>(line) else {
            self.malformed = true;
            return Applied::Other;
        };
        if record.kind == "session_meta" {
            let delegated = record.payload.parent_thread_id.is_some()
                || matches!(
                    &record.payload.source,
                    Some(LogSource::Details { subagent: Some(_) })
                );
            let noninteractive =
                matches!(&record.payload.source, Some(LogSource::Name(source)) if source == "exec");
            self.valid_session = record.payload.id.as_deref() == Some(&self.thread_id)
                && !delegated
                && !noninteractive;
            self.status = AgentActivity::Waiting;
            self.active_turn = None;
            self.completed_turn = None;
            return Applied::Meta;
        }
        if record.kind == "event_msg" {
            let Some(turn) = record.payload.turn_id.filter(|id| !id.is_empty()) else {
                return Applied::Other;
            };
            match record.payload.kind.as_str() {
                "task_started" | "turn_started" => {
                    // A new explicit start establishes the current turn even
                    // when an older record was too large or malformed to parse.
                    self.malformed = false;
                    self.active_turn = Some(turn);
                    self.completed_turn = None;
                    self.status = AgentActivity::Working;
                    return Applied::Started;
                }
                "task_complete" | "turn_complete" if self.active_turn.as_deref() == Some(&turn) => {
                    self.active_turn = None;
                    self.completed_turn = Some(turn);
                    self.status = AgentActivity::Done;
                }
                "turn_aborted" if self.active_turn.as_deref() == Some(&turn) => {
                    self.active_turn = None;
                    self.completed_turn = None;
                    self.status = AgentActivity::Waiting;
                }
                _ => {}
            }
        }
        Applied::Other
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Applied {
    Meta,
    Started,
    Other,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        sessions::ShellKind,
        store::{Project, Worktree},
    };

    struct Fixture {
        root: PathBuf,
        home: PathBuf,
        log_home: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root = env::temp_dir().join(format!("riwork-activity-test-{}", Uuid::new_v4()));
            let home = root.join("state");
            let log_home = root.join("codex");
            fs::create_dir_all(log_home.join("sessions/2026/09/27")).unwrap();
            Self {
                root,
                home,
                log_home,
            }
        }
        fn log(&self, thread: &str, records: &str) -> PathBuf {
            let path = self
                .log_home
                .join(format!("sessions/2026/09/27/rollout-test-{thread}.jsonl"));
            fs::write(&path, records).unwrap();
            path
        }
        fn tracker(&self) -> ActivityTracker {
            let mut tracker = ActivityTracker::at(self.home.clone());
            tracker.default_codex_home = Some(self.log_home.clone());
            tracker
        }
        fn bind(&self, shell: &ShellSession, thread: &str) {
            bind_codex_thread(&self.home, &shell.id, thread, &self.log_home).unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn shell(harness: Option<HarnessKind>) -> ShellSession {
        ShellSession {
            id: Uuid::new_v4().to_string(),
            project_id: Some("project-a".into()),
            worktree_id: None,
            kind: ShellKind::Project,
            cwd: PathBuf::from("/same/project"),
            command: None,
            editor_path: None,
            harness,
            unrestricted: false,
            codex_account_id: None,
            codex_account_label: None,
            codex_account_email: None,
            codex_home: None,
            orchestrator_skill_loaded: false,
            orchestrator_skill_version: None,
            orchestrator_project_root: None,
            created_at_unix: 1,
            alive: true,
        }
    }
    fn meta(thread: &str) -> String {
        format!(
            "{}\n",
            serde_json::json!({"type":"session_meta", "payload":{"id":thread,"source":"cli","cwd":"/same/project"}})
        )
    }
    fn event(kind: &str, turn: &str) -> String {
        format!(
            "{}\n",
            serde_json::json!({"type":"event_msg","payload":{"type":kind,"turn_id":turn}})
        )
    }
    fn append(path: &Path, value: &str) {
        OpenOptions::new()
            .append(true)
            .open(path)
            .unwrap()
            .write_all(value.as_bytes())
            .unwrap();
    }

    #[test]
    fn scheduled_readiness_cannot_transfer_to_a_replacement_provider_thread() {
        let fixture = Fixture::new();
        let mut session = shell(Some(HarnessKind::Codex));
        session.codex_home = Some(fixture.log_home.clone());
        let first = Uuid::new_v4().to_string();
        fixture.log(
            &first,
            &(meta(&first) + &event("task_started", "turn-1") + &event("task_complete", "turn-1")),
        );
        bind_codex_thread(&fixture.home, &session.id, &first, &fixture.log_home).unwrap();
        let mut tracker = fixture.tracker();
        assert_eq!(
            tracker.schedule_idle_token(&session, &first),
            Some("turn-1".into())
        );

        let replacement = Uuid::new_v4().to_string();
        fixture.log(
            &replacement,
            &(meta(&replacement)
                + &event("task_started", "turn-2")
                + &event("task_complete", "turn-2")),
        );
        bind_codex_thread(&fixture.home, &session.id, &replacement, &fixture.log_home).unwrap();
        assert_eq!(tracker.schedule_idle_token(&session, &first), None);
        assert_eq!(
            tracker.schedule_idle_token(&session, &replacement),
            Some("turn-2".into())
        );
    }

    #[test]
    fn scheduling_rollout_binding_requires_the_pinned_home_and_primary_cli_identity() {
        let fixture = Fixture::new();
        let thread = Uuid::new_v4().to_string();
        let path = fixture.log(
            &thread,
            &(meta(&thread) + &event("task_started", "turn") + &event("task_complete", "turn")),
        );
        let mut session = shell(Some(HarnessKind::Codex));
        session.codex_home = Some(fixture.log_home.clone());
        let tracker = fixture.tracker();
        assert_eq!(
            tracker.bind_schedule_rollout(&session, &path).unwrap(),
            thread
        );
        assert_eq!(tracker.schedule_identity(&session), Some(thread.clone()));
        session.codex_home = Some(fixture.home.clone());
        assert!(tracker.bind_schedule_rollout(&session, &path).is_err());
        session.codex_home = Some(fixture.log_home.clone());
        let noninteractive = Uuid::new_v4().to_string();
        let path = fixture.log(&noninteractive, &format!("{}\n", serde_json::json!({"type":"session_meta", "payload":{"id":noninteractive,"source":"exec"}})));
        assert!(tracker.bind_schedule_rollout(&session, &path).is_err());
        assert_eq!(tracker.schedule_identity(&session), Some(thread));
    }

    #[test]
    fn activity_requires_exact_thread_binding_even_when_all_threads_share_a_cwd() {
        let fixture = Fixture::new();
        let first = Uuid::new_v4().to_string();
        let second = Uuid::new_v4().to_string();
        fixture.log(&first, &(meta(&first) + &event("task_started", "first")));
        fixture.log(
            &second,
            &(meta(&second) + &event("task_started", "second") + &event("task_complete", "second")),
        );
        let a = shell(Some(HarnessKind::Codex));
        let b = shell(Some(HarnessKind::Codex));
        let plain = shell(None);
        let mut tracker = fixture.tracker();
        let unknown = tracker.sample(&[a.clone(), b.clone(), plain.clone()]);
        assert_eq!(unknown[&a.id], AgentActivity::Unknown);
        assert!(!unknown.contains_key(&plain.id));
        fixture.bind(&a, &first);
        fixture.bind(&b, &second);
        let result = tracker.sample(&[a.clone(), b.clone(), plain]);
        assert_eq!(result[&a.id], AgentActivity::Working);
        assert_eq!(result[&b.id], AgentActivity::Done);

        let wrong =
            serde_json::json!({"shell_id":b.id,"thread_id":second,"codex_home":fixture.log_home});
        fs::write(
            fixture
                .home
                .join("agent-activity")
                .join(format!("{}.json", a.id)),
            serde_json::to_vec(&wrong).unwrap(),
        )
        .unwrap();
        assert_eq!(tracker.sample(&[a.clone()])[&a.id], AgentActivity::Unknown);
    }

    #[test]
    fn partial_lines_and_stale_completions_never_finish_a_newer_turn() {
        let fixture = Fixture::new();
        let thread = Uuid::new_v4().to_string();
        let path = fixture.log(&thread, &(meta(&thread) + &event("task_started", "old")));
        let session = shell(Some(HarnessKind::Codex));
        fixture.bind(&session, &thread);
        let mut tracker = fixture.tracker();
        let sessions = [session.clone()];
        assert_eq!(
            tracker.sample(&sessions)[&session.id],
            AgentActivity::Working
        );
        let completion = event("task_complete", "old");
        append(&path, completion.trim_end());
        assert_eq!(
            tracker.sample(&sessions)[&session.id],
            AgentActivity::Working
        );
        append(&path, "\n");
        assert_eq!(tracker.sample(&sessions)[&session.id], AgentActivity::Done);
        append(
            &path,
            &(event("turn_started", "new") + &event("task_complete", "old")),
        );
        assert_eq!(
            tracker.sample(&sessions)[&session.id],
            AgentActivity::Working
        );
        append(&path, &event("turn_aborted", "new"));
        assert_eq!(
            tracker.sample(&sessions)[&session.id],
            AgentActivity::Waiting
        );
        append(&path, &event("turn_complete", "new"));
        assert_eq!(
            tracker.sample(&sessions)[&session.id],
            AgentActivity::Waiting
        );
        append(
            &path,
            &(event("task_started", "third") + &event("turn_complete", "third")),
        );
        assert_eq!(tracker.sample(&sessions)[&session.id], AgentActivity::Done);
        // Truncation revalidates session metadata and discards the old completion.
        fs::write(&path, meta(&thread) + &event("task_started", "replaced")).unwrap();
        assert_eq!(
            tracker.sample(&sessions)[&session.id],
            AgentActivity::Working
        );
    }

    #[test]
    fn mismatched_or_delegated_rollouts_stay_unknown_and_exit_overrides_old_work() {
        let fixture = Fixture::new();
        let thread = Uuid::new_v4().to_string();
        let other = Uuid::new_v4().to_string();
        let path = fixture.log(&thread, &(meta(&other) + &event("task_started", "turn")));
        let mut session = shell(Some(HarnessKind::Codex));
        fixture.bind(&session, &thread);
        let mut tracker = fixture.tracker();
        assert_eq!(
            tracker.sample(&[session.clone()])[&session.id],
            AgentActivity::Unknown
        );
        let delegated = format!(
            "{}\n",
            serde_json::json!({"type":"session_meta","payload":{"id":thread,"source":{"subagent":{"thread_spawn":{"parent_thread_id":other}}}}})
        );
        fs::write(&path, delegated + &event("task_started", "turn")).unwrap();
        assert_eq!(
            fixture.tracker().sample(&[session.clone()])[&session.id],
            AgentActivity::Unknown
        );
        fs::write(&path, meta(&thread) + &event("task_started", "turn")).unwrap();
        assert_eq!(
            fixture.tracker().sample(&[session.clone()])[&session.id],
            AgentActivity::Working
        );
        session.alive = false;
        assert_eq!(
            tracker.sample(&[session.clone()])[&session.id],
            AgentActivity::Exited
        );
        let mut plain = shell(None);
        plain.alive = false;
        assert!(!tracker.sample(&[plain.clone()]).contains_key(&plain.id));
    }

    #[test]
    fn completion_fallback_ignores_initial_done_and_emits_each_new_matching_turn_once() {
        let fixture = Fixture::new();
        let thread = Uuid::new_v4().to_string();
        let path = fixture.log(
            &thread,
            &(meta(&thread) + &event("task_started", "old") + &event("task_complete", "old")),
        );
        let session = shell(Some(HarnessKind::Codex));
        fixture.bind(&session, &thread);
        let mut tracker = fixture.tracker();
        tracker.sample(&[session.clone()]);
        assert!(tracker.take_completions().is_empty());
        append(
            &path,
            &(event("task_started", "new") + &event("task_complete", "old")),
        );
        tracker.sample(&[session.clone()]);
        assert!(tracker.take_completions().is_empty());
        append(&path, &event("task_complete", "new"));
        tracker.sample(&[session.clone()]);
        let completions = tracker.take_completions();
        assert_eq!(completions.len(), 1);
        assert_eq!(completions[0].shell_id, session.id);
        assert_eq!(
            completions[0].event_id,
            codex_completion_event_id(&fixture.log_home, &thread, "new")
        );
        tracker.sample(&[session.clone()]);
        assert!(tracker.take_completions().is_empty());
        let replacement = path.with_extension("replacement");
        fs::write(
            &replacement,
            meta(&thread)
                + &event("task_started", "historical")
                + &event("task_complete", "historical"),
        )
        .unwrap();
        fs::rename(replacement, &path).unwrap();
        tracker.sample(&[session.clone()]);
        assert!(tracker.take_completions().is_empty());
        append(
            &path,
            &(event("task_started", "aborted")
                + &event("turn_aborted", "aborted")
                + &event("task_complete", "aborted")),
        );
        tracker.sample(&[session.clone()]);
        assert!(tracker.take_completions().is_empty());
    }

    #[test]
    fn initial_backlogged_rollout_does_not_replay_historical_completions() {
        let fixture = Fixture::new();
        let thread = Uuid::new_v4().to_string();
        let padding = "x".repeat(MAX_POLL_BYTES + 512);
        let path = fixture.log(
            &thread,
            &(meta(&thread)
                + &event("task_started", "old")
                + &format!(
                    "{{\"type\":\"response_item\",\"payload\":{{\"ignored\":\"{padding}\"}}}}\n"
                )
                + &event("task_complete", "old")),
        );
        let session = shell(Some(HarnessKind::Codex));
        fixture.bind(&session, &thread);
        let mut tracker = fixture.tracker();
        // The first poll seeds from the head and tail instead of replaying
        // 8 MiB per poll, and still treats the existing Done as a baseline.
        assert_eq!(
            tracker.sample(&[session.clone()])[&session.id],
            AgentActivity::Done
        );
        assert!(tracker.take_completions().is_empty());
        assert_eq!(
            tracker.sample(&[session.clone()])[&session.id],
            AgentActivity::Done
        );
        assert!(tracker.take_completions().is_empty());
        append(
            &path,
            &(event("task_started", "new") + &event("task_complete", "new")),
        );
        tracker.sample(&[session.clone()]);
        assert_eq!(tracker.take_completions().len(), 1);
    }

    #[test]
    fn notification_only_queues_the_validated_current_completed_turn() {
        let fixture = Fixture::new();
        let project_root = fixture.root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let store = crate::store::Store::open(fixture.home.clone()).unwrap();
        let project = store
            .add_project(project_root, Some("Test project"))
            .unwrap();
        store.set_project_notifications(&project.id, true).unwrap();
        let mut session = shell(Some(HarnessKind::Codex));
        session.project_id = Some(project.id);
        fs::write(
            fixture.home.join("sessions.json"),
            serde_json::json!({"sessions":[session]}).to_string(),
        )
        .unwrap();
        let thread = Uuid::new_v4().to_string();
        let path = fixture.log(
            &thread,
            &(meta(&thread) + &event("task_started", "new") + &event("task_complete", "new")),
        );
        let notify = |turn: &str| Notification {
            kind: "agent-turn-complete".into(),
            thread_id: thread.clone(),
            turn_id: Some(turn.into()),
        };
        record_codex_notification_at(&fixture.home, &session.id, notify("old"), &fixture.log_home)
            .unwrap();
        assert!(
            crate::notifications::claim_pending(&fixture.home)
                .unwrap()
                .is_empty()
        );
        record_codex_notification_at(&fixture.home, &session.id, notify("new"), &fixture.log_home)
            .unwrap();
        assert_eq!(
            crate::notifications::claim_pending(&fixture.home)
                .unwrap()
                .len(),
            1
        );
        record_codex_notification_at(&fixture.home, &session.id, notify("new"), &fixture.log_home)
            .unwrap();
        assert!(
            crate::notifications::claim_pending(&fixture.home)
                .unwrap()
                .is_empty()
        );
        append(&path, &event("task_started", "working"));
        record_codex_notification_at(
            &fixture.home,
            &session.id,
            notify("working"),
            &fixture.log_home,
        )
        .unwrap();
        assert!(
            crate::notifications::claim_pending(&fixture.home)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn exact_resume_argv_binds_without_matching_paths_or_reading_messages() {
        let fixture = Fixture::new();
        let thread = Uuid::new_v4().to_string();
        fixture.log(&thread, &(meta(&thread) + &event("task_started", "turn")));
        let mut session = shell(Some(HarnessKind::Codex));
        session.command = Some(format!(
            "exec '/Applications/Codex CLI/codex' '-c' 'model=\"model\"' 'resume' '{thread}'"
        ));
        assert_eq!(resume_thread(&session), Some(thread.clone()));
        assert_eq!(
            fixture.tracker().sample(&[session.clone()])[&session.id],
            AgentActivity::Working
        );
        session.command = Some(format!("exec 'codex' 'please resume {thread}'"));
        assert_eq!(resume_thread(&session), None);
        session.command = Some(format!("exec 'codex' 'resume' '{thread}'; echo ignored"));
        assert_eq!(resume_thread(&session), None);
        session.command = Some(format!("exec 'codex' 'resume' '{thread}'"));
        session.harness = None;
        assert_eq!(resume_thread(&session), None);
    }

    #[test]
    fn saved_account_home_prevents_resume_activity_from_using_another_accounts_log() {
        let fixture = Fixture::new();
        let account_home = fixture.root.join("another-account");
        fs::create_dir_all(account_home.join("sessions")).unwrap();
        let thread = Uuid::new_v4().to_string();
        fixture.log(&thread, &(meta(&thread) + &event("task_started", "turn")));
        fs::write(
            account_home
                .join("sessions")
                .join(format!("rollout-{thread}.jsonl")),
            meta(&thread) + &event("task_started", "turn") + &event("task_complete", "turn"),
        )
        .unwrap();
        let mut session = shell(Some(HarnessKind::Codex));
        session.command = Some(format!("exec codex resume {thread}"));
        session.codex_home = Some(fixture.log_home.clone());
        let mut tracker = fixture.tracker();
        tracker.default_codex_home = Some(account_home.clone());
        assert_eq!(
            tracker.sample(&[session.clone()])[&session.id],
            AgentActivity::Working
        );
        session.codex_home = Some(account_home);
        assert_eq!(
            tracker.sample(&[session.clone()])[&session.id],
            AgentActivity::Done
        );
    }

    #[test]
    fn notification_bindings_keep_only_identifiers_and_ignore_delegated_threads() {
        let fixture = Fixture::new();
        let thread = Uuid::new_v4().to_string();
        fixture.log(
            &thread,
            &(meta(&thread) + &event("task_started", "turn") + &event("task_complete", "turn")),
        );
        let session = shell(Some(HarnessKind::Codex));
        let input = serde_json::json!({"type":"agent-turn-complete","thread-id":thread,"last-assistant-message":"private response","input-messages":["private request"]});
        let notification: Notification = serde_json::from_value(input).unwrap();
        record_codex_notification_at(&fixture.home, &session.id, notification, &fixture.log_home)
            .unwrap();
        let binding_path = fixture
            .home
            .join("agent-activity")
            .join(format!("{}.json", session.id));
        let bytes = fs::read(&binding_path).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("private"));
        let binding: Binding = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(binding.thread_id, thread);
        let child = Uuid::new_v4().to_string();
        fixture.log(&child, &format!("{}\n",serde_json::json!({"type":"session_meta","payload":{"id":child,"parent_thread_id":thread}})));
        record_codex_notification_at(
            &fixture.home,
            &session.id,
            Notification {
                kind: "agent-turn-complete".into(),
                thread_id: child,
                turn_id: None,
            },
            &fixture.log_home,
        )
        .unwrap();
        assert_eq!(fs::read(&binding_path).unwrap(), bytes);
    }

    #[test]
    fn grouping_uses_longest_current_worktree_with_stored_assignment_as_fallback() {
        let mut state = State::default();
        state.projects.push(Project {
            id: "project-a".into(),
            name: "Project".into(),
            root: PathBuf::from("/project"),
            repository_roots: Vec::new(),
            folder_id: None,
            created_at: 1,
            notify_on_agent_done: false,
            codex_account: crate::store::ProjectCodexAccount::default(),
        });
        for (id, project, path) in [
            ("root", "project-a", "/project"),
            ("nested", "project-a", "/project/nested"),
            ("foreign", "project-b", "/project/nested/deep"),
        ] {
            state.worktrees.push(Worktree {
                id: id.into(),
                project_id: project.into(),
                branch: id.into(),
                path: PathBuf::from(path),
                is_primary: id == "root",
                repository_root: None,
                created_at: 1,
            });
        }
        let mut a = shell(Some(HarnessKind::Codex));
        a.worktree_id = Some("root".into());
        let mut b = shell(Some(HarnessKind::Codex));
        b.worktree_id = Some("nested".into());
        let mut c = shell(Some(HarnessKind::Codex));
        c.project_id = Some("project-b".into());
        c.worktree_id = Some("foreign".into());
        let plain = shell(None);
        let activity = BTreeMap::from([
            (a.id.clone(), AgentActivity::Working),
            (b.id.clone(), AgentActivity::Done),
            (c.id.clone(), AgentActivity::Working),
        ]);
        let cwds = BTreeMap::from([(a.id.clone(), PathBuf::from("/project/nested/deep/src"))]);
        let sessions = [a, b, c, plain];
        let project = ActivityCounts::for_project("project-a", &sessions, &activity);
        assert_eq!(project.working, 1);
        assert_eq!(project.done, 1);
        let nested = ActivityCounts::for_worktree("nested", &state, &sessions, &cwds, &activity);
        assert_eq!(nested, project);
        assert_eq!(
            ActivityCounts::for_worktree("root", &state, &sessions, &cwds, &activity),
            ActivityCounts::default()
        );
        assert_eq!(
            ActivityCounts {
                unknown: 1,
                exited: 1,
                ..ActivityCounts::default()
            }
            .summary(),
            Some("? 1 unknown".to_owned())
        );
        assert_eq!(ActivityCounts::default().summary(), None);
        assert_eq!(project.summary().as_deref(), Some("● 1 working · ✓ 1 done"));
    }

    fn oversized_record_rollout(fixture: &Fixture, thread: &str) -> PathBuf {
        let path = fixture.log(
            thread,
            &(meta(thread) + &event("task_started", "old") + &event("task_complete", "old")),
        );
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(
            b"{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"content\":\"",
        )
        .unwrap();
        let buffer = [b'x'; 64 * 1024];
        let mut remaining = MAX_RECORD_BYTES + 1;
        while remaining > 0 {
            let length = remaining.min(buffer.len());
            file.write_all(&buffer[..length]).unwrap();
            remaining -= length;
        }
        file.write_all(b"\"}}\n").unwrap();
        file.write_all(event("task_started", "current").as_bytes())
            .unwrap();
        path
    }

    #[test]
    fn oversized_records_are_bounded_and_a_later_explicit_turn_recovers_activity() {
        let fixture = Fixture::new();
        let thread = Uuid::new_v4().to_string();
        let path = oversized_record_rollout(&fixture, &thread);
        // The incremental scan (no tail seeding) still enforces the record cap.
        let mut cursor = RolloutCursor::new(thread.clone());
        assert_eq!(
            cursor.read_with(&path, &[]).unwrap(),
            AgentActivity::Unknown
        );
        assert!(cursor.offset <= MAX_POLL_BYTES as u64);
        let mut status = AgentActivity::Unknown;
        for _ in 0..8 {
            status = cursor.read_with(&path, &[]).unwrap();
            if status == AgentActivity::Working {
                break;
            }
        }
        assert_eq!(status, AgentActivity::Working);
        // Seeding skips the oversized record the same way and needs one poll.
        let session = shell(Some(HarnessKind::Codex));
        fixture.bind(&session, &thread);
        let mut tracker = fixture.tracker();
        let sessions = [session.clone()];
        assert_eq!(
            tracker.sample(&sessions)[&session.id],
            AgentActivity::Working
        );
        append(&path, &event("task_complete", "current"));
        assert_eq!(tracker.sample(&sessions)[&session.id], AgentActivity::Done);
    }

    fn padding(bytes: usize) -> String {
        format!(
            "{{\"type\":\"response_item\",\"payload\":{{\"ignored\":\"{}\"}}}}\n",
            "x".repeat(bytes)
        )
    }
    const MIB: usize = 1024 * 1024;

    #[test]
    fn rollout_past_the_poll_limit_is_seeded_in_one_read_and_then_followed_incrementally() {
        let fixture = Fixture::new();
        let thread = Uuid::new_v4().to_string();
        let path = fixture.log(
            &thread,
            &(meta(&thread)
                + &event("task_started", "old")
                + &event("task_complete", "old")
                + &padding(MAX_POLL_BYTES + 512)
                + &event("task_started", "new")),
        );
        let length = fs::metadata(&path).unwrap().len();
        assert!(length > MAX_POLL_BYTES as u64);
        let mut cursor = RolloutCursor::new(thread.clone());
        assert_eq!(cursor.read(&path).unwrap(), AgentActivity::Working);
        assert!(cursor.caught_up && cursor.valid_session && !cursor.restarted);
        assert_eq!(cursor.offset, length);
        assert_eq!(cursor.active_turn.as_deref(), Some("new"));
        append(&path, &event("task_complete", "new"));
        assert_eq!(cursor.read(&path).unwrap(), AgentActivity::Done);
        assert_eq!(cursor.completed_turn.as_deref(), Some("new"));
        // A partially written trailing record is kept for the next poll.
        let completion = event("task_started", "next");
        append(&path, completion.trim_end());
        assert_eq!(cursor.read(&path).unwrap(), AgentActivity::Done);
        append(&path, "\n");
        assert_eq!(cursor.read(&path).unwrap(), AgentActivity::Working);
    }

    #[test]
    fn seeding_widens_the_tail_until_it_contains_a_turn_start() {
        let fixture = Fixture::new();
        let thread = Uuid::new_v4().to_string();
        // The turn start is 9 MiB from the end: the 1, 4 and 16 MiB windows
        // are tried in turn, and the 16 MiB one is the first to contain it.
        let path = fixture.log(
            &thread,
            &(meta(&thread)
                + &padding(9 * MIB)
                + &event("task_started", "long")
                + &padding(6 * MIB)
                + &event("task_complete", "long")
                + &padding(3 * MIB)),
        );
        let mut cursor = RolloutCursor::new(thread.clone());
        assert_eq!(cursor.read(&path).unwrap(), AgentActivity::Done);
        assert_eq!(cursor.completed_turn.as_deref(), Some("long"));
        assert_eq!(cursor.offset, fs::metadata(&path).unwrap().len());
        // With only a 1 MiB window nothing proves the state, so the incremental
        // scan from byte zero takes over (and is still bounded per call).
        let mut fallback = RolloutCursor::new(thread.clone());
        assert_eq!(
            fallback.read_with(&path, &[MIB as u64]).unwrap(),
            AgentActivity::Unknown
        );
        assert!(fallback.offset <= MAX_POLL_BYTES as u64 && !fallback.caught_up);
        let mut status = AgentActivity::Unknown;
        for _ in 0..4 {
            status = fallback.read_with(&path, &[MIB as u64]).unwrap();
        }
        assert_eq!(status, AgentActivity::Done);
    }

    #[test]
    fn tail_window_that_starts_on_a_record_boundary_keeps_that_record() {
        let fixture = Fixture::new();
        let thread = Uuid::new_v4().to_string();
        let started = event("task_started", "boundary");
        let prefix = meta(&thread) + &padding(MAX_POLL_BYTES + 1024);
        let path = fixture.log(&thread, &(prefix.clone() + &started));
        let mut cursor = RolloutCursor::new(thread.clone());
        // Window exactly as long as the final record: it starts on a boundary.
        assert_eq!(
            cursor.read_with(&path, &[started.len() as u64]).unwrap(),
            AgentActivity::Working
        );
        assert_eq!(cursor.active_turn.as_deref(), Some("boundary"));
    }

    #[test]
    fn seeded_cursor_resets_when_the_rollout_is_replaced_or_truncated() {
        let fixture = Fixture::new();
        let thread = Uuid::new_v4().to_string();
        let path = fixture.log(
            &thread,
            &(meta(&thread) + &padding(MAX_POLL_BYTES + 1024) + &event("task_started", "big")),
        );
        let mut cursor = RolloutCursor::new(thread.clone());
        assert_eq!(cursor.read(&path).unwrap(), AgentActivity::Working);
        fs::write(&path, meta(&thread) + &event("task_started", "small")).unwrap();
        assert_eq!(cursor.read(&path).unwrap(), AgentActivity::Working);
        assert!(cursor.restarted);
        assert_eq!(cursor.active_turn.as_deref(), Some("small"));
        // A large replacement (a new file) is seeded again after the reset.
        let replacement = path.with_extension("replacement");
        fs::write(
            &replacement,
            meta(&thread) + &padding(MAX_POLL_BYTES + 1024) + &event("task_started", "again"),
        )
        .unwrap();
        fs::rename(replacement, &path).unwrap();
        assert_eq!(cursor.read(&path).unwrap(), AgentActivity::Working);
        assert!(cursor.restarted);
        assert_eq!(cursor.active_turn.as_deref(), Some("again"));
    }

    #[test]
    fn seeding_requires_a_session_meta_first_record() {
        let fixture = Fixture::new();
        let thread = Uuid::new_v4().to_string();
        let path = fixture.log(
            &thread,
            &(event("task_started", "first")
                + &meta(&thread)
                + &padding(MAX_POLL_BYTES + 1024)
                + &event("task_started", "last")),
        );
        let mut cursor = RolloutCursor::new(thread);
        // Not the expected shape: fall back to the exact incremental replay.
        assert_eq!(cursor.read(&path).unwrap(), AgentActivity::Unknown);
        assert!(!cursor.caught_up);
    }

    fn notified_project(fixture: &Fixture) -> ShellSession {
        let project_root = fixture.root.join("project");
        fs::create_dir_all(&project_root).unwrap();
        let store = crate::store::Store::open(fixture.home.clone()).unwrap();
        let project = store.add_project(project_root, Some("Large")).unwrap();
        store.set_project_notifications(&project.id, true).unwrap();
        let mut session = shell(Some(HarnessKind::Codex));
        session.project_id = Some(project.id);
        fs::write(
            fixture.home.join("sessions.json"),
            serde_json::json!({"sessions":[session]}).to_string(),
        )
        .unwrap();
        session
    }

    #[test]
    fn notify_hook_queues_completions_for_rollouts_over_the_poll_limit() {
        let fixture = Fixture::new();
        let session = notified_project(&fixture);
        let thread = Uuid::new_v4().to_string();
        let path = fixture.log(
            &thread,
            &(meta(&thread)
                + &event("task_started", "old")
                + &event("task_complete", "old")
                + &padding(MAX_POLL_BYTES * 2)
                + &event("task_started", "new")
                + &event("task_complete", "new")),
        );
        assert!(fs::metadata(&path).unwrap().len() > MAX_POLL_BYTES as u64 * 2);
        let notify = |turn: &str| Notification {
            kind: "agent-turn-complete".into(),
            thread_id: thread.clone(),
            turn_id: Some(turn.into()),
        };
        // A stale turn id is still vetoed by the rollout.
        record_codex_notification_at(&fixture.home, &session.id, notify("old"), &fixture.log_home)
            .unwrap();
        assert!(
            crate::notifications::claim_pending(&fixture.home)
                .unwrap()
                .is_empty()
        );
        record_codex_notification_at(&fixture.home, &session.id, notify("new"), &fixture.log_home)
            .unwrap();
        assert_eq!(
            crate::notifications::claim_pending(&fixture.home)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn notify_hook_trusts_the_payload_when_the_turn_holds_an_unreadable_record() {
        let fixture = Fixture::new();
        let session = notified_project(&fixture);
        let thread = Uuid::new_v4().to_string();
        let path = fixture.log(
            &thread,
            &(meta(&thread) + &event("task_started", "current")),
        );
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{\"type\":\"response_item\",\"payload\":{\"content\":\"")
            .unwrap();
        file.write_all(&vec![b'x'; MAX_RECORD_BYTES + 1]).unwrap();
        file.write_all(b"\"}}\n").unwrap();
        drop(file);
        append(&path, &event("task_complete", "current"));
        let mut cursor = RolloutCursor::new(thread.clone());
        // The oversized record inside the turn leaves the activity unproven.
        assert_eq!(cursor.read(&path).unwrap(), AgentActivity::Unknown);
        assert!(cursor.valid_session && cursor.malformed);
        let notify = |turn: Option<&str>| Notification {
            kind: "agent-turn-complete".into(),
            thread_id: thread.clone(),
            turn_id: turn.map(str::to_owned),
        };
        record_codex_notification_at(&fixture.home, &session.id, notify(None), &fixture.log_home)
            .unwrap();
        assert!(
            crate::notifications::claim_pending(&fixture.home)
                .unwrap()
                .is_empty()
        );
        record_codex_notification_at(
            &fixture.home,
            &session.id,
            notify(Some("current")),
            &fixture.log_home,
        )
        .unwrap();
        assert_eq!(
            crate::notifications::claim_pending(&fixture.home)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn missing_rollouts_are_looked_up_with_exponential_backoff() {
        let fixture = Fixture::new();
        let thread = Uuid::new_v4().to_string();
        let session = shell(Some(HarnessKind::Codex));
        fixture.bind(&session, &thread);
        let mut tracker = fixture.tracker();
        let sessions = [session.clone()];
        assert_eq!(
            tracker.sample(&sessions)[&session.id],
            AgentActivity::Unknown
        );
        let first = tracker.cursors[&session.id].lookup_after.unwrap();
        // The file appearing during the wait is not noticed until it elapses.
        fixture.log(&thread, &(meta(&thread) + &event("task_started", "turn")));
        assert_eq!(
            tracker.sample(&sessions)[&session.id],
            AgentActivity::Unknown
        );
        assert_eq!(tracker.cursors[&session.id].lookup_after, Some(first));
        tracker.cursors.get_mut(&session.id).unwrap().lookup_after =
            Some(Instant::now() - Duration::from_secs(1));
        assert_eq!(
            tracker.sample(&sessions)[&session.id],
            AgentActivity::Working
        );
        assert_eq!(tracker.cursors[&session.id].lookup_misses, 0);
        // Repeated misses double the wait up to the cap.
        let mut cursor = BoundCursor::new(Binding {
            shell_id: session.id.clone(),
            thread_id: thread,
            codex_home: fixture.log_home.clone(),
        });
        let mut waits = Vec::new();
        for _ in 0..9 {
            cursor.missed_lookup();
            waits.push(cursor.lookup_after.unwrap().duration_since(Instant::now()));
        }
        assert!(waits[0] <= Duration::from_secs(2));
        assert!(waits[3] > Duration::from_secs(10) && waits[3] <= Duration::from_secs(16));
        assert!(waits[8] > Duration::from_secs(100) && waits[8] <= LOOKUP_BACKOFF_CAP);
        // A file that disappears is resolved again, also with a wait.
        fs::remove_file(tracker.cursors[&session.id].path.clone().unwrap()).unwrap();
        assert_eq!(
            tracker.sample(&sessions)[&session.id],
            AgentActivity::Unknown
        );
        let lost = &tracker.cursors[&session.id];
        assert!(lost.path.is_none() && lost.lookup_after.is_some());
    }
}
