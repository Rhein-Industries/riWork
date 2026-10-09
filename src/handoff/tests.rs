//! A handoff from start to finish: a source read from disk or asked, a document written,
//! and a target chat made by the in-process chat host with its fake drivers.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    time::{Duration, Instant},
};

use super::{
    summary::Timing,
    testing::{chat_info, write_chat},
    *,
};
use crate::chat::{
    model::{ChatCommand, ChatEvent, ChatState, Delta, Item, ItemBody, ItemStatus, Provider},
    testing::{TestHost, fake_for, short_home},
};

fn quick() -> Timing {
    Timing {
        wait: Duration::from_secs(20),
        poll: Duration::from_millis(20),
        grace: Duration::from_millis(400),
    }
}

fn account(_: &Path, query: &str) -> Result<CodexAccountBinding, String> {
    match query {
        "Work" | "acct-work" => Ok(CodexAccountBinding {
            id: Some("acct-work".into()),
            label: Some("me@example.com · Work".into()),
            email: Some("me@example.com".into()),
            home: "/accounts/work/home".into(),
        }),
        "System default" => Ok(CodexAccountBinding {
            id: None,
            label: None,
            email: None,
            home: "/home/me/.codex".into(),
        }),
        other => Err(format!("No Codex account matches '{other}'.")),
    }
}

/// A chat to hand off from, with its conversation on disk.
fn source_chat(host: &TestHost, messages: &[(&str, &str)]) -> ChatInfo {
    let mut info = chat_info(Some("claude-opus-5-5"));
    info.provider = Provider::Claude;
    info.cwd = host.work();
    info.project_id = Some("project-1".into());
    info.worktree_id = None;
    let mut events = vec![ChatEvent::Info { info: info.clone() }];
    for (index, (user, agent)) in messages.iter().enumerate() {
        let item = |id: String, body| Item {
            presentation: Default::default(),
            id,
            turn_id: Some(format!("t{index}")),
            status: ItemStatus::Completed,
            body,
        };
        events.push(ChatEvent::ItemStarted {
            item: item(
                format!("u{index}"),
                ItemBody::UserMessage {
                    text: (*user).into(),
                },
            ),
        });
        events.push(ChatEvent::ItemCompleted {
            item: item(
                format!("a{index}"),
                ItemBody::AgentMessage {
                    text: (*agent).into(),
                },
            ),
        });
    }
    write_chat(&host.home, info.clone(), events);
    info
}

fn request(source: Source, kind: Kind, provider: HarnessKind) -> Request {
    Request {
        source,
        kind,
        provider,
        model: None,
        effort: None,
        account: None,
        mode: None,
        context: Context::Transcript,
        note: None,
    }
}

struct Run<'a> {
    host: &'a TestHost,
}

impl Run<'_> {
    fn go(&self, request: Request) -> Result<Outcome, String> {
        let socket = self.host.socket();
        let ensure = move || Ok(socket.clone());
        let env = Env {
            home: &self.host.home,
            ensure: &ensure,
            account: &account,
            timing: quick(),
        };
        run(&env, request, &|_| {})
    }
}

fn chat_of(outcome: &Outcome) -> &ChatInfo {
    match &outcome.target {
        Started::Chat(chat) => chat,
        Started::Shell(shell) => panic!("a shell: {}", shell.id),
    }
}

/// The text of the last message sent to a driver in the host's working directory.
fn last_message(host: &TestHost) -> String {
    match host.fake().commands().last() {
        Some(ChatCommand::Send { text }) => text.clone(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_chat_goes_to_a_new_chat_with_the_model_effort_mode_and_account_chosen() {
    let host = TestHost::new();
    let source = source_chat(
        &host,
        &[
            ("Fix the build.", "Done: the build passes."),
            ("Now update the docs.", "Docs updated in `README.md`."),
        ],
    );
    let mut asked = request(Source::Chat(source.clone()), Kind::Chat, HarnessKind::Codex);
    asked.model = Some("gpt-5".into());
    asked.effort = Some("high".into());
    asked.account = Some("Work".into());
    asked.mode = Some(ApprovalMode::AutoEdit);
    asked.note = Some("Use  the\nWork account\tfrom here on.".into());
    let outcome = Run { host: &host }.go(asked).unwrap();

    // The chat is where the source is, runs what was asked, and is not the source.
    let chat = chat_of(&outcome);
    assert_ne!(chat.id, source.id);
    assert_eq!(chat.provider, Provider::Codex);
    assert_eq!(chat.cwd, source.cwd);
    assert_eq!(chat.project_id, source.project_id);
    assert_eq!(chat.model.as_deref(), Some("gpt-5"));
    assert_eq!(chat.effort.as_deref(), Some("high"));
    assert_eq!(chat.approval_mode, ApprovalMode::AutoEdit);
    assert_eq!(chat.codex_account_id.as_deref(), Some("acct-work"));
    assert!(
        chat.title
            .starts_with("Handoff from Claude chat \"Fix the build\""),
        "{}",
        chat.title
    );
    let start = host.fake().starts.lock().unwrap().last().cloned().unwrap();
    assert_eq!(
        (
            start.model.as_deref(),
            start.effort.as_deref(),
            start.approval_mode
        ),
        (Some("gpt-5"), Some("high"), ApprovalMode::AutoEdit)
    );

    // Its first message: who it takes over from, the short document in it, the note.
    let message = last_message(&host);
    let label = Source::Chat(source.clone()).label();
    assert!(
        message.starts_with(&format!(
            "You are taking over a conversation from {label}. The handoff is below; continue from where it left off. Note from the person who handed this over: Use the Work account from here on.\n\n<handoff>\n# Handoff\n"
        )),
        "{message}"
    );
    assert!(message.ends_with("\n</handoff>"));
    assert!(!message.contains(&outcome.document.display().to_string()));

    // The document: the same text, owner-only, in the data directory and not the repo.
    let document = fs::read_to_string(&outcome.document).unwrap();
    assert!(message.contains(document.trim_end()));
    assert_eq!(
        outcome.document,
        host.home
            .join("handoffs")
            .join(format!("{}.md", outcome.handoff_id))
    );
    assert_eq!(
        fs::metadata(&outcome.document)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(host.home.join("handoffs"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    for wanted in [
        "- **From:** Claude chat \"Fix the build\"",
        "- **Model:** claude-opus-5-5",
        "- **Directory:** `",
        "### User\n\nFix the build.\n",
        "### Agent\n\nDone: the build passes.\n",
        "### User\n\nNow update the docs.\n",
        "### Agent\n\nDocs updated in `README.md`.\n",
        "Use the Work account from here on.",
    ] {
        assert!(
            document.contains(wanted),
            "missing {wanted:?} in\n{document}"
        );
    }
    assert_eq!(
        (outcome.context, outcome.fallback),
        (Context::Transcript, None)
    );

    // The source stays as it was: not stopped, not touched.
    assert_eq!(
        host.fake()
            .shutdowns
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}

#[test]
fn the_system_default_account_is_a_choice_and_no_account_is_the_projects() {
    let host = TestHost::new();
    let source = source_chat(&host, &[("hi", "hello")]);
    let mut asked = request(Source::Chat(source.clone()), Kind::Chat, HarnessKind::Codex);
    asked.account = Some("System default".into());
    let outcome = Run { host: &host }.go(asked).unwrap();
    // `system-default` is what the host resolves to the CLI's own profile: no account id.
    assert_eq!(
        chat_of(&outcome).codex_account_id.as_deref(),
        Some("system-default")
    );

    let outcome = Run { host: &host }
        .go(request(
            Source::Chat(source),
            Kind::Chat,
            HarnessKind::Codex,
        ))
        .unwrap();
    // The test host picks "account-a" for a project, as the real one picks the project's.
    assert_eq!(
        chat_of(&outcome).codex_account_id.as_deref(),
        Some("account-a")
    );
}

#[test]
fn a_document_too_long_to_send_is_read_from_its_file() {
    let host = TestHost::new();
    let long = "a sentence of the conversation. ".repeat(1500);
    let source = source_chat(&host, &[("start", long.as_str()), ("go on", long.as_str())]);
    let mut asked = request(
        Source::Chat(source.clone()),
        Kind::Chat,
        HarnessKind::Claude,
    );
    asked.note = Some("mind the tests".into());
    let outcome = Run { host: &host }.go(asked).unwrap();
    let document = fs::read_to_string(&outcome.document).unwrap();
    assert!(document.len() > 48 * 1024);
    let label = Source::Chat(source).label();
    assert_eq!(
        last_message(&host),
        format!(
            "You are taking over a conversation from {label}. Read the handoff at {} and continue from where it left off. Note from the person who handed this over: mind the tests",
            outcome.document.display()
        )
    );
    assert!(outcome.document.is_absolute());
}

#[test]
fn a_summary_is_asked_for_and_becomes_the_document() {
    let host = TestHost::new();
    let chat = host.create(Provider::Claude);
    host.wait_for_state(&chat.id, |state| *state == ChatState::Idle);
    let fake = host.fake();
    // The agent: writes where the request says when it arrives.
    std::thread::spawn(move || {
        let end = Instant::now() + Duration::from_secs(20);
        while Instant::now() < end {
            if let Some(ChatCommand::Send { text }) = fake.commands().first() {
                let path = text
                    .split(" to ")
                    .nth(1)
                    .and_then(|rest| rest.split(" (a Markdown").next())
                    .unwrap();
                fs::write(path, "## Goal\nHand it over.\n\n## Next steps\n- ship\n").unwrap();
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    });
    let mut asked = request(Source::Chat(chat.clone()), Kind::Chat, HarnessKind::Codex);
    asked.context = Context::Summary;
    let steps = std::sync::Mutex::new(Vec::new());
    let socket = host.socket();
    let ensure = move || Ok(socket.clone());
    let env = Env {
        home: &host.home,
        ensure: &ensure,
        account: &account,
        timing: quick(),
    };
    let outcome = run(&env, asked, &|step| {
        steps.lock().unwrap().push(step.to_owned())
    })
    .unwrap();
    assert_eq!(
        (outcome.context, outcome.fallback.as_deref()),
        (Context::Summary, None)
    );
    let document = fs::read_to_string(&outcome.document).unwrap();
    assert!(document.contains("- **Contents:** a summary the agent wrote of the conversation"));
    assert!(document.contains("## Goal\nHand it over.\n\n## Next steps\n- ship\n"));
    // The summary file was a means, and the progress said what was going on.
    let leftovers: Vec<_> = fs::read_dir(host.home.join("handoffs"))
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(leftovers.len(), 1, "{leftovers:?}");
    let steps = steps.lock().unwrap();
    assert_eq!(steps[0], "Writing summary…");
    assert!(
        steps.contains(&"Starting the chat…".to_owned()),
        "{steps:?}"
    );
    // The new chat got the summary.
    let new_chat = chat_of(&outcome);
    assert_ne!(new_chat.id, chat.id);
    assert!(last_message(&host).contains("## Goal\nHand it over."));
}

#[test]
fn a_summary_that_does_not_come_is_replaced_by_the_transcript_and_the_document_says_so() {
    let host = TestHost::new();
    // A live chat with a conversation of its own; its fake agent answers but writes nothing.
    let chat = host.create(Provider::Codex);
    host.wait_for_state(&chat.id, |state| *state == ChatState::Idle);
    host.client()
        .command(
            &chat.id,
            ChatCommand::Send {
                text: "build the thing".into(),
            },
        )
        .unwrap();
    host.wait_for_log(&chat.id, |log| {
        log.iter()
            .any(|e| matches!(e.event, ChatEvent::TurnCompleted { .. }))
    });
    host.wait_for_state(&chat.id, |state| *state == ChatState::Idle);
    let mut asked = request(Source::Chat(chat), Kind::Chat, HarnessKind::Claude);
    asked.context = Context::Summary;
    let outcome = Run { host: &host }.go(asked).unwrap();
    assert_eq!(outcome.context, Context::Transcript);
    let reason = outcome.fallback.clone().expect("a reason");
    assert!(
        reason.starts_with("the agent's turn ended without a summary at "),
        "{reason}"
    );
    let document = fs::read_to_string(&outcome.document).unwrap();
    assert!(
        document.contains("- **Caveat:** A summary was asked for, but the agent's turn ended"),
        "{document}"
    );
    assert!(document.contains("this is the transcript instead."));
    // The transcript holds both the person's message and the request that was sent.
    assert!(
        document.contains("### User\n\nbuild the thing\n"),
        "{document}"
    );
    assert!(document.contains("### Agent\n\necho: build the thing\n"));
}

#[test]
fn what_cannot_be_done_is_refused_before_anything_is_read_or_started() {
    let host = TestHost::new();
    let source = source_chat(&host, &[("hi", "hello")]);
    let go = |change: &dyn Fn(&mut Request)| {
        let mut asked = request(Source::Chat(source.clone()), Kind::Chat, HarnessKind::Codex);
        change(&mut asked);
        Run { host: &host }.go(asked).err().unwrap()
    };
    // Claude (and Grok) have no accounts of RiWork's.
    let error = go(&|asked| {
        asked.provider = HarnessKind::Claude;
        asked.account = Some("Work".into());
    });
    assert!(
        error.starts_with("Claude has no RiWork-managed accounts"),
        "{error}"
    );
    assert!(error.contains("system"));
    let error = go(&|asked| {
        asked.kind = Kind::Shell;
        asked.provider = HarnessKind::Grok;
        asked.account = Some("Work".into());
    });
    assert!(
        error.starts_with("Grok has no RiWork-managed accounts"),
        "{error}"
    );
    // Chats run Codex and Claude.
    let error = go(&|asked| asked.provider = HarnessKind::Grok);
    assert!(error.contains("Grok runs in a terminal"), "{error}");
    // An account that does not exist is not a quiet fall back to another.
    let error = go(&|asked| asked.account = Some("Nobody".into()));
    assert_eq!(error, "No Codex account matches 'Nobody'.");
    // A shell that is no agent has no one to write a summary.
    let shell = testing::shell_with(&Uuid::new_v4().to_string(), "grok", None);
    let mut asked = request(Source::Shell(shell), Kind::Chat, HarnessKind::Codex);
    asked.context = Context::Summary;
    assert!(
        check(&asked)
            .unwrap_err()
            .contains("not a Codex or Claude agent")
    );
    // An orchestrator is nobody's to hand off.
    let mut orchestrator = testing::shell_with(&Uuid::new_v4().to_string(), "codex", None);
    orchestrator.kind = ShellKind::Orchestrator;
    let asked = request(Source::Shell(orchestrator), Kind::Chat, HarnessKind::Codex);
    assert!(check(&asked).unwrap_err().contains("orchestrator"));
    assert!(host.client().list().unwrap().is_empty());
    assert!(!host.home.join("handoffs").exists(), "nothing was written");
}

#[test]
fn a_chat_whose_provider_does_not_start_is_reported_with_the_document_to_retry_from() {
    let host = TestHost::new();
    let source = source_chat(&host, &[("hi", "hello")]);
    *fake_for(&source.cwd).fail_start.lock().unwrap() = Some("codex is not installed".into());
    let error = Run { host: &host }
        .go(request(
            Source::Chat(source),
            Kind::Chat,
            HarnessKind::Codex,
        ))
        .err()
        .unwrap();
    assert!(
        error.contains("was created, but its provider did not start: codex is not installed"),
        "{error}"
    );
    let documents: Vec<PathBuf> = fs::read_dir(host.home.join("handoffs"))
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .collect();
    assert_eq!(documents.len(), 1);
    assert!(
        error.contains(&documents[0].display().to_string()),
        "{error}"
    );
}

#[test]
fn a_terminal_needs_a_project_to_start_in() {
    let host = TestHost::new();
    let mut source = source_chat(&host, &[("hi", "hello")]);
    source.project_id = None;
    let error = Run { host: &host }
        .go(request(
            Source::Chat(source),
            Kind::Shell,
            HarnessKind::Claude,
        ))
        .err()
        .unwrap();
    assert!(error.contains("belongs to no project"), "{error}");
    assert!(error.contains("Hand off to a chat instead"), "{error}");
    assert!(
        !host.home.join("handoffs").exists(),
        "refused before a document"
    );

    // Also before a live source is asked for a summary and waited for.
    let live = host.create(Provider::Codex);
    host.wait_for_state(&live.id, |state| *state == ChatState::Idle);
    let mut asked = request(Source::Chat(live), Kind::Shell, HarnessKind::Claude);
    asked.context = Context::Summary;
    let error = Run { host: &host }.go(asked).err().unwrap();
    assert!(error.contains("belongs to no project"), "{error}");
    assert!(host.fake().commands().is_empty(), "nothing was asked");
}

#[test]
fn a_chat_host_that_does_not_take_the_account_is_noticed_before_the_chat_is_sent_anything() {
    use crate::chat::{
        host::{Host, Providers},
        testing::{fake_providers, quick_options},
    };
    // An older host: the request carries a field it does not know and puts the chat on the
    // project's account.
    fn project_account(
        _: &Path,
        provider: Provider,
        _: Option<&str>,
        _asked_for: Option<&str>,
    ) -> Result<Option<String>, String> {
        Ok((provider == Provider::Codex).then(|| "account-a".to_owned()))
    }
    let home = short_home();
    fs::create_dir_all(home.join("work")).unwrap();
    let host = Host::start(
        &home,
        Providers {
            account: project_account,
            ..fake_providers()
        },
        quick_options(),
    )
    .unwrap();
    let mut info = chat_info(None);
    info.cwd = home.join("work");
    write_chat(&home, info.clone(), Vec::new());
    let socket = host.socket().to_path_buf();
    let ensure = {
        let socket = socket.clone();
        move || Ok(socket.clone())
    };
    let env = Env {
        home: &home,
        ensure: &ensure,
        account: &account,
        timing: quick(),
    };
    let mut asked = request(Source::Chat(info), Kind::Chat, HarnessKind::Codex);
    asked.account = Some("Work".into());
    let error = run(&env, asked, &|_| {}).err().unwrap();
    assert!(
        error.contains("did not start the chat on the Codex account"),
        "{error}"
    );
    // The chat that was made is gone, and was sent nothing.
    let mut client = crate::chat::client::Client::connect(&socket).unwrap();
    assert!(client.list().unwrap().is_empty());
    assert!(fake_for(&home.join("work")).commands().is_empty());
    drop(host);
    fs::remove_dir_all(home).unwrap();
}

// ---- The source ----------------------------------------------------------------------------

#[test]
fn the_caller_is_the_source_unless_it_says_otherwise() {
    let chat = "11111111-2222-3333-4444-555555555555";
    let shell = "66666666-7777-8888-9999-000000000000";
    assert_eq!(
        source_selector(Some("abcd1234"), Some(shell), Some(chat)).unwrap(),
        "abcd1234"
    );
    assert_eq!(source_selector(None, Some(shell), None).unwrap(), shell);
    assert_eq!(source_selector(None, None, Some(chat)).unwrap(), chat);
    // An empty variable is not set.
    assert_eq!(source_selector(None, Some(""), Some(chat)).unwrap(), chat);
    assert_eq!(
        source_selector(Some("  "), Some(shell), None).unwrap(),
        shell
    );
    let both = source_selector(None, Some(shell), Some(chat)).unwrap_err();
    assert!(
        both.contains("Both RIWORK_SHELL_ID and RIWORK_CHAT_ID") && both.contains("--from"),
        "{both}"
    );
    let none = source_selector(None, None, None).unwrap_err();
    assert!(none.starts_with("Pass --from"), "{none}");
}

#[test]
fn a_source_is_named_by_its_id_or_by_a_unique_start_of_it() {
    let home = short_home();
    let mut first = chat_info(None);
    first.id = "aaaaaaaa-1111-4111-8111-111111111111".into();
    let mut second = chat_info(None);
    second.id = "aaaaaaaa-2222-4222-8222-222222222222".into();
    let mut third = chat_info(None);
    third.id = "bbbbbbbb-3333-4333-8333-333333333333".into();
    for info in [&first, &second, &third] {
        write_chat(&home, (*info).clone(), Vec::new());
    }
    let id = |source: Result<Source, String>| source.unwrap().id().to_owned();
    assert_eq!(id(resolve_source(&home, &third.id)), third.id);
    assert_eq!(id(resolve_source(&home, "bbbbbbbb")), third.id);
    assert_eq!(id(resolve_source(&home, "aaaaaaaa-2")), second.id);
    assert_eq!(id(resolve_source(&home, &first.id)), first.id);
    let ambiguous = resolve_source(&home, "aaaaaaaa").err().unwrap();
    assert!(ambiguous.contains("matches more than one"), "{ambiguous}");
    for bad in ["aaaaaaa", "", "zzzzzzzz", "../../etc"] {
        let error = resolve_source(&home, bad).err().unwrap();
        assert!(error.contains("not a shell or chat id"), "{bad}: {error}");
    }
    let unknown = resolve_source(&home, "cccccccc").err().unwrap();
    assert!(
        unknown.contains("No shell or chat has the id cccccccc"),
        "{unknown}"
    );
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn deltas_of_a_running_chat_are_not_part_of_its_transcript_twice() {
    // A chat that streamed its reply: the log has the item and then its deltas.
    let home = short_home();
    let info = chat_info(None);
    let item = |text: &str, status| Item {
        presentation: Default::default(),
        id: "a1".into(),
        turn_id: Some("t".into()),
        status,
        body: ItemBody::AgentMessage { text: text.into() },
    };
    let id = write_chat(
        &home,
        info.clone(),
        vec![
            ChatEvent::Info { info },
            ChatEvent::ItemStarted {
                item: item("", ItemStatus::InProgress),
            },
            ChatEvent::ItemDelta {
                item_id: "a1".into(),
                delta: Delta::Text("once".into()),
            },
            ChatEvent::ItemCompleted {
                item: item("once", ItemStatus::Completed),
            },
        ],
    );
    let read = sources::read_chat(&home, &id).unwrap();
    let Body::Conversation(entries) = read.body else {
        panic!()
    };
    assert_eq!(entries, [document::Entry::Agent("once".into())]);
    fs::remove_dir_all(home).unwrap();
}
