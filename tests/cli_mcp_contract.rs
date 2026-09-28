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
    let text = home.run(&["usage", "--shell", &shell]);
    assert!(text.status.success());
    assert!(String::from_utf8_lossy(&text.stdout).starts_with("grok"));
}
