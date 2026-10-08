//! Orchestrators in both modes, against a chat host with fake drivers and a
//! throwaway `RIWORK_HOME`. A terminal orchestrator here is a `sleep` in a tmux
//! server of its own, which is killed with the guard.
use super::*;
use crate::chat::model::ChatEvent;
use crate::chat::testing::*;
use crate::sessions::SessionManager;

const PROJECT: &str = "11111111-1111-4111-8111-111111111111";

fn ensure_in_process(home: &Path) -> Result<PathBuf, String> {
    Ok(socket_path(home))
}

fn chat_host(home: &Path) -> ChatHost<'_> {
    ChatHost {
        home,
        ensure: &ensure_in_process,
    }
}

/// Kills the tmux server a test's terminal orchestrator started.
struct Server(SessionManager);

impl Drop for Server {
    fn drop(&mut self) {
        self.0.kill_server();
    }
}

fn sessions(host: &TestHost) -> (SessionManager, Server) {
    let sessions = SessionManager::at(host.home.clone()).unwrap();
    let server = Server(sessions.clone());
    (sessions, server)
}

fn project_root(host: &TestHost) -> PathBuf {
    let root = host.home.join("project");
    fs::create_dir_all(&root).unwrap();
    root.canonicalize().unwrap()
}

fn id_of(orchestrator: &Orchestrator) -> &str {
    match orchestrator {
        Orchestrator::Terminal(shell) => &shell.id,
        Orchestrator::Chat(chat) => &chat.id,
    }
}

fn chat_of(created: (Orchestrator, bool)) -> (ChatInfo, bool) {
    match created {
        (Orchestrator::Chat(chat), created) => (chat, created),
        (other, _) => panic!("expected a chat orchestrator, got {other:?}"),
    }
}

fn global_chat(
    sessions: &SessionManager,
    host: &TestHost,
    runs: OrchestratorRuns,
) -> Result<(Orchestrator, bool), String> {
    create(
        sessions,
        &chat_host(&host.home),
        &OrchestratorScope::Global,
        None,
        host.home.clone(),
        None,
        runs,
    )
}

fn sent_texts(chat: &ChatInfo) -> Vec<String> {
    fake_for(&chat.cwd)
        .commands()
        .into_iter()
        .filter_map(|command| match command {
            ChatCommand::Send { text } => Some(text),
            _ => None,
        })
        .collect()
}

#[test]
fn a_global_chat_orchestrator_works_where_a_terminal_one_does_and_is_told_what_it_is() {
    let host = TestHost::new();
    let (sessions, _server) = sessions(&host);
    let (chat, created) =
        chat_of(global_chat(&sessions, &host, OrchestratorRuns::Chat(Provider::Codex)).unwrap());
    assert!(created);
    assert_eq!(chat.orchestrator, Some(OrchestratorScope::Global));
    assert_eq!(chat.provider, Provider::Codex);
    assert_eq!(chat.project_id, None);
    assert_eq!(chat.worktree_id, None);
    // The folder and the skill are the terminal orchestrator's.
    let context = host.home.join("orchestrator");
    assert_eq!(chat.cwd, context);
    let skill = sessions.orchestrator_skill_path_scoped(None).unwrap();
    assert!(skill.ends_with("orchestrator/.agents/skills/riwork-orchestrator/SKILL.md"));
    assert_eq!(
        fs::read_to_string(&skill).unwrap(),
        include_str!("../../skills/riwork-orchestrator/SKILL.md")
    );
    // The global orchestrator asks before commands and edits.
    assert_eq!(chat.approval_mode, ApprovalMode::Supervised);
    assert_eq!(chat.title, "G·ORCH · GLOBAL");
    assert_eq!(chat.codex_account_id.as_deref(), Some("account-a"));

    // The first message is the terminal's startup message: the scope, and the
    // skill to read rather than the skill itself.
    let messages = sent_texts(&chat);
    assert_eq!(messages.len(), 1);
    let first = &messages[0];
    assert!(first.starts_with("$riwork-orchestrator\n"), "{first}");
    assert!(first.contains("Scope: global."), "{first}");
    assert!(
        first.contains(&skill.to_string_lossy().into_owned()),
        "{first}"
    );
    assert!(!first.contains("<riwork-orchestrator-skill>"));
    assert!(first.len() < 4_000, "{} bytes", first.len());
    // It is the same builder.
    let executable = env::current_exe().unwrap();
    assert_eq!(
        *first,
        orchestrator_prompt(&skill, &executable, None, None, false)
    );
    // The skill is current, and the chat is a chat orchestrator in the host.
    assert!(chat_skill_is_current(&host.home, &chat));
    let settled = host.wait_for_state(&chat.id, |state| *state == ChatState::Idle);
    assert_eq!(chat_orchestrators(&chat_host(&host.home)), vec![settled]);
    // The agent knows which orchestrator it is, as a terminal one does.
    let starts = fake_for(&chat.cwd).starts.lock().unwrap().clone();
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0].approval_mode, ApprovalMode::Supervised);
    assert_eq!(starts[0].cwd, context);
}

#[test]
fn a_project_chat_orchestrator_belongs_to_its_project_and_never_asks() {
    let host = TestHost::new();
    let (sessions, _server) = sessions(&host);
    let root = project_root(&host);
    let scope = scope_of(Some(PROJECT));
    let (chat, created) = chat_of(
        create(
            &sessions,
            &chat_host(&host.home),
            &scope,
            Some(root.clone()),
            root.clone(),
            None,
            OrchestratorRuns::Chat(Provider::Claude),
        )
        .unwrap(),
    );
    assert!(created);
    assert_eq!(chat.provider, Provider::Claude);
    assert_eq!(chat.project_id.as_deref(), Some(PROJECT));
    assert_eq!(chat.orchestrator, Some(scope));
    assert_eq!(chat.approval_mode, ApprovalMode::Full);
    assert_eq!(chat.title, "P·ORCH · PROJECT");
    assert_eq!(
        chat.cwd,
        host.home.join("orchestrators/projects").join(PROJECT)
    );
    let skill = sessions
        .orchestrator_skill_path_scoped(Some(PROJECT))
        .unwrap();
    assert!(skill.is_file());
    // Claude chats have no Codex account.
    assert_eq!(chat.codex_account_id, None);
    let first = &sent_texts(&chat)[0];
    assert!(first.contains("Scope: project."), "{first}");
    assert!(first.contains(PROJECT), "{first}");
    assert!(
        first.contains(&root.to_string_lossy().into_owned()),
        "{first}"
    );
    assert_eq!(
        *first,
        orchestrator_prompt(
            &skill,
            &env::current_exe().unwrap(),
            Some(PROJECT),
            Some(root.as_path()),
            false
        )
    );
}

#[test]
#[ignore = "slow: starts a real tmux server for a terminal orchestrator"]
fn a_scope_has_one_orchestrator_across_modes_and_scopes_do_not_share() {
    let host = TestHost::new();
    let (sessions, _server) = sessions(&host);
    let chat_runs = OrchestratorRuns::Chat(Provider::Codex);
    let (first, created) = chat_of(global_chat(&sessions, &host, chat_runs).unwrap());
    assert!(created);

    // Creating again, in either mode, returns the chat and sends nothing more.
    for runs in [chat_runs, OrchestratorRuns::Terminal] {
        let (again, created) = chat_of(global_chat(&sessions, &host, runs).unwrap());
        assert!(!created);
        assert_eq!(again.id, first.id);
    }
    assert_eq!(sent_texts(&first).len(), 1);
    assert_eq!(host.client().list().unwrap().len(), 1);
    assert!(sessions.orchestrator_get().unwrap().is_none());
    // `find` agrees.
    let found = find(
        &sessions,
        &chat_host(&host.home),
        &OrchestratorScope::Global,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(found, Orchestrator::Chat(_)));
    assert_eq!(id_of(&found), first.id);

    // Another scope is another orchestrator.
    let root = project_root(&host);
    let (project, created) = chat_of(
        create(
            &sessions,
            &chat_host(&host.home),
            &scope_of(Some(PROJECT)),
            Some(root.clone()),
            root,
            None,
            chat_runs,
        )
        .unwrap(),
    );
    assert!(created);
    assert_ne!(project.id, first.id);
    assert_eq!(host.client().list().unwrap().len(), 2);

    // A terminal orchestrator in a scope keeps a chat out of it, in either order.
    let other = "22222222-2222-4222-8222-222222222222";
    let other_root = host.home.join("other");
    fs::create_dir_all(&other_root).unwrap();
    let terminal = sessions
        .orchestrator_create_for_project(other.into(), other_root.clone(), Some("sleep 600".into()))
        .unwrap();
    let (found, created) = create(
        &sessions,
        &chat_host(&host.home),
        &scope_of(Some(other)),
        Some(other_root),
        host.home.clone(),
        None,
        chat_runs,
    )
    .unwrap();
    assert!(!created);
    assert_eq!(found, Orchestrator::Terminal(terminal.clone()));
    assert_eq!(host.client().list().unwrap().len(), 2, "no third chat");
    assert_eq!(
        find(&sessions, &chat_host(&host.home), &scope_of(Some(other))).unwrap(),
        Some(Orchestrator::Terminal(terminal))
    );
}

#[test]
fn creators_that_race_make_one_chat_and_one_first_message() {
    let host = TestHost::new();
    let (sessions, _server) = sessions(&host);
    let home = host.home.clone();
    let results: Vec<_> = std::thread::scope(|scope| {
        (0..4)
            .map(|_| {
                scope.spawn(|| {
                    create(
                        &sessions,
                        &chat_host(&home),
                        &OrchestratorScope::Global,
                        None,
                        home.clone(),
                        None,
                        OrchestratorRuns::Chat(Provider::Codex),
                    )
                    .map(chat_of)
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|handle| handle.join().unwrap().unwrap())
            .collect()
    });
    assert_eq!(results.iter().filter(|(_, created)| *created).count(), 1);
    let id = &results[0].0.id;
    assert!(results.iter().all(|(chat, _)| &chat.id == id));
    assert_eq!(host.client().list().unwrap().len(), 1);
    assert_eq!(sent_texts(&results[0].0).len(), 1);
}

#[test]
#[ignore = "slow: starts a real tmux server for a terminal orchestrator"]
fn a_dead_terminal_orchestrator_makes_room_for_a_chat() {
    let host = TestHost::new();
    let (sessions, _server) = sessions(&host);
    let terminal = sessions
        .orchestrator_create(host.home.clone(), Some("sleep 600".into()))
        .unwrap();
    sessions.close(&terminal.id).unwrap();
    // Closing removes the row; put a dead one back, as a crashed tmux leaves it.
    let registry = host.home.join("sessions.json");
    let mut value: Value = serde_json::from_slice(&fs::read(&registry).unwrap()).unwrap();
    assert!(value["sessions"].as_array().unwrap().is_empty());
    value["sessions"] = json!([serde_json::to_value(&terminal).unwrap()]);
    fs::write(&registry, serde_json::to_vec(&value).unwrap()).unwrap();
    let dead = sessions.orchestrator_get().unwrap().unwrap();
    assert!(!dead.alive);

    let (chat, created) =
        chat_of(global_chat(&sessions, &host, OrchestratorRuns::Chat(Provider::Codex)).unwrap());
    assert!(created);
    assert!(
        sessions.orchestrator_get().unwrap().is_none(),
        "the dead row is gone"
    );
    let found = find(
        &sessions,
        &chat_host(&host.home),
        &OrchestratorScope::Global,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(found, Orchestrator::Chat(_)));
    assert_eq!(id_of(&found), chat.id);
}

#[test]
#[ignore = "slow: starts a real tmux server for a terminal orchestrator"]
fn a_custom_command_is_a_terminal_whatever_the_setting_says() {
    let host = TestHost::new();
    let (sessions, _server) = sessions(&host);
    let (found, created) = create(
        &sessions,
        &chat_host(&host.home),
        &OrchestratorScope::Global,
        None,
        host.home.clone(),
        Some("sleep 600".into()),
        OrchestratorRuns::Chat(Provider::Codex),
    )
    .unwrap();
    assert!(created);
    assert!(matches!(found, Orchestrator::Terminal(_)), "{found:?}");
    assert!(host.client().list().unwrap().is_empty());
}

#[test]
fn a_chat_that_cannot_start_is_not_left_behind() {
    let host = TestHost::new();
    let (sessions, _server) = sessions(&host);
    let context = orchestrator_context(&host.home, None);
    fs::create_dir_all(&context).unwrap();
    *fake_for(&context).fail_start.lock().unwrap() = Some("codex is not installed".into());
    let error = global_chat(&sessions, &host, OrchestratorRuns::Chat(Provider::Codex)).unwrap_err();
    assert!(
        error.contains("could not start") && error.contains("codex is not installed"),
        "{error}"
    );
    assert!(host.client().list().unwrap().is_empty());
    assert!(
        !orchestrator_context(&host.home, None)
            .join("chat-skill.json")
            .exists()
    );
    // The next try starts clean.
    let (chat, created) =
        chat_of(global_chat(&sessions, &host, OrchestratorRuns::Chat(Provider::Codex)).unwrap());
    assert!(created);
    let settled = host.wait_for_state(&chat.id, |state| *state == ChatState::Idle);
    assert_eq!(host.client().list().unwrap(), vec![settled]);
}

#[test]
fn closing_a_chat_orchestrator_deletes_its_chat_and_the_next_one_starts_afresh() {
    let host = TestHost::new();
    let (sessions, _server) = sessions(&host);
    let runs = OrchestratorRuns::Chat(Provider::Codex);
    let (first, _) = chat_of(global_chat(&sessions, &host, runs).unwrap());
    close(&chat_host(&host.home), &first).unwrap();
    assert!(host.client().list().unwrap().is_empty());
    assert!(!host.home.join("chats").join(&first.id).exists());
    assert_eq!(chat_skill_version(&host.home, &first), None);
    assert!(
        find(
            &sessions,
            &chat_host(&host.home),
            &OrchestratorScope::Global
        )
        .unwrap()
        .is_none()
    );

    let (second, created) = chat_of(global_chat(&sessions, &host, runs).unwrap());
    assert!(created);
    assert_ne!(second.id, first.id);
    assert_eq!(
        sent_texts(&second).len(),
        2,
        "one startup message per chat, on a shared fake"
    );
}

#[test]
fn chat_orchestrators_are_found_from_disk_while_no_host_runs() {
    let home = short_home();
    let chat = ChatInfo {
        id: Uuid::new_v4().to_string(),
        provider: Provider::Claude,
        project_id: Some(PROJECT.into()),
        worktree_id: None,
        cwd: home.clone(),
        title: "P·ORCH · PROJECT".into(),
        created_at_unix: 7,
        provider_thread_id: None,
        model: None,
        effort: None,
        approval_mode: ApprovalMode::Full,
        codex_account_id: None,
        state: ChatState::Running,
        orchestrator: Some(scope_of(Some(PROJECT))),
        fast: false,
        carried_over: None,
    };
    let plain = ChatInfo {
        id: Uuid::new_v4().to_string(),
        orchestrator: None,
        ..chat.clone()
    };
    for info in [&chat, &plain] {
        chat::log::ChatLog::create(&chat::log::chat_dir(&home, &info.id).unwrap(), info).unwrap();
    }
    let found = chat_orchestrators(&chat_host(&home));
    // Nothing runs without a host, so a chat that was at work is stopped.
    assert_eq!(
        found,
        vec![ChatInfo {
            state: ChatState::Stopped,
            ..chat.clone()
        }]
    );
    let _ = fs::remove_dir_all(home);
}

#[test]
fn the_entry_of_a_chat_orchestrator_has_the_usual_fields_and_the_three_that_name_it() {
    let host = TestHost::new();
    let (sessions, _server) = sessions(&host);
    let (chat, _) =
        chat_of(global_chat(&sessions, &host, OrchestratorRuns::Chat(Provider::Codex)).unwrap());
    let chat = host.wait_for_state(&chat.id, |state| *state == ChatState::Idle);
    let entry = chat_entry(&host.home, &chat);
    assert_eq!(entry["id"], chat.id);
    assert_eq!(entry["mode"], "chat");
    assert_eq!(entry["chat_id"], chat.id);
    assert_eq!(entry["provider"], "codex");
    assert_eq!(entry["harness"], "codex");
    assert_eq!(entry["kind"], "orchestrator");
    assert_eq!(entry["project_id"], Value::Null);
    assert_eq!(entry["worktree_id"], Value::Null);
    assert_eq!(entry["command"], Value::Null);
    assert_eq!(entry["alive"], true);
    assert_eq!(entry["unrestricted"], false);
    assert_eq!(entry["state"], "idle");
    assert_eq!(entry["activity"], "done");
    assert_eq!(entry["created_at_unix"], chat.created_at_unix);
    assert_eq!(entry["cwd"], chat.cwd.to_string_lossy().as_ref());
    assert_eq!(entry["orchestrator_skill_loaded"], true);
    assert_eq!(
        entry["orchestrator_skill_version"],
        orchestrator_skill_version()
    );
    assert!(entry["last_activity_unix"].as_u64().unwrap() > 0);
    assert!(entry.get("state_message").is_none());

    // A stopped chat is still an orchestrator that a message resumes.
    host.client().close(&chat.id).unwrap();
    let stopped = chat_entry(&host.home, &host.info(&chat.id));
    assert_eq!(
        (stopped["alive"].clone(), stopped["state"].clone()),
        (json!(true), json!("stopped"))
    );
    assert_eq!(stopped["activity"], "unknown");
    // A failed one says why.
    let failed = ChatInfo {
        state: ChatState::Failed {
            message: "gone".into(),
        },
        ..chat.clone()
    };
    let entry = chat_entry(&host.home, &failed);
    assert_eq!(
        (entry["state"].clone(), entry["state_message"].clone()),
        (json!("failed"), json!("gone"))
    );
    assert_eq!(entry["activity"], "exited");
}

#[test]
fn load_skill_sends_the_full_skill_once_and_only_to_an_idle_chat() {
    let host = TestHost::new();
    let (sessions, _server) = sessions(&host);
    let (chat, _) =
        chat_of(global_chat(&sessions, &host, OrchestratorRuns::Chat(Provider::Codex)).unwrap());
    let chat_host = chat_host(&host.home);
    host.wait_for_state(&chat.id, |state| *state == ChatState::Idle);
    // The skill it started with is the current one: nothing to load.
    load_chat_skill(&sessions, &chat_host, &chat, None).unwrap();
    assert_eq!(sent_texts(&chat).len(), 1);

    // After an update of the app the skill it has is an old one.
    let mark = skill_mark_path(&host.home, None);
    fs::write(
        &mark,
        serde_json::to_vec(&SkillMark {
            chat_id: chat.id.clone(),
            version: "0000000000000000".into(),
        })
        .unwrap(),
    )
    .unwrap();
    assert!(!chat_skill_is_current(&host.home, &chat));
    let entry = chat_entry(&host.home, &chat);
    assert_eq!(entry["orchestrator_skill_version"], "0000000000000000");

    // It is not loaded into a turn that is under way.
    send(&chat_host, &chat, "hang").unwrap();
    host.wait_for_state(&chat.id, |state| *state == ChatState::Running);
    let error = load_chat_skill(&sessions, &chat_host, &chat, None).unwrap_err();
    assert!(error.contains("busy"), "{error}");
    assert!(!chat_skill_is_current(&host.home, &chat));
    fake_for(&chat.cwd).emit(ChatEvent::State {
        state: ChatState::Idle,
    });
    host.wait_for_state(&chat.id, |state| *state == ChatState::Idle);

    // Idle again: the whole skill goes to the chat as a message.
    let root = project_root(&host);
    load_chat_skill(&sessions, &chat_host, &chat, Some(root.as_path())).unwrap();
    let sent = sent_texts(&chat);
    let message = sent.last().unwrap();
    assert!(message.contains("<riwork-orchestrator-skill>"));
    assert!(message.contains(include_str!("../../skills/riwork-orchestrator/SKILL.md")));
    assert!(chat_skill_is_current(&host.home, &chat));
    // And it is not sent twice.
    load_chat_skill(&sessions, &chat_host, &chat, None).unwrap();
    assert_eq!(sent_texts(&chat).len(), sent.len());
}
