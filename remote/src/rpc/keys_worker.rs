//! The typing worker: one long-lived `somtus shell keys --serve` per device.
//!
//! A `shell.keys` batch used to start its own `somtus shell keys SHELL -- ITEM...`
//! process, so a phone typing a word cost a process start per batch. When the CLI
//! says it can (`shell_keys_serve` in `somtus capabilities --json`), `Rpc::keys`
//! hands the batch to this device's worker instead, which types it exactly as that
//! process would (`crates/riwork-cli/src/cli/keys_serve.rs`). Everything around the
//! typing stays in `Rpc::keys` as it was: the batch ledger written before anything is
//! typed, the device and shell locks (so batches still go one at a time, in order),
//! and the reading of the CLI's error tokens. A worker answers with the same error
//! text the process would have printed, so `keys_fault` reads both alike.
//!
//! What can go wrong and what it means for the batch:
//!
//! - No worker could be started, or it never said it was ready: nothing reached it,
//!   and `Rpc::keys` types the batch with a process of its own, as before.
//! - Writing the request failed: the worker never had the whole line (its last byte
//!   is the newline), and it is killed, so nothing was typed. One fresh worker is
//!   tried, then the process.
//! - The request was written and no proper answer came (the worker died, timed out
//!   or answered something else): it may have typed part of the batch. The worker
//!   is killed and the batch is reported like a process that failed after typing
//!   began, so a repeat answers `uncertain`.
//!
//! A worker is only ever used by one batch at a time (the caller holds the device's
//! keys lock), and is taken out of its slot while it is in use: a batch that is cut
//! short (its task dropped) drops the worker with it, and the process is killed, so
//! a later batch never reads an answer meant for another.
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    process::Stdio,
    sync::Mutex,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    time::timeout,
};

/// A worker idle this long is replaced when it is next needed, so a CLI updated
/// meanwhile is the one that types.
const IDLE: Duration = Duration::from_secs(600);
/// How long a new worker has to say it is ready.
const READY: Duration = Duration::from_secs(5);
/// The longest answer line read from a worker.
const MAX_ANSWER: u64 = 64 * 1024;

/// What became of one batch.
pub(super) enum Outcome {
    /// Typed.
    Sent,
    /// Refused with the CLI's own error text (what `somtus: ` would precede):
    /// a token first when nothing was typed.
    Refused(String),
    /// No worker had it: nothing was typed.
    Unavailable(anyhow::Error),
    /// A worker had it and gave no proper answer: part of it may be typed.
    Lost(anyhow::Error),
}

struct Worker {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    /// The `n` of the next request.
    next: u64,
    used: Instant,
}

impl Worker {
    /// A worker that is still running and was used lately enough.
    fn fresh(&mut self) -> bool {
        self.used.elapsed() < IDLE && matches!(self.child.try_wait(), Ok(None))
    }

    /// One answer line, at most `MAX_ANSWER` bytes.
    async fn line(&mut self) -> Result<Value> {
        let mut line = Vec::new();
        (&mut self.stdout)
            .take(MAX_ANSWER)
            .read_until(b'\n', &mut line)
            .await
            .context("read the typing worker's answer")?;
        if line.last() != Some(&b'\n') {
            bail!("the typing worker stopped answering");
        }
        serde_json::from_slice(&line).context("the typing worker's answer is not JSON")
    }
}

/// The workers of every device, by device id. Only idle ones are here.
pub(super) struct KeysWorkers {
    cli: PathBuf,
    idle: Mutex<BTreeMap<String, Worker>>,
}

impl KeysWorkers {
    pub(super) fn new(cli: PathBuf) -> Self {
        Self {
            cli,
            idle: Mutex::new(BTreeMap::new()),
        }
    }

    /// Lets go of (and stops) a device's worker.
    pub(super) fn forget(&self, device: &str) {
        self.idle.lock().expect("typing workers").remove(device);
    }

    /// Types `items` (the argv encoding of `shell keys`) into `shell` with
    /// `device`'s worker, starting one if there is none. The caller holds the
    /// device's keys lock. `limit` bounds the whole call.
    pub(super) async fn send(
        &self,
        device: &str,
        shell: &str,
        items: &[String],
        limit: Duration,
    ) -> Outcome {
        let started = Instant::now();
        let mut held = self.idle.lock().expect("typing workers").remove(device);
        // One that exited or sat idle too long is dropped (stopped) and replaced.
        if held.as_mut().is_some_and(|worker| !worker.fresh()) {
            held = None;
        }
        let mut last_error = anyhow!("no typing worker");
        for _ in 0..2 {
            let remaining = limit.saturating_sub(started.elapsed());
            let mut worker = match held.take() {
                Some(worker) => worker,
                None => match self.spawn(remaining.min(READY)).await {
                    Ok(worker) => worker,
                    Err(error) => return Outcome::Unavailable(error),
                },
            };
            let n = worker.next;
            worker.next += 1;
            let mut request = json!({"n": n, "shell": shell, "items": items}).to_string();
            request.push('\n');
            let remaining = limit.saturating_sub(started.elapsed());
            match timeout(remaining, worker.stdin.write_all(request.as_bytes())).await {
                Ok(Ok(())) => {}
                // The worker never had the whole line, and is dropped (killed) here.
                Ok(Err(error)) => {
                    last_error = anyhow!(error).context("write to the typing worker");
                    continue;
                }
                Err(_) => {
                    last_error = anyhow!("the typing worker did not take the batch in time");
                    continue;
                }
            }
            let remaining = limit.saturating_sub(started.elapsed());
            let answer = match timeout(remaining, worker.line()).await {
                Ok(Ok(answer)) => answer,
                Ok(Err(error)) => return Outcome::Lost(error),
                Err(_) => return Outcome::Lost(anyhow!("Somtus CLI timeout")),
            };
            if answer.get("n") != Some(&json!(n)) {
                return Outcome::Lost(anyhow!("the typing worker answered another request"));
            }
            let outcome = match (answer.get("ok"), answer.get("error")) {
                (Some(Value::Bool(true)), None) => Outcome::Sent,
                (Some(Value::Bool(false)), Some(Value::String(error))) => {
                    Outcome::Refused(error.trim().to_owned())
                }
                _ => return Outcome::Lost(anyhow!("the typing worker's answer is malformed")),
            };
            worker.used = Instant::now();
            self.idle
                .lock()
                .expect("typing workers")
                .insert(device.to_owned(), worker);
            return outcome;
        }
        Outcome::Unavailable(last_error)
    }

    /// A new worker that said it is ready within `limit`.
    async fn spawn(&self, limit: Duration) -> Result<Worker> {
        let mut child = Command::new(&self.cli)
            .env_remove("RIWORK_CHAT_ID")
            .env_remove("RIWORK_SHELL_ID")
            .env_remove("RIWORK_ORCHESTRATOR_SCOPE")
            .args(["shell", "keys", "--serve"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Nothing reads it while the worker runs; a full pipe must not stall it.
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("start configured Somtus CLI {}", self.cli.display()))?;
        let stdin = child.stdin.take().context("stdin")?;
        let stdout = BufReader::new(child.stdout.take().context("stdout")?);
        let mut worker = Worker {
            child,
            stdin,
            stdout,
            next: 1,
            used: Instant::now(),
        };
        let ready = timeout(limit, worker.line())
            .await
            .map_err(|_| anyhow!("the typing worker did not start in time"))??;
        if ready.get("ready") != Some(&Value::Bool(true)) {
            bail!("the typing worker did not say it is ready");
        }
        Ok(worker)
    }
}
