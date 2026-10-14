use super::*;
use crate::chat::model::{ContributorMode, Provider, Transcript};
use crate::chat::testkit::{Fake, fixture, fold, until};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};

#[test]
fn approval_modes_map_to_muse_words() {
    assert_eq!(policy(ApprovalMode::Supervised), "promptUnmatched");
    assert_eq!(policy(ApprovalMode::AutoEdit), "onRequest");
    assert_eq!(policy(ApprovalMode::Full), "allowAll");
    assert_eq!(policy(ApprovalMode::Plan), "denyUnmatched");
}

fn choice(decision: &str, scope: &str, choice_id: &str) -> Choice {
    Choice {
        decision: decision.into(),
        scope: scope.into(),
        choice_id: choice_id.into(),
    }
}

#[test]
fn a_riwork_decision_never_sends_a_choice_that_saves_a_rule() {
    // A saved rule first on the wire must not be taken for "for this session".
    let choices = [
        choice("approvedPolicyAmendment", "localPersistent", "always"),
        choice("approved", "localPersistent", "always-too"),
        choice("approvedForSession", "session", "session"),
        choice("approved", "once", "once"),
        choice("deniedPolicyAmendment", "localPersistent", "never"),
        choice("denied", "once", "no"),
        choice("abort", "once", "stop"),
    ];
    let picked = |decision| pick(&choices, decision).map(|c| c.choice_id.as_str());
    assert_eq!(picked(Decision::Accept), Some("once"));
    assert_eq!(picked(Decision::AcceptForSession), Some("session"));
    assert_eq!(picked(Decision::Decline), Some("no"));
    assert_eq!(picked(Decision::Cancel), Some("stop"));
    assert_eq!(
        offered_of(&choices),
        [
            Decision::Accept,
            Decision::AcceptForSession,
            Decision::Decline,
            Decision::Cancel
        ]
    );
    // Only lasting grants and a refusal that saves a rule: nothing RiWork can say yes with.
    let lasting = [
        choice("approved", "localPersistent", "always"),
        choice("approvedPolicyAmendment", "localPersistent", "rule"),
        choice("deniedPolicyAmendment", "localPersistent", "never"),
        choice("abort", "once", "stop"),
    ];
    assert_eq!(offered_of(&lasting), [Decision::Cancel]);
    assert!(pick(&lasting, Decision::AcceptForSession).is_none());
    assert!(pick(&lasting, Decision::Decline).is_none());
    // A session-scoped rule is a grant for this session; unknown words are never guessed.
    assert_eq!(
        offered_of(&[choice("approvedPolicyAmendment", "session", "s")]),
        [Decision::AcceptForSession]
    );
    assert!(
        offered_of(&[
            choice("approved", "mystery", "m"),
            choice("timedOut", "once", "t")
        ])
        .is_empty()
    );
    // A fold someone else decided reads as wide as it was.
    assert_eq!(
        wire_decision(Some("approvedPolicyAmendment")),
        Decision::AcceptForSession
    );
    assert_eq!(wire_decision(Some("approved")), Decision::Accept);
    assert_eq!(wire_decision(Some("abort")), Decision::Cancel);
    assert_eq!(wire_decision(Some("timedOut")), Decision::Decline);
}

#[test]
fn answers_take_exactly_one_form_and_refuse_what_muse_would() {
    let single = WireQuestion {
        name: "Pick".into(),
        multi: false,
        labels: vec!["A".into(), "B".into()],
        min: None,
        max: None,
    };
    let several = WireQuestion {
        name: "Several".into(),
        multi: true,
        labels: vec!["x".into(), "y".into(), "z".into()],
        min: Some(1),
        max: Some(2),
    };
    let answer = |q: &WireQuestion, given: &[&str]| {
        let given: Vec<String> = given.iter().map(|s| s.to_string()).collect();
        wire_answer("q", q, &given)
    };
    assert_eq!(
        answer(&single, &["B"]).unwrap(),
        json!({"questionId": "q", "selectedLabel": "B"})
    );
    assert_eq!(
        answer(&single, &["my own words"]).unwrap(),
        json!({"questionId": "q", "freeText": "my own words"})
    );
    assert_eq!(
        answer(&several, &["x", "z"]).unwrap(),
        json!({"questionId": "q", "selectedLabels": ["x", "z"]})
    );
    // Typed text among the choices goes as free text, choices included.
    assert_eq!(
        answer(&several, &["x", "other"]).unwrap(),
        json!({"questionId": "q", "freeText": "x, other"})
    );
    assert!(
        answer(&several, &["x", "y", "z"])
            .unwrap_err()
            .contains("between 1 and 2")
    );
    assert!(
        answer(&several, &[])
            .unwrap_err()
            .contains("between 1 and 2")
    );
    assert!(answer(&single, &[]).unwrap_err().contains("Answer “Pick”"));
    // Too long is refused whole, never cut.
    let long = "w".repeat(FREE_TEXT_CHARS + 1);
    assert!(
        answer(&single, &[long.as_str()])
            .unwrap_err()
            .contains("at most 500")
    );
    assert!(answer(&single, &[&long[..FREE_TEXT_CHARS]]).is_ok());
}

#[test]
fn the_cua_server_is_riwork_cua_mcp_for_this_session_with_literal_values() {
    let fake = Fake::new(&[""]);
    let config = fake.config(Provider::Muse);
    let home = config
        .env
        .iter()
        .find(|(name, _)| name == "RIWORK_HOME")
        .map(|(_, value)| value.to_string_lossy().into_owned())
        .unwrap();
    let servers = cua_servers(
        &config,
        Path::new("/Applications/RiWork.app/Contents/MacOS/riwork"),
        Some(Path::new("/opt/cua/driver")),
    )
    .unwrap();
    assert_eq!(
        servers,
        json!({"cua-driver": {
            "transport": "stdio",
            "command": "/Applications/RiWork.app/Contents/MacOS/riwork",
            "args": ["cua", "mcp"],
            "env": {"RIWORK_HOME": home, "RIWORK_CUA_DRIVER": "/opt/cua/driver"},
            "mode": "required",
        }})
    );
    let mut bare = config.clone();
    bare.env.retain(|(name, _)| name != "RIWORK_HOME");
    assert!(cua_servers(&bare, Path::new("/x"), None).is_none());
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
    // An item status the driver does not know is over, not running.
    assert_eq!(
        item_status(&json!({"status": "somethingNew"}), false),
        ItemStatus::Completed
    );
    assert_eq!(item_status(&json!({}), false), ItemStatus::InProgress);
}

#[test]
fn usage_is_what_muse_counts_and_prices_and_the_session_model_is_the_default() {
    let fake = Fake::new(&[""]);
    let (sender, events) = mpsc::channel();
    let mut session = Session::new(&fake.config(Provider::Muse), sender);
    session.session_id = Some("sess-1".into());
    let usage = |session: &mut Session, params: Value| {
        session.notification("session/tokenUsage", &params);
        session.usage.clone()
    };
    let priced = usage(
        &mut session,
        json!({"sessionId": "sess-1", "cumulative": {"promptTokens": 9, "outputTokens": 2, "cacheReadTokens": 1, "cost": {"usd": 0.5, "partial": false}}}),
    );
    assert_eq!(priced.cost_usd, Some(0.5));
    // Absent means unpriced, not the last price.
    let unpriced = usage(
        &mut session,
        json!({"sessionId": "sess-1", "cumulative": {"promptTokens": 10, "outputTokens": 2}}),
    );
    assert_eq!((unpriced.input_tokens, unpriced.cost_usd), (10, None));
    session.notification(
        "session/contextUsage",
        &json!({"sessionId": "sess-1", "usedTokens": 5, "windowTokens": 100, "pressure": "normal"}),
    );
    assert_eq!(session.usage.context_window, Some(100));
    session.notification(
        "session/contextUsage",
        &json!({"sessionId": "sess-1", "usedTokens": 6, "pressure": "normal"}),
    );
    assert_eq!(session.usage.context_window, None, "no limit, no window");
    // Another session's frames are not this chat's.
    session.notification(
        "session/tokenUsage",
        &json!({"sessionId": "other", "cumulative": {"promptTokens": 999}}),
    );
    assert_eq!(session.usage.input_tokens, 10);
    drop(events);
    let (models, _) = model_options(&json!({"models": [
        {"modelId": "a", "isDefault": true, "isActive": false},
        {"modelId": "b", "isDefault": false, "isActive": true},
    ]}));
    assert_eq!(
        models
            .iter()
            .map(|m| (m.id.as_str(), m.is_default))
            .collect::<Vec<_>>(),
        [("a", false), ("b", true)]
    );
    assert!(
        models.iter().all(|m| !m.supports_fast),
        "Muse has no fast tier"
    );
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

    /// Whatever has arrived by now, without waiting.
    fn drain(&mut self) {
        self.seen.extend(self.events.try_iter());
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

    /// Every user-message item event, in order: what any client ever saw.
    fn user_events(&self) -> Vec<Item> {
        user_events(&self.seen)
    }

    fn end(mut self) {
        self.driver.shutdown();
        assert!(!self.fake.saw("mismatch"), "{:#?}", self.fake.entries());
    }
}

fn user_events(events: &[ChatEvent]) -> Vec<Item> {
    events
        .iter()
        .filter_map(|event| match event {
            ChatEvent::ItemStarted { item } | ChatEvent::ItemCompleted { item }
                if matches!(item.body, ItemBody::UserMessage { .. }) =>
            {
                Some(item.clone())
            }
            _ => None,
        })
        .collect()
}

fn user_text(item: &Item) -> &str {
    match &item.body {
        ItemBody::UserMessage { text } => text,
        _ => "",
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

fn is_notice(event: &ChatEvent) -> bool {
    matches!(event, ChatEvent::ItemCompleted { item } if matches!(item.body, ItemBody::Notice { .. }))
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

/// A text file and a PNG staged as the chat's own snapshots.
fn snapshots(dir: &Path) -> Vec<Attachment> {
    let file = dir.join("notes.txt");
    std::fs::write(&file, "secret file body").unwrap();
    let text = attachments::stage(dir, &file).unwrap();
    let mut png = Vec::new();
    image::DynamicImage::new_rgb8(2, 1)
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    let file = dir.join("photo.png");
    std::fs::write(&file, png).unwrap();
    vec![text, attachments::stage(dir, &file).unwrap()]
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
    // `serve` and the contributor mode, off by default: Cua.ai Driver rides the session, not
    // Muse's command line.
    let argv = run.fake.starts()[0]["argv"].clone();
    assert_eq!(argv, json!(["serve", "--contributor-mode", "off"]));
    let start = &run.fake.received_method("session/start")[0]["params"];
    let server = &start["config"]["mcpServers"]["cua-driver"];
    assert_eq!(
        server["command"],
        json!(std::env::current_exe().unwrap().to_string_lossy())
    );
    assert_eq!(
        server["env"]["RIWORK_HOME"],
        json!(run.fake.dir.join("riwork_home").to_string_lossy())
    );
    assert!(
        start.get("permissionProfile").is_none(),
        "only Plan picks a profile"
    );
    assert!(
        run.fake.received_method("initialized")[0]
            .get("params")
            .is_none(),
        "empty params are left out"
    );
    run.end();
}

#[test]
fn a_trusted_chat_folder_is_trusted_by_its_serve_for_that_process_only() {
    let mut run = Run::start_with("turn", |config| config.trust_workspace = true);
    run.send("hello");
    run.finish_turn();
    // After the subcommand, as Muse 1.4.7 reads it; nothing on the wire carries it.
    assert_eq!(
        run.fake.starts()[0]["argv"],
        json!(["serve", "--contributor-mode", "off", "--trust-workspace"])
    );
    run.end();
}

#[test]
fn every_serve_says_the_chats_contributor_mode_once_off_unless_turned_on() {
    for (mode, flag) in [(ContributorMode::Off, "off"), (ContributorMode::On, "on")] {
        let argv = json!(["serve", "--contributor-mode", flag]);
        // Muse's per-process override, after the subcommand, at the first start…
        let mut run = Run::start_with("turn", |config| config.contributor_mode = mode);
        run.send("hello");
        run.finish_turn();
        assert_eq!(run.fake.starts()[0]["argv"], argv);
        run.end();
        // …and at a resume, since nothing on the wire carries it.
        let run = Run::start_with("resume_active", |config| {
            config.resume = Some("sess-old".into());
            config.contributor_mode = mode;
        });
        assert_eq!(run.fake.received_method("session/resume").len(), 1);
        assert_eq!(run.fake.starts()[0]["argv"], argv);
        run.end();
    }
}

#[test]
fn an_approval_is_receipted_decided_and_closed_by_the_fold() {
    let mut run = Run::start("approval_accept");
    run.send("clean up");
    let approval = last_approval(&run.until(is_approval));
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
fn approvals_offer_and_send_only_what_lasts_no_longer_than_asked() {
    let mut run = Run::start("approval_scopes");
    run.send("go");
    let first = last_approval(&run.until(is_approval));
    assert_eq!(
        first.choices,
        [
            Decision::Accept,
            Decision::AcceptForSession,
            Decision::Decline
        ]
    );
    assert!(
        first.detail.contains("allow rm this session"),
        "{}",
        first.detail
    );
    assert!(
        !first.detail.contains("allow rm forever"),
        "a rule RiWork never sends is not shown as one of its choices"
    );
    // The fixture accepts only the session choice, though a saved rule comes first.
    run.driver
        .command(ChatCommand::Approve {
            request_id: "ap-1".into(),
            decision: Decision::AcceptForSession,
        })
        .unwrap();
    let second = last_approval(&run.until(|event| {
        matches!(event, ChatEvent::ApprovalRequested { approval } if approval.request_id == "ap-2")
    }));
    assert_eq!(second.choices, [Decision::Decline, Decision::Cancel]);
    // A one-time yes Muse offers only as a saved rule is refused here, with a word why.
    run.driver
        .command(ChatCommand::Approve {
            request_id: "ap-2".into(),
            decision: Decision::Accept,
        })
        .unwrap();
    run.until(is_notice);
    assert!(
        run.notices()
            .iter()
            .any(|(_, text)| text.contains("saves a lasting rule")),
        "{:?}",
        run.notices()
    );
    run.driver
        .command(ChatCommand::Approve {
            request_id: "ap-2".into(),
            decision: Decision::Decline,
        })
        .unwrap();
    run.finish_turn();
    let decided: Vec<Value> = run
        .fake
        .received_method("approval/decide")
        .iter()
        .map(|frame| frame["params"]["choiceId"].clone())
        .collect();
    assert_eq!(decided, [json!("session"), json!("no")]);
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
fn questions_take_choices_several_choices_and_free_text_and_refusals_send_nothing() {
    let mut run = Run::start("questions");
    run.send("ask me");
    let events = run.until(is_question);
    let question = events
        .iter()
        .find_map(|event| match event {
            ChatEvent::QuestionRequested { question } => Some(question.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        question
            .questions
            .iter()
            .map(|q| (q.header.as_deref(), q.multi_select, q.options.len()))
            .collect::<Vec<_>>(),
        [
            (Some("Pick"), false, 2),
            (Some("Several"), true, 3),
            (Some("Why"), false, 0)
        ]
    );
    run.until(is_notice);
    assert!(
        run.notices()
            .iter()
            .any(|(_, text)| text.contains("by itself in 60 seconds")),
        "{:?}",
        run.notices()
    );
    // Over the bounds, or too long: refused here, nothing sent, the prompt stays.
    for answers in [
        vec![
            vec!["B".into()],
            vec!["x".into(), "y".into(), "z".into()],
            vec!["ok".into()],
        ],
        vec![vec!["B".into()], vec!["x".into()], vec!["w".repeat(501)]],
    ] {
        assert!(
            run.driver
                .command(ChatCommand::Answer {
                    request_id: "ui-1".into(),
                    answers,
                })
                .is_err()
        );
    }
    assert!(run.fake.received_method("userInput/answer").is_empty());
    run.driver
        .command(ChatCommand::Answer {
            request_id: "ui-1".into(),
            answers: vec![
                vec!["B".into()],
                vec!["x".into(), "y".into()],
                vec!["because, really".into()],
            ],
        })
        .unwrap();
    run.finish_turn();
    assert_eq!(
        run.fake.received_method("userInput/answer")[0]["params"]["answers"],
        json!([
            {"questionId": "q1", "selectedLabel": "B"},
            {"questionId": "q2", "selectedLabels": ["x", "y"]},
            {"questionId": "q3", "freeText": "because, really"},
        ])
    );
    // Showing the timed prompt did not engage it.
    assert!(run.fake.received_method("userInput/engaged").is_empty());
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
fn a_queued_turn_withdrawn_elsewhere_leaves_the_chat_idle_for_the_next() {
    let mut run = Run::start("unqueued");
    run.send("later");
    run.until(is_notice);
    run.until(is_state(ChatState::Idle));
    assert!(run.notices()[0].1.contains("will not run"));
    run.send("next");
    run.finish_turn();
    assert_eq!(run.fake.received_method("turn/start").len(), 2);
    run.end();
}

#[test]
fn an_approval_update_redescribes_the_prompt_and_keeps_its_item() {
    let mut run = Run::start("approval_updated");
    run.send("clean up");
    let first = last_approval(&run.until(is_approval));
    assert_eq!(first.title, "rm -rf build");
    assert!(first.detail.contains("rm -rf build"), "{:?}", first.detail);
    let second = last_approval(&run.until(is_approval));
    assert_eq!(second.title, first.title);
    assert_eq!(second.item_id, first.item_id);
    assert!(
        second.detail.starts_with(&first.detail),
        "the raw arguments stay: {:?}",
        second.detail
    );
    assert!(
        second.detail.ends_with("\nrm -rf build"),
        "{:?}",
        second.detail
    );
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
        config.instructions = Some("EARLIER TALK".into());
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
    assert!(
        run.fake.received_method("session/resume")[0]["params"]["config"]["mcpServers"]
            ["cua-driver"]
            .is_object(),
        "the session's MCP servers are named again at resume"
    );
    run.end();
}

#[test]
fn the_carried_over_conversation_goes_once_and_the_user_sees_only_their_words() {
    let mut run = Run::start_with("carry_over", |config| {
        config.instructions = Some("EARLIER TALK".into());
    });
    assert!(
        run.notices().is_empty(),
        "nothing says the conversation was lost: {:?}",
        run.notices()
    );
    // A refused start keeps it for the next, and gives the typed message back.
    run.send("first try");
    run.until(is_notice);
    run.until(is_state(ChatState::Idle));
    assert!(
        run.notices()[0].1.ends_with("“first try”"),
        "{:?}",
        run.notices()
    );
    run.send("hello");
    run.finish_turn();
    run.send("again");
    run.finish_turn();
    let starts = run.fake.received_method("turn/start");
    assert_eq!(starts.len(), 3);
    assert_eq!(
        starts[2]["params"]["input"],
        json!([{"type": "text", "text": "again"}])
    );
    assert!(starts[2]["params"].get("displayText").is_none());
    let users = run.user_events();
    assert!(
        users
            .iter()
            .all(|item| !user_text(item).contains("EARLIER")),
        "no client ever saw the carried-over text as the user's: {users:?}"
    );
    let shown: Vec<String> = run
        .transcript()
        .items
        .iter()
        .filter(|item| matches!(item.body, ItemBody::UserMessage { .. }))
        .map(|item| user_text(item).to_owned())
        .collect();
    assert_eq!(shown, ["hello", "again"]);
    run.end();
}

#[test]
fn a_resumed_session_that_never_ran_a_turn_still_gets_the_carried_over_conversation() {
    let mut run = Run::start_with("resume_unsent", |config| {
        config.resume = Some("sess-1".into());
        config.instructions = Some("EARLIER TALK".into());
    });
    run.send("hello");
    run.finish_turn();
    run.end();
}

#[test]
fn attachments_start_and_steer_exactly_and_show_the_users_message_with_its_snapshots() {
    let fake = Fake::new(&[&fixture("muse/attachments.ndjson")]);
    let attachments = snapshots(&fake.dir);
    let (sender, events) = mpsc::channel();
    let mut driver = start(fake.config(Provider::Muse), sender).unwrap();
    let mut seen = until(&events, is_state(ChatState::Idle));
    driver
        .command(ChatCommand::SendAttachments {
            text: "look".into(),
            attachments: attachments.clone(),
        })
        .unwrap();
    // Built here independently of the encoder: Muse joins text parts as they come.
    let png = std::fs::read(&attachments[1].path).unwrap();
    let start = &fake.received_method("turn/start")[0]["params"];
    assert_eq!(
        start["input"],
        json!([
            {"type": "text", "text": "look"},
            {"type": "text", "text": "\n\nAttached file: notes.txt\nsecret file body"},
            {"type": "text", "text": "\n\nAttached image: photo.png\n"},
            {"type": "image", "mediaType": "image/png", "base64Data": STANDARD.encode(&png)},
        ])
    );
    let summary = attachments::summary("look", &attachments);
    assert_eq!(start["displayText"], json!(summary));
    seen.extend(until(
        &events,
        |event| matches!(event, ChatEvent::ItemCompleted { item } if item.id == "um-1"),
    ));
    let echo = user_events(&seen).pop().unwrap();
    assert_eq!(user_text(&echo), summary);
    assert_eq!(echo.presentation.attachments, attachments);
    assert_eq!(
        echo.presentation.images,
        [ChatImage {
            label: "photo.png".into(),
            source: ImageSource::Local {
                path: attachments[1].path.display().to_string()
            },
        }]
    );
    // A refused steer is the sender's to hear, and is never retried as a start.
    let refused = driver
        .command(ChatCommand::SendAttachments {
            text: "again".into(),
            attachments: attachments[..1].to_vec(),
        })
        .unwrap_err();
    assert!(refused.contains("already_terminal"), "{refused}");
    assert!(!refused.starts_with(UNKNOWN_SUBMISSION));
    driver
        .command(ChatCommand::SendAttachments {
            text: "third".into(),
            attachments: attachments[1..].to_vec(),
        })
        .unwrap();
    seen.extend(until(&events, is_turn_completed));
    seen.extend(until(&events, is_state(ChatState::Idle)));
    let steers = fake.received_method("turn/steer");
    assert_eq!(steers.len(), 2);
    assert!(
        steers
            .iter()
            .all(|frame| frame["params"]["expectedTurnId"] == "turn-1")
    );
    let transcript = fold(&seen);
    let third = transcript
        .items
        .iter()
        .find(|item| item.id == "um-2")
        .unwrap();
    assert_eq!(
        user_text(third),
        attachments::summary("third", &attachments[1..])
    );
    assert_eq!(third.presentation.attachments, attachments[1..].to_vec());
    assert!(
        user_events(&seen)
            .iter()
            .all(|item| !user_text(item).contains("secret file body")),
        "file contents are model input, not what the user typed"
    );
    driver
        .command(ChatCommand::Send {
            text: "plain".into(),
        })
        .unwrap();
    until(&events, is_turn_completed);
    assert_eq!(fake.received_method("turn/start").len(), 2);
    driver.shutdown();
    assert!(!fake.saw("mismatch"), "{:#?}", fake.entries());
}

#[test]
fn a_refused_attachment_start_is_the_senders_and_the_chat_goes_on() {
    let fake = Fake::new(&[&fixture("muse/attachments_refused.ndjson")]);
    let attachments = snapshots(&fake.dir);
    let (sender, events) = mpsc::channel();
    let mut driver = start(fake.config(Provider::Muse), sender).unwrap();
    until(&events, is_state(ChatState::Idle));
    let error = driver
        .command(ChatCommand::SendAttachments {
            text: "keep me".into(),
            attachments,
        })
        .unwrap_err();
    assert_eq!(error, "Invalid params: turn/start image payload is invalid");
    let after: Vec<ChatEvent> = events.try_iter().collect();
    assert!(!after.iter().any(is_turn_started), "{after:?}");
    assert!(
        !after.iter().any(is_notice),
        "the refusal is the sender's, not a notice"
    );
    assert!(after.iter().any(is_state(ChatState::Idle)));
    driver
        .command(ChatCommand::Send {
            text: "plain".into(),
        })
        .unwrap();
    until(&events, is_turn_completed);
    driver.shutdown();
    assert!(!fake.saw("mismatch"), "{:#?}", fake.entries());
}

#[test]
fn an_attachment_start_muse_dies_on_is_unknown_not_refused() {
    let fake = Fake::new(&[&fixture("muse/attachments_lost.ndjson")]);
    let attachments = snapshots(&fake.dir);
    let (sender, events) = mpsc::channel();
    let mut driver = start(fake.config(Provider::Muse), sender).unwrap();
    until(&events, is_state(ChatState::Idle));
    let error = driver
        .command(ChatCommand::SendAttachments {
            text: "maybe".into(),
            attachments,
        })
        .unwrap_err();
    assert!(error.starts_with(UNKNOWN_SUBMISSION), "{error}");
    assert_eq!(
        fake.received_method("turn/start").len(),
        1,
        "never written twice"
    );
    driver.shutdown();
}

#[test]
fn attachments_wait_for_a_start_in_flight_instead_of_guessing_its_turn() {
    let fake = Fake::new(&[&fixture("muse/attachments_starting.ndjson")]);
    let attachments = snapshots(&fake.dir);
    let (sender, events) = mpsc::channel();
    let mut driver = start(fake.config(Provider::Muse), sender).unwrap();
    until(&events, is_state(ChatState::Idle));
    driver
        .command(ChatCommand::Send {
            text: "slow".into(),
        })
        .unwrap();
    let error = driver
        .command(ChatCommand::SendAttachments {
            text: "keep me".into(),
            attachments,
        })
        .unwrap_err();
    assert!(error.contains("starting"), "{error}");
    until(&events, is_turn_completed);
    assert_eq!(fake.received_method("turn/start").len(), 1);
    assert!(fake.received_method("turn/steer").is_empty());
    driver.shutdown();
    assert!(!fake.saw("mismatch"), "{:#?}", fake.entries());
}

#[test]
fn a_steer_refused_as_its_turn_ends_starts_the_next_turn_once() {
    let mut run = Run::start("steer_races");
    run.send("one");
    run.until(is_turn_started);
    run.send("more");
    run.until(|event| matches!(event, ChatEvent::TurnStarted { turn_id } if turn_id == "turn-2"));
    run.finish_turn();
    let starts = run.fake.received_method("turn/start");
    assert_eq!(starts.len(), 2);
    assert_eq!(
        starts[1]["params"]["input"],
        json!([{"type": "text", "text": "more"}])
    );
    assert_eq!(run.fake.received_method("turn/steer").len(), 1);
    run.end();
}

#[test]
fn a_steer_muse_acknowledges_for_another_turn_comes_back_and_is_not_sent_again() {
    let mut run = Run::start("steer_not_absorbed");
    run.send("one");
    run.until(is_turn_started);
    run.send("more");
    run.until(is_notice);
    run.finish_turn();
    let notices = run.notices();
    assert!(
        notices
            .iter()
            .any(|(_, text)| text.contains("not sent again") && text.ends_with("“more”")),
        "{notices:?}"
    );
    assert_eq!(
        run.fake.received_method("turn/start").len(),
        1,
        "never resent"
    );
    assert_eq!(run.fake.received_method("turn/steer").len(), 1);
    run.end();
}

#[test]
fn a_withdrawn_queued_start_hands_on_its_context_and_waiting_message_without_a_stale_interrupt() {
    let mut run = Run::start_with("unqueued_followup", |config| {
        config.instructions = Some("EARLIER TALK".into());
    });
    run.send("A");
    run.send("B");
    // Meant for "A", which never runs.
    run.driver.command(ChatCommand::Interrupt).unwrap();
    run.until(|event| matches!(event, ChatEvent::TurnStarted { turn_id } if turn_id == "turn-3"));
    run.finish_turn();
    assert!(
        run.fake.received_method("turn/interrupt").is_empty(),
        "the interrupt meant for the withdrawn start does not stop the next turn"
    );
    assert_eq!(run.fake.received_method("turn/start").len(), 2);
    assert!(run.notices().iter().any(|(_, t)| t.contains("withdrawn")));
    run.end();
}

#[test]
fn an_approval_stage_riwork_cannot_answer_closes_the_prompt_and_old_choices_are_gone() {
    let mut run = Run::start("approval_unanswerable");
    run.send("go");
    run.until(is_approval);
    run.until(|event| {
        matches!(
            event,
            ChatEvent::ApprovalResolved { request_id, decision: Decision::Cancel } if request_id == "ap-1"
        )
    });
    assert!(
        run.notices()
            .iter()
            .any(|(_, text)| text.contains("cannot answer without saving a rule")),
        "{:?}",
        run.notices()
    );
    for decision in [
        Decision::Accept,
        Decision::AcceptForSession,
        Decision::Decline,
    ] {
        assert!(
            run.driver
                .command(ChatCommand::Approve {
                    request_id: "ap-1".into(),
                    decision,
                })
                .is_err()
        );
    }
    assert!(run.fake.received_method("approval/decide").is_empty());
    run.end();
}

#[test]
fn a_typed_message_that_never_reaches_muse_is_the_senders_and_the_chat_is_idle() {
    let script = fixture("muse/carry_over.ndjson")
        .lines()
        .filter(|line| !line.starts_with('#'))
        .take(4)
        .collect::<Vec<_>>()
        .join("\n");
    let fake = Fake::new(&[&script]);
    let (sender, events) = mpsc::channel();
    let mut driver = start(fake.config(Provider::Muse), sender).unwrap();
    let mut seen = until(&events, is_state(ChatState::Idle));
    // Every write fails before a byte goes out, as when the host cancels I/O.
    driver.cancel_io().unwrap()();
    let error = driver
        .command(ChatCommand::Send {
            text: "lost".into(),
        })
        .unwrap_err();
    assert!(!error.starts_with(UNKNOWN_SUBMISSION), "{error}");
    seen.extend(events.try_iter());
    assert_eq!(fold(&seen).state, ChatState::Idle, "{seen:?}");
    assert!(!seen.iter().any(is_notice), "the sender has it back");
    driver.shutdown();
}

#[test]
fn messages_muse_never_took_or_never_confirmed_come_back_when_it_dies_or_stops() {
    fn given_back(events: &[ChatEvent]) -> (String, String) {
        let notices: Vec<String> = fold(events)
            .items
            .into_iter()
            .filter_map(|item| match item.body {
                ItemBody::Notice { text, .. } => Some(text),
                _ => None,
            })
            .collect();
        let find = |lead: &str| {
            notices
                .iter()
                .find(|text| text.starts_with(lead))
                .cloned()
                .unwrap_or_else(|| panic!("no {lead:?} in {notices:?}"))
        };
        (
            find("Muse stopped before these messages were sent"),
            find("Muse stopped before confirming"),
        )
    }
    // Muse dies after reading the first start.
    let mut run = Run::start("exit_while_starting");
    run.send("first");
    run.send("second");
    run.until(|event| {
        matches!(
            event,
            ChatEvent::State {
                state: ChatState::Failed { .. }
            }
        )
    });
    let (unsent, unconfirmed) = given_back(&run.seen);
    assert!(
        unsent.ends_with("“second”") && !unsent.contains("“first”"),
        "{unsent}"
    );
    assert!(unconfirmed.ends_with("“first”"), "{unconfirmed}");
    run.driver.shutdown();
    // The chat is stopped instead.
    let mut run = Run::start("stop_while_starting");
    run.send("first");
    run.send("second");
    run.driver.shutdown();
    run.drain();
    let (unsent, unconfirmed) = given_back(&run.seen);
    assert!(unsent.ends_with("“second”"), "{unsent}");
    assert!(unconfirmed.ends_with("“first”"), "{unconfirmed}");
}

#[test]
fn a_message_sent_while_starting_is_steered_into_exactly_that_turn() {
    let mut run = Run::start("queue_while_starting");
    run.send("slow");
    run.send("follow");
    run.finish_turn();
    assert_eq!(run.fake.received_method("turn/start").len(), 1);
    assert_eq!(
        run.fake.received_method("turn/steer")[0]["params"]["expectedTurnId"],
        "turn-1"
    );
    run.end();
}

#[test]
fn plan_starts_read_only_and_cannot_be_left_and_fast_is_refused() {
    let mut run = Run::start_with("plan", |config| {
        config.approval_mode = ApprovalMode::Plan;
        config.fast = true;
    });
    run.drain();
    let notices = run.notices();
    assert!(
        notices
            .iter()
            .any(|(level, text)| *level == NoticeLevel::Info && text.contains("no planning mode")),
        "{notices:?}"
    );
    assert!(
        notices
            .iter()
            .any(|(level, text)| *level == NoticeLevel::Warning && text.contains("no fast mode")),
        "{notices:?}"
    );
    for refused in [
        ChatCommand::Configure {
            model: None,
            effort: Some("low".into()),
            approval_mode: Some(ApprovalMode::Supervised),
            fast: None,
            contributor_mode: None,
            contributor_opt_out: None,
            claude_billing: None,
        },
        ChatCommand::Configure {
            model: None,
            effort: Some("low".into()),
            approval_mode: None,
            fast: Some(true),
            contributor_mode: None,
            contributor_opt_out: None,
            claude_billing: None,
        },
    ] {
        assert!(run.driver.command(refused).is_err());
    }
    assert!(
        run.fake
            .received_method("session/setApprovalMode")
            .is_empty()
            && run
                .fake
                .received_method("session/setReasoningEffort")
                .is_empty(),
        "a refused change changes nothing"
    );
    // Plan to Plan, and other settings, go on.
    run.driver
        .command(ChatCommand::Configure {
            model: None,
            effort: Some("high".into()),
            approval_mode: Some(ApprovalMode::Plan),
            fast: Some(false),
            contributor_mode: None,
            contributor_opt_out: None,
            claude_billing: None,
        })
        .unwrap();
    super::super::testing::eventually(|| {
        run.fake.received_method("session/setReasoningEffort").len() == 1
    });
    run.end();
}

#[test]
fn a_muse_without_session_mcp_runs_the_chat_and_says_cua_is_not_connected() {
    let run = Run::start("no_session_mcp");
    assert!(
        run.fake.received_method("session/start")[0]["params"]
            .get("config")
            .is_none()
    );
    assert!(
        run.notices()
            .iter()
            .any(|(_, text)| text.contains("Cua.ai Driver is not connected")),
        "{:?}",
        run.notices()
    );
    run.end();
}

#[test]
fn a_request_for_another_session_is_refused_not_shown() {
    let mut run = Run::start("foreign_request");
    run.send("go");
    run.finish_turn();
    assert!(!run.seen.iter().any(is_approval));
    run.end();
}

#[test]
fn noise_from_muse_is_skipped_and_said() {
    let mut run = Run::start("noise");
    run.until(is_notice);
    assert!(
        run.notices()[0].1.contains("oversized"),
        "{:?}",
        run.notices()
    );
    run.send("hi");
    run.finish_turn();
    assert_eq!(
        run.item("msg-1").body,
        ItemBody::AgentMessage {
            text: "still fine".into()
        }
    );
    assert!(
        !run.states()
            .iter()
            .any(|s| matches!(s, ChatState::Failed { .. }))
    );
    run.end();
}

#[test]
fn muse_dying_mid_turn_fails_the_turn_and_the_chat() {
    let mut run = Run::start("exit_mid_turn");
    run.send("work");
    let events = run.until(is_turn_completed);
    let Some(ChatEvent::TurnCompleted {
        outcome: TurnOutcome::Failed { message },
        ..
    }) = events.last()
    else {
        panic!("{events:?}")
    };
    assert!(
        message.contains("provider crashed") || message.contains('7'),
        "{message}"
    );
    run.until(|event| {
        matches!(
            event,
            ChatEvent::State {
                state: ChatState::Failed { .. }
            }
        )
    });
    assert!(
        run.driver
            .command(ChatCommand::Send { text: "x".into() })
            .is_err()
    );
    run.driver.shutdown();
}

#[test]
fn an_idle_unload_stops_the_chat() {
    let mut run = Run::start("session_closed");
    run.until(is_state(ChatState::Stopped));
    assert!(
        run.driver
            .command(ChatCommand::Send { text: "x".into() })
            .is_err()
    );
    run.end();
}

#[test]
fn compaction_shows_only_what_compacted_and_says_the_rest() {
    let mut run = Run::start("compact");
    run.driver.command(ChatCommand::Compact).unwrap();
    run.until(is_notice);
    run.send("long");
    run.finish_turn();
    let transcript = run.transcript();
    let markers: Vec<&str> = transcript
        .items
        .iter()
        .filter(|item| matches!(item.body, ItemBody::Compaction))
        .map(|item| item.id.as_str())
        .collect();
    assert_eq!(markers, ["c-2"]);
    assert!(
        !run.seen.iter().any(|event| matches!(
            event,
            ChatEvent::ItemStarted { item } if item.id == "c-1"
        )),
        "no marker before the outcome is known"
    );
    let notices = run.notices();
    assert!(
        notices
            .iter()
            .any(|(_, t)| t.contains("nothing to compact (no_compactable_history)"))
    );
    assert!(notices.iter().any(|(_, t)| t.contains("summarizer_failed")));
    run.end();
}

// ---- Live: the real `muse serve --provider echo` -------------------------------------

/// The real Muse binary, named exactly (never looked up on `PATH`, so no
/// launcher, RiWork shim or update check runs): run with
/// `MUSE_LIVE_BIN=…/muse-bin-<version> cargo test -p riwork-engine live_ -- --ignored`.
fn live_muse() -> PathBuf {
    std::env::var_os("MUSE_LIVE_BIN")
        .map(PathBuf::from)
        .expect("set MUSE_LIVE_BIN to the muse binary itself")
}

/// A throwaway everything: homes, Muse's data, the workspace, and a private
/// stand-in for `riwork cua mcp`. Muse runs its offline echo provider.
struct Live {
    dir: PathBuf,
    fake_mcp: PathBuf,
}

impl Live {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("riwork-muse-live-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        for sub in [
            "home", "config", "data", "state", "cache", "tmp", "riwork", "work",
        ] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        let program = dir.join("muse-echo");
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\nexec '{}' \"$@\" --provider echo\n",
                live_muse().display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let fake_mcp = dir.join("fake_cua_mcp.py");
        std::fs::write(&fake_mcp, include_str!("testdata/muse/fake_cua_mcp.py")).unwrap();
        std::fs::set_permissions(&fake_mcp, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self { dir, fake_mcp }
    }

    fn config(&self) -> DriverConfig {
        let path = |sub: &str| self.dir.join(sub).into_os_string();
        DriverConfig {
            provider: Provider::Muse,
            program: self.dir.join("muse-echo"),
            cwd: self.dir.join("work"),
            approval_mode: ApprovalMode::Supervised,
            model: None,
            effort: None,
            fast: false,
            contributor_mode: Default::default(),
            trust_workspace: false,
            resume: None,
            resume_has_history: false,
            reporter: Default::default(),
            outstanding_notices: Default::default(),
            extra_args: Vec::new(),
            instructions: None,
            env: vec![
                ("HOME".into(), path("home")),
                ("XDG_CONFIG_HOME".into(), path("config")),
                ("XDG_DATA_HOME".into(), path("data")),
                ("XDG_STATE_HOME".into(), path("state")),
                ("XDG_CACHE_HOME".into(), path("cache")),
                ("TMPDIR".into(), path("tmp")),
                ("MUSE_SESSIONS".into(), path("data")),
                ("MUSE_PROJECTS_HOME".into(), path("data")),
                ("RIWORK_HOME".into(), path("riwork")),
                ("MUSE_NO_AUTO_UPDATE".into(), "1".into()),
                ("MUSE_LOGIN".into(), "0".into()),
            ],
            env_remove: [
                "ANTHROPIC_API_KEY",
                "OPENAI_API_KEY",
                "MUSE_MODEL",
                "MUSE_CHANNEL",
            ]
            .into_iter()
            .map(Into::into)
            .collect(),
        }
    }

    fn start(
        &self,
        config: DriverConfig,
    ) -> (Box<dyn Driver>, Receiver<ChatEvent>, Vec<ChatEvent>) {
        let servers = cua_servers(&config, &self.fake_mcp, None);
        let (sender, events) = mpsc::channel();
        let driver =
            start_with(config, sender, servers).expect("the driver starts against real muse");
        let seen = until(&events, is_state(ChatState::Idle));
        (driver, events, seen)
    }

    /// What the stand-in recorded: starts, requests, ends.
    fn mcp(&self) -> Vec<Value> {
        std::fs::read_to_string(self.dir.join("riwork/fake_cua_mcp.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn mcp_starts(&self) -> Vec<Value> {
        self.mcp()
            .into_iter()
            .filter_map(|entry| entry.get("start").cloned())
            .collect()
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Events until `done` holds of everything seen so far.
fn until_all(
    events: &Receiver<ChatEvent>,
    seen: &mut Vec<ChatEvent>,
    done: impl Fn(&[ChatEvent]) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done(seen) {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "timed out; saw {seen:#?}");
        if let Ok(event) = events.recv_timeout(left) {
            seen.push(event);
        }
    }
}

fn agent_texts(events: &[ChatEvent]) -> Vec<String> {
    fold(events)
        .items
        .into_iter()
        .filter_map(|item| match item.body {
            ItemBody::AgentMessage { text } => Some(text),
            _ => None,
        })
        .collect()
}

fn idle_now(events: &[ChatEvent]) -> bool {
    let transcript = fold(events);
    transcript.state == ChatState::Idle && transcript.turn_id.is_none()
}

#[test]
#[ignore = "live: needs MUSE_LIVE_BIN, the real muse binary"]
fn live_echo_chat_carries_context_attachments_steers_and_resumes_with_cua_mcp() {
    let live = Live::new();
    let mut config = live.config();
    config.instructions = Some("CARRIED CONTEXT".into());
    let (mut driver, events, mut seen) = live.start(config.clone());
    let thread = driver.provider_thread_id().expect("a session");

    // RiWork's Cua server, for this session only: exactly the command, arguments and
    // RiWork home the driver named, initialized and listed by Muse.
    super::super::testing::eventually(|| {
        live.mcp().iter().any(|entry| entry["recv"] == "tools/list")
    });
    let starts = live.mcp_starts();
    assert_eq!(starts.len(), 1, "{:?}", live.mcp());
    assert_eq!(starts[0]["argv"], json!(["cua", "mcp"]));
    assert_eq!(
        starts[0]["env"]["RIWORK_HOME"],
        json!(live.dir.join("riwork").to_string_lossy())
    );
    assert!(live.mcp().iter().any(|entry| entry["recv"] == "initialize"));

    // The carried-over conversation reaches the model; the user sees their words.
    driver
        .command(ChatCommand::Send {
            text: "hello live".into(),
        })
        .unwrap();
    until_all(&events, &mut seen, |seen| {
        seen.iter().any(is_turn_completed) && idle_now(seen)
    });
    let echo = agent_texts(&seen).join("\n");
    assert!(
        echo.contains("CARRIED CONTEXT") && echo.contains("hello live"),
        "{echo}"
    );
    assert!(
        user_events(&seen)
            .iter()
            .all(|item| user_text(item) == "hello live"),
        "{:?}",
        user_events(&seen)
    );

    // Attachments: a real image part and a text file, shown as the user sent them.
    let attachments = snapshots(&live.dir.join("riwork"));
    driver
        .command(ChatCommand::SendAttachments {
            text: "look".into(),
            attachments: attachments.clone(),
        })
        .unwrap();
    let before = seen.len();
    until_all(&events, &mut seen, |seen| {
        seen[before..].iter().any(is_turn_completed) && idle_now(seen)
    });
    let echo = agent_texts(&seen[before..]).join("\n");
    assert!(
        echo.contains("[Image #1]") && echo.contains("secret file body"),
        "{echo}"
    );
    let shown = user_events(&seen[before..]);
    assert!(!shown.is_empty());
    for item in &shown {
        assert_eq!(user_text(item), attachments::summary("look", &attachments));
        assert_eq!(item.presentation.attachments, attachments);
    }

    // A message sent into a running turn is taken by it, or (Muse 1.4.7, once
    // the turn's last model call has begun) acknowledged for no turn and given
    // back to the user; it is never sent twice.
    driver
        .command(ChatCommand::Send {
            text: "long one".into(),
        })
        .unwrap();
    let before = seen.len();
    until_all(&events, &mut seen, |seen| {
        seen[before..].iter().any(is_turn_started)
    });
    driver
        .command(ChatCommand::Send {
            text: "steer me".into(),
        })
        .unwrap();
    let given_back = |seen: &[ChatEvent]| {
        fold(seen).items.iter().any(|item| {
            matches!(&item.body, ItemBody::Notice { text, .. } if text.ends_with("“steer me”"))
        })
    };
    let taken = |seen: &[ChatEvent]| {
        fold(seen)
            .items
            .iter()
            .filter(|item| user_text(item) == "steer me")
            .count()
    };
    until_all(&events, &mut seen, |seen| {
        (taken(seen) > 0 || given_back(seen)) && idle_now(seen)
    });
    assert!(taken(&seen) + usize::from(given_back(&seen)) == 1);
    eprintln!(
        "live steer: {}",
        if taken(&seen) == 1 {
            "taken by the running turn"
        } else {
            "acknowledged for no turn; given back, not resent"
        }
    );

    // Interrupt a running turn: it ends, cancelled or (echo being quick) completed.
    driver
        .command(ChatCommand::Send {
            text: "to interrupt".into(),
        })
        .unwrap();
    let before = seen.len();
    until_all(&events, &mut seen, |seen| {
        seen[before..].iter().any(is_turn_started)
    });
    driver.command(ChatCommand::Interrupt).unwrap();
    until_all(&events, &mut seen, |seen| {
        seen[before..].iter().any(is_turn_completed) && idle_now(seen)
    });
    let outcome = seen[before..].iter().find_map(|event| match event {
        ChatEvent::TurnCompleted { outcome, .. } => Some(outcome.clone()),
        _ => None,
    });
    eprintln!("live interrupt outcome: {outcome:?}");
    assert!(matches!(
        outcome,
        Some(TurnOutcome::Interrupted | TurnOutcome::Completed)
    ));
    driver.shutdown();

    // Resume in a new process: the Cua server is named again, and the
    // conversation, already Muse's, is not sent twice.
    config.resume = Some(thread.clone());
    let (mut driver, events, mut seen) = live.start(config);
    assert_eq!(
        driver.provider_thread_id().as_deref(),
        Some(thread.as_str())
    );
    super::super::testing::eventually(|| live.mcp_starts().len() == 2);
    driver
        .command(ChatCommand::Send {
            text: "after resume".into(),
        })
        .unwrap();
    until_all(&events, &mut seen, |seen| {
        seen.iter().any(is_turn_completed) && idle_now(seen)
    });
    let echo = agent_texts(&seen).join("\n");
    assert!(
        echo.contains("after resume") && !echo.contains("CARRIED"),
        "{echo}"
    );
    driver.shutdown();
}

#[test]
#[ignore = "live: needs MUSE_LIVE_BIN, the real muse binary"]
fn live_plan_chat_runs_under_the_read_only_profile() {
    let live = Live::new();
    let mut config = live.config();
    config.approval_mode = ApprovalMode::Plan;
    let (mut driver, events, mut seen) = live.start(config);
    // The profile is the session's durable record: find it in Muse's own log.
    let mut logs = Vec::new();
    let mut stack = vec![live.dir.join("data")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().is_some_and(|name| name == "session.jsonl") {
                logs.push(std::fs::read_to_string(path).unwrap());
            }
        }
    }
    // Muse records it as a profile commit, the id JSON-escaped inside the record.
    assert!(
        logs.iter()
            .any(|log| log.contains("permission_profile_committed") && log.contains(":read-only")),
        "no read-only profile in {} session logs",
        logs.len()
    );
    assert!(
        driver
            .command(ChatCommand::Configure {
                model: None,
                effort: None,
                approval_mode: Some(ApprovalMode::Supervised),
                fast: None,
                contributor_mode: None,
                contributor_opt_out: None,
                claude_billing: None,
            })
            .is_err()
    );
    driver
        .command(ChatCommand::Send {
            text: "plan this".into(),
        })
        .unwrap();
    until_all(&events, &mut seen, |seen| {
        seen.iter().any(is_turn_completed) && idle_now(seen)
    });
    assert!(agent_texts(&seen).join("\n").contains("plan this"));
    driver.shutdown();
}

#[test]
#[ignore = "live: needs MUSE_LIVE_BIN and its configured provider; queries models without a session or turn"]
fn live_model_discovery_lists_models_without_creating_a_conversation() {
    let live = Live::new();
    let mut config = live.config();
    config.program = live_muse(); // actual default provider, not the echo wrapper
    // Read this machine's configured provider catalog and login. Keep Muse's
    // session/project stores, RiWork state and temporary files isolated; only
    // metadata is queried, with no model turn or interactive login.
    config.env.retain(|(name, _)| {
        ![
            "HOME",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_STATE_HOME",
            "XDG_CACHE_HOME",
        ]
        .iter()
        .any(|key| name == key)
    });
    let models = discover_models(config).unwrap();
    assert!(
        !models.is_empty(),
        "this installed Muse publishes its provider catalog"
    );
    assert!(
        models
            .iter()
            .all(|model| super::super::catalog::valid_model(&model.id))
    );
    assert!(
        live.mcp_starts().is_empty(),
        "discovery attaches no desktop MCP server"
    );
    let sessions = std::fs::read_dir(live.dir.join("data"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "jsonl"))
        .count();
    assert_eq!(sessions, 0, "no conversation was written");
    eprintln!("discovered {} Muse models without a session", models.len());
}
