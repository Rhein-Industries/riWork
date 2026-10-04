//! Reading the conversation of a chat or a terminal into `Entry`s.
//!
//! - A chat is its `events.jsonl`, folded with `Transcript` as a chat tab folds it.
//! - A terminal's Codex is its rollout, found through the binding RiWork keeps for the pane
//!   (`activity::bound_rollout`); its Claude is the session transcript at
//!   `~/.claude/projects/<folder>/<session>.jsonl`, found through the session id that
//!   Claude's hooks report (`agent_hooks`).
//! - Any other terminal, and an agent whose conversation file is not known yet, is read
//!   from the tmux scrollback, which is the rendered screen and nothing more.

use std::{
    fs::File,
    io::{BufRead, BufReader, Read as _, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use serde_json::Value;

use super::document::{Body, Entry, FileEdit, Mark};
use crate::{
    chat::model::{ChangeKind, ItemBody, ItemStatus, NoticeLevel, StepStatus, Transcript},
    sessions::{HarnessKind, SessionManager, ShellSession},
};

mod claude;
mod codex;

/// How many lines of a terminal's scrollback are read; the document keeps what fits.
const SCROLLBACK_LINES: usize = 5000;
/// A conversation file bigger than this is read from its end: the newest part is what a
/// handoff keeps, and a rollout can run to hundreds of megabytes.
const MAX_READ: u64 = 64 << 20;
/// A line of a conversation file longer than this is skipped.
const MAX_LINE: usize = 16 << 20;

/// A conversation as read, and what to tell about how it was read.
pub struct Read {
    pub body: Body,
    pub model: Option<String>,
    /// Where it came from: "the chat's event log", "the Codex rollout", …
    pub origin: String,
    /// Why this is less than the whole conversation, if it is.
    pub caveat: Option<String>,
}

/// The conversation of a chat, from its log on disk.
pub fn read_chat(home: &Path, chat_id: &str) -> Result<Read, String> {
    let dir = crate::chat::log::chat_dir(home, chat_id).ok_or("invalid chat id")?;
    let envelopes = crate::chat::log::read_envelopes(&dir)
        .map_err(|error| format!("Cannot read the chat's event log: {error}"))?;
    let mut transcript = Transcript::default();
    for envelope in &envelopes {
        transcript.apply(&envelope.event);
    }
    let mut entries = Vec::new();
    for item in &transcript.items {
        if let Some(entry) = chat_entry(&item.body, item.status) {
            push(&mut entries, entry);
        }
    }
    Ok(Read {
        body: Body::Conversation(entries),
        model: transcript.info.and_then(|info| info.model),
        origin: "the chat's event log".into(),
        caveat: None,
    })
}

fn chat_entry(body: &ItemBody, status: ItemStatus) -> Option<Entry> {
    let declined = matches!(status, ItemStatus::Declined);
    let failed = matches!(status, ItemStatus::Failed | ItemStatus::Interrupted) || declined;
    let ended = |name: &str| {
        if declined {
            format!("{name} (declined)")
        } else {
            name.to_owned()
        }
    };
    Some(match body {
        ItemBody::UserMessage { text } => Entry::User(text.clone()),
        ItemBody::AgentMessage { text } => Entry::Agent(text.clone()),
        ItemBody::Reasoning { .. } => return None,
        ItemBody::Plan { steps, .. } => Entry::Checklist {
            title: "Plan",
            items: steps
                .iter()
                .map(|step| (mark(step.status), step.text.clone()))
                .collect(),
        },
        ItemBody::Command {
            command,
            output,
            exit_code,
            ..
        } => Entry::Command {
            command: ended(command),
            output: output.clone(),
            exit_code: exit_code.or(failed.then_some(1)),
        },
        ItemBody::FileChange { changes } if failed => Entry::Tool {
            // A change that was refused or did not happen changed no file.
            name: ended("file changes"),
            detail: changes
                .iter()
                .map(|change| change.path.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            output: None,
            failed: true,
        },
        ItemBody::FileChange { changes } => Entry::Files(
            changes
                .iter()
                .map(|change| FileEdit {
                    path: change.path.clone(),
                    change: match change.kind {
                        ChangeKind::Add => "added",
                        ChangeKind::Modify => "edited",
                        ChangeKind::Delete => "deleted",
                        ChangeKind::Rename => "renamed",
                    },
                })
                .collect(),
        ),
        ItemBody::ToolCall {
            server,
            tool,
            input,
            output,
        } => {
            let name = match server {
                Some(server) => format!("{server}/{tool}"),
                None => tool.clone(),
            };
            Entry::Tool {
                name: ended(&name),
                detail: input_detail(input),
                output: output.clone().filter(|_| failed || is_report(tool)),
                failed,
            }
        }
        ItemBody::WebSearch { query } => Entry::Search(query.clone()),
        ItemBody::Todo { items } => Entry::Checklist {
            title: "Todo list",
            items: items
                .iter()
                .map(|item| (mark(item.status), item.text.clone()))
                .collect(),
        },
        ItemBody::Compaction => Entry::Note("The context was compacted here.".into()),
        ItemBody::Notice { level, text } => match level {
            NoticeLevel::Info => return None,
            NoticeLevel::Warning => Entry::Note(format!("Warning: {text}")),
            NoticeLevel::Error => Entry::Note(format!("Error: {text}")),
        },
    })
}

fn mark(status: StepStatus) -> Mark {
    match status {
        StepStatus::Pending => Mark::Pending,
        StepStatus::InProgress => Mark::Active,
        StepStatus::Completed => Mark::Done,
    }
}

/// The tools whose output is a report worth a few lines: what a delegated agent found.
fn is_report(tool: &str) -> bool {
    matches!(tool, "Task" | "Agent")
}

/// What a tool was asked, in a few words: the first of the usual fields that is there.
pub(super) fn input_detail(input: &Value) -> String {
    if let Some(text) = input.as_str() {
        return super::document::clip(text.trim(), 160);
    }
    for key in [
        "file_path",
        "path",
        "pattern",
        "url",
        "query",
        "description",
        "command",
        "cmd",
        "prompt",
    ] {
        if let Some(text) = input.get(key).and_then(Value::as_str) {
            return super::document::clip(text.trim(), 160);
        }
    }
    match input {
        Value::Null => String::new(),
        Value::Object(map) if map.is_empty() => String::new(),
        other => super::document::clip(&other.to_string(), 160),
    }
}

/// Adds an entry, folding a run of file changes into one entry so that thirty edits to
/// one file read as a line, not a page.
pub(super) fn push(entries: &mut Vec<Entry>, entry: Entry) {
    if let (Some(Entry::Files(before)), Entry::Files(more)) = (entries.last_mut(), &entry) {
        for file in more {
            if !before.contains(file) {
                before.push(file.clone());
            }
        }
        return;
    }
    entries.push(entry);
}

// ---- Terminals ---------------------------------------------------------------------------

/// The conversation of a terminal shell, from the best place there is for what runs in it.
/// A Codex or Claude whose conversation file cannot be found falls back to the scrollback,
/// and the caveat says so.
pub fn read_shell(manager: &SessionManager, shell: &ShellSession) -> Result<Read, String> {
    let located = match shell.harness {
        Some(HarnessKind::Codex) => crate::activity::bound_rollout(manager.state_home(), shell)
            .ok_or("RiWork cannot find this terminal's Codex conversation (it learns which one after the first turn)")
            .map(|path| (path, true)),
        Some(HarnessKind::Claude) => claude::locate(manager.state_home(), shell)
            .ok_or("RiWork cannot find this terminal's Claude session (it learns which one after the first turn)")
            .map(|path| (path, false)),
        _ => Err(""),
    };
    match located {
        Ok((path, true)) => codex::read(&path),
        Ok((path, false)) => claude::read(&path),
        Err(reason) => {
            let mut read = read_scrollback(manager, shell)?;
            if !reason.is_empty() {
                read.caveat = Some(format!("{reason}, so the terminal's scrollback is used."));
            }
            Ok(read)
        }
    }
}

fn read_scrollback(manager: &SessionManager, shell: &ShellSession) -> Result<Read, String> {
    let text = manager.capture(&shell.id, SCROLLBACK_LINES)?;
    Ok(Read {
        body: Body::Scrollback(text),
        model: None,
        origin: "the terminal's scrollback".into(),
        caveat: None,
    })
}

// ---- JSON lines ------------------------------------------------------------------------

/// Calls `each` with every record of a JSON lines file, oldest first, and returns how many
/// bytes at the start of the file were not read (it is read from its end when it is bigger
/// than `MAX_READ`). A line that is not JSON, or longer than `MAX_LINE`, is skipped.
pub(super) fn read_records(path: &Path, each: impl FnMut(Value)) -> Result<u64, String> {
    read_records_within(path, MAX_READ, MAX_LINE, each)
}

fn read_records_within(
    path: &Path,
    max_read: u64,
    max_line: usize,
    mut each: impl FnMut(Value),
) -> Result<u64, String> {
    let fail = |error: std::io::Error| format!("Cannot read {}: {error}", path.display());
    let mut file = File::open(path).map_err(fail)?;
    let length = file.metadata().map_err(fail)?.len();
    let start = length.saturating_sub(max_read);
    file.seek(SeekFrom::Start(start)).map_err(fail)?;
    let mut reader = BufReader::with_capacity(256 * 1024, file);
    let mut line = Vec::new();
    if start > 0 {
        // Landed inside a line: the rest of it is not a record.
        reader.read_until(b'\n', &mut line).map_err(fail)?;
    }
    loop {
        line.clear();
        let read = (&mut reader)
            .take((max_line as u64).saturating_add(1))
            .read_until(b'\n', &mut line)
            .map_err(fail)?;
        if read == 0 {
            break;
        }
        if line.len() > max_line && !line.ends_with(b"\n") {
            // Too long: drop the rest of it.
            let mut rest = Vec::new();
            reader.read_until(b'\n', &mut rest).map_err(fail)?;
            continue;
        }
        if let Ok(value) = serde_json::from_slice::<Value>(&line) {
            each(value);
        }
    }
    Ok(start)
}

/// The text of `value` when it is a string, or else of its parts: a message's content is
/// a string in one version and a list of blocks with a `text` in another.
pub(super) fn text_of(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| match part {
                Value::String(text) => Some(text.clone()),
                other => other.get("text").and_then(Value::as_str).map(str::to_owned),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// The folder Claude keeps a working directory's sessions in: every character that is not
/// a letter or a digit becomes a dash.
pub(super) fn claude_folder(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Claude's configuration folder: the one the environment names, else `~/.claude`.
pub(super) fn claude_config_dir() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".claude")))
}

#[cfg(test)]
mod tests;
