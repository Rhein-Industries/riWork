//! Authenticated-account stdio transport. SSH owns login, host keys and encryption;
//! the remote process exposes the existing RPC API, never a new network listener.
use crate::{
    client::{
        CallError, Command, Connection, Features, HostRecord, HostSummary, HostsFile, LinkState,
        LinkTiming, Outcome, Reply,
    },
    config::Storage,
    lanes::{self, Lanes},
    rpc::Rpc,
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::HashMap, path::PathBuf, process::Stdio, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command as Process},
    sync::{mpsc, oneshot, watch},
    task::JoinSet,
    time::{Instant, timeout},
};

const VERSION: u8 = 1;
const MAX_LINE: usize = crate::link::MAX_INFLATED;
const HANDSHAKE: Duration = Duration::from_secs(15);
const WRITE_WAIT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Route {
    pub target: String,
    pub remote_bin: String,
    pub somtus: Option<String>,
    pub home: Option<String>,
    pub host_id: String,
    pub client_id: String,
}
impl Route {
    fn validate_arguments(&self) -> Result<()> {
        ensure!(
            !self.target.is_empty()
                && self.target.len() <= 512
                && !self.target.starts_with('-')
                && !self
                    .target
                    .chars()
                    .any(|c| c.is_whitespace() || c.is_control()),
            "invalid SSH target; use an SSH config alias or user@host"
        );
        for value in std::iter::once(&self.remote_bin)
            .chain(self.somtus.iter())
            .chain(self.home.iter())
        {
            ensure!(
                !value.is_empty() && value.len() <= 4096 && !value.chars().any(char::is_control),
                "invalid remote path"
            );
        }
        crate::crypto::uuid(&self.client_id)?;
        Ok(())
    }
    pub fn validate(&self) -> Result<()> {
        self.validate_arguments()?;
        crate::crypto::uuid(&self.host_id)?;
        Ok(())
    }
    fn remote_command(&self) -> String {
        let mut command = String::new();
        if let Some(home) = &self.home {
            command.push_str(&format!("env RIWORK_HOME={} ", quote(home)));
        }
        command.push_str(&format!("{} connect", quote(&self.remote_bin)));
        if let Some(cli) = &self.somtus {
            command.push_str(&format!(" --somtus {}", quote(cli)));
        }
        command
    }
    fn command(&self) -> Process {
        let mut command = Process::new("ssh");
        command
            .args([
                "-T",
                "-o",
                "BatchMode=yes",
                "-o",
                "StrictHostKeyChecking=yes",
                "-o",
                "ConnectTimeout=10",
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "ServerAliveCountMax=3",
                "--",
            ])
            .arg(&self.target)
            .arg(self.remote_command())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }
}
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Cancel-safe bounded JSON lines: partial bytes remain in `pending` when a
/// heartbeat or outgoing request wins the surrounding select.
async fn read_json<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    pending: &mut Vec<u8>,
    limit: usize,
) -> Result<Option<Value>> {
    loop {
        let bytes = reader.fill_buf().await?;
        if bytes.is_empty() {
            ensure!(pending.is_empty(), "truncated stdio frame");
            return Ok(None);
        }
        let end = bytes.iter().position(|b| *b == b'\n');
        let count = end.map_or(bytes.len(), |n| n + 1);
        ensure!(
            pending.len() + count <= limit + 1,
            "stdio frame exceeds limit"
        );
        pending.extend_from_slice(&bytes[..count]);
        reader.consume(count);
        if end.is_some() {
            let value = serde_json::from_slice(pending)?;
            pending.clear();
            return Ok(Some(value));
        }
    }
}
async fn write_json<W: AsyncWrite + Unpin>(writer: &mut W, value: &Value) -> Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    ensure!(bytes.len() <= MAX_LINE, "stdio frame exceeds limit");
    bytes.push(b'\n');
    timeout(WRITE_WAIT, async {
        writer.write_all(&bytes).await?;
        writer.flush().await
    })
    .await
    .context("stdio write timed out")??;
    Ok(())
}

struct Dialed {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    pending: Vec<u8>,
    stderr: tokio::task::JoinHandle<()>,
}
impl Drop for Dialed {
    fn drop(&mut self) {
        self.stderr.abort();
        let _ = self.child.start_kill();
    }
}
async fn dial(route: &Route, expected: Option<&str>) -> Result<(Dialed, Value)> {
    route.validate_arguments()?;
    dial_command(route, expected, route.command()).await
}
async fn dial_command(
    route: &Route,
    expected: Option<&str>,
    mut command: Process,
) -> Result<(Dialed, Value)> {
    let mut child = command
        .spawn()
        .context("start SSH (log in and trust the host using ssh first)")?;
    let input = child.stdin.take().context("SSH stdin")?;
    let output = BufReader::new(child.stdout.take().context("SSH stdout")?);
    let mut stderr = child.stderr.take().context("SSH stderr")?;
    // Drain without logging remote output, paths or potential credentials.
    let stderr = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
    });
    let mut dialed = Dialed {
        child,
        input,
        output,
        pending: Vec::new(),
        stderr,
    };
    write_json(&mut dialed.input, &json!({"v":VERSION,"type":"hello","client_id":route.client_id,"expected_host_id":expected})).await?;
    let hello = timeout(
        HANDSHAKE,
        read_json(&mut dialed.output, &mut dialed.pending, MAX_LINE),
    )
    .await
    .context("SSH handshake timed out; check the installed Somtus paths and SSH login")??
    .context("SSH closed before ready; check ssh login, known_hosts and remote executables")?;
    validate_ready(&hello, expected)?;
    Ok((dialed, hello))
}
fn validate_ready(ready: &Value, expected: Option<&str>) -> Result<()> {
    ensure!(
        ready["v"] == VERSION && ready["type"] == "ready",
        "invalid SSH ready handshake"
    );
    let id = ready["host_id"]
        .as_str()
        .context("SSH ready has no host identity")?;
    crate::crypto::uuid(id)?;
    ensure!(
        expected.is_none_or(|expected| expected == id),
        "SSH host identity changed; reconnect refused before sending requests"
    );
    Ok(())
}

pub async fn add(
    storage: &Storage,
    mut route: Route,
    label: Option<String>,
) -> Result<HostSummary> {
    let (dialed, ready) = dial(&route, None).await?;
    route.host_id = ready["host_id"]
        .as_str()
        .context("host identity")?
        .to_owned();
    drop(dialed);
    let record = HostRecord {
        id: uuid::Uuid::new_v4().to_string(),
        label: label.unwrap_or_else(|| route.target.clone()),
        pairing: None,
        ssh: Some(route),
        relay: String::new(),
        route: String::new(),
        added_at_unix: crate::client::now_unix(),
        allow_insecure_loopback: false,
    };
    record.validate()?;
    let _lock = HostsFile::lock(storage)?;
    let mut file = HostsFile::load(storage)?;
    ensure!(file.hosts.len() < 64, "host registry is full");
    ensure!(
        !file.hosts.iter().any(|h| h
            .ssh
            .as_ref()
            .is_some_and(|s| s.target == record.ssh.as_ref().unwrap().target
                && s.home == record.ssh.as_ref().unwrap().home)),
        "this SSH target/home is already registered"
    );
    let summary = record.summary();
    file.hosts.push(record);
    file.save(storage)?;
    Ok(summary)
}

pub(crate) async fn attempt(
    route: &Route,
    timing: LinkTiming,
    state: &watch::Sender<LinkState>,
    conn: &watch::Sender<Option<Connection>>,
    generation: &mut u64,
) -> Outcome {
    let (dialed, ready) = match dial(route, Some(&route.host_id)).await {
        Ok(v) => v,
        Err(e) => return Outcome::failed(format!("SSH: {e:#}")),
    };
    connected(dialed, ready, timing, state, conn, generation).await
}
async fn connected(
    mut dialed: Dialed,
    ready: Value,
    timing: LinkTiming,
    state: &watch::Sender<LinkState>,
    conn: &watch::Sender<Option<Connection>>,
    generation: &mut u64,
) -> Outcome {
    *generation += 1;
    let (tx, mut rx) = mpsc::channel(256);
    conn.send_replace(Some(Connection {
        generation: *generation,
        features: Features::from_ready(&ready),
        tx,
    }));
    state.send_replace(LinkState::Online { rtt_ms: None });
    let started = Instant::now();
    let mut heard = Instant::now();
    let mut ping_sent = None;
    let mut pending: HashMap<String, (Instant, oneshot::Sender<Result<Reply, CallError>>)> =
        HashMap::new();
    let mut ping = tokio::time::interval(timing.ping);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let result: Result<()> = async {
        loop {
            tokio::select! {
                command = rx.recv() => match command {
                    Some(Command::Request { id, method, params, reply }) => {
                        if reply.is_closed() { continue; }
                        pending.retain(|_, (_, reply)| !reply.is_closed());
                        let request = json!({"v":1,"type":"request","id":id,"method":method,"params":params});
                        if pending.len() >= 256 || serde_json::to_vec(&request)?.len() > crate::MAX_PLAINTEXT {
                            let _ = reply.send(Err(CallError::Invalid("request not sent: SSH capacity limit".into()))); continue;
                        }
                        pending.insert(id, (Instant::now(), reply));
                        write_json(&mut dialed.input, &request).await?;
                    },
                    Some(Command::Reset(reason)) => bail!("{reason}"), None => return Ok(()),
                },
                value = read_json(&mut dialed.output, &mut dialed.pending, MAX_LINE) => {
                    let value = value?.context("SSH connection closed")?; heard = Instant::now();
                    ensure!(value["v"] == VERSION, "unsupported SSH protocol version");
                    if value["type"] == "pong" {
                        if let Some(sent) = ping_sent.take() { state.send_replace(LinkState::Online { rtt_ms: Some(Instant::now().duration_since(sent).as_millis() as u64) }); }
                    } else {
                        ensure!(value["type"] == "response", "unexpected SSH frame");
                        if let Some((sent, reply)) = value["id"].as_str().and_then(|id| pending.remove(id)) {
                            let _ = reply.send(Ok(Reply { response: value, elapsed: sent.elapsed() }));
                        }
                    }
                },
                _ = ping.tick() => {
                    ensure!(heard.elapsed() < timing.silence, "SSH host stopped answering");
                    if ping_sent.is_none() { ping_sent = Some(Instant::now()); }
                    write_json(&mut dialed.input, &json!({"v":VERSION,"type":"ping"})).await?;
                },
            }
        }
    }.await;
    conn.send_replace(None);
    rx.close();
    let reason = result
        .err()
        .map_or_else(|| "SSH link closed".into(), |e| format!("SSH: {e:#}"));
    let lost = CallError::Offline(reason.clone());
    for (_, (_, reply)) in pending {
        let _ = reply.send(Err(lost.clone()));
    }
    while let Ok(Command::Request { reply, .. }) = rx.try_recv() {
        let _ = reply.send(Err(lost.clone()));
    }
    Outcome {
        reason: crate::log_safe(&reason),
        online_for: Some(started.elapsed()),
        host_away: false,
    }
}

/// Run only as an OS-account-authorized stdio endpoint (normally `ssh ...
/// somtus-remote connect`). It creates no listening socket and grants no new
/// account authority. Requests use the same allowlist, lanes and outcome ledger.
pub async fn serve(storage: Storage, cli: PathBuf) -> Result<()> {
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
    }
    serve_io(storage, cli, tokio::io::stdin(), tokio::io::stdout()).await
}
async fn serve_io(
    storage: Storage,
    cli: PathBuf,
    input: impl AsyncRead + Unpin,
    mut output: impl AsyncWrite + Unpin,
) -> Result<()> {
    let mut input = BufReader::new(input);
    let mut partial = Vec::new();
    let hello = timeout(HANDSHAKE, read_json(&mut input, &mut partial, 16 * 1024))
        .await??
        .context("missing SSH hello")?;
    ensure!(
        hello["v"] == VERSION && hello["type"] == "hello",
        "invalid SSH hello"
    );
    let client = hello["client_id"]
        .as_str()
        .context("missing client id")?
        .to_owned();
    crate::crypto::uuid(&client)?;
    let rpc = Arc::new(Rpc::for_account_session(
        cli,
        storage.clone(),
        client.clone(),
    )?);
    let identity = rpc.handle(&client, json!({"v":1,"type":"request","id":uuid::Uuid::new_v4().to_string(),"method":"native.invoke","params":{"module":"Hosts","method":"local","args":{}}})).await?;
    ensure!(
        identity["ok"] == true,
        "installed Somtus must support Hosts.local for stable SSH identity: {}",
        identity["error"]["message"]
            .as_str()
            .unwrap_or("invalid response")
    );
    let host_id = identity["result"]["value"]["hostId"]
        .as_str()
        .context("Hosts.local did not report identity")?;
    crate::crypto::uuid(host_id)?;
    if let Some(expected) = hello["expected_host_id"].as_str() {
        ensure!(expected == host_id, "SSH host identity changed");
    }
    // Upload sweeping may coexist with the relay connector. This is an owner
    // marker only; it grants no relay authorization and contains no credentials.
    crate::config::private_write(
        &storage.dir.join(format!("ssh-client-{client}.json")),
        &json!({"v":1,"host_id":host_id}),
    )?;
    let (native, watch_features) = rpc.native_features().await;
    write_json(&mut output, &json!({"v":VERSION,"type":"ready","host_id":host_id,"features":{"pty":crate::pty::features(),"history_max_lines":rpc.history_max_lines(),"native":native,"watch":watch_features}})).await?;
    let viewport = Arc::new(tokio::sync::Mutex::new(None));
    let pty = crate::pty::PtySet::new();
    let (ended, _) = watch::channel(false);
    let mut lanes = Lanes::default();
    let mut jobs = JoinSet::new();
    let result: Result<()> = async {
        loop {
            while let Some((lane, request)) = lanes.next_ready() {
                let (rpc, client, viewport, pty) = (rpc.clone(), client.clone(), viewport.clone(), pty.clone()); let mut ended = ended.subscribe();
                jobs.spawn(async move {
                    let run = rpc.handle_session_up_to(&client, request, &viewport, Some(&pty), MAX_LINE);
                    let response = if lane.cancellable() {
                        tokio::select! { result = run => Some(result), _ = ended.changed() => None }
                    } else { Some(run.await) };
                    (lane, response)
                });
            }
            tokio::select! {
                frame = read_json(&mut input, &mut partial, crate::MAX_PLAINTEXT), if lanes.queued() < lanes::MAX_QUEUED => {
                    let Some(frame) = frame? else { return Ok(()) };
                    if frame["v"] == VERSION && frame["type"] == "ping" { write_json(&mut output, &json!({"v":VERSION,"type":"pong"})).await?; }
                    else if matches!(frame["method"].as_str(), Some("pty.write" | "pty.resize" | "pty.close")) {
                        write_json(&mut output, &rpc.handle_pty_inline(&client, frame, &pty)?).await?;
                    } else { lanes.push(lanes::classify(&frame), frame); }
                },
                done = jobs.join_next(), if !jobs.is_empty() => {
                    let (lane, response) = done.context("request task ended")??; lanes.finished(lane);
                    if let Some(response) = response { write_json(&mut output, &response?).await?; }
                },
            }
        }
    }.await;
    // Never start queued mutations after observing disconnect, never cancel accepted
    // ordered work halfway through, and never carry requests into a new link.
    ended.send_replace(true);
    while jobs.join_next().await.is_some() {}
    pty.close_all();
    if let Some(mut viewport) = viewport.lock().await.take() {
        let _ = rpc.clear_viewport(&mut viewport).await;
    }
    result
}

#[cfg(test)]
mod tests;
