//! Local-source updates staged away from the running app and installed atomically.

use fs2::FileExt;
use serde::Serialize;
use std::{
    env, fs,
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicI32, Ordering},
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

const BUILD_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const BUNDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const SMOKE_TIMEOUT: Duration = Duration::from_secs(30);
/// Cargo and rustc get this long to exit on SIGTERM before the group is killed.
const STAGE_TERMINATE_GRACE: Duration = Duration::from_secs(5);
/// Update logs kept across runs, not counting the current one.
const KEPT_LOGS: usize = 5;
/// Marks staging left behind on purpose because it holds the only copy of the old build.
const PRESERVED_MARKER: &str = "PRESERVED";
const PRESERVED_RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);

#[derive(Debug, Serialize)]
pub struct UpdateBuild {
    pub source: PathBuf,
    pub profile: String,
    pub executable: PathBuf,
    pub bundle: PathBuf,
    /// The build this one replaced, kept for rollback until the next update.
    pub previous_bundle: Option<PathBuf>,
    pub log_path: PathBuf,
}

/// Where an update finds its tools, whether the bundle's signature is checked
/// (fixture bundles in tests are not signed), and the identity it is signed
/// with (`None`: the bundler's ad-hoc default).
struct UpdateEnvironment {
    tool_path: std::ffi::OsString,
    verify_signature: bool,
    codesign_identity: Option<SigningIdentity>,
    /// The Zig the Ghostty build is given (`None`: whatever `ZIG` or `PATH` says).
    zig: Option<PathBuf>,
}

/// A code signing identity for the bundle: what `codesign --sign` is given,
/// and the name it is reported by.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SigningIdentity {
    sign_with: String,
    name: String,
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
    for relative in [
        "Cargo.toml",
        "src/main.rs",
        "scripts/bundle-macos.sh",
        // The only icon the bundler reads.
        "assets/app-icon/RiWork-legacy.icns",
    ] {
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
        if package && let Some((key, value)) = line.split_once('=') {
            let value = value.trim().split('#').next().unwrap_or_default().trim();
            if key.trim() == "name" && matches!(value, "\"riwork\"" | "'riwork'") {
                riwork = true;
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
    build_update_with(
        source,
        profile,
        &UpdateEnvironment {
            tool_path: update_tool_path()?,
            verify_signature: cfg!(target_os = "macos"),
            codesign_identity: update_signing_identity(),
            zig: update_zig(),
        },
    )
}

fn build_update_with(
    source: &Path,
    profile: Option<&str>,
    environment: &UpdateEnvironment,
) -> Result<UpdateBuild, String> {
    // Declared first so it is released last: a signal that arrives during
    // cleanup must not end the process before staging is removed.
    let _interrupts = InterruptGuard::install();
    let source = validate_source(source)?;
    let profile = update_profile(profile)?.to_owned();
    let target = source.join("target");
    fs::create_dir_all(&target)
        .map_err(|error| format!("Cannot create {}: {error}", target.display()))?;
    let _lock = update_lock(&target)?;
    let swept = sweep_stale_artifacts(&target);
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
    if swept != (0, 0) {
        writeln!(
            log,
            "Removed {} stale staging directories and {} old logs.",
            swept.0, swept.1
        )
        .ok();
    }
    let mut staging = StagingDirectory::new(target.join(format!(".riwork-update-{identifier}")))?;
    let staged_target = staging.path.join("target");
    fs::create_dir_all(&staged_target)
        .map_err(|error| format!("Cannot create staging target: {error}"))?;
    let tool_path = environment.tool_path.as_os_str();
    seed_build_cache(
        &target.join(&profile),
        &staged_target.join(&profile),
        tool_path,
        &mut log,
        &log_path,
    )?;
    let cargo = find_tool("cargo", tool_path)
        .ok_or("Cargo is required to rebuild RiWork. Install Rust or put cargo on PATH.")?;
    let mut build = Command::new(&cargo);
    build
        .current_dir(&source)
        .args(["build", "--locked", "--bin", "riwork"])
        .env("CARGO_TARGET_DIR", &staged_target)
        .env("PATH", tool_path);
    if let Some(zig) = &environment.zig {
        build.env("ZIG", zig);
        eprintln!("Using Zig at {}.", zig.display());
    }
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

    // Build the companion in the disposable bundler layout. Never copy a stale
    // optional binary from the running app or silently omit it after failure.
    let companion = source.join("remote/Cargo.toml").is_file();
    if companion {
        let remote_target = staging.path.join("remote/target");
        fs::create_dir_all(&remote_target)
            .map_err(|e| format!("Cannot stage remote target: {e}"))?;
        seed_build_cache(
            &source.join("remote/target").join(&profile),
            &remote_target.join(&profile),
            tool_path,
            &mut log,
            &log_path,
        )?;
        let mut remote_build =
            companion_build_command(&cargo, &source, &remote_target, &profile, tool_path);
        eprintln!("Building riwork-remote ({profile}).");
        run_stage(
            &mut remote_build,
            "Remote companion build",
            BUILD_TIMEOUT,
            &mut log,
            &log_path,
        )?;
    }

    // The existing bundler removes target/<profile>/RiWork.app. Run an exact
    // copy in a disposable checkout layout so it can never remove a live app.
    let bundler = stage_bundle_inputs(&source, &staging.path)?;
    let mut bundle_command = Command::new("/bin/sh");
    bundle_command
        .arg(&bundler)
        .arg(&profile)
        .current_dir(&staging.path)
        .env("PATH", tool_path);
    if let Some(identity) = &environment.codesign_identity {
        bundle_command.env("CODESIGN_IDENTITY", &identity.sign_with);
        eprintln!("Signing with {}.", identity.name);
    }
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
    validate_companion(&staged_bundle, companion)?;
    if environment.verify_signature {
        let mut verify = Command::new("/usr/bin/codesign");
        verify
            .args(["--verify", "--deep", "--strict"])
            .arg(&staged_bundle);
        run_stage(
            &mut verify,
            "Bundle signature check",
            BUNDLE_TIMEOUT,
            &mut log,
            &log_path,
        )?;
    }
    // A build that compiles can still crash at startup. Run both binaries that
    // will be installed before anything is replaced, so a bad build never
    // displaces the working one.
    eprintln!("Checking that the new build starts.");
    smoke_test(&staged_executable, &staging.path, &mut log, &log_path)?;
    smoke_test(
        &staged_bundle.join("Contents/MacOS/riwork"),
        &staging.path,
        &mut log,
        &log_path,
    )?;
    let destination = target.join(&profile);
    fs::create_dir_all(&destination)
        .map_err(|error| format!("Cannot create install directory: {error}"))?;
    let executable = destination.join("riwork");
    let bundle = destination.join("RiWork.app");
    log.sync_all()
        .map_err(|error| format!("Cannot sync update log: {error}"))?;
    // Past this point the swap is short and must complete, so this is the last
    // chance for an interrupt to abandon the update.
    if let Some(signal) = interrupted() {
        return Err(interrupted_message(signal, Some(&log_path)));
    }
    if let Err(error) = install_artifacts(&staged_executable, &staged_bundle, &executable, &bundle)
    {
        staging.preserve();
        return Err(format!(
            "{error}. Staging preserved at {}. Log: {}",
            staging.path.display(),
            log_path.display()
        ));
    }
    let previous_bundle = retain_previous(
        &staged_executable,
        &staged_bundle,
        &executable,
        &bundle,
        &mut log,
    );
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
        previous_bundle,
        log_path,
    })
}

fn companion_build_command(
    cargo: &Path,
    source: &Path,
    target: &Path,
    profile: &str,
    tool_path: &std::ffi::OsStr,
) -> Command {
    let mut command = Command::new(cargo);
    command
        .current_dir(source)
        .args([
            "build",
            "--locked",
            "--bin",
            "riwork-remote",
            "--manifest-path",
        ])
        .arg(source.join("remote/Cargo.toml"))
        .env("CARGO_TARGET_DIR", target)
        .env("PATH", tool_path);
    if profile == "release" {
        command.arg("--release");
    }
    command
}
fn validate_companion(bundle: &Path, required: bool) -> Result<(), String> {
    if required {
        let path = bundle.join("Contents/MacOS/riwork-remote");
        let metadata = fs::metadata(&path)
            .map_err(|e| format!("Missing staged remote companion {}: {e}", path.display()))?;
        if !metadata.is_file() || metadata.len() == 0 {
            return Err("Staged remote companion must be a nonempty executable".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o111 == 0 {
                return Err("Staged remote companion is not executable".into());
            }
        }
    }
    Ok(())
}

fn stage_bundle_inputs(source: &Path, staging: &Path) -> Result<PathBuf, String> {
    for relative in [
        "scripts/bundle-macos.sh",
        "assets/app-icon/RiWork-legacy.icns",
    ] {
        let destination = staging.join(relative);
        fs::create_dir_all(destination.parent().unwrap())
            .map_err(|error| format!("Cannot create staging directory for {relative}: {error}"))?;
        fs::copy(source.join(relative), &destination)
            .map_err(|error| format!("Cannot stage {relative}: {error}"))?;
    }
    Ok(staging.join("scripts/bundle-macos.sh"))
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
        if let Some(signal) = interrupted() {
            return Err(interrupted_message(signal, None));
        }
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

/// The identity an update signs the bundle with. An ad-hoc signature names one
/// build's code hash, so macOS takes every update for a new app: it asks again
/// for the folders a terminal starts in (Documents, Desktop, Downloads), and the
/// replacement's terminal waits on that question until the reload gives up. A
/// certificate keeps one designated requirement across builds, so an answer
/// given once holds. `CODESIGN_IDENTITY` decides when set (`-` is ad hoc);
/// otherwise a Developer ID or Apple Development identity in the keychain is
/// used, and without one the bundle stays ad hoc.
fn update_signing_identity() -> Option<SigningIdentity> {
    if let Some(chosen) = env::var("CODESIGN_IDENTITY")
        .ok()
        .filter(|chosen| !chosen.trim().is_empty())
    {
        return (chosen != "-").then(|| SigningIdentity {
            sign_with: chosen.clone(),
            name: chosen,
        });
    }
    if !cfg!(target_os = "macos") {
        return None;
    }
    let output = Command::new("/usr/bin/security")
        .args(["find-identity", "-v", "-p", "codesigning"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    pick_signing_identity(&String::from_utf8_lossy(&output.stdout))
}

/// The identity to sign with from `security find-identity -v -p codesigning`:
/// a Developer ID Application identity before an Apple Development one, the
/// first listed of each. It is named by its SHA-1 hash, which stays unambiguous
/// when a renewed certificate shares the old one's name.
fn pick_signing_identity(listing: &str) -> Option<SigningIdentity> {
    let identities: Vec<SigningIdentity> = listing
        .lines()
        .filter_map(|line| {
            let (_, rest) = line.trim().split_once(") ")?;
            let (hash, quoted) = rest.split_once(' ')?;
            let name = quoted.trim().strip_prefix('"')?.strip_suffix('"')?;
            (hash.len() == 40 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())).then(|| {
                SigningIdentity {
                    sign_with: hash.to_owned(),
                    name: name.to_owned(),
                }
            })
        })
        .collect();
    ["Developer ID Application: ", "Apple Development: "]
        .iter()
        .find_map(|prefix| {
            identities
                .iter()
                .find(|identity| identity.name.starts_with(prefix))
                .cloned()
        })
}

/// The Zig 0.16 toolchain kept under the RiWork data directory, for the
/// Ghostty build, unless `ZIG` is set. Homebrew's Zig is often another version,
/// which fails that build, and a toolchain unpacked under /tmp loses its
/// standard library to the system's periodic cleanup.
fn update_zig() -> Option<PathBuf> {
    if env::var_os("ZIG").is_some_and(|zig| !zig.is_empty()) {
        return None;
    }
    find_zig(&crate::paths::riwork_home().ok()?.join("toolchains"))
}

/// The newest `zig-0.16*/zig` in `toolchains` whose standard library is
/// present.
fn find_zig(toolchains: &Path) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(toolchains)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("zig-0.16"))
        .map(|entry| entry.path())
        .filter(|dir| dir.join("zig").is_file() && dir.join("lib/std/std.zig").is_file())
        .map(|dir| dir.join("zig"))
        .collect();
    found.sort();
    found.pop()
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
    if let Some(signal) = interrupted() {
        return Err(interrupted_message(signal, Some(log_path)));
    }
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
        // The stage runs in its own process group, so the terminal's signal
        // reaches only this process. Stop the stage here or it would outlive us.
        if let Some(signal) = interrupted() {
            eprintln!("Interrupted. Stopping {name} and removing the staging directory.");
            terminate_stage(&mut child);
            return Err(interrupted_message(signal, Some(log_path)));
        }
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

/// SIGTERM to the stage's whole group lets cargo and rustc exit; SIGKILL
/// follows for anything that ignores it.
fn terminate_stage(child: &mut Child) {
    #[cfg(unix)]
    {
        signal_group(child.id(), libc::SIGTERM);
        let deadline = Instant::now() + STAGE_TERMINATE_GRACE;
        while Instant::now() < deadline && matches!(child.try_wait(), Ok(None)) {
            thread::sleep(Duration::from_millis(20));
        }
        signal_group(child.id(), libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(unix)]
fn signal_group(leader: u32, signal: libc::c_int) {
    if let Ok(leader) = libc::pid_t::try_from(leader) {
        // Stages run with `process_group(0)`, so the group id is the pid.
        unsafe { libc::kill(-leader, signal) };
    }
}

/// Run a staged binary the way any user first would. `help` prints and exits
/// without opening a window or reading state, so a nonzero exit, a crash or
/// a hang means the build is unusable.
fn smoke_test(
    executable: &Path,
    staging: &Path,
    log: &mut File,
    log_path: &Path,
) -> Result<(), String> {
    let home = staging.join("smoke-home");
    fs::create_dir_all(&home).map_err(|error| format!("Cannot prepare smoke test: {error}"))?;
    let mut command = Command::new(executable);
    command
        .arg("help")
        .env_clear()
        .env("HOME", &home)
        .env("RIWORK_HOME", &home)
        .env("PATH", "/usr/bin:/bin");
    run_stage(
        &mut command,
        &format!("Smoke test: {} help", executable.display()),
        SMOKE_TIMEOUT,
        log,
        log_path,
    )
    .map_err(|error| {
        format!(
            "The new build does not run, so it was not installed and the current build is unchanged. {error}"
        )
    })
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
        "Contents/Resources/RiWork.icns",
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

    /// Keep the directory: after a failed install it holds the previous build.
    fn preserve(&mut self) {
        self.cleanup = false;
        let _ = fs::write(self.path.join(PRESERVED_MARKER), "");
    }
}
impl Drop for StagingDirectory {
    fn drop(&mut self) {
        if self.cleanup {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

/// Set by the signal handler, polled by every wait in an update. A handler may
/// do nothing more than this, so the stage is stopped and staging removed from
/// ordinary code as the update unwinds.
static INTERRUPT: AtomicI32 = AtomicI32::new(0);

fn interrupted() -> Option<i32> {
    match INTERRUPT.load(Ordering::SeqCst) {
        0 => None,
        signal => Some(signal),
    }
}

fn interrupted_message(signal: i32, log_path: Option<&Path>) -> String {
    #[cfg(unix)]
    let name = match signal {
        libc::SIGINT => "SIGINT",
        libc::SIGTERM => "SIGTERM",
        libc::SIGHUP => "SIGHUP",
        _ => "a signal",
    };
    #[cfg(not(unix))]
    let name = {
        let _ = signal;
        "a signal"
    };
    let mut message = format!("Update interrupted by {name}. The installed build was not changed.");
    if let Some(log_path) = log_path {
        message.push_str(&format!(" Log: {}", log_path.display()));
    }
    message
}

/// Routes SIGINT, SIGTERM and SIGHUP into `INTERRUPT` for the length of an
/// update and restores the previous handlers afterwards. A signal that was
/// already ignored, as under `nohup`, stays ignored.
struct InterruptGuard {
    #[cfg(unix)]
    previous: Vec<(libc::c_int, libc::sigaction)>,
}

#[cfg(unix)]
extern "C" fn record_interrupt(signal: libc::c_int) {
    INTERRUPT.store(signal, Ordering::SeqCst);
}

#[cfg(unix)]
impl InterruptGuard {
    fn install() -> Self {
        INTERRUPT.store(0, Ordering::SeqCst);
        let mut previous = Vec::new();
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            // SAFETY: plain sigaction calls with zeroed, then filled, structures.
            // The handler only stores to an atomic, which is async-signal-safe.
            unsafe {
                let mut current: libc::sigaction = std::mem::zeroed();
                if libc::sigaction(signal, std::ptr::null(), &mut current) != 0
                    || current.sa_sigaction == libc::SIG_IGN
                {
                    continue;
                }
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction =
                    record_interrupt as extern "C" fn(libc::c_int) as libc::sighandler_t;
                libc::sigemptyset(&mut action.sa_mask);
                if libc::sigaction(signal, &action, std::ptr::null_mut()) == 0 {
                    previous.push((signal, current));
                }
            }
        }
        Self { previous }
    }
}

#[cfg(unix)]
impl Drop for InterruptGuard {
    fn drop(&mut self) {
        for (signal, action) in &self.previous {
            // SAFETY: restores the action that sigaction returned for this signal.
            unsafe { libc::sigaction(*signal, action, std::ptr::null_mut()) };
        }
        INTERRUPT.store(0, Ordering::SeqCst);
    }
}

#[cfg(not(unix))]
impl InterruptGuard {
    fn install() -> Self {
        Self {}
    }
}

/// Runs under the update lock, so no other update owns anything it finds. An
/// interrupted or killed update leaves its 1 GB staging clone, and every run
/// leaves a log. Returns how many staging directories and logs were removed.
fn sweep_stale_artifacts(target: &Path) -> (usize, usize) {
    let Ok(entries) = fs::read_dir(target) else {
        return (0, 0);
    };
    let mut staging = 0;
    let mut logs = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let (Some(name), Ok(kind)) = (name.to_str(), entry.file_type()) else {
            continue;
        };
        if kind.is_dir()
            && name
                .strip_prefix(".riwork-update-")
                .is_some_and(|id| Uuid::parse_str(id).is_ok())
        {
            let preserved = fs::metadata(entry.path().join(PRESERVED_MARKER))
                .map(|marker| {
                    marker
                        .modified()
                        .ok()
                        .and_then(|modified| modified.elapsed().ok())
                        .is_none_or(|age| age < PRESERVED_RETENTION)
                })
                .unwrap_or(false);
            if !preserved && fs::remove_dir_all(entry.path()).is_ok() {
                staging += 1;
            }
        } else if kind.is_file()
            && name
                .strip_prefix("riwork-update-")
                .and_then(|rest| rest.strip_suffix(".log"))
                .is_some_and(|id| Uuid::parse_str(id).is_ok())
            && let Ok(modified) = entry.metadata().and_then(|metadata| metadata.modified())
        {
            logs.push((modified, entry.path()));
        }
    }
    logs.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    let removed = logs
        .into_iter()
        .skip(KEPT_LOGS)
        .filter(|(_, path)| fs::remove_file(path).is_ok())
        .count();
    (staging, removed)
}

fn previous_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(".previous");
    path.with_file_name(name)
}

/// After the swap the staging paths hold the builds that were just replaced.
/// Keep them beside the new ones for rollback, replacing the set kept by the
/// previous update. Failing to keep them never fails an install that succeeded.
fn retain_previous(
    staged_executable: &Path,
    staged_bundle: &Path,
    executable: &Path,
    bundle: &Path,
    log: &mut File,
) -> Option<PathBuf> {
    let mut previous_bundle = None;
    for (replaced, installed) in [(staged_executable, executable), (staged_bundle, bundle)] {
        if fs::symlink_metadata(replaced).is_err() {
            continue;
        }
        let previous = previous_path(installed);
        let kept = match fs::symlink_metadata(&previous) {
            Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(&previous),
            Ok(_) => fs::remove_file(&previous),
            Err(_) => Ok(()),
        }
        .and_then(|()| fs::rename(replaced, &previous));
        match kept {
            Ok(()) if installed == bundle => previous_bundle = Some(previous),
            Ok(()) => {}
            Err(error) => {
                writeln!(
                    log,
                    "Could not keep the previous build at {}: {error}",
                    previous.display()
                )
                .ok();
            }
        }
    }
    previous_bundle
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    #[test]
    fn the_kept_zig_toolchain_is_found_only_when_complete() {
        let temporary = StagingDirectory::new(
            env::temp_dir().join(format!("riwork-update-zig-{}", Uuid::new_v4())),
        )
        .unwrap();
        let toolchains = temporary.path.as_path();
        assert_eq!(find_zig(toolchains), None, "nothing kept");
        let broken = toolchains.join("zig-0.16.0");
        fs::create_dir_all(broken.join("lib/std")).unwrap();
        fs::write(broken.join("zig"), "").unwrap();
        assert_eq!(
            find_zig(toolchains),
            None,
            "a toolchain whose standard library was cleaned away is no use"
        );
        fs::write(broken.join("lib/std/std.zig"), "").unwrap();
        let other = toolchains.join("zig-0.14.0");
        fs::create_dir_all(other.join("lib/std")).unwrap();
        fs::write(other.join("zig"), "").unwrap();
        fs::write(other.join("lib/std/std.zig"), "").unwrap();
        assert_eq!(find_zig(toolchains), Some(broken.join("zig")));
    }

    #[test]
    fn a_certificate_is_preferred_to_an_ad_hoc_signature() {
        let development = "A".repeat(40);
        let developer_id = "B".repeat(40);
        let listing = format!(
            "  1) {development} \"Apple Development: Someone (K94D56Z2AA)\"\n  2) {developer_id} \"Developer ID Application: Someone (ZR7A22CNVY)\"\n     2 valid identities found\n"
        );
        assert_eq!(
            pick_signing_identity(&listing),
            Some(SigningIdentity {
                sign_with: developer_id,
                name: "Developer ID Application: Someone (ZR7A22CNVY)".to_owned(),
            }),
            "Developer ID first"
        );
        let only_development = format!(
            "  1) {development} \"Apple Development: Someone (K94D56Z2AA)\"\n     1 valid identities found\n"
        );
        assert_eq!(
            pick_signing_identity(&only_development).map(|identity| identity.sign_with),
            Some(development),
            "named by its hash, which a renewed certificate with the same name does not share"
        );
    }

    #[test]
    fn without_a_usable_certificate_the_bundle_stays_ad_hoc() {
        for listing in [
            "     0 valid identities found\n",
            "",
            &format!(
                "  1) {} \"Apple Distribution: Someone (ZR7A22CNVY)\"\n",
                "C".repeat(40)
            ),
            "  1) not-a-hash \"Apple Development: Someone (K94D56Z2AA)\"\n",
        ] {
            assert_eq!(pick_signing_identity(listing), None, "{listing:?}");
        }
    }

    fn fixture(parent: &Path, name: &str) -> PathBuf {
        let source = parent.join(name);
        fs::create_dir_all(source.join("src")).unwrap();
        fs::create_dir_all(source.join("scripts")).unwrap();
        fs::create_dir_all(source.join("assets/app-icon")).unwrap();
        fs::write(
            source.join("Cargo.toml"),
            "[package]\nname = \"riwork\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::write(source.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(source.join("scripts/bundle-macos.sh"), "#!/bin/sh\n").unwrap();
        fs::write(
            source.join("assets/app-icon/RiWork-legacy.icns"),
            b"legacy icon fixture",
        )
        .unwrap();
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
    fn source_validation_rejects_another_package_and_missing_bundle_inputs() {
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
        fs::write(source.join("scripts/bundle-macos.sh"), "#!/bin/sh\n").unwrap();
        // The bundler reads only the rounded icon; the full-bleed export is optional.
        let relative = "assets/app-icon/RiWork-legacy.icns";
        fs::remove_file(source.join(relative)).unwrap();
        assert!(validate_source(&source).unwrap_err().contains(relative));
        fs::write(source.join(relative), b"icon fixture").unwrap();
        assert!(!source.join("assets/app-icon/RiWork.icns").exists());
        validate_source(&source).unwrap();
        for profile in ["../release", "release; false", "", "/tmp", "custom"] {
            assert!(update_profile(Some(profile)).is_err());
        }
        assert_eq!(update_profile(Some("dev")).unwrap(), "debug");
        assert_eq!(update_profile(Some("debug")).unwrap(), "debug");
        assert_eq!(update_profile(Some("release")).unwrap(), "release");
        // Updates optimize the app even when invoked by a development CLI.
        assert_eq!(update_profile(None).unwrap(), "release");
    }

    #[test]
    fn bundle_staging_transports_the_bundled_icon_and_validation_requires_it() {
        let temporary = StagingDirectory::new(
            env::temp_dir().join(format!("riwork-update-icon-{}", Uuid::new_v4())),
        )
        .unwrap();
        let source = fixture(&temporary.path, "source");
        let staging = temporary.path.join("staging");
        // A full-bleed export present in the checkout is not a bundler input.
        fs::write(source.join("assets/app-icon/RiWork.icns"), b"icon fixture").unwrap();
        let bundler = stage_bundle_inputs(&source, &staging).unwrap();
        assert_eq!(fs::read_to_string(bundler).unwrap(), "#!/bin/sh\n");
        assert!(!staging.join("assets/app-icon/RiWork.icns").exists());
        assert_eq!(
            fs::read(staging.join("assets/app-icon/RiWork-legacy.icns")).unwrap(),
            b"legacy icon fixture"
        );

        let executable = staging.join("riwork");
        let bundle = staging.join("RiWork.app");
        fs::write(&executable, "executable fixture").unwrap();
        for relative in [
            "Contents/MacOS/riwork",
            "Contents/Info.plist",
            "Contents/Resources/terminfo/78/xterm-ghostty",
            "Contents/Resources/ghostty/shell-integration/zsh/ghostty-integration",
        ] {
            let file = bundle.join(relative);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, "fixture").unwrap();
        }
        assert!(
            validate_artifacts(&executable, &bundle)
                .unwrap_err()
                .contains("Contents/Resources/RiWork.icns")
        );
        fs::copy(
            staging.join("assets/app-icon/RiWork-legacy.icns"),
            bundle.join("Contents/Resources/RiWork.icns"),
        )
        .unwrap();
        validate_artifacts(&executable, &bundle).unwrap();
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

    #[test]
    fn companion_build_uses_staged_target_and_bundle_validation_fails_closed() {
        let temporary = StagingDirectory::new(
            env::temp_dir().join(format!("riwork-update-companion-{}", Uuid::new_v4())),
        )
        .unwrap();
        let source = temporary.path.join("source with spaces");
        let target = temporary.path.join("staging/remote/target");
        for profile in ["debug", "release"] {
            let command = companion_build_command(
                Path::new("/test/cargo"),
                &source,
                &target,
                profile,
                std::ffi::OsStr::new("/test/bin"),
            );
            let args = command
                .get_args()
                .map(|a| a.to_string_lossy().to_string())
                .collect::<Vec<_>>();
            assert_eq!(
                &args[..5],
                [
                    "build",
                    "--locked",
                    "--bin",
                    "riwork-remote",
                    "--manifest-path"
                ]
            );
            assert_eq!(args[5], source.join("remote/Cargo.toml").to_string_lossy());
            assert_eq!(args.iter().any(|a| a == "--release"), profile == "release");
            assert_eq!(command.get_current_dir(), Some(source.as_path()));
            assert!(
                command
                    .get_envs()
                    .any(|(k, v)| k == "CARGO_TARGET_DIR" && v == Some(target.as_os_str()))
            );
        }
        let bundle = temporary.path.join("RiWork.app");
        validate_companion(&bundle, false).unwrap();
        assert!(validate_companion(&bundle, true).is_err());
        let binary = bundle.join("Contents/MacOS/riwork-remote");
        fs::create_dir_all(binary.parent().unwrap()).unwrap();
        fs::write(&binary, "companion").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert!(validate_companion(&bundle, true).is_err());
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        }
        validate_companion(&bundle, true).unwrap();
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

    fn log_file(path: &Path) -> File {
        OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(path)
            .unwrap()
    }

    fn write_executable(path: &Path, body: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[cfg(unix)]
    fn wait_until_gone(pid: libc::pid_t) -> bool {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if unsafe { libc::kill(pid, 0) } != 0 {
                return true;
            }
            thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[cfg(unix)]
    #[test]
    fn an_interrupt_stops_the_running_stage_and_everything_it_started() {
        let temporary = StagingDirectory::new(
            env::temp_dir().join(format!("riwork-update-interrupt-{}", Uuid::new_v4())),
        )
        .unwrap();
        let log_path = temporary.path.join("build.log");
        let mut log = log_file(&log_path);
        let pid_file = temporary.path.join("descendant.pid");
        let script = temporary.path.join("stage.sh");
        // Like cargo, the stage runs children of its own in its process group.
        write_executable(
            &script,
            &format!(
                "sleep 60 &\necho $! > {}.tmp\nmv {0}.tmp {0}\nwait",
                pid_file.display()
            ),
        );
        let guard = InterruptGuard::install();
        let signaller = {
            let pid_file = pid_file.clone();
            thread::spawn(move || {
                while !pid_file.exists() {
                    thread::sleep(Duration::from_millis(10));
                }
                // SIGTERM rather than Ctrl-C's SIGINT: a shell starts background
                // jobs, and so possibly this test, with SIGINT ignored.
                unsafe { libc::kill(libc::getpid(), libc::SIGTERM) };
            })
        };
        let start = Instant::now();
        let error = run_stage(
            Command::new("/bin/sh").arg(&script),
            "fixture build",
            Duration::from_secs(60),
            &mut log,
            &log_path,
        )
        .unwrap_err();
        signaller.join().unwrap();
        assert!(error.contains("interrupted by SIGTERM"), "{error}");
        assert!(error.contains("installed build was not changed"), "{error}");
        assert!(start.elapsed() < Duration::from_secs(4));
        let descendant = fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse::<libc::pid_t>()
            .unwrap();
        assert!(wait_until_gone(descendant), "the stage's child survived");
        // Later stages do not start once an update was interrupted.
        let error = run_stage(
            Command::new("/bin/sh").args(["-c", "touch never-runs"]),
            "later stage",
            Duration::from_secs(5),
            &mut log,
            &log_path,
        )
        .unwrap_err();
        assert!(error.contains("interrupted"), "{error}");
        assert!(
            matches!(update_lock(&temporary.path), Err(error) if error.contains("interrupted"))
        );
        drop(guard);
        assert_eq!(interrupted(), None);
        for (signal, name) in [
            (libc::SIGINT, "SIGINT"),
            (libc::SIGTERM, "SIGTERM"),
            (libc::SIGHUP, "SIGHUP"),
        ] {
            assert!(interrupted_message(signal, None).contains(name));
        }
    }

    #[cfg(unix)]
    #[test]
    fn interrupt_handlers_are_restored_and_an_ignored_signal_stays_ignored() {
        fn disposition(signal: libc::c_int) -> libc::sighandler_t {
            unsafe {
                let mut current: libc::sigaction = std::mem::zeroed();
                libc::sigaction(signal, std::ptr::null(), &mut current);
                current.sa_sigaction
            }
        }
        let before = [
            disposition(libc::SIGINT),
            disposition(libc::SIGTERM),
            disposition(libc::SIGHUP),
        ];
        {
            let _guard = InterruptGuard::install();
            let handler = record_interrupt as extern "C" fn(libc::c_int) as libc::sighandler_t;
            for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
                if before[[libc::SIGINT, libc::SIGTERM, libc::SIGHUP]
                    .iter()
                    .position(|candidate| *candidate == signal)
                    .unwrap()]
                    != libc::SIG_IGN
                {
                    assert_eq!(disposition(signal), handler);
                }
            }
        }
        assert_eq!(
            before,
            [
                disposition(libc::SIGINT),
                disposition(libc::SIGTERM),
                disposition(libc::SIGHUP)
            ]
        );
        // `nohup riwork update` must keep surviving a closed terminal.
        unsafe { libc::signal(libc::SIGHUP, libc::SIG_IGN) };
        {
            let _guard = InterruptGuard::install();
            assert_eq!(disposition(libc::SIGHUP), libc::SIG_IGN);
        }
        unsafe { libc::signal(libc::SIGHUP, before[2]) };
        assert_eq!(disposition(libc::SIGHUP), before[2]);
    }

    #[test]
    fn stale_staging_and_old_logs_are_swept_but_other_files_and_kept_staging_are_not() {
        let temporary = StagingDirectory::new(
            env::temp_dir().join(format!("riwork-update-sweep-{}", Uuid::new_v4())),
        )
        .unwrap();
        let target = &temporary.path;
        let stale = target.join(format!(".riwork-update-{}", Uuid::new_v4()));
        fs::create_dir_all(stale.join("target/release")).unwrap();
        fs::write(stale.join("target/release/big"), "clone").unwrap();
        let preserved = target.join(format!(".riwork-update-{}", Uuid::new_v4()));
        fs::create_dir(&preserved).unwrap();
        fs::write(preserved.join(PRESERVED_MARKER), "").unwrap();
        let abandoned = target.join(format!(".riwork-update-{}", Uuid::new_v4()));
        fs::create_dir(&abandoned).unwrap();
        let marker = File::create(abandoned.join(PRESERVED_MARKER)).unwrap();
        marker
            .set_modified(SystemTime::now() - PRESERVED_RETENTION - Duration::from_secs(60))
            .unwrap();
        let unrelated = [
            target.join(".riwork-update.lock"),
            target.join(".riwork-update-notes"),
            target.join("riwork-update-notes.log"),
            target.join("cargo.log"),
        ];
        for path in &unrelated {
            fs::write(path, "keep").unwrap();
        }
        let mut logs = Vec::new();
        for age in 0..9u64 {
            let path = target.join(format!("riwork-update-{}.log", Uuid::new_v4()));
            let file = File::create(&path).unwrap();
            file.set_modified(SystemTime::now() - Duration::from_secs(60 * (age + 1)))
                .unwrap();
            logs.push(path);
        }
        assert_eq!(sweep_stale_artifacts(target), (2, 9 - KEPT_LOGS));
        assert!(!stale.exists() && !abandoned.exists());
        assert!(preserved.exists(), "recovery material is kept for a week");
        assert!(unrelated.iter().all(|path| path.exists()));
        let (newest, oldest) = logs.split_at(KEPT_LOGS);
        assert!(newest.iter().all(|path| path.exists()));
        assert!(oldest.iter().all(|path| !path.exists()));
        assert_eq!(sweep_stale_artifacts(target), (0, 0));
        assert_eq!(sweep_stale_artifacts(&target.join("missing")), (0, 0));
    }

    #[test]
    fn the_replaced_build_is_kept_as_previous_until_the_next_update() {
        let temporary = StagingDirectory::new(
            env::temp_dir().join(format!("riwork-update-previous-{}", Uuid::new_v4())),
        )
        .unwrap();
        let root = &temporary.path;
        let log_path = root.join("log");
        let mut log = log_file(&log_path);
        let staged_executable = root.join("staged/riwork");
        let staged_bundle = root.join("staged/RiWork.app");
        let executable = root.join("release/riwork");
        let bundle = root.join("release/RiWork.app");
        fs::create_dir_all(staged_bundle.parent().unwrap()).unwrap();
        fs::create_dir_all(&bundle).unwrap();
        // The state right after install_artifacts swapped the names.
        fs::write(&staged_executable, "generation 1 executable").unwrap();
        fs::create_dir(&staged_bundle).unwrap();
        fs::write(staged_bundle.join("version"), "generation 1 bundle").unwrap();
        fs::write(&executable, "generation 2 executable").unwrap();
        fs::write(bundle.join("version"), "generation 2 bundle").unwrap();
        // An older rollback set is superseded.
        fs::create_dir(root.join("release/RiWork.app.previous")).unwrap();
        fs::write(
            root.join("release/RiWork.app.previous/version"),
            "generation 0",
        )
        .unwrap();
        fs::write(root.join("release/riwork.previous"), "generation 0").unwrap();
        let previous = retain_previous(
            &staged_executable,
            &staged_bundle,
            &executable,
            &bundle,
            &mut log,
        );
        assert_eq!(previous, Some(root.join("release/RiWork.app.previous")));
        assert_eq!(
            fs::read_to_string(root.join("release/riwork.previous")).unwrap(),
            "generation 1 executable"
        );
        assert_eq!(
            fs::read_to_string(root.join("release/RiWork.app.previous/version")).unwrap(),
            "generation 1 bundle"
        );
        assert_eq!(
            fs::read_to_string(&executable).unwrap(),
            "generation 2 executable"
        );
        assert!(!staged_executable.exists() && !staged_bundle.exists());
        // A first install has nothing to keep and leaves the existing set alone.
        let previous = retain_previous(
            &staged_executable,
            &staged_bundle,
            &executable,
            &bundle,
            &mut log,
        );
        assert_eq!(previous, None);
        assert!(root.join("release/riwork.previous").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_build_that_does_not_start_is_rejected_before_installation() {
        let temporary = StagingDirectory::new(
            env::temp_dir().join(format!("riwork-update-smoke-{}", Uuid::new_v4())),
        )
        .unwrap();
        let log_path = temporary.path.join("build.log");
        let mut log = log_file(&log_path);
        let good = temporary.path.join("good/riwork");
        write_executable(
            &good,
            "[ \"$1\" = help ] && [ -z \"$GH_TOKEN\" ] && echo usage",
        );
        smoke_test(&good, &temporary.path, &mut log, &log_path).unwrap();
        for (name, body) in [
            ("exits", "exit 3"),
            ("crashes", "kill -SEGV $$"),
            ("aborts", "echo 'dyld: Library not loaded' >&2; exit 134"),
        ] {
            let bad = temporary.path.join(name).join("riwork");
            write_executable(&bad, body);
            let error = smoke_test(&bad, &temporary.path, &mut log, &log_path).unwrap_err();
            assert!(error.contains("not installed"), "{name}: {error}");
            assert!(
                error.contains("current build is unchanged"),
                "{name}: {error}"
            );
        }
        assert!(
            smoke_test(
                &temporary.path.join("missing/riwork"),
                &temporary.path,
                &mut log,
                &log_path
            )
            .is_err()
        );
    }

    /// A checkout whose cargo and bundler are fakes, so the complete update
    /// pipeline runs in a second without building anything.
    #[cfg(target_os = "macos")]
    fn pipeline_fixture(temporary: &Path) -> (PathBuf, UpdateEnvironment) {
        let source = fixture(temporary, "pipeline source");
        write_executable(
            &source.join("scripts/bundle-macos.sh"),
            "set -eu\nbundle=target/$1/RiWork.app\nmkdir -p $bundle/Contents/MacOS $bundle/Contents/Resources/terminfo/78 $bundle/Contents/Resources/ghostty/shell-integration/zsh\ncp target/$1/riwork $bundle/Contents/MacOS/riwork\ncp assets/app-icon/RiWork-legacy.icns $bundle/Contents/Resources/RiWork.icns\ntouch $bundle/Contents/Info.plist $bundle/Contents/Resources/terminfo/78/xterm-ghostty $bundle/Contents/Resources/ghostty/shell-integration/zsh/ghostty-integration",
        );
        let tools = temporary.join("tools");
        write_executable(
            &tools.join("cargo"),
            "mkdir -p \"$CARGO_TARGET_DIR/debug\"\nmode=$(cat build-mode)\nprintf '#!/bin/sh\\n%s\\n' \"$mode\" > \"$CARGO_TARGET_DIR/debug/riwork\"\nchmod +x \"$CARGO_TARGET_DIR/debug/riwork\"",
        );
        let tool_path =
            env::join_paths([tools, PathBuf::from("/usr/bin"), PathBuf::from("/bin")]).unwrap();
        (
            source,
            UpdateEnvironment {
                tool_path,
                verify_signature: false,
                codesign_identity: None,
                zig: None,
            },
        )
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn updates_install_only_builds_that_run_and_keep_the_previous_one() {
        let temporary = StagingDirectory::new(
            env::temp_dir().join(format!("riwork-update-pipeline-{}", Uuid::new_v4())),
        )
        .unwrap();
        let (source, environment) = pipeline_fixture(&temporary.path);
        let source = source.canonicalize().unwrap();
        let build_mode = |mode: &str| fs::write(source.join("build-mode"), mode).unwrap();
        let installed = |name: &str| {
            fs::read_to_string(source.join("target/debug").join(name)).unwrap_or_default()
        };
        // Leftovers of an interrupted update and of earlier runs.
        fs::create_dir_all(source.join("target")).unwrap();
        let stale = source.join(format!("target/.riwork-update-{}", Uuid::new_v4()));
        fs::create_dir_all(stale.join("target")).unwrap();
        for age in 0..8u64 {
            let log =
                File::create(source.join(format!("target/riwork-update-{}.log", Uuid::new_v4())))
                    .unwrap();
            log.set_modified(SystemTime::now() - Duration::from_secs(3600 * (age + 1)))
                .unwrap();
        }

        build_mode("echo generation one");
        let first = build_update_with(&source, Some("debug"), &environment).unwrap();
        assert!(!stale.exists(), "interrupted staging is swept");
        assert_eq!(first.previous_bundle, None);
        assert!(installed("riwork").contains("generation one"));
        let logs = |source: &Path| {
            fs::read_dir(source.join("target"))
                .unwrap()
                .flatten()
                .filter(|entry| entry.file_name().to_string_lossy().ends_with(".log"))
                .count()
        };
        assert_eq!(logs(&source), KEPT_LOGS + 1);

        build_mode("echo generation two");
        let second = build_update_with(&source, Some("debug"), &environment).unwrap();
        assert!(installed("riwork").contains("generation two"));
        assert!(installed("RiWork.app/Contents/MacOS/riwork").contains("generation two"));
        assert_eq!(
            second.previous_bundle,
            Some(source.join("target/debug/RiWork.app.previous"))
        );
        assert!(installed("riwork.previous").contains("generation one"));
        assert!(installed("RiWork.app.previous/Contents/MacOS/riwork").contains("generation one"));

        // Compiles, but fails at startup: nothing is replaced and nothing is left behind.
        build_mode("exit 1");
        let error = build_update_with(&source, Some("debug"), &environment).unwrap_err();
        assert!(error.contains("does not run"), "{error}");
        assert!(installed("riwork").contains("generation two"));
        assert!(installed("RiWork.app/Contents/MacOS/riwork").contains("generation two"));
        assert!(installed("riwork.previous").contains("generation one"));
        assert!(
            fs::read_dir(source.join("target"))
                .unwrap()
                .flatten()
                .all(|entry| !entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".riwork-update-")
                    || entry.file_name().to_string_lossy() == ".riwork-update.lock")
        );

        build_mode("echo generation three");
        build_update_with(&source, Some("debug"), &environment).unwrap();
        assert!(installed("riwork.previous").contains("generation two"));
    }
}
