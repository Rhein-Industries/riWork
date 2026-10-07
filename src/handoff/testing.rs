//! What the handoff tests share: fixtures on disk, a chat's log, a shell record, and a
//! tmux server of its own.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use uuid::Uuid;

use crate::{
    chat::{
        model::{ApprovalMode, ChatEvent, ChatInfo, ChatState, Provider},
        wire::Envelope,
    },
    sessions::{SessionManager, ShellSession},
};

pub fn testdata(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/handoff/testdata")
        .join(name)
}

/// A chat directory the way the host writes it: `events.jsonl`, one envelope a line,
/// and `info.json`. Returns the chat's id.
pub fn write_chat(home: &Path, info: ChatInfo, events: Vec<ChatEvent>) -> String {
    let dir = home.join("chats").join(&info.id);
    fs::create_dir_all(&dir).unwrap();
    let log: String = events
        .into_iter()
        .enumerate()
        .map(|(index, event)| {
            let envelope = Envelope {
                chat_id: info.id.clone(),
                seq: index as u64 + 1,
                event,
            };
            format!("{}\n", serde_json::to_string(&envelope).unwrap())
        })
        .collect();
    fs::write(dir.join("events.jsonl"), log).unwrap();
    fs::write(dir.join("info.json"), serde_json::to_vec(&info).unwrap()).unwrap();
    info.id
}

pub fn chat_info(model: Option<&str>) -> ChatInfo {
    ChatInfo {
        parent_id: None,
        user_title: None,
        first_user_message: None,
        id: Uuid::new_v4().to_string(),
        provider: Provider::Codex,
        project_id: None,
        worktree_id: None,
        cwd: "/work/app".into(),
        title: "Fix the build".into(),
        created_at_unix: 1,
        provider_thread_id: Some("thread".into()),
        model: model.map(str::to_owned),
        effort: None,
        approval_mode: ApprovalMode::Supervised,
        codex_account_id: None,
        orchestrator: None,
        fast: false,
        state: ChatState::Idle,
    }
}

/// A shell record for an agent in `/work/app`.
pub fn shell_with(id: &str, harness: &str, codex_home: Option<&Path>) -> ShellSession {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "project_id": null,
        "worktree_id": null,
        "kind": "project",
        "cwd": "/work/app",
        "command": null,
        "harness": harness,
        "codex_home": codex_home,
        "created_at_unix": 0
    }))
    .unwrap()
}

/// A tmux server of its own on a throwaway home, stopped when the test is over.
pub struct Tmux {
    pub home: PathBuf,
    pub manager: SessionManager,
}

impl Tmux {
    /// None where there is no tmux to start.
    pub fn new() -> Option<Self> {
        let home = crate::chat::testing::short_home();
        let manager = SessionManager::at(home.clone()).ok()?;
        Some(Self { home, manager })
    }

    /// A plain shell that runs `script` and waits until `ready` shows on its screen.
    pub fn shell(&self, script: &str, ready: &str) -> ShellSession {
        let shell = self
            .manager
            .create(
                Uuid::new_v4().to_string(),
                None,
                self.home.clone(),
                Some(script.to_owned()),
            )
            .unwrap();
        let end = Instant::now() + Duration::from_secs(20);
        while !self
            .manager
            .capture(&shell.id, 50)
            .is_ok_and(|text| text.contains(ready))
        {
            assert!(Instant::now() < end, "the pane never showed {ready:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
        shell
    }
}

impl Drop for Tmux {
    fn drop(&mut self) {
        self.manager.kill_server();
        let _ = fs::remove_dir_all(&self.home);
    }
}
