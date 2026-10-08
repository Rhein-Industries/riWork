use super::*;
use crate::chat::model::{Provider, Transcript};
use crate::chat::testkit::{Fake, fixture, fold, until};
use std::env;
use std::fs;
use std::sync::mpsc::{self, Receiver};
use uuid::Uuid;

#[test]
fn attachment_late_turn_receipt_cannot_steer_queued_text_into_a_newer_turn() {
    let fake = Fake::new(&[]);
    let (tx, _rx) = mpsc::channel();
    let mut session = Session::new(&fake.config(Provider::Codex), tx);
    session.thread_id = Some("thread".into());
    session.ready = true;
    session.begin_turn("old");
    session.finish_turn("old", TurnOutcome::Completed);
    session.begin_turn("new");
    session.queued.push("for the newer turn".into());
    assert!(session.begin_turn("old").is_empty());
    assert_eq!(session.turn.as_deref(), Some("new"));
    assert_eq!(session.queued, vec!["for the newer turn"]);
    assert!(session.begin_turn("").is_empty());
    assert_eq!(session.turn.as_deref(), Some("new"));
}

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
        presentation: crate::chat::model::Presentation {
            phase: matches!(body, ItemBody::AgentMessage { .. })
                .then_some(crate::chat::model::MessagePhase::Final),
            images: vec![],
            ..Default::default()
        },
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

    assert_notice(
        &completed_notices(&run.seen)[0],
        notice_kind::RESUMED_FRESH,
        false,
    );
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
            fast: None,
        })
        .unwrap();
    run.send("plan it");
    run.finish_turn();
    run.driver
        .command(ChatCommand::Configure {
            model: None,
            effort: None,
            approval_mode: Some(ApprovalMode::AutoEdit),
            fast: None,
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
            level: NoticeLevel::Info,
            text: "Reconnected.".into(),
            kind: Some(notice_kind::RECONNECTING.into()),
            resolved: true,
            resets_at: None
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
        vec![ItemBody::notice(
            NoticeLevel::Error,
            "usage limit reached\ntry later",
            Some(notice_kind::PROVIDER_ERROR.into())
        )]
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
            ItemBody::Notice { level, text, .. } => Some((*level, text.clone())),
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

    assert_notice(
        &completed_notices(&run.seen)[0],
        notice_kind::OVERSIZED_LINE,
        false,
    );
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

fn is_models(event: &ChatEvent) -> bool {
    matches!(event, ChatEvent::Models { .. })
}

fn models_of(events: &[ChatEvent]) -> Vec<ModelOption> {
    match events.last() {
        Some(ChatEvent::Models { models }) => models.clone(),
        other => panic!("no Models event: {other:?}"),
    }
}

fn configure(model: Option<&str>, effort: Option<&str>, fast: Option<bool>) -> ChatCommand {
    ChatCommand::Configure {
        model: model.map(str::to_owned),
        effort: effort.map(str::to_owned),
        approval_mode: None,
        fast,
    }
}

/// `model/list` as codex 0.160.0 answered it with an empty `CODEX_HOME`, hidden models
/// included (`testdata/codex/model_list.json`).
fn recorded_model_list() -> Value {
    serde_json::from_str(&fixture("codex/model_list.json")).unwrap()
}

#[test]
fn a_recorded_model_list_maps_to_the_models_the_picker_offers() {
    let list = recorded_model_list();
    assert_eq!(list["data"].as_array().unwrap().len(), 11);
    let models = model_options(&list["data"]);
    // Hidden models (the two special-purpose ones and the auto-review model) are not offered.
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "gpt-6.1-sol",
            "gpt-6-astra",
            "gpt-6-sol",
            "gpt-6-luna",
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
            "gpt-5.5"
        ]
    );
    assert_eq!(
        models[0],
        ModelOption {
            id: "gpt-6.1-sol".into(),
            name: "GPT-6.1-Sol".into(),
            description: "Latest workhorse model for coding and everyday work.".into(),
            efforts: ["low", "medium", "high", "xhigh", "max", "ultra"]
                .map(str::to_owned)
                .into(),
            default_effort: Some("low".into()),
            supports_fast: true,
            is_default: true,
        }
    );
    // One default; every offered model has the `priority` tier; the efforts differ by model.
    assert_eq!(models.iter().filter(|m| m.is_default).count(), 1);
    assert!(models.iter().all(|m| m.supports_fast));
    let luna = models.iter().find(|m| m.id == "gpt-6-luna").unwrap();
    assert_eq!(luna.efforts.last().map(String::as_str), Some("max"));
    let last = models.last().unwrap();
    assert_eq!(
        (
            last.name.as_str(),
            last.efforts.len(),
            last.default_effort.as_deref()
        ),
        ("GPT-5.5", 4, Some("medium"))
    );
    // A hidden model keeps its facts when it is shown anyway: no tier, no Fast.
    let blue = &list["data"].as_array().unwrap()[7];
    assert_eq!(blue["id"], "gpt-daybreak-blue-latest");
    let mut shown = blue.clone();
    shown["hidden"] = json!(false);
    assert!(!model_option(&shown).unwrap().supports_fast);
}

#[test]
fn a_model_has_fast_when_it_lists_the_priority_tier_or_an_older_server_says_fast() {
    let model = |extra: Value| {
        let mut base = json!({"id": "m", "model": "m", "displayName": "M", "hidden": false,
                              "supportedReasoningEfforts": [], "defaultReasoningEffort": "low"});
        base.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        model_option(&base).unwrap()
    };
    let tiers = |ids: &[&str]| json!({"serviceTiers": ids.iter().map(|id| json!({"id": id, "name": "n", "description": "d"})).collect::<Vec<_>>()});
    assert!(model(tiers(&["priority"])).supports_fast);
    assert!(!model(tiers(&[])).supports_fast);
    // Some other tier is not Fast.
    assert!(!model(tiers(&["flex"])).supports_fast);
    // `serviceTiers` wins over the deprecated `additionalSpeedTiers`; without it, the latter counts.
    assert!(!model(json!({"serviceTiers": [], "additionalSpeedTiers": ["fast"]})).supports_fast);
    assert!(model(json!({"additionalSpeedTiers": ["fast"]})).supports_fast);
    assert!(!model(json!({})).supports_fast);
    // Without a display name the id is the name; `model` is what turn/start takes.
    let bare = model_option(&json!({"id": "a", "model": "b"})).unwrap();
    assert_eq!((bare.id.as_str(), bare.name.as_str()), ("b", "b"));
    assert!(model_option(&json!({"displayName": "no id"})).is_none());
    assert!(model_options(&json!(null)).is_empty());
}

#[test]
fn the_listed_models_come_as_an_event_from_every_page_and_fast_and_efforts_follow_them() {
    let mut run = Run::start_with("models_fast", |config| {
        config.model = Some("gpt-6.1-sol".into());
        config.effort = Some("high".into());
        config.fast = true;
    });
    let models = models_of(&run.until(is_models));
    // Both pages: three, then the other eight (three of them hidden) and one more.
    assert_eq!(models.len(), 9);
    assert_eq!(models[0].id, "gpt-6.1-sol");
    assert_eq!(models[8].id, "plain-model");
    assert!(models[0].supports_fast && !models[8].supports_fast);
    assert_eq!(run.fake.received_method("model/list").len(), 2);
    assert_eq!(
        run.fake.received_method("model/list")[1]["params"],
        json!({"cursor": "3"})
    );
    // The thread was opened in Fast mode: the tier is `priority`.
    assert_eq!(
        run.fake.received_method("thread/start")[0]["params"]["serviceTier"],
        "priority"
    );

    // Fast on: the tier rides on the turn.
    run.send("one");
    run.finish_turn();
    // Fast off: the standard tier is said, since a tier set on a turn stays.
    run.driver
        .command(configure(None, None, Some(false)))
        .unwrap();
    run.send("two");
    run.finish_turn();
    // A model without the tier gets none, and an effort it does not list is dropped with a
    // notice, once. Choosing another such effort is refused with a notice and changes nothing.
    run.driver
        .command(configure(Some("plain-model"), None, Some(true)))
        .unwrap();
    run.send("three");
    run.finish_turn();
    run.driver
        .command(configure(None, Some("max"), None))
        .unwrap();
    // Back on a model that takes both.
    run.driver
        .command(configure(Some("gpt-6-sol"), None, None))
        .unwrap();
    run.send("four");
    run.finish_turn();

    let turns = run.fake.received_method("turn/start");
    let tier = |turn: usize| turns[turn]["params"].get("serviceTier").cloned();
    let effort = |turn: usize| turns[turn]["params"].get("effort").cloned();
    assert_eq!(
        (tier(0), tier(1), tier(2), tier(3)),
        (
            Some(json!("priority")),
            Some(json!("default")),
            None,
            Some(json!("priority"))
        )
    );
    assert_eq!(
        (effort(0), effort(1), effort(2), effort(3)),
        (
            Some(json!("high")),
            Some(json!("high")),
            None,
            Some(json!("high"))
        )
    );
    let notices: Vec<String> = run.notices().into_iter().map(|(_, text)| text).collect();
    assert_eq!(notices.len(), 2, "{notices:?}");
    assert!(
        notices[0].contains("plain-model") && notices[0].contains("high"),
        "{notices:?}"
    );
    assert!(
        notices[1].contains("plain-model") && notices[1].contains("max"),
        "{notices:?}"
    );

    assert_notice(
        &completed_notices(&run.seen)[0],
        notice_kind::EFFORT_REFUSED,
        false,
    );
    run.end();
}

#[test]
fn without_a_model_list_fast_is_sent_when_asked_for_and_taken_back_once() {
    // A server that does not answer model/list (all the older fixtures): nothing is known
    // about the models, so Fast goes to the server, which decides.
    let (events, _unused) = mpsc::channel();
    let fake = Fake::new(&[&fixture("codex/idle.ndjson")]);
    let mut config = fake.config(Provider::Codex);
    config.fast = true;
    let mut session = Session::new(&config, events);
    assert_eq!(session.service_tier(), Some(FAST_TIER));
    assert_eq!(session.service_tier(), Some(FAST_TIER));
    session.settings.fast = false;
    // The tier sticks on the server: the standard one is said once, then the field is left out.
    assert_eq!(session.service_tier(), Some(STANDARD_TIER));
    assert_eq!(session.service_tier(), None);
    // A chat that never asked for Fast never sends a tier.
    config.fast = false;
    let (events, _unused) = mpsc::channel();
    let mut session = Session::new(&config, events);
    assert_eq!(session.service_tier(), None);
    // Efforts cannot be checked either, and go as they are.
    session.settings.effort = Some("anything".into());
    assert_eq!(session.effort_to_send().as_deref(), Some("anything"));
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
        fast: false,
        resume: None,
        outstanding_notices: Default::default(),
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

#[test]
fn provider_phase_and_real_image_payload_reach_the_log_without_raw_bitmap_text() {
    let fake = Fake::new(&[]);
    let (sender, receiver) = mpsc::channel();
    let mut core = Session::new(&fake.config(Provider::Codex), sender);
    core.item(
        &json!({"id":"comment","type":"agentMessage","text":"Checking…","phase":"commentary"}),
        Some("turn".into()),
        true,
    );
    core.item(&json!({"id":"picture","type":"mcpToolCall","status":"completed","server":"cua-driver","tool":"screenshot","arguments":{},"result":{"content":[{"type":"image","mimeType":"image/png","data":"aGVsbG8="}]}}), Some("turn".into()), true);
    core.item(&json!({"id":"dynamic","type":"dynamicToolCall","tool":"picture","contentItems":[{"type":"inputImage","imageUrl":"data:image/png;base64,aGVsbG8="}]}), Some("turn".into()), true);
    let events: Vec<_> = receiver.try_iter().collect();
    let transcript = fold(&events);
    assert_eq!(
        transcript.items[0].presentation.phase,
        Some(crate::chat::model::MessagePhase::Commentary)
    );
    assert_eq!(transcript.items[1].presentation.images.len(), 1);
    assert_eq!(transcript.items[2].presentation.images.len(), 1);
    assert!(
        matches!(&transcript.items[1].body, ItemBody::ToolCall { output: Some(output), .. } if !output.contains("aGVsbG8="))
    );
}

fn completed_notices(events: &[ChatEvent]) -> Vec<Item> {
    events
        .iter()
        .filter_map(|event| match event {
            ChatEvent::ItemCompleted { item } if matches!(item.body, ItemBody::Notice { .. }) => {
                Some(item.clone())
            }
            _ => None,
        })
        .collect()
}

fn assert_notice(item: &Item, expected: &str, resolved: bool) {
    assert!(
        matches!(&item.body, ItemBody::Notice {kind: Some(kind), resolved: actual, ..}
        if kind == expected && *actual == resolved),
        "{item:?}"
    );
}

#[test]
fn reconnecting_updates_and_resolves_on_items_or_completion_for_its_turn() {
    let fake = Fake::new(&[]);
    let (sender, events) = mpsc::channel();
    let mut session = Session::new(&fake.config(Provider::Codex), sender);
    session.begin_turn("turn-1");
    for attempt in [1, 2] {
        session.notification(
            "error",
            &json!({"turnId":"turn-1","willRetry":true,
            "error":{"message":format!("Reconnecting {attempt}/5")}}),
        );
    }
    session.notification(
        "item/agentMessage/delta",
        &json!({"turnId":"other","itemId":"other","delta":"x"}),
    );
    session.notification(
        "item/agentMessage/delta",
        &json!({"turnId":"turn-1","itemId":"answer","delta":"ok"}),
    );
    let items = completed_notices(&events.try_iter().collect::<Vec<_>>());
    assert_eq!(items.len(), 3);
    assert!(items.iter().all(|item| item.id == "retry-turn-1"));
    assert_notice(&items[0], notice_kind::RECONNECTING, false);
    assert_notice(&items[2], notice_kind::RECONNECTING, true);
    session.notification(
        "error",
        &json!({"turnId":"turn-1","willRetry":true,"error":{"message":"retry"}}),
    );
    session.finish_turn("turn-1", TurnOutcome::Completed);
    let items = completed_notices(&events.try_iter().collect::<Vec<_>>());
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].id, items[1].id);
    assert_notice(&items[1], notice_kind::RECONNECTING, true);
    // A completion received before turn/start's receipt still resolves that turn's retry.
    session.notification(
        "error",
        &json!({"turnId":"unreceived","willRetry":true,"error":{"message":"retry"}}),
    );
    session.finish_turn("unreceived", TurnOutcome::Completed);
    let items = completed_notices(&events.try_iter().collect::<Vec<_>>());
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].id, items[1].id);
    assert_notice(&items[1], notice_kind::RECONNECTING, true);
    // Missing ids with no active turn are unique, rather than the shared "retry-".
    for _ in 0..2 {
        session.notification(
            "error",
            &json!({"turnId":"","willRetry":true,"error":{"message":"retry"}}),
        );
    }
    let items = completed_notices(&events.try_iter().collect::<Vec<_>>());
    assert_eq!(items.len(), 2);
    assert_ne!(items[0].id, items[1].id);
    assert!(items.iter().all(|item| item.id != "retry-"));
    session.begin_turn("turn-2");
    session.notification(
        "error",
        &json!({"turnId":"","willRetry":true,"error":{"message":"retry"}}),
    );
    session.finish_turn("turn-2", TurnOutcome::Completed);
    let items = completed_notices(&events.try_iter().collect::<Vec<_>>());
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].id, "retry-turn-2");
    assert_notice(&items[1], notice_kind::RECONNECTING, true);
}

#[test]
fn codex_authentication_and_usage_errors_accept_string_and_object_variants() {
    for (info, kind) in [
        (json!("unauthorized"), notice_kind::AUTH_REQUIRED),
        (json!({"unauthorized":{}}), notice_kind::AUTH_REQUIRED),
        (json!("usageLimitExceeded"), "rate_limit:codex"),
        (json!({"usageLimitExceeded":{}}), "rate_limit:codex"),
    ] {
        let fake = Fake::new(&[]);
        let (sender, events) = mpsc::channel();
        let mut session = Session::new(&fake.config(Provider::Codex), sender);
        session.begin_turn("failed");
        session.notification(
            "error",
            &json!({"turnId":"failed","willRetry":false,
            "error":{"message":"sign in or wait","codexErrorInfo":info}}),
        );
        session.finish_turn(
            "failed",
            TurnOutcome::Failed {
                message: "sign in or wait".into(),
            },
        );
        let items = completed_notices(&events.try_iter().collect::<Vec<_>>());
        assert_notice(&items[0], kind, false);
        assert_eq!(items.len(), 1);
        session.begin_turn("success");
        session.finish_turn("success", TurnOutcome::Completed);
        let resolved = completed_notices(&events.try_iter().collect::<Vec<_>>());
        assert_eq!(resolved.len(), 1);
        assert_eq!(items[0].id, resolved[0].id);
        assert_notice(&resolved[0], kind, true);
    }
}

#[test]
fn account_notifications_require_auth_and_success_resolves_the_latest_notice() {
    for (method, params, expected) in [
        (
            "account/updated",
            json!({"authMode":null}),
            "Codex is signed out. Run `codex login` to sign in again.",
        ),
        (
            "account/updated",
            json!({}),
            "Codex is signed out. Run `codex login` to sign in again.",
        ),
        (
            "account/login/completed",
            json!({"success":false,"error":"login failed"}),
            "login failed",
        ),
    ] {
        let fake = Fake::new(&[]);
        let (sender, events) = mpsc::channel();
        let mut session = Session::new(&fake.config(Provider::Codex), sender);
        session.notification(method, &params);
        session.notification("account/updated", &json!({"authMode":"chatgpt"}));
        session.notification("account/login/completed", &json!({"success":true}));
        session.begin_turn("success");
        session.finish_turn("success", TurnOutcome::Completed);
        let items = completed_notices(&events.try_iter().collect::<Vec<_>>());
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, items[1].id);
        assert_notice(&items[0], notice_kind::AUTH_REQUIRED, false);
        assert_notice(&items[1], notice_kind::AUTH_REQUIRED, true);
        assert!(matches!(&items[0].body, ItemBody::Notice {text, ..} if text == expected));
    }
}

#[test]
fn model_fallback_deprecation_and_provider_warnings_have_their_contract_kinds() {
    let fake = Fake::new(&[]);
    let (sender, events) = mpsc::channel();
    let mut session = Session::new(&fake.config(Provider::Codex), sender);
    for (method, params, kind, level, expected) in [
        (
            "model/rerouted",
            json!({"fromModel":"large","toModel":"small","reason":"capacity"}),
            notice_kind::MODEL_FALLBACK,
            NoticeLevel::Warning,
            "Codex answered with small instead of large (capacity).",
        ),
        (
            "deprecationNotice",
            json!({"summary":"old setting","details":"use the new one"}),
            notice_kind::DEPRECATION,
            NoticeLevel::Info,
            "old setting\nuse the new one",
        ),
        (
            "warning",
            json!({"message":"heads up"}),
            notice_kind::PROVIDER_WARNING,
            NoticeLevel::Warning,
            "heads up",
        ),
        (
            "configWarning",
            json!({"summary":"config","details":"details"}),
            notice_kind::CONFIG_WARNING,
            NoticeLevel::Warning,
            "config\ndetails",
        ),
    ] {
        session.notification(method, &params);
        let items = completed_notices(&events.try_iter().collect::<Vec<_>>());
        assert_eq!(items.len(), 1);
        assert_notice(&items[0], kind, false);
        assert!(
            matches!(&items[0].body, ItemBody::Notice {level: actual, text, ..} if *actual == level && text == expected)
        );
    }
}

#[test]
fn token_refresh_requests_emit_auth_required_once_and_are_answered_with_an_error() {
    let mut text = fixture("codex/idle.ndjson");
    text.push_str("\n");
    text.push_str(
        &json!({"type":"expect","frame":{"method":"turn/start"},"reply":[
            {"id":801,"method":"account/chatgptAuthTokens/refresh","params":{}},
            {"id":802,"method":"account/chatgptAuthTokens/refresh","params":{}}
        ]})
        .to_string(),
    );
    for id in [801, 802] {
        text.push_str("\n");
        text.push_str(
            &json!({"type":"expect","frame":{"id":id,"error":{"code":-32601,
            "message":"riwork does not handle account/chatgptAuthTokens/refresh"}}})
            .to_string(),
        );
    }
    let fake = Fake::new(&[&text]);
    let (sender, events) = mpsc::channel();
    let mut driver = start(fake.config(Provider::Codex), sender).unwrap();
    until(&events, is_state(ChatState::Idle));
    driver
        .command(ChatCommand::Send { text: "go".into() })
        .unwrap();
    let seen = until(
        &events,
        |event| matches!(event, ChatEvent::ItemCompleted {item} if matches!(item.body, ItemBody::Notice {..})),
    );
    for _ in 0..100 {
        if fake
            .received()
            .iter()
            .filter(|frame| frame["id"] == 801 || frame["id"] == 802)
            .count()
            == 2
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        fake.received()
            .iter()
            .filter(|frame| frame["id"] == 801 || frame["id"] == 802)
            .count(),
        2
    );
    let mut seen = seen;
    seen.extend(events.try_iter());
    let notices = completed_notices(&seen);
    assert_eq!(notices.len(), 1);
    assert_notice(&notices[0], notice_kind::AUTH_REQUIRED, false);
    assert!(!fake.saw("mismatch"));
}

#[test]
fn refused_start_compact_and_mcp_elicitations_use_the_contract_kinds() {
    for (method, command) in [
        ("turn/start", ChatCommand::Send { text: "go".into() }),
        ("thread/compact/start", ChatCommand::Compact),
    ] {
        let text = format!(
            "{}\n{}",
            fixture("codex/idle.ndjson"),
            json!({
                "type":"expect","frame":{"method":method},
                "reply":[{"id":"$id","error":{"code":-32602,"message":"refused"}}]
            })
        );
        let fake = Fake::new(&[&text]);
        let (sender, events) = mpsc::channel();
        let mut driver = start(fake.config(Provider::Codex), sender).unwrap();
        until(&events, is_state(ChatState::Idle));
        driver.command(command).unwrap();
        let seen = until(
            &events,
            |event| matches!(event, ChatEvent::ItemCompleted {item} if matches!(item.body, ItemBody::Notice {..})),
        );
        let notices = completed_notices(&seen);
        assert_eq!(notices.len(), 1);
        assert_notice(&notices[0], notice_kind::SETTING_REFUSED, false);
        assert!(!fake.saw("mismatch"));
    }
    let text = format!(
        "{}\n{}\n{}",
        fixture("codex/idle.ndjson"),
        json!({"type":"expect","frame":{"method":"turn/start"},"reply":[{
            "id":803,"method":"mcpServer/elicitation/request","params":{}
        }]}),
        json!({"type":"expect","frame":{"id":803,"result":{"action":"decline"}}})
    );
    let fake = Fake::new(&[&text]);
    let (sender, events) = mpsc::channel();
    let mut driver = start(fake.config(Provider::Codex), sender).unwrap();
    until(&events, is_state(ChatState::Idle));
    driver
        .command(ChatCommand::Send { text: "go".into() })
        .unwrap();
    let seen = until(
        &events,
        |event| matches!(event, ChatEvent::ItemCompleted {item} if matches!(item.body, ItemBody::Notice {..})),
    );
    assert_notice(
        &completed_notices(&seen)[0],
        notice_kind::MCP_ELICITATION,
        false,
    );
}

#[test]
fn a_success_before_the_start_receipt_resolves_auth_and_usage_notices() {
    let fake = Fake::new(&[]);
    let (sender, events) = mpsc::channel();
    let mut session = Session::new(&fake.config(Provider::Codex), sender);
    session.notification("account/updated", &json!({"authMode":null}));
    session.notification(
        "error",
        &json!({"error":{"message":"usage exhausted","codexErrorInfo":"usageLimitExceeded"}}),
    );
    session.notification(
        "turn/completed",
        &json!({"turn":{"id":"completed-before-receipt","status":"completed"}}),
    );
    let items = completed_notices(&events.try_iter().collect::<Vec<_>>());
    assert_eq!(items.len(), 4);
    assert_eq!(items[1].id, items[2].id);
    assert_eq!(items[0].id, items[3].id);
    assert_notice(&items[2], "rate_limit:codex", true);
    assert_notice(&items[3], notice_kind::AUTH_REQUIRED, true);
}

#[test]
fn usage_exhaustion_stays_open_after_failed_completion_until_a_successful_turn() {
    let fake = Fake::new(&[]);
    let (sender, events) = mpsc::channel();
    let mut session = Session::new(&fake.config(Provider::Codex), sender);
    session.begin_turn("exhausted");
    session.notification(
        "error",
        &json!({"turnId":"exhausted","willRetry":false,
            "error":{"message":"usage exhausted","codexErrorInfo":"usageLimitExceeded"}}),
    );
    session.notification(
        "turn/completed",
        &json!({"turn":{"id":"exhausted","status":"failed",
            "error":{"message":"usage exhausted"}}}),
    );
    let items = completed_notices(&events.try_iter().collect::<Vec<_>>());
    assert_eq!(items.len(), 1);
    assert_notice(&items[0], "rate_limit:codex", false);
    assert_eq!(session.open_notices["rate_limit:codex"], items[0]);
    session.begin_turn("success");
    session.notification(
        "turn/completed",
        &json!({"turn":{"id":"success","status":"completed"}}),
    );
    let resolved = completed_notices(&events.try_iter().collect::<Vec<_>>());
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].id, items[0].id);
    assert_notice(&resolved[0], "rate_limit:codex", true);
}

#[test]
fn restored_auth_and_usage_notices_resolve_on_the_first_successful_turn() {
    let fake = Fake::new(&[]);
    let (sender, events) = mpsc::channel();
    let mut config = fake.config(Provider::Codex);
    let mut previous = Session::new(&config, sender.clone());
    previous.notification("account/updated", &json!({"authMode":null}));
    previous.notification(
        "error",
        &json!({"error":{"message":"usage exhausted","codexErrorInfo":"usageLimitExceeded"}}),
    );
    config.outstanding_notices = previous.open_notices.clone();
    let original = completed_notices(&events.try_iter().collect::<Vec<_>>());
    let mut session = Session::new(&config, sender);
    session.begin_turn("success");
    session.finish_turn("success", TurnOutcome::Completed);
    let resolved = completed_notices(&events.try_iter().collect::<Vec<_>>());
    assert_eq!(resolved.len(), 2);
    for item in &original {
        let ItemBody::Notice {
            kind: Some(kind), ..
        } = &item.body
        else {
            panic!("{item:?}")
        };
        let update = resolved.iter().find(|update| update.id == item.id).unwrap();
        assert_notice(update, kind, true);
    }
    assert!(session.open_notices.is_empty());
}

#[test]
fn the_threads_own_name_reaches_the_chat_on_resume_and_on_a_rename() {
    let mut run = Run::start_with("resume_named", |config| {
        config.resume = Some("thread-9".into())
    });
    run.send("again");
    run.finish_turn();
    let titles = run
        .seen
        .iter()
        .filter_map(|event| match event {
            ChatEvent::ProviderTitle { title } => Some(title.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    // A sub-agent's thread renamed on the same connection is not this chat's name.
    assert_eq!(titles, ["Release checklist", "Release notes"]);
    run.end();
}
