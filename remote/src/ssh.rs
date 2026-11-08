//! Authenticated-account stdio transport. SSH owns login, host keys and encryption;
//! the remote process exposes the existing RPC API, never a new network listener.
use crate::{
    client::{
        CallError, Command, Connection, Features, HostRecord, HostSummary, HostsFile, LinkState,
        LinkTiming, MAX_HOSTS, MAX_LABEL_CHARS, OfflineCause, Outcome, Reply, SshSummary,
        check_label,
    },
    config::{LockBusy, Storage},
    lanes::{self, Lanes},
    rpc::{CLI_START_FAILED, Rpc},
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::Read,
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{
        AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt,
        BufReader,
    },
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command as Process},
    sync::{mpsc, oneshot, watch},
    task::JoinSet,
    time::{Instant, timeout},
};

const VERSION: u8 = 1;
/// What `hosts add-ssh` runs on the host when it is not told otherwise.
pub const DEFAULT_REMOTE_BIN: &str = "somtus-remote";
const MAX_LINE: usize = crate::link::MAX_INFLATED;
const HANDSHAKE: Duration = Duration::from_secs(15);
/// How long one chunk of a frame may take to go out. A frame is written `WRITE_CHUNK`
/// bytes at a time, because this is a limit on no progress, not on a large reply over
/// a slow link: neither end reads while it writes, so two peers stuck writing to each
/// other depend on it to break the deadlock.
const WRITE_WAIT: Duration = Duration::from_secs(10);
const WRITE_CHUNK: usize = 64 * 1024;
/// How much of `ssh`'s stderr a failed dial keeps, and how many lines of it
/// `hosts add-ssh` shows.
const STDERR_TAIL: usize = 4 * 1024;
#[cfg(unix)]
const STDERR_LINES: usize = 20;
/// After a failed hello: how long what the far side printed may take to arrive.
const FAILED_HELLO_WAIT: Duration = Duration::from_secs(1);
/// After a dial has failed: how long the process gets to exit, so that its status
/// can be read, and then how long its stderr gets to end. Something else can hold
/// stderr open after `ssh` is gone (a `ControlPersist` master does).
const REAP_WAIT: Duration = Duration::from_secs(1);
const STDERR_DRAIN_WAIT: Duration = Duration::from_millis(500);
/// How long a forwarded session waits for an earlier one of its principal to finish.
/// It stays well inside the client's `HANDSHAKE`.
const FORWARDED_LOCK_WAIT: Duration = Duration::from_secs(5);
/// How long after its hello a session may still be waiting for the CLI to say what it can
/// do. A first run after an update can take seconds, and a session that started without
/// the answer has no chats, tabs or native features for as long as its link lasts.
const CLI_QUESTION_WAIT: Duration = Duration::from_secs(10);
/// How long after its hello the questions of `Rpc::phone_features` may still be asked: they
/// get what the handshake's own question left of this, and none is asked once it has passed.
/// So a CLI that never answers holds `ready` back this long, and the client's `HANDSHAKE`,
/// which started before `ssh` connected and logged in (that can take seconds), covers it
/// with seconds to spare.
const FEATURES_BY: Duration = Duration::from_secs(11);

/// What a session does when it does not hear from its client; tests shorten these.
#[derive(Clone, Copy)]
struct IdleLimits {
    /// How long a session goes without a frame. A forwarded session holds its principal's
    /// lock, so a gateway that lost the path must not keep a reconnect out for long; it
    /// stays well above the gateway's ping interval.
    direct: Duration,
    forwarded: Duration,
    /// How often the viewport lease is renewed, and the session looks at what it only
    /// notices on a timer: a frame still growing, and the identity file.
    renew: Duration,
    /// How long after the last request a forwarded session still renews it, as a first
    /// hop does (`connector::Timing::mobile_active`): the gateway's pings keep the link
    /// up and say nothing about whether a phone is looking at the terminal.
    renewal_window: Duration,
}
const IDLE_LIMITS: IdleLimits = IdleLimits {
    direct: Duration::from_secs(60),
    forwarded: Duration::from_secs(25),
    renew: Duration::from_secs(3),
    renewal_window: Duration::from_secs(20),
};

/// Why a dial ended before `ready`. Nothing here holds what the far side printed:
/// that can carry paths or credentials, so it picks a variant and goes no further.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DialFailure {
    IdentityChanged,
    IdentityUnavailable,
    CliUnsupported,
    CliMissing,
    HomeUnavailable,
    SessionBusy,
    AuthFailed,
    HostKey,
    NameResolution,
    Unreachable,
    NotInstalled,
    LoginOutput,
    HandshakeTimeout,
    SshNotStarted,
    NoForwarding,
    Closed,
}
impl DialFailure {
    /// The `cause` of a status line. The first six are also the `code`s of a
    /// `refused` frame.
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::IdentityChanged => "identity_changed",
            Self::IdentityUnavailable => "identity_unavailable",
            Self::CliUnsupported => "cli_unsupported",
            Self::CliMissing => "cli_missing",
            Self::HomeUnavailable => "home_unavailable",
            Self::SessionBusy => "session_busy",
            Self::AuthFailed => "auth_failed",
            Self::HostKey => "host_key",
            Self::NameResolution => "name_resolution",
            Self::Unreachable => "unreachable",
            Self::NotInstalled => "not_installed",
            Self::LoginOutput => "login_output",
            Self::HandshakeTimeout => "handshake_timeout",
            Self::SshNotStarted => "ssh_not_started",
            Self::NoForwarding => "no_forwarding",
            Self::Closed => "closed",
        }
    }
    /// What the person reads, with the remedy. Short enough that `log_safe` keeps
    /// all of it.
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::IdentityChanged => {
                "SSH host identity changed: this address now reaches another Somtus installation, so requests are refused. If that is intended, remove the route and add it again."
            }
            Self::IdentityUnavailable => {
                "the host cannot report its Somtus identity, or its saved identity is damaged. Check that Somtus runs there, or restore host-identity from a backup."
            }
            Self::CliUnsupported => {
                "the Somtus installed on the host is too old to report Hosts.local. Update Somtus on the host."
            }
            Self::CliMissing => {
                "somtus-remote on the host cannot start the Somtus CLI. Give the route --somtus with the CLI's absolute path."
            }
            Self::HomeUnavailable => {
                "the Somtus state directory on the host cannot be used. Check that this account can write to it, and the route's --home."
            }
            Self::SessionBusy => {
                "an earlier session of this client is still finishing on the host. Try again in a moment."
            }
            Self::AuthFailed => {
                "SSH login was refused. Check the account and the key, and that `ssh -T -o BatchMode=yes` to this target works without a password prompt."
            }
            Self::HostKey => {
                "SSH does not trust this host's key (unknown or changed). Check the key, then connect once with ssh in a terminal; Somtus never accepts host keys."
            }
            Self::NameResolution => {
                "SSH could not resolve the host name. Check the target and this computer's DNS or VPN."
            }
            Self::Unreachable => {
                "SSH could not reach the host (refused, timed out or no route). Check that it is awake and on the network."
            }
            Self::NotInstalled => {
                "somtus-remote could not run on the host. Install or update Somtus there, or give the route --remote-bin with its absolute path."
            }
            Self::LoginOutput => {
                "the account's shell startup files print text before Somtus starts. Silence them for noninteractive logins (for example .zshenv or .bashrc)."
            }
            Self::HandshakeTimeout => {
                "SSH handshake timed out; check the installed Somtus paths and SSH login"
            }
            Self::SshNotStarted => {
                "could not start ssh on this computer. Check that OpenSSH is installed and on PATH."
            }
            Self::NoForwarding => "the SSH host must be updated to support forwarded sessions",
            Self::Closed => {
                "SSH closed before ready; check ssh login, known_hosts and remote executables"
            }
        }
    }
    /// Whether retrying cannot help until a person acts.
    pub(crate) fn needs_action(self) -> bool {
        match self {
            Self::IdentityChanged
            | Self::IdentityUnavailable
            | Self::CliUnsupported
            | Self::CliMissing
            | Self::HomeUnavailable
            | Self::AuthFailed
            | Self::HostKey
            | Self::NotInstalled
            | Self::LoginOutput
            | Self::SshNotStarted
            | Self::NoForwarding => true,
            Self::SessionBusy
            | Self::NameResolution
            | Self::Unreachable
            | Self::HandshakeTimeout
            | Self::Closed => false,
        }
    }
    /// The code a phone is given for a failed `hosts.open`.
    pub(crate) fn gateway_code(self) -> &'static str {
        match self {
            Self::IdentityChanged => "host_changed",
            Self::SessionBusy => "host_busy",
            Self::AuthFailed | Self::HostKey => "host_login_failed",
            Self::NameResolution | Self::Unreachable => "host_unreachable",
            Self::NotInstalled | Self::CliMissing | Self::CliUnsupported => "host_not_installed",
            Self::HandshakeTimeout => "host_timeout",
            Self::NoForwarding => "host_unsupported",
            Self::IdentityUnavailable
            | Self::HomeUnavailable
            | Self::LoginOutput
            | Self::SshNotStarted
            | Self::Closed => "host_unavailable",
        }
    }
    pub(crate) fn cause(self) -> OfflineCause {
        OfflineCause {
            code: self.code(),
            needs_action: self.needs_action(),
        }
    }
    /// A code from a `refused` frame. One this build does not know is a closed
    /// connection, which is what an older client would make of it too.
    fn from_refused(code: &str) -> Self {
        [
            Self::IdentityChanged,
            Self::IdentityUnavailable,
            Self::CliUnsupported,
            Self::CliMissing,
            Self::HomeUnavailable,
            Self::SessionBusy,
        ]
        .into_iter()
        .find(|known| known.code() == code)
        .unwrap_or(Self::Closed)
    }
}

/// A failed dial, carried through `anyhow`. `Display` is the fixed text, because
/// that reaches status lines, logs and phones. `stderr` is what the process
/// printed, kept for `print_dial_error` alone, so `Debug` leaves it out too.
pub(crate) struct DialError {
    failure: DialFailure,
    stderr: String,
}
impl std::fmt::Display for DialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.failure.message())
    }
}
impl std::fmt::Debug for DialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DialError")
            .field("failure", &self.failure)
            .field("stderr_bytes", &self.stderr.len())
            .finish()
    }
}
impl std::error::Error for DialError {}
pub(crate) fn dial_error(failure: DialFailure, stderr: String) -> anyhow::Error {
    anyhow::Error::new(DialError { failure, stderr })
}
/// The typed failure in `error`, if it has one.
pub(crate) fn dial_failure(error: &anyhow::Error) -> Option<DialFailure> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<DialError>())
        .map(|dial| dial.failure)
}
/// The last lines the failed process printed, each through `log_safe`.
#[cfg(unix)]
fn dial_output(error: &anyhow::Error) -> Vec<String> {
    let Some(dial) = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<DialError>())
    else {
        return Vec::new();
    };
    let lines: Vec<&str> = dial
        .stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let first = lines.len().saturating_sub(STDERR_LINES);
    lines[first..]
        .iter()
        .map(|line| crate::log_safe(line))
        .collect()
}
/// For a command a person is running on their own terminal: the error, then what
/// the process printed. Nothing else may print `DialError::stderr`.
#[cfg(unix)]
pub(crate) fn print_dial_error(error: &anyhow::Error) {
    eprintln!("Error: {error:#}");
    let output = dial_output(error);
    if !output.is_empty() {
        eprintln!("ssh printed:");
        for line in output {
            eprintln!("  {line}");
        }
    }
}

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
/// The rules for what a route connects with: these become arguments of `ssh`, and of a shell
/// on the far side.
fn check_connection_values(
    target: &str,
    remote_bin: &str,
    somtus: Option<&str>,
    home: Option<&str>,
) -> Result<()> {
    ensure!(
        !target.is_empty()
            && target.len() <= 512
            && !target.starts_with('-')
            && !target.chars().any(|c| c.is_whitespace() || c.is_control()),
        "invalid SSH target; use an SSH config alias or user@host"
    );
    for value in std::iter::once(remote_bin).chain(somtus).chain(home) {
        ensure!(
            !value.is_empty() && value.len() <= 4096 && !value.chars().any(char::is_control),
            "invalid remote path"
        );
    }
    Ok(())
}
impl SshSummary {
    /// How a route connects, from its entry in a registry that failed validation. Its ids
    /// and any key this build does not know have no part in that, and whatever broke them
    /// should not lose the only copy of the rest. A value that is there has to be one
    /// `hosts add-ssh` would take: a command without it would be for some other route.
    pub(crate) fn from_damaged(ssh: &Value) -> Option<Self> {
        let optional = |name: &str| match &ssh[name] {
            Value::Null => Some(None),
            Value::String(value) => Some(Some(value.clone())),
            _ => None,
        };
        let summary = Self {
            target: ssh["target"].as_str()?.to_owned(),
            remote_bin: ssh["remote_bin"].as_str()?.to_owned(),
            somtus: optional("somtus")?,
            home: optional("home")?,
        };
        check_connection_values(
            &summary.target,
            &summary.remote_bin,
            summary.somtus.as_deref(),
            summary.home.as_deref(),
        )
        .ok()?;
        Some(summary)
    }
    /// The `hosts add-ssh` command for this route, for a POSIX shell. `--option=value`,
    /// because a separate value cannot start with a hyphen, and a label can. The add mints
    /// a new id and client id, and pins whichever installation answers then.
    pub fn add_command(&self, label: Option<&str>) -> String {
        let remote_bin = (self.remote_bin != DEFAULT_REMOTE_BIN).then_some(&*self.remote_bin);
        let mut command = String::from("somtus-remote hosts add-ssh");
        for (option, value) in [
            ("--target", Some(&*self.target)),
            ("--remote-bin", remote_bin),
            ("--somtus", self.somtus.as_deref()),
            ("--home", self.home.as_deref()),
            ("--label", label),
        ] {
            if let Some(value) = value {
                command.push_str(&format!(" {option}={}", quote(value)));
            }
        }
        command
    }
}
impl Route {
    fn validate_arguments(&self) -> Result<()> {
        check_connection_values(
            &self.target,
            &self.remote_bin,
            self.somtus.as_deref(),
            self.home.as_deref(),
        )?;
        crate::crypto::uuid(&self.client_id)?;
        Ok(())
    }
    pub fn validate(&self) -> Result<()> {
        self.validate_arguments()?;
        crate::crypto::uuid(&self.host_id)?;
        Ok(())
    }
    /// How this route connects, without its `client_id`.
    pub fn summary(&self) -> SshSummary {
        SshSummary {
            target: self.target.clone(),
            remote_bin: self.remote_bin.clone(),
            somtus: self.somtus.clone(),
            home: self.home.clone(),
        }
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
    write_frame(writer, &bytes, WRITE_WAIT).await
}
/// `write_json` on a session that counts its peer's silence. Neither end reads while it
/// writes, so what this took is not time the peer said nothing, and its frames may be
/// waiting in the pipe. A peer that takes nothing fails the write itself, on `WRITE_WAIT`.
async fn write_timed<W: AsyncWrite + Unpin>(
    writer: &mut W,
    value: &Value,
    heard: &mut Instant,
) -> Result<()> {
    let started = Instant::now();
    write_json(writer, value).await?;
    *heard += started.elapsed();
    Ok(())
}
/// A flush after each chunk, because a write into tokio's stdout only copies the
/// bytes: only the flush shows that the peer is taking them.
async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    bytes: &[u8],
    wait: Duration,
) -> Result<()> {
    for chunk in bytes.chunks(WRITE_CHUNK) {
        timeout(wait, async {
            writer.write_all(chunk).await?;
            writer.flush().await
        })
        .await
        .context("stdio write timed out")??;
    }
    Ok(())
}
/// What a dial skipped ahead of its first frame, which decides whether its output can be
/// why the dial failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Skipped {
    Nothing,
    /// Whole lines, which cost nothing and stop no frame from arriving: a login that
    /// prints a line works every day, and a dial of it can still fail for another reason.
    Lines,
    /// Text with no end of line. Whatever follows it joins its line, a frame included.
    Unterminated,
}
/// The first frame of a dial. A shell startup file can print to stdout ahead of it,
/// so what is not a frame is skipped, here and only here: the bounds stay those of
/// `read_json` and the caller's timeout, and a live session stays strict. `skipped`
/// outlives a timeout around this call, and `pending` has the text it interrupted.
async fn read_handshake<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    pending: &mut Vec<u8>,
    skipped: &mut Skipped,
) -> Result<Option<Value>> {
    loop {
        match read_json(reader, pending, MAX_LINE).await {
            Ok(Some(frame)) if frame.get("v").is_some() && frame.get("type").is_some() => {
                return Ok(Some(frame));
            }
            Ok(Some(_)) => *skipped = (*skipped).max(Skipped::Lines),
            Ok(None) => return Ok(None),
            Err(error) if error.is::<serde_json::Error>() => {
                *skipped = (*skipped).max(if holds_a_frame(pending) {
                    Skipped::Unterminated
                } else {
                    Skipped::Lines
                });
                pending.clear();
            }
            // The line too long to keep, or the text a process printed as it ended.
            Err(error) => return Err(error),
        }
    }
}
/// Whether a line that is not a frame ends in one, because the text before it had no end
/// of line: what the host said can then never be read.
fn holds_a_frame(line: &[u8]) -> bool {
    [&br#""type":"ready""#[..], br#""type":"refused""#]
        .iter()
        .any(|frame| line.windows(frame.len()).any(|window| window == *frame))
}
/// Whether `text`, which has no end of line, can be the start of a frame: a `ready` that
/// the link or the handshake's timer cut off, rather than something a login printed.
fn starts_a_frame(text: &[u8]) -> bool {
    text.starts_with(b"{")
        && serde_json::from_slice::<Value>(text).map_or_else(|error| error.is_eof(), |_| true)
}

/// Why a dial ended when the far side did not say. `ssh` exits 255 for its own
/// failures and otherwise with the remote command's status. The words are OpenSSH's,
/// and those of older servers, which have no `refused` frame. Only the choice of
/// variant leaves here. Skipped output explains only an end nothing else explains,
/// and only when it can have caused it: a wrong path stays a wrong path, however chatty
/// the login, and whole lines do not bring ssh down (255) or get a process killed
/// (above 128, as a shell reports a signal).
fn classify_exit(code: Option<i32>, stderr: &str, skipped: Skipped) -> DialFailure {
    use DialFailure::*;
    let stderr = stderr.to_lowercase();
    let says = |words: &[&str]| words.iter().any(|word| stderr.contains(word));
    let failure = match code {
        // OpenSSH names the methods it tried; a line a startup file printed on the way does
        // not (`bash: x: Permission denied`).
        Some(255) if says(&["permission denied (", "too many authentication failures"]) => {
            AuthFailed
        }
        Some(255)
            if says(&[
                "host key verification failed",
                "remote host identification has changed",
                "host key is known",
            ]) =>
        {
            HostKey
        }
        Some(255)
            if says(&[
                "could not resolve hostname",
                "name or service not known",
                "nodename nor servname",
            ]) =>
        {
            NameResolution
        }
        Some(255)
            if says(&[
                "connection refused",
                "timed out",
                "no route to host",
                "network is unreachable",
                "connection closed",
                "connection reset",
            ]) =>
        {
            Unreachable
        }
        Some(255) => Closed,
        Some(126 | 127) => NotInstalled,
        _ if says(&["host identity changed"]) => IdentityChanged,
        // An older server's one sentence for every failure of `Hosts.local`, with the
        // reason after it. Most reasons pass by themselves, and none is the login's.
        _ if says(&["must support hosts.local"]) => {
            return if says(&["start configured somtus cli"]) {
                CliMissing
            } else if says(&[
                "unsupported rpc method",
                "unsupported native method",
                "encrypted response limit",
                "too large to show",
                "response exceeded limit",
            ]) {
                CliUnsupported
            } else {
                Closed
            };
        }
        _ if says(&["unrecognized subcommand"]) => NotInstalled,
        _ if says(&["timed out waiting for another remote process"]) => SessionBusy,
        _ if says(&["pass --somtus"]) => CliMissing,
        _ => Closed,
    };
    let output_can_be_why = match skipped {
        Skipped::Nothing => false,
        Skipped::Lines => code.is_some_and(|code| code <= 128),
        Skipped::Unterminated => true,
    };
    if failure == Closed && output_can_be_why {
        LoginOutput
    } else {
        failure
    }
}
/// The end of `stderr`, in memory only, read until it ends so that `ssh` never
/// blocks on a full pipe. It can hold remote output, paths or credentials, so it is
/// only ever matched against fixed words or shown to the person who asked for a dial
/// (`DialError`).
async fn keep_stderr_tail(mut stderr: ChildStderr, tail: Arc<Mutex<Vec<u8>>>) {
    let mut buffer = [0; 1024];
    while let Ok(read) = stderr.read(&mut buffer).await
        && read > 0
    {
        let mut tail = tail.lock().expect("stderr tail");
        tail.extend_from_slice(&buffer[..read]);
        let over = tail.len().saturating_sub(STDERR_TAIL);
        tail.drain(..over);
    }
}

struct Dialed {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    pending: Vec<u8>,
    stderr: tokio::task::JoinHandle<()>,
    stderr_tail: Arc<Mutex<Vec<u8>>>,
    /// `connected`'s `heard`, for whoever holds the link.
    last_heard: Arc<Mutex<Instant>>,
}
impl Dialed {
    fn publish_heard(&self, heard: Instant) {
        *self.last_heard.lock().expect("last heard") = heard;
    }
    /// The error for a dial that went no further: `known`, or else what the process's
    /// exit says. It is given a moment to exit, then its stderr a moment to end.
    async fn failed(&mut self, known: Option<DialFailure>, skipped: Skipped) -> anyhow::Error {
        let status = timeout(REAP_WAIT, self.child.wait()).await;
        let code = status.ok().and_then(Result::ok).and_then(|s| s.code());
        let _ = timeout(STDERR_DRAIN_WAIT, &mut self.stderr).await;
        let stderr =
            String::from_utf8_lossy(&self.stderr_tail.lock().expect("stderr tail")).into_owned();
        let failure = known.unwrap_or_else(|| classify_exit(code, &stderr, skipped));
        dial_error(failure, stderr)
    }
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
    command: Process,
) -> Result<(Dialed, Value)> {
    dial_command_session(route, expected, command, None).await
}
async fn dial_command_session(
    route: &Route,
    expected: Option<&str>,
    command: Process,
    session: Option<&str>,
) -> Result<(Dialed, Value)> {
    dial_within(route, expected, command, session, HANDSHAKE).await
}
async fn dial_within(
    route: &Route,
    expected: Option<&str>,
    mut command: Process,
    session: Option<&str>,
    handshake: Duration,
) -> Result<(Dialed, Value)> {
    let mut child = command
        .spawn()
        .map_err(|_| dial_error(DialFailure::SshNotStarted, String::new()))?;
    let input = child.stdin.take().context("SSH stdin")?;
    let output = BufReader::new(child.stdout.take().context("SSH stdout")?);
    let stderr = child.stderr.take().context("SSH stderr")?;
    let stderr_tail = Arc::default();
    let stderr = tokio::spawn(keep_stderr_tail(stderr, Arc::clone(&stderr_tail)));
    let mut dialed = Dialed {
        child,
        input,
        output,
        pending: Vec::new(),
        stderr,
        stderr_tail,
        last_heard: Arc::new(Mutex::new(Instant::now())),
    };
    let mut hello = json!({"v":VERSION,"type":"hello","client_id":route.client_id,"expected_host_id":expected,"refused":1});
    if let Some(session) = session {
        hello["session_id"] = json!(session);
    }
    // A far side that has already left cannot take the hello, but what it printed
    // before it left is still in the pipe.
    let written = write_json(&mut dialed.input, &hello).await.is_ok();
    let wait = if written {
        handshake
    } else {
        FAILED_HELLO_WAIT
    };
    let mut skipped = Skipped::Nothing;
    let frame = timeout(
        wait,
        read_handshake(&mut dialed.output, &mut dialed.pending, &mut skipped),
    )
    .await;
    if !dialed.pending.is_empty() && !starts_a_frame(&dialed.pending) {
        skipped = Skipped::Unterminated;
    }
    match frame {
        Ok(Ok(Some(frame))) if frame["v"] == VERSION && frame["type"] == "refused" => {
            let refused = DialFailure::from_refused(frame["code"].as_str().unwrap_or_default());
            Err(dialed.failed(Some(refused), skipped).await)
        }
        Ok(Ok(Some(ready))) => match validate_ready(&ready, expected) {
            Ok(()) => Ok((dialed, ready)),
            Err(error) if skipped != Skipped::Nothing && dial_failure(&error).is_none() => {
                Err(dialed.failed(Some(DialFailure::LoginOutput), skipped).await)
            }
            Err(error) => Err(error),
        },
        // Nothing more will come from this process. Ending it also ends its stderr,
        // which is where a login that waits for a browser says where to go.
        Err(_) if written => {
            let _ = dialed.child.start_kill();
            let failure = if skipped == Skipped::Unterminated {
                DialFailure::LoginOutput
            } else {
                DialFailure::HandshakeTimeout
            };
            Err(dialed.failed(Some(failure), skipped).await)
        }
        _ => Err(dialed.failed(None, skipped).await),
    }
}
fn validate_ready(ready: &Value, expected: Option<&str>) -> Result<()> {
    ensure!(
        ready["v"] == VERSION && ready["type"] == "ready",
        "invalid SSH ready handshake"
    );
    let id = ready["host_id"]
        .as_str()
        .filter(|id| crate::crypto::uuid(id).is_ok())
        .context("SSH ready has no valid host identity")?;
    if expected.is_some_and(|expected| expected != id) {
        return Err(dial_error(DialFailure::IdentityChanged, String::new()));
    }
    Ok(())
}
/// Whether `ready` is a forwarded session's, the one asked for.
fn validate_forwarded(ready: &Value, session: &str) -> Result<()> {
    if ready["features"]["forwarded_sessions"] == 1 && ready["session_id"] == session {
        return Ok(());
    }
    Err(dial_error(DialFailure::NoForwarding, String::new()))
}
/// What a link publishes after a failed dial: fixed words, and the cause when there
/// is one. The rest of `error` carries no remote text either, but only a `DialError`
/// has words chosen for a person.
fn dial_outcome(error: &anyhow::Error) -> Outcome {
    match dial_failure(error) {
        Some(failure) => Outcome {
            failure: Some(failure),
            ..Outcome::failed(failure.message())
        },
        None => Outcome::failed(format!("SSH: {error:#}")),
    }
}

/// A label nobody gave: the target, cut to what a label may hold. A target has no
/// whitespace or control characters, so the rest of `check_label` cannot refuse it.
fn default_label(target: &str) -> String {
    target.chars().take(MAX_LABEL_CHARS).collect()
}
/// Asked only of a value being set now. In `validate_arguments` it would also run on every
/// load, and one route registered earlier with a relative home would fail the whole registry.
pub(crate) fn check_new_home(home: &str) -> Result<()> {
    ensure!(
        home.starts_with('/'),
        "--home must be an absolute path on the host; `~` and variables are not expanded there"
    );
    Ok(())
}
/// That no route but `except` connects with this target and home.
fn check_unregistered(file: &HostsFile, route: &Route, except: Option<&str>) -> Result<()> {
    let taken = file.hosts.iter().find(|host| {
        Some(host.id.as_str()) != except
            && host
                .ssh
                .as_ref()
                .is_some_and(|ssh| ssh.target == route.target && ssh.home == route.home)
    });
    match taken {
        Some(host) => bail!(
            "this SSH target/home is already registered, as {} (\"{}\")",
            host.id,
            crate::log_safe(&host.label)
        ),
        None => Ok(()),
    }
}
fn check_room(file: &HostsFile, route: &Route) -> Result<()> {
    ensure!(file.hosts.len() < MAX_HOSTS, "host registry is full");
    check_unregistered(file, route, None)
}
/// What a person should hear about the installation a new route reached. None of it stops
/// the add: routes that share an installation are designed for, and a copied home makes two
/// machines report one identity, which only a person can tell apart. A relay record has no
/// pinned identity to compare, so this can only see SSH routes.
fn added_warnings(
    storage: &Storage,
    file: &HostsFile,
    route: &Route,
    id: &str,
    new_home: bool,
) -> Vec<String> {
    let mut warnings = Vec::new();
    if new_home {
        let home = route
            .home
            .as_deref()
            .unwrap_or("the account's default Somtus home");
        warnings.push(format!(
            "{home} looks new, so this route may be pinned to a new, empty installation. If you meant an existing one, run `somtus-remote hosts remove {id}` and add it again with the right --home."
        ));
    }
    for other in file.hosts.iter().filter(|host| {
        host.ssh
            .as_ref()
            .is_some_and(|ssh| ssh.host_id == route.host_id)
    }) {
        warnings.push(format!(
            "route {0} (\"{1}\") already reaches this Somtus installation, or a copy of its home, so the two show up as one host with separate tabs and drafts. If you meant to change how that route connects, run `somtus-remote hosts remove {id}` and then `somtus-remote hosts update-ssh {0}`.",
            other.id,
            crate::log_safe(&other.label),
        ));
    }
    // Read only: this machine's identity is the engine's to create, and unknown here if
    // it is missing or damaged.
    if saved_identity(storage) == SavedIdentity::Valid(route.host_id.clone()) {
        warnings.push(format!(
            "this route reaches this computer's own Somtus installation, or a copy of its home, so its sessions also show up under this computer. If you meant another machine, run `somtus-remote hosts remove {id}`: `somtus-remote hosts update-ssh` only ever points a route at the same installation."
        ));
    }
    warnings
}

/// The result of `hosts add-ssh`.
#[derive(Debug)]
pub struct AddedRoute {
    pub host: HostSummary,
    /// For stderr: under `--json` stdout is one JSON value.
    pub warnings: Vec<String>,
}
pub async fn add(storage: &Storage, mut route: Route, label: Option<String>) -> Result<AddedRoute> {
    // A dial takes up to 15 s and leaves a marker on the target, so whatever can be refused
    // without one is. These look at the registry without its lock, which a dial must not
    // hold: the checks that count are the ones after it.
    route.validate_arguments()?;
    if let Some(home) = &route.home {
        check_new_home(home)?;
    }
    let label = label.unwrap_or_else(|| default_label(&route.target));
    check_label(&label)?;
    check_room(&HostsFile::load(storage)?, &route)?;
    let (dialed, ready) = dial(&route, None).await?;
    route.host_id = ready["host_id"]
        .as_str()
        .context("host identity")?
        .to_owned();
    let new_home = ready["new_home"] == true;
    drop(dialed);
    let record = HostRecord {
        id: uuid::Uuid::new_v4().to_string(),
        label,
        pairing: None,
        ssh: Some(route.clone()),
        relay: String::new(),
        route: String::new(),
        added_at_unix: crate::client::now_unix(),
        allow_insecure_loopback: false,
    };
    record.validate()?;
    let _lock = HostsFile::lock(storage)?;
    let mut file = HostsFile::load(storage)?;
    check_room(&file, &route)?;
    let warnings = added_warnings(storage, &file, &route, &record.id, new_home);
    let host = record.summary();
    file.hosts.push(record);
    file.save(storage)?;
    Ok(AddedRoute { host, warnings })
}

/// What `hosts update-ssh` was asked to change. `None` leaves a value as it is; an inner
/// `None` takes the option off the route.
#[derive(Default)]
pub struct RouteChange {
    pub target: Option<String>,
    pub remote_bin: Option<String>,
    pub somtus: Option<Option<String>>,
    pub home: Option<Option<String>>,
    pub label: Option<String>,
}
impl RouteChange {
    fn is_empty(&self) -> bool {
        self.target.is_none()
            && self.remote_bin.is_none()
            && self.somtus.is_none()
            && self.home.is_none()
            && self.label.is_none()
    }
    fn applied_to(&self, route: &Route) -> Route {
        let mut changed = route.clone();
        if let Some(target) = &self.target {
            changed.target.clone_from(target);
        }
        if let Some(remote_bin) = &self.remote_bin {
            changed.remote_bin.clone_from(remote_bin);
        }
        if let Some(somtus) = &self.somtus {
            changed.somtus.clone_from(somtus);
        }
        if let Some(home) = &self.home {
            changed.home.clone_from(home);
        }
        changed
    }
}
/// The result of `hosts update-ssh`.
#[derive(Debug)]
pub struct UpdatedRoute {
    pub host: HostSummary,
    /// Whether anything had to change at all.
    pub saved: bool,
    /// Whether how the route connects changed, which its client process has to hear of.
    pub route_changed: bool,
    /// For stderr: under `--json` stdout is one JSON value.
    pub warnings: Vec<String>,
}
fn ssh_route<'a>(file: &'a HostsFile, id: &str) -> Result<&'a Route> {
    file.require(id)?.ssh.as_ref().context(
        "this host is a relay pairing, whose address comes with its pairing: `hosts update-ssh` only changes SSH routes",
    )
}
/// Changes how an SSH route connects, or its label, and keeps what hangs on the route: its
/// id (tabs, drafts, the phone's saved host), its client id (the target's ledgers) and its
/// pinned identity. New connection values are tried before they are saved, by the same dial
/// a client process makes, so both ends enforce the pin. What reaches another installation
/// is refused: that is a new route.
pub async fn update(storage: &Storage, id: &str, change: RouteChange) -> Result<UpdatedRoute> {
    crate::crypto::uuid(id).context("hosts update-ssh takes a route's full id")?;
    ensure!(
        !change.is_empty(),
        "nothing to change: give --target, --remote-bin, --somtus, --no-somtus, --home, --no-home or --label"
    );
    if let Some(label) = &change.label {
        check_label(label)?;
    }
    if let Some(Some(home)) = &change.home {
        check_new_home(home)?;
    }
    let file = HostsFile::load(storage)?;
    let tried = ssh_route(&file, id)?.clone();
    let candidate = change.applied_to(&tried);
    let route_changed = candidate != tried;
    if route_changed {
        candidate.validate_arguments()?;
        check_unregistered(&file, &candidate, Some(id))?;
        let (dialed, _) = dial(&candidate, Some(&tried.host_id)).await?;
        drop(dialed);
    }
    // The dial held no lock. All of the route has to be as it was, not only its identity:
    // these are the values the candidate was made from, and saving it over others would
    // undo another update.
    let _lock = HostsFile::lock(storage)?;
    let mut file = HostsFile::load(storage)?;
    ensure!(
        ssh_route(&file, id)? == &tried,
        "this route changed while the new values were being tried, so nothing was saved. Run the command again."
    );
    if route_changed {
        check_unregistered(&file, &candidate, Some(id))?;
    }
    let record = file
        .hosts
        .iter_mut()
        .find(|host| host.id == id)
        .context("host")?;
    let mut warnings = Vec::new();
    if change.label.is_none()
        && candidate.target != tried.target
        && record.label == default_label(&tried.target)
    {
        warnings.push(format!(
            "the label \"{}\" still names the old target. `somtus-remote hosts update-ssh {id} --label NAME` changes it.",
            crate::log_safe(&record.label)
        ));
    }
    let label = change.label.unwrap_or_else(|| record.label.clone());
    let saved = route_changed || label != record.label;
    record.ssh = Some(candidate);
    record.label = label;
    record.validate()?;
    let host = record.summary();
    if saved {
        file.save(storage)?;
    }
    Ok(UpdatedRoute {
        host,
        saved,
        route_changed,
        warnings,
    })
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
        Err(e) => return dial_outcome(&e),
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
    conn.send_replace(Some(Connection::new(
        *generation,
        Features::from_ready(&ready),
        tx,
    )));
    state.send_replace(LinkState::Online { rtt_ms: None });
    let started = Instant::now();
    let mut heard = Instant::now();
    dialed.publish_heard(heard);
    let mut heard_partial = 0;
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
                        if pending.contains_key(&id) || pending.len() >= 256 || serde_json::to_vec(&request)?.len() > crate::MAX_PLAINTEXT {
                            let _ = reply.send(Err(CallError::Invalid("request not sent: SSH capacity limit".into()))); continue;
                        }
                        pending.insert(id, (Instant::now(), reply));
                        write_timed(&mut dialed.input, &request, &mut heard).await?;
                        dialed.publish_heard(heard);
                    },
                    Some(Command::Reset(reason)) => bail!("{reason}"), None => return Ok(()),
                },
                value = read_json(&mut dialed.output, &mut dialed.pending, MAX_LINE) => {
                    let value = value?.context("SSH connection closed")?; heard = Instant::now(); heard_partial = 0;
                    dialed.publish_heard(heard);
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
                    // A large reply over a slow link is a host answering, for as long
                    // as its frame keeps growing.
                    if dialed.pending.len() > heard_partial { heard = Instant::now(); }
                    heard_partial = dialed.pending.len();
                    ensure!(heard.elapsed() < timing.silence, "SSH host stopped answering");
                    if ping_sent.is_none() { ping_sent = Some(Instant::now()); }
                    write_timed(&mut dialed.input, &json!({"v":VERSION,"type":"ping"}), &mut heard).await?;
                    dialed.publish_heard(heard);
                },
            }
        }
    }.await;
    conn.send_replace(None);
    rx.close();
    let reason = result
        .err()
        .map_or_else(|| "SSH link closed".into(), |e| format!("SSH: {e:#}"));
    let lost = CallError::Lost(reason.clone());
    for (_, (_, reply)) in pending {
        let _ = reply.send(Err(lost.clone()));
    }
    // What never left the queue was never written, a second reset in it or not.
    let unsent = CallError::Offline(reason.clone());
    while let Ok(command) = rx.try_recv() {
        if let Command::Request { reply, .. } = command {
            let _ = reply.send(Err(unsent.clone()));
        }
    }
    Outcome {
        reason: crate::log_safe(&reason),
        online_for: Some(started.elapsed()),
        host_away: false,
        failure: None,
    }
}

/// A host that stopped answering has to be found out while the phone can still be told
/// about this route alone, so a phone's links listen harder than a desktop's. The
/// target gives up on a silent gateway well after this ping (`IDLE_LIMITS`).
fn gateway_timing() -> LinkTiming {
    LinkTiming {
        ping: Duration::from_secs(5),
        silence: Duration::from_secs(20),
        ..LinkTiming::default()
    }
}

/// A dedicated, non-reconnecting link owned by a single phone session.
/// Dropping/closing it kills SSH, causing EOF and cleanup on the account server.
/// The existing desktop daemon is deliberately never shared with this client.
pub(crate) struct GatewayLink {
    connection: Connection,
    task: tokio::task::JoinHandle<Outcome>,
    closed: std::sync::atomic::AtomicBool,
    last_heard: Arc<Mutex<Instant>>,
    /// A couple of its pings: a host that answers them is never quiet for this long.
    quiet_after: Duration,
}
impl GatewayLink {
    pub async fn open(route: &Route, session: &str) -> Result<(Self, Value)> {
        route.validate()?;
        crate::crypto::uuid(session)?;
        let (dialed, ready) =
            dial_command_session(route, Some(&route.host_id), route.command(), Some(session))
                .await?;
        validate_forwarded(&ready, session)?;
        ensure!(ready["features"].is_object(), "missing SSH features");
        let features = ready["features"].clone();
        let link = Self::start(dialed, ready, gateway_timing()).await?;
        Ok((link, features))
    }
    async fn start(dialed: Dialed, ready: Value, timing: LinkTiming) -> Result<Self> {
        let last_heard = Arc::clone(&dialed.last_heard);
        let (state, _) = watch::channel(LinkState::Online { rtt_ms: None });
        let (connection, mut current) = watch::channel(None);
        let task = tokio::spawn(async move {
            connected(dialed, ready, timing, &state, &connection, &mut 0).await
        });
        struct Starting(Option<tokio::task::JoinHandle<Outcome>>);
        impl Drop for Starting {
            fn drop(&mut self) {
                if let Some(task) = &self.0 {
                    task.abort();
                }
            }
        }
        let mut starting = Starting(Some(task));
        current
            .changed()
            .await
            .context("SSH ended before session opened")?;
        let connection = current
            .borrow()
            .clone()
            .context("SSH ended before session opened")?;
        let task = starting.0.take().expect("new SSH task");
        Ok(Self {
            connection,
            task,
            closed: std::sync::atomic::AtomicBool::new(false),
            last_heard,
            quiet_after: timing.ping * 2,
        })
    }
    /// A link to `script`, which takes a dial's place and has to answer it. A host silent
    /// for a test's short while reads as quiet only with a `ping` to match.
    #[cfg(test)]
    pub async fn to_script(route: &Route, script: &str, ping: Duration) -> Self {
        let mut process = Process::new("/bin/sh");
        process
            .arg("-c")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let (dialed, ready) = dial_command(route, Some(&route.host_id), process)
            .await
            .unwrap();
        let timing = LinkTiming {
            ping,
            silence: Duration::from_secs(60),
            ..gateway_timing()
        };
        Self::start(dialed, ready, timing).await.unwrap()
    }
    /// Whether the host has been heard from lately: a reply, a pong or a frame still
    /// growing. What a request that has gone unanswered can then be blamed on is the
    /// request, not the link.
    pub fn heard_lately(&self) -> bool {
        self.last_heard.lock().expect("last heard").elapsed() < self.quiet_after
    }
    pub fn is_open(&self) -> bool {
        !self.closed.load(std::sync::atomic::Ordering::Acquire) && !self.task.is_finished()
    }
    pub fn close(&self) {
        self.closed
            .store(true, std::sync::atomic::Ordering::Release);
        self.task.abort();
    }
    pub async fn call(
        &self,
        id: String,
        method: String,
        params: Value,
    ) -> Result<Value, CallError> {
        if !self.is_open() {
            return Err(CallError::Offline("host session closed".into()));
        }
        let (reply, rx) = oneshot::channel();
        self.connection
            .tx
            .try_send(Command::Request {
                id,
                method,
                params,
                reply,
            })
            .map_err(|_| CallError::Invalid("request not sent: host session unavailable".into()))?;
        match timeout(Duration::from_secs(120), rx).await {
            Ok(Ok(Ok(reply))) => Ok(reply.response),
            Ok(Ok(Err(error))) => Err(error),
            Ok(Err(_)) => Err(CallError::Lost("host session ended".into())),
            Err(_) => Err(CallError::Timeout),
        }
    }
}
impl Drop for GatewayLink {
    fn drop(&mut self) {
        self.close();
    }
}

#[derive(Debug, PartialEq, Eq)]
enum SavedIdentity {
    Valid(String),
    Missing,
    Corrupt,
}
/// `$RIWORK_HOME/host-identity`, only ever read: the mint lives in the engine's
/// `host_sessions::identity`, under its lock, and this crate does not depend on it. The
/// same bounded read and trim as the CLI's, and stricter about the text: the CLI takes
/// any form of a UUID and this only the canonical one, because a pin is compared as text.
/// A file that exists and cannot be read is not missing, so that nothing gets to
/// mint over it. Only a regular file is read, and opening one does not wait: a client looks
/// while it holds the registry lock, and a session's loop stands still while it looks, so
/// a FIFO here must not stop either for want of a writer.
fn saved_identity(storage: &Storage) -> SavedIdentity {
    let Some(home) = storage.dir.parent() else {
        return SavedIdentity::Missing;
    };
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let mut text = Vec::new();
    let read = options.open(home.join("host-identity")).and_then(|file| {
        if !file.metadata()?.is_file() {
            return Err(std::io::ErrorKind::InvalidInput.into());
        }
        file.take(129).read_to_end(&mut text)
    });
    match read {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => SavedIdentity::Missing,
        Err(_) => SavedIdentity::Corrupt,
        Ok(_) => match std::str::from_utf8(&text).map(str::trim) {
            Ok(id) if crate::crypto::uuid(id).is_ok() => SavedIdentity::Valid(id.to_owned()),
            _ => SavedIdentity::Corrupt,
        },
    }
}
/// The identity this session speaks for, without building the host's inventory when
/// a saved one can answer: a large inventory, a missing tmux or an unreadable store
/// says nothing about which installation this is. With nothing saved yet `Hosts.local`
/// is what mints it, under the session's reply limit so that a large inventory fits.
async fn identity_to_announce(
    rpc: &Rpc,
    client: &str,
    storage: &Storage,
) -> std::result::Result<String, (DialFailure, anyhow::Error)> {
    match saved_identity(storage) {
        SavedIdentity::Valid(id) => return Ok(id),
        // Never replaced: this may be the only record of which installation this is.
        SavedIdentity::Corrupt => {
            return Err((
                DialFailure::IdentityUnavailable,
                anyhow!("invalid saved host identity; restore host-identity from backup"),
            ));
        }
        SavedIdentity::Missing => {}
    }
    let request = json!({"v":1,"type":"request","id":uuid::Uuid::new_v4().to_string(),"method":"native.invoke","params":{"module":"Hosts","method":"local","args":{}}});
    let no_viewport = tokio::sync::Mutex::new(None);
    let answer = rpc
        .handle_session_up_to(client, request, &no_viewport, None, MAX_LINE)
        .await;
    let failure = match &answer {
        Ok(reply) if reply["ok"] == true => {
            let id = reply["result"]["value"]["hostId"].as_str();
            match id.filter(|id| crate::crypto::uuid(id).is_ok()) {
                Some(id) => return Ok(id.to_owned()),
                None => (
                    DialFailure::IdentityUnavailable,
                    anyhow!("Hosts.local did not report identity"),
                ),
            }
        }
        Ok(reply)
            if reply["error"]["message"]
                .as_str()
                .is_some_and(|message| message.starts_with(CLI_START_FAILED)) =>
        {
            (DialFailure::CliMissing, cli_start_error(rpc))
        }
        Ok(reply) => {
            let message = reply["error"]["message"]
                .as_str()
                .unwrap_or("invalid response");
            let unsupported = reply["error"]["code"] == "invalid_request"
                && (message.contains("unsupported RPC method")
                    || message.contains("unsupported native method"));
            (
                if unsupported {
                    DialFailure::CliUnsupported
                } else {
                    DialFailure::IdentityUnavailable
                },
                anyhow!(
                    "installed Somtus must support Hosts.local for stable SSH identity: {message}"
                ),
            )
        }
        Err(error) => (
            DialFailure::IdentityUnavailable,
            anyhow!("Hosts.local failed: {error:#}"),
        ),
    };
    // `local()` mints the identity before it can fail on tmux or the store.
    match saved_identity(storage) {
        SavedIdentity::Valid(id) => Ok(id),
        _ => Err(failure),
    }
}
fn cli_start_error(rpc: &Rpc) -> anyhow::Error {
    anyhow!(
        "cannot start the Somtus CLI {}; pass --somtus /absolute/path/to/somtus",
        rpc.cli.display()
    )
}
/// The line that tells a client why this session will not start. It is only a hint:
/// the pin holds without it, and a client that misses it still sees the session end.
fn refused_frame(failure: DialFailure) -> Value {
    json!({"v":VERSION,"type":"refused","code":failure.code()})
}
/// Only to a client whose hello asked: an older one reads the line as a broken handshake,
/// and says that instead of the hint it gives for a session that just ends. What `connect`
/// refuses before it has read a hello cannot know that (`print_refusal`).
async fn refuse<W: AsyncWrite + Unpin>(output: &mut W, hello: &Value, failure: DialFailure) {
    if hello["refused"] == 1 {
        let _ = write_json(output, &refused_frame(failure)).await;
    }
}
fn print_refusal(failure: DialFailure) {
    use std::io::Write;
    let mut stdout = std::io::stdout();
    let _ = writeln!(stdout, "{}", refused_frame(failure));
    let _ = stdout.flush();
}
/// For `connect` when it never got as far as `serve`.
pub fn refuse_cli_missing() {
    print_refusal(DialFailure::CliMissing);
}
/// For `connect` when it never got as far as `serve`.
pub fn refuse_home_unavailable() {
    print_refusal(DialFailure::HomeUnavailable);
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
    output: impl AsyncWrite + Unpin,
) -> Result<()> {
    serve_limited(storage, cli, input, output, IDLE_LIMITS).await
}
async fn serve_limited(
    storage: Storage,
    cli: PathBuf,
    input: impl AsyncRead + Unpin,
    mut output: impl AsyncWrite + Unpin,
    idle: IdleLimits,
) -> Result<()> {
    // Looked at first: `Hosts.local` below mints the identity. A host that never had a
    // project writes no `state.json`, and can still hold terminals and chats.
    let new_home = storage.dir.parent().is_some_and(|home| {
        !["state.json", "host-identity", "sessions.json", "chats"]
            .iter()
            .any(|name| home.join(name).exists())
    });
    let mut input = BufReader::new(input);
    let mut partial = Vec::new();
    let hello = timeout(HANDSHAKE, read_json(&mut input, &mut partial, 16 * 1024))
        .await??
        .context("missing SSH hello")?;
    let heard_hello = Instant::now();
    let cli_answers_by = heard_hello + CLI_QUESTION_WAIT;
    ensure!(
        hello["v"] == VERSION && hello["type"] == "hello",
        "invalid SSH hello"
    );
    let client = hello["client_id"]
        .as_str()
        .context("missing client id")?
        .to_owned();
    crate::crypto::uuid(&client)?;
    let forwarded_session = hello
        .get("session_id")
        .map(|value| {
            let session = value.as_str().context("invalid session id")?;
            crate::crypto::uuid(session)?;
            Ok::<_, anyhow::Error>(session.to_owned())
        })
        .transpose()?;
    // One stable phone/route principal owns one account session at a time.
    // This also serializes its existing upload and input ledgers across process
    // boundaries when a previous connection is still finishing accepted work.
    let _account_lock = if forwarded_session.is_some() {
        let (storage, client) = (storage.clone(), client.clone());
        let locked = tokio::task::spawn_blocking(move || {
            storage.lock_wait(&format!("ssh-forwarded-{client}.lock"), FORWARDED_LOCK_WAIT)
        })
        .await?;
        match locked {
            Ok(lock) => Some(lock),
            Err(error) => {
                let failure = if error.is::<LockBusy>() {
                    DialFailure::SessionBusy
                } else {
                    DialFailure::HomeUnavailable
                };
                refuse(&mut output, &hello, failure).await;
                return Err(error);
            }
        }
    } else {
        None
    };
    let rpc = Arc::new(Rpc::for_account_session(
        cli,
        storage.clone(),
        client.clone(),
    )?);
    let host_id = match identity_to_announce(&rpc, &client, &storage).await {
        Ok(host_id) => host_id,
        Err((failure, error)) => {
            refuse(&mut output, &hello, failure).await;
            return Err(error);
        }
    };
    if hello["expected_host_id"]
        .as_str()
        .is_some_and(|expected| expected != host_id)
    {
        refuse(&mut output, &hello, DialFailure::IdentityChanged).await;
        bail!("SSH host identity changed");
    }
    // After the comparison, so a changed host need not run anything to be refused. A
    // saved identity does not need the CLI and `phone_features` takes any failure for
    // a no, so a mistyped path would otherwise look like a healthy host.
    if rpc
        .cli_cannot_start(cli_answers_by.saturating_duration_since(Instant::now()))
        .await
    {
        refuse(&mut output, &hello, DialFailure::CliMissing).await;
        return Err(cli_start_error(&rpc));
    }
    // Upload sweeping may coexist with the relay connector. This is an owner
    // marker only; it grants no relay authorization and contains no credentials.
    // Uploads belong to the stable phone/route principal, so reconnect can
    // resume partial files and completed paths survive terminal paste/chat use.
    if let Err(error) = crate::config::private_write(
        &storage.dir.join(format!("ssh-client-{client}.json")),
        &json!({"v":1,"host_id":host_id}),
    ) {
        refuse(&mut output, &hello, DialFailure::HomeUnavailable).await;
        return Err(error);
    }
    let mut features = rpc
        .phone_features(Some((heard_hello + FEATURES_BY).into_std()))
        .await;
    // Stdio uses JSON lines; deflate belongs only to the encrypted first hop.
    features
        .as_object_mut()
        .expect("phone features")
        .remove("deflate");
    features["forwarded_sessions"] = json!(1);
    if forwarded_session.is_none() {
        features["pty"] = crate::pty::features();
    }
    let mut ready = json!({"v":VERSION,"type":"ready","host_id":host_id,"session_id":forwarded_session,"features":features});
    if new_home {
        ready["new_home"] = json!(true);
    }
    write_json(&mut output, &ready).await?;
    let viewport = Arc::new(tokio::sync::Mutex::new(Some(
        crate::viewport::Viewport::new(rpc.cli.clone(), client.clone()),
    )));
    let pty = crate::pty::PtySet::new();
    let (ended, _) = watch::channel(false);
    let mut lanes: Lanes<Value> = Lanes::default();
    let mut jobs = JoinSet::new();
    let mut renewing = JoinSet::new();
    let mut tick = tokio::time::interval(idle.renew);
    let mut heard = Instant::now();
    let mut heard_partial = 0;
    let (request_heard, last_request) = watch::channel(Instant::now());
    let idle_limit = if forwarded_session.is_some() {
        idle.forwarded
    } else {
        idle.direct
    };
    let streams_allowed = forwarded_session.is_none();
    // Not an arm of the loop below, which starts nothing while it writes: a long reply
    // would otherwise let the lease lapse under a live terminal.
    renewing.spawn({
        let (rpc, viewport) = (rpc.clone(), viewport.clone());
        let forwarded = forwarded_session.is_some();
        async move {
            let mut renew = tokio::time::interval(idle.renew);
            renew.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                renew.tick().await;
                let looking = !forwarded || last_request.borrow().elapsed() <= idle.renewal_window;
                if looking
                    && let Ok(mut held) = viewport.clone().try_lock_owned()
                    && let Some(viewport) = held.as_mut()
                    && viewport.selected.is_some()
                {
                    rpc.renew_lease(viewport).await;
                }
            }
        }
    });
    let result: Result<()> = async {
        loop {
            while let Some((lane, request)) = lanes.next_ready() {
                let (rpc, client, viewport, pty) = (rpc.clone(), client.clone(), viewport.clone(), pty.clone());
                let mut ended = ended.subscribe();
                // Upload store work runs on blocking threads. Let accepted writes
                // finish before releasing this principal's process lock, even if
                // the caller disappeared while its disk operation was running.
                let cancellable = lane.cancellable()
                    && !request["method"].as_str().is_some_and(|method| method.starts_with("upload."));
                jobs.spawn(async move {
                    let run = rpc.handle_session_up_to(&client, request, &viewport, streams_allowed.then_some(&pty), MAX_LINE);
                    let response = if cancellable {
                        tokio::select! { result = run => Some(result), _ = ended.changed() => None }
                    } else { Some(run.await) };
                    (lane, response)
                });
            }
            let reading = lanes.queued() < lanes::MAX_QUEUED;
            let idle_at = heard + idle_limit;
            tokio::select! {
                biased;
                _ = tick.tick() => {
                    // Noted as it happens, not only at the deadline, so that a frame
                    // that stalled is given the limit once, not twice.
                    if partial.len() > heard_partial { heard = Instant::now(); }
                    heard_partial = partial.len();
                    // The home can be replaced under a running session, and requests
                    // would then reach the new installation. Ending it hands the client
                    // to the handshake, which refuses. A file that cannot be read says
                    // nothing about which installation this is, so healthy terminals
                    // go on.
                    if let SavedIdentity::Valid(current) = saved_identity(&storage) {
                        ensure!(current == host_id, "SSH host identity changed");
                    }
                },
                Some(done) = renewing.join_next(), if !renewing.is_empty() => { done?; },
                done = jobs.join_next(), if !jobs.is_empty() => {
                    let (lane, response) = done.context("request task ended")??; lanes.finished(lane);
                    if let Some(response) = response { write_timed(&mut output, &response?, &mut heard).await?; }
                },
                frame = read_json(&mut input, &mut partial, crate::MAX_PLAINTEXT), if reading => {
                    let Some(frame) = frame? else { return Ok(()) };
                    heard = Instant::now(); heard_partial = 0;
                    if frame["v"] == VERSION && frame["type"] == "ping" { write_timed(&mut output, &json!({"v":VERSION,"type":"pong"}), &mut heard).await?; continue; }
                    request_heard.send_replace(Instant::now());
                    if streams_allowed && matches!(frame["method"].as_str(), Some("pty.write" | "pty.resize" | "pty.close")) {
                        write_timed(&mut output, &rpc.handle_pty_inline(&client, frame, &pty)?, &mut heard).await?;
                    } else { lanes.push(lanes::classify(&frame), frame); }
                },
                // Reading comes before the deadline, so already-waiting pings
                // count. Pausing reads for our bounded queue is not peer silence.
                _ = tokio::time::sleep_until(idle_at), if reading => {
                    // A frame still growing is a client still sending.
                    if partial.len() > heard_partial { heard = Instant::now(); }
                    heard_partial = partial.len();
                    ensure!(heard.elapsed() < idle_limit, "SSH client stopped answering");
                },
            }
        }
    }.await;
    // Never start queued mutations after observing disconnect, never cancel accepted
    // ordered work halfway through, and never carry requests into a new link.
    ended.send_replace(true);
    renewing.abort_all();
    while renewing.join_next().await.is_some() {}
    while jobs.join_next().await.is_some() {}
    pty.close_all();
    if let Some(mut viewport) = viewport.lock().await.take() {
        let _ = rpc.clear_viewport(&mut viewport).await;
    }
    result
}

#[cfg(test)]
mod tests;
