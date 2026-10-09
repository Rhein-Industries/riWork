//! `riwork shell paste ID [--json] -- FILE...` as the remote connector's `shell.paste` runs it
//! (`remote/src/rpc/upload.rs`): the paths of existing files reach the shell as a drop of those
//! files on its terminal would type them, without Return, and errors that start with a token
//! happened before anything was pasted. `shell close` takes the shell's upload inbox with it.
//! Each run has a throwaway RIWORK_HOME and its own tmux server; nothing touches a real one.

use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

struct Home {
    path: PathBuf,
}

impl Home {
    fn new() -> Self {
        // macOS temp dirs are symlinks; the CLI keys its tmux socket by the canonical path.
        let path = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("riwork-paste-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_riwork"))
            .args(args)
            .env("RIWORK_HOME", &self.path)
            .env("RIWORK_RUNTIME_DIR", self.path.join("runtime"))
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> Value {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn error(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(!output.status.success(), "{args:?} succeeded");
        String::from_utf8_lossy(&output.stderr).into_owned()
    }

    /// A shell running `command` in a new project, its id.
    fn shell(&self, command: &str) -> String {
        let root = self.path.join("app");
        fs::create_dir_all(&root).unwrap();
        self.ok(&["project", "add", root.to_str().unwrap(), "--json"]);
        let projects = self.ok(&["project", "list", "--json"]);
        let project = projects[0]["id"].as_str().unwrap().to_owned();
        let shell = self.ok(&[
            "shell",
            "create",
            "--project",
            &project,
            "--command",
            command,
            "--json",
        ]);
        shell["id"].as_str().unwrap().to_owned()
    }

    /// The screen with its rows joined: a long path wraps.
    fn screen(&self, shell: &str) -> String {
        self.ok(&["shell", "output", shell, "--json"])["output"]
            .as_str()
            .unwrap()
            .replace('\n', "")
    }

    fn wait_for(&self, shell: &str, text: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let screen = self.screen(shell);
            if screen.contains(text) || Instant::now() > deadline {
                return screen;
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        if let Ok(registry) = fs::read(self.path.join("sessions.json")) {
            let registry: Value = serde_json::from_slice(&registry).unwrap_or(Value::Null);
            for session in registry["sessions"].as_array().into_iter().flatten() {
                if let Some(id) = session["id"].as_str() {
                    let _ = self.run(&["shell", "close", id]);
                }
            }
        }
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn file(dir: &Path, name: &str) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    fs::write(&path, b"x").unwrap();
    path
}

#[test]
#[ignore = "slow: real tmux shell"]
fn paths_reach_the_shell_as_a_drop_types_them_and_close_takes_the_inbox() {
    let home = Home::new();
    // `cat` in front: not an agent, so the paths are escaped and joined like Ghostty's drop.
    let shell = home.shell("cat -v");
    let inbox = home.path.join("uploads").join(&shell);
    let first = file(&inbox, "a b.png");
    let second = file(&inbox, "c(1).txt");
    let answer = home.ok(&[
        "shell",
        "paste",
        &shell,
        "--json",
        "--",
        first.to_str().unwrap(),
        second.to_str().unwrap(),
    ]);
    assert_eq!(answer["id"], shell.as_str());
    assert_eq!(answer["pasted"], 2);
    let expected = format!(
        "{} {}",
        first.to_str().unwrap().replace(' ', "\\ "),
        second
            .to_str()
            .unwrap()
            .replace('(', "\\(")
            .replace(')', "\\)")
    );
    let screen = home.wait_for(&shell, &expected);
    assert!(screen.contains(&expected), "{screen:?}");
    // No Return: cat has not printed the line back yet.
    assert_eq!(screen.matches(&expected).count(), 1, "{screen:?}");

    // `--json` after `--` is a file name, not the flag.
    let error = home.error(&["shell", "paste", &shell, "--", "--json"]);
    assert!(
        error.contains("invalid_request: not an existing file"),
        "{error}"
    );

    home.ok(&["shell", "close", &shell, "--json"]);
    assert!(!inbox.exists());
    assert!(home.path.join("uploads").is_dir());
}

#[test]
#[ignore = "slow: real tmux shell"]
fn refusals_name_their_token_and_paste_nothing() {
    let home = Home::new();
    let shell = home.shell("cat -v");
    let real = file(&home.path.join("uploads").join(&shell), "a.png");
    let real = real.to_str().unwrap();
    let many: Vec<String> = (0..17).map(|_| real.to_owned()).collect();
    let mut too_many = vec!["shell", "paste", shell.as_str(), "--"];
    too_many.extend(many.iter().map(String::as_str));
    let missing = home.path.join("missing.png");
    let cases: Vec<(Vec<&str>, &str)> = vec![
        (vec!["shell", "paste", &shell], "invalid_request: Usage"),
        (
            vec!["shell", "paste", &shell, "--"],
            "invalid_request: shell paste takes 1 to 16",
        ),
        (too_many, "invalid_request: shell paste takes 1 to 16"),
        (
            vec!["shell", "paste", &shell, "--", "relative.png"],
            "invalid_request: not an existing file",
        ),
        (
            vec!["shell", "paste", &shell, "--", missing.to_str().unwrap()],
            "invalid_request: not an existing file",
        ),
        (
            vec!["shell", "paste", &shell, "--", home.path.to_str().unwrap()],
            "invalid_request: not an existing file",
        ),
        (
            vec![
                "shell",
                "paste",
                "00000000-0000-4000-8000-000000000000",
                "--",
                real,
            ],
            "not_found: unknown shell",
        ),
        (
            vec!["shell", "paste", "abc", "--", real],
            "invalid_request:",
        ),
    ];
    for (args, expected) in cases {
        let error = home.error(&args);
        assert!(
            error.starts_with(&format!("riwork: {expected}")),
            "{args:?}: {error}"
        );
    }
    thread::sleep(Duration::from_millis(200));
    assert!(!home.screen(&shell).contains("a.png"));
}
