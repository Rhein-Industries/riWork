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
    let stdin = io::stdin();
    let mut stdout = BufWriter::new(io::stdout().lock());
    for line in stdin.lock().lines() {
        let line = line.map_err(|error| format!("read MCP request: {error}"))?;
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(request) => respond(&request),
            Err(error) => Some(rpc_error(
                Value::Null,
                -32700,
                &format!("Parse error: {error}"),
            )),
        };
        if let Some(response) = response {
            serde_json::to_writer(&mut stdout, &response)
                .map_err(|error| format!("encode MCP response: {error}"))?;
            stdout
                .write_all(b"\n")
                .and_then(|_| stdout.flush())
                .map_err(|error| format!("write MCP response: {error}"))?;
        }
    }
    Ok(())
}

fn respond(request: &Value) -> Option<Value> {
    let id = request.get("id").cloned();
    let Some(id) = id else {
        // Notifications, including notifications/initialized and cancelled,
        // have no response.
        return None;
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

fn schedule_tool_error(error: ScheduleError) -> Value {
    let text = error.message.clone();
    json!({"content":[{"type":"text","text":text}],"structuredContent":{"error":error},"isError":true})
}

fn call_tool(params: &Value) -> Result<Value, String> {
    let name = required_str(params, "name")?;
    if !tools()
        .iter()
        .any(|tool| tool["name"].as_str() == Some(name))
    {
        return Err(format!("Unknown tool: {name}"));
    }
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
        "riwork_schedule_pause" | "riwork_schedule_resume" | "riwork_schedule_delete" => &[
            "schedule_id",
            "revision",
            "scope",
            "project_id",
            "worktree_id",
            "shell_id",
        ],
        _ => return Err(schedule_argument(format!("Unknown schedule tool '{name}'"))),
    };
    if let Some(key) = args
        .as_object()
        .and_then(|map| map.keys().find(|key| !allowed.contains(&key.as_str())))
    {
        return Err(schedule_argument(format!(
            "'{key}' is not accepted by {name}"
        )));
    }
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
                let worktree = state.worktree(selector)?;
                if let Some(project_selector) = project {
                    if state.project(project_selector)?.id != worktree.project_id {
                        return Err("Worktree belongs to another project".to_owned());
                    }
                }
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
            let session = selected_orchestrator(&manager, optional_str(args, "project_id")?)?;
            Ok(json!({"session":session}))
        }
        "riwork_orchestrator_output" => {
            let manager = SessionManager::open_default()?;
            let session = selected_orchestrator(&manager, optional_str(args, "project_id")?)?
                .ok_or("No orchestrator shell exists")?;
            let lines = optional_usize(args, "lines")?
                .unwrap_or(200)
                .clamp(1, 100_000);
            let output = manager.capture(&session.id, lines)?;
            Ok(json!({"shell_id":session.id,"output":output}))
        }
        _ => Err(format!("Unknown RiWork tool '{name}'")),
    }
}

fn selected_orchestrator(
    manager: &SessionManager,
    project_selector: Option<&str>,
) -> Result<Option<crate::sessions::ShellSession>, String> {
    match project_selector {
        Some(selector) => {
            let state = Store::open_default()?.snapshot()?;
            let project = state.project(selector)?;
            manager.orchestrator_get_for_project(&project.id)
        }
        None => manager.orchestrator_get(),
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
        "annotations":{"readOnlyHint":read_only,"destructiveHint":name == "riwork_shell_send_line"}
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
        "provider_session":{"type":"string"}
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
    value["annotations"]["destructiveHint"] = json!(name.ends_with("delete"));
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
            "List Git worktrees, refreshing externally created worktrees first. Defaults to the active project.",
            json!({"project_id":{"type":"string"},"all":{"type":"boolean"}}),
            &[],
            true,
        ),
        tool(
            "riwork_worktree_create",
            "Create worktree",
            "Create and register a Git worktree on a branch. Defaults to the active project. Multi-repository projects require repo (repository path, name, or one of its worktree UUIDs). The repository needs an initial commit or an existing base.",
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
            "Start a persistent project shell in a project or worktree. Set command to codex, claude, or grok to launch an official CLI session.",
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
            "Read an orchestrator shell and its live state, if present. Omit project_id for the global orchestrator; set a project selector for that project's orchestrator.",
            json!({"project_id":{"type":"string"}}),
            &[],
            true,
        ),
        tool(
            "riwork_orchestrator_output",
            "Read orchestrator output",
            "Read current tmux pane and scrollback of an orchestrator shell. Omit project_id for the global orchestrator; set a project selector for a project's orchestrator.",
            json!({"project_id":{"type":"string"},"lines":{"type":"integer","minimum":1,"maximum":100000}}),
            &[],
            true,
        ),
    ];
    items.extend([
        schedule_tool("riwork_schedule_list", "List schedules", "List schedules in the configured RIWORK_HOME ledger; omit scope for all, or filter by explicit app/project/workspace identity. No sessions are opened or dispatched.", &[], true),
        schedule_tool("riwork_schedule_show", "Show schedule", "Read one schedule by its full UUID, including current revision, pinned target and latest outcome.", &["schedule_id"], true),
        schedule_tool("riwork_schedule_create", "Create schedule", "Bind an existing live Codex/Claude session in the explicit scope and schedule a future prompt. Dispatch occurs only while RiWork desktop is open.", &["scope","shell_id","title","prompt","at"], false),
        schedule_tool("riwork_schedule_update", "Update schedule", "Edit a pinned schedule with its full UUID, current revision, explicit scope and shell UUID. Provide a new exact future at; omit recurrence to retain it, use once=true to clear it.", &["schedule_id","revision","scope","shell_id","at"], false),
        schedule_tool("riwork_schedule_pause", "Pause schedule", "Pause a schedule using its full UUID, current revision, explicit scope and pinned shell UUID.", &["schedule_id","revision","scope","shell_id"], false),
        schedule_tool("riwork_schedule_resume", "Resume schedule", "Resume a schedule using its full UUID, current revision, explicit scope and pinned shell UUID. Failed/uncertain outcomes require a future edit first.", &["schedule_id","revision","scope","shell_id"], false),
        schedule_tool("riwork_schedule_delete", "Delete schedule", "Delete a schedule using its full UUID, current revision, explicit scope and pinned shell UUID.", &["schedule_id","revision","scope","shell_id"], false),
    ]);
    items
}
