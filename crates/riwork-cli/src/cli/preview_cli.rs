//! Agent-facing preview lifecycle and testing commands. No GUI or simulator is
//! started by the CLI: commands are acknowledged by an already running workspace.

use super::{ensure_empty, print_json, take_flag, take_option, write_stdout};
use riwork_core::app_preview::{
    IosPreviewSpec, PreviewInput, PreviewMode, PreviewMouseButton, PreviewSpec,
};
use riwork_engine::preview_control::{ControlError, PreviewAction, PreviewControl};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

const HELP: &str = "\
riwork preview open --command CMD [--mode cooperative|window_capture] [--cwd PATH] [--project ID] [--background] [--wait] [--timeout-seconds N] [--json]
riwork preview open --ios [--scheme NAME] [--xcode-project PATH] [--bundle-id ID] [--device-type NAME] [--runtime NAME] [--configuration Debug] [--cwd PATH] [--project ID] [--background] [--wait] [--timeout-seconds N] [--json]
riwork preview list [--json]
riwork preview status PREVIEW_UUID [--json]
riwork preview screenshot PREVIEW_UUID --output FILE.png [--timeout-seconds N] [--json]
riwork preview input PREVIEW_UUID --input-json '{\"kind\":\"click\",\"x\":100,\"y\":200,\"button\":\"left\"}' [--json]
riwork preview tap PREVIEW_UUID X Y [--json]
riwork preview text PREVIEW_UUID --text TEXT [--json]
riwork preview key PREVIEW_UUID --key KEY [--json]
riwork preview close PREVIEW_UUID [--json]

open reuses an identical preview in the current project, selects its tab without
activating the OS window, and explicitly runs it. --background leaves tab selection
alone. --wait returns only after an actual frame; its default timeout is 120 seconds.
Restored tabs remain stopped until Run or an explicit open command. iOS previews
create an owned simulator and never reuse a user's existing device.
The project comes from --project, the calling RiWork chat/shell, or the current
working folder. Use --xcode-project for an .xcodeproj or .xcworkspace; --project
always selects a RiWork project. screenshot wakes the preview and waits for a fresh
frame. Input coordinates are logical pixels in the original frame. The input
protocol also supports pointer_down, pointer_move, pointer_up, scroll, key_up,
resize, and focus. Input acknowledgement means queued, not verified delivery.
All operations require this project open in an updated RiWork GUI. --json errors
have {\"error\":{\"code\",\"message\"}} and exit status 2.
";

pub(super) fn command(args: Vec<String>, json_output: bool) -> Result<(), String> {
    if args.is_empty()
        || matches!(args.as_slice(), [help] if matches!(help.as_str(), "help" | "-h" | "--help"))
    {
        print!("{HELP}");
        return Ok(());
    }
    match execute(args) {
        Ok(value) if json_output => print_json(&value),
        Ok(value) => {
            if let Some(path) = value.get("output").and_then(Value::as_str) {
                println!("{path}");
            } else if let Some(items) = value.get("items").and_then(Value::as_array) {
                for item in items {
                    println!(
                        "{}  {}  {}",
                        item["preview_id"].as_str().unwrap_or_default(),
                        item["phase"].as_str().unwrap_or_default(),
                        item["message"].as_str().unwrap_or_default()
                    );
                }
            } else {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?
                );
            }
            Ok(())
        }
        Err(error) if json_output => {
            print_json(&json!({"error":error}))?;
            Err(String::new())
        }
        Err(error) => Err(error.message),
    }
}

pub(crate) fn execute(mut args: Vec<String>) -> Result<Value, ControlError> {
    let operation = args.remove(0);
    let timeout = take_option(&mut args, "--timeout-seconds")
        .map_err(invalid)?
        .map(|s| {
            s.parse::<u64>()
                .map_err(|_| invalid("--timeout-seconds must be an integer."))
        })
        .transpose()?
        .unwrap_or(if operation == "open" { 120 } else { 10 });
    if !(1..=3600).contains(&timeout) {
        return Err(invalid("--timeout-seconds must be between 1 and 3600."));
    }
    let home = crate::paths::riwork_home().map_err(internal)?;
    let control = PreviewControl::open(&home)?;
    match operation.as_str() {
        "open" => {
            let (project_id, spec, background, wait) = open_arguments(&home, args)?;
            let reply = control.request(
                PreviewAction::Open {
                    project_id,
                    spec,
                    background,
                },
                Duration::from_secs(10),
            )?;
            let status = if wait {
                Some(control.wait_ready(&reply.preview_id, Duration::from_secs(timeout))?)
            } else {
                control.status(&reply.preview_id).ok()
            };
            Ok(json!({"v":1,"preview_id":reply.preview_id,"reused":reply.reused,"status":status}))
        }
        "list" => {
            ensure_empty(&args).map_err(invalid)?;
            Ok(json!({"v":1,"items":control.statuses()?}))
        }
        "status" => {
            let id = single_id(args)?;
            serde_json::to_value(control.status(&id)?).map_err(internal)
        }
        "close" => {
            let id = single_id(args)?;
            let reply = control.request(
                PreviewAction::Close { preview_id: id },
                Duration::from_secs(10),
            )?;
            Ok(json!({"v":1,"preview_id":reply.preview_id,"closed":true}))
        }
        "screenshot" | "snapshot" => {
            let output = take_option(&mut args, "--output")
                .map_err(invalid)?
                .ok_or_else(|| invalid("screenshot requires --output FILE.png."))?;
            let id = single_id(args)?;
            let before = control.status(&id)?.frame.map_or(0, |frame| frame.sequence);
            control.request(
                PreviewAction::Refresh {
                    preview_id: id.clone(),
                },
                Duration::from_secs(10),
            )?;
            let (status, png) = control.wait_frame(&id, before, Duration::from_secs(timeout))?;
            let path = PathBuf::from(output);
            std::fs::write(&path, png).map_err(internal)?;
            Ok(json!({"v":1,"preview_id":id,"output":path,"frame":status.frame}))
        }
        "input" | "tap" | "text" | "key" => {
            let (id, input) = input_arguments(&operation, args)?;
            let reply = control.request(
                PreviewAction::Input {
                    preview_id: id,
                    input,
                },
                Duration::from_secs(10),
            )?;
            Ok(json!({"v":1,"preview_id":reply.preview_id,"queued":true}))
        }
        _ => Err(invalid(
            "Unknown preview operation; run riwork preview help.",
        )),
    }
}

fn open_arguments(
    home: &Path,
    mut args: Vec<String>,
) -> Result<(String, PreviewSpec, bool, bool), ControlError> {
    let cwd = take_option(&mut args, "--cwd")
        .map_err(invalid)?
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(std::env::current_dir)
        .map_err(internal)?
        .canonicalize()
        .map_err(internal)?;
    if !cwd.is_dir() {
        return Err(invalid("--cwd must name a directory."));
    }
    let project = take_option(&mut args, "--project").map_err(invalid)?;
    let background = take_flag(&mut args, "--background");
    let wait = take_flag(&mut args, "--wait");
    let ios = take_flag(&mut args, "--ios");
    let mode = take_option(&mut args, "--mode").map_err(invalid)?;
    let command = take_option(&mut args, "--command").map_err(invalid)?;
    let spec = if ios {
        if mode.is_some() || command.is_some() {
            return Err(invalid(
                "--ios cannot be combined with --mode or --command.",
            ));
        }
        let ios = IosPreviewSpec {
            project: take_option(&mut args, "--xcode-project")
                .map_err(invalid)?
                .map(PathBuf::from),
            scheme: take_option(&mut args, "--scheme")
                .map_err(invalid)?
                .unwrap_or_default(),
            bundle_id: take_option(&mut args, "--bundle-id").map_err(invalid)?,
            device_type: take_option(&mut args, "--device-type").map_err(invalid)?,
            runtime: take_option(&mut args, "--runtime").map_err(invalid)?,
            configuration: take_option(&mut args, "--configuration")
                .map_err(invalid)?
                .unwrap_or_else(|| "Debug".into()),
        };
        PreviewSpec {
            command: String::new(),
            cwd,
            mode: PreviewMode::IosSimulator,
            ios: Some(ios),
        }
    } else {
        let mode = match mode.as_deref().unwrap_or("cooperative") {
            "cooperative" => PreviewMode::Cooperative,
            "window_capture" | "window-capture" => PreviewMode::WindowCapture,
            _ => return Err(invalid("--mode must be cooperative or window_capture.")),
        };
        let command = command
            .filter(|command| !command.trim().is_empty())
            .ok_or_else(|| invalid("open requires --command CMD or --ios."))?;
        PreviewSpec {
            command,
            cwd,
            mode,
            ios: None,
        }
    };
    ensure_empty(&args).map_err(invalid)?;
    let project_id = project_for(home, project.as_deref(), &spec.cwd)?;
    Ok((project_id, spec, background, wait))
}

fn project_for(home: &Path, explicit: Option<&str>, cwd: &Path) -> Result<String, ControlError> {
    let state = crate::store::Store::open(home)
        .map_err(internal)?
        .snapshot()
        .map_err(internal)?;
    if let Some(project) = explicit {
        return Ok(state.project(project).map_err(invalid)?.id.clone());
    }
    // Session attribution wins over a mutable global active project.
    for name in ["RIWORK_CHAT_ID", "RIWORK_SHELL_ID"] {
        if let Ok(id) = std::env::var(name) {
            let project = if name == "RIWORK_CHAT_ID" {
                crate::chat::log::chat_dir(home, &id)
                    .and_then(|path| std::fs::read(path.join("info.json")).ok())
                    .and_then(|bytes| {
                        serde_json::from_slice::<crate::chat::model::ChatInfo>(&bytes).ok()
                    })
                    .and_then(|chat| chat.project_id)
            } else {
                crate::sessions::registered_shell(home, &id)
                    .ok()
                    .and_then(|shell| shell.project_id)
            };
            if let Some(project) = project {
                return Ok(state.project(&project).map_err(invalid)?.id.clone());
            }
        }
    }
    let by_path = state
        .worktrees
        .iter()
        .map(|worktree| (&worktree.path, &worktree.project_id))
        .chain(
            state
                .projects
                .iter()
                .map(|project| (&project.root, &project.id)),
        )
        .filter(|(path, _)| cwd.starts_with(path))
        .max_by_key(|(path, _)| path.components().count())
        .map(|(_, project)| project.clone());
    by_path
        .ok_or_else(|| invalid("Cannot infer a RiWork project for this folder; pass --project ID."))
}

fn input_arguments(
    operation: &str,
    mut args: Vec<String>,
) -> Result<(String, PreviewInput), ControlError> {
    let input = match operation {
        "tap" if args.len() == 3 => {
            let parse = |value: &str| {
                value
                    .parse::<f32>()
                    .ok()
                    .filter(|v| v.is_finite() && *v >= 0.)
                    .ok_or_else(|| {
                        invalid("Touch coordinates must be finite, nonnegative numbers.")
                    })
            };
            let x = parse(&args[1])?;
            let y = parse(&args[2])?;
            args.truncate(1);
            PreviewInput::Click {
                x,
                y,
                button: PreviewMouseButton::Left,
            }
        }
        "text" => PreviewInput::Text {
            text: take_option(&mut args, "--text")
                .map_err(invalid)?
                .ok_or_else(|| invalid("text requires --text TEXT."))?,
        },
        "key" => PreviewInput::Key {
            key: take_option(&mut args, "--key")
                .map_err(invalid)?
                .ok_or_else(|| invalid("key requires --key KEY."))?,
        },
        "input" => {
            let value = take_option(&mut args, "--input-json")
                .map_err(invalid)?
                .ok_or_else(|| invalid("input requires --input-json JSON."))?;
            if value.len() > 64 * 1024 {
                return Err(invalid("Input JSON must be at most 64 KiB."));
            }
            serde_json::from_str(&value).map_err(invalid)?
        }
        _ => return Err(invalid("tap requires PREVIEW_UUID X Y.")),
    };
    let id = single_id(args)?;
    Ok((id, input))
}
fn single_id(args: Vec<String>) -> Result<String, ControlError> {
    match args.as_slice() {
        [id] if uuid::Uuid::parse_str(id).is_ok_and(|u| u.to_string() == *id) => Ok(id.clone()),
        _ => Err(invalid("A full canonical preview UUID is required.")),
    }
}
fn invalid(message: impl std::fmt::Display) -> ControlError {
    ControlError::new("invalid_request", message.to_string())
}
fn internal(message: impl std::fmt::Display) -> ControlError {
    ControlError::new("cli_error", message.to_string())
}

#[cfg(test)]
mod tests;
