//! A chat's transcript as plain text, for what reads a chat the way it reads a
//! terminal: `riwork orchestrator output` and the MCP tool of the same name.
//!
//! The user's and the agent's messages are given whole. Every other item is one
//! line (a command with how it ended, the files a change touched, the tool a
//! call used), because a reader that wants the output of a command has the
//! command. What the chat still waits for comes last, so a reader sees at once
//! that a turn is held up.

use super::model::{ChangeKind, ItemBody, ItemStatus, NoticeLevel, StepStatus, Transcript};

/// The longest command, path list or query a one-line summary shows.
const SUMMARY_CHARS: usize = 160;

/// The transcript as lines of text, oldest first.
pub fn lines(transcript: &Transcript) -> Vec<String> {
    let mut lines = Vec::new();
    for item in &transcript.items {
        match &item.body {
            ItemBody::UserMessage { text } => message(&mut lines, "user", text),
            ItemBody::AgentMessage { text } => message(&mut lines, "agent", text),
            // The agent's thinking is not part of what it said.
            ItemBody::Reasoning { .. } => {}
            ItemBody::Plan { explanation, steps } => {
                let done = steps
                    .iter()
                    .filter(|step| step.status == StepStatus::Completed)
                    .count();
                let mut line = format!("[plan] {done} of {} steps done", steps.len());
                if let Some(explanation) = explanation.as_deref().filter(|text| !text.is_empty()) {
                    line.push_str(": ");
                    line.push_str(&summary(explanation));
                }
                lines.push(line);
            }
            ItemBody::Command {
                command, exit_code, ..
            } => lines.push(format!(
                "[command] {}{}",
                summary(command),
                match (item.status, exit_code) {
                    (ItemStatus::InProgress, _) => " (running)".to_owned(),
                    (ItemStatus::Declined, _) => " (declined)".to_owned(),
                    (ItemStatus::Interrupted, _) => " (interrupted)".to_owned(),
                    (_, Some(code)) => format!(" (exit {code})"),
                    (ItemStatus::Failed, None) => " (failed)".to_owned(),
                    (ItemStatus::Completed, None) => String::new(),
                }
            )),
            ItemBody::FileChange { changes } => {
                let files = changes
                    .iter()
                    .map(|change| {
                        let verb = match change.kind {
                            ChangeKind::Add => "add",
                            ChangeKind::Modify => "modify",
                            ChangeKind::Delete => "delete",
                            ChangeKind::Rename => "rename",
                        };
                        format!("{} ({verb})", change.path)
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                lines.push(format!(
                    "[edit] {}{}",
                    summary(&files),
                    outcome(item.status)
                ));
            }
            ItemBody::ToolCall { server, tool, .. } => lines.push(format!(
                "[tool] {}{}",
                match server {
                    Some(server) => format!("{server}.{tool}"),
                    None => tool.clone(),
                },
                outcome(item.status)
            )),
            ItemBody::WebSearch { query } => lines.push(format!("[search] {}", summary(query))),
            ItemBody::Todo { items } => {
                let done = items
                    .iter()
                    .filter(|step| step.status == StepStatus::Completed)
                    .count();
                lines.push(format!("[todo] {done} of {} items done", items.len()));
            }
            ItemBody::Compaction => lines.push("[context compacted]".to_owned()),
            ItemBody::Notice { level, text } => lines.push(format!(
                "[{}] {}",
                match level {
                    NoticeLevel::Info => "notice",
                    NoticeLevel::Warning => "warning",
                    NoticeLevel::Error => "error",
                },
                summary(text)
            )),
        }
    }
    for approval in &transcript.approvals {
        lines.push(format!(
            "[waiting for approval] {}",
            summary(&approval.title)
        ));
    }
    for question in &transcript.questions {
        for prompt in &question.questions {
            lines.push(format!(
                "[waiting for an answer] {}",
                summary(&prompt.question)
            ));
        }
    }
    lines
}

/// The last `max` lines of the transcript, one per line of output.
pub fn tail(transcript: &Transcript, max: usize) -> Vec<String> {
    let mut lines = lines(transcript);
    let keep = lines.len().saturating_sub(max);
    lines.drain(..keep);
    lines
}

/// A message: its first line is labelled, the rest are indented under it.
fn message(lines: &mut Vec<String>, label: &str, text: &str) {
    let mut parts = clean(text).lines().map(str::to_owned).collect::<Vec<_>>();
    if parts.is_empty() {
        parts.push(String::new());
    }
    for (index, part) in parts.into_iter().enumerate() {
        lines.push(match (index, part.is_empty()) {
            (0, _) => format!("{label}: {part}"),
            (_, true) => part,
            (_, false) => format!("  {part}"),
        });
    }
}

/// How an item that did not simply succeed ended.
fn outcome(status: ItemStatus) -> &'static str {
    match status {
        ItemStatus::InProgress => " (running)",
        ItemStatus::Completed => "",
        ItemStatus::Failed => " (failed)",
        ItemStatus::Declined => " (declined)",
        ItemStatus::Interrupted => " (interrupted)",
    }
}

/// One line, cut to `SUMMARY_CHARS`.
fn summary(text: &str) -> String {
    let one_line = clean(text).split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= SUMMARY_CHARS {
        return one_line;
    }
    let mut cut: String = one_line.chars().take(SUMMARY_CHARS).collect();
    cut.push('…');
    cut
}

/// The text without control characters other than line breaks and tabs: what an
/// agent says can carry terminal escape sequences, and this is printed.
fn clean(text: &str) -> String {
    text.replace("\r\n", "\n")
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::model::{
        Approval, ApprovalKind, ChatEvent, Decision, FileChange, Item, Question, QuestionPrompt,
        Step,
    };

    fn transcript(bodies: Vec<(ItemStatus, ItemBody)>) -> Transcript {
        let mut transcript = Transcript::default();
        for (number, (status, body)) in bodies.into_iter().enumerate() {
            transcript.apply(&ChatEvent::ItemCompleted {
                item: Item {
                    presentation: Default::default(),
                    id: format!("item-{number}"),
                    turn_id: None,
                    status,
                    body,
                },
            });
        }
        transcript
    }

    #[test]
    fn messages_are_given_whole_and_other_items_in_one_line_each() {
        let transcript = transcript(vec![
            (
                ItemStatus::Completed,
                ItemBody::UserMessage {
                    text: "Read the skill.".into(),
                },
            ),
            (
                ItemStatus::Completed,
                ItemBody::Reasoning {
                    text: "thinking about it".into(),
                },
            ),
            (
                ItemStatus::Completed,
                ItemBody::Command {
                    command: "ls -la".into(),
                    cwd: None,
                    output: "total 0\nfile\n".into(),
                    exit_code: Some(0),
                },
            ),
            (
                ItemStatus::Failed,
                ItemBody::Command {
                    command: "false".into(),
                    cwd: None,
                    output: String::new(),
                    exit_code: Some(1),
                },
            ),
            (
                ItemStatus::InProgress,
                ItemBody::Command {
                    command: "sleep 9".into(),
                    cwd: None,
                    output: String::new(),
                    exit_code: None,
                },
            ),
            (
                ItemStatus::Completed,
                ItemBody::FileChange {
                    changes: vec![
                        FileChange {
                            path: "a.rs".into(),
                            kind: ChangeKind::Modify,
                            diff: Some("secret diff".into()),
                        },
                        FileChange {
                            path: "b.rs".into(),
                            kind: ChangeKind::Add,
                            diff: None,
                        },
                    ],
                },
            ),
            (
                ItemStatus::Completed,
                ItemBody::ToolCall {
                    server: Some("riwork".into()),
                    tool: "riwork_task_list".into(),
                    input: serde_json::json!({"project_id": "p"}),
                    output: Some("long output".into()),
                },
            ),
            (
                ItemStatus::Completed,
                ItemBody::AgentMessage {
                    text: "Ready.\nWaiting for an objective.".into(),
                },
            ),
        ]);
        assert_eq!(
            lines(&transcript),
            [
                "user: Read the skill.",
                "[command] ls -la (exit 0)",
                "[command] false (exit 1)",
                "[command] sleep 9 (running)",
                "[edit] a.rs (modify), b.rs (add)",
                "[tool] riwork.riwork_task_list",
                "agent: Ready.",
                "  Waiting for an objective.",
            ]
        );
    }

    #[test]
    fn the_tail_keeps_the_last_lines_and_what_the_chat_waits_for_comes_last() {
        let mut transcript = transcript(vec![
            (
                ItemStatus::Completed,
                ItemBody::UserMessage { text: "one".into() },
            ),
            (
                ItemStatus::Completed,
                ItemBody::AgentMessage { text: "two".into() },
            ),
            (
                ItemStatus::Completed,
                ItemBody::WebSearch {
                    query: "rust   async".into(),
                },
            ),
        ]);
        transcript.apply(&ChatEvent::ApprovalRequested {
            approval: Approval {
                request_id: "r1".into(),
                item_id: None,
                kind: ApprovalKind::Command,
                title: "rm -rf build".into(),
                detail: String::new(),
                choices: vec![Decision::Accept],
            },
        });
        transcript.apply(&ChatEvent::QuestionRequested {
            question: Question {
                request_id: "q1".into(),
                questions: vec![QuestionPrompt {
                    header: None,
                    question: "Which branch?".into(),
                    options: Vec::new(),
                    multi_select: false,
                }],
            },
        });
        assert_eq!(
            tail(&transcript, 3),
            [
                "[search] rust async",
                "[waiting for approval] rm -rf build",
                "[waiting for an answer] Which branch?",
            ]
        );
        assert_eq!(tail(&transcript, 100).len(), 5);
        assert!(tail(&transcript, 0).is_empty());
    }

    #[test]
    fn control_characters_never_reach_the_output_and_long_summaries_are_cut() {
        let transcript = transcript(vec![
            (
                ItemStatus::Completed,
                ItemBody::AgentMessage {
                    text: "red \u{1b}[31mtext\u{1b}[0m\r\nnext".into(),
                },
            ),
            (
                ItemStatus::Completed,
                ItemBody::Command {
                    command: "echo ".to_owned() + &"x".repeat(400),
                    cwd: None,
                    output: String::new(),
                    exit_code: Some(0),
                },
            ),
            (
                ItemStatus::Completed,
                ItemBody::Todo {
                    items: vec![
                        Step {
                            text: "a".into(),
                            status: StepStatus::Completed,
                        },
                        Step {
                            text: "b".into(),
                            status: StepStatus::Pending,
                        },
                    ],
                },
            ),
        ]);
        let lines = lines(&transcript);
        assert_eq!(lines[0], "agent: red [31mtext[0m");
        assert_eq!(lines[1], "  next");
        assert!(
            lines[2].chars().count() < 200 && lines[2].contains('…'),
            "{}",
            lines[2]
        );
        assert_eq!(lines[3], "[todo] 1 of 2 items done");
        assert!(lines.iter().all(|line| !line.contains('\u{1b}')));
    }
}
