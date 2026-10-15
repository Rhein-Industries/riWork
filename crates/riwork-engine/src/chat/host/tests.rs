use super::*;
use crate::anthropic_key::KeyStore;
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
        contributor_mode: None,
        contributor_opt_out: None,
        claude_billing: None,
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
        contributor_mode: None,
        contributor_opt_out: None,
        claude_billing: None,
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
        contributor_mode: None,
        contributor_opt_out: None,
        claude_billing: None,
    }
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
                contributor_mode: None,
                contributor_opt_out: None,
                claude_billing: None,
            },
        )
        .unwrap();
    let starts = host.fake().starts.lock().unwrap().clone();
    assert_eq!(starts.len(), 2, "the retry resumed the provider");
    assert!(starts[1].fast, "with the fast mode the chat has");
}

#[test]
fn a_muse_chat_refuses_fast_and_moving_its_conversation_into_or_out_of_plan() {
    let host = TestHost::new();
    let chat = host.create(Provider::Muse);
    host.wait_for_info(&chat.id, |info| info.provider_thread_id.is_some());
    let mut client = host.client();
    let plan = ChatCommand::Configure {
        model: None,
        effort: Some("low".into()),
        approval_mode: Some(ApprovalMode::Plan),
        fast: None,
        contributor_mode: None,
        contributor_opt_out: None,
        claude_billing: None,
    };
    for (command, refusal) in [
        (configure_fast(true), "no fast mode"),
        (plan.clone(), "cannot become Plan"),
    ] {
        let error = client.command(&chat.id, command).unwrap_err();
        assert!(error.contains(refusal), "{error}");
    }
    assert!(
        host.fake().commands().is_empty(),
        "nothing reached the driver"
    );
    // Stopped, the host keeps the settings itself, and refuses the same.
    client.close(&chat.id).unwrap();
    for command in [configure_fast(true), plan] {
        assert!(client.command(&chat.id, command).is_err());
    }
    let info = host.info(&chat.id);
    assert_eq!(
        (info.approval_mode, info.fast, info.effort),
        (ApprovalMode::Supervised, false, None)
    );
    // Any other mode change, and fast off, go through.
    client
        .command(
            &chat.id,
            ChatCommand::Configure {
                model: None,
                effort: None,
                approval_mode: Some(ApprovalMode::Full),
                fast: Some(false),
                contributor_mode: None,
                contributor_opt_out: None,
                claude_billing: None,
            },
        )
        .unwrap();
    assert_eq!(host.info(&chat.id).approval_mode, ApprovalMode::Full);
    // Moving another chat to Muse asks no fast mode of it either.
    let codex = host.create(Provider::Codex);
    host.wait_for_state(&codex.id, |s| *s == ChatState::Idle);
    let error = client
        .command(
            &codex.id,
            ChatCommand::Switch {
                provider: Provider::Muse,
                model: None,
                effort: None,
                fast: Some(true),
            },
        )
        .unwrap_err();
    assert!(error.contains("no fast mode"), "{error}");
    assert_eq!(host.info(&codex.id).provider, Provider::Codex);
    // An automation may still ask a new Muse chat for fast: it is made, not fast, and says so.
    let mut new = host.new_chat(Provider::Muse);
    new.fast = true;
    let created = client.create(new).unwrap();
    assert!(!created.fast);
    assert!(
        notices(&host.log(&created.id))
            .iter()
            .any(|notice| notice.contains("no fast mode")),
        "{:?}",
        notices(&host.log(&created.id))
    );
}

/// One request on a connection of its own, as any client of the host's socket writes it.
fn raw_request(host: &TestHost, request: serde_json::Value) -> Response {
    let stream = UnixStream::connect(host.socket()).unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut writer = stream;
    writeln!(writer, "{request}").unwrap();
    let mut answer = String::new();
    reader.read_line(&mut answer).unwrap();
    serde_json::from_str(&answer).unwrap()
}

#[test]
fn a_muse_chat_starts_off_unless_turned_on_and_keeps_its_mode_through_resumes_restarts_and_switches()
 {
    use ContributorMode::{Off, On};
    let mut host = TestHost::new();
    // Made on, it starts on: first, resumed by a message after a stop, and after the host itself
    // restarted.
    let mut new = host.new_chat(Provider::Muse);
    new.contributor_mode = On;
    let chat = host.client().create(new).unwrap();
    assert_eq!(chat.contributor_mode, On);
    assert!(!chat.contributor_opt_out);
    host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
    let mut client = host.client();
    client.close(&chat.id).unwrap();
    send(&mut client, &chat.id, "after a stop");
    host.wait_for_log(&chat.id, |log| turns_completed(log) == 1);
    host.restart(quick_options());
    assert_eq!(
        host.info(&chat.id).contributor_mode,
        On,
        "read back from disk"
    );
    assert_eq!(saved_info(&host, &chat.id).contributor_mode, On);
    send(&mut host.client(), &chat.id, "after a restart");
    host.wait_for_log(&chat.id, |log| turns_completed(log) == 2);
    let starts = host.fake().starts.lock().unwrap().clone();
    assert_eq!(starts.len(), 3);
    assert_eq!(starts[0].resume, None);
    assert_eq!(starts[2].resume.as_deref(), Some("thread-1"));
    assert!(starts.iter().all(|config| config.contributor_mode == On));
    // It stays with the chat through a switch away, and holds again back on Muse.
    for provider in [Provider::Codex, Provider::Muse] {
        let switch = ChatCommand::Switch {
            provider,
            model: None,
            effort: None,
            fast: None,
        };
        host.client().command(&chat.id, switch).unwrap();
        let info = host.info(&chat.id);
        assert_eq!(
            (info.contributor_mode, info.contributor_opt_out),
            (On, false)
        );
    }
    let last = host.fake().starts.lock().unwrap().last().cloned().unwrap();
    assert_eq!(
        (last.provider, last.resume, last.contributor_mode),
        (Provider::Muse, None, On)
    );

    // Asked for nothing, or for an off in any way a caller before `contributor_mode` could, a
    // Muse chat starts off, and says so to a reader from before.
    for (directory, legacy) in [
        ("plain", None),
        (
            "explicit-off",
            Some(serde_json::json!({"contributor_mode": "off"})),
        ),
        (
            "legacy-false",
            Some(serde_json::json!({"contributor_opt_out": false})),
        ),
        (
            "legacy-true",
            Some(serde_json::json!({"contributor_opt_out": true})),
        ),
    ] {
        let cwd = host.home.join(directory);
        fs::create_dir_all(&cwd).unwrap();
        let mut chat = serde_json::json!({"provider": "muse", "cwd": cwd});
        for (key, value) in legacy.iter().flat_map(|fields| fields.as_object().unwrap()) {
            chat[key] = value.clone();
        }
        let answer = raw_request(
            &host,
            serde_json::json!({"op": "create", "id": "c", "chat": chat}),
        );
        assert!(answer.ok, "{directory}: {:?}", answer.error);
        let created: ChatInfo = serde_json::from_value(answer.result.unwrap()).unwrap();
        assert_eq!(
            (created.contributor_mode, created.contributor_opt_out),
            (Off, true),
            "{directory}"
        );
        assert_eq!(created.reported_contributor_mode(), Some(Off));
        eventually(|| fake_for(&cwd).start_count() == 1);
        assert_eq!(
            fake_for(&cwd).starts.lock().unwrap()[0].contributor_mode,
            Off
        );
    }
    // On beside an older off contradicts itself, and makes no chat.
    let mut contradiction = host.new_chat(Provider::Muse);
    contradiction.contributor_mode = On;
    contradiction.contributor_opt_out = true;
    let before = host.client().list().unwrap().len();
    let error = host.client().create(contradiction).unwrap_err();
    assert!(error.contains("cannot be combined"), "{error}");
    assert_eq!(host.client().list().unwrap().len(), before);

    // Codex and Claude have no contributor mode: asked for one either way, made off, as Muse is
    // without fast, and they say so; asked for nothing, they say nothing.
    for provider in [Provider::Codex, Provider::Claude] {
        for (asked, opt_out, said) in [(On, false, true), (Off, true, true), (Off, false, false)] {
            let mut new = host.new_chat(provider);
            new.cwd = host.home.join(format!("{provider:?}-{asked:?}-{opt_out}"));
            fs::create_dir_all(&new.cwd).unwrap();
            new.contributor_mode = asked;
            new.contributor_opt_out = opt_out;
            let created = host.client().create(new).unwrap();
            assert_eq!(
                (created.contributor_mode, created.contributor_opt_out),
                (Off, false)
            );
            assert_eq!(
                fake_for(&created.cwd).starts.lock().unwrap()[0].contributor_mode,
                Off
            );
            let notices = notices(&host.log(&created.id));
            assert_eq!(
                notices.iter().any(|notice| notice.contains("Only Muse")),
                said,
                "{provider:?} {asked:?} {opt_out}: {notices:?}"
            );
        }
    }
}

#[test]
fn a_muse_chat_saved_in_any_form_before_contributor_mode_starts_off_and_an_on_saved_since_stays_on()
{
    use ContributorMode::{Off, On};
    let mut host = TestHost::new();
    // Chats as this host saves them, then rewritten into what hosts before it wrote.
    let forms: [(&str, &dyn Fn(&mut serde_json::Value)); 4] = [
        ("missing", &|info| {
            info.as_object_mut().unwrap().remove("contributor_opt_out");
        }),
        ("opted-in-to-settings", &|info| {
            info["contributor_opt_out"] = false.into();
        }),
        ("opted-out", &|info| {
            info["contributor_opt_out"] = true.into()
        }),
        ("on", &|info| {
            info.as_object_mut().unwrap().remove("contributor_opt_out");
            info["contributor_mode"] = "on".into();
        }),
    ];
    let mut chats = Vec::new();
    for (name, _) in &forms {
        let chat = host.create_in(name, Provider::Muse);
        host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
        chats.push(chat);
    }
    host.stop();
    for ((name, rewrite), chat) in forms.iter().zip(&chats) {
        let dir = host.home.join("chats").join(&chat.id);
        let rewritten = |info: &mut serde_json::Value| {
            info.as_object_mut().unwrap().remove("contributor_mode");
            rewrite(info);
        };
        let path = dir.join("info.json");
        let mut info: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        rewritten(&mut info);
        fs::write(&path, info.to_string()).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.contains("contributor_mode"), *name == "on", "{text}");
        // The log's own copies of the info, as the host that wrote the file wrote them.
        let log = dir.join("events.jsonl");
        let lines: Vec<String> = fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(|line| {
                let mut envelope: serde_json::Value = serde_json::from_str(line).unwrap();
                if envelope["event"]["event"] == "info" {
                    rewritten(&mut envelope["event"]["info"]);
                }
                envelope.to_string()
            })
            .collect();
        fs::write(&log, lines.join("\n") + "\n").unwrap();
    }
    host.restart(quick_options());
    for ((name, _), chat) in forms.iter().zip(&chats) {
        let wanted = if *name == "on" { On } else { Off };
        let info = host.info(&chat.id);
        assert_eq!(info.contributor_mode, wanted, "{name}");
        // Settled: a reader from before sees an off chat opted out, an on one not, and so does a
        // tab that replays the log.
        assert_eq!(info.contributor_opt_out, wanted == Off, "{name}");
        let logged = host
            .log(&chat.id)
            .into_iter()
            .rev()
            .find_map(|envelope| match envelope.event {
                ChatEvent::Info { info } => Some(info),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            logged.reported_contributor_mode(),
            Some(wanted),
            "{name}: {logged:?}"
        );
        send(&mut host.client(), &chat.id, "after the update");
        host.wait_for_log(&chat.id, |log| turns_completed(log) == 1);
        let started = fake_for(&chat.cwd).starts.lock().unwrap().clone();
        assert_eq!(started.len(), 2, "{name}");
        assert_eq!(
            (started[1].resume.as_deref(), started[1].contributor_mode),
            (Some("thread-1"), wanted),
            "{name}"
        );
    }
}

fn configure_contributor(mode: ContributorMode) -> ChatCommand {
    ChatCommand::Configure {
        model: None,
        effort: None,
        approval_mode: None,
        fast: None,
        contributor_mode: Some(mode),
        contributor_opt_out: None,
        claude_billing: None,
    }
}

/// The contributor change a client from before `contributor_mode` sends.
fn configure_opt_out(opt_out: bool) -> ChatCommand {
    ChatCommand::Configure {
        model: None,
        effort: None,
        approval_mode: None,
        fast: None,
        contributor_mode: None,
        contributor_opt_out: Some(opt_out),
        claude_billing: None,
    }
}

#[test]
fn a_contributor_change_either_way_waits_for_a_quiet_muse_chat_and_a_stopped_one_keeps_it() {
    use ContributorMode::{Off, On};
    for (from, to) in [(Off, On), (On, Off)] {
        let host = TestHost::new();
        let mut new = host.new_chat(Provider::Muse);
        new.contributor_mode = from;
        let chat = host.client().create(new).unwrap();
        host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
        let mut client = host.client();
        let refused = |client: &mut Client, command: ChatCommand, reason: &str, keeps| {
            let error = client.command(&chat.id, command).unwrap_err();
            assert!(error.contains(reason), "{from:?} to {to:?}: {error}");
            assert_eq!(host.info(&chat.id).contributor_mode, keeps);
        };
        // A change comes alone: the Muse started again for it could refuse the rest. Beside
        // other settings, the mode the chat has is no change.
        let with_effort = |mode| ChatCommand::Configure {
            model: None,
            effort: Some("low".into()),
            approval_mode: None,
            fast: None,
            contributor_mode: Some(mode),
            contributor_opt_out: None,
            claude_billing: None,
        };
        refused(&mut client, with_effort(to), "on its own", from);
        assert_eq!(host.info(&chat.id).effort, None);
        client.command(&chat.id, with_effort(from)).unwrap();
        host.wait_for_info(&chat.id, |info| info.effort.as_deref() == Some("low"));
        // An older client's false asked for the Muse settings, and on beside an off asks for
        // both: refused, whatever the chat has.
        refused(
            &mut client,
            configure_opt_out(false),
            "no longer follows your Muse settings",
            from,
        );
        let both = ChatCommand::Configure {
            model: None,
            effort: None,
            approval_mode: None,
            fast: None,
            contributor_mode: Some(On),
            contributor_opt_out: Some(true),
            claude_billing: None,
        };
        refused(&mut client, both, "cannot be combined", from);
        assert_eq!(host.fake().start_count(), 1);
        // Still starting, a turn running, a request waiting for the user: refused, nothing
        // changes.
        host.fake().emit(ChatEvent::State {
            state: ChatState::Starting,
        });
        host.wait_for_state(&chat.id, |s| *s == ChatState::Starting);
        refused(
            &mut client,
            configure_contributor(to),
            "still starting",
            from,
        );
        host.fake().emit(ChatEvent::State {
            state: ChatState::Idle,
        });
        host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
        send(&mut client, &chat.id, "hang");
        host.wait_for_log(&chat.id, |log| {
            log.iter().any(|e| {
                matches!(
                    &e.event,
                    ChatEvent::ItemStarted { item } if matches!(item.body, ItemBody::AgentMessage { .. })
                )
            })
        });
        refused(
            &mut client,
            configure_contributor(to),
            "Wait for the turn to finish",
            from,
        );
        assert_eq!(host.fake().start_count(), 1);
        assert_eq!(host.fake().shutdowns.load(Ordering::SeqCst), 0);

        // Stopped, the chat keeps it for its next start and starts nothing; the same again is
        // no change.
        client.close(&chat.id).unwrap();
        for _ in 0..2 {
            client.command(&chat.id, configure_contributor(to)).unwrap();
        }
        assert_eq!(host.info(&chat.id).contributor_mode, to);
        assert_eq!(saved_info(&host, &chat.id).contributor_mode, to);
        assert_eq!(host.fake().start_count(), 1, "nothing started");
        send(&mut client, &chat.id, "next");
        host.wait_for_log(&chat.id, |log| {
            turns_completed(log) == 2 && last_state(log) == Some(ChatState::Idle)
        });
        let started = host.fake().starts.lock().unwrap()[1].clone();
        assert_eq!(
            (started.resume.as_deref(), started.contributor_mode),
            (Some("thread-1"), to)
        );
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
        refused(
            &mut client,
            configure_contributor(from),
            "Wait for the turn to finish",
            to,
        );
        assert_eq!(host.fake().start_count(), 2);
        assert!(
            host.fake()
                .configurations
                .lock()
                .unwrap()
                .iter()
                .all(|(command, _)| matches!(
                    command,
                    ChatCommand::Configure {
                        contributor_mode: None,
                        contributor_opt_out: None,
                        claude_billing: None,
                        ..
                    }
                )),
            "no driver is asked for contributor mode"
        );
    }

    // An older client's true is an off: it turns an on chat off, and is no change to an off one.
    let host = TestHost::new();
    let mut new = host.new_chat(Provider::Muse);
    new.contributor_mode = On;
    let chat = host.client().create(new).unwrap();
    host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
    let mut client = host.client();
    client.close(&chat.id).unwrap();
    for _ in 0..2 {
        client.command(&chat.id, configure_opt_out(true)).unwrap();
        let info = host.info(&chat.id);
        assert_eq!(
            (info.contributor_mode, info.contributor_opt_out),
            (Off, true)
        );
    }
    assert_eq!(host.fake().start_count(), 1, "nothing started");

    // Codex and Claude have no contributor mode, as Muse has no fast one.
    for provider in [Provider::Codex, Provider::Claude] {
        let other = host.create_in(&format!("{provider:?}"), provider);
        host.wait_for_state(&other.id, |s| *s == ChatState::Idle);
        let error = client
            .command(&other.id, configure_contributor(On))
            .unwrap_err();
        assert!(
            error.contains("Only Muse has a contributor mode"),
            "{error}"
        );
        // What it has already is no change, as fast off is none for Muse.
        client
            .command(&other.id, configure_contributor(Off))
            .unwrap();
        client.command(&other.id, configure_opt_out(true)).unwrap();
        assert_eq!(host.info(&other.id).contributor_mode, Off);
        let fake = fake_for(&other.cwd);
        assert_eq!(fake.start_count(), 1);
        assert!(fake.commands().is_empty(), "{:?}", fake.commands());
    }

    // A Muse chat turned on keeps it through a switch away; there it can be turned off, which
    // only stores it, but not on again, and back on Muse it starts off.
    let mut new = host.new_chat(Provider::Muse);
    new.cwd = host.home.join("switched");
    fs::create_dir_all(&new.cwd).unwrap();
    new.contributor_mode = On;
    let chat = client.create(new).unwrap();
    host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
    let switch = |provider| ChatCommand::Switch {
        provider,
        model: None,
        effort: None,
        fast: None,
    };
    client.command(&chat.id, switch(Provider::Codex)).unwrap();
    host.wait_for_info(&chat.id, |info| {
        info.provider == Provider::Codex && info.state == ChatState::Idle
    });
    let fake = fake_for(&chat.cwd);
    let starts = fake.start_count();
    assert_eq!(host.info(&chat.id).contributor_mode, On);
    // Beside other settings too: nothing starts again for it.
    let off_and_effort = ChatCommand::Configure {
        model: None,
        effort: Some("low".into()),
        approval_mode: None,
        fast: None,
        contributor_mode: Some(Off),
        contributor_opt_out: None,
        claude_billing: None,
    };
    client.command(&chat.id, off_and_effort).unwrap();
    let info = host.wait_for_info(&chat.id, |info| info.effort.as_deref() == Some("low"));
    assert_eq!(
        (info.contributor_mode, info.contributor_opt_out),
        (Off, false)
    );
    assert_eq!(fake.start_count(), starts, "the Codex agent runs on");
    let error = client
        .command(&chat.id, configure_contributor(On))
        .unwrap_err();
    assert!(error.contains("keeps it off"), "{error}");
    client.command(&chat.id, switch(Provider::Muse)).unwrap();
    let back = fake.starts.lock().unwrap().last().cloned().unwrap();
    assert_eq!(
        (back.provider, back.contributor_mode),
        (Provider::Muse, Off)
    );
    assert!(host.info(&chat.id).contributor_opt_out);
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
        provider_title: None,

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
        contributor_mode: Default::default(),
        contributor_opt_out: false,
        claude_billing: None,
        carried_over: None,
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
#[ignore = "slow: idle-exit timer, 1.5 s quiet periods (3.5 s)"]
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
            contributor_mode: Default::default(),
            contributor_opt_out: false,
            claude_billing: None,
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
#[ignore = "slow: stub process racing a host started after 600 ms"]
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
#[ignore = "slow: idle timer against 200 ms sleeps (2.5 s)"]
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
#[ignore = "slow: stub processes racing a lock released after 500 ms"]
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
    /// Worker chats' ended turns, delivered by `deliver_worker_notes` in a real host.
    worker_notes: Mutex<Receiver<ParentNote>>,
}

impl NoticeHost {
    fn shared(home: &Path) -> (Shared, Receiver<ParentNote>) {
        let dismissals = Arc::new(Mutex::new(
            super::super::notice_dismissals::Dismissals::open(home).unwrap(),
        ));
        let (worker_notes, receiver) = mpsc::channel();
        let chats = load_chats(home, &dismissals, &fake_providers(), &worker_notes);
        let shared = Shared {
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
            worker_notes,
        };
        (shared, receiver)
    }

    fn new() -> Self {
        let home = short_home();
        fs::create_dir_all(home.join("chats")).unwrap();
        let (shared, worker_notes) = Self::shared(&home);
        Self {
            shared,
            home,
            worker_notes: Mutex::new(worker_notes),
        }
    }

    fn restart(&mut self) {
        let (shared, worker_notes) = Self::shared(&self.home);
        self.shared = shared;
        self.worker_notes = Mutex::new(worker_notes);
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
        inner.set_account_identity(Some(super::super::account_identity::Identity::fixture(
            super::super::account_identity::hash("test-login"),
        )));
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
        inner.set_account_identity(Some(super::super::account_identity::Identity::fixture(
            super::super::account_identity::hash(account),
        )));
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
    let expected = format!(
        "codex:{}|rate_limit:seven_day@{reset}",
        super::super::account_identity::hash("account-a")
    );
    let snapshot = log::read_snapshot(&host.home, &id, None, u64::MAX, 50, 1 << 20, &[]).unwrap();
    // The account's fingerprint is kept on the Mac, out of what a phone receives.
    assert_eq!(
        snapshot.dismissed_notices,
        [format!("codex|rate_limit:seven_day@{reset}")]
    );
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
        let account = |email| {
            super::super::account_identity::Identity::fixture(super::super::account_identity::hash(
                email,
            ))
        };
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

#[test]
fn canonical_claude_dismissal_survives_rotated_tokens_and_host_restart() {
    let fake = crate::chat::testkit::Fake::new(&[]);
    let config = fake.config(Provider::Claude);
    let dir = config
        .env
        .iter()
        .find(|(name, _)| name == "CLAUDE_CONFIG_DIR")
        .unwrap()
        .1
        .clone();
    let credentials = |token| serde_json::json!({"oauthAccount":{"accountUuid":"account-a","emailAddress":"private@example.test"},"claudeAiOauth":{"refreshToken":token}});
    let path = PathBuf::from(dir).join(".credentials.json");
    fs::write(&path, serde_json::to_vec(&credentials("before")).unwrap()).unwrap();
    let mut host = NoticeHost::new();
    let chat = host.create(Provider::Claude);
    let identity = super::super::account_identity::canonical(&config, None, None).unwrap();
    lock(&chat.inner).set_account_identity(Some(identity.clone()));
    let reset = super::super::notice_dismissals::now() + 3600;
    host.emit(
        &chat,
        dismissal_notice("weekly", "rate_limit:seven_day", Some(reset)),
    );
    host.dismiss(&chat, "weekly");
    let id = chat.info().id;
    drop(chat);
    fs::write(&path, serde_json::to_vec(&credentials("after")).unwrap()).unwrap();
    host.restart();
    let chat = host.shared.find(&id).unwrap();
    let fresh = super::super::account_identity::canonical(
        &config,
        None,
        lock(&chat.inner).provider_account_identity.as_ref(),
    )
    .unwrap();
    assert_eq!(fresh.scope, identity.scope);
    lock(&chat.inner).set_account_identity(Some(fresh));
    assert!(host.dismissed(&chat, "weekly"));
    let snapshot = log::read_snapshot(&host.home, &id, None, u64::MAX, 50, 1 << 20, &[]).unwrap();
    assert_eq!(
        snapshot.dismissed_notices,
        [format!("claude|rate_limit:seven_day@{reset}")]
    );
    let persisted = fs::read_to_string(chat.dir.join("account-identity.json")).unwrap();
    assert!(!persisted.contains("private@example"));
    assert!(!persisted.contains("account-a"));
    assert!(!persisted.contains("before"));
    assert!(!persisted.contains("after"));
}

#[test]
fn a_legacy_token_scope_is_migration_evidence_only_and_never_a_new_scope() {
    let mut host = NoticeHost::new();
    let chat = host.create(Provider::Claude);
    let id = chat.info().id;
    let old = super::super::account_identity::hash("Claude:credential:legacy-token");
    fs::write(
        chat.dir.join("account-identity.json"),
        serde_json::to_vec(&Some(old)).unwrap(),
    )
    .unwrap();
    drop(chat);
    host.restart();
    let chat = host.shared.find(&id).unwrap();
    assert!(lock(&chat.inner).account_scope().is_none());
    host.emit(
        &chat,
        dismissal_notice(
            "blocking",
            "rate_limit:seven_day",
            Some(super::super::notice_dismissals::now() + 3600),
        ),
    );
    assert!(
        dismiss_notice(&host.shared, &chat, "blocking")
            .unwrap_err()
            .contains("identity is unavailable")
    );
}

#[test]
fn a_verified_codex_email_to_id_mapping_survives_logout_and_restart() {
    let fake = crate::chat::testkit::Fake::new(&[]);
    let mut config = fake.config(Provider::Codex);
    let mut host = NoticeHost::new();
    let chat = host.create(Provider::Codex);
    let id = chat.info().id;
    for (name, value) in &mut config.env {
        if name == "RIWORK_HOME" {
            *value = host.home.as_os_str().to_owned();
        }
    }
    config
        .env
        .push(("RIWORK_CHAT_ID".into(), id.clone().into()));
    let auth = PathBuf::from(
        config
            .env
            .iter()
            .find(|(name, _)| name == "CODEX_HOME")
            .unwrap()
            .1
            .clone(),
    )
    .join("auth.json");
    let email_frame = serde_json::json!({"account":{"email":"a@example.test"}});
    fs::write(&auth, serde_json::to_vec(&email_frame).unwrap()).unwrap();
    let email = super::super::account_identity::canonical(&config, None, None).unwrap();
    lock(&chat.inner).set_account_identity(Some(email.clone()));
    let reset = super::super::notice_dismissals::now() + 3600;
    host.emit(
        &chat,
        dismissal_notice("weekly", "rate_limit:codex", Some(reset)),
    );
    host.dismiss(&chat, "weekly");
    fs::write(&auth, br#"{"tokens":{"account_id":"account-a"}}"#).unwrap();
    let account =
        super::super::account_identity::canonical(&config, Some(&email_frame), Some(&email))
            .unwrap();
    assert_ne!(account.scope, email.scope);
    lock(&chat.inner).set_account_identity(Some(account.clone()));
    assert!(host.dismissed(&chat, "weekly"));
    lock(&chat.inner).set_account_identity(None);
    assert!(lock(&chat.inner).account_scope().is_none());
    drop(chat);
    fs::write(&auth, serde_json::to_vec(&email_frame).unwrap()).unwrap();
    host.restart();
    let returned =
        super::super::account_identity::canonical(&config, Some(&email_frame), None).unwrap();
    assert_eq!(returned.scope, account.scope);
    let chat = host.shared.find(&id).unwrap();
    lock(&chat.inner).set_account_identity(Some(returned));
    assert!(host.dismissed(&chat, "weekly"));
    let snapshot = log::read_snapshot(&host.home, &id, None, u64::MAX, 50, 1 << 20, &[]).unwrap();
    assert_eq!(
        snapshot.dismissed_notices,
        [format!("codex|rate_limit:codex@{reset}")]
    );
}

// ---- Switching provider -------------------------------------------------------------------------

fn switch(provider: Provider, model: Option<&str>) -> ChatCommand {
    ChatCommand::Switch {
        provider,
        model: model.map(str::to_owned),
        effort: None,
        fast: None,
    }
}

fn notices(log: &[Envelope]) -> Vec<String> {
    log.iter()
        .filter_map(|e| match &e.event {
            ChatEvent::ItemCompleted {
                item:
                    Item {
                        body: ItemBody::Notice { text, .. },
                        ..
                    },
            } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// The fake `muse serve` the host's real Muse driver runs in the next test.
static MUSE_FIXTURE: std::sync::OnceLock<(PathBuf, PathBuf)> = std::sync::OnceLock::new();

/// The fake providers' configuration, with Muse started from `MUSE_FIXTURE`.
fn muse_fixture_config(
    home: &Path,
    info: &ChatInfo,
    resume: Option<String>,
) -> Result<DriverConfig, String> {
    let mut config = (fake_providers().config)(home, info, resume)?;
    if info.provider == Provider::Muse {
        let (program, dir) = MUSE_FIXTURE.get().cloned().ok_or("no Muse fixture")?;
        config.program = program;
        config.env = vec![("RIWORK_HOME".into(), dir.into_os_string())];
    }
    Ok(config)
}

#[test]
fn a_chat_switched_to_muse_gives_the_real_driver_the_conversation_with_the_first_message() {
    let fake = crate::chat::testkit::Fake::new(&[&crate::chat::testkit::fixture(
        "muse/host_switch.ndjson",
    )]);
    MUSE_FIXTURE
        .set((fake.program.clone(), fake.dir.clone()))
        .unwrap();
    let mut providers = fake_providers();
    providers.muse = super::super::muse::start;
    providers.config = muse_fixture_config;
    let host = TestHost::with_providers(quick_options(), providers);
    let chat = host.create(Provider::Codex);
    let mut client = host.client();
    send(&mut client, &chat.id, "remember the blue door");
    host.wait_for_log(&chat.id, |log| {
        turns_completed(log) == 1 && last_state(log) == Some(ChatState::Idle)
    });
    client
        .command(&chat.id, switch(Provider::Muse, None))
        .unwrap();
    host.wait_for_info(&chat.id, |info| {
        info.provider == Provider::Muse && info.state == ChatState::Idle
    });
    send(&mut client, &chat.id, "hello");
    host.wait_for_log(&chat.id, |log| {
        turns_completed(log) == 2 && last_state(log) == Some(ChatState::Idle)
    });
    let start = &fake.received_method("turn/start")[0]["params"];
    let context = start["input"][0]["text"].as_str().unwrap();
    assert!(
        context.contains("taking it over")
            && context.contains("remember the blue door")
            && context.ends_with("\n\n"),
        "{context}"
    );
    assert_eq!(
        start["input"][1],
        serde_json::json!({"type": "text", "text": "hello"})
    );
    assert_eq!(start["displayText"], "hello");
    let log = host.log(&chat.id);
    let echoed: Vec<&str> = log
        .iter()
        .filter_map(|e| match &e.event {
            ChatEvent::ItemStarted { item } | ChatEvent::ItemCompleted { item }
                if item.id == "um-1" =>
            {
                match &item.body {
                    ItemBody::UserMessage { text } => Some(text.as_str()),
                    _ => None,
                }
            }
            _ => None,
        })
        .collect();
    assert!(
        !echoed.is_empty() && echoed.iter().all(|text| *text == "hello"),
        "the durable log keeps only what was typed: {echoed:?}"
    );
    assert!(
        notices(&log)
            .iter()
            .any(|notice| notice.contains("goes to Muse with the next message")),
        "{:?}",
        notices(&log)
    );
    drop(host);
    assert!(!fake.saw("mismatch"), "{:#?}", fake.entries());
}

/// The fake `muse serve` of each test that runs the host's real Muse driver, by its host's home.
fn muse_fakes() -> &'static Mutex<HashMap<PathBuf, (PathBuf, PathBuf)>> {
    static FAKES: std::sync::OnceLock<Mutex<HashMap<PathBuf, (PathBuf, PathBuf)>>> =
        std::sync::OnceLock::new();
    FAKES.get_or_init(Default::default)
}

/// The fake providers' configuration, with Muse started from the fake of the host's test.
fn muse_fake_config(
    home: &Path,
    info: &ChatInfo,
    resume: Option<String>,
) -> Result<DriverConfig, String> {
    let mut config = (fake_providers().config)(home, info, resume)?;
    if info.provider == Provider::Muse {
        let (program, dir) = lock(muse_fakes())
            .get(home)
            .cloned()
            .ok_or("no Muse fake for this host")?;
        config.program = program;
        config.env = vec![("RIWORK_HOME".into(), dir.into_os_string())];
    }
    Ok(config)
}

/// A host whose Muse chats run the real driver against `fake`, and one such chat with a
/// standing model, effort and approval mode, and `contributor_mode`.
fn muse_host_and_chat(
    fake: &crate::chat::testkit::Fake,
    contributor_mode: ContributorMode,
) -> (TestHost, ChatInfo) {
    let mut providers = fake_providers();
    providers.muse = super::super::muse::start;
    providers.config = muse_fake_config;
    let host = TestHost::with_providers(quick_options(), providers);
    lock(muse_fakes()).insert(host.home.clone(), (fake.program.clone(), fake.dir.clone()));
    let mut new = host.new_chat(Provider::Muse);
    new.model = Some("muse-pro".into());
    new.effort = Some("high".into());
    new.approval_mode = ApprovalMode::Full;
    new.contributor_mode = contributor_mode;
    let chat = host.client().create(new).unwrap();
    (host, chat)
}

/// The arguments of every `muse serve` the fake was started with, in order.
fn serve_argv(fake: &crate::chat::testkit::Fake) -> Vec<Vec<String>> {
    fake.starts()
        .iter()
        .map(|start| serde_json::from_value(start["argv"].clone()).unwrap())
        .collect()
}

/// `muse serve` with a contributor mode, as every start of a Muse chat says it.
fn serve(mode: ContributorMode) -> Vec<String> {
    ["serve", "--contributor-mode", mode.as_str()]
        .map(str::to_owned)
        .to_vec()
}

#[test]
fn a_muse_chat_made_on_says_so_to_every_serve_and_turning_it_off_and_on_resumes_the_same_session() {
    use crate::chat::testkit::{Fake, fixture};
    use ContributorMode::{Off, On};
    let resume = fixture("muse/contributor_resume.ndjson");
    let fake = Fake::new(&[
        &fixture("muse/contributor_start.ndjson"),
        &resume,
        &resume,
        &fixture("muse/contributor_resume_turn.ndjson"),
        &resume,
        &fixture("muse/contributor_switch_start.ndjson"),
    ]);
    let (mut host, chat) = muse_host_and_chat(&fake, On);
    assert_eq!(chat.contributor_mode, On);
    let mut client = host.client();
    send(&mut client, &chat.id, "hello");
    host.wait_for_log(&chat.id, |log| {
        turns_completed(log) == 1 && last_state(log) == Some(ChatState::Idle)
    });
    // Off, then on again: each a `muse serve` of its own on the same session, which takes the
    // chat's model, approval mode and effort again as any resume does.
    for (mode, starts) in [(Off, 2), (On, 3)] {
        client
            .command(&chat.id, configure_contributor(mode))
            .unwrap();
        let info = host.wait_for_info(&chat.id, |info| info.state == ChatState::Idle);
        assert_eq!(
            (
                info.contributor_mode,
                info.provider_thread_id.as_deref(),
                info.model.as_deref(),
                info.effort.as_deref(),
                info.approval_mode,
            ),
            (
                mode,
                Some("sess-1"),
                Some("muse-pro"),
                Some("high"),
                ApprovalMode::Full
            )
        );
        // Every frame the resume sends has reached the fake before the next restart.
        eventually(|| fake.received_method("model/list").len() == starts);
    }
    assert_eq!(fake.received_method("session/start").len(), 1);
    assert_eq!(fake.received_method("session/setModel").len(), 2);
    assert_eq!(fake.received_method("session/setApprovalMode").len(), 2);
    assert_eq!(fake.received_method("session/setReasoningEffort").len(), 3);

    // Stopped, the chat keeps it and starts nothing; its next message starts Muse with it.
    client.close(&chat.id).unwrap();
    client
        .command(&chat.id, configure_contributor(Off))
        .unwrap();
    assert_eq!(fake.starts().len(), 3, "a stopped chat starts nothing");
    assert_eq!(host.info(&chat.id).contributor_mode, Off);
    send(&mut client, &chat.id, "again");
    host.wait_for_log(&chat.id, |log| {
        turns_completed(log) == 2 && last_state(log) == Some(ChatState::Idle)
    });

    // On again while stopped, it holds across a restart of the host itself.
    client.close(&chat.id).unwrap();
    client.command(&chat.id, configure_contributor(On)).unwrap();
    drop(client);
    host.restart(quick_options());
    assert_eq!(saved_info(&host, &chat.id).contributor_mode, On);
    assert_eq!(
        host.info(&chat.id).contributor_mode,
        On,
        "read back from disk"
    );
    let retry = ChatCommand::Configure {
        model: None,
        effort: None,
        approval_mode: None,
        fast: None,
        contributor_mode: None,
        contributor_opt_out: None,
        claude_billing: None,
    };
    let mut client = host.client();
    client.command(&chat.id, retry).unwrap();
    host.wait_for_info(&chat.id, |info| info.state == ChatState::Idle);
    eventually(|| fake.received_method("model/list").len() == 5);
    let resumed = fake.received_method("session/resume");
    assert_eq!(resumed.len(), 4);
    assert!(
        resumed
            .iter()
            .all(|frame| frame["params"]["sessionId"] == "sess-1"),
        "{resumed:#?}"
    );

    // Away to Codex and back: the new Muse session starts with it too.
    for provider in [Provider::Codex, Provider::Muse] {
        let switch = ChatCommand::Switch {
            provider,
            model: None,
            effort: None,
            fast: None,
        };
        client.command(&chat.id, switch).unwrap();
    }
    let info = host.wait_for_info(&chat.id, |info| {
        info.provider == Provider::Muse && info.state == ChatState::Idle
    });
    assert_eq!(
        (info.contributor_mode, info.provider_thread_id.as_deref()),
        (On, Some("sess-2"))
    );
    eventually(|| fake.received_method("model/list").len() == 6);
    assert_eq!(
        serve_argv(&fake),
        [
            serve(On),
            serve(Off),
            serve(On),
            serve(Off),
            serve(On),
            serve(On)
        ]
    );
    drop(host);
    assert!(!fake.saw("mismatch"), "{:#?}", fake.entries());
}

#[test]
fn a_contributor_change_either_way_waits_for_a_muse_turn_and_one_muse_cannot_start_with_is_rolled_back()
 {
    use crate::chat::testkit::{Fake, fixture};
    use ContributorMode::{Off, On};
    for (from, to) in [(Off, On), (On, Off)] {
        let fake = Fake::new(&[
            &fixture("muse/contributor_busy.ndjson"),
            &fixture("muse/contributor_refused.ndjson"),
            &fixture("muse/contributor_resume_turn.ndjson"),
        ]);
        let (host, chat) = muse_host_and_chat(&fake, from);
        let mut client = host.client();
        send(&mut client, &chat.id, "hello");
        host.wait_for_log(&chat.id, |log| {
            log.iter().any(|e| {
                matches!(
                    &e.event,
                    ChatEvent::ItemStarted { item } if matches!(item.body, ItemBody::AgentMessage { .. })
                )
            })
        });
        let error = client
            .command(&chat.id, configure_contributor(to))
            .unwrap_err();
        assert!(error.contains("Wait for the turn to finish"), "{error}");
        assert_eq!(host.info(&chat.id).contributor_mode, from);
        assert_eq!(fake.starts().len(), 1, "the running turn's Muse goes on");
        client.command(&chat.id, ChatCommand::Interrupt).unwrap();
        host.wait_for_log(&chat.id, |log| {
            turns_completed(log) == 1 && last_state(log) == Some(ChatState::Idle)
        });

        // This Muse does not start with it: the chat fails as any failed start does, keeps the
        // mode its next start runs with, and says so.
        let error = client
            .command(&chat.id, configure_contributor(to))
            .unwrap_err();
        assert!(
            error.contains("Contributor mode was not changed")
                && error.contains("unexpected argument '--contributor-mode'")
                && error.contains(&format!("keeps contributor mode {}", from.as_str())),
            "{error}"
        );
        let info = host.info(&chat.id);
        assert_eq!(info.contributor_mode, from);
        assert_eq!(info.contributor_opt_out, from == Off);
        assert_eq!(saved_info(&host, &chat.id).contributor_mode, from);
        assert!(
            matches!(info.state, ChatState::Failed { .. }),
            "{:?}",
            info.state
        );
        assert!(
            notices(&host.log(&chat.id))
                .iter()
                .any(|notice| notice.contains("Contributor mode was not changed")),
            "{:?}",
            notices(&host.log(&chat.id))
        );
        send(&mut client, &chat.id, "again");
        host.wait_for_log(&chat.id, |log| {
            turns_completed(log) == 2 && last_state(log) == Some(ChatState::Idle)
        });
        assert_eq!(serve_argv(&fake), [serve(from), serve(to), serve(from)]);
        assert_eq!(
            fake.received_method("session/resume")[0]["params"]["sessionId"],
            "sess-1"
        );
        drop(host);
        assert!(!fake.saw("mismatch"), "{:#?}", fake.entries());
    }
}

#[test]
fn a_switch_goes_on_with_the_other_provider_in_the_same_chat_and_carries_the_conversation() {
    let host = TestHost::new();
    let chat = host.create(Provider::Codex);
    let mut client = host.client();
    send(&mut client, &chat.id, "remember the blue door");
    host.wait_for_log(&chat.id, |log| {
        turns_completed(log) == 1 && last_state(log) == Some(ChatState::Idle)
    });
    host.fake().emit(ChatEvent::Models {
        models: vec![ModelOption {
            id: "gpt-5.5".into(),
            name: "GPT-5.5".into(),
            ..ModelOption::default()
        }],
    });
    host.fake().emit(ChatEvent::Usage {
        usage: crate::chat::model::Usage {
            input_tokens: 10,
            ..Default::default()
        },
    });
    host.wait_for_log(&chat.id, |log| {
        log.iter()
            .any(|e| matches!(e.event, ChatEvent::Usage { .. }))
    });

    client
        .command(&chat.id, switch(Provider::Claude, Some("opus")))
        .unwrap();
    let info = host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
    // The same chat, now Claude's, on a thread of its own.
    assert_eq!(info.id, chat.id);
    assert_eq!(info.provider, Provider::Claude);
    assert_eq!(info.model.as_deref(), Some("opus"));
    assert_eq!(info.provider_thread_id.as_deref(), Some("thread-2"));
    assert_eq!(info.codex_account_id, None, "Claude has no Codex account");
    assert_eq!(info.title, "Claude chat", "a default title follows");
    assert_eq!(info.approval_mode, chat.approval_mode);
    let carried = info
        .carried_over
        .clone()
        .expect("the conversation is carried");
    assert!(
        carried.from.starts_with("Codex chat \"Codex chat\""),
        "{}",
        carried.from
    );
    assert_eq!(
        carried.document,
        host.home.join("chats").join(&chat.id).join("context.md")
    );
    assert_eq!(mode(&carried.document), 0o600);
    let document = fs::read_to_string(&carried.document).unwrap();
    assert!(document.contains("remember the blue door"), "{document}");
    assert!(
        document.contains("echo: remember the blue door"),
        "{document}"
    );

    // The old agent was stopped and the new one started fresh, told the conversation.
    assert_eq!(host.fake().shutdowns.load(Ordering::SeqCst), 1);
    let starts = host.fake().starts.lock().unwrap().clone();
    assert_eq!(starts.len(), 2);
    assert_eq!(starts[1].provider, Provider::Claude);
    assert_eq!(starts[1].resume, None);
    assert_eq!(starts[1].model.as_deref(), Some("opus"));
    let told = starts[1].instructions.clone().unwrap();
    assert!(told.contains("Codex chat"), "{told}");
    assert!(told.contains("<handoff>"), "{told}");
    assert!(told.contains("echo: remember the blue door"), "{told}");
    assert!(
        starts[0].instructions.is_none(),
        "a chat that never switched tells nothing"
    );

    // The log says so: an Info with the new provider, then a notice for every client.
    let log = host.log(&chat.id);
    assert_gapless(&log, 1);
    assert_eq!(
        notices(&log),
        ["Continued with Claude (opus), which has the conversation so far."]
    );
    let mut transcript = Transcript::default();
    for envelope in &log {
        transcript.apply(&envelope.event);
    }
    assert_eq!(
        transcript.info.as_ref().map(|info| info.provider),
        Some(Provider::Claude)
    );
    assert!(transcript.models.is_empty(), "Codex's models are gone");
    assert_eq!(transcript.usage, None, "and so is its usage");
    assert!(transcript.items.iter().any(|item| matches!(
        &item.body,
        ItemBody::UserMessage { text } if text == "remember the blue door"
    )));

    // Messages go to the new agent.
    send(&mut client, &chat.id, "which door?");
    host.wait_for_log(&chat.id, |log| turns_completed(log) == 2);
    assert_eq!(host.fake().start_count(), 2);
}

#[test]
fn a_switch_is_refused_while_a_turn_runs_and_changes_nothing() {
    let host = TestHost::new();
    let chat = host.create(Provider::Codex);
    let mut client = host.client();
    send(&mut client, &chat.id, "hang");
    // The turn's last event: the agent's message that never ends.
    host.wait_for_log(&chat.id, |log| {
        log.iter().any(|e| {
            matches!(
                &e.event,
                ChatEvent::ItemStarted { item } if matches!(item.body, ItemBody::AgentMessage { .. })
            )
        })
    });
    let before = host.log(&chat.id);
    let error = client
        .command(&chat.id, switch(Provider::Claude, None))
        .unwrap_err();
    assert!(error.contains("Wait for the turn to finish"), "{error}");
    let info = host.info(&chat.id);
    assert_eq!(info.provider, Provider::Codex);
    assert_eq!(info.carried_over, None);
    assert_eq!(host.fake().start_count(), 1);
    assert_eq!(host.fake().shutdowns.load(Ordering::SeqCst), 0);
    assert_eq!(host.log(&chat.id), before);
    assert!(
        !host
            .home
            .join("chats")
            .join(&chat.id)
            .join("context.md")
            .exists()
    );
}

#[test]
fn only_an_untitled_ordinary_chat_takes_its_first_message_as_title_and_only_once() {
    let info = |extra: serde_json::Value| {
        let mut v = serde_json::json!({"id":uuid::Uuid::new_v4().to_string(),"provider":"codex","cwd":"/tmp","title":"Codex chat","created_at_unix":1});
        v.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::from_value::<ChatInfo>(v).unwrap()
    };
    assert!(needs_auto_title(&info(serde_json::json!({}))));
    // Once named, a resume, restart or reread keeps it: the next message names nothing.
    assert!(!needs_auto_title(&info(
        serde_json::json!({"first_user_message":"First"})
    )));
    assert!(!needs_auto_title(&info(
        serde_json::json!({"user_title":"Mine"})
    )));
    assert!(!needs_auto_title(&info(
        serde_json::json!({"provider_title":"Thread"})
    )));
    assert!(!needs_auto_title(&info(
        serde_json::json!({"orchestrator":{"scope":"global"}})
    )));
}

#[test]
fn a_switch_to_the_chats_own_provider_is_an_ordinary_change() {
    let host = TestHost::new();
    let chat = host.create(Provider::Claude);
    host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
    let mut client = host.client();
    client
        .command(&chat.id, switch(Provider::Claude, Some("sonnet")))
        .unwrap();
    let info = host.wait_for_info(&chat.id, |info| info.model.as_deref() == Some("sonnet"));
    assert_eq!(info.provider, Provider::Claude);
    assert_eq!(info.carried_over, None);
    assert_eq!(host.fake().start_count(), 1, "the agent keeps running");
    assert!(notices(&host.log(&chat.id)).is_empty());
}

#[test]
fn a_switched_chat_resumes_on_its_new_provider_and_is_told_the_conversation_again() {
    let mut host = TestHost::new();
    let chat = host.create(Provider::Claude);
    let mut client = host.client();
    send(&mut client, &chat.id, "one");
    host.wait_for_log(&chat.id, |log| {
        turns_completed(log) == 1 && last_state(log) == Some(ChatState::Idle)
    });
    client
        .command(&chat.id, switch(Provider::Codex, None))
        .unwrap();
    let switched = host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
    assert_eq!(switched.provider, Provider::Codex);
    assert_eq!(switched.model, None, "the provider's own default");
    assert_eq!(
        switched.codex_account_id.as_deref(),
        Some("account-a"),
        "the project's or the app's Codex account"
    );
    assert_eq!(
        notices(&host.log(&chat.id)),
        ["Continued with Codex, which has the conversation so far."]
    );
    send(&mut client, &chat.id, "two");
    host.wait_for_log(&chat.id, |log| {
        turns_completed(log) == 2 && last_state(log) == Some(ChatState::Idle)
    });
    drop(client);

    host.restart(quick_options());
    let info = host.info(&chat.id);
    assert_eq!(info.state, ChatState::Stopped);
    assert_eq!(info.provider, Provider::Codex);
    let thread = info.provider_thread_id.clone().unwrap();
    let mut client = host.client();
    send(&mut client, &chat.id, "three");
    let starts = host.fake().starts.lock().unwrap().clone();
    let last = starts.last().unwrap();
    assert_eq!(last.provider, Provider::Codex);
    assert_eq!(last.resume.as_deref(), Some(thread.as_str()));
    let told = last.instructions.clone().unwrap();
    assert!(told.contains("Claude chat"), "{told}");
    assert!(told.contains("echo: one"), "{told}");

    // And back: the second switch carries everything, both agents' parts.
    host.wait_for_log(&chat.id, |log| {
        turns_completed(log) == 3 && last_state(log) == Some(ChatState::Idle)
    });
    client
        .command(&chat.id, switch(Provider::Claude, None))
        .unwrap();
    let back = host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
    assert_eq!(back.provider, Provider::Claude);
    let document = fs::read_to_string(back.carried_over.unwrap().document).unwrap();
    for said in ["echo: one", "echo: two", "echo: three"] {
        assert!(document.contains(said), "{said}: {document}");
    }
}

#[test]
fn switching_retires_notices_limits_and_login_scope_even_when_start_fails() {
    use crate::chat::{
        account_identity::{self, Identity},
        model::RateWindow,
    };
    for stage in ["config-fails", "driver-fails", "ready"] {
        for (from, to) in [
            (Provider::Claude, Provider::Codex),
            (Provider::Codex, Provider::Claude),
        ] {
            let mut host = NoticeHost::new();
            let chat = host.create(from);
            let reset = super::super::notice_dismissals::now() + 3600;
            // Poison the combination of the NEW provider and OLD login with a dismissal.
            // Reusing that login during a failed start would incorrectly hide its banner.
            let other = host.create(to);
            host.emit(
                &other,
                dismissal_notice("other", "rate_limit:weekly", Some(reset)),
            );
            host.dismiss(&other, "other");
            for (id, kind) in [
                ("limit", "rate_limit:weekly"),
                ("auth", "auth_required"),
                ("retry", "api_retry"),
            ] {
                let mut item = dismissal_notice(id, kind, Some(reset));
                if let ItemBody::Notice { level, .. } = &mut item.body {
                    *level = NoticeLevel::Error;
                }
                host.emit(&chat, item);
            }
            host.emit(
                &chat,
                dismissal_notice("dismissed", "rate_limit:daily", Some(reset)),
            );
            host.dismiss(&chat, "dismissed");
            lock(&chat.inner).take_driver_event(ChatEvent::RateLimits {
                windows: vec![RateWindow {
                    id: "weekly".into(),
                    label: "old weekly".into(),
                    used_percent: 100.,
                    resets_at: Some(reset),
                    warn_at: 70.,
                }],
            });
            if stage == "config-fails" {
                host.shared.providers.config =
                    |_, _, _| Err("fixture configuration refused".into());
            } else if stage == "driver-fails" {
                *fake_for(&host.home).fail_start.lock().unwrap() =
                    Some("fixture start refused".into());
            }
            super::switch(&host.shared, &chat, to, None, None, None).unwrap();
            if stage == "ready" {
                stop_locked(&chat);
            }
            let inner = lock(&chat.inner);
            assert_eq!(inner.info.provider, to);
            if stage != "ready" {
                assert!(matches!(inner.info.state, ChatState::Failed { .. }));
            }
            assert!(
                inner.provider_account_identity.is_none(),
                "{stage}: no old login in memory"
            );
            assert!(
                account_identity::read(&chat.dir).is_none(),
                "{stage}: no old login on disk"
            );
            drop(inner);
            let saved: serde_json::Value =
                serde_json::from_slice(&fs::read(chat.dir.join("account-identity.json")).unwrap())
                    .unwrap();
            assert_eq!(
                saved["known"],
                serde_json::json!([]),
                "old binding cannot seed new-provider migrations"
            );
            let t = log::read_notice_transcript(&chat.dir).unwrap();
            for id in ["limit", "auth", "retry", "dismissed"] {
                assert!(
                    matches!(
                        t.item(id).unwrap().body,
                        ItemBody::Notice { resolved: true, .. }
                    ),
                    "{stage}: {id}"
                );
            }
            assert!(outstanding_sticky_notices(&t).is_empty());
            if stage == "ready" {
                let fake = fake_for(&host.home);
                let starts = fake.starts.lock().unwrap();
                assert!(
                    starts.last().unwrap().outstanding_notices.is_empty(),
                    "no old banners handed to new driver"
                );
            }
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
                    .any(|event| matches!(event, ChatEvent::RateLimits { .. }))
            );
            host.emit(
                &chat,
                dismissal_notice("new-limit", "rate_limit:weekly", Some(reset)),
            );
            assert!(
                !host.dismissed(&chat, "new-limit"),
                "{stage}: old login's dismissal cannot match"
            );
            lock(&chat.inner)
                .set_account_identity(Some(Identity::fixture(account_identity::hash("new-login"))));
            assert!(!host.dismissed(&chat, "new-limit"));
            host.dismiss(&chat, "new-limit");
            assert!(
                host.dismissed(&chat, "new-limit"),
                "new login can dismiss its own banner"
            );
        }
    }
}

// ---- Resume and effective configuration contract ------------------------------------

fn choose(model: Option<&str>, effort: Option<&str>, mode: Option<ApprovalMode>) -> ChatCommand {
    ChatCommand::Configure {
        model: model.map(str::to_owned),
        effort: effort.map(str::to_owned),
        approval_mode: mode,
        fast: None,
        contributor_mode: None,
        contributor_opt_out: None,
        claude_billing: None,
    }
}

fn configure_reply(host: &TestHost, index: usize) -> DriverReporter {
    host.fake().configurations.lock().unwrap()[index].1.clone()
}

fn saved_info(host: &TestHost, id: &str) -> ChatInfo {
    serde_json::from_slice(&fs::read(host.home.join("chats").join(id).join("info.json")).unwrap())
        .unwrap()
}

#[test]
fn missing_resume_preserves_used_sessions_and_allows_never_used_sessions_for_every_provider() {
    for provider in [Provider::Codex, Provider::Claude, Provider::Muse] {
        for used in [false, true] {
            let mut host = TestHost::new();
            let mut new = host.new_chat(provider);
            new.project_id = Some(PROJECT.into());
            let mut parent = host.new_chat(provider);
            parent.project_id = Some(PROJECT.into());
            parent.cwd = host.home.join("parent");
            fs::create_dir_all(&parent.cwd).unwrap();
            new.parent_id = Some(host.client().create(parent).unwrap().id);
            new.worktree_id = Some("33333333-3333-4333-8333-333333333333".into());
            new.title = Some("Keep this conversation".into());
            new.approval_mode = ApprovalMode::Plan;
            let chat = host.client().create(new).unwrap();
            host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
            if used {
                send(&mut host.client(), &chat.id, "remember this 🦀");
                host.wait_for_log(&chat.id, |log| {
                    turns_completed(log) == 1 && last_state(log) == Some(ChatState::Idle)
                });
            }
            host.client().close(&chat.id).unwrap();
            let before = host.info(&chat.id);
            assert!(
                before.parent_id.is_some(),
                "same-project parent remains attached"
            );
            let prefix = host.log(&chat.id);
            let tabs_path = host
                .home
                .join("project-tabs")
                .join(format!("{PROJECT}.json"));
            let tabs = fs::read(&tabs_path).unwrap();
            let draft = host.home.join("chat-drafts").join(&chat.id);
            fs::create_dir_all(draft.parent().unwrap()).unwrap();
            fs::write(&draft, "  my unsent draft 🦀\n").unwrap();
            // Reloading must derive history for this session, not just this run.
            host.restart(quick_options());
            host.fake().resume_missing.store(true, Ordering::SeqCst);
            let result = host.client().command(
                &chat.id,
                ChatCommand::Send {
                    text: "  my unsent draft 🦀\n".into(),
                },
            );
            let config = host.fake().starts.lock().unwrap().last().unwrap().clone();
            assert_eq!(config.resume_has_history, used, "{provider:?}");
            assert_eq!(config.resume, before.provider_thread_id);
            if used {
                assert!(result.unwrap_err().contains("Historical session"));
                let after = host.info(&chat.id);
                assert!(matches!(after.state, ChatState::Failed { .. }));
                let mut comparable = after.clone();
                comparable.state = before.state.clone();
                assert_eq!(comparable, before, "metadata and original identity survive");
                assert_eq!(saved_info(&host, &chat.id), after);
                assert_eq!(&host.log(&chat.id)[..prefix.len()], prefix.as_slice());
                assert_eq!(
                    host.fake().commands().len(),
                    1,
                    "failed Send never reaches the driver, so the caller keeps its draft"
                );
            } else {
                result.unwrap();
                let after = host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
                assert_ne!(
                    after.provider_thread_id, before.provider_thread_id,
                    "unused missing sessions may start fresh"
                );
                assert_eq!(after.approval_mode, before.approval_mode);
            }
            assert_eq!(
                fs::read_to_string(&draft).unwrap(),
                "  my unsent draft 🦀\n"
            );
            assert_eq!(fs::read(&tabs_path).unwrap(), tabs);
            assert_eq!(
                host.info(&chat.id).id,
                chat.id,
                "RiWork session identity remains stable"
            );
        }
    }
}

#[test]
fn a_guarded_start_cannot_publish_a_replacement_even_if_the_adapter_ignores_the_guard() {
    for provider in [Provider::Codex, Provider::Claude, Provider::Muse] {
        let host = TestHost::new();
        let chat = host.create(provider);
        send(&mut host.client(), &chat.id, "saved history");
        host.wait_for_log(&chat.id, |log| {
            turns_completed(log) == 1 && last_state(log) == Some(ChatState::Idle)
        });
        host.client().close(&chat.id).unwrap();
        let original = host.info(&chat.id).provider_thread_id;
        host.fake().resume_missing.store(true, Ordering::SeqCst);
        host.fake()
            .ignore_resume_guard
            .store(true, Ordering::SeqCst);
        let error = host
            .client()
            .command(&chat.id, choose(None, None, None))
            .unwrap_err();
        assert!(
            error.contains("did not resume historical session"),
            "{error}"
        );
        let after = host.info(&chat.id);
        assert_eq!(after.provider_thread_id, original);
        assert_eq!(saved_info(&host, &chat.id).provider_thread_id, original);
        assert!(host.log(&chat.id).iter().all(|e| !matches!(&e.event, ChatEvent::Info { info } if info.provider_thread_id != original && info.provider_thread_id.is_some())));
        eventually(|| host.fake().shutdowns.load(Ordering::SeqCst) == 2);
    }
}

#[test]
fn reports_from_a_start_that_fails_do_not_change_identity_or_settings() {
    let host = TestHost::new();
    let chat = host.create(Provider::Claude);
    host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
    host.client().close(&chat.id).unwrap();
    let before = host.info(&chat.id);
    *host.fake().startup_report.lock().unwrap() = Some(DriverReport {
        provider_thread_id: Some("replacement-never-attached".into()),
        model: Some(Some("unaccepted-model".into())),
        effort: Some(Some("max".into())),
        approval_mode: Some(ApprovalMode::Full),
        fast: Some(true),
    });
    *host.fake().fail_start.lock().unwrap() = Some("guarded resume not found".into());
    assert!(
        host.client()
            .command(&chat.id, choose(None, None, None))
            .is_err()
    );
    let mut after = host.info(&chat.id);
    assert!(matches!(after.state, ChatState::Failed { .. }));
    after.state = before.state.clone();
    assert_eq!(after, before);
}

#[test]
fn history_guard_uses_the_current_provider_session_after_switching_and_switching_back() {
    let host = TestHost::new();
    let chat = host.create(Provider::Codex);
    send(&mut host.client(), &chat.id, "Codex history");
    host.wait_for_log(&chat.id, |log| {
        turns_completed(log) == 1 && last_state(log) == Some(ChatState::Idle)
    });
    for provider in [Provider::Claude, Provider::Muse, Provider::Codex] {
        host.client()
            .command(&chat.id, switch(provider, None))
            .unwrap();
        host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
        let old_id = host.info(&chat.id).provider_thread_id;
        host.client().close(&chat.id).unwrap();
        host.fake().resume_missing.store(true, Ordering::SeqCst);
        host.client()
            .command(&chat.id, choose(None, None, None))
            .unwrap();
        host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
        assert!(
            !host
                .fake()
                .starts
                .lock()
                .unwrap()
                .last()
                .unwrap()
                .resume_has_history
        );
        assert_ne!(host.info(&chat.id).provider_thread_id, old_id);
        let document = host.info(&chat.id).carried_over.unwrap().document;
        assert!(
            fs::read_to_string(document)
                .unwrap()
                .contains("echo: Codex history")
        );
    }
    assert_eq!(turns_completed(&host.log(&chat.id)), 1);
}

#[test]
fn legacy_history_outside_the_notice_tail_still_guards_resume_and_bad_history_fails_closed() {
    for corrupt in [false, true] {
        let mut host = TestHost::new();
        let chat = host.create(Provider::Muse);
        host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
        host.client().close(&chat.id).unwrap();
        host.stop();
        let dir = host.home.join("chats").join(&chat.id);
        let events = [
            ChatEvent::ItemCompleted {
                item: Item {
                    id: "legacy-user".into(),
                    turn_id: None,
                    status: ItemStatus::Completed,
                    body: ItemBody::UserMessage {
                        text: "old user message".into(),
                    },
                    presentation: Default::default(),
                },
            },
            ChatEvent::ItemCompleted {
                item: Item {
                    id: "long-notice".into(),
                    turn_id: None,
                    status: ItemStatus::Completed,
                    body: ItemBody::notice(
                        NoticeLevel::Info,
                        "n".repeat(log::NOTICE_TAIL as usize + 100),
                        None,
                    ),
                    presentation: Default::default(),
                },
            },
        ];
        let mut bytes = Vec::new();
        for (i, event) in events.into_iter().enumerate() {
            if corrupt && i == 1 {
                bytes.extend_from_slice(b"{\"seq\":2,\"unknown\":true}\n");
            }
            let seq = i as u64 + 1 + u64::from(corrupt && i == 1);
            serde_json::to_writer(
                &mut bytes,
                &Envelope {
                    chat_id: chat.id.clone(),
                    seq,
                    event,
                },
            )
            .unwrap();
            bytes.push(b'\n');
        }
        fs::write(dir.join("events.jsonl"), &bytes).unwrap();
        host.restart(quick_options());
        host.fake().resume_missing.store(true, Ordering::SeqCst);
        let count = host.fake().start_count();
        assert!(
            host.client()
                .command(&chat.id, choose(None, None, None))
                .is_err()
        );
        assert_eq!(
            host.info(&chat.id).provider_thread_id,
            chat.provider_thread_id
        );
        if corrupt {
            assert_eq!(
                host.fake().start_count(),
                count,
                "unreadable ownership must not start a provider"
            );
        } else {
            assert!(
                host.fake()
                    .starts
                    .lock()
                    .unwrap()
                    .last()
                    .unwrap()
                    .resume_has_history
            );
            assert_eq!(
                &fs::read(dir.join("events.jsonl")).unwrap()[..bytes.len()],
                bytes.as_slice()
            );
        }
    }
}

#[test]
fn unconfirmed_or_rejected_configuration_does_not_persist_the_requested_values() {
    for provider in [Provider::Codex, Provider::Claude, Provider::Muse] {
        let host = TestHost::new();
        let mut new = host.new_chat(provider);
        new.model = Some("working-model".into());
        new.effort = Some("low".into());
        let chat = host.client().create(new).unwrap();
        host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
        host.fake().defer_configure.store(true, Ordering::SeqCst);
        let before = host.info(&chat.id);
        host.client()
            .command(
                &chat.id,
                ChatCommand::Configure {
                    model: Some("refused-model".into()),
                    effort: Some("refused-effort".into()),
                    approval_mode: Some(ApprovalMode::Full),
                    fast: (provider != Provider::Muse).then_some(true),
                    contributor_mode: None,
                    contributor_opt_out: None,
                    claude_billing: None,
                },
            )
            .unwrap();
        assert_eq!(
            host.info(&chat.id),
            before,
            "Ok accepts delivery, not provider settings"
        );
        configure_reply(&host, 0).report(DriverReport {
            model: Some(before.model.clone()),
            effort: Some(before.effort.clone()),
            approval_mode: Some(before.approval_mode),
            fast: Some(before.fast),
            ..Default::default()
        });
        assert_eq!(saved_info(&host, &chat.id), before);
        assert!(host.log(&chat.id).iter().all(|e| !matches!(&e.event, ChatEvent::Info { info } if info.model.as_deref() == Some("refused-model"))));
        // A supported model and an unsupported effort in one Configure can
        // settle independently, preserving the real retained effort.
        host.client()
            .command(
                &chat.id,
                choose(Some("accepted-model"), Some("bad-effort"), None),
            )
            .unwrap();
        configure_reply(&host, 1).report(DriverReport {
            model: Some(Some("accepted-model".into())),
            effort: Some(Some("low".into())),
            ..Default::default()
        });
        assert_eq!(host.info(&chat.id).model.as_deref(), Some("accepted-model"));
        assert_eq!(host.info(&chat.id).effort.as_deref(), Some("low"));
        host.client().close(&chat.id).unwrap();
        host.client()
            .command(&chat.id, choose(None, None, None))
            .unwrap();
        let config = host.fake().starts.lock().unwrap().last().unwrap().clone();
        assert_eq!(config.model.as_deref(), Some("accepted-model"));
        assert_eq!(config.effort.as_deref(), Some("low"));
        assert_eq!(config.approval_mode, ApprovalMode::Supervised);
        assert!(!config.fast);
    }
}

#[test]
fn delayed_configuration_results_do_not_overwrite_newer_choices_but_independent_fields_can_settle()
{
    let mut host = TestHost::new();
    let chat = host.create(Provider::Codex);
    host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
    host.fake().defer_configure.store(true, Ordering::SeqCst);
    host.client()
        .command(
            &chat.id,
            choose(Some("older"), Some("high"), Some(ApprovalMode::AutoEdit)),
        )
        .unwrap();
    let older = configure_reply(&host, 0);
    host.client()
        .command(&chat.id, choose(Some("newer"), None, None))
        .unwrap();
    configure_reply(&host, 1).report(DriverReport {
        model: Some(Some("newer".into())),
        ..Default::default()
    });
    older.report(DriverReport {
        provider_thread_id: Some("obsolete-id".into()),
        model: Some(Some("older".into())),
        effort: Some(Some("high".into())),
        approval_mode: Some(ApprovalMode::AutoEdit),
        ..Default::default()
    });
    let info = host.info(&chat.id);
    assert_eq!(info.model.as_deref(), Some("newer"));
    assert_eq!(info.effort.as_deref(), Some("high"));
    assert_eq!(info.approval_mode, ApprovalMode::AutoEdit);
    assert_eq!(info.provider_thread_id, chat.provider_thread_id);
    // Clearing to a default is distinct from an omitted report field.
    host.client()
        .command(&chat.id, choose(Some("default"), Some("default"), None))
        .unwrap();
    configure_reply(&host, 2).report(DriverReport {
        model: Some(None),
        effort: Some(None),
        ..Default::default()
    });
    older.report(DriverReport {
        model: Some(Some("older".into())),
        effort: Some(Some("high".into())),
        ..Default::default()
    });
    assert_eq!(host.info(&chat.id).model, None);
    assert_eq!(host.info(&chat.id).effort, None);
    host.restart(quick_options());
    assert_eq!(host.info(&chat.id).model, None);
    assert_eq!(host.info(&chat.id).effort, None);
}

#[test]
fn a_local_configuration_refusal_does_not_supersede_an_earlier_pending_choice() {
    let host = TestHost::new();
    let chat = host.create(Provider::Codex);
    host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
    host.fake().defer_configure.store(true, Ordering::SeqCst);
    host.client()
        .command(&chat.id, choose(Some("pending"), None, None))
        .unwrap();
    let pending = configure_reply(&host, 0);
    *host.fake().fail_configure.lock().unwrap() = Some("invalid local choice".into());
    assert!(
        host.client()
            .command(&chat.id, choose(Some("invalid"), None, None))
            .is_err()
    );
    pending.report(DriverReport {
        model: Some(Some("pending".into())),
        ..Default::default()
    });
    assert_eq!(host.info(&chat.id).model.as_deref(), Some("pending"));
    configure_reply(&host, 1).report(DriverReport {
        model: Some(Some("invalid".into())),
        ..Default::default()
    });
    assert_eq!(host.info(&chat.id).model.as_deref(), Some("pending"));
}

#[test]
fn provider_observations_supersede_older_acknowledgements_and_reach_existing_info_only() {
    for provider in [Provider::Codex, Provider::Claude, Provider::Muse] {
        let host = TestHost::new();
        let mut new = host.new_chat(provider);
        new.title = Some("My named session".into());
        new.project_id = Some(PROJECT.into());
        new.worktree_id = Some("33333333-3333-4333-8333-333333333333".into());
        new.model = Some("chosen".into());
        new.effort = Some("low".into());
        let chat = host.client().create(new).unwrap();
        host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
        let before = host.info(&chat.id);
        let run_reporter = host.fake().starts.lock().unwrap()[0].reporter.clone();
        host.fake().defer_configure.store(true, Ordering::SeqCst);
        let pending_mode = if provider == Provider::Muse {
            ApprovalMode::Full
        } else {
            ApprovalMode::Plan
        };
        host.client()
            .command(&chat.id, choose(Some("pending"), None, Some(pending_mode)))
            .unwrap();
        let last_seq = host.log(&chat.id).last().unwrap().seq;
        let follower = Follower::open(&host.socket(), &chat.id, last_seq).unwrap();
        run_reporter.report(DriverReport {
            provider_thread_id: Some("provider-adopted-id".into()),
            model: Some(Some("real-model".into())),
            effort: Some(Some("high".into())),
            approval_mode: Some(ApprovalMode::AutoEdit),
            ..Default::default()
        });
        let live = follower.next().unwrap();
        let expected = ChatInfo {
            provider_thread_id: Some("provider-adopted-id".into()),
            model: Some("real-model".into()),
            effort: Some("high".into()),
            approval_mode: ApprovalMode::AutoEdit,
            ..before.clone()
        };
        assert_eq!(host.info(&chat.id), expected);
        assert_eq!(saved_info(&host, &chat.id), expected);
        assert_eq!(
            live.event,
            ChatEvent::Info {
                info: expected.clone()
            }
        );
        let shape = serde_json::to_value(&live).unwrap();
        assert_eq!(
            shape
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            ["chat_id", "event", "seq"]
        );
        assert_eq!(
            shape["event"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            ["event", "info"]
        );
        assert_eq!(shape["event"]["event"], "info");
        let disk = fs::read_to_string(host.home.join("chats").join(&chat.id).join("events.jsonl"))
            .unwrap();
        for internal in [
            "reporter",
            "settings_revisions",
            "resume_has_history",
            "generation",
        ] {
            assert!(!disk.contains(internal));
        }
        configure_reply(&host, 0).report(DriverReport {
            provider_thread_id: Some("older-id".into()),
            model: Some(Some("pending".into())),
            approval_mode: Some(pending_mode),
            ..Default::default()
        });
        assert_eq!(host.info(&chat.id), expected);
        assert_eq!(
            host.log(&chat.id).last().unwrap().seq,
            last_seq + 1,
            "stale reports publish nothing"
        );
        host.client().close(&chat.id).unwrap();
        host.client()
            .command(&chat.id, choose(None, None, None))
            .unwrap();
        let config = host.fake().starts.lock().unwrap().last().unwrap().clone();
        assert_eq!(config.resume.as_deref(), Some("provider-adopted-id"));
        assert_eq!(config.approval_mode, ApprovalMode::AutoEdit);
        assert_eq!(config.model.as_deref(), Some("real-model"));
        assert_eq!(config.effort.as_deref(), Some("high"));
        assert_eq!(
            host.fake().commands().len(),
            2,
            "reports never send a permission/configuration command"
        );
    }
}

#[test]
fn stopped_or_replaced_runs_cannot_report_identity_or_settings_into_the_new_provider() {
    let host = TestHost::new();
    let chat = host.create(Provider::Codex);
    host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
    let run = host.fake().starts.lock().unwrap()[0].reporter.clone();
    host.fake().defer_configure.store(true, Ordering::SeqCst);
    host.client()
        .command(&chat.id, choose(Some("old-pending"), None, None))
        .unwrap();
    let pending = configure_reply(&host, 0);
    host.client().close(&chat.id).unwrap();
    let stopped = host.info(&chat.id);
    let stale = DriverReport {
        provider_thread_id: Some("old-session".into()),
        model: Some(Some("old-model".into())),
        effort: Some(Some("max".into())),
        approval_mode: Some(ApprovalMode::Full),
        fast: Some(true),
    };
    run.report(stale.clone());
    pending.report(stale.clone());
    assert_eq!(host.info(&chat.id), stopped);
    host.client()
        .command(&chat.id, switch(Provider::Claude, Some("new-model")))
        .unwrap();
    let current = host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
    let seq = host.log(&chat.id).last().unwrap().seq;
    run.report(stale.clone());
    pending.report(stale);
    assert_eq!(host.info(&chat.id), current);
    assert_eq!(saved_info(&host, &chat.id), current);
    assert_eq!(host.log(&chat.id).last().unwrap().seq, seq);
}

#[test]
fn a_stale_start_failure_cannot_fail_a_newer_run() {
    let host = NoticeHost::new();
    let chat = host.create(Provider::Claude);
    ensure_running(&host.shared, &chat).unwrap();
    let first = lock(&chat.inner).run.as_ref().unwrap().generation;
    stop(&chat);
    ensure_running(&host.shared, &chat).unwrap();
    let before = chat.info();
    assert!(fail_start(&chat, first, "delayed old failure".into()).is_err());
    assert_eq!(chat.info(), before);
    assert_ne!(lock(&chat.inner).run.as_ref().unwrap().generation, first);
    stop(&chat);
}

#[test]
fn stop_cancels_a_blocked_configure_before_waiting_on_lifecycle_or_driver_locks() {
    struct BlockedConfigure {
        entered: mpsc::Sender<()>,
        cancelled: mpsc::Receiver<()>,
        cancel: mpsc::Sender<()>,
    }
    impl Driver for BlockedConfigure {
        fn cancel_io(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
            let cancel = self.cancel.clone();
            Some(Arc::new(move || {
                let _ = cancel.send(());
            }))
        }
        fn command(&mut self, _: ChatCommand) -> Result<(), String> {
            Ok(())
        }
        fn configure(&mut self, _: ChatCommand, reporter: DriverReporter) -> Result<(), String> {
            self.entered.send(()).unwrap();
            self.cancelled
                .recv_timeout(Duration::from_secs(3))
                .map_err(|_| "Stop deadlocked behind Configure".to_owned())?;
            // A cancellation result from the old attempt must not change info.
            reporter.report(DriverReport {
                model: Some(Some("cancelled-choice".into())),
                approval_mode: Some(ApprovalMode::Full),
                ..Default::default()
            });
            Err("configuration cancelled".into())
        }
        fn provider_thread_id(&self) -> Option<String> {
            Some("existing-session".into())
        }
        fn shutdown(&mut self) {}
    }
    let host = Arc::new(NoticeHost::new());
    let chat = host.create(Provider::Claude);
    let (entered, entry) = mpsc::channel();
    let (cancel, cancelled) = mpsc::channel();
    let driver: Box<dyn Driver> = Box::new(BlockedConfigure {
        entered,
        cancelled,
        cancel,
    });
    let cancel = driver.cancel_io();
    {
        let mut inner = lock(&chat.inner);
        inner.generations = 1;
        let mut run = Run::starting(1);
        run.driver = Some(Arc::new(Mutex::new(driver)));
        run.cancel_io = cancel;
        run.reader_done.store(true, Ordering::SeqCst);
        inner.run = Some(run);
        inner.set_state(ChatState::Idle);
    }
    let configuring = {
        let (host, chat) = (host.clone(), chat.clone());
        thread::spawn(move || {
            run_command(
                &host.shared,
                &chat,
                choose(Some("blocked-choice"), None, None),
            )
        })
    };
    entry.recv_timeout(Duration::from_secs(2)).unwrap();
    let start = Instant::now();
    stop(&chat);
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "Stop must cancel without acquiring the blocked locks"
    );
    assert_eq!(
        configuring.join().unwrap().unwrap_err(),
        "configuration cancelled"
    );
    assert_eq!(chat.info().state, ChatState::Stopped);
    assert_eq!(chat.info().model, None);
    assert_eq!(chat.info().approval_mode, ApprovalMode::Supervised);
    assert!(lock(&chat.inner).run.is_none());
}

#[test]
fn a_provider_reported_new_session_has_its_own_history_even_with_an_old_auto_title() {
    let host = TestHost::new();
    let chat = host.create(Provider::Claude);
    send(&mut host.client(), &chat.id, "history before clear");
    host.wait_for_log(&chat.id, |log| {
        turns_completed(log) == 1 && last_state(log) == Some(ChatState::Idle)
    });
    let before = host.info(&chat.id);
    assert!(before.first_user_message.is_some());
    let report = host.fake().starts.lock().unwrap()[0].reporter.clone();
    report.report(DriverReport {
        provider_thread_id: Some("cleared-session".into()),
        ..Default::default()
    });
    host.client().close(&chat.id).unwrap();
    host.fake().resume_missing.store(true, Ordering::SeqCst);
    host.client()
        .command(&chat.id, choose(None, None, None))
        .unwrap();
    let info = host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
    let config = host.fake().starts.lock().unwrap().last().unwrap().clone();
    assert_eq!(config.resume.as_deref(), Some("cleared-session"));
    assert!(
        !config.resume_has_history,
        "the previous session's turns and auto title are not the new session's history"
    );
    assert_ne!(info.provider_thread_id.as_deref(), Some("cleared-session"));
    assert_eq!(info.first_user_message, before.first_user_message);
    assert_eq!(turns_completed(&host.log(&chat.id)), 1);
}

#[test]
fn metadata_only_legacy_sessions_are_unknown_rather_than_proven_never_used() {
    let mut host = TestHost::new();
    let chat = host.create(Provider::Codex);
    host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
    host.client().close(&chat.id).unwrap();
    let before = host.info(&chat.id);
    assert_eq!(before.first_user_message, None);
    host.stop();
    fs::write(
        host.home.join("chats").join(&chat.id).join("events.jsonl"),
        b"",
    )
    .unwrap();
    host.restart(quick_options());
    host.fake().resume_missing.store(true, Ordering::SeqCst);
    assert!(
        host.client()
            .command(&chat.id, choose(None, None, None))
            .is_err()
    );
    assert!(
        host.fake()
            .starts
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .resume_has_history
    );
    assert_eq!(
        host.info(&chat.id).provider_thread_id,
        before.provider_thread_id
    );
}

#[test]
fn identity_adoption_during_an_open_turn_keeps_its_history_but_an_idle_clear_resets_it() {
    let host = NoticeHost::new();
    let chat = host.create(Provider::Claude);
    let mut inner = lock(&chat.inner);
    inner.info.provider_thread_id = Some("initial-session".into());
    inner.publish_info();
    inner.append(ChatEvent::TurnStarted {
        turn_id: "first-turn".into(),
    });
    inner.append(ChatEvent::ItemCompleted {
        item: Item {
            id: "first-input".into(),
            turn_id: Some("first-turn".into()),
            status: ItemStatus::Completed,
            body: ItemBody::UserMessage {
                text: "accepted before system/init".into(),
            },
            presentation: Default::default(),
        },
    });
    inner.info.provider_thread_id = Some("actual-provider-session".into());
    inner.publish_info();
    assert!(
        inner.log.provider_session_has_history(&inner.info).unwrap(),
        "an adopted id does not discard its open turn's accepted input"
    );
    inner.append(ChatEvent::TurnCompleted {
        turn_id: "first-turn".into(),
        outcome: TurnOutcome::Completed,
    });
    inner.info.provider_thread_id = Some("cleared-idle-session".into());
    inner.publish_info();
    assert!(
        !inner.log.provider_session_has_history(&inner.info).unwrap(),
        "a later idle session has not been used"
    );
}

#[test]
fn legacy_running_state_guards_resume_even_when_no_turn_items_were_saved() {
    let mut host = TestHost::new();
    let chat = host.create(Provider::Codex);
    host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
    host.client().close(&chat.id).unwrap();
    let before = host.info(&chat.id);
    assert_eq!(before.first_user_message, None);
    host.stop();
    let events = [
        ChatEvent::Info {
            info: before.clone(),
        },
        ChatEvent::State {
            state: ChatState::Running,
        },
        ChatEvent::State {
            state: ChatState::Stopped,
        },
    ];
    let mut bytes = Vec::new();
    for (i, event) in events.into_iter().enumerate() {
        serde_json::to_writer(
            &mut bytes,
            &Envelope {
                chat_id: chat.id.clone(),
                seq: i as u64 + 1,
                event,
            },
        )
        .unwrap();
        bytes.push(b'\n');
    }
    fs::write(
        host.home.join("chats").join(&chat.id).join("events.jsonl"),
        bytes,
    )
    .unwrap();
    host.restart(quick_options());
    host.fake().resume_missing.store(true, Ordering::SeqCst);
    assert!(
        host.client()
            .command(&chat.id, choose(None, None, None))
            .is_err()
    );
    assert!(
        host.fake()
            .starts
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .resume_has_history
    );
    assert_eq!(
        host.info(&chat.id).provider_thread_id,
        before.provider_thread_id
    );
}

fn configure_billing(billing: ClaudeBilling) -> ChatCommand {
    ChatCommand::Configure {
        model: None,
        effort: None,
        approval_mode: None,
        fast: None,
        contributor_mode: None,
        contributor_opt_out: None,
        claude_billing: Some(billing),
    }
}

fn set_billing_default(host: &TestHost, billing: ClaudeBilling) {
    crate::settings::SettingsStore::open(&host.home)
        .unwrap()
        .update(|settings| settings.claude_chat_billing = billing)
        .unwrap();
}

/// What a start did with `ANTHROPIC_API_KEY`: the key it set, `removed`, or `inherited`.
fn api_key_of(config: &DriverConfig) -> String {
    let set = config
        .env
        .iter()
        .rev()
        .find(|(name, _)| name == "ANTHROPIC_API_KEY")
        .map(|(_, value)| value.to_string_lossy().into_owned());
    let removed = config
        .env_remove
        .iter()
        .any(|name| name == "ANTHROPIC_API_KEY");
    match (set, removed) {
        (Some(key), false) => key,
        (None, true) => "removed".into(),
        (None, false) => "inherited".into(),
        (Some(_), true) => panic!("both set and removed: {config:?}"),
    }
}

const HOST_KEY: &str = "sk-ant-dummy-host";

#[test]
fn a_claude_chat_is_made_with_the_settings_billing_and_keeps_its_own_through_resumes_restarts_and_switches()
 {
    use ClaudeBilling::{ApiKey, Subscription};
    let mut host = TestHost::new();
    keys_for(&host.home).save(HOST_KEY).unwrap();
    set_billing_default(&host, Subscription);
    let chat = host.create(Provider::Claude);
    assert_eq!(chat.claude_billing, Some(Subscription));
    host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
    // A later default leaves the chat as it was made: stopped and resumed, and after the host
    // itself restarted.
    set_billing_default(&host, ApiKey);
    let mut client = host.client();
    client.close(&chat.id).unwrap();
    send(&mut client, &chat.id, "after a stop");
    host.wait_for_log(&chat.id, |log| turns_completed(log) == 1);
    host.restart(quick_options());
    assert_eq!(host.info(&chat.id).claude_billing, Some(Subscription));
    assert_eq!(
        saved_info(&host, &chat.id).claude_billing,
        Some(Subscription)
    );
    send(&mut host.client(), &chat.id, "after a restart");
    host.wait_for_log(&chat.id, |log| {
        turns_completed(log) == 2 && last_state(log) == Some(ChatState::Idle)
    });
    let starts = host.fake().starts.lock().unwrap().clone();
    assert_eq!(starts.len(), 3);
    assert_eq!(starts[2].resume.as_deref(), Some("thread-1"));
    assert!(starts.iter().all(|config| api_key_of(config) == "removed"));
    // It stays with the chat through a switch away, and holds again back on Claude.
    for provider in [Provider::Codex, Provider::Claude] {
        host.client()
            .command(&chat.id, switch(provider, None))
            .unwrap();
        assert_eq!(host.info(&chat.id).claude_billing, Some(Subscription));
    }
    let starts = host.fake().starts.lock().unwrap().clone();
    let codex = &starts[starts.len() - 2];
    assert_eq!(
        (codex.provider, api_key_of(codex).as_str()),
        (Provider::Codex, "inherited")
    );
    let last = starts.last().unwrap();
    assert_eq!(
        (last.provider, last.resume.clone(), api_key_of(last)),
        (Provider::Claude, None, "removed".to_owned())
    );

    // Made on the default API key, a chat's Claude gets the saved key; asked for Subscription,
    // it loses the key. Another provider's chat is made without a billing, whatever it asks.
    for (directory, asked, wanted, key) in [
        ("default", None, ApiKey, HOST_KEY),
        ("asked", Some(Subscription), Subscription, "removed"),
    ] {
        let mut new = host.new_chat(Provider::Claude);
        new.cwd = host.home.join(directory);
        fs::create_dir_all(&new.cwd).unwrap();
        new.claude_billing = asked;
        let created = host.client().create(new).unwrap();
        assert_eq!(created.claude_billing, Some(wanted), "{directory}");
        host.wait_for_state(&created.id, |s| *s == ChatState::Idle);
        assert_eq!(
            api_key_of(&fake_for(&created.cwd).starts.lock().unwrap()[0]),
            key,
            "{directory}"
        );
    }
    for provider in [Provider::Codex, Provider::Muse] {
        let mut new = host.new_chat(provider);
        new.cwd = host.home.join(format!("{provider:?}"));
        fs::create_dir_all(&new.cwd).unwrap();
        new.claude_billing = Some(Subscription);
        let created = host.client().create(new).unwrap();
        assert_eq!(created.claude_billing, None, "{provider:?}");
        host.wait_for_state(&created.id, |s| *s == ChatState::Idle);
        assert_eq!(
            api_key_of(&fake_for(&created.cwd).starts.lock().unwrap()[0]),
            "inherited"
        );
    }
}

#[test]
fn a_claude_chat_saved_before_chats_had_a_billing_follows_the_settings_default_at_each_start() {
    use ClaudeBilling::{ApiKey, Subscription};
    let mut host = TestHost::new();
    keys_for(&host.home).save(HOST_KEY).unwrap();
    let chat = host.create(Provider::Claude);
    host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
    host.stop();
    // What hosts before the choice wrote: no `claude_billing`, in the info and the log.
    let dir = host.home.join("chats").join(&chat.id);
    let path = dir.join("info.json");
    let mut info: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    info.as_object_mut().unwrap().remove("claude_billing");
    fs::write(&path, info.to_string()).unwrap();
    let log = dir.join("events.jsonl");
    let lines: Vec<String> = fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(|line| {
            let mut envelope: serde_json::Value = serde_json::from_str(line).unwrap();
            if envelope["event"]["event"] == "info" {
                envelope["event"]["info"]
                    .as_object_mut()
                    .unwrap()
                    .remove("claude_billing");
            }
            envelope.to_string()
        })
        .collect();
    fs::write(&log, lines.join("\n") + "\n").unwrap();
    host.restart(quick_options());
    assert_eq!(host.info(&chat.id).claude_billing, None);
    // Each start bills the default of the moment, as every Claude chat did before.
    for (turn, (default, key)) in [(Subscription, "removed"), (ApiKey, HOST_KEY)]
        .into_iter()
        .enumerate()
    {
        set_billing_default(&host, default);
        let mut client = host.client();
        client.close(&chat.id).unwrap();
        send(&mut client, &chat.id, "next");
        host.wait_for_log(&chat.id, |log| {
            turns_completed(log) == turn + 1 && last_state(log) == Some(ChatState::Idle)
        });
        let last = host.fake().starts.lock().unwrap().last().cloned().unwrap();
        assert_eq!(api_key_of(&last), key, "{default:?}");
        assert_eq!(last.resume.as_deref(), Some("thread-1"));
    }
    // Chosen once, it is the chat's own.
    let mut client = host.client();
    client.close(&chat.id).unwrap();
    client
        .command(&chat.id, configure_billing(Subscription))
        .unwrap();
    assert_eq!(
        saved_info(&host, &chat.id).claude_billing,
        Some(Subscription)
    );
}

#[test]
fn a_billing_change_waits_for_a_quiet_claude_chat_restarts_it_on_its_session_and_a_stopped_one_keeps_it()
 {
    use ClaudeBilling::{ApiKey, Subscription};
    for (from, to) in [(Subscription, ApiKey), (ApiKey, Subscription)] {
        let host = TestHost::new();
        keys_for(&host.home).save(HOST_KEY).unwrap();
        let key = |billing| match billing {
            Subscription => "removed",
            ApiKey => HOST_KEY,
        };
        let mut new = host.new_chat(Provider::Claude);
        new.claude_billing = Some(from);
        let chat = host.client().create(new).unwrap();
        host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
        let mut client = host.client();
        let refused = |client: &mut Client, command: ChatCommand, reason: &str| {
            let error = client.command(&chat.id, command).unwrap_err();
            assert!(error.contains(reason), "{from:?} to {to:?}: {error}");
            assert_eq!(host.info(&chat.id).claude_billing, Some(from));
        };
        // A change comes alone: the Claude started again for it could refuse the rest.
        let mut with_effort = configure_billing(to);
        if let ChatCommand::Configure { effort, .. } = &mut with_effort {
            *effort = Some("low".into());
        }
        refused(&mut client, with_effort, "on its own");
        // Asking for what it has is no change, and restarts nothing.
        client.command(&chat.id, configure_billing(from)).unwrap();
        assert_eq!(host.fake().start_count(), 1);
        // Still starting, a turn running, a request waiting for the user: refused.
        host.fake().emit(ChatEvent::State {
            state: ChatState::Starting,
        });
        host.wait_for_state(&chat.id, |s| *s == ChatState::Starting);
        refused(&mut client, configure_billing(to), "still starting");
        host.fake().emit(ChatEvent::State {
            state: ChatState::Idle,
        });
        host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
        send(&mut client, &chat.id, "hang");
        host.wait_for_log(&chat.id, |log| {
            log.iter().any(|e| {
                matches!(
                    &e.event,
                    ChatEvent::ItemStarted { item } if matches!(item.body, ItemBody::AgentMessage { .. })
                )
            })
        });
        refused(
            &mut client,
            configure_billing(to),
            "Wait for the turn to finish",
        );
        assert_eq!(host.fake().start_count(), 1);
        assert_eq!(host.fake().shutdowns.load(Ordering::SeqCst), 0);
        // The turn ends, as an interrupted one does.
        host.fake().emit(ChatEvent::TurnCompleted {
            turn_id: "turn-1-1".into(),
            outcome: TurnOutcome::Interrupted,
        });
        host.fake().emit(ChatEvent::State {
            state: ChatState::Idle,
        });
        host.wait_for_log(&chat.id, |log| {
            turns_completed(log) == 1 && last_state(log) == Some(ChatState::Idle)
        });

        // Idle: the process is stopped and started again with the change, on the same session.
        client.command(&chat.id, configure_billing(to)).unwrap();
        let info = host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
        assert_eq!(info.claude_billing, Some(to));
        assert_eq!(saved_info(&host, &chat.id).claude_billing, Some(to));
        let starts = host.fake().starts.lock().unwrap().clone();
        assert_eq!(starts.len(), 2);
        assert_eq!(host.fake().shutdowns.load(Ordering::SeqCst), 1);
        assert_eq!(
            (starts[1].resume.as_deref(), api_key_of(&starts[1])),
            (Some("thread-1"), key(to).to_owned())
        );
        assert_eq!(api_key_of(&starts[0]), key(from));

        // Stopped, the chat keeps it for its next start and starts nothing.
        client.close(&chat.id).unwrap();
        for _ in 0..2 {
            client.command(&chat.id, configure_billing(from)).unwrap();
        }
        assert_eq!(host.info(&chat.id).claude_billing, Some(from));
        assert_eq!(host.fake().start_count(), 2, "nothing started");
        send(&mut client, &chat.id, "next");
        host.wait_for_log(&chat.id, |log| {
            turns_completed(log) == 2 && last_state(log) == Some(ChatState::Idle)
        });
        let started = host.fake().starts.lock().unwrap()[2].clone();
        assert_eq!(
            (started.resume.as_deref(), api_key_of(&started)),
            (Some("thread-1"), key(from).to_owned())
        );
        // No driver is configured with it: it is the process's.
        assert!(
            host.fake()
                .configurations
                .lock()
                .unwrap()
                .iter()
                .all(|(command, _)| !matches!(
                    command,
                    ChatCommand::Configure {
                        claude_billing: Some(_),
                        ..
                    }
                ))
        );
    }
    // Another provider's chat has no billing to change.
    let host = TestHost::new();
    let codex = host.create(Provider::Codex);
    let error = host
        .client()
        .command(&codex.id, configure_billing(ClaudeBilling::ApiKey))
        .unwrap_err();
    assert!(error.contains("Only a Claude chat"), "{error}");
    assert_eq!(host.info(&codex.id).claude_billing, None);
}

#[test]
fn a_billing_change_the_keychain_or_a_restart_refuses_leaves_the_chat_as_it_was() {
    use ClaudeBilling::{ApiKey, Subscription};
    let host = TestHost::new();
    let keys = keys_for(&host.home);
    keys.save(HOST_KEY).unwrap();
    let mut new = host.new_chat(Provider::Claude);
    new.claude_billing = Some(Subscription);
    let chat = host.client().create(new).unwrap();
    host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
    let mut client = host.client();
    // A saved key the Keychain will not give: refused before the running Claude is stopped,
    // so the chat goes on as it was, and no Claude starts on another sign-in for it.
    keys.fail_reads(Some(
        "The Keychain could not read the Anthropic API key: User interaction is not allowed.",
    ));
    let error = client
        .command(&chat.id, configure_billing(ApiKey))
        .unwrap_err();
    assert!(
        error.contains("Billing was not changed")
            && error.contains("User interaction is not allowed")
            && error.contains("stays on Subscription")
            && !error.contains(".."),
        "{error}"
    );
    let info = host.info(&chat.id);
    assert_eq!(info.claude_billing, Some(Subscription));
    assert_eq!(
        saved_info(&host, &chat.id).claude_billing,
        Some(Subscription)
    );
    assert_eq!(info.state, ChatState::Idle, "still running as it was");
    assert_eq!(host.fake().start_count(), 1);
    assert_eq!(host.fake().shutdowns.load(Ordering::SeqCst), 0);
    keys.fail_reads(None);

    // A restart that fails on its own leaves the chat failed, as any failed start does,
    // with the billing it had, and says so; the next message resumes with that billing.
    *host.fake().fail_start.lock().unwrap() = Some("claude exited".into());
    let error = client
        .command(&chat.id, configure_billing(ApiKey))
        .unwrap_err();
    assert!(
        error.contains("Billing was not changed")
            && error.contains("claude exited")
            && error.contains("stays on Subscription"),
        "{error}"
    );
    let info = host.info(&chat.id);
    assert_eq!(info.claude_billing, Some(Subscription));
    assert_eq!(
        saved_info(&host, &chat.id).claude_billing,
        Some(Subscription)
    );
    assert!(
        matches!(info.state, ChatState::Failed { .. }),
        "{:?}",
        info.state
    );
    assert!(
        notices(&host.log(&chat.id))
            .iter()
            .any(|notice| notice.contains("Billing was not changed")),
        "{:?}",
        notices(&host.log(&chat.id))
    );
    send(&mut client, &chat.id, "again");
    host.wait_for_log(&chat.id, |log| {
        turns_completed(log) == 1 && last_state(log) == Some(ChatState::Idle)
    });
    let last = host.fake().starts.lock().unwrap().last().cloned().unwrap();
    assert_eq!(
        (last.resume.as_deref(), api_key_of(&last).as_str()),
        (Some("thread-1"), "removed")
    );
}

#[test]
fn starting_the_host_asks_the_keychain_nothing() {
    use ClaudeBilling::ApiKey;
    let mut host = TestHost::new();
    let keys = keys_for(&host.home);
    keys.save(HOST_KEY).unwrap();
    let mut new = host.new_chat(Provider::Claude);
    new.claude_billing = Some(ApiKey);
    let chat = host.client().create(new).unwrap();
    host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
    host.stop();
    let asked = keys.lookups();
    host.restart(quick_options());
    let info = host.info(&chat.id);
    assert_eq!(
        (info.claude_billing, info.state),
        (Some(ApiKey), ChatState::Stopped)
    );
    assert_eq!(
        keys.lookups(),
        asked,
        "loading the chats asked the Keychain"
    );
    send(&mut host.client(), &chat.id, "go");
    host.wait_for_log(&chat.id, |log| {
        turns_completed(log) == 1 && last_state(log) == Some(ChatState::Idle)
    });
    let last = host.fake().starts.lock().unwrap().last().cloned().unwrap();
    assert_eq!(api_key_of(&last), HOST_KEY);
}

// ---- Workers in the parent's timeline --------------------------------------------------------

mod worker_markers {
    use super::*;
    use crate::chat::workers::{WorkerEnd, WorkerFinal};

    fn new_in(host: &TestHost, project: &str, cwd: &str, parent: Option<&str>) -> NewChat {
        let mut new = host.new_chat(Provider::Codex);
        new.project_id = Some(project.into());
        new.cwd = host.home.join(cwd);
        new.parent_id = parent.map(str::to_owned);
        fs::create_dir_all(&new.cwd).unwrap();
        new
    }

    fn register_shell(home: &Path, id: &str, parent: &str, project: &str) {
        let path = home.join("sessions.json");
        let mut document: serde_json::Value = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_else(|| serde_json::json!({"sessions": []}));
        document["sessions"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "id": id, "parent_id": parent, "project_id": project, "worktree_id": null,
                "kind": "project", "cwd": home, "command": null, "harness": "claude",
                "created_at_unix": 1
            }));
        fs::write(path, serde_json::to_vec(&document).unwrap()).unwrap();
    }

    fn spawn(id: &str) -> WorkerNote {
        WorkerNote::Spawn {
            session_id: id.into(),
            kind: WorkerKind::Shell,
            late: false,
        }
    }

    fn markers(log: &[Envelope]) -> Vec<Item> {
        let mut transcript = Transcript::default();
        for envelope in log {
            transcript.apply(&envelope.event);
        }
        transcript
            .items
            .into_iter()
            .filter(|item| item.presentation.worker.is_some())
            .collect()
    }

    fn fold(log: &[Envelope]) -> Transcript {
        let mut transcript = Transcript::default();
        for envelope in log {
            transcript.apply(&envelope.event);
        }
        transcript
    }

    #[test]
    fn a_terminal_worker_gets_one_completed_card_in_its_parents_open_turn() {
        let host = TestHost::new();
        let parent = host
            .client()
            .create(new_in(&host, "p", "work", None))
            .unwrap();
        host.wait_for_state(&parent.id, |s| *s == ChatState::Idle);
        let fake = host.fake();
        fake.emit(ChatEvent::TurnStarted {
            turn_id: "t1".into(),
        });
        fake.emit(ChatEvent::ItemStarted {
            item: Item {
                presentation: Default::default(),
                id: "spawn-command".into(),
                turn_id: Some("t1".into()),
                status: ItemStatus::InProgress,
                body: ItemBody::Command {
                    command: "riwork shell create --worktree w --harness claude --json".into(),
                    cwd: None,
                    output: String::new(),
                    exit_code: None,
                },
            },
        });
        host.wait_for_log(&parent.id, |log| fold(log).items.len() == 1);
        let shell = Uuid::new_v4().to_string();
        register_shell(&host.home, &shell, &parent.id, "p");
        let mut client = host.client();
        client.note_worker(&parent.id, spawn(&shell)).unwrap();
        // The same start again (a retry, or a late sighting) writes nothing.
        client.note_worker(&parent.id, spawn(&shell)).unwrap();
        client
            .note_worker(
                &parent.id,
                WorkerNote::Spawn {
                    session_id: shell.clone(),
                    kind: WorkerKind::Shell,
                    late: true,
                },
            )
            .unwrap();
        // The first assignment of the same turn joins the card.
        client
            .note_worker(
                &parent.id,
                WorkerNote::Assign {
                    session_id: shell.clone(),
                    kind: WorkerKind::Shell,
                    text: "Fix the poller".into(),
                },
            )
            .unwrap();
        let log = host.log(&parent.id);
        let found = markers(&log);
        assert_eq!(found.len(), 1);
        let marker = &found[0];
        assert_eq!(marker.id, format!("worker:{shell}"));
        assert_eq!(marker.turn_id.as_deref(), Some("t1"));
        assert_eq!(marker.status, ItemStatus::Completed);
        let worker = marker.presentation.worker.as_ref().unwrap();
        assert_eq!(worker.assignment.as_deref(), Some("Fix the poller"));
        assert_eq!(worker.harness.as_deref(), Some("claude"));
        assert!(!worker.late);
        let transcript = fold(&log);
        assert_eq!(transcript.items[0].id, "spawn-command");
        assert_eq!(
            transcript.items[1].id, marker.id,
            "the card follows its command"
        );

        // The turn ends; the card stays completed, the command is closed.
        fake.emit(ChatEvent::TurnCompleted {
            turn_id: "t1".into(),
            outcome: TurnOutcome::Interrupted,
        });
        host.wait_for_log(&parent.id, |log| turns_completed(log) == 1);
        let transcript = fold(&host.log(&parent.id));
        assert_eq!(transcript.items[1].status, ItemStatus::Completed);

        // Finished: the card is replaced in place with its result.
        client
            .note_worker(
                &parent.id,
                WorkerNote::Finish {
                    session_id: shell.clone(),
                    kind: WorkerKind::Shell,
                    end: WorkerFinal {
                        status: WorkerEnd::Done,
                        ended_at: now_unix() + 5,
                        tail: "All tests pass".into(),
                        turn: None,
                        turn_end: None,
                        done_since: None,
                    },
                    done_since: Some(now_unix() + 5),
                    turn: None,
                    turn_end: None,
                    took: Vec::new(),
                },
            )
            .unwrap();
        let log = host.log(&parent.id);
        let transcript = fold(&log);
        assert_eq!(transcript.items.len(), 2);
        let done = transcript.items[1].presentation.worker.as_ref().unwrap();
        assert_eq!(done.end.as_ref().unwrap().status, WorkerEnd::Done);
        assert_eq!(done.end.as_ref().unwrap().tail, "All tests pass");

        // The phone's snapshot carries it at the place it was first written.
        let snapshot =
            log::read_snapshot(&host.home, &parent.id, None, u64::MAX, 100, 1 << 20, &[]).unwrap();
        let orders: Vec<_> = snapshot
            .items
            .iter()
            .map(|row| (row.item.id.clone(), row.order))
            .collect();
        let command = orders
            .iter()
            .find(|(id, _)| id == "spawn-command")
            .unwrap()
            .1;
        let card = orders.iter().find(|(id, _)| *id == marker.id).unwrap().1;
        assert!(command < card);
        let first_seq = log
            .iter()
            .find(|envelope| matches!(&envelope.event, ChatEvent::ItemCompleted { item } if item.id == marker.id))
            .unwrap()
            .seq;
        assert_eq!(card, first_seq, "an upsert keeps the snapshot order");
        assert!(snapshot.items.iter().any(|row| {
            row.item
                .presentation
                .worker
                .as_ref()
                .is_some_and(|w| w.end.is_some())
        }));
    }

    #[test]
    fn a_note_about_someone_elses_worker_is_refused_and_a_stopped_chat_is_not_started() {
        let host = TestHost::new();
        let parent = host
            .client()
            .create(new_in(&host, "p", "work", None))
            .unwrap();
        host.wait_for_state(&parent.id, |s| *s == ChatState::Idle);
        let other = Uuid::new_v4().to_string();
        let foreign = Uuid::new_v4().to_string();
        register_shell(&host.home, &foreign, &other, "p");
        let elsewhere = Uuid::new_v4().to_string();
        register_shell(&host.home, &elsewhere, &parent.id, "another-project");
        let mut client = host.client();
        for shell in [&foreign, &elsewhere] {
            assert!(client.note_worker(&parent.id, spawn(shell)).is_err());
        }
        assert!(
            client
                .note_worker(&parent.id, spawn(&Uuid::new_v4().to_string()))
                .is_err()
        );
        assert!(markers(&host.log(&parent.id)).is_empty());

        client.close(&parent.id).unwrap();
        host.wait_for_state(&parent.id, |s| *s == ChatState::Stopped);
        let starts = host.fake().start_count();
        let ours = Uuid::new_v4().to_string();
        register_shell(&host.home, &ours, &parent.id, "p");
        client
            .note_worker(
                &parent.id,
                WorkerNote::Assign {
                    session_id: ours.clone(),
                    kind: WorkerKind::Shell,
                    text: "Carry on".into(),
                },
            )
            .unwrap();
        let found = markers(&host.log(&parent.id));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].turn_id, None, "no turn is open in a stopped chat");
        assert_eq!(
            host.fake().start_count(),
            starts,
            "a note never starts a chat"
        );
        assert_eq!(host.info(&parent.id).state, ChatState::Stopped);
        // After a host restart the card is found in the log, not written twice.
        drop(client);
        let mut host = host;
        host.restart(quick_options());
        host.client().note_worker(&parent.id, spawn(&ours)).unwrap();
        assert_eq!(markers(&host.log(&parent.id)).len(), 1);
    }

    #[test]
    fn a_worker_chat_has_a_card_in_its_parent_that_ends_with_its_reply() {
        let host = TestHost::new();
        let parent = host
            .client()
            .create(new_in(&host, "p", "work", None))
            .unwrap();
        host.wait_for_state(&parent.id, |s| *s == ChatState::Idle);
        let child = host
            .client()
            .create(new_in(&host, "p", "child", Some(&parent.id)))
            .unwrap();
        assert_eq!(child.parent_id.as_deref(), Some(parent.id.as_str()));
        let found = markers(&host.log(&parent.id));
        assert_eq!(found.len(), 1);
        let worker = found[0].presentation.worker.as_ref().unwrap();
        assert_eq!(worker.kind, WorkerKind::Chat);
        assert_eq!(worker.session_id, child.id);
        assert_eq!(worker.harness.as_deref(), Some("codex"));
        host.wait_for_state(&child.id, |s| *s == ChatState::Idle);
        let fake = fake_for(&host.home.join("child"));
        fake.emit(ChatEvent::TurnStarted {
            turn_id: "c1".into(),
        });
        // The card was given nothing, and is the latest: the first message is its.
        fake.emit(ChatEvent::ItemCompleted {
            item: Item {
                presentation: Default::default(),
                id: "task".into(),
                turn_id: Some("c1".into()),
                status: ItemStatus::Completed,
                body: ItemBody::UserMessage {
                    text: "Review the patch".into(),
                },
            },
        });
        fake.emit(ChatEvent::ItemCompleted {
            item: Item {
                presentation: Default::default(),
                id: "reply".into(),
                turn_id: Some("c1".into()),
                status: ItemStatus::Completed,
                body: ItemBody::AgentMessage {
                    text: "Reviewed.\nAll checks pass.".into(),
                },
            },
        });
        fake.emit(ChatEvent::TurnCompleted {
            turn_id: "c1".into(),
            outcome: TurnOutcome::Completed,
        });
        let log = host.wait_for_log(&parent.id, |log| {
            markers(log)
                .first()
                .and_then(|marker| marker.presentation.worker.as_ref()?.end.clone())
                .is_some()
        });
        let end = markers(&log)[0]
            .presentation
            .worker
            .as_ref()
            .unwrap()
            .end
            .clone()
            .unwrap();
        assert_eq!(end.status, WorkerEnd::Done);
        assert_eq!(end.tail, "Reviewed.\nAll checks pass.");
        assert_eq!(markers(&log).len(), 1);
    }

    #[test]
    fn an_older_host_that_cannot_read_a_note_is_nothing_recorded() {
        let home = short_home();
        let socket = crate::chat::client::socket_path(&home);
        fs::create_dir_all(socket.parent().unwrap()).unwrap();
        let listener = UnixListener::bind(&socket).unwrap();
        let old_host = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let id = serde_json::from_str::<serde_json::Value>(&line).unwrap()["id"]
                .as_str()
                .unwrap()
                .to_owned();
            let mut stream = stream;
            writeln!(
                stream,
                "{}",
                serde_json::json!({"id": id, "ok": false, "error": "unreadable request: unknown variant `note_worker`"})
            )
            .unwrap();
        });
        let error =
            crate::chat::workers::send(&home, &Uuid::new_v4().to_string(), spawn("s")).unwrap_err();
        assert!(error.contains("unreadable request"));
        old_host.join().unwrap();
        // With no host at all, a spawner's note is silently nothing.
        fs::remove_file(&socket).unwrap();
        assert!(crate::chat::workers::send(&home, "c", spawn("s")).is_err());
        let _ = fs::remove_dir_all(home);
    }
}

/// How a worker's end finds its card: by the worker chat's own turn, through a restart,
/// for a closed terminal or a deleted chat, and never quietly unwritten.
mod worker_ends {
    use super::*;
    use crate::chat::model::Delta;
    use crate::chat::workers::{WorkerEnd, WorkerFinal, WorkerRef};

    /// A chat of project `p`, started by `parent` when there is one.
    fn chat(host: &NoticeHost, parent: Option<&str>) -> Arc<Chat> {
        let info: ChatInfo = serde_json::from_value(serde_json::json!({
            "id": Uuid::new_v4().to_string(), "provider": Provider::Codex, "cwd": host.home,
            "title": "test", "created_at_unix": 0, "state": {"state":"idle"},
            "project_id": "p", "parent_id": parent
        }))
        .unwrap();
        let dir = log::chat_dir(&host.home, &info.id).unwrap();
        let chat_log = ChatLog::create(&dir, &info).unwrap();
        let mut inner = Inner::new(info.clone(), chat_log, 0, host.shared.dismissals.clone());
        inner.worker_notes = Some(host.shared.worker_notes.clone());
        inner.append(ChatEvent::Info { info: info.clone() });
        let chat = Arc::new(Chat {
            dir,
            lifecycle: Mutex::new(()),
            inner: Mutex::new(inner),
        });
        lock(&host.shared.chats).insert(info.id, chat.clone());
        chat
    }

    fn emit(chat: &Chat, event: ChatEvent) {
        lock(&chat.inner).take_driver_event(event);
    }

    fn turn(chat: &Chat, id: &str) {
        emit(chat, ChatEvent::TurnStarted { turn_id: id.into() });
    }

    fn end(chat: &Chat, id: &str, outcome: TurnOutcome) {
        emit(
            chat,
            ChatEvent::TurnCompleted {
                turn_id: id.into(),
                outcome,
            },
        );
    }

    fn said(chat: &Chat, id: &str, turn: &str, body: ItemBody) {
        emit(
            chat,
            ChatEvent::ItemCompleted {
                item: Item {
                    presentation: Default::default(),
                    id: id.into(),
                    turn_id: Some(turn.into()),
                    status: ItemStatus::Completed,
                    body,
                },
            },
        );
    }

    fn user(text: &str) -> ItemBody {
        ItemBody::UserMessage { text: text.into() }
    }

    fn reply(text: &str) -> ItemBody {
        ItemBody::AgentMessage { text: text.into() }
    }

    /// The parent's chat-worker card for `child`, made in the parent's turn `p1` and given
    /// its first assignment there, as `riwork chat new` and `chat send` make it.
    fn started(host: &NoticeHost, parent: &Chat, child: &Chat, text: &str) {
        turn(parent, "p1");
        note_worker(
            &host.shared,
            &parent.info().id,
            WorkerNote::Spawn {
                session_id: child.info().id,
                kind: WorkerKind::Chat,
                late: false,
            },
        )
        .unwrap();
        assign(host, parent, child, text);
    }

    fn assign(host: &NoticeHost, parent: &Chat, child: &Chat, text: &str) {
        note_worker(
            &host.shared,
            &parent.info().id,
            WorkerNote::Assign {
                session_id: child.info().id,
                kind: WorkerKind::Chat,
                text: text.into(),
            },
        )
        .unwrap();
    }

    /// What a real host's worker thread does with what is queued.
    fn deliver(host: &NoticeHost) {
        let receiver = lock(&host.worker_notes);
        while let Ok(note) = receiver.try_recv() {
            deliver_worker_note(&host.shared, note);
        }
    }

    fn cards(home: &Path, chat_id: &str) -> Vec<WorkerRef> {
        let mut transcript = Transcript::default();
        for envelope in log::read_envelopes(&log::chat_dir(home, chat_id).unwrap()).unwrap() {
            transcript.apply(&envelope.event);
        }
        transcript
            .items
            .iter()
            .filter_map(|item| item.presentation.worker.clone())
            .collect()
    }

    fn ended(card: &WorkerRef) -> Option<(WorkerEnd, &str)> {
        card.end.as_ref().map(|end| (end.status, end.tail.as_str()))
    }

    #[test]
    fn a_follow_up_sent_while_the_last_turn_runs_is_not_ended_by_that_turn() {
        let host = NoticeHost::new();
        let parent = chat(&host, None);
        let child = chat(&host, Some(&parent.info().id));
        started(&host, &parent, &child, "Task one");
        turn(&child, "c1");
        said(&child, "u1", "c1", user("Task one"));
        end(&parent, "p1", TurnOutcome::Completed);
        turn(&parent, "p2");
        assign(&host, &parent, &child, "Task two");
        said(&child, "r1", "c1", reply("One done"));
        end(&child, "c1", TurnOutcome::Completed);
        deliver(&host);
        let found = cards(&host.home, &parent.info().id);
        assert_eq!(found.len(), 2);
        assert_eq!(ended(&found[0]), Some((WorkerEnd::Done, "One done")));
        assert_eq!(ended(&found[1]), None, "the follow-up waits for its turn");

        turn(&child, "c2");
        said(&child, "u2", "c2", user("Task two"));
        said(&child, "r2", "c2", reply("Two done"));
        end(&child, "c2", TurnOutcome::Completed);
        deliver(&host);
        let found = cards(&host.home, &parent.info().id);
        assert_eq!(ended(&found[0]), Some((WorkerEnd::Done, "One done")));
        assert_eq!(ended(&found[1]), Some((WorkerEnd::Done, "Two done")));
    }

    #[test]
    fn messages_steered_into_one_turn_all_end_with_it() {
        let host = NoticeHost::new();
        let parent = chat(&host, None);
        let child = chat(&host, Some(&parent.info().id));
        started(&host, &parent, &child, "Task one");
        turn(&child, "c1");
        said(&child, "u1", "c1", user("Task one"));
        end(&parent, "p1", TurnOutcome::Completed);
        turn(&parent, "p2");
        assign(&host, &parent, &child, "Also check the docs");
        // Codex steers it into the running turn; Claude takes it behind the first.
        said(&child, "u2", "c1", user("Also check the docs"));
        said(&child, "r1", "c1", reply("Both done"));
        end(&child, "c1", TurnOutcome::Completed);
        deliver(&host);
        let found = cards(&host.home, &parent.info().id);
        assert_eq!(found.len(), 2);
        assert_eq!(ended(&found[0]), Some((WorkerEnd::Done, "Both done")));
        assert_eq!(ended(&found[1]), Some((WorkerEnd::Done, "Both done")));
    }

    #[test]
    fn a_reply_cut_off_mid_stream_is_the_cards_result() {
        let host = NoticeHost::new();
        let parent = chat(&host, None);
        let child = chat(&host, Some(&parent.info().id));
        started(&host, &parent, &child, "Task one");
        turn(&child, "c1");
        said(&child, "u1", "c1", user("Task one"));
        emit(
            &child,
            ChatEvent::ItemStarted {
                item: Item {
                    presentation: Default::default(),
                    id: "r1".into(),
                    turn_id: Some("c1".into()),
                    status: ItemStatus::InProgress,
                    body: reply(""),
                },
            },
        );
        emit(
            &child,
            ChatEvent::ItemDelta {
                item_id: "r1".into(),
                delta: Delta::Text("Partial answer".into()),
            },
        );
        end(&child, "c1", TurnOutcome::Interrupted);
        deliver(&host);
        let found = cards(&host.home, &parent.info().id);
        assert_eq!(
            ended(&found[0]),
            Some((WorkerEnd::Stopped, "Partial answer"))
        );
    }

    #[test]
    fn a_restart_ends_the_interrupted_turns_card_with_what_it_had_said() {
        let mut host = NoticeHost::new();
        let parent = chat(&host, None);
        let child = chat(&host, Some(&parent.info().id));
        let (parent_id, child_id) = (parent.info().id, child.info().id);
        started(&host, &parent, &child, "Task one");
        lock(&child.inner).set_state(ChatState::Running);
        turn(&child, "c1");
        said(&child, "u1", "c1", user("Task one"));
        emit(
            &child,
            ChatEvent::ItemStarted {
                item: Item {
                    presentation: Default::default(),
                    id: "r1".into(),
                    turn_id: Some("c1".into()),
                    status: ItemStatus::InProgress,
                    body: reply("Halfway"),
                },
            },
        );
        end(&parent, "p1", TurnOutcome::Completed);
        turn(&parent, "p2");
        // Written before its message went: the host ends before the message arrives.
        assign(&host, &parent, &child, "Task two");
        drop((parent, child));
        host.restart();
        deliver(&host);
        let found = cards(&host.home, &parent_id);
        assert_eq!(found.len(), 2);
        assert_eq!(ended(&found[0]), Some((WorkerEnd::Stopped, "Halfway")));
        assert_eq!(ended(&found[1]), None, "the queued follow-up is still open");
        assert!(cards(&host.home, &child_id).is_empty());
    }

    #[test]
    fn an_end_the_host_could_not_deliver_before_it_ended_is_recorded_at_the_next_start() {
        let mut host = NoticeHost::new();
        let parent = chat(&host, None);
        let child = chat(&host, Some(&parent.info().id));
        let parent_id = parent.info().id;
        started(&host, &parent, &child, "Task one");
        turn(&child, "c1");
        said(&child, "u1", "c1", user("Task one"));
        said(&child, "r1", "c1", reply("Finished"));
        end(&child, "c1", TurnOutcome::Completed);
        // The host ends with the parent's note still queued.
        drop((parent, child));
        host.restart();
        deliver(&host);
        let found = cards(&host.home, &parent_id);
        assert_eq!(ended(&found[0]), Some((WorkerEnd::Done, "Finished")));
        let lines = host_log_len(&host.home, &parent_id);
        // Started again, nothing is recorded twice.
        host.restart();
        deliver(&host);
        assert_eq!(host_log_len(&host.home, &parent_id), lines);
    }

    fn host_log_len(home: &Path, chat_id: &str) -> usize {
        log::read_envelopes(&log::chat_dir(home, chat_id).unwrap())
            .unwrap()
            .len()
    }

    #[test]
    fn a_worker_chat_deleted_before_its_end_was_delivered_still_ends_its_card() {
        let host = NoticeHost::new();
        let parent = chat(&host, None);
        let child = chat(&host, Some(&parent.info().id));
        let (parent_id, child_id) = (parent.info().id, child.info().id);
        started(&host, &parent, &child, "Task one");
        turn(&child, "c1");
        said(&child, "u1", "c1", user("Task one"));
        said(&child, "r1", "c1", reply("Done before deletion"));
        end(&child, "c1", TurnOutcome::Completed);
        drop(child);
        delete(&host.shared, &child_id).unwrap();
        deliver(&host);
        let found = cards(&host.home, &parent_id);
        assert_eq!(
            ended(&found[0]),
            Some((WorkerEnd::Done, "Done before deletion"))
        );
    }

    fn register_shell(home: &Path, id: &str, parent: &str) {
        fs::write(
            home.join("sessions.json"),
            serde_json::to_vec(&serde_json::json!({"sessions": [{
                "id": id, "parent_id": parent, "project_id": "p", "worktree_id": null,
                "kind": "project", "cwd": home, "command": null, "harness": "claude",
                "created_at_unix": 1
            }]}))
            .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn a_closed_terminals_exit_is_recorded_on_its_card() {
        let host = NoticeHost::new();
        let parent = chat(&host, None);
        let parent_id = parent.info().id;
        let shell = Uuid::new_v4().to_string();
        register_shell(&host.home, &shell, &parent_id);
        let shell_note = |end: Option<WorkerEnd>| match end {
            None => WorkerNote::Spawn {
                session_id: shell.clone(),
                kind: WorkerKind::Shell,
                late: false,
            },
            Some(status) => WorkerNote::Finish {
                session_id: shell.clone(),
                kind: WorkerKind::Shell,
                end: WorkerFinal {
                    status,
                    ended_at: now_unix(),
                    tail: "Last lines".into(),
                    turn: None,
                    turn_end: None,
                    done_since: None,
                },
                done_since: None,
                turn: None,
                turn_end: None,
                took: Vec::new(),
            },
        };
        note_worker(&host.shared, &parent_id, shell_note(None)).unwrap();
        // `shell close` takes it out of the registry before anyone notes its exit.
        fs::write(host.home.join("sessions.json"), r#"{"sessions": []}"#).unwrap();
        note_worker(
            &host.shared,
            &parent_id,
            shell_note(Some(WorkerEnd::Exited)),
        )
        .unwrap();
        let found = cards(&host.home, &parent_id);
        assert_eq!(ended(&found[0]), Some((WorkerEnd::Exited, "Last lines")));
        // A terminal that never had a card here is still nobody's to record.
        let stranger = Uuid::new_v4().to_string();
        let note = WorkerNote::Spawn {
            session_id: stranger,
            kind: WorkerKind::Shell,
            late: false,
        };
        assert!(note_worker(&host.shared, &parent_id, note).is_err());
    }

    #[test]
    fn a_note_the_log_cannot_take_is_an_error() {
        use std::os::unix::fs::PermissionsExt;
        let host = NoticeHost::new();
        let parent = chat(&host, None);
        let parent_id = parent.info().id;
        let shell = Uuid::new_v4().to_string();
        register_shell(&host.home, &shell, &parent_id);
        let events = parent.dir.join("events.jsonl");
        fs::set_permissions(&events, fs::Permissions::from_mode(0o444)).unwrap();
        let note = WorkerNote::Spawn {
            session_id: shell,
            kind: WorkerKind::Shell,
            late: false,
        };
        let result = note_worker(&host.shared, &parent_id, note);
        fs::set_permissions(&events, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(result.is_err(), "nothing was recorded: {result:?}");
    }
}
