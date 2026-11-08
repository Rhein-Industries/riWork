//! Host-owned session metadata. Connection health is never an agent state.
//! The wire contains no transcripts, prompts, paths, credentials or approval payloads.
use crate::{
    activity,
    chat::{log, model::ChatState},
    orchestrators::{ChatHost, running_chats},
    sessions::SessionManager,
};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::Path,
};

pub const VERSION: u32 = 1;
pub const MAX_SESSIONS: usize = 512;
pub const MAX_HOSTS: usize = 32;
pub const MAX_CACHE_BYTES: usize = 4 * 1024 * 1024;
const MAX_SEEN_BYTES: usize = MAX_CACHE_BYTES / 4;
/// Above this many acknowledgements `Cache::open` discards the whole file.
const MAX_SEEN: usize = 32768;
/// A row last read longer ago than this says nothing about the link now, if nobody was watching.
/// Rows that are being polled are not held to it: a read can take about 25 s and backoff reaches
/// 32 s.
const UNOBSERVED_LIMIT_SECS: u64 = 30;
/// For how many reads after first contact a completion with no time can still be history. Logs
/// are read a budget at a time, and those of running chats again on every read, so old logs may
/// be covered as few as one a read, of the up to 128 chats a read observes (`MAX_CHATS` in
/// `global_overview/observed.rs`). Reads stop while nobody watches, so this counts reads rather
/// than time. Only a chat listed before can be history this way (see `Cache::seed_history`), so a
/// long window swallows no chat that first turns up later.
const BASELINE_READS: u32 = 128;
/// The most acknowledgements first contact records for one host: two full inventories, and few
/// enough that no host can push the shared table over `MAX_SEEN`.
const MAX_SEEDED: u32 = 1024;
/// How much later than the newest observation's receipt a live route must have answered before
/// its data is shown instead; see `Cache::rows`. Well above how long one read can take.
const LIVE_RECEIPT_SECS: u64 = 60;
const KINDS: [&str; 2] = ["chat", "terminal"];
const STATES: [&str; 10] = [
    "starting", "working", "waiting", "blocked", "error", "idle", "done", "stopped", "exited",
    "unknown",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: String,
    pub kind: String,
    pub project_id: Option<String>,
    pub parent_id: Option<String>,
    pub title: String,
    pub provider: Option<String>,
    pub state: String,
    pub source: String,
    pub since: Option<u64>,
    pub subagents: Option<usize>,
    pub requests: Vec<String>,
    pub completion: Option<String>,
    #[serde(default)]
    pub unseen: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalSnapshot {
    pub version: u32,
    pub host_id: String,
    pub observed_at: u64,
    pub capabilities: Vec<String>,
    pub sessions: Vec<Session>,
    pub total: usize,
    pub partial: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Host {
    /// Stable execution identity, independent of the currently used route.
    pub id: String,
    /// Local connection registry key, used only for routing to this host.
    pub connection_id: Option<String>,
    pub label: String,
    pub route: String,
    pub connection: String,
    pub freshness: String,
    /// Client clock, distinct from the originating host's observed_at.
    pub received_at: Option<u64>,
    pub error: Option<String>,
    pub snapshot: Option<LocalSnapshot>,
    /// When the registry says this route's record was added. A relay route that is removed and
    /// paired again keeps its id, so this is what shows the row describes an older record.
    #[serde(default)]
    pub added_at: Option<u64>,
}
impl Host {
    pub fn pending(connection_id: Option<String>, label: String, route: &str) -> Self {
        Self {
            id: connection_id.clone().unwrap_or_default(),
            connection_id,
            label: bounded(&label, 256),
            route: bounded(route, 64),
            connection: "connecting".into(),
            freshness: "unknown".into(),
            received_at: None,
            error: None,
            snapshot: None,
            added_at: None,
        }
    }
    /// A failed read leaves both the last known session state and completion token intact.
    pub fn failed(&mut self, error: String, blocked: bool) {
        self.connection = if blocked { "blocked" } else { "backoff" }.into();
        self.freshness = if self.snapshot.is_some() {
            "stale"
        } else {
            "unknown"
        }
        .into();
        self.error = Some(bounded(&error, 300));
    }
    /// The link answered and the host could not report: the connection is fine and only the data
    /// is behind. Like `failed`, this leaves the last known state intact.
    pub fn report_failed(&mut self, error: String) {
        self.failed(error, false);
        self.connection = "online".into();
    }
    /// The link is still being made. That is no failure, and it says nothing about why a row is
    /// blocked, so the block stays while a daemon restarts rather than flickering away.
    pub fn connecting(&mut self) {
        if self.connection == "blocked" {
            return;
        }
        self.connection = "connecting".into();
        self.freshness = if self.snapshot.is_some() {
            "stale"
        } else {
            "unknown"
        }
        .into();
        self.error = None;
    }
    /// Reads stop while nobody watches, so a row kept from before cannot still claim to be
    /// connected and current. Only a row that says the link is up can claim that: one that says
    /// it is down, blocked or still being made is already honest about the link and its data.
    /// The data and its receive time stay. Returns whether the row changed.
    fn age_unobserved(&mut self, now: u64) -> bool {
        let old = self
            .received_at
            .is_some_and(|at| now.saturating_sub(at) > UNOBSERVED_LIMIT_SECS);
        if !old || self.connection != "online" {
            return false;
        }
        self.connection = "connecting".into();
        self.freshness = "stale".into();
        true
    }
    /// One read applied to this row alone. The service reads through `Cache::apply_read`.
    #[cfg(test)]
    pub fn apply(&mut self, snapshot: LocalSnapshot, received_at: u64) -> Result<(), String> {
        self.accept(snapshot, received_at, true).map(|_| ())
    }
    /// `pinned` refuses a snapshot from any identity but the one the row already holds. Returns
    /// the snapshot it replaces.
    fn accept(
        &mut self,
        mut snapshot: LocalSnapshot,
        received_at: u64,
        pinned: bool,
    ) -> Result<Option<LocalSnapshot>, String> {
        // Limits are checked as sent, so dropping a row cannot hide a malformed snapshot.
        snapshot.check_limits()?;
        snapshot.normalize();
        if pinned
            && let Some(previous) = &self.snapshot
            && previous.host_id != snapshot.host_id
        {
            return Err("The connection now identifies a different host. Remove and pair it again before using it.".into());
        }
        self.id = snapshot.host_id.clone();
        let previous = self.snapshot.replace(snapshot);
        self.received_at = Some(received_at);
        self.connection = "online".into();
        self.freshness = "current".into();
        self.error = None;
        Ok(previous)
    }
    fn valid_cached(&self) -> bool {
        plain(&self.id, 256)
            && plain(&self.label, 2048)
            && plain(&self.route, 64)
            && self.connection_id.as_ref().is_none_or(|v| plain(v, 256))
            && self.error.as_ref().is_none_or(|v| plain(v, 2048))
            && matches!(
                self.connection.as_str(),
                "connecting" | "online" | "blocked" | "backoff"
            )
            && matches!(self.freshness.as_str(), "unknown" | "stale" | "current")
            && self
                .snapshot
                .as_ref()
                .is_some_and(|s| self.id == s.host_id && s.validate().is_ok())
    }
}
fn plain(value: &str, limit: usize) -> bool {
    value.len() <= limit && !value.chars().any(char::is_control)
}
impl LocalSnapshot {
    pub fn validate(&self) -> Result<(), String> {
        self.check_limits()?;
        if self
            .sessions
            .iter()
            .any(|s| !KINDS.contains(&s.kind.as_str()) || !STATES.contains(&s.state.as_str()))
        {
            return Err("Invalid host session metadata".into());
        }
        Ok(())
    }
    /// The protocol is additive, so a newer host may use words this build has never seen. That
    /// should cost the row some detail, not the host its whole inventory. Nothing here acts on a
    /// `state`, so an unknown one reads as unknown. A `kind` decides where a click goes, so a row
    /// of an unknown kind cannot be kept, and the snapshot then says it is incomplete.
    fn normalize(&mut self) {
        let sent = self.sessions.len();
        self.sessions.retain(|s| KINDS.contains(&s.kind.as_str()));
        self.partial |= self.sessions.len() < sent;
        for s in &mut self.sessions {
            if !STATES.contains(&s.state.as_str()) {
                s.state = "unknown".into();
            }
        }
    }
    /// Everything about a snapshot from another machine except which words it uses.
    fn check_limits(&self) -> Result<(), String> {
        if self.version != VERSION || uuid::Uuid::parse_str(&self.host_id).is_err() {
            return Err("Incompatible host session protocol or invalid host identity".into());
        }
        if self.sessions.len() > MAX_SESSIONS
            || self.capabilities.len() > 32
            || self.capabilities.iter().any(|value| !plain(value, 64))
            || self.total < self.sessions.len()
            || self.total > self.sessions.len() && !self.partial
        {
            return Err("Host session metadata exceeds its limits".into());
        }
        let mut ids = BTreeSet::new();
        for s in &self.sessions {
            if s.id.is_empty()
                || !plain(&s.id, 256)
                || !ids.insert((&s.kind, &s.id))
                || !plain(&s.kind, 64)
                || !plain(&s.state, 64)
                || !plain(&s.title, 2048)
                || s.requests.len() > 8
                || s.requests.iter().any(|s| !plain(s, 64))
                || s.provider.as_ref().is_some_and(|s| !plain(s, 64))
                || !plain(&s.source, 64)
                || s.completion.as_ref().is_some_and(|s| !plain(s, 256))
                || s.project_id.as_ref().is_some_and(|s| !plain(s, 256))
                || s.parent_id.as_ref().is_some_and(|s| !plain(s, 256))
            {
                return Err("Invalid host session metadata".into());
            }
        }
        Ok(())
    }
}

pub fn bounded(value: &str, max: usize) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect()
}

/// A fully written identity is published atomically under a cross-process lock.
/// Corrupt identity is an error, never an excuse to silently become another host.
pub fn identity(home: &Path) -> Result<String, String> {
    fs::create_dir_all(home).map_err(|e| e.to_string())?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(home.join("host-identity.lock"))
        .map_err(|e| e.to_string())?;
    lock.lock_exclusive().map_err(|e| e.to_string())?;
    let path = home.join("host-identity");
    let saved = fs::File::open(&path).and_then(|file| {
        let mut text = String::new();
        file.take(129).read_to_string(&mut text)?;
        Ok(text)
    });
    match saved {
        Ok(id) if uuid::Uuid::parse_str(id.trim()).is_ok() => Ok(id.trim().into()),
        Ok(_) => Err("Invalid saved host identity; restore host-identity from backup".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let id = uuid::Uuid::new_v4().to_string();
            atomic_json_bytes(&path, id.as_bytes())?;
            Ok(id)
        }
        Err(e) => Err(e.to_string()),
    }
}

pub fn atomic_json_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;
    let parent = path.parent().ok_or("Missing parent directory")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let temp = parent.join(format!(".hosts-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temp)
            .map_err(|e| e.to_string())?;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        fs::rename(&temp, path).map_err(|e| e.to_string())?;
        fs::File::open(parent)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())
    })();
    let _ = fs::remove_file(temp);
    result
}

pub fn local(home: &Path) -> Result<LocalSnapshot, String> {
    local_with(home, crate::sessions::tmux_installed())
}

fn local_with(home: &Path, tmux_installed: bool) -> Result<LocalSnapshot, String> {
    let now = activity::unix_now();
    let host_id = identity(home)?;
    // No tmux means no terminals, a true answer for a chat-only host. A tmux that fails or an
    // unreadable session registry is different: a read (`Cache::apply_read`) replaces the host's
    // whole snapshot, so reporting nothing here would make those rows vanish where they must stay
    // last known. Shells that are registered with no tmux to be found may live on under one this
    // read cannot see, say through a PATH it lacks, so the inventory then says it may be
    // incomplete.
    let (shells, unlisted) = if tmux_installed {
        let shells = SessionManager::at(home.to_path_buf())?
            .sample(false)?
            .shells;
        (shells, false)
    } else {
        let registered = crate::sessions::shells_registered(home).unwrap_or(true);
        (Vec::new(), registered)
    };
    let states = activity::states_once(home, &shells, now);
    let never_start = |_: &Path| Err("Monitoring starts no chat host".to_owned());
    let live_chats = running_chats(&ChatHost {
        home,
        ensure: &never_start,
    });
    let chats_live = live_chats.is_some();
    let chats = match live_chats {
        Some(chats) => chats,
        None => log::read_infos_checked(home)?,
    };
    // The store only names projects and branches in the dashboard's summaries, which nothing here
    // reads, so one this build cannot parse (a newer schema, say) must not hide the chats.
    let store = crate::store::Store::open(home)
        .and_then(|store| store.snapshot())
        .unwrap_or_default();
    let observations = crate::global_overview::observed::read(home, &store, Some(&chats), now);
    let mut sessions = Vec::new();
    for chat in &chats {
        let status = match &chat.state {
            ChatState::Starting => "starting",
            ChatState::Running => "working",
            ChatState::Waiting => "waiting",
            ChatState::Idle => "idle",
            ChatState::Stopped => "stopped",
            ChatState::Failed { .. } => "error",
        };
        let status = if !chats_live && matches!(status, "starting" | "working" | "waiting") {
            "unknown"
        } else {
            status
        };
        let requests: BTreeSet<_> = observations
            .decisions
            .iter()
            .filter(|d| d.chat_id == chat.id)
            .map(|d| match d.request {
                crate::global_overview::observed::Request::Approval { .. } => {
                    "permission".to_owned()
                }
                crate::global_overview::observed::Request::Question { .. } => "question".to_owned(),
            })
            .collect();
        let ended = observations
            .completed_turns
            .iter()
            .filter(|a| a.source.chat_id == chat.id)
            .max_by_key(|a| a.source.event_seq);
        let ended = ended.filter(|_| matches!(status, "idle" | "error" | "stopped"));
        let completion = ended
            .filter(|event| matches!(event.outcome.as_str(), "completed" | "failed"))
            .map(|event| format!("turn:{}:{}", event.source.event_seq, event.outcome));
        let since = ended
            .filter(|event| {
                matches!(
                    (status, event.outcome.as_str()),
                    ("idle", "completed") | ("error", "failed")
                )
            })
            .and_then(|event| event.ended_at.map(|ms| ms / 1000));
        sessions.push(Session {
            id: chat.id.clone(),
            kind: "chat".into(),
            project_id: chat.project_id.clone(),
            parent_id: chat.parent_id.clone(),
            title: bounded(&chat.title, 256),
            provider: Some(
                serde_json::to_value(chat.provider)
                    .unwrap()
                    .as_str()
                    .unwrap_or("unknown")
                    .into(),
            ),
            state: status.into(),
            source: if chats_live { "provider" } else { "saved" }.into(),
            since,
            subagents: None,
            requests: if chats_live {
                requests.into_iter().collect()
            } else {
                Vec::new()
            },
            completion,
            unseen: false,
        });
    }
    for shell in shells {
        let state = states.get(&shell.id).cloned().unwrap_or_default();
        let status = if shell.alive {
            state.activity.as_str()
        } else {
            "exited"
        };
        sessions.push(Session {
            id: shell.id.clone(),
            kind: "terminal".into(),
            project_id: shell.project_id,
            parent_id: shell.parent_id,
            title: shell
                .harness
                .map(|h| h.program().to_owned())
                .unwrap_or_else(|| "Terminal".into()),
            provider: shell.harness.map(|h| h.program().to_owned()),
            state: status.into(),
            source: if state.since_unix.is_some() {
                "hook"
            } else {
                "process"
            }
            .into(),
            since: state.since_unix,
            subagents: Some(state.subagents.working),
            requests: vec![],
            completion: if status == "done" {
                state.since_unix.map(|n| format!("done:{n}"))
            } else {
                None
            },
            unseen: false,
        });
    }
    for session in sessions
        .iter_mut()
        .filter(|s| s.kind == "terminal" && s.state != "exited")
    {
        if let Some(report) = crate::program_status::snapshot(home, &session.id) {
            apply_program_status(session, &report);
        }
    }
    // Delegated Somtus chats carry their actual parent identity.
    let working: Vec<_> = sessions
        .iter()
        .filter(|s| s.state == "working")
        .filter_map(|s| s.parent_id.clone())
        .collect();
    for row in &mut sessions {
        let n = working.iter().filter(|id| *id == &row.id).count();
        if n > 0 {
            row.subagents = Some(row.subagents.unwrap_or(0) + n);
        }
    }
    let total = sessions.len();
    keep_most_relevant(&mut sessions, MAX_SESSIONS);
    Ok(LocalSnapshot {
        version: VERSION,
        host_id,
        observed_at: now,
        capabilities: vec![
            "chats".into(),
            "terminals".into(),
            "agentStatus".into(),
            "osc7501".into(),
            "acp".into(),
        ],
        sessions,
        total,
        partial: total > MAX_SESSIONS
            || observations.chats_complete < observations.chats_total
            || unlisted,
    })
}

/// How much a person needs a row when not all of them fit, most first: anything asking for them,
/// then work in progress, then a finished row they may not have seen, and the newest of each. The
/// ids make the order total, so the same rows stay in across reads.
fn relevance(row: &Session) -> (u8, Reverse<Option<u64>>, &str, &str) {
    let class = if !row.requests.is_empty()
        || matches!(row.state.as_str(), "waiting" | "blocked" | "error")
    {
        0
    } else if matches!(row.state.as_str(), "working" | "starting") {
        1
    } else if row.completion.is_some() {
        2
    } else {
        3
    };
    (class, Reverse(row.since), &row.kind, &row.id)
}

fn by_kind_and_id(a: &Session, b: &Session) -> std::cmp::Ordering {
    a.kind.cmp(&b.kind).then(a.id.cmp(&b.id))
}

fn keep_most_relevant(rows: &mut Vec<Session>, limit: usize) {
    if rows.len() > limit {
        rows.sort_by(|a, b| relevance(a).cmp(&relevance(b)));
        rows.truncate(limit);
    }
    rows.sort_by(by_kind_and_id);
}

fn apply_program_status(session: &mut Session, report: &crate::program_status::Snapshot) {
    let Some(root) = &report.root else { return };
    if !report.connected {
        if session.since.is_some_and(|since| since > root.since_unix) {
            return;
        }
        // A newer disconnected producer cannot resurrect an older hook's Done.
        session.state = "unknown".into();
        session.source = "osc7501-stale".into();
        session.since = None;
        session.requests.clear();
        session.completion = None;
        session.subagents = None;
        return;
    }
    session.state = serde_json::to_value(root.state)
        .unwrap()
        .as_str()
        .unwrap()
        .into();
    session.source = "osc7501".into();
    session.since = Some(root.since_unix);
    session.requests = root
        .kind
        .map(|kind| vec![serde_json::to_value(kind).unwrap().as_str().unwrap().into()])
        .unwrap_or_default();
    session.completion = matches!(
        root.state,
        crate::program_status::State::Done | crate::program_status::State::Error
    )
    .then(|| {
        format!(
            "{}:{}:{}",
            report.generation, session.state, root.since_unix
        )
    });
    if let Some(title) = &root.title {
        session.title = bounded(title, 256);
    }
    if let Some(app) = &root.app {
        session.provider = Some(app.clone());
    }
    session.subagents = Some(
        report
            .records
            .values()
            .filter(|r| r.state == crate::program_status::State::Working)
            .count(),
    );
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Cache {
    pub hosts: BTreeMap<String, Host>,
    pub seen: BTreeMap<String, String>,
    /// When each host in `hosts` was first read, by execution identity and on its own clock.
    #[serde(default)]
    pub baselines: BTreeMap<String, u64>,
    /// How far first contact with each host in `baselines` has got.
    #[serde(default)]
    pub first_contact: BTreeMap<String, FirstContact>,
}
/// Counted from a host's baseline on, and kept across restarts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FirstContact {
    /// Successful reads.
    pub reads: u32,
    /// Acknowledgements recorded as history.
    pub seeded: u32,
}
pub fn session_key(host: &str, kind: &str, session: &str) -> String {
    serde_json::to_string(&(host, kind, session)).unwrap()
}
/// The host a `session_key` belongs to.
fn key_host(key: &str) -> Option<String> {
    serde_json::from_str::<(String, String, String)>(key)
        .ok()
        .map(|(host, _, _)| host)
}
/// Which of one host's routes to show the data of: the newest observation, a tie staying with
/// `chosen`. When that observation is on a route that is not up while a live route has answered
/// well after it, the host's clock may have been set back meanwhile, and the live route's data
/// is shown instead of a past it can no longer correct.
fn newest_observation<'a>(routes: &[&'a Host], chosen: &'a Host) -> &'a Host {
    let observed = |h: &Host| h.snapshot.as_ref().map(|s| s.observed_at);
    let mut newest = chosen;
    for route in routes {
        if observed(route) > observed(newest) {
            newest = route;
        }
    }
    if newest.connection == "online" {
        return newest;
    }
    let after = newest
        .received_at
        .unwrap_or(0)
        .saturating_add(LIVE_RECEIPT_SECS);
    let mut live: Option<&Host> = None;
    for route in routes.iter().copied().filter(|route| {
        route.connection == "online" && route.received_at.is_some_and(|at| at > after)
    }) {
        if live.is_none_or(|best| observed(route) > observed(best)) {
            live = Some(route);
        }
    }
    live.unwrap_or(newest)
}
impl Cache {
    pub fn open(home: &Path) -> Self {
        let mut bytes = Vec::new();
        let Ok(file) = fs::File::open(home.join("host-sessions.json")) else {
            return Self::default();
        };
        if file
            .take(MAX_CACHE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .is_err()
            || bytes.len() > MAX_CACHE_BYTES
        {
            return Self::default();
        }
        let Ok(mut cache) = serde_json::from_slice::<Self>(&bytes) else {
            return Self::default();
        };
        if cache.hosts.len() > MAX_HOSTS + 1 || cache.seen.len() > MAX_SEEN {
            return Self::default();
        }
        cache
            .hosts
            .retain(|key, host| plain(key, 256) && host.valid_cached());
        cache
            .seen
            .retain(|key, token| plain(key, 2048) && plain(token, 256));
        cache.prune_baselines();
        for host in cache.hosts.values_mut() {
            host.connection = "connecting".into();
            host.freshness = "stale".into();
        }
        cache
    }
    /// Persist bounded partial inventories rather than write a file the reader must
    /// discard. Budget each route fairly; all host identities and receipt times survive.
    /// The live cache stays complete. Its next successful read restores trimmed rows.
    pub fn save(&self, home: &Path) -> Result<(), String> {
        self.clone().save_owned(home)
    }
    /// Persist a detached snapshot without cloning its complete inventory again.
    /// Validation and byte-budget trimming consume only this owned cache.
    pub fn save_owned(self, home: &Path) -> Result<(), String> {
        let mut retained = self;
        retained
            .hosts
            .retain(|key, host| plain(key, 256) && host.valid_cached());
        while retained.hosts.len() > MAX_HOSTS + 1 {
            retained.hosts.pop_last();
        }
        let live_keys: BTreeSet<_> = retained
            .hosts
            .values()
            .flat_map(|host| {
                host.snapshot.iter().flat_map(move |s| {
                    s.sessions
                        .iter()
                        .map(move |row| session_key(&host.id, &row.kind, &row.id))
                })
            })
            .collect();
        // A partial inventory cannot show that a session is gone, so its host's other
        // acknowledgements stay. They only get what the rows on the list leave of the budget.
        let partial_hosts: BTreeSet<_> = retained
            .hosts
            .values()
            .filter_map(|host| host.snapshot.as_ref())
            .filter(|snapshot| snapshot.partial)
            .map(|snapshot| snapshot.host_id.clone())
            .collect();
        retained.prune_baselines();
        let mut seen_bytes = 0usize;
        let mut fits = |key: &String, token: &String| {
            let size =
                serde_json::to_vec(&(key, token)).map_or(MAX_SEEN_BYTES + 1, |v| v.len() + 2);
            if seen_bytes + size > MAX_SEEN_BYTES {
                return false;
            }
            seen_bytes += size;
            true
        };
        let (live, others): (BTreeMap<_, _>, BTreeMap<_, _>) = std::mem::take(&mut retained.seen)
            .into_iter()
            .filter(|(key, token)| plain(key, 2048) && plain(token, 256))
            .partition(|(key, _)| live_keys.contains(key));
        retained.seen = live;
        retained.seen.retain(|key, token| fits(key, token));
        retained
            .seen
            .extend(others.into_iter().filter(|(key, token)| {
                key_host(key).is_some_and(|host| partial_hosts.contains(&host)) && fits(key, token)
            }));
        // Reserve JSON/map punctuation and the seen inventory separately.
        let budget = (MAX_CACHE_BYTES - MAX_SEEN_BYTES - 64 * 1024) / retained.hosts.len().max(1);
        for host in retained.hosts.values_mut() {
            let Some(snapshot) = host.snapshot.as_mut() else {
                continue;
            };
            let mut rows = std::mem::take(&mut snapshot.sessions);
            rows.sort_by(|a, b| relevance(a).cmp(&relevance(b)));
            let mut used = serde_json::to_vec(&host).map_err(|e| e.to_string())?.len();
            let snapshot = host.snapshot.as_mut().unwrap();
            for row in rows {
                let size = serde_json::to_vec(&row).map_err(|e| e.to_string())?.len() + 1;
                if used + size > budget {
                    snapshot.partial = true;
                    continue;
                }
                used += size;
                snapshot.sessions.push(row);
            }
            snapshot.sessions.sort_by(by_kind_and_id);
        }
        let bytes = serde_json::to_vec(&retained).map_err(|e| e.to_string())?;
        if bytes.len() > MAX_CACHE_BYTES {
            return Err("Host cache exceeded its byte budget".into());
        }
        atomic_json_bytes(&home.join("host-sessions.json"), &bytes)
    }
    /// One completed read, as the service applies it. The row accepts it knowing only itself; this
    /// adds what takes the whole cache: this machine's own identity, and the baseline taken on
    /// first contact with a host.
    pub fn apply_read(
        &mut self,
        route: &str,
        snapshot: LocalSnapshot,
        received_at: u64,
    ) -> Result<(), String> {
        let id = snapshot.host_id.clone();
        let observed_at = snapshot.observed_at;
        // A row holding this identity with no baseline comes from a build before baselines. Its
        // unseen rows were already on show, so nothing about this read is first contact.
        let met = self.baselines.contains_key(&id)
            || self
                .hosts
                .values()
                .any(|host| host.snapshot.as_ref().is_some_and(|s| s.host_id == id));
        let Some(host) = self.hosts.get_mut(route) else {
            return Ok(());
        };
        // `identity()` is the authority on this machine and there is no connection to replace, so
        // a new identity takes over here instead of being refused.
        let previous = host.accept(snapshot, received_at, route != "local")?;
        if !met {
            self.baselines.insert(id.clone(), observed_at);
            self.prune_baselines();
        }
        // This machine's row shows only what this machine read (see `rows`), so another route's
        // answer for its identity has no history to add, nor any first reads to use up.
        let this_machine = route != "local"
            && self
                .hosts
                .get("local")
                .and_then(|host| host.snapshot.as_ref())
                .is_some_and(|snapshot| snapshot.host_id == id);
        // This machine's last snapshot may be of an identity it no longer has.
        let previous = previous.filter(|previous| previous.host_id == id);
        if !this_machine {
            self.seed_history(route, &id, observed_at, previous.as_ref());
        }
        Ok(())
    }
    /// Completions from before first contact are history rather than news, or adding a host would
    /// flood the list with them. A completion whose time is not after the baseline is history
    /// however late a read turns it up. One with no time cannot be told apart, so it counts as
    /// history only during the first reads, while old logs are still being covered, and only on a
    /// chat that was listed before: one that first turns up later, as after a pause in the reads,
    /// is new. Nor is it history on a row last seen at work. A completion after the baseline is
    /// never swallowed, and neither is one already on show: a token already recorded is not
    /// replaced, since a new one on that row means something new finished, and a row that already
    /// showed a completion stays as it was shown. `previous` is what this route showed before this
    /// read.
    fn seed_history(
        &mut self,
        route: &str,
        id: &str,
        observed_at: u64,
        previous: Option<&LocalSnapshot>,
    ) {
        let Some(&baseline) = self.baselines.get(id) else {
            return;
        };
        let contact = self.first_contact.entry(id.to_owned()).or_default();
        let reads = contact.reads;
        contact.reads = reads.saturating_add(1);
        // A clock behind the baseline, set back or ahead at first contact, cannot tell which side
        // of it a time falls on.
        if observed_at < baseline {
            return;
        }
        let Some(snapshot) = self
            .hosts
            .get(route)
            .and_then(|host| host.snapshot.as_ref())
        else {
            return;
        };
        // What this host already showed, through this route or another one.
        let others = self
            .hosts
            .iter()
            .filter(|(key, _)| key.as_str() != route)
            .filter_map(|(_, host)| host.snapshot.as_ref())
            .filter(|other| other.host_id == id);
        let earlier: Vec<&LocalSnapshot> = previous.into_iter().chain(others).collect();
        // Whether those lists tell a chat that is new to the host from an old one: a host lists
        // every chat it has, whether or not a read could afford its log. There are none before the
        // first read, and one cut to the row limit, or to the cache's budget on a save, may have
        // left any chat out.
        let lists_tell = reads > 0 && earlier.iter().all(|list| list.sessions.len() >= list.total);
        let mut listed = BTreeSet::new();
        let mut finished = BTreeSet::new();
        let mut busy = BTreeSet::new();
        for row in earlier.iter().flat_map(|s| &s.sessions) {
            let key = (row.kind.as_str(), row.id.as_str());
            listed.insert(key);
            if row.completion.is_some() {
                finished.insert(key);
            }
            if matches!(row.state.as_str(), "working" | "starting" | "waiting") {
                busy.insert(key);
            }
        }
        let mut seeded = contact.seeded;
        for row in &snapshot.sessions {
            let Some(token) = &row.completion else {
                continue;
            };
            let key = (row.kind.as_str(), row.id.as_str());
            let history = !finished.contains(&key)
                && match row.since {
                    Some(since) => since <= baseline,
                    None => {
                        reads < BASELINE_READS
                            && !busy.contains(&key)
                            && (!lists_tell || listed.contains(&key))
                    }
                };
            if !history {
                continue;
            }
            if seeded >= MAX_SEEDED {
                break;
            }
            if let std::collections::btree_map::Entry::Vacant(entry) =
                self.seen.entry(session_key(id, &row.kind, &row.id))
            {
                entry.insert(token.clone());
                seeded += 1;
            }
        }
        contact.seeded = seeded;
        self.bound_seen();
    }
    fn prune_baselines(&mut self) {
        let known: BTreeSet<_> = self
            .hosts
            .values()
            .filter_map(|host| host.snapshot.as_ref())
            .map(|snapshot| snapshot.host_id.clone())
            .collect();
        self.baselines.retain(|id, _| known.contains(id));
        let baselines = &self.baselines;
        self.first_contact
            .retain(|id, _| baselines.contains_key(id));
    }
    /// Rows kept while nobody watched; see `Host::age_unobserved`. Returns whether any changed.
    pub fn age_unobserved(&mut self, now: u64) -> bool {
        self.hosts
            .values_mut()
            .fold(false, |changed, host| host.age_unobserved(now) || changed)
    }
    /// One row per execution identity. The route is chosen by what it is rather than by which read
    /// finished last, so that a click keeps its target: this machine first, then a live route, then
    /// the lowest registry key. This machine's row shows what this machine read, since a route can
    /// claim any identity it knows. Otherwise the data comes from the newest observation of that
    /// host, on the host's own clock, so a slow route's older answer cannot move a session
    /// backwards; see `newest_observation` for the one exception.
    pub fn rows(&self) -> Vec<Host> {
        let mut identities: BTreeMap<String, Vec<&Host>> = BTreeMap::new();
        for host in self.hosts.values() {
            let key = if host.snapshot.is_some() {
                host.id.clone()
            } else {
                format!("pending:{}", host.id)
            };
            identities.entry(key).or_default().push(host);
        }
        let rank = |h: &Host| (h.connection_id.is_none(), h.connection == "online");
        identities
            .into_values()
            .map(|routes| {
                let mut chosen = routes[0];
                for route in &routes {
                    if rank(route) > rank(chosen) {
                        chosen = route;
                    }
                }
                let newest = if chosen.connection_id.is_none() {
                    chosen
                } else {
                    newest_observation(&routes, chosen)
                };
                let mut host = Host {
                    id: chosen.id.clone(),
                    connection_id: chosen.connection_id.clone(),
                    label: chosen.label.clone(),
                    route: chosen.route.clone(),
                    connection: chosen.connection.clone(),
                    freshness: newest.freshness.clone(),
                    received_at: newest.received_at,
                    error: chosen.error.clone(),
                    snapshot: newest.snapshot.clone(),
                    added_at: chosen.added_at,
                };
                if let Some(snapshot) = &mut host.snapshot {
                    for s in &mut snapshot.sessions {
                        s.unseen = s.completion.as_ref().is_some_and(|token| {
                            self.seen.get(&session_key(&host.id, &s.kind, &s.id)) != Some(token)
                        });
                    }
                }
                host
            })
            .collect()
    }
    pub fn seen(
        &mut self,
        host: &str,
        kind: &str,
        session: &str,
        token: &str,
    ) -> Result<(), String> {
        let rows = self.rows();
        let row = rows
            .iter()
            .find(|h| h.id == host)
            .and_then(|h| h.snapshot.as_ref())
            .and_then(|s| {
                s.sessions
                    .iter()
                    .find(|s| s.kind == kind && s.id == session)
            })
            .ok_or("Session no longer exists")?;
        if row.completion.as_deref() != Some(token) {
            return Err("Session changed; refresh before marking it seen".into());
        }
        self.seen
            .insert(session_key(host, kind, session), token.into());
        self.bound_seen();
        Ok(())
    }
    fn bound_seen(&mut self) {
        if self.seen.len() > MAX_SEEN {
            let live: BTreeSet<_> = self
                .hosts
                .values()
                .flat_map(|h| {
                    h.snapshot.iter().flat_map(move |s| {
                        s.sessions
                            .iter()
                            .map(move |s| session_key(&h.id, &s.kind, &s.id))
                    })
                })
                .collect();
            self.seen.retain(|key, _| live.contains(key));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot() -> LocalSnapshot {
        LocalSnapshot {
            version: 1,
            host_id: uuid::Uuid::new_v4().to_string(),
            observed_at: 10,
            capabilities: vec![],
            sessions: vec![Session {
                id: "same-id".into(),
                kind: "chat".into(),
                project_id: None,
                parent_id: None,
                title: "Chat".into(),
                provider: None,
                state: "working".into(),
                source: "provider".into(),
                since: None,
                subagents: None,
                requests: vec![],
                completion: None,
                unseen: false,
            }],
            total: 1,
            partial: false,
        }
    }
    #[test]
    fn disconnect_preserves_work_and_identity_change_is_refused() {
        let mut host = Host::pending(Some("route".into()), "Box".into(), "relay");
        let s = snapshot();
        host.apply(s.clone(), 20).unwrap();
        host.failed("offline".into(), false);
        assert_eq!(host.snapshot.as_ref().unwrap().sessions[0].state, "working");
        assert_eq!(host.freshness, "stale");
        assert_eq!(host.received_at, Some(20));
        assert!(host.apply(snapshot(), 30).is_err());
        assert_eq!(host.id, s.host_id);
    }
    #[test]
    fn acknowledgements_are_host_scoped_and_compare_completion() {
        let mut c = Cache::default();
        for key in ["a", "b"] {
            let mut s = snapshot();
            s.sessions[0].state = "done".into();
            s.sessions[0].completion = Some("turn:1".into());
            let mut h = Host::pending(Some(key.into()), key.into(), "relay");
            h.apply(s, 20).unwrap();
            c.hosts.insert(key.into(), h);
        }
        let id = c.hosts["a"].id.clone();
        assert!(
            c.rows()
                .iter()
                .all(|h| h.snapshot.as_ref().unwrap().sessions[0].unseen)
        );
        assert!(c.seen(&id, "chat", "same-id", "turn:2").is_err());
        c.seen(&id, "chat", "same-id", "turn:1").unwrap();
        assert!(
            !c.rows()
                .iter()
                .find(|h| h.connection_id.as_deref() == Some("a"))
                .unwrap()
                .snapshot
                .as_ref()
                .unwrap()
                .sessions[0]
                .unseen
        );
        assert!(
            c.rows()
                .iter()
                .find(|h| h.connection_id.as_deref() == Some("b"))
                .unwrap()
                .snapshot
                .as_ref()
                .unwrap()
                .sessions[0]
                .unseen
        );
    }
    #[test]
    fn corrupt_or_duplicate_remote_metadata_is_rejected() {
        let mut s = snapshot();
        s.sessions.push(s.sessions[0].clone());
        assert!(s.validate().is_err());
        s.sessions.pop();
        s.version = 2;
        assert!(s.validate().is_err());
    }
    #[test]
    fn multiple_routes_share_execution_identity_and_choose_a_live_route() {
        let s = snapshot();
        let mut relay = Host::pending(Some("relay".into()), "Mac".into(), "relay");
        relay.apply(s.clone(), 50).unwrap();
        relay.failed("offline".into(), false);
        let mut ssh = Host::pending(Some("ssh".into()), "Mac".into(), "ssh");
        ssh.apply(s, 30).unwrap();
        let mut cache = Cache::default();
        cache.hosts.insert("relay".into(), relay);
        cache.hosts.insert("ssh".into(), ssh);
        let rows = cache.rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].connection_id.as_deref(), Some("ssh"));
    }
    #[test]
    fn identity_is_stable_and_corruption_is_not_repaired_silently() {
        let home = std::env::temp_dir().join(format!("somtus-host-id-{}", uuid::Uuid::new_v4()));
        let a = identity(&home).unwrap();
        assert_eq!(a, identity(&home).unwrap());
        fs::write(home.join("host-identity"), b"").unwrap();
        assert!(identity(&home).is_err());
        let _ = fs::remove_dir_all(home);
    }
    #[test]
    fn cache_budget_preserves_every_host_with_explicit_partial_inventory() {
        let home = std::env::temp_dir().join(format!("somtus-host-cache-{}", uuid::Uuid::new_v4()));
        let mut cache = Cache::default();
        for n in 0..=MAX_HOSTS {
            let mut s = snapshot();
            let template = s.sessions[0].clone();
            s.sessions = (0..MAX_SESSIONS)
                .map(|i| Session {
                    id: format!("session-{i}"),
                    title: "x".repeat(2048),
                    ..template.clone()
                })
                .collect();
            s.total = MAX_SESSIONS;
            let key = format!("route-{n}");
            let mut host = Host::pending(Some(key.clone()), format!("Host {n}"), "relay");
            host.apply(s, 20).unwrap();
            cache.hosts.insert(key, host);
        }
        cache.save(&home).unwrap();
        assert!(
            fs::metadata(home.join("host-sessions.json")).unwrap().len() <= MAX_CACHE_BYTES as u64
        );
        assert!(
            cache
                .hosts
                .values()
                .all(|h| h.snapshot.as_ref().unwrap().sessions.len() == MAX_SESSIONS),
            "persistence must not mutate the live inventory"
        );
        let loaded = Cache::open(&home);
        assert_eq!(loaded.hosts.len(), MAX_HOSTS + 1);
        for host in loaded.hosts.values() {
            let s = host.snapshot.as_ref().unwrap();
            assert!(s.partial);
            assert_eq!(s.total, MAX_SESSIONS);
            assert!(!s.sessions.is_empty());
            assert!(s.sessions.len() < MAX_SESSIONS);
            assert_eq!(host.received_at, Some(20));
            assert_eq!(host.freshness, "stale");
            assert_eq!(s.sessions[0].state, "working");
        }
        fs::remove_dir_all(home).unwrap();
    }
    #[test]
    fn metadata_strings_and_cached_host_identity_are_validated() {
        let mut s = snapshot();
        s.capabilities = vec!["x".repeat(65)];
        assert!(s.validate().is_err());
        s.capabilities.clear();
        s.sessions[0].title = "bad\u{1b}[0m".into();
        assert!(s.validate().is_err());
        let mut host = Host::pending(Some("route".into()), "Box".into(), "relay");
        host.apply(snapshot(), 20).unwrap();
        assert!(host.valid_cached());
        host.id = uuid::Uuid::new_v4().to_string();
        assert!(!host.valid_cached());
    }
    /// One read of a host: rows as `(id, state, since, completion)` chats.
    fn read_of(
        host_id: &str,
        observed_at: u64,
        rows: &[(&str, &str, Option<u64>, Option<&str>)],
    ) -> LocalSnapshot {
        let template = snapshot().sessions.remove(0);
        LocalSnapshot {
            host_id: host_id.into(),
            observed_at,
            total: rows.len(),
            sessions: rows
                .iter()
                .map(|(id, state, since, completion)| Session {
                    id: (*id).into(),
                    state: (*state).into(),
                    since: *since,
                    completion: completion.map(str::to_owned),
                    ..template.clone()
                })
                .collect(),
            ..snapshot()
        }
    }
    fn cache_with(routes: &[&str]) -> Cache {
        let mut cache = Cache::default();
        for route in routes {
            let connection = (*route != "local").then(|| (*route).to_owned());
            cache.hosts.insert(
                (*route).into(),
                Host::pending(connection, format!("Route {route}"), "relay"),
            );
        }
        cache
    }
    fn unseen(cache: &Cache, session: &str) -> bool {
        cache
            .rows()
            .iter()
            .flat_map(|host| host.snapshot.iter().flat_map(|s| s.sessions.iter()))
            .find(|row| row.id == session)
            .unwrap_or_else(|| panic!("no row {session}"))
            .unseen
    }

    // C2: which route and which data a host's one row shows.
    #[test]
    fn two_live_routes_keep_one_target_and_show_the_newest_observation() {
        let id = uuid::Uuid::new_v4().to_string();
        let mut cache = cache_with(&["a", "b"]);
        let done = [("chat", "done", Some(95), Some("turn:2:completed"))];
        let working = [("chat", "working", None, None)];
        // The slow route's older observation arrives last, with the later receive time.
        cache
            .hosts
            .get_mut("a")
            .unwrap()
            .apply(read_of(&id, 100, &done), 100)
            .unwrap();
        cache
            .hosts
            .get_mut("b")
            .unwrap()
            .apply(read_of(&id, 90, &working), 105)
            .unwrap();
        let rows = cache.rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].connection_id.as_deref(), Some("a"));
        assert_eq!(rows[0].label, "Route a");
        let shown = rows[0].snapshot.as_ref().unwrap();
        assert_eq!(shown.observed_at, 100);
        assert_eq!(shown.sessions[0].state, "done");
        assert_eq!(rows[0].received_at, Some(100));
        // Acknowledging what is shown works although the other route read last.
        cache.seen(&id, "chat", "chat", "turn:2:completed").unwrap();
        assert!(!unseen(&cache, "chat"));
        // The route stays when the newer data comes from the other one, whose receipt comes along.
        cache
            .hosts
            .get_mut("b")
            .unwrap()
            .apply(read_of(&id, 120, &working), 121)
            .unwrap();
        cache
            .hosts
            .get_mut("a")
            .unwrap()
            .apply(read_of(&id, 110, &done), 125)
            .unwrap();
        let rows = cache.rows();
        assert_eq!(rows[0].connection_id.as_deref(), Some("a"));
        assert_eq!(rows[0].snapshot.as_ref().unwrap().observed_at, 120);
        assert_eq!(rows[0].received_at, Some(121));
        // A tie stays with the chosen route's own data.
        cache
            .hosts
            .get_mut("a")
            .unwrap()
            .apply(read_of(&id, 120, &done), 130)
            .unwrap();
        let rows = cache.rows();
        assert_eq!(rows[0].snapshot.as_ref().unwrap().sessions[0].state, "done");
        assert_eq!(rows[0].received_at, Some(130));
        // The freshness goes with the data: the newest observation is the one that went stale.
        cache
            .hosts
            .get_mut("b")
            .unwrap()
            .apply(read_of(&id, 140, &working), 141)
            .unwrap();
        cache
            .hosts
            .get_mut("b")
            .unwrap()
            .failed("offline".into(), false);
        let rows = cache.rows();
        assert_eq!(rows[0].connection_id.as_deref(), Some("a"));
        assert_eq!(rows[0].connection, "online");
        assert_eq!(rows[0].freshness, "stale");
        assert_eq!(rows[0].snapshot.as_ref().unwrap().observed_at, 140);
    }
    #[test]
    fn this_machine_keeps_its_row_against_a_route_back_to_itself() {
        let id = uuid::Uuid::new_v4().to_string();
        // Route keys are UUIDs, which sort before "local".
        let mut cache = cache_with(&["0-route", "local"]);
        let rows_of = |cache: &Cache| {
            let rows = cache.rows();
            assert_eq!(rows.len(), 1);
            rows.into_iter().next().unwrap()
        };
        cache
            .hosts
            .get_mut("local")
            .unwrap()
            .apply(read_of(&id, 10, &[]), 10)
            .unwrap();
        cache
            .hosts
            .get_mut("0-route")
            .unwrap()
            .apply(read_of(&id, 20, &[]), 20)
            .unwrap();
        let row = rows_of(&cache);
        assert_eq!(row.connection_id, None);
        assert_eq!(row.label, "Route local");
        // What this machine read, although the route's observation is newer.
        assert_eq!(row.snapshot.as_ref().unwrap().observed_at, 10);
        assert_eq!(row.received_at, Some(10));
        // Even while its own read fails: a click on it must still mean this machine.
        cache
            .hosts
            .get_mut("local")
            .unwrap()
            .report_failed("tmux timed out".into());
        let row = rows_of(&cache);
        assert_eq!(row.connection_id, None);
        assert_eq!(row.error.as_deref(), Some("tmux timed out"));
    }
    // #7: a route can claim any identity it knows, this machine's included.
    #[test]
    fn a_route_claiming_this_machines_identity_shows_none_of_its_sessions_here() {
        let id = uuid::Uuid::new_v4().to_string();
        let mut cache = cache_with(&["0-route", "local"]);
        cache
            .apply_read(
                "local",
                read_of(&id, 10, &[("mine", "waiting", None, None)]),
                10,
            )
            .unwrap();
        let theirs = [("theirs", "idle", Some(5), Some("turn:1:completed"))];
        cache
            .apply_read("0-route", read_of(&id, u64::MAX, &theirs), 20)
            .unwrap();
        let rows = cache.rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].connection_id, None);
        let shown = rows[0].snapshot.as_ref().unwrap();
        assert_eq!(shown.observed_at, 10);
        let ids: Vec<_> = shown.sessions.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(ids, ["mine"]);
        assert_eq!(rows[0].received_at, Some(10));
        assert_eq!(rows[0].freshness, "current");
        // Nothing of the route's can be acknowledged through this machine's row either, and its
        // answers take nothing from this machine's first contact.
        assert!(
            cache
                .seen(&id, "chat", "theirs", "turn:1:completed")
                .is_err()
        );
        assert!(!cache.seen.contains_key(&session_key(&id, "chat", "theirs")));
        assert_eq!(cache.first_contact[&id].reads, 1);
    }
    // #51: with one route down, its newest observation may be from a clock that has since been
    // set back.
    #[test]
    fn a_live_route_outranks_an_old_observation_left_on_a_route_that_went_down() {
        let id = uuid::Uuid::new_v4().to_string();
        let working = [("chat", "working", None, None)];
        let idle = [("chat", "idle", None, None)];
        let after = |b_received: u64| {
            let mut cache = cache_with(&["a", "b"]);
            cache
                .apply_read("a", read_of(&id, 5_000, &working), 100)
                .unwrap();
            cache
                .apply_read("b", read_of(&id, 5_000, &working), 100)
                .unwrap();
            cache
                .hosts
                .get_mut("a")
                .unwrap()
                .failed("offline".into(), false);
            // The host clock went back by 1000 s, and b reads on.
            cache
                .apply_read("b", read_of(&id, 4_000, &idle), b_received)
                .unwrap();
            let rows = cache.rows();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].connection_id.as_deref(), Some("b"));
            assert_eq!(rows[0].connection, "online");
            rows.into_iter().next().unwrap()
        };
        let row = after(200);
        assert_eq!(row.freshness, "current");
        assert_eq!(row.received_at, Some(200));
        assert_eq!(row.snapshot.as_ref().unwrap().sessions[0].state, "idle");
        // Within about a minute of the other receipt, the newest observation still wins.
        let row = after(150);
        assert_eq!(row.freshness, "stale");
        assert_eq!(row.received_at, Some(100));
        assert_eq!(row.snapshot.as_ref().unwrap().sessions[0].state, "working");
    }

    // C3: a read's identity.
    #[test]
    fn this_machines_new_identity_replaces_its_snapshot_but_a_route_stays_pinned() {
        let (first, second) = (snapshot(), snapshot());
        let mut cache = cache_with(&["local", "route"]);
        cache.apply_read("local", first.clone(), 10).unwrap();
        cache.apply_read("local", second.clone(), 20).unwrap();
        assert_eq!(cache.hosts["local"].id, second.host_id);
        assert_eq!(cache.hosts["local"].connection, "online");
        cache.apply_read("route", first.clone(), 10).unwrap();
        assert!(cache.apply_read("route", second.clone(), 20).is_err());
        assert_eq!(cache.hosts["route"].id, first.host_id);
        // A local read this build cannot accept takes nothing away.
        let mut wrong = snapshot();
        wrong.version = 2;
        assert!(cache.apply_read("local", wrong, 30).is_err());
        assert_eq!(cache.hosts["local"].id, second.host_id);
        assert!(cache.hosts["local"].snapshot.is_some());
        assert_eq!(cache.hosts["local"].received_at, Some(20));
    }
    #[test]
    fn a_route_record_from_an_old_cache_loads_without_its_added_time() {
        let mut host = Host::pending(Some("route".into()), "Box".into(), "relay");
        host.apply(snapshot(), 20).unwrap();
        let mut value = serde_json::to_value(&host).unwrap();
        value.as_object_mut().unwrap().remove("addedAt").unwrap();
        let loaded: Host = serde_json::from_value(value).unwrap();
        assert_eq!(loaded.added_at, None);
        assert!(loaded.valid_cached());
        let cache: Cache = serde_json::from_str(r#"{"hosts":{},"seen":{}}"#).unwrap();
        assert!(cache.baselines.is_empty());
    }

    // C1, C4: what a failed or unanswered read leaves on the row.
    #[test]
    fn a_host_that_could_not_report_keeps_the_link_online() {
        let mut host = Host::pending(Some("route".into()), "Box".into(), "relay");
        host.report_failed("tmux timed out".into());
        assert_eq!(
            (host.connection.as_str(), host.freshness.as_str()),
            ("online", "unknown")
        );
        assert_eq!(host.error.as_deref(), Some("tmux timed out"));
        host.apply(snapshot(), 20).unwrap();
        let before = host.snapshot.clone();
        host.report_failed("x".repeat(400));
        assert_eq!(
            (host.connection.as_str(), host.freshness.as_str()),
            ("online", "stale")
        );
        assert_eq!(host.error.as_ref().unwrap().chars().count(), 300);
        assert_eq!(host.snapshot, before);
        assert_eq!(host.received_at, Some(20));
        assert!(host.valid_cached());
    }
    #[test]
    fn a_link_still_being_made_is_not_a_failure_and_does_not_lift_a_block() {
        let mut host = Host::pending(Some("route".into()), "Box".into(), "relay");
        host.failed("relay closed".into(), false);
        host.connecting();
        assert_eq!(
            (host.connection.as_str(), host.freshness.as_str()),
            ("connecting", "unknown")
        );
        assert_eq!(host.error, None);
        host.apply(snapshot(), 20).unwrap();
        host.connecting();
        assert_eq!(
            (host.connection.as_str(), host.freshness.as_str()),
            ("connecting", "stale")
        );
        assert_eq!(host.received_at, Some(20));
        assert!(host.snapshot.is_some());
        host.failed("This host needs a Somtus update".into(), true);
        host.connecting();
        assert_eq!(host.connection, "blocked");
        assert_eq!(
            host.error.as_deref(),
            Some("This host needs a Somtus update")
        );
    }

    // C5: rows kept while nobody watched.
    #[test]
    fn rows_aged_for_a_returning_observer_keep_their_data() {
        let mut cache = cache_with(&["old", "recent", "blocked", "pending"]);
        for route in ["old", "recent", "blocked"] {
            let read = read_of(
                &uuid::Uuid::new_v4().to_string(),
                50,
                &[("chat", "working", None, None)],
            );
            cache.apply_read(route, read, 100).unwrap();
        }
        cache.hosts.get_mut("recent").unwrap().received_at = Some(141);
        cache
            .hosts
            .get_mut("blocked")
            .unwrap()
            .failed("Needs an update".into(), true);
        let before = cache.hosts["old"].snapshot.clone();
        assert!(cache.age_unobserved(171));
        let host = &cache.hosts["old"];
        assert_eq!(
            (host.connection.as_str(), host.freshness.as_str()),
            ("connecting", "stale")
        );
        assert_eq!(host.received_at, Some(100));
        assert_eq!(host.snapshot, before);
        assert_eq!(host.snapshot.as_ref().unwrap().sessions[0].state, "working");
        assert!(host.valid_cached());
        // Thirty seconds is still fresh.
        let host = &cache.hosts["recent"];
        assert_eq!(
            (host.connection.as_str(), host.freshness.as_str()),
            ("online", "current")
        );
        assert_eq!(cache.hosts["blocked"].connection, "blocked");
        assert_eq!(cache.hosts["pending"].freshness, "unknown");
        // Nothing left to change.
        assert!(!cache.age_unobserved(171));
    }
    // #22: only a row that says the link is up can say too much.
    #[test]
    fn aging_leaves_a_row_that_already_says_the_link_is_down() {
        let mut cache = cache_with(&["down"]);
        let read = read_of(
            &uuid::Uuid::new_v4().to_string(),
            50,
            &[("chat", "working", None, None)],
        );
        cache.apply_read("down", read, 100).unwrap();
        cache
            .hosts
            .get_mut("down")
            .unwrap()
            .failed("relay closed".into(), false);
        let before = cache.hosts["down"].clone();
        assert!(!cache.age_unobserved(100_000));
        assert_eq!(cache.hosts["down"], before);
        assert_eq!(cache.hosts["down"].connection, "backoff");
        assert_eq!(cache.hosts["down"].error.as_deref(), Some("relay closed"));
    }

    // C6: what fits when not everything does.
    fn terminal(id: &str, state: &str, since: Option<u64>) -> Session {
        Session {
            id: id.into(),
            kind: "terminal".into(),
            state: state.into(),
            since,
            ..snapshot().sessions.remove(0)
        }
    }
    #[test]
    fn rows_that_need_a_person_survive_the_row_limit() {
        // "chat" sorts before "terminal", so these terminals used to be the first to go.
        let mut rows: Vec<Session> = (0..600)
            .map(|n| Session {
                id: format!("chat-{n:04}"),
                state: "stopped".into(),
                ..snapshot().sessions.remove(0)
            })
            .collect();
        let mut asking = terminal("t-asking", "idle", None);
        asking.requests = vec!["permission".into()];
        let mut finished = terminal("t-finished", "done", Some(9));
        finished.completion = Some("done:9".into());
        rows.extend([
            terminal("t-waiting", "waiting", Some(1)),
            terminal("t-blocked", "blocked", None),
            terminal("t-error", "error", None),
            terminal("t-working", "working", None),
            terminal("t-starting", "starting", None),
            asking,
            finished,
            terminal("t-idle-new", "idle", Some(500)),
        ]);
        let mut kept = rows.clone();
        keep_most_relevant(&mut kept, 512);
        assert_eq!(kept.len(), 512);
        for id in [
            "t-asking",
            "t-waiting",
            "t-blocked",
            "t-error",
            "t-working",
            "t-starting",
            "t-finished",
            "t-idle-new",
        ] {
            assert!(kept.iter().any(|row| row.id == id), "{id} was cut");
        }
        // Back in the wire's order, and with the same members whatever order they arrived in.
        let order: Vec<_> = kept
            .iter()
            .map(|r| (r.kind.clone(), r.id.clone()))
            .collect();
        let mut sorted = order.clone();
        sorted.sort();
        assert_eq!(order, sorted);
        rows.reverse();
        keep_most_relevant(&mut rows, 512);
        assert_eq!(rows, kept);

        // Attention beats work, work beats a finished row, and a finished row beats the rest.
        let mut classes = vec![
            terminal("plain-newest", "idle", Some(900)),
            terminal("finished", "done", Some(1)),
            terminal("working", "working", Some(2)),
            terminal("waiting", "waiting", Some(3)),
        ];
        classes[1].completion = Some("done:1".into());
        for (limit, expected) in [
            (1, vec!["waiting"]),
            (2, vec!["waiting", "working"]),
            (3, vec!["finished", "waiting", "working"]),
        ] {
            let mut rows = classes.clone();
            keep_most_relevant(&mut rows, limit);
            let ids: Vec<_> = rows.iter().map(|row| row.id.as_str()).collect();
            assert_eq!(ids, expected, "limit {limit}");
        }
        // Newest first within a class, an unknown time last, and then a stable order.
        let mut rows = vec![
            terminal("b-none", "idle", None),
            terminal("a-none", "idle", None),
            terminal("old", "idle", Some(1)),
            terminal("new", "idle", Some(2)),
        ];
        keep_most_relevant(&mut rows, 3);
        let ids: Vec<_> = rows.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(ids, vec!["a-none", "new", "old"]);
    }
    #[test]
    fn persistence_trims_the_rows_that_matter_least() {
        let home = std::env::temp_dir().join(format!("somtus-host-cache-{}", uuid::Uuid::new_v4()));
        let mut cache = Cache::default();
        for n in 0..=MAX_HOSTS {
            let mut s = snapshot();
            let template = s.sessions[0].clone();
            s.sessions = (0..MAX_SESSIONS - 1)
                .map(|i| Session {
                    id: format!("session-{i:03}"),
                    title: "x".repeat(2048),
                    state: "stopped".into(),
                    ..template.clone()
                })
                .collect();
            s.sessions.push(Session {
                title: "x".repeat(2048),
                ..terminal("waiting", "waiting", None)
            });
            s.total = MAX_SESSIONS;
            let key = format!("route-{n}");
            let mut host = Host::pending(Some(key.clone()), format!("Host {n}"), "relay");
            host.apply(s, 20).unwrap();
            cache.hosts.insert(key, host);
        }
        let trimmed = session_key(&cache.hosts["route-0"].id, "chat", "session-500");
        cache.seen.insert(trimmed.clone(), "t:1".into());
        cache.save(&home).unwrap();
        let restored = Cache::open(&home);
        for host in restored.hosts.values() {
            let s = host.snapshot.as_ref().unwrap();
            assert!(s.partial);
            assert!(
                s.sessions.iter().any(|row| row.id == "waiting"),
                "cut by the trim"
            );
            assert!(s.sessions.iter().all(|row| row.id != "session-500"));
            let order: Vec<_> = s.sessions.iter().map(|r| (&r.kind, &r.id)).collect();
            let mut sorted = order.clone();
            sorted.sort();
            assert_eq!(order, sorted);
        }
        // The first save after a restart has only the rows that fitted, which cannot show that the
        // trimmed one is gone.
        restored.save(&home).unwrap();
        assert!(Cache::open(&home).seen.contains_key(&trimmed));
        fs::remove_dir_all(home).unwrap();
    }

    // C7: history on first contact.
    #[test]
    fn first_contact_treats_history_as_seen_and_later_completions_as_news() {
        let id = uuid::Uuid::new_v4().to_string();
        let mut cache = cache_with(&["a"]);
        let first = read_of(
            &id,
            1000,
            &[
                ("old", "idle", Some(900), Some("turn:1:completed")),
                ("at-contact", "error", Some(1000), Some("turn:2:failed")),
                ("no-time", "stopped", None, Some("turn:3:completed")),
                ("running", "working", None, None),
                ("just-after", "idle", Some(1001), Some("turn:4:completed")),
            ],
        );
        cache.apply_read("a", first, 2000).unwrap();
        assert_eq!(cache.baselines.get(&id), Some(&1000));
        for row in ["old", "at-contact", "no-time", "running"] {
            assert!(!unseen(&cache, row), "{row}");
        }
        assert!(unseen(&cache, "just-after"), "finished after first contact");

        // Coverage of old logs grows over the next reads; a new completion is still news.
        let later = read_of(
            &id,
            1060,
            &[
                ("old", "idle", Some(900), Some("turn:1:completed")),
                ("read-late", "idle", Some(800), Some("turn:9:completed")),
                ("running", "idle", Some(1050), Some("turn:5:completed")),
                ("just-after", "idle", Some(1001), Some("turn:4:completed")),
            ],
        );
        cache.apply_read("a", later, 2060).unwrap();
        assert!(!unseen(&cache, "read-late"));
        assert!(unseen(&cache, "running"));
        assert!(unseen(&cache, "just-after"));
        assert_eq!(cache.baselines.get(&id), Some(&1000));

        // A token that replaces one already recorded is new, whatever it says about time.
        let replaced = read_of(
            &id,
            1100,
            &[("old", "stopped", None, Some("turn:7:completed"))],
        );
        cache.apply_read("a", replaced, 2100).unwrap();
        assert!(unseen(&cache, "old"));

        // A time from before first contact makes a completion history however late it turns up.
        // Without one, a completion can only be told apart during the first reads, even on a chat
        // listed all along.
        for n in 3..BASELINE_READS {
            let listed = [("no-time-late", "idle", None, None)];
            cache
                .apply_read("a", read_of(&id, 1100 + u64::from(n), &listed), 2100)
                .unwrap();
        }
        let late = read_of(
            &id,
            9000,
            &[
                ("past", "idle", Some(700), Some("turn:1:completed")),
                ("no-time-late", "stopped", None, Some("turn:2:completed")),
            ],
        );
        cache.apply_read("a", late, 10_000).unwrap();
        assert!(!unseen(&cache, "past"));
        assert!(unseen(&cache, "no-time-late"));
    }
    // #6: reads stop while nobody watches, so old logs can be covered long after first contact.
    #[test]
    fn history_covered_after_a_long_pause_is_still_history() {
        let id = uuid::Uuid::new_v4().to_string();
        let mut cache = cache_with(&["a"]);
        let covered = ("covered", "idle", Some(900), Some("turn:1:completed"));
        // A host lists every chat it has, whether or not the read could afford its log.
        let first = [
            covered,
            ("late-idle", "idle", None, None),
            ("late-stopped", "stopped", None, None),
        ];
        cache
            .apply_read("a", read_of(&id, 1000, &first), 1000)
            .unwrap();
        // Ten minutes hidden, and then the next read covers more of the old logs.
        let later = [
            covered,
            ("late-idle", "idle", Some(800), Some("turn:2:completed")),
            ("late-stopped", "stopped", None, Some("turn:3:completed")),
            ("news", "idle", Some(1500), Some("turn:4:completed")),
        ];
        cache
            .apply_read("a", read_of(&id, 1600, &later), 1600)
            .unwrap();
        assert!(!unseen(&cache, "covered"));
        assert!(!unseen(&cache, "late-idle"));
        assert!(!unseen(&cache, "late-stopped"));
        assert!(unseen(&cache, "news"));
    }
    // F4: a chat made while nobody watched can finish with no time to show for it, and then turn
    // up within the first reads.
    #[test]
    fn a_chat_that_first_turns_up_after_a_pause_is_news() {
        let id = uuid::Uuid::new_v4().to_string();
        let mut cache = cache_with(&["a", "b"]);
        let old = ("old", "stopped", None, Some("turn:1:completed"));
        let uncovered = ("uncovered", "stopped", None, None);
        // About a minute of watching, then Agents is closed and the reads stop.
        for n in 0..10 {
            let at = 1000 + 5 * n;
            cache
                .apply_read("a", read_of(&id, at, &[old, uncovered]), at)
                .unwrap();
        }
        // The next day the old log is covered at last, and a chat made meanwhile has finished.
        let next_day = [
            old,
            ("uncovered", "stopped", None, Some("turn:2:completed")),
            ("meanwhile", "stopped", None, Some("turn:1:completed")),
            ("slow-log", "stopped", None, None),
        ];
        cache
            .apply_read("a", read_of(&id, 90_000, &next_day), 90_000)
            .unwrap();
        assert!(!unseen(&cache, "old"));
        assert!(!unseen(&cache, "uncovered"));
        assert!(unseen(&cache, "meanwhile"));
        // A second route's first read: a chat the first route lists is not new to this host.
        let second = [
            ("slow-log", "stopped", None, Some("turn:3:completed")),
            ("unlisted", "stopped", None, Some("turn:1:completed")),
        ];
        cache
            .apply_read("b", read_of(&id, 90_010, &second), 90_010)
            .unwrap();
        assert!(!unseen(&cache, "slow-log"));
        assert!(unseen(&cache, "unlisted"));
    }
    // A list cut to the row limit, or to the cache's budget on a save, may have left any chat out.
    #[test]
    fn a_chat_that_a_cut_list_may_have_left_out_can_still_be_history() {
        let id = uuid::Uuid::new_v4().to_string();
        let mut cache = cache_with(&["a"]);
        let mut cut = read_of(&id, 1000, &[("kept", "stopped", None, None)]);
        cut.partial = true;
        cut.total = 2;
        cache.apply_read("a", cut, 1000).unwrap();
        let whole = [
            ("kept", "stopped", None, None),
            ("left-out", "stopped", None, Some("turn:1:completed")),
        ];
        cache
            .apply_read("a", read_of(&id, 1005, &whole), 1005)
            .unwrap();
        assert!(!unseen(&cache, "left-out"));
        // A list that holds every row does tell a new chat from an old one.
        let more = [
            ("kept", "stopped", None, None),
            ("left-out", "stopped", None, Some("turn:1:completed")),
            ("new", "stopped", None, Some("turn:1:completed")),
        ];
        cache
            .apply_read("a", read_of(&id, 1010, &more), 1010)
            .unwrap();
        assert!(unseen(&cache, "new"));
    }
    // F5: the logs of running chats are read again on every read, and can leave room for only a
    // few old ones each time.
    #[test]
    fn old_logs_covered_a_few_at_a_time_are_all_history() {
        let id = uuid::Uuid::new_v4().to_string();
        let mut cache = cache_with(&["a"]);
        let saved: Vec<String> = (0..125).map(|n| format!("saved-{n:03}")).collect();
        let mut reads = 0u64;
        loop {
            let covered = saved.len().min(3 * (reads as usize + 1));
            let mut rows: Vec<(&str, &str, Option<u64>, Option<&str>)> =
                ["run-0", "run-1", "run-2"]
                    .iter()
                    .map(|chat| (*chat, "working", None, None))
                    .collect();
            rows.extend(saved.iter().enumerate().map(|(n, chat)| {
                let token = (n < covered).then_some("turn:1:completed");
                (chat.as_str(), "stopped", None, token)
            }));
            let at = 1000 + 5 * reads;
            cache.apply_read("a", read_of(&id, at, &rows), at).unwrap();
            reads += 1;
            if covered == saved.len() {
                break;
            }
        }
        // More reads than the window used to have.
        assert!(reads > 30, "{reads}");
        let news: Vec<_> = saved.iter().filter(|chat| unseen(&cache, chat)).collect();
        assert!(news.is_empty(), "{} of them: {news:?}", news.len());
    }
    // #21: a host clock can be set back past the baseline.
    #[test]
    fn a_host_clock_behind_its_baseline_seeds_nothing() {
        let id = uuid::Uuid::new_v4().to_string();
        let mut cache = cache_with(&["a"]);
        cache
            .apply_read("a", read_of(&id, 2_000_000, &[]), 1000)
            .unwrap();
        // Ten days back: which side of the baseline a time falls on says nothing now.
        let behind = 2_000_000 - 10 * 86_400;
        let finished = [
            ("new", "idle", Some(behind - 5), Some("turn:1:completed")),
            ("stopped", "stopped", None, Some("turn:2:completed")),
        ];
        cache
            .apply_read("a", read_of(&id, behind, &finished), 2000)
            .unwrap();
        assert!(unseen(&cache, "new"));
        assert!(unseen(&cache, "stopped"));
        // Once the clock is past the baseline again, what was already on show is still news.
        cache
            .apply_read("a", read_of(&id, 2_000_010, &finished), 3000)
            .unwrap();
        assert!(unseen(&cache, "new"));
        assert!(unseen(&cache, "stopped"));
    }
    // #20: a chat that stops loses the time of its last turn.
    #[test]
    fn a_completion_on_show_is_not_swallowed_when_its_chat_stops() {
        let id = uuid::Uuid::new_v4().to_string();
        let stopped = [
            ("finished", "stopped", None, Some("turn:5:completed")),
            ("cut-off", "stopped", None, Some("turn:6:completed")),
        ];
        // The read that sees them stopped may come through the route that read them before, or
        // be another route's first read of this host.
        for route in ["a", "b"] {
            let mut cache = cache_with(&["a", "b"]);
            let working = [
                ("finished", "working", None, None),
                ("cut-off", "working", None, None),
            ];
            cache
                .apply_read("a", read_of(&id, 1000, &working), 1000)
                .unwrap();
            let one_done = [
                ("finished", "idle", Some(1025), Some("turn:5:completed")),
                ("cut-off", "working", None, None),
            ];
            cache
                .apply_read("a", read_of(&id, 1030, &one_done), 1030)
                .unwrap();
            assert!(unseen(&cache, "finished"));
            // Both stop: one finished on show, the other between two reads.
            cache
                .apply_read(route, read_of(&id, 1060, &stopped), 1060)
                .unwrap();
            assert!(unseen(&cache, "finished"), "{route}");
            assert!(unseen(&cache, "cut-off"), "{route}");
        }
    }
    // #45: a host decides its own clock and what it lists.
    #[test]
    fn first_contact_with_one_host_cannot_crowd_out_anothers_acknowledgements() {
        let (hostile, other) = (
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
        );
        let mut cache = cache_with(&["hostile", "partial"]);
        let mut read = read_of(&other, 1000, &[("on-list", "idle", Some(1), Some("t:1"))]);
        read.partial = true;
        read.total = 2;
        cache.apply_read("partial", read, 1000).unwrap();
        // Acknowledged while it was on the list; an incomplete list cannot show it is gone.
        let kept = session_key(&other, "chat", "off-list");
        cache.seen.insert(kept.clone(), "t:1".into());
        // A clock that never leaves the baseline, and new finished chats on every read: first
        // with no time, then with one from before first contact.
        for n in 0..140u32 {
            let since = (n >= 70).then_some(0);
            let ids: Vec<_> = (0..MAX_SESSIONS).map(|i| format!("{n}-{i}")).collect();
            let rows: Vec<_> = ids
                .iter()
                .map(|id| (id.as_str(), "stopped", since, Some("turn:1:completed")))
                .collect();
            cache
                .apply_read("hostile", read_of(&hostile, 1000, &rows), 1000)
                .unwrap();
        }
        let seeded = cache
            .seen
            .keys()
            .filter(|key| key_host(key).as_deref() == Some(hostile.as_str()))
            .count();
        assert!(seeded <= MAX_SEEDED as usize, "{seeded}");
        assert!(cache.seen.contains_key(&kept));
    }
    #[test]
    fn a_failed_read_opens_no_baseline() {
        let (pinned, other) = (snapshot(), snapshot());
        let mut cache = cache_with(&["a"]);
        cache.apply_read("a", pinned.clone(), 20).unwrap();
        let mut refused = other.clone();
        refused.sessions[0].completion = Some("turn:1:completed".into());
        assert!(cache.apply_read("a", refused, 30).is_err());
        assert!(!cache.baselines.contains_key(&other.host_id));
        let mut invalid = snapshot();
        invalid.version = 2;
        assert!(cache.apply_read("a", invalid.clone(), 30).is_err());
        assert!(!cache.baselines.contains_key(&invalid.host_id));
        assert_eq!(cache.seen.len(), 0);
    }
    #[test]
    fn a_host_baseline_is_per_identity_and_outlives_a_restart() {
        let home = std::env::temp_dir().join(format!("somtus-host-cache-{}", uuid::Uuid::new_v4()));
        let id = uuid::Uuid::new_v4().to_string();
        let mut cache = cache_with(&["a", "b"]);
        let old = [("old", "idle", Some(900), Some("turn:1:completed"))];
        cache
            .apply_read("a", read_of(&id, 1000, &old), 2000)
            .unwrap();
        // A second route to a host already met, after the window: history stays as it was, and
        // what happened meanwhile on the host is news.
        let more = [
            ("old", "idle", Some(900), Some("turn:1:completed")),
            ("meanwhile", "idle", Some(1500), Some("turn:2:completed")),
        ];
        cache
            .apply_read("b", read_of(&id, 2000, &more), 3000)
            .unwrap();
        assert_eq!(cache.baselines.get(&id), Some(&1000));
        assert!(!unseen(&cache, "old"));
        assert!(unseen(&cache, "meanwhile"));

        cache.save(&home).unwrap();
        let mut restored = Cache::open(&home);
        assert_eq!(restored.baselines.get(&id), Some(&1000));
        assert!(!unseen(&restored, "old"));
        assert!(unseen(&restored, "meanwhile"));
        // After a restart the window has not reopened.
        restored
            .apply_read("a", read_of(&id, 3000, &more), 4000)
            .unwrap();
        assert!(unseen(&restored, "meanwhile"));
        // Nor does a restart give the first reads back: they are counted on disk.
        let listed = [more[0], more[1], ("stopped", "stopped", None, None)];
        for _ in 3..BASELINE_READS {
            restored
                .apply_read("a", read_of(&id, 3000, &listed), 4000)
                .unwrap();
        }
        restored.save(&home).unwrap();
        let mut again = Cache::open(&home);
        let stopped = [("stopped", "stopped", None, Some("turn:3:completed"))];
        again
            .apply_read("a", read_of(&id, 3100, &stopped), 4100)
            .unwrap();
        assert!(unseen(&again, "stopped"));
        fs::remove_dir_all(home).unwrap();
    }
    #[test]
    fn a_cache_from_before_baselines_keeps_what_was_unseen() {
        let id = uuid::Uuid::new_v4().to_string();
        let mut cache = cache_with(&["a"]);
        let rows = [("finished", "idle", Some(900), Some("turn:1:completed"))];
        cache
            .hosts
            .get_mut("a")
            .unwrap()
            .apply(read_of(&id, 1000, &rows), 1000)
            .unwrap();
        assert!(cache.baselines.is_empty());
        // Nothing here is first contact, so nothing is swallowed by the upgrade.
        cache
            .apply_read("a", read_of(&id, 1005, &rows), 1005)
            .unwrap();
        assert!(unseen(&cache, "finished"));
        let mut other_route = cache_with(&["a", "b"]);
        other_route
            .hosts
            .insert("a".into(), cache.hosts["a"].clone());
        other_route
            .apply_read("b", read_of(&id, 1010, &rows), 1010)
            .unwrap();
        assert!(unseen(&other_route, "finished"));
    }
    #[test]
    fn baselines_stay_bounded_by_known_hosts() {
        let home = std::env::temp_dir().join(format!("somtus-host-cache-{}", uuid::Uuid::new_v4()));
        let (gone, kept) = (
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
        );
        let mut cache = cache_with(&["a", "b"]);
        cache
            .apply_read("a", read_of(&gone, 1000, &[]), 1000)
            .unwrap();
        cache
            .apply_read("b", read_of(&kept, 1000, &[]), 1000)
            .unwrap();
        cache.hosts.remove("a");
        // Meeting another host drops what the map no longer needs, without waiting for a save.
        cache.hosts.insert(
            "c".into(),
            Host::pending(Some("c".into()), "C".into(), "relay"),
        );
        let third = uuid::Uuid::new_v4().to_string();
        cache
            .apply_read("c", read_of(&third, 1000, &[]), 1000)
            .unwrap();
        assert!(!cache.baselines.contains_key(&gone));
        assert_eq!(cache.baselines.len(), 2);
        // A file never records one for a host it no longer holds, and a loaded one is checked too.
        cache.hosts.remove("c");
        cache.save(&home).unwrap();
        let text = fs::read_to_string(home.join("host-sessions.json")).unwrap();
        assert!(!text.contains(&third));
        let mut value: serde_json::Value = serde_json::from_str(&text).unwrap();
        value["baselines"][gone.as_str()] = 5.into();
        value["first_contact"][gone.as_str()] = serde_json::json!({"reads": 1, "seeded": 1});
        fs::write(home.join("host-sessions.json"), value.to_string()).unwrap();
        let loaded = Cache::open(&home);
        assert_eq!(
            loaded.first_contact,
            BTreeMap::from([(
                kept.clone(),
                FirstContact {
                    reads: 1,
                    seeded: 0
                }
            )])
        );
        assert_eq!(loaded.baselines, BTreeMap::from([(kept, 1000)]));
        fs::remove_dir_all(home).unwrap();
    }
    #[test]
    fn seeding_respects_the_seen_limit() {
        let id = uuid::Uuid::new_v4().to_string();
        let mut cache = cache_with(&["a"]);
        for n in 0..MAX_SEEN {
            cache.seen.insert(
                session_key("gone", "chat", &format!("{n}")),
                "turn:1".into(),
            );
        }
        let rows = [("old", "idle", Some(900), Some("turn:1:completed"))];
        cache
            .apply_read("a", read_of(&id, 1000, &rows), 1000)
            .unwrap();
        assert!(cache.seen.len() <= MAX_SEEN);
        assert!(!unseen(&cache, "old"));
    }

    // C8: an inventory that may be incomplete cannot prove a session is gone.
    #[test]
    fn acknowledgements_of_a_partial_inventory_survive_a_save() {
        let home = std::env::temp_dir().join(format!("somtus-host-cache-{}", uuid::Uuid::new_v4()));
        let (partial, complete) = (
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
        );
        let mut cache = cache_with(&["partial", "complete"]);
        let mut read = read_of(&partial, 1000, &[("on-list", "idle", Some(1), Some("t:1"))]);
        read.partial = true;
        read.total = 2;
        cache
            .hosts
            .get_mut("partial")
            .unwrap()
            .apply(read, 1000)
            .unwrap();
        let read = read_of(
            &complete,
            1000,
            &[("on-list", "idle", Some(1), Some("t:1"))],
        );
        cache
            .hosts
            .get_mut("complete")
            .unwrap()
            .apply(read, 1000)
            .unwrap();
        for host in [&partial, &complete] {
            for row in ["on-list", "off-list"] {
                cache
                    .seen
                    .insert(session_key(host, "chat", row), "t:1".into());
            }
        }
        cache
            .seen
            .insert(session_key("removed", "chat", "row"), "t:1".into());
        cache.save(&home).unwrap();
        // Saving is not allowed to change the live cache.
        assert_eq!(cache.seen.len(), 5);

        let mut loaded = Cache::open(&home);
        assert!(
            loaded
                .seen
                .contains_key(&session_key(&partial, "chat", "on-list"))
        );
        assert!(
            loaded
                .seen
                .contains_key(&session_key(&partial, "chat", "off-list"))
        );
        assert!(
            loaded
                .seen
                .contains_key(&session_key(&complete, "chat", "on-list"))
        );
        // A complete inventory, or none at all, still proves those sessions are gone.
        assert!(
            !loaded
                .seen
                .contains_key(&session_key(&complete, "chat", "off-list"))
        );
        assert!(
            !loaded
                .seen
                .contains_key(&session_key("removed", "chat", "row"))
        );
        // The session comes back onto the list after the restart, still acknowledged.
        let back = read_of(
            &partial,
            1100,
            &[("off-list", "idle", Some(1), Some("t:1"))],
        );
        loaded
            .hosts
            .get_mut("partial")
            .unwrap()
            .apply(back, 1100)
            .unwrap();
        assert!(!unseen(&loaded, "off-list"));
        fs::remove_dir_all(home).unwrap();
    }
    #[test]
    fn a_full_seen_budget_goes_to_rows_on_the_list_first() {
        let home = std::env::temp_dir().join(format!("somtus-host-cache-{}", uuid::Uuid::new_v4()));
        let id = uuid::Uuid::new_v4().to_string();
        let mut cache = cache_with(&["a"]);
        // Sorts after every key below, so it would be the first to lose out in key order.
        let rows = [("zz-live", "idle", Some(1), Some("t:1"))];
        let mut read = read_of(&id, 1000, &rows);
        read.partial = true;
        read.total = 2;
        cache.hosts.get_mut("a").unwrap().apply(read, 1000).unwrap();
        for n in 0..20_000 {
            cache.seen.insert(
                session_key(&id, "chat", &format!("off-{n:05}")),
                "t:1".into(),
            );
        }
        cache
            .seen
            .insert(session_key(&id, "chat", "zz-live"), "t:1".into());
        cache.save(&home).unwrap();
        let loaded = Cache::open(&home);
        assert!(
            loaded
                .seen
                .contains_key(&session_key(&id, "chat", "zz-live"))
        );
        assert!(
            loaded.seen.len() > 1,
            "off-list acknowledgements keep what is left"
        );
        assert!(loaded.seen.len() < 20_000, "the byte budget still holds");
        fs::remove_dir_all(home).unwrap();
    }

    // C9: newer hosts may use words this build does not know.
    #[test]
    fn newer_words_degrade_per_row_and_malformed_snapshots_stay_rejected() {
        let mut s = snapshot();
        s.sessions[0].state = "pondering".into();
        let template = s.sessions[0].clone();
        s.sessions.push(Session {
            id: "workflow-1".into(),
            kind: "workflow".into(),
            ..template.clone()
        });
        s.sessions.push(Session {
            id: "plain".into(),
            state: "idle".into(),
            ..template.clone()
        });
        s.total = 3;
        let mut host = Host::pending(Some("route".into()), "Box".into(), "relay");
        host.apply(s.clone(), 20).unwrap();
        let kept = host.snapshot.as_ref().unwrap();
        assert_eq!(kept.sessions.len(), 2);
        assert_eq!(kept.sessions[0].state, "unknown");
        assert_eq!(kept.sessions[1].state, "idle");
        assert_eq!(kept.total, 3);
        assert!(kept.partial);
        assert!(host.valid_cached());

        // Known words alone change nothing.
        let mut known = snapshot();
        known.sessions[0].state = "idle".into();
        let mut host = Host::pending(Some("route".into()), "Box".into(), "relay");
        host.apply(known.clone(), 20).unwrap();
        assert_eq!(host.snapshot.as_ref(), Some(&known));

        // Everything else about a snapshot from another machine stays strict.
        let rejected = |change: &dyn Fn(&mut LocalSnapshot)| {
            let mut bad = s.clone();
            change(&mut bad);
            let mut host = Host::pending(Some("route".into()), "Box".into(), "relay");
            host.apply(bad, 20).is_err() && host.snapshot.is_none()
        };
        assert!(rejected(&|s| s.version = 2));
        assert!(rejected(&|s| s.host_id = "not-a-uuid".into()));
        assert!(rejected(&|s| s.total = 2), "fewer in total than rows sent");
        assert!(
            rejected(&|s| s.total = 4),
            "more in total than rows, without saying so"
        );
        assert!(
            rejected(&|s| {
                let extra = s.sessions[1].clone();
                s.sessions.extend((0..MAX_SESSIONS).map(|n| Session {
                    id: format!("n{n}"),
                    ..extra.clone()
                }));
                s.total = s.sessions.len();
            }),
            "the row limit counts rows that will be dropped"
        );
        assert!(rejected(&|s| {
            let duplicate = s.sessions[1].clone();
            s.sessions.push(duplicate);
            s.total = 4;
        }));
        assert!(rejected(&|s| s.sessions[0].title = "bad\u{1b}[0m".into()));
        assert!(
            rejected(&|s| s.sessions[1].title = "bad\u{1b}[0m".into()),
            "a dropped row too"
        );
        assert!(rejected(&|s| s.sessions[0].state = "bad\u{1b}[0m".into()));
        assert!(rejected(&|s| s.sessions[1].kind = "bad\u{1b}[0m".into()));
        assert!(rejected(&|s| s.sessions[0].state = "x".repeat(65)));
        assert!(rejected(&|s| s.sessions[1].kind = "x".repeat(65)));
        // The stored form stays strict too: an unknown word in a file means a broken file.
        let mut stored = known;
        stored.sessions[0].state = "pondering".into();
        assert!(stored.validate().is_err());
    }

    // A2: what a host with no tmux or an unreadable store still reports.
    fn saved_chat(home: &Path, state: serde_json::Value) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        let dir = home.join("chats").join(&id);
        fs::create_dir_all(&dir).unwrap();
        let info = serde_json::json!({
            "id": id,
            "provider": "claude",
            "cwd": "/tmp",
            "title": "Saved chat",
            "created_at_unix": 1_790_000_000u64,
            "state": state,
        });
        fs::write(dir.join("info.json"), info.to_string()).unwrap();
        fs::write(dir.join("events.jsonl"), "").unwrap();
        id
    }
    #[test]
    fn a_host_without_tmux_reports_its_chats_and_no_terminals() {
        let home = std::env::temp_dir().join(format!("somtus-host-local-{}", uuid::Uuid::new_v4()));
        let chat = {
            fs::create_dir_all(&home).unwrap();
            saved_chat(&home, serde_json::json!({"state": "stopped"}))
        };
        // A registered shell that a tmux, had there been one, would report as exited.
        let shell = uuid::Uuid::new_v4().to_string();
        let registry = serde_json::json!({"sessions": [{
            "id": shell,
            "project_id": null,
            "worktree_id": null,
            "kind": "project",
            "cwd": "/tmp",
            "command": null,
            "created_at_unix": 0,
        }]});
        fs::write(home.join("sessions.json"), registry.to_string()).unwrap();
        let snapshot = local_with(&home, false).unwrap();
        snapshot.validate().unwrap();
        assert_eq!(
            snapshot
                .sessions
                .iter()
                .map(|row| (row.kind.as_str(), row.id.as_str()))
                .collect::<Vec<_>>(),
            vec![("chat", chat.as_str())]
        );
        assert_eq!(snapshot.total, 1);
        // #53: the shell may well be alive under a tmux this read could not find, so the list
        // says it may be incomplete.
        assert!(snapshot.partial);
        // With no shell registered nothing is missing.
        fs::write(home.join("sessions.json"), r#"{"sessions":[]}"#).unwrap();
        assert!(!local_with(&home, false).unwrap().partial);
        fs::remove_file(home.join("sessions.json")).unwrap();
        assert!(!local_with(&home, false).unwrap().partial);
        // A registry this build cannot read may list some.
        fs::write(home.join("sessions.json"), b"{ not json").unwrap();
        let unreadable = local_with(&home, false).unwrap();
        assert!(unreadable.partial);
        assert_eq!(unreadable.sessions.len(), 1);
        // The identity is the same one, and it is still the one thing that always fails the read.
        assert_eq!(snapshot.host_id, identity(&home).unwrap());
        fs::write(home.join("host-identity"), b"corrupt").unwrap();
        assert!(
            local_with(&home, false)
                .unwrap_err()
                .contains("Invalid saved host identity")
        );
        fs::remove_dir_all(home).unwrap();
    }
    #[test]
    fn a_store_this_build_cannot_read_does_not_hide_the_chats() {
        let home = std::env::temp_dir().join(format!("somtus-host-local-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&home).unwrap();
        let chat = saved_chat(&home, serde_json::json!({"state": "stopped"}));
        fs::write(home.join("state.json"), b"{ not json").unwrap();
        let snapshot = local_with(&home, false).unwrap();
        assert_eq!(snapshot.sessions.len(), 1);
        assert_eq!(snapshot.sessions[0].id, chat);
        fs::remove_dir_all(home).unwrap();
    }
    #[test]
    fn disconnected_newer_status_does_not_resurrect_an_old_completion() {
        let mut row = snapshot().sessions.remove(0);
        row.kind = "terminal".into();
        row.state = "done".into();
        row.since = Some(10);
        row.completion = Some("done:10".into());
        let report = crate::program_status::Snapshot {
            version: 1,
            source: "osc7501".into(),
            shell_id: row.id.clone(),
            generation: "generation".into(),
            connected: false,
            observed_at_unix: 30,
            updated_at_unix: 20,
            root: Some(crate::program_status::Record {
                state: crate::program_status::State::Working,
                kind: None,
                progress: None,
                app: None,
                title: None,
                message: None,
                since_unix: 20,
            }),
            records: BTreeMap::new(),
        };
        apply_program_status(&mut row, &report);
        assert_eq!(row.state, "unknown");
        assert_eq!(row.source, "osc7501-stale");
        assert_eq!(row.completion, None);
        assert_eq!(row.since, None);
        row.state = "done".into();
        row.since = Some(25);
        row.completion = Some("done:25".into());
        apply_program_status(&mut row, &report);
        assert_eq!(row.state, "done");
        assert_eq!(row.completion.as_deref(), Some("done:25"));
    }
}
