//! Headless shell/process routing for dropped or pasted paths.

use crate::{session_input::Input, sessions::HarnessKind};
use std::path::{Path, PathBuf};

/// The characters Ghostty puts a backslash before in a dropped path (`Ghostty.Shell.escape`).
const ESCAPED: &str = "\\ ()[]{}<>\"'`!#$&;|*?\t";

/// Control-free `text` with a backslash before every character a shell would read specially.
/// File paths must pass `validate_paths` before being escaped for terminal input.
pub fn shell_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if ESCAPED.contains(character) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

/// Refuse an entire file batch before any terminal input. Bracketed paste is optional,
/// and tmux converts line feeds to Return: a filename must never become a key or command.
pub fn validate_paths(paths: &[PathBuf]) -> Result<(), String> {
    if paths.iter().any(|path| {
        path.to_string_lossy()
            .chars()
            .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
    }) {
        return Err(
            "File paths containing control characters or line separators cannot be pasted into a terminal."
                .into(),
        );
    }
    Ok(())
}

/// What a drop of `paths` types: each path escaped, separated by spaces, with nothing after the
/// last, as Ghostty does. `None` for no paths or an unsafe path; callers use `validate_paths`
/// to report the reason before attempting a paste.
pub fn dropped_text(paths: &[PathBuf]) -> Option<String> {
    (!paths.is_empty() && validate_paths(paths).is_ok()).then(|| {
        paths
            .iter()
            .map(|path| shell_escape(&path.to_string_lossy()))
            .collect::<Vec<_>>()
            .join(" ")
    })
}

/// What a terminal's foreground program does with a paste of paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Program {
    /// A shell, or any program that is not one of the agents: it gets what Ghostty would paste.
    Shell,
    Claude,
    Codex,
    Grok,
    Muse,
}

/// The agent a process name stands for. Claude Code's process is named after its version
/// (`2.1.289`); Grok's after its build (`grok-1.0.46-mac`).
fn agent(name: &str) -> Option<Program> {
    let name = name.trim().trim_start_matches('-');
    let name = name.rsplit('/').next().unwrap_or(name);
    match name {
        "claude" => Some(Program::Claude),
        "codex" => Some(Program::Codex),
        "grok" => Some(Program::Grok),
        "muse" => Some(Program::Muse),
        _ if name.starts_with("grok-") => Some(Program::Grok),
        _ if !name.is_empty() && name.chars().all(|c| c.is_ascii_digit() || c == '.') => {
            Some(Program::Claude)
        }
        _ => None,
    }
}

fn shell(name: &str) -> bool {
    let name = name.trim().trim_start_matches('-');
    let name = name.rsplit('/').next().unwrap_or(name);
    matches!(
        name,
        "zsh" | "bash" | "sh" | "fish" | "dash" | "ksh" | "tcsh" | "csh" | "nu" | "login"
    )
}

/// The program in front in a pane whose tmux `pane_current_command` is `command`, in a shell
/// started for `harness`. `foreground` names the processes in front on the pane's terminal; it
/// is asked only when `command` does not settle it, as for Codex installed by npm, which runs
/// as `node` with the real `codex` beside it. An agent RiWork started may also run under
/// RiWork's launcher, where the harness tells. A shell in front means the agent is gone.
pub fn program(
    harness: Option<HarnessKind>,
    command: &str,
    foreground: impl FnOnce() -> Vec<String>,
) -> Program {
    if let Some(program) = agent(command) {
        return program;
    }
    if shell(command) {
        return Program::Shell;
    }
    if let Some(program) = foreground().iter().find_map(|name| agent(name)) {
        return program;
    }
    match harness {
        Some(HarnessKind::Claude) => Program::Claude,
        Some(HarnessKind::Codex) => Program::Codex,
        Some(HarnessKind::Grok) => Program::Grok,
        Some(HarnessKind::Muse) => Program::Muse,
        None => Program::Shell,
    }
}

/// The names of the processes in the foreground of terminal `tty` (`/dev/ttys001`).
pub fn foreground_names(tty: &str) -> Vec<String> {
    let Some(tty) = tty.strip_prefix("/dev/").filter(|tty| !tty.is_empty()) else {
        return Vec::new();
    };
    let Ok(output) = std::process::Command::new("/bin/ps")
        .args(["-t", tty, "-o", "stat=,comm="])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.trim().split_once(char::is_whitespace))
        .filter(|(stat, _)| stat.contains('+'))
        .map(|(_, name)| name.trim().to_owned())
        .collect()
}

/// Whether Codex would likely take `path` for a picture: it reads the file, RiWork guesses from
/// the extension, which only decides whether a space follows.
fn picture(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ["png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff"]
                .contains(&extension.to_ascii_lowercase().as_str())
        })
}

/// The pastes that put `paths` into `program`: the text Ghostty would paste, or for Codex one
/// paste per path (see the module comment).
pub fn path_input(program: Program, paths: &[PathBuf]) -> Vec<Input> {
    if validate_paths(paths).is_err() {
        return Vec::new();
    }
    if program != Program::Codex {
        return dropped_text(paths).map(Input::Paste).into_iter().collect();
    }
    paths
        .iter()
        .enumerate()
        .map(|(index, path)| {
            let mut text = shell_escape(&path.to_string_lossy());
            if index + 1 < paths.len() && !picture(path) {
                text.push(' ');
            }
            Input::Paste(text)
        })
        .collect()
}

/// What ⌘V finds on the pasteboard, as far as the terminal is concerned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Copied {
    /// Files, copied in Finder: their paths go in as for a drop.
    Files(Vec<PathBuf>),
    /// A picture and no text and no file: an agent reads it from the pasteboard on Ctrl+V.
    Picture,
    /// Text, or nothing: Ghostty's own paste.
    Text,
}

/// What ⌘V with `copied` on the pasteboard gives `program`; `None` leaves ⌘V to Ghostty.
pub fn paste_input(copied: &Copied, program: Program) -> Option<Vec<Input>> {
    match copied {
        Copied::Files(paths) => Some(path_input(program, paths)),
        Copied::Picture if program == Program::Shell => Some(Vec::new()),
        Copied::Picture => Some(vec![Input::Key("C-v")]),
        Copied::Text => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_what_ghostty_escapes() {
        assert_eq!(
            shell_escape("/Users/me/Desktop/Screenshot 2026-10-04 at 10.00.00.png"),
            "/Users/me/Desktop/Screenshot\\ 2026-10-04\\ at\\ 10.00.00.png"
        );
        assert_eq!(
            shell_escape("a\\b c(d)[e]{f}<g>\"h'i`j!k#l$m&n;o|p*q?r\ts"),
            "a\\\\b\\ c\\(d\\)\\[e\\]\\{f\\}\\<g\\>\\\"h\\'i\\`j\\!k\\#l\\$m\\&n\\;o\\|p\\*q\\?r\\\ts"
        );
        // Ghostty leaves these alone.
        assert_eq!(
            shell_escape("/a/b-c_d.e,f=g~h%i^j+k@l:m"),
            "/a/b-c_d.e,f=g~h%i^j+k@l:m"
        );
        assert_eq!(shell_escape("/tmp/Bild ü.png"), "/tmp/Bild\\ ü.png");
    }

    #[test]
    fn control_character_paths_refuse_the_whole_batch_without_terminal_input() {
        for control in [
            '\n', '\r', '\t', '\u{1b}', '\u{7f}', '\u{85}', '\u{2028}', '\u{2029}',
        ] {
            let paths = [
                PathBuf::from("/tmp/ordinary file.png"),
                PathBuf::from(format!("/tmp/path{control}false")),
            ];
            let error = validate_paths(&paths).unwrap_err();
            assert!(
                !error.contains(control),
                "unsafe filename leaked into the error"
            );
            assert!(dropped_text(&paths).is_none());
            for program in [
                Program::Shell,
                Program::Claude,
                Program::Codex,
                Program::Grok,
                Program::Muse,
            ] {
                assert!(path_input(program, &paths).is_empty());
                assert_eq!(
                    paste_input(&Copied::Files(paths.to_vec()), program),
                    Some(Vec::new()),
                    "unsafe clipboard paths must not fall through to a native text paste"
                );
            }
        }
        assert!(validate_paths(&[PathBuf::from("/tmp/Bild ü 'quoted'.png")]).is_ok());
    }

    #[test]
    fn the_program_in_front_decides_and_a_shell_in_front_means_the_agent_is_gone() {
        use HarnessKind::{Claude, Codex, Grok};
        let none = Vec::new;
        let unasked = || -> Vec<String> { panic!("the command settles it") };
        assert_eq!(program(None, "zsh", unasked), Program::Shell);
        assert_eq!(program(None, "-zsh\n", unasked), Program::Shell);
        assert_eq!(program(None, "vim", none), Program::Shell);
        assert_eq!(program(None, "claude", unasked), Program::Claude);
        // Claude Code's process carries its version as its name.
        assert_eq!(program(None, "2.1.289", unasked), Program::Claude);
        assert_eq!(program(None, "codex", unasked), Program::Codex);
        assert_eq!(program(None, "grok-1.0.46-mac", unasked), Program::Grok);
        // Codex from npm: node in front, the real binary beside it.
        let npm = || {
            vec![
                "node".to_owned(),
                "/usr/lib/node_modules/@openai/codex/vendor/bin/codex".to_owned(),
            ]
        };
        assert_eq!(program(None, "node", npm), Program::Codex);
        assert_eq!(
            program(None, "node", || vec!["node".to_owned()]),
            Program::Shell
        );
        // An agent RiWork started may run under a launcher.
        assert_eq!(program(Some(Codex), "node", none), Program::Codex);
        assert_eq!(program(Some(Claude), "riwork", none), Program::Claude);
        assert_eq!(program(Some(Grok), "riwork", none), Program::Grok);
        assert_eq!(program(Some(Codex), "zsh", unasked), Program::Shell);
        assert_eq!(program(Some(Claude), "codex", unasked), Program::Codex);
    }

    #[test]
    fn codex_gets_one_paste_per_path_and_the_others_ghosttys_text() {
        let paths = [
            PathBuf::from("/tmp/Screen Shot.png"),
            PathBuf::from("/tmp/notes file.txt"),
            PathBuf::from("/tmp/b.JPG"),
            PathBuf::from("/tmp/c"),
        ];
        let paste = |text: &str| Input::Paste(text.to_owned());
        assert_eq!(
            path_input(Program::Codex, &paths),
            [
                paste("/tmp/Screen\\ Shot.png"),
                paste("/tmp/notes\\ file.txt "),
                paste("/tmp/b.JPG"),
                paste("/tmp/c"),
            ]
        );
        let joined = [paste(
            "/tmp/Screen\\ Shot.png /tmp/notes\\ file.txt /tmp/b.JPG /tmp/c",
        )];
        for program in [Program::Shell, Program::Claude, Program::Grok] {
            assert_eq!(path_input(program, &paths), joined);
        }
        assert!(path_input(Program::Codex, &[]).is_empty());
        assert!(path_input(Program::Shell, &[]).is_empty());
    }

    #[test]
    fn a_lone_picture_is_ctrl_v_for_an_agent_and_nothing_for_a_shell() {
        assert_eq!(
            paste_input(&Copied::Picture, Program::Claude),
            Some(vec![Input::Key("C-v")])
        );
        assert_eq!(
            paste_input(&Copied::Picture, Program::Codex),
            Some(vec![Input::Key("C-v")])
        );
        assert_eq!(
            paste_input(&Copied::Picture, Program::Grok),
            Some(vec![Input::Key("C-v")])
        );
        assert_eq!(
            paste_input(&Copied::Picture, Program::Shell),
            Some(Vec::new())
        );
        assert_eq!(paste_input(&Copied::Text, Program::Claude), None);
        assert_eq!(
            paste_input(
                &Copied::Files(vec![PathBuf::from("/tmp/x")]),
                Program::Shell
            ),
            Some(vec![Input::Paste("/tmp/x".into())])
        );
    }
}
