//! RiWork's shared Cua.ai Driver installation and app-owned desktop runtime.

use std::{
    env,
    ffi::OsString,
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
/// Identity RiWork pins for the managed app: Cua AI, Inc.'s Developer ID.
const APP_BUNDLE_ID: &str = "com.trycua.driver";
const APP_TEAM_ID: &str = "YCK386LBJ7";
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
/// The first exec after boot can spend seconds in code-signature and dyld setup.
const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const VERSION_RETRY_DELAY: Duration = Duration::from_millis(500);
const VERIFY_TIMEOUT: Duration = Duration::from_secs(60);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(300);
/// Long enough for the vendor installer's EXIT/TERM traps to release its lock
/// and restore a backed-up app.
const INSTALL_TERMINATE_GRACE: Duration = Duration::from_secs(10);
/// The vendor installer treats a lock without a live holder as stale after this long.
const INSTALL_LOCK_STALE_AFTER: Duration = Duration::from_secs(600);
const DAEMON_LOCK_WAIT: Duration = Duration::from_secs(5);
const DAEMON_START_WAIT: Duration = Duration::from_secs(30);
const OUTPUT_LIMIT: u64 = 16 * 1024;
const TIMED_OUT: &str = "Cua command timed out";

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
    app_path: PathBuf,
    /// `None` where CuaDriver.app is not used, so nothing is pinned.
    verifier: Option<AppVerifier>,
}

/// An empty `RIWORK_CUA_DRIVER` is unset rather than a path to nothing.
fn driver_override() -> Option<PathBuf> {
    non_empty_path(env::var_os("RIWORK_CUA_DRIVER"))
}

fn non_empty_path(value: Option<OsString>) -> Option<PathBuf> {
    value.filter(|value| !value.is_empty()).map(PathBuf::from)
}

/// A relative or empty entry would resolve against whatever directory the
/// caller happens to be in.
fn search_directories(path: &std::ffi::OsStr) -> Vec<PathBuf> {
    env::split_paths(path)
        .filter(|directory| directory.is_absolute())
        .collect()
}

/// The override to hand to MCP servers, whose environments are filtered by the
/// harnesses, made absolute because their working directory differs.
pub fn driver_override_for_harness() -> Option<PathBuf> {
    driver_override().map(|path| std::path::absolute(&path).unwrap_or(path))
}

impl CuaManager {
    pub fn open_default() -> Result<Self, String> {
        Self::at(crate::paths::riwork_home()?)
    }

    pub fn at(home: PathBuf) -> Result<Self, String> {
        crate::paths::create_private_dir(&home)
            .map_err(|error| format!("Cannot create {}: {error}", home.display()))?;
        let home = home
            .canonicalize()
            .map_err(|error| format!("Cannot resolve {}: {error}", home.display()))?;
        Ok(Self {
            home,
            override_driver: driver_override(),
            user_home: env::var_os("HOME").map(PathBuf::from),
            search_path: env::var_os("PATH")
                .map(|value| search_directories(&value))
                .unwrap_or_default(),
            app_path: PathBuf::from(APP_PATH),
            verifier: cfg!(target_os = "macos").then(AppVerifier::system),
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
        candidates.push(self.app_driver());
        candidates.extend(self.search_path.iter().map(|dir| dir.join("cua-driver")));
        Ok(candidates.iter().find_map(|path| executable_path(path)))
    }

    fn app_driver(&self) -> PathBuf {
        self.app_path.join("Contents/MacOS/cua-driver")
    }

    /// The verifier when the managed app is pinned. A developer's
    /// `RIWORK_CUA_DRIVER` is an explicit choice of a different binary.
    fn pinned_verifier(&self) -> Option<&AppVerifier> {
        self.verifier
            .as_ref()
            .filter(|_| self.override_driver.is_none())
    }

    /// Resolve the driver for execution. Unlike `driver_path`, this proves that
    /// a managed driver is the CuaDriver.app signed by Cua before anything runs it.
    fn trusted_driver(&self) -> Result<PathBuf, String> {
        let driver = self.driver_path()?;
        match self.managed_app_problem(&driver)? {
            None => Ok(driver),
            Some(problem) => Err(problem.describe(&self.app_path)),
        }
    }

    /// `Err` means the check itself could not run, which is not evidence of a
    /// bad installation.
    fn managed_app_problem(&self, driver: &Path) -> Result<Option<AppProblem>, String> {
        let Some(verifier) = self.pinned_verifier() else {
            return Ok(None);
        };
        if !self.app_driver().is_file() {
            return Ok(Some(AppProblem::Missing));
        }
        let app = self
            .app_path
            .canonicalize()
            .map_err(|error| format!("Cannot resolve {}: {error}", self.app_path.display()))?;
        if !driver.starts_with(&app) {
            return Ok(Some(AppProblem::Untrusted(format!(
                "{} is not inside {}",
                driver.display(),
                app.display()
            ))));
        }
        let cache = self.home.join("cua/app-verified");
        let fingerprint = app_fingerprint(&app);
        if let Some(fingerprint) = &fingerprint
            && fs::read_to_string(&cache).is_ok_and(|cached| cached == *fingerprint)
        {
            return Ok(None);
        }
        match verifier.verify(&app)? {
            Verdict::Rejected(reason) => Ok(Some(AppProblem::Untrusted(reason))),
            Verdict::Trusted => {
                if let Some(fingerprint) = fingerprint {
                    let _ = fs::create_dir_all(self.home.join("cua"));
                    let _ = fs::write(&cache, fingerprint);
                }
                Ok(None)
            }
        }
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
        // Never execute a managed driver whose app has not passed the identity check.
        let app_problem = match self.managed_app_problem(&driver) {
            Ok(problem) => problem,
            Err(error) => {
                return Ok(self.blocked_status(
                    driver,
                    true,
                    format!("Cua Driver's signature could not be checked: {error}"),
                ));
            }
        };
        if let Some(problem) = app_problem {
            let installed = !matches!(problem, AppProblem::Missing);
            return Ok(self.blocked_status(driver, installed, problem.describe(&self.app_path)));
        }
        let version = driver_version(&driver);
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
        let ready = version.is_ok()
            && running
            && accessibility
            && screen_recording
            && direct_capture_verified;
        let message = if let Err(error) = &version {
            format!(
                "Cua Driver could not report its version: {error}. Run `riwork setup` to repair the installation."
            )
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
        } else if self.override_driver.is_some() {
            "Cua Driver is ready (custom driver from RIWORK_CUA_DRIVER; its signature is not verified). RiWork harnesses share its desktop service.".to_owned()
        } else {
            "Cua Driver is ready. RiWork harnesses share its desktop service.".to_owned()
        };
        Ok(CuaStatus {
            installed: true,
            driver_path: Some(driver),
            version: version.ok(),
            accessibility,
            screen_recording,
            direct_capture_verified,
            running,
            ready,
            message,
        })
    }

    /// A status for a driver RiWork refuses to run, so nothing probes it.
    fn blocked_status(&self, driver: PathBuf, installed: bool, message: String) -> CuaStatus {
        CuaStatus {
            installed,
            driver_path: Some(driver),
            version: None,
            accessibility: false,
            screen_recording: false,
            direct_capture_verified: false,
            running: false,
            ready: false,
            message,
        }
    }

    pub fn setup(&self) -> Result<CuaStatus, String> {
        let _lock = self.lock("install.lock", Duration::from_secs(5))?;
        let repair = match self.installed_driver_health()? {
            DriverHealth::Usable => None,
            DriverHealth::Missing => Some(None),
            DriverHealth::Broken(reason) => Some(Some(reason)),
        };
        if let Some(reason) = repair {
            self.install().map_err(|error| match reason {
                Some(reason) => {
                    format!("{error} (repairing an installation that failed: {reason})")
                }
                None => error,
            })?;
        }
        // Whatever the installer left must pass the same check before anything runs it.
        self.trusted_driver()?;
        let executable = env::current_exe()
            .map_err(|error| format!("Cannot locate the RiWork executable: {error}"))?;
        self.ensure_harness_shims(&executable)?;
        self.ensure_started()?;
        self.status()
    }

    fn installed_driver_health(&self) -> Result<DriverHealth, String> {
        let Some(driver) = self.find_driver()? else {
            return Ok(DriverHealth::Missing);
        };
        // Identity first: the version probe executes the driver.
        if let Some(problem) = self.managed_app_problem(&driver)? {
            return Ok(DriverHealth::Broken(problem.describe(&self.app_path)));
        }
        match driver_version(&driver) {
            Ok(_) => Ok(DriverHealth::Usable),
            Err(error) if self.override_driver.is_some() => Err(format!(
                "RIWORK_CUA_DRIVER cannot report its version: {error}"
            )),
            // A driver that is merely slow is not broken, and a reinstall stops the shared daemon.
            Err(error) if is_timeout_error(&error) => Err(format!(
                "Cua Driver did not answer its version check ({error}). RiWork did not reinstall it because a slow first start is common; retry in a moment. If it keeps failing, delete {} and run `riwork setup`.",
                self.app_path.display()
            )),
            Err(error) => Ok(DriverHealth::Broken(error)),
        }
    }

    fn install(&self) -> Result<(), String> {
        self.clear_stale_install_lock()?;
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
        let mut install = self.installer_command(
            Path::new("/bin/bash"),
            &script,
            installer_environment(env::vars_os()),
        );
        run_bounded_graceful(&mut install, INSTALL_TIMEOUT, INSTALL_TERMINATE_GRACE)
            .map_err(|error| {
                if is_timeout_error(&error) {
                    format!(
                        "{error}. If no installer is running, remove its lock at {} and retry.",
                        self.install_lock_dir().display()
                    )
                } else {
                    error
                }
            })?
            .require_success("Install Cua Driver")?;
        Ok(())
    }

    /// The vendor installer runs with a scrubbed environment: it is third-party
    /// code that needs neither RiWork's credentials nor its API keys.
    fn installer_command(
        &self,
        shell: &Path,
        script: &Path,
        environment: Vec<(OsString, OsString)>,
    ) -> Command {
        let mut install = Command::new(shell);
        install
            .arg(script)
            .arg("--bin-dir")
            .arg(self.home.join("cua/bin"))
            .arg("--no-modify-path")
            .args(["--channel", "stable"])
            .env_clear()
            .envs(environment)
            .env("CUA_DRIVER_RS_HOME", self.home.join("cua/package"));
        install
    }

    fn install_lock_dir(&self) -> PathBuf {
        self.home.join("cua/package/packages/.install.lock.d")
    }

    /// A killed installer leaves its lock behind and every later run then waits
    /// on it. Only a lock whose recorded holder is gone is removed.
    fn clear_stale_install_lock(&self) -> Result<(), String> {
        let lock = self.install_lock_dir();
        let Ok(metadata) = fs::symlink_metadata(&lock) else {
            return Ok(());
        };
        if !metadata.is_dir() {
            return Ok(());
        }
        let stale = match fs::read_to_string(lock.join("info"))
            .ok()
            .as_deref()
            .and_then(installer_lock_pid)
        {
            Some(pid) => !process_alive(pid),
            // No recorded holder: only reclaim it once the installer itself would.
            None => metadata
                .modified()
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age >= INSTALL_LOCK_STALE_AFTER),
        };
        if stale {
            fs::remove_dir_all(&lock).map_err(|error| {
                format!(
                    "Cannot remove the stale Cua installer lock {}: {error}",
                    lock.display()
                )
            })?;
        }
        Ok(())
    }

    /// Start the app-owned daemon without permission prompts or downloads.
    pub fn ensure_started(&self) -> Result<(), String> {
        let driver = self.trusted_driver()?;
        if daemon_running(&driver) {
            return Ok(());
        }
        let Some(_lock) = self.acquire_daemon_lock(&driver, DAEMON_LOCK_WAIT, DAEMON_START_WAIT)?
        else {
            return Ok(());
        };
        if daemon_running(&driver) {
            return Ok(());
        }
        #[cfg(target_os = "macos")]
        if self.override_driver.is_none() {
            let mut launch = Command::new("/usr/bin/open");
            launch.args(["-n", "-g", "-a"]).arg(&self.app_path).args([
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

    /// Take the launch lock, or return `None` once another launcher's daemon is
    /// up. Agents spawned together against a cold daemon all arrive here; the
    /// losers wait for the winner instead of failing their required MCP server.
    fn acquire_daemon_lock(
        &self,
        driver: &Path,
        lock_wait: Duration,
        start_wait: Duration,
    ) -> Result<Option<File>, String> {
        if let Some(lock) = self.try_lock("daemon.lock", lock_wait)? {
            return Ok(Some(lock));
        }
        let deadline = Instant::now() + start_wait;
        loop {
            if daemon_running(driver) {
                return Ok(None);
            }
            // The launcher holding the lock may have failed; take over.
            if let Some(lock) = self.try_lock("daemon.lock", Duration::ZERO)? {
                return Ok(Some(lock));
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "Another Cua launch held the desktop service lock and the service did not start within {} seconds. Run `riwork cua status` and retry.",
                    (lock_wait + start_wait).as_secs()
                ));
            }
            thread::sleep(Duration::from_millis(250));
        }
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
        let driver = self.trusted_driver()?;
        #[cfg(target_os = "macos")]
        if self.override_driver.is_none() && !self.status()?.ready {
            // Explicit setup may restart the shared service. Cua's normal
            // onboarding gate uses fresh helpers to avoid cached TCC denials.
            let _lock = self.lock("daemon.lock", Duration::from_secs(5))?;
            run_probe(&driver, &["stop"])?.require_success("Restart Cua permission onboarding")?;
            let mut launch = Command::new("/usr/bin/open");
            launch.args(["-n", "-g", "-a"]).arg(&self.app_path).args([
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
        let driver = self.trusted_driver()?;
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
        let bin = self.home.join("cua/harness-bin");
        fs::create_dir_all(&bin)
            .map_err(|error| format!("Cannot create Cua harness directory: {error}"))?;
        for harness in ["codex", "claude", "grok"] {
            let content = harness_shim(harness, &executable, &bin);
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
        self.try_lock(name, timeout)?.ok_or_else(|| {
            "Another Cua setup or startup is running. Retry when it finishes.".to_owned()
        })
    }

    /// `None` means the lock stayed busy for the whole timeout.
    fn try_lock(&self, name: &str, timeout: Duration) -> Result<Option<File>, String> {
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
                Ok(()) => return Ok(Some(file)),
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(50))
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
                Err(error) => return Err(format!("Cannot lock Cua setup: {error}")),
            }
        }
    }
}

enum DriverHealth {
    Missing,
    Usable,
    /// The version probe failed outright, so the installation needs repair.
    Broken(String),
}

enum AppProblem {
    Missing,
    Untrusted(String),
}

impl AppProblem {
    fn describe(&self, app: &Path) -> String {
        match self {
            Self::Missing => "CuaDriver.app is missing. Run `riwork setup` to install its signed desktop service.".to_owned(),
            Self::Untrusted(reason) => format!(
                "CuaDriver.app failed RiWork's identity check ({reason}). RiWork only runs {} when it is signed by Cua AI, Inc. (Team ID {APP_TEAM_ID}) and notarized by Apple. Move it to the Trash and run `riwork setup` to reinstall it, or set RIWORK_CUA_DRIVER to a developer build.",
                app.display()
            ),
        }
    }
}

enum Verdict {
    Trusted,
    Rejected(String),
}

/// Checks the pinned identity with macOS's own tools. The paths are fields so
/// tests can substitute fakes.
#[derive(Clone, Debug)]
struct AppVerifier {
    codesign: PathBuf,
    spctl: PathBuf,
}

impl AppVerifier {
    fn system() -> Self {
        Self {
            codesign: PathBuf::from("/usr/bin/codesign"),
            spctl: PathBuf::from("/usr/sbin/spctl"),
        }
    }

    /// `Err` means a tool could not run or timed out; `Rejected` is a verdict.
    fn verify(&self, app: &Path) -> Result<Verdict, String> {
        let requirement = format!(
            "=anchor apple generic and identifier \"{APP_BUNDLE_ID}\" and certificate leaf[subject.OU] = \"{APP_TEAM_ID}\""
        );
        let mut codesign = Command::new(&self.codesign);
        codesign
            .args(["--verify", "--deep", "--strict", "-R"])
            .arg(requirement)
            .arg(app);
        let signature = run_bounded(&mut codesign, VERIFY_TIMEOUT)?;
        if !signature.status.success() {
            return Ok(Verdict::Rejected(format!(
                "code signature check failed: {}",
                signature.diagnostic()
            )));
        }
        let mut spctl = Command::new(&self.spctl);
        spctl.args(["--assess", "--type", "execute"]).arg(app);
        let assessment = run_bounded(&mut spctl, VERIFY_TIMEOUT)?;
        if !assessment.status.success() {
            return Ok(Verdict::Rejected(format!(
                "Gatekeeper notarization check failed: {}",
                assessment.diagnostic()
            )));
        }
        Ok(Verdict::Trusted)
    }
}

/// Identifies the exact bundle contents that were verified. Replacing the app
/// changes these; a cached verdict for the previous one no longer applies.
fn app_fingerprint(app: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let mut fingerprint = format!("{}\n", app.display());
    for relative in [
        "",
        "Contents/Info.plist",
        "Contents/MacOS/cua-driver",
        "Contents/_CodeSignature/CodeResources",
    ] {
        let metadata = app.join(relative).metadata().ok()?;
        fingerprint.push_str(&format!(
            "{relative}\t{}\t{}\t{}\t{}\n",
            metadata.ino(),
            metadata.len(),
            metadata.mtime(),
            metadata.mtime_nsec()
        ));
    }
    Some(fingerprint)
}

/// Only what the installer needs. Credentials and API keys stay behind; the
/// telemetry opt-out and proxy settings are kept because they change what the
/// installer may do or whether it can reach GitHub at all.
fn installer_environment(
    parent: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    const KEPT: [&str; 14] = [
        "HOME",
        "TMPDIR",
        "LANG",
        "CUA_TELEMETRY_ENABLED",
        "CUA_DRIVER_RS_TELEMETRY_ENABLED",
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "ALL_PROXY",
        "all_proxy",
        "NO_PROXY",
        "no_proxy",
        "SSL_CERT_FILE",
    ];
    let mut path = vec![
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
        PathBuf::from("/usr/sbin"),
        PathBuf::from("/sbin"),
    ];
    let mut environment = Vec::new();
    for (name, value) in parent {
        let Some(name_text) = name.to_str() else {
            continue;
        };
        if name_text == "PATH" {
            // System tools win over anything earlier in the caller's PATH.
            path.extend(env::split_paths(&value).filter(|directory| directory.is_absolute()));
        } else if KEPT.contains(&name_text) || name_text.starts_with("LC_") {
            environment.push((name, value));
        }
    }
    let mut unique = Vec::new();
    for directory in path {
        if !unique.contains(&directory) {
            unique.push(directory);
        }
    }
    if let Ok(path) = env::join_paths(unique) {
        environment.push((OsString::from("PATH"), path));
    }
    environment
}

/// `pid=N` in the installer lock's `info` file.
fn installer_lock_pid(info: &str) -> Option<u32> {
    info.lines()
        .find_map(|line| line.strip_prefix("pid="))
        .and_then(|pid| pid.trim().parse().ok())
        .filter(|pid| *pid > 0)
}

fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return true;
        };
        // EPERM still means the process exists.
        unsafe {
            libc::kill(pid, 0) == 0
                || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

fn is_timeout_error(error: &str) -> bool {
    error.starts_with(TIMED_OUT)
}

/// A shim runs the harness through RiWork, but a moved or cleaned checkout must
/// not leave every `codex`, `claude`, and `grok` in a RiWork shell broken.
fn harness_shim(harness: &str, executable: &Path, shim_directory: &Path) -> String {
    let executable = shell_quote(&executable.to_string_lossy());
    let shim_directory = shell_quote(&shim_directory.to_string_lossy());
    format!(
        "#!/bin/sh\n\
# RiWork Cua harness shim\n\
riwork_bin={executable}\n\
shim_dir={shim_directory}\n\
if [ -f \"$riwork_bin\" ] && [ -x \"$riwork_bin\" ]; then\n\
  exec \"$riwork_bin\" cua harness {harness} -- \"$@\"\n\
fi\n\
echo \"riwork: $riwork_bin is missing; starting {harness} without RiWork's Cua connection. Open RiWork to repair its launchers.\" >&2\n\
set -f\n\
old_ifs=$IFS\n\
IFS=:\n\
for dir in $PATH; do\n\
  case $dir in /*) ;; *) continue ;; esac\n\
  [ \"${{dir%/}}\" = \"${{shim_dir%/}}\" ] && continue\n\
  if [ -f \"$dir/{harness}\" ] && [ -x \"$dir/{harness}\" ]; then\n\
    IFS=$old_ifs\n\
    exec \"$dir/{harness}\" \"$@\"\n\
  fi\n\
done\n\
IFS=$old_ifs\n\
echo \"riwork: {harness} is not installed on PATH outside RiWork's launchers.\" >&2\n\
exit 127\n"
    )
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

/// One retry: a transient failure right after boot must not read as a broken install.
fn driver_version(driver: &Path) -> Result<String, String> {
    let probe = || {
        let output = run_bounded(Command::new(driver).arg("--version"), VERSION_PROBE_TIMEOUT)?
            .require_success("Read Cua Driver version")?;
        let version = output.stdout.trim();
        if version.is_empty() {
            return Err("Cua Driver returned an empty version".to_owned());
        }
        Ok(version.to_owned())
    };
    probe().or_else(|_| {
        thread::sleep(VERSION_RETRY_DELAY);
        probe()
    })
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
        Err(format!(
            "{action} failed ({}): {}",
            self.status,
            self.diagnostic()
        ))
    }

    fn diagnostic(&self) -> &str {
        if self.stderr.trim().is_empty() {
            self.stdout.trim()
        } else {
            self.stderr.trim()
        }
    }
}

/// Files avoid pipe deadlocks when a driver or installer leaves a subprocess alive.
fn run_bounded(command: &mut Command, timeout: Duration) -> Result<ProcessOutput, String> {
    run_bounded_graceful(command, timeout, Duration::ZERO)
}

/// On timeout, asks the command's process group to terminate and allows `grace`
/// for its cleanup traps before killing it.
fn run_bounded_graceful(
    command: &mut Command,
    timeout: Duration,
    grace: Duration,
) -> Result<ProcessOutput, String> {
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
                terminate_child_after(&mut child, grace);
                return Err(format!(
                    "{TIMED_OUT} after {} seconds",
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
    terminate_child_after(child, Duration::ZERO);
}

/// A timed-out wrapper may have its own children, so signal its private group.
/// SIGKILL skips shell traps, which is how an installer strands its lock.
fn terminate_child_after(child: &mut Child, grace: Duration) {
    #[cfg(unix)]
    {
        if !grace.is_zero() {
            signal_group(child.id(), libc::SIGTERM);
            let deadline = Instant::now() + grace;
            while Instant::now() < deadline && matches!(child.try_wait(), Ok(None)) {
                thread::sleep(Duration::from_millis(20));
            }
        }
        signal_group(child.id(), libc::SIGKILL);
    }
    #[cfg(not(unix))]
    let _ = grace;
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(unix)]
fn signal_group(leader: u32, signal: libc::c_int) {
    if let Ok(leader) = libc::pid_t::try_from(leader) {
        // The commands run with `process_group(0)`, so the group id is the pid.
        unsafe { libc::kill(-leader, signal) };
    }
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
            // Tests must never find, run, or verify the developer's real CuaDriver.app.
            app_path: home.join("no-such/CuaDriver.app"),
            home,
            override_driver: None,
            user_home: None,
            search_path: vec![],
            verifier: None,
        }
    }

    fn fake_driver(path: &Path, body: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        atomic_executable(path, format!("#!/bin/sh\n{body}\n").as_bytes()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_new_state_directory_is_owner_only_and_an_existing_one_keeps_its_mode() {
        use std::os::unix::fs::PermissionsExt;
        let temp = TemporaryDirectory::new(&env::temp_dir(), "riwork-cua-test").unwrap();
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        let fresh = temp.path.join("parent/state");
        CuaManager::at(fresh.clone()).unwrap();
        assert_eq!(mode(&fresh), 0o700);
        let shared = temp.path.join("shared");
        fs::create_dir(&shared).unwrap();
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o755)).unwrap();
        CuaManager::at(shared.clone()).unwrap();
        assert_eq!(mode(&shared), 0o755);
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
        assert_eq!(
            manager.driver_path().unwrap(),
            external.canonicalize().unwrap()
        );
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
        assert!(matches!(
            manager.installed_driver_health().unwrap(),
            DriverHealth::Broken(reason) if reason.contains("Read Cua Driver version failed")
        ));
        manager.override_driver = Some(path);
        assert!(
            manager
                .installed_driver_health()
                .err()
                .unwrap()
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

    /// A CuaDriver.app lookalike plus fake `codesign` and `spctl` that log their
    /// arguments and fail while `codesign-fails` or `spctl-fails` exists.
    struct FakeApp {
        root: PathBuf,
        app: PathBuf,
        log: PathBuf,
        verifier: AppVerifier,
    }

    impl FakeApp {
        fn new(root: &Path) -> Self {
            let log = root.join("tools.log");
            let tool = |name: &str| {
                let path = root.join(name);
                fake_driver(
                    &path,
                    &format!(
                        "printf '%s\\n' \"{name} $*\" >> {}\nif [ -e {} ]; then echo '{name} says no' >&2; exit 3; fi",
                        shell_quote(&log.to_string_lossy()),
                        shell_quote(&root.join(format!("{name}-fails")).to_string_lossy()),
                    ),
                );
                path
            };
            let verifier = AppVerifier {
                codesign: tool("codesign"),
                spctl: tool("spctl"),
            };
            let app = root.join("CuaDriver.app");
            let fake = Self {
                root: root.to_owned(),
                app,
                log,
                verifier,
            };
            fake.write_app("v1");
            fake
        }

        fn write_app(&self, version: &str) {
            let driver = self.app.join("Contents/MacOS/cua-driver");
            fake_driver(
                &driver,
                &format!(
                    "case \"$1\" in\n --version) echo 'cua-driver {version}';;\n status) touch {}; echo 'Cua Driver daemon is running';;\n *) exit 99;;\nesac",
                    shell_quote(&self.root.join("driver-ran").to_string_lossy()),
                ),
            );
            for relative in [
                "Contents/Info.plist",
                "Contents/_CodeSignature/CodeResources",
            ] {
                let path = self.app.join(relative);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, version).unwrap();
            }
        }

        fn manager(&self, home: &Path) -> CuaManager {
            let mut manager = manager(home.to_owned());
            manager.app_path = self.app.clone();
            manager.verifier = Some(self.verifier.clone());
            manager.search_path = vec![self.app.join("Contents/MacOS")];
            manager
        }

        fn tool_calls(&self) -> Vec<String> {
            fs::read_to_string(&self.log)
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect()
        }
    }

    #[test]
    fn the_managed_app_is_pinned_to_cuas_identity_and_notarization() {
        let temp = TemporaryDirectory::new(&env::temp_dir(), "riwork-cua-test").unwrap();
        let fake = FakeApp::new(&temp.path);
        let manager = fake.manager(&temp.path.join("home"));
        let driver = manager.trusted_driver().unwrap();
        assert_eq!(
            driver,
            fake.app
                .join("Contents/MacOS/cua-driver")
                .canonicalize()
                .unwrap()
        );
        let app = fake.app.canonicalize().unwrap();
        assert_eq!(
            fake.tool_calls(),
            [
                format!(
                    "codesign --verify --deep --strict -R =anchor apple generic and identifier \"com.trycua.driver\" and certificate leaf[subject.OU] = \"YCK386LBJ7\" {}",
                    app.display()
                ),
                format!("spctl --assess --type execute {}", app.display()),
            ]
        );
    }

    #[test]
    fn an_app_that_fails_either_check_is_refused_with_an_actionable_error() {
        for (tool, detail) in [
            ("codesign", "code signature check failed"),
            ("spctl", "Gatekeeper notarization check failed"),
        ] {
            let temp = TemporaryDirectory::new(&env::temp_dir(), "riwork-cua-test").unwrap();
            let fake = FakeApp::new(&temp.path);
            fs::write(temp.path.join(format!("{tool}-fails")), "").unwrap();
            let manager = fake.manager(&temp.path.join("home"));
            let error = manager.trusted_driver().unwrap_err();
            assert!(error.contains(detail), "{error}");
            assert!(error.contains(&format!("{tool} says no")), "{error}");
            assert!(error.contains("YCK386LBJ7"), "{error}");
            assert!(error.contains("riwork setup"), "{error}");
            // A refusal is never cached.
            assert!(manager.trusted_driver().is_err());
            let checks = fake.tool_calls().len();
            assert!(checks >= 2, "{checks}");
            fs::remove_file(temp.path.join(format!("{tool}-fails"))).unwrap();
            manager.trusted_driver().unwrap();
        }
    }

    #[test]
    fn a_verified_app_is_not_rechecked_until_it_is_replaced() {
        let temp = TemporaryDirectory::new(&env::temp_dir(), "riwork-cua-test").unwrap();
        let fake = FakeApp::new(&temp.path);
        let manager = fake.manager(&temp.path.join("home"));
        manager.trusted_driver().unwrap();
        let first = fake.tool_calls().len();
        assert_eq!(first, 2);
        // The cache survives a new manager, as it does across `riwork cua mcp` launches.
        fake.manager(&temp.path.join("home"))
            .trusted_driver()
            .unwrap();
        assert_eq!(fake.tool_calls().len(), first);
        // Replacing the binary invalidates the cached verdict.
        fs::write(temp.path.join("codesign-fails"), "").unwrap();
        fake.write_app("v2 with different contents");
        assert!(manager.trusted_driver().is_err());
        assert!(fake.tool_calls().len() > first);
    }

    #[test]
    fn an_unverified_app_is_never_executed_by_status() {
        let temp = TemporaryDirectory::new(&env::temp_dir(), "riwork-cua-test").unwrap();
        let fake = FakeApp::new(&temp.path);
        fs::write(temp.path.join("codesign-fails"), "").unwrap();
        let manager = fake.manager(&temp.path.join("home"));
        let status = manager.status().unwrap();
        assert!(status.installed && !status.ready && !status.running);
        assert!(
            status.message.contains("identity check"),
            "{}",
            status.message
        );
        assert!(!temp.path.join("driver-ran").exists());
        assert!(manager.ensure_started().is_err());
        assert!(!temp.path.join("driver-ran").exists());
    }

    #[test]
    fn a_managed_driver_outside_the_verified_app_is_not_trusted() {
        let temp = TemporaryDirectory::new(&env::temp_dir(), "riwork-cua-test").unwrap();
        let fake = FakeApp::new(&temp.path);
        let home = temp.path.join("home");
        let manager = fake.manager(&home);
        let managed = home.join("cua/bin/cua-driver");
        fake_driver(&managed, "exit 0");
        let error = manager.trusted_driver().unwrap_err();
        assert!(error.contains("is not inside"), "{error}");
        // The installer's own layout: a link into the app.
        fs::remove_file(&managed).unwrap();
        std::os::unix::fs::symlink(fake.app.join("Contents/MacOS/cua-driver"), &managed).unwrap();
        manager.trusted_driver().unwrap();
        // Without the app, a driver found elsewhere does not stand in for it.
        fs::remove_dir_all(&fake.app).unwrap();
        fake_driver(&temp.path.join("elsewhere/cua-driver"), "exit 0");
        let mut manager = manager;
        manager.search_path = vec![temp.path.join("elsewhere")];
        fs::remove_file(&managed).unwrap();
        assert_eq!(
            manager.trusted_driver().unwrap_err(),
            AppProblem::Missing.describe(&fake.app)
        );
    }

    #[test]
    fn a_developer_override_skips_identity_pinning() {
        let temp = TemporaryDirectory::new(&env::temp_dir(), "riwork-cua-test").unwrap();
        let fake = FakeApp::new(&temp.path);
        fs::write(temp.path.join("codesign-fails"), "").unwrap();
        let custom = temp.path.join("custom-driver");
        fake_driver(
            &custom,
            "case \"$1\" in\n --version) echo 'cua-driver dev';;\n status) echo 'Cua Driver daemon is running';;\n *) exit 99;;\nesac",
        );
        let mut manager = fake.manager(&temp.path.join("home"));
        manager.override_driver = Some(custom.clone());
        assert_eq!(
            manager.trusted_driver().unwrap(),
            custom.canonicalize().unwrap()
        );
        assert!(fake.tool_calls().is_empty());
        let status = manager.status().unwrap();
        assert!(status.installed && status.running);
        assert!(!status.message.contains("identity check"));
    }

    #[test]
    fn empty_and_relative_environment_paths_are_ignored() {
        assert_eq!(non_empty_path(Some(OsString::new())), None);
        assert_eq!(non_empty_path(None), None);
        assert_eq!(
            non_empty_path(Some(OsString::from("/x/driver"))),
            Some(PathBuf::from("/x/driver"))
        );
        assert_eq!(
            search_directories(std::ffi::OsStr::new("/usr/bin::relative:.:/opt/bin:")),
            [PathBuf::from("/usr/bin"), PathBuf::from("/opt/bin")]
        );
    }

    #[test]
    fn the_installer_sees_only_what_it_needs() {
        let parent = [
            ("PATH", "/opt/tools:relative:/usr/local/bin"),
            ("HOME", "/Users/test"),
            ("TMPDIR", "/tmp/x"),
            ("LANG", "en_US.UTF-8"),
            ("LC_ALL", "C"),
            ("CUA_TELEMETRY_ENABLED", "0"),
            ("HTTPS_PROXY", "http://proxy:3128"),
            ("GH_TOKEN", "secret"),
            ("GITHUB_TOKEN", "secret"),
            ("OPENAI_API_KEY", "secret"),
            ("ANTHROPIC_API_KEY", "secret"),
            ("CUA_DRIVER_RS_VERSION", "0.0.1"),
            ("AWS_SECRET_ACCESS_KEY", "secret"),
        ]
        .map(|(name, value)| (OsString::from(name), OsString::from(value)));
        let environment = installer_environment(parent);
        let names = environment
            .iter()
            .map(|(name, _)| name.to_str().unwrap())
            .collect::<Vec<_>>();
        for kept in [
            "HOME",
            "TMPDIR",
            "LANG",
            "LC_ALL",
            "CUA_TELEMETRY_ENABLED",
            "HTTPS_PROXY",
            "PATH",
        ] {
            assert!(names.contains(&kept), "{kept} in {names:?}");
        }
        for dropped in [
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "CUA_DRIVER_RS_VERSION",
            "AWS_SECRET_ACCESS_KEY",
        ] {
            assert!(!names.contains(&dropped), "{dropped} in {names:?}");
        }
        let path = &environment
            .iter()
            .find(|(name, _)| name == "PATH")
            .unwrap()
            .1;
        let directories = env::split_paths(path).collect::<Vec<_>>();
        assert_eq!(directories[0], Path::new("/usr/bin"));
        assert!(directories.contains(&PathBuf::from("/opt/tools")));
        assert!(!directories.contains(&PathBuf::from("relative")));
    }

    #[test]
    fn the_installer_process_does_not_inherit_the_callers_environment() {
        let temp = TemporaryDirectory::new(&env::temp_dir(), "riwork-cua-test").unwrap();
        let script = temp.path.join("install.sh");
        fake_driver(&script, "env; printf 'ARGS %s\\n' \"$*\"");
        let manager = manager(temp.path.join("home"));
        let mut command = manager.installer_command(
            Path::new("/bin/sh"),
            &script,
            installer_environment([(OsString::from("HOME"), OsString::from("/Users/test"))]),
        );
        let output = run_bounded(&mut command, Duration::from_secs(10)).unwrap();
        assert!(output.status.success());
        let names = output
            .stdout
            .lines()
            .filter_map(|line| line.split_once('=').map(|(name, _)| name))
            .collect::<Vec<_>>();
        // The test process itself runs under cargo, which exports many variables.
        assert!(env::vars_os().count() > 3);
        for name in names {
            assert!(
                [
                    "HOME",
                    "PATH",
                    "CUA_DRIVER_RS_HOME",
                    "PWD",
                    "SHLVL",
                    "_",
                    "OLDPWD"
                ]
                .contains(&name),
                "unexpected installer variable {name}"
            );
        }
        assert!(output.stdout.contains("HOME=/Users/test"));
        assert!(output.stdout.contains(&format!(
            "CUA_DRIVER_RS_HOME={}",
            temp.path.join("home/cua/package").display()
        )));
        assert!(output.stdout.contains("--no-modify-path --channel stable"));
    }

    #[cfg(unix)]
    #[test]
    fn a_timed_out_installer_runs_its_cleanup_traps_before_being_killed() {
        let temp = TemporaryDirectory::new(&env::temp_dir(), "riwork-cua-test").unwrap();
        let lock = temp.path.join("lock.d");
        let script = temp.path.join("installer.sh");
        fake_driver(
            &script,
            &format!(
                "mkdir {0}\ntrap 'rmdir {0}; exit 143' TERM\nsleep 30 &\nwait $!",
                shell_quote(&lock.to_string_lossy())
            ),
        );
        // SIGKILL skips the trap, which is how the vendor lock was stranded.
        let error = run_bounded(
            Command::new("/bin/sh").arg(&script),
            Duration::from_millis(500),
        )
        .err()
        .unwrap();
        assert!(is_timeout_error(&error));
        assert!(lock.exists());
        fs::remove_dir(&lock).unwrap();
        let start = Instant::now();
        let error = run_bounded_graceful(
            Command::new("/bin/sh").arg(&script),
            Duration::from_millis(500),
            Duration::from_secs(5),
        )
        .err()
        .unwrap();
        assert!(is_timeout_error(&error), "{error}");
        assert!(!lock.exists(), "the TERM trap did not run");
        assert!(start.elapsed() < Duration::from_secs(4));
    }

    #[cfg(unix)]
    #[test]
    fn an_installer_that_ignores_termination_is_killed_after_the_grace_period() {
        let temp = TemporaryDirectory::new(&env::temp_dir(), "riwork-cua-test").unwrap();
        let script = temp.path.join("stubborn.sh");
        fake_driver(&script, "trap '' TERM\nsleep 30 &\nwait");
        let start = Instant::now();
        let error = run_bounded_graceful(
            Command::new("/bin/sh").arg(&script),
            Duration::from_millis(300),
            Duration::from_millis(400),
        )
        .err()
        .unwrap();
        assert!(is_timeout_error(&error));
        assert!(start.elapsed() >= Duration::from_millis(700));
        assert!(start.elapsed() < Duration::from_secs(4));
    }

    #[test]
    fn only_an_installer_lock_without_a_live_holder_is_cleared() {
        let temp = TemporaryDirectory::new(&env::temp_dir(), "riwork-cua-test").unwrap();
        let manager = manager(temp.path.clone());
        let lock = manager.install_lock_dir();
        assert!(lock.ends_with("cua/package/packages/.install.lock.d"));
        manager.clear_stale_install_lock().unwrap();
        let mut dead = Command::new("/usr/bin/true").spawn().unwrap();
        let dead_pid = dead.id();
        dead.wait().unwrap();
        let write_info = |pid: u32| {
            fs::create_dir_all(&lock).unwrap();
            fs::write(
                lock.join("info"),
                format!("pid={pid}\nstarted=2026-09-28T00:00:00Z\nargv=install.sh\n"),
            )
            .unwrap();
        };
        write_info(std::process::id());
        manager.clear_stale_install_lock().unwrap();
        assert!(lock.exists(), "a live holder keeps its lock");
        write_info(dead_pid);
        manager.clear_stale_install_lock().unwrap();
        assert!(!lock.exists());
        // A fresh lock with no readable holder may still be mid-creation.
        fs::create_dir_all(&lock).unwrap();
        manager.clear_stale_install_lock().unwrap();
        assert!(lock.exists());
        assert_eq!(installer_lock_pid("started=x\npid=42\n"), Some(42));
        assert_eq!(installer_lock_pid("pid=0\n"), None);
        assert_eq!(installer_lock_pid("pid=abc\n"), None);
    }

    #[test]
    fn a_slow_or_flaky_version_probe_is_retried_once() {
        let temp = TemporaryDirectory::new(&env::temp_dir(), "riwork-cua-test").unwrap();
        let driver = temp.path.join("cua-driver");
        let count = temp.path.join("count");
        fake_driver(
            &driver,
            &format!(
                "n=$(cat {0} 2>/dev/null || echo 0)\necho $((n + 1)) > {0}\n[ \"$n\" -ge 1 ] || {{ echo 'not ready' >&2; exit 1; }}\necho 'cua-driver 1.0'",
                shell_quote(&count.to_string_lossy())
            ),
        );
        assert_eq!(driver_version(&driver).unwrap(), "cua-driver 1.0");
        assert_eq!(fs::read_to_string(&count).unwrap().trim(), "2");
        // A driver that keeps failing reports why instead of hiding the cause.
        fake_driver(&driver, "echo 'dyld: missing library' >&2; exit 1");
        assert!(
            driver_version(&driver)
                .unwrap_err()
                .contains("dyld: missing library")
        );
        let manager = {
            let mut manager = manager(temp.path.clone());
            manager.search_path = vec![temp.path.clone()];
            manager
        };
        assert!(manager.installed_driver_health().is_ok());
        let status = manager.status().unwrap();
        assert!(
            status.message.contains("dyld: missing library"),
            "{}",
            status.message
        );
    }

    fn holding_daemon_lock(manager: &CuaManager) -> File {
        manager
            .try_lock("daemon.lock", Duration::from_secs(1))
            .unwrap()
            .unwrap()
    }

    fn marker_driver(path: &Path, marker: &Path) {
        fake_driver(
            path,
            &format!(
                "case \"$1\" in\n status) [ -e {} ] && echo 'Cua Driver daemon is running';;\n *) exit 99;;\nesac",
                shell_quote(&marker.to_string_lossy())
            ),
        );
    }

    #[test]
    fn a_busy_daemon_lock_waits_for_the_daemon_the_other_launcher_starts() {
        let temp = TemporaryDirectory::new(&env::temp_dir(), "riwork-cua-test").unwrap();
        let driver = temp.path.join("driver");
        let marker = temp.path.join("running");
        marker_driver(&driver, &marker);
        let manager = manager(temp.path.join("home"));
        let _holder = holding_daemon_lock(&manager);
        let starter = {
            let marker = marker.clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(400));
                fs::write(marker, "").unwrap();
            })
        };
        let start = Instant::now();
        let lock = manager
            .acquire_daemon_lock(&driver, Duration::from_millis(50), Duration::from_secs(10))
            .unwrap();
        assert!(lock.is_none(), "the daemon was already started");
        assert!(start.elapsed() < Duration::from_secs(5));
        starter.join().unwrap();
    }

    #[test]
    fn a_daemon_lock_released_while_waiting_is_taken_over() {
        let temp = TemporaryDirectory::new(&env::temp_dir(), "riwork-cua-test").unwrap();
        let driver = temp.path.join("driver");
        marker_driver(&driver, &temp.path.join("never"));
        let manager = manager(temp.path.join("home"));
        let holder = holding_daemon_lock(&manager);
        let releaser = thread::spawn(move || {
            thread::sleep(Duration::from_millis(400));
            drop(holder);
        });
        let lock = manager
            .acquire_daemon_lock(&driver, Duration::from_millis(50), Duration::from_secs(10))
            .unwrap();
        assert!(lock.is_some());
        releaser.join().unwrap();
    }

    #[test]
    fn a_daemon_lock_that_never_yields_a_daemon_eventually_reports_it() {
        let temp = TemporaryDirectory::new(&env::temp_dir(), "riwork-cua-test").unwrap();
        let driver = temp.path.join("driver");
        marker_driver(&driver, &temp.path.join("never"));
        let manager = manager(temp.path.join("home"));
        let _holder = holding_daemon_lock(&manager);
        let error = manager
            .acquire_daemon_lock(
                &driver,
                Duration::from_millis(50),
                Duration::from_millis(600),
            )
            .unwrap_err();
        assert!(error.contains("did not start"), "{error}");
        assert!(error.contains("riwork cua status"), "{error}");
    }

    #[test]
    fn shims_fall_back_to_the_next_cli_when_the_riwork_binary_is_gone() {
        let temp = TemporaryDirectory::new(&env::temp_dir(), "riwork-cua-test").unwrap();
        let executable = temp.path.join("moved checkout/riwork");
        fake_driver(&executable, "echo riwork \"$@\"");
        let manager = manager(temp.path.join("home"));
        let bin = manager.ensure_harness_shims(&executable).unwrap();
        for harness in ["codex", "claude", "grok"] {
            fake_driver(
                &temp.path.join("real").join(harness),
                &format!("echo real {harness} \"$@\""),
            );
        }
        let path = |entries: &[String]| entries.join(":");
        let search = path(&[
            format!("{}/", bin.display()),
            String::new(),
            "relative".to_owned(),
            temp.path.join("real").display().to_string(),
        ]);
        let run = |harness: &str, search: &str| {
            Command::new(bin.join(harness))
                .args(["a b", "--flag"])
                .env_clear()
                .env("PATH", search)
                .output()
                .unwrap()
        };
        let present = run("codex", &search);
        assert!(String::from_utf8_lossy(&present.stdout).starts_with("riwork cua harness codex"));
        fs::remove_file(&executable).unwrap();
        for harness in ["codex", "claude", "grok"] {
            let output = run(harness, &search);
            assert!(output.status.success(), "{output:?}");
            assert_eq!(
                String::from_utf8_lossy(&output.stdout),
                format!("real {harness} a b --flag\n")
            );
            assert!(String::from_utf8_lossy(&output.stderr).contains("is missing"));
        }
        // With no other CLI the shim cannot loop on itself.
        let output = run("codex", &format!("{}:relative", bin.display()));
        assert_eq!(output.status.code(), Some(127));
        assert!(String::from_utf8_lossy(&output.stderr).contains("not installed on PATH"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_real_requirement_rejects_a_lookalike_signed_with_another_identity() {
        let temp = TemporaryDirectory::new(&env::temp_dir(), "riwork-cua-test").unwrap();
        let app = temp.path.join("CuaDriver.app");
        let executable = app.join("Contents/MacOS/cua-driver");
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        // The bundle identifier alone must not be enough.
        fs::write(
            app.join("Contents/Info.plist"),
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict>\
                 <key>CFBundleIdentifier</key><string>{APP_BUNDLE_ID}</string>\
                 <key>CFBundleExecutable</key><string>cua-driver</string>\
                 <key>CFBundlePackageType</key><string>APPL</string></dict></plist>"
            ),
        )
        .unwrap();
        fs::copy("/bin/echo", &executable).unwrap();
        let signed = Command::new("/usr/bin/codesign")
            .args(["--force", "--sign", "-", "--identifier", APP_BUNDLE_ID])
            .arg(&app)
            .output()
            .unwrap();
        assert!(signed.status.success(), "{signed:?}");
        match AppVerifier::system().verify(&app).unwrap() {
            Verdict::Rejected(reason) => {
                assert!(reason.contains("code signature check failed"), "{reason}")
            }
            Verdict::Trusted => panic!("an ad-hoc lookalike was trusted"),
        }
    }
}
