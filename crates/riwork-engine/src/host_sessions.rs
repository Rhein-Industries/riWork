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
    pub fn apply(&mut self, snapshot: LocalSnapshot, received_at: u64) -> Result<(), String> {
        snapshot.validate()?;
        if let Some(previous) = &self.snapshot {
            if previous.host_id != snapshot.host_id {
                return Err("The connection now identifies a different host. Remove and pair it again before using it.".into());
            }
        }
        self.id = snapshot.host_id.clone();
        self.snapshot = Some(snapshot);
        self.received_at = Some(received_at);
        self.connection = "online".into();
        self.freshness = "current".into();
        self.error = None;
        Ok(())
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
                || !matches!(s.kind.as_str(), "chat" | "terminal")
                || !plain(&s.title, 2048)
                || s.requests.len() > 8
                || s.requests.iter().any(|s| !plain(s, 64))
                || s.provider.as_ref().is_some_and(|s| !plain(s, 64))
                || !plain(&s.source, 64)
                || s.completion.as_ref().is_some_and(|s| !plain(s, 256))
                || s.project_id.as_ref().is_some_and(|s| !plain(s, 256))
                || s.parent_id.as_ref().is_some_and(|s| !plain(s, 256))
                || !matches!(
                    s.state.as_str(),
                    "starting"
                        | "working"
                        | "waiting"
                        | "blocked"
                        | "error"
                        | "idle"
                        | "done"
                        | "stopped"
                        | "exited"
                        | "unknown"
                )
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
    let now = activity::unix_now();
    let host_id = identity(home)?;
    let shells = SessionManager::at(home.to_path_buf())?
        .sample(false)?
        .shells;
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
    let store = crate::store::Store::open(home)?.snapshot()?;
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
    sessions.sort_by(|a, b| a.kind.cmp(&b.kind).then(a.id.cmp(&b.id)));
    let total = sessions.len();
    sessions.truncate(MAX_SESSIONS);
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
        partial: total > MAX_SESSIONS || observations.chats_complete < observations.chats_total,
    })
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
}
pub fn session_key(host: &str, kind: &str, session: &str) -> String {
    serde_json::to_string(&(host, kind, session)).unwrap()
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
        if cache.hosts.len() > MAX_HOSTS + 1 || cache.seen.len() > 32768 {
            return Self::default();
        }
        cache
            .hosts
            .retain(|key, host| plain(key, 256) && host.valid_cached());
        cache
            .seen
            .retain(|key, token| plain(key, 2048) && plain(token, 256));
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
        let mut retained = self.clone();
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
        let mut seen_bytes = 0usize;
        retained.seen.retain(|key, token| {
            if !live_keys.contains(key) || !plain(key, 2048) || !plain(token, 256) {
                return false;
            }
            let size =
                serde_json::to_vec(&(key, token)).map_or(MAX_SEEN_BYTES + 1, |v| v.len() + 2);
            if seen_bytes + size > MAX_SEEN_BYTES {
                return false;
            }
            seen_bytes += size;
            true
        });
        // Reserve JSON/map punctuation and the seen inventory separately.
        let budget = (MAX_CACHE_BYTES - MAX_SEEN_BYTES - 64 * 1024) / retained.hosts.len().max(1);
        for host in retained.hosts.values_mut() {
            let Some(snapshot) = host.snapshot.as_mut() else {
                continue;
            };
            let rows = std::mem::take(&mut snapshot.sessions);
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
        }
        let bytes = serde_json::to_vec(&retained).map_err(|e| e.to_string())?;
        if bytes.len() > MAX_CACHE_BYTES {
            return Err("Host cache exceeded its byte budget".into());
        }
        atomic_json_bytes(&home.join("host-sessions.json"), &bytes)
    }
    pub fn rows(&self) -> Vec<Host> {
        let mut hosts: BTreeMap<String, Host> = BTreeMap::new();
        for host in self.hosts.values() {
            let key = if host.snapshot.is_some() {
                host.id.clone()
            } else {
                format!("pending:{}", host.id)
            };
            let rank = |h: &Host| (h.connection == "online", h.received_at);
            if hosts.get(&key).is_none_or(|old| rank(host) > rank(old)) {
                hosts.insert(key, host.clone());
            }
        }
        hosts
            .into_values()
            .map(|mut host| {
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
        if self.seen.len() > 32768 {
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
        Ok(())
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
