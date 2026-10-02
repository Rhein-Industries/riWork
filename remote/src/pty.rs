//! Terminal streams for a paired desktop ("Desktop terminal extension" in
//! `docs/remote-protocol.md`).
//!
//! Another Mac running RiWork shows this Mac's shells as real terminals. The
//! shell itself keeps living in tmux; what travels is a tmux *client*. A stream
//! is `riwork shell attach SHELL --exec` running on a pseudo-terminal this
//! connector owns, so the bytes are exactly what a Ghostty window here would
//! have been sent: redraws, the alternate screen, mouse and cursor modes, copy
//! mode and the repaint a new client gets, all from tmux. Closing the master
//! hangs the client up; the shell is never touched.
//!
//! - `pty.open` starts the client and answers on its first byte (`Lane::Attach`).
//! - `pty.read` parks until output exists (`Lane::Stream`, no process).
//! - `pty.write`, `pty.resize` and `pty.close` are answered by the connection
//!   loop itself, in arrival order, like `link.configure`: they only touch
//!   memory and the pseudo-terminal, never wait for a process, and a write is
//!   queued for a task of its own so a stuck client can hold up only its stream.
//!
//! The streams of one session live in a [`PtySet`]. The connector ends the set
//! when the session does (a peer change, revocation, any error that closes the
//! connection) and every client process in it is killed and reaped.
use crate::{
    crypto::{b64, uuid},
    lanes,
};
use serde_json::{Map, Value, json};
use std::{
    collections::{HashMap, VecDeque},
    ffi::OsString,
    ops::RangeInclusive,
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};
use tokio::{
    sync::{Notify, watch},
    task::AbortHandle,
    time::{Instant, sleep, timeout},
};

/// Streams one session may hold at once, `pty.open` in flight included.
pub const MAX_STREAMS: usize = 8;
/// `pty.read` requests parked at once (the size of `Lane::Stream`).
pub const MAX_READS: usize = lanes::STREAM_SLOTS;
/// Bytes of one `pty.write`.
pub const MAX_WRITE: usize = 32 * 1024;
/// Bytes of one `pty.read` answer.
pub const MAX_CHUNK: usize = 64 * 1024;
/// The longest a `pty.read` may park.
pub const MAX_WAIT_MS: u64 = 25_000;
/// Largest `columns` and `rows` of a stream.
pub const MAX_COLUMNS: u64 = 1000;
pub const MAX_ROWS: u64 = 500;
/// The most a `pty.write` may ask the host to wait before its bytes.
pub const MAX_GAP_MS: u64 = 1000;
/// What the host actually waits: Codex treats a Return within 150 ms of pasted
/// text as part of the paste, and no client needs more than that.
pub const GAP_CAP: Duration = Duration::from_millis(150);

/// After the first byte of an answer, how long more output is collected.
const COALESCE: Duration = Duration::from_millis(3);
/// Output kept for a client that is slow to read. Past it the pseudo-terminal
/// is no longer read, which holds tmux's client back instead of losing bytes.
const OUTPUT_BACKLOG: usize = 1024 * 1024;
/// Input accepted but not yet written. A client that is ahead of the terminal
/// by this many bytes, or this many writes (a stuck terminal and one byte at a
/// time must not cost more than a full one), is refused (`pty_limit`) and
/// sends the same bytes again.
const INPUT_BACKLOG: usize = 256 * 1024;
const INPUT_BACKLOG_WRITES: usize = 256;
/// A stream whose output, or whose input, has not moved for this long while its
/// buffer is full is ended with the reason `limit`.
const STALL_LIMIT: Duration = Duration::from_secs(30);
/// How long a client may take to draw its first byte.
const OPEN_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a refused attach may take to finish saying why.
const REFUSAL_WAIT: Duration = Duration::from_secs(2);
/// How long a client that began with plain text (not an escape sequence) is
/// given to end before it is taken for a screen.
const PLAIN_TEXT_WAIT: Duration = Duration::from_millis(400);
/// What the CLI's own refusals begin with (they arrive on the terminal). tmux
/// starts with an escape sequence, never with this.
const REFUSAL_PREFIX: &[u8] = b"riwork: ";

/// What `ready` announces to a desktop device (`features.pty`).
pub fn features() -> Value {
    json!({
        "max_streams": MAX_STREAMS,
        "max_reads": MAX_READS,
        "max_write": MAX_WRITE,
        "max_chunk": MAX_CHUNK
    })
}

/// An error answer: the code and the sentence for the client.
#[derive(Debug, PartialEq, Eq)]
pub struct PtyFault {
    pub code: &'static str,
    pub message: String,
}
impl PtyFault {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}
fn invalid(message: impl Into<String>) -> PtyFault {
    PtyFault::new("invalid_request", message)
}
fn gone() -> PtyFault {
    PtyFault::new("not_found", "terminal stream not found")
}

// ---- parameters -----------------------------------------------------------

/// The params object with no field outside `allowed` (so no array, no null).
fn object<'a>(params: &'a Value, allowed: &[&str]) -> Result<&'a Map<String, Value>, PtyFault> {
    let object = params
        .as_object()
        .ok_or_else(|| invalid("params must be an object"))?;
    if let Some(unknown) = object.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(invalid(format!("unknown field {unknown}")));
    }
    Ok(object)
}
fn text<'a>(object: &'a Map<String, Value>, name: &str) -> Result<&'a str, PtyFault> {
    match object.get(name) {
        Some(Value::String(text)) => Ok(text),
        Some(_) => Err(invalid(format!("{name} must be a string"))),
        None => Err(invalid(format!("{name} is required"))),
    }
}
/// A whole number inside `range`; `default` when the field is absent.
fn number(
    object: &Map<String, Value>,
    name: &str,
    range: RangeInclusive<u64>,
    default: Option<u64>,
) -> Result<u64, PtyFault> {
    match object.get(name) {
        None => default.ok_or_else(|| invalid(format!("{name} is required"))),
        Some(value) => value.as_u64().filter(|n| range.contains(n)).ok_or_else(|| {
            invalid(format!(
                "{name} must be a whole number from {} to {}",
                range.start(),
                range.end()
            ))
        }),
    }
}
fn stream_id(object: &Map<String, Value>) -> Result<String, PtyFault> {
    let id = text(object, "stream")?;
    uuid(id).map_err(|_| invalid("stream must be a full lowercase canonical UUID"))?;
    Ok(id.to_owned())
}
fn size(object: &Map<String, Value>) -> Result<(u16, u16), PtyFault> {
    // Both fit u16 by the ranges.
    Ok((
        number(object, "columns", 1..=MAX_COLUMNS, None)? as u16,
        number(object, "rows", 1..=MAX_ROWS, None)? as u16,
    ))
}

/// A validated `pty.open`.
#[derive(Debug, PartialEq, Eq)]
pub struct OpenSpec {
    pub shell: String,
    pub columns: u16,
    pub rows: u16,
    pub term: &'static str,
    pub ignore_size: bool,
}
pub fn open_spec(params: &Value) -> Result<OpenSpec, PtyFault> {
    let object = object(
        params,
        &["shell_id", "columns", "rows", "term", "ignore_size"],
    )?;
    let shell = text(object, "shell_id")?;
    uuid(shell).map_err(|_| invalid("shell_id must be a full lowercase canonical UUID"))?;
    let (columns, rows) = size(object)?;
    let term = match text(object, "term")? {
        "xterm-ghostty" => "xterm-ghostty",
        "xterm-256color" => "xterm-256color",
        _ => return Err(invalid("term must be xterm-ghostty or xterm-256color")),
    };
    let ignore_size = match object.get("ignore_size") {
        None => false,
        Some(Value::Bool(flag)) => *flag,
        Some(_) => return Err(invalid("ignore_size must be a boolean")),
    };
    Ok(OpenSpec {
        shell: shell.to_owned(),
        columns,
        rows,
        term,
        ignore_size,
    })
}

/// The argument vector of the CLI that becomes the tmux client. One argument
/// per value; nothing passes through a shell.
pub fn attach_args(spec: &OpenSpec) -> Vec<String> {
    let mut args: Vec<String> = ["shell", "attach", spec.shell.as_str(), "--exec"]
        .map(String::from)
        .into();
    if spec.ignore_size {
        args.push("--ignore-size".into());
    }
    args
}

/// The only environment the client gets, from the connector's own (`vars`).
/// A LaunchAgent has almost none, and nothing else in it is the client's
/// business: no `TMUX` (the CLI would be refused as nested), no tokens.
///
/// - `PATH`, `HOME`, `LANG`, `LC_*`, and the two variables that point the CLI at
///   the same RiWork state as the connector (`RIWORK_HOME`, `RIWORK_RUNTIME_DIR`);
/// - `TERM` as asked (one of the two allowed values) and `COLORTERM=truecolor`,
///   because the display is a Ghostty; `TERMINFO` only if the connector has one
///   (the CLI decides whether `xterm-ghostty` can be used here, see
///   `attach_terminal` in `src/sessions.rs`);
/// - tmux decides from the locale whether a client speaks UTF-8, so a locale
///   that is not one becomes `LANG=en_US.UTF-8`.
pub fn environment(
    term: &str,
    vars: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    const KEPT: [&str; 6] = [
        "PATH",
        "HOME",
        "LANG",
        "TERMINFO",
        "RIWORK_HOME",
        "RIWORK_RUNTIME_DIR",
    ];
    let mut kept: Vec<(OsString, OsString)> = vars
        .into_iter()
        .filter(|(name, value)| {
            let name = name.to_string_lossy();
            !value.is_empty() && (KEPT.contains(&name.as_ref()) || name.starts_with("LC_"))
        })
        .collect();
    kept.sort();
    let utf8 = |value: &OsString| {
        let value = value.to_string_lossy().to_ascii_lowercase();
        value.contains("utf-8") || value.contains("utf8")
    };
    // POSIX: LC_ALL beats LC_CTYPE beats LANG.
    let effective = ["LC_ALL", "LC_CTYPE", "LANG"].into_iter().find_map(|name| {
        kept.iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value)
    });
    if !effective.is_some_and(utf8) {
        kept.retain(|(name, _)| name != "LANG" && !name.to_string_lossy().starts_with("LC_"));
        kept.push(("LANG".into(), "en_US.UTF-8".into()));
    }
    kept.push(("TERM".into(), term.into()));
    kept.push(("COLORTERM".into(), "truecolor".into()));
    kept
}

/// The error for an attach that ended before it drew anything. `refusal` is what
/// it printed on the terminal, which is the CLI's `riwork: ...` line.
pub fn refusal_fault(refusal: &str, shell: &str) -> PtyFault {
    let line = refusal.lines().next().unwrap_or_default().trim();
    let detail = line.strip_prefix("riwork: ").unwrap_or(line);
    if detail == format!("unknown shell {shell}") {
        PtyFault::new("not_found", "existing shell ID not found")
    } else if detail == format!("shell {shell} has exited") {
        PtyFault::new("not_found", "selected shell is not alive")
    } else if detail.starts_with("can't find session")
        || detail.starts_with("no server running")
        || detail.starts_with("no sessions")
        || detail.starts_with("error connecting to")
    {
        // tmux's own words: the session went away between the CLI's check and tmux's.
        PtyFault::new("not_found", "selected shell is not alive")
    } else if detail.starts_with("Usage: riwork shell attach")
        || detail.starts_with("Unexpected arguments")
    {
        // A CLI from before `--exec`; `capabilities` normally says so first.
        PtyFault::new(
            "cli_error",
            "the installed riwork CLI cannot open terminal streams; update RiWork",
        )
    } else if detail.is_empty() {
        PtyFault::new("cli_error", "the terminal ended before it drew anything")
    } else {
        PtyFault::new("cli_error", crate::log_safe(detail))
    }
}

// ---- one stream -----------------------------------------------------------

/// Why a stream ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndReason {
    /// The client process ended on its own (tmux detached it, or the shell died).
    Exited,
    /// `pty.close`, or the session ended.
    Closed,
    /// The host gave up on a stalled stream (see `STALL_LIMIT`).
    Limit,
}
impl EndReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exited => "exited",
            Self::Closed => "closed",
            Self::Limit => "limit",
        }
    }
}

struct Input {
    bytes: Vec<u8>,
    /// Waited before the first byte, if that byte is a Return.
    gap: Duration,
}
#[derive(Default)]
struct State {
    /// Output that no `pty.read` has taken yet.
    output: VecDeque<u8>,
    /// Bytes handed out so far: the `seq` of `output[0]`.
    taken: u64,
    ended: Option<EndReason>,
    /// Bytes the client has written so far: the `seq` its next write must carry.
    accepted: u64,
    input: VecDeque<Input>,
    /// Bytes of `input`, and of the write in progress.
    input_bytes: usize,
    /// The terminal refused a write (its client hung up): nothing more is taken.
    /// The end of the stream is for the reader to find, so that nothing the
    /// client said last is lost.
    input_closed: bool,
}

/// What a `pty.read` found.
#[derive(Debug, PartialEq, Eq)]
pub enum Taken {
    Data {
        seq: u64,
        bytes: Vec<u8>,
    },
    /// Nothing within the wait.
    Idle {
        seq: u64,
    },
    End {
        seq: u64,
        reason: EndReason,
    },
}

/// What the tasks of a stream and the requests for it share.
struct Shared {
    state: Mutex<State>,
    /// Counts changes of `state` that a parked `pty.read` or an opening stream waits for.
    changed: watch::Sender<u64>,
    /// The output buffer has room again (for the task that reads the terminal).
    space: Notify,
    /// Input is queued (for the task that writes the terminal).
    input: Notify,
    killer: sys::Killer,
}
impl Shared {
    fn new(killer: sys::Killer) -> Self {
        Self {
            state: Mutex::new(State::default()),
            changed: watch::channel(0).0,
            space: Notify::new(),
            input: Notify::new(),
            killer,
        }
    }
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
    fn touch(&self) {
        self.changed.send_modify(|version| *version += 1);
    }
    /// Ends the stream with `reason` unless it ended already (the first reason
    /// stands). Anything but the client ending itself also kills the client.
    fn end(&self, reason: EndReason) {
        {
            let mut state = self.lock();
            if state.ended.is_some() {
                return;
            }
            state.ended = Some(reason);
            if reason != EndReason::Exited {
                // Nobody is left to want it.
                state.output.clear();
            }
            state.input.clear();
            state.input_bytes = 0;
        }
        if reason != EndReason::Exited {
            self.killer.kill();
        }
        self.touch();
        self.space.notify_one();
        self.input.notify_one();
    }

    /// `pty.read`: parks for up to `wait` until output exists or the stream ends.
    async fn take(&self, wait: Duration) -> Taken {
        let deadline = Instant::now() + wait;
        // Subscribing first means no change between the look below and the wait
        // can be missed.
        let mut changes = self.changed.subscribe();
        loop {
            let (ready, wants_more) = {
                let state = self.lock();
                (
                    !state.output.is_empty() || state.ended.is_some(),
                    (1..MAX_CHUNK).contains(&state.output.len()) && state.ended.is_none(),
                )
            };
            if ready {
                if wants_more {
                    // The rest of a burst usually follows the first byte.
                    sleep(COALESCE).await;
                }
                if let Some(taken) = self.take_now() {
                    return taken;
                }
                // Another read took it first.
                continue;
            }
            if timeout(
                deadline.saturating_duration_since(Instant::now()),
                changes.changed(),
            )
            .await
            .is_err()
            {
                return Taken::Idle {
                    seq: self.lock().taken,
                };
            }
        }
    }
    /// The next chunk of output, or the end, or `None` if there is neither.
    fn take_now(&self) -> Option<Taken> {
        let mut state = self.lock();
        let seq = state.taken;
        if !state.output.is_empty() {
            let count = state.output.len().min(MAX_CHUNK);
            let bytes: Vec<u8> = state.output.drain(..count).collect();
            state.taken += count as u64;
            drop(state);
            self.space.notify_one();
            return Some(Taken::Data { seq, bytes });
        }
        state.ended.map(|reason| Taken::End { seq, reason })
    }

    /// `pty.write`: accepts `bytes` at `seq` for the writer task.
    fn accept(&self, seq: u64, bytes: Vec<u8>, gap: Duration) -> Result<(), PtyFault> {
        let mut state = self.lock();
        if state.ended.is_some() || state.input_closed {
            return Err(PtyFault::new("not_found", "terminal stream has ended"));
        }
        if seq < state.accepted {
            return Err(invalid(format!(
                "write seq {seq} was written already; the next is {}",
                state.accepted
            )));
        }
        if seq > state.accepted {
            return Err(invalid(format!(
                "write seq {seq} skips bytes; the next is {}",
                state.accepted
            )));
        }
        if state.input_bytes + bytes.len() > INPUT_BACKLOG
            || state.input.len() >= INPUT_BACKLOG_WRITES
        {
            // `accepted` stays: the same write can be sent again.
            return Err(PtyFault::new(
                "pty_limit",
                "terminal input is backed up; send the write again shortly",
            ));
        }
        state.accepted += bytes.len() as u64;
        state.input_bytes += bytes.len();
        state.input.push_back(Input { bytes, gap });
        drop(state);
        self.input.notify_one();
        Ok(())
    }
}

/// Reads the terminal into the buffer until the client ends.
async fn pump(shared: Arc<Shared>, master: Arc<sys::Master>) {
    let mut chunk = vec![0u8; 16 * 1024];
    loop {
        // A full buffer holds the terminal back; a client that never reads
        // does not hold it for ever.
        while shared.lock().output.len() >= OUTPUT_BACKLOG {
            if shared.lock().ended.is_some() {
                return;
            }
            if timeout(STALL_LIMIT, shared.space.notified()).await.is_err()
                && shared.lock().output.len() >= OUTPUT_BACKLOG
            {
                shared.end(EndReason::Limit);
                return;
            }
        }
        match master.read(&mut chunk).await {
            // 0 on macOS, EIO elsewhere: every holder of the other end is gone.
            Ok(0) | Err(_) => {
                shared.end(EndReason::Exited);
                return;
            }
            Ok(count) => {
                {
                    let mut state = shared.lock();
                    if state.ended.is_some() {
                        return;
                    }
                    state.output.extend(&chunk[..count]);
                }
                shared.touch();
            }
        }
    }
}

/// Writes accepted input to the terminal, in order.
async fn writer(shared: Arc<Shared>, master: Arc<sys::Master>) {
    loop {
        let next = {
            let mut state = shared.lock();
            if state.ended.is_some() {
                return;
            }
            state.input.pop_front()
        };
        let Some(Input { bytes, gap }) = next else {
            shared.input.notified().await;
            continue;
        };
        // Codex reads a Return that follows text by less than 150 ms as part
        // of a paste. The client says how long it was idle; the host waits
        // that long so the Return still means "submit".
        if bytes.first() == Some(&b'\r') && !gap.is_zero() {
            sleep(gap.min(GAP_CAP)).await;
        }
        let mut written = 0;
        while written < bytes.len() {
            match timeout(STALL_LIMIT, master.write(&bytes[written..])).await {
                Ok(Ok(count)) => written += count,
                // The client hung up. Ending the stream here could cut off what
                // it printed last, which the reader has yet to take: it will see
                // the end of the terminal and end the stream itself.
                Ok(Err(_)) => {
                    let mut state = shared.lock();
                    state.input_closed = true;
                    state.input.clear();
                    state.input_bytes = 0;
                    return;
                }
                // Stuck for ever.
                Err(_) => {
                    shared.end(EndReason::Limit);
                    return;
                }
            }
        }
        let mut state = shared.lock();
        state.input_bytes = state.input_bytes.saturating_sub(bytes.len());
    }
}

/// Stops the tasks of a stream with it.
struct Tasks(Vec<AbortHandle>);
impl Drop for Tasks {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

/// One terminal stream: a client process on a pseudo-terminal. Dropping it kills
/// and reaps the client.
pub struct Stream {
    id: String,
    shell: String,
    shared: Arc<Shared>,
    pty: sys::Pty,
    _tasks: Tasks,
}
impl Stream {
    /// Starts `program args` on a new pseudo-terminal of `columns` by `rows`.
    fn spawn(
        shell: &str,
        program: &Path,
        args: &[String],
        env: &[(OsString, OsString)],
        (columns, rows): (u16, u16),
    ) -> std::io::Result<Self> {
        let pty = sys::Pty::spawn(program, args, env, columns, rows)?;
        let shared = Arc::new(Shared::new(pty.killer()));
        let master = pty.master();
        let tasks = Tasks(vec![
            tokio::spawn(pump(shared.clone(), master.clone())).abort_handle(),
            tokio::spawn(writer(shared.clone(), master)).abort_handle(),
        ]);
        Ok(Self {
            id: uuid::Uuid::new_v4().to_string(),
            shell: shell.to_owned(),
            shared,
            pty,
            _tasks: tasks,
        })
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn shell(&self) -> &str {
        &self.shell
    }
    pub async fn read(&self, wait: Duration) -> Taken {
        self.shared.take(wait).await
    }
    pub fn write(&self, seq: u64, bytes: Vec<u8>, gap: Duration) -> Result<(), PtyFault> {
        self.shared.accept(seq, bytes, gap)
    }
    pub fn resize(&self, columns: u16, rows: u16) -> Result<(), PtyFault> {
        if self.shared.lock().ended.is_some() {
            return Err(PtyFault::new("not_found", "terminal stream has ended"));
        }
        self.pty
            .master()
            .resize(columns, rows)
            .map_err(|e| PtyFault::new("cli_error", format!("resize the terminal: {e}")))
    }
    pub fn end(&self, reason: EndReason) {
        self.shared.end(reason);
    }
    pub fn ended(&self) -> Option<EndReason> {
        self.shared.lock().ended
    }

    /// Waits for what the attach does first: draws, or says why it will not.
    ///
    /// A tmux client starts with an escape sequence, so one that does is open
    /// at once. The CLI's own refusals begin `riwork: ` and tmux's own errors
    /// ("can't find session", "missing or unsuitable terminal") are plain text:
    /// those the client prints and exits, so after plain text it is given a
    /// moment to end, and if it does, that text is the answer.
    async fn settle(&self, shell: &str) -> Result<(), PtyFault> {
        let mut changes = self.shared.changed.subscribe();
        let head = loop {
            {
                let state = self.shared.lock();
                let head: Vec<u8> = state
                    .output
                    .iter()
                    .take(REFUSAL_PREFIX.len())
                    .copied()
                    .collect();
                let partial = !head.is_empty()
                    && head.len() < REFUSAL_PREFIX.len()
                    && REFUSAL_PREFIX.starts_with(&head);
                if head.first() == Some(&0x1b) {
                    return Ok(());
                }
                if state.ended.is_some() || (!head.is_empty() && !partial) {
                    break head;
                }
            }
            // Cannot fail: the sender lives in `self`.
            let _ = changes.changed().await;
        };
        // How long to give it to finish what it is printing before taking it
        // for a screen: the CLI's refusal is one line, then it exits.
        let patience = if head.starts_with(REFUSAL_PREFIX) {
            REFUSAL_WAIT
        } else {
            PLAIN_TEXT_WAIT
        };
        let ended = async {
            loop {
                if self.shared.lock().ended.is_some() {
                    return;
                }
                if changes.changed().await.is_err() {
                    return;
                }
            }
        };
        if timeout(patience, ended).await.is_err() {
            // Still running after plain text: a screen that does not start
            // with an escape. The CLI's refusal always ends.
            return if head.starts_with(REFUSAL_PREFIX) {
                Err(refusal_fault(&self.printed(), shell))
            } else {
                Ok(())
            };
        }
        // It ended. Escape sequences in what it printed mean a screen that
        // was drawn and then went away: that is the stream's to report.
        let printed = self.printed();
        if printed.contains('\u{1b}') {
            return Ok(());
        }
        Err(refusal_fault(&printed, shell))
    }
    /// What the client has printed and nobody has read.
    fn printed(&self) -> String {
        let bytes: Vec<u8> = self.shared.lock().output.iter().copied().collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}
impl Drop for Stream {
    fn drop(&mut self) {
        self.shared.end(EndReason::Closed);
    }
}

// ---- the streams of one session -------------------------------------------

#[derive(Default)]
struct Registry {
    /// Set once the session is over: nothing is added any more.
    closed: bool,
    streams: HashMap<String, Arc<Stream>>,
    /// Opens in flight, which count against `MAX_STREAMS`.
    opening: usize,
}

/// The streams of one authenticated session of one desktop device.
#[derive(Default)]
pub struct PtySet {
    registry: Mutex<Registry>,
}
impl PtySet {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    fn lock(&self) -> MutexGuard<'_, Registry> {
        self.registry
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
    /// Streams now, opens in flight not counted.
    pub fn len(&self) -> usize {
        self.lock().streams.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Room for one more stream, or `pty_limit`.
    fn reserve(self: &Arc<Self>) -> Result<Reservation, PtyFault> {
        let mut registry = self.lock();
        if registry.closed {
            return Err(PtyFault::new("not_found", "the session has ended"));
        }
        if registry.streams.len() + registry.opening >= MAX_STREAMS {
            return Err(PtyFault::new(
                "pty_limit",
                format!("at most {MAX_STREAMS} terminal streams at once; close one first"),
            ));
        }
        registry.opening += 1;
        Ok(Reservation { set: self.clone() })
    }
    fn get(&self, id: &str) -> Result<Arc<Stream>, PtyFault> {
        self.lock().streams.get(id).cloned().ok_or_else(gone)
    }
    /// Ends and forgets one stream. Unknown is `not_found`.
    pub fn close(&self, id: &str) -> Result<(), PtyFault> {
        let stream = self.lock().streams.remove(id).ok_or_else(gone)?;
        stream.end(EndReason::Closed);
        Ok(())
    }
    /// The session is over: ends every stream, which kills its client, and
    /// refuses new ones. Parked reads answer `closed`.
    pub fn close_all(&self) {
        let streams = {
            let mut registry = self.lock();
            registry.closed = true;
            std::mem::take(&mut registry.streams)
        };
        for stream in streams.into_values() {
            stream.end(EndReason::Closed);
        }
    }
}
/// A place for a stream that is being opened.
struct Reservation {
    set: Arc<PtySet>,
}
impl Reservation {
    fn commit(self, stream: Stream) -> Result<Arc<Stream>, PtyFault> {
        let stream = Arc::new(stream);
        let mut registry = self.set.lock();
        if registry.closed {
            // `Drop` (this reservation, then the stream) does the rest.
            return Err(PtyFault::new("not_found", "the session has ended"));
        }
        registry.streams.insert(stream.id.clone(), stream.clone());
        Ok(stream)
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        let mut registry = self.set.lock();
        registry.opening = registry.opening.saturating_sub(1);
    }
}

// ---- the five methods -----------------------------------------------------

/// `pty.open`. `cli` is the RiWork executable the connector was given.
pub async fn open(set: &Arc<PtySet>, cli: &Path, spec: OpenSpec) -> Result<Value, PtyFault> {
    let reservation = set.reserve()?;
    let env = environment(spec.term, std::env::vars_os());
    let stream = Stream::spawn(
        &spec.shell,
        cli,
        &attach_args(&spec),
        &env,
        (spec.columns, spec.rows),
    )
    .map_err(|e| {
        PtyFault::new(
            "cli_error",
            format!("start configured RiWork CLI {}: {e}", cli.display()),
        )
    })?;
    match timeout(OPEN_TIMEOUT, stream.settle(&spec.shell)).await {
        Ok(Ok(())) => {}
        Ok(Err(fault)) => return Err(fault),
        Err(_) => {
            return Err(PtyFault::new(
                "cli_error",
                "the terminal did not draw anything in time",
            ));
        }
    }
    let stream = reservation.commit(stream)?;
    Ok(json!({"stream": stream.id(), "shell_id": stream.shell()}))
}

/// `pty.read`. Parks, so it holds no lock while it waits.
pub async fn read(set: &PtySet, params: &Value) -> Result<Value, PtyFault> {
    let object = object(params, &["stream", "wait_ms"])?;
    let id = stream_id(object)?;
    let wait = number(object, "wait_ms", 0..=MAX_WAIT_MS, Some(0))?;
    let stream = set.get(&id)?;
    Ok(match stream.read(Duration::from_millis(wait)).await {
        Taken::Data { seq, bytes } => json!({"stream": id, "seq": seq, "data": b64(&bytes)}),
        // Nothing yet: empty data, and the seq to expect next.
        Taken::Idle { seq } => json!({"stream": id, "seq": seq, "data": ""}),
        Taken::End { seq, reason } => {
            json!({"stream": id, "seq": seq, "eof": true, "reason": reason.as_str()})
        }
    })
}

/// base64url without padding, in its one canonical spelling.
fn decode_data(encoded: &str) -> Result<Vec<u8>, PtyFault> {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    let bad = || {
        invalid(format!(
            "data must be 1..={MAX_WRITE} bytes of unpadded base64url"
        ))
    };
    // Bound the work before decoding: 4 characters per 3 bytes.
    if encoded.is_empty() || encoded.len() > MAX_WRITE.div_ceil(3) * 4 {
        return Err(bad());
    }
    let bytes = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| bad())?;
    if bytes.is_empty() || bytes.len() > MAX_WRITE || b64(&bytes) != encoded {
        return Err(bad());
    }
    Ok(bytes)
}

/// `pty.write`, `pty.resize` and `pty.close`: they never wait, so the
/// connection loop answers them itself. `Err` is the answer's error.
pub fn inline(set: &PtySet, method: &str, params: &Value) -> Result<Value, PtyFault> {
    match method {
        "pty.write" => {
            let object = object(params, &["stream", "seq", "data", "gap_ms"])?;
            let id = stream_id(object)?;
            let seq = number(object, "seq", 0..=u64::MAX, None)?;
            let bytes = decode_data(text(object, "data")?)?;
            let gap = number(object, "gap_ms", 0..=MAX_GAP_MS, Some(0))?;
            set.get(&id)?
                .write(seq, bytes, Duration::from_millis(gap))?;
            Ok(json!({"stream": id, "seq": seq, "status": "written"}))
        }
        "pty.resize" => {
            let object = object(params, &["stream", "columns", "rows"])?;
            let id = stream_id(object)?;
            let (columns, rows) = size(object)?;
            set.get(&id)?.resize(columns, rows)?;
            Ok(json!({"stream": id, "status": "resized"}))
        }
        "pty.close" => {
            let object = object(params, &["stream"])?;
            let id = stream_id(object)?;
            set.close(&id)?;
            Ok(json!({"stream": id, "status": "closed"}))
        }
        _ => Err(invalid("unsupported RPC method")),
    }
}

// ---- the pseudo-terminal --------------------------------------------------

#[cfg(unix)]
mod sys {
    use std::{
        ffi::OsString,
        io,
        os::{
            fd::{AsRawFd, FromRawFd, OwnedFd},
            unix::process::CommandExt,
        },
        path::Path,
        process::{Child, Command, Stdio},
        sync::{Arc, Mutex},
    };
    use tokio::io::unix::AsyncFd;

    fn check(result: libc::c_int) -> io::Result<libc::c_int> {
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(result)
        }
    }

    /// The master end: where the client's output is read and its input written.
    pub struct Master(AsyncFd<OwnedFd>);
    impl Master {
        pub async fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
            loop {
                let mut ready = self.0.readable().await?;
                let result = ready.try_io(|fd| {
                    // SAFETY: `buf` is valid for `buf.len()` bytes.
                    let count =
                        unsafe { libc::read(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
                    if count < 0 {
                        Err(io::Error::last_os_error())
                    } else {
                        Ok(count as usize)
                    }
                });
                if let Ok(result) = result {
                    return result;
                }
            }
        }
        pub async fn write(&self, data: &[u8]) -> io::Result<usize> {
            loop {
                let mut ready = self.0.writable().await?;
                let result = ready.try_io(|fd| {
                    // SAFETY: `data` is valid for `data.len()` bytes.
                    let count =
                        unsafe { libc::write(fd.as_raw_fd(), data.as_ptr().cast(), data.len()) };
                    if count < 0 {
                        Err(io::Error::last_os_error())
                    } else {
                        Ok(count as usize)
                    }
                });
                if let Ok(result) = result {
                    return result;
                }
            }
        }
        /// TIOCSWINSZ: the client gets SIGWINCH and reads the new size.
        pub fn resize(&self, columns: u16, rows: u16) -> io::Result<()> {
            let size = libc::winsize {
                ws_row: rows,
                ws_col: columns,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            // SAFETY: TIOCSWINSZ reads one `winsize`.
            check(unsafe { libc::ioctl(self.0.as_raw_fd(), libc::TIOCSWINSZ as _, &size) })?;
            Ok(())
        }
    }

    /// Kills the client's process group, once and never after it is reaped (the
    /// process id could be someone else's by then).
    #[derive(Clone)]
    pub struct Killer(Arc<Mutex<Option<libc::pid_t>>>);
    impl Killer {
        #[cfg(test)]
        pub fn none() -> Self {
            Self(Arc::new(Mutex::new(None)))
        }
        pub fn kill(&self) {
            if let Some(pid) = *self.0.lock().unwrap_or_else(|p| p.into_inner()) {
                // SAFETY: plain system call; the group is ours until `Pty` drops.
                unsafe { libc::killpg(pid, libc::SIGKILL) };
            }
        }
        fn retire(&self) {
            let mut pid = self.0.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(pid) = pid.take() {
                // SAFETY: as above; the child is not reaped before this returns.
                unsafe { libc::killpg(pid, libc::SIGKILL) };
            }
        }
    }

    /// A process on a pseudo-terminal. It is the leader of a session of its own
    /// with the terminal as controlling terminal, so closing the master hangs it
    /// up, and so would the end of this process.
    pub struct Pty {
        master: Arc<Master>,
        killer: Killer,
        child: Option<Child>,
    }
    impl Pty {
        pub fn spawn(
            program: &Path,
            args: &[String],
            env: &[(OsString, OsString)],
            columns: u16,
            rows: u16,
        ) -> io::Result<Self> {
            let (master, slave) = open_pair(columns, rows)?;
            // Registered before the child exists: if this fails there is no
            // process to leave behind.
            let master = Arc::new(Master(AsyncFd::new(master)?));
            let mut command = Command::new(program);
            command
                .args(args)
                .env_clear()
                .envs(env.iter().map(|(name, value)| (name, value)))
                .current_dir("/")
                .stdin(Stdio::from(slave.try_clone()?))
                .stdout(Stdio::from(slave.try_clone()?))
                .stderr(Stdio::from(slave));
            // SAFETY: only async-signal-safe calls between fork and exec.
            unsafe {
                command.pre_exec(|| {
                    check(libc::setsid())?;
                    // Standard input is the slave by now.
                    check(libc::ioctl(0, libc::TIOCSCTTY as _, 0))?;
                    Ok(())
                });
            }
            let child = command.spawn()?;
            // The parent's copies of the slave went with `command`.
            drop(command);
            let killer = Killer(Arc::new(Mutex::new(Some(child.id() as libc::pid_t))));
            Ok(Self {
                master,
                killer,
                child: Some(child),
            })
        }
        pub fn master(&self) -> Arc<Master> {
            self.master.clone()
        }
        pub fn killer(&self) -> Killer {
            self.killer.clone()
        }
    }
    impl Drop for Pty {
        fn drop(&mut self) {
            self.killer.retire();
            if let Some(mut child) = self.child.take() {
                // Reaped on a thread of its own: the kill is on its way, but
                // this may run on an async worker.
                let reaper =
                    std::thread::Builder::new()
                        .name("pty-reaper".into())
                        .spawn(move || {
                            let _ = child.wait();
                        });
                drop(reaper);
            }
        }
    }

    /// A master and slave of `columns` by `rows` cells, both close-on-exec from the
    /// start (not `openpty`, which would leave a moment in which a process started
    /// by another thread inherits them: the terminal would then not hang up when
    /// its master closes), the master non-blocking.
    fn open_pair(columns: u16, rows: u16) -> io::Result<(OwnedFd, OwnedFd)> {
        // SAFETY: plain system calls on descriptors this function owns.
        let master = unsafe {
            OwnedFd::from_raw_fd(check(libc::posix_openpt(
                libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
            ))?)
        };
        let fd = master.as_raw_fd();
        check(unsafe { libc::grantpt(fd) })?;
        check(unsafe { libc::unlockpt(fd) })?;
        let path = slave_path(&master)?;
        let slave = unsafe {
            OwnedFd::from_raw_fd(check(libc::open(
                path.as_ptr(),
                libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
            ))?)
        };
        let size = libc::winsize {
            ws_row: rows,
            ws_col: columns,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        check(unsafe { libc::ioctl(fd, libc::TIOCSWINSZ as _, &size) })?;
        let flags = check(unsafe { libc::fcntl(fd, libc::F_GETFL) })?;
        check(unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) })?;
        Ok((master, slave))
    }

    /// The device file of the slave that goes with `master`.
    #[cfg(target_vendor = "apple")]
    fn slave_path(master: &OwnedFd) -> io::Result<std::ffi::CString> {
        let mut name = [0 as libc::c_char; 128];
        // SAFETY: TIOCPTYGNAME writes a NUL-terminated name of at most 128 bytes.
        check(unsafe {
            libc::ioctl(
                master.as_raw_fd(),
                libc::TIOCPTYGNAME as _,
                name.as_mut_ptr(),
            )
        })?;
        // SAFETY: NUL-terminated by the call above.
        Ok(unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }.to_owned())
    }
    #[cfg(not(target_vendor = "apple"))]
    fn slave_path(master: &OwnedFd) -> io::Result<std::ffi::CString> {
        let mut name = [0 as libc::c_char; 128];
        // SAFETY: ptsname_r writes a NUL-terminated name of at most its length.
        let rc = unsafe { libc::ptsname_r(master.as_raw_fd(), name.as_mut_ptr(), name.len()) };
        if rc != 0 {
            return Err(io::Error::from_raw_os_error(rc));
        }
        // SAFETY: NUL-terminated by the call above.
        Ok(unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }.to_owned())
    }
}

/// No pseudo-terminals here: streams cannot be opened.
#[cfg(not(unix))]
mod sys {
    use std::{ffi::OsString, io, path::Path, sync::Arc};

    fn unsupported() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "terminal streams need a Unix system",
        )
    }
    pub struct Master;
    impl Master {
        pub async fn read(&self, _: &mut [u8]) -> io::Result<usize> {
            Err(unsupported())
        }
        pub async fn write(&self, _: &[u8]) -> io::Result<usize> {
            Err(unsupported())
        }
        pub fn resize(&self, _: u16, _: u16) -> io::Result<()> {
            Err(unsupported())
        }
    }
    #[derive(Clone)]
    pub struct Killer;
    impl Killer {
        #[cfg(test)]
        pub fn none() -> Self {
            Self
        }
        pub fn kill(&self) {}
    }
    pub struct Pty;
    impl Pty {
        pub fn spawn(
            _: &Path,
            _: &[String],
            _: &[(OsString, OsString)],
            _: u16,
            _: u16,
        ) -> io::Result<Self> {
            Err(unsupported())
        }
        pub fn master(&self) -> Arc<Master> {
            Arc::new(Master)
        }
        pub fn killer(&self) -> Killer {
            Killer
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fault(result: Result<impl std::fmt::Debug, PtyFault>) -> (&'static str, String) {
        let fault = result.unwrap_err();
        (fault.code, fault.message)
    }
    fn uuid4() -> String {
        uuid::Uuid::new_v4().to_string()
    }

    #[test]
    fn open_takes_exactly_its_fields_within_their_limits() {
        let shell = uuid4();
        let ok = |extra: Value| {
            let mut params =
                json!({"shell_id":shell,"columns":80,"rows":24,"term":"xterm-ghostty"});
            params
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            open_spec(&params)
        };
        let spec = ok(json!({})).unwrap();
        assert_eq!(
            spec,
            OpenSpec {
                shell: shell.clone(),
                columns: 80,
                rows: 24,
                term: "xterm-ghostty",
                ignore_size: false
            }
        );
        assert!(ok(json!({"ignore_size":true})).unwrap().ignore_size);
        assert_eq!(
            ok(json!({"columns":1000,"rows":500})).unwrap().columns,
            1000
        );
        assert_eq!(ok(json!({"columns":1,"rows":1})).unwrap().rows, 1);
        assert_eq!(
            ok(json!({"term":"xterm-256color"})).unwrap().term,
            "xterm-256color"
        );

        for bad in [
            json!({"columns":0}),
            json!({"columns":1001}),
            json!({"rows":0}),
            json!({"rows":501}),
            json!({"columns":-1}),
            json!({"columns":80.5}),
            json!({"columns":"80"}),
            json!({"columns":null}),
            json!({"term":"vt100"}),
            json!({"term":"xterm"}),
            json!({"term":null}),
            json!({"ignore_size":1}),
            json!({"ignore_size":null}),
            json!({"shell_id":"nope"}),
            json!({"shell_id":shell.to_uppercase()}),
            json!({"shell_id":7}),
            json!({"extra":true}),
        ] {
            assert_eq!(fault(ok(bad.clone())).0, "invalid_request", "{bad}");
        }
        // Every field is needed, and nothing but an object will do.
        for missing in ["shell_id", "columns", "rows", "term"] {
            let mut params =
                json!({"shell_id":shell,"columns":80,"rows":24,"term":"xterm-ghostty"});
            params.as_object_mut().unwrap().remove(missing);
            assert_eq!(fault(open_spec(&params)).0, "invalid_request", "{missing}");
        }
        for params in [
            json!(null),
            json!([]),
            json!("x"),
            json!([shell, 80, 24, "xterm-ghostty"]),
        ] {
            assert_eq!(fault(open_spec(&params)).0, "invalid_request", "{params}");
        }
    }

    #[test]
    fn the_attach_command_line_is_one_argument_per_value() {
        let shell = uuid4();
        let mut spec = OpenSpec {
            shell: shell.clone(),
            columns: 80,
            rows: 24,
            term: "xterm-256color",
            ignore_size: false,
        };
        assert_eq!(attach_args(&spec), ["shell", "attach", &shell, "--exec"]);
        spec.ignore_size = true;
        assert_eq!(
            attach_args(&spec),
            ["shell", "attach", &shell, "--exec", "--ignore-size"]
        );
    }

    #[test]
    fn the_client_gets_an_allowlisted_environment_with_a_utf8_locale() {
        let vars = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(n, v)| (OsString::from(n), OsString::from(v)))
                .collect::<Vec<_>>()
        };
        let lookup = |env: &[(OsString, OsString)], name: &str| {
            env.iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.to_string_lossy().into_owned())
        };
        let env = environment(
            "xterm-ghostty",
            vars(&[
                ("PATH", "/usr/bin:/bin"),
                ("HOME", "/Users/me"),
                ("LANG", "de_DE.UTF-8"),
                ("LC_TIME", "en_GB.UTF-8"),
                ("RIWORK_HOME", "/tmp/rwA"),
                ("RIWORK_RUNTIME_DIR", "/tmp/rwA/run"),
                ("TERMINFO", "/opt/terminfo"),
                ("TMUX", "/tmp/tmux-501/default,1,0"),
                ("TMUX_TMPDIR", "/tmp"),
                ("SSH_AUTH_SOCK", "/tmp/agent"),
                ("OPENAI_API_KEY", "secret"),
                ("RIWORK_CLI", "/x/riwork"),
                ("TERM", "dumb"),
                ("COLORTERM", "no"),
            ]),
        );
        let names: Vec<_> = env
            .iter()
            .map(|(n, _)| n.to_string_lossy().into_owned())
            .collect();
        for kept in [
            "PATH",
            "HOME",
            "LANG",
            "LC_TIME",
            "RIWORK_HOME",
            "RIWORK_RUNTIME_DIR",
            "TERMINFO",
        ] {
            assert!(names.contains(&kept.to_owned()), "{kept} in {names:?}");
        }
        for dropped in [
            "TMUX",
            "TMUX_TMPDIR",
            "SSH_AUTH_SOCK",
            "OPENAI_API_KEY",
            "RIWORK_CLI",
        ] {
            assert!(
                !names.contains(&dropped.to_owned()),
                "{dropped} in {names:?}"
            );
        }
        // The asked-for terminal wins over the connector's own.
        assert_eq!(lookup(&env, "TERM").as_deref(), Some("xterm-ghostty"));
        assert_eq!(lookup(&env, "COLORTERM").as_deref(), Some("truecolor"));
        assert_eq!(names.iter().filter(|n| *n == "TERM").count(), 1);
        assert_eq!(lookup(&env, "LANG").as_deref(), Some("de_DE.UTF-8"));

        // No usable locale: tmux would take the client for a non-UTF-8 one.
        for locale in [
            vars(&[]),
            vars(&[("LANG", "C")]),
            vars(&[("LANG", "en_US.UTF-8"), ("LC_ALL", "C")]),
            vars(&[("LC_CTYPE", "POSIX")]),
        ] {
            let env = environment("xterm-256color", locale);
            assert_eq!(lookup(&env, "LANG").as_deref(), Some("en_US.UTF-8"));
            assert_eq!(lookup(&env, "LC_ALL"), None);
            assert_eq!(lookup(&env, "LC_CTYPE"), None);
        }
        // LC_ALL decides over LANG.
        let env = environment(
            "xterm-256color",
            vars(&[("LANG", "C"), ("LC_ALL", "fr_FR.UTF-8")]),
        );
        assert_eq!(lookup(&env, "LANG").as_deref(), Some("C"));
        assert_eq!(lookup(&env, "LC_ALL").as_deref(), Some("fr_FR.UTF-8"));
    }

    #[test]
    fn a_refused_attach_is_told_in_the_codes_the_phone_extensions_use() {
        let shell = uuid4();
        let said = |line: &str| refusal_fault(line, &shell);
        assert_eq!(
            said(&format!("riwork: unknown shell {shell}\r\n")),
            PtyFault::new("not_found", "existing shell ID not found")
        );
        assert_eq!(
            said(&format!("riwork: shell {shell} has exited\r\n")),
            PtyFault::new("not_found", "selected shell is not alive")
        );
        // Another shell's id is not this one's.
        let other = said(&format!("riwork: unknown shell {}\r\n", uuid4()));
        assert_eq!(other.code, "cli_error");
        assert_eq!(
            said("riwork: Usage: riwork shell attach ID").code,
            "cli_error"
        );
        assert!(
            said("Usage: riwork shell attach ID\r\n")
                .message
                .contains("update RiWork")
        );
        assert_eq!(
            said("").message,
            "the terminal ended before it drew anything"
        );
        // tmux's own words: the session went away after the CLI looked.
        for tmux in [
            format!("can't find session: {shell}\r\n"),
            "no server running on /tmp/tmux-501/riwork-0123\r\n".to_owned(),
            "no sessions\r\n".to_owned(),
            "error connecting to /tmp/tmux-501/x (No such file or directory)\r\n".to_owned(),
        ] {
            assert_eq!(
                said(&tmux),
                PtyFault::new("not_found", "selected shell is not alive"),
                "{tmux:?}"
            );
        }
        assert_eq!(
            said("missing or unsuitable terminal: xterm-ghostty\r\n"),
            PtyFault::new("cli_error", "missing or unsuitable terminal: xterm-ghostty")
        );
        // Only the first line, without control characters.
        let long = said("riwork: tmux: server\u{1b}[31m failed\r\nsecond line\r\n");
        assert_eq!(long.code, "cli_error");
        assert!(
            !long.message.contains('\u{1b}') && !long.message.contains("second"),
            "{long:?}"
        );
    }

    #[test]
    fn writes_must_arrive_at_the_running_offset_without_gaps_or_repeats() {
        let shared = Shared::new(sys::Killer::none());
        let write = |seq: u64, len: usize| shared.accept(seq, vec![b'x'; len], Duration::ZERO);
        write(0, 5).unwrap();
        // The same bytes again, or an earlier offset: a duplicate.
        let (code, message) = fault(write(0, 5));
        assert_eq!(code, "invalid_request");
        assert!(
            message.contains("written already") && message.contains("next is 5"),
            "{message}"
        );
        assert_eq!(fault(write(3, 2)).0, "invalid_request");
        // Skipping ahead leaves a gap.
        let (code, message) = fault(write(6, 1));
        assert_eq!(code, "invalid_request");
        assert!(
            message.contains("skips") && message.contains("next is 5"),
            "{message}"
        );
        // Neither moved the offset.
        write(5, 3).unwrap();
        write(8, 1).unwrap();
        assert_eq!(shared.lock().accepted, 9);
        assert_eq!(shared.lock().input.len(), 3);
    }

    #[test]
    fn a_client_too_far_ahead_is_told_pty_limit_and_may_send_the_same_write_again() {
        let shared = Shared::new(sys::Killer::none());
        let chunk = MAX_WRITE;
        let mut seq = 0u64;
        while shared.lock().input_bytes + chunk <= INPUT_BACKLOG {
            shared
                .accept(seq, vec![b'x'; chunk], Duration::ZERO)
                .unwrap();
            seq += chunk as u64;
        }
        let (code, _) = fault(shared.accept(seq, vec![b'x'; chunk], Duration::ZERO));
        assert_eq!(code, "pty_limit");
        assert_eq!(
            shared.lock().accepted,
            seq,
            "the refused write is not counted"
        );
        // Room again once the writer has gone through some.
        {
            let mut state = shared.lock();
            state.input.pop_front();
            state.input_bytes -= chunk;
        }
        shared
            .accept(seq, vec![b'x'; chunk], Duration::ZERO)
            .unwrap();
    }

    #[test]
    fn a_stuck_terminal_costs_a_bounded_number_of_writes_even_if_each_is_one_byte() {
        let shared = Shared::new(sys::Killer::none());
        for seq in 0..INPUT_BACKLOG_WRITES as u64 {
            shared.accept(seq, vec![b'x'], Duration::ZERO).unwrap();
        }
        let next = INPUT_BACKLOG_WRITES as u64;
        assert_eq!(
            fault(shared.accept(next, vec![b'x'], Duration::ZERO)).0,
            "pty_limit"
        );
        assert_eq!(
            shared.lock().accepted,
            next,
            "the refused write is not counted"
        );
        shared.lock().input.pop_front();
        shared.accept(next, vec![b'x'], Duration::ZERO).unwrap();
    }

    #[test]
    fn a_terminal_that_refuses_input_takes_no_more_but_does_not_end_the_stream() {
        // The reader finds the end, so what the client printed last is not lost.
        let shared = Shared::new(sys::Killer::none());
        shared.accept(0, b"a".to_vec(), Duration::ZERO).unwrap();
        {
            let mut state = shared.lock();
            state.input_closed = true;
            state.input.clear();
            state.input_bytes = 0;
        }
        assert_eq!(
            fault(shared.accept(1, b"b".to_vec(), Duration::ZERO)).0,
            "not_found"
        );
        assert_eq!(shared.lock().ended, None);
    }

    #[test]
    fn nothing_is_accepted_after_the_end() {
        let shared = Shared::new(sys::Killer::none());
        shared.accept(0, b"a".to_vec(), Duration::ZERO).unwrap();
        shared.end(EndReason::Closed);
        assert_eq!(
            fault(shared.accept(1, b"b".to_vec(), Duration::ZERO)).0,
            "not_found"
        );
        assert!(shared.lock().input.is_empty());
        // The first reason stands.
        shared.end(EndReason::Limit);
        assert_eq!(shared.lock().ended, Some(EndReason::Closed));
    }

    #[tokio::test]
    async fn reads_hand_out_bytes_in_order_with_their_offsets_and_drain_before_they_end() {
        let shared = Shared::new(sys::Killer::none());
        // Idle: nothing within the wait, and the offset to expect.
        assert_eq!(
            shared.take(Duration::from_millis(5)).await,
            Taken::Idle { seq: 0 }
        );
        shared.lock().output.extend(b"hello");
        assert_eq!(
            shared.take(Duration::ZERO).await,
            Taken::Data {
                seq: 0,
                bytes: b"hello".to_vec()
            }
        );
        // A burst bigger than one chunk comes out in chunks of MAX_CHUNK.
        shared.lock().output.extend(vec![b'z'; MAX_CHUNK + 10]);
        let Taken::Data { seq, bytes } = shared.take(Duration::ZERO).await else {
            panic!("data")
        };
        assert_eq!((seq, bytes.len()), (5, MAX_CHUNK));
        let Taken::Data { seq, bytes } = shared.take(Duration::ZERO).await else {
            panic!("data")
        };
        assert_eq!((seq, bytes.len()), (5 + MAX_CHUNK as u64, 10));
        // The client exiting leaves what it said to be read first.
        shared.lock().output.extend(b"bye");
        shared.end(EndReason::Exited);
        let end = 5 + MAX_CHUNK as u64 + 10;
        assert_eq!(
            shared.take(Duration::ZERO).await,
            Taken::Data {
                seq: end,
                bytes: b"bye".to_vec()
            }
        );
        assert_eq!(
            shared.take(Duration::ZERO).await,
            Taken::End {
                seq: end + 3,
                reason: EndReason::Exited
            }
        );
        // And again: the end is not used up.
        assert_eq!(
            shared.take(Duration::from_secs(5)).await,
            Taken::End {
                seq: end + 3,
                reason: EndReason::Exited
            }
        );
    }

    #[tokio::test]
    async fn a_parked_read_wakes_for_output_and_for_the_end() {
        let shared = Arc::new(Shared::new(sys::Killer::none()));
        let parked = {
            let shared = shared.clone();
            tokio::spawn(async move { shared.take(Duration::from_secs(20)).await })
        };
        sleep(Duration::from_millis(30)).await;
        shared.lock().output.extend(b"late");
        shared.touch();
        assert_eq!(
            parked.await.unwrap(),
            Taken::Data {
                seq: 0,
                bytes: b"late".to_vec()
            }
        );

        // Two parked reads both hear the end; a close discards what was unread.
        shared.lock().output.extend(b"unread");
        let reads: Vec<_> = (0..2)
            .map(|_| {
                let shared = shared.clone();
                tokio::spawn(async move {
                    sleep(Duration::from_millis(30)).await;
                    shared.take(Duration::from_secs(20)).await
                })
            })
            .collect();
        shared.end(EndReason::Closed);
        for read in reads {
            assert_eq!(
                read.await.unwrap(),
                Taken::End {
                    seq: 4,
                    reason: EndReason::Closed
                }
            );
        }
    }

    #[test]
    fn write_data_is_canonical_unpadded_base64url_of_one_to_32_kib() {
        assert_eq!(decode_data("aGk").unwrap(), b"hi");
        assert_eq!(decode_data(&b64(&[0xfb, 0xff])).unwrap(), [0xfb, 0xff]);
        assert_eq!(
            decode_data(&b64(&vec![7; MAX_WRITE])).unwrap().len(),
            MAX_WRITE
        );
        for bad in [
            "",
            "aGk=",
            "aG k",
            "+/+/",
            "aGl",
            "a",
            &b64(&vec![7; MAX_WRITE + 1]),
            &"A".repeat(MAX_WRITE.div_ceil(3) * 4 + 4),
        ] {
            assert_eq!(fault(decode_data(bad)).0, "invalid_request", "{bad:.20}");
        }
    }

    #[test]
    fn write_resize_and_close_validate_before_they_look_for_the_stream() {
        let set = PtySet::new();
        let stream = uuid4();
        let cases = [
            (
                "pty.write",
                json!({"stream":stream,"seq":0,"data":"aGk","gap_ms":1001}),
            ),
            ("pty.write", json!({"stream":stream,"seq":-1,"data":"aGk"})),
            ("pty.write", json!({"stream":stream,"seq":"0","data":"aGk"})),
            ("pty.write", json!({"stream":stream,"seq":0})),
            (
                "pty.write",
                json!({"stream":stream,"seq":0,"data":"aGk","gap_ms":null}),
            ),
            (
                "pty.write",
                json!({"stream":stream,"seq":0,"data":"aGk","x":1}),
            ),
            ("pty.write", json!({"stream":"s","seq":0,"data":"aGk"})),
            ("pty.resize", json!({"stream":stream,"columns":0,"rows":24})),
            (
                "pty.resize",
                json!({"stream":stream,"columns":1001,"rows":24}),
            ),
            (
                "pty.resize",
                json!({"stream":stream,"columns":80,"rows":501}),
            ),
            ("pty.resize", json!({"stream":stream,"columns":80})),
            ("pty.close", json!({"stream":stream,"force":true})),
            ("pty.close", json!({})),
            ("pty.close", json!(null)),
            ("pty.nope", json!({})),
        ];
        for (method, params) in cases {
            assert_eq!(
                fault(inline(&set, method, &params)).0,
                "invalid_request",
                "{method} {params}"
            );
        }
        // A well-formed request for a stream that is not there.
        for (method, params) in [
            (
                "pty.write",
                json!({"stream":stream,"seq":0,"data":"aGk","gap_ms":1000}),
            ),
            (
                "pty.resize",
                json!({"stream":stream,"columns":1000,"rows":500}),
            ),
            ("pty.close", json!({"stream":stream})),
        ] {
            assert_eq!(
                fault(inline(&set, method, &params)).0,
                "not_found",
                "{method}"
            );
        }
    }

    #[tokio::test]
    async fn read_validates_its_params_and_knows_only_its_own_streams() {
        let set = PtySet::new();
        let stream = uuid4();
        for params in [
            json!({"stream":stream,"wait_ms":25001}),
            json!({"stream":stream,"wait_ms":-1}),
            json!({"stream":stream,"wait_ms":"5"}),
            json!({"stream":stream,"wait_ms":1.5}),
            json!({"stream":stream,"wait_ms":null}),
            json!({"stream":stream,"seq":0}),
            json!({"wait_ms":5}),
            json!(null),
        ] {
            assert_eq!(
                fault(read(&set, &params).await).0,
                "invalid_request",
                "{params}"
            );
        }
        assert_eq!(
            fault(read(&set, &json!({"stream":stream,"wait_ms":25000})).await).0,
            "not_found"
        );
    }

    #[test]
    fn the_announced_features_are_the_limits_in_force() {
        assert_eq!(
            features(),
            json!({"max_streams":8,"max_reads":12,"max_write":32768,"max_chunk":65536})
        );
    }
}
