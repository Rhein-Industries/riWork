//! The chat host: `riwork chat serve`.
//!
//! One host per `RIWORK_HOME` owns the provider processes of every chat, so
//! chats keep running while app windows reload or quit. It listens on
//! `RIWORK_HOME/run/chat.sock` (mode 600, in a mode-700 directory), drops
//! connections of other users, and a lock file keeps a second host out.
//!
//! **On disk** (`log`): each chat is `RIWORK_HOME/chats/<id>/` with `info.json`
//! (its `ChatInfo`) and `events.jsonl` (its log, one `Envelope` per line).
//!
//! **Lifecycle of a chat.** `Create` persists the chat and starts its driver.
//! Starting the host never starts a provider: every chat is loaded with state
//! `Stopped` (a `Failed` one stays failed), and the first `Send` or `Compact`
//! starts its driver with `resume` set to the saved provider thread id. `Close`
//! (and the `Stop` command) shuts the driver down and keeps the history;
//! `Delete` also removes the directory. Whenever a chat ends, a turn it left open
//! is closed with a `TurnCompleted` first, so no view shows a turn that cannot
//! finish.
//!
//! **Events.** One reader thread per driver appends what the driver sends to the
//! log (the line number is the `seq`), and broadcasts it to the chat's
//! subscribers. A driver's `Info` is merged into the host's own `ChatInfo` (the
//! driver only knows the thread id and model), and the host publishes its own
//! `Info` whenever a field other than the state changes; state changes travel as
//! `State` events, which `Transcript` folds into the info too. A subscriber
//! reads from a bounded queue: one that falls too far behind is dropped, and
//! resubscribes with the `seq` it last saw.
//!
//! **Lifetime.** `ensure` starts the host detached (own session, output to
//! `run/chat.log`). The host exits by itself when no client has been connected
//! and no chat has been starting, running or waiting for `DEFAULT_IDLE`; the
//! next `ensure` starts it again.

use super::client;
use super::driver::{Driver, DriverConfig, StartDriver};
use super::log::{self, ChatLog};
use super::model::{
    ChatCommand, ChatEvent, ChatInfo, ChatState, Decision, NewChat, Provider, TurnOutcome,
};
use super::wire::{Envelope, Request, Response};
use fs2::FileExt;
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// How long the host stays up with no client and no chat at work.
pub const DEFAULT_IDLE: Duration = Duration::from_secs(15 * 60);
/// A macOS `sockaddr_un` holds 104 bytes including the terminating NUL.
const MAX_SOCKET_PATH: usize = 103;
/// The longest request line: a message to a chat may be long, a megabyte of
/// pasted text is not unusual, but nothing needs more than this.
const MAX_REQUEST: usize = 8 << 20;
const MAX_CONNECTIONS: usize = 256;
const MAX_TITLE: usize = 120;
/// How long a stopping chat waits for its driver's last events.
const DRAIN: Duration = Duration::from_secs(2);
/// How long `ensure` waits for a host it started to answer.
const START_WAIT: Duration = Duration::from_secs(10);

// ---- Configuration -------------------------------------------------------------

/// What the host needs from the outside world. `system()` is the real thing;
/// tests replace the drivers (a `StartDriver` is a plain function) and the
/// machine-dependent resolution of programs and accounts.
#[derive(Clone, Copy)]
pub struct Providers {
    pub codex: StartDriver,
    pub claude: StartDriver,
    /// The Codex account a new chat of this provider and project runs under.
    pub account: AccountFor,
    /// How to start the provider process of a chat, resuming a thread if given.
    pub config: ConfigureDriver,
}

/// `(RIWORK_HOME, provider, project id)` to a Codex account id, if any.
pub type AccountFor = fn(&Path, Provider, Option<&str>) -> Result<Option<String>, String>;
/// `(RIWORK_HOME, chat, thread to resume)` to the driver's configuration.
pub type ConfigureDriver = fn(&Path, &ChatInfo, Option<String>) -> Result<DriverConfig, String>;

impl Providers {
    pub fn system() -> Self {
        Self {
            codex: super::codex::start,
            claude: super::claude::start,
            account: super::launch::account_for,
            config: super::launch::driver_config,
        }
    }

    fn start_for(&self, provider: Provider) -> StartDriver {
        match provider {
            Provider::Codex => self.codex,
            Provider::Claude => self.claude,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Exit after this long with no client and no chat at work.
    pub idle: Duration,
    /// Events a subscriber may have queued before it is dropped.
    pub subscriber_events: usize,
    /// Bytes a subscriber may have queued before it is dropped (one event that
    /// is larger than this alone is still delivered).
    pub subscriber_bytes: usize,
    /// How long a write to a client may block.
    pub write_timeout: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            idle: DEFAULT_IDLE,
            subscriber_events: 4096,
            subscriber_bytes: 64 << 20,
            write_timeout: Duration::from_secs(30),
        }
    }
}

// ---- Paths ----------------------------------------------------------------------

/// Everything the host keeps under `RIWORK_HOME/run/`.
#[derive(Clone, Debug)]
pub struct Paths {
    pub dir: PathBuf,
    pub socket: PathBuf,
    /// Held by the running host.
    pub lock: PathBuf,
    /// Held while `ensure` decides whether to start a host.
    pub ensure_lock: PathBuf,
    pub log: PathBuf,
}

impl Paths {
    pub fn new(home: &Path) -> Result<Self, String> {
        let home = std::path::absolute(home)
            .map_err(|error| format!("Cannot resolve {}: {error}", home.display()))?;
        let socket = client::socket_path(&home);
        if socket.as_os_str().len() > MAX_SOCKET_PATH {
            return Err(format!(
                "the socket path {} is longer than the {MAX_SOCKET_PATH} bytes a Unix socket allows; use a shorter RIWORK_HOME",
                socket.display()
            ));
        }
        let dir = home.join("run");
        Ok(Self {
            lock: dir.join("chat.lock"),
            ensure_lock: dir.join("chat.ensure"),
            log: dir.join("chat.log"),
            socket,
            dir,
        })
    }
}

/// The run directory, owner-only. One that exists with looser permissions is
/// tightened: it is ours, and the socket in it must not be reachable.
fn prepare_run_dir(dir: &Path) -> Result<(), String> {
    let fail = |error: io::Error| format!("Cannot prepare {}: {error}", dir.display());
    if let Some(parent) = dir.parent() {
        crate::paths::create_private_dir(parent).map_err(fail)?;
    }
    match fs::symlink_metadata(dir) {
        Ok(meta) => {
            if !meta.is_dir() {
                return Err(format!("{} is not a real directory", dir.display()));
            }
            if meta.uid() != effective_uid() {
                return Err(format!("{} belongs to another user", dir.display()));
            }
            if meta.permissions().mode() & 0o077 != 0 {
                fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(fail)?;
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new()
                .mode(0o700)
                .create(dir)
                .map_err(fail)?;
        }
        Err(error) => return Err(fail(error)),
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
fn try_lock(path: &Path) -> Result<Option<File>, String> {
    let file = open_private(path, false)
        .map_err(|error| format!("Cannot open {}: {error}", path.display()))?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(Some(file)),
        Err(error) if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
            Ok(None)
        }
        Err(error) => Err(format!("Cannot lock {}: {error}", path.display())),
    }
}

fn effective_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panicking connection thread must not take the whole host down with it.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

// ---- A chat in memory ----------------------------------------------------------------

type DriverHandle = Arc<Mutex<Box<dyn Driver>>>;

/// The provider process of a chat while one is wanted.
struct Run {
    /// Which start this is: events of an earlier run are dropped.
    generation: u64,
    /// `None` while `StartDriver` is still running.
    driver: Option<DriverHandle>,
    /// A stop is shutting the driver down; its last events are still logged.
    stopping: bool,
    /// The driver reported that it ended before `StartDriver` returned.
    ended: bool,
    /// Set by the reader thread when the driver's event channel closes.
    reader_done: Arc<AtomicBool>,
}

impl Run {
    fn starting(generation: u64) -> Self {
        Self {
            generation,
            driver: None,
            stopping: false,
            ended: false,
            reader_done: Arc::new(AtomicBool::new(false)),
        }
    }

    fn live(&self) -> bool {
        !self.ended && !self.stopping
    }

    /// Reaps the process of a run that is over, without making the caller wait.
    fn reap(self) {
        if let Some(driver) = self.driver {
            thread::spawn(move || lock(&driver).shutdown());
        }
    }
}

/// The part of a transcript the host needs to end a chat tidily: the turn in
/// progress and the requests waiting for the user. Not the whole `Transcript`:
/// the log has the items, and a host that kept them as well would hold every
/// chat in memory twice.
#[derive(Default)]
struct Open {
    turn: Option<String>,
    approvals: Vec<String>,
    questions: Vec<String>,
}

impl Open {
    fn apply(&mut self, event: &ChatEvent) {
        match event {
            ChatEvent::TurnStarted { turn_id } => self.turn = Some(turn_id.clone()),
            ChatEvent::TurnCompleted { turn_id, .. } => {
                if self.turn.as_ref() == Some(turn_id) {
                    self.turn = None;
                }
                self.approvals.clear();
                self.questions.clear();
            }
            ChatEvent::ApprovalRequested { approval } => {
                self.approvals.retain(|id| *id != approval.request_id);
                self.approvals.push(approval.request_id.clone());
            }
            ChatEvent::ApprovalResolved { request_id, .. } => {
                self.approvals.retain(|id| id != request_id)
            }
            ChatEvent::QuestionRequested { question } => {
                self.questions.retain(|id| *id != question.request_id);
                self.questions.push(question.request_id.clone());
            }
            ChatEvent::QuestionResolved { request_id } => {
                self.questions.retain(|id| id != request_id)
            }
            _ => {}
        }
    }
}

/// A connection that follows a chat's events. The chat offers it each new
/// serialized line; one that cannot take it is dropped, and its connection
/// ends once it has written what it was given.
struct Subscriber {
    tx: SyncSender<Arc<str>>,
    queued: Arc<AtomicUsize>,
    max_bytes: usize,
}

impl Subscriber {
    /// Whether the subscriber stays.
    fn offer(&self, line: &Arc<str>) -> bool {
        let size = line.len();
        let before = self.queued.fetch_add(size, Ordering::SeqCst);
        if before > 0 && before + size > self.max_bytes {
            self.queued.fetch_sub(size, Ordering::SeqCst);
            return false;
        }
        match self.tx.try_send(line.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.queued.fetch_sub(size, Ordering::SeqCst);
                false
            }
        }
    }
}

struct Inner {
    info: ChatInfo,
    log: ChatLog,
    next_seq: u64,
    open: Open,
    subscribers: Vec<Subscriber>,
    run: Option<Run>,
    generations: u64,
    deleted: bool,
    /// The log cannot be written: the chat is over.
    broken: bool,
    /// The reader thread should stop the chat (the log broke).
    stop_wanted: bool,
}

impl Inner {
    fn new(info: ChatInfo, log: ChatLog, last_seq: u64) -> Self {
        Self {
            info,
            log,
            next_seq: last_seq + 1,
            open: Open::default(),
            subscribers: Vec::new(),
            run: None,
            generations: 0,
            deleted: false,
            broken: false,
            stop_wanted: false,
        }
    }

    fn live(&self) -> bool {
        self.run.as_ref().is_some_and(Run::live)
    }

    fn busy(&self) -> bool {
        self.run.is_some()
            && matches!(
                self.info.state,
                ChatState::Starting | ChatState::Running | ChatState::Waiting
            )
    }

    /// Writes `event` to the log, then offers it to the subscribers.
    fn append(&mut self, event: ChatEvent) {
        if self.broken || self.deleted {
            return;
        }
        self.open.apply(&event);
        // Boundaries are worth surviving a power loss; the stream in between is
        // written at once but synced with the next boundary.
        let sync = !matches!(
            event,
            ChatEvent::ItemStarted { .. }
                | ChatEvent::ItemDelta { .. }
                | ChatEvent::ItemCompleted { .. }
                | ChatEvent::Usage { .. }
        );
        let envelope = Envelope {
            chat_id: self.info.id.clone(),
            seq: self.next_seq,
            event,
        };
        let mut line = match serde_json::to_string(&envelope) {
            Ok(line) => line,
            Err(error) => {
                eprintln!("riwork chat: cannot serialize an event: {error}");
                return;
            }
        };
        line.push('\n');
        if let Err(error) = self.log.append(line.as_bytes(), sync) {
            let message = format!("cannot write the chat's event log: {error}");
            eprintln!("riwork chat: {}: {message}", self.info.id);
            self.broken = true;
            self.stop_wanted = true;
            self.info.state = ChatState::Failed { message };
            return;
        }
        self.next_seq += 1;
        let line: Arc<str> = Arc::from(line);
        self.subscribers
            .retain(|subscriber| subscriber.offer(&line));
    }

    fn save_info(&self) {
        // The directory of a deleted chat is going away.
        if self.deleted {
            return;
        }
        if let Err(error) = self.log.save_info(&self.info) {
            eprintln!("riwork chat: {}: {error}", self.info.id);
        }
    }

    /// Records a new state. `info.json` goes first, so after a crash it never
    /// says less than the log does.
    fn set_state(&mut self, state: ChatState) {
        if self.broken || self.info.state == state {
            return;
        }
        self.info.state = state.clone();
        self.save_info();
        self.append(ChatEvent::State { state });
    }

    /// Publishes the info after a change to anything but the state.
    fn publish_info(&mut self) {
        self.save_info();
        self.append(ChatEvent::Info {
            info: self.info.clone(),
        });
    }

    /// What a driver tells about the chat it runs: the thread id and the model.
    fn learn(&mut self, thread: Option<String>, model: Option<String>, effort: Option<String>) {
        let mut changed = false;
        if let Some(thread) = thread.filter(|thread| !thread.is_empty())
            && self.info.provider_thread_id.as_ref() != Some(&thread)
        {
            self.info.provider_thread_id = Some(thread);
            changed = true;
        }
        if let Some(model) = model.filter(|model| !model.is_empty())
            && self.info.model.as_ref() != Some(&model)
        {
            self.info.model = Some(model);
            changed = true;
        }
        if let Some(effort) = effort.filter(|effort| !effort.is_empty())
            && self.info.effort.as_ref() != Some(&effort)
        {
            self.info.effort = Some(effort);
            changed = true;
        }
        if changed {
            self.publish_info();
        }
    }

    /// Ends what the chat has open: the turn in progress, or else the requests
    /// waiting for the user, so a view does not wait for something that cannot
    /// happen any more.
    fn settle(&mut self, outcome: TurnOutcome) {
        if let Some(turn_id) = self.open.turn.clone() {
            self.append(ChatEvent::TurnCompleted { turn_id, outcome });
        }
        for request_id in std::mem::take(&mut self.open.approvals) {
            self.append(ChatEvent::ApprovalResolved {
                request_id,
                decision: Decision::Cancel,
            });
        }
        for request_id in std::mem::take(&mut self.open.questions) {
            self.append(ChatEvent::QuestionResolved { request_id });
        }
    }

    /// Takes in one event of the driver. Returns whether the event says the
    /// provider process is over.
    fn take_driver_event(&mut self, event: ChatEvent) -> bool {
        match event {
            ChatEvent::Info { info } => {
                self.learn(info.provider_thread_id, info.model, info.effort);
                false
            }
            ChatEvent::State {
                state: ChatState::Stopped,
            } => {
                self.settle(TurnOutcome::Interrupted);
                self.set_state(ChatState::Stopped);
                true
            }
            ChatEvent::State {
                state: ChatState::Failed { message },
            } => {
                self.settle(TurnOutcome::Failed {
                    message: message.clone(),
                });
                self.set_state(ChatState::Failed { message });
                true
            }
            ChatEvent::State { state } => {
                self.set_state(state);
                false
            }
            other => {
                self.append(other);
                false
            }
        }
    }
}

struct Chat {
    dir: PathBuf,
    /// Taken for the whole of a start or a stop, so the two never overlap.
    lifecycle: Mutex<()>,
    inner: Mutex<Inner>,
}

impl Chat {
    fn info(&self) -> ChatInfo {
        lock(&self.inner).info.clone()
    }
}

// ---- The host --------------------------------------------------------------------------

struct Shared {
    home: PathBuf,
    paths: Paths,
    providers: Providers,
    options: Options,
    chats: Mutex<HashMap<String, Arc<Chat>>>,
    connections: AtomicUsize,
    /// When a client last connected or left, or a chat was last at work: what
    /// the idle timer counts from (a request is over long before it is polled).
    activity: Mutex<Instant>,
    quit: AtomicBool,
}

impl Shared {
    fn touch(&self) {
        *lock(&self.activity) = Instant::now();
    }

    fn find(&self, chat_id: &str) -> Result<Arc<Chat>, String> {
        lock(&self.chats)
            .get(chat_id)
            .cloned()
            .ok_or_else(|| format!("unknown chat {chat_id}"))
    }

    fn all(&self) -> Vec<Arc<Chat>> {
        lock(&self.chats).values().cloned().collect()
    }

    fn busy_chats(&self) -> usize {
        self.all()
            .iter()
            .filter(|chat| lock(&chat.inner).busy())
            .count()
    }
}

#[derive(Debug)]
pub enum StartError {
    /// Another host holds the lock for this `RIWORK_HOME`.
    AlreadyRunning,
    Failed(String),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyRunning => f.write_str("a chat host already runs for this RIWORK_HOME"),
            Self::Failed(message) => f.write_str(message),
        }
    }
}

impl From<String> for StartError {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}

/// Why `Host::run` returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    /// No client and no chat at work for `Options::idle`.
    Idle,
    /// SIGTERM or SIGINT.
    Signal,
    /// `Stopper::stop`.
    Asked,
}

pub struct Host {
    shared: Arc<Shared>,
    accept: Option<JoinHandle<()>>,
    /// Held for as long as the host lives.
    _lock: File,
    down: bool,
}

/// Lets another thread end `Host::run`.
#[cfg(test)]
#[derive(Clone)]
pub struct Stopper(Arc<Shared>);

#[cfg(test)]
impl Stopper {
    pub fn stop(&self) {
        self.0.quit.store(true, Ordering::SeqCst);
    }
}

static TERMINATE: AtomicBool = AtomicBool::new(false);

extern "C" fn on_terminate(_signal: libc::c_int) {
    TERMINATE.store(true, Ordering::SeqCst);
}

impl Host {
    /// Takes the lock, loads the chats from disk (none is started), and starts
    /// listening.
    pub fn start(home: &Path, providers: Providers, options: Options) -> Result<Self, StartError> {
        let home = std::path::absolute(home)
            .map_err(|error| format!("Cannot resolve {}: {error}", home.display()))?;
        let paths = Paths::new(&home)?;
        prepare_run_dir(&paths.dir)?;
        let Some(lock_file) = try_lock(&paths.lock)? else {
            return Err(StartError::AlreadyRunning);
        };
        // For `ensure` and for whoever wants to signal the host.
        let _ = lock_file.set_len(0);
        let _ = (&lock_file).write_all(format!("{}\n", std::process::id()).as_bytes());
        // The lock is ours, so a socket that is still there is a dead host's.
        if let Ok(meta) = fs::symlink_metadata(&paths.socket) {
            if !meta.file_type().is_socket() {
                return Err(StartError::Failed(format!(
                    "{} exists and is not a socket",
                    paths.socket.display()
                )));
            }
            fs::remove_file(&paths.socket)
                .map_err(|error| format!("Cannot remove {}: {error}", paths.socket.display()))?;
        }
        let chats_dir = log::chats_dir(&home);
        crate::paths::create_private_dir(&chats_dir)
            .map_err(|error| format!("Cannot create {}: {error}", chats_dir.display()))?;
        let chats = load_chats(&home);
        let listener = UnixListener::bind(&paths.socket)
            .map_err(|error| format!("Cannot listen on {}: {error}", paths.socket.display()))?;
        fs::set_permissions(&paths.socket, fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("Cannot protect {}: {error}", paths.socket.display()))?;
        let shared = Arc::new(Shared {
            home,
            paths,
            providers,
            options,
            chats: Mutex::new(chats),
            connections: AtomicUsize::new(0),
            activity: Mutex::new(Instant::now()),
            quit: AtomicBool::new(false),
        });
        let accept = {
            let shared = shared.clone();
            thread::Builder::new()
                .name("chat-accept".into())
                .spawn(move || accept_loop(shared, listener))
                .map_err(|error| format!("Cannot start the accept thread: {error}"))?
        };
        Ok(Self {
            shared,
            accept: Some(accept),
            _lock: lock_file,
            down: false,
        })
    }

    pub fn socket(&self) -> &Path {
        &self.shared.paths.socket
    }

    #[cfg(test)]
    pub fn stopper(&self) -> Stopper {
        Stopper(self.shared.clone())
    }

    /// Blocks until the host has been idle for `Options::idle`, a termination
    /// signal arrives, or a `Stopper` asks.
    pub fn run(&self) -> Exit {
        let idle = self.shared.options.idle;
        let poll = (idle / 4).clamp(Duration::from_millis(10), Duration::from_millis(100));
        loop {
            if TERMINATE.load(Ordering::SeqCst) {
                return Exit::Signal;
            }
            if self.shared.quit.load(Ordering::SeqCst) {
                return Exit::Asked;
            }
            if self.shared.connections.load(Ordering::SeqCst) > 0 || self.shared.busy_chats() > 0 {
                self.shared.touch();
            } else if lock(&self.shared.activity).elapsed() >= idle {
                return Exit::Idle;
            }
            thread::sleep(poll);
        }
    }

    /// Stops accepting, stops every chat's driver (the history stays), and
    /// removes the socket. The lock is released last.
    pub fn shutdown(&mut self) {
        if std::mem::replace(&mut self.down, true) {
            return;
        }
        self.shared.quit.store(true, Ordering::SeqCst);
        // `accept` only returns for a connection: make one.
        let _ = UnixStream::connect(&self.shared.paths.socket);
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
        let chats = self.shared.all();
        thread::scope(|scope| {
            for chat in &chats {
                scope.spawn(move || {
                    stop(chat);
                    lock(&chat.inner).subscribers.clear();
                });
            }
        });
        let _ = fs::remove_file(&self.shared.paths.socket);
        // The pid is of a process that is gone now; it was for signalling it.
        let _ = self._lock.set_len(0);
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Every connection and every provider process takes descriptors, and an app
/// started from the Dock hands down macOS's soft limit of 256.
fn raise_descriptor_limit() {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `limit` is a live local of the type the call fills in or reads.
    unsafe {
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) == 0 {
            // macOS refuses a soft limit above OPEN_MAX even when the hard one is infinite.
            limit.rlim_cur = limit.rlim_max.min(10240);
            libc::setrlimit(libc::RLIMIT_NOFILE, &limit);
        }
    }
}

/// Runs the host in the foreground until it is idle or signalled.
pub fn serve(home: &Path, idle: Duration) -> Result<(), String> {
    let options = Options {
        idle,
        ..Options::default()
    };
    raise_descriptor_limit();
    // SAFETY: the handler only stores to an atomic. A hangup must not end a
    // host whose terminal has closed.
    unsafe {
        libc::signal(
            libc::SIGTERM,
            on_terminate as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGINT,
            on_terminate as *const () as libc::sighandler_t,
        );
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
    }
    let mut host = match Host::start(home, Providers::system(), options) {
        Ok(host) => host,
        Err(StartError::AlreadyRunning) => {
            eprintln!("riwork chat: a chat host already runs for this RIWORK_HOME.");
            return Ok(());
        }
        Err(StartError::Failed(message)) => return Err(message),
    };
    eprintln!(
        "riwork chat: serving {} (exits after {} s without clients or running chats).",
        host.socket().display(),
        idle.as_secs()
    );
    let exit = host.run();
    eprintln!("riwork chat: stopping ({exit:?}).");
    host.shutdown();
    Ok(())
}

// ---- Accepting and serving connections ----------------------------------------------------

struct ConnectionGuard(Arc<Shared>);

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.touch();
        self.0.connections.fetch_sub(1, Ordering::SeqCst);
    }
}

fn accept_loop(shared: Arc<Shared>, listener: UnixListener) {
    for stream in listener.incoming() {
        if shared.quit.load(Ordering::SeqCst) {
            break;
        }
        let Ok(stream) = stream else {
            // Out of descriptors, say: do not spin on it.
            thread::sleep(Duration::from_millis(50));
            continue;
        };
        // Counted before the thread runs, so the idle timer cannot miss a client.
        shared.connections.fetch_add(1, Ordering::SeqCst);
        let guard = ConnectionGuard(shared.clone());
        if shared.connections.load(Ordering::SeqCst) > MAX_CONNECTIONS {
            continue;
        }
        let shared = shared.clone();
        let _ = thread::Builder::new()
            .name("chat-connection".into())
            .spawn(move || serve_connection(&shared, stream, guard));
    }
}

/// The uid of the process on the other end of `stream`.
fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    #[cfg(target_os = "linux")]
    {
        let mut credentials = libc::ucred {
            pid: 0,
            uid: 0,
            gid: 0,
        };
        let mut length = size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: the pointers refer to live locals of the sizes passed.
        let status = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut credentials as *mut libc::ucred).cast(),
                &mut length,
            )
        };
        if status == 0 {
            Ok(credentials.uid)
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let (mut uid, mut gid) = (0, 0);
        // SAFETY: the pointers refer to live locals.
        let status = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
        if status == 0 {
            Ok(uid)
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

fn serve_connection(shared: &Shared, stream: UnixStream, _guard: ConnectionGuard) {
    if !matches!(peer_uid(&stream), Ok(uid) if uid == effective_uid()) {
        return;
    }
    let _ = stream.set_write_timeout(Some(shared.options.write_timeout));
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(read_half);
    let mut writer = stream;
    loop {
        let line = match read_request(&mut reader) {
            Ok(Some(line)) => line,
            Ok(None) => return,
            Err(error) => {
                let _ = send(&mut writer, &failure(String::new(), error.to_string()));
                return;
            }
        };
        let request = match serde_json::from_str::<Request>(&line) {
            Ok(request) => request,
            Err(error) => {
                let id = serde_json::from_str::<serde_json::Value>(&line)
                    .ok()
                    .and_then(|value| value.get("id")?.as_str().map(str::to_owned))
                    .unwrap_or_default();
                let answer = failure(id, format!("unreadable request: {error}"));
                if send(&mut writer, &answer).is_err() {
                    return;
                }
                continue;
            }
        };
        if let Request::Subscribe { id, chat_id, since } = request {
            subscribe(shared, writer, id, &chat_id, since);
            return;
        }
        if send(&mut writer, &dispatch(shared, request)).is_err() {
            return;
        }
    }
}

/// The next request line, without its newline. `None` at the end of the
/// connection.
fn read_request(reader: &mut impl BufRead) -> io::Result<Option<String>> {
    let mut line = Vec::new();
    reader
        .by_ref()
        .take(MAX_REQUEST as u64 + 1)
        .read_until(b'\n', &mut line)?;
    if line.len() > MAX_REQUEST {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("a request is longer than {MAX_REQUEST} bytes"),
        ));
    }
    if line.last() != Some(&b'\n') {
        return Ok(None);
    }
    line.pop();
    String::from_utf8(line)
        .map(Some)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "a request is not UTF-8"))
}

fn send(writer: &mut UnixStream, response: &Response) -> io::Result<()> {
    let mut line = serde_json::to_vec(response).map_err(io::Error::other)?;
    line.push(b'\n');
    writer.write_all(&line)
}

fn success(id: String, result: Option<serde_json::Value>) -> Response {
    Response {
        id,
        ok: true,
        result,
        error: None,
    }
}

fn failure(id: String, error: String) -> Response {
    Response {
        id,
        ok: false,
        result: None,
        error: Some(error),
    }
}

fn answer<T: serde::Serialize>(id: String, result: Result<T, String>) -> Response {
    match result.and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())) {
        Ok(serde_json::Value::Null) => success(id, None),
        Ok(value) => success(id, Some(value)),
        Err(error) => failure(id, error),
    }
}

/// Everything but `Subscribe`, which takes over its connection.
fn dispatch(shared: &Shared, request: Request) -> Response {
    match request {
        Request::Create { id, chat } => answer(id, create(shared, chat)),
        Request::List { id } => answer(id, Ok(list(shared))),
        Request::Command {
            id,
            chat_id,
            command,
        } => answer(
            id,
            shared
                .find(&chat_id)
                .and_then(|chat| run_command(shared, &chat, command)),
        ),
        Request::Close { id, chat_id } => answer(id, shared.find(&chat_id).map(|chat| stop(&chat))),
        Request::Delete { id, chat_id } => answer(id, delete(shared, &chat_id)),
        Request::Subscribe { id, .. } => failure(id, "subscribe takes over a connection".into()),
    }
}

fn list(shared: &Shared) -> Vec<ChatInfo> {
    let mut infos: Vec<ChatInfo> = shared.all().iter().map(|chat| chat.info()).collect();
    infos.sort_by(|a, b| (a.created_at_unix, &a.id).cmp(&(b.created_at_unix, &b.id)));
    infos
}

// ---- Subscriptions ------------------------------------------------------------------------------

/// Answers a `Subscribe`, replays the log from `since`, and then streams live
/// events until the client leaves or the host drops it for falling behind.
fn subscribe(shared: &Shared, mut stream: UnixStream, id: String, chat_id: &str, since: u64) {
    let registered = shared.find(chat_id).and_then(|chat| {
        let mut inner = lock(&chat.inner);
        if inner.deleted {
            return Err(format!("unknown chat {chat_id}"));
        }
        let last = inner.next_seq - 1;
        if since > last {
            return Err(format!(
                "chat {chat_id} has {last} events, so it cannot continue after {since}"
            ));
        }
        let (tx, rx) = mpsc::sync_channel(shared.options.subscriber_events);
        let queued = Arc::new(AtomicUsize::new(0));
        inner.subscribers.push(Subscriber {
            tx,
            queued: queued.clone(),
            max_bytes: shared.options.subscriber_bytes,
        });
        Ok((chat.dir.clone(), rx, queued, last))
    });
    let (dir, rx, queued, last) = match registered {
        Ok(registered) => registered,
        Err(error) => {
            let _ = send(&mut stream, &failure(id, error));
            return;
        }
    };
    if send(&mut stream, &success(id, None)).is_err() {
        return;
    }
    // Events that arrive while the replay is written wait in the queue: nothing
    // in it is older than the replay's last line.
    if let Err(error) = log::replay(&dir, since, last, &mut stream) {
        eprintln!("riwork chat: cannot replay {chat_id}: {error}");
        return;
    }
    loop {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(line) => {
                queued.fetch_sub(line.len(), Ordering::SeqCst);
                if stream.write_all(line.as_bytes()).is_err() {
                    return;
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if client_left(&mut stream) {
                    return;
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Whether the client has closed the connection. A subscriber sends nothing
/// after its request, so anything it does send is ignored.
fn client_left(stream: &mut UnixStream) -> bool {
    if stream.set_nonblocking(true).is_err() {
        return true;
    }
    let mut byte = [0];
    let left = match stream.read(&mut byte) {
        Ok(0) => true,
        Ok(_) => false,
        Err(error) => error.kind() != io::ErrorKind::WouldBlock,
    };
    left || stream.set_nonblocking(false).is_err()
}

// ---- Chats: create, load, delete ------------------------------------------------------------

fn create(shared: &Shared, new: NewChat) -> Result<ChatInfo, String> {
    if !new.cwd.is_absolute() || !new.cwd.is_dir() {
        return Err(format!(
            "the working directory {} is not an existing absolute directory",
            new.cwd.display()
        ));
    }
    if shared.quit.load(Ordering::SeqCst) {
        return Err("the chat host is shutting down".into());
    }
    let account =
        (shared.providers.account)(&shared.home, new.provider, new.project_id.as_deref())?;
    let id = Uuid::new_v4().to_string();
    let title = new
        .title
        .as_deref()
        .map(|title| {
            title
                .chars()
                .filter(|c| !c.is_control())
                .take(MAX_TITLE)
                .collect::<String>()
        })
        .map(|title| title.trim().to_owned())
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| match new.provider {
            Provider::Codex => "Codex chat".to_owned(),
            Provider::Claude => "Claude chat".to_owned(),
        });
    let info = ChatInfo {
        id: id.clone(),
        provider: new.provider,
        project_id: new.project_id,
        worktree_id: new.worktree_id,
        cwd: new.cwd,
        title,
        created_at_unix: now_unix(),
        provider_thread_id: None,
        model: new.model,
        effort: new.effort,
        approval_mode: new.approval_mode,
        codex_account_id: account,
        state: ChatState::Starting,
    };
    let dir = log::chat_dir(&shared.home, &id).ok_or("invalid chat id")?;
    let log = ChatLog::create(&dir, &info)?;
    let mut inner = Inner::new(info.clone(), log, 0);
    inner.append(ChatEvent::Info { info });
    let chat = Arc::new(Chat {
        dir,
        lifecycle: Mutex::new(()),
        inner: Mutex::new(inner),
    });
    lock(&shared.chats).insert(id, chat.clone());
    // A driver that cannot start leaves the chat failed, not missing: its
    // message is in the chat, and the next message tries again.
    let _ = ensure_running(shared, &chat);
    Ok(chat.info())
}

/// Reads every chat from disk. No provider is started: a chat that was running
/// when the last host ended is stopped now, and a turn it left open is closed.
fn load_chats(home: &Path) -> HashMap<String, Arc<Chat>> {
    let mut chats = HashMap::new();
    let Ok(entries) = fs::read_dir(log::chats_dir(home)) else {
        return chats;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(dir) = log::chat_dir(home, &name).filter(|dir| dir.is_dir()) else {
            continue;
        };
        let (chat_log, info, last) = match ChatLog::open(&dir) {
            Ok(opened) => opened,
            Err(error) => {
                eprintln!("riwork chat: skipping {name}: {error}");
                continue;
            }
        };
        if info.id != name {
            eprintln!("riwork chat: skipping {name}: its info.json names another chat");
            continue;
        }
        let was_at_work = matches!(
            info.state,
            ChatState::Starting | ChatState::Running | ChatState::Waiting
        );
        let mut inner = Inner::new(info, chat_log, last);
        if was_at_work {
            // Only a chat that died mid-turn needs its log read whole.
            match log::read_envelopes(&dir) {
                Ok(envelopes) => {
                    for envelope in &envelopes {
                        inner.open.apply(&envelope.event);
                    }
                }
                Err(error) => eprintln!("riwork chat: cannot read the log of {name}: {error}"),
            }
        }
        if !matches!(
            inner.info.state,
            ChatState::Stopped | ChatState::Failed { .. }
        ) {
            inner.settle(TurnOutcome::Interrupted);
            inner.set_state(ChatState::Stopped);
        }
        chats.insert(
            name.clone(),
            Arc::new(Chat {
                dir,
                lifecycle: Mutex::new(()),
                inner: Mutex::new(inner),
            }),
        );
    }
    chats
}

fn delete(shared: &Shared, chat_id: &str) -> Result<(), String> {
    let chat = shared.find(chat_id)?;
    {
        let _turn = lock(&chat.lifecycle);
        stop_locked(&chat);
        let mut inner = lock(&chat.inner);
        inner.deleted = true;
        inner.subscribers.clear();
    }
    lock(&shared.chats).remove(chat_id);
    fs::remove_dir_all(&chat.dir)
        .map_err(|error| format!("Cannot remove {}: {error}", chat.dir.display()))
}

// ---- Driving a chat -----------------------------------------------------------------------------

/// Starts the chat's provider process unless one is running.
fn ensure_running(shared: &Shared, chat: &Arc<Chat>) -> Result<(), String> {
    let _turn = lock(&chat.lifecycle);
    if shared.quit.load(Ordering::SeqCst) {
        return Err("the chat host is shutting down".into());
    }
    let (info, generation, over, reader_done) = {
        let mut inner = lock(&chat.inner);
        if inner.deleted {
            return Err("this chat was deleted".into());
        }
        if inner.broken {
            return Err("this chat's event log cannot be written".into());
        }
        if inner.live() {
            return Ok(());
        }
        // A run that ended is reaped now, so there is only one process.
        let over = inner.run.take();
        inner.generations += 1;
        let generation = inner.generations;
        let run = Run::starting(generation);
        let reader_done = run.reader_done.clone();
        inner.run = Some(run);
        inner.settle(TurnOutcome::Interrupted);
        inner.set_state(ChatState::Starting);
        (inner.info.clone(), generation, over, reader_done)
    };
    if let Some(over) = over {
        over.reap();
    }
    let config = (shared.providers.config)(&shared.home, &info, info.provider_thread_id.clone());
    let config = match config {
        Ok(config) => config,
        Err(error) => return fail_start(chat, generation, error),
    };
    let (events, receiver) = mpsc::channel();
    let reader = {
        let chat = chat.clone();
        thread::Builder::new()
            .name("chat-events".into())
            .spawn(move || read_events(chat, generation, receiver, reader_done))
    };
    if let Err(error) = reader {
        return fail_start(
            chat,
            generation,
            format!("Cannot start the event thread: {error}"),
        );
    }
    match shared.providers.start_for(info.provider)(config, events) {
        Ok(driver) => attach(chat, generation, driver),
        Err(error) => fail_start(chat, generation, error),
    }
}

fn fail_start(chat: &Chat, generation: u64, message: String) -> Result<(), String> {
    let mut inner = lock(&chat.inner);
    if inner
        .run
        .as_ref()
        .is_some_and(|run| run.generation == generation)
    {
        inner.run = None;
    }
    inner.settle(TurnOutcome::Failed {
        message: message.clone(),
    });
    inner.set_state(ChatState::Failed {
        message: message.clone(),
    });
    Err(message)
}

/// Hands a started driver to its run.
fn attach(chat: &Chat, generation: u64, driver: Box<dyn Driver>) -> Result<(), String> {
    let thread = driver.provider_thread_id();
    let handle: DriverHandle = Arc::new(Mutex::new(driver));
    let mut inner = lock(&chat.inner);
    let Some(run) = inner
        .run
        .as_mut()
        .filter(|run| run.generation == generation)
    else {
        drop(inner);
        thread::spawn(move || lock(&handle).shutdown());
        return Err("the chat was stopped while it started".into());
    };
    run.driver = Some(handle);
    let ended = run.ended;
    inner.learn(thread, None, None);
    if ended {
        // The provider was over before `start` returned. If it said how, that
        // is the chat's state; if its channel just closed, the chat fails here.
        let message = match &inner.info.state {
            ChatState::Failed { message } => message.clone(),
            _ => "the provider process ended while it started".to_owned(),
        };
        if let Some(run) = inner.run.take() {
            run.reap();
        }
        if !matches!(
            inner.info.state,
            ChatState::Stopped | ChatState::Failed { .. }
        ) {
            inner.settle(TurnOutcome::Failed {
                message: message.clone(),
            });
            inner.set_state(ChatState::Failed {
                message: message.clone(),
            });
        }
        return Err(message);
    }
    Ok(())
}

/// The reader thread of one run: moves the driver's events into the chat until
/// the driver lets go of its channel.
fn read_events(
    chat: Arc<Chat>,
    generation: u64,
    receiver: Receiver<ChatEvent>,
    reader_done: Arc<AtomicBool>,
) {
    for event in receiver {
        let mut inner = lock(&chat.inner);
        if inner
            .run
            .as_ref()
            .is_none_or(|run| run.generation != generation)
        {
            continue;
        }
        let over = inner.take_driver_event(event);
        let mut reap = None;
        if over {
            match inner.run.as_mut() {
                // The stop in progress reaps it.
                Some(run) if run.stopping => {}
                // `start` has not returned yet: `attach` sees `ended`.
                Some(run) if run.driver.is_none() => run.ended = true,
                Some(_) => reap = inner.run.take(),
                None => {}
            }
        }
        let stop_wanted = std::mem::take(&mut inner.stop_wanted);
        drop(inner);
        if let Some(run) = reap {
            run.reap();
        }
        if stop_wanted {
            let chat = chat.clone();
            thread::spawn(move || stop(&chat));
        }
    }
    // The channel is closed: the provider process is gone.
    let mut inner = lock(&chat.inner);
    let mut reap = None;
    let started = inner
        .run
        .as_ref()
        .filter(|run| run.generation == generation && !run.stopping)
        .map(|run| run.driver.is_some());
    match started {
        Some(true) => {
            let message = "the provider process ended unexpectedly".to_owned();
            reap = inner.run.take();
            inner.settle(TurnOutcome::Failed {
                message: message.clone(),
            });
            inner.set_state(ChatState::Failed { message });
        }
        Some(false) => {
            if let Some(run) = inner.run.as_mut() {
                run.ended = true;
            }
        }
        None => {}
    }
    drop(inner);
    if let Some(run) = reap {
        run.reap();
    }
    reader_done.store(true, Ordering::SeqCst);
}

/// Shuts the chat's provider process down and marks the chat stopped. The
/// history stays.
fn stop(chat: &Chat) {
    let _turn = lock(&chat.lifecycle);
    stop_locked(chat);
}

fn stop_locked(chat: &Chat) {
    let (driver, reader_done) = {
        let mut inner = lock(&chat.inner);
        match inner.run.as_mut() {
            Some(run) => {
                run.stopping = true;
                (run.driver.take(), Some(run.reader_done.clone()))
            }
            None => (None, None),
        }
    };
    if let Some(driver) = driver {
        lock(&driver).shutdown();
        // A driver may hold its end of the event channel until it is dropped.
    }
    // The driver's last events are still on their way to the log.
    if let Some(reader_done) = reader_done {
        let deadline = Instant::now() + DRAIN;
        while !reader_done.load(Ordering::SeqCst) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
    }
    let mut inner = lock(&chat.inner);
    inner.run = None;
    if !inner.broken {
        inner.settle(TurnOutcome::Interrupted);
        inner.set_state(ChatState::Stopped);
    }
}

fn live_driver(chat: &Chat) -> Option<DriverHandle> {
    let inner = lock(&chat.inner);
    inner
        .run
        .as_ref()
        .filter(|run| run.live())
        .and_then(|run| run.driver.clone())
}

/// Routes a user command to the chat's driver, starting (resuming) the driver
/// first for a command that needs a process.
fn run_command(shared: &Shared, chat: &Arc<Chat>, command: ChatCommand) -> Result<(), String> {
    if matches!(command, ChatCommand::Stop) {
        stop(chat);
        return Ok(());
    }
    // A Configure that changes nothing is a tab's Retry: resume the provider.
    let retry = matches!(
        &command,
        ChatCommand::Configure {
            model: None,
            effort: None,
            approval_mode: None,
        }
    );
    if retry || matches!(command, ChatCommand::Send { .. } | ChatCommand::Compact) {
        ensure_running(shared, chat)?;
    }
    let Some(driver) = live_driver(chat) else {
        // Settings are the chat's own; the next process starts with them.
        if let ChatCommand::Configure {
            model,
            effort,
            approval_mode,
        } = command
        {
            configure(chat, model, effort, approval_mode);
            return Ok(());
        }
        return Err("the chat is stopped; send a message to resume it".into());
    };
    lock(&driver).command(command.clone())?;
    if let ChatCommand::Configure {
        model,
        effort,
        approval_mode,
    } = command
    {
        configure(chat, model, effort, approval_mode);
    }
    Ok(())
}

fn configure(
    chat: &Chat,
    model: Option<String>,
    effort: Option<String>,
    approval_mode: Option<super::model::ApprovalMode>,
) {
    let mut inner = lock(&chat.inner);
    let mut changed = false;
    if let Some(model) = model.filter(|model| inner.info.model.as_ref() != Some(model)) {
        inner.info.model = Some(model);
        changed = true;
    }
    if let Some(effort) = effort.filter(|effort| inner.info.effort.as_ref() != Some(effort)) {
        inner.info.effort = Some(effort);
        changed = true;
    }
    if let Some(mode) = approval_mode.filter(|mode| inner.info.approval_mode != *mode) {
        inner.info.approval_mode = mode;
        changed = true;
    }
    if changed {
        inner.publish_info();
    }
}

// ---- Starting the host (the client side) --------------------------------------------------------

/// Whether a host answers on `socket`. A host that accepts and says nothing
/// counts as not answering.
fn probe(socket: &Path) -> bool {
    let attempt = || -> io::Result<bool> {
        let timeout = Some(Duration::from_secs(2));
        let mut stream = UnixStream::connect(socket)?;
        stream.set_read_timeout(timeout)?;
        stream.set_write_timeout(timeout)?;
        let request = Request::List { id: "probe".into() };
        let mut line = serde_json::to_vec(&request).map_err(io::Error::other)?;
        line.push(b'\n');
        stream.write_all(&line)?;
        let mut answer = String::new();
        BufReader::new(stream).read_line(&mut answer)?;
        Ok(serde_json::from_str::<Response>(&answer).is_ok_and(|response| response.ok))
    };
    attempt().unwrap_or(false)
}

fn log_tail(path: &Path) -> String {
    let text = fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(5)..].join("\n")
}

/// Makes sure a host serves `home` and returns its socket path. Idempotent and
/// safe to run concurrently: callers take turns on a lock, and the first to
/// find no host starts one. `exe` is the `riwork` to run as `chat serve`.
pub fn ensure(home: &Path, exe: &Path) -> Result<PathBuf, String> {
    ensure_within(home, exe, START_WAIT)
}

fn ensure_within(home: &Path, exe: &Path, wait: Duration) -> Result<PathBuf, String> {
    let paths = Paths::new(home)?;
    prepare_run_dir(&paths.dir)?;
    if probe(&paths.socket) {
        return Ok(paths.socket);
    }
    let end = Instant::now() + wait;
    let _turn = loop {
        match try_lock(&paths.ensure_lock)? {
            Some(turn) => break turn,
            None => {
                if Instant::now() >= end {
                    return Err("timed out waiting for another `chat ensure`".into());
                }
                thread::sleep(Duration::from_millis(25));
            }
        }
    };
    // Whoever held the lock may have started it.
    if probe(&paths.socket) {
        return Ok(paths.socket);
    }
    let home = std::path::absolute(home).map_err(|error| error.to_string())?;
    let end = Instant::now() + wait;
    'start: loop {
        let mut child = spawn_host(exe, &home, &paths)?;
        let mut exited = None;
        loop {
            if probe(&paths.socket) {
                // Reaped when it ends, so a long-lived caller leaves no zombie.
                thread::spawn(move || {
                    let _ = child.wait();
                });
                return Ok(paths.socket);
            }
            if exited.is_none() {
                exited = child.try_wait().map_err(|error| error.to_string())?;
            }
            match exited {
                // Another host holds the lock: one that is still starting will
                // answer soon. One that is shutting down releases the lock
                // without having answered, and then this start was the one that
                // was needed.
                Some(status) if status.success() && Instant::now() < end => {
                    if matches!(try_lock(&paths.lock), Ok(Some(_))) {
                        continue 'start;
                    }
                }
                Some(status) => {
                    return Err(format!(
                        "the chat host exited ({status}) before it was ready:\n{}",
                        log_tail(&paths.log)
                    ));
                }
                None if Instant::now() >= end => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "the chat host did not become ready in {} s:\n{}",
                        wait.as_secs(),
                        log_tail(&paths.log)
                    ));
                }
                None => {}
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
}

/// `riwork chat serve`, detached, with its output in the run directory's log.
fn spawn_host(exe: &Path, home: &Path, paths: &Paths) -> Result<std::process::Child, String> {
    let log = open_private(&paths.log, true)
        .map_err(|error| format!("Cannot open {}: {error}", paths.log.display()))?;
    if log.metadata().is_ok_and(|meta| meta.len() > 256 * 1024) {
        let _ = log.set_len(0);
    }
    let mut command = Command::new(exe);
    command
        .args(["chat", "serve"])
        .env("RIWORK_HOME", home)
        // The host is nobody's terminal: it must not carry this one's pane or
        // account into the chats it starts.
        .env_remove("RIWORK_SHELL_ID")
        .stdin(Stdio::null())
        .stdout(Stdio::from(
            log.try_clone().map_err(|error| error.to_string())?,
        ))
        .stderr(Stdio::from(log));
    crate::codex_accounts::scrub_injected_environment(&mut command);
    // SAFETY: setsid is async-signal-safe and touches no memory of ours. The
    // host gets a session and process group of its own, so the terminal or
    // app that started it can close without taking it along.
    unsafe {
        use std::os::unix::process::CommandExt;
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command
        .spawn()
        .map_err(|error| format!("Cannot start {}: {error}", exe.display()))
}

/// For the app: makes sure the chat host of `home` runs, by running the
/// bundled `riwork chat ensure`, and returns the socket to connect to. Blocks
/// for up to a few seconds while a host starts; call it from a background task.
pub fn ensure_host(home: &Path) -> Result<PathBuf, String> {
    let exe = std::env::current_exe()
        .map_err(|error| format!("Cannot locate the RiWork executable: {error}"))?;
    ensure_host_with(&exe, home)
}

fn ensure_host_with(exe: &Path, home: &Path) -> Result<PathBuf, String> {
    let output = Command::new(exe)
        .args(["chat", "ensure"])
        .env("RIWORK_HOME", home)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("Cannot run {}: {error}", exe.display()))?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(if message.is_empty() {
            format!("`riwork chat ensure` failed ({})", output.status)
        } else {
            message
        });
    }
    Ok(Paths::new(home)?.socket)
}

#[cfg(test)]
mod tests;
