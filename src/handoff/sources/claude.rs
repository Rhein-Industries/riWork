//! A Claude Code session transcript (`~/.claude/projects/<folder>/<session>.jsonl`) as
//! `Entry`s.
//!
//! One line per content block: the user's messages (a string or text blocks), the agent's
//! text, its tool calls, and the tool results, which are matched back to their calls by
//! id. Notes the CLI writes for itself, a subagent's own conversation and the model's
//! hidden thinking are not part of what a successor needs.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use serde_json::Value;

use super::{Read, claude_config_dir, claude_folder, input_detail, push, read_records, text_of};
use crate::{
    handoff::document::{Body, Entry, FileEdit, Mark},
    sessions::ShellSession,
};

/// The transcript of the session a terminal's Claude is in. The session id comes from
/// the hooks RiWork gives every launch; the file is in the folder of the directory the
/// session started in, or else in whichever folder has it (a session moves with `/resume`).
pub fn locate(home: &Path, shell: &ShellSession) -> Option<PathBuf> {
    let (session, _) = crate::agent_hooks::schedule_state(home, &shell.id)?;
    find(&claude_config_dir()?, &shell.cwd, &session)
}

pub fn find(config: &Path, cwd: &Path, session: &str) -> Option<PathBuf> {
    // The id names a file below the projects folder; nothing else may.
    if session.is_empty()
        || !session
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return None;
    }
    let projects = config.join("projects");
    let own = projects
        .join(claude_folder(cwd))
        .join(format!("{session}.jsonl"));
    if own.is_file() {
        return Some(own);
    }
    std::fs::read_dir(&projects)
        .ok()?
        .flatten()
        .map(|folder| folder.path().join(format!("{session}.jsonl")))
        .find(|path| path.is_file())
}

pub fn read(path: &Path) -> Result<Read, String> {
    let mut entries: Vec<Entry> = Vec::new();
    // Tool calls waiting for their result, by call id: where their entry is, and what the
    // tool is called.
    let mut waiting: HashMap<String, (usize, String)> = HashMap::new();
    let mut model = None;
    let skipped = read_records(path, |record| {
        if record["isSidechain"].as_bool() == Some(true) || record["isMeta"].as_bool() == Some(true)
        {
            return;
        }
        let message = &record["message"];
        match record["type"].as_str() {
            Some("assistant") => {
                if let Some(name) = message["model"]
                    .as_str()
                    .filter(|name| !name.starts_with('<'))
                {
                    model = Some(name.to_owned());
                }
                for block in blocks(&message["content"]) {
                    match block["type"].as_str() {
                        Some("text") => {
                            let text = text_of(&block["text"]);
                            if !text.trim().is_empty() {
                                push(&mut entries, Entry::Agent(text));
                            }
                        }
                        Some("tool_use") => {
                            let entry = tool_entry(
                                block["name"].as_str().unwrap_or("tool"),
                                &block["input"],
                            );
                            if let Some(id) = block["id"].as_str() {
                                waiting.insert(
                                    id.to_owned(),
                                    (
                                        entries.len(),
                                        block["name"].as_str().unwrap_or("tool").to_owned(),
                                    ),
                                );
                            }
                            // Folding file changes would move the entry a result
                            // arrives for; only a result's own entry is indexed.
                            entries.push(entry);
                        }
                        _ => {}
                    }
                }
            }
            Some("user") => {
                if record["isCompactSummary"].as_bool() == Some(true) {
                    let summary = text_of(&message["content"]);
                    entries.push(Entry::Note("The context was compacted here.".into()));
                    if !summary.trim().is_empty() {
                        entries.push(Entry::Summary(summary));
                    }
                    return;
                }
                for block in blocks(&message["content"]) {
                    match block["type"].as_str() {
                        Some("text") => {
                            if let Some(text) = user_text(&text_of(&block["text"])) {
                                push(&mut entries, Entry::User(text));
                            }
                        }
                        Some("tool_result") => {
                            let Some((at, tool)) = block["tool_use_id"]
                                .as_str()
                                .and_then(|id| waiting.get(id))
                                .cloned()
                            else {
                                continue;
                            };
                            let result = text_of(&block["content"]);
                            let failed = block["is_error"].as_bool() == Some(true);
                            // An edit that did not happen changed no file.
                            if failed && let Entry::Files(files) = &entries[at] {
                                entries[at] = Entry::Tool {
                                    name: tool,
                                    detail: files
                                        .iter()
                                        .map(|file| file.path.as_str())
                                        .collect::<Vec<_>>()
                                        .join(", "),
                                    output: Some(result),
                                    failed: true,
                                };
                                continue;
                            }
                            match &mut entries[at] {
                                Entry::Command {
                                    output, exit_code, ..
                                } => {
                                    *exit_code = exit_status(&result).or(failed.then_some(1));
                                    *output = result;
                                }
                                Entry::Tool {
                                    name,
                                    output,
                                    failed: was_failed,
                                    ..
                                } => {
                                    *was_failed = failed;
                                    if failed || matches!(name.as_str(), "Task" | "Agent") {
                                        *output = Some(result);
                                    }
                                }
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    })?;
    Ok(Read {
        body: Body::Conversation(fold_files(entries)),
        model,
        origin: "the Claude session transcript".into(),
        caveat: (skipped > 0).then(|| {
            format!(
                "The transcript is long: its first {} MB were not read, so the conversation starts later than it began.",
                skipped >> 20
            )
        }),
    })
}

/// The content of a message as blocks: a string is one text block.
fn blocks(content: &Value) -> Vec<Value> {
    match content {
        Value::String(text) => vec![serde_json::json!({"type": "text", "text": text})],
        Value::Array(blocks) => blocks.clone(),
        _ => Vec::new(),
    }
}

/// What the person typed, or `None` for text the CLI wrote into the user's turn for its
/// own sake: command output, reminders, interrupt notices. A slash command reads as the
/// command.
fn user_text(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty()
        || text.starts_with("<local-command-")
        || text.starts_with("<system-reminder>")
        || text.starts_with("[Request interrupted")
    {
        return None;
    }
    if let Some(name) = tagged(text, "command-name") {
        let arguments = tagged(text, "command-args").unwrap_or_default();
        return Some(format!("{name} {arguments}").trim().to_owned());
    }
    Some(text.to_owned())
}

fn tagged(text: &str, tag: &str) -> Option<String> {
    let start = text.find(&format!("<{tag}>"))? + tag.len() + 2;
    let end = text[start..].find(&format!("</{tag}>"))? + start;
    Some(text[start..end].trim().to_owned())
}

/// `Exit code 2` at the start of a failed command's result.
fn exit_status(result: &str) -> Option<i32> {
    result
        .strip_prefix("Exit code ")?
        .split(|c: char| !c.is_ascii_digit() && c != '-')
        .next()?
        .parse()
        .ok()
}

fn tool_entry(name: &str, input: &Value) -> Entry {
    let path = || input["file_path"].as_str().unwrap_or("").to_owned();
    match name {
        "Bash" => Entry::Command {
            command: input["command"].as_str().unwrap_or("").to_owned(),
            output: String::new(),
            exit_code: None,
        },
        "Edit" | "MultiEdit" | "NotebookEdit" => Entry::Files(vec![FileEdit {
            path: if name == "NotebookEdit" {
                input["notebook_path"].as_str().unwrap_or("").to_owned()
            } else {
                path()
            },
            change: "edited",
        }]),
        "Write" => Entry::Files(vec![FileEdit {
            path: path(),
            change: "written",
        }]),
        "WebSearch" => Entry::Search(input["query"].as_str().unwrap_or("").to_owned()),
        "TodoWrite" => Entry::Checklist {
            title: "Todo list",
            items: input["todos"]
                .as_array()
                .map(|todos| {
                    todos
                        .iter()
                        .map(|todo| {
                            let mark = match todo["status"].as_str() {
                                Some("completed") => Mark::Done,
                                Some("in_progress") => Mark::Active,
                                _ => Mark::Pending,
                            };
                            (mark, text_of(&todo["content"]))
                        })
                        .collect()
                })
                .unwrap_or_default(),
        },
        _ => Entry::Tool {
            name: name.to_owned(),
            detail: input_detail(input),
            output: None,
            failed: false,
        },
    }
}

/// Runs of file changes become one entry each, after the results are matched to their
/// calls (a result is found by position until then).
fn fold_files(entries: Vec<Entry>) -> Vec<Entry> {
    let mut folded = Vec::with_capacity(entries.len());
    for entry in entries {
        push(&mut folded, entry);
    }
    folded
}
