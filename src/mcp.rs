//! A local stdio MCP bridge for the same workspace data used by GPUI and the CLI.
//!
//! This server deliberately writes only JSON-RPC to stdout. A host can launch
//! `riwork mcp` and discover the available operations through `tools/list`.

use std::{
    io::{self, BufRead, BufWriter, Write},
    path::Path,
};

use serde_json::{Value, json};

use crate::{
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
        "instructions":"Use these tools to manage RiWork projects, Git worktrees, and tasks. Shell output and metrics are snapshots of persistent local tmux sessions."
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
        return Err("Tool arguments must be a JSON object".to_owned());
    }
    Ok(match execute_tool(name, &args) {
        Ok(value) => tool_result(value),
        Err(error) => tool_error(&error),
    })
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

fn tools() -> Vec<Value> {
    vec![
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
            "Start a persistent project shell in a project or worktree. Set command to codex or claude to launch an official CLI session.",
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
    ]
}
