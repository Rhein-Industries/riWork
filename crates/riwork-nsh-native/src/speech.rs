//! Local MLX speech models for dictation: a pinned catalog, downloads into the Hugging Face
//! cache, and one long-lived worker process that keeps the chosen model loaded between
//! dictations (a large model takes seconds to load; dictation should not wait for that twice).
//!
//! The worker (speech/worker.py) runs in RiWork's own Python runtime, installed on request with
//! `uv sync --frozen` from speech/pyproject.toml and speech/uv.lock into RIWORK_HOME/speech.
//! Weights live in the standard Hugging Face cache, shared with other local apps such as
//! riTranscribe, and every model is pinned to one commit with the exact files and sizes it needs:
//! whether a model is on this Mac, and how far its download is, is read from the cache itself.
//! Only downloads use the network; the worker loads models with the Hub offline.
use std::{
    collections::{HashMap, VecDeque},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use nsh_core::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{Subscription, hub::Hub, service};

const CATALOG: &str = include_str!("../speech/models.json");
const PYPROJECT: &str = include_str!("../speech/pyproject.toml");
const LOCK: &str = include_str!("../speech/uv.lock");
const WORKER: &str = include_str!("../speech/worker.py");

/// One choice of what dictation listens with.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelSpec {
    pub key: String,
    pub label: String,
    pub detail: String,
    /// `apple` (Apple's on-device speech, in the riwork-speech helper), or the mlx-audio model
    /// family the worker loads: `parakeet`, `whisper` or `nemotron`.
    pub family: String,
    pub repo: Option<String>,
    pub revision: Option<String>,
    /// False for a model that can no longer be fetched but is used while it is in the cache.
    pub downloadable: bool,
    pub draft_seconds: Option<f64>,
    pub bytes: u64,
    /// Every file the model needs at its pinned commit, with its exact size.
    pub files: Vec<(String, u64)>,
}

impl ModelSpec {
    pub fn is_apple(&self) -> bool {
        self.family == "apple"
    }
    /// How long a stopped dictation may take to settle its last words. A large model decodes the
    /// final phrase in seconds; Apple's helper has its own five-second rule.
    pub fn finish_deadline(&self) -> Duration {
        match self.family.as_str() {
            "apple" => crate::dictation::STOPPING_DEADLINE,
            "nemotron" => Duration::from_secs(40),
            _ => Duration::from_secs(20),
        }
    }
}

pub fn catalog() -> &'static [ModelSpec] {
    static MODELS: OnceLock<Vec<ModelSpec>> = OnceLock::new();
    MODELS.get_or_init(|| {
        #[derive(Deserialize)]
        struct Catalog {
            models: Vec<ModelSpec>,
        }
        serde_json::from_str::<Catalog>(CATALOG)
            .expect("speech/models.json is valid")
            .models
    })
}

pub fn model(key: &str) -> Option<&'static ModelSpec> {
    catalog().iter().find(|spec| spec.key == key)
}

/// The model a `dictation_model` setting names: Apple's for a key this build does not know.
pub fn effective(key: &str) -> &'static ModelSpec {
    model(key).unwrap_or_else(|| {
        model(riwork_core::settings::DICTATION_APPLE).expect("the catalog has Apple's speech")
    })
}

// MARK: The Hugging Face cache

/// Where the Hugging Face libraries keep models: `HF_HUB_CACHE`, `HF_HOME/hub`,
/// `XDG_CACHE_HOME/huggingface/hub`, else `~/.cache/huggingface/hub`. The worker is handed the
/// same directory explicitly, so both sides always agree.
pub fn hub_cache() -> Option<PathBuf> {
    let var = |name: &str| {
        std::env::var_os(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    var("HF_HUB_CACHE")
        .or_else(|| var("HF_HOME").map(|home| home.join("hub")))
        .or_else(|| var("XDG_CACHE_HOME").map(|cache| cache.join("huggingface/hub")))
        .or_else(|| var("HOME").map(|home| home.join(".cache/huggingface/hub")))
}

fn repo_dir(hub: &Path, repo: &str) -> PathBuf {
    hub.join(format!("models--{}", repo.replace('/', "--")))
}

/// How much of a model is in the cache.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cached {
    /// Every pinned file is there at its exact size.
    pub complete: bool,
    /// Whole files, plus what downloads in progress have written so far.
    pub bytes: u64,
}

pub fn cached(spec: &ModelSpec, hub: &Path) -> Cached {
    let (Some(repo), Some(revision)) = (&spec.repo, &spec.revision) else {
        return Cached { complete: spec.is_apple(), bytes: 0 };
    };
    let repo = repo_dir(hub, repo);
    let snapshot = repo.join("snapshots").join(revision);
    let mut cached = Cached { complete: !spec.files.is_empty(), bytes: 0 };
    for (name, size) in &spec.files {
        // Snapshot entries are symlinks into blobs/; metadata follows them.
        match std::fs::metadata(snapshot.join(name)) {
            Ok(metadata) if metadata.len() == *size => cached.bytes += size,
            _ => cached.complete = false,
        }
    }
    if !cached.complete {
        if let Ok(entries) = std::fs::read_dir(repo.join("blobs")) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().ends_with(".incomplete") {
                    cached.bytes += entry.metadata().map(|metadata| metadata.len()).unwrap_or(0);
                }
            }
        }
        cached.bytes = cached.bytes.min(spec.bytes);
    }
    cached
}

// MARK: The runtime

fn runtime_hash() -> String {
    let digest = Sha256::digest(format!("{PYPROJECT}\0{LOCK}"));
    digest.iter().take(6).map(|byte| format!("{byte:02x}")).collect()
}

fn speech_home() -> Result<PathBuf, String> {
    Ok(service::process_home()?.join("speech"))
}

/// Keyed by the lock: a build with other pinned packages installs a runtime of its own.
fn runtime_dir(speech: &Path) -> PathBuf {
    speech.join(format!("runtime-{}", runtime_hash()))
}

fn python(runtime: &Path) -> PathBuf {
    runtime.join(".venv/bin/python")
}

fn runtime_ready(runtime: &Path) -> bool {
    runtime.join("installed").is_file() && python(runtime).is_file()
}

/// `uv`: `RIWORK_UV`, else on PATH, else where its installers put it. An app opened from the
/// Dock has a minimal PATH, so the usual places are looked at too.
pub fn find_uv() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("RIWORK_UV").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(path)).filter(|path| path.is_file());
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).map(|dir| dir.join("uv")).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .chain(["/opt/homebrew/bin/uv", "/usr/local/bin/uv"].map(PathBuf::from))
        .chain(home.iter().flat_map(|home| [home.join(".local/bin/uv"), home.join(".cargo/bin/uv")]))
        .find(|path| path.is_file())
}

fn write_if_changed(path: &Path, contents: &str) -> Result<(), String> {
    if std::fs::read_to_string(path).ok().as_deref() == Some(contents) {
        return Ok(());
    }
    let staged = path.with_extension("staged");
    std::fs::write(&staged, contents).map_err(service::io_error)?;
    std::fs::rename(&staged, path).map_err(service::io_error)
}

/// Runs a process to its end, keeping the last lines it wrote to stderr for an error; `cancel`
/// stops it. `Ok(true)` when it succeeded, `Ok(false)` when it was cancelled.
fn run(mut command: Command, cancel: &AtomicBool, what: &str) -> Result<bool, String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not start {what}: {error}"))?;
    let tail = Arc::new(Mutex::new(VecDeque::<String>::new()));
    let reader = child.stderr.take().map(|stderr| {
        let tail = tail.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let line = line.trim().to_owned();
                if line.is_empty() || line.starts_with("Warning: You are sending unauthenticated") {
                    continue;
                }
                let mut tail = tail.lock().unwrap();
                tail.push_back(line);
                if tail.len() > 12 {
                    tail.pop_front();
                }
            }
        })
    });
    let status = loop {
        if cancel.load(Ordering::Acquire) {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(false);
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(error) => return Err(format!("{what} failed: {error}")),
        }
    };
    if let Some(reader) = reader {
        let _ = reader.join();
    }
    if status.success() {
        return Ok(true);
    }
    let tail = tail.lock().unwrap();
    // A Python traceback ends with the exception; SystemExit's message is the whole line.
    let reason = tail.back().cloned().unwrap_or_else(|| format!("exit status {status}"));
    Err(format!("{what} failed: {reason}"))
}

/// Installs the pinned runtime: Python 3.12 and mlx-audio with everything it needs, exactly as
/// locked. Older runtimes are removed after a new one is in place.
fn install_runtime(speech: &Path, cancel: &AtomicBool) -> Result<bool, String> {
    let runtime = runtime_dir(speech);
    if runtime_ready(&runtime) {
        return Ok(true);
    }
    let uv = find_uv().ok_or_else(|| {
        "Local speech models need uv to set up their runtime. Install it (brew install uv) and try again."
            .to_owned()
    })?;
    std::fs::create_dir_all(&runtime).map_err(service::io_error)?;
    write_if_changed(&runtime.join("pyproject.toml"), PYPROJECT)?;
    write_if_changed(&runtime.join("uv.lock"), LOCK)?;
    let mut command = Command::new(uv);
    command
        .args(["sync", "--frozen", "--no-progress", "--python", "3.12", "--project"])
        .arg(&runtime)
        .env("UV_PROJECT_ENVIRONMENT", runtime.join(".venv"));
    if !run(command, cancel, "Setting up the speech runtime")? {
        return Ok(false);
    }
    std::fs::write(runtime.join("installed"), runtime_hash()).map_err(service::io_error)?;
    if let Ok(entries) = std::fs::read_dir(speech) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("runtime-") && entry.path() != runtime {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
    Ok(true)
}

fn worker_script(runtime: &Path) -> Result<PathBuf, String> {
    let path = runtime.join("worker.py");
    write_if_changed(&path, WORKER)?;
    Ok(path)
}

fn worker_command(runtime: &Path, hub: &Path) -> Result<Command, String> {
    let mut command = Command::new(python(runtime));
    command
        .arg("-I")
        .arg(worker_script(runtime)?)
        .env("HF_HUB_CACHE", hub)
        .env("HF_HUB_DISABLE_TELEMETRY", "1")
        .env("HF_HUB_DISABLE_PROGRESS_BARS", "1")
        .current_dir(runtime);
    Ok(command)
}

// MARK: Downloads and the published state

struct Download {
    cancel: Arc<AtomicBool>,
    /// `runtime` while the runtime is set up first, then `model`.
    stage: &'static str,
}

#[derive(Default)]
struct State {
    downloads: HashMap<String, Download>,
    errors: HashMap<String, String>,
}

struct Manager {
    speech: PathBuf,
    hub: Option<PathBuf>,
    state: Mutex<State>,
    topic: Hub<Value>,
    worker: Mutex<Option<Arc<Worker>>>,
    ticking: AtomicBool,
}

static MANAGER: OnceLock<Arc<Manager>> = OnceLock::new();

fn manager() -> Result<Arc<Manager>, String> {
    if let Some(manager) = MANAGER.get() {
        return Ok(manager.clone());
    }
    let speech = speech_home()?;
    let manager = Arc::new(Manager {
        hub: hub_cache(),
        topic: Hub::new(Value::Null),
        state: Mutex::new(State::default()),
        worker: Mutex::new(None),
        ticking: AtomicBool::new(false),
        speech,
    });
    let _ = MANAGER.set(manager);
    let manager = MANAGER.get().unwrap().clone();
    manager.topic.publish(manager.snapshot());
    Ok(manager)
}

impl Manager {
    fn snapshot(&self) -> Value {
        let state = self.state.lock().unwrap();
        let runtime = runtime_ready(&runtime_dir(&self.speech));
        let models: Vec<Value> = catalog()
            .iter()
            .map(|spec| {
                let cached = self.hub.as_deref().map(|hub| cached(spec, hub)).unwrap_or_default();
                let download = state.downloads.get(&spec.key);
                let status = if spec.is_apple() {
                    "builtin"
                } else if let Some(download) = download {
                    if download.stage == "runtime" { "installing" } else { "downloading" }
                } else if cached.complete && runtime {
                    "ready"
                } else if cached.complete {
                    "setup"
                } else if !spec.downloadable {
                    "unavailable"
                } else {
                    "missing"
                };
                json!({
                    "key": spec.key, "label": spec.label, "detail": spec.detail, "family": spec.family,
                    "bytes": spec.bytes, "cachedBytes": cached.bytes, "downloadable": spec.downloadable,
                    "status": status, "error": state.errors.get(&spec.key),
                })
            })
            .collect();
        json!({
            "models": models,
            "runtime": {"ready": runtime, "uv": find_uv().is_some()},
            "cache": self.hub.as_ref().map(|hub| hub.display().to_string()),
        })
    }

    fn publish(&self) {
        self.topic.publish(self.snapshot());
    }

    /// Republishes progress twice a second while anything downloads, then stops.
    fn tick(self: &Arc<Self>) {
        if self.ticking.swap(true, Ordering::AcqRel) {
            return;
        }
        let manager = self.clone();
        let _ = std::thread::Builder::new().name("nsh-speech-progress".into()).spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(500));
                // Stop under the lock a new download takes, so it starts the next ticker.
                let idle = {
                    let state = manager.state.lock().unwrap();
                    let idle = state.downloads.is_empty();
                    if idle {
                        manager.ticking.store(false, Ordering::Release);
                    }
                    idle
                };
                // Published after the check: the last snapshot is never one from before the end.
                manager.publish();
                if idle {
                    return;
                }
            }
        });
    }

    /// Sets up the runtime when it is missing, then fetches the model's pinned files. A model
    /// already in the cache only needs the runtime.
    fn download(self: &Arc<Self>, key: &str) -> Result<(), String> {
        let spec = model(key).filter(|spec| !spec.is_apple()).ok_or("invalid: unknown speech model")?;
        let hub = self.hub.clone().ok_or("The Hugging Face cache folder could not be found (HOME is not set).")?;
        let complete = cached(spec, &hub).complete;
        if !complete && !spec.downloadable {
            return Err(format!("{} can no longer be downloaded.", spec.label));
        }
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut state = self.state.lock().unwrap();
            if state.downloads.contains_key(key) {
                return Ok(());
            }
            let runtime = runtime_ready(&runtime_dir(&self.speech));
            if complete && runtime {
                return Ok(());
            }
            state.errors.remove(key);
            state.downloads.insert(key.to_owned(), Download {
                cancel: cancel.clone(),
                stage: if runtime { "model" } else { "runtime" },
            });
        }
        self.publish();
        self.tick();
        let manager = self.clone();
        let spec = spec.clone();
        std::thread::Builder::new()
            .name("nsh-speech-download".into())
            .spawn(move || {
                let result = manager.fetch(&spec, &hub, &cancel);
                let mut state = manager.state.lock().unwrap();
                state.downloads.remove(&spec.key);
                if let Err(error) = result {
                    state.errors.insert(spec.key.clone(), error);
                }
                drop(state);
                manager.publish();
            })
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    fn fetch(&self, spec: &ModelSpec, hub: &Path, cancel: &AtomicBool) -> Result<(), String> {
        if !install_runtime(&self.speech, cancel)? {
            return Ok(());
        }
        if let Some(download) = self.state.lock().unwrap().downloads.get_mut(&spec.key) {
            download.stage = "model";
        }
        self.publish();
        if cached(spec, hub).complete {
            return Ok(());
        }
        let runtime = runtime_dir(&self.speech);
        let mut command = worker_command(&runtime, hub)?;
        command
            .arg("download")
            .arg(serde_json::to_string(spec).map_err(|error| error.to_string())?);
        run(command, cancel, &format!("Downloading {}", spec.label))?;
        Ok(())
    }

    fn cancel(&self, key: &str) {
        if let Some(download) = self.state.lock().unwrap().downloads.get(key) {
            download.cancel.store(true, Ordering::Release);
        }
    }

    /// The running worker, or a new one.
    fn worker(&self) -> Result<Arc<Worker>, String> {
        let mut slot = self.worker.lock().unwrap();
        if let Some(worker) = slot.as_ref().filter(|worker| worker.alive.load(Ordering::Acquire)) {
            return Ok(worker.clone());
        }
        let hub = self.hub.clone().ok_or("The Hugging Face cache folder could not be found.")?;
        let worker = Worker::spawn(&runtime_dir(&self.speech), &hub, &self.speech)?;
        *slot = Some(worker.clone());
        Ok(worker)
    }
}

// MARK: The worker

/// What the worker says about one dictation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkerEvent {
    Note(String),
    Loaded,
    Heard(String),
    Finished(String),
    Failed { problem: String, detail: String },
}

impl WorkerEvent {
    fn parse(line: &str) -> Option<(u64, Self)> {
        #[derive(Deserialize)]
        struct Wire {
            event: String,
            session: u64,
            #[serde(default)]
            text: String,
            #[serde(default)]
            problem: String,
            #[serde(default)]
            detail: String,
        }
        let wire: Wire = serde_json::from_str(line).ok()?;
        let event = match wire.event.as_str() {
            "note" => Self::Note(wire.text),
            "loaded" => Self::Loaded,
            "heard" => Self::Heard(wire.text),
            "finished" => Self::Finished(wire.text),
            "failed" => Self::Failed { problem: wire.problem, detail: wire.detail },
            _ => return None,
        };
        Some((wire.session, event))
    }
}

/// The long-lived `worker.py serve` process. It ends when RiWork's end of its stdin closes.
/// Requests go through a writer thread, so audio from the microphone never waits for a busy
/// worker to read: the capture helper's pipe keeps draining and its watchdog stays quiet.
struct Worker {
    outbox: async_channel::Sender<String>,
    child: Mutex<Child>,
    routes: Mutex<HashMap<u64, async_channel::Sender<WorkerEvent>>>,
    alive: AtomicBool,
}

impl Worker {
    fn spawn(runtime: &Path, hub: &Path, speech: &Path) -> Result<Arc<Self>, String> {
        // The worker's own diagnostics (its stderr), kept across restarts and cut at 1 MB.
        let log_path = speech.join("worker.log");
        if std::fs::metadata(&log_path).is_ok_and(|metadata| metadata.len() > 1 << 20) {
            let _ = std::fs::rename(&log_path, speech.join("worker.log.1"));
        }
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(service::io_error)?;
        let mut child = worker_command(runtime, hub)?
            .arg("serve")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log))
            .spawn()
            .map_err(|error| format!("could not start the speech engine: {error}"))?;
        let stdin = child.stdin.take().ok_or("the speech engine has no input")?;
        let stdout = child.stdout.take().ok_or("the speech engine has no output")?;
        let (outbox, requests) = async_channel::unbounded::<String>();
        std::thread::Builder::new()
            .name("nsh-speech-requests".into())
            .spawn(move || write_requests(stdin, &requests))
            .map_err(|error| error.to_string())?;
        let worker = Arc::new(Self {
            outbox,
            child: Mutex::new(child),
            routes: Mutex::new(HashMap::new()),
            alive: AtomicBool::new(true),
        });
        let reader = Arc::downgrade(&worker);
        std::thread::Builder::new()
            .name("nsh-speech-worker".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    let Some(worker) = reader.upgrade() else { return };
                    let Some((session, event)) = WorkerEvent::parse(&line) else { continue };
                    let last = matches!(event, WorkerEvent::Finished(_) | WorkerEvent::Failed { .. });
                    let mut routes = worker.routes.lock().unwrap();
                    if let Some(route) = routes.get(&session) {
                        let _ = route.send_blocking(event);
                    }
                    if last {
                        routes.remove(&session);
                    }
                }
                if let Some(worker) = reader.upgrade() {
                    worker.alive.store(false, Ordering::Release);
                    for (_, route) in worker.routes.lock().unwrap().drain() {
                        let _ = route.send_blocking(WorkerEvent::Failed {
                            problem: "failed".into(),
                            detail: "the speech engine stopped".into(),
                        });
                    }
                    let _ = worker.child.lock().unwrap().wait();
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(worker)
    }

    fn send(&self, message: Value) -> bool {
        self.alive.load(Ordering::Acquire) && self.outbox.try_send(message.to_string()).is_ok()
    }
}

/// Writes requests in order until the worker is gone or RiWork drops the worker.
fn write_requests(mut stdin: ChildStdin, requests: &async_channel::Receiver<String>) {
    while let Ok(line) = requests.recv_blocking() {
        if writeln!(stdin, "{line}").and_then(|_| stdin.flush()).is_err() {
            return;
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.lock().unwrap().kill();
    }
}

/// Why a local model cannot dictate yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotReady {
    /// Its files, or the runtime that runs it, are not on this Mac: Settings offers both.
    Missing,
    Failed(String),
}

/// One dictation in the worker. Dropping it cancels it there, also while its last words are
/// being decoded (a dictation abandoned at its deadline must not hold up the next); the worker
/// ignores the cancel of one it already finished.
pub struct ModelSession {
    worker: Arc<Worker>,
    id: u64,
    finished: AtomicBool,
}

static SESSIONS: AtomicU64 = AtomicU64::new(0);

/// Starts a dictation with a local model; the worker's events for it arrive on the receiver.
pub fn begin(spec: &ModelSpec) -> Result<(ModelSession, async_channel::Receiver<WorkerEvent>), NotReady> {
    let manager = manager().map_err(NotReady::Failed)?;
    let hub = manager.hub.clone().ok_or(NotReady::Missing)?;
    if !runtime_ready(&runtime_dir(&manager.speech)) || !cached(spec, &hub).complete {
        return Err(NotReady::Missing);
    }
    let worker = manager.worker().map_err(NotReady::Failed)?;
    let id = SESSIONS.fetch_add(1, Ordering::AcqRel) + 1;
    let (sender, receiver) = async_channel::unbounded();
    worker.routes.lock().unwrap().insert(id, sender);
    if !worker.send(json!({"op": "begin", "session": id, "model": spec})) {
        worker.routes.lock().unwrap().remove(&id);
        return Err(NotReady::Failed("the speech engine stopped".into()));
    }
    Ok((ModelSession { worker, id, finished: AtomicBool::new(false) }, receiver))
}

impl ModelSession {
    /// Base64 16 kHz mono float32 samples, as the capture helper writes them.
    pub fn audio(&self, data: &str) {
        if !self.finished.load(Ordering::Acquire) {
            self.worker.send(json!({"op": "audio", "session": self.id, "data": data}));
        }
    }
    /// The audio has ended: settle the last words and report them.
    pub fn finish(&self) {
        if !self.finished.swap(true, Ordering::AcqRel) {
            self.worker.send(json!({"op": "finish", "session": self.id}));
        }
    }
}

impl Drop for ModelSession {
    fn drop(&mut self) {
        self.worker.send(json!({"op": "cancel", "session": self.id}));
        self.worker.routes.lock().unwrap().remove(&self.id);
    }
}

// MARK: JavaScript

#[module]
pub struct SpeechModels;
#[module]
impl SpeechModels {
    /// The catalog with what is on this Mac: `{models: [{key, label, detail, family, bytes,
    /// cachedBytes, downloadable, status, error}], runtime: {ready, uv}, cache}`. `status` is
    /// builtin, ready, setup (on this Mac, runtime not set up), missing, unavailable (no longer
    /// downloadable), installing (runtime being set up) or downloading.
    pub async fn snapshot() -> Result<Value, String> {
        service::blocking(|| Ok(manager()?.snapshot())).await
    }
    pub fn subscribe(listener: Callback<Value>, cx: CallCx<'_>) -> Subscription {
        Subscription::start(cx, async move {
            if let Ok(subscription) = service::blocking(|| Ok(manager()?.topic.subscribe())).await {
                while let Ok(snapshot) = subscription.recv().await {
                    let data = subscription.latest(snapshot).data;
                    if !data.is_null() {
                        listener.post(data);
                    }
                    next_frame().await;
                }
            }
        })
    }
    /// Sets up the runtime and downloads the model, in the background; progress is published.
    pub async fn download(key: String) -> Result<Value, String> {
        service::blocking(move || {
            let manager = manager()?;
            manager.download(&key)?;
            Ok(manager.snapshot())
        })
        .await
    }
    pub async fn cancel(key: String) -> Result<Value, String> {
        service::blocking(move || {
            let manager = manager()?;
            manager.cancel(&key);
            Ok(manager.snapshot())
        })
        .await
    }
}

#[cfg(test)]
pub(crate) fn test_snapshot() -> Value {
    manager().unwrap().snapshot()
}

#[cfg(test)]
pub(crate) fn test_download(key: &str) -> Result<(), String> {
    manager()?.download(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(files: &[(&str, u64)]) -> ModelSpec {
        ModelSpec {
            key: "test".into(),
            label: "Test".into(),
            detail: String::new(),
            family: "parakeet".into(),
            repo: Some("org/model".into()),
            revision: Some("0123abcd".into()),
            downloadable: true,
            draft_seconds: Some(1.0),
            bytes: files.iter().map(|(_, size)| size).sum(),
            files: files.iter().map(|(name, size)| ((*name).into(), *size)).collect(),
        }
    }

    #[test]
    fn the_catalog_pins_every_model_and_knows_apple() {
        let catalog = catalog();
        assert_eq!(catalog[0].key, "apple");
        assert!(catalog[0].is_apple() && catalog[0].files.is_empty());
        for spec in catalog.iter().filter(|spec| !spec.is_apple()) {
            let revision = spec.revision.as_deref().unwrap();
            assert_eq!(revision.len(), 40, "{} is pinned to a full commit", spec.key);
            assert!(spec.repo.is_some() && !spec.files.is_empty(), "{}", spec.key);
            assert_eq!(spec.bytes, spec.files.iter().map(|(_, size)| size).sum::<u64>(), "{}", spec.key);
            assert!(spec.files.iter().all(|(name, _)| !name.ends_with(".py")), "{} downloads no code", spec.key);
            assert!(["parakeet", "whisper", "nemotron"].contains(&spec.family.as_str()));
            assert!(riwork_core::settings::dictation_model_key(spec.key.clone()).is_some());
        }
        assert_eq!(effective("parakeet-v3").key, "parakeet-v3");
        assert_eq!(effective("from-a-later-build").key, "apple");
    }

    #[test]
    fn the_cache_counts_exact_files_and_downloads_in_progress() {
        let hub = std::env::temp_dir().join(format!("riwork-speech-hub-{}", uuid::Uuid::new_v4()));
        let spec = spec(&[("config.json", 4), ("model.safetensors", 10)]);
        assert_eq!(cached(&spec, &hub), Cached { complete: false, bytes: 0 });
        let repo = repo_dir(&hub, "org/model");
        let snapshot = repo.join("snapshots/0123abcd");
        std::fs::create_dir_all(&snapshot).unwrap();
        std::fs::create_dir_all(repo.join("blobs")).unwrap();
        std::fs::write(repo.join("blobs/aaa"), b"conf").unwrap();
        std::os::unix::fs::symlink("../../blobs/aaa", snapshot.join("config.json")).unwrap();
        std::fs::write(repo.join("blobs/bbb.incomplete"), b"12345").unwrap();
        assert_eq!(cached(&spec, &hub), Cached { complete: false, bytes: 9 });
        // A file of another size (a different revision's) is not this model's.
        std::fs::write(snapshot.join("model.safetensors"), b"123").unwrap();
        assert_eq!(cached(&spec, &hub).complete, false);
        std::fs::remove_file(repo.join("blobs/bbb.incomplete")).unwrap();
        std::fs::write(snapshot.join("model.safetensors"), b"0123456789").unwrap();
        assert_eq!(cached(&spec, &hub), Cached { complete: true, bytes: 14 });
        std::fs::remove_dir_all(hub).unwrap();
    }

    #[test]
    fn worker_lines_name_their_dictation() {
        assert_eq!(
            WorkerEvent::parse(r#"{"event":"heard","session":3,"text":"hello"}"#),
            Some((3, WorkerEvent::Heard("hello".into())))
        );
        assert_eq!(
            WorkerEvent::parse(r#"{"event":"failed","session":1,"problem":"model-unavailable","detail":"x"}"#),
            Some((1, WorkerEvent::Failed { problem: "model-unavailable".into(), detail: "x".into() }))
        );
        assert_eq!(WorkerEvent::parse(r#"{"event":"loaded","session":2}"#), Some((2, WorkerEvent::Loaded)));
        assert_eq!(WorkerEvent::parse(r#"{"event":"heard","text":"no session"}"#), None);
        assert_eq!(WorkerEvent::parse("not json"), None);
    }

    #[test]
    fn the_runtime_is_keyed_by_its_lock() {
        let dir = runtime_dir(Path::new("/speech"));
        assert!(dir.starts_with("/speech"));
        assert_eq!(dir.file_name().unwrap().len(), "runtime-".len() + 12);
        assert!(LOCK.contains("name = \"mlx-audio\"") && PYPROJECT.contains("mlx-audio[stt]==0.6.3"));
    }
}
