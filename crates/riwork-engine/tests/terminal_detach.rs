//! Opt-in proof of exact-tty detach against real clients on a private server.
//! Run only with RIWORK_HOME set to a fresh temp directory whose basename starts
//! with `riwork-terminal-detach-`; no default/live RiWork state is consulted.
#![cfg(unix)]

use riwork_engine::sessions::{AttachOptions, SessionManager};
use std::{
    ffi::CStr,
    fs::{self, File},
    io::{self, Read},
    os::{fd::FromRawFd, unix::process::CommandExt},
    path::PathBuf,
    process::{Child, Command},
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

struct PtyClient {
    child: Child,
    tty: String,
    reader: Option<thread::JoinHandle<()>>,
}

impl PtyClient {
    fn attach(mut command: Command) -> Self {
        let (mut master, mut slave) = (-1, -1);
        let mut size = libc::winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut size,
                )
            },
            0,
            "open fixture PTY: {}",
            io::Error::last_os_error()
        );
        assert_eq!(
            unsafe { libc::fcntl(master, libc::F_SETFD, libc::FD_CLOEXEC) },
            0
        );
        assert_eq!(
            unsafe { libc::fcntl(slave, libc::F_SETFD, libc::FD_CLOEXEC) },
            0
        );
        let tty = unsafe { CStr::from_ptr(libc::ttyname(slave)) }
            .to_str()
            .unwrap()
            .to_owned();
        let mut master = unsafe { File::from_raw_fd(master) };
        let slave = unsafe { File::from_raw_fd(slave) };
        command
            .stdin(slave.try_clone().unwrap())
            .stdout(slave.try_clone().unwrap())
            .stderr(slave);
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().expect("spawn private tmux attach client");
        let reader = thread::spawn(move || {
            let mut buffer = [0_u8; 16384];
            while master.read(&mut buffer).is_ok_and(|count| count != 0) {}
        });
        Self {
            child,
            tty,
            reader: Some(reader),
        }
    }

    fn wait_exit(&mut self) {
        wait_for(
            || self.child.try_wait().unwrap().is_some(),
            "detached PTY child exit",
        );
        self.reader.take().unwrap().join().unwrap();
    }
}

impl Drop for PtyClient {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        // Closing our own child also closes its PTY slave and unblocks the reader.
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

struct Fixture {
    home: PathBuf,
    tmux: PathBuf,
    socket: String,
}

struct OwnedSession(SessionManager, String);
impl Drop for OwnedSession {
    fn drop(&mut self) {
        let _ = self.0.close(&self.1);
    }
}

impl Fixture {
    fn output(&self, args: &[&str]) -> String {
        let result = Command::new(&self.tmux)
            .args(["-u", "-L", &self.socket, "-f", "/dev/null"])
            .args(args)
            .env_remove("TMUX")
            .env_remove("TMUX_TMPDIR")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "private tmux {args:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        String::from_utf8(result.stdout).unwrap()
    }

    fn clients(&self) -> String {
        self.output(&["list-clients", "-F", "#{client_tty}\t#{session_name}"])
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Only the unpredictable socket derived from this test's private home.
        let _ = Command::new(&self.tmux)
            .args(["-L", &self.socket, "kill-server"])
            .env_remove("TMUX")
            .env_remove("TMUX_TMPDIR")
            .output();
        let _ = fs::remove_dir_all(&self.home);
    }
}

fn wait_for(mut condition: impl FnMut() -> bool, label: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out: {label}");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "requires explicit fresh RIWORK_HOME named riwork-terminal-detach-* and real tmux"]
fn exact_tty_detach_preserves_other_client_and_both_sessions() {
    let root =
        PathBuf::from(std::env::var_os("RIWORK_HOME").expect("explicit fixture RIWORK_HOME"));
    assert!(
        root.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("riwork-terminal-detach-")
    );
    assert!(
        root.canonicalize()
            .unwrap()
            .starts_with(std::env::temp_dir().canonicalize().unwrap()),
        "RIWORK_HOME must be beneath the OS temporary directory"
    );
    let home = root.join(Uuid::new_v4().to_string());
    let manager = SessionManager::at(home.clone()).unwrap().without_parent();
    let project = Uuid::new_v4().to_string();
    let session = manager
        .create(
            project.clone(),
            None,
            home.clone(),
            Some("exec /bin/sleep 120".into()),
        )
        .unwrap();
    let _session_guard = OwnedSession(manager.clone(), session.id.clone());
    let argv = manager
        .attach_argv(&session.id, AttachOptions::default())
        .unwrap();
    let socket_index = argv.iter().position(|arg| arg == "-L").unwrap();
    let fixture = Fixture {
        home,
        tmux: PathBuf::from(&argv[socket_index - 1]),
        socket: argv[socket_index + 1].clone(),
    };
    let other = manager
        .create(
            project,
            None,
            fixture.home.clone(),
            Some("exec /bin/sleep 120".into()),
        )
        .unwrap();
    let identity = fixture.output(&[
        "list-panes",
        "-t",
        &session.id,
        "-F",
        "#{session_id}\t#{pane_id}\t#{pane_pid}",
    ]);
    let mut first = PtyClient::attach(
        manager
            .attach_exec_command(&session.id, AttachOptions::default())
            .unwrap(),
    );
    let mut second = PtyClient::attach(
        manager
            .attach_exec_command(&session.id, AttachOptions::default())
            .unwrap(),
    );
    wait_for(
        || fixture.clients().lines().count() == 2,
        "two real clients attach",
    );

    let wrong = manager
        .detach_terminal_client(&other.id, &first.tty)
        .unwrap_err();
    assert!(wrong.contains("another session"), "{wrong}");
    assert_eq!(fixture.clients().lines().count(), 2);
    assert!(
        manager
            .detach_terminal_client(&session.id, "../../dev/tty")
            .is_err()
    );

    manager
        .detach_terminal_client(&session.id, &first.tty)
        .unwrap();
    first.wait_exit();
    let remaining = fixture.clients();
    assert_eq!(remaining.lines().count(), 1);
    assert!(remaining.starts_with(&second.tty), "{remaining}");
    assert!(second.child.try_wait().unwrap().is_none());
    // Repeating the exact completed teardown cannot affect the surviving client.
    manager
        .detach_terminal_client(&session.id, &first.tty)
        .unwrap();
    assert_eq!(fixture.clients(), remaining);

    manager
        .detach_terminal_client(&session.id, &second.tty)
        .unwrap();
    second.wait_exit();
    assert!(fixture.clients().trim().is_empty());
    assert_eq!(
        fixture.output(&[
            "list-panes",
            "-t",
            &session.id,
            "-F",
            "#{session_id}\t#{pane_id}\t#{pane_pid}"
        ]),
        identity
    );
    assert!(manager.get(&session.id).unwrap().alive);
    assert!(manager.get(&other.id).unwrap().alive);
}
