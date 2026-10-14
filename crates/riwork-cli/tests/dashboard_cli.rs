//! `riwork dashboard --json`, which the remote connector answers the phone's `dashboard.get`
//! with (`remote/src/rpc/dashboard.rs`, "Dashboard extension" in docs/remote-protocol.md): its
//! shape, that it reports the store's projects and tasks and, with no chat host running, the
//! saved chats, and that reading it starts nothing: no chat host, chat, shell or tmux server.
//!
//! Every child runs in a throwaway RIWORK_HOME, HOME and runtime directory, so nothing of a
//! real installation is read or written.

use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

/// The real tmux, found where the CLI would look: the dashboard reads the shells from it.
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

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
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
            .join(format!("riwork-dashboard-{}", Uuid::new_v4()));
        fs::create_dir_all(path.join("home")).unwrap();
        Self {
            path,
            tmux: tmux.to_path_buf(),
        }
    }

    fn state(&self) -> PathBuf {
        self.path.join("state")
    }

    fn run(&self, args: &[&str]) -> Output {
        let child = Command::new(env!("CARGO_BIN_EXE_riwork-cli"))
            .args(args)
            .current_dir(&self.path)
            .env("HOME", self.path.join("home"))
            .env("RIWORK_HOME", self.state())
            .env("RIWORK_RUNTIME_DIR", self.path.join("runtime"))
            .env_remove("CODEX_HOME")
            // A test may itself run in a RiWork shell or chat: the command under test is
            // nobody's session.
            .env_remove("RIWORK_SHELL_ID")
            .env_remove("RIWORK_CHAT_ID")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        finish(child, args)
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

    fn project(&self, name: &str) -> String {
        let root = self.path.join(name);
        fs::create_dir_all(&root).unwrap();
        let project = self.ok(&["project", "add", root.to_str().unwrap(), "--json"]);
        project["id"].as_str().unwrap().to_owned()
    }

    fn task(&self, project: &str, title: &str, status: &str) -> String {
        let task = self.ok(&["task", "add", title, "--project", project, "--json"]);
        let id = task["id"].as_str().unwrap().to_owned();
        if status != "todo" {
            self.ok(&["task", "status", &id, status, "--json"]);
        }
        id
    }

    /// A chat as the chat host saves it, with a log written just now.
    fn saved_chat(&self, project: &str, state: Value) -> String {
        let id = Uuid::new_v4().to_string();
        let dir = self.state().join("chats").join(&id);
        fs::create_dir_all(&dir).unwrap();
        let info = json!({
            "id": id,
            "provider": "claude",
            "project_id": project,
            "cwd": self.path.join("alpha"),
            "title": "Refactor the parser",
            "created_at_unix": 1_790_000_000u64,
            "state": state,
        });
        fs::write(dir.join("info.json"), info.to_string()).unwrap();
        fs::write(dir.join("events.jsonl"), "").unwrap();
        id
    }

    /// The dashboard, and its raw output.
    fn dashboard(&self) -> (Value, String) {
        let output = self.run(&["dashboard", "--json"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8(output.stdout).unwrap();
        (serde_json::from_str(&text).unwrap(), text)
    }

    /// The private tmux server's socket name, as the CLI derives it (stable hash of the home).
    fn socket(&self) -> String {
        let mut hash = 0xcbf29ce484222325u64;
        for byte in self.state().as_os_str().to_string_lossy().as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        format!("riwork-{hash:016x}")
    }

    /// Whether this home's tmux server runs.
    fn tmux_server_runs(&self) -> bool {
        Command::new(&self.tmux)
            .args(["-L", &self.socket(), "list-sessions"])
            .output()
            .unwrap()
            .status
            .success()
    }

    /// Every file and directory under the state directory, with each file's bytes.
    fn files(&self) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
        fn walk(root: &Path, dir: &Path, found: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
            for entry in fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                let relative = path.strip_prefix(root).unwrap().to_path_buf();
                if entry.file_type().unwrap().is_dir() {
                    found.insert(relative, None);
                    walk(root, &path, found);
                } else {
                    found.insert(relative, Some(fs::read(&path).unwrap_or_default()));
                }
            }
        }
        let mut found = BTreeMap::new();
        walk(&self.state(), &self.state(), &mut found);
        found
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        // Close fixture shells, then stop the tmux server private to this home.
        if let Ok(registry) = fs::read(self.state().join("sessions.json")) {
            let registry: Value = serde_json::from_slice(&registry).unwrap_or(Value::Null);
            for session in registry["sessions"].as_array().into_iter().flatten() {
                if let Some(id) = session["id"].as_str() {
                    let _ = self.run(&["shell", "close", id]);
                }
            }
        }
        let _ = Command::new(&self.tmux)
            .args(["-L", &self.socket(), "kill-server"])
            .output();
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Reads a child's output while it runs: output that outgrows the pipe's buffer
/// would block it in write, and it would never exit.
fn read_meanwhile(pipe: Option<impl Read + Send + 'static>) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            pipe.read_to_end(&mut bytes).unwrap();
        }
        bytes
    })
}

fn finish(mut child: Child, what: &[&str]) -> Output {
    let stdout = read_meanwhile(child.stdout.take());
    let stderr = read_meanwhile(child.stderr.take());
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
    Output {
        status,
        stdout: stdout.join().unwrap(),
        stderr: stderr.join().unwrap(),
    }
}

/// Every key anywhere in `value`.
fn keys(value: &Value, found: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                found.push(key.clone());
                keys(value, found);
            }
        }
        Value::Array(items) => items.iter().for_each(|item| keys(item, found)),
        _ => {}
    }
}

const LISTS: [&str; 5] = [
    "attention",
    "working",
    "finished",
    "projects",
    "tasks_in_progress",
];

#[test]
fn the_dashboard_reports_the_store_and_the_saved_chats_and_starts_nothing() {
    let Some(tmux) = real_tmux() else {
        eprintln!("skipped: tmux is not installed");
        return;
    };
    let home = Home::new(&tmux);
    let alpha = home.project("alpha");
    let beta = home.project("beta");
    let shipping = home.task(&alpha, "Ship the release", "in_progress");
    home.task(&beta, "Write the docs", "todo");
    home.task(&beta, "Old chore", "done");
    let failed = home.saved_chat(
        &alpha,
        json!({"state": "failed", "message": "provider exited"}),
    );
    // A chat that was at work when its host ended: stopped, as the next host will find it.
    home.saved_chat(&beta, json!({"state": "running"}));
    let before = home.files();
    assert!(!home.tmux_server_runs());

    let (dashboard, text) = home.dashboard();
    // One line: the connector reads it under the reply limit.
    assert_eq!(text.trim_end().lines().count(), 1, "{text}");

    // The shape.
    assert_eq!(dashboard["v"], 1);
    let generated = dashboard["generated_at"].as_u64().unwrap();
    assert!(now().abs_diff(generated) < 120, "{generated}");
    assert_eq!(dashboard["chats_known"], true);
    for list in LISTS {
        let rows = dashboard[list]
            .as_array()
            .unwrap_or_else(|| panic!("{list}"));
        assert_eq!(dashboard[format!("{list}_total")], rows.len(), "{list}");
    }
    let mut top: Vec<&str> = dashboard
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    top.sort_unstable();
    assert_eq!(
        top,
        [
            "attention",
            "attention_total",
            "automations",
            "chats_known",
            "finished",
            "finished_total",
            "generated_at",
            "global",
            "projects",
            "projects_total",
            "tasks_in_progress",
            "tasks_in_progress_total",
            "totals",
            "v",
            "working",
            "working_total"
        ]
    );
    let mut all = Vec::new();
    keys(&dashboard, &mut all);
    assert!(
        !all.iter().any(|key| key == "model" || key == "root"),
        "{all:?}"
    );

    // The store's projects and tasks.
    let totals = &dashboard["totals"];
    assert_eq!(totals["projects"], 2);
    assert_eq!(totals["active_projects"], 2, "both have open tasks");
    assert_eq!(totals["live_shells"], 0);
    // The running chat was saved by a host that is gone: it is stopped, not live.
    assert_eq!(totals["live_chats"], 0);
    assert_eq!(
        totals["tasks"],
        json!({"todo": 1, "in_progress": 1, "done": 1})
    );
    assert_eq!(
        totals["agents"],
        json!({"working": 0, "waiting": 0, "done": 0, "unknown": 0, "exited": 0, "subagents": 0})
    );
    let projects = dashboard["projects"].as_array().unwrap();
    // Alpha's failed chat needs attention, so it comes first.
    assert_eq!(projects[0]["id"], alpha.as_str());
    assert_eq!(projects[0]["name"], "alpha");
    assert_eq!(projects[0]["attention"], 1);
    assert_eq!(projects[0]["worktrees"], 1);
    assert_eq!(projects[0]["live_chats"], 0);
    assert_eq!(projects[1]["id"], beta.as_str());
    assert_eq!(
        projects[1]["tasks"],
        json!({"todo": 1, "in_progress": 0, "done": 1})
    );
    assert_eq!(
        dashboard["tasks_in_progress"],
        json!([{
            "id": shipping,
            "title": "Ship the release",
            "project_id": alpha,
            "project_name": "alpha",
            "updated_at": dashboard["tasks_in_progress"][0]["updated_at"],
        }])
    );
    assert!(dashboard["tasks_in_progress"][0]["updated_at"].is_u64());

    // The saved chat that failed, from its log's time.
    assert_eq!(totals["attention"], 1);
    let attention = &dashboard["attention"][0];
    assert_eq!(attention["kind"], "chat_failed");
    assert_eq!(attention["message"], "provider exited");
    assert_eq!(
        attention["target"],
        json!({"type": "chat", "id": failed, "project_id": alpha})
    );
    assert_eq!(attention["session"]["agent"], "Claude");
    assert_eq!(attention["session"]["title"], "Refactor the parser");
    assert!(now().abs_diff(attention["at"].as_u64().unwrap()) < 120);
    assert_eq!(
        dashboard["global"],
        json!({"kind": "none", "exited_terminal": false})
    );
    assert_eq!(
        dashboard["automations"],
        json!({"state": "read", "total": 0, "paused": 0, "items": [], "items_total": 0})
    );

    // Nothing was started or written: no chat host (no socket, no lock, no run directory),
    // no chat, no shell registry, no tmux server. The automations' lock file is the only
    // file a read may leave, as the desktop's reads do.
    let after = home.files();
    assert!(!home.state().join("run").exists());
    assert!(!home.state().join("sessions.json").exists());
    assert!(!home.tmux_server_runs());
    let added: Vec<&PathBuf> = after
        .keys()
        .filter(|path| !before.contains_key(*path))
        .collect();
    assert!(
        added
            .iter()
            .all(|path| *path == Path::new("schedules.lock")),
        "{added:?}"
    );
    for (path, bytes) in &before {
        assert_eq!(after.get(path), Some(bytes), "{} changed", path.display());
    }

    // The same, as text, and nothing else is accepted.
    let output = home.run(&["dashboard"]);
    assert!(output.status.success());
    let summary = String::from_utf8_lossy(&output.stdout);
    assert!(summary.starts_with("2 projects (2 active)"), "{summary}");
    assert!(summary.contains("provider exited"), "{summary}");
    assert!(!home.run(&["dashboard", "--all"]).status.success());
}

#[test]
fn chats_that_cannot_be_read_are_unknown_not_none() {
    let Some(tmux) = real_tmux() else {
        eprintln!("skipped: tmux is not installed");
        return;
    };
    let home = Home::new(&tmux);
    let alpha = home.project("alpha");
    // One damaged chat is left out; the chats are known without it.
    let chat = home.saved_chat(&alpha, json!({"state": "stopped"}));
    fs::write(
        home.state().join("chats").join(&chat).join("info.json"),
        "{ damaged",
    )
    .unwrap();
    let (dashboard, _) = home.dashboard();
    assert_eq!(dashboard["chats_known"], true);
    assert_eq!(dashboard["totals"]["live_chats"], 0);
    // A chats directory that cannot be read at all: no host runs, so nothing is known.
    fs::remove_dir_all(home.state().join("chats")).unwrap();
    fs::write(home.state().join("chats"), "not a directory").unwrap();
    let (dashboard, _) = home.dashboard();
    assert_eq!(dashboard["chats_known"], false);
    assert!(dashboard["totals"].get("live_chats").is_none());
    assert!(dashboard["projects"][0].get("live_chats").is_none());
    assert_eq!(dashboard["global"], json!({"kind": "unknown"}));
    let output = home.run(&["dashboard"]);
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("chats not read"),
        "{output:?}"
    );
}

#[test]
fn capabilities_say_the_dashboard_is_there() {
    let Some(tmux) = real_tmux() else {
        eprintln!("skipped: tmux is not installed");
        return;
    };
    let home = Home::new(&tmux);
    let capabilities = home.ok(&["capabilities", "--json"]);
    assert_eq!(capabilities["v"], 1);
    assert_eq!(capabilities["dashboard"], true);
}

#[test]
#[ignore = "slow: a real tmux session"]
fn a_live_terminal_is_counted_and_reading_leaves_the_registry_alone() {
    let Some(tmux) = real_tmux() else {
        eprintln!("skipped: tmux is not installed");
        return;
    };
    let home = Home::new(&tmux);
    let alpha = home.project("alpha");
    let shell = home.ok(&[
        "shell",
        "create",
        "--project",
        &alpha,
        "--command",
        "sleep 600",
        "--json",
    ]);
    let registry = fs::read(home.state().join("sessions.json")).unwrap();
    let (dashboard, _) = home.dashboard();
    assert_eq!(dashboard["totals"]["live_shells"], 1);
    assert_eq!(dashboard["projects"][0]["live_shells"], 1);
    assert_eq!(dashboard["totals"]["active_projects"], 1);
    // A plain shell has no agent, so it is in no list of work.
    assert_eq!(dashboard["working_total"], 0);
    assert_eq!(
        fs::read(home.state().join("sessions.json")).unwrap(),
        registry,
        "reading changed the shell registry"
    );
    let shells = home.ok(&["shell", "list", "--all", "--json"]);
    assert_eq!(shells.as_array().unwrap().len(), 1);
    assert_eq!(shells[0]["id"], shell["id"]);
}
