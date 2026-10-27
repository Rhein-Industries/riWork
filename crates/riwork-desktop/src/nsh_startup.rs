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

pub(crate) fn scrub_gui_child_ids() {
    // Main calls this before starting threads. CLI dispatch deliberately runs
    // first and preserves the invoking shell/chat identity.
    unsafe {
        std::env::remove_var("RIWORK_CHAT_ID");
        std::env::remove_var("RIWORK_SHELL_ID");
    }
}
