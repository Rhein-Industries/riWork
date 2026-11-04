//! Runs an add-in's workers: the native part an add-in ships (an optimized GPU kernel, a Python
//! model), each a process of its own beside Somtus.
//!
//! A worker is declared in the add-in's `addin.json` (`workers`): `command[0]` is a file in the
//! add-in's folder, run with the rest of `command` as its arguments, in that folder. It runs
//! with the person's own rights, so only for a trusted add-in (the grant covers every file a
//! worker runs), and [`WorkerProcess::start`] refuses a program that is not an executable
//! regular file inside the folder once every link is followed: a symlink out of the folder is
//! refused.
//!
//! # Protocol
//!
//! JSON lines on the worker's stdin and stdout, UTF-8, one object per line:
//!
//! - Somtus to the worker, a request: `{"id": <u64>, "method": "<name>", "params": <any JSON>}`
//! - the worker to Somtus, its response: `{"id": <u64>, "result": <any>}`, or
//!   `{"id": <u64>, "error": {"message": "<text>"}}`
//! - the worker to Somtus, an event (no id): `{"event": "<name>", "data": <any>}`
//!
//! Several requests may be in flight at once; responses may come in any order and are matched
//! by their id. Lines that are not JSON objects are ignored. The worker exits when its stdin
//! closes. Its stderr is for diagnostics: it goes to a log file, never to Somtus's output.
//!
//! The worker runs in a process group of its own, in an environment of its own: PATH, HOME,
//! TMPDIR, USER, LANG and LC_* of Somtus's, nothing that looks like a secret (a name with
//! SECRET, TOKEN, KEY or PASSWORD, or any ANTHROPIC_, OPENAI_ or RIWORK_ variable), and
//! `RIWORK_ADDIN_DIR`, its folder. Stopping it closes its stdin, gives it 2 s, then kills its
//! group. Its stdin is a pipe only Somtus holds, so when Somtus dies, however it dies, the
//! worker reads EOF: its signal to exit.
use std::{
    collections::HashMap,
    env,
    ffi::OsString,
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use riwork_core::addins::{self, WorkerSpec};
use serde_json::{Value, json};

/// How long a stopped worker has to exit on its own before its process group is killed.
const STOP_GRACE: Duration = Duration::from_secs(2);
/// How often a waiting request looks whether the worker still runs.
const POLL: Duration = Duration::from_millis(100);
/// How long a request waits for lines still in the pipe of a worker that exited.
const EXIT_GRACE: Duration = Duration::from_millis(500);
/// An output line longer than this is dropped, not kept whole.
const LINE_LIMIT: u64 = 64 << 20;
/// A log larger than this starts over when its worker starts.
const LOG_LIMIT: u64 = 4 << 20;

type Reply = mpsc::Sender<Result<Value, String>>;
/// The requests waiting for their response, by id; `None` once the worker's output ended.
type Waiting = Mutex<Option<HashMap<u64, Reply>>>;

/// A running worker of an add-in. Requests may come from several threads at once. Dropping it
/// stops the worker.
pub struct WorkerProcess {
    id: String,
    log: PathBuf,
    pid: i32,
    /// `None` once it is reaped.
    child: Mutex<Option<Child>>,
    /// Lines for its stdin; dropping the sender closes stdin. `None` once it is stopped.
    input: Mutex<Option<mpsc::Sender<Vec<u8>>>>,
    waiting: Arc<Waiting>,
    next_id: AtomicU64,
}

impl WorkerProcess {
    /// Starts the worker `spec` of the add-in in `dir`, with `args` after its own, its stderr
    /// appended to `log`. `on_event` gets its events, on the thread that reads its output:
    /// responses wait while it runs.
    pub fn start(
        dir: &Path,
        spec: &WorkerSpec,
        args: &[String],
        log: &Path,
        on_event: impl Fn(String, Value) + Send + 'static,
    ) -> Result<Self, String> {
        Self::spawn(dir, spec, args, log, Box::new(on_event), env::vars_os())
    }

    fn spawn(
        dir: &Path,
        spec: &WorkerSpec,
        args: &[String],
        log: &Path,
        on_event: Box<dyn Fn(String, Value) + Send>,
        inherited: impl IntoIterator<Item = (OsString, OsString)>,
    ) -> Result<Self, String> {
        let (folder, program) = program(dir, spec)?;
        let stderr = open_log(log)?;
        let mut command = Command::new(&program);
        command
            .args(&spec.command[1..])
            .args(args)
            .current_dir(&folder)
            .env_clear()
            .envs(inherited.into_iter().filter(|(name, _)| kept(&name.to_string_lossy())))
            .env("RIWORK_ADDIN_DIR", &folder)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .process_group(0);
        let mut child = command.spawn().map_err(|error| format!("could not start the {} worker: {error}", spec.id))?;
        let (stdin, stdout) = (child.stdin.take(), child.stdout.take());
        let (input, lines) = mpsc::channel();
        let worker = Self {
            id: spec.id.clone(),
            log: log.to_owned(),
            pid: child.id() as i32,
            child: Mutex::new(Some(child)),
            input: Mutex::new(Some(input)),
            waiting: Arc::new(Mutex::new(Some(HashMap::new()))),
            next_id: AtomicU64::new(1),
        };
        // From here on, dropping `worker` stops the process.
        let (Some(stdin), Some(stdout)) = (stdin, stdout) else {
            return Err(format!("the {} worker has no stdin or stdout", spec.id));
        };
        let failed = |error: std::io::Error| format!("could not start the {} worker: {error}", spec.id);
        thread::Builder::new()
            .name(format!("addin-worker-{}-in", spec.id))
            .spawn(move || write_lines(stdin, lines))
            .map_err(failed)?;
        let waiting = Arc::clone(&worker.waiting);
        thread::Builder::new()
            .name(format!("addin-worker-{}-out", spec.id))
            .spawn(move || read_lines(stdout, &waiting, &*on_event))
            .map_err(failed)?;
        Ok(worker)
    }

    /// Sends `method` and waits up to `timeout` for its response: its `result`, or the message
    /// of its `error`. A worker that is gone, or exits before it answers, is an error, as is
    /// no answer in time (the worker goes on running).
    pub fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (reply, answer) = mpsc::channel();
        match lock(&self.waiting).as_mut() {
            Some(waiting) => waiting.insert(id, reply),
            None => return Err(self.gone()),
        };
        let mut line = json!({"id": id, "method": method, "params": params}).to_string().into_bytes();
        line.push(b'\n');
        if !lock(&self.input).as_ref().is_some_and(|input| input.send(line).is_ok()) {
            self.forget(id);
            return Err(self.gone());
        }
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                self.forget(id);
                return Err(format!(
                    "the {} worker did not answer {method} within {} s",
                    self.id,
                    timeout.as_secs_f64()
                ));
            }
            match answer.recv_timeout(left.min(POLL)) {
                Ok(result) => return result,
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(self.gone()),
                // Exited, while something it started may hold its output open: its last lines
                // may still be on their way.
                Err(mpsc::RecvTimeoutError::Timeout) if self.exited() => {
                    let last = answer.recv_timeout(EXIT_GRACE);
                    self.forget(id);
                    return last.unwrap_or_else(|_| Err(self.gone()));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }

    /// Whether it runs and can still answer.
    pub fn is_alive(&self) -> bool {
        lock(&self.waiting).is_some() && !self.exited()
    }

    /// Closes its stdin, gives it 2 s to exit, then kills its process group, which ends what it
    /// started too. Requests still waiting fail. Stopping again does nothing.
    pub fn stop(&self) {
        // Its stdin closes once the lines already sent are written.
        lock(&self.input).take();
        let mut child = lock(&self.child);
        if let Some(mut process) = child.take() {
            let deadline = Instant::now() + STOP_GRACE;
            while !has_exited(self.pid) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(20));
            }
            // SAFETY: this group was made for this child by process_group(0), and the child is
            // not reaped yet, so its id cannot have been reused.
            unsafe {
                libc::killpg(self.pid, libc::SIGKILL);
            }
            let _ = process.kill();
            let _ = process.wait();
        }
        drop(child);
        lock(&self.waiting).take();
    }

    fn exited(&self) -> bool {
        lock(&self.child).is_none() || has_exited(self.pid)
    }

    fn forget(&self, id: u64) {
        if let Some(waiting) = lock(&self.waiting).as_mut() {
            waiting.remove(&id);
        }
    }

    fn gone(&self) -> String {
        format!("the {} worker is not running (its log: {})", self.id, self.log.display())
    }
}

impl Drop for WorkerProcess {
    fn drop(&mut self) {
        self.stop();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether the child `pid` exited, without reaping it: `stop` kills its group by that id, which
/// must stay its own until then.
fn has_exited(pid: i32) -> bool {
    // SAFETY: waitid only writes `info`, which is zeroed and ours.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let status = unsafe {
        libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, libc::WEXITED | libc::WNOHANG | libc::WNOWAIT)
    };
    #[cfg(target_os = "macos")]
    let found = info.si_pid;
    #[cfg(not(target_os = "macos"))]
    let found = unsafe { info.si_pid() };
    // An error is a child that is reaped already (or was never ours).
    status != 0 || found != 0
}

/// The file a worker runs, every link followed: an executable regular file inside the add-in's
/// folder. Also the folder, canonical.
fn program(dir: &Path, spec: &WorkerSpec) -> Result<(PathBuf, PathBuf), String> {
    let refuse = |why: String| Err(format!("the {} worker {why}", spec.id));
    let Some(relative) = spec.command.first().filter(|program| addins::inside(program)) else {
        return refuse("must run a file of the add-in's folder (a relative path)".into());
    };
    let folder = dir.canonicalize().map_err(|error| format!("the add-in folder {}: {error}", dir.display()))?;
    let Ok(path) = folder.join(relative).canonicalize() else {
        return refuse(format!("runs {relative}, which is not in the add-in's folder"));
    };
    if !path.starts_with(&folder) {
        return refuse(format!("runs {relative}, which leads out of the add-in's folder"));
    }
    match fs::metadata(&path) {
        Ok(metadata) if !metadata.is_file() => refuse(format!("runs {relative}, which is not a file")),
        Ok(metadata) if metadata.permissions().mode() & 0o111 == 0 => {
            refuse(format!("runs {relative}, which is not executable (chmod +x it)"))
        }
        Ok(_) => Ok((folder, path)),
        Err(error) => refuse(format!("runs {relative}: {error}")),
    }
}

/// Whether a worker gets this variable of Somtus's environment: the basics a program needs, and
/// never one that looks like a secret.
fn kept(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    let basic = matches!(name, "PATH" | "HOME" | "TMPDIR" | "LANG" | "USER") || name.starts_with("LC_");
    let secret = ["SECRET", "TOKEN", "KEY", "PASSWORD"].iter().any(|word| upper.contains(word))
        || ["ANTHROPIC_", "OPENAI_", "RIWORK_"].iter().any(|prefix| upper.starts_with(prefix));
    basic && !secret
}

fn open_log(log: &Path) -> Result<fs::File, String> {
    let failed = |error: std::io::Error| format!("could not open the worker log {}: {error}", log.display());
    if let Some(parent) = log.parent() {
        fs::create_dir_all(parent).map_err(failed)?;
    }
    let mut options = OpenOptions::new();
    if fs::metadata(log).is_ok_and(|metadata| metadata.len() > LOG_LIMIT) {
        options.write(true).truncate(true);
    } else {
        options.append(true);
    }
    options.create(true).open(log).map_err(failed)
}

fn write_lines(mut stdin: ChildStdin, lines: mpsc::Receiver<Vec<u8>>) {
    for line in lines {
        if stdin.write_all(&line).is_err() {
            return;
        }
    }
    // Stopped: stdin closes as it drops, the worker's signal to exit.
}

fn read_lines(stdout: ChildStdout, waiting: &Waiting, on_event: &dyn Fn(String, Value)) {
    let mut stdout = BufReader::new(stdout);
    let mut line = Vec::new();
    loop {
        line.clear();
        match (&mut stdout).take(LINE_LIMIT).read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if line.len() as u64 == LINE_LIMIT && line.last() != Some(&b'\n') {
            match stdout.skip_until(b'\n') {
                Ok(0) | Err(_) => break,
                Ok(_) => continue,
            }
        }
        route(&line, waiting, on_event);
    }
    // Its output ended: nothing waiting gets an answer now.
    lock(waiting).take();
}

/// One line of the worker's: a response goes to the request waiting for it, an event to
/// `on_event`; anything else is ignored.
fn route(line: &[u8], waiting: &Waiting, on_event: &dyn Fn(String, Value)) {
    let Ok(Value::Object(mut message)) = serde_json::from_slice::<Value>(line) else { return };
    let Some(id) = message.get("id") else {
        if let Some(Value::String(event)) = message.remove("event") {
            on_event(event, message.remove("data").unwrap_or(Value::Null));
        }
        return;
    };
    let Some(id) = id.as_u64() else { return };
    let result = match (message.remove("error"), message.remove("result")) {
        (Some(error), _) => Err(match error.get("message").and_then(Value::as_str) {
            Some(text) => text.to_owned(),
            None => error.to_string(),
        }),
        (None, Some(result)) => Ok(result),
        (None, None) => return,
    };
    if let Some(reply) = lock(waiting).as_mut().and_then(|waiting| waiting.remove(&id)) {
        let _ = reply.send(result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A worker as the protocol describes one: each request answered on a thread of its own,
    /// so slower ones answer later.
    const FAKE: &str = r#"#!/usr/bin/python3 -I
import json, os, subprocess, sys, threading, time

lock = threading.Lock()
lingering = []

def send(message):
    with lock:
        sys.stdout.write(json.dumps(message) + "\n")
        sys.stdout.flush()

def answer(request):
    id, method, params = request["id"], request["method"], request.get("params")
    if method == "echo":
        send({"id": id, "result": params})
    elif method == "fail":
        send({"id": id, "error": {"message": "cannot " + params}})
    elif method == "noise":
        with lock:
            sys.stdout.write("not json\n[1, 2]\n\"text\"\n{\"id\": \"other\", \"result\": 1}\n")
            sys.stdout.flush()
        send({"id": id, "result": None})
    elif method == "ticks":
        for n in range(params):
            send({"event": "tick", "data": n})
        send({"id": id, "result": "ticked"})
    elif method == "sleep":
        time.sleep(params["seconds"])
        send({"id": id, "result": params["value"]})
    elif method == "env":
        send({"id": id, "result": {"env": dict(os.environ), "cwd": os.getcwd(), "argv": sys.argv[1:]}})
    elif method == "crash":
        sys.stderr.write("crashing on purpose\n")
        sys.stderr.flush()
        os._exit(3)
    elif method == "linger":
        lingering.append(subprocess.Popen(["/bin/sleep", "60"]).pid)
        send({"id": id, "result": lingering[-1]})

for line in sys.stdin:
    threading.Thread(target=answer, args=(json.loads(line),), daemon=True).start()
if lingering:
    time.sleep(60)
"#;

    struct Folder(PathBuf);

    impl Folder {
        fn new() -> Self {
            let dir = std::env::temp_dir()
                .canonicalize()
                .unwrap()
                .join(format!("riwork-addin-worker-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(dir.join("addin/bin")).unwrap();
            let folder = Self(dir);
            folder.script("addin/bin/worker", FAKE);
            folder
        }

        fn addin(&self) -> PathBuf {
            self.0.join("addin")
        }

        fn log(&self) -> PathBuf {
            self.0.join("logs/worker.log")
        }

        fn script(&self, relative: &str, text: &str) {
            let path = self.0.join(relative);
            fs::write(&path, text).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }

        fn start(&self) -> WorkerProcess {
            WorkerProcess::start(&self.addin(), &spec(&["bin/worker", "--fast"]), &["extra".into()], &self.log(), |_, _| {})
                .unwrap()
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn spec(command: &[&str]) -> WorkerSpec {
        WorkerSpec { id: "worker".into(), command: command.iter().map(|part| part.to_string()).collect(), description: String::new() }
    }

    const SECOND: Duration = Duration::from_secs(10);

    #[test]
    fn requests_get_their_results_and_errors_their_messages() {
        let folder = Folder::new();
        let worker = folder.start();
        assert!(worker.is_alive());
        assert_eq!(worker.request("echo", json!({"a": [1, "two"]}), SECOND).unwrap(), json!({"a": [1, "two"]}));
        assert_eq!(worker.request("fail", json!("scan"), SECOND).unwrap_err(), "cannot scan");
        // Lines that are not responses are passed over.
        assert_eq!(worker.request("noise", json!(null), SECOND).unwrap(), Value::Null);
        assert_eq!(worker.request("echo", json!(7), SECOND).unwrap(), json!(7));
    }

    #[test]
    fn events_reach_the_callback_in_order() {
        let folder = Folder::new();
        let (events, received) = mpsc::channel();
        let worker = WorkerProcess::start(&folder.addin(), &spec(&["bin/worker"]), &[], &folder.log(), move |name, data| {
            events.send((name, data)).unwrap();
        })
        .unwrap();
        assert_eq!(worker.request("ticks", json!(3), SECOND).unwrap(), "ticked");
        let got: Vec<(String, Value)> = received.try_iter().collect();
        assert_eq!(got, [("tick".to_owned(), json!(0)), ("tick".to_owned(), json!(1)), ("tick".to_owned(), json!(2))]);
    }

    #[test]
    fn concurrent_requests_each_get_their_own_answer() {
        let folder = Folder::new();
        let worker = folder.start();
        let started = Instant::now();
        thread::scope(|scope| {
            let worker = &worker;
            let answers: Vec<_> = (0..8u64)
                .map(|n| {
                    // The first asked answers last.
                    let seconds = (8 - n) as f64 * 0.05;
                    scope.spawn(move || worker.request("sleep", json!({"seconds": seconds, "value": n}), SECOND))
                })
                .collect();
            for (n, answer) in answers.into_iter().enumerate() {
                assert_eq!(answer.join().unwrap().unwrap(), json!(n));
            }
        });
        assert!(started.elapsed() < Duration::from_secs(2), "they ran together: {:?}", started.elapsed());
    }

    #[test]
    fn a_request_times_out_and_the_worker_goes_on() {
        let folder = Folder::new();
        let worker = folder.start();
        let started = Instant::now();
        let error = worker.request("sleep", json!({"seconds": 3, "value": 1}), Duration::from_millis(300)).unwrap_err();
        assert!(error.contains("did not answer sleep within 0.3 s"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(worker.is_alive());
        assert_eq!(worker.request("echo", json!("still here"), SECOND).unwrap(), "still here");
    }

    #[test]
    fn a_crash_fails_the_request_at_once_and_every_later_one() {
        let folder = Folder::new();
        let worker = folder.start();
        let started = Instant::now();
        let error = worker.request("crash", json!(null), Duration::from_secs(30)).unwrap_err();
        assert!(error.contains("not running") && error.contains("worker.log"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(!worker.is_alive());
        assert!(worker.request("echo", json!(1), SECOND).unwrap_err().contains("not running"));
        // Its stderr went to the log, not to our output.
        assert!(fs::read_to_string(folder.log()).unwrap().contains("crashing on purpose"));
    }

    #[test]
    fn only_executable_files_inside_the_folder_run() {
        let folder = Folder::new();
        let start = |command: &[&str]| {
            WorkerProcess::start(&folder.addin(), &spec(command), &[], &folder.log(), |_, _| {}).map(|_| ()).unwrap_err()
        };
        // A link inside the folder to a program outside it.
        folder.script("outside", FAKE);
        std::os::unix::fs::symlink(folder.0.join("outside"), folder.addin().join("bin/link")).unwrap();
        assert!(start(&["bin/link"]).contains("leads out of the add-in's folder"));
        std::os::unix::fs::symlink(folder.0.join("outside"), folder.addin().join("escape")).unwrap();
        assert!(start(&["escape"]).contains("leads out"));
        assert!(start(&["../outside"]).contains("relative path"));
        assert!(start(&[folder.0.join("outside").to_str().unwrap()]).contains("relative path"));
        assert!(start(&[]).contains("relative path"));
        assert!(start(&["bin/missing"]).contains("not in the add-in's folder"));
        assert!(start(&["bin"]).contains("not a file"));
        fs::write(folder.addin().join("bin/plain"), FAKE).unwrap();
        assert!(start(&["bin/plain"]).contains("not executable"));
        // A link that stays inside runs.
        std::os::unix::fs::symlink("worker", folder.addin().join("bin/alias")).unwrap();
        let worker = WorkerProcess::start(&folder.addin(), &spec(&["bin/alias"]), &[], &folder.log(), |_, _| {}).unwrap();
        assert_eq!(worker.request("echo", json!(1), SECOND).unwrap(), 1);
    }

    #[test]
    fn the_environment_keeps_the_basics_and_no_secret() {
        let folder = Folder::new();
        let inherited = [
            ("PATH", "/usr/bin:/bin"),
            ("HOME", "/Users/someone"),
            ("USER", "someone"),
            ("LANG", "en_US.UTF-8"),
            ("LC_ALL", "en_US.UTF-8"),
            ("TMPDIR", "/tmp/x"),
            ("ANTHROPIC_API_KEY", "sk-1"),
            ("OPENAI_BASE_URL", "https://x"),
            ("GITHUB_TOKEN", "ghp"),
            ("AWS_SECRET_ACCESS_KEY", "s"),
            ("DB_PASSWORD", "p"),
            ("RIWORK_HOME", "/riwork"),
            ("RIWORK_CHAT_ID", "c"),
            ("LC_API_KEY", "k"),
            ("EDITOR", "vim"),
        ]
        .map(|(name, value)| (OsString::from(name), OsString::from(value)));
        let worker = WorkerProcess::spawn(&folder.addin(), &spec(&["bin/worker", "--fast"]), &["extra".into()], &folder.log(), Box::new(|_, _| {}), inherited)
            .unwrap();
        let seen = worker.request("env", json!(null), SECOND).unwrap();
        let env = seen["env"].as_object().unwrap();
        for (name, value) in [("PATH", "/usr/bin:/bin"), ("HOME", "/Users/someone"), ("USER", "someone"), ("LANG", "en_US.UTF-8"), ("LC_ALL", "en_US.UTF-8"), ("TMPDIR", "/tmp/x")] {
            assert_eq!(env.get(name).and_then(Value::as_str), Some(value), "{name}");
        }
        for name in ["ANTHROPIC_API_KEY", "OPENAI_BASE_URL", "GITHUB_TOKEN", "AWS_SECRET_ACCESS_KEY", "DB_PASSWORD", "RIWORK_HOME", "RIWORK_CHAT_ID", "LC_API_KEY", "EDITOR"] {
            assert!(!env.contains_key(name), "{name}");
        }
        assert_eq!(env["RIWORK_ADDIN_DIR"], folder.addin().to_str().unwrap());
        assert_eq!(seen["cwd"], folder.addin().to_str().unwrap());
        assert_eq!(seen["argv"], json!(["--fast", "extra"]));
    }

    #[test]
    fn stop_closes_stdin_then_kills_what_lingers() {
        let folder = Folder::new();
        // It exits when its stdin closes.
        let worker = folder.start();
        let started = Instant::now();
        worker.stop();
        assert!(started.elapsed() < Duration::from_secs(1), "{:?}", started.elapsed());
        assert!(!worker.is_alive());
        assert!(worker.request("echo", json!(1), SECOND).unwrap_err().contains("not running"));
        worker.stop();
        // One that stays on after EOF, with a process of its own: both end.
        let worker = folder.start();
        let sleeper = worker.request("linger", json!(null), SECOND).unwrap().as_i64().unwrap() as i32;
        let started = Instant::now();
        drop(worker);
        let elapsed = started.elapsed();
        assert!(elapsed >= STOP_GRACE && elapsed < STOP_GRACE + Duration::from_secs(1), "{elapsed:?}");
        // The orphaned sleeper is killed with its group, then reaped by launchd.
        let gone = (0..100).any(|_| {
            thread::sleep(Duration::from_millis(20));
            // SAFETY: signal 0 only asks whether the process exists.
            unsafe { libc::kill(sleeper, 0) != 0 }
        });
        assert!(gone, "the worker's own process outlived it");
    }
}
