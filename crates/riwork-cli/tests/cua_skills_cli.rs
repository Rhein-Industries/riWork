//! Process-level checks of the `cua-driver` skill through `riwork setup`,
//! `riwork cua setup` and `riwork cua status`. Everything runs in a throwaway
//! user home with a private `RIWORK_HOME`, an Orca profile with two saved Codex
//! accounts, and a fake Cua Driver (`RIWORK_CUA_DRIVER`) whose pack lives in
//! the fixture. The fake logs each `skills` call so a fetch shows up.

#![cfg(unix)]

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::Value;
use uuid::Uuid;

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("riwork-cua-skills-{}", Uuid::new_v4()));
        let home = root.join("home");
        for directory in [".claude", ".codex", ".grok", ".config/muse"] {
            fs::create_dir_all(home.join(directory)).unwrap();
        }
        for directory in ["bin", "tmp", "state"] {
            fs::create_dir_all(root.join(directory)).unwrap();
        }
        let fixture = Self { root };
        fixture.write_driver();
        fixture.save_codex_accounts();
        fixture
    }

    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    fn pack(&self) -> PathBuf {
        self.root.join("home/.cua-driver/skills/cua-driver")
    }

    fn accounts(&self) -> [PathBuf; 2] {
        ["account-a", "account-b"]
            .map(|id| self.root.join("orca/codex-accounts").join(id).join("home"))
    }

    /// Answers RiWork's probes as a ready driver would, prints the pack
    /// location for `skills path`, and writes the pack for `skills install`.
    fn write_driver(&self) {
        let driver = self.root.join("bin/cua-driver");
        let pack = self.pack();
        let log = self.root.join("driver-skills.log");
        fs::write(
            &driver,
            format!(
                r#"#!/bin/sh
case "$1 $2" in
  "--version "*) echo 'cua-driver 0.30.4' ;;
  "status "*) echo 'Cua Driver daemon is running' ;;
  "permissions status") echo '{{"accessibility":true,"screen_recording":true,"screen_recording_capturable":true,"source":{{"attribution":"driver-daemon"}}}}' ;;
  "skills path") echo "$1 $2" >> '{log}'; echo '{pack}' ;;
  "skills install") echo "$1 $2 HOME=$HOME" >> '{log}'; mkdir -p '{pack}' && printf -- '---\nname: cua-driver\ndescription: Drive desktop apps with Cua Driver.\n---\nUse Cua.\n' > '{pack}/SKILL.md' ;;
  *) exit 99 ;;
esac
"#,
                log = log.display(),
                pack = pack.display(),
            ),
        )
        .unwrap();
        fs::set_permissions(&driver, fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn save_codex_accounts(&self) {
        let user_data = self.root.join("orca");
        for home in self.accounts() {
            fs::create_dir_all(home).unwrap();
        }
        let cache = self.root.join("state/codex-accounts.json");
        fs::write(
            &cache,
            serde_json::to_vec(&serde_json::json!({
                "version": 1,
                "user_data": user_data,
                "accounts": [
                    {"id":"account-a","email":"a@example.test","managedHomeRuntime":"host"},
                    {"id":"account-b","email":"b@example.test","managedHomeRuntime":"host"}
                ],
                "source_active_id": "account-a"
            }))
            .unwrap(),
        )
        .unwrap();
        fs::set_permissions(&cache, fs::Permissions::from_mode(0o600)).unwrap();
    }

    fn command(&self, arguments: &[&str], environment: &[(&str, &Path)]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_riwork-cli"));
        command
            .args(arguments)
            .current_dir(&self.root)
            .env_clear()
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.root.join("bin").display()),
            )
            .env("HOME", self.home())
            .env("TMPDIR", self.root.join("tmp"))
            .env("RIWORK_HOME", self.root.join("state"))
            .env("RIWORK_RUNTIME_DIR", self.root.join("tmp"))
            .env("ORCA_USER_DATA_PATH", self.root.join("orca"))
            .env("RIWORK_CUA_DRIVER", self.root.join("bin/cua-driver"));
        for (name, value) in environment {
            command.env(name, value);
        }
        command
    }

    /// Runs RiWork and returns its JSON status.
    fn json(&self, arguments: &[&str], environment: &[(&str, &Path)]) -> Value {
        let output = self
            .command(arguments, environment)
            .arg("--json")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn skill_calls(&self) -> Vec<String> {
        fs::read_to_string(self.root.join("driver-skills.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// Every entry under the user home and the Orca profile.
    fn homes(&self) -> BTreeMap<PathBuf, String> {
        let mut entries = BTreeMap::new();
        let mut pending = vec![self.home(), self.root.join("orca")];
        while let Some(directory) = pending.pop() {
            for entry in fs::read_dir(&directory).unwrap() {
                let path = entry.unwrap().path();
                let metadata = fs::symlink_metadata(&path).unwrap();
                let description = if metadata.file_type().is_symlink() {
                    format!("link {}", fs::read_link(&path).unwrap().display())
                } else if metadata.is_dir() {
                    pending.push(path.clone());
                    "directory".to_owned()
                } else {
                    format!("file {}", fs::read_to_string(&path).unwrap_or_default())
                };
                entries.insert(path, description);
            }
        }
        entries
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn targets(status: &Value) -> Vec<(String, String, PathBuf)> {
    status["skills"]["targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|target| {
            (
                target["name"].as_str().unwrap().to_owned(),
                target["state"].as_str().unwrap().to_owned(),
                PathBuf::from(target["path"].as_str().unwrap()),
            )
        })
        .collect()
}

#[test]
fn status_reports_every_cli_without_writing_and_setup_fetches_then_links() {
    let fixture = Fixture::new();
    let home = fixture.home();
    let before = fixture.homes();
    let status = fixture.json(&["cua", "status"], &[]);
    assert_eq!(status["ready"], true, "{status:#}");
    assert_eq!(
        status["skills"]["pack"],
        fixture.pack().display().to_string()
    );
    assert_eq!(status["skills"]["pack_installed"], false);
    let expected = |state: &str| {
        vec![
            (
                "Claude Code".to_owned(),
                state.to_owned(),
                home.join(".claude/skills/cua-driver"),
            ),
            (
                "Codex".to_owned(),
                state.to_owned(),
                home.join(".agents/skills/cua-driver"),
            ),
            (
                "Grok".to_owned(),
                state.to_owned(),
                home.join(".grok/skills/cua-driver"),
            ),
            (
                "Muse".to_owned(),
                state.to_owned(),
                home.join(".config/muse/skills/cua-driver"),
            ),
        ]
    };
    assert_eq!(targets(&status), expected("pack_missing"));
    // The Codex link serves the system home and both saved Orca accounts.
    let [account_a, account_b] = fixture.accounts();
    let codex = &status["skills"]["targets"][1];
    assert_eq!(
        codex["homes"],
        serde_json::json!([home.join(".codex"), account_a, account_b])
    );
    assert_eq!(fixture.homes(), before, "status wrote into a home");
    assert_eq!(fixture.skill_calls(), ["skills path"]);
    assert!(!fixture.root.join("state/cua/skill-links.json").exists());

    // Setup asks the trusted driver for its pack, then links it everywhere.
    let setup = fixture.json(&["setup"], &[]);
    assert_eq!(targets(&setup), expected("linked"), "{setup:#}");
    assert_eq!(setup["skills"]["pack_installed"], true);
    assert_eq!(
        fixture.skill_calls(),
        [
            "skills path".to_owned(),
            "skills path".to_owned(),
            format!("skills install HOME={}", home.display()),
            "skills path".to_owned(),
        ]
    );
    for (_, _, path) in targets(&setup) {
        assert_eq!(fs::read_link(&path).unwrap(), fixture.pack());
    }
    // Orca's account homes are not written: Codex reads `~/.agents/skills` under them.
    for account in fixture.accounts() {
        assert_eq!(fs::read_dir(account).unwrap().count(), 0);
    }

    // Running it again fetches nothing and changes nothing.
    let linked = fixture.homes();
    let again = fixture.json(&["cua", "setup"], &[]);
    assert_eq!(targets(&again), expected("linked"));
    assert_eq!(fixture.homes(), linked);
    let installs = fixture.skill_calls();
    let installs = installs
        .iter()
        .filter(|call| call.starts_with("skills install"));
    assert_eq!(installs.count(), 1);
    let status = fixture.json(&["cua", "status"], &[]);
    assert_eq!(targets(&status), expected("linked"));
    assert!(
        status["skills"]["message"]
            .as_str()
            .unwrap()
            .contains("running sessions keep the skills they started with")
    );
}

#[test]
fn setup_links_the_relocated_homes_riwork_launches_and_leaves_foreign_paths() {
    let fixture = Fixture::new();
    let home = fixture.home();
    // The pack is already there, as after Cua Driver's own `skills install`,
    // which also linked Claude Code itself.
    fs::create_dir_all(fixture.pack()).unwrap();
    fs::write(
        fixture.pack().join("SKILL.md"),
        "---\nname: cua-driver\ndescription: Drive desktop apps with Cua Driver.\n---\n",
    )
    .unwrap();
    fs::create_dir_all(home.join(".claude/skills")).unwrap();
    symlink(fixture.pack(), home.join(".claude/skills/cua-driver")).unwrap();
    // Grok has someone else's skill under the same name; Muse's is a link elsewhere.
    fs::create_dir_all(home.join(".grok/skills/cua-driver")).unwrap();
    fs::write(home.join(".grok/skills/cua-driver/SKILL.md"), "theirs").unwrap();
    let elsewhere = fixture.root.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::create_dir_all(home.join(".config/muse/skills")).unwrap();
    symlink(&elsewhere, home.join(".config/muse/skills/cua-driver")).unwrap();
    // Profiles a terminal can point RiWork at.
    let claude = fixture.root.join("profiles/claude");
    let grok = fixture.root.join("profiles/grok");
    fs::create_dir_all(&claude).unwrap();
    fs::create_dir_all(&grok).unwrap();
    let relocated: [(&str, &Path); 2] = [("CLAUDE_CONFIG_DIR", &claude), ("GROK_HOME", &grok)];

    let setup = fixture.json(&["cua", "setup"], &relocated);
    assert_eq!(
        targets(&setup),
        [
            (
                "Claude Code".to_owned(),
                "linked".to_owned(),
                home.join(".claude/skills/cua-driver")
            ),
            (
                "Claude Code ($CLAUDE_CONFIG_DIR)".to_owned(),
                "linked".to_owned(),
                claude.join("skills/cua-driver")
            ),
            (
                "Codex".to_owned(),
                "linked".to_owned(),
                home.join(".agents/skills/cua-driver")
            ),
            (
                "Grok".to_owned(),
                "blocked_by_foreign_path".to_owned(),
                home.join(".grok/skills/cua-driver")
            ),
            (
                "Grok ($GROK_HOME)".to_owned(),
                "linked".to_owned(),
                grok.join("skills/cua-driver")
            ),
            (
                "Muse".to_owned(),
                "blocked_by_foreign_path".to_owned(),
                home.join(".config/muse/skills/cua-driver")
            ),
        ],
        "{setup:#}"
    );
    assert!(
        !fixture
            .skill_calls()
            .iter()
            .any(|call| call.starts_with("skills install"))
    );
    assert_eq!(
        fs::read_to_string(home.join(".grok/skills/cua-driver/SKILL.md")).unwrap(),
        "theirs"
    );
    assert_eq!(
        fs::read_link(home.join(".config/muse/skills/cua-driver")).unwrap(),
        elsewhere
    );
    // Claude Code's link was Cua Driver's; RiWork records only the links it made.
    let manifest: Value =
        serde_json::from_slice(&fs::read(fixture.root.join("state/cua/skill-links.json")).unwrap())
            .unwrap();
    let recorded: BTreeSet<&str> = manifest["links"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let expected: Vec<String> = [
        home.join(".agents/skills/cua-driver"),
        claude.join("skills/cua-driver"),
        grok.join("skills/cua-driver"),
    ]
    .iter()
    .map(|path| path.display().to_string())
    .collect();
    assert_eq!(recorded, expected.iter().map(String::as_str).collect());

    let text = fixture
        .command(&["cua", "status"], &relocated)
        .output()
        .unwrap();
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(
        text.contains(&format!("Skill pack: {}", fixture.pack().display())),
        "{text}"
    );
    assert!(text.contains("blocked by foreign path"), "{text}");
    assert!(text.contains("a directory RiWork did not create"), "{text}");
    assert!(text.contains("read under every Codex home"), "{text}");
}

#[test]
fn an_ordinary_harness_launch_leaves_the_skill_directories_alone() {
    let fixture = Fixture::new();
    let fake = fixture.root.join("bin/claude");
    fs::write(&fake, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o700)).unwrap();
    let before = fixture.homes();
    let output = fixture
        .command(&["cua", "harness", "claude", "--", "--version"], &[])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fixture.homes(), before);
    assert!(fixture.skill_calls().is_empty());
}

// Live discovery: the installed CLIs, run against the fixture homes after
// `riwork setup` has linked them. Each CLI only lists what it found; none is
// logged in, and every home and config directory it sees is the fixture's.
// Set `RIWORK_LIVE_CLAUDE`, `RIWORK_LIVE_CODEX`, `RIWORK_LIVE_GROK` or
// `RIWORK_LIVE_MUSE` to choose a binary; otherwise the first one on `PATH`
// that is not RiWork's launcher is used, and a CLI that is not installed is
// reported as skipped.

const SHIM_MARKER: &[u8] = b"# RiWork Cua harness shim";

fn installed(name: &str) -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(format!("RIWORK_LIVE_{}", name.to_uppercase())) {
        return Some(PathBuf::from(path));
    }
    let found = std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|directory| directory.join(name))
        .find(|path| {
            let mut head = [0; 256];
            let read = fs::File::open(path)
                .and_then(|mut file| std::io::Read::read(&mut file, &mut head))
                .unwrap_or(0);
            path.is_file()
                && !head[..read]
                    .windows(SHIM_MARKER.len())
                    .any(|window| window == SHIM_MARKER)
        })?;
    if name != "muse" {
        return Some(found);
    }
    // Muse's launcher updates itself; run the release binary it would exec.
    let mut releases: Vec<PathBuf> = fs::read_dir(found.parent()?)
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("muse-bin-"))
        })
        .collect();
    releases.sort();
    releases.pop()
}

/// The CLI's own process with nothing but the fixture to read.
fn live(fixture: &Fixture, program: &Path, environment: &[(&str, &Path)]) -> Command {
    let directory = fixture.root.join("project");
    fs::create_dir_all(&directory).unwrap();
    let mut command = Command::new(program);
    command
        .current_dir(directory)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", fixture.home())
        .env("TMPDIR", fixture.root.join("tmp"))
        .env("XDG_DATA_HOME", fixture.root.join("xdg/data"))
        .env("XDG_STATE_HOME", fixture.root.join("xdg/state"))
        .env("XDG_CACHE_HOME", fixture.root.join("xdg/cache"))
        .env("MUSE_NO_AUTO_UPDATE", "1")
        .env("DISABLE_AUTOUPDATER", "1")
        .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1");
    for (name, value) in environment {
        command.env(name, value);
    }
    command
}

fn bounded_output(mut command: Command, seconds: u64) -> std::process::Output {
    use std::process::Stdio;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
    while child.try_wait().unwrap().is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "{command:?} did not finish"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    child.wait_with_output().unwrap()
}

/// Claude Code has no command that lists skills, and a prompt would reach the
/// model. Not logged in, it stops before that, after its debug log records
/// where it read user skills and how many it loaded.
fn claude_loads(fixture: &Fixture, claude: &Path, config: &Path) -> String {
    let log = fixture.root.join(format!("claude-{}.log", Uuid::new_v4()));
    let mut command = live(fixture, claude, &[("CLAUDE_CONFIG_DIR", config)]);
    command
        .args(["-p", "x", "--debug", "--debug-file"])
        .arg(&log);
    let output = bounded_output(command, 60);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Not logged in"),
        "a logged-in Claude Code must not run here: {stdout}"
    );
    fs::read_to_string(log).unwrap()
}

/// Codex has no CLI listing; its app server answers `skills/list`.
fn codex_skills(fixture: &Fixture, codex: &Path, codex_home: &Path) -> Vec<Value> {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;
    let mut child = live(fixture, codex, &[("CODEX_HOME", codex_home)])
        .arg("app-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let stdout = child.stdout.take().unwrap();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let cwd = fixture.root.join("project");
    for message in [
        serde_json::json!({"id": 1, "method": "initialize", "params": {"clientInfo": {"name": "riwork-test", "version": "0"}}}),
        serde_json::json!({"method": "initialized"}),
        serde_json::json!({"id": 2, "method": "skills/list", "params": {"cwds": [cwd], "forceReload": true}}),
    ] {
        writeln!(input, "{message}").unwrap();
    }
    let answer = loop {
        let line = receiver
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("codex app-server did not answer skills/list");
        let message: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
        if message["id"] == 2 {
            break message;
        }
    };
    drop(input);
    let _ = child.kill();
    let _ = child.wait();
    answer["result"]["data"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|entry| entry["skills"].as_array().cloned().unwrap_or_default())
        .filter(|skill| skill["name"] == "cua-driver")
        .collect()
}

#[test]
#[ignore = "live: runs the installed Claude Code, Codex, Grok and Muse against fixture homes"]
fn the_installed_clis_find_the_skill_setup_linked() {
    let fixture = Fixture::new();
    let home = fixture.home();
    let claude_profile = fixture.root.join("profiles/claude");
    let grok_profile = fixture.root.join("profiles/grok");
    let config = fixture.root.join("profiles/config");
    for directory in [&claude_profile, &grok_profile, &config.join("muse")] {
        fs::create_dir_all(directory).unwrap();
    }
    let setup = fixture.json(
        &["setup"],
        &[
            ("CLAUDE_CONFIG_DIR", &claude_profile),
            ("GROK_HOME", &grok_profile),
            ("XDG_CONFIG_HOME", &config),
        ],
    );
    assert!(
        targets(&setup)
            .iter()
            .all(|(_, state, _)| state == "linked"),
        "{setup:#}"
    );
    let mut skipped = Vec::new();

    match installed("claude") {
        Some(claude) => {
            for config in [home.join(".claude"), claude_profile.clone()] {
                let log = claude_loads(&fixture, &claude, &config);
                assert!(
                    log.contains(&format!("user={}", config.join("skills").display())),
                    "{log}"
                );
                assert!(log.contains("Loaded 1 unique skills"), "{log}");
                assert!(log.contains("user: 1"), "{log}");
            }
        }
        None => skipped.push("claude"),
    }

    match installed("codex") {
        Some(codex) => {
            for codex_home in std::iter::once(home.join(".codex")).chain(fixture.accounts()) {
                let found = codex_skills(&fixture, &codex, &codex_home);
                assert_eq!(found.len(), 1, "{codex_home:?}: {found:?}");
                assert_eq!(found[0]["scope"], "user");
                assert_eq!(
                    found[0]["path"],
                    fixture.pack().join("SKILL.md").display().to_string()
                );
            }
        }
        None => skipped.push("codex"),
    }

    match installed("grok") {
        Some(grok) => {
            for (grok_home, expected) in [
                (
                    home.join(".grok"),
                    home.join(".grok/skills/cua-driver/SKILL.md"),
                ),
                (
                    grok_profile.clone(),
                    grok_profile.join("skills/cua-driver/SKILL.md"),
                ),
            ] {
                let mut command = live(&fixture, &grok, &[("GROK_HOME", &grok_home)]);
                command.args(["inspect", "--json"]);
                let output = bounded_output(command, 60);
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let report: Value = serde_json::from_slice(&output.stdout).unwrap();
                let found: Vec<&Value> = report["skills"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|skill| skill["name"] == "cua-driver")
                    .collect();
                assert_eq!(found.len(), 1, "{found:?}");
                assert_eq!(found[0]["source"]["path"], expected.display().to_string());
            }
        }
        None => skipped.push("grok"),
    }

    match installed("muse") {
        Some(muse) => {
            for xdg_config in [home.join(".config"), config.clone()] {
                let mut command = live(&fixture, &muse, &[("XDG_CONFIG_HOME", &xdg_config)]);
                command.args(["skills", "list", "--source", "user", "--json"]);
                let output = bounded_output(command, 90);
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let report: Value = serde_json::from_slice(&output.stdout).unwrap();
                let found: Vec<&Value> = report["skills"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|skill| skill["name"] == "cua-driver")
                    .collect();
                assert_eq!(found.len(), 1, "{found:?}");
                assert_eq!(found[0]["path"], "$CONFIG_DIR/skills/cua-driver/SKILL.md");
            }
        }
        None => skipped.push("muse"),
    }
    if !skipped.is_empty() {
        eprintln!("skipped, not installed: {}", skipped.join(", "));
    }
}
