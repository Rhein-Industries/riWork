//! `riwork flow`: a project's flows (statecharts in `.ontack/flows`) and the chats they lead.

use std::path::PathBuf;

use riwork_core::flows as model;
use riwork_engine::flows::{self as runs, ChatRef, Run};
use serde_json::json;

use super::{ensure_empty, find_chat, pop_command, print_json, project_id, take_option, write_stdout};
use crate::store::Store;

const HELP: &str = "\
riwork flow list [--project ID] [--json]      The project's flows and whether each is valid
riwork flow show FLOW [--project ID] [--mermaid] [--json]   A flow's states, or its diagram source
riwork flow new NAME [--project ID]           Create .ontack/flows/ID.json from a starting flow
riwork flow start FLOW [--chat CHAT_ID]       Lead a chat through the flow (the project is the chat's)
riwork flow status [--chat CHAT_ID] [--json]  Where the chat's flow is and how it moves on
riwork flow next EVENT [--evidence TEXT] [--chat CHAT_ID]   Move on (an agent runs this)
riwork flow runs [--json]                     Every run, newest first
riwork flow approve|deny RUN_ID [--note TEXT] Decide a transition that waits for approval
riwork flow stop RUN_ID                       End a run

A flow is a JSON statechart in the project's .ontack/flows folder: states with instructions,
nested states with an initial child, and events that move on to a target. A transition may
need evidence, a check command that passes in the chat's folder (within its timeout, at most
3600 s), or the person's approval. --chat defaults to RIWORK_CHAT_ID, so an agent runs
`riwork flow next EVENT` from its own shell. start, approve, deny and stop tell the chat in a
message; a chat that starts its own run reads its first step from the command instead.
next answers what the agent does now and exits 2 when it cannot move on yet.
";

pub(super) fn command(mut args: Vec<String>, json: bool) -> Result<(), String> {
    let home = crate::paths::riwork_home()?;
    let operation = pop_command(&mut args, "help");
    match operation.as_str() {
        "help" | "--help" | "-h" => write_stdout(format_args!("{HELP}")),
        "list" => {
            let root = project_root(take_option(&mut args, "--project")?.as_deref())?.1;
            ensure_empty(&args)?;
            let found = model::discover(&root);
            if json {
                return print_json(&json!({
                    "folder": model::project_dir(&root),
                    "flows": found.iter().map(|flow| match &flow.flow {
                        Ok(parsed) => json!({"id": flow.id, "path": flow.path, "name": parsed.name, "description": parsed.description}),
                        Err(error) => json!({"id": flow.id, "path": flow.path, "error": error}),
                    }).collect::<Vec<_>>(),
                }));
            }
            if found.is_empty() {
                write_stdout(format_args!("No flows in {}. Create one: riwork flow new NAME\n", model::project_dir(&root).display()));
            }
            for flow in found {
                match flow.flow {
                    Ok(parsed) => write_stdout(format_args!("{}  {}\n", flow.id, parsed.name)),
                    Err(error) => write_stdout(format_args!("{}  (invalid: {error})\n", flow.id)),
                }
            }
        }
        "show" => {
            let mermaid = super::take_flag(&mut args, "--mermaid");
            let root = project_root(take_option(&mut args, "--project")?.as_deref())?.1;
            let id = super::take_single(args, "flow show FLOW")?;
            let flow = model::find(&root, &id)?;
            if json {
                return print_json(&json!({"id": id, "flow": flow, "mermaid": flow.mermaid(None)}));
            }
            if mermaid {
                write_stdout(format_args!("{}\n", flow.mermaid(None)));
            } else {
                write_stdout(format_args!("{}", outline(&flow)));
            }
        }
        "new" => {
            let root = project_root(take_option(&mut args, "--project")?.as_deref())?.1;
            if args.is_empty() || args.iter().any(|arg| arg.starts_with("--")) {
                return Err("Usage: riwork flow new NAME [--project ID]".into());
            }
            let (id, path) = model::create(&root, &args.join(" "))?;
            if json {
                return print_json(&json!({"id": id, "path": path}));
            }
            write_stdout(format_args!("Created {} ({id}). Edit it, then: riwork flow start {id}\n", path.display()));
        }
        "start" => {
            let chat = take_option(&mut args, "--chat")?;
            let flow = super::take_single(args, "flow start FLOW [--chat CHAT_ID]")?;
            let own = std::env::var("RIWORK_CHAT_ID").ok().filter(|id| !id.is_empty());
            let selector = chat.clone().or(own.clone()).ok_or("Name the chat with --chat CHAT_ID.")?;
            let socket = crate::chat::host::ensure_host(&home)?;
            let chats = crate::chat::client::Client::connect(&socket)?.list()?;
            let info = find_chat(&chats, &selector)?;
            let project = info.project_id.as_deref().ok_or("This chat belongs to no project, so it has no flows.")?;
            let (project_id, root) = project_root(Some(project))?;
            let chat = ChatRef { id: info.id.clone(), title: info.title.clone(), cwd: info.cwd.clone() };
            // A chat that starts its own run is in a turn: it reads its first step here.
            let itself = own.as_deref() == Some(info.id.as_str());
            let run = if itself {
                runs::start(&home, &root, &project_id, &flow, &chat, &runs::deliver_nowhere)?
            } else {
                runs::start(&home, &root, &project_id, &flow, &chat, &runs::deliver_to_chat(&home))?
            };
            if json {
                return print_json(&run);
            }
            if itself {
                write_stdout(format_args!("{}\n", runs::status(&home, &run.chat_id)?));
            } else {
                write_stdout(format_args!("Started {} in {} (run {}).\n", run.flow_name, chat_label(&run), run.id));
            }
        }
        "status" => {
            let chat = chat_id(take_option(&mut args, "--chat")?)?;
            ensure_empty(&args)?;
            let text = runs::status(&home, &chat)?;
            if json {
                return print_json(&json!({"run": runs::run_for_chat(&home, &chat), "briefing": text}));
            }
            write_stdout(format_args!("{text}\n"));
        }
        "next" => {
            let chat = chat_id(take_option(&mut args, "--chat")?)?;
            let evidence = take_option(&mut args, "--evidence")?.unwrap_or_default();
            let event = super::take_single(args, "flow next EVENT [--evidence TEXT]")?;
            match runs::next(&home, &chat, &event, &evidence) {
                Ok(text) if json => print_json(&json!({"moved": true, "briefing": text, "run": runs::run_for_chat(&home, &chat)}))?,
                Ok(text) => write_stdout(format_args!("{text}\n")),
                // Not moving on is an answer the agent reads, not a failure of the command.
                Err(text) if json => {
                    print_json(&json!({"moved": false, "briefing": text}))?;
                    std::process::exit(2);
                }
                Err(text) => {
                    write_stdout(format_args!("{text}\n"));
                    std::process::exit(2);
                }
            }
        }
        "runs" => {
            ensure_empty(&args)?;
            let all = runs::runs(&home);
            if json {
                return print_json(&all);
            }
            for run in all {
                write_stdout(format_args!("{}  {:<9}  {}  {}  {}\n", run.id, format!("{:?}", run.status).to_lowercase(), run.flow_name, run.state, chat_label(&run)));
            }
        }
        "approve" | "deny" => {
            let note = take_option(&mut args, "--note")?.unwrap_or_default();
            let id = super::take_single(args, "flow approve|deny RUN_ID [--note TEXT]")?;
            let deliver = runs::deliver_to_chat(&home);
            let run = if operation == "approve" { runs::approve(&home, &id, &note, &deliver)? } else { runs::deny(&home, &id, &note, &deliver)? };
            if json {
                return print_json(&run);
            }
            write_stdout(format_args!("{} {}: now in {}.\n", if operation == "approve" { "Approved" } else { "Turned down" }, run.flow_name, run.state));
        }
        "stop" => {
            let id = super::take_single(args, "flow stop RUN_ID")?;
            let run = runs::stop(&home, &id, &runs::deliver_to_chat(&home))?;
            if json {
                return print_json(&run);
            }
            write_stdout(format_args!("Stopped {} in {}.\n", run.flow_name, chat_label(&run)));
        }
        other => return Err(format!("Unknown flow command {other:?}. Run: riwork flow help")),
    }
    Ok(())
}

fn project_root(selector: Option<&str>) -> Result<(String, PathBuf), String> {
    let state = Store::open_default()?.snapshot()?;
    let id = project_id(&state, selector)?;
    let project = state.project(&id)?;
    Ok((project.id.clone(), project.root.clone()))
}

fn chat_id(selector: Option<String>) -> Result<String, String> {
    selector
        .or_else(|| std::env::var("RIWORK_CHAT_ID").ok().filter(|id| !id.is_empty()))
        .ok_or_else(|| "Name the chat with --chat CHAT_ID (an agent's shell has RIWORK_CHAT_ID).".to_owned())
}

fn chat_label(run: &Run) -> String {
    if run.chat_title.is_empty() { format!("chat {}", &run.chat_id[..run.chat_id.len().min(8)]) } else { format!("chat {:?}", run.chat_title) }
}

/// A flow as indented text: each state, its instructions' first line, and its events.
fn outline(flow: &model::Flow) -> String {
    fn walk(out: &mut String, states: &std::collections::BTreeMap<String, model::State>, initial: Option<&str>, depth: usize) {
        for (id, state) in states {
            let indent = "  ".repeat(depth);
            let mark = if Some(id.as_str()) == initial { "→ " } else { "  " };
            let title = if state.title.is_empty() { id.clone() } else { format!("{} ({id})", state.title) };
            out.push_str(&format!("{indent}{mark}{title}{}\n", if state.r#final { "  [final]" } else { "" }));
            if let Some(line) = state.instructions.lines().next().filter(|line| !line.trim().is_empty()) {
                out.push_str(&format!("{indent}     {line}\n"));
            }
            for (event, transition) in &state.on {
                let gate = match &transition.check {
                    Some(check) if check.approval => " (approval)".to_owned(),
                    Some(check) => format!(" (check: {})", check.command.as_deref().unwrap_or_default()),
                    None => String::new(),
                };
                out.push_str(&format!("{indent}     {event} → {}{gate}{}\n", transition.target, if transition.evidence { " (evidence)" } else { "" }));
            }
            walk(out, &state.states, state.initial.as_deref(), depth + 1);
        }
    }
    let mut out = format!("{}\n", flow.name);
    if !flow.description.is_empty() {
        out.push_str(&format!("{}\n", flow.description));
    }
    out.push('\n');
    walk(&mut out, &flow.states, Some(flow.initial.as_str()), 0);
    out
}
