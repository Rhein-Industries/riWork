//! Opt-in latency and idle-CPU measurement of the path a phone request takes:
//! mobile socket -> relay process -> connector process -> `riwork` CLI -> tmux.
//!
//! Every process is a real one and uses a fresh temporary RIWORK_HOME with one
//! disposable zsh session of its own; nothing outside it is read or written.
//! The relay and connector are the `riwork-remote` binary named by
//! `RIWORK_BENCH_REMOTE` (default: the one this test was built with), so two
//! builds can be compared by pointing the variable at each:
//!
//! ```sh
//! RIWORK_TEST_CLI=/abs/riwork RIWORK_BENCH_REMOTE=/abs/riwork-remote \
//!   cargo test --release --test latency_bench -- --ignored --nocapture
//! ```
//!
//! `RIWORK_BENCH_ROUNDS` (default 40) sets the samples per request type,
//! `RIWORK_BENCH_IDLE_SECONDS` (default 20) the length of the idle phase and
//! `RIWORK_BENCH_LEDGER` (default 4000) how many typed batches the device
//! has already recorded.
use anyhow::{Context, Result, ensure};
use riwork_remote::{
    config::{Pairing, Storage},
    connector::{connect_registered, receive_json, send_json},
    crypto::{Envelope, ServerHello, Session, accept_server, client_hello, decode, random32},
};
use serde_json::{Value, json};
use std::{path::PathBuf, process::Stdio, time::Instant};
use tokio::{
    process::{Child, Command},
    time::{Duration, sleep, timeout},
};
use uuid::Uuid;

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct Bench {
    temp: tempfile::TempDir,
    home: PathBuf,
    cli: PathBuf,
    remote: PathBuf,
    shell: String,
    connector: Option<Child>,
    relay: Option<Child>,
}
impl Drop for Bench {
    fn drop(&mut self) {
        for child in [self.connector.as_mut(), self.relay.as_mut()]
            .into_iter()
            .flatten()
        {
            let _ = child.start_kill();
        }
        let _ = std::process::Command::new(&self.cli)
            .env("RIWORK_HOME", &self.home)
            .args(["shell", "close", &self.shell])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        // The scratch tmux server dies with its last session; make sure.
        let _ = std::process::Command::new("tmux")
            .args(["-L", &self.socket(), "kill-server"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}
impl Bench {
    fn socket(&self) -> String {
        let canonical = self.home.canonicalize().unwrap_or(self.home.clone());
        let mut hash = 0xcbf29ce484222325u64;
        for byte in canonical.to_string_lossy().as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        format!("riwork-{hash:016x}")
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
            "CLI {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(serde_json::from_slice(&out.stdout)?)
    }
    fn tmux(&self, args: &[&str]) -> Result<String> {
        let out = std::process::Command::new("tmux")
            .args(["-L", &self.socket()])
            .args(args)
            .output()?;
        ensure!(out.status.success(), "tmux {args:?}");
        Ok(String::from_utf8(out.stdout)?.trim().to_owned())
    }
}

/// CPU seconds a process used itself, and its reaped children (macOS `proc_pid_rusage`).
#[allow(deprecated)] // libc points to mach2 for the timebase; one call is not worth a dependency
fn cpu_seconds(pid: u32) -> (f64, f64) {
    // SAFETY: plain FFI calls with a correctly sized, zeroed out-buffer.
    unsafe {
        let mut info: libc::rusage_info_v2 = std::mem::zeroed();
        let rc = libc::proc_pid_rusage(
            pid as i32,
            libc::RUSAGE_INFO_V2,
            &mut info as *mut _ as *mut libc::rusage_info_t,
        );
        if rc != 0 {
            return (0.0, 0.0);
        }
        let mut base: libc::mach_timebase_info = std::mem::zeroed();
        libc::mach_timebase_info(&mut base);
        let seconds =
            |ticks: u64| ticks as f64 * f64::from(base.numer) / f64::from(base.denom) / 1e9;
        (
            seconds(info.ri_user_time + info.ri_system_time),
            seconds(info.ri_child_user_time + info.ri_child_system_time),
        )
    }
}
fn pids_matching(pattern: &str) -> Vec<u32> {
    std::process::Command::new("pgrep")
        .args(["-f", pattern])
        .output()
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(|l| l.trim().parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

async fn mobile(pair: &Pairing) -> Result<(Socket, Session)> {
    let end = tokio::time::Instant::now() + Duration::from_secs(10);
    let (mut ws, online) = loop {
        match connect_registered(&pair.relay_url, &pair.route_id, "mobile", &pair.relay_token).await
        {
            Ok(s) => break s,
            Err(e) => {
                ensure!(tokio::time::Instant::now() < end, "relay unreachable: {e}");
                sleep(Duration::from_millis(50)).await;
            }
        }
    };
    if !online {
        let peer = timeout(Duration::from_secs(10), receive_json(&mut ws)).await??;
        ensure!(peer["type"] == "peer" && peer["online"] == true, "desktop");
    }
    let nonce = random32();
    let secret = decode::<32>(&pair.pairing_secret)?;
    send_json(&mut ws, &client_hello(&pair.identity(), &secret, nonce)?).await?;
    let server: ServerHello =
        serde_json::from_value(timeout(Duration::from_secs(5), receive_json(&mut ws)).await??)?;
    let (finish, mut session) = accept_server(&pair.identity(), &secret, &nonce, &server)?;
    send_json(&mut ws, &finish).await?;
    let ready: Envelope = serde_json::from_value(receive_json(&mut ws).await?)?;
    session.open("d2c", &ready)?;
    Ok((ws, session))
}
async fn send(ws: &mut Socket, s: &mut Session, method: &str, params: Value) -> Result<String> {
    let id = Uuid::new_v4().to_string();
    let request = json!({"v":1,"type":"request","id":id,"method":method,"params":params});
    send_json(ws, &s.seal("c2d", &serde_json::to_vec(&request)?)?).await?;
    Ok(id)
}
async fn receive(ws: &mut Socket, s: &mut Session) -> Result<Value> {
    let frame = timeout(Duration::from_secs(30), receive_json(ws)).await??;
    let envelope: Envelope = serde_json::from_value(frame)?;
    Ok(serde_json::from_slice(&s.open("d2c", &envelope)?)?)
}
/// One request, its response, and how long that took in milliseconds.
async fn call(
    ws: &mut Socket,
    s: &mut Session,
    method: &str,
    params: Value,
) -> Result<(Value, f64)> {
    let started = Instant::now();
    let id = send(ws, s, method, params).await?;
    let response = receive(ws, s).await?;
    let ms = started.elapsed().as_secs_f64() * 1000.0;
    ensure!(response["id"] == id, "correlation");
    Ok((response, ms))
}

struct Samples(Vec<f64>);
impl Samples {
    fn report(mut self, label: &str) {
        self.0.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let n = self.0.len();
        let at = |q: f64| self.0[((n - 1) as f64 * q).round() as usize];
        println!(
            "BENCH {label:<34} n={n:<3} min={:>7.1} median={:>7.1} p95={:>7.1} max={:>7.1} ms",
            self.0[0],
            at(0.5),
            at(0.95),
            self.0[n - 1]
        );
    }
}

fn env_number(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[tokio::test]
#[ignore = "requires RIWORK_TEST_CLI; measures a real relay, connector, CLI and tmux in a temporary home"]
async fn request_latency_and_idle_cpu() -> Result<()> {
    let cli = PathBuf::from(std::env::var_os("RIWORK_TEST_CLI").context("set RIWORK_TEST_CLI")?);
    ensure!(cli.is_absolute(), "absolute CLI path required");
    let remote = std::env::var_os("RIWORK_BENCH_REMOTE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_riwork-remote")));
    let rounds = env_number("RIWORK_BENCH_ROUNDS", 40);
    let idle_seconds = env_number("RIWORK_BENCH_IDLE_SECONDS", 20);
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("home");
    std::fs::create_dir(&home)?;
    let mut b = Bench {
        temp,
        home,
        cli,
        remote,
        shell: String::new(),
        connector: None,
        relay: None,
    };
    let project_dir = b.temp.path().join("project");
    std::fs::create_dir(&project_dir)?;
    let project = b
        .cli(&[
            "project",
            "add",
            project_dir.to_str().unwrap(),
            "--name",
            "bench",
        ])
        .await?;
    let project_id = project["id"].as_str().context("project")?.to_owned();
    let shell = b
        .cli(&[
            "shell",
            "create",
            "--project",
            &project_id,
            "--command",
            "/bin/zsh",
        ])
        .await?;
    b.shell = shell["id"].as_str().context("shell")?.to_owned();
    // Plenty of colored scrollback, so history pages have something to carry.
    let fill = Command::new(&b.cli)
        .env("RIWORK_HOME", &b.home)
        .args(["shell", "send", &b.shell])
        .arg(
            "for i in $(seq 1 3000); do printf \"\\033[1;3%dmline %d\\033[0m \\033[38;5;%dmlorem ipsum dolor sit amet consectetur\\033[0m\\n\" $((i%8)) $i $((i%255)); done; echo BENCH-FILLED",
        )
        .status()
        .await?;
    ensure!(fill.success(), "fill");
    for _ in 0..100 {
        let plain = Command::new(&b.cli)
            .env("RIWORK_HOME", &b.home)
            .args(["shell", "output", &b.shell, "--lines", "5"])
            .output()
            .await?;
        if String::from_utf8_lossy(&plain.stdout).contains("BENCH-FILLED") {
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }

    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    drop(listener);
    let storage = Storage::at(b.home.clone())?;
    let routes = b.temp.path().join("routes.json");
    let pair = storage.pair(
        format!("ws://{addr}/v1/ws"),
        "bench phone".into(),
        true,
        &b.temp.path().join("pair.json"),
        Some(&routes),
    )?;
    // A device that has typed for a while: the batch ledger is kept full (4096
    // entries, the oldest pruned as new ones come), and every batch reads and
    // rewrites all of it.
    let full = env_number("RIWORK_BENCH_LEDGER", 4000);
    if full > 0 {
        let batches: Vec<Value> = (0..full)
            .map(|_| json!({"batch":Uuid::new_v4().to_string(),"state":"sent"}))
            .collect();
        riwork_remote::config::private_write(
            &b.home
                .join("remote")
                .join(format!("keys-{}.json", pair.device_id)),
            &json!({"batches":batches}),
        )?;
    }
    b.relay = Some(
        Command::new(&b.remote)
            .args(["relay", "--bind", &addr.to_string(), "--routes"])
            .arg(&routes)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?,
    );
    b.connector = Some(
        Command::new(&b.remote)
            .env("RIWORK_HOME", &b.home)
            .args(["start", "--riwork"])
            .arg(&b.cli)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?,
    );
    let connector_pid = b.connector.as_ref().and_then(Child::id).context("pid")?;
    let (mut ws, mut s) = mobile(&pair).await?;
    let shell = b.shell.clone();

    // What the phone does on connect: size the terminal, then read it.
    let (resize, _) = call(
        &mut ws,
        &mut s,
        "shell.resize",
        json!({"shell_id":shell,"columns":50,"rows":20}),
    )
    .await?;
    ensure!(resize["ok"] == true, "resize: {resize}");
    let live = json!({"shell_id":shell,"lines":120,"styled":true});
    let (first, _) = call(&mut ws, &mut s, "shell.output", live.clone()).await?;
    ensure!(first["ok"] == true, "output: {first}");
    ensure!(first["result"]["hash"].is_string(), "hash");

    // Immediate reads.
    let mut samples = Samples(vec![]);
    for round in 0..rounds + 3 {
        let (r, ms) = call(&mut ws, &mut s, "shell.output", live.clone()).await?;
        ensure!(r["ok"] == true, "output: {r}");
        if round >= 3 {
            samples.0.push(ms);
        }
    }
    samples.report("shell.output 120 styled");
    for lines in [300, 1000] {
        let mut samples = Samples(vec![]);
        for round in 0..rounds / 2 + 3 {
            let (r, ms) = call(
                &mut ws,
                &mut s,
                "shell.history",
                json!({"shell_id":shell,"end":0,"lines":lines,"styled":true}),
            )
            .await?;
            ensure!(r["ok"] == true, "history: {r}");
            if round >= 3 {
                samples.0.push(ms);
            }
        }
        samples.report(&format!("shell.history {lines} styled"));
    }

    // Typing: a key batch, then the same with a long poll waiting for the echo.
    let mut keys = Samples(vec![]);
    let mut poll_echo = Samples(vec![]);
    for round in 0..rounds + 3 {
        let item = if round % 2 == 0 {
            json!({"text":"x"})
        } else {
            json!({"key":"Backspace"})
        };
        // The screen as the phone has it, once the previous echo has settled.
        sleep(Duration::from_millis(150)).await;
        let (current, _) = call(&mut ws, &mut s, "shell.output", live.clone()).await?;
        let hash = current["result"]["hash"]
            .as_str()
            .context("hash")?
            .to_owned();
        // The long poll first, as the phone keeps one waiting while it types.
        let poll = send(
            &mut ws,
            &mut s,
            "shell.output",
            json!({"shell_id":shell,"lines":120,"styled":true,"if_changed":hash,"wait_ms":8000}),
        )
        .await?;
        sleep(Duration::from_millis(400)).await;
        let started = Instant::now();
        let batch = send(
            &mut ws,
            &mut s,
            "shell.keys",
            json!({"shell_id":shell,"batch":Uuid::new_v4().to_string(),"items":[item]}),
        )
        .await?;
        let (mut keys_ms, mut poll_ms) = (None, None);
        while keys_ms.is_none() || poll_ms.is_none() {
            let r = receive(&mut ws, &mut s).await?;
            let ms = started.elapsed().as_secs_f64() * 1000.0;
            if r["id"] == batch {
                ensure!(r["ok"] == true, "keys: {r}");
                keys_ms = Some(ms);
            } else if r["id"] == poll {
                ensure!(r["ok"] == true, "poll: {r}");
                ensure!(r["result"]["unchanged"] != true, "echo not seen: {r}");
                poll_ms = Some(ms);
            }
        }
        if round >= 3 {
            keys.0.push(keys_ms.unwrap());
            poll_echo.0.push(poll_ms.unwrap());
        }
    }
    keys.report("shell.keys (one item)");
    poll_echo.report("keystroke -> echo reaches phone");

    // Idle with the phone attached and nothing typed: the lease is renewed,
    // and the phone either keeps a long poll waiting (as it does while a
    // terminal is on screen) or has none. CPU of every process involved.
    let tmux_pid: u32 = b.tmux(&["display-message", "-p", "#{pid}"])?.parse()?;
    for with_poll in [false, true] {
        sleep(Duration::from_millis(500)).await;
        let (current, _) = call(&mut ws, &mut s, "shell.output", live.clone()).await?;
        let hash = current["result"]["hash"]
            .as_str()
            .context("hash")?
            .to_owned();
        let idle_poll =
            json!({"shell_id":shell,"lines":120,"styled":true,"if_changed":hash,"wait_ms":8000});
        let mut pending = None;
        if with_poll {
            pending = Some(send(&mut ws, &mut s, "shell.output", idle_poll.clone()).await?);
        }
        sleep(Duration::from_secs(2)).await;
        let sample = |b: &Bench| {
            let watchdogs: f64 = pids_matching(&format!("viewport-watch {}", b.shell))
                .into_iter()
                .map(|pid| cpu_seconds(pid).0 + cpu_seconds(pid).1)
                .sum();
            (cpu_seconds(connector_pid), cpu_seconds(tmux_pid), watchdogs)
        };
        let (before, started) = (sample(&b), Instant::now());
        let mut answered = 0;
        while started.elapsed() < Duration::from_secs(idle_seconds as u64) {
            if let Ok(r) = timeout(Duration::from_millis(250), receive(&mut ws, &mut s)).await {
                let r = r?;
                ensure!(
                    Some(&r["id"]) == pending.as_ref().map(|p| json!(p)).as_ref(),
                    "{r}"
                );
                answered += 1;
                // Nothing was typed: it can only have waited out (`unchanged`).
                ensure!(
                    r["result"]["unchanged"] == true,
                    "screen changed while idle: {r}"
                );
                pending = Some(send(&mut ws, &mut s, "shell.output", idle_poll.clone()).await?);
            }
        }
        let seconds = started.elapsed().as_secs_f64();
        let after = sample(&b);
        let pct = |x: f64| 100.0 * x / seconds;
        let own = after.0.0 - before.0.0;
        let children = after.0.1 - before.0.1;
        let (tmux, watchdog) = (
            after.1.0 + after.1.1 - before.1.0 - before.1.1,
            after.2 - before.2,
        );
        println!(
            "BENCH idle, {} long poll: {:.2}% of one core over {seconds:.0} s = connector {:.2}% + its CLI/tmux children {:.2}% + tmux server {:.2}% + lease watchdog {:.2}% ({answered} polls answered)",
            if with_poll { "with a" } else { "no" },
            pct(own + children + tmux + watchdog),
            pct(own),
            pct(children),
            pct(tmux),
            pct(watchdog),
        );
        // Let a poll still in flight finish before the next phase.
        if let Some(id) = pending {
            let r = receive(&mut ws, &mut s).await?;
            ensure!(r["id"] == json!(id), "correlation");
        }
    }
    Ok(())
}
