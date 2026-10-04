use super::*;
use crate::chat::model::{Provider, Transcript};
use crate::chat::testkit::{Fake, fixture, fold, until};
use std::env;
use std::fs;
use std::sync::mpsc::{self, Receiver};
use uuid::Uuid;

/// A driver running against the fake `codex app-server` replaying `name`.
struct Run {
    fake: Fake,
    driver: Box<dyn Driver>,
    events: Receiver<ChatEvent>,
    /// Every event so far, in order.
    seen: Vec<ChatEvent>,
}

impl Run {
    fn start(name: &str) -> Self {
        Self::start_with(name, |_| {})
    }

    fn start_with(name: &str, configure: impl FnOnce(&mut DriverConfig)) -> Self {
        let fake = Fake::new(&[&fixture(&format!("codex/{name}.ndjson"))]);
        let mut config = fake.config(Provider::Codex);
        configure(&mut config);
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

    /// Run the turn that was just sent to its end, and to the `Idle` after it.
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

    /// The notices, in order, with the level and text.
    fn notices(&self) -> Vec<(NoticeLevel, String)> {
        self.transcript()
            .items
            .into_iter()
            .filter_map(|item| match item.body {
                ItemBody::Notice { level, text } => Some((level, text)),
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

    /// Stop and check the fake never saw a frame it did not expect.
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

fn is_turn_started(event: &ChatEvent) -> bool {
    matches!(event, ChatEvent::TurnStarted { .. })
}

fn is_approval(event: &ChatEvent) -> bool {
    matches!(event, ChatEvent::ApprovalRequested { .. })
}

fn is_question(event: &ChatEvent) -> bool {
    matches!(event, ChatEvent::QuestionRequested { .. })
}

fn is_item_started(id: &'static str) -> impl Fn(&ChatEvent) -> bool {
    move |event| matches!(event, ChatEvent::ItemStarted { item } if item.id == id)
}

fn approval_of(events: &[ChatEvent]) -> Approval {
    events
        .iter()
        .find_map(|event| match event {
            ChatEvent::ApprovalRequested { approval } => Some(approval.clone()),
            _ => None,
        })
        .expect("an approval request")
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

fn text(text: &str) -> Delta {
    Delta::Text(text.into())
}

#[test]
fn a_whole_turn_becomes_items_usage_and_states() {
    let mut run = Run::start_with("turn", |config| {
        config.extra_args = vec!["-c".into(), "mcp_servers.x.command=\"y\"".into()];
        config.env = vec![("CODEX_HOME".into(), "/tmp/riwork-codex-home".into())];
    });
    assert_eq!(run.driver.provider_thread_id().as_deref(), Some("thread-1"));
    assert_eq!(run.states(), vec![ChatState::Starting, ChatState::Idle]);

    run.send("hello");
    run.finish_turn();

    let seen = run.seen.clone();
    assert_eq!(
        deltas(&seen, "msg-1"),
        vec![text("Hel"), text("lo")],
        "streamed text"
    );
    assert_eq!(
        deltas(&seen, "rs-1"),
        vec![text("**Checking**"), text("\n\n**Replying**")],
        "reasoning parts are separated"
    );
    assert_eq!(
        deltas(&seen, "exec-1"),
        vec![Delta::Output("a\n".into()), Delta::Output("b\n".into())]
    );

    let transcript = run.transcript();
    let item = |id: &str, body: ItemBody| Item {
        id: id.into(),
        turn_id: Some("turn-1".into()),
        status: ItemStatus::Completed,
        body,
    };
    assert_eq!(
        transcript.items,
        vec![
            item(
                "um-1",
                ItemBody::UserMessage {
                    text: "hello".into()
                }
            ),
            item(
                "rs-1",
                ItemBody::Reasoning {
                    text: "**Checking**\n\n**Replying**".into()
                }
            ),
            item(
                "exec-1",
                ItemBody::Command {
                    command: "ls -la".into(),
                    cwd: Some("/work".into()),
                    // Completed without aggregatedOutput: what streamed stays.
                    output: "a\nb\n".into(),
                    exit_code: Some(0),
                }
            ),
            item(
                "msg-1",
                ItemBody::AgentMessage {
                    text: "Hello".into()
                }
            ),
        ]
    );
    assert_eq!(
        transcript.usage,
        Some(Usage {
            input_tokens: 100,
            output_tokens: 10,
            cached_input_tokens: 40,
            context_window: Some(258_400),
            context_used: Some(60),
            cost_usd: None,
        })
    );
    assert_eq!(
        run.states(),
        vec![
            ChatState::Starting,
            ChatState::Idle,
            ChatState::Running,
            ChatState::Idle
        ]
    );
    assert!(transcript.turn_id.is_none());

    // What the driver sent.
    let start = &run.fake.starts()[0];
    assert_eq!(
        start["argv"],
        json!(["-c", "mcp_servers.x.command=\"y\"", "app-server"])
    );
    assert_eq!(start["env"]["CODEX_HOME"], "/tmp/riwork-codex-home");
    let turn = &run.fake.received_method("turn/start")[0]["params"];
    assert_eq!(turn["summary"], "auto");
    run.driver.shutdown();
    assert_eq!(
        run.until(is_state(ChatState::Stopped)).last(),
        Some(&ChatEvent::State {
            state: ChatState::Stopped
        })
    );
    assert!(
        run.fake.saw("eof"),
        "the input was closed, not the process killed"
    );
    assert!(!run.fake.saw("mismatch"));
}

#[test]
fn an_accepted_command_approval_offers_what_codex_offered() {
    let mut run = Run::start("approval_accept");
    run.send("write it");
    let events = run.until(is_approval);
    assert_eq!(
        approval_of(&events),
        Approval {
            request_id: "0".into(),
            item_id: Some("exec-1".into()),
            kind: ApprovalKind::Command,
            title: "echo hi > f".into(),
            detail: "May I write the file?\nin /work".into(),
            // The execpolicy amendment is not offered.
            choices: vec![Decision::Accept, Decision::Cancel],
        }
    );
    run.until(is_state(ChatState::Waiting));
    assert_eq!(run.transcript().approvals.len(), 1);

    run.driver
        .command(ChatCommand::Approve {
            request_id: "0".into(),
            decision: Decision::Accept,
        })
        .unwrap();
    assert!(
        run.until(is_state(ChatState::Running))
            .contains(&ChatEvent::ApprovalResolved {
                request_id: "0".into(),
                decision: Decision::Accept
            })
    );
    run.finish_turn();

    let transcript = run.transcript();
    assert!(transcript.approvals.is_empty());
    assert_eq!(transcript.items[0].status, ItemStatus::Completed);
    // Answering twice is an error, not a second response.
    assert!(
        run.driver
            .command(ChatCommand::Approve {
                request_id: "0".into(),
                decision: Decision::Accept
            })
            .is_err()
    );
    run.end();
}

#[test]
fn a_declined_file_change_is_reported_as_declined() {
    let mut run = Run::start("approval_decline");
    run.send("add a file");
    let events = run.until(is_approval);
    let approval = approval_of(&events);
    assert_eq!(approval.request_id, "req-7");
    assert_eq!(approval.kind, ApprovalKind::FileChange);
    assert_eq!(approval.title, "Change /work/new.txt");
    // The diff of an added file is shown the way a diff is.
    assert_eq!(approval.detail, "/work/new.txt\n@@ -0,0 +1,1 @@\n+new");
    assert_eq!(
        approval.choices,
        vec![
            Decision::Accept,
            Decision::AcceptForSession,
            Decision::Decline,
            Decision::Cancel
        ]
    );
    run.driver
        .command(ChatCommand::Approve {
            request_id: "req-7".into(),
            decision: Decision::Decline,
        })
        .unwrap();
    run.finish_turn();
    let change = run.item("fc-1");
    assert_eq!(change.status, ItemStatus::Declined);
    assert_eq!(
        change.body,
        ItemBody::FileChange {
            changes: vec![FileChange {
                path: "/work/new.txt".into(),
                kind: ChangeKind::Add,
                diff: Some("@@ -0,0 +1,1 @@\n+new".into()),
            }]
        }
    );
    run.end();
}

#[test]
fn questions_and_permission_requests_are_answered_by_request_id() {
    let mut run = Run::start("question");
    run.send("ask me");
    let events = run.until(is_question);
    let Some(ChatEvent::QuestionRequested { question }) = events.last() else {
        panic!("{events:#?}");
    };
    assert_eq!(question.request_id, "5");
    assert_eq!(question.questions.len(), 2);
    assert_eq!(question.questions[0].header.as_deref(), Some("Schema"));
    assert_eq!(
        question.questions[0].options[0],
        QuestionOption {
            label: "Strict (Recommended)".into(),
            description: "Validate tightly.".into()
        }
    );
    // An empty header is none; no options is none.
    assert_eq!(question.questions[1].header, None);
    assert!(question.questions[1].options.is_empty());
    run.until(is_state(ChatState::Waiting));

    // An approval cannot answer a question, nor the reverse.
    assert!(
        run.driver
            .command(ChatCommand::Approve {
                request_id: "5".into(),
                decision: Decision::Accept
            })
            .is_err()
    );
    run.driver
        .command(ChatCommand::Answer {
            request_id: "5".into(),
            answers: vec![
                vec!["Strict (Recommended)".into()],
                vec!["no thanks".into()],
            ],
        })
        .unwrap();
    let events = run.until(is_approval);
    assert!(events.contains(&ChatEvent::QuestionResolved {
        request_id: "5".into()
    }));
    let approval = approval_of(&events);
    assert_eq!(approval.kind, ApprovalKind::Permissions);
    assert_eq!(approval.choices.len(), 3);
    assert!(approval.detail.starts_with("Needs the network"));
    run.driver
        .command(ChatCommand::Approve {
            request_id: "6".into(),
            decision: Decision::AcceptForSession,
        })
        .unwrap();
    run.finish_turn();
    run.end();
}

#[test]
fn interrupt_asks_codex_to_stop_the_turn_and_closes_what_it_left_open() {
    let mut run = Run::start("interrupt");
    run.send("sleep");
    run.until(is_item_started("exec-1"));
    run.driver.command(ChatCommand::Interrupt).unwrap();
    let events = run.until(is_turn_completed);
    assert_eq!(
        events.last(),
        Some(&ChatEvent::TurnCompleted {
            turn_id: "turn-1".into(),
            outcome: TurnOutcome::Interrupted
        })
    );
    run.until(is_state(ChatState::Idle));
    assert_eq!(run.item("exec-1").status, ItemStatus::Interrupted);
    assert_eq!(
        run.fake.received_method("turn/interrupt")[0]["params"],
        json!({"threadId": "thread-1", "turnId": "turn-1"})
    );
    // With nothing running, an interrupt does nothing.
    run.driver.command(ChatCommand::Interrupt).unwrap();
    assert_eq!(run.fake.received_method("turn/interrupt").len(), 1);
    run.end();
}

#[test]
fn a_message_sent_while_a_turn_runs_steers_it() {
    let mut run = Run::start("steer");
    run.send("first");
    run.until(is_turn_started);
    run.send("second");
    run.finish_turn();
    let texts: Vec<String> = run
        .transcript()
        .items
        .into_iter()
        .filter_map(|item| match item.body {
            ItemBody::UserMessage { text } => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(texts, vec!["first", "second"]);
    assert_eq!(run.fake.received_method("turn/start").len(), 1);
    assert_eq!(run.fake.received_method("turn/steer").len(), 1);
    run.end();
}

#[test]
fn a_message_sent_before_the_turn_id_is_known_steers_once_it_is() {
    let mut run = Run::start("queued");
    run.send("first");
    run.send("second");
    run.finish_turn();
    assert_eq!(run.fake.received_method("turn/start").len(), 1);
    let steers = run.fake.received_method("turn/steer");
    assert_eq!(steers.len(), 1);
    assert_eq!(steers[0]["params"]["input"][0]["text"], "second");
    run.end();
}

#[test]
fn a_steer_that_loses_the_race_with_the_end_of_the_turn_starts_the_next_turn() {
    let mut run = Run::start("steer_race");
    run.send("first");
    run.until(is_item_started("um-1"));
    run.send("second");
    // Turn one ends, then the refused steer becomes turn two.
    run.until(is_turn_completed);
    run.finish_turn();
    let transcript = run.transcript();
    assert_eq!(run.item("msg-2").status, ItemStatus::Completed);
    assert!(
        transcript
            .items
            .iter()
            .all(|item| !matches!(item.body, ItemBody::Notice { .. })),
        "the refusal is not reported: the message went through"
    );
    assert_eq!(run.fake.received_method("turn/start").len(), 2);
    run.end();
}

#[test]
fn resuming_opens_the_given_thread_without_its_history() {
    let mut run = Run::start_with("resume", |config| config.resume = Some("thread-9".into()));
    assert_eq!(run.driver.provider_thread_id().as_deref(), Some("thread-9"));
    // The usage Codex replays while resuming reaches the chat.
    assert_eq!(
        run.transcript().usage.map(|usage| usage.input_tokens),
        Some(100)
    );
    run.send("again");
    run.finish_turn();
    assert_eq!(
        run.fake.received_method("turn/start")[0]["params"]["threadId"],
        "thread-9"
    );
    assert!(run.fake.received_method("thread/start").is_empty());
    run.end();
}

#[test]
fn resuming_a_thread_codex_never_saved_opens_a_new_one_and_says_so() {
    let run = Run::start_with("resume_missing", |config| {
        config.resume = Some("thread-9".into())
    });
    assert_eq!(run.driver.provider_thread_id().as_deref(), Some("thread-2"));
    let notices = run.notices();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].0, NoticeLevel::Info);
    assert!(notices[0].1.contains("thread-9"), "{notices:?}");
    run.end();
}

#[test]
fn any_other_resume_failure_fails_the_start() {
    let fake = Fake::new(&[&fixture("codex/resume_error.ndjson")]);
    let mut config = fake.config(Provider::Codex);
    config.resume = Some("thread-9".into());
    let (sender, _events) = mpsc::channel();
    let error = start(config, sender).err().unwrap();
    assert!(error.contains("database is locked"), "{error}");
}

#[test]
fn configure_applies_on_the_next_turn_and_plan_mode_is_left_explicitly() {
    let mut run = Run::start_with("configure", |config| config.model = Some("gpt-x".into()));
    run.driver
        .command(ChatCommand::Configure {
            model: None,
            effort: Some("high".into()),
            approval_mode: Some(ApprovalMode::Plan),
        })
        .unwrap();
    run.send("plan it");
    run.finish_turn();
    run.driver
        .command(ChatCommand::Configure {
            model: None,
            effort: None,
            approval_mode: Some(ApprovalMode::AutoEdit),
        })
        .unwrap();
    run.send("do it");
    run.finish_turn();
    run.send("again");
    run.finish_turn();
    let starts = run.fake.received_method("turn/start");
    assert!(
        starts[0]["params"]["collaborationMode"]["settings"]["developer_instructions"]
            .as_str()
            .unwrap()
            .contains("<proposed_plan>")
    );
    assert!(starts[2]["params"].get("collaborationMode").is_none());
    run.end();
}

#[test]
fn a_refused_steer_waits_and_starts_the_next_turn() {
    let mut run = Run::start("steer_refused");
    run.send("first");
    run.until(is_item_started("um-1"));
    run.send("second");
    // The second turn follows the first without an idle moment between.
    run.finish_turn();
    assert_eq!(run.fake.received_method("turn/start").len(), 2);
    assert!(run.notices().is_empty(), "the refusal is not shown");
    run.end();
}

#[test]
fn a_message_after_an_interrupt_starts_a_turn_of_its_own() {
    let mut run = Run::start("interrupt_then_send");
    run.send("first");
    run.until(is_item_started("um-1"));
    run.driver.command(ChatCommand::Interrupt).unwrap();
    run.send("second");
    run.finish_turn();
    assert!(run.fake.received_method("turn/steer").is_empty());
    let outcomes: Vec<TurnOutcome> = run
        .seen
        .iter()
        .filter_map(|event| match event {
            ChatEvent::TurnCompleted { outcome, .. } => Some(outcome.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        outcomes,
        vec![TurnOutcome::Interrupted, TurnOutcome::Completed]
    );
    run.end();
}

#[test]
fn a_question_that_does_not_block_leaves_the_turn_running_but_can_be_answered() {
    let mut run = Run::start("question_nonblocking");
    run.send("ask");
    run.until(is_question);
    run.driver
        .command(ChatCommand::Answer {
            request_id: "5".into(),
            answers: vec![vec!["yes".into()]],
        })
        .unwrap();
    run.finish_turn();
    assert!(
        !run.states().contains(&ChatState::Waiting),
        "{:?}",
        run.states()
    );
    run.end();
}

#[test]
fn a_turn_start_result_that_arrives_after_the_turn_ended_does_not_reopen_it() {
    let mut run = Run::start("late_response");
    run.send("quick");
    run.finish_turn();
    // The warning follows the late result, so the result has been read.
    run.until(|event| matches!(event, ChatEvent::ItemCompleted { item } if matches!(item.body, ItemBody::Notice { .. })));
    assert_eq!(
        run.states(),
        vec![
            ChatState::Starting,
            ChatState::Idle,
            ChatState::Running,
            ChatState::Idle
        ]
    );
    let started = run
        .seen
        .iter()
        .filter(|event| is_turn_started(event))
        .count();
    assert_eq!(started, 1, "the late result opened a turn again");
    run.end();
}

#[test]
fn a_resumed_plan_mode_thread_is_taken_out_of_plan_mode_by_the_next_turn() {
    let mut run = Run::start_with("resume_plan", |config| {
        config.resume = Some("thread-9".into());
        config.approval_mode = ApprovalMode::AutoEdit;
    });
    run.send("go");
    run.finish_turn();
    run.end();
}

#[test]
fn compacting_shows_a_compaction_item() {
    let mut run = Run::start("compact");
    run.driver.command(ChatCommand::Compact).unwrap();
    run.finish_turn();
    assert_eq!(run.item("cc-1").body, ItemBody::Compaction);
    assert_eq!(run.item("cc-1").status, ItemStatus::Completed);
    assert_eq!(
        run.states(),
        vec![
            ChatState::Starting,
            ChatState::Idle,
            ChatState::Running,
            ChatState::Idle
        ]
    );
    run.end();
}

#[test]
fn plans_tools_searches_and_file_changes_map_to_items() {
    let mut run = Run::start("items");
    run.send("everything");
    run.finish_turn();
    let transcript = run.transcript();
    // Notice ids carry a per-session prefix; the rest are Codex's.
    let ids: Vec<&str> = transcript
        .items
        .iter()
        .map(|item| match item.id.starts_with("notice-") {
            true => "notice",
            false => item.id.as_str(),
        })
        .collect();
    // Nothing from the sub-agent's thread, and nothing for the reasoning
    // item that had no text.
    assert_eq!(
        ids,
        vec![
            "plan-turn-1",
            "turn-1-plan",
            "mcp-1",
            "mcp-2",
            "ws-1",
            "fc-1",
            "notice",
            "retry-turn-1"
        ]
    );
    let step = |text: &str, status| Step {
        text: text.into(),
        status,
    };
    assert_eq!(
        run.item("plan-turn-1").body,
        ItemBody::Plan {
            explanation: Some("Two steps".into()),
            steps: vec![
                step("Inspect", StepStatus::Completed),
                step("Report", StepStatus::InProgress)
            ],
        }
    );
    assert_eq!(
        run.item("turn-1-plan").body,
        ItemBody::Plan {
            explanation: Some("1. Do the thing".into()),
            steps: Vec::new(),
        }
    );
    assert_eq!(
        run.item("mcp-1").body,
        ItemBody::ToolCall {
            server: Some("cua-driver".into()),
            tool: "list_apps".into(),
            input: json!({"all": true}),
            output: Some("Finder\nSafari".into()),
        }
    );
    let failed = run.item("mcp-2");
    assert_eq!(failed.status, ItemStatus::Failed);
    assert!(matches!(
        failed.body,
        ItemBody::ToolCall { output: Some(ref output), .. } if output == "no such window"
    ));
    assert_eq!(
        run.item("ws-1").body,
        ItemBody::WebSearch {
            query: "rust process groups".into()
        }
    );
    assert_eq!(
        run.item("fc-1").body,
        ItemBody::FileChange {
            changes: vec![
                FileChange {
                    path: "/work/a.rs".into(),
                    kind: ChangeKind::Modify,
                    diff: Some("@@ -1 +1 @@\n-old\n+new\n".into()),
                },
                FileChange {
                    path: "/work/b.rs".into(),
                    kind: ChangeKind::Rename,
                    diff: Some("@@ -1 +1 @@\n-x\n+y\n".into()),
                },
                FileChange {
                    path: "/work/gone.rs".into(),
                    kind: ChangeKind::Delete,
                    diff: Some("@@ -1,1 +0,0 @@\n-bye".into()),
                },
            ]
        }
    );
    assert_eq!(
        run.notices()[0],
        (NoticeLevel::Warning, "Heads up".to_owned())
    );
    // An error that Codex will retry is a warning.
    assert_eq!(
        run.item("retry-turn-1").body,
        ItemBody::Notice {
            level: NoticeLevel::Warning,
            text: "Reconnecting 1/5".into()
        }
    );
    run.end();
}

#[test]
fn a_failed_turn_says_why_once() {
    let mut run = Run::start("failed_turn");
    run.send("fail");
    let events = run.until(is_turn_completed);
    assert_eq!(
        events.last(),
        Some(&ChatEvent::TurnCompleted {
            turn_id: "turn-1".into(),
            outcome: TurnOutcome::Failed {
                message: "usage limit reached".into()
            }
        })
    );
    run.until(is_state(ChatState::Idle));
    let notices: Vec<ItemBody> = run
        .transcript()
        .items
        .into_iter()
        .map(|item| item.body)
        .collect();
    assert_eq!(
        notices,
        vec![ItemBody::Notice {
            level: NoticeLevel::Error,
            text: "usage limit reached\ntry later".into()
        }]
    );
    run.end();
}

#[test]
fn an_oversized_line_is_skipped_with_a_notice_and_the_turn_goes_on() {
    let mut run = Run::start("oversized");
    run.send("big");
    run.finish_turn();
    let transcript = run.transcript();
    let notice = transcript
        .items
        .iter()
        .find_map(|item| match &item.body {
            ItemBody::Notice { level, text } => Some((*level, text.clone())),
            _ => None,
        })
        .expect("a notice");
    assert_eq!(notice.0, NoticeLevel::Warning);
    assert!(
        notice.1.contains("oversized") && notice.1.contains("18 MB"),
        "{notice:?}"
    );
    assert_eq!(
        run.item("msg-1").body,
        ItemBody::AgentMessage {
            text: "still here".into()
        }
    );
    assert_eq!(run.item("msg-1").status, ItemStatus::Completed);
    run.end();
}

#[test]
fn a_crash_mid_turn_fails_the_turn_and_the_chat_with_the_reason() {
    let mut run = Run::start("crash");
    run.send("crash");
    let events = run.until(is_turn_completed);
    let Some(ChatEvent::TurnCompleted { turn_id, outcome }) = events.last() else {
        panic!("{events:#?}");
    };
    assert_eq!(turn_id, "turn-1");
    let TurnOutcome::Failed { message } = outcome else {
        panic!("{outcome:?}");
    };
    assert!(
        message.contains("kaboom") && message.contains("exit status: 3"),
        "{message}"
    );
    let events = run.until(|event| {
        matches!(
            event,
            ChatEvent::State {
                state: ChatState::Failed { .. }
            }
        )
    });
    let Some(ChatEvent::State {
        state: ChatState::Failed { message: chat },
    }) = events.last()
    else {
        panic!("{events:#?}");
    };
    assert_eq!(chat, message);
    assert_eq!(run.item("msg-1").status, ItemStatus::Failed);
    assert!(run.transcript().turn_id.is_none());
    // A dead chat takes no more commands; it is resumed by a new driver.
    assert!(
        run.driver
            .command(ChatCommand::Send { text: "hi".into() })
            .is_err()
    );
    run.driver.shutdown();
    // Stopping a failed chat leaves it failed.
    assert!(matches!(run.transcript().state, ChatState::Failed { .. }));
}

#[test]
fn a_program_that_dies_at_startup_is_an_error_with_its_stderr() {
    let fake = Fake::new(&[&fixture("codex/exit_at_start.ndjson")]);
    let (sender, events) = mpsc::channel();
    let error = start(fake.config(Provider::Codex), sender).err().unwrap();
    assert!(
        error.contains("not logged in") && error.contains("exit status: 2"),
        "{error}"
    );
    // The events end with the failure, for subscribers.
    assert_eq!(
        events.try_iter().collect::<Vec<_>>(),
        vec![
            ChatEvent::State {
                state: ChatState::Starting
            },
            ChatEvent::State {
                state: ChatState::Failed { message: error }
            }
        ]
    );
}

#[test]
fn a_missing_program_is_an_error_naming_it() {
    let fake = Fake::new(&[&fixture("codex/idle.ndjson")]);
    let mut config = fake.config(Provider::Codex);
    config.program = "/nonexistent/riwork-codex".into();
    let (sender, events) = mpsc::channel();
    let error = start(config, sender).err().unwrap();
    assert!(error.contains("/nonexistent/riwork-codex"), "{error}");
    assert_eq!(
        events.try_iter().last(),
        Some(ChatEvent::State {
            state: ChatState::Failed { message: error }
        })
    );
}

#[test]
fn stop_ends_the_process_and_later_commands_fail() {
    let mut run = Run::start("idle");
    run.driver.command(ChatCommand::Stop).unwrap();
    run.until(is_state(ChatState::Stopped));
    assert!(run.fake.saw("eof"));
    assert!(
        run.driver
            .command(ChatCommand::Send { text: "hi".into() })
            .is_err()
    );
    // Stopping again is harmless and says nothing more.
    run.driver.shutdown();
    assert!(run.events.try_recv().is_err());
}

#[test]
fn dropping_the_driver_stops_the_process() {
    let run = Run::start("idle");
    let Run {
        fake,
        driver,
        events,
        ..
    } = run;
    drop(driver);
    until(&events, is_state(ChatState::Stopped));
    assert!(fake.saw("eof"));
}

#[test]
fn shell_wrappers_are_stripped_from_commands() {
    for (given, shown) in [
        (r#"/bin/bash -lc "git status""#, "git status"),
        (r#"bash -lc 'echo "hi there"'"#, r#"echo "hi there""#),
        (
            r#"/bin/zsh -lc "echo it's \"fine\"""#,
            r#"echo it's "fine""#,
        ),
        (r#"sh -c 'echo '"'"'x'"'"''"#, "echo 'x'"),
        ("ls -la", "ls -la"),
        (r#"bash -lc "unterminated"#, r#"bash -lc "unterminated"#),
        ("bash -lc two words", "bash -lc two words"),
        ("python3 -c 'print(1)'", "python3 -c 'print(1)'"),
    ] {
        assert_eq!(display_command(given), shown, "{given}");
    }
}

#[test]
fn approval_modes_map_to_codex_policies() {
    let table = [
        (ApprovalMode::Supervised, "untrusted", "read-only", false),
        (
            ApprovalMode::AutoEdit,
            "on-request",
            "workspace-write",
            false,
        ),
        (ApprovalMode::Full, "never", "danger-full-access", false),
        (ApprovalMode::Plan, "never", "read-only", true),
    ];
    for (mode, approval, sandbox, plan) in table {
        let policy = policy(mode);
        assert_eq!(
            (policy.approval, policy.sandbox_mode, policy.plan),
            (approval, sandbox, plan),
            "{mode:?}"
        );
    }
    assert_eq!(
        policy(ApprovalMode::Full).sandbox,
        json!({"type": "dangerFullAccess"})
    );
}

#[test]
fn a_refusal_falls_back_to_the_nearest_offered_decision() {
    let offered = [Decision::Accept, Decision::Cancel];
    assert_eq!(pick(Decision::Decline, &offered), Decision::Cancel);
    assert_eq!(pick(Decision::AcceptForSession, &offered), Decision::Accept);
    assert_eq!(pick(Decision::Accept, &offered), Decision::Accept);
    let all = [Decision::Accept, Decision::Decline];
    assert_eq!(pick(Decision::Decline, &all), Decision::Decline);
    // Without a list, a command offers every decision.
    assert_eq!(command_choices(&json!({})).len(), 4);
    assert_eq!(
        command_choices(&json!({"availableDecisions": [{"applyNetworkPolicyAmendment": {}}]}))
            .len(),
        4
    );
}

#[test]
fn deleted_and_added_files_get_unified_diffs_and_real_diffs_pass_through() {
    assert_eq!(
        unified(ChangeKind::Add, "a\nb\n"),
        "@@ -0,0 +1,2 @@\n+a\n+b"
    );
    assert_eq!(unified(ChangeKind::Delete, "a\n"), "@@ -1,1 +0,0 @@\n-a");
    let diff = "--- a/f\n+++ b/f\n@@ -1 +1 @@\n-x\n+y\n";
    assert_eq!(unified(ChangeKind::Add, diff), diff);
    assert_eq!(unified(ChangeKind::Modify, "plain"), "plain");
    // A new file that starts like a diff header (front matter) is still content.
    assert_eq!(
        unified(ChangeKind::Add, "---\ntitle: x\n---\n"),
        "@@ -0,0 +1,3 @@\n+---\n+title: x\n+---"
    );
}

#[test]
fn unknown_items_are_not_shown() {
    for kind in ["hookPrompt", "functionCallOutput", "sleep", "somethingNew"] {
        assert!(convert_item(&json!({"type": kind, "id": "x"}), true).is_none());
    }
    let (body, status) = convert_item(
        &json!({"type": "collabAgentToolCall", "id": "x", "tool": "spawnAgent", "status": "inProgress"}),
        false,
    )
    .unwrap();
    assert_eq!(status, ItemStatus::InProgress);
    assert!(matches!(body, ItemBody::ToolCall { ref tool, .. } if tool == "collabAgentToolCall"));
}

/// Opt-in smoke test against the real `codex app-server`; it spends tokens.
///
/// `RIWORK_LIVE_CODEX=1 cargo test live_codex -- --ignored --nocapture`
/// (`RIWORK_LIVE_CODEX_PROGRAM` names the executable, `codex` by default).
#[test]
#[ignore = "starts the real codex and spends tokens"]
fn live_codex_answers_a_trivial_prompt() {
    if env::var_os("RIWORK_LIVE_CODEX").is_none() {
        return;
    }
    let dir = env::temp_dir().join(format!("riwork-live-codex-{}", Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    let config = DriverConfig {
        provider: Provider::Codex,
        program: env::var_os("RIWORK_LIVE_CODEX_PROGRAM")
            .unwrap_or_else(|| "codex".into())
            .into(),
        cwd: dir.clone(),
        approval_mode: ApprovalMode::Supervised,
        model: None,
        effort: None,
        resume: None,
        extra_args: Vec::new(),
        env: Vec::new(),
        env_remove: Vec::new(),
    };
    let (sender, events) = mpsc::channel();
    let mut driver = start(config, sender).expect("codex starts");
    assert!(driver.provider_thread_id().is_some());
    driver
        .command(ChatCommand::Send {
            text: "Reply with the single word: pong".into(),
        })
        .unwrap();
    let seen = until(&events, is_turn_completed);
    let transcript = fold(&seen);
    assert!(
        transcript
            .items
            .iter()
            .any(|item| matches!(&item.body, ItemBody::AgentMessage { text } if !text.is_empty())),
        "{seen:#?}"
    );
    assert!(matches!(
        seen.last(),
        Some(ChatEvent::TurnCompleted {
            outcome: TurnOutcome::Completed,
            ..
        })
    ));
    driver.shutdown();
    let _ = fs::remove_dir_all(dir);
}
