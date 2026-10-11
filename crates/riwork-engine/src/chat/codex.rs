//! The Codex driver: `codex app-server` over JSON-RPC 2.0 on stdio, one JSON
//! object per line (the `"jsonrpc"` member is left out, as Codex's own clients
//! do, and not required of Codex).
//!
//! Every method and field below is taken from the schema `codex app-server
//! generate-json-schema --experimental` writes (checked against codex 0.159.3)
//! and from recorded conversations (T3 Code's replay fixtures, codex 0.156.1).
//! The server is marked experimental, so unknown notifications, requests and
//! fields are ignored (a request we cannot answer gets a JSON-RPC error so the
//! provider never waits on us).
//!
//! # Lifecycle
//!
//! `start` spawns `codex <extra_args> app-server` and does the handshake before
//! it returns: `initialize` (experimental API on, `turn/diff/updated` and
//! `item/plan/delta` opted out as large and unused), `initialized`, then
//! `thread/start`, or `thread/resume` with `excludeTurns` when `config.resume`
//! is set (the host already has the history). So when `start` returns, the
//! thread id is known and the events so far are `State { Starting }` and
//! `State { Idle }`. Right after that the driver asks `model/list` (page after page
//! while `nextCursor` is set, at most 20) without making the chat wait, and says
//! what it finds in one `Models` event, a moment after `Idle`.
//!
//! Codex only saves a thread once it has run a turn, so resuming one that never
//! did fails with "no rollout found". Then (and only then) the driver opens a
//! new thread and says so in a `Notice`: the host must read
//! `provider_thread_id()` after `start`, since it may not be `config.resume`.
//!
//! The driver never emits `Info`: it does not know the chat's id, title or
//! creation time. The host builds `Info` from `provider_thread_id()`.
//!
//! # Events
//!
//! | Codex | `ChatEvent` |
//! |---|---|
//! | `turn/started` (or the `turn/start` result) | `TurnStarted`, `State { Running }` |
//! | `turn/completed` `completed` / `interrupted` / `failed` | `TurnCompleted` with `Completed` / `Interrupted` / `Failed { message }`, `State { Idle }` |
//! | `item/started`, `item/completed` | `ItemStarted`, `ItemCompleted` (below) |
//! | `item/agentMessage/delta` | `ItemDelta { Text }` |
//! | `item/reasoning/summaryTextDelta`, `.../textDelta` | `ItemDelta { Text }` (summary parts are separated by a blank line; whichever of the two streams comes first wins) |
//! | `item/commandExecution/outputDelta` | `ItemDelta { Output }` |
//! | `item/fileChange/patchUpdated` | `ItemStarted` again with the new changes |
//! | `turn/plan/updated` | `Plan` item `plan-<turn>`, replaced in place |
//! | `thread/tokenUsage/updated` | `Usage` (totals; `context_used` is the last request's tokens) |
//! | `model/list` (all pages) | `Models` (below); nothing if the list is empty or the same as before |
//! | `item/commandExecution/requestApproval` | `ApprovalRequested` (`Command`) |
//! | `item/fileChange/requestApproval` | `ApprovalRequested` (`FileChange`) |
//! | `item/permissions/requestApproval` | `ApprovalRequested` (`Permissions`) |
//! | `item/tool/requestUserInput` | `QuestionRequested` |
//! | `serverRequest/resolved` for a request still open | `ApprovalResolved { Cancel }` or `QuestionResolved` |
//! | `error`, `warning`, `configWarning` | a `Notice` item (the "Reconnecting... n/5" errors share one notice per turn, updated in place) |
//! | process exit | `TurnCompleted { Failed }` if a turn was open, `State { Failed }` |
//!
//! An oversized line (over 16 MiB; diffs of tens of megabytes have been seen)
//! is skipped with a `Notice`; an item whose completion was in it stays open
//! until its turn ends.
//!
//! `State` follows the driver's own bookkeeping, not `thread/status/changed`:
//! `Running` while a turn is open, `Waiting` while it has open requests.
//! Notifications for other threads (sub-agents) are ignored; their approval
//! requests are still shown, since the sub-agent would wait for them.
//!
//! | `ThreadItem` | `ItemBody` |
//! |---|---|
//! | `userMessage` | `UserMessage` (text parts joined) |
//! | `agentMessage` | `AgentMessage` |
//! | `reasoning` | `Reasoning` (summary, else raw content) |
//! | `plan` | `Plan` with the proposed plan's markdown as `explanation` and no steps |
//! | `commandExecution` | `Command` (a `bash -lc '…'` wrapper is stripped; output and exit code) |
//! | `fileChange` | `FileChange` (`add`/`delete`/`update`, `update` with a `move_path` is a rename; content-only diffs of added and deleted files become unified diffs) |
//! | `mcpToolCall`, `dynamicToolCall` | `ToolCall` |
//! | `collabAgentToolCall`, `imageView`, `imageGeneration` | `ToolCall` named after the item type |
//! | `webSearch` | `WebSearch` |
//! | `contextCompaction` | `Compaction` |
//! | `hookPrompt`, `functionCallOutput`, `sleep`, `subAgentActivity`, review modes | ignored |
//!
//! Item statuses `inProgress`, `completed`, `failed` and `declined` map to
//! `ItemStatus` by name. `ItemCompleted` replaces what deltas built, and Codex
//! often completes a command with `aggregatedOutput: null`, so the driver keeps
//! the streamed output (up to 4 MiB per item) and puts it back.
//!
//! # Commands
//!
//! | `ChatCommand` | Codex |
//! |---|---|
//! | `Send` | `turn/start`; while a turn runs, `turn/steer` (a steer that loses a race with the turn's end starts a new turn instead) |
//! | `Interrupt` | `turn/interrupt` (remembered when the turn id is not known yet) |
//! | `Approve` | the JSON-RPC response to the open request: `{ decision }`, or `{ permissions, scope }` for permissions |
//! | `Answer` | the response `{ answers: { <question id>: { answers } } }` |
//! | `Configure` | stored; model, effort, approval mode and fast mode ride on the next `turn/start` (an effort the chosen model does not list is refused with a `Notice`) |
//! | `Compact` | `thread/compact/start` |
//! | `Stop` | shutdown |
//!
//! `ApprovalMode` becomes `approvalPolicy` and `sandboxPolicy` on every
//! `turn/start`: `Supervised` is `untrusted` with `readOnly`, `AutoEdit` is
//! `on-request` with `workspaceWrite`, `Full` is `never` with
//! `dangerFullAccess`, and `Plan` is `never` with `readOnly` plus
//! `collaborationMode` `plan` (an experimental field, which is why
//! `initialize` opts in). Choosing another mode afterwards sends
//! `collaborationMode` `default` once, since the mode sticks to the thread.
//!
//! # Models, efforts and Fast mode
//!
//! `Models` has one `ModelOption` per model of `model/list` that is not `hidden`, in the
//! server's order: `id` is the `model` string (the one `turn/start` takes), `name` the
//! `displayName`, `efforts` the `reasoningEffort` of each `supportedReasoningEfforts`,
//! `default_effort` the `defaultReasoningEffort`, `is_default` the `isDefault`, and
//! `supports_fast` is true when `serviceTiers` has a tier whose `id` is `priority` (the tier
//! the apps call Fast; `additionalSpeedTiers` with `fast` counts for a server that predates
//! `serviceTiers`).
//!
//! Fast mode is the `serviceTier` `priority`. It is sent on `thread/start` and
//! `thread/resume` when the chat has it on, and on every `turn/start`: a model with a Fast
//! tier gets `priority` when it is on and `default` (standard speed) when it is off, since
//! the tier a turn sets stays for the turns after it and a resumed thread does not carry it
//! over; a model without the tier gets none; before the list is known, `priority` goes
//! out when asked for and `default` once after it is turned off. (The server drops a tier a
//! model does not advertise, with a `warning`.) A model whose catalog default is `priority`
//! is therefore at standard speed unless the chat has Fast on, which is what the toggle shows.
//!
//! An effort the model does not list is left out of `turn/start` and of the Plan mode
//! settings, with a `Notice` (once per model and effort), and a `Configure` that names one
//! changes nothing and says so. The server itself does not check the effort.
//!
//! Approval choices come from `availableDecisions` (`accept`,
//! `acceptForSession`, `decline`, `cancel`); the amendment forms, which
//! change persistent rules, are not offered. File-change and permission
//! requests carry no list, so they offer all the decisions their response
//! accepts.
//!
//! # Verified and assumed
//!
//! Run against a real codex 0.159.3 with an empty `CODEX_HOME` (no login, so
//! no tokens): `initialize`, `thread/start` and `thread/resume` with the
//! parameters above, `turn/start` with `collaborationMode`, `effort`,
//! `sandboxPolicy` and `summary`, `thread/compact/start`, the `error`
//! retry/failure notifications, closing stdin as shutdown, and the "no rollout
//! found" failure of resuming a thread without turns. `model/list` (and the
//! `serviceTier` values `priority` and `default` on `thread/start` and
//! `turn/start`, echoed in `thread/settings/updated`) was run the same way against
//! codex 0.160.0, the version installed when it was added: eight visible models and
//! three hidden, all in one page without a `limit`, `testdata/codex/model_list.json`;
//! so does the fact that `thread/resume` answers `serviceTier: null` whatever was sent.
//! Everything about approvals, questions, tool items, plans, steering and interrupting
//! comes from the schema and the T3 Code recordings, not from a live run.
//!
//! Assumed: that `thread/compact/start` is followed by the usual
//! `turn/started`, `contextCompaction` item and `turn/completed`
//! notifications, and the wording of the Plan mode developer instructions
//! (modelled on T3 Code's, which Codex acts on).

use super::child::{self, Frame, FrameReader, MAX_FRAME_BYTES, Proc};
use super::driver::{Driver, DriverConfig, StartDriver};
use super::model::notice_kind;
use super::model::{
    Approval, ApprovalKind, ApprovalMode, ChangeKind, ChatCommand, ChatEvent, ChatState, Decision,
    Delta, FileChange, Item, ItemBody, ItemStatus, ModelOption, NoticeLevel, Question,
    QuestionOption, QuestionPrompt, Step, StepStatus, TurnOutcome, Usage,
};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use uuid::Uuid;

const _: StartDriver = start;

const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(30);
const THREAD_TIMEOUT: Duration = Duration::from_secs(60);
/// The service tier that is Fast (`ModelServiceTier.id`; the name shown is "Fast", and the
/// server also takes `fast` as another name for it).
const FAST_TIER: &str = "priority";
/// The service tier that is standard speed. Sent explicitly to leave Fast: a tier set on a
/// turn stays for the turns after it, and leaving the field out keeps it.
const STANDARD_TIER: &str = "default";
/// The most pages of `model/list` read (the server returns everything in one by default).
const MODEL_PAGES: usize = 20;
/// Notifications that are large and that nothing here uses.
const OPT_OUT: [&str; 2] = ["turn/diff/updated", "item/plan/delta"];
/// Output kept per command or reasoning item to restore it on completion.
const STREAM_KEEP_BYTES: usize = 4 * 1024 * 1024;
/// How much of a diff goes into an approval's detail.
const DETAIL_BYTES: usize = 20_000;

const PLAN_INSTRUCTIONS: &str = "You are in Plan mode. Investigate without changing anything. \
Ask clarifying questions with request_user_input when they matter. When the plan is complete, \
present it wrapped in <proposed_plan> and </proposed_plan>.";

/// Start `codex app-server` for `config` and open (or resume) its thread.
pub fn start(config: DriverConfig, events: Sender<ChatEvent>) -> Result<Box<dyn Driver>, String> {
    let _ = events.send(ChatEvent::State {
        state: ChatState::Starting,
    });
    let (proc, stdout) =
        Proc::spawn(child::command(&config, &["app-server"])).inspect_err(|error| {
            let _ = events.send(ChatEvent::State {
                state: ChatState::Failed {
                    message: error.clone(),
                },
            });
        })?;
    let codex = Arc::new(Codex {
        proc,
        session: Mutex::new(Session::new(&config, events)),
    });
    let reader = {
        let codex = Arc::clone(&codex);
        thread::spawn(move || codex.read_loop(stdout))
    };
    let mut driver = CodexDriver {
        codex,
        reader: Some(reader),
        stopped: false,
    };
    match driver.codex.handshake(&config) {
        Ok(()) => Ok(Box::new(driver)),
        Err(error) => {
            driver.stop_process();
            driver.stopped = true;
            driver
                .codex
                .with(|session| session.finish_failed(error.clone()));
            Err(error)
        }
    }
}

struct CodexDriver {
    codex: Arc<Codex>,
    reader: Option<JoinHandle<()>>,
    /// The process has been stopped (by `shutdown`, or `start` giving up).
    stopped: bool,
}

impl CodexDriver {
    /// Stop the process and wait for the reader to see its end.
    fn stop_process(&mut self) {
        self.codex.with(|session| session.stopping = true);
        self.codex.proc.stop();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

impl Driver for CodexDriver {
    fn cancel_io(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        let proc = Arc::clone(&self.codex.proc);
        Some(Arc::new(move || proc.cancel_writes()))
    }
    fn command(&mut self, command: ChatCommand) -> Result<(), String> {
        if matches!(command, ChatCommand::Stop) {
            self.shutdown();
            return Ok(());
        }
        self.codex.command(command)
    }

    fn provider_thread_id(&self) -> Option<String> {
        self.codex.with(|session| session.thread_id.clone())
    }

    fn shutdown(&mut self) {
        if std::mem::replace(&mut self.stopped, true) {
            return;
        }
        self.stop_process();
        // The reader has seen the end of the output; nothing else emits now.
        self.codex.with(Session::finish_stopped);
    }
}

impl Drop for CodexDriver {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// What a request's answer does once it arrives, on the reader thread.
type Reply = Box<dyn FnOnce(&Codex, Result<Value, String>) + Send>;

struct Codex {
    proc: Arc<Proc>,
    session: Mutex<Session>,
}

/// An open request from Codex that waits for the user.
struct OpenRequest {
    request_id: String,
    /// The JSON-RPC id to answer.
    rpc_id: Value,
    kind: RequestKind,
    /// Whether the turn waits for the answer.
    blocking: bool,
}

enum RequestKind {
    Command { choices: Vec<Decision> },
    FileChange { choices: Vec<Decision> },
    Permissions { requested: Value },
    Question { ids: Vec<String> },
}

/// Text streamed into an item, kept to restore it when the item completes.
#[derive(Default)]
struct Streamed {
    text: String,
    /// The item has been shown (reasoning is shown with its first text).
    started: bool,
    /// `summaryIndex` or `contentIndex` last seen, for blank lines between parts.
    part: Option<i64>,
    /// Whether the reasoning came as summary (`true`) or raw content.
    summary: Option<bool>,
}

struct Settings {
    model: Option<String>,
    effort: Option<String>,
    mode: ApprovalMode,
    /// The user wants the Fast service tier, on a model that has one.
    fast: bool,
}

struct Session {
    identity_config: DriverConfig,
    identity: Option<super::account_identity::Identity>,
    events: Sender<ChatEvent>,
    next_request: i64,
    waiting: HashMap<i64, Reply>,
    thread_id: Option<String>,
    /// The model the thread runs, from `thread/start`; Plan mode needs one.
    thread_model: Option<String>,
    settings: Settings,
    /// The models `model/list` named, once it has answered (none for a server
    /// that has no such method).
    models: Vec<ModelOption>,
    rate_limits: super::rate_limits::CodexRates,
    rate_notification_generation: u64,
    account_notification_generation: u64,
    /// The Fast tier has been sent on this thread. The tier sticks, so a model
    /// that is not known to have tiers still gets the standard one back.
    tier_sent: bool,
    /// The effort and model of the last dropped effort, so one notice says it.
    effort_dropped: Option<(String, String)>,
    /// Whether the thread was last put in plan collaboration mode.
    plan_sent: bool,
    state: ChatState,
    /// The handshake is done; process failures are events from here on.
    ready: bool,
    /// The turn in progress, once its id is known.
    turn: Option<String>,
    /// A `turn/start` is in flight and its turn id is not known yet.
    starting: bool,
    start_serial: u64,
    /// Completions observed during this start dispatch, including before its
    /// receipt. Bounded; overflow makes the receipt uncertain rather than reopening.
    completed_before_receipt: HashSet<String>,
    completion_overflow: bool,
    /// Messages sent before the turn id was known, steered in once it is.
    queued: Vec<String>,
    interrupt_wanted: bool,
    /// An interrupt was sent for the open turn: messages wait for the next one.
    interrupting: bool,
    last_finished_turn: Option<String>,
    /// Whether an `error` notification already told the user about this turn.
    error_shown: bool,
    requests: Vec<OpenRequest>,
    streamed: HashMap<String, Streamed>,
    file_changes: HashMap<String, Vec<FileChange>>,
    notices: u64,
    open_notices: HashMap<String, Item>,
    reconnecting: HashMap<String, Item>,
    /// Keeps this session's notice ids apart from earlier sessions' in the
    /// chat's log, where an equal id would replace the older notice.
    notice_prefix: String,
    /// `shutdown` is under way: the process ending is expected.
    stopping: bool,
    /// Nothing more will happen: stopped or failed.
    finished: bool,
}

impl Session {
    fn new(config: &DriverConfig, events: Sender<ChatEvent>) -> Self {
        Self {
            identity_config: config.clone(),
            identity: super::account_identity::for_config(config),
            events,
            next_request: 0,
            waiting: HashMap::new(),
            thread_id: config.resume.clone(),
            thread_model: None,
            settings: Settings {
                model: config.model.clone(),
                effort: config.effort.clone(),
                mode: config.approval_mode,
                fast: config.fast,
            },
            models: Vec::new(),
            rate_limits: Default::default(),
            rate_notification_generation: 0,
            account_notification_generation: 0,
            tier_sent: false,
            effort_dropped: None,
            plan_sent: false,
            state: ChatState::Starting,
            ready: false,
            turn: None,
            starting: false,
            start_serial: 0,
            completed_before_receipt: HashSet::new(),
            completion_overflow: false,
            queued: Vec::new(),
            interrupt_wanted: false,
            interrupting: false,
            last_finished_turn: None,
            error_shown: false,
            requests: Vec::new(),
            streamed: HashMap::new(),
            file_changes: HashMap::new(),
            notices: 0,
            open_notices: config.outstanding_notices.clone(),
            reconnecting: HashMap::new(),
            notice_prefix: Uuid::new_v4().simple().to_string()[..8].to_owned(),
            stopping: false,
            finished: false,
        }
    }

    fn emit(&self, event: ChatEvent) {
        let _ = self.events.send(event);
    }

    /// Register `reply` for a new request and return the frame to send.
    fn request(&mut self, method: &str, params: Value, reply: Reply) -> Value {
        self.next_request += 1;
        self.waiting.insert(self.next_request, reply);
        json!({"id": self.next_request, "method": method, "params": params})
    }

    fn set_state(&mut self, state: ChatState) {
        if self.state != state {
            self.state = state.clone();
            self.emit(ChatEvent::State { state });
        }
    }

    /// `Running` or `Waiting` while a turn is open, otherwise `Idle`.
    fn refresh_state(&mut self) {
        if self.finished || !self.ready {
            return;
        }
        let state = if self.turn.is_none() && !self.starting {
            ChatState::Idle
        } else if self.requests.iter().all(|open| !open.blocking) {
            ChatState::Running
        } else {
            ChatState::Waiting
        };
        self.set_state(state);
    }

    fn notice(&mut self, level: NoticeLevel, kind: &str, text: String) {
        self.notices += 1;
        let id = format!("notice-{}-{}", self.notice_prefix, self.notices);
        self.notice_with_id(id, level, kind, text, self.turn.clone());
    }

    /// A notice that a later one with the same id replaces.
    fn notice_with_id(
        &mut self,
        id: String,
        level: NoticeLevel,
        kind: &str,
        text: String,
        turn_id: Option<String>,
    ) -> Item {
        let mut body = ItemBody::notice(level, text, Some(kind));
        if kind == "rate_limit:codex" {
            if let ItemBody::Notice { resets_at, .. } = &mut body {
                *resets_at = self.rate_limits.blocking_reset();
            }
        }
        let item = Item {
            presentation: Default::default(),
            id,
            turn_id,
            status: ItemStatus::Completed,
            body,
        };
        if matches!(kind, notice_kind::AUTH_REQUIRED | "rate_limit:codex") {
            self.open_notices.insert(kind.to_owned(), item.clone());
        }
        self.emit(ChatEvent::ItemCompleted { item: item.clone() });
        item
    }

    fn resolve_item(&self, mut item: Item, text: &str) {
        if let ItemBody::Notice {
            resolved,
            text: message,
            level,
            ..
        } = &mut item.body
        {
            *resolved = true;
            *message = text.to_owned();
            *level = NoticeLevel::Info;
        }
        self.emit(ChatEvent::ItemCompleted { item });
    }

    fn resolve_notice(&mut self, kind: &str, text: &str) {
        if let Some(item) = self.open_notices.remove(kind) {
            self.resolve_item(item, text);
        }
    }

    fn resolve_reconnecting(&mut self, turn: &str) {
        if let Some(item) = self.reconnecting.remove(turn) {
            self.resolve_item(item, "Reconnected.");
        }
    }

    /// Open `turn_id` unless it is open or over already. Returns the frames to
    /// send: queued messages steered in, a wanted interrupt.
    fn begin_turn(&mut self, turn_id: &str) -> Vec<Value> {
        let mut frames = Vec::new();
        if turn_id.trim().is_empty() {
            return frames;
        }
        // A delayed receipt for an old turn must not drain its queued text into a
        // newer turn. Completion can also precede the start receipt.
        if self.last_finished_turn.as_deref() == Some(turn_id)
            || self.completed_before_receipt.contains(turn_id)
        {
            if self.turn.is_none() {
                self.starting = false;
                self.refresh_state();
            }
            return frames;
        }
        if self
            .turn
            .as_deref()
            .is_some_and(|current| current != turn_id)
        {
            return frames;
        }
        // Both the `turn/start` result and `turn/started` come here, and either
        // can arrive after the turn is over.
        if self.turn.is_none() && self.last_finished_turn.as_deref() != Some(turn_id) {
            self.starting = false;
            self.turn = Some(turn_id.to_owned());
            self.error_shown = false;
            self.interrupting = false;
            self.emit(ChatEvent::TurnStarted {
                turn_id: turn_id.to_owned(),
            });
        }
        if let (Some(thread), Some(turn)) = (self.thread_id.clone(), self.turn.clone()) {
            for text in std::mem::take(&mut self.queued) {
                frames.push(self.steer_frame(&thread, &turn, text));
            }
            if std::mem::take(&mut self.interrupt_wanted) {
                frames.push(self.request(
                    "turn/interrupt",
                    json!({"threadId": thread, "turnId": turn}),
                    Box::new(|_, _| {}),
                ));
            }
        }
        self.refresh_state();
        frames
    }

    fn steer_frame(&mut self, thread: &str, turn: &str, text: String) -> Value {
        let params = json!({
            "threadId": thread,
            "expectedTurnId": turn,
            "input": text_input(&text),
        });
        let turn = turn.to_owned();
        self.request(
            "turn/steer",
            params,
            Box::new(move |codex, outcome| {
                if outcome.is_ok() {
                    return;
                }
                // The turn may have ended just as the message was sent: then
                // the message starts the next one.
                let ended = codex.with(|session| session.turn.as_deref() != Some(turn.as_str()));
                if ended {
                    let _ = codex.send_text(text);
                } else {
                    // Codex refuses steering at times (while compacting, or
                    // as the turn ends): the message starts the next turn.
                    codex.with(|session| session.queued.push(text));
                }
            }),
        )
    }

    /// The model the next turn runs: the chosen one, else the thread's, else the
    /// provider's default.
    fn effective_model(&self) -> Option<String> {
        self.settings
            .model
            .clone()
            .or_else(|| self.thread_model.clone())
            .or_else(|| {
                self.models
                    .iter()
                    .find(|model| model.is_default)
                    .map(|model| model.id.clone())
            })
    }

    /// What `model/list` said about `model`, once it has answered.
    fn model_option(&self, model: Option<&str>) -> Option<&ModelOption> {
        let model = model?;
        self.models.iter().find(|option| option.id == model)
    }

    /// Whether `model` takes `effort`: yes for a model the list does not
    /// describe, or describes without efforts.
    fn takes_effort(&self, model: Option<&str>, effort: &str) -> bool {
        self.model_option(model).is_none_or(|option| {
            option.efforts.is_empty() || option.efforts.iter().any(|e| e == effort)
        })
    }

    /// The effort to send: the chosen one if the model takes it. One it does not
    /// take is left out, and a notice says so once.
    fn effort_to_send(&mut self) -> Option<String> {
        let effort = self.settings.effort.clone()?;
        let model = self.effective_model();
        if self.takes_effort(model.as_deref(), &effort) {
            return Some(effort);
        }
        let model = model.unwrap_or_default();
        let dropped = (model.clone(), effort.clone());
        if self.effort_dropped.as_ref() != Some(&dropped) {
            self.effort_dropped = Some(dropped);
            self.notice(
                NoticeLevel::Warning,
                notice_kind::EFFORT_REFUSED,
                format!(
                    "{model} does not take the reasoning effort {effort}; the model's own is used."
                ),
            );
        }
        None
    }

    /// Remember the models the server offers and tell the chat. A list with nothing
    /// in it says nothing, and the same list as before is not said again.
    fn set_models(&mut self, models: Vec<ModelOption>) {
        if models.is_empty() || models == self.models {
            return;
        }
        self.models = models.clone();
        self.emit(ChatEvent::Models { models });
    }

    /// The service tier to send with a turn, `None` to leave the field out. A model
    /// with a Fast tier gets the tier the user chose every time (the server keeps
    /// what a turn sets, so the standard tier has to be said); a model without
    /// one gets nothing, and a model the list does not describe gets Fast only
    /// when it was asked for.
    fn service_tier(&mut self) -> Option<&'static str> {
        let model = self.effective_model();
        let tier = match self.model_option(model.as_deref()).map(|m| m.supports_fast) {
            Some(true) => Some(if self.settings.fast {
                FAST_TIER
            } else {
                STANDARD_TIER
            }),
            Some(false) => None,
            None if self.settings.fast => Some(FAST_TIER),
            None => self.tier_sent.then_some(STANDARD_TIER),
        };
        self.tier_sent = tier == Some(FAST_TIER);
        tier
    }

    /// The parameters of a `turn/start`, with the settings in force.
    fn turn_params(&mut self, thread: &str, text: &str) -> Value {
        let policy = policy(self.settings.mode);
        let mut params = json!({
            "threadId": thread,
            "input": text_input(text),
            "approvalPolicy": policy.approval,
            "sandboxPolicy": policy.sandbox,
            "summary": "auto",
        });
        if let Some(model) = &self.settings.model {
            params["model"] = json!(model);
        }
        let effort = self.effort_to_send();
        if let Some(effort) = &effort {
            params["effort"] = json!(effort);
        }
        if let Some(tier) = self.service_tier() {
            params["serviceTier"] = json!(tier);
        }
        // The collaboration mode sticks to the thread: Plan sets it, and the
        // first turn after Plan puts it back.
        if policy.plan || self.plan_sent {
            let model = self.effective_model();
            if let Some(model) = model {
                params["collaborationMode"] = json!({
                    "mode": if policy.plan { "plan" } else { "default" },
                    "settings": {
                        "model": model,
                        "reasoning_effort": effort,
                        "developer_instructions": policy.plan.then_some(PLAN_INSTRUCTIONS),
                    },
                });
                self.plan_sent = policy.plan;
            }
        }
        params
    }

    /// The end of the turn `turn_id`; frames to send for messages still queued.
    fn finish_turn(&mut self, turn_id: &str, outcome: TurnOutcome) -> Vec<Value> {
        if turn_id.trim().is_empty() {
            return Vec::new();
        }
        if turn_id.len() > 512 {
            self.completion_overflow = true;
        } else if self.completed_before_receipt.len() < 64 {
            self.completed_before_receipt.insert(turn_id.to_owned());
        } else if !self.completed_before_receipt.contains(turn_id) {
            self.completion_overflow = true;
        }
        let repeated = self.last_finished_turn.as_deref() == Some(turn_id);
        self.last_finished_turn = Some(turn_id.to_owned());
        self.resolve_reconnecting(turn_id);
        // A completion can precede the turn/start receipt. It still proves recovery.
        if matches!(outcome, TurnOutcome::Completed)
            && (self.turn.as_deref() == Some(turn_id) || (self.turn.is_none() && !repeated))
        {
            self.resolve_notice("rate_limit:codex", "Codex can answer again.");
            self.resolve_notice(notice_kind::AUTH_REQUIRED, "Codex is signed in again.");
        }
        if self.turn.as_deref() != Some(turn_id) {
            return Vec::new();
        }
        self.turn = None;
        self.starting = false;
        self.interrupting = false;
        self.requests.clear();
        self.file_changes.clear();
        self.streamed.clear();
        self.interrupt_wanted = false;
        if let TurnOutcome::Failed { message } = &outcome
            && !self.error_shown
        {
            self.notice(
                NoticeLevel::Error,
                notice_kind::TURN_FAILED,
                message.clone(),
            );
        }
        self.emit(ChatEvent::TurnCompleted {
            turn_id: turn_id.to_owned(),
            outcome,
        });
        // Messages that came too late to steer start the next turn.
        let queued = std::mem::take(&mut self.queued);
        if let (false, Some(thread)) = (queued.is_empty(), self.thread_id.clone()) {
            return vec![self.start_frame(&thread, queued.join("\n\n"))];
        }
        self.refresh_state();
        Vec::new()
    }

    /// A `turn/start` for `text`, marking the chat busy.
    fn start_frame(&mut self, thread: &str, text: String) -> Value {
        self.starting = true;
        self.start_serial += 1;
        self.completed_before_receipt.clear();
        self.completion_overflow = false;
        let serial = self.start_serial;
        self.refresh_state();
        let params = self.turn_params(thread, &text);
        self.request(
            "turn/start",
            params,
            Box::new(move |codex, outcome| match outcome {
                Ok(result) => {
                    if let Some(turn_id) = result["turn"]["id"].as_str() {
                        let frames = codex.with(|session| {
                            if session.start_serial == serial {
                                session.begin_turn(turn_id)
                            } else {
                                Vec::new()
                            }
                        });
                        codex.send_all(frames);
                    }
                }
                Err(message) => codex.with(|session| {
                    if session.start_serial != serial {
                        return;
                    }
                    session.starting = false;
                    session.queued.clear();
                    session.interrupt_wanted = false;
                    session.notice(
                        NoticeLevel::Error,
                        notice_kind::SETTING_REFUSED,
                        format!("Codex did not start the turn: {message}"),
                    );
                    session.refresh_state();
                }),
            }),
        )
    }

    /// The chat has stopped on purpose.
    fn finish_stopped(&mut self) {
        self.finished = true;
        self.waiting.clear();
        self.requests.clear();
        if let Some(turn) = self.turn.take() {
            self.emit(ChatEvent::TurnCompleted {
                turn_id: turn,
                outcome: TurnOutcome::Interrupted,
            });
        }
        if !matches!(self.state, ChatState::Failed { .. }) {
            self.set_state(ChatState::Stopped);
        }
    }

    /// The process ended by itself.
    fn finish_failed(&mut self, message: String) {
        self.finished = true;
        self.waiting.clear();
        self.requests.clear();
        if let Some(turn) = self.turn.take() {
            self.emit(ChatEvent::TurnCompleted {
                turn_id: turn,
                outcome: TurnOutcome::Failed {
                    message: message.clone(),
                },
            });
        }
        self.set_state(ChatState::Failed { message });
    }
}

impl Codex {
    fn with<R>(&self, f: impl FnOnce(&mut Session) -> R) -> R {
        f(&mut self.session.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// Write frames after the session lock is released. (A message larger than
    /// the pipe can still hold the reader up while Codex is not reading; stopping
    /// the chat ends that.)
    fn send_all(&self, frames: Vec<Value>) {
        for frame in frames {
            if let Err(error) = self.proc.send(&frame) {
                self.with(|session| {
                    if !session.stopping && !session.finished {
                        // A `turn/start` that never went out leaves nothing running.
                        if session.turn.is_none() {
                            session.starting = false;
                        }
                        session.notice(NoticeLevel::Error, notice_kind::SETTING_REFUSED, error);
                        session.refresh_state();
                    }
                });
                break;
            }
        }
    }

    /// A request whose answer the caller waits for (the handshake).
    fn call(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let (answer, wait) = mpsc::channel();
        let frame = self.with(|session| {
            session.request(
                method,
                params,
                Box::new(move |_, outcome| {
                    let _ = answer.send(outcome);
                }),
            )
        });
        if self.proc.send(&frame).is_err() {
            return Err(format!(
                "codex {}",
                self.proc.describe_exit(Duration::from_secs(2))
            ));
        }
        match wait.recv_timeout(timeout) {
            Ok(outcome) => outcome.map_err(|error| format!("{method}: {error}")),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(format!(
                "codex {}",
                self.proc.describe_exit(Duration::from_secs(2))
            )),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(format!(
                "codex did not answer {method} within {} seconds",
                timeout.as_secs()
            )),
        }
    }

    fn handshake(&self, config: &DriverConfig) -> Result<(), String> {
        self.call(
            "initialize",
            json!({
                "clientInfo": {
                    "name": "riwork",
                    "title": "RiWork",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "capabilities": {
                    "experimentalApi": true,
                    "optOutNotificationMethods": OPT_OUT,
                },
            }),
            INITIALIZE_TIMEOUT,
        )?;
        self.proc.send(&json!({"method": "initialized"}))?;
        let policy = policy(config.approval_mode);
        let mut params = json!({
            "cwd": config.cwd.to_string_lossy(),
            "approvalPolicy": policy.approval,
            "sandbox": policy.sandbox_mode,
        });
        if let Some(model) = &config.model {
            params["model"] = json!(model);
        }
        if config.fast {
            params["serviceTier"] = json!(FAST_TIER);
        }
        // The conversation a chat had before it switched to Codex, which no Codex thread
        // holds. Sent on every start and resume, as a Claude chat's system prompt is.
        if let Some(instructions) = config
            .instructions
            .as_ref()
            .filter(|text| !text.trim().is_empty())
        {
            params["developerInstructions"] = json!(instructions);
        }
        let mut replaced = None;
        let result = match &config.resume {
            Some(thread) => {
                let mut resume = params.clone();
                resume["threadId"] = json!(thread);
                resume["excludeTurns"] = json!(true);
                match self.call("thread/resume", resume, THREAD_TIMEOUT) {
                    Ok(result) => result,
                    // A thread that never ran a turn has nothing saved to resume,
                    // and there is nothing in it to lose: open a new one.
                    Err(error) if error.contains("no rollout found") => {
                        replaced = Some(thread.clone());
                        self.call("thread/start", params, THREAD_TIMEOUT)?
                    }
                    Err(error) => return Err(error),
                }
            }
            None => self.call("thread/start", params, THREAD_TIMEOUT)?,
        };
        let thread = result["thread"]["id"]
            .as_str()
            .ok_or("codex did not name the thread")?
            .to_owned();
        self.with(|session| {
            session.thread_id = Some(thread);
            // A resumed thread brings the name it was given.
            if let Some(title) = result["thread"]["name"]
                .as_str()
                .filter(|t| !t.trim().is_empty())
            {
                session.emit(ChatEvent::ProviderTitle {
                    title: title.to_owned(),
                });
            }
            session.thread_model = result["model"].as_str().map(str::to_owned);
            session.tier_sent = config.fast;
            // A resumed thread may still be in the plan mode it was left in.
            session.plan_sent = result["collaborationMode"]["mode"] == "plan";
            session.ready = true;
            if let Some(old) = replaced {
                session.notice(
                    NoticeLevel::Info, notice_kind::RESUMED_FRESH,
                    format!("Codex had no saved thread {old} to resume, so this chat continues in a new thread."),
                );
            }
            session.refresh_state();
        });
        // The chat does not wait for the list: it arrives as an event.
        self.list_models(None, Vec::new(), 1);
        self.read_rate_limits();
        self.read_account_identity();
        Ok(())
    }

    fn read_account_identity(&self) {
        let frame = self.with(Session::account_request);
        let _ = self.proc.send(&frame);
    }

    /// Background quota read: unsupported methods never hold up starting a chat.
    fn read_rate_limits(&self) {
        let frame = self.with(|session| {
            let generation = session.rate_notification_generation;
            session.request(
                "account/rateLimits/read",
                Value::Null,
                Box::new(move |codex, outcome| {
                    if let Ok(result) = outcome {
                        codex.with(|session| session.apply_rate_read(generation, &result));
                    }
                }),
            )
        });
        let _ = self.proc.send(&frame);
    }

    /// Ask for page number `page` of `model/list` (from `cursor`, after the models
    /// already in `listed`) and, at the last page, tell the chat about the models.
    /// A server without the method, or one that fails it, leaves the chat without a
    /// list; it still works, with free text for the model.
    fn list_models(&self, cursor: Option<String>, listed: Vec<ModelOption>, page: usize) {
        let params = match &cursor {
            Some(cursor) => json!({"cursor": cursor}),
            None => json!({}),
        };
        let frame = self.with(|session| {
            session.request(
                "model/list",
                params,
                Box::new(move |codex, outcome| {
                    let Ok(result) = outcome else { return };
                    let mut listed = listed;
                    listed.extend(model_options(&result["data"]));
                    match result["nextCursor"].as_str() {
                        Some(next) if page < MODEL_PAGES => {
                            codex.list_models(Some(next.to_owned()), listed, page + 1)
                        }
                        _ => codex.with(|session| session.set_models(listed)),
                    }
                }),
            )
        });
        // A failure to write is the process going away, which the reader sees.
        let _ = self.proc.send(&frame);
    }

    fn read_loop(&self, stdout: std::process::ChildStdout) {
        let mut reader = FrameReader::new(stdout, MAX_FRAME_BYTES);
        loop {
            match reader.next_frame() {
                Ok(Some(Frame::Line(line))) => {
                    if let Ok(frame) = serde_json::from_slice::<Value>(&line) {
                        self.on_frame(frame);
                    }
                }
                Ok(Some(Frame::Oversized(bytes))) => self.with(|session| {
                    session.notice(
                        NoticeLevel::Warning,
                        notice_kind::OVERSIZED_LINE,
                        format!(
                            "Skipped an oversized message from codex ({} MB).",
                            bytes.div_ceil(1_000_000)
                        ),
                    )
                }),
                Ok(None) | Err(_) => break,
            }
        }
        self.on_exit();
    }

    fn on_exit(&self) {
        let stopping = self.with(|session| session.stopping);
        let message = if stopping {
            String::new()
        } else {
            format!("codex {}", self.proc.describe_exit(Duration::from_secs(2)))
        };
        self.with(|session| {
            // Nobody will answer these now. Dropping them closes the channels
            // the handshake waits on.
            session.waiting.clear();
            if !session.stopping && session.ready {
                session.finish_failed(message.clone());
            }
        });
    }

    fn on_frame(&self, frame: Value) {
        let method = frame.get("method").and_then(Value::as_str);
        match (method, frame.get("id")) {
            (Some(method), Some(id)) => self.on_request(id.clone(), method, &frame["params"]),
            (Some(method), None) => self.on_notification(method, &frame["params"]),
            (None, Some(id)) => {
                let Some(id) = id.as_i64() else { return };
                let outcome = match frame.get("error") {
                    Some(error) => Err(error["message"]
                        .as_str()
                        .unwrap_or("request failed")
                        .to_owned()),
                    None => Ok(frame["result"].clone()),
                };
                let reply = self.with(|session| session.waiting.remove(&id));
                if let Some(reply) = reply {
                    reply(self, outcome);
                }
            }
            (None, None) => {}
        }
    }

    // ---- Commands ----

    fn command(&self, command: ChatCommand) -> Result<(), String> {
        if self.with(|session| session.finished) {
            return Err("the Codex process is not running".into());
        }
        match command {
            // Another provider is another process: the host does that.
            ChatCommand::Switch { .. } => Err("the chat host switches providers".into()),
            ChatCommand::Send { text } => self.send_text(text),
            ChatCommand::SendAttachments { text, attachments } => {
                let input = super::attachments::inputs(&text, &attachments, false)?;
                self.send_attachments(input)
            }
            ChatCommand::Interrupt => self.interrupt(),
            ChatCommand::Approve {
                request_id,
                decision,
            } => self.approve(&request_id, decision),
            ChatCommand::Answer {
                request_id,
                answers,
            } => self.answer(&request_id, answers),
            ChatCommand::Configure {
                model,
                effort,
                approval_mode,
                fast,
            } => {
                self.with(|session| {
                    if model.is_some() {
                        session.settings.model = model;
                    }
                    // Against the model just chosen, if there is a new one.
                    if let Some(effort) = effort {
                        let chosen = session.effective_model();
                        if session.takes_effort(chosen.as_deref(), &effort) {
                            session.settings.effort = Some(effort);
                        } else {
                            let chosen = chosen.unwrap_or_default();
                            session.notice(
                                NoticeLevel::Warning,
                                notice_kind::EFFORT_REFUSED,
                                format!(
                                    "{chosen} does not take the reasoning effort {effort}, \
                                     so it was not changed."
                                ),
                            );
                        }
                    }
                    if let Some(mode) = approval_mode {
                        session.settings.mode = mode;
                    }
                    if let Some(fast) = fast {
                        session.settings.fast = fast;
                    }
                });
                Ok(())
            }
            ChatCommand::Compact => {
                let thread = self.thread()?;
                let frame = self.with(|session| {
                    session.request(
                        "thread/compact/start",
                        json!({"threadId": thread}),
                        Box::new(|codex, outcome| {
                            if let Err(message) = outcome {
                                codex.with(|session| {
                                    session.notice(
                                        NoticeLevel::Error,
                                        notice_kind::SETTING_REFUSED,
                                        format!("Codex could not compact the context: {message}"),
                                    )
                                });
                            }
                        }),
                    )
                });
                self.proc.send(&frame)
            }
            // Handled by the driver, which owns the process.
            ChatCommand::Stop => Ok(()),
            ChatCommand::DismissNotice { .. } => Err("notice dismissal belongs to the host".into()),
        }
    }

    fn thread(&self) -> Result<String, String> {
        self.with(|session| session.thread_id.clone())
            .ok_or_else(|| "the Codex thread is not open".to_owned())
    }

    fn send_text(&self, text: String) -> Result<(), String> {
        let thread = self.thread()?;
        let frame = self.with(|session| {
            if session.interrupting || (session.turn.is_none() && session.starting) {
                // Not for the turn that is ending, nor before it has an id.
                session.queued.push(text);
                None
            } else if let Some(turn) = session.turn.clone() {
                Some(session.steer_frame(&thread, &turn, text))
            } else {
                Some(session.start_frame(&thread, text))
            }
        });
        let Some(frame) = frame else { return Ok(()) };
        let sent = self.proc.send(&frame);
        if sent.is_err() {
            self.with(|session| {
                session.starting = false;
                session.refresh_state();
            });
        }
        sent
    }

    /// Unlike legacy text steering, attachment submission has no implicit retry/queue.
    /// A reply acknowledges submission; the separate turn events describe completion.
    fn send_attachments(&self, input: Value) -> Result<(), String> {
        let deadline = Instant::now() + child::WRITE_WAIT;
        let thread = self.thread()?;
        let (answer, wait) = mpsc::channel();
        let frame = self.with(|session| {
            if session.finished || session.interrupting || session.starting {
                return Err("Codex is starting or interrupting a turn; keep the draft and send after it settles".to_owned());
            }
            let expected_turn = session.turn.clone();
            let (method, params, starting) = if let Some(turn) = &session.turn {
                ("turn/steer", json!({"threadId":thread,"expectedTurnId":turn,"input":input}), false)
            } else {
                let mut params = session.turn_params(&thread, "");
                params["input"] = input;
                session.starting = true;
                session.start_serial += 1;
                session.completed_before_receipt.clear();
                session.completion_overflow = false;
                session.refresh_state();
                ("turn/start", params, true)
            };
            let serial = session.start_serial;
            Ok(session.request(method, params, Box::new(move |codex, outcome| {
                let result = match outcome {
                    Ok(value) => {
                        if starting {
                            if let Some(id) = value["turn"]["id"].as_str().filter(|id| !id.trim().is_empty()) {
                                let frames = codex.with(|s| {
                                    if s.finished || s.start_serial != serial || s.completion_overflow || s.turn.as_deref().is_some_and(|current| current != id) {
                                        Err(format!("{} Codex start receipt arrived after another lifecycle; inspect the transcript before resending", super::attachments::UNKNOWN_SUBMISSION))
                                    } else { Ok(s.begin_turn(id)) }
                                });
                                frames.map(|frames| codex.send_all(frames))
                            } else {
                                Err(format!("{} Codex acknowledged a turn without its id; inspect the transcript before resending", super::attachments::UNKNOWN_SUBMISSION))
                            }
                        } else if value["turnId"].as_str() == expected_turn.as_deref() {
                            Ok(())
                        } else {
                            Err(format!("{} Codex steering receipt did not identify the expected turn; inspect the transcript before resending", super::attachments::UNKNOWN_SUBMISSION))
                        }
                    }
                    Err(message) => {
                        if starting {
                            codex.with(|s| {
                                if s.start_serial != serial || s.turn.is_some() {
                                    Err(format!("{} Codex refusal arrived after a different lifecycle ({message}); inspect the transcript before resending", super::attachments::UNKNOWN_SUBMISSION))
                                } else {
                                    s.starting = false; s.refresh_state(); Err(message)
                                }
                            })
                        } else { Err(message) }
                    }
                };
                let _ = answer.send(result);
            })))
        })?;
        // Even a partial pipe write can have submitted the request. Never retry it.
        if let Err(error) = self.proc.send_line_until(&frame.to_string(), deadline) {
            // No bytes means no submission. Remove its callback so a delayed unrelated
            // response cannot turn that known refusal into an accepted dispatch.
            if error.written == 0 {
                self.with(|s| {
                    if let Some(id) = frame["id"].as_i64() {
                        s.waiting.remove(&id);
                    }
                    if frame["method"] == "turn/start" {
                        s.starting = false;
                        s.refresh_state();
                    }
                });
                return Err(error.to_string());
            }
            return Err(format!(
                "{} {error}; inspect the transcript before resending",
                super::attachments::UNKNOWN_SUBMISSION
            ));
        }
        loop {
            if let Ok(result) = wait.try_recv() {
                return result;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() || self.proc.writes_cancelled() {
                return Err(format!(
                    "{} Codex submission reply timed out or was cancelled; inspect the transcript before resending",
                    super::attachments::UNKNOWN_SUBMISSION
                ));
            }
            match wait.recv_timeout(remaining.min(Duration::from_millis(20))) {
                Ok(result) => return result,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(error) => {
                    return Err(format!(
                        "{} Codex submission reply lost ({error}); inspect the transcript before resending",
                        super::attachments::UNKNOWN_SUBMISSION
                    ));
                }
            }
        }
    }

    fn interrupt(&self) -> Result<(), String> {
        let thread = self.thread()?;
        let frame = self.with(|session| match session.turn.clone() {
            Some(turn) => {
                session.interrupting = true;
                Some(session.request(
                    "turn/interrupt",
                    json!({"threadId": thread, "turnId": turn}),
                    Box::new(|_, _| {}),
                ))
            }
            None => {
                session.interrupt_wanted = session.starting;
                None
            }
        });
        match frame {
            Some(frame) => self.proc.send(&frame),
            None => Ok(()),
        }
    }

    /// Take the open request `request_id`: a question if `question`, else an
    /// approval.
    fn take_request(&self, request_id: &str, question: bool) -> Result<OpenRequest, String> {
        self.with(|session| {
            let at = session
                .requests
                .iter()
                .position(|open| open.request_id == request_id)
                .ok_or_else(|| format!("no open request {request_id}"))?;
            if matches!(session.requests[at].kind, RequestKind::Question { .. }) != question {
                return Err(format!(
                    "{request_id} is {}",
                    if question {
                        "an approval"
                    } else {
                        "a question"
                    }
                ));
            }
            Ok(session.requests.remove(at))
        })
    }

    fn approve(&self, request_id: &str, decision: Decision) -> Result<(), String> {
        let open = self.take_request(request_id, false)?;
        let (decision, result) = match &open.kind {
            RequestKind::Command { choices } | RequestKind::FileChange { choices } => {
                let decision = pick(decision, choices);
                (decision, json!({"decision": wire_decision(decision)}))
            }
            RequestKind::Permissions { requested } => {
                let granted = matches!(decision, Decision::Accept | Decision::AcceptForSession);
                let scope = if decision == Decision::AcceptForSession {
                    "session"
                } else {
                    "turn"
                };
                let permissions = if granted {
                    requested.clone()
                } else {
                    json!({})
                };
                (
                    decision,
                    json!({"permissions": permissions, "scope": scope}),
                )
            }
            RequestKind::Question { .. } => return Ok(()),
        };
        self.with(|session| {
            session.emit(ChatEvent::ApprovalResolved {
                request_id: open.request_id.clone(),
                decision,
            });
            session.refresh_state();
        });
        let sent = self
            .proc
            .send(&json!({"id": open.rpc_id, "result": result}));
        // A permission request has no way to cancel the turn; stop it ourselves.
        if decision == Decision::Cancel && matches!(open.kind, RequestKind::Permissions { .. }) {
            let _ = self.interrupt();
        }
        sent
    }

    fn answer(&self, request_id: &str, answers: Vec<Vec<String>>) -> Result<(), String> {
        let open = self.take_request(request_id, true)?;
        let RequestKind::Question { ids } = &open.kind else {
            return Ok(());
        };
        let mut answers = answers.into_iter();
        let by_question: serde_json::Map<String, Value> = ids
            .iter()
            .map(|id| {
                (
                    id.clone(),
                    json!({"answers": answers.next().unwrap_or_default()}),
                )
            })
            .collect();
        self.with(|session| {
            session.emit(ChatEvent::QuestionResolved {
                request_id: open.request_id.clone(),
            });
            session.refresh_state();
        });
        self.proc
            .send(&json!({"id": open.rpc_id, "result": {"answers": by_question}}))
    }

    // ---- Requests from Codex ----

    fn on_request(&self, id: Value, method: &str, params: &Value) {
        let request_id = match &id {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        let reply = match method {
            "item/commandExecution/requestApproval" => {
                let choices = command_choices(params);
                let command = params["command"].as_str().map(display_command).or_else(|| {
                    (params["kind"] == "writeStdin")
                        .then(|| "Send input to a running command".to_owned())
                });
                let network = params["networkApprovalContext"]["host"]
                    .as_str()
                    .map(|host| format!("Network access to {host}"));
                let title = command
                    .or(network)
                    .or_else(|| params["reason"].as_str().map(str::to_owned))
                    .unwrap_or_else(|| "Run a command".to_owned());
                let mut detail = Vec::new();
                if let Some(reason) = params["reason"].as_str() {
                    detail.push(reason.to_owned());
                }
                if let Some(cwd) = params["cwd"].as_str() {
                    detail.push(format!("in {cwd}"));
                }
                let approval = Approval {
                    request_id,
                    item_id: params["itemId"].as_str().map(str::to_owned),
                    kind: ApprovalKind::Command,
                    title,
                    detail: detail.join("\n"),
                    choices: choices.clone(),
                };
                self.open_request(id, approval, RequestKind::Command { choices });
                return;
            }
            "item/fileChange/requestApproval" => {
                let choices = vec![
                    Decision::Accept,
                    Decision::AcceptForSession,
                    Decision::Decline,
                    Decision::Cancel,
                ];
                let changes = params["itemId"]
                    .as_str()
                    .and_then(|item| self.with(|session| session.file_changes.get(item).cloned()))
                    .unwrap_or_default();
                let title = match (changes.as_slice(), params["grantRoot"].as_str()) {
                    ([], Some(root)) => format!("Write under {root}"),
                    ([], None) => "Change files".to_owned(),
                    ([one], _) => format!("Change {}", one.path),
                    (many, _) => format!("Change {} files", many.len()),
                };
                let mut detail = params["reason"].as_str().unwrap_or_default().to_owned();
                for change in &changes {
                    if let Some(diff) = &change.diff {
                        detail.push_str(&format!("\n\n{}\n{diff}", change.path));
                    }
                }
                truncate(&mut detail, DETAIL_BYTES);
                let approval = Approval {
                    request_id,
                    item_id: params["itemId"].as_str().map(str::to_owned),
                    kind: ApprovalKind::FileChange,
                    title,
                    detail: detail.trim().to_owned(),
                    choices: choices.clone(),
                };
                self.open_request(id, approval, RequestKind::FileChange { choices });
                return;
            }
            "item/permissions/requestApproval" => {
                let requested = params["permissions"].clone();
                let mut detail = params["reason"].as_str().unwrap_or_default().to_owned();
                let described = serde_json::to_string_pretty(&requested).unwrap_or_default();
                if !detail.is_empty() {
                    detail.push_str("\n\n");
                }
                detail.push_str(&described);
                let approval = Approval {
                    request_id,
                    item_id: params["itemId"].as_str().map(str::to_owned),
                    kind: ApprovalKind::Permissions,
                    title: "Grant more permissions".to_owned(),
                    detail,
                    choices: vec![
                        Decision::Accept,
                        Decision::AcceptForSession,
                        Decision::Decline,
                    ],
                };
                self.open_request(id, approval, RequestKind::Permissions { requested });
                return;
            }
            "item/tool/requestUserInput" => {
                let ids: Vec<String> = params["questions"]
                    .as_array()
                    .map(|questions| {
                        questions
                            .iter()
                            .map(|q| q["id"].as_str().unwrap_or_default().to_owned())
                            .collect()
                    })
                    .unwrap_or_default();
                let prompts = params["questions"]
                    .as_array()
                    .map(|questions| questions.iter().map(question_prompt).collect())
                    .unwrap_or_default();
                // A question that does not block leaves the turn running.
                let blocking = params["isBlocking"].as_bool().unwrap_or(true);
                self.with(|session| {
                    session.requests.push(OpenRequest {
                        request_id: request_id.clone(),
                        rpc_id: id,
                        kind: RequestKind::Question { ids },
                        blocking,
                    });
                    session.emit(ChatEvent::QuestionRequested {
                        question: Question {
                            request_id,
                            questions: prompts,
                        },
                    });
                    session.refresh_state();
                });
                return;
            }
            "mcpServer/elicitation/request" => {
                self.with(|session| {
                    session.notice(
                        NoticeLevel::Warning,
                        notice_kind::MCP_ELICITATION,
                        "An MCP server asked for input that chats cannot take yet; declined."
                            .to_owned(),
                    )
                });
                json!({"id": id, "result": {"action": "decline"}})
            }
            "account/chatgptAuthTokens/refresh" => {
                self.with(|session| {
                    if !session.open_notices.contains_key(notice_kind::AUTH_REQUIRED) {
                        session.notice(NoticeLevel::Error, notice_kind::AUTH_REQUIRED,
                            "Codex could not refresh sign-in tokens. Run `codex login` to sign in again.".into());
                    }
                });
                json!({"id": id, "error": {"code": -32601, "message": format!("riwork does not handle {method}")}})
            }
            // Requests RiWork cannot serve: tool calls it never registered,
            // token refreshes (it does not handle credentials), attestation.
            _ => json!({
                "id": id,
                "error": {"code": -32601, "message": format!("riwork does not handle {method}")},
            }),
        };
        let _ = self.proc.send(&reply);
    }

    /// Show `approval` and hold `kind` until the user answers it.
    fn open_request(&self, rpc_id: Value, approval: Approval, kind: RequestKind) {
        self.with(|session| {
            session.requests.push(OpenRequest {
                request_id: approval.request_id.clone(),
                rpc_id,
                kind,
                blocking: true,
            });
            session.emit(ChatEvent::ApprovalRequested { approval });
            session.refresh_state();
        });
    }

    // ---- Notifications ----

    fn on_notification(&self, method: &str, params: &Value) {
        let frames = self.with(|session| session.notification(method, params));
        self.send_all(frames);
    }
}

impl Session {
    fn account_request(&mut self) -> Value {
        let generation = self.account_notification_generation;
        self.request(
            "account/read",
            json!({"refreshToken": false}),
            Box::new(move |codex, outcome| {
                if let Ok(value) = outcome {
                    codex.with(|session| session.apply_account_read(generation, &value));
                }
            }),
        )
    }

    fn apply_account_read(&mut self, generation: u64, value: &Value) {
        if generation != self.account_notification_generation {
            return;
        }
        if let Some(identity) = super::account_identity::canonical(
            &self.identity_config,
            Some(value),
            self.identity.as_ref(),
        ) {
            self.identity = Some(identity.clone());
            self.emit(ChatEvent::ProviderAccountIdentity {
                identity: Some(identity),
            });
        }
    }

    fn apply_rate_read(&mut self, generation: u64, value: &Value) {
        if self.rate_notification_generation == generation {
            self.update_rate_limits(value);
        }
    }

    fn update_rate_limits(&mut self, value: &Value) {
        let changed = self.rate_limits.update(value);
        if changed {
            self.emit(ChatEvent::RateLimits {
                windows: self.rate_limits.windows.clone(),
            });
        }
        if changed || self.rate_limits.observed_usage {
            if !self.rate_limits.windows.is_empty() && !self.rate_limits.has_exhausted_window() {
                self.resolve_notice("rate_limit:codex", "Codex can answer again.");
            } else if let Some(reset) = self.rate_limits.exhausted_reset() {
                let mut update = None;
                if let Some(item) = self.open_notices.get_mut("rate_limit:codex") {
                    if let ItemBody::Notice { resets_at, .. } = &mut item.body {
                        if *resets_at != Some(reset) {
                            *resets_at = Some(reset);
                            update = Some(item.clone());
                        }
                    }
                }
                if let Some(item) = update {
                    self.emit(ChatEvent::ItemCompleted { item });
                }
            }
        }
    }

    /// Apply one notification; returns frames to send afterwards.
    fn notification(&mut self, method: &str, params: &Value) -> Vec<Value> {
        if method == "account/updated"
            || (method == "account/login/completed" && params["success"].as_bool() == Some(true))
        {
            self.account_notification_generation += 1;
            if let Some(identity) = params
                .get("account")
                .filter(|v| v.is_object())
                .and_then(|_| {
                    super::account_identity::canonical(
                        &self.identity_config,
                        Some(params),
                        self.identity.as_ref(),
                    )
                })
            {
                self.identity = Some(identity.clone());
                self.emit(ChatEvent::ProviderAccountIdentity {
                    identity: Some(identity),
                });
            } else {
                // account/updated normally reports authMode and planType, not identity.
                // Do not retain the previous login's scope while the new read is pending.
                self.identity = None;
                self.emit(ChatEvent::ProviderAccountIdentity { identity: None });
                if method == "account/login/completed"
                    || !params["authMode"].is_null()
                    || !params["planType"].is_null()
                {
                    return vec![self.account_request()];
                }
            }
        }
        // Sub-agents report on the same connection under their own thread ids.
        if method != "serverRequest/resolved"
            && let (Some(theirs), Some(ours)) = (params["threadId"].as_str(), &self.thread_id)
            && theirs != ours
        {
            return Vec::new();
        }
        let turn_id = params["turnId"]
            .as_str()
            .filter(|id| !id.trim().is_empty())
            .map(str::to_owned);
        if method.starts_with("item/") {
            if let Some(id) = turn_id.as_deref() {
                self.resolve_reconnecting(id);
            }
        }
        match method {
            "account/rateLimits/updated" => {
                self.rate_notification_generation += 1;
                self.update_rate_limits(params);
            }
            "turn/started" => {
                if let Some(id) = params["turn"]["id"].as_str() {
                    return self.begin_turn(id);
                }
            }
            "turn/completed" => {
                let turn = &params["turn"];
                let outcome = match turn["status"].as_str() {
                    Some("completed") => TurnOutcome::Completed,
                    Some("interrupted") => TurnOutcome::Interrupted,
                    Some("failed") => TurnOutcome::Failed {
                        message: turn["error"]["message"]
                            .as_str()
                            .unwrap_or("the turn failed")
                            .to_owned(),
                    },
                    _ => return Vec::new(),
                };
                if let Some(id) = turn["id"].as_str() {
                    return self.finish_turn(id, outcome);
                }
            }
            "item/started" | "item/completed" => {
                let done = method == "item/completed";
                self.item(&params["item"], turn_id, done);
            }
            "item/agentMessage/delta" => {
                if let (Some(id), Some(text)) =
                    (params["itemId"].as_str(), params["delta"].as_str())
                {
                    self.keep(id, text);
                    self.delta(id, Delta::Text(text.to_owned()));
                }
            }
            "item/reasoning/summaryTextDelta" => {
                self.reasoning_delta(params, "summaryIndex", true);
            }
            "item/reasoning/textDelta" => {
                self.reasoning_delta(params, "contentIndex", false);
            }
            "item/commandExecution/outputDelta" => {
                if let (Some(id), Some(text)) =
                    (params["itemId"].as_str(), params["delta"].as_str())
                {
                    self.keep(id, text);
                    self.delta(id, Delta::Output(text.to_owned()));
                }
            }
            "item/fileChange/patchUpdated" => {
                if let Some(id) = params["itemId"].as_str() {
                    let changes = file_changes(&params["changes"]);
                    self.file_changes.insert(id.to_owned(), changes.clone());
                    self.emit(ChatEvent::ItemStarted {
                        item: Item {
                            presentation: Default::default(),
                            id: id.to_owned(),
                            turn_id,
                            status: ItemStatus::InProgress,
                            body: ItemBody::FileChange { changes },
                        },
                    });
                }
            }
            "turn/plan/updated" => {
                if let Some(turn) = turn_id {
                    self.emit(ChatEvent::ItemStarted {
                        item: Item {
                            presentation: Default::default(),
                            id: format!("plan-{turn}"),
                            turn_id: Some(turn),
                            status: ItemStatus::InProgress,
                            body: ItemBody::Plan {
                                explanation: params["explanation"].as_str().map(str::to_owned),
                                steps: params["plan"]
                                    .as_array()
                                    .map(|steps| {
                                        steps
                                            .iter()
                                            .map(|step| Step {
                                                text: step["step"]
                                                    .as_str()
                                                    .unwrap_or_default()
                                                    .to_owned(),
                                                status: match step["status"].as_str() {
                                                    Some("completed") => StepStatus::Completed,
                                                    Some("inProgress") => StepStatus::InProgress,
                                                    _ => StepStatus::Pending,
                                                },
                                            })
                                            .collect()
                                    })
                                    .unwrap_or_default(),
                            },
                        },
                    });
                }
            }
            // Codex names a thread only when it is renamed (`/rename`, `thread/name/set`).
            "thread/name/updated" => {
                if let Some(title) = params["threadName"]
                    .as_str()
                    .filter(|t| !t.trim().is_empty())
                {
                    self.emit(ChatEvent::ProviderTitle {
                        title: title.to_owned(),
                    });
                }
            }
            "thread/tokenUsage/updated" => {
                let usage = &params["tokenUsage"];
                let total = &usage["total"];
                self.emit(ChatEvent::Usage {
                    usage: Usage {
                        input_tokens: total["inputTokens"].as_u64().unwrap_or(0),
                        output_tokens: total["outputTokens"].as_u64().unwrap_or(0),
                        cached_input_tokens: total["cachedInputTokens"].as_u64().unwrap_or(0),
                        context_window: usage["modelContextWindow"].as_u64(),
                        context_used: usage["last"]["totalTokens"].as_u64(),
                        cost_usd: None,
                    },
                });
            }
            "serverRequest/resolved" => {
                let id = match &params["requestId"] {
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                };
                if let Some(at) = self.requests.iter().position(|open| open.request_id == id) {
                    let open = self.requests.remove(at);
                    self.emit(match open.kind {
                        RequestKind::Question { .. } => {
                            ChatEvent::QuestionResolved { request_id: id }
                        }
                        _ => ChatEvent::ApprovalResolved {
                            request_id: id,
                            decision: Decision::Cancel,
                        },
                    });
                    self.refresh_state();
                }
            }
            "error" => {
                let error = &params["error"];
                let mut text = error["message"]
                    .as_str()
                    .unwrap_or("Codex reported an error")
                    .to_owned();
                if let Some(details) = error["additionalDetails"].as_str() {
                    text.push_str(&format!("\n{details}"));
                }
                let info = &error["codexErrorInfo"];
                let has = |key: &str| {
                    info.as_str() == Some(key)
                        || info.as_object().is_some_and(|obj| obj.contains_key(key))
                };
                let kind = if has("unauthorized") {
                    notice_kind::AUTH_REQUIRED
                } else if has("usageLimitExceeded") {
                    "rate_limit:codex"
                } else {
                    notice_kind::PROVIDER_ERROR
                };
                if params["willRetry"].as_bool().unwrap_or(false)
                    && kind == notice_kind::PROVIDER_ERROR
                {
                    // "Reconnecting... 3/5" repeats: one notice, updated.
                    // A missing turn id must not collapse unrelated retries into "retry-".
                    let retry_turn = turn_id.clone().or_else(|| self.turn.clone());
                    if let Some(turn) = retry_turn.filter(|id| !id.trim().is_empty()) {
                        let id = format!("retry-{turn}");
                        let item = self.notice_with_id(
                            id,
                            NoticeLevel::Warning,
                            notice_kind::RECONNECTING,
                            text,
                            Some(turn.clone()),
                        );
                        self.reconnecting.insert(turn, item);
                    } else {
                        self.notice(NoticeLevel::Warning, notice_kind::RECONNECTING, text);
                    }
                } else {
                    self.error_shown = true;
                    self.notice(NoticeLevel::Error, kind, text);
                }
            }
            "account/updated" if params["authMode"].is_null() && params["planType"].is_null() => {
                self.notice(
                    NoticeLevel::Error,
                    notice_kind::AUTH_REQUIRED,
                    "Codex is signed out. Run `codex login` to sign in again.".into(),
                );
            }
            "account/login/completed" if params["success"].as_bool() == Some(false) => {
                let text = params["error"]
                    .as_str()
                    .or_else(|| params["error"]["message"].as_str())
                    .unwrap_or("Codex sign-in failed. Run `codex login` to sign in again.");
                self.notice(NoticeLevel::Error, notice_kind::AUTH_REQUIRED, text.into());
            }
            "model/rerouted" => {
                let from = params["fromModel"].as_str().unwrap_or("the selected model");
                let to = params["toModel"].as_str().unwrap_or("another model");
                let reason = params["reason"]
                    .as_str()
                    .unwrap_or("the selected model was unavailable");
                self.notice(
                    NoticeLevel::Warning,
                    notice_kind::MODEL_FALLBACK,
                    format!("Codex answered with {to} instead of {from} ({reason})."),
                );
            }
            "deprecationNotice" => {
                if let Some(summary) = params["summary"].as_str() {
                    let mut text = summary.to_owned();
                    if let Some(details) = params["details"].as_str() {
                        text.push_str(&format!("\n{details}"));
                    }
                    self.notice(NoticeLevel::Info, notice_kind::DEPRECATION, text);
                }
            }
            "warning" => {
                if let Some(message) = params["message"].as_str() {
                    self.notice(
                        NoticeLevel::Warning,
                        notice_kind::PROVIDER_WARNING,
                        message.to_owned(),
                    );
                }
            }
            "configWarning" => {
                if let Some(summary) = params["summary"].as_str() {
                    let mut text = summary.to_owned();
                    if let Some(details) = params["details"].as_str() {
                        text.push_str(&format!("\n{details}"));
                    }
                    self.notice(NoticeLevel::Warning, notice_kind::CONFIG_WARNING, text);
                }
            }
            _ => {}
        }
        Vec::new()
    }

    fn delta(&self, item_id: &str, delta: Delta) {
        self.emit(ChatEvent::ItemDelta {
            item_id: item_id.to_owned(),
            delta,
        });
    }

    /// Remember streamed text for the completion of item `id`.
    fn keep(&mut self, id: &str, text: &str) {
        let streamed = self.streamed.entry(id.to_owned()).or_default();
        if streamed.text.len() + text.len() <= STREAM_KEEP_BYTES {
            streamed.text.push_str(text);
        }
    }

    fn reasoning_delta(&mut self, params: &Value, index_key: &str, summary: bool) {
        let (Some(id), Some(text)) = (params["itemId"].as_str(), params["delta"].as_str()) else {
            return;
        };
        let index = params[index_key].as_i64();
        let streamed = self.streamed.entry(id.to_owned()).or_default();
        // Summary and raw content describe the same thinking: show one.
        if *streamed.summary.get_or_insert(summary) != summary {
            return;
        }
        let mut shown = text.to_owned();
        if streamed.part.is_some() && streamed.part != index && !streamed.text.is_empty() {
            shown.insert_str(0, "\n\n");
        }
        streamed.part = index;
        if streamed.text.len() + shown.len() <= STREAM_KEEP_BYTES {
            streamed.text.push_str(&shown);
        }
        if !std::mem::replace(&mut streamed.started, true) {
            self.emit(ChatEvent::ItemStarted {
                item: Item {
                    presentation: Default::default(),
                    id: id.to_owned(),
                    turn_id: params["turnId"].as_str().map(str::to_owned),
                    status: ItemStatus::InProgress,
                    body: ItemBody::Reasoning {
                        text: String::new(),
                    },
                },
            });
        }
        self.delta(id, Delta::Text(shown));
    }

    /// An `item/started` or `item/completed` notification.
    fn item(&mut self, item: &Value, turn_id: Option<String>, done: bool) {
        let Some(id) = item["id"].as_str() else {
            return;
        };
        let Some((mut body, status)) = convert_item(item, done) else {
            return;
        };
        // Reasoning is shown with its first text: many reasoning items have
        // none to show (it stays with the model).
        if matches!(body, ItemBody::Reasoning { .. }) && !done {
            self.streamed.entry(id.to_owned()).or_default();
            return;
        }
        let kept = if done { self.streamed.remove(id) } else { None };
        let started = kept.as_ref().is_some_and(|kept| kept.started);
        let streamed = kept.map(|kept| kept.text);
        // Completion replaces what the deltas built: keep their text where
        // Codex does not repeat it.
        match &mut body {
            ItemBody::Command { output, .. } if output.is_empty() => {
                *output = streamed.unwrap_or_default();
            }
            ItemBody::AgentMessage { text } | ItemBody::Reasoning { text } if text.is_empty() => {
                *text = streamed.unwrap_or_default();
                if text.is_empty() && !started && matches!(body, ItemBody::Reasoning { .. }) {
                    return;
                }
            }
            ItemBody::FileChange { changes } => {
                if done {
                    self.file_changes.remove(id);
                } else {
                    self.file_changes.insert(id.to_owned(), changes.clone());
                }
            }
            _ => {}
        }
        let item = Item {
            presentation: super::media::presentation(item),
            id: id.to_owned(),
            turn_id,
            status,
            body,
        };
        self.emit(if done {
            ChatEvent::ItemCompleted { item }
        } else {
            ChatEvent::ItemStarted { item }
        });
    }
}

// ---- Translation helpers ----

struct Policy {
    approval: &'static str,
    /// For `thread/start` and `thread/resume`.
    sandbox_mode: &'static str,
    /// For `turn/start`.
    sandbox: Value,
    plan: bool,
}

fn policy(mode: ApprovalMode) -> Policy {
    match mode {
        ApprovalMode::Supervised => Policy {
            approval: "untrusted",
            sandbox_mode: "read-only",
            sandbox: json!({"type": "readOnly"}),
            plan: false,
        },
        ApprovalMode::AutoEdit => Policy {
            approval: "on-request",
            sandbox_mode: "workspace-write",
            sandbox: json!({"type": "workspaceWrite"}),
            plan: false,
        },
        ApprovalMode::Full => Policy {
            approval: "never",
            sandbox_mode: "danger-full-access",
            sandbox: json!({"type": "dangerFullAccess"}),
            plan: false,
        },
        ApprovalMode::Plan => Policy {
            approval: "never",
            sandbox_mode: "read-only",
            sandbox: json!({"type": "readOnly"}),
            plan: true,
        },
    }
}

/// The models of a `model/list` page, in the server's order, without the hidden ones
/// (internal and special-purpose models the picker leaves out). The id that `turn/start`
/// takes is `model` (the same string as `id` in every list seen).
fn model_options(data: &Value) -> Vec<ModelOption> {
    data.as_array()
        .into_iter()
        .flatten()
        .filter_map(model_option)
        .collect()
}

fn model_option(model: &Value) -> Option<ModelOption> {
    if model["hidden"].as_bool() == Some(true) {
        return None;
    }
    let id = model["model"].as_str().or(model["id"].as_str())?;
    // `serviceTiers` replaced `additionalSpeedTiers` (which still says `fast`).
    let supports_fast = match model["serviceTiers"].as_array() {
        Some(tiers) => tiers.iter().any(|tier| tier["id"] == FAST_TIER),
        None => model["additionalSpeedTiers"]
            .as_array()
            .is_some_and(|tiers| tiers.iter().any(|tier| tier == "fast")),
    };
    Some(ModelOption {
        id: id.to_owned(),
        name: model["displayName"].as_str().unwrap_or(id).to_owned(),
        description: model["description"].as_str().unwrap_or_default().to_owned(),
        efforts: model["supportedReasoningEfforts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|option| option["reasoningEffort"].as_str())
            .map(str::to_owned)
            .collect(),
        default_effort: model["defaultReasoningEffort"].as_str().map(str::to_owned),
        supports_fast,
        is_default: model["isDefault"].as_bool() == Some(true),
    })
}

fn text_input(text: &str) -> Value {
    json!([{"type": "text", "text": text}])
}

fn wire_decision(decision: Decision) -> &'static str {
    match decision {
        Decision::Accept => "accept",
        Decision::AcceptForSession => "acceptForSession",
        Decision::Decline => "decline",
        Decision::Cancel => "cancel",
    }
}

/// The decisions a command request offers: its `availableDecisions`, minus
/// the amendment forms (objects), or all four when it lists none.
fn command_choices(params: &Value) -> Vec<Decision> {
    let listed: Vec<Decision> = params["availableDecisions"]
        .as_array()
        .map(|decisions| {
            decisions
                .iter()
                .filter_map(|decision| match decision.as_str()? {
                    "accept" => Some(Decision::Accept),
                    "acceptForSession" => Some(Decision::AcceptForSession),
                    "decline" => Some(Decision::Decline),
                    "cancel" => Some(Decision::Cancel),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    if listed.is_empty() {
        vec![
            Decision::Accept,
            Decision::AcceptForSession,
            Decision::Decline,
            Decision::Cancel,
        ]
    } else {
        listed
    }
}

/// The nearest decision Codex offered: a refusal falls back to `cancel` and a
/// session-wide accept to a plain one.
fn pick(decision: Decision, offered: &[Decision]) -> Decision {
    if offered.contains(&decision) {
        return decision;
    }
    match decision {
        Decision::Decline if offered.contains(&Decision::Cancel) => Decision::Cancel,
        Decision::AcceptForSession if offered.contains(&Decision::Accept) => Decision::Accept,
        other => other,
    }
}

fn question_prompt(question: &Value) -> QuestionPrompt {
    QuestionPrompt {
        header: question["header"]
            .as_str()
            .filter(|header| !header.is_empty())
            .map(str::to_owned),
        question: question["question"].as_str().unwrap_or_default().to_owned(),
        options: question["options"]
            .as_array()
            .map(|options| {
                options
                    .iter()
                    .map(|option| QuestionOption {
                        label: option["label"].as_str().unwrap_or_default().to_owned(),
                        description: option["description"]
                            .as_str()
                            .unwrap_or_default()
                            .to_owned(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        multi_select: false,
    }
}

fn item_status(item: &Value, done: bool) -> ItemStatus {
    match item["status"].as_str() {
        Some("completed") => ItemStatus::Completed,
        Some("failed") => ItemStatus::Failed,
        Some("declined") => ItemStatus::Declined,
        Some("interrupted") => ItemStatus::Interrupted,
        Some("inProgress") => ItemStatus::InProgress,
        _ if done => ItemStatus::Completed,
        _ => ItemStatus::InProgress,
    }
}

/// The body and status of a `ThreadItem`, or `None` for kinds a chat does not
/// show.
fn convert_item(item: &Value, done: bool) -> Option<(ItemBody, ItemStatus)> {
    let status = item_status(item, done);
    let text = |key: &str| item[key].as_str().unwrap_or_default().to_owned();
    let body = match item["type"].as_str()? {
        "userMessage" => ItemBody::UserMessage {
            text: item["content"]
                .as_array()
                .map(|parts| {
                    parts
                        .iter()
                        .map(|part| match part["type"].as_str() {
                            Some("text") => part["text"].as_str().unwrap_or_default().to_owned(),
                            Some(other) => format!("[{other}]"),
                            None => String::new(),
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default(),
        },
        "agentMessage" => ItemBody::AgentMessage { text: text("text") },
        "reasoning" => {
            let parts = |key: &str| -> Vec<String> {
                item[key]
                    .as_array()
                    .map(|parts| {
                        parts
                            .iter()
                            .filter_map(|part| part.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default()
            };
            let summary = parts("summary");
            ItemBody::Reasoning {
                text: if summary.is_empty() {
                    parts("content").join("\n\n")
                } else {
                    summary.join("\n\n")
                },
            }
        }
        "plan" => ItemBody::Plan {
            explanation: Some(text("text")),
            steps: Vec::new(),
        },
        "commandExecution" => ItemBody::Command {
            command: display_command(item["command"].as_str().unwrap_or_default()),
            cwd: item["cwd"].as_str().map(str::to_owned),
            output: item["aggregatedOutput"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            exit_code: item["exitCode"]
                .as_i64()
                .and_then(|code| i32::try_from(code).ok()),
        },
        "fileChange" => ItemBody::FileChange {
            changes: file_changes(&item["changes"]),
        },
        "mcpToolCall" => ItemBody::ToolCall {
            server: item["server"].as_str().map(str::to_owned),
            tool: text("tool"),
            input: item["arguments"].clone(),
            output: mcp_output(item),
        },
        "dynamicToolCall" => ItemBody::ToolCall {
            server: item["namespace"].as_str().map(str::to_owned),
            tool: text("tool"),
            input: item["arguments"].clone(),
            output: item["contentItems"].as_array().map(|parts| {
                parts
                    .iter()
                    .filter_map(|part| part["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            }),
        },
        kind @ ("collabAgentToolCall" | "imageView" | "imageGeneration") => {
            let mut input = super::media::without_payloads(item);
            if let Some(object) = input.as_object_mut() {
                object.remove("id");
                object.remove("type");
            }
            ItemBody::ToolCall {
                server: None,
                tool: kind.to_owned(),
                input,
                output: None,
            }
        }
        "webSearch" => ItemBody::WebSearch {
            query: search_query(item),
        },
        "contextCompaction" => ItemBody::Compaction,
        _ => return None,
    };
    Some((body, status))
}

fn search_query(item: &Value) -> String {
    let action = &item["action"];
    [
        item["query"].as_str(),
        action["query"].as_str(),
        action["queries"].get(0).and_then(Value::as_str),
        action["url"].as_str(),
    ]
    .into_iter()
    .flatten()
    .find(|query| !query.is_empty())
    .unwrap_or_default()
    .to_owned()
}

fn mcp_output(item: &Value) -> Option<String> {
    if let Some(message) = item["error"]["message"].as_str() {
        return Some(message.to_owned());
    }
    let result = item.get("result").filter(|result| !result.is_null())?;
    let text = result["content"]
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .map(|block| match block["text"].as_str() {
                    Some(text) => text.to_owned(),
                    None => super::media::without_payloads(block).to_string(),
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    if text.is_empty() && !result["structuredContent"].is_null() {
        return Some(result["structuredContent"].to_string());
    }
    Some(text).filter(|text| !text.is_empty())
}

fn file_changes(changes: &Value) -> Vec<FileChange> {
    changes
        .as_array()
        .map(|changes| {
            changes
                .iter()
                .filter_map(|change| {
                    let kind = match change["kind"]["type"].as_str()? {
                        "add" => ChangeKind::Add,
                        "delete" => ChangeKind::Delete,
                        "update" if change["kind"]["move_path"].is_string() => ChangeKind::Rename,
                        "update" => ChangeKind::Modify,
                        _ => return None,
                    };
                    let diff = change["diff"]
                        .as_str()
                        .filter(|diff| !diff.is_empty())
                        .map(|diff| unified(kind, diff));
                    Some(FileChange {
                        path: change["path"].as_str()?.to_owned(),
                        kind,
                        diff,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Codex gives the whole content for an added or deleted file; show it the way
/// a diff would.
fn unified(kind: ChangeKind, diff: &str) -> String {
    let real = diff.starts_with("diff --git ")
        || diff.starts_with("@@ -")
        || (diff.starts_with("--- ") && diff.lines().nth(1).is_some_and(|l| l.starts_with("+++ ")));
    if real {
        return diff.to_owned();
    }
    let lines: Vec<&str> = diff.lines().collect();
    let (sign, mut out) = match kind {
        ChangeKind::Add => ('+', format!("@@ -0,0 +1,{} @@", lines.len())),
        ChangeKind::Delete => ('-', format!("@@ -1,{} +0,0 @@", lines.len())),
        ChangeKind::Modify | ChangeKind::Rename => return diff.to_owned(),
    };
    for line in lines {
        out.push('\n');
        out.push(sign);
        out.push_str(line);
    }
    out
}

fn truncate(text: &mut String, limit: usize) {
    if text.len() > limit {
        let mut end = limit;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("\n…");
    }
}

/// `/bin/bash -lc "git status"` as the command it runs: Codex reports the
/// shell wrapper it started, which is noise in a chat.
fn display_command(command: &str) -> String {
    let mut words = command.trim().splitn(3, char::is_whitespace);
    if let (Some(shell), Some(flag), Some(script)) = (words.next(), words.next(), words.next())
        && matches!(shell.rsplit('/').next(), Some("bash" | "sh" | "zsh"))
        && flag.starts_with('-')
        && flag.ends_with('c')
        && let Some(script) = shell_word(script.trim())
    {
        return script;
    }
    command.to_owned()
}

/// The single shell word `text` spells (quotes and backslashes resolved), or
/// `None` if it is more than one word or is malformed.
fn shell_word(text: &str) -> Option<String> {
    let mut word = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => loop {
                match chars.next()? {
                    '\'' => break,
                    other => word.push(other),
                }
            },
            '"' => loop {
                match chars.next()? {
                    '"' => break,
                    '\\' => match chars.next()? {
                        escaped @ ('"' | '\\' | '$' | '`') => word.push(escaped),
                        '\n' => {}
                        other => {
                            word.push('\\');
                            word.push(other);
                        }
                    },
                    other => word.push(other),
                }
            },
            '\\' => word.push(chars.next()?),
            c if c.is_whitespace() => return None,
            other => word.push(other),
        }
    }
    Some(word)
}

#[cfg(test)]
#[path = "codex_tests.rs"]
mod tests;
