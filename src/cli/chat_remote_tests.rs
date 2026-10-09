//! `chat events`, `chat command` and the options the remote connector uses on
//! `chat list` and `chat new`, against a running chat host with fake drivers on
//! a throwaway `RIWORK_HOME`; and the page collector on its own, with a script
//! in place of the host.
use super::chat_remote::*;
use super::*;
use crate::chat::client::{Poll, socket_path};
use crate::chat::model::{
    ApprovalMode, ChatCommand, ChatEvent, ChatInfo, ChatState, Decision, Item, ItemBody,
    ItemStatus, Provider, Usage,
};
use crate::chat::testing::*;
use crate::chat::wire::Envelope;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

fn run(home: &Path, words: &[&str], json: bool) -> Result<String, String> {
    chat_client_command(
        home,
        words.iter().map(|word| (*word).to_owned()).collect(),
        json,
        &|home| Ok(socket_path(home)),
    )
}

fn run_json(home: &Path, words: &[&str]) -> Value {
    serde_json::from_str(&run(home, words, true).unwrap()).unwrap()
}

/// A host with a project called `demo` in its working directory.
fn host_with_project() -> (TestHost, crate::store::Project) {
    let host = TestHost::new();
    let project = Store::open(&host.home)
        .unwrap()
        .add_project(host.work(), Some("demo"))
        .unwrap();
    (host, project)
}

fn usage_event(tokens: u64) -> ChatEvent {
    ChatEvent::Usage {
        usage: Usage {
            input_tokens: tokens,
            ..Usage::default()
        },
    }
}

fn message_event(text: &str) -> ChatEvent {
    ChatEvent::ItemCompleted {
        item: Item {
            presentation: Default::default(),
            id: "agent-x".into(),
            turn_id: None,
            status: ItemStatus::Completed,
            body: ItemBody::AgentMessage { text: text.into() },
        },
    }
}

/// An idle chat and the number of events it has by then.
fn idle_chat(host: &TestHost) -> (ChatInfo, u64) {
    let chat = host.create(Provider::Codex);
    host.wait_for_state(&chat.id, |state| *state == ChatState::Idle);
    let count = host.log(&chat.id).len() as u64;
    (chat, count)
}

/// Emits events as the driver would and waits until the log has them.
fn emit(host: &TestHost, chat: &ChatInfo, events: Vec<ChatEvent>) -> u64 {
    let wanted = host.log(&chat.id).len() + events.len();
    for event in events {
        host.fake().emit(event);
    }
    host.wait_for_log(&chat.id, |log| log.len() >= wanted);
    wanted as u64
}

fn events(host: &TestHost, id: &str, options: &[&str]) -> Value {
    let mut words = vec!["events", id];
    words.extend_from_slice(options);
    run_json(&host.home, &words)
}

fn seqs(page: &Value) -> Vec<u64> {
    page["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["seq"].as_u64().unwrap())
        .collect()
}

// ---- list and new --------------------------------------------------------------------------

#[test]
fn list_can_be_limited_to_one_project_and_an_unknown_project_starts_nothing() {
    let (host, demo) = host_with_project();
    let other_dir = host.home.join("other");
    std::fs::create_dir_all(&other_dir).unwrap();
    let other = Store::open(&host.home)
        .unwrap()
        .add_project(other_dir, Some("other"))
        .unwrap();
    let in_demo = run_json(
        &host.home,
        &["new", "--provider", "codex", "--project", "demo"],
    );
    let in_other = run_json(
        &host.home,
        &["new", "--provider", "claude", "--project", "other"],
    );
    assert_eq!(in_demo["project_id"], demo.id.as_str());
    assert_eq!(in_other["project_id"], other.id.as_str());

    let only = |selector: &str| -> Vec<String> {
        run_json(&host.home, &["list", "--project", selector])
            .as_array()
            .unwrap()
            .iter()
            .map(|chat| chat["id"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(
        only("demo"),
        vec![in_demo["id"].as_str().unwrap().to_owned()]
    );
    assert_eq!(
        only(&other.id),
        vec![in_other["id"].as_str().unwrap().to_owned()]
    );
    let text = run(&host.home, &["list", "--project", "demo"], false).unwrap();
    assert_eq!(text.lines().count(), 1, "{text}");
    assert_eq!(run_json(&host.home, &["list"]).as_array().unwrap().len(), 2);

    let started = std::cell::Cell::new(false);
    let error = chat_client_command(
        &host.home,
        vec!["list".into(), "--project".into(), "nobody".into()],
        true,
        &|_| {
            started.set(true);
            Err("no".into())
        },
    )
    .unwrap_err();
    assert!(error.contains("No project matches 'nobody'"), "{error}");
    assert!(!started.get());
    assert!(run(&host.home, &["list", "--project"], false).is_err());
}

#[test]
fn new_takes_a_model_an_effort_and_a_title_as_they_are() {
    let (host, project) = host_with_project();
    let chat: ChatInfo = serde_json::from_value(run_json(
        &host.home,
        &[
            "new",
            "--provider",
            "codex",
            "--project",
            "demo",
            "--mode",
            "auto-edit",
            "--model",
            "gpt-5.5",
            "--effort=high",
            "--fast",
            "--title",
            "Fix the build",
        ],
    ))
    .unwrap();
    assert_eq!(chat.model.as_deref(), Some("gpt-5.5"));
    assert_eq!(chat.effort.as_deref(), Some("high"));
    assert!(chat.fast);
    assert_eq!(chat.title, "Fix the build");
    assert_eq!(chat.approval_mode, ApprovalMode::AutoEdit);
    let starts = fake_for(&project.root).starts.lock().unwrap().clone();
    assert_eq!(starts[0].model.as_deref(), Some("gpt-5.5"));
    assert_eq!(starts[0].effort.as_deref(), Some("high"));
    assert!(starts[0].fast, "the driver starts with fast mode on");

    // A value may look like an option, in either form: `--fast` as a title is a title.
    let titled = run_json(
        &host.home,
        &[
            "new",
            "--provider",
            "codex",
            "--project",
            "demo",
            "--title",
            "--fast",
        ],
    );
    assert_eq!(titled["title"], "--fast");
    assert_eq!(titled["fast"], false);
    let odd = run_json(
        &host.home,
        &[
            "new",
            "--provider",
            "claude",
            "--project",
            "demo",
            "--title=--draft",
            "--model",
            "--opus",
        ],
    );
    assert_eq!(odd["title"], "--draft");
    assert_eq!(odd["model"], "--opus");
    // A blank title is none: the default one.
    let plain = run_json(
        &host.home,
        &[
            "new",
            "--provider",
            "claude",
            "--project",
            "demo",
            "--title=  ",
        ],
    );
    assert_eq!(plain["title"], "Claude chat");
}

#[test]
fn new_refuses_settings_that_are_blank_long_or_hold_controls_before_anything_starts() {
    let (host, _project) = host_with_project();
    let long = |n: usize| "x".repeat(n);
    let base = ["new", "--provider", "codex", "--project", "demo"];
    let with = |extra: &[&str]| {
        let mut words = base.to_vec();
        words.extend_from_slice(extra);
        run(&host.home, &words, true).unwrap_err()
    };
    assert!(with(&["--model="]).contains("--model must not be blank"));
    assert!(with(&["--model", "  "]).contains("--model must not be blank"));
    assert!(with(&[&format!("--model={}", long(101))]).contains("at most 100"));
    assert!(with(&["--effort="]).contains("--effort must not be blank"));
    assert!(with(&[&format!("--effort={}", long(33))]).contains("at most 32"));
    assert!(with(&[&format!("--title={}", long(201))]).contains("at most 200"));
    assert!(with(&["--title=a\tb"]).contains("control characters"));
    assert!(with(&["--model=a\u{2028}b"]).contains("control characters"));
    assert!(with(&["--title"]).contains("--title needs a value"));
    assert!(with(&["--title=a", "--title=b"]).contains("only be given once"));
    // The limits themselves are fine.
    assert!(
        run(
            &host.home,
            &[
                "new",
                "--provider",
                "codex",
                "--project",
                "demo",
                &format!("--model={}", long(100)),
                &format!("--effort={}", long(32)),
                &format!("--title={}", long(200)),
            ],
            true
        )
        .is_ok()
    );
    assert_eq!(host.client().list().unwrap().len(), 1);
}

#[test]
fn a_chat_whose_provider_does_not_start_is_printed_as_json_and_is_an_error_as_text() {
    let (host, project) = host_with_project();
    *fake_for(&project.root).fail_start.lock().unwrap() = Some("codex is not installed".into());
    let chat: ChatInfo = serde_json::from_value(run_json(
        &host.home,
        &["new", "--provider", "codex", "--project", "demo"],
    ))
    .unwrap();
    assert_eq!(
        chat.state,
        ChatState::Failed {
            message: "codex is not installed".into()
        }
    );
    assert_eq!(host.client().list().unwrap().len(), 1);
    // No second chat appears when the caller does not retry.
    *fake_for(&project.root).fail_start.lock().unwrap() = Some("codex is not installed".into());
    let error = run(
        &host.home,
        &["new", "--provider", "codex", "--project", "demo"],
        false,
    )
    .unwrap_err();
    assert!(
        error.contains("was created, but its provider did not start"),
        "{error}"
    );
}

// ---- events --------------------------------------------------------------------------------

#[test]
fn events_that_are_there_come_at_once_whatever_the_wait() {
    let host = TestHost::new();
    let (chat, count) = idle_chat(&host);
    let started = Instant::now();
    let page = events(&host, &chat.id, &["--since", "0", "--wait-ms", "20000"]);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(page["chat_id"], chat.id.as_str());
    assert_eq!(seqs(&page), (1..=count).collect::<Vec<_>>());
    assert_eq!(
        (page["next"].as_u64(), page["more"].as_bool()),
        (Some(count), Some(false))
    );
    // Every entry is the event as the host logged it.
    let log = host.log(&chat.id);
    for (entry, logged) in page["events"].as_array().unwrap().iter().zip(&log) {
        assert_eq!(entry["event"], serde_json::to_value(&logged.event).unwrap());
    }
    // It is one compact line.
    let line = run(&host.home, &["events", &chat.id, "--wait-ms", "0"], true).unwrap();
    assert_eq!(line.lines().count(), 1);
    assert!(!line.contains("\n  "), "{line}");

    // From the middle, with a wait of none.
    let tail = events(&host, &chat.id, &["--since", "2"]);
    assert_eq!(seqs(&tail), (3..=count).collect::<Vec<_>>());
    // A prefix of the id is enough, as for the other commands.
    let by_prefix = events(&host, &chat.id[..8], &["--since", "2"]);
    assert_eq!(by_prefix, tail);
}

#[test]
fn nothing_new_is_an_empty_page_after_the_wait_with_next_unchanged() {
    let host = TestHost::new();
    let (chat, count) = idle_chat(&host);
    let started = Instant::now();
    let page = events(
        &host,
        &chat.id,
        &["--since", &count.to_string(), "--wait-ms", "300"],
    );
    let waited = started.elapsed();
    assert!(waited >= Duration::from_millis(290), "{waited:?}");
    assert!(waited < Duration::from_secs(5), "{waited:?}");
    assert_eq!(page["events"], json!([]));
    assert_eq!(
        (page["next"].as_u64(), page["more"].as_bool()),
        (Some(count), Some(false))
    );
    // With no wait it answers without one.
    let started = Instant::now();
    let page = events(&host, &chat.id, &["--since", &count.to_string()]);
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(page["events"], json!([]));
}

#[test]
fn a_wait_ends_with_the_first_event_and_collects_what_follows_within_a_short_window() {
    let host = TestHost::new();
    let (chat, count) = idle_chat(&host);
    let fake = host.fake();
    let feeder = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        // A burst, then one well past the window of its first.
        fake.emit(usage_event(1));
        fake.emit(usage_event(2));
        fake.emit(usage_event(3));
        std::thread::sleep(Duration::from_millis(1000));
        fake.emit(usage_event(4));
    });
    let started = Instant::now();
    let page = events(
        &host,
        &chat.id,
        &["--since", &count.to_string(), "--wait-ms", "20000"],
    );
    let waited = started.elapsed();
    assert!(waited >= Duration::from_millis(390), "{waited:?}");
    assert!(waited < Duration::from_secs(3), "{waited:?}");
    assert_eq!(seqs(&page), vec![count + 1, count + 2, count + 3]);
    assert_eq!(page["next"], count + 3);
    assert_eq!(page["more"], false);
    feeder.join().unwrap();
    host.wait_for_log(&chat.id, |log| log.len() as u64 == count + 4);
    let rest = events(&host, &chat.id, &["--since", &(count + 3).to_string()]);
    assert_eq!(seqs(&rest), vec![count + 4]);
}

#[test]
fn a_full_page_says_whether_there_is_more_and_next_continues_it() {
    let host = TestHost::new();
    let (chat, count) = idle_chat(&host);
    let total = emit(&host, &chat, (1..=5).map(usage_event).collect());
    assert_eq!(total, count + 5);
    let since = count.to_string();

    let first = events(&host, &chat.id, &["--since", &since, "--max", "3"]);
    assert_eq!(seqs(&first), vec![count + 1, count + 2, count + 3]);
    assert_eq!(
        (first["next"].as_u64(), first["more"].as_bool()),
        (Some(count + 3), Some(true))
    );
    let second = events(
        &host,
        &chat.id,
        &["--since", &(count + 3).to_string(), "--max", "3"],
    );
    assert_eq!(seqs(&second), vec![count + 4, count + 5]);
    assert_eq!(
        (second["next"].as_u64(), second["more"].as_bool()),
        (Some(total), Some(false))
    );
    // Exactly as many as were asked for is not "more".
    let exact = events(&host, &chat.id, &["--since", &since, "--max", "5"]);
    assert_eq!(exact["events"].as_array().unwrap().len(), 5);
    assert_eq!(exact["more"], false);
    let one = events(&host, &chat.id, &["--since", &since, "--max", "1"]);
    assert_eq!(
        (seqs(&one), one["more"].as_bool()),
        (vec![count + 1], Some(true))
    );
}

#[test]
fn a_page_stops_at_the_bytes_it_may_print_and_leaves_the_rest_for_the_next() {
    let host = TestHost::new();
    let (chat, count) = idle_chat(&host);
    let text = "y".repeat(2000);
    emit(&host, &chat, (0..5).map(|_| message_event(&text)).collect());
    let since = count.to_string();
    let line = run(
        &host.home,
        &["events", &chat.id, "--since", &since, "--max-bytes", "4096"],
        true,
    )
    .unwrap();
    assert!(line.trim_end().len() <= 4096, "{}", line.len());
    let page: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(seqs(&page), vec![count + 1]);
    assert_eq!(
        (page["next"].as_u64(), page["more"].as_bool()),
        (Some(count + 1), Some(true))
    );
    // Walking the page size gets everything, once.
    let mut got = Vec::new();
    let mut next = count;
    loop {
        let page = events(
            &host,
            &chat.id,
            &["--since", &next.to_string(), "--max-bytes", "5000"],
        );
        got.extend(seqs(&page));
        next = page["next"].as_u64().unwrap();
        if page["more"] == false {
            break;
        }
    }
    assert_eq!(got, (count + 1..=count + 5).collect::<Vec<_>>());
}

#[test]
fn an_event_too_big_for_a_page_keeps_a_sequence_slot_with_cut_text_or_the_existing_note() {
    let host = TestHost::new();
    let (chat, count) = idle_chat(&host);
    let since = count.to_string();
    emit(
        &host,
        &chat,
        vec![message_event(&"z\u{e9}".repeat(100_000)), usage_event(1)],
    );
    let page = events(&host, &chat.id, &["--since", &since, "--max-bytes", "4096"]);
    let first = &page["events"][0];
    assert_eq!(first["seq"], count + 1);
    let cut = first["event"]["item"]["body"]["text"].as_str().unwrap();
    assert!(
        cut.ends_with('\u{2026}') && cut.len() < 2000,
        "{}",
        cut.len()
    );
    assert_eq!(first["event"]["event"], "item_completed");
    // What came after it still fits the page.
    assert_eq!(seqs(&page), vec![count + 1, count + 2]);
    assert_eq!(
        (page["next"].as_u64(), page["more"].as_bool()),
        (Some(count + 2), Some(false))
    );

    // A tool call with thousands of short fields cannot be made small.
    let input: serde_json::Map<String, Value> =
        (0..3000).map(|n| (format!("key{n}"), json!("v"))).collect();
    let hopeless = ChatEvent::ItemCompleted {
        item: Item {
            presentation: Default::default(),
            id: "tool-1".into(),
            turn_id: None,
            status: ItemStatus::Completed,
            body: ItemBody::ToolCall {
                server: None,
                tool: "Big".into(),
                input: Value::Object(input),
                output: None,
            },
        },
    };
    let before = emit(&host, &chat, vec![hopeless.clone()]);
    let dropped = events(
        &host,
        &chat.id,
        &["--since", &(before - 1).to_string(), "--max-bytes", "4096"],
    );
    assert_eq!(dropped["events"][0]["seq"], before);
    assert_eq!(dropped["events"][0]["event"]["item"]["id"], "tool-1");
    assert_eq!(
        dropped["events"][0]["event"]["item"]["body"]["text"],
        crate::chat::remote_payload::TRUNCATION_NOTE
    );
    assert_eq!(
        (dropped["next"].as_u64(), dropped["more"].as_bool()),
        (Some(before), Some(false))
    );
    let total = emit(&host, &chat, vec![hopeless, usage_event(2)]);
    let across = events(
        &host,
        &chat.id,
        &["--since", &before.to_string(), "--max-bytes", "4096"],
    );
    assert_eq!(seqs(&across), vec![total - 1, total]);
    assert_eq!(across["next"], total);
}

#[test]
fn events_say_what_is_wrong_with_the_chat_or_the_options_they_were_given() {
    let host = TestHost::new();
    let (chat, count) = idle_chat(&host);
    let fail = |words: &[&str]| run(&host.home, words, true).unwrap_err();
    assert!(fail(&["events", "00000000-0000-0000-0000-000000000000"]).contains("Unknown chat"));
    assert!(fail(&["events", "abc"]).contains("at least eight"));
    let beyond = fail(&["events", &chat.id, "--since", &(count + 1).to_string()]);
    assert!(beyond.starts_with("invalid_request: "), "{beyond}");
    assert!(beyond.contains("cannot continue after"), "{beyond}");
    for bad in [
        &["events"][..],
        &["events", &chat.id, "extra"],
        &["events", &chat.id, "--wait-ms", "25001"],
        &["events", &chat.id, "--wait-ms", "-1"],
        &["events", &chat.id, "--since", "x"],
        &["events", &chat.id, "--max", "0"],
        &["events", &chat.id, "--max", "2001"],
        &["events", &chat.id, "--max-bytes", "1023"],
        &["events", &chat.id, "--max-bytes", "2097153"],
        &["events", &chat.id, "--since"],
    ] {
        let error = fail(bad);
        assert!(
            error.starts_with("Usage: riwork chat events")
                || error.contains("must be a whole number")
                || error.contains("needs a value"),
            "{bad:?}: {error}"
        );
    }
    // The limits themselves are fine.
    for ok in [
        &["--wait-ms", "0"][..],
        &["--max", "2000"],
        &["--max", "1"],
        &["--max-bytes", "1024"],
        &["--max-bytes", "2097152"],
    ] {
        let mut words = vec!["events", chat.id.as_str(), "--since", "0"];
        words.extend_from_slice(ok);
        assert!(run(&host.home, &words, true).is_ok(), "{ok:?}");
    }
    // The wait of 25000 is allowed (the chat has events, so it returns at once).
    assert!(
        run(
            &host.home,
            &["events", &chat.id, "--wait-ms", "25000"],
            true
        )
        .is_ok()
    );
}

#[test]
fn events_without_json_print_one_line_per_event() {
    let host = TestHost::new();
    let (chat, count) = idle_chat(&host);
    let text = run(&host.home, &["events", &chat.id], false).unwrap();
    let lines: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len() as u64, count);
    assert_eq!(lines[0]["seq"], 1);
    assert!(lines[0]["event"]["event"].is_string());
    assert_eq!(
        run(
            &host.home,
            &["events", &chat.id, "--since", &count.to_string()],
            false
        )
        .unwrap(),
        ""
    );
}

// ---- command -------------------------------------------------------------------------------

fn command(host: &TestHost, id: &str, json_text: &str) -> Result<String, String> {
    run(
        &host.home,
        &["command", id, "--command-json", json_text],
        true,
    )
}

#[test]
fn every_command_reaches_the_driver_in_either_argument_form() {
    let host = TestHost::new();
    let (chat, _) = idle_chat(&host);
    let ok = |text: &str| {
        let answer: Value = serde_json::from_str(&command(&host, &chat.id, text).unwrap()).unwrap();
        assert_eq!(answer, json!({"id": chat.id, "status": "ok"}));
    };
    ok(r#"{"command":"send","text":"hello there"}"#);
    ok(r#"{"command":"interrupt"}"#);
    ok(r#"{"command":"approve","request_id":"r1","decision":"accept_for_session"}"#);
    ok(r#"{"command":"answer","request_id":"q1","answers":[["Yes"],["a","b"],[]]}"#);
    ok(r#"{"command":"compact"}"#);
    ok(r#"{"command":"configure","model":"gpt-5.5","effort":"low","approval_mode":"plan"}"#);
    ok(r#"{"command":"configure","effort":null,"model":"gpt-5.6"}"#);
    ok(r#"{"command":"configure","fast":true}"#);
    ok(r#"{"command":"configure","fast":false,"model":null}"#);
    assert_eq!(
        host.fake().commands(),
        vec![
            ChatCommand::Send {
                text: "hello there".into()
            },
            ChatCommand::Interrupt,
            ChatCommand::Approve {
                request_id: "r1".into(),
                decision: Decision::AcceptForSession
            },
            ChatCommand::Answer {
                request_id: "q1".into(),
                answers: vec![vec!["Yes".into()], vec!["a".into(), "b".into()], vec![]]
            },
            ChatCommand::Compact,
            ChatCommand::Configure {
                model: Some("gpt-5.5".into()),
                effort: Some("low".into()),
                approval_mode: Some(ApprovalMode::Plan),
                fast: None,
            },
            ChatCommand::Configure {
                model: Some("gpt-5.6".into()),
                effort: None,
                approval_mode: None,
                fast: None,
            },
            ChatCommand::Configure {
                model: None,
                effort: None,
                approval_mode: None,
                fast: Some(true),
            },
            ChatCommand::Configure {
                model: None,
                effort: None,
                approval_mode: None,
                fast: Some(false),
            },
        ]
    );
    let info = host.info(&chat.id);
    assert_eq!(info.model.as_deref(), Some("gpt-5.6"));
    assert_eq!(info.approval_mode, ApprovalMode::Plan);
    assert!(!info.fast, "the last Configure turned fast mode off");

    // After a separator, the same, and the text form prints nothing.
    let text = chat_client_command(
        &host.home,
        vec![
            "command".into(),
            chat.id[..8].into(),
            "--".into(),
            r#"{"command":"send","text":"--json"}"#.into(),
        ],
        false,
        &|home| Ok(socket_path(home)),
    )
    .unwrap();
    assert_eq!(text, "");
    assert_eq!(
        host.fake().commands().last(),
        Some(&ChatCommand::Send {
            text: "--json".into()
        })
    );

    // Stop is a command too.
    ok(r#"{"command":"stop"}"#);
    assert_eq!(host.info(&chat.id).state, ChatState::Stopped);
}

#[test]
fn a_send_to_a_stopped_chat_resumes_it() {
    let host = TestHost::new();
    let (chat, _) = idle_chat(&host);
    host.client().close(&chat.id).unwrap();
    assert_eq!(host.fake().start_count(), 1);
    command(&host, &chat.id, r#"{"command":"send","text":"wake up"}"#).unwrap();
    assert_eq!(host.fake().start_count(), 2);
    assert_eq!(
        host.fake().commands(),
        vec![ChatCommand::Send {
            text: "wake up".into()
        }]
    );
}

#[test]
fn a_command_that_is_not_strictly_a_command_is_refused_before_a_host_is_asked() {
    let started = std::cell::Cell::new(false);
    let home = short_home();
    let refuse = |json_text: &str| {
        chat_client_command(
            &home,
            vec![
                "command".into(),
                "00000000-0000-0000-0000-000000000000".into(),
                "--command-json".into(),
                json_text.into(),
            ],
            true,
            &|_| {
                started.set(true);
                Err("no host".into())
            },
        )
        .unwrap_err()
    };
    let big = format!(
        r#"{{"command":"send","text":"{}"}}"#,
        "x".repeat(MAX_TEXT + 1)
    );
    let wrong_answer = format!(
        r#"{{"command":"answer","request_id":"r","answers":"{}"}}"#,
        "SECRET".repeat(30)
    );
    for bad in [
        "",
        "not json",
        "[]",
        r#""send""#,
        "null",
        "{}",
        r#"{"command":"send"}"#,
        r#"{"command":"send","text":"hi","extra":1}"#,
        r#"{"command":"send","text":"hi","Text":"hi"}"#,
        r#"{"command":"send","text":5}"#,
        r#"{"command":"send","text":null}"#,
        r#"{"command":"send","text":"   \n "}"#,
        big.as_str(),
        r#"{"command":"interrupt","text":"x"}"#,
        r#"{"command":"stop","now":true}"#,
        // A field the command does not have is refused whatever it holds, null included.
        r#"{"command":"interrupt","x":null}"#,
        r#"{"command":"send","text":"hi","model":null}"#,
        r#"{"command":"approve","request_id":"r","decision":"accept","effort":null}"#,
        r#"{"command":"fly"}"#,
        r#"{"command":"Send","text":"hi"}"#,
        r#"{"command":"configure"}"#,
        r#"{"command":"configure","model":null}"#,
        r#"{"command":"configure","model":""}"#,
        r#"{"command":"configure","model":"a\u0007b"}"#,
        r#"{"command":"configure","effort":"x234567890123456789012345678901234"}"#,
        r#"{"command":"configure","approval_mode":"reckless"}"#,
        // Fast is a boolean; the host's Retry (nothing to change) is not a command a client sends.
        r#"{"command":"configure","fast":"yes"}"#,
        r#"{"command":"configure","fast":1}"#,
        r#"{"command":"configure","fast":null}"#,
        r#"{"command":"send","text":"hi","fast":true}"#,
        r#"{"command":"approve","request_id":"r","decision":"yes"}"#,
        r#"{"command":"approve","request_id":"r"}"#,
        r#"{"command":"answer","request_id":"r","answers":["a"]}"#,
        wrong_answer.as_str(),
    ] {
        let error = refuse(bad);
        assert!(error.starts_with("invalid_request: "), "{bad:.60}: {error}");
        assert!(!error.contains("SECRET"), "{error}");
        assert!(error.len() < 300, "{}", error.len());
    }
    // The longest message there is, is fine for the check.
    let longest = format!(r#"{{"command":"send","text":"{}"}}"#, "x".repeat(MAX_TEXT));
    assert!(parse_command(&longest).is_ok());
    assert!(!started.get(), "no host was asked for any of them");
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn command_says_what_is_wrong_with_the_chat_or_the_arguments() {
    let host = TestHost::new();
    let (chat, _) = idle_chat(&host);
    let stop = r#"{"command":"interrupt"}"#;
    assert!(
        command(&host, "00000000-0000-0000-0000-000000000000", stop)
            .unwrap_err()
            .contains("Unknown chat")
    );
    for words in [
        vec!["command"],
        vec!["command", chat.id.as_str()],
        vec!["command", chat.id.as_str(), "--command-json"],
        vec!["command", chat.id.as_str(), "--", stop, "extra"],
        vec![
            "command",
            chat.id.as_str(),
            "--command-json",
            stop,
            "--",
            stop,
        ],
        vec!["command", chat.id.as_str(), "extra", "--command-json", stop],
    ] {
        let error = run(&host.home, &words, true).unwrap_err();
        assert!(
            error.starts_with("Usage: riwork chat command") || error.contains("needs a value"),
            "{words:?}: {error}"
        );
    }
    assert!(host.fake().commands().is_empty());
}

#[test]
fn text_that_looks_like_an_option_stays_text() {
    let mut args: Vec<String> = ["--title", "--draft", "--title-x", "--model=", "keep"]
        .map(String::from)
        .into();
    assert_eq!(
        take_verbatim_option(&mut args, "--title")
            .unwrap()
            .as_deref(),
        Some("--draft")
    );
    assert_eq!(args, ["--title-x", "--model=", "keep"]);
    assert_eq!(
        take_verbatim_option(&mut args, "--model")
            .unwrap()
            .as_deref(),
        Some("")
    );
    assert_eq!(args, ["--title-x", "keep"]);
    assert_eq!(take_verbatim_option(&mut args, "--effort").unwrap(), None);
    let mut twice: Vec<String> = vec!["--a=1".into(), "--a".into(), "2".into()];
    assert!(take_verbatim_option(&mut twice, "--a").is_err());
    let mut dangling: Vec<String> = vec!["--a".into()];
    assert!(take_verbatim_option(&mut dangling, "--a").is_err());
}

// ---- the page collector --------------------------------------------------------------------

/// Events for the collector to read, then silence (or the end).
struct Script {
    steps: VecDeque<Result<Poll, String>>,
    then_closed: bool,
}

impl Script {
    fn of(seqs: impl IntoIterator<Item = u64>) -> Self {
        Self {
            steps: seqs
                .into_iter()
                .map(|seq| Ok(Poll::Event(envelope(seq, usage_event(seq)))))
                .collect(),
            then_closed: false,
        }
    }
}

impl Source for Script {
    fn next(&mut self, _wait: Duration) -> Result<Poll, String> {
        match self.steps.pop_front() {
            Some(step) => step,
            None if self.then_closed => Ok(Poll::Closed),
            None => Ok(Poll::TimedOut),
        }
    }
}

fn envelope(seq: u64, event: ChatEvent) -> Envelope {
    Envelope {
        chat_id: "c".into(),
        seq,
        event,
    }
}

fn plan(since: u64, max: usize, max_bytes: usize) -> Plan {
    Plan {
        chat_id: "c".into(),
        since,
        first_by: Instant::now() + Duration::from_millis(500),
        max,
        max_bytes,
    }
}

#[test]
fn a_page_is_what_the_source_gave_up_to_the_limits() {
    let page = collect(&mut Script::of([]), &plan(7, 10, 100_000)).unwrap();
    assert_eq!((page.events.len(), page.next, page.more), (0, 7, false));

    let page = collect(&mut Script::of([8, 9, 10]), &plan(7, 10, 100_000)).unwrap();
    assert_eq!(
        (
            page.events.iter().map(|e| e.seq).collect::<Vec<_>>(),
            page.next,
            page.more
        ),
        (vec![8, 9, 10], 10, false)
    );

    let page = collect(&mut Script::of([8, 9, 10]), &plan(7, 2, 100_000)).unwrap();
    assert_eq!((page.events.len(), page.next, page.more), (2, 9, true));
    let page = collect(&mut Script::of([8, 9]), &plan(7, 2, 100_000)).unwrap();
    assert_eq!((page.events.len(), page.next, page.more), (2, 9, false));

    // Room for two entries and not three.
    let one = entry_len(&Script::of([8]));
    let page = collect(
        &mut Script::of([8, 9, 10]),
        &plan(7, 10, 105 + 2 * one + one / 2),
    )
    .unwrap();
    assert_eq!((page.events.len(), page.next, page.more), (2, 9, true));
    assert!(page.line().unwrap().len() <= 105 + 2 * one + one / 2);
}

fn entry_len(script: &Script) -> usize {
    let Some(Ok(Poll::Event(first))) = script.steps.front() else {
        panic!("no event");
    };
    serde_json::to_string(&Entry {
        seq: first.seq,
        event: serde_json::to_value(&first.event).unwrap(),
    })
    .unwrap()
    .len()
}

#[test]
fn a_page_that_ends_with_the_connection_is_still_a_page_unless_it_is_empty() {
    let mut script = Script::of([8, 9]);
    script.then_closed = true;
    let page = collect(&mut script, &plan(7, 10, 100_000)).unwrap();
    assert_eq!((page.events.len(), page.next), (2, 9));
    let mut script = Script::of([]);
    script.then_closed = true;
    assert_eq!(
        collect(&mut script, &plan(7, 10, 100_000)).unwrap_err(),
        "chat host closed the connection"
    );
    // An event that cannot be read ends the page with an error.
    let mut script = Script::of([8]);
    script.steps.push_back(Err("unreadable".into()));
    assert!(collect(&mut script, &plan(7, 10, 100_000)).is_err());
    // The host never goes back.
    assert!(collect(&mut Script::of([8, 8]), &plan(7, 10, 100_000)).is_err());
    assert!(collect(&mut Script::of([7]), &plan(7, 10, 100_000)).is_err());
}

#[test]
fn strings_are_cut_on_a_character_boundary_and_marked() {
    let mut value = json!({"a": "\u{e9}".repeat(10), "b": ["short", "x".repeat(300)], "n": 5});
    cut_strings(&mut value, 5);
    // Four whole characters fit in five bytes.
    assert_eq!(value["a"], "\u{e9}\u{e9}\u{2026}");
    assert_eq!(value["b"][0], "short");
    assert_eq!(value["b"][1], format!("{}\u{2026}", "x".repeat(5)));
    assert_eq!(value["n"], 5);
}

#[test]
fn a_hopeless_entry_is_not_shrunk_and_a_long_one_is() {
    let long = Entry {
        seq: 1,
        event: json!({"event":"item_delta","item_id":"long","delta":{"kind":"text","text":"t".repeat(50_000)}}),
    };
    let shrunk = shrink_body(&long, 2000).unwrap();
    assert!(serde_json::to_string(&shrunk).unwrap().len() < 2000);
    assert!(
        shrunk.event["delta"]["text"]
            .as_str()
            .unwrap()
            .ends_with('\u{2026}')
    );
    let wide = Entry {
        seq: 1,
        event: Value::Array((0..5000).map(|n| json!(n)).collect()),
    };
    assert!(shrink_body(&wide, 2000).is_none());
}

#[test]
fn remote_shrink_preserves_small_images_and_marks_large_images_unavailable() {
    let mut value = json!({"item":{"presentation":{"images":[
        {"label":"small","source":{"kind":"data","mime":"image/png","base64":"aGVsbG8="}},
        {"label":"large","source":{"kind":"data","mime":"image/png","base64":"a".repeat(1000)}}
    ]}}});
    cut_strings(&mut value, 128);
    assert_eq!(
        value["item"]["presentation"]["images"][0]["source"]["base64"],
        "aGVsbG8="
    );
    assert_eq!(
        value["item"]["presentation"]["images"][1]["source"]["kind"],
        "unavailable"
    );
    assert!(
        value["item"]["presentation"]["images"][1]["source"]
            .get("base64")
            .is_none()
    );
}

#[test]
fn complete_event_pages_elide_oversized_events_and_keep_sequence_slots() {
    let event = ChatEvent::ItemCompleted {
        item: crate::chat::model::Item {
            id: "large".into(),
            turn_id: None,
            status: crate::chat::model::ItemStatus::Completed,
            presentation: Default::default(),
            body: crate::chat::model::ItemBody::AgentMessage {
                text: "x".repeat(5000),
            },
        },
    };
    let mut script = Script::of([]);
    script
        .steps
        .push_back(Ok(Poll::Event(envelope(8, event.clone()))));
    let recovered = collect(&mut script, &plan(7, 10, 1024)).unwrap();
    assert_eq!(recovered.next, 8);
    assert_eq!(recovered.events[0].event["item"]["id"], "large");
    assert_eq!(
        recovered.events[0].event["item"]["body"]["text"],
        crate::chat::remote_payload::TRUNCATION_NOTE
    );
    let mut script = Script::of([8]);
    script.steps.push_back(Ok(Poll::Event(envelope(9, event))));
    let page = collect(&mut script, &plan(7, 10, 1024)).unwrap();
    assert_eq!(page.next, 8);
    assert!(page.more);
    assert_eq!(page.events.len(), 1);
}

#[test]
fn snapshot_is_read_only_without_a_host_or_an_ensure_call() {
    let home = std::env::temp_dir().join(format!("riwork-cli-snapshot-{}", uuid::Uuid::new_v4()));
    let id = uuid::Uuid::new_v4().to_string();
    let dir = crate::chat::log::chat_dir(&home, &id).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    let line = serde_json::to_vec(&envelope(1, usage_event(42))).unwrap();
    let mut value: Value = serde_json::from_slice(&line).unwrap();
    value["chat_id"] = json!(id);
    let log = value.to_string() + "\n{partial";
    std::fs::write(dir.join("events.jsonl"), &log).unwrap();
    let result = chat_client_command(&home, vec!["snapshot".into(), id], true, &|_| {
        panic!("snapshot must never activate the host")
    })
    .unwrap();
    let result: Value = serde_json::from_str(&result).unwrap();
    assert_eq!(result["next"], 1);
    assert_eq!(
        std::fs::read_to_string(dir.join("events.jsonl")).unwrap(),
        log
    );
    assert!(!home.join("run").exists());
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn bounded_recovery_represents_bodies_and_never_shortens_or_skips_controls() {
    let item = Item {
        id: "stable".into(),
        turn_id: Some("turn".into()),
        status: ItemStatus::Completed,
        presentation: Default::default(),
        body: ItemBody::AgentMessage {
            text: "x".repeat(200_000),
        },
    };
    let mut source = Script::of([]);
    source.steps.push_back(Ok(Poll::Event(envelope(
        1,
        ChatEvent::ItemCompleted { item },
    ))));
    source
        .steps
        .push_back(Ok(Poll::Event(envelope(2, usage_event(42)))));
    let page = collect(&mut source, &plan(0, 10, 120_000)).unwrap();
    assert_eq!(page.next, 2);
    assert_eq!(page.events[0].event["item"]["id"], "stable");
    assert_eq!(page.events[0].event["item"]["turn_id"], "turn");
    assert_eq!(page.events[0].event["item"]["status"], "completed");
    assert!(
        page.events[0].event["item"]["body"]["text"]
            .as_str()
            .unwrap()
            .ends_with('…')
    );
    assert_eq!(page.events[1].event["usage"]["input_tokens"], 42);
    let mut source = Script::of([]);
    source.steps.push_back(Ok(Poll::Event(envelope(
        1,
        ChatEvent::State {
            state: ChatState::Failed {
                message: "x".repeat(200_000),
            },
        },
    ))));
    let recovered = collect(&mut source, &plan(0, 10, 120_000)).unwrap();
    assert_eq!(recovered.next, 1);
    assert_eq!(recovered.events[0].event["event"], "control_elided");
    assert_eq!(recovered.events[0].event["of"], "state");
    assert_eq!(recovered.events[0].event["elided"], true);
}

#[test]
fn bounded_body_placeholder_says_full_text_is_on_the_mac_and_keeps_identity() {
    let item = Item {
        id: "large-structured-body".into(),
        turn_id: Some("turn".into()),
        status: ItemStatus::Completed,
        presentation: Default::default(),
        body: ItemBody::ToolCall {
            server: None,
            tool: "fixture".into(),
            input: json!(vec!["x"; 3000]),
            output: None,
        },
    };
    let mut source = Script::of([]);
    source.steps.push_back(Ok(Poll::Event(envelope(
        1,
        ChatEvent::ItemCompleted { item },
    ))));
    let page = collect(&mut source, &plan(0, 1, 1024)).unwrap();
    assert_eq!(page.next, 1);
    assert_eq!(page.events[0].event["item"]["id"], "large-structured-body");
    assert_eq!(page.events[0].event["item"]["turn_id"], "turn");
    assert_eq!(
        page.events[0].event["item"]["body"]["text"],
        "This message is too long to show here. Full text is on your Mac."
    );
}

#[test]
fn notice_dismiss_command_decodes_and_rejects_invalid_fields() {
    assert_eq!(
        super::chat_remote::parse_command(r#"{"command":"dismiss_notice","item_id":"notice-1"}"#)
            .unwrap(),
        ChatCommand::DismissNotice {
            item_id: "notice-1".into()
        }
    );
    for json in [
        r#"{"command":"dismiss_notice"}"#,
        r#"{"command":"dismiss_notice","item_id":""}"#,
        r#"{"command":"dismiss_notice","item_id":"bad\nline"}"#,
        r#"{"command":"dismiss_notice","item_id":"one","kind":"auth_required"}"#,
    ] {
        assert!(super::chat_remote::parse_command(json).is_err(), "{json}");
    }
}

#[test]
fn snapshot_cli_pages_normal_rows_and_recovers_a_single_payload_under_the_requested_budget() {
    let home = std::env::temp_dir().join(format!(
        "riwork-cli-snapshot-review-{}",
        uuid::Uuid::new_v4()
    ));
    let id = uuid::Uuid::new_v4().to_string();
    let dir = crate::chat::log::chat_dir(&home, &id).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    let write = |events: Vec<Value>| {
        let log: String = events
            .into_iter()
            .enumerate()
            .map(|(i, event)| {
                json!({"v":1,"type":"event","chat_id":id,"seq":i + 1,"event":event}).to_string()
                    + "\n"
            })
            .collect();
        std::fs::write(dir.join("events.jsonl"), &log).unwrap();
        log
    };
    let snapshot = || -> Value {
        let text = chat_client_command(
            &home,
            vec![
                "snapshot".into(),
                id.clone(),
                "--max".into(),
                "100".into(),
                "--max-bytes".into(),
                "120000".into(),
            ],
            true,
            &|_| panic!("snapshot must not start a host"),
        )
        .unwrap();
        assert!(text.len() <= 120_000);
        serde_json::from_str(&text).unwrap()
    };
    let rows: Vec<_> = (1..=100).map(|seq| json!({"event":"item_completed","item":{
        "id":format!("normal-{seq}"),"status":"completed","body":{"type":"agent_message","text":"normal ".repeat(600)}}})).collect();
    let log = write(rows);
    let result = snapshot();
    assert_eq!(result["more"], true);
    assert!(result["items"].as_array().unwrap().len() < 100);
    assert!(
        result["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["item"].get("elided").is_none()
                && row["item"]["body"]["text"] == "normal ".repeat(600))
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("events.jsonl")).unwrap(),
        log
    );

    // The typed log reader refuses a single oversized control at 120 KiB; the
    // CLI's exceptional response recovery handles it without widening its output.
    let log = write(vec![
        json!({"event":"approval_requested","approval":{"request_id":"huge","item_id":"tool",
        "kind":"tool","title":"request","detail":"x".repeat(2 * 1024 * 1024),"choices":["accept","decline"]}}),
    ]);
    let result = snapshot();
    assert_eq!(result["next"], 1);
    let control = result["controls"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["event"] == "control_elided")
        .unwrap();
    assert_eq!(control["of"], "approval_requested");
    assert_eq!(control["approval"]["request_id"], "huge");
    assert!(
        result["controls"]
            .as_array()
            .unwrap()
            .iter()
            .all(|event| event["event"] != "approval_requested")
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("events.jsonl")).unwrap(),
        log
    );
    assert!(!home.join("run").exists());
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn cli_keeps_its_256_byte_minimum_cut() {
    let mut event = json!({"event":"item_delta","item_id":"long","delta":{"kind":"text","text":"x".repeat(5000)}});
    let entry = Entry {
        seq: 1,
        event: event.take(),
    };
    let shrunk = shrink_body(&entry, 450).unwrap();
    assert_eq!(
        shrunk.event["delta"]["text"].as_str().unwrap().len(),
        256 + '…'.len_utf8()
    );
}
