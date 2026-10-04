//! One Return, with a settled bracketed paste for nonempty text, under a shell lock.
use std::{path::Path, time::Duration};
use uuid::Uuid;

/// Like `session_viewport::Tmux`, but the second argument is fed to the tmux
/// client's stdin. Text travels this way because a tmux argument ending in `;`
/// is parsed as a command separator and long arguments are rejected.
pub type TmuxInput<'a> = dyn Fn(&[&str], &[u8]) -> Result<String, String> + 'a;

pub fn submit(
    home: &Path,
    id: &str,
    text: &str,
    t: &crate::session_viewport::Tmux<'_>,
    load: &TmuxInput<'_>,
) -> Result<(), String> {
    submit_checked(home, id, text, t, load, || Ok(()))
}

/// Run the scheduling gate inside the very same lock as ordinary `send`.
pub fn submit_checked(
    home: &Path,
    id: &str,
    text: &str,
    t: &crate::session_viewport::Tmux<'_>,
    load: &TmuxInput<'_>,
    gate: impl FnOnce() -> Result<(), String>,
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
    gate()?;
    // tmux does not retain an empty buffer. Preserve Return-only submission
    // under the same lock and mode checks without attempting an empty paste.
    if text.is_empty() {
        t(&["send-keys", "-t", &pane, "Enter"])?;
        return Ok(());
    }
    let buffer = format!("riwork-input-{}", Uuid::new_v4());
    let result = (|| {
        load(&["load-buffer", "-b", &buffer, "-"], text.as_bytes())?;
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

/// Paste `text` into a shell's pane as a terminal paste would, without Return: bracketed when
/// the program asked for bracketed paste, under the same input lock as `submit`. A pane in copy
/// mode is taken out of it first, as a paste in Ghostty scrolls back to the prompt; a pane whose
/// input is disabled is refused. Used for files dropped on a terminal.
pub fn paste(
    home: &Path,
    id: &str,
    text: &str,
    t: &crate::session_viewport::Tmux<'_>,
    load: &TmuxInput<'_>,
) -> Result<(), String> {
    if text.is_empty() {
        return Ok(());
    }
    let _lock = crate::session_viewport::lock(home, id, "input")?;
    let pane = format!("{id}:0.0");
    let status = t(&[
        "display-message",
        "-p",
        "-t",
        &pane,
        "#{pane_in_mode}|#{pane_input_off}",
    ])?;
    match status.trim() {
        "0|0" => {}
        "1|0" => {
            t(&["send-keys", "-t", &pane, "-X", "cancel"])?;
        }
        _ => return Err("terminal input is disabled for this pane".into()),
    }
    let buffer = format!("riwork-input-{}", Uuid::new_v4());
    let result = (|| {
        load(&["load-buffer", "-b", &buffer, "-"], text.as_bytes())?;
        // Without `-r` tmux turns line feeds into carriage returns, as a terminal paste does.
        t(&["paste-buffer", "-p", "-d", "-b", &buffer, "-t", &pane])?;
        Ok(())
    })();
    if result.is_err() {
        let _ = t(&["delete-buffer", "-b", &buffer]);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const SHELL: &str = "00000000-0000-4000-8000-0000000000aa";

    fn home() -> std::path::PathBuf {
        let home = std::env::temp_dir().join(format!("riwork-input-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        home
    }

    /// Records every tmux call. `fail` names a subcommand that returns an error.
    struct Recorder {
        calls: RefCell<Vec<Vec<String>>>,
        loaded: RefCell<Vec<u8>>,
        fail: &'static str,
    }

    impl Recorder {
        fn new(fail: &'static str) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                loaded: RefCell::new(Vec::new()),
                fail,
            }
        }

        fn run(&self, args: &[&str], input: Option<&[u8]>) -> Result<String, String> {
            self.calls
                .borrow_mut()
                .push(args.iter().map(|arg| arg.to_string()).collect());
            if let Some(input) = input {
                *self.loaded.borrow_mut() = input.to_vec();
            }
            if args[0] == self.fail {
                return Err(format!("{} failed", args[0]));
            }
            Ok(if args[0] == "display-message" {
                "0|0".into()
            } else {
                String::new()
            })
        }

        fn submit(&self, text: &str) -> Result<(), String> {
            let home = home();
            let result = submit(
                &home,
                SHELL,
                text,
                &|args| self.run(args, None),
                &|args, input| self.run(args, Some(input)),
            );
            let _ = std::fs::remove_dir_all(home);
            result
        }

        fn subcommands(&self) -> Vec<String> {
            self.calls
                .borrow()
                .iter()
                .map(|call| call[0].clone())
                .collect()
        }
    }

    #[test]
    fn text_reaches_tmux_on_stdin_never_as_an_argument() {
        let text = "select 1;";
        let tmux = Recorder::new("");
        tmux.submit(text).unwrap();
        assert_eq!(
            tmux.subcommands(),
            [
                "display-message",
                "load-buffer",
                "paste-buffer",
                "send-keys"
            ]
        );
        assert_eq!(&*tmux.loaded.borrow(), text.as_bytes());
        let calls = tmux.calls.borrow();
        assert!(calls.iter().flatten().all(|arg| arg != text));
        let buffer = &calls[1][2];
        assert_eq!(calls[1], ["load-buffer", "-b", buffer, "-"]);
        assert_eq!(
            calls[2],
            [
                "paste-buffer",
                "-p",
                "-r",
                "-d",
                "-b",
                buffer,
                "-t",
                &format!("{SHELL}:0.0")
            ]
        );
        assert_eq!(
            calls[3],
            ["send-keys", "-t", &format!("{SHELL}:0.0"), "Enter"]
        );
    }

    #[test]
    fn empty_text_only_sends_return() {
        let tmux = Recorder::new("");
        tmux.submit("").unwrap();
        assert_eq!(tmux.subcommands(), ["display-message", "send-keys"]);
    }

    #[test]
    fn failed_load_or_paste_deletes_the_buffer_and_never_sends_return() {
        for failing in ["load-buffer", "paste-buffer"] {
            let tmux = Recorder::new(failing);
            assert_eq!(
                tmux.submit("hello").unwrap_err(),
                format!("{failing} failed")
            );
            let subcommands = tmux.subcommands();
            assert_eq!(subcommands.last().unwrap(), "delete-buffer");
            assert!(!subcommands.contains(&"send-keys".to_string()));
            let calls = tmux.calls.borrow();
            let load = calls.iter().find(|call| call[0] == "load-buffer").unwrap();
            assert_eq!(calls.last().unwrap(), &["delete-buffer", "-b", &load[2]]);
        }
    }

    #[test]
    fn gate_and_mode_check_run_before_any_buffer_is_created() {
        let tmux = Recorder::new("");
        let home = home();
        let error = submit_checked(
            &home,
            SHELL,
            "hello",
            &|args| tmux.run(args, None),
            &|args, input| tmux.run(args, Some(input)),
            || Err("not idle".into()),
        )
        .unwrap_err();
        let _ = std::fs::remove_dir_all(home);
        assert_eq!(error, "not idle");
        assert_eq!(tmux.subcommands(), ["display-message"]);
    }

    #[test]
    fn paste_loads_on_stdin_and_pastes_bracketed_without_return() {
        let text = "/tmp/a\\ b.png";
        let tmux = Recorder::new("");
        let home = home();
        paste(
            &home,
            SHELL,
            text,
            &|args| tmux.run(args, None),
            &|args, input| tmux.run(args, Some(input)),
        )
        .unwrap();
        let _ = std::fs::remove_dir_all(home);
        assert_eq!(
            tmux.subcommands(),
            ["display-message", "load-buffer", "paste-buffer"]
        );
        assert_eq!(&*tmux.loaded.borrow(), text.as_bytes());
        let calls = tmux.calls.borrow();
        let buffer = &calls[1][2];
        assert_eq!(
            calls[2],
            [
                "paste-buffer",
                "-p",
                "-d",
                "-b",
                buffer,
                "-t",
                &format!("{SHELL}:0.0")
            ]
        );
    }

    #[test]
    fn paste_leaves_copy_mode_first_and_refuses_disabled_input() {
        let calls = RefCell::new(Vec::new());
        let run = |answer: &'static str| {
            let calls = &calls;
            move |args: &[&str]| -> Result<String, String> {
                calls
                    .borrow_mut()
                    .push(args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>());
                Ok(if args[0] == "display-message" {
                    answer.into()
                } else {
                    String::new()
                })
            }
        };
        let home = home();
        let in_mode = run("1|0");
        paste(&home, SHELL, "x", &in_mode, &|args, _| in_mode(args)).unwrap();
        let subcommands: Vec<String> = calls.borrow().iter().map(|c| c[0].clone()).collect();
        assert_eq!(
            subcommands,
            [
                "display-message",
                "send-keys",
                "load-buffer",
                "paste-buffer"
            ]
        );
        assert_eq!(calls.borrow()[1][3..], ["-X", "cancel"]);
        calls.borrow_mut().clear();
        let off = run("0|1");
        assert!(paste(&home, SHELL, "x", &off, &|args, _| off(args)).is_err());
        assert_eq!(calls.borrow().len(), 1);
        let _ = std::fs::remove_dir_all(home);
    }
}
