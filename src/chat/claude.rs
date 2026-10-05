//! The Claude driver: `claude` spoken to directly in stream-json, the way the
//! Agent SDK does, with no SDK in between.
//!
//! The process runs unmodified and logged in (never given a token; the
//! environment loses `ANTHROPIC_API_KEY`, which would silently switch it to API
//! billing) as
//!
//! ```text
//! claude <extra_args> --output-format stream-json --verbose --input-format stream-json
//!   --permission-prompt-tool stdio --include-partial-messages
//!   --allow-dangerously-skip-permissions --permission-mode <mode>
//!   [--model M] [--effort E] [--settings '{"fastMode":true}'] (--resume ID | --session-id UUID)
//! ```
//!
//! with `CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS=1`, which makes it announce
//! `running` and `idle`. `idle` is the end-of-turn signal (`result` is missing
//! after some failures). The permission mode is always passed, because the
//! process otherwise starts in `auto`, and
//! `--allow-dangerously-skip-permissions` only makes bypass available to a
//! later `Configure`.
//!
//! `start` sends the `initialize` control request and returns once it is
//! answered, with stdin kept open for the whole chat. The answer carries the models
//! the CLI offers (`models`, SDK `ModelInfo`) and the Fast mode state
//! (`fast_mode_state`, `fast_mode_disabled_reason`); see "Models and Fast mode". A new chat is started
//! with a session id of our own (`--session-id`), since `system/init` only
//! arrives with the first message; the id is known as soon as `start` returns.
//! Resuming a session Claude never saved (it exits 1 with "No conversation
//! found") starts it again under the same id, once, and says so in a notice.
//!
//! # What the host sees
//!
//! Drivers never emit `Info`: they lack the chat's id, title and timestamps.
//! The host builds it from `provider_thread_id()`, which is the Claude session
//! id and is `Some` once `start` has returned. The events start with
//! `State { Starting }` and `State { Idle }`; `State { Stopped }` follows an
//! intentional stop and `State { Failed }` an unexpected end. `Send` echoes the
//! message as a `UserMessage` item, since the CLI does not.
//!
//! # Frames to events
//!
//! | stream-json                                  | event                                                   |
//! |----------------------------------------------|---------------------------------------------------------|
//! | `system/session_state_changed` `running`     | `TurnStarted`, `State { Running }`                      |
//! | `system/session_state_changed` `idle`        | `TurnCompleted` (Completed, Failed or Interrupted), `State { Idle }` |
//! | `stream_event` text/thinking deltas          | `ItemStarted` then `ItemDelta { Text }` on `AgentMessage` / `Reasoning` |
//! | `assistant` text / thinking block            | `ItemCompleted` `AgentMessage` / `Reasoning`            |
//! | `assistant` `tool_use` `Bash`                | `ItemStarted` `Command`                                 |
//! | `assistant` `tool_use` `Edit` `Write` `MultiEdit` `NotebookEdit` | `ItemStarted` `FileChange` with a simple diff |
//! | `assistant` `tool_use` `TodoWrite`           | `ItemStarted` `Todo`                                    |
//! | `assistant` `tool_use` `ExitPlanMode`        | `ItemStarted` `Plan`                                    |
//! | `assistant` `tool_use` `WebSearch`           | `ItemStarted` `WebSearch`                               |
//! | `assistant` `tool_use`, anything else        | `ItemStarted` `ToolCall` (`mcp__server__tool` splits into server and tool) |
//! | `user` `tool_result`                         | `ItemCompleted` for the matching item, with output, exit code and status |
//! | `control_request` `can_use_tool`             | `ApprovalRequested` (kind `Command`, `FileChange` or `Tool`; choices Accept, AcceptForSession, Decline), `State { Waiting }` |
//! | `control_request` `can_use_tool` `AskUserQuestion` | `QuestionRequested`                               |
//! | `control_cancel_request`                     | `ApprovalResolved { Cancel }` / `QuestionResolved`      |
//! | `result`                                     | `Usage` (cost is the CLI's running total, never summed); a failed result also a `Notice` |
//! | `system/compact_boundary`                    | `ItemCompleted` `Compaction`                            |
//! | `system/api_retry`, `rate_limit_event` (not `allowed`) | `Notice`                                      |
//! | oversized line, silence for two minutes      | `Notice` (the line is dropped; the process is left alone) |
//! | end of output                                | `TurnCompleted { Failed }` then `State { Failed }`, with the end of stderr |
//!
//! `Usage` counts tokens the way the Codex driver does: `input_tokens` includes
//! cache reads and writes and `cached_input_tokens` is the cache reads, summed
//! over the turns of this driver (the CLI reports each turn alone);
//! `context_used` is the context after the turn's last request and
//! `context_window` comes from `modelUsage`. `total_cost_usd` is the running
//! total of the CLI process, so it starts again from zero in a restarted one.
//!
//! Everything else (hooks, `status`, `task_*`, `thinking_tokens`, unknown
//! types, lines that are not JSON) is ignored. Subagent text and thinking
//! (`parent_tool_use_id` set) is skipped; subagent tool calls still appear.
//! Read-only tools inside the working directory never ask for permission, so
//! tool items come from `tool_use` blocks, not from permission requests.
//!
//! # Commands to frames
//!
//! | command                  | sent                                                              |
//! |--------------------------|-------------------------------------------------------------------|
//! | `Send`                   | `{"type":"user","session_id":"","message":{"role":"user","content":TEXT},"parent_tool_use_id":null}`; queued while the process restarts |
//! | `Interrupt`              | `control_request` `interrupt` (with `cancel_queued`); after 5 s SIGINT to the process; after 5 s more, or if the process leaves within 10 s of SIGINT, it is stopped and restarted with `--resume`, closing the turn as Interrupted |
//! | `Approve`                | `control_response` `{behavior:"allow", updatedInput}` (AcceptForSession adds `updatedPermissions`, always with destination `session`), or `{behavior:"deny", message}` (Cancel adds `interrupt:true` and also starts the `Interrupt` escalation) |
//! | `Answer`                 | `control_response` `allow` with `updatedInput { questions, answers }`, answers keyed by question text, multiple choices joined by ", " |
//! | `Configure`              | `set_permission_mode` / `set_model` / `apply_flag_settings` (Fast mode) control requests; a request the CLI cannot do, and an effort change, restart it with `--resume` (when idle) |
//! | `Compact`                | the user message `/compact`                                       |
//! | `Stop`                   | `shutdown()`: close stdin, wait 5 s, SIGTERM, then SIGKILL, on the process group |
//!
//! # Models and Fast mode
//!
//! `Models` has one `ModelOption` per entry of the `initialize` answer's `models`: `id` is
//! the `value` (what `--model` and `set_model` take; `default` is the CLI's own choice, and
//! `is_default` is true for it only), `name` the `displayName`, `efforts` the
//! `supportedEffortLevels` (none for a model without them, Haiku), `supports_fast` the
//! `supportsFastMode` (absent means no). `default_effort` is not told by the CLI. The list
//! is said once per process start when it changes, and again when a later `initialize` answer
//! differs. A CLI that names no models says nothing, and the chat keeps a text field for the
//! model (the `system/init` model is not made into a one-entry list: that would take the
//! text field away). Which models exist depends on the account, the plan and the CLI's
//! configuration, so nothing is assumed about them.
//!
//! Fast mode is a setting of the CLI's flag layer, `fastMode`: a headless CLI ignores a
//! `fastMode` saved in the user's own settings. A chat with Fast mode on passes it as
//! `--settings '{"fastMode":true}'` at every start (a restart too), and `Configure` changes it
//! in the running process with the control request `apply_flag_settings`
//! `{"settings":{"fastMode":B}}`; a CLI that does not know it is restarted with the flag as
//! it is for other settings. The answer to that request is always `{}`, whether or not Fast
//! mode can be served, so the driver then asks `initialize` again, which answers with the
//! current `fast_mode_state` (`off`, `cooldown`, `on`) and `fast_mode_disabled_reason`.
//! Those two also arrive in `system/init` and `result`.
//!
//! The chat's `fast` is the user's choice and stays whatever the CLI grants. When it is on and
//! the state is not `on`, one `Notice` (Warning) says why, once for each state and reason:
//! "Fast mode is cooling down after a rate limit..." for `cooldown`, the CLI's own
//! reasons (a plan without it, the organization, the network, ...) for `off`; and one
//! (Info) says "Fast mode is on again." when it comes back. A model without Fast mode
//! (`supportsFastMode` absent in the list) makes the CLI say `off` without a reason, and
//! nothing is said for it; `pending` is not said either.
//!
//! # Verified and assumed
//!
//! Verified against live captures of claude 2.1.289 (`initialize`, `interrupt`,
//! `set_permission_mode`, the error for an unknown subtype, `--help`) and the
//! recorded T3 Code transcripts: the `system/init`, `stream_event`,
//! `assistant`, `user`, `result`, `compact_boundary` and `rate_limit_event`
//! shapes, one assistant frame per content block sharing `message.id`, the
//! cumulative `total_cost_usd`, per-turn `usage`, `modelUsage.contextWindow`.
//! Taken from the SDK types (`sdk.d.ts`, `sdk-tools.d.ts`) and not captured
//! live: the `can_use_tool` request and its `PermissionResult` reply, the
//! `AskUserQuestion` answer format, `control_cancel_request`,
//! `session_state_changed` (the live captures predate asking for it),
//! `set_model`, and the tool input shapes. Parsing is tolerant of all of them.
//! The `models` array, `fast_mode_state` and `fast_mode_disabled_reason` of the `initialize`
//! answer, `--settings '{"fastMode":true}'` and `apply_flag_settings` followed by a second
//! `initialize` were checked live against claude 2.1.289 in an isolated, logged-out
//! configuration (`testdata/claude/models_fast.ndjson` has the models as it answered). Without
//! an account the CLI cannot look up the organization's Fast mode setting and answers
//! `off`/`preference` to a request for it, so `on` and the cooldown come from the SDK types
//! and from a probe that skipped that look-up, not from a real subscriber's account.
//! Also checked live: `--resume` of an unknown session exits 1 with "No
//! conversation found with session ID: ..." on stderr and a `result` error on
//! stdout. Assumed: a session that never received a message counts as unknown
//! (a restart before the first message uses `--session-id` again), and `Write`
//! onto a file that exists is reported as a modification (decided from the disk
//! when the tool call is seen).

use super::child::{self, Frame, FrameReader, MAX_FRAME_BYTES, Proc};
use super::driver::{Driver, DriverConfig};
use super::model::{
    Approval, ApprovalKind, ApprovalMode, ChangeKind, ChatCommand, ChatEvent, ChatState, Decision,
    FileChange, Item, ItemBody, ItemStatus, ModelOption, NoticeLevel, Question, QuestionOption,
    QuestionPrompt, Step, StepStatus, TurnOutcome, Usage,
};
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::process::ChildStdout;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::thread;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// The longest tool output or diff kept in an item; the rest is cut.
const TEXT_LIMIT: usize = 64 * 1024;
/// What the CLI is told when a request is refused.
const DECLINED: &str = "User declined tool execution.";
/// What `claude --resume` says, on stderr and with exit status 1, about a
/// session it has no transcript for.
const NO_CONVERSATION: &str = "No conversation found";
/// How long after SIGINT a process that exits is taken to have been ended by it.
const SIGINT_EXIT_WINDOW: Duration = Duration::from_secs(10);

/// How long the driver waits at each step; tests shorten them.
#[derive(Clone, Copy, Debug)]
struct Tuning {
    /// For `initialize` to be answered.
    handshake: Duration,
    /// For an interrupt request to end the turn, before SIGINT.
    interrupt_grace: Duration,
    /// For SIGINT to end the turn, before the process is restarted.
    interrupt_kill: Duration,
    /// Without a frame during a turn before a notice says so.
    inactivity: Duration,
    /// How often the watchdog looks.
    tick: Duration,
    /// For a process to leave by itself once its input is closed.
    stop_grace: Duration,
    /// For a process to leave after SIGTERM.
    term_wait: Duration,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            handshake: Duration::from_secs(30),
            interrupt_grace: Duration::from_secs(5),
            interrupt_kill: Duration::from_secs(5),
            inactivity: Duration::from_secs(120),
            tick: Duration::from_millis(250),
            stop_grace: child::GRACE,
            term_wait: child::TERM_WAIT,
        }
    }
}

/// Start `claude` for `config` and return once it has answered `initialize`.
pub fn start(config: DriverConfig, events: Sender<ChatEvent>) -> Result<Box<dyn Driver>, String> {
    start_with(config, events, Tuning::default())
}

fn start_with(
    config: DriverConfig,
    events: Sender<ChatEvent>,
    tuning: Tuning,
) -> Result<Box<dyn Driver>, String> {
    let resuming = config.resume.is_some();
    let session_id = config
        .resume
        .clone()
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let core = Core::new(&config, events, session_id.clone());
    let shared = Arc::new(Shared {
        core: Mutex::new(core),
        restart_done: Condvar::new(),
        config,
        tuning,
    });
    // A session Claude never saved (nothing was ever sent in it) cannot be
    // resumed. Start it again under the same id, and say so; only the last
    // failure is announced.
    match launch(&shared, resuming) {
        Err(error) if resuming && error.contains(NO_CONVERSATION) => {
            shared.lock().session_used = false;
            launch(&shared, false)?;
            shared.lock().notice(
                NoticeLevel::Info,
                format!(
                    "Claude had no saved conversation {session_id} to resume, \
                     so this chat starts it again."
                ),
            );
        }
        Err(error) => {
            if resuming {
                shared.lock().set_state(ChatState::Failed {
                    message: error.clone(),
                });
            }
            return Err(error);
        }
        Ok(()) => {}
    }
    {
        let mut core = shared.lock();
        if core.dead.is_none() {
            core.set_state(ChatState::Idle);
        }
    }
    let weak = Arc::downgrade(&shared);
    thread::spawn(move || supervise(weak));
    Ok(Box::new(ClaudeDriver { shared }))
}

struct ClaudeDriver {
    shared: Arc<Shared>,
}

impl Driver for ClaudeDriver {
    fn command(&mut self, command: ChatCommand) -> Result<(), String> {
        if matches!(command, ChatCommand::Stop) {
            self.shutdown();
            return Ok(());
        }
        let mut core = self.shared.lock();
        let result = core.command(command);
        let restart = core.restart_due();
        drop(core);
        if restart {
            spawn_restart(&self.shared);
        }
        result
    }

    fn provider_thread_id(&self) -> Option<String> {
        Some(self.shared.lock().session_id.clone())
    }

    fn shutdown(&mut self) {
        shutdown(&self.shared);
    }
}

impl Drop for ClaudeDriver {
    fn drop(&mut self) {
        shutdown(&self.shared);
    }
}

/// State shared by the driver, the reader of each process and the watchdog.
struct Shared {
    core: Mutex<Core>,
    /// Signalled when a restart is over, for `shutdown` to wait on.
    restart_done: Condvar,
    config: DriverConfig,
    tuning: Tuning,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Core> {
        self.core.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// A callback for the answer to one of our control requests, run on the reader
/// thread with the state locked.
type Reply = Box<dyn FnOnce(&mut Core, Result<Value, String>) + Send>;

struct Settings {
    /// The CLI's own name for the mode (`default`, `acceptEdits`, ...).
    permission_mode: String,
    model: Option<String>,
    effort: Option<String>,
    /// The user wants Fast mode. Whether the CLI grants it is another matter
    /// (`Core::fast_reported`).
    fast: bool,
}

/// A turn in progress.
struct Turn {
    id: String,
    /// How the `result` frame says it went; decided when `idle` arrives.
    result: Option<TurnOutcome>,
    /// The last assistant text, so a failure is not said twice.
    last_text: Option<String>,
    last_item: Option<Item>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// Asked politely; waiting for the turn to end.
    Control,
    /// SIGINT sent.
    Sigint,
    /// Being restarted.
    Restarting,
}

struct Interrupt {
    at: Instant,
    stage: Stage,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Text,
    Thinking,
}

/// A streamed text or thinking block, found again by its index.
struct Block {
    kind: BlockKind,
    item_id: String,
    started: bool,
}

#[derive(Default)]
struct Stream {
    message_id: Option<String>,
    blocks: HashMap<u64, Block>,
    /// How many content blocks of each message have arrived as `assistant`
    /// frames, which come one per block in order: the next block's index.
    frames: HashMap<String, u64>,
}

enum Pending {
    Approval {
        approval: Approval,
        tool: String,
        input: Value,
        tool_use_id: String,
        suggestions: Vec<Value>,
    },
    Question {
        request_id: String,
        input: Value,
        questions: Vec<String>,
    },
}

impl Pending {
    fn request_id(&self) -> &str {
        match self {
            Pending::Approval { approval, .. } => &approval.request_id,
            Pending::Question { request_id, .. } => request_id,
        }
    }
}

#[derive(Default)]
struct Totals {
    input: u64,
    output: u64,
    cached: u64,
}

struct Core {
    events: Sender<ChatEvent>,
    // The process.
    generation: u64,
    proc: Option<Arc<Proc>>,
    out: Option<Sender<String>>,
    ready: bool,
    restarting: bool,
    stopped: bool,
    /// Why the process is gone, once it has failed.
    dead: Option<String>,
    /// The process a restart is stopping, for `shutdown` to stop as well.
    retiring: Option<Arc<Proc>>,
    // The session.
    session_id: String,
    /// Whether the session has a transcript on disk to resume from.
    session_used: bool,
    settings: Settings,
    restart_wanted: bool,
    // The chat.
    state: ChatState,
    turn: Option<Turn>,
    stream: Stream,
    /// Tool calls the CLI has made and not yet reported on, by tool use id.
    tools: HashMap<String, Item>,
    pending: Vec<Pending>,
    /// Tool calls the user refused, to tell their results from failures.
    declined: HashSet<String>,
    /// Messages sent while the process restarts, and whether each is echoed as
    /// a user message (a queued `Compact` is not).
    queued: Vec<(String, bool)>,
    // The protocol.
    requests: HashMap<String, Reply>,
    request_counter: u64,
    state_events: bool,
    // Watching.
    last_activity: Instant,
    silent: bool,
    interrupt: Option<Interrupt>,
    /// When SIGINT was last sent to a process, whatever became of the turn.
    sigint_at: Option<Instant>,
    // Counters and usage.
    /// Makes notice ids unique among the drivers a chat has had: the host keeps
    /// one log for the chat, and a transcript replaces items with equal ids.
    notice_prefix: String,
    notice_counter: u64,
    totals: Totals,
    /// The models the CLI offers, from its `initialize` answer; empty if it said none.
    models: Vec<ModelOption>,
    /// The last Fast mode state a notice told the user about, as `(state, reason)`,
    /// and `None` while Fast mode is on, not asked for, or not yet known.
    fast_reported: Option<(String, Option<String>)>,
    /// The model `system/init` named, whose window `context_window` is.
    model: Option<String>,
    context_window: Option<u64>,
    context_used: Option<u64>,
}

impl Core {
    fn new(config: &DriverConfig, events: Sender<ChatEvent>, session_id: String) -> Self {
        Self {
            events,
            generation: 0,
            proc: None,
            out: None,
            ready: false,
            restarting: false,
            stopped: false,
            dead: None,
            retiring: None,
            session_used: config.resume.is_some(),
            session_id,
            settings: Settings {
                permission_mode: permission_mode(config.approval_mode).to_owned(),
                model: config.model.clone(),
                effort: config.effort.clone(),
                fast: config.fast,
            },
            restart_wanted: false,
            // No process yet; `launch` announces `Starting`.
            state: ChatState::Stopped,
            turn: None,
            stream: Stream::default(),
            tools: HashMap::new(),
            pending: Vec::new(),
            declined: HashSet::new(),
            queued: Vec::new(),
            requests: HashMap::new(),
            request_counter: 0,
            state_events: false,
            last_activity: Instant::now(),
            silent: false,
            interrupt: None,
            sigint_at: None,
            notice_prefix: Uuid::new_v4().simple().to_string()[..8].to_owned(),
            notice_counter: 0,
            totals: Totals::default(),
            models: Vec::new(),
            fast_reported: None,
            model: None,
            context_window: None,
            context_used: None,
        }
    }

    fn emit(&self, event: ChatEvent) {
        // The host going away is not the driver's concern.
        let _ = self.events.send(event);
    }

    fn set_state(&mut self, state: ChatState) {
        if self.state != state {
            self.state = state.clone();
            self.emit(ChatEvent::State { state });
        }
    }

    /// Running, or Waiting while the user owes an answer.
    fn refresh_state(&mut self) {
        if self.turn.is_some() {
            let state = if self.pending.is_empty() {
                ChatState::Running
            } else {
                ChatState::Waiting
            };
            self.set_state(state);
        }
    }

    fn turn_id(&self) -> Option<String> {
        self.turn.as_ref().map(|turn| turn.id.clone())
    }

    /// The open turn's id, opening one if there is none. Frames and messages
    /// can arrive before `running` does, in either order.
    fn ensure_turn(&mut self) -> String {
        if let Some(id) = self.turn_id() {
            return id;
        }
        let id = format!("turn-{}", Uuid::new_v4());
        self.turn = Some(Turn {
            id: id.clone(),
            result: None,
            last_text: None,
            last_item: None,
        });
        self.last_activity = Instant::now();
        self.silent = false;
        self.emit(ChatEvent::TurnStarted {
            turn_id: id.clone(),
        });
        self.set_state(ChatState::Running);
        id
    }

    fn close_turn(&mut self, outcome: TurnOutcome) {
        let Some(turn) = self.turn.take() else {
            return;
        };
        for pending in std::mem::take(&mut self.pending) {
            match pending {
                Pending::Approval { approval, .. } => self.emit(ChatEvent::ApprovalResolved {
                    request_id: approval.request_id,
                    decision: Decision::Cancel,
                }),
                Pending::Question { request_id, .. } => {
                    self.emit(ChatEvent::QuestionResolved { request_id })
                }
            }
        }
        self.emit(ChatEvent::TurnCompleted {
            turn_id: turn.id,
            outcome,
        });
        self.tools.clear();
        self.stream = Stream::default();
        self.declined.clear();
        self.interrupt = None;
    }

    /// An item that is complete when it appears.
    fn born(&mut self, mut item: Item) {
        item.status = ItemStatus::Completed;
        self.emit(ChatEvent::ItemStarted { item: item.clone() });
        self.emit(ChatEvent::ItemCompleted { item });
    }

    fn notice(&mut self, level: NoticeLevel, text: impl Into<String>) {
        self.notice_counter += 1;
        let item = Item {
            presentation: Default::default(),
            id: format!("notice-{}-{}", self.notice_prefix, self.notice_counter),
            turn_id: self.turn_id(),
            status: ItemStatus::Completed,
            body: ItemBody::Notice {
                level,
                text: text.into(),
            },
        };
        self.born(item);
    }

    fn write(&self, value: &Value) -> Result<(), String> {
        self.out
            .as_ref()
            .ok_or_else(|| "claude is not running".to_owned())?
            .send(value.to_string())
            .map_err(|_| "claude has closed its input".to_owned())
    }

    fn next_request_id(&mut self) -> String {
        self.request_counter += 1;
        format!(
            "req_{}_{}",
            self.request_counter,
            &Uuid::new_v4().simple().to_string()[..8]
        )
    }

    /// Send a control request and run `reply` with its answer.
    fn control(&mut self, request: Value, reply: Reply) -> Result<(), String> {
        let id = self.next_request_id();
        self.write(&json!({"type": "control_request", "request_id": id, "request": request}))?;
        self.requests.insert(id, reply);
        Ok(())
    }

    fn respond(&self, request_id: &str, body: Value) -> Result<(), String> {
        self.write(&json!({
            "type": "control_response",
            "response": {"subtype": "success", "request_id": request_id, "response": body},
        }))
    }

    /// Whether a setting that needs a new process is waiting and the chat is
    /// idle. If so, the restart is claimed: the caller must run `restart`.
    fn restart_due(&mut self) -> bool {
        if !self.restart_wanted || self.turn.is_some() || !self.ready {
            return false;
        }
        // Still wanted if the claim is refused (a restart is under way).
        let claimed = self.begin_restart();
        if claimed {
            self.restart_wanted = false;
        }
        claimed
    }

    /// Claim the replacement of the process. From here on `Send` queues, and
    /// whatever the old process still says is ignored.
    fn begin_restart(&mut self) -> bool {
        if self.restarting || self.stopped || self.dead.is_some() {
            return false;
        }
        self.restarting = true;
        self.ready = false;
        self.generation += 1;
        self.out = None;
        self.requests.clear();
        true
    }

    // Commands.

    fn command(&mut self, command: ChatCommand) -> Result<(), String> {
        if self.stopped {
            return Err("the chat has been stopped".into());
        }
        if let Some(reason) = &self.dead {
            return Err(format!("claude is not running ({reason})"));
        }
        match command {
            ChatCommand::Send { text } => {
                if self.restarting {
                    self.queued.push((text, true));
                    Ok(())
                } else {
                    self.send_user(text, true)
                }
            }
            ChatCommand::Compact => {
                if self.restarting {
                    self.queued.push(("/compact".into(), false));
                    Ok(())
                } else {
                    self.send_user("/compact".into(), false)
                }
            }
            ChatCommand::Interrupt => self.interrupt(),
            ChatCommand::Approve {
                request_id,
                decision,
            } => self.approve(&request_id, decision),
            ChatCommand::Answer {
                request_id,
                answers,
            } => self.answer(&request_id, &answers),
            ChatCommand::Configure {
                model,
                effort,
                approval_mode,
                fast,
            } => {
                self.configure(model, effort, approval_mode, fast);
                Ok(())
            }
            ChatCommand::Stop => Ok(()),
        }
    }

    fn send_user(&mut self, text: String, echo: bool) -> Result<(), String> {
        if self.out.is_none() {
            return Err("claude is not running".into());
        }
        self.write(&json!({
            "type": "user",
            "session_id": "",
            "message": {"role": "user", "content": text},
            "parent_tool_use_id": null,
        }))?;
        self.session_used = true;
        let turn_id = self.ensure_turn();
        self.last_activity = Instant::now();
        self.silent = false;
        if echo {
            self.born(Item {
                presentation: Default::default(),
                id: format!("user-{}", Uuid::new_v4()),
                turn_id: Some(turn_id),
                status: ItemStatus::Completed,
                body: ItemBody::UserMessage { text },
            });
        }
        Ok(())
    }

    fn interrupt(&mut self) -> Result<(), String> {
        if self.restarting || self.interrupt.is_some() {
            return Ok(());
        }
        if self.turn.is_none() {
            return Ok(());
        }
        // The CLI is blocked on any prompt we owe an answer to.
        for pending in &self.pending {
            let (request_id, tool_use_id) = match pending {
                Pending::Approval {
                    approval,
                    tool_use_id,
                    ..
                } => (approval.request_id.as_str(), tool_use_id.as_str()),
                Pending::Question { request_id, .. } => (request_id.as_str(), ""),
            };
            let _ = self.respond(request_id, deny_body(tool_use_id, true));
        }
        self.begin_interrupt()
    }

    /// Ask the CLI to stop the turn, and start the clock on escalating if it
    /// does not. `cancel_queued` also drops messages it has queued behind the
    /// turn (older CLIs ignore the field).
    fn begin_interrupt(&mut self) -> Result<(), String> {
        if self.interrupt.is_some() {
            return Ok(());
        }
        self.interrupt = Some(Interrupt {
            at: Instant::now(),
            stage: Stage::Control,
        });
        self.control(
            json!({"subtype": "interrupt", "cancel_queued": true}),
            Box::new(|_, _| {}),
        )
    }

    fn approve(&mut self, request_id: &str, decision: Decision) -> Result<(), String> {
        let at = self
            .pending
            .iter()
            .position(|pending| pending.request_id() == request_id)
            .ok_or_else(|| format!("no request {request_id} is waiting"))?;
        let Pending::Approval {
            approval,
            tool,
            input,
            tool_use_id,
            suggestions,
        } = &self.pending[at]
        else {
            return Err(format!("request {request_id} is a question"));
        };
        let offered = approval.choices.contains(&decision);
        let (decision, body) = match decision {
            Decision::Accept => (decision, allow_body(input, tool_use_id, None)),
            Decision::AcceptForSession if offered => (
                decision,
                allow_body(
                    input,
                    tool_use_id,
                    Some(session_permissions(tool, input, suggestions)),
                ),
            ),
            // Less persistent than asked for, which is the safe direction.
            Decision::AcceptForSession => (Decision::Accept, allow_body(input, tool_use_id, None)),
            Decision::Decline => (decision, deny_body(tool_use_id, false)),
            Decision::Cancel => (decision, deny_body(tool_use_id, true)),
        };
        let tool_use_id = tool_use_id.clone();
        self.respond(request_id, body)?;
        self.pending.remove(at);
        if matches!(decision, Decision::Decline | Decision::Cancel) {
            self.declined.insert(tool_use_id);
        }
        self.emit(ChatEvent::ApprovalResolved {
            request_id: request_id.to_owned(),
            decision,
        });
        self.refresh_state();
        self.answered();
        if decision == Decision::Cancel {
            // The reply asks for the interrupt too, but a CLI that ignores it
            // gets the same escalation as for `Interrupt`.
            let _ = self.begin_interrupt();
        }
        Ok(())
    }

    fn answer(&mut self, request_id: &str, answers: &[Vec<String>]) -> Result<(), String> {
        let at = self
            .pending
            .iter()
            .position(|pending| pending.request_id() == request_id)
            .ok_or_else(|| format!("no request {request_id} is waiting"))?;
        let Pending::Question {
            input, questions, ..
        } = &self.pending[at]
        else {
            return Err(format!("request {request_id} is an approval"));
        };
        let mut given = Map::new();
        for (index, question) in questions.iter().enumerate() {
            let labels = answers.get(index).map(Vec::as_slice).unwrap_or_default();
            given.insert(question.clone(), Value::String(labels.join(", ")));
        }
        let mut updated = input.clone();
        if let Some(object) = updated.as_object_mut() {
            object.insert("answers".into(), Value::Object(given));
        }
        let body = json!({"behavior": "allow", "updatedInput": updated});
        self.respond(request_id, body)?;
        self.pending.remove(at);
        self.emit(ChatEvent::QuestionResolved {
            request_id: request_id.to_owned(),
        });
        self.refresh_state();
        self.answered();
        Ok(())
    }

    /// A prompt was answered: the time the user took is not the CLI's silence.
    fn answered(&mut self) {
        self.last_activity = Instant::now();
        self.silent = false;
    }

    fn configure(
        &mut self,
        model: Option<String>,
        effort: Option<String>,
        approval_mode: Option<ApprovalMode>,
        fast: Option<bool>,
    ) {
        if let Some(mode) = approval_mode {
            let mode = permission_mode(mode).to_owned();
            if mode != self.settings.permission_mode {
                let before = std::mem::replace(&mut self.settings.permission_mode, mode.clone());
                self.change_setting(
                    json!({"subtype": "set_permission_mode", "mode": mode}),
                    "the permission mode",
                    move |core| core.settings.permission_mode = before,
                );
            }
        }
        if let Some(model) = model
            && self.settings.model.as_deref() != Some(model.as_str())
        {
            let before = self.settings.model.replace(model.clone());
            self.change_setting(
                json!({"subtype": "set_model", "model": model}),
                "the model",
                move |core| core.settings.model = before,
            );
        }
        if let Some(effort) = effort
            && self.settings.effort.as_deref() != Some(effort.as_str())
        {
            // Only a flag at start-up sets it.
            self.settings.effort = Some(effort);
            self.restart_wanted = true;
        }
        if let Some(fast) = fast
            && self.settings.fast != fast
        {
            self.settings.fast = fast;
            self.fast_reported = None;
            self.change_fast(fast);
        }
    }

    /// Turn Fast mode on or off in the running process: a flag-layer setting, which
    /// the process keeps until it ends (a restart passes it at launch). Its answer
    /// never says whether Fast mode is available, so the state is asked for again.
    fn change_fast(&mut self, fast: bool) {
        let reply: Reply = Box::new(move |core, result| match result {
            Ok(_) => core.ask_state(),
            Err(error) if error.contains("Unsupported") => core.restart_wanted = true,
            Err(error) => {
                core.settings.fast = !fast;
                core.notice(
                    NoticeLevel::Warning,
                    format!("claude did not change Fast mode: {error}"),
                );
            }
        });
        let request = json!({"subtype": "apply_flag_settings", "settings": {"fastMode": fast}});
        if self.control(request, reply).is_err() {
            self.restart_wanted = true;
        }
    }

    /// Ask the CLI where it stands: `initialize` answers again with the models and the
    /// Fast mode state as they are now.
    fn ask_state(&mut self) {
        let reply: Reply = Box::new(|core, result| {
            if let Ok(state) = result {
                core.take_state(&state);
            }
        });
        let _ = self.control(json!({"subtype": "initialize", "hooks": null}), reply);
    }

    /// What the CLI said about itself in an `initialize` answer: the models it offers
    /// and whether Fast mode is on.
    fn take_state(&mut self, state: &Value) {
        let models: Vec<ModelOption> = state["models"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(model_option)
            .collect();
        if !models.is_empty() && models != self.models {
            self.models = models.clone();
            self.emit(ChatEvent::Models { models });
        }
        self.note_fast_state(state);
    }

    /// Say so when Fast mode was asked for and is not on, and when it is on again.
    /// `frame` is anything that carries `fast_mode_state` (the `initialize` answer,
    /// `system/init`, `result`); one without it changes nothing. Each state is said
    /// once, however many frames repeat it.
    fn note_fast_state(&mut self, frame: &Value) {
        let Some(state) = str_of(frame, "fast_mode_state") else {
            return;
        };
        let reason = str_of(frame, "fast_mode_disabled_reason").map(str::to_owned);
        // A model without Fast mode explains itself with its toggle gone.
        let model = self.settings.model.as_deref().unwrap_or("default");
        let unsupported = self
            .models
            .iter()
            .find(|option| option.id == model)
            .is_some_and(|option| !option.supports_fast);
        if !self.settings.fast || unsupported {
            self.fast_reported = None;
            return;
        }
        if state == "on" {
            if self.fast_reported.take().is_some() {
                self.notice(NoticeLevel::Info, "Fast mode is on again.");
            }
            return;
        }
        // Checking is over soon, and says nothing yet.
        if reason.as_deref() == Some("pending") {
            return;
        }
        let said = Some((state.to_owned(), reason.clone()));
        if self.fast_reported != said {
            self.fast_reported = said;
            self.notice(
                NoticeLevel::Warning,
                fast_off_text(state, reason.as_deref()),
            );
        }
    }

    /// Ask the running process for a change. If it cannot (an unsupported
    /// request), restart it with the new settings; if it refuses, put the old
    /// value back and say so.
    fn change_setting(
        &mut self,
        request: Value,
        what: &'static str,
        revert: impl FnOnce(&mut Core) + Send + 'static,
    ) {
        let reply: Reply = Box::new(move |core, result| {
            if let Err(error) = result {
                if error.contains("Unsupported") {
                    core.restart_wanted = true;
                } else {
                    revert(core);
                    core.notice(
                        NoticeLevel::Warning,
                        format!("claude did not change {what}: {error}"),
                    );
                }
            }
        });
        if self.control(request, reply).is_err() {
            self.restart_wanted = true;
        }
    }

    // Frames from the CLI.

    fn on_frame(&mut self, frame: &Value) {
        let kind = frame.get("type").and_then(Value::as_str);
        // Until `initialize` is answered only that answer matters: a process
        // that is refusing to start says why on stderr, and its last words on
        // stdout (a `result` error) are not part of the chat.
        if !self.ready && kind != Some("control_response") {
            return;
        }
        self.last_activity = Instant::now();
        self.silent = false;
        match kind {
            Some("system") => self.on_system(frame),
            Some("stream_event") => self.on_stream_event(frame),
            Some("assistant") => self.on_assistant(frame),
            Some("user") => self.on_user(frame),
            Some("result") => self.on_result(frame),
            Some("control_request") => self.on_control_request(frame),
            Some("control_response") => self.on_control_response(frame),
            Some("control_cancel_request") => self.on_cancel(frame),
            Some("rate_limit_event") => self.on_rate_limit(frame),
            _ => {}
        }
    }

    fn on_system(&mut self, frame: &Value) {
        match frame.get("subtype").and_then(Value::as_str) {
            Some("init") => {
                if let Some(id) = str_of(frame, "session_id")
                    && id != self.session_id
                {
                    self.session_id = id.to_owned();
                }
                if let Some(mode) = str_of(frame, "permissionMode") {
                    self.settings.permission_mode = mode.to_owned();
                }
                if let Some(model) = str_of(frame, "model") {
                    self.model = Some(model.to_owned());
                }
                self.note_fast_state(frame);
                self.session_used = true;
            }
            Some("status") => {
                if let Some(mode) = str_of(frame, "permissionMode") {
                    self.settings.permission_mode = mode.to_owned();
                }
            }
            Some("session_state_changed") => {
                self.state_events = true;
                match str_of(frame, "state") {
                    Some("running") => {
                        self.ensure_turn();
                    }
                    Some("idle") => self.finish_turn(),
                    _ => {}
                }
            }
            Some("compact_boundary") => {
                // The compaction's own `result` carries no iterations to take
                // the context from, so it would keep the size from before.
                if let Some(tokens) = frame
                    .pointer("/compact_metadata/post_tokens")
                    .and_then(Value::as_u64)
                {
                    self.context_used = Some(tokens);
                }
                let turn_id = self.turn_id();
                self.born(Item {
                    presentation: Default::default(),
                    id: format!("compaction-{}", Uuid::new_v4()),
                    turn_id,
                    status: ItemStatus::Completed,
                    body: ItemBody::Compaction,
                });
            }
            Some("api_retry") => {
                let attempt = frame.get("attempt").and_then(Value::as_u64).unwrap_or(0);
                let max = frame
                    .get("max_retries")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let delay = frame
                    .get("retry_delay_ms")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let cause = match frame.get("error_status").and_then(Value::as_u64) {
                    Some(status) => format!("status {status}"),
                    None => str_of(frame, "error")
                        .unwrap_or("a connection error")
                        .to_owned(),
                };
                self.notice(
                    NoticeLevel::Warning,
                    format!(
                        "The API failed ({cause}); retrying in {:.1} s (attempt {attempt} of {max})",
                        delay as f64 / 1000.0
                    ),
                );
            }
            _ => {}
        }
    }

    /// `idle`: the turn is over, whichever way it went.
    fn finish_turn(&mut self) {
        let outcome = if self.interrupt.is_some() {
            TurnOutcome::Interrupted
        } else {
            self.turn
                .as_mut()
                .and_then(|turn| turn.result.take())
                .unwrap_or(TurnOutcome::Completed)
        };
        self.close_turn(outcome);
        self.set_state(ChatState::Idle);
    }

    fn on_stream_event(&mut self, frame: &Value) {
        let Some(event) = frame.get("event") else {
            return;
        };
        let subagent = !frame["parent_tool_use_id"].is_null();
        match str_of(event, "type") {
            Some("message_start") => {
                self.stream.message_id = event
                    .pointer("/message/id")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                self.stream.blocks.clear();
            }
            Some("content_block_start") if !subagent => {
                let Some(index) = event.get("index").and_then(Value::as_u64) else {
                    return;
                };
                let kind = match event.pointer("/content_block/type").and_then(Value::as_str) {
                    Some("text") => BlockKind::Text,
                    Some("thinking") => BlockKind::Thinking,
                    _ => return,
                };
                self.stream_block(index, kind);
            }
            Some("content_block_delta") if !subagent => {
                let Some(index) = event.get("index").and_then(Value::as_u64) else {
                    return;
                };
                let delta = &event["delta"];
                let (kind, text) = match str_of(delta, "type") {
                    Some("text_delta") => (BlockKind::Text, str_of(delta, "text")),
                    Some("thinking_delta") => (BlockKind::Thinking, str_of(delta, "thinking")),
                    _ => return,
                };
                let Some(text) = text.filter(|text| !text.is_empty()) else {
                    return;
                };
                let turn_id = self.ensure_turn();
                self.stream_block(index, kind);
                let Some(block) = self.stream.blocks.get_mut(&index) else {
                    return;
                };
                let item_id = block.item_id.clone();
                if !block.started {
                    block.started = true;
                    let body = match kind {
                        BlockKind::Text => ItemBody::AgentMessage {
                            text: String::new(),
                        },
                        BlockKind::Thinking => ItemBody::Reasoning {
                            text: String::new(),
                        },
                    };
                    self.emit(ChatEvent::ItemStarted {
                        item: Item {
                            presentation: Default::default(),
                            id: item_id.clone(),
                            turn_id: Some(turn_id),
                            status: ItemStatus::InProgress,
                            body,
                        },
                    });
                }
                self.emit(ChatEvent::ItemDelta {
                    item_id,
                    delta: super::model::Delta::Text(text.to_owned()),
                });
            }
            _ => {}
        }
    }

    /// Remember a streamed block; its item is only started by its first text.
    fn stream_block(&mut self, index: u64, kind: BlockKind) {
        if self.stream.blocks.contains_key(&index) {
            return;
        }
        let message = self
            .stream
            .message_id
            .clone()
            .unwrap_or_else(|| "message".to_owned());
        self.stream.blocks.insert(
            index,
            Block {
                kind,
                item_id: format!("{message}:{index}"),
                started: false,
            },
        );
    }

    fn on_assistant(&mut self, frame: &Value) {
        let message = &frame["message"];
        let subagent = !frame["parent_tool_use_id"].is_null();
        let message_id = str_of(message, "id")
            .map(str::to_owned)
            .or_else(|| self.stream.message_id.clone())
            .unwrap_or_else(|| "message".to_owned());
        let synthetic = str_of(message, "model") == Some("<synthetic>");
        if !synthetic && let Some(usage) = message.get("usage") {
            self.context_used = context_tokens(usage).or(self.context_used);
        }
        let Some(content) = message.get("content").and_then(Value::as_array) else {
            return;
        };
        for block in content {
            let index = {
                let next = self.stream.frames.entry(message_id.clone()).or_default();
                let index = *next;
                *next += 1;
                index
            };
            let turn_id = self.ensure_turn();
            match str_of(block, "type") {
                Some(kind @ ("text" | "thinking")) if !subagent => {
                    let (kind, text) = if kind == "text" {
                        (BlockKind::Text, str_of(block, "text"))
                    } else {
                        (BlockKind::Thinking, str_of(block, "thinking"))
                    };
                    self.finish_text(&message_id, index, kind, text.unwrap_or_default(), turn_id);
                }
                Some("image") if !subagent => {
                    let item = Item {
                        presentation: super::media::presentation(block),
                        id: format!("{message_id}:{index}"),
                        turn_id: Some(turn_id),
                        status: ItemStatus::Completed,
                        body: ItemBody::ToolCall {
                            server: None,
                            tool: "Image".into(),
                            input: Value::Null,
                            output: None,
                        },
                    };
                    self.emit(ChatEvent::ItemCompleted { item });
                }
                Some("tool_use") => {
                    let (Some(id), Some(name)) = (str_of(block, "id"), str_of(block, "name"))
                    else {
                        continue;
                    };
                    let input = block.get("input").cloned().unwrap_or(Value::Null);
                    let item = Item {
                        presentation: Default::default(),
                        id: id.to_owned(),
                        turn_id: Some(turn_id),
                        status: ItemStatus::InProgress,
                        body: tool_body(name, &input),
                    };
                    self.tools.insert(id.to_owned(), item.clone());
                    self.emit(ChatEvent::ItemStarted { item });
                }
                _ => {}
            }
        }
    }

    fn finish_text(
        &mut self,
        message_id: &str,
        index: u64,
        kind: BlockKind,
        text: &str,
        turn_id: String,
    ) {
        // The streamed block is this one only if it is of the same message: a
        // frame with no `message_start` of its own (an error the CLI makes up)
        // must not take the place of an earlier message's block at that index.
        let streamed = self
            .stream
            .blocks
            .get(&index)
            .filter(|block| block.kind == kind)
            .filter(|_| self.stream.message_id.as_deref() == Some(message_id))
            .map(|block| (block.item_id.clone(), block.started));
        let (item_id, started) = streamed.unwrap_or((format!("{message_id}:{index}"), false));
        if text.trim().is_empty() && !started {
            return;
        }
        let body = match kind {
            BlockKind::Text => ItemBody::AgentMessage {
                text: text.to_owned(),
            },
            BlockKind::Thinking => ItemBody::Reasoning {
                text: text.to_owned(),
            },
        };
        let mut item = Item {
            presentation: Default::default(),
            id: item_id,
            turn_id: Some(turn_id),
            status: ItemStatus::InProgress,
            body,
        };
        if !started {
            self.emit(ChatEvent::ItemStarted { item: item.clone() });
        }
        item.status = ItemStatus::Completed;
        self.emit(ChatEvent::ItemCompleted { item: item.clone() });
        if kind == BlockKind::Text
            && let Some(turn) = &mut self.turn
        {
            turn.last_text = Some(text.to_owned());
            turn.last_item = Some(item);
        }
    }

    fn on_user(&mut self, frame: &Value) {
        let Some(content) = frame.pointer("/message/content").and_then(Value::as_array) else {
            return;
        };
        for block in content {
            if str_of(block, "type") != Some("tool_result") {
                continue;
            }
            let Some(id) = str_of(block, "tool_use_id") else {
                continue;
            };
            let Some(mut item) = self.tools.remove(id) else {
                continue;
            };
            let is_error = block
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let text = content_text(&block["content"]);
            item.status = if !is_error {
                ItemStatus::Completed
            } else if self.declined.contains(id) {
                ItemStatus::Declined
            } else if text.contains("interrupted by user") {
                ItemStatus::Interrupted
            } else {
                ItemStatus::Failed
            };
            item.presentation.images = super::media::presentation(block).images;
            apply_result(
                &mut item.body,
                &text,
                is_error,
                frame.get("tool_use_result"),
            );
            self.emit(ChatEvent::ItemCompleted { item });
        }
    }

    fn on_result(&mut self, frame: &Value) {
        let failed = frame
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || str_of(frame, "subtype").is_some_and(|subtype| subtype.starts_with("error"));
        // The window of the main model; failing that, the largest one used.
        let windows = frame.get("modelUsage").and_then(Value::as_object);
        let window = |model: &Value| model.get("contextWindow").and_then(Value::as_u64);
        if let Some(window) = windows.and_then(|models| {
            self.model
                .as_deref()
                .and_then(|name| models.get(name))
                .and_then(window)
                .or_else(|| models.values().filter_map(window).max())
        }) {
            self.context_window = Some(window);
        }
        let usage = self.usage_from(frame);
        self.emit(ChatEvent::Usage { usage });
        self.note_fast_state(frame);
        if failed {
            let message = str_of(frame, "result")
                .filter(|text| !text.is_empty())
                .or_else(|| str_of(frame, "subtype"))
                .unwrap_or("claude reported an error")
                .to_owned();
            let interrupted = self.interrupt.is_some();
            let repeated = self
                .turn
                .as_ref()
                .and_then(|turn| turn.last_text.as_deref())
                == Some(message.as_str());
            if !interrupted && !repeated {
                self.notice(NoticeLevel::Error, message.clone());
            }
            if let Some(turn) = &mut self.turn {
                turn.result = Some(TurnOutcome::Failed { message });
            }
        }
        if !failed
            && let Some(result) = str_of(frame, "result").filter(|text| !text.trim().is_empty())
        {
            let turn_id = self.ensure_turn();
            let matching = self
                .turn
                .as_ref()
                .and_then(|turn| turn.last_item.clone())
                .filter(
                    |item| matches!(&item.body, ItemBody::AgentMessage { text } if text == result),
                );
            let mut item = matching.unwrap_or_else(|| Item {
                presentation: Default::default(),
                id: format!("{turn_id}:result"),
                turn_id: Some(turn_id),
                status: ItemStatus::Completed,
                body: ItemBody::AgentMessage {
                    text: result.to_owned(),
                },
            });
            item.presentation.phase = Some(super::model::MessagePhase::Final);
            self.emit(ChatEvent::ItemCompleted { item });
        }
        // Without state events nothing else says the turn is over.
        if !self.state_events && self.turn.is_some() {
            self.finish_turn();
        }
    }

    fn usage_from(&mut self, result: &Value) -> Usage {
        let usage = &result["usage"];
        let count = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
        let cached = count("cache_read_input_tokens");
        self.totals.input += count("input_tokens") + count("cache_creation_input_tokens") + cached;
        self.totals.output += count("output_tokens");
        self.totals.cached += cached;
        // The last request of the turn is the context the next one starts from.
        let last = usage
            .get("iterations")
            .and_then(Value::as_array)
            .and_then(|iterations| iterations.last())
            .and_then(context_tokens);
        if last.is_some() {
            self.context_used = last;
        }
        Usage {
            input_tokens: self.totals.input,
            output_tokens: self.totals.output,
            cached_input_tokens: self.totals.cached,
            context_window: self.context_window,
            context_used: self.context_used,
            cost_usd: result.get("total_cost_usd").and_then(Value::as_f64),
        }
    }

    fn on_rate_limit(&mut self, frame: &Value) {
        let info = &frame["rate_limit_info"];
        let (level, what) = match str_of(info, "status") {
            Some("allowed_warning") => (NoticeLevel::Warning, "is close to"),
            Some("rejected") => (NoticeLevel::Error, "has reached"),
            _ => return,
        };
        let window = match str_of(info, "rateLimitType") {
            Some("five_hour") => "the five-hour usage limit",
            Some("seven_day") => "the weekly usage limit",
            Some("seven_day_opus") => "the weekly Opus limit",
            Some("seven_day_sonnet") => "the weekly Sonnet limit",
            Some("overage") => "the extra usage limit",
            _ => "a usage limit",
        };
        // `utilization` and `resetsAt` are left out: the SDK types do not say
        // in what units they come.
        self.notice(level, format!("This account {what} {window}"));
    }

    fn on_control_request(&mut self, frame: &Value) {
        let Some(request_id) = str_of(frame, "request_id") else {
            return;
        };
        let request = &frame["request"];
        let subtype = str_of(request, "subtype").unwrap_or_default();
        if subtype != "can_use_tool" {
            // Nothing else is supported, but the CLI waits for an answer.
            let _ = self.write(&json!({
                "type": "control_response",
                "response": {
                    "subtype": "error",
                    "request_id": request_id,
                    "error": format!("Unsupported control request subtype: {subtype}"),
                },
            }));
            return;
        }
        self.ensure_turn();
        let pending = if str_of(request, "tool_name") == Some("AskUserQuestion") {
            build_question(request_id, request)
        } else {
            build_approval(request_id, request)
        };
        match pending {
            Some(pending) => {
                self.pending.retain(|p| p.request_id() != request_id);
                let event = match &pending {
                    Pending::Approval { approval, .. } => ChatEvent::ApprovalRequested {
                        approval: approval.clone(),
                    },
                    Pending::Question {
                        request_id,
                        input,
                        questions,
                    } => ChatEvent::QuestionRequested {
                        question: Question {
                            request_id: request_id.clone(),
                            questions: question_prompts(input, questions),
                        },
                    },
                };
                self.pending.push(pending);
                self.emit(event);
                self.refresh_state();
            }
            None => {
                let _ = self.write(&json!({
                    "type": "control_response",
                    "response": {
                        "subtype": "error",
                        "request_id": request_id,
                        "error": "Unreadable can_use_tool request",
                    },
                }));
            }
        }
    }

    fn on_control_response(&mut self, frame: &Value) {
        let response = &frame["response"];
        let Some(id) = str_of(response, "request_id") else {
            return;
        };
        let Some(reply) = self.requests.remove(id) else {
            return;
        };
        let result = match str_of(response, "subtype") {
            Some("success") => Ok(response.get("response").cloned().unwrap_or(Value::Null)),
            _ => Err(str_of(response, "error")
                .unwrap_or("the request failed")
                .to_owned()),
        };
        reply(self, result);
    }

    /// The CLI no longer needs an answer to a prompt it showed.
    fn on_cancel(&mut self, frame: &Value) {
        let Some(request_id) = str_of(frame, "request_id") else {
            return;
        };
        let Some(at) = self
            .pending
            .iter()
            .position(|pending| pending.request_id() == request_id)
        else {
            return;
        };
        match self.pending.remove(at) {
            Pending::Approval { approval, .. } => self.emit(ChatEvent::ApprovalResolved {
                request_id: approval.request_id,
                decision: Decision::Cancel,
            }),
            Pending::Question { request_id, .. } => {
                self.emit(ChatEvent::QuestionResolved { request_id })
            }
        }
        self.refresh_state();
    }

    // Watching.

    /// What the watchdog does on each tick.
    fn watch(&mut self, tuning: &Tuning) -> bool {
        if !self.ready || self.restarting || self.stopped || self.dead.is_some() {
            return false;
        }
        let now = Instant::now();
        let silence = now.duration_since(self.last_activity);
        if self.turn.is_some()
            && self.pending.is_empty()
            && !self.silent
            && silence >= tuning.inactivity
        {
            self.silent = true;
            self.notice(
                NoticeLevel::Warning,
                format!(
                    "claude has been silent for {}; it may be running a long command",
                    describe_duration(silence)
                ),
            );
        }
        let Some(interrupt) = &self.interrupt else {
            return false;
        };
        let (stage, waited) = (interrupt.stage, now.duration_since(interrupt.at));
        match stage {
            Stage::Control if waited >= tuning.interrupt_grace => {
                if let Some(proc) = &self.proc {
                    proc.signal(libc::SIGINT);
                }
                self.sigint_at = Some(now);
                self.interrupt = Some(Interrupt {
                    at: now,
                    stage: Stage::Sigint,
                });
                false
            }
            Stage::Sigint if waited >= tuning.interrupt_kill => {
                self.interrupt = Some(Interrupt {
                    at: now,
                    stage: Stage::Restarting,
                });
                self.begin_restart()
            }
            _ => false,
        }
    }
}

// Starting, restarting and stopping processes.

/// The arguments for the next process of `core`'s session.
fn launch_args(core: &Core) -> Vec<String> {
    let mut args: Vec<String> = [
        "--output-format",
        "stream-json",
        "--verbose",
        "--input-format",
        "stream-json",
        "--permission-prompt-tool",
        "stdio",
        "--include-partial-messages",
        "--allow-dangerously-skip-permissions",
        "--permission-mode",
    ]
    .map(str::to_owned)
    .into();
    args.push(core.settings.permission_mode.clone());
    if let Some(model) = &core.settings.model {
        args.extend(["--model".into(), model.clone()]);
    }
    if let Some(effort) = &core.settings.effort {
        args.extend(["--effort".into(), effort.clone()]);
    }
    // Fast mode is on only for a settings overlay that says so: the user's own
    // settings are not read for it when claude runs without a terminal.
    if core.settings.fast {
        args.extend(["--settings".into(), json!({"fastMode": true}).to_string()]);
    }
    // A session nobody has written to has nothing to resume.
    let flag = if core.session_used {
        "--resume"
    } else {
        "--session-id"
    };
    args.extend([flag.into(), core.session_id.clone()]);
    args
}

/// Start a process for the session and shake hands with it. A failure ends
/// the events with `State { Failed }` unless `quiet`, for a caller that goes on
/// to another attempt or has more to say first.
fn launch(shared: &Arc<Shared>, quiet: bool) -> Result<(), String> {
    let result = start_process(shared);
    if let Err(message) = &result {
        let mut core = shared.lock();
        if !quiet && !core.stopped {
            core.set_state(ChatState::Failed {
                message: message.clone(),
            });
        }
    }
    result
}

fn start_process(shared: &Arc<Shared>) -> Result<(), String> {
    let args = {
        let core = shared.lock();
        if core.stopped {
            return Err("the chat has been stopped".into());
        }
        launch_args(&core)
    };
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut command = child::command(&shared.config, &args);
    command
        .env("CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS", "1")
        .env_remove("ANTHROPIC_API_KEY");
    let (proc, stdout) = Proc::spawn(command)?;
    let (answer_tx, answer_rx) = mpsc::channel();
    let (out_tx, out_rx) = mpsc::channel::<String>();
    let generation = {
        let mut core = shared.lock();
        if core.stopped {
            drop(core);
            proc.stop_after(Duration::ZERO, shared.tuning.term_wait);
            return Err("the chat has been stopped".into());
        }
        core.generation += 1;
        core.proc = Some(Arc::clone(&proc));
        core.out = Some(out_tx);
        core.ready = false;
        core.sigint_at = None;
        core.set_state(ChatState::Starting);
        // The answer makes the chat ready as the reader handles it, so that
        // what the process says next is not dropped as startup noise.
        let started = core.control(
            json!({"subtype": "initialize", "hooks": null}),
            Box::new(move |core, result| {
                core.ready |= result.is_ok();
                if let Ok(state) = &result {
                    core.take_state(state);
                }
                let _ = answer_tx.send(result);
            }),
        );
        if let Err(error) = started {
            core.out = None;
            core.proc = None;
            drop(core);
            proc.stop_after(Duration::ZERO, shared.tuning.term_wait);
            return Err(error);
        }
        core.generation
    };
    // Input goes through its own thread so a child that stops reading cannot
    // stall the reader (and so every other command) behind a full pipe.
    let writer = Arc::clone(&proc);
    thread::spawn(move || {
        for line in out_rx {
            if writer.send_line(&line).is_err() {
                break;
            }
        }
    });
    let reader = Arc::clone(shared);
    let reading = Arc::clone(&proc);
    thread::spawn(move || read_loop(reader, generation, reading, stdout));

    let failure = match answer_rx.recv_timeout(shared.tuning.handshake) {
        Ok(Ok(_)) => return Ok(()),
        Ok(Err(error)) => error,
        Err(mpsc::RecvTimeoutError::Disconnected) => "the chat was stopped".to_owned(),
        Err(mpsc::RecvTimeoutError::Timeout) => format!(
            "claude did not answer initialize within {} seconds",
            shared.tuning.handshake.as_secs()
        ),
    };
    {
        let mut core = shared.lock();
        if core.generation == generation {
            core.generation += 1;
            core.out = None;
            core.proc = None;
            core.requests.clear();
        }
    }
    proc.stop_after(Duration::from_millis(200), shared.tuning.term_wait);
    Err(failure)
}

/// Reads a process's output to its end. After the process has been replaced or
/// stopped the output is still read (and dropped), so that a process in the
/// middle of writing is not left blocked on a full pipe while it is stopped.
fn read_loop(shared: Arc<Shared>, generation: u64, proc: Arc<Proc>, stdout: ChildStdout) {
    let mut reader = FrameReader::new(stdout, MAX_FRAME_BYTES);
    loop {
        match reader.next_frame() {
            Ok(Some(Frame::Line(line))) => {
                // Lines that are not JSON are ignored, as are unknown types.
                let Ok(frame) = serde_json::from_slice::<Value>(&line) else {
                    continue;
                };
                let mut core = shared.lock();
                if core.generation != generation {
                    continue;
                }
                core.on_frame(&frame);
                let restart = core.restart_due();
                drop(core);
                if restart {
                    spawn_restart(&shared);
                }
            }
            Ok(Some(Frame::Oversized(bytes))) => {
                let mut core = shared.lock();
                if core.generation != generation || !core.ready {
                    continue;
                }
                core.last_activity = Instant::now();
                core.notice(
                    NoticeLevel::Warning,
                    format!(
                        "Skipped an oversized message from claude ({:.1} MB)",
                        bytes as f64 / 1_000_000.0
                    ),
                );
            }
            Ok(None) | Err(_) => break,
        }
    }
    process_ended(&shared, generation, &proc);
}

/// The process's output ended without our having stopped it.
fn process_ended(shared: &Arc<Shared>, generation: u64, proc: &Arc<Proc>) {
    // The exit status can lag the end of stdout a little.
    let reason = format!("claude {}", proc.describe_exit(Duration::from_secs(2)));
    let mut core = shared.lock();
    if core.generation != generation {
        return;
    }
    if !core.ready {
        // `launch` is waiting for the answer to `initialize`.
        for (_, reply) in std::mem::take(&mut core.requests) {
            reply(&mut core, Err(reason.clone()));
        }
        return;
    }
    // A process that leaves soon after SIGINT was ended by it, even if the
    // turn had already ended and the interrupt was forgotten: carry on with a
    // new one.
    if core
        .sigint_at
        .is_some_and(|at| at.elapsed() < SIGINT_EXIT_WINDOW)
    {
        let claimed = core.begin_restart();
        drop(core);
        if claimed {
            spawn_restart(shared);
        }
        return;
    }
    core.dead = Some(reason.clone());
    core.out = None;
    core.close_turn(TurnOutcome::Failed {
        message: reason.clone(),
    });
    core.set_state(ChatState::Failed { message: reason });
}

fn spawn_restart(shared: &Arc<Shared>) {
    let shared = Arc::clone(shared);
    thread::spawn(move || restart(&shared));
}

/// Replace the process with a new one that resumes the session, once
/// `Core::begin_restart` has claimed the right to. The turn it was in, if any,
/// is over.
fn restart(shared: &Arc<Shared>) {
    let old = {
        let mut core = shared.lock();
        core.close_turn(TurnOutcome::Interrupted);
        if !core.stopped {
            core.set_state(ChatState::Starting);
        }
        // `shutdown` stops it too if it comes now, and does not return before.
        core.retiring = core.proc.take();
        core.retiring.clone()
    };
    if let Some(old) = old {
        old.stop_after(shared.tuning.stop_grace, shared.tuning.term_wait);
    }
    let result = launch(shared, true);
    let mut core = shared.lock();
    core.retiring = None;
    core.restarting = false;
    match result {
        Ok(()) if core.stopped => {}
        Ok(()) => {
            core.set_state(ChatState::Idle);
            for (text, echo) in std::mem::take(&mut core.queued) {
                let _ = core.send_user(text, echo);
            }
        }
        Err(_) if core.stopped => {}
        Err(error) => {
            let undelivered = std::mem::take(&mut core.queued).len();
            if undelivered > 0 {
                let what = if undelivered == 1 {
                    "1 message sent while claude restarted was not delivered".to_owned()
                } else {
                    format!("{undelivered} messages sent while claude restarted were not delivered")
                };
                core.notice(NoticeLevel::Warning, what);
            }
            core.dead = Some(error.clone());
            core.set_state(ChatState::Failed { message: error });
        }
    }
    drop(core);
    shared.restart_done.notify_all();
}

fn shutdown(shared: &Arc<Shared>) {
    let procs = {
        let mut core = shared.lock();
        if core.stopped {
            return;
        }
        core.stopped = true;
        core.generation += 1;
        core.out = None;
        core.requests.clear();
        [core.proc.take(), core.retiring.clone()]
    };
    for proc in procs.into_iter().flatten() {
        proc.stop_after(shared.tuning.stop_grace, shared.tuning.term_wait);
    }
    // A restart under way gives up as soon as it looks at `stopped`. Wait for
    // that, so that nothing it started outlives this call.
    let core = shared.lock();
    let mut core = shared
        .restart_done
        .wait_timeout_while(core, Duration::from_secs(10), |core| core.restarting)
        .map(|(core, _)| core)
        .unwrap_or_else(|poisoned| poisoned.into_inner().0);
    core.close_turn(TurnOutcome::Interrupted);
    if !matches!(core.state, ChatState::Failed { .. }) {
        core.set_state(ChatState::Stopped);
    }
}

/// Watches the process for as long as the driver lives.
fn supervise(shared: Weak<Shared>) {
    loop {
        let Some(shared) = shared.upgrade() else {
            return;
        };
        let (restart, tick) = {
            let mut core = shared.lock();
            if core.stopped {
                return;
            }
            (core.watch(&shared.tuning), shared.tuning.tick)
        };
        if restart {
            spawn_restart(&shared);
        }
        drop(shared);
        thread::sleep(tick);
    }
}

// Translation.

fn permission_mode(mode: ApprovalMode) -> &'static str {
    match mode {
        ApprovalMode::Supervised => "default",
        ApprovalMode::AutoEdit => "acceptEdits",
        ApprovalMode::Full => "bypassPermissions",
        ApprovalMode::Plan => "plan",
    }
}

/// A model the CLI offers (`ModelInfo` of the SDK) as the chat shows it. Its `value` is
/// what `--model` and `set_model` take (`default` is the CLI's own choice).
fn model_option(model: &Value) -> Option<ModelOption> {
    let id = str_of(model, "value")?;
    Some(ModelOption {
        id: id.to_owned(),
        name: str_of(model, "displayName").unwrap_or(id).to_owned(),
        description: str_of(model, "description").unwrap_or_default().to_owned(),
        efforts: model["supportedEffortLevels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        default_effort: None,
        supports_fast: model["supportsFastMode"].as_bool() == Some(true),
        is_default: id == "default",
    })
}

/// Why Fast mode is not on although it was asked for. `reason` is the CLI's
/// `fast_mode_disabled_reason`, absent for a cooldown (a pause after a rate limit)
/// and for a model that cannot do it.
fn fast_off_text(state: &str, reason: Option<&str>) -> String {
    let text = match (state, reason) {
        ("cooldown", _) => {
            "Fast mode is cooling down after a rate limit; turns run at standard speed until it ends"
        }
        (_, Some("free")) => "Fast mode needs a paid Claude subscription",
        (_, Some("preference")) => "Your organization has turned off Fast mode",
        (_, Some("extra_usage_disabled")) => "Fast mode needs extra usage, which is not enabled",
        (_, Some("network_error")) => {
            "Fast mode is unavailable: claude could not reach the network"
        }
        (_, Some("not_first_party")) => "Fast mode only works with the Anthropic API directly",
        (_, Some("disabled_by_env")) => "Fast mode is turned off in claude's environment",
        (_, Some("model_not_allowed")) => {
            "Your organization does not allow this model with Fast mode"
        }
        (_, Some("sdk_opt_in_required")) => "Fast mode did not turn on",
        (_, Some("unknown")) => "Fast mode is unavailable right now",
        (_, Some(other)) => return format!("Fast mode is off ({other})"),
        (_, None) => "Fast mode is off for this model",
    };
    text.to_owned()
}

fn str_of<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str()
}

fn describe_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds >= 120 {
        format!("{} minutes", seconds / 60)
    } else {
        format!("{seconds} seconds")
    }
}

/// Tokens in the context after one request: what it read and what it wrote.
fn context_tokens(usage: &Value) -> Option<u64> {
    let count = |key: &str| usage.get(key).and_then(Value::as_u64);
    let total = count("input_tokens").unwrap_or(0)
        + count("cache_creation_input_tokens").unwrap_or(0)
        + count("cache_read_input_tokens").unwrap_or(0)
        + count("output_tokens").unwrap_or(0);
    (total > 0).then_some(total)
}

/// `text` cut to `TEXT_LIMIT` bytes at a character boundary: the start of it,
/// or the end for command output, where the failure usually is.
fn cap_text(text: &str, keep_end: bool) -> String {
    if text.len() <= TEXT_LIMIT {
        return text.to_owned();
    }
    if keep_end {
        let mut start = text.len() - TEXT_LIMIT;
        while !text.is_char_boundary(start) {
            start += 1;
        }
        format!("[output cut]\n{}", &text[start..])
    } else {
        let mut end = TEXT_LIMIT;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}\n[output cut]", &text[..end])
    }
}

/// The text of a tool result: a string, or the text blocks of a list.
fn content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .map(|block| match str_of(block, "type") {
                Some("text") => str_of(block, "text").unwrap_or_default().to_owned(),
                Some("image") => "[image]".to_owned(),
                _ => String::new(),
            })
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// `Exit code N` and the output after it, as Bash reports a failure.
fn split_exit_code(text: &str) -> (Option<i32>, &str) {
    let Some(rest) = text.strip_prefix("Exit code ") else {
        return (None, text);
    };
    let (number, output) = rest.split_once('\n').unwrap_or((rest, ""));
    match number.trim().parse() {
        Ok(code) => (Some(code), output),
        Err(_) => (None, text),
    }
}

/// `mcp__server__tool` as (server, tool).
fn split_mcp(name: &str) -> Option<(&str, &str)> {
    name.strip_prefix("mcp__")?.split_once("__")
}

fn file_path(input: &Value) -> Option<&str> {
    str_of(input, "file_path").or_else(|| str_of(input, "notebook_path"))
}

/// A unified diff of replacing each `old` with its `new`. The driver does not
/// read the file, so line numbers count from the start of each replaced text.
fn unified_diff(path: &str, new_file: bool, edits: &[(&str, &str)]) -> String {
    let mut diff = String::new();
    if new_file {
        diff.push_str("--- /dev/null\n");
    } else {
        diff.push_str(&format!("--- {path}\n"));
    }
    diff.push_str(&format!("+++ {path}\n"));
    for (old, new) in edits {
        let old: Vec<&str> = old.lines().collect();
        let new: Vec<&str> = new.lines().collect();
        let start = |lines: &[&str]| usize::from(!lines.is_empty());
        diff.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            start(&old),
            old.len(),
            start(&new),
            new.len()
        ));
        for line in old {
            diff.push_str(&format!("-{line}\n"));
        }
        for line in new {
            diff.push_str(&format!("+{line}\n"));
        }
    }
    cap_text(&diff, false)
}

/// The item for a tool call, by what the tool does.
fn tool_body(name: &str, input: &Value) -> ItemBody {
    match name {
        "Bash" => ItemBody::Command {
            command: str_of(input, "command").unwrap_or_default().to_owned(),
            cwd: None,
            output: String::new(),
            exit_code: None,
        },
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => {
            let Some(path) = file_path(input) else {
                return generic_tool(name, input);
            };
            // One stat, taken while the state is locked: it only labels the
            // change as an addition or an edit.
            let new_file = name == "Write" && !std::path::Path::new(path).exists();
            let diff = match name {
                "Edit" => Some(unified_diff(
                    path,
                    false,
                    &[(
                        str_of(input, "old_string").unwrap_or_default(),
                        str_of(input, "new_string").unwrap_or_default(),
                    )],
                )),
                "MultiEdit" => {
                    let edits: Vec<(&str, &str)> = input
                        .get("edits")
                        .and_then(Value::as_array)
                        .map(|edits| {
                            edits
                                .iter()
                                .map(|edit| {
                                    (
                                        str_of(edit, "old_string").unwrap_or_default(),
                                        str_of(edit, "new_string").unwrap_or_default(),
                                    )
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    Some(unified_diff(path, false, &edits))
                }
                "Write" => {
                    let content = str_of(input, "content").unwrap_or_default();
                    Some(unified_diff(path, new_file, &[("", content)]))
                }
                _ => str_of(input, "new_source")
                    .filter(|_| str_of(input, "edit_mode") != Some("delete"))
                    .map(|source| unified_diff(path, false, &[("", source)])),
            };
            let kind = if new_file {
                ChangeKind::Add
            } else {
                ChangeKind::Modify
            };
            ItemBody::FileChange {
                changes: vec![FileChange {
                    path: path.to_owned(),
                    kind,
                    diff,
                }],
            }
        }
        "TodoWrite" => {
            let Some(todos) = input.get("todos").and_then(Value::as_array) else {
                return generic_tool(name, input);
            };
            ItemBody::Todo {
                items: todos
                    .iter()
                    .map(|todo| {
                        let status = match str_of(todo, "status") {
                            Some("completed") => StepStatus::Completed,
                            Some("in_progress") => StepStatus::InProgress,
                            _ => StepStatus::Pending,
                        };
                        let text = match status {
                            StepStatus::InProgress => {
                                str_of(todo, "activeForm").or_else(|| str_of(todo, "content"))
                            }
                            _ => str_of(todo, "content"),
                        };
                        Step {
                            text: text.unwrap_or_default().to_owned(),
                            status,
                        }
                    })
                    .collect(),
            }
        }
        "ExitPlanMode" => match str_of(input, "plan") {
            Some(plan) => ItemBody::Plan {
                explanation: Some(plan.to_owned()),
                steps: Vec::new(),
            },
            None => generic_tool(name, input),
        },
        "WebSearch" => match str_of(input, "query") {
            Some(query) => ItemBody::WebSearch {
                query: query.to_owned(),
            },
            None => generic_tool(name, input),
        },
        _ => generic_tool(name, input),
    }
}

fn generic_tool(name: &str, input: &Value) -> ItemBody {
    let (server, tool) = match split_mcp(name) {
        Some((server, tool)) => (Some(server.to_owned()), tool.to_owned()),
        None => (None, name.to_owned()),
    };
    ItemBody::ToolCall {
        server,
        tool,
        input: input.clone(),
        output: None,
    }
}

/// Put a tool's result into its item.
fn apply_result(body: &mut ItemBody, text: &str, is_error: bool, extra: Option<&Value>) {
    match body {
        ItemBody::Command {
            output, exit_code, ..
        } => {
            let (code, rest) = split_exit_code(text);
            // The tool's own stdout and stderr read better than the placeholder
            // text the model is shown for an empty result.
            let streams = extra.filter(|extra| extra.is_object()).map(|extra| {
                let stdout = str_of(extra, "stdout").unwrap_or_default();
                let stderr = str_of(extra, "stderr").unwrap_or_default();
                match (stdout.is_empty(), stderr.is_empty()) {
                    (_, true) => stdout.to_owned(),
                    (true, false) => stderr.to_owned(),
                    (false, false) => format!("{stdout}\n{stderr}"),
                }
            });
            *output = cap_text(&streams.unwrap_or_else(|| rest.to_owned()), true);
            *exit_code = code.or((!is_error).then_some(0));
        }
        ItemBody::ToolCall { output, .. } => *output = Some(cap_text(text, false)),
        _ => {}
    }
}

/// Without ANSI escapes, which the CLI may put in its reasons.
fn strip_ansi(text: &str) -> String {
    let mut clean = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            clean.push(ch);
            continue;
        }
        if chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
        }
    }
    clean
}

fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default();
    match line.char_indices().nth(200) {
        Some((end, _)) => format!("{}…", &line[..end]),
        None => line.to_owned(),
    }
}

fn approval_kind(tool: &str) -> ApprovalKind {
    match tool {
        "Bash" => ApprovalKind::Command,
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => ApprovalKind::FileChange,
        _ => ApprovalKind::Tool,
    }
}

/// The prompt for a `can_use_tool` request, with the permission updates the CLI
/// suggested for "allow for the session".
fn build_approval(request_id: &str, request: &Value) -> Option<Pending> {
    let tool = str_of(request, "tool_name")?;
    let input = request.get("input").cloned().unwrap_or(Value::Null);
    let tool_use_id = str_of(request, "tool_use_id")
        .unwrap_or_default()
        .to_owned();
    let kind = approval_kind(tool);
    let title = match tool {
        "Bash" => first_line(str_of(&input, "command").unwrap_or(tool)),
        "ExitPlanMode" => "Approve the plan".to_owned(),
        _ if kind == ApprovalKind::FileChange => file_path(&input).unwrap_or(tool).to_owned(),
        _ => str_of(request, "display_name")
            .or_else(|| str_of(request, "title"))
            .unwrap_or(tool)
            .to_owned(),
    };
    let mut detail = Vec::new();
    if let Some(reason) = str_of(request, "decision_reason") {
        detail.push(strip_ansi(reason));
    }
    if let Some(path) = str_of(request, "blocked_path") {
        detail.push(format!("Needs access to {path}"));
    }
    match tool {
        "Bash" => {
            if let Some(description) = str_of(&input, "description") {
                detail.push(description.to_owned());
            }
            let command = str_of(&input, "command").unwrap_or_default();
            if command.lines().count() > 1 || command.len() > 200 {
                detail.push(command.to_owned());
            }
        }
        "ExitPlanMode" => detail.push(str_of(&input, "plan").unwrap_or_default().to_owned()),
        _ if kind == ApprovalKind::FileChange => {
            if let ItemBody::FileChange { changes } = tool_body(tool, &input) {
                detail.extend(changes.into_iter().filter_map(|change| change.diff));
            }
        }
        _ => {
            if !input.is_null() {
                let pretty = serde_json::to_string_pretty(&input).unwrap_or_default();
                detail.push(cap_text(&pretty, false).chars().take(2000).collect());
            }
        }
    }
    let suggestions = request
        .get("permission_suggestions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut choices = vec![Decision::Accept];
    if !request
        .get("suppress_always_allow_rule")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        choices.push(Decision::AcceptForSession);
    }
    choices.push(Decision::Decline);
    Some(Pending::Approval {
        approval: Approval {
            request_id: request_id.to_owned(),
            item_id: (!tool_use_id.is_empty()).then(|| tool_use_id.clone()),
            kind,
            title,
            detail: detail.join("\n\n"),
            choices,
        },
        tool: tool.to_owned(),
        input,
        tool_use_id,
        suggestions,
    })
}

/// The `updatedPermissions` for "allow for the session": what the CLI
/// suggested, else a session rule for the tool (for Bash, this exact command).
/// Every entry is kept to the session, whatever destination the CLI suggested
/// (`PermissionUpdateDestination` also has user, project and local settings
/// files, which would make "for the session" permanent).
fn session_permissions(tool: &str, input: &Value, suggestions: &[Value]) -> Value {
    let suggested: Vec<Value> = suggestions
        .iter()
        .filter_map(|suggestion| {
            let mut update = suggestion.as_object()?.clone();
            update.insert("destination".into(), Value::String("session".into()));
            Some(Value::Object(update))
        })
        .collect();
    if !suggested.is_empty() {
        return Value::Array(suggested);
    }
    let mut rule = json!({"toolName": tool});
    if tool == "Bash"
        && let Some(command) = str_of(input, "command")
    {
        rule["ruleContent"] = Value::String(command.to_owned());
    }
    json!([{"type": "addRules", "rules": [rule], "behavior": "allow", "destination": "session"}])
}

fn allow_body(input: &Value, tool_use_id: &str, permissions: Option<Value>) -> Value {
    let mut body = json!({"behavior": "allow", "updatedInput": input, "toolUseID": tool_use_id});
    if let Some(permissions) = permissions {
        body["updatedPermissions"] = permissions;
    }
    body
}

fn deny_body(tool_use_id: &str, interrupt: bool) -> Value {
    let mut body = json!({"behavior": "deny", "message": DECLINED, "toolUseID": tool_use_id});
    if interrupt {
        body["interrupt"] = Value::Bool(true);
    }
    body
}

/// The prompt for an `AskUserQuestion` request.
fn build_question(request_id: &str, request: &Value) -> Option<Pending> {
    let input = request.get("input").cloned()?;
    let questions: Vec<String> = input
        .get("questions")?
        .as_array()?
        .iter()
        .map(|question| str_of(question, "question").unwrap_or_default().to_owned())
        .collect();
    if questions.is_empty() {
        return None;
    }
    Some(Pending::Question {
        request_id: request_id.to_owned(),
        input,
        questions,
    })
}

fn question_prompts(input: &Value, questions: &[String]) -> Vec<QuestionPrompt> {
    let asked = input
        .get("questions")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    questions
        .iter()
        .enumerate()
        .map(|(index, question)| {
            let source = asked.get(index).unwrap_or(&Value::Null);
            QuestionPrompt {
                header: str_of(source, "header").map(str::to_owned),
                question: question.clone(),
                options: source
                    .get("options")
                    .and_then(Value::as_array)
                    .map(|options| {
                        options
                            .iter()
                            .map(|option| QuestionOption {
                                label: str_of(option, "label").unwrap_or_default().to_owned(),
                                description: str_of(option, "description")
                                    .unwrap_or_default()
                                    .to_owned(),
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                multi_select: source
                    .get("multiSelect")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "claude_tests.rs"]
mod tests;
