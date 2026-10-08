//! Real CLI and MCP stdio acceptance in a fresh child RIWORK_HOME.
//! Fixture shells are inert `/bin/cat` processes; no desktop scheduler runs.

use chrono::{Duration, FixedOffset, SecondsFormat, Utc};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
};
use uuid::Uuid;

struct Fixture {
    home: PathBuf,
    project_id: String,
    worktree_id: String,
    shells: [String; 3],
}

impl Fixture {
    fn new() -> Self {
        let home =
            std::env::temp_dir().join(format!("riwork-schedule-interface-{}", Uuid::new_v4()));
        let root = home.join("project");
        fs::create_dir_all(&root).unwrap();
        let mut fixture = Self {
            home,
            project_id: String::new(),
            worktree_id: String::new(),
            shells: std::array::from_fn(|_| String::new()),
        };
        let project = fixture.cli_ok(&["project", "add", root.to_str().unwrap(), "--json"]);
        fixture.project_id = project["id"].as_str().unwrap().to_owned();
        let worktrees = fixture.cli_ok(&[
            "worktree",
            "list",
            "--project",
            &fixture.project_id,
            "--json",
        ]);
        fixture.worktree_id = worktrees.as_array().unwrap()[0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let app = fixture.cli_ok(&[
            "orchestrator",
            "create",
            "--cwd",
            root.to_str().unwrap(),
            "--command",
            "/bin/cat",
            "--json",
        ]);
        let project = fixture.cli_ok(&[
            "orchestrator",
            "create",
            "--project",
            &fixture.project_id,
            "--command",
            "/bin/cat",
            "--json",
        ]);
        let worker = fixture.cli_ok(&[
            "shell",
            "create",
            "--worktree",
            &fixture.worktree_id,
            "--command",
            "/bin/cat",
            "--json",
        ]);
        fixture.shells =
            [app, project, worker].map(|value| value["id"].as_str().unwrap().to_owned());

        // The disposable sessions have exact, fixture-only provider identities.
        // This avoids launching or inspecting any authenticated agent process.
        let registry = fixture.home.join("sessions.json");
        let mut sessions: Value = serde_json::from_slice(&fs::read(&registry).unwrap()).unwrap();
        for session in sessions["sessions"].as_array_mut().unwrap() {
            session["harness"] = json!("claude");
        }
        fs::write(registry, serde_json::to_vec(&sessions).unwrap()).unwrap();
        let hooks = fixture.home.join("agent-hooks/claude");
        fs::create_dir_all(&hooks).unwrap();
        for shell in &fixture.shells {
            fs::write(hooks.join(format!("{shell}.json")), json!({
                "session_id":format!("fixture-{shell}"),"turn_id":"completed-fixture-turn","completed":true
            }).to_string()).unwrap();
        }
        fixture
    }

    fn command(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_riwork"))
            .args(args)
            .env("RIWORK_HOME", &self.home)
            .env("RIWORK_RUNTIME_DIR", self.home.join("runtime"))
            .output()
            .unwrap()
    }

    fn cli_ok(&self, args: &[&str]) -> Value {
        let output = self.command(args);
        assert!(
            output.status.success(),
            "CLI {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn cli_error(&self, args: &[&str], code: &str) -> Value {
        let output = self.command(args);
        assert!(
            !output.status.success(),
            "CLI unexpectedly succeeded: {args:?}"
        );
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["error"]["code"], code, "{value}");
        value
    }

    fn mcp(&self, calls: &[Value]) -> Vec<Value> {
        let mut process = Command::new(env!("CARGO_BIN_EXE_riwork"))
            .arg("mcp")
            .env("RIWORK_HOME", &self.home)
            .env("RIWORK_RUNTIME_DIR", self.home.join("runtime"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        {
            let mut input = process.stdin.take().unwrap();
            for call in calls {
                writeln!(input, "{call}").unwrap();
            }
        }
        let output = process.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "MCP: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stderr.is_empty(),
            "MCP wrote stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
            .stdout
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice::<Value>(line).unwrap())
            .collect()
    }

    fn tool(&self, name: &str, arguments: Value) -> Value {
        let responses = self.mcp(&[json!({
            "jsonrpc":"2.0","id":1,"method":"tools/call",
            "params":{"name":name,"arguments":arguments}
        })]);
        assert_eq!(responses.len(), 1);
        responses.into_iter().next().unwrap()["result"].clone()
    }

    fn tool_ok(&self, name: &str, arguments: Value) -> Value {
        let result = self.tool(name, arguments);
        assert_eq!(result["isError"], false, "{result}");
        result["structuredContent"].clone()
    }

    fn tool_error(&self, name: &str, arguments: Value, code: &str) -> Value {
        let result = self.tool(name, arguments);
        assert_eq!(result["isError"], true, "{result}");
        // Clients validate structuredContent against the declared outputSchema,
        // so a failure carries its `{"error":...}` object as text only.
        assert!(result.get("structuredContent").is_none(), "{result}");
        let text = result["content"][0]["text"].as_str().unwrap();
        let error: Value = serde_json::from_str(text).unwrap();
        assert_eq!(error["error"]["code"], code, "{result}");
        error["error"].clone()
    }

    fn scope_args(&self, index: usize) -> Vec<&str> {
        match index {
            0 => vec!["--scope", "app", "--shell", &self.shells[0]],
            1 => vec![
                "--scope",
                "project",
                "--project",
                &self.project_id,
                "--shell",
                &self.shells[1],
            ],
            2 => vec![
                "--scope",
                "workspace",
                "--project",
                &self.project_id,
                "--worktree",
                &self.worktree_id,
                "--shell",
                &self.shells[2],
            ],
            _ => unreachable!(),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for shell in &self.shells {
            if !shell.is_empty() {
                let _ = self.command(&["shell", "close", shell]);
            }
        }
        let _ = fs::remove_dir_all(&self.home);
    }
}

fn future_at(days: i64) -> String {
    (Utc::now() + Duration::days(days))
        .with_timezone(&FixedOffset::east_opt(2 * 3600).unwrap())
        .to_rfc3339_opts(SecondsFormat::Secs, false)
}

#[test]
#[ignore = "slow: three real tmux fixture shells per test"]
fn cli_and_mcp_share_three_scope_lifecycle_and_stdio_protocol() {
    let fixture = Fixture::new();
    let protocol = fixture.mcp(&[
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
    ]);
    assert_eq!(protocol.len(), 2, "Notifications must not write to stdout");
    assert_eq!(protocol[0]["result"]["protocolVersion"], "2025-11-25");
    let tools = protocol[1]["result"]["tools"].as_array().unwrap();
    for name in [
        "list", "show", "create", "update", "pause", "resume", "delete",
    ] {
        let tool = tools
            .iter()
            .find(|tool| tool["name"] == format!("riwork_schedule_{name}"))
            .unwrap();
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        assert!(tool["outputSchema"].is_object());
        if name == "create" {
            assert_eq!(
                tool["inputSchema"]["properties"]["at"]["format"],
                "date-time"
            );
            assert_eq!(
                tool["inputSchema"]["properties"]["every_minutes"]["minimum"],
                5
            );
            assert_eq!(tool["outputSchema"]["properties"]["schedule"]["properties"]["target"]["properties"]["scope"]["oneOf"].as_array().unwrap().len(), 3);
        }
    }

    let at = future_at(7);
    let mut app_create = vec!["schedule", "create"];
    app_create.extend(fixture.scope_args(0));
    app_create.extend([
        "--title",
        "App check",
        "--prompt",
        "Fixture app prompt",
        "--at",
        &at,
        "--json",
    ]);
    let app = fixture.cli_ok(&app_create)["schedule"].clone();
    let project = fixture.tool_ok(
        "riwork_schedule_create",
        json!({
            "scope":"project","project_id":fixture.project_id,"shell_id":fixture.shells[1],
            "title":"Project check","prompt":"Fixture project prompt","at":at
        }),
    )["schedule"]
        .clone();
    let mut workspace_create = vec!["schedule", "create"];
    workspace_create.extend(fixture.scope_args(2));
    workspace_create.extend([
        "--title",
        "Workspace check",
        "--prompt",
        "Fixture workspace prompt",
        "--at",
        &at,
        "--every-minutes",
        "60",
        "--json",
    ]);
    let workspace = fixture.cli_ok(&workspace_create)["schedule"].clone();
    assert_eq!(workspace["timing"]["seconds"], 3600);
    for (index, item) in [&app, &project, &workspace].into_iter().enumerate() {
        assert_eq!(item["target"]["shell_id"], fixture.shells[index]);
        assert_eq!(item["revision"], 1);
        assert!(!item["target"]["pane_identity"].as_str().unwrap().is_empty());
        assert_eq!(
            item["target"]["provider_session"],
            format!("fixture-{}", fixture.shells[index])
        );
    }
    assert_eq!(
        fixture.cli_ok(&["schedule", "list", "--json"])["items"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        fixture.tool_ok("riwork_schedule_list", json!({}))["items"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(fixture.tool_ok("riwork_schedule_list", json!({"scope":"workspace","project_id":fixture.project_id,"worktree_id":fixture.worktree_id}))["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        fixture.cli_ok(&["schedule", "show", app["id"].as_str().unwrap(), "--json"])["schedule"]["id"],
        app["id"]
    );

    let app_id = app["id"].as_str().unwrap();
    let app_paused = fixture.tool_ok(
        "riwork_schedule_pause",
        json!({"schedule_id":app_id,"revision":1,"scope":"app","shell_id":fixture.shells[0]}),
    )["schedule"]
        .clone();
    assert_eq!(app_paused["revision"], 2);
    assert_eq!(app_paused["paused"], true);
    let stale = fixture.tool_error(
        "riwork_schedule_resume",
        json!({"schedule_id":app_id,"revision":1,"scope":"app","shell_id":fixture.shells[0]}),
        "revision_conflict",
    );
    assert_eq!(stale["current"]["revision"], 2);
    let mut app_resume = vec!["schedule", "resume", app_id, "--revision", "2"];
    app_resume.extend(fixture.scope_args(0));
    app_resume.push("--json");
    let resumed = fixture.cli_ok(&app_resume)["schedule"].clone();
    assert_eq!(resumed["revision"], 3);
    assert_eq!(resumed["paused"], false);

    let project_id = project["id"].as_str().unwrap();
    let edited = fixture.tool_ok("riwork_schedule_update", json!({
        "schedule_id":project_id,"revision":1,"scope":"project","project_id":fixture.project_id,
        "shell_id":fixture.shells[1],"title":"Edited project","at":future_at(8),"every_minutes":5
    }))["schedule"].clone();
    assert_eq!(edited["revision"], 2);
    assert_eq!(edited["target"], project["target"]);
    assert_eq!(edited["timing"]["seconds"], 300);
    let mut project_pause = vec!["schedule", "pause", project_id, "--revision", "2"];
    project_pause.extend(fixture.scope_args(1));
    project_pause.push("--json");
    let paused = fixture.cli_ok(&project_pause)["schedule"].clone();
    assert_eq!(paused["paused"], true);
    let continued = fixture.tool_ok("riwork_schedule_resume", json!({
        "schedule_id":project_id,"revision":3,"scope":"project","project_id":fixture.project_id,"shell_id":fixture.shells[1]
    }))["schedule"].clone();
    assert_eq!(continued["revision"], 4);

    let workspace_id = workspace["id"].as_str().unwrap();
    let mut workspace_edit = vec!["schedule", "update", workspace_id, "--revision", "1"];
    workspace_edit.extend(fixture.scope_args(2));
    let later = future_at(9);
    workspace_edit.extend(["--at", &later, "--once", "--json"]);
    let workspace_edited = fixture.cli_ok(&workspace_edit)["schedule"].clone();
    assert_eq!(workspace_edited["timing"]["kind"], "once");
    assert_eq!(workspace_edited["target"], workspace["target"]);
    let mut workspace_delete = vec!["schedule", "delete", workspace_id, "--revision", "2"];
    workspace_delete.extend(fixture.scope_args(2));
    workspace_delete.push("--json");
    assert_eq!(fixture.cli_ok(&workspace_delete)["deleted"], true);
    assert_eq!(fixture.tool_ok("riwork_schedule_delete", json!({
        "schedule_id":project_id,"revision":4,"scope":"project","project_id":fixture.project_id,"shell_id":fixture.shells[1]
    }))["deleted"], true);
    assert_eq!(
        fixture.tool_ok(
            "riwork_schedule_delete",
            json!({
                "schedule_id":app_id,"revision":3,"scope":"app","shell_id":fixture.shells[0]
            })
        )["deleted"],
        true
    );
    assert!(
        fixture.cli_ok(&["schedule", "list", "--json"])["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let ledger: Value =
        serde_json::from_slice(&fs::read(fixture.home.join("schedules.json")).unwrap()).unwrap();
    assert!(ledger["schedules"].as_array().unwrap().is_empty());
}

#[test]
#[ignore = "slow: three real tmux fixture shells per test"]
fn invalid_ids_scope_time_revision_and_target_are_structured_errors() {
    let fixture = Fixture::new();
    let at = future_at(7);
    let app = fixture.tool_ok("riwork_schedule_create", json!({
        "scope":"app","shell_id":fixture.shells[0],"title":"Safe fixture","prompt":"Fixture prompt","at":at
    }))["schedule"].clone();
    let id = app["id"].as_str().unwrap();
    fixture.tool_error("riwork_schedule_create", json!([]), "invalid_argument");
    fixture.tool_error(
        "riwork_schedule_show",
        json!({"schedule_id":id,"at":at}),
        "invalid_argument",
    );
    fixture.tool_error(
        "riwork_schedule_show",
        json!({"schedule_id":&id[..8]}),
        "invalid_argument",
    );
    fixture.tool_error(
        "riwork_schedule_create",
        json!({
            "scope":"app","project_id":fixture.project_id,"shell_id":fixture.shells[0],
            "title":"Bad scope","prompt":"Fixture","at":at
        }),
        "invalid_argument",
    );
    fixture.tool_error(
        "riwork_schedule_create",
        json!({
            "scope":"workspace","project_id":fixture.project_id,"worktree_id":Uuid::new_v4(),
            "shell_id":fixture.shells[2],"title":"Bad target","prompt":"Fixture","at":at
        }),
        "binding_failed",
    );
    fixture.tool_error("riwork_schedule_create", json!({
        "scope":"app","shell_id":fixture.shells[0],"title":"Bad time","prompt":"Fixture","at":"2030-01-01T09:00:00"
    }), "invalid_argument");
    fixture.tool_error("riwork_schedule_create", json!({
        "scope":"app","shell_id":fixture.shells[0],"title":"Bad interval","prompt":"Fixture","at":at,"every_minutes":4
    }), "invalid_argument");
    fixture.tool_error("riwork_schedule_create", json!({
        "scope":"app","shell_id":&fixture.shells[0][..8],"title":"Short shell","prompt":"Fixture","at":at
    }), "invalid_argument");
    fixture.tool_error(
        "riwork_schedule_create",
        json!({
            "scope":"project","project_id":&fixture.project_id[..8],"shell_id":fixture.shells[1],
            "title":"Short project","prompt":"Fixture","at":at
        }),
        "invalid_argument",
    );
    fixture.tool_error("riwork_schedule_create", json!({
        "scope":"app","shell_id":fixture.shells[0],"title":"Past time","prompt":"Fixture","at":"2000-01-01T09:00:00+00:00"
    }), "invalid_argument");
    fixture.tool_error("riwork_schedule_pause", json!({
        "schedule_id":id,"revision":1,"scope":"project","project_id":fixture.project_id,"shell_id":fixture.shells[0]
    }), "target_mismatch");
    fixture.tool_error(
        "riwork_schedule_pause",
        json!({
            "schedule_id":id,"revision":1,"scope":"app","shell_id":fixture.shells[1]
        }),
        "target_mismatch",
    );
    fixture.tool_error(
        "riwork_schedule_pause",
        json!({
            "schedule_id":id,"revision":0,"scope":"app","shell_id":fixture.shells[0]
        }),
        "invalid_argument",
    );
    let mut stale = vec!["schedule", "update", id, "--revision", "2"];
    stale.extend(fixture.scope_args(0));
    stale.extend(["--at", &at, "--json"]);
    assert_eq!(
        fixture.cli_error(&stale, "revision_conflict")["error"]["current"]["revision"],
        1
    );
    let mut bad_id = vec!["schedule", "delete", &id[..8], "--revision", "1"];
    bad_id.extend(fixture.scope_args(0));
    bad_id.push("--json");
    fixture.cli_error(&bad_id, "invalid_argument");
    let mut bad_time = vec!["schedule", "update", id, "--revision", "1"];
    bad_time.extend(fixture.scope_args(0));
    bad_time.extend(["--at", "2030-01-01T09:00:00", "--json"]);
    fixture.cli_error(&bad_time, "invalid_argument");
    let bad_scope = vec![
        "schedule",
        "list",
        "--project",
        &fixture.project_id,
        "--json",
    ];
    fixture.cli_error(&bad_scope, "invalid_argument");
    assert!(
        fixture
            .command(&["shell", "close", &fixture.shells[2]])
            .status
            .success()
    );
    fixture.tool_error(
        "riwork_schedule_create",
        json!({
            "scope":"workspace","project_id":fixture.project_id,"worktree_id":fixture.worktree_id,
            "shell_id":fixture.shells[2],"title":"Exited worker","prompt":"Fixture","at":at
        }),
        "binding_failed",
    );
    assert_eq!(
        fixture.tool_ok("riwork_schedule_list", json!({}))["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
#[ignore = "slow: three real tmux fixture shells per test"]
fn two_cli_processes_cannot_apply_the_same_revision() {
    let fixture = Fixture::new();
    let created = fixture.tool_ok(
        "riwork_schedule_create",
        json!({
            "scope":"app","shell_id":fixture.shells[0],"title":"Concurrent fixture",
            "prompt":"Fixture prompt","at":future_at(7)
        }),
    )["schedule"]
        .clone();
    let id = created["id"].as_str().unwrap();
    let args = [
        "schedule",
        "pause",
        id,
        "--revision",
        "1",
        "--scope",
        "app",
        "--shell",
        &fixture.shells[0],
        "--json",
    ];
    let spawn = || {
        Command::new(env!("CARGO_BIN_EXE_riwork"))
            .args(args)
            .env("RIWORK_HOME", &fixture.home)
            .env("RIWORK_RUNTIME_DIR", fixture.home.join("runtime"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };
    let first = spawn();
    let second = spawn();
    let results = [
        first.wait_with_output().unwrap(),
        second.wait_with_output().unwrap(),
    ];
    assert_eq!(
        results
            .iter()
            .filter(|result| result.status.success())
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|result| !result.status.success())
            .count(),
        1
    );
    for result in &results {
        let value: Value = serde_json::from_slice(&result.stdout).unwrap();
        if result.status.success() {
            assert_eq!(value["schedule"]["revision"], 2);
        } else {
            assert_eq!(value["error"]["code"], "revision_conflict");
            assert_eq!(value["error"]["current"]["revision"], 2);
        }
    }
    let current = fixture.tool_ok("riwork_schedule_show", json!({"schedule_id":id}));
    assert_eq!(current["schedule"]["revision"], 2);
    assert_eq!(current["schedule"]["paused"], true);
}

#[test]
#[ignore = "slow: three real tmux fixture shells per test"]
fn grok_sessions_and_control_character_titles_are_refused_by_cli_and_mcp() {
    let fixture = Fixture::new();
    let registry = fixture.home.join("sessions.json");
    let mut sessions: Value = serde_json::from_slice(&fs::read(&registry).unwrap()).unwrap();
    for session in sessions["sessions"].as_array_mut().unwrap() {
        if session["id"] == fixture.shells[2].as_str() {
            session["harness"] = json!("grok");
        }
    }
    fs::write(&registry, serde_json::to_vec(&sessions).unwrap()).unwrap();
    let at = future_at(7);

    let mut grok = vec!["schedule", "create"];
    grok.extend(fixture.scope_args(2));
    grok.extend([
        "--title",
        "Grok check",
        "--prompt",
        "Fixture prompt",
        "--at",
        &at,
        "--json",
    ]);
    let error = fixture.cli_error(&grok, "binding_failed");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("grok sessions cannot be scheduled"),
        "{error}"
    );
    let error = fixture.tool_error(
        "riwork_schedule_create",
        json!({
            "scope":"workspace","project_id":fixture.project_id,"worktree_id":fixture.worktree_id,
            "shell_id":fixture.shells[2],"title":"Grok check","prompt":"Fixture prompt","at":at
        }),
        "binding_failed",
    );
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("cannot be scheduled")
    );

    // Titles are printed raw by text-mode commands, so escape sequences are refused.
    let hostile = "Check\u{1b}]0;owned\u{7}";
    let mut cli = vec!["schedule", "create"];
    cli.extend(fixture.scope_args(0));
    cli.extend([
        "--title", hostile, "--prompt", "Fixture", "--at", &at, "--json",
    ]);
    fixture.cli_error(&cli, "invalid_argument");
    fixture.tool_error(
        "riwork_schedule_create",
        json!({
            "scope":"app","shell_id":fixture.shells[0],"title":hostile,"prompt":"Fixture","at":at
        }),
        "invalid_argument",
    );
    let created = fixture.tool_ok(
        "riwork_schedule_create",
        json!({
            "scope":"app","shell_id":fixture.shells[0],"title":"Clean title","prompt":"Fixture","at":at
        }),
    )["schedule"]
        .clone();
    let mut update = vec![
        "schedule",
        "update",
        created["id"].as_str().unwrap(),
        "--revision",
        "1",
    ];
    update.extend(fixture.scope_args(0));
    update.extend(["--at", &at, "--title", hostile, "--json"]);
    fixture.cli_error(&update, "invalid_argument");
    assert_eq!(
        fixture.cli_ok(&["schedule", "list", "--json"])["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
#[ignore = "slow: three real tmux fixture shells per test"]
fn fresh_chat_cli_mcp_create_revision_lifecycle_and_invalid_destinations() {
    let fixture = Fixture::new();
    let at = future_at(8);
    let cli = fixture.cli_ok(&[
        "automation",
        "create",
        "--new-chat",
        "--scope",
        "project",
        "--project",
        &fixture.project_id,
        "--provider",
        "claude",
        "--title",
        "Fresh CLI",
        "--prompt",
        "Fixture only",
        "--at",
        &at,
        "--model",
        "fixture-model",
        "--effort",
        "high",
        "--fast",
        "--json",
    ])["schedule"]
        .clone();
    assert_eq!(cli["target"]["new_chat"]["provider"], "claude");
    assert_eq!(cli["target"]["new_chat"]["approval_mode"], "supervised");
    assert_eq!(cli["target"]["new_chat"]["model"], "fixture-model");
    assert_eq!(cli["target"]["new_chat"]["fast"], true);
    let created=fixture.tool_ok("riwork_schedule_create",json!({"destination":"new_chat","scope":"project","project_id":fixture.project_id,"provider":"claude","permission":"plan","title":"Fresh MCP","prompt":"Fixture only","at":at,"every_minutes":5}))["schedule"].clone();
    assert_eq!(created["target"]["new_chat"]["approval_mode"], "plan");
    let key = json!({"schedule_id":created["id"],"revision":created["revision"],"scope":"project","project_id":fixture.project_id,"shell_id":created["target"]["shell_id"]});
    let paused = fixture.tool_ok("riwork_schedule_pause", key.clone())["schedule"].clone();
    assert_eq!(paused["paused"], true);
    fixture.tool_error("riwork_schedule_resume", key, "revision_conflict");
    let edited=fixture.tool_ok("riwork_schedule_update",json!({"schedule_id":created["id"],"revision":paused["revision"],"scope":"project","project_id":fixture.project_id,"shell_id":created["target"]["shell_id"],"title":"Edited automation","at":future_at(9),"once":true}))["schedule"].clone();
    let mut expected_target = created["target"].clone();
    expected_target["new_chat"]["title"] = json!("Edited automation");
    assert_eq!(edited["target"], expected_target);
    assert_eq!(edited["timing"]["kind"], "once");
    fixture.tool_error("riwork_schedule_create",json!({"destination":"new_chat","scope":"project","project_id":fixture.project_id,"shell_id":fixture.shells[1],"title":"Invalid","prompt":"Fixture","at":at}),"invalid_argument");
    fixture.tool_error("riwork_schedule_create",json!({"destination":"new_chat","scope":"app","title":"Invalid","prompt":"Fixture","at":at}),"invalid_argument");
    fixture.tool_error("riwork_schedule_create",json!({"destination":"new_chat","scope":"project","project_id":Uuid::new_v4(),"title":"Invalid","prompt":"Fixture","at":at}),"binding_failed");
    fixture.tool_error("riwork_schedule_create",json!({"destination":"new_chat","scope":"project","project_id":fixture.project_id,"codex_account_id":"missing-account","title":"Invalid","prompt":"Fixture","at":at}),"binding_failed");
    fixture.tool_error("riwork_schedule_create",json!({"destination":"new_chat","scope":"project","project_id":fixture.project_id,"permission":"danger","title":"Invalid","prompt":"Fixture","at":at}),"invalid_argument");
    fixture.cli_error(
        &[
            "schedule",
            "create",
            "--new-chat",
            "--scope",
            "project",
            "--project",
            &fixture.project_id,
            "--shell",
            &fixture.shells[1],
            "--title",
            "Invalid",
            "--prompt",
            "Fixture",
            "--at",
            &at,
            "--json",
        ],
        "invalid_argument",
    );
    assert!(
        !fixture.home.join("chats").exists(),
        "Saving must not start a provider or create a chat"
    );
}

#[test]
#[ignore = "slow: three real tmux fixture shells per test"]
fn explicit_existing_shell_interfaces_pin_ordinary_project_kind_and_preserve_legacy_defaults() {
    let fixture = Fixture::new();
    let shell = &fixture.shells[2];
    let path = fixture.home.join("sessions.json");
    let mut registry: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    for entry in registry["sessions"].as_array_mut().unwrap() {
        if entry["id"] == *shell {
            entry["worktree_id"] = Value::Null;
        }
    }
    fs::write(path, serde_json::to_vec(&registry).unwrap()).unwrap();
    let at = future_at(10);
    let cli = fixture.cli_ok(&[
        "automation",
        "create",
        "--existing-shell",
        "--scope",
        "project",
        "--project",
        &fixture.project_id,
        "--shell",
        shell,
        "--title",
        "Root shell CLI",
        "--prompt",
        "Fixture only",
        "--at",
        &at,
        "--json",
    ])["schedule"]
        .clone();
    assert_eq!(cli["target"]["shell_kind"], "project");
    assert!(cli["target"].get("new_chat").is_none());
    let created = fixture.tool_ok("riwork_schedule_create", json!({"destination":"existing_shell","scope":"project","project_id":fixture.project_id,"shell_id":shell,"title":"Root shell MCP","prompt":"Fixture only","at":at}))["schedule"].clone();
    assert_eq!(created["target"], cli["target"]);
    fixture.tool_error("riwork_schedule_create", json!({"scope":"project","project_id":fixture.project_id,"shell_id":shell,"title":"Legacy ordinary rejection","prompt":"Fixture only","at":at}), "binding_failed");
    fixture.tool_error("riwork_schedule_create", json!({"destination":"existing_shell","scope":"workspace","project_id":fixture.project_id,"worktree_id":fixture.worktree_id,"shell_id":shell,"title":"Wrong workspace","prompt":"Fixture only","at":at}), "binding_failed");
    let old = fixture.tool_ok("riwork_schedule_create", json!({"scope":"project","project_id":fixture.project_id,"shell_id":fixture.shells[1],"title":"Legacy orchestrator","prompt":"Fixture only","at":at}))["schedule"].clone();
    assert!(old["target"].get("shell_kind").is_none());
    let mut key = json!({"schedule_id":created["id"],"revision":1,"scope":"project","project_id":fixture.project_id,"shell_id":shell});
    let paused = fixture.tool_ok("riwork_schedule_pause", key.clone())["schedule"].clone();
    assert_eq!(paused["target"], created["target"]);
    key["revision"] = 2.into();
    let resumed = fixture.tool_ok("riwork_schedule_resume", key.clone())["schedule"].clone();
    assert_eq!(resumed["target"], created["target"]);
    key["revision"] = 3.into();
    key["at"] = future_at(11).into();
    key["title"] = "Edited root shell".into();
    let updated = fixture.tool_ok("riwork_schedule_update", key.clone())["schedule"].clone();
    assert_eq!(updated["target"], created["target"]);
    key["revision"] = 4.into();
    key.as_object_mut().unwrap().remove("at");
    key.as_object_mut().unwrap().remove("title");
    assert_eq!(
        fixture.tool_ok("riwork_schedule_delete", key)["deleted"],
        true
    );
    assert_eq!(
        fixture.cli_ok(&["schedule", "show", cli["id"].as_str().unwrap(), "--json"])["schedule"]["target"],
        cli["target"]
    );
    assert!(!fixture.home.join("chats").exists());
}

#[test]
#[ignore = "slow: three real tmux fixture shells per test"]
fn new_create_only_fields_are_rejected_on_every_other_mcp_operation_without_mutation() {
    let fixture = Fixture::new();
    let at = future_at(8);
    let saved = fixture.tool_ok(
        "riwork_schedule_create",
        json!({
            "destination":"new_chat", "scope":"project", "project_id":fixture.project_id,
            "provider":"claude", "title":"Create-only validation", "prompt":"Fixture only", "at":at
        }),
    )["schedule"]
        .clone();
    let key = json!({"schedule_id":saved["id"],"revision":saved["revision"],
        "scope":"project","project_id":fixture.project_id,"shell_id":saved["target"]["shell_id"]});
    let fields = [
        ("destination", json!("new_chat")),
        ("provider", json!("codex")),
        ("model", json!("other-model")),
        ("effort", json!("high")),
        ("fast", json!(false)),
        ("permission", json!("full")),
        ("codex_account_id", json!("other-account")),
    ];
    let mut requests = Vec::new();
    for name in [
        "riwork_schedule_update",
        "riwork_schedule_pause",
        "riwork_schedule_resume",
        "riwork_schedule_delete",
        "riwork_schedule_list",
        "riwork_schedule_show",
    ] {
        for (field, value) in &fields {
            // Presence itself is invalid, including explicitly null values.
            for value in [value.clone(), Value::Null] {
                let mut arguments = match name {
                    "riwork_schedule_list" => json!({}),
                    "riwork_schedule_show" => json!({"schedule_id":saved["id"]}),
                    _ => key.clone(),
                };
                if name == "riwork_schedule_update" {
                    arguments["at"] = json!(future_at(9));
                    arguments["title"] = json!("Must not change");
                }
                arguments[*field] = value;
                requests.push(
                    json!({"jsonrpc":"2.0","id":requests.len()+1,"method":"tools/call",
                    "params":{"name":name,"arguments":arguments}}),
                );
            }
        }
    }
    let replies = fixture.mcp(&requests);
    assert_eq!(replies.len(), requests.len());
    for (request, response) in requests.iter().zip(replies) {
        assert_eq!(response["id"], request["id"]);
        let result = &response["result"];
        assert_eq!(result["isError"], true, "{request}: {response}");
        let error: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(
            error["error"]["code"], "invalid_argument",
            "{request}: {response}"
        );
    }
    for (option, value) in [("--provider", "codex"), ("--model", "other-model")] {
        fixture.cli_error(
            &[
                "schedule",
                "update",
                saved["id"].as_str().unwrap(),
                "--revision",
                "1",
                "--scope",
                "project",
                "--project",
                &fixture.project_id,
                "--shell",
                saved["target"]["shell_id"].as_str().unwrap(),
                "--at",
                &at,
                option,
                value,
                "--json",
            ],
            "invalid_argument",
        );
    }
    let unchanged =
        fixture.tool_ok("riwork_schedule_show", json!({"schedule_id":saved["id"]}))["schedule"]
            .clone();
    assert_eq!(unchanged["revision"], 1);
    assert_eq!(unchanged["target"], saved["target"]);
    assert_eq!(unchanged, saved);
    assert!(!fixture.home.join("chats").exists());
}

#[test]
#[ignore = "slow: three real tmux fixture shells per test"]
fn fresh_create_rejects_explicit_shell_presence_even_when_empty() {
    let fixture = Fixture::new();
    let at = future_at(8);
    for shell in [json!(""), Value::Null, json!(fixture.shells[1])] {
        fixture.tool_error("riwork_schedule_create", json!({
            "destination":"new_chat", "scope":"project", "project_id":fixture.project_id,
            "provider":"claude", "shell_id":shell, "title":"Invalid shell presence", "prompt":"Fixture only", "at":at
        }), "invalid_argument");
    }
    for shell in ["", fixture.shells[1].as_str()] {
        fixture.cli_error(
            &[
                "automation",
                "create",
                "--new-chat",
                "--scope",
                "project",
                "--project",
                &fixture.project_id,
                "--provider",
                "claude",
                "--shell",
                shell,
                "--title",
                "Invalid shell presence",
                "--prompt",
                "Fixture only",
                "--at",
                &at,
                "--json",
            ],
            "invalid_argument",
        );
    }
    assert!(
        fixture.tool_ok("riwork_schedule_list", json!({}))["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(!fixture.home.join("chats").exists());
    // Absence is still accepted; the shared service's internal empty sentinel is unchanged.
    let saved = fixture.tool_ok(
        "riwork_schedule_create",
        json!({
            "destination":"new_chat", "scope":"project", "project_id":fixture.project_id,
            "provider":"claude", "title":"Omitted shell", "prompt":"Fixture only", "at":at
        }),
    )["schedule"]
        .clone();
    assert_eq!(saved["revision"], 1);
    assert!(saved["target"].get("new_chat").is_some());
    assert!(!fixture.home.join("chats").exists());
}
