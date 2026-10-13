//! The Muse driver: one `muse serve` process per chat, spoken to over MSP
//! (JSON-RPC 2.0, one object per line on stdio).
//!
//! The shape follows the Codex driver, whose app-server protocol MSP most
//! resembles: a caller thread runs the blocking handshake, a reader thread
//! owns stdout and dispatches every frame, our requests correlate by integer
//! id, and server requests (exactly two: `approval/request` and
//! `userInput/request`) become the transcript's approvals and questions.
//! State is derived from the driver's own bookkeeping, never from
//! `session/statusChanged`, which is only a cross-check.
//!
//! Frames in (the ones that become events):
//!
//! | MSP | ChatEvent |
//! |---|---|
//! | `turn/started` | `TurnStarted`, state Running |
//! | `turn/completed` | `TurnCompleted` (terminal maps to outcome), state Idle |
//! | `item/started` / `item/updated` / `item/completed` | `ItemStarted` / `ItemCompleted` via `convert_item` |
//! | `item/delta` | `ItemDelta` (field `text`/absent → Text, `output` → Output, `summary.N` → reasoning Text) |
//! | `approval/request` (server request) | receipt `{}` at once, then `ApprovalRequested` |
//! | `userInput/request` (server request) | receipt `{}` at once, then `QuestionRequested` |
//! | `approval/resolved` / `userInput/settled` | `ApprovalResolved` / `QuestionResolved` (authoritative close) |
//! | `session/tokenUsage` | `Usage` from the cumulative counters only |
//! | `model/list` result | `Models` |
//!
//! Commands out: `Send` → `turn/start` (or `turn/steer` into the open turn),
//! `Interrupt` → `turn/interrupt` (or `userInput/interrupt` while a question
//! pends), `Approve` → `approval/decide`, `Answer` → `userInput/answer`,
//! `Configure` → `session/setModel`/`setReasoningEffort`/`setApprovalMode`,
//! `Compact` → `session/compact`. Approvals are answered in two steps: the
//! JSON-RPC receipt acknowledges presentation only; the decision travels as
//! its own command, and the prompt closes when the server folds
//! `approval/resolved` — never on our decide ack.
//!
//! Approval modes map onto MSP's closed enum: Supervised →
//! `promptUnmatched`, AutoEdit → `onRequest`, Full → `allowAll`, Plan →
//! `denyUnmatched` (nearest available; MSP has no plan collaboration mode).
//!
//! Verified against `muse serve --provider echo` (Muse 1.4.6): framing,
//! initialize/initialized, session start, two echo turns with deltas and
//! token usage, `model/list`, `usage/read`. Assumed from the MSP schema
//! (echo is text-only and cannot produce them): toolCall item shapes and
//! tool names, approval choice inventories, userInput answers, compact
//! success, non-zero usage and model rows. Attachments are refused: MSP
//! image parts exist, but RiWork's attachment presentation cannot ride the
//! echoed user message yet.

use super::child::{self, Frame, FrameReader, Proc};
use super::driver::{Driver, DriverConfig, StartDriver};
use super::model::{
    Approval, ApprovalKind, ApprovalMode, ChatCommand, ChatEvent, ChatState, Decision, Delta, Item,
    ItemBody, ItemStatus, ModelOption, NoticeLevel, Question, QuestionOption, QuestionPrompt, Step,
    StepStatus, TurnOutcome, Usage, notice_kind,
};
use crate::sessions::HarnessKind;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::ChildStdout;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;
use uuid::Uuid;

const _: StartDriver = start;

const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(30);
const SESSION_TIMEOUT: Duration = Duration::from_secs(60);
/// How much streamed text one item keeps for restoring an empty completion.
const STREAM_KEEP_BYTES: usize = 4 * 1024 * 1024;
/// Approval detail is shown to the user; the raw arguments can be huge.
const DETAIL_BYTES: usize = 20_000;
/// The closed MSP reasoning-effort vocabulary.
const EFFORTS: [&str; 8] = [
    "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
];

/// A JSON-RPC error as the driver keeps it: branch on `kind`, show `message`.
#[derive(Clone, Debug)]
struct RpcError {
    message: String,
    kind: Option<String>,
    data: Value,
}

impl RpcError {
    fn of(frame: &Value) -> Self {
        let error = &frame["error"];
        Self {
            message: error["message"]
                .as_str()
                .unwrap_or("muse reported an error")
                .to_owned(),
            kind: error["data"]["kind"].as_str().map(str::to_owned),
            data: error["data"].clone(),
        }
    }
}

type Reply = Box<dyn FnOnce(&Muse, Result<Value, RpcError>) + Send>;

fn command_id() -> String {
    Uuid::now_v7().to_string()
}

/// The Cua MCP config as arguments after `serve`. The subcommand must lead
/// Muse's command line, so these cannot ride `DriverConfig::extra_args`
/// (which `child::command` puts first); the driver builds them itself from
/// the same helper the terminal launch uses.
fn serve_args(config: &DriverConfig) -> Vec<String> {
    let mut args = vec!["serve".to_owned()];
    let home = config
        .env
        .iter()
        .find(|(name, _)| name == "RIWORK_HOME")
        .map(|(_, value)| PathBuf::from(value));
    if let (Some(home), Ok(executable)) = (home, std::env::current_exe()) {
        args.extend(crate::sessions::cua_harness_arguments(
            HarnessKind::Muse,
            &executable,
            &home,
        ));
    }
    args
}

pub fn start(config: DriverConfig, events: Sender<ChatEvent>) -> Result<Box<dyn Driver>, String> {
    let _ = events.send(ChatEvent::State {
        state: ChatState::Starting,
    });
    let args = serve_args(&config);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let (proc, stdout) = Proc::spawn(child::command(&config, &refs)).inspect_err(|error| {
        let _ = events.send(ChatEvent::State {
            state: ChatState::Failed {
                message: error.clone(),
            },
        });
    })?;
    let muse = Arc::new(Muse {
        proc,
        session: Mutex::new(Session::new(&config, events)),
    });
    let reader_muse = muse.clone();
    let reader = std::thread::spawn(move || reader_muse.read_loop(stdout));
    let mut driver = MuseDriver {
        muse,
        reader: Some(reader),
        stopped: false,
    };
    match driver.muse.clone().handshake(&config) {
        Ok(()) => Ok(Box::new(driver)),
        Err(message) => {
            driver.stop_process();
            driver.stopped = true;
            driver
                .muse
                .with(|session| session.finish_failed(message.clone()));
            Err(message)
        }
    }
}

struct MuseDriver {
    muse: Arc<Muse>,
    reader: Option<JoinHandle<()>>,
    stopped: bool,
}

impl MuseDriver {
    fn stop_process(&mut self) {
        self.muse.with(|session| session.stopping = true);
        self.muse.proc.stop();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

impl Driver for MuseDriver {
    fn cancel_io(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        let proc = self.muse.proc.clone();
        Some(Arc::new(move || proc.cancel_writes()))
    }

    fn command(&mut self, command: ChatCommand) -> Result<(), String> {
        if matches!(command, ChatCommand::Stop) {
            self.shutdown();
            return Ok(());
        }
        self.muse.command(command)
    }

    fn provider_thread_id(&self) -> Option<String> {
        self.muse.with(|session| session.session_id.clone())
    }

    fn shutdown(&mut self) {
        if std::mem::replace(&mut self.stopped, true) {
            return;
        }
        self.stop_process();
        self.muse.with(|session| session.finish_stopped());
    }
}

impl Drop for MuseDriver {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct Muse {
    proc: Arc<Proc>,
    session: Mutex<Session>,
}

impl Muse {
    fn with<R>(&self, run: impl FnOnce(&mut Session) -> R) -> R {
        let mut session = self.session.lock().unwrap_or_else(PoisonError::into_inner);
        run(&mut session)
    }

    /// Send frames collected under the lock; a failed write is a notice, like
    /// Codex's — the reader's exit path reports the death itself.
    fn send_all(&self, frames: Vec<Value>) {
        for frame in frames {
            if self.proc.send(&frame).is_err() {
                self.with(|session| {
                    if !session.stopping && !session.finished {
                        session.notice(
                            NoticeLevel::Error,
                            notice_kind::SETTING_REFUSED,
                            "RiWork could not write to muse; the message was not sent.".to_owned(),
                        );
                    }
                });
                return;
            }
        }
    }

    /// One request waited on inline; the handshake is the only caller.
    fn call(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, RpcError> {
        let (sender, receiver) = mpsc::channel();
        let frame = self.with(|session| {
            session.request(
                method,
                params,
                Box::new(move |_, outcome| {
                    let _ = sender.send(outcome);
                }),
            )
        });
        let forget = |muse: &Self| {
            if let Some(id) = frame["id"].as_i64() {
                muse.with(|session| {
                    session.waiting.remove(&id);
                });
            }
        };
        if self.proc.send(&frame).is_err() {
            forget(self);
            return Err(RpcError {
                message: format!("muse {}", self.proc.describe_exit(Duration::from_secs(2))),
                kind: None,
                data: Value::Null,
            });
        }
        match receiver.recv_timeout(timeout) {
            Ok(outcome) => outcome,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                forget(self);
                Err(RpcError {
                    message: format!("muse {}", self.proc.describe_exit(Duration::from_secs(2))),
                    kind: None,
                    data: Value::Null,
                })
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                forget(self);
                Err(RpcError {
                    message: format!(
                        "muse did not answer {method} within {} seconds",
                        timeout.as_secs()
                    ),
                    kind: None,
                    data: Value::Null,
                })
            }
        }
    }

    fn handshake(self: Arc<Self>, config: &DriverConfig) -> Result<(), String> {
        self.call(
            "initialize",
            json!({"clientInfo": {"name": "riwork", "version": env!("CARGO_PKG_VERSION")}}),
            INITIALIZE_TIMEOUT,
        )
        .map_err(|error| error.message)?;
        let _ = self
            .proc
            .send(&json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}));
        let (session_id, name, fresh, active_turn, resumed) = match &config.resume {
            Some(resume) => {
                match self.call(
                    "session/resume",
                    json!({"commandId": command_id(), "sessionId": resume, "excludeItems": true}),
                    SESSION_TIMEOUT,
                ) {
                    Ok(result) => (
                        session_id_of(&result).ok_or("muse resumed without a session id")?,
                        result["session"]["name"].as_str().map(str::to_owned),
                        false,
                        result["session"]["activeTurnId"]
                            .as_str()
                            .filter(|id| !id.is_empty())
                            .map(str::to_owned),
                        true,
                    ),
                    Err(error) if error.kind.as_deref() == Some("sessionNotFound") => {
                        let (id, name) = self.start_session(config)?;
                        (id, name, true, None, false)
                    }
                    Err(error) => return Err(error.message),
                }
            }
            None => {
                let (id, name) = self.start_session(config)?;
                (id, name, false, None, false)
            }
        };
        let effort = config.effort.clone();
        let adopted = self.with(|session| {
            session.session_id = Some(session_id.clone());
            session.ready = true;
            if fresh {
                session.notice(
                    NoticeLevel::Info,
                    notice_kind::RESUMED_FRESH,
                    "Muse no longer has this conversation; a fresh one was started.".to_owned(),
                );
            }
            if let Some(name) = &name
                && !name.is_empty()
            {
                session.emit(ChatEvent::ProviderTitle {
                    title: name.clone(),
                });
            }
            // Only the switch start (no session to resume) drops the context;
            // later restarts resume it, so the warning is for that start only.
            if config.resume.is_none()
                && config
                    .instructions
                    .as_deref()
                    .is_some_and(|text| !text.is_empty())
            {
                session.notice(
                    NoticeLevel::Warning,
                    notice_kind::PROVIDER_WARNING,
                    "The conversation this chat had with its previous provider could not be passed to Muse; it starts without that context.".to_owned(),
                );
            }
            // A turn still running server-side is ours again: without this the
            // chat would read idle and its completion would be dropped.
            let frames = match &active_turn {
                Some(turn) => session.begin_turn(turn),
                None => Vec::new(),
            };
            session.refresh_state();
            frames
        });
        self.send_all(adopted);
        // `session/start` takes model and approval mode as parameters;
        // `session/resume` does not, so re-apply the saved ones explicitly.
        if resumed {
            let mut frames = Vec::new();
            if let Some(model) = &config.model {
                let model = model.clone();
                frames.push(self.with(|session| {
                    session.request(
                        "session/setModel",
                        json!({"commandId": command_id(), "sessionId": session_id, "model": {"modelId": model}}),
                        Box::new(|muse, outcome| {
                            if let Err(error) = outcome {
                                muse.with(|session| {
                                    session.notice(
                                        NoticeLevel::Warning,
                                        notice_kind::SETTING_REFUSED,
                                        format!("Muse did not take the saved model: {}", error.message),
                                    );
                                });
                            }
                        }),
                    )
                }));
            }
            frames.push(self.with(|session| {
                session.request(
                    "session/setApprovalMode",
                    json!({"commandId": command_id(), "sessionId": session_id, "mode": policy(config.approval_mode)}),
                    Box::new(|_, _| {}),
                )
            }));
            self.send_all(frames);
        }
        if let Some(effort) = effort.filter(|effort| EFFORTS.contains(&effort.as_str())) {
            let frame = self.with(|session| {
                session.request(
                    "session/setReasoningEffort",
                    json!({"commandId": command_id(), "sessionId": session_id, "reasoningEffort": effort}),
                    Box::new(|muse, outcome| {
                        if let Err(error) = outcome {
                            muse.with(|session| {
                                session.notice(
                                    NoticeLevel::Warning,
                                    notice_kind::EFFORT_REFUSED,
                                    format!("Muse did not take the saved reasoning effort: {}", error.message),
                                );
                            });
                        }
                    }),
                )
            });
            self.send_all(vec![frame]);
        }
        self.list_models();
        Ok(())
    }

    fn start_session(&self, config: &DriverConfig) -> Result<(String, Option<String>), String> {
        let mut params = json!({
            "commandId": command_id(),
            "workspaceRoot": config.cwd.to_string_lossy(),
            "approvalMode": policy(config.approval_mode),
        });
        if let Some(model) = &config.model {
            params["modelId"] = json!(model);
        }
        let result = self
            .call("session/start", params, SESSION_TIMEOUT)
            .map_err(|error| error.message)?;
        Ok((
            session_id_of(&result).ok_or("muse started without a session id")?,
            result["session"]["name"].as_str().map(str::to_owned),
        ))
    }

    fn list_models(&self) {
        let Some(session_id) = self.with(|session| session.session_id.clone()) else {
            return;
        };
        let frame = self.with(|session| {
            session.request(
                "model/list",
                json!({"sessionId": session_id}),
                Box::new(|muse, outcome| {
                    if let Ok(result) = outcome {
                        let (models, rows) = model_options(&result);
                        muse.with(|session| {
                            session.catalog = rows;
                            session.set_models(models);
                        });
                    }
                }),
            )
        });
        self.send_all(vec![frame]);
    }
}

fn session_id_of(result: &Value) -> Option<String> {
    result["session"]["sessionId"]
        .as_str()
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

impl Muse {
    fn read_loop(self: Arc<Self>, stdout: ChildStdout) {
        let mut reader = FrameReader::new(stdout, child::MAX_FRAME_BYTES);
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
                            "Skipped an oversized message from muse ({} MB).",
                            bytes.div_ceil(1_000_000)
                        ),
                    );
                }),
                Ok(None) | Err(_) => break,
            }
        }
        self.on_exit();
    }

    fn on_exit(&self) {
        let message = format!("muse {}", self.proc.describe_exit(Duration::from_secs(2)));
        self.with(|session| {
            session.waiting.clear();
            if session.stopping || session.finished {
                return;
            }
            if session.ready {
                session.finish_failed(message);
            }
        });
    }

    fn on_frame(&self, frame: Value) {
        match (frame["method"].as_str(), frame.get("id")) {
            (Some(method), Some(id)) => self.on_request(id.clone(), method, &frame["params"]),
            (Some(method), None) => {
                let frames = self.with(|session| session.notification(method, &frame["params"]));
                self.send_all(frames);
            }
            (None, Some(id)) => {
                let Some(id) = id.as_i64() else { return };
                let outcome = if frame.get("error").is_some_and(|error| !error.is_null()) {
                    Err(RpcError::of(&frame))
                } else {
                    Ok(frame["result"].clone())
                };
                if let Some(reply) = self.with(|session| session.waiting.remove(&id)) {
                    reply(self, outcome);
                }
            }
            (None, None) => {}
        }
    }

    /// A server request: the receipt goes back at once — it acknowledges
    /// presentation only; the decision travels later as its own command.
    fn on_request(&self, id: Value, method: &str, params: &Value) {
        let mut frames = Vec::new();
        match method {
            "approval/request" | "userInput/request" => {
                frames.push(json!({"jsonrpc": "2.0", "id": id, "result": {}}));
                frames.extend(self.with(|session| session.present(method, params)));
            }
            _ => frames.push(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": format!("riwork does not handle {method}")},
            })),
        }
        self.send_all(frames);
    }
}

struct OpenRequest {
    request_id: String,
    kind: RequestKind,
    blocking: bool,
}

enum RequestKind {
    Approval {
        requirement: Value,
        choices: Vec<(Decision, String)>,
        /// The prompt as presented, so a stage refresh can re-present it
        /// with its title and detail intact.
        presented: Approval,
        /// A `approval/decide` is in flight; further clicks are ignored.
        deciding: bool,
    },
    Question {
        ids: Vec<String>,
        questions: Vec<WireQuestion>,
    },
}

struct WireQuestion {
    multi: bool,
    labels: Vec<String>,
}

#[derive(Default)]
struct Streamed {
    text: String,
    started: bool,
    reasoning: bool,
    summary_part: Option<i64>,
}

struct Settings {
    model: Option<String>,
    effort: Option<String>,
    mode: ApprovalMode,
}

struct Session {
    events: Sender<ChatEvent>,
    next_request: i64,
    waiting: HashMap<i64, Reply>,
    session_id: Option<String>,
    settings: Settings,
    models: Vec<ModelOption>,
    /// Raw `model/list` rows, replayed by `session/setModel`.
    catalog: Vec<Value>,
    state: ChatState,
    ready: bool,
    turn: Option<String>,
    /// A turn the server queued behind other work; adopted on `turn/started`.
    pending_turn: Option<String>,
    starting: bool,
    start_serial: u64,
    completed_before_receipt: HashSet<String>,
    completion_overflow: bool,
    queued: Vec<String>,
    interrupt_wanted: bool,
    interrupting: bool,
    last_finished_turn: Option<String>,
    error_shown: bool,
    requests: Vec<OpenRequest>,
    streamed: HashMap<String, Streamed>,
    revisions: HashMap<String, u64>,
    ignored: HashSet<String>,
    usage: Usage,
    usage_seen: bool,
    effort_dropped: Option<(String, String)>,
    notices: u64,
    open_notices: HashMap<String, Item>,
    notice_prefix: String,
    stopping: bool,
    finished: bool,
}

impl Session {
    fn new(config: &DriverConfig, events: Sender<ChatEvent>) -> Self {
        Self {
            events,
            next_request: 0,
            waiting: HashMap::new(),
            session_id: config.resume.clone(),
            settings: Settings {
                model: config.model.clone(),
                effort: config.effort.clone(),
                mode: config.approval_mode,
            },
            models: Vec::new(),
            catalog: Vec::new(),
            state: ChatState::Starting,
            ready: false,
            turn: None,
            pending_turn: None,
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
            revisions: HashMap::new(),
            ignored: HashSet::new(),
            usage: Usage::default(),
            usage_seen: false,
            effort_dropped: None,
            notices: 0,
            open_notices: config.outstanding_notices.clone(),
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
        json!({"jsonrpc": "2.0", "id": self.next_request, "method": method, "params": params})
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
        let item = Item {
            presentation: Default::default(),
            id,
            turn_id: self.turn.clone(),
            status: ItemStatus::Completed,
            body: ItemBody::notice(level, text, Some(kind)),
        };
        if kind == notice_kind::AUTH_REQUIRED {
            self.open_notices.insert(kind.to_owned(), item.clone());
        }
        self.emit(ChatEvent::ItemCompleted { item });
    }

    fn resolve_notice(&mut self, kind: &str, text: &str) {
        if let Some(mut item) = self.open_notices.remove(kind) {
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
    }

    fn set_models(&mut self, models: Vec<ModelOption>) {
        if models.is_empty() || models == self.models {
            return;
        }
        self.models = models.clone();
        self.emit(ChatEvent::Models { models });
    }

    fn effective_model(&self) -> Option<String> {
        self.settings.model.clone().or_else(|| {
            self.models
                .iter()
                .find(|model| model.is_default)
                .map(|model| model.id.clone())
        })
    }

    fn takes_effort(&self, effort: &str) -> bool {
        if !EFFORTS.contains(&effort) {
            return false;
        }
        let Some(model) = self.effective_model() else {
            return true;
        };
        self.models
            .iter()
            .find(|option| option.id == model)
            .is_none_or(|option| {
                option.efforts.is_empty() || option.efforts.iter().any(|e| e == effort)
            })
    }

    /// The effort to send: the chosen one if Muse and the model take it.
    fn effort_to_send(&mut self) -> Option<String> {
        let effort = self.settings.effort.clone()?;
        if self.takes_effort(&effort) {
            return Some(effort);
        }
        let model = self.effective_model().unwrap_or_default();
        let dropped = (model, effort.clone());
        if self.effort_dropped.as_ref() != Some(&dropped) {
            self.notice(
                NoticeLevel::Warning,
                notice_kind::EFFORT_REFUSED,
                format!(
                    "The reasoning effort {effort} is not one Muse takes here; its own is used."
                ),
            );
            self.effort_dropped = Some(dropped);
        }
        None
    }

    /// Open `turn_id` unless it is open or over already. Returns the frames
    /// to send: queued messages steered in, a wanted interrupt.
    fn begin_turn(&mut self, turn_id: &str) -> Vec<Value> {
        let mut frames = Vec::new();
        if turn_id.trim().is_empty() {
            return frames;
        }
        // A delayed receipt for an old turn must not drain its queued text
        // into a newer turn. Completion can also precede the start receipt.
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
        // Both the `turn/start` result and `turn/started` come here, and
        // either can arrive after the turn is over.
        if self.turn.is_none() && self.last_finished_turn.as_deref() != Some(turn_id) {
            self.starting = false;
            if self.pending_turn.as_deref() == Some(turn_id) {
                self.pending_turn = None;
            }
            self.turn = Some(turn_id.to_owned());
            self.error_shown = false;
            self.interrupting = false;
            self.emit(ChatEvent::TurnStarted {
                turn_id: turn_id.to_owned(),
            });
        }
        if let (Some(session), Some(turn)) = (self.session_id.clone(), self.turn.clone()) {
            for text in std::mem::take(&mut self.queued) {
                frames.push(self.steer_frame(&session, &turn, text));
            }
            if std::mem::take(&mut self.interrupt_wanted) {
                frames.push(self.interrupt_frame(&session, &turn));
            }
        }
        self.refresh_state();
        frames
    }

    fn interrupt_frame(&mut self, session: &str, turn: &str) -> Value {
        self.interrupting = true;
        self.request(
            "turn/interrupt",
            json!({"commandId": command_id(), "sessionId": session, "turnId": turn, "retract": false}),
            Box::new(|_, _| {}),
        )
    }

    fn steer_frame(&mut self, session: &str, turn: &str, text: String) -> Value {
        let params = json!({
            "commandId": command_id(),
            "sessionId": session,
            "expectedTurnId": turn,
            "input": [{"type": "text", "text": text}],
        });
        let turn = turn.to_owned();
        self.request(
            "turn/steer",
            params,
            Box::new(move |muse, outcome| {
                if outcome.is_ok() {
                    return;
                }
                // The turn may have ended just as the message was sent: then
                // the message starts the next one; else it queues for it.
                let ended = muse.with(|session| session.turn.as_deref() != Some(turn.as_str()));
                if ended {
                    let _ = muse.send_text(text);
                } else {
                    muse.with(|session| session.queued.push(text));
                }
            }),
        )
    }

    /// A `turn/start` for `text`, marking the chat busy.
    fn start_frame(&mut self, session: &str, text: String) -> Value {
        self.starting = true;
        self.start_serial += 1;
        self.completed_before_receipt.clear();
        self.completion_overflow = false;
        let serial = self.start_serial;
        self.refresh_state();
        let mut params = json!({
            "commandId": command_id(),
            "sessionId": session,
            "input": [{"type": "text", "text": text}],
        });
        if let Some(effort) = self.effort_to_send() {
            params["reasoningEffort"] = json!(effort);
        }
        self.request(
            "turn/start",
            params,
            Box::new(move |muse, outcome| match outcome {
                Ok(result) => {
                    let frames = muse.with(|session| {
                        if session.start_serial != serial {
                            return Vec::new();
                        }
                        let Some(turn_id) = result["turnId"].as_str() else {
                            session.starting = false;
                            session.notice(
                                NoticeLevel::Error,
                                notice_kind::SETTING_REFUSED,
                                "Muse took the message without naming a turn.".to_owned(),
                            );
                            session.refresh_state();
                            return Vec::new();
                        };
                        // `disposition` says what happened to the input: a
                        // queued turn is adopted when its `turn/started`
                        // arrives, a steered one was folded into the turn
                        // already open, anything else starts now.
                        match result["disposition"].as_str() {
                            Some("queued") => {
                                session.pending_turn = Some(turn_id.to_owned());
                                Vec::new()
                            }
                            Some("steered") => {
                                session.starting = false;
                                session.refresh_state();
                                Vec::new()
                            }
                            _ => session.begin_turn(turn_id),
                        }
                    });
                    muse.send_all(frames);
                }
                Err(error) => {
                    let frame = muse.with(|session| {
                        if session.start_serial != serial {
                            return None;
                        }
                        session.starting = false;
                        session.interrupt_wanted = false;
                        session.notice(
                            NoticeLevel::Error,
                            notice_kind::SETTING_REFUSED,
                            format!("Muse did not start the turn: {}", error.message),
                        );
                        // A message accepted behind this start still
                        // deserves a turn of its own, not the bin.
                        let queued = std::mem::take(&mut session.queued);
                        match (queued.is_empty(), session.session_id.clone()) {
                            (false, Some(session_id)) => {
                                Some(session.start_frame(&session_id, queued.join("\n\n")))
                            }
                            _ => {
                                session.refresh_state();
                                None
                            }
                        }
                    });
                    if let Some(frame) = frame {
                        muse.send_all(vec![frame]);
                    }
                }
            }),
        )
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
        if matches!(outcome, TurnOutcome::Completed)
            && (self.turn.as_deref() == Some(turn_id) || (self.turn.is_none() && !repeated))
        {
            self.resolve_notice(notice_kind::AUTH_REQUIRED, "Muse is signed in again.");
        }
        if self.turn.as_deref() != Some(turn_id) {
            return Vec::new();
        }
        self.turn = None;
        self.starting = false;
        self.interrupting = false;
        if self.pending_turn.as_deref() == Some(turn_id) {
            self.pending_turn = None;
        }
        self.requests.clear();
        self.streamed.clear();
        self.revisions.clear();
        self.ignored.clear();
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
        if let (false, Some(session)) = (queued.is_empty(), self.session_id.clone()) {
            return vec![self.start_frame(&session, queued.join("\n\n"))];
        }
        self.refresh_state();
        Vec::new()
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

impl Muse {
    fn command(&self, command: ChatCommand) -> Result<(), String> {
        if self.with(|session| session.finished) {
            return Err("the Muse process is not running".to_owned());
        }
        match command {
            ChatCommand::Send { text } => self.send_text(text),
            ChatCommand::SendAttachments { .. } => {
                Err("Muse chats do not take attachments yet; keep the draft".to_owned())
            }
            ChatCommand::DismissNotice { .. } => {
                Err("notice dismissal belongs to the host".to_owned())
            }
            ChatCommand::Interrupt => {
                let frames = self.with(|session| session.interrupt());
                self.send_all(frames);
                Ok(())
            }
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
            ChatCommand::Compact => {
                let frames = self.with(|session| session.compact_frame());
                self.send_all(frames);
                Ok(())
            }
            ChatCommand::Stop => Ok(()),
            ChatCommand::Switch { .. } => Err("the chat host switches providers".to_owned()),
        }
    }

    fn send_text(&self, text: String) -> Result<(), String> {
        let frame = self.with(|session| -> Result<Option<Value>, String> {
            let Some(session_id) = session.session_id.clone() else {
                return Err("muse has no session yet".to_owned());
            };
            if session.interrupting || (session.turn.is_none() && session.starting) {
                session.queued.push(text);
                Ok(None)
            } else if let Some(turn) = session.turn.clone() {
                Ok(Some(session.steer_frame(&session_id, &turn, text)))
            } else {
                Ok(Some(session.start_frame(&session_id, text)))
            }
        })?;
        if let Some(frame) = frame {
            self.send_all(vec![frame]);
        }
        Ok(())
    }

    fn approve(&self, request_id: &str, decision: Decision) -> Result<(), String> {
        enum Plan {
            Decide {
                session_id: String,
                choice_id: String,
                requirement: Value,
                stop_turn: bool,
            },
            RefuseDurableOnly,
            AlreadyDeciding,
        }
        let plan = self.with(|session| -> Result<Plan, String> {
            let open = session
                .requests
                .iter_mut()
                .find(|open| open.request_id == request_id)
                .ok_or_else(|| format!("no open request {request_id}"))?;
            let RequestKind::Approval {
                requirement,
                choices,
                deciding,
                ..
            } = &mut open.kind
            else {
                return Err(format!("request {request_id} is not an approval"));
            };
            if *deciding {
                return Ok(Plan::AlreadyDeciding);
            }
            let session_id = session
                .session_id
                .clone()
                .ok_or("muse has no session yet")?;
            let wanted: &[Decision] = match decision {
                Decision::Accept => &[Decision::Accept],
                Decision::AcceptForSession => &[Decision::AcceptForSession, Decision::Accept],
                Decision::Decline => &[Decision::Decline, Decision::Cancel],
                Decision::Cancel => &[Decision::Cancel, Decision::Decline],
            };
            let picked = wanted.iter().find_map(|want| {
                choices
                    .iter()
                    .find(|(offered, _)| offered == want)
                    .map(|(_, choice_id)| choice_id.clone())
            });
            match picked {
                Some(choice_id) => {
                    *deciding = true;
                    Ok(Plan::Decide {
                        session_id,
                        choice_id,
                        requirement: requirement.clone(),
                        stop_turn: decision == Decision::Cancel
                            && !choices.iter().any(|(d, _)| *d == Decision::Cancel),
                    })
                }
                None if decision == Decision::Accept => Ok(Plan::RefuseDurableOnly),
                None => Err(format!("muse did not offer {decision:?} for {request_id}")),
            }
        })?;
        match plan {
            Plan::AlreadyDeciding => Ok(()),
            Plan::RefuseDurableOnly => {
                self.with(|session| {
                    session.notice(
                        NoticeLevel::Warning,
                        notice_kind::SETTING_REFUSED,
                        "Muse offers only a lasting approval here; choose “for this session” to grant it, or decline.".to_owned(),
                    );
                });
                Ok(())
            }
            Plan::Decide {
                session_id,
                choice_id,
                requirement,
                stop_turn,
            } => {
                let approval_id = request_id.to_owned();
                let frame = self.with(|session| {
                    let approval = approval_id.clone();
                    session.request(
                        "approval/decide",
                        json!({
                            "commandId": command_id(),
                            "sessionId": session_id,
                            "approvalId": approval_id,
                            "choiceId": choice_id,
                            "requirementId": requirement,
                        }),
                        Box::new(move |muse, outcome| match outcome {
                            Ok(_) => muse.with(|session| {
                                session.set_deciding(&approval, false);
                            }),
                            Err(error) => muse.decide_failed(&approval, &error),
                        }),
                    )
                });
                self.send_all(vec![frame]);
                if stop_turn {
                    let frames = self.with(|session| session.interrupt());
                    self.send_all(frames);
                }
                Ok(())
            }
        }
    }

    fn decide_failed(&self, approval_id: &str, error: &RpcError) {
        self.with(|session| {
            session.set_deciding(approval_id, false);
            match error.kind.as_deref() {
                Some("approvalAlreadyResolved") => {
                    let decision = wire_decision(error.data["resolution"]["decision"].as_str());
                    session.close_approval(approval_id, decision);
                }
                Some("approvalRequirementStale") => {
                    // The requirement moved under the click: keep the prompt,
                    // take the fresh requirement, and ask again rather than
                    // dropping the decision silently.
                    let mut presented = None;
                    if let Some(open) = session
                        .requests
                        .iter_mut()
                        .find(|open| open.request_id == approval_id)
                        && let RequestKind::Approval {
                            requirement,
                            presented: shown,
                            deciding,
                            ..
                        } = &mut open.kind
                    {
                        if !error.data["currentRequirementId"].is_null() {
                            *requirement = error.data["currentRequirementId"].clone();
                        }
                        *deciding = false;
                        presented = Some(shown.clone());
                    }
                    if let Some(approval) = presented {
                        session.notice(
                            NoticeLevel::Warning,
                            notice_kind::PROVIDER_WARNING,
                            "The approval changed before Muse took the decision; decide again."
                                .to_owned(),
                        );
                        session.emit(ChatEvent::ApprovalRequested { approval });
                    }
                }
                Some("approvalNotFound") => session.close_approval(approval_id, Decision::Decline),
                _ => session.notice(
                    NoticeLevel::Warning,
                    notice_kind::PROVIDER_WARNING,
                    format!("Muse did not take the decision: {}", error.message),
                ),
            }
        });
    }

    fn answer(&self, request_id: &str, answers: &[Vec<String>]) -> Result<(), String> {
        let (session_id, wire): (String, Vec<Value>) = self.with(|session| {
            let open = session
                .requests
                .iter()
                .find(|open| open.request_id == request_id)
                .ok_or_else(|| format!("no open request {request_id}"))?;
            let RequestKind::Question { ids, questions } = &open.kind else {
                return Err(format!("request {request_id} is not a question"));
            };
            let session_id = session
                .session_id
                .clone()
                .ok_or("muse has no session yet")?;
            let wire = ids
                .iter()
                .zip(questions)
                .enumerate()
                .map(|(index, (id, question))| {
                    let given: &[String] = answers.get(index).map(Vec::as_slice).unwrap_or(&[]);
                    let mut answer = json!({"questionId": id});
                    if question.multi
                        && !given.is_empty()
                        && given.iter().all(|label| question.labels.contains(label))
                    {
                        answer["selectedLabels"] = json!(given);
                    } else if !question.multi
                        && given.len() == 1
                        && question.labels.contains(&given[0])
                    {
                        answer["selectedLabel"] = json!(given[0]);
                    } else {
                        answer["freeText"] = json!(truncate_chars(&given.join(", "), 500));
                    }
                    answer
                })
                .collect();
            Ok((session_id, wire))
        })?;
        let user_input_id = request_id.to_owned();
        let frame = self.with(|session| {
            let id = user_input_id.clone();
            session.request(
                "userInput/answer",
                json!({
                    "commandId": command_id(),
                    "sessionId": session_id,
                    "userInputId": user_input_id,
                    "answers": wire,
                }),
                Box::new(move |muse, outcome| {
                    if let Err(error) = outcome {
                        muse.with(|session| match error.kind.as_deref() {
                            Some("userInputAlreadySettled") | Some("userInputNotFound") => {
                                session.close_question(&id)
                            }
                            _ => session.notice(
                                NoticeLevel::Warning,
                                notice_kind::PROVIDER_WARNING,
                                format!("Muse did not take the answer: {}", error.message),
                            ),
                        });
                    }
                }),
            )
        });
        self.send_all(vec![frame]);
        Ok(())
    }

    fn configure(
        &self,
        model: Option<String>,
        effort: Option<String>,
        approval_mode: Option<ApprovalMode>,
        _fast: Option<bool>,
    ) {
        // MSP has no fast tier; the choice is kept by the host and ignored here.
        let mut frames = Vec::new();
        self.with(|session| {
            let Some(session_id) = session.session_id.clone() else {
                return;
            };
            if let Some(model) = model {
                let before = session.settings.model.replace(model.clone());
                let mut selection = json!({"modelId": model});
                if let Some(row) = session
                    .catalog
                    .iter()
                    .find(|row| row["modelId"].as_str() == Some(model.as_str()))
                {
                    for field in ["providerId", "profileId", "displayLabel"] {
                        if !row[field].is_null() {
                            selection[field] = row[field].clone();
                        }
                    }
                }
                frames.push(session.request(
                    "session/setModel",
                    json!({"commandId": command_id(), "sessionId": session_id, "model": selection}),
                    Box::new(move |muse, outcome| {
                        if let Err(error) = outcome {
                            muse.with(|session| {
                                session.settings.model = before;
                                session.notice(
                                    NoticeLevel::Error,
                                    notice_kind::SETTING_REFUSED,
                                    format!("Muse did not change the model: {}", error.message),
                                );
                            });
                        }
                    }),
                ));
            }
            if let Some(effort) = effort {
                if session.takes_effort(&effort) {
                    let before = session.settings.effort.replace(effort.clone());
                    frames.push(session.request(
                        "session/setReasoningEffort",
                        json!({"commandId": command_id(), "sessionId": session_id, "reasoningEffort": effort}),
                        Box::new(move |muse, outcome| {
                            if let Err(error) = outcome {
                                muse.with(|session| {
                                    session.settings.effort = before;
                                    session.notice(
                                        NoticeLevel::Error,
                                        notice_kind::SETTING_REFUSED,
                                        format!("Muse did not change the reasoning effort: {}", error.message),
                                    );
                                });
                            }
                        }),
                    ));
                } else {
                    session.notice(
                        NoticeLevel::Warning,
                        notice_kind::EFFORT_REFUSED,
                        format!("The reasoning effort {effort} is not one Muse takes here."),
                    );
                }
            }
            if let Some(mode) = approval_mode {
                let before = std::mem::replace(&mut session.settings.mode, mode);
                frames.push(session.request(
                    "session/setApprovalMode",
                    json!({"commandId": command_id(), "sessionId": session_id, "mode": policy(mode)}),
                    Box::new(move |muse, outcome| {
                        if let Err(error) = outcome {
                            muse.with(|session| {
                                session.settings.mode = before;
                                session.notice(
                                    NoticeLevel::Error,
                                    notice_kind::SETTING_REFUSED,
                                    format!("Muse did not change the approval mode: {}", error.message),
                                );
                            });
                        }
                    }),
                ));
            }
        });
        self.send_all(frames);
    }
}

impl Session {
    fn interrupt(&mut self) -> Vec<Value> {
        let Some(session_id) = self.session_id.clone() else {
            return Vec::new();
        };
        if let Some(open) = self
            .requests
            .iter()
            .find(|open| matches!(open.kind, RequestKind::Question { .. }))
        {
            let user_input_id = open.request_id.clone();
            return vec![self.request(
                "userInput/interrupt",
                json!({"commandId": command_id(), "sessionId": session_id, "userInputId": user_input_id}),
                Box::new(|_, _| {}),
            )];
        }
        if let Some(turn) = self.turn.clone() {
            return vec![self.interrupt_frame(&session_id, &turn)];
        }
        self.interrupt_wanted = self.starting;
        Vec::new()
    }

    fn compact_frame(&mut self) -> Vec<Value> {
        let Some(session_id) = self.session_id.clone() else {
            return Vec::new();
        };
        vec![self.request(
            "session/compact",
            json!({"commandId": command_id(), "sessionId": session_id}),
            Box::new(|muse, outcome| {
                if let Err(error) = outcome {
                    muse.with(|session| {
                        session.notice(
                            NoticeLevel::Error,
                            notice_kind::SETTING_REFUSED,
                            format!("Muse could not compact the context: {}", error.message),
                        );
                    });
                }
            }),
        )]
    }

    fn set_deciding(&mut self, request_id: &str, deciding: bool) {
        if let Some(open) = self
            .requests
            .iter_mut()
            .find(|open| open.request_id == request_id)
            && let RequestKind::Approval { deciding: flag, .. } = &mut open.kind
        {
            *flag = deciding;
        }
    }

    fn close_approval(&mut self, request_id: &str, decision: Decision) {
        let before = self.requests.len();
        self.requests.retain(|open| open.request_id != request_id);
        if self.requests.len() != before {
            self.emit(ChatEvent::ApprovalResolved {
                request_id: request_id.to_owned(),
                decision,
            });
            self.refresh_state();
        }
    }

    fn close_question(&mut self, request_id: &str) {
        let before = self.requests.len();
        self.requests.retain(|open| open.request_id != request_id);
        if self.requests.len() != before {
            self.emit(ChatEvent::QuestionResolved {
                request_id: request_id.to_owned(),
            });
            self.refresh_state();
        }
    }

    /// An approval or question to show, from the server request or its
    /// notification twin; both dedupe on the domain id. Returns frames to
    /// send (the `userInput/engaged` notification that disarms a timed
    /// prompt's auto-resolution).
    fn present(&mut self, method: &str, params: &Value) -> Vec<Value> {
        match method {
            "approval/request" | "approval/requested" => {
                self.present_approval(params);
                Vec::new()
            }
            "userInput/request" | "userInput/requested" => self.present_question(params),
            _ => Vec::new(),
        }
    }

    fn present_approval(&mut self, params: &Value) {
        let Some(approval_id) = params["approvalId"].as_str().filter(|id| !id.is_empty()) else {
            return;
        };
        let approval_id = approval_id.to_owned();
        let requirement = params["currentRequirementId"].clone();
        // One prompt per requirement stage: a re-issued approval replaces
        // the stored choices under the same request id.
        let choices = map_choices(params);
        let subject = &params["subject"];
        let kind = match subject["kind"].as_str().unwrap_or_default() {
            "shell" | "process" => ApprovalKind::Command,
            "fileAccess" => ApprovalKind::FileChange,
            "network" | "unixSocket" => ApprovalKind::Permissions,
            _ => ApprovalKind::Tool,
        };
        let title = subject["command"]
            .as_str()
            .or_else(|| subject["path"].as_str())
            .map(str::to_owned)
            .or_else(|| {
                subject["host"]
                    .as_str()
                    .map(|host| match subject["port"].as_u64() {
                        Some(port) => format!("{host}:{port}"),
                        None => host.to_owned(),
                    })
            })
            .or_else(|| subject["toolName"].as_str().map(str::to_owned))
            .or_else(|| params["toolName"].as_str().map(str::to_owned))
            .unwrap_or_else(|| "Muse asks to continue".to_owned());
        let mut detail = String::new();
        if params["subagentOrigin"].is_object() {
            detail.push_str("A subagent asks. ");
        }
        if params["judgeEscalated"].as_bool() == Some(true) {
            detail.push_str("Escalated by Muse's approval judge. ");
        }
        if params["protectedWrite"].as_bool() == Some(true) {
            detail.push_str("This writes somewhere protected. ");
        }
        if let Some(raw) = params["rawArgs"].as_str().filter(|raw| !raw.is_empty()) {
            detail.push_str(raw);
        }
        if let Some(stages) = subject["stages"].as_array() {
            for stage in stages {
                if let Some(argv) = stage["argv"].as_array() {
                    let words: Vec<&str> = argv.iter().filter_map(Value::as_str).collect();
                    if !detail.is_empty() {
                        detail.push('\n');
                    }
                    detail.push_str(&words.join(" "));
                }
            }
        }
        for choice in params["availableChoices"].as_array().into_iter().flatten() {
            if let Some(preview) = choice["rulePreview"].as_str().filter(|p| !p.is_empty()) {
                if !detail.is_empty() {
                    detail.push('\n');
                }
                detail.push_str(preview);
            }
        }
        truncate_bytes(&mut detail, DETAIL_BYTES);
        let offered = offered_of(&choices);
        if offered.is_empty() {
            // A prompt with no answerable choice would wedge the chat in
            // Waiting; say so and leave the turn interruptible instead.
            self.notice(
                NoticeLevel::Warning,
                notice_kind::PROVIDER_WARNING,
                "Muse asked for an approval RiWork cannot answer.".to_owned(),
            );
            return;
        }
        let approval = Approval {
            request_id: approval_id.clone(),
            item_id: params["itemId"].as_str().map(str::to_owned),
            kind,
            title,
            detail,
            choices: offered,
        };
        self.requests.retain(|open| open.request_id != approval_id);
        self.requests.push(OpenRequest {
            request_id: approval_id,
            kind: RequestKind::Approval {
                requirement,
                choices,
                presented: approval.clone(),
                deciding: false,
            },
            blocking: true,
        });
        self.emit(ChatEvent::ApprovalRequested { approval });
        self.refresh_state();
    }

    /// A stage refresh for an open approval: patch its requirement and
    /// choices in place. The params carry no title/detail material, so
    /// re-presenting from them would strip the prompt the user is reading.
    fn update_approval(&mut self, params: &Value) {
        let Some(approval_id) = params["approvalId"].as_str().filter(|id| !id.is_empty()) else {
            return;
        };
        let Some(open) = self
            .requests
            .iter_mut()
            .find(|open| open.request_id == approval_id)
        else {
            return;
        };
        let RequestKind::Approval {
            requirement,
            choices,
            presented,
            ..
        } = &mut open.kind
        else {
            return;
        };
        if !params["currentRequirementId"].is_null() {
            *requirement = params["currentRequirementId"].clone();
        }
        let mapped = map_choices(params);
        if !mapped.is_empty() {
            presented.choices = offered_of(&mapped);
            *choices = mapped;
        }
        let approval = presented.clone();
        self.emit(ChatEvent::ApprovalRequested { approval });
    }

    fn present_question(&mut self, params: &Value) -> Vec<Value> {
        let Some(user_input_id) = params["userInputId"].as_str().filter(|id| !id.is_empty()) else {
            return Vec::new();
        };
        let user_input_id = user_input_id.to_owned();
        let mut ids = Vec::new();
        let mut wire = Vec::new();
        let mut prompts = Vec::new();
        for question in params["questions"].as_array().into_iter().flatten() {
            let Some(id) = question["id"].as_str() else {
                continue;
            };
            let labels: Vec<String> = question["options"]
                .as_array()
                .map(|options| {
                    options
                        .iter()
                        .filter_map(|option| option["label"].as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            let multi = question["selection"]["mode"].as_str() == Some("multiple");
            prompts.push(QuestionPrompt {
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
                            .filter_map(|option| {
                                Some(QuestionOption {
                                    label: option["label"].as_str()?.to_owned(),
                                    description: option["description"]
                                        .as_str()
                                        .unwrap_or_default()
                                        .to_owned(),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                multi_select: multi,
            });
            ids.push(id.to_owned());
            wire.push(WireQuestion { multi, labels });
        }
        if ids.is_empty() {
            return Vec::new();
        }
        self.requests
            .retain(|open| open.request_id != user_input_id);
        self.requests.push(OpenRequest {
            request_id: user_input_id.clone(),
            kind: RequestKind::Question {
                ids,
                questions: wire,
            },
            blocking: true,
        });
        self.emit(ChatEvent::QuestionRequested {
            question: Question {
                request_id: user_input_id.clone(),
                questions: prompts,
            },
        });
        self.refresh_state();
        if params["autoResolutionMs"].is_number() {
            return vec![json!({
                "jsonrpc": "2.0",
                "method": "userInput/engaged",
                "params": {"sessionId": self.session_id, "userInputId": user_input_id},
            })];
        }
        Vec::new()
    }

    fn notification(&mut self, method: &str, params: &Value) -> Vec<Value> {
        // Frames for another session on this connection are not ours.
        if let (Some(theirs), Some(ours)) = (params["sessionId"].as_str(), &self.session_id)
            && theirs != ours
        {
            return Vec::new();
        }
        match method {
            "turn/started" => {
                return self.begin_turn(params["turnId"].as_str().unwrap_or_default());
            }
            "turn/completed" => {
                let turn_id = params["turnId"].as_str().unwrap_or_default().to_owned();
                let outcome = match params["terminal"].as_str() {
                    Some("completed") => TurnOutcome::Completed,
                    Some("cancelled" | "interrupted") => TurnOutcome::Interrupted,
                    // `terminal` is an open enum: anything not plainly a
                    // success is a failure, never a silent completion.
                    _ => {
                        let message = params["error"]["message"]
                            .as_str()
                            .or_else(|| params["reason"].as_str())
                            .unwrap_or("the turn failed")
                            .to_owned();
                        if params["error"]["kind"].as_str() == Some("authRequired") {
                            self.error_shown = true;
                            self.notice(
                                NoticeLevel::Error,
                                notice_kind::AUTH_REQUIRED,
                                format!("Muse reported that authentication is required: {message}"),
                            );
                        }
                        TurnOutcome::Failed { message }
                    }
                };
                return self.finish_turn(&turn_id, outcome);
            }
            "turn/retryScheduled" => self.notice(
                NoticeLevel::Warning,
                notice_kind::API_RETRY,
                "Muse is retrying after a failed request.".to_owned(),
            ),
            "item/started" | "item/updated" => self.item(&params["item"], false),
            "item/completed" => self.item(&params["item"], true),
            "item/delta" => self.delta(params),
            "approval/requested" => self.present_approval(params),
            "approval/updated" => self.update_approval(params),
            "approval/resolved" => {
                if let Some(id) = params["approvalId"].as_str() {
                    self.close_approval(id, wire_decision(params["decision"].as_str()));
                }
            }
            "userInput/requested" => return self.present_question(params),
            "userInput/settled" => {
                if let Some(id) = params["userInputId"].as_str() {
                    self.close_question(id);
                }
            }
            "session/tokenUsage" => {
                let cumulative = &params["cumulative"];
                let mut usage = self.usage.clone();
                usage.input_tokens = cumulative["promptTokens"].as_u64().unwrap_or(0);
                usage.output_tokens = cumulative["outputTokens"].as_u64().unwrap_or(0);
                usage.cached_input_tokens = cumulative["cacheReadTokens"].as_u64().unwrap_or(0);
                usage.cost_usd = cumulative["cost"]
                    .as_f64()
                    .or_else(|| cumulative["cost"]["usd"].as_f64())
                    .or(usage.cost_usd);
                if usage != self.usage {
                    self.usage = usage.clone();
                    self.usage_seen = true;
                    self.emit(ChatEvent::Usage { usage });
                }
            }
            "session/contextUsage" => {
                let mut usage = self.usage.clone();
                if let Some(used) = params["usedTokens"].as_u64() {
                    usage.context_used = Some(used);
                }
                if let Some(window) = params["windowTokens"].as_u64() {
                    usage.context_window = Some(window);
                }
                if usage != self.usage {
                    self.usage = usage.clone();
                    self.usage_seen = true;
                    self.emit(ChatEvent::Usage { usage });
                }
                if params["pressure"].as_str() == Some("blocked") {
                    self.notice(
                        NoticeLevel::Warning,
                        notice_kind::PROVIDER_WARNING,
                        "Muse's context window is full; compact the chat to continue.".to_owned(),
                    );
                }
            }
            "session/nameChanged" => {
                if let Some(name) = params["name"].as_str().filter(|name| !name.is_empty()) {
                    self.emit(ChatEvent::ProviderTitle {
                        title: name.to_owned(),
                    });
                }
            }
            "session/modelRouteUnserved" => self.notice(
                NoticeLevel::Warning,
                notice_kind::MODEL_FALLBACK,
                params["reason"]
                    .as_str()
                    .or_else(|| params["message"].as_str())
                    .unwrap_or("Muse could not serve the chosen model and fell back.")
                    .to_owned(),
            ),
            "session/todoListChanged" => self.todos(params),
            "session/closed" => self.finish_stopped(),
            "view/gap" => self.notice(
                NoticeLevel::Warning,
                notice_kind::PROVIDER_WARNING,
                "Some Muse events were dropped; the transcript may be missing a step.".to_owned(),
            ),
            _ => {}
        }
        Vec::new()
    }

    fn todos(&mut self, params: &Value) {
        let Some(list) = params["items"]
            .as_array()
            .or_else(|| params["todos"].as_array())
        else {
            return;
        };
        let items: Vec<Step> = list
            .iter()
            .filter_map(|todo| {
                let status = match todo["status"].as_str() {
                    Some("pending") => StepStatus::Pending,
                    Some("inProgress") => StepStatus::InProgress,
                    Some("completed") => StepStatus::Completed,
                    _ => return None,
                };
                let text = todo["text"]
                    .as_str()
                    .or_else(|| todo["title"].as_str())?
                    .to_owned();
                Some(Step { text, status })
            })
            .collect();
        self.emit(ChatEvent::ItemCompleted {
            item: Item {
                presentation: Default::default(),
                id: "muse-todo".to_owned(),
                turn_id: self.turn.clone(),
                status: ItemStatus::Completed,
                body: ItemBody::Todo { items },
            },
        });
    }

    fn item(&mut self, item: &Value, done: bool) {
        let Some(id) = item["itemId"].as_str().filter(|id| !id.is_empty()) else {
            return;
        };
        let id = id.to_owned();
        if self.ignored.contains(&id) {
            if done {
                self.ignored.remove(&id);
            }
            return;
        }
        if let Some(revision) = item["revision"].as_u64() {
            let stored = self.revisions.get(&id).copied().unwrap_or(0);
            if !done && revision <= stored && stored > 0 {
                return;
            }
            self.revisions.insert(id.clone(), revision);
        }
        let Some((mut body, status)) = convert_item(item, done) else {
            self.ignored.insert(id);
            return;
        };
        let reasoning = matches!(body, ItemBody::Reasoning { .. });
        if !done {
            let entry = self.streamed.entry(id.clone()).or_default();
            entry.reasoning = reasoning;
            if reasoning
                && matches!(&body, ItemBody::Reasoning { text } if text.is_empty())
                && entry.text.is_empty()
            {
                return; // shown with its first delta instead
            }
            entry.started = true;
            self.emit(ChatEvent::ItemStarted {
                item: Item {
                    presentation: Default::default(),
                    id,
                    turn_id: item["turnId"].as_str().map(str::to_owned),
                    status,
                    body,
                },
            });
            return;
        }
        if let Some(streamed) = self.streamed.remove(&id) {
            match &mut body {
                ItemBody::AgentMessage { text } | ItemBody::Reasoning { text }
                    if text.is_empty() =>
                {
                    *text = streamed.text;
                }
                ItemBody::Command { output, .. } if output.is_empty() => {
                    *output = streamed.text;
                }
                ItemBody::ToolCall { output, .. }
                    if output.is_none() && !streamed.text.is_empty() =>
                {
                    *output = Some(streamed.text);
                }
                _ => {}
            }
        }
        self.emit(ChatEvent::ItemCompleted {
            item: Item {
                presentation: Default::default(),
                id,
                turn_id: item["turnId"].as_str().map(str::to_owned),
                status,
                body,
            },
        });
    }

    fn delta(&mut self, params: &Value) {
        let Some(id) = params["itemId"].as_str() else {
            return;
        };
        if self.ignored.contains(id) {
            return;
        }
        let Some(text) = params["delta"].as_str() else {
            return;
        };
        let field = params["field"].as_str().unwrap_or_default();
        let entry = self.streamed.entry(id.to_owned()).or_default();
        let (chunk, output) = if field == "output" {
            (text.to_owned(), true)
        } else if let Some(part) = field
            .strip_prefix("summary.")
            .and_then(|rest| rest.parse::<i64>().ok())
        {
            let separator =
                entry.summary_part.is_some_and(|seen| seen != part) && !entry.text.is_empty();
            entry.summary_part = Some(part);
            (
                if separator {
                    format!("\n\n{text}")
                } else {
                    text.to_owned()
                },
                false,
            )
        } else if field.is_empty() || field == "text" {
            (text.to_owned(), false)
        } else {
            return;
        };
        entry.text.push_str(&chunk);
        if entry.text.len() > STREAM_KEEP_BYTES {
            let excess = entry.text.len() - STREAM_KEEP_BYTES;
            let cut = (excess..entry.text.len())
                .find(|at| entry.text.is_char_boundary(*at))
                .unwrap_or(entry.text.len());
            entry.text.drain(..cut);
        }
        if !entry.started {
            entry.started = true;
            // The item has not been seen yet, so the field is all we know:
            // output bytes are a tool's, summary parts are reasoning, and
            // only plain text is agent prose. Completion replaces the body.
            let body = if output {
                ItemBody::ToolCall {
                    server: None,
                    tool: "tool".to_owned(),
                    input: Value::Null,
                    output: Some(entry.text.clone()),
                }
            } else if entry.reasoning || entry.summary_part.is_some() {
                entry.reasoning = true;
                ItemBody::Reasoning {
                    text: entry.text.clone(),
                }
            } else {
                ItemBody::AgentMessage {
                    text: entry.text.clone(),
                }
            };
            self.emit(ChatEvent::ItemStarted {
                item: Item {
                    presentation: Default::default(),
                    id: id.to_owned(),
                    turn_id: self.turn.clone(),
                    status: ItemStatus::InProgress,
                    body,
                },
            });
            return;
        }
        self.emit(ChatEvent::ItemDelta {
            item_id: id.to_owned(),
            delta: if output {
                Delta::Output(chunk)
            } else {
                Delta::Text(chunk)
            },
        });
    }
}

/// How much Muse may do without asking, in its own closed vocabulary.
/// Plan has no true equivalent; `denyUnmatched` at least changes nothing
/// the rules do not already allow.
fn policy(mode: ApprovalMode) -> &'static str {
    match mode {
        ApprovalMode::Supervised => "promptUnmatched",
        ApprovalMode::AutoEdit => "onRequest",
        ApprovalMode::Full => "allowAll",
        ApprovalMode::Plan => "denyUnmatched",
    }
}

/// The offered MSP choices as (decision, choiceId) pairs; a pair that
/// does not map is dropped, never guessed.
fn map_choices(params: &Value) -> Vec<(Decision, String)> {
    params["availableChoices"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|choice| {
                    let decision = map_choice(
                        choice["decision"].as_str().unwrap_or_default(),
                        choice["scope"].as_str().unwrap_or_default(),
                    )?;
                    let choice_id = choice["choiceId"].as_str()?.to_owned();
                    Some((decision, choice_id))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The distinct decisions a choice list offers, in wire order.
fn offered_of(choices: &[(Decision, String)]) -> Vec<Decision> {
    let mut seen: Vec<Decision> = Vec::new();
    for (decision, _) in choices {
        if !seen.contains(decision) {
            seen.push(*decision);
        }
    }
    seen
}

/// A wire decision as the transcript names it (for folds that close an
/// approval we did not decide ourselves).
fn wire_decision(decision: Option<&str>) -> Decision {
    match decision {
        Some("approved") => Decision::Accept,
        Some("approvedForSession" | "approvedPolicyAmendment") => Decision::AcceptForSession,
        Some("abort") => Decision::Cancel,
        _ => Decision::Decline,
    }
}

/// One offered MSP choice as a RiWork decision, on the (decision, scope)
/// pair. Unknown pairs are skipped, never guessed into an accept; a lasting
/// grant reads as AcceptForSession so the transcript never understates it
/// as a one-off.
fn map_choice(decision: &str, scope: &str) -> Option<Decision> {
    match (decision, scope) {
        ("approved", "once") => Some(Decision::Accept),
        ("approvedForSession", _) | ("approved", "session") => Some(Decision::AcceptForSession),
        ("approvedPolicyAmendment", _) | ("approved", "localPersistent") => {
            Some(Decision::AcceptForSession)
        }
        ("denied", _) | ("deniedPolicyAmendment", _) => Some(Decision::Decline),
        ("abort", _) => Some(Decision::Cancel),
        _ => None,
    }
}

fn model_options(result: &Value) -> (Vec<ModelOption>, Vec<Value>) {
    let rows: Vec<Value> = result["models"].as_array().cloned().unwrap_or_default();
    let models = rows
        .iter()
        .filter_map(|row| {
            let id = row["modelId"].as_str().filter(|id| !id.is_empty())?;
            let efforts: Vec<String> = match row["variants"].as_array() {
                Some(variants) => variants
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|effort| EFFORTS.contains(effort))
                    .map(str::to_owned)
                    .collect(),
                None => Vec::new(),
            };
            Some(ModelOption {
                id: id.to_owned(),
                name: row["displayLabel"]
                    .as_str()
                    .filter(|name| !name.is_empty())
                    .unwrap_or(id)
                    .to_owned(),
                description: row["description"].as_str().unwrap_or_default().to_owned(),
                efforts,
                default_effort: row["defaultReasoningEffort"].as_str().map(str::to_owned),
                supports_fast: false,
                is_default: row["isDefault"].as_bool().unwrap_or(false),
            })
        })
        .collect();
    (models, rows)
}

fn item_status(item: &Value, done: bool) -> ItemStatus {
    match item["status"].as_str() {
        Some("inProgress") => ItemStatus::InProgress,
        Some("completed") => ItemStatus::Completed,
        Some("failed" | "timedOut") => ItemStatus::Failed,
        Some("rejected") => ItemStatus::Declined,
        Some("cancelled") => ItemStatus::Interrupted,
        _ if done => ItemStatus::Completed,
        _ => ItemStatus::InProgress,
    }
}

fn convert_item(item: &Value, done: bool) -> Option<(ItemBody, ItemStatus)> {
    let status = item_status(item, done);
    let text = |item: &Value| item["text"].as_str().unwrap_or_default().to_owned();
    let body = match item["kind"].as_str().unwrap_or_default() {
        "userMessage" => ItemBody::UserMessage {
            text: item["displayText"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| text(item)),
        },
        "agentMessage" => ItemBody::AgentMessage { text: text(item) },
        "reasoning" => {
            let summary: Vec<&str> = item["summary"]
                .as_array()
                .map(|parts| parts.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            ItemBody::Reasoning {
                text: if summary.is_empty() {
                    text(item)
                } else {
                    summary.join("\n\n")
                },
            }
        }
        "toolCall" => {
            let tool = item["tool"].as_str().unwrap_or("tool").to_owned();
            let input: Value = item["args"]
                .as_str()
                .and_then(|args| serde_json::from_str(args).ok())
                .unwrap_or_else(|| match item["args"].as_str() {
                    Some(args) => Value::String(args.to_owned()),
                    None => item["args"].clone(),
                });
            let mut output = item["visibleOutput"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            if let Some(reason) = item["failureReason"]
                .as_str()
                .filter(|reason| !reason.is_empty())
            {
                if !output.is_empty() {
                    output.push('\n');
                }
                output.push_str(reason);
            }
            let shell_like = {
                let name = tool.to_ascii_lowercase();
                ["shell", "bash", "exec", "terminal", "command"]
                    .iter()
                    .any(|hint| name.contains(hint))
            };
            if !item["patchRef"].is_null() || !item["patchSummary"].is_null() {
                // The patch document itself needs item/readOutput, whose
                // inner schema is untyped; until a real one is captured the
                // summary rides a ToolCall rather than inventing FileChanges.
                let summary = &item["patchSummary"];
                let mut input = input;
                if !summary.is_null() {
                    input = json!({"patch_summary": summary, "arguments": input});
                }
                ItemBody::ToolCall {
                    server: None,
                    tool,
                    input,
                    output: (!output.is_empty()).then_some(output),
                }
            } else if shell_like
                && let Some(command) = input["command"].as_str().filter(|c| !c.is_empty())
            {
                ItemBody::Command {
                    command: command.to_owned(),
                    cwd: input["cwd"].as_str().map(str::to_owned),
                    output,
                    exit_code: None,
                }
            } else {
                ItemBody::ToolCall {
                    server: None,
                    tool,
                    input,
                    output: (!output.is_empty()).then_some(output),
                }
            }
        }
        "userShell" => ItemBody::Command {
            command: item["commandText"].as_str().unwrap_or_default().to_owned(),
            cwd: None,
            output: item["visibleOutput"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            exit_code: item["exitCode"].as_i64().map(|code| code as i32),
        },
        "webSearch" => match item["query"].as_str().filter(|query| !query.is_empty()) {
            Some(query) => ItemBody::WebSearch {
                query: query.to_owned(),
            },
            None => ItemBody::ToolCall {
                server: None,
                tool: "webSearch".to_owned(),
                input: item.clone(),
                output: item["fallbackText"].as_str().map(str::to_owned),
            },
        },
        // Only a completed compaction earns the marker; a no-op or failed
        // one must not claim the context was compacted.
        "compaction" => match item["outcome"].as_str() {
            None | Some("compacted") => ItemBody::Compaction,
            _ => return None,
        },
        "subagent" => ItemBody::ToolCall {
            server: None,
            tool: item["role"].as_str().unwrap_or("subagent").to_owned(),
            input: json!({"objective": item["objective"]}),
            output: item["result"]["summary"]
                .as_str()
                .or_else(|| item["result"]["text"].as_str())
                .or_else(|| item["text"].as_str())
                .map(str::to_owned),
        },
        "workflow" => ItemBody::ToolCall {
            server: None,
            tool: "workflow".to_owned(),
            input: item.clone(),
            output: item["finalSummary"]
                .as_str()
                .or_else(|| item["message"].as_str())
                .map(str::to_owned),
        },
        "reminderChild" | "hookRun" | "sideChat" => return None,
        kind => ItemBody::ToolCall {
            server: None,
            tool: kind.to_owned(),
            input: item.clone(),
            output: item["fallbackText"].as_str().map(str::to_owned),
        },
    };
    Some((body, status))
}

fn truncate_chars(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    text.chars().take(limit).collect()
}

fn truncate_bytes(text: &mut String, limit: usize) {
    if text.len() <= limit {
        return;
    }
    let cut = (0..=limit)
        .rev()
        .find(|at| text.is_char_boundary(*at))
        .unwrap_or(0);
    text.truncate(cut);
    text.push_str("\n… (truncated)");
}

#[cfg(test)]
#[path = "muse_tests.rs"]
mod tests;
