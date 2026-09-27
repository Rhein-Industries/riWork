//! Local-source updates staged away from the running app and installed atomically.

use fs2::FileExt;
use serde::Serialize;
use std::{
    env, fs,
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

const BUILD_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const BUNDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Serialize)]
pub struct UpdateBuild {
    pub source: PathBuf,
    pub profile: String,
    pub executable: PathBuf,
    pub bundle: PathBuf,
    pub log_path: PathBuf,
}

pub fn resolve_source(explicit: Option<&Path>) -> Result<PathBuf, String> {
    resolve_source_from(
        explicit,
        env::var_os("RIWORK_SOURCE_DIR").as_deref().map(Path::new),
        Path::new(env!("CARGO_MANIFEST_DIR")),
        env::current_exe().ok().as_deref(),
    )
}

fn resolve_source_from(
    explicit: Option<&Path>,
    configured: Option<&Path>,
    manifest: &Path,
    executable: Option<&Path>,
) -> Result<PathBuf, String> {
    if let Some(source) = explicit.or(configured) {
        return validate_source(source);
    }
    if let Ok(source) = validate_source(manifest) {
        return Ok(source);
    }
    if let Some(executable) = executable {
        for candidate in executable.ancestors().skip(1) {
            if let Ok(source) = validate_source(candidate) {
                return Ok(source);
            }
        }
    }
    Err("Cannot find the RiWork source checkout. Use `riwork update --source /path/to/riWork` or set RIWORK_SOURCE_DIR.".to_owned())
}

fn validate_source(source: &Path) -> Result<PathBuf, String> {
    let source = source
        .canonicalize()
        .map_err(|error| format!("Cannot resolve source {}: {error}", source.display()))?;
    for relative in ["Cargo.toml", "src/main.rs", "scripts/bundle-macos.sh"] {
        if !source.join(relative).is_file() {
            return Err(format!(
                "{} is not a RiWork checkout: missing {relative}",
                source.display()
            ));
        }
    }
    let manifest = fs::read_to_string(source.join("Cargo.toml"))
        .map_err(|error| format!("Cannot read RiWork Cargo.toml: {error}"))?;
    let mut package = false;
    let mut riwork = false;
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            package = line == "[package]";
        }
        if package {
            if let Some((key, value)) = line.split_once('=') {
                let value = value.trim().split('#').next().unwrap_or_default().trim();
                if key.trim() == "name" && matches!(value, "\"riwork\"" | "'riwork'") {
                    riwork = true;
                }
            }
        }
    }
    if !riwork {
        return Err(format!(
            "{} does not contain the riwork Cargo package",
            source.display()
        ));
    }
    Ok(source)
}

fn update_profile(profile: Option<&str>) -> Result<&str, String> {
    let profile = profile.unwrap_or("release");
    match profile {
        "debug" | "dev" => Ok("debug"),
        "release" => Ok("release"),
        _ => Err("Update profile must be debug or release".to_owned()),
    }
}

/// Build the selected checkout without pulling Git changes or touching user
/// settings. The returned paths are installed artifacts; GUI reload is separate.
pub fn build_update(source: &Path, profile: Option<&str>) -> Result<UpdateBuild, String> {
    let source = validate_source(source)?;
    let profile = update_profile(profile)?.to_owned();
    let target = source.join("target");
    fs::create_dir_all(&target)
        .map_err(|error| format!("Cannot create {}: {error}", target.display()))?;
    let _lock = update_lock(&target)?;
    let identifier = Uuid::new_v4();
    let log_path = target.join(format!("riwork-update-{identifier}.log"));
    let mut log_options = OpenOptions::new();
    log_options.create_new(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        log_options.mode(0o600);
    }
    let mut log = log_options
        .open(&log_path)
        .map_err(|error| format!("Cannot create update log: {error}"))?;
    let mut staging = StagingDirectory::new(target.join(format!(".riwork-update-{identifier}")))?;
    let staged_target = staging.path.join("target");
    fs::create_dir_all(&staged_target)
        .map_err(|error| format!("Cannot create staging target: {error}"))?;
    let tool_path = update_tool_path()?;
    seed_build_cache(
        &target.join(&profile),
        &staged_target.join(&profile),
        &tool_path,
        &mut log,
        &log_path,
    )?;
    let cargo = find_tool("cargo", &tool_path)
        .ok_or("Cargo is required to rebuild RiWork. Install Rust or put cargo on PATH.")?;
    let mut build = Command::new(cargo);
    build
        .current_dir(&source)
        .args(["build", "--bin", "riwork"])
        .env("CARGO_TARGET_DIR", &staged_target)
        .env("PATH", &tool_path);
    if profile == "release" {
        build.arg("--release");
    }
    eprintln!("Building RiWork ({profile}). Log: {}", log_path.display());
    run_stage(
        &mut build,
        "Cargo build",
        BUILD_TIMEOUT,
        &mut log,
        &log_path,
    )?;

    // The existing bundler removes target/<profile>/RiWork.app. Run an exact
    // copy in a disposable checkout layout so it can never remove a live app.
    let scripts = staging.path.join("scripts");
    fs::create_dir_all(&scripts)
        .map_err(|error| format!("Cannot create staging scripts: {error}"))?;
    let bundler = scripts.join("bundle-macos.sh");
    fs::copy(source.join("scripts/bundle-macos.sh"), &bundler)
        .map_err(|error| format!("Cannot stage macOS bundler: {error}"))?;
    let mut bundle_command = Command::new("/bin/sh");
    bundle_command
        .arg(&bundler)
        .arg(&profile)
        .current_dir(&staging.path)
        .env("PATH", &tool_path);
    eprintln!("Packaging RiWork.app.");
    run_stage(
        &mut bundle_command,
        "macOS bundle",
        BUNDLE_TIMEOUT,
        &mut log,
        &log_path,
    )?;
    let staged_executable = staged_target.join(&profile).join("riwork");
    let staged_bundle = staged_target.join(&profile).join("RiWork.app");
    validate_artifacts(&staged_executable, &staged_bundle)?;
    let destination = target.join(&profile);
    fs::create_dir_all(&destination)
        .map_err(|error| format!("Cannot create install directory: {error}"))?;
    let executable = destination.join("riwork");
    let bundle = destination.join("RiWork.app");
    log.sync_all()
        .map_err(|error| format!("Cannot sync update log: {error}"))?;
    if let Err(error) = install_artifacts(&staged_executable, &staged_bundle, &executable, &bundle)
    {
        staging.cleanup = false;
        return Err(format!(
            "{error}. Staging preserved at {}. Log: {}",
            staging.path.display(),
            log_path.display()
        ));
    }
    writeln!(
        log,
        "Installed executable: {}\nInstalled bundle: {}",
        executable.display(),
        bundle.display()
    )
    .ok();
    let _ = log.sync_all();
    Ok(UpdateBuild {
        source,
        profile,
        executable,
        bundle,
        log_path,
    })
}

fn update_lock(target: &Path) -> Result<File, String> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(target.join(".riwork-update.lock"))
        .map_err(|error| format!("Cannot open update lock: {error}"))?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(file),
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(50))
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                return Err("Another RiWork update is running. Wait for it to finish.".to_owned());
            }
            Err(error) => return Err(format!("Cannot lock RiWork update: {error}")),
        }
    }
}

fn update_tool_path() -> Result<std::ffi::OsString, String> {
    let mut directories = env::var_os("PATH")
        .map(|path| env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default();
    if let Some(home) = env::var_os("HOME") {
        for directory in [
            PathBuf::from(&home).join(".cargo/bin"),
            PathBuf::from(home).join(".local/bin"),
        ] {
            if !directories.contains(&directory) {
                directories.push(directory);
            }
        }
    }
    for directory in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"] {
        let directory = PathBuf::from(directory);
        if !directories.contains(&directory) {
            directories.push(directory);
        }
    }
    env::join_paths(directories)
        .map_err(|error| format!("Cannot construct update tool PATH: {error}"))
}

fn find_tool(name: &str, path: &std::ffi::OsStr) -> Option<PathBuf> {
    env::split_paths(path)
        .map(|directory| directory.join(name))
        .find(|path| path.is_file())
}

fn seed_build_cache(
    existing: &Path,
    staged: &Path,
    tool_path: &std::ffi::OsStr,
    log: &mut File,
    log_path: &Path,
) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    if existing.is_dir() {
        let mut copy = Command::new("/bin/cp");
        copy.args(["-cR", "--"])
            .arg(existing)
            .arg(staged)
            .env("PATH", tool_path);
        if run_stage(
            &mut copy,
            "Clone build cache",
            BUNDLE_TIMEOUT,
            log,
            log_path,
        )
        .is_err()
        {
            // Non-APFS filesystems need not support copy-on-write. Rebuild from
            // an empty staging target rather than writing into the live one.
            if staged.exists() {
                fs::remove_dir_all(staged)
                    .map_err(|error| format!("Cannot clear incomplete staging cache: {error}"))?;
            }
            writeln!(log, "Clone unavailable; building without cached artifacts.").ok();
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (existing, staged, tool_path, log, log_path);
    Ok(())
}

fn run_stage(
    command: &mut Command,
    name: &str,
    timeout: Duration,
    log: &mut File,
    log_path: &Path,
) -> Result<(), String> {
    writeln!(log, "\n{name}").map_err(|error| format!("Cannot write update log: {error}"))?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(
            log.try_clone().map_err(|error| error.to_string())?,
        ))
        .stderr(Stdio::from(
            log.try_clone().map_err(|error| error.to_string())?,
        ));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("Cannot start {name}: {error}. Log: {}", log_path.display()))?;
    let deadline = Instant::now() + timeout;
    let mut progress = Instant::now() + Duration::from_secs(30);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                return Err(format!(
                    "{name} failed ({status}). Log: {}\n{}",
                    log_path.display(),
                    log_tail(log_path)
                ));
            }
            Ok(None) if Instant::now() >= deadline => {
                terminate_stage(&mut child);
                return Err(format!(
                    "{name} timed out after {} seconds. Log: {}\n{}",
                    timeout.as_secs(),
                    log_path.display(),
                    log_tail(log_path)
                ));
            }
            Ok(None) => {
                if Instant::now() >= progress {
                    eprintln!("{name} is still running. Log: {}", log_path.display());
                    progress = Instant::now() + Duration::from_secs(30);
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(error) => {
                terminate_stage(&mut child);
                return Err(format!(
                    "Cannot wait for {name}: {error}. Log: {}",
                    log_path.display()
                ));
            }
        }
    }
}

fn terminate_stage(child: &mut Child) {
    #[cfg(unix)]
    let _ = Command::new("/bin/kill")
        .args(["-KILL", "--", &format!("-{}", child.id())])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = child.kill();
    let _ = child.wait();
}

fn log_tail(path: &Path) -> String {
    let Ok(mut file) = File::open(path) else {
        return String::new();
    };
    let length = file
        .metadata()
        .map(|metadata| metadata.len())
        .unwrap_or_default();
    let _ = file.seek(SeekFrom::Start(length.saturating_sub(8192)));
    let mut bytes = Vec::new();
    let _ = file.take(8192).read_to_end(&mut bytes);
    let text = String::from_utf8_lossy(&bytes);
    let lines = text.lines().collect::<Vec<_>>();
    lines[lines.len().saturating_sub(30)..].join("\n")
}

fn validate_artifacts(executable: &Path, bundle: &Path) -> Result<(), String> {
    for file in [
        executable.to_path_buf(),
        bundle.join("Contents/MacOS/riwork"),
        bundle.join("Contents/Info.plist"),
    ] {
        if !file.is_file() {
            return Err(format!("Update artifact is missing: {}", file.display()));
        }
    }
    for relative in [
        "Contents/Resources/terminfo/78/xterm-ghostty",
        "Contents/Resources/ghostty/shell-integration/zsh/ghostty-integration",
    ] {
        if !bundle.join(relative).is_file() {
            return Err(format!("Update bundle is missing {relative}"));
        }
    }
    Ok(())
}

fn install_artifacts(
    staged_executable: &Path,
    staged_bundle: &Path,
    executable: &Path,
    bundle: &Path,
) -> Result<(), String> {
    let executable_replaced = atomic_replace(staged_executable, executable)?;
    if let Err(error) = atomic_replace(staged_bundle, bundle) {
        let rollback = if executable_replaced {
            atomic_replace(staged_executable, executable).map(|_| ())
        } else {
            fs::rename(executable, staged_executable).map_err(|rollback| rollback.to_string())
        };
        return Err(match rollback {
            Ok(()) => {
                format!("Cannot install app bundle; restored the previous executable: {error}")
            }
            Err(rollback) => format!(
                "Cannot install app bundle: {error}. Executable rollback also failed: {rollback}"
            ),
        });
    }
    Ok(())
}

/// With an existing destination, exchange names so old inodes remain intact
/// for running processes and can be restored if the other artifact fails.
fn atomic_replace(staged: &Path, destination: &Path) -> Result<bool, String> {
    if fs::symlink_metadata(destination)
        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    {
        fs::rename(staged, destination)
            .map_err(|error| format!("Cannot install {}: {error}", destination.display()))?;
        return Ok(false);
    }
    #[cfg(target_os = "macos")]
    {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        unsafe extern "C" {
            fn renameatx_np(
                from_fd: i32,
                from: *const std::ffi::c_char,
                to_fd: i32,
                to: *const std::ffi::c_char,
                flags: u32,
            ) -> i32;
        }
        let from =
            CString::new(staged.as_os_str().as_bytes()).map_err(|error| error.to_string())?;
        let to =
            CString::new(destination.as_os_str().as_bytes()).map_err(|error| error.to_string())?;
        // Darwin's AT_FDCWD=-2 and RENAME_SWAP=0x2. Both paths are on the same
        // source/target filesystem and remain owned by this transaction.
        if unsafe { renameatx_np(-2, from.as_ptr(), -2, to.as_ptr(), 0x2) } != 0 {
            return Err(format!(
                "Cannot atomically replace {}: {}",
                destination.display(),
                std::io::Error::last_os_error()
            ));
        }
        Ok(true)
    }
    #[cfg(not(target_os = "macos"))]
    Err(format!(
        "Replacing an existing app atomically requires macOS: {}",
        destination.display()
    ))
}

struct StagingDirectory {
    path: PathBuf,
    cleanup: bool,
}
impl StagingDirectory {
    fn new(path: PathBuf) -> Result<Self, String> {
        fs::create_dir(&path)
            .map_err(|error| format!("Cannot create update staging directory: {error}"))?;
        Ok(Self {
            path,
            cleanup: true,
        })
    }
}
impl Drop for StagingDirectory {
    fn drop(&mut self) {
        if self.cleanup {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(parent: &Path, name: &str) -> PathBuf {
        let source = parent.join(name);
        fs::create_dir_all(source.join("src")).unwrap();
        fs::create_dir_all(source.join("scripts")).unwrap();
        fs::write(
            source.join("Cargo.toml"),
            "[package]\nname = \"riwork\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::write(source.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(source.join("scripts/bundle-macos.sh"), "#!/bin/sh\n").unwrap();
        source
    }

    #[test]
    fn source_resolution_honors_explicit_and_configured_checkouts() {
        let temporary = StagingDirectory::new(
            env::temp_dir().join(format!("riwork-update-source-{}", Uuid::new_v4())),
        )
        .unwrap();
        let explicit = fixture(&temporary.path, "explicit source");
        let configured = fixture(&temporary.path, "configured source");
        let compiled = fixture(&temporary.path, "compiled source");
        assert_eq!(
            resolve_source_from(Some(&explicit), Some(&configured), &compiled, None).unwrap(),
            explicit.canonicalize().unwrap()
        );
        assert_eq!(
            resolve_source_from(None, Some(&configured), &compiled, None).unwrap(),
            configured.canonicalize().unwrap()
        );
        assert_eq!(
            resolve_source_from(None, None, &compiled, None).unwrap(),
            compiled.canonicalize().unwrap()
        );
        let missing = temporary.path.join("missing");
        assert!(resolve_source_from(Some(&missing), Some(&configured), &compiled, None).is_err());
        assert!(resolve_source_from(None, Some(&missing), &compiled, None).is_err());
        let executable = compiled.join("target/debug/RiWork.app/Contents/MacOS/riwork");
        assert_eq!(
            resolve_source_from(None, None, &missing, Some(&executable)).unwrap(),
            compiled.canonicalize().unwrap()
        );
    }

    #[test]
    fn source_validation_rejects_another_package_and_missing_bundler() {
        let temporary = StagingDirectory::new(
            env::temp_dir().join(format!("riwork-update-validation-{}", Uuid::new_v4())),
        )
        .unwrap();
        let source = fixture(&temporary.path, "source");
        fs::write(
            source.join("Cargo.toml"),
            "[package]\nname = \"other\"\n[dependencies]\nname = \"riwork\"\n",
        )
        .unwrap();
        assert!(
            validate_source(&source)
                .unwrap_err()
                .contains("riwork Cargo package")
        );
        fs::write(
            source.join("Cargo.toml"),
            "[package]\nname = 'riwork' # package\n",
        )
        .unwrap();
        fs::remove_file(source.join("scripts/bundle-macos.sh")).unwrap();
        assert!(
            validate_source(&source)
                .unwrap_err()
                .contains("scripts/bundle-macos.sh")
        );
        for profile in ["../release", "release; false", "", "/tmp", "custom"] {
            assert!(update_profile(Some(profile)).is_err());
        }
        assert_eq!(update_profile(Some("dev")).unwrap(), "debug");
        assert_eq!(update_profile(Some("debug")).unwrap(), "debug");
        assert_eq!(update_profile(Some("release")).unwrap(), "release");
        // Updates optimize the app even when invoked by a development CLI.
        assert_eq!(update_profile(None).unwrap(), "release");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn atomic_install_preserves_open_inodes_and_rolls_back_bundle_failure() {
        let temporary = StagingDirectory::new(
            env::temp_dir().join(format!("riwork-update-install-{}", Uuid::new_v4())),
        )
        .unwrap();
        let staged_executable = temporary.path.join("staged executable");
        let executable = temporary.path.join("installed executable");
        let staged_bundle = temporary.path.join("staged bundle");
        let bundle = temporary.path.join("installed bundle");
        fs::write(&executable, "old executable").unwrap();
        fs::write(&staged_executable, "new executable").unwrap();
        fs::create_dir(&bundle).unwrap();
        fs::write(bundle.join("version"), "old bundle").unwrap();
        let mut live_executable = File::open(&executable).unwrap();
        // Failure installing the second artifact must restore the first.
        let error = install_artifacts(&staged_executable, &staged_bundle, &executable, &bundle)
            .unwrap_err();
        assert!(
            error.contains("restored the previous executable"),
            "{error}"
        );
        assert_eq!(fs::read_to_string(&executable).unwrap(), "old executable");
        assert_eq!(
            fs::read_to_string(bundle.join("version")).unwrap(),
            "old bundle"
        );
        fs::create_dir(&staged_bundle).unwrap();
        fs::write(staged_bundle.join("version"), "new bundle").unwrap();
        install_artifacts(&staged_executable, &staged_bundle, &executable, &bundle).unwrap();
        assert_eq!(fs::read_to_string(&executable).unwrap(), "new executable");
        assert_eq!(
            fs::read_to_string(bundle.join("version")).unwrap(),
            "new bundle"
        );
        let mut mapped_old_contents = String::new();
        live_executable
            .read_to_string(&mut mapped_old_contents)
            .unwrap();
        assert_eq!(mapped_old_contents, "old executable");
        assert_eq!(
            fs::read_to_string(&staged_executable).unwrap(),
            "old executable"
        );
        assert_eq!(
            fs::read_to_string(staged_bundle.join("version")).unwrap(),
            "old bundle"
        );
    }

    #[cfg(unix)]
    #[test]
    fn failed_or_timed_out_build_stages_keep_diagnostic_logs() {
        let temporary = StagingDirectory::new(
            env::temp_dir().join(format!("riwork-update-stage-{}", Uuid::new_v4())),
        )
        .unwrap();
        let log_path = temporary.path.join("build.log");
        let mut log = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&log_path)
            .unwrap();
        let error = run_stage(
            Command::new("/bin/sh").args(["-c", "printf 'meaningful failure\\n' >&2; exit 7"]),
            "fixture build",
            Duration::from_secs(1),
            &mut log,
            &log_path,
        )
        .unwrap_err();
        assert!(error.contains("meaningful failure"), "{error}");
        let start = Instant::now();
        let error = run_stage(
            Command::new("/bin/sh").args(["-c", "sleep 30 & wait"]),
            "fixture timeout",
            Duration::from_millis(30),
            &mut log,
            &log_path,
        )
        .unwrap_err();
        assert!(error.contains("timed out"), "{error}");
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(
            fs::read_to_string(log_path)
                .unwrap()
                .contains("meaningful failure")
        );
    }
}
