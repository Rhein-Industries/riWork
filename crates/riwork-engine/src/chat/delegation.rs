//! Durable assignments to native child chats. The existing host owns provider
//! execution and worker cards; this service owns task identity and outcomes.
//! Retrying an assignment observes the original task, never creates/sends again.

use super::client::{Client, socket_path};
use super::model::{
    ApprovalMode, ChatCommand, ChatEvent, ChatInfo, ChatState, ContributorMode, ItemBody, NewChat,
    Provider, TurnOutcome,
};
use super::receipts::{self, CommandStatus};
use super::wire::{CommandGuard, Envelope};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const DIRECTORY: &str = "delegated-tasks";
const MAX_PROMPT: usize = 1 << 20;
const MAX_RECORD: u64 = 4 << 20;
const MAX_EVENT: u64 = 16 << 20;
const MAX_RESULT: usize = 64 << 10;
pub const MAX_WAIT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DelegateRequest {
    pub request_id: String,
    pub prompt: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub provider: Option<Provider>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub approval_mode: Option<ApprovalMode>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Running,
    Waiting,
    Completed,
    Interrupted,
    Failed,
    Cancelling,
    Cancelled,
    Uncertain,
}

impl TaskStatus {
    pub fn terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Interrupted | Self::Failed | Self::Cancelled
        )
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskResult {
    pub turn_id: String,
    pub completion_seq: u64,
    pub outcome: TurnOutcome,
    pub text: String,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DelegatedTask {
    pub task_id: String,
    pub request_id: String,
    pub parent_chat_id: String,
    pub parent_turn_id: String,
    pub child_chat_id: String,
    pub assignment: String,
    pub title: Option<String>,
    pub provider: Provider,
    pub approval_mode: ApprovalMode,
    pub status: TaskStatus,
    pub result: Option<TaskResult>,
    pub error: Option<String>,
    pub created_at_unix: u64,
    pub updated_at_unix: u64,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct Stored {
    version: u32,
    task: DelegatedTask,
    request: DelegateRequest,
    cancel_requested: bool,
    cursor: u64,
    last_seq: u64,
    turn_id: Option<String>,
    output: String,
    output_truncated: bool,
}

impl Stored {
    fn finished(&self) -> bool {
        self.task.status.terminal() || self.task.result.is_some()
    }

    // Called while the task record is locked. A provider can finish and another
    // reader can persist its result before the original Send acknowledgment.
    fn settle_dispatch(&mut self, receipt: &receipts::CommandReceipt) {
        if self.finished() || self.cancel_requested {
            return;
        }
        self.task.status = match receipt.status {
            CommandStatus::Accepted => TaskStatus::Running,
            CommandStatus::Rejected => TaskStatus::Failed,
            CommandStatus::Uncertain => TaskStatus::Uncertain,
        };
        self.task.error = receipt.error.clone();
    }

    fn request_cancel(&mut self) -> bool {
        if self.finished() {
            return false;
        }
        self.cancel_requested = true;
        self.task.status = TaskStatus::Cancelling;
        true
    }

    fn record_error(&mut self, error: String) {
        if !self.finished() {
            self.task.error = Some(error);
        }
    }
}

/// `caller` is the native RiWork chat from the invocation context. MCP obtains
/// it from RIWORK_CHAT_ID/RIWORK_SHELL_ID, never a model-supplied parent selector.
pub struct DelegationService {
    home: PathBuf,
    caller: String,
}

impl DelegationService {
    pub fn at(home: impl Into<PathBuf>, caller: &str) -> Result<Self, String> {
        canonical_id(caller)?;
        Ok(Self {
            home: home.into(),
            caller: caller.into(),
        })
    }

    pub fn open_default() -> Result<Self, String> {
        let caller = crate::project_tabs::caller_parent()
            .ok_or("Delegation requires a native parent chat invocation context")?;
        Self::at(crate::paths::riwork_home()?, &caller)
    }

    fn client(&self) -> Result<Client, String> {
        // No ensure(): querying or cancelling work must not launch a host.
        Client::connect(&socket_path(&self.home))
    }

    fn live_parent(&self, expected_turn: Option<&str>) -> Result<(ChatInfo, String), String> {
        let parent = self
            .client()?
            .list()?
            .into_iter()
            .find(|chat| chat.id == self.caller)
            .ok_or("The parent is not a native chat owned by the running host")?;
        if !matches!(parent.state, ChatState::Running | ChatState::Waiting) {
            return Err("The parent chat has no active turn; no child was started".into());
        }
        let turn = super::log::read_open_turn(&self.home, &self.caller)?
            .ok_or("The parent turn identity is unavailable; no child was started")?;
        if expected_turn.is_some_and(|expected| expected != turn) {
            return Err("The parent turn changed before task dispatch".into());
        }
        Ok((parent, turn))
    }

    fn check_parent(
        &self,
        original: &ChatInfo,
        turn: &str,
        permission: ApprovalMode,
    ) -> Result<(), String> {
        let (current, _) = self.live_parent(Some(turn))?;
        if current.project_id != original.project_id
            || current.worktree_id != original.worktree_id
            || current.cwd != original.cwd
        {
            return Err("The parent's workspace changed before task dispatch".into());
        }
        if permission_rank(permission) > permission_rank(current.approval_mode) {
            return Err("The parent's permission ceiling changed before task dispatch".into());
        }
        Ok(())
    }

    fn stop_unassigned(&self, child_id: &str, task_id: &str) {
        // Guarded even for cleanup: a user's intervening input is never stopped.
        let _ = self.client().and_then(|mut client| {
            client
                .command_identified_guarded(
                    child_id,
                    &format!("delegation:{task_id}:unassigned-stop"),
                    ChatCommand::Stop,
                    CommandGuard::DelegatedCancel {
                        parent_chat_id: self.caller.clone(),
                        send_operation_id: send_operation(task_id),
                        turn_id: None,
                        allow_unstarted: true,
                    },
                )
                .map_err(|error| error.to_string())
        });
    }

    pub fn delegate(
        &self,
        request: DelegateRequest,
        wait: Duration,
    ) -> Result<DelegatedTask, String> {
        validate_request(&request)?;
        let id = task_id(&self.caller, &request.request_id);
        // Retries remain observations even after the parent stops or moves on.
        if let Some(existing) = read(&self.home, &id)? {
            self.owned(&existing)?;
            if existing.request != request {
                return Err(
                    "DELEGATION_REQUEST_CONFLICT: request_id belongs to another assignment".into(),
                );
            }
            return self.wait(&id, wait);
        }
        let (parent, parent_turn) = self.live_parent(None)?;
        let permission = request.approval_mode.unwrap_or(parent.approval_mode);
        if permission_rank(permission) > permission_rank(parent.approval_mode) {
            return Err("A delegated child cannot have broader permissions than its parent".into());
        }
        let provider = request.provider.unwrap_or(parent.provider);
        let same_provider = provider == parent.provider;
        let child_id = Uuid::new_v4().to_string();
        let operation_id = send_operation(&id);
        let mut client = self.client()?;
        if !client
            .supports_identified_create(Duration::from_secs(5))
            .map_err(|error| error.to_string())?
        {
            return Err("The running chat host does not support identified creation".into());
        }
        client
            .command_receipt(&child_id, &operation_id)
            .map_err(|error| error.to_string())?;
        let now = now();
        let stored = Stored {
            version: 1,
            task: DelegatedTask {
                task_id: id.clone(),
                request_id: request.request_id.clone(),
                parent_chat_id: self.caller.clone(),
                parent_turn_id: parent_turn.clone(),
                child_chat_id: child_id.clone(),
                assignment: request.prompt.clone(),
                title: request.title.clone(),
                provider,
                approval_mode: permission,
                status: TaskStatus::Uncertain,
                result: None,
                error: Some("Assignment reserved; dispatch is not yet confirmed".into()),
                created_at_unix: now,
                updated_at_unix: now,
            },
            request: request.clone(),
            cancel_requested: false,
            cursor: 0,
            last_seq: 0,
            turn_id: None,
            output: String::new(),
            output_truncated: false,
        };
        {
            let _lock = acquire(&self.home, &id)?;
            if let Some(existing) = read(&self.home, &id)? {
                if existing.request != request {
                    return Err(
                        "DELEGATION_REQUEST_CONFLICT: request_id belongs to another assignment"
                            .into(),
                    );
                }
                drop(_lock);
                return self.wait(&id, wait);
            }
            write(&self.home, &stored)?;
        }
        // From reservation onward, every failure returns the durable task. A
        // retry never repeats a creation or send whose outcome was uncertain.
        let launch = (|| -> Result<(), String> {
            self.check_parent(&parent, &parent_turn, permission)?;
            if self.load(&id)?.cancel_requested {
                return Ok(());
            }
            let child = client
                .create_identified(
                    &child_id,
                    NewChat {
                        parent_id: Some(self.caller.clone()),
                        provider,
                        project_id: parent.project_id.clone(),
                        worktree_id: parent.worktree_id.clone(),
                        cwd: parent.cwd.clone(),
                        title: request.title.clone(),
                        approval_mode: permission,
                        model: request
                            .model
                            .clone()
                            .or_else(|| same_provider.then(|| parent.model.clone()).flatten()),
                        effort: request
                            .effort
                            .clone()
                            .or_else(|| same_provider.then(|| parent.effort.clone()).flatten()),
                        codex_account_id: if same_provider {
                            parent.codex_account_id.clone()
                        } else {
                            None
                        },
                        orchestrator: None,
                        fast: same_provider && parent.fast && provider != Provider::Muse,
                        contributor_mode: ContributorMode::Off,
                        contributor_opt_out: provider == Provider::Muse,
                        claude_billing: if same_provider {
                            parent.claude_billing
                        } else {
                            None
                        },
                    },
                )
                .map_err(|error| error.to_string())?;
            if child.parent_id.as_deref() != Some(self.caller.as_str())
                || child.project_id != parent.project_id
                || child.worktree_id != parent.worktree_id
                || child.cwd != parent.cwd
                || child.provider != provider
                || permission_rank(child.approval_mode) > permission_rank(permission)
            {
                return Err(
                    "The created child did not preserve its delegated scope and permissions".into(),
                );
            }
            if matches!(child.state, ChatState::Stopped | ChatState::Failed { .. }) {
                return Err(
                    "The child provider did not start; delegation will not restart it".into(),
                );
            }
            if let Err(error) = self.check_parent(&parent, &parent_turn, permission) {
                self.stop_unassigned(&child_id, &id);
                self.update(&id, |stored| {
                    if !stored.finished() {
                        stored.task.status = TaskStatus::Failed;
                        stored.task.error = Some(error.clone());
                    }
                    Ok(())
                })?;
                return Err(error);
            }
            if self.load(&id)?.cancel_requested {
                return Ok(());
            }
            let receipt = client
                .command_identified_guarded(
                    &child_id,
                    &operation_id,
                    ChatCommand::Send {
                        text: request.prompt,
                    },
                    CommandGuard::DelegatedSend {
                        parent_chat_id: self.caller.clone(),
                        parent_turn_id: parent_turn.clone(),
                    },
                )
                .map_err(|error| error.to_string())?;
            if receipt.status == CommandStatus::Rejected {
                self.stop_unassigned(&child_id, &id);
            }
            self.update(&id, |stored| {
                stored.settle_dispatch(&receipt);
                Ok(())
            })?;
            Ok(())
        })();
        if let Err(error) = launch {
            self.update(&id, |stored| {
                stored.record_error(error);
                Ok(())
            })?;
        }
        if self.load(&id)?.cancel_requested {
            return self.cancel(&id);
        }
        self.wait(&id, wait)
    }

    /// Observes logs and receipts and saves a terminal result. Never executes a
    /// provider command, even for a task interrupted during creation or send.
    pub fn status(&self, id: &str) -> Result<DelegatedTask, String> {
        let (infos, host_running) = match self.client().and_then(|mut client| client.list()) {
            Ok(infos) => (infos, true),
            Err(_) => (super::log::read_infos_checked(&self.home)?, false),
        };
        self.update(id, |stored| {
            if stored.task.status.terminal() { return Ok(()); }
            let child = infos.iter().find(|info| info.id == stored.task.child_chat_id);
            let receipt = receipts::read(&self.home, &stored.task.child_chat_id, &send_operation(id))?;
            if let Some(receipt) = &receipt {
                if receipt.status == CommandStatus::Rejected {
                    stored.task.status = TaskStatus::Failed;
                    stored.task.error = receipt.error.clone();
                    return Ok(());
                }
            }
            // The child may receive a manual message before guarded assignment
            // admission. Its first turn is ours only after that admission was
            // accepted; an uncertain/rejected Send cannot claim those results.
            if receipt.as_ref().is_some_and(|receipt| receipt.status == CommandStatus::Accepted) {
                if let Err(error) = observe(&self.home, stored) {
                    stored.task.error = Some(error);
                    stored.task.status = TaskStatus::Uncertain;
                    return Ok(());
                }
                if stored.task.result.is_some() { return Ok(()); }
            }
            if stored.cancel_requested {
                stored.task.status = if host_running && child.is_some_and(|child| matches!(child.state, ChatState::Stopped | ChatState::Failed { .. })) { TaskStatus::Cancelled } else { TaskStatus::Cancelling };
            } else if receipt.as_ref().is_some_and(|receipt| receipt.status == CommandStatus::Accepted) {
                stored.task.status = match child.filter(|_| host_running).map(|child| &child.state) {
                    Some(ChatState::Waiting) => TaskStatus::Waiting,
                    Some(ChatState::Starting | ChatState::Running | ChatState::Idle) => TaskStatus::Running,
                    _ => TaskStatus::Uncertain,
                };
                if stored.task.status == TaskStatus::Uncertain {
                    stored.task.error = Some("The provider is unavailable and no task result is recorded; it will not restart automatically".into());
                }
            }
            Ok(())
        }).map(|stored| stored.task)
    }

    pub fn wait(&self, id: &str, timeout: Duration) -> Result<DelegatedTask, String> {
        let end = Instant::now() + timeout.min(MAX_WAIT);
        loop {
            let task = self.status(id)?;
            if task.status.terminal()
                || task.status == TaskStatus::Uncertain
                || Instant::now() >= end
            {
                return Ok(task);
            }
            std::thread::sleep(
                Duration::from_millis(50).min(end.saturating_duration_since(Instant::now())),
            );
        }
    }

    /// Cancellation is explicit, durable, and idempotent. If the host is down,
    /// report uncertainty; this call never restarts it just to stop a child.
    pub fn cancel(&self, id: &str) -> Result<DelegatedTask, String> {
        let task = self.status(id)?;
        if task.status.terminal() {
            return Ok(task);
        }
        let stored = self.update(id, |stored| {
            stored.request_cancel();
            Ok(())
        })?;
        // status() above is a snapshot; a concurrent observer may have saved
        // completion before this locked update acquired the task record.
        if stored.finished() {
            return Ok(stored.task);
        }
        let cancelled = (|| -> Result<(), String> {
            let mut client = self.client()?;
            let child = client
                .list()?
                .into_iter()
                .find(|child| child.id == stored.task.child_chat_id);
            let Some(child) = child else {
                return Err(
                    "Child creation is unconfirmed; cancellation is recorded but not yet confirmed"
                        .into(),
                );
            };
            if child.parent_id.as_deref() != Some(self.caller.as_str()) {
                return Err("The child no longer belongs to the calling parent".into());
            }
            let guard = CommandGuard::DelegatedCancel {
                parent_chat_id: self.caller.clone(),
                send_operation_id: send_operation(id),
                turn_id: stored.turn_id.clone(),
                allow_unstarted: receipts::read(
                    &self.home,
                    &stored.task.child_chat_id,
                    &send_operation(id),
                )?
                .is_none(),
            };
            let guard_key = format!(
                "{:x}",
                Sha256::digest(serde_json::to_vec(&guard).map_err(|error| error.to_string())?)
            );
            let receipt = client
                .command_identified_guarded(
                    &child.id,
                    &format!("delegation:{id}:cancel:{guard_key}"),
                    ChatCommand::Stop,
                    guard,
                )
                .map_err(|error| error.to_string())?;
            if receipt.status != CommandStatus::Accepted {
                return Err(receipt
                    .error
                    .unwrap_or_else(|| "Cancellation outcome is uncertain".into()));
            }
            Ok(())
        })();
        if let Err(error) = cancelled {
            self.update(id, |stored| {
                stored.record_error(error);
                Ok(())
            })?;
        }
        self.status(id)
    }

    fn owned(&self, stored: &Stored) -> Result<(), String> {
        if stored.task.parent_chat_id != self.caller {
            return Err("Delegated task belongs to another parent chat".into());
        }
        Ok(())
    }

    fn load(&self, id: &str) -> Result<Stored, String> {
        let stored = read(&self.home, id)?.ok_or("Unknown delegated task")?;
        self.owned(&stored)?;
        Ok(stored)
    }

    fn update(
        &self,
        id: &str,
        change: impl FnOnce(&mut Stored) -> Result<(), String>,
    ) -> Result<Stored, String> {
        let _lock = acquire(&self.home, id)?;
        let mut stored = self.load(id)?;
        let before = stored.clone();
        change(&mut stored)?;
        if stored != before {
            stored.task.updated_at_unix = now();
            write(&self.home, &stored)?;
        }
        Ok(stored)
    }
}

fn permission_rank(permission: ApprovalMode) -> u8 {
    match permission {
        ApprovalMode::Plan => 0,
        ApprovalMode::Supervised => 1,
        ApprovalMode::AutoEdit => 2,
        ApprovalMode::Full => 3,
    }
}

fn validate_request(request: &DelegateRequest) -> Result<(), String> {
    if request.request_id.is_empty()
        || request.request_id.len() > 256
        || request.request_id.chars().any(char::is_control)
    {
        return Err("request_id must contain 1 to 256 bytes without control characters".into());
    }
    if request.prompt.trim().is_empty() || request.prompt.len() > MAX_PROMPT {
        return Err("prompt must contain 1 to 1048576 bytes".into());
    }
    if request
        .title
        .as_ref()
        .is_some_and(|title| title.len() > 512)
    {
        return Err("title must be at most 512 bytes".into());
    }
    if request
        .model
        .as_ref()
        .is_some_and(|model| model.len() > 256)
        || request
            .effort
            .as_ref()
            .is_some_and(|effort| effort.len() > 256)
    {
        return Err("model and effort must be at most 256 bytes".into());
    }
    Ok(())
}

fn task_id(parent: &str, request: &str) -> String {
    let hash = Sha256::digest(format!("riwork-delegation-v1\0{parent}\0{request}").as_bytes());
    let mut bytes: [u8; 16] = hash[..16].try_into().unwrap();
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes).to_string()
}

fn send_operation(id: &str) -> String {
    format!("delegation:{id}:send")
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn canonical_id(id: &str) -> Result<(), String> {
    if Uuid::parse_str(id).is_ok_and(|uuid| uuid.to_string() == id) {
        Ok(())
    } else {
        Err("A canonical UUID is required".into())
    }
}

fn acquire(home: &Path, id: &str) -> Result<File, String> {
    canonical_id(id)?;
    let directory = home.join(DIRECTORY);
    let existed = directory.is_dir();
    crate::paths::create_private_dir(&directory).map_err(|error| error.to_string())?;
    if !existed {
        File::open(home)
            .and_then(|file| file.sync_all())
            .map_err(|error| error.to_string())?;
    }
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(directory.join(format!("{id}.lock")))
        .map_err(|error| error.to_string())?;
    file.lock_exclusive().map_err(|error| error.to_string())?;
    Ok(file)
}

fn read(home: &Path, id: &str) -> Result<Option<Stored>, String> {
    canonical_id(id)?;
    let file = match File::open(home.join(DIRECTORY).join(format!("{id}.json"))) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_RECORD + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_RECORD {
        return Err("Delegated task record is too large".into());
    }
    let stored: Stored = serde_json::from_slice(&bytes)
        .map_err(|error| format!("Cannot read delegated task: {error}"))?;
    if stored.version != 1 || stored.task.task_id != id {
        return Err("Delegated task identity or version is invalid".into());
    }
    Ok(Some(stored))
}

fn write(home: &Path, stored: &Stored) -> Result<(), String> {
    let directory = home.join(DIRECTORY);
    let temporary = directory.join(format!(".{}.tmp", Uuid::new_v4()));
    let bytes = serde_json::to_vec(stored).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_RECORD {
        return Err("Delegated task record is too large".into());
    }
    let result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(
            &temporary,
            directory.join(format!("{}.json", stored.task.task_id)),
        )?;
        File::open(&directory)?.sync_all()
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        return Err(format!("Cannot save delegated task: {error}"));
    }
    Ok(())
}

fn observe(home: &Path, stored: &mut Stored) -> Result<(), String> {
    let dir =
        super::log::chat_dir(home, &stored.task.child_chat_id).ok_or("Invalid child chat ID")?;
    let mut file = match File::open(dir.join("events.jsonl")) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    let length = file.metadata().map_err(|error| error.to_string())?.len();
    if length < stored.cursor {
        return Err("Child event log shrank; task outcome is uncertain".into());
    }
    file.seek(SeekFrom::Start(stored.cursor))
        .map_err(|error| error.to_string())?;
    let mut reader = BufReader::new(file.take(length - stored.cursor));
    let mut bytes = Vec::new();
    let mut processed = 0;
    while processed < MAX_EVENT && stored.task.result.is_none() {
        bytes.clear();
        reader
            .by_ref()
            .take(MAX_EVENT + 1)
            .read_until(b'\n', &mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() as u64 > MAX_EVENT {
            return Err("Child event exceeds the task observer limit".into());
        }
        if bytes.last() != Some(&b'\n') {
            break;
        }
        let envelope: Envelope = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Unreadable child event: {error}"))?;
        if envelope.chat_id != stored.task.child_chat_id || envelope.seq != stored.last_seq + 1 {
            return Err("Child event sequence is invalid".into());
        }
        stored.cursor += bytes.len() as u64;
        stored.last_seq = envelope.seq;
        processed += bytes.len() as u64;
        match envelope.event {
            ChatEvent::TurnStarted { turn_id, .. } if stored.turn_id.is_none() => {
                stored.turn_id = Some(turn_id)
            }
            ChatEvent::ItemCompleted { item }
                if item.turn_id == stored.turn_id && stored.turn_id.is_some() =>
            {
                if let ItemBody::AgentMessage { text } = item.body {
                    if !stored.output.is_empty() {
                        stored.output.push('\n');
                    }
                    stored.output.push_str(&text);
                    if stored.output.len() > MAX_RESULT {
                        let mut end = MAX_RESULT;
                        while !stored.output.is_char_boundary(end) {
                            end -= 1;
                        }
                        stored.output.truncate(end);
                        stored.output_truncated = true;
                    }
                }
            }
            ChatEvent::TurnCompleted {
                turn_id, outcome, ..
            } if stored.turn_id.as_deref() == Some(turn_id.as_str()) => {
                stored.task.status = match &outcome {
                    TurnOutcome::Completed => TaskStatus::Completed,
                    TurnOutcome::Interrupted if stored.cancel_requested => TaskStatus::Cancelled,
                    TurnOutcome::Interrupted => TaskStatus::Interrupted,
                    TurnOutcome::Failed { .. } => TaskStatus::Failed,
                };
                stored.task.error = match &outcome {
                    TurnOutcome::Failed { message } => Some(message.clone()),
                    _ => None,
                };
                stored.task.result = Some(TaskResult {
                    turn_id,
                    completion_seq: envelope.seq,
                    outcome,
                    text: std::mem::take(&mut stored.output),
                    truncated: stored.output_truncated,
                });
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
