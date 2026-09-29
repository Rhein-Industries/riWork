//! Tests that drive tmux itself: terminal input, bounded clients, and the
//! registry behaviours that depend on tmux liveness. Real tmux servers use a
//! private `-L` socket that is killed on drop; without tmux those tests skip.
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

#[test]
fn empty_submission_sends_a_single_return_and_copy_mode_is_refused() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    let id = fixture.recording_session();
    fixture.manager.paste_and_submit(&id, "").unwrap();
    assert_eq!(fixture.wait_for_received(1), b"\r");
    fixture
        .manager
        .tmux_checked(&["copy-mode", "-t", &pane_target(&id)])
        .unwrap();
    let error = fixture
        .manager
        .paste_and_submit(&id, "blocked;")
        .unwrap_err();
    assert!(error.contains("copy mode"), "{error}");
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(fixture.received(), b"\r");
}

#[cfg(unix)]
#[test]
fn bounded_runner_streams_large_input_and_output_without_deadlock() {
    let mut command = Command::new("cat");
    command.env_clear();
    let input = vec![b'x'; 4 * 1024 * 1024];
    let output = run_bounded(command, Some(&input), Duration::from_secs(20), "cat").unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, input);
}

#[cfg(unix)]
#[test]
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

#[cfg(unix)]
#[test]
fn wedged_tmux_fails_a_listing_instead_of_hanging_the_caller() {
    let fixture = Fixture::new(|root| {
        let tmux = root.join("fake-tmux");
        // Teardown's own tmux calls must not wedge as well.
        Fixture::script(
            &tmux,
            "case \"$*\" in *kill-server|*socket_path*) exit 0;; esac; exec sleep 60",
        );
        tmux
    });
    let started = Instant::now();
    let error = fixture
        .manager
        .attach_command(&Uuid::new_v4().to_string())
        .unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(15));
    assert!(error.contains("did not finish"), "{error}");
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

/// A plain shell keeps the Codex label of its last `codex` launch, since the
/// launcher execs Codex and nothing runs afterwards to clear it. A listing
/// reports the label only while something else than the shell owns the pane.
#[cfg(unix)]
#[test]
fn list_hides_the_codex_label_of_a_plain_shell_that_is_back_at_its_prompt() {
    let ids: Vec<String> = (1..=6)
        .map(|n| format!("00000000-0000-4000-8000-0000000000d{n}"))
        .collect();
    let [plain, running, wrapped, tab, claude, other] = &ids[..] else {
        unreachable!()
    };
    let fixture = Fixture::new(|root| {
        let tmux = root.join("fake-tmux");
        let names: String = ids.iter().map(|id| format!("{id}\\n")).collect();
        let panes = root.join("panes");
        Fixture::script(
            &tmux,
            &format!(
                "shift 4\ncase \"$1\" in\n  list-sessions) printf '{names}' ;;\n  show-options) echo /bin/sh ;;\n  list-panes) cat {} || exit 1 ;;\nesac",
                quote_arg(&panes.to_string_lossy())
            ),
        );
        tmux
    });
    let account = fixture.root.join("codex-a");
    let labelled = |id: &str, harness: &str, command: Option<&str>| {
        let mut session = shell(id, Some(harness), None);
        session.command = command.map(str::to_owned);
        session.unrestricted = harness == "codex";
        session.codex_account_id = Some("account-a".into());
        session.codex_account_label = Some("A".into());
        session.codex_account_email = Some("a@example.test".into());
        session.codex_home = Some(account.clone());
        session
    };
    fixture.registry(vec![
        labelled(plain, "codex", None),
        labelled(running, "codex", None),
        labelled(wrapped, "codex", None),
        // A Codex tab or orchestrator is Codex whatever its pane shows.
        labelled(tab, "codex", Some("exec codex")),
        labelled(claude, "claude", None),
        labelled(other, "codex", None),
    ]);
    let panes = |commands: [&str; 6]| {
        let lines: String = ids
            .iter()
            .zip(commands)
            .map(|(id, command)| format!("{id}\t0\t0\t{command}\n"))
            .collect();
        fs::write(fixture.root.join("panes"), lines).unwrap();
    };
    let has_label = |session: &ShellSession| {
        session.harness.is_some()
            || session.codex_home.is_some()
            || session.codex_account_id.is_some()
            || session.codex_account_label.is_some()
            || session.codex_account_email.is_some()
    };
    let listed = |fixture: &Fixture| -> Vec<ShellSession> { fixture.manager.list().unwrap() };

    // The first pane is idle at `sh`; a login shell may carry a leading dash.
    panes(["sh", "codex", "node", "sh", "sh", "vim"]);
    let sessions = listed(&fixture);
    assert!(!has_label(&sessions[0]), "{:?}", sessions[0]);
    assert!(!sessions[0].unrestricted);
    assert!(sessions[0].alive);
    for kept in &sessions[1..] {
        assert!(has_label(kept), "{}", kept.id);
    }
    assert!(sessions[1].unrestricted && sessions[3].unrestricted);
    // Only the listing changes: the saved row and `get` keep the account, so a
    // `codex resume` typed in the idle shell finds it.
    let saved = fixture.manager.read_registry().unwrap().sessions;
    assert_eq!(saved[0].harness, Some(HarnessKind::Codex));
    assert_eq!(saved[0].codex_home.as_ref(), Some(&account));
    let got = fixture.manager.get(plain).unwrap();
    assert_eq!(got.harness, Some(HarnessKind::Codex));
    assert_eq!(got.codex_home.as_ref(), Some(&account));

    // Starting Codex brings the label back; a dashed shell name is still idle.
    panes(["codex", "-sh", "codex", "codex", "codex", "codex"]);
    let sessions = listed(&fixture);
    assert!(has_label(&sessions[0]));
    assert!(!has_label(&sessions[1]));

    // A tmux that cannot answer is not evidence that Codex exited.
    fs::remove_file(fixture.root.join("panes")).unwrap();
    assert!(listed(&fixture).iter().all(has_label));
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

#[test]
fn a_respawned_pane_keeps_the_directory_it_was_in() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    let id = fixture.recording_session();
    for (index, name) in AWKWARD_DIRECTORIES.iter().enumerate() {
        let directory = fixture.root.join("respawn").join(name);
        fs::create_dir_all(&directory).unwrap();
        let out = fixture.root.join(format!("respawned-{index}"));
        let arguments = respawn_arguments(
            &id,
            &directory.to_string_lossy(),
            &fixture.root,
            std::ffi::OsStr::new("/usr/bin:/bin"),
            None,
            &format!(
                "pwd -P > {}; exec sleep 300;",
                quote_arg(&out.to_string_lossy())
            ),
        );
        let borrowed: Vec<&str> = arguments.iter().map(String::as_str).collect();
        fixture.manager.tmux_checked(&borrowed).unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        let printed = loop {
            if let Some(text) = fs::read_to_string(&out)
                .ok()
                .and_then(|text| text.strip_suffix('\n').map(str::to_owned))
            {
                break text;
            }
            assert!(Instant::now() < deadline, "{name:?} never started");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(printed, directory.to_string_lossy(), "{name:?}");
    }
}

#[cfg(unix)]
#[test]
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

#[test]
fn new_panes_keep_a_hundred_thousand_lines_of_history() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    // A server that already exists, with tmux's own default of 2000 lines.
    fixture
        .manager
        .tmux_checked(&["new-session", "-d", "-s", "older", "sleep 300"])
        .unwrap();
    let older = fixture
        .manager
        .tmux_text(&[
            "display-message",
            "-p",
            "-t",
            "older:0.0",
            "#{history_limit}",
        ])
        .unwrap();
    assert_eq!(older.trim(), "2000");

    let session = fixture
        .manager
        .create(
            Uuid::new_v4().to_string(),
            None,
            fixture.root.clone(),
            Some("seq 1 6000; exec sleep 300".into()),
        )
        .unwrap();
    let limit = fixture
        .manager
        .tmux_text(&[
            "display-message",
            "-p",
            "-t",
            &pane_target(&session.id),
            "#{history_limit}",
        ])
        .unwrap();
    assert_eq!(limit.trim(), HISTORY_LINES.to_string());
    let deadline = Instant::now() + Duration::from_secs(15);
    let captured = loop {
        let text = fixture.manager.capture(&session.id, HISTORY_LINES).unwrap();
        if text.lines().any(|line| line == "6000") || Instant::now() >= deadline {
            break text;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let numbers: Vec<&str> = captured.lines().filter(|line| !line.is_empty()).collect();
    assert_eq!(numbers.len(), 6000, "kept {} of 6000 lines", numbers.len());
    assert_eq!(numbers.first(), Some(&"1"));
}

#[test]
fn stale_server_environment_does_not_reach_a_new_pane() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    // A server started from another environment, still holding old values.
    fixture
        .manager
        .tmux_checked(&["new-session", "-d", "-s", "older", "sleep 300"])
        .unwrap();
    let stale = ["CLAUDE_CONFIG_DIR", "GROK_HOME", "RIWORK_CUA_DRIVER"];
    for variable in stale {
        fixture
            .manager
            .tmux_checked(&["set-environment", "-g", variable, "/stale/value"])
            .unwrap();
    }
    let out = fixture.root.join("environment");
    fixture
        .manager
        .create(
            Uuid::new_v4().to_string(),
            None,
            fixture.root.clone(),
            Some(format!(
                "env > {}; exec sleep 300",
                quote_arg(&out.to_string_lossy())
            )),
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let environment = loop {
        if let Ok(text) = fs::read_to_string(&out)
            && text.lines().any(|line| line.starts_with("PATH="))
        {
            break text;
        }
        assert!(
            Instant::now() < deadline,
            "the pane never printed its environment"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let inherited = |variable: &str| {
        if variable == "RIWORK_CUA_DRIVER" {
            return crate::cua::driver_override_for_harness()
                .map(|path| path.to_string_lossy().into_owned());
        }
        env::var_os(variable).map(|value| value.to_string_lossy().into_owned())
    };
    for variable in stale {
        let in_pane = environment
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{variable}=")));
        assert_eq!(
            in_pane.map(str::to_owned),
            inherited(variable),
            "{variable}"
        );
        // The server no longer offers the old value to later panes either.
        let global = fixture
            .manager
            .tmux_text(&["show-environment", "-g", variable])
            .unwrap_or_default();
        if inherited(variable).is_none() {
            assert!(!global.contains("/stale/value"), "{variable}: {global}");
        }
    }
}

/// tmux honours `TMUX_TMPDIR`, so a terminal that sets it and an app that does
/// not would run two servers. RiWork clears it for every call and for the
/// attach command.
#[cfg(unix)]
#[test]
fn tmux_ignores_an_inherited_tmux_tmpdir() {
    const NAME: &str = "sessions::tmux_tests::tmux_ignores_an_inherited_tmux_tmpdir";
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    let Some(inherited) = env::var_os("RIWORK_TEST_TMUX_TMPDIR") else {
        let output = Command::new(env::current_exe().unwrap())
            .args(["--exact", NAME, "--nocapture"])
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

#[test]
fn no_server_at_all_means_no_sessions() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    // The socket does not exist yet.
    assert!(fixture.manager.live_session_names().unwrap().is_empty());
    assert!(
        !fixture
            .manager
            .is_alive(&Uuid::new_v4().to_string())
            .unwrap()
    );
}

#[cfg(unix)]
#[test]
fn a_connection_that_is_not_a_missing_server_is_an_error() {
    let fixture = Fixture::new(|root| {
        let tmux = root.join("fake-tmux");
        Fixture::script(
            &tmux,
            "echo 'error connecting to /a/long/path (File name too long)' >&2; exit 1",
        );
        tmux
    });
    let error = fixture.manager.live_session_names().unwrap_err();
    assert!(error.contains("File name too long"), "{error}");
}

#[cfg(unix)]
#[test]
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

/// Every window refreshes through `sample`. However many ask, tmux is queried
/// once per tick, and a change this process makes is visible at once.
#[cfg(unix)]
#[test]
fn windows_share_one_tmux_sample_until_this_process_changes_a_session() {
    let id = "00000000-0000-4000-8000-0000000000e1";
    let mut calls = PathBuf::new();
    let fixture = Fixture::new(|root| {
        calls = root.join("calls");
        let tmux = root.join("fake-tmux");
        Fixture::script(
            &tmux,
            &format!(
                "printf '%s\\n' \"$*\" >> {calls}\n\
                 case \"$*\" in\n\
                 *list-sessions*) printf '{id}\\n' ;;\n\
                 *pane_pid*) printf '{id}\\t1\\n' ;;\n\
                 *pane_current_path*) printf '{id}\\t0\\t0\\t/work\\n' ;;\n\
                 esac",
                calls = quote_arg(&calls.to_string_lossy()),
            ),
        );
        tmux
    });
    fixture.registry(vec![shell(id, None, None)]);
    let tmux_calls = || {
        fs::read_to_string(&calls)
            .unwrap_or_default()
            .lines()
            .count()
    };
    let baseline = tmux_calls();

    for _ in 0..7 {
        let sample = fixture.manager.sample(true).unwrap();
        assert_eq!(sample.shells.len(), 1);
        assert!(sample.shells[0].alive);
        assert_eq!(
            sample.directories.unwrap().get(id),
            Some(&PathBuf::from("/work"))
        );
        assert!(sample.metrics.is_some());
    }
    // list-sessions, then the pane directories and the pane pids.
    assert_eq!(tmux_calls() - baseline, 3);

    // Creating, closing and attaching all go through these.
    fixture.registry(vec![shell(id, None, None)]);
    fixture.manager.sample(true).unwrap();
    assert_eq!(tmux_calls() - baseline, 6);
    fixture.manager.kill_tmux_session(id).unwrap();
    fixture.manager.sample(true).unwrap();
    assert_eq!(tmux_calls() - baseline, 10);
    fixture.manager.attach_command(id).unwrap();
    fixture.manager.sample(true).unwrap();
    let after_attach = tmux_calls();
    fixture.manager.sample(true).unwrap();
    assert_eq!(tmux_calls(), after_attach);

    // The CLI and MCP read tmux directly and never see a cached answer.
    let before = tmux_calls();
    fixture.manager.list().unwrap();
    fixture.manager.list().unwrap();
    assert_eq!(tmux_calls() - before, 2);

    // What each window used to run every tick, for comparison: four tmux
    // clients (and a `ps`) per window, where the process now runs three in all.
    let before = tmux_calls();
    let shells = fixture.manager.list().unwrap();
    fixture.manager.metrics_snapshot().unwrap();
    fixture.manager.current_directories(&shells).unwrap();
    assert_eq!(tmux_calls() - before, 4);
}
