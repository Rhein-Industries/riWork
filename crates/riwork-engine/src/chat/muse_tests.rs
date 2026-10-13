use super::*;
use crate::chat::model::{Provider, Transcript};
use crate::chat::testkit::{Fake, fixture, fold, until};
use std::sync::mpsc::{self, Receiver};

#[test]
fn approval_modes_map_to_muse_words() {
    assert_eq!(policy(ApprovalMode::Supervised), "promptUnmatched");
    assert_eq!(policy(ApprovalMode::AutoEdit), "onRequest");
    assert_eq!(policy(ApprovalMode::Full), "allowAll");
    assert_eq!(policy(ApprovalMode::Plan), "denyUnmatched");
}

#[test]
fn choices_map_on_decision_and_scope_pairs() {
    assert_eq!(map_choice("approved", "once"), Some(Decision::Accept));
    assert_eq!(
        map_choice("approved", "session"),
        Some(Decision::AcceptForSession)
    );
    assert_eq!(
        map_choice("approvedForSession", "once"),
        Some(Decision::AcceptForSession)
    );
    // A lasting grant is never understated as a one-off accept.
    assert_eq!(
        map_choice("approvedPolicyAmendment", "localPersistent"),
        Some(Decision::AcceptForSession)
    );
    assert_eq!(map_choice("denied", "once"), Some(Decision::Decline));
    assert_eq!(map_choice("abort", "once"), Some(Decision::Cancel));
    assert_eq!(map_choice("timedOut", "once"), None);
    assert_eq!(map_choice("mystery", "once"), None);
    assert_eq!(map_choice("approved", "mystery"), None);
}

#[test]
fn items_convert_with_msp_field_names() {
    let (body, status) = convert_item(
        &serde_json::json!({"kind": "agentMessage", "status": "completed", "text": "hi"}),
        true,
    )
    .unwrap();
    assert_eq!(
        body,
        ItemBody::AgentMessage {
            text: "hi".to_owned()
        }
    );
    assert_eq!(status, ItemStatus::Completed);
    // The tool name field is `tool`, and `args` is verbatim JSON text.
    let (body, _) = convert_item(
        &serde_json::json!({
            "kind": "toolCall", "status": "inProgress", "tool": "read_file",
            "args": "{\"path\": \"a.rs\"}"
        }),
        false,
    )
    .unwrap();
    match body {
        ItemBody::ToolCall { tool, input, .. } => {
            assert_eq!(tool, "read_file");
            assert_eq!(input["path"], "a.rs");
        }
        other => panic!("expected a tool call, got {other:?}"),
    }
    // Almost-JSON args survive as a string instead of failing the item.
    let (body, _) = convert_item(
        &serde_json::json!({"kind": "toolCall", "tool": "x", "args": "{nope"}),
        false,
    )
    .unwrap();
    match body {
        ItemBody::ToolCall { input, .. } => assert_eq!(input, serde_json::json!("{nope")),
        other => panic!("expected a tool call, got {other:?}"),
    }
    // Reminder machinery never reaches the transcript.
    assert!(convert_item(&serde_json::json!({"kind": "reminderChild"}), false).is_none());
}

/// A driver running against the fake `muse serve` replaying `name`.
struct Run {
    fake: Fake,
    driver: Box<dyn Driver>,
    events: Receiver<ChatEvent>,
    seen: Vec<ChatEvent>,
}

impl Run {
    fn start(name: &str) -> Self {
        Self::start_with(name, |_| {})
    }

    fn start_with(name: &str, edit: impl FnOnce(&mut DriverConfig)) -> Self {
        let fake = Fake::new(&[&fixture(&format!("muse/{name}.ndjson"))]);
        let mut config = fake.config(Provider::Muse);
        edit(&mut config);
        let (sender, events) = mpsc::channel();
        let driver = start(config, sender).expect("the driver starts");
        let mut run = Self {
            fake,
            driver,
            events,
            seen: Vec::new(),
        };
        run.until(is_state(ChatState::Idle));
        run
    }

    fn until(&mut self, done: impl Fn(&ChatEvent) -> bool) -> Vec<ChatEvent> {
        let events = until(&self.events, done);
        self.seen.extend(events.iter().cloned());
        events
    }

    fn send(&mut self, text: &str) {
        self.driver
            .command(ChatCommand::Send { text: text.into() })
            .unwrap();
    }

    fn finish_turn(&mut self) {
        self.until(is_turn_completed);
        self.until(is_state(ChatState::Idle));
    }

    fn transcript(&self) -> Transcript {
        fold(&self.seen)
    }

    fn states(&self) -> Vec<ChatState> {
        self.seen
            .iter()
            .filter_map(|event| match event {
                ChatEvent::State { state } => Some(state.clone()),
                _ => None,
            })
            .collect()
    }

    fn notices(&self) -> Vec<(NoticeLevel, String)> {
        self.transcript()
            .items
            .into_iter()
            .filter_map(|item| match item.body {
                ItemBody::Notice { level, text, .. } => Some((level, text)),
                _ => None,
            })
            .collect()
    }

    fn item(&self, id: &str) -> Item {
        self.transcript()
            .items
            .into_iter()
            .find(|item| item.id == id)
            .unwrap_or_else(|| panic!("no item {id}"))
    }

    fn end(mut self) {
        self.driver.shutdown();
        assert!(!self.fake.saw("mismatch"), "{:#?}", self.fake.entries());
    }
}

fn is_state(wanted: ChatState) -> impl Fn(&ChatEvent) -> bool {
    move |event| matches!(event, ChatEvent::State { state } if *state == wanted)
}

fn is_turn_completed(event: &ChatEvent) -> bool {
    matches!(event, ChatEvent::TurnCompleted { .. })
}

fn is_approval(event: &ChatEvent) -> bool {
    matches!(event, ChatEvent::ApprovalRequested { .. })
}

fn is_question(event: &ChatEvent) -> bool {
    matches!(event, ChatEvent::QuestionRequested { .. })
}

fn deltas(events: &[ChatEvent], item: &str) -> Vec<Delta> {
    events
        .iter()
        .filter_map(|event| match event {
            ChatEvent::ItemDelta { item_id, delta } if item_id == item => Some(delta.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_whole_turn_becomes_items_usage_and_states() {
    let mut run = Run::start("turn");
    assert_eq!(run.driver.provider_thread_id().as_deref(), Some("sess-1"));
    run.send("hello");
    let events = run.until(is_turn_completed);
    run.until(is_state(ChatState::Idle));
    assert_eq!(
        run.states(),
        [
            ChatState::Starting,
            ChatState::Idle,
            ChatState::Running,
            ChatState::Idle
        ]
    );
    let transcript = run.transcript();
    assert_eq!(
        transcript
            .models
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
        ["muse-test"]
    );
    assert_eq!(transcript.models[0].efforts, ["low", "high"]);
    let usage = transcript.usage.expect("cumulative usage");
    assert_eq!(
        (
            usage.input_tokens,
            usage.output_tokens,
            usage.cached_input_tokens
        ),
        (100, 10, 40)
    );
    assert!(
        !transcript.items.iter().any(|item| item.id == "rc-1"),
        "reminder machinery stays out of the transcript"
    );
    assert_eq!(
        run.item("rs-1").body,
        ItemBody::Reasoning {
            text: "**Checking**\n\n**Replying**".to_owned()
        }
    );
    assert_eq!(
        run.item("tc-1").body,
        ItemBody::Command {
            command: "ls".to_owned(),
            cwd: None,
            output: "a\nb\n".to_owned(),
            exit_code: None,
        }
    );
    assert_eq!(
        deltas(&events, "msg-1"),
        [Delta::Text("Hel".into()), Delta::Text("lo".into())]
    );
    assert_eq!(
        run.item("msg-1").body,
        ItemBody::AgentMessage {
            text: "Hello".to_owned()
        }
    );
    let argv = run.fake.starts()[0]["argv"].clone();
    assert_eq!(argv[0], "serve", "{argv:?}");
    run.end();
}

#[test]
fn an_approval_is_receipted_decided_and_closed_by_the_fold() {
    let mut run = Run::start("approval_accept");
    run.send("clean up");
    let events = run.until(is_approval);
    let approval = events
        .iter()
        .find_map(|event| match event {
            ChatEvent::ApprovalRequested { approval } => Some(approval.clone()),
            _ => None,
        })
        .expect("an approval request");
    assert_eq!(approval.request_id, "ap-1");
    assert_eq!(approval.kind, ApprovalKind::Command);
    assert_eq!(approval.title, "rm -rf build");
    assert_eq!(
        approval.choices,
        [
            Decision::Accept,
            Decision::AcceptForSession,
            Decision::Decline,
            Decision::Cancel
        ]
    );
    run.until(is_state(ChatState::Waiting));
    run.driver
        .command(ChatCommand::Approve {
            request_id: "ap-1".into(),
            decision: Decision::Accept,
        })
        .unwrap();
    run.finish_turn();
    assert!(run.seen.iter().any(|event| matches!(
        event,
        ChatEvent::ApprovalResolved { request_id, decision: Decision::Accept }
            if request_id == "ap-1"
    )));
    run.end();
}

#[test]
fn a_question_is_answered_by_label_and_closed_by_the_settle() {
    let mut run = Run::start("question");
    run.send("which one?");
    let events = run.until(is_question);
    let question = events
        .iter()
        .find_map(|event| match event {
            ChatEvent::QuestionRequested { question } => Some(question.clone()),
            _ => None,
        })
        .expect("a question");
    assert_eq!(question.request_id, "ui-1");
    assert_eq!(question.questions.len(), 1);
    assert_eq!(question.questions[0].header.as_deref(), Some("Pick"));
    assert!(!question.questions[0].multi_select);
    run.driver
        .command(ChatCommand::Answer {
            request_id: "ui-1".into(),
            answers: vec![vec!["B".into()]],
        })
        .unwrap();
    run.finish_turn();
    assert!(run.seen.iter().any(|event| matches!(
        event,
        ChatEvent::QuestionResolved { request_id } if request_id == "ui-1"
    )));
    run.end();
}

#[test]
fn a_failed_turn_is_a_notice_and_the_driver_lives() {
    let mut run = Run::start("failed_turn");
    run.send("boom");
    run.finish_turn();
    assert!(run.seen.iter().any(|event| matches!(
        event,
        ChatEvent::TurnCompleted { outcome: TurnOutcome::Failed { message }, .. }
            if message == "the model exploded"
    )));
    assert_eq!(
        run.notices(),
        [(NoticeLevel::Error, "the model exploded".to_owned())]
    );
    run.end();
}

#[test]
fn a_queued_turn_is_adopted_when_it_starts() {
    let mut run = Run::start("queued_turn");
    run.send("later");
    run.finish_turn();
    assert_eq!(
        run.seen
            .iter()
            .filter(
                |event| matches!(event, ChatEvent::TurnStarted { turn_id } if turn_id == "turn-2")
            )
            .count(),
        1,
        "exactly one TurnStarted for the queued turn"
    );
    assert_eq!(
        run.item("msg-1").body,
        ItemBody::AgentMessage {
            text: "done later".to_owned()
        }
    );
    run.end();
}

#[test]
fn an_approval_update_patches_choices_and_keeps_the_prompt() {
    fn last_approval(events: &[ChatEvent]) -> Approval {
        events
            .iter()
            .rev()
            .find_map(|event| match event {
                ChatEvent::ApprovalRequested { approval } => Some(approval.clone()),
                _ => None,
            })
            .expect("an approval request")
    }
    let mut run = Run::start("approval_updated");
    run.send("clean up");
    let first = last_approval(&run.until(is_approval));
    assert_eq!(first.title, "rm -rf build");
    assert!(first.detail.contains("rm -rf build"), "{:?}", first.detail);
    let second = last_approval(&run.until(is_approval));
    assert_eq!(second.title, first.title);
    assert_eq!(second.detail, first.detail);
    assert_eq!(second.item_id, first.item_id);
    assert_eq!(second.choices, [Decision::Decline, Decision::Cancel]);
    // The fixture only accepts a decide under the refreshed requirement.
    run.driver
        .command(ChatCommand::Approve {
            request_id: "ap-1".into(),
            decision: Decision::Decline,
        })
        .unwrap();
    run.finish_turn();
    run.end();
}

#[test]
fn resume_adopts_the_running_turn_and_reapplies_the_mode() {
    let run = Run::start_with("resume_active", |config| {
        config.resume = Some("sess-old".into());
    });
    assert_eq!(run.driver.provider_thread_id().as_deref(), Some("sess-old"));
    assert!(
        run.seen.iter().any(|event| matches!(
            event,
            ChatEvent::TurnStarted { turn_id } if turn_id == "turn-9"
        )),
        "{:?}",
        run.seen
    );
    assert_eq!(
        run.item("msg-9").body,
        ItemBody::AgentMessage {
            text: "still here".to_owned()
        }
    );
    assert_eq!(
        run.fake.received_method("session/setApprovalMode").len(),
        1,
        "the approval mode is re-applied after resume"
    );
    run.end();
}

/// The real binary, headless: `muse serve` with the echo provider behind a
/// wrapper, in a throwaway home. Run with `--ignored` on a machine with Muse
/// installed; it proves the driver end to end, not the fixtures' memory of it.
#[test]
#[ignore = "live: needs the real muse binary"]
fn live_echo_turn_through_the_real_driver() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("riwork-muse-live-{}", Uuid::new_v4()));
    std::fs::create_dir_all(dir.join("home")).unwrap();
    std::fs::create_dir_all(dir.join("work")).unwrap();
    // The driver configures this process as the `cua-driver` MCP server
    // (in production that is the RiWork binary, which serves `cua mcp`);
    // the test binary cannot, and muse refuses the turn when a required
    // server fails to start — so the wrapper drops the -c pair and this
    // test covers the driver↔serve lifecycle, not the MCP wiring itself.
    let wrapper = dir.join("muse-echo");
    std::fs::write(
        &wrapper,
        "#!/usr/bin/env python3\n\
         import os, sys\n\
         args, skip = [], False\n\
         for arg in sys.argv[1:]:\n\
         \x20   if skip: skip = False; continue\n\
         \x20   if arg == '-c': skip = True; continue\n\
         \x20   args.append(arg)\n\
         os.execvp('muse', ['muse'] + args + ['--provider', 'echo'])\n",
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (sender, events) = mpsc::channel();
    let config = DriverConfig {
        provider: Provider::Muse,
        program: wrapper,
        cwd: dir.join("work"),
        approval_mode: ApprovalMode::Supervised,
        model: None,
        effort: None,
        fast: false,
        resume: None,
        resume_has_history: false,
        reporter: Default::default(),
        outstanding_notices: Default::default(),
        extra_args: Vec::new(),
        instructions: None,
        env: ["HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "RIWORK_HOME"]
            .into_iter()
            .map(|key| {
                let path = dir.join("home").join(key.to_ascii_lowercase());
                std::fs::create_dir_all(&path).unwrap();
                (key.into(), path.into_os_string())
            })
            .collect(),
        env_remove: Vec::new(),
    };
    let mut driver = start(config, sender).expect("the driver starts against real muse");
    let mut seen = until(&events, is_state(ChatState::Idle));
    assert!(driver.provider_thread_id().is_some());
    driver
        .command(ChatCommand::Send {
            text: "hello live".into(),
        })
        .unwrap();
    seen.extend(until(&events, is_turn_completed));
    seen.extend(until(&events, is_state(ChatState::Idle)));
    let transcript = fold(&seen);
    assert!(
        transcript.items.iter().any(|item| matches!(
            &item.body,
            ItemBody::AgentMessage { text } if text == "echo: hello live"
        )),
        "{:?}",
        transcript.items
    );
    driver.shutdown();
    std::fs::remove_dir_all(dir).unwrap();
}
