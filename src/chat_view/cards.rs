//! What a transcript item looks like in the list, before any drawing: the one-line head of
//! each card, its status badge, and the text a card holds shortened and cleaned for the
//! screen.

use crate::chat::model::{
    ChangeKind, FileChange, Item, ItemBody, ItemStatus, NoticeLevel, Step, StepStatus,
};
use serde_json::Value;

/// How loudly a badge or a line speaks; the view picks the theme color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Muted,
    Accent,
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Badge {
    pub label: String,
    pub tone: Tone,
    /// Something is still going on, so the badge pulses.
    pub live: bool,
}

impl Badge {
    fn new(label: impl Into<String>, tone: Tone) -> Self {
        Self {
            label: label.into(),
            tone,
            live: false,
        }
    }
}

/// The first line of a card: what it is, a second muted piece of it, and a badge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Head {
    pub title: String,
    pub subtitle: Option<String>,
    pub badge: Option<Badge>,
    /// There is a body to show or hide.
    pub expandable: bool,
}

/// Lines added and removed, per file of a file change, in order.
pub type FileStats = Vec<(usize, usize)>;

/// A command's longest title; longer ones are cut with an ellipsis.
const TITLE_CHARS: usize = 200;

pub fn head(item: &Item, stats: Option<&FileStats>) -> Option<Head> {
    let status = item.status;
    Some(match &item.body {
        ItemBody::Command {
            command,
            cwd,
            output,
            exit_code,
        } => Head {
            title: first_line(command),
            subtitle: cwd.as_deref().map(short_path),
            badge: command_badge(status, *exit_code),
            expandable: !output.trim().is_empty() || command.trim().contains('\n'),
        },
        ItemBody::FileChange { changes } => Head {
            title: file_change_title(changes),
            subtitle: stats
                .map(|stats| change_counts(stats))
                .filter(|s| !s.is_empty()),
            badge: status_badge(status),
            expandable: changes.iter().any(|change| change.diff.is_some()) || changes.len() > 1,
        },
        ItemBody::ToolCall {
            server,
            tool,
            input,
            output,
        } => {
            let (server, tool) = tool_name(server.as_deref(), tool);
            Head {
                title: match server {
                    Some(server) => format!("{server} · {tool}"),
                    None => tool.clone(),
                },
                subtitle: Some(input_summary(input)).filter(|s| !s.is_empty()),
                badge: status_badge(status),
                expandable: !input.is_null() || output.as_ref().is_some_and(|o| !o.is_empty()),
            }
        }
        ItemBody::Plan { steps, .. } => Head {
            title: format!("Plan · {}", progress(steps)),
            // The explanation is the first line of the card's body.
            subtitle: None,
            badge: None,
            expandable: false,
        },
        ItemBody::Todo { items } => Head {
            title: format!("Todo · {}", progress(items)),
            subtitle: None,
            badge: None,
            expandable: false,
        },
        ItemBody::WebSearch { query } => Head {
            title: format!("Web search · {}", first_line(query)),
            subtitle: None,
            badge: status_badge(status),
            expandable: false,
        },
        ItemBody::Reasoning { text } => Head {
            title: if status == ItemStatus::InProgress {
                "Thinking…".to_owned()
            } else {
                "Thought".to_owned()
            },
            subtitle: None,
            badge: None,
            expandable: !text.trim().is_empty(),
        },
        ItemBody::UserMessage { .. }
        | ItemBody::AgentMessage { .. }
        | ItemBody::Compaction
        | ItemBody::Notice { .. } => return None,
    })
}

fn command_badge(status: ItemStatus, exit_code: Option<i32>) -> Option<Badge> {
    match (status, exit_code) {
        (ItemStatus::Completed, Some(0)) => Some(Badge::new("exit 0", Tone::Muted)),
        (ItemStatus::Completed | ItemStatus::Failed, Some(code)) if code != 0 => {
            Some(Badge::new(format!("exit {code}"), Tone::Error))
        }
        _ => status_badge(status),
    }
}

/// The badge a status has on its own. A finished item that went well has none.
pub fn status_badge(status: ItemStatus) -> Option<Badge> {
    match status {
        ItemStatus::InProgress => Some(Badge {
            label: "running".into(),
            tone: Tone::Accent,
            live: true,
        }),
        ItemStatus::Completed => None,
        ItemStatus::Failed => Some(Badge::new("failed", Tone::Error)),
        ItemStatus::Declined => Some(Badge::new("declined", Tone::Warning)),
        ItemStatus::Interrupted => Some(Badge::new("interrupted", Tone::Warning)),
    }
}

/// The tone of a notice line.
pub fn notice_tone(level: NoticeLevel) -> Tone {
    match level {
        NoticeLevel::Info => Tone::Muted,
        NoticeLevel::Warning => Tone::Warning,
        NoticeLevel::Error => Tone::Error,
    }
}

/// Done steps out of all: `2/5 done`.
pub fn progress(steps: &[Step]) -> String {
    let done = steps
        .iter()
        .filter(|step| step.status == StepStatus::Completed)
        .count();
    format!("{done}/{} done", steps.len())
}

pub fn step_mark(status: StepStatus) -> &'static str {
    match status {
        StepStatus::Pending => "○",
        StepStatus::InProgress => "◐",
        StepStatus::Completed => "✓",
    }
}

/// The letter on a file's badge and the word for its tooltip.
pub fn change_badge(kind: ChangeKind) -> (&'static str, &'static str) {
    match kind {
        ChangeKind::Add => ("A", "added"),
        ChangeKind::Modify => ("M", "modified"),
        ChangeKind::Delete => ("D", "deleted"),
        ChangeKind::Rename => ("R", "renamed"),
    }
}

fn file_change_title(changes: &[FileChange]) -> String {
    match changes {
        [] => "No file changes".to_owned(),
        [one] => format!("{} {}", verb(one.kind), one.path),
        many => {
            let kinds = many.iter().map(|change| change.kind).collect::<Vec<_>>();
            let verb = if kinds.iter().all(|kind| *kind == kinds[0]) {
                verb(kinds[0])
            } else {
                "Changed"
            };
            format!("{verb} {} files", many.len())
        }
    }
}

fn verb(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Add => "Added",
        ChangeKind::Modify => "Edited",
        ChangeKind::Delete => "Deleted",
        ChangeKind::Rename => "Renamed",
    }
}

/// `+12 −3` for the lines of all files together.
fn change_counts(stats: &[(usize, usize)]) -> String {
    let (added, removed) = stats
        .iter()
        .fold((0, 0), |(a, r), (added, removed)| (a + added, r + removed));
    match (added, removed) {
        (0, 0) => String::new(),
        (added, 0) => format!("+{added}"),
        (0, removed) => format!("−{removed}"),
        (added, removed) => format!("+{added} −{removed}"),
    }
}

/// The server and the tool to show. Claude names a tool of an MCP server
/// `mcp__server__tool`.
fn tool_name(server: Option<&str>, tool: &str) -> (Option<String>, String) {
    if let Some(rest) = tool.strip_prefix("mcp__")
        && let Some((server, tool)) = rest.split_once("__")
    {
        return (Some(server.to_owned()), tool.to_owned());
    }
    (server.map(str::to_owned), tool.to_owned())
}

/// The input of a tool call in a few words: the first of the usual arguments that is
/// there, else the arguments as JSON.
pub fn input_summary(input: &Value) -> String {
    const USUAL: [&str; 11] = [
        "command",
        "file_path",
        "path",
        "pattern",
        "query",
        "url",
        "description",
        "prompt",
        "text",
        "name",
        "id",
    ];
    let summary = match input {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Object(fields) => USUAL
            .iter()
            .find_map(|key| fields.get(*key).and_then(Value::as_str))
            .map(str::to_owned)
            .unwrap_or_else(|| input.to_string()),
        other => other.to_string(),
    };
    let line = first_line(&summary);
    if summary.trim().lines().count() > 1 {
        format!("{line} …")
    } else {
        line
    }
}

/// A tool's input as indented JSON, for the expanded card.
pub fn input_pretty(input: &Value) -> String {
    match input {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
    }
}

/// The first line of `text`, trimmed and cut to a title's length.
pub fn first_line(text: &str) -> String {
    let line = text.trim().lines().next().unwrap_or_default().trim();
    if line.chars().count() > TITLE_CHARS {
        let cut = line.chars().take(TITLE_CHARS).collect::<String>();
        format!("{cut}…")
    } else {
        line.to_owned()
    }
}

/// A folder's last two parts: `~/work/app` is `work/app`.
fn short_path(path: &str) -> String {
    let parts = path
        .trim_end_matches('/')
        .rsplit('/')
        .take(2)
        .collect::<Vec<_>>();
    parts.into_iter().rev().collect::<Vec<_>>().join("/")
}

/// Command output as it is drawn: terminal escapes gone, a carriage return back to the
/// start of its line as a terminal shows it (so a progress bar is its last state), tabs
/// spread.
pub fn clean_output(raw: &str) -> String {
    let mut lines = Vec::new();
    for line in raw.split('\n') {
        let shown = line
            .rsplit('\r')
            .find(|part| !part.is_empty())
            .unwrap_or("");
        lines.push(without_sgr(&crate::sgr::keep_sgr_only(shown)).replace('\t', "    "));
    }
    lines.join("\n")
}

/// `text` without the `ESC [ … m` sequences `sgr::keep_sgr_only` leaves in.
fn without_sgr(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            for skipped in chars.by_ref() {
                if skipped == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The last lines of `text`, at most `max_lines` and `max_bytes`, and how many lines before
/// them were left out. Output is read from its end: that is where a build says why it
/// failed.
pub fn tail(text: &str, max_lines: usize, max_bytes: usize) -> (&str, usize) {
    let text = text.trim_end_matches('\n');
    let line_start = |before: usize| text[..before].rfind('\n').map_or(0, |at| at + 1);
    let mut start = line_start(text.len());
    let mut lines = 1;
    while start > 0 && lines < max_lines {
        let previous = line_start(start - 1);
        if text.len() - previous > max_bytes {
            break;
        }
        start = previous;
        lines += 1;
    }
    // A last line that is longer than the limit alone is cut from its front.
    let mut cut_line = 0;
    if text.len() - start > max_bytes {
        start = text.len() - max_bytes;
        while !text.is_char_boundary(start) {
            start += 1;
        }
        cut_line = 1;
    }
    (
        &text[start..],
        text[..start].matches('\n').count() + cut_line,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn output_loses_escapes_and_keeps_the_last_state_of_a_progress_line() {
        assert_eq!(
            clean_output("\u{1b}[1;32m   Compiling\u{1b}[0m riwork\n"),
            "   Compiling riwork\n"
        );
        assert_eq!(clean_output("10%\r50%\r100%\ndone"), "100%\ndone");
        assert_eq!(clean_output("a\r\nb"), "a\nb");
        assert_eq!(clean_output("a\tb"), "a    b");
        assert_eq!(clean_output("\u{1b}]0;title\u{7}text"), "text");
    }

    #[test]
    fn a_long_output_is_read_from_its_end() {
        let lines: String = (1..=500).map(|n| format!("line {n}\n")).collect();
        let (shown, hidden) = tail(&lines, 200, 1 << 20);
        assert_eq!(shown.lines().count(), 200);
        assert!(shown.starts_with("line 301\n") && shown.ends_with("line 500"));
        assert_eq!(hidden, 300);
        // Short output is whole.
        assert_eq!(tail("a\nb\n", 200, 1 << 20), ("a\nb", 0));
        // The byte limit cuts lines too, and a single long line from its front.
        let (shown, hidden) = tail(&lines, 1000, 100);
        assert!(
            shown.len() <= 100 && hidden > 400,
            "{} {hidden}",
            shown.len()
        );
        let accents = "é".repeat(300);
        let (shown, hidden) = tail(&accents, 10, 100);
        assert!(shown.len() <= 100 && shown.chars().all(|c| c == 'é'));
        assert_eq!(hidden, 1);
        assert_eq!(tail("", 10, 10), ("", 0));
    }
}
