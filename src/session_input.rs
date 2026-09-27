//! One Return, with a settled bracketed paste for nonempty text, under a shell lock.
use std::{path::Path, time::Duration};
use uuid::Uuid;

pub fn submit(
    home: &Path,
    id: &str,
    text: &str,
    t: &crate::session_viewport::Tmux<'_>,
) -> Result<(), String> {
    let _lock = crate::session_viewport::lock(home, id, "input")?;
    let pane = format!("{id}:0.0");
    let status = t(&[
        "display-message",
        "-p",
        "-t",
        &pane,
        "#{pane_in_mode}|#{pane_input_off}",
    ])?;
    if status.trim() != "0|0" {
        return Err("leave copy mode and enable terminal input before submitting".into());
    }
    // tmux does not retain an empty set-buffer. Preserve Return-only submission
    // under the same lock and mode checks without attempting an empty paste.
    if text.is_empty() {
        t(&["send-keys", "-t", &pane, "Enter"])?;
        return Ok(());
    }
    let buffer = format!("riwork-input-{}", Uuid::new_v4());
    t(&["set-buffer", "-b", &buffer, "--", text])?;
    let result = (|| {
        t(&["paste-buffer", "-p", "-r", "-d", "-b", &buffer, "-t", &pane])?;
        // Codex's documented paste suppression window is 120 ms. Bracketed
        // paste avoids character-burst reclassification; give the TUI 500 ms
        // to consume the completed block before exactly one submit gesture.
        // Never retry Return: uncertain submission is handled by the RPC ledger.
        std::thread::sleep(Duration::from_millis(500));
        t(&["send-keys", "-t", &pane, "Enter"])?;
        Ok(())
    })();
    if result.is_err() {
        let _ = t(&["delete-buffer", "-b", &buffer]);
    }
    result
}
