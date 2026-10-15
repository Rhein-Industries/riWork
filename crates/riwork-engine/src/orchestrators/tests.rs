//! Orchestrators in both modes, against a chat host with fake drivers and a
//! throwaway `RIWORK_HOME`. A terminal orchestrator here is a `sleep` in a tmux
//! server of its own, which is killed with the guard.
use super::*;
use crate::chat::host::Providers;
use crate::chat::model::ChatEvent;
use crate::chat::testing::*;
use crate::sessions::SessionManager;
use crate::store::Store;

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

fn chat_of(created: Created) -> (ChatInfo, bool) {
    match created.orchestrator {
        Orchestrator::Chat(chat) => (chat, created.created),
        other => panic!("expected a chat orchestrator, got {other:?}"),
    }
}

fn global_chat(
    sessions: &SessionManager,
    host: &TestHost,
    runs: OrchestratorRuns,
) -> Result<Created, String> {
    create(
        sessions,
        &chat_host(&host.home),
        &OrchestratorScope::Global,
        None,
        host.home.clone(),
        None,
        &runs,
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
        chat_of(global_chat(&sessions, &host, OrchestratorRuns::chat(Provider::Codex)).unwrap());
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
    assert_eq!(chat.title, "Global orchestrator");
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
        orchestrator_prompt(
            &skill,
            &executable,
            None,
            None,
            &Delegation::built_in(),
            false
        )
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
            &OrchestratorRuns::chat(Provider::Claude),
        )
        .unwrap(),
    );
    assert!(created);
    assert_eq!(chat.provider, Provider::Claude);
    assert_eq!(chat.project_id.as_deref(), Some(PROJECT));
    assert_eq!(chat.orchestrator, Some(scope));
    assert_eq!(chat.approval_mode, ApprovalMode::Full);
    assert_eq!(chat.title, "Project orchestrator");
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
            &Delegation::built_in(),
            false
        )
    );
}

#[test]
#[ignore = "slow: starts a real tmux server for a terminal orchestrator"]
fn a_scope_has_one_orchestrator_across_modes_and_scopes_do_not_share() {
    let host = TestHost::new();
    let (sessions, _server) = sessions(&host);
    let chat_runs = OrchestratorRuns::chat(Provider::Codex);
    let (first, created) = chat_of(global_chat(&sessions, &host, chat_runs.clone()).unwrap());
    assert!(created);

    // Creating again, in either mode, returns the chat and sends nothing more.
    for runs in [chat_runs.clone(), OrchestratorRuns::Terminal] {
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
            &chat_runs,
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
    let Created {
        orchestrator: found,
        created,
        ..
    } = create(
        &sessions,
        &chat_host(&host.home),
        &scope_of(Some(other)),
        Some(other_root),
        host.home.clone(),
        None,
        &chat_runs,
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
                        &OrchestratorRuns::chat(Provider::Codex),
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
        chat_of(global_chat(&sessions, &host, OrchestratorRuns::chat(Provider::Codex)).unwrap());
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
    let Created {
        orchestrator: found,
        created,
        ..
    } = create(
        &sessions,
        &chat_host(&host.home),
        &OrchestratorScope::Global,
        None,
        host.home.clone(),
        Some("sleep 600".into()),
        &OrchestratorRuns::chat(Provider::Codex),
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
    let error = global_chat(&sessions, &host, OrchestratorRuns::chat(Provider::Codex)).unwrap_err();
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
        chat_of(global_chat(&sessions, &host, OrchestratorRuns::chat(Provider::Codex)).unwrap());
    assert!(created);
    let settled = host.wait_for_state(&chat.id, |state| *state == ChatState::Idle);
    assert_eq!(host.client().list().unwrap(), vec![settled]);
}

#[test]
fn closing_a_chat_orchestrator_deletes_its_chat_and_the_next_one_starts_afresh() {
    let host = TestHost::new();
    let (sessions, _server) = sessions(&host);
    let runs = OrchestratorRuns::chat(Provider::Codex);
    let (first, _) = chat_of(global_chat(&sessions, &host, runs.clone()).unwrap());
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
        parent_id: None,
        user_title: None,
        first_user_message: None,
        provider_title: None,
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
        contributor_mode: Default::default(),
        contributor_opt_out: false,
        claude_billing: None,
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
        chat_of(global_chat(&sessions, &host, OrchestratorRuns::chat(Provider::Codex)).unwrap());
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
        chat_of(global_chat(&sessions, &host, OrchestratorRuns::chat(Provider::Codex)).unwrap());
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
            delegation: None,
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

#[test]
fn the_shared_inventory_keeps_ordinary_and_stopped_chats_while_orchestrators_stay_filtered() {
    let host = TestHost::new();
    let mut request = host.new_chat(Provider::Codex);
    request.project_id = Some(PROJECT.into());
    let chat = host.client().create(request).unwrap();
    host.client().close(&chat.id).unwrap();
    let inventory = running_chats(&chat_host(&host.home)).unwrap();
    assert_eq!(inventory.len(), 1);
    assert_eq!(inventory[0].id, chat.id);
    assert_eq!(inventory[0].state, ChatState::Stopped);
    assert!(
        running_chat_orchestrators(&chat_host(&host.home))
            .unwrap()
            .is_empty()
    );
}

fn starts(chat: &ChatInfo) -> Vec<(Provider, Option<String>)> {
    fake_for(&chat.cwd)
        .starts
        .lock()
        .unwrap()
        .iter()
        .map(|start| (start.provider, start.model.clone()))
        .collect()
}

fn choose(host: &TestHost, runs: &OrchestratorRuns) {
    SettingsStore::open(&host.home)
        .unwrap()
        .update(|settings| settings.set_orchestrator_runs(runs))
        .unwrap();
}

fn create_planned(
    sessions: &SessionManager,
    home: &Path,
    project: Option<(&str, &Path)>,
) -> Result<Created, String> {
    let plan = plan(home, project.map(|(id, _)| id))?;
    let root = project.map(|(_, root)| root.to_path_buf());
    create(
        sessions,
        &chat_host(home),
        &scope_of(project.map(|(id, _)| id)),
        root.clone(),
        root.unwrap_or_else(|| home.to_path_buf()),
        None,
        &plan.runs,
    )
}

const WONDER_FULL: &str = "muse-wonder-full-1.0";
const SOL: &str = "gpt-6.1-sol";

#[test]
fn fresh_settings_start_a_muse_chat_on_wonder_full() {
    let host = TestHost::new();
    let (sessions, _server) = sessions(&host);
    // Nothing saved: Settings say Automatic, and the global scope follows them.
    let planned = plan(&host.home, None).unwrap();
    assert_eq!(planned.runs, OrchestratorRuns::Automatic);
    assert_eq!(planned.source, PlanSource::Settings);
    assert!(planned.notes.is_empty());

    let made = create_planned(&sessions, &host.home, None).unwrap();
    assert!(made.created);
    assert_eq!(made.fallback, None);
    let (chat, _) = chat_of(made);
    assert_eq!(chat.provider, Provider::Muse);
    assert_eq!(chat.model.as_deref(), Some(WONDER_FULL));
    // The model reached the provider's start, and the entry says it.
    assert_eq!(
        starts(&chat),
        [(Provider::Muse, Some(WONDER_FULL.to_owned()))]
    );
    // Under the secure default the Muse orchestrator runs with contributor mode off.
    let started = fake_for(&chat.cwd).starts.lock().unwrap().clone();
    assert_eq!(
        started[0].contributor_mode,
        crate::chat::model::ContributorMode::Off
    );
    assert_eq!(
        chat.contributor_mode,
        crate::chat::model::ContributorMode::Off
    );
    let entry = chat_entry(&host.home, &chat);
    assert_eq!(
        (entry["provider"].clone(), entry["model"].clone()),
        (json!("muse"), json!(WONDER_FULL))
    );
    // It was told what it is, with the skill, as any orchestrator is.
    assert_eq!(sent_texts(&chat).len(), 1);
    assert!(chat_skill_is_current(&host.home, &chat));
}

#[test]
fn when_muse_cannot_start_automatic_runs_codex_on_sol_and_says_why() {
    let host = TestHost::with_providers(
        quick_options(),
        Providers {
            muse: missing_start,
            ..fake_providers()
        },
    );
    let (sessions, _server) = sessions(&host);
    let made = create_planned(&sessions, &host.home, None).unwrap();
    let fallback = made.fallback.clone().expect("a fallback is disclosed");
    assert!(
        fallback.contains("Muse chat on muse-wonder-full-1.0 did not start"),
        "{fallback}"
    );
    assert!(
        fallback.contains("muse is not installed or is not on PATH"),
        "{fallback}"
    );
    assert!(fallback.contains("Codex chat on gpt-6.1-sol"), "{fallback}");
    let (chat, created) = chat_of(made);
    assert!(created);
    assert_eq!(
        (chat.provider, chat.model.as_deref()),
        (Provider::Codex, Some(SOL))
    );
    // Muse was tried first, and its chat is not left behind.
    assert_eq!(
        starts(&chat),
        [
            (Provider::Muse, Some(WONDER_FULL.to_owned())),
            (Provider::Codex, Some(SOL.to_owned()))
        ]
    );
    let listed = host.client().list().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, chat.id);
    assert_eq!(sent_texts(&chat).len(), 1, "one startup message, to Codex");
}

#[test]
fn when_neither_can_start_the_error_names_both_and_nothing_is_left() {
    let host = TestHost::with_providers(
        quick_options(),
        Providers {
            muse: missing_start,
            codex: missing_start,
            ..fake_providers()
        },
    );
    let (sessions, _server) = sessions(&host);
    let error = create_planned(&sessions, &host.home, None).unwrap_err();
    assert!(error.starts_with("No orchestrator could start."), "{error}");
    assert!(error.contains("muse is not installed"), "{error}");
    assert!(error.contains("codex is not installed"), "{error}");
    assert!(
        error.contains(WONDER_FULL) && error.contains(SOL),
        "{error}"
    );
    assert!(host.client().list().unwrap().is_empty());
    assert!(!skill_mark_path(&host.home, None).exists());
}

#[test]
fn explicit_settings_win_and_an_explicit_chat_that_cannot_start_is_not_replaced() {
    let host = TestHost::with_providers(
        quick_options(),
        Providers {
            muse: missing_start,
            ..fake_providers()
        },
    );
    let (sessions, _server) = sessions(&host);
    // An explicit Muse chat that cannot start is an error, never a Codex chat.
    choose(
        &host,
        &OrchestratorRuns::Chat {
            provider: Provider::Muse,
            model: Some(WONDER_FULL.into()),
        },
    );
    let error = create_planned(&sessions, &host.home, None).unwrap_err();
    assert!(
        error.contains("could not start") && error.contains("muse"),
        "{error}"
    );
    assert!(host.client().list().unwrap().is_empty());

    // An explicit Claude chat on Opus is what starts, though Automatic would not.
    let opus = OrchestratorRuns::Chat {
        provider: Provider::Claude,
        model: Some("opus".into()),
    };
    choose(&host, &opus);
    let planned = plan(&host.home, None).unwrap();
    assert_eq!(
        (planned.runs.clone(), planned.source),
        (opus, PlanSource::Settings)
    );
    let made = create_planned(&sessions, &host.home, None).unwrap();
    assert_eq!(made.fallback, None);
    let (chat, _) = chat_of(made);
    assert_eq!(
        (chat.provider, chat.model.as_deref()),
        (Provider::Claude, Some("opus"))
    );
    assert_eq!(
        starts(&chat).last().unwrap(),
        &(Provider::Claude, Some("opus".to_owned()))
    );
    // An explicit chat on the provider's own model sends none.
    close(&chat_host(&host.home), &chat).unwrap();
    choose(&host, &OrchestratorRuns::chat(Provider::Codex));
    let (chat, _) = chat_of(create_planned(&sessions, &host.home, None).unwrap());
    assert_eq!((chat.provider, chat.model.clone()), (Provider::Codex, None));
}

#[test]
fn a_project_choice_applies_to_its_own_orchestrator_only_and_inherit_follows_settings() {
    let host = TestHost::new();
    let (sessions, _server) = sessions(&host);
    let store = Store::open(&host.home).unwrap();
    let mine = host.home.join("mine");
    let theirs = host.home.join("theirs");
    fs::create_dir_all(&mine).unwrap();
    fs::create_dir_all(&theirs).unwrap();
    let mine = store.add_project(&mine, Some("Mine")).unwrap();
    let theirs = store.add_project(&theirs, Some("Theirs")).unwrap();
    // Inherit is what a project has unless it chooses.
    assert_eq!(theirs.orchestrator, ProjectOrchestrator::Inherit);
    let opus = OrchestratorRuns::Chat {
        provider: Provider::Claude,
        model: Some("opus".into()),
    };
    store
        .set_project_orchestrator(&mine.id, ProjectOrchestrator::Choose(opus.clone()))
        .unwrap();

    let planned = plan(&host.home, Some(&mine.id)).unwrap();
    assert_eq!(
        (planned.runs.clone(), planned.source),
        (opus.clone(), PlanSource::Project)
    );
    for scope in [Some(theirs.id.as_str()), None] {
        let planned = plan(&host.home, scope).unwrap();
        assert_eq!(
            (planned.runs, planned.source),
            (OrchestratorRuns::Automatic, PlanSource::Settings)
        );
    }
    // The project's choice wins over explicit Settings too, in its scope only.
    choose(&host, &OrchestratorRuns::Terminal);
    assert_eq!(plan(&host.home, Some(&mine.id)).unwrap().runs, opus);
    assert_eq!(
        plan(&host.home, Some(&theirs.id)).unwrap().runs,
        OrchestratorRuns::Terminal
    );
    assert_eq!(
        plan(&host.home, None).unwrap().runs,
        OrchestratorRuns::Terminal
    );
    choose(&host, &OrchestratorRuns::Automatic);

    // Created through the plan, each project gets its own.
    let made = create_planned(&sessions, &host.home, Some((&mine.id, &mine.root))).unwrap();
    let (chat, _) = chat_of(made);
    assert_eq!(
        (chat.provider, chat.model.as_deref()),
        (Provider::Claude, Some("opus"))
    );
    assert_eq!(chat.project_id.as_deref(), Some(mine.id.as_str()));
    let (chat, _) =
        chat_of(create_planned(&sessions, &host.home, Some((&theirs.id, &theirs.root))).unwrap());
    assert_eq!(
        (chat.provider, chat.model.as_deref()),
        (Provider::Muse, Some(WONDER_FULL))
    );

    // A choice this build cannot read is passed over, and said to be.
    let unreadable = ProjectOrchestrator::Unreadable(json!({"mode": "hologram"}));
    store
        .set_project_orchestrator(&theirs.id, unreadable)
        .unwrap();
    let planned = plan(&host.home, Some(&theirs.id)).unwrap();
    assert_eq!(
        (planned.runs, planned.source),
        (OrchestratorRuns::Automatic, PlanSource::Settings)
    );
    assert_eq!(planned.notes.len(), 1);
    assert!(planned.notes[0].contains("hologram"), "{:?}", planned.notes);
    assert!(plan(&host.home, Some("missing-project")).is_err());
}

/// Each scope is told the delegation policy it resolves to when it starts and when it
/// loads its skill: a project's own choices over Settings' over RiWork's, field by field.
/// A change reaches no running conversation until LOAD SKILL.
#[test]
fn each_scope_is_told_its_resolved_delegation_policy_at_start_and_on_load_skill() {
    use crate::delegation::{DelegationPolicy, Route, Work};
    let host = TestHost::new();
    let (sessions, _server) = sessions(&host);
    let store = Store::open(&host.home).unwrap();
    let mine = host.home.join("mine");
    let theirs = host.home.join("theirs");
    fs::create_dir_all(&mine).unwrap();
    fs::create_dir_all(&theirs).unwrap();
    let mine = store.add_project(&mine, Some("Mine")).unwrap();
    let theirs = store.add_project(&theirs, Some("Theirs")).unwrap();
    // Nothing chosen anywhere: RiWork's own policy, today's routing.
    assert_eq!(delegation(&host.home, None), Delegation::built_in());
    assert_eq!(
        delegation(&host.home, Some(&mine.id)),
        Delegation::built_in()
    );

    let settings = SettingsStore::open(&host.home).unwrap();
    settings
        .update(|settings| {
            let policy = &mut settings.delegation_policy;
            policy.set_spread_work(Some(false));
            policy.set_parallel_shells(Some(6));
            policy
                .set_route(Work::Workhorse, Some(Route::recommended(Provider::Muse)))
                .unwrap();
        })
        .unwrap();
    store
        .set_project_delegation_policy(&mine.id, |policy| {
            policy.set_spread_work(Some(true));
            policy.set_parallel_shells(Some(2));
            policy
                .set_route(Work::Design, Some(Route::recommended(Provider::Codex)))
                .unwrap();
        })
        .unwrap();
    let global_policy = "Delegation policy for this scope (Settings, else RiWork's default, \
                         as they stood when this message was written): spread work off; \
                         about 6 parallel worker shells; design work on claude, model opus; \
                         workhorse work on muse, model muse-wonder-full-1.0.";
    let mine_policy = "Delegation policy for this scope (this project's override, else Settings, \
                       else RiWork's default, as they stood when this message was written): \
                       spread work on; about 2 parallel worker shells; design work on codex, \
                       model gpt-6.1-sol; workhorse work on muse, model muse-wonder-full-1.0.";
    let theirs_policy = "Delegation policy for this scope (this project's override, else \
                         Settings, else RiWork's default, as they stood when this message was \
                         written): spread work off; about 6 parallel worker shells; design \
                         work on claude, model opus; workhorse work on muse, model \
                         muse-wonder-full-1.0.";
    let (global, _) = chat_of(create_planned(&sessions, &host.home, None).unwrap());
    let (mine_chat, _) =
        chat_of(create_planned(&sessions, &host.home, Some((&mine.id, &mine.root))).unwrap());
    let (theirs_chat, _) =
        chat_of(create_planned(&sessions, &host.home, Some((&theirs.id, &theirs.root))).unwrap());
    for (chat, policy) in [
        (&global, global_policy),
        (&mine_chat, mine_policy),
        (&theirs_chat, theirs_policy),
    ] {
        let first = &sent_texts(chat)[0];
        assert!(first.contains(policy), "{first}");
        assert!(first.contains("Fan out and relay"), "{first}");
    }
    // A project the store cannot name adds nothing to Settings.
    assert_eq!(
        delegation(&host.home, Some(PROJECT)),
        delegation(&host.home, None)
    );

    // Changing the policy tells no running orchestrator anything...
    settings
        .update(|settings| settings.delegation_policy = DelegationPolicy::default())
        .unwrap();
    store
        .set_project_delegation_policy(&mine.id, |policy| policy.set_parallel_shells(Some(9)))
        .unwrap();
    for chat in [&global, &mine_chat, &theirs_chat] {
        assert_eq!(
            sent_texts(chat).len(),
            1,
            "a running conversation is not rewritten"
        );
    }
    // ...until LOAD SKILL, which tells each scope what it resolves to now.
    for (chat, scope, root, policy) in [
        (
            &global,
            None,
            None,
            "spread work on; about 4 parallel worker shells; design work on claude, model opus; \
             workhorse work on codex, model gpt-6.1-sol.",
        ),
        (
            &mine_chat,
            Some(mine.id.as_str()),
            Some(mine.root.as_path()),
            "spread work on; about 9 parallel worker shells; design work on codex, model \
             gpt-6.1-sol; workhorse work on codex, model gpt-6.1-sol.",
        ),
    ] {
        host.wait_for_state(&chat.id, |state| *state == ChatState::Idle);
        fs::write(
            skill_mark_path(&host.home, scope),
            serde_json::to_vec(&SkillMark {
                chat_id: chat.id.clone(),
                version: "0000000000000000".into(),
                delegation: None,
            })
            .unwrap(),
        )
        .unwrap();
        load_chat_skill(&sessions, &chat_host(&host.home), chat, root).unwrap();
        let loaded = sent_texts(chat).last().unwrap().clone();
        assert!(loaded.contains(policy), "{loaded}");
        assert!(loaded.contains("<riwork-orchestrator-skill>"));
    }

    // Settings this build cannot read are RiWork's own policy, and never an error.
    fs::write(host.home.join("settings.json"), "{not json").unwrap();
    assert_eq!(delegation(&host.home, None), Delegation::built_in());
    assert_eq!(
        delegation(&host.home, Some(&mine.id)).parallel_shells,
        9,
        "the project's choice still stands"
    );
}

#[test]
fn an_existing_orchestrator_is_reused_as_it_runs_whatever_the_settings_say_now() {
    let host = TestHost::new();
    let (sessions, _server) = sessions(&host);
    let (first, created) = chat_of(create_planned(&sessions, &host.home, None).unwrap());
    assert!(created);
    assert_eq!(first.provider, Provider::Muse);
    for runs in [
        OrchestratorRuns::Chat {
            provider: Provider::Claude,
            model: Some("opus".into()),
        },
        OrchestratorRuns::chat(Provider::Codex),
        OrchestratorRuns::Automatic,
    ] {
        choose(&host, &runs);
        let made = create_planned(&sessions, &host.home, None).unwrap();
        assert_eq!(made.fallback, None);
        let (again, created) = chat_of(made);
        assert!(!created);
        assert_eq!(again.id, first.id);
        assert_eq!(
            (again.provider, again.model.as_deref()),
            (Provider::Muse, Some(WONDER_FULL))
        );
    }
    assert_eq!(starts(&first).len(), 1, "never started again");
    assert_eq!(sent_texts(&first).len(), 1, "never told again");
    assert_eq!(host.client().list().unwrap().len(), 1);
}

#[test]
fn both_scopes_are_given_the_skill_with_model_routing_and_dashboard_briefs() {
    let skill = include_str!("../../skills/riwork-orchestrator/SKILL.md");
    for needle in [
        "## Model routing",
        "`muse`, `muse-wonder-full-1.0`",
        "`claude`, `opus`",
        "`codex`, `gpt-6.1-sol`",
        "riwork chat new --provider",
        "riwork shell create --worktree WORKTREE_ID --harness codex|claude|muse|grok --model MODEL",
        "riwork handoff --from SHELL_OR_CHAT_ID",
        "riwork chat list --project PROJECT_ID --json",
        "Never fall back silently",
        "## Dashboard briefs",
        "RIWORK BRIEF REQUEST",
        "END RIWORK BRIEF",
        "Never assigns work directly to a project's workers",
        "without delegating",
    ] {
        assert!(skill.contains(needle), "the skill lacks {needle:?}");
    }
    let host = TestHost::new();
    let (sessions, _server) = sessions(&host);
    let root = project_root(&host);
    let (global, _) = chat_of(create_planned(&sessions, &host.home, None).unwrap());
    // A project's orchestrator, as Automatic makes it (PROJECT is not in the store).
    let (project, _) = chat_of(
        create(
            &sessions,
            &chat_host(&host.home),
            &scope_of(Some(PROJECT)),
            Some(root.clone()),
            root.clone(),
            None,
            &OrchestratorRuns::Automatic,
        )
        .unwrap(),
    );
    for (chat, scope) in [(&global, None), (&project, Some(PROJECT))] {
        let installed = sessions.orchestrator_skill_path_scoped(scope).unwrap();
        assert_eq!(fs::read_to_string(&installed).unwrap(), skill);
        assert!(sent_texts(chat)[0].contains(&installed.to_string_lossy().into_owned()));
        assert!(chat_skill_is_current(&host.home, chat));
        // LOAD SKILL after an update sends this text, whole, to either scope.
        host.wait_for_state(&chat.id, |state| *state == ChatState::Idle);
        fs::write(
            skill_mark_path(&host.home, scope),
            serde_json::to_vec(&SkillMark {
                chat_id: chat.id.clone(),
                version: "0000000000000000".into(),
                delegation: None,
            })
            .unwrap(),
        )
        .unwrap();
        load_chat_skill(
            &sessions,
            &chat_host(&host.home),
            chat,
            Some(root.as_path()),
        )
        .unwrap();
        let loaded = sent_texts(chat).last().unwrap().clone();
        assert!(loaded.contains("## Model routing") && loaded.contains("## Dashboard briefs"));
        assert!(loaded.contains(skill));
        assert!(chat_skill_is_current(&host.home, chat));
    }
}

/// A change of the delegation policy alone, with the skill as it was, makes a running
/// orchestrator out of date, and LOAD SKILL tells it the policy now.
#[test]
fn load_skill_brings_a_policy_change_to_a_chat_whose_skill_is_current() {
    let host = TestHost::new();
    let (sessions, _server) = sessions(&host);
    let chat_host = chat_host(&host.home);
    let (chat, _) =
        chat_of(global_chat(&sessions, &host, OrchestratorRuns::chat(Provider::Codex)).unwrap());
    host.wait_for_state(&chat.id, |state| *state == ChatState::Idle);
    assert!(chat_skill_is_current(&host.home, &chat));
    assert_eq!(
        chat_skill_version(&host.home, &chat),
        Some(orchestrator_skill_version())
    );

    SettingsStore::open(&host.home)
        .unwrap()
        .update(|settings| settings.delegation_policy.set_parallel_shells(Some(7)))
        .unwrap();
    assert!(
        !chat_skill_is_current(&host.home, &chat),
        "told 4 worker shells, Settings say 7"
    );
    load_chat_skill(&sessions, &chat_host, &chat, None).unwrap();
    let sent = sent_texts(&chat);
    assert_eq!(sent.len(), 2);
    assert!(
        sent[1].contains("about 7 parallel worker shells"),
        "{}",
        sent[1]
    );
    assert!(sent[1].contains("<riwork-orchestrator-skill>"));
    assert!(chat_skill_is_current(&host.home, &chat));
    host.wait_for_state(&chat.id, |state| *state == ChatState::Idle);
    load_chat_skill(&sessions, &chat_host, &chat, None).unwrap();
    assert_eq!(sent_texts(&chat).len(), 2, "told once");

    // A mark from a build that did not record the policy cannot say what it was told.
    fs::write(
        skill_mark_path(&host.home, None),
        serde_json::to_vec(&json!({
            "chat_id": chat.id,
            "version": orchestrator_skill_version(),
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(!chat_skill_is_current(&host.home, &chat));
    assert_eq!(
        chat_skill_version(&host.home, &chat),
        Some(orchestrator_skill_version())
    );
}

/// A start that holds until `HOLD` in the chat's folder is gone, then starts the fake.
fn held_start(
    config: crate::chat::driver::DriverConfig,
    events: std::sync::mpsc::Sender<ChatEvent>,
) -> Result<Box<dyn crate::chat::driver::Driver>, String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while config.cwd.join(HOLD).exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    (fake_providers().codex)(config, events)
}

const HOLD: &str = "hold-start";

/// The host lists a new orchestrator chat before its agent has started, and so before
/// it has its startup message. Another creator waits for that, and gets the chat only
/// once it has been told what it is.
#[test]
fn a_second_creator_gets_a_chat_only_after_its_startup_message() {
    let host = TestHost::with_providers(
        quick_options(),
        Providers {
            codex: held_start,
            ..fake_providers()
        },
    );
    let (sessions, _server) = sessions(&host);
    let context = orchestrator_context(&host.home, None);
    fs::create_dir_all(&context).unwrap();
    fs::write(context.join(HOLD), "").unwrap();
    let runs = OrchestratorRuns::chat(Provider::Codex);
    std::thread::scope(|scope| {
        let first = scope.spawn(|| chat_of(global_chat(&sessions, &host, runs.clone()).unwrap()));
        // The first creator's chat is listed, its agent held in its start.
        eventually(|| host.client().list().unwrap().len() == 1);
        let second = scope.spawn(|| {
            let (chat, created) = chat_of(global_chat(&sessions, &host, runs.clone()).unwrap());
            (chat.clone(), created, sent_texts(&chat).len())
        });
        std::thread::sleep(std::time::Duration::from_millis(300));
        fs::remove_file(context.join(HOLD)).unwrap();
        let (first, created) = first.join().unwrap();
        assert!(created);
        let (second, created, told) = second.join().unwrap();
        assert!(!created);
        assert_eq!(second.id, first.id);
        assert_eq!(
            told, 1,
            "the second creator got the chat before it was told"
        );
    });
    assert_eq!(host.client().list().unwrap().len(), 1);
}

/// A chat whose skill cannot be recorded is not kept: creation fails and leaves nothing
/// that a later creation would take up as it is.
#[test]
fn a_chat_whose_skill_cannot_be_recorded_is_not_left_behind() {
    let host = TestHost::new();
    let (sessions, _server) = sessions(&host);
    // A folder where the mark goes cannot be replaced by the mark.
    let mark = skill_mark_path(&host.home, None);
    fs::create_dir_all(&mark).unwrap();
    for runs in [
        OrchestratorRuns::chat(Provider::Codex),
        OrchestratorRuns::Automatic,
    ] {
        let error = global_chat(&sessions, &host, runs).unwrap_err();
        assert!(error.contains("Cannot record"), "{error}");
        assert!(host.client().list().unwrap().is_empty(), "{error}");
    }
    fs::remove_dir(&mark).unwrap();
    let (chat, created) =
        chat_of(global_chat(&sessions, &host, OrchestratorRuns::chat(Provider::Codex)).unwrap());
    assert!(created);
    assert!(chat_skill_is_current(&host.home, &chat));
}

/// A terminal and a chat creator of one scope that race make one orchestrator: the
/// first to look holds the scope until its orchestrator runs. `ensure`, on the chat
/// creator's way from looking to creating, lets the terminal creator try meanwhile.
#[test]
#[ignore = "slow: starts a real tmux server for a terminal orchestrator"]
fn a_terminal_and_a_chat_creator_that_race_make_one_orchestrator() {
    use std::sync::mpsc;
    let host = TestHost::new();
    let (sessions, _server) = sessions(&host);
    let home = host.home.clone();
    let (started, wait_started) = mpsc::channel::<()>();
    let (finished, wait_finished) = mpsc::channel::<Result<Created, String>>();
    let waited = std::sync::Mutex::new(None);
    std::thread::scope(|scope| {
        let terminal = {
            let sessions = sessions.clone();
            let home = home.clone();
            scope.spawn(move || {
                if wait_started
                    .recv_timeout(std::time::Duration::from_secs(20))
                    .is_err()
                {
                    return;
                }
                let made = create(
                    &sessions,
                    &chat_host(&home),
                    &OrchestratorScope::Global,
                    None,
                    home.clone(),
                    Some("sleep 600".into()),
                    &OrchestratorRuns::Terminal,
                );
                finished.send(made).unwrap();
            })
        };
        let ensure = |home: &Path| -> Result<PathBuf, String> {
            let mut waited = waited.lock().unwrap();
            if waited.is_none() {
                started.send(()).unwrap();
                *waited = Some(
                    wait_finished
                        .recv_timeout(std::time::Duration::from_secs(2))
                        .ok(),
                );
            }
            Ok(socket_path(home))
        };
        let chat = create(
            &sessions,
            &ChatHost {
                home: &home,
                ensure: &ensure,
            },
            &OrchestratorScope::Global,
            None,
            home.clone(),
            None,
            &OrchestratorRuns::chat(Provider::Codex),
        )
        .unwrap();
        terminal.join().unwrap();
        let terminal = match waited.lock().unwrap().take().flatten() {
            Some(made) => made,
            None => wait_finished.recv().unwrap(),
        }
        .unwrap();
        let live_terminals = sessions
            .orchestrator_get()
            .unwrap()
            .filter(|shell| shell.alive)
            .into_iter()
            .count();
        let chats = host.client().list().unwrap().len();
        assert_eq!(
            live_terminals + chats,
            1,
            "chat {chat:?}, terminal {terminal:?}"
        );
        assert_eq!(id_of(&chat.orchestrator), id_of(&terminal.orchestrator));
        assert!(chat.created != terminal.created);
    });
}
