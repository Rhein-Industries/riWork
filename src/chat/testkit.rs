//! Test support for the drivers: a fake provider program that replays a
//! fixture and records what the driver sends it.
//!
//! The fake is `testdata/fake_provider.py` (see its header for the fixture
//! format) behind a one-line shell wrapper, so a driver starts it exactly like
//! the real `codex` or `claude`. Fixtures are small transcripts modelled on
//! recordings of the real programs, with the driver's side written as `expect`
//! steps.

use super::driver::DriverConfig;
use super::model::{ApprovalMode, ChatEvent, Provider, Transcript};
use serde_json::Value;
use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::Duration;
use uuid::Uuid;

const FAKE_PROVIDER: &str = include_str!("testdata/fake_provider.py");

/// How long a test waits for an event before it gives up.
pub const WAIT: Duration = Duration::from_secs(20);

/// A fake provider program in a temporary directory, removed when dropped.
pub struct Fake {
    pub dir: PathBuf,
    pub program: PathBuf,
    record: PathBuf,
}

impl Fake {
    /// `fixtures` are the texts of fixture files, one per start of the program.
    pub fn new(fixtures: &[&str]) -> Self {
        let dir = env::temp_dir().join(format!("riwork-chat-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        let script = dir.join("fake_provider.py");
        fs::write(&script, FAKE_PROVIDER).unwrap();
        let record = dir.join("record.jsonl");
        let mut command = format!(
            "#!/bin/sh\nexec /usr/bin/python3 '{}' '{}'",
            script.display(),
            record.display()
        );
        for (index, text) in fixtures.iter().enumerate() {
            let path = dir.join(format!("fixture-{}.ndjson", index + 1));
            fs::write(&path, text).unwrap();
            command.push_str(&format!(" '{}'", path.display()));
        }
        command.push_str(" -- \"$@\"\n");
        let program = dir.join("provider");
        fs::write(&program, command).unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            dir,
            program,
            record,
        }
    }

    /// A driver configuration that starts this fake in its own directory.
    pub fn config(&self, provider: Provider) -> DriverConfig {
        DriverConfig {
            provider,
            program: self.program.clone(),
            cwd: self.dir.clone(),
            approval_mode: ApprovalMode::Supervised,
            model: None,
            effort: None,
            resume: None,
            extra_args: Vec::new(),
            env: Vec::new(),
            env_remove: Vec::new(),
        }
    }

    /// Everything the fake recorded, in order.
    pub fn entries(&self) -> Vec<Value> {
        fs::read_to_string(&self.record)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// The `start` records: how the program was started, once per start.
    pub fn starts(&self) -> Vec<Value> {
        self.entries()
            .into_iter()
            .filter_map(|entry| entry.get("start").cloned())
            .collect()
    }

    /// The frames the driver sent, in order.
    pub fn received(&self) -> Vec<Value> {
        self.entries()
            .into_iter()
            .filter_map(|entry| entry.get("recv").cloned())
            .collect()
    }

    /// Whether the fake recorded an entry with this key (`eof`, `mismatch`, …).
    pub fn saw(&self, key: &str) -> bool {
        self.entries().iter().any(|entry| entry.get(key).is_some())
    }

    /// The frames sent for `method` (a JSON-RPC method), in order.
    pub fn received_method(&self, method: &str) -> Vec<Value> {
        self.received()
            .into_iter()
            .filter(|frame| frame["method"] == method)
            .collect()
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Events from `events` up to and including the first that `done` accepts.
/// Panics with what it saw if none arrives within `WAIT`.
pub fn until(events: &Receiver<ChatEvent>, done: impl Fn(&ChatEvent) -> bool) -> Vec<ChatEvent> {
    let mut seen = Vec::new();
    loop {
        match events.recv_timeout(WAIT) {
            Ok(event) => {
                let end = done(&event);
                seen.push(event);
                if end {
                    return seen;
                }
            }
            Err(error) => panic!("no matching event ({error}); saw {seen:#?}"),
        }
    }
}

/// What a tab would show after `events`.
pub fn fold(events: &[ChatEvent]) -> Transcript {
    let mut transcript = Transcript::default();
    for event in events {
        transcript.apply(event);
    }
    transcript
}

/// A fixture file shipped with the tests, by name under `testdata/`.
pub fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/chat/testdata")
        .join(name);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}
