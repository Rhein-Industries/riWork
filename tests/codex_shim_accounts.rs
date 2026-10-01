//! Process-level checks of the managed `codex` launcher (`riwork cua harness
//! codex`) against a fake `codex` executable in a throwaway environment. The
//! fake records its arguments and `CODEX_HOME`, so a refused command shows up
//! as a fake that never ran.

#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};
use uuid::Uuid;

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("riwork-codex-shim-{}", Uuid::new_v4()));
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::create_dir_all(root.join("home")).unwrap();
        let fake = root.join("bin/codex");
        fs::write(
            &fake,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"CODEX_HOME=$CODEX_HOME\" \"$@\" > '{}'\n",
                root.join("ran").display()
            ),
        )
        .unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o700)).unwrap();
        Self { root }
    }

    /// An Orca-managed saved account home, as RiWork exports it to a shell.
    fn managed_home(&self) -> PathBuf {
        let home = self.root.join("orca/codex-accounts/account-a/home");
        fs::create_dir_all(&home).unwrap();
        home
    }

    fn ran(&self) -> Option<Vec<String>> {
        let recorded = fs::read_to_string(self.root.join("ran")).ok()?;
        fs::remove_file(self.root.join("ran")).unwrap();
        Some(recorded.lines().map(str::to_owned).collect())
    }

    fn codex(&self, codex_home: &Path, arguments: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_riwork"))
            .args(["cua", "harness", "codex", "--"])
            .args(arguments)
            .current_dir(&self.root)
            .env_clear()
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.root.join("bin").display()),
            )
            .env("HOME", self.root.join("home"))
            .env("RIWORK_HOME", self.root.join("state"))
            .env("ORCA_USER_DATA_PATH", self.root.join("orca"))
            .env("CODEX_HOME", codex_home)
            .output()
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn credential_and_config_commands_never_reach_an_orca_managed_home() {
    let fixture = Fixture::new();
    let managed = fixture.managed_home();
    for arguments in [
        &["logout"][..],
        &["login"],
        &["login", "--device-auth"],
        &["--profile", "work", "logout"],
        &["mcp", "add", "docs", "--", "server"],
        &["mcp", "remove", "docs"],
        &["plugin", "marketplace", "add", "x"],
    ] {
        let output = fixture.codex(&managed, arguments);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{arguments:?} was allowed");
        assert!(stderr.contains("Orca"), "{arguments:?}: {stderr}");
        assert!(stderr.contains("CODEX_HOME="), "{arguments:?}: {stderr}");
        assert!(stderr.contains("`codex "), "{arguments:?}: {stderr}");
        assert_eq!(fixture.ran(), None, "{arguments:?} reached codex");
    }
}

#[test]
fn reads_and_the_users_own_profile_still_pass_through() {
    let fixture = Fixture::new();
    let managed = fixture.managed_home();
    for arguments in [
        &["login", "status"][..],
        &["mcp", "list"],
        &["logout", "--help"],
        &["--version"],
    ] {
        let output = fixture.codex(&managed, arguments);
        assert!(
            output.status.success(),
            "{arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let ran = fixture
            .ran()
            .unwrap_or_else(|| panic!("{arguments:?} did not run"));
        assert_eq!(ran[0], format!("CODEX_HOME={}", managed.display()));
        assert_eq!(ran[1..], *arguments);
    }

    // Naming one's own home on the command line is the documented way out.
    let own = fixture.root.join("home/.codex");
    fs::create_dir_all(&own).unwrap();
    let output = fixture.codex(&own, &["logout"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let ran = fixture.ran().expect("the user's own logout ran");
    assert_eq!(
        ran,
        [format!("CODEX_HOME={}", own.display()), "logout".into()]
    );
}

/// Launches through the managed wrappers with RiWork's agent-screen setting
/// either left at its default or written to `settings.json`.
impl Fixture {
    fn driver(&self) -> PathBuf {
        let driver = self.root.join("bin/fake-cua-driver");
        fs::write(&driver, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&driver, fs::Permissions::from_mode(0o700)).unwrap();
        driver
    }

    fn install_claude(&self) {
        let fake = self.root.join("bin/claude");
        fs::write(
            &fake,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"SCREEN=${{CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN-unset}}\" \"$@\" > '{}'\n",
                self.root.join("ran").display()
            ),
        )
        .unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn set_inline(&self, inline: bool) {
        fs::create_dir_all(self.root.join("state")).unwrap();
        fs::write(
            self.root.join("state/settings.json"),
            format!("{{\"schema_version\":1,\"agent_inline_mode\":{inline}}}"),
        )
        .unwrap();
    }

    fn launch(&self, harness: &str, arguments: &[&str]) -> Vec<String> {
        let own = self.root.join("home/.codex");
        fs::create_dir_all(&own).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_riwork"))
            .args(["cua", "harness", harness, "--"])
            .args(arguments)
            .current_dir(&self.root)
            .env_clear()
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.root.join("bin").display()),
            )
            .env("HOME", self.root.join("home"))
            .env("RIWORK_HOME", self.root.join("state"))
            .env("RIWORK_CUA_DRIVER", self.driver())
            .env("CODEX_HOME", &own)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{harness} {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        self.ran()
            .unwrap_or_else(|| panic!("{harness} {arguments:?} did not run"))
    }
}

#[test]
fn interactive_codex_launches_run_inline_unless_the_setting_is_off() {
    let fixture = Fixture::new();
    let count = |ran: &[String]| ran.iter().filter(|a| *a == "--no-alt-screen").count();

    // Nothing saved yet: the default is inline, on the bare command, a resume
    // and a prompt alike.
    for arguments in [
        &[][..],
        &["resume", "--last"],
        &["--model", "m", "fix the bug"],
    ] {
        let ran = fixture.launch("codex", arguments);
        assert_eq!(count(&ran), 1, "{arguments:?}: {ran:?}");
    }
    // Commands that never open the interactive screen would reject the flag.
    for arguments in [&["exec", "say hi"][..], &["review"], &["mcp", "list"]] {
        let ran = fixture.launch("codex", arguments);
        assert_eq!(count(&ran), 0, "{arguments:?}: {ran:?}");
    }
    // The user's own flag is not doubled.
    let ran = fixture.launch("codex", &["--no-alt-screen"]);
    assert_eq!(count(&ran), 1, "{ran:?}");

    fixture.set_inline(false);
    for arguments in [&[][..], &["resume", "--last"]] {
        let ran = fixture.launch("codex", arguments);
        assert_eq!(count(&ran), 0, "{arguments:?}: {ran:?}");
    }
    fixture.set_inline(true);
    assert_eq!(count(&fixture.launch("codex", &["resume"])), 1);
}

#[test]
fn interactive_claude_launches_leave_the_alternate_screen_unless_the_setting_is_off() {
    let fixture = Fixture::new();
    fixture.install_claude();
    for arguments in [&[][..], &["--resume"]] {
        let ran = fixture.launch("claude", arguments);
        assert_eq!(ran[0], "SCREEN=1", "{arguments:?}: {ran:?}");
        assert!(!ran.iter().any(|a| a == "--no-alt-screen"), "{ran:?}");
    }
    // A utility command is not an interactive screen.
    assert_eq!(fixture.launch("claude", &["--version"])[0], "SCREEN=unset");
    fixture.set_inline(false);
    assert_eq!(fixture.launch("claude", &[])[0], "SCREEN=unset");
}
