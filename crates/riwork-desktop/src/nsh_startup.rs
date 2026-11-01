//! Process environment handling before the GUI starts any worker.
/// A vanished explicit folder can still open the active project with a notice.
/// This desktop-only recovery runs after the unchanged CLI has rejected it.
pub(crate) fn missing_folder_request(args: &[String]) -> bool {
    let args: Vec<_> = args.iter().filter(|a| !a.starts_with("-psn_")).collect();
    matches!(args.as_slice(), [path] if !path.starts_with('-')
        && (path.starts_with('/') || path.starts_with("./") || path.starts_with("../"))
        && !std::path::Path::new(path).exists())
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_an_explicit_missing_folder_gets_desktop_recovery() {
        for path in [
            "/definitely/missing/riwork-project",
            "./missing/riwork-project",
            "../missing/riwork-project",
        ] {
            assert!(super::missing_folder_request(&[path.into()]));
        }
        for args in [
            vec!["unknown".into()],
            vec!["--unknown".into()],
            vec!["open".into(), "/missing".into()],
        ] {
            assert!(!super::missing_folder_request(&args));
        }
    }
}

/// What a window must not inherit from the shell or agent that launched it (a reload hands the old app's environment to the
/// new one): RiWork's own shell, chat and orchestrator identity, the tmux client it ran in, and the agent session around it.
/// With `RIWORK_ORCHESTRATOR_SCOPE` left set, every shell the window makes is saved as an agent's hidden worker.
const GUI_INHERITED_IDENTITY: &[&str] = &[
    "RIWORK_CHAT_ID",
    "RIWORK_SHELL_ID",
    "RIWORK_ORCHESTRATOR_SCOPE",
    "RIWORK_PROJECT_ID",
    "RIWORK_CODEX_SHELL_ID",
    "TMUX",
    "TMUX_PANE",
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_PID",
    "CODEX_THREAD_ID",
    "CODEX_SESSION_ID",
    "CODEX_CI",
];

pub(crate) fn scrub_gui_child_ids() {
    // Main calls this before starting threads. CLI dispatch deliberately runs
    // first and preserves the invoking shell/chat identity.
    for name in GUI_INHERITED_IDENTITY {
        unsafe { std::env::remove_var(name) };
    }
}
