//! The provider-neutral chat vocabulary. Codex and Claude events are translated
//! into these types by their drivers; the chat host stores and replays them;
//! a chat tab folds them with `Transcript` and draws the result.
//!
//! Everything here is serialized on the host's socket and in its event logs, so
//! names are part of the stored format: add variants and optional fields, never
//! rename or repurpose them.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Codex,
    Claude,
}

/// How much the agent may do without asking.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    /// Ask before commands and edits. Codex: `untrusted` with a read-only
    /// sandbox. Claude: `default`.
    #[default]
    Supervised,
    /// Edit the workspace freely, ask for the rest. Codex: `on-request` with
    /// `workspaceWrite`. Claude: `acceptEdits`.
    AutoEdit,
    /// Never ask. Codex: `never` with `dangerFullAccess`. Claude:
    /// `bypassPermissions`. RiWork's "unrestricted" launches map here.
    Full,
    /// Plan first, change nothing. Codex: collaboration mode `plan`. Claude:
    /// permission mode `plan`.
    Plan,
}

/// Which orchestrator a chat is. RiWork has one global orchestrator and one for
/// each project; the host keeps at most one chat per scope, and the CLI, the MCP
/// tools, the scheduler and the apps find an orchestrator in chat mode by this
/// mark. A chat without it is an ordinary chat.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum OrchestratorScope {
    Global,
    Project { project_id: String },
}

impl OrchestratorScope {
    /// The project of a project orchestrator; the global one has none.
    pub fn project_id(&self) -> Option<&str> {
        match self {
            Self::Global => None,
            Self::Project { project_id } => Some(project_id),
        }
    }
}

/// What a chat is doing as a whole.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ChatState {
    /// The provider process is starting or resuming.
    #[default]
    Starting,
    /// Ready for a message.
    Idle,
    /// A turn is in progress.
    Running,
    /// A turn waits for an approval or an answer from the user.
    Waiting,
    /// No provider process; the next message resumes the chat.
    Stopped,
    /// The provider process failed; the next message tries to resume.
    Failed { message: String },
}

/// One chat, as the host knows it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatInfo {
    /// RiWork's own id for the chat (a UUID), stable across resumes.
    pub id: String,
    /// RiWork session UUID of the caller, across chat and shell kinds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// Explicit user name, distinct from provider-generated titles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_title: Option<String>,
    /// First accepted user message, normalized and bounded to 40 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_user_message: Option<String>,
    pub provider: Provider,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<String>,
    pub cwd: PathBuf,
    pub title: String,
    pub created_at_unix: u64,
    /// Codex thread id or Claude session id, once the provider has named one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Whether the user asked for the provider's fast mode (Codex's fast service
    /// tier, Claude's `fastMode`). It is the user's choice, not what the
    /// provider granted: Claude can turn it off for a while on its own, and says
    /// so in a notice.
    #[serde(default)]
    pub fast: bool,
    #[serde(default)]
    pub approval_mode: ApprovalMode,
    /// The Codex account (RiWork's account id) the chat runs under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_account_id: Option<String>,
    #[serde(default)]
    pub state: ChatState,
    /// Set when the chat is the orchestrator of this scope. Chats and logs
    /// written before orchestrators could be chats do not have it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestrator: Option<OrchestratorScope>,
}

/// What a new chat starts with.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NewChat {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    pub provider: Provider,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<String>,
    pub cwd: PathBuf,
    /// Run a Codex chat under this saved account (a RiWork account id, as
    /// `ChatInfo::codex_account_id` keeps it) instead of the project's or the
    /// app's selection. A Claude chat has no such account and is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default)]
    pub approval_mode: ApprovalMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Makes the chat the orchestrator of this scope. The host refuses it when
    /// the scope already has one (`ORCHESTRATOR_EXISTS`), and requires the
    /// chat's project to be the scope's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestrator: Option<OrchestratorScope>,
    /// Start with the provider's fast mode on. A model without one ignores it.
    #[serde(default)]
    pub fast: bool,
}

/// A model the provider offers, as its driver found it out (Codex `model/list`,
/// Claude's `initialize` reply). The chat's `model` is the `id`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelOption {
    /// What `model` takes: Codex's model id, Claude's alias or model name (the
    /// default model's is `default`).
    pub id: String,
    /// The name to show.
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// The reasoning efforts this model takes, in the order to offer them.
    /// Empty when it has none to choose.
    #[serde(default)]
    pub efforts: Vec<String>,
    /// The effort the provider uses when none is chosen.
    #[serde(default)]
    pub default_effort: Option<String>,
    /// Whether the model has a fast mode.
    #[serde(default)]
    pub supports_fast: bool,
    /// Whether the provider uses this model when none is chosen.
    #[serde(default)]
    pub is_default: bool,
}

/// The start of the error `Create` answers when the orchestrator of the scope
/// already has a chat; the id of that chat follows it.
pub const ORCHESTRATOR_EXISTS: &str = "orchestrator_exists:";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    #[default]
    InProgress,
    Completed,
    Failed,
    /// Refused at an approval prompt.
    Declined,
    Interrupted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Add,
    Modify,
    Delete,
    Rename,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub kind: ChangeKind,
    /// A unified diff when the provider gives one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    pub text: String,
    #[serde(default)]
    pub status: StepStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeLevel {
    Info,
    Warning,
    Error,
}

/// One provider quota window, independent of blocking notice banners.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RateWindow {
    pub id: String,
    pub label: String,
    pub used_percent: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<u64>,
    pub warn_at: f64,
}

/// The content of one transcript item.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ItemBody {
    UserMessage {
        text: String,
    },
    /// Markdown.
    AgentMessage {
        text: String,
    },
    Reasoning {
        text: String,
    },
    /// The agent's plan (Codex `turn/plan/updated`, Claude `ExitPlanMode`).
    Plan {
        #[serde(default)]
        explanation: Option<String>,
        steps: Vec<Step>,
    },
    /// A shell command (Codex `commandExecution`, Claude `Bash`).
    Command {
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default)]
        output: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
    },
    /// File edits (Codex `fileChange`, Claude `Edit`/`Write`/`MultiEdit`).
    FileChange {
        changes: Vec<FileChange>,
    },
    /// Any other tool: MCP calls, Claude's Read/Grep/WebFetch/Task, …
    ToolCall {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        server: Option<String>,
        tool: String,
        #[serde(default)]
        input: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
    },
    WebSearch {
        query: String,
    },
    /// Claude `TodoWrite`.
    Todo {
        items: Vec<Step>,
    },
    /// The context was compacted here.
    Compaction,
    /// Something the provider or the driver wants the user to know. Clients show these
    /// in a banner above the composer, not in the transcript (see docs/chat-notices.md).
    Notice {
        level: NoticeLevel,
        text: String,
        /// What the notice is about (`notice_kind`): a newer notice of the same kind
        /// replaces the older one's banner. None for an older log or a one-off.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
        /// Its cause has gone (the retry worked, the limit reset): the banner goes,
        /// the history keeps it.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        resolved: bool,
        /// The user dismissed this occurrence on the host.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        dismissed: bool,
        /// When the limit the notice is about resets, in Unix seconds, if it says.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resets_at: Option<u64>,
    },
}

impl ItemBody {
    /// A notice of `kind`, unresolved, with no reset time.
    pub fn notice(level: NoticeLevel, text: impl Into<String>, kind: Option<&str>) -> Self {
        Self::Notice {
            level,
            text: text.into(),
            kind: kind.map(str::to_owned),
            resolved: false,
            dismissed: false,
            resets_at: None,
        }
    }
}

/// The stable `kind` values of `ItemBody::Notice`; docs/chat-notices.md lists them for
/// the phone. A kind with a `:` suffix (`rate_limit:seven_day`) is one per suffix.
pub mod notice_kind {
    /// `rate_limit:<window>`, the window as the provider names it.
    pub const RATE_LIMIT: &str = "rate_limit";
    pub const API_RETRY: &str = "api_retry";
    pub const RECONNECTING: &str = "reconnecting";
    pub const SILENCE: &str = "silence";
    pub const TURN_FAILED: &str = "turn_failed";
    pub const AUTH_REQUIRED: &str = "auth_required";
    pub const MODEL_FALLBACK: &str = "model_fallback";
    pub const EFFORT_REFUSED: &str = "effort_refused";
    pub const OVERSIZED_LINE: &str = "oversized_line";
    pub const FAST_MODE: &str = "fast_mode";
    pub const SETTING_REFUSED: &str = "setting_refused";
    pub const RESUMED_FRESH: &str = "resumed_fresh";
    pub const UNDELIVERED: &str = "undelivered";
    pub const PROVIDER_ERROR: &str = "provider_error";
    pub const PROVIDER_WARNING: &str = "provider_warning";
    pub const CONFIG_WARNING: &str = "config_warning";
    pub const MCP_ELICITATION: &str = "mcp_elicitation";
    pub const DEPRECATION: &str = "deprecation";

    /// `rate_limit:<window>`.
    pub fn rate_limit(window: &str) -> String {
        format!("{RATE_LIMIT}:{window}")
    }
}

/// Provider intent, distinct from an item's completion (commentary also completes).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessagePhase {
    Commentary,
    Final,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Presentation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<MessagePhase>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ChatImage>,
    /// Owned, bounded submitted snapshots, including text files. Durable history
    /// can distinguish equal-length versions without inlining their payloads.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<super::attachments::Attachment>,
}
impl Presentation {
    pub fn is_empty(&self) -> bool {
        self.phase.is_none() && self.images.is_empty() && self.attachments.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatImage {
    pub label: String,
    pub source: ImageSource,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ImageSource {
    Local { path: String },
    Data { mime: String, base64: String },
    Url { url: String },
    Unavailable { reason: String },
}

/// One entry of the transcript.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Item {
    /// Additive presentation hints; older logs and clients may omit/ignore them.
    #[serde(default, skip_serializing_if = "Presentation::is_empty")]
    pub presentation: Presentation,
    /// Unique within the chat. Drivers use the provider's id when it has one.
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default)]
    pub status: ItemStatus,
    pub body: ItemBody,
}

/// Appended to an item while it streams.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "text", rename_all = "snake_case")]
pub enum Delta {
    /// More of an agent message's or reasoning item's text.
    Text(String),
    /// More of a command's output.
    Output(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Accept,
    /// Accept this and the same kind of request for the rest of the session.
    AcceptForSession,
    Decline,
    /// Decline and stop the turn.
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalKind {
    Command,
    FileChange,
    Permissions,
    /// Claude asks before a tool by name.
    Tool,
}

/// A turn waits for the user to allow something.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Approval {
    pub request_id: String,
    /// The transcript item the request is about, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
    pub kind: ApprovalKind,
    /// One line: the command, the file, the tool.
    pub title: String,
    /// More detail: the reason, the input, a diff.
    #[serde(default)]
    pub detail: String,
    /// The decisions the provider offers, in its order.
    pub choices: Vec<Decision>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionPrompt {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    pub question: String,
    #[serde(default)]
    pub options: Vec<QuestionOption>,
    #[serde(default)]
    pub multi_select: bool,
}

/// A turn waits for the user to answer questions (Claude `AskUserQuestion`,
/// Codex `item/tool/requestUserInput`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    pub request_id: String,
    pub questions: Vec<QuestionPrompt>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cached_input_tokens: u64,
    /// The model's context window, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// Tokens of the window in use now, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_used: Option<u64>,
    /// The provider's own estimate (Claude's `total_cost_usd`), never a bill.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum TurnOutcome {
    Completed,
    Interrupted,
    Failed { message: String },
}

/// Everything that happens in a chat, in order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ChatEvent {
    /// The chat's metadata changed (thread id learned, model or mode changed).
    Info {
        info: ChatInfo,
    },
    State {
        state: ChatState,
    },
    TurnStarted {
        turn_id: String,
    },
    TurnCompleted {
        turn_id: String,
        outcome: TurnOutcome,
    },
    ItemStarted {
        item: Item,
    },
    ItemDelta {
        item_id: String,
        delta: Delta,
    },
    /// The item's final form; replaces what deltas built.
    ItemCompleted {
        item: Item,
    },
    ApprovalRequested {
        approval: Approval,
    },
    ApprovalResolved {
        request_id: String,
        decision: Decision,
    },
    QuestionRequested {
        question: Question,
    },
    QuestionResolved {
        request_id: String,
    },
    Usage {
        usage: Usage,
    },
    /// Current quota windows; replaces the previous snapshot wholesale.
    RateLimits {
        windows: Vec<RateWindow>,
    },
    /// The models the provider offers. A driver sends it once after its
    /// handshake and again if the list changes; each replaces the last.
    Models {
        models: Vec<ModelOption>,
    },
}

/// What the user asks of a chat.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum ChatCommand {
    /// Send a message: start a turn, or steer the running one where the
    /// provider allows it.
    Send {
        text: String,
    },
    /// A distinct command: legacy hosts must refuse rather than discard attachments.
    SendAttachments {
        text: String,
        attachments: Vec<super::attachments::Attachment>,
    },
    /// Persist dismissal of a sticky notice; the host resolves its occurrence key.
    DismissNotice {
        item_id: String,
    },
    Interrupt,
    Approve {
        request_id: String,
        decision: Decision,
    },
    /// One entry per question, in order: the chosen labels, or free text.
    Answer {
        request_id: String,
        answers: Vec<Vec<String>>,
    },
    Configure {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effort: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        approval_mode: Option<ApprovalMode>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fast: Option<bool>,
    },
    Compact,
    /// Stop the provider process; the chat resumes with the next message.
    Stop,
}

/// A chat's transcript as events build it: what a tab draws, and what the
/// host replays to a tab that connects late.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Transcript {
    pub info: Option<ChatInfo>,
    pub state: ChatState,
    pub items: Vec<Item>,
    /// Requests still waiting for the user, in arrival order.
    pub approvals: Vec<Approval>,
    pub questions: Vec<Question>,
    pub usage: Option<Usage>,
    pub rate_limits: Vec<RateWindow>,
    /// The models the provider offers, empty until its driver has said (an
    /// older driver never does).
    pub models: Vec<ModelOption>,
    pub turn_id: Option<String>,
    index: HashMap<String, usize>,
}

impl Transcript {
    pub fn apply(&mut self, event: &ChatEvent) {
        match event {
            ChatEvent::Info { info } => {
                self.state = info.state.clone();
                self.info = Some(info.clone());
            }
            ChatEvent::State { state } => {
                self.state = state.clone();
                if let Some(info) = &mut self.info {
                    info.state = state.clone();
                }
            }
            ChatEvent::TurnStarted { turn_id } => self.turn_id = Some(turn_id.clone()),
            ChatEvent::TurnCompleted { turn_id, outcome } => {
                if self.turn_id.as_ref() == Some(turn_id) {
                    self.turn_id = None;
                }
                // Whatever the provider left open in this turn is over now.
                let closed = match outcome {
                    TurnOutcome::Completed => ItemStatus::Completed,
                    TurnOutcome::Interrupted => ItemStatus::Interrupted,
                    TurnOutcome::Failed { .. } => ItemStatus::Failed,
                };
                for item in &mut self.items {
                    if item.status == ItemStatus::InProgress
                        && item.turn_id.as_ref().is_none_or(|id| id == turn_id)
                    {
                        item.status = closed;
                    }
                }
                self.approvals.clear();
                self.questions.clear();
            }
            ChatEvent::ItemStarted { item } | ChatEvent::ItemCompleted { item } => {
                match self.index.get(&item.id) {
                    Some(&at) => self.items[at] = item.clone(),
                    None => {
                        self.index.insert(item.id.clone(), self.items.len());
                        self.items.push(item.clone());
                    }
                }
            }
            ChatEvent::ItemDelta { item_id, delta } => {
                let Some(&at) = self.index.get(item_id) else {
                    return;
                };
                match (&mut self.items[at].body, delta) {
                    (ItemBody::AgentMessage { text }, Delta::Text(more))
                    | (ItemBody::Reasoning { text }, Delta::Text(more)) => text.push_str(more),
                    (ItemBody::Command { output, .. }, Delta::Output(more)) => {
                        output.push_str(more)
                    }
                    _ => {}
                }
            }
            ChatEvent::ApprovalRequested { approval } => {
                self.approvals
                    .retain(|a| a.request_id != approval.request_id);
                self.approvals.push(approval.clone());
            }
            ChatEvent::ApprovalResolved { request_id, .. } => {
                self.approvals.retain(|a| &a.request_id != request_id)
            }
            ChatEvent::QuestionRequested { question } => {
                self.questions
                    .retain(|q| q.request_id != question.request_id);
                self.questions.push(question.clone());
            }
            ChatEvent::QuestionResolved { request_id } => {
                self.questions.retain(|q| &q.request_id != request_id)
            }
            ChatEvent::Usage { usage } => self.usage = Some(usage.clone()),
            ChatEvent::Models { models } => self.models = models.clone(),
            ChatEvent::RateLimits { windows } => self.rate_limits = windows.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str, text: &str, status: ItemStatus) -> Item {
        Item {
            presentation: Default::default(),
            id: id.into(),
            turn_id: Some("t1".into()),
            status,
            body: ItemBody::AgentMessage { text: text.into() },
        }
    }

    #[test]
    fn deltas_build_an_item_and_its_completion_replaces_it() {
        let mut t = Transcript::default();
        t.apply(&ChatEvent::TurnStarted {
            turn_id: "t1".into(),
        });
        t.apply(&ChatEvent::ItemStarted {
            item: agent("a", "", ItemStatus::InProgress),
        });
        for part in ["Hel", "lo"] {
            t.apply(&ChatEvent::ItemDelta {
                item_id: "a".into(),
                delta: Delta::Text(part.into()),
            });
        }
        assert_eq!(
            t.items[0].body,
            ItemBody::AgentMessage {
                text: "Hello".into()
            }
        );
        t.apply(&ChatEvent::ItemCompleted {
            item: agent("a", "Hello!", ItemStatus::Completed),
        });
        assert_eq!(t.items.len(), 1);
        assert_eq!(t.items[0], agent("a", "Hello!", ItemStatus::Completed));
    }

    #[test]
    fn a_finished_turn_closes_what_it_left_open_and_its_requests() {
        let mut t = Transcript::default();
        t.apply(&ChatEvent::TurnStarted {
            turn_id: "t1".into(),
        });
        t.apply(&ChatEvent::ItemStarted {
            item: agent("a", "…", ItemStatus::InProgress),
        });
        t.apply(&ChatEvent::ApprovalRequested {
            approval: Approval {
                request_id: "r1".into(),
                item_id: None,
                kind: ApprovalKind::Command,
                title: "rm -rf build".into(),
                detail: String::new(),
                choices: vec![Decision::Accept, Decision::Decline],
            },
        });
        assert_eq!(t.approvals.len(), 1);
        t.apply(&ChatEvent::TurnCompleted {
            turn_id: "t1".into(),
            outcome: TurnOutcome::Interrupted,
        });
        assert_eq!(t.items[0].status, ItemStatus::Interrupted);
        assert!(t.approvals.is_empty() && t.turn_id.is_none());
    }

    #[test]
    fn a_chat_written_before_orchestrators_could_be_chats_still_loads() {
        // `info.json` and an `Info` event as an earlier build wrote them.
        let old = r#"{"id":"c1","provider":"codex","cwd":"/work","title":"Codex chat",
            "created_at_unix":5,"approval_mode":"full","state":{"state":"idle"}}"#;
        let info: ChatInfo = serde_json::from_str(old).unwrap();
        assert_eq!(info.orchestrator, None);
        assert_eq!(info.approval_mode, ApprovalMode::Full);
        let event: ChatEvent =
            serde_json::from_str(&format!(r#"{{"event":"info","info":{old}}}"#)).unwrap();
        assert!(matches!(event, ChatEvent::Info { info } if info.orchestrator.is_none()));
        let new: NewChat = serde_json::from_str(r#"{"provider":"claude","cwd":"/work"}"#).unwrap();
        assert_eq!(new.orchestrator, None);

        // An ordinary chat is written as it always was.
        assert!(
            !serde_json::to_string(&info)
                .unwrap()
                .contains("orchestrator")
        );
        assert!(
            !serde_json::to_string(&new)
                .unwrap()
                .contains("orchestrator")
        );
    }

    #[test]
    fn an_orchestrator_chat_names_its_scope_in_info_and_in_the_request_that_makes_it() {
        let project = OrchestratorScope::Project {
            project_id: "11111111-1111-4111-8111-111111111111".into(),
        };
        assert_eq!(
            serde_json::to_value(&OrchestratorScope::Global).unwrap(),
            serde_json::json!({"scope": "global"})
        );
        assert_eq!(
            serde_json::to_value(&project).unwrap(),
            serde_json::json!({"scope": "project", "project_id": "11111111-1111-4111-8111-111111111111"})
        );
        assert_eq!(
            project.project_id(),
            Some("11111111-1111-4111-8111-111111111111")
        );
        assert_eq!(OrchestratorScope::Global.project_id(), None);

        let text = r#"{"id":"c1","provider":"claude","project_id":"11111111-1111-4111-8111-111111111111",
            "cwd":"/work","title":"P","created_at_unix":5,
            "orchestrator":{"scope":"project","project_id":"11111111-1111-4111-8111-111111111111"}}"#;
        let info: ChatInfo = serde_json::from_str(text).unwrap();
        assert_eq!(info.orchestrator, Some(project.clone()));
        assert_eq!(
            serde_json::from_str::<ChatInfo>(&serde_json::to_string(&info).unwrap()).unwrap(),
            info
        );
        let new: NewChat = serde_json::from_str(
            r#"{"provider":"codex","cwd":"/work","orchestrator":{"scope":"global"}}"#,
        )
        .unwrap();
        assert_eq!(new.orchestrator, Some(OrchestratorScope::Global));
        // A scope this build does not know is an error, not an ordinary chat.
        assert!(
            serde_json::from_str::<NewChat>(
                r#"{"provider":"codex","cwd":"/work","orchestrator":{"scope":"galaxy"}}"#
            )
            .is_err()
        );
    }

    #[test]
    fn events_round_trip_through_json() {
        let events = vec![
            ChatEvent::ItemStarted {
                item: Item {
                    presentation: Default::default(),
                    id: "c".into(),
                    turn_id: None,
                    status: ItemStatus::InProgress,
                    body: ItemBody::Command {
                        command: "ls".into(),
                        cwd: None,
                        output: String::new(),
                        exit_code: None,
                    },
                },
            },
            ChatEvent::ItemDelta {
                item_id: "c".into(),
                delta: Delta::Output("a\n".into()),
            },
            ChatEvent::State {
                state: ChatState::Failed {
                    message: "gone".into(),
                },
            },
        ];
        for event in events {
            let line = serde_json::to_string(&event).unwrap();
            assert_eq!(
                serde_json::from_str::<ChatEvent>(&line).unwrap(),
                event,
                "{line}"
            );
        }
        let command = ChatCommand::Approve {
            request_id: "r".into(),
            decision: Decision::AcceptForSession,
        };
        let line = serde_json::to_string(&command).unwrap();
        assert_eq!(serde_json::from_str::<ChatCommand>(&line).unwrap(), command);
    }

    fn model_option() -> ModelOption {
        ModelOption {
            id: "gpt-5.5".into(),
            name: "GPT-5.5".into(),
            description: "Frontier model".into(),
            efforts: vec!["low".into(), "medium".into(), "high".into()],
            default_effort: Some("medium".into()),
            supports_fast: true,
            is_default: true,
        }
    }

    #[test]
    fn a_model_option_has_the_keys_the_phone_reads_and_defaults_the_rest() {
        let json = serde_json::to_value(model_option()).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "id": "gpt-5.5",
                "name": "GPT-5.5",
                "description": "Frontier model",
                "efforts": ["low", "medium", "high"],
                "default_effort": "medium",
                "supports_fast": true,
                "is_default": true,
            })
        );
        assert_eq!(
            serde_json::from_value::<ModelOption>(json).unwrap(),
            model_option()
        );
        // Only the id and the name are required.
        let bare: ModelOption = serde_json::from_str(r#"{"id":"m","name":"M"}"#).unwrap();
        assert_eq!(
            bare,
            ModelOption {
                id: "m".into(),
                name: "M".into(),
                ..ModelOption::default()
            }
        );
        let event = ChatEvent::Models {
            models: vec![model_option(), bare],
        };
        let line = serde_json::to_string(&event).unwrap();
        assert!(
            line.starts_with(r#"{"event":"models","models":["#),
            "{line}"
        );
        assert_eq!(serde_json::from_str::<ChatEvent>(&line).unwrap(), event);
    }

    #[test]
    fn fast_defaults_off_in_info_new_chat_and_configure_written_before_it_existed() {
        let info: ChatInfo = serde_json::from_str(
            r#"{"id":"i","provider":"codex","cwd":"/w","title":"t","created_at_unix":1}"#,
        )
        .unwrap();
        assert!(!info.fast);
        let new: NewChat = serde_json::from_str(r#"{"provider":"claude","cwd":"/w"}"#).unwrap();
        assert!(!new.fast);
        let old: ChatCommand =
            serde_json::from_str(r#"{"command":"configure","effort":"high"}"#).unwrap();
        assert_eq!(
            old,
            ChatCommand::Configure {
                model: None,
                effort: Some("high".into()),
                approval_mode: None,
                fast: None,
            }
        );
        // Leaving it out stays out of the JSON; turning it off is a change that is sent.
        assert_eq!(
            serde_json::to_string(&old).unwrap(),
            r#"{"command":"configure","effort":"high"}"#
        );
        for fast in [true, false] {
            let command = ChatCommand::Configure {
                model: None,
                effort: None,
                approval_mode: None,
                fast: Some(fast),
            };
            let line = serde_json::to_string(&command).unwrap();
            assert_eq!(line, format!(r#"{{"command":"configure","fast":{fast}}}"#));
            assert_eq!(serde_json::from_str::<ChatCommand>(&line).unwrap(), command);
        }
        let mut info = info;
        info.fast = true;
        let line = serde_json::to_string(&info).unwrap();
        assert!(line.contains(r#""fast":true"#), "{line}");
        assert_eq!(serde_json::from_str::<ChatInfo>(&line).unwrap(), info);
    }

    #[test]
    fn each_models_event_replaces_the_transcripts_list() {
        let mut t = Transcript::default();
        assert!(t.models.is_empty());
        t.apply(&ChatEvent::Models {
            models: vec![model_option()],
        });
        assert_eq!(t.models, [model_option()]);
        t.apply(&ChatEvent::Models { models: Vec::new() });
        assert!(t.models.is_empty());
    }
}

/// Notices whose occurrence remains relevant across turns.
pub(crate) fn sticky_notice(kind: Option<&str>) -> bool {
    kind.is_some_and(|kind| {
        kind == notice_kind::AUTH_REQUIRED
            || kind == notice_kind::RATE_LIMIT
            || kind.starts_with("rate_limit:")
    })
}

#[cfg(test)]
mod rate_window_tests {
    use super::*;

    #[test]
    fn rate_limits_wire_round_trip_and_transcript_replacement() {
        let event = ChatEvent::RateLimits {
            windows: vec![RateWindow {
                id: "five_hour".into(),
                label: "5h".into(),
                used_percent: 30.0,
                resets_at: Some(1767225600),
                warn_at: 70.0,
            }],
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["event"], "rate_limits");
        assert_eq!(value["windows"][0]["used_percent"], 30.0);
        assert_eq!(serde_json::from_value::<ChatEvent>(value).unwrap(), event);
        let old: RateWindow =
            serde_json::from_str(r#"{"id":"primary","label":"5h","used_percent":50,"warn_at":50}"#)
                .unwrap();
        assert_eq!(old.resets_at, None);
        assert!(
            serde_json::to_value(old)
                .unwrap()
                .get("resets_at")
                .is_none()
        );
        let mut transcript = Transcript::default();
        transcript.apply(&event);
        assert_eq!(transcript.rate_limits.len(), 1);
        transcript.apply(&ChatEvent::RateLimits {
            windows: Vec::new(),
        });
        assert!(transcript.rate_limits.is_empty());
    }
}
