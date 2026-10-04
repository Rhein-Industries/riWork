//! `riwork chat ensure|serve|list|stop` with the real binary on a throwaway
//! RIWORK_HOME: the host starts detached and once, answers on a private socket,
//! stops on SIGTERM or when idle, and the next `ensure` starts it again. No
//! provider is ever started: that needs a driver, which the in-process tests
//! fake.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

/// A short path: a Unix socket's holds 103 bytes.
struct Home(PathBuf);

impl Home {
    fn new() -> Self {
        let path = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("rwc-{}", &Uuid::new_v4().to_string()[..6]));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn socket(&self) -> PathBuf {
        self.0.join("run/chat.sock")
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_riwork"));
        command
            .args(args)
            .env("RIWORK_HOME", &self.0)
            .env("RIWORK_RUNTIME_DIR", self.0.join("runtime"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        let child = self.command(args).spawn().unwrap();
        let (done, output) = std::sync::mpsc::channel();
        thread::spawn(move || done.send(child.wait_with_output().unwrap()));
        output
            .recv_timeout(Duration::from_secs(60))
            .unwrap_or_else(|_| panic!("riwork {args:?} did not exit"))
    }

    /// The pid of the running host, from its lock file.
    fn host_pid(&self) -> i32 {
        fs::read_to_string(self.0.join("run/chat.lock"))
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        if let Some(pid) = fs::read_to_string(self.0.join("run/chat.lock"))
            .ok()
            .and_then(|text| text.trim().parse::<i32>().ok())
        {
            terminate(pid);
        }
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// SIGTERM, and a wait until the process is gone.
fn terminate(pid: i32) {
    if !alive(pid) {
        return;
    }
    // SAFETY: an ordinary signal to a process of ours.
    unsafe { libc::kill(pid, libc::SIGTERM) };
    let end = Instant::now() + Duration::from_secs(20);
    while alive(pid) {
        assert!(Instant::now() < end, "process {pid} ignored SIGTERM");
        thread::sleep(Duration::from_millis(20));
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn mode(path: &std::path::Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn ensure_starts_one_detached_host_and_every_later_ensure_finds_it() {
    let home = Home::new();
    let first = home.run(&["chat", "ensure"]);
    assert!(first.status.success(), "{}", stderr(&first));
    assert_eq!(stdout(&first).trim(), home.socket().to_string_lossy());
    let pid = home.host_pid();
    assert!(alive(pid));

    // Private socket in a private directory; the host is a session of its own,
    // so the terminal or app that started it can go away.
    assert_eq!(mode(&home.0.join("run")), 0o700);
    assert_eq!(mode(&home.socket()), 0o600);
    // SAFETY: getsid only reads process tables.
    let session = unsafe { libc::getsid(pid) };
    assert_eq!(session, pid, "the host leads its own session");

    // Idempotent, also when several callers ask at once.
    let callers: Vec<_> = (0..4)
        .map(|_| {
            let child = home.command(&["chat", "ensure", "--json"]).spawn().unwrap();
            thread::spawn(move || child.wait_with_output().unwrap())
        })
        .collect();
    for caller in callers {
        let output = caller.join().unwrap();
        assert!(output.status.success(), "{}", stderr(&output));
        assert!(stdout(&output).contains("chat.sock"), "{}", stdout(&output));
    }
    assert_eq!(home.host_pid(), pid, "no second host was started");

    // The host answers the other commands.
    assert_eq!(stdout(&home.run(&["chat", "list"])), "");
    assert_eq!(stdout(&home.run(&["chat", "list", "--json"])).trim(), "[]");
    // A host asked to serve where one already runs says so and leaves.
    let again = home.run(&["chat", "serve"]);
    assert!(again.status.success());
    assert!(
        stderr(&again).contains("already runs"),
        "{}",
        stderr(&again)
    );
    assert_eq!(home.host_pid(), pid);
    let log = fs::read_to_string(home.0.join("run/chat.log")).unwrap();
    assert!(log.contains("serving"), "{log}");

    // SIGTERM ends it and takes the socket along; stop finds nothing to stop.
    terminate(pid);
    assert!(!home.socket().exists());
    let stop = home.run(&["chat", "stop", "abcdefgh"]);
    assert!(!stop.status.success());
    assert!(
        stderr(&stop).contains("No chat host is running"),
        "{}",
        stderr(&stop)
    );

    // The next ensure starts a new one.
    let restarted = home.run(&["chat", "ensure"]);
    assert!(restarted.status.success(), "{}", stderr(&restarted));
    assert_ne!(home.host_pid(), pid);
    assert!(home.socket().exists());
}

#[test]
fn a_host_in_the_foreground_exits_by_itself_when_idle() {
    let home = Home::new();
    let mut host = home
        .command(&["chat", "serve", "--idle-seconds", "1"])
        .spawn()
        .unwrap();
    let end = Instant::now() + Duration::from_secs(30);
    while !home.socket().exists() {
        assert!(Instant::now() < end, "the host did not start");
        thread::sleep(Duration::from_millis(20));
    }
    let list = home.run(&["chat", "list", "--json"]);
    assert_eq!(stdout(&list).trim(), "[]", "{}", stderr(&list));
    let status = loop {
        if let Some(status) = host.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < end, "the idle host did not exit");
        thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success());
    assert!(!home.socket().exists());
}

#[test]
fn chat_commands_refuse_what_they_cannot_do_without_starting_a_host() {
    let home = Home::new();
    let new = home.run(&["chat", "new", "--provider", "codex"]);
    assert_eq!(new.status.code(), Some(2));
    assert!(
        stderr(&new).contains("No active project"),
        "{}",
        stderr(&new)
    );
    let unknown = home.run(&["chat", "frobnicate"]);
    assert_eq!(unknown.status.code(), Some(2));
    assert!(
        stderr(&unknown).contains("Usage: riwork chat"),
        "{}",
        stderr(&unknown)
    );
    let serve = home.run(&["chat", "serve", "--idle-seconds", "0"]);
    assert_eq!(serve.status.code(), Some(2));
    assert!(!home.socket().exists(), "none of them started a host");
}
