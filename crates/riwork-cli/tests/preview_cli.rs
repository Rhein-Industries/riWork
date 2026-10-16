//! Process-level agent loop against an isolated GUI mailbox fixture. No app,
//! simulator, desktop window, or provider is launched by these tests.
use riwork_core::{
    app_preview::{PreviewInput, PreviewMode},
    store::Store,
};
use riwork_engine::{
    app_preview::{PreviewFrame, PreviewPhase, PreviewState},
    preview_control::{PreviewAction, PreviewControl, PreviewReply},
};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

struct Fixture {
    root: PathBuf,
    project: String,
    host: String,
    control: PreviewControl,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("riwork-preview-cli-process-{}", Uuid::new_v4()));
        fs::create_dir_all(root.join("project")).unwrap();
        let project = Store::open(root.join("state"))
            .unwrap()
            .add_project(root.join("project"), Some("Preview fixture"))
            .unwrap()
            .id;
        let control = PreviewControl::open(&root.join("state")).unwrap();
        Self {
            root,
            project,
            host: Uuid::new_v4().to_string(),
            control,
        }
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_riwork-cli"));
        command
            .args(args)
            .current_dir(self.root.join("project"))
            .env("RIWORK_HOME", self.root.join("state"))
            .env("RIWORK_RUNTIME_DIR", self.root.join("runtime"))
            .env_remove("RIWORK_CHAT_ID")
            .env_remove("RIWORK_SHELL_ID");
        command
    }
    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }
    fn ok(&self, args: &[&str]) -> Value {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "{args:?}: {} {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn an_agent_can_open_wait_reuse_touch_capture_and_close_the_exact_preview() {
    let f = Fixture::new();
    f.control.heartbeat(&f.host, &f.project).unwrap();
    let control = f.control.clone();
    let host = f.host.clone();
    let project = f.project.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = Arc::clone(&stop);
    let input = Arc::new(Mutex::new(None));
    let worker_input = Arc::clone(&input);
    let worker = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(15);
        let preview_id = Uuid::new_v4().to_string();
        let mut spec = None;
        let mut state = PreviewState {
            phase: PreviewPhase::Streaming,
            mode: PreviewMode::Cooperative,
            pid: Some(std::process::id()),
            window_id: None,
            windows: vec![],
            message: "Fixture is streaming".into(),
            warning: None,
            frame: Some(Arc::new(PreviewFrame {
                png: b"first-png".to_vec(),
                width: 320,
                height: 640,
                sequence: 1,
            })),
            logs: vec![],
            ios: None,
        };
        while !worker_stop.load(Ordering::Relaxed) && Instant::now() < deadline {
            for request in control.take_requests(&host, &project).unwrap() {
                let reused = spec.is_some();
                match request.action {
                    PreviewAction::Open {
                        spec: requested, ..
                    } => {
                        if spec.is_none() {
                            control.begin(&preview_id, &host).unwrap();
                            spec = Some(requested);
                        }
                        control
                            .publish(
                                &preview_id,
                                &host,
                                &project,
                                spec.as_ref().unwrap(),
                                0,
                                &state,
                            )
                            .unwrap();
                    }
                    PreviewAction::Input {
                        preview_id: id,
                        input,
                    } => {
                        assert_eq!(id, preview_id);
                        *worker_input.lock().unwrap() = Some(input);
                    }
                    PreviewAction::Refresh { preview_id: id } => {
                        assert_eq!(id, preview_id);
                        state.frame = Some(Arc::new(PreviewFrame {
                            png: b"fresh-png".to_vec(),
                            width: 320,
                            height: 640,
                            sequence: 2,
                        }));
                        control
                            .publish(
                                &preview_id,
                                &host,
                                &project,
                                spec.as_ref().unwrap(),
                                0,
                                &state,
                            )
                            .unwrap();
                    }
                    PreviewAction::Close { preview_id: id } => {
                        assert_eq!(id, preview_id);
                        control
                            .closed(&preview_id, &host, &project, spec.as_ref().unwrap())
                            .unwrap();
                    }
                }
                control
                    .respond(
                        &request.id,
                        Ok(PreviewReply {
                            preview_id: preview_id.clone(),
                            reused,
                        }),
                    )
                    .unwrap();
            }
            thread::sleep(Duration::from_millis(5));
        }
    });
    let open = f.ok(&[
        "preview",
        "open",
        "--command",
        "development-app",
        "--wait",
        "--json",
    ]);
    assert_eq!(open["reused"], false);
    assert_eq!(open["status"]["ready"], true);
    let id = open["preview_id"].as_str().unwrap();
    let reopened = f.ok(&[
        "preview",
        "open",
        "--command",
        "development-app",
        "--wait",
        "--json",
    ]);
    assert_eq!(reopened["preview_id"], id);
    assert_eq!(reopened["reused"], true);
    assert_eq!(
        f.ok(&["preview", "status", id, "--json"])["phase"],
        "streaming"
    );
    assert_eq!(
        f.ok(&["preview", "tap", id, "120.5", "300", "--json"])["queued"],
        true
    );
    assert!(matches!(
        *input.lock().unwrap(),
        Some(PreviewInput::Click {
            x: 120.5,
            y: 300.,
            ..
        })
    ));
    let png = f.root.join("capture.png");
    let capture = f.ok(&[
        "preview",
        "screenshot",
        id,
        "--output",
        png.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(capture["frame"]["sequence"], 2);
    assert_eq!(fs::read(png).unwrap(), b"fresh-png");
    assert_eq!(f.ok(&["preview", "close", id, "--json"])["closed"], true);
    assert_eq!(
        f.ok(&["preview", "status", id, "--json"])["phase"],
        "closed"
    );
    stop.store(true, Ordering::Relaxed);
    worker.join().unwrap();
}

#[test]
fn missing_gui_and_invalid_input_return_machine_readable_errors_without_gui_startup() {
    let f = Fixture::new();
    let out = f.run(&["preview", "open", "--ios", "--scheme", "PhoneApp", "--json"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["error"]["code"],
        "host_unavailable"
    );
    assert!(!f.root.join("runtime/instances").exists());
    let out = f.run(&["preview", "tap", "../../state", "NaN", "1", "--json"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["error"]["code"],
        "invalid_request"
    );
}

#[test]
fn mcp_advertises_preview_tools_and_reports_missing_hosts_as_tool_errors() {
    use std::{io::Write, process::Stdio};
    let f = Fixture::new();
    let mut child = f
        .command(&["mcp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let messages = [
        json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"riwork_preview_open","arguments":{"ios":true,"scheme":"PhoneApp"}}}),
    ];
    for message in messages {
        writeln!(child.stdin.as_mut().unwrap(), "{message}").unwrap();
    }
    drop(child.stdin.take());
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let lines = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    let tools = lines[0]["result"]["tools"].as_array().unwrap();
    for name in [
        "riwork_preview_open",
        "riwork_preview_status",
        "riwork_preview_screenshot",
        "riwork_preview_input",
        "riwork_preview_close",
    ] {
        assert!(tools.iter().any(|tool| tool["name"] == name), "{name}");
    }
    assert_eq!(lines[1]["result"]["isError"], true);
    let error: Value =
        serde_json::from_str(lines[1]["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(error["error"]["code"], "host_unavailable");
}
