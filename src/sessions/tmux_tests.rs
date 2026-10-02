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
                 *pane_pid*) printf '{id}\\t0\\t0\\t1\\tzsh\\t/work\\n' ;;\n\
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
    // One `list-panes -a` answers the sessions, directories and pane processes.
    let log = fs::read_to_string(&calls).unwrap();
    assert_eq!(tmux_calls() - baseline, 1, "{log}");
    assert!(log.contains("list-panes -a"), "{log}");

    // Creating, closing and attaching all go through these.
    fixture.registry(vec![shell(id, None, None)]);
    fixture.manager.sample(true).unwrap();
    assert_eq!(tmux_calls() - baseline, 2);
    fixture.manager.kill_tmux_session(id).unwrap();
    fixture.manager.sample(true).unwrap();
    assert_eq!(tmux_calls() - baseline, 4);
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

    // What each window used to run every tick, for comparison: the session
    // list, the pane processes and the pane directories were three tmux clients
    // (and a `ps`), four with a Codex-labelled shell, and the process ran them
    // once per tick however many windows asked. The sample is one now.
    let before = tmux_calls();
    let shells = fixture.manager.list().unwrap();
    fixture.manager.metrics_snapshot().unwrap();
    assert_eq!(tmux_calls() - before, 3, "{shells:?}");
}

/// A shell labelled as Codex whose pane is back at the prompt loses the label
/// in a sample, from the same single tmux query.
#[cfg(unix)]
#[test]
fn a_sample_hides_an_exited_codex_label_without_more_tmux_clients() {
    let id = "00000000-0000-4000-8000-0000000000e2";
    let busy = "00000000-0000-4000-8000-0000000000e3";
    let mut calls = PathBuf::new();
    let fixture = Fixture::new(|root| {
        calls = root.join("calls");
        let tmux = root.join("fake-tmux");
        Fixture::script(
            &tmux,
            &format!(
                "printf '%s\\n' \"$*\" >> {calls}\n\
                 case \"$*\" in\n\
                 *default-shell*) printf '/bin/zsh\\n' ;;\n\
                 *pane_pid*) printf '{id}\\t0\\t0\\t1\\t-zsh\\t/work\\n{busy}\\t0\\t0\\t2\\tcodex\\t/busy\\n' ;;\n\
                 esac",
                calls = quote_arg(&calls.to_string_lossy()),
            ),
        );
        tmux
    });
    fixture.registry(vec![
        shell(id, Some("codex"), None),
        shell(busy, Some("codex"), None),
    ]);
    let tmux_calls = || {
        fs::read_to_string(&calls)
            .unwrap_or_default()
            .lines()
            .count()
    };
    let baseline = tmux_calls();
    let sample = fixture.manager.sample(false).unwrap();
    let harness = |id: &str| {
        sample
            .shells
            .iter()
            .find(|shell| shell.id == id)
            .and_then(|shell| shell.harness)
    };
    assert_eq!(harness(id), None, "at its prompt the label is hidden");
    assert!(harness(busy).is_some(), "a running Codex keeps it");
    // The pane table, and the default shell the first time it is needed.
    assert_eq!(tmux_calls() - baseline, 2);
}

/// The pane table must say what the separate queries it replaced said: which
/// sessions exist, the process of each pane and the directory of the owned one.
#[cfg(unix)]
#[test]
fn the_pane_table_agrees_with_the_queries_it_replaced() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    let ids = [Uuid::new_v4().to_string(), Uuid::new_v4().to_string()];
    let directories = ["first dir", "second dir", "extra"].map(|name| {
        let directory = fixture.root.join(name);
        fs::create_dir_all(&directory).unwrap();
        directory
    });
    for (id, directory) in ids.iter().zip(&directories) {
        fixture
            .manager
            .tmux_checked(&[
                "new-session",
                "-d",
                "-s",
                id,
                "-c",
                &directory.to_string_lossy(),
                "sleep 600",
            ])
            .unwrap();
    }
    // A second window and a split in the first session: only window 0, pane 0
    // is the shell's, but every pane has a process.
    fixture
        .manager
        .tmux_checked(&[
            "new-window",
            "-t",
            &format!("={}:", ids[0]),
            "-c",
            &directories[2].to_string_lossy(),
            "sleep 600",
        ])
        .unwrap();
    fixture
        .manager
        .tmux_checked(&[
            "split-window",
            "-t",
            &format!("={}:1", ids[0]),
            "-c",
            &directories[2].to_string_lossy(),
            "sleep 600",
        ])
        .unwrap();

    let table = fixture.manager.pane_table().unwrap();
    let live = fixture.manager.live_session_names().unwrap();
    assert_eq!(
        table.sessions(),
        live.iter().map(String::as_str).collect::<HashSet<_>>()
    );
    let wanted: HashSet<&str> = ids.iter().map(String::as_str).collect();
    assert_eq!(
        table.roots(&wanted),
        fixture.manager.pane_roots(&wanted).unwrap()
    );
    let directories = table.directories(&wanted);
    assert_eq!(directories.len(), 2);
    for id in &ids {
        let asked = fixture
            .manager
            .tmux_text(&[
                "display-message",
                "-p",
                "-t",
                &pane_target(id),
                "#{pane_current_path}",
            ])
            .unwrap();
        assert_eq!(
            directories.get(id),
            Some(&PathBuf::from(asked.trim())),
            "{id}"
        );
    }
    // No server: no panes, and not an error.
    let _ = fixture.manager.tmux_command(&["kill-server"]);
    assert!(fixture.manager.pane_table().unwrap().sessions().is_empty());
}

// Direct typing: `send_keys` and the screen geometry of `capture_screen`.

use crate::session_keys::{Item, Key};

impl Fixture {
    /// A registered session running `script` in a `columns` x `rows` pane, in
    /// a directory of its own.
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

    fn recording_pane(&self) -> String {
        let id = self.recording_session();
        self.registry(vec![shell(&id, None, None)]);
        id
    }

    fn pane_format(&self, id: &str, format: &str) -> String {
        self.manager
            .tmux_text(&["display-message", "-p", "-t", &pane_target(id), format])
            .unwrap()
            .trim()
            .to_owned()
    }

    /// The screen once `wanted` accepts it; a failure shows the last screen.
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

    fn wait_for_command(&self, id: &str, command: &str) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while self.pane_format(id, "#{pane_current_command}") != command {
            assert!(Instant::now() < deadline, "pane never ran {command}");
            std::thread::sleep(Duration::from_millis(25));
        }
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
        home: fixture.root.clone(),
        tmux: broken,
        socket_name: fixture.manager.socket_name.clone(),
    };
    let error = manager.send_keys(&id, &items).unwrap_err();
    assert!(error.starts_with("not_sent: "), "{error}");
}

#[test]
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

const VALID_REPORT: &str = "8|30|2|1|0|0|0\n";

#[test]
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
        home: fixture.root.clone(),
        tmux: picky,
        socket_name: fixture.manager.socket_name.clone(),
    };
    let capture = manager.capture_screen(&id, 10).unwrap();
    assert_eq!(capture.screen, None);
    assert_eq!(capture.output, "hi\n\n\n\n");
}

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

// Waiting without polling, and the tmux processes a call costs.

impl Fixture {
    /// A fixture whose tmux is a wrapper around the real one that records the
    /// arguments of every call (one line each) in `calls`, and fails every
    /// control-mode client when `refuse_control` is set.
    fn counting(refuse_control: bool) -> Option<Self> {
        let real = find_tmux()?;
        Some(Self::new(|root| {
            let tmux = root.join("counting-tmux");
            let refuse = if refuse_control {
                "case \"$*\" in *' -C '*) exit 1;; esac;"
            } else {
                ""
            };
            Self::script(
                &tmux,
                &format!(
                    "printf '%s\\n' \"$*\" >> {calls}; {refuse} exec {real} \"$@\"",
                    calls = quote_arg(&root.join("calls").to_string_lossy()),
                    real = quote_arg(&real.to_string_lossy()),
                ),
            );
            tmux
        }))
    }

    /// The tmux calls made since the last `forget_calls`, one per element.
    fn calls(&self) -> Vec<String> {
        fs::read_to_string(self.root.join("calls"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn forget_calls(&self) {
        let _ = fs::remove_file(self.root.join("calls"));
    }

    /// How many recorded calls mention `word`.
    fn calls_with(&self, word: &str) -> usize {
        self.calls().iter().filter(|c| c.contains(word)).count()
    }
}

fn watched_query<'a>(hash: &'a str, wait_ms: u64) -> OutputQuery<'a> {
    OutputQuery {
        lines: 100,
        styled: false,
        if_changed: Some(hash),
        wait: Duration::from_millis(wait_ms),
    }
}

fn first_hash(fixture: &Fixture, id: &str) -> String {
    match fixture
        .manager
        .read_output(
            id,
            &OutputQuery {
                lines: 100,
                styled: false,
                if_changed: None,
                wait: Duration::ZERO,
            },
        )
        .unwrap()
    {
        OutputRead::Changed { hash, .. } => hash,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_call_costs_the_tmux_processes_it_needs_and_no_more() {
    let Some(fixture) = Fixture::counting(false) else {
        return;
    };
    let id = fixture.pane(30, 4, "printf hello; exec sleep 60");
    fixture.wait_for_screen(&id, |screen| screen.contains("hello"));
    // The registry says whether this is a shell; tmux answers once, in the one
    // command list that captures the screen and reports on the pane.
    fixture.forget_calls();
    first_hash(&fixture, &id);
    assert_eq!(fixture.calls().len(), 1, "{:?}", fixture.calls());
    assert_eq!(fixture.calls_with("list-sessions"), 0);
    fixture.forget_calls();
    fixture.manager.read_history(&id, 0, 5, false).unwrap();
    assert_eq!(fixture.calls().len(), 1, "{:?}", fixture.calls());
    // A batch asks for the state of the pane, then types.
    let recording = fixture.recording_pane();
    fixture.forget_calls();
    fixture
        .manager
        .send_keys(&recording, &[typed("a")])
        .unwrap();
    assert_eq!(fixture.calls().len(), 2, "{:?}", fixture.calls());
    assert_eq!(fixture.calls_with("list-sessions"), 0);
}

#[test]
fn a_shell_that_is_gone_is_still_told_apart_from_a_tmux_error() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    let id = fixture.pane(30, 4, "printf up; exec sleep 60");
    fixture.wait_for_screen(&id, |screen| screen.contains("up"));
    let query = OutputQuery {
        lines: 10,
        styled: false,
        if_changed: None,
        wait: Duration::ZERO,
    };
    fixture.manager.kill_tmux_session(&id).unwrap();
    // Registered but gone: the reason is looked up when the capture fails.
    let output = fixture.manager.read_output(&id, &query).unwrap_err();
    assert_eq!(output, format!("shell {id} has exited"));
    let history = fixture.manager.read_history(&id, 0, 5, false).unwrap_err();
    assert_eq!(history, format!("shell {id} has exited"));
    let keys = fixture.manager.send_keys(&id, &[typed("a")]).unwrap_err();
    assert_eq!(keys, format!("not_found: shell {id} has exited"));
    // Not registered at all, or not an id.
    let unknown = Uuid::new_v4().to_string();
    assert_eq!(
        fixture.manager.read_output(&unknown, &query).unwrap_err(),
        format!("unknown shell {unknown}")
    );
    assert_eq!(
        fixture.manager.read_output("nope", &query).unwrap_err(),
        "invalid UUID: nope"
    );
}

#[test]
fn a_waiting_read_watches_the_pane_instead_of_capturing_it_every_80_ms() {
    let Some(fixture) = Fixture::counting(false) else {
        return;
    };
    let id = fixture.pane(
        30,
        4,
        "printf first; while [ ! -f go ]; do sleep 0.05; done; printf second; exec sleep 60",
    );
    fixture.wait_for_screen(&id, |screen| screen.contains("first"));
    let hash = first_hash(&fixture, &id);
    let size = fixture.pane_format(&id, "#{pane_width}x#{pane_height}");
    fixture.forget_calls();
    let (read, elapsed, during) = std::thread::scope(|scope| {
        let waiter = scope.spawn(|| {
            let started = Instant::now();
            let read = fixture
                .manager
                .read_output(&id, &watched_query(&hash, 10_000));
            (read, started.elapsed())
        });
        std::thread::sleep(Duration::from_millis(1200));
        // While it waits: one control client, and it neither resizes nor types.
        let clients = fixture
            .manager
            .tmux_text(&[
                "list-clients",
                "-F",
                "#{client_control_mode}|#{client_flags}",
            ])
            .unwrap();
        let during = (
            clients,
            fixture.pane_format(&id, "#{pane_width}x#{pane_height}"),
            fixture.calls_with("capture-pane"),
            fixture.calls_with(" -C "),
        );
        fs::write(fixture.root.join("work").join("go"), "").unwrap();
        let (read, elapsed) = waiter.join().unwrap();
        (read, elapsed, during)
    });
    // The change ends the wait about when it happens (touched at 1.2 s).
    let OutputRead::Changed { capture, .. } = read.unwrap() else {
        panic!("expected the change");
    };
    assert!(capture.output.starts_with("firstsecond"), "{capture:?}");
    assert!(elapsed >= Duration::from_millis(1100), "{elapsed:?}");
    assert!(
        elapsed < Duration::from_millis(2600),
        "returned at {elapsed:?}, long after the change"
    );
    // A second of silence cost three tmux calls, not one per 80 ms.
    // The capture that found nothing, one client, and the capture right after
    // it attached: no matter how long the pane stays quiet.
    let (clients, size_during, captures_during, clients_started) = during;
    assert_eq!(captures_during, 2, "{:?}", fixture.calls());
    assert_eq!(clients_started, 1, "{:?}", fixture.calls());
    let control: Vec<&str> = clients.lines().filter(|c| c.starts_with("1|")).collect();
    assert_eq!(control.len(), 1, "{clients}");
    assert!(control[0].contains("read-only"), "{clients}");
    assert!(control[0].contains("ignore-size"), "{clients}");
    assert_eq!(size_during, size);
    // And nothing stays attached afterwards.
    let clients = fixture
        .manager
        .tmux_text(&["list-clients", "-F", "#{client_control_mode}"])
        .unwrap();
    assert!(!clients.lines().any(|c| c == "1"), "{clients}");
}

#[test]
fn a_quiet_pane_is_not_captured_while_watched_and_a_wait_times_out_on_time() {
    let Some(fixture) = Fixture::counting(false) else {
        return;
    };
    let id = fixture.pane(30, 4, "printf still; exec sleep 60");
    fixture.wait_for_screen(&id, |screen| screen.contains("still"));
    let hash = first_hash(&fixture, &id);
    fixture.forget_calls();
    let started = Instant::now();
    let read = fixture
        .manager
        .read_output(&id, &watched_query(&hash, 1_500))
        .unwrap();
    let elapsed = started.elapsed();
    assert!(matches!(read, OutputRead::Unchanged { .. }), "{read:?}");
    assert!(elapsed >= Duration::from_millis(1500), "{elapsed:?}");
    assert!(elapsed < Duration::from_millis(5000), "{elapsed:?}");
    // The first capture, the client, the capture right after it attached.
    assert!(
        fixture.calls_with("capture-pane") <= 3,
        "{:?}",
        fixture.calls()
    );
    assert_eq!(fixture.calls_with(" -C "), 1);
}

#[test]
fn a_pane_that_cannot_be_watched_is_polled_as_before() {
    let Some(fixture) = Fixture::counting(true) else {
        return;
    };
    let id = fixture.pane(
        30,
        4,
        "printf first; while [ ! -f go ]; do sleep 0.05; done; printf second; exec sleep 60",
    );
    fixture.wait_for_screen(&id, |screen| screen.contains("first"));
    let hash = first_hash(&fixture, &id);
    fixture.forget_calls();
    let (read, elapsed) = std::thread::scope(|scope| {
        let waiter = scope.spawn(|| {
            let started = Instant::now();
            let read = fixture
                .manager
                .read_output(&id, &watched_query(&hash, 10_000));
            (read, started.elapsed())
        });
        std::thread::sleep(Duration::from_millis(600));
        fs::write(fixture.root.join("work").join("go"), "").unwrap();
        waiter.join().unwrap()
    });
    let OutputRead::Changed { capture, .. } = read.unwrap() else {
        panic!("expected the change");
    };
    assert!(capture.output.starts_with("firstsecond"), "{capture:?}");
    assert!(elapsed < Duration::from_secs(3), "{elapsed:?}");
    // It tried to watch once, then captured about every 80 ms.
    assert_eq!(fixture.calls_with(" -C "), 1, "{:?}", fixture.calls());
    assert!(
        fixture.calls_with("capture-pane") >= 4,
        "{:?}",
        fixture.calls()
    );
}

#[test]
fn a_watched_wait_notices_a_pane_that_ends() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    let id = fixture.pane(30, 4, "printf alive; exec sleep 60");
    fixture.wait_for_screen(&id, |screen| screen.contains("alive"));
    let hash = first_hash(&fixture, &id);
    let (result, elapsed) = std::thread::scope(|scope| {
        let waiter = scope.spawn(|| {
            let started = Instant::now();
            let result = fixture
                .manager
                .read_output(&id, &watched_query(&hash, 10_000));
            (result, started.elapsed())
        });
        std::thread::sleep(Duration::from_millis(700));
        fixture.manager.kill_tmux_session(&id).unwrap();
        waiter.join().unwrap()
    });
    let error = result.unwrap_err();
    assert!(error.contains("exited"), "{error}");
    // The client leaving is the wake-up: not the four seconds of a safety poll.
    assert!(elapsed < Duration::from_millis(3500), "{elapsed:?}");
}

#[test]
fn a_watched_wait_sees_scrollback_cleared_without_any_output_within_the_safety_poll() {
    let Some(fixture) = Fixture::with_tmux() else {
        return;
    };
    let id = fixture.deep_pane(30, 4, "seq 1 30; exec sleep 60");
    fixture.wait_for_screen(&id, |screen| screen.contains("30"));
    let hash = first_hash(&fixture, &id);
    let (read, elapsed) = std::thread::scope(|scope| {
        let waiter = scope.spawn(|| {
            let started = Instant::now();
            let read = fixture
                .manager
                .read_output(&id, &watched_query(&hash, 10_000));
            (read, started.elapsed())
        });
        std::thread::sleep(Duration::from_millis(500));
        // tmux announces nothing to a control client for this.
        fixture
            .manager
            .tmux_checked(&["clear-history", "-t", &pane_target(&id)])
            .unwrap();
        waiter.join().unwrap()
    });
    let OutputRead::Changed { capture, .. } = read.unwrap() else {
        panic!("expected the change");
    };
    assert_eq!(capture.screen.unwrap().history_size, 0);
    assert!(
        elapsed < watch::SAFETY_POLL + Duration::from_secs(3),
        "{elapsed:?}"
    );
}

#[cfg(unix)]
#[test]
fn bounded_runner_returns_the_exit_status_and_both_streams_when_the_child_ends() {
    let mut command = Command::new("sh");
    command.args(["-c", "printf out; printf err >&2; exit 3"]);
    let output = run_bounded(command, None, Duration::from_secs(10), "sh").unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(
        (output.stdout, output.stderr),
        (b"out".to_vec(), b"err".to_vec())
    );
    // A child that is slow but within its time is waited for.
    let mut command = Command::new("sh");
    command.args(["-c", "sleep 0.3; printf late"]);
    let started = Instant::now();
    let output = run_bounded(command, None, Duration::from_secs(10), "sh").unwrap();
    assert_eq!(output.stdout, b"late");
    assert!(started.elapsed() >= Duration::from_millis(300));
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[cfg(unix)]
#[test]
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
