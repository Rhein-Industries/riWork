//! `riwork handoff`: the command line, the caller as the source, and what is printed,
//! against the in-process chat host.

use std::time::Duration;

use super::*;
use crate::chat::{
    model::{ChatCommand, ChatEvent, ChatState, Provider},
    testing::TestHost,
};

fn words(line: &str) -> Vec<String> {
    line.split_whitespace().map(str::to_owned).collect()
}

fn fake_account(_: &Path, query: &str) -> Result<CodexAccountBinding, String> {
    if query == "Work" {
        Ok(CodexAccountBinding {
            id: Some("acct-work".into()),
            label: Some("Work".into()),
            email: None,
            home: "/accounts/work/home".into(),
        })
    } else {
        Err(format!("No Codex account matches '{query}'."))
    }
}

struct Rig {
    host: TestHost,
}

impl Rig {
    fn new() -> Self {
        Self {
            host: TestHost::new(),
        }
    }

    /// A chat with a short conversation of its own.
    fn chat(&self) -> crate::chat::model::ChatInfo {
        self.chat_in("work")
    }

    /// The same in a directory of its own. A chat's fake agent is the one of its directory,
    /// and a handoff's new chat starts in the source's: a source that is asked after a
    /// handoff from its directory needs a directory not used before.
    fn chat_in(&self, directory: &str) -> crate::chat::model::ChatInfo {
        let chat = self.host.create_in(directory, Provider::Claude);
        self.host
            .wait_for_state(&chat.id, |state| *state == ChatState::Idle);
        self.host
            .client()
            .command(
                &chat.id,
                ChatCommand::Send {
                    text: "fix the build".into(),
                },
            )
            .unwrap();
        self.host.wait_for_log(&chat.id, |log| {
            log.iter()
                .any(|e| matches!(e.event, ChatEvent::TurnCompleted { .. }))
        });
        self.host
            .wait_for_state(&chat.id, |state| *state == ChatState::Idle);
        chat
    }

    fn run(
        &self,
        line: &str,
        json: bool,
        shell_env: Option<&str>,
        chat_env: Option<&str>,
    ) -> Result<String, String> {
        let socket = self.host.socket();
        let ensure = move || Ok(socket.clone());
        output(
            &Caller {
                home: &self.host.home,
                shell_env,
                chat_env,
                ensure: &ensure,
                account: &fake_account,
                timing: Timing {
                    wait: Duration::from_secs(20),
                    poll: Duration::from_millis(20),
                    grace: Duration::from_millis(300),
                },
            },
            words(line),
            json,
        )
    }
}

// ---- The command line ------------------------------------------------------------------------

#[test]
fn the_whole_command_line_is_read() {
    let parsed = parse(words(
        "--from 12345678 --to chat --provider codex --model gpt-5 --effort high --account Work --mode auto-edit --context summary",
    ))
    .unwrap();
    assert_eq!(
        parsed,
        Arguments {
            from: Some("12345678".into()),
            kind: Kind::Chat,
            provider: HarnessKind::Codex,
            model: Some("gpt-5".into()),
            effort: Some("high".into()),
            account: Some("Work".into()),
            mode: Some(ApprovalMode::AutoEdit),
            context: Context::Summary,
            note: None,
        }
    );
    // Only the target is required; the rest is the project's, the CLI's and the default.
    let minimal = parse(words("--to shell --provider grok")).unwrap();
    assert_eq!(
        minimal,
        Arguments {
            from: None,
            kind: Kind::Shell,
            provider: HarnessKind::Grok,
            model: None,
            effort: None,
            account: None,
            mode: None,
            context: Context::Transcript,
            note: None,
        }
    );
    for (word, mode) in [
        ("supervised", ApprovalMode::Supervised),
        ("auto-edit", ApprovalMode::AutoEdit),
        ("full", ApprovalMode::Full),
        ("plan", ApprovalMode::Plan),
    ] {
        let parsed = parse(words(&format!("--to chat --provider claude --mode {word}"))).unwrap();
        assert_eq!(parsed.mode, Some(mode));
    }
}

#[test]
fn free_text_is_taken_as_it_is_even_when_it_looks_like_an_option() {
    let arguments = vec![
        "--to=chat".to_owned(),
        "--provider=codex".to_owned(),
        "--model=--odd".to_owned(),
        "--note".to_owned(),
        "  Use the\n   Work account --json, please  ".to_owned(),
        "--account".to_owned(),
        "--work".to_owned(),
    ];
    let parsed = parse(arguments).unwrap();
    assert_eq!(parsed.model.as_deref(), Some("--odd"));
    assert_eq!(parsed.account.as_deref(), Some("--work"));
    assert_eq!(
        parsed.note.as_deref(),
        Some("Use the Work account --json, please")
    );
    // A blank note is none.
    let parsed = parse(words("--to chat --provider codex --note")).err();
    assert_eq!(parsed.as_deref(), Some("--note needs a value"));
    let parsed = parse(vec![
        "--to".into(),
        "chat".into(),
        "--provider".into(),
        "codex".into(),
        "--note".into(),
        "  \n ".into(),
    ])
    .unwrap();
    assert_eq!(parsed.note, None);
}

#[test]
fn what_cannot_be_understood_is_refused_with_the_usage() {
    let refused = |line: &str| parse(words(line)).unwrap_err();
    assert!(
        refused("--provider codex")
            .starts_with("--to must be shell or chat\nUsage: riwork handoff")
    );
    assert!(refused("--to robot --provider codex").starts_with("--to must be shell or chat"));
    assert!(refused("--to chat").starts_with("--provider must be codex, claude or grok"));
    assert!(refused("--to chat --provider gemini").starts_with("--provider must be"));
    assert_eq!(
        refused("--to chat --provider codex --mode reckless"),
        "--mode must be supervised, auto-edit, full, or plan"
    );
    assert_eq!(
        refused("--to chat --provider codex --context everything"),
        "--context must be transcript or summary"
    );
    assert_eq!(
        refused("--to chat --provider codex extra words"),
        "Unexpected arguments: extra words"
    );
    assert_eq!(
        refused("--to chat --provider codex --to shell"),
        "--to can only be given once"
    );
    assert_eq!(
        refused("--to chat --provider codex --model"),
        "--model needs a value"
    );
    assert!(
        parse(vec![
            "--to".into(),
            "chat".into(),
            "--provider".into(),
            "codex".into(),
            "--model".into(),
            " ".into()
        ])
        .unwrap_err()
        .contains("must not be blank")
    );
    assert!(
        parse(vec![
            "--to".into(),
            "chat".into(),
            "--provider".into(),
            "codex".into(),
            "--note".into(),
            "a\u{7}b".into()
        ])
        .unwrap_err()
        .contains("control characters")
    );
    let long = "x".repeat(MAX_NOTE_CHARS + 1);
    assert!(
        parse(vec![
            "--to".into(),
            "chat".into(),
            "--provider".into(),
            "codex".into(),
            format!("--note={long}"),
        ])
        .unwrap_err()
        .contains("at most 2000")
    );
}

// ---- The caller as the source ------------------------------------------------------------------

#[test]
fn a_chats_own_agent_hands_off_its_chat_by_saying_nothing_of_the_source() {
    let rig = Rig::new();
    let chat = rig.chat();
    let text = rig
        .run(
            "--to chat --provider codex --model gpt-5",
            false,
            None,
            Some(&chat.id),
        )
        .unwrap();
    let chats = rig.host.client().list().unwrap();
    let target = chats.iter().find(|c| c.id != chat.id).expect("a new chat");
    assert_eq!(target.provider, Provider::Codex);
    assert_eq!(target.model.as_deref(), Some("gpt-5"));
    let documents: Vec<_> = std::fs::read_dir(rig.host.home.join("handoffs"))
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(documents.len(), 1);
    assert_eq!(
        text,
        format!(
            "Handed off to a Codex chat: {}\nThe handoff is in {}\n",
            target.id,
            documents[0].path().display()
        )
    );
}

#[test]
fn from_names_the_source_by_a_prefix_and_beats_the_environment() {
    let rig = Rig::new();
    let chat = rig.chat();
    let other = "ffffffff-ffff-4fff-8fff-ffffffffffff";
    let json = rig
        .run(
            &format!("--from {} --to chat --provider claude", &chat.id[..8]),
            true,
            Some(other),
            Some(other),
        )
        .unwrap();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["target"]["kind"], "chat");
    let target = value["target"]["id"].as_str().unwrap();
    assert!(
        rig.host
            .client()
            .list()
            .unwrap()
            .iter()
            .any(|c| c.id == target)
    );
    assert_eq!(
        value["document"].as_str().unwrap(),
        rig.host
            .home
            .join("handoffs")
            .join(format!("{}.md", value["handoff_id"].as_str().unwrap()))
            .to_str()
            .unwrap()
    );
    assert_eq!(value["context"], "transcript");
    assert!(value.get("fallback").is_none(), "{value}");
    // The JSON has exactly what a script needs.
    let mut keys: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["context", "document", "handoff_id", "target"]);
}

#[test]
fn without_a_source_or_with_two_the_command_asks_for_from() {
    let rig = Rig::new();
    let chat = rig.chat();
    let none = rig
        .run("--to chat --provider codex", false, None, None)
        .unwrap_err();
    assert!(none.starts_with("Pass --from"), "{none}");
    let both = rig
        .run(
            "--to chat --provider codex",
            false,
            Some(&chat.id),
            Some(&chat.id),
        )
        .unwrap_err();
    assert!(
        both.contains("Both RIWORK_SHELL_ID and RIWORK_CHAT_ID are set"),
        "{both}"
    );
    let unknown = rig
        .run(
            "--from cccccccc --to chat --provider codex",
            false,
            None,
            None,
        )
        .unwrap_err();
    assert!(
        unknown.contains("No shell or chat has the id cccccccc"),
        "{unknown}"
    );
    // An unknown shell in the environment is as unknown as a chat.
    let stale = rig
        .run(
            "--to chat --provider codex",
            false,
            Some("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"),
            None,
        )
        .unwrap_err();
    assert!(stale.contains("No shell or chat has the id"), "{stale}");
    assert_eq!(
        rig.host.client().list().unwrap().len(),
        1,
        "nothing was started"
    );
}

#[test]
fn an_account_is_by_label_and_a_claude_target_has_none() {
    let rig = Rig::new();
    let chat = rig.chat();
    rig.run(
        "--to chat --provider codex --account Work",
        false,
        None,
        Some(&chat.id),
    )
    .unwrap();
    let chats = rig.host.client().list().unwrap();
    let target = chats.iter().find(|c| c.id != chat.id).unwrap();
    assert_eq!(target.codex_account_id.as_deref(), Some("acct-work"));

    let error = rig
        .run(
            "--to chat --provider claude --account Work",
            false,
            None,
            Some(&chat.id),
        )
        .unwrap_err();
    assert!(
        error.starts_with("Claude has no RiWork-managed accounts"),
        "{error}"
    );
    let error = rig
        .run(
            "--to chat --provider codex --account Nobody",
            false,
            None,
            Some(&chat.id),
        )
        .unwrap_err();
    assert_eq!(error, "No Codex account matches 'Nobody'.");
    assert_eq!(rig.host.client().list().unwrap().len(), 2);
}

#[test]
fn a_summary_that_does_not_come_is_reported_where_the_caller_reads() {
    let rig = Rig::new();
    let (chat, again) = (rig.chat_in("first"), rig.chat_in("second"));
    let line = "--to chat --provider codex --context summary";
    let text = rig.run(line, false, None, Some(&chat.id)).unwrap();
    assert!(text.starts_with("Handed off to a Codex chat: "), "{text}");
    assert!(
        text.contains("No summary was had (the agent's turn ended without a summary at "),
        "{text}"
    );
    assert!(
        text.ends_with("), so the handoff holds the transcript.\n"),
        "{text}"
    );
    let json = rig.run(line, true, None, Some(&again.id)).unwrap();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["context"], "transcript");
    assert!(
        value["fallback"]
            .as_str()
            .unwrap()
            .starts_with("the agent's turn ended")
    );
}
