//! The command line view of the same data the GPUI workspace renders.

use std::{
    env, fmt,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use serde::Serialize;
use serde_json::json;

use crate::{
    cua::{CuaManager, CuaStatus},
    schedule_service::{
        CreateRequest, RepeatChange, ScheduleError, ScheduleKey, ScheduleService, ScopeInput,
        UpdateRequest,
    },
    sessions::{HarnessKind, SessionManager, ShellKind, ShellSession},
    store::{SearchHit, State, Store, Task, TaskStatus, Worktree},
};

// std's print macros panic when stdout is a closed pipe (`riwork shell output
// ID | head`). Everything in this module prints through these instead, and the
// command still finishes with its real exit status.
static STDOUT_CLOSED: AtomicBool = AtomicBool::new(false);

fn write_stdout(args: fmt::Arguments<'_>) {
    if STDOUT_CLOSED.load(Ordering::Relaxed) {
        return;
    }
    if let Err(error) = io::stdout().lock().write_fmt(args) {
        STDOUT_CLOSED.store(true, Ordering::Relaxed);
        if error.kind() != io::ErrorKind::BrokenPipe {
            eprintln!("riwork: cannot write output: {error}");
        }
    }
}

macro_rules! print {
    ($($arg:tt)*) => { write_stdout(format_args!($($arg)*)) };
}

macro_rules! println {
    () => { write_stdout(format_args!("\n")) };
    ($($arg:tt)*) => { write_stdout(format_args!("{}\n", format_args!($($arg)*))) };
}

const HELP: &str = "\
riwork [PROJECT_PATH]                  Open the GPUI workspace
riwork open [PROJECT_OR_PATH]           Open an independent project window
riwork reload [--all] [--session] [--shell ID]   Reload windows; optionally resume a Codex session
riwork update [--source PATH] [--release | --debug] [--no-reload]   Build release by default, install, and reload all windows
riwork instances                        List running RiWork apps and their windows
riwork usage [--shell ID]               Read harness subscription usage
riwork setup                            Install and start Cua.ai Driver
riwork cua setup|status|permissions      Manage native computer use
riwork cua mcp                          Serve Cua.ai Driver over MCP stdio
riwork cua harness codex|claude|grok -- ARG...   Start a harness with shared Cua
riwork import orca [--preview]          Import local Orca projects and worktrees once
riwork project add PATH [--name NAME]   Register a project and its root worktree
riwork project create [PATH] [--name NAME] [--no-git]   Create a project (Git by default)
riwork project inspect PATH            Inspect contained repositories
riwork project list|show|use [ID]       List, inspect, or switch projects
riwork project update ID [--name NAME] [--folder ID | --ungrouped]   Edit project display metadata
riwork project folder create NAME [--parent ID]   Create a virtual folder or subfolder
riwork project folder move ID (--parent ID | --root)   Move a virtual folder
riwork project folder list|rename|remove   Manage virtual project folders
riwork project tasks [ID]              Show tasks for a project
riwork worktree create BRANCH [--project ID] [--repo PATH] [--path PATH] [--base REF]   --base only applies to a new branch
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
riwork version | --version              Print the RiWork version
riwork remote pair|revoke|devices|start|relay   Encrypted mobile access (standalone binary)
riwork remote --help                    Pairing, relay and connector command options
riwork shell create [--project ID | --worktree ID] [--command CMD]
riwork shell create [--worktree ID] --harness codex|claude|grok [--unrestricted]
riwork shell list [--project ID | --all]
riwork shell output ID [--lines N]      Read current shell output by UUID
riwork shell send ID TEXT               Paste a complete line and submit once
riwork shell resize ID --columns N --rows N --owner UUID --lease UUID
riwork shell resize-clear ID --owner UUID --lease UUID   Restore desktop sizing
riwork shell cwd|metrics|attach|close ID
riwork orchestrator [--project ID]       Show the selected orchestrator status
riwork orchestrator create [--project ID | --cwd PATH] [--command CMD]
riwork orchestrator list [--project ID]   List global and project orchestrators
riwork orchestrator status|output|cwd|metrics|attach|close [--project ID]
riwork orchestrator send [--project ID] TEXT   Send a line to the selected orchestrator
riwork orchestrator load-skill [--project ID]   Load its workspace skill
riwork schedule list [--scope app|project|workspace] [--project UUID] [--worktree UUID]
riwork schedule show SCHEDULE_UUID
riwork schedule create --scope SCOPE [--project UUID] [--worktree UUID] --shell UUID --title TITLE --prompt TEXT --at RFC3339 [--every-minutes N]
riwork schedule update SCHEDULE_UUID --revision N --scope SCOPE [--project UUID] [--worktree UUID] --shell UUID --at RFC3339 [--title TITLE] [--prompt TEXT] [--every-minutes N | --once]
riwork schedule pause|resume|delete SCHEDULE_UUID --revision N --scope SCOPE [--project UUID] [--worktree UUID] --shell UUID
riwork schedule help                    Show schedule contract and examples

Add --json to read commands for structured output. Project, worktree, and task
IDs accept a unique UUID prefix of at least eight characters; shell IDs need
their full UUID. Project, worktree, and task data lives in RIWORK_HOME
or ~/.local/share/riwork. Shell processes stay alive independently of the UI.
Only no arguments or one existing project directory open the workspace; any
other unknown command or option exits with an error.
worktree create --base REF only chooses the start point of a new branch. If
BRANCH already exists, it is checked out as is and --base is ignored.
shell create --project ID --worktree SELECTOR looks SELECTOR up in that project
first, so a branch name shared by several projects is not ambiguous.
Orchestrator commands without --project use the global session; list shows all
scopes. For send, place --project before the text; use send -- TEXT to send a
global literal line beginning with --project.
Without PATH, project create requires --name and uses ~/Documents/riwork/NAME.
Explicit project paths remain relative to the current directory when needed.
";

const SCHEDULE_HELP: &str = "\
Schedule management (add --json to any command for structured stdout):
  riwork schedule list [--scope app|project|workspace] [--project UUID] [--worktree UUID]
  riwork schedule show SCHEDULE_UUID
  riwork schedule create --scope app|project|workspace --shell UUID --title TITLE --prompt TEXT --at RFC3339 [--every-minutes N]
  riwork schedule update SCHEDULE_UUID --revision N --scope SCOPE --shell UUID --at RFC3339 [--title TITLE] [--prompt TEXT] [--every-minutes N | --once]
  riwork schedule pause|resume|delete SCHEDULE_UUID --revision N --scope SCOPE --shell UUID

For project scope add --project PROJECT_UUID. For workspace scope add both
--project PROJECT_UUID and --worktree WORKTREE_UUID. All UUIDs must be full,
canonical values. Create binds an existing live Codex/Claude shell in that
scope; edits keep its pinned identity. First run must be a future RFC 3339
time with seconds and timezone, such as 2026-10-05T09:00:00+02:00. Recurrence
is elapsed whole minutes from 5 to 525600; omit it for one run. Update needs
a new future --at, retains recurrence unless changed, and may rearm a reviewed
failed/uncertain schedule. Read its latest revision first. The desktop app
must be open to dispatch; these commands never launch a session or agent.

Examples (replace UUIDs and future time with values from your own fixture):
  riwork schedule create --scope app --shell SHELL_UUID --title Check --prompt 'Review status' --at 2026-10-05T09:00:00+02:00 --json
  riwork schedule create --scope project --project PROJECT_UUID --shell SHELL_UUID --title Check --prompt 'Review status' --at 2026-10-05T09:00:00+02:00
  riwork schedule create --scope workspace --project PROJECT_UUID --worktree WORKTREE_UUID --shell SHELL_UUID --title Check --prompt 'Review status' --at 2026-10-05T09:00:00+02:00 --every-minutes 60
  riwork schedule pause SCHEDULE_UUID --revision 1 --scope workspace --project PROJECT_UUID --worktree WORKTREE_UUID --shell SHELL_UUID --json
";

/// Returns `false` when the arguments should launch the graphical workspace.
/// Anything else that is not a command is an error, so a typo or a headless
/// agent's `riwork --version` never starts (and registers) a GUI instance.
pub fn run_cli(args: &[String]) -> Result<bool, String> {
    if args.is_empty() {
        return Ok(false);
    }
    if args.first().map(String::as_str) == Some("remote") {
        crate::remote_cli::forward(&args[1..])?;
        return Ok(true);
    }
    let original = args;
    let mut args = args.to_vec();
    let literal_arguments = matches!(
        args.first().map(String::as_str),
        Some("shell" | "orchestrator")
    ) && args.get(1).map(String::as_str) == Some("send")
        || args.first().map(String::as_str) == Some("cua")
            && args.get(1).map(String::as_str) == Some("harness")
        || matches!(
            args.first().map(String::as_str),
            Some("agent-notify" | "agent-hook")
        );
    let json = if literal_arguments {
        // Forwarded harness options and sent shell input must remain literal.
        false
    } else {
        take_flag(&mut args, "--json")
    };
    let command = args.first().cloned().unwrap_or_default();
    if !matches!(
        command.as_str(),
        "open"
            | "reload"
            | "update"
            | "instances"
            | "reload-session-worker"
            | "project"
            | "projects"
            | "worktree"
            | "worktrees"
            | "task"
            | "tasks"
            | "shell"
            | "orchestrator"
            | "schedule"
            | "search"
            | "usage"
            | "telemetry"
            | "agent-notify"
            | "agent-hook"
            | "mcp"
            | "setup"
            | "cua"
            | "import"
            | "help"
            | "-h"
            | "--help"
            | "version"
            | "--version"
            | "-V"
    ) {
        return if opens_workspace(original) {
            Ok(false)
        } else {
            // Name the offending token, not a leading --json that was consumed.
            Err(unknown_invocation(if args.is_empty() {
                original
            } else {
                args.as_slice()
            }))
        };
    }
    args.remove(0);
    match command.as_str() {
        "help" | "-h" | "--help" => print!("{HELP}"),
        "version" | "--version" | "-V" => {
            ensure_empty(&args)?;
            if json {
                print_json(&json!({"version": env!("CARGO_PKG_VERSION")}))?;
            } else {
                println!("riwork {}", env!("CARGO_PKG_VERSION"));
            }
        }
        "open" => open_command(args, json)?,
        "reload" => reload_command(args, json)?,
        "update" => update_command(args, json)?,
        "instances" => {
            ensure_empty(&args)?;
            let instances = crate::runtime::RuntimeManager::open_default()?.instances()?;
            if json {
                print_json(&instances)?;
            } else {
                for instance in instances {
                    println!(
                        "{}  {} windows  {}",
                        instance.pid,
                        instance.windows.len(),
                        instance.executable.display()
                    );
                }
            }
        }
        "reload-session-worker" => {
            let path = take_single(args, "reload-session-worker REQUEST")?;
            crate::session_reload::run_reload_worker(std::path::Path::new(&path))?;
        }
        "usage" => usage_command(args, json)?,
        "setup" => {
            ensure_empty(&args)?;
            print_cua_status(&CuaManager::open_default()?.setup()?, json)?;
        }
        "cua" => cua_command(args, json)?,
        "import" => import_command(args, json)?,
        "telemetry" => telemetry_command(args)?,
        "agent-hook" => agent_hook_command(args, io::stdin().lock())?,
        "agent-notify" => {
            if args.len() != 3 {
                return Err("Usage: riwork agent-notify STATE_HOME SHELL_UUID JSON".to_owned());
            }
            crate::activity::record_codex_notification(
                std::path::Path::new(&args[0]),
                &args[1],
                &args[2],
            )?;
        }
        "project" | "projects" => project_command(args, json)?,
        "worktree" | "worktrees" => worktree_command(args, json)?,
        "task" | "tasks" => task_command(args, json)?,
        "shell" => shell_command(args, json)?,
        "orchestrator" => orchestrator_command(args, json)?,
        "schedule" => schedule_command(args, json)?,
        "search" => search_command(args, json)?,
        "mcp" => {
            ensure_empty(&args)?;
            crate::mcp::run()?;
        }
        _ => unreachable!(),
    }
    Ok(true)
}

/// The GUI's own startup forms: no arguments, or one project directory, which
/// is how `riwork open`, reload restore, and the shell launchers start it.
/// Older macOS may add a `-psn_` process serial number when launching a bundle.
fn opens_workspace(args: &[String]) -> bool {
    let mut args = args.iter().filter(|arg| !arg.starts_with("-psn_"));
    match (args.next(), args.next()) {
        (None, _) => true,
        (Some(path), None) => Path::new(path).is_dir(),
        _ => false,
    }
}

fn unknown_invocation(args: &[String]) -> String {
    let first = args.first().map(String::as_str).unwrap_or_default();
    let problem = if first.starts_with('-') {
        format!("Unknown option '{first}'")
    } else {
        format!("'{first}' is not a riwork command or an existing project directory")
    };
    format!(
        "{problem}\nUsage: riwork [PROJECT_DIR] | riwork COMMAND [ARGS...]\nRun `riwork help` for all commands."
    )
}

fn import_command(mut args: Vec<String>, json: bool) -> Result<(), String> {
    let preview_only = take_flag(&mut args, "--preview");
    if args.first().map(String::as_str) != Some("orca") {
        return Err("Usage: riwork import orca [--preview] [--json]".to_owned());
    }
    args.remove(0);
    ensure_empty(&args)?;
    let manager = crate::orca_import::ImportManager::open_default()?;
    let preview = manager.inspect()?;
    if preview_only {
        if json {
            print_json(&preview)?;
        } else if let Some(receipt) = &preview.already_imported {
            println!(
                "Orca import already completed: {} projects and {} worktrees.",
                receipt.project_count, receipt.worktree_count
            );
        } else {
            println!(
                "Orca import will add {} projects and {} worktrees.",
                preview.project_count, preview.worktree_count
            );
            for warning in &preview.warnings {
                println!("{warning}");
            }
        }
        return Ok(());
    }
    let receipt = manager.import(&preview)?;
    if json {
        print_json(&receipt)?;
    } else {
        println!(
            "Orca import completed: {} projects and {} worktrees.",
            receipt.project_count, receipt.worktree_count
        );
        for warning in &preview.warnings {
            println!("{warning}");
        }
    }
    Ok(())
}

fn gui_executable() -> Result<PathBuf, String> {
    if let Some(executable) =
        crate::runtime::RuntimeManager::open_default()?.installed_executable()?
    {
        return Ok(executable);
    }
    let executable =
        env::current_exe().map_err(|error| format!("Cannot locate RiWork: {error}"))?;
    Ok(executable
        .parent()
        .map(|parent| parent.join("RiWork.app/Contents/MacOS/riwork"))
        .filter(|path| path.is_file())
        .unwrap_or(executable))
}

fn reload_command(mut args: Vec<String>, json: bool) -> Result<(), String> {
    take_flag(&mut args, "--all");
    let session = take_flag(&mut args, "--session");
    let shell = take_option(&mut args, "--shell")?;
    ensure_empty(&args)?;
    if shell.is_some() && !session {
        return Err("--shell requires --session".to_owned());
    }
    if session {
        crate::session_reload::validate_reload_for_shell(shell.as_deref())?;
    }
    let executable = gui_executable()?;
    let manager = crate::runtime::RuntimeManager::open_default()?;
    let report = manager.reload_all(&executable)?;
    let report = manager.wait_for_reload(report, Duration::from_secs(30))?;
    if report.failed > 0 || report.pending > 0 {
        if json {
            print_json(&json!({"reload":report,"session":null}))?;
        }
        return Err(format!(
            "Reload incomplete: {} restored, {} failed, {} pending. Existing apps remain open where restoration failed.",
            report.reloaded, report.failed, report.pending
        ));
    }
    let session = if session {
        Some(crate::session_reload::queue_reload_for_shell(
            &executable,
            shell.as_deref(),
        )?)
    } else {
        None
    };
    if json {
        print_json(&json!({"reload":report,"session":session}))?;
    } else {
        print_reload_report(&report);
        if session.is_some() {
            println!(
                "This Codex conversation will resume with Cua after its active turn finishes."
            );
        }
    }
    Ok(())
}

fn update_command(mut args: Vec<String>, json: bool) -> Result<(), String> {
    let source = take_option(&mut args, "--source")?.map(PathBuf::from);
    let profile = take_update_profile(&mut args)?;
    let no_reload = take_flag(&mut args, "--no-reload");
    ensure_empty(&args)?;
    let source = crate::update::resolve_source(source.as_deref())?;
    eprintln!("Building RiWork from {}…", source.display());
    let build = crate::update::build_update(&source, profile)?;
    let reload_result = (|| {
        let manager = crate::runtime::RuntimeManager::open_default()?;
        let executable = build.bundle.join("Contents/MacOS/riwork");
        manager.record_installed_build(&executable)?;
        if no_reload {
            return Ok(None);
        }
        let report = manager.reload_all(&executable)?;
        let report = manager.wait_for_reload(report, Duration::from_secs(30))?;
        if report.failed > 0 || report.pending > 0 {
            return Err(format!(
                "{} restored, {} failed, {} pending",
                report.reloaded, report.failed, report.pending
            ));
        }
        Ok(Some(report))
    })();
    let reload = match reload_result {
        Ok(reload) => reload,
        Err(error) => {
            if json {
                print_json(&json!({"build":build,"reload_error":error}))?;
            }
            return Err(format!(
                "Installed {}, but reload failed: {error}. Retry `riwork reload`. Build log: {}",
                build.bundle.display(),
                build.log_path.display()
            ));
        }
    };
    if json {
        print_json(&json!({"build":build,"reload":reload}))?;
    } else {
        println!("Installed {}", build.bundle.display());
        if let Some(reload) = reload {
            print_reload_report(&reload);
        }
    }
    Ok(())
}

fn take_update_profile(args: &mut Vec<String>) -> Result<Option<&'static str>, String> {
    match (take_flag(args, "--release"), take_flag(args, "--debug")) {
        (true, true) => {
            Err("Choose either --release or --debug for an update, not both.".to_owned())
        }
        (true, false) => Ok(Some("release")),
        (false, true) => Ok(Some("debug")),
        (false, false) => Ok(None),
    }
}

fn print_reload_report(report: &crate::runtime::ReloadReport) {
    if report.instances.is_empty() {
        println!("No registered RiWork apps are open.");
    } else {
        let windows: usize = report
            .instances
            .iter()
            .map(|instance| instance.window_count)
            .sum();
        println!(
            "Reloaded {} RiWork apps ({} windows). Running shells and agents were preserved.",
            report.reloaded, windows
        );
    }
}

fn cua_command(mut args: Vec<String>, json: bool) -> Result<(), String> {
    let command = args.first().cloned().unwrap_or_else(|| "status".to_owned());
    if !args.is_empty() {
        args.remove(0);
    }
    if command == "harness" {
        let harness = match args.first().map(String::as_str) {
            Some("codex") => HarnessKind::Codex,
            Some("claude") => HarnessKind::Claude,
            Some("grok") => HarnessKind::Grok,
            _ => return Err("Usage: riwork cua harness codex|claude|grok -- ARG...".to_owned()),
        };
        args.remove(0);
        if args.first().map(String::as_str) == Some("--") {
            args.remove(0);
        }
        return crate::sessions::run_cua_harness(harness, &args);
    }
    ensure_empty(&args)?;
    let manager = CuaManager::open_default()?;
    match command.as_str() {
        "setup" => print_cua_status(&manager.setup()?, json),
        "status" => print_cua_status(&manager.status()?, json),
        "permissions" => print_cua_status(&manager.request_permissions()?, json),
        "mcp" => manager.run_mcp(),
        _ => Err("Usage: riwork cua setup|status|permissions|mcp [--json]".to_owned()),
    }
}

fn print_cua_status(status: &CuaStatus, json: bool) -> Result<(), String> {
    if json {
        print_json(status)?;
    } else {
        if let Some(version) = &status.version {
            println!("{version}");
        }
        if let Some(path) = &status.driver_path {
            println!("Driver: {}", path.display());
        }
        println!("{}", status.message);
    }
    Ok(())
}

fn gui_command(executable: PathBuf, root: &std::path::Path) -> Command {
    let mut command = Command::new(executable);
    command
        .arg(root)
        .env_remove("RIWORK_RESTORE_TICKET")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Run from an account-bound session, `open` must not turn that session's
    // Codex home into the new window's system default.
    crate::codex_accounts::scrub_injected_environment(&mut command);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
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
    let child = gui_command(gui_executable()?, &root)
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
        if shell.harness == Some(HarnessKind::Codex) {
            crate::usage::read_codex_usage_at(frozen_codex_usage_home(&shell)?)?
        } else if shell.harness == Some(HarnessKind::Grok) {
            // RiWork cannot read Grok's quota yet. Report that as unknown in the
            // usual shape rather than failing a check-in loop over many shells.
            grok_usage_unknown()
        } else if let Some(usage) = crate::usage::read_claude_usage(&shell_id)? {
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
            crate::usage::read_codex_usage_at(frozen_codex_usage_home(&shell)?)?
        } else {
            return Err(
                "This shell has no harness usage data. Launch it with a Codex or Claude preset."
                    .to_owned(),
            );
        }
    } else {
        let manager = SessionManager::open_default()?;
        let settings = crate::settings::SettingsStore::open_default()?.load()?;
        let binding = crate::codex_accounts::resolve_launch_binding(
            manager.state_home(),
            settings.selected_codex_account.as_deref(),
        )?;
        crate::usage::read_codex_usage_at(&binding.home)?
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

/// Grok quota is only available through its own `grok usage` command.
fn grok_usage_unknown() -> crate::usage::ProviderUsage {
    crate::usage::ProviderUsage {
        provider: "grok".to_owned(),
        windows: Vec::new(),
        updated_at_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs()),
        account_label: Some("unknown".to_owned()),
        context_used_percent: None,
        session_cost_usd: None,
    }
}

fn frozen_codex_usage_home(shell: &ShellSession) -> Result<&std::path::Path, String> {
    let home = shell.codex_home.as_deref().ok_or_else(|| {
        "This Codex session's account is unknown; its usage cannot be attributed safely.".to_owned()
    })?;
    if !home.is_absolute() || !home.is_dir() {
        return Err("This Codex session's account home is unavailable.".to_owned());
    }
    Ok(home)
}

/// Claude treats exit 2 from UserPromptSubmit/Stop as a blocking decision.
/// Delivery is optional: every input, I/O, and queue failure must stay silent
/// and successful so notifications cannot alter the agent's work.
fn agent_hook_command(args: Vec<String>, input: impl io::Read) -> Result<(), String> {
    if args.len() != 3 || args[0] != "claude" {
        return Ok(());
    }
    let mut body = String::new();
    if input.take(1_048_577).read_to_string(&mut body).is_err() || body.len() > 1_048_576 {
        return Ok(());
    }
    let _ = crate::agent_hooks::record_claude_hook(std::path::Path::new(&args[1]), &args[2], &body);
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
        "update" => {
            let name = take_option(&mut args, "--name")?;
            let folder = take_option(&mut args, "--folder")?;
            let ungrouped = take_flag(&mut args, "--ungrouped");
            if folder.is_some() && ungrouped {
                return Err("Use --folder or --ungrouped, not both".to_owned());
            }
            let selector = take_single(
                args,
                "project update ID [--name NAME] [--folder ID | --ungrouped]",
            )?;
            // Each edit is one transaction over the fields it changes, so an
            // unrelated concurrent change (a GUI folder move, a rename) survives.
            let moves = folder.is_some() || ungrouped;
            let project = match (name.as_deref(), moves) {
                (None, false) => {
                    return Err(
                        "Nothing to update; give --name, --folder, or --ungrouped".to_owned()
                    );
                }
                (None, true) => store.move_project_to_folder(&selector, folder.as_deref())?,
                (Some(name), false) => store.rename_project(&selector, name)?,
                (Some(name), true) => {
                    store.update_project_metadata(&selector, name, folder.as_deref())?
                }
            };
            if json {
                print_json(&project)?;
            } else {
                println!("Updated project: {} ({})", project.name, project.id);
            }
        }
        "folder" | "folders" => project_folder_command(&store, args, json)?,
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

fn project_folder_command(store: &Store, mut args: Vec<String>, json: bool) -> Result<(), String> {
    let operation = pop_command(&mut args, "list");
    match operation.as_str() {
        "list" => {
            ensure_empty(&args)?;
            let state = store.snapshot()?;
            if json {
                print_json(&state.project_folders)?;
            } else {
                for folder in &state.project_folders {
                    let count = state
                        .projects
                        .iter()
                        .filter(|project| project.folder_id.as_deref() == Some(&folder.id))
                        .count();
                    println!(
                        "{}  {}  {} projects",
                        folder.id,
                        state.project_folder_path(&folder.id),
                        count
                    );
                }
            }
        }
        "create" | "rename" => {
            let folder = if operation == "create" {
                let parent = take_option(&mut args, "--parent")?;
                let name = take_single(args, "project folder create NAME [--parent ID]")?;
                if let Some(parent) = parent {
                    store.create_project_folder_in(&name, Some(&parent))?
                } else {
                    store.create_project_folder(&name)?
                }
            } else {
                if args.len() != 2 {
                    return Err("Usage: riwork project folder rename ID NAME".to_owned());
                }
                store.rename_project_folder(&args[0], &args[1])?
            };
            if json {
                print_json(&folder)?;
            } else {
                let state = store.snapshot()?;
                println!("{}  {}", folder.id, state.project_folder_path(&folder.id));
            }
        }
        "move" => {
            let parent = take_option(&mut args, "--parent")?;
            let root = take_flag(&mut args, "--root");
            if parent.is_some() == root {
                return Err("Choose --parent ID or --root".to_owned());
            }
            let selector = take_single(args, "project folder move ID (--parent ID | --root)")?;
            let folder = store.move_project_folder(&selector, parent.as_deref())?;
            if json {
                print_json(&folder)?;
            } else {
                let state = store.snapshot()?;
                println!("{}  {}", folder.id, state.project_folder_path(&folder.id));
            }
        }
        "remove" => {
            let selector = take_single(args, "project folder remove ID")?;
            store.remove_project_folder(&selector)?;
            if json {
                print_json(&json!({"removed": selector}))?;
            } else {
                println!(
                    "Removed project folder; its projects and subfolders moved to its parent."
                );
            }
        }
        _ => {
            return Err(format!(
                "Unknown project folder command '{operation}'; use list, create, rename, move, or remove"
            ));
        }
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
                    "grok" => Ok(HarnessKind::Grok),
                    _ => Err("--harness must be codex, claude, or grok".to_owned()),
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
                // A named project scopes the selector, so a branch shared by
                // many projects (`main`) is not ambiguous.
                let worktree = match project.as_deref() {
                    Some(project_selector) => {
                        let selected = state.project(project_selector)?;
                        let worktree = state.worktree_in_project(&selected.id, &selector)?;
                        if selected.id != worktree.project_id {
                            return Err("Worktree belongs to another project".to_owned());
                        }
                        worktree
                    }
                    None => state.worktree(&selector)?,
                };
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
        "resize" | "resize-clear" | "viewport-watch" => {
            let owner = take_option(&mut args, "--owner")?.ok_or("--owner UUID is required")?;
            let lease = take_option(&mut args, "--lease")?.ok_or("--lease UUID is required")?;
            let columns = take_option(&mut args, "--columns")?;
            let rows = take_option(&mut args, "--rows")?;
            let id = take_single(
                args,
                "shell resize|resize-clear ID --owner UUID --lease UUID",
            )?;
            if operation == "resize" {
                let columns = columns
                    .ok_or("--columns required")?
                    .parse::<u32>()
                    .map_err(|_| "--columns must be an integer")?;
                let rows = rows
                    .ok_or("--rows required")?
                    .parse::<u32>()
                    .map_err(|_| "--rows must be an integer")?;
                let size = manager.resize_viewport(&id, &owner, &lease, columns, rows)?;
                if json {
                    print_json(&size)?;
                } else {
                    println!("{} {}x{}", size.shell_id, size.columns, size.rows);
                }
            } else {
                if columns.is_some() || rows.is_some() {
                    return Err("dimensions only apply to resize".into());
                }
                if operation == "viewport-watch" {
                    manager.watch_viewport(&id, &owner, &lease)?;
                } else {
                    manager.clear_viewport(&id, &owner, &lease)?;
                    if json {
                        print_json(&json!({"shell_id":id,"status":"cleared"}))?;
                    } else {
                        println!("Restored desktop sizing for {id}");
                    }
                }
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

fn schedule_command(mut args: Vec<String>, json: bool) -> Result<(), String> {
    let operation = pop_command(&mut args, "list");
    if matches!(operation.as_str(), "help" | "-h" | "--help") {
        ensure_empty(&args)?;
        print!("{SCHEDULE_HELP}");
        return Ok(());
    }
    let result = schedule_command_inner(&operation, args);
    match result {
        Ok(value) => {
            if json || operation == "show" {
                print_json(&value)?;
            } else if operation == "list" {
                for schedule in value["items"].as_array().into_iter().flatten() {
                    print_schedule_line(schedule);
                }
            } else if operation == "delete" {
                println!(
                    "Deleted {}",
                    value["schedule"]["id"].as_str().unwrap_or("schedule")
                );
            } else {
                print_schedule_line(&value["schedule"]);
            }
            Ok(())
        }
        Err(error) if json => {
            print_json(&json!({"error":error}))?;
            // The empty error suppresses a second unstructured stderr message.
            Err(String::new())
        }
        Err(error) => Err(error.message),
    }
}

fn schedule_command_inner(
    operation: &str,
    mut args: Vec<String>,
) -> Result<serde_json::Value, ScheduleError> {
    let option =
        |args: &mut Vec<String>, name: &str| take_option(args, name).map_err(schedule_cli_error);
    let required = |value: Option<String>, name: &str| {
        value.ok_or_else(|| schedule_cli_error(format!("{name} is required")))
    };
    let scope = |args: &mut Vec<String>| -> Result<ScopeInput, ScheduleError> {
        Ok(ScopeInput {
            scope: required(option(args, "--scope")?, "--scope")?,
            project_id: option(args, "--project")?,
            worktree_id: option(args, "--worktree")?,
        })
    };
    let key = |args: &mut Vec<String>| -> Result<ScheduleKey, ScheduleError> {
        let identity = scope(args)?;
        let shell_id = required(option(args, "--shell")?, "--shell")?;
        let revision = required(option(args, "--revision")?, "--revision")?
            .parse::<u64>()
            .map_err(|_| schedule_cli_error("--revision must be a positive integer"))?;
        let id = schedule_single(args, "SCHEDULE_UUID")?;
        Ok(ScheduleKey {
            id,
            revision,
            scope: identity,
            shell_id,
        })
    };
    let service = ScheduleService::open_default()?;
    match operation {
        "list" => {
            let scope = option(&mut args, "--scope")?;
            let project_id = option(&mut args, "--project")?;
            let worktree_id = option(&mut args, "--worktree")?;
            schedule_empty(&args)?;
            let filter = match scope {
                Some(scope) => Some(ScopeInput {
                    scope,
                    project_id,
                    worktree_id,
                }),
                None if project_id.is_none() && worktree_id.is_none() => None,
                None => {
                    return Err(schedule_cli_error(
                        "--scope is required with --project or --worktree",
                    ));
                }
            };
            Ok(json!({"items":service.list(filter.as_ref())?}))
        }
        "show" => {
            let id = schedule_single(&mut args, "SCHEDULE_UUID")?;
            Ok(json!({"schedule":service.show(&id)?}))
        }
        "create" => {
            let identity = scope(&mut args)?;
            let shell_id = required(option(&mut args, "--shell")?, "--shell")?;
            let title = required(option(&mut args, "--title")?, "--title")?;
            let prompt = required(option(&mut args, "--prompt")?, "--prompt")?;
            let at = required(option(&mut args, "--at")?, "--at")?;
            let every_minutes = schedule_minutes(option(&mut args, "--every-minutes")?)?;
            schedule_empty(&args)?;
            Ok(json!({"schedule":service.create(CreateRequest {
                scope: identity, shell_id, title, prompt, at, every_minutes,
            })?}))
        }
        "update" => {
            // Parse optional edits before the positional schedule ID.
            let title = option(&mut args, "--title")?;
            let prompt = option(&mut args, "--prompt")?;
            let at = required(option(&mut args, "--at")?, "--at")?;
            let every_minutes = schedule_minutes(option(&mut args, "--every-minutes")?)?;
            let once = take_flag(&mut args, "--once");
            if once && every_minutes.is_some() {
                return Err(schedule_cli_error("Choose --once or --every-minutes"));
            }
            let repeat = if once {
                RepeatChange::Once
            } else if let Some(minutes) = every_minutes {
                RepeatChange::EveryMinutes(minutes)
            } else {
                RepeatChange::Keep
            };
            let key = key(&mut args)?;
            Ok(
                json!({"schedule":service.update(UpdateRequest { key, title, prompt, at, repeat })?}),
            )
        }
        "pause" | "resume" => {
            let key = key(&mut args)?;
            Ok(json!({"schedule":service.pause(&key, operation == "pause")?}))
        }
        "delete" => {
            let key = key(&mut args)?;
            Ok(json!({"deleted":true,"schedule":service.delete(&key)?}))
        }
        _ => Err(schedule_cli_error(format!(
            "Unknown schedule command '{operation}'. Run `riwork schedule help`."
        ))),
    }
}

fn schedule_minutes(value: Option<String>) -> Result<Option<u64>, ScheduleError> {
    value
        .map(|value| {
            value
                .parse::<u64>()
                .map_err(|_| schedule_cli_error("--every-minutes must be a whole positive integer"))
        })
        .transpose()
}

fn schedule_single(args: &mut Vec<String>, name: &str) -> Result<String, ScheduleError> {
    if args.len() == 1 && !args[0].starts_with("--") {
        Ok(args.remove(0))
    } else {
        Err(schedule_cli_error(format!("Expected exactly one {name}")))
    }
}

fn schedule_empty(args: &[String]) -> Result<(), ScheduleError> {
    ensure_empty(args).map_err(schedule_cli_error)
}

fn schedule_cli_error(message: impl Into<String>) -> ScheduleError {
    ScheduleError {
        code: "invalid_argument",
        message: message.into(),
        current: None,
    }
}

fn print_schedule_line(value: &serde_json::Value) {
    println!(
        "{}  rev {}  {}  {}  {}",
        value["id"].as_str().unwrap_or("?"),
        value["revision"].as_u64().unwrap_or(0),
        value["target"]["scope"]["scope"].as_str().unwrap_or("?"),
        if value["paused"].as_bool().unwrap_or(false) {
            "paused"
        } else {
            "active"
        },
        value["title"].as_str().unwrap_or(""),
    );
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
    use super::{
        agent_hook_command, frozen_codex_usage_home, grok_usage_unknown, opens_workspace,
        take_orchestrator_project, take_update_profile, unknown_invocation,
    };
    use crate::sessions::ShellSession;
    use crate::store::{State, Store};
    use std::io;

    #[test]
    fn claude_notification_input_failures_always_return_nonblocking_success() {
        struct FailedRead;
        impl io::Read for FailedRead {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("simulated input failure"))
            }
        }
        let home = std::env::temp_dir().join(format!("riwork-hook-input-{}", uuid::Uuid::new_v4()));
        let args = vec![
            "claude".into(),
            home.to_string_lossy().into_owned(),
            uuid::Uuid::new_v4().to_string(),
        ];
        assert!(agent_hook_command(args.clone(), "invalid JSON".as_bytes()).is_ok());
        assert!(agent_hook_command(args.clone(), vec![b'x'; 1_048_577].as_slice()).is_ok());
        assert!(agent_hook_command(args, FailedRead).is_ok());
        assert!(agent_hook_command(Vec::new(), "invalid invocation".as_bytes()).is_ok());
        assert!(!home.exists());
    }

    #[test]
    fn failed_claude_completion_queue_never_returns_a_blocking_cli_error() {
        let home = std::env::temp_dir().join(format!("riwork-hook-queue-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(home.join("project")).unwrap();
        let store = Store::open(home.clone()).unwrap();
        let project = store
            .add_project(home.join("project"), Some("Hook test"))
            .unwrap();
        store.set_project_notifications(&project.id, true).unwrap();
        let shell = uuid::Uuid::new_v4().to_string();
        std::fs::write(
            home.join("sessions.json"),
            serde_json::json!({"sessions":[{
                "id":shell,"project_id":project.id,"worktree_id":null,"kind":"project",
                "cwd":project.root,"command":null,"harness":"claude","created_at_unix":1
            }]})
            .to_string(),
        )
        .unwrap();
        let args = vec![
            "claude".into(),
            home.to_string_lossy().into_owned(),
            shell.clone(),
        ];
        let payload = |event| {
            serde_json::json!({"session_id":"test-session","prompt_id":"test-prompt","hook_event_name":event}).to_string()
        };
        assert!(agent_hook_command(args.clone(), payload("UserPromptSubmit").as_bytes()).is_ok());
        std::fs::write(home.join("agent-notifications.json"), "broken queue").unwrap();
        assert!(agent_hook_command(args, payload("Stop").as_bytes()).is_ok());
        assert_eq!(
            std::fs::read_to_string(home.join("agent-notifications.json")).unwrap(),
            "broken queue"
        );
        let cursor: serde_json::Value = serde_json::from_slice(
            &std::fs::read(
                home.join("agent-hooks/claude")
                    .join(format!("{shell}.json")),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(cursor["completed"], false);
        std::fs::remove_dir_all(home).unwrap();
    }

    fn codex_shell(home: Option<std::path::PathBuf>) -> ShellSession {
        serde_json::from_value(serde_json::json!({
            "id":"fixture-shell", "project_id":null, "worktree_id":null,
            "kind":"project", "cwd":"/", "command":"codex", "harness":"codex",
            "codex_account_id":"original-account", "codex_home":home,
            "created_at_unix":1
        }))
        .unwrap()
    }

    #[test]
    fn shell_usage_reads_only_its_frozen_account_home() {
        let home = std::env::temp_dir().join(format!("riwork-cli-usage-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        let shell = codex_shell(Some(home.clone()));
        assert_eq!(frozen_codex_usage_home(&shell).unwrap(), home);
        std::fs::remove_dir_all(home).unwrap();
        assert!(
            frozen_codex_usage_home(&shell)
                .unwrap_err()
                .contains("unavailable")
        );
    }

    #[test]
    fn legacy_or_relative_shell_home_never_falls_back_to_another_account() {
        assert!(
            frozen_codex_usage_home(&codex_shell(None))
                .unwrap_err()
                .contains("unknown")
        );
        assert!(
            frozen_codex_usage_home(&codex_shell(Some("relative".into())))
                .unwrap_err()
                .contains("unavailable")
        );
    }

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn update_profile_flags_keep_default_and_explicit_builds_separate() {
        let mut default = args(&["--no-reload"]);
        assert_eq!(take_update_profile(&mut default).unwrap(), None);
        assert_eq!(default, args(&["--no-reload"]));
        for (flag, profile) in [("--debug", "debug"), ("--release", "release")] {
            let mut input = args(&[flag, "--no-reload"]);
            assert_eq!(take_update_profile(&mut input).unwrap(), Some(profile));
            assert_eq!(input, args(&["--no-reload"]));
        }
        for values in [["--debug", "--release"], ["--release", "--debug"]] {
            assert!(take_update_profile(&mut args(&values)).is_err());
        }
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

    #[test]
    fn only_gui_startup_forms_open_the_workspace() {
        let directory = std::env::temp_dir().to_string_lossy().into_owned();
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
        assert!(opens_workspace(&[]));
        assert!(opens_workspace(&args(&[&directory])));
        // Older macOS adds a process serial number when launching the bundle.
        assert!(opens_workspace(&args(&["-psn_0_12345"])));
        assert!(opens_workspace(&args(&["-psn_0_12345", &directory])));
        for rejected in [
            args(&["--version"]),
            args(&["--json"]),
            args(&["shells", "list"]),
            args(&["--project", "A", "task", "list"]),
            args(&["/definitely/not/a/riwork/project"]),
            args(&[file]),
            args(&[&directory, "extra"]),
        ] {
            assert!(!opens_workspace(&rejected), "{rejected:?}");
        }
    }

    #[test]
    fn unknown_invocation_names_the_argument_and_points_to_usage() {
        let option = unknown_invocation(&args(&["--bogus"]));
        assert!(option.contains("Unknown option '--bogus'"), "{option}");
        let command = unknown_invocation(&args(&["shells", "list"]));
        assert!(
            command.contains("'shells' is not a riwork command"),
            "{command}"
        );
        for message in [option, command] {
            assert!(message.contains("Usage: riwork"), "{message}");
            assert!(message.contains("riwork help"), "{message}");
        }
    }

    #[test]
    fn grok_usage_is_an_empty_report_not_an_error() {
        let usage = grok_usage_unknown();
        let json = serde_json::to_value(&usage).unwrap();
        assert_eq!(json["provider"], "grok");
        assert_eq!(json["windows"], serde_json::json!([]));
        assert_eq!(json["account_label"], "unknown");
    }

    fn main_worktree_state() -> (State, [String; 3]) {
        let projects: [String; 3] = std::array::from_fn(|_| uuid::Uuid::new_v4().to_string());
        let mut worktrees = Vec::new();
        for (index, project) in projects.iter().enumerate() {
            worktrees.push(serde_json::json!({
                "id":uuid::Uuid::new_v4().to_string(),"project_id":project,"branch":"main",
                "path":format!("/fixture/project-{index}"),"is_primary":true,"created_at":1
            }));
        }
        worktrees.push(serde_json::json!({
            "id":uuid::Uuid::new_v4().to_string(),"project_id":projects[0],"branch":"feature",
            "path":"/fixture/project-0-feature","created_at":1
        }));
        let state = serde_json::from_value(serde_json::json!({
            "projects":projects.iter().enumerate().map(|(index, id)| serde_json::json!({
                "id":id,"name":format!("Project {index}"),"root":format!("/fixture/project-{index}"),"created_at":1
            })).collect::<Vec<_>>(),
            "worktrees":worktrees,
        }))
        .unwrap();
        (state, projects)
    }

    #[test]
    fn project_scope_disambiguates_a_branch_shared_by_many_projects() {
        let (state, [first, second, _]) = main_worktree_state();
        assert!(
            state
                .worktree("main")
                .unwrap_err()
                .contains("More than one")
        );
        let in_first = state.worktree_in_project(&first, "main").unwrap();
        assert_eq!(in_first.project_id, first);
        let in_second = state.worktree_in_project(&second, "main").unwrap();
        assert_eq!(in_second.project_id, second);
        let feature = state.worktree_in_project(&first, "feature").unwrap();
        assert_eq!(
            state
                .worktree_in_project(&first, &feature.id[..8])
                .unwrap()
                .id,
            feature.id
        );
        // A selector that only exists elsewhere still resolves globally so the
        // caller can report "belongs to another project".
        assert_eq!(
            state
                .worktree_in_project(&first, &in_second.id)
                .unwrap()
                .project_id,
            second
        );
        assert!(
            state
                .worktree_in_project(&first, "missing")
                .unwrap_err()
                .contains("No worktree matches")
        );
    }

    #[test]
    fn two_matches_inside_one_project_stay_ambiguous() {
        let (mut state, [first, ..]) = main_worktree_state();
        let mut duplicate = state.worktrees[0].clone();
        duplicate.id = uuid::Uuid::new_v4().to_string();
        duplicate.path = "/fixture/project-0-second-main".into();
        state.worktrees.push(duplicate);
        assert!(
            state
                .worktree_in_project(&first, "main")
                .unwrap_err()
                .contains("More than one")
        );
    }

    #[test]
    fn rename_keeps_a_folder_moved_after_the_caller_read_its_snapshot() {
        let home = std::env::temp_dir().join(format!("riwork-rename-{}", uuid::Uuid::new_v4()));
        let root = home.join("project");
        std::fs::create_dir_all(&root).unwrap();
        let store = Store::open(home.join("state")).unwrap();
        let project = store.add_project(root, Some("Before")).unwrap();
        let folder = store.create_project_folder("Group").unwrap();
        // The stale read is what `project update --name` used to copy the
        // folder from; a move landing in between must survive the rename.
        let stale = store.snapshot().unwrap();
        assert_eq!(stale.project(&project.id).unwrap().folder_id, None);
        store
            .move_project_to_folder(&project.id, Some(&folder.id))
            .unwrap();
        let renamed = store.rename_project(&project.id, "  After  ").unwrap();
        assert_eq!(renamed.name, "After");
        assert_eq!(renamed.folder_id.as_deref(), Some(folder.id.as_str()));
        let saved = store.snapshot().unwrap();
        assert_eq!(
            saved.project(&project.id).unwrap().folder_id.as_deref(),
            Some(folder.id.as_str())
        );
        assert!(store.rename_project(&project.id, "   ").is_err());
        std::fs::remove_dir_all(home).unwrap();
    }
}

#[cfg(test)]
mod open_tests {
    use super::gui_command;
    use std::{
        collections::BTreeSet,
        env, fs,
        path::{Path, PathBuf},
        process::Command,
    };

    fn removed_variables(command: &Command) -> BTreeSet<String> {
        command
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(name, _)| name.to_string_lossy().into_owned())
            .collect()
    }

    /// Runs this test again in a child with a controlled environment.
    fn child(case: &str, root: &Path, codex_home: &Path, pinned_home: &Path) {
        let output = Command::new(env::current_exe().unwrap())
            .args([
                "--exact",
                "cli::open_tests::open_scrubs_only_an_injected_codex_home",
                "--nocapture",
            ])
            .env("RIWORK_TEST_OPEN_CASE", case)
            .env("ORCA_USER_DATA_PATH", root.join("profile"))
            .env("CODEX_HOME", codex_home)
            .env("RIWORK_CODEX_ACCOUNT_HOME", pinned_home)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        // A filter that matches nothing also exits successfully.
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "{case}\n{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn open_scrubs_only_an_injected_codex_home() {
        if let Some(case) = env::var_os("RIWORK_TEST_OPEN_CASE") {
            let removed = removed_variables(&gui_command(
                PathBuf::from("/unused/riwork"),
                Path::new("/unused/project"),
            ));
            assert!(removed.contains("RIWORK_RESTORE_TICKET"));
            assert!(removed.contains("RIWORK_CODEX_ACCOUNT_HOME"));
            assert!(removed.contains("RIWORK_CODEX_SHELL_ID"));
            assert_eq!(removed.contains("CODEX_HOME"), case == "injected");
            return;
        }
        let root = env::temp_dir().join(format!("riwork-open-test-{}", uuid::Uuid::new_v4()));
        let account = root.join("profile/codex-accounts/account-a/home");
        fs::create_dir_all(&account).unwrap();
        let own = root.join("my-codex");
        // An account-bound session exports its account home; a system-default
        // session pins the user's own CODEX_HOME, which `open` must keep.
        child("injected", &root, &account, &account);
        child("own", &root, &own, &own);
        let _ = fs::remove_dir_all(root);
    }
}
