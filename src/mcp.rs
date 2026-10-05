//! A local stdio MCP bridge for the same workspace data used by GPUI and the CLI.
//!
//! This server deliberately writes only JSON-RPC to stdout. A host can launch
//! `riwork mcp` and discover the available operations through `tools/list`.

use std::{
    io::{self, BufRead, BufWriter, Write},
    path::Path,
};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    orchestrators::{self, Orchestrator},
    schedule_service::{
        CreateRequest, RepeatChange, ScheduleError, ScheduleKey, ScheduleService, ScopeInput,
        UpdateRequest,
    },
    sessions::{SessionManager, ShellKind},
    store::{State, Store, TaskStatus},
};

const PROTOCOL_VERSION: &str = "2025-11-25";

/// Serve one MCP connection until its stdin closes. Errors from individual
/// requests are returned over JSON-RPC; only transport failures escape here.
pub fn run() -> Result<(), String> {
    let mut stdin = io::stdin().lock();
    let mut stdout = BufWriter::new(io::stdout().lock());
    let mut line = Vec::new();
    loop {
        line.clear();
        // Read bytes, not `lines()`: a request that is not UTF-8 must get a
        // parse error instead of ending the whole connection.
        let read = stdin
            .read_until(b'\n', &mut line)
            .map_err(|error| format!("read MCP request: {error}"))?;
        if read == 0 {
            return Ok(());
        }
        if let Some(response) = handle_line(&line) {
            serde_json::to_writer(&mut stdout, &response)
                .map_err(|error| format!("encode MCP response: {error}"))?;
            stdout
                .write_all(b"\n")
                .and_then(|_| stdout.flush())
                .map_err(|error| format!("write MCP response: {error}"))?;
        }
    }
}

fn handle_line(line: &[u8]) -> Option<Value> {
    let Ok(text) = std::str::from_utf8(line) else {
        return Some(rpc_error(
            Value::Null,
            -32700,
            "Parse error: request is not valid UTF-8",
        ));
    };
    match serde_json::from_str::<Value>(text) {
        Ok(message) => handle_message(&message),
        Err(error) => Some(rpc_error(
            Value::Null,
            -32700,
            &format!("Parse error: {error}"),
        )),
    }
}

/// JSON-RPC batches (part of MCP 2025-03-26) answer with an array of the
/// responses that exist; a batch of only notifications answers nothing.
fn handle_message(message: &Value) -> Option<Value> {
    match message {
        Value::Array(items) if items.is_empty() => Some(rpc_error(
            Value::Null,
            -32600,
            "Invalid JSON-RPC request: empty batch",
        )),
        Value::Array(items) => {
            let responses: Vec<Value> = items.iter().filter_map(respond).collect();
            (!responses.is_empty()).then_some(Value::Array(responses))
        }
        _ => respond(message),
    }
}

fn respond(request: &Value) -> Option<Value> {
    let Some(fields) = request.as_object() else {
        return Some(rpc_error(
            Value::Null,
            -32600,
            "Invalid JSON-RPC request: expected an object",
        ));
    };
    let Some(id) = fields.get("id").cloned() else {
        // Notifications, including notifications/initialized and cancelled,
        // have no response. Anything else without an id is malformed.
        return if fields.get("method").is_some_and(Value::is_string) {
            None
        } else {
            Some(rpc_error(
                Value::Null,
                -32600,
                "Invalid JSON-RPC request: method is required",
            ))
        };
    };
    if request.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Some(rpc_error(id, -32600, "Invalid JSON-RPC request"));
    }
    let Some(method) = request.get("method").and_then(Value::as_str) else {
        return Some(rpc_error(id, -32600, "JSON-RPC method is required"));
    };
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
    let result = match method {
        "initialize" => Ok(initialize(&params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => call_tool(&params),
        _ => return Some(rpc_error(id, -32601, "Method not found")),
    };
    Some(match result {
        Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
        Err(message) => rpc_error(id, -32602, &message),
    })
}

fn initialize(params: &Value) -> Value {
    let requested = params.get("protocolVersion").and_then(Value::as_str);
    let version = match requested {
        Some("2025-11-25" | "2025-06-18" | "2025-03-26" | "2024-11-05") => requested.unwrap(),
        _ => PROTOCOL_VERSION,
    };
    json!({
        "protocolVersion":version,
        "capabilities":{"tools":{"listChanged":false}},
        "serverInfo":{"name":"riwork","title":"RiWork Workspaces","version":env!("CARGO_PKG_VERSION")},
        "instructions":"Use these tools to manage RiWork projects, Git worktrees, tasks, and desktop schedules. Schedule mutations require full UUIDs, explicit scope, and a current revision; dispatch occurs only while the desktop app is open."
    })
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

fn tool_result(value: Value) -> Value {
    let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
    // The 2025 protocol requires structuredContent to be an object.
    json!({"content":[{"type":"text","text":text}],"structuredContent":value,"isError":false})
}

fn tool_error(message: &str) -> Value {
    json!({"content":[{"type":"text","text":message}],"isError":true})
}

/// Failed results carry no `structuredContent`: clients validate it against the
/// declared `outputSchema` (`schedule`/`items`/`deleted`), so an error object
/// there would make them throw instead of showing the failure. The text is the
/// same `{"error":{code,message,current?}}` object the CLI prints with --json.
fn schedule_tool_error(error: ScheduleError) -> Value {
    let text = serde_json::to_string_pretty(&json!({ "error": error }))
        .unwrap_or_else(|_| error.message.clone());
    json!({"content":[{"type":"text","text":text}],"isError":true})
}

/// Every advertised `inputSchema` forbids additional properties, so enforce
/// that for all tools: a misspelled selector must fail rather than silently
/// fall back to the active project.
fn unaccepted_arguments(definition: &Value, args: &Value) -> Option<String> {
    let accepted = definition["inputSchema"]["properties"].as_object()?;
    let unknown: Vec<String> = args
        .as_object()?
        .keys()
        .filter(|key| !accepted.contains_key(*key))
        .map(|key| format!("'{key}'"))
        .collect();
    if unknown.is_empty() {
        return None;
    }
    let accepted: Vec<&str> = accepted.keys().map(String::as_str).collect();
    Some(format!(
        "{} {} not accepted by {} (accepted: {})",
        unknown.join(", "),
        if unknown.len() == 1 { "is" } else { "are" },
        definition["name"].as_str().unwrap_or("this tool"),
        if accepted.is_empty() {
            "none".to_owned()
        } else {
            accepted.join(", ")
        }
    ))
}

fn call_tool(params: &Value) -> Result<Value, String> {
    let name = required_str(params, "name")?;
    let definition = tools()
        .into_iter()
        .find(|tool| tool["name"].as_str() == Some(name))
        .ok_or_else(|| format!("Unknown tool: {name}"))?;
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !args.is_object() {
        if name.starts_with("riwork_schedule_") {
            return Ok(schedule_tool_error(schedule_argument(
                "Tool arguments must be a JSON object",
            )));
        }
        return Err("Tool arguments must be a JSON object".to_owned());
    }
    if let Some(message) = unaccepted_arguments(&definition, &args) {
        return Ok(if name.starts_with("riwork_schedule_") {
            schedule_tool_error(schedule_argument(message))
        } else {
            tool_error(&message)
        });
    }
    if name.starts_with("riwork_schedule_") {
        return Ok(match execute_schedule_tool(name, &args) {
            Ok(value) => tool_result(value),
            Err(error) => schedule_tool_error(error),
        });
    }
    Ok(match execute_tool(name, &args) {
        Ok(value) => tool_result(value),
        Err(error) => tool_error(&error),
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScheduleArguments {
    scope: Option<String>,
    project_id: Option<String>,
    worktree_id: Option<String>,
    shell_id: Option<String>,
    schedule_id: Option<String>,
    revision: Option<u64>,
    title: Option<String>,
    prompt: Option<String>,
    at: Option<String>,
    every_minutes: Option<u64>,
    once: Option<bool>,
}

impl ScheduleArguments {
    fn scope(&self) -> Result<ScopeInput, ScheduleError> {
        Ok(ScopeInput {
            scope: self
                .scope
                .clone()
                .ok_or_else(|| schedule_argument("scope is required"))?,
            project_id: self.project_id.clone(),
            worktree_id: self.worktree_id.clone(),
        })
    }
    fn key(&self) -> Result<ScheduleKey, ScheduleError> {
        Ok(ScheduleKey {
            id: self
                .schedule_id
                .clone()
                .ok_or_else(|| schedule_argument("schedule_id is required"))?,
            revision: self
                .revision
                .ok_or_else(|| schedule_argument("revision is required"))?,
            scope: self.scope()?,
            shell_id: self
                .shell_id
                .clone()
                .ok_or_else(|| schedule_argument("shell_id is required"))?,
        })
    }
}

fn schedule_argument(message: impl Into<String>) -> ScheduleError {
    ScheduleError {
        code: "invalid_argument",
        message: message.into(),
        current: None,
    }
}

fn execute_schedule_tool(name: &str, args: &Value) -> Result<Value, ScheduleError> {
    let args: ScheduleArguments = serde_json::from_value(args.clone())
        .map_err(|error| schedule_argument(format!("Invalid schedule arguments: {error}")))?;
    let service = ScheduleService::open_default()?;
    let required = |value: Option<String>, name: &str| {
        value.ok_or_else(|| schedule_argument(format!("{name} is required")))
    };
    match name {
        "riwork_schedule_list" => {
            let scope = if args.scope.is_some() {
                Some(args.scope()?)
            } else {
                if args.project_id.is_some() || args.worktree_id.is_some() {
                    return Err(schedule_argument(
                        "scope is required with project_id or worktree_id",
                    ));
                }
                None
            };
            Ok(json!({"items":service.list(scope.as_ref())?}))
        }
        "riwork_schedule_show" => {
            Ok(json!({"schedule":service.show(&required(args.schedule_id, "schedule_id")?)?}))
        }
        "riwork_schedule_create" => {
            if args.once == Some(true) {
                return Err(schedule_argument("once is only valid for update"));
            }
            Ok(json!({"schedule":service.create(CreateRequest {
                scope: args.scope()?,
                shell_id: required(args.shell_id, "shell_id")?,
                title: required(args.title, "title")?,
                prompt: required(args.prompt, "prompt")?,
                at: required(args.at, "at")?,
                every_minutes: args.every_minutes,
            })?}))
        }
        "riwork_schedule_update" => {
            if args.once == Some(true) && args.every_minutes.is_some() {
                return Err(schedule_argument("Choose once or every_minutes"));
            }
            let repeat = if args.once == Some(true) {
                RepeatChange::Once
            } else if let Some(minutes) = args.every_minutes {
                RepeatChange::EveryMinutes(minutes)
            } else {
                RepeatChange::Keep
            };
            Ok(json!({"schedule":service.update(UpdateRequest {
                key: args.key()?, title: args.title, prompt: args.prompt,
                at: required(args.at, "at")?, repeat,
            })?}))
        }
        "riwork_schedule_pause" | "riwork_schedule_resume" => {
            Ok(json!({"schedule":service.pause(&args.key()?, name.ends_with("pause"))?}))
        }
        "riwork_schedule_delete" => {
            Ok(json!({"deleted":true,"schedule":service.delete(&args.key()?)?}))
        }
        _ => Err(schedule_argument(format!("Unknown schedule tool '{name}'"))),
    }
}

fn execute_tool(name: &str, args: &Value) -> Result<Value, String> {
    match name {
        "riwork_project_list" => {
            let state = Store::open_default()?.snapshot()?;
            Ok(json!({"active_project_id":state.active_project_id,"items":state.projects}))
        }
        "riwork_project_add" => {
            let path = required_str(args, "path")?;
            let name = optional_str(args, "name")?;
            let project = Store::open_default()?.add_project(path, name)?;
            Ok(json!({"project":project}))
        }
        "riwork_project_inspect" => {
            let inspection = Store::inspect_project(required_str(args, "path")?)?;
            Ok(json!({"inspection":inspection}))
        }
        "riwork_project_create" => {
            let project = Store::open_default()?.create_project(
                required_str(args, "path")?,
                optional_str(args, "name")?,
                optional_bool(args, "init_git")?.unwrap_or(true),
            )?;
            Ok(json!({"project":project}))
        }
        "riwork_project_use" => {
            let project = Store::open_default()?.use_project(required_str(args, "project_id")?)?;
            Ok(json!({"project":project}))
        }
        "riwork_worktree_list" => {
            let store = Store::open_default()?;
            let all = optional_bool(args, "all")?.unwrap_or(false);
            let requested = optional_str(args, "project_id")?;
            if all && requested.is_some() {
                return Err("Use either all or project_id".to_owned());
            }
            let state = store.snapshot()?;
            if all {
                for project in &state.projects {
                    store.sync_worktrees(&project.id)?;
                }
            } else if let Some(selector) = requested {
                store.sync_worktrees(selector)?;
            } else if let Some(project_id) = state.active_project_id.as_deref() {
                store.sync_worktrees(project_id)?;
            }
            let state = store.snapshot()?;
            let project_id = if all {
                None
            } else {
                Some(select_project_id(&state, requested)?)
            };
            let items: Vec<_> = state
                .worktrees
                .iter()
                .filter(|worktree| {
                    project_id
                        .as_deref()
                        .is_none_or(|id| worktree.project_id == id)
                })
                .collect();
            Ok(json!({"items":items}))
        }
        "riwork_worktree_create" => {
            let branch = required_str(args, "branch")?;
            let requested = optional_str(args, "project_id")?;
            let path = optional_str(args, "path")?.map(Path::new);
            let base = optional_str(args, "base")?;
            let repository = optional_str(args, "repo")?;
            let store = Store::open_default()?;
            let project_id = select_project_id(&store.snapshot()?, requested)?;
            let worktree = if let Some(repository) = repository {
                store.create_worktree_in_repo(&project_id, branch, path, base, Some(repository))?
            } else {
                store.create_worktree(&project_id, branch, path, base)?
            };
            Ok(json!({"worktree":worktree}))
        }
        "riwork_task_list" => {
            let state = Store::open_default()?.snapshot()?;
            let all = optional_bool(args, "all")?.unwrap_or(false);
            let project = optional_str(args, "project_id")?;
            let worktree = optional_str(args, "worktree_id")?;
            if usize::from(all) + usize::from(project.is_some()) + usize::from(worktree.is_some())
                > 1
            {
                return Err("Use one of all, project_id, or worktree_id".to_owned());
            }
            let items = if all {
                state.tasks.iter().collect()
            } else if let Some(selector) = worktree {
                state.tasks_for_worktree(&state.worktree(selector)?.id)
            } else {
                state.tasks_for_project(&select_project_id(&state, project)?)
            };
            Ok(json!({"items":items}))
        }
        "riwork_task_add" => {
            let title = required_str(args, "title")?;
            let details = optional_str(args, "details")?.unwrap_or_default();
            let requested = optional_str(args, "project_id")?;
            let store = Store::open_default()?;
            let project_id = select_project_id(&store.snapshot()?, requested)?;
            let task = store.add_task(&project_id, title, details)?;
            Ok(json!({"task":task}))
        }
        "riwork_task_assign" => {
            let worktree_id = required_str(args, "worktree_id")?;
            let task_ids = required_string_array(args, "task_ids")?;
            let items = Store::open_default()?.assign_tasks(worktree_id, &task_ids)?;
            Ok(json!({"items":items}))
        }
        "riwork_task_unassign" => {
            let task_ids = required_string_array(args, "task_ids")?;
            let items = Store::open_default()?.unassign_tasks(&task_ids)?;
            Ok(json!({"items":items}))
        }
        "riwork_task_status" => {
            let task_id = required_str(args, "task_id")?;
            let status = TaskStatus::parse(required_str(args, "status")?)?;
            let task = Store::open_default()?.set_task_status(task_id, status)?;
            Ok(json!({"task":task}))
        }
        "riwork_search" => {
            let query = required_str(args, "query")?;
            let items = Store::open_default()?.snapshot()?.search(query);
            Ok(json!({"items":items}))
        }
        "riwork_shell_list" => {
            let all = optional_bool(args, "all")?.unwrap_or(false);
            let requested = optional_str(args, "project_id")?;
            if all && requested.is_some() {
                return Err("Use either all or project_id".to_owned());
            }
            let project_id = if all {
                None
            } else {
                Some(select_project_id(
                    &Store::open_default()?.snapshot()?,
                    requested,
                )?)
            };
            let items: Vec<_> = SessionManager::open_default()?
                .list()?
                .into_iter()
                .filter(|session| {
                    session.kind == ShellKind::Project
                        && project_id
                            .as_deref()
                            .is_none_or(|id| session.project_id.as_deref() == Some(id))
                })
                .collect();
            Ok(json!({"items":items}))
        }
        "riwork_shell_create" => {
            let state = Store::open_default()?.snapshot()?;
            let project = optional_str(args, "project_id")?;
            let worktree = optional_str(args, "worktree_id")?;
            let command = optional_str(args, "command")?.map(str::to_owned);
            let (project_id, worktree_id, cwd) = if let Some(selector) = worktree {
                // A named project scopes the selector, so a branch shared by
                // many projects (`main`) is not ambiguous.
                let worktree = match project {
                    Some(project_selector) => {
                        let selected = state.project(project_selector)?;
                        let worktree = state.worktree_in_project(&selected.id, selector)?;
                        if selected.id != worktree.project_id {
                            return Err("Worktree belongs to another project".to_owned());
                        }
                        worktree
                    }
                    None => state.worktree(selector)?,
                };
                (
                    worktree.project_id.clone(),
                    Some(worktree.id.clone()),
                    worktree.path.clone(),
                )
            } else {
                let project_id = select_project_id(&state, project)?;
                let project = state.project(&project_id)?;
                let primary = state
                    .worktrees_for(&project_id)
                    .into_iter()
                    .find(|worktree| worktree.is_primary);
                (
                    project_id,
                    primary.map(|worktree| worktree.id.clone()),
                    project.root.clone(),
                )
            };
            let session =
                SessionManager::open_default()?.create(project_id, worktree_id, cwd, command)?;
            Ok(json!({"session":session}))
        }
        "riwork_shell_output" => {
            let id = required_str(args, "shell_id")?;
            let lines = optional_usize(args, "lines")?
                .unwrap_or(200)
                .clamp(1, 100_000);
            let output = SessionManager::open_default()?.capture(id, lines)?;
            Ok(json!({"shell_id":id,"output":output}))
        }
        "riwork_shell_cwd" => {
            let id = required_str(args, "shell_id")?;
            let cwd = SessionManager::open_default()?.current_directory(id)?;
            Ok(json!({"shell_id":id,"cwd":cwd}))
        }
        "riwork_shell_metrics" => {
            let manager = SessionManager::open_default()?;
            if let Some(id) = optional_str(args, "shell_id")? {
                Ok(json!({"shell_id":id,"metrics":manager.metrics(id)?}))
            } else {
                Ok(json!({"items":manager.metrics_snapshot()?}))
            }
        }
        "riwork_shell_send_line" => {
            let id = required_str(args, "shell_id")?;
            let text = required_str(args, "text")?;
            SessionManager::open_default()?.send(id, text)?;
            Ok(json!({"shell_id":id,"sent":true}))
        }
        "riwork_orchestrator_status" => {
            let manager = SessionManager::open_default()?;
            let project_id = orchestrator_project(optional_str(args, "project_id")?)?;
            orchestrator_status(&manager, project_id.as_deref())
        }
        "riwork_orchestrator_output" => {
            let manager = SessionManager::open_default()?;
            let project_id = orchestrator_project(optional_str(args, "project_id")?)?;
            let lines = optional_usize(args, "lines")?
                .unwrap_or(200)
                .clamp(1, 100_000);
            orchestrator_output(&manager, project_id.as_deref(), lines)
        }
        _ => Err(format!("Unknown RiWork tool '{name}'")),
    }
}

/// The project a tool names, by the exact id the store knows it under.
fn orchestrator_project(selector: Option<&str>) -> Result<Option<String>, String> {
    selector
        .map(|selector| {
            Ok(Store::open_default()?
                .snapshot()?
                .project(selector)?
                .id
                .clone())
        })
        .transpose()
}

/// The orchestrator of a scope, whichever way it runs. Looking starts no chat host.
fn selected_orchestrator(
    manager: &SessionManager,
    home: &Path,
    project_id: Option<&str>,
) -> Result<Option<Orchestrator>, String> {
    let host = orchestrators::ChatHost {
        home,
        ensure: &orchestrators::system_ensure,
    };
    orchestrators::find(manager, &host, &orchestrators::scope_of(project_id))
}

/// `riwork_orchestrator_status`: the orchestrator as `riwork orchestrator status --json`
/// shows it (`null` when the scope has none), with its `mode`.
fn orchestrator_status(
    manager: &SessionManager,
    project_id: Option<&str>,
) -> Result<Value, String> {
    let home = manager.state_home();
    let found = selected_orchestrator(manager, home, project_id)?;
    Ok(json!({"session": found.map(|found| orchestrators::entry(home, &found))}))
}

/// `riwork_orchestrator_output`: the pane and scrollback of a terminal orchestrator, or the
/// conversation of a chat orchestrator as plain text, in the same fields.
fn orchestrator_output(
    manager: &SessionManager,
    project_id: Option<&str>,
    lines: usize,
) -> Result<Value, String> {
    let home = manager.state_home();
    let found =
        selected_orchestrator(manager, home, project_id)?.ok_or("No orchestrator shell exists")?;
    match found {
        Orchestrator::Terminal(session) => {
            let output = manager.capture(&session.id, lines)?;
            Ok(json!({"shell_id":session.id,"output":output}))
        }
        Orchestrator::Chat(chat) => {
            let output = orchestrators::output(home, &chat, lines)?.join("\n");
            Ok(json!({"shell_id":chat.id,"mode":"chat","chat_id":chat.id,"output":output}))
        }
    }
}

fn select_project_id(state: &State, selector: Option<&str>) -> Result<String, String> {
    match selector {
        Some(selector) => Ok(state.project(selector)?.id.clone()),
        None => state
            .active_project_id
            .clone()
            .ok_or("No active project; add or select a project first".to_owned()),
    }
}

fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("'{key}' must be a nonempty string"))
}

fn optional_str<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match args.get(key) {
        Some(Value::String(value)) => Ok(Some(value)),
        Some(Value::Null) | None => Ok(None),
        _ => Err(format!("'{key}' must be a string")),
    }
}

fn optional_bool(args: &Value, key: &str) -> Result<Option<bool>, String> {
    match args.get(key) {
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(Value::Null) | None => Ok(None),
        _ => Err(format!("'{key}' must be a boolean")),
    }
}

fn optional_usize(args: &Value, key: &str) -> Result<Option<usize>, String> {
    match args.get(key) {
        Some(Value::Number(value)) => value
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .map(Some)
            .ok_or_else(|| format!("'{key}' must be a nonnegative integer")),
        Some(Value::Null) | None => Ok(None),
        _ => Err(format!("'{key}' must be a nonnegative integer")),
    }
}

fn required_string_array(args: &Value, key: &str) -> Result<Vec<String>, String> {
    args.get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("'{key}' must be an array of strings"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| format!("'{key}' must contain only nonempty strings"))
        })
        .collect()
}

/// Tools whose effect goes beyond bookkeeping: they run a command, type into an
/// agent now or later (a schedule), or delete state. Clients use this hint to
/// decide when to ask, so it errs toward true.
fn destructive(name: &str) -> bool {
    matches!(
        name,
        "riwork_shell_create"
            | "riwork_shell_send_line"
            | "riwork_schedule_create"
            | "riwork_schedule_update"
            | "riwork_schedule_resume"
            | "riwork_schedule_delete"
    )
}

fn tool(
    name: &str,
    title: &str,
    description: &str,
    properties: Value,
    required: &[&str],
    read_only: bool,
) -> Value {
    json!({
        "name":name,
        "title":title,
        "description":description,
        "inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false},
        "annotations":{"readOnlyHint":read_only,"destructiveHint":destructive(name)}
    })
}

fn schedule_tool(
    name: &str,
    title: &str,
    description: &str,
    required: &[&str],
    read_only: bool,
) -> Value {
    let mut properties = json!({
        "scope":{"type":"string","enum":["app","project","workspace"],"description":"Explicit scope. Project requires project_id; workspace requires project_id and worktree_id."},
        "project_id":{"type":"string","format":"uuid","description":"Full canonical project UUID for project/workspace scope."},
        "worktree_id":{"type":"string","format":"uuid","description":"Full canonical worktree UUID for workspace scope."},
        "shell_id":{"type":"string","format":"uuid","description":"Full canonical UUID of the existing pinned session."},
        "schedule_id":{"type":"string","format":"uuid","description":"Full canonical schedule UUID."},
        "revision":{"type":"integer","minimum":1,"description":"Current revision returned by list/show; required for every mutation after create."},
        "title":{"type":"string","minLength":1,"maxLength":120},
        "prompt":{"type":"string","minLength":1,"maxLength":16384,"description":"Single line, no control characters."},
        "at":{"type":"string","format":"date-time","description":"Exact future RFC 3339 instant with seconds and timezone, e.g. 2026-10-05T09:00:00+02:00."},
        "every_minutes":{"type":"integer","minimum":5,"maximum":525600,"description":"Optional elapsed-time recurrence, whole minutes from 5 to 525600."},
        "once":{"type":"boolean","description":"On update, set true to clear recurrence. Omit to retain existing recurrence."}
    });
    let allowed: &[&str] = match name {
        "riwork_schedule_list" => &["scope", "project_id", "worktree_id"],
        "riwork_schedule_show" => &["schedule_id"],
        "riwork_schedule_create" => &[
            "scope",
            "project_id",
            "worktree_id",
            "shell_id",
            "title",
            "prompt",
            "at",
            "every_minutes",
        ],
        "riwork_schedule_update" => &[
            "schedule_id",
            "revision",
            "scope",
            "project_id",
            "worktree_id",
            "shell_id",
            "title",
            "prompt",
            "at",
            "every_minutes",
            "once",
        ],
        _ => &[
            "schedule_id",
            "revision",
            "scope",
            "project_id",
            "worktree_id",
            "shell_id",
        ],
    };
    properties
        .as_object_mut()
        .unwrap()
        .retain(|key, _| allowed.contains(&key.as_str()));
    let mut value = tool(name, title, description, properties, required, read_only);
    if name != "riwork_schedule_show" {
        value["inputSchema"]["allOf"] = json!([
            {"if":{"properties":{"scope":{"const":"project"}},"required":["scope"]},"then":{"required":["project_id"]}},
            {"if":{"properties":{"scope":{"const":"workspace"}},"required":["scope"]},"then":{"required":["project_id","worktree_id"]}}
        ]);
    }
    let scope = json!({"oneOf":[
        {"type":"object","properties":{"scope":{"const":"app"}},"required":["scope"]},
        {"type":"object","properties":{"scope":{"const":"project"},"project_id":{"type":"string","format":"uuid"}},"required":["scope","project_id"]},
        {"type":"object","properties":{"scope":{"const":"workspace"},"project_id":{"type":"string","format":"uuid"},"worktree_id":{"type":"string","format":"uuid"}},"required":["scope","project_id","worktree_id"]}
    ]});
    let timing = json!({"oneOf":[
        {"type":"object","properties":{"kind":{"const":"once"},"at":{"type":"integer","minimum":0}},"required":["kind","at"]},
        {"type":"object","properties":{"kind":{"const":"interval"},"first":{"type":"integer","minimum":0},"seconds":{"type":"integer","minimum":300,"maximum":31536000}},"required":["kind","first","seconds"]}
    ]});
    let target = json!({"type":"object","properties":{
        "scope":scope,"shell_id":{"type":"string","format":"uuid"},
        "created_at":{"type":"integer","minimum":0},"command":{"type":["string","null"]},
        "harness":{"type":"string","enum":["codex","claude"]},
        "codex_home":{"type":["string","null"]},"pane_identity":{"type":"string"},
        "provider_session":{"type":"string"},
        "chat":{"type":"object","properties":{"codex_account_id":{"type":["string","null"]}}}
    },"required":["scope","shell_id","created_at","command","harness","codex_home","pane_identity","provider_session"]});
    let run = json!({"type":["object","null"],"properties":{
        "due_at":{"type":"integer","minimum":0},"observed_at":{"type":"integer","minimum":0},
        "outcome":{"type":"string","enum":["dispatching","submitted","deferred","missed","failed","uncertain"]},
        "message":{"type":"string"}
    }});
    let schedule = json!({"type":"object","properties":{
        "id":{"type":"string","format":"uuid"},"revision":{"type":"integer","minimum":1},
        "title":{"type":"string"},"prompt":{"type":"string"},
        "target":target,"timing":timing,
        "paused":{"type":"boolean"},"review_required":{"type":"boolean"},
        "next_run":{"type":["integer","null"]},"last_run":run,
        "check_after":{"type":"integer","minimum":0}
    },"required":["id","revision","title","prompt","target","timing","paused","review_required","next_run","last_run","check_after"]});
    value["outputSchema"] = if name.ends_with("list") {
        json!({"type":"object","properties":{"items":{"type":"array","items":schedule}},"required":["items"]})
    } else if name.ends_with("delete") {
        json!({"type":"object","properties":{"deleted":{"type":"boolean"},"schedule":schedule},"required":["deleted","schedule"]})
    } else {
        json!({"type":"object","properties":{"schedule":schedule},"required":["schedule"]})
    };
    value
}

fn tools() -> Vec<Value> {
    let mut items = vec![
        tool(
            "riwork_project_list",
            "List projects",
            "List registered projects and the active project UUID.",
            json!({}),
            &[],
            true,
        ),
        tool(
            "riwork_project_add",
            "Add project",
            "Passively register a local project directory and discover its repositories/worktrees. Does not initialize Git.",
            json!({"path":{"type":"string"},"name":{"type":"string"}}),
            &["path"],
            false,
        ),
        tool(
            "riwork_project_inspect",
            "Inspect project folder",
            "Read a project directory's repositories and whether Git initialization is available. Missing paths are inspected without creating them.",
            json!({"path":{"type":"string"}}),
            &["path"],
            true,
        ),
        tool(
            "riwork_project_create",
            "Create project",
            "Create a project directory if needed and register it. Git initializes by default only when no project repository exists; existing repositories are never wrapped. Set init_git false for a plain folder.",
            json!({"path":{"type":"string"},"name":{"type":"string"},"init_git":{"type":"boolean","default":true}}),
            &["path"],
            false,
        ),
        tool(
            "riwork_project_use",
            "Select project",
            "Choose the default project for future RiWork windows and CLI commands. Existing windows retain their project.",
            json!({"project_id":{"type":"string"}}),
            &["project_id"],
            false,
        ),
        tool(
            "riwork_worktree_list",
            "List worktrees",
            "List Git worktrees, refreshing externally created worktrees first. The refresh registers newly found worktrees in RiWork's saved state, so this is not read-only. Defaults to the active project.",
            json!({"project_id":{"type":"string"},"all":{"type":"boolean"}}),
            &[],
            false,
        ),
        tool(
            "riwork_worktree_create",
            "Create worktree",
            "Create and register a Git worktree on a branch. Defaults to the active project. Multi-repository projects require repo (repository path, name, or one of its worktree UUIDs). The repository needs an initial commit or an existing base. base only chooses the start point of a new branch; if the branch already exists it is checked out as is and base is ignored.",
            json!({"project_id":{"type":"string"},"repo":{"type":"string"},"branch":{"type":"string"},"path":{"type":"string"},"base":{"type":"string"}}),
            &["branch"],
            false,
        ),
        tool(
            "riwork_task_list",
            "List tasks",
            "List tasks for a project, worktree, or all projects. Defaults to the active project.",
            json!({"project_id":{"type":"string"},"worktree_id":{"type":"string"},"all":{"type":"boolean"}}),
            &[],
            true,
        ),
        tool(
            "riwork_task_add",
            "Add task",
            "Create a task in a project. Defaults to the active project.",
            json!({"project_id":{"type":"string"},"title":{"type":"string"},"details":{"type":"string"}}),
            &["title"],
            false,
        ),
        tool(
            "riwork_task_assign",
            "Assign tasks",
            "Assign multiple tasks to one worktree atomically; all must belong to the same project.",
            json!({"worktree_id":{"type":"string"},"task_ids":{"type":"array","items":{"type":"string"},"minItems":1}}),
            &["worktree_id", "task_ids"],
            false,
        ),
        tool(
            "riwork_task_unassign",
            "Unassign tasks",
            "Remove worktree assignment from multiple tasks atomically.",
            json!({"task_ids":{"type":"array","items":{"type":"string"},"minItems":1}}),
            &["task_ids"],
            false,
        ),
        tool(
            "riwork_task_status",
            "Set task status",
            "Set a task status to todo, in_progress, or done.",
            json!({"task_id":{"type":"string"},"status":{"type":"string","enum":["todo","in_progress","done"]}}),
            &["task_id", "status"],
            false,
        ),
        tool(
            "riwork_search",
            "Search workspace",
            "Search projects, worktrees, and tasks by name, path, title, details, or UUID.",
            json!({"query":{"type":"string"}}),
            &["query"],
            true,
        ),
        tool(
            "riwork_shell_list",
            "List shells",
            "List persistent project shells with UUIDs, ownership, worktree, and live state.",
            json!({"project_id":{"type":"string"},"all":{"type":"boolean"}}),
            &[],
            true,
        ),
        tool(
            "riwork_shell_create",
            "Create shell",
            "Start a persistent project shell in a project or worktree. command runs as an arbitrary program with your user's permissions; set it to codex, claude, or grok to launch an official CLI session. worktree_id may be a UUID, unique prefix, branch, or path; with project_id it is looked up in that project first.",
            json!({"project_id":{"type":"string"},"worktree_id":{"type":"string"},"command":{"type":"string"}}),
            &[],
            false,
        ),
        tool(
            "riwork_shell_output",
            "Read shell output",
            "Read current tmux pane and scrollback for a shell UUID.",
            json!({"shell_id":{"type":"string"},"lines":{"type":"integer","minimum":1,"maximum":100000}}),
            &["shell_id"],
            true,
        ),
        tool(
            "riwork_shell_cwd",
            "Read shell directory",
            "Read the current working directory of a live shell UUID.",
            json!({"shell_id":{"type":"string"}}),
            &["shell_id"],
            true,
        ),
        tool(
            "riwork_shell_metrics",
            "Read shell metrics",
            "Read CPU percent and resident RAM for one shell UUID or all live shells.",
            json!({"shell_id":{"type":"string"}}),
            &[],
            true,
        ),
        tool(
            "riwork_shell_send_line",
            "Send shell line",
            "Send literal text followed by Return to a live shell UUID. This can run commands in that shell.",
            json!({"shell_id":{"type":"string"},"text":{"type":"string"}}),
            &["shell_id", "text"],
            false,
        ),
        tool(
            "riwork_orchestrator_status",
            "Read orchestrator status",
            "Read an orchestrator and its live state, if present: a terminal session, or a chat (its entry has mode chat, chat_id, provider and state). Omit project_id for the global orchestrator; set a project selector for that project's orchestrator.",
            json!({"project_id":{"type":"string"}}),
            &[],
            true,
        ),
        tool(
            "riwork_orchestrator_output",
            "Read orchestrator output",
            "Read the current tmux pane and scrollback of a terminal orchestrator, or the last lines of a chat orchestrator's conversation as plain text. Omit project_id for the global orchestrator; set a project selector for a project's orchestrator.",
            json!({"project_id":{"type":"string"},"lines":{"type":"integer","minimum":1,"maximum":100000}}),
            &[],
            true,
        ),
    ];
    items.extend([
        schedule_tool("riwork_schedule_list", "List schedules", "List schedules in the configured RIWORK_HOME ledger; omit scope for all, or filter by explicit app/project/workspace identity. No sessions are opened or dispatched.", &[], true),
        schedule_tool("riwork_schedule_show", "Show schedule", "Read one schedule by its full UUID, including current revision, pinned target and latest outcome.", &["schedule_id"], true),
        schedule_tool("riwork_schedule_create", "Create schedule", "Bind an existing live Codex/Claude session in the explicit scope and schedule a future prompt. RiWork later types that prompt into the agent session, only while the desktop app is open.", &["scope","shell_id","title","prompt","at"], false),
        schedule_tool("riwork_schedule_update", "Update schedule", "Edit a pinned schedule with its full UUID, current revision, explicit scope and shell UUID. Provide a new exact future at; omit recurrence to retain it, use once=true to clear it. The new prompt is later typed into the pinned agent session.", &["schedule_id","revision","scope","shell_id","at"], false),
        schedule_tool("riwork_schedule_pause", "Pause schedule", "Pause a schedule using its full UUID, current revision, explicit scope and pinned shell UUID.", &["schedule_id","revision","scope","shell_id"], false),
        schedule_tool("riwork_schedule_resume", "Resume schedule", "Resume a schedule using its full UUID, current revision, explicit scope and pinned shell UUID. Its prompt will again be typed into the agent session when due. Failed/uncertain outcomes require a future edit first.", &["schedule_id","revision","scope","shell_id"], false),
        schedule_tool("riwork_schedule_delete", "Delete schedule", "Delete a schedule using its full UUID, current revision, explicit scope and pinned shell UUID.", &["schedule_id","revision","scope","shell_id"], false),
    ]);
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ping(id: u64) -> Value {
        json!({"jsonrpc":"2.0","id":id,"method":"ping"})
    }

    fn reply(message: &Value) -> Option<Value> {
        handle_line(message.to_string().as_bytes())
    }

    #[test]
    fn orchestrator_tools_read_a_chat_orchestrator_the_way_they_read_a_terminal_one() {
        use crate::chat::client::socket_path;
        use crate::chat::model::{ChatCommand, ChatState, Provider};
        use crate::chat::testing::TestHost;
        use crate::settings::OrchestratorRuns;

        fn in_process(home: &Path) -> Result<std::path::PathBuf, String> {
            Ok(socket_path(home))
        }

        /// Ends the tmux server the terminal orchestrator below starts.
        struct Server(SessionManager);
        impl Drop for Server {
            fn drop(&mut self) {
                self.0.kill_server();
            }
        }

        let host = TestHost::new();
        let manager = SessionManager::at(host.home.clone()).unwrap();
        let _server = Server(manager.clone());
        // Nothing yet: status says so, output cannot read it.
        assert_eq!(
            orchestrator_status(&manager, None).unwrap(),
            json!({"session": null})
        );
        assert_eq!(
            orchestrator_output(&manager, None, 10).unwrap_err(),
            "No orchestrator shell exists"
        );

        let chat_host = orchestrators::ChatHost {
            home: &host.home,
            ensure: &in_process,
        };
        let (made, created) = orchestrators::create(
            &manager,
            &chat_host,
            &crate::chat::model::OrchestratorScope::Global,
            None,
            host.home.clone(),
            None,
            OrchestratorRuns::Chat(Provider::Codex),
        )
        .unwrap();
        assert!(created);
        let Orchestrator::Chat(chat) = made else {
            panic!("a chat was asked for");
        };
        host.wait_for_state(&chat.id, |state| *state == ChatState::Idle);
        host.client()
            .command(
                &chat.id,
                ChatCommand::Send {
                    text: "what is open?".into(),
                },
            )
            .unwrap();
        host.wait_for_log(&chat.id, |log| {
            log.iter()
                .filter(|e| matches!(e.event, crate::chat::model::ChatEvent::TurnCompleted { .. }))
                .count()
                == 2
        });

        // The status is the chat's entry: its mode, its chat, its state.
        let status = orchestrator_status(&manager, None).unwrap();
        let session = &status["session"];
        assert_eq!(session["id"], chat.id);
        assert_eq!(session["mode"], "chat");
        assert_eq!(session["chat_id"], chat.id);
        assert_eq!(session["provider"], "codex");
        assert_eq!(session["kind"], "orchestrator");
        assert_eq!(session["state"], "idle");
        // A project's orchestrator is another scope.
        assert_eq!(
            orchestrator_status(&manager, Some("11111111-1111-4111-8111-111111111111")).unwrap(),
            json!({"session": null})
        );
        // The output is the conversation as text, in the fields a terminal's has.
        let output = orchestrator_output(&manager, None, 2).unwrap();
        assert_eq!(output["shell_id"], chat.id);
        assert_eq!(output["mode"], "chat");
        assert_eq!(
            output["output"],
            "user: what is open?\nagent: echo: what is open?"
        );

        // A terminal orchestrator in another scope says it is one.
        let project = "22222222-2222-4222-8222-222222222222";
        let root = host.home.join("project");
        std::fs::create_dir_all(&root).unwrap();
        let terminal = manager
            .orchestrator_create_for_project(project.into(), root, Some("sleep 600".into()))
            .unwrap();
        let status = orchestrator_status(&manager, Some(project)).unwrap();
        assert_eq!(status["session"]["id"], terminal.id.as_str());
        assert_eq!(status["session"]["mode"], "terminal");
    }

    #[test]
    fn a_batch_answers_with_an_array_of_only_the_responses_that_exist() {
        let response = reply(&json!([
            ping(1),
            {"jsonrpc":"2.0","method":"notifications/initialized"},
            {"jsonrpc":"2.0","id":"two","method":"tools/list"}
        ]))
        .unwrap();
        let responses = response.as_array().unwrap();
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0]["id"], 1);
        assert_eq!(responses[1]["id"], "two");
        assert!(responses[1]["result"]["tools"].is_array());
        // Only notifications: JSON-RPC forbids an empty response array.
        assert_eq!(
            reply(&json!([{"jsonrpc":"2.0","method":"notifications/initialized"}])),
            None
        );
    }

    #[test]
    fn empty_and_non_object_messages_are_never_ignored() {
        let empty = reply(&json!([])).unwrap();
        assert_eq!(empty["error"]["code"], -32600);
        assert!(empty["id"].is_null());
        for message in [json!(1), json!("text"), json!(null), json!(true), json!({})] {
            let response = reply(&message).unwrap();
            assert_eq!(response["error"]["code"], -32600, "{message}");
            assert!(response["id"].is_null(), "{message}");
        }
        let batch = reply(&json!([1, ping(3), []])).unwrap();
        let batch = batch.as_array().unwrap();
        assert_eq!(batch.len(), 3);
        assert_eq!(batch[0]["error"]["code"], -32600);
        assert_eq!(batch[1]["id"], 3);
        assert_eq!(batch[2]["error"]["code"], -32600);
    }

    #[test]
    fn notifications_stay_silent_but_a_request_without_a_method_does_not() {
        assert_eq!(
            reply(&json!({"jsonrpc":"2.0","method":"notifications/cancelled"})),
            None
        );
        let response = reply(&json!({"jsonrpc":"2.0","id":9})).unwrap();
        assert_eq!(response["id"], 9);
        assert_eq!(response["error"]["code"], -32600);
    }

    #[test]
    fn undecodable_lines_get_a_parse_error() {
        for line in [&b"{not json"[..], b"\xff\xfe{\"id\":1}\n", b"\xc3\x28"] {
            let response = handle_line(line).unwrap();
            assert_eq!(response["error"]["code"], -32700);
            assert!(response["id"].is_null());
        }
    }

    fn text(result: &Value) -> &str {
        result["content"][0]["text"].as_str().unwrap()
    }

    #[test]
    fn every_tool_rejects_arguments_its_schema_does_not_declare() {
        for definition in tools() {
            let name = definition["name"].as_str().unwrap();
            let result = call_tool(&json!({
                "name":name,"arguments":{"not_a_real_argument":true}
            }))
            .unwrap();
            assert_eq!(result["isError"], true, "{name}");
            assert!(
                result.get("structuredContent").is_none(),
                "{name}: {result}"
            );
            let message = text(&result);
            assert!(
                message.contains("'not_a_real_argument' is not accepted by")
                    && message.contains(name),
                "{name}: {message}"
            );
        }
    }

    #[test]
    fn a_misspelled_selector_does_not_fall_back_to_a_default() {
        let result = call_tool(&json!({
            "name":"riwork_shell_create","arguments":{"worktree":"main","command":"true"}
        }))
        .unwrap();
        assert_eq!(result["isError"], true);
        assert!(text(&result).contains("'worktree' is not accepted by riwork_shell_create"));
        assert!(text(&result).contains("worktree_id"));
    }

    #[test]
    fn declared_arguments_pass_the_gate() {
        let definitions = tools();
        for definition in &definitions {
            let properties = definition["inputSchema"]["properties"].as_object().unwrap();
            let args = Value::Object(
                properties
                    .keys()
                    .map(|key| (key.clone(), Value::Null))
                    .collect(),
            );
            assert_eq!(unaccepted_arguments(definition, &args), None);
        }
    }

    #[test]
    fn hints_do_not_understate_what_a_tool_can_do() {
        let read_only = [
            "riwork_project_list",
            "riwork_project_inspect",
            "riwork_task_list",
            "riwork_search",
            "riwork_shell_list",
            "riwork_shell_output",
            "riwork_shell_cwd",
            "riwork_shell_metrics",
            "riwork_orchestrator_status",
            "riwork_orchestrator_output",
            "riwork_schedule_list",
            "riwork_schedule_show",
        ];
        // Run a command, type into an agent now or on a schedule, or delete.
        let destructive = [
            "riwork_shell_create",
            "riwork_shell_send_line",
            "riwork_schedule_create",
            "riwork_schedule_update",
            "riwork_schedule_resume",
            "riwork_schedule_delete",
        ];
        for definition in tools() {
            let name = definition["name"].as_str().unwrap();
            let hints = &definition["annotations"];
            assert_eq!(
                hints["readOnlyHint"],
                read_only.contains(&name),
                "readOnlyHint for {name}"
            );
            assert_eq!(
                hints["destructiveHint"],
                destructive.contains(&name),
                "destructiveHint for {name}"
            );
        }
        let list = tools()
            .into_iter()
            .find(|tool| tool["name"] == "riwork_worktree_list")
            .unwrap();
        // Listing refreshes worktrees into state.json.
        assert_eq!(list["annotations"]["readOnlyHint"], false);
    }

    #[test]
    fn schedule_failures_keep_code_and_conflict_without_structured_content() {
        let result = schedule_tool_error(schedule_argument("bad input"));
        assert_eq!(result["isError"], true);
        assert!(result.get("structuredContent").is_none());
        let error: Value = serde_json::from_str(text(&result)).unwrap();
        assert_eq!(error["error"]["code"], "invalid_argument");
        assert_eq!(error["error"]["message"], "bad input");
    }
}
