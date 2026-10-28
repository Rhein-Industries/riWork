//! Chat payloads that cross `riwork/native` (contracts §1.6). Engine wire and
//! log formats are untouched; these camelCase DTOs exist only at the bridge.
//! Each type declares its TypeScript shape literally and converts through serde,
//! the same pattern as `TabUpdate`; inputs decode strictly.

use nsh_core::{
    JsonValue,
    element::{JsHost, JsIn, JsOwned, JsValue, PropError, TsTypes},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

macro_rules! serde_js {
    ($ty:ty, $name:literal, $decl:expr) => {
        impl JsValue for $ty {
            fn ts_type(types: &mut TsTypes) -> String {
                types.declare($name, |_| $decl.into())
            }
            fn from_js(value: JsIn<'_>) -> Result<Self, PropError> {
                serde_json::from_value(JsonValue::from_js(value)?)
                    .map_err(|error| PropError(format!("invalid: {error}")))
            }
            fn to_js<'a>(&self, host: &'a dyn JsHost) -> Result<JsOwned<'a>, PropError> {
                serde_json::to_value(self)
                    .map_err(|error| PropError(error.to_string()))?
                    .to_js(host)
            }
        }
    };
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactDto {
    pub item_id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_index: Option<usize>,
    /// Embedded transcript bytes need no filesystem or network access. Local
    /// references still require an explicit preview action and native scope check.
    #[serde(default)]
    pub inline_image: bool,
}

/// One flattened, visible transcript entry: a header, never a body. Its `key`
/// is the List key and the body-map key (sections of long answers would be
/// `<id>#<block>`; Phase 4a keeps answers whole).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowDto {
    pub artifacts: Vec<ArtifactDto>,
    pub file_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<AttachmentDto>,
    pub key: String,
    pub id: String,
    pub kind: String,
    pub depth: u32,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<String>,
    pub open: bool,
    /// A chevron and focus stop (CHATT-029): never User/Final/Commentary/
    /// Compaction/Notice/Request.
    pub disclosure: bool,
    pub failed_count: usize,
    pub running: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_label: Option<String>,
    pub provisional: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// The row shows a body (paged separately by viewport).
    pub has_body: bool,
}

const ROW_TS: &str = "export interface ChatRow { artifacts?: {itemId:string;label:string;path?:string|null;line?:number|null;imageIndex?:number|null;inlineImage?:boolean}[]; fileCount?:number; attachments?: ChatAttachment[]; key: string; id: string; kind: 'user' | 'final' | 'commentary' | 'reasoning' | 'shell' | 'read' | 'search' | 'list' | 'web_search' | 'web_fetch' | 'mcp' | 'worker' | 'subagent' | 'other' | 'edit' | 'plan' | 'todo' | 'compaction' | 'notice' | 'request' | 'group' | 'activity'; depth: number; title: string; target?: string; meta?: string; open: boolean; disclosure: boolean; failedCount: number; running: boolean; durationMs?: number; currentLabel?: string; provisional: boolean; requestId?: string; status?: 'in_progress' | 'completed' | 'failed' | 'declined' | 'interrupted'; hasBody: boolean; }";
serde_js!(RowDto, "ChatRow", ROW_TS);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepDto {
    pub text: String,
    pub status: String,
}

/// A row's body. Markdown is parsed in Rust and arrives as nsh/base TextView
/// document blocks; TSX never parses transcript Markdown (CHATT-071).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BodyDto {
    Changes { files: Vec<super::review::ChatChangedFile> },
    Tool { view: Value, raw: String },
    Markdown {
        blocks: Vec<Value>,
    },
    Text {
        text: String,
        tone: String,
    },
    Command {
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        output: String,
        /// Bytes of older output left out of `output`.
        omitted: usize,
        #[serde(rename = "exitCode", default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        running: bool,
    },
    Detail {
        text: String,
    },
    Steps {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        explanation: Option<String>,
        steps: Vec<StepDto>,
    },
}

pub const BODY_TS: &str = "export type ChatBody = {kind:'changes';files:ChatChangedFile[]} | {kind:'tool';raw:string;view: {kind:'file_search';query:string;hits:{path:string;line?:number;preview:string}[];omitted:number} | {kind:'web_search';query:string;hits:{title:string;url:string;snippet:string}[];omitted:number;raw?:string} | {kind:'file_read';path:string;text:string;truncated:boolean} | {kind:'json';text:string;truncated:boolean}} | { kind: 'markdown'; blocks: import('nsh/base').TextViewBlock[] } | { kind: 'text'; text: string; tone: 'user' | 'reasoning' | 'info' | 'warning' | 'error' } | { kind: 'command'; command: string; cwd?: string; output: string; omitted: number; exitCode?: number; running: boolean } | { kind: 'detail'; text: string } | { kind: 'steps'; explanation?: string; steps: { text: string; status: 'pending' | 'in_progress' | 'completed' }[] };";
serde_js!(BodyDto, "ChatBody", BODY_TS);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChoiceDto {
    pub decision: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalDto {
    pub request_id: String,
    pub kind: String,
    /// "Run a command?", "Change files?", ... (CHATT-206)
    pub heading: String,
    pub title: String,
    pub detail: String,
    pub choices: Vec<ChoiceDto>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OptionDto {
    pub label: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    pub question: String,
    pub options: Vec<OptionDto>,
    pub multi_select: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestionDto {
    pub request_id: String,
    pub prompts: Vec<PromptDto>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PickRowDto {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub current: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolbarDto {
    pub provider: String,
    pub provider_label: String,
    pub model_label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub models: Vec<PickRowDto>,
    /// With no driver list, the field takes a typed name with these suggestions.
    pub model_suggestions: Vec<String>,
    pub effort_label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    pub efforts: Vec<PickRowDto>,
    pub effort_available: bool,
    pub fast_available: bool,
    pub fast: bool,
    pub approval_mode: String,
    pub mode_label: String,
    pub modes: Vec<PickRowDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contributor_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contributor_label: Option<String>,
    /// Other providers this conversation can move to (`Switch`).
    pub providers: Vec<PickRowDto>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoticeDto {
    pub id: String,
    pub level: String,
    pub text: String,
}

/// Everything about the chat apart from its rows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatPanelDto {
    pub chat_id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<String>,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_message: Option<String>,
    pub link: String,
    /// Running or Waiting: ⌘. and Stop interrupt (CHATT-020).
    pub running: bool,
    pub empty: bool,
    /// The footer status line (CHATT-019), when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    pub approvals: Vec<ApprovalDto>,
    pub questions: Vec<QuestionDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toolbar: Option<ToolbarDto>,
    pub notices: Vec<NoticeDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing: Option<super::billing::BillingDto>,
    pub file_changes: usize,
    pub workers: Vec<super::workers::ChatWorker>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_detail: Option<String>,
    pub mode: String,
    /// Commands that could not be delivered since the last delivery (CHATT-012).
    pub failures: Vec<String>,
}

const PANEL_TS: &str = "export interface ChatPanel { chatId: string; title: string; provider?: 'codex' | 'claude' | 'muse'; cwd?: string; projectId?: string; worktreeId?: string; state: 'starting' | 'idle' | 'running' | 'waiting' | 'stopped' | 'failed'; stateMessage?: string; link: 'connecting' | 'live' | 'reconnecting' | 'deleted'; running: boolean; empty: boolean; status?: string; approvals: ChatApproval[]; questions: ChatQuestion[]; toolbar?: ChatToolbar; notices: { id: string; level: 'info' | 'warning' | 'error'; text: string }[]; billing?: {selected?: 'subscription'|'api_key';reported?:'subscription'|'api_key';source?:string;limits:{id:string;resetsAt?:number}[]}; fileChanges?: number; workers?: ChatWorker[]; mode: 'normal' | 'verbose'; failures: string[]; sessionId?: string; usageLabel?: string; usageDetail?: string; }
export interface ChatApproval { requestId: string; kind: 'command' | 'file_change' | 'permissions' | 'tool'; heading: string; title: string; detail: string; choices: { decision: ChatDecision; label: string; key?: string }[]; }
export type ChatDecision = 'accept' | 'accept_for_session' | 'decline' | 'cancel';
export interface ChatQuestion { requestId: string; prompts: { header?: string; question: string; options: { label: string; description: string }[]; multiSelect: boolean }[]; }
export interface ChatPick { id: string; label: string; detail?: string; current: boolean; }
export interface ChatToolbar { provider: 'codex' | 'claude' | 'muse'; providerLabel: string; modelLabel: string; model?: string; models: ChatPick[]; modelSuggestions: string[]; effortLabel: string; effort?: string; efforts: ChatPick[]; effortAvailable: boolean; fastAvailable: boolean; fast: boolean; approvalMode: string; modeLabel: string; modes: ChatPick[]; providers: ChatPick[]; contributorMode?: 'on' | 'off'; contributorLabel?: string; }";
serde_js!(ChatPanelDto, "ChatPanel", PANEL_TS);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHitDto {
    pub id: String,
    pub label: String,
    pub preview: String,
}
serde_js!(SearchHitDto, "ChatSearchHit", "export interface ChatSearchHit { id: string; label: string; preview: string; }");

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkTargetDto {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}
serde_js!(LinkTargetDto, "ChatLinkTarget", "export interface ChatLinkTarget { path: string; line?: number; }");

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpliceDto {
    pub key: String,
    pub from: usize,
    pub len: usize,
    pub blocks: Vec<Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppendDto {
    pub key: String,
    pub text: String,
}

/// One delivery of `ChatHandle.subscribe`: a `reset` first (and after a
/// resync), then `patch`es. Rows are headers; bodies arrive only for rows near
/// the viewport (`setViewport`), and streaming arrives as `append` (plain
/// text) or `splice` (Markdown tail blocks), never as a whole message.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatDelivery {
    pub kind: String,
    pub rev: u64,
    pub seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panel: Option<ChatPanelDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<Vec<RowDto>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub upsert: Vec<RowDto>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remove: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bodies: BTreeMap<String, BodyDto>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drop: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub splice: Vec<SpliceDto>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub append: Vec<AppendDto>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub resync: bool,
}

const DELIVERY_TS: &str = "export interface ChatDelivery { kind: 'reset' | 'patch'; rev: number; seq: number; panel?: ChatPanel; rows?: ChatRow[]; upsert?: ChatRow[]; remove?: string[]; order?: string[]; bodies?: Record<string, ChatBody>; drop?: string[]; splice?: { key: string; from: number; len: number; blocks: import('nsh/base').TextViewBlock[] }[]; append?: { key: string; text: string }[]; resync?: boolean; }";

impl JsValue for ChatDelivery {
    fn ts_type(types: &mut TsTypes) -> String {
        RowDto::ts_type(types);
        BodyDto::ts_type(types);
        ChatPanelDto::ts_type(types);
        types.declare("ChatDelivery", |_| DELIVERY_TS.into())
    }
    fn from_js(value: JsIn<'_>) -> Result<Self, PropError> {
        serde_json::from_value(JsonValue::from_js(value)?)
            .map_err(|error| PropError(format!("invalid: {error}")))
    }
    fn to_js<'a>(&self, host: &'a dyn JsHost) -> Result<JsOwned<'a>, PropError> {
        serde_json::to_value(self)
            .map_err(|error| PropError(error.to_string()))?
            .to_js(host)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogModelDto {
    pub id: String,
    pub name: String,
    pub description: String,
    pub efforts: Vec<String>,
    pub default_effort: Option<String>,
    pub supports_fast: bool,
    pub is_default: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCatalogDto {
    pub supported: Vec<CatalogModelDto>,
    pub configured: Vec<String>,
    pub account_label: Option<String>,
    pub balanced: bool,
    pub error: Option<String>,
    pub warning: Option<String>,
}
serde_js!(ModelCatalogDto, "ChatModelCatalog", "export interface ChatModelCatalog { supported: { id: string; name: string; description: string; efforts: string[]; defaultEffort?: string | null; supportsFast: boolean; isDefault: boolean }[]; configured: string[]; accountLabel?: string | null; balanced: boolean; error?: string | null; warning?: string | null; }");

/// A chat as the host lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSummaryDto {
    pub id: String,
    pub title: String,
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<String>,
    pub cwd: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

serde_js!(
    ChatSummaryDto,
    "ChatSummary",
    "export interface ChatSummary { id: string; title: string; provider: 'codex' | 'claude' | 'muse'; projectId?: string; worktreeId?: string; cwd: string; state: string; model?: string; }"
);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewChatRequest {
    pub provider: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub worktree_id: Option<String>,
    pub cwd: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub approval_mode: Option<String>,
    #[serde(default)]
    pub fast: bool,
    #[serde(default)]
    pub title: Option<String>,
}

serde_js!(
    NewChatRequest,
    "NewChatRequest",
    "export interface NewChatRequest { provider: 'codex' | 'claude' | 'muse'; projectId?: string | null; worktreeId?: string | null; cwd: string; model?: string | null; effort?: string | null; approvalMode?: 'supervised' | 'auto_edit' | 'full' | 'plan' | null; fast?: boolean; title?: string | null; }"
);

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OpenOptions {
    /// Follow only an existing host (history browsing never starts one).
    #[serde(default)]
    pub history: bool,
    #[serde(default)]
    pub mode: Option<String>,
}

serde_js!(
    OpenOptions,
    "ChatOpenOptions",
    "export interface ChatOpenOptions { history?: boolean; mode?: 'normal' | 'verbose' | null; }"
);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AttachmentDto(pub riwork_engine::chat::attachments::Attachment);
serde_js!(AttachmentDto, "ChatAttachment", "export interface ChatAttachment { id: string; name: string; path: string; kind: { kind: 'text' } | { kind: 'image'; mime: string }; bytes: number; fingerprint: string; preview: { kind: 'text'; excerpt: string } | { kind: 'image'; path: string }; }");

/// A composer snapshot: exactly one exchange, with a receipt (CHATC-032..035).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubmitRequest {
    #[serde(default)]
    pub attachments: Vec<AttachmentDto>,
    pub text: String,
    pub editor: u64,
    pub generation: u64,
}

serde_js!(
    SubmitRequest,
    "ChatSubmit",
    "export interface ChatSubmit { attachments?: ChatAttachment[]; text: string; editor: number; generation: number; }"
);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptDto {
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// The composer may clear: the receipt still owns the draft it sent.
    pub clear: bool,
}

serde_js!(
    ReceiptDto,
    "ChatReceipt",
    "export interface ChatReceipt { status: 'submitted' | 'refused' | 'uncertain'; error?: string; key?: string; clear: boolean; }"
);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AnswerRequest {
    pub request_id: String,
    pub answers: Vec<Vec<String>>,
}

serde_js!(
    AnswerRequest,
    "ChatAnswer",
    "export interface ChatAnswer { requestId: string; answers: string[][]; }"
);

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigureRequest {
    #[serde(default)]
    pub contributor_mode: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub fast: Option<bool>,
    #[serde(default)]
    pub approval_mode: Option<String>,
}

serde_js!(
    ConfigureRequest,
    "ChatConfigure",
    "export interface ChatConfigure { contributorMode?: 'on' | 'off' | null; model?: string | null; effort?: string | null; fast?: boolean | null; approvalMode?: 'supervised' | 'auto_edit' | 'full' | 'plan' | null; }"
);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedSubmissionDto {
    pub attachments: Vec<AttachmentDto>,
    pub key: String,
    pub text: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftDto {
    pub attachments: Vec<AttachmentDto>,
    pub text: String,
    pub submissions: Vec<SavedSubmissionDto>,
    pub warnings: Vec<String>,
}

serde_js!(
    DraftDto,
    "ChatDraft",
    "export interface ChatDraft { attachments: ChatAttachment[]; text: string; submissions: { attachments: ChatAttachment[]; key: string; text: string; status: 'pending' | 'submitted' | 'refused' | 'uncertain'; error?: string }[]; warnings: string[]; }"
);

pub fn decision_name(decision: riwork_engine::chat::model::Decision) -> &'static str {
    use riwork_engine::chat::model::Decision::*;
    match decision {
        Accept => "accept",
        AcceptForSession => "accept_for_session",
        Decline => "decline",
        Cancel => "cancel",
    }
}

pub fn parse_decision(name: &str) -> Result<riwork_engine::chat::model::Decision, String> {
    use riwork_engine::chat::model::Decision::*;
    Ok(match name {
        "accept" => Accept,
        "accept_for_session" => AcceptForSession,
        "decline" => Decline,
        "cancel" => Cancel,
        other => return Err(format!("invalid: unknown decision {other:?}")),
    })
}

/// A serde snake_case name of an engine enum value (provider, state, mode).
pub fn wire_name<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(Value::String(name)) => name,
        Ok(Value::Object(map)) => map
            .values()
            .next()
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        _ => String::new(),
    }
}

pub fn from_wire<T: for<'de> Deserialize<'de>>(name: &str) -> Result<T, String> {
    serde_json::from_value(Value::String(name.to_owned()))
        .map_err(|error| format!("invalid: {name:?}: {error}"))
}
