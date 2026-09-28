//! Compile the production root terminal modules with strict Clippy, without GPUI.
#[path = "../../src/session_input.rs"]
mod session_input;
#[path = "../../src/session_viewport.rs"]
mod session_viewport;
use anyhow::{Result, ensure};
use session_viewport::{Size, clear, resize};
use std::process::Command;
use uuid::Uuid;
struct Server(String);
impl Server {
    fn tmux(&self, args: &[&str]) -> std::result::Result<String, String> {
        let out = Command::new("tmux")
            .args(["-L", &self.0])
            .args(args)
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).into());
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().into())
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.tmux(&["kill-server"]);
    }
}
#[test]
fn real_equal_size_pins_policy_and_old_owner_cannot_clear_new_owner() -> Result<()> {
    let home = tempfile::tempdir()?;
    let server = Server(format!("riwork-viewport-test-{}", Uuid::new_v4()));
    let shell = Uuid::new_v4().to_string();
    let owner = Uuid::new_v4().to_string();
    let old = Uuid::new_v4().to_string();
    let new = Uuid::new_v4().to_string();
    let t = |args: &[&str]| server.tmux(args);
    let window = format!("{shell}:0");
    t(&[
        "new-session",
        "-d",
        "-s",
        &shell,
        "-x",
        "80",
        "-y",
        "24",
        "/bin/sleep 60",
    ])
    .map_err(anyhow::Error::msg)?;
    // No watchdog is needed in this scoped probe: clear explicitly before cleanup.
    let size = || Size {
        shell_id: shell.clone(),
        columns: 80,
        rows: 24,
    };
    resize(
        home.path(),
        size(),
        &owner,
        &old,
        std::path::Path::new("/usr/bin/true"),
        &t,
    )
    .map_err(anyhow::Error::msg)?;
    assert_eq!(
        t(&["show-options", "-wqv", "-t", &window, "window-size"]).unwrap(),
        "manual"
    );
    clear(home.path(), &shell, &owner, &old, &t).map_err(anyhow::Error::msg)?;
    assert_eq!(
        t(&["show-options", "-wqv", "-t", &window, "window-size"]).unwrap(),
        ""
    );
    t(&["set-option", "-w", "-t", &window, "window-size", "smallest"])
        .map_err(anyhow::Error::msg)?;
    resize(
        home.path(),
        size(),
        &owner,
        &new,
        std::path::Path::new("/usr/bin/true"),
        &t,
    )
    .map_err(anyhow::Error::msg)?;
    assert!(
        clear(home.path(), &shell, &owner, &old, &t)
            .unwrap_err()
            .contains("viewport_busy")
    );
    t(&["split-window", "-d", "-t", &window, "/bin/sleep 60"]).map_err(anyhow::Error::msg)?;
    clear(home.path(), &shell, &owner, &new, &t).map_err(anyhow::Error::msg)?;
    assert_eq!(
        t(&["show-options", "-wqv", "-t", &window, "window-size"]).unwrap(),
        "smallest"
    );
    session_viewport::watch(home.path(), &shell, &owner, &new, &t).map_err(anyhow::Error::msg)?;
    ensure!(
        resize(
            home.path(),
            size(),
            &owner,
            &new,
            std::path::Path::new("/usr/bin/true"),
            &t
        )
        .unwrap_err()
        .contains("viewport_unsupported")
    );
    Ok(())
}
#[test]
fn private_controls_and_invalid_ids_fail_before_terminal_input() {
    let home = tempfile::tempdir().unwrap();
    assert!(
        session_input::submit(
            home.path(),
            "bad-id",
            "text",
            &|_| panic!("must not interact"),
            &|_, _| panic!("must not interact")
        )
        .is_err()
    );
    assert!(session_viewport::validate_size(19, 17).is_err());
    assert!(session_viewport::validate_size(43, 161).is_err());
    let id = Uuid::new_v4().to_string();
    let dir = session_viewport::control_dir(home.path()).unwrap();
    #[cfg(unix)]
    {
        use std::{
            fs,
            os::unix::fs::{PermissionsExt, symlink},
        };
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let target = home.path().join("unrelated");
        fs::write(&target, "protected").unwrap();
        symlink(&target, dir.join(format!("{id}-input.lock"))).unwrap();
        assert!(session_viewport::lock(home.path(), &id, "input").is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "protected");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(session_viewport::control_dir(home.path()).is_err());
    }
}
