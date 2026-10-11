//! RiWork's shared Cua.ai Driver installation and app-owned desktop runtime.

use std::{
    env,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio},
    sync::{
        Arc, Mutex, MutexGuard, TryLockError,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
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
/// Bound for the preflight handshake. The desktop service is already up, so
/// this only covers the MCP proxy, and it stays well under Grok's limits.
const MCP_READY_TIMEOUT: Duration = Duration::from_secs(20);
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

    /// Start the desktop service, if it is down, and complete an MCP handshake
    /// before Grok is launched. Grok's own clock has not started yet. Closing
    /// the probe's stdin ends the MCP proxy; this does not stop the service.
    pub fn prepare_for_grok(&self) -> Result<(), String> {
        self.prepare_for_grok_within(MCP_READY_TIMEOUT)
    }

    fn prepare_for_grok_within(&self, handshake_timeout: Duration) -> Result<(), String> {
        let started = Instant::now();
        let result = (|| {
            let driver = self.trusted_driver()?;
            self.ensure_started()?;
            handshake_mcp(&driver, handshake_timeout)
        })();
        let log = self.home.join("cua/driver.log");
        match result {
            Ok(()) => {
                self.record_driver_log(&format!(
                    "Grok preflight ready in {:.1}s",
                    started.elapsed().as_secs_f32()
                ));
                Ok(())
            }
            Err(cause) => {
                let explained = explain_grok_preflight(&cause, started.elapsed(), &log);
                self.record_driver_log(&explained);
                Err(explained)
            }
        }
    }

    fn record_driver_log(&self, message: &str) {
        if let Ok(mut log) = self.log_file("driver.log") {
            let _ = writeln!(log, "riwork: {message}");
        }
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

    /// Serve Cua Driver over MCP stdio. Stdout carries only the protocol.
    ///
    /// This process stays between the agent and `cua-driver mcp` instead of
    /// replacing itself with it. The driver's MCP process dies whenever the
    /// shared desktop service ends, and an exec'd agent connection would die
    /// with it for good. The proxy restarts the service and the driver, replays
    /// the agent's handshake and carries on. Grok launches call
    /// `prepare_for_grok` first, so the first start is not racing Grok's
    /// 30-second limit. Codex and a direct `riwork cua mcp` still start the
    /// service here.
    pub fn run_mcp(&self) -> Result<(), String> {
        self.run_mcp_with(
            McpIo {
                input: Box::new(BufReader::with_capacity(64 * 1024, io::stdin())),
                output: Box::new(io::stdout()),
                stderr: Box::new(io::stderr()),
            },
            &McpPolicy::standard(),
        )
    }

    fn run_mcp_with(&self, io: McpIo, policy: &McpPolicy) -> Result<(), String> {
        // Each start re-runs the signature-checked path, so a driver replaced
        // while the proxy was up is the one that gets launched.
        let prepare = || {
            self.ensure_started()?;
            self.trusted_driver()
        };
        let log = self.clone();
        run_mcp_proxy(
            &prepare,
            io,
            policy,
            Box::new(move |message| log.record_driver_log(&format!("MCP proxy: {message}"))),
        )
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

fn explain_grok_preflight(cause: &str, elapsed: Duration, log: &Path) -> String {
    format!(
        "{cause} RiWork starts Cua Driver and completes its MCP handshake before launching Grok, because Grok's default MCP startup limit is 30 seconds. This check took {:.1} seconds. Log: {}. Run `riwork cua status`.",
        elapsed.as_secs_f32(),
        log.display()
    )
}

/// Speak the newline-delimited JSON-RPC that `cua-driver mcp` answers.
/// `initialize` then `tools/list` is the same handshake Grok performs.
fn handshake_mcp(driver: &Path, timeout: Duration) -> Result<(), String> {
    let mut command = Command::new(driver);
    command
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    detach_process(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("Cannot start Cua Driver MCP handshake: {error}"))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "Cannot write to Cua Driver MCP".to_owned())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Cannot read Cua Driver MCP".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Cannot read Cua Driver MCP errors".to_owned())?;
    let stderr_text = collect_stderr(stderr);
    let lines = spawn_stdout_lines(stdout);
    let exchange = mcp_exchange(&mut child, stdin, &lines, timeout);
    finish_mcp_child(&mut child);
    let stderr_text = stderr_text
        .recv_timeout(Duration::from_millis(200))
        .unwrap_or_default();
    exchange.map_err(|error| append_driver_stderr(error, &stderr_text))
}

fn mcp_exchange(
    child: &mut Child,
    mut stdin: ChildStdin,
    lines: &mpsc::Receiver<std::io::Result<String>>,
    timeout: Duration,
) -> Result<(), String> {
    let started = Instant::now();
    let result = (|| {
        write_mcp(
            &mut stdin,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": "riwork", "version": "0" }
                }
            }),
        )?;
        let initialized = read_mcp_response(child, lines, started, timeout, 1)?;
        if initialized.get("result").is_none() {
            return Err("Cua Driver MCP answered initialize without a result".to_owned());
        }
        write_mcp(
            &mut stdin,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            }),
        )?;
        write_mcp(
            &mut stdin,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/list",
                "params": {}
            }),
        )?;
        let listed = read_mcp_response(child, lines, started, timeout, 2)?;
        let tools = listed
            .pointer("/result/tools")
            .and_then(Value::as_array)
            .ok_or_else(|| "Cua Driver MCP answered tools/list without a tools array".to_owned())?;
        if tools.is_empty() {
            return Err("Cua Driver MCP answered tools/list with no tools".to_owned());
        }
        Ok(())
    })();
    // End of input asks the MCP proxy to exit. The desktop service stays up.
    drop(stdin);
    result
}

fn write_mcp(stdin: &mut ChildStdin, value: &Value) -> Result<(), String> {
    let mut line = serde_json::to_string(value)
        .map_err(|error| format!("Cannot encode Cua Driver MCP request: {error}"))?;
    line.push('\n');
    stdin
        .write_all(line.as_bytes())
        .map_err(|error| format!("Cannot write to Cua Driver MCP: {error}"))?;
    stdin
        .flush()
        .map_err(|error| format!("Cannot write to Cua Driver MCP: {error}"))
}

fn read_mcp_response(
    child: &mut Child,
    lines: &mpsc::Receiver<std::io::Result<String>>,
    started: Instant,
    timeout: Duration,
    id: i64,
) -> Result<Value, String> {
    let deadline = started + timeout;
    loop {
        if Instant::now() >= deadline {
            return Err(format!(
                "Cua Driver MCP did not answer request {id} within {:.1} seconds",
                timeout.as_secs_f32()
            ));
        }
        let incoming = match lines.try_recv() {
            Ok(line) => Some(line),
            Err(mpsc::TryRecvError::Empty) => match child.try_wait() {
                Ok(Some(status)) => {
                    thread::sleep(Duration::from_millis(30));
                    match lines.try_recv() {
                        Ok(line) => Some(line),
                        Err(_) => {
                            return Err(format!(
                                "Cua Driver MCP exited before answering request {id} ({status})"
                            ));
                        }
                    }
                }
                Ok(None) => {
                    thread::sleep(Duration::from_millis(20));
                    None
                }
                Err(error) => return Err(format!("Cannot wait for Cua Driver MCP: {error}")),
            },
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err(format!(
                    "Cua Driver MCP closed its output before answering request {id}"
                ));
            }
        };
        if let Some(line) = incoming {
            let line = line.map_err(|error| format!("Cannot read Cua Driver MCP: {error}"))?;
            match mcp_message(line, id)? {
                Some(value) => return Ok(value),
                None => continue,
            }
        }
    }
}

/// `Ok(None)` is a notification or a response for a different request.
fn mcp_message(line: String, id: i64) -> Result<Option<Value>, String> {
    let line = line.trim();
    if line.is_empty() {
        return Ok(None);
    }
    let value: Value = serde_json::from_str(line).map_err(|error| {
        format!(
            "Cua Driver MCP sent non-JSON output ({error}): {}",
            snippet(line)
        )
    })?;
    if let Some(error) = value.get("error") {
        let matches = value.get("id").and_then(Value::as_i64) == Some(id);
        let unnamed = value.get("id").is_none_or(Value::is_null);
        if matches || unnamed {
            return Err(format!(
                "Cua Driver MCP rejected request {id}: {}",
                snippet(&error.to_string())
            ));
        }
    }
    if value.get("id").and_then(Value::as_i64) == Some(id) {
        Ok(Some(value))
    } else {
        Ok(None)
    }
}

fn spawn_stdout_lines(
    stdout: impl Read + Send + 'static,
) -> mpsc::Receiver<std::io::Result<String>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if sender.send(Ok(line)).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let _ = sender.send(Err(error));
                    break;
                }
            }
        }
    });
    receiver
}

fn collect_stderr(stderr: impl Read + Send + 'static) -> mpsc::Receiver<String> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut buffer = String::new();
        let mut chunk = [0_u8; 1024];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(size) => {
                    if buffer.len() < 2000 {
                        let room = 2000 - buffer.len();
                        buffer.push_str(&String::from_utf8_lossy(&chunk[..size.min(room)]));
                    }
                }
            }
        }
        let _ = sender.send(buffer);
    });
    receiver
}

fn finish_mcp_child(child: &mut Child) {
    let deadline = Instant::now() + Duration::from_millis(500);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            _ => {
                terminate_child(child);
                return;
            }
        }
    }
}

fn append_driver_stderr(error: String, stderr: &str) -> String {
    let stderr = stderr.trim();
    if stderr.is_empty() {
        error
    } else {
        format!("{error} Driver said: {}", snippet(stderr))
    }
}

fn snippet(text: &str) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= 180 {
        compact
    } else {
        let end = compact
            .char_indices()
            .nth(180)
            .map(|(index, _)| index)
            .unwrap_or(compact.len());
        format!("{}…", &compact[..end])
    }
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

// The MCP stdio proxy.
//
// `riwork cua mcp` sits between an agent and `cua-driver mcp`. MCP stdio is
// newline-delimited JSON-RPC, so the proxy forwards whole lines verbatim, in
// order, and looks inside them only to keep the bookkeeping a restart needs:
// which client requests have no answer yet, the client's `initialize`
// parameters, and whether it sent `notifications/initialized`.
//
// When the driver's MCP process ends while the client is still connected, the
// proxy answers every unanswered request with a JSON-RPC error, makes sure the
// desktop service is running, starts a new driver, replays the handshake and
// carries on. Data flows on three kinds of thread: one pumping the client's
// input, one per driver pumping its output, and the calling thread, which
// supervises. Locks are taken in the order child_input, output, state; `state`
// is never held across a write.

/// Longest single MCP message (one line) buffered in either direction.
const MCP_LINE_LIMIT: usize = 16 * 1024 * 1024;
/// Client requests the proxy tracks at once; more than a driver is ever sent.
const MCP_MAX_IN_FLIGHT: usize = 4096;
/// JSON-RPC server error code used for every error the proxy makes up.
const MCP_PROXY_ERROR: i64 = -32000;
/// JSON-RPC "invalid request", used when a client line is too long to read.
const MCP_INVALID_REQUEST: i64 = -32600;
const MCP_RESTART_MESSAGE: &str = "Cua Driver restarted; retry the request";
const MCP_POLL: Duration = Duration::from_millis(100);
/// How long a driver whose output has closed may take to exit by itself.
const MCP_EXIT_GRACE: Duration = Duration::from_millis(200);
/// How long output still in flight from a driver that has exited may take to arrive.
const MCP_DRAIN_GRACE: Duration = Duration::from_millis(300);

struct McpIo {
    input: Box<dyn BufRead + Send>,
    output: Box<dyn Write + Send>,
    stderr: Box<dyn Write + Send>,
}

#[derive(Clone, Debug)]
struct McpPolicy {
    /// Pauses before successive restart attempts; the last one repeats.
    backoff: Vec<Duration>,
    /// A driver that stayed up this long starts the schedule over.
    stable_after: Duration,
    /// How long a new driver has to answer the replayed `initialize`.
    replay_timeout: Duration,
    line_limit: usize,
}

impl McpPolicy {
    fn standard() -> Self {
        Self {
            backoff: [1, 2, 5, 10, 30].map(Duration::from_secs).to_vec(),
            stable_after: Duration::from_secs(30),
            replay_timeout: MCP_READY_TIMEOUT,
            line_limit: MCP_LINE_LIMIT,
        }
    }

    fn delay(&self, attempt: usize) -> Duration {
        self.backoff
            .get(attempt)
            .or(self.backoff.last())
            .copied()
            .unwrap_or(Duration::from_secs(1))
    }
}

/// The parts of a JSON-RPC message the proxy needs. Payloads are skipped
/// without being built, so a screenshot response costs no copy.
#[derive(Deserialize)]
struct Envelope {
    id: Option<Value>,
    method: Option<String>,
    result: Option<serde::de::IgnoredAny>,
    error: Option<serde::de::IgnoredAny>,
}

impl Envelope {
    fn request_id(&self) -> Option<&Value> {
        self.method.as_ref().and(self.id.as_ref())
    }

    fn response_id(&self) -> Option<&Value> {
        self.method.as_ref().map_or(self.id.as_ref(), |_| None)
    }

    fn succeeded(&self) -> bool {
        self.result.is_some() && self.error.is_none()
    }
}

/// `None` for anything that is not a JSON object or array of them.
fn parse_envelopes(line: &[u8]) -> Option<Vec<Envelope>> {
    match line.iter().find(|byte| !byte.is_ascii_whitespace())? {
        b'{' => serde_json::from_slice::<Envelope>(line)
            .ok()
            .map(|envelope| vec![envelope]),
        b'[' => serde_json::from_slice::<Vec<Envelope>>(line).ok(),
        _ => None,
    }
}

fn error_line(id: &Value, code: i64, message: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    }))
    .unwrap_or_default()
}

fn write_line<W: Write + ?Sized>(writer: &mut W, line: &[u8]) -> io::Result<()> {
    writer.write_all(line)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

enum Frame {
    Line(Vec<u8>),
    /// A line over the limit, discarded up to its newline without being kept.
    TooLong,
    Eof,
}

/// Read one line, without its newline, holding at most `limit` bytes of it.
fn read_frame(reader: &mut dyn BufRead, limit: usize) -> io::Result<Frame> {
    let mut line = Vec::new();
    let mut too_long = false;
    loop {
        let buffer = match reader.fill_buf() {
            Ok(buffer) => buffer,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if buffer.is_empty() {
            return Ok(if too_long {
                Frame::TooLong
            } else if line.is_empty() {
                Frame::Eof
            } else {
                Frame::Line(line)
            });
        }
        let newline = buffer.iter().position(|byte| *byte == b'\n');
        let chunk = &buffer[..newline.unwrap_or(buffer.len())];
        if !too_long {
            if line.len() + chunk.len() > limit {
                too_long = true;
                line = Vec::new();
            } else {
                line.extend_from_slice(chunk);
            }
        }
        let used = chunk.len() + usize::from(newline.is_some());
        reader.consume(used);
        if newline.is_some() {
            return Ok(if too_long {
                Frame::TooLong
            } else {
                Frame::Line(line)
            });
        }
    }
}

enum McpEvent {
    /// The client closed its input, or its output stopped accepting messages.
    ClientGone,
    /// The driver of this epoch stopped producing output (or broke the protocol).
    ChildEnded {
        epoch: u64,
        reason: String,
        oversized: bool,
    },
    /// The driver of this epoch answered the proxy's own `initialize`.
    InitReplied {
        epoch: u64,
        ok: bool,
        list_changed: bool,
    },
}

/// Everything the proxy remembers about the session. Each driver process
/// belongs to an epoch; anything it says after the epoch moves on is dropped.
#[derive(Default)]
struct McpState {
    epoch: u64,
    /// Client traffic reaches the driver only while this is set.
    ready: bool,
    /// Ids of client requests the driver has not answered, in arrival order.
    in_flight: Vec<Value>,
    /// The client's `initialize` request still waiting for its answer.
    initializing: Option<(Value, Value)>,
    /// Params of the `initialize` the driver accepted for this client.
    initialize: Option<Value>,
    initialized: bool,
    /// Id of the proxy's own `initialize` while its answer is pending.
    replay_id: Option<Value>,
}

impl McpState {
    fn take_in_flight(&mut self, id: &Value) {
        if let Some(position) = self.in_flight.iter().position(|pending| pending == id) {
            self.in_flight.remove(position);
        }
    }
}

enum DriverLine {
    Forward,
    Drop,
    Replay { ok: bool },
}

impl McpState {
    fn on_driver_message(&mut self, epoch: u64, envelopes: &[Envelope]) -> DriverLine {
        if epoch != self.epoch {
            return DriverLine::Drop;
        }
        if let [only] = envelopes
            && let Some(id) = only.response_id()
            && self.replay_id.as_ref() == Some(id)
        {
            self.replay_id = None;
            return DriverLine::Replay {
                ok: only.succeeded(),
            };
        }
        for envelope in envelopes {
            let Some(id) = envelope.response_id() else {
                continue;
            };
            self.take_in_flight(id);
            if self
                .initializing
                .as_ref()
                .is_some_and(|(pending, _)| pending == id)
                && let Some((_, params)) = self.initializing.take()
                && envelope.succeeded()
            {
                self.initialize = Some(params);
                self.initialized = false;
            }
        }
        DriverLine::Forward
    }
}

struct McpProxy {
    /// The client's stdout. One message at a time.
    output: Mutex<Box<dyn Write + Send>>,
    stderr: Mutex<Box<dyn Write + Send>>,
    /// The current driver's stdin and the epoch it belongs to.
    child_input: Mutex<Option<(u64, ChildStdin)>>,
    state: Mutex<McpState>,
    events: mpsc::Sender<McpEvent>,
    on_note: Box<dyn Fn(&str) + Send + Sync>,
    line_limit: usize,
    client_gone: AtomicBool,
    output_failed: AtomicBool,
}

struct LiveChild {
    process: Child,
    epoch: u64,
    started: Instant,
}

enum StartError {
    ClientGone,
    Failed(String),
}

enum Watch {
    ClientGone,
    Ended { reason: String, oversized: bool },
}

fn run_mcp_proxy(
    prepare: &dyn Fn() -> Result<PathBuf, String>,
    io: McpIo,
    policy: &McpPolicy,
    on_note: Box<dyn Fn(&str) + Send + Sync>,
) -> Result<(), String> {
    let (sender, events) = mpsc::channel();
    let proxy = Arc::new(McpProxy {
        output: Mutex::new(io.output),
        stderr: Mutex::new(io.stderr),
        child_input: Mutex::new(None),
        state: Mutex::new(McpState::default()),
        events: sender,
        on_note,
        line_limit: policy.line_limit,
        client_gone: AtomicBool::new(false),
        output_failed: AtomicBool::new(false),
    });
    // Failing to start at all is reported the way an exec failure was: the
    // agent's launcher shows this message. Later failures are retried.
    let child = match proxy.start_child(prepare, policy, &events) {
        Ok(child) => child,
        Err(StartError::Failed(message)) => return Err(message),
        Err(StartError::ClientGone) => return Ok(()),
    };
    let input = io.input;
    let reader = Arc::clone(&proxy);
    thread::spawn(move || reader.pump_client(input));
    proxy.supervise(prepare, policy, &events, child);
    if proxy.output_failed.load(Ordering::SeqCst) {
        return Err("The MCP client stopped reading Cua Driver's output".to_owned());
    }
    Ok(())
}

impl McpProxy {
    fn note(&self, message: &str) {
        (self.on_note)(message);
        self.write_stderr(&format!("riwork: {message}\n"));
    }

    /// Text the driver put on stdout that is not an MCP message must not
    /// reach the client, whose parser would fail on it.
    fn stray_output(&self, line: &[u8]) {
        let text = String::from_utf8_lossy(line);
        self.write_stderr(&format!(
            "riwork: ignored non-MCP output from Cua Driver: {}\n",
            snippet(&text)
        ));
    }

    /// Best effort. An agent that stops draining stderr blocks the driver's
    /// stderr relay while it holds this lock; that must not stall a restart.
    fn write_stderr(&self, text: &str) {
        let deadline = Instant::now() + Duration::from_millis(250);
        let mut sink = loop {
            match self.stderr.try_lock() {
                Ok(sink) => break sink,
                Err(TryLockError::Poisoned(poisoned)) => break poisoned.into_inner(),
                Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(TryLockError::WouldBlock) => return,
            }
        };
        let _ = sink.write_all(text.as_bytes());
        let _ = sink.flush();
    }

    /// Callers hold `output`, so a message is never interleaved with another.
    fn write_message(&self, output: &mut dyn Write, line: &[u8]) {
        if write_line(output, line).is_err() && !self.client_gone.swap(true, Ordering::SeqCst) {
            self.output_failed.store(true, Ordering::SeqCst);
            let _ = self.events.send(McpEvent::ClientGone);
        }
    }

    fn reject(&self, ids: &[Value], code: i64, message: &str) {
        if ids.is_empty() {
            return;
        }
        let mut output = locked(&self.output);
        for id in ids {
            self.write_message(&mut **output, &error_line(id, code, message));
        }
    }

    fn pump_client(&self, mut input: Box<dyn BufRead + Send>) {
        loop {
            match read_frame(&mut *input, self.line_limit) {
                Ok(Frame::Line(line)) => {
                    if !line.iter().all(u8::is_ascii_whitespace) {
                        self.forward_client_line(&line);
                    }
                }
                Ok(Frame::TooLong) => {
                    // Its id was never read, so the error cannot name one.
                    self.note(&format!(
                        "dropped an MCP message from the client longer than {} bytes",
                        self.line_limit
                    ));
                    self.reject(
                        &[Value::Null],
                        MCP_INVALID_REQUEST,
                        &format!("MCP message exceeds the {} byte limit", self.line_limit),
                    );
                }
                Ok(Frame::Eof) => break,
                Err(error) => {
                    self.note(&format!("cannot read the MCP client's input: {error}"));
                    break;
                }
            }
        }
        self.client_gone.store(true, Ordering::SeqCst);
        let _ = self.events.send(McpEvent::ClientGone);
    }

    fn forward_client_line(&self, line: &[u8]) {
        let envelopes = parse_envelopes(line);
        let requests: Vec<Value> = envelopes
            .iter()
            .flatten()
            .filter_map(|envelope| envelope.request_id().cloned())
            .collect();
        let initialized = envelopes
            .iter()
            .flatten()
            .any(|envelope| envelope.method.as_deref() == Some("notifications/initialized"));
        // A cancelled request may never be answered; do not wait for it.
        let cancelled = match envelopes.as_deref() {
            Some([only]) if only.method.as_deref() == Some("notifications/cancelled") => {
                serde_json::from_slice::<Value>(line)
                    .ok()
                    .and_then(|message| message.pointer("/params/requestId").cloned())
            }
            _ => None,
        };
        let initialize = match envelopes.as_deref() {
            Some([only]) if only.method.as_deref() == Some("initialize") => only
                .id
                .clone()
                .zip(serde_json::from_slice::<Value>(line).ok()),
            _ => None,
        }
        .map(|(id, message)| (id, message.get("params").cloned().unwrap_or(Value::Null)));

        // The writer lock orders this against a restart's own writes.
        let mut slot = locked(&self.child_input);
        let verdict = {
            let mut state = locked(&self.state);
            if initialized {
                state.initialized = true;
            }
            if let Some(id) = &cancelled {
                state.take_in_flight(id);
            }
            if !state.ready {
                Err(MCP_RESTART_MESSAGE)
            } else if state.in_flight.len() + requests.len() > MCP_MAX_IN_FLIGHT {
                Err("Too many MCP requests are waiting for Cua Driver")
            } else {
                state.in_flight.extend(requests.iter().cloned());
                state.initializing = initialize.or(state.initializing.take());
                Ok(state.epoch)
            }
        };
        let epoch = match verdict {
            Ok(epoch) => epoch,
            Err(message) => {
                drop(slot);
                self.reject(&requests, MCP_PROXY_ERROR, message);
                return;
            }
        };
        let written = match slot.as_mut() {
            Some((current, stdin)) if *current == epoch => write_line(stdin, line),
            // The driver is being replaced, and the restart answers these.
            _ => return,
        };
        drop(slot);
        if written.is_err() {
            self.fail_requests(epoch, &requests);
            let _ = self.events.send(McpEvent::ChildEnded {
                epoch,
                reason: "stopped reading its input".to_owned(),
                oversized: false,
            });
        }
    }

    /// The driver did not take these requests; answer those still unanswered.
    fn fail_requests(&self, epoch: u64, ids: &[Value]) {
        let mut output = locked(&self.output);
        let failed: Vec<Value> = {
            let mut state = locked(&self.state);
            if state.epoch != epoch {
                return;
            }
            ids.iter()
                .filter(|id| {
                    let before = state.in_flight.len();
                    state.take_in_flight(id);
                    state.in_flight.len() != before
                })
                .cloned()
                .collect()
        };
        for id in &failed {
            self.write_message(
                &mut **output,
                &error_line(id, MCP_PROXY_ERROR, MCP_RESTART_MESSAGE),
            );
        }
    }

    fn pump_child(&self, epoch: u64, stdout: ChildStdout) {
        let mut reader = BufReader::with_capacity(64 * 1024, stdout);
        let (reason, oversized) = loop {
            match read_frame(&mut reader, self.line_limit) {
                Ok(Frame::Line(line)) => self.forward_child_line(epoch, &line),
                Ok(Frame::TooLong) => {
                    break (
                        format!("sent a message over the {} byte limit", self.line_limit),
                        true,
                    );
                }
                Ok(Frame::Eof) => break ("closed its output".to_owned(), false),
                Err(error) => break (format!("failed reading its output ({error})"), false),
            }
        };
        let _ = self.events.send(McpEvent::ChildEnded {
            epoch,
            reason,
            oversized,
        });
    }

    fn forward_child_line(&self, epoch: u64, line: &[u8]) {
        if line.iter().all(u8::is_ascii_whitespace) {
            return;
        }
        let Some(envelopes) = parse_envelopes(line) else {
            self.stray_output(line);
            return;
        };
        let mut output = locked(&self.output);
        let action = locked(&self.state).on_driver_message(epoch, &envelopes);
        match action {
            DriverLine::Forward => self.write_message(&mut **output, line),
            DriverLine::Drop => {}
            DriverLine::Replay { ok } => {
                drop(output);
                let list_changed = serde_json::from_slice::<Value>(line)
                    .ok()
                    .and_then(|message| {
                        message
                            .pointer("/result/capabilities/tools/listChanged")
                            .and_then(Value::as_bool)
                    })
                    .unwrap_or(false);
                let _ = self.events.send(McpEvent::InitReplied {
                    epoch,
                    ok,
                    list_changed,
                });
            }
        }
    }

    /// Answer every client request the ended driver left unanswered and start
    /// a new epoch, so nothing the old driver says can arrive afterwards.
    fn sweep(&self, message: &str) {
        let mut output = locked(&self.output);
        let ids = {
            let mut state = locked(&self.state);
            state.ready = false;
            state.epoch += 1;
            state.replay_id = None;
            state.initializing = None;
            std::mem::take(&mut state.in_flight)
        };
        for id in &ids {
            self.write_message(&mut **output, &error_line(id, MCP_PROXY_ERROR, message));
        }
    }

    /// Make sure the desktop service is up, start a driver and, when the
    /// client had completed its handshake, replay it.
    fn start_child(
        self: &Arc<Self>,
        prepare: &dyn Fn() -> Result<PathBuf, String>,
        policy: &McpPolicy,
        events: &mpsc::Receiver<McpEvent>,
    ) -> Result<LiveChild, StartError> {
        let driver = prepare().map_err(StartError::Failed)?;
        if self.client_gone.load(Ordering::SeqCst) {
            return Err(StartError::ClientGone);
        }
        let epoch = locked(&self.state).epoch;
        let mut command = Command::new(&driver);
        command
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        detach_process(&mut command);
        let mut process = command.spawn().map_err(|error| {
            StartError::Failed(format!("Cannot launch Cua Driver MCP: {error}"))
        })?;
        let (Some(stdin), Some(stdout), Some(stderr)) = (
            process.stdin.take(),
            process.stdout.take(),
            process.stderr.take(),
        ) else {
            terminate_child(&mut process);
            return Err(StartError::Failed(
                "Cannot connect to Cua Driver MCP".to_owned(),
            ));
        };
        *locked(&self.child_input) = Some((epoch, stdin));
        let reader = Arc::clone(self);
        thread::spawn(move || reader.pump_child(epoch, stdout));
        let relay = Arc::clone(self);
        thread::spawn(move || relay.pump_child_stderr(stderr));
        let mut child = LiveChild {
            process,
            epoch,
            started: Instant::now(),
        };
        match self.bring_up(&mut child, policy, events) {
            Ok(()) => Ok(child),
            Err(error) => {
                self.abandon(child);
                Err(error)
            }
        }
    }
}

impl McpProxy {
    fn pump_child_stderr(&self, mut stderr: ChildStderr) {
        let mut buffer = [0_u8; 8192];
        loop {
            match stderr.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(size) => {
                    let mut sink = locked(&self.stderr);
                    let _ = sink.write_all(&buffer[..size]);
                    let _ = sink.flush();
                }
            }
        }
    }

    /// Open the new driver to client traffic, after replaying the handshake
    /// the client had already completed with the old one.
    fn bring_up(
        &self,
        child: &mut LiveChild,
        policy: &McpPolicy,
        events: &mpsc::Receiver<McpEvent>,
    ) -> Result<(), StartError> {
        let Some(params) = locked(&self.state).initialize.clone() else {
            locked(&self.state).ready = true;
            return Ok(());
        };
        let id = Value::String(format!("riwork-cua-proxy-initialize-{}", child.epoch));
        let mut request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
        });
        if !params.is_null() {
            request["params"] = params;
        }
        let request = serde_json::to_vec(&request)
            .map_err(|error| StartError::Failed(format!("Cannot replay initialize: {error}")))?;
        {
            let mut slot = locked(&self.child_input);
            locked(&self.state).replay_id = Some(id);
            let Some((_, stdin)) = slot.as_mut() else {
                return Err(StartError::Failed("Cua Driver MCP has no input".to_owned()));
            };
            write_line(stdin, &request).map_err(|error| {
                StartError::Failed(format!(
                    "Cannot replay initialize to Cua Driver MCP: {error}"
                ))
            })?;
        }
        let (ok, list_changed) = self.await_replay(child, policy, events)?;
        if !ok {
            return Err(StartError::Failed(
                "Cua Driver MCP rejected the replayed initialize".to_owned(),
            ));
        }
        {
            // Under the writer lock, the client's own `initialized` either
            // is seen here or is forwarded once the driver is ready, not both.
            let mut slot = locked(&self.child_input);
            let replay_initialized = locked(&self.state).initialized;
            if replay_initialized {
                let notification = br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
                let Some((_, stdin)) = slot.as_mut() else {
                    return Err(StartError::Failed("Cua Driver MCP has no input".to_owned()));
                };
                write_line(stdin, notification).map_err(|error| {
                    StartError::Failed(format!("Cannot replay initialized: {error}"))
                })?;
            }
            locked(&self.state).ready = true;
        }
        if list_changed {
            let mut output = locked(&self.output);
            self.write_message(
                &mut **output,
                br#"{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}"#,
            );
        }
        Ok(())
    }

    /// `(succeeded, tools may have changed)` from the driver's answer.
    fn await_replay(
        &self,
        child: &mut LiveChild,
        policy: &McpPolicy,
        events: &mpsc::Receiver<McpEvent>,
    ) -> Result<(bool, bool), StartError> {
        let deadline = Instant::now() + policy.replay_timeout;
        loop {
            if self.client_gone.load(Ordering::SeqCst) {
                return Err(StartError::ClientGone);
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return Err(StartError::Failed(format!(
                    "Cua Driver MCP did not answer the replayed initialize within {:.1} seconds",
                    policy.replay_timeout.as_secs_f32()
                )));
            };
            match events.recv_timeout(left.min(MCP_POLL)) {
                Ok(McpEvent::InitReplied {
                    epoch,
                    ok,
                    list_changed,
                }) if epoch == child.epoch => return Ok((ok, list_changed)),
                Ok(McpEvent::ChildEnded { epoch, reason, .. }) if epoch == child.epoch => {
                    return Err(StartError::Failed(format!(
                        "Cua Driver MCP {reason} before answering the replayed initialize"
                    )));
                }
                Ok(_) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if let Ok(Some(status)) = child.process.try_wait() {
                        return Err(StartError::Failed(format!(
                            "Cua Driver MCP exited ({status}) before answering the replayed initialize"
                        )));
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(StartError::ClientGone),
            }
        }
    }

    /// Stop a driver that has ended, or is being given up on, and detach it.
    /// Returns how it ended, for the log.
    fn retire(&self, mut child: LiveChild) -> String {
        let deadline = Instant::now() + MCP_EXIT_GRACE;
        let detail = loop {
            match child.process.try_wait() {
                Ok(Some(status)) => break format!("{status}"),
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                _ => {
                    // Only a driver that has not been reaped: a reaped pid may
                    // already belong to someone else.
                    terminate_child(&mut child.process);
                    break "stopped by RiWork".to_owned();
                }
            }
        };
        *locked(&self.child_input) = None;
        detail
    }

    /// Give up on a driver that never became ready. The epoch moves on so
    /// whatever it still says is dropped.
    fn abandon(&self, child: LiveChild) {
        self.retire(child);
        self.sweep(MCP_RESTART_MESSAGE);
    }

    fn watch(&self, child: &mut LiveChild, events: &mpsc::Receiver<McpEvent>) -> Watch {
        loop {
            if self.client_gone.load(Ordering::SeqCst) {
                return Watch::ClientGone;
            }
            match events.recv_timeout(MCP_POLL) {
                Ok(McpEvent::ClientGone) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Watch::ClientGone;
                }
                Ok(McpEvent::ChildEnded {
                    epoch,
                    reason,
                    oversized,
                }) if epoch == child.epoch => return Watch::Ended { reason, oversized },
                Ok(_) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if matches!(child.process.try_wait(), Ok(Some(_))) {
                        // Its last words may still be on their way.
                        self.await_output_end(child.epoch, events);
                        return Watch::Ended {
                            reason: "exited".to_owned(),
                            oversized: false,
                        };
                    }
                }
            }
        }
    }

    fn await_output_end(&self, epoch: u64, events: &mpsc::Receiver<McpEvent>) {
        let deadline = Instant::now() + MCP_DRAIN_GRACE;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match events.recv_timeout(left) {
                Ok(McpEvent::ChildEnded { epoch: ended, .. }) if ended == epoch => return,
                Ok(_) => {}
                Err(_) => return,
            }
        }
    }

    /// Wait out `delay`. True if the client left in the meantime.
    fn pause(&self, delay: Duration, events: &mpsc::Receiver<McpEvent>) -> bool {
        let deadline = Instant::now() + delay;
        loop {
            if self.client_gone.load(Ordering::SeqCst) {
                return true;
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            let _ = events.recv_timeout(left);
        }
    }

    /// The client is gone: close the driver's input, and stop it if it does
    /// not exit by itself.
    fn shutdown(&self, child: LiveChild, events: &mpsc::Receiver<McpEvent>) {
        let LiveChild {
            mut process, epoch, ..
        } = child;
        self.close_child_input(&mut process);
        finish_mcp_child(&mut process);
        self.await_output_end(epoch, events);
    }

    /// Close the driver's stdin. A writer stuck on a driver that stopped
    /// reading holds the lock, and only stopping the driver frees it.
    fn close_child_input(&self, process: &mut Child) {
        let mut slot = match self.child_input.try_lock() {
            Ok(slot) => slot,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => {
                if matches!(process.try_wait(), Ok(None)) {
                    terminate_child(process);
                }
                locked(&self.child_input)
            }
        };
        *slot = None;
    }

    fn supervise(
        self: &Arc<Self>,
        prepare: &dyn Fn() -> Result<PathBuf, String>,
        policy: &McpPolicy,
        events: &mpsc::Receiver<McpEvent>,
        mut child: LiveChild,
    ) {
        let mut attempt = 0;
        loop {
            match self.watch(&mut child, events) {
                Watch::ClientGone => {
                    self.shutdown(child, events);
                    return;
                }
                Watch::Ended { reason, oversized } => {
                    let lived = child.started.elapsed();
                    let detail = self.retire(child);
                    self.sweep(&if oversized {
                        format!(
                            "Cua Driver sent a message over the {} byte limit; the driver was restarted",
                            policy.line_limit
                        )
                    } else {
                        MCP_RESTART_MESSAGE.to_owned()
                    });
                    self.note(&format!("Cua Driver MCP {reason} ({detail}); restarting"));
                    if lived >= policy.stable_after {
                        attempt = 0;
                    }
                    child = loop {
                        let delay = policy.delay(attempt);
                        attempt += 1;
                        if self.pause(delay, events) {
                            return;
                        }
                        match self.start_child(prepare, policy, events) {
                            Ok(next) => {
                                self.note("Cua Driver MCP restarted");
                                break next;
                            }
                            Err(StartError::ClientGone) => return,
                            Err(StartError::Failed(cause)) => self.note(&format!(
                                "Cannot restart Cua Driver MCP: {cause}; retrying in {:?}",
                                policy.delay(attempt)
                            )),
                        }
                    };
                }
            }
        }
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
    #[ignore = "slow: runs a fake driver process"]
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
    #[ignore = "slow: real child process and a wall-clock bound"]
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
    #[ignore = "slow: runs the generated shims as real processes"]
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
    #[ignore = "slow: runs fake codesign/spctl and driver processes"]
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
    #[ignore = "slow: runs fake codesign/spctl and driver processes"]
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
    #[ignore = "slow: runs fake codesign/spctl and driver processes"]
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
    #[ignore = "slow: runs fake codesign/spctl and driver processes"]
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
    #[ignore = "slow: real installer process and wall-clock timeouts"]
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
    #[ignore = "slow: real driver process and a timed lock wait"]
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

    struct StopDaemon(PathBuf);

    impl Drop for StopDaemon {
        fn drop(&mut self) {
            let Ok(pid) = fs::read_to_string(&self.0) else {
                return;
            };
            let pid = pid.trim();
            if !pid.is_empty() {
                let _ = Command::new("/bin/kill").args(["-9", pid]).status();
            }
        }
    }

    /// A driver double that records each subcommand and speaks one MCP handshake.
    fn install_handshake_driver(path: &Path, marker: &Path, calls: &Path, mode: &str) {
        let script = r#"#!/usr/bin/python3
import json, os, sys, time
marker = "MARKER_PATH"
calls = "CALLS_PATH"
mode = "MODE"
def note(action):
    with open(calls, "a", encoding="utf-8") as handle:
        handle.write(action + "\n")
cmd = sys.argv[1] if len(sys.argv) > 1 else ""
note(cmd or "none")
if cmd == "--version":
    print("cua-driver test")
elif cmd == "status":
    if os.path.exists(marker):
        print("Cua Driver daemon is running")
        raise SystemExit(0)
    raise SystemExit(1)
elif cmd == "serve":
    with open(marker, "w", encoding="utf-8") as handle:
        handle.write(str(os.getpid()))
    while True:
        time.sleep(0.2)
elif cmd == "mcp":
    if mode == "sleep":
        time.sleep(30)
        raise SystemExit(0)
    for line in sys.stdin:
        message = json.loads(line)
        method = message.get("method")
        if method == "initialize":
            body = {"jsonrpc": "2.0", "id": message.get("id"), "result": {"capabilities": {"tools": {}}}}
            print(json.dumps(body), flush=True)
        elif method == "tools/list":
            tools = [] if mode == "empty" else [{"name": "probe_tool", "inputSchema": {"type": "object"}}]
            body = {"jsonrpc": "2.0", "id": message.get("id"), "result": {"tools": tools}}
            print(json.dumps(body), flush=True)
else:
    raise SystemExit(99)
"#;
        let script = script
            .replace("MARKER_PATH", &marker.display().to_string())
            .replace("CALLS_PATH", &calls.display().to_string())
            .replace("MODE", mode);
        atomic_executable(path, script.as_bytes()).unwrap();
    }

    fn handshake_manager(home: PathBuf, driver: PathBuf) -> CuaManager {
        let mut manager = manager(home);
        manager.override_driver = Some(driver);
        manager
    }

    #[test]
    #[ignore = "slow: real Python driver double"]
    fn grok_preflight_starts_a_down_driver_and_lists_its_tools() {
        let temp = TemporaryDirectory::new(&env::temp_dir(), "riwork-cua-test").unwrap();
        let driver = temp.path.join("driver");
        let marker = temp.path.join("running");
        let calls = temp.path.join("calls");
        install_handshake_driver(&driver, &marker, &calls, "ready");
        let _stop = StopDaemon(marker.clone());
        let manager = handshake_manager(temp.path.join("home"), driver);
        let started = Instant::now();
        manager.prepare_for_grok().unwrap();
        assert!(started.elapsed() < Duration::from_secs(5));
        let recorded = fs::read_to_string(&calls).unwrap();
        assert!(recorded.contains("serve\n"), "{recorded}");
        assert!(recorded.contains("mcp\n"), "{recorded}");
        let log = fs::read_to_string(temp.path.join("home/cua/driver.log")).unwrap();
        assert!(log.contains("Grok preflight ready"), "{log}");
        // A second launch finds the service already up and only handshakes.
        let before = recorded.matches("serve\n").count();
        manager.prepare_for_grok().unwrap();
        let recorded = fs::read_to_string(&calls).unwrap();
        assert_eq!(recorded.matches("serve\n").count(), before, "{recorded}");
        assert!(recorded.matches("mcp\n").count() >= 2, "{recorded}");
    }

    #[test]
    #[ignore = "slow: real Python driver double and a wall-clock bound"]
    fn grok_preflight_rejects_an_empty_tool_list_and_a_stuck_proxy() {
        let temp = TemporaryDirectory::new(&env::temp_dir(), "riwork-cua-test").unwrap();
        let marker = temp.path.join("running");
        fs::write(&marker, "1").unwrap();
        let empty = temp.path.join("empty");
        install_handshake_driver(&empty, &marker, &temp.path.join("empty-calls"), "empty");
        let manager = handshake_manager(temp.path.join("empty-home"), empty);
        let error = manager.prepare_for_grok().unwrap_err();
        assert!(error.contains("no tools"), "{error}");

        let stuck = temp.path.join("stuck");
        install_handshake_driver(&stuck, &marker, &temp.path.join("stuck-calls"), "sleep");
        let manager = handshake_manager(temp.path.join("stuck-home"), stuck);
        let started = Instant::now();
        let error = manager
            .prepare_for_grok_within(Duration::from_millis(400))
            .unwrap_err();
        assert!(
            error.contains("did not answer request 1 within 0.4 seconds"),
            "{error} after {:?}",
            started.elapsed()
        );
        // Status probes have their own 3-second bound, so a loaded machine can
        // spend several of those before the handshake budget starts. The 30-second
        // sleep in the double must not be what this call waits for.
        assert!(
            started.elapsed() < Duration::from_secs(12),
            "{error} after {:?}",
            started.elapsed()
        );
        assert!(error.contains("30 seconds"), "{error}");
    }

    #[test]
    fn grok_preflight_names_a_missing_driver() {
        let temp = TemporaryDirectory::new(&env::temp_dir(), "riwork-cua-test").unwrap();
        let manager = handshake_manager(temp.path.join("home"), temp.path.join("missing-driver"));
        let error = manager.prepare_for_grok().unwrap_err();
        assert!(error.contains("not an executable"), "{error}");
        assert!(error.contains("30 seconds"), "{error}");
        assert!(error.contains("riwork cua status"), "{error}");
    }

    #[test]
    #[ignore = "slow: runs the generated shims as real processes"]
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
    // ---- MCP stdio proxy ----

    /// A driver double for `riwork cua mcp`: `status`/`serve` behave like the
    /// daemon (a marker file), and `mcp` speaks enough MCP to be supervised.
    /// Each `mcp` start consumes the next word of the `plan` file, which picks
    /// how that process misbehaves: `ok`, `noisy`, `stubborn` (ignores EOF and
    /// SIGTERM) or `deaf` (never reads its input). Everything it does is logged
    /// to files in the directory.
    fn install_mcp_double(path: &Path, dir: &Path) {
        let script = r##"#!/usr/bin/python3
import json, os, signal, sys, time

d = "__DIR__"
marker = os.path.join(d, "running")

def log(name, text):
    with open(os.path.join(d, name), "a", encoding="utf-8") as handle:
        handle.write(text + "\n")

cmd = sys.argv[1] if len(sys.argv) > 1 else ""
if cmd == "--version":
    print("cua-driver test")
elif cmd == "status":
    if os.path.exists(marker):
        print("Cua Driver daemon is running")
        raise SystemExit(0)
    raise SystemExit(1)
elif cmd == "serve":
    log("calls", "serve")
    with open(marker, "w", encoding="utf-8") as handle:
        handle.write(str(os.getpid()))
    while True:
        time.sleep(0.2)
elif cmd == "mcp":
    step = "ok"
    plan = os.path.join(d, "plan")
    if os.path.exists(plan):
        with open(plan, encoding="utf-8") as handle:
            words = handle.read().split()
        if words:
            step = words[0]
            with open(plan, "w", encoding="utf-8") as handle:
                handle.write("\n".join(words[1:]))
    log("starts", "%f %s %d" % (time.time(), step, os.getpid()))
    with open(os.path.join(d, "mcp.pid"), "w", encoding="utf-8") as handle:
        handle.write(str(os.getpid()))
    print("driver says hello", file=sys.stderr, flush=True)

    def send(message):
        sys.stdout.write(json.dumps(message) + "\n")
        sys.stdout.flush()

    if step == "noisy":
        sys.stdout.write("starting up, not JSON\n")
        sys.stdout.flush()
    if step == "stubborn":
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        signal.signal(signal.SIGHUP, signal.SIG_IGN)
    if step == "deaf":
        send({"jsonrpc": "2.0", "method": "notifications/message", "params": {"data": "first"}})
        time.sleep(1.5)
        send({"jsonrpc": "2.0", "method": "notifications/message", "params": {"data": "second"}})
        while True:
            time.sleep(1)
    for raw in sys.stdin:
        text = raw.rstrip("\n")
        log("received", text if len(text) < 300 else "<%d bytes>" % len(text))
        try:
            message = json.loads(text)
        except ValueError:
            continue
        method = message.get("method")
        mid = message.get("id")
        if method == "initialize":
            send({"jsonrpc": "2.0", "id": mid, "result": {
                "protocolVersion": "2024-11-05",
                "capabilities": {"tools": {"listChanged": True}},
                "serverInfo": {"name": "double", "version": "1"}}})
        elif method == "tools/list":
            send({"jsonrpc": "2.0", "id": mid, "result": {"tools": [{"name": "echo", "inputSchema": {"type": "object"}}]}})
        elif method == "ping":
            send({"jsonrpc": "2.0", "id": mid, "result": {}})
        elif method == "tools/call":
            name = message["params"]["name"]
            if name == "die":
                os._exit(3)
            elif name == "hold":
                pass
            elif name == "notify":
                send({"jsonrpc": "2.0", "method": "notifications/message", "params": {"level": "info", "data": "working"}})
                send({"jsonrpc": "2.0", "id": "srv-1", "method": "roots/list"})
                send({"jsonrpc": "2.0", "id": mid, "result": {"content": []}})
            else:
                send({"jsonrpc": "2.0", "id": mid, "result": {"content": [{"type": "text", "text": json.dumps(message["params"].get("arguments"))}]}})
        elif method is None and mid is not None:
            log("responses", json.dumps(mid))
    if step == "stubborn":
        while True:
            time.sleep(1)
else:
    raise SystemExit(99)
"##
        .replace("__DIR__", &dir.display().to_string());
        atomic_executable(path, script.as_bytes()).unwrap();
    }

    /// Client input for the proxy: chunks from a channel, EOF once it closes.
    struct ChannelReader {
        chunks: mpsc::Receiver<Vec<u8>>,
        pending: Vec<u8>,
        offset: usize,
    }

    impl Read for ChannelReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            while self.offset == self.pending.len() {
                match self.chunks.recv() {
                    Ok(chunk) => (self.pending, self.offset) = (chunk, 0),
                    Err(_) => return Ok(0),
                }
            }
            let size = buffer.len().min(self.pending.len() - self.offset);
            buffer[..size].copy_from_slice(&self.pending[self.offset..self.offset + size]);
            self.offset += size;
            Ok(size)
        }
    }

    /// Client output from the proxy, delivered a line at a time.
    struct ChannelWriter {
        lines: mpsc::Sender<Vec<u8>>,
        partial: Vec<u8>,
        /// Set once the client stops reading its end.
        broken: Arc<AtomicBool>,
    }

    impl Write for ChannelWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            if self.broken.load(Ordering::SeqCst) {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            self.partial.extend_from_slice(buffer);
            while let Some(end) = self.partial.iter().position(|byte| *byte == b'\n') {
                let line: Vec<u8> = self.partial.drain(..=end).collect();
                let _ = self.lines.send(line[..end].to_vec());
            }
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[derive(Clone, Default)]
    struct SharedBuffer(Arc<Mutex<Vec<u8>>>);

    impl SharedBuffer {
        fn text(&self) -> String {
            String::from_utf8_lossy(&locked(&self.0)).into_owned()
        }
    }

    impl Write for SharedBuffer {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            locked(&self.0).extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// The agent's side of the proxy.
    struct ProxyClient {
        input: Option<mpsc::Sender<Vec<u8>>>,
        lines: mpsc::Receiver<Vec<u8>>,
        stderr: SharedBuffer,
        broken: Arc<AtomicBool>,
        seen: std::cell::RefCell<Vec<Value>>,
        run: Option<thread::JoinHandle<Result<(), String>>>,
    }

    const CLIENT_PATIENCE: Duration = Duration::from_secs(30);

    impl ProxyClient {
        fn launch(run: impl FnOnce(McpIo) -> Result<(), String> + Send + 'static) -> Self {
            let (input, chunks) = mpsc::channel();
            let (sender, lines) = mpsc::channel();
            let stderr = SharedBuffer::default();
            let broken = Arc::new(AtomicBool::new(false));
            let io = McpIo {
                input: Box::new(BufReader::new(ChannelReader {
                    chunks,
                    pending: Vec::new(),
                    offset: 0,
                })),
                output: Box::new(ChannelWriter {
                    lines: sender,
                    partial: Vec::new(),
                    broken: Arc::clone(&broken),
                }),
                stderr: Box::new(stderr.clone()),
            };
            Self {
                input: Some(input),
                lines,
                stderr,
                broken,
                seen: Default::default(),
                run: Some(thread::spawn(move || run(io))),
            }
        }

        fn send_raw(&self, text: &str) {
            self.input
                .as_ref()
                .unwrap()
                .send(format!("{text}\n").into_bytes())
                .unwrap();
        }

        fn send(&self, message: Value) {
            self.send_raw(&message.to_string());
        }

        fn raw_within(&self, timeout: Duration) -> Option<String> {
            let line = self.lines.recv_timeout(timeout).ok()?;
            Some(String::from_utf8(line).unwrap())
        }

        fn raw(&self) -> String {
            self.raw_within(CLIENT_PATIENCE)
                .unwrap_or_else(|| panic!("no message from the proxy\n{}", self.stderr.text()))
        }

        fn recv(&self) -> Value {
            let raw = self.raw();
            let message: Value = serde_json::from_str(&raw)
                .unwrap_or_else(|error| panic!("stdout carried non-JSON ({error}): {raw}"));
            self.seen.borrow_mut().push(message.clone());
            message
        }

        /// The response to `id`, skipping notifications and server requests.
        fn recv_reply(&self, id: &Value) -> Value {
            loop {
                let message = self.recv();
                if message.get("method").is_none() && message.get("id") == Some(id) {
                    return message;
                }
            }
        }

        fn wait_for_method(&self, method: &str) {
            let has = |seen: &[Value]| seen.iter().any(|m| m["method"] == method);
            while !has(&self.seen.borrow()) {
                self.recv();
            }
        }

        fn request(&self, id: Value, method: &str, params: Value) -> Value {
            self.send(serde_json::json!({
                "jsonrpc": "2.0", "id": id, "method": method, "params": params,
            }));
            self.recv_reply(&id)
        }

        fn handshake(&self) -> Value {
            let reply = self.request(
                serde_json::json!(1),
                "initialize",
                serde_json::json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": "test-agent", "version": "9" },
                }),
            );
            self.send(
                serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            );
            reply
        }

        /// Ask for the tool list until it is answered with a result. Every
        /// attempt must get exactly one reply, an error while the driver is down.
        fn wait_until_serving(&self) -> Value {
            for attempt in 0..400 {
                let id = serde_json::json!(format!("probe-{attempt}"));
                let reply = self.request(id, "tools/list", serde_json::json!({}));
                if reply.get("result").is_some() {
                    return reply;
                }
                assert_eq!(reply["error"]["code"], -32000, "{reply}");
                thread::sleep(Duration::from_millis(50));
            }
            panic!("the proxy never recovered\n{}", self.stderr.text());
        }

        fn close(&mut self) {
            self.input = None;
        }

        fn finish(&mut self) -> Result<(), String> {
            self.close();
            let run = self.run.take().unwrap();
            let deadline = Instant::now() + CLIENT_PATIENCE;
            while !run.is_finished() {
                assert!(
                    Instant::now() < deadline,
                    "the proxy did not exit\n{}",
                    self.stderr.text()
                );
                thread::sleep(Duration::from_millis(10));
            }
            run.join().unwrap()
        }
    }

    struct McpWorld {
        dir: PathBuf,
        manager: CuaManager,
        _stop: StopDaemon,
        _temp: TemporaryDirectory,
    }

    impl McpWorld {
        fn new(plan: &[&str]) -> Self {
            let temp = TemporaryDirectory::new(&env::temp_dir(), "riwork-cua-test").unwrap();
            let dir = temp.path.clone();
            install_mcp_double(&dir.join("driver"), &dir);
            fs::write(dir.join("plan"), plan.join("\n")).unwrap();
            Self {
                manager: handshake_manager(dir.join("home"), dir.join("driver")),
                _stop: StopDaemon(dir.join("running")),
                dir,
                _temp: temp,
            }
        }

        /// The real launch path: `ensure_started`, signature check and all.
        fn start(&self, policy: McpPolicy) -> ProxyClient {
            let manager = self.manager.clone();
            ProxyClient::launch(move |io| manager.run_mcp_with(io, &policy))
        }

        fn read(&self, name: &str) -> String {
            fs::read_to_string(self.dir.join(name)).unwrap_or_default()
        }

        /// Wall-clock start time and pid of every `mcp` process the double ran.
        fn starts(&self) -> Vec<(f64, u32)> {
            self.read("starts")
                .lines()
                .map(|line| {
                    let mut words = line.split(' ');
                    let time = words.next().unwrap().parse().unwrap();
                    let pid = words.nth(1).unwrap().parse().unwrap();
                    (time, pid)
                })
                .collect()
        }

        /// The messages the double received, as JSON, in order.
        fn received(&self) -> Vec<Value> {
            self.read("received")
                .lines()
                .map(|line| serde_json::from_str(line).unwrap_or(Value::String(line.to_owned())))
                .collect()
        }

        fn received_methods(&self) -> Vec<String> {
            self.received()
                .iter()
                .map(|message| message["method"].as_str().unwrap_or("?").to_owned())
                .collect()
        }

        fn wait_for(&self, what: &str, done: impl Fn() -> bool) {
            let deadline = Instant::now() + CLIENT_PATIENCE;
            while !done() {
                assert!(Instant::now() < deadline, "timed out waiting for {what}");
                thread::sleep(Duration::from_millis(20));
            }
        }
    }

    fn quick_policy() -> McpPolicy {
        McpPolicy {
            backoff: vec![Duration::from_millis(50), Duration::from_millis(100)],
            stable_after: Duration::from_secs(30),
            replay_timeout: Duration::from_secs(20),
            line_limit: MCP_LINE_LIMIT,
        }
    }

    fn tool_call(name: &str, arguments: Value) -> Value {
        serde_json::json!({ "name": name, "arguments": arguments })
    }

    fn pid_gone(pid: u32) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while process_alive(pid) {
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(20));
        }
        true
    }

    #[test]
    #[ignore = "slow: real Python MCP driver double"]
    fn the_proxy_forwards_requests_notifications_and_server_messages_verbatim() {
        let world = McpWorld::new(&["noisy"]);
        let mut client = world.start(quick_policy());
        let reply = client.handshake();
        assert_eq!(reply["id"], 1);
        assert_eq!(reply["result"]["serverInfo"]["name"], "double");

        // Spacing and key order reach the driver untouched, and an id keeps its
        // type: 5 and "5" are different requests.
        let odd = r#"{"method":"ping",  "id": 5 ,"jsonrpc":"2.0"}"#;
        client.send_raw(odd);
        client.send_raw(r#"{"jsonrpc":"2.0","id":"5","method":"ping"}"#);
        let (number, string) = (client.recv(), client.recv());
        assert_eq!(number["id"], serde_json::json!(5));
        assert_eq!(string["id"], serde_json::json!("5"));

        // Driver-initiated messages arrive in the order it wrote them, and the
        // client's answer to its request goes back to it.
        client.send(serde_json::json!({
            "jsonrpc": "2.0", "id": 8, "method": "tools/call",
            "params": tool_call("notify", serde_json::json!({})),
        }));
        let (notification, request, response) = (client.recv(), client.recv(), client.recv());
        assert_eq!(notification["method"], "notifications/message");
        assert!(notification.get("id").is_none());
        assert_eq!(request["method"], "roots/list");
        assert_eq!(request["id"], "srv-1");
        assert_eq!(response["id"], 8);
        client.send_raw(r#"{"jsonrpc":"2.0","id":"srv-1","result":{"roots":[]}}"#);
        world.wait_for("the client's answer", || {
            world.read("responses").contains("\"srv-1\"")
        });
        assert!(world.read("received").lines().any(|line| line == odd));

        // Nothing but MCP reached stdout; the driver's chatter went to stderr.
        let stderr = client.stderr.text();
        assert!(stderr.contains("driver says hello"), "{stderr}");
        assert!(stderr.contains("starting up, not JSON"), "{stderr}");
        assert!(
            !client
                .seen
                .borrow()
                .iter()
                .any(|m| m.to_string().contains("starting up")),
        );

        let pid = world.starts()[0].1;
        assert_eq!(client.finish(), Ok(()));
        assert!(pid_gone(pid));
        assert_eq!(world.starts().len(), 1, "nothing was restarted");
    }

    #[test]
    #[ignore = "slow: real Python MCP driver double and restart backoff"]
    fn a_driver_that_ends_mid_session_is_replaced_and_the_handshake_replayed() {
        let world = McpWorld::new(&[]);
        let mut client = world.start(quick_policy());
        let first = client.handshake();
        let ids = [
            serde_json::json!(7),
            serde_json::json!("7"),
            serde_json::json!("abc"),
            serde_json::json!(9_007_199_254_740_993_u64),
            serde_json::json!(-3),
        ];
        for id in &ids[..4] {
            client.send(serde_json::json!({
                "jsonrpc": "2.0", "id": id, "method": "tools/call",
                "params": tool_call("hold", serde_json::json!({})),
            }));
        }
        client.send(serde_json::json!({
            "jsonrpc": "2.0", "id": -3, "method": "tools/call",
            "params": tool_call("die", serde_json::json!({})),
        }));

        // Every request the driver never answered gets an error, in the order
        // they were sent, each keeping its id exactly.
        for id in &ids {
            let raw = client.raw();
            let reply: Value = serde_json::from_str(&raw).unwrap();
            assert_eq!(&reply["id"], id, "{raw}");
            assert_eq!(reply["error"]["code"], -32000, "{raw}");
            assert_eq!(
                reply["error"]["message"],
                "Cua Driver restarted; retry the request"
            );
            assert!(raw.contains(&format!("\"id\":{id}")), "{raw}");
            client.seen.borrow_mut().push(reply);
        }

        // Later requests work on the new driver, and the client never saw the
        // replayed handshake.
        let listed = client.wait_until_serving();
        assert_eq!(listed["result"]["tools"][0]["name"], "echo");
        client.wait_for_method("notifications/tools/list_changed");
        assert!(client.seen.borrow().iter().all(|m| {
            !m["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("riwork-cua-proxy"))
        }));
        // Only the answer the client itself asked for was ever delivered.
        assert_eq!(
            client.seen.borrow().iter().filter(|m| m["id"] == 1).count(),
            1,
            "{first}"
        );

        assert_eq!(world.starts().len(), 2);
        let received = world.received();
        let methods = world.received_methods();
        assert_eq!(
            methods[..9],
            [
                "initialize",
                "notifications/initialized",
                "tools/call",
                "tools/call",
                "tools/call",
                "tools/call",
                "tools/call",
                "initialize",
                "notifications/initialized",
            ]
        );
        assert!(methods[9..].iter().all(|method| method == "tools/list"));
        let (original, replay) = (&received[0], &received[7]);
        assert_eq!(original["id"], 1);
        assert!(
            replay["id"]
                .as_str()
                .unwrap()
                .starts_with("riwork-cua-proxy-"),
            "{replay}"
        );
        assert_eq!(replay["params"], original["params"]);
        assert_eq!(replay["params"]["clientInfo"]["name"], "test-agent");

        let stderr = client.stderr.text();
        assert!(stderr.contains("restarting"), "{stderr}");
        let log = fs::read_to_string(world.dir.join("home/cua/driver.log")).unwrap();
        assert!(
            log.contains("MCP proxy: Cua Driver MCP closed its output"),
            "{log}"
        );
        assert_eq!(client.finish(), Ok(()));
    }

    /// A proxy whose driver has just died and which cannot start another until
    /// `allow` is set. `calls` holds the time of each launch attempt.
    struct DownDriver {
        client: ProxyClient,
        allow: Arc<AtomicBool>,
        calls: Arc<Mutex<Vec<Instant>>>,
        _world: McpWorld,
    }

    fn driver_that_cannot_restart() -> DownDriver {
        let world = McpWorld::new(&[]);
        let driver = world.dir.join("driver");
        let allow = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let client = {
            let (allow, calls) = (Arc::clone(&allow), Arc::clone(&calls));
            ProxyClient::launch(move |io| {
                let prepare = move || {
                    let mut calls = locked(&calls);
                    calls.push(Instant::now());
                    if calls.len() == 1 || allow.load(Ordering::SeqCst) {
                        Ok(driver.clone())
                    } else {
                        Err("driver unavailable".to_owned())
                    }
                };
                let policy = McpPolicy {
                    backoff: [100, 200].map(Duration::from_millis).to_vec(),
                    ..quick_policy()
                };
                run_mcp_proxy(&prepare, io, &policy, Box::new(|_| {}))
            })
        };
        client.handshake();
        let died = client.request(
            serde_json::json!(2),
            "tools/call",
            tool_call("die", serde_json::json!({})),
        );
        assert_eq!(died["error"]["code"], -32000);
        DownDriver {
            client,
            allow,
            calls,
            _world: world,
        }
    }

    #[test]
    #[ignore = "slow: real Python MCP driver double and restart backoff"]
    fn requests_are_answered_at_once_while_the_driver_cannot_be_started() {
        let mut down = driver_that_cannot_restart();
        for attempt in 0..3 {
            let started = Instant::now();
            let reply = down.client.request(
                serde_json::json!(format!("waiting-{attempt}")),
                "tools/list",
                serde_json::json!({}),
            );
            assert_eq!(reply["error"]["code"], -32000, "{reply}");
            assert_eq!(
                reply["error"]["message"],
                "Cua Driver restarted; retry the request"
            );
            assert!(started.elapsed() < Duration::from_secs(5));
        }
        // A notification sent while down is dropped, not queued for later.
        down.client.send(serde_json::json!({
            "jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 1},
        }));
        thread::sleep(Duration::from_millis(700));
        let calls = locked(&down.calls).clone();
        // Backing off, not spinning: one launch attempt per pause.
        assert!((3..=8).contains(&calls.len()), "{} attempts", calls.len());
        for pair in calls[1..].windows(2).skip(1) {
            assert!(pair[1] - pair[0] >= Duration::from_millis(190));
        }
        down.allow.store(true, Ordering::SeqCst);
        down.client.wait_until_serving();
        let world = &down._world;
        assert!(!world.read("received").contains("notifications/cancelled"));
        assert_eq!(down.client.finish(), Ok(()));
    }

    #[test]
    #[ignore = "slow: real Python MCP driver double"]
    fn client_eof_kills_a_driver_that_ignores_it() {
        let world = McpWorld::new(&["stubborn"]);
        let mut client = world.start(quick_policy());
        client.handshake();
        let pid = world.starts()[0].1;
        let closed = Instant::now();
        assert_eq!(client.finish(), Ok(()));
        assert!(pid_gone(pid), "the driver outlived the proxy");
        assert!(closed.elapsed() < Duration::from_secs(10));
        assert_eq!(world.starts().len(), 1);
    }

    #[test]
    #[ignore = "slow: real Python MCP driver double"]
    fn a_client_line_over_the_cap_is_refused_without_reaching_the_driver() {
        let world = McpWorld::new(&[]);
        let mut client = world.start(McpPolicy {
            line_limit: 2048,
            ..quick_policy()
        });
        client.handshake();
        client.send(serde_json::json!({
            "jsonrpc": "2.0", "id": 11, "method": "tools/call",
            "params": tool_call("echo", serde_json::json!({ "pad": "a".repeat(5000) })),
        }));
        let refused = client.recv();
        assert!(refused["id"].is_null(), "{refused}");
        assert_eq!(refused["error"]["code"], -32600);
        assert!(
            refused["error"]["message"]
                .as_str()
                .unwrap()
                .contains("2048")
        );
        // The stream is still in step: the next message is read normally.
        let pong = client.request(serde_json::json!(12), "ping", serde_json::json!({}));
        assert!(pong.get("result").is_some());
        assert!(!world.read("received").contains("bytes>"));
        assert!(client.stderr.text().contains("longer than 2048"));
        assert_eq!(world.starts().len(), 1);
        assert_eq!(client.finish(), Ok(()));
    }

    #[test]
    fn a_driver_that_cannot_start_at_first_fails_with_its_reason() {
        let temp = TemporaryDirectory::new(&env::temp_dir(), "riwork-cua-test").unwrap();
        let manager = handshake_manager(temp.path.join("home"), temp.path.join("missing-driver"));
        let mut client = ProxyClient::launch(move |io| manager.run_mcp_with(io, &quick_policy()));
        let error = client.finish().unwrap_err();
        assert!(error.contains("not an executable"), "{error}");
    }

    #[test]
    #[ignore = "slow: real Python MCP driver double and wall-clock waits"]
    fn a_write_stuck_on_a_driver_that_stopped_reading_cannot_block_shutdown() {
        let world = McpWorld::new(&["deaf"]);
        let mut client = world.start(quick_policy());
        assert_eq!(client.recv()["params"]["data"], "first");
        let pid = world.starts()[0].1;
        // The proxy's client reader blocks in a write the driver never drains.
        client.send_raw(&format!(
            r#"{{"jsonrpc":"2.0","method":"notifications/x","params":{{"pad":"{}"}}}}"#,
            "p".repeat(1 << 20)
        ));
        thread::sleep(Duration::from_millis(300));
        // The client stops reading; the driver's next message finds that out.
        client.broken.store(true, Ordering::SeqCst);
        let error = client.finish().unwrap_err();
        assert!(error.contains("stopped reading"), "{error}");
        assert!(pid_gone(pid), "the deaf driver outlived the proxy");
    }

    fn frames(data: &[u8], capacity: usize, limit: usize) -> Vec<String> {
        let mut reader = BufReader::with_capacity(capacity, io::Cursor::new(data.to_vec()));
        let mut seen = Vec::new();
        loop {
            match read_frame(&mut reader, limit).unwrap() {
                Frame::Line(line) => seen.push(format!("line:{}", String::from_utf8_lossy(&line))),
                Frame::TooLong => seen.push("too-long".to_owned()),
                Frame::Eof => return seen,
            }
        }
    }

    #[test]
    fn frames_split_on_newlines_and_oversized_ones_are_dropped_unbuffered() {
        let mut data = b"one\n\ntwo words\n".to_vec();
        data.extend(vec![b'x'; 50]);
        data.extend(b"\nafter\nlast");
        // A four-byte buffer makes every line arrive in pieces.
        for capacity in [4, 7, 8192] {
            assert_eq!(
                frames(&data, capacity, 10),
                [
                    "line:one",
                    "line:",
                    "line:two words",
                    "too-long",
                    "line:after",
                    "line:last"
                ]
            );
        }
        // Over the cap at end of input is reported once, then EOF.
        assert_eq!(frames(&[b'y'; 30], 4, 10), ["too-long"]);
        assert_eq!(frames(b"exactly10!\n", 4, 10), ["line:exactly10!"]);
        assert_eq!(frames(b"", 4, 10), Vec::<String>::new());
    }
}
