//! Opt-in real installed RiWork/tmux integration. Every process receives a fresh
//! temporary RIWORK_HOME; only UUIDs created by this test are ever sent/closed.
use anyhow::{Context, Result, ensure};
use futures_util::SinkExt;
use riwork_remote::{
    config::{Pairing, Storage, private_read, private_write},
    connector::{connect_registered, receive_json, send_json},
    crypto::{Envelope, ServerHello, Session, accept_server, client_hello, decode, random32},
    relay::{Relay, Routes},
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
};
use tokio::{
    process::{Child, Command},
    time::{Duration, sleep, timeout},
};
use uuid::Uuid;
type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
struct Fixture {
    temp: tempfile::TempDir,
    home: PathBuf,
    cli: PathBuf,
    shells: Vec<String>,
    connector: Option<Child>,
    relay: Option<tokio::task::JoinHandle<()>>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(c) = self.connector.as_mut() {
            let _ = c.start_kill();
        }
        if let Some(t) = self.relay.take() {
            t.abort();
        }
        // Registry is isolated. Close only test-created persistent sessions.
        for id in &self.shells {
            let _ = std::process::Command::new(&self.cli)
                .env("RIWORK_HOME", &self.home)
                .args(["shell", "close", id])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}
impl Fixture {
    fn tmux(&self, args: &[&str]) -> Result<String> {
        let canonical = self.home.canonicalize()?;
        let mut hash = 0xcbf29ce484222325u64;
        for byte in canonical.to_string_lossy().as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        let output = std::process::Command::new("tmux")
            .args(["-L", &format!("riwork-{hash:016x}")])
            .args(args)
            .output()?;
        ensure!(
            output.status.success(),
            "isolated tmux: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(String::from_utf8(output.stdout)?.trim().into())
    }
    async fn cli(&self, args: &[&str]) -> Result<Value> {
        let out = Command::new(&self.cli)
            .env("RIWORK_HOME", &self.home)
            .args(args)
            .arg("--json")
            .kill_on_drop(true)
            .output()
            .await?;
        ensure!(
            out.status.success(),
            "isolated CLI: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(serde_json::from_slice(&out.stdout)?)
    }
    async fn start_connector(&mut self) -> Result<()> {
        self.connector = Some(
            Command::new(env!("CARGO_BIN_EXE_riwork-remote"))
                .env("RIWORK_HOME", &self.home)
                .args(["start", "--riwork"])
                .arg(&self.cli)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()?,
        );
        Ok(())
    }
    async fn stop_connector(&mut self) -> Result<()> {
        if let Some(mut c) = self.connector.take() {
            c.kill().await?;
            c.wait().await?;
        }
        Ok(())
    }
}
async fn cells(f: &Fixture, shell: &str, columns: u32, rows: u32) -> Result<String> {
    let target = format!("{shell}:0.0");
    for _ in 0..300 {
        let size = f.tmux(&[
            "display-message",
            "-p",
            "-t",
            &target,
            "#{pane_width}|#{pane_height}|#{session_name}|#{window_id}|#{pane_id}|#{pane_pid}",
        ])?;
        let expected = format!("{columns}|{rows}|{shell}|");
        if let Some(identity) = size.strip_prefix(&expected) {
            return Ok(identity.to_owned());
        }
        sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!("isolated shell did not reach {columns}x{rows}")
}
async fn harness(path: &Path, turns: usize) -> Result<Value> {
    for _ in 0..100 {
        if let Ok(bytes) = std::fs::read(path) {
            let value: Value = serde_json::from_slice(&bytes)?;
            if value["turns"].as_array().is_some_and(|t| t.len() == turns) {
                return Ok(value);
            }
        }
        sleep(Duration::from_millis(30)).await;
    }
    anyhow::bail!("raw PTY harness did not receive exactly {turns} turns")
}
async fn mobile(pair: &Pairing) -> Result<(Socket, Session)> {
    let end = tokio::time::Instant::now() + Duration::from_secs(5);
    let (mut ws, online) = loop {
        match connect_registered(&pair.relay_url, &pair.route_id, "mobile", &pair.relay_token).await
        {
            Ok(s) => break s,
            Err(e) => {
                if tokio::time::Instant::now() > end {
                    return Err(e);
                }
                sleep(Duration::from_millis(50)).await;
            }
        }
    };
    if !online {
        let peer = timeout(Duration::from_secs(5), receive_json(&mut ws)).await??;
        ensure!(
            peer["type"] == "peer" && peer["online"] == true,
            "expected desktop online"
        );
    }
    let nonce = random32();
    let secret = decode::<32>(&pair.pairing_secret)?;
    let hello = client_hello(&pair.identity(), &secret, nonce)?;
    send_json(&mut ws, &hello).await?;
    let server: ServerHello =
        serde_json::from_value(timeout(Duration::from_secs(5), receive_json(&mut ws)).await??)?;
    let (finish, mut session) = accept_server(&pair.identity(), &secret, &nonce, &server)?;
    send_json(&mut ws, &finish).await?;
    let ready: Envelope = serde_json::from_value(receive_json(&mut ws).await?)?;
    let ready: Value = serde_json::from_slice(&session.open("d2c", &ready)?)?;
    ensure!(
        ready["type"] == "ready" && ready["device_id"] == pair.device_id,
        "ready identity"
    );
    Ok((ws, session))
}
fn request(id: &str, method: &str, params: Value) -> Value {
    json!({"v":1,"type":"request","id":id,"method":method,"params":params})
}
async fn rpc(ws: &mut Socket, s: &mut Session, r: &Value) -> Result<Value> {
    send_json(ws, &s.seal("c2d", &serde_json::to_vec(r)?)?).await?;
    let v = timeout(Duration::from_secs(5), receive_json(ws)).await??;
    let e: Envelope = serde_json::from_value(v)?;
    let response: Value = serde_json::from_slice(&s.open("d2c", &e)?)?;
    ensure!(response["id"] == r["id"], "request correlation");
    Ok(response)
}
async fn call(ws: &mut Socket, s: &mut Session, method: &str, params: Value) -> Result<Value> {
    rpc(ws, s, &request(&Uuid::new_v4().to_string(), method, params)).await
}
async fn marker_count(path: &Path, n: usize) -> Result<()> {
    for _ in 0..40 {
        if std::fs::read(path).unwrap_or_default().len() == n {
            return Ok(());
        }
        sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!("expected exactly {n} bytes of executed input")
}

#[tokio::test]
#[ignore = "requires RIWORK_TEST_CLI absolute installed RiWork CLI and tmux; creates only isolated temporary data"]
async fn real_relay_connector_persistent_shell_retry_restart_and_revocation() -> Result<()> {
    let cli = std::env::var_os("RIWORK_TEST_CLI")
        .map(PathBuf::from)
        .context("set RIWORK_TEST_CLI")?;
    ensure!(cli.is_absolute(), "absolute CLI path required");
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("home");
    std::fs::create_dir(&home)?;
    let mut f = Fixture {
        temp,
        home,
        cli,
        shells: vec![],
        connector: None,
        relay: None,
    };
    let project_dir = f.temp.path().join("project");
    std::fs::create_dir(&project_dir)?;
    let project = f
        .cli(&[
            "project",
            "add",
            project_dir.to_str().unwrap(),
            "--name",
            "isolated remote test",
        ])
        .await?;
    let project_id = project["id"].as_str().context("project ID")?.to_owned();
    let task = f
        .cli(&[
            "task",
            "add",
            "isolated remote fixture",
            "--project",
            &project_id,
        ])
        .await?;
    let shell = f
        .cli(&[
            "shell",
            "create",
            "--project",
            &project_id,
            "--command",
            "/bin/zsh",
        ])
        .await?;
    let shell_id = shell["id"].as_str().context("shell ID")?.to_owned();
    f.shells.push(shell_id.clone());
    let orch = f
        .cli(&[
            "orchestrator",
            "create",
            "--project",
            &project_id,
            "--command",
            "/bin/zsh",
        ])
        .await?;
    let orch_id = orch["id"].as_str().context("orchestrator ID")?.to_owned();
    f.shells.push(orch_id.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let storage = Storage::at(f.home.clone())?;
    let routes_path = f.temp.path().join("routes.json");
    let pair = storage.pair(
        format!("ws://{addr}/v1/ws"),
        "isolated mobile".into(),
        true,
        &f.temp.path().join("pair.json"),
        Some(&routes_path),
    )?;
    let routes: Routes = private_read(&routes_path, 1_048_576)?;
    let relay = Relay::new(routes, 16)?;
    f.relay = Some(tokio::spawn(async move {
        axum::serve(listener, relay.router()).await.unwrap();
    }));
    f.start_connector().await?;
    let (mut ws, mut session) = mobile(&pair).await?;
    let projects = call(&mut ws, &mut session, "projects.list", json!({})).await?;
    assert_eq!(projects["result"]["projects"][0]["id"], project_id);
    let trees = call(
        &mut ws,
        &mut session,
        "worktrees.list",
        json!({"project_id":project_id}),
    )
    .await?;
    assert_eq!(trees["ok"], true);
    let tasks = call(
        &mut ws,
        &mut session,
        "tasks.list",
        json!({"project_id":project_id}),
    )
    .await?;
    assert_eq!(tasks["result"]["tasks"][0]["id"], task["id"]);
    let shells = call(
        &mut ws,
        &mut session,
        "shells.list",
        json!({"project_id":project_id}),
    )
    .await?;
    assert_eq!(shells["result"]["shells"][0]["id"], shell_id);
    let orch = call(&mut ws, &mut session, "orchestrators.list", json!({})).await?;
    assert_eq!(orch["result"]["orchestrators"][0]["id"], orch_id);
    let invalid = call(
        &mut ws,
        &mut session,
        "shell.input",
        json!({"shell_id":shell_id,"line":"first\nsecond"}),
    )
    .await?;
    assert_eq!(invalid["error"]["code"], "invalid_request");
    let unknown = call(
        &mut ws,
        &mut session,
        "shell.output",
        json!({"shell_id":Uuid::new_v4().to_string()}),
    )
    .await?;
    assert_eq!(unknown["error"]["code"], "not_found");
    let marker = f.temp.path().join("input-count");
    let line = format!("printf x >> '{}'", marker.display());
    let input_id = Uuid::new_v4().to_string();
    let input = request(
        &input_id,
        "shell.input",
        json!({"shell_id":shell_id,"line":line}),
    );
    let outcome = rpc(&mut ws, &mut session, &input).await?;
    assert_eq!(outcome["result"]["status"], "sent");
    marker_count(&marker, 1).await?;
    assert_eq!(rpc(&mut ws, &mut session, &input).await?, outcome);
    marker_count(&marker, 1).await?;
    let conflicting = request(
        &input_id,
        "shell.input",
        json!({"shell_id":shell_id,"line":"echo different"}),
    );
    assert_eq!(
        rpc(&mut ws, &mut session, &conflicting).await?["error"]["code"],
        "request_conflict"
    );
    // Set a variable in the persistent shell and read output through encrypted RPC.
    let v=call(&mut ws,&mut session,"shell.input",json!({"shell_id":shell_id,"line":"RIWORK_REMOTE_TEST_STATE=preserved; echo BEFORE_TRANSPORT_STOP"})).await?;
    assert_eq!(v["ok"], true);
    // Drop the transport, stop connector, preserve tmux/harness process state.
    ws.close(None).await?;
    drop(ws);
    f.stop_connector().await?;
    sleep(Duration::from_millis(100)).await;
    assert_eq!(
        f.cli(&["shell", "list", "--project", &project_id]).await?[0]["alive"],
        true
    );
    f.start_connector().await?;
    let (mut ws, mut fresh) = mobile(&pair).await?;
    assert_ne!(fresh.id, session.id);
    assert_eq!(rpc(&mut ws, &mut fresh, &input).await?, outcome);
    marker_count(&marker, 1).await?;
    let v = call(
        &mut ws,
        &mut fresh,
        "shell.input",
        json!({"shell_id":shell_id,"line":"echo AFTER_TRANSPORT_STOP:$RIWORK_REMOTE_TEST_STATE"}),
    )
    .await?;
    assert_eq!(v["ok"], true);
    let mut output = Value::Null;
    for _ in 0..30 {
        output = call(
            &mut ws,
            &mut fresh,
            "shell.output",
            json!({"shell_id":shell_id,"lines":200}),
        )
        .await?;
        if output["result"]["output"]
            .as_str()
            .unwrap_or("")
            .contains("AFTER_TRANSPORT_STOP:preserved")
        {
            break;
        }
        sleep(Duration::from_millis(50)).await;
    }
    assert!(
        output["result"]["output"]
            .as_str()
            .unwrap()
            .contains("AFTER_TRANSPORT_STOP:preserved")
    );
    let orch_input = call(
        &mut ws,
        &mut fresh,
        "shell.input",
        json!({"shell_id":orch_id,"line":"echo ISOLATED_ORCHESTRATOR"}),
    )
    .await?;
    assert_eq!(orch_input["ok"], true);
    // Simulate crash between write-ahead pending and outcome commit. It must
    // never execute again even after connector restart.
    ws.close(None).await?;
    f.stop_connector().await?;
    let ledger_path = storage
        .dir
        .join(format!("outcomes-{}.json", pair.device_id));
    let mut ledger: Value = private_read(&ledger_path, 64 * 1024 * 1024)?;
    ledger["entries"][&input_id]["response"] = Value::Null;
    private_write(&ledger_path, &ledger)?;
    f.start_connector().await?;
    let (mut ws, mut s) = mobile(&pair).await?;
    let unknown = rpc(&mut ws, &mut s, &input).await?;
    assert_eq!(unknown["error"]["code"], "outcome_unknown");
    marker_count(&marker, 1).await?;
    // Tamper, then reconnect: failed authentication must close access, not send.
    let mut tampered = s.seal("c2d", &serde_json::to_vec(&input)?)?;
    tampered.ciphertext.replace_range(
        0..1,
        if tampered.ciphertext.starts_with('A') {
            "B"
        } else {
            "A"
        },
    );
    send_json(&mut ws, &tampered).await?;
    let p = timeout(Duration::from_secs(3), receive_json(&mut ws)).await??;
    assert_eq!(p["type"], "peer");
    assert_eq!(p["online"], false);
    ws.close(None).await?;
    let (mut ws, mut s) = mobile(&pair).await?;
    // Replay the identical authenticated read envelope. It must close this
    // endpoint session rather than process a second request.
    let replay_request = request(&Uuid::new_v4().to_string(), "projects.list", json!({}));
    let replay = s.seal("c2d", &serde_json::to_vec(&replay_request)?)?;
    send_json(&mut ws, &replay).await?;
    let response: Envelope = serde_json::from_value(receive_json(&mut ws).await?)?;
    let _: Value = serde_json::from_slice(&s.open("d2c", &response)?)?;
    send_json(&mut ws, &replay).await?;
    let lost = timeout(Duration::from_secs(3), receive_json(&mut ws)).await??;
    assert_eq!(lost["online"], false);
    ws.close(None).await?;
    // Valid mobile routing token and even the paired PSK cannot authenticate a
    // hello claiming another device's UUID on this route.
    let mut wrong_device = pair.clone();
    wrong_device.device_id = Uuid::new_v4().to_string();
    assert!(mobile(&wrong_device).await.is_err());
    marker_count(&marker, 1).await?;
    let (mut ws, mut s) = mobile(&pair).await?;
    storage.revoke(&pair.device_id)?;
    let revoked = timeout(Duration::from_secs(1), receive_json(&mut ws)).await??;
    assert_eq!(revoked["online"], false);
    // No desktop peer after revoke, even though relay token itself is retained
    // in the mobile pairing document. Submission cannot reach the CLI.
    let _ = ws
        .send(tokio_tungstenite::tungstenite::Message::Text(
            serde_json::to_string(&s.seal("c2d", &serde_json::to_vec(&input)?)?)?.into(),
        ))
        .await;
    assert!(
        timeout(Duration::from_secs(2), receive_json(&mut ws))
            .await?
            .is_err()
    );
    marker_count(&marker, 1).await?;
    f.stop_connector().await?;
    assert_eq!(
        f.cli(&["shell", "list", "--project", &project_id]).await?[0]["alive"],
        true
    );
    println!(
        "PASS: isolated projects/worktrees/tasks/shells/orchestrators; encrypted input/output; cached retry; restart preservation; pending unknown; tamper/replay/wrong-device closure; live revocation; no global data touched."
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires RIWORK_TEST_CLI; isolated real tmux/CLI/relay, 12-second crash recovery"]
async fn mobile_viewport_ownership_restoration_and_long_turns_across_devices() -> Result<()> {
    let cli = PathBuf::from(std::env::var_os("RIWORK_TEST_CLI").context("set RIWORK_TEST_CLI")?);
    ensure!(cli.is_absolute(), "absolute CLI path required");
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("home");
    std::fs::create_dir(&home)?;
    let mut f = Fixture {
        temp,
        home,
        cli,
        shells: vec![],
        connector: None,
        relay: None,
    };
    let project_dir = f.temp.path().join("project");
    std::fs::create_dir(&project_dir)?;
    let project = f
        .cli(&["project", "add", project_dir.to_str().unwrap()])
        .await?;
    let project_id = project["id"].as_str().unwrap();
    let capture = f.temp.path().join("turns.json");
    let negative = f.temp.path().join("negative.json");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/paste_harness.py");
    let command = |path: &Path| {
        format!(
            "exec /usr/bin/python3 '{}' '{}'",
            script.display(),
            path.display()
        )
    };
    for cmd in [command(&capture), command(&negative), "/bin/zsh".into()] {
        let shell = f
            .cli(&[
                "shell",
                "create",
                "--project",
                project_id,
                "--command",
                &cmd,
            ])
            .await?;
        f.shells.push(shell["id"].as_str().unwrap().to_owned());
    }
    let shell = f.shells[0].clone();
    let negative_shell = f.shells[1].clone();
    let other = f.shells[2].clone();
    harness(&capture, 0).await?;
    harness(&negative, 0).await?;
    // Negative control reproduces the reviewed literal-text/immediate-Return bug.
    let negative_pane = format!("{negative_shell}:0.0");
    f.tmux(&[
        "send-keys",
        "-t",
        &negative_pane,
        "-l",
        "--",
        &"n".repeat(3500),
    ])?;
    f.tmux(&["send-keys", "-t", &negative_pane, "Enter"])?;
    sleep(Duration::from_millis(200)).await;
    let bad = harness(&negative, 0).await?;
    assert_eq!(bad["suppressed"], 1);
    assert_eq!(bad["enters"], 1);
    let window = format!("{shell}:0");
    f.tmux(&["resize-window", "-t", &window, "-x", "120", "-y", "40"])?;
    f.tmux(&["set-option", "-wu", "-t", &window, "window-size"])?;
    let original = cells(&f, &shell, 120, 40).await?;
    let other_pane = format!("{other}:0.0");
    let other_size = f.tmux(&[
        "display-message",
        "-p",
        "-t",
        &other_pane,
        "#{pane_width}|#{pane_height}",
    ])?;
    let dims = other_size
        .split('|')
        .map(str::parse::<u32>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let other_identity = cells(&f, &other, dims[0], dims[1]).await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let storage = Storage::at(f.home.clone())?;
    let routes_path = f.temp.path().join("routes.json");
    let relay_url = format!("ws://{}/v1/ws", listener.local_addr()?);
    let a = storage.pair(
        relay_url.clone(),
        "a".into(),
        true,
        &f.temp.path().join("a.json"),
        Some(&routes_path),
    )?;
    let b = storage.pair(
        relay_url,
        "b".into(),
        true,
        &f.temp.path().join("b.json"),
        Some(&routes_path),
    )?;
    let relay = Relay::new(private_read::<Routes>(&routes_path, 1_048_576)?, 16)?;
    f.relay = Some(tokio::spawn(async move {
        axum::serve(listener, relay.router()).await.unwrap();
    }));
    f.start_connector().await?;
    let (mut wa, mut sa) = mobile(&a).await?;
    let (mut wb, mut sb) = mobile(&b).await?;
    let size = json!({"shell_id":shell,"columns":43,"rows":17});
    assert_eq!(
        call(&mut wa, &mut sa, "shell.resize", size.clone()).await?["result"],
        size
    );
    assert_eq!(cells(&f, &shell, 43, 17).await?, original);
    assert_eq!(
        call(&mut wb, &mut sb, "shell.resize", size.clone()).await?["error"]["code"],
        "viewport_busy"
    );
    assert_eq!(
        call(
            &mut wb,
            &mut sb,
            "shell.resize.clear",
            json!({"shell_id":shell})
        )
        .await?["error"]["code"],
        "viewport_busy"
    );
    assert_eq!(cells(&f, &shell, 43, 17).await?, original);
    // One complete long turn, then its cached retry sends neither text nor Enter.
    let long = "a".repeat(3500);
    let input = request(
        &Uuid::new_v4().to_string(),
        "shell.input",
        json!({"shell_id":shell,"line":long}),
    );
    let first = rpc(&mut wa, &mut sa, &input).await?;
    assert_eq!(first["ok"], true);
    assert_eq!(rpc(&mut wa, &mut sa, &input).await?, first);
    let turns = harness(&capture, 1).await?;
    assert_eq!(turns["turns"], json!([long]));
    assert_eq!(turns["enters"], 1);
    assert_eq!(turns["suppressed"], 0);
    assert_eq!(turns["bracketed"], 1);
    assert_eq!(turns["cells"], json!([43, 17]));
    // Switch to another existing tab: release previous size and preserve both PIDs.
    assert_eq!(
        call(
            &mut wa,
            &mut sa,
            "shell.resize",
            json!({"shell_id":other,"columns":57,"rows":21})
        )
        .await?["ok"],
        true
    );
    assert_eq!(cells(&f, &shell, 120, 40).await?, original);
    assert_eq!(cells(&f, &other, 57, 21).await?, other_identity);
    assert_eq!(
        call(
            &mut wa,
            &mut sa,
            "shell.resize.clear",
            json!({"shell_id":other})
        )
        .await?["ok"],
        true
    );
    assert_eq!(cells(&f, &other, dims[0], dims[1]).await?, other_identity);
    // The same device B connection stays usable after a denied resize.
    let line_a = "b".repeat(3500);
    let line_b = "c".repeat(3500);
    let (ra, rb) = tokio::try_join!(
        call(
            &mut wa,
            &mut sa,
            "shell.input",
            json!({"shell_id":shell,"line":line_a})
        ),
        call(
            &mut wb,
            &mut sb,
            "shell.input",
            json!({"shell_id":shell,"line":line_b})
        )
    )?;
    assert_eq!(ra["ok"], true);
    assert_eq!(rb["ok"], true);
    let turns = harness(&capture, 3).await?;
    let got = turns["turns"].as_array().unwrap();
    assert_eq!(got[0], long);
    assert!(
        got[1..] == [json!(line_a.clone()), json!(line_b.clone())]
            || got[1..] == [json!(line_b), json!(line_a)]
    );
    assert_eq!(turns["enters"], 3);
    assert_eq!(turns["suppressed"], 0);
    assert_eq!(turns["bracketed"], 3);
    // Renewal keeps the override beyond a lease period. Graceful peer loss clears it.
    assert_eq!(
        call(&mut wa, &mut sa, "shell.resize", size.clone()).await?["ok"],
        true
    );
    sleep(Duration::from_secs(13)).await;
    assert_eq!(cells(&f, &shell, 43, 17).await?, original);
    wa.close(None).await?;
    assert_eq!(cells(&f, &shell, 120, 40).await?, original);
    let (mut wa, mut fresh) = mobile(&a).await?;
    assert_ne!(fresh.id, sa.id);
    assert_eq!(rpc(&mut wa, &mut fresh, &input).await?, first);
    assert_eq!(
        call(&mut wa, &mut fresh, "shell.resize", size.clone()).await?["ok"],
        true
    );
    // A forged encrypted frame also releases an active viewport immediately.
    let mut forged = fresh.seal(
        "c2d",
        &serde_json::to_vec(&request(
            &Uuid::new_v4().to_string(),
            "projects.list",
            json!({}),
        ))?,
    )?;
    forged.ciphertext.replace_range(
        0..1,
        if forged.ciphertext.starts_with('A') {
            "B"
        } else {
            "A"
        },
    );
    send_json(&mut wa, &forged).await?;
    let lost = timeout(Duration::from_secs(3), receive_json(&mut wa)).await??;
    assert_eq!(lost["online"], false);
    assert_eq!(cells(&f, &shell, 120, 40).await?, original);
    wa.close(None).await?;
    let (mut wa, mut fresh) = mobile(&a).await?;
    assert_eq!(
        call(&mut wa, &mut fresh, "shell.resize", size.clone()).await?["ok"],
        true
    );
    storage.revoke(&a.device_id)?;
    let lost = timeout(Duration::from_secs(1), receive_json(&mut wa)).await??;
    assert_eq!(lost["online"], false);
    assert_eq!(cells(&f, &shell, 120, 40).await?, original);
    assert_eq!(
        call(&mut wb, &mut sb, "shell.resize", size).await?["ok"],
        true
    );
    let start = std::time::Instant::now();
    f.stop_connector().await?; // SIGKILL: independent watchdog must recover.
    assert_eq!(cells(&f, &shell, 120, 40).await?, original);
    assert!(start.elapsed() < Duration::from_secs(15));
    assert_eq!(harness(&capture, 3).await?["enters"], 3);
    assert_eq!(
        f.tmux(&["show-options", "-wqv", "-t", &window, "window-size"])?,
        ""
    );
    println!(
        "PASS: real 120x40 ->43x17 ->120x40; same UUID/window/pane/PID; tab switch; owner denial; renewal, disconnect, reconnect, revoke and SIGKILL restoration; negative immediate-Enter control; complete 3500-char turns, cached retry, cross-device serialization; no user sessions touched."
    );
    Ok(())
}
