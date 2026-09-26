//! The command line view of the same data the GPUI workspace renders.

use std::{
    env,
    io::{self, Read},
    path::PathBuf,
    process::{Command, Stdio},
};

use serde::Serialize;
use serde_json::json;

use crate::{
    sessions::{HarnessKind, SessionManager, ShellKind, ShellSession},
    store::{SearchHit, State, Store, Task, TaskStatus, Worktree},
};

const HELP: &str = "\
riwork [PROJECT_PATH]                  Open the GPUI workspace
riwork open [PROJECT_OR_PATH]           Open an independent project window
riwork usage [--shell ID]               Read harness subscription usage
riwork project add PATH [--name NAME]   Register a project and its root worktree
riwork project create [PATH] [--name NAME] [--no-git]   Create a project (Git by default)
riwork project inspect PATH            Inspect contained repositories
riwork project list|show|use [ID]       List, inspect, or switch projects
riwork project tasks [ID]              Show tasks for a project
riwork worktree create BRANCH [--project ID] [--repo PATH] [--path PATH] [--base REF]
riwork worktree list [--project ID] [--all]
riwork worktree tasks ID                Show tasks assigned to a worktree
riwork worktree forget ID               Forget a missing worktree with no tasks or shells
riwork task add TITLE [--project ID] [--details TEXT]
riwork task list [--project ID | --worktree ID | --all]
riwork task assign WORKTREE_ID TASK_ID...    Assign tasks as one batch
riwork task unassign TASK_ID...         Remove worktree assignment
riwork task status TASK_ID todo|in_progress|done
riwork search QUERY                     Search projects, worktrees, and tasks
riwork mcp                              Serve workspace tools over MCP stdio
riwork shell create [--project ID | --worktree ID] [--command CMD]
riwork shell create [--worktree ID] --harness codex|claude [--unrestricted]
riwork shell list [--project ID | --all]
riwork shell output ID [--lines N]      Read current shell output by UUID
riwork shell send ID TEXT               Send a line to the shell
riwork shell cwd|metrics|attach|close ID
riwork orchestrator [--project ID]       Show the selected orchestrator status
riwork orchestrator create [--project ID | --cwd PATH] [--command CMD]
riwork orchestrator list [--project ID]   List global and project orchestrators
riwork orchestrator status|output|cwd|metrics|attach|close [--project ID]
riwork orchestrator send [--project ID] TEXT   Send a line to the selected orchestrator
riwork orchestrator load-skill [--project ID]   Load its workspace skill

Add --json to read commands for structured output. Project, worktree, and task
IDs accept a unique UUID prefix of at least eight characters; shell IDs need
their full UUID. Project, worktree, and task data lives in RIWORK_HOME
or ~/.local/share/riwork. Shell processes stay alive independently of the UI.
Orchestrator commands without --project use the global session; list shows all
scopes. For send, place --project before the text; use send -- TEXT to send a
global literal line beginning with --project.
Without PATH, project create requires --name and uses ~/Documents/riwork/NAME.
Explicit project paths remain relative to the current directory when needed.
";

/// Returns `false` when the arguments should launch the graphical workspace.
pub fn run_cli(args: &[String]) -> Result<bool, String> {
    if args.is_empty() {
        return Ok(false);
    }
    let mut args = args.to_vec();
    let send_command = matches!(
        args.first().map(String::as_str),
        Some("shell" | "orchestrator")
    ) && args.get(1).map(String::as_str) == Some("send");
    let json = if send_command {
        // Everything after `send` is literal input, including `--json`.
        false
    } else {
        take_flag(&mut args, "--json")
    };
    let command = args.first().cloned().unwrap_or_default();
    if !matches!(
        command.as_str(),
        "open"
            | "project"
            | "projects"
            | "worktree"
            | "worktrees"
            | "task"
            | "tasks"
            | "shell"
            | "orchestrator"
            | "search"
            | "usage"
            | "telemetry"
            | "mcp"
            | "help"
            | "-h"
            | "--help"
    ) {
        return Ok(false);
    }
    args.remove(0);
    match command.as_str() {
        "help" | "-h" | "--help" => print!("{HELP}"),
        "open" => open_command(args, json)?,
        "usage" => usage_command(args, json)?,
        "telemetry" => telemetry_command(args)?,
        "project" | "projects" => project_command(args, json)?,
        "worktree" | "worktrees" => worktree_command(args, json)?,
        "task" | "tasks" => task_command(args, json)?,
        "shell" => shell_command(args, json)?,
        "orchestrator" => orchestrator_command(args, json)?,
        "search" => search_command(args, json)?,
        "mcp" => {
            ensure_empty(&args)?;
            crate::mcp::run()?;
        }
        _ => unreachable!(),
    }
    Ok(true)
}

fn open_command(args: Vec<String>, json: bool) -> Result<(), String> {
    let selector = optional_single(args, "open [PROJECT_OR_PATH] [--json]")?;
    let state = Store::open_default()?.snapshot()?;
    let (root, project_id, project_name) = match selector.as_deref() {
        Some(selector) => match state.project(selector) {
            Ok(project) => (
                project.root.clone(),
                Some(project.id.clone()),
                Some(project.name.clone()),
            ),
            Err(error) => {
                let path = PathBuf::from(selector);
                if !path.is_dir() {
                    return Err(error);
                }
                let root = path
                    .canonicalize()
                    .map_err(|error| format!("Cannot resolve project directory: {error}"))?;
                (root, None, None)
            }
        },
        None => match state.active_project() {
            Some(project) => (
                project.root.clone(),
                Some(project.id.clone()),
                Some(project.name.clone()),
            ),
            None => (
                env::current_dir()
                    .map_err(|error| format!("Cannot read current directory: {error}"))?,
                None,
                None,
            ),
        },
    };
    if !root.is_dir() {
        return Err(format!(
            "Project directory is unavailable: {}",
            root.display()
        ));
    }
    let current_executable = env::current_exe()
        .map_err(|error| format!("Cannot locate the RiWork executable: {error}"))?;
    let adjacent_bundle = current_executable
        .parent()
        .map(|parent| parent.join("RiWork.app/Contents/MacOS/riwork"));
    let executable = adjacent_bundle
        .filter(|path| path.is_file())
        .unwrap_or(current_executable);
    let mut command = Command::new(executable);
    command
        .arg(&root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let child = command
        .spawn()
        .map_err(|error| format!("Cannot launch the project window: {error}"))?;
    if json {
        print_json(&serde_json::json!({
            "pid": child.id(),
            "project_id": project_id,
            "project": project_name,
            "root": root,
        }))?;
    } else {
        println!("Opened {} (PID {})", root.display(), child.id());
    }
    Ok(())
}

fn usage_command(mut args: Vec<String>, json: bool) -> Result<(), String> {
    let shell_id = take_option(&mut args, "--shell")?;
    ensure_empty(&args)?;
    let usage = if let Some(shell_id) = shell_id {
        let shell = SessionManager::open_default()?.get(&shell_id)?;
        if let Some(usage) = crate::usage::read_claude_usage(&shell_id)? {
            usage
        } else if matches!(shell.harness, Some(HarnessKind::Claude))
            || shell
                .command
                .as_deref()
                .is_some_and(|command| command.contains("claude"))
        {
            return Err("Claude usage is not available yet. It appears after the first response in a session launched with the Claude preset.".to_owned());
        } else if matches!(shell.harness, Some(HarnessKind::Codex))
            || shell
                .command
                .as_deref()
                .is_some_and(|command| command.contains("codex"))
        {
            crate::usage::read_codex_usage()?
        } else {
            return Err(
                "This shell has no harness usage data. Launch it with a Codex or Claude preset."
                    .to_owned(),
            );
        }
    } else {
        crate::usage::read_codex_usage()?
    };
    if json {
        print_json(&usage)?;
    } else {
        let account = usage.account_label.as_deref().unwrap_or("current account");
        println!("{} · {}", usage.provider, account);
        for window in &usage.windows {
            let remaining = (100.0 - window.used_percent).clamp(0.0, 100.0);
            let reset = window
                .resets_at
                .map(|timestamp| {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |duration| duration.as_secs());
                    let minutes = timestamp.saturating_sub(now).div_ceil(60);
                    if minutes >= 1_440 {
                        format!(
                            " · resets in {}d {}h",
                            minutes / 1_440,
                            minutes % 1_440 / 60
                        )
                    } else if minutes >= 60 {
                        format!(" · resets in {}h {}m", minutes / 60, minutes % 60)
                    } else if minutes > 0 {
                        format!(" · resets in {minutes}m")
                    } else {
                        " · reset due".to_owned()
                    }
                })
                .unwrap_or_default();
            println!("{}: {:.0}% remaining{reset}", window.label, remaining);
        }
        if usage.windows.is_empty() {
            println!("Subscription quota is not reported for this account.");
        }
        if let Some(context) = usage.context_used_percent {
            println!("Context: {context:.0}% used");
        }
        if let Some(cost) = usage.session_cost_usd {
            println!("Session cost estimate: ${cost:.2}");
        }
    }
    Ok(())
}

fn telemetry_command(mut args: Vec<String>) -> Result<(), String> {
    let provider = pop_command(&mut args, "");
    if provider != "claude" {
        return Err("Usage: riwork telemetry claude SHELL_UUID (JSON on stdin)".to_owned());
    }
    let shell_id = take_single(args, "telemetry claude SHELL_UUID")?;
    let mut input = String::new();
    io::stdin()
        .lock()
        .take(1_048_577)
        .read_to_string(&mut input)
        .map_err(|error| format!("Cannot read Claude status: {error}"))?;
    if input.len() > 1_048_576 {
        return Err("Claude status exceeds the 1 MiB limit".to_owned());
    }
    let value = serde_json::from_str(&input)
        .map_err(|error| format!("Cannot parse Claude status: {error}"))?;
    crate::usage::record_claude_status(&shell_id, &value)?;
    println!("{}", crate::usage::claude_status_text(&value));
    Ok(())
}

fn project_command(mut args: Vec<String>, json: bool) -> Result<(), String> {
    let store = Store::open_default()?;
    let operation = pop_command(&mut args, "list");
    match operation.as_str() {
        "add" | "create" => {
            let name = take_option(&mut args, "--name")?;
            let no_git = operation == "create" && take_flag(&mut args, "--no-git");
            let path = if operation == "create" && args.is_empty() {
                let name = name.as_deref().ok_or("Usage: riwork project create PATH [--name NAME] [--no-git], or riwork project create --name NAME [--no-git]")?;
                crate::paths::default_new_project_path(name)?
            } else {
                PathBuf::from(take_single(args, &format!("project {operation} PATH"))?)
            };
            let project = if operation == "create" {
                store.create_project(path, name.as_deref(), !no_git)?
            } else {
                store.add_project(path, name.as_deref())?
            };
            if json {
                print_json(&project)?;
            } else {
                println!(
                    "{}  {}  {}",
                    project.id,
                    project.name,
                    project.root.display()
                );
            }
        }
        "inspect" => {
            let path = take_single(args, "project inspect PATH")?;
            let inspection = Store::inspect_project(path)?;
            if json {
                print_json(&inspection)?;
            } else {
                println!(
                    "{} · {} repositories · Git initialization {}",
                    inspection.root.display(),
                    inspection.repository_count,
                    if inspection.can_init_git {
                        "available"
                    } else {
                        "unavailable"
                    }
                );
                for repository in inspection.repository_roots {
                    println!("  {}", repository.display());
                }
                if let Some(warning) = inspection.warning {
                    println!("{warning}");
                }
            }
        }
        "list" => {
            ensure_empty(&args)?;
            let state = store.snapshot()?;
            if json {
                print_json(&state.projects)?;
            } else {
                for project in &state.projects {
                    let active = if state.active_project_id.as_deref() == Some(&project.id) {
                        "*"
                    } else {
                        " "
                    };
                    println!(
                        "{active} {}  {}  {}",
                        project.id,
                        project.name,
                        project.root.display()
                    );
                }
            }
        }
        "show" => {
            let state = store.snapshot()?;
            let selector = optional_single(args, "project show [ID]")?;
            let project = match selector {
                Some(selector) => state.project(&selector)?,
                None => state.active_project().ok_or("No active project")?,
            };
            if json {
                print_json(project)?;
            } else {
                println!(
                    "{}  {}  {}",
                    project.id,
                    project.name,
                    project.root.display()
                );
            }
        }
        "use" => {
            let selector = take_single(args, "project use ID")?;
            let project = store.use_project(&selector)?;
            if json {
                print_json(&project)?;
            } else {
                println!("Active project: {} ({})", project.name, project.id);
            }
        }
        "tasks" => {
            let state = store.snapshot()?;
            let selector = optional_single(args, "project tasks [ID]")?;
            let project = match selector {
                Some(selector) => state.project(&selector)?,
                None => state.active_project().ok_or("No active project")?,
            };
            let tasks = state.tasks_for_project(&project.id);
            print_tasks(&tasks, json)?;
        }
        _ => return Err(format!("Unknown project command '{operation}'\n{HELP}")),
    }
    Ok(())
}

fn worktree_command(mut args: Vec<String>, json: bool) -> Result<(), String> {
    let store = Store::open_default()?;
    let operation = pop_command(&mut args, "list");
    match operation.as_str() {
        "create" => {
            let project = take_option(&mut args, "--project")?;
            let repository = take_option(&mut args, "--repo")?;
            let path = take_option(&mut args, "--path")?.map(PathBuf::from);
            let base = take_option(&mut args, "--base")?;
            let branch = take_single(args, "worktree create BRANCH")?;
            let state = store.snapshot()?;
            let project_id = project_id(&state, project.as_deref())?;
            let worktree = if let Some(repository) = repository {
                store.create_worktree_in_repo(
                    &project_id,
                    &branch,
                    path.as_deref(),
                    base.as_deref(),
                    Some(&repository),
                )?
            } else {
                store.create_worktree(&project_id, &branch, path.as_deref(), base.as_deref())?
            };
            if json {
                print_json(&worktree)?;
            } else {
                print_worktree(&worktree);
            }
        }
        "list" => {
            let project = take_option(&mut args, "--project")?;
            let all = take_flag(&mut args, "--all");
            ensure_empty(&args)?;
            if all && project.is_some() {
                return Err("Use either --all or --project".to_owned());
            }
            let before_sync = store.snapshot()?;
            if all {
                for project in &before_sync.projects {
                    store.sync_worktrees(&project.id)?;
                }
            } else if let Some(selector) = project.as_deref() {
                store.sync_worktrees(selector)?;
            } else if let Some(id) = before_sync.active_project_id.as_deref() {
                store.sync_worktrees(id)?;
            }
            let state = store.snapshot()?;
            let filter = if all {
                None
            } else if let Some(selector) = project {
                Some(state.project(&selector)?.id.as_str())
            } else {
                state.active_project_id.as_deref()
            };
            let worktrees: Vec<_> = state
                .worktrees
                .iter()
                .filter(|worktree| filter.is_none_or(|id| worktree.project_id == id))
                .collect();
            if json {
                print_json(&worktrees)?;
            } else {
                for worktree in worktrees {
                    print_worktree(worktree);
                }
            }
        }
        "show" => {
            let selector = take_single(args, "worktree show ID")?;
            let state = store.snapshot()?;
            let worktree = state.worktree(&selector)?;
            if json {
                print_json(worktree)?;
            } else {
                print_worktree(worktree);
            }
        }
        "tasks" => {
            let selector = take_single(args, "worktree tasks ID")?;
            let state = store.snapshot()?;
            let worktree = state.worktree(&selector)?;
            let tasks = state.tasks_for_worktree(&worktree.id);
            print_tasks(&tasks, json)?;
        }
        "forget" => {
            let selector = take_single(args, "worktree forget ID")?;
            let state = store.snapshot()?;
            let worktree = state.worktree(&selector)?;
            if SessionManager::open_default()?
                .list()?
                .iter()
                .any(|shell| shell.worktree_id.as_deref() == Some(worktree.id.as_str()))
            {
                return Err(format!("Shells still reference worktree {}", worktree.id));
            }
            let forgotten = store.forget_missing_worktree(&worktree.id)?;
            if json {
                print_json(&forgotten)?;
            } else {
                println!("Forgot {} ({})", forgotten.branch, forgotten.id);
            }
        }
        _ => return Err(format!("Unknown worktree command '{operation}'\n{HELP}")),
    }
    Ok(())
}

fn task_command(mut args: Vec<String>, json: bool) -> Result<(), String> {
    let store = Store::open_default()?;
    let operation = pop_command(&mut args, "list");
    match operation.as_str() {
        "add" => {
            let project = take_option(&mut args, "--project")?;
            let details = take_option(&mut args, "--details")?.unwrap_or_default();
            let title = join_nonempty(args, "task add TITLE")?;
            let state = store.snapshot()?;
            let project_id = project_id(&state, project.as_deref())?;
            let task = store.add_task(&project_id, &title, &details)?;
            print_task_result(&task, json)?;
        }
        "list" => {
            let project = take_option(&mut args, "--project")?;
            let worktree = take_option(&mut args, "--worktree")?;
            let all = take_flag(&mut args, "--all");
            ensure_empty(&args)?;
            if usize::from(project.is_some()) + usize::from(worktree.is_some()) + usize::from(all)
                > 1
            {
                return Err("Use one of --project, --worktree, or --all".to_owned());
            }
            let state = store.snapshot()?;
            let tasks: Vec<&Task> = if let Some(selector) = worktree {
                let id = &state.worktree(&selector)?.id;
                state.tasks_for_worktree(id)
            } else if let Some(selector) = project {
                let id = &state.project(&selector)?.id;
                state.tasks_for_project(id)
            } else if all {
                state.tasks.iter().collect()
            } else if let Some(id) = state.active_project_id.as_ref() {
                state.tasks_for_project(id)
            } else {
                state.tasks.iter().collect()
            };
            print_tasks(&tasks, json)?;
        }
        "show" => {
            let selector = take_single(args, "task show ID")?;
            let state = store.snapshot()?;
            print_task_result(state.task(&selector)?, json)?;
        }
        "assign" => {
            if args.len() < 2 {
                return Err("Usage: riwork task assign WORKTREE_ID TASK_ID...".to_owned());
            }
            let worktree = args.remove(0);
            let tasks = store.assign_tasks(&worktree, &args)?;
            if json {
                print_json(&tasks)?;
            } else {
                for task in &tasks {
                    print_task(task);
                }
            }
        }
        "unassign" => {
            let tasks = store.unassign_tasks(&args)?;
            if json {
                print_json(&tasks)?;
            } else {
                for task in &tasks {
                    print_task(task);
                }
            }
        }
        "status" => {
            if args.len() != 2 {
                return Err("Usage: riwork task status TASK_ID todo|in_progress|done".to_owned());
            }
            let status = TaskStatus::parse(&args[1])?;
            let task = store.set_task_status(&args[0], status)?;
            print_task_result(&task, json)?;
        }
        _ => return Err(format!("Unknown task command '{operation}'\n{HELP}")),
    }
    Ok(())
}

fn search_command(args: Vec<String>, json: bool) -> Result<(), String> {
    let query = join_nonempty(args, "search QUERY")?;
    let state = Store::open_default()?.snapshot()?;
    let hits = state.search(&query);
    if json {
        print_json(&hits)?;
    } else {
        for hit in hits {
            match hit {
                SearchHit::Project(project) => println!(
                    "project   {}  {}  {}",
                    project.id,
                    project.name,
                    project.root.display()
                ),
                SearchHit::Worktree(worktree) => {
                    print!("worktree  ");
                    print_worktree(&worktree);
                }
                SearchHit::Task(task) => {
                    print!("task      ");
                    print_task(&task);
                }
            }
        }
    }
    Ok(())
}

fn shell_command(mut args: Vec<String>, json: bool) -> Result<(), String> {
    let manager = SessionManager::open_default()?;
    let operation = pop_command(&mut args, "list");
    match operation.as_str() {
        "create" => {
            let project = take_option(&mut args, "--project")?;
            let worktree = take_option(&mut args, "--worktree")?;
            let command = take_option(&mut args, "--command")?;
            let harness = take_option(&mut args, "--harness")?
                .map(|value| match value.as_str() {
                    "codex" => Ok(HarnessKind::Codex),
                    "claude" => Ok(HarnessKind::Claude),
                    _ => Err("--harness must be codex or claude".to_owned()),
                })
                .transpose()?;
            let unrestricted = take_flag(&mut args, "--unrestricted");
            ensure_empty(&args)?;
            if harness.is_some() && command.is_some() {
                return Err("Use either --harness or --command".to_owned());
            }
            if unrestricted && harness.is_none() {
                return Err("--unrestricted requires --harness".to_owned());
            }
            let state = Store::open_default()?.snapshot()?;
            let (project_id, worktree_id, cwd) = if let Some(selector) = worktree {
                let worktree = state.worktree(&selector)?;
                if let Some(project_selector) = project {
                    let selected = state.project(&project_selector)?;
                    if selected.id != worktree.project_id {
                        return Err("Worktree belongs to another project".to_owned());
                    }
                }
                (
                    worktree.project_id.clone(),
                    Some(worktree.id.clone()),
                    worktree.path.clone(),
                )
            } else {
                let project_id = project_id(&state, project.as_deref())?;
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
            let shell = if let Some(harness) = harness {
                manager.create_harness(project_id, worktree_id, cwd, harness, unrestricted)?
            } else {
                manager.create(project_id, worktree_id, cwd, command)?
            };
            print_shell_result(&shell, json)?;
        }
        "list" => {
            let project = take_option(&mut args, "--project")?;
            let all = take_flag(&mut args, "--all");
            ensure_empty(&args)?;
            if all && project.is_some() {
                return Err("Use either --all or --project".to_owned());
            }
            let state = Store::open_default()?.snapshot()?;
            let filter = if all {
                None
            } else if let Some(selector) = project {
                Some(state.project(&selector)?.id.clone())
            } else {
                state.active_project_id.clone()
            };
            let shells: Vec<_> = manager
                .list()?
                .into_iter()
                .filter(|shell| {
                    shell.kind == ShellKind::Project
                        && filter
                            .as_ref()
                            .is_none_or(|id| shell.project_id.as_ref() == Some(id))
                })
                .collect();
            if json {
                print_json(&shells)?;
            } else {
                for shell in &shells {
                    print_shell(shell);
                }
            }
        }
        "output" => {
            let lines = take_option(&mut args, "--lines")?
                .map(|value| {
                    value
                        .parse::<usize>()
                        .map_err(|_| "--lines needs a positive integer".to_owned())
                })
                .transpose()?
                .unwrap_or(200);
            let id = take_single(args, "shell output ID [--lines N]")?;
            let output = manager.capture(&id, lines)?;
            if json {
                print_json(&json!({ "id": id, "output": output }))?;
            } else {
                print!("{output}");
            }
        }
        "send" => {
            if args.len() < 2 {
                return Err("Usage: riwork shell send ID TEXT".to_owned());
            }
            let id = args.remove(0);
            let input = args.join(" ");
            manager.send(&id, &input)?;
            if json {
                print_json(&json!({ "id": id, "sent": input }))?;
            }
        }
        "close" => {
            let id = take_single(args, "shell close ID")?;
            manager.close(&id)?;
            if json {
                print_json(&json!({ "id": id, "closed": true }))?;
            } else {
                println!("Closed {id}");
            }
        }
        "cwd" => {
            let id = take_single(args, "shell cwd ID")?;
            let cwd = manager.current_directory(&id)?;
            if json {
                print_json(&json!({ "id": id, "cwd": cwd }))?;
            } else {
                println!("{}", cwd.display());
            }
        }
        "metrics" => {
            let id = take_single(args, "shell metrics ID")?;
            let metrics = manager.metrics(&id)?;
            if json {
                print_json(&metrics)?;
            } else {
                println!(
                    "CPU {:.1}%  RAM {:.1} MiB  {} processes",
                    metrics.cpu_percent,
                    metrics.ram_bytes as f64 / 1_048_576.0,
                    metrics.process_count
                );
            }
        }
        "attach" => {
            let id = take_single(args, "shell attach ID")?;
            let command = manager.attach_command(&id)?;
            if json {
                print_json(&json!({ "id": id, "command": command }))?;
            } else {
                println!("{command}");
            }
        }
        _ => return Err(format!("Unknown shell command '{operation}'\n{HELP}")),
    }
    Ok(())
}

fn orchestrator_command(mut args: Vec<String>, json: bool) -> Result<(), String> {
    let operation = if args.first().map(String::as_str) == Some("--project") {
        "show".to_owned()
    } else {
        pop_command(&mut args, "show")
    };
    let selector = take_orchestrator_project(&mut args, &operation)?;
    if operation == "send" && args.is_empty() {
        return Err("Usage: riwork orchestrator send [--project ID] TEXT".to_owned());
    }
    let manager = SessionManager::open_default()?;
    let project = selector
        .as_deref()
        .map(|selector| {
            Store::open_default()?
                .snapshot()?
                .project(selector)
                .cloned()
        })
        .transpose()?;
    let selected = |manager: &SessionManager| match &project {
        Some(project) => manager.orchestrator_get_for_project(&project.id),
        None => manager.orchestrator_get(),
    };
    let missing = match &project {
        Some(project) => format!(
            "No project orchestrator session. Run: riwork orchestrator create --project {}",
            project.id
        ),
        None => "No global orchestrator session. Run: riwork orchestrator create".to_owned(),
    };
    match operation.as_str() {
        "create" | "start" => {
            let cwd = take_option(&mut args, "--cwd")?.map(PathBuf::from);
            let command = take_option(&mut args, "--command")?;
            ensure_empty(&args)?;
            let shell = if let Some(project) = project {
                if cwd.is_some() {
                    return Err(
                        "Use either --project or --cwd for orchestrator creation".to_owned()
                    );
                }
                manager.orchestrator_create_for_project(project.id, project.root, command)?
            } else {
                let cwd = match cwd {
                    Some(cwd) => cwd,
                    None => env::current_dir().map_err(|error| error.to_string())?,
                };
                manager.orchestrator_create(cwd, command)?
            };
            print_shell_result(&shell, json)?;
        }
        "list" => {
            ensure_empty(&args)?;
            let shells: Vec<_> = manager
                .list()?
                .into_iter()
                .filter(|shell| {
                    shell.kind == ShellKind::Orchestrator
                        && project.as_ref().is_none_or(|project| {
                            shell.project_id.as_deref() == Some(project.id.as_str())
                        })
                })
                .collect();
            if json {
                print_json(&shells)?;
            } else {
                for shell in &shells {
                    print_shell(shell);
                }
            }
        }
        "show" | "status" => {
            ensure_empty(&args)?;
            let shell = selected(&manager)?;
            if json {
                print_json(&shell)?;
            } else if let Some(shell) = shell {
                print_shell(&shell);
            } else {
                println!("{missing}");
            }
        }
        "load-skill" => {
            ensure_empty(&args)?;
            let shell = selected(&manager)?.ok_or_else(|| missing.clone())?;
            let shell = manager.load_orchestrator_skill(&shell.id)?;
            print_shell_result(&shell, json)?;
        }
        "output" | "send" | "close" | "cwd" | "metrics" | "attach" => {
            let shell = selected(&manager)?.ok_or_else(|| missing.clone())?;
            let mut shell_args = vec![operation];
            shell_args.push(shell.id);
            shell_args.extend(args);
            shell_command(shell_args, json)?;
        }
        _ => {
            return Err(format!(
                "Unknown orchestrator command '{operation}'\n{HELP}"
            ));
        }
    }
    Ok(())
}

fn take_orchestrator_project(
    args: &mut Vec<String>,
    operation: &str,
) -> Result<Option<String>, String> {
    if operation != "send" {
        return take_option(args, "--project");
    }
    // Text stays literal once it starts, including tokens that look like flags.
    if args.first().map(String::as_str) == Some("--") {
        args.remove(0);
        return Ok(None);
    }
    if args.first().map(String::as_str) != Some("--project") {
        return Ok(None);
    }
    if args.get(1).is_none_or(|value| value.starts_with("--")) {
        return Err("--project needs a value before the send text".to_owned());
    }
    args.remove(0);
    let project = args.remove(0);
    if args.first().map(String::as_str) == Some("--") {
        args.remove(0);
    }
    Ok(Some(project))
}

fn print_json(value: &impl Serialize) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|error| error.to_string())?
    );
    Ok(())
}

fn print_worktree(worktree: &Worktree) {
    let marker = if !worktree.path.exists() {
        " [missing]"
    } else if worktree.is_primary {
        " [root]"
    } else {
        ""
    };
    println!(
        "{}  {}{}  {}{}",
        worktree.id,
        worktree.branch,
        marker,
        worktree.path.display(),
        worktree
            .repository_root
            .as_ref()
            .map(|root| format!("  [repo {}]", root.display()))
            .unwrap_or_default()
    );
}

fn print_task(task: &Task) {
    let assigned = task.worktree_id.as_deref().unwrap_or("unassigned");
    println!(
        "{}  {}  {}  [{}]",
        task.id,
        task.status.as_str(),
        task.title,
        assigned
    );
}

fn print_task_result(task: &Task, json: bool) -> Result<(), String> {
    if json {
        print_json(task)
    } else {
        print_task(task);
        Ok(())
    }
}

fn print_tasks(tasks: &[&Task], json: bool) -> Result<(), String> {
    if json {
        print_json(&tasks)
    } else {
        for task in tasks {
            print_task(task);
        }
        Ok(())
    }
}

fn print_shell(shell: &ShellSession) {
    let scope = shell
        .worktree_id
        .as_deref()
        .map(|id| format!("worktree {id}"))
        .or_else(|| {
            shell
                .project_id
                .as_deref()
                .map(|id| format!("project {id}"))
        })
        .unwrap_or_else(|| "orchestrator".to_owned());
    println!(
        "{}  {:?}  {}  {}  {}",
        shell.id,
        shell.kind,
        if shell.alive { "running" } else { "exited" },
        scope,
        shell.cwd.display()
    );
}

fn print_shell_result(shell: &ShellSession, json: bool) -> Result<(), String> {
    if json {
        print_json(shell)
    } else {
        print_shell(shell);
        Ok(())
    }
}

fn project_id(state: &State, selector: Option<&str>) -> Result<String, String> {
    match selector {
        Some(selector) => Ok(state.project(selector)?.id.clone()),
        None => state
            .active_project_id
            .clone()
            .ok_or_else(|| "No active project. Run: riwork project add PATH".to_owned()),
    }
}

fn take_option(args: &mut Vec<String>, option: &str) -> Result<Option<String>, String> {
    let mut result = None;
    while let Some(index) = args.iter().position(|arg| arg == option) {
        if index + 1 >= args.len() || args[index + 1].starts_with("--") {
            return Err(format!("{option} needs a value"));
        }
        args.remove(index);
        let value = args.remove(index);
        if result.replace(value).is_some() {
            return Err(format!("{option} can only be given once"));
        }
    }
    Ok(result)
}

fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    if let Some(index) = args.iter().position(|arg| arg == flag) {
        args.remove(index);
        true
    } else {
        false
    }
}

fn pop_command(args: &mut Vec<String>, default: &str) -> String {
    if args.is_empty() {
        default.to_owned()
    } else {
        args.remove(0)
    }
}

fn take_single(args: Vec<String>, usage: &str) -> Result<String, String> {
    if args.len() == 1 && !args[0].starts_with("--") {
        Ok(args.into_iter().next().unwrap())
    } else {
        Err(format!("Usage: riwork {usage}"))
    }
}

fn optional_single(args: Vec<String>, usage: &str) -> Result<Option<String>, String> {
    if args.is_empty() {
        Ok(None)
    } else {
        take_single(args, usage).map(Some)
    }
}

fn join_nonempty(args: Vec<String>, usage: &str) -> Result<String, String> {
    if args.is_empty() || args.iter().any(|arg| arg.starts_with("--")) {
        Err(format!("Usage: riwork {usage}"))
    } else {
        Ok(args.join(" "))
    }
}

fn ensure_empty(args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(format!("Unexpected arguments: {}", args.join(" ")))
    }
}

#[cfg(test)]
mod tests {
    use super::take_orchestrator_project;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn scoped_send_preserves_flags_in_literal_task_text() {
        let mut input = args(&[
            "--project",
            "alpha",
            "--",
            "run",
            "--project",
            "literal",
            "--json",
            "--",
        ]);
        assert_eq!(
            take_orchestrator_project(&mut input, "send").unwrap(),
            Some("alpha".to_owned())
        );
        assert_eq!(
            input,
            args(&["run", "--project", "literal", "--json", "--"])
        );
    }

    #[test]
    fn global_send_scope_like_text_stays_literal_after_text_or_delimiter() {
        for (original, expected) in [
            (
                args(&["write", "--project", "literal", "--json"]),
                args(&["write", "--project", "literal", "--json"]),
            ),
            (
                args(&["--", "--project", "literal", "--json"]),
                args(&["--project", "literal", "--json"]),
            ),
        ] {
            let mut input = original;
            assert_eq!(take_orchestrator_project(&mut input, "send").unwrap(), None);
            assert_eq!(input, expected);
        }
    }

    #[test]
    fn missing_send_scope_value_and_duplicate_read_scopes_fail() {
        for values in [&["--project"][..], &["--project", "--"][..]] {
            assert!(take_orchestrator_project(&mut args(values), "send").is_err());
        }
        assert!(
            take_orchestrator_project(
                &mut args(&["--project", "alpha", "--project", "beta"]),
                "status"
            )
            .is_err()
        );
    }
}
