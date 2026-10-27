//! The AppKit executable remains the CLI executable. Compare its process output
//! with the unchanged dispatcher, and make GUI initialization observably invalid.
//! Every child has a private HOME, RIWORK_HOME, and RIWORK_RUNTIME_DIR.

#![cfg(unix)]

use std::{
    fs::{self, File},
    io::{Read, Write},
    os::fd::AsRawFd,
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        // Keep the chat Unix socket below macOS's 104-byte sockaddr_un limit.
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("rwnsh-{}", &uuid::Uuid::new_v4().to_string()[..8]));
        fs::create_dir_all(root.join("home")).unwrap();
        Self { root }
    }

    fn state(&self) -> PathBuf {
        self.root.join("state")
    }

    fn command(&self, executable: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(executable);
        command
            .current_dir(&self.root)
            .env("HOME", self.root.join("home"))
            .env("RIWORK_HOME", self.state())
            .env("RIWORK_RUNTIME_DIR", self.root.join("runtime"))
            // If startup crosses into GUI initialization it must fail, even on
            // a logged-in Mac with a working NSH bundle next to the binary.
            .env("RIWORK_RESTORE_TICKET", "cli-must-not-read-this-ticket")
            .env("RIWORK_NSH_BUNDLE", self.root.join("missing.nsh"))
            .env_remove("RIWORK_CHAT_ID")
            .env_remove("RIWORK_SHELL_ID")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn desktop(&self, args: &[&str], input: &[u8]) -> Output {
        let mut command = self.command(env!("CARGO_BIN_EXE_riwork"));
        command.args(args);
        run(command, input)
    }

    fn reference(&self, args: &[&str], input: &[u8]) -> Output {
        let mut command = self.command(std::env::current_exe().unwrap());
        command
            .args(["--exact", "direct_dispatch_reference", "--nocapture"])
            .env("RIWORK_TEST_CLI_ARGS", serde_json::to_string(args).unwrap())
            .env("RIWORK_TEST_CLI_CAPTURE", &self.root);
        let mut output = run(command, input);
        // The test harness has its own preamble. Only these files contain the
        // dispatcher's byte stream, redirected before run_cli is called.
        output.stdout = fs::read(self.root.join("reference.stdout")).unwrap();
        output.stderr = fs::read(self.root.join("reference.stderr")).unwrap();
        output
    }

    fn assert_parity(&self, args: &[&str], input: &[u8]) -> Output {
        let expected = self.reference(args, input);
        let actual = self.desktop(args, input);
        assert_eq!(actual.status.code(), expected.status.code(), "{args:?}");
        assert_eq!(actual.stdout, expected.stdout, "stdout: {args:?}");
        assert_eq!(actual.stderr, expected.stderr, "stderr: {args:?}");
        self.assert_no_gui();
        actual
    }

    fn assert_no_gui(&self) {
        for path in [
            self.root.join("runtime"),
            self.root.join("home/Documents/riwork"),
            self.state().join("nsh"),
            self.state().join("run/desktop.lock"),
            self.state().join("run/desktop.sock"),
        ] {
            assert!(
                !path.exists(),
                "CLI initialized GUI state: {}",
                path.display()
            );
        }
    }

    fn add_project(&self) -> riwork_core::store::Project {
        let root = self.root.join("project");
        fs::create_dir(&root).unwrap();
        riwork_core::store::Store::open(self.state())
            .unwrap()
            .add_project(root, Some("NSH CLI fixture"))
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn read_pipe(mut pipe: impl Read + Send + 'static) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes).unwrap();
        bytes
    })
}

fn finish(mut child: ChildGuard) -> Output {
    let stdout = read_pipe(child.0.stdout.take().unwrap());
    let stderr = read_pipe(child.0.stderr.take().unwrap());
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "CLI did not exit; GUI may have started"
        );
        thread::sleep(Duration::from_millis(10));
    };
    Output {
        status,
        stdout: stdout.join().unwrap(),
        stderr: stderr.join().unwrap(),
    }
}

fn run(mut command: Command, input: &[u8]) -> Output {
    let mut child = ChildGuard(command.spawn().unwrap());
    child.0.stdin.take().unwrap().write_all(input).unwrap();
    finish(child)
}

/// A subprocess-only oracle; its normal test invocation performs no work.
#[test]
fn direct_dispatch_reference() {
    let Ok(args) = std::env::var("RIWORK_TEST_CLI_ARGS") else {
        return;
    };
    let root = PathBuf::from(std::env::var_os("RIWORK_TEST_CLI_CAPTURE").unwrap());
    let stdout = File::create(root.join("reference.stdout")).unwrap();
    let stderr = File::create(root.join("reference.stderr")).unwrap();
    std::io::stdout().flush().unwrap();
    std::io::stderr().flush().unwrap();
    // SAFETY: this is a dedicated child process. Both files stay alive through
    // dispatch; dup2 makes independent descriptors for stdout and stderr.
    unsafe {
        assert_eq!(
            libc::dup2(stdout.as_raw_fd(), libc::STDOUT_FILENO),
            libc::STDOUT_FILENO
        );
        assert_eq!(
            libc::dup2(stderr.as_raw_fd(), libc::STDERR_FILENO),
            libc::STDERR_FILENO
        );
    }
    let args: Vec<String> = serde_json::from_str(&args).unwrap();
    let status = match riwork_cli::cli::run_cli(&args) {
        Ok(true) => 0,
        Ok(false) => panic!("reference case unexpectedly requested GUI startup"),
        Err(error) => {
            if !error.is_empty() {
                eprintln!("riwork: {error}");
            }
            2
        }
    };
    std::io::stdout().flush().unwrap();
    std::io::stderr().flush().unwrap();
    std::process::exit(status);
}

#[test]
fn help_version_and_errors_keep_the_dispatchers_bytes_and_exit_status() {
    let fixture = Fixture::new();
    for args in [
        &["help"][..],
        &["--help"],
        &["-h"],
        &["version"],
        &["version", "--json"],
        &["--unknown-option"],
        &["project", "unknown"],
        &["chat", "serve", "--idle-seconds", "0"],
    ] {
        fixture.assert_parity(args, b"");
    }
}

#[test]
fn project_list_keeps_text_and_json_bytes_for_a_real_isolated_project() {
    let fixture = Fixture::new();
    let project = fixture.add_project();
    let text = fixture.assert_parity(&["project", "list"], b"");
    assert!(text.status.success());
    assert!(String::from_utf8_lossy(&text.stdout).contains(&project.id));
    let json = fixture.assert_parity(&["project", "list", "--json"], b"");
    assert!(json.status.success());
    let projects: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(projects[0]["id"], project.id);
}

#[test]
fn settings_json_keeps_the_gpui_reference_field_order() {
    // A dispatcher built in this same graph is insufficient as an oracle:
    // feature unification changes serde_json::Value order for both binaries.
    // These exact bytes were captured from the ea0f2f5 GPUI desktop executable.
    let fixture = Fixture::new();
    let output = fixture.desktop(&["settings", "list", "--json"], b"");
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert_eq!(
        output.stdout,
        include_bytes!("fixtures/settings-list-ea0f2f5.json")
    );
    fixture.assert_no_gui();
}

#[test]
fn mcp_stdio_keeps_protocol_bytes_without_gui_initialization() {
    let fixture = Fixture::new();
    let output = fixture.assert_parity(
        &["mcp"],
        b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n",
    );
    assert!(output.status.success());
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["id"], 1);
    assert_eq!(response["result"], serde_json::json!({}));
}

#[test]
fn chat_host_serves_in_the_same_executable_and_exits_headlessly() {
    let fixture = Fixture::new();
    let mut command = fixture.command(env!("CARGO_BIN_EXE_riwork"));
    command.args(["chat", "serve", "--idle-seconds", "2"]);
    let mut child = ChildGuard(command.spawn().unwrap());
    drop(child.0.stdin.take());
    let deadline = Instant::now() + Duration::from_secs(15);
    while !fixture.state().join("run/chat.sock").exists() {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "chat host exited before serving"
        );
        assert!(
            Instant::now() < deadline,
            "chat host did not create its fixture socket"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let output = fixture.assert_parity(&["chat", "list", "--json"], b"");
    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        serde_json::json!([])
    );
    let output = finish(child);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!fixture.state().join("run/chat.sock").exists());
    fixture.assert_no_gui();
}
