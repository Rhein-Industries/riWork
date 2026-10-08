//! The agent fields of `riwork shell list --json`, `riwork orchestrator list --json` and
//! `riwork project list --json`, as the remote connector reads them for the phone
//! (`remote/src/rpc.rs`, `SESSION_FIELDS` and `PROJECT_FIELDS`): `activity`,
//! `activity_since_unix`, `subagents_working` and `subagent_kinds` per shell, and
//! `last_edited_unix`, `last_activity_unix` and `agents` per project, and `last_activity_unix`
//! per shell (when tmux last saw output in it).
//!
//! Every child runs in a throwaway RIWORK_HOME and HOME with its own tmux server, and the
//! agents are `sleep` processes dressed up as Claude and Codex by editing the registry. The
//! hook input goes through the real `riwork agent-hook claude` and the Codex rollouts are
//! files this test writes, so nothing of a real installation is read or written and no
//! model is called.

use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
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
            .join(format!("riwork-agent-activity-{}", Uuid::new_v4()));
        fs::create_dir_all(path.join("home")).unwrap();
        Self {
            path,
            tmux: tmux.to_path_buf(),
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_riwork"));
        command
            .args(args)
            .current_dir(&self.path)
            .env("HOME", self.path.join("home"))
            .env("RIWORK_HOME", self.path.join("state"))
            .env("RIWORK_RUNTIME_DIR", self.path.join("runtime"))
            .env_remove("CODEX_HOME")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn state(&self) -> PathBuf {
        self.path.join("state")
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

    fn project(&self, name: &str) -> String {
        let root = self.path.join(name);
        fs::create_dir_all(&root).unwrap();
        let project = self.ok(&["project", "add", root.to_str().unwrap(), "--json"]);
        project["id"].as_str().unwrap().to_owned()
    }

    /// A live shell running `sleep`, registered as `harness` (`claude`, `codex`, `grok`) or as a
    /// plain shell when `None`.
    fn shell(&self, project: &str, harness: Option<&str>) -> String {
        let shell = self.ok(&[
            "shell",
            "create",
            "--project",
            project,
            "--command",
            "sleep 600",
            "--json",
        ]);
        let id = shell["id"].as_str().unwrap().to_owned();
        if let Some(harness) = harness {
            self.edit_registry(&id, |session| session["harness"] = json!(harness));
        }
        id
    }

    fn edit_registry(&self, id: &str, edit: impl Fn(&mut Value)) {
        let path = self.state().join("sessions.json");
        let mut registry: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        for session in registry["sessions"].as_array_mut().unwrap() {
            if session["id"] == id {
                edit(session);
            }
        }
        fs::write(path, serde_json::to_vec(&registry).unwrap()).unwrap();
    }

    /// One Claude hook event, through the command Claude would run.
    fn hook(&self, shell: &str, payload: Value) {
        let state = self.state();
        let mut child = self
            .command(&["agent-hook", "claude", state.to_str().unwrap(), shell])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        let output = finish(child, &["agent-hook"]);
        assert!(output.status.success());
    }

    /// The binding that makes a Codex thread this shell's; its id. The rollout is `rollout`'s.
    fn bind_codex(&self, shell: &str) -> String {
        let thread = Uuid::new_v4().to_string();
        fs::create_dir_all(self.state().join("agent-activity")).unwrap();
        fs::write(
            self.state().join(format!("agent-activity/{shell}.json")),
            json!({"shell_id": shell, "thread_id": thread, "codex_home": self.path.join("codex")})
                .to_string(),
        )
        .unwrap();
        thread
    }

    fn rollout(&self, thread: &str, records: &[Value]) -> PathBuf {
        let directory = self.path.join("codex/sessions/2026/10/03");
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!("rollout-2026-10-03T10-00-00-{thread}.jsonl"));
        let mut text = String::new();
        for record in records {
            text.push_str(&record.to_string());
            text.push('\n');
        }
        fs::write(&path, text).unwrap();
        path
    }

    fn shells(&self, extra: &[&str]) -> Vec<Value> {
        let mut args = vec!["shell", "list"];
        args.extend_from_slice(extra);
        args.push("--json");
        self.ok(&args).as_array().unwrap().clone()
    }
}

fn entry<'a>(list: &'a [Value], id: &str) -> &'a Value {
    list.iter()
        .find(|entry| entry["id"] == id)
        .unwrap_or_else(|| panic!("{id} is not listed"))
}

fn hook_event(event: &str, session: &str, extra: Value) -> Value {
    let mut payload = json!({
        "hook_event_name": event,
        "session_id": session,
        "prompt": "private request",
        "last_assistant_message": "private reply"
    });
    for (key, value) in extra.as_object().unwrap() {
        payload[key] = value.clone();
    }
    payload
}

impl Home {
    /// The private tmux server's socket name, as the CLI derives it (stable hash of the home).
    fn socket(&self) -> String {
        let mut hash = 0xcbf29ce484222325u64;
        for byte in self.state().as_os_str().to_string_lossy().as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        format!("riwork-{hash:016x}")
    }

    /// Ends a shell's tmux session behind the registry's back, as a crashed agent would.
    fn kill_session(&self, id: &str) {
        let output = Command::new(&self.tmux)
            .args([
                "-L",
                &self.socket(),
                "kill-session",
                "-t",
                &format!("={id}"),
            ])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        // Close fixture shells, then stop the tmux server private to this home.
        if let Ok(registry) = fs::read(self.state().join("sessions.json")) {
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

fn event(kind: &str, turn: &str) -> Value {
    json!({"timestamp": "2026-10-03T10:00:30.000Z", "type": "event_msg",
        "payload": {"type": kind, "turn_id": turn}})
}

fn session_meta(thread: &str, source: Value) -> Value {
    json!({"timestamp": "2026-10-03T10:00:00.000Z", "type": "session_meta",
        "payload": {"id": thread, "source": source}})
}

#[test]
#[ignore = "slow: real tmux sessions and wall-clock activity times"]
fn lists_report_what_each_agent_is_doing_and_the_projects_count_them() {
    let Some(tmux) = real_tmux() else {
        eprintln!("skipped: tmux is not installed");
        return;
    };
    let home = Home::new(&tmux);
    let project = home.project("app");
    let quiet_project = home.project("quiet");

    let claude = home.shell(&project, Some("claude"));
    let claude_idle = home.shell(&project, Some("claude"));
    let codex = home.shell(&project, Some("codex"));
    let grok = home.shell(&project, Some("grok"));
    let plain = home.shell(&project, None);
    let gone = home.shell(&project, Some("claude"));

    // Claude: a turn with two subagents, so it works with both counted.
    home.hook(
        &claude,
        hook_event("SessionStart", "session-a", json!({"source": "startup"})),
    );
    home.hook(
        &claude,
        hook_event("UserPromptSubmit", "session-a", json!({"prompt_id": "p1"})),
    );
    for (agent, kind) in [("agent-1", "general-purpose"), ("agent-2", "Explore")] {
        home.hook(
            &claude,
            hook_event(
                "SubagentStart",
                "session-a",
                json!({"prompt_id": "p1", "agent_id": agent, "agent_type": kind}),
            ),
        );
    }
    // Codex: a working parent with one child thread mid-turn and one that finished.
    let parent = home.bind_codex(&codex);
    home.rollout(
        &parent,
        &[
            session_meta(&parent, json!("cli")),
            event("task_started", "t1"),
        ],
    );
    for (records, role) in [
        (vec![event("task_started", "c1")], "explorer"),
        (
            vec![event("task_started", "c2"), event("task_complete", "c2")],
            "worker",
        ),
    ] {
        let child = Uuid::new_v4().to_string();
        let mut all = vec![session_meta(
            &child,
            json!({"subagent": {"thread_spawn": {"parent_thread_id": parent, "agent_role": role}}}),
        )];
        all.extend(records);
        home.rollout(&child, &all);
    }
    // A Claude shell whose tmux session ends is exited.
    home.hook(
        &gone,
        hook_event("UserPromptSubmit", "session-g", json!({"prompt_id": "g1"})),
    );
    home.kill_session(&gone);

    let before = now();
    for extra in [&["--all"][..], &["--project", project.as_str()][..]] {
        let list = home.shells(extra);
        let claude_entry = entry(&list, &claude);
        assert_eq!(claude_entry["activity"], "working", "{claude_entry}");
        assert_eq!(claude_entry["subagents_working"], 2);
        assert_eq!(
            claude_entry["subagent_kinds"],
            json!(["general-purpose", "Explore"])
        );
        let since = claude_entry["activity_since_unix"].as_u64().unwrap();
        assert!(since + 120 >= before && since <= now(), "{since}");
        // Everything the list always had is still there.
        assert_eq!(claude_entry["harness"], "claude");
        assert_eq!(claude_entry["alive"], true);
        assert!(claude_entry["created_at_unix"].as_u64().is_some());

        let codex_entry = entry(&list, &codex);
        assert_eq!(codex_entry["activity"], "working", "{codex_entry}");
        assert_eq!(codex_entry["subagents_working"], 1);
        assert_eq!(codex_entry["subagent_kinds"], json!(["explorer"]));
        assert_eq!(codex_entry["activity_since_unix"], 1_791_021_630u64);

        // Hooks that never spoke: not known. Grok has no tracking at all.
        let idle = entry(&list, &claude_idle);
        assert_eq!(idle["activity"], "unknown");
        assert!(
            idle.get("subagents_working").is_none() && idle.get("activity_since_unix").is_none()
        );
        let grok_entry = entry(&list, &grok);
        assert_eq!(grok_entry["activity"], "unknown");
        assert!(grok_entry.get("subagents_working").is_none());
        // A shell that runs no agent has no activity to report.
        let plain_entry = entry(&list, &plain);
        for key in [
            "activity",
            "activity_since_unix",
            "subagents_working",
            "subagent_kinds",
        ] {
            assert!(plain_entry.get(key).is_none(), "{key}");
        }
        let exited = entry(&list, &gone);
        assert_eq!(exited["activity"], "exited");
        assert_eq!(exited["alive"], false);
        // tmux knows when a live shell last printed, whatever runs in it; a gone one has no time.
        assert!(exited.get("last_activity_unix").is_none());
        for live in [&claude, &claude_idle, &codex, &grok, &plain] {
            let at = entry(&list, live)["last_activity_unix"].as_u64().unwrap();
            assert!(at + 120 >= before && at <= now() + 1, "{live}: {at}");
        }
    }

    // The turn ends: done, and the subagents go with it.
    home.hook(
        &claude,
        hook_event(
            "Stop",
            "session-a",
            json!({"prompt_id": "p1", "background_tasks": [], "session_crons": []}),
        ),
    );
    let list = home.shells(&["--all"]);
    let finished = entry(&list, &claude);
    assert_eq!(finished["activity"], "done");
    assert!(
        finished.get("subagents_working").is_none() && finished.get("subagent_kinds").is_none()
    );
    // The hook cursor keeps identifiers only.
    let cursor = fs::read_to_string(
        home.state()
            .join(format!("agent-hooks/claude/{claude}.json")),
    )
    .unwrap();
    assert!(!cursor.contains("private"), "{cursor}");

    // The projects: counted by state, and dated once the app has published dates.
    let projects = home.ok(&["project", "list", "--json"]);
    let app = projects
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == project.as_str())
        .unwrap();
    assert_eq!(
        app["agents"],
        json!({"working": 1, "waiting": 0, "done": 1}),
        "{app}"
    );
    assert!(app.get("last_edited_unix").is_none());
    let quiet = projects
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == quiet_project.as_str())
        .unwrap();
    assert_eq!(
        quiet["agents"],
        json!({"working": 0, "waiting": 0, "done": 0})
    );
    fs::write(
        home.state().join("project-recency.json"),
        json!({"v": 1, "updated_at": 5, "projects": {project.as_str(): 1_790_000_123u64}})
            .to_string(),
    )
    .unwrap();
    let projects = home.ok(&["project", "list", "--json"]);
    let find = |id: &str| {
        projects
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(find(&project)["last_edited_unix"], 1_790_000_123u64);
    assert!(find(&quiet_project).get("last_edited_unix").is_none());
    // The shells are live, so the project has a time of its own; the one without shells has none.
    assert!(find(&project)["last_activity_unix"].as_u64().is_some());
    assert!(find(&quiet_project).get("last_activity_unix").is_none());
    // The project fields the phone relies on are unchanged.
    for key in ["id", "name", "root", "created_at"] {
        assert!(find(&project).get(key).is_some(), "{key}");
    }

    // The plain listing is the same text as before.
    let text = String::from_utf8(home.run(&["shell", "list", "--all"]).stdout).unwrap();
    assert!(
        !text.contains("working") && !text.contains("subagent"),
        "{text}"
    );
}

#[test]
#[ignore = "slow: real tmux sessions and wall-clock activity times"]
fn orchestrators_report_activity_too() {
    let Some(tmux) = real_tmux() else {
        eprintln!("skipped: tmux is not installed");
        return;
    };
    let home = Home::new(&tmux);
    let project = home.project("app");
    let created = home.ok(&[
        "orchestrator",
        "create",
        "--project",
        &project,
        "--command",
        "sleep 600",
        "--json",
    ]);
    let id = created["id"].as_str().unwrap().to_owned();
    home.edit_registry(&id, |session| session["harness"] = json!("claude"));
    home.hook(
        &id,
        hook_event("UserPromptSubmit", "session-o", json!({"prompt_id": "o1"})),
    );
    let list = home.ok(&["orchestrator", "list", "--json"]);
    let listed = list.as_array().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["kind"], "orchestrator");
    assert_eq!(listed[0]["activity"], "working", "{}", listed[0]);
    assert!(listed[0]["activity_since_unix"].as_u64().is_some());
}

/// The shell figure is tmux's own `window_activity`, so the text a phone sends moves it (the
/// pane echoes what it is sent); the project figure is the newest of its shells.
#[test]
#[ignore = "slow: real tmux sessions and wall-clock activity times"]
fn a_project_was_last_active_when_its_newest_shell_last_printed() {
    let Some(tmux) = real_tmux() else {
        eprintln!("skipped: tmux is not installed");
        return;
    };
    let home = Home::new(&tmux);
    let (app, other, quiet) = (
        home.project("app"),
        home.project("other"),
        home.project("quiet"),
    );
    let started = now();
    let older = home.shell(&app, None);
    let newer = home.shell(&app, Some("claude"));
    let elsewhere = home.shell(&other, None);
    let time_of = |list: &[Value], id: &str| entry(list, id)["last_activity_unix"].as_u64();
    let project_time = |id: &str| {
        home.ok(&["project", "list", "--json"])
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == id)
            .unwrap()["last_activity_unix"]
            .as_u64()
    };

    let list = home.shells(&["--all"]);
    for id in [&older, &newer, &elsewhere] {
        let at = time_of(&list, id).expect("a live shell has a time");
        assert!(at + 2 >= started && at <= now() + 1, "{id}: {at}");
    }
    let elsewhere_at = time_of(&list, &elsewhere);
    // A project is as active as its newest shell, and the figures are the ones `shell list` gave.
    assert_eq!(
        project_time(&app),
        time_of(&list, &older).max(time_of(&list, &newer))
    );
    assert_eq!(project_time(&other), time_of(&list, &elsewhere));
    // No shell, no figure.
    assert!(
        home.ok(&["project", "list", "--json"])
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == quiet.as_str())
            .unwrap()
            .get("last_activity_unix")
            .is_none()
    );

    // tmux counts whole seconds. Text sent to the older shell is echoed by its terminal, and
    // that is output: the shell, and with it the project, is now newer than anything else.
    thread::sleep(Duration::from_millis(1200));
    assert!(
        home.run(&["shell", "send", &older, "typed on the phone"])
            .status
            .success()
    );
    let list = home.shells(&["--all"]);
    let (after, before) = (
        time_of(&list, &older).unwrap(),
        time_of(&list, &newer).unwrap(),
    );
    assert!(
        after > before,
        "the shell that was typed into is newer: {after} vs {before}"
    );
    assert_eq!(
        time_of(&list, &elsewhere),
        elsewhere_at,
        "a shell nobody typed into did not move"
    );
    assert_eq!(project_time(&app), Some(after));
    // The same list for one project says the same.
    let scoped = home.shells(&["--project", app.as_str()]);
    assert_eq!(time_of(&scoped, &older), Some(after));
    assert_eq!(scoped.len(), 2);

    // A shell that is gone has no time, and its project follows the ones that are left.
    home.kill_session(&older);
    let list = home.shells(&["--all"]);
    assert!(entry(&list, &older).get("last_activity_unix").is_none());
    assert_eq!(project_time(&app), Some(before));
}

/// A project's own orchestrator counts for its project; the global orchestrator belongs to no
/// project, however newly it printed.
#[test]
#[ignore = "slow: real tmux sessions and wall-clock activity times"]
fn the_projects_orchestrator_counts_and_the_global_one_does_not() {
    let Some(tmux) = real_tmux() else {
        eprintln!("skipped: tmux is not installed");
        return;
    };
    let home = Home::new(&tmux);
    let (with_shell, only_orchestrator) =
        (home.project("with-shell"), home.project("orchestrated"));
    home.shell(&with_shell, None);
    let created = home.ok(&[
        "orchestrator",
        "create",
        "--project",
        &only_orchestrator,
        "--command",
        "sleep 600",
        "--json",
    ]);
    let project_orchestrator = created["id"].as_str().unwrap().to_owned();
    thread::sleep(Duration::from_millis(1200));
    let global = home.ok(&["orchestrator", "create", "--command", "sleep 600", "--json"]);
    let global = global["id"].as_str().unwrap().to_owned();

    let orchestrators = home.ok(&["orchestrator", "list", "--json"]);
    let orchestrators = orchestrators.as_array().unwrap();
    let time_of = |id: &str| {
        entry(orchestrators, id)["last_activity_unix"]
            .as_u64()
            .unwrap()
    };
    assert!(time_of(&global) > time_of(&project_orchestrator));

    let projects = home.ok(&["project", "list", "--json"]);
    let figure = |id: &str| {
        projects
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == id)
            .unwrap()["last_activity_unix"]
            .as_u64()
    };
    assert_eq!(
        figure(&only_orchestrator),
        Some(time_of(&project_orchestrator))
    );
    for id in [&with_shell, &only_orchestrator] {
        assert_ne!(
            figure(id),
            Some(time_of(&global)),
            "the global orchestrator is nobody's"
        );
    }
}

#[test]
fn a_corrupt_recency_file_or_missing_tmux_state_never_breaks_the_project_list() {
    let Some(tmux) = real_tmux() else {
        eprintln!("skipped: tmux is not installed");
        return;
    };
    let home = Home::new(&tmux);
    let project = home.project("app");
    fs::write(home.state().join("project-recency.json"), "{ not json").unwrap();
    let projects = home.ok(&["project", "list", "--json"]);
    assert_eq!(projects[0]["id"], project.as_str());
    assert!(projects[0].get("last_edited_unix").is_none());
    // No shell at all: zero agents, which is an answer.
    assert_eq!(
        projects[0]["agents"],
        json!({"working": 0, "waiting": 0, "done": 0})
    );
}
