//! Explicit Program Status Protocol reports from a pane's raw PTY stream.
//! Nothing is inferred from screen text, silence, or connection loss. tmux's
//! output pipe survives client detach; the collector stores reports, not output.

use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, VecDeque},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};
use uuid::Uuid;

const MAX_BODY: usize = 4096 - 9;
const MAX_RECORDS: usize = 256;
const MAX_SNAPSHOT: u64 = 2 * 1024 * 1024;
const LEASE_SECONDS: u64 = 10;
pub const CAPABILITIES: &str =
    "?:states=idle,working,done,blocked,error:kinds=permission,question,auth";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Idle,
    Working,
    Done,
    Blocked,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Permission,
    Question,
    Auth,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Record {
    pub state: State,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<Kind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub since_unix: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Snapshot {
    pub version: u32,
    pub source: String,
    pub shell_id: String,
    pub generation: String,
    /// A recently heard collector, not proof that the reported work is healthy.
    pub connected: bool,
    pub observed_at_unix: u64,
    pub updated_at_unix: u64,
    pub root: Option<Record>,
    pub records: BTreeMap<String, Record>,
}

impl Snapshot {
    fn empty(shell: &str, generation: &str) -> Self {
        Self {
            version: 1,
            source: "osc7501".into(),
            shell_id: shell.into(),
            generation: generation.into(),
            connected: false,
            observed_at_unix: 0,
            updated_at_unix: crate::activity::unix_now(),
            root: None,
            records: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Query {
        bell: bool,
    },
    Report {
        id: String,
        record: Record,
    },
    Clear {
        id: String,
    },
    /// An internal launch boundary, carried as an otherwise normal root clear.
    Boundary(String),
}

#[derive(Default)]
enum Mode {
    #[default]
    Ground,
    Escape,
    Osc,
    OscEscape,
    Discard,
    DiscardEscape,
    String,
    StringEscape,
}

/// Incremental bounded parser; callbacks run once per complete valid sequence.
/// OSC-looking bytes inside DCS/APC/PM strings are never interpreted as reports.
#[derive(Default)]
pub struct Parser {
    mode: Mode,
    body: Vec<u8>,
}

impl Parser {
    pub fn feed(&mut self, bytes: &[u8], mut emit: impl FnMut(Command)) {
        for &byte in bytes {
            if matches!(byte, 0x18 | 0x1a) {
                self.body.clear();
                self.mode = Mode::Ground;
                continue;
            }
            self.mode = match std::mem::take(&mut self.mode) {
                Mode::Ground => {
                    if byte == 0x1b {
                        Mode::Escape
                    } else {
                        Mode::Ground
                    }
                }
                Mode::Escape => match byte {
                    b']' => {
                        self.body.clear();
                        Mode::Osc
                    }
                    b'P' | b'_' | b'^' | b'X' => Mode::String,
                    0x1b => Mode::Escape,
                    _ => Mode::Ground,
                },
                Mode::String => {
                    if byte == 0x1b {
                        Mode::StringEscape
                    } else {
                        Mode::String
                    }
                }
                Mode::StringEscape => {
                    if byte == b'\\' {
                        Mode::Ground
                    } else if byte == 0x1b {
                        Mode::StringEscape
                    } else {
                        Mode::String
                    }
                }
                Mode::Osc => match byte {
                    0x07 => {
                        if let Some(c) = parse(&self.body, true) {
                            emit(c);
                        }
                        self.body.clear();
                        Mode::Ground
                    }
                    0x1b => Mode::OscEscape,
                    _ if self.body.len() >= MAX_BODY + 5 => {
                        self.body.clear();
                        Mode::Discard
                    }
                    _ => {
                        self.body.push(byte);
                        Mode::Osc
                    }
                },
                Mode::OscEscape => match byte {
                    b'\\' => {
                        if let Some(c) = parse(&self.body, false) {
                            emit(c);
                        }
                        self.body.clear();
                        Mode::Ground
                    }
                    b']' => {
                        self.body.clear();
                        Mode::Osc
                    }
                    _ => {
                        self.body.clear();
                        Mode::Ground
                    }
                },
                Mode::Discard => match byte {
                    0x07 => Mode::Ground,
                    0x1b => Mode::DiscardEscape,
                    _ => Mode::Discard,
                },
                Mode::DiscardEscape => {
                    if byte == b'\\' {
                        Mode::Ground
                    } else if byte == 0x1b {
                        Mode::DiscardEscape
                    } else {
                        Mode::Discard
                    }
                }
            };
        }
    }
}

fn name(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.+-".contains(&b))
}
fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.split('/').count() <= 8
        && s.split('/').all(|p| p.len() <= 32 && name(p))
}
fn text(s: &str, max: usize) -> Option<Option<String>> {
    if s.is_empty() {
        return Some(None);
    }
    let bytes = STANDARD
        .decode(s)
        .or_else(|_| STANDARD_NO_PAD.decode(s))
        .ok()?;
    if bytes.len() > max {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    (!text.chars().any(char::is_control)).then_some(Some(text))
}
fn parse(body: &[u8], bell: bool) -> Option<Command> {
    let body = std::str::from_utf8(body).ok()?.strip_prefix("7501;")?;
    if body.len() > MAX_BODY {
        return None;
    }
    if body == "?" {
        return Some(Command::Query { bell });
    }
    let mut fields = BTreeMap::new();
    for pair in body.split(':') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if key.len() > 16 {
            return None;
        }
        if !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.+-,/=".contains(&b))
        {
            continue;
        }
        if (key == "id" && !valid_id(value))
            || (key == "app" && value.len() > 32)
            || (key == "title" && value.len() > 256)
            || (key == "msg" && value.len() > 2732)
        {
            return None;
        }
        fields.insert(key, value);
    }
    let title = fields.get("title").map_or(Some(None), |v| text(v, 192))?;
    let message = fields.get("msg").map_or(Some(None), |v| text(v, 2048))?;
    let id = fields.get("id").copied().unwrap_or("").to_owned();
    let state = match *fields.get("state")? {
        "clear" => {
            if id.is_empty()
                && let Some(token) = fields.get("somtus")
                && canonical_uuid(token)
            {
                return Some(Command::Boundary((*token).into()));
            }
            return Some(Command::Clear { id });
        }
        "idle" => State::Idle,
        "working" => State::Working,
        "done" => State::Done,
        "blocked" => State::Blocked,
        "error" => State::Error,
        _ => return None,
    };
    let kind = (state == State::Blocked)
        .then(|| match fields.get("kind").copied() {
            Some("permission") => Some(Kind::Permission),
            Some("question") => Some(Kind::Question),
            Some("auth") => Some(Kind::Auth),
            _ => None,
        })
        .flatten();
    let progress = matches!(state, State::Working | State::Blocked)
        .then(|| {
            fields
                .get("progress")
                .filter(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|p| p.parse::<u8>().ok())
                .filter(|p| *p <= 100)
        })
        .flatten();
    Some(Command::Report {
        id,
        record: Record {
            state,
            kind,
            progress,
            app: fields
                .get("app")
                .filter(|v| name(v))
                .map(|s| (*s).to_owned()),
            title,
            message,
            since_unix: 0,
        },
    })
}

fn canonical_uuid(s: &str) -> bool {
    Uuid::parse_str(s).is_ok_and(|id| id.to_string() == s)
}
fn directory(home: &Path) -> PathBuf {
    home.join("program-status")
}
fn path(home: &Path, shell: &str) -> PathBuf {
    directory(home).join(format!("{shell}.json"))
}
fn generation_path(home: &Path, shell: &str) -> PathBuf {
    directory(home).join(format!("{shell}.generation"))
}
fn generation(home: &Path, shell: &str) -> Option<String> {
    let mut value = String::new();
    File::open(generation_path(home, shell))
        .ok()?
        .take(128)
        .read_to_string(&mut value)
        .ok()?;
    canonical_uuid(&value).then_some(value)
}
fn lock(home: &Path, shell: &str) -> Result<File, String> {
    crate::paths::create_private_dir(&directory(home)).map_err(|e| e.to_string())?;
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(directory(home).join(format!("{shell}.lock")))
        .map_err(|e| e.to_string())?;
    file.lock().map_err(|e| e.to_string())?;
    Ok(file)
}
fn atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temp = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut f = options.open(&temp)?;
        f.write_all(bytes)?;
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(|e: std::io::Error| e.to_string())
}

/// Start a new pane incarnation before launching it. Old collectors cannot
/// publish until the launcher sends this exact generation's root clear.
pub fn prepare(home: &Path, shell: &str) -> Result<String, String> {
    let token = Uuid::new_v4().to_string();
    prepare_with_token(home, shell, &token)?;
    Ok(token)
}

/// Commit a token chosen by a replacement launcher, only once that new pane
/// process has actually started. A canceled account switch never invalidates
/// the original collector.
pub fn prepare_with_token(home: &Path, shell: &str, token: &str) -> Result<(), String> {
    if !canonical_uuid(shell) || !canonical_uuid(token) {
        return Err("invalid program status shell UUID".into());
    }
    let _lock = lock(home, shell)?;
    atomic(&generation_path(home, shell), token.as_bytes())?;
    atomic(
        &path(home, shell),
        &serde_json::to_vec(&Snapshot::empty(shell, &token)).unwrap(),
    )?;
    Ok(())
}

/// Read only a bounded, current-incarnation snapshot. An expired collector
/// lease means disconnected; the reported status remains historical, not done.
pub fn snapshot(home: &Path, shell: &str) -> Option<Snapshot> {
    if !canonical_uuid(shell) {
        return None;
    }
    let mut bytes = Vec::new();
    File::open(path(home, shell))
        .ok()?
        .take(MAX_SNAPSHOT + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_SNAPSHOT {
        return None;
    }
    let mut value: Snapshot = serde_json::from_slice(&bytes).ok()?;
    if value.version != 1
        || value.source != "osc7501"
        || value.shell_id != shell
        || generation(home, shell).as_deref() != Some(&value.generation)
        || value.records.len() >= MAX_RECORDS
        || value.root.as_ref().is_some_and(|r| !valid_record(r))
        || value
            .records
            .iter()
            .any(|(id, record)| !valid_id(id) || !valid_record(record))
    {
        return None;
    }
    value.connected &=
        crate::activity::unix_now().saturating_sub(value.updated_at_unix) <= LEASE_SECONDS;
    Some(value)
}
fn valid_record(record: &Record) -> bool {
    record
        .app
        .as_ref()
        .is_none_or(|app| app.len() <= 32 && name(app))
        && record
            .title
            .as_ref()
            .is_none_or(|text| text.len() <= 192 && !text.chars().any(char::is_control))
        && record
            .message
            .as_ref()
            .is_none_or(|text| text.len() <= 2048 && !text.chars().any(char::is_control))
        && record.progress.is_none_or(|value| {
            value <= 100 && matches!(record.state, State::Working | State::Blocked)
        })
        && (record.kind.is_none() || record.state == State::Blocked)
}
fn save(home: &Path, value: &Snapshot) -> Result<(), String> {
    let _lock = lock(home, &value.shell_id)?;
    if generation(home, &value.shell_id).as_deref() != Some(&value.generation) {
        return Ok(());
    }
    atomic(
        &path(home, &value.shell_id),
        &serde_json::to_vec(value).map_err(|e| e.to_string())?,
    )
}

struct Reports {
    value: Snapshot,
    order: VecDeque<String>,
}
impl Reports {
    fn new(shell: &str, token: &str) -> Self {
        let mut value = Snapshot::empty(shell, token);
        value.connected = true;
        Self {
            value,
            order: VecDeque::new(),
        }
    }
    fn apply(&mut self, command: Command, now: u64) -> bool {
        match command {
            Command::Report { id, mut record } => {
                let previous = if id.is_empty() {
                    self.value.root.as_ref()
                } else {
                    self.value.records.get(&id)
                };
                record.since_unix = previous
                    .filter(|p| p.state == record.state)
                    .map_or(now, |p| p.since_unix);
                if id.is_empty() {
                    self.value.root = Some(record);
                } else {
                    self.order.retain(|old| old != &id);
                    if !self.value.records.contains_key(&id)
                        && self.value.records.len() >= MAX_RECORDS - 1
                        && let Some(oldest) = self.order.pop_front()
                    {
                        self.value.records.remove(&oldest);
                    }
                    self.order.push_back(id.clone());
                    self.value.records.insert(id, record);
                }
            }
            Command::Clear { id } => {
                if id.is_empty() {
                    self.value.root = None;
                    self.value.records.clear();
                    self.order.clear();
                } else {
                    let prefix = format!("{id}/");
                    self.value
                        .records
                        .retain(|key, _| key != &id && !key.starts_with(&prefix));
                    self.order
                        .retain(|key| key != &id && !key.starts_with(&prefix));
                }
            }
            _ => return false,
        }
        self.value.observed_at_unix = now;
        true
    }
}

/// stdin is tmux pipe-pane's raw PTY output, stdout carries capability replies
/// only. Snapshot writing is coalesced off the byte reader; retained memory is
/// bounded independently of terminal output volume. EOF only loses the lease.
pub fn collect(
    home: &Path,
    shell: &str,
    token: &str,
    mut input: impl Read,
    mut replies: impl Write,
) -> Result<(), String> {
    if !canonical_uuid(shell) || generation(home, shell).as_deref() != Some(token) {
        return Err("program status collector incarnation is no longer current".into());
    }
    let mut reports = Reports::new(shell, token);
    let shared = Arc::new((Mutex::new((reports.value.clone(), false)), Condvar::new()));
    let writer_state = shared.clone();
    let writer_home = home.to_owned();
    let writer = std::thread::spawn(move || {
        let (mutex, wake) = &*writer_state;
        let mut previous = None;
        let mut last_write = std::time::Instant::now();
        loop {
            let mut guard = mutex.lock().unwrap_or_else(|p| p.into_inner());
            let mut value = guard.0.clone();
            let finished = guard.1;
            drop(guard);
            if previous.as_ref() != Some(&value)
                || last_write.elapsed() >= Duration::from_secs(2)
                || finished
            {
                previous = Some(value.clone());
                value.updated_at_unix = crate::activity::unix_now();
                let _ = save(&writer_home, &value);
                last_write = std::time::Instant::now();
            }
            if finished {
                break;
            }
            guard = mutex.lock().unwrap_or_else(|p| p.into_inner());
            if !guard.1 {
                let (next, _) = wake
                    .wait_timeout(guard, Duration::from_millis(200))
                    .unwrap_or_else(|p| p.into_inner());
                drop(next);
            }
        }
    });
    let mut parser = Parser::default();
    let mut buffer = [0u8; 16 * 1024];
    let result = loop {
        let count = match input.read(&mut buffer) {
            Ok(0) => break Ok(()),
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => break Err(e.to_string()),
        };
        let mut changed = false;
        parser.feed(&buffer[..count], |command| match command {
            Command::Query { bell } => {
                let end = if bell { "\x07" } else { "\x1b\\" };
                let _ = write!(replies, "\x1b]7501;{CAPABILITIES}{end}");
                let _ = replies.flush();
            }
            Command::Boundary(next) => {
                if generation(home, shell).as_deref() == Some(&next) {
                    reports = Reports::new(shell, &next);
                    changed = true;
                }
            }
            report => {
                changed |= reports.apply(report, crate::activity::unix_now());
            }
        });
        if changed {
            shared.0.lock().unwrap_or_else(|p| p.into_inner()).0 = reports.value.clone();
        }
    };
    reports.value.connected = false;
    {
        let mut guard = shared.0.lock().unwrap_or_else(|p| p.into_inner());
        *guard = (reports.value, true);
    }
    shared.1.notify_one();
    let _ = writer.join();
    result
}

#[cfg(test)]
mod tests;
