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

pub use riwork_core::provider::{ClaudeBilling, Provider};

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

/// Muse's contributor mode, which RiWork passes to every Muse it starts
/// (`--contributor-mode off|on`, a per-process override Muse never saves). Off unless the user
/// turns it on for one chat or one launch: RiWork never leaves it to the user's Muse settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContributorMode {
    #[default]
    Off,
    On,
}

impl ContributorMode {
    pub fn is_off(&self) -> bool {
        *self == Self::Off
    }

    /// What `--contributor-mode` takes.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::On => "on",
        }
    }
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
    /// The chat's auto title from its first accepted user message (`message_title`), set
    /// once: resumes, restarts and re-reading the log never derive it again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_user_message: Option<String>,
    /// The provider's own name for its thread or session, when it gives one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_title: Option<String>,
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
    /// Muse's contributor mode for this chat: every `muse serve` it starts, first, on each
    /// resume and after a switch back to Muse, says `--contributor-mode` with it. Only the user
    /// turns it on, for a Muse chat when it is made or later (`ChatCommand::Configure`); it
    /// stays with a chat that switches provider until it is turned off. Off is left out of the
    /// JSON, so a chat from before it, whatever its `contributor_opt_out`, is off.
    #[serde(default, skip_serializing_if = "ContributorMode::is_off")]
    pub contributor_mode: ContributorMode,
    /// What hosts before `contributor_mode` read and wrote: true started the chat's Muse with
    /// contributor mode off, false left it to the user's Muse settings. Read, it never turns
    /// contributor mode on. The host keeps it true for a Muse chat that is off
    /// (`ChatInfo::settle_contributor`), so a reader from before sees it off, and a client can
    /// tell a host from before, which reports a Muse chat with neither field, by what the chat
    /// says it runs with (`ChatInfo::reported_contributor_mode`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub contributor_opt_out: bool,
    /// Who the chat's Claude bills (`ClaudeBilling`): every `claude` it starts, first, on
    /// each resume and after a switch back to Claude, gets the saved Anthropic API key or
    /// loses `ANTHROPIC_API_KEY` by it (`launch`). A Claude chat is made with the Settings
    /// default, and the user changes it for the chat later (`ChatCommand::Configure`). None
    /// for a chat made before chats had it, which follows the Settings default at each
    /// start as every Claude chat did then, and for another provider's chat until it is
    /// chosen. It stays with a chat that switches provider. Only the choice is kept here,
    /// never a key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_billing: Option<ClaudeBilling>,
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
    /// The conversation the chat had before it moved to its current provider (`Switch`),
    /// which the agent is given at every start. A chat that never switched has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carried_over: Option<CarriedOver>,
}

impl ChatInfo {
    /// Who the chat's Claude bills at its next start: its own choice, else `default`, the
    /// Settings default (`settings::claude_chat_billing`).
    pub fn claude_billing_or(&self, default: ClaudeBilling) -> ClaudeBilling {
        self.claude_billing.unwrap_or(default)
    }

    /// Keeps `contributor_opt_out` what a reader from before `contributor_mode` should see:
    /// true for a Muse chat that is off. The host settles every chat it loads, makes or
    /// changes.
    pub fn settle_contributor(&mut self) {
        self.contributor_opt_out =
            self.provider == Provider::Muse && self.contributor_mode.is_off();
    }

    /// The contributor mode the chat's Muse runs with, as the host that reported the chat says.
    /// None for a Muse chat from a host before `contributor_mode` that leaves it to the user's
    /// Muse settings: neither on nor `contributor_opt_out`. A client checks a change by this,
    /// never by `contributor_mode` alone, which such a host leaves off.
    pub fn reported_contributor_mode(&self) -> Option<ContributorMode> {
        match self.contributor_mode {
            ContributorMode::On => Some(ContributorMode::On),
            ContributorMode::Off if self.contributor_opt_out || self.provider != Provider::Muse => {
                Some(ContributorMode::Off)
            }
            ContributorMode::Off => None,
        }
    }
}

/// Refused: a request that asks for contributor mode both on and off.
pub const CONTRIBUTOR_CONTRADICTION: &str =
    "contributor_mode on cannot be combined with contributor_opt_out, which asks for it off.";

/// Refused: the false of `contributor_opt_out`, which asked for the user's Muse settings.
pub const CONTRIBUTOR_SETTINGS_GONE: &str = "Contributor mode no longer follows your Muse \
     settings: RiWork keeps it off unless it is turned on for a chat (contributor_mode on).";

/// What a chat that switched provider passes on to its new agent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CarriedOver {
    /// The conversation up to the switch, as a Markdown document in the chat's folder.
    pub document: PathBuf,
    /// Who had it, as the document names it: `Codex chat "Fix it" (1234abcd)`.
    pub from: String,
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
    /// Muse's contributor mode for a Muse chat (`ChatInfo::contributor_mode`); off unless asked
    /// on. A Codex or Claude chat has no contributor mode: asked for one, it is made off, and
    /// says so.
    #[serde(default, skip_serializing_if = "ContributorMode::is_off")]
    pub contributor_mode: ContributorMode,
    /// The off that callers before `contributor_mode` could ask for, which never turns it on.
    /// The client sends it beside an off Muse chat (`Client::create`), so a host from
    /// before `contributor_mode`, which reads only this, starts the chat off too. Together with
    /// `contributor_mode` on it is refused.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub contributor_opt_out: bool,
    /// Who a Claude chat bills (`ChatInfo::claude_billing`); none is the Settings default.
    /// Another provider's chat is made without one. A host from before it reads past the
    /// field and makes the chat follow the Settings default, as chats did then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_billing: Option<ClaudeBilling>,
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

/// Stable tool categories. Unknown provider values remain renderable as `Other`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    Shell,
    Read,
    Search,
    List,
    WebSearch,
    WebFetch,
    Mcp,
    Subagent,
    #[default]
    Other,
}

impl<'de> Deserialize<'de> for ToolKind {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        Ok(match value.as_str() {
            Some("shell") => Self::Shell,
            Some("read") => Self::Read,
            Some("search") => Self::Search,
            Some("list") => Self::List,
            Some("web_search") => Self::WebSearch,
            Some("web_fetch") => Self::WebFetch,
            Some("mcp") => Self::Mcp,
            Some("subagent") => Self::Subagent,
            _ => Self::Other,
        })
    }
}

// Presentation is best-effort metadata: malformed or relay-elided hints must not
// make an otherwise readable item or durable event disappear.
fn optional_metadata<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    if value.get("elided").and_then(serde_json::Value::as_bool) == Some(true) {
        return Ok(None);
    }
    Ok(serde_json::from_value(value).ok())
}

fn default_metadata<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    Ok(optional_metadata(deserializer)?.unwrap_or_default())
}

fn metadata_list<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value.as_array().map_or_else(Vec::new, |values| {
        values
            .iter()
            .filter_map(|value| serde_json::from_value(value.clone()).ok())
            .collect()
    }))
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineRange {
    pub start: u64,
    #[serde(
        default,
        deserialize_with = "optional_metadata",
        skip_serializing_if = "Option::is_none"
    )]
    pub end: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathRef {
    pub path: String,
    #[serde(
        default,
        deserialize_with = "optional_metadata",
        skip_serializing_if = "Option::is_none"
    )]
    pub line: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Worker {
    #[serde(
        deserialize_with = "optional_metadata",
        skip_serializing_if = "Option::is_none"
    )]
    pub role: Option<String>,
    #[serde(
        deserialize_with = "optional_metadata",
        skip_serializing_if = "Option::is_none"
    )]
    pub objective: Option<String>,
    #[serde(
        deserialize_with = "optional_metadata",
        skip_serializing_if = "Option::is_none"
    )]
    pub child_count: Option<u64>,
}

/// Additive semantic hints, populated at provider conversion or by legacy fallback.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Semantics {
    pub kind: ToolKind,
    #[serde(
        deserialize_with = "optional_metadata",
        skip_serializing_if = "Option::is_none"
    )]
    pub target: Option<String>,
    #[serde(
        deserialize_with = "optional_metadata",
        skip_serializing_if = "Option::is_none"
    )]
    pub range: Option<LineRange>,
    #[serde(
        deserialize_with = "optional_metadata",
        skip_serializing_if = "Option::is_none"
    )]
    pub match_count: Option<u64>,
    #[serde(
        deserialize_with = "optional_metadata",
        skip_serializing_if = "Option::is_none"
    )]
    pub excerpt: Option<String>,
    #[serde(
        deserialize_with = "metadata_list",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub produced: Vec<PathRef>,
    #[serde(
        deserialize_with = "optional_metadata",
        skip_serializing_if = "Option::is_none"
    )]
    pub worker: Option<Worker>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Presentation {
    #[serde(
        default,
        deserialize_with = "optional_metadata",
        skip_serializing_if = "Option::is_none"
    )]
    pub semantics: Option<Semantics>,
    #[serde(
        default,
        deserialize_with = "optional_metadata",
        skip_serializing_if = "Option::is_none"
    )]
    pub parent_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "optional_metadata",
        skip_serializing_if = "Option::is_none"
    )]
    pub phase: Option<MessagePhase>,
    #[serde(
        default,
        deserialize_with = "metadata_list",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub images: Vec<ChatImage>,
    /// Owned, bounded submitted snapshots, including text files. Durable history
    /// can distinguish equal-length versions without inlining their payloads.
    #[serde(
        default,
        deserialize_with = "metadata_list",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub attachments: Vec<super::attachments::Attachment>,
}
impl Presentation {
    pub fn is_empty(&self) -> bool {
        self.phase.is_none()
            && self.images.is_empty()
            && self.attachments.is_empty()
            && self.semantics.is_none()
            && self.parent_id.is_none()
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
    /// Host-captured lifecycle times in Unix milliseconds; absent in older logs.
    #[serde(
        default,
        deserialize_with = "optional_metadata",
        skip_serializing_if = "Option::is_none"
    )]
    pub started_at: Option<u64>,
    #[serde(
        default,
        deserialize_with = "optional_metadata",
        skip_serializing_if = "Option::is_none"
    )]
    pub ended_at: Option<u64>,
    /// Additive presentation hints; older logs and clients may omit/ignore them.
    #[serde(
        default,
        deserialize_with = "default_metadata",
        skip_serializing_if = "Presentation::is_empty"
    )]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
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

/// How the agent says it signed in, from its own handshake: only what tells who is
/// billed, never an email, an organization or a credential.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    /// Set when the agent runs on an API key, which is billed instead of a subscription:
    /// where the key came from, as the agent names it (Claude's `apiKeySource`, such as
    /// `ANTHROPIC_API_KEY` or `apiKeyHelper`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_source: Option<String>,
    /// The subscription it runs under, as the agent names it ("Claude Max").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
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
    /// Driver-to-host only: never logged or sent to clients.
    #[serde(skip)]
    ProviderAccountIdentity {
        identity: Option<super::account_identity::Identity>,
    },
    /// The chat's metadata changed (thread id learned, model or mode changed).
    Info {
        info: ChatInfo,
    },
    /// A driver learned the provider's own name for the thread or session. The host takes
    /// it into the info (`ChatInfo::provider_title`); it is never logged or sent.
    ProviderTitle {
        title: String,
    },
    State {
        state: ChatState,
    },
    TurnStarted {
        turn_id: String,
        #[serde(
            default,
            deserialize_with = "optional_metadata",
            skip_serializing_if = "Option::is_none"
        )]
        started_at: Option<u64>,
    },
    TurnCompleted {
        turn_id: String,
        outcome: TurnOutcome,
        #[serde(
            default,
            deserialize_with = "optional_metadata",
            skip_serializing_if = "Option::is_none"
        )]
        ended_at: Option<u64>,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        answer_summary: Option<String>,
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
    /// How the agent signed in. A driver sends it after its handshake and again if
    /// that changes; each replaces the last.
    Account {
        account: Account,
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
        /// Muse only (`ChatInfo::contributor_mode`), and on its own. Muse fixes it for a
        /// process, so the host restarts an idle chat's `muse serve` and resumes its session,
        /// stores it for a stopped chat's next start, and refuses it while a turn runs, waits or
        /// the process still starts. A host from before it reads past the field: the client
        /// checks the chat afterwards (`Client::command`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        contributor_mode: Option<ContributorMode>,
        /// What clients before `contributor_mode` sent: true asks for off, as
        /// `contributor_mode` off does, and the client sends it beside that, so a host that reads
        /// only this turns it off too. False asked for the user's Muse settings, which RiWork no
        /// longer follows, and is refused (`ChatCommand::requested_contributor_mode`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        contributor_opt_out: Option<bool>,
        /// Claude only (`ChatInfo::claude_billing`), and on its own: the environment is
        /// fixed when `claude` starts, so the host restarts an idle chat's `claude` and resumes
        /// its session, stores it for a stopped chat's next start, and refuses it while a
        /// turn runs, waits or the process still starts, as a contributor mode change. A host
        /// from before it reads past the field: the client checks the chat afterwards
        /// (`Client::command`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        claude_billing: Option<ClaudeBilling>,
    },
    Compact,
    /// Stop the provider process; the chat resumes with the next message.
    Stop,
    /// Go on with another provider: the chat keeps its id, its log and its tab, and the
    /// new agent starts on a thread of its own with the conversation so far as context.
    /// Refused while a turn runs or waits. A host from before it refuses the command as
    /// unreadable, so it is never taken for a `Configure`.
    Switch {
        provider: Provider,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effort: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fast: Option<bool>,
    },
}

impl ChatCommand {
    /// What a Configure asks of contributor mode: `contributor_mode`, or the off an older
    /// client says with `contributor_opt_out` true. That field never turns it on: its false
    /// asked for the user's Muse settings, which RiWork no longer follows, and is refused, as
    /// is on together with an off.
    pub fn requested_contributor_mode(&self) -> Result<Option<ContributorMode>, String> {
        let ChatCommand::Configure {
            contributor_mode,
            contributor_opt_out,
            ..
        } = self
        else {
            return Ok(None);
        };
        match (contributor_mode, contributor_opt_out) {
            (_, Some(false)) => Err(CONTRIBUTOR_SETTINGS_GONE.into()),
            (Some(ContributorMode::On), Some(true)) => Err(CONTRIBUTOR_CONTRADICTION.into()),
            (Some(mode), _) => Ok(Some(*mode)),
            (None, Some(true)) => Ok(Some(ContributorMode::Off)),
            (None, None) => Ok(None),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestKind {
    #[default]
    Approval,
    Question,
}

/// Durable request history, ordered by the original request event sequence.
/// The pending dock continues to use `approvals` and `questions` separately.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RequestRecord {
    pub request_id: String,
    pub kind: RequestKind,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
    pub requested_seq: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_seq: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer_summary: Option<String>,
}

/// Host wall-clock timestamps in milliseconds for a whole turn. Legacy logs
/// have no timing; clients then fall back to summed known call durations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TurnTiming {
    #[serde(
        deserialize_with = "optional_metadata",
        skip_serializing_if = "Option::is_none"
    )]
    pub started_at: Option<u64>,
    #[serde(
        deserialize_with = "optional_metadata",
        skip_serializing_if = "Option::is_none"
    )]
    pub ended_at: Option<u64>,
}

/// A chat's transcript as events build it: what a tab draws, and what the
/// host replays to a tab that connects late.
#[derive(Clone, Debug, Default)]
pub struct Transcript {
    pub info: Option<ChatInfo>,
    pub state: ChatState,
    pub items: Vec<Item>,
    /// Requests still waiting for the user, in arrival order.
    pub approvals: Vec<Approval>,
    pub questions: Vec<Question>,
    pub requests: Vec<RequestRecord>,
    pub usage: Option<Usage>,
    pub rate_limits: Vec<RateWindow>,
    /// The models the provider offers, empty until its driver has said (an
    /// older driver never does).
    pub models: Vec<ModelOption>,
    /// How the agent signed in, once its driver has said (an older driver never does).
    pub account: Option<Account>,
    pub turn_id: Option<String>,
    pub turn_timing: HashMap<String, TurnTiming>,
    index: HashMap<String, usize>,
    item_sequences: HashMap<String, u64>,
    last_seq: u64,
    /// Bumped by every event applied, so what is derived from the transcript can be kept
    /// until it changes. Not part of what the transcript says.
    revision: u64,
}

/// Two transcripts are equal when they say the same, however many events built them.
impl PartialEq for Transcript {
    fn eq(&self, other: &Self) -> bool {
        self.info == other.info
            && self.state == other.state
            && self.items == other.items
            && self.approvals == other.approvals
            && self.questions == other.questions
            && self.requests == other.requests
            && self.usage == other.usage
            && self.rate_limits == other.rate_limits
            && self.models == other.models
            && self.turn_id == other.turn_id
            && self.turn_timing == other.turn_timing
            && self.index == other.index
    }
}

impl Transcript {
    fn record_request(&mut self, record: RequestRecord) {
        // A repeated request updates the pending payload without moving its boundary.
        if let Some(existing) = self
            .requests
            .iter_mut()
            .find(|r| r.request_id == record.request_id)
        {
            existing.title = record.title;
            existing.item_id = record.item_id.or_else(|| existing.item_id.clone());
            existing.turn_id = record.turn_id.or_else(|| existing.turn_id.clone());
        } else {
            self.requests.push(record);
            self.requests.sort_by_key(|record| record.requested_seq);
        }
    }

    fn resolve_request(
        &mut self,
        request_id: &str,
        seq: u64,
        resolution: &str,
        answer_summary: Option<String>,
    ) {
        if let Some(record) = self
            .requests
            .iter_mut()
            .find(|r| r.request_id == request_id)
        {
            record.resolved_seq = Some(seq);
            record.resolution = Some(resolution.into());
            record.answer_summary = answer_summary;
        }
    }

    /// Changes whenever an event is applied.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The item with `id`, found through the index.
    pub fn item(&self, id: &str) -> Option<&Item> {
        self.index
            .get(id)
            .and_then(|&at| self.items.get(at))
            .filter(|item| item.id == id)
            .or_else(|| self.items.iter().find(|item| item.id == id))
    }

    /// Sequence of the first event introducing this item.
    pub fn item_seq(&self, id: &str) -> Option<u64> {
        self.item_sequences.get(id).copied()
    }

    /// Fold an unsequenced synthetic event. Log and socket consumers should use
    /// `apply_sequenced` to preserve request/item ordering across partial replay.
    pub fn apply(&mut self, event: &ChatEvent) {
        self.apply_sequenced(self.last_seq.saturating_add(1), event);
    }

    pub fn apply_sequenced(&mut self, seq: u64, event: &ChatEvent) {
        self.last_seq = self.last_seq.max(seq);
        self.revision = self.revision.wrapping_add(1);
        match event {
            ChatEvent::Info { info } => {
                // A chat that moved to another provider starts over with what belongs to
                // one: the new agent lists its own models, counts its own usage and says
                // how it signed in.
                if self
                    .info
                    .as_ref()
                    .is_some_and(|before| before.provider != info.provider)
                {
                    self.models.clear();
                    self.rate_limits.clear();
                    self.usage = None;
                    self.account = None;
                }
                self.state = info.state.clone();
                self.info = Some(info.clone());
            }
            // The host folds it into the next Info.
            ChatEvent::ProviderTitle { .. } => {}
            ChatEvent::State { state } => {
                self.state = state.clone();
                if let Some(info) = &mut self.info {
                    info.state = state.clone();
                }
            }
            ChatEvent::TurnStarted {
                turn_id,
                started_at,
            } => {
                self.turn_id = Some(turn_id.clone());
                let timing = self.turn_timing.entry(turn_id.clone()).or_default();
                timing.started_at = timing.started_at.or(*started_at);
            }
            ChatEvent::TurnCompleted {
                turn_id,
                outcome,
                ended_at,
            } => {
                let timing = self.turn_timing.entry(turn_id.clone()).or_default();
                timing.ended_at = ended_at.or(timing.ended_at);
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
                for record in &mut self.requests {
                    if record.resolved_seq.is_none()
                        && record.turn_id.as_ref().is_none_or(|id| id == turn_id)
                    {
                        record.resolved_seq = Some(seq);
                        record.resolution = Some("cancel".into());
                    }
                }
                self.approvals.clear();
                self.questions.clear();
            }
            ChatEvent::ItemStarted { item } | ChatEvent::ItemCompleted { item } => {
                let mut item = item.clone();
                self.item_sequences.entry(item.id.clone()).or_insert(seq);
                match self.index.get(&item.id) {
                    Some(&at) => {
                        item.started_at = item.started_at.or(self.items[at].started_at);
                        item.presentation.parent_id = item
                            .presentation
                            .parent_id
                            .or_else(|| self.items[at].presentation.parent_id.clone());
                        super::semantics::enrich_inheriting(
                            &mut item,
                            self.items[at].presentation.semantics.as_ref(),
                        );
                        super::semantics::refresh_output(&mut item);
                        self.items[at] = item;
                    }
                    None => {
                        self.index.insert(item.id.clone(), self.items.len());
                        super::semantics::enrich(&mut item);
                        super::semantics::refresh_output(&mut item);
                        self.items.push(item);
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
                self.record_request(RequestRecord {
                    request_id: approval.request_id.clone(),
                    kind: RequestKind::Approval,
                    title: approval.title.clone(),
                    turn_id: approval
                        .item_id
                        .as_deref()
                        .and_then(|id| self.item(id))
                        .and_then(|item| item.turn_id.clone())
                        .or_else(|| self.turn_id.clone()),
                    item_id: approval.item_id.clone(),
                    requested_seq: seq,
                    ..Default::default()
                });
            }
            ChatEvent::ApprovalResolved {
                request_id,
                decision,
            } => {
                self.approvals.retain(|a| &a.request_id != request_id);
                let resolution = match decision {
                    Decision::Accept => "accept",
                    Decision::AcceptForSession => "accept_for_session",
                    Decision::Decline => "decline",
                    Decision::Cancel => "cancel",
                };
                self.resolve_request(request_id, seq, resolution, None);
            }
            ChatEvent::QuestionRequested { question } => {
                self.questions
                    .retain(|q| q.request_id != question.request_id);
                self.questions.push(question.clone());
                self.record_request(RequestRecord {
                    request_id: question.request_id.clone(),
                    kind: RequestKind::Question,
                    title: question
                        .questions
                        .iter()
                        .map(|prompt| prompt.question.as_str())
                        .collect::<Vec<_>>()
                        .join("; "),
                    turn_id: question
                        .item_id
                        .as_deref()
                        .and_then(|id| self.item(id))
                        .and_then(|item| item.turn_id.clone())
                        .or_else(|| self.turn_id.clone()),
                    item_id: question.item_id.clone(),
                    requested_seq: seq,
                    ..Default::default()
                });
            }
            ChatEvent::QuestionResolved {
                request_id,
                answer_summary,
            } => {
                self.questions.retain(|q| &q.request_id != request_id);
                self.resolve_request(request_id, seq, "answered", answer_summary.clone());
            }
            ChatEvent::Usage { usage } => self.usage = Some(usage.clone()),
            ChatEvent::Models { models } => self.models = models.clone(),
            ChatEvent::RateLimits { windows } => self.rate_limits = windows.clone(),
            ChatEvent::ProviderAccountIdentity { .. } => {}
            ChatEvent::Account { account } => self.account = Some(account.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str, text: &str, status: ItemStatus) -> Item {
        Item {
            started_at: None,
            ended_at: None,
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
            started_at: None,
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
            started_at: None,
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
            ended_at: None,
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
                    started_at: None,
                    ended_at: None,
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
                contributor_mode: None,
                contributor_opt_out: None,
                claude_billing: None,
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
                contributor_mode: None,
                contributor_opt_out: None,
                claude_billing: None,
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
    fn every_contributor_form_written_before_contributor_mode_reads_as_off_and_on_is_its_own() {
        use ContributorMode::{Off, On};
        let base = r#""id":"i","provider":"muse","cwd":"/w","title":"t","created_at_unix":1"#;
        // A chat a host before `contributor_mode` wrote, opted out or not, is off: an older false
        // (the user's Muse settings) never turns it on.
        for (legacy, opted_out) in [("", false), (r#","contributor_opt_out":false"#, false)]
            .into_iter()
            .chain([(r#","contributor_opt_out":true"#, true)])
        {
            let mut info: ChatInfo = serde_json::from_str(&format!("{{{base}{legacy}}}")).unwrap();
            assert_eq!(info.contributor_mode, Off, "{legacy}");
            assert_eq!(info.contributor_opt_out, opted_out, "{legacy}");
            // As a host before it reports it, only the opted-out one runs off for certain.
            let reported = if opted_out { Some(Off) } else { None };
            assert_eq!(info.reported_contributor_mode(), reported, "{legacy}");
            // Settled by this host, it is off, and says so to a reader from before.
            info.settle_contributor();
            assert!(info.contributor_opt_out);
            assert_eq!(info.reported_contributor_mode(), Some(Off));
            let line = serde_json::to_string(&info).unwrap();
            assert!(line.contains(r#""contributor_opt_out":true"#), "{line}");
            assert!(!line.contains("contributor_mode"), "{line}");
        }
        // On is written as itself, distinct from anything an older host wrote, and a reader
        // from before sees no opt-out, as it would for a chat that follows the Muse settings.
        let mut info: ChatInfo = serde_json::from_str(&format!("{{{base}}}")).unwrap();
        info.contributor_mode = On;
        info.settle_contributor();
        let line = serde_json::to_string(&info).unwrap();
        assert!(line.contains(r#""contributor_mode":"on""#), "{line}");
        assert!(!line.contains("contributor_opt_out"), "{line}");
        assert_eq!(serde_json::from_str::<ChatInfo>(&line).unwrap(), info);
        assert_eq!(info.reported_contributor_mode(), Some(On));
        // Another provider's chat keeps the mode for a switch back, and is never opted out.
        let mut codex: ChatInfo = serde_json::from_str(
            r#"{"id":"i","provider":"codex","cwd":"/w","title":"t","created_at_unix":1,"contributor_opt_out":true}"#,
        )
        .unwrap();
        codex.settle_contributor();
        assert!(!codex.contributor_opt_out);
        assert_eq!(codex.reported_contributor_mode(), Some(Off));
        assert!(
            !serde_json::to_string(&codex)
                .unwrap()
                .contains("contributor")
        );

        // A new chat asked before `contributor_mode`, opted out or not, is off.
        for legacy in [
            "",
            r#","contributor_opt_out":false"#,
            r#","contributor_opt_out":true"#,
        ] {
            let new: NewChat =
                serde_json::from_str(&format!(r#"{{"provider":"muse","cwd":"/w"{legacy}}}"#))
                    .unwrap();
            assert_eq!(new.contributor_mode, Off, "{legacy}");
        }
        let mut new: NewChat = serde_json::from_str(r#"{"provider":"muse","cwd":"/w"}"#).unwrap();
        assert!(!serde_json::to_string(&new).unwrap().contains("contributor"));
        new.contributor_mode = On;
        let line = serde_json::to_string(&new).unwrap();
        assert!(line.contains(r#""contributor_mode":"on""#), "{line}");
        assert_eq!(serde_json::from_str::<NewChat>(&line).unwrap(), new);

        // A Configure from before the fields leaves contributor mode alone, and is written as
        // before.
        let old: ChatCommand =
            serde_json::from_str(r#"{"command":"configure","model":"m","fast":false}"#).unwrap();
        assert_eq!(
            old,
            ChatCommand::Configure {
                model: Some("m".into()),
                effort: None,
                approval_mode: None,
                fast: Some(false),
                contributor_mode: None,
                contributor_opt_out: None,
                claude_billing: None,
            }
        );
        assert_eq!(old.requested_contributor_mode(), Ok(None));
        assert_eq!(
            serde_json::to_string(&old).unwrap(),
            r#"{"command":"configure","model":"m","fast":false}"#
        );
        // Each state is asked for unambiguously; an older client's true is an off, its false
        // (the Muse settings) and on beside an off are refused.
        let configure =
            |mode: Option<ContributorMode>, opt_out: Option<bool>| ChatCommand::Configure {
                model: None,
                effort: None,
                approval_mode: None,
                fast: None,
                contributor_mode: mode,
                contributor_opt_out: opt_out,
                claude_billing: None,
            };
        for (mode, opt_out, line, requested) in [
            (
                Some(On),
                None,
                r#"{"command":"configure","contributor_mode":"on"}"#,
                Ok(Some(On)),
            ),
            (
                Some(Off),
                None,
                r#"{"command":"configure","contributor_mode":"off"}"#,
                Ok(Some(Off)),
            ),
            (
                Some(Off),
                Some(true),
                r#"{"command":"configure","contributor_mode":"off","contributor_opt_out":true}"#,
                Ok(Some(Off)),
            ),
            (
                None,
                Some(true),
                r#"{"command":"configure","contributor_opt_out":true}"#,
                Ok(Some(Off)),
            ),
            (
                None,
                Some(false),
                r#"{"command":"configure","contributor_opt_out":false}"#,
                Err(CONTRIBUTOR_SETTINGS_GONE.to_owned()),
            ),
            (
                Some(Off),
                Some(false),
                r#"{"command":"configure","contributor_mode":"off","contributor_opt_out":false}"#,
                Err(CONTRIBUTOR_SETTINGS_GONE.to_owned()),
            ),
            (
                Some(On),
                Some(true),
                r#"{"command":"configure","contributor_mode":"on","contributor_opt_out":true}"#,
                Err(CONTRIBUTOR_CONTRADICTION.to_owned()),
            ),
        ] {
            let command = configure(mode, opt_out);
            assert_eq!(serde_json::to_string(&command).unwrap(), line);
            assert_eq!(serde_json::from_str::<ChatCommand>(line).unwrap(), command);
            assert_eq!(command.requested_contributor_mode(), requested, "{line}");
        }
        // Nothing but on and off is a contributor mode.
        for bad in [r#""ON""#, r#""muse""#, "true", "null"] {
            let line = format!(r#"{{"command":"configure","contributor_mode":{bad}}}"#);
            let parsed = serde_json::from_str::<ChatCommand>(&line);
            if bad == "null" {
                assert_eq!(parsed.unwrap(), configure(None, None));
            } else {
                assert!(parsed.is_err(), "{line}");
            }
        }
    }

    #[test]
    fn a_switch_command_and_carried_over_info_have_their_own_shape() {
        let switch = ChatCommand::Switch {
            provider: Provider::Claude,
            model: Some("opus".into()),
            effort: None,
            fast: None,
        };
        let line = serde_json::to_string(&switch).unwrap();
        assert_eq!(
            line,
            r#"{"command":"switch","provider":"claude","model":"opus"}"#
        );
        assert_eq!(serde_json::from_str::<ChatCommand>(&line).unwrap(), switch);
        // A switch needs a provider; it is never a configure without one.
        assert!(
            serde_json::from_str::<ChatCommand>(r#"{"command":"switch","model":"o"}"#).is_err()
        );

        let text = r#"{"id":"i","provider":"claude","cwd":"/w","title":"t","created_at_unix":1,
            "carried_over":{"document":"/home/chats/i/context.md","from":"Codex chat \"t\" (i)"}}"#;
        let info: ChatInfo = serde_json::from_str(text).unwrap();
        assert_eq!(
            info.carried_over,
            Some(CarriedOver {
                document: "/home/chats/i/context.md".into(),
                from: "Codex chat \"t\" (i)".into(),
            })
        );
        let mut plain = info.clone();
        plain.carried_over = None;
        assert!(
            !serde_json::to_string(&plain)
                .unwrap()
                .contains("carried_over")
        );
    }

    #[test]
    fn info_with_another_provider_drops_models_usage_account_and_rate_limits() {
        let info = |provider| ChatInfo {
            parent_id: None,
            user_title: None,
            first_user_message: None,
            provider_title: None,
            id: "i".into(),
            provider,
            project_id: None,
            worktree_id: None,
            cwd: "/w".into(),
            title: "t".into(),
            created_at_unix: 1,
            provider_thread_id: None,
            model: None,
            effort: None,
            fast: false,
            contributor_mode: ContributorMode::Off,
            contributor_opt_out: false,
            claude_billing: None,
            approval_mode: ApprovalMode::Supervised,
            codex_account_id: None,
            state: ChatState::Idle,
            orchestrator: None,
            carried_over: None,
        };
        let mut t = Transcript::default();
        t.apply(&ChatEvent::Info {
            info: info(Provider::Codex),
        });
        t.apply(&ChatEvent::Models {
            models: vec![model_option()],
        });
        t.apply(&ChatEvent::Usage {
            usage: Usage::default(),
        });
        t.apply(&ChatEvent::Account {
            account: Account {
                api_key_source: Some("ANTHROPIC_API_KEY".into()),
                plan: None,
            },
        });
        t.apply(&ChatEvent::RateLimits {
            windows: vec![RateWindow {
                id: "old-weekly".into(),
                label: "old weekly".into(),
                used_percent: 100.,
                resets_at: Some(u64::MAX),
                warn_at: 70.,
            }],
        });
        // The same provider again (a model change, a thread learned) keeps them.
        t.apply(&ChatEvent::Info {
            info: info(Provider::Codex),
        });
        assert_eq!(t.models, [model_option()]);
        assert!(t.usage.is_some());
        assert!(t.account.is_some());
        assert_eq!(t.rate_limits.len(), 1);
        t.apply(&ChatEvent::Info {
            info: info(Provider::Claude),
        });
        assert!(t.models.is_empty());
        assert_eq!(t.usage, None);
        // A badge for how the old agent signed in would be wrong for the new one.
        assert_eq!(t.account, None);
        assert!(t.rate_limits.is_empty());
        assert_eq!(t.info.unwrap().provider, Provider::Claude);
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

    #[test]
    fn each_account_event_replaces_the_last_and_its_wire_form_is_stable() {
        let mut t = Transcript::default();
        assert_eq!(t.account, None);
        let key = Account {
            api_key_source: Some("ANTHROPIC_API_KEY".into()),
            plan: None,
        };
        let event = ChatEvent::Account {
            account: key.clone(),
        };
        let line = serde_json::to_string(&event).unwrap();
        assert_eq!(
            line,
            r#"{"event":"account","account":{"api_key_source":"ANTHROPIC_API_KEY"}}"#
        );
        assert_eq!(serde_json::from_str::<ChatEvent>(&line).unwrap(), event);
        t.apply(&event);
        assert_eq!(t.account.as_ref(), Some(&key));
        let plan = Account {
            api_key_source: None,
            plan: Some("Claude Max".into()),
        };
        t.apply(&ChatEvent::Account {
            account: plan.clone(),
        });
        assert_eq!(t.account, Some(plan));
    }
}

/// Notices whose occurrence remains relevant across turns.
pub fn sticky_notice(kind: Option<&str>) -> bool {
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

#[cfg(test)]
mod tool_contract_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn old_items_and_unknown_or_malformed_hints_remain_readable() {
        let legacy = json!({"id":"old","body":{"type":"tool_call","tool":"Read"}});
        let item: Item = serde_json::from_value(legacy.clone()).unwrap();
        assert!(item.presentation.is_empty());
        assert_eq!(item.started_at, None);
        assert_eq!(item.ended_at, None);
        assert!(
            serde_json::to_value(item)
                .unwrap()
                .get("presentation")
                .is_none()
        );
        let mut value = legacy;
        value["started_at"] = json!("elided");
        value["ended_at"] = json!(-1);
        value["presentation"] = json!({"parent_id":43,"semantics":{
            "kind":"future_tool", "target":false, "range":{"elided":true},
            "match_count":"many", "excerpt":{"elided":true}, "worker":45,
            "produced":[null,{"path":"report.md","line":3}]
        }});
        let item: Item = serde_json::from_value(value.clone()).unwrap();
        let semantics = item.presentation.semantics.unwrap();
        assert_eq!(semantics.kind, ToolKind::Other);
        assert_eq!(semantics.target, None);
        assert_eq!(semantics.range, None);
        assert_eq!(semantics.match_count, None);
        assert_eq!(semantics.excerpt, None);
        assert_eq!(semantics.worker, None);
        assert_eq!(
            semantics.produced,
            vec![PathRef {
                path: "report.md".into(),
                line: Some(3)
            }]
        );
        value["presentation"] = json!({"elided":true});
        assert!(
            serde_json::from_value::<Item>(value.clone())
                .unwrap()
                .presentation
                .is_empty()
        );
        value["presentation"] = json!("elided");
        assert!(
            serde_json::from_value::<Item>(value)
                .unwrap()
                .presentation
                .is_empty()
        );
    }

    #[test]
    fn new_presentation_fields_keep_the_presentation_serialized() {
        let presentation = Presentation {
            parent_id: Some("worker".into()),
            ..Default::default()
        };
        assert!(!presentation.is_empty());
        let presentation = Presentation {
            semantics: Some(Semantics {
                kind: ToolKind::Read,
                target: Some("main.rs".into()),
                range: Some(LineRange {
                    start: 10,
                    end: Some(20),
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(!presentation.is_empty());
        assert_eq!(
            serde_json::from_value::<Presentation>(serde_json::to_value(&presentation).unwrap())
                .unwrap(),
            presentation
        );
    }

    #[test]
    fn turn_timing_is_optional_tolerant_and_retains_the_first_start() {
        let mut transcript = Transcript::default();
        for value in [
            json!({"event":"turn_started","turn_id":"legacy"}),
            json!({"event":"turn_completed","turn_id":"legacy","outcome":{"outcome":"completed"}}),
            json!({"event":"turn_started","turn_id":"t","started_at":1000}),
            json!({"event":"turn_started","turn_id":"t","started_at":2000}),
            json!({"event":"turn_completed","turn_id":"t","outcome":{"outcome":"completed"},"ended_at":73000}),
            json!({"event":"turn_started","turn_id":"elided","started_at":{"elided":true}}),
            json!({"event":"turn_completed","turn_id":"elided","outcome":{"outcome":"completed"},"ended_at":"bad"}),
        ] {
            let event = serde_json::from_value(value).unwrap();
            transcript.apply(&event);
        }
        assert_eq!(transcript.turn_timing["legacy"], TurnTiming::default());
        assert_eq!(transcript.turn_timing["elided"], TurnTiming::default());
        assert_eq!(
            transcript.turn_timing["t"],
            TurnTiming {
                started_at: Some(1000),
                ended_at: Some(73000)
            }
        );
        let old: ChatEvent =
            serde_json::from_value(json!({"event":"turn_started","turn_id":"old"})).unwrap();
        assert_eq!(
            serde_json::to_value(old).unwrap(),
            json!({"event":"turn_started","turn_id":"old"})
        );
    }

    #[test]
    fn generic_legacy_completion_inherits_provider_kind_in_raw_and_replay_folds() {
        let mut unknown: Item = serde_json::from_value(json!({
            "id":"unknown", "body":{"type":"tool_call","tool":"unclassified"}
        }))
        .unwrap();
        super::super::semantics::enrich_inheriting(&mut unknown, None);
        assert!(
            unknown.presentation.semantics.is_none(),
            "an inferred Other is not a provider decision"
        );
        for completion_semantics in [None, Some(json!({"kind":"other"}))] {
            let start = json!({
                "event":"item_started", "item":{
                    "id":"worker", "body":{"type":"tool_call","tool":"auditor"},
                    "presentation":{"semantics":{"kind":"subagent","target":"Audit models"}}
                }
            });
            let mut completed = json!({
                "event":"item_completed", "item":{
                    "id":"worker", "body":{"type":"tool_call","tool":"auditor","output":"done"}
                }
            });
            if let Some(semantics) = completion_semantics.as_ref() {
                completed["item"]["presentation"] = json!({"semantics":semantics});
            }
            let mut raw = Transcript::default();
            let mut replay = Transcript::default();
            for (index, event) in [start, completed].into_iter().enumerate() {
                let event: ChatEvent = serde_json::from_value(event).unwrap();
                raw.apply_sequenced(index as u64 + 1, &event);
                let mut enriched = event;
                if let ChatEvent::ItemStarted { item } | ChatEvent::ItemCompleted { item } =
                    &mut enriched
                {
                    super::super::semantics::enrich(item);
                    super::super::semantics::refresh_output(item);
                }
                replay.apply_sequenced(index as u64 + 1, &enriched);
            }
            assert_eq!(raw, replay);
            let semantics = raw
                .item("worker")
                .unwrap()
                .presentation
                .semantics
                .as_ref()
                .unwrap();
            if completion_semantics.is_some() {
                assert_eq!(semantics.kind, ToolKind::Other, "explicit Other wins");
                assert_eq!(semantics.target, None);
            } else {
                assert_eq!(semantics.kind, ToolKind::Subagent);
                assert_eq!(semantics.target.as_deref(), Some("Audit models"));
            }
        }
    }

    #[test]
    fn completed_legacy_arguments_replace_empty_start_fallback() {
        let mut transcript = Transcript::default();
        for (event, query) in [("item_started", ""), ("item_completed", "rust serde")] {
            transcript.apply(
                &serde_json::from_value(json!({
                    "event": event,
                    "item": {"id":"web", "body":{"type":"web_search","query":query}}
                }))
                .unwrap(),
            );
        }
        assert_eq!(
            transcript
                .item("web")
                .unwrap()
                .presentation
                .semantics
                .as_ref()
                .unwrap()
                .target
                .as_deref(),
            Some("rust serde")
        );
    }

    #[test]
    fn request_history_uses_real_sequences_and_keeps_resolution_in_order() {
        let mut transcript = Transcript::default();
        transcript.apply_sequenced(
            100,
            &ChatEvent::TurnStarted {
                turn_id: "turn".into(),
                started_at: None,
            },
        );
        let event: ChatEvent = serde_json::from_value(json!({"event":"item_started","item":{
            "id":"call","turn_id":"turn","body":{"type":"command","command":"npm test"}
        }}))
        .unwrap();
        transcript.apply_sequenced(105, &event);
        let request = ChatEvent::ApprovalRequested {
            approval: Approval {
                request_id: "approval".into(),
                item_id: Some("call".into()),
                kind: ApprovalKind::Command,
                title: "npm test".into(),
                detail: String::new(),
                choices: vec![Decision::Accept],
            },
        };
        transcript.apply_sequenced(110, &request);
        transcript.apply_sequenced(111, &request); // repeated pending payload retains original place
        transcript.apply_sequenced(
            120,
            &ChatEvent::ApprovalResolved {
                request_id: "approval".into(),
                decision: Decision::Accept,
            },
        );
        transcript.apply_sequenced(
            130,
            &ChatEvent::QuestionRequested {
                question: Question {
                    request_id: "question".into(),
                    item_id: None,
                    questions: vec![QuestionPrompt {
                        header: None,
                        question: "Which suite?".into(),
                        options: vec![],
                        multi_select: false,
                    }],
                },
            },
        );
        transcript.apply_sequenced(
            140,
            &ChatEvent::QuestionResolved {
                request_id: "question".into(),
                answer_summary: Some("Unit tests".into()),
            },
        );
        transcript.apply_sequenced(
            150,
            &ChatEvent::TurnCompleted {
                turn_id: "turn".into(),
                outcome: TurnOutcome::Completed,
                ended_at: None,
            },
        );
        assert_eq!(transcript.item_seq("call"), Some(105));
        assert!(transcript.approvals.is_empty() && transcript.questions.is_empty());
        assert_eq!(
            transcript
                .requests
                .iter()
                .map(|r| r.requested_seq)
                .collect::<Vec<_>>(),
            vec![110, 130]
        );
        assert_eq!(transcript.requests[0].turn_id.as_deref(), Some("turn"));
        assert_eq!(transcript.requests[0].resolved_seq, Some(120));
        assert_eq!(transcript.requests[0].resolution.as_deref(), Some("accept"));
        assert_eq!(
            transcript.requests[1].answer_summary.as_deref(),
            Some("Unit tests")
        );
        let legacy: ChatEvent =
            serde_json::from_value(json!({"event":"question_resolved","request_id":"old"}))
                .unwrap();
        assert!(matches!(
            legacy,
            ChatEvent::QuestionResolved {
                answer_summary: None,
                ..
            }
        ));
    }
}
