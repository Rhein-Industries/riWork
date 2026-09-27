//! Standalone remote command forwarding, independent of GPUI.
use std::{env, path::PathBuf, process::Command};

/// Forward verbatim before generic CLI flag parsing; retain RIWORK_HOME.
pub fn forward(args: &[String]) -> Result<(), String> {
    let binary = env::var_os("RIWORK_REMOTE_BIN")
        .map(PathBuf::from)
        .or_else(|| {
            env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(|dir| dir.join("riwork-remote")))
        })
        .ok_or("Cannot locate riwork-remote; set RIWORK_REMOTE_BIN to its absolute path")?;
    if !binary.is_file() {
        return Err(format!(
            "Missing standalone remote binary at {}. Build it with `cargo build --release --manifest-path /path/to/riWork/remote/Cargo.toml`, then set RIWORK_REMOTE_BIN=/path/to/riWork/remote/target/release/riwork-remote or install it beside riwork.",
            binary.display()
        ));
    }
    let mut command = Command::new(&binary);
    command.args(args);
    if env::var_os("RIWORK_CLI").is_none()
        && let Ok(exe) = env::current_exe()
    {
        command.env("RIWORK_CLI", exe);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(format!("Start {}: {}", binary.display(), command.exec()))
    }
    #[cfg(not(unix))]
    {
        let status = command
            .status()
            .map_err(|error| format!("Start {}: {error}", binary.display()))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("riwork-remote exited with {status}"))
        }
    }
}
