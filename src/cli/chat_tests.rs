//! The `chat` commands against a running chat host with fake drivers, on a
//! throwaway `RIWORK_HOME`.
use super::*;
use crate::chat::client::socket_path;
use crate::chat::model::{ApprovalMode, ChatCommand, ChatInfo, ChatState, Provider};
use crate::chat::testing::*;

fn run_json(home: &Path, line: &str, json: bool) -> Result<String, String> {
    chat_client_command(
        home,
        line.split_whitespace().map(str::to_owned).collect(),
        json,
        &|home| Ok(socket_path(home)),
    )
}

fn run(host: &TestHost, line: &str) -> Result<String, String> {
    run_json(&host.home, line, false)
}

fn listed(host: &TestHost) -> Vec<ChatInfo> {
    serde_json::from_str(&run_json(&host.home, "list", true).unwrap()).unwrap()
}

#[test]
fn list_shows_every_chat_on_a_line_and_as_json() {
    let host = TestHost::new();
    assert_eq!(run(&host, "list").unwrap(), "");
    assert_eq!(run(&host, "").unwrap(), "", "list is the default");
    assert_eq!(run_json(&host.home, "list", true).unwrap().trim(), "[]");

    let codex = host.create(Provider::Codex);
    host.wait_for_state(&codex.id, |s| *s == ChatState::Idle);
    let claude = host.create_in("other", Provider::Claude);
    host.client().close(&claude.id).unwrap();
    let text = run(&host, "list").unwrap();
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    let line = |id: &str| lines.iter().find(|line| line.starts_with(id)).unwrap();
    assert_eq!(
        *line(&codex.id),
        format!(
            "{}  codex  idle  Codex chat  {}",
            codex.id,
            host.work().display()
        )
    );
    assert!(
        line(&claude.id).contains("  claude  stopped  Claude chat  "),
        "{text}"
    );
    let chats = listed(&host);
    assert_eq!(chats.len(), 2);
    assert!(
        chats
            .iter()
            .any(|chat| chat.id == claude.id && chat.state == ChatState::Stopped)
    );
    assert!(run(&host, "list extra").is_err());
}

#[test]
fn ensure_prints_the_socket_to_connect_to() {
    let host = TestHost::new();
    let socket = socket_path(&host.home);
    assert_eq!(
        run(&host, "ensure").unwrap(),
        format!("{}\n", socket.display())
    );
    let json: serde_json::Value =
        serde_json::from_str(&run_json(&host.home, "ensure", true).unwrap()).unwrap();
    assert_eq!(json["socket"], socket.to_string_lossy().as_ref());
    assert!(run(&host, "ensure now").is_err());
}

#[test]
fn new_starts_a_chat_in_the_projects_directory_in_the_chosen_mode() {
    let host = TestHost::new();
    let project = Store::open(&host.home)
        .unwrap()
        .add_project(host.work(), Some("demo"))
        .unwrap();
    let text = run(&host, "new --provider claude --project demo --mode plan").unwrap();
    let chats = listed(&host);
    assert_eq!(chats.len(), 1);
    let chat = &chats[0];
    // One line: the chat, whatever state the driver has got to.
    assert!(
        text.starts_with(&format!("{}  claude  ", chat.id)),
        "{text}"
    );
    assert!(
        text.ends_with(&format!("  Claude chat  {}\n", project.root.display())),
        "{text}"
    );
    assert_eq!(chat.project_id.as_deref(), Some(project.id.as_str()));
    assert_eq!(chat.cwd, project.root);
    assert_eq!(chat.approval_mode, ApprovalMode::Plan);
    let starts = fake_for(&project.root).starts.lock().unwrap().clone();
    assert_eq!(starts.len(), 1);
    assert_eq!(
        (starts[0].provider, starts[0].approval_mode),
        (Provider::Claude, ApprovalMode::Plan)
    );

    // The default mode asks before everything; JSON is the chat itself.
    let json = run_json(&host.home, "new --provider codex", true).unwrap();
    let created: ChatInfo = serde_json::from_str(&json).unwrap();
    assert_eq!(created.approval_mode, ApprovalMode::Supervised);
    assert_eq!(created.provider, Provider::Codex);
}

#[test]
fn new_refuses_what_it_cannot_understand() {
    let host = TestHost::new();
    let usage = |line: &str| run(&host, line).unwrap_err();
    assert!(usage("new").starts_with("Usage: riwork chat new"));
    assert!(usage("new --provider gemini").starts_with("Usage: riwork chat new"));
    assert!(usage("new --provider codex --mode reckless").contains("--mode must be"));
    assert!(usage("new --provider codex extra").contains("Unexpected arguments"));
    // With no project to start in, like `shell create`.
    assert!(usage("new --provider codex").contains("No active project"));
    assert!(host.client().list().unwrap().is_empty());
}

#[test]
fn a_chat_whose_provider_does_not_start_is_reported_and_kept() {
    let host = TestHost::new();
    let project = Store::open(&host.home)
        .unwrap()
        .add_project(host.work(), Some("demo"))
        .unwrap();
    *fake_for(&project.root).fail_start.lock().unwrap() = Some("codex is not installed".into());
    let error = run(&host, "new --provider codex --project demo").unwrap_err();
    assert!(error.contains("codex is not installed"), "{error}");
    let chats = listed(&host);
    assert_eq!(chats.len(), 1);
    assert!(error.contains(&chats[0].id), "{error}");
    assert!(matches!(chats[0].state, ChatState::Failed { .. }));
    assert!(
        run(&host, "list")
            .unwrap()
            .contains("failed (codex is not installed)")
    );
}

#[test]
fn send_and_stop_take_a_unique_prefix_and_send_resumes_a_stopped_chat() {
    let host = TestHost::new();
    let chat = host.create(Provider::Codex);
    host.wait_for_state(&chat.id, |s| *s == ChatState::Idle);
    let prefix = &chat.id[..8];

    assert_eq!(
        run(&host, &format!("send {prefix} hello   brave new world")).unwrap(),
        ""
    );
    assert_eq!(
        host.fake().commands(),
        vec![ChatCommand::Send {
            text: "hello brave new world".into()
        }]
    );
    let json = run_json(&host.home, &format!("send {} again", chat.id), true).unwrap();
    let json: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(
        (json["id"].as_str(), json["sent"].as_str()),
        (Some(chat.id.as_str()), Some("again"))
    );

    assert_eq!(
        run(&host, &format!("stop {prefix}")).unwrap(),
        format!("Stopped {}\n", chat.id)
    );
    assert_eq!(host.info(&chat.id).state, ChatState::Stopped);
    run(&host, &format!("send {prefix} wake up")).unwrap();
    assert_eq!(host.fake().start_count(), 2);
    assert_eq!(
        host.fake().starts.lock().unwrap()[1].resume.as_deref(),
        Some("thread-1")
    );
    let stopped = run_json(&host.home, &format!("stop {prefix}"), true).unwrap();
    assert!(stopped.contains("\"stopped\""), "{stopped}");
}

#[test]
fn send_and_stop_say_what_is_wrong_with_the_chat_they_were_given() {
    let host = TestHost::new();
    let chat = host.create(Provider::Codex);
    assert!(
        run(&host, "send")
            .unwrap_err()
            .starts_with("Usage: riwork chat send")
    );
    assert!(
        run(&host, &format!("send {}", chat.id))
            .unwrap_err()
            .starts_with("Usage")
    );
    assert!(
        run(&host, "send 00000000-0000 hi")
            .unwrap_err()
            .contains("Unknown chat")
    );
    assert!(
        run(&host, "send abc hi")
            .unwrap_err()
            .contains("at least eight")
    );
    assert!(
        run(&host, "stop")
            .unwrap_err()
            .starts_with("Usage: riwork chat stop")
    );
    assert!(
        run(&host, "stop 00000000")
            .unwrap_err()
            .contains("Unknown chat")
    );
    assert!(
        run(&host, "frobnicate")
            .unwrap_err()
            .starts_with("Usage: riwork chat")
    );
}

#[test]
fn stop_does_not_start_a_host_just_to_find_nothing_running() {
    let home = short_home();
    let started = std::cell::Cell::new(false);
    let error = chat_client_command(
        &home,
        vec!["stop".into(), "abcdefgh".into()],
        false,
        &|_| {
            started.set(true);
            Err("no".into())
        },
    )
    .unwrap_err();
    assert!(error.contains("No chat host is running"), "{error}");
    assert!(!started.get());
    let _ = std::fs::remove_dir_all(home);
}
