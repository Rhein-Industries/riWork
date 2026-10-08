//! `riwork shell create` as the remote connector drives it for the phone's
//! `shell.create`: the argv shapes it sends, the JSON it reads back (the very
//! entry `shell list` would show), and the sentences of the failures it turns
//! into error codes (`remote/src/rpc.rs`, `create_fault`). Each child runs in a
//! throwaway RIWORK_HOME with its own tmux server; nothing touches a real one.

use serde_json::{Value, json};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

struct Home(PathBuf);

impl Home {
    fn new() -> Self {
        // macOS temp dirs are symlinks; project roots are stored canonically.
        let path = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("riwork-shell-create-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_riwork"));
        command
            .args(args)
            .env("RIWORK_HOME", &self.0)
            .env("RIWORK_RUNTIME_DIR", self.runtime())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn runtime(&self) -> PathBuf {
        self.0.join("runtime")
    }

    fn run(&self, args: &[&str]) -> Output {
        finish(self.command(args).spawn().unwrap(), args)
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

    /// A project with a Git root, its id and its primary worktree's id.
    fn project(&self, name: &str) -> (String, String, PathBuf) {
        let root = self.0.join(name);
        fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "--initial-branch=main", "--template="]);
        git(&root, &["commit", "--allow-empty", "-m", "fixture"]);
        let project = self.ok(&["project", "add", root.to_str().unwrap(), "--json"]);
        let project = project["id"].as_str().unwrap().to_owned();
        let worktrees = self.ok(&["worktree", "list", "--project", &project, "--json"]);
        let worktree = worktrees[0]["id"].as_str().unwrap().to_owned();
        (project, worktree, root.canonicalize().unwrap())
    }

    fn shells(&self) -> Vec<Value> {
        self.ok(&["shell", "list", "--all", "--json"])
            .as_array()
            .unwrap()
            .clone()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        // Close fixture shells so their tmux server (private to this home) exits.
        if let Ok(registry) = fs::read(self.0.join("sessions.json")) {
            let registry: Value = serde_json::from_slice(&registry).unwrap_or(Value::Null);
            for session in registry["sessions"].as_array().into_iter().flatten() {
                if let Some(id) = session["id"].as_str() {
                    let _ = self.command(&["shell", "close", id]).output();
                }
            }
        }
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn finish(mut child: Child, what: &[&str]) -> Output {
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("riwork {what:?} did not exit; it may have started the GUI");
        }
        thread::sleep(Duration::from_millis(20));
    };
    let mut output = Output {
        status,
        stdout: Vec::new(),
        stderr: Vec::new(),
    };
    if let Some(mut stdout) = child.stdout.take() {
        stdout.read_to_end(&mut output.stdout).unwrap();
    }
    if let Some(mut stderr) = child.stderr.take() {
        stderr.read_to_end(&mut output.stderr).unwrap();
    }
    output
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "user.name=RiWork Tests"])
        .args(["-c", "user.email=riwork-tests@example.invalid"])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The argv the connector builds for a validated `shell.create`, then `--json`.
fn connector_argv(
    target: (&str, &str),
    harness: Option<&str>,
    unrestricted: bool,
    command: Option<&str>,
) -> Vec<String> {
    let mut args: Vec<String> = ["shell", "create", target.0, target.1]
        .map(String::from)
        .to_vec();
    if let Some(harness) = harness {
        args.extend(["--harness".into(), harness.into()]);
        if unrestricted {
            args.push("--unrestricted".into());
        }
    }
    if let Some(command) = command {
        args.extend(["--command".into(), command.into()]);
    }
    args.push("--json".into());
    args
}

fn run_vec(home: &Home, args: &[String]) -> Output {
    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
    home.run(&borrowed)
}

fn is_canonical_uuid(text: &str) -> bool {
    Uuid::parse_str(text).is_ok_and(|uuid| uuid.to_string() == text)
}

#[test]
#[ignore = "slow: real tmux shells"]
fn create_prints_the_session_that_shell_list_shows_for_a_project_or_a_worktree() {
    let home = Home::new();
    let (project, worktree, root) = home.project("app");
    let keep_running = "exec sleep 300";

    // The connector's argv shapes for a plain shell, with and without a command.
    let by_project = run_vec(
        &home,
        &connector_argv(("--project", &project), None, false, Some(keep_running)),
    );
    assert!(
        by_project.status.success(),
        "{}",
        String::from_utf8_lossy(&by_project.stderr)
    );
    let by_project: Value = serde_json::from_slice(&by_project.stdout).unwrap();
    let by_worktree = run_vec(
        &home,
        &connector_argv(("--worktree", &worktree), None, false, Some(keep_running)),
    );
    assert!(
        by_worktree.status.success(),
        "{}",
        String::from_utf8_lossy(&by_worktree.stderr)
    );
    let by_worktree: Value = serde_json::from_slice(&by_worktree.stdout).unwrap();

    for created in [&by_project, &by_worktree] {
        assert!(
            is_canonical_uuid(created["id"].as_str().unwrap()),
            "{created}"
        );
        assert_eq!(created["kind"], "project");
        assert_eq!(created["project_id"], project.as_str());
        // A project alone goes to its primary worktree.
        assert_eq!(created["worktree_id"], worktree.as_str());
        assert_eq!(created["cwd"], root.to_str().unwrap());
        assert_eq!(created["harness"], Value::Null);
        assert_eq!(created["alive"], true);
        assert!(created["created_at_unix"].as_u64().unwrap() > 1_700_000_000);
    }
    assert_ne!(by_project["id"], by_worktree["id"]);

    // Exactly the entry `shell list` has for it: the phone can use it as is.
    let listed = home.ok(&["shell", "list", "--project", &project, "--json"]);
    for created in [&by_project, &by_worktree] {
        let entry = listed
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["id"] == created["id"])
            .unwrap_or_else(|| panic!("{created} is not listed: {listed}"));
        // `shell create` prints what it just made; the list adds when tmux last saw output.
        let mut entry = entry.clone();
        entry.as_object_mut().unwrap().remove("last_activity_unix");
        assert_eq!(&entry, created);
    }

    // Both together are allowed when they agree.
    let both = run_vec(
        &home,
        &[
            "shell",
            "create",
            "--project",
            &project,
            "--worktree",
            &worktree,
            "--command",
            keep_running,
            "--json",
        ]
        .map(String::from),
    );
    assert!(
        both.status.success(),
        "{}",
        String::from_utf8_lossy(&both.stderr)
    );

    // A plain shell with no command is the user's own shell.
    let plain = run_vec(
        &home,
        &connector_argv(("--worktree", &worktree), None, false, None),
    );
    assert!(
        plain.status.success(),
        "{}",
        String::from_utf8_lossy(&plain.stderr)
    );
    let plain: Value = serde_json::from_slice(&plain.stdout).unwrap();
    assert_eq!(plain["command"], Value::Null);
    assert_eq!(plain["harness"], Value::Null);

    // Closing prints what the connector's `shell.close` relies on, and the
    // terminal is gone from the list.
    let id = by_project["id"].as_str().unwrap();
    assert_eq!(
        home.ok(&["shell", "close", id, "--json"]),
        json!({"id": id, "closed": true})
    );
    assert!(home.shells().iter().all(|entry| entry["id"] != id));
    assert!(!home.runtime().exists(), "a GUI instance was registered");
}

#[test]
#[ignore = "slow: real tmux shells"]
fn a_command_is_one_argument_and_reaches_the_terminal_as_typed() {
    let home = Home::new();
    let (_, worktree, _) = home.project("app");
    // Quotes, `;`, `$()`, backticks and unicode in one argument; the terminal
    // prints it back, so nothing but the terminal's own shell has read it.
    let command = "printf '%s\\n' 'a \"b\" ; $(echo no) `echo no` \u{e9}'; exec sleep 300";
    let created = home.ok(&[
        "shell",
        "create",
        "--worktree",
        &worktree,
        "--command",
        command,
        "--json",
    ]);
    let id = created["id"].as_str().unwrap().to_owned();
    assert_eq!(created["command"], command);
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let screen = home.ok(&["shell", "output", &id, "--lines", "20", "--json"]);
        if screen["output"]
            .as_str()
            .unwrap()
            .contains("a \"b\" ; $(echo no) `echo no` \u{e9}")
        {
            break;
        }
        assert!(Instant::now() < deadline, "the command never ran: {screen}");
        thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn failures_say_what_the_connector_turns_into_error_codes() {
    let home = Home::new();
    let (project, worktree, _) = home.project("app");
    let unknown = Uuid::new_v4().to_string();
    let stderr = |args: &[&str]| {
        let output = home.run(args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        String::from_utf8_lossy(&output.stderr).trim().to_owned()
    };
    // A project or worktree that does not exist: the connector says not_found.
    let message = stderr(&["shell", "create", "--project", &unknown, "--json"]);
    assert_eq!(message, format!("riwork: No project matches '{unknown}'"));
    let message = stderr(&["shell", "create", "--worktree", &unknown, "--json"]);
    assert_eq!(message, format!("riwork: No worktree matches '{unknown}'"));
    // The target must belong together.
    let other = home.project("other");
    let message = stderr(&[
        "shell",
        "create",
        "--project",
        &other.0,
        "--worktree",
        &worktree,
        "--json",
    ]);
    assert_eq!(message, "riwork: Worktree belongs to another project");
    // Options that make no sense together, which the connector refuses first.
    assert_eq!(
        stderr(&[
            "shell",
            "create",
            "--project",
            &project,
            "--unrestricted",
            "--json"
        ]),
        "riwork: --unrestricted requires --harness"
    );
    assert_eq!(
        stderr(&[
            "shell",
            "create",
            "--project",
            &project,
            "--harness",
            "codex",
            "--command",
            "ls",
            "--json"
        ]),
        "riwork: Use either --harness or --command"
    );
    assert_eq!(
        stderr(&[
            "shell",
            "create",
            "--project",
            &project,
            "--harness",
            "bash",
            "--json"
        ]),
        "riwork: --harness must be codex, claude, or grok"
    );
    // The reason the connector never sends a command that begins with `-`.
    assert_eq!(
        stderr(&[
            "shell",
            "create",
            "--project",
            &project,
            "--command",
            "--json"
        ]),
        "riwork: --command needs a value"
    );
    assert_eq!(
        stderr(&[
            "shell",
            "create",
            "--project",
            &project,
            "--command",
            "--harness",
            "--json"
        ]),
        "riwork: --command needs a value"
    );
    // None of the failures left a terminal behind or started a window.
    assert!(home.shells().is_empty());
    assert!(!home.runtime().exists(), "a GUI instance was registered");
}

#[test]
#[ignore = "slow: real tmux shells"]
fn an_agent_is_restricted_unless_unrestricted_is_asked_for() {
    let home = Home::new();
    let (project, worktree, _) = home.project("app");
    // A stand-in agent CLI and Cua driver, first on PATH, so the real ones are
    // never started and the test does not depend on what is installed.
    let bin = home.0.join("bin");
    fs::create_dir_all(&bin).unwrap();
    for (name, body) in [("claude", "exec sleep 300"), ("cua-driver", "exit 0")] {
        let path = bin.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let agent = |argv: &[String]| -> Value {
        let borrowed: Vec<&str> = argv.iter().map(String::as_str).collect();
        let mut command = home.command(&borrowed);
        command
            .env("PATH", &path)
            .env("RIWORK_CUA_DRIVER", bin.join("cua-driver"));
        let output = finish(command.spawn().unwrap(), &borrowed);
        assert!(
            output.status.success(),
            "{argv:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    };

    let restricted = agent(&connector_argv(
        ("--worktree", &worktree),
        Some("claude"),
        false,
        None,
    ));
    assert_eq!(restricted["harness"], "claude");
    assert_eq!(restricted["unrestricted"], false);
    assert_eq!(restricted["worktree_id"], worktree.as_str());
    assert_eq!(restricted["alive"], true);
    assert!(
        !restricted["command"]
            .as_str()
            .unwrap()
            .contains("--dangerously-skip-permissions"),
        "{restricted}"
    );
    let unrestricted = agent(&connector_argv(
        ("--project", &project),
        Some("claude"),
        true,
        None,
    ));
    assert_eq!(unrestricted["harness"], "claude");
    assert_eq!(unrestricted["unrestricted"], true);
    assert!(
        unrestricted["command"]
            .as_str()
            .unwrap()
            .contains("--dangerously-skip-permissions"),
        "{unrestricted}"
    );
    // The same entries `shell list` gives the phone, which adds the optional `activity` fields
    // (`shell create` prints what it just made and does not know them).
    let mut listed = home.shells();
    for entry in &mut listed {
        for additive in [
            "activity",
            "activity_since_unix",
            "subagents_working",
            "subagent_kinds",
            "last_activity_unix",
        ] {
            entry.as_object_mut().unwrap().remove(additive);
        }
    }
    for created in [&restricted, &unrestricted] {
        assert!(
            listed.iter().any(|entry| entry == created),
            "{created} not in {listed:?}"
        );
    }
}

#[test]
#[ignore = "slow: real tmux shells"]
fn as_settings_follows_the_agent_terminals_setting_and_needs_an_agent() {
    let home = Home::new();
    let (_, worktree, _) = home.project("app");
    let bin = home.0.join("bin");
    fs::create_dir_all(&bin).unwrap();
    for (name, body) in [("claude", "exec sleep 300"), ("cua-driver", "exit 0")] {
        let path = bin.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let run = |args: &[&str]| -> Output {
        let mut command = home.command(args);
        command
            .env("PATH", &path)
            .env("RIWORK_CUA_DRIVER", bin.join("cua-driver"));
        finish(command.spawn().unwrap(), args)
    };
    let create = |extra: &[&str]| -> Value {
        let mut args = vec![
            "shell",
            "create",
            "--worktree",
            &worktree,
            "--harness",
            "claude",
        ];
        args.extend_from_slice(extra);
        args.push("--json");
        let output = run(&args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    };
    let bypass = |shell: &Value| {
        shell["command"]
            .as_str()
            .unwrap()
            .contains("--dangerously-skip-permissions")
    };

    assert_eq!(
        home.ok(&["capabilities", "--json"])["shell_create_as_settings"],
        true
    );
    // No settings file: on by default.
    let shell = create(&["--as-settings"]);
    assert_eq!(shell["unrestricted"], true, "{shell}");
    assert!(bypass(&shell), "{shell}");
    // Without the flag the CLI keeps its old meaning: restricted unless asked.
    let shell = create(&[]);
    assert_eq!(shell["unrestricted"], false, "{shell}");
    assert!(!bypass(&shell), "{shell}");

    // Turned off in Settings: the same request asks before it acts.
    fs::write(
        home.0.join("settings.json"),
        r#"{"schema_version":1,"agent_terminals_unrestricted":false}"#,
    )
    .unwrap();
    let shell = create(&["--as-settings"]);
    assert_eq!(shell["unrestricted"], false, "{shell}");
    assert!(!bypass(&shell), "{shell}");
    // An explicit --unrestricted is still honored.
    let shell = create(&["--unrestricted"]);
    assert_eq!(shell["unrestricted"], true, "{shell}");

    // A plain shell is never unrestricted, and the two flags do not go together.
    for (args, error) in [
        (
            vec![
                "shell",
                "create",
                "--worktree",
                worktree.as_str(),
                "--as-settings",
            ],
            "--as-settings requires --harness",
        ),
        (
            vec![
                "shell",
                "create",
                "--worktree",
                worktree.as_str(),
                "--harness",
                "claude",
                "--as-settings",
                "--unrestricted",
            ],
            "Use either --unrestricted or --as-settings",
        ),
    ] {
        let output = run(&args);
        assert!(!output.status.success(), "{args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(error), "{args:?}: {stderr}");
    }
}

#[test]
fn the_look_ups_the_connector_makes_before_creating_say_which_id_they_found() {
    let home = Home::new();
    let (project, worktree, _) = home.project("app");
    // `project show` and `worktree show` print the entity, whose `id` the
    // connector compares with the id it was given: the CLI also resolves names,
    // branches, paths and id prefixes, which must not stand in for an id.
    assert_eq!(
        home.ok(&["project", "show", &project, "--json"])["id"],
        project.as_str()
    );
    assert_eq!(
        home.ok(&["worktree", "show", &worktree, "--json"])["id"],
        worktree.as_str()
    );
    assert_eq!(
        home.ok(&["project", "show", "app", "--json"])["id"],
        project.as_str()
    );
    assert_eq!(
        home.ok(&["worktree", "show", "main", "--json"])["id"],
        worktree.as_str()
    );
    let unknown = Uuid::new_v4().to_string();
    for (args, sentence) in [
        (
            ["project", "show", &unknown, "--json"],
            format!("riwork: No project matches '{unknown}'"),
        ),
        (
            ["worktree", "show", &unknown, "--json"],
            format!("riwork: No worktree matches '{unknown}'"),
        ),
    ] {
        let output = home.run(&args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert_eq!(String::from_utf8_lossy(&output.stderr).trim(), sentence);
    }
    assert!(!home.runtime().exists(), "a GUI instance was registered");
}
