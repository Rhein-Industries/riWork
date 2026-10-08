//! `riwork shell attach ID --exec [--ignore-size] [--read-only]` as the remote
//! connector's `pty.open` runs it (`remote/src/pty.rs`): the process becomes a
//! tmux client of the shell, on the terminal it was started on, announcing a
//! terminal type tmux can use. Each child runs in a throwaway RIWORK_HOME with
//! its own tmux server; nothing touches a real one.
//!
//! A `tmux` placed first on PATH (the CLI looks there first) either records what
//! the CLI executed it with and exits, or hands over to the real tmux.

use serde_json::Value;
use std::{
    fs,
    io::Read,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{fs::PermissionsExt, process::CommandExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

/// The real tmux, found where the CLI would look.
fn real_tmux() -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .chain(
            [
                "/opt/homebrew/bin",
                "/usr/local/bin",
                "/opt/local/bin",
                "/usr/bin",
            ]
            .map(PathBuf::from),
        )
        .map(|dir| dir.join("tmux"))
        .find(|path| path.is_file())
}

struct Home {
    path: PathBuf,
    tmux: PathBuf,
}

impl Home {
    fn new(tmux: &Path) -> Self {
        // macOS temp dirs are symlinks; the CLI keys its tmux socket by the canonical path.
        let path = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("riwork-attach-exec-{}", Uuid::new_v4()));
        fs::create_dir_all(path.join("bin")).unwrap();
        let wrapper = path.join("bin/tmux");
        fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\n\
                 case \" $* \" in\n\
                 *\" attach-session \"*)\n\
                   if [ -n \"$RECORD\" ]; then\n\
                     {{ echo \"pid:$$\"; echo \"TERM=$TERM\"; echo \"TERMINFO=${{TERMINFO-unset}}\";\n\
                        echo \"TMUX=${{TMUX-unset}}\"; echo \"TMUX_TMPDIR=${{TMUX_TMPDIR-unset}}\";\n\
                        for a in \"$@\"; do echo \"arg:$a\"; done; }} > \"$RECORD\"\n\
                     exit 0\n\
                   fi;;\n\
                 esac\n\
                 exec '{}' \"$@\"\n",
                tmux.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            path,
            tmux: tmux.to_path_buf(),
        }
    }

    fn path_env(&self) -> std::ffi::OsString {
        let mut dirs = vec![self.path.join("bin")];
        dirs.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        std::env::join_paths(dirs).unwrap()
    }

    fn command_for(&self, binary: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(binary);
        command
            .args(args)
            .env("RIWORK_HOME", &self.path)
            .env("RIWORK_RUNTIME_DIR", self.path.join("runtime"))
            .env("PATH", self.path_env())
            .env_remove("TERM")
            .env_remove("TERMINFO")
            .env_remove("RECORD")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn command(&self, args: &[&str]) -> Command {
        self.command_for(Path::new(env!("CARGO_BIN_EXE_riwork")), args)
    }

    fn run(&self, args: &[&str]) -> Output {
        finish(self.command(args).spawn().unwrap(), args)
    }

    fn ok(&self, args: &[&str]) -> Value {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    /// A shell running `command` in a new project, its id.
    fn shell(&self, command: &str) -> String {
        let root = self.path.join("app");
        if !root.exists() {
            fs::create_dir_all(&root).unwrap();
            git(&root, &["init", "--initial-branch=main", "--template="]);
            git(&root, &["commit", "--allow-empty", "-m", "fixture"]);
            self.ok(&["project", "add", root.to_str().unwrap(), "--json"]);
        }
        let projects = self.ok(&["project", "list", "--json"]);
        let project = projects[0]["id"].as_str().unwrap().to_owned();
        let shell = self.ok(&[
            "shell",
            "create",
            "--project",
            &project,
            "--command",
            command,
            "--json",
        ]);
        shell["id"].as_str().unwrap().to_owned()
    }

    /// Runs the attach with `RECORD` set (the tmux stand-in records and exits) and
    /// returns what it was run with: one `name=value` or `arg:value` per line.
    fn attach_recorded(&self, binary: &Path, args: &[&str], env: &[(&str, &str)]) -> Vec<String> {
        let record = self.path.join(format!("record-{}", Uuid::new_v4()));
        let mut command = self.command_for(binary, args);
        command.env("RECORD", &record).envs(env.iter().copied());
        let child = command.spawn().unwrap();
        let started = child.id();
        let output = finish(child, args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let lines: Vec<String> = fs::read_to_string(&record)
            .unwrap_or_else(|_| panic!("tmux was never run: {args:?}"))
            .lines()
            .map(str::to_owned)
            .collect();
        // Exec, not spawn: tmux is the very process that was started.
        assert_eq!(
            lines.first().map(String::as_str),
            Some(format!("pid:{started}").as_str()),
            "{lines:?}"
        );
        lines
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        // Close fixture shells, then stop the tmux server private to this home.
        if let Ok(registry) = fs::read(self.path.join("sessions.json")) {
            let registry: Value = serde_json::from_slice(&registry).unwrap_or(Value::Null);
            for session in registry["sessions"].as_array().into_iter().flatten() {
                if let Some(id) = session["id"].as_str() {
                    let _ = self.command(&["shell", "close", id]).output();
                }
            }
        }
        let _ = Command::new(&self.tmux)
            .args(["-L", &self.socket(), "kill-server"])
            .output();
        let _ = fs::remove_dir_all(&self.path);
    }
}

impl Home {
    /// The private tmux server's socket name, as the CLI derives it (stable hash of the home).
    fn socket(&self) -> String {
        let mut hash = 0xcbf29ce484222325u64;
        for byte in self.path.as_os_str().to_string_lossy().as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        format!("riwork-{hash:016x}")
    }
}

fn finish(mut child: Child, what: &[&str]) -> Output {
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("riwork {what:?} did not exit; it may have started the GUI");
        }
        thread::sleep(Duration::from_millis(20));
    };
    let mut output = Output {
        status,
        stdout: Vec::new(),
        stderr: Vec::new(),
    };
    if let Some(mut stdout) = child.stdout.take() {
        stdout.read_to_end(&mut output.stdout).unwrap();
    }
    if let Some(mut stderr) = child.stderr.take() {
        stderr.read_to_end(&mut output.stderr).unwrap();
    }
    output
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "user.name=RiWork Tests"])
        .args(["-c", "user.email=riwork-tests@example.invalid"])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn value<'a>(record: &'a [String], name: &str) -> Option<&'a str> {
    record
        .iter()
        .find_map(|line| line.strip_prefix(&format!("{name}=")))
}
fn args(record: &[String]) -> Vec<&str> {
    record
        .iter()
        .filter_map(|line| line.strip_prefix("arg:"))
        .collect()
}

/// A terminfo directory in macOS's layout holding an `xterm-ghostty` entry.
fn terminfo_in(dir: &Path) -> PathBuf {
    let terminfo = dir.join("Resources/terminfo");
    fs::create_dir_all(terminfo.join("78")).unwrap();
    fs::write(terminfo.join("78/xterm-ghostty"), b"compiled entry").unwrap();
    terminfo
}

#[test]
fn capabilities_say_the_cli_can_attach_in_place() {
    let sandbox = std::env::temp_dir().join(format!("riwork-attach-caps-{}", Uuid::new_v4()));
    fs::create_dir_all(&sandbox).unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_riwork"))
            .args(args)
            .env("RIWORK_HOME", &sandbox)
            .output()
            .unwrap()
    };
    let json = run(&["capabilities", "--json"]);
    assert!(json.status.success());
    let json: Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(json["shell_attach_exec"], true);
    // Earlier answers keep theirs.
    assert_eq!(json["v"], 1);
    assert_eq!(json["verifies_shell"], true);
    assert_eq!(json["project_create_exclusive"], true);
    assert_eq!(json["shell_paste"], true);
    assert_eq!(json["shell_create_as_settings"], true);
    let text = String::from_utf8_lossy(&run(&["capabilities"]).stdout).into_owned();
    assert!(text.contains("shell_attach_exec yes"), "{text}");
    fs::remove_dir_all(sandbox).unwrap();
}

#[test]
#[ignore = "slow: real tmux server"]
fn exec_becomes_the_tmux_client_the_printed_command_would_start_with_only_the_flags_asked_for() {
    let Some(tmux) = real_tmux() else {
        eprintln!("tmux is not installed; skipping");
        return;
    };
    let home = Home::new(&tmux);
    let shell = home.shell("exec cat");
    let riwork = Path::new(env!("CARGO_BIN_EXE_riwork"));

    // The printed command, which Ghostty runs, names the same client.
    let printed = home.ok(&["shell", "attach", &shell, "--json"]);
    let printed = printed["command"].as_str().unwrap().to_owned();
    assert!(
        printed.starts_with("/usr/bin/env -u TMUX -u TMUX_TMPDIR ")
            && printed.ends_with(&format!(" attach-session -t {shell}")),
        "{printed}"
    );

    // No flags: nothing but `attach-session -t ID`, from a clean tmux environment
    // (the caller's TMUX and TMUX_TMPDIR do not follow into the client).
    let record = home.attach_recorded(
        riwork,
        &["shell", "attach", &shell, "--exec"],
        &[
            ("TERM", "xterm-256color"),
            ("TMUX", "/tmp/x,1,0"),
            ("TMUX_TMPDIR", "/tmp/x"),
        ],
    );
    let tmux_args = args(&record);
    assert_eq!(
        tmux_args,
        ["-L", &home.socket(), "attach-session", "-t", &shell],
        "{record:?}"
    );
    assert_eq!(value(&record, "TMUX"), Some("unset"));
    assert_eq!(value(&record, "TMUX_TMPDIR"), Some("unset"));
    assert_eq!(value(&record, "TERM"), Some("xterm-256color"));

    // --ignore-size and --read-only become tmux's own flags, before `-t`.
    let record = home.attach_recorded(
        riwork,
        &[
            "shell",
            "attach",
            &shell,
            "--exec",
            "--ignore-size",
            "--read-only",
        ],
        &[("TERM", "xterm-256color")],
    );
    assert_eq!(
        args(&record),
        [
            "-L",
            &home.socket(),
            "attach-session",
            "-r",
            "-f",
            "ignore-size",
            "-t",
            &shell
        ]
    );
    let record = home.attach_recorded(
        riwork,
        &["shell", "attach", "--exec", &shell, "--ignore-size"],
        &[("TERM", "xterm-256color")],
    );
    assert_eq!(
        args(&record),
        [
            "-L",
            &home.socket(),
            "attach-session",
            "-f",
            "ignore-size",
            "-t",
            &shell
        ]
    );
}

#[test]
#[ignore = "slow: real tmux server"]
fn the_terminal_it_announces_is_ghostty_where_its_terminfo_exists_and_a_plain_xterm_where_not() {
    let Some(tmux) = real_tmux() else {
        eprintln!("tmux is not installed; skipping");
        return;
    };
    let home = Home::new(&tmux);
    let shell = home.shell("exec cat");
    let riwork = Path::new(env!("CARGO_BIN_EXE_riwork"));
    let attach = ["shell", "attach", shell.as_str(), "--exec"];

    // The executable as the build puts it (target/debug/riwork) may have a packaged app
    // beside it; one in a folder of its own has no bundle anywhere near.
    fs::create_dir_all(home.path.join("plain")).unwrap();
    let plain = home.path.join("plain/riwork");
    let have_plain = fs::hard_link(riwork, &plain).is_ok();
    if !have_plain {
        eprintln!("cannot link the test binary into a folder here; skipping the no-bundle part");
    }
    if have_plain {
        // No terminfo for it anywhere: a plain xterm.
        let record = home.attach_recorded(&plain, &attach, &[("TERM", "xterm-ghostty")]);
        assert_eq!(value(&record, "TERM"), Some("xterm-256color"), "{record:?}");
        assert_eq!(value(&record, "TERMINFO"), Some("unset"));
    }

    if have_plain {
        // Started by a Ghostty that says where its terminfo is.
        let theirs = terminfo_in(&home.path.join("ghostty"));
        let record = home.attach_recorded(
            &plain,
            &attach,
            &[
                ("TERM", "xterm-ghostty"),
                ("TERMINFO", theirs.to_str().unwrap()),
            ],
        );
        assert_eq!(value(&record, "TERM"), Some("xterm-ghostty"), "{record:?}");
        assert_eq!(value(&record, "TERMINFO"), theirs.to_str());
    }

    // The executable sits in an app bundle that ships it: that one is used, and
    // asking for it finds it with no help from the environment.
    let bundle = home.path.join("RiWork.app/Contents");
    fs::create_dir_all(bundle.join("MacOS")).unwrap();
    let bundled = terminfo_in(&bundle);
    let inside = bundle.join("MacOS/riwork");
    if fs::hard_link(riwork, &inside).is_err() {
        eprintln!("cannot link the test binary into a bundle here; skipping that part");
    } else {
        let record = home.attach_recorded(&inside, &attach, &[("TERM", "xterm-ghostty")]);
        assert_eq!(value(&record, "TERM"), Some("xterm-ghostty"), "{record:?}");
        assert_eq!(
            value(&record, "TERMINFO").map(Path::new),
            Some(bundled.canonicalize().unwrap().as_path()),
            "{record:?}"
        );
        // Its terminfo wins over the one the caller names.
        let theirs = terminfo_in(&home.path.join("ghostty"));
        let record = home.attach_recorded(
            &inside,
            &attach,
            &[
                ("TERM", "xterm-ghostty"),
                ("TERMINFO", theirs.to_str().unwrap()),
            ],
        );
        assert_eq!(
            value(&record, "TERMINFO").map(Path::new),
            Some(bundled.canonicalize().unwrap().as_path())
        );
    }

    // A terminal that is not Ghostty's stays as the caller has it; none at all is a plain xterm.
    let record = home.attach_recorded(riwork, &attach, &[("TERM", "screen-256color")]);
    assert_eq!(value(&record, "TERM"), Some("screen-256color"));
    let record = home.attach_recorded(riwork, &attach, &[]);
    assert_eq!(value(&record, "TERM"), Some("xterm-256color"));
    let record = home.attach_recorded(riwork, &attach, &[("TERM", "dumb")]);
    assert_eq!(value(&record, "TERM"), Some("xterm-256color"));
}

#[test]
#[ignore = "slow: real tmux server"]
fn a_shell_that_is_not_live_is_refused_before_anything_is_executed_and_so_is_misuse() {
    let Some(tmux) = real_tmux() else {
        eprintln!("tmux is not installed; skipping");
        return;
    };
    let home = Home::new(&tmux);
    let record = home.path.join("never");
    let refuse = |args: &[&str]| {
        let mut command = home.command(args);
        command.env("RECORD", &record).env("TERM", "xterm-256color");
        let output = finish(command.spawn().unwrap(), args);
        assert!(!output.status.success(), "{args:?} succeeded");
        assert!(output.stdout.is_empty(), "{args:?}");
        String::from_utf8_lossy(&output.stderr).into_owned()
    };

    let stranger = Uuid::new_v4().to_string();
    // The words the remote connector maps to `not_found` (`Checked::explain`).
    let said = refuse(&["shell", "attach", &stranger, "--exec"]);
    assert_eq!(said.trim(), format!("riwork: unknown shell {stranger}"));

    // A shell whose process ended is registered but not live.
    let dying = home.shell("sleep 1");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let listed = home.ok(&["shell", "list", "--all", "--json"]);
        let alive = listed
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == dying.as_str() && s["alive"] == true);
        if !alive {
            break;
        }
        assert!(Instant::now() < deadline, "the shell never ended");
        thread::sleep(Duration::from_millis(100));
    }
    let said = refuse(&["shell", "attach", &dying, "--exec"]);
    assert_eq!(said.trim(), format!("riwork: shell {dying} has exited"));

    let live = home.shell("exec cat");
    for (args, expected) in [
        (
            vec!["shell", "attach", live.as_str(), "--ignore-size"],
            "--ignore-size and --read-only only apply to --exec",
        ),
        (
            vec!["shell", "attach", live.as_str(), "--read-only"],
            "--ignore-size and --read-only only apply to --exec",
        ),
        (
            vec!["shell", "attach", live.as_str(), "--exec", "--json"],
            "shell attach --exec cannot print JSON",
        ),
        (
            vec!["shell", "attach", "--exec"],
            "Usage: riwork shell attach ID --exec [--ignore-size] [--read-only]",
        ),
        (
            vec!["shell", "attach", live.as_str(), "extra", "--exec"],
            "Usage: riwork shell attach ID --exec",
        ),
    ] {
        let said = refuse(&args);
        assert!(said.contains(expected), "{args:?}: {said}");
    }
    // Without --exec the command is as it was.
    let said = refuse(&["shell", "attach"]);
    assert!(said.contains("Usage: riwork shell attach ID"), "{said}");
    assert!(!said.contains("--exec"), "{said}");
    assert!(!record.exists(), "tmux must not have been run to attach");
}

/// A pseudo-terminal pair of 100 by 30 cells.
fn pty_pair() -> (OwnedFd, OwnedFd) {
    let (mut master, mut slave) = (-1, -1);
    let size = libc::winsize {
        ws_row: 30,
        ws_col: 100,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: openpty fills two descriptors.
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::from_ref(&size).cast_mut(),
        )
    };
    assert_eq!(rc, 0, "openpty failed");
    // SAFETY: both are open and ours.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    // The client must not inherit the master, or closing it would hang nothing up.
    for fd in [&master, &slave] {
        // SAFETY: plain flag change on an open descriptor.
        assert_eq!(
            unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) },
            0
        );
    }
    (master, slave)
}

/// Everything readable on `fd` within `wait`.
fn drain(fd: &OwnedFd, wait: Duration) -> Vec<u8> {
    let mut out = Vec::new();
    let end = Instant::now() + wait;
    while Instant::now() < end {
        let mut poll = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        if unsafe { libc::poll(&mut poll, 1, 50) } <= 0 {
            continue;
        }
        let mut buffer = [0u8; 8192];
        // SAFETY: the buffer is valid for its length.
        let count = unsafe { libc::read(fd.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) };
        if count <= 0 {
            break;
        }
        out.extend_from_slice(&buffer[..count as usize]);
    }
    out
}

#[test]
#[ignore = "slow: real tmux server"]
fn on_a_terminal_it_draws_the_shell_and_hanging_up_leaves_the_shell_running() {
    let Some(tmux) = real_tmux() else {
        eprintln!("tmux is not installed; skipping");
        return;
    };
    let home = Home::new(&tmux);
    let marker = format!("ATTACHED-{}", Uuid::new_v4().simple());
    let shell = home.shell(&format!("echo {marker}; exec cat"));

    let (master, slave) = pty_pair();
    let mut command = home.command(&["shell", "attach", &shell, "--exec"]);
    command
        .env("TERM", "xterm-256color")
        .stdin(slave.try_clone().unwrap())
        .stdout(slave.try_clone().unwrap())
        .stderr(slave.try_clone().unwrap());
    // SAFETY: only async-signal-safe calls between fork and exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut client = command.spawn().unwrap();
    drop(command);
    drop(slave);

    // What the client draws is the shell's screen: tmux's own redraw of it.
    let mut drawn = Vec::new();
    let end = Instant::now() + Duration::from_secs(20);
    while !String::from_utf8_lossy(&drawn).contains(&marker) {
        assert!(
            Instant::now() < end,
            "the shell never appeared on the terminal: {:?}",
            String::from_utf8_lossy(&drawn)
        );
        drawn.extend(drain(&master, Duration::from_millis(200)));
        if let Some(status) = client.try_wait().unwrap() {
            panic!(
                "the client ended ({status}) before drawing: {:?}",
                String::from_utf8_lossy(&drawn)
            );
        }
    }

    // It is a tmux client in place of the CLI, not a child of it.
    let name = Command::new("ps")
        .args(["-o", "comm=", "-p", &client.id().to_string()])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&name.stdout).contains("tmux"),
        "{:?}",
        String::from_utf8_lossy(&name.stdout)
    );

    // Hanging up (the master closing, as when a remote desktop goes away) ends the
    // client and nothing else: the shell and what it printed are still there.
    drop(master);
    let hung_up = Instant::now() + Duration::from_secs(10);
    while client.try_wait().unwrap().is_none() {
        assert!(Instant::now() < hung_up, "the client outlived its terminal");
        thread::sleep(Duration::from_millis(50));
    }
    let listed = home.ok(&["shell", "list", "--all", "--json"]);
    let entry = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == shell.as_str())
        .unwrap();
    assert_eq!(entry["alive"], true);
    let output = home.ok(&["shell", "output", &shell, "--json"]);
    assert!(output["output"].as_str().unwrap().contains(&marker));
}
