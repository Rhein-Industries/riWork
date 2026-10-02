//! Other Macs' RiWork, reached through `riwork-remote` and its per-host client daemon.
//!
//! The GUI never speaks the relay protocol itself: a `riwork-remote` daemon per paired host owns
//! the encrypted relay session and exposes it on a local Unix socket as JSON lines. This module
//! is the blocking std client for that socket, plus thin wrappers around the `riwork-remote`
//! subcommands that manage the host registry and mint pairing links. Everything here blocks, so
//! callers run it on a background executor, never on the UI thread.

use std::{
    collections::HashMap,
    ffi::OsString,
    fs::{self, DirBuilder},
    io::{self, Read, Write},
    os::unix::{fs::DirBuilderExt, net::UnixStream},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde_json::{Value, json};

/// How long an ordinary `riwork-remote` subcommand may run before it is killed.
const CLI_TIMEOUT: Duration = Duration::from_secs(20);
/// `client ensure` may have to start a daemon and wait for its relay session.
const ENSURE_TIMEOUT: Duration = Duration::from_secs(30);
/// `pair` talks to the relay to register the route.
const PAIR_TIMEOUT: Duration = Duration::from_secs(30);
/// How often a running subcommand is checked for exit.
const CLI_POLL: Duration = Duration::from_millis(5);
/// How long to wait for a subcommand's pipes to reach EOF once it has exited.
const PIPE_GRACE: Duration = Duration::from_millis(500);
/// The most output kept per pipe. A subcommand that talks more is still drained, not stalled.
const PIPE_CAP: usize = 1 << 20;
/// Slack on top of a call's own timeout so that the daemon's timeout answer normally arrives
/// before the local deadline fires.
const REPLY_GRACE: Duration = Duration::from_secs(5);
/// A status query is answered from the daemon's memory; anything slower means it is wedged.
const STATUS_WAIT: Duration = Duration::from_secs(5);
/// The daemon reads requests immediately, so a write that blocks this long has no reader.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// The longest daemon line accepted. Replies are bounded by the relay's 2 MiB inflate limit, so
/// this only stops a broken peer from growing the buffer without end.
const MAX_LINE: usize = 16 << 20;
/// The longest error text handed to the UI.
const MAX_ERROR_CHARS: usize = 300;
/// Pairing links start with this; anything containing it is treated as a secret.
const LINK_SCHEME: &str = "riwork://";
const LINK_PREFIX: &str = "riwork://pair?";
const HIDDEN_LINK: &str = "[pairing link hidden]";

/// A paired host as `riwork-remote hosts list --json` reports it. Pairing secrets in the
/// output are never read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Host {
    pub id: String,
    pub label: String,
}

/// The daemon's view of its relay connection to one host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkState {
    Connecting,
    Online,
    Offline,
}

/// What `{"op":"status"}` reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostStatus {
    pub state: LinkState,
    pub rtt_ms: Option<u64>,
    pub since: Option<u64>,
    pub reason: Option<String>,
    pub label: Option<String>,
}

/// Why a daemon request produced no result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteError {
    /// The host (or the daemon) answered `ok: false`.
    Rpc { code: String, message: String },
    /// No answer within the allowed time. The request may still have run on the host.
    Timeout,
    /// The daemon could not be started or reached.
    Unreachable(String),
    /// The daemon's reply was not what the protocol allows.
    Protocol(String),
}

/// The `riwork-remote` binary.
#[derive(Clone, Debug)]
pub struct RemoteCli {
    binary: PathBuf,
    /// Upper bound on every subprocess timeout; only tests set it, so a hung fake CLI does not
    /// make them wait out the real limits.
    timeout_cap: Option<Duration>,
}

/// A one-time pairing link for another Mac to redeem. It is a secret: it has no `Debug`
/// output and is never stored.
pub struct PairingLink(String);

/// What `pair_desktop` needs to mint a link.
#[derive(Clone, Debug)]
pub struct PairRequest {
    pub name: String,
    pub relay: String,
    /// The relay's route manifest, appended to by `pair`.
    pub routes: PathBuf,
}

/// Talks to the per-host client daemons, starting them when needed.
pub struct Daemons {
    cli: RemoteCli,
    sockets: Mutex<HashMap<String, PathBuf>>,
    /// One lock per host, held while its daemon is being started. Several panes ask for the same
    /// host at once on launch; without this each would run its own `client ensure`.
    gates: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// See `REPLY_GRACE`; a field so tests can shrink it.
    grace: Duration,
    /// See `STATUS_WAIT`; a field so tests can shrink it.
    status_wait: Duration,
}

impl RemoteCli {
    /// Finds the `riwork-remote` that ships next to this executable, honoring
    /// `RIWORK_REMOTE_BIN`, the same way `riwork remote ...` forwarding does.
    pub fn locate() -> Result<Self, String> {
        let exe = std::env::current_exe()
            .map_err(|error| format!("Cannot locate RiWork executable: {error}"))?;
        let binary = crate::remote_cli::resolve(
            &exe,
            std::env::var_os("RIWORK_REMOTE_BIN").map(PathBuf::from),
        )?;
        Ok(Self::at(binary))
    }

    pub fn at(binary: PathBuf) -> Self {
        Self {
            binary,
            timeout_cap: None,
        }
    }

    pub fn binary(&self) -> &Path {
        &self.binary
    }

    /// The registered hosts, in registry order.
    pub fn hosts(&self) -> Result<Vec<Host>, String> {
        let stdout = self.run(&argv(&["hosts", "list", "--json"]), CLI_TIMEOUT, &[])?;
        parse_hosts(&stdout)
    }

    /// Redeems a pairing link from another Mac. `riwork-remote` stores the established host
    /// before returning. The link goes in on standard input (`--link -`), not on the command
    /// line where any local process could read it, and is never kept or echoed.
    pub fn add_host(&self, link: &str, label: &str) -> Result<(), String> {
        let link = link.trim();
        if link.is_empty() {
            return Err("Paste the pairing link from the other Mac first".into());
        }
        let mut args = argv(&["hosts", "add", "--link", "-"]);
        let label = label.trim();
        if !label.is_empty() {
            args.push("--label".into());
            args.push(label.into());
        }
        // The link is secret whatever it looks like, so it is scrubbed from errors even when it
        // lacks the usual scheme.
        self.run_with_input(&args, CLI_TIMEOUT, &[link], Some(link))?;
        Ok(())
    }

    /// Unregisters a host. Callers also `Daemons::forget` it so a stale socket path is dropped.
    pub fn remove_host(&self, id: &str) -> Result<(), String> {
        let id = id.trim();
        if id.is_empty() {
            return Err("No host selected".into());
        }
        let mut args = argv(&["hosts", "remove"]);
        args.push(id.into());
        self.run(&args, CLI_TIMEOUT, &[])?;
        Ok(())
    }

    /// Mints a one-time link for another Mac to control this one. `pair` also writes the new
    /// device's long-lived export to `--out`; that file is of no use here and holds secrets, so it
    /// goes into a private directory that is removed on every path out of this function.
    pub fn pair_desktop(&self, request: &PairRequest) -> Result<PairingLink, String> {
        let name = request.name.trim();
        let relay = request.relay.trim();
        if name.is_empty() {
            return Err("Give the other Mac a name first".into());
        }
        if relay.is_empty() {
            return Err("No relay URL is configured".into());
        }
        if let Some(parent) = request
            .routes
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)
                .map_err(|error| format!("Cannot create {}: {error}", parent.display()))?;
        }
        let scratch = PrivateDir::create()
            .map_err(|error| format!("Cannot create a private folder for pairing: {error}"))?;
        let export = scratch.path().join("desktop.pairing.json");

        let mut args = argv(&["pair", "--protocol", "2", "--kind", "desktop", "--name"]);
        args.push(name.into());
        args.push("--relay".into());
        args.push(relay.into());
        args.push("--out".into());
        args.push(export.into());
        args.push("--relay-routes".into());
        args.push(request.routes.clone().into());
        args.push("--show-link".into());
        if is_insecure_loopback(relay) {
            args.push("--allow-insecure-loopback".into());
        }
        let stdout = self.run(&args, PAIR_TIMEOUT, &[])?;
        stdout
            .split_whitespace()
            .find(|token| token.starts_with(LINK_PREFIX))
            .map(|link| PairingLink(link.to_owned()))
            .ok_or_else(|| "riwork-remote printed no pairing link".to_owned())
    }

    #[cfg(test)]
    fn with_timeout_cap(mut self, cap: Duration) -> Self {
        self.timeout_cap = Some(cap);
        self
    }

    /// Runs `riwork-remote args...` to completion and returns its stdout. Never hangs: the child
    /// is killed once `timeout` passes. A failure is described by the first line the child
    /// printed, with anything that looks like a pairing link (or an entry of `secrets`) hidden.
    fn run(
        &self,
        args: &[OsString],
        timeout: Duration,
        secrets: &[&str],
    ) -> Result<String, String> {
        self.run_with_input(args, timeout, secrets, None)
    }

    /// `run`, with `input` written to the child's standard input (then closed). Without input
    /// the child gets none at all.
    fn run_with_input(
        &self,
        args: &[OsString],
        timeout: Duration,
        secrets: &[&str],
        input: Option<&str>,
    ) -> Result<String, String> {
        let timeout = self.timeout_cap.map_or(timeout, |cap| cap.min(timeout));
        let mut hidden: Vec<String> = secrets.iter().map(|secret| (*secret).to_owned()).collect();
        hidden.extend(
            args.iter()
                .filter_map(|arg| arg.to_str())
                .filter(|arg| has_link_scheme(arg))
                .map(str::to_owned),
        );
        let hidden: Vec<&str> = hidden.iter().map(String::as_str).collect();

        let mut child = Command::new(&self.binary)
            .args(args)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("Cannot start {}: {error}", self.binary.display()))?;
        if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
            // Written on its own thread so a child that never reads cannot stall the wait
            // below; a child that exits first just closes the pipe.
            let input = input.to_owned();
            thread::spawn(move || {
                let _ = stdin.write_all(input.as_bytes());
                let _ = stdin.write_all(b"\n");
            });
        }
        let stdout = Drain::start(child.stdout.take());
        let stderr = Drain::start(child.stderr.take());

        let deadline = deadline_in(timeout);
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(scrub(
                        &format!("Cannot wait for riwork-remote: {error}"),
                        &hidden,
                    ));
                }
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(scrub(
                    &format!(
                        "riwork-remote {} timed out after {timeout:?}",
                        describe(args)
                    ),
                    &hidden,
                ));
            }
            thread::sleep(CLI_POLL);
        };

        let drained = deadline_in(PIPE_GRACE);
        let stdout = stdout.finish(drained);
        let stderr = stderr.finish(drained);
        if status.success() {
            Ok(stdout)
        } else {
            Err(failure_text(status, &stdout, &stderr, &hidden))
        }
    }
}

impl PairingLink {
    /// The link text, for showing or copying once. The only way to read it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

/// The argument vector of the bridge a remote tab's terminal runs, after the binary.
pub fn attach_args(host_id: &str, shell_id: &str) -> Vec<String> {
    vec![
        "attach".into(),
        "--desktop".into(),
        host_id.into(),
        "--shell".into(),
        shell_id.into(),
    ]
}

impl Daemons {
    pub fn new(cli: RemoteCli) -> Self {
        Self {
            cli,
            sockets: Mutex::default(),
            gates: Mutex::default(),
            grace: REPLY_GRACE,
            status_wait: STATUS_WAIT,
        }
    }

    #[cfg(test)]
    fn with_limits(cli: RemoteCli, grace: Duration, status_wait: Duration) -> Self {
        Self {
            grace,
            status_wait,
            ..Self::new(cli)
        }
    }

    /// Drops the cached socket path of a host, e.g. after it is removed, so the next request asks
    /// `riwork-remote` again instead of trusting a path that may be gone.
    pub fn forget(&self, host_id: &str) {
        lock(&self.sockets).remove(host_id);
    }

    /// Runs one RPC on the host through its daemon and returns the result.
    ///
    /// The request line is written once and never resent: calls such as `shell.create` are not
    /// idempotent, so after a lost connection the caller decides whether to try again. Only
    /// reaching the daemon is retried (see `connect`).
    pub fn call(
        &self,
        host_id: &str,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, RemoteError> {
        let id = next_request_id();
        let request = json!({
            "op": "call",
            "id": id,
            "method": method,
            "params": params,
            "timeout_ms": u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
        });
        let mut stream = self.connect(host_id)?;
        send(&mut stream, &request)?;
        // One deadline for the whole exchange: a daemon that trickles unrelated lines must not
        // keep extending the wait. The daemon's own timer starts when it reads the request, so
        // the grace is added from here rather than from before a possible daemon start.
        let deadline = deadline_in(timeout.saturating_add(self.grace));
        let mut lines = LineReader::new(stream);
        loop {
            let line = lines.next_reply(deadline)?;
            let mut reply: Value = serde_json::from_str(&line)
                .map_err(|error| RemoteError::Protocol(format!("unreadable reply ({error})")))?;
            if reply.get("id").and_then(Value::as_str) == Some(id.as_str()) {
                return reply_result(&mut reply);
            }
        }
    }

    /// The daemon's current view of its link to the host.
    pub fn status(&self, host_id: &str) -> Result<HostStatus, RemoteError> {
        let mut stream = self.connect(host_id)?;
        send(&mut stream, &json!({ "op": "status" }))?;
        let line = LineReader::new(stream).next_reply(deadline_in(self.status_wait))?;
        parse_status(&line)
    }

    /// Connects to the host's daemon, starting it when it is not running. A cached socket path
    /// may be stale (the daemon exits when idle), so one failed connect re-ensures the daemon and
    /// asks for its path again; a second failure is reported.
    fn connect(&self, host_id: &str) -> Result<UnixStream, RemoteError> {
        let path = self.socket_for(host_id)?;
        if let Ok(stream) = UnixStream::connect(&path) {
            return Ok(stream);
        }
        self.drop_path(host_id, &path);
        let path = self.resolve(host_id)?;
        UnixStream::connect(&path).map_err(|error| {
            self.drop_path(host_id, &path);
            RemoteError::Unreachable(format!(
                "Cannot connect to the client daemon for this Mac: {error}"
            ))
        })
    }

    /// The socket path of the host's daemon: the cached one, else a freshly ensured one.
    fn socket_for(&self, host_id: &str) -> Result<PathBuf, RemoteError> {
        match self.cached(host_id) {
            Some(path) => Ok(path),
            None => self.resolve(host_id),
        }
    }

    /// Starts the daemon (if needed) and learns its socket path, once per host at a time.
    fn resolve(&self, host_id: &str) -> Result<PathBuf, RemoteError> {
        let gate = Arc::clone(lock(&self.gates).entry(host_id.to_owned()).or_default());
        let _turn = lock(&gate);
        // Another thread may have finished while this one waited for its turn.
        if let Some(path) = self.cached(host_id) {
            return Ok(path);
        }
        let unreachable = |detail: String| {
            RemoteError::Unreachable(format!("Cannot start the connection to this Mac: {detail}"))
        };
        let desktop = |sub: &str| argv(&["client", sub, "--desktop", host_id]);
        self.cli
            .run(&desktop("ensure"), ENSURE_TIMEOUT, &[])
            .map_err(unreachable)?;
        let out = self
            .cli
            .run(&desktop("socket"), CLI_TIMEOUT, &[])
            .map_err(unreachable)?;
        let path = first_line(&out)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or_else(|| {
                unreachable("riwork-remote printed no absolute socket path".to_owned())
            })?;
        lock(&self.sockets).insert(host_id.to_owned(), path.clone());
        Ok(path)
    }

    fn cached(&self, host_id: &str) -> Option<PathBuf> {
        lock(&self.sockets).get(host_id).cloned()
    }

    /// Forgets `path` for the host unless another thread has already replaced it.
    fn drop_path(&self, host_id: &str, path: &Path) {
        let mut sockets = lock(&self.sockets);
        if sockets.get(host_id).is_some_and(|cached| cached == path) {
            sockets.remove(host_id);
        }
    }
}

impl RemoteError {
    /// The host runs a RiWork that predates the method (or the device kind may not call it).
    pub fn is_unsupported(&self) -> bool {
        matches!(self, Self::Rpc { code, message }
            if code == "invalid_request"
                && message.to_ascii_lowercase().contains("unsupported rpc method"))
    }

    /// A short sentence for the UI.
    pub fn message(&self) -> String {
        match self {
            Self::Rpc { code, message } if message.trim().is_empty() => {
                format!("The host reported an error ({code})")
            }
            Self::Rpc { message, .. } => message.clone(),
            Self::Timeout => "No answer in time".into(),
            Self::Unreachable(detail) => detail.clone(),
            Self::Protocol(detail) => format!("Unexpected answer from the client daemon: {detail}"),
        }
    }
}

impl std::fmt::Display for RemoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rpc { code, message } if !message.trim().is_empty() => {
                write!(f, "{message} ({code})")
            }
            _ => f.write_str(&self.message()),
        }
    }
}

impl std::error::Error for RemoteError {}

/// Parses one status line from the daemon.
pub fn parse_status(line: &str) -> Result<HostStatus, RemoteError> {
    let value: Value = serde_json::from_str(line)
        .map_err(|error| RemoteError::Protocol(format!("unreadable status ({error})")))?;
    let state = match value.get("state").and_then(Value::as_str) {
        Some("connecting") => LinkState::Connecting,
        Some("online") => LinkState::Online,
        Some("offline") => LinkState::Offline,
        Some(other) => {
            return Err(RemoteError::Protocol(format!(
                "unknown link state {other:?}"
            )));
        }
        None => return Err(RemoteError::Protocol("status without a state".into())),
    };
    let text = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    Ok(HostStatus {
        state,
        rtt_ms: value.get("rtt_ms").and_then(Value::as_u64),
        since: value.get("since").and_then(Value::as_u64),
        reason: text("reason"),
        label: text("label"),
    })
}

/// Collects one of a child's pipes on its own thread. Reading on the waiting thread would stall a
/// chatty child on a full pipe, and waiting for EOF would hang if the child left behind a daemon
/// that inherited the pipe, so `finish` gives up after a grace period and keeps what arrived.
struct Drain {
    bytes: Arc<Mutex<Vec<u8>>>,
    eof: mpsc::Receiver<()>,
}

impl Drain {
    fn start<R: Read + Send + 'static>(pipe: Option<R>) -> Self {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let (done, eof) = mpsc::channel();
        let sink = Arc::clone(&bytes);
        // If the thread cannot start (or there is no pipe) `done` is dropped, and `finish`
        // returns at once with nothing.
        let _ = thread::Builder::new()
            .name("riwork-remote-pipe".into())
            .spawn(move || {
                let Some(mut pipe) = pipe else { return };
                let mut chunk = [0u8; 8192];
                loop {
                    match pipe.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(read) => {
                            let mut kept = lock(&sink);
                            let room = PIPE_CAP.saturating_sub(kept.len());
                            kept.extend_from_slice(&chunk[..read.min(room)]);
                        }
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                        Err(_) => break,
                    }
                }
                let _ = done.send(());
            });
        Self { bytes, eof }
    }

    fn finish(self, deadline: Instant) -> String {
        let _ = self
            .eof
            .recv_timeout(deadline.saturating_duration_since(Instant::now()));
        String::from_utf8_lossy(&lock(&self.bytes)).into_owned()
    }
}

/// A private directory removed on drop. `pair --out` writes the new device's long-lived export
/// into it, and that must not outlive the call whatever happens.
struct PrivateDir(PathBuf);

impl PrivateDir {
    fn create() -> io::Result<Self> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let name = format!(
            "riwork-pair-{}-{nanos}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let path = std::env::temp_dir().join(name);
        // Not recursive: an existing directory is an error, never reused.
        DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for PrivateDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// What the line reader found within its wait.
enum Line {
    /// A complete, non-blank line without its terminator.
    Text(String),
    /// Nothing complete arrived before the deadline.
    Idle,
    /// The daemon closed the connection.
    Closed,
}

/// Reads daemon lines against a deadline. A partial line survives a timeout, which a
/// `BufReader::read_line` on a socket with a read timeout does not promise.
struct LineReader {
    stream: UnixStream,
    pending: Vec<u8>,
    /// How much of `pending` is already known to hold no newline.
    scanned: usize,
}

impl LineReader {
    fn new(stream: UnixStream) -> Self {
        Self {
            stream,
            pending: Vec::new(),
            scanned: 0,
        }
    }

    fn next(&mut self, deadline: Instant) -> io::Result<Line> {
        loop {
            if let Some(offset) = self.pending[self.scanned..]
                .iter()
                .position(|&b| b == b'\n')
            {
                let end = self.scanned + offset;
                let text = std::str::from_utf8(&self.pending[..end])
                    .map(|text| text.trim().to_owned())
                    .map_err(|_| invalid_data("the daemon sent text that is not UTF-8"));
                self.pending.drain(..=end);
                self.scanned = 0;
                match text? {
                    text if text.is_empty() => continue,
                    text => return Ok(Line::Text(text)),
                }
            }
            self.scanned = self.pending.len();
            if self.pending.len() > MAX_LINE {
                return Err(invalid_data("the daemon sent an oversized line"));
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(Line::Idle);
            }
            let wait = (deadline - now).max(Duration::from_millis(1));
            // macOS refuses to set a timeout on a socket whose peer has already hung up (EINVAL).
            // Such a socket cannot block: the read below returns what is left, then EOF.
            if let Err(error) = self.stream.set_read_timeout(Some(wait))
                && error.kind() != io::ErrorKind::InvalidInput
            {
                return Err(error);
            }
            let mut chunk = [0u8; 16 * 1024];
            match self.stream.read(&mut chunk) {
                Ok(0) => return Ok(Line::Closed),
                Ok(read) => self.pending.extend_from_slice(&chunk[..read]),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(error),
            }
        }
    }

    /// The next line of a one-shot exchange, where silence, a hang-up and a broken socket are all
    /// failures.
    fn next_reply(&mut self, deadline: Instant) -> Result<String, RemoteError> {
        match self.next(deadline) {
            Ok(Line::Text(text)) => Ok(text),
            Ok(Line::Idle) => Err(RemoteError::Timeout),
            Ok(Line::Closed) => Err(RemoteError::Protocol(
                "the daemon closed the connection".into(),
            )),
            Err(error) => Err(RemoteError::Protocol(format!(
                "lost the daemon connection ({error})"
            ))),
        }
    }
}

fn invalid_data(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_owned())
}

/// Sends one request line in a single write. Nothing is ever sent twice.
fn send(stream: &mut UnixStream, request: &Value) -> Result<(), RemoteError> {
    let mut line = request.to_string();
    line.push('\n');
    stream
        .set_write_timeout(Some(WRITE_TIMEOUT))
        .and_then(|()| stream.write_all(line.as_bytes()))
        .map_err(|error| {
            RemoteError::Unreachable(format!(
                "Cannot talk to the client daemon for this Mac: {error}"
            ))
        })
}

/// Maps the daemon's answer to a call. The daemon's own timeout answer is the same outcome as
/// the local deadline passing, so callers handle one `Timeout`.
fn reply_result(reply: &mut Value) -> Result<Value, RemoteError> {
    match reply.get("ok").and_then(Value::as_bool) {
        Some(true) => Ok(reply
            .get_mut("result")
            .map(Value::take)
            .unwrap_or(Value::Null)),
        Some(false) => {
            let field = |name: &str| {
                reply
                    .get("error")
                    .and_then(|error| error.get(name))
                    .and_then(Value::as_str)
            };
            let code = field("code").unwrap_or("unknown");
            if code == "timeout" {
                return Err(RemoteError::Timeout);
            }
            Err(RemoteError::Rpc {
                code: code.to_owned(),
                message: field("message").unwrap_or_default().to_owned(),
            })
        }
        None => Err(RemoteError::Protocol("a reply without an ok flag".into())),
    }
}

/// A request id that is unique within this process and, with the pid, across runs.
fn next_request_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!(
        "rw-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// `wait` from now, saturating instead of panicking on an absurd duration.
fn deadline_in(wait: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(wait)
        .unwrap_or_else(|| now + Duration::from_secs(365 * 24 * 3600))
}

/// A poisoned lock only means another thread panicked mid-update of a cache; the data is still
/// usable, and a UI that stops talking to every host over it would be worse.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn argv(parts: &[&str]) -> Vec<OsString> {
    parts.iter().map(OsString::from).collect()
}

/// The leading words of a subcommand ("hosts add"), for messages. Stops at the first flag so
/// that values such as a pairing link never get in.
fn describe(args: &[OsString]) -> String {
    args.iter()
        .take(2)
        .map(|arg| arg.to_string_lossy())
        .take_while(|arg| !arg.starts_with('-'))
        .collect::<Vec<_>>()
        .join(" ")
}

fn first_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).find(|line| !line.is_empty())
}

fn has_link_scheme(text: &str) -> bool {
    text.to_ascii_lowercase().contains(LINK_SCHEME)
}

/// Hides pairing links. A link is a one-time secret that grants control of this Mac, and child
/// output or argv echoed into an error would end up in the UI and in logs. Each `secrets` entry is
/// replaced wherever it occurs, and then every whitespace-delimited token that contains the link
/// scheme is replaced as a whole, which catches links the caller did not know about.
fn scrub(text: &str, secrets: &[&str]) -> String {
    let mut text = text.to_owned();
    for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
        text = text.replace(secret, HIDDEN_LINK);
    }
    let mut out = String::with_capacity(text.len());
    for chunk in text.split_inclusive(char::is_whitespace) {
        let token = chunk.trim_end();
        if has_link_scheme(token) {
            out.push_str(HIDDEN_LINK);
            out.push_str(&chunk[token.len()..]);
        } else {
            out.push_str(chunk);
        }
    }
    out
}

/// What to tell the user about a subcommand that exited unsuccessfully: its first line of stderr,
/// else of stdout (skipping the link `pair` prints there), else the exit code.
fn failure_text(status: ExitStatus, stdout: &str, stderr: &str, hidden: &[&str]) -> String {
    let detail = first_line(stderr)
        .or_else(|| {
            stdout
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty() && !has_link_scheme(line))
        })
        .map(str::to_owned)
        .unwrap_or_else(|| match status.code() {
            Some(code) => format!("riwork-remote exited with {code}"),
            None => "riwork-remote was stopped by a signal".to_owned(),
        });
    let detail = scrub(&detail, hidden);
    if detail.chars().count() > MAX_ERROR_CHARS {
        let mut shortened: String = detail.chars().take(MAX_ERROR_CHARS).collect();
        shortened.push_str("...");
        shortened
    } else {
        detail
    }
}

/// Reads `hosts list --json` leniently: a top-level array or `{"hosts": [...]}`, entries with a
/// string `id`, and a `label` that may be missing. Every other field, pairing secrets included,
/// is left unread.
fn parse_hosts(text: &str) -> Result<Vec<Host>, String> {
    let value: Value = serde_json::from_str(text)
        .map_err(|error| format!("riwork-remote printed an unreadable host list ({error})"))?;
    let entries = match &value {
        Value::Array(entries) => entries,
        Value::Object(map) => match map.get("hosts") {
            Some(Value::Array(entries)) => entries,
            _ => return Err("riwork-remote printed a host list without hosts".into()),
        },
        _ => return Err("riwork-remote printed an unexpected host list".into()),
    };
    Ok(entries.iter().filter_map(host_from).collect())
}

fn host_from(entry: &Value) -> Option<Host> {
    let id = entry.get("id")?.as_str().filter(|id| !id.is_empty())?;
    let label = entry
        .get("label")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map_or_else(|| id.chars().take(8).collect(), str::to_owned);
    Some(Host {
        id: id.to_owned(),
        label,
    })
}

/// Whether the relay is a plain `ws://` URL on this machine, which `pair` refuses unless told
/// that loopback is intended (the local test setup).
fn is_insecure_loopback(relay: &str) -> bool {
    let relay = relay.to_ascii_lowercase();
    let Some(rest) = relay.strip_prefix("ws://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = authority.rsplit('@').next().unwrap_or_default();
    let host = match host_port.strip_prefix('[') {
        Some(inner) => inner.split(']').next().map(|ip| format!("[{ip}]")),
        None => host_port.split(':').next().map(str::to_owned),
    };
    matches!(host.as_deref(), Some("127.0.0.1" | "localhost" | "[::1]"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader},
        os::unix::{fs::PermissionsExt, net::UnixListener},
        sync::atomic::{AtomicBool, AtomicUsize},
        thread::JoinHandle,
    };

    fn unique() -> String {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )
    }

    /// A temporary directory removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("rh-test-{}", unique()));
            fs::create_dir_all(&path).expect("create scratch dir");
            Self(path)
        }

        fn file(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }

        fn write(&self, name: &str, text: &str) {
            fs::write(self.file(name), text).expect("write scratch file");
        }

        fn read(&self, name: &str) -> String {
            fs::read_to_string(self.file(name)).unwrap_or_default()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    type Handler = dyn Fn(Value, UnixStream) + Send + Sync;

    /// A stand-in for the client daemon: accepts connections at a short socket path, reads the
    /// first request line of each, and lets `handler` play the rest of the conversation.
    struct FakeDaemon {
        path: PathBuf,
        requests: Arc<Mutex<Vec<Value>>>,
        accepted: Arc<AtomicUsize>,
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl FakeDaemon {
        fn start(handler: impl Fn(Value, UnixStream) + Send + Sync + 'static) -> Self {
            // The macOS sun_path limit is 104 bytes, so the socket cannot live in a nested
            // scratch directory.
            let path = PathBuf::from(format!("/tmp/rh-{}.sock", unique()));
            let _ = fs::remove_file(&path);
            let listener = UnixListener::bind(&path).expect("bind fake daemon socket");
            let handler: Arc<Handler> = Arc::new(handler);
            let requests = Arc::new(Mutex::new(Vec::new()));
            let accepted = Arc::new(AtomicUsize::new(0));
            let stop = Arc::new(AtomicBool::new(false));
            let thread = {
                let (requests, accepted, stop) = (
                    Arc::clone(&requests),
                    Arc::clone(&accepted),
                    Arc::clone(&stop),
                );
                thread::spawn(move || {
                    for connection in listener.incoming() {
                        if stop.load(Ordering::Acquire) {
                            break;
                        }
                        let Ok(stream) = connection else { continue };
                        accepted.fetch_add(1, Ordering::SeqCst);
                        let (handler, requests) = (Arc::clone(&handler), Arc::clone(&requests));
                        thread::spawn(move || {
                            let Ok(reader) = stream.try_clone() else {
                                return;
                            };
                            let mut line = String::new();
                            if BufReader::new(reader).read_line(&mut line).is_err() {
                                return;
                            }
                            let Ok(request) = serde_json::from_str::<Value>(&line) else {
                                return;
                            };
                            lock(&requests).push(request.clone());
                            handler(request, stream);
                        });
                    }
                })
            };
            Self {
                path,
                requests,
                accepted,
                stop,
                thread: Some(thread),
            }
        }

        fn path(&self) -> &Path {
            &self.path
        }

        fn requests(&self) -> Vec<Value> {
            lock(&self.requests).clone()
        }

        fn accepted(&self) -> usize {
            self.accepted.load(Ordering::SeqCst)
        }
    }

    impl Drop for FakeDaemon {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            let _ = UnixStream::connect(&self.path);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
            let _ = fs::remove_file(&self.path);
        }
    }

    /// Blocks until the client closes its end, so a handler can keep a connection open.
    fn hold_until_closed(mut stream: UnixStream) {
        let mut sink = [0u8; 256];
        while stream.read(&mut sink).is_ok_and(|read| read > 0) {}
    }

    fn reply(stream: &mut UnixStream, line: &str) {
        let _ = writeln!(stream, "{line}");
    }

    fn ok_reply(request: &Value, result: Value) -> String {
        json!({ "id": request["id"], "ok": true, "result": result, "server_ms": 1 }).to_string()
    }

    const FAKE_CLI: &str = r#"#!/bin/sh
DIR='@DIR@'
echo "$*" >> "$DIR/log"
if [ -e "$DIR/hang" ]; then
  echo $$ > "$DIR/pid"
  exec sleep 30
fi
case "$1" in
  client)
    case "$2" in
      ensure)
        if [ -e "$DIR/ensure_fail" ]; then
          echo "relay is down" >&2
          exit 3
        fi
        ;;
      socket) cat "$DIR/socket" ;;
    esac
    exit 0
    ;;
  hosts)
    case "$2" in
      list) cat "$DIR/hosts.json" ;;
      add)
        cat > "$DIR/stdin"
        if [ -e "$DIR/add_fail" ]; then
          echo "riwork-remote: cannot redeem: $* $(cat "$DIR/stdin")" >&2
          exit 2
        fi
        ;;
    esac
    exit 0
    ;;
  pair)
    OUT=''
    while [ $# -gt 0 ]; do
      if [ "$1" = "--out" ]; then OUT="$2"; fi
      shift
    done
    if [ -e "$OUT" ]; then
      echo "export file already exists" >&2
      exit 9
    fi
    ls -ld "$(dirname "$OUT")" | cut -c1-10 > "$DIR/outdir_mode"
    echo "long-lived secrets" > "$OUT"
    echo "Pairing ready for this Mac"
    case "$(cat "$DIR/pair_mode" 2>/dev/null)" in
      nolink) echo "no link today" ;;
      fail)
        echo "riwork://pair?v=2&data=LEAKED"
        echo "pair failed: cannot write routes" >&2
        exit 1
        ;;
      failquiet)
        echo "riwork://pair?v=2&data=LEAKED"
        exit 1
        ;;
      *)
        echo "riwork://pair?v=2&data=SECRET2"
        echo "Expires in 10 minutes"
        ;;
    esac
    exit 0
    ;;
esac
exit 0
"#;

    /// A fake `riwork-remote` that logs its arguments and behaves per files in its directory.
    struct FakeCli {
        dir: Scratch,
        cli: RemoteCli,
    }

    impl FakeCli {
        fn new(socket: &Path) -> Self {
            let dir = Scratch::new();
            dir.write("socket", &socket.display().to_string());
            dir.write("hosts.json", "[]");
            let binary = dir.file("riwork-remote");
            let script = FAKE_CLI.replace("@DIR@", &dir.0.display().to_string());
            fs::write(&binary, script).expect("write fake cli");
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o755))
                .expect("chmod fake cli");
            let cli = RemoteCli::at(binary);
            Self { dir, cli }
        }

        fn log(&self) -> Vec<String> {
            self.dir.read("log").lines().map(str::to_owned).collect()
        }

        fn count(&self, prefix: &str) -> usize {
            self.log()
                .iter()
                .filter(|line| line.starts_with(prefix))
                .count()
        }

        /// The directory `pair --out` was given, from the logged invocation.
        fn pair_dir(&self) -> PathBuf {
            let log = self.log();
            let line = log
                .iter()
                .find(|line| line.starts_with("pair "))
                .expect("pair was run");
            let mut words = line.split_whitespace();
            words.find(|word| *word == "--out");
            let out = words.next().expect("--out value");
            Path::new(out)
                .parent()
                .expect("out has a parent")
                .to_path_buf()
        }
    }

    fn echo_daemon() -> FakeDaemon {
        FakeDaemon::start(|request, mut stream| {
            let params = request["params"].clone();
            reply(&mut stream, &ok_reply(&request, json!({ "echo": params })));
        })
    }

    fn daemons(fake: &FakeCli) -> Daemons {
        Daemons::with_limits(
            fake.cli.clone(),
            Duration::from_millis(100),
            Duration::from_millis(300),
        )
    }

    /// Resolves the daemon's socket up front. macOS scans a freshly written script the first time
    /// it runs, which can take longer than the timings under test.
    fn warm(daemons: &Daemons) {
        daemons.socket_for("h1").expect("resolve the socket");
    }

    #[test]
    fn call_returns_result_with_unique_ids_and_skips_unrelated_lines() {
        let daemon = FakeDaemon::start(|request, mut stream| {
            reply(
                &mut stream,
                r#"{"id":"someone-else","ok":true,"result":{"wrong":1}}"#,
            );
            reply(&mut stream, r#"{"op":"noise"}"#);
            reply(&mut stream, "");
            let params = request["params"].clone();
            reply(&mut stream, &ok_reply(&request, json!({ "echo": params })));
        });
        let fake = FakeCli::new(daemon.path());
        let daemons = daemons(&fake);

        let first = daemons
            .call(
                "h1",
                "projects.list",
                json!({ "n": 1 }),
                Duration::from_secs(15),
            )
            .expect("first call");
        let second = daemons
            .call(
                "h1",
                "projects.list",
                json!({ "n": 2 }),
                Duration::from_secs(15),
            )
            .expect("second call");
        assert_eq!(first, json!({ "echo": { "n": 1 } }));
        assert_eq!(second, json!({ "echo": { "n": 2 } }));

        let requests = daemon.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["op"], "call");
        assert_eq!(requests[0]["method"], "projects.list");
        assert_eq!(requests[0]["timeout_ms"], 15000);
        assert_ne!(requests[0]["id"], requests[1]["id"]);
        assert!(requests[0]["id"].as_str().is_some_and(|id| !id.is_empty()));
    }

    #[test]
    fn call_maps_error_replies() {
        let daemon = FakeDaemon::start(|request, mut stream| {
            let id = request["id"].clone();
            let line = match request["method"].as_str().unwrap_or_default() {
                "missing" => json!({ "id": id, "ok": false,
                    "error": { "code": "not_found", "message": "No such shell" } }),
                "slow" => json!({ "id": id, "ok": false,
                    "error": { "code": "timeout", "message": "host did not answer" } }),
                "old" => json!({ "id": id, "ok": false,
                    "error": { "code": "invalid_request",
                               "message": "Unsupported RPC method: pty.open" } }),
                "bare" => json!({ "id": id, "ok": true }),
                "vague" => json!({ "id": id, "ok": false }),
                "noflag" => json!({ "id": id }),
                _ => return,
            };
            reply(&mut stream, &line.to_string());
        });
        let fake = FakeCli::new(daemon.path());
        let daemons = daemons(&fake);
        let call = |method: &str| daemons.call("h1", method, json!({}), Duration::from_secs(5));

        let missing = call("missing").expect_err("not_found");
        assert_eq!(
            missing,
            RemoteError::Rpc {
                code: "not_found".into(),
                message: "No such shell".into()
            }
        );
        assert_eq!(missing.message(), "No such shell");
        assert!(!missing.is_unsupported());

        assert_eq!(call("slow").err(), Some(RemoteError::Timeout));

        let old = call("old").expect_err("unsupported");
        assert!(old.is_unsupported());

        assert_eq!(call("bare").expect("ok without result"), Value::Null);
        assert_eq!(
            call("vague").err(),
            Some(RemoteError::Rpc {
                code: "unknown".into(),
                message: String::new()
            })
        );
        assert!(matches!(
            call("noflag").err(),
            Some(RemoteError::Protocol(_))
        ));
    }

    #[test]
    fn call_times_out_when_the_daemon_never_answers() {
        let daemon = FakeDaemon::start(|_request, stream| hold_until_closed(stream));
        let fake = FakeCli::new(daemon.path());
        let daemons = daemons(&fake);
        warm(&daemons);

        let started = Instant::now();
        let outcome = daemons.call("h1", "projects.list", json!({}), Duration::from_millis(300));
        let waited = started.elapsed();
        assert_eq!(outcome.err(), Some(RemoteError::Timeout));
        assert!(
            waited >= Duration::from_millis(350),
            "gave up early: {waited:?}"
        );
        assert!(
            waited < Duration::from_secs(3),
            "waited too long: {waited:?}"
        );
    }

    #[test]
    fn a_trickle_of_unrelated_lines_does_not_extend_the_deadline() {
        let daemon = FakeDaemon::start(|_request, mut stream| {
            while writeln!(stream, r#"{{"id":"other","ok":true}}"#).is_ok() {
                thread::sleep(Duration::from_millis(20));
            }
        });
        let fake = FakeCli::new(daemon.path());
        let daemons = daemons(&fake);
        warm(&daemons);

        let started = Instant::now();
        let outcome = daemons.call("h1", "projects.list", json!({}), Duration::from_millis(200));
        let waited = started.elapsed();
        assert_eq!(outcome.err(), Some(RemoteError::Timeout));
        assert!(
            waited < Duration::from_secs(2),
            "waited too long: {waited:?}"
        );
    }

    #[test]
    fn call_reports_protocol_errors_for_eof_and_garbage() {
        let daemon = FakeDaemon::start(|request, mut stream| {
            if request["method"] == "garbage" {
                reply(&mut stream, "this is not json");
            }
        });
        let fake = FakeCli::new(daemon.path());
        let daemons = daemons(&fake);

        let eof = daemons
            .call("h1", "eof", json!({}), Duration::from_secs(5))
            .err();
        assert_eq!(
            eof,
            Some(RemoteError::Protocol(
                "the daemon closed the connection".into()
            ))
        );
        let garbage = daemons
            .call("h1", "garbage", json!({}), Duration::from_secs(5))
            .err();
        assert!(matches!(garbage, Some(RemoteError::Protocol(_))));
    }

    #[test]
    fn a_sent_request_is_never_resent() {
        let daemon = FakeDaemon::start(|_request, stream| drop(stream));
        let fake = FakeCli::new(daemon.path());
        let daemons = daemons(&fake);

        let outcome = daemons.call("h1", "shell.create", json!({}), Duration::from_secs(5));
        assert!(matches!(outcome.err(), Some(RemoteError::Protocol(_))));
        // Give a wrongly retried request time to show up before counting.
        thread::sleep(Duration::from_millis(200));
        assert_eq!(daemon.requests().len(), 1);
        assert_eq!(daemon.accepted(), 1);
        assert_eq!(fake.count("client ensure"), 1);
    }

    #[test]
    fn status_parses_every_state_and_optional_fields() {
        let lines = [
            r#"{"state":"online","rtt_ms":12,"since":1790000000,"label":"MacBook"}"#,
            r#"{"state":"connecting","label":"MacBook"}"#,
            r#"{"state":"offline","since":1790000100,"reason":"relay closed","label":"MacBook"}"#,
            r#"{"state":"sideways"}"#,
        ];
        let next = Arc::new(AtomicUsize::new(0));
        let daemon = FakeDaemon::start(move |_request, mut stream| {
            let index = next.fetch_add(1, Ordering::SeqCst);
            reply(&mut stream, lines[index]);
        });
        let fake = FakeCli::new(daemon.path());
        let daemons = daemons(&fake);

        assert_eq!(
            daemons.status("h1").expect("online"),
            HostStatus {
                state: LinkState::Online,
                rtt_ms: Some(12),
                since: Some(1_790_000_000),
                reason: None,
                label: Some("MacBook".into()),
            }
        );
        assert_eq!(
            daemons.status("h1").expect("connecting"),
            HostStatus {
                state: LinkState::Connecting,
                rtt_ms: None,
                since: None,
                reason: None,
                label: Some("MacBook".into()),
            }
        );
        assert_eq!(
            daemons.status("h1").expect("offline"),
            HostStatus {
                state: LinkState::Offline,
                rtt_ms: None,
                since: Some(1_790_000_100),
                reason: Some("relay closed".into()),
                label: Some("MacBook".into()),
            }
        );
        assert!(matches!(
            daemons.status("h1").err(),
            Some(RemoteError::Protocol(_))
        ));
        assert_eq!(
            daemon
                .requests()
                .iter()
                .map(|r| r["op"].clone())
                .collect::<Vec<_>>(),
            vec![json!("status"); 4]
        );
    }

    #[test]
    fn status_times_out_on_a_silent_daemon() {
        let daemon = FakeDaemon::start(|_request, stream| hold_until_closed(stream));
        let fake = FakeCli::new(daemon.path());
        let daemons = daemons(&fake);
        warm(&daemons);

        let started = Instant::now();
        assert_eq!(daemons.status("h1").err(), Some(RemoteError::Timeout));
        let waited = started.elapsed();
        assert!(
            waited >= Duration::from_millis(250),
            "gave up early: {waited:?}"
        );
        assert!(
            waited < Duration::from_secs(3),
            "waited too long: {waited:?}"
        );
    }

    #[test]
    fn parse_status_rejects_unknown_states_and_junk() {
        assert!(matches!(
            parse_status(r#"{"state":"asleep"}"#),
            Err(RemoteError::Protocol(_))
        ));
        assert!(matches!(
            parse_status(r#"{"rtt_ms":3}"#),
            Err(RemoteError::Protocol(_))
        ));
        assert!(matches!(
            parse_status("nope"),
            Err(RemoteError::Protocol(_))
        ));
        let status = parse_status(r#"{"state":"online","reason":"","label":""}"#).expect("status");
        assert_eq!((status.reason, status.label), (None, None));
    }

    #[test]
    fn the_daemon_is_ensured_once_and_its_socket_cached() {
        let daemon = echo_daemon();
        let fake = FakeCli::new(daemon.path());
        let daemons = daemons(&fake);
        let timeout = Duration::from_secs(5);

        daemons.call("h1", "a", json!({}), timeout).expect("first");
        daemons.call("h1", "b", json!({}), timeout).expect("second");
        assert_eq!(fake.count("client ensure --desktop h1"), 1);
        assert_eq!(fake.count("client socket --desktop h1"), 1);
        assert_eq!(daemon.accepted(), 2);
    }

    #[test]
    fn concurrent_first_calls_start_the_daemon_once() {
        let daemon = echo_daemon();
        let fake = FakeCli::new(daemon.path());
        let daemons = Arc::new(daemons(&fake));

        let threads: Vec<_> = (0..8)
            .map(|_| {
                let daemons = Arc::clone(&daemons);
                thread::spawn(move || daemons.call("h1", "a", json!({}), Duration::from_secs(5)))
            })
            .collect();
        for thread in threads {
            assert!(thread.join().expect("call thread").is_ok());
        }
        assert_eq!(fake.count("client ensure"), 1);
        assert_eq!(fake.count("client socket"), 1);
    }

    #[test]
    fn a_stale_socket_is_recovered_by_ensuring_again() {
        let first = echo_daemon();
        let fake = FakeCli::new(first.path());
        let daemons = daemons(&fake);
        let timeout = Duration::from_secs(5);
        daemons
            .call("h1", "a", json!({}), timeout)
            .expect("first daemon");

        drop(first);
        let second = echo_daemon();
        fake.dir
            .write("socket", &second.path().display().to_string());
        daemons
            .call("h1", "b", json!({}), timeout)
            .expect("recovered");
        assert_eq!(fake.count("client ensure"), 2);
        assert_eq!(fake.count("client socket"), 2);
        assert_eq!(second.requests().len(), 1);
    }

    #[test]
    fn connect_gives_up_after_one_retry_and_forgets_the_path() {
        let fake = FakeCli::new(Path::new("/tmp/rh-nobody-listens.sock"));
        let daemons = daemons(&fake);

        let error = daemons
            .call("h1", "a", json!({}), Duration::from_secs(5))
            .expect_err("unreachable");
        assert!(matches!(error, RemoteError::Unreachable(_)), "{error:?}");
        assert_eq!(fake.count("client ensure"), 2);

        // The failed path was not kept, so the next call asks again.
        daemons
            .call("h1", "a", json!({}), Duration::from_secs(5))
            .err();
        assert_eq!(fake.count("client ensure"), 4);
    }

    #[test]
    fn ensure_failures_and_bad_socket_paths_are_unreachable() {
        let fake = FakeCli::new(Path::new("relative.sock"));
        let daemons = daemons(&fake);
        let timeout = Duration::from_secs(5);

        let relative = daemons
            .call("h1", "a", json!({}), timeout)
            .expect_err("error");
        assert!(
            matches!(relative, RemoteError::Unreachable(_)),
            "{relative:?}"
        );

        fake.dir.write("ensure_fail", "");
        let failed = daemons
            .call("h1", "a", json!({}), timeout)
            .expect_err("error");
        let RemoteError::Unreachable(detail) = failed else {
            panic!("expected Unreachable, got {failed:?}");
        };
        assert!(detail.contains("relay is down"), "{detail}");
    }

    #[test]
    fn forget_drops_the_cached_socket() {
        let daemon = echo_daemon();
        let fake = FakeCli::new(daemon.path());
        let daemons = daemons(&fake);
        let timeout = Duration::from_secs(5);

        daemons.call("h1", "a", json!({}), timeout).expect("first");
        daemons.forget("h1");
        daemons.call("h1", "a", json!({}), timeout).expect("second");
        assert_eq!(fake.count("client ensure"), 2);
    }

    #[test]
    fn hosts_parses_arrays_objects_and_ignores_secrets() {
        let fake = FakeCli::new(Path::new("/tmp/unused.sock"));
        let array = r#"[
            {"id":"aaaaaaaa-1111","label":"Studio","pairing":{"key":"TOPSECRET"},"relay":"wss://r"},
            {"id":"bbbbbbbbbbbb-2222"},
            {"id":"cccccccc-3333","label":"  "},
            {"label":"no id"},
            {"id":7,"label":"numeric id"},
            {"id":"","label":"empty id"},
            "junk"
        ]"#;
        let expected = vec![
            Host {
                id: "aaaaaaaa-1111".into(),
                label: "Studio".into(),
            },
            Host {
                id: "bbbbbbbbbbbb-2222".into(),
                label: "bbbbbbbb".into(),
            },
            Host {
                id: "cccccccc-3333".into(),
                label: "cccccccc".into(),
            },
        ];
        fake.dir.write("hosts.json", array);
        assert_eq!(fake.cli.hosts().expect("array form"), expected);

        fake.dir
            .write("hosts.json", &format!(r#"{{"v":1,"hosts":{array}}}"#));
        assert_eq!(fake.cli.hosts().expect("object form"), expected);

        fake.dir.write("hosts.json", "[]");
        assert_eq!(fake.cli.hosts().expect("empty"), Vec::new());
        assert_eq!(fake.log(), vec!["hosts list --json"; 3]);
    }

    #[test]
    fn hosts_rejects_unreadable_output_without_echoing_it() {
        let fake = FakeCli::new(Path::new("/tmp/unused.sock"));
        for output in ["", "not json TOPSECRET", r#"{"v":1}"#, r#""just a string""#] {
            fake.dir.write("hosts.json", output);
            let error = fake.cli.hosts().expect_err("unreadable");
            assert!(!error.contains("TOPSECRET"), "{error}");
            assert!(error.starts_with("riwork-remote printed"), "{error}");
        }
    }

    #[test]
    fn add_host_builds_the_command_and_validates_up_front() {
        let fake = FakeCli::new(Path::new("/tmp/unused.sock"));
        assert!(fake.cli.add_host("  \n", "x").is_err());
        assert!(fake.log().is_empty());

        fake.cli
            .add_host("  riwork://pair?v=2&data=ABC \n", "  Studio  ")
            .expect("add with label");
        fake.cli
            .add_host("riwork://pair?v=2&data=ABC", "   ")
            .expect("add without label");
        // The link never appears on the command line, only on standard input.
        assert_eq!(
            fake.log(),
            vec!["hosts add --link - --label Studio", "hosts add --link -"]
        );
        assert!(
            fake.log()
                .iter()
                .all(|line| !line.contains("riwork://") && !line.contains("ABC"))
        );
        assert_eq!(fake.dir.read("stdin").trim(), "riwork://pair?v=2&data=ABC");
    }

    #[test]
    fn remove_host_runs_the_command() {
        let fake = FakeCli::new(Path::new("/tmp/unused.sock"));
        fake.cli.remove_host("h-1").expect("remove");
        assert!(fake.cli.remove_host(" ").is_err());
        assert_eq!(fake.log(), vec!["hosts remove h-1"]);
    }

    #[test]
    fn a_failing_add_host_never_leaks_the_link() {
        let fake = FakeCli::new(Path::new("/tmp/unused.sock"));
        fake.dir.write("add_fail", "");
        let link = "riwork://pair?v=2&data=SECRET";

        let error = fake.cli.add_host(link, "Studio").expect_err("add fails");
        assert!(!error.contains("SECRET"), "{error}");
        assert!(!error.contains("riwork://"), "{error}");
        assert!(error.contains("cannot redeem"), "{error}");

        // A link without the usual scheme is still the caller's secret.
        let error = fake
            .cli
            .add_host("opaque-token-XYZ", "")
            .expect_err("add fails");
        assert!(!error.contains("opaque-token-XYZ"), "{error}");
    }

    #[test]
    fn pair_desktop_returns_the_link_and_removes_its_scratch_directory() {
        let fake = FakeCli::new(Path::new("/tmp/unused.sock"));
        let routes = fake.dir.file("nested/state/routes.json");
        let request = PairRequest {
            name: " Studio ".into(),
            relay: "ws://127.0.0.1:8787".into(),
            routes: routes.clone(),
        };

        let link = fake.cli.pair_desktop(&request).expect("link");
        assert_eq!(link.expose(), "riwork://pair?v=2&data=SECRET2");

        let scratch = fake.pair_dir();
        assert!(!scratch.exists(), "{} was left behind", scratch.display());
        assert_eq!(fake.dir.read("outdir_mode").trim(), "drwx------");
        let parent_mode = fs::metadata(routes.parent().expect("parent"))
            .expect("routes dir")
            .permissions()
            .mode();
        assert_eq!(parent_mode & 0o777, 0o700);

        let invocation = &fake.log()[0];
        assert!(
            invocation.starts_with("pair --protocol 2 --kind desktop --name Studio "),
            "{invocation}"
        );
        assert!(
            invocation.contains(" --relay ws://127.0.0.1:8787 "),
            "{invocation}"
        );
        assert!(
            invocation.contains(&format!("--relay-routes {} ", routes.display())),
            "{invocation}"
        );
        assert!(invocation.contains("--show-link"), "{invocation}");
        assert!(
            invocation.ends_with("--allow-insecure-loopback"),
            "{invocation}"
        );
    }

    #[test]
    fn pair_desktop_only_allows_insecure_loopback_for_local_ws_relays() {
        let fake = FakeCli::new(Path::new("/tmp/unused.sock"));
        for relay in [
            "wss://relay.example.com",
            "ws://relay.example.com:80",
            "ws://10.0.0.5:8787",
        ] {
            let request = PairRequest {
                name: "Studio".into(),
                relay: relay.into(),
                routes: fake.dir.file("routes.json"),
            };
            fake.cli.pair_desktop(&request).expect("link");
        }
        assert!(
            fake.log()
                .iter()
                .all(|line| !line.contains("--allow-insecure-loopback"))
        );
    }

    #[test]
    fn failing_pair_desktop_cleans_up_and_hides_the_link() {
        let fake = FakeCli::new(Path::new("/tmp/unused.sock"));
        let request = PairRequest {
            name: "Studio".into(),
            relay: "wss://relay.example.com".into(),
            routes: fake.dir.file("routes.json"),
        };

        fake.dir.write("pair_mode", "fail");
        let error = fake.cli.pair_desktop(&request).err().expect("fails");
        assert_eq!(error, "pair failed: cannot write routes");
        assert!(!fake.pair_dir().exists());

        // With nothing on stderr the error falls back to stdout, but never to the link line.
        fake.dir.write("pair_mode", "failquiet");
        let error = fake.cli.pair_desktop(&request).err().expect("fails");
        assert_eq!(error, "Pairing ready for this Mac");
        assert!(!error.contains("LEAKED") && !error.contains("riwork://"));
        assert!(!fake.pair_dir().exists());

        fake.dir.write("pair_mode", "nolink");
        let error = fake.cli.pair_desktop(&request).err().expect("fails");
        assert_eq!(error, "riwork-remote printed no pairing link");
        assert!(!fake.pair_dir().exists());
    }

    #[test]
    fn pair_desktop_validates_its_request() {
        let fake = FakeCli::new(Path::new("/tmp/unused.sock"));
        let request = |name: &str, relay: &str| PairRequest {
            name: name.into(),
            relay: relay.into(),
            routes: fake.dir.file("routes.json"),
        };
        assert!(fake.cli.pair_desktop(&request(" ", "wss://r")).is_err());
        assert!(fake.cli.pair_desktop(&request("Studio", "")).is_err());
        assert!(fake.log().is_empty());
    }

    #[test]
    fn a_hanging_subprocess_is_killed_at_the_timeout() {
        let fake = FakeCli::new(Path::new("/tmp/unused.sock"));
        let cli = fake
            .cli
            .clone()
            .with_timeout_cap(Duration::from_millis(600));
        // The first run of a fresh script is slow on macOS; pay for it before the clock starts.
        fake.cli.hosts().expect("warm-up run");
        fake.dir.write("hang", "");

        let started = Instant::now();
        let error = cli.hosts().expect_err("times out");
        let waited = started.elapsed();
        assert!(error.contains("timed out"), "{error}");
        assert!(waited >= Duration::from_millis(550), "{waited:?}");
        assert!(waited < Duration::from_secs(5), "{waited:?}");

        // The script exec'd into `sleep`, keeping its pid; the kill must have reaped it.
        let pid = fake.dir.read("pid").trim().to_owned();
        assert!(!pid.is_empty());
        let alive = Command::new("kill")
            .args(["-0", &pid])
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        assert!(!alive, "process {pid} survived the timeout");
    }

    #[test]
    fn a_missing_binary_is_a_plain_error() {
        let cli = RemoteCli::at(PathBuf::from("/nonexistent/riwork-remote"));
        let error = cli.hosts().expect_err("cannot start");
        assert!(
            error.starts_with("Cannot start /nonexistent/riwork-remote"),
            "{error}"
        );
    }

    #[test]
    fn scrub_hides_every_token_with_a_link() {
        assert_eq!(
            scrub(
                "bad link riwork://pair?v=2&data=A then 'RIWORK://pair?x=1' end",
                &[]
            ),
            "bad link [pairing link hidden] then [pairing link hidden] end"
        );
        assert_eq!(
            scrub("line one\nriwork://a\n", &[]),
            "line one\n[pairing link hidden]\n"
        );
        assert_eq!(scrub("nothing here", &[]), "nothing here");
        assert_eq!(
            scrub("token abc123 used", &["abc123"]),
            "token [pairing link hidden] used"
        );
        assert_eq!(scrub("keep it", &[""]), "keep it");
    }

    #[test]
    fn loopback_detection_is_limited_to_plain_local_ws_urls() {
        for relay in [
            "ws://127.0.0.1:8787",
            "ws://localhost",
            "ws://localhost:1/path?x=1",
            "WS://LOCALHOST:8787",
            "ws://[::1]:9000/x",
            "ws://user@127.0.0.1:8787",
        ] {
            assert!(is_insecure_loopback(relay), "{relay}");
        }
        for relay in [
            "wss://127.0.0.1:8787",
            "ws://example.com",
            "ws://127.0.0.1.example.com",
            "ws://example.com/127.0.0.1",
            "ws://[::2]:80",
            "http://localhost",
            "",
        ] {
            assert!(!is_insecure_loopback(relay), "{relay}");
        }
    }

    #[test]
    fn error_helpers_classify_and_describe() {
        let rpc = |code: &str, message: &str| RemoteError::Rpc {
            code: code.into(),
            message: message.into(),
        };
        assert!(rpc("invalid_request", "Unsupported RPC method: pty.open").is_unsupported());
        assert!(!rpc("not_found", "unsupported RPC method").is_unsupported());
        assert!(!rpc("invalid_request", "bad params").is_unsupported());
        assert_eq!(RemoteError::Timeout.message(), "No answer in time");
        assert_eq!(
            rpc("busy", "").message(),
            "The host reported an error (busy)"
        );
        assert_eq!(rpc("busy", "Try later").to_string(), "Try later (busy)");
        assert_eq!(RemoteError::Timeout.to_string(), "No answer in time");
        let boxed: Box<dyn std::error::Error> = Box::new(RemoteError::Timeout);
        assert_eq!(boxed.to_string(), "No answer in time");
    }

    #[test]
    fn daemons_can_be_shared_across_threads() {
        fn shared<T: Send + Sync>() {}
        shared::<Daemons>();
        shared::<RemoteCli>();
    }

    #[test]
    fn attach_args_name_the_host_and_shell() {
        assert_eq!(
            attach_args("h1", "s1"),
            vec!["attach", "--desktop", "h1", "--shell", "s1"]
        );
    }
}
