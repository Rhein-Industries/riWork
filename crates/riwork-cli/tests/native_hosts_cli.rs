//! Real headless CLI contract. No desktop, provider, or tmux server is started;
//! every child uses a private home and a fixture tmux that only accepts reads.+#![cfg(unix)]

use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

struct Home(PathBuf);

impl Home {
    fn new() -> Self {
        let path = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("somtus-headless-hosts-{}", Uuid::new_v4()));
        fs::create_dir_all(path.join("home")).unwrap();
        fs::create_dir_all(path.join("bin")).unwrap();
        let tmux = path.join("bin/tmux");
        fs::write(
            &tmux,
            "#!/bin/sh\ncase \" $* \" in\n*' list-panes '*) printf '%s\\n' 'no server running' >&2; exit 1;;\nesac\nprintf '%s\\n' \"$*\" >> \"$HOME/forbidden-tmux-command\"\nexit 73\n",
        )
        .unwrap();
        fs::set_permissions(&tmux, fs::Permissions::from_mode(0o700)).unwrap();
        // Run the actual built CLI in a directory with no adjacent app bundle.
        // A hard link avoids copying a large debug binary for every fixture.
        let binary = path.join("bin/somtus-cli");
        fs::hard_link(env!("CARGO_BIN_EXE_somtus-cli"), &binary)
            .or_else(|_| fs::copy(env!("CARGO_BIN_EXE_somtus-cli"), &binary).map(|_| ()))
            .unwrap();
        Self(path)
    }

    fn run(&self, args: &[&str], input: &[u8]) -> Output {
        let mut child = Command::new(self.0.join("bin/somtus-cli"))
            .args(args)
            .current_dir(&self.0)
            .env("HOME", self.0.join("home"))
            .env("PATH", self.0.join("bin"))
            .env("RIWORK_HOME", self.0.join("state"))
            .env("RIWORK_RUNTIME_DIR", self.0.join("runtime"))
            .env_remove("CODEX_HOME")
            .env_remove("RIWORK_SHELL_ID")
            .env_remove("RIWORK_CHAT_ID")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let input = input.to_vec();
        let writer = thread::spawn(move || {
            // A rejected request may close stdin before an oversized input ends.
            let _ = stdin.write_all(&input);
        });
        let read = |mut pipe: Box<dyn Read + Send>| {
            thread::spawn(move || {
                let mut bytes = Vec::new();
                pipe.read_to_end(&mut bytes).unwrap();
                bytes
            })
        };
        let stdout = read(Box::new(child.stdout.take().unwrap()));
        let stderr = read(Box::new(child.stderr.take().unwrap()));
        let deadline = Instant::now() + Duration::from_secs(15);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("headless command did not finish: {args:?}");
            }
            thread::sleep(Duration::from_millis(10));
        };
        writer.join().unwrap();
        Output {
            status,
            stdout: stdout.join().unwrap(),
            stderr: stderr.join().unwrap(),
        }
    }

    fn invoke(&self, target: &str, input: &[u8]) -> Value {
        let output = self.run(&["native", "invoke", target, "--json"], input);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            output.stdout.iter().filter(|&&byte| byte == b'\n').count(),
            1
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn assert_no_runtime_started(&self) {
        assert!(
            !self.0.join("runtime").exists(),
            "desktop runtime was opened"
        );
        assert!(!self.0.join("state/run/chat.sock").exists());
        assert!(!self.0.join("home/forbidden-tmux-command").exists());
        let registry = self.0.join("state/sessions.json");
        if registry.exists() {
            let value: Value = serde_json::from_slice(&fs::read(registry).unwrap()).unwrap();
            assert!(value["sessions"].as_array().unwrap().is_empty());
        }
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn capabilities_and_local_snapshot_are_available_without_a_desktop() {
    let home = Home::new();
    let output = home.run(&["capabilities", "--json"], b"");
    assert!(output.status.success());
    let capabilities: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        capabilities["native"],
        json!({"v": 1, "methods": ["Hosts.local"]})
    );
    assert!(capabilities.get("watch").is_none());
    assert!(
        !home.0.join("state").exists(),
        "capability read created state"
    );

    let first = home.invoke("Hosts.local", b"{}");
    assert_eq!(first["v"], 1);
    assert!(first.get("error").is_none(), "{first}");
    let snapshot = &first["value"];
    assert_eq!(snapshot["version"], 1);
    assert_eq!(snapshot["sessions"], json!([]));
    assert_eq!(snapshot["total"], 0);
    assert_eq!(snapshot["partial"], false);
    assert!(snapshot["observedAt"].as_u64().unwrap() > 0);
    Uuid::parse_str(snapshot["hostId"].as_str().unwrap()).unwrap();
    let second = home.invoke("Hosts.local", b" \n ");
    assert_eq!(snapshot["hostId"], second["value"]["hostId"]);
    home.assert_no_runtime_started();
}

#[test]
fn invalid_native_requests_are_bounded_and_do_not_open_state() {
    let home = Home::new();
    for input in [
        b"[]".as_slice(),
        b"null",
        b"{",
        b"{}{}",
        br#"{"unexpected":true}"#,
    ] {
        let answer = home.invoke("Hosts.local", input);
        assert_eq!(answer["error"]["code"], "invalid_request", "{answer}");
        assert!(answer.get("value").is_none());
    }
    let too_large = home.invoke("Hosts.local", &vec![b' '; 256 * 1024 + 1]);
    assert_eq!(too_large["error"]["code"], "invalid_request");
    let unsupported = home.invoke("Settings.set", b"{}");
    assert_eq!(unsupported["error"]["code"], "invalid_request");
    assert!(!home.0.join("state").exists());
    for args in [
        vec!["native"],
        vec!["native", "invoke"],
        vec!["native", "invoke", "Hosts.local"],
        vec!["native", "invoke", "Hosts.local", "extra", "--json"],
        vec!["native", "other", "Hosts.local", "--json"],
    ] {
        let output = home.run(&args, b"{}");
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("Usage:"));
        assert!(output.stdout.is_empty());
    }
    home.assert_no_runtime_started();
}

#[test]
fn backend_error_retains_the_native_error_envelope() {
    let home = Home::new();
    fs::create_dir_all(home.0.join("state")).unwrap();
    fs::write(home.0.join("state/host-identity"), b"invalid").unwrap();
    let answer = home.invoke("Hosts.local", b"{}");
    assert_eq!(answer["v"], 1);
    assert_eq!(answer["error"]["code"], "cli_error");
    assert!(
        answer["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Invalid saved host identity")
    );
    assert!(answer.get("value").is_none());
    home.assert_no_runtime_started();
}
