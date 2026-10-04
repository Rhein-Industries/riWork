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
    use serde_json::json;

    fn item(status: ItemStatus, body: ItemBody) -> Item {
        Item {
            id: "i".into(),
            turn_id: None,
            status,
            body,
        }
    }

    fn command(status: ItemStatus, exit_code: Option<i32>, output: &str) -> Item {
        item(
            status,
            ItemBody::Command {
                command: "cargo test --all".into(),
                cwd: Some("/Users/me/work/app".into()),
                output: output.into(),
                exit_code,
            },
        )
    }

    #[test]
    fn a_command_card_shows_the_command_the_folder_and_how_it_ended() {
        let running = head(&command(ItemStatus::InProgress, None, ""), None).unwrap();
        assert_eq!(running.title, "cargo test --all");
        assert_eq!(running.subtitle.as_deref(), Some("work/app"));
        assert_eq!(
            running.badge,
            Some(Badge {
                label: "running".into(),
                tone: Tone::Accent,
                live: true
            })
        );
        assert!(!running.expandable, "nothing to show yet");

        let ok = head(&command(ItemStatus::Completed, Some(0), "ok\n"), None).unwrap();
        assert_eq!(ok.badge, Some(Badge::new("exit 0", Tone::Muted)));
        assert!(ok.expandable);

        let failed = head(&command(ItemStatus::Completed, Some(101), "boom"), None).unwrap();
        assert_eq!(failed.badge, Some(Badge::new("exit 101", Tone::Error)));

        // Refused at the approval prompt, or cut short, there is no exit code to show.
        let declined = head(&command(ItemStatus::Declined, None, ""), None).unwrap();
        assert_eq!(declined.badge, Some(Badge::new("declined", Tone::Warning)));
        let stopped = head(&command(ItemStatus::Interrupted, None, "x"), None).unwrap();
        assert_eq!(
            stopped.badge,
            Some(Badge::new("interrupted", Tone::Warning))
        );
        let broken = head(&command(ItemStatus::Failed, None, ""), None).unwrap();
        assert_eq!(broken.badge, Some(Badge::new("failed", Tone::Error)));
    }

    #[test]
    fn a_long_or_multi_line_command_gets_a_one_line_title() {
        let long = "x".repeat(500);
        let card = head(
            &item(
                ItemStatus::Completed,
                ItemBody::Command {
                    command: format!("echo {long}\nsecond line"),
                    cwd: None,
                    output: String::new(),
                    exit_code: Some(0),
                },
            ),
            None,
        )
        .unwrap();
        assert_eq!(card.title.chars().count(), TITLE_CHARS + 1);
        assert!(card.title.ends_with('…') && !card.title.contains('\n'));
        assert!(card.expandable, "the rest of the command is the body");
    }

    #[test]
    fn a_file_change_card_names_the_file_or_counts_the_files() {
        let change = |path: &str, kind| FileChange {
            path: path.into(),
            kind,
            diff: Some("+x".into()),
        };
        let one = item(
            ItemStatus::Completed,
            ItemBody::FileChange {
                changes: vec![change("src/main.rs", ChangeKind::Modify)],
            },
        );
        let card = head(&one, Some(&vec![(12, 3)])).unwrap();
        assert_eq!(card.title, "Edited src/main.rs");
        assert_eq!(card.subtitle.as_deref(), Some("+12 −3"));
        assert!(card.expandable && card.badge.is_none());

        let many = item(
            ItemStatus::InProgress,
            ItemBody::FileChange {
                changes: vec![
                    change("a", ChangeKind::Add),
                    change("b", ChangeKind::Add),
                    change("c", ChangeKind::Delete),
                ],
            },
        );
        let card = head(&many, Some(&vec![(2, 0), (1, 0), (0, 4)])).unwrap();
        assert_eq!(card.title, "Changed 3 files");
        assert_eq!(card.subtitle.as_deref(), Some("+3 −4"));
        assert!(card.badge.unwrap().live);
        let same = item(
            ItemStatus::Completed,
            ItemBody::FileChange {
                changes: vec![change("a", ChangeKind::Add), change("b", ChangeKind::Add)],
            },
        );
        assert_eq!(head(&same, None).unwrap().title, "Added 2 files");
        assert_eq!(head(&same, None).unwrap().subtitle, None);
        assert_eq!(change_counts(&[(0, 0)]), "");
        assert_eq!(change_counts(&[(5, 0)]), "+5");
        assert_eq!(change_counts(&[(0, 2)]), "−2");
        assert_eq!(change_badge(ChangeKind::Rename), ("R", "renamed"));
    }

    #[test]
    fn a_tool_card_names_the_server_and_summarises_the_input() {
        let tool = |server: Option<&str>, tool: &str, input: Value, output: Option<&str>| {
            head(
                &item(
                    ItemStatus::Completed,
                    ItemBody::ToolCall {
                        server: server.map(str::to_owned),
                        tool: tool.into(),
                        input,
                        output: output.map(str::to_owned),
                    },
                ),
                None,
            )
            .unwrap()
        };
        let read = tool(
            None,
            "Read",
            json!({"file_path": "/a/b.rs", "limit": 5}),
            None,
        );
        assert_eq!(read.title, "Read");
        assert_eq!(read.subtitle.as_deref(), Some("/a/b.rs"));
        assert!(read.expandable);
        let mcp = tool(
            Some("github"),
            "search",
            json!({"query": "bug"}),
            Some("3 hits"),
        );
        assert_eq!(mcp.title, "github · search");
        assert_eq!(mcp.subtitle.as_deref(), Some("bug"));
        // Claude's own spelling of an MCP tool.
        let claude = tool(None, "mcp__cua-driver__click", json!({"x": 1}), None);
        assert_eq!(claude.title, "cua-driver · click");
        let bare = tool(None, "TodoRead", Value::Null, None);
        assert!(!bare.expandable && bare.subtitle.is_none());
    }

    #[test]
    fn tool_inputs_are_summarised_by_their_usual_argument() {
        assert_eq!(
            input_summary(&json!({"pattern": "fn main", "path": "src"})),
            "src"
        );
        assert_eq!(input_summary(&json!({"zzz": 1})), r#"{"zzz":1}"#);
        assert_eq!(input_summary(&json!("plain\nmore")), "plain …");
        assert_eq!(input_summary(&Value::Null), "");
        assert_eq!(input_summary(&json!(42)), "42");
        assert_eq!(input_pretty(&json!({"a": 1})), "{\n  \"a\": 1\n}");
        assert_eq!(input_pretty(&json!("text")), "text");
    }

    #[test]
    fn plans_and_todo_lists_count_their_done_steps() {
        let steps = vec![
            Step {
                text: "a".into(),
                status: StepStatus::Completed,
            },
            Step {
                text: "b".into(),
                status: StepStatus::InProgress,
            },
            Step {
                text: "c".into(),
                status: StepStatus::Pending,
            },
        ];
        let plan = head(
            &item(
                ItemStatus::Completed,
                ItemBody::Plan {
                    explanation: Some("Do it in order.\nMore.".into()),
                    steps: steps.clone(),
                },
            ),
            None,
        )
        .unwrap();
        assert_eq!(plan.title, "Plan · 1/3 done");
        assert_eq!(plan.subtitle, None);
        let todo = head(
            &item(ItemStatus::Completed, ItemBody::Todo { items: steps }),
            None,
        )
        .unwrap();
        assert_eq!(todo.title, "Todo · 1/3 done");
        assert_eq!(
            [
                StepStatus::Pending,
                StepStatus::InProgress,
                StepStatus::Completed
            ]
            .map(step_mark),
            ["○", "◐", "✓"]
        );
    }

    #[test]
    fn reasoning_is_thinking_until_it_is_done_and_expands_only_with_text() {
        let reasoning = |status, text: &str| {
            head(
                &item(status, ItemBody::Reasoning { text: text.into() }),
                None,
            )
            .unwrap()
        };
        assert_eq!(reasoning(ItemStatus::InProgress, "").title, "Thinking…");
        assert!(!reasoning(ItemStatus::InProgress, "  ").expandable);
        let done = reasoning(ItemStatus::Completed, "weighing options");
        assert_eq!(done.title, "Thought");
        assert!(done.expandable);
        let search = head(
            &item(
                ItemStatus::Completed,
                ItemBody::WebSearch {
                    query: "gpui list".into(),
                },
            ),
            None,
        )
        .unwrap();
        assert_eq!(search.title, "Web search · gpui list");
    }

    #[test]
    fn messages_dividers_and_notices_have_no_card_head() {
        for body in [
            ItemBody::UserMessage { text: "hi".into() },
            ItemBody::AgentMessage {
                text: "hello".into(),
            },
            ItemBody::Compaction,
            ItemBody::Notice {
                level: NoticeLevel::Info,
                text: "x".into(),
            },
        ] {
            assert_eq!(head(&item(ItemStatus::Completed, body), None), None);
        }
        assert_eq!(notice_tone(NoticeLevel::Info), Tone::Muted);
        assert_eq!(notice_tone(NoticeLevel::Warning), Tone::Warning);
        assert_eq!(notice_tone(NoticeLevel::Error), Tone::Error);
    }

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
