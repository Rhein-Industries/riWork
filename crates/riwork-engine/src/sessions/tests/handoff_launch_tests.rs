//! What `riwork handoff` chooses for the terminal agent it starts: the model, the effort,
//! how much it may do without asking, its first message and its Codex account.

use super::*;
use crate::chat::model::ApprovalMode;

fn command_line(
    harness: HarnessKind,
    choices: &HarnessChoices,
    unrestricted: bool,
    inline: bool,
) -> Vec<String> {
    shell_arguments(
        &harness_command(
            harness,
            HarnessOptions {
                unrestricted,
                inline,
                choices,
            },
            Path::new("/opt/bin/cli"),
            Path::new("/fake/riwork"),
            Path::new("/Users/test/state"),
            "shell-id",
            None,
        )
        .unwrap(),
    )
}

fn chosen(mode: Option<ApprovalMode>) -> HarnessChoices {
    HarnessChoices {
        model: Some("the-model".into()),
        effort: Some("high".into()),
        mode,
        prompt: Some("Take over from /home/me/handoffs/x.md.".into()),
        binding: None,
    }
}

#[test]
fn codex_gets_the_model_effort_and_permissions_as_flags_and_the_message_with_the_guidance() {
    let arguments = command_line(
        HarnessKind::Codex,
        &chosen(Some(ApprovalMode::AutoEdit)),
        false,
        true,
    );
    assert!(contains_sequence(&arguments, &["-m", "the-model"]));
    assert!(contains_sequence(
        &arguments,
        &["-c", "model_reasoning_effort=\"high\""]
    ));
    assert!(contains_sequence(
        &arguments,
        &[
            "--sandbox",
            "workspace-write",
            "--ask-for-approval",
            "on-request"
        ]
    ));
    assert!(arguments.contains(&"--no-alt-screen".to_owned()));
    // The first message is the last argument, and the guidance Codex otherwise starts with
    // follows it.
    assert_eq!(
        arguments.last().unwrap(),
        &format!("Take over from /home/me/handoffs/x.md.\n\n{CUA_GUIDANCE}")
    );
    // The Cua wiring is the New Tab menu's.
    assert!(
        arguments
            .iter()
            .any(|argument| argument.starts_with("mcp_servers.cua-driver.command="))
    );

    let plan = command_line(
        HarnessKind::Codex,
        &chosen(Some(ApprovalMode::Plan)),
        false,
        false,
    );
    assert!(contains_sequence(
        &plan,
        &["--sandbox", "read-only", "--ask-for-approval", "on-request"]
    ));
    // Full is the permission bypass the New Tab menu's unrestricted entries use, once.
    let full = command_line(
        HarnessKind::Codex,
        &chosen(Some(ApprovalMode::Full)),
        true,
        false,
    );
    assert_eq!(
        full.iter()
            .filter(|a| *a == "--dangerously-bypass-approvals-and-sandbox")
            .count(),
        1
    );
    assert!(!full.contains(&"--sandbox".to_owned()));
    // Supervised is the CLI's usual permissions: nothing is added.
    let supervised = command_line(
        HarnessKind::Codex,
        &chosen(Some(ApprovalMode::Supervised)),
        false,
        false,
    );
    assert!(!supervised.contains(&"--sandbox".to_owned()));
    assert!(!supervised.contains(&"--ask-for-approval".to_owned()));
}

#[test]
fn claude_gets_its_flags_and_the_message_after_its_settings() {
    let arguments = command_line(
        HarnessKind::Claude,
        &chosen(Some(ApprovalMode::Plan)),
        false,
        false,
    );
    assert!(contains_sequence(&arguments, &["--model", "the-model"]));
    assert!(contains_sequence(&arguments, &["--effort", "high"]));
    assert!(contains_sequence(
        &arguments,
        &["--permission-mode", "plan"]
    ));
    assert_eq!(
        arguments.last().unwrap(),
        "Take over from /home/me/handoffs/x.md."
    );
    let settings = arguments.iter().position(|a| a == "--settings").unwrap();
    assert_eq!(
        settings + 2,
        arguments.len() - 1,
        "the message follows the settings value"
    );
    // The guidance rides in the system prompt, as for any Claude launch.
    assert!(contains_sequence(
        &arguments,
        &["--append-system-prompt", CUA_GUIDANCE]
    ));
    let edits = command_line(
        HarnessKind::Claude,
        &chosen(Some(ApprovalMode::AutoEdit)),
        false,
        false,
    );
    assert!(contains_sequence(
        &edits,
        &["--permission-mode", "acceptEdits"]
    ));
}

#[test]
fn grok_gets_its_flags_and_the_message_last() {
    let arguments = command_line(
        HarnessKind::Grok,
        &chosen(Some(ApprovalMode::AutoEdit)),
        false,
        true,
    );
    assert!(contains_sequence(&arguments, &["-m", "the-model"]));
    assert!(contains_sequence(
        &arguments,
        &["--reasoning-effort", "high"]
    ));
    assert!(contains_sequence(
        &arguments,
        &["--permission-mode", "acceptEdits"]
    ));
    assert!(arguments.contains(&"--minimal".to_owned()));
    assert_eq!(
        arguments.last().unwrap(),
        "Take over from /home/me/handoffs/x.md."
    );
}

#[test]
fn a_launch_that_chooses_nothing_is_the_menus_launch() {
    let nothing = HarnessChoices::default();
    let codex = command_line(HarnessKind::Codex, &nothing, false, false);
    assert!(!codex.contains(&"-m".to_owned()));
    assert!(
        codex
            .last()
            .unwrap()
            .starts_with("Load the following RiWork desktop automation guidance")
    );
    let claude = command_line(HarnessKind::Claude, &nothing, false, false);
    assert!(!claude.contains(&"--model".to_owned()));
    assert_eq!(
        claude.last().unwrap().chars().next(),
        Some('{'),
        "{claude:?}"
    );
    let grok = command_line(HarnessKind::Grok, &nothing, false, false);
    assert!(!grok.contains(&"-m".to_owned()));
}

#[test]
#[cfg(unix)]
#[ignore = "slow: re-runs the test binary and launches through a fake tmux script"]
fn a_handed_off_terminal_starts_with_its_choices_and_the_account_picked_by_name() {
    use crate::store::Store;
    const NAME: &str = "handoff_launch_tests::a_handed_off_terminal_starts_with_its_choices_and_the_account_picked_by_name";
    let fixture = AccountFixture::new();
    if !fixture.run_in_child(NAME) {
        return;
    }
    let state = fixture.selected("account-a");
    let (manager, capture) = recording_launcher(&fixture, &state);
    executable_script(&fixture.0.join("bin/claude"), "exit 0");
    let root = fixture.0.join("work");
    fs::create_dir(&root).unwrap();
    let project = Store::open(&state)
        .unwrap()
        .add_project(&root, Some("Demo"))
        .unwrap();
    let account_b =
        crate::codex_accounts::resolve_launch_binding(&state, Some("account-b")).unwrap();
    let account_a =
        crate::codex_accounts::resolve_launch_binding(&state, Some("account-a")).unwrap();

    // Codex under an account named by the caller, not the app's choice (account-a).
    let mut choices = chosen(Some(ApprovalMode::Full));
    choices.binding = Some(account_b.clone());
    let codex = manager
        .create_harness_with(
            project.id.clone(),
            None,
            root.clone(),
            HarnessKind::Codex,
            choices,
        )
        .unwrap();
    assert!(codex.unrestricted);
    assert_eq!(codex.harness, Some(HarnessKind::Codex));
    assert_eq!(codex.codex_account_id, account_b.id);
    assert_eq!(codex.codex_home.as_ref(), Some(&account_b.home));
    let arguments = shell_arguments(&codex.command.unwrap());
    assert!(arguments.contains(&format!("CODEX_HOME={}", account_b.home.display())));
    assert!(contains_sequence(&arguments, &["-m", "the-model"]));
    assert!(arguments.contains(&"--dangerously-bypass-approvals-and-sandbox".to_owned()));
    assert!(
        arguments
            .last()
            .unwrap()
            .starts_with("Take over from /home/me/handoffs/x.md.")
    );
    let recorded = take_recorded(&capture);
    assert!(
        recorded.contains(&format!("CODEX_HOME={}", account_b.home.display())),
        "{recorded:?}"
    );
    assert!(
        recorded
            .iter()
            .any(|a| a == &format!("RIWORK_CODEX_ACCOUNT_HOME={}", account_b.home.display()))
    );

    // No account named: the project's, as a new tab would have it.
    let default = manager
        .create_harness_with(
            project.id.clone(),
            None,
            root.clone(),
            HarnessKind::Codex,
            chosen(None),
        )
        .unwrap();
    assert_eq!(default.codex_account_id, account_a.id);
    assert!(!default.unrestricted);
    take_recorded(&capture);

    // Claude: its flags, and no account to give it.
    let claude = manager
        .create_harness_with(
            project.id.clone(),
            None,
            root.clone(),
            HarnessKind::Claude,
            chosen(Some(ApprovalMode::Plan)),
        )
        .unwrap();
    let arguments = shell_arguments(&claude.command.unwrap());
    assert!(contains_sequence(&arguments, &["--model", "the-model"]));
    assert!(contains_sequence(
        &arguments,
        &["--permission-mode", "plan"]
    ));
    assert_eq!((claude.codex_home, claude.codex_account_id), (None, None));
    take_recorded(&capture);
    let mut with_account = chosen(None);
    with_account.binding = Some(account_b);
    let error = manager
        .create_harness_with(project.id, None, root, HarnessKind::Claude, with_account)
        .unwrap_err();
    assert_eq!(error, "claude has no RiWork-managed accounts");
    assert!(take_recorded(&capture).is_empty(), "nothing was started");
}
