//! Process-level checks of the CLI's argument handling and the MCP stdio loop.
//! Each child runs in a throwaway RIWORK_HOME. A regression that lets an
//! unknown argument start the GUI is caught by a timeout and killed.

use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
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
            .join(format!("riwork-cli-contract-{}", Uuid::new_v4()));
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

    fn mcp(&self, input: &[u8]) -> (Output, Vec<Value>) {
        let mut child = self
            .command(&["mcp"])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        let output = finish(child, &["mcp"]);
        let responses = output
            .stdout
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        (output, responses)
    }

    fn tool(&self, name: &str, arguments: Value) -> Value {
        let request = json!({
            "jsonrpc":"2.0","id":1,"method":"tools/call",
            "params":{"name":name,"arguments":arguments}
        });
        let (output, responses) = self.mcp(format!("{request}\n").as_bytes());
        assert!(output.status.success());
        responses[0]["result"].clone()
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

fn repository(home: &Home, name: &str) -> PathBuf {
    let root = home.0.join(name);
    fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "--initial-branch=main", "--template="]);
    git(&root, &["commit", "--allow-empty", "-m", "fixture"]);
    root
}

fn ping() -> String {
    json!({"jsonrpc":"2.0","id":1,"method":"ping"}).to_string()
}

#[test]
fn version_prints_the_crate_version_and_never_starts_the_gui() {
    let home = Home::new();
    let expected = format!("riwork {}\n", env!("CARGO_PKG_VERSION"));
    for args in [&["--version"][..], &["version"], &["-V"]] {
        let output = home.run(args);
        assert!(output.status.success(), "{args:?}");
        assert_eq!(String::from_utf8_lossy(&output.stdout), expected);
    }
    let json = home.ok(&["version", "--json"]);
    assert_eq!(json["version"], env!("CARGO_PKG_VERSION"));
    let help = String::from_utf8(home.run(&["help"]).stdout).unwrap();
    assert!(help.contains("--version"));
    assert!(help.contains("--base REF"));
    assert!(!home.runtime().exists(), "a GUI instance was registered");
}

#[test]
fn unknown_arguments_fail_with_usage_instead_of_opening_the_workspace() {
    let home = Home::new();
    let directory = home.0.to_string_lossy().into_owned();
    let cases: Vec<Vec<&str>> = vec![
        vec!["--bogus"],
        vec!["shells", "list"],
        vec!["--project", "A", "task", "list"],
        vec!["--json"],
        vec!["/definitely/not/a/riwork/directory"],
        vec![&directory, "extra"],
    ];
    for args in cases {
        let output = home.run(&args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("Usage: riwork"), "{args:?}: {stderr}");
    }
    assert!(!home.runtime().exists(), "a GUI instance was registered");
}

#[test]
fn a_closed_stdout_pipe_ends_output_quietly() {
    let home = Home::new();
    for args in [&["help"][..], &["version"]] {
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let child = home.command(args).stdout(writer).spawn().unwrap();
        let output = finish(child, args);
        assert_eq!(output.status.code(), Some(0), "{args:?}");
        assert!(
            output.stderr.is_empty(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn mcp_survives_invalid_utf8_and_answers_batches_and_scalars() {
    let home = Home::new();
    let mut input = b"\xff\xfe not utf-8\n".to_vec();
    input.extend_from_slice(format!("{}\n", ping()).as_bytes());
    input.extend_from_slice(
        format!(
            "[{},{{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}},{}]\n",
            ping(),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})
        )
        .as_bytes(),
    );
    input.extend_from_slice(b"[]\n42\n");
    input.extend_from_slice(
        format!(
            "[{}]\n",
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .as_bytes(),
    );
    input.extend_from_slice(format!("{}\n", ping()).as_bytes());
    let (output, responses) = home.mcp(&input);
    assert!(output.status.success());
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(responses.len(), 6, "{responses:?}");
    assert_eq!(responses[0]["error"]["code"], -32700);
    assert_eq!(responses[1]["result"], json!({}));
    let batch = responses[2].as_array().unwrap();
    assert_eq!(batch.len(), 2);
    assert_eq!(batch[0]["id"], 1);
    assert!(batch[1]["result"]["tools"].is_array());
    assert_eq!(responses[3]["error"]["code"], -32600);
    assert_eq!(responses[4]["error"]["code"], -32600);
    // The lone-notification batch is silent; the server is still serving.
    assert_eq!(responses[5]["result"], json!({}));
}

#[test]
fn mcp_rejects_an_undeclared_argument_before_touching_any_state() {
    let home = Home::new();
    let result = home.tool(
        "riwork_shell_create",
        json!({"worktree":"main","command":"/bin/cat"}),
    );
    assert_eq!(result["isError"], true);
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("'worktree' is not accepted by riwork_shell_create")
    );
    assert!(
        !home.0.join("sessions.json").exists(),
        "a shell was created"
    );
}

#[test]
fn a_named_project_scopes_worktree_selectors_in_the_cli_and_mcp() {
    let home = Home::new();
    let first = repository(&home, "first");
    let second = repository(&home, "second");
    let first_id = home.ok(&["project", "add", first.to_str().unwrap(), "--json"])["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let second_id = home.ok(&["project", "add", second.to_str().unwrap(), "--json"])["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let first_root = home.ok(&["worktree", "list", "--project", &first_id, "--json"])[0]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Both projects have a `main` worktree, so the bare selector is ambiguous.
    let ambiguous = home.run(&[
        "shell",
        "create",
        "--worktree",
        "main",
        "--command",
        "/bin/cat",
    ]);
    assert!(!ambiguous.status.success());
    assert!(String::from_utf8_lossy(&ambiguous.stderr).contains("More than one worktree"));

    let created = home.ok(&[
        "shell",
        "create",
        "--project",
        &first_id,
        "--worktree",
        "main",
        "--command",
        "/bin/cat",
        "--json",
    ]);
    assert_eq!(created["project_id"], first_id);
    assert_eq!(created["worktree_id"], first_root);

    let other = home.run(&[
        "shell",
        "create",
        "--project",
        &second_id,
        "--worktree",
        &first_root,
        "--command",
        "/bin/cat",
    ]);
    assert!(!other.status.success());
    assert!(String::from_utf8_lossy(&other.stderr).contains("belongs to another project"));

    let session = home.tool(
        "riwork_shell_create",
        json!({"project_id":second_id,"worktree_id":"main","command":"/bin/cat"}),
    );
    assert_eq!(session["isError"], false, "{session}");
    assert_eq!(
        session["structuredContent"]["session"]["project_id"],
        second_id
    );
}

#[test]
fn renaming_a_project_leaves_its_folder_alone() {
    let home = Home::new();
    let root = home.0.join("plain");
    fs::create_dir_all(&root).unwrap();
    let folder = home.ok(&["project", "folder", "create", "Group", "--json"])["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let project = home.ok(&["project", "add", root.to_str().unwrap(), "--json"])["id"]
        .as_str()
        .unwrap()
        .to_owned();
    home.ok(&["project", "update", &project, "--folder", &folder, "--json"]);
    let renamed = home.ok(&["project", "update", &project, "--name", "Renamed", "--json"]);
    assert_eq!(renamed["name"], "Renamed");
    assert_eq!(renamed["folder_id"], folder);
    let ungrouped = home.ok(&[
        "project",
        "update",
        &project,
        "--name",
        "Loose",
        "--ungrouped",
        "--json",
    ]);
    assert_eq!(ungrouped["name"], "Loose");
    assert!(ungrouped["folder_id"].is_null());
    let nothing = home.run(&["project", "update", &project]);
    assert!(!nothing.status.success());
    assert!(String::from_utf8_lossy(&nothing.stderr).contains("Nothing to update"));
}

#[test]
fn grok_shell_usage_is_unknown_not_an_error() {
    let home = Home::new();
    let shell = Uuid::new_v4().to_string();
    fs::write(
        home.0.join("sessions.json"),
        json!({"sessions":[{
            "id":shell,"project_id":null,"worktree_id":null,"kind":"project","cwd":"/",
            "command":"grok","harness":"grok","created_at_unix":1
        }]})
        .to_string(),
    )
    .unwrap();
    let usage = home.ok(&["usage", "--shell", &shell, "--json"]);
    assert_eq!(usage["provider"], "grok");
    assert_eq!(usage["windows"], json!([]));
    assert_eq!(usage["account_label"], "unknown");
    // Not running: no session figures, and the reason instead of a failure.
    assert!(usage.get("session").is_none());
    assert_eq!(usage["session_error"], "This Grok session is not running");
    let text = home.run(&["usage", "--shell", &shell]);
    assert!(text.status.success());
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(text.starts_with("grok"));
    assert!(text.contains("open /usage in Grok"), "{text}");
    assert!(text.contains("Session usage unavailable"), "{text}");
}

/// Marks a registered shell as a Grok tab without launching Grok.
fn mark_as_grok(home: &Home, shell: &str) {
    let path = home.0.join("sessions.json");
    let mut registry: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    for session in registry["sessions"].as_array_mut().unwrap() {
        if session["id"] == shell {
            session["harness"] = json!("grok");
        }
    }
    fs::write(path, registry.to_string()).unwrap();
}

#[cfg(unix)]
#[test]
fn grok_shell_usage_reports_the_session_grok_lists_for_the_panes_process() {
    let home = Home::new();
    let project = home.0.join("project");
    fs::create_dir_all(&project).unwrap();
    let project_id = home.ok(&["project", "add", project.to_str().unwrap(), "--json"])["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // The pane's process is a copy of this build named `grok`, so the kernel
    // reports a Grok executable for it. `mcp` waits on the terminal for input.
    let running = home.0.join("running");
    fs::create_dir_all(&running).unwrap();
    let grok_process = running.join("grok");
    fs::copy(env!("CARGO_BIN_EXE_riwork"), &grok_process).unwrap();
    let pid_file = home.0.join("pane.pid");
    let live = home.ok(&[
        "shell",
        "create",
        "--project",
        &project_id,
        "--command",
        &format!(
            "echo $$ > '{}'; exec '{}' mcp",
            pid_file.display(),
            grok_process.display()
        ),
        "--json",
    ])["id"]
        .as_str()
        .unwrap()
        .to_owned();
    // A Grok tab whose pane is not Grok, which Grok's list still names: what
    // remains after a session dies and its pid is reused.
    let reused_pid_file = home.0.join("reused.pid");
    let stale = home.ok(&[
        "shell",
        "create",
        "--project",
        &project_id,
        "--command",
        &format!("echo $$ > '{}'; exec /bin/cat", reused_pid_file.display()),
        "--json",
    ])["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let read_pid = |file: &Path| -> u32 {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Ok(text) = fs::read_to_string(file)
                && let Ok(pid) = text.trim().parse()
            {
                return pid;
            }
            assert!(
                Instant::now() < deadline,
                "{} never appeared",
                file.display()
            );
            thread::sleep(Duration::from_millis(20));
        }
    };
    let (live_pid, stale_pid) = (read_pid(&pid_file), read_pid(&reused_pid_file));
    // The pid file is written before the shell replaces itself with the copy.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let name = Command::new("ps")
            .args(["-o", "comm=", "-p", &live_pid.to_string()])
            .output()
            .unwrap();
        if String::from_utf8_lossy(&name.stdout)
            .trim()
            .ends_with("/grok")
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the pane never became the grok copy"
        );
        thread::sleep(Duration::from_millis(20));
    }
    mark_as_grok(&home, &live);
    mark_as_grok(&home, &stale);

    let session = "01a0ebde-d244-7bc3-8222-9d1a4330cd15";
    let other = "01a0ebd0-7000-7000-8000-000000000001";
    let grok_home = home.0.join("grok-home");
    fs::create_dir_all(&grok_home).unwrap();
    let opened_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
    fs::write(
        grok_home.join("active_sessions.json"),
        json!([
            {"session_id": session, "pid": live_pid, "cwd": "/w", "opened_at": opened_at},
            {"session_id": other, "pid": stale_pid, "cwd": "/w", "opened_at": opened_at},
        ])
        .to_string(),
    )
    .unwrap();
    // The official CLI, which the fake answers for the listed session only.
    let bin = home.0.join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(
        bin.join("report.json"),
        json!({
            "sessionId": session, "updatedAt": "2026-09-29T13:02:35.793940+00:00",
            "session": {
                "inputTokens": 100_000, "outputTokens": 28_000, "cachedReadTokens": 60_000,
                "cacheCreationTokens": 5, "reasoningTokens": 9_000, "totalTokens": 128_000,
                "modelCalls": 12, "costUsdTicks": 4_200_000_000u64, "turnCount": 3,
                "primaryModelId": "grok-4.7-build-fast",
                "modelUsage": {"grok-4.7-build-fast": {
                    "inputTokens": 100_000, "outputTokens": 28_000, "totalTokens": 128_000,
                    "modelCalls": 12, "costUsdTicks": 4_200_000_000u64
                }}
            },
            "turns": []
        })
        .to_string(),
    )
    .unwrap();
    let cli = bin.join("grok");
    fs::write(
        &cli,
        format!(
            "#!/bin/sh\nif [ \"$1\" = usage ] && [ \"$2\" = {session} ]; then cat \"${{0%/*}}/report.json\"; else echo \"Error: Session '$2' not found.\" >&2; exit 1; fi\n"
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&cli, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let usage_of = |shell: &str, json: bool| {
        let mut args = vec!["usage", "--shell", shell];
        if json {
            args.push("--json");
        }
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut command = home.command(&args);
        command.env("GROK_HOME", &grok_home).env("PATH", path);
        finish(command.spawn().unwrap(), &args)
    };

    let output = usage_of(&live, true);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let usage: Value = serde_json::from_slice(&output.stdout).unwrap();
    // The allowance is unknown and stays out of `windows`.
    assert_eq!(usage["provider"], "grok");
    assert_eq!(usage["windows"], json!([]));
    assert_eq!(usage["account_label"], "unknown");
    assert!(usage.get("session_error").is_none(), "{usage}");
    assert_eq!(usage["session_cost_usd"], 0.42);
    let report = &usage["session"];
    assert_eq!(report["session_id"], session);
    assert_eq!(report["primary_model"], "grok-4.7-build-fast");
    assert_eq!(report["turns"], 3);
    assert_eq!(report["model_calls"], 12);
    assert_eq!(report["updated_at"], "2026-09-29T13:02:35Z");
    assert_eq!(report["cost_usd"], 0.42);
    assert_eq!(report["tokens"]["total"], 128_000);
    assert_eq!(report["tokens"]["input"], 100_000);
    assert_eq!(report["tokens"]["output"], 28_000);
    assert_eq!(report["tokens"]["cached_read"], 60_000);
    assert_eq!(report["tokens"]["cache_creation"], 5);
    assert_eq!(report["tokens"]["reasoning"], 9_000);

    let text = usage_of(&live, false);
    assert!(text.status.success());
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(text.starts_with("grok · unknown"), "{text}");
    assert!(text.contains("open /usage in Grok"), "{text}");
    assert!(
        text.contains("grok-4.7-build-fast · 3 turns · 12 model calls"),
        "{text}"
    );
    assert!(text.contains("Tokens: 128k total"), "{text}");
    assert!(text.contains("Session cost: $0.42"), "{text}");

    // The stale entry is refused: the report says why instead of showing a
    // session that is not this tab's.
    let output = usage_of(&stale, true);
    assert!(output.status.success());
    let usage: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(usage["provider"], "grok");
    assert_eq!(usage["windows"], json!([]));
    assert!(usage.get("session").is_none(), "{usage}");
    assert!(usage.get("session_cost_usd").is_none_or(Value::is_null));
    assert!(
        usage["session_error"]
            .as_str()
            .unwrap()
            .contains("not registered"),
        "{usage}"
    );
}

#[test]
fn shell_keys_rejects_bad_invocations_before_reaching_any_shell() {
    let home = Home::new();
    let shell = Uuid::new_v4().to_string();
    let too_long = format!("t:{}", "a".repeat(4097));
    let cases: Vec<Vec<&str>> = vec![
        vec!["shell", "keys"],
        vec!["shell", "keys", &shell],
        vec!["shell", "keys", &shell, "--json"],
        vec!["shell", "keys", &shell, "--"],
        vec!["shell", "keys", &shell, "k:Enter"],
        vec!["shell", "keys", "--", "k:Enter"],
        vec!["shell", "keys", &shell, "extra", "--", "k:Enter"],
        vec!["shell", "keys", &shell, "--", "Enter"],
        vec!["shell", "keys", &shell, "--", "k:Return"],
        vec!["shell", "keys", &shell, "--", "k:enter"],
        vec!["shell", "keys", &shell, "--", "k:C-A"],
        vec!["shell", "keys", &shell, "--", "k:C-1"],
        vec!["shell", "keys", &shell, "--", "t:"],
        vec!["shell", "keys", &shell, "--", "t:a\tb"],
        vec!["shell", "keys", &shell, "--", "t:a\nb"],
        vec!["shell", "keys", &shell, "--", "--json"],
        vec!["shell", "keys", &shell, "--", "k:Enter", "--", "k:Enter"],
        vec!["shell", "keys", &shell, "--", &too_long],
        vec!["shell", "keys", "12345678", "--", "k:Enter"],
    ];
    for args in cases {
        let output = home.run(&args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("Usage: riwork shell keys") || stderr.contains("invalid_request: "),
            "{args:?}: {stderr}"
        );
    }
    // Sixty-five items, and 4097 text bytes across two items.
    let mut many = vec!["shell", "keys", &shell, "--"];
    many.extend(std::iter::repeat_n("k:Up", 65));
    let output = home.run(&many);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid_request: "));
    let (half, rest) = (
        format!("t:{}", "a".repeat(2048)),
        format!("t:{}", "a".repeat(2049)),
    );
    let output = home.run(&["shell", "keys", &shell, "--", &half, &rest]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid_request: "));
    // A well-formed batch for a shell that does not exist is a lookup failure.
    let output = home.run(&["shell", "keys", &shell, "--", "t:x", "k:Enter"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("not_found: "));
    assert!(!home.runtime().exists(), "a GUI instance was registered");
}

#[test]
fn shell_keys_types_into_a_pane_and_output_json_reports_the_screen() {
    let home = Home::new();
    let repo = repository(&home, "typing");
    home.ok(&["project", "add", repo.to_str().unwrap(), "--json"]);
    let created = home.ok(&["shell", "create", "--command", "/bin/cat", "--json"]);
    let id = created["id"].as_str().unwrap().to_owned();
    let output_of = |lines: &str| home.ok(&["shell", "output", &id, "--lines", lines, "--json"]);

    let wait_for = |wanted: &str| {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let screen = output_of("50");
            if screen["output"].as_str().unwrap().contains(wanted) {
                break screen;
            }
            assert!(
                Instant::now() < deadline,
                "typed text never arrived: {screen}"
            );
            thread::sleep(Duration::from_millis(25));
        }
    };
    // Text ending in `;`, then Enter, in one batch. `--json` is ours before
    // the `--`; after it, it is literal text like any other.
    let sent = home.ok(&["shell", "keys", &id, "--json", "--", "t:hello;", "k:Enter"]);
    assert_eq!(sent, json!({"id": id, "sent": 2}));
    wait_for("hello;\nhello;\n");
    let sent = home.ok(&["shell", "keys", &id, "--json", "--", "t:--json", "t: \\;"]);
    assert_eq!(sent, json!({"id": id, "sent": 2}));
    let screen = wait_for("hello;\nhello;\n--json \\;");
    // The last `rows` lines of the output are the visible screen, and the
    // cursor is a cell of it: after the second line's text, on the third row.
    let rows = screen["rows"].as_u64().unwrap() as usize;
    let cols = screen["cols"].as_u64().unwrap();
    assert!(rows >= 2 && cols >= 20, "{screen}");
    assert_eq!(screen["in_mode"], false);
    let lines: Vec<&str> = screen["output"].as_str().unwrap().lines().collect();
    let visible = &lines[lines.len() - rows..];
    assert_eq!(&visible[..3], ["hello;", "hello;", "--json \\;"]);
    let cursor = &screen["cursor"];
    assert_eq!(
        (cursor["x"].as_u64(), cursor["y"].as_u64()),
        (Some(9), Some(2)),
        "{screen}"
    );
    assert_eq!(visible[cursor["y"].as_u64().unwrap() as usize].len(), 9);
    // The plain form is unchanged: the same text, no JSON.
    let text = home.run(&["shell", "output", &id, "--lines", "50"]);
    assert_eq!(
        String::from_utf8_lossy(&text.stdout),
        screen["output"].as_str().unwrap()
    );
    // Text mode of keys prints nothing.
    let quiet = home.run(&["shell", "keys", &id, "--", "k:Enter"]);
    assert!(quiet.status.success());
    assert!(quiet.stdout.is_empty());
}

#[test]
fn shell_history_pages_the_scrollback_and_output_json_tells_how_long_it_is() {
    let home = Home::new();
    let repo = repository(&home, "scrollback");
    home.ok(&["project", "add", repo.to_str().unwrap(), "--json"]);
    let created = home.ok(&[
        "shell",
        "create",
        "--command",
        "seq 1 500; printf x; exec sleep 300",
        "--json",
    ]);
    let id = created["id"].as_str().unwrap().to_owned();
    let output_of = || home.ok(&["shell", "output", &id, "--lines", "5", "--json"]);
    let deadline = Instant::now() + Duration::from_secs(15);
    let screen = loop {
        let screen = output_of();
        if screen["output"].as_str().unwrap().ends_with("500\nx\n") {
            break screen;
        }
        assert!(Instant::now() < deadline, "{screen}");
        thread::sleep(Duration::from_millis(25));
    };
    // The 500 lines and the line with the "x", less the rows of the screen.
    let rows = screen["rows"].as_u64().unwrap();
    let history = 501 - rows;
    assert_eq!(screen["history_size"], history, "{screen}");
    assert_eq!(screen["alternate"], false);
    let expected_hash = screen["hash"].as_str().unwrap().to_owned();
    // The unchanged form carries both fields next to the hash.
    let unchanged = home.ok(&[
        "shell",
        "output",
        &id,
        "--lines",
        "5",
        "--json",
        &format!("--if-changed={expected_hash}"),
    ]);
    assert_eq!(
        unchanged,
        json!({"id": id, "unchanged": true, "hash": expected_hash,
               "history_size": history, "alternate": false})
    );

    let lines = |first: u64, last: u64| {
        (first..=last)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let page = home.ok(&[
        "shell", "history", &id, "--end", "0", "--lines", "3", "--json",
    ]);
    assert_eq!(
        page,
        json!({"id": id, "output": lines(history - 2, history), "line_count": 3,
               "history_size": history, "complete": false})
    );
    // Earlier pages, up to the very top and beyond it.
    let higher = home.ok(&[
        "shell", "history", &id, "--end", "10", "--lines", "4", "--styled", "--json",
    ]);
    assert_eq!(higher["output"], lines(history - 13, history - 10));
    let top = home.ok(&[
        "shell",
        "history",
        &id,
        "--end",
        &(history - 2).to_string(),
        "--lines",
        "1000",
        "--json",
    ]);
    assert_eq!(
        (
            top["output"].as_str(),
            top["line_count"].as_u64(),
            top["complete"].as_bool()
        ),
        (Some("1\n2"), Some(2), Some(true))
    );
    let beyond = home.ok(&[
        "shell",
        "history",
        &id,
        "--end",
        &history.to_string(),
        "--lines",
        "5",
        "--json",
    ]);
    assert_eq!(
        beyond,
        json!({"id": id, "output": "", "line_count": 0, "history_size": history, "complete": true})
    );
    // Without --json the page is the text, one line per line.
    let text = home.run(&["shell", "history", &id, "--end", "0", "--lines", "3"]);
    assert!(text.status.success());
    assert_eq!(
        String::from_utf8_lossy(&text.stdout),
        format!("{}\n", lines(history - 2, history))
    );
    let nothing = home.run(&[
        "shell",
        "history",
        &id,
        "--end",
        &history.to_string(),
        "--lines",
        "3",
    ]);
    assert!(nothing.status.success() && nothing.stdout.is_empty());

    // Usage errors exit 2 and print nothing on stdout; a missing shell is an error.
    for args in [
        vec!["shell", "history"],
        vec!["shell", "history", &id],
        vec!["shell", "history", &id, "--end", "0"],
        vec!["shell", "history", &id, "--lines", "5"],
        vec!["shell", "history", &id, "--end", "0", "--lines", "0"],
        vec!["shell", "history", &id, "--end", "0", "--lines", "1001"],
        vec!["shell", "history", &id, "--end", "-1", "--lines", "5"],
        vec!["shell", "history", &id, "--end", "x", "--lines", "5"],
        vec![
            "shell", "history", &id, "--end", "0", "--lines", "5", "extra",
        ],
    ] {
        let output = home.run(&args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
    }
    let missing = Uuid::new_v4().to_string();
    let output = home.run(&["shell", "history", &missing, "--end", "0", "--lines", "5"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown shell"));
    assert!(!home.runtime().exists(), "a GUI instance was registered");
}
