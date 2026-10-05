//! A Codex rollout (`~/.codex/sessions/…/rollout-….jsonl`) as `Entry`s.
//!
//! A rollout holds every conversation twice. `event_msg` records of type `item_completed`
//! carry the items a Codex window draws (the user's messages, the agent's, commands with
//! their output, file changes, tool calls); `response_item` records carry what was sent to
//! the model. The first are the cleaner account, so they are used whenever there are any;
//! a rollout from a version before them is read from the second.

use std::{collections::HashMap, path::Path};

use serde_json::Value;

use super::{Read, input_detail, push, read_records, text_of};
use crate::handoff::document::{Body, Entry, FileEdit, Mark};

pub fn read(path: &Path) -> Result<Read, String> {
    let mut items = Vec::new();
    let mut sent = Sent::default();
    // How much of what was sent came before the first item: the turns of a rollout that
    // began in a version without items and went on in one with them.
    let mut sent_before_items = None;
    let mut model = None;
    let skipped = read_records(path, |record| {
        let payload = &record["payload"];
        match record["type"].as_str() {
            Some("event_msg") => match payload["type"].as_str() {
                Some("item_completed") => {
                    sent_before_items.get_or_insert(sent.entries.len());
                    if let Some(entry) = item_entry(&payload["item"]) {
                        push(&mut items, entry);
                    }
                }
                Some("thread_settings_applied") => {
                    model = text(&payload["thread_settings"]["model"]).or(model.take());
                }
                _ => {}
            },
            Some("turn_context") => model = text(&payload["model"]).or(model.take()),
            Some("response_item") => sent.take(payload),
            _ => {}
        }
    })?;
    let entries = match sent_before_items {
        None => sent.entries,
        // Whatever was sent before the first item and is not one of them is an older turn.
        Some(before) => {
            let mut older: Vec<Entry> = sent
                .entries
                .into_iter()
                .take(before)
                .filter(|entry| !items.contains(entry))
                .collect();
            older.extend(items);
            older
        }
    };
    Ok(Read {
        body: Body::Conversation(entries),
        model,
        origin: "the Codex rollout".into(),
        caveat: (skipped > 0).then(|| {
            format!(
                "The rollout is long: its first {} MB were not read, so the conversation starts later than it began.",
                skipped >> 20
            )
        }),
    })
}

fn text(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// A user message that RiWork or Codex put there, not the person.
fn injected(text: &str) -> bool {
    let start = text.trim_start();
    start.starts_with("Load the following RiWork desktop automation guidance")
        || [
            "<environment_context>",
            "<user_instructions>",
            "<permissions",
            "# AGENTS.md",
            "<turn_aborted>",
        ]
        .iter()
        .any(|tag| start.starts_with(tag))
}

// ---- Items ---------------------------------------------------------------------------

fn item_entry(item: &Value) -> Option<Entry> {
    Some(match item["type"].as_str()? {
        "UserMessage" => {
            let message = text_of(&item["content"]);
            if message.trim().is_empty() || injected(&message) {
                return None;
            }
            Entry::User(message)
        }
        "AgentMessage" => {
            let message = text_of(&item["content"]);
            if message.trim().is_empty() {
                return None;
            }
            Entry::Agent(message)
        }
        "CommandExecution" => Entry::Command {
            command: match item["status"].as_str() {
                Some("declined") => format!("{} (declined)", script(&item["command"])),
                _ => script(&item["command"]),
            },
            output: text(&item["aggregated_output"]).unwrap_or_else(|| {
                format!("{}{}", text_of(&item["stdout"]), text_of(&item["stderr"]))
            }),
            exit_code: item["exit_code"].as_i64().map(|code| code as i32),
        },
        "FileChange" => {
            let files: Vec<FileEdit> = item["changes"]
                .as_object()?
                .iter()
                .map(|(path, change)| FileEdit {
                    path: path.clone(),
                    change: match change["type"].as_str() {
                        Some("add") => "added",
                        Some("delete") => "deleted",
                        _ if change["move_path"].is_string() => "renamed",
                        _ => "edited",
                    },
                })
                .collect();
            match item["status"].as_str() {
                // A change that was refused or failed changed no file.
                Some(status @ ("failed" | "declined")) => Entry::Tool {
                    name: format!("file changes ({status})"),
                    detail: files
                        .iter()
                        .map(|file| file.path.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    output: None,
                    failed: true,
                },
                _ => Entry::Files(files),
            }
        }
        "McpToolCall" => {
            let failed = item["status"].as_str() == Some("failed") || item["error"].is_object();
            Entry::Tool {
                name: format!(
                    "{}/{}",
                    item["server"].as_str().unwrap_or("mcp"),
                    item["tool"].as_str().unwrap_or("tool")
                ),
                detail: input_detail(&item["arguments"]),
                output: failed.then(|| text_of(&item["result"]["content"])),
                failed,
            }
        }
        "DynamicToolCall" => Entry::Tool {
            name: format!(
                "{}/{}",
                item["namespace"].as_str().unwrap_or("tool"),
                item["tool"].as_str().unwrap_or("call")
            ),
            detail: input_detail(&item["arguments"]),
            output: None,
            failed: item["success"].as_bool() == Some(false),
        },
        "WebSearch" => Entry::Search(text(&item["query"])?),
        "Extension" if item["kind"].as_str() == Some("web.search") => {
            Entry::Search(text(&item["query"])?)
        }
        "ContextCompaction" => Entry::Note("The context was compacted here.".into()),
        "ImageView" => Entry::Tool {
            name: "view_image".into(),
            detail: item["path"].as_str().unwrap_or("").to_owned(),
            output: None,
            failed: false,
        },
        "Plan" => Entry::Checklist {
            title: "Plan",
            items: vec![(Mark::Pending, text_of(&item["text"]))],
        },
        _ => return None,
    })
}

/// A command as its script: Codex runs `["/bin/zsh", "-lc", "the script"]`, and the script
/// is what the agent wrote.
fn script(command: &Value) -> String {
    if let Some(command) = command.as_str() {
        return command.to_owned();
    }
    let words: Vec<&str> = command
        .as_array()
        .map(|words| words.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    match words.as_slice() {
        [_, flag, script, ..] if matches!(*flag, "-lc" | "-c") => (*script).to_owned(),
        _ => words.join(" "),
    }
}

// ---- What was sent to the model ---------------------------------------------------------

/// The fallback reading: messages and tool calls as they were sent, joined to their
/// outputs by call id.
#[derive(Default)]
struct Sent {
    entries: Vec<Entry>,
    /// Commands and tools waiting for their output, by call id.
    waiting: HashMap<String, usize>,
}

impl Sent {
    fn take(&mut self, payload: &Value) {
        match payload["type"].as_str() {
            Some("message") => {
                let message = text_of(&payload["content"]);
                if message.trim().is_empty() {
                    return;
                }
                match payload["role"].as_str() {
                    Some("user") if !injected(&message) => {
                        push(&mut self.entries, Entry::User(message))
                    }
                    Some("assistant") => push(&mut self.entries, Entry::Agent(message)),
                    _ => {}
                }
            }
            Some("function_call") => {
                let arguments: Value = payload["arguments"]
                    .as_str()
                    .and_then(|text| serde_json::from_str(text).ok())
                    .unwrap_or(Value::Null);
                let name = payload["name"].as_str().unwrap_or("tool");
                let entry = if matches!(
                    name,
                    "shell" | "exec_command" | "container.exec" | "local_shell"
                ) {
                    Entry::Command {
                        command: match (&arguments["command"], &arguments["cmd"]) {
                            (Value::Array(_), _) => script(&arguments["command"]),
                            (Value::String(command), _) | (_, Value::String(command)) => {
                                command.clone()
                            }
                            _ => String::new(),
                        },
                        output: String::new(),
                        exit_code: None,
                    }
                } else {
                    Entry::Tool {
                        name: name.to_owned(),
                        detail: input_detail(&arguments),
                        output: None,
                        failed: false,
                    }
                };
                self.start(payload, entry);
            }
            Some("custom_tool_call") => {
                let name = payload["name"].as_str().unwrap_or("tool");
                let input = payload["input"].as_str().unwrap_or("");
                if name == "apply_patch" {
                    push(&mut self.entries, Entry::Files(patched_files(input)));
                } else {
                    self.start(
                        payload,
                        Entry::Tool {
                            name: name.to_owned(),
                            detail: input_detail(&Value::String(
                                input.lines().next().unwrap_or("").to_owned(),
                            )),
                            output: None,
                            failed: false,
                        },
                    );
                }
            }
            Some("function_call_output" | "custom_tool_call_output") => {
                let Some(&at) = payload["call_id"]
                    .as_str()
                    .and_then(|id| self.waiting.get(id))
                else {
                    return;
                };
                if let Entry::Command {
                    output, exit_code, ..
                } = &mut self.entries[at]
                {
                    let (text, code) = tool_output(&payload["output"]);
                    *output = text;
                    *exit_code = code;
                }
            }
            Some("web_search_call") => {
                if let Some(query) = text(&payload["action"]["query"]) {
                    push(&mut self.entries, Entry::Search(query));
                }
            }
            _ => {}
        }
    }

    fn start(&mut self, payload: &Value, entry: Entry) {
        if let Some(id) = payload["call_id"].as_str() {
            self.waiting.insert(id.to_owned(), self.entries.len());
        }
        self.entries.push(entry);
    }
}

/// A tool's output as text and its exit code: a string, a list of text parts, or a JSON
/// string holding `output` and `metadata.exit_code`.
fn tool_output(output: &Value) -> (String, Option<i32>) {
    let text = text_of(output);
    if let Ok(Value::Object(object)) = serde_json::from_str::<Value>(&text)
        && let Some(inner) = object.get("output").and_then(Value::as_str)
    {
        let code = object
            .get("metadata")
            .and_then(|metadata| metadata["exit_code"].as_i64())
            .map(|code| code as i32);
        return (inner.to_owned(), code);
    }
    (text, None)
}

/// The files an `apply_patch` call names.
fn patched_files(patch: &str) -> Vec<FileEdit> {
    patch
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let (change, path) = if let Some(path) = line.strip_prefix("*** Add File: ") {
                ("added", path)
            } else if let Some(path) = line.strip_prefix("*** Update File: ") {
                ("edited", path)
            } else if let Some(path) = line.strip_prefix("*** Delete File: ") {
                ("deleted", path)
            } else {
                return None;
            };
            Some(FileEdit {
                path: path.trim().to_owned(),
                change,
            })
        })
        .collect()
}
