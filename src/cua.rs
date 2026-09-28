//! RiWork's shared Cua.ai Driver installation and app-owned desktop runtime.

use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use fs2::FileExt;
use serde::Serialize;
use serde_json::Value;

const INSTALLER_URL: &str = "https://cua.ai/driver/install.sh";
const APP_PATH: &str = "/Applications/CuaDriver.app";
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const OUTPUT_LIMIT: u64 = 16 * 1024;

#[derive(Clone, Debug, Serialize)]
pub struct CuaStatus {
    pub installed: bool,
    pub driver_path: Option<PathBuf>,
    pub version: Option<String>,
    pub accessibility: bool,
    pub screen_recording: bool,
    pub direct_capture_verified: bool,
    pub running: bool,
    pub ready: bool,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct CuaManager {
    home: PathBuf,
    override_driver: Option<PathBuf>,
    user_home: Option<PathBuf>,
    search_path: Vec<PathBuf>,
}

impl CuaManager {
    pub fn open_default() -> Result<Self, String> {
        let home = match env::var_os("RIWORK_HOME") {
            Some(path) => PathBuf::from(path),
            None => PathBuf::from(env::var_os("HOME").ok_or("HOME is not set; set RIWORK_HOME")?)
                .join(".local/share/riwork"),
        };
        Self::at(home)
    }

    pub fn at(home: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(&home)
            .map_err(|error| format!("Cannot create {}: {error}", home.display()))?;
        let home = home
            .canonicalize()
            .map_err(|error| format!("Cannot resolve {}: {error}", home.display()))?;
        Ok(Self {
            home,
            override_driver: env::var_os("RIWORK_CUA_DRIVER").map(PathBuf::from),
            user_home: env::var_os("HOME").map(PathBuf::from),
            search_path: env::var_os("PATH")
                .map(|value| env::split_paths(&value).collect())
                .unwrap_or_default(),
        })
    }

    /// Resolve an installed executable without downloading or changing agent configuration.
    pub fn driver_path(&self) -> Result<PathBuf, String> {
        self.find_driver()?.ok_or_else(|| {
            "Cua Driver is not installed. Open RiWork Settings → Computer Use and select Set up Cua, or run `riwork setup`.".to_owned()
        })
    }

    fn find_driver(&self) -> Result<Option<PathBuf>, String> {
        if let Some(path) = &self.override_driver {
            return executable_path(path).map(Some).ok_or_else(|| {
                format!(
                    "RIWORK_CUA_DRIVER is not an executable file: {}",
                    path.display()
                )
            });
        }
        let mut candidates = vec![self.home.join("cua/bin/cua-driver")];
        if let Some(home) = &self.user_home {
            candidates.push(home.join(".local/bin/cua-driver"));
        }
        candidates.push(PathBuf::from(APP_PATH).join("Contents/MacOS/cua-driver"));
        candidates.extend(self.search_path.iter().map(|dir| dir.join("cua-driver")));
        Ok(candidates.iter().find_map(|path| executable_path(path)))
    }

    pub fn status(&self) -> Result<CuaStatus, String> {
        let Some(driver) = self.find_driver()? else {
            return Ok(CuaStatus {
                installed: false,
                driver_path: None,
                version: None,
                accessibility: false,
                screen_recording: false,
                direct_capture_verified: false,
                running: false,
                ready: false,
                message: "Cua Driver is not installed. Select Set up Cua or run `riwork setup`."
                    .to_owned(),
            });
        };
        let version = driver_version(&driver).ok();
        let running = daemon_running(&driver);
        #[cfg(target_os = "macos")]
        let permissions = run_probe(&driver, &["permissions", "status", "--json"])
            .and_then(|output| output.require_success("Read Cua Driver permissions"))
            .and_then(|output| parse_permissions(&output.stdout));
        #[cfg(not(target_os = "macos"))]
        let permissions: Result<PermissionStatus, String> = Ok(PermissionStatus {
            accessibility: true,
            screen_recording: true,
            direct_capture_verified: true,
            running: None,
            diagnostic: None,
        });
        let (accessibility, screen_recording, direct_capture_verified, running, diagnostic) =
            match permissions {
                Ok(permissions) => (
                    permissions.accessibility,
                    permissions.screen_recording,
                    permissions.direct_capture_verified,
                    permissions.running.unwrap_or(running),
                    permissions.diagnostic,
                ),
                Err(error) => (false, false, false, running, Some(error)),
            };
        let ready = version.is_some()
            && running
            && accessibility
            && screen_recording
            && direct_capture_verified;
        let message = if version.is_none() {
            "Cua Driver could not report its version. Run `riwork setup` to repair the installation.".to_owned()
        } else if let Some(diagnostic) = diagnostic {
            format!("Cua Driver permissions could not be checked: {diagnostic}")
        } else if !running {
            "Cua Driver is installed; its desktop service is not running.".to_owned()
        } else if !accessibility || !screen_recording {
            let missing = match (accessibility, screen_recording) {
                (false, false) => "Accessibility and Screen Recording",
                (false, true) => "Accessibility",
                _ => "Screen Recording",
            };
            format!(
                "Enable {missing} for CuaDriver in macOS System Settings. Select Open permissions or run `riwork cua permissions`."
            )
        } else if !direct_capture_verified {
            "macOS access is enabled. Select Verify screen capture or run `riwork cua permissions` to verify CuaDriver's screen capture access.".to_owned()
        } else {
            "Cua Driver is ready. RiWork harnesses share its desktop service.".to_owned()
        };
        Ok(CuaStatus {
            installed: true,
            driver_path: Some(driver),
            version,
            accessibility,
            screen_recording,
            direct_capture_verified,
            running,
            ready,
            message,
        })
    }

    pub fn setup(&self) -> Result<CuaStatus, String> {
        let _lock = self.lock("install.lock", Duration::from_secs(5))?;
        let installed = self.installed_driver_usable()?;
        #[cfg(target_os = "macos")]
        let installed = installed
            && (self.override_driver.is_some()
                || Path::new(APP_PATH)
                    .join("Contents/MacOS/cua-driver")
                    .is_file());
        if !installed {
            self.install()?;
        }
        self.driver_path()?;
        let executable = env::current_exe()
            .map_err(|error| format!("Cannot locate the RiWork executable: {error}"))?;
        self.ensure_harness_shims(&executable)?;
        self.ensure_started()?;
        self.status()
    }

    fn installed_driver_usable(&self) -> Result<bool, String> {
        let Some(driver) = self.find_driver()? else {
            return Ok(false);
        };
        match driver_version(&driver) {
            Ok(_) => Ok(true),
            Err(error) if self.override_driver.is_some() => Err(format!(
                "RIWORK_CUA_DRIVER cannot report its version: {error}"
            )),
            Err(_) => Ok(false),
        }
    }

    fn install(&self) -> Result<(), String> {
        let download = TemporaryDirectory::new(&self.home.join("cua"), "install")?;
        let script = download.path.join("install.sh");
        let mut curl = Command::new("/usr/bin/curl");
        curl.args([
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--connect-timeout",
            "15",
            "--max-time",
            "45",
            "--output",
        ])
        .arg(&script)
        .arg(INSTALLER_URL);
        run_bounded(&mut curl, Duration::from_secs(50))?
            .require_success("Download Cua Driver installer")?;
        let bin_dir = self.home.join("cua/bin");
        let mut install = Command::new("/bin/bash");
        install
            .arg(&script)
            .arg("--bin-dir")
            .arg(&bin_dir)
            .arg("--no-modify-path")
            .args(["--channel", "stable"])
            .env("CUA_DRIVER_RS_HOME", self.home.join("cua/package"))
            .env_remove("CUA_DRIVER_RS_VERSION")
            .env_remove("CUA_DRIVER_VERSION");
        run_bounded(&mut install, Duration::from_secs(300))?
            .require_success("Install Cua Driver")?;
        Ok(())
    }

    /// Start the app-owned daemon without permission prompts or downloads.
    pub fn ensure_started(&self) -> Result<(), String> {
        let driver = self.driver_path()?;
        if daemon_running(&driver) {
            return Ok(());
        }
        let _lock = self.lock("daemon.lock", Duration::from_secs(5))?;
        if daemon_running(&driver) {
            return Ok(());
        }
        #[cfg(target_os = "macos")]
        if self.override_driver.is_none() {
            if !Path::new(APP_PATH)
                .join("Contents/MacOS/cua-driver")
                .is_file()
            {
                return Err("CuaDriver.app is missing. Run `riwork setup` to install its signed desktop service.".to_owned());
            }
            let mut launch = Command::new("/usr/bin/open");
            launch.args([
                "-n",
                "-g",
                "-a",
                APP_PATH,
                "--args",
                "serve",
                "--permission-mode",
                "standard",
                "--no-permissions-gate",
            ]);
            run_bounded(&mut launch, PROBE_TIMEOUT)?.require_success("Start CuaDriver.app")?;
        } else {
            self.spawn_daemon(&driver)?;
        }
        #[cfg(not(target_os = "macos"))]
        self.spawn_daemon(&driver)?;
        wait_for_daemon(&driver)
    }

    fn spawn_daemon(&self, driver: &Path) -> Result<(), String> {
        let log = self.log_file("driver.log")?;
        let mut command = Command::new(driver);
        command
            .args(["serve", "--permission-mode", "standard"])
            .stdin(Stdio::null())
            .stdout(Stdio::from(
                log.try_clone().map_err(|error| error.to_string())?,
            ))
            .stderr(Stdio::from(log));
        #[cfg(target_os = "macos")]
        command.arg("--no-permissions-gate");
        detach_process(&mut command);
        let mut child = command
            .spawn()
            .map_err(|error| format!("Cannot start Cua Driver: {error}"))?;
        thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }

    /// Explicit user setup action: macOS owns consent; this only opens Cua's helper.
    pub fn request_permissions(&self) -> Result<CuaStatus, String> {
        self.ensure_started()?;
        let driver = self.driver_path()?;
        #[cfg(target_os = "macos")]
        if self.override_driver.is_none() && !self.status()?.ready {
            // Explicit setup may restart the shared service. Cua's normal
            // onboarding gate uses fresh helpers to avoid cached TCC denials.
            let _lock = self.lock("daemon.lock", Duration::from_secs(5))?;
            run_probe(&driver, &["stop"])?.require_success("Restart Cua permission onboarding")?;
            let mut launch = Command::new("/usr/bin/open");
            launch.args([
                "-n",
                "-g",
                "-a",
                APP_PATH,
                "--args",
                "serve",
                "--permission-mode",
                "standard",
            ]);
            run_bounded(&mut launch, PROBE_TIMEOUT)?
                .require_success("Open Cua permission onboarding")?;
            wait_for_daemon(&driver)?;
        }
        let log = self.log_file("permissions.log")?;
        let mut command = Command::new(driver);
        command
            .args(["permissions", "grant"])
            .stdin(Stdio::null())
            .stdout(Stdio::from(
                log.try_clone().map_err(|error| error.to_string())?,
            ))
            .stderr(Stdio::from(log));
        detach_process(&mut command);
        let mut child = command
            .spawn()
            .map_err(|error| format!("Cannot open Cua Driver permissions: {error}"))?;
        thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(300);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) | Err(_) => break,
                    Ok(None) if Instant::now() >= deadline => {
                        terminate_child(&mut child);
                        break;
                    }
                    Ok(None) => thread::sleep(Duration::from_millis(250)),
                }
            }
        });
        let mut status = self.status()?;
        status.message = "Cua Driver's permission helper is open. Approve the macOS prompts and enable CuaDriver under Accessibility and Screen Recording. Return to RiWork to check readiness.".to_owned();
        Ok(status)
    }

    /// Keep stdout exclusively for the MCP protocol.
    pub fn run_mcp(&self) -> Result<(), String> {
        self.ensure_started()?;
        let driver = self.driver_path()?;
        let mut command = Command::new(driver);
        command.arg("mcp");
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            Err(format!("Cannot launch Cua Driver MCP: {}", command.exec()))
        }
        #[cfg(not(unix))]
        {
            let status = command
                .status()
                .map_err(|error| format!("Cannot launch Cua Driver MCP: {error}"))?;
            if status.success() {
                Ok(())
            } else {
                Err(format!("Cua Driver MCP exited with {status}"))
            }
        }
    }

    pub fn ensure_harness_shims(&self, riwork_executable: &Path) -> Result<PathBuf, String> {
        let executable = executable_path(riwork_executable).ok_or_else(|| {
            format!(
                "RiWork is not an executable file: {}",
                riwork_executable.display()
            )
        })?;
        let quoted_executable = shell_quote(&executable.to_string_lossy());
        let bin = self.home.join("cua/harness-bin");
        fs::create_dir_all(&bin)
            .map_err(|error| format!("Cannot create Cua harness directory: {error}"))?;
        for harness in ["codex", "claude", "grok"] {
            let content = format!(
                "#!/bin/sh\n# RiWork Cua harness shim\nexec {quoted_executable} cua harness {harness} -- \"$@\"\n"
            );
            atomic_executable(&bin.join(harness), content.as_bytes())?;
        }
        Ok(bin)
    }

    fn log_file(&self, name: &str) -> Result<File, String> {
        let dir = self.home.join("cua");
        fs::create_dir_all(&dir)
            .map_err(|error| format!("Cannot create Cua directory: {error}"))?;
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(dir.join(name))
            .map_err(|error| format!("Cannot write Cua log: {error}"))
    }

    fn lock(&self, name: &str, timeout: Duration) -> Result<File, String> {
        let dir = self.home.join("cua");
        fs::create_dir_all(&dir)
            .map_err(|error| format!("Cannot create Cua directory: {error}"))?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join(name))
            .map_err(|error| format!("Cannot open Cua lock: {error}"))?;
        let deadline = Instant::now() + timeout;
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(file),
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(50))
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    return Err(
                        "Another Cua setup or startup is running. Retry when it finishes."
                            .to_owned(),
                    );
                }
                Err(error) => return Err(format!("Cannot lock Cua setup: {error}")),
            }
        }
    }
}

fn executable_path(path: &Path) -> Option<PathBuf> {
    let metadata = path.metadata().ok()?;
    if !metadata.is_file() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return None;
        }
    }
    path.canonicalize().ok()
}

fn daemon_running(driver: &Path) -> bool {
    run_probe(driver, &["status"])
        .is_ok_and(|result| result.status.success() && result.stdout.contains("daemon is running"))
}

fn wait_for_daemon(driver: &Path) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if daemon_running(driver) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }
    Err(
        "Cua Driver's desktop service did not start. Run `riwork cua status` and retry setup."
            .to_owned(),
    )
}

fn driver_version(driver: &Path) -> Result<String, String> {
    let output = run_probe(driver, &["--version"])?.require_success("Read Cua Driver version")?;
    let version = output.stdout.trim();
    if version.is_empty() {
        return Err("Cua Driver returned an empty version".to_owned());
    }
    Ok(version.to_owned())
}

#[derive(Debug, PartialEq)]
struct PermissionStatus {
    accessibility: bool,
    screen_recording: bool,
    direct_capture_verified: bool,
    running: Option<bool>,
    diagnostic: Option<String>,
}

fn parse_permissions(output: &str) -> Result<PermissionStatus, String> {
    let payload: Value = serde_json::from_str(output)
        .map_err(|_| "Cua Driver returned invalid permission status JSON".to_owned())?;
    if !payload.is_object() {
        return Err("Cua Driver returned an invalid permission status".to_owned());
    }
    let trusted = payload.get("source").is_none()
        || payload["source"]["attribution"].as_str() == Some("driver-daemon");
    // Read-only status never starts a capture. The explicit permission helper
    // records its successful probe; a failed probe clears that record.
    let capture_status = payload["direct_capture_status"].as_str();
    let capture_failed = payload["screen_recording_capturable"].as_bool() == Some(false)
        || matches!(
            capture_status,
            Some(
                "unavailable"
                    | "timed_out"
                    | "probe_failed"
                    | "blocked"
                    | "blocked_by_screen_recording"
            )
        )
        || payload
            .get("direct_capture_verification_error")
            .is_some_and(|value| !value.is_null());
    let verification = &payload["direct_capture_verification"];
    let verified = payload["screen_recording_capturable"].as_bool() == Some(true)
        || capture_status == Some("ready")
        || (verification["source"].as_str() == Some("permissions_grant")
            && verification["bundle_id"].as_str() == Some("com.trycua.driver")
            && verification["verified_at"]
                .as_str()
                .is_some_and(|value| !value.is_empty()));
    Ok(PermissionStatus {
        accessibility: trusted && payload["accessibility"].as_bool() == Some(true),
        screen_recording: trusted && payload["screen_recording"].as_bool() == Some(true),
        direct_capture_verified: trusted && verified && !capture_failed,
        running: payload["daemon_running"].as_bool(),
        diagnostic: if payload["accessibility"].is_boolean()
            && payload["screen_recording"].is_boolean()
        {
            None
        } else {
            Some(payload["reason"].as_str().unwrap_or("Cua permission onboarding is pending. Complete the macOS access prompts, then check again.").to_owned())
        },
    })
}

fn run_probe(program: &Path, args: &[&str]) -> Result<ProcessOutput, String> {
    run_bounded(Command::new(program).args(args), PROBE_TIMEOUT)
}

struct ProcessOutput {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

impl ProcessOutput {
    fn require_success(self, action: &str) -> Result<Self, String> {
        if self.status.success() {
            return Ok(self);
        }
        let diagnostic = if self.stderr.trim().is_empty() {
            self.stdout.trim()
        } else {
            self.stderr.trim()
        };
        Err(format!("{action} failed ({}): {diagnostic}", self.status))
    }
}

/// Files avoid pipe deadlocks when a driver or installer leaves a subprocess alive.
fn run_bounded(command: &mut Command, timeout: Duration) -> Result<ProcessOutput, String> {
    let capture = TemporaryDirectory::new(&env::temp_dir(), "riwork-cua-probe")?;
    let stdout_path = capture.path.join("stdout");
    let stderr_path = capture.path.join("stderr");
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(
            File::create(&stdout_path).map_err(|error| error.to_string())?,
        ))
        .stderr(Stdio::from(
            File::create(&stderr_path).map_err(|error| error.to_string())?,
        ));
    detach_process(command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("Cannot run Cua command: {error}"))?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                terminate_child(&mut child);
                return Err(format!(
                    "Cua command timed out after {} seconds",
                    timeout.as_secs_f32()
                ));
            }
            Err(error) => {
                terminate_child(&mut child);
                return Err(format!("Cannot wait for Cua command: {error}"));
            }
        }
    };
    Ok(ProcessOutput {
        status,
        stdout: read_limited(&stdout_path)?,
        stderr: read_limited(&stderr_path)?,
    })
}

fn detach_process(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(not(unix))]
    let _ = command;
}

fn terminate_child(child: &mut Child) {
    #[cfg(unix)]
    {
        // A timed-out wrapper may have its own children. Terminate its private group.
        let _ = Command::new("/bin/kill")
            .args(["-KILL", "--", &format!("-{}", child.id())])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn read_limited(path: &Path) -> Result<String, String> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|error| error.to_string())?
        .take(OUTPUT_LIMIT)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn atomic_executable(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if fs::read(path).ok().as_deref() == Some(bytes) && executable_path(path).is_some() {
        return Ok(());
    }
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o755);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|error| format!("Cannot create Cua harness shim: {error}"))?;
        file.write_all(bytes)
            .map_err(|error| format!("Cannot write Cua harness shim: {error}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o755))
                .map_err(|error| format!("Cannot set Cua harness shim permissions: {error}"))?;
        }
        file.sync_all()
            .map_err(|error| format!("Cannot sync Cua harness shim: {error}"))?;
        fs::rename(&temporary, path)
            .map_err(|error| format!("Cannot install Cua harness shim: {error}"))
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

struct TemporaryDirectory {
    path: PathBuf,
}

impl TemporaryDirectory {
    fn new(parent: &Path, prefix: &str) -> Result<Self, String> {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Cannot create Cua directory: {error}"))?;
        let path = parent.join(format!("{prefix}-{}", uuid::Uuid::new_v4()));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&path)
            .map_err(|error| format!("Cannot create Cua temporary directory: {error}"))?;
        Ok(Self { path })
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manager(home: PathBuf) -> CuaManager {
        CuaManager {
            home,
            override_driver: None,
            user_home: None,
            search_path: vec![],
        }
    }

    fn fake_driver(path: &Path, body: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        atomic_executable(path, format!("#!/bin/sh\n{body}\n").as_bytes()).unwrap();
    }

    #[test]
    fn discovery_prefers_override_then_managed_then_user_then_path() {
        let temp = TemporaryDirectory::new(&env::temp_dir(), "riwork-cua-test").unwrap();
        let home = temp.path.join("riwork");
        let mut manager = manager(home.clone());
        let managed = home.join("cua/bin/cua-driver");
        let user = temp.path.join("user/.local/bin/cua-driver");
        let external = temp.path.join("path/cua-driver");
        let custom = temp.path.join("custom-driver");
        for path in [&managed, &user, &external, &custom] {
            fake_driver(path, "exit 0");
        }
        manager.user_home = Some(temp.path.join("user"));
        manager.search_path.push(temp.path.join("path"));
        manager.override_driver = Some(custom.clone());
        assert_eq!(
            manager.driver_path().unwrap(),
            custom.canonicalize().unwrap()
        );
        manager.override_driver = None;
        assert_eq!(
            manager.driver_path().unwrap(),
            managed.canonicalize().unwrap()
        );
        fs::remove_file(managed).unwrap();
        assert_eq!(manager.driver_path().unwrap(), user.canonicalize().unwrap());
        fs::remove_file(user).unwrap();
        // A real system app may exist on developer machines; isolate PATH precedence separately.
        if !Path::new(APP_PATH)
            .join("Contents/MacOS/cua-driver")
            .is_file()
        {
            assert_eq!(
                manager.driver_path().unwrap(),
                external.canonicalize().unwrap()
            );
        }
    }

    #[test]
    fn invalid_explicit_override_does_not_fall_back() {
        let temp = TemporaryDirectory::new(&env::temp_dir(), "riwork-cua-test").unwrap();
        let mut manager = manager(temp.path.clone());
        fake_driver(&temp.path.join("cua/bin/cua-driver"), "exit 0");
        manager.override_driver = Some(temp.path.join("missing"));
        assert!(
            manager
                .driver_path()
                .unwrap_err()
                .contains("RIWORK_CUA_DRIVER")
        );
    }

    #[test]
    fn broken_default_driver_needs_repair_but_broken_override_is_an_error() {
        let temp = TemporaryDirectory::new(&env::temp_dir(), "riwork-cua-test").unwrap();
        let path = temp.path.join("cua/bin/cua-driver");
        fake_driver(&path, "exit 1");
        let mut manager = manager(temp.path.clone());
        assert!(!manager.installed_driver_usable().unwrap());
        manager.override_driver = Some(path);
        assert!(
            manager
                .installed_driver_usable()
                .unwrap_err()
                .contains("RIWORK_CUA_DRIVER")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn failed_permission_command_cannot_claim_readiness_from_its_output() {
        let temp = TemporaryDirectory::new(&env::temp_dir(), "riwork-cua-test").unwrap();
        let driver = temp.path.join("fake-driver");
        fake_driver(
            &driver,
            "case \"$1\" in\n --version) echo 'cua-driver 0.30.1';;\n status) echo 'Cua Driver daemon is running';;\n permissions) echo '{\"accessibility\":true,\"screen_recording\":true,\"source\":{\"attribution\":\"driver-daemon\"}}'; exit 1;;\n *) exit 99;;\nesac",
        );
        let mut manager = manager(temp.path.clone());
        manager.override_driver = Some(driver);
        let status = manager.status().unwrap();
        assert!(status.installed && status.running);
        assert!(!status.accessibility && !status.screen_recording && !status.ready);
        assert!(status.message.contains("permissions could not be checked"));
    }

    #[test]
    fn permission_status_does_not_confuse_host_grants_with_driver_grants() {
        let good = r#"{"accessibility":true,"screen_recording":true,"source":{"attribution":"driver-daemon"}}"#;
        assert!(parse_permissions(good).unwrap().accessibility);
        let host =
            r#"{"accessibility":true,"screen_recording":true,"source":{"attribution":"host"}}"#;
        let host = parse_permissions(host).unwrap();
        assert!(!host.accessibility && !host.screen_recording);
        let unknown = parse_permissions(r#"{"daemon_running":false,"status":"unknown"}"#).unwrap();
        assert!(!unknown.accessibility && !unknown.screen_recording);
        assert_eq!(unknown.running, Some(false));
        assert!(parse_permissions("not JSON").is_err());
        assert!(parse_permissions("[]").is_err());
    }

    #[test]
    fn capture_readiness_requires_verification_and_rejects_explicit_failure() {
        let mut payload = serde_json::json!({
            "accessibility": true,
            "screen_recording": true,
            "screen_recording_capturable": null,
            "direct_capture_status": "not_checked",
            "source": {"attribution": "driver-daemon"}
        });
        let parsed = |payload: &Value| parse_permissions(&payload.to_string()).unwrap();
        assert!(!parsed(&payload).direct_capture_verified);
        payload["direct_capture_verification"] = serde_json::json!({
            "source": "permissions_grant", "bundle_id": "com.trycua.driver",
            "verified_at": "2026-09-27T00:00:00Z"
        });
        assert!(parsed(&payload).direct_capture_verified);
        payload["screen_recording_capturable"] = Value::Bool(false);
        assert!(!parsed(&payload).direct_capture_verified);
        payload["screen_recording_capturable"] = Value::Null;
        payload["direct_capture_status"] = Value::String("blocked_by_screen_recording".into());
        assert!(!parsed(&payload).direct_capture_verified);
        payload["direct_capture_status"] = Value::String("not_checked".into());
        payload["direct_capture_verification"]["bundle_id"] = Value::String("another.app".into());
        assert!(!parsed(&payload).direct_capture_verified);
        payload["screen_recording_capturable"] = Value::Bool(true);
        assert!(parsed(&payload).direct_capture_verified);
        payload["source"]["attribution"] = Value::String("host".into());
        assert!(!parsed(&payload).direct_capture_verified);
    }

    #[test]
    fn status_checks_driver_without_installing_or_starting_it() {
        let temp = TemporaryDirectory::new(&env::temp_dir(), "riwork-cua-test").unwrap();
        let driver = temp.path.join("fake-driver");
        fake_driver(
            &driver,
            "case \"$1\" in\n --version) echo 'cua-driver 0.30.1';;\n status) echo 'Cua Driver daemon is running';;\n permissions) echo '{\"accessibility\":true,\"screen_recording\":false,\"source\":{\"attribution\":\"driver-daemon\"}}';;\n *) exit 99;;\nesac",
        );
        let mut manager = manager(temp.path.clone());
        manager.override_driver = Some(driver.clone());
        let status = manager.status().unwrap();
        assert!(status.installed && status.running);
        assert_eq!(status.version.as_deref(), Some("cua-driver 0.30.1"));
        #[cfg(target_os = "macos")]
        {
            assert!(status.accessibility && !status.screen_recording && !status.ready);
            assert!(status.message.contains("Screen Recording"));
        }
        assert_eq!(status.driver_path, Some(driver.canonicalize().unwrap()));
        assert!(!temp.path.join("cua").exists());
    }

    #[test]
    fn commands_are_bounded_even_when_a_child_keeps_output_open() {
        let start = Instant::now();
        let error = run_bounded(
            Command::new("/bin/sh").args(["-c", "sleep 30 & wait"]),
            Duration::from_millis(80),
        )
        .err()
        .unwrap();
        assert!(error.contains("timed out"));
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn shims_forward_literal_arguments_and_quote_executable_path() {
        let temp = TemporaryDirectory::new(&env::temp_dir(), "riwork-cua-test").unwrap();
        let executable = temp.path.join("RiWork 'test'/riwork");
        fake_driver(&executable, "printf '%s\\n' \"$@\"");
        let manager = manager(temp.path.clone());
        let bin = manager.ensure_harness_shims(&executable).unwrap();
        for harness in ["codex", "grok"] {
            let output = Command::new(bin.join(harness))
                .args(["--json", "a b", "$(literal)"])
                .output()
                .unwrap();
            assert!(output.status.success());
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                format!("cua\nharness\n{harness}\n--\n--json\na b\n$(literal)\n")
            );
        }
        assert!(
            fs::read_to_string(bin.join("claude"))
                .unwrap()
                .contains("# RiWork Cua harness shim")
        );
    }
}
