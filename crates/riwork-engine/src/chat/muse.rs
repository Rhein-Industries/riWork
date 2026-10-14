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
//! `SendAttachments` → the same with MSP `image` parts, answered only once
//! Muse has taken or refused it, `Interrupt` → `turn/interrupt` (or
//! `userInput/interrupt` while a question pends), `Approve` →
//! `approval/decide`, `Answer` → `userInput/answer`, `Configure` →
//! `session/setModel`/`setReasoningEffort`/`setApprovalMode`, `Compact` →
//! `session/compact`. Approvals are answered in two steps: the JSON-RPC
//! receipt acknowledges presentation only; the decision travels as its own
//! command, and the prompt closes when the server folds `approval/resolved` —
//! never on our decide ack. A choice that would save a rule
//! (`localPersistent`) is never sent for a RiWork decision.
//!
//! RiWork's Cua.ai Driver reaches the session as a session-only MCP server
//! (`config.mcpServers` on `session/start` and `session/resume`, granted by
//! the `sessionMcp` capability), never through Muse's settings. The
//! conversation a chat had before it switched provider rides the first turn
//! as a leading text part; `displayText` and the user-message echo keep the
//! user's own words on screen.
//!
//! Approval modes map onto MSP's closed enum: Supervised →
//! `promptUnmatched`, AutoEdit → `onRequest`, Full → `allowAll`. Muse has no
//! planning mode: Plan starts the session under Muse's `:read-only`
//! permission profile with `denyUnmatched`, and since a profile is fixed when
//! a session starts, a running conversation does not move into or out of
//! Plan. Muse has no fast tier. Contributor mode is fixed for a `muse serve` process and
//! MSP has no field for it: every process, first and at each resume, starts with
//! `--contributor-mode` and the chat's mode (`DriverConfig::contributor_mode`), off unless the
//! user turned it on, so the user's Muse settings never decide.
//!
//! Verified against `muse serve --provider echo` (Muse 1.4.7, schema
//! `sha256:6de32db0…`): framing, the handshake and its `sessionMcp` grant, a
//! session MCP server started with exactly the configured command, arguments
//! and environment at start and again at resume, echo turns with deltas and
//! usage, `displayText`, image parts up to a 4.4 MB frame, steering into a
//! running turn, the `:read-only` profile refusing a write, and the refusals
//! of a steer or interrupt without a running turn. Assumed from the MSP schema
//! (echo cannot produce them): toolCall item shapes, approval and question
//! inventories and their answers, compaction, non-zero usage and model rows.

use super::attachments::{self, Attachment, AttachmentKind, UNKNOWN_SUBMISSION};
use super::child::{self, Frame, FrameReader, Proc};
use super::driver::{Driver, DriverConfig, DriverReport, DriverReporter, StartDriver};
use super::model::{
    Approval, ApprovalKind, ApprovalMode, ChatCommand, ChatEvent, ChatImage, ChatState, Decision,
    Delta, ImageSource, Item, ItemBody, ItemStatus, ModelOption, NoticeLevel, Presentation,
    Question, QuestionOption, QuestionPrompt, Step, StepStatus, TurnOutcome, Usage, notice_kind,
};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::process::ChildStdout;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use uuid::Uuid;

const _: StartDriver = start;

/// Muse's permission profile for Plan: "Reads files only".
const READ_ONLY_PROFILE: &str = ":read-only";
/// The most characters `userInput/answer` takes as one free-text answer.
const FREE_TEXT_CHARS: usize = 500;
/// How many submissions keep their on-screen form for the user-message echo.
const SHOWN_KEEP: usize = 64;

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

/// A frame that did not reach Muse whole: why, whether part of it did, and
/// the typed message it carried.
struct Unwritten {
    error: String,
    partial: bool,
    text: Option<String>,
}

fn command_id() -> String {
    Uuid::now_v7().to_string()
}

/// RiWork's Cua.ai Driver as Muse's `SessionConfig.mcpServers`: `executable
/// cua mcp` with the chat's RiWork state, the same server a terminal launch
/// names with `-c`, but for this session only and with literal values (a
/// session server's environment is its own, not Muse's `${VAR}` settings).
/// `None` without a RiWork home to point it at.
fn cua_servers(
    config: &DriverConfig,
    executable: &Path,
    custom_driver: Option<&Path>,
) -> Option<Value> {
    let home = config
        .env
        .iter()
        .find(|(name, _)| name == "RIWORK_HOME")
        .map(|(_, value)| value.to_string_lossy().into_owned())?;
    let mut env = json!({"RIWORK_HOME": home});
    if let Some(driver) = custom_driver {
        env["RIWORK_CUA_DRIVER"] = json!(driver.to_string_lossy());
    }
    Some(json!({"cua-driver": {
        "transport": "stdio",
        "command": executable.to_string_lossy(),
        "args": ["cua", "mcp"],
        "env": env,
        "mode": "required",
    }}))
}

pub fn start(config: DriverConfig, events: Sender<ChatEvent>) -> Result<Box<dyn Driver>, String> {
    let servers = std::env::current_exe().ok().and_then(|executable| {
        cua_servers(
            &config,
            &executable,
            crate::cua::driver_override_for_harness().as_deref(),
        )
    });
    start_with(config, events, servers)
}

/// `start` with `servers` as the session's MCP servers; a live test names a
/// private stand-in for `riwork cua mcp` here.
fn start_with(
    config: DriverConfig,
    events: Sender<ChatEvent>,
    servers: Option<Value>,
) -> Result<Box<dyn Driver>, String> {
    let mut driver = spawn_driver(&config, events, servers)?;
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

fn spawn_driver(
    config: &DriverConfig,
    events: Sender<ChatEvent>,
    servers: Option<Value>,
) -> Result<MuseDriver, String> {
    let _ = events.send(ChatEvent::State {
        state: ChatState::Starting,
    });
    let (proc, stdout) =
        Proc::spawn(child::command(config, &serve_args(config))).inspect_err(|error| {
            let _ = events.send(ChatEvent::State {
                state: ChatState::Failed {
                    message: error.clone(),
                },
            });
        })?;
    let muse = Arc::new(Muse {
        proc,
        servers,
        session: Mutex::new(Session::new(config, events)),
    });
    let reader_muse = muse.clone();
    let reader = std::thread::spawn(move || reader_muse.read_loop(stdout));
    Ok(MuseDriver {
        muse,
        reader: Some(reader),
        stopped: false,
    })
}

/// `muse serve` with Muse's per-process contributor mode override, always exactly once, and
/// `--trust-workspace` when the chat's folder is to be trusted; the subcommand comes first, its
/// flags after it (Muse 1.4.7 reads options before a subcommand as the interactive screen's).
fn serve_args(config: &DriverConfig) -> Vec<&'static str> {
    let mut arguments = vec![
        "serve",
        "--contributor-mode",
        config.contributor_mode.as_str(),
    ];
    if config.trust_workspace {
        arguments.push("--trust-workspace");
    }
    arguments
}

/// Read the provider catalog before any chat exists. MSP model/list needs no
/// session: this sends no session/start, resume, turn or tool command, and no
/// Cua server is attached. The ordinary process owner reaps it on every result.
pub(super) fn discover_models(config: DriverConfig) -> Result<Vec<ModelOption>, String> {
    let (events, _) = mpsc::channel();
    let mut driver = spawn_driver(&config, events, None)?;
    let muse = &driver.muse;
    let result = (|| {
        muse.call(
            "initialize",
            json!({"clientInfo":{"name":"riwork","version":env!("CARGO_PKG_VERSION")}}),
            Duration::from_secs(5),
        )
        .map_err(|error| error.message)?;
        muse.proc
            .send(&json!({"jsonrpc":"2.0","method":"initialized"}))?;
        let result = muse
            .call("model/list", json!({}), Duration::from_secs(5))
            .map_err(|error| error.message)?;
        if !result["models"].is_array() {
            return Err("Muse returned no model catalog".to_owned());
        }
        Ok(model_options(&result).0)
    })();
    driver.shutdown();
    result
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

    fn configure(&mut self, command: ChatCommand, reporter: DriverReporter) -> Result<(), String> {
        let ChatCommand::Configure {
            model,
            effort,
            approval_mode,
            fast,
            contributor_mode: _,
            contributor_opt_out: _,
            claude_billing: _,
        } = command
        else {
            return self.command(command);
        };
        self.muse
            .configure(model, effort, approval_mode, fast, reporter)
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
    /// `SessionConfig.mcpServers` for start and resume (RiWork's Cua.ai Driver).
    servers: Option<Value>,
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
        let mut frames = frames.into_iter();
        let Some(failed) = frames.by_ref().find_map(|frame| self.write(&frame).err()) else {
            return;
        };
        // Frames after a failed write were never sent: undo what they set up.
        let rest: Vec<Value> = frames.collect();
        let (more, mut unsent) = self.with(|session| {
            let mut more = Vec::new();
            let mut unsent = Vec::new();
            for frame in &rest {
                let (frames, text) = session.unwrite(frame);
                more.extend(frames);
                unsent.extend(text);
            }
            (more, unsent)
        });
        self.with(|session| {
            if session.stopping || session.finished {
                return;
            }
            match failed.text {
                Some(text) if failed.partial => session.undelivered(
                    &format!(
                        "RiWork's write to muse broke off ({}); this message may have reached it, so check the transcript before sending it again:",
                        failed.error
                    ),
                    &[text],
                ),
                Some(text) => unsent.insert(0, text),
                None => {}
            }
            if unsent.is_empty() {
                session.notice(
                    NoticeLevel::Error,
                    notice_kind::SETTING_REFUSED,
                    format!("RiWork could not write to muse ({}).", failed.error),
                );
            } else {
                session.undelivered(
                    &format!("RiWork could not write to muse ({}); not sent:", failed.error),
                    &unsent,
                );
            }
        });
        // Messages that waited behind an unwritten start; a second failure
        // leaves nothing queued to retry.
        self.send_all(more);
    }

    /// Writes one frame. When none of it reached Muse, what it set up is
    /// undone (`Session::unwrite`); when part of it did, it is an unknown
    /// submission, never written again.
    fn write(&self, frame: &Value) -> Result<(), Unwritten> {
        let Err(error) = self
            .proc
            .send_line_until(&frame.to_string(), Instant::now() + child::WRITE_WAIT)
        else {
            return Ok(());
        };
        if error.written > 0 {
            let text = self.with(|session| {
                frame["id"]
                    .as_i64()
                    .and_then(|id| session.in_flight.remove(&id))
            });
            return Err(Unwritten {
                error: error.to_string(),
                partial: true,
                text,
            });
        }
        let (more, text) = self.with(|session| session.unwrite(frame));
        self.send_all(more);
        Err(Unwritten {
            error: error.to_string(),
            partial: false,
            text,
        })
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
        let initialized = self
            .call(
                "initialize",
                json!({
                    "clientInfo": {"name": "riwork", "version": env!("CARGO_PKG_VERSION")},
                    "capabilities": {"requestedCapabilities": ["sessionMcp"]},
                }),
                INITIALIZE_TIMEOUT,
            )
            .map_err(|error| error.message)?;
        let _ = self
            .proc
            .send(&json!({"jsonrpc": "2.0", "method": "initialized"}));
        // Session MCP servers are refused without the grant; a Muse that does
        // not give it runs the chat without RiWork's Cua.ai Driver, and says so.
        let session_mcp = initialized["grantedCapabilities"]
            .as_array()
            .is_some_and(|granted| granted.iter().any(|name| name == "sessionMcp"));
        let session_config = self
            .servers
            .as_ref()
            .filter(|_| session_mcp)
            .map(|servers| json!({"mcpServers": servers}));
        let observed: Value;
        let (session_id, name, fresh, active_turn, resumed, unsent) = match &config.resume {
            Some(resume) => {
                let mut params =
                    json!({"commandId": command_id(), "sessionId": resume, "excludeItems": true});
                if let Some(session_config) = &session_config {
                    params["config"] = session_config.clone();
                }
                match self.call("session/resume", params, SESSION_TIMEOUT) {
                    Ok(result) => {
                        observed = result["session"].clone();
                        let active = result["session"]["activeTurnId"]
                            .as_str()
                            .filter(|id| !id.is_empty())
                            .map(str::to_owned);
                        // No turn has run or runs yet: the carried-over
                        // conversation has not reached Muse.
                        let unsent =
                            active.is_none() && result["session"]["turnCount"].as_u64() == Some(0);
                        (
                            session_id_of(&result).ok_or("muse resumed without a session id")?,
                            result["session"]["name"].as_str().map(str::to_owned),
                            false,
                            active,
                            true,
                            unsent,
                        )
                    }
                    Err(error) if error.kind.as_deref() == Some("sessionNotFound") => {
                        if config.resume_has_history {
                            return Err(format!(
                                "Muse could not find historical session {resume}. Its identity and transcript were kept; restore Muse's session store and retry."
                            ));
                        }
                        let (id, name, metadata) =
                            self.start_session(config, session_config.as_ref())?;
                        observed = metadata;
                        (id, name, true, None, false, true)
                    }
                    Err(error) => return Err(error.message),
                }
            }
            None => {
                let (id, name, metadata) = self.start_session(config, session_config.as_ref())?;
                observed = metadata;
                (id, name, false, None, false, true)
            }
        };
        let adopted = self.with(|session| {
            session.session_id = Some(session_id.clone());
            session.observe_metadata(&observed);
            session.ready = true;
            if fresh {
                session.notice(
                    NoticeLevel::Info,
                    notice_kind::RESUMED_FRESH,
                    "Muse no longer has this conversation; a fresh one was started.".to_owned(),
                );
            }
            if self.servers.is_some() && !session_mcp {
                session.notice(
                    NoticeLevel::Warning,
                    notice_kind::PROVIDER_WARNING,
                    "This Muse does not take MCP servers for one session, so RiWork's Cua.ai Driver is not connected to this chat.".to_owned(),
                );
            }
            if let Some(name) = &name
                && !name.is_empty()
            {
                session.emit(ChatEvent::ProviderTitle {
                    title: name.clone(),
                });
            }
            // The conversation from before a provider switch goes with the
            // first message of the session, which keeps it from then on.
            if unsent {
                session.context = config
                    .instructions
                    .clone()
                    .filter(|text| !text.trim().is_empty());
            }
            if !resumed && config.approval_mode == ApprovalMode::Plan {
                session.notice(
                    NoticeLevel::Info,
                    notice_kind::PROVIDER_WARNING,
                    "Muse has no planning mode. This conversation runs under Muse's Read only permission profile, which reads files only, and actions Muse's rules do not allow are declined instead of asked. It stays read-only for its whole life.".to_owned(),
                );
            }
            if !resumed && config.fast {
                session.notice(
                    NoticeLevel::Warning,
                    notice_kind::FAST_MODE,
                    "Muse has no fast mode; this chat runs at Muse's usual speed.".to_owned(),
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
        // A resume observes the provider's standing values first. Saved selections
        // become effective only once the corresponding reconfigure is accepted.
        if resumed {
            self.configure(
                config.model.clone(),
                None,
                None,
                None,
                config.reporter.clone(),
            )?;
            self.configure(
                None,
                None,
                Some(config.approval_mode),
                None,
                config.reporter.clone(),
            )?;
        }
        if config
            .effort
            .as_ref()
            .is_some_and(|effort| EFFORTS.contains(&effort.as_str()))
        {
            self.configure(
                None,
                config.effort.clone(),
                None,
                None,
                config.reporter.clone(),
            )?;
        }
        if let Some(effort) = &config.effort
            && !EFFORTS.contains(&effort.as_str())
        {
            self.with(|session| session.notice(NoticeLevel::Warning, notice_kind::EFFORT_REFUSED,
                format!("The saved reasoning effort {effort} is not one Muse takes; its default is used.")));
            config.reporter.report(DriverReport {
                effort: Some(None),
                ..Default::default()
            });
        }
        self.list_models();
        Ok(())
    }

    fn start_session(
        &self,
        config: &DriverConfig,
        session_config: Option<&Value>,
    ) -> Result<(String, Option<String>, Value), String> {
        let mut params = json!({
            "commandId": command_id(),
            "workspaceRoot": config.cwd.to_string_lossy(),
            "approvalMode": policy(config.approval_mode),
        });
        if let Some(model) = &config.model {
            params["modelId"] = json!(model);
        }
        if let Some(session_config) = session_config {
            params["config"] = session_config.clone();
        }
        // A profile is chosen once, here: `session/resume` keeps it and
        // does not take another. A host that cannot give it refuses the start.
        if config.approval_mode == ApprovalMode::Plan {
            params["permissionProfile"] = json!(READ_ONLY_PROFILE);
        }
        let result = self
            .call("session/start", params, SESSION_TIMEOUT)
            .map_err(|error| error.message)?;
        Ok((
            session_id_of(&result).ok_or("muse started without a session id")?,
            result["session"]["name"].as_str().map(str::to_owned),
            result["session"].clone(),
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
                Box::new(|muse, outcome| match outcome {
                    Ok(result) => {
                        let (models, rows) = model_options(&result);
                        muse.with(|session| {
                            session.catalog = rows;
                            session.set_models(models);
                        });
                    }
                    Err(error) => muse.with(|session| {
                        session.notice(
                            NoticeLevel::Warning,
                            notice_kind::PROVIDER_WARNING,
                            format!("Muse could not list its models: {}", error.message),
                        )
                    }),
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
                if let Some(reply) = self.with(|session| {
                    session.in_flight.remove(&id);
                    session.waiting.remove(&id)
                }) {
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
        let theirs = params["sessionId"].as_str();
        let ours = self.with(|session| session.session_id.clone());
        match method {
            // A request for another session is not this chat's to show.
            "approval/request" | "userInput/request"
                if theirs.is_some() && ours.is_some() && theirs != ours.as_deref() =>
            {
                frames.push(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {"code": -32602, "message": "riwork serves another session on this connection"},
                }));
            }
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
        choices: Vec<Choice>,
        /// The request as it came, so a stage refresh (which carries the
        /// new subject but not the raw arguments) re-describes it whole.
        request: Value,
        /// The prompt as presented.
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
    /// How the question reads, for a refusal to name it.
    name: String,
    multi: bool,
    labels: Vec<String>,
    min: Option<u64>,
    max: Option<u64>,
}

/// One `UserInputAnswer`: exactly one of `selectedLabel` (single choice),
/// `selectedLabels` (multiple, within the question's bounds) or `freeText`
/// (at most 500 characters). What does not fit is refused, never cut, so
/// the answer can be fixed and sent again.
fn wire_answer(id: &str, question: &WireQuestion, given: &[String]) -> Result<Value, String> {
    let mut answer = json!({"questionId": id});
    let labels = given.iter().all(|label| question.labels.contains(label));
    if question.multi && labels {
        let count = given.len() as u64;
        if question.min.is_some_and(|min| count < min)
            || question.max.is_some_and(|max| count > max)
        {
            return Err(format!(
                "Muse wants between {} and {} choices for “{}”",
                question.min.unwrap_or(0),
                question
                    .max
                    .map_or("any number of".to_owned(), |max| max.to_string()),
                question.name
            ));
        }
        answer["selectedLabels"] = json!(given);
    } else if !question.multi && labels && given.len() == 1 {
        answer["selectedLabel"] = json!(given[0]);
    } else {
        let text = given.join(", ");
        if text.trim().is_empty() {
            return Err(format!("Answer “{}” before sending", question.name));
        }
        if text.chars().count() > FREE_TEXT_CHARS {
            return Err(format!(
                "Muse takes at most {FREE_TEXT_CHARS} characters as the answer to “{}”; shorten it",
                question.name
            ));
        }
        answer["freeText"] = json!(text);
    }
    Ok(answer)
}

#[derive(Default)]
struct Streamed {
    text: String,
    started: bool,
    reasoning: bool,
    summary_part: Option<i64>,
}

/// How a submission shows in the transcript. Muse's user-message echo
/// carries the model's input (the carried-over conversation, file contents);
/// the transcript shows what the user sent, with the snapshots attached.
#[derive(Clone)]
struct Shown {
    text: String,
    presentation: Presentation,
}

/// One message for Muse: its input parts and how it shows.
struct Submission {
    input: Vec<Value>,
    shown: Shown,
}

impl Submission {
    fn text(text: String) -> Self {
        Self {
            input: vec![json!({"type": "text", "text": text})],
            shown: Shown {
                text,
                presentation: Presentation::default(),
            },
        }
    }

    fn attachments(text: &str, attachments: &[Attachment]) -> Result<Self, String> {
        let input = attachments::muse_inputs(text, attachments)?;
        let images = attachments
            .iter()
            .filter(|a| matches!(a.kind, AttachmentKind::Image { .. }))
            .map(|a| ChatImage {
                label: a.name.clone(),
                source: ImageSource::Local {
                    path: a.path.display().to_string(),
                },
            })
            .collect();
        Ok(Self {
            input,
            shown: Shown {
                text: attachments::summary(text, attachments),
                presentation: Presentation {
                    images,
                    attachments: attachments.to_vec(),
                    ..Default::default()
                },
            },
        })
    }

    /// The input's text parts as Muse joins them.
    fn joined(&self) -> String {
        self.input
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect()
    }
}

#[derive(Clone)]
struct Settings {
    model: Option<String>,
    effort: Option<String>,
    mode: ApprovalMode,
}

struct PendingSetting {
    revision: u64,
    value: String,
    command_id: String,
    reporter: DriverReporter,
}

struct Session {
    events: Sender<ChatEvent>,
    next_request: i64,
    waiting: HashMap<i64, Reply>,
    session_id: Option<String>,
    settings: Settings,
    reporter: DriverReporter,
    setting_revisions: [u64; 3],
    pending_settings: [Option<PendingSetting>; 3],
    plan_profile: bool,
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
    /// The conversation from before a provider switch, until a turn takes it.
    context: Option<String>,
    /// That conversation on its way, with the serial of the start carrying it.
    context_in_flight: Option<(u64, String)>,
    /// How recent submissions show, by `commandId`.
    shown: VecDeque<(String, Shown)>,
    /// Typed messages riding a request Muse has not answered, by request id:
    /// if the process ends first, they are given back in a notice, never resent.
    in_flight: HashMap<i64, String>,
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
                effort: None,
                mode: config.approval_mode,
            },
            reporter: config.reporter.clone(),
            setting_revisions: [0; 3],
            pending_settings: std::array::from_fn(|_| None),
            plan_profile: config.approval_mode == ApprovalMode::Plan,
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
            context: None,
            context_in_flight: None,
            shown: VecDeque::new(),
            in_flight: HashMap::new(),
        }
    }

    fn mapped_mode(&self, wire: &Value) -> Option<ApprovalMode> {
        if self.plan_profile {
            return Some(ApprovalMode::Plan);
        }
        match wire.as_str()? {
            "promptUnmatched" => Some(ApprovalMode::Supervised),
            "onRequest" => Some(ApprovalMode::AutoEdit),
            "allowAll" => Some(ApprovalMode::Full),
            _ => None,
        }
    }

    fn observe_metadata(&mut self, metadata: &Value) {
        let model = metadata
            .get("modelId")
            .map(|value| value.as_str().map(str::to_owned));
        if let Some(model) = &model {
            self.settings.model = model.clone();
        }
        let mode = self.mapped_mode(&metadata["approvalMode"]["mode"]);
        if let Some(mode) = mode {
            self.settings.mode = mode;
        }
        self.reporter.report(DriverReport {
            provider_thread_id: self.session_id.clone(),
            model,
            approval_mode: mode,
            fast: Some(false),
            ..Default::default()
        });
    }

    /// MSP emits a change notification before its accepted reply. Route our
    /// own change through its request reporter; unrelated durable changes
    /// supersede pending requests and use the run's observation reporter.
    fn report_setting_change(
        &mut self,
        field: usize,
        value: &str,
        params: &Value,
        report: DriverReport,
        changed: bool,
    ) {
        let own = self.pending_settings[field].as_ref().filter(|pending| {
            pending.revision == self.setting_revisions[field]
                && pending.value == value
                && if field == 2 {
                    params["commandId"].as_str() == Some(pending.command_id.as_str())
                } else {
                    params["source"].as_str() == Some("user")
                }
        });
        if let Some(pending) = own {
            pending.reporter.report(report);
        } else {
            if changed {
                self.setting_revisions[field] += 1;
                self.pending_settings[field] = None;
            }
            self.reporter.report(report);
        }
    }

    fn setting_refusal(&mut self, name: &str, outcome: &Result<Value, RpcError>) {
        let reason = match outcome {
            Err(error) => error.message.clone(),
            Ok(value) => format!("unconfirmed response {}", value["status"]),
        };
        self.notice(
            NoticeLevel::Warning,
            notice_kind::SETTING_REFUSED,
            format!("Muse did not change the {name}: {reason}"),
        );
    }

    fn remember(&mut self, command_id: &str, shown: Shown) {
        if self.shown.len() >= SHOWN_KEEP {
            self.shown.pop_front();
        }
        self.shown.push_back((command_id.to_owned(), shown));
    }

    /// `text`, typed by the user, rides `frame` until Muse answers it.
    fn track(&mut self, frame: &Value, text: String) {
        if let Some(id) = frame["id"].as_i64() {
            self.in_flight.insert(id, text);
        }
    }

    /// Typed messages given back to the user, whole, when they did not or
    /// may not have reached Muse.
    fn undelivered(&mut self, lead: &str, texts: &[String]) {
        if texts.is_empty() {
            return;
        }
        let quoted: Vec<String> = texts.iter().map(|text| format!("“{text}”")).collect();
        self.notice(
            NoticeLevel::Warning,
            notice_kind::UNDELIVERED,
            format!("{lead}\n\n{}", quoted.join("\n\n")),
        );
    }

    /// The process is over: say which typed messages it never took (waiting
    /// for a turn) and which it never confirmed (their request unanswered).
    fn give_back(&mut self) {
        let waiting = std::mem::take(&mut self.queued);
        self.undelivered("Muse stopped before these messages were sent:", &waiting);
        let mut unconfirmed: Vec<(i64, String)> = self.in_flight.drain().collect();
        unconfirmed.sort();
        let unconfirmed: Vec<String> = unconfirmed.into_iter().map(|(_, text)| text).collect();
        self.undelivered(
            "Muse stopped before confirming these messages; check the transcript before sending them again:",
            &unconfirmed,
        );
    }

    fn forget(&mut self, command_id: &str) {
        self.shown.retain(|(id, _)| id != command_id);
    }

    /// `frame` never reached Muse: forget its reply and its on-screen form;
    /// an interrupt is not under way, and a start is not starting (messages
    /// that waited for it start a turn of their own, the frames returned).
    /// The typed message it carried, if any, is returned to be given back.
    fn unwrite(&mut self, frame: &Value) -> (Vec<Value>, Option<String>) {
        let text = frame["id"].as_i64().and_then(|id| {
            self.waiting.remove(&id);
            self.in_flight.remove(&id)
        });
        if let Some(command) = frame["params"]["commandId"].as_str() {
            self.forget(command);
        }
        let frames = match frame["method"].as_str() {
            Some("turn/interrupt") => {
                self.interrupting = false;
                self.refresh_state();
                Vec::new()
            }
            Some("turn/start") if self.turn.is_none() => {
                self.unsend_context();
                self.settle_start()
            }
            _ => Vec::new(),
        };
        (frames, text)
    }

    /// A start is over without a turn of its own (refused, withdrawn,
    /// absorbed, unwritten, or its turn already over): nothing is starting,
    /// an interrupt meant for it is moot, and messages that waited for it
    /// start a turn of their own (the frames returned).
    fn settle_start(&mut self) -> Vec<Value> {
        self.starting = false;
        self.interrupt_wanted = false;
        if self.turn.is_some() {
            // The open turn takes them (`begin_turn` steered them already).
            self.refresh_state();
            return Vec::new();
        }
        let queued = std::mem::take(&mut self.queued);
        match (queued.is_empty(), self.session_id.clone()) {
            (false, Some(session_id)) => {
                vec![self.start_frame(&session_id, Submission::text(queued.join("\n\n")), None)]
            }
            _ => {
                self.refresh_state();
                Vec::new()
            }
        }
    }

    /// What a sender that stopped waiting for `what` is told when Muse's
    /// answer comes after all.
    fn late_outcome(&mut self, what: &str, message: &str) {
        let text = if message.starts_with(UNKNOWN_SUBMISSION) {
            format!(
                "Muse answered the {what} sent earlier without naming a turn that took it; check the transcript before sending it again."
            )
        } else {
            format!("Muse did not take the {what}: {message}")
        };
        self.notice(NoticeLevel::Error, notice_kind::SETTING_REFUSED, text);
    }

    /// The current start did not reach Muse: its carried-over conversation
    /// goes with the next one.
    fn unsend_context(&mut self) {
        if let Some((serial, context)) = self.context_in_flight.take()
            && serial == self.start_serial
            && self.context.is_none()
        {
            self.context = Some(context);
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
                return self.settle_start();
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
                // Our queued start runs: what it carried is in now.
                self.pending_turn = None;
                self.context_in_flight = None;
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
        let typed = text.clone();
        let params = json!({
            "commandId": command_id(),
            "sessionId": session,
            "expectedTurnId": turn,
            "input": [{"type": "text", "text": text}],
        });
        let turn = turn.to_owned();
        let frame = self.request(
            "turn/steer",
            params,
            Box::new(move |muse, outcome| {
                match outcome {
                    // In: Muse names the turn that absorbed it.
                    Ok(result) if result["turnId"].as_str() == Some(turn.as_str()) => {}
                    // Acknowledged but not for this turn: Muse 1.4.7 answers
                    // so a steer that came after the turn's last model call,
                    // and drops it. Whether it is in is not certain, so it is
                    // not sent again; the user has it back to decide.
                    Ok(_) => muse.with(|session| {
                        session.undelivered(
                            "Muse acknowledged this message without taking it into the running turn, so it may not have reached the model; it was not sent again:",
                            &[text],
                        )
                    }),
                    // Refused, so certainly not in: it starts the next turn
                    // if this one is over, else it waits for that.
                    Err(_) => {
                        let ended =
                            muse.with(|session| session.turn.as_deref() != Some(turn.as_str()));
                        if !ended {
                            muse.with(|session| session.queued.push(text));
                        } else if let Err(error) = muse.send_text(text.clone()) {
                            muse.with(|session| {
                                session.undelivered(
                                    &format!("A message for Muse was not sent ({error}):"),
                                    &[text],
                                )
                            });
                        }
                    }
                }
            }),
        );
        self.track(&frame, typed);
        frame
    }

    /// A `turn/start` for `submission`, marking the chat busy. The
    /// carried-over conversation, while Muse has not had it, leads the input;
    /// `answer`, when given, hears how Muse took the start (an attachment
    /// send waits for it), else a refusal is a notice.
    fn start_frame(
        &mut self,
        session: &str,
        submission: Submission,
        answer: Option<Sender<Result<(), String>>>,
    ) -> Value {
        self.starting = true;
        self.start_serial += 1;
        self.completed_before_receipt.clear();
        self.completion_overflow = false;
        let serial = self.start_serial;
        self.refresh_state();
        let command = command_id();
        let context = self.context.take();
        self.context_in_flight = context.clone().map(|context| (serial, context));
        let raw = submission.joined();
        let Submission { mut input, shown } = submission;
        // What the user typed, for a notice if Muse never takes it (an
        // attachment sender hears the outcome itself).
        let typed = answer.is_none().then(|| shown.text.clone());
        if let Some(context) = &context {
            input.insert(0, json!({"type": "text", "text": format!("{context}\n\n")}));
        }
        let mut params = json!({
            "commandId": command,
            "sessionId": session,
            "input": input,
        });
        if context.is_some() || shown.text != raw || !shown.presentation.is_empty() {
            params["displayText"] = json!(shown.text);
            self.remember(&command, shown);
        }
        if let Some(effort) = self.effort_to_send() {
            params["reasoningEffort"] = json!(effort);
        }
        let given_back = typed.clone();
        let frame = self.request(
            "turn/start",
            params,
            Box::new(move |muse, outcome| {
                let (frames, result) =
                    muse.with(|session| session.started(serial, &command, outcome, given_back));
                muse.send_all(frames);
                // A sender that stopped waiting was told the outcome is
                // unknown; a refusal that comes after is still said.
                if let Some(answer) = answer
                    && let Err(mpsc::SendError(Err(message))) = answer.send(result)
                {
                    muse.with(|session| session.late_outcome("attachments", &message));
                }
            }),
        );
        if let Some(typed) = typed {
            self.track(&frame, typed);
        }
        frame
    }

    /// How Muse took the `turn/start` numbered `serial`: frames to send, and
    /// the outcome for a sender that waits. A refusal gives the carried-over
    /// conversation back for the next start; a receipt that arrives after
    /// another start, or names no turn, leaves it unknown whether the
    /// message is in.
    fn started(
        &mut self,
        serial: u64,
        command: &str,
        outcome: Result<Value, RpcError>,
        typed: Option<String>,
    ) -> (Vec<Value>, Result<(), String>) {
        let result = match outcome {
            Ok(result) => {
                // Taken with the input, unless only queued: then it is in
                // once the turn starts (`begin_turn`), and back if withdrawn.
                if result["disposition"].as_str() != Some("queued")
                    && self
                        .context_in_flight
                        .as_ref()
                        .is_some_and(|(sent, _)| *sent == serial)
                {
                    self.context_in_flight = None;
                }
                result
            }
            Err(error) => {
                if self.start_serial != serial {
                    // Refused all the same: the typed message is given back.
                    if let Some(typed) = typed {
                        self.undelivered(
                            &format!("Muse did not start the turn ({}); not sent:", error.message),
                            &[typed],
                        );
                    }
                    return (
                        Vec::new(),
                        Err(format!(
                            "{UNKNOWN_SUBMISSION} Muse refused a start ({}) after another one began; inspect the transcript before resending",
                            error.message
                        )),
                    );
                }
                self.unsend_context();
                self.forget(command);
                if let Some(typed) = typed {
                    self.undelivered(
                        &format!("Muse did not start the turn ({}); not sent:", error.message),
                        &[typed],
                    );
                }
                // A message accepted behind this start still deserves a
                // turn of its own, not the bin.
                return (self.settle_start(), Err(error.message));
            }
        };
        if self.start_serial != serial {
            return (
                Vec::new(),
                Err(format!(
                    "{UNKNOWN_SUBMISSION} Muse's start receipt arrived after another start; inspect the transcript before resending"
                )),
            );
        }
        let Some(turn_id) = result["turnId"].as_str().filter(|id| !id.trim().is_empty()) else {
            if let Some(typed) = typed {
                self.undelivered(
                    "Muse took this message without naming a turn, so it may not run; check the transcript before sending it again:",
                    &[typed],
                );
            }
            return (
                self.settle_start(),
                Err(format!(
                    "{UNKNOWN_SUBMISSION} Muse took the message without naming a turn; inspect the transcript before resending"
                )),
            );
        };
        // `disposition` says what happened to the input: a queued turn is
        // adopted when its `turn/started` arrives, a steered one was folded
        // into the turn already open, anything else starts now.
        let frames = match result["disposition"].as_str() {
            Some("queued") => {
                self.pending_turn = Some(turn_id.to_owned());
                Vec::new()
            }
            // Folded into a turn this driver does not hold open.
            Some("steered") => self.settle_start(),
            _ => self.begin_turn(turn_id),
        };
        (frames, Ok(()))
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
            return vec![self.start_frame(&session, Submission::text(queued.join("\n\n")), None)];
        }
        self.refresh_state();
        Vec::new()
    }

    /// The chat has stopped on purpose.
    fn finish_stopped(&mut self) {
        self.give_back();
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
        self.give_back();
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
            ChatCommand::SendAttachments { text, attachments } => {
                self.send_attachments(&text, &attachments)
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
                contributor_mode: _,
                contributor_opt_out: _,
                claude_billing: _,
            } => self.configure(
                model,
                effort,
                approval_mode,
                fast,
                self.with(|session| session.reporter.clone()),
            ),
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
                Ok(Some(session.start_frame(
                    &session_id,
                    Submission::text(text),
                    None,
                )))
            }
        })?;
        match frame {
            // The sender hears it, and keeps the message.
            Some(frame) => self.write(&frame).map_err(|unwritten| {
                if unwritten.partial {
                    format!(
                        "{UNKNOWN_SUBMISSION} {}; inspect the transcript before resending",
                        unwritten.error
                    )
                } else {
                    unwritten.error
                }
            }),
            None => Ok(()),
        }
    }

    /// Attachments go only when Muse can take them now: into the open turn
    /// (exactly that turn) or as a new one. The answer is Muse's, so a
    /// refusal keeps the draft; nothing is queued or retried, and a reply
    /// that cannot be matched leaves the submission unknown.
    fn send_attachments(&self, text: &str, attachments: &[Attachment]) -> Result<(), String> {
        let deadline = Instant::now() + child::WRITE_WAIT;
        if text.len() > attachments::TEXT_BYTES {
            return Err("attachment message text exceeds 1 MiB".into());
        }
        let submission = Submission::attachments(text, attachments)?;
        let (answer, wait) = mpsc::channel();
        let frame = self.with(|session| -> Result<_, String> {
            let Some(session_id) = session.session_id.clone() else {
                return Err("muse has no session yet; keep the draft".to_owned());
            };
            if session.interrupting || session.starting {
                return Err("Muse is starting or interrupting a turn; keep the draft and send after it settles".to_owned());
            }
            match session.turn.clone() {
                Some(turn) => {
                    let command = command_id();
                    let Submission { input, shown } = submission;
                    session.remember(&command, shown);
                    let forget = command.clone();
                    let frame = session.request(
                        "turn/steer",
                        json!({
                            "commandId": command,
                            "sessionId": session_id,
                            "expectedTurnId": turn,
                            "input": input,
                        }),
                        Box::new(move |muse, outcome| {
                            let result = match outcome {
                                Ok(value) if value["turnId"].as_str() == Some(turn.as_str()) => Ok(()),
                                // Not absorbed by this turn (Muse 1.4.7 answers so when
                                // the turn has just ended, and drops the input), yet
                                // acknowledged: whether it is in is for the transcript.
                                Ok(_) => Err(format!(
                                    "{UNKNOWN_SUBMISSION} Muse acknowledged the attachments without naming the turn that took them; inspect the transcript before resending"
                                )),
                                Err(error) => {
                                    muse.with(|session| session.forget(&forget));
                                    Err(error.message)
                                }
                            };
                            // A sender that stopped waiting still hears of it.
                            if let Err(mpsc::SendError(Err(message))) = answer.send(result) {
                                muse.with(|session| session.late_outcome("attachments", &message));
                            }
                        }),
                    );
                    Ok(frame)
                }
                None => {
                    Ok(session.start_frame(&session_id, submission, Some(answer)))
                }
            }
        })?;
        // Even a partial write may have submitted it: never write it again.
        if let Err(error) = self.proc.send_line_until(&frame.to_string(), deadline) {
            if error.written == 0 {
                // Nothing reached Muse: undo what the send set up, so a late
                // unrelated reply cannot read as this message's.
                let (more, _) = self.with(|session| session.unwrite(&frame));
                self.send_all(more);
                return Err(error.to_string());
            }
            return Err(format!(
                "{UNKNOWN_SUBMISSION} {error}; inspect the transcript before resending"
            ));
        }
        loop {
            if let Ok(result) = wait.try_recv() {
                return result;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() || self.proc.writes_cancelled() {
                return Err(format!(
                    "{UNKNOWN_SUBMISSION} Muse's answer to the submission timed out or was cancelled; inspect the transcript before resending"
                ));
            }
            match wait.recv_timeout(remaining.min(Duration::from_millis(20))) {
                Ok(result) => return result,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(error) => {
                    return Err(format!(
                        "{UNKNOWN_SUBMISSION} Muse's answer to the submission was lost ({error}); inspect the transcript before resending"
                    ));
                }
            }
        }
    }

    fn approve(&self, request_id: &str, decision: Decision) -> Result<(), String> {
        enum Plan {
            Decide {
                session_id: String,
                choice_id: String,
                requirement: Value,
                stop_turn: bool,
            },
            RefuseNotOneOff {
                for_session: bool,
            },
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
            // A narrower grant may stand in for a wider one, and a stop for
            // a refusal; never the other way round.
            let wanted: &[Decision] = match decision {
                Decision::Accept => &[Decision::Accept],
                Decision::AcceptForSession => &[Decision::AcceptForSession, Decision::Accept],
                Decision::Decline => &[Decision::Decline, Decision::Cancel],
                Decision::Cancel => &[Decision::Cancel, Decision::Decline],
            };
            let picked = wanted.iter().find_map(|want| {
                pick(choices, *want).map(|choice| (*want, choice.choice_id.clone()))
            });
            match picked {
                Some((carried, choice_id)) => {
                    *deciding = true;
                    Ok(Plan::Decide {
                        session_id,
                        choice_id,
                        requirement: requirement.clone(),
                        stop_turn: decision == Decision::Cancel && carried != Decision::Cancel,
                    })
                }
                None if decision == Decision::Accept => Ok(Plan::RefuseNotOneOff {
                    for_session: pick(choices, Decision::AcceptForSession).is_some(),
                }),
                None => Err(format!("muse did not offer {decision:?} for {request_id}")),
            }
        })?;
        match plan {
            Plan::AlreadyDeciding => Ok(()),
            Plan::RefuseNotOneOff { for_session } => {
                self.with(|session| {
                    session.notice(
                        NoticeLevel::Warning,
                        notice_kind::SETTING_REFUSED,
                        if for_session {
                            "Muse offers no one-time approval here; choose “for this session” to grant it, or decline.".to_owned()
                        } else {
                            "Muse offers only an approval that saves a lasting rule here, which RiWork does not give; decline it, or decide in Muse itself.".to_owned()
                        },
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
                    wire_answer(id, question, given)
                })
                .collect::<Result<Vec<_>, _>>()?;
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

    /// Keep standing values until Muse accepts the change. Each callback owns
    /// its field revision and request-specific host reporter; a later selection
    /// or provider observation makes older replies harmless.
    fn configure(
        &self,
        model: Option<String>,
        effort: Option<String>,
        approval_mode: Option<ApprovalMode>,
        fast: Option<bool>,
        reporter: DriverReporter,
    ) -> Result<(), String> {
        if fast == Some(true) {
            return Err("Muse has no fast mode".to_owned());
        }
        let frames = self.with(|session| -> Result<Vec<Value>, String> {
            if session.finished { return Err("the Muse process is not running".to_owned()); }
            if let Some(mode) = approval_mode
                && (mode == ApprovalMode::Plan) != session.plan_profile
            {
                return Err(plan_switch_refusal(mode));
            }
            if let Some(effort) = &effort {
                let selected = model.clone().or_else(|| session.effective_model());
                let supported = EFFORTS.contains(&effort.as_str()) && selected.as_ref().is_none_or(|id| {
                    session.models.iter().find(|row| &row.id == id).is_none_or(|row| {
                        row.efforts.is_empty() || row.efforts.contains(effort)
                    })
                });
                if !supported { return Err(format!("The reasoning effort {effort} is not one Muse takes here.")); }
            }
            let session_id = session.session_id.clone().ok_or("muse has no session yet")?;
            let mut frames = Vec::new();
            if let Some(model) = model {
                session.setting_revisions[0] += 1;
                let revision = session.setting_revisions[0];
                let command = command_id();
                session.pending_settings[0] = Some(PendingSetting { revision, value: model.clone(), command_id: command.clone(), reporter: reporter.clone() });
                let mut selection = json!({"modelId": model});
                if let Some(row) = session.catalog.iter().find(|row| row["modelId"].as_str() == Some(model.as_str())) {
                    for field in ["providerId", "profileId", "displayLabel"] {
                        if !row[field].is_null() { selection[field] = row[field].clone(); }
                    }
                }
                let report = reporter.clone();
                frames.push(session.request("session/setModel",
                    json!({"commandId": command, "sessionId": session_id, "model": selection}),
                    Box::new(move |muse, outcome| muse.with(|session| {
                        if session.finished || session.setting_revisions[0] != revision { return; }
                        session.pending_settings[0] = None;
                        if admitted(&outcome) { session.settings.model = Some(model); }
                        else { session.setting_refusal("model", &outcome); }
                        report.report(DriverReport { model: Some(session.settings.model.clone()), ..Default::default() });
                    })),
                ));
            }
            if let Some(effort) = effort {
                session.setting_revisions[1] += 1;
                let revision = session.setting_revisions[1];
                let command = command_id();
                session.pending_settings[1] = Some(PendingSetting { revision, value: effort.clone(), command_id: command.clone(), reporter: reporter.clone() });
                let report = reporter.clone();
                frames.push(session.request("session/setReasoningEffort",
                    json!({"commandId": command, "sessionId": session_id, "reasoningEffort": effort}),
                    Box::new(move |muse, outcome| muse.with(|session| {
                        if session.finished || session.setting_revisions[1] != revision { return; }
                        session.pending_settings[1] = None;
                        if admitted(&outcome) { session.settings.effort = Some(effort); }
                        else { session.setting_refusal("reasoning effort", &outcome); }
                        report.report(DriverReport { effort: Some(session.settings.effort.clone()), ..Default::default() });
                    })),
                ));
            }
            if let Some(mode) = approval_mode {
                session.setting_revisions[2] += 1;
                let revision = session.setting_revisions[2];
                let command = command_id();
                session.pending_settings[2] = Some(PendingSetting { revision, value: policy(mode).to_owned(), command_id: command.clone(), reporter: reporter.clone() });
                let report = reporter.clone();
                frames.push(session.request("session/setApprovalMode",
                    json!({"commandId": command, "sessionId": session_id, "mode": policy(mode)}),
                    Box::new(move |muse, outcome| muse.with(|session| {
                        if session.finished || session.setting_revisions[2] != revision { return; }
                        session.pending_settings[2] = None;
                        if admitted(&outcome) {
                            let effective = outcome.as_ref().ok().and_then(|value| session.mapped_mode(&value["effectiveMode"]["mode"]));
                            session.settings.mode = effective.unwrap_or(mode);
                        } else { session.setting_refusal("approval mode", &outcome); }
                        report.report(DriverReport { approval_mode: Some(session.settings.mode), ..Default::default() });
                    })),
                ));
            }
            Ok(frames)
        })?;
        if fast == Some(false) {
            reporter.report(DriverReport {
                fast: Some(false),
                ..Default::default()
            });
        }
        // Settle writes that never left through the same request callback. A
        // write failure cannot leave an optimistic setting or pending callback.
        let mut frames = frames.into_iter();
        while let Some(frame) = frames.next() {
            if let Err(error) = self.proc.send(&frame) {
                let message = error.to_string();
                for failed in std::iter::once(frame).chain(frames) {
                    let reply = self.with(|session| {
                        failed["id"]
                            .as_i64()
                            .and_then(|id| session.waiting.remove(&id))
                    });
                    if let Some(reply) = reply {
                        reply(
                            self,
                            Err(RpcError {
                                message: message.clone(),
                                kind: None,
                                data: Value::Null,
                            }),
                        );
                    }
                }
                break;
            }
        }
        Ok(())
    }
}

fn admitted(outcome: &Result<Value, RpcError>) -> bool {
    matches!(outcome, Ok(value) if value["status"].as_str() == Some("accepted"))
}

/// Why a Muse conversation does not move into or out of Plan.
pub(crate) fn plan_switch_refusal(to: ApprovalMode) -> String {
    if to == ApprovalMode::Plan {
        "Muse fixes read-only access when a conversation starts, so this one cannot become Plan; start a new Muse chat in Plan".to_owned()
    } else {
        "This Muse conversation was started read-only for Plan and stays read-only; start a new Muse chat for another mode".to_owned()
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
            Box::new(|muse, outcome| match outcome {
                Ok(result) if result["status"].as_str() == Some("noop") => muse.with(|session| {
                    session.notice(
                        NoticeLevel::Info,
                        notice_kind::PROVIDER_WARNING,
                        match result["reason"].as_str() {
                            Some(reason) => format!("Muse had nothing to compact ({reason})."),
                            None => "Muse had nothing to compact.".to_owned(),
                        },
                    );
                }),
                Ok(_) => {}
                Err(error) => muse.with(|session| {
                    session.notice(
                        NoticeLevel::Error,
                        notice_kind::SETTING_REFUSED,
                        format!("Muse could not compact the context: {}", error.message),
                    );
                }),
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
        let choices = wire_choices(params);
        let offered = offered_of(&choices);
        if offered.is_empty() {
            // A prompt with no answerable choice would wedge the chat in
            // Waiting; say so and leave the turn interruptible instead.
            self.notice(
                NoticeLevel::Warning,
                notice_kind::PROVIDER_WARNING,
                "Muse asked for an approval RiWork cannot answer without saving a rule; interrupt the turn, or decide in Muse itself.".to_owned(),
            );
            return;
        }
        let (kind, title, detail) = describe(params);
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
                request: params.clone(),
                presented: approval.clone(),
                deciding: false,
            },
            blocking: true,
        });
        self.emit(ChatEvent::ApprovalRequested { approval });
        self.refresh_state();
    }

    /// A stage refresh for an open approval: the new requirement, choices
    /// and subject, described with the request's own raw arguments, under
    /// the same request and item.
    fn update_approval(&mut self, params: &Value) {
        let Some(approval_id) = params["approvalId"].as_str().filter(|id| !id.is_empty()) else {
            return;
        };
        let fresh = wire_choices(params);
        let offered = offered_of(&fresh);
        // A stage RiWork cannot answer: the old buttons would aim old choices
        // at the new requirement, so the prompt closes here and says why;
        // Muse's turn waits for an interrupt or a decision in Muse itself.
        if params["availableChoices"].is_array()
            && offered.is_empty()
            && self
                .requests
                .iter()
                .any(|open| open.request_id == approval_id)
        {
            self.notice(
                NoticeLevel::Warning,
                notice_kind::PROVIDER_WARNING,
                "Muse's approval moved to a step RiWork cannot answer without saving a rule; interrupt the turn, or decide in Muse itself.".to_owned(),
            );
            self.close_approval(approval_id, Decision::Cancel);
            return;
        }
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
            request,
            presented,
            ..
        } = &mut open.kind
        else {
            return;
        };
        if !params["currentRequirementId"].is_null() {
            *requirement = params["currentRequirementId"].clone();
        }
        for field in ["subject", "availableChoices", "currentRequirementId"] {
            if !params[field].is_null() {
                request[field] = params[field].clone();
            }
        }
        if !offered.is_empty() {
            presented.choices = offered;
            *choices = fresh;
        }
        let (kind, title, detail) = describe(request);
        presented.kind = kind;
        presented.title = title;
        presented.detail = detail;
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
            wire.push(WireQuestion {
                name: question["header"]
                    .as_str()
                    .filter(|header| !header.is_empty())
                    .or_else(|| question["question"].as_str())
                    .unwrap_or(id)
                    .to_owned(),
                multi,
                labels,
                min: question["selection"]["minSelections"].as_u64(),
                max: question["selection"]["maxSelections"].as_u64(),
            });
        }
        if ids.is_empty() {
            return Vec::new();
        }
        let again = self
            .requests
            .iter()
            .any(|open| open.request_id == user_input_id);
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
        // A timed prompt settles by itself unless the user engages with it;
        // showing it is not engaging (`userInput/engaged` is for that), so
        // the countdown stays and the user is told of it.
        if let Some(ms) = params["autoResolutionMs"].as_u64().filter(|_| !again) {
            self.notice(
                NoticeLevel::Info,
                notice_kind::PROVIDER_WARNING,
                format!(
                    "Muse settles this question by itself in {} seconds unless it is answered.",
                    ms.div_ceil(1000)
                ),
            );
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
                match (
                    params["nextAttempt"].as_u64(),
                    params["maxAttempts"].as_u64(),
                ) {
                    (Some(next), Some(max)) => format!(
                        "Muse is retrying after a failed request (attempt {next} of {max}): {}",
                        params["reason"].as_str().unwrap_or("the request failed")
                    ),
                    _ => "Muse is retrying after a failed request.".to_owned(),
                },
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
                // `cost {usd, partial}`; absent is unpriced, not the last price.
                usage.cost_usd = cumulative["cost"]["usd"]
                    .as_f64()
                    .or_else(|| cumulative["cost"].as_f64());
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
                // Absent when the host's basis has no limit: no window then.
                usage.context_window = params["windowTokens"].as_u64();
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
            "session/modelChanged" => {
                if let Some(model) = params["modelId"].as_str().filter(|id| !id.is_empty()) {
                    let changed = self.settings.model.as_deref() != Some(model);
                    self.settings.model = Some(model.to_owned());
                    self.report_setting_change(
                        0,
                        model,
                        params,
                        DriverReport {
                            model: Some(self.settings.model.clone()),
                            ..Default::default()
                        },
                        changed,
                    );
                }
            }
            "session/reasoningEffortChanged" => {
                if let Some(effort) = params["reasoningEffort"]
                    .as_str()
                    .filter(|tier| EFFORTS.contains(tier))
                {
                    let changed = self.settings.effort.as_deref() != Some(effort);
                    self.settings.effort = Some(effort.to_owned());
                    self.report_setting_change(
                        1,
                        effort,
                        params,
                        DriverReport {
                            effort: Some(self.settings.effort.clone()),
                            ..Default::default()
                        },
                        changed,
                    );
                }
            }
            "session/approvalModeChanged" => {
                if let Some(mode) = self.mapped_mode(&params["mode"]) {
                    let changed = self.settings.mode != mode;
                    self.settings.mode = mode;
                    self.report_setting_change(
                        2,
                        params["mode"].as_str().unwrap_or_default(),
                        params,
                        DriverReport {
                            approval_mode: Some(mode),
                            ..Default::default()
                        },
                        changed,
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
            // Disclosure only: the chosen model stays chosen but cannot be
            // served by the provider now signed in.
            "session/modelRouteUnserved" => self.notice(
                NoticeLevel::Warning,
                notice_kind::MODEL_FALLBACK,
                format!(
                    "Muse's provider {} cannot serve the model {}; choose another model.",
                    params["installedProviderId"]
                        .as_str()
                        .unwrap_or("now signed in"),
                    params["modelId"].as_str().unwrap_or("this chat uses"),
                ),
            ),
            // A queued start reclaimed elsewhere never runs.
            "turn/unqueued" => {
                if let Some(turn) = params["turnId"].as_str()
                    && self.pending_turn.as_deref() == Some(turn)
                {
                    self.pending_turn = None;
                    self.notice(
                        NoticeLevel::Warning,
                        notice_kind::UNDELIVERED,
                        "A message waiting for its turn was withdrawn from Muse and will not run."
                            .to_owned(),
                    );
                    // What it carried never ran: the conversation from before a
                    // switch goes with the next start, and so do the messages
                    // that waited behind it.
                    self.unsend_context();
                    return self.settle_start();
                }
            }
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
        let Some(list) = params["items"].as_array() else {
            return;
        };
        let items: Vec<Step> = list
            .iter()
            .filter_map(|todo| {
                let text = todo["text"].as_str()?.to_owned();
                // A cancelled step is over without being done; it stays on
                // the list, marked, rather than vanishing.
                let (status, text) = match todo["status"].as_str() {
                    Some("pending") => (StepStatus::Pending, text),
                    Some("inProgress") => (StepStatus::InProgress, text),
                    Some("completed") => (StepStatus::Completed, text),
                    Some("cancelled") => (StepStatus::Completed, format!("{text} (cancelled)")),
                    _ => return None,
                };
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
        // A compaction's outcome is known only at its end: no marker before
        // then, and none for one that did not compact.
        if item["kind"].as_str() == Some("compaction") {
            if !done {
                return;
            }
            match item["outcome"].as_str() {
                None | Some("compacted") => {}
                Some("noop") => {
                    self.notice(
                        NoticeLevel::Info,
                        notice_kind::PROVIDER_WARNING,
                        "Muse had nothing to compact.".to_owned(),
                    );
                    return;
                }
                Some(outcome) => {
                    self.notice(
                        NoticeLevel::Warning,
                        notice_kind::PROVIDER_WARNING,
                        format!(
                            "Muse could not compact the context ({}).",
                            item["reason"].as_str().unwrap_or(outcome)
                        ),
                    );
                    return;
                }
            }
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
        // Our own submission, at every revision: what the user sent, never
        // the model input Muse echoes.
        let mut presentation = Presentation::default();
        if matches!(body, ItemBody::UserMessage { .. })
            && let Some(shown) = item["commandId"].as_str().and_then(|command| {
                self.shown
                    .iter()
                    .rev()
                    .find(|(id, _)| id == command)
                    .map(|(_, shown)| shown.clone())
            })
        {
            body = ItemBody::UserMessage { text: shown.text };
            presentation = shown.presentation;
        }
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
                    presentation,
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
                presentation,
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

/// What an approval asks, from its `ApprovalRequestParams`: the kind of
/// action, a one-line title and the detail (raw arguments, stage argv, the
/// rule each choice would add), bounded for the screen.
fn describe(params: &Value) -> (ApprovalKind, String, String) {
    let subject = &params["subject"];
    let kind = match subject["kind"].as_str().unwrap_or_default() {
        "shell" | "process" => ApprovalKind::Command,
        "fileAccess" if subject["access"].as_str() == Some("read") => ApprovalKind::Permissions,
        "fileAccess" => ApprovalKind::FileChange,
        "network" | "unixSocket" => ApprovalKind::Permissions,
        _ => ApprovalKind::Tool,
    };
    let title = subject["command"]
        .as_str()
        .or_else(|| subject["path"].as_str())
        .or_else(|| subject["target"].as_str())
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
    if let Some(access) = subject["access"]
        .as_str()
        .filter(|access| !access.is_empty())
    {
        detail.push_str(&format!("Access: {access}. "));
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
    // The rule a choice adds, for the choices RiWork can make (never a saved one).
    for choice in params["availableChoices"].as_array().into_iter().flatten() {
        if choice["scope"].as_str() == Some("localPersistent") {
            continue;
        }
        if let Some(preview) = choice["rulePreview"].as_str().filter(|p| !p.is_empty()) {
            if !detail.is_empty() {
                detail.push('\n');
            }
            detail.push_str(preview);
        }
    }
    truncate_bytes(&mut detail, DETAIL_BYTES);
    (kind, title, detail)
}

/// How much Muse may do without asking, in its own closed vocabulary. Plan
/// also starts its session under the `:read-only` profile (`start_session`);
/// `denyUnmatched` alone would not keep it from writing.
fn policy(mode: ApprovalMode) -> &'static str {
    match mode {
        ApprovalMode::Supervised => "promptUnmatched",
        ApprovalMode::AutoEdit => "onRequest",
        ApprovalMode::Full => "allowAll",
        ApprovalMode::Plan => "denyUnmatched",
    }
}

/// One offered MSP choice: what it decides, for how long, and its id.
#[derive(Clone, Debug, PartialEq)]
struct Choice {
    decision: String,
    scope: String,
    choice_id: String,
}

/// The offered MSP choices, in wire order.
fn wire_choices(params: &Value) -> Vec<Choice> {
    params["availableChoices"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|choice| {
                    Some(Choice {
                        decision: choice["decision"].as_str()?.to_owned(),
                        scope: choice["scope"].as_str().unwrap_or_default().to_owned(),
                        choice_id: choice["choiceId"].as_str()?.to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The MSP (decision, scope) pairs that carry out a RiWork decision, best
/// first. A `localPersistent` choice saves a rule beyond this session and a
/// policy-amendment denial saves a refusal; RiWork has no decision that asks
/// for either, so neither is ever sent.
fn carries(decision: Decision) -> &'static [(&'static str, &'static str)] {
    match decision {
        Decision::Accept => &[("approved", "once")],
        Decision::AcceptForSession => &[
            ("approvedForSession", "session"),
            ("approved", "session"),
            ("approvedPolicyAmendment", "session"),
            ("approvedForSession", "once"),
        ],
        Decision::Decline => &[("denied", "once")],
        Decision::Cancel => &[("abort", "once"), ("abort", "session"), ("abort", "")],
    }
}

/// The offered choice that carries out `decision`, if any.
fn pick(choices: &[Choice], decision: Decision) -> Option<&Choice> {
    carries(decision).iter().find_map(|(wire, scope)| {
        choices
            .iter()
            .find(|choice| choice.decision == *wire && choice.scope == *scope)
    })
}

/// The RiWork decisions the offered choices can carry out, in RiWork's order.
fn offered_of(choices: &[Choice]) -> Vec<Decision> {
    [
        Decision::Accept,
        Decision::AcceptForSession,
        Decision::Decline,
        Decision::Cancel,
    ]
    .into_iter()
    .filter(|decision| pick(choices, *decision).is_some())
    .collect()
}

/// A wire decision as the transcript names it (for folds that close an
/// approval we did not decide ourselves). A saved rule never reads as a
/// one-off.
fn wire_decision(decision: Option<&str>) -> Decision {
    match decision {
        Some("approved") => Decision::Accept,
        Some("approvedForSession" | "approvedPolicyAmendment") => Decision::AcceptForSession,
        Some("abort") => Decision::Cancel,
        _ => Decision::Decline,
    }
}

fn model_options(result: &Value) -> (Vec<ModelOption>, Vec<Value>) {
    let rows: Vec<Value> = result["models"].as_array().cloned().unwrap_or_default();
    let any_active = rows
        .iter()
        .any(|row| row["isActive"].as_bool() == Some(true));
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
                // Asked with the session, `isActive` marks the model it runs
                // with no choice made, which is what "default" means here.
                is_default: if any_active {
                    row["isActive"].as_bool().unwrap_or(false)
                } else {
                    row["isDefault"].as_bool().unwrap_or(false)
                },
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
        // Anything but `inProgress` is over; an unknown word is over too.
        Some(_) => ItemStatus::Completed,
        None if done => ItemStatus::Completed,
        None => ItemStatus::InProgress,
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

#[cfg(test)]
#[path = "muse_contract_tests.rs"]
mod contract_tests;
