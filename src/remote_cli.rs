//! Standalone remote command forwarding, independent of GPUI.
use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

/// PATH links commonly target target/<profile>/riwork, while the companion is
/// packaged inside the adjacent app. Canonicalize before searching both places.
pub fn resolve(executable: &Path, configured: Option<PathBuf>) -> Result<PathBuf, String> {
    let exe = executable
        .canonicalize()
        .unwrap_or_else(|_| executable.to_path_buf());
    let candidates = if let Some(path) = configured {
        vec![path]
    } else {
        let dir = exe
            .parent()
            .ok_or("Cannot locate RiWork executable directory")?;
        vec![
            dir.join("riwork-remote"),
            dir.join("RiWork.app/Contents/MacOS/riwork-remote"),
        ]
    };
    if let Some(path) = candidates.iter().find(|path| path.is_file()) {
        return Ok(path.clone());
    }
    Err(format!(
        "Missing standalone remote binary at {}. Run `riwork update --source /path/to/riWork --no-reload` to build and package it, or `cargo build --locked --release --manifest-path /path/to/riWork/remote/Cargo.toml` and set RIWORK_REMOTE_BIN=/path/to/riWork/remote/target/release/riwork-remote.",
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(" or ")
    ))
}

/// Forward verbatim before generic CLI flag parsing; retain RIWORK_HOME.
pub fn forward(args: &[String]) -> Result<(), String> {
    let exe = env::current_exe().map_err(|e| format!("Cannot locate RiWork executable: {e}"))?;
    let binary = resolve(&exe, env::var_os("RIWORK_REMOTE_BIN").map(PathBuf::from))?;
    let mut command = Command::new(&binary);
    command.args(args);
    if env::var_os("RIWORK_CLI").is_none() {
        command.env("RIWORK_CLI", exe.canonicalize().unwrap_or(exe));
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
