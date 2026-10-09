//! Read-only session navigation. Render and selection only inspect the cached snapshot.

use std::{
    collections::HashSet,
    fs::{self, File},
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::Path,
    time::{Duration, Instant},
};

use crate::chat::{
    client::socket_path,
    log::chats_dir,
    model::{ChatInfo, ChatState, Provider},
    wire::{Request, Response},
};

const REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const READ_DEADLINE: Duration = Duration::from_secs(2);
const CATALOG_BYTES: usize = 16 * 1024 * 1024;
const INFO_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Filter {
    #[default]
    All,
    Codex,
    Claude,
    Shells,
}

impl Filter {
    pub const ALL: [Self; 4] = [Self::All, Self::Codex, Self::Claude, Self::Shells];

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All sessions",
            Self::Codex => "Codex chats",
            Self::Claude => "Claude chats",
            Self::Shells => "Shells",
        }
    }

    pub fn accepts_chat(self, provider: Provider) -> bool {
        match self {
            Self::All => true,
            Self::Codex => provider == Provider::Codex,
            Self::Claude => provider == Provider::Claude,
            Self::Shells => false,
        }
    }

    pub fn accepts_shell(self) -> bool {
        matches!(self, Self::All | Self::Shells)
    }
}

pub fn provider_label(provider: Provider) -> &'static str {
    match provider {
        Provider::Codex => "Codex",
        Provider::Claude => "Claude",
    }
}

pub fn status_label(state: &ChatState) -> &'static str {
    match state {
        ChatState::Starting => "Starting",
        ChatState::Idle => "Idle",
        ChatState::Running => "Working",
        ChatState::Waiting => "Waiting",
        ChatState::Stopped => "Stopped",
        ChatState::Failed { .. } => "Failed",
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub project: String,
    pub generation: u64,
}

pub struct Snapshot {
    pub chats: Vec<ChatInfo>,
    pub note: Option<String>,
}

#[derive(Default)]
pub struct Catalog {
    binding: Option<Binding>,
    in_flight: Option<Binding>,
    sampled: Option<Instant>,
    chats: Vec<ChatInfo>,
    note: Option<String>,
}

impl Catalog {
    pub fn bind(&mut self, project: &str, generation: u64) {
        let binding = Binding {
            project: project.into(),
            generation,
        };
        if self.binding.as_ref() != Some(&binding) {
            self.binding = Some(binding);
            self.sampled = None;
            self.chats.clear();
            self.note = None;
            // Keep an obsolete request in flight until it completes: project switches
            // cannot accumulate blocked readers. Its result will not be published.
        }
    }

    pub fn begin(
        &mut self,
        project: &str,
        generation: u64,
        now: Instant,
        force: bool,
    ) -> Option<Binding> {
        self.bind(project, generation);
        if self.in_flight.is_some()
            || (!force
                && self
                    .sampled
                    .is_some_and(|at| now.saturating_duration_since(at) < REFRESH_INTERVAL))
        {
            return None;
        }
        let ticket = self.binding.clone()?;
        self.in_flight = Some(ticket.clone());
        Some(ticket)
    }

    pub fn finish(&mut self, ticket: &Binding, snapshot: Snapshot, now: Instant) -> bool {
        if self.in_flight.as_ref() != Some(ticket) {
            return false;
        }
        self.in_flight = None;
        if self.binding.as_ref() != Some(ticket) {
            return false;
        }
        let mut ids = HashSet::new();
        self.chats = snapshot
            .chats
            .into_iter()
            .filter(|chat| {
                chat.project_id.as_deref() == Some(ticket.project.as_str())
                    && uuid::Uuid::parse_str(&chat.id).is_ok_and(|id| id.to_string() == chat.id)
                    && ids.insert(chat.id.clone())
            })
            .collect();
        self.chats.sort_by(|a, b| {
            b.created_at_unix
                .cmp(&a.created_at_unix)
                .then(a.id.cmp(&b.id))
        });
        self.note = snapshot.note;
        self.sampled = Some(now);
        true
    }

    pub fn chats(&self) -> &[ChatInfo] {
        &self.chats
    }
    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }
    pub fn loading(&self) -> bool {
        self.in_flight.is_some()
    }

    /// Queued UI actions must still belong to the same project and incarnation.
    pub fn selected(&self, project: &str, generation: u64, id: &str) -> Option<ChatInfo> {
        let binding = self.binding.as_ref()?;
        if binding.project != project || binding.generation != generation {
            return None;
        }
        self.chats.iter().find(|chat| chat.id == id).cloned()
    }
}

fn decode_list(bytes: &[u8], request_id: &str) -> Result<Vec<ChatInfo>, String> {
    let response: Response = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    if response.id != request_id {
        return Err("Chat catalog response identity mismatch".into());
    }
    if !response.ok {
        return Err(response
            .error
            .unwrap_or_else(|| "Chat catalog refused".into()));
    }
    serde_json::from_value(response.result.ok_or("Missing chat catalog")?)
        .map_err(|error| error.to_string())
}

fn read_time_left(deadline: Instant, now: Instant) -> Result<Duration, String> {
    deadline
        .checked_duration_since(now)
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| "Chat catalog read timed out".into())
}

fn append_response(response: &mut Vec<u8>, chunk: &[u8], limit: usize) -> Result<bool, String> {
    let end = chunk.iter().position(|byte| *byte == b'\n');
    let length = end.unwrap_or(chunk.len());
    if response.len().saturating_add(length) > limit {
        return Err("Chat catalog exceeds the response read limit".into());
    }
    response.extend_from_slice(&chunk[..length]);
    Ok(end.is_some())
}

/// Only List, never ensure/capabilities/create/command. The absolute deadline also
/// bounds a peer that trickles bytes; response storage is independently bounded.
fn running_chats(home: &Path) -> Result<Vec<ChatInfo>, String> {
    let mut socket = UnixStream::connect(socket_path(home)).map_err(|error| error.to_string())?;
    let deadline = Instant::now() + READ_DEADLINE;
    socket
        .set_write_timeout(Some(READ_DEADLINE))
        .map_err(|error| error.to_string())?;
    let id = uuid::Uuid::new_v4().to_string();
    let mut request =
        serde_json::to_vec(&Request::List { id: id.clone() }).map_err(|error| error.to_string())?;
    request.push(b'\n');
    socket
        .write_all(&request)
        .map_err(|error| error.to_string())?;
    let mut response = Vec::new();
    let mut chunk = [0; 4096];
    loop {
        let remaining = read_time_left(deadline, Instant::now())?;
        socket
            .set_read_timeout(Some(remaining))
            .map_err(|error| error.to_string())?;
        let read = socket.read(&mut chunk).map_err(|error| error.to_string())?;
        if read == 0 {
            return Err("Chat catalog connection closed".into());
        }
        if append_response(&mut response, &chunk[..read], CATALOG_BYTES)? {
            return decode_list(&response, &id);
        }
    }
}

fn decode_saved_info(
    bytes: &[u8],
    directory_id: &str,
    project: &str,
) -> Result<Option<ChatInfo>, String> {
    let mut info: ChatInfo = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    if info.id != directory_id {
        return Err("Saved chat identity mismatch".into());
    }
    if info.project_id.as_deref() != Some(project) {
        return Ok(None);
    }
    if !matches!(info.state, ChatState::Stopped | ChatState::Failed { .. }) {
        info.state = ChatState::Stopped;
    }
    Ok(Some(info))
}

/// Bounded metadata only; no transcript reads, repair, writes or provider processes.
fn saved_chats(home: &Path, project: &str) -> Snapshot {
    let mut chats = Vec::new();
    let mut bytes = 0usize;
    let mut skipped = false;
    match fs::read_dir(chats_dir(home)) {
        Ok(entries) => {
            for entry in entries {
                let Ok(entry) = entry else {
                    skipped = true;
                    continue;
                };
                let name = entry.file_name().to_string_lossy().into_owned();
                if !uuid::Uuid::parse_str(&name).is_ok_and(|id| id.to_string() == name) {
                    continue;
                }
                let mut data = Vec::new();
                let result = File::open(entry.path().join("info.json"))
                    .and_then(|file| file.take((INFO_BYTES + 1) as u64).read_to_end(&mut data));
                if result.is_err() || data.len() > INFO_BYTES {
                    skipped = true;
                    continue;
                }
                let info = match decode_saved_info(&data, &name, project) {
                    Ok(Some(info)) => info,
                    Ok(None) => continue,
                    Err(_) => {
                        skipped = true;
                        continue;
                    }
                };
                bytes = bytes.saturating_add(data.len());
                if bytes > CATALOG_BYTES {
                    skipped = true;
                    break;
                }
                chats.push(info);
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => skipped = true,
    }
    Snapshot {
        chats,
        note: skipped.then(|| {
            "Some saved chat metadata could not be read within the catalog limits.".into()
        }),
    }
}

fn unavailable_note(error: &str, saved_note: Option<&str>) -> String {
    let error: String = error.chars().take(180).collect();
    let detail = saved_note
        .map(|note| format!(" {note}"))
        .unwrap_or_default();
    format!(
        "Live chat catalog unavailable ({error}); showing saved sessions; transcript loads when the backend is available. Selecting a session does not start it.{detail}"
    )
}

/// Background task entry point. A legacy host supports List without negotiation.
pub fn read(home: &Path, project: &str) -> Snapshot {
    match running_chats(home) {
        Ok(chats) => Snapshot { chats, note: None },
        Err(error) => {
            let mut saved = saved_chats(home, project);
            saved.note = Some(unavailable_note(&error, saved.note.as_deref()));
            saved
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::model::ApprovalMode;

    pub fn chat(id: u128, provider: Provider, project: &str, state: ChatState) -> ChatInfo {
        ChatInfo {
            parent_id: None, user_title: None, first_user_message: None, provider_title: None,
            id: uuid::Uuid::from_u128(id).to_string(),
            provider,
            project_id: Some(project.into()),
            worktree_id: Some("tree-a".into()),
            cwd: "/synthetic/worktree".into(),
            title: format!("History {id}"),
            created_at_unix: id as u64,
            provider_thread_id: None,
            model: None,
            effort: None,
            fast: false,
            approval_mode: ApprovalMode::default(),
            codex_account_id: None,
            state,
            orchestrator: None,
            carried_over: None,
        }
    }

    #[test]
    fn full_catalog_retains_closed_stopped_failed_and_both_providers_by_project_uuid() {
        let mut cache = Catalog::default();
        let now = Instant::now();
        let ticket = cache.begin("project-a", 1, now, false).unwrap();
        let claude = chat(1, Provider::Claude, "project-a", ChatState::Stopped);
        let codex = chat(
            2,
            Provider::Codex,
            "project-a",
            ChatState::Failed {
                message: "offline".into(),
            },
        );
        assert!(cache.finish(
            &ticket,
            Snapshot {
                chats: vec![
                    claude.clone(),
                    codex.clone(),
                    claude.clone(),
                    chat(3, Provider::Claude, "other-project", ChatState::Idle)
                ],
                note: None
            },
            now
        ));
        assert_eq!(cache.chats(), &[codex, claude.clone()]);
        assert_eq!(cache.selected("project-a", 1, &claude.id), Some(claude));
        assert!(
            cache
                .selected("other-project", 1, &uuid::Uuid::from_u128(1).to_string())
                .is_none()
        );
        assert!(Filter::Claude.accepts_chat(Provider::Claude));
        assert!(!Filter::Claude.accepts_chat(Provider::Codex));
        assert!(!Filter::Claude.accepts_shell());
        assert!(Filter::All.accepts_shell() && Filter::Shells.accepts_shell());
    }

    #[test]
    fn switches_and_same_project_new_generation_reject_stale_results_and_actions_singleflight() {
        let mut cache = Catalog::default();
        let now = Instant::now();
        let old = cache.begin("a", 1, now, false).unwrap();
        assert!(cache.begin("b", 2, now, true).is_none());
        assert!(cache.begin("a", 3, now, true).is_none());
        assert!(!cache.finish(
            &old,
            Snapshot {
                chats: vec![chat(1, Provider::Claude, "a", ChatState::Stopped)],
                note: None
            },
            now
        ));
        assert!(cache.chats().is_empty());
        let current = cache.begin("a", 3, now, false).unwrap();
        assert!(cache.finish(
            &current,
            Snapshot {
                chats: vec![chat(1, Provider::Claude, "a", ChatState::Stopped)],
                note: None
            },
            now
        ));
        assert!(
            cache
                .selected("a", 1, &uuid::Uuid::from_u128(1).to_string())
                .is_none()
        );
        assert!(cache.begin("a", 3, now, false).is_none());
        assert!(cache.begin("a", 3, now, true).is_some());
    }

    #[test]
    fn saved_history_normalizes_inactive_status_without_losing_failure_or_identity() {
        let live = chat(1, Provider::Claude, "project", ChatState::Running);
        let bytes = serde_json::to_vec(&live).unwrap();
        let saved = decode_saved_info(&bytes, &live.id, "project")
            .unwrap()
            .unwrap();
        assert_eq!(saved.state, ChatState::Stopped);
        assert_eq!(
            (&saved.id, &saved.title, &saved.worktree_id, &saved.cwd),
            (&live.id, &live.title, &live.worktree_id, &live.cwd)
        );
        assert!(
            decode_saved_info(&bytes, &live.id, "other-project")
                .unwrap()
                .is_none()
        );
        assert!(decode_saved_info(&bytes, "other-id", "project").is_err());
        let failed = chat(
            2,
            Provider::Codex,
            "project",
            ChatState::Failed {
                message: "provider exited".into(),
            },
        );
        assert_eq!(
            decode_saved_info(&serde_json::to_vec(&failed).unwrap(), &failed.id, "project")
                .unwrap(),
            Some(failed)
        );
    }

    #[test]
    fn response_storage_and_absolute_deadline_stay_bounded_even_with_partial_reads() {
        let now = Instant::now();
        let deadline = now + READ_DEADLINE;
        let mut response = Vec::new();
        assert!(!append_response(&mut response, b"ab", 4).unwrap());
        assert_eq!(
            read_time_left(deadline, now + Duration::from_secs(1)).unwrap(),
            Duration::from_secs(1)
        );
        assert!(append_response(&mut response, b"cd\nignored", 4).unwrap());
        assert_eq!(response, b"abcd");
        assert!(read_time_left(deadline, deadline).is_err());
        assert!(read_time_left(deadline, deadline + Duration::from_secs(1)).is_err());
        assert!(append_response(&mut response, b"x", 4).is_err());
        assert_eq!(
            response, b"abcd",
            "over-budget input cannot grow retained storage"
        );
    }

    #[test]
    fn list_protocol_is_read_only_and_requires_matching_response_identity() {
        let request = serde_json::to_value(Request::List {
            id: "request".into(),
        })
        .unwrap();
        assert_eq!(request, serde_json::json!({"op":"list", "id":"request"}));
        let response = serde_json::to_vec(&Response {
            id: "request".into(),
            ok: true,
            result: Some(
                serde_json::to_value(vec![chat(1, Provider::Claude, "p", ChatState::Stopped)])
                    .unwrap(),
            ),
            error: None,
        })
        .unwrap();
        assert_eq!(decode_list(&response, "request").unwrap().len(), 1);
        assert!(decode_list(&response, "stale").is_err());
        assert!(
            decode_list(
                b"{\"id\":\"request\",\"ok\":false,\"error\":\"refused\"}",
                "request"
            )
            .is_err()
        );
    }
}
