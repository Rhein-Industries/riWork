//! The per-host client daemon: `riwork-remote client serve --desktop ID`.
//!
//! The relay lets one socket per role hold a route, so one process per host owns
//! the [`Link`] and everything on this Mac that talks to the host (the app's
//! project list, each terminal's bridge) talks to that process over a Unix socket.
//! `client ensure` starts it on demand; it exits after a few minutes without
//! clients.
//!
//! The socket lives in a mode-700 directory under `$RIWORK_HOME/remote/run/`, is
//! mode 600, and a connection from another user is dropped (`peer_cred`, which is
//! `getpeereid` on macOS). The first line of a connection is a JSON request:
//!
//! - `{"op":"call","id","method","params","timeout_ms"}` answers
//!   `{"id","ok":true,"result","server_ms"}` or `{"id","ok":false,"error":{code,message}}`,
//!   on the same connection, any number of times and concurrently. The daemon adds
//!   the codes `offline`, `timeout` and `invalid_request` to the host's own.
//! - `{"op":"status"}` answers one status line, `{"op":"watch"}` a line now and one
//!   at every change: `{"state":"connecting"|"online"|"offline","rtt_ms"?,"since"?,"reason"?,"label"}`.
//! - `{"op":"shutdown"}` ends the daemon (`hosts remove` uses it).
//! - `{"op":"attach","shell_id","columns","rows","term","ignore_size"}` turns the
//!   connection into binary frames, `u8 type || u32 big-endian length || payload`:
//!   `D` data (both ways), `R` resize `{columns,rows}` (bridge to daemon), `S` status
//!   (as above) and `E` end `{reason}` (daemon to bridge).
//!
//! Attaching is a `pty.open` on the host followed by parked `pty.read`s (one or
//! two, within the host's `max_reads`) and pipelined `pty.write`s. When the link
//! drops the bridge is told (`S` offline) and keeps its terminal frozen; when the
//! link is back the stream is opened again, `S` online follows, and the bridge
//! resets its terminal before tmux repaints it. Keystrokes typed while the link is
//! down are dropped, never replayed.
use crate::{
    client::{CallError, Connection, Link, LinkState, PtyFeatures, Reply, load_host, now_unix},
    config::Storage,
    crypto::uuid,
    log_safe,
};
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use fs2::FileExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, VecDeque},
    fs::{File, OpenOptions},
    io,
    os::unix::{
        fs::{FileTypeExt, OpenOptionsExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{
        AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt,
        BufReader,
    },
    net::{UnixListener, UnixStream, unix::OwnedWriteHalf},
    sync::{Notify, mpsc, watch},
    task::JoinSet,
    time::{Instant, sleep, sleep_until, timeout},
};

/// Frame types of an attached connection.
pub const FRAME_DATA: u8 = b'D';
pub const FRAME_RESIZE: u8 = b'R';
pub const FRAME_STATUS: u8 = b'S';
pub const FRAME_END: u8 = b'E';
/// The largest frame payload either side accepts. Terminal output arrives in
/// pieces of at most the host's `max_chunk` (64 KiB), keystrokes in less.
pub const MAX_FRAME_PAYLOAD: usize = 1 << 20;
/// One request line of the JSON ops.
const MAX_LINE: usize = 1 << 20;
/// A macOS `sockaddr_un` holds 104 bytes including the terminating NUL.
const MAX_SOCKET_PATH: usize = 103;
pub const DEFAULT_IDLE: Duration = Duration::from_secs(300);
const MAX_CALL_TIMEOUT_MS: u64 = 180_000;
const DEFAULT_CALL_TIMEOUT_MS: u64 = 15_000;
/// How long a parked `pty.read` waits on the host for output.
const READ_WAIT_MS: u64 = 15_000;
const READ_SLACK: Duration = Duration::from_secs(15);
/// `pty.open` answers on the host's first byte of output.
const OPEN_TIMEOUT: Duration = Duration::from_secs(30);
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
/// How long to wait before asking again after the host said `pty_limit`.
const RETRY_PAUSE: Duration = Duration::from_millis(100);
/// `pty.write`s in flight at once. The relay closes a socket whose peer queue
/// passes 16 messages; a paste is therefore never more than this many chunks ahead.
const MAX_WRITES_IN_FLIGHT: usize = 4;
/// Undelivered terminal output, and unsent keystrokes, a stream holds before it
/// stops asking the host for more, or the bridge for more.
const OUTBOX_HIGH: usize = 256 * 1024;
const INPUT_HIGH: usize = 256 * 1024;
/// A CR that follows other input by less than this is sent with that gap; see
/// `gap_ms`.
const MAX_GAP_MS: u64 = 150;

// ---- Frames -----------------------------------------------------------------

/// One frame as bytes.
pub fn encode_frame(kind: u8, payload: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        payload.len() <= MAX_FRAME_PAYLOAD,
        "frame payload over the limit"
    );
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(kind);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}
pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    kind: u8,
    payload: &[u8],
) -> io::Result<()> {
    let frame = encode_frame(kind, payload)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
    writer.write_all(&frame).await
}
/// The next frame, or `None` when the peer closed between frames.
pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<Option<(u8, Vec<u8>)>> {
    let mut header = [0u8; 5];
    let mut got = 0;
    while got < header.len() {
        let n = reader.read(&mut header[got..]).await?;
        if n == 0 {
            return if got == 0 {
                Ok(None)
            } else {
                Err(io::ErrorKind::UnexpectedEof.into())
            };
        }
        got += n;
    }
    let length = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
    if length > MAX_FRAME_PAYLOAD {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame payload over the limit",
        ));
    }
    let mut payload = vec![0u8; length];
    reader.read_exact(&mut payload).await?;
    Ok(Some((header[0], payload)))
}

/// A line of at most `max` bytes without its newline; `None` at a clean end.
async fn read_line_limited<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    max: usize,
) -> io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let buffered = reader.fill_buf().await?;
        if buffered.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Err(io::ErrorKind::UnexpectedEof.into())
            };
        }
        if let Some(at) = buffered.iter().position(|b| *b == b'\n') {
            line.extend_from_slice(&buffered[..at]);
            reader.consume(at + 1);
            return Ok(Some(line));
        }
        let n = buffered.len();
        line.extend_from_slice(buffered);
        reader.consume(n);
        if line.len() > max {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
        }
    }
}
fn json_line(value: &Value) -> Vec<u8> {
    let mut line = serde_json::to_vec(value).unwrap_or_default();
    line.push(b'\n');
    line
}

// ---- Paths ------------------------------------------------------------------

/// Everything the daemon of one host keeps on disk, under `remote/run/`.
#[derive(Clone, Debug)]
pub struct DaemonPaths {
    pub dir: PathBuf,
    pub socket: PathBuf,
    /// Held by the running daemon.
    pub lock: PathBuf,
    /// Held while `ensure` decides whether to start one.
    pub ensure_lock: PathBuf,
    pub log: PathBuf,
}
/// The paths for a host, from its desktop id alone. The names are a hash: a
/// UUID plus a long home would pass the 104 bytes a socket path may have.
pub fn daemon_paths(storage: &Storage, desktop_id: &str) -> Result<DaemonPaths> {
    uuid(desktop_id).context("--desktop takes a host's full desktop id")?;
    let digest = Sha256::digest(format!("riwork/client-socket/v1\0{desktop_id}"));
    let name = hex::encode(&digest[..6]);
    let dir = std::path::absolute(storage.dir.join("run"))?;
    let socket = dir.join(format!("{name}.sock"));
    ensure!(
        socket.as_os_str().len() <= MAX_SOCKET_PATH,
        "the socket path {} is longer than the {MAX_SOCKET_PATH} bytes a Unix socket allows; use a shorter RIWORK_HOME",
        socket.display()
    );
    Ok(DaemonPaths {
        lock: dir.join(format!("{name}.lock")),
        ensure_lock: dir.join(format!("{name}.ensure")),
        log: dir.join(format!("{name}.log")),
        socket,
        dir,
    })
}
/// The one place the socket path is derived: `serve` binds it, `client socket`
/// prints it, and callers connect to what that printed.
pub fn socket_path(storage: &Storage, desktop_id: &str) -> Result<PathBuf> {
    Ok(daemon_paths(storage, desktop_id)?.socket)
}
fn prepare_run_dir(dir: &Path) -> Result<()> {
    if dir.exists() {
        let meta = std::fs::symlink_metadata(dir)?;
        ensure!(
            meta.is_dir() && !meta.file_type().is_symlink(),
            "{} is not a real directory",
            dir.display()
        );
        ensure!(
            meta.permissions().mode() & 0o077 == 0,
            "{} must have mode 700",
            dir.display()
        );
    } else {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(0o700).create(dir)?;
    }
    Ok(())
}
fn open_private(path: &Path, append: bool) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .append(append)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}
/// `Ok(None)` when someone else holds the lock.
fn try_lock(path: &Path) -> Result<Option<File>> {
    let file = open_private(path, false)?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(Some(file)),
        Err(e) if e.raw_os_error() == fs2::lock_contended_error().raw_os_error() => Ok(None),
        Err(e) => Err(e).context("lock"),
    }
}
fn effective_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

// ---- Sizes and terminals -----------------------------------------------------

fn clamp_size(columns: u64, rows: u64) -> (u16, u16) {
    (columns.clamp(1, 1000) as u16, rows.clamp(1, 500) as u16)
}
/// The two values `pty.open` takes for TERM.
pub fn is_known_term(term: &str) -> bool {
    matches!(term, "xterm-ghostty" | "xterm-256color")
}

/// The request line of `attach`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachRequest {
    pub shell_id: String,
    pub columns: u16,
    pub rows: u16,
    pub term: String,
    pub ignore_size: bool,
}
impl AttachRequest {
    pub fn to_line(&self) -> Vec<u8> {
        json_line(&json!({
            "op":"attach","shell_id":self.shell_id,"columns":self.columns,"rows":self.rows,
            "term":self.term,"ignore_size":self.ignore_size
        }))
    }
    fn parse(request: &Value) -> Result<Self> {
        let shell_id = request["shell_id"].as_str().unwrap_or_default();
        uuid(shell_id).context("shell_id must be a full lowercase UUID")?;
        let term = request["term"].as_str().unwrap_or("xterm-256color");
        ensure!(
            is_known_term(term),
            "term must be xterm-ghostty or xterm-256color"
        );
        let number = |name: &str| {
            request[name]
                .as_u64()
                .filter(|n| *n >= 1)
                .with_context(|| format!("{name} must be a positive integer"))
        };
        let (columns, rows) = clamp_size(number("columns")?, number("rows")?);
        Ok(Self {
            shell_id: shell_id.into(),
            columns,
            rows,
            term: term.into(),
            ignore_size: request["ignore_size"].as_bool().unwrap_or(false),
        })
    }
}

// ---- Terminal streams: pure parts ---------------------------------------------

/// Puts the replies of parked `pty.read`s back in order. Replies of two reads
/// can arrive out of order; each says where its bytes start (`seq`, the count of
/// bytes read before it), so the stream is rebuilt from that and not from the
/// order of arrival.
#[derive(Debug, Default)]
pub struct Reassembler {
    next: u64,
    held: BTreeMap<u64, Vec<u8>>,
    held_bytes: usize,
    eof: Option<(Option<u64>, String)>,
}
impl Reassembler {
    pub fn new() -> Self {
        Self::default()
    }
    /// Takes one reply's bytes; returns the pieces that are now in order.
    pub fn push_data(&mut self, seq: u64, mut data: Vec<u8>) -> Result<Vec<Vec<u8>>> {
        if data.is_empty() {
            return Ok(vec![]);
        }
        if seq < self.next {
            // Already delivered, wholly or in part.
            let seen = (self.next - seq) as usize;
            if seen >= data.len() {
                return Ok(vec![]);
            }
            data.drain(..seen);
            return self.push_data(self.next, data);
        }
        if seq > self.next {
            ensure!(
                self.held.len() < 16 && self.held_bytes + data.len() <= 4 * MAX_FRAME_PAYLOAD,
                "terminal output arrived with a gap that did not close"
            );
            self.held_bytes += data.len();
            if let Some(same_place) = self.held.insert(seq, data) {
                self.held_bytes -= same_place.len();
            }
            return Ok(vec![]);
        }
        let mut ready = vec![];
        self.next += data.len() as u64;
        ready.push(data);
        while let Some((&at, _)) = self.held.first_key_value() {
            if at > self.next {
                break;
            }
            let mut chunk = self.held.remove(&at).unwrap_or_default();
            self.held_bytes -= chunk.len();
            let seen = (self.next - at) as usize;
            if seen >= chunk.len() {
                continue;
            }
            chunk.drain(..seen);
            self.next += chunk.len() as u64;
            ready.push(chunk);
        }
        Ok(ready)
    }
    pub fn push_eof(&mut self, seq: Option<u64>, reason: String) {
        self.eof.get_or_insert((seq, reason));
    }
    /// The end reason, once everything before the end has been handed out.
    pub fn finished(&self) -> Option<&str> {
        let (seq, reason) = self.eof.as_ref()?;
        seq.is_none_or(|seq| self.next >= seq)
            .then_some(reason.as_str())
    }
    pub fn next_seq(&self) -> u64 {
        self.next
    }
}

/// Keystrokes waiting for a `pty.write`, each with the moment it arrived.
#[derive(Debug, Default)]
pub struct InputQueue {
    events: VecDeque<(Instant, Vec<u8>)>,
    bytes: usize,
}
/// What one `pty.write` carries.
#[derive(Debug, PartialEq, Eq)]
pub struct Chunk {
    pub data: Vec<u8>,
    /// When its first and last byte arrived from the bridge.
    pub first: Instant,
    pub last: Instant,
}
impl InputQueue {
    pub fn push(&mut self, at: Instant, data: Vec<u8>) {
        if !data.is_empty() {
            self.bytes += data.len();
            self.events.push_back((at, data));
        }
    }
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
    /// The next chunk, of at most `max` bytes. Input that arrived while earlier
    /// writes were in flight is merged, except that a chunk never takes in input
    /// that starts with CR: the CR gets a chunk of its own, so that the pause
    /// before it can be kept (see [`gap_ms`]).
    pub fn next_chunk(&mut self, max: usize) -> Option<Chunk> {
        let first = self.events.front()?.0;
        let mut data = Vec::new();
        let mut last = first;
        while data.len() < max {
            let Some((at, front)) = self.events.front_mut() else {
                break;
            };
            if !data.is_empty() && front.first() == Some(&b'\r') {
                break;
            }
            let take = front.len().min(max - data.len());
            data.extend(front.drain(..take));
            last = *at;
            if front.is_empty() {
                self.events.pop_front();
            }
        }
        self.bytes -= data.len();
        Some(Chunk { data, first, last })
    }
}
/// The pause the person made before a chunk that starts with CR: how long they really
/// waited since their previous input, at most 150 ms. Codex treats a Return that follows
/// text too closely as part of a paste, so a Return must not reach it sooner after the
/// text than the person typed it. The network can squeeze two writes together; the host
/// waits `gap_ms` before writing a Return to re-create the pause. [`Writer`] sends only
/// what the two writes are not already apart by, so a Return typed after a pause costs
/// no extra delay.
pub fn gap_ms(previous: Option<Instant>, chunk: &Chunk) -> u64 {
    if chunk.data.first() != Some(&b'\r') {
        return 0;
    }
    previous.map_or(0, |previous| {
        let waited = chunk.first.saturating_duration_since(previous);
        (waited.as_millis() as u64).min(MAX_GAP_MS)
    })
}

/// How many `pty.read`s the streams of this daemon may have parked at once: the
/// host's `max_reads`, counted over all of them. A stream parks one: that is all
/// it needs to see output, and the relay drops a socket whose queue passes 16
/// messages, so a second one would only add to the replies that can arrive at once.
#[derive(Default)]
struct ReadBudget {
    in_use: std::sync::Mutex<usize>,
    freed: Notify,
}
struct ReadSlot(Arc<ReadBudget>);
impl Drop for ReadSlot {
    fn drop(&mut self) {
        *self.0.in_use.lock().unwrap_or_else(|e| e.into_inner()) -= 1;
        self.0.freed.notify_waiters();
    }
}
impl ReadBudget {
    fn try_take(self: &Arc<Self>, max_reads: usize) -> Option<ReadSlot> {
        let mut in_use = self.in_use.lock().unwrap_or_else(|e| e.into_inner());
        (*in_use < max_reads).then(|| {
            *in_use += 1;
            ReadSlot(self.clone())
        })
    }
    async fn take(self: &Arc<Self>, max_reads: usize) -> ReadSlot {
        loop {
            let notified = self.freed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(slot) = self.try_take(max_reads) {
                return slot;
            }
            notified.await;
        }
    }
}

// ---- The daemon ------------------------------------------------------------------

struct Daemon {
    link: Link,
    label: String,
    budget: Arc<ReadBudget>,
    clients: watch::Sender<usize>,
    quit: Notify,
}
struct ClientGuard(Arc<Daemon>);
impl ClientGuard {
    fn new(daemon: &Arc<Daemon>) -> Self {
        daemon.clients.send_modify(|n| *n += 1);
        Self(daemon.clone())
    }
}
impl Drop for ClientGuard {
    fn drop(&mut self) {
        self.0.clients.send_modify(|n| *n -= 1);
    }
}

/// Resolves once no client has been connected for `idle`.
async fn idle_wait(mut clients: watch::Receiver<usize>, idle: Duration) {
    loop {
        if clients.wait_for(|n| *n == 0).await.is_err() {
            return;
        }
        if timeout(idle, clients.wait_for(|n| *n > 0)).await.is_err() {
            return;
        }
    }
}

/// Runs the daemon of one host until it is idle, told to quit or signalled.
/// Returns at once, successfully, when another daemon already runs for the host.
pub async fn serve(storage: Storage, desktop_id: &str, idle: Duration) -> Result<()> {
    let host = load_host(&storage, desktop_id)?;
    let paths = daemon_paths(&storage, desktop_id)?;
    prepare_run_dir(&paths.dir)?;
    let Some(_running) = try_lock(&paths.lock)? else {
        eprintln!("riwork-remote client: a daemon already runs for this host.");
        return Ok(());
    };
    // The lock is ours, so a socket that is still there is a dead daemon's.
    if let Ok(meta) = std::fs::symlink_metadata(&paths.socket) {
        ensure!(
            meta.file_type().is_socket(),
            "{} exists and is not a socket",
            paths.socket.display()
        );
        std::fs::remove_file(&paths.socket)?;
    }
    let listener = UnixListener::bind(&paths.socket)
        .with_context(|| format!("bind {}", paths.socket.display()))?;
    std::fs::set_permissions(&paths.socket, std::fs::Permissions::from_mode(0o600))?;
    use tokio::signal::unix::{SignalKind, signal};
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    // The controlling terminal of whoever started us may close; we outlive it.
    let mut hangup = signal(SignalKind::hangup())?;
    let (clients, clients_rx) = watch::channel(0usize);
    let daemon = Arc::new(Daemon {
        label: host.label.clone(),
        link: Link::spawn(host),
        budget: Arc::new(ReadBudget::default()),
        clients,
        quit: Notify::new(),
    });
    eprintln!(
        "riwork-remote client: serving \"{}\" on {} (exits after {} s without clients).",
        log_safe(&daemon.label),
        paths.socket.display(),
        idle.as_secs()
    );
    let result = loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    // Counted before the task runs, so a client cannot be missed by the idle timer.
                    let guard = ClientGuard::new(&daemon);
                    let daemon = daemon.clone();
                    tokio::spawn(async move { handle_client(daemon, stream, guard).await });
                }
                Err(e) => break Err(anyhow::Error::from(e).context("accept")),
            },
            () = idle_wait(clients_rx.clone(), idle) => {
                eprintln!("riwork-remote client: no clients for {} s; exiting.", idle.as_secs());
                break Ok(());
            }
            () = daemon.quit.notified() => {
                eprintln!("riwork-remote client: asked to quit.");
                break Ok(());
            }
            _ = terminate.recv() => break Ok(()),
            _ = interrupt.recv() => break Ok(()),
            _ = hangup.recv() => {}
        }
    };
    let _ = std::fs::remove_file(&paths.socket);
    result
}

async fn handle_client(daemon: Arc<Daemon>, stream: UnixStream, guard: ClientGuard) {
    let _guard = guard;
    match stream.peer_cred() {
        Ok(cred) if cred.uid() == effective_uid() => {}
        _ => return,
    }
    let (read, write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let (lines, mut lines_rx) = mpsc::channel::<Vec<u8>>(64);
    let writer = tokio::spawn(async move {
        let mut write = write;
        while let Some(line) = lines_rx.recv().await {
            if write.write_all(&line).await.is_err() {
                break;
            }
        }
        write
    });
    let mut calls: JoinSet<()> = JoinSet::new();
    let mut taken_over = None;
    loop {
        let line = match read_line_limited(&mut reader, MAX_LINE).await {
            Ok(Some(line)) => line,
            _ => break,
        };
        let Ok(request) = serde_json::from_slice::<Value>(&line) else {
            let _ = lines
                .send(json_line(&error_reply(
                    Value::Null,
                    "invalid_request",
                    "a request is one JSON line",
                )))
                .await;
            break;
        };
        match request["op"].as_str() {
            Some("call") => {
                let (daemon, lines) = (daemon.clone(), lines.clone());
                calls.spawn(async move {
                    let reply = run_call(&daemon.link, &request).await;
                    let _ = lines.send(json_line(&reply)).await;
                });
            }
            Some("status") => {
                let status = daemon.link.state().to_status(&daemon.label);
                if lines.send(json_line(&status)).await.is_err() {
                    break;
                }
            }
            Some("shutdown") => {
                let _ = lines.send(json_line(&json!({"ok":true}))).await;
                daemon.quit.notify_one();
                break;
            }
            Some(op @ ("watch" | "attach")) => {
                taken_over = Some((op.to_owned(), request));
                break;
            }
            _ => {
                let reply = error_reply(Value::Null, "invalid_request", "unknown op");
                if lines.send(json_line(&reply)).await.is_err() {
                    break;
                }
            }
        }
    }
    match taken_over {
        Some((op, _)) if op == "watch" => {
            drop(calls);
            drop(lines);
            if let Ok(write) = writer.await {
                watch_status(&daemon, reader, write).await;
            }
        }
        Some((_, request)) => {
            drop(calls);
            drop(lines);
            if let Ok(write) = writer.await {
                attach(daemon, request, reader, write).await;
            }
        }
        None => {
            // The client may only have finished writing (it still reads), so calls
            // that are running are answered. If it is gone altogether the writer has
            // failed and `lines` is closed: then they are dropped, so that a caller
            // that vanished does not keep the daemon from idling for minutes.
            let answered = async { while calls.join_next().await.is_some() {} };
            tokio::select! {
                () = answered => {}
                () = lines.closed() => {}
            }
            drop(lines);
            let _ = writer.await;
        }
    }
}

/// An error the daemon makes itself: the host took no time over it, so `server_ms` is 0.
fn error_reply(id: Value, code: &str, message: &str) -> Value {
    json!({"id":id,"ok":false,"error":{"code":code,"message":message},"server_ms":0})
}

/// One `call` request, answered as the line the caller reads.
async fn run_call(link: &Link, request: &Value) -> Value {
    let id = request["id"].clone();
    if !(id.is_null() || id.is_string() || id.is_number()) {
        return error_reply(
            Value::Null,
            "invalid_request",
            "id must be a string or a number",
        );
    }
    let method = request["method"].as_str().unwrap_or_default();
    if method.is_empty()
        || method.len() > 64
        || !method
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'_')
    {
        return error_reply(
            id,
            "invalid_request",
            "method must be a name such as projects.list",
        );
    }
    let params = match request.get("params") {
        None | Some(Value::Null) => json!({}),
        Some(params) if params.is_object() => params.clone(),
        Some(_) => return error_reply(id, "invalid_request", "params must be an object"),
    };
    let limit = request["timeout_ms"]
        .as_u64()
        .unwrap_or(DEFAULT_CALL_TIMEOUT_MS)
        .clamp(100, MAX_CALL_TIMEOUT_MS);
    match link
        .call(method, params, Duration::from_millis(limit))
        .await
    {
        Ok(reply) => {
            let server_ms = reply.server_ms().unwrap_or(0);
            let mut out = match reply.into_result() {
                Ok(result) => json!({"id":id,"ok":true,"result":result}),
                Err(e) => error_reply(id, e.code(), &e.message()),
            };
            out["server_ms"] = json!(server_ms);
            out
        }
        Err(e) => error_reply(id, e.code(), &e.message()),
    }
}

async fn watch_status(
    daemon: &Daemon,
    mut reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    mut write: OwnedWriteHalf,
) {
    let mut state = daemon.link.subscribe();
    loop {
        let line = json_line(&state.borrow_and_update().to_status(&daemon.label));
        if write.write_all(&line).await.is_err() {
            return;
        }
        tokio::select! {
            changed = state.changed() => if changed.is_err() { return },
            // Anything from the client, or its end, finishes the watch.
            _ = read_line_limited(&mut reader, MAX_LINE) => return,
        }
    }
}

// ---- Attach ----------------------------------------------------------------------

/// A parked `pty.read` that is stopped when the stream is done with it.
struct ReadTask(tokio::task::JoinHandle<std::result::Result<Reply, CallError>>);
impl Drop for ReadTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// One `pty.write` that was sent and is not yet answered.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Sent {
    seq: u64,
    data: Vec<u8>,
    gap_ms: u64,
}
/// The writing side of a stream: what to send next, and what to do when the host
/// refuses with `pty_limit` because it is behind.
///
/// Writes are pipelined, and the host takes them only at the running offset. When one
/// is refused (the host's input is backed up), the offset does not advance, so the
/// writes after it, already on their way, are refused as skipping bytes. The window is
/// then drained and sent again from the first refusal, in order and with the same
/// offsets, after a short pause. Nothing is lost and nothing is written twice.
#[derive(Default)]
struct Writer {
    /// The `seq` of the next new chunk.
    next_seq: u64,
    /// Sent and not yet acknowledged, oldest first.
    unacked: VecDeque<Sent>,
    /// Refused, to be sent again before anything new.
    resend: VecDeque<Sent>,
    /// The host refused a write: nothing new is sent until the window has drained.
    refused: bool,
    pause_until: Option<Instant>,
    /// Refusals in a row, to give up on a host that never catches up.
    refusals: u32,
    /// When the last write was sent.
    last_sent: Option<Instant>,
}
impl Writer {
    fn may_send(&self) -> bool {
        !self.refused && self.pause_until.is_none()
    }
    fn paused_until(&self) -> Option<Instant> {
        self.pause_until
    }
    fn resume(&mut self) {
        self.pause_until = None;
    }
    /// The next write to send: a refused one again, or a new chunk of the queue.
    fn next(
        &mut self,
        queue: &mut InputQueue,
        last_input: &mut Option<Instant>,
        max_write: usize,
        now: Instant,
    ) -> Option<Sent> {
        let sent = match self.resend.pop_front() {
            Some(sent) => sent,
            None => {
                let chunk = queue.next_chunk(max_write)?;
                // The pause the person made, less the time the two writes are apart anyway.
                let apart = self
                    .last_sent
                    .map_or(0, |at| now.saturating_duration_since(at).as_millis() as u64);
                let gap_ms = gap_ms(*last_input, &chunk).saturating_sub(apart);
                *last_input = Some(chunk.last);
                let sent = Sent {
                    seq: self.next_seq,
                    gap_ms,
                    data: chunk.data,
                };
                self.next_seq += sent.data.len() as u64;
                sent
            }
        };
        self.last_sent = Some(now);
        self.unacked.push_back(sent.clone());
        Some(sent)
    }
    /// The answer to the write at `seq`. `Err` is a failure of the stream.
    fn answered(&mut self, seq: u64, answer: Result<Value, CallError>) -> Result<(), CallError> {
        match answer {
            Ok(_) => {
                self.refusals = 0;
                self.unacked.retain(|sent| sent.seq != seq);
            }
            Err(CallError::Rpc { code, .. }) if code == "pty_limit" => {
                self.refused = true;
            }
            // Writes behind a refused one skip the bytes it left unwritten.
            Err(CallError::Rpc { code, .. }) if code == "invalid_request" && self.refused => {}
            Err(e) => return Err(e),
        }
        Ok(())
    }
    /// Called when no write is in flight: after a refusal, queues the unacknowledged
    /// ones to be sent again. `Err` after too many refusals in a row.
    fn drained(&mut self) -> Result<(), CallError> {
        if self.refused {
            self.refused = false;
            self.refusals += 1;
            if self.refusals > 100 {
                return Err(CallError::Rpc {
                    code: "pty_limit".into(),
                    message: "the host's terminal input stayed backed up".into(),
                });
            }
            self.resend = std::mem::take(&mut self.unacked);
            self.pause_until = Some(Instant::now() + RETRY_PAUSE);
        }
        Ok(())
    }
}

enum Input {
    Data(Instant, Vec<u8>),
    Resize(u16, u16),
}

/// The connection of one bridge, and the stream it is attached to.
struct Attach {
    daemon: Arc<Daemon>,
    request: AttachRequest,
    out: mpsc::Sender<(u8, Vec<u8>)>,
    input: mpsc::Receiver<Input>,
    size: (u16, u16),
    /// The size `pty.open` was sent with; a resize that followed is sent after it.
    opened_size: (u16, u16),
    queue: InputQueue,
    /// Arrival of the last byte of the chunk written before.
    last_input: Option<Instant>,
}

async fn attach(
    daemon: Arc<Daemon>,
    request: Value,
    mut reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    mut write: OwnedWriteHalf,
) {
    let request = match AttachRequest::parse(&request) {
        Ok(request) => request,
        Err(e) => {
            let reason = format!("invalid attach request: {e:#}");
            let _ = write_frame(&mut write, FRAME_END, &json_end(&reason)).await;
            return;
        }
    };
    let (out, mut out_rx) = mpsc::channel::<(u8, Vec<u8>)>(32);
    let writer = tokio::spawn(async move {
        while let Some((kind, payload)) = out_rx.recv().await {
            if write_frame(&mut write, kind, &payload).await.is_err() {
                break;
            }
        }
    });
    let (input_tx, input) = mpsc::channel::<Input>(64);
    let reading = tokio::spawn(async move {
        while let Ok(Some((kind, payload))) = read_frame(&mut reader).await {
            let event = match kind {
                FRAME_DATA => Input::Data(Instant::now(), payload),
                FRAME_RESIZE => {
                    let Ok(size) = serde_json::from_slice::<Value>(&payload) else {
                        continue;
                    };
                    match (size["columns"].as_u64(), size["rows"].as_u64()) {
                        (Some(c), Some(r)) if c >= 1 && r >= 1 => {
                            let (c, r) = clamp_size(c, r);
                            Input::Resize(c, r)
                        }
                        _ => continue,
                    }
                }
                _ => continue,
            };
            if input_tx.send(event).await.is_err() {
                break;
            }
        }
    });
    let run = Attach {
        size: (request.columns, request.rows),
        opened_size: (request.columns, request.rows),
        daemon,
        request,
        out,
        input,
        queue: InputQueue::default(),
        last_input: None,
    };
    run.run().await;
    // `run` held the only sender; the writer drains what is queued and ends.
    let _ = writer.await;
    reading.abort();
}

fn json_end(reason: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({"reason":reason})).unwrap_or_default()
}

enum Opened {
    /// The stream's id, and the session it was opened on.
    Stream(String, u64),
    /// The link broke before or while opening.
    LinkLost(String),
    Fatal(String),
    Gone,
}
enum StreamEnd {
    Eof(String),
    LinkLost(String),
    ClientGone,
    Fatal(String),
}

/// What a bridge is told when `pty.open` is refused.
fn open_failure(error: &CallError) -> String {
    match error {
        CallError::Rpc { code, message }
            if code == "invalid_request" && message.contains("unsupported RPC method") =>
        {
            "this host does not allow terminal streams for this Mac; pair it again with `pair --protocol 2 --kind desktop` (a phone pairing cannot attach)".into()
        }
        CallError::Rpc { code, message } => format!("{code}: {}", log_safe(message)),
        other => other.to_string(),
    }
}

impl Attach {
    async fn status(&self, state: Value) -> bool {
        let payload = serde_json::to_vec(&state).unwrap_or_default();
        self.out.send((FRAME_STATUS, payload)).await.is_ok()
    }
    async fn end(&self, reason: &str) {
        let _ = self.out.send((FRAME_END, json_end(reason))).await;
    }
    fn synthetic_offline(&self, reason: &str) -> Value {
        json!({"state":"offline","since":now_unix(),"reason":reason,"label":self.daemon.label})
    }

    async fn run(mut self) {
        let link = self.daemon.link.clone();
        let mut last_generation = 0u64;
        // Why the previous stream ended, until the bridge was told.
        let mut lost: Option<String> = None;
        loop {
            let connection = match link.connection() {
                Some(c) if c.generation > last_generation => {
                    // The bridge is told even when the link came back at once: it has to
                    // reset its terminal before the new stream draws.
                    if let Some(reason) = lost.take()
                        && !self.status(self.synthetic_offline(&reason)).await
                    {
                        return;
                    }
                    c
                }
                _ => {
                    let state = match lost.take() {
                        Some(reason) => self.synthetic_offline(&reason),
                        None => link.state().to_status(&self.daemon.label),
                    };
                    if !self.status(state).await {
                        return;
                    }
                    match self.wait_session(last_generation).await {
                        Some(c) => c,
                        None => return,
                    }
                }
            };
            last_generation = connection.generation;
            let Some(pty) = connection.features.pty else {
                self.end("this host does not offer terminal streams; pair this Mac with `pair --protocol 2 --kind desktop` on the host").await;
                return;
            };
            let stream = match self.open().await {
                Opened::Stream(stream, generation) if generation == connection.generation => stream,
                Opened::Stream(stream, _) => {
                    // The link flapped while the terminal opened: the stream belongs to a
                    // session that is over (or to the next one, which this cannot use).
                    self.close_stream(stream);
                    lost = Some("the link changed while the terminal was opening".into());
                    continue;
                }
                Opened::LinkLost(reason) => {
                    lost = Some(reason);
                    continue;
                }
                Opened::Fatal(reason) => {
                    self.end(&reason).await;
                    return;
                }
                Opened::Gone => return,
            };
            // The reads come first: a stream that cannot read must not be announced.
            let Some(read_slot) = self.acquire_reads(pty).await else {
                self.close_stream(stream);
                return;
            };
            let rtt_ms = match link.state() {
                LinkState::Online { rtt_ms } => rtt_ms,
                _ => None,
            };
            let online = LinkState::Online { rtt_ms }.to_status(&self.daemon.label);
            if !self.status(online).await {
                self.close_stream(stream);
                return;
            }
            match self.stream(&connection, pty, &stream, read_slot).await {
                StreamEnd::Eof(reason) => {
                    // Normally already gone on the host; this makes sure its slot is free.
                    self.close_stream(stream);
                    self.end(&reason).await;
                    return;
                }
                StreamEnd::Fatal(reason) => {
                    self.close_stream(stream);
                    self.end(&reason).await;
                    return;
                }
                StreamEnd::ClientGone => {
                    self.close_stream(stream);
                    return;
                }
                StreamEnd::LinkLost(reason) => {
                    // Whatever was typed and not yet written is dropped with the stream, as
                    // the bridge's notice says; it must not appear in the next one.
                    self.queue = InputQueue::default();
                    self.last_input = None;
                    lost = Some(reason);
                }
            }
        }
    }

    /// Waits for a session newer than `after`, keeping up with the bridge's size
    /// and dropping its keystrokes. `None`: the bridge went away.
    async fn wait_session(&mut self, after: u64) -> Option<Connection> {
        let link = self.daemon.link.clone();
        let session = link.next_session(after);
        tokio::pin!(session);
        loop {
            tokio::select! {
                connection = &mut session => return Some(connection),
                event = self.input.recv() => match event {
                    Some(Input::Resize(columns, rows)) => self.size = (columns, rows),
                    Some(Input::Data(..)) => {}
                    None => return None,
                },
                () = self.out.closed() => return None,
            }
        }
    }

    async fn open(&mut self) -> Opened {
        let link = self.daemon.link.clone();
        self.opened_size = self.size;
        let params = json!({
            "shell_id": self.request.shell_id,
            "columns": self.size.0,
            "rows": self.size.1,
            "term": self.request.term,
            "ignore_size": self.request.ignore_size,
        });
        let call = match link.start("pty.open", params).await {
            Ok(call) => call,
            Err(e) => return Opened::LinkLost(e.message()),
        };
        let generation = call.generation;
        let wait = call.wait(OPEN_TIMEOUT);
        tokio::pin!(wait);
        let reply = loop {
            tokio::select! {
                reply = &mut wait => break reply,
                event = self.input.recv() => match event {
                    Some(Input::Resize(columns, rows)) => self.size = (columns, rows),
                    Some(Input::Data(..)) => {}
                    None => return Opened::Gone,
                },
                () = self.out.closed() => return Opened::Gone,
            }
        };
        match reply.and_then(Reply::into_result) {
            Ok(result) => match result["stream"].as_str().map(|s| (s, uuid(s))) {
                Some((stream, Ok(_))) => Opened::Stream(stream.to_owned(), generation),
                _ => Opened::Fatal("the host answered pty.open without a stream id".into()),
            },
            Err(CallError::Timeout) => {
                // The stream may exist on the host; a new session is what removes it.
                link.reconnect("pty.open timed out");
                Opened::LinkLost("the host did not answer pty.open".into())
            }
            Err(e) if e.is_link_loss() => Opened::LinkLost(e.message()),
            Err(e) => Opened::Fatal(open_failure(&e)),
        }
    }

    /// The read slot for the life of the stream (see [`ReadBudget`]). `None`: the bridge
    /// went away while waiting for one.
    async fn acquire_reads(&mut self, pty: PtyFeatures) -> Option<ReadSlot> {
        let budget = self.daemon.budget.clone();
        let slot = budget.take(pty.max_reads as usize);
        tokio::pin!(slot);
        loop {
            tokio::select! {
                slot = &mut slot => return Some(slot),
                event = self.input.recv() => match event {
                    Some(Input::Resize(columns, rows)) => self.size = (columns, rows),
                    Some(Input::Data(..)) => {}
                    None => return None,
                },
                () = self.out.closed() => return None,
            }
        }
    }

    /// Best effort; the host also drops the stream when the session ends.
    fn close_stream(&self, stream: String) {
        let link = self.daemon.link.clone();
        tokio::spawn(async move {
            let _ = link
                .call(
                    "pty.close",
                    json!({"stream":stream}),
                    Duration::from_secs(3),
                )
                .await;
        });
    }

    /// Runs one stream until it ends.
    async fn stream(
        &mut self,
        connection: &Connection,
        pty: PtyFeatures,
        stream: &str,
        _read_slot: ReadSlot,
    ) -> StreamEnd {
        let link = self.daemon.link.clone();
        let mut session = link.subscribe_connection();
        let mut read: Option<ReadTask> = None;
        let mut writes: JoinSet<(u64, std::result::Result<Reply, CallError>)> = JoinSet::new();
        let mut reassembler = Reassembler::new();
        let mut outbox: VecDeque<Vec<u8>> = VecDeque::new();
        let mut outbox_bytes = 0usize;
        let mut writer = Writer::default();
        // The host answers an idle read with no data, and a busy one with `pty_limit`
        // only if something else holds its reads: ask again shortly.
        let mut read_pause: Option<Instant> = None;
        let mut read_refusals = 0u32;
        // Window changes are folded into the latest size, with one request in flight:
        // dragging a window must not queue a repaint of tmux for every pixel.
        let mut resizes: JoinSet<std::result::Result<Reply, CallError>> = JoinSet::new();
        // The window may have changed while the stream was being opened.
        let mut resize_wanted = (self.size != self.opened_size).then_some(self.size);
        loop {
            if let Some(reason) = reassembler.finished()
                && outbox.is_empty()
            {
                return StreamEnd::Eof(reason.to_owned());
            }
            // Keep a read parked, unless the bridge is behind or the host asked for a pause.
            if read.is_none()
                && reassembler.finished().is_none()
                && outbox_bytes <= OUTBOX_HIGH
                && read_pause.is_none()
            {
                let params = json!({"stream":stream,"wait_ms":READ_WAIT_MS});
                match link.start("pty.read", params).await {
                    Ok(call) => {
                        read = Some(ReadTask(tokio::spawn(
                            call.wait(Duration::from_millis(READ_WAIT_MS) + READ_SLACK),
                        )));
                    }
                    Err(e) => return self.failure(&link, e),
                }
            }
            if resizes.is_empty()
                && let Some((columns, rows)) = resize_wanted.take()
            {
                let params = json!({"stream":stream,"columns":columns,"rows":rows});
                match link.start("pty.resize", params).await {
                    Ok(call) => {
                        resizes.spawn(call.wait(WRITE_TIMEOUT));
                    }
                    Err(e) => return self.failure(&link, e),
                }
            }
            if writes.is_empty()
                && let Err(e) = writer.drained()
            {
                return self.failure(&link, e);
            }
            while writes.len() < MAX_WRITES_IN_FLIGHT && writer.may_send() {
                let Some(sent) = writer.next(
                    &mut self.queue,
                    &mut self.last_input,
                    pty.max_write,
                    Instant::now(),
                ) else {
                    break;
                };
                let params = json!({
                    "stream": stream,
                    "seq": sent.seq,
                    "data": URL_SAFE_NO_PAD.encode(&sent.data),
                    "gap_ms": sent.gap_ms,
                });
                match link.start("pty.write", params).await {
                    Ok(call) => {
                        let seq = sent.seq;
                        writes.spawn(async move { (seq, call.wait(WRITE_TIMEOUT).await) });
                    }
                    Err(e) => return self.failure(&link, e),
                }
            }
            tokio::select! {
                joined = async { (&mut read.as_mut().expect("guarded").0).await }, if read.is_some() => {
                    read = None;
                    let reply = match joined {
                        Ok(Ok(reply)) => reply,
                        Ok(Err(e)) => return self.failure(&link, e),
                        Err(_) => return StreamEnd::Fatal("internal error while reading".into()),
                    };
                    match reply.into_result() {
                        Ok(result) => {
                            read_refusals = 0;
                            if let Err(e) = take_read(&result, stream, &mut reassembler, &mut outbox, &mut outbox_bytes) {
                                return StreamEnd::Fatal(format!("{e:#}"));
                            }
                        }
                        // The host's reads are all taken, perhaps by a stream that is going.
                        Err(CallError::Rpc { code, message }) if code == "pty_limit" => {
                            read_refusals += 1;
                            if read_refusals > 50 {
                                return StreamEnd::Fatal(format!("pty_limit: {}", log_safe(&message)));
                            }
                            read_pause = Some(Instant::now() + RETRY_PAUSE);
                        }
                        Err(e) => return self.failure(&link, e),
                    }
                }
                Some(joined) = writes.join_next(), if !writes.is_empty() => {
                    let Ok((seq, answer)) = joined else {
                        return StreamEnd::Fatal("internal error while writing".into());
                    };
                    let answer = match answer {
                        Ok(reply) => reply.into_result(),
                        Err(e) => Err(e),
                    };
                    if let Err(e) = writer.answered(seq, answer) {
                        return self.failure(&link, e);
                    }
                }
                Some(joined) = resizes.join_next(), if !resizes.is_empty() => {
                    match joined {
                        Ok(Ok(reply)) => if let Err(e) = reply.into_result() {
                            return self.failure(&link, e);
                        },
                        Ok(Err(e)) => return self.failure(&link, e),
                        Err(_) => return StreamEnd::Fatal("internal error while resizing".into()),
                    }
                }
                () = async { sleep_until(read_pause.expect("guarded")).await }, if read_pause.is_some() => {
                    read_pause = None;
                }
                () = async { sleep_until(writer.paused_until().expect("guarded")).await }, if writer.paused_until().is_some() => {
                    writer.resume();
                }
                permit = self.out.reserve(), if !outbox.is_empty() => {
                    let Ok(permit) = permit else { return StreamEnd::ClientGone };
                    if let Some(data) = outbox.pop_front() {
                        outbox_bytes -= data.len();
                        permit.send((FRAME_DATA, data));
                    }
                }
                event = self.input.recv(), if self.queue.bytes() < INPUT_HIGH => match event {
                    Some(Input::Data(at, data)) => self.queue.push(at, data),
                    Some(Input::Resize(columns, rows)) => {
                        self.size = (columns, rows);
                        resize_wanted = Some((columns, rows));
                    }
                    None => return StreamEnd::ClientGone,
                },
                changed = session.changed() => {
                    let current = session.borrow().as_ref().map(|c| c.generation);
                    if changed.is_err() || current != Some(connection.generation) {
                        return StreamEnd::LinkLost("the link to the host was lost".into());
                    }
                }
            }
        }
    }

    /// What a failed call means for the stream.
    fn failure(&self, link: &Link, error: CallError) -> StreamEnd {
        match error {
            CallError::Offline(reason) => StreamEnd::LinkLost(reason),
            CallError::Timeout => {
                // A request without an answer leaves the stream in an unknown state;
                // a new session is the only way to be sure the host's side is gone.
                link.reconnect("a terminal request timed out");
                StreamEnd::LinkLost("the host stopped answering".into())
            }
            CallError::Rpc { code, .. } if code == "not_found" => StreamEnd::Eof("closed".into()),
            other => StreamEnd::Fatal(other.to_string()),
        }
    }
}

/// Files one `pty.read` result: data into the reassembler (and, once in order,
/// the outbox), or the end of the stream.
fn take_read(
    result: &Value,
    stream: &str,
    reassembler: &mut Reassembler,
    outbox: &mut VecDeque<Vec<u8>>,
    outbox_bytes: &mut usize,
) -> Result<()> {
    ensure!(
        result["stream"].as_str() == Some(stream),
        "the host answered pty.read for another stream"
    );
    let seq = result["seq"].as_u64();
    if result["eof"] == true {
        let reason = result["reason"].as_str().unwrap_or("closed");
        reassembler.push_eof(seq, log_safe(reason));
        return Ok(());
    }
    let data = match result["data"].as_str() {
        None | Some("") => return Ok(()),
        Some(text) => URL_SAFE_NO_PAD
            .decode(text.trim_end_matches('='))
            .context("the host sent terminal data that is not base64url")?,
    };
    let seq = seq.context("the host sent terminal data without a seq")?;
    for piece in reassembler.push_data(seq, data)? {
        *outbox_bytes += piece.len();
        outbox.push_back(piece);
    }
    Ok(())
}

// ---- Starting the daemon -----------------------------------------------------------

/// Whether a daemon answers on `socket`.
async fn probe(socket: &Path) -> bool {
    let attempt = async {
        let mut stream = UnixStream::connect(socket).await?;
        stream.write_all(b"{\"op\":\"status\"}\n").await?;
        let mut reader = BufReader::new(stream);
        let line = read_line_limited(&mut reader, MAX_LINE).await?;
        let status: Value = serde_json::from_slice(&line.unwrap_or_default())?;
        Ok::<_, anyhow::Error>(status["state"].is_string())
    };
    matches!(timeout(Duration::from_secs(2), attempt).await, Ok(Ok(true)))
}

/// The last lines of a daemon's log, for the error of an `ensure` that failed.
fn log_tail(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(5)..]
        .iter()
        .map(|l| log_safe(l))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Makes sure a daemon serves the host and returns its socket path. Idempotent
/// and safe to run concurrently: a lock makes the callers take turns, and the
/// first one to find no daemon starts it. `exe` is the `riwork-remote` to run.
pub async fn ensure(storage: &Storage, desktop_id: &str, exe: &Path) -> Result<PathBuf> {
    load_host(storage, desktop_id)?;
    let paths = daemon_paths(storage, desktop_id)?;
    prepare_run_dir(&paths.dir)?;
    if probe(&paths.socket).await {
        return Ok(paths.socket);
    }
    let end = Instant::now() + Duration::from_secs(10);
    let _turn = loop {
        match try_lock(&paths.ensure_lock)? {
            Some(lock) => break lock,
            None => {
                ensure!(
                    Instant::now() < end,
                    "timed out waiting for another `client ensure`"
                );
                sleep(Duration::from_millis(25)).await;
            }
        }
    };
    // Whoever held the lock may have started it.
    if probe(&paths.socket).await {
        return Ok(paths.socket);
    }
    let log = open_private(&paths.log, true)?;
    if log.metadata()?.len() > 256 * 1024 {
        log.set_len(0)?;
    }
    let mut child = std::process::Command::new(exe)
        .args(["client", "serve", "--desktop", desktop_id])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        // Its own process group: the terminal that started it may close.
        .process_group(0)
        .spawn()
        .with_context(|| format!("start {}", exe.display()))?;
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        if probe(&paths.socket).await {
            // Reaped when it ends, so a long-lived caller (a bridge) leaves no zombie.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            return Ok(paths.socket);
        }
        if let Some(status) = child.try_wait()? {
            // Another daemon may have won a race to bind; that one is fine.
            if probe(&paths.socket).await {
                return Ok(paths.socket);
            }
            bail!(
                "the client daemon exited ({status}) before it was ready:\n{}",
                log_tail(&paths.log)
            );
        }
        if Instant::now() >= end {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "the client daemon did not become ready in 10 s:\n{}",
                log_tail(&paths.log)
            );
        }
        sleep(Duration::from_millis(50)).await;
    }
}

/// Asks a running daemon to quit; a missing one is not an error.
pub async fn shutdown(storage: &Storage, desktop_id: &str) -> Result<()> {
    let socket = socket_path(storage, desktop_id)?;
    let attempt = async {
        let mut stream = UnixStream::connect(&socket).await?;
        stream.write_all(b"{\"op\":\"shutdown\"}\n").await?;
        let mut reader = BufReader::new(stream);
        read_line_limited(&mut reader, MAX_LINE).await?;
        Ok::<_, io::Error>(())
    };
    let _ = timeout(Duration::from_secs(2), attempt).await;
    Ok(())
}

// ---- Talking to a daemon (the CLI side) ------------------------------------------------

/// One `call`; the reply line as the daemon sent it.
pub async fn daemon_call(
    socket: &Path,
    method: &str,
    params: Value,
    timeout_ms: u64,
) -> Result<Value> {
    let mut stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("connect {}", socket.display()))?;
    let request =
        json!({"op":"call","id":"1","method":method,"params":params,"timeout_ms":timeout_ms});
    stream.write_all(&json_line(&request)).await?;
    let mut reader = BufReader::new(stream);
    // The daemon answers within the call's own timeout; the extra is its slack.
    let line = timeout(
        Duration::from_millis(timeout_ms.min(MAX_CALL_TIMEOUT_MS)) + Duration::from_secs(5),
        read_line_limited(&mut reader, MAX_LINE),
    )
    .await
    .context("the client daemon did not answer")??
    .context("the client daemon closed the connection")?;
    Ok(serde_json::from_slice(&line)?)
}
/// The status line now.
pub async fn daemon_status(socket: &Path) -> Result<Value> {
    let mut stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("connect {}", socket.display()))?;
    stream.write_all(b"{\"op\":\"status\"}\n").await?;
    let mut reader = BufReader::new(stream);
    let line = timeout(
        Duration::from_secs(5),
        read_line_limited(&mut reader, MAX_LINE),
    )
    .await
    .context("the client daemon did not answer")??
    .context("the client daemon closed the connection")?;
    Ok(serde_json::from_slice(&line)?)
}
/// Status lines as they change, until the daemon ends the watch or `on_line`
/// returns false.
pub async fn daemon_watch(socket: &Path, mut on_line: impl FnMut(Value) -> bool) -> Result<()> {
    let mut stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("connect {}", socket.display()))?;
    stream.write_all(b"{\"op\":\"watch\"}\n").await?;
    let mut reader = BufReader::new(stream);
    while let Some(line) = read_line_limited(&mut reader, MAX_LINE).await? {
        if !on_line(serde_json::from_slice(&line)?) {
            break;
        }
    }
    Ok(())
}
/// Connects and sends the attach request; frames follow on the stream.
pub async fn daemon_attach(socket: &Path, request: &AttachRequest) -> Result<UnixStream> {
    let mut stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("connect {}", socket.display()))?;
    stream.write_all(&request.to_line()).await?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frames_round_trip_and_a_partial_or_oversized_frame_is_an_error() {
        let mut bytes = Vec::new();
        write_frame(&mut bytes, FRAME_DATA, b"hello").await.unwrap();
        write_frame(&mut bytes, FRAME_RESIZE, br#"{"columns":80,"rows":24}"#)
            .await
            .unwrap();
        write_frame(&mut bytes, FRAME_END, b"").await.unwrap();
        assert_eq!(&bytes[..6], &[b'D', 0, 0, 0, 5, b'h']);
        let mut reader = &bytes[..];
        assert_eq!(
            read_frame(&mut reader).await.unwrap(),
            Some((FRAME_DATA, b"hello".to_vec()))
        );
        let (kind, payload) = read_frame(&mut reader).await.unwrap().unwrap();
        assert_eq!(kind, FRAME_RESIZE);
        assert_eq!(payload, br#"{"columns":80,"rows":24}"#);
        assert_eq!(
            read_frame(&mut reader).await.unwrap(),
            Some((FRAME_END, vec![]))
        );
        assert_eq!(read_frame(&mut reader).await.unwrap(), None, "clean end");

        // Cut in the header, and cut in the payload.
        assert!(read_frame(&mut &bytes[..3]).await.is_err());
        assert!(read_frame(&mut &bytes[..8]).await.is_err());
        // A length past the limit is refused before anything is allocated.
        let mut huge = vec![b'D'];
        huge.extend_from_slice(&((MAX_FRAME_PAYLOAD as u32) + 1).to_be_bytes());
        let error = read_frame(&mut &huge[..]).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(encode_frame(FRAME_DATA, &vec![0; MAX_FRAME_PAYLOAD + 1]).is_err());
        assert_eq!(
            encode_frame(FRAME_DATA, &vec![7; MAX_FRAME_PAYLOAD])
                .unwrap()
                .len(),
            MAX_FRAME_PAYLOAD + 5
        );
    }

    #[tokio::test]
    async fn request_lines_are_bounded_and_split_on_newlines() {
        let mut reader = BufReader::new(&b"{\"a\":1}\n{\"b\":2}\nrest"[..]);
        assert_eq!(
            read_line_limited(&mut reader, 64).await.unwrap().unwrap(),
            b"{\"a\":1}"
        );
        assert_eq!(
            read_line_limited(&mut reader, 64).await.unwrap().unwrap(),
            b"{\"b\":2}"
        );
        assert!(
            read_line_limited(&mut reader, 64).await.is_err(),
            "a cut line"
        );
        let long = [b'x'; 200];
        let mut reader = BufReader::with_capacity(16, &long[..]);
        assert!(read_line_limited(&mut reader, 64).await.is_err());
        let mut empty = BufReader::new(&b""[..]);
        assert!(read_line_limited(&mut empty, 64).await.unwrap().is_none());
    }

    fn storage_in(path: &Path) -> Storage {
        Storage::at(path.to_path_buf()).unwrap()
    }

    #[test]
    fn socket_names_are_short_stable_and_one_per_host() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = storage_in(tmp.path());
        let a = "11111111-2222-4333-8444-555555555555";
        let b = "11111111-2222-4333-8444-555555555556";
        let first = daemon_paths(&storage, a).unwrap();
        assert_eq!(first.socket, socket_path(&storage, a).unwrap());
        assert_eq!(first.socket, daemon_paths(&storage, a).unwrap().socket);
        assert_ne!(first.socket, socket_path(&storage, b).unwrap());
        let name = first.socket.file_name().unwrap().to_str().unwrap();
        assert_eq!(name.len(), 12 + ".sock".len(), "{name}");
        assert!(name.ends_with(".sock") && !name.contains(a));
        assert!(first.socket.is_absolute());
        assert_eq!(
            first.socket.parent().unwrap(),
            tmp.path().join("remote/run")
        );
        assert!(first.lock.to_str().unwrap().ends_with(".lock"));
        assert!(first.socket.as_os_str().len() <= MAX_SOCKET_PATH);
        // A host id is a full lowercase UUID, nothing else.
        assert!(daemon_paths(&storage, "mac").is_err());
        assert!(daemon_paths(&storage, "AAAAAAAA-BBBB-4CCC-8DDD-EEEEEEEEEEEE").is_err());
    }

    #[test]
    fn a_home_too_long_for_a_socket_is_refused_with_advice() {
        let tmp = tempfile::tempdir().unwrap();
        let mut long = tmp.path().to_path_buf();
        while long.as_os_str().len() < 100 {
            long.push("a-directory-with-a-long-name");
        }
        std::fs::create_dir_all(&long).unwrap();
        let storage = storage_in(&long);
        let error = daemon_paths(&storage, "11111111-2222-4333-8444-555555555555")
            .unwrap_err()
            .to_string();
        assert!(error.contains("shorter RIWORK_HOME"), "{error}");
    }

    #[test]
    fn the_run_directory_is_private_and_a_loose_one_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let run = tmp.path().join("run");
        prepare_run_dir(&run).unwrap();
        assert_eq!(
            std::fs::metadata(&run).unwrap().permissions().mode() & 0o777,
            0o700
        );
        prepare_run_dir(&run).unwrap();
        std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(prepare_run_dir(&run).is_err());
        let lock = tmp.path().join("x.lock");
        let held = try_lock(&lock).unwrap().unwrap();
        assert!(
            try_lock(&lock).unwrap().is_none(),
            "a second holder is refused"
        );
        assert_eq!(
            std::fs::metadata(&lock).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(held);
        assert!(try_lock(&lock).unwrap().is_some());
    }

    #[test]
    fn attach_requests_are_checked() {
        let shell = "44444444-4444-4444-8444-444444444444";
        let ok = AttachRequest::parse(&json!({"op":"attach","shell_id":shell,"columns":120,"rows":40,"term":"xterm-ghostty","ignore_size":true})).unwrap();
        assert_eq!((ok.columns, ok.rows, ok.ignore_size), (120, 40, true));
        assert_eq!(ok.term, "xterm-ghostty");
        let line = ok.to_line();
        assert_eq!(*line.last().unwrap(), b'\n');
        assert_eq!(
            AttachRequest::parse(&serde_json::from_slice(&line).unwrap()).unwrap(),
            ok
        );
        // Sizes are clamped to what the host takes, TERM is one of two.
        let huge =
            AttachRequest::parse(&json!({"shell_id":shell,"columns":99999,"rows":99999})).unwrap();
        assert_eq!((huge.columns, huge.rows), (1000, 500));
        assert_eq!(huge.term, "xterm-256color");
        for bad in [
            json!({"shell_id":"nope","columns":80,"rows":24}),
            json!({"shell_id":shell,"columns":0,"rows":24}),
            json!({"shell_id":shell,"columns":80}),
            json!({"shell_id":shell,"columns":80,"rows":24,"term":"vt100"}),
            json!({"columns":80,"rows":24}),
        ] {
            assert!(AttachRequest::parse(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn reads_that_arrive_out_of_order_are_delivered_in_order() {
        let mut r = Reassembler::new();
        assert!(
            r.push_data(5, b"world".to_vec()).unwrap().is_empty(),
            "held until the gap closes"
        );
        assert_eq!(
            r.push_data(0, b"hello".to_vec()).unwrap(),
            vec![b"hello".to_vec(), b"world".to_vec()]
        );
        assert_eq!(r.next_seq(), 10);
        // A duplicate is dropped, an overlap is trimmed, an empty reply is nothing.
        assert!(r.push_data(0, b"hello".to_vec()).unwrap().is_empty());
        assert_eq!(
            r.push_data(8, b"ldXY".to_vec()).unwrap(),
            vec![b"XY".to_vec()]
        );
        assert!(r.push_data(12, vec![]).unwrap().is_empty());
        assert_eq!(r.next_seq(), 12);
        // The same bytes held twice are counted once.
        assert!(r.push_data(20, vec![1; 10]).unwrap().is_empty());
        assert!(r.push_data(20, vec![1; 10]).unwrap().is_empty());
        assert_eq!(r.held_bytes, 10);
    }

    #[test]
    fn the_end_waits_for_the_data_before_it() {
        let mut r = Reassembler::new();
        assert_eq!(r.finished(), None);
        r.push_eof(Some(8), "exited".into());
        assert_eq!(r.finished(), None, "3 bytes are still on their way");
        r.push_data(0, b"abc".to_vec()).unwrap();
        assert_eq!(r.finished(), None);
        r.push_data(3, b"defgh".to_vec()).unwrap();
        assert_eq!(r.finished(), Some("exited"));
        // The first end wins; a second read's copy of it changes nothing.
        r.push_eof(Some(8), "other".into());
        assert_eq!(r.finished(), Some("exited"));
        // An end without a seq follows whatever is in order.
        let mut r = Reassembler::new();
        r.push_eof(None, "closed".into());
        assert_eq!(r.finished(), Some("closed"));
    }

    #[test]
    fn a_gap_that_never_closes_is_an_error_not_unbounded_memory() {
        let mut r = Reassembler::new();
        let mut failed = false;
        for i in 1..40u64 {
            if r.push_data(i * 1000, vec![0; 100]).is_err() {
                failed = true;
                break;
            }
        }
        assert!(failed);
    }

    #[test]
    fn input_is_cut_into_chunks_and_a_return_gets_its_own() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut q = InputQueue::default();
        q.push(at(0), b"ls".to_vec());
        q.push(at(5), b" -l".to_vec());
        q.push(at(400), b"\r".to_vec());
        q.push(at(405), b"pwd".to_vec());
        assert_eq!(q.bytes(), 9);
        let first = q.next_chunk(1000).unwrap();
        assert_eq!(first.data, b"ls -l");
        assert_eq!((first.first, first.last), (at(0), at(5)));
        assert_eq!(gap_ms(None, &first), 0, "text has no gap");
        let second = q.next_chunk(1000).unwrap();
        assert_eq!(
            second.data, b"\rpwd",
            "what follows a Return may ride with it"
        );
        // The person waited 395 ms after the last byte of the text; 150 is the most kept.
        assert_eq!(gap_ms(Some(first.last), &second), MAX_GAP_MS);
        assert!(q.is_empty() && q.bytes() == 0 && q.next_chunk(10).is_none());

        // A short pause is kept as it was; with nothing before it there is no gap to keep.
        let mut q = InputQueue::default();
        q.push(at(0), b"\r".to_vec());
        let alone = q.next_chunk(10).unwrap();
        assert_eq!(gap_ms(None, &alone), 0);
        assert_eq!(gap_ms(Some(at(0) - Duration::from_millis(40)), &alone), 40);
        assert_eq!(
            gap_ms(Some(at(0) + Duration::from_millis(40)), &alone),
            0,
            "clock skew"
        );
    }

    #[test]
    fn a_big_paste_is_split_at_the_hosts_write_limit_in_order() {
        let t0 = Instant::now();
        let paste: Vec<u8> = (0..100_000u32).map(|i| b'a' + (i % 26) as u8).collect();
        let mut q = InputQueue::default();
        q.push(t0, paste.clone());
        let mut seen = Vec::new();
        let mut sizes = Vec::new();
        while let Some(chunk) = q.next_chunk(32_768) {
            assert!(chunk.data.len() <= 32_768);
            sizes.push(chunk.data.len());
            seen.extend(chunk.data);
        }
        assert_eq!(sizes, vec![32_768, 32_768, 32_768, 1_696]);
        assert_eq!(seen, paste);
        assert_eq!(q.bytes(), 0);
    }

    #[tokio::test]
    async fn the_read_budget_is_shared_and_waiters_wake_when_a_slot_frees() {
        let budget = Arc::new(ReadBudget::default());
        let a = budget.try_take(2).unwrap();
        let _b = budget.try_take(2).unwrap();
        assert!(budget.try_take(2).is_none());
        let waiting = {
            let budget = budget.clone();
            tokio::spawn(async move { budget.take(2).await })
        };
        sleep(Duration::from_millis(50)).await;
        assert!(!waiting.is_finished());
        drop(a);
        let slot = timeout(Duration::from_secs(2), waiting)
            .await
            .unwrap()
            .unwrap();
        drop(slot);
        // A host that lowers its limit is respected at the next take.
        assert!(budget.try_take(1).is_none());
    }

    fn typed(queue: &mut InputQueue, at: Instant, text: &str) {
        queue.push(at, text.as_bytes().to_vec());
    }
    fn rpc(code: &str) -> CallError {
        CallError::Rpc {
            code: code.into(),
            message: "m".into(),
        }
    }

    #[test]
    fn a_refused_write_is_sent_again_with_the_writes_behind_it_in_order_and_with_the_same_offsets()
    {
        let t0 = Instant::now();
        let (mut queue, mut last) = (InputQueue::default(), None);
        let mut writer = Writer::default();
        // Four keys, each in a write of its own, all in flight.
        let mut sent = Vec::new();
        for key in ["a", "b", "c", "d"] {
            typed(&mut queue, t0, key);
            // One event at a time, as they arrive when the host is quick to take them.
            sent.push(writer.next(&mut queue, &mut last, 1000, t0).unwrap());
        }
        assert_eq!(sent.iter().map(|s| s.seq).collect::<Vec<_>>(), [0, 1, 2, 3]);
        // The host is behind: the first is refused and the others skip its bytes.
        writer.answered(0, Err(rpc("pty_limit"))).unwrap();
        assert!(!writer.may_send(), "nothing new while the window drains");
        for seq in 1..4 {
            writer.answered(seq, Err(rpc("invalid_request"))).unwrap();
        }
        writer.drained().unwrap();
        assert!(!writer.may_send(), "a pause first");
        writer.resume();
        // New input meanwhile waits behind the refused writes.
        typed(&mut queue, t0, "e");
        let again: Vec<Sent> = std::iter::from_fn(|| writer.next(&mut queue, &mut last, 1000, t0))
            .take(5)
            .collect();
        assert_eq!(
            again[..4],
            sent[..],
            "the same bytes at the same offsets, in order"
        );
        assert_eq!((again[4].seq, again[4].data.as_slice()), (4, &b"e"[..]));
        // Acknowledged ones leave the window; a later skip is then an error again.
        for sent in &again {
            writer.answered(sent.seq, Ok(json!({}))).unwrap();
        }
        assert!(writer.unacked.is_empty());
        assert!(writer.answered(9, Err(rpc("invalid_request"))).is_err());
        assert!(writer.answered(9, Err(rpc("not_found"))).is_err());
    }

    #[test]
    fn a_return_is_given_only_the_pause_the_network_has_not_kept_for_it() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let send = |text: &str,
                    typed_at: u64,
                    sent_at: u64,
                    writer: &mut Writer,
                    queue: &mut InputQueue,
                    last: &mut Option<Instant>| {
            typed(queue, ms(typed_at), text);
            writer.next(queue, last, 1000, ms(sent_at)).unwrap()
        };
        // Text, then a Return 100 ms later, each sent as typed: the pause is already there.
        let (mut writer, mut queue, mut last) = (Writer::default(), InputQueue::default(), None);
        send("ls", 0, 1, &mut writer, &mut queue, &mut last);
        assert_eq!(
            send("\r", 100, 101, &mut writer, &mut queue, &mut last).gap_ms,
            0
        );
        // The same, but the text waited for room and went out only just before the Return:
        // the host is asked to keep the 100 ms the person made.
        let (mut writer, mut queue, mut last) = (Writer::default(), InputQueue::default(), None);
        send("ls", 0, 120, &mut writer, &mut queue, &mut last);
        let squeezed = send("\r", 100, 125, &mut writer, &mut queue, &mut last);
        assert_eq!(squeezed.gap_ms, 95);
        // However long the person waited, at most 150 ms is asked for.
        let (mut writer, mut queue, mut last) = (Writer::default(), InputQueue::default(), None);
        send("ls", 0, 5000, &mut writer, &mut queue, &mut last);
        assert_eq!(
            send("\r", 4000, 5001, &mut writer, &mut queue, &mut last).gap_ms,
            149
        );
        // Text never has a gap, and a Return with nothing before it has none either.
        let (mut writer, mut queue, mut last) = (Writer::default(), InputQueue::default(), None);
        assert_eq!(
            send("\r", 100, 101, &mut writer, &mut queue, &mut last).gap_ms,
            0
        );
        assert_eq!(
            send("x", 102, 103, &mut writer, &mut queue, &mut last).gap_ms,
            0
        );
    }

    #[test]
    fn a_host_that_never_catches_up_ends_the_stream() {
        let (mut queue, mut last) = (InputQueue::default(), None);
        let mut writer = Writer::default();
        typed(&mut queue, Instant::now(), "x");
        let sent = writer
            .next(&mut queue, &mut last, 10, Instant::now())
            .unwrap();
        let mut ended = false;
        for _ in 0..200 {
            writer.answered(sent.seq, Err(rpc("pty_limit"))).unwrap();
            if writer.drained().is_err() {
                ended = true;
                break;
            }
            writer.resume();
            writer
                .next(&mut queue, &mut last, 10, Instant::now())
                .unwrap();
        }
        assert!(ended);
    }

    #[test]
    fn read_results_are_checked_and_filed() {
        let stream = "55555555-5555-4555-8555-555555555555";
        let mut r = Reassembler::new();
        let (mut outbox, mut bytes) = (VecDeque::new(), 0usize);
        let data = |seq: u64, text: &[u8]| json!({"stream":stream,"seq":seq,"data":URL_SAFE_NO_PAD.encode(text)});
        take_read(&data(0, b"abc"), stream, &mut r, &mut outbox, &mut bytes).unwrap();
        take_read(&data(3, b"de"), stream, &mut r, &mut outbox, &mut bytes).unwrap();
        assert_eq!((outbox.len(), bytes), (2, 5));
        // A read that waited out and found nothing is not an error.
        take_read(
            &json!({"stream":stream,"seq":5,"data":""}),
            stream,
            &mut r,
            &mut outbox,
            &mut bytes,
        )
        .unwrap();
        assert_eq!(outbox.len(), 2);
        take_read(
            &json!({"stream":stream,"seq":5,"eof":true,"reason":"exited"}),
            stream,
            &mut r,
            &mut outbox,
            &mut bytes,
        )
        .unwrap();
        assert_eq!(r.finished(), Some("exited"));
        // Padded base64 is read too; other streams and broken data are refused.
        take_read(
            &json!({"stream":stream,"seq":5,"data":"Zg=="}),
            stream,
            &mut r,
            &mut outbox,
            &mut bytes,
        )
        .unwrap();
        assert_eq!(outbox.back().unwrap(), b"f");
        for bad in [
            json!({"stream":"66666666-6666-4666-8666-666666666666","seq":0,"data":"AA"}),
            json!({"stream":stream,"seq":0,"data":"!!!"}),
            json!({"stream":stream,"data":"AA"}),
        ] {
            let mut r = Reassembler::new();
            assert!(
                take_read(&bad, stream, &mut r, &mut VecDeque::new(), &mut 0).is_err(),
                "{bad}"
            );
        }
    }
}
