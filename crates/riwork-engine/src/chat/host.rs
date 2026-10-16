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
//! **Orchestrators.** A chat created with an `orchestrator` scope is the global or
//! a project's orchestrator (`orchestrators`). The host keeps at most one chat for
//! each scope: `Create` for a scope that has one is refused with
//! `ORCHESTRATOR_EXISTS` and the id of that chat, so racing creators end up with one
//! chat and the loser knows it must not start it. Such a chat has the scope's
//! project and no worktree.
//!
//! **Events.** One reader thread per driver appends what the driver sends to the
//! log (the line number is the `seq`), and broadcasts it to the chat's
//! subscribers. Internal driver reports update the host's own `ChatInfo` (only
//! provider identity and settings); legacy driver's `Info` is still accepted.
//! The host publishes its own `Info` whenever a field other than the state
//! changes; state changes travel as `State` events, which `Transcript` folds
//! into the info too. A subscriber
//! reads from a bounded queue: one that falls too far behind is dropped, and
//! resubscribes with the `seq` it last saw.
//!
//! **Configuration.** Command delivery alone confirms no live settings: a
//! driver reports accepted selections or effective observations. Reports are
//! scoped to a run, with Configure replies also pinned to per-field revisions.
//! Deferred selections and stopped-chat choices remain available for the next
//! turn/start. Startup reports are committed only after successful attachment;
//! historical missing resumes preserve their original identity and transcript.
//!
//! **Workers.** A terminal or chat a chat started is that chat's worker. The host is the
//! only writer of the parent's log, so it writes the worker's marker there (`workers`):
//! on a `NoteWorker` request from whoever started or messaged the worker, when a worker
//! chat is created here, and when a worker chat's turn ends. A note never starts the
//! parent's provider.
//!
//! **Lifetime.** `ensure` starts the host detached (own session, output to
//! `run/chat.log`). The host exits by itself when no client has been connected
//! and no chat has been starting, running or waiting for `DEFAULT_IDLE`; the
//! next `ensure` starts it again.

use super::client;
use super::driver::{Driver, DriverConfig, DriverReport, DriverReporter, StartDriver};
use super::log::{self, ChatLog};
use super::model::{
    CarriedOver, ChatCommand, ChatEvent, ChatInfo, ChatState, ClaudeBilling, ContributorMode,
    Decision, Item, ItemBody, ItemStatus, NewChat, NoticeLevel, ORCHESTRATOR_EXISTS,
    OrchestratorScope, Provider, Transcript, TurnOutcome, notice_kind,
};
use super::wire::{Envelope, Request, Response};
use super::workers::{self, WorkerKind, WorkerNote};
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
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
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
    pub muse: StartDriver,
    /// The Codex account a new chat of this provider and project runs under,
    /// or the one the chat asked for by name.
    pub account: AccountFor,
    /// How to start the provider process of a chat, resuming a thread if given.
    pub config: ConfigureDriver,
}

/// `(RIWORK_HOME, provider, project id, the account asked for)` to a Codex
/// account id, if any.
pub type AccountFor =
    fn(&Path, Provider, Option<&str>, Option<&str>) -> Result<Option<String>, String>;
/// `(RIWORK_HOME, chat, thread to resume)` to the driver's configuration.
pub type ConfigureDriver = fn(&Path, &ChatInfo, Option<String>) -> Result<DriverConfig, String>;

impl Providers {
    pub fn system() -> Self {
        Self {
            codex: super::codex::start,
            claude: super::claude::start,
            muse: super::muse::start,
            account: super::launch::account_for,
            config: super::launch::driver_config,
        }
    }

    fn start_for(&self, provider: Provider) -> StartDriver {
        match provider {
            Provider::Codex => self.codex,
            Provider::Claude => self.claude,
            Provider::Muse => self.muse,
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

/// Each field has its own request revision: choosing an effort must not make
/// an independent pending model acknowledgement stale.
#[derive(Clone, Copy, Default)]
struct SettingsRevisions {
    model: u64,
    effort: u64,
    approval_mode: u64,
    fast: u64,
}

/// The provider process of a chat while one is wanted.
struct Run {
    /// Which start this is: events of an earlier run are dropped.
    generation: u64,
    settings_revisions: SettingsRevisions,
    configurations: u64,
    /// A successful guarded start must still own this historical session.
    guarded_resume: Option<String>,
    /// Do not publish settings/identity from a start that may still fail.
    startup_reports: Vec<DriverReport>,
    /// `None` while `StartDriver` is still running.
    driver: Option<DriverHandle>,
    cancel_io: Option<Arc<dyn Fn() + Send + Sync>>,
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
            settings_revisions: SettingsRevisions::default(),
            configurations: 0,
            guarded_resume: None,
            startup_reports: Vec::new(),
            driver: None,
            cancel_io: None,
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
        if let Some(cancel) = self.cancel_io {
            cancel();
        }
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
    provider_account_identity: Option<super::account_identity::Identity>,
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
    dismissals: Arc<Mutex<super::notice_dismissals::Dismissals>>,
    /// The worker markers in this chat's log by session, oldest first: read on the first
    /// note, then kept current by `append`.
    workers: Option<HashMap<String, Vec<Item>>>,
    /// Where a worker chat's ended turns go, to be recorded in its parent.
    worker_notes: Option<mpsc::Sender<ParentNote>>,
    /// A worker chat's open turns: the messages they took and what they said.
    worker_turn: workers::TurnRecord,
}

/// Work for the thread that records worker chats' ends in their parents.
enum ParentNote {
    /// A worker chat's turn ended; its parent's cards say how.
    Ended { parent: String, note: WorkerNote },
    /// At start: ends the last host may have ended before it recorded them.
    Recover,
}

impl Inner {
    fn new(
        info: ChatInfo,
        log: ChatLog,
        last_seq: u64,
        dismissals: Arc<Mutex<super::notice_dismissals::Dismissals>>,
    ) -> Self {
        // A chat saved before `contributor_mode` is off, and says so to readers from before.
        let mut info = info;
        info.settle_contributor();
        Self {
            provider_account_identity: super::account_identity::read(log.dir()),
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
            dismissals,
            workers: None,
            worker_notes: None,
            worker_turn: Default::default(),
        }
    }

    fn worker_markers(&mut self) -> Result<&mut HashMap<String, Vec<Item>>, String> {
        if self.workers.is_none() {
            let mut index: HashMap<String, Vec<Item>> = HashMap::new();
            for item in log::read_worker_markers(self.log.dir())? {
                if let Some(worker) = workers::of(&item) {
                    index
                        .entry(worker.session_id.clone())
                        .or_default()
                        .push(item);
                }
            }
            self.workers = Some(index);
        }
        Ok(self.workers.get_or_insert_default())
    }

    fn account_scope(&self) -> Option<&str> {
        self.provider_account_identity
            .as_ref()
            .filter(|identity| identity.rank() > 0)
            .map(|identity| identity.scope.as_str())
    }

    fn set_account_identity(&mut self, identity: Option<super::account_identity::Identity>) {
        let identity = identity.filter(|s| s.valid() && s.rank() > 0);
        if identity == self.provider_account_identity {
            if let Some(identity) = &identity {
                if let Err(error) = lock(&self.dismissals).migrate(
                    self.info.provider,
                    identity,
                    self.info.codex_account_id.as_deref(),
                ) {
                    eprintln!("riwork chat: {error}");
                }
            }
            return;
        }
        // Persist before matching notices; a failure must not reuse another login's scope.
        if let Err(error) = self.log.save_account_identity(identity.as_ref()) {
            eprintln!("riwork chat: {error}");
        }
        self.provider_account_identity = identity;
        if let Some(identity) = &self.provider_account_identity {
            if let Err(error) = lock(&self.dismissals).migrate(
                self.info.provider,
                identity,
                self.info.codex_account_id.as_deref(),
            ) {
                eprintln!("riwork chat: {error}");
            }
        }
        if let Ok(transcript) = log::read_notice_transcript(self.log.dir()) {
            for mut item in transcript.items {
                let changed = lock(&self.dismissals).mark(
                    self.info.provider,
                    self.account_scope(),
                    &mut item,
                    super::notice_dismissals::now(),
                );
                if changed {
                    self.append(ChatEvent::ItemCompleted { item });
                }
            }
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
    fn append(&mut self, mut event: ChatEvent) {
        if self.broken || self.deleted {
            return;
        }
        // No pruning here: it reads every saved chat, and this runs for every item.
        // Expired entries never match; startup and dismissals prune them.
        if let ChatEvent::ItemStarted { item } | ChatEvent::ItemCompleted { item } = &mut event {
            let mut dismissals = lock(&self.dismissals);
            if let Err(error) =
                dismissals.forget_worsened(self.info.provider, self.account_scope(), item)
            {
                eprintln!("riwork chat: {error}");
            }
            dismissals.mark(
                self.info.provider,
                self.account_scope(),
                item,
                super::notice_dismissals::now(),
            );
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
        if let ChatEvent::ItemStarted { item } | ChatEvent::ItemCompleted { item } = &envelope.event
            && let Some(worker) = workers::of(item)
            && let Some(index) = self.workers.as_mut()
        {
            let markers = index.entry(worker.session_id.clone()).or_default();
            match markers.iter_mut().find(|marker| marker.id == item.id) {
                Some(marker) => *marker = item.clone(),
                None => markers.push(item.clone()),
            }
        }
        if let (Some(parent), Some(notes)) = (&self.info.parent_id, &self.worker_notes)
            && let Some(note) =
                self.worker_turn
                    .follow(&self.info.id, envelope.seq, &envelope.event, now_unix())
        {
            let _ = notes.send(ParentNote::Ended {
                parent: parent.clone(),
                note,
            });
        }
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
        self.info.settle_contributor();
        self.save_info();
        self.append(ChatEvent::Info {
            info: self.info.clone(),
        });
    }

    /// What a driver actually retained or observed, without accepting any of
    /// the chat/project/account metadata the provider does not own.
    fn learn(&mut self, report: DriverReport) {
        let mut changed = false;
        if let Some(thread) = report
            .provider_thread_id
            .filter(|thread| !thread.is_empty())
            && self.info.provider_thread_id.as_ref() != Some(&thread)
        {
            self.info.provider_thread_id = Some(thread);
            changed = true;
        }
        if let Some(model) = report.model
            && model.as_ref().is_none_or(|model| !model.is_empty())
            && self.info.model != model
        {
            self.info.model = model;
            changed = true;
        }
        if let Some(effort) = report.effort
            && effort.as_ref().is_none_or(|effort| !effort.is_empty())
            && self.info.effort != effort
        {
            self.info.effort = effort;
            changed = true;
        }
        if let Some(mode) = report.approval_mode
            && self.info.approval_mode != mode
        {
            self.info.approval_mode = mode;
            changed = true;
        }
        if let Some(fast) = report.fast
            && self.info.fast != fast
        {
            self.info.fast = fast;
            changed = true;
        }
        if changed {
            self.publish_info();
        }
    }

    fn take_driver_report(
        &mut self,
        mut report: DriverReport,
        revisions: Option<SettingsRevisions>,
    ) {
        if self.deleted || self.broken {
            return;
        }
        let Some(run) = self.run.as_mut().filter(|run| run.live()) else {
            return;
        };
        if revisions.is_none() && run.driver.is_some() {
            // An actual provider change supersedes pending acknowledgements
            // for that field (or the old session). An unchanged observation
            // must not invalidate a legitimate next-turn selection.
            let identity = report.provider_thread_id.as_ref().is_some_and(|id| {
                !id.is_empty() && self.info.provider_thread_id.as_ref() != Some(id)
            });
            let model = report.model.as_ref().is_some_and(|value| {
                value.as_ref().is_none_or(|id| !id.is_empty()) && self.info.model != *value
            });
            let effort = report.effort.as_ref().is_some_and(|value| {
                value.as_ref().is_none_or(|id| !id.is_empty()) && self.info.effort != *value
            });
            let mode = report
                .approval_mode
                .is_some_and(|mode| self.info.approval_mode != mode);
            let fast = report.fast.is_some_and(|fast| self.info.fast != fast);
            if identity || model || effort || mode || fast {
                run.configurations += 1;
                let revision = run.configurations;
                if identity || model {
                    run.settings_revisions.model = revision;
                }
                if identity || effort {
                    run.settings_revisions.effort = revision;
                }
                if identity || mode {
                    run.settings_revisions.approval_mode = revision;
                }
                if identity || fast {
                    run.settings_revisions.fast = revision;
                }
            }
        }
        if let Some(revisions) = revisions {
            let current = run.settings_revisions;
            if revisions.model != current.model {
                report.model = None;
            }
            if revisions.effort != current.effort {
                report.effort = None;
            }
            if revisions.approval_mode != current.approval_mode {
                report.approval_mode = None;
            }
            if revisions.fast != current.fast {
                report.fast = None;
            }
            // A settings reply has no authority to restore an identity from
            // before another request. Session observations use the run sink.
            if revisions.model != current.model
                || revisions.effort != current.effort
                || revisions.approval_mode != current.approval_mode
                || revisions.fast != current.fast
            {
                report.provider_thread_id = None;
            }
        }
        if run.driver.is_none() {
            run.startup_reports.push(report);
        } else {
            self.learn(report);
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
            ChatEvent::ProviderAccountIdentity { identity } => {
                self.set_account_identity(identity);
                false
            }
            ChatEvent::Info { info } => {
                // Compatibility for existing adapters; new adapters use the
                // internal reporter for precise optional fields and replies.
                self.take_driver_report(
                    DriverReport {
                        provider_thread_id: info.provider_thread_id,
                        model: info.model.map(Some),
                        effort: info.effort.map(Some),
                        ..Default::default()
                    },
                    None,
                );
                false
            }
            ChatEvent::ProviderTitle { title } => {
                let title = title
                    .trim()
                    .chars()
                    .filter(|c| !c.is_control())
                    .collect::<String>();
                if !title.is_empty() && self.info.provider_title.as_ref() != Some(&title) {
                    self.info.provider_title = Some(title);
                    self.publish_info();
                }
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
    dismissals: Arc<Mutex<super::notice_dismissals::Dismissals>>,
    paths: Paths,
    providers: Providers,
    options: Options,
    chats: Mutex<HashMap<String, Arc<Chat>>>,
    /// Held while an orchestrator chat is created, from the check that its scope
    /// has none to the chat being listed, so two creators cannot both pass it.
    orchestrator_creation: Mutex<()>,
    connections: AtomicUsize,
    /// When a client last connected or left, or a chat was last at work: what
    /// the idle timer counts from (a request is over long before it is polled).
    activity: Mutex<Instant>,
    quit: AtomicBool,
    /// Ended turns of worker chats, recorded in their parents by `deliver_worker_notes`.
    worker_notes: mpsc::Sender<ParentNote>,
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
/// Whether a message may name the chat: only a chat with no title of its own yet, never an
/// orchestrator (whose name is fixed), and only once. A resume, a host restart or a log
/// read keeps `first_user_message`, so nothing here derives it again.
fn needs_auto_title(info: &ChatInfo) -> bool {
    info.first_user_message.is_none()
        && info.orchestrator.is_none()
        && info.provider_title.is_none()
        && info
            .user_title
            .as_deref()
            .is_none_or(|title| title.trim().is_empty())
}

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
        let dismissals = Arc::new(Mutex::new(super::notice_dismissals::Dismissals::open(
            &home,
        )?));
        let (worker_notes, worker_receiver) = mpsc::channel();
        let chats = load_chats(&home, &dismissals, &providers, &worker_notes);
        let listener = UnixListener::bind(&paths.socket)
            .map_err(|error| format!("Cannot listen on {}: {error}", paths.socket.display()))?;
        fs::set_permissions(&paths.socket, fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("Cannot protect {}: {error}", paths.socket.display()))?;
        let shared = Arc::new(Shared {
            home,
            dismissals,
            paths,
            providers,
            options,
            chats: Mutex::new(chats),
            orchestrator_creation: Mutex::new(()),
            connections: AtomicUsize::new(0),
            activity: Mutex::new(Instant::now()),
            quit: AtomicBool::new(false),
            worker_notes,
        });
        {
            let shared = Arc::downgrade(&shared);
            thread::Builder::new()
                .name("chat-workers".into())
                .spawn(move || deliver_worker_notes(shared, worker_receiver))
                .map_err(|error| format!("Cannot start the worker thread: {error}"))?;
        }
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
        Request::Capabilities { id } => answer(
            id,
            Ok(super::wire::Capabilities {
                identified_create: true,
            }),
        ),
        Request::StageAttachment { id, chat_id, path } => answer(
            id,
            shared.find(&chat_id).and_then(|chat| {
                let inner = lock(&chat.inner);
                if inner.deleted {
                    return Err(format!("unknown chat {chat_id}"));
                }
                super::attachments::stage(&chat.dir, &path)
            }),
        ),
        Request::Create { id, chat, chat_id } => {
            answer(id, create_identified(shared, chat, chat_id))
        }
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
        Request::NoteWorker { id, chat_id, note } => {
            answer(id, note_worker(shared, &chat_id, note))
        }
        Request::Subscribe { id, .. } => failure(id, "subscribe takes over a connection".into()),
    }
}

fn list(shared: &Shared) -> Vec<ChatInfo> {
    let mut infos: Vec<ChatInfo> = shared.all().iter().map(|chat| chat.info()).collect();
    infos.sort_by(|a, b| (a.created_at_unix, &a.id).cmp(&(b.created_at_unix, &b.id)));
    infos
}

// ---- Workers ------------------------------------------------------------------------------------

/// Records `note` about one of `chat_id`'s workers in its log. The worker must be the
/// chat's own (its `parent_id`), in the chat's project; the host describes it from the
/// inventory, not from the note. A worker that is gone (a closed terminal, a deleted chat)
/// can still end: its card here says whose it was. The chat's provider is never started.
fn note_worker(shared: &Shared, chat_id: &str, note: WorkerNote) -> Result<(), String> {
    let chat = shared.find(chat_id)?;
    let info = chat.info();
    let now = now_unix();
    let session_id = note.session_id();
    let ending = matches!(note, WorkerNote::Finish { .. });
    let fresh = match note.kind() {
        WorkerKind::Shell => match crate::sessions::registered_shell(&shared.home, session_id) {
            Ok(shell) => {
                if shell.parent_id.as_deref() != Some(chat_id)
                    || shell.project_id != info.project_id
                {
                    return Err(format!(
                        "terminal {session_id} is not a worker of chat {chat_id}"
                    ));
                }
                let tasks = crate::store::Store::open(&shared.home)
                    .and_then(|store| store.snapshot())
                    .map(|state| state.tasks)
                    .unwrap_or_default();
                Some(workers::describe_shell(&shell, &tasks, now))
            }
            Err(_) if ending => None,
            Err(error) => return Err(error),
        },
        WorkerKind::Chat => match shared.find(session_id) {
            Ok(child) => {
                let (child, next) = {
                    let inner = lock(&child.inner);
                    (inner.info.clone(), inner.next_seq)
                };
                if child.parent_id.as_deref() != Some(chat_id) {
                    return Err(format!(
                        "chat {session_id} is not a worker of chat {chat_id}"
                    ));
                }
                Some(workers::WorkerRef {
                    after: Some(next),
                    ..workers::describe_chat(&child, now)
                })
            }
            Err(_) if ending => None,
            Err(error) => return Err(error),
        },
        WorkerKind::Unknown => return Err("unknown worker kind".into()),
    };
    record_worker(&chat, chat_id, &note, fresh, now)
}

/// Writes what `note` changes on the cards of one of `chat_id`'s workers. With no fresh
/// description (the worker is gone), only a worker with a card here has anything to end.
/// An error says nothing was recorded that should have been.
fn record_worker(
    chat: &Chat,
    chat_id: &str,
    note: &WorkerNote,
    fresh: Option<workers::WorkerRef>,
    now: u64,
) -> Result<(), String> {
    let mut inner = lock(&chat.inner);
    if inner.deleted || inner.broken {
        return Err(format!("chat {chat_id} cannot be written"));
    }
    let turn = inner.open.turn.clone();
    let markers = inner
        .worker_markers()?
        .get(note.session_id())
        .cloned()
        .unwrap_or_default();
    let Some(fresh) = fresh.or_else(|| markers.last().and_then(workers::of).cloned()) else {
        return Ok(());
    };
    let items = match workers::apply(&markers, note, fresh, turn.as_deref(), now) {
        workers::Upsert::Append(item) => vec![item],
        workers::Upsert::Replace(items) => items,
        workers::Upsert::Nothing => Vec::new(),
    };
    for item in items {
        inner.append(ChatEvent::ItemCompleted { item });
        if inner.broken {
            return Err(format!(
                "chat {chat_id}: the worker's card could not be written"
            ));
        }
    }
    Ok(())
}

/// Records the ended turns of worker chats in their parents, away from the lock of the
/// worker chat that ended one.
fn deliver_worker_notes(shared: Weak<Shared>, receiver: Receiver<ParentNote>) {
    for note in receiver {
        let Some(shared) = shared.upgrade() else {
            return;
        };
        deliver_worker_note(&shared, note);
    }
}

fn deliver_worker_note(shared: &Shared, note: ParentNote) {
    match note {
        ParentNote::Recover => recover_worker_ends(shared),
        ParentNote::Ended { parent, note } => {
            // A parent that is a terminal, or gone, has no card to complete. The worker
            // chat may be gone by now: its card says it was the parent's.
            if let Ok(chat) = shared.find(&parent)
                && let Err(error) = record_worker(&chat, &parent, &note, None, now_unix())
            {
                eprintln!("riwork chat: worker card in {parent}: {error}");
            }
        }
    }
}

/// The ends of worker chats' turns are queued in memory, so a host that ended could leave
/// one unrecorded. Each worker chat whose latest card is still open has its log's turn
/// ends recorded again; what its cards already have is nothing (`workers::apply`).
fn recover_worker_ends(shared: &Shared) {
    for child in shared.all() {
        let info = child.info();
        let Some(parent_id) = info.parent_id.as_deref() else {
            continue;
        };
        let Ok(parent) = shared.find(parent_id) else {
            continue;
        };
        // Only a worker whose latest card is open can be missing an end, and only in what
        // its log took since its oldest open card was written (a card from before `after`
        // was recorded reads from the start).
        let from = {
            let mut inner = lock(&parent.inner);
            let Some(cards) = inner
                .worker_markers()
                .ok()
                .and_then(|index| index.get(&info.id).cloned())
            else {
                continue;
            };
            if !cards
                .last()
                .is_some_and(|card| workers::of(card).is_some_and(|worker| worker.end.is_none()))
            {
                continue;
            }
            cards
                .iter()
                .filter_map(workers::of)
                .filter(|worker| worker.end.is_none())
                .map(|worker| worker.after.unwrap_or(1))
                .min()
                .unwrap_or(1)
                .max(1)
        };
        let last = lock(&child.inner).next_seq.saturating_sub(1);
        if from > last {
            continue;
        }
        let mut lines = Vec::new();
        let envelopes = log::replay(&child.dir, from - 1, last, &mut lines)
            .map_err(|error| error.to_string())
            .and_then(|()| {
                lines
                    .split(|byte| *byte == b'\n')
                    .filter(|line| !line.is_empty())
                    .map(|line| {
                        serde_json::from_slice::<Envelope>(line).map_err(|error| error.to_string())
                    })
                    .collect::<Result<Vec<_>, _>>()
            });
        let envelopes = match envelopes {
            Ok(envelopes) => envelopes,
            Err(error) => {
                eprintln!("riwork chat: cannot read the log of {}: {error}", info.id);
                continue;
            }
        };
        let mut record = workers::TurnRecord::default();
        let now = now_unix();
        for envelope in &envelopes {
            if let Some(note) = record.follow(&info.id, envelope.seq, &envelope.event, now)
                && let Err(error) = record_worker(&parent, parent_id, &note, None, now)
            {
                eprintln!("riwork chat: worker card in {parent_id}: {error}");
            }
        }
    }
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

#[cfg(test)]
fn create(shared: &Shared, new: NewChat) -> Result<ChatInfo, String> {
    create_identified(shared, new, None)
}

fn create_identified(
    shared: &Shared,
    mut new: NewChat,
    requested_id: Option<String>,
) -> Result<ChatInfo, String> {
    // Serialize caller-owned IDs as well as orchestrators. Never overwrite an
    // existing chat, including one saved by an interrupted host.
    let creation = lock(&shared.orchestrator_creation);
    let id = requested_id.unwrap_or_else(|| Uuid::new_v4().to_string());
    if !Uuid::parse_str(&id).is_ok_and(|uuid| uuid.to_string() == id) {
        return Err("invalid chat id".into());
    }
    if shared.find(&id).is_ok() || log::chat_dir(&shared.home, &id).is_some_and(|dir| dir.exists())
    {
        return Err("chat identity already exists; review it without retrying".into());
    }

    if new
        .parent_id
        .as_ref()
        .is_some_and(|id| !Uuid::parse_str(id).is_ok_and(|uuid| uuid.to_string() == *id))
    {
        return Err("parent_id must be a canonical RiWork session UUID".into());
    }
    if new.orchestrator.is_some() && new.parent_id.is_some() {
        return Err("an orchestrator cannot be a child session".into());
    }
    // Muse has no fast mode. A saved automation may still ask for it, so the chat is made,
    // never recorded as fast, and says so (`fast_refused` below).
    let fast_refused = new.provider == Provider::Muse && new.fast;
    new.fast &= !fast_refused;
    // Contributor mode is off unless asked on; an older caller's opt-out is an off.
    if new.contributor_mode == ContributorMode::On && new.contributor_opt_out {
        return Err(super::model::CONTRIBUTOR_CONTRADICTION.into());
    }
    // It is Muse's alone. A Codex or Claude chat asked for one is made off, as every chat is
    // unless asked on, and it says so (`contributor_refused`).
    let contributor_refused = new.provider != Provider::Muse
        && (new.contributor_mode == ContributorMode::On || new.contributor_opt_out);
    if contributor_refused {
        new.contributor_mode = ContributorMode::Off;
    }
    new.parent_id = crate::project_tabs::same_project_parent(
        &shared.home,
        new.project_id.as_deref(),
        new.parent_id,
    );
    if !new.cwd.is_absolute() || !new.cwd.is_dir() {
        return Err(format!(
            "the working directory {} is not an existing absolute directory",
            new.cwd.display()
        ));
    }
    if shared.quit.load(Ordering::SeqCst) {
        return Err("the chat host is shutting down".into());
    }
    // Held until the chat is listed: see `Shared::orchestrator_creation`.
    if let Some(scope) = &new.orchestrator {
        check_orchestrator_scope(&new, scope)?;
        if let Some(existing) = shared
            .all()
            .iter()
            .map(|chat| chat.info())
            .find(|info| info.orchestrator.as_ref() == Some(scope))
        {
            return Err(format!("{ORCHESTRATOR_EXISTS} {}", existing.id));
        }
    }
    let account = (shared.providers.account)(
        &shared.home,
        new.provider,
        new.project_id.as_deref(),
        new.codex_account_id.as_deref(),
    )?;
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
        .unwrap_or_else(|| default_title(new.provider).to_owned());
    let mut info = ChatInfo {
        parent_id: new.parent_id,
        user_title: new
            .title
            .as_ref()
            .filter(|t| !t.trim().is_empty())
            .map(|_| title.clone()),
        first_user_message: None,
        provider_title: None,
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
        fast: new.fast,
        contributor_mode: new.contributor_mode,
        contributor_opt_out: false,
        // A Claude chat has its billing from the start, the Settings default unless asked:
        // a later change of the default leaves it as it was made.
        claude_billing: (new.provider == Provider::Claude).then(|| {
            new.claude_billing
                .unwrap_or_else(|| crate::settings::claude_chat_billing(&shared.home))
        }),
        approval_mode: new.approval_mode,
        codex_account_id: account,
        state: ChatState::Starting,
        orchestrator: new.orchestrator,
        carried_over: None,
    };
    info.settle_contributor();
    let provider = info.provider;
    let dir = log::chat_dir(&shared.home, &id).ok_or("invalid chat id")?;
    let log = ChatLog::create(&dir, &info)?;
    let mut inner = Inner::new(info.clone(), log, 0, shared.dismissals.clone());
    inner.worker_notes = Some(shared.worker_notes.clone());
    inner.append(ChatEvent::Info { info });
    if fast_refused {
        let id = format!("fast-refused-{}", inner.next_seq);
        inner.append(ChatEvent::ItemCompleted {
            item: Item {
                presentation: Default::default(),
                id,
                turn_id: None,
                status: ItemStatus::Completed,
                body: ItemBody::notice(
                    NoticeLevel::Warning,
                    "Muse has no fast mode; this chat runs at Muse's usual speed.".to_owned(),
                    Some(notice_kind::FAST_MODE),
                ),
            },
        });
    }
    if contributor_refused {
        let id = format!("contributor-refused-{}", inner.next_seq);
        inner.append(ChatEvent::ItemCompleted {
            item: Item {
                presentation: Default::default(),
                id,
                turn_id: None,
                status: ItemStatus::Completed,
                body: ItemBody::notice(
                    NoticeLevel::Warning,
                    format!(
                        "Only Muse has a contributor mode; this {} chat starts without one.",
                        provider_name(provider)
                    ),
                    Some(notice_kind::SETTING_REFUSED),
                ),
            },
        });
    }
    let chat = Arc::new(Chat {
        dir,
        lifecycle: Mutex::new(()),
        inner: Mutex::new(inner),
    });
    lock(&shared.chats).insert(id, chat.clone());
    if let Some(project) = chat.info().project_id {
        if let Err(error) =
            crate::project_tabs::TabStore::at(&shared.home, &project).and_then(|store| {
                store.register_created(&crate::project_tabs::Session::chat(&chat.info()))
            })
        {
            eprintln!("riwork tabs: {error}");
        }
    }
    drop(creation);
    // A worker chat's card in the chat that started it, where that chat's turn is now.
    let info = chat.info();
    if let Some(parent) = info
        .parent_id
        .as_deref()
        .filter(|id| shared.find(id).is_ok())
    {
        let note = WorkerNote::Spawn {
            session_id: info.id.clone(),
            kind: WorkerKind::Chat,
            late: false,
        };
        if let Err(error) = note_worker(shared, parent, note) {
            eprintln!("riwork chat: worker card for {}: {error}", info.id);
        }
    }
    // A driver that cannot start leaves the chat failed, not missing: its
    // message is in the chat, and the next message tries again.
    let _ = ensure_running(shared, &chat);
    Ok(chat.info())
}

/// The title of a chat that was given none.
fn default_title(provider: Provider) -> &'static str {
    match provider {
        Provider::Codex => "Codex chat",
        Provider::Claude => "Claude chat",
        Provider::Muse => "Muse chat",
    }
}

fn provider_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Codex => "Codex",
        Provider::Claude => "Claude",
        Provider::Muse => "Muse",
    }
}

/// An orchestrator chat belongs to its scope: the global orchestrator has no
/// project or worktree, a project's has that project and no worktree.
fn check_orchestrator_scope(new: &NewChat, scope: &OrchestratorScope) -> Result<(), String> {
    let fits = new.worktree_id.is_none()
        && match scope {
            OrchestratorScope::Global => new.project_id.is_none(),
            OrchestratorScope::Project { project_id } => {
                new.project_id.as_deref() == Some(project_id.as_str())
                    && Uuid::parse_str(project_id).is_ok_and(|uuid| uuid.to_string() == *project_id)
            }
        };
    if fits {
        Ok(())
    } else {
        Err("an orchestrator chat has the project of its scope and no worktree".into())
    }
}

/// Reads every chat from disk. No provider is started: a chat that was running
/// when the last host ended is stopped now, and a turn it left open is closed.
fn load_chats(
    home: &Path,
    dismissals: &Arc<Mutex<super::notice_dismissals::Dismissals>>,
    providers: &Providers,
    worker_notes: &mpsc::Sender<ParentNote>,
) -> HashMap<String, Arc<Chat>> {
    let mut chats = HashMap::new();
    let Ok(entries) = fs::read_dir(log::chats_dir(home)) else {
        return chats;
    };
    let entries: Vec<_> = entries.flatten().collect();
    // Discover every saved chat first; default migration must see all accounts,
    // rather than depending on directory iteration order. This starts no provider.
    let mut identities = Vec::new();
    for entry in &entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(dir) = log::chat_dir(home, &name).filter(|dir| dir.is_dir()) else {
            continue;
        };
        let Ok(bytes) = fs::read(dir.join("info.json")) else {
            continue;
        };
        let Ok(info) = serde_json::from_slice::<ChatInfo>(&bytes) else {
            continue;
        };
        if info.id != name {
            continue;
        }
        let previous = super::account_identity::read(&dir);
        // Who a chat bills plays no part in whom it is signed in as. Asked as Subscription,
        // its configuration never reads the API key: starting the host asks the Keychain
        // nothing, and cannot wait on a Keychain prompt before it serves.
        let mut probe = info.clone();
        if probe.provider == Provider::Claude {
            probe.claude_billing = Some(ClaudeBilling::Subscription);
        }
        let identity = (providers.config)(home, &probe, info.provider_thread_id.clone())
            .ok()
            .and_then(|config| super::account_identity::canonical(&config, None, previous.as_ref()))
            .or_else(|| previous.filter(|identity| identity.rank() > 0));
        if let Some(identity) = identity {
            if let Err(error) = log::write_account_identity(&dir, Some(&identity)) {
                eprintln!("riwork chat: {error}");
                continue;
            }
            identities.push((info.provider, identity));
        }
    }
    if let Err(error) = lock(dismissals).migrate_all(identities) {
        eprintln!("riwork chat: {error}");
    }
    for entry in entries {
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
        let mut settled = info.clone();
        settled.settle_contributor();
        let unsettled = settled != info;
        let mut inner = Inner::new(info, chat_log, last, dismissals.clone());
        inner.worker_notes = Some(worker_notes.clone());
        if was_at_work {
            // Only a chat that died mid-turn needs its log read whole.
            match log::read_envelopes(&dir) {
                Ok(envelopes) => {
                    for envelope in &envelopes {
                        inner.open.apply(&envelope.event);
                        // What the open turn took and said, for its card in the parent.
                        if inner.info.parent_id.is_some() {
                            inner
                                .worker_turn
                                .follow(&name, envelope.seq, &envelope.event, 0);
                        }
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
        // A chat saved before `contributor_mode` is off: its log says so too, for a tab that
        // replays it and a reader from before.
        if unsettled {
            inner.publish_info();
        }
        if let Err(error) = refresh_notice_dismissals(home, &name, &mut inner) {
            eprintln!("riwork chat: {error}");
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
    // Before anything new: what the last host may not have recorded in parents.
    let _ = worker_notes.send(ParentNote::Recover);
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
    if let Some(project) = chat.info().project_id {
        let result = crate::project_tabs::TabStore::at(&shared.home, &project)
            .and_then(|store| store.forget(&format!("chat:{chat_id}")));
        if let Err(error) = result {
            eprintln!("riwork tabs: {error}");
        }
    }
    // Files a phone sent to this chat (see `upload_inbox`) go with it.
    crate::upload_inbox::remove(&shared.home, chat_id);
    fs::remove_dir_all(&chat.dir)
        .map_err(|error| format!("Cannot remove {}: {error}", chat.dir.display()))
}

// ---- Driving a chat -----------------------------------------------------------------------------

fn outstanding_sticky_notices(transcript: &Transcript) -> HashMap<String, Item> {
    let mut newest = HashMap::new();
    for item in &transcript.items {
        if let ItemBody::Notice {
            kind: Some(kind), ..
        } = &item.body
            && (kind == notice_kind::AUTH_REQUIRED || kind.starts_with("rate_limit:"))
        {
            newest.insert(kind.clone(), item.clone());
        }
    }
    newest.retain(|_, item| {
        matches!(
            item.body,
            ItemBody::Notice {
                resolved: false,
                ..
            }
        )
    });
    newest
}

/// Starts the chat's provider process unless one is running.
fn ensure_running(shared: &Shared, chat: &Arc<Chat>) -> Result<(), String> {
    let _turn = lock(&chat.lifecycle);
    ensure_running_locked(shared, chat)
}

/// The caller already serializes start/stop/switch/configure.
fn ensure_running_locked(shared: &Shared, chat: &Arc<Chat>) -> Result<(), String> {
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
    let mut config = match config {
        Ok(config) => config,
        Err(error) => return fail_start(chat, generation, error),
    };
    let mut inner = lock(&chat.inner);
    if config.resume.is_some() {
        config.resume_has_history = match inner.log.provider_session_has_history(&info) {
            Ok(history) => history,
            Err(error) => {
                drop(inner);
                return fail_start(chat, generation, error);
            }
        };
    }
    if let Some(run) = inner.run.as_mut() {
        run.guarded_resume = config.resume.clone().filter(|_| config.resume_has_history);
    }
    let identity =
        super::account_identity::canonical(&config, None, inner.provider_account_identity.as_ref());
    inner.set_account_identity(identity);
    drop(inner);
    config.reporter = driver_reporter(chat, generation, None);
    // Only the log's tail, and only its notices: a long chat does not hold up the start,
    // and one that cannot be read leaves nothing to resolve.
    config.outstanding_notices =
        outstanding_sticky_notices(&log::notice_tail(&chat.dir, log::NOTICE_TAIL));
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

fn driver_reporter(
    chat: &Arc<Chat>,
    generation: u64,
    revisions: Option<SettingsRevisions>,
) -> DriverReporter {
    // A driver may outlive its run. Do not keep the chat alive via a callback
    // owned by its driver (or a saved fixture configuration).
    let chat = Arc::downgrade(chat);
    DriverReporter::new(move |report| {
        let Some(chat) = chat.upgrade() else {
            return;
        };
        let mut inner = lock(&chat.inner);
        if inner
            .run
            .as_ref()
            .is_some_and(|run| run.generation == generation)
        {
            inner.take_driver_report(report, revisions);
        }
    })
}

fn fail_start(chat: &Chat, generation: u64, message: String) -> Result<(), String> {
    let mut inner = lock(&chat.inner);
    if inner
        .run
        .as_ref()
        .is_none_or(|run| run.generation != generation)
    {
        return Err(message);
    }
    inner.run = None;
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
    let cancel_io = driver.cancel_io();
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
    run.cancel_io = cancel_io;
    let ended = run.ended || run.stopping;
    let replaced = run
        .guarded_resume
        .as_ref()
        .filter(|original| {
            thread.as_ref() != Some(original)
                || run.startup_reports.iter().any(|report| {
                    report
                        .provider_thread_id
                        .as_ref()
                        .is_some_and(|id| id != *original)
                })
        })
        .cloned();
    if ended || replaced.is_some() {
        // The provider was over before `start` returned. If it said how, that
        // is the chat's state; if its channel just closed, the chat fails here.
        let message = match (&inner.info.state, replaced) {
            (_, Some(original)) => format!(
                "The provider did not resume historical session {original}. Its original identity \
                 and transcript were kept; restore the provider's session store and retry."
            ),
            (ChatState::Failed { message }, _) => message.clone(),
            _ => "the provider process ended while it started".to_owned(),
        };
        let reap = inner.run.take();
        if !ended
            || !matches!(
                inner.info.state,
                ChatState::Stopped | ChatState::Failed { .. }
            )
        {
            inner.settle(TurnOutcome::Failed {
                message: message.clone(),
            });
            inner.set_state(ChatState::Failed {
                message: message.clone(),
            });
        }
        drop(inner);
        if let Some(run) = reap {
            run.reap();
        }
        return Err(message);
    }
    let reports = std::mem::take(&mut run.startup_reports);
    inner.learn(DriverReport {
        provider_thread_id: thread,
        ..Default::default()
    });
    for report in reports {
        inner.learn(report);
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
    // A Configure can block on child I/O while holding lifecycle and driver.
    // Cancel before waiting on those locks, so Stop can always release it.
    let cancel = {
        let mut inner = lock(&chat.inner);
        inner.run.as_mut().and_then(|run| {
            run.stopping = true;
            run.cancel_io.clone()
        })
    };
    if let Some(cancel) = cancel {
        cancel();
    }
    let _turn = lock(&chat.lifecycle);
    stop_locked(chat);
}

fn stop_locked(chat: &Chat) {
    let (driver, reader_done, cancel_io) = {
        let mut inner = lock(&chat.inner);
        match inner.run.as_mut() {
            Some(run) => {
                run.stopping = true;
                (
                    run.driver.take(),
                    Some(run.reader_done.clone()),
                    run.cancel_io.take(),
                )
            }
            None => (None, None, None),
        }
    };
    if let Some(cancel) = cancel_io {
        cancel();
    }
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

fn live_driver(chat: &Chat) -> Option<(u64, DriverHandle)> {
    let inner = lock(&chat.inner);
    inner
        .run
        .as_ref()
        .filter(|run| run.live())
        .and_then(|run| run.driver.clone().map(|driver| (run.generation, driver)))
}

/// Routes a user command to the chat's driver, starting (resuming) the driver
/// first for a command that needs a process.
fn run_command(shared: &Shared, chat: &Arc<Chat>, command: ChatCommand) -> Result<(), String> {
    if let ChatCommand::DismissNotice { item_id } = &command {
        return dismiss_notice(shared, chat, item_id);
    }
    if let ChatCommand::SendAttachments { text, attachments } = &command {
        if text.len() > super::attachments::TEXT_BYTES {
            return Err("attachment message text exceeds 1 MiB".into());
        }
        super::attachments::validate_owned(&chat.dir, attachments)?;
        // Encoding bounds apply before a stopped chat can start a provider, too.
        super::attachments::inputs(text, attachments, chat.info().provider == Provider::Claude)?;
    }
    if matches!(command, ChatCommand::Stop) {
        stop(chat);
        return Ok(());
    }
    match command {
        ChatCommand::Switch {
            provider,
            model,
            effort,
            fast,
        } => {
            return switch(shared, chat, provider, model, effort, fast);
        }
        ChatCommand::Configure { .. } => {
            let _turn = lock(&chat.lifecycle);
            return configure_command_locked(shared, chat, command);
        }
        _ => {}
    }
    if matches!(
        command,
        ChatCommand::Send { .. } | ChatCommand::SendAttachments { .. } | ChatCommand::Compact
    ) {
        ensure_running(shared, chat)?;
    }
    let Some((generation, driver)) = live_driver(chat) else {
        return Err("the chat is stopped; send a message to resume it".into());
    };
    let mut driver = lock(&driver);
    if !lock(&chat.inner)
        .run
        .as_ref()
        .is_some_and(|run| run.generation == generation && run.live())
    {
        return Err("the chat was stopped before the command reached its provider".into());
    }
    driver.command(command.clone())?;
    drop(driver);
    if let ChatCommand::Send { text } | ChatCommand::SendAttachments { text, .. } = &command {
        let mut inner = lock(&chat.inner);
        if inner
            .run
            .as_ref()
            .is_some_and(|run| run.generation == generation)
            && needs_auto_title(&inner.info)
        {
            let title = crate::project_tabs::message_title(text);
            if !title.is_empty() {
                inner.info.first_user_message = Some(title);
                inner.publish_info();
            }
        }
    }
    Ok(())
}

fn configure_command_locked(
    shared: &Shared,
    chat: &Arc<Chat>,
    command: ChatCommand,
) -> Result<(), String> {
    muse_refusal(&chat.info(), &command)?;
    let contributor = command.requested_contributor_mode()?;
    contributor_refusal(&chat.info(), &command, contributor)?;
    billing_refusal(&chat.info(), &command)?;
    let ChatCommand::Configure {
        model,
        effort,
        approval_mode,
        fast,
        claude_billing,
        ..
    } = &command
    else {
        return Err("expected a Configure command".into());
    };
    let settings = model.is_some() || effort.is_some() || approval_mode.is_some() || fast.is_some();
    // A tab's Retry has no settings, and is the only Configure that starts a
    // stopped provider. Choices made before first use remain saved for start.
    if !settings && contributor.is_none() && claude_billing.is_none() {
        ensure_running_locked(shared, chat)?;
    }
    {
        let inner = lock(&chat.inner);
        if inner.deleted || inner.broken {
            return Err("this chat cannot be configured".into());
        }
    }
    // A change comes alone (`contributor_refusal`); beside other settings it is the value the
    // chat has, and only they change.
    if let Some(mode) = contributor {
        configure_contributor_locked(shared, chat, mode)?;
        if !settings {
            return Ok(());
        }
    }
    // Alone too (`billing_refusal`).
    if let Some(billing) = *claude_billing {
        configure_billing_locked(shared, chat, billing)?;
        if !settings {
            return Ok(());
        }
    }
    // A driver has no contributor mode to set: that is the process's, chosen above.
    let command = ChatCommand::Configure {
        model: model.clone(),
        effort: effort.clone(),
        approval_mode: *approval_mode,
        fast: *fast,
        contributor_mode: None,
        contributor_opt_out: None,
        claude_billing: None,
    };
    let Some((generation, driver)) = live_driver(chat) else {
        configure(chat, model.clone(), effort.clone(), *approval_mode, *fast);
        return Ok(());
    };
    let mut driver = lock(&driver);
    let (before, revisions) = {
        let mut inner = lock(&chat.inner);
        let Some(run) = inner
            .run
            .as_mut()
            .filter(|run| run.generation == generation && run.live())
        else {
            return Err("the provider ended before it could be configured".into());
        };
        let before = run.settings_revisions;
        run.configurations += 1;
        let revision = run.configurations;
        if model.is_some() {
            run.settings_revisions.model = revision;
        }
        if effort.is_some() {
            run.settings_revisions.effort = revision;
        }
        if approval_mode.is_some() {
            run.settings_revisions.approval_mode = revision;
        }
        if fast.is_some() {
            run.settings_revisions.fast = revision;
        }
        (before, run.settings_revisions)
    };
    let result = driver.configure(command, driver_reporter(chat, generation, Some(revisions)));
    if result.is_err() {
        let mut inner = lock(&chat.inner);
        if let Some(run) = inner
            .run
            .as_mut()
            .filter(|run| run.generation == generation)
        {
            // A synchronous refusal did not supersede an earlier pending
            // selection. Do not undo intervening provider observations.
            if run.settings_revisions.model == revisions.model {
                run.settings_revisions.model = before.model;
            }
            if run.settings_revisions.effort == revisions.effort {
                run.settings_revisions.effort = before.effort;
            }
            if run.settings_revisions.approval_mode == revisions.approval_mode {
                run.settings_revisions.approval_mode = before.approval_mode;
            }
            if run.settings_revisions.fast == revisions.fast {
                run.settings_revisions.fast = before.fast;
            }
        }
    }
    result
}

/// A contributor mode change that is refused before anything changes: for a chat that is not
/// Muse's, as Muse refuses a fast mode (Codex and Claude have none), and together with other
/// settings, which the Muse started again for the change could still refuse, leaving the change
/// made and the command failed. Asking for the value the chat already has changes nothing and is
/// not refused, as asking Muse for no fast mode is not.
fn contributor_refusal(
    info: &ChatInfo,
    command: &ChatCommand,
    requested: Option<ContributorMode>,
) -> Result<(), String> {
    let (
        Some(mode),
        ChatCommand::Configure {
            model,
            effort,
            approval_mode,
            fast,
            ..
        },
    ) = (requested, command)
    else {
        return Ok(());
    };
    if mode == info.contributor_mode {
        return Ok(());
    }
    // A chat that is not Muse's starts no Muse: it keeps the mode it had as Muse's for a switch
    // back, which it can turn off, but never on.
    if info.provider != Provider::Muse {
        return match mode {
            ContributorMode::Off => Ok(()),
            ContributorMode::On => Err(format!(
                "Only Muse has a contributor mode to turn on; this {} chat keeps it off.",
                provider_name(info.provider)
            )),
        };
    }
    if model.is_some() || effort.is_some() || approval_mode.is_some() || fast.is_some() {
        return Err("Change contributor mode on its own: Muse starts again for it.".into());
    }
    Ok(())
}

/// Turns a Muse chat's contributor mode on or off. Muse fixes the mode when `muse serve` starts
/// and MSP has no field for it, so no
/// driver can be configured with it. A chat with no process keeps the value for its next
/// start. An idle one has its process stopped and started again with the value, and the start
/// resumes the same Muse session with the standing model, effort and approval mode, as any
/// resume does. While a turn runs or waits for the user, or the process still starts, the
/// change is refused and the chat keeps its value. A start that fails leaves the chat failed,
/// as any failed start does, and with the value it had: no process runs with the new one, and
/// the next start uses the old one again; a notice says so. A chat of another provider, which
/// keeps the mode for a switch back to Muse, only stores it: its own process is not Muse's.
fn configure_contributor_locked(
    shared: &Shared,
    chat: &Arc<Chat>,
    mode: ContributorMode,
) -> Result<(), String> {
    let before = {
        let mut inner = lock(&chat.inner);
        if inner.info.contributor_mode == mode {
            return Ok(());
        }
        if inner.info.provider != Provider::Muse {
            inner.info.contributor_mode = mode;
            inner.publish_info();
            return Ok(());
        }
        let at_work = matches!(inner.info.state, ChatState::Running | ChatState::Waiting)
            || inner.open.turn.is_some()
            || !inner.open.approvals.is_empty()
            || !inner.open.questions.is_empty();
        if at_work {
            return Err(
                "Wait for the turn to finish, or interrupt it, before changing contributor mode."
                    .into(),
            );
        }
        if inner.busy() {
            return Err("Muse is still starting; change contributor mode once it is ready.".into());
        }
        if !inner.live() {
            inner.info.contributor_mode = mode;
            inner.publish_info();
            return Ok(());
        }
        inner.info.contributor_mode
    };
    if shared.quit.load(Ordering::SeqCst) {
        return Err("the chat host is shutting down".into());
    }
    stop_locked(chat);
    {
        let mut inner = lock(&chat.inner);
        if inner.deleted || inner.broken {
            return Err("this chat cannot be configured".into());
        }
        inner.info.contributor_mode = mode;
        inner.publish_info();
    }
    let Err(error) = ensure_running_locked(shared, chat) else {
        return Ok(());
    };
    let mut inner = lock(&chat.inner);
    inner.info.contributor_mode = before;
    inner.publish_info();
    let text = format!(
        "Contributor mode was not changed: the restart for it did not complete ({error}). \
         This chat keeps contributor mode {}.",
        before.as_str()
    );
    let id = format!("contributor-refused-{}", inner.next_seq);
    inner.append(ChatEvent::ItemCompleted {
        item: Item {
            presentation: Default::default(),
            id,
            turn_id: None,
            status: ItemStatus::Completed,
            body: ItemBody::notice(
                NoticeLevel::Warning,
                text.clone(),
                Some(notice_kind::SETTING_REFUSED),
            ),
        },
    });
    Err(text)
}

/// A billing change that is refused before anything changes: for a chat that is not
/// Claude's, which bills no Anthropic account, and together with other settings or a
/// contributor mode, which the Claude started again for it could still refuse, leaving the
/// change made and the command failed. Asking for the billing the chat already has changes
/// nothing and is not refused.
fn billing_refusal(info: &ChatInfo, command: &ChatCommand) -> Result<(), String> {
    let ChatCommand::Configure {
        model,
        effort,
        approval_mode,
        fast,
        contributor_mode,
        contributor_opt_out,
        claude_billing: Some(billing),
    } = command
    else {
        return Ok(());
    };
    if info.provider != Provider::Claude {
        return Err(format!(
            "Only a Claude chat bills a subscription or an API key; this {} chat has no such choice.",
            provider_name(info.provider)
        ));
    }
    if info.claude_billing == Some(*billing) {
        return Ok(());
    }
    if model.is_some()
        || effort.is_some()
        || approval_mode.is_some()
        || fast.is_some()
        || contributor_mode.is_some()
        || contributor_opt_out.is_some()
    {
        return Err("Change billing on its own: Claude starts again for it.".into());
    }
    Ok(())
}

/// Moves a Claude chat between the subscription and the API key (`ChatInfo::claude_billing`).
/// The key is in the environment `claude` starts with (`launch`), so no driver can be
/// configured with it, as with Muse's contributor mode (`configure_contributor_locked`), whose
/// discipline this follows: a chat with no process keeps the value for its next start; an idle
/// one has its process stopped and started again with the value, and the start resumes the same
/// Claude session with the standing model, effort and approval mode. While a turn runs or waits
/// for the user, or the process still starts, the change is refused and the chat keeps its
/// value. A start that fails leaves the chat failed, with the value it had, and a notice says
/// so.
fn configure_billing_locked(
    shared: &Shared,
    chat: &Arc<Chat>,
    billing: ClaudeBilling,
) -> Result<(), String> {
    let before = {
        let mut inner = lock(&chat.inner);
        if inner.info.claude_billing == Some(billing) {
            return Ok(());
        }
        let at_work = matches!(inner.info.state, ChatState::Running | ChatState::Waiting)
            || inner.open.turn.is_some()
            || !inner.open.approvals.is_empty()
            || !inner.open.questions.is_empty();
        if at_work {
            return Err(
                "Wait for the turn to finish, or interrupt it, before changing who this chat bills."
                    .into(),
            );
        }
        if inner.busy() {
            return Err("Claude is still starting; change billing once it is ready.".into());
        }
        if !inner.live() {
            inner.info.claude_billing = Some(billing);
            inner.publish_info();
            return Ok(());
        }
        inner.info.claude_billing
    };
    if shared.quit.load(Ordering::SeqCst) {
        return Err("the chat host is shutting down".into());
    }
    // What the new start would be told, before the running Claude is stopped for it: a
    // configuration that cannot be made (a key the Keychain will not give) leaves the chat
    // running as it was.
    let kept = || {
        before
            .unwrap_or_else(|| crate::settings::claude_chat_billing(&shared.home))
            .label()
    };
    let mut planned = chat.info();
    planned.claude_billing = Some(billing);
    if let Err(error) =
        (shared.providers.config)(&shared.home, &planned, planned.provider_thread_id.clone())
    {
        return Err(format!(
            "Billing was not changed: {}. This chat stays on {}.",
            error.trim_end_matches('.'),
            kept()
        ));
    }
    stop_locked(chat);
    {
        let mut inner = lock(&chat.inner);
        if inner.deleted || inner.broken {
            return Err("this chat cannot be configured".into());
        }
        inner.info.claude_billing = Some(billing);
        inner.publish_info();
    }
    let Err(error) = ensure_running_locked(shared, chat) else {
        return Ok(());
    };
    let mut inner = lock(&chat.inner);
    inner.info.claude_billing = before;
    inner.publish_info();
    let text = format!(
        "Billing was not changed: the restart for it did not complete ({error}). \
         This chat stays on {}.",
        kept()
    );
    let id = format!("billing-refused-{}", inner.next_seq);
    inner.append(ChatEvent::ItemCompleted {
        item: Item {
            presentation: Default::default(),
            id,
            turn_id: None,
            status: ItemStatus::Completed,
            body: ItemBody::notice(
                NoticeLevel::Warning,
                text.clone(),
                Some(notice_kind::SETTING_REFUSED),
            ),
        },
    });
    Err(text)
}

/// What a Muse chat cannot be asked, refused before anything changes whether or not its
/// process runs: a fast tier (Muse has none), or moving a conversation it already has into
/// or out of Plan (Muse fixes the read-only profile when a conversation starts).
fn muse_refusal(info: &ChatInfo, command: &ChatCommand) -> Result<(), String> {
    let (approval_mode, fast) = match command {
        ChatCommand::Configure {
            approval_mode,
            fast,
            ..
        } if info.provider == Provider::Muse => (*approval_mode, *fast),
        ChatCommand::Switch {
            provider: Provider::Muse,
            fast,
            ..
        } => (None, *fast),
        _ => return Ok(()),
    };
    if fast == Some(true) {
        return Err("Muse has no fast mode".into());
    }
    if let Some(mode) = approval_mode
        && info.provider_thread_id.is_some()
        && (mode == super::model::ApprovalMode::Plan)
            != (info.approval_mode == super::model::ApprovalMode::Plan)
    {
        return Err(super::muse::plan_switch_refusal(mode));
    }
    Ok(())
}

/// Moves the chat to `provider`. The chat keeps its id, its log, its subscribers and its
/// approval mode; its agent is stopped, the conversation so far is written down for the new
/// one (`handoff::chat_document`, kept as `context.md` in the chat's folder and named by
/// `ChatInfo::carried_over`), and the new agent starts on a thread of its own, with that
/// conversation in its instructions (`launch`). The log says it twice: an `Info` with the new
/// provider, which makes a `Transcript` drop the old provider's models and usage, and a
/// notice item that reads as a divider in any client. Refused while a turn runs or waits for
/// the user, and before anything changes when the new provider cannot have the chat (a Codex
/// account that cannot be used). A new agent that cannot start leaves the chat failed with
/// its reason, as a new chat does; the switch stands and the next message tries again.
fn switch(
    shared: &Shared,
    chat: &Arc<Chat>,
    provider: Provider,
    model: Option<String>,
    effort: Option<String>,
    fast: Option<bool>,
) -> Result<(), String> {
    let _turn = lock(&chat.lifecycle);
    muse_refusal(
        &chat.info(),
        &ChatCommand::Switch {
            provider,
            model: model.clone(),
            effort: effort.clone(),
            fast,
        },
    )?;
    // Decide under the same lock as the switch, so racing switches cannot
    // turn a settings change into a new session of the wrong provider.
    if provider == chat.info().provider {
        return configure_command_locked(
            shared,
            chat,
            ChatCommand::Configure {
                model,
                effort,
                approval_mode: None,
                fast,
                contributor_mode: None,
                contributor_opt_out: None,
                claude_billing: None,
            },
        );
    }
    {
        let before = {
            let inner = lock(&chat.inner);
            if inner.deleted {
                return Err("this chat was deleted".into());
            }
            if inner.broken {
                return Err("this chat's event log cannot be written".into());
            }
            let at_work = matches!(inner.info.state, ChatState::Running | ChatState::Waiting)
                || inner.open.turn.is_some()
                || !inner.open.approvals.is_empty()
                || !inner.open.questions.is_empty();
            if at_work {
                return Err(format!(
                    "Wait for the turn to finish, or interrupt it, before going on with {}.",
                    provider_name(provider)
                ));
            }
            inner.info.clone()
        };
        if shared.quit.load(Ordering::SeqCst) {
            return Err("the chat host is shutting down".into());
        }
        let account =
            (shared.providers.account)(&shared.home, provider, before.project_id.as_deref(), None)?;
        // Read before the agent stops, so a log that cannot be read leaves the chat as it was.
        // No turn is open, so nothing the conversation says is still on its way.
        let document = crate::handoff::chat_document(&shared.home, &before)?;
        stop_locked(chat);
        let mut inner = lock(&chat.inner);
        if inner.deleted || inner.broken {
            return Err("this chat cannot go on".into());
        }
        let path = inner.log.save_context(&document)?;
        // Retire the old provider's banners in the durable log before the new driver
        // can inherit outstanding notices or a client sees its Info. Keep them in history.
        let notices = log::read_notice_transcript(inner.log.dir())?;
        for mut item in notices.items {
            if let ItemBody::Notice { resolved, .. } = &mut item.body {
                if !*resolved {
                    *resolved = true;
                    inner.append(ChatEvent::ItemCompleted { item });
                }
            }
        }
        // Clear both memory and disk before changing provider. Even a configuration or
        // driver-start failure must leave no old login scope under the new provider.
        inner.log.reset_account_identity()?;
        inner.provider_account_identity = None;

        let info = &mut inner.info;
        info.provider = provider;
        info.provider_thread_id = None;
        info.model = model.filter(|model| !model.is_empty());
        info.effort = effort.filter(|effort| !effort.is_empty());
        info.fast = fast.unwrap_or(false);
        info.codex_account_id = account;
        // Its first Claude session bills as a new Claude chat would, unless it already chose
        // while it was Claude's before.
        if provider == Provider::Claude && info.claude_billing.is_none() {
            info.claude_billing = Some(crate::settings::claude_chat_billing(&shared.home));
        }
        if info.title == default_title(before.provider) {
            info.title = default_title(provider).to_owned();
        }
        info.carried_over = Some(CarriedOver {
            document: path,
            from: crate::handoff::chat_label(before.provider, &before.title, &before.id),
        });
        // Muse takes no instructions beside the conversation: the one so far goes with
        // the first message of its new conversation, so the notice says when.
        let text = match (provider, &info.model) {
            (Provider::Muse, _) => "Continued with Muse in a new conversation; \
                 the conversation so far goes to Muse with the next message."
                .to_owned(),
            (_, Some(model)) => format!(
                "Continued with {} ({model}), which has the conversation so far.",
                provider_name(provider)
            ),
            (_, None) => format!(
                "Continued with {}, which has the conversation so far.",
                provider_name(provider)
            ),
        };
        inner.publish_info();
        let id = format!("switch-{}", inner.next_seq);
        inner.append(ChatEvent::ItemCompleted {
            item: Item {
                presentation: Default::default(),
                id,
                turn_id: None,
                status: ItemStatus::Completed,
                body: ItemBody::notice(NoticeLevel::Info, text, None),
            },
        });
    }
    let _ = ensure_running_locked(shared, chat);
    Ok(())
}

fn configure(
    chat: &Chat,
    model: Option<String>,
    effort: Option<String>,
    approval_mode: Option<super::model::ApprovalMode>,
    fast: Option<bool>,
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
    if let Some(fast) = fast.filter(|fast| inner.info.fast != *fast) {
        inner.info.fast = fast;
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

/// `riwork chat serve` for `home`, with the environment a host may have.
fn host_command(exe: &Path, home: &Path) -> Command {
    let mut command = Command::new(exe);
    command
        .args(["chat", "serve"])
        .env("RIWORK_HOME", home)
        // The host is nobody's terminal: it must not carry this one's pane or
        // account into the chats it starts, nor the chat that started it. Nor the
        // opt-in out of the Cua guard on input to RiWork's windows: one host serves
        // every chat, orchestrators included, for as long as it runs.
        .env_remove("RIWORK_SHELL_ID")
        .env_remove("RIWORK_CHAT_ID")
        .env_remove(crate::cua::ALLOW_RIWORK_INPUT);
    crate::codex_accounts::scrub_injected_environment(&mut command);
    crate::anthropic_key::scrub_injected(&mut command);
    command
}

/// `riwork chat serve`, detached, with its output in the run directory's log.
fn spawn_host(exe: &Path, home: &Path, paths: &Paths) -> Result<std::process::Child, String> {
    let log = open_private(&paths.log, true)
        .map_err(|error| format!("Cannot open {}: {error}", paths.log.display()))?;
    if log.metadata().is_ok_and(|meta| meta.len() > 256 * 1024) {
        let _ = log.set_len(0);
    }
    let mut command = host_command(exe, home);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(
            log.try_clone().map_err(|error| error.to_string())?,
        ))
        .stderr(Stdio::from(log));
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
    // In a test build the current executable is the test harness: run with
    // `chat ensure` it would take those words as test filters, run the chat
    // tests again, and each of them would start it once more.
    if cfg!(test) {
        return Err("a test build never starts a chat host from its own executable".into());
    }
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

/// Re-emit changed notice bodies so replay, snapshot and live subscribers agree.
fn refresh_notice_dismissals(home: &Path, id: &str, inner: &mut Inner) -> Result<(), String> {
    let dir = log::chat_dir(home, id).ok_or("invalid chat id")?;
    let transcript = log::read_notice_transcript(&dir)?;
    for mut item in transcript.items {
        let changed = lock(&inner.dismissals).mark(
            inner.info.provider,
            inner.account_scope(),
            &mut item,
            super::notice_dismissals::now(),
        );
        if changed {
            inner.append(ChatEvent::ItemCompleted { item });
        }
    }
    Ok(())
}

fn dismiss_notice(shared: &Shared, chat: &Chat, item_id: &str) -> Result<(), String> {
    {
        let inner = lock(&chat.inner);
        if inner.deleted || inner.broken {
            return Err("this chat is unavailable".into());
        }
        let transcript = log::read_notice_transcript(&chat.dir)?;
        let item = transcript
            .items
            .iter()
            .find(|item| item.id == item_id)
            .ok_or("unknown notice item")?;
        lock(&shared.dismissals).dismiss(
            inner.info.provider,
            inner.account_scope(),
            item,
            super::notice_dismissals::now(),
        )?;
    }
    // Never hold the store lock while acquiring a chat lock (append uses the reverse).
    for chat in shared.all() {
        let mut inner = lock(&chat.inner);
        if !inner.deleted && !inner.broken {
            let id = inner.info.id.clone();
            refresh_notice_dismissals(&shared.home, &id, &mut inner)?;
        }
    }
    Ok(())
}
