//! The readers against conversations as the agents write them: a chat's log built from
//! its own events, a Codex rollout and a Claude transcript in `testdata`, and the
//! scrollback of a real tmux pane.

use std::{fs, path::Path};

use uuid::Uuid;

use super::*;
use crate::{
    chat::{
        model::{ChatEvent, Delta, FileChange, Item, ItemBody, ItemStatus, Step},
        testing::short_home,
    },
    handoff::{
        document::{Entry, FileEdit, Mark},
        testing::{Tmux, chat_info, shell_with, testdata, write_chat},
    },
};

fn entries(read: &Read) -> &[Entry] {
    match &read.body {
        Body::Conversation(entries) => entries,
        other => panic!("not a conversation: {other:?}"),
    }
}

fn edit(path: &str, change: &'static str) -> FileEdit {
    FileEdit {
        path: path.into(),
        change,
    }
}

// ---- A chat ------------------------------------------------------------------------------

fn item(id: &str, status: ItemStatus, body: ItemBody) -> Item {
    Item {
        presentation: Default::default(),
        id: id.into(),
        turn_id: Some("t1".into()),
        status,
        body,
    }
}

#[test]
fn a_chats_log_becomes_its_messages_and_short_accounts_of_its_tools() {
    let home = short_home();
    let started = |item: Item| ChatEvent::ItemStarted { item };
    let completed = |item: Item| ChatEvent::ItemCompleted { item };
    let command = |output: &str, exit_code: Option<i32>, status: ItemStatus| {
        item(
            "cmd",
            status,
            ItemBody::Command {
                command: "cargo test".into(),
                cwd: None,
                output: output.into(),
                exit_code,
            },
        )
    };
    let info = chat_info(Some("gpt-5"));
    let id = write_chat(
        &home,
        info.clone(),
        vec![
            ChatEvent::Info { info },
            ChatEvent::TurnStarted {
                turn_id: "t1".into(),
            },
            started(item(
                "u1",
                ItemStatus::Completed,
                ItemBody::UserMessage {
                    text: "Fix the build.".into(),
                },
            )),
            started(item(
                "think",
                ItemStatus::Completed,
                ItemBody::Reasoning {
                    text: "hidden".into(),
                },
            )),
            // An agent message that streamed in pieces.
            started(item(
                "a1",
                ItemStatus::InProgress,
                ItemBody::AgentMessage {
                    text: String::new(),
                },
            )),
            ChatEvent::ItemDelta {
                item_id: "a1".into(),
                delta: Delta::Text("Running the ".into()),
            },
            ChatEvent::ItemDelta {
                item_id: "a1".into(),
                delta: Delta::Text("tests.".into()),
            },
            // A command whose output arrived as deltas and was never completed.
            started(command("", None, ItemStatus::InProgress)),
            ChatEvent::ItemDelta {
                item_id: "cmd".into(),
                delta: Delta::Output("1 failed\n".into()),
            },
            completed(command("1 failed\n", Some(1), ItemStatus::Completed)),
            completed(item(
                "files",
                ItemStatus::Completed,
                ItemBody::FileChange {
                    changes: vec![
                        FileChange {
                            path: "src/lib.rs".into(),
                            kind: crate::chat::model::ChangeKind::Modify,
                            diff: Some("@@ big diff @@".into()),
                        },
                        FileChange {
                            path: "src/new.rs".into(),
                            kind: crate::chat::model::ChangeKind::Add,
                            diff: None,
                        },
                    ],
                },
            )),
            completed(item(
                "tool",
                ItemStatus::Completed,
                ItemBody::ToolCall {
                    server: Some("docs".into()),
                    tool: "search".into(),
                    input: serde_json::json!({"query": "borrow checker"}),
                    output: Some("a long result that is not kept".into()),
                },
            )),
            completed(item(
                "denied",
                ItemStatus::Declined,
                ItemBody::ToolCall {
                    server: None,
                    tool: "Bash".into(),
                    input: serde_json::json!({"command": "rm -rf build"}),
                    output: None,
                },
            )),
            completed(item(
                "refused",
                ItemStatus::Declined,
                ItemBody::FileChange {
                    changes: vec![FileChange {
                        path: "src/secret.rs".into(),
                        kind: crate::chat::model::ChangeKind::Modify,
                        diff: None,
                    }],
                },
            )),
            completed(item(
                "todo",
                ItemStatus::Completed,
                ItemBody::Todo {
                    items: vec![
                        Step {
                            text: "fix".into(),
                            status: crate::chat::model::StepStatus::Completed,
                        },
                        Step {
                            text: "ship".into(),
                            status: crate::chat::model::StepStatus::Pending,
                        },
                    ],
                },
            )),
            completed(item("compact", ItemStatus::Completed, ItemBody::Compaction)),
            completed(item(
                "info",
                ItemStatus::Completed,
                ItemBody::notice(
                    crate::chat::model::NoticeLevel::Info,
                    "not worth a line",
                    None,
                ),
            )),
            completed(item(
                "warn",
                ItemStatus::Completed,
                ItemBody::notice(
                    crate::chat::model::NoticeLevel::Warning,
                    "rate limited",
                    None,
                ),
            )),
            completed(item(
                "a2",
                ItemStatus::Completed,
                ItemBody::AgentMessage {
                    text: "One test still fails.".into(),
                },
            )),
        ],
    );
    let read = read_chat(&home, &id).unwrap();
    assert_eq!(read.model.as_deref(), Some("gpt-5"));
    assert_eq!(
        entries(&read),
        [
            Entry::User("Fix the build.".into()),
            Entry::Agent("Running the tests.".into()),
            Entry::Command {
                command: "cargo test".into(),
                output: "1 failed\n".into(),
                exit_code: Some(1),
            },
            Entry::Files(vec![
                edit("src/lib.rs", "edited"),
                edit("src/new.rs", "added")
            ]),
            Entry::Tool {
                name: "docs/search".into(),
                detail: "borrow checker".into(),
                output: None,
                failed: false,
            },
            Entry::Tool {
                name: "Bash (declined)".into(),
                detail: "rm -rf build".into(),
                output: None,
                failed: true,
            },
            Entry::Tool {
                name: "file changes (declined)".into(),
                detail: "src/secret.rs".into(),
                output: None,
                failed: true,
            },
            Entry::Checklist {
                title: "Todo list",
                items: vec![(Mark::Done, "fix".into()), (Mark::Pending, "ship".into())],
            },
            Entry::Note("The context was compacted here.".into()),
            Entry::Note("Warning: rate limited".into()),
            Entry::Agent("One test still fails.".into()),
        ]
    );
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn a_chat_that_is_not_there_is_an_error_and_an_id_that_is_not_one_cannot_reach_a_path() {
    let home = short_home();
    assert!(read_chat(&home, &Uuid::new_v4().to_string()).is_err());
    assert_eq!(
        read_chat(&home, "../../etc").err().as_deref(),
        Some("invalid chat id")
    );
    fs::remove_dir_all(home).unwrap();
}

// ---- A Codex rollout -----------------------------------------------------------------------

#[test]
fn a_codex_rollout_becomes_messages_commands_files_and_tools() {
    let read = codex::read(&testdata("codex-rollout.jsonl")).unwrap();
    assert_eq!(read.model.as_deref(), Some("gpt-5.5"));
    assert_eq!(read.caveat, None);
    let output: String = (1..=30)
        .map(|n| format!("line {n} of the build log\n"))
        .collect();
    let output = output.trim_end().to_owned();
    assert_eq!(
        entries(&read),
        [
            // The startup guidance RiWork sends is not the person's.
            Entry::User(
                "Fix the failing build in `src/lib.rs`.\nIt broke after the last merge.".into()
            ),
            Entry::Agent("I'll run the tests first to see the failure.".into()),
            Entry::Command {
                command: "cargo test --lib".into(),
                output,
                exit_code: Some(101),
            },
            // Two changes to the same file, one more file: a line.
            Entry::Files(vec![
                edit("/work/app/src/lib.rs", "edited"),
                edit("/work/app/src/new.rs", "added")
            ]),
            // What the person refused did not happen.
            Entry::Tool {
                name: "file changes (declined)".into(),
                detail: "/work/app/src/secret.rs".into(),
                output: None,
                failed: true,
            },
            Entry::Command {
                command: "rm -rf build (declined)".into(),
                output: String::new(),
                exit_code: None,
            },
            Entry::Tool {
                name: "cua-driver/list_windows".into(),
                detail: String::new(),
                output: Some("driver is not running".into()),
                failed: true,
            },
            Entry::Tool {
                name: "docs/search".into(),
                detail: "borrow checker".into(),
                output: None,
                failed: false,
            },
            Entry::Search("rust E0502 mutable borrow".into()),
            Entry::Note("The context was compacted here.".into()),
            Entry::Agent(
                "The build passes now.\n\nTwo files changed: `src/lib.rs` and `src/new.rs`.".into()
            ),
        ]
    );
}

#[test]
fn an_older_rollout_is_read_from_what_was_sent_to_the_model() {
    let read = codex::read(&testdata("codex-rollout-sent.jsonl")).unwrap();
    assert_eq!(read.model, None);
    assert_eq!(
        entries(&read),
        [
            Entry::User("List the files.".into()),
            Entry::Command {
                command: "ls".into(),
                output: "Cargo.toml\nsrc\n".into(),
                exit_code: Some(0),
            },
            Entry::Files(vec![
                edit("src/main.rs", "edited"),
                edit("src/extra.rs", "added")
            ]),
            Entry::Agent("There are two files: Cargo.toml and src.".into()),
        ]
    );
}

#[test]
fn a_rollout_that_began_before_items_and_went_on_with_them_keeps_its_older_turns() {
    let home = short_home();
    let path = home.join("mixed.jsonl");
    // The older turns as they were sent, then the newer ones as items; the messages that the
    // first item repeats are not shown twice.
    let older = fs::read_to_string(testdata("codex-rollout-sent.jsonl")).unwrap();
    let newer = fs::read_to_string(testdata("codex-rollout.jsonl")).unwrap();
    fs::write(&path, format!("{older}{newer}")).unwrap();
    let read = codex::read(&path).unwrap();
    let all = entries(&read);
    assert_eq!(all.len(), 4 + 11);
    assert_eq!(all[0], Entry::User("List the files.".into()));
    assert_eq!(
        all[3],
        Entry::Agent("There are two files: Cargo.toml and src.".into())
    );
    assert!(matches!(&all[4], Entry::User(text) if text.starts_with("Fix the failing build")));
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn a_terminals_codex_is_found_through_the_binding_of_its_pane() {
    let home = short_home();
    let codex_home = home.join("codex-home");
    let thread = "01a0fcdf-612f-7120-92e3-f06de3a52a41";
    let day = codex_home.join("sessions/2026/10/04");
    fs::create_dir_all(&day).unwrap();
    fs::copy(
        testdata("codex-rollout.jsonl"),
        day.join(format!("rollout-2026-10-04T10-00-00-{thread}.jsonl")),
    )
    .unwrap();
    let shell_id = Uuid::new_v4().to_string();
    let shell = shell_with(&shell_id, "codex", Some(&codex_home));
    let manager = SessionManager::at(home.clone()).unwrap();

    // Before the first turn binds the pane there is no rollout to find: the scrollback
    // of a pane that is not there is the only thing left, and the error says so.
    assert!(
        crate::activity::bound_rollout(&home, &shell).is_none(),
        "unbound"
    );
    crate::activity::bind_codex_thread(&home, &shell_id, thread, &codex_home).unwrap();
    let read = read_shell(&manager, &shell).unwrap();
    assert_eq!(read.origin, "the Codex rollout");
    assert_eq!(read.model.as_deref(), Some("gpt-5.5"));
    assert_eq!(entries(&read).len(), 11);
    fs::remove_dir_all(home).unwrap();
}

// ---- A Claude transcript ---------------------------------------------------------------------

#[test]
fn a_claude_transcript_becomes_messages_and_short_accounts_of_its_tools() {
    let read = claude::read(&testdata("claude-session.jsonl")).unwrap();
    assert_eq!(read.model.as_deref(), Some("claude-opus-5-5"));
    let result = format!(
        "Exit code 101\n{}",
        (1..=25)
            .map(|n| format!("test {n} ... FAILED"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert_eq!(
        entries(&read),
        [
            // A slash command reads as the command; the CLI's own notes are gone.
            Entry::User("/model opus".into()),
            Entry::User("Make the parser reject empty input.\nKeep the error message short.".into()),
            Entry::Agent("I'll look at the parser.".into()),
            Entry::Tool {
                name: "Read".into(),
                detail: "/work/app/src/parser.rs".into(),
                output: None,
                failed: false,
            },
            Entry::Files(vec![
                edit("/work/app/src/parser.rs", "edited"),
                edit("/work/app/src/empty.rs", "written"),
            ]),
            Entry::Command {
                command: "cargo test parser".into(),
                output: result,
                exit_code: Some(101),
            },
            Entry::Checklist {
                title: "Todo list",
                items: vec![
                    (Mark::Done, "Reject empty input".into()),
                    (Mark::Active, "Add a test".into()),
                    (Mark::Pending, "Update docs".into()),
                ],
            },
            // A delegated agent's report is worth its lines; its own talk is not.
            Entry::Tool {
                name: "Task".into(),
                detail: "Survey the callers".into(),
                output: Some("Two callers: main and cli.".into()),
                failed: false,
            },
            // An edit that failed changed no file.
            Entry::Tool {
                name: "Edit".into(),
                detail: "/work/app/src/other.rs".into(),
                output: Some("String to replace not found in file.".into()),
                failed: true,
            },
            Entry::Note("The context was compacted here.".into()),
            Entry::Summary("This session is being continued. Summary: the parser rejects empty input; a test remains.".into()),
            Entry::Agent("Done: the parser now rejects empty input.".into()),
        ]
    );
}

#[test]
fn a_claude_session_is_found_in_its_folder_or_in_any_other() {
    let config = short_home();
    let session = "1897c4fd-91ab-4848-94b3-1c2582c045e9";
    let own = config.join("projects/-work-app-v2");
    let other = config.join("projects/-somewhere-else");
    fs::create_dir_all(&own).unwrap();
    fs::create_dir_all(&other).unwrap();
    // The folder name is the directory with everything but letters and digits as dashes.
    assert_eq!(claude_folder(Path::new("/work/app.v2")), "-work-app-v2");
    assert_eq!(
        claude_folder(Path::new("/Users/me/Documents/riwork/My_App")),
        "-Users-me-Documents-riwork-My-App"
    );

    assert_eq!(
        claude::find(&config, Path::new("/work/app.v2"), session),
        None
    );
    fs::write(other.join(format!("{session}.jsonl")), "{}\n").unwrap();
    assert_eq!(
        claude::find(&config, Path::new("/work/app.v2"), session),
        Some(other.join(format!("{session}.jsonl")))
    );
    fs::write(own.join(format!("{session}.jsonl")), "{}\n").unwrap();
    assert_eq!(
        claude::find(&config, Path::new("/work/app.v2"), session),
        Some(own.join(format!("{session}.jsonl")))
    );
    // An id is a file name below the projects folder, never a path.
    for bad in ["", "../x", "a/b", "a.b", "..", "x\0y"] {
        assert_eq!(
            claude::find(&config, Path::new("/work/app.v2"), bad),
            None,
            "{bad:?}"
        );
    }
    fs::remove_dir_all(config).unwrap();
}

// ---- The files ---------------------------------------------------------------------------------

#[test]
fn a_file_of_records_is_read_in_order_and_a_damaged_line_is_skipped() {
    let home = short_home();
    let path = home.join("log.jsonl");
    fs::write(
        &path,
        "{\"n\":1}\nnot json\n\n{\"n\":2}\n{\"n\":3,\"pad\":\"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\"}\n{\"n\":4}",
    )
    .unwrap();
    let numbers = |max_read, max_line| {
        let mut seen = Vec::new();
        let skipped = read_records_within(&path, max_read, max_line, |record| {
            seen.push(record["n"].as_i64().unwrap());
        })
        .unwrap();
        (skipped, seen)
    };
    assert_eq!(numbers(u64::MAX, usize::MAX), (0, vec![1, 2, 3, 4]));
    // A line past the limit is dropped whole, and the next one is read.
    assert_eq!(numbers(u64::MAX, 30), (0, vec![1, 2, 4]));
    // A file past the read limit is read from its end, from a whole line on.
    let (skipped, seen) = numbers(40, usize::MAX);
    assert!(skipped > 0);
    assert_eq!(seen, vec![4]);
    fs::remove_dir_all(home).unwrap();
}

// ---- A terminal's scrollback ----------------------------------------------------------------------

#[test]
fn a_plain_shell_is_read_from_its_scrollback() {
    let Some(tmux) = Tmux::new() else {
        return;
    };
    let shell = tmux.shell(
        "for n in $(seq 1 200); do echo \"history line $n\"; done; echo scrollback-end; exec sleep 300",
        "scrollback-end",
    );
    let read = read_shell(&tmux.manager, &shell).unwrap();
    assert_eq!(read.origin, "the terminal's scrollback");
    assert_eq!(read.caveat, None);
    let Body::Scrollback(text) = &read.body else {
        panic!("{:?}", read.body);
    };
    // The screen is 30 rows; the rest came out of the history above it.
    assert!(text.contains("history line 1\n"), "{text}");
    assert!(text.contains("history line 200\n") && text.contains("scrollback-end"));
}

#[test]
fn an_agent_whose_conversation_file_is_unknown_falls_back_to_the_scrollback_and_says_so() {
    let Some(tmux) = Tmux::new() else {
        return;
    };
    let plain = tmux.shell("echo scrollback-end; exec sleep 300", "scrollback-end");
    for harness in ["codex", "claude"] {
        let mut shell = plain.clone();
        shell.harness = Some(match harness {
            "codex" => HarnessKind::Codex,
            _ => HarnessKind::Claude,
        });
        let read = read_shell(&tmux.manager, &shell).unwrap();
        assert!(matches!(read.body, Body::Scrollback(_)));
        let caveat = read.caveat.expect("a caveat");
        assert!(
            caveat.contains("so the terminal's scrollback is used"),
            "{caveat}"
        );
    }
}
