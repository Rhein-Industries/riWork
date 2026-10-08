//! The Mac-owned project membership API. All writers apply operations to the
//! latest file under a project lock; window layouts never overwrite this state.
use crate::{
    chat::model::{ChatInfo, ChatState, Provider},
    layouts::{LayoutStore, SavedTab},
};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet, VecDeque},
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
    /// A live shell with nothing running in it that RiWork follows: at its prompt, or a
    /// program without activity tracking.
    Idle,
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
    /// Stores written before pins were removed carry a `pinned` field; it is ignored on load
    /// and not written again.
    pub hidden: bool,
    #[serde(default)]
    pub worker: bool,
    #[serde(default)]
    pub legacy_user_opened: bool,
    #[serde(default)]
    pub legacy_dismissed: bool,
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
    Hide {
        key: String,
    },
    Unhide {
        key: String,
    },
    /// Move before a tab of the same pin group, or to its end (before=null).
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
            Self::Hide { key }
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
/// Where a tab's base title comes from, lowest first (`Entry::title_priority`). A title is
/// set once: only a higher source replaces it, and only an explicit name replaces one of
/// its own kind. Stores written before these ranks kept explicit names at 2, which reads
/// as a provider title and gives way to the same name at `USER`.
pub mod title_source {
    /// "Claude chat", "Codex chat", a plain shell's program, a chat's creation title.
    pub const DEFAULT: u8 = 0;
    /// A chat's first message; a shell's program · branch.
    pub const DERIVED: u8 = 1;
    /// The provider's own thread or session title; an editor's file.
    pub const PROVIDER: u8 = 2;
    /// An orchestrator's fixed name.
    pub const FIXED: u8 = 3;
    /// An explicit name.
    pub const USER: u8 = 4;
}
/// Longest auto title from a first message, in characters, ellipsis included.
const MESSAGE_TITLE_MAX: usize = 60;
/// A chat's auto title from its first message: the first sentence, or the first line when
/// it has none, cut at a word boundary with an ellipsis when it runs past 60 characters.
pub fn message_title(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let words = line
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>();
    // The first sentence ends at . ! ? or 。 before a space or the end.
    let chars = words.chars().collect::<Vec<_>>();
    let end = (0..chars.len())
        .find(|&i| {
            matches!(chars[i], '.' | '!' | '?' | '。')
                && chars.get(i + 1).is_none_or(|c| c.is_whitespace())
        })
        .map_or(chars.len(), |i| if chars[i] == '.' { i } else { i + 1 });
    let sentence = &chars[..end];
    if sentence.len() <= MESSAGE_TITLE_MAX {
        return sentence.iter().collect::<String>().trim().to_owned();
    }
    let room = &sentence[..MESSAGE_TITLE_MAX - 1];
    let cut = room
        .iter()
        .rposition(|c| c.is_whitespace())
        .filter(|at| *at > 0)
        .unwrap_or(room.len());
    let mut title = room[..cut].iter().collect::<String>().trim_end().to_owned();
    title.push('…');
    title
}
impl Session {
    pub fn chat(info: &ChatInfo) -> Self {
        use title_source::*;
        let named = |title: &Option<String>| title.clone().filter(|t| !t.trim().is_empty());
        // RiWork names an orchestrator; a name it gave one is not the user's. Any other chat
        // keeps whatever it was explicitly called, "Project orchestrator" included.
        let given = |title: &str| {
            info.orchestrator.is_some() && crate::orchestrators::fixed_title(title).is_some()
        };
        let user = named(&info.user_title).filter(|t| !given(t));
        let fixed = info
            .orchestrator
            .as_ref()
            .map(crate::orchestrators::chat_title);
        let (title_priority, title) = if let Some(title) = user {
            (USER, title)
        } else if let Some(title) = fixed {
            (FIXED, title.to_owned())
        } else if let Some(title) = named(&info.provider_title) {
            (PROVIDER, title)
        } else if let Some(title) = named(&info.first_user_message) {
            (DERIVED, title)
        } else if !matches!(info.title.as_str(), "Codex chat" | "Claude chat" | "")
            && !given(&info.title)
        {
            (DEFAULT, info.title.clone())
        } else {
            (
                DEFAULT,
                match info.provider {
                    Provider::Claude => "Claude chat".into(),
                    Provider::Codex => "Codex chat".into(),
                },
            )
        };
        Self {
            title_priority,
            key: format!("chat:{}", info.id),
            kind: Kind::Chat,
            title,
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
    epoch: String,
    #[serde(default)]
    deletion_order: VecDeque<String>,
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
        if state.epoch.is_empty() {
            state.epoch = uuid::Uuid::new_v4().to_string();
        }
        // Old stores had no ordering metadata. Preserve their keys deterministically;
        // every deletion recorded by this version is ordered by insertion.
        let queued = state.deletion_order.iter().cloned().collect::<HashSet<_>>();
        let mut legacy = state
            .deleted
            .difference(&queued)
            .cloned()
            .collect::<Vec<_>>();
        legacy.sort();
        state.deletion_order.extend(legacy);
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
                    seed(&mut state, key.clone(), true);
                    if let Some(entry) = state.entries.iter_mut().find(|entry| entry.key == key && entry.hidden)
                    {
                        entry.legacy_dismissed = true;
                    }
                }
            }
        }
        // History outside the saved panes starts hidden on the first migration.
        let mut remaining = known.into_iter().collect::<Vec<_>>();
        remaining.sort();
        for key in remaining {
            if !state.entries.iter().any(|entry| entry.key == key) {
                seed(&mut state, key, true);
            }
        }
        Ok(state)
    }
    /// Merge metadata without replacing membership or user operations. A partial or
    /// stale snapshot cannot delete entries; explicit deletion calls `forget`.
    /// Complete host/registry inventory: prune vanished sessions and bound stopped history.
    pub fn reconcile_inventory(&self, sessions: &[Session]) -> Result<Vec<Entry>, String> {
        self.reconcile(sessions)?;
        self.transact(|state| {
            // An unreadable metadata directory is unknown, not an empty inventory.
            if let Ok(mut known) = self.metadata_keys() {
                known.extend(sessions.iter().map(|s| s.key.clone()));
                // A mid-create/unreadable info file is not evidence of deletion.
                if let Ok(dirs) = fs::read_dir(crate::chat::log::chats_dir(&self.home)) {
                    known.extend(
                        dirs.filter_map(Result::ok)
                            .filter(|d| d.path().is_dir())
                            .filter_map(|d| {
                                let key = format!("chat:{}", d.file_name().to_string_lossy());
                                valid_key(&key).ok().map(|_| key)
                            }),
                    );
                }
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
                    e.hidden && stopped.contains(e.key.as_str()) && !parents.contains(&e.key)
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
    /// Creation is explicit user intent, including when the first registration
    /// itself initializes a store and migration seeds the new metadata as hidden.
    pub fn register_created(&self, session: &Session) -> Result<(), String> {
        self.reconcile_with_creation(std::slice::from_ref(session), true)
            .map(|_| ())
    }
    pub fn reconcile(&self, sessions: &[Session]) -> Result<Vec<Entry>, String> {
        self.reconcile_with_creation(sessions, false)
    }
    fn reconcile_with_creation(
        &self,
        sessions: &[Session],
        created: bool,
    ) -> Result<Vec<Entry>, String> {
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
                    if session.orchestrator
                        && (created
                            || (entry.created == 0 && !entry.legacy_dismissed)
                            || (entry.title.is_empty() && !entry.hidden))
                    {
                        entry.hidden = false;
                    }
                    // Set once: only a higher source replaces the title, and an explicit
                    // name a newer one.
                    let explicit = session.title_priority == title_source::USER
                        && session.title != entry.base_title;
                    if session.title_priority > entry.title_priority
                        || explicit
                        || entry.base_title.is_empty()
                    {
                        entry.base_title = session.title.clone();
                        entry.title_priority = session.title_priority;
                    }
                    entry.title = entry
                        .rename
                        .clone()
                        .unwrap_or_else(|| entry.base_title.clone());
                    if created && !session.worker {
                        entry.hidden = false;
                    }
                    if entry.created == 0 && session.parent_id.is_some() {
                        entry.hidden = true;
                    }
                    entry.created = session.created;
                    entry.status = session.status.clone();
                    entry.parent = parent;
                    entry.worker |= session.worker
                        && !(entry.legacy_user_opened && session.parent_id.is_none());
                } else {
                    state.entries.push(Entry {
                        key: session.key.clone(),
                        kind: session.kind.clone(),
                        title: session.title.clone(),
                        base_title: session.title.clone(),
                        title_priority: session.title_priority,
                        status: session.status.clone(),
                        hidden: session.worker
                            || (session.kind == Kind::Shell && session.status == Status::Stopped),
                        worker: session.worker,
                        legacy_user_opened: false,
                        legacy_dismissed: false,
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
                epoch: state.epoch.clone(),
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
            state.deletion_order.retain(|old| old != key);
            state.deletion_order.push_back(key.into());
            for child in &mut state.entries {
                if child.parent.as_deref() == Some(key) {
                    child.parent = None;
                }
            }
            while state.deleted.len() > 1024 {
                if let Some(old) = state.deletion_order.pop_front() {
                    state.deleted.remove(&old);
                } else {
                    break;
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
                Update::Hide { .. } => state.entries[at].hidden = true,
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
                    // Any tab is a place to go: a worker sits beside tabs that are not its
                    // siblings in the one order every strip draws, and keeps its parent
                    // wherever it goes.
                    if let Some(before) = before
                        && !state.entries.iter().any(|e| &e.key == before)
                    {
                        return Err("move target not found".into());
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
    let legacy_user_opened = !hidden && kind == Kind::Shell;
    state.entries.push(Entry {
        key,
        kind,
        title: String::new(),
        base_title: String::new(),
        title_priority: 0,
        status: Status::Stopped,
        hidden,
        worker: false,
        legacy_user_opened,
        legacy_dismissed: false,
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
    }
}
fn normalize(state: &mut State) {
    for (order, e) in state.entries.iter_mut().enumerate() {
        e.order = order;
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
/// A shell's status from whether its tmux session is there and what its agent is doing
/// (`AgentActivity::as_str`, absent for a shell without one): `stopped` without the
/// session, `working` or `waiting` while an agent works or waits on an approval, `done`
/// once an agent's run has ended, `error` for a failed program, and otherwise `idle`.
pub fn shell_status(alive: bool, activity: Option<&str>) -> Status {
    if !alive {
        return Status::Stopped;
    }
    match activity {
        Some("working") => Status::Working,
        Some("waiting") => Status::Waiting,
        Some("done") => Status::Done,
        Some("error" | "failed") => Status::Error,
        // At its prompt, a program RiWork does not follow, or an agent gone back to it.
        _ => Status::Idle,
    }
}
/// A shell's base title from its current directory and program; the store keeps the first
/// one it is given (`title_source`).
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
        let status = shell_status(
            shell.alive,
            entries
                .iter()
                .find(|e| e["id"].as_str() == Some(&shell.id))
                .and_then(|e| e["activity"].as_str()),
        );
        result.push(Session {
            title_priority: if shell.kind == crate::sessions::ShellKind::Orchestrator {
                title_source::FIXED
            } else if shell.editor_path.is_some() {
                title_source::PROVIDER
            } else {
                title_source::DERIVED
            },
            key: format!("shell:{}", shell.id),
            kind: Kind::Shell,
            title: name,
            parent_id: shell.parent_id.clone(),
            worker: shell.is_worker(),
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
    TabStore::at(home, project)?.register_created(&Session {
        title_priority: if shell.kind == crate::sessions::ShellKind::Orchestrator {
            title_source::FIXED
        } else if shell.editor_path.is_some() {
            title_source::PROVIDER
        } else if branch.is_some() {
            title_source::DERIVED
        } else {
            title_source::DEFAULT
        },
        key: format!("shell:{}", shell.id),
        kind: Kind::Shell,
        title: name,
        parent_id: shell.parent_id.clone(),
        worker: shell.is_worker(),
        status: Status::Done,
        orchestrator: shell.kind == crate::sessions::ShellKind::Orchestrator,
        created: shell.created_at_unix,
    })?;
    Ok(())
}
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub epoch: String,
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
        let mut value = serde_json::json!({"key":e.key,"kind":e.kind,"title":e.title,"status":e.status,"hidden":e.hidden,"worker":e.worker,"order":e.order,"parent":e.parent,"child_count":e.child_count});
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
    fn worker_flag_is_present_on_roots_children_and_orphans() {
        let f = Fixture::new();
        let root = session(2);
        let mut child = session(3);
        child.parent_id = Some(id(2));
        child.worker = true;
        let mut harness = session(4);
        harness.worker = true;
        let wire = wire(
            &f.store
                .reconcile(&[root.clone(), child.clone(), harness])
                .unwrap(),
        );
        assert_eq!(wire["entries"][0]["worker"], false);
        assert_eq!(wire["entries"][0]["children"][0]["worker"], true);
        assert_eq!(wire["entries"][1]["worker"], true);
        f.store.forget(&root.key).unwrap();
        let mut orphan = child;
        orphan.parent_id = None;
        orphan.worker = false;
        assert!(
            f.store
                .reconcile(&[orphan])
                .unwrap()
                .iter()
                .find(|e| e.key == session(3).key)
                .unwrap()
                .worker
        );
    }
    #[test]
    fn epoch_is_stable_until_the_store_is_reset_and_deletions_evict_oldest() {
        let f = Fixture::new();
        let old = f.store.snapshot().unwrap();
        assert_eq!(old.epoch, f.store.snapshot().unwrap().epoch);
        fs::remove_file(f.home.join("project-tabs").join(format!("{}.json", id(1)))).unwrap();
        let reset = f.store.snapshot().unwrap();
        assert_ne!(reset.epoch, old.epoch);
        assert_eq!(wire_snapshot(&reset)["epoch"], reset.epoch);
        f.store
            .transact(|state| {
                // Lexicographically largest key is deliberately the oldest.
                let keys = (0..1024)
                    .rev()
                    .map(|n| format!("chat:{}", uuid::Uuid::from_u128(n)))
                    .collect::<Vec<_>>();
                state.deleted.extend(keys.iter().cloned());
                state.deletion_order.extend(keys);
                Ok(())
            })
            .unwrap();
        f.store
            .forget(&format!("chat:{}", uuid::Uuid::from_u128(1024)))
            .unwrap();
        f.store
            .transact(|state| {
                assert!(
                    !state
                        .deleted
                        .contains(&format!("chat:{}", uuid::Uuid::from_u128(1023)))
                );
                assert!(
                    state
                        .deleted
                        .contains(&format!("chat:{}", uuid::Uuid::from_u128(0)))
                );
                Ok(())
            })
            .unwrap();
    }
    #[test]
    fn update_rejects_unknown_fields_on_every_variant() {
        for action in ["hide", "unhide", "move", "rename"] {
            let mut value = serde_json::json!({"action":action,"key":session(2).key});
            if action == "rename" {
                value["title"] = serde_json::json!("title");
            }
            if action == "move" {
                value["before"] = serde_json::Value::Null;
            }
            assert!(serde_json::from_value::<Update>(value.clone()).is_ok());
            value["unexpected"] = serde_json::json!(true);
            assert!(serde_json::from_value::<Update>(value).is_err());
        }
    }
    #[test]
    fn migration_hides_old_history_preserves_legacy_harness_and_skips_bad_chats() {
        use crate::layouts::{Layout, ProjectLayout, SavedPane};
        let f = Fixture::new();
        for n in [2, 3, 5] {
            let dir = f.home.join("chats").join(id(n));
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("info.json"), serde_json::to_vec(&serde_json::json!({"id":id(n),"provider":"codex","project_id":id(1),"cwd":"/tmp","title":"history","created_at_unix":n})).unwrap()).unwrap();
        }
        // Mid-create directory plus malformed metadata must not poison the list.
        fs::create_dir_all(f.home.join("chats").join(id(8))).unwrap();
        let bad = f.home.join("chats").join(id(9));
        fs::create_dir_all(&bad).unwrap();
        fs::write(bad.join("info.json"), "bad").unwrap();
        assert_eq!(
            crate::chat::log::read_infos_checked(&f.home).unwrap().len(),
            3
        );
        fs::write(f.home.join("sessions.json"), serde_json::to_vec(&serde_json::json!({"sessions":[{"id":id(4),"project_id":id(1),"kind":"project","cwd":"/tmp","harness":"codex","created_at_unix":1}]})).unwrap()).unwrap();
        let mut layout: ProjectLayout =
            serde_json::from_value(serde_json::json!({"layout":Layout::Pane(1),"active_pane":1}))
                .unwrap();
        layout.panes.insert(
            1,
            SavedPane {
                tabs: vec![
                    SavedTab::Chat { chat_id: id(2) },
                    SavedTab::Shell { shell_id: id(4) },
                ],
                ..Default::default()
            },
        );
        LayoutStore::open(&f.home)
            .unwrap()
            .save(&id(1), &layout)
            .unwrap();
        let mut harness = session(4);
        harness.kind = Kind::Shell;
        harness.key = format!("shell:{}", id(4));
        harness.worker = true;
        let mut orch = session(5);
        orch.orchestrator = true;
        let sessions = vec![session(2), session(3), harness, orch];
        let list = f.store.reconcile(&sessions).unwrap();
        let orch = list.iter().find(|entry| entry.key == session(5).key).unwrap();
        assert!(!orch.hidden && !orch.worker);
        assert!(
            !list
                .iter()
                .find(|e| e.key == session(2).key)
                .unwrap()
                .hidden
        );
        assert!(
            list.iter()
                .find(|e| e.key == session(3).key)
                .unwrap()
                .hidden
        );
        let legacy = list
            .iter()
            .find(|e| e.key == format!("shell:{}", id(4)))
            .unwrap();
        assert!(!legacy.hidden && !legacy.worker);
        let list = f.store.reconcile(&sessions).unwrap();
        assert!(!list.iter().find(|e| e.key == legacy.key).unwrap().worker);
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
    fn new_sessions_append_and_any_tab_hides() {
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
        assert!(
            f.store
                .update(&Update::Hide {
                    key: session(4).key
                })
                .unwrap()[2]
                .hidden
        );
    }
    #[test]
    fn stale_inventory_keeps_hide_and_rename() {
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
        f.store.update(&Update::Unhide { key: s.key }).unwrap();
        assert!(!other.list().unwrap()[0].hidden);
    }
    #[test]
    fn moves_are_durable_and_need_a_known_target() {
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
        assert_eq!(
            f.store
                .update(&Update::Move {
                    key: session(3).key,
                    before: Some(session(9).key)
                })
                .unwrap_err(),
            "move target not found"
        );
        assert_eq!(f.store.list().unwrap()[1].key, session(2).key);
    }
    #[test]
    fn pins_are_gone_from_old_stores_the_wire_and_updates() {
        let f = Fixture::new();
        let mut orch = session(3);
        orch.orchestrator = true;
        f.store.reconcile(&[session(2), orch.clone()]).unwrap();
        // A store written while pins existed: the orchestrator pinned and sorted first.
        let path = f.home.join("project-tabs").join(format!("{}.json", id(1)));
        let mut old: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let entries = old["entries"].as_array_mut().unwrap();
        entries.reverse();
        for entry in entries.iter_mut() {
            entry["pinned"] = (entry["key"] == orch.key.as_str()).into();
        }
        std::fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        let list = f.store.list().unwrap();
        assert_eq!(list[0].key, orch.key, "the order it had is kept");
        // The orchestrator is an ordinary tab: it moves after others and hides.
        let list = f
            .store
            .update(&Update::Move {
                key: orch.key.clone(),
                before: None,
            })
            .unwrap();
        assert_eq!(list[1].key, orch.key);
        assert!(
            f.store
                .update(&Update::Hide {
                    key: orch.key.clone()
                })
                .unwrap()[1]
                .hidden
        );
        assert!(
            !String::from_utf8(std::fs::read(&path).unwrap())
                .unwrap()
                .contains("pinned")
        );
        assert!(!wire(&list).to_string().contains("pinned"));
        for action in ["pin", "unpin"] {
            let value = serde_json::json!({"action":action,"key":orch.key});
            assert!(serde_json::from_value::<Update>(value).is_err());
        }
    }
    #[test]
    fn a_root_moves_beside_another_tabs_worker_and_the_worker_keeps_its_parent() {
        // A strip draws one order of roots and workers: dropping a root on a worker of
        // another tab, or a worker among roots, is a move like any other.
        let f = Fixture::new();
        let mut worker = session(3);
        worker.parent_id = Some(id(2));
        f.store
            .reconcile(&[session(2), worker.clone(), session(4)])
            .unwrap();
        let list = f
            .store
            .update(&Update::Move {
                key: session(4).key,
                before: Some(worker.key.clone()),
            })
            .unwrap();
        let keys = list.iter().map(|e| e.key.clone()).collect::<Vec<_>>();
        assert_eq!(keys, [session(2).key, session(4).key, worker.key.clone()]);
        let list = f
            .store
            .update(&Update::Move {
                key: worker.key.clone(),
                before: None,
            })
            .unwrap();
        assert_eq!(list[2].key, worker.key);
        assert_eq!(list[2].parent, Some(session(2).key));
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
    fn a_first_message_names_by_its_first_sentence_or_sixty_characters_at_a_word() {
        assert_eq!(message_title("  hello\n world  "), "hello");
        assert_eq!(
            message_title("Fix the build. Then run tests."),
            "Fix the build"
        );
        assert_eq!(
            message_title("Why does it hang? Look at x."),
            "Why does it hang?"
        );
        assert_eq!(message_title("version 1.2 is out"), "version 1.2 is out");
        let long = "Please look into why the tab strip loses its scroll position after a rename";
        let title = message_title(long);
        assert_eq!(
            title,
            "Please look into why the tab strip loses its scroll…"
        );
        assert!(title.chars().count() <= 60);
        let cjk = message_title(&"猫".repeat(80));
        assert_eq!(cjk.chars().count(), 60);
        assert!(cjk.ends_with('…'));
        assert_eq!(message_title("\n\n  "), "");
    }
    fn chat_info(value: serde_json::Value) -> ChatInfo {
        let mut base = serde_json::json!({"id":id(2),"provider":"claude","project_id":id(1),"cwd":"/tmp","title":"Claude chat","created_at_unix":1});
        base.as_object_mut()
            .unwrap()
            .extend(value.as_object().unwrap().clone());
        serde_json::from_value(base).unwrap()
    }
    #[test]
    fn titles_come_from_a_rename_then_the_provider_then_the_first_message_then_the_kind() {
        use title_source::*;
        let title = |v| {
            let s = Session::chat(&chat_info(v));
            (s.title_priority, s.title)
        };
        assert_eq!(
            title(serde_json::json!({})),
            (DEFAULT, "Claude chat".into())
        );
        assert_eq!(
            title(serde_json::json!({"first_user_message":"Fix it"})),
            (DERIVED, "Fix it".into())
        );
        assert_eq!(
            title(serde_json::json!({"first_user_message":"Fix it","provider_title":"Build fix"})),
            (PROVIDER, "Build fix".into())
        );
        assert_eq!(
            title(serde_json::json!({"user_title":"Mine","provider_title":"Build fix"})),
            (USER, "Mine".into())
        );
    }
    #[test]
    fn orchestrators_keep_their_fixed_names_and_only_a_rename_changes_them() {
        use title_source::*;
        let project = serde_json::json!({"orchestrator":{"scope":"project","project_id":id(1)}});
        let with = |extra: serde_json::Value| {
            let mut v = project.clone();
            v.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            let s = Session::chat(&chat_info(v));
            (s.title_priority, s.title)
        };
        // Neither its first message, the provider, nor the label it was created with.
        for extra in [
            serde_json::json!({"first_user_message":"Next thing to do"}),
            serde_json::json!({"provider_title":"Thread name"}),
            serde_json::json!({"user_title":"P·ORCH · PROJECT","title":"P·ORCH · PROJECT"}),
        ] {
            assert_eq!(with(extra), (FIXED, "Project orchestrator".into()));
        }
        assert_eq!(
            with(serde_json::json!({"user_title":"Planner"})),
            (USER, "Planner".into())
        );
        let global = Session::chat(&chat_info(
            serde_json::json!({"orchestrator":{"scope":"global"},"first_user_message":"hi"}),
        ));
        assert_eq!(global.title, "Global orchestrator");
        // An ordinary chat explicitly named like one keeps its name.
        for named in ["Project orchestrator", "P·ORCH · PROJECT"] {
            let chat = Session::chat(&chat_info(
                serde_json::json!({"user_title":named,"title":named,"first_user_message":"hi"}),
            ));
            assert_eq!((chat.title_priority, chat.title.as_str()), (USER, named));
        }
    }
    #[test]
    fn a_title_is_set_once_and_only_a_higher_source_or_a_new_rename_replaces_it() {
        use title_source::*;
        let f = Fixture::new();
        let with = |title: &str, priority: u8| {
            let mut s = session(2);
            s.title = title.into();
            s.title_priority = priority;
            s
        };
        let title = |s: Session| f.store.reconcile(&[s]).unwrap()[0].title.clone();
        assert_eq!(title(with("Claude chat", DEFAULT)), "Claude chat");
        assert_eq!(title(with("First message", DERIVED)), "First message");
        // A restart that saw another "first" message, or the default again, changes nothing.
        assert_eq!(title(with("Next message", DERIVED)), "First message");
        assert_eq!(title(with("Claude chat", DEFAULT)), "First message");
        // The provider's title replaces a first-message title once.
        assert_eq!(title(with("Provider name", PROVIDER)), "Provider name");
        assert_eq!(
            title(with("Another provider name", PROVIDER)),
            "Provider name"
        );
        // An explicit name wins, and a newer explicit name replaces it; nothing else does.
        assert_eq!(title(with("Mine", USER)), "Mine");
        assert_eq!(title(with("Provider name", PROVIDER)), "Mine");
        assert_eq!(title(with("Renamed", USER)), "Renamed");
        // A shell's program · branch is set once too.
        let mut shell = with("Codex", DEFAULT);
        shell.kind = Kind::Shell;
        shell.key = format!("shell:{}", id(3));
        assert_eq!(
            f.store.reconcile(&[shell.clone()]).unwrap()[1].title,
            "Codex"
        );
        shell.title = "Codex · main".into();
        shell.title_priority = DERIVED;
        assert_eq!(
            f.store.reconcile(&[shell.clone()]).unwrap()[1].title,
            "Codex · main"
        );
        shell.title = "Codex · feature".into();
        assert_eq!(
            f.store.reconcile(&[shell]).unwrap()[1].title,
            "Codex · main"
        );
    }
    #[test]
    fn an_orchestrator_named_by_its_first_message_gets_its_fixed_name_back() {
        let f = Fixture::new();
        // As stores were left by the 40-character cut: an orchestrator and a user chat
        // titled by their first messages, and an orchestrator renamed in the strip.
        let mut orch = session(2);
        orch.orchestrator = true;
        orch.title = "Please check why the nightly build fail".into();
        orch.title_priority = title_source::DERIVED;
        let mut chat = session(3);
        chat.title = "Write the release notes for version 2.4".into();
        chat.title_priority = title_source::DERIVED;
        let mut renamed = session(4);
        renamed.orchestrator = true;
        renamed.title = "Some first message".into();
        renamed.title_priority = title_source::DERIVED;
        f.store
            .reconcile(&[orch.clone(), chat.clone(), renamed.clone()])
            .unwrap();
        f.store
            .update(&Update::Rename {
                key: renamed.key.clone(),
                title: "My planner".into(),
            })
            .unwrap();
        // The next inventory reads them as this version does.
        let info = |n: u8, orchestrator: bool, first: &str| {
            let mut v = serde_json::json!({"id":id(n),"first_user_message":first});
            if orchestrator {
                v["orchestrator"] = serde_json::json!({"scope":"project","project_id":id(1)});
            }
            Session::chat(&chat_info(v))
        };
        let list = f
            .store
            .reconcile(&[
                info(2, true, "Please check why the nightly build fail"),
                info(3, false, "Write the release notes for version 2.4"),
                info(4, true, "Some first message"),
            ])
            .unwrap();
        assert_eq!(list[0].title, "Project orchestrator");
        assert_eq!(list[1].title, "Write the release notes for version 2.4");
        assert_eq!(list[2].title, "My planner");
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
        // The saved pane order stands; the orchestrator is visible but not put first.
        assert_eq!(list[0].key, session(3).key);
        assert_eq!(list[1].key, session(2).key);
        assert!(!list[1].hidden);
        assert!(list[2].hidden);
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
    reply["epoch"] = serde_json::json!(snapshot.epoch);
    reply["revision"] = serde_json::json!(snapshot.revision);
    reply
}
