//! ACP v1 over newline-delimited JSON-RPC on a child process's stdio.
//! References: https://agentclientprotocol.com/protocol/v1/{initialization,session-setup,prompt-turn,tool-calls}
//! The agent owns tools and sandboxing. We advertise no filesystem or terminal service.
//! Resume loads the native session and suppresses its replay because the host already
//! retains our transcript. We never replace a missing historical session with a new one.
use super::{
    child::{self, Frame, FrameReader, Proc},
    driver::{Driver, DriverConfig, DriverReport, DriverReporter},
    model::*,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    process::ChildStdout,
    sync::{
        Arc, Mutex,
        mpsc::{self, Sender},
    },
    thread::JoinHandle,
    time::Duration,
};

const CALL_TIMEOUT: Duration = Duration::from_secs(30);
const CANCEL_TIMEOUT: Duration = Duration::from_secs(10);
const TEXT_LIMIT: usize = 4 * 1024 * 1024;
const ITEM_LIMIT: usize = 4096;
const TURN_BYTES_LIMIT: usize = 16 * 1024 * 1024;
type Reply = Result<Value, String>;
enum Pending {
    Call(Sender<Reply>),
    Prompt(String),
}
struct Permission {
    rpc_id: Value,
    allow: Option<String>,
    deny: Option<String>,
}
struct State {
    events: Sender<ChatEvent>,
    next: u64,
    pending: HashMap<u64, Pending>,
    session: Option<String>,
    active: Option<String>,
    loading: bool,
    stopping: bool,
    failed: bool,
    cancelling: bool,
    mode: ApprovalMode,
    permissions: HashMap<String, Permission>,
    items: HashMap<String, Item>,
    item_bytes: HashMap<String, usize>,
    retained_bytes: usize,
    stream: Option<(String, String)>,
    models: Vec<ModelOption>,
    instructions: Option<String>,
}
struct Acp {
    proc: Arc<Proc>,
    state: Mutex<State>,
}
struct AcpDriver {
    acp: Arc<Acp>,
    reader: Option<JoinHandle<()>>,
    stopped: bool,
}

fn item(id: String, turn: Option<String>, body: ItemBody) -> Item {
    Item {
        id,
        turn_id: turn,
        body,
        status: ItemStatus::InProgress,
        started_at: None,
        ended_at: None,
        presentation: Default::default(),
    }
}
fn short(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}
fn valid_id(id: &str, max: usize) -> bool {
    !id.is_empty() && id.len() <= max && !id.chars().any(char::is_control)
}
fn rpc_error(frame: &Value) -> String {
    let error = &frame["error"];
    let message = short(
        error["message"].as_str().unwrap_or("agent request failed"),
        2048,
    );
    if error["code"].as_i64() == Some(-32000) || message.to_lowercase().contains("auth") {
        format!("ACP agent: {message}. Sign in with the configured agent's own CLI, then retry.")
    } else {
        format!("ACP agent: {message}")
    }
}

pub fn start(config: DriverConfig, events: Sender<ChatEvent>) -> Result<Box<dyn Driver>, String> {
    let _ = events.send(ChatEvent::State {
        state: ChatState::Starting,
    });
    let validation = if config.fast {
        Err("ACP v1 does not define fast mode")
    } else if config.effort.is_some() {
        Err("ACP v1 does not define reasoning effort")
    } else if config.approval_mode == ApprovalMode::Plan {
        Err(
            "ACP has no portable enforced Plan mode; configure the agent's own sandbox and use supervised mode",
        )
    } else if config.fork.is_some() {
        Err("This ACP adapter does not support native session forks")
    } else {
        Ok(())
    };
    if let Err(message) = validation {
        let _ = events.send(ChatEvent::State {
            state: ChatState::Failed {
                message: message.into(),
            },
        });
        return Err(message.into());
    }
    let (proc, stdout) = Proc::spawn(child::command(&config, &[])).inspect_err(|message| {
        let _ = events.send(ChatEvent::State {
            state: ChatState::Failed {
                message: message.clone(),
            },
        });
    })?;
    let acp = Arc::new(Acp {
        proc,
        state: Mutex::new(State {
            events,
            next: 0,
            pending: HashMap::new(),
            session: config.resume.clone(),
            active: None,
            loading: true,
            stopping: false,
            failed: false,
            cancelling: false,
            mode: config.approval_mode,
            permissions: HashMap::new(),
            items: HashMap::new(),
            item_bytes: HashMap::new(),
            retained_bytes: 0,
            stream: None,
            models: vec![],
            instructions: config
                .instructions
                .clone()
                .filter(|_| config.resume.is_none()),
        }),
    });
    let reader_acp = acp.clone();
    let mut driver = AcpDriver {
        acp,
        reader: Some(std::thread::spawn(move || reader_acp.read(stdout))),
        stopped: false,
    };
    if let Err(message) = driver.acp.handshake(&config) {
        driver.acp.with(|s| s.fail(message.clone()));
        driver.shutdown();
        return Err(message);
    }
    Ok(Box::new(driver))
}

impl State {
    fn emit(&self, event: ChatEvent) {
        let _ = self.events.send(event);
    }
    fn state(&self, state: ChatState) {
        self.emit(ChatEvent::State { state });
    }
    fn next(&mut self) -> u64 {
        self.next += 1;
        self.next
    }
    fn finish(&mut self, outcome: TurnOutcome) {
        let Some(turn_id) = self.active.take() else {
            return;
        };
        self.cancelling = false;
        for (_, mut value) in self.items.drain().collect::<Vec<_>>() {
            if value.status == ItemStatus::InProgress {
                value.status = match outcome {
                    TurnOutcome::Completed => ItemStatus::Completed,
                    TurnOutcome::Interrupted => ItemStatus::Interrupted,
                    _ => ItemStatus::Failed,
                };
                self.emit(ChatEvent::ItemCompleted { item: value });
            }
        }
        for request_id in self
            .permissions
            .drain()
            .map(|(id, _)| id)
            .collect::<Vec<_>>()
        {
            self.emit(ChatEvent::ApprovalResolved {
                request_id,
                decision: Decision::Cancel,
            });
        }
        self.stream = None;
        self.item_bytes.clear();
        self.retained_bytes = 0;
        self.emit(ChatEvent::TurnCompleted {
            turn_id,
            outcome,
            ended_at: None,
        });
        if !self.failed && !self.stopping {
            self.state(ChatState::Idle);
        }
    }
    fn fail(&mut self, message: String) {
        if self.failed {
            return;
        }
        self.failed = true;
        self.finish(TurnOutcome::Failed {
            message: message.clone(),
        });
        for (_, request) in self.pending.drain() {
            if let Pending::Call(reply) = request {
                let _ = reply.send(Err(message.clone()));
            }
        }
        self.state(ChatState::Failed { message });
    }
    fn update(&mut self, value: &Value) {
        if self.loading || self.failed || self.active.is_none() {
            return;
        }
        let kind = value["sessionUpdate"].as_str().unwrap_or("");
        match kind {
            "agent_message_chunk" | "agent_thought_chunk" => {
                let Some(text) = value["content"]["text"].as_str() else {
                    return;
                };
                if value["messageId"]
                    .as_str()
                    .is_some_and(|id| !valid_id(id, 512))
                {
                    self.fail("ACP agent returned an invalid message ID".into());
                    return;
                }
                let key = format!("{kind}:{}", value["messageId"].as_str().unwrap_or(kind));
                if self.retained_bytes.saturating_add(text.len()) > TURN_BYTES_LIMIT {
                    self.fail("ACP turn exceeded retained output limit".into());
                    return;
                }
                let id = match &self.stream {
                    Some((previous, id)) if previous == &key => id.clone(),
                    _ => {
                        if self.items.len() >= ITEM_LIMIT {
                            self.fail("ACP turn exceeded item limit".into());
                            return;
                        }
                        let ordinal = self.next();
                        let id = format!("acp:{}:{ordinal}", self.active.as_deref().unwrap());
                        let body = if kind == "agent_thought_chunk" {
                            ItemBody::Reasoning {
                                text: String::new(),
                            }
                        } else {
                            ItemBody::AgentMessage {
                                text: String::new(),
                            }
                        };
                        let value = item(id.clone(), self.active.clone(), body);
                        self.emit(ChatEvent::ItemStarted {
                            item: value.clone(),
                        });
                        self.items.insert(id.clone(), value);
                        self.stream = Some((key, id.clone()));
                        id
                    }
                };
                let retained = self
                    .items
                    .get_mut(&id)
                    .and_then(|i| match &mut i.body {
                        ItemBody::Reasoning { text } | ItemBody::AgentMessage { text } => {
                            Some(text)
                        }
                        _ => None,
                    })
                    .unwrap();
                if retained.len() + text.len() > TEXT_LIMIT {
                    self.fail("ACP message exceeded text limit".into());
                    return;
                }
                retained.push_str(text);
                self.retained_bytes += text.len();
                *self.item_bytes.entry(id.clone()).or_default() += text.len();
                self.emit(ChatEvent::ItemDelta {
                    item_id: id,
                    delta: Delta::Text(text.into()),
                });
            }
            "tool_call" | "tool_call_update" => {
                self.stream = None;
                let Some(native_id) = value["toolCallId"].as_str() else {
                    return;
                };
                if !valid_id(native_id, 512) {
                    self.fail("ACP agent returned an invalid tool call ID".into());
                    return;
                }
                let id = format!("acp:{}:tool:{native_id}", self.active.as_deref().unwrap());
                if !self.items.contains_key(&id) && self.items.len() >= ITEM_LIMIT {
                    self.fail("ACP turn exceeded item limit".into());
                    return;
                }
                let existing = self.items.get(&id).cloned();
                let mut output = existing.clone().unwrap_or_else(|| {
                    item(
                        id.clone(),
                        self.active.clone(),
                        ItemBody::ToolCall {
                            server: None,
                            tool: "ACP tool".into(),
                            input: Value::Null,
                            output: None,
                        },
                    )
                });
                if let ItemBody::ToolCall {
                    tool,
                    input,
                    output: body,
                    ..
                } = &mut output.body
                {
                    if let Some(title) = value["title"].as_str() {
                        *tool = short(title, 2048);
                    }
                    if !value["rawInput"].is_null() {
                        *input = bounded_json(&value["rawInput"]);
                    }
                    if !value["rawOutput"].is_null() {
                        *body = Some(short(&display_json(&value["rawOutput"]), TEXT_LIMIT));
                    }
                    if let Some(content) = value["content"].as_array() {
                        *body = Some(short(
                            &content
                                .iter()
                                .filter_map(tool_content)
                                .collect::<Vec<_>>()
                                .join("\n"),
                            TEXT_LIMIT,
                        ));
                    }
                }
                output.status = match value["status"].as_str() {
                    Some("completed") => ItemStatus::Completed,
                    Some("failed") => ItemStatus::Failed,
                    Some("pending" | "in_progress") => ItemStatus::InProgress,
                    _ => output.status,
                };
                let size = serde_json::to_vec(&output).map_or(TURN_BYTES_LIMIT + 1, |v| v.len());
                let total = self
                    .retained_bytes
                    .saturating_sub(*self.item_bytes.get(&id).unwrap_or(&0))
                    .saturating_add(size);
                if total > TURN_BYTES_LIMIT {
                    self.fail("ACP turn exceeded retained output limit".into());
                    return;
                }
                self.item_bytes.insert(id.clone(), size);
                self.retained_bytes = total;
                self.emit(if existing.is_none() {
                    ChatEvent::ItemStarted {
                        item: output.clone(),
                    }
                } else {
                    ChatEvent::ItemCompleted {
                        item: output.clone(),
                    }
                });
                self.items.insert(id, output);
            }
            "plan" => {
                let steps = value["entries"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .take(256)
                    .map(|entry| Step {
                        text: short(entry["content"].as_str().unwrap_or(""), 8192),
                        status: match entry["status"].as_str() {
                            Some("completed") => StepStatus::Completed,
                            Some("in_progress") => StepStatus::InProgress,
                            _ => StepStatus::Pending,
                        },
                    })
                    .collect();
                let id = format!("acp:{}:plan", self.active.as_deref().unwrap());
                let mut value = item(
                    id,
                    self.active.clone(),
                    ItemBody::Plan {
                        explanation: None,
                        steps,
                    },
                );
                value.status = ItemStatus::Completed;
                self.emit(ChatEvent::ItemCompleted { item: value });
            }
            _ => {}
        }
    }
}

fn display_json(v: &Value) -> String {
    v.as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| v.to_string())
}
fn bounded_json(v: &Value) -> Value {
    let text = v.to_string();
    if text.len() > 65536 {
        Value::String(short(&text, 65536))
    } else {
        v.clone()
    }
}
fn tool_content(v: &Value) -> Option<String> {
    match v["type"].as_str()? {
        "content" => v["content"]["text"].as_str().map(str::to_owned),
        "diff" => Some(format!(
            "{}\n--- before\n{}\n+++ after\n{}",
            v["path"].as_str().unwrap_or(""),
            v["oldText"].as_str().unwrap_or(""),
            v["newText"].as_str().unwrap_or("")
        )),
        _ => None,
    }
}

impl Acp {
    fn with<T>(&self, f: impl FnOnce(&mut State) -> T) -> T {
        f(&mut self.state.lock().unwrap_or_else(|p| p.into_inner()))
    }
    fn call(&self, method: &str, params: Value) -> Reply {
        let (tx, rx) = mpsc::channel();
        let id = self.with(|s| {
            let id = s.next();
            s.pending.insert(id, Pending::Call(tx));
            id
        });
        if let Err(e) = self
            .proc
            .send(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
        {
            self.with(|s| {
                s.pending.remove(&id);
            });
            return Err(e);
        }
        rx.recv_timeout(CALL_TIMEOUT).unwrap_or_else(|_| {
            self.with(|s| {
                s.pending.remove(&id);
            });
            Err(format!("ACP {method} timed out or the agent exited"))
        })
    }
    fn handshake(&self, config: &DriverConfig) -> Result<(), String> {
        let init = self.call("initialize", json!({"protocolVersion":1,"clientInfo":{"name":"somtus","version":env!("CARGO_PKG_VERSION")},"clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false},"terminal":false}}))?;
        if init["protocolVersion"].as_u64() != Some(1) {
            return Err("ACP agent did not negotiate supported protocol version 1".into());
        }
        if config.resume.is_some() && init["agentCapabilities"]["loadSession"] != true {
            return Err("ACP agent cannot load saved sessions; this conversation was preserved. Start a new chat explicitly.".into());
        }
        let mut params = json!({"cwd":config.cwd,"mcpServers":[]});
        if let Some(id) = &config.resume {
            params["sessionId"] = json!(id);
        }
        let session = self.call(
            if config.resume.is_some() {
                "session/load"
            } else {
                "session/new"
            },
            params,
        )?;
        let id = config
            .resume
            .clone()
            .or_else(|| {
                session["sessionId"]
                    .as_str()
                    .filter(|s| valid_id(s, 1024))
                    .map(str::to_owned)
            })
            .ok_or("ACP agent returned no session ID")?;
        let models: Vec<ModelOption> = session["models"]["availableModels"]
            .as_array()
            .into_iter()
            .flatten()
            .take(256)
            .filter_map(|v| {
                let id = v["modelId"].as_str()?;
                if !valid_id(id, 100) {
                    return None;
                }
                Some(ModelOption {
                    id: id.into(),
                    name: short(v["name"].as_str().unwrap_or(id), 512),
                    ..Default::default()
                })
            })
            .collect();
        self.with(|s| {
            s.session = Some(id.clone());
            s.models = models.clone();
        });
        let mut selected = session["models"]["currentModelId"]
            .as_str()
            .filter(|id| valid_id(id, 100))
            .map(str::to_owned);
        if let Some(model) = &config.model {
            if !models.iter().any(|m| &m.id == model) {
                return Err("ACP agent has not advertised the requested model; choose its default or an advertised model".into());
            }
            self.call("session/set_model", json!({"sessionId":id,"modelId":model}))?;
            selected = Some(model.clone());
        }
        config.reporter.report(DriverReport {
            provider_thread_id: Some(id),
            model: Some(selected),
            approval_mode: Some(config.approval_mode),
            ..Default::default()
        });
        self.with(|s| {
            s.loading = false;
            s.emit(ChatEvent::Models { models });
            s.state(ChatState::Idle);
        });
        Ok(())
    }
    fn read(&self, stdout: ChildStdout) {
        let mut frames = FrameReader::new(stdout, child::MAX_FRAME_BYTES);
        loop {
            let result = match frames.next_frame() {
                Ok(Some(Frame::Line(line))) => serde_json::from_slice::<Value>(&line)
                    .map_err(|e| format!("Invalid ACP JSON frame: {e}")),
                Ok(Some(Frame::Oversized(_))) => Err("ACP frame exceeded the size limit".into()),
                Ok(None) => break,
                Err(e) => {
                    self.with(|s| s.fail(format!("ACP output failed: {e}")));
                    break;
                }
            };
            match result {
                Ok(frame) => self.frame(frame),
                Err(e) => self.with(|s| s.fail(e)),
            }
            if self.with(|s| s.failed && !s.stopping) {
                self.proc.cancel_writes();
                self.proc.stop();
                break;
            }
        }
        self.with(|s| { if !s.stopping { s.fail("ACP agent disconnected; send again to load its saved session. The interrupted prompt is not replayed.".into()); } });
    }
    fn frame(&self, frame: Value) {
        if frame["jsonrpc"] != "2.0" {
            self.with(|s| s.fail("ACP frame has no JSON-RPC 2.0 version".into()));
            return;
        }
        if let Some(id) = frame.get("id") {
            if !(id.is_i64() || id.is_u64() || id.as_str().is_some_and(|id| valid_id(id, 256))) {
                self.with(|s| s.fail("ACP agent returned an invalid JSON-RPC identifier".into()));
                return;
            }
        }
        if let Some(method) = frame["method"].as_str() {
            let params = &frame["params"];
            if method == "session/update" && frame.get("id").is_none() {
                self.with(|s| {
                    if s.session.as_deref() != params["sessionId"].as_str() {
                        return;
                    }
                    if params["update"]["sessionUpdate"] == "session_info_update" && !s.loading {
                        if let Some(title) = params["update"]["title"].as_str() {
                            s.emit(ChatEvent::ProviderTitle {
                                title: short(title, 512),
                            });
                        }
                    } else {
                        s.update(&params["update"]);
                    }
                });
            } else if method == "session/request_permission" && frame.get("id").is_some() {
                self.permission(frame["id"].clone(), params);
            } else if let Some(id) = frame.get("id") {
                let _ = self.proc.send(&json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Somtus does not implement this ACP client capability"}}));
            }
            return;
        }
        let Some(id) = frame["id"].as_u64() else {
            return;
        };
        let request = self.with(|s| s.pending.remove(&id));
        let result = if frame.get("error").is_some() {
            Err(rpc_error(&frame))
        } else if let Some(value) = frame.get("result") {
            Ok(value.clone())
        } else {
            Err("ACP response contains neither result nor error".into())
        };
        match request {
            Some(Pending::Call(reply)) => {
                let _ = reply.send(result);
            }
            Some(Pending::Prompt(turn)) => self.with(|s| {
                if s.active.as_ref() != Some(&turn) {
                    return;
                }
                let outcome = match result {
                    Ok(v) => match v["stopReason"].as_str() {
                        Some("end_turn" | "max_tokens" | "max_turn_requests" | "refusal") => {
                            TurnOutcome::Completed
                        }
                        Some("cancelled") => TurnOutcome::Interrupted,
                        _ => TurnOutcome::Failed {
                            message: "ACP prompt returned an unknown stop reason".into(),
                        },
                    },
                    Err(message) => TurnOutcome::Failed { message },
                };
                s.finish(outcome);
            }),
            None => {}
        }
    }
    fn permission(&self, rpc_id: Value, params: &Value) {
        let response = self.with(|s| {
            if s.loading
                || s.failed
                || s.active.is_none()
                || s.session.as_deref() != params["sessionId"].as_str()
            {
                return Some(json!({"outcome":"cancelled"}));
            }
            let options = params["options"].as_array();
            let pick = |kind: &str| {
                options
                    .into_iter()
                    .flatten()
                    .find(|o| o["kind"] == kind)
                    .and_then(|o| o["optionId"].as_str())
                    .filter(|id| valid_id(id, 512))
                    .map(str::to_owned)
            };
            let allow = pick("allow_once");
            let deny = pick("reject_once");
            let kind = params["toolCall"]["kind"].as_str().unwrap_or("other");
            if (s.mode == ApprovalMode::Full || s.mode == ApprovalMode::AutoEdit && kind == "edit")
                && allow.is_some()
            {
                return Some(json!({"outcome":"selected","optionId":allow}));
            }
            if s.permissions.len() >= 64 {
                return Some(json!({"outcome":"cancelled"}));
            }
            let request_id = format!("acp-permission:{}", rpc_id);
            if s.permissions.contains_key(&request_id) {
                return None;
            }
            let mut choices = Vec::new();
            if allow.is_some() {
                choices.push(Decision::Accept);
            }
            if deny.is_some() {
                choices.push(Decision::Decline);
            }
            choices.push(Decision::Cancel);
            s.emit(ChatEvent::ApprovalRequested {
                approval: Approval {
                    request_id: request_id.clone(),
                    item_id: params["toolCall"]["toolCallId"]
                        .as_str()
                        .map(|id| format!("acp:{}:tool:{id}", s.active.as_deref().unwrap())),
                    kind: match kind {
                        "execute" => ApprovalKind::Command,
                        "edit" | "delete" | "move" => ApprovalKind::FileChange,
                        _ => ApprovalKind::Tool,
                    },
                    title: short(
                        params["toolCall"]["title"]
                            .as_str()
                            .unwrap_or("ACP tool permission"),
                        2048,
                    ),
                    detail: short(&params["toolCall"].to_string(), 20_000),
                    choices,
                },
            });
            s.permissions.insert(
                request_id,
                Permission {
                    rpc_id: rpc_id.clone(),
                    allow,
                    deny,
                },
            );
            s.state(ChatState::Waiting);
            None
        });
        if let Some(outcome) = response {
            let _ = self
                .proc
                .send(&json!({"jsonrpc":"2.0","id":rpc_id,"result":{"outcome":outcome}}));
        }
    }
    fn send(&self, text: String) -> Result<(), String> {
        let (id, params) = self.with(|s| {
            if s.failed || s.stopping {
                return Err("ACP agent is not available".to_owned());
            }
            if s.active.is_some() {
                return Err("ACP v1 cannot steer an active prompt; wait or cancel first".to_owned());
            }
            let session = s.session.clone().ok_or("ACP session is not ready")?;
            let turn = uuid::Uuid::now_v7().to_string();
            let id = s.next();
            let prompt = match s.instructions.take() {
                Some(context) => format!("{context}\n\n{text}"),
                None => text.clone(),
            };
            s.active = Some(turn.clone());
            s.pending.insert(id, Pending::Prompt(turn.clone()));
            s.emit(ChatEvent::TurnStarted {
                turn_id: turn.clone(),
                started_at: None,
            });
            let mut user = item(
                format!("acp:{turn}:user"),
                Some(turn),
                ItemBody::UserMessage { text },
            );
            user.status = ItemStatus::Completed;
            s.emit(ChatEvent::ItemCompleted { item: user });
            s.state(ChatState::Running);
            Ok((
                id,
                json!({"sessionId":session,"prompt":[{"type":"text","text":prompt}]}),
            ))
        })?;
        self.proc
            .send(&json!({"jsonrpc":"2.0","id":id,"method":"session/prompt","params":params}))
            .inspect_err(|message| {
                self.with(|s| {
                    s.pending.remove(&id);
                    s.fail(message.clone());
                });
            })
    }
    fn approve(&self, request_id: String, decision: Decision) -> Result<(), String> {
        self.with(|s| {
            let p = s.permissions.get(&request_id).ok_or("ACP permission is no longer pending")?;
            let outcome = match decision {
                Decision::Accept => json!({"outcome":"selected","optionId":p.allow.as_ref().ok_or("ACP agent offered no one-time allow option")?}),
                Decision::Decline => json!({"outcome":"selected","optionId":p.deny.as_ref().ok_or("ACP agent offered no one-time reject option")?}),
                Decision::Cancel => json!({"outcome":"cancelled"}),
                Decision::AcceptForSession => return Err("ACP permission options do not guarantee session-scoped grants; choose Allow once".to_owned()),
            };
            let response = json!({"jsonrpc":"2.0","id":p.rpc_id,"result":{"outcome":outcome}});
            // Keep acknowledgement ordered before an immediate prompt completion.
            // Proc's bounded writer is independently cancellable by the host.
            self.proc.send(&response)?;
            s.permissions.remove(&request_id);
            s.emit(ChatEvent::ApprovalResolved {
                request_id,
                decision,
            });
            if s.active.is_some() && s.permissions.is_empty() {
                s.state(ChatState::Running);
            }
            Ok(())
        })
    }
    fn interrupt(self: &Arc<Self>) -> Result<(), String> {
        let (session, turn, responses) = self.with(|s| {
            let turn = s.active.clone().ok_or("ACP has no active prompt")?;
            if s.cancelling { return Err("ACP cancellation is already pending".into()); }
            s.cancelling = true;
            let mut responses = vec![];
            for (request_id, permission) in s.permissions.drain().collect::<Vec<_>>() {
                responses.push(json!({"jsonrpc":"2.0","id":permission.rpc_id,"result":{"outcome":{"outcome":"cancelled"}}}));
                s.emit(ChatEvent::ApprovalResolved { request_id, decision: Decision::Cancel });
            }
            Ok::<_, String>((s.session.clone(), turn, responses))
        })?;
        for response in responses {
            self.proc.send(&response)?;
        }
        self.proc.send(
            &json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":session}}),
        )?;
        // No successful stop is claimed until the agent answers the original prompt.
        let weak = Arc::downgrade(self);
        std::thread::spawn(move || {
            std::thread::sleep(CANCEL_TIMEOUT);
            if let Some(acp) = weak.upgrade() {
                let stop = acp.with(|s| {
                    if s.active.as_ref() != Some(&turn) || s.stopping { return false; }
                    s.fail("ACP agent did not confirm cancellation; its process was stopped. Resume requires loading the saved session.".into()); true
                });
                if stop {
                    acp.proc.cancel_writes();
                    acp.proc.stop();
                }
            }
        });
        Ok(())
    }
}

impl Driver for AcpDriver {
    fn cancel_io(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        let proc = self.acp.proc.clone();
        Some(Arc::new(move || proc.cancel_writes()))
    }
    fn provider_thread_id(&self) -> Option<String> {
        self.acp.with(|s| s.session.clone())
    }
    fn command(&mut self, command: ChatCommand) -> Result<(), String> {
        match command {
            ChatCommand::Send { text } => self.acp.send(text),
            ChatCommand::SendAttachments { text, attachments } if attachments.is_empty() => {
                self.acp.send(text)
            }
            ChatCommand::SendAttachments { .. } => Err(
                "ACP attachments are not implemented; send text or use the agent's own file tools"
                    .into(),
            ),
            ChatCommand::Interrupt => self.acp.interrupt(),
            ChatCommand::Approve {
                request_id,
                decision,
            } => {
                self.acp.approve(request_id, decision)?;
                if decision == Decision::Cancel {
                    self.acp.interrupt()?;
                }
                Ok(())
            }
            ChatCommand::Configure { .. } => self.configure(command, DriverReporter::default()),
            ChatCommand::Stop => {
                self.shutdown();
                Ok(())
            }
            _ => Err("This command is not supported by the ACP v1 adapter".into()),
        }
    }
    fn configure(&mut self, command: ChatCommand, reporter: DriverReporter) -> Result<(), String> {
        let ChatCommand::Configure {
            model,
            effort,
            approval_mode,
            fast,
            contributor_mode,
            contributor_opt_out,
            claude_billing,
        } = command
        else {
            return self.command(command);
        };
        if effort.is_some()
            || fast == Some(true)
            || contributor_mode.is_some()
            || contributor_opt_out.is_some()
            || claude_billing.is_some()
        {
            return Err("ACP does not support effort, fast mode, contributor mode or Claude billing settings".into());
        }
        if approval_mode == Some(ApprovalMode::Plan) {
            return Err("ACP has no portable enforced Plan mode".into());
        }
        let session = self.acp.with(|s| {
            if s.active.is_some() || s.failed || s.stopping {
                return Err("Configure ACP only while its session is idle".to_owned());
            }
            if let Some(model) = &model {
                if !s.models.iter().any(|m| &m.id == model) {
                    return Err("ACP agent has not advertised this model".into());
                }
            }
            Ok(s.session.clone())
        })?;
        if let Some(model) = &model {
            self.acp.call(
                "session/set_model",
                json!({"sessionId":session,"modelId":model}),
            )?;
        }
        if let Some(mode) = approval_mode {
            self.acp.with(|s| s.mode = mode);
        }
        reporter.report(DriverReport {
            model: model.map(Some),
            approval_mode,
            fast,
            ..Default::default()
        });
        Ok(())
    }
    fn shutdown(&mut self) {
        if std::mem::replace(&mut self.stopped, true) {
            return;
        }
        self.acp.with(|s| {
            s.stopping = true;
            s.finish(TurnOutcome::Interrupted);
        });
        self.acp.proc.stop();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        self.acp.with(|s| {
            if !s.failed {
                s.state(ChatState::Stopped);
            }
        });
    }
}
impl Drop for AcpDriver {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
#[path = "acp_tests.rs"]
mod tests;
