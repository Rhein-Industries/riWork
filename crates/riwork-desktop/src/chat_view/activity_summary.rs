//! Short, factual summaries of a turn's work. Search hits are not files read, and
//! failed or declined edits are not reported as successful file changes.
use crate::chat::model::{Item, ItemBody, ItemStatus, Transcript, TurnOutcome};
use serde_json::Value;
use std::collections::HashSet;

#[derive(Default, Debug, PartialEq, Eq)]
pub(super) struct Summary {
    read: HashSet<String>,
    changed: HashSet<String>,
    commands: usize,
    searches: usize,
    tools: usize,
    plans: bool,
    failed: usize,
    declined: usize,
    interrupted: usize,
}

impl Summary {
    pub fn label(&self) -> String {
        let mut parts = Vec::new();
        let mut count = |number, verb: &str, singular: &str, plural: &str| {
            if number > 0 {
                parts.push(format!(
                    "{verb} {number} {}",
                    if number == 1 { singular } else { plural }
                ));
            }
        };
        count(self.read.len(), "Read", "file", "files");
        count(self.changed.len(), "Changed", "file", "files");
        count(self.commands, "Ran", "command", "commands");
        count(self.searches, "Ran", "search", "searches");
        count(self.tools, "Used", "tool", "tools");
        if self.plans {
            parts.push("Updated plan".into());
        }
        if self.failed > 0 {
            parts.push(format!("{} failed", self.failed));
        }
        if self.declined > 0 {
            parts.push(format!("{} declined", self.declined));
        }
        if self.interrupted > 0 {
            parts.push(format!("{} interrupted", self.interrupted));
        }
        if parts.is_empty() {
            "Activity from this turn".into()
        } else {
            parts.join(" · ")
        }
    }

    #[cfg(test)]
    pub fn has_failure(&self) -> bool {
        self.failed > 0
    }
}

fn file_path(path: &str) -> Option<String> {
    let path = path.trim();
    if path.is_empty() || path.chars().any(char::is_control) || path.contains("://") {
        return None;
    }
    // Do not resolve `..` or touch the filesystem: these are reported paths, and
    // they may refer to files that no longer exist by the time this turn is read.
    let path = path.trim_start_matches("./");
    (!path.is_empty()).then(|| path.to_owned())
}

fn read_paths(input: &Value) -> HashSet<String> {
    let mut paths = HashSet::new();
    for key in ["file_path", "path", "filename", "file"] {
        if let Some(path) = input.get(key).and_then(Value::as_str).and_then(file_path) {
            paths.insert(path);
        }
    }
    if let Some(values) = input.get("paths").and_then(Value::as_array) {
        paths.extend(
            values
                .iter()
                .filter_map(Value::as_str)
                .filter_map(file_path),
        );
    }
    paths
}

fn summarize<'a>(items: impl Iterator<Item = &'a Item>) -> Summary {
    let mut result = Summary::default();
    for item in items {
        let work = matches!(
            item.body,
            ItemBody::Command { .. }
                | ItemBody::ToolCall { .. }
                | ItemBody::FileChange { .. }
                | ItemBody::WebSearch { .. }
                | ItemBody::Plan { .. }
                | ItemBody::Todo { .. }
        );
        if !work || crate::chat::workers::is_marker(item) {
            continue;
        }
        let failed = item.status == ItemStatus::Failed
            || matches!(item.body, ItemBody::Command { exit_code: Some(code), .. } if code != 0);
        if failed {
            result.failed += 1;
        } else if item.status == ItemStatus::Declined {
            result.declined += 1;
        } else if item.status == ItemStatus::Interrupted {
            result.interrupted += 1;
        }
        if item.status == ItemStatus::Declined || item.status == ItemStatus::InProgress {
            continue;
        }
        let succeeded = item.status == ItemStatus::Completed && !failed;
        match &item.body {
            ItemBody::Command { .. } => result.commands += 1,
            ItemBody::FileChange { changes } if succeeded => {
                result
                    .changed
                    .extend(changes.iter().filter_map(|change| file_path(&change.path)));
            }
            ItemBody::WebSearch { .. } => result.searches += 1,
            ItemBody::Plan { .. } | ItemBody::Todo { .. } if succeeded => result.plans = true,
            ItemBody::ToolCall { tool, input, .. } => {
                let name = tool
                    .rsplit("__")
                    .next()
                    .unwrap_or(tool)
                    .to_ascii_lowercase()
                    .replace('-', "_");
                match name.as_str() {
                    "read" | "read_file" | "read_text_file" | "read_multiple_files"
                        if succeeded =>
                    {
                        let paths = read_paths(input);
                        if paths.is_empty() {
                            result.tools += 1;
                        } else {
                            result.read.extend(paths);
                        }
                    }
                    "grep" | "ripgrep" | "rg" | "glob" | "search_files" | "file_search"
                    | "find_files" | "web_search" | "websearch" => result.searches += 1,
                    _ => result.tools += 1,
                }
            }
            _ => {}
        }
    }
    result
}

/// Include visible failures as well as the successful rows inside the disclosure.
/// The completion boundary prevents a late item with a reused turn id changing
/// a historical summary and keeps this work bounded to the selected turn.
pub(super) fn for_turn(
    transcript: &Transcript,
    completed: &[(String, TurnOutcome, usize)],
    turn: &str,
) -> Summary {
    let Some(position) = completed.iter().position(|(id, _, _)| id == turn) else {
        return Summary::default();
    };
    let end = completed[position].2.min(transcript.items.len());
    let start = position
        .checked_sub(1)
        .map_or(0, |previous| completed[previous].2)
        .min(end);
    summarize(
        transcript.items[start..end]
            .iter()
            .filter(|item| item.turn_id.as_deref() == Some(turn)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::model::{ChangeKind, FileChange};
    use serde_json::json;

    fn item(id: &str, body: ItemBody) -> Item {
        Item {
            started_at: None,
            ended_at: None,
            id: id.into(),
            turn_id: Some("turn".into()),
            status: ItemStatus::Completed,
            presentation: Default::default(),
            body,
        }
    }
    fn read(path: &str) -> Item {
        item(
            path,
            ItemBody::ToolCall {
                server: None,
                tool: "mcp__files__read_file".into(),
                input: json!({"path": path}),
                output: None,
            },
        )
    }
    fn change(path: &str) -> Item {
        item(
            path,
            ItemBody::FileChange {
                changes: vec![FileChange {
                    path: path.into(),
                    kind: ChangeKind::Modify,
                    diff: None,
                }],
            },
        )
    }

    #[test]
    fn repeated_paths_are_counted_once_and_search_hits_are_not_reads() {
        let items = vec![
            read("src/a.rs"),
            read("./src/a.rs"),
            read("src/b.rs"),
            change("src/a.rs"),
            change("./src/a.rs"),
            item(
                "search",
                ItemBody::ToolCall {
                    server: None,
                    tool: "Grep".into(),
                    input: json!({"path": "src"}),
                    output: Some("100 matching files".into()),
                },
            ),
        ];
        assert_eq!(
            summarize(items.iter()).label(),
            "Read 2 files · Changed 1 file · Ran 1 search"
        );
    }

    #[test]
    fn failed_changes_and_declined_commands_do_not_claim_success() {
        let mut failed = change("src/a.rs");
        failed.status = ItemStatus::Failed;
        let mut declined = item(
            "no",
            ItemBody::Command {
                command: "rm example".into(),
                cwd: None,
                output: String::new(),
                exit_code: None,
            },
        );
        declined.status = ItemStatus::Declined;
        let command = item(
            "test",
            ItemBody::Command {
                command: "cargo test".into(),
                cwd: None,
                output: String::new(),
                exit_code: Some(1),
            },
        );
        let summary = summarize([failed, declined, command].iter());
        assert_eq!(summary.label(), "Ran 1 command · 2 failed · 1 declined");
        assert!(summary.has_failure());
    }

    #[test]
    fn batches_deduplicate_paths_and_unknown_tools_remain_honest() {
        let items = vec![
            item(
                "batch",
                ItemBody::ToolCall {
                    server: None,
                    tool: "read_multiple_files".into(),
                    input: json!({"paths": ["a", "b", "a", 3]}),
                    output: None,
                },
            ),
            read("https://example.com/a"),
            item(
                "other",
                ItemBody::ToolCall {
                    server: None,
                    tool: "database_query".into(),
                    input: Value::Null,
                    output: None,
                },
            ),
        ];
        assert_eq!(
            summarize(items.iter()).label(),
            "Read 2 files · Used 2 tools"
        );
    }

    #[test]
    fn turn_boundaries_exclude_other_turns_and_late_items() {
        let mut previous = read("previous");
        previous.turn_id = Some("previous".into());
        let mut transcript = Transcript::default();
        transcript.items = vec![previous, read("current"), read("late")];
        let completed = vec![
            ("previous".into(), TurnOutcome::Completed, 1),
            ("turn".into(), TurnOutcome::Completed, 2),
        ];
        assert_eq!(
            for_turn(&transcript, &completed, "turn").label(),
            "Read 1 file"
        );
        assert_eq!(
            for_turn(&transcript, &completed, "missing").label(),
            "Activity from this turn"
        );
    }
}
