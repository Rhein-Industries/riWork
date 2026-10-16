//! Account creation uses private fake metadata, drivers and tmux only. Each test
//! re-executes in its own environment so profile selection cannot race other tests.
use super::*;
use crate::chat::{
    driver::DriverConfig,
    host::Providers,
    model::{ChatInfo, ChatState},
    testing::*,
};
use serde_json::Value;
use std::{fs, os::unix::fs::PermissionsExt};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        Self(
            env::var_os("RIWORK_TEST_CREATE_ACCOUNTS")
                .map(PathBuf::from)
                .unwrap_or_else(private_socket_fixture_home),
        )
    }

    fn child(&self, name: &str) -> bool {
        if env::var_os("RIWORK_TEST_CREATE_ACCOUNTS").is_some() {
            return true;
        }
        for directory in ["bin", "user/.codex", "state", "orca", "tmux"] {
            fs::create_dir_all(self.0.join(directory)).unwrap();
        }
        let script = |name: &str, body: &str| {
            let path = self.0.join("bin").join(name);
            fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        };
        script(
            "tmux",
            &format!("printf '%s\\n' \"$@\" >> '{}/tmux-argv'", self.0.display()),
        );
        script(
            "codex",
            &format!(
                "printf '%s\\n' \"$CODEX_HOME\" \"$@\" > '{}/codex-resume'",
                self.0.display()
            ),
        );
        script("cua-driver", "exit 0");
        // An accidental discovery must never reach the installed account CLI.
        script("orca", "exit 91");
        let output = Command::new(env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("cli::account_tests::{name}"),
                "--nocapture",
            ])
            .env("RIWORK_TEST_CREATE_ACCOUNTS", &self.0)
            .env("RIWORK_HOME", self.0.join("state"))
            .env("HOME", self.0.join("user"))
            .env("ORCA_USER_DATA_PATH", self.0.join("orca"))
            .env("ORCA_CLI_COMMAND", self.0.join("bin/orca"))
            .env("RIWORK_CUA_DRIVER", self.0.join("bin/cua-driver"))
            .env("TMUX_TMPDIR", self.0.join("tmux"))
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.0.join("bin").display()),
            )
            .env_remove("CODEX_HOME")
            .env_remove("RIWORK_CODEX_ACCOUNT_HOME")
            .env_remove("RIWORK_CHAT_ID")
            .env_remove("RIWORK_SHELL_ID")
            .env_remove("RIWORK_ORCHESTRATOR_SCOPE")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        false
    }

    fn accounts(&self, home: &Path) {
        let profile = self.0.join("orca");
        for id in ["account-a", "account-b"] {
            fs::create_dir_all(profile.join("codex-accounts").join(id).join("home")).unwrap();
        }
        let cache = home.join("codex-accounts.json");
        fs::write(
            &cache,
            json!({
                "version": 1, "user_data": profile,
                "accounts": [
                    {"id":"account-a", "email":"a@example.test", "workspaceLabel":"Team A", "managedHomeRuntime":"host"},
                    {"id":"account-b", "email":"b@example.test", "workspaceLabel":"Team B", "managedHomeRuntime":"host"}
                ],
                "source_active_id": "account-a"
            }).to_string(),
        ).unwrap();
        fs::set_permissions(cache, fs::Permissions::from_mode(0o600)).unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn words(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

#[test]
fn chat_account_flag_rejects_invalid_selections_before_starting_a_host() {
    let fixture = Fixture::new();
    if !fixture.child("chat_account_flag_rejects_invalid_selections_before_starting_a_host") {
        return;
    }
    let home = fixture.0.join("state");
    fixture.accounts(&home);
    for (provider, options, expected) in invalid_accounts(false) {
        let mut args = words(&["new", "--provider", provider]);
        args.extend(options);
        let error =
            chat_client_command(&home, args, true, &|_| panic!("host must not start")).unwrap_err();
        assert!(error.contains(expected), "{error}");
    }
    assert!(!home.join("chats").exists());
    assert!(!home.join("state.json").exists());
    assert!(!fixture.0.join("tmux-argv").exists());
}

fn invalid_accounts(shell: bool) -> Vec<(&'static str, Vec<String>, &'static str)> {
    let mut cases = vec![
        (
            "codex",
            words(&["--account", "missing"]),
            "No Codex account matches 'missing'",
        ),
        (
            "codex",
            words(&["--account=Team"]),
            "More than one Codex account matches 'Team'",
        ),
        ("codex", words(&["--account"]), "--account needs a value"),
        (
            "codex",
            words(&["--account=a", "--account=b"]),
            "--account can only be given once",
        ),
        (
            "codex",
            words(&["--account=  "]),
            "--account must be an account label or id",
        ),
        (
            "codex",
            words(&["--account=a\tb"]),
            "--account must be an account label or id",
        ),
        (
            "codex",
            vec![format!("--account={}", "a".repeat(201))],
            "at most 200 characters",
        ),
    ];
    for provider in if shell {
        vec!["claude", "muse", "grok"]
    } else {
        vec!["claude", "muse"]
    } {
        cases.push((
            provider,
            words(&["--account", "missing"]),
            "Leave out --account; only Codex accounts can be chosen.",
        ));
    }
    cases
}

fn account_config(
    home: &Path,
    info: &ChatInfo,
    resume: Option<String>,
) -> Result<DriverConfig, String> {
    let mut config = (fake_providers().config)(home, info, resume)?;
    let binding =
        crate::codex_accounts::resolve_launch_binding(home, info.codex_account_id.as_deref())?;
    config
        .env
        .push(("CODEX_HOME".into(), binding.home.into_os_string()));
    Ok(config)
}

#[test]
fn chat_account_flag_pins_saved_and_system_accounts_across_host_restart() {
    let fixture = Fixture::new();
    if !fixture.child("chat_account_flag_pins_saved_and_system_accounts_across_host_restart") {
        return;
    }
    let mut host = TestHost::with_providers(
        quick_options(),
        Providers {
            account: crate::chat::launch::account_for,
            config: account_config,
            ..fake_providers()
        },
    );
    fixture.accounts(&host.home);
    let store = Store::open(&host.home).unwrap();
    let project = store.add_project(host.work(), Some("demo")).unwrap();
    let fake = fake_for(&project.root);
    let run = |home: &Path, args: &[&str]| -> Value {
        serde_json::from_str(
            &chat_client_command(home, words(args), true, &|home| {
                Ok(crate::chat::client::socket_path(home))
            })
            .unwrap(),
        )
        .unwrap()
    };
    for (query, expected) in [
        (" Team B ", Some("account-b")),
        ("account-b", Some("account-b")),
        ("system-default", None),
    ] {
        store
            .set_project_codex_account(
                &project.id,
                crate::store::ProjectCodexAccount::Saved("account-a".into()),
            )
            .unwrap();
        let created = run(
            &host.home,
            &[
                "new",
                "--provider",
                "codex",
                "--project",
                "demo",
                "--account",
                query,
            ],
        );
        assert_eq!(created["codex_account_id"].as_str(), expected);
        let id = created["id"].as_str().unwrap();
        host.wait_for_state(id, |state| *state == ChatState::Idle);
        let expected_home = crate::codex_accounts::resolve_launch_binding(&host.home, expected)
            .unwrap()
            .home;
        assert_eq!(
            fake.starts
                .lock()
                .unwrap()
                .last()
                .unwrap()
                .env
                .iter()
                .find(|(key, _)| key == "CODEX_HOME")
                .unwrap()
                .1,
            expected_home.as_os_str()
        );
        let listed = run(&host.home, &["list"]);
        assert_eq!(
            listed
                .as_array()
                .unwrap()
                .iter()
                .find(|chat| chat["id"] == id)
                .unwrap()["codex_account_id"]
                .as_str(),
            expected
        );
        host.client().close(id).unwrap();
        store
            .set_project_codex_account(
                &project.id,
                crate::store::ProjectCodexAccount::Saved("account-b".into()),
            )
            .unwrap();
        host.restart(quick_options());
        run(
            &host.home,
            &["send", id, "Resume on the pinned subscription"],
        );
        host.wait_for_state(id, |state| *state == ChatState::Idle);
        let starts = fake.starts.lock().unwrap().clone();
        let resumed = starts.last().unwrap();
        assert!(resumed.resume.is_some());
        assert_eq!(
            resumed
                .env
                .iter()
                .find(|(key, _)| key == "CODEX_HOME")
                .unwrap()
                .1,
            expected_home.as_os_str()
        );
        assert_eq!(
            crate::chat::log::read_infos(&host.home)
                .iter()
                .find(|chat| chat.id == id)
                .unwrap()
                .codex_account_id
                .as_deref(),
            expected
        );
    }
}

/// A host using only the account field already understood before this flag. It
/// deliberately has no capabilities exchange or new wire fields.
fn legacy_create(
    fixture: &Fixture,
    query: &str,
    made: Option<&str>,
    deleted: bool,
) -> Result<String, String> {
    use std::{
        io::{BufRead, BufReader},
        os::unix::net::UnixListener,
        thread,
    };
    let home = fixture.0.join("state");
    fixture.accounts(&home);
    let store = Store::open(&home).unwrap();
    if store.snapshot().unwrap().projects.is_empty() {
        let work = fixture.0.join("work");
        fs::create_dir_all(&work).unwrap();
        store.add_project(work, Some("demo")).unwrap();
    }
    let socket = fixture.0.join("legacy.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let made = made.map(str::to_owned);
    let wanted = if query == "system-default" {
        query
    } else {
        "account-b"
    }
    .to_owned();
    let server = thread::spawn(move || {
        let mut stream = bounded_fixture_accept(&listener);
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let request: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["op"], "create");
        #[derive(serde::Deserialize)]
        struct LegacyNewChat {
            codex_account_id: Option<String>,
        }
        let chat: LegacyNewChat = serde_json::from_value(request["chat"].clone()).unwrap();
        assert_eq!(chat.codex_account_id.as_deref(), Some(wanted.as_str()));
        let info = json!({"id":"legacy-chat", "provider":"codex", "cwd":request["chat"]["cwd"], "title":"Codex chat", "created_at_unix":1, "codex_account_id":made, "state":{"state":"idle"}});
        writeln!(
            stream,
            "{}",
            json!({"id":request["id"], "ok":true, "result":info})
        )
        .unwrap();
        line.clear();
        if deleted {
            reader.read_line(&mut line).unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["op"], "delete");
            assert_eq!(request["chat_id"], "legacy-chat");
            writeln!(stream, "{}", json!({"id":request["id"], "ok":true})).unwrap();
        }
    });
    let result = chat_client_command(
        &home,
        words(&[
            "new",
            "--provider",
            "codex",
            "--project",
            "demo",
            &format!("--account={query}"),
        ]),
        true,
        &|_| Ok(socket.clone()),
    );
    server.join().unwrap();
    fs::remove_file(socket).unwrap();
    result
}

#[test]
fn chat_account_flag_uses_the_existing_account_field_on_legacy_hosts() {
    let fixture = Fixture::new();
    if !fixture.child("chat_account_flag_uses_the_existing_account_field_on_legacy_hosts") {
        return;
    }
    for (query, expected) in [
        ("Team B", Some("account-b")),
        ("account-b", Some("account-b")),
        ("system-default", None),
    ] {
        let chat: Value =
            serde_json::from_str(&legacy_create(&fixture, query, expected, false).unwrap())
                .unwrap();
        assert_eq!(chat["codex_account_id"].as_str(), expected);
    }
}

#[test]
fn chat_account_flag_refuses_and_removes_a_legacy_hosts_wrong_account() {
    let fixture = Fixture::new();
    if !fixture.child("chat_account_flag_refuses_and_removes_a_legacy_hosts_wrong_account") {
        return;
    }
    for (query, made) in [
        ("Team B", Some("account-a")),
        ("account-b", None),
        ("system-default", Some("account-a")),
    ] {
        let error = legacy_create(&fixture, query, made, true).unwrap_err();
        assert!(error.contains("older build"), "{error}");
        assert!(error.contains("The chat was removed."), "{error}");
    }
}

#[test]
fn shell_account_flag_rejects_invalid_selections_before_launch() {
    let fixture = Fixture::new();
    if !fixture.child("shell_account_flag_rejects_invalid_selections_before_launch") {
        return;
    }
    let home = fixture.0.join("state");
    fixture.accounts(&home);
    for (harness, options, expected) in invalid_accounts(true) {
        let mut args = words(&["create", "--harness", harness]);
        args.extend(options);
        let error = shell_command(args, true).unwrap_err();
        assert!(error.contains(expected), "{error}");
    }
    for options in [words(&[]), words(&["--command", "codex"])] {
        let mut args = words(&["create", "--account", "Team B"]);
        args.extend(options);
        assert!(
            shell_command(args, true)
                .unwrap_err()
                .contains("--account requires --harness codex")
        );
    }
    assert!(!home.join("sessions.json").exists());
    assert!(!fixture.0.join("tmux-argv").exists());
}

#[test]
fn shell_account_flag_records_an_explicit_binding_with_or_without_a_model() {
    if env::var_os("RIWORK_TEST_ACCOUNT_RESUME").is_some() {
        crate::sessions::run_cua_harness(HarnessKind::Codex, &words(&["resume", "thread-pinned"]))
            .unwrap();
        unreachable!("the fake Codex replaces this process");
    }
    let fixture = Fixture::new();
    if !fixture.child("shell_account_flag_records_an_explicit_binding_with_or_without_a_model") {
        return;
    }
    let home = fixture.0.join("state");
    fixture.accounts(&home);
    let work = fixture.0.join("work");
    fs::create_dir_all(&work).unwrap();
    let store = Store::open(&home).unwrap();
    let project = store.add_project(&work, Some("demo")).unwrap();
    for (query, expected, model) in [
        ("Team B", Some("account-b"), false),
        ("account-b", Some("account-b"), true),
        ("system-default", None, false),
    ] {
        store
            .set_project_codex_account(
                &project.id,
                crate::store::ProjectCodexAccount::Saved("account-a".into()),
            )
            .unwrap();
        let mut args = words(&[
            "create",
            "--project",
            "demo",
            "--harness",
            "codex",
            &format!("--account={query}"),
        ]);
        if model {
            args.extend(words(&["--model", "gpt-6.1-sol", "--unrestricted"]));
        }
        shell_command(args, true).unwrap();
        let registry: Value =
            serde_json::from_slice(&fs::read(home.join("sessions.json")).unwrap()).unwrap();
        let shell = registry["sessions"].as_array().unwrap().last().unwrap();
        assert_eq!(shell["codex_account_id"].as_str(), expected);
        let binding = crate::codex_accounts::resolve_launch_binding(&home, expected).unwrap();
        assert_eq!(shell["codex_home"].as_str(), binding.home.to_str());
        assert_eq!(
            shell["codex_account_label"].as_str(),
            binding.label.as_deref()
        );
        assert_eq!(
            shell["codex_account_email"].as_str(),
            binding.email.as_deref()
        );
        let launched = fs::read_to_string(fixture.0.join("tmux-argv")).unwrap();
        for name in ["CODEX_HOME", "RIWORK_CODEX_ACCOUNT_HOME"] {
            assert!(
                launched
                    .lines()
                    .any(|line| line == format!("{name}={}", binding.home.display())),
                "{launched}"
            );
        }
        fs::remove_file(fixture.0.join("tmux-argv")).unwrap();
        assert_eq!(shell["unrestricted"], model);
        if model {
            assert!(
                shell["command"]
                    .as_str()
                    .unwrap()
                    .contains("-m gpt-6.1-sol")
            );
        }
        store
            .set_project_codex_account(
                &project.id,
                crate::store::ProjectCodexAccount::Saved("account-b".into()),
            )
            .unwrap();
        let saved = SessionManager::at(home.clone())
            .unwrap()
            .registered_session(shell["id"].as_str().unwrap())
            .unwrap();
        assert_eq!(saved.codex_account_id.as_deref(), expected);
        assert_eq!(saved.codex_home, Some(binding.home));
        let resumed = Command::new(env::current_exe().unwrap())
            .args(["--exact", "cli::account_tests::shell_account_flag_records_an_explicit_binding_with_or_without_a_model"])
            .env("RIWORK_TEST_ACCOUNT_RESUME", "1")
            .env("RIWORK_SHELL_ID", &saved.id)
            .env_remove("CODEX_HOME")
            .env_remove("RIWORK_CODEX_ACCOUNT_HOME")
            .output().unwrap();
        assert!(
            resumed.status.success(),
            "{}",
            String::from_utf8_lossy(&resumed.stderr)
        );
        let recorded = fs::read_to_string(fixture.0.join("codex-resume")).unwrap();
        let lines: Vec<_> = recorded.lines().collect();
        assert_eq!(lines[0], saved.codex_home.unwrap().to_str().unwrap());
        assert!(
            lines
                .windows(2)
                .any(|pair| pair == ["resume", "thread-pinned"]),
            "{recorded}"
        );
    }
}
