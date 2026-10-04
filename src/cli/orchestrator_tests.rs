//! The `orchestrator` commands against a chat host with fake drivers, on a
//! throwaway `RIWORK_HOME`: the same words for an orchestrator that runs as a
//! chat as for one that runs in a terminal. A terminal orchestrator here is a
//! `sleep` in a tmux server of its own, which is killed with the guard.
use super::*;
use crate::chat::client::socket_path;
use crate::chat::model::{ChatCommand, ChatEvent, ChatState, Provider};
use crate::chat::testing::*;
use crate::orchestrators::ChatHost;
use crate::settings::{OrchestratorMode, SettingsStore};
use serde_json::Value;

fn ensure_in_process(home: &Path) -> Result<PathBuf, String> {
    Ok(socket_path(home))
}

/// A test host, the sessions of its home, and the tmux server they may start.
struct Setup {
    host: TestHost,
    sessions: SessionManager,
}

impl Drop for Setup {
    fn drop(&mut self) {
        self.sessions.kill_server();
    }
}

impl Setup {
    fn new() -> Self {
        let host = TestHost::new();
        let sessions = SessionManager::at(host.home.clone()).unwrap();
        Self { host, sessions }
    }

    /// Chat orchestrators from now on, behind `provider`.
    fn choose_chat(&self, provider: Provider) {
        SettingsStore::open(&self.host.home)
            .unwrap()
            .update(|settings| {
                settings.orchestrator_mode = OrchestratorMode::Chat;
                settings.orchestrator_chat_provider = provider;
            })
            .unwrap();
    }

    fn choose_terminal(&self) {
        SettingsStore::open(&self.host.home)
            .unwrap()
            .update(|settings| settings.orchestrator_mode = OrchestratorMode::Terminal)
            .unwrap();
    }

    fn output(&self, args: &[&str], json: bool) -> Result<OrchestratorOutput, String> {
        let host = ChatHost {
            home: &self.host.home,
            ensure: &ensure_in_process,
        };
        orchestrator_client_command(
            &self.sessions,
            &host,
            args.iter().map(|arg| (*arg).to_owned()).collect(),
            json,
        )
    }

    fn text_of(&self, args: &[&str], json: bool) -> Result<String, String> {
        match self.output(args, json)? {
            OrchestratorOutput::Text(text) => Ok(text),
            other => panic!("expected text, got {other:?}"),
        }
    }

    /// The words of `line`, split at spaces.
    fn run(&self, line: &str) -> Result<String, String> {
        let words: Vec<_> = line.split_whitespace().collect();
        self.text_of(&words, false)
    }

    fn text(&self, line: &str, json: bool) -> Result<String, String> {
        let words: Vec<_> = line.split_whitespace().collect();
        self.text_of(&words, json)
    }

    fn json(&self, line: &str) -> Value {
        serde_json::from_str(&self.text(line, true).unwrap()).unwrap()
    }

    fn json_args(&self, args: &[&str]) -> Value {
        serde_json::from_str(&self.text_of(args, true).unwrap()).unwrap()
    }

    /// The only chat the host has.
    fn only_chat(&self) -> crate::chat::model::ChatInfo {
        let mut chats = self.host.client().list().unwrap();
        assert_eq!(chats.len(), 1, "{chats:?}");
        chats.remove(0)
    }

    fn project(&self) -> crate::store::Project {
        Store::open(&self.host.home)
            .unwrap()
            .add_project(self.host.work(), Some("demo"))
            .unwrap()
    }
}

#[test]
fn create_follows_the_setting_and_a_second_create_returns_the_orchestrator_it_made() {
    let setup = Setup::new();
    setup.choose_chat(Provider::Codex);
    let created = setup.json("create");
    let chat = setup.only_chat();
    assert_eq!(created["created"], true);
    assert_eq!(created["mode"], "chat");
    assert_eq!(created["id"], chat.id);
    assert_eq!(created["chat_id"], chat.id);
    assert_eq!(created["provider"], "codex");
    assert_eq!(created["kind"], "orchestrator");
    assert_eq!(created["project_id"], Value::Null);
    assert_eq!(
        chat.orchestrator,
        Some(crate::chat::model::OrchestratorScope::Global)
    );

    // The same words again find it, in either mode, and make no second chat.
    for choice in [Setup::choose_terminal as fn(&Setup), |setup: &Setup| {
        setup.choose_chat(Provider::Claude)
    }] {
        choice(&setup);
        let again = setup.json("create");
        assert_eq!(again["created"], false);
        assert_eq!(again["id"], chat.id);
        assert_eq!(again["provider"], "codex");
    }
    assert_eq!(setup.only_chat().id, chat.id);
    // Plain text is a line: the id, what it is, its state and scope, its folder.
    let line = setup.run("create").unwrap();
    assert!(
        line.starts_with(&format!("{}  Orchestrator  ", chat.id)),
        "{line}"
    );
    assert!(line.ends_with("  chat codex\n"), "{line}");
    assert!(
        line.contains(&format!("  orchestrator  {}  ", chat.cwd.display())),
        "{line}"
    );
}

#[test]
fn a_project_chat_orchestrator_is_created_for_its_project_and_listed_with_the_terminal_ones() {
    let setup = Setup::new();
    setup.choose_chat(Provider::Claude);
    let project = setup.project();
    let created = setup.json("create --project demo");
    assert_eq!(created["created"], true);
    assert_eq!(created["mode"], "chat");
    assert_eq!(created["provider"], "claude");
    assert_eq!(created["project_id"], project.id.as_str());
    let chat = setup.only_chat();
    assert_eq!(chat.project_id.as_deref(), Some(project.id.as_str()));
    assert_eq!(chat.approval_mode, crate::chat::model::ApprovalMode::Full);
    // --cwd is for the global orchestrator's custom command only.
    assert!(setup.run("create --project demo --cwd /tmp").is_err());

    // A terminal one (a custom command is always a terminal) beside it.
    let terminal = setup.json_args(&["create", "--command", "sleep 600"]);
    assert_eq!(terminal["mode"], "terminal");
    let list = setup.json("list");
    let entries = list.as_array().unwrap();
    assert_eq!(entries.len(), 2, "{list}");
    let modes: Vec<_> = entries
        .iter()
        .map(|entry| entry["mode"].as_str().unwrap())
        .collect();
    assert_eq!(modes, ["terminal", "chat"], "terminals first, then chats");
    assert_eq!(entries[0]["kind"], "orchestrator");
    assert_eq!(entries[0]["id"], terminal["id"]);
    assert_eq!(entries[1]["id"], chat.id);
    assert_eq!(entries[1]["chat_id"], chat.id);
    assert_eq!(entries[1]["provider"], "claude");
    assert!(entries[0].get("chat_id").is_none() && entries[0].get("provider").is_none());
    // A project's list holds that project's orchestrators.
    let only = setup.json("list --project demo");
    assert_eq!(only.as_array().unwrap().len(), 1);
    assert_eq!(only[0]["mode"], "chat");
    // Plain lists give a line each.
    let text = setup.run("list").unwrap();
    assert_eq!(text.lines().count(), 2, "{text}");
    assert!(text.contains("  chat claude"), "{text}");
}

#[test]
fn status_reports_the_state_of_the_chat_as_a_terminal_reports_its_session() {
    let setup = Setup::new();
    assert_eq!(
        setup.run("status").unwrap(),
        "No global orchestrator session. Run: riwork orchestrator create\n"
    );
    assert_eq!(setup.text("status", true).unwrap().trim(), "null");
    setup.choose_chat(Provider::Codex);
    setup.run("create").unwrap();
    let chat = setup
        .host
        .wait_for_state(&setup.only_chat().id, |s| *s == ChatState::Idle);
    let state = |line: &str| {
        let status = setup.json(line);
        (
            status["state"].as_str().unwrap().to_owned(),
            status["activity"].as_str().unwrap().to_owned(),
        )
    };
    assert_eq!(state("status"), ("idle".into(), "done".into()));
    assert!(
        setup
            .run("status")
            .unwrap()
            .contains("  Orchestrator  idle  orchestrator  ")
    );
    // `show` is the same word.
    assert_eq!(setup.json("show")["id"], chat.id);

    // A turn under way is running; one that waits for the user is waiting.
    setup.run("send hang").unwrap();
    setup
        .host
        .wait_for_state(&chat.id, |s| *s == ChatState::Running);
    assert_eq!(state("status"), ("running".into(), "working".into()));
    fake_for(&chat.cwd).emit(ChatEvent::State {
        state: ChatState::Waiting,
    });
    setup
        .host
        .wait_for_state(&chat.id, |s| *s == ChatState::Waiting);
    assert_eq!(state("status"), ("waiting".into(), "waiting".into()));
    assert!(
        setup
            .run("status")
            .unwrap()
            .contains("  Orchestrator  waiting  ")
    );

    // A stopped chat is still the orchestrator; the next message resumes it.
    setup.host.client().close(&chat.id).unwrap();
    assert_eq!(state("status"), ("stopped".into(), "unknown".into()));
    setup.run("send wake up").unwrap();
    setup
        .host
        .wait_for_state(&chat.id, |s| *s == ChatState::Idle);
    assert_eq!(fake_for(&chat.cwd).start_count(), 2);
}

#[test]
fn send_is_a_chat_message_and_output_is_the_conversation_as_text() {
    let setup = Setup::new();
    setup.choose_chat(Provider::Codex);
    setup.run("create").unwrap();
    let chat = setup.only_chat();
    setup
        .host
        .wait_for_state(&chat.id, |s| *s == ChatState::Idle);

    assert_eq!(setup.run("send  Review   the open tasks").unwrap(), "");
    let sent = setup.json_args(&["send", "--json", "with", "options"]);
    assert_eq!(sent["id"], chat.id);
    assert_eq!(sent["sent"], "--json with options");
    let texts: Vec<String> = fake_for(&chat.cwd)
        .commands()
        .into_iter()
        .filter_map(|command| match command {
            ChatCommand::Send { text } => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(
        texts.len(),
        3,
        "the startup message and two messages: {texts:?}"
    );
    assert!(texts[0].starts_with("$riwork-orchestrator"));
    assert_eq!(texts[1], "Review the open tasks");
    setup.host.wait_for_log(&chat.id, |log| {
        log.iter()
            .filter(|e| matches!(e.event, ChatEvent::TurnCompleted { .. }))
            .count()
            == 3
    });

    let output = setup.run("output --lines 4").unwrap();
    assert_eq!(
        output,
        "user: Review the open tasks\nagent: echo: Review the open tasks\n\
         user: --json with options\nagent: echo: --json with options\n"
    );
    let all = setup.run("output").unwrap();
    assert!(all.starts_with("user: $riwork-orchestrator"), "{all}");
    // The start message and its echo are several lines each.
    assert!(all.lines().count() > 6, "{all}");
    assert!(all.lines().all(|line| line == line.trim_end()), "{all}");
    let json = setup.json("output --lines 2");
    assert_eq!(json["mode"], "chat");
    assert_eq!(json["id"], chat.id);
    assert_eq!(
        json["output"],
        "user: --json with options\nagent: echo: --json with options"
    );
    assert_eq!(json["line_count"], 2);
    // A terminal's options mean nothing for a transcript.
    assert!(
        setup
            .run("output --styled")
            .unwrap_err()
            .contains("Unexpected")
    );
    assert!(
        setup
            .run("output --lines many")
            .unwrap_err()
            .contains("--lines")
    );
    assert!(
        setup
            .run("send")
            .unwrap_err()
            .starts_with("Usage: riwork orchestrator send")
    );
}

#[test]
fn close_ends_the_chat_orchestrator_and_the_next_create_follows_the_setting_then() {
    let setup = Setup::new();
    setup.choose_chat(Provider::Codex);
    setup.run("create").unwrap();
    let chat = setup.only_chat();
    assert_eq!(setup.run("close").unwrap(), format!("Closed {}\n", chat.id));
    assert!(setup.host.client().list().unwrap().is_empty());
    assert!(
        setup
            .run("status")
            .unwrap()
            .starts_with("No global orchestrator")
    );
    assert!(
        setup
            .run("close")
            .unwrap_err()
            .starts_with("No global orchestrator")
    );
    let closed = setup.json("list");
    assert_eq!(closed.as_array().unwrap().len(), 0);

    // Recreated under the setting of now: a terminal, with a command.
    setup.choose_terminal();
    let terminal = setup.json_args(&["create", "--command", "sleep 600"]);
    assert_eq!(terminal["created"], true);
    assert_eq!(terminal["mode"], "terminal");
    assert!(setup.host.client().list().unwrap().is_empty());
    let status = setup.json("status");
    assert_eq!(status["id"], terminal["id"]);
    assert_eq!(status["mode"], "terminal");
    // And a chat orchestrator is never made beside a terminal one.
    setup.choose_chat(Provider::Codex);
    let again = setup.json("create");
    assert_eq!(again["created"], false);
    assert_eq!(again["id"], terminal["id"]);
    assert!(setup.host.client().list().unwrap().is_empty());
}

#[test]
fn what_only_a_terminal_has_says_so_for_a_chat_and_the_terminals_commands_are_still_the_shells() {
    let setup = Setup::new();
    setup.choose_chat(Provider::Codex);
    setup.run("create").unwrap();
    let chat = setup.only_chat();
    for operation in ["metrics", "attach"] {
        let error = setup.run(operation).unwrap_err();
        assert!(error.contains("this one runs as a chat"), "{error}");
        assert!(
            error.contains(&format!("riwork chat events {}", chat.id)),
            "{error}"
        );
    }
    assert_eq!(
        setup.run("cwd").unwrap(),
        format!("{}\n", chat.cwd.display())
    );
    assert_eq!(
        setup.json("cwd")["cwd"],
        chat.cwd.to_string_lossy().as_ref()
    );

    // For a terminal orchestrator the command is handed to `shell`, with its id.
    setup.run("close").unwrap();
    let terminal = setup.json_args(&["create", "--command", "sleep 600"]);
    let id = terminal["id"].as_str().unwrap().to_owned();
    assert_eq!(
        setup.output(&["output", "--lines", "5"], false).unwrap(),
        OrchestratorOutput::Shell(vec![
            "output".into(),
            id.clone(),
            "--lines".into(),
            "5".into()
        ])
    );
    assert_eq!(
        setup.output(&["send", "hello", "there"], false).unwrap(),
        OrchestratorOutput::Shell(vec![
            "send".into(),
            id.clone(),
            "hello".into(),
            "there".into()
        ])
    );
    assert_eq!(
        setup.output(&["close"], false).unwrap(),
        OrchestratorOutput::Shell(vec!["close".into(), id])
    );
    assert!(
        setup
            .run("frobnicate")
            .unwrap_err()
            .starts_with("Unknown orchestrator command")
    );
}

#[test]
fn load_skill_gives_a_chat_orchestrator_the_whole_skill_once() {
    let setup = Setup::new();
    setup.choose_chat(Provider::Codex);
    setup.run("create").unwrap();
    let chat = setup.only_chat();
    setup
        .host
        .wait_for_state(&chat.id, |s| *s == ChatState::Idle);
    let skill_messages = || {
        fake_for(&chat.cwd)
            .commands()
            .into_iter()
            .filter(|command| matches!(command, ChatCommand::Send { text } if text.contains("<riwork-orchestrator-skill>")))
            .count()
    };
    // It started with the skill this build ships.
    let current = setup.json("load-skill");
    assert_eq!(current["id"], chat.id);
    assert_eq!(skill_messages(), 0);

    // An older skill is replaced by sending the current one.
    std::fs::write(
        chat.cwd.join("chat-skill.json"),
        format!(
            r#"{{"chat_id":"{}","version":"0000000000000000"}}"#,
            chat.id
        ),
    )
    .unwrap();
    assert_eq!(
        setup.json("status")["orchestrator_skill_version"],
        "0000000000000000"
    );
    setup.run("load-skill").unwrap();
    assert_eq!(skill_messages(), 1);
    assert_ne!(
        setup.json("status")["orchestrator_skill_version"],
        "0000000000000000"
    );
    setup.run("load-skill").unwrap();
    assert_eq!(skill_messages(), 1, "loaded once");
    assert!(
        Setup::new()
            .run("load-skill")
            .unwrap_err()
            .starts_with("No global orchestrator")
    );
}

#[test]
fn looking_at_orchestrators_never_starts_a_chat_host() {
    let home = short_home();
    let manager = SessionManager::at(home.clone()).unwrap();
    let started = std::cell::Cell::new(false);
    let ensure = |_: &Path| {
        started.set(true);
        Err("no".to_owned())
    };
    let host = ChatHost {
        home: &home,
        ensure: &ensure,
    };
    for word in ["list", "status", "show"] {
        for json in [true, false] {
            let output = orchestrator_client_command(&manager, &host, vec![word.into()], json);
            assert!(output.is_ok(), "{word}: {output:?}");
        }
    }
    // Nor does a command for an orchestrator that is not there.
    for word in ["output", "close", "cwd", "load-skill"] {
        let error = orchestrator_client_command(&manager, &host, vec![word.into()], false);
        assert!(error.unwrap_err().starts_with("No global orchestrator"));
    }
    assert!(!started.get(), "looking started a host");
    manager.kill_server();
    let _ = std::fs::remove_dir_all(home);
}
