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
