//! The bridge in a real terminal: a pseudo-terminal stands in for a Ghostty surface,
//! the bridge is the real binary, it starts the real client daemon, and the host is
//! the stand-in of `client_support`. What the terminal receives is checked byte for
//! byte, and so is what the terminal is left as.
mod client_support;

use client_support::{FakeHost, HostOptions, Net, REMOTE, eventually};
use riwork_remote::{client::add_host, config::Storage};
use std::{
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::process::CommandExt,
    },
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};

struct Terminal {
    master: std::fs::File,
    /// Held open so that the terminal outlives the bridge and can be inspected after.
    _slave: OwnedFd,
    output: Arc<Mutex<Vec<u8>>>,
}
fn winsize(columns: u16, rows: u16) -> libc::winsize {
    libc::winsize {
        ws_row: rows,
        ws_col: columns,
        ws_xpixel: 0,
        ws_ypixel: 0,
    }
}
impl Terminal {
    fn open(columns: u16, rows: u16) -> (Self, OwnedFd) {
        let (mut master, mut slave) = (0, 0);
        let mut size = winsize(columns, rows);
        // SAFETY: openpty fills the two descriptors; the pointers are valid for the call.
        let opened = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut size,
            )
        };
        assert_eq!(opened, 0);
        for fd in [master, slave] {
            // SAFETY: fcntl on a descriptor we just opened. Without this the bridge and
            // the client process it starts would hold the terminal open after the test.
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        }
        // SAFETY: both descriptors are new and owned by nobody else.
        let (master, slave) =
            unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        let child_end = slave.try_clone().unwrap();
        let master = std::fs::File::from(master);
        let output = Arc::new(Mutex::new(Vec::new()));
        {
            let (mut reader, output) = (master.try_clone().unwrap(), output.clone());
            std::thread::spawn(move || {
                let mut buffer = [0u8; 8192];
                while let Ok(n) = reader.read(&mut buffer) {
                    if n == 0 {
                        break;
                    }
                    output.lock().unwrap().extend_from_slice(&buffer[..n]);
                }
            });
        }
        (
            Self {
                master,
                _slave: slave,
                output,
            },
            child_end,
        )
    }
    fn seen(&self) -> Vec<u8> {
        self.output.lock().unwrap().clone()
    }
    fn type_keys(&mut self, bytes: &[u8]) {
        self.master.write_all(bytes).unwrap();
    }
    fn resize(&self, columns: u16, rows: u16) {
        let size = winsize(columns, rows);
        // SAFETY: TIOCSWINSZ reads one winsize.
        let done = unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ as _, &size) };
        assert_eq!(done, 0);
    }
    /// The settings of the terminal as the bridge left or holds them.
    fn raw(&self) -> bool {
        // SAFETY: tcgetattr fills the struct we pass.
        let mut settings: libc::termios = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::tcgetattr(self.master.as_raw_fd(), &mut settings) },
            0
        );
        settings.c_lflag & (libc::ICANON | libc::ECHO) == 0
    }
    async fn wait_for(&self, needle: &[u8], what: &str) {
        let output = self.output.clone();
        let needle = needle.to_vec();
        eventually(15, what, move || contains(&output.lock().unwrap(), &needle)).await;
    }
}
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}
fn count(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}
fn position(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).rposition(|w| w == needle)
}

/// Starts the bridge with the terminal as stdin, stdout and stderr and as its
/// controlling terminal, so that a window change reaches it as SIGWINCH.
fn spawn_bridge(home: &std::path::Path, slave: &OwnedFd, desktop: &str, shell: &str) -> Child {
    let mut command = Command::new(REMOTE);
    command
        .env("RIWORK_HOME", home)
        .env("TERM", "xterm-ghostty")
        .args(["attach", "--desktop", desktop, "--shell", shell])
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave.try_clone().unwrap()));
    // SAFETY: only setsid and ioctl, both async-signal-safe, run between fork and exec.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            libc::ioctl(0, libc::TIOCSCTTY as _, 0);
            Ok(())
        });
    }
    command.spawn().unwrap()
}
async fn exit_of(child: &mut Child, seconds: u64) -> std::process::ExitStatus {
    let end = std::time::Instant::now() + Duration::from_secs(seconds);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        assert!(std::time::Instant::now() < end, "the bridge did not exit");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Removes the host so that the client daemon the bridge started quits.
struct Cleanup(std::path::PathBuf, String);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = Command::new(REMOTE)
            .env("RIWORK_HOME", &self.0)
            .args(["hosts", "remove", &self.1])
            .output();
    }
}

struct Rig {
    // Dropped first: the client process is asked to quit while its home still exists.
    _cleanup: Cleanup,
    net: Net,
    host: FakeHost,
    home: std::path::PathBuf,
    id: String,
    shell: String,
}
async fn rig(options: HostOptions) -> Rig {
    let net = Net::new(1).await;
    let host = FakeHost::start(&net, &net.invites[0].device_id, options);
    let home = net.client_home("c");
    let client = Storage::at(home.clone()).unwrap();
    add_host(&client, &net.link(0), Some("Studio"), true)
        .await
        .unwrap();
    let id = net.desktop_id();
    Rig {
        _cleanup: Cleanup(home.clone(), id.clone()),
        net,
        host,
        home,
        id,
        shell: uuid::Uuid::new_v4().to_string(),
    }
}

#[tokio::test]
async fn the_bridge_shows_the_shell_types_for_the_person_and_follows_the_window() {
    let r = rig(HostOptions::default()).await;
    let (mut terminal, slave) = Terminal::open(100, 30);
    let mut bridge = spawn_bridge(&r.home, &slave, &r.id, &r.shell);
    terminal.wait_for(b"READY", "the first output").await;

    // It asked the host for what this terminal is.
    let open = r.host.seen(|o| o.opens[0].clone());
    assert_eq!(open["term"], "xterm-ghostty");
    assert_eq!(
        (open["columns"].as_u64(), open["rows"].as_u64()),
        (Some(100), Some(30))
    );
    assert_eq!(open["shell_id"], r.shell.as_str());
    assert!(terminal.raw(), "keys must reach the host as they are typed");

    // Keys go to the host untouched, and nothing is echoed locally: what appears is the host's echo.
    terminal.type_keys(b"ls -l\x03\r");
    terminal.wait_for(b"ls -l\x03\r", "the echo").await;
    assert_eq!(count(&terminal.seen(), b"ls -l"), 1);
    assert_eq!(r.host.seen(|o| o.typed()), b"ls -l\x03\r");

    // The window changes: the host's terminal follows.
    terminal.resize(120, 40);
    r.host
        .wait(10, "the resize", |o| o.resizes.last() == Some(&(120, 40)))
        .await;

    // The link drops: the frozen screen gets a dimmed line, keys are dropped, and when the
    // stream is back the terminal is reset before the host draws again.
    let ready_before = count(&terminal.seen(), b"READY");
    r.host.drop_connection(Duration::from_millis(1500));
    terminal.wait_for(b"is unreachable", "the notice").await;
    let seen = terminal.seen();
    let notice = position(&seen, b"is unreachable").unwrap();
    assert!(contains(&seen[..notice], b"\x1b[2m"), "dimmed");
    assert!(
        contains(&seen[..notice], b"\x1b[40;1H"),
        "on the last row of the new size"
    );
    terminal.type_keys(b"LOST");
    terminal.wait_for(b"\x1b[!p", "the reset").await;
    let output = terminal.output.clone();
    eventually(15, "the second stream", move || {
        count(&output.lock().unwrap(), b"READY") > ready_before
    })
    .await;
    let seen = terminal.seen();
    assert!(
        position(&seen, b"\x1b[!p").unwrap() < position(&seen, b"READY").unwrap(),
        "reset before new output"
    );
    assert!(contains(&seen, b"\x1b[?1049l") && contains(&seen, b"\x1b[?1000l"));
    terminal.type_keys(b"x");
    terminal.wait_for(b"x", "typing works again").await;
    let typed = r.host.seen(|o| o.typed());
    assert!(
        !contains(&typed, b"LOST"),
        "keys typed offline are dropped, never replayed"
    );
    assert!(typed.ends_with(b"x"));
    // The second stream was opened with the window as it was by then.
    assert_eq!(r.host.seen(|o| o.opens[1]["columns"].as_u64()), Some(120));

    // The shell ends: the reason is printed, the exit is clean and the terminal is as it was.
    r.host.end("exited");
    terminal
        .wait_for(b"The session ended.", "the end message")
        .await;
    let status = exit_of(&mut bridge, 10).await;
    assert!(status.success(), "{status}");
    assert!(!terminal.raw(), "the terminal is put back");
    let _ = &r.net;
}

#[tokio::test]
async fn output_arrives_whole_while_keys_and_window_changes_keep_coming() {
    let r = rig(HostOptions {
        // No echo, so that the keys typed meanwhile cannot land inside the output.
        echo: false,
        ..HostOptions::default()
    })
    .await;
    let (mut terminal, slave) = Terminal::open(100, 30);
    let mut bridge = spawn_bridge(&r.home, &slave, &r.id, &r.shell);
    terminal.wait_for(b"READY", "the first output").await;
    // Much more than a socket holds, in frames that arrive in pieces.
    let block: Vec<u8> = (0..600_000u32)
        .map(|i| b'a' + ((i * 7 + i / 251) % 26) as u8)
        .collect();
    let mut sent = b"<<BEGIN>>".to_vec();
    sent.extend_from_slice(&block);
    sent.extend_from_slice(b"<<END>>");
    r.host.output(&sent);
    // Keys and window changes at the same time: any of them must leave a frame in the
    // middle of being read untouched.
    for i in 0..300u16 {
        terminal.type_keys(b"k");
        terminal.resize(100 + i % 40, 30 + i % 7);
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    terminal.wait_for(b"<<END>>", "the end of the output").await;
    let seen = terminal.seen();
    assert!(contains(&seen, &sent), "the output was cut or reordered");
    assert!(!contains(&seen, b"payload over the limit"));
    assert!(!contains(&seen, b"Reconnecting"));
    r.host.end("exited");
    assert!(exit_of(&mut bridge, 10).await.success());
}

#[tokio::test]
async fn the_bridge_says_what_is_wrong_when_the_host_refuses_and_leaves_the_terminal_as_it_found_it()
 {
    let r = rig(HostOptions {
        refuse_open: Some(("not_found", "no such shell")),
        ..HostOptions::default()
    })
    .await;
    let (terminal, slave) = Terminal::open(80, 24);
    let mut bridge = spawn_bridge(&r.home, &slave, &r.id, &r.shell);
    terminal
        .wait_for(b"not_found: no such shell", "the reason")
        .await;
    let status = exit_of(&mut bridge, 10).await;
    assert!(status.success());
    assert!(!terminal.raw());

    // An unknown host fails before the terminal is touched.
    let (terminal, slave) = Terminal::open(80, 24);
    let mut bridge = spawn_bridge(
        &r.home,
        &slave,
        "11111111-2222-4333-8444-555555555555",
        &r.shell,
    );
    let status = exit_of(&mut bridge, 10).await;
    assert!(!status.success());
    terminal.wait_for(b"no host", "the error").await;
    assert!(!terminal.raw());
}

#[tokio::test]
async fn closing_the_terminal_ends_the_bridge_and_its_stream_on_the_host() {
    let r = rig(HostOptions::default()).await;
    let (terminal, slave) = Terminal::open(80, 24);
    let mut bridge = spawn_bridge(&r.home, &slave, &r.id, &r.shell);
    terminal.wait_for(b"READY", "the first output").await;
    let pid = bridge.id() as libc::pid_t;
    // What the surface closing does to its child.
    // SAFETY: kill sends a signal to a process of ours.
    unsafe { libc::kill(pid, libc::SIGHUP) };
    let status = exit_of(&mut bridge, 10).await;
    assert!(!status.success());
    assert!(!terminal.raw());
    r.host.wait(10, "pty.close", |o| o.closes.len() == 1).await;
}
