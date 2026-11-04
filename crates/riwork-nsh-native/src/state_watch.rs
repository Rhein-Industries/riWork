//! `somtus watch --json`: wait until a topic of the desktop's data changes, for the remote
//! connector's `state.watch` (`docs/remote-protocol.md`). One call per wait, from another
//! process: the desktop app's own watcher and hubs are not reachable from here.
//!
//! The request on standard input is `{"since":{TOPIC:REV|null,…,"tabs":{PROJECT:REV|null}},
//! "wait_ms":0..25000}`. A topic's revision is a digest of what that topic's reader would
//! answer (the store, the shells, one project's tabs, the settings) or of the files it reads
//! (automations, usage, the dashboard's inputs), so equal revisions mean nothing changed for
//! that reader and a file that was only touched wakes nobody. The answer is
//! `{"v":1,"changed":[TOPIC…],"revs":{…}}` as soon as a requested topic's revision differs
//! from `since` (null always differs: that is how a client learns the first ones), or
//! `{"v":1,"unchanged":true,"revs":{…}}` when `wait_ms` passed without one.
//!
//! It waits on a recursive watch of RIWORK_HOME, registered before the first read so no change
//! falls between them, and samples the shells every two seconds while `sessions`, `tabs` or
//! `dashboard` are asked for: tmux does not write a file when a shell starts or ends. An event
//! re-reads only the topics its file belongs to.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant, UNIX_EPOCH},
};

use notify::{EventKind, RecursiveMode, Watcher};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::{
    backend::{
        CoreStoreBackend, EngineSessionsBackend, EngineTabsBackend, SessionsBackend, StoreBackend,
        TabsBackend,
    },
    settings::{CoreSettingsBackend, SettingsBackend, SettingsDto},
};

/// Every topic, as `since`, `changed` and `revs` name them.
pub const TOPICS: [&str; 7] = [
    "store",
    "sessions",
    "tabs",
    "settings",
    "schedules",
    "usage",
    "dashboard",
];
/// The longest wait, as for `chat.events`.
pub const MAX_WAIT_MS: u64 = 25_000;
/// The most projects whose tabs one request follows.
pub const MAX_TAB_PROJECTS: usize = 16;
/// How often the shells are sampled while a wait needs them.
const SAMPLE: Duration = Duration::from_secs(2);
/// Events that come together (a rename and its directory) are read once.
const SETTLE: Duration = Duration::from_millis(50);

const STORE: u8 = 1;
const SESSIONS: u8 = 2;
const TABS: u8 = 4;
const SETTINGS: u8 = 8;
const SCHEDULES: u8 = 16;
const USAGE: u8 = 32;
const DASHBOARD: u8 = 64;
const SAMPLED: u8 = SESSIONS | TABS | DASHBOARD;

fn bit(topic: &str) -> u8 {
    match topic {
        "store" => STORE,
        "sessions" => SESSIONS,
        "tabs" => TABS,
        "settings" => SETTINGS,
        "schedules" => SCHEDULES,
        "usage" => USAGE,
        "dashboard" => DASHBOARD,
        _ => 0,
    }
}

/// What one request asks about: topics and, for `tabs`, projects, each with the revision the
/// client has (`None` for null).
struct Request {
    topics: BTreeMap<&'static str, Option<String>>,
    tabs: BTreeMap<String, Option<String>>,
    wait: Duration,
}

impl Request {
    fn wanted(&self) -> u8 {
        self.topics.keys().fold(0, |bits, topic| bits | bit(topic))
    }
}

fn revision(value: &Value) -> Result<Option<String>, String> {
    match value {
        Value::Null => Ok(None),
        Value::String(rev)
            if (1..=64).contains(&rev.len()) && rev.bytes().all(|b| b.is_ascii_graphic()) =>
        {
            Ok(Some(rev.clone()))
        }
        _ => Err("invalid: a revision is null or 1 to 64 printable ASCII characters".into()),
    }
}

fn parse(input: &[u8]) -> Result<Request, String> {
    let value: Value =
        serde_json::from_slice(input).map_err(|error| format!("invalid: {error}"))?;
    let Value::Object(mut request) = value else {
        return Err("invalid: the request must be a JSON object".into());
    };
    let since = request.remove("since");
    let wait = request.remove("wait_ms");
    if let Some(key) = request.keys().next() {
        return Err(format!("invalid: unknown field `{key}`"));
    }
    let Some(Value::Object(since)) = since else {
        return Err("invalid: since must be an object".into());
    };
    let wait = wait
        .as_ref()
        .and_then(Value::as_u64)
        .filter(|ms| *ms <= MAX_WAIT_MS)
        .ok_or(format!("invalid: wait_ms must be 0 to {MAX_WAIT_MS}"))?;
    let mut topics = BTreeMap::new();
    let mut tabs = BTreeMap::new();
    for (key, value) in since {
        let topic = TOPICS
            .iter()
            .find(|topic| **topic == key)
            .ok_or(format!("invalid: unknown topic `{key}`"))?;
        if *topic == "tabs" {
            let Value::Object(projects) = value else {
                return Err("invalid: since.tabs maps project ids to revisions".into());
            };
            if projects.is_empty() || projects.len() > MAX_TAB_PROJECTS {
                return Err(format!(
                    "invalid: since.tabs names 1 to {MAX_TAB_PROJECTS} projects"
                ));
            }
            for (project, rev) in projects {
                crate::service::validate_uuid(&project)?;
                tabs.insert(project, revision(&rev)?);
            }
            topics.insert(*topic, None);
        } else {
            topics.insert(*topic, revision(&value)?);
        }
    }
    if topics.is_empty() {
        return Err("invalid: since names no topic".into());
    }
    Ok(Request {
        topics,
        tabs,
        wait: Duration::from_millis(wait),
    })
}

fn digest(bytes: &[u8]) -> String {
    let hash = Sha256::digest(bytes);
    hash[..8].iter().map(|byte| format!("{byte:02x}")).collect()
}

fn digest_of<T: serde::Serialize>(read: anyhow::Result<T>) -> String {
    match read {
        Ok(value) => digest(&serde_json::to_vec(&value).unwrap_or_default()),
        // A reader that fails is a state of its own: its recovery is a change.
        Err(error) => digest(format!("error: {error}").as_bytes()),
    }
}

/// Name, size and modification time of the files under `dir` that `keep` accepts, `depth`
/// levels deep at most, in a stable order. Contents are not read.
fn stamps(dir: &Path, depth: usize, keep: &dyn Fn(&Path) -> bool, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries = entries
        .flatten()
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    entries.sort();
    for path in entries {
        if out.len() >= 4096 {
            return;
        }
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.is_dir() {
            if depth > 1 {
                stamps(&path, depth - 1, keep, out);
            }
        } else if keep(&path) {
            let modified = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |time| time.as_nanos());
            out.push(format!(
                "{}\t{}\t{modified}",
                path.display(),
                metadata.len()
            ));
        }
    }
}

fn file_digest(path: &Path) -> String {
    match std::fs::read(path) {
        Ok(bytes) => digest(&bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => digest(b"absent"),
        Err(error) => digest(format!("error: {error}").as_bytes()),
    }
}

/// The readers a wait uses, built once.
struct Readers {
    home: PathBuf,
    store: CoreStoreBackend,
    sessions: EngineSessionsBackend,
    tabs: EngineTabsBackend,
}

/// The revisions last read, per topic and per project for `tabs`.
#[derive(Clone, Default, PartialEq)]
struct Revs {
    topics: BTreeMap<&'static str, String>,
    tabs: BTreeMap<String, String>,
}

impl Readers {
    fn new(home: PathBuf) -> Self {
        Self {
            store: CoreStoreBackend::new(home.clone()),
            sessions: EngineSessionsBackend::new(home.clone()),
            tabs: EngineTabsBackend::new(home.clone()),
            home,
        }
    }

    fn usage(&self) -> String {
        let mut out = Vec::new();
        stamps(&self.home.join("usage"), 3, &|_| true, &mut out);
        digest(out.join("\n").as_bytes())
    }

    /// Re-reads the topics in `dirty` that the request wants.
    fn read(&self, request: &Request, dirty: u8, revs: &mut Revs) {
        let wanted = request.wanted() & dirty;
        // The dashboard is built from these; read each once.
        let need = |bits: u8| wanted & bits != 0;
        let store = need(STORE | DASHBOARD).then(|| digest_of(self.store.snapshot()));
        let sessions = need(SESSIONS | DASHBOARD).then(|| digest_of(self.sessions.list()));
        let schedules =
            need(SCHEDULES | DASHBOARD).then(|| file_digest(&self.home.join("schedules.json")));
        let usage = need(USAGE | DASHBOARD).then(|| self.usage());
        for (topic, value) in [
            ("store", &store),
            ("sessions", &sessions),
            ("schedules", &schedules),
            ("usage", &usage),
        ] {
            if let Some(value) = value
                && request.topics.contains_key(topic)
            {
                revs.topics.insert(topic, value.clone());
            }
        }
        if need(SETTINGS) {
            let settings = CoreSettingsBackend::new(self.home.clone())
                .and_then(|backend| backend.get())
                .map(|settings| SettingsDto::from_settings(&settings, None))
                .map_err(anyhow::Error::msg);
            revs.topics.insert("settings", digest_of(settings));
        }
        if need(TABS) {
            for project in request.tabs.keys() {
                revs.tabs
                    .insert(project.clone(), digest_of(self.tabs.snapshot(project)));
            }
        }
        if need(DASHBOARD) {
            let mut chats = Vec::new();
            stamps(
                &self.home.join("chats"),
                2,
                &|path| path.file_name().is_some_and(|name| name == "info.json"),
                &mut chats,
            );
            let inputs = [
                store.unwrap_or_default(),
                sessions.unwrap_or_default(),
                schedules.unwrap_or_default(),
                usage.unwrap_or_default(),
                chats.join("\n"),
            ];
            revs.topics
                .insert("dashboard", digest(inputs.join("\n").as_bytes()));
        }
    }
}

impl Revs {
    fn changed(&self, request: &Request) -> Vec<&'static str> {
        request
            .topics
            .iter()
            .filter(|(topic, since)| {
                if **topic == "tabs" {
                    request
                        .tabs
                        .iter()
                        .any(|(project, since)| since.as_ref() != self.tabs.get(project))
                } else {
                    since.as_ref() != self.topics.get(*topic)
                }
            })
            .map(|(topic, _)| *topic)
            .collect()
    }

    fn json(&self, request: &Request) -> Value {
        let mut out = Map::new();
        for topic in request.topics.keys() {
            if *topic == "tabs" {
                out.insert("tabs".into(), json!(self.tabs));
            } else if let Some(rev) = self.topics.get(topic) {
                out.insert((*topic).into(), json!(rev));
            }
        }
        Value::Object(out)
    }
}

/// Which topics a change of `path` (under `home`) may have changed.
fn affected(path: &Path, home: &Path) -> u8 {
    let Ok(relative) = path.strip_prefix(home) else {
        return 0;
    };
    let first = relative
        .components()
        .next()
        .and_then(|part| part.as_os_str().to_str())
        .unwrap_or("");
    match first {
        "state.json" | riwork_core::recency_file::FILE_NAME => STORE | DASHBOARD,
        "settings.json" => SETTINGS,
        "schedules.json" => SCHEDULES | DASHBOARD,
        "usage" => USAGE | DASHBOARD,
        "sessions.json" | "orchestrator-wakes.json" | "agent-activity" => {
            SESSIONS | TABS | DASHBOARD
        }
        "project-tabs" => TABS,
        "chats" if relative.file_name().is_some_and(|name| name == "info.json") => TABS | DASHBOARD,
        "chats" if relative.components().count() <= 2 => TABS | DASHBOARD,
        _ => 0,
    }
}

/// The answer to one `somtus watch --json` request, as the line it prints.
pub fn run(input: &[u8]) -> Value {
    match watch(input) {
        Ok(answer) => answer,
        Err(error) => crate::invoke::failure(&error),
    }
}

fn watch(input: &[u8]) -> Result<Value, String> {
    let request = parse(input)?;
    let home = riwork_core::paths::riwork_home().map_err(|error| format!("io: {error}"))?;
    riwork_core::paths::create_private_dir(&home).map_err(|error| format!("io: {error}"))?;
    let home = home
        .canonicalize()
        .map_err(|error| format!("io: {error}"))?;
    let readers = Readers::new(home.clone());
    let (events, received) = mpsc::channel::<u8>();
    let watch_home = home.clone();
    // Registered before the first read: a change cannot fall between the two.
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let bits = match event {
            Ok(event) if event.need_rescan() => u8::MAX,
            Ok(event) if !matches!(event.kind, EventKind::Access(_)) => event
                .paths
                .iter()
                .fold(0, |bits, path| bits | affected(path, &watch_home)),
            Ok(_) => 0,
            Err(_) => u8::MAX,
        };
        if bits != 0 {
            let _ = events.send(bits);
        }
    })
    .map_err(|error| format!("io: cannot watch the Somtus home: {error}"))?;
    if request.wait > Duration::ZERO {
        watcher
            .watch(&home, RecursiveMode::Recursive)
            .map_err(|error| format!("io: cannot watch the Somtus home: {error}"))?;
    }
    let deadline = Instant::now() + request.wait;
    let mut revs = Revs::default();
    readers.read(&request, u8::MAX, &mut revs);
    let answer = |revs: &Revs, changed: Vec<&'static str>| {
        if changed.is_empty() {
            json!({"v": 1, "unchanged": true, "revs": revs.json(&request)})
        } else {
            json!({"v": 1, "changed": changed, "revs": revs.json(&request)})
        }
    };
    let changed = revs.changed(&request);
    if !changed.is_empty() || request.wait == Duration::ZERO {
        return Ok(answer(&revs, changed));
    }
    let sampled = request.wanted() & SAMPLED != 0;
    let mut next_sample = Instant::now() + SAMPLE;
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Ok(answer(&revs, Vec::new()));
        }
        let until = if sampled {
            deadline.min(next_sample)
        } else {
            deadline
        };
        let mut dirty = match received.recv_timeout(until.saturating_duration_since(now)) {
            Ok(bits) => {
                std::thread::sleep(SETTLE);
                let mut bits = bits;
                while let Ok(more) = received.try_recv() {
                    bits |= more;
                }
                bits
            }
            Err(mpsc::RecvTimeoutError::Timeout) => 0,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("io: the Somtus home watcher stopped".into());
            }
        };
        if sampled && Instant::now() >= next_sample {
            dirty |= SAMPLED;
            next_sample = Instant::now() + SAMPLE;
        }
        if dirty & request.wanted() == 0 {
            continue;
        }
        readers.read(&request, dirty, &mut revs);
        let changed = revs.changed(&request);
        if !changed.is_empty() {
            return Ok(answer(&revs, changed));
        }
    }
}
