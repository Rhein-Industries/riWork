//! Tests that drive tmux itself: terminal input, bounded clients, and the
//! registry behaviours that depend on tmux liveness. Real tmux servers use a
//! private `-L` socket that is killed on drop; without tmux those tests skip.
//! Every test that starts tmux, a fake tmux script or another child process is
//! `#[ignore]`d as slow; run them with `cargo test -- --include-ignored`.
use super::*;
use std::time::{Duration, Instant};

struct Fixture {
    manager: SessionManager,
    root: PathBuf,
}

impl Fixture {
    fn new(tmux: impl FnOnce(&Path) -> PathBuf) -> Self {
        let root = env::temp_dir().join(format!("riwork-tmux-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        Self {
            manager: SessionManager {
                inherit_parent: false,
                user_creation: true,
                home: root.clone(),
                tmux: tmux(&root),
                socket_name: format!("riwork-test-{}", &Uuid::new_v4().simple().to_string()[..12]),
            },
            root,
        }
    }

    fn with_tmux() -> Option<Self> {
        find_tmux().map(|tmux| Self::new(|_| tmux))
    }

    /// A fake tmux whose `list-sessions` prints `live` and whose other
    /// commands succeed silently.
    #[cfg(unix)]
    fn with_live_sessions(live: &[&str]) -> Self {
        let names: String = live.iter().map(|name| format!("{name}\\n")).collect();
        Self::new(|root| {
            let tmux = root.join("fake-tmux");
            Self::script(&tmux, &format!("printf '{names}'"));
            tmux
        })
    }

    #[cfg(unix)]
    fn script(path: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    /// A session named by a fresh UUID whose pane stores every byte it
    /// receives in `received`, unmodified by the line discipline.
    fn recording_session(&self) -> String {
        let id = Uuid::new_v4().to_string();
        let script = format!(
            "stty raw -echo cs8; : > {ready}; exec cat >> {received}",
            ready = quote_arg(&self.root.join("ready").to_string_lossy()),
            received = quote_arg(&self.root.join("received").to_string_lossy()),
        );
        self.manager
            .tmux_checked(&[
                "new-session",
                "-d",
                "-s",
                &id,
                "-x",
                "100",
                "-y",
                "30",
                &format!("sh -c {}", quote_arg(&script)),
            ])
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.root.join("ready").exists() {
            assert!(Instant::now() < deadline, "pane never became ready");
            std::thread::sleep(Duration::from_millis(20));
        }
        id
    }

    fn received(&self) -> Vec<u8> {
        fs::read(self.root.join("received")).unwrap_or_default()
    }

    fn wait_for_received(&self, length: usize) -> Vec<u8> {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let received = self.received();
            if received.len() >= length || Instant::now() >= deadline {
                return received;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn registry(&self, sessions: Vec<ShellSession>) {
        self.manager.write_registry(&Registry { sessions }).unwrap();
    }
}

#[test]
fn shell_creation_records_user_or_automation_provenance() {
    let Some(f) = Fixture::with_tmux() else {
        return;
    };
    let store = crate::store::Store::open(&f.root).unwrap();
    let project = store
        .add_project(f.root.clone(), Some("provenance".into()))
        .unwrap();
    let user = f
        .manager
        .for_user()
        .create(project.id.clone(), None, f.root.clone(), None)
        .unwrap();
    assert!(user.user_opened && !user.is_worker());
    assert!(
        !crate::project_tabs::TabStore::at(&f.root, &project.id)
            .unwrap()
            .list()
            .unwrap()
            .iter()
            .find(|e| e.key == format!("shell:{}", user.id))
            .unwrap()
            .hidden
    );
    let worker = f
        .manager
        .for_automation()
        .create(project.id.clone(), None, f.root.clone(), None)
        .unwrap();
    assert!(!worker.user_opened && worker.is_worker());
    assert!(
        crate::project_tabs::TabStore::at(&f.root, &project.id)
            .unwrap()
            .list()
            .unwrap()
            .iter()
            .find(|e| e.key == format!("shell:{}", worker.id))
            .unwrap()
            .hidden
    );
    // An orchestrator itself is pinned/root even though it was not a user shell create.
    let mut orch = worker;
    orch.kind = ShellKind::Orchestrator;
    assert!(!orch.is_worker());
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // tmux leaves its socket file behind after `kill-server`.
        let socket = self
            .manager
            .tmux_text(&["display-message", "-p", "#{socket_path}"])
            .ok()
            .map(|path| PathBuf::from(path.trim()));
        let _ = self.manager.tmux_command(&["kill-server"]);
        if let Some(socket) = socket.filter(|path| path.is_absolute()) {
            let _ = fs::remove_file(socket);
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn shell(id: &str, harness: Option<&str>, editor: Option<&str>) -> ShellSession {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "project_id": null,
        "worktree_id": null,
        "kind": "project",
        "cwd": "/tmp",
        "command": null,
        "editor_path": editor,
        "harness": harness,
        "created_at_unix": 0
    }))
    .unwrap()
}

#[test]
#[ignore = "slow: real tmux; scheduled text reaches the pane verbatim with one Return"]
fn submitted_text_survives_tmux_command_parsing_verbatim() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    let id = fixture.recording_session();
    let cases = [
        "select 1;".to_owned(),
        "find . -exec ls {} \\;".to_owned(),
        "\\;".to_owned(),
        ";".to_owned(),
        ";;".to_owned(),
        "a; b ;c;".to_owned(),
        "ends with backslash\\".to_owned(),
        "héllo ✓ 日本語 🚀;".to_owned(),
        "line one\nline two;\n".to_owned(),
        "-b -t leading dash; #{pane_id} $(touch never) `x` '\"".to_owned(),
        // Past tmux's command-length limit; schedules allow up to 16 KiB.
        format!("{};", "0123456789abcdef".repeat(16 * 1024 / 16 + 512)),
    ];
    let mut expected = Vec::new();
    for text in &cases {
        fixture.manager.paste_and_submit(&id, text).unwrap();
        // Exactly one Return follows the text; the raw pane reports it as CR.
        expected.extend_from_slice(text.as_bytes());
        expected.push(b'\r');
        let received = fixture.wait_for_received(expected.len());
        assert!(
            received == expected,
            "unexpected input after {:?}: got {} bytes, wanted {}",
            &text[..text.len().min(40)],
            received.len(),
            expected.len()
        );
    }
    let buffers = fixture.manager.tmux_text(&["list-buffers"]).unwrap();
    assert!(!buffers.contains("riwork-input-"), "{buffers}");
}

#[cfg(unix)]
#[test]
#[ignore = "slow: real child process and a wall-clock timeout"]
fn bounded_runner_kills_a_child_that_outlives_the_timeout() {
    let mut command = Command::new("sleep");
    command.arg("60");
    let started = Instant::now();
    let error = run_bounded(
        command,
        None,
        Duration::from_millis(200),
        "tmux list-sessions",
    )
    .unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(
        error.contains("tmux list-sessions did not finish within 0.2s"),
        "{error}"
    );
    let missing = run_bounded(
        Command::new("/nonexistent/tmux"),
        None,
        Duration::from_secs(1),
        "tmux",
    )
    .unwrap_err();
    assert!(missing.starts_with("run /nonexistent/tmux:"), "{missing}");
}

#[test]
fn only_a_saved_codex_or_unlabelled_pane_records_a_codex_launch() {
    let fixture = Fixture::new(|_| PathBuf::from("/unused/tmux"));
    let (manager, home) = (&fixture.manager, &fixture.root);
    // (id, saved harness, whether the launch may relabel the pane)
    let cases = [
        (
            "00000000-0000-4000-8000-000000000001",
            Some("claude"),
            false,
        ),
        ("00000000-0000-4000-8000-000000000002", Some("grok"), false),
        ("00000000-0000-4000-8000-000000000003", Some("codex"), true),
        ("00000000-0000-4000-8000-000000000004", None, true),
    ];
    fixture.registry(
        cases
            .iter()
            .map(|(id, harness, _)| shell(id, *harness, None))
            .collect(),
    );
    fs::create_dir_all(home.join("agent-activity")).unwrap();
    let activity = |id: &str| home.join("agent-activity").join(format!("{id}.json"));
    let binding = crate::codex_accounts::CodexAccountBinding {
        id: Some("account-a".into()),
        label: Some("A".into()),
        email: None,
        home: home.join("codex-a"),
    };
    for (id, _, _) in cases {
        fs::write(activity(id), "{}").unwrap();
        manager
            .record_codex_launch(id, &binding, &["--yolo".into()], false)
            .unwrap();
    }
    let saved = manager.read_registry().unwrap().sessions;
    for ((id, harness, records), session) in cases.into_iter().zip(&saved) {
        assert_eq!(session.id, id);
        if records {
            assert_eq!(session.harness, Some(HarnessKind::Codex));
            assert_eq!(session.codex_account_id.as_deref(), Some("account-a"));
            assert_eq!(session.codex_home.as_ref(), Some(&binding.home));
            assert!(session.unrestricted);
            assert!(!activity(id).exists(), "a new Codex launch resets activity");
        } else {
            // A Claude or Grok agent that runs `codex exec` keeps its own pane.
            let kept = match harness {
                Some("claude") => HarnessKind::Claude,
                _ => HarnessKind::Grok,
            };
            assert_eq!(session.harness, Some(kept));
            assert_eq!(session.codex_account_id, None);
            assert_eq!(session.codex_home, None);
            assert!(!session.unrestricted);
            assert!(activity(id).exists(), "the pane's activity is untouched");
        }
    }
}

#[cfg(unix)]
#[test]
#[ignore = "slow: fake tmux processes; pruning removes only exited editors"]
fn list_prunes_exited_editor_sessions_and_nothing_else() {
    let live_editor = "00000000-0000-4000-8000-0000000000a1";
    let dead_editor = "00000000-0000-4000-8000-0000000000a2";
    let dead_agent = "00000000-0000-4000-8000-0000000000b1";
    let live_agent = "00000000-0000-4000-8000-0000000000b2";
    let fixture = Fixture::with_live_sessions(&[live_editor, live_agent]);
    fixture.registry(vec![
        shell(live_editor, None, Some("/work/a.txt")),
        shell(dead_editor, None, Some("/work/b.txt")),
        shell(dead_agent, Some("claude"), None),
        shell(live_agent, Some("codex"), None),
    ]);
    let listed = fixture.manager.list().unwrap();
    let summary = |sessions: &[ShellSession]| {
        sessions
            .iter()
            .map(|session| (session.id.clone(), session.alive))
            .collect::<Vec<_>>()
    };
    let expected = vec![
        (live_editor.to_owned(), true),
        (dead_agent.to_owned(), false),
        (live_agent.to_owned(), true),
    ];
    assert_eq!(summary(&listed), expected);
    let saved = fixture.manager.read_registry().unwrap().sessions;
    assert_eq!(
        saved
            .iter()
            .map(|session| session.id.as_str())
            .collect::<Vec<_>>(),
        [live_editor, dead_agent, live_agent]
    );
    assert_eq!(summary(&fixture.manager.list().unwrap()), expected);
}

#[cfg(unix)]
#[test]
#[ignore = "slow: fake tmux processes; a tmux failure never prunes the registry"]
fn pruning_never_waits_for_the_registry_lock_and_tolerates_tmux_failure() {
    let dead_editor = "00000000-0000-4000-8000-0000000000a2";
    let fixture = Fixture::with_live_sessions(&[]);
    fixture.registry(vec![shell(dead_editor, None, Some("/work/b.txt"))]);
    {
        // Scheduled delivery holds this lock while it calls `get`, which lists.
        let _held = fixture.manager.lock_registry().unwrap();
        let listed = fixture.manager.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert!(!listed[0].alive);
    }
    assert_eq!(fixture.manager.read_registry().unwrap().sessions.len(), 1);
    assert!(fixture.manager.list().unwrap().is_empty());
    assert!(fixture.manager.read_registry().unwrap().sessions.is_empty());

    // An unreadable tmux is "unknown", not "exited".
    fixture.registry(vec![shell(dead_editor, None, Some("/work/b.txt"))]);
    Fixture::script(&fixture.manager.tmux, "echo 'protocol error' >&2; exit 1");
    assert!(fixture.manager.list().is_err());
    assert_eq!(fixture.manager.read_registry().unwrap().sessions.len(), 1);
    assert!(fixture.manager.close(dead_editor).is_err());
    assert_eq!(fixture.manager.read_registry().unwrap().sessions.len(), 1);
}

/// Start `command` as a plain project shell in `directory`, and return what
/// `pwd -P` printed inside the pane.
fn started_in(fixture: &Fixture, directory: &Path, index: usize) -> String {
    let out = fixture.root.join(format!("pwd-{index}"));
    let command = format!(
        "pwd -P > {}; exec sleep 300",
        quote_arg(&out.to_string_lossy())
    );
    fixture
        .manager
        .create(
            Uuid::new_v4().to_string(),
            None,
            directory.to_owned(),
            Some(command),
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        // The shell writes the line and its newline in one call.
        if let Ok(text) = fs::read_to_string(&out)
            && let Some(text) = text.strip_suffix('\n')
        {
            return text.to_owned();
        }
        assert!(
            Instant::now() < deadline,
            "the pane never printed its directory"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Directory names that tmux would read as formats or command separators.
const AWKWARD_DIRECTORIES: &[&str] = &[
    "C#Tools",
    "a##b",
    "#{pane_id}",
    "#T and #H and #P",
    "tail#",
    "#(touch PWNED)",
    "x;",
    "x\\;",
    "semi;colon",
    "日本語 ✓ é",
    "quote'd \"$x\" `y`",
    "new\nline",
];

#[test]
#[ignore = "slow: real tmux; directory names are never expanded or run"]
fn directories_tmux_would_reinterpret_are_started_in_exactly() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    for (index, name) in AWKWARD_DIRECTORIES.iter().enumerate() {
        let directory = fixture.root.join("dirs").join(name);
        fs::create_dir_all(&directory).unwrap();
        assert_eq!(
            started_in(&fixture, &directory, index),
            directory.to_string_lossy(),
            "{name:?}"
        );
        // A `#(...)` in a directory name must never run.
        assert!(!fixture.root.join("dirs").join("PWNED").exists());
        assert!(!directory.join("PWNED").exists());
    }
}

#[cfg(unix)]
#[test]
#[ignore = "slow: fake tmux process; a pane started elsewhere is refused and killed"]
fn a_directory_tmux_did_not_start_in_is_refused_and_its_session_removed() {
    let calls = |root: &Path| root.join("calls");
    let fixture = Fixture::new(|root| {
        let tmux = root.join("fake-tmux");
        // Reports the pane's directory as something else, as tmux does when it
        // expands the name it was given.
        Fixture::script(
            &tmux,
            &format!(
                "echo \"$*\" >> {}\ncase \"$*\" in *new-session*) printf '/somewhere/else\\n';; esac",
                quote_arg(&calls(root).to_string_lossy())
            ),
        );
        tmux
    });
    let directory = fixture.root.join("project");
    fs::create_dir(&directory).unwrap();
    let error = fixture
        .manager
        .create(
            Uuid::new_v4().to_string(),
            None,
            directory.clone(),
            Some("sleep 1".into()),
        )
        .unwrap_err();
    assert!(error.contains("/somewhere/else"), "{error}");
    assert!(error.contains(&*directory.to_string_lossy()), "{error}");
    let log = fs::read_to_string(calls(&fixture.root)).unwrap();
    assert!(log.contains("kill-session"), "{log}");
    assert!(fixture.manager.read_registry().unwrap().sessions.is_empty());
}

#[cfg(unix)]
#[test]
#[ignore = "slow: fake tmux processes; a tmux failure never replaces or respawns a session"]
fn a_tmux_that_cannot_answer_does_not_make_a_session_dead() {
    let orchestrator = "00000000-0000-4000-8000-0000000000c1";
    let absent = "00000000-0000-4000-8000-0000000000c2";
    let fixture = Fixture::new(|root| {
        let tmux = root.join("fake-tmux");
        Fixture::script(&tmux, &format!("printf '{orchestrator}\\n'"));
        tmux
    });
    let mut row = shell(orchestrator, Some("codex"), None);
    row.kind = ShellKind::Orchestrator;
    fixture.registry(vec![row]);
    let manager = &fixture.manager;
    assert!(manager.is_alive(orchestrator).unwrap());
    assert!(!manager.is_alive(absent).unwrap());

    // The server is there but does not answer: nothing is dropped or replaced.
    let calls = fixture.root.join("calls");
    Fixture::script(
        &manager.tmux,
        &format!(
            "echo \"$*\" >> {}\necho 'lost server connection' >&2; exit 1",
            quote_arg(&calls.to_string_lossy())
        ),
    );
    assert!(
        manager
            .is_alive(orchestrator)
            .unwrap_err()
            .contains("lost server connection")
    );
    let error = manager
        .orchestrator_create(fixture.root.clone(), Some("sleep 1".into()))
        .unwrap_err();
    assert!(error.contains("lost server connection"), "{error}");
    let error = manager.load_orchestrator_skill(orchestrator).unwrap_err();
    assert!(error.contains("lost server connection"), "{error}");
    let error = manager
        .respawn_command(orchestrator, "codex resume x")
        .unwrap_err();
    assert!(error.contains("lost server connection"), "{error}");
    let log = fs::read_to_string(&calls).unwrap();
    assert!(!log.contains("new-session"), "{log}");
    assert!(!log.contains("respawn-pane"), "{log}");
    let saved = manager.read_registry().unwrap().sessions;
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].id, orchestrator);
}

// Direct typing (`send_keys`), and the pure alignment of screen and history captures.

use crate::session_keys::{Item, Key};

impl Fixture {
    fn recording_pane(&self) -> String {
        let id = self.recording_session();
        self.registry(vec![shell(&id, None, None)]);
        id
    }

    fn type_items(&self, id: &str, items: &[Item]) {
        self.manager.send_keys(id, items).unwrap();
    }
}

fn typed(text: &str) -> Item {
    Item::Text(text.to_owned())
}

fn key(name: &str) -> Item {
    Item::Key(Key::parse(name).unwrap())
}

#[test]
#[ignore = "slow: real tmux; typed text reaches the pane verbatim"]
fn typed_text_survives_tmux_command_parsing_verbatim() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    let id = fixture.recording_pane();
    let cases = [
        "select 1;".to_owned(),
        "find . -exec ls {} \\;".to_owned(),
        "\\;".to_owned(),
        "\\\\;".to_owned(),
        ";".to_owned(),
        ";;".to_owned(),
        "a; b ;c;".to_owned(),
        "ends with backslash\\".to_owned(),
        " ".to_owned(),
        "  spaces   inside  and around  ".to_owned(),
        "héllo ✓ 日本語 🚀;".to_owned(),
        "-b -t leading dash; #{pane_id} $(touch never) `x` '\"".to_owned(),
        "-l".to_owned(),
        "--".to_owned(),
        format!("{};", "0123456789abcdef".repeat(255)),
    ];
    let mut expected = Vec::new();
    for text in &cases {
        fixture.type_items(&id, &[typed(text)]);
        // Text alone never adds a Return.
        expected.extend_from_slice(text.as_bytes());
        let received = fixture.wait_for_received(expected.len());
        assert!(
            received == expected,
            "unexpected input after {:?}: got {} bytes, wanted {}",
            &text[..text.len().min(40)],
            received.len(),
            expected.len()
        );
    }
    // Items of one batch arrive in order, whatever their kind.
    let batch = [
        typed("ls;"),
        key("Enter"),
        typed("\\;"),
        key("Tab"),
        key("C-c"),
        typed(";"),
        key("Up"),
    ];
    fixture.type_items(&id, &batch);
    expected.extend_from_slice(b"ls;\r\\;\t\x03;\x1b[A");
    assert_eq!(fixture.wait_for_received(expected.len()), expected);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(fixture.received(), expected, "nothing extra was typed");
}

#[test]
#[ignore = "slow: process-backed tmux integration"]
fn every_key_sends_the_bytes_a_terminal_would() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    let id = fixture.recording_pane();
    let mut keys = vec![
        ("Enter", b"\r".to_vec()),
        ("Tab", b"\t".to_vec()),
        ("BTab", b"\x1b[Z".to_vec()),
        ("Escape", b"\x1b".to_vec()),
        ("Backspace", b"\x7f".to_vec()),
        ("Delete", b"\x1b[3~".to_vec()),
        ("Up", b"\x1b[A".to_vec()),
        ("Down", b"\x1b[B".to_vec()),
        ("Right", b"\x1b[C".to_vec()),
        ("Left", b"\x1b[D".to_vec()),
        ("Home", b"\x1b[1~".to_vec()),
        ("End", b"\x1b[4~".to_vec()),
        ("PageUp", b"\x1b[5~".to_vec()),
        ("PageDown", b"\x1b[6~".to_vec()),
    ];
    let controls: Vec<(String, Vec<u8>)> = (b'a'..=b'z')
        .map(|letter| (format!("C-{}", char::from(letter)), vec![letter - b'a' + 1]))
        .collect();
    keys.extend(
        controls
            .iter()
            .map(|(name, bytes)| (name.as_str(), bytes.clone())),
    );
    let mut expected = Vec::new();
    for (name, bytes) in &keys {
        fixture.type_items(&id, &[key(name)]);
        expected.extend_from_slice(bytes);
        assert_eq!(
            fixture.wait_for_received(expected.len()),
            expected,
            "{name}"
        );
    }
    // The same keys as one batch: runs of keys share a tmux command.
    let all: Vec<Item> = keys.iter().map(|(name, _)| key(name)).collect();
    fixture.type_items(&id, &all);
    let once = expected.clone();
    expected.extend_from_slice(&once);
    assert_eq!(fixture.wait_for_received(expected.len()), expected);
}

#[test]
#[ignore = "slow: process-backed tmux integration"]
fn typing_edits_and_runs_commands_in_a_real_shell() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    let work = fixture.root.join("work");
    fs::create_dir_all(&work).unwrap();
    fs::write(work.join("unique-file-name-xyz"), "").unwrap();
    let id = fixture.pane(
        80,
        24,
        "exec env PS1='PROMPT> ' HISTFILE=/dev/null BASH_SILENCE_DEPRECATION_WARNING=1 bash --noprofile --norc -i",
    );
    fixture.wait_for_screen(&id, |screen| screen.contains("PROMPT>"));
    let lines = |screen: &str, wanted: &str| screen.lines().filter(|line| *line == wanted).count();

    // Backspace edits the line; Enter runs it.
    fixture.type_items(
        &id,
        &[
            typed("echo abx"),
            key("Backspace"),
            typed("c"),
            key("Enter"),
        ],
    );
    let screen = fixture.wait_for_screen(&id, |screen| lines(screen, "abc") == 1);
    assert!(screen.contains("PROMPT> echo abc"), "{screen}");

    // Up recalls it; the run repeats.
    fixture.type_items(&id, &[key("Up"), key("Enter")]);
    fixture.wait_for_screen(&id, |screen| lines(screen, "abc") == 2);

    // Left, Home, Right, Delete and End move within the line. A key at the
    // very end of one batch and at the start of the next both land.
    fixture.type_items(
        &id,
        &[typed("echo 12"), key("Left"), typed("X"), key("Enter")],
    );
    fixture.wait_for_screen(&id, |screen| lines(screen, "1X2") == 1);
    fixture.type_items(&id, &[typed("echo xyz"), key("Home")]);
    fixture.type_items(
        &id,
        &[
            key("Right"),
            key("Right"),
            key("Right"),
            key("Right"),
            key("Right"),
            key("Delete"),
            key("End"),
            typed("!"),
            key("Enter"),
        ],
    );
    fixture.wait_for_screen(&id, |screen| lines(screen, "yz!") == 1);

    // Tab completes a file name.
    fixture.type_items(&id, &[typed("echo unique-fi"), key("Tab"), key("Enter")]);
    fixture.wait_for_screen(&id, |screen| lines(screen, "unique-file-name-xyz") == 1);

    // C-c stops a running command: the next line reaches the prompt, not sleep.
    fixture.type_items(&id, &[typed("sleep 1000"), key("Enter")]);
    fixture.wait_for_command(&id, "sleep");
    fixture.type_items(&id, &[key("C-c")]);
    fixture.wait_for_command(&id, "bash");
    fixture.type_items(&id, &[typed("echo alive"), key("Enter")]);
    fixture.wait_for_screen(&id, |screen| lines(screen, "alive") == 1);

    // C-d at an empty prompt ends the shell.
    fixture.type_items(&id, &[key("C-d")]);
    let deadline = Instant::now() + Duration::from_secs(15);
    while fixture.manager.is_alive(&id).unwrap() {
        assert!(Instant::now() < deadline, "C-d never ended the shell");
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
#[ignore = "slow: process-backed tmux integration"]
fn copy_mode_is_left_before_typing_so_keys_reach_the_program() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    let id = fixture.recording_pane();
    let pane = pane_target(&id);
    fixture
        .manager
        .tmux_checked(&["copy-mode", "-t", &pane])
        .unwrap();
    assert_eq!(fixture.pane_format(&id, "#{pane_in_mode}"), "1");
    fixture.type_items(&id, &[typed("q"), key("Enter"), key("C-c")]);
    assert_eq!(fixture.pane_format(&id, "#{pane_in_mode}"), "0");
    assert_eq!(fixture.wait_for_received(3), b"q\r\x03");
    // Without a mode, no cancel is attempted and nothing else is typed.
    fixture.type_items(&id, &[typed("z")]);
    assert_eq!(fixture.wait_for_received(4), b"q\r\x03z");
}

#[test]
#[ignore = "slow: process-backed tmux integration"]
fn disabled_terminal_input_is_refused_and_nothing_is_typed() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    let id = fixture.recording_pane();
    let pane = pane_target(&id);
    fixture
        .manager
        .tmux_checked(&["select-pane", "-d", "-t", &pane])
        .unwrap();
    assert_eq!(fixture.pane_format(&id, "#{pane_input_off}"), "1");
    for items in [
        vec![typed("nope;")],
        vec![key("Enter")],
        vec![typed("a"), key("C-c")],
    ] {
        let error = fixture.manager.send_keys(&id, &items).unwrap_err();
        assert!(error.starts_with("input_unavailable: "), "{error}");
    }
    // Copy mode does not excuse it: the pane is not cancelled out of either.
    fixture
        .manager
        .tmux_checked(&["copy-mode", "-t", &pane])
        .unwrap();
    let error = fixture.manager.send_keys(&id, &[key("Enter")]).unwrap_err();
    assert!(error.starts_with("input_unavailable: "), "{error}");
    assert_eq!(fixture.pane_format(&id, "#{pane_in_mode}"), "1");
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(fixture.received(), b"");

    fixture
        .manager
        .tmux_checked(&["select-pane", "-e", "-t", &pane])
        .unwrap();
    fixture.type_items(&id, &[typed("ok")]);
    assert_eq!(fixture.wait_for_received(2), b"ok");
}

#[test]
#[ignore = "slow: process-backed tmux integration"]
fn unknown_and_exited_shells_are_not_found_and_bad_ids_are_invalid() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    let id = fixture.recording_pane();
    let items = [key("Enter")];
    let unknown = Uuid::new_v4().to_string();
    let error = fixture.manager.send_keys(&unknown, &items).unwrap_err();
    assert!(error.starts_with("not_found: "), "{error}");
    let error = fixture.manager.send_keys("12345678", &items).unwrap_err();
    assert!(error.starts_with("invalid_request: "), "{error}");
    fixture.manager.kill_tmux_session(&id).unwrap();
    let error = fixture.manager.send_keys(&id, &items).unwrap_err();
    assert!(error.starts_with("not_found: "), "{error}");
    // Anything else that keeps tmux from answering is "not sent", not "not found".
    let broken = fixture.root.join("broken-tmux");
    Fixture::script(&broken, "echo 'protocol error' >&2; exit 1");
    let manager = SessionManager {
        inherit_parent: false,
        user_creation: true,
        home: fixture.root.clone(),
        tmux: broken,
        socket_name: fixture.manager.socket_name.clone(),
    };
    let error = manager.send_keys(&id, &items).unwrap_err();
    assert!(error.starts_with("not_sent: "), "{error}");
}

#[test]
#[ignore = "slow: real tmux and a wall-clock wait; typing honours the input lock"]
fn a_batch_waits_for_the_shells_input_lock() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    let id = fixture.recording_pane();
    let held = crate::session_viewport::lock(&fixture.root, &id, "input").unwrap();
    std::thread::scope(|scope| {
        let typing = scope.spawn(|| fixture.manager.send_keys(&id, &[typed("locked")]));
        std::thread::sleep(Duration::from_millis(400));
        assert!(
            !typing.is_finished(),
            "typed while another writer held the lock"
        );
        assert_eq!(fixture.received(), b"");
        drop(held);
        typing.join().unwrap().unwrap();
    });
    assert_eq!(fixture.wait_for_received(6), b"locked");
}

#[test]
#[ignore = "slow: process-backed tmux integration"]
fn a_wedged_tmux_fails_a_batch_within_the_bound() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    let id = fixture.recording_pane();
    // Every call goes through the bounded runner: none may hang the caller.
    let tmux = fixture.manager.tmux.clone();
    let wedged = fixture.root.join("wedged-tmux");
    Fixture::script(
        &wedged,
        &format!(
            "case \"$*\" in *kill-server|*socket_path*) exec {} \"$@\";; *send-keys*) exec sleep 60;; esac; exec {} \"$@\"",
            quote_arg(&tmux.to_string_lossy()),
            quote_arg(&tmux.to_string_lossy()),
        ),
    );
    let manager = SessionManager {
        inherit_parent: false,
        user_creation: true,
        home: fixture.root.clone(),
        tmux: wedged,
        socket_name: fixture.manager.socket_name.clone(),
    };
    let started = Instant::now();
    let error = manager.send_keys(&id, &[typed("x")]).unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(15));
    assert!(error.contains("did not finish"), "{error}");
    // A batch that may have been half-sent does not claim it was not sent.
    assert!(!error.starts_with("not_sent: "), "{error}");
}



#[test]
#[ignore = "slow: process-backed tmux integration"]
fn capture_screen_ends_with_exactly_the_visible_rows_blank_ones_included() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    // Two lines of a 30x8 pane; the cursor rests after "cd".
    let id = fixture.pane(30, 8, "printf 'ab\\ncd'; exec sleep 60");
    fixture.wait_for_screen(&id, |screen| screen.contains("cd"));
    let capture = fixture.manager.capture_screen(&id, 200).unwrap();
    assert_eq!(capture.output, "ab\ncd\n\n\n\n\n\n\n");
    assert_eq!(
        capture.screen,
        Some(Screen {
            cursor: Cursor { x: 2, y: 1 },
            rows: 8,
            cols: 30,
            in_mode: false,
            history_size: 0,
            alternate: false
        })
    );
    // No more lines than history plus rows, and the text is the plain capture.
    assert_eq!(capture.output, fixture.manager.capture(&id, 200).unwrap());
    assert_eq!(capture.output.lines().count(), 8);
    // A missing shell is an error, as for `capture`.
    assert!(
        fixture
            .manager
            .capture_screen(&Uuid::new_v4().to_string(), 10)
            .is_err()
    );
}

#[test]
#[ignore = "slow: process-backed tmux integration"]
fn capture_screen_keeps_the_last_rows_lines_as_the_screen_over_history() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    // 31 printed rows through a 5-row pane: 26 scroll into history.
    let id = fixture.pane(20, 5, "seq 1 30; printf x; exec sleep 60");
    fixture.wait_for_screen(&id, |screen| screen.ends_with("\nx\n"));
    for (lines, total) in [(1, 6), (3, 8), (26, 31), (200, 31), (100_000, 31)] {
        let capture = fixture.manager.capture_screen(&id, lines).unwrap();
        let all: Vec<&str> = capture.output.lines().collect();
        let screen = capture.screen.expect("screen");
        assert_eq!(all.len(), total, "--lines {lines}");
        assert_eq!((screen.rows, screen.cols), (5, 20));
        assert_eq!(all[all.len() - 5..], ["27", "28", "29", "30", "x"]);
        // The cursor is on the last screen row, right after the "x".
        assert_eq!(screen.cursor, Cursor { x: 1, y: 4 });
        assert_eq!(all[all.len() - 5 + screen.cursor.y as usize], "x");
    }
    // Blank rows at the bottom of a screen that has history are still counted.
    let id = fixture.pane(
        20,
        5,
        "seq 1 12; printf '\\033[2J\\033[Htop'; exec sleep 60",
    );
    fixture.wait_for_screen(&id, |screen| screen.contains("top"));
    let capture = fixture.manager.capture_screen(&id, 4).unwrap();
    let all: Vec<&str> = capture.output.lines().collect();
    assert_eq!(all.len(), 4 + 5, "{:?}", capture.output);
    assert_eq!(all[all.len() - 5..], ["top", "", "", "", ""]);
    assert_eq!(capture.screen.unwrap().cursor, Cursor { x: 3, y: 0 });
}

#[test]
#[ignore = "slow: process-backed tmux integration"]
fn capture_screen_follows_the_alternate_screen_and_reports_copy_mode() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    // History from the normal screen, then a full-screen program.
    let id = fixture.pane(
        30,
        6,
        "seq 1 30; printf '\\033[?1049h\\033[2;3Hzz'; exec sleep 60",
    );
    fixture.wait_for_screen(&id, |screen| screen.contains("zz"));
    let capture = fixture.manager.capture_screen(&id, 100_000).unwrap();
    let all: Vec<&str> = capture.output.lines().collect();
    let screen = capture.screen.expect("screen");
    assert_eq!((screen.rows, screen.cols), (6, 30));
    assert_eq!(all[all.len() - 6..], ["", "  zz", "", "", "", ""]);
    assert_eq!(screen.cursor, Cursor { x: 4, y: 1 });
    assert!(!screen.in_mode);

    fixture
        .manager
        .tmux_checked(&["copy-mode", "-t", &pane_target(&id)])
        .unwrap();
    let in_mode = fixture.manager.capture_screen(&id, 100_000).unwrap();
    assert!(in_mode.screen.unwrap().in_mode);
    assert_eq!(in_mode.output, capture.output);
}

#[test]
#[ignore = "slow: process-backed tmux integration"]
fn capture_screen_falls_back_to_a_plain_capture_when_the_report_fails() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    let id = fixture.pane(30, 4, "printf hi; exec sleep 60");
    fixture.wait_for_screen(&id, |screen| screen.contains("hi"));
    // A tmux that cannot run the combined command still captures.
    let tmux = fixture.manager.tmux.clone();
    let picky = fixture.root.join("picky-tmux");
    Fixture::script(
        &picky,
        &format!(
            "case \"$*\" in *display-message*capture-pane*|*capture-pane*display-message*) echo 'no lists' >&2; exit 1;; esac; exec {} \"$@\"",
            quote_arg(&tmux.to_string_lossy()),
        ),
    );
    let manager = SessionManager {
        inherit_parent: false,
        user_creation: true,
        home: fixture.root.clone(),
        tmux: picky,
        socket_name: fixture.manager.socket_name.clone(),
    };
    let capture = manager.capture_screen(&id, 10).unwrap();
    assert_eq!(capture.screen, None);
    assert_eq!(capture.output, "hi\n\n\n\n");
}

const VALID_REPORT: &str = "8|30|2|1|0|0|0\n";

#[test]
fn align_screen_restores_trimmed_rows_and_refuses_what_it_cannot_align() {
    let report =
        |rows, cols, x, y, mode, history| format!("{rows}|{cols}|{x}|{y}|{mode}|{history}|0\n");
    // The full capture passes through untouched.
    let full = "1\n2\nhi\n\n\n";
    let kept = align_screen(full, &report(3, 40, 2, 0, 0, 2), 100).unwrap();
    assert_eq!(kept.output, full);
    assert_eq!(
        kept.screen,
        Some(Screen {
            cursor: Cursor { x: 2, y: 0 },
            rows: 3,
            cols: 40,
            in_mode: false,
            history_size: 2,
            alternate: false
        })
    );
    // A tmux that trimmed the trailing blank rows gets them back, whether it
    // dropped some or all of them, or the last newline.
    for trimmed in ["1\n2\nhi\n\n", "1\n2\nhi\n", "1\n2\nhi"] {
        let aligned = align_screen(trimmed, &report(5, 40, 2, 2, 0, 2), 100).unwrap();
        assert_eq!(aligned.output, "1\n2\nhi\n\n\n\n\n", "{trimmed:?}");
    }
    // Only as much history as exists is expected, and `requested` caps it.
    let capped = align_screen("9\nhi\n", &report(3, 40, 0, 0, 0, 50), 1).unwrap();
    assert_eq!(capped.output, "9\nhi\n\n\n");
    assert_eq!(
        align_screen("", &report(2, 10, 0, 0, 0, 0), 5)
            .unwrap()
            .output,
        "\n\n"
    );
    assert!(align_screen("a\nb\nc\nd\n", &report(2, 10, 0, 0, 0, 0), 5).is_none());
    // Copy mode is reported.
    assert!(
        align_screen("a\n", &report(1, 10, 0, 0, 1, 0), 5)
            .unwrap()
            .screen
            .unwrap()
            .in_mode
    );
    // The history size and the alternate screen are reported.
    let alternate = align_screen("1\n2\nhi\n\n\n", "3|40|2|0|0|2|1\n", 100).unwrap();
    let screen = alternate.screen.unwrap();
    assert_eq!((screen.history_size, screen.alternate), (2, true));
    // An unreadable or impossible report leaves the screen out.
    for bad in [
        "",
        "8|30|2|1|0|0",
        "8|30|2|1|0",
        "8|30|2|1|0|0|0|0",
        "a|30|2|1|0|0|0",
        "0|30|2|1|0|0|0",
        "8|0|2|1|0|0|0",
        "8|30|2|8|0|0|0",
        "8|30|2|-1|0|0|0",
        "8|30|2|1|2|0|0",
        "8|30|2|1|0|0|2",
        "8|30|2|1|0|0|",
        "8|30|2|1|0||0",
        "8|30|2|1||0|0",
        "8,30,2,1,0,0,0",
    ] {
        assert!(
            align_screen("a\n", &format!("{bad}\n"), 5).is_none(),
            "{bad:?}"
        );
    }
    assert!(align_screen("a\n", VALID_REPORT, 5).is_some());
}

#[test]
#[ignore = "slow: process-backed tmux integration"]
fn a_key_after_text_arrives_after_the_pause_and_the_line_still_runs() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    // A pane that stamps every read from the terminal with a clock.
    let log = fixture.root.join("reads.log");
    let reader = fixture.root.join("reader.py");
    fs::write(
        &reader,
        "import os, sys, time\n\
         out = open(sys.argv[1], 'a', buffering=1)\n\
         out.write('ready\\n')\n\
         while True:\n\
         \tdata = os.read(0, 4096)\n\
         \tif not data:\n\
         \t\tbreak\n\
         \tout.write('%.6f %s\\n' % (time.monotonic(), data.hex()))\n",
    )
    .unwrap();
    let id = fixture.pane(
        80,
        24,
        &format!(
            "stty raw -echo; exec /usr/bin/python3 {} {}",
            quote_arg(&reader.to_string_lossy()),
            quote_arg(&log.to_string_lossy())
        ),
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    while !fs::read_to_string(&log)
        .unwrap_or_default()
        .contains("ready")
    {
        assert!(Instant::now() < deadline, "reader never started");
        std::thread::sleep(Duration::from_millis(20));
    }
    let started = Instant::now();
    fixture.type_items(&id, &[typed("echo hi"), key("Enter")]);
    assert!(started.elapsed() >= crate::session_keys::KEY_AFTER_TEXT_PAUSE);
    let reads = || -> Vec<(f64, String)> {
        fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.split_once(' '))
            .map(|(time, hex)| (time.parse().unwrap(), hex.to_owned()))
            .collect()
    };
    let deadline = Instant::now() + Duration::from_secs(15);
    while !reads().iter().any(|(_, hex)| hex.ends_with("0d")) {
        assert!(Instant::now() < deadline, "Enter never arrived");
        std::thread::sleep(Duration::from_millis(20));
    }
    let reads = reads();
    let (enter_at, enter) = reads.last().unwrap();
    assert_eq!(enter, "0d", "Enter arrives on its own");
    let text: String = reads[..reads.len() - 1]
        .iter()
        .map(|(_, hex)| hex.as_str())
        .collect();
    assert_eq!(text, "6563686f206869", "echo hi");
    let last_text_at = reads[reads.len() - 2].0;
    assert!(
        enter_at - last_text_at >= 0.14,
        "Enter followed the text by {:.0} ms",
        (enter_at - last_text_at) * 1000.0
    );
}

#[test]
#[ignore = "slow: process-backed tmux integration"]
fn text_then_enter_runs_the_command_in_a_real_shell() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    let id = fixture.pane(
        80,
        24,
        "exec env PS1='PROMPT> ' HISTFILE=/dev/null BASH_SILENCE_DEPRECATION_WARNING=1 bash --noprofile --norc -i",
    );
    fixture.wait_for_screen(&id, |screen| screen.contains("PROMPT>"));
    fixture.type_items(&id, &[typed("echo hi"), key("Enter")]);
    let lines = |screen: &str, wanted: &str| screen.lines().filter(|line| *line == wanted).count();
    fixture.wait_for_screen(&id, |screen| lines(screen, "hi") == 1);
    // Several boundaries in one batch: each Enter runs the text before it.
    fixture.type_items(
        &id,
        &[
            typed("echo one"),
            key("Enter"),
            typed("echo two"),
            key("Enter"),
            key("Up"),
            key("Enter"),
        ],
    );
    let screen = fixture.wait_for_screen(&id, |screen| lines(screen, "two") == 2);
    assert_eq!(lines(&screen, "one"), 1, "{screen}");
}

// Styled capture and the change-detection loop of `shell output`.

/// `text` without its well-formed SGR sequences; anything else escaped fails.
fn without_sgr(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        assert_eq!(chars.next(), Some('['), "escape other than CSI in {text:?}");
        loop {
            match chars.next() {
                Some('m') => break,
                Some(c) if c.is_ascii_digit() || c == ';' || c == ':' => {}
                other => panic!("not an SGR sequence ({other:?}) in {text:?}"),
            }
        }
    }
    out
}

#[test]
#[ignore = "slow: process-backed tmux integration"]
fn styled_capture_keeps_the_sgr_sequences_tmux_writes_and_nothing_else() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    // Colors (16, 256, truecolor), attributes, an OSC 8 hyperlink and a
    // line-drawing charset: `capture-pane -e` writes all of them.
    let id = fixture.pane(
        40,
        6,
        "printf '\\033[1;31mred\\033[0m plain \\033[38;5;200m256\\033[0m \\033[38;2;10;20;30mtrue\\033[0m\\n\
         \\033[3mit\\033[23m \\033[4mun\\033[24m \\033[7minv\\033[27m \\033[2mdim\\033[22m \\033[48;2;1;2;3mbg\\033[0m\\n\
         \\033]8;;http://x\\033\\\\link\\033]8;;\\033\\\\ \\033(0lqk\\033(Bdone'; exec sleep 60",
    );
    fixture.wait_for_screen(&id, |screen| screen.contains("done"));
    let raw = fixture
        .manager
        .tmux_text(&[
            "capture-pane",
            "-p",
            "-e",
            "-t",
            &pane_target(&id),
            "-S",
            "-100",
        ])
        .unwrap();
    // The premise: tmux really writes more than SGR here.
    assert!(raw.contains("\u{1b}]8;;"), "no OSC in {raw:?}");
    assert!(
        raw.contains('\u{e}') && raw.contains('\u{f}'),
        "no SO/SI in {raw:?}"
    );

    let styled = fixture
        .manager
        .capture_screen_styled(&id, 100, true)
        .unwrap();
    let plain = fixture
        .manager
        .capture_screen_styled(&id, 100, false)
        .unwrap();
    for sgr in [
        "\u{1b}[1m",
        "\u{1b}[31m",
        "\u{1b}[38;5;200m",
        "\u{1b}[38;2;10;20;30m",
        "\u{1b}[48;2;1;2;3m",
        "\u{1b}[3m",
        "\u{1b}[4m",
        "\u{1b}[7m",
        "\u{1b}[2m",
        "\u{1b}[0m",
    ] {
        assert!(
            styled.output.contains(sgr),
            "{sgr:?} missing in {:?}",
            styled.output
        );
    }
    // Nothing but SGR is escaped, and no control character but the newline.
    let text = without_sgr(&styled.output);
    assert!(
        text.chars().all(|c| !c.is_control() || c == '\n'),
        "{:?}",
        styled.output
    );
    // The text is the plain capture's, line for line, screen and all.
    assert_eq!(text, plain.output);
    assert_eq!(styled.screen, plain.screen);
    assert!(plain.output.contains("link"), "{:?}", plain.output);
    assert!(!plain.output.contains('\u{1b}'));
    assert_eq!(styled.output.lines().count(), plain.output.lines().count());
    assert_eq!(
        styled.output.matches('\n').count(),
        plain.output.matches('\n').count()
    );

    // The screen rule holds for every `--lines`: the last `rows` lines.
    for lines in [1, 3, 100_000] {
        let styled = fixture
            .manager
            .capture_screen_styled(&id, lines, true)
            .unwrap();
        let plain = fixture
            .manager
            .capture_screen_styled(&id, lines, false)
            .unwrap();
        assert_eq!(without_sgr(&styled.output), plain.output, "--lines {lines}");
        assert_eq!(styled.screen, plain.screen);
        assert!(styled.output.lines().count() >= 6);
    }
    // Styled or not, a missing shell is an error.
    assert!(
        fixture
            .manager
            .capture_screen_styled(&Uuid::new_v4().to_string(), 10, true)
            .is_err()
    );
}

#[test]
#[ignore = "slow: process-backed tmux integration"]
fn styled_capture_falls_back_to_a_filtered_plain_capture_when_the_report_fails() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    let id = fixture.pane(30, 4, "printf '\\033[31mred\\033[0m'; exec sleep 60");
    fixture.wait_for_screen(&id, |screen| screen.contains("red"));
    let tmux = fixture.manager.tmux.clone();
    let picky = fixture.root.join("picky-tmux");
    Fixture::script(
        &picky,
        &format!(
            "case \"$*\" in *display-message*capture-pane*|*capture-pane*display-message*) echo 'no lists' >&2; exit 1;; esac; exec {} \"$@\"",
            quote_arg(&tmux.to_string_lossy()),
        ),
    );
    let manager = SessionManager {
        inherit_parent: false,
        user_creation: true,
        home: fixture.root.clone(),
        tmux: picky,
        socket_name: fixture.manager.socket_name.clone(),
    };
    let capture = manager.capture_screen_styled(&id, 10, true).unwrap();
    assert_eq!(capture.screen, None);
    assert!(
        capture.output.starts_with("\u{1b}[31mred"),
        "{:?}",
        capture.output
    );
    assert_eq!(without_sgr(&capture.output), "red\n\n\n\n");
}

/// Polls `read_output` on a real pane: one that prints `first`, waits for a
/// file named `go` in its directory, then prints `second`.
#[test]
#[ignore = "slow: process-backed tmux integration"]
fn read_output_waits_for_a_real_change_and_reports_unchanged_on_timeout() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    let id = fixture.pane(
        30,
        4,
        "printf first; while [ ! -f go ]; do sleep 0.05; done; printf second; exec sleep 60",
    );
    fixture.wait_for_screen(&id, |screen| screen.contains("first"));
    let ask = |if_changed: Option<&str>, wait_ms: u64, styled: bool| {
        let started = Instant::now();
        let read = fixture
            .manager
            .read_output(
                &id,
                &OutputQuery {
                    lines: 100,
                    styled,
                    if_changed,
                    wait: Duration::from_millis(wait_ms),
                },
            )
            .unwrap();
        (read, started.elapsed())
    };
    let changed = |read: OutputRead| match read {
        OutputRead::Changed { capture, hash } => (capture, hash),
        other => panic!("expected content, got {other:?}"),
    };
    // The hash, `history_size` and `alternate` of an unchanged answer.
    let unchanged = |read: OutputRead| match read {
        OutputRead::Unchanged {
            hash,
            screen: Some(screen),
        } => (hash, screen.history_size, screen.alternate),
        other => panic!("expected unchanged, got {other:?}"),
    };

    // No hash: content and its hash, at once.
    let (read, elapsed) = ask(None, 5_000, false);
    let (first, h1) = changed(read);
    assert_eq!(first.output, "first\n\n\n\n");
    assert!(elapsed < Duration::from_secs(4), "{elapsed:?}");
    // The same question about the same screen has the same hash, and a
    // different question has another one.
    assert_eq!(changed(ask(None, 0, false).0).1, h1);
    let (styled, hs) = changed(ask(None, 0, true).0);
    assert_ne!(hs, h1);
    assert_eq!(styled.output, first.output);
    // A hash that does not match is answered with the content, not a wait.
    let (read, elapsed) = ask(Some("0000000000000000"), 5_000, false);
    assert_eq!(changed(read).1, h1);
    assert!(elapsed < Duration::from_secs(4), "{elapsed:?}");

    // Nothing changes: unchanged, after the wait and not much later.
    let (read, elapsed) = ask(Some(&h1), 500, false);
    assert_eq!(unchanged(read), (h1.clone(), 0, false));
    assert!(elapsed >= Duration::from_millis(500), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
    let (read, elapsed) = ask(Some(&h1), 0, false);
    assert_eq!(unchanged(read), (h1.clone(), 0, false));
    assert!(elapsed < Duration::from_secs(4), "{elapsed:?}");

    // A change ends a long wait as soon as it happens.
    let (read, elapsed) = std::thread::scope(|scope| {
        let waiter = scope.spawn(|| ask(Some(&h1), 10_000, false));
        std::thread::sleep(Duration::from_millis(400));
        fs::write(fixture.root.join("work").join("go"), "").unwrap();
        waiter.join().unwrap()
    });
    let (second, h2) = changed(read);
    assert_eq!(second.output, "firstsecond\n\n\n\n");
    assert_ne!(h2, h1);
    assert!(elapsed >= Duration::from_millis(300), "{elapsed:?}");
    assert!(
        elapsed < Duration::from_secs(6),
        "returned at {elapsed:?}, not at the change"
    );

    // The new screen is stable again, and the cursor and mode are in the hash.
    let (read, _) = ask(Some(&h2), 300, false);
    assert_eq!(unchanged(read), (h2.clone(), 0, false));
    fixture
        .manager
        .tmux_checked(&["copy-mode", "-t", &pane_target(&id)])
        .unwrap();
    let (moded, h3) = changed(ask(Some(&h2), 0, false).0);
    assert_eq!(moded.output, second.output);
    assert!(moded.screen.unwrap().in_mode);
    assert_ne!(h3, h2);
}

#[test]
#[ignore = "slow: process-backed tmux integration"]
fn read_output_ends_a_wait_with_an_error_when_the_shell_goes_away() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    let id = fixture.pane(30, 4, "printf still; exec sleep 60");
    fixture.wait_for_screen(&id, |screen| screen.contains("still"));
    let hash = match fixture
        .manager
        .read_output(
            &id,
            &OutputQuery {
                lines: 10,
                styled: false,
                if_changed: None,
                wait: Duration::ZERO,
            },
        )
        .unwrap()
    {
        OutputRead::Changed { hash, .. } => hash,
        other => panic!("{other:?}"),
    };
    let (result, elapsed) = std::thread::scope(|scope| {
        let waiter = scope.spawn(|| {
            let started = Instant::now();
            let result = fixture.manager.read_output(
                &id,
                &OutputQuery {
                    lines: 10,
                    styled: false,
                    if_changed: Some(&hash),
                    wait: Duration::from_secs(10),
                },
            );
            (result, started.elapsed())
        });
        std::thread::sleep(Duration::from_millis(300));
        fixture.manager.kill_tmux_session(&id).unwrap();
        waiter.join().unwrap()
    });
    let error = result.unwrap_err();
    assert!(
        error.contains("exited") || error.contains("tmux"),
        "{error}"
    );
    assert!(elapsed < Duration::from_secs(6), "{elapsed:?}");
}

// Scrollback pages (`read_history`) and the `history_size` and `alternate`
// fields of `shell output`.

impl Fixture {
    /// Like `pane`, with the 100000 lines of history RiWork gives its panes
    /// (tmux's own default is 2000).
    fn deep_pane(&self, columns: u32, rows: u32, script: &str) -> String {
        let id = Uuid::new_v4().to_string();
        let work = self.root.join("work");
        fs::create_dir_all(&work).unwrap();
        self.manager
            .tmux_checked(&[
                "start-server",
                ";",
                "set-option",
                "-g",
                "history-limit",
                &HISTORY_LINES.to_string(),
                ";",
                "new-session",
                "-d",
                "-s",
                &id,
                "-c",
                &work.to_string_lossy(),
                "-x",
                &columns.to_string(),
                "-y",
                &rows.to_string(),
                &format!("sh -c {}", quote_arg(script)),
            ])
            .unwrap();
        self.registry(vec![shell(&id, None, None)]);
        id
    }

    /// `#{history_size}` as tmux itself says it.
    fn tmux_history_size(&self, id: &str) -> u32 {
        self.pane_format(id, "#{history_size}").parse().unwrap()
    }
}

/// The numbers `first..=last`, one per line, joined by newlines.
fn numbers(first: u32, last: u32) -> String {
    (first..=last)
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

fn page(
    fixture: &Fixture,
    id: &str,
    end: u32,
    lines: u32,
    styled: bool,
) -> Result<HistoryPage, String> {
    fixture.manager.read_history(id, end, lines, styled)
}

#[test]
#[ignore = "slow: process-backed tmux integration"]
fn history_pages_hold_exactly_the_numbered_lines_above_the_screen() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    // 5000 numbered lines and an unterminated "x" through a 10-row pane: the
    // screen is 4992..5000 and the "x", so lines 1..=4991 are history.
    let id = fixture.deep_pane(40, 10, "seq 1 5000; printf x; exec sleep 60");
    fixture.wait_for_screen(&id, |screen| screen.ends_with("\n5000\nx\n"));
    let total = 4991;
    assert_eq!(fixture.tmux_history_size(&id), total);

    // What every page must be, worked out from the numbering alone.
    let expected = |end: u32, lines: u32| -> HistoryPage {
        if end >= total {
            return HistoryPage {
                output: String::new(),
                line_count: 0,
                history_size: total,
                complete: true,
            };
        }
        let last = total - end;
        let first = (last + 1).saturating_sub(lines).max(1);
        HistoryPage {
            output: numbers(first, last),
            line_count: last - first + 1,
            history_size: total,
            complete: first == 1,
        }
    };
    let cases = [
        (0, 1),
        (0, 2),
        (0, 10),
        (0, 1000),
        (1, 1),
        (1, 999),
        (7, 100),
        (100, 1000),
        (1000, 1000),
        (3000, 1000),
        // Ends exactly at the top, and one line short of it.
        (total - 1000, 1000),
        (total - 1001, 1000),
        (total - 999, 1000),
        (total - 5, 5),
        (total - 5, 6),
        (total - 6, 5),
        (total - 2, 1),
        (total - 2, 2),
        (total - 1, 1),
        (total - 1, 1000),
        // At and beyond the top of the history: nothing, and complete.
        (total, 1),
        (total, 1000),
        (total + 1, 10),
        (total + 100_000, 1000),
        (u32::MAX - 1, 1),
        (u32::MAX, 1000),
    ];
    for (end, lines) in cases {
        let got = page(&fixture, &id, end, lines, false).unwrap();
        assert_eq!(got, expected(end, lines), "end {end} lines {lines}");
    }
    // Spot checks against the literal numbers, so the table above is not
    // just the code's own arithmetic.
    assert_eq!(
        page(&fixture, &id, 0, 3, false).unwrap().output,
        "4989\n4990\n4991"
    );
    assert_eq!(
        page(&fixture, &id, 4, 2, false).unwrap().output,
        "4986\n4987"
    );
    let top = page(&fixture, &id, total - 3, 100, false).unwrap();
    assert_eq!(
        (top.output.as_str(), top.line_count, top.complete),
        ("1\n2\n3", 3, true)
    );
    // Exactly the lines a screen capture holds above the screen.
    let capture = fixture.manager.capture_screen(&id, 60).unwrap();
    let rows = capture.screen.unwrap().rows as usize;
    let all: Vec<&str> = capture.output.lines().collect();
    assert_eq!(all.len(), 60 + rows);
    assert_eq!(
        all[..60].join("\n"),
        page(&fixture, &id, 0, 60, false).unwrap().output
    );
}

#[test]
#[ignore = "slow: process-backed tmux integration"]
fn paging_upward_from_the_screen_rebuilds_the_history_without_gaps_or_overlaps() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    let id = fixture.deep_pane(40, 10, "seq 1 5000; printf x; exec sleep 60");
    fixture.wait_for_screen(&id, |screen| screen.ends_with("\n5000\nx\n"));
    // One line at a time, as far as it takes to be sure: the newest 50.
    let mut end = 0;
    let mut singles = Vec::new();
    for _ in 0..50 {
        let got = page(&fixture, &id, end, 1, false).unwrap();
        assert_eq!((got.line_count, got.history_size), (1, 4991));
        end += got.line_count;
        singles.push(got.output);
    }
    singles.reverse();
    assert_eq!(singles.join("\n"), numbers(4942, 4991));
    // Whole history, from the screen up to the top, in pages of every size.
    for size in [333, 1000, 999, 250] {
        let mut pages = Vec::new();
        let mut end = 0;
        loop {
            let got = page(&fixture, &id, end, size, false).unwrap();
            assert_eq!(got.history_size, 4991, "size {size}");
            end += got.line_count;
            let done = got.complete;
            pages.push(got);
            if done {
                break;
            }
            assert!(pages.len() < 1000, "never reached the top");
        }
        // Only the last page is complete, and pages are full until then.
        let (last, rest) = pages.split_last().unwrap();
        assert!(rest.iter().all(|p| !p.complete && p.line_count == size));
        assert_eq!(pages.len() as u32, 4991_u32.div_ceil(size), "size {size}");
        assert_eq!(end, 4991);
        // Oldest page first: the numbers are 1..=4991, each exactly once.
        let rebuilt: Vec<&str> = pages
            .iter()
            .rev()
            .flat_map(|p| p.output.split('\n'))
            .collect();
        let wanted: Vec<String> = (1..=4991).map(|n| n.to_string()).collect();
        assert_eq!(rebuilt, wanted, "size {size}");
        assert_eq!(last.output.split('\n').next(), Some("1"));
        // One more page above the top is empty.
        let beyond = page(&fixture, &id, end, size, false).unwrap();
        assert_eq!((beyond.line_count, beyond.complete), (0, true));
        assert_eq!(beyond.output, "");
    }
}

#[test]
#[ignore = "slow: process-backed tmux integration"]
fn history_pages_keep_blank_lines_and_the_count_tells_one_blank_line_from_none() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    // Eleven lines through a 4-row pane: a, two blanks, b, two blanks, 1 to 4
    // and "end"; the screen is 2, 3, 4 and "end", the history the other seven.
    let id = fixture.deep_pane(
        20,
        4,
        "printf 'a\\n\\n\\nb\\n\\n\\n'; seq 1 4; printf end; exec sleep 60",
    );
    fixture.wait_for_screen(&id, |screen| screen.contains("end"));
    assert_eq!(fixture.tmux_history_size(&id), 7);
    let get = |end, lines| page(&fixture, &id, end, lines, false).unwrap();
    let whole = get(0, 7);
    assert_eq!(whole.output, "a\n\n\nb\n\n\n1");
    assert_eq!((whole.line_count, whole.complete), (7, true));
    // Blank lines at the bottom of a page are lines: nothing is trimmed.
    let two_blanks = get(1, 2);
    assert_eq!(two_blanks.output, "\n");
    assert_eq!(two_blanks.line_count, 2);
    assert_eq!(get(2, 3).output, "\nb\n");
    assert_eq!(get(2, 3).line_count, 3);
    // One blank line and no line are both "" but not the same page.
    let one = get(1, 1);
    assert_eq!(
        (one.output.as_str(), one.line_count, one.complete),
        ("", 1, false)
    );
    let none = get(7, 1);
    assert_eq!(
        (none.output.as_str(), none.line_count, none.complete),
        ("", 0, true)
    );
    let top_blank = get(5, 1);
    assert_eq!((top_blank.output.as_str(), top_blank.line_count), ("", 1));
    assert!(!top_blank.complete);
    let first = get(6, 1);
    assert_eq!((first.output.as_str(), first.complete), ("a", true));
    // A page that ends in blank lines, mid-history.
    assert_eq!(get(3, 4).output, "a\n\n\nb");
    assert_eq!(get(1, 4).output, "\nb\n\n");
}

#[test]
#[ignore = "slow: process-backed tmux integration"]
fn history_pages_stay_whole_while_the_pane_is_printing() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    // A producer that outruns the requests: history grows and, past 100000
    // lines, loses its oldest lines between pages. Every page is still a run
    // of consecutive numbers, because it comes with its own history size.
    let id = fixture.deep_pane(40, 10, "seq 1 400000; printf done; exec sleep 60");
    for round in 0..30_u32 {
        let got = page(&fixture, &id, round * 97, 200, false).unwrap();
        let values: Vec<u32> = got
            .output
            .split('\n')
            .filter(|line| !line.is_empty())
            .map(|line| line.parse().expect(&got.output))
            .collect();
        assert_eq!(values.len() as u32, got.line_count);
        assert!(
            values.windows(2).all(|pair| pair[1] == pair[0] + 1),
            "{values:?}"
        );
        assert!(got.history_size <= HISTORY_LINES as u32);
    }
    fixture.wait_for_screen(&id, |screen| screen.contains("done"));
    // Settled: the newest history line is just above the screen.
    let capture = fixture.manager.capture_screen(&id, 1).unwrap();
    let above = page(&fixture, &id, 0, 1, false).unwrap();
    let lines: Vec<&str> = capture.output.lines().collect();
    assert_eq!(lines[0], above.output);
}

#[test]
#[ignore = "slow: process-backed tmux integration"]
fn styled_history_pages_keep_sgr_and_the_plain_pages_lines() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    // 300 colored numbers, each followed by an OSC 8 hyperlink.
    let id = fixture.deep_pane(
        40,
        6,
        "for i in $(seq 1 300); do printf '\\033[1;31m%s\\033[0m \\033]8;;http://x\\033\\\\link\\033]8;;\\033\\\\ \\033[38;2;1;2;3mc\\033[0m\\n' \"$i\"; done; printf done; exec sleep 60",
    );
    fixture.wait_for_screen(&id, |screen| screen.contains("done"));
    let total = fixture.tmux_history_size(&id);
    assert_eq!(total, 301 - 6);
    let raw = fixture
        .manager
        .tmux_text(&[
            "capture-pane",
            "-p",
            "-e",
            "-t",
            &pane_target(&id),
            "-S",
            "-20",
            "-E",
            "-1",
        ])
        .unwrap();
    // The premise: tmux writes more than SGR here.
    assert!(raw.contains("\u{1b}]8;;"), "no OSC in {raw:?}");
    for (end, lines) in [(0, 20), (13, 100), (total - 40, 1000), (total - 1, 5)] {
        let styled = page(&fixture, &id, end, lines, true).unwrap();
        let plain = page(&fixture, &id, end, lines, false).unwrap();
        assert!(
            styled.output.contains("\u{1b}[1m") && styled.output.contains("\u{1b}[31m"),
            "{:?}",
            styled.output
        );
        assert!(styled.output.contains("\u{1b}[38;2;1;2;3m"));
        // Nothing but SGR is escaped, and no control character but the newline.
        let text = without_sgr(&styled.output);
        assert!(text.chars().all(|c| !c.is_control() || c == '\n'));
        // The same lines as the plain page, line for line.
        assert_eq!(text, plain.output, "end {end} lines {lines}");
        assert!(!plain.output.contains('\u{1b}'));
        assert!(plain.output.contains("link"));
        assert_eq!(
            (styled.line_count, styled.history_size, styled.complete),
            (plain.line_count, plain.history_size, plain.complete)
        );
        assert_eq!(
            styled.output.split('\n').count() as u32,
            styled.line_count,
            "{:?}",
            styled.output
        );
    }
    // The styled page beyond the top is as empty as the plain one.
    let beyond = page(&fixture, &id, total, 10, true).unwrap();
    assert_eq!(
        (beyond.output.as_str(), beyond.line_count, beyond.complete),
        ("", 0, true)
    );
}

#[test]
#[ignore = "slow: process-backed tmux integration"]
fn a_styled_page_starts_from_the_default_attributes_so_it_can_be_drawn_alone() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    // Red is switched on once, before the first number, and never off again.
    let id = fixture.deep_pane(
        20,
        4,
        "printf '\\033[31m'; seq 1 60; printf '\\033[0mdone'; exec sleep 60",
    );
    fixture.wait_for_screen(&id, |screen| screen.contains("done"));
    // A page from the middle of the run, not its start: every line, the first
    // included, says what it needs without the lines above it.
    let mid = page(&fixture, &id, 10, 5, true).unwrap();
    assert_eq!(without_sgr(&mid.output), "43\n44\n45\n46\n47", "{mid:?}");
    for line in mid.output.split('\n') {
        assert!(
            line.starts_with("\u{1b}[31m"),
            "{line:?} in {:?}",
            mid.output
        );
    }
    let first = page(&fixture, &id, 10, 1, true).unwrap();
    assert!(first.output.starts_with("\u{1b}[31m"), "{first:?}");
}

#[test]
#[ignore = "slow: process-backed tmux integration"]
fn a_history_page_needs_a_live_shell_and_one_to_a_thousand_lines() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    let id = fixture.deep_pane(30, 4, "seq 1 20; exec sleep 60");
    fixture.wait_for_screen(&id, |screen| screen.contains("20"));
    for lines in [0, 5001, u32::MAX] {
        let error = page(&fixture, &id, 0, lines, false).unwrap_err();
        assert!(error.contains("--lines"), "{error}");
    }
    assert!(page(&fixture, &id, 0, 1, false).is_ok());
    assert!(page(&fixture, &id, 0, 5000, false).is_ok());
    let unknown = Uuid::new_v4().to_string();
    let error = page(&fixture, &unknown, 0, 5, false).unwrap_err();
    assert!(error.starts_with("unknown shell"), "{error}");
    let error = page(&fixture, "not-a-uuid", 0, 5, false).unwrap_err();
    assert!(error.starts_with("invalid UUID"), "{error}");
    // A shell that has exited is refused like `capture`.
    fixture.manager.kill_tmux_session(&id).unwrap();
    let dead = fixture.manager.read_history(&id, 0, 5, false);
    assert!(dead.is_err(), "{dead:?}");
}

#[test]
#[ignore = "slow: process-backed tmux integration"]
fn the_alternate_screen_is_reported_and_the_history_above_it_stays_readable() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    // 30 lines make 25 of history in a 6-row pane (30 lines and the cursor's
    // row, less the screen). A full-screen program then takes over the screen.
    let id = fixture.deep_pane(
        30,
        6,
        "seq 1 30; printf '\\033[?1049h\\033[2;3Hzz'; exec sleep 60",
    );
    fixture.wait_for_screen(&id, |screen| screen.contains("zz"));
    let capture = fixture.manager.capture_screen(&id, 100_000).unwrap();
    let screen = capture.screen.expect("screen");
    assert_eq!((screen.history_size, screen.alternate), (25, true));
    assert_eq!(fixture.pane_format(&id, "#{alternate_on}"), "1");
    let got = page(&fixture, &id, 0, 1000, false).unwrap();
    assert_eq!(got.output, numbers(1, 25));
    assert_eq!((got.history_size, got.complete), (25, true));
    // The JSON-level read says the same.
    match fixture
        .manager
        .read_output(
            &id,
            &OutputQuery {
                lines: 100,
                styled: false,
                if_changed: None,
                wait: Duration::ZERO,
            },
        )
        .unwrap()
    {
        OutputRead::Changed { capture, .. } => {
            let screen = capture.screen.unwrap();
            assert_eq!((screen.history_size, screen.alternate), (25, true));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
#[ignore = "slow: process-backed tmux integration"]
fn alternate_follows_a_real_pager_in_and_out() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    let has_less = Command::new("less")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success());
    if !has_less {
        return;
    }
    // LESS may hold the user's -F or -X, which would keep less off the
    // alternate screen.
    let id = fixture.deep_pane(
        30,
        6,
        "seq 1 100; seq 1 300 | env -u LESS less -+F -+X; printf after; exec sleep 60",
    );
    let read = || {
        fixture
            .manager
            .capture_screen(&id, 100)
            .unwrap()
            .screen
            .unwrap()
    };
    let deadline = Instant::now() + Duration::from_secs(15);
    let inside = loop {
        let screen = read();
        if screen.alternate {
            break screen;
        }
        assert!(Instant::now() < deadline, "less never took the screen");
        std::thread::sleep(Duration::from_millis(25));
    };
    // The 100 lines and the cursor's row, less the 6 rows of the screen.
    assert_eq!(inside.history_size, 95);
    let got = page(&fixture, &id, 0, 5, false).unwrap();
    assert_eq!(got.output, numbers(91, 95));
    // Quitting the pager gives the normal screen back.
    fixture.type_items(&id, &[typed("q")]);
    fixture.wait_for_screen(&id, |screen| screen.contains("after"));
    let outside = read();
    assert!(!outside.alternate);
    assert!(outside.history_size >= inside.history_size);
    assert_eq!(fixture.pane_format(&id, "#{alternate_on}"), "0");
}

#[test]
#[ignore = "slow: process-backed tmux integration"]
fn output_reports_the_history_size_and_its_growth_changes_the_hash() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    // The screen is three x's over the cursor's row. Printing another x
    // scrolls, and looks exactly the same; only the history grows.
    let id = fixture.deep_pane(
        20,
        4,
        "for i in 1 2 3 4 5 6; do echo x; done; while [ ! -f go ]; do sleep 0.05; done; echo x; echo x; exec sleep 60",
    );
    fixture.wait_for_screen(&id, |screen| screen.ends_with("x\nx\nx\n\n"));
    let ask = |if_changed: Option<&str>| {
        fixture
            .manager
            .read_output(
                &id,
                &OutputQuery {
                    lines: 50,
                    styled: false,
                    if_changed,
                    wait: Duration::ZERO,
                },
            )
            .unwrap()
    };
    let OutputRead::Changed {
        capture: before,
        hash: h1,
    } = ask(None)
    else {
        panic!("expected content");
    };
    let screen = before.screen.unwrap();
    assert_eq!((screen.history_size, screen.alternate), (3, false));
    assert_eq!(fixture.tmux_history_size(&id), 3);
    assert_eq!(page(&fixture, &id, 0, 50, false).unwrap().history_size, 3);
    // Unchanged still says how long the history is.
    let OutputRead::Unchanged {
        hash,
        screen: Some(screen),
    } = ask(Some(&h1))
    else {
        panic!("expected unchanged");
    };
    assert_eq!(
        (hash.as_str(), screen.history_size, screen.alternate),
        (h1.as_str(), 3, false)
    );

    fs::write(fixture.root.join("work").join("go"), "").unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let after = loop {
        if let OutputRead::Changed { capture, hash } = ask(Some(&h1)) {
            break (capture, hash);
        }
        assert!(Instant::now() < deadline, "the history never grew");
        std::thread::sleep(Duration::from_millis(25));
    };
    let (after, h2) = after;
    // The history is longer; the lines it shows are not. Both are in the hash.
    assert_eq!(after.screen.unwrap().history_size, 5);
    assert_eq!(fixture.tmux_history_size(&id), 5);
    assert_ne!(h2, h1);
    let screen_text = |capture: &Capture| {
        let lines: Vec<&str> = capture.output.lines().collect();
        lines[lines.len() - 4..].join("\n")
    };
    assert_eq!(screen_text(&after), screen_text(&before));
    let OutputRead::Unchanged {
        hash,
        screen: Some(screen),
    } = ask(Some(&h2))
    else {
        panic!("expected unchanged");
    };
    assert_eq!((hash, screen.history_size), (h2, 5));
}

#[test]
#[ignore = "slow: process-backed tmux integration"]
fn a_full_screen_program_changes_the_hash_and_the_reported_fields() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    let id = fixture.deep_pane(
        30,
        5,
        "seq 1 12; while [ ! -f go ]; do sleep 0.05; done; printf '\\033[?1049h\\033[2J\\033[Halt'; exec sleep 60",
    );
    fixture.wait_for_screen(&id, |screen| screen.contains("12"));
    let ask = |if_changed: Option<&str>| {
        fixture
            .manager
            .read_output(
                &id,
                &OutputQuery {
                    lines: 100,
                    styled: false,
                    if_changed,
                    wait: Duration::ZERO,
                },
            )
            .unwrap()
    };
    let OutputRead::Changed { capture, hash: h1 } = ask(None) else {
        panic!("expected content");
    };
    let screen = capture.screen.unwrap();
    assert_eq!((screen.history_size, screen.alternate), (8, false));
    fs::write(fixture.root.join("work").join("go"), "").unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let (capture, h2) = loop {
        if let OutputRead::Changed { capture, hash } = ask(Some(&h1)) {
            break (capture, hash);
        }
        assert!(Instant::now() < deadline, "the program never started");
        std::thread::sleep(Duration::from_millis(25));
    };
    let screen = capture.screen.unwrap();
    assert_eq!((screen.history_size, screen.alternate), (8, true));
    assert_ne!(h2, h1);
    let OutputRead::Unchanged {
        hash,
        screen: Some(screen),
    } = ask(Some(&h2))
    else {
        panic!("expected unchanged");
    };
    assert_eq!((hash, screen.history_size, screen.alternate), (h2, 8, true));
}

#[test]
fn a_history_page_is_what_the_capture_holds_and_never_more_than_the_history() {
    let page = |text: &str, history, end, lines| history_page(text, history, end, lines);
    let ok = |text: &str, history, end, lines| page(text, history, end, lines).unwrap();
    // A page in the middle, and the newline after its last line is not part of it.
    assert_eq!(
        ok("1\n2\n3\n", 10, 4, 3),
        HistoryPage {
            output: "1\n2\n3".into(),
            line_count: 3,
            history_size: 10,
            complete: false
        }
    );
    // Reaching the top exactly, and going past it, are both complete; the
    // count is what exists.
    assert!(ok("1\n2\n3\n", 10, 7, 3).complete);
    let clamped = ok("1\n2\n", 10, 8, 5);
    assert_eq!(
        (
            clamped.output.as_str(),
            clamped.line_count,
            clamped.complete
        ),
        ("1\n2", 2, true)
    );
    assert!(!ok("2\n", 10, 8, 1).complete);
    // A page above the top: tmux prints line 0 for it, which is not the page.
    for (end, lines) in [(10, 3), (11, 3), (u32::MAX, 1000)] {
        let empty = ok("1\n", 10, end, lines);
        assert_eq!(
            empty,
            HistoryPage {
                output: String::new(),
                line_count: 0,
                history_size: 10,
                complete: true
            }
        );
    }
    let nothing = ok("", 0, 0, 5);
    assert_eq!((nothing.line_count, nothing.complete), (0, true));
    // No overflow at the largest numbers.
    assert!(ok("x\n", 1, 0, 1000).complete);
    assert!(!ok("x\n", u32::MAX, 0, 1).complete);
    // Blank lines are lines, and a tmux that trimmed the last ones gets them back.
    assert_eq!(ok("a\n\n\n", 5, 0, 3).output, "a\n\n");
    assert_eq!(ok("a\n", 5, 0, 3).output, "a\n\n");
    assert_eq!(ok("a", 5, 0, 3).output, "a\n\n");
    assert_eq!(ok("", 5, 0, 2).output, "\n");
    assert_eq!(ok("\n", 5, 0, 1).output, "");
    assert_eq!(ok("", 5, 0, 1).output, "");
    assert_eq!(ok("", 5, 0, 1).line_count, 1);
    // More lines than the page can hold is an error, not a longer page.
    assert!(page("1\n2\n3\n4\n", 10, 0, 3).is_err());
    assert!(page("1\n2\n", 10, 9, 5).is_err());
}

#[cfg(unix)]
#[test]
#[ignore = "slow: fake tmux process for the liveness check"]
fn attach_command_is_the_quoted_attach_argv_and_exec_adds_only_the_flags_asked_for() {
    let id = "00000000-0000-4000-8000-0000000000c1";
    let fixture = Fixture::with_live_sessions(&[id]);
    fixture.registry(vec![shell(id, None, None)]);
    let manager = &fixture.manager;
    let (tmux, socket) = (manager.tmux.to_string_lossy(), &manager.socket_name);

    // The string Ghostty parses is exactly what it always was.
    assert_eq!(
        manager.attach_command(id).unwrap(),
        format!(
            "/usr/bin/env -u TMUX -u TMUX_TMPDIR {} -L {} attach-session -t {id}",
            quote_arg(&tmux),
            quote_arg(socket)
        )
    );
    let argv = |options| manager.attach_argv(id, options).unwrap();
    let plain = argv(AttachOptions::default());
    assert_eq!(
        plain,
        [
            "/usr/bin/env",
            "-u",
            "TMUX",
            "-u",
            "TMUX_TMPDIR",
            &tmux,
            "-L",
            socket,
            "attach-session",
            "-t",
            id
        ]
    );
    // `-r` and `-f ignore-size` go before `-t`, and nothing else changes.
    let tail = |options| argv(options)[8..].to_vec();
    assert_eq!(
        tail(AttachOptions {
            ignore_size: true,
            read_only: false
        }),
        ["attach-session", "-f", "ignore-size", "-t", id]
    );
    assert_eq!(
        tail(AttachOptions {
            ignore_size: false,
            read_only: true
        }),
        ["attach-session", "-r", "-t", id]
    );
    assert_eq!(
        tail(AttachOptions {
            ignore_size: true,
            read_only: true
        }),
        ["attach-session", "-r", "-f", "ignore-size", "-t", id]
    );

    // Run in place it is that argv, announcing a terminal tmux can start.
    let command = manager
        .attach_exec_command(
            id,
            AttachOptions {
                ignore_size: true,
                read_only: true,
            },
        )
        .unwrap();
    assert_eq!(command.get_program(), "/usr/bin/env");
    let args: Vec<_> = command.get_args().map(|a| a.to_string_lossy()).collect();
    assert_eq!(
        args,
        argv(AttachOptions {
            ignore_size: true,
            read_only: true
        })[1..]
    );
    let term = command
        .get_envs()
        .find(|(name, _)| *name == "TERM")
        .and_then(|(_, value)| value)
        .expect("TERM is set");
    assert!(!term.is_empty() && term != "dumb");

    // A shell that is not live is refused before anything is run.
    let gone = "00000000-0000-4000-8000-0000000000c2";
    assert!(
        manager
            .attach_exec_command(gone, AttachOptions::default())
            .is_err()
    );
}

/// tmux honours `TMUX_TMPDIR`, so a terminal that sets it and an app that does
/// not would run two servers. RiWork clears it for every call and for the
/// attach command.
#[cfg(unix)]
#[test]
#[ignore = "slow: real tmux in a re-run of the test binary; guards the two-servers bug"]
fn tmux_ignores_an_inherited_tmux_tmpdir() {
    const NAME: &str = "sessions::tmux_tests::tmux_ignores_an_inherited_tmux_tmpdir";
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    let Some(inherited) = env::var_os("RIWORK_TEST_TMUX_TMPDIR") else {
        // `--include-ignored`: the re-run must not skip this ignored test.
        let output = Command::new(env::current_exe().unwrap())
            .args(["--exact", NAME, "--nocapture", "--include-ignored"])
            .env("RIWORK_TEST_TMUX_TMPDIR", fixture.root.join("elsewhere"))
            .env("TMUX_TMPDIR", fixture.root.join("elsewhere"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    };
    let inherited = PathBuf::from(inherited);
    fs::create_dir_all(&inherited).unwrap();
    assert_eq!(env::var_os("TMUX_TMPDIR"), Some(inherited.clone().into()));
    let id = fixture.recording_session();
    fixture.registry(vec![shell(&id, None, None)]);
    let socket = fixture
        .manager
        .tmux_text(&["display-message", "-p", "#{socket_path}"])
        .unwrap();
    assert!(
        !Path::new(socket.trim()).starts_with(&inherited),
        "{socket} is under {}",
        inherited.display()
    );
    assert_eq!(fs::read_dir(&inherited).unwrap().count(), 0);
    assert!(
        fixture
            .manager
            .attach_command(&id)
            .unwrap()
            .contains(" -u TMUX_TMPDIR ")
    );
}

impl Fixture {
    fn pane(&self, columns: u32, rows: u32, script: &str) -> String {
        let id = Uuid::new_v4().to_string();
        fs::create_dir_all(self.root.join("work")).unwrap();
        self.manager
            .tmux_checked(&[
                "new-session",
                "-d",
                "-s",
                &id,
                "-c",
                &self.root.join("work").to_string_lossy(),
                "-x",
                &columns.to_string(),
                "-y",
                &rows.to_string(),
                &format!("sh -c {}", quote_arg(script)),
            ])
            .unwrap();
        self.registry(vec![shell(&id, None, None)]);
        id
    }
}

impl Fixture {
    fn pane_format(&self, id: &str, format: &str) -> String {
        self.manager
            .tmux_text(&["display-message", "-p", "-t", &pane_target(id), format])
            .unwrap()
            .trim()
            .to_owned()
    }
}

impl Fixture {
    fn wait_for_screen(&self, id: &str, wanted: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let screen = self.manager.capture(id, 100).unwrap();
            if wanted(&screen) {
                return screen;
            }
            assert!(Instant::now() < deadline, "screen never matched:\n{screen}");
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

impl Fixture {
    fn wait_for_command(&self, id: &str, command: &str) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while self.pane_format(id, "#{pane_current_command}") != command {
            assert!(Instant::now() < deadline, "pane never ran {command}");
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}
