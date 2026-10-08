use super::*;
use crate::chat::client::{Client, Subscription};
use crate::chat::model::{
    Approval, ApprovalKind, ApprovalMode, Item, ItemBody, ItemStatus, ModelOption, Transcript,
};
use crate::chat::testing::*;
use std::io::{BufRead, BufReader, Write};

fn send(client: &mut Client, chat_id: &str, text: &str) {
    client
        .command(chat_id, ChatCommand::Send { text: text.into() })
        .unwrap();
}

fn turns_completed(log: &[Envelope]) -> usize {
    log.iter()
        .filter(|e| matches!(e.event, ChatEvent::TurnCompleted { .. }))
        .count()
}

fn last_state(log: &[Envelope]) -> Option<ChatState> {
    log.iter().rev().find_map(|e| match &e.event {
        ChatEvent::State { state } => Some(state.clone()),
        _ => None,
    })
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

// ---- Create, list, command ----------------------------------------------------------

#[test]
fn creating_a_chat_saves_it_starts_its_driver_and_lists_it() {
    let host = TestHost::new();
    let mut new = host.new_chat(Provider::Codex);
    new.approval_mode = ApprovalMode::Full;
    new.model = Some("a-model".into());
    new.project_id = Some("project-1".into());
    let created = host.client().create(new).unwrap();
    assert!(Uuid::parse_str(&created.id).is_ok());
    assert_eq!(created.title, "Codex chat");
    assert_eq!(created.cwd, host.work());
    assert_eq!(created.approval_mode, ApprovalMode::Full);
    assert_eq!(created.project_id.as_deref(), Some("project-1"));
    assert_eq!(created.codex_account_id.as_deref(), Some("account-a"));

    // The driver was started once, with what the chat asked for.
    let starts = host.fake().starts.lock().unwrap().clone();
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0].cwd, host.work());
    assert_eq!(starts[0].approval_mode, ApprovalMode::Full);
    assert_eq!(starts[0].model.as_deref(), Some("a-model"));
    assert_eq!(starts[0].resume, None);

    // It reports the thread; the chat is ready and says so everywhere.
    let ready = host.wait_for_state(&created.id, |s| *s == ChatState::Idle);
    assert_eq!(ready.provider_thread_id.as_deref(), Some("thread-1"));
    let listed = host.client().list().unwrap();
    assert_eq!(listed, vec![ready.clone()]);

    // On disk: info.json is the chat, the log starts with its Info.
    let dir = host.home.join("chats").join(&created.id);
    let saved: ChatInfo =
        serde_json::from_str(&fs::read_to_string(dir.join("info.json")).unwrap()).unwrap();
    assert_eq!(saved, ready);
    let log = host.wait_for_log(&created.id, |log| last_state(log) == Some(ChatState::Idle));
    assert_gapless(&log, 1);
    assert!(matches!(&log[0].event, ChatEvent::Info { info } if info.id == created.id));
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&dir.join("info.json")), 0o600);
    assert_eq!(mode(&dir.join("events.jsonl")), 0o600);
}

// ---- Orchestrator chats --------------------------------------------------------------

const PROJECT: &str = "11111111-1111-4111-8111-111111111111";

fn orchestrator_chat(host: &TestHost, scope: OrchestratorScope) -> NewChat {
    let mut new = host.new_chat(Provider::Codex);
    new.project_id = scope.project_id().map(str::to_owned);
    new.orchestrator = Some(scope);
    new
}

#[test]
fn a_scope_has_one_orchestrator_chat_and_it_stays_one_across_a_restart() {
    let mut host = TestHost::new();
    let global = host
        .client()
        .create(orchestrator_chat(&host, OrchestratorScope::Global))
        .unwrap();
    assert_eq!(global.orchestrator, Some(OrchestratorScope::Global));
    // On disk too, so the next host knows it.
    let dir = host.home.join("chats").join(&global.id);
    let saved: ChatInfo =
        serde_json::from_str(&fs::read_to_string(dir.join("info.json")).unwrap()).unwrap();
    assert_eq!(saved.orchestrator, Some(OrchestratorScope::Global));

    // A second one for the scope is refused, naming the first.
    let error = host
        .client()
        .create(orchestrator_chat(&host, OrchestratorScope::Global))
        .unwrap_err();
    assert_eq!(error, format!("{ORCHESTRATOR_EXISTS} {}", global.id));
    // Another scope, and ordinary chats, are not in its way.
    let scope = OrchestratorScope::Project {
        project_id: PROJECT.into(),
    };
    let project = host
        .client()
        .create(orchestrator_chat(&host, scope.clone()))
        .unwrap();
    assert_eq!(project.project_id.as_deref(), Some(PROJECT));
    host.create(Provider::Claude);
    assert_eq!(host.client().list().unwrap().len(), 3);

    host.restart(quick_options());
    let error = host
        .client()
        .create(orchestrator_chat(&host, scope))
        .unwrap_err();
    assert_eq!(error, format!("{ORCHESTRATOR_EXISTS} {}", project.id));

    // Deleting the chat frees the scope.
    host.client().delete(&global.id).unwrap();
    let again = host
        .client()
        .create(orchestrator_chat(&host, OrchestratorScope::Global))
        .unwrap();
    assert_ne!(again.id, global.id);
}

#[test]
fn an_orchestrator_chat_has_the_project_of_its_scope_and_no_worktree() {
    let host = TestHost::new();
    let refused = |new: NewChat| {
        let error = host.client().create(new).unwrap_err();
        assert!(error.contains("project of its scope"), "{error}");
    };
    let mut global = orchestrator_chat(&host, OrchestratorScope::Global);
    global.project_id = Some(PROJECT.into());
    refused(global);
    let scope = OrchestratorScope::Project {
        project_id: PROJECT.into(),
    };
    let mut other = orchestrator_chat(&host, scope.clone());
    other.project_id = Some("22222222-2222-4222-8222-222222222222".into());
    refused(other);
    let mut none = orchestrator_chat(&host, scope.clone());
    none.project_id = None;
    refused(none);
    let mut worktree = orchestrator_chat(&host, scope);
    worktree.worktree_id = Some("33333333-3333-4333-8333-333333333333".into());
    refused(worktree);
    let scope = OrchestratorScope::Project {
        project_id: "not-a-uuid".into(),
    };
    refused(orchestrator_chat(&host, scope));
    assert!(host.client().list().unwrap().is_empty());
}

#[test]
fn a_chat_can_ask_for_its_codex_account_and_keeps_it() {
    let host = TestHost::new();
    let mut new = host.new_chat(Provider::Codex);
    new.codex_account_id = Some("account-b".into());
    let created = host.client().create(new).unwrap();
    assert_eq!(created.codex_account_id.as_deref(), Some("account-b"));
    // It is the chat's own from then on: on disk and in what the host lists.
    let saved: ChatInfo = serde_json::from_str(
        &fs::read_to_string(host.home.join("chats").join(&created.id).join("info.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(saved.codex_account_id.as_deref(), Some("account-b"));
    // A chat that does not ask gets the project's.
    let plain = host.create(Provider::Codex);
    assert_eq!(plain.codex_account_id.as_deref(), Some("account-a"));
    // The request travels as a field that older hosts and clients leave out.
    let line = serde_json::to_string(&host.new_chat(Provider::Codex)).unwrap();
    assert!(!line.contains("codex_account_id"), "{line}");
    let old: NewChat = serde_json::from_str(&line).unwrap();
    assert_eq!(old.codex_account_id, None);
}

#[test]
fn a_chat_needs_a_real_working_directory_and_a_claude_chat_has_no_codex_account() {
    let host = TestHost::new();
    let mut new = host.new_chat(Provider::Claude);
    new.cwd = host.home.join("missing");
    assert!(
        host.client()
            .create(new.clone())
            .unwrap_err()
            .contains("working directory")
    );
    new.cwd = "relative".into();
    assert!(host.client().create(new).is_err());
    assert!(host.client().list().unwrap().is_empty());

    let mut named = host.new_chat(Provider::Claude);
    named.title = Some("  Fix the build \u{7}  ".into());
    let chat = host.client().create(named).unwrap();
    assert_eq!(chat.title, "Fix the build");
    assert_eq!(chat.codex_account_id, None);
    assert_eq!(host.create(Provider::Claude).title, "Claude chat");
}

#[test]
fn commands_reach_the_driver_and_every_event_is_numbered_without_gaps() {
    let host = TestHost::new();
    let chat = host.create(Provider::Codex);
    let mut client = host.client();
    send(&mut client, &chat.id, "hello");
    client.command(&chat.id, ChatCommand::Interrupt).unwrap();
    client
        .command(
            &chat.id,
            ChatCommand::Approve {
                request_id: "r1".into(),
                decision: Decision::Accept,
            },
        )
        .unwrap();
    assert_eq!(
        host.fake().commands(),
        vec![
            ChatCommand::Send {
                text: "hello".into()
            },
            ChatCommand::Interrupt,
            ChatCommand::Approve {
                request_id: "r1".into(),
                decision: Decision::Accept
            },
        ]
    );
    let log = host.wait_for_log(&chat.id, |log| {
        turns_completed(log) == 1 && last_state(log) == Some(ChatState::Idle)
    });
    assert_gapless(&log, 1);
    // The reply made it into the log, in order, after the user's message.
    let texts: Vec<_> = log
        .iter()
        .filter_map(|e| match &e.event {
            ChatEvent::ItemCompleted {
                item:
                    Item {
                        body: ItemBody::AgentMessage { text },
                        ..
                    },
            } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(texts, ["echo: hello"]);
    // Nothing is logged twice: the driver's States are the chat's States.
    let states: Vec<_> = log
        .iter()
        .filter_map(|e| match &e.event {
            ChatEvent::State { state } => Some(state.clone()),
            _ => None,
        })
        .collect();
    // (The chat's first state, Starting, is in its first Info.)
    assert_eq!(
        states,
        [ChatState::Idle, ChatState::Running, ChatState::Idle]
    );
}

#[test]
fn what_the_driver_learns_is_merged_into_the_chats_own_info() {
    let host = TestHost::new();
    let chat = host.create(Provider::Claude);
    host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
    // A driver knows the thread and the model, but not the chat's id or title.
    let config = host.fake().starts.lock().unwrap()[0].clone();
    let mut said = placeholder_info(&config, "session-9");
    said.model = Some("claude-x".into());
    said.title = "not the title".into();
    said.id = "not the id".into();
    host.fake().emit(ChatEvent::Info { info: said });
    let info = host.wait_for_info(&chat.id, |info| info.model.is_some());
    assert_eq!(info.provider_thread_id.as_deref(), Some("session-9"));
    assert_eq!(info.model.as_deref(), Some("claude-x"));
    assert_eq!(
        (info.id.as_str(), info.title.as_str()),
        (chat.id.as_str(), "Claude chat")
    );
    // info.json and the log agree, and the log holds the host's Info, not the driver's.
    let log = host.wait_for_log(&chat.id, |log| {
        log.iter()
            .any(|e| matches!(&e.event, ChatEvent::Info { info } if info.model.is_some()))
    });
    let published = log
        .iter()
        .rev()
        .find_map(|e| match &e.event {
            ChatEvent::Info { info } => Some(info.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(published.id, chat.id);
    assert_eq!(published.provider_thread_id.as_deref(), Some("session-9"));
    let dir = host.home.join("chats").join(&chat.id);
    let saved: ChatInfo =
        serde_json::from_str(&fs::read_to_string(dir.join("info.json")).unwrap()).unwrap();
    assert_eq!(saved, info);
}

#[test]
fn settings_change_the_chat_even_while_it_is_stopped() {
    let host = TestHost::new();
    let chat = host.create(Provider::Codex);
    host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
    let mut client = host.client();
    let configure = ChatCommand::Configure {
        model: Some("m2".into()),
        effort: Some("low".into()),
        approval_mode: Some(ApprovalMode::Plan),
        fast: None,
    };
    client.command(&chat.id, configure.clone()).unwrap();
    assert_eq!(host.fake().commands(), vec![configure.clone()]);
    let info = host.info(&chat.id);
    assert_eq!(
        (
            info.model.as_deref(),
            info.effort.as_deref(),
            info.approval_mode
        ),
        (Some("m2"), Some("low"), ApprovalMode::Plan)
    );
    client.close(&chat.id).unwrap();
    // A stopped chat takes new settings without starting a process.
    let again = ChatCommand::Configure {
        model: Some("m3".into()),
        effort: None,
        approval_mode: Some(ApprovalMode::Full),
        fast: None,
    };
    client.command(&chat.id, again).unwrap();
    assert_eq!(host.fake().start_count(), 1);
    send(&mut client, &chat.id, "go");
    let starts = host.fake().starts.lock().unwrap().clone();
    assert_eq!(starts.len(), 2);
    assert_eq!(
        (
            starts[1].model.as_deref(),
            starts[1].effort.as_deref(),
            starts[1].approval_mode
        ),
        (Some("m3"), Some("low"), ApprovalMode::Full)
    );
}

fn configure_fast(fast: bool) -> ChatCommand {
    ChatCommand::Configure {
        model: None,
        effort: None,
        approval_mode: None,
        fast: Some(fast),
    }
}

#[test]
fn fast_mode_belongs_to_the_chat_and_goes_to_every_driver_that_starts() {
    let host = TestHost::new();
    let mut new = host.new_chat(Provider::Codex);
    new.fast = true;
    let mut client = host.client();
    let chat = client.create(new).unwrap();
    assert!(chat.fast);
    host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
    assert!(host.fake().starts.lock().unwrap()[0].fast);

    // Turned off while the driver runs: the driver is told, and the chat remembers.
    client.command(&chat.id, configure_fast(false)).unwrap();
    assert_eq!(host.fake().commands(), vec![configure_fast(false)]);
    assert!(!host.info(&chat.id).fast);
    let dir = host.home.join("chats").join(&chat.id);
    let saved = |dir: &Path| -> ChatInfo {
        serde_json::from_str(&fs::read_to_string(dir.join("info.json")).unwrap()).unwrap()
    };
    assert!(!saved(&dir).fast);
    // The same again changes nothing and publishes nothing.
    let published = host.log(&chat.id).len();
    client.command(&chat.id, configure_fast(false)).unwrap();
    assert_eq!(host.log(&chat.id).len(), published);

    // Turned on while the chat is stopped: no process starts for it, the next one has it.
    client.close(&chat.id).unwrap();
    client.command(&chat.id, configure_fast(true)).unwrap();
    assert_eq!(host.fake().start_count(), 1, "a real change starts nothing");
    assert!(host.info(&chat.id).fast && saved(&dir).fast);
    send(&mut client, &chat.id, "go");
    let starts = host.fake().starts.lock().unwrap().clone();
    assert_eq!(starts.len(), 2);
    assert!(starts[1].fast && starts[1].resume.is_some());
    // The log says so: an Info with the change, in the order it happened.
    let log = host.wait_for_log(&chat.id, |log| {
        log.iter()
            .any(|e| matches!(&e.event, ChatEvent::Info { info } if info.fast))
    });
    assert_gapless(&log, 1);
}

#[test]
fn only_a_configure_that_changes_nothing_at_all_is_a_retry_and_fast_alone_is_not() {
    let host = TestHost::new();
    let chat = host.create(Provider::Claude);
    let mut client = host.client();
    client.close(&chat.id).unwrap();
    client.command(&chat.id, configure_fast(true)).unwrap();
    assert_eq!(host.fake().start_count(), 1, "fast alone resumes nothing");
    client
        .command(
            &chat.id,
            ChatCommand::Configure {
                model: None,
                effort: None,
                approval_mode: None,
                fast: None,
            },
        )
        .unwrap();
    let starts = host.fake().starts.lock().unwrap().clone();
    assert_eq!(starts.len(), 2, "the retry resumed the provider");
    assert!(starts[1].fast, "with the fast mode the chat has");
}

#[test]
fn the_models_a_driver_reports_are_logged_and_replayed_like_any_event() {
    let host = TestHost::new();
    let chat = host.create(Provider::Codex);
    host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
    let models = vec![
        ModelOption {
            id: "gpt-6.1-sol".into(),
            name: "GPT-6.1-Sol".into(),
            efforts: vec!["low".into(), "high".into()],
            default_effort: Some("low".into()),
            supports_fast: true,
            is_default: true,
            ..ModelOption::default()
        },
        ModelOption {
            id: "plain".into(),
            name: "Plain".into(),
            ..ModelOption::default()
        },
    ];
    host.fake().emit(ChatEvent::Models {
        models: models.clone(),
    });
    let log = host.wait_for_log(&chat.id, |log| {
        log.iter()
            .any(|e| matches!(e.event, ChatEvent::Models { .. }))
    });
    assert_gapless(&log, 1);
    // A tab that connects late gets them from the start of the log.
    let late = Follower::open(&host.socket(), &chat.id, 0).unwrap();
    let mut transcript = Transcript::default();
    for envelope in late.take(log.len()) {
        transcript.apply(&envelope.event);
    }
    assert_eq!(transcript.models, models);
    // A later list replaces it; one with nothing in it clears it.
    host.fake().emit(ChatEvent::Models { models: Vec::new() });
    let log = host.wait_for_log(&chat.id, |log| {
        log.iter()
            .filter(|e| matches!(e.event, ChatEvent::Models { .. }))
            .count()
            == 2
    });
    let mut transcript = Transcript::default();
    for envelope in &log {
        transcript.apply(&envelope.event);
    }
    assert!(transcript.models.is_empty());
}

#[test]
fn requests_for_unknown_chats_and_things_a_stopped_chat_cannot_do_are_refused() {
    let host = TestHost::new();
    let mut client = host.client();
    assert!(client.close("nope").unwrap_err().contains("unknown chat"));
    assert!(client.delete("nope").unwrap_err().contains("unknown chat"));
    assert!(client.command("nope", ChatCommand::Interrupt).is_err());
    assert!(Subscription::open(&host.socket(), "nope", 0).is_err());

    let chat = host.create(Provider::Codex);
    client.close(&chat.id).unwrap();
    // Nothing to interrupt or approve in a chat that has no process.
    for command in [
        ChatCommand::Interrupt,
        ChatCommand::Approve {
            request_id: "r".into(),
            decision: Decision::Accept,
        },
        ChatCommand::Answer {
            request_id: "r".into(),
            answers: vec![],
        },
    ] {
        let error = client.command(&chat.id, command).unwrap_err();
        assert!(error.contains("stopped"), "{error}");
    }
    assert_eq!(
        host.fake().start_count(),
        1,
        "none of them starts a process"
    );
    // Stop is always fine.
    client.command(&chat.id, ChatCommand::Stop).unwrap();
}

#[test]
fn a_request_that_cannot_be_read_gets_an_error_and_the_connection_carries_on() {
    let host = TestHost::new();
    let stream = UnixStream::connect(host.socket()).unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut writer = stream;
    let mut ask = |line: &str| -> Response {
        writeln!(writer, "{line}").unwrap();
        let mut answer = String::new();
        reader.read_line(&mut answer).unwrap();
        serde_json::from_str(&answer).unwrap()
    };
    let bad = ask("this is not json");
    assert!(!bad.ok && bad.error.unwrap().contains("unreadable"));
    let unknown = ask(r#"{"op":"frobnicate","id":"abc"}"#);
    assert_eq!((unknown.ok, unknown.id.as_str()), (false, "abc"));
    let list = ask(r#"{"op":"list","id":"q"}"#);
    assert_eq!(
        (list.ok, list.id.as_str(), list.result),
        (true, "q", Some(serde_json::json!([])))
    );
}

// ---- Subscriptions ---------------------------------------------------------------------

#[test]
fn a_subscriber_gets_the_history_after_since_and_then_live_events_with_no_gap() {
    let host = TestHost::new();
    let chat = host.create(Provider::Codex);
    let mut client = host.client();
    send(&mut client, &chat.id, "first");
    let log = host.wait_for_log(&chat.id, |log| {
        turns_completed(log) == 1 && last_state(log) == Some(ChatState::Idle)
    });
    let total = log.len() as u64;
    assert_gapless(&log, 1);

    // Everything from the start, as logged.
    let all = Follower::open(&host.socket(), &chat.id, 0).unwrap();
    assert_eq!(all.take(total as usize), log);
    // Only what the subscriber has not seen.
    let tail = Follower::open(&host.socket(), &chat.id, 3).unwrap();
    assert_eq!(tail.take(total as usize - 3), log[3..]);
    // Nothing to replay: only what happens next.
    let live = Follower::open(&host.socket(), &chat.id, total).unwrap();

    send(&mut client, &chat.id, "second");
    let more = host.wait_for_log(&chat.id, |log| {
        turns_completed(log) == 2 && last_state(log) == Some(ChatState::Idle)
    });
    assert_gapless(&more, 1);
    let new = &more[total as usize..];
    assert_eq!(live.take(new.len()), new);
    assert_eq!(
        all.take(new.len()),
        new,
        "a live subscriber continues where the replay ended"
    );
    assert_eq!(live.take(0).len(), 0);

    // A client cannot be ahead of the chat.
    let error = Subscription::open(&host.socket(), &chat.id, more.len() as u64 + 1)
        .err()
        .unwrap();
    assert!(error.contains("cannot continue"), "{error}");
}

#[test]
fn events_that_arrive_while_a_replay_is_written_are_not_lost_or_repeated() {
    let host = TestHost::new();
    let chat = host.create(Provider::Codex);
    let mut client = host.client();
    for n in 0..30 {
        send(&mut client, &chat.id, &format!("message {n}"));
    }
    let followers: Vec<_> = (0..8)
        .map(|_| Follower::open(&host.socket(), &chat.id, 0).unwrap())
        .collect();
    // More traffic while the replays are still running.
    for n in 30..40 {
        send(&mut client, &chat.id, &format!("message {n}"));
    }
    let log = host.wait_for_log(&chat.id, |log| {
        turns_completed(log) == 40 && last_state(log) == Some(ChatState::Idle)
    });
    assert_gapless(&log, 1);
    for follower in followers {
        assert_eq!(follower.take(log.len()), log);
    }
}

#[test]
fn a_subscriber_that_falls_behind_is_dropped_without_holding_up_the_others() {
    let host = TestHost::with(Options {
        subscriber_events: 32,
        subscriber_bytes: 8 << 20,
        ..quick_options()
    });
    let chat = host.create(Provider::Codex);
    host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
    let fast = Follower::open(&host.socket(), &chat.id, 0).unwrap();

    // A subscriber that asks and then never reads.
    let mut slow = UnixStream::connect(host.socket()).unwrap();
    slow.set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    let request = Request::Subscribe {
        id: "s".into(),
        chat_id: chat.id.clone(),
        since: 0,
    };
    writeln!(slow, "{}", serde_json::to_string(&request).unwrap()).unwrap();

    // 10 MB of output: more than a socket and a queue of 32 can hold.
    send(&mut host.client(), &chat.id, "flood:150:65536");
    let log = host.wait_for_log(&chat.id, |log| {
        turns_completed(log) == 1 && last_state(log) == Some(ChatState::Idle)
    });
    assert_gapless(&log, 1);
    let total = log.len();
    // The fast one saw all of it, in order.
    let seen = fast.take(total);
    assert_eq!(seen.len(), total);
    assert_gapless(&seen, 1);

    // The slow one was cut off: reading now gets what was written and then the end.
    let mut reader = BufReader::new(slow);
    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    assert!(serde_json::from_str::<Response>(&response).unwrap().ok);
    let mut received = Vec::new();
    let mut line = String::new();
    while reader.read_line(&mut line).unwrap() > 0 && line.ends_with('\n') {
        received.push(serde_json::from_str::<Envelope>(&line).unwrap());
        line.clear();
    }
    assert!(received.len() < total, "{} of {total}", received.len());
    assert_gapless(&received, 1);
    // It resubscribes from the last seq it saw and gets exactly the rest.
    let from = received.last().map_or(0, |e| e.seq);
    let rest = Follower::open(&host.socket(), &chat.id, from).unwrap();
    assert_eq!(rest.take(total - from as usize), log[from as usize..]);
}

// ---- Close, resume, restart, delete -------------------------------------------------------

#[test]
fn closing_a_chat_keeps_its_history_and_the_next_message_resumes_its_thread() {
    let host = TestHost::new();
    let chat = host.create(Provider::Codex);
    let mut client = host.client();
    send(&mut client, &chat.id, "before");
    host.wait_for_log(&chat.id, |log| {
        turns_completed(log) == 1 && last_state(log) == Some(ChatState::Idle)
    });
    let before = host.log(&chat.id);

    let started = Instant::now();
    client.close(&chat.id).unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "a close does not wait for the event channel"
    );
    assert_eq!(host.info(&chat.id).state, ChatState::Stopped);
    assert_eq!(host.fake().shutdowns.load(Ordering::SeqCst), 1);
    let closed = host.log(&chat.id);
    assert_eq!(
        closed[..before.len()],
        before[..],
        "the history is untouched"
    );
    assert_eq!(last_state(&closed), Some(ChatState::Stopped));
    // Closing again changes nothing.
    client.close(&chat.id).unwrap();
    assert_eq!(host.log(&chat.id), closed);

    send(&mut client, &chat.id, "after");
    let starts = host.fake().starts.lock().unwrap().clone();
    assert_eq!(starts.len(), 2);
    assert_eq!(starts[1].resume.as_deref(), Some("thread-1"));
    let log = host.wait_for_log(&chat.id, |log| {
        turns_completed(log) == 2 && last_state(log) == Some(ChatState::Idle)
    });
    assert_gapless(&log, 1);
    assert_eq!(log[..closed.len()], closed[..]);
}

#[test]
fn an_empty_configure_resumes_a_stopped_chat_and_other_configures_do_not() {
    // A tab's Retry sends a Configure that changes nothing.
    let host = TestHost::new();
    let chat = host.create(Provider::Codex);
    let mut client = host.client();
    client.close(&chat.id).unwrap();
    assert_eq!(host.info(&chat.id).state, ChatState::Stopped);
    client
        .command(
            &chat.id,
            ChatCommand::Configure {
                model: Some("gpt-5".into()),
                effort: None,
                approval_mode: None,
                fast: None,
            },
        )
        .unwrap();
    assert_eq!(
        host.fake().starts.lock().unwrap().len(),
        1,
        "a real change starts nothing"
    );
    client
        .command(
            &chat.id,
            ChatCommand::Configure {
                model: None,
                effort: None,
                approval_mode: None,
                fast: None,
            },
        )
        .unwrap();
    let starts = host.fake().starts.lock().unwrap().clone();
    assert_eq!(starts.len(), 2, "the retry resumed the provider");
    assert_eq!(starts[1].resume.as_deref(), Some("thread-1"));
}

#[test]
fn closing_a_chat_in_the_middle_of_a_turn_ends_the_turn() {
    let host = TestHost::new();
    let chat = host.create(Provider::Claude);
    let mut client = host.client();
    send(&mut client, &chat.id, "hang");
    host.wait_for_log(&chat.id, |log| {
        log.iter()
            .any(|e| matches!(e.event, ChatEvent::TurnStarted { .. }))
    });
    host.fake().emit(ChatEvent::ApprovalRequested {
        approval: Approval {
            request_id: "r1".into(),
            item_id: None,
            kind: ApprovalKind::Tool,
            title: "Bash".into(),
            detail: String::new(),
            choices: vec![Decision::Accept, Decision::Decline],
        },
    });
    host.wait_for_log(&chat.id, |log| {
        log.iter()
            .any(|e| matches!(e.event, ChatEvent::ApprovalRequested { .. }))
    });
    client.close(&chat.id).unwrap();

    // Replaying the log gives a chat that is stopped with nothing left open.
    let mut transcript = Transcript::default();
    for envelope in host.log(&chat.id) {
        transcript.apply(&envelope.event);
    }
    assert_eq!(transcript.state, ChatState::Stopped);
    assert_eq!(transcript.turn_id, None);
    assert!(transcript.approvals.is_empty());
    assert!(
        transcript
            .items
            .iter()
            .all(|item| item.status != ItemStatus::InProgress)
    );
    let log = host.log(&chat.id);
    assert!(log.iter().any(|e| matches!(
        &e.event,
        ChatEvent::TurnCompleted {
            outcome: TurnOutcome::Interrupted,
            ..
        }
    )));
    assert_gapless(&log, 1);
}

#[test]
fn a_restarted_host_has_every_chat_stopped_with_its_history_and_starts_nothing_until_asked() {
    let mut host = TestHost::new();
    let chat = host.create(Provider::Codex);
    let mut client = host.client();
    send(&mut client, &chat.id, "remember me");
    let log = host.wait_for_log(&chat.id, |log| {
        turns_completed(log) == 1 && last_state(log) == Some(ChatState::Idle)
    });
    // The chat is still running when the host goes away.
    let thread = host.info(&chat.id).provider_thread_id;
    drop(client);
    host.restart(quick_options());

    let listed = host.client().list().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, chat.id);
    assert_eq!(listed[0].state, ChatState::Stopped);
    assert_eq!(listed[0].provider_thread_id, thread);
    assert_eq!(
        host.fake().start_count(),
        1,
        "loading a chat does not start its provider"
    );

    // The history replays, ending in the stop the old host recorded.
    let replayed = host.log(&chat.id);
    assert_eq!(replayed[..log.len()], log[..]);
    assert_eq!(last_state(&replayed), Some(ChatState::Stopped));
    assert_gapless(&replayed, 1);
    let follower = Follower::open(&host.socket(), &chat.id, 0).unwrap();
    assert_eq!(follower.take(replayed.len()), replayed);

    // The first message starts the provider again, on the saved thread.
    send(&mut host.client(), &chat.id, "do you?");
    let starts = host.fake().starts.lock().unwrap().clone();
    assert_eq!(starts.len(), 2);
    assert_eq!(starts[1].resume, thread);
    let log = host.wait_for_log(&chat.id, |log| {
        turns_completed(log) == 2 && last_state(log) == Some(ChatState::Idle)
    });
    assert_gapless(&log, 1);
    assert_eq!(
        follower.take(log.len() - replayed.len()),
        log[replayed.len()..]
    );
}

#[test]
fn a_chat_that_died_in_the_middle_of_a_turn_is_made_tidy_when_the_next_host_loads_it() {
    let home = short_home();
    let work = home.join("work");
    fs::create_dir_all(&work).unwrap();
    let id = Uuid::new_v4().to_string();
    let info = ChatInfo {
        parent_id: None,
        user_title: None,
        first_user_message: None,

        id: id.clone(),
        provider: Provider::Claude,
        project_id: None,
        worktree_id: None,
        cwd: work.clone(),
        title: "Left running".into(),
        created_at_unix: 5,
        provider_thread_id: Some("session-1".into()),
        model: None,
        effort: None,
        approval_mode: ApprovalMode::Supervised,
        codex_account_id: None,
        state: ChatState::Waiting,
        orchestrator: None,
        fast: false,
    };
    let dir = log::chat_dir(&home, &id).unwrap();
    let chat_log = ChatLog::create(&dir, &info).unwrap();
    let events = [
        ChatEvent::Info { info: info.clone() },
        ChatEvent::State {
            state: ChatState::Running,
        },
        ChatEvent::TurnStarted {
            turn_id: "t1".into(),
        },
        ChatEvent::ApprovalRequested {
            approval: Approval {
                request_id: "r1".into(),
                item_id: None,
                kind: ApprovalKind::Command,
                title: "rm".into(),
                detail: String::new(),
                choices: vec![Decision::Accept],
            },
        },
        ChatEvent::State {
            state: ChatState::Waiting,
        },
    ];
    for (n, event) in events.into_iter().enumerate() {
        let envelope = Envelope {
            chat_id: id.clone(),
            seq: n as u64 + 1,
            event,
        };
        chat_log
            .append(
                format!("{}\n", serde_json::to_string(&envelope).unwrap()).as_bytes(),
                false,
            )
            .unwrap();
    }
    drop(chat_log);
    // The crash cut the next line in half.
    let mut events = OpenOptions::new()
        .append(true)
        .open(dir.join("events.jsonl"))
        .unwrap();
    events
        .write_all(b"{\"chat_id\":\"x\",\"seq\":6,\"ev")
        .unwrap();
    drop(events);

    let host = start_host(&home, quick_options());
    let mut client = Client::connect(host.socket()).unwrap();
    let chat = client.list().unwrap().remove(0);
    assert_eq!(chat.state, ChatState::Stopped);
    let follower = Follower::open(host.socket(), &id, 0).unwrap();
    let mut transcript = Transcript::default();
    let mut seen = Vec::new();
    while seen.len() < 7 {
        let envelope = follower.next().unwrap();
        transcript.apply(&envelope.event);
        seen.push(envelope);
    }
    assert_gapless(&seen, 1);
    assert!(matches!(
        &seen[5].event,
        ChatEvent::TurnCompleted { turn_id, outcome: TurnOutcome::Interrupted } if turn_id == "t1"
    ));
    assert_eq!(transcript.state, ChatState::Stopped);
    assert_eq!(transcript.turn_id, None);
    assert!(transcript.approvals.is_empty());
    drop(host);
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn a_directory_that_is_not_a_chat_is_left_alone_when_chats_load() {
    let home = short_home();
    let chats = home.join("chats");
    fs::create_dir_all(chats.join("not-a-uuid")).unwrap();
    fs::create_dir_all(chats.join(Uuid::new_v4().to_string())).unwrap();
    let host = start_host(&home, quick_options());
    assert!(
        Client::connect(host.socket())
            .unwrap()
            .list()
            .unwrap()
            .is_empty()
    );
    drop(host);
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn deleting_a_chat_stops_it_ends_its_subscriptions_and_removes_its_directory() {
    let host = TestHost::new();
    let chat = host.create(Provider::Codex);
    let kept = host.create_in("other", Provider::Codex);
    let mut client = host.client();
    send(&mut client, &chat.id, "bye");
    host.wait_for_log(&chat.id, |log| turns_completed(log) == 1);
    let follower = Follower::open(&host.socket(), &chat.id, 0).unwrap();
    follower.next().unwrap();

    client.delete(&chat.id).unwrap();
    assert!(!host.home.join("chats").join(&chat.id).exists());
    assert!(host.home.join("chats").join(&kept.id).is_dir());
    assert_eq!(client.list().unwrap().len(), 1);
    assert_eq!(host.fake().shutdowns.load(Ordering::SeqCst), 1);
    // The subscription ends; nothing else is sent on it.
    follower.until_closed();
    assert!(
        client
            .command(&chat.id, ChatCommand::Interrupt)
            .unwrap_err()
            .contains("unknown chat")
    );
    assert!(Subscription::open(&host.socket(), &chat.id, 0).is_err());
    // Gone for good: a restart does not bring it back.
    let mut host = host;
    host.restart(quick_options());
    assert_eq!(host.client().list().unwrap().len(), 1);
}

// ---- Failures ---------------------------------------------------------------------------

#[test]
fn a_provider_that_cannot_start_leaves_a_failed_chat_that_the_next_message_retries() {
    let host = TestHost::new();
    *host.fake().fail_start.lock().unwrap() = Some("codex is not installed".into());
    let chat = host.create(Provider::Codex);
    let failed = ChatState::Failed {
        message: "codex is not installed".into(),
    };
    assert_eq!(chat.state, failed);
    assert_eq!(host.info(&chat.id).state, failed);
    let log = host.log(&chat.id);
    assert_eq!(last_state(&log), Some(failed));
    assert_gapless(&log, 1);

    let mut client = host.client();
    send(&mut client, &chat.id, "again");
    assert_eq!(host.fake().start_count(), 2);
    host.wait_for_log(&chat.id, |log| {
        turns_completed(log) == 1 && last_state(log) == Some(ChatState::Idle)
    });

    // When it still fails, the message says why.
    client.close(&chat.id).unwrap();
    *host.fake().fail_start.lock().unwrap() = Some("no network".into());
    let error = client
        .command(&chat.id, ChatCommand::Send { text: "x".into() })
        .unwrap_err();
    assert_eq!(error, "no network");
}

#[test]
fn a_driver_that_reports_its_own_failure_ends_the_run_and_the_next_message_resumes() {
    let host = TestHost::new();
    let chat = host.create(Provider::Claude);
    let mut client = host.client();
    send(&mut client, &chat.id, "hang");
    host.wait_for_log(&chat.id, |log| {
        log.iter()
            .any(|e| matches!(e.event, ChatEvent::TurnStarted { .. }))
    });
    host.fake().emit(ChatEvent::State {
        state: ChatState::Failed {
            message: "boom".into(),
        },
    });
    let info = host.wait_for_state(&chat.id, |s| matches!(s, ChatState::Failed { .. }));
    assert_eq!(
        info.state,
        ChatState::Failed {
            message: "boom".into()
        }
    );
    let log = host.log(&chat.id);
    let turn_end = log
        .iter()
        .position(|e| matches!(&e.event, ChatEvent::TurnCompleted { outcome: TurnOutcome::Failed { message }, .. } if message == "boom"))
        .expect("the turn ends with the failure");
    assert!(matches!(
        &log[turn_end + 1].event,
        ChatEvent::State {
            state: ChatState::Failed { .. }
        }
    ));
    // The dead process is reaped in the background.
    eventually(|| host.fake().shutdowns.load(Ordering::SeqCst) == 1);

    send(&mut client, &chat.id, "are you back");
    let starts = host.fake().starts.lock().unwrap().clone();
    assert_eq!(starts.len(), 2);
    assert_eq!(starts[1].resume.as_deref(), Some("thread-1"));
    host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
}

#[test]
fn a_provider_that_vanishes_without_a_word_fails_the_chat() {
    let host = TestHost::new();
    let chat = host.create(Provider::Codex);
    host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
    host.fake().vanish();
    let info = host.wait_for_state(&chat.id, |s| matches!(s, ChatState::Failed { .. }));
    assert!(
        matches!(&info.state, ChatState::Failed { message } if message.contains("ended unexpectedly"))
    );
    eventually(|| host.fake().shutdowns.load(Ordering::SeqCst) == 1);
    // Closing a failed chat makes it plainly stopped.
    host.client().close(&chat.id).unwrap();
    assert_eq!(host.info(&chat.id).state, ChatState::Stopped);
}

#[test]
fn events_of_a_run_that_was_replaced_are_dropped() {
    let host = TestHost::new();
    let chat = host.create(Provider::Codex);
    let mut client = host.client();
    host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
    // An end of the channel that outlives the process, as a straggling thread
    // of a real driver might hold.
    let straggler = host.fake().sender().unwrap();
    client.close(&chat.id).unwrap();
    let before = host.log(&chat.id);
    straggler
        .send(ChatEvent::TurnStarted {
            turn_id: "ghost".into(),
        })
        .unwrap();
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(host.log(&chat.id), before);
    assert_eq!(host.info(&chat.id).state, ChatState::Stopped);
}

#[test]
fn a_provider_that_is_gone_before_it_has_started_fails_the_chat() {
    let host = TestHost::new();
    host.fake().vanish_on_start.store(true, Ordering::SeqCst);
    let chat = host.create(Provider::Claude);
    let info = host.wait_for_state(&chat.id, |s| matches!(s, ChatState::Failed { .. }));
    assert!(
        matches!(&info.state, ChatState::Failed { message } if message.contains("ended")),
        "{:?}",
        info.state
    );
    let log = host.log(&chat.id);
    assert!(matches!(last_state(&log), Some(ChatState::Failed { .. })));
    assert_gapless(&log, 1);
    // Nothing is left running, and the next message starts afresh.
    eventually(|| host.fake().shutdowns.load(Ordering::SeqCst) == 1);
    send(&mut host.client(), &chat.id, "again");
    assert_eq!(host.fake().start_count(), 2);
    host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
}

// ---- Lock, socket, lifetime ----------------------------------------------------------------

#[test]
fn only_one_host_runs_per_home_and_a_stopped_host_gives_way_to_the_next() {
    let mut host = TestHost::new();
    match Host::start(&host.home, fake_providers(), quick_options()) {
        Err(StartError::AlreadyRunning) => {}
        other => panic!("{:?}", other.err()),
    }
    // The first one is unharmed.
    assert!(host.client().list().unwrap().is_empty());
    let lock = fs::read_to_string(host.home.join("run/chat.lock")).unwrap();
    assert_eq!(lock.trim(), std::process::id().to_string());
    host.restart(quick_options());
    assert!(host.client().list().unwrap().is_empty());
}

#[test]
fn the_socket_is_private_and_a_dead_hosts_leftovers_are_replaced() {
    let home = short_home();
    let run = home.join("run");
    // A directory that was left open to others, and a socket nobody listens on.
    fs::create_dir_all(&run).unwrap();
    fs::set_permissions(&run, fs::Permissions::from_mode(0o755)).unwrap();
    drop(std::os::unix::net::UnixListener::bind(run.join("chat.sock")).unwrap());
    let host = start_host(&home, quick_options());
    assert_eq!(mode(&run), 0o700);
    assert_eq!(mode(&run.join("chat.sock")), 0o600);
    assert_eq!(mode(&run.join("chat.lock")), 0o600);
    assert_eq!(mode(&home.join("chats")), 0o700);
    assert!(
        Client::connect(host.socket())
            .unwrap()
            .list()
            .unwrap()
            .is_empty()
    );
    drop(host);
    assert!(
        !run.join("chat.sock").exists(),
        "the socket goes with the host"
    );
    // Something that is not a socket is not ours to delete.
    fs::write(run.join("chat.sock"), "precious").unwrap();
    let error = loop {
        match Host::start(&home, fake_providers(), quick_options()) {
            // (A child of another test may still hold the old lock for a moment.)
            Err(StartError::AlreadyRunning) => thread::sleep(Duration::from_millis(20)),
            other => break other.err().unwrap(),
        }
    };
    assert!(error.to_string().contains("not a socket"), "{error}");
    assert_eq!(
        fs::read_to_string(run.join("chat.sock")).unwrap(),
        "precious"
    );
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn a_home_too_long_for_a_unix_socket_is_refused_with_advice() {
    let long = std::env::temp_dir().join("x".repeat(120));
    let error = Host::start(&long, fake_providers(), quick_options())
        .err()
        .unwrap();
    assert!(error.to_string().contains("shorter RIWORK_HOME"), "{error}");
}

#[test]
fn the_peer_of_a_connection_is_identified_by_its_user() {
    let (a, _b) = UnixStream::pair().unwrap();
    assert_eq!(peer_uid(&a).unwrap(), effective_uid());
}

#[test]
fn an_idle_host_exits_but_not_while_a_client_is_connected_or_a_chat_is_at_work() {
    let home = short_home();
    fs::create_dir_all(home.join("work")).unwrap();
    let options = Options {
        idle: Duration::from_millis(400),
        ..quick_options()
    };
    let host = start_host(&home, options);
    let socket = host.socket().to_owned();
    let (exits, exited) = std::sync::mpsc::channel();
    let runner = thread::spawn(move || {
        let exit = host.run();
        exits.send(exit).unwrap();
        drop(host);
    });
    let quiet = |wait: Duration| exited.recv_timeout(wait).ok();

    // A connected client keeps the host up, however long it says nothing.
    let client = Client::connect(&socket).unwrap();
    assert_eq!(quiet(Duration::from_millis(1500)), None);
    // A chat at work does too, with no client at all.
    let mut busy = Client::connect(&socket).unwrap();
    let chat = busy
        .create(NewChat {
            parent_id: None,

            provider: Provider::Codex,
            project_id: None,
            worktree_id: None,
            cwd: home.join("work"),
            codex_account_id: None,
            title: None,
            approval_mode: ApprovalMode::Supervised,
            model: None,
            effort: None,
            orchestrator: None,
            fast: false,
        })
        .unwrap();
    busy.command(
        &chat.id,
        ChatCommand::Send {
            text: "hang".into(),
        },
    )
    .unwrap();
    // The host has seen the turn start; from here on it is the chat that keeps it up.
    eventually(|| busy.list().unwrap()[0].state == ChatState::Running);
    drop((client, busy));
    assert_eq!(quiet(Duration::from_millis(1500)), None);
    // Once the chat is idle again and nobody is connected, the host goes.
    let fake = fake_for(&home.join("work"));
    fake.emit(ChatEvent::TurnCompleted {
        turn_id: "turn-1".into(),
        outcome: TurnOutcome::Completed,
    });
    fake.emit(ChatEvent::State {
        state: ChatState::Idle,
    });
    assert_eq!(quiet(Duration::from_secs(10)), Some(Exit::Idle));
    runner.join().unwrap();
    // It left the socket and the chat's history behind, and stopped the chat.
    assert!(!socket.exists());
    assert_eq!(fake.shutdowns.load(Ordering::SeqCst), 1);
    let host = start_host(&home, quick_options());
    let chats = Client::connect(host.socket()).unwrap().list().unwrap();
    assert_eq!((chats.len(), &chats[0].state), (1, &ChatState::Stopped));
    drop(host);
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn a_host_that_is_asked_to_stop_returns_from_run() {
    let home = short_home();
    let host = start_host(&home, quick_options());
    let stopper = host.stopper();
    let runner = thread::spawn(move || host.run());
    thread::sleep(Duration::from_millis(50));
    stopper.stop();
    assert_eq!(runner.join().unwrap(), Exit::Asked);
    let _ = fs::remove_dir_all(&home);
}

// ---- Starting the host -----------------------------------------------------------------------

/// A stand-in for `riwork`: a script that does what `body` says.
fn stub_riwork(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("riwork-stub");
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    // The first run of a fresh script is slow on macOS; do it before timing anything.
    let _ = Command::new(&path).status();
    path
}

#[test]
fn ensure_reports_why_a_host_that_would_not_start_did_not() {
    let home = short_home();
    let stub = stub_riwork(&home, "echo \"boom: cannot bind\" >&2\nexit 3");
    let error = ensure(&home, &stub).unwrap_err();
    assert!(
        error.contains("exited") && error.contains("boom: cannot bind"),
        "{error}"
    );
    // It asked for the right thing, and left a private run directory.
    assert_eq!(mode(&home.join("run")), 0o700);
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn ensure_waits_for_a_host_that_won_the_race_for_the_lock() {
    let home = short_home();
    // The process `ensure` starts finds the lock taken and leaves with success,
    // while the winner is still getting ready to answer.
    let stub = stub_riwork(&home, "echo 'a chat host already runs' >&2");
    let late = {
        let home = home.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(600));
            start_host(&home, quick_options())
        })
    };
    let socket = ensure(&home, &stub).unwrap();
    assert_eq!(socket, home.join("run/chat.sock"));
    drop(late.join().unwrap());
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn the_app_helper_runs_chat_ensure_with_the_home_it_was_given() {
    let home = short_home();
    let record = home.join("record");
    let stub = stub_riwork(
        &home,
        &format!("echo \"$@ in $RIWORK_HOME\" > {}", record.display()),
    );
    let socket = ensure_host_with(&stub, &home).unwrap();
    assert_eq!(socket, home.join("run/chat.sock"));
    assert_eq!(
        fs::read_to_string(&record).unwrap().trim(),
        format!("chat ensure in {}", home.display())
    );
    let failing = stub_riwork(&home, "echo 'no cua driver' >&2\nexit 1");
    assert_eq!(
        ensure_host_with(&failing, &home).unwrap_err(),
        "no cua driver"
    );
    let silent = stub_riwork(&home, "exit 1");
    assert!(
        ensure_host_with(&silent, &home)
            .unwrap_err()
            .contains("failed")
    );
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn short_requests_keep_an_idle_host_up_because_the_timer_counts_from_the_last_one() {
    let home = short_home();
    let options = Options {
        idle: Duration::from_millis(600),
        ..quick_options()
    };
    let host = start_host(&home, options);
    let socket = host.socket().to_owned();
    let (exits, exited) = std::sync::mpsc::channel();
    let runner = thread::spawn(move || {
        exits.send(host.run()).unwrap();
        drop(host);
    });
    // A request every 200 ms is over long before the host looks, again and again.
    for _ in 0..10 {
        Client::connect(&socket).unwrap().list().unwrap();
        assert!(
            exited.try_recv().is_err(),
            "the host left while it was in use"
        );
        thread::sleep(Duration::from_millis(200));
    }
    assert_eq!(exited.recv_timeout(Duration::from_secs(10)), Ok(Exit::Idle));
    runner.join().unwrap();
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn ensure_starts_another_host_when_the_one_it_found_was_shutting_down() {
    let home = short_home();
    let run = home.join("run");
    fs::create_dir_all(&run).unwrap();
    fs::set_permissions(&run, fs::Permissions::from_mode(0o700)).unwrap();
    // A host that is on its way out holds the lock and answers no one.
    let dying = try_lock(&run.join("chat.lock")).unwrap().unwrap();
    // Every start this stand-in is asked for finds the lock taken and leaves
    // with success, as `chat serve` does, until the lock is free.
    let record = home.join("record");
    let stub = stub_riwork(&home, &format!("echo started >> {}", record.display()));
    let helper = {
        let (home, record) = (home.clone(), record.clone());
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(500));
            drop(dying);
            // The second start is the one that gets a host.
            while fs::read_to_string(&record).map_or(0, |text| text.lines().count()) < 2 {
                thread::sleep(Duration::from_millis(10));
            }
            // (`ensure` looks at the lock now and then, which can make a start
            // that runs at that moment give way; it asks again.)
            loop {
                match Host::start(&home, fake_providers(), quick_options()) {
                    Err(StartError::AlreadyRunning) => thread::sleep(Duration::from_millis(5)),
                    other => break other.unwrap(),
                }
            }
        })
    };
    let socket = ensure_within(&home, &stub, Duration::from_secs(20)).unwrap();
    assert_eq!(socket, run.join("chat.sock"));
    assert!(fs::read_to_string(&record).unwrap().lines().count() >= 2);
    drop(helper.join().unwrap());
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn outstanding_sticky_notices_exclude_resolved_and_superseded_items() {
    let mut transcript = Transcript::default();
    for (id, kind, resolved) in [
        ("auth-old", Some("auth_required"), false),
        ("auth-new", Some("auth_required"), false),
        ("weekly-old", Some("rate_limit:seven_day"), false),
        ("weekly-new", Some("rate_limit:seven_day"), true),
        ("codex", Some("rate_limit:codex"), false),
        ("future-window", Some("rate_limit:future_window"), false),
        ("retry", Some("api_retry"), false),
        ("one-off", None, false),
        ("five-hour", Some("rate_limit:five_hour"), false),
        ("five-hour", Some("rate_limit:five_hour"), true),
    ] {
        let mut body = ItemBody::notice(super::super::model::NoticeLevel::Error, "notice", kind);
        if let ItemBody::Notice {
            resolved: value, ..
        } = &mut body
        {
            *value = resolved;
        }
        transcript.apply(&ChatEvent::ItemCompleted {
            item: Item {
                presentation: Default::default(),
                id: id.into(),
                turn_id: None,
                status: ItemStatus::Completed,
                body,
            },
        });
    }
    let outstanding = outstanding_sticky_notices(&transcript);
    assert_eq!(outstanding.len(), 3);
    assert_eq!(outstanding["auth_required"].id, "auth-new");
    assert_eq!(outstanding["rate_limit:codex"].id, "codex");
    assert_eq!(outstanding["rate_limit:future_window"].id, "future-window");
}

fn dismissal_notice(id: &str, kind: &str, reset: Option<u64>) -> Item {
    let mut body = ItemBody::notice(
        super::super::model::NoticeLevel::Warning,
        "notice",
        Some(kind),
    );
    if let ItemBody::Notice { resets_at, .. } = &mut body {
        *resets_at = reset;
    }
    Item {
        id: id.into(),
        turn_id: None,
        status: ItemStatus::Completed,
        body,
        presentation: Default::default(),
    }
}

// Exercise the host's real persistence, command and subscriber paths without a
// socket or provider process. This also runs where the sandbox forbids bind().
struct NoticeHost {
    home: PathBuf,
    shared: Shared,
}

impl NoticeHost {
    fn shared(home: &Path) -> Shared {
        let dismissals = Arc::new(Mutex::new(
            super::super::notice_dismissals::Dismissals::open(home).unwrap(),
        ));
        let chats = load_chats(home, &dismissals);
        Shared {
            home: home.to_owned(),
            dismissals,
            paths: Paths::new(home).unwrap(),
            providers: fake_providers(),
            options: quick_options(),
            chats: Mutex::new(chats),
            orchestrator_creation: Mutex::new(()),
            connections: AtomicUsize::new(0),
            activity: Mutex::new(Instant::now()),
            quit: AtomicBool::new(false),
        }
    }

    fn new() -> Self {
        let home = short_home();
        fs::create_dir_all(home.join("chats")).unwrap();
        Self {
            shared: Self::shared(&home),
            home,
        }
    }

    fn restart(&mut self) {
        self.shared = Self::shared(&self.home);
    }

    fn create(&self, provider: Provider) -> Arc<Chat> {
        let info: ChatInfo = serde_json::from_value(serde_json::json!({
            "id": Uuid::new_v4().to_string(), "provider": provider, "cwd": self.home,
            "title": "test", "created_at_unix": 0, "state": {"state":"stopped"}
        }))
        .unwrap();
        let dir = log::chat_dir(&self.home, &info.id).unwrap();
        let chat_log = ChatLog::create(&dir, &info).unwrap();
        let mut inner = Inner::new(info.clone(), chat_log, 0, self.shared.dismissals.clone());
        inner.set_account_identity(Some(super::super::account_identity::hash("test-login")));
        inner.append(ChatEvent::Info { info: info.clone() });
        let chat = Arc::new(Chat {
            dir,
            lifecycle: Mutex::new(()),
            inner: Mutex::new(inner),
        });
        lock(&self.shared.chats).insert(info.id, chat.clone());
        chat
    }

    fn emit(&self, chat: &Chat, item: Item) {
        lock(&chat.inner).take_driver_event(ChatEvent::ItemCompleted { item });
    }

    fn dismissed(&self, chat: &Chat, id: &str) -> bool {
        let t = log::read_notice_transcript(&chat.dir).unwrap();
        matches!(
            t.items.iter().find(|item| item.id == id).unwrap().body,
            ItemBody::Notice {
                dismissed: true,
                ..
            }
        )
    }

    fn dismiss(&self, chat: &Arc<Chat>, id: &str) {
        run_command(
            &self.shared,
            chat,
            ChatCommand::DismissNotice { item_id: id.into() },
        )
        .unwrap();
    }
}

impl Drop for NoticeHost {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.home).unwrap();
    }
}

#[test]
fn notice_dismissal_persists_across_host_restart_covers_other_chats_and_snapshot() {
    let mut host = NoticeHost::new();
    let first = host.create(Provider::Claude);
    let second = host.create(Provider::Claude);
    let reset = super::super::notice_dismissals::now() + 3600;
    host.emit(
        &first,
        dismissal_notice("one", "rate_limit:seven_day", Some(reset)),
    );
    host.emit(
        &second,
        dismissal_notice("two", "rate_limit:seven_day", Some(reset)),
    );
    let (tx, rx) = mpsc::sync_channel(10);
    lock(&second.inner).subscribers.push(Subscriber {
        tx,
        queued: Arc::new(AtomicUsize::new(0)),
        max_bytes: 1 << 20,
    });
    // A stopped chat can dismiss without resuming its provider.
    host.dismiss(&first, "one");
    assert!(lock(&first.inner).run.is_none());
    assert!(host.dismissed(&first, "one"));
    assert!(host.dismissed(&second, "two"));
    let live: Envelope =
        serde_json::from_str(&rx.recv_timeout(Duration::from_secs(2)).unwrap()).unwrap();
    assert!(
        matches!(live.event, ChatEvent::ItemCompleted { item } if item.id == "two" && matches!(item.body, ItemBody::Notice { dismissed: true, .. }))
    );
    host.dismiss(&first, "one");
    assert!(rx.try_recv().is_err(), "repeated dismissal is idempotent");
    let snapshot = serde_json::to_value(
        log::read_snapshot(
            &host.home,
            &second.info().id,
            None,
            u64::MAX,
            50,
            1 << 20,
            &[],
        )
        .unwrap(),
    )
    .unwrap();
    assert!(
        snapshot["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["item"]["id"] == "two" && row["item"]["body"]["dismissed"] == true)
    );
    host.emit(
        &second,
        dismissal_notice("three", "rate_limit:seven_day", Some(reset)),
    );
    assert!(host.dismissed(&second, "three"));
    let first_id = first.info().id;
    let second_id = second.info().id;
    drop(first);
    drop(second);
    host.restart();
    assert!(host.dismissed(&host.shared.find(&first_id).unwrap(), "one"));
    assert!(host.dismissed(&host.shared.find(&second_id).unwrap(), "three"));
    let third = host.create(Provider::Claude);
    host.emit(
        &third,
        dismissal_notice("after-restart", "rate_limit:seven_day", Some(reset)),
    );
    assert!(host.dismissed(&third, "after-restart"));
    // An in-place update with a later reset is a new occurrence even with the same id.
    host.emit(
        &third,
        dismissal_notice("after-restart", "rate_limit:seven_day", Some(reset + 3600)),
    );
    assert!(!host.dismissed(&third, "after-restart"));
    host.emit(&third, dismissal_notice("auth", "auth_required", None));
    host.dismiss(&third, "auth");
    assert!(host.dismissed(&third, "auth"));
    host.emit(&third, dismissal_notice("new-auth", "auth_required", None));
    assert!(!host.dismissed(&third, "new-auth"));
    assert!(
        run_command(
            &host.shared,
            &third,
            ChatCommand::DismissNotice {
                item_id: "unknown".into()
            }
        )
        .is_err()
    );
    host.emit(&third, dismissal_notice("retry", "api_retry", None));
    assert!(
        run_command(
            &host.shared,
            &third,
            ChatCommand::DismissNotice {
                item_id: "retry".into()
            }
        )
        .is_err()
    );
}

#[test]
fn rate_limits_control_replays_to_a_late_joiner_and_survives_restart_in_snapshot() {
    let mut host = NoticeHost::new();
    let chat = host.create(Provider::Claude);
    let id = chat.info().id;
    let windows = vec![crate::chat::model::RateWindow {
        id: "seven_day".into(),
        label: "weekly".into(),
        used_percent: 87.0,
        resets_at: Some(1767225600),
        warn_at: 70.0,
    }];
    lock(&chat.inner).take_driver_event(ChatEvent::RateLimits {
        windows: windows.clone(),
    });
    let mut replay = Vec::new();
    log::replay(&chat.dir, 0, lock(&chat.inner).next_seq - 1, &mut replay).unwrap();
    let mut transcript = Transcript::default();
    for line in replay
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
    {
        let envelope: Envelope = serde_json::from_slice(line).unwrap();
        transcript.apply(&envelope.event);
    }
    assert_eq!(transcript.rate_limits, windows);
    drop(chat);
    host.restart();
    let legacy = log::read_snapshot(&host.home, &id, None, u64::MAX, 50, 1 << 20, &[]).unwrap();
    assert!(
        !legacy
            .controls
            .iter()
            .any(|e| matches!(e, ChatEvent::RateLimits { .. }))
    );
    let snapshot =
        log::read_snapshot_with_features(&host.home, &id, None, u64::MAX, 50, 1 << 20, &[], true)
            .unwrap();
    assert!(
        snapshot
            .controls
            .iter()
            .any(|e| matches!(e, ChatEvent::RateLimits { windows: stored } if stored == &windows))
    );
}

#[test]
fn account_notice_keys_persist_but_a_worsened_limit_and_another_account_show() {
    let mut host = NoticeHost::new();
    let first = host.create(Provider::Codex);
    let second = host.create(Provider::Codex);
    for (chat, account) in [(&first, "account-a"), (&second, "account-b")] {
        let mut inner = lock(&chat.inner);
        inner.info.codex_account_id = Some(account.into());
        inner.publish_info();
    }
    let reset = super::super::notice_dismissals::now() + 3600;
    host.emit(
        &first,
        dismissal_notice("warning", "rate_limit:seven_day", Some(reset)),
    );
    host.emit(
        &second,
        dismissal_notice("other", "rate_limit:seven_day", Some(reset)),
    );
    host.dismiss(&first, "warning");
    assert!(host.dismissed(&first, "warning"));
    assert!(!host.dismissed(&second, "other"));
    let id = first.info().id;
    let expected = format!("codex:account-a|rate_limit:seven_day@{reset}");
    let snapshot = log::read_snapshot(&host.home, &id, None, u64::MAX, 50, 1 << 20, &[]).unwrap();
    assert_eq!(snapshot.dismissed_notices, [expected.clone()]);
    assert!(
        log::read_snapshot(
            &host.home,
            &second.info().id,
            None,
            u64::MAX,
            50,
            1 << 20,
            &[]
        )
        .unwrap()
        .dismissed_notices
        .is_empty()
    );
    drop(first);
    drop(second);
    host.restart();
    let first = host.shared.find(&id).unwrap();
    assert!(host.dismissed(&first, "warning"));
    host.emit(
        &first,
        dismissal_notice("same-reset", "rate_limit:seven_day", Some(reset)),
    );
    assert!(host.dismissed(&first, "same-reset"));
    let mut blocking = dismissal_notice("blocked", "rate_limit:seven_day", Some(reset));
    if let ItemBody::Notice { level, .. } = &mut blocking.body {
        *level = crate::chat::model::NoticeLevel::Error;
    }
    host.emit(&first, blocking.clone());
    assert!(
        !host.dismissed(&first, "blocked"),
        "a warning close cannot hide rejection"
    );
    assert!(
        log::read_snapshot(&host.home, &id, None, u64::MAX, 50, 1 << 20, &[])
            .unwrap()
            .dismissed_notices
            .is_empty(),
        "worsening removes the obsolete warning dismissal key"
    );
    host.dismiss(&first, "blocked");
    assert!(host.dismissed(&first, "blocked"));
    let data = fs::read_to_string(host.home.join("chats/notice-dismissals.json")).unwrap();
    assert!(data.contains(&expected) && data.contains("error"));
    host.emit(
        &first,
        dismissal_notice("later-reset", "rate_limit:seven_day", Some(reset + 3600)),
    );
    assert!(!host.dismissed(&first, "later-reset"));
}

#[test]
fn system_login_scopes_isolate_dismissals_and_survive_host_restart() {
    for provider in [Provider::Claude, Provider::Codex] {
        let mut host = NoticeHost::new();
        let chat = host.create(provider);
        let id = chat.info().id;
        let account = |email| super::super::account_identity::hash(email);
        lock(&chat.inner).set_account_identity(Some(account("private-a@example.com")));
        let reset = super::super::notice_dismissals::now() + 3600;
        host.emit(
            &chat,
            dismissal_notice("blocking", "rate_limit:seven_day", Some(reset)),
        );
        host.dismiss(&chat, "blocking");
        assert!(host.dismissed(&chat, "blocking"));
        let serialized =
            fs::read_to_string(host.home.join("chats/notice-dismissals.json")).unwrap();
        assert!(!serialized.contains("private-a"));
        assert!(serialized.contains("sha256:"));
        drop(chat);
        host.restart();
        let chat = host.shared.find(&id).unwrap();
        assert!(host.dismissed(&chat, "blocking"));
        lock(&chat.inner).take_driver_event(ChatEvent::ProviderAccountIdentity {
            identity: Some(account("private-b@example.com")),
        });
        assert!(!host.dismissed(&chat, "blocking"));
        lock(&chat.inner).take_driver_event(ChatEvent::ProviderAccountIdentity { identity: None });
        assert!(!host.dismissed(&chat, "blocking"));
        lock(&chat.inner).take_driver_event(ChatEvent::ProviderAccountIdentity {
            identity: Some(account("private-a@example.com")),
        });
        assert!(host.dismissed(&chat, "blocking"));
        // Identity updates never become protocol events or leak raw account identifiers.
        let log = fs::read_to_string(chat.dir.join("events.jsonl")).unwrap();
        assert!(!log.contains("provider_account_identity"));
        assert!(!log.contains("private-a"));
    }
}

#[test]
fn an_empty_opted_in_snapshot_has_no_rate_limits_control() {
    let host = NoticeHost::new();
    let chat = host.create(Provider::Claude);
    let snapshot = log::read_snapshot_with_features(
        &host.home,
        &chat.info().id,
        None,
        u64::MAX,
        50,
        1 << 20,
        &[],
        true,
    )
    .unwrap();
    assert!(
        !snapshot
            .controls
            .iter()
            .any(|e| matches!(e, ChatEvent::RateLimits { .. }))
    );
}

#[test]
fn resolving_a_dismissed_limit_preserves_its_occurrence_key() {
    let host = NoticeHost::new();
    let chat = host.create(Provider::Codex);
    let reset = super::super::notice_dismissals::now() + 3600;
    let mut item = dismissal_notice("blocking", "rate_limit:codex", Some(reset));
    if let ItemBody::Notice { level, .. } = &mut item.body {
        *level = super::super::model::NoticeLevel::Error;
    }
    host.emit(&chat, item.clone());
    host.dismiss(&chat, "blocking");
    let path = host.home.join("chats/notice-dismissals.json");
    let keys = fs::read(&path).unwrap();
    if let ItemBody::Notice {
        level, resolved, ..
    } = &mut item.body
    {
        *level = super::super::model::NoticeLevel::Info;
        *resolved = true;
    }
    host.emit(&chat, item);
    assert!(host.dismissed(&chat, "blocking"));
    assert_eq!(fs::read(path).unwrap(), keys);
}
