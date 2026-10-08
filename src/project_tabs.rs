//! The Mac-owned project membership API. All writers apply operations to the
//! latest file under a project lock; window layouts never overwrite this state.
use crate::{
    chat::model::{ChatInfo, ChatState, Provider},
    layouts::{LayoutStore, SavedTab},
};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Chat,
    Shell,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Working,
    Waiting,
    Error,
    Done,
    Stopped,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub key: String,
    pub kind: Kind,
    pub title: String,
    #[serde(default)]
    pub base_title: String,
    #[serde(default)]
    pub title_priority: u8,
    pub status: Status,
    pub pinned: bool,
    pub hidden: bool,
    #[serde(default)]
    pub worker: bool,
    pub order: usize,
    pub parent: Option<String>,
    pub children: Vec<Entry>,
    pub child_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rename: Option<String>,
    #[serde(default)]
    pub created: u64,
}
#[derive(Clone, Debug)]
pub struct Session {
    pub title_priority: u8,
    pub key: String,
    pub kind: Kind,
    pub title: String,
    pub parent_id: Option<String>,
    pub status: Status,
    pub orchestrator: bool,
    pub worker: bool,
    pub created: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Update {
    Pin {
        key: String,
    },
    Unpin {
        key: String,
    },
    Hide {
        key: String,
    },
    Unhide {
        key: String,
    },
    /// Move before a sibling of the same pin group, or to its end (before=null).
    Move {
        key: String,
        before: Option<String>,
    },
    Rename {
        key: String,
        title: String,
    },
}
impl Update {
    pub fn key(&self) -> &str {
        match self {
            Self::Pin { key }
            | Self::Unpin { key }
            | Self::Hide { key }
            | Self::Unhide { key }
            | Self::Move { key, .. }
            | Self::Rename { key, .. } => key,
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        valid_key(self.key())?;
        if let Self::Move {
            before: Some(before),
            ..
        } = self
        {
            valid_key(before)?;
        }
        if let Self::Rename { title, .. } = self {
            if title.chars().count() > 200 || title.chars().any(unsafe_title_character) {
                return Err("title must be at most 200 printable characters".into());
            }
        }
        Ok(())
    }
}
pub fn valid_key(key: &str) -> Result<(), String> {
    let (kind, id) = key.split_once(':').ok_or("invalid session key")?;
    if !matches!(kind, "chat" | "shell")
        || !uuid::Uuid::parse_str(id).is_ok_and(|v| v.to_string() == id)
    {
        return Err("invalid session key".into());
    }
    Ok(())
}
pub fn unsafe_title_character(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200e}' | '\u{200f}' | '\u{061c}')
}
/// Parent attribution is metadata-only and restricted to the destination project.
pub fn same_project_parent(
    home: &Path,
    project: Option<&str>,
    parent: Option<String>,
) -> Option<String> {
    let project = project?;
    let id = parent?;
    let chat = crate::chat::log::chat_dir(home, &id)
        .and_then(|dir| fs::read(dir.join("info.json")).ok())
        .and_then(|bytes| serde_json::from_slice::<ChatInfo>(&bytes).ok());
    if let Some(chat) = chat {
        return (chat.project_id.as_deref() == Some(project)).then_some(id);
    }
    crate::sessions::SessionManager::at(home.to_path_buf())
        .ok()
        .and_then(|manager| manager.registered_session(&id).ok())
        .filter(|shell| shell.project_id.as_deref() == Some(project))
        .map(|_| id)
}
pub fn caller_parent() -> Option<String> {
    ["RIWORK_CHAT_ID", "RIWORK_SHELL_ID"]
        .iter()
        .find_map(|name| {
            std::env::var(name)
                .ok()
                .filter(|id| uuid::Uuid::parse_str(id).is_ok_and(|v| v.to_string() == *id))
        })
}
pub fn message_title(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .take(40)
        .collect()
}
impl Session {
    pub fn chat(info: &ChatInfo) -> Self {
        Self {
            title_priority: if info.user_title.is_some() {
                2
            } else if info.first_user_message.is_some() {
                1
            } else {
                0
            },
            key: format!("chat:{}", info.id),
            kind: Kind::Chat,
            title: info
                .user_title
                .clone()
                .or_else(|| info.first_user_message.clone())
                .or_else(|| {
                    (!matches!(info.title.as_str(), "Codex chat" | "Claude chat" | ""))
                        .then(|| info.title.clone())
                })
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| match info.provider {
                    Provider::Claude => "Claude chat".into(),
                    Provider::Codex => "Codex chat".into(),
                }),
            parent_id: info.parent_id.clone(),
            worker: info.parent_id.is_some(),
            status: match info.state {
                ChatState::Starting | ChatState::Running => Status::Working,
                ChatState::Waiting => Status::Waiting,
                ChatState::Idle => Status::Done,
                ChatState::Stopped => Status::Stopped,
                ChatState::Failed { .. } => Status::Error,
            },
            orchestrator: matches!(
                info.orchestrator,
                Some(crate::chat::model::OrchestratorScope::Project { .. })
            ),
            created: info.created_at_unix,
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    revision: u64,
    entries: Vec<Entry>,
    #[serde(default)]
    deleted: HashSet<String>,
    #[serde(default)]
    retired: HashSet<String>,
}
#[derive(Clone, Debug)]
pub struct TabStore {
    home: PathBuf,
    project: String,
}
impl TabStore {
    pub fn at(home: impl Into<PathBuf>, project: &str) -> Result<Self, String> {
        if !uuid::Uuid::parse_str(project).is_ok_and(|v| v.to_string() == project) {
            return Err("invalid project UUID".into());
        }
        Ok(Self {
            home: home.into(),
            project: project.into(),
        })
    }
    fn transact<T>(&self, f: impl FnOnce(&mut State) -> Result<T, String>) -> Result<T, String> {
        let dir = self.home.join("project-tabs");
        crate::paths::create_private_dir(&dir).map_err(|e| e.to_string())?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(dir.join(format!("{}.lock", self.project)))
            .map_err(|e| e.to_string())?;
        lock.lock_exclusive().map_err(|e| e.to_string())?;
        let path = dir.join(format!("{}.json", self.project));
        let exists = path.exists();
        let mut state = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| format!("Cannot read {}: {e}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => self.migrate()?,
            Err(e) => return Err(e.to_string()),
        };
        let mut keys = HashSet::new();
        for entry in &state.entries {
            valid_key(&entry.key)?;
            if !keys.insert(&entry.key) {
                return Err("duplicate tab key in stored project list".into());
            }
        }
        let before = durable_bytes(&state)?;
        let answer = f(&mut state)?;
        if exists && before == durable_bytes(&state)? {
            return Ok(answer);
        }
        state.revision = state
            .revision
            .checked_add(1)
            .ok_or("tab revision exhausted")?;
        let data = serde_json::to_vec(&state).map_err(|e| e.to_string())?;
        let tmp = dir.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)
                .map_err(|e| e.to_string())?;
            file.write_all(&data).map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            fs::rename(&tmp, &path).map_err(|e| e.to_string())
        })();
        if result.is_err() {
            let _ = fs::remove_file(tmp);
        }
        result?;
        Ok(answer)
    }
    fn metadata_keys(&self) -> Result<HashSet<String>, String> {
        Ok(crate::chat::log::read_infos_checked(&self.home)?
            .into_iter()
            .filter(|c| c.project_id.as_deref() == Some(&self.project))
            .map(|c| format!("chat:{}", c.id))
            .chain(
                crate::sessions::SessionManager::at(self.home.clone())?
                    .registered_sessions()?
                    .into_iter()
                    .filter(|s| s.project_id.as_deref() == Some(&self.project))
                    .map(|s| format!("shell:{}", s.id)),
            )
            .collect())
    }
    fn migrate(&self) -> Result<State, String> {
        let mut state = State::default();
        let known = self.metadata_keys()?;
        if let Some(layout) = LayoutStore::open(&self.home)?.load(&self.project)? {
            let panes = layout.layout.pane_ids();
            for pane in panes.iter().filter_map(|id| layout.panes.get(id)) {
                for key in pane
                    .tabs
                    .iter()
                    .filter_map(|tab| match tab {
                        SavedTab::Chat { .. } | SavedTab::Shell { .. } => Some(tab.key()),
                        _ => None,
                    })
                    .chain(pane.shell_ids.iter().map(|id| format!("shell:{id}")))
                {
                    if known.contains(&key) {
                        seed(&mut state, key, false);
                    }
                }
            }
            let mut hidden = layout
                .detached_chat_ids
                .iter()
                .map(|id| format!("chat:{id}"))
                .chain(
                    layout
                        .detached_shell_ids
                        .iter()
                        .map(|id| format!("shell:{id}")),
                )
                .collect::<Vec<_>>();
            hidden.sort();
            for key in hidden {
                if known.contains(&key) {
                    seed(&mut state, key, true);
                }
            }
        }
        Ok(state)
    }
    /// Merge metadata without replacing membership or user operations. A partial or
    /// stale snapshot cannot delete entries; explicit deletion calls `forget`.
    /// Complete host/registry inventory: prune vanished sessions and bound stopped history.
    pub fn reconcile_inventory(&self, sessions: &[Session]) -> Result<Vec<Entry>, String> {
        self.transact(|state| {
            // An unreadable metadata directory is unknown, not an empty inventory.
            if let Ok(mut known) = self.metadata_keys() {
                known.extend(sessions.iter().map(|s| s.key.clone()));
                state.entries.retain(|e| known.contains(&e.key));
                state.retired.retain(|key| known.contains(key));
            }
            // Only hidden, stopped leaf history is expendable. Preserve parents
            // with children so retention cannot flood the root strip.
            let parents = state
                .entries
                .iter()
                .filter_map(|e| e.parent.clone())
                .collect::<HashSet<_>>();
            let stopped = sessions
                .iter()
                .filter(|s| s.kind == Kind::Chat && s.status == Status::Stopped)
                .map(|s| s.key.as_str())
                .collect::<HashSet<_>>();
            let mut hidden = state
                .entries
                .iter()
                .filter(|e| {
                    e.hidden
                        && !e.pinned
                        && stopped.contains(e.key.as_str())
                        && !parents.contains(&e.key)
                })
                .collect::<Vec<_>>();
            hidden.sort_by_key(|e| std::cmp::Reverse((e.created, &e.key)));
            let expired = hidden
                .into_iter()
                .skip(200)
                .map(|e| e.key.clone())
                .collect::<HashSet<_>>();
            // Retention tombstones prevent the next inventory from resurrecting
            // a closed entry. Unlike explicit deletions these live until metadata
            // disappears; visible sessions never enter this set.
            state.retired.extend(expired.iter().cloned());
            state.entries.retain(|e| !expired.contains(&e.key));
            Ok(())
        })?;
        self.reconcile(sessions)
    }
    pub fn reconcile(&self, sessions: &[Session]) -> Result<Vec<Entry>, String> {
        self.transact(|state| {
            let dead_shells = sessions
                .iter()
                .filter(|s| s.kind == Kind::Shell && s.status == Status::Stopped)
                .map(|s| s.key.clone())
                .collect::<HashSet<_>>();
            state.entries.retain(|e| !dead_shells.contains(&e.key));
            let ids = state
                .entries
                .iter()
                .map(|e| (e.key.rsplit(':').next().unwrap().to_owned(), e.key.clone()))
                .chain(
                    sessions
                        .iter()
                        .map(|s| (s.key.rsplit(':').next().unwrap().to_owned(), s.key.clone())),
                )
                .collect::<HashMap<_, _>>();

            let mut sessions = sessions.iter().collect::<Vec<_>>();
            sessions.sort_by_key(|s| (s.created, &s.key));
            for session in &sessions {
                if state.deleted.contains(&session.key)
                    || state.retired.contains(&session.key)
                    || dead_shells.contains(&session.key)
                {
                    continue;
                }
                let parent = session
                    .parent_id
                    .as_ref()
                    .and_then(|id| ids.get(id))
                    .filter(|key| {
                        *key != &session.key
                            && !state.deleted.contains(*key)
                            && !state.retired.contains(*key)
                            && !dead_shells.contains(*key)
                    })
                    .cloned();
                if let Some(entry) = state.entries.iter_mut().find(|e| e.key == session.key) {
                    if session.orchestrator && entry.title.is_empty() && !entry.hidden {
                        entry.pinned = true;
                        entry.hidden = false;
                    }
                    if session.title_priority >= entry.title_priority {
                        entry.base_title = session.title.clone();
                        entry.title_priority = session.title_priority;
                    }
                    entry.title = entry
                        .rename
                        .clone()
                        .unwrap_or_else(|| entry.base_title.clone());
                    if entry.created == 0 && session.worker {
                        entry.hidden = true;
                    }
                    entry.created = session.created;
                    entry.status = session.status.clone();
                    entry.parent = parent;
                    entry.worker = session.worker;
                } else {
                    state.entries.push(Entry {
                        key: session.key.clone(),
                        kind: session.kind.clone(),
                        title: session.title.clone(),
                        base_title: session.title.clone(),
                        title_priority: session.title_priority,
                        status: session.status.clone(),
                        pinned: session.orchestrator,
                        hidden: session.worker
                            || (session.kind == Kind::Shell && session.status == Status::Stopped),
                        worker: session.worker,
                        order: 0,
                        parent,
                        children: vec![],
                        child_count: 0,
                        rename: None,
                        created: session.created,
                    });
                }
            }
            for e in &mut state.entries {
                if e.rename.is_none() && !e.base_title.is_empty() {
                    e.title = e.base_title.clone();
                }
            }
            disambiguate(state);
            let known = state
                .entries
                .iter()
                .map(|e| e.key.clone())
                .collect::<HashSet<_>>();
            for entry in &mut state.entries {
                if entry
                    .parent
                    .as_ref()
                    .is_some_and(|p| !known.contains(p) || p == &entry.key)
                {
                    entry.parent = None;
                }
            }
            bound_parent_depth(state);
            normalize(state);
            Ok(tree(state))
        })
    }
    pub fn snapshot(&self) -> Result<Snapshot, String> {
        self.list()?;
        self.transact(|state| {
            Ok(Snapshot {
                revision: state.revision,
                entries: tree(state),
                known_keys: state
                    .entries
                    .iter()
                    .map(|e| e.key.clone())
                    .chain(state.deleted.iter().cloned())
                    .chain(state.retired.iter().cloned())
                    .collect(),
            })
        })
    }
    pub fn forget(&self, key: &str) -> Result<(), String> {
        valid_key(key)?;
        self.transact(|state| {
            state.entries.retain(|e| e.key != key);
            state.deleted.insert(key.into());
            for child in &mut state.entries {
                if child.parent.as_deref() == Some(key) {
                    child.parent = None;
                }
            }
            if state.deleted.len() > 1024 {
                let mut keys = state.deleted.iter().cloned().collect::<Vec<_>>();
                keys.sort();
                for old in keys
                    .into_iter()
                    .filter(|old| old != key)
                    .take(state.deleted.len() - 1024)
                {
                    state.deleted.remove(&old);
                }
            }
            Ok(())
        })
    }
    pub fn list(&self) -> Result<Vec<Entry>, String> {
        self.transact(|state| {
            bound_parent_depth(state);
            normalize(state);
            Ok(tree(state))
        })
    }
    pub fn update(&self, update: &Update) -> Result<Vec<Entry>, String> {
        update.validate()?;
        self.transact(|state| {
            let at = state
                .entries
                .iter()
                .position(|e| e.key == update.key())
                .ok_or("session tab not found")?;
            match update {
                Update::Pin { .. } => {
                    if state.entries[at].parent.is_some() {
                        return Err("only root tabs can be pinned".into());
                    }
                    state.entries[at].pinned = true;
                    state.entries[at].hidden = false;
                }
                Update::Unpin { .. } => state.entries[at].pinned = false,
                Update::Hide { .. } => {
                    if state.entries[at].pinned {
                        return Err("unpin the tab before hiding it".into());
                    }
                    state.entries[at].hidden = true;
                }
                Update::Unhide { .. } => state.entries[at].hidden = false,
                Update::Rename { title, .. } => {
                    state.entries[at].rename =
                        (!title.trim().is_empty()).then(|| title.trim().into());
                    state.entries[at].title = state.entries[at]
                        .rename
                        .clone()
                        .unwrap_or_else(|| state.entries[at].base_title.clone());
                }
                Update::Move { before, .. } => {
                    if before.as_deref() == Some(update.key()) {
                        return Ok(tree(state));
                    }
                    let (parent, pinned) =
                        (state.entries[at].parent.clone(), state.entries[at].pinned);
                    if let Some(before) = before {
                        let target = state
                            .entries
                            .iter()
                            .find(|e| &e.key == before)
                            .ok_or("move target not found")?;
                        if target.parent != parent || target.pinned != pinned {
                            return Err(
                                "move target must be a sibling in the same pin group".into()
                            );
                        }
                    }
                    let entry = state.entries.remove(at);
                    let into = before
                        .as_ref()
                        .and_then(|key| state.entries.iter().position(|e| &e.key == key))
                        .unwrap_or(state.entries.len());
                    state.entries.insert(into, entry);
                }
            }
            disambiguate(state);
            normalize(state);
            Ok(tree(state))
        })
    }
}
fn seed(state: &mut State, key: String, hidden: bool) {
    if valid_key(&key).is_err() {
        return;
    }
    if let Some(e) = state.entries.iter_mut().find(|e| e.key == key) {
        e.hidden |= hidden;
        return;
    }
    let kind = if key.starts_with("chat:") {
        Kind::Chat
    } else {
        Kind::Shell
    };
    state.entries.push(Entry {
        key,
        kind,
        title: String::new(),
        base_title: String::new(),
        title_priority: 0,
        status: Status::Stopped,
        pinned: false,
        hidden,
        worker: false,
        order: state.entries.len(),
        parent: None,
        children: vec![],
        child_count: 0,
        rename: None,
        created: 0,
    });
}
/// Root depth is zero. Promote the first child beyond eight edges to a root;
/// cycles are also broken. Stable input order makes subsequent polls idempotent.
fn bound_parent_depth(state: &mut State) {
    let mut parents = state
        .entries
        .iter()
        .map(|e| (e.key.clone(), e.parent.clone()))
        .collect::<HashMap<_, _>>();
    for entry in &mut state.entries {
        let mut cursor = entry.key.clone();
        let mut seen = HashSet::new();
        let mut depth = 0;
        while let Some(Some(parent)) = parents.get(&cursor) {
            if !seen.insert(cursor.clone()) || depth >= 8 {
                entry.parent = None;
                parents.insert(entry.key.clone(), None);
                break;
            }
            cursor = parent.clone();
            depth += 1;
        }
        if entry.parent.is_some() {
            entry.pinned = false;
        }
    }
}
fn normalize(state: &mut State) {
    state.entries.sort_by_key(|e| !e.pinned);
    for (order, e) in state.entries.iter_mut().enumerate() {
        e.order = order;
        if e.pinned {
            e.hidden = false;
        }
        e.children.clear();
        e.child_count = 0;
    }
}
// Status-only polling does not advance the durable revision or write/fsync the file.
fn durable_bytes(state: &State) -> Result<Vec<u8>, String> {
    let mut state = state.clone();
    for e in &mut state.entries {
        e.status = Status::Stopped;
    }
    serde_json::to_vec(&state).map_err(|e| e.to_string())
}
fn disambiguate(state: &mut State) {
    for e in &mut state.entries {
        e.title = e.rename.clone().unwrap_or_else(|| e.base_title.clone());
    }
    let mut groups = HashMap::<String, Vec<usize>>::new();
    for (i, e) in state.entries.iter().enumerate() {
        if e.kind == Kind::Shell && e.rename.is_none() && !e.hidden && e.status != Status::Stopped {
            groups.entry(e.base_title.clone()).or_default().push(i);
        }
    }
    for indices in groups.values_mut().filter(|v| v.len() > 1) {
        indices.sort_by_key(|i| (state.entries[*i].created, state.entries[*i].key.clone()));
        for (n, i) in indices.iter().enumerate() {
            state.entries[*i].title = format!("{} · {}", state.entries[*i].base_title, n + 1);
        }
    }
}
// Internal entries are flat. Child lookup/count is linear, without duplicating subtrees.
fn tree(state: &State) -> Vec<Entry> {
    let mut counts = HashMap::<&str, usize>::new();
    for e in &state.entries {
        if let Some(parent) = &e.parent {
            *counts.entry(parent).or_default() += 1;
        }
    }
    state
        .entries
        .iter()
        .map(|e| {
            let mut e = e.clone();
            e.children.clear();
            e.child_count = counts.get(e.key.as_str()).copied().unwrap_or(0);
            e
        })
        .collect()
}
pub fn snapshot_for(store: &TabStore, sessions: &[Session]) -> Result<Snapshot, String> {
    let mut snapshot = store.snapshot()?;
    let status = sessions
        .iter()
        .map(|s| (s.key.as_str(), &s.status))
        .collect::<HashMap<_, _>>();
    for e in &mut snapshot.entries {
        if let Some(s) = status.get(e.key.as_str()) {
            e.status = (*s).clone();
        }
    }
    Ok(snapshot)
}
pub fn refresh_snapshot(home: &Path, project: &str) -> Result<Snapshot, String> {
    let sessions = inventory(home, project)?;
    let store = TabStore::at(home, project)?;
    store.reconcile_inventory(&sessions)?;
    snapshot_for(&store, &sessions)
}
/// Shared base title, following the local shell's current directory and program.
/// Numbering and user overrides are applied by the store, never by pane IDs.
pub fn shell_base_title(
    shell: &crate::sessions::ShellSession,
    state: &crate::store::State,
    cwd: Option<&std::path::Path>,
) -> String {
    if let Some(path) = &shell.editor_path {
        return format!(
            "VIM · {}",
            path.file_name().unwrap_or_default().to_string_lossy()
        );
    }
    if shell.kind == crate::sessions::ShellKind::Orchestrator {
        return "Project orchestrator".into();
    }
    let cwd = cwd.unwrap_or(&shell.cwd);
    let branch = state
        .worktrees
        .iter()
        .filter(|w| {
            shell.project_id.as_deref() == Some(w.project_id.as_str()) && cwd.starts_with(&w.path)
        })
        .max_by_key(|w| w.path.as_os_str().len())
        .map(|w| w.branch.as_str())
        .unwrap_or("outside");
    let program = shell.harness.map(|h| format!("{h:?}")).unwrap_or_else(|| {
        std::env::var_os("SHELL")
            .filter(|s| std::path::Path::new(s).is_file())
            .and_then(|s| {
                std::path::Path::new(&s)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "zsh".into())
    });
    format!("{program} · {branch}")
}
/// Reads inventory without launching a provider; shared by CLI and desktop polling.
pub fn inventory(home: &Path, project: &str) -> Result<Vec<Session>, String> {
    let state = crate::store::Store::open(home)?.snapshot()?;
    state.project(project)?;
    let manager = crate::sessions::SessionManager::at(home.to_owned())?;
    let (shells, activity) = manager.list_with_activity()?;
    let entries = serde_json::to_value(crate::cli_agents::shell_entries(home, &shells, &activity))
        .map_err(|e| e.to_string())?;
    let entries = entries.as_array().ok_or("invalid shell inventory")?;
    let directories = manager.sample(false)?.directories.unwrap_or_default();
    let mut result = Vec::new();
    for shell in shells
        .iter()
        .filter(|s| s.project_id.as_deref() == Some(project))
    {
        let name = shell_base_title(
            shell,
            &state,
            directories.get(&shell.id).map(PathBuf::as_path),
        );
        let status = if !shell.alive {
            Status::Stopped
        } else {
            match entries
                .iter()
                .find(|e| e["id"].as_str() == Some(&shell.id))
                .and_then(|e| e["activity"].as_str())
            {
                Some("working") => Status::Working,
                Some("waiting") => Status::Waiting,
                _ => Status::Done,
            }
        };
        result.push(Session {
            title_priority: if shell.editor_path.is_some() { 2 } else { 1 },
            key: format!("shell:{}", shell.id),
            kind: Kind::Shell,
            title: name,
            parent_id: shell.parent_id.clone(),
            worker: shell.parent_id.is_some()
                || (shell.harness.is_some()
                    && !shell.user_opened
                    && shell.kind != crate::sessions::ShellKind::Orchestrator),
            status,
            orchestrator: shell.kind == crate::sessions::ShellKind::Orchestrator,
            created: shell.created_at_unix,
        });
    }
    let chats = match crate::chat::client::Client::connect(&crate::chat::client::socket_path(home))
    {
        Ok(mut client) => client.list()?,
        Err(_) => crate::chat::log::read_infos_checked(home)?,
    };
    result.extend(
        chats
            .iter()
            .filter(|c| c.project_id.as_deref() == Some(project))
            .map(Session::chat),
    );
    Ok(result)
}
pub fn register_shell(
    home: &Path,
    shell: &crate::sessions::ShellSession,
    project: &str,
) -> Result<(), String> {
    let state = crate::store::Store::open(home)?.snapshot()?;
    let branch = state
        .worktrees
        .iter()
        .find(|w| {
            w.project_id == project
                && shell
                    .worktree_id
                    .as_deref()
                    .map_or(w.is_primary, |id| w.id == id)
        })
        .map(|w| w.branch.as_str());
    let name = if let Some(path) = &shell.editor_path {
        format!(
            "VIM · {}",
            path.file_name().unwrap_or_default().to_string_lossy()
        )
    } else if shell.kind == crate::sessions::ShellKind::Orchestrator {
        "Project orchestrator".into()
    } else {
        let name = shell
            .harness
            .map(|h| format!("{h:?}"))
            .unwrap_or_else(|| "Shell".into());
        branch.map_or(name.clone(), |b| format!("{name} · {b}"))
    };
    TabStore::at(home, project)?.reconcile(&[Session {
        title_priority: if shell.editor_path.is_some() {
            2
        } else {
            u8::from(branch.is_some())
        },
        key: format!("shell:{}", shell.id),
        kind: Kind::Shell,
        title: name,
        parent_id: shell.parent_id.clone(),
        worker: shell.parent_id.is_some()
            || (shell.harness.is_some()
                && !shell.user_opened
                && shell.kind != crate::sessions::ShellKind::Orchestrator),
        status: Status::Done,
        orchestrator: shell.kind == crate::sessions::ShellKind::Orchestrator,
        created: shell.created_at_unix,
    }])?;
    Ok(())
}
#[derive(Clone, Debug)]
pub struct Snapshot {
    /// Internal ownership includes tombstones; unrelated local views are outside this project.
    pub known_keys: HashSet<String>,
    pub revision: u64,
    pub entries: Vec<Entry>,
}

/// Public wire projection excludes persistence details from both call results.
pub fn wire(entries: &[Entry]) -> serde_json::Value {
    let mut nodes = entries
        .iter()
        .map(|e| (e.key.clone(), e))
        .collect::<HashMap<_, _>>();
    let mut children = HashMap::<String, Vec<String>>::new();
    let mut roots = Vec::new();
    for e in entries {
        if let Some(parent) = &e.parent {
            children
                .entry(parent.clone())
                .or_default()
                .push(e.key.clone());
        } else {
            roots.push(e.key.clone());
        }
    }
    fn take(
        key: &str,
        nodes: &mut HashMap<String, &Entry>,
        children: &mut HashMap<String, Vec<String>>,
    ) -> Option<serde_json::Value> {
        let e = nodes.remove(key)?;
        let nested = children
            .remove(key)
            .unwrap_or_default()
            .iter()
            .filter_map(|k| take(k, nodes, children))
            .collect::<Vec<_>>();
        let mut value = serde_json::json!({"key":e.key,"kind":e.kind,"title":e.title,"status":e.status,"pinned":e.pinned,"hidden":e.hidden,"order":e.order,"parent":e.parent,"child_count":e.child_count});
        value
            .as_object_mut()
            .unwrap()
            .insert("children".into(), serde_json::Value::Array(nested));
        Some(value)
    }
    let mut result = roots
        .iter()
        .filter_map(|key| take(key, &mut nodes, &mut children))
        .collect::<Vec<_>>();
    // Damaged legacy cycles still remain reachable, exactly once.
    for e in entries {
        if let Some(mut value) = take(&e.key, &mut nodes, &mut children) {
            value["parent"] = serde_json::Value::Null;
            result.push(value);
        }
    }
    serde_json::json!({"entries":result})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn id(n: u8) -> String {
        format!("00000000-0000-4000-8000-{n:012}")
    }
    struct Fixture {
        home: PathBuf,
        store: TabStore,
    }
    impl Fixture {
        fn new() -> Self {
            let home = std::env::temp_dir().join(format!("riwork-tabs-b-{}", uuid::Uuid::new_v4()));
            Self {
                store: TabStore::at(&home, &id(1)).unwrap(),
                home,
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.home);
        }
    }
    fn session(n: u8) -> Session {
        Session {
            title_priority: 0,
            key: format!("chat:{}", id(n)),
            kind: Kind::Chat,
            title: format!("Chat {n}"),
            parent_id: None,
            status: Status::Working,
            orchestrator: false,
            worker: false,
            created: n.into(),
        }
    }
    #[test]
    fn workers_start_hidden_open_on_both_devices_and_never_resurrect_after_detach() {
        let f = Fixture::new();
        let root = session(2);
        let mut worker = session(3);
        worker.parent_id = Some(id(2));
        worker.worker = true;
        let initial = f.store.reconcile(&[root.clone(), worker.clone()]).unwrap();
        assert!(!initial.iter().find(|e| e.key == root.key).unwrap().hidden);
        assert!(initial.iter().find(|e| e.key == worker.key).unwrap().hidden);
        let peer = TabStore::at(&f.home, &id(1)).unwrap();
        peer.update(&Update::Unhide {
            key: worker.key.clone(),
        })
        .unwrap();
        assert!(
            !f.store
                .reconcile(&[root.clone(), worker.clone()])
                .unwrap()
                .iter()
                .find(|e| e.key == worker.key)
                .unwrap()
                .hidden
        );
        f.store
            .update(&Update::Hide {
                key: worker.key.clone(),
            })
            .unwrap();
        let closed_revision = f.store.snapshot().unwrap().revision;
        for _ in 0..3 {
            peer.reconcile(&[root.clone(), worker.clone()]).unwrap();
        }
        let after = peer.snapshot().unwrap();
        assert_eq!(after.revision, closed_revision);
        let child = after.entries.iter().find(|e| e.key == worker.key).unwrap();
        assert!(child.hidden && child.worker);
        assert_eq!(child.parent.as_deref(), Some(root.key.as_str()));
        f.store.forget(&root.key).unwrap();
        let orphan = peer
            .list()
            .unwrap()
            .into_iter()
            .find(|e| e.key == worker.key)
            .unwrap();
        assert!(orphan.hidden && orphan.worker);
        assert!(orphan.parent.is_none());
    }
    #[test]
    fn background_harness_roots_are_hidden_but_explicit_user_sessions_are_visible() {
        let f = Fixture::new();
        let mut harness = session(3);
        harness.kind = Kind::Shell;
        harness.key = format!("shell:{}", id(3));
        harness.worker = true;
        let user = session(2);
        let entries = f.store.reconcile(&[harness.clone(), user.clone()]).unwrap();
        assert!(
            entries
                .iter()
                .find(|e| e.key == harness.key)
                .unwrap()
                .hidden
        );
        assert!(!entries.iter().find(|e| e.key == user.key).unwrap().hidden);
    }
    #[test]
    fn new_sessions_append_and_pin_groups_are_first() {
        let f = Fixture::new();
        f.store.reconcile(&[session(3), session(2)]).unwrap();
        let keys = f
            .store
            .reconcile(&[session(4)])
            .unwrap()
            .into_iter()
            .map(|e| e.key)
            .collect::<Vec<_>>();
        assert_eq!(keys, vec![session(2).key, session(3).key, session(4).key]);
        let list = f
            .store
            .update(&Update::Pin {
                key: session(4).key,
            })
            .unwrap();
        assert_eq!(list[0].key, session(4).key);
        assert!(
            f.store
                .update(&Update::Hide {
                    key: session(4).key
                })
                .is_err()
        );
        f.store
            .update(&Update::Unpin {
                key: session(4).key,
            })
            .unwrap();
        assert!(
            f.store
                .update(&Update::Hide {
                    key: session(4).key
                })
                .unwrap()[0]
                .hidden
        );
    }
    #[test]
    fn stale_inventory_keeps_hide_pin_and_rename() {
        let f = Fixture::new();
        let s = session(2);
        f.store.reconcile(&[s.clone()]).unwrap();
        let other = TabStore::at(&f.home, &id(1)).unwrap();
        other
            .update(&Update::Rename {
                key: s.key.clone(),
                title: "Mine".into(),
            })
            .unwrap();
        other.update(&Update::Hide { key: s.key.clone() }).unwrap();
        let entries = f.store.reconcile(&[s.clone()]).unwrap();
        assert_eq!(entries[0].title, "Mine");
        assert!(entries[0].hidden);
        f.store.update(&Update::Pin { key: s.key }).unwrap();
        let entries = other.list().unwrap();
        assert!(entries[0].pinned);
        assert!(!entries[0].hidden);
    }
    #[test]
    fn moves_are_durable_and_do_not_cross_pin_or_parent_groups() {
        let f = Fixture::new();
        f.store
            .reconcile(&[session(2), session(3), session(4)])
            .unwrap();
        let list = f
            .store
            .update(&Update::Move {
                key: session(4).key,
                before: Some(session(2).key),
            })
            .unwrap();
        assert_eq!(list[0].key, session(4).key);
        f.store
            .update(&Update::Pin {
                key: session(2).key,
            })
            .unwrap();
        assert!(
            f.store
                .update(&Update::Move {
                    key: session(3).key,
                    before: Some(session(2).key)
                })
                .is_err()
        );
        assert_eq!(f.store.list().unwrap()[1].key, session(4).key);
    }
    #[test]
    fn parents_cross_kinds_and_children_carry_status() {
        let f = Fixture::new();
        let parent = session(2);
        let mut child = session(3);
        child.kind = Kind::Shell;
        child.key = format!("shell:{}", id(3));
        child.parent_id = Some(id(2));
        child.status = Status::Waiting;
        let list = f.store.reconcile(&[child.clone(), parent.clone()]).unwrap();
        assert_eq!(list[0].child_count, 1);
        assert_eq!(
            wire(&list)["entries"][0]["children"][0]["status"],
            "waiting"
        );
        assert_eq!(wire(&list)["entries"].as_array().unwrap().len(), 1);
        assert_eq!(list[1].parent, Some(parent.key));
        assert_eq!(list.iter().filter(|e| e.parent.is_none()).count(), 1);
    }
    #[test]
    fn orphans_are_roots_and_deleted_parents_never_reappear() {
        let f = Fixture::new();
        let parent = session(2);
        let mut child = session(3);
        child.parent_id = Some(id(2));
        assert!(
            f.store.reconcile(&[child.clone()]).unwrap()[0]
                .parent
                .is_none()
        );
        let entries = f.store.reconcile(&[parent.clone(), child.clone()]).unwrap();
        assert_eq!(
            entries
                .iter()
                .find(|e| e.key == child.key)
                .unwrap()
                .parent
                .as_deref(),
            Some(parent.key.as_str())
        );
        f.store.forget(&parent.key).unwrap();
        assert!(f.store.list().unwrap()[0].parent.is_none());
        assert!(
            f.store.reconcile(&[parent, child]).unwrap()[0]
                .parent
                .is_none()
        );
    }
    #[test]
    fn deletion_tombstone_resists_stale_inventory() {
        let f = Fixture::new();
        let s = session(2);
        f.store.reconcile(&[s.clone()]).unwrap();
        f.store.forget(&s.key).unwrap();
        assert!(f.store.reconcile(&[s]).unwrap().is_empty());
    }
    #[test]
    fn first_message_title_is_unicode_bounded_and_never_regresses() {
        assert_eq!(message_title("  hello\n world  "), "hello world");
        assert_eq!(message_title(&"猫".repeat(80)).chars().count(), 40);
        let f = Fixture::new();
        let old = session(2);
        let mut new = old.clone();
        new.title = "First message".into();
        new.title_priority = 1;
        f.store.reconcile(&[new]).unwrap();
        assert_eq!(f.store.reconcile(&[old]).unwrap()[0].title, "First message");
    }
    #[test]
    fn duplicate_shell_names_number_only_shells_and_remain_stable() {
        let f = Fixture::new();
        let mut a = session(2);
        a.kind = Kind::Shell;
        a.key = format!("shell:{}", id(2));
        a.title = "Codex · main".into();
        let mut b = a.clone();
        b.key = format!("shell:{}", id(3));
        b.created = 3;
        assert_eq!(
            f.store.reconcile(&[a.clone()]).unwrap()[0].title,
            "Codex · main"
        );
        let list = f.store.reconcile(&[b.clone()]).unwrap();
        assert_eq!(list[0].title, "Codex · main · 1");
        assert_eq!(list[1].title, "Codex · main · 2");
        assert_eq!(f.store.reconcile(&[a, b]).unwrap(), list);
    }
    #[test]
    fn migration_preserves_pane_order_dismissals_and_orchestrator_default() {
        use crate::layouts::{Layout, ProjectLayout, SavedPane};
        let f = Fixture::new();
        for n in [2, 3, 4] {
            let dir = f.home.join("chats").join(id(n));
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("info.json"), serde_json::to_vec(&serde_json::json!({"id":id(n),"provider":"codex","project_id":id(1),"cwd":"/tmp","title":"known","created_at_unix":n})).unwrap()).unwrap();
        }
        let mut layout: ProjectLayout =
            serde_json::from_value(serde_json::json!({"layout":Layout::Pane(1),"active_pane":1}))
                .unwrap();
        layout.panes.insert(
            1,
            SavedPane {
                tabs: vec![
                    SavedTab::Chat { chat_id: id(3) },
                    SavedTab::Chat { chat_id: id(2) },
                ],
                ..Default::default()
            },
        );
        layout.detached_chat_ids.insert(id(4));
        LayoutStore::open(&f.home)
            .unwrap()
            .save(&id(1), &layout)
            .unwrap();
        let mut orch = session(2);
        orch.orchestrator = true;
        let mut dismissed_orch = session(4);
        dismissed_orch.orchestrator = true;
        let list = f
            .store
            .reconcile(&[orch, session(3), dismissed_orch])
            .unwrap();
        assert_eq!(list[0].key, session(2).key);
        assert!(list[0].pinned);
        assert!(list[2].hidden);
        assert!(!list[2].pinned);
        layout.detached_chat_ids.clear();
        LayoutStore::open(&f.home)
            .unwrap()
            .save(&id(1), &layout)
            .unwrap();
        assert!(f.store.list().unwrap()[2].hidden);
    }
    #[test]
    fn pruning_history_status_writes_and_stable_numbering() {
        let f = Fixture::new();
        let mut a = session(2);
        a.kind = Kind::Shell;
        a.key = format!("shell:{}", id(2));
        a.title = "VIM · file".into();
        a.created = 10;
        let mut b = a.clone();
        b.key = format!("shell:{}", id(3));
        b.created = 20;
        let list = f.store.reconcile(&[b.clone(), a.clone()]).unwrap();
        assert_eq!(list[0].title, "VIM · file · 1");
        f.store
            .update(&Update::Move {
                key: b.key.clone(),
                before: Some(a.key.clone()),
            })
            .unwrap();
        assert_eq!(
            f.store.reconcile(&[a.clone(), b.clone()]).unwrap()[0].title,
            "VIM · file · 2"
        );
        f.store
            .update(&Update::Hide { key: b.key.clone() })
            .unwrap();
        assert_eq!(
            f.store.reconcile(&[a.clone(), b.clone()]).unwrap()[1].title,
            "VIM · file"
        );
        f.store
            .update(&Update::Rename {
                key: a.key.clone(),
                title: "Override".into(),
            })
            .unwrap();
        assert_eq!(
            f.store
                .update(&Update::Rename {
                    key: a.key.clone(),
                    title: "".into()
                })
                .unwrap()[1]
                .title,
            "VIM · file"
        );
        f.store.reconcile_inventory(&[a.clone()]).unwrap();
        assert_eq!(f.store.list().unwrap().len(), 1);
        let path = f.home.join("project-tabs").join(format!("{}.json", id(1)));
        let before = fs::read(&path).unwrap();
        let revision = f.store.snapshot().unwrap().revision;
        a.status = Status::Waiting;
        assert_eq!(f.store.reconcile(&[a]).unwrap()[0].status, Status::Waiting);
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(f.store.snapshot().unwrap().revision, revision);
        assert!(
            Update::Rename {
                key: b.key,
                title: "bad\u{202e}title".into()
            }
            .validate()
            .is_err()
        );
    }
    #[test]
    fn migration_ignores_unknown_layout_ids_and_dead_registration_is_hidden() {
        let f = Fixture::new();
        let layout: crate::layouts::ProjectLayout =
            serde_json::from_value(serde_json::json!({"layout":{"pane":1},"active_pane":1}))
                .unwrap_or_else(|_| {
                    serde_json::from_value(
                serde_json::json!({"layout":crate::layouts::Layout::Pane(1),"active_pane":1}),
            )
            .unwrap()
                });
        let mut layout = layout;
        layout.panes.insert(
            1,
            crate::layouts::SavedPane {
                tabs: vec![SavedTab::Chat { chat_id: id(99) }],
                ..Default::default()
            },
        );
        LayoutStore::open(&f.home)
            .unwrap()
            .save(&id(1), &layout)
            .unwrap();
        assert!(f.store.list().unwrap().is_empty());
        let mut dead = session(2);
        dead.kind = Kind::Shell;
        dead.key = format!("shell:{}", id(2));
        dead.status = Status::Stopped;
        assert!(f.store.reconcile(&[dead]).unwrap().is_empty());
    }
    #[test]
    fn stopped_history_and_tombstones_are_bounded() {
        let f = Fixture::new();
        let sessions = (2..240)
            .map(|n| {
                let mut s = session(n);
                s.status = Status::Stopped;
                s.created = n as u64;
                s
            })
            .collect::<Vec<_>>();
        assert_eq!(
            f.store.reconcile_inventory(&sessions).unwrap().len(),
            sessions.len()
        );
        for session in &sessions {
            f.store
                .update(&Update::Hide {
                    key: session.key.clone(),
                })
                .unwrap();
        }
        assert_eq!(f.store.reconcile_inventory(&sessions).unwrap().len(), 200);
        assert_eq!(f.store.reconcile_inventory(&sessions).unwrap().len(), 200);
        f.store
            .transact(|state| {
                state
                    .deleted
                    .extend((0..1100).map(|n| format!("chat:{}", uuid::Uuid::from_u128(n))));
                Ok(())
            })
            .unwrap();
        f.store.forget(&session(2).key).unwrap();
        f.store
            .transact(|state| {
                assert_eq!(state.deleted.len(), 1024);
                Ok(())
            })
            .unwrap();
    }
    #[test]
    fn dead_shell_parents_are_pruned_and_children_promoted() {
        let f = Fixture::new();
        let mut parent = session(2);
        parent.kind = Kind::Shell;
        parent.key = format!("shell:{}", id(2));
        let mut child = session(3);
        child.parent_id = Some(id(2));
        f.store.reconcile(&[parent.clone(), child.clone()]).unwrap();
        parent.status = Status::Stopped;
        let entries = f
            .store
            .reconcile_inventory(&[parent, child.clone()])
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, child.key);
        assert!(entries[0].parent.is_none());
        // A dormant chat is still a valid parent, unlike an exited shell.
        let mut parent = session(2);
        parent.status = Status::Stopped;
        let entries = f.store.reconcile(&[parent, child.clone()]).unwrap();
        assert!(
            entries
                .iter()
                .find(|e| e.key == child.key)
                .unwrap()
                .parent
                .is_some()
        );
        assert_eq!(
            f.store.update(&Update::Pin { key: child.key }).unwrap_err(),
            "only root tabs can be pinned"
        );
    }
    #[test]
    fn hidden_parent_with_children_is_not_retired_by_history_cap() {
        let f = Fixture::new();
        let mut sessions = (2..240)
            .map(|n| {
                let mut s = session(n);
                s.status = Status::Stopped;
                s
            })
            .collect::<Vec<_>>();
        let mut child = session(241);
        child.parent_id = Some(id(2));
        sessions.push(child.clone());
        f.store.reconcile(&sessions).unwrap();
        for s in sessions.iter().filter(|s| s.status == Status::Stopped) {
            f.store
                .update(&Update::Hide { key: s.key.clone() })
                .unwrap();
        }
        let entries = f.store.reconcile_inventory(&sessions).unwrap();
        assert_eq!(entries.len(), 202); // 200 hidden leaves, hidden parent, live child.
        assert!(
            entries
                .iter()
                .find(|e| e.key == session(2).key)
                .unwrap()
                .hidden
        );
        assert_eq!(
            entries.iter().find(|e| e.key == child.key).unwrap().parent,
            Some(session(2).key)
        );
    }
    #[test]
    fn unreadable_metadata_never_prunes_the_last_good_chat_list() {
        let f = Fixture::new();
        let s = session(2);
        f.store.reconcile(&[s.clone()]).unwrap();
        fs::write(f.home.join("chats"), "not a directory").unwrap();
        assert!(crate::chat::log::read_infos_checked(&f.home).is_err());
        assert_eq!(f.store.reconcile_inventory(&[]).unwrap()[0].key, s.key);
    }
    #[test]
    fn shell_title_follows_current_worktree_outside_and_program() {
        let project = id(1);
        let state: crate::store::State = serde_json::from_value(serde_json::json!({"projects":[],"worktrees":[{"id":id(2),"project_id":project,"path":"/project","branch":"main","is_primary":true,"created_at":1},{"id":id(3),"project_id":project,"path":"/project/child","branch":"feature","is_primary":false,"created_at":1}]})).unwrap();
        let shell: crate::sessions::ShellSession = serde_json::from_value(serde_json::json!({"id":id(4),"project_id":project,"kind":"project","cwd":"/project","harness":"codex","alive":true,"created_at_unix":1})).unwrap();
        assert_eq!(
            shell_base_title(
                &shell,
                &state,
                Some(std::path::Path::new("/project/child/sub"))
            ),
            "Codex · feature"
        );
        assert_eq!(
            shell_base_title(&shell, &state, Some(std::path::Path::new("/elsewhere"))),
            "Codex · outside"
        );
        let mut shell = shell;
        shell.harness = Some(crate::sessions::HarnessKind::Claude);
        assert_eq!(shell_base_title(&shell, &state, None), "Claude · main");
    }
    #[test]
    fn concurrent_operations_merge_under_lock() {
        let f = Fixture::new();
        let sessions = (2..18).map(session).collect::<Vec<_>>();
        f.store.reconcile(&sessions).unwrap();
        let threads = sessions
            .iter()
            .map(|s| {
                let store = f.store.clone();
                let key = s.key.clone();
                std::thread::spawn(move || store.update(&Update::Hide { key }).unwrap())
            })
            .collect::<Vec<_>>();
        for t in threads {
            t.join().unwrap();
        }
        assert!(f.store.list().unwrap().iter().all(|e| e.hidden));
    }
    #[test]
    fn corruption_is_preserved_and_bad_ids_cannot_escape_home() {
        let f = Fixture::new();
        assert!(TabStore::at(&f.home, "../x").is_err());
        f.store.list().unwrap();
        let path = f.home.join("project-tabs").join(format!("{}.json", id(1)));
        fs::write(&path, b"broken").unwrap();
        assert!(f.store.list().is_err());
        assert_eq!(fs::read(path).unwrap(), b"broken");
    }
    #[test]
    fn revisions_advance_only_for_changes_and_snapshots_are_atomic() {
        let f = Fixture::new();
        let first = f.store.snapshot().unwrap();
        assert_eq!(f.store.snapshot().unwrap().revision, first.revision);
        f.store.reconcile(&[session(2)]).unwrap();
        let added = f.store.snapshot().unwrap();
        assert!(added.revision > first.revision);
        f.store
            .update(&Update::Hide {
                key: session(2).key,
            })
            .unwrap();
        let hidden = f.store.snapshot().unwrap();
        assert!(hidden.revision > added.revision);
        assert!(hidden.entries[0].hidden);
        f.store
            .update(&Update::Hide {
                key: session(2).key,
            })
            .unwrap();
        assert_eq!(f.store.snapshot().unwrap().revision, hidden.revision);
    }
    #[test]
    fn nested_wire_contains_each_identity_once() {
        let f = Fixture::new();
        let sessions = (2..80)
            .map(|n| {
                let mut s = session(n);
                if n > 2 {
                    s.parent_id = Some(id(n - 1));
                }
                s
            })
            .collect::<Vec<_>>();
        let value = wire(&f.store.reconcile(&sessions).unwrap());
        let mut stack = value["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| (e, 0))
            .collect::<Vec<_>>();
        let mut keys = HashSet::new();
        while let Some((entry, depth)) = stack.pop() {
            assert!(depth <= 8);
            assert!(keys.insert(entry["key"].as_str().unwrap()));
            stack.extend(
                entry["children"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|e| (e, depth + 1)),
            );
        }
        assert_eq!(keys.len(), sessions.len());
        assert!(value["entries"].as_array().unwrap().len() > 1);
        let encoded = serde_json::to_vec(&value).unwrap();
        assert!(serde_json::from_slice::<serde_json::Value>(&encoded).is_ok());
    }
    #[test]
    fn wire_excludes_persistence_fields() {
        let f = Fixture::new();
        let entries = f.store.reconcile(&[session(2)]).unwrap();
        let wire = wire(&entries);
        assert!(wire["entries"][0].get("base_title").is_none());
        assert!(wire["entries"][0].get("rename").is_none());
    }
}

pub fn wire_snapshot(snapshot: &Snapshot) -> serde_json::Value {
    let mut reply = wire(&snapshot.entries);
    reply["revision"] = serde_json::json!(snapshot.revision);
    reply
}
