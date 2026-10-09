//! A chat host with fake drivers, for the host's tests and the CLI's.
//!
//! A `StartDriver` is a plain function, so a fake keeps what it needs in a
//! registry keyed by the chat's working directory; every `TestHost` has a
//! working directory of its own.

use super::client::{Client, Subscription};
use super::driver::{Driver, DriverConfig};
use super::host::{Host, Options, Providers, StartError};
use super::model::*;
use super::wire::Envelope;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use uuid::Uuid;

/// What the fake driver of one working directory did and what a test may make
/// it do.
#[derive(Default)]
pub struct Fake {
    /// The configuration of every start, in order.
    pub starts: Mutex<Vec<DriverConfig>>,
    /// Every command that reached a driver.
    pub commands: Mutex<Vec<ChatCommand>>,
    pub shutdowns: AtomicUsize,
    /// The next start fails with this message.
    pub fail_start: Mutex<Option<String>>,
    /// The next start succeeds, and the provider is gone before it returns.
    pub vanish_on_start: AtomicBool,
    /// The newest run's number and its end of the event channel.
    sender: Mutex<Option<(usize, Sender<ChatEvent>)>>,
    started: AtomicUsize,
}

impl Fake {
    /// Sends an event as the running driver would.
    pub fn emit(&self, event: ChatEvent) {
        if let Some((_, sender)) = self.sender.lock().unwrap().as_ref() {
            let _ = sender.send(event);
        }
    }

    /// A second end of the running driver's event channel.
    pub fn sender(&self) -> Option<Sender<ChatEvent>> {
        self.sender
            .lock()
            .unwrap()
            .as_ref()
            .map(|(_, sender)| sender.clone())
    }

    /// The provider process disappears without a word: the channel closes.
    pub fn vanish(&self) {
        *self.sender.lock().unwrap() = None;
    }

    /// Closes the channel of run `number`, unless a newer run has replaced it.
    fn release(&self, number: usize) {
        let mut sender = self.sender.lock().unwrap();
        if sender
            .as_ref()
            .is_some_and(|(current, _)| *current == number)
        {
            *sender = None;
        }
    }

    pub fn start_count(&self) -> usize {
        self.starts.lock().unwrap().len()
    }

    pub fn commands(&self) -> Vec<ChatCommand> {
        self.commands.lock().unwrap().clone()
    }
}

fn registry() -> &'static Mutex<HashMap<PathBuf, Arc<Fake>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Arc<Fake>>>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}

pub fn fake_for(cwd: &Path) -> Arc<Fake> {
    registry()
        .lock()
        .unwrap()
        .entry(cwd.to_owned())
        .or_default()
        .clone()
}

struct FakeDriver {
    fake: Arc<Fake>,
    number: usize,
    thread_id: String,
    turn: u64,
}

fn agent_item(id: &str, turn: &str, text: &str, status: ItemStatus) -> Item {
    Item {
        presentation: Default::default(),
        id: id.into(),
        turn_id: Some(turn.into()),
        status,
        body: ItemBody::AgentMessage { text: text.into() },
    }
}

impl Driver for FakeDriver {
    /// `Send` runs a scripted turn. The text `hang` starts one that never
    /// ends, and `flood:N:BYTES` streams N deltas of that size.
    fn command(&mut self, command: ChatCommand) -> Result<(), String> {
        self.fake.commands.lock().unwrap().push(command.clone());
        let text = match command {
            ChatCommand::Send { text } | ChatCommand::SendAttachments { text, .. } => text,
            _ => return Ok(()),
        };
        self.turn += 1;
        // Unique across runs, as a real provider's ids are: a resumed or switched chat
        // folds the items of every run into one transcript.
        let turn = format!("turn-{}-{}", self.number, self.turn);
        let agent = format!("agent-{}-{}", self.number, self.turn);
        let fake = &self.fake;
        fake.emit(ChatEvent::State {
            state: ChatState::Running,
        });
        fake.emit(ChatEvent::TurnStarted {
            turn_id: turn.clone(),
        });
        fake.emit(ChatEvent::ItemStarted {
            item: Item {
                presentation: Default::default(),
                id: format!("user-{}-{}", self.number, self.turn),
                turn_id: Some(turn.clone()),
                status: ItemStatus::Completed,
                body: ItemBody::UserMessage { text: text.clone() },
            },
        });
        fake.emit(ChatEvent::ItemStarted {
            item: agent_item(&agent, &turn, "", ItemStatus::InProgress),
        });
        if text == "hang" {
            return Ok(());
        }
        let mut reply = String::new();
        if let Some(rest) = text.strip_prefix("flood:") {
            let (count, bytes) = rest.split_once(':').unwrap();
            let chunk = "x".repeat(bytes.parse().unwrap());
            for _ in 0..count.parse::<usize>().unwrap() {
                fake.emit(ChatEvent::ItemDelta {
                    item_id: agent.clone(),
                    delta: Delta::Text(chunk.clone()),
                });
                // Paced, so that a subscriber that reads keeps up.
                std::thread::sleep(Duration::from_millis(2));
            }
        } else {
            reply = format!("echo: {text}");
            fake.emit(ChatEvent::ItemDelta {
                item_id: agent.clone(),
                delta: Delta::Text(reply.clone()),
            });
        }
        fake.emit(ChatEvent::ItemCompleted {
            item: agent_item(&agent, &turn, &reply, ItemStatus::Completed),
        });
        fake.emit(ChatEvent::TurnCompleted {
            turn_id: turn,
            outcome: TurnOutcome::Completed,
        });
        fake.emit(ChatEvent::State {
            state: ChatState::Idle,
        });
        Ok(())
    }

    fn provider_thread_id(&self) -> Option<String> {
        Some(self.thread_id.clone())
    }

    fn shutdown(&mut self) {
        self.fake.shutdowns.fetch_add(1, Ordering::SeqCst);
    }
}

impl Drop for FakeDriver {
    /// A driver keeps its end of the event channel until it is dropped, as a
    /// real one that sends from `command` does.
    fn drop(&mut self) {
        self.fake.release(self.number);
    }
}

fn fake_start(config: DriverConfig, events: Sender<ChatEvent>) -> Result<Box<dyn Driver>, String> {
    let fake = fake_for(&config.cwd);
    fake.starts.lock().unwrap().push(config.clone());
    if let Some(message) = fake.fail_start.lock().unwrap().take() {
        return Err(message);
    }
    let number = fake.started.fetch_add(1, Ordering::SeqCst) + 1;
    let thread_id = config
        .resume
        .clone()
        .unwrap_or_else(|| format!("thread-{number}"));
    *fake.sender.lock().unwrap() = Some((number, events));
    fake.emit(ChatEvent::State {
        state: ChatState::Starting,
    });
    fake.emit(ChatEvent::Info {
        info: placeholder_info(&config, &thread_id),
    });
    fake.emit(ChatEvent::State {
        state: ChatState::Idle,
    });
    if fake.vanish_on_start.swap(false, Ordering::SeqCst) {
        fake.vanish();
    }
    Ok(Box::new(FakeDriver {
        fake,
        number,
        thread_id,
        turn: 0,
    }))
}

/// What a driver can say about a chat: it does not know the chat's id or title.
pub fn placeholder_info(config: &DriverConfig, thread_id: &str) -> ChatInfo {
    ChatInfo {
        parent_id: None,
        user_title: None,
        first_user_message: None,
        provider_title: None,

        id: String::new(),
        provider: config.provider,
        project_id: None,
        worktree_id: None,
        cwd: config.cwd.clone(),
        title: String::new(),
        created_at_unix: 0,
        provider_thread_id: Some(thread_id.into()),
        model: config.model.clone(),
        effort: config.effort.clone(),
        fast: config.fast,
        approval_mode: config.approval_mode,
        codex_account_id: None,
        state: ChatState::Idle,
        orchestrator: None,
        carried_over: None,
    }
}

fn fake_config(
    _home: &Path,
    info: &ChatInfo,
    resume: Option<String>,
) -> Result<DriverConfig, String> {
    Ok(DriverConfig {
        provider: info.provider,
        program: format!("/fake/{:?}", info.provider).into(),
        cwd: info.cwd.clone(),
        approval_mode: info.approval_mode,
        model: info.model.clone(),
        effort: info.effort.clone(),
        fast: info.fast,
        resume,
        outstanding_notices: Default::default(),
        extra_args: Vec::new(),
        instructions: super::launch::instructions(info),
        env: Vec::new(),
        env_remove: Vec::new(),
    })
}

fn fake_account(
    _home: &Path,
    provider: Provider,
    _project: Option<&str>,
    requested: Option<&str>,
) -> Result<Option<String>, String> {
    Ok((provider == Provider::Codex).then(|| requested.unwrap_or("account-a").to_owned()))
}

pub fn fake_providers() -> Providers {
    Providers {
        codex: fake_start,
        claude: fake_start,
        account: fake_account,
        config: fake_config,
    }
}

/// Options with short timeouts, so a test that waits for a drop does not wait
/// long.
pub fn quick_options() -> Options {
    Options {
        idle: Duration::from_secs(60),
        write_timeout: Duration::from_secs(5),
        ..Options::default()
    }
}

/// A fresh, short `RIWORK_HOME` (a Unix socket path holds 103 bytes).
pub fn short_home() -> PathBuf {
    let home = std::env::temp_dir().join(format!("rwh-{}", &Uuid::new_v4().to_string()[..6]));
    std::fs::create_dir_all(&home).unwrap();
    home
}

/// An exclusively created private root for protocol-only socket fixtures.
/// This helper never constructs a Host, provider or SessionManager. It lives
/// under `/tmp`, not `$TMPDIR`: macOS's per-user temp directory is long enough
/// that `<root>/run/chat.sock` would exceed a Unix socket path's 103 bytes.
pub fn private_socket_fixture_home() -> PathBuf {
    use std::os::unix::fs::DirBuilderExt;
    let root = Path::new("/tmp").join(format!("rwcp-{}", &Uuid::new_v4().simple().to_string()[..12]));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .unwrap();
    root.canonicalize().unwrap()
}

/// Fixture-only accept/read/write deadlines; no production host or socket.
pub fn bounded_fixture_accept(
    listener: &std::os::unix::net::UnixListener,
) -> std::os::unix::net::UnixStream {
    listener.set_nonblocking(true).unwrap();
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                // macOS can inherit O_NONBLOCK from the listener. The fixture
                // reads bounded frames with SO_RCVTIMEO rather than polling.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                return stream;
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < end =>
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("private fixture accept failed or timed out: {error}"),
        }
    }
}

/// Starts a host with fake drivers. Another test may fork a child (a stub
/// script) at the moment a previous host on the home lets go of its lock, and
/// the child keeps the lock until it execs: `AlreadyRunning` is asked again.
pub fn start_host(home: &Path, options: Options) -> Host {
    start_host_with(home, options, fake_providers())
}
fn start_host_with(home: &Path, options: Options, providers: Providers) -> Host {
    for _ in 0..100 {
        match Host::start(home, providers, options) {
            Err(StartError::AlreadyRunning) => std::thread::sleep(Duration::from_millis(20)),
            other => return other.unwrap(),
        }
    }
    panic!("the lock of the previous host was never released");
}

/// A running host with fake drivers on a throwaway home.
pub struct TestHost {
    pub home: PathBuf,
    host: Option<Host>,
    providers: Providers,
}

impl TestHost {
    pub fn new() -> Self {
        Self::with(quick_options())
    }

    pub fn with(options: Options) -> Self {
        Self::with_providers(options, fake_providers())
    }
    pub fn with_providers(options: Options, providers: Providers) -> Self {
        let home = short_home();
        std::fs::create_dir_all(home.join("work")).unwrap();
        let mut test = Self {
            home,
            host: None,
            providers,
        };
        test.start(options);
        test
    }

    fn start(&mut self, options: Options) {
        self.host = Some(start_host_with(&self.home, options, self.providers));
    }

    /// Stops only this isolated fake host, retaining its fixture files.
    pub fn stop(&mut self) {
        drop(self.host.take());
    }

    /// Ends the host as an exit would and starts another on the same home.
    pub fn restart(&mut self, options: Options) {
        drop(self.host.take());
        self.start(options);
    }

    pub fn socket(&self) -> PathBuf {
        super::client::socket_path(&self.home)
    }

    pub fn client(&self) -> Client {
        Client::connect(&self.socket()).unwrap()
    }

    /// The chats' working directory, which also names their fake.
    pub fn work(&self) -> PathBuf {
        self.home.join("work")
    }

    pub fn fake(&self) -> Arc<Fake> {
        fake_for(&self.work())
    }

    pub fn new_chat(&self, provider: Provider) -> NewChat {
        NewChat {
            parent_id: None,

            provider,
            project_id: None,
            worktree_id: None,
            cwd: self.work(),
            codex_account_id: None,
            title: None,
            approval_mode: ApprovalMode::Supervised,
            model: None,
            effort: None,
            orchestrator: None,
            fast: false,
        }
    }

    pub fn create(&self, provider: Provider) -> ChatInfo {
        self.client().create(self.new_chat(provider)).unwrap()
    }

    /// A chat in another working directory, which has a fake of its own.
    pub fn create_in(&self, directory: &str, provider: Provider) -> ChatInfo {
        let mut new = self.new_chat(provider);
        new.cwd = self.home.join(directory);
        std::fs::create_dir_all(&new.cwd).unwrap();
        self.client().create(new).unwrap()
    }

    /// The chat's log as the host wrote it.
    pub fn log(&self, chat_id: &str) -> Vec<Envelope> {
        let path = self.home.join("chats").join(chat_id).join("events.jsonl");
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    pub fn info(&self, chat_id: &str) -> ChatInfo {
        self.client()
            .list()
            .unwrap()
            .into_iter()
            .find(|chat| chat.id == chat_id)
            .unwrap()
    }

    pub fn wait_for_info(&self, chat_id: &str, wanted: impl Fn(&ChatInfo) -> bool) -> ChatInfo {
        let mut info = self.info(chat_id);
        eventually(|| {
            info = self.info(chat_id);
            wanted(&info)
        });
        info
    }

    pub fn wait_for_state(&self, chat_id: &str, wanted: impl Fn(&ChatState) -> bool) -> ChatInfo {
        self.wait_for_info(chat_id, |info| wanted(&info.state))
    }

    pub fn wait_for_log(&self, chat_id: &str, done: impl Fn(&[Envelope]) -> bool) -> Vec<Envelope> {
        let mut log = Vec::new();
        eventually(|| {
            log = self.log(chat_id);
            done(&log)
        });
        log
    }
}

impl Drop for TestHost {
    fn drop(&mut self) {
        drop(self.host.take());
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// Polls `check` until it holds; a test that waits longer than this has failed.
pub fn eventually(mut check: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(20);
    while !check() {
        assert!(Instant::now() < end, "timed out waiting");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The envelopes of a `Subscription`, read on a thread of their own so a test
/// can wait for them with a timeout.
pub struct Follower {
    receiver: mpsc::Receiver<Envelope>,
}

impl Follower {
    pub fn open(socket: &Path, chat_id: &str, since: u64) -> Result<Self, String> {
        let mut subscription = Subscription::open(socket, chat_id, since)?;
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            while let Some(Ok(envelope)) = subscription.next_envelope() {
                if sender.send(envelope).is_err() {
                    break;
                }
            }
        });
        Ok(Self { receiver })
    }

    pub fn next(&self) -> Option<Envelope> {
        self.receiver.recv_timeout(Duration::from_secs(20)).ok()
    }

    /// `count` envelopes, or fewer if the connection ends first.
    pub fn take(&self, count: usize) -> Vec<Envelope> {
        (0..count).map_while(|_| self.next()).collect()
    }

    /// Everything until the host ends the connection.
    pub fn until_closed(&self) -> Vec<Envelope> {
        let mut all = Vec::new();
        loop {
            match self.receiver.recv_timeout(Duration::from_secs(20)) {
                Ok(envelope) => all.push(envelope),
                Err(mpsc::RecvTimeoutError::Disconnected) => return all,
                Err(mpsc::RecvTimeoutError::Timeout) => panic!("the connection did not end"),
            }
        }
    }
}

/// The numbering is 1-based from `first` and has no gaps.
pub fn assert_gapless(envelopes: &[Envelope], first: u64) {
    for (offset, envelope) in envelopes.iter().enumerate() {
        assert_eq!(envelope.seq, first + offset as u64, "{envelopes:#?}");
    }
}
