//! Cooperative GUI reloads. Desktop windows are replaced only after a new GUI
//! confirms restoration. Terminal and harness processes are never signalled; the
//! one process a GUI may stop is the replacement it launched itself and gave up
//! on (see `stop_replacement`), and that replacement stops itself if it is
//! wedged and nobody is left to do it (see `RestoreWatchdog`).

use crate::layouts::ProjectLayout;
use fs2::FileExt;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const MAX_BYTES: u64 = 32 * 1024 * 1024;
const REQUEST_LIFETIME: u64 = 300;
/// Reload tickets, requests and responses are dead well before this; anything
/// older is a leftover from a crashed GUI or CLI.
const LEFTOVER_AGE: Duration = Duration::from_secs(60 * 60);
/// A registration nobody can parse is only removed once no live GUI could still
/// be refreshing it (running GUIs rewrite theirs at least every HEARTBEAT).
const UNREADABLE_AGE: Duration = Duration::from_secs(10 * 60);
const HEARTBEAT: Duration = Duration::from_secs(30);
/// Reload diagnostics are what someone reads after a failed reload, so they
/// outlive the other leftovers.
const DIAGNOSTIC_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const MAX_DIAGNOSTIC_BYTES: u64 = 64 * 1024;
/// How long a GUI waits for the replacement it launched to confirm.
const REPLACEMENT_TIMEOUT: Duration = Duration::from_secs(25);
/// Between asking a replacement it gave up on to terminate and killing it.
const STOP_GRACE: Duration = Duration::from_secs(3);
/// How long a killed replacement may take to disappear before it is reported
/// as unstoppable (a process stuck in the kernel ignores even SIGKILL).
const KILL_WAIT: Duration = Duration::from_secs(1);
/// How long `riwork reload` and `riwork update` wait for every GUI to answer.
/// A GUI answers at most REPLACEMENT_TIMEOUT + STOP_GRACE + KILL_WAIT (plus its
/// half-second poll) after launching, so the requester outlasts it.
pub const RELOAD_WAIT: Duration = Duration::from_secs(35);
/// A replacement stops itself when it has not confirmed restoration by then. It
/// is longer than the launching GUI's patience on purpose: a replacement that
/// could still be confirmed must never be stopped by its own watchdog.
pub const RESTORE_WATCHDOG_DEADLINE: Duration = Duration::from_secs(40);
/// Exit status of a replacement stopped by its own watchdog (EX_TEMPFAIL).
pub const RESTORE_WATCHDOG_EXIT_CODE: i32 = 75;
/// A confirmation in flight when the deadline passes gets this long to finish.
const CONFIRM_GRACE: Duration = Duration::from_secs(10);
/// How often the watchdog copies the latest breadcrumb to disk.
const MIRROR_INTERVAL: Duration = Duration::from_millis(250);
/// Diagnostics and unregistering are best effort; exiting is not.
const WATCHDOG_CLEANUP_LIMIT: Duration = Duration::from_secs(3);
const _: () = assert!(
    RESTORE_WATCHDOG_DEADLINE.as_secs()
        > REPLACEMENT_TIMEOUT.as_secs() + STOP_GRACE.as_secs() + KILL_WAIT.as_secs()
);
const _: () = assert!(
    RELOAD_WAIT.as_secs()
        > REPLACEMENT_TIMEOUT.as_secs() + STOP_GRACE.as_secs() + KILL_WAIT.as_secs()
);

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowGeometry {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowMode {
    #[default]
    Windowed,
    Maximized,
    Fullscreen,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RuntimeWindow {
    pub project_id: Option<String>,
    pub path: PathBuf,
    pub bounds: Option<WindowGeometry>,
    #[serde(default)]
    pub mode: WindowMode,
    pub layout: Option<ProjectLayout>,
    #[serde(default)]
    pub focus_mode: bool,
    #[serde(default)]
    pub focus_centered: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeInstance {
    pub id: String,
    pub pid: u32,
    pub uid: u32,
    pub started_token: String,
    pub executable: PathBuf,
    pub state_home: PathBuf,
    pub windows: Vec<RuntimeWindow>,
    pub updated_at: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReloadRequest {
    pub request_id: String,
    pub instance_id: String,
    pub ticket_id: String,
    pub replacement_executable: PathBuf,
    pub created_at: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RestoreSnapshot {
    pub ticket_id: String,
    pub request_id: String,
    pub old_instance_id: String,
    pub replacement_executable: PathBuf,
    pub state_home: PathBuf,
    pub windows: Vec<RuntimeWindow>,
    pub created_at: u64,
}
#[derive(Clone, Debug)]
pub struct ReloadLaunch {
    pub pid: u32,
    request: ReloadRequest,
    started: Instant,
    exited: Arc<AtomicBool>,
    /// Who the launched process is, taken right after it was spawned. Without
    /// it (the platform would not say) a replacement is never signalled.
    replacement: Option<ReplacementProcess>,
    /// Between SIGTERM and SIGKILL when the replacement has to be stopped.
    stop_grace: Duration,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReloadState {
    Requested,
    Reloaded,
    Failed,
    TimedOut,
    Busy,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReloadInstanceResult {
    pub instance_id: String,
    pub pid: u32,
    pub state_home: PathBuf,
    pub window_count: usize,
    pub state: ReloadState,
    pub new_pid: Option<u32>,
    pub message: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReloadReport {
    pub request_id: String,
    pub replacement_executable: PathBuf,
    pub requested: usize,
    pub reloaded: usize,
    pub pending: usize,
    pub failed: usize,
    /// Registrations of GUIs that are running but could not be understood,
    /// typically because they were built with an incompatible layout format.
    /// They are not part of `instances` and were not asked to reload.
    #[serde(default)]
    pub unreadable_registrations: usize,
    pub instances: Vec<ReloadInstanceResult>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Response {
    request_id: String,
    instance_id: String,
    state: ReloadState,
    new_instance: Option<RuntimeInstance>,
    message: String,
}
/// The fields every registration format has carried, enough to tell whether an
/// unparsable registration still belongs to a running GUI.
#[derive(Deserialize)]
struct RegistrationIdentity {
    pid: u32,
    uid: u32,
    started_token: String,
}
#[derive(Debug, Serialize, Deserialize)]
struct InstalledBuild {
    executable: PathBuf,
    recorded_at: u64,
}
#[derive(Clone, Debug)]
pub struct RuntimeManager {
    directory: PathBuf,
}
#[derive(Debug)]
pub struct RuntimeRegistration {
    manager: RuntimeManager,
    instance: RuntimeInstance,
    written_at: Instant,
}

impl RuntimeManager {
    pub fn open_default() -> Result<Self, String> {
        if let Some(directory) = env::var_os("RIWORK_RUNTIME_DIR") {
            return Self::at(PathBuf::from(directory));
        }
        let home = PathBuf::from(
            env::var_os("HOME").ok_or("HOME is unset; cannot locate RiWork runtime registry")?,
        );
        #[cfg(target_os = "macos")]
        let directory = home.join("Library/Caches/riwork/runtime");
        #[cfg(not(target_os = "macos"))]
        let directory = home.join(".cache/riwork/runtime");
        Self::at(directory)
    }
    pub fn at(directory: PathBuf) -> Result<Self, String> {
        private_directory(&directory)?;
        for name in [
            "instances",
            "requests",
            "restores",
            "responses",
            "diagnostics",
        ] {
            private_directory(&directory.join(name))?;
        }
        let directory = directory
            .canonicalize()
            .map_err(|e| format!("Cannot resolve runtime directory: {e}"))?;
        Ok(Self { directory })
    }
    /// Remember the last successful installation across CLI build profiles.
    pub fn record_installed_build(&self, executable: &Path) -> Result<(), String> {
        let installed = InstalledBuild {
            executable: executable_path(executable)?,
            recorded_at: now(),
        };
        let _lock = self.lock()?;
        // This one outlives every GUI, so it is worth surviving a power loss.
        write_json_synced(
            &self.directory.join("installed-build.json"),
            &installed,
            true,
        )
    }

    /// A removed or no longer executable installation allows normal fallback.
    pub fn installed_executable(&self) -> Result<Option<PathBuf>, String> {
        let _lock = self.lock()?;
        let Some(installed) =
            read_optional::<InstalledBuild>(&self.directory.join("installed-build.json"))?
        else {
            return Ok(None);
        };
        Ok(executable_path(&installed.executable).ok())
    }
    pub fn register(&self, state_home: PathBuf) -> Result<RuntimeRegistration, String> {
        fs::create_dir_all(&state_home).map_err(|e| format!("Cannot create RiWork state: {e}"))?;
        let state_home = state_home
            .canonicalize()
            .map_err(|e| format!("Cannot resolve RiWork state: {e}"))?;
        let identity =
            process_identity(std::process::id())?.ok_or("Cannot inspect the RiWork GUI process")?;
        let instance = RuntimeInstance {
            id: Uuid::new_v4().to_string(),
            pid: identity.pid,
            uid: identity.uid,
            started_token: identity.started_token,
            executable: identity
                .executable
                .ok_or("Cannot locate the RiWork GUI executable")?,
            state_home,
            windows: vec![],
            updated_at: now(),
        };
        let _lock = self.lock()?;
        self.prune_leftovers();
        write_json(&self.instance_path(&instance.id)?, &instance)?;
        Ok(RuntimeRegistration {
            manager: self.clone(),
            instance,
            written_at: Instant::now(),
        })
    }
    pub fn instances(&self) -> Result<Vec<RuntimeInstance>, String> {
        let _lock = self.lock()?;
        let (live, unreadable) = self.live_instances()?;
        warn_unreadable(unreadable);
        Ok(live)
    }
    /// Running GUIs, plus how many registrations could not be understood. A
    /// GUI built with an incompatible format is running but cannot be listed;
    /// callers must say so instead of reporting that nothing is open.
    fn live_instances(&self) -> Result<(Vec<RuntimeInstance>, usize), String> {
        let mut live = vec![];
        let mut unreadable = 0;
        for entry in fs::read_dir(self.directory.join("instances")).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let instance = match read_json::<RuntimeInstance>(&path) {
                Ok(instance) if self.instance_path(&instance.id).is_ok_and(|p| p == path) => {
                    instance
                }
                _ => {
                    match read_json::<RegistrationIdentity>(&path) {
                        Ok(identity) => {
                            if process_alive(identity.pid, identity.uid, &identity.started_token)? {
                                unreadable += 1;
                            } else {
                                let _ = fs::remove_file(&path);
                            }
                        }
                        Err(_) if older_than(&path, UNREADABLE_AGE) => {
                            let _ = fs::remove_file(&path);
                        }
                        Err(_) => unreadable += 1,
                    }
                    continue;
                }
            };
            if process_matches(&instance)? {
                live.push(instance);
            } else {
                fs::remove_file(&path)
                    .map_err(|e| format!("Cannot remove stale GUI registration: {e}"))?;
                let _ = fs::remove_file(self.request_path(&instance.id)?);
            }
        }
        live.sort_by_key(|instance| instance.pid);
        Ok((live, unreadable))
    }
    /// Files a crashed GUI or CLI never cleaned up. Callers hold the lock.
    fn prune_leftovers(&self) {
        for name in [
            "requests",
            "restores",
            "responses",
            "instances",
            "diagnostics",
        ] {
            let Ok(entries) = fs::read_dir(self.directory.join(name)) else {
                continue;
            };
            let age = if name == "diagnostics" {
                DIAGNOSTIC_AGE
            } else {
                LEFTOVER_AGE
            };
            for entry in entries.flatten() {
                let path = entry.path();
                // Registrations are judged by their process, not their age.
                if name == "instances" && path.extension().is_some_and(|ext| ext == "json") {
                    continue;
                }
                if older_than(&path, age) {
                    let _ = fs::remove_file(path);
                }
            }
        }
    }
    pub fn reload_all(&self, replacement: &Path) -> Result<ReloadReport, String> {
        let replacement = executable_path(replacement)?;
        let _lock = self.lock()?;
        self.prune_leftovers();
        let (instances, unreadable) = self.live_instances()?;
        warn_unreadable(unreadable);
        let request_id = Uuid::new_v4().to_string();
        let mut report = ReloadReport {
            request_id: request_id.clone(),
            replacement_executable: replacement.clone(),
            requested: 0,
            reloaded: 0,
            pending: 0,
            failed: 0,
            unreadable_registrations: unreadable,
            instances: vec![],
        };
        for instance in instances {
            let existing = read_optional::<ReloadRequest>(&self.request_path(&instance.id)?)?;
            let busy = existing
                .as_ref()
                .is_some_and(|request| now().saturating_sub(request.created_at) < REQUEST_LIFETIME)
                && existing.as_ref().is_some_and(|request| {
                    !self
                        .response_path(&request.instance_id, &request.request_id)
                        .is_ok_and(|path| path.exists())
                });
            let (state, message) = if busy {
                (
                    ReloadState::Busy,
                    "A reload is already in progress for this GUI.".to_owned(),
                )
            } else {
                let request = ReloadRequest {
                    request_id: request_id.clone(),
                    instance_id: instance.id.clone(),
                    ticket_id: Uuid::new_v4().to_string(),
                    replacement_executable: replacement.clone(),
                    created_at: now(),
                };
                write_json(&self.request_path(&instance.id)?, &request)?;
                report.requested += 1;
                (
                    ReloadState::Requested,
                    "Waiting for the GUI to save and restore its windows.".to_owned(),
                )
            };
            report.instances.push(ReloadInstanceResult {
                instance_id: instance.id,
                pid: instance.pid,
                state_home: instance.state_home,
                window_count: instance.windows.len(),
                state,
                new_pid: None,
                message,
            });
        }
        refresh_counts(&mut report);
        Ok(report)
    }
    pub fn wait_for_reload(
        &self,
        mut report: ReloadReport,
        timeout: Duration,
    ) -> Result<ReloadReport, String> {
        let deadline = Instant::now() + timeout;
        loop {
            for instance in &mut report.instances {
                if instance.state != ReloadState::Requested {
                    continue;
                }
                if let Some(response) = read_optional::<Response>(
                    &self.response_path(&instance.instance_id, &report.request_id)?,
                )? {
                    if response.request_id != report.request_id
                        || response.instance_id != instance.instance_id
                    {
                        continue;
                    }
                    if response.state == ReloadState::Reloaded
                        && response
                            .new_instance
                            .as_ref()
                            .is_none_or(|new| !process_matches(new).unwrap_or(false))
                    {
                        continue;
                    }
                    instance.state = response.state;
                    instance.new_pid = response.new_instance.map(|new| new.pid);
                    instance.message = response.message;
                }
            }
            refresh_counts(&mut report);
            if report.pending == 0 {
                return Ok(report);
            }
            if Instant::now() >= deadline {
                for instance in &mut report.instances {
                    if instance.state == ReloadState::Requested {
                        instance.state = ReloadState::TimedOut;
                        instance.message="GUI has not confirmed restoration; its existing windows were left running.".to_owned();
                    }
                }
                refresh_counts(&mut report);
                return Ok(report);
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
    pub fn restore_from_env(&self) -> Result<Option<RestoreSnapshot>, String> {
        match env::var("RIWORK_RESTORE_TICKET") {
            Ok(ticket) => self.restore_ticket(&ticket).map(Some),
            Err(env::VarError::NotPresent) => Ok(None),
            Err(_) => Err("Invalid RiWork restore ticket".to_owned()),
        }
    }
    fn restore_ticket(&self, ticket: &str) -> Result<RestoreSnapshot, String> {
        let snapshot: RestoreSnapshot = read_json(&self.restore_path(ticket)?)?;
        if snapshot.ticket_id != ticket
            || now().saturating_sub(snapshot.created_at) > REQUEST_LIFETIME
        {
            return Err("RiWork restore ticket is invalid or expired".to_owned());
        }
        valid_id(&snapshot.request_id)?;
        valid_id(&snapshot.old_instance_id)?;
        let executable = env::current_exe().map_err(|e| e.to_string())?;
        if executable_path(&executable)? != snapshot.replacement_executable {
            return Err("Restore ticket belongs to a different RiWork executable".to_owned());
        }
        validate_windows(&snapshot.windows)?;
        Ok(snapshot)
    }
    pub fn mark_restore_ready(
        &self,
        snapshot: &RestoreSnapshot,
        registration: &RuntimeRegistration,
    ) -> Result<(), String> {
        let stored = self.restore_ticket(&snapshot.ticket_id)?;
        let current = read_json::<RuntimeInstance>(
            &registration
                .manager
                .instance_path(&registration.instance.id)?,
        )?;
        if self.directory != registration.manager.directory
            || stored.request_id != snapshot.request_id
            || current.state_home != stored.state_home
            || current.executable != stored.replacement_executable
            || current.windows.len() != stored.windows.len()
            || !process_matches(&current)?
        {
            return Err("Restored GUI does not match its reload ticket".to_owned());
        }
        let response = Response {
            request_id: stored.request_id.clone(),
            instance_id: stored.old_instance_id.clone(),
            state: ReloadState::Reloaded,
            new_instance: Some(current),
            message: "Replacement GUI restored its windows; running shells were preserved."
                .to_owned(),
        };
        let response_path = self.response_path(&stored.old_instance_id, &stored.request_id)?;
        let _lock = self.lock()?;
        // The previous GUI may have given up between the checks above and this
        // lock. Confirming then would leave both GUIs open with the same windows.
        let abandoned = read_optional::<Response>(&response_path)?
            .is_some_and(|existing| existing.state == ReloadState::Failed);
        if abandoned || !self.restore_path(&stored.ticket_id)?.exists() {
            return Err("The previous RiWork gave up on this reload; closing this copy".to_owned());
        }
        write_json(&response_path, &response)?;
        fs::remove_file(self.restore_path(&stored.ticket_id)?)
            .map_err(|e| format!("Cannot consume restore ticket: {e}"))?;
        Ok(())
    }
    /// Write (replace) the diagnostic for one reload request. It takes no
    /// registry lock: it is a single atomic rename that only the replacement
    /// and the GUI that launched it ever touch, and a hung filesystem must not
    /// be able to stall anything else behind it.
    fn write_diagnostic(&self, diagnostic: &ReloadDiagnostic) -> Result<PathBuf, String> {
        let path = self.diagnostic_path(&diagnostic.request_id, &diagnostic.ticket_id)?;
        write_private_file(&path, diagnostic.render().as_bytes(), false)?;
        Ok(path)
    }
    /// The diagnostic the replacement with `pid` left for this launch, if any.
    /// A file from an earlier replacement of the same launch is not returned.
    fn read_diagnostic(
        &self,
        request: &str,
        ticket: &str,
        pid: u32,
    ) -> Option<(PathBuf, ReloadDiagnostic)> {
        let path = self.diagnostic_path(request, ticket).ok()?;
        let text = String::from_utf8(read_private_file(&path, MAX_DIAGNOSTIC_BYTES).ok()?).ok()?;
        let diagnostic = ReloadDiagnostic::parse(&text)?;
        (diagnostic.request_id == request
            && diagnostic.ticket_id == ticket
            && diagnostic.pid == pid)
            .then_some((path, diagnostic))
    }
    fn remove_diagnostic(&self, request: &str, ticket: &str) {
        if let Ok(path) = self.diagnostic_path(request, ticket) {
            let _ = fs::remove_file(path);
        }
    }
    /// Take an instance out of the registry from any thread. Unlinking is
    /// atomic, so a registry that stays locked does not hold this up; the
    /// lock is only taken (briefly) to stay in step with writers. A
    /// registration rewritten by a still-running main thread afterwards is
    /// harmless: its process is gone soon, and `live_instances` prunes it.
    fn remove_registration(&self, instance: &str) {
        let _lock = self.lock().ok();
        if let Ok(path) = self.instance_path(instance) {
            let _ = fs::remove_file(path);
        }
        if let Ok(path) = self.request_path(instance) {
            let _ = fs::remove_file(path);
        }
    }
    fn instance_path(&self, id: &str) -> Result<PathBuf, String> {
        valid_id(id)?;
        Ok(self.directory.join("instances").join(format!("{id}.json")))
    }
    fn request_path(&self, id: &str) -> Result<PathBuf, String> {
        valid_id(id)?;
        Ok(self.directory.join("requests").join(format!("{id}.json")))
    }
    fn restore_path(&self, id: &str) -> Result<PathBuf, String> {
        valid_id(id)?;
        Ok(self.directory.join("restores").join(format!("{id}.json")))
    }
    /// One file per launch: every app reloaded by one `riwork reload` shares the
    /// request id, and only the ticket id tells their replacements apart.
    fn diagnostic_path(&self, request: &str, ticket: &str) -> Result<PathBuf, String> {
        valid_id(request)?;
        valid_id(ticket)?;
        Ok(self
            .directory
            .join("diagnostics")
            .join(format!("reload-{request}-{ticket}.txt")))
    }
    fn response_path(&self, instance: &str, request: &str) -> Result<PathBuf, String> {
        valid_id(instance)?;
        valid_id(request)?;
        Ok(self
            .directory
            .join("responses")
            .join(format!("{instance}-{request}.json")))
    }
    fn lock_file(&self) -> Result<File, String> {
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.directory.join("registry.lock"))
            .map_err(|e| e.to_string())
    }
    /// One attempt, for callers that must never wait (the GUI's UI thread).
    fn try_lock(&self) -> Result<Option<File>, String> {
        let file = self.lock_file()?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Some(file)),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(format!("Cannot lock RiWork runtime registry: {e}")),
        }
    }
    fn lock(&self) -> Result<File, String> {
        let file = self.lock_file()?;
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(file),
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(20))
                }
                Err(e) => return Err(format!("Cannot lock RiWork runtime registry: {e}")),
            }
        }
    }
}
impl RuntimeRegistration {
    /// Blocking publish for startup and restore confirmation, where the file
    /// must reflect `windows` before the caller continues.
    pub fn publish_windows(&mut self, windows: Vec<RuntimeWindow>) -> Result<(), String> {
        self.publish(windows, true).map(|_| ())
    }
    /// Publish from a thread that must not stall (the GUI's UI thread). Nothing
    /// is written unless the windows changed, the file went missing, or the
    /// heartbeat is due, and a busy registry lock is retried on the next call
    /// instead of waited for. Returns whether the file is now current.
    pub fn try_publish_windows(&mut self, windows: Vec<RuntimeWindow>) -> Result<bool, String> {
        self.publish(windows, false)
    }
    fn publish(&mut self, windows: Vec<RuntimeWindow>, wait: bool) -> Result<bool, String> {
        let path = self.manager.instance_path(&self.instance.id)?;
        let changed = windows != self.instance.windows;
        if !changed && self.written_at.elapsed() < HEARTBEAT && path.exists() {
            return Ok(true);
        }
        if changed {
            validate_windows(&windows)?;
        }
        let _lock = if wait {
            self.manager.lock()?
        } else if let Some(lock) = self.manager.try_lock()? {
            lock
        } else {
            return Ok(false);
        };
        let previous = std::mem::replace(&mut self.instance.windows, windows);
        self.instance.updated_at = now();
        if let Err(error) = write_json(&path, &self.instance) {
            self.instance.windows = previous;
            return Err(error);
        }
        self.written_at = Instant::now();
        Ok(true)
    }
    /// Start guarding this replacement's restore (see `RestoreWatchdog`). Call
    /// it as soon as the registration exists, before any window is opened.
    pub fn start_restore_watchdog(&self, snapshot: &RestoreSnapshot) -> RestoreWatchdog {
        let progress = Arc::new(RestoreProgress::new());
        let _ = RESTORE_PROGRESS.set(progress.clone());
        RestoreWatchdog::spawn(
            WatchdogContext {
                manager: self.manager.clone(),
                instance_id: self.instance.id.clone(),
                request_id: snapshot.request_id.clone(),
                ticket_id: snapshot.ticket_id.clone(),
                pid: self.instance.pid,
                executable: self.instance.executable.clone(),
                progress,
                write: RuntimeManager::write_diagnostic,
            },
            WatchdogConfig {
                deadline: RESTORE_WATCHDOG_DEADLINE,
                confirm_grace: CONFIRM_GRACE,
                mirror_interval: MIRROR_INTERVAL,
            },
            exit_after_restore_timeout,
        )
    }
    pub fn pending_reload(&self) -> Result<Option<ReloadRequest>, String> {
        let Some(request) =
            read_optional::<ReloadRequest>(&self.manager.request_path(&self.instance.id)?)?
        else {
            return Ok(None);
        };
        if request.instance_id != self.instance.id
            || now().saturating_sub(request.created_at) > REQUEST_LIFETIME
        {
            return Ok(None);
        };
        valid_id(&request.request_id)?;
        valid_id(&request.ticket_id)?;
        if self
            .manager
            .response_path(&request.instance_id, &request.request_id)?
            .exists()
        {
            return Ok(None);
        };
        // Answer a request that can never launch now, rather than erroring on
        // every poll until the requester times out.
        if let Err(error) = executable_path(&request.replacement_executable) {
            self.fail_reload(&request, &error)?;
            return Ok(None);
        }
        Ok(Some(request))
    }
    pub fn launch_reload(
        &self,
        request: &ReloadRequest,
        windows: Vec<RuntimeWindow>,
    ) -> Result<ReloadLaunch, String> {
        if request.instance_id != self.instance.id
            || now().saturating_sub(request.created_at) > REQUEST_LIFETIME
        {
            return Err("Reload request belongs to a different or expired GUI instance".to_owned());
        }
        valid_id(&request.request_id)?;
        valid_id(&request.ticket_id)?;
        validate_windows(&windows)?;
        let executable = self.trusted_replacement(&request.replacement_executable)?;
        let snapshot = RestoreSnapshot {
            ticket_id: request.ticket_id.clone(),
            request_id: request.request_id.clone(),
            old_instance_id: self.instance.id.clone(),
            replacement_executable: executable.clone(),
            state_home: self.instance.state_home.clone(),
            windows,
            created_at: now(),
        };
        {
            let _lock = self.manager.lock()?;
            write_json(&self.manager.restore_path(&request.ticket_id)?, &snapshot)?;
        }
        let mut command = Command::new(executable);
        command
            .env("RIWORK_HOME", &snapshot.state_home)
            .env("RIWORK_RUNTIME_DIR", &self.manager.directory)
            .env("RIWORK_RESTORE_TICKET", &snapshot.ticket_id)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(first) = snapshot.windows.first() {
            if first.path.is_dir() {
                command.current_dir(&first.path);
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command.spawn().map_err(|e| {
            let _ = fs::remove_file(
                self.manager
                    .restore_path(&request.ticket_id)
                    .unwrap_or_default(),
            );
            format!("Cannot launch replacement RiWork GUI: {e}")
        })?;
        let pid = child.id();
        // Taken before anything else can happen to the child. It is not reaped
        // until the thread below runs, so its pid cannot have been reused yet.
        let replacement = ReplacementProcess::of(pid, cfg!(unix));
        let exited = Arc::new(AtomicBool::new(false));
        let child_exited = exited.clone();
        thread::spawn(move || {
            let _ = child.wait();
            child_exited.store(true, Ordering::Release);
        });
        Ok(ReloadLaunch {
            pid,
            request: request.clone(),
            started: Instant::now(),
            exited,
            replacement,
            stop_grace: STOP_GRACE,
        })
    }
    /// Request files live in a user-writable cache directory, so the path in
    /// one is not authority to run it. A GUI only relaunches itself: as the
    /// executable it is already running, its sibling app bundle (the layout the
    /// CLI falls back to), or the build `riwork update` last recorded.
    fn trusted_replacement(&self, requested: &Path) -> Result<PathBuf, String> {
        let requested = executable_path(requested)?;
        let mut allowed = vec![];
        if let Ok(current) = env::current_exe() {
            if let Some(parent) = current.parent() {
                allowed.push(parent.join("RiWork.app/Contents/MacOS/riwork"));
            }
            allowed.push(current);
        }
        allowed.extend(self.manager.installed_executable()?);
        if !allowed
            .iter()
            .filter_map(|path| path.canonicalize().ok())
            .any(|path| path == requested)
        {
            return Err(format!(
                "Refusing to launch {}: a reload may only use this app's own executable or the build recorded by `riwork update`. Run `riwork update` to install and record that build.",
                requested.display()
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = requested.metadata().map_err(|e| e.to_string())?;
            // An administrator-installed app is root-owned; anything else must
            // be ours, and nothing may be writable by everyone.
            if !(metadata.uid() == current_uid() || metadata.uid() == 0)
                || metadata.mode() & 0o002 != 0
            {
                return Err(
                    "Replacement RiWork executable must be owned by you or root and not world-writable"
                        .to_owned(),
                );
            }
        }
        Ok(requested)
    }
    /// A replacement that registered, restored every window, and is still the
    /// process this GUI launched.
    fn confirmed_by(&self, response: &Response, launch: Option<&ReloadLaunch>) -> bool {
        response.state == ReloadState::Reloaded
            && response.instance_id == self.instance.id
            && launch.is_none_or(|launch| response.request_id == launch.request.request_id)
            && response.new_instance.as_ref().is_some_and(|new| {
                launch.is_none_or(|launch| new.pid == launch.pid)
                    && process_matches(new).unwrap_or(false)
            })
    }
    pub fn reload_ready(&self, launch: &ReloadLaunch) -> Result<bool, String> {
        // An unreadable response is not a verdict; the timeouts below still apply.
        let response = read_optional::<Response>(
            &self
                .manager
                .response_path(&self.instance.id, &launch.request.request_id)?,
        )
        .unwrap_or(None);
        if let Some(response) = response {
            if response.state == ReloadState::Failed {
                return Err(response.message);
            }
            if self.confirmed_by(&response, Some(launch)) {
                return Ok(true);
            }
        }
        if !launch.exited.load(Ordering::Acquire) {
            if launch.started.elapsed() < REPLACEMENT_TIMEOUT {
                return Ok(false);
            }
            return self.give_up_on_hung_replacement(launch);
        }
        // Nothing left to stop. The replacement may have confirmed since the
        // read above; that is decided again under the lock.
        let message =
            "Replacement RiWork exited before restoring its windows. Existing windows remain open.";
        match self.settle_failure(&launch.request, message, Some(launch)) {
            Ok(true) => Ok(true),
            Ok(false) => Err(message.to_owned()),
            Err(_) => {
                // The failure could not be recorded (registry busy). Withdraw
                // the ticket so a late replacement cannot confirm a reload
                // this GUI has given up on.
                if let Ok(ticket) = self.manager.restore_path(&launch.request.ticket_id) {
                    let _ = fs::remove_file(ticket);
                }
                Err(message.to_owned())
            }
        }
    }
    /// The replacement is running but did not confirm in time. In this order:
    ///
    /// 1. Under the registry lock, either find that it confirmed after all
    ///    (then that stands and nothing is touched) or withdraw its restore
    ///    ticket. `mark_restore_ready` checks the ticket under the same lock, so
    ///    from here the replacement can never confirm.
    /// 2. Stop it. Only a replacement that can no longer confirm is ever
    ///    signalled, and only after its identity is checked again.
    /// 3. Record the failure with what happened to it and where it hung.
    fn give_up_on_hung_replacement(&self, launch: &ReloadLaunch) -> Result<bool, String> {
        match self.abandon(&launch.request, launch) {
            Ok(true) => return Ok(true),
            Ok(false) => {}
            Err(_) => {
                // The registry is busy, so nothing proves the replacement has not
                // just confirmed. It is left alone rather than risk stopping one
                // that succeeded; its own watchdog is the backstop. Withdraw the
                // ticket so that a late confirmation is refused.
                if let Ok(ticket) = self.manager.restore_path(&launch.request.ticket_id) {
                    let _ = fs::remove_file(ticket);
                }
                self.drop_request();
                return Err(timeout_message(launch.pid, None, None));
            }
        }
        let outcome = launch.stop_replacement();
        // Read after the stop so it reflects the very last thing the replacement did.
        let diagnostic = self.manager.read_diagnostic(
            &launch.request.request_id,
            &launch.request.ticket_id,
            launch.pid,
        );
        let message = timeout_message(
            launch.pid,
            Some(&outcome),
            diagnostic
                .as_ref()
                .map(|(path, diagnostic)| (path.as_path(), diagnostic)),
        );
        if self.write_failure(&launch.request, &message).is_err() {
            self.drop_request();
        }
        Err(message)
    }
    /// Nothing could be recorded for the request being given up on, so it
    /// must not be launched again: `pending_reload` would find no response and
    /// start another replacement while the first may still be running. The
    /// requester then times out. Removing a file takes no registry lock.
    fn drop_request(&self) {
        if let Ok(path) = self.manager.request_path(&self.instance.id) {
            let _ = fs::remove_file(path);
        }
    }
    /// Under the registry lock: `true` if `launch` confirmed the reload,
    /// otherwise withdraw the restore ticket so that it never can. The check and
    /// the withdrawal are one step with respect to `mark_restore_ready`.
    fn abandon(&self, request: &ReloadRequest, launch: &ReloadLaunch) -> Result<bool, String> {
        if request.instance_id != self.instance.id {
            return Err("Reload request belongs to another GUI".to_owned());
        }
        let path = self
            .manager
            .response_path(&self.instance.id, &request.request_id)?;
        let ticket = self.manager.restore_path(&request.ticket_id)?;
        let _lock = self.manager.lock()?;
        let existing = read_optional::<Response>(&path).unwrap_or(None);
        if let Some(existing) = &existing
            && existing.request_id == request.request_id
            && self.confirmed_by(existing, Some(launch))
        {
            return Ok(true);
        }
        match fs::remove_file(&ticket) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("Cannot withdraw the restore ticket: {e}")),
        }
        Ok(false)
    }
    fn write_failure(&self, request: &ReloadRequest, message: &str) -> Result<(), String> {
        let path = self
            .manager
            .response_path(&self.instance.id, &request.request_id)?;
        let _lock = self.manager.lock()?;
        write_json(
            &path,
            &Response {
                request_id: request.request_id.clone(),
                instance_id: self.instance.id.clone(),
                state: ReloadState::Failed,
                new_instance: None,
                message: message.to_owned(),
            },
        )?;
        let _ = fs::remove_file(self.manager.restore_path(&request.ticket_id)?);
        Ok(())
    }
    pub fn fail_reload(&self, request: &ReloadRequest, message: &str) -> Result<(), String> {
        self.settle_failure(request, message, None).map(|_| ())
    }
    /// Record a failed reload unless a live replacement has already confirmed
    /// it, in which case that confirmation stands and `true` is returned. The
    /// check and the write share the registry lock with `mark_restore_ready`,
    /// so a GUI never gives up on a reload the replacement has completed.
    fn settle_failure(
        &self,
        request: &ReloadRequest,
        message: &str,
        launch: Option<&ReloadLaunch>,
    ) -> Result<bool, String> {
        if request.instance_id != self.instance.id {
            return Err("Reload request belongs to another GUI".to_owned());
        }
        let path = self
            .manager
            .response_path(&self.instance.id, &request.request_id)?;
        let _lock = self.manager.lock()?;
        // Whatever is there but unreadable cannot be a confirmation.
        let existing = read_optional::<Response>(&path).unwrap_or(None);
        if let Some(existing) = &existing
            && existing.request_id == request.request_id
            && self.confirmed_by(existing, launch)
        {
            return Ok(true);
        }
        write_json(
            &path,
            &Response {
                request_id: request.request_id.clone(),
                instance_id: self.instance.id.clone(),
                state: ReloadState::Failed,
                new_instance: None,
                message: message.to_owned(),
            },
        )?;
        let _ = fs::remove_file(self.manager.restore_path(&request.ticket_id)?);
        Ok(false)
    }
}
impl Drop for RuntimeRegistration {
    fn drop(&mut self) {
        if let Ok(_lock) = self.manager.lock() {
            let _ = fs::remove_file(
                self.manager
                    .instance_path(&self.instance.id)
                    .unwrap_or_default(),
            );
            let _ = fs::remove_file(
                self.manager
                    .request_path(&self.instance.id)
                    .unwrap_or_default(),
            );
        }
    }
}
// ---------------------------------------------------------------------------
// Stopping a replacement that failed
// ---------------------------------------------------------------------------

/// The process a GUI launched as its replacement, as it was when launched.
/// A pid alone is not an identity: the kernel start time is what says whether
/// the process now using that pid is still the one that was launched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReplacementProcess {
    pid: u32,
    uid: u32,
    started_token: String,
    /// Started as the leader of its own process group, so that group is the
    /// replacement plus whatever it spawned without moving elsewhere.
    leads_group: bool,
}
impl ReplacementProcess {
    /// `leads_group`: the process was started as leader of a group of its own.
    /// That is checked here rather than assumed.
    fn of(pid: u32, leads_group: bool) -> Option<Self> {
        let identity = process_identity(pid).ok().flatten()?;
        #[cfg(unix)]
        let leads_group = leads_group && unsafe { libc::getpgid(pid as libc::pid_t) } == pid as i32;
        Some(Self {
            pid,
            uid: identity.uid,
            started_token: identity.started_token,
            leads_group,
        })
    }
    /// `None` when the platform cannot say, which is never taken to mean gone.
    fn liveness(&self) -> Option<Liveness> {
        let identity = process_identity(self.pid).ok()?;
        Some(liveness(
            self,
            identity
                .as_ref()
                .map(|identity| (identity.uid, identity.started_token.as_str())),
        ))
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Liveness {
    /// Nothing runs under that pid.
    Gone,
    /// The process that was launched.
    Same,
    /// Something else runs under that pid now.
    Reused,
}
/// Compare what runs under the replacement's pid now, as `(uid, kernel start
/// time)`, with what was launched.
fn liveness(expected: &ReplacementProcess, actual: Option<(u32, &str)>) -> Liveness {
    match actual {
        None => Liveness::Gone,
        Some((uid, started)) if uid == expected.uid && started == expected.started_token => {
            Liveness::Same
        }
        Some(_) => Liveness::Reused,
    }
}
/// What a signal for the replacement may be sent to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scope {
    /// Its whole process group.
    Group(i32),
    /// Only that process.
    Process(i32),
}
/// Decide where a signal for the replacement may go, or `None` when nowhere is
/// safe. `live_pgid` is the group the pid is in now, `None` if it is gone. Never
/// the calling process, never its group, never pid 0 or 1 (which would address
/// the caller's group or every process), and a group only when the replacement
/// leads it: any other group it is in is shared with processes that are not ours
/// to signal.
fn signal_scope(
    pid: u32,
    leads_group: bool,
    live_pgid: Option<i32>,
    own_pid: u32,
    own_pgid: i32,
) -> Option<Scope> {
    let target = i32::try_from(pid).ok().filter(|pid| *pid > 1)?;
    if pid == own_pid || target == own_pgid {
        return None;
    }
    match live_pgid {
        Some(pgid) if pgid == target => Some(Scope::Group(target)),
        Some(_) => Some(Scope::Process(target)),
        // The leader is gone; only the group it led can be left.
        None => leads_group.then_some(Scope::Group(target)),
    }
}
/// What became of a replacement the old GUI gave up on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum StopOutcome {
    /// It exited after SIGTERM.
    Terminated,
    /// It ignored SIGTERM for the grace period and was killed.
    Killed,
    /// It was gone before anything was sent.
    AlreadyGone,
    /// Its pid belongs to another process now; nothing was sent.
    PidReused,
    /// The platform would not identify it at launch, so it is not touched.
    Unverified,
    /// Every safe target was ruled out (see `signal_scope`).
    Refused,
    /// Still there after SIGKILL, typically stuck inside the kernel.
    Survived,
}
impl StopOutcome {
    fn describe(&self, pid: u32) -> String {
        match self {
            Self::Terminated | Self::Killed => {
                format!("The unresponsive replacement (pid {pid}) was stopped.")
            }
            Self::AlreadyGone => format!("The replacement (pid {pid}) had already exited."),
            Self::PidReused => format!(
                "The replacement (pid {pid}) had already exited, and its pid now belongs to another process, which was left alone."
            ),
            Self::Unverified => format!(
                "The replacement (pid {pid}) could not be identified, so it was left running."
            ),
            Self::Refused => format!(
                "The replacement (pid {pid}) could not be signalled safely and was left running."
            ),
            Self::Survived => format!(
                "The replacement (pid {pid}) did not exit after SIGKILL and may still be running."
            ),
        }
    }
}
impl ReloadLaunch {
    /// Terminate the replacement: SIGTERM, `stop_grace`, then SIGKILL, sent to
    /// its process group when it leads one. Terminal clients (`login`, `tmux
    /// attach`) are children of the terminals, not of the group: they lose their
    /// pty with the replacement and hang up. tmux sessions live in the tmux
    /// server, which is neither, and are untouched.
    fn stop_replacement(&self) -> StopOutcome {
        match &self.replacement {
            Some(process) => stop_process(process, self.stop_grace),
            None => StopOutcome::Unverified,
        }
    }
}
#[cfg(unix)]
const STOP_POLL: Duration = Duration::from_millis(25);
#[cfg(unix)]
fn group_exists(pgid: i32) -> bool {
    // Signal 0 checks existence only. macOS answers EPERM, not success, for a
    // group whose only members are zombies, and that group is as good as gone:
    // nothing in it can be signalled or is running.
    unsafe { libc::kill(-pgid, 0) == 0 }
}
#[cfg(unix)]
fn send_signal(scope: Scope, signal: i32) {
    // Failures (ESRCH: already gone) are decided by looking again, not here.
    let _ = unsafe {
        match scope {
            Scope::Group(group) => libc::killpg(group, signal),
            Scope::Process(process) => libc::kill(process, signal),
        }
    };
}
/// Where a signal may go right now, re-checking the replacement's identity.
#[cfg(unix)]
fn stop_plan(process: &ReplacementProcess) -> Result<Scope, StopOutcome> {
    let (own_pid, own_pgid) = unsafe { (std::process::id(), libc::getpgrp()) };
    let pgid = |pid: u32| {
        let pgid = unsafe { libc::getpgid(pid as libc::pid_t) };
        (pgid > 0).then_some(pgid)
    };
    match process.liveness() {
        None => Err(StopOutcome::Unverified),
        Some(Liveness::Reused) => Err(StopOutcome::PidReused),
        Some(Liveness::Same) => signal_scope(
            process.pid,
            process.leads_group,
            pgid(process.pid),
            own_pid,
            own_pgid,
        )
        .ok_or(StopOutcome::Refused),
        Some(Liveness::Gone) => {
            // Its group can outlive it. A group that still exists under its pid
            // is its own: the kernel never reuses a pid that names a live group.
            let group = i32::try_from(process.pid)
                .ok()
                .filter(|_| process.leads_group);
            match group {
                Some(group) if group_exists(group) => {
                    signal_scope(process.pid, true, None, own_pid, own_pgid)
                        .ok_or(StopOutcome::Refused)
                }
                _ => Err(StopOutcome::AlreadyGone),
            }
        }
    }
}
#[cfg(unix)]
fn wait_until_gone(process: &ReplacementProcess, scope: Scope, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        let gone = match process.liveness() {
            None | Some(Liveness::Same) => false,
            // Someone else has the pid: what was launched is gone, and nothing
            // more may be sent to that pid.
            Some(Liveness::Reused) => true,
            Some(Liveness::Gone) => match scope {
                Scope::Group(group) => !group_exists(group),
                Scope::Process(_) => true,
            },
        };
        if gone {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(STOP_POLL);
    }
}
/// SIGTERM, wait up to `grace`, SIGKILL. The identity is verified before each
/// signal, so a pid that was reused meanwhile is never signalled.
#[cfg(unix)]
fn stop_process(process: &ReplacementProcess, grace: Duration) -> StopOutcome {
    let scope = match stop_plan(process) {
        Ok(scope) => scope,
        Err(outcome) => return outcome,
    };
    send_signal(scope, libc::SIGTERM);
    if wait_until_gone(process, scope, grace) {
        return StopOutcome::Terminated;
    }
    let scope = match stop_plan(process) {
        Ok(scope) => scope,
        // It went away while the last look was being taken.
        Err(StopOutcome::AlreadyGone | StopOutcome::PidReused) => return StopOutcome::Terminated,
        Err(outcome) => return outcome,
    };
    send_signal(scope, libc::SIGKILL);
    if wait_until_gone(process, scope, KILL_WAIT) {
        StopOutcome::Killed
    } else {
        StopOutcome::Survived
    }
}
#[cfg(not(unix))]
fn stop_process(_process: &ReplacementProcess, _grace: Duration) -> StopOutcome {
    StopOutcome::Unverified
}
/// The reason recorded when a GUI gives up on a replacement that is still
/// running: what it was doing (if the replacement left a diagnostic) and what
/// was done about it.
fn timeout_message(
    pid: u32,
    outcome: Option<&StopOutcome>,
    diagnostic: Option<(&Path, &ReloadDiagnostic)>,
) -> String {
    let mut message = format!(
        "Replacement RiWork did not confirm restoration within {} seconds",
        REPLACEMENT_TIMEOUT.as_secs()
    );
    if let Some((path, diagnostic)) = diagnostic {
        message.push_str(&format!(
            ": {}; see {}",
            diagnostic.stuck_at(),
            path.display()
        ));
    }
    message.push('.');
    if let Some(outcome) = outcome {
        message.push(' ');
        message.push_str(&outcome.describe(pid));
    }
    message.push_str(" Existing windows remain open.");
    message
}

// ---------------------------------------------------------------------------
// Reload diagnostics
// ---------------------------------------------------------------------------

/// The step a restoring GUI was last seen in while attaching a terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AttachStep {
    Start,
    Done,
    Failed,
}
impl AttachStep {
    fn label(self) -> &'static str {
        match self {
            Self::Start => "attach_terminal start",
            Self::Done => "attach_terminal done",
            Self::Failed => "attach_terminal failed",
        }
    }
    fn from_label(label: &str) -> Option<Self> {
        [Self::Start, Self::Done, Self::Failed]
            .into_iter()
            .find(|step| step.label() == label)
    }
}
/// The last thing a restoring GUI did with a terminal: enough to say where it
/// hung without a debugger.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Breadcrumb {
    pub(crate) session_id: String,
    pub(crate) cwd: String,
    pub(crate) step: AttachStep,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DiagnosticStatus {
    /// Mirrored from memory while restoring, so a GUI that gives up on a
    /// wedged replacement can still say where it hung.
    InProgress,
    /// The replacement's own watchdog fired.
    WatchdogTimeout,
}
impl DiagnosticStatus {
    fn label(self) -> &'static str {
        match self {
            Self::InProgress => "in progress",
            Self::WatchdogTimeout => "watchdog timeout",
        }
    }
    fn from_label(label: &str) -> Option<Self> {
        [Self::InProgress, Self::WatchdogTimeout]
            .into_iter()
            .find(|status| status.label() == label)
    }
}
/// `<runtime>/diagnostics/reload-<request id>.txt`, plain `key: value` lines.
#[derive(Clone, Debug, PartialEq)]
struct ReloadDiagnostic {
    status: DiagnosticStatus,
    request_id: String,
    ticket_id: String,
    pid: u32,
    executable: PathBuf,
    build: String,
    elapsed: Duration,
    attached: usize,
    last: Option<Breadcrumb>,
}
const DIAGNOSTIC_HEADER: &str = "RiWork reload diagnostic";
fn build_label() -> String {
    format!(
        "{} {}",
        env!("CARGO_PKG_VERSION"),
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    )
}
fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}
impl ReloadDiagnostic {
    fn render(&self) -> String {
        let mut text = format!(
            "{DIAGNOSTIC_HEADER}\nstatus: {}\nrequest: {}\nticket: {}\nreplacement pid: {}\nexecutable: {}\nbuild: {}\nelapsed: {:.1}s\nterminals attached: {}\n",
            self.status.label(),
            self.request_id,
            self.ticket_id,
            self.pid,
            one_line(&self.executable.to_string_lossy()),
            one_line(&self.build),
            self.elapsed.as_secs_f64(),
            self.attached,
        );
        match &self.last {
            Some(last) => text.push_str(&format!(
                "last step: {}\nsession: {}\ncwd: {}\n",
                last.step.label(),
                one_line(&last.session_id),
                one_line(&last.cwd)
            )),
            None => text.push_str("last step: none\n"),
        }
        text
    }
    fn parse(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        if lines.next()? != DIAGNOSTIC_HEADER {
            return None;
        }
        let fields: Vec<(&str, &str)> = lines.filter_map(|line| line.split_once(": ")).collect();
        let get = |key: &str| {
            fields
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| *value)
        };
        let last = match get("last step")? {
            "none" => None,
            step => Some(Breadcrumb {
                session_id: get("session")?.to_owned(),
                cwd: get("cwd")?.to_owned(),
                step: AttachStep::from_label(step)?,
            }),
        };
        let elapsed = get("elapsed")?.strip_suffix('s')?.parse::<f64>().ok()?;
        Some(Self {
            status: DiagnosticStatus::from_label(get("status")?)?,
            request_id: get("request")?.to_owned(),
            ticket_id: get("ticket")?.to_owned(),
            pid: get("replacement pid")?.parse().ok()?,
            executable: PathBuf::from(get("executable")?),
            build: get("build")?.to_owned(),
            elapsed: Duration::try_from_secs_f64(elapsed).ok()?,
            attached: get("terminals attached")?.parse().ok()?,
            last,
        })
    }
    /// Where the restoring GUI was, for a person reading a failure.
    fn stuck_at(&self) -> String {
        match &self.last {
            Some(last) => {
                let (session, cwd) = (&last.session_id, &last.cwd);
                match last.step {
                    AttachStep::Start => {
                        format!("stuck attaching terminal for session {session} (cwd {cwd})")
                    }
                    AttachStep::Done => format!(
                        "no progress after attaching terminal for session {session} (cwd {cwd})"
                    ),
                    AttachStep::Failed => format!(
                        "no progress after a failed attach of terminal for session {session} (cwd {cwd})"
                    ),
                }
            }
            None => "stuck before attaching its first terminal".to_owned(),
        }
    }
}

// ---------------------------------------------------------------------------
// The replacement's side: breadcrumbs and the self-watchdog
// ---------------------------------------------------------------------------

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic elsewhere must not turn breadcrumbs into a second panic.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
/// What a restoring GUI has done so far, shared between the main thread that
/// records it and the watchdog thread that reports it. Recording never touches
/// the disk or waits for anything: the main thread may be about to hang.
pub(crate) struct RestoreProgress {
    started: Instant,
    active: AtomicBool,
    revision: AtomicU64,
    attached: AtomicUsize,
    last: Mutex<Option<Breadcrumb>>,
}
impl RestoreProgress {
    pub(crate) fn new() -> Self {
        Self {
            started: Instant::now(),
            active: AtomicBool::new(true),
            revision: AtomicU64::new(0),
            attached: AtomicUsize::new(0),
            last: Mutex::new(None),
        }
    }
    pub(crate) fn attach_started(&self, session_id: &str, cwd: &Path) {
        if !self.active.load(Ordering::Acquire) {
            return;
        }
        *locked(&self.last) = Some(Breadcrumb {
            session_id: session_id.to_owned(),
            cwd: cwd.to_string_lossy().into_owned(),
            step: AttachStep::Start,
        });
        self.revision.fetch_add(1, Ordering::AcqRel);
    }
    pub(crate) fn attach_finished(&self, session_id: &str, attached: bool) {
        if !self.active.load(Ordering::Acquire) {
            return;
        }
        if attached {
            self.attached.fetch_add(1, Ordering::AcqRel);
        }
        if let Some(last) = locked(&self.last).as_mut()
            && last.session_id == session_id
        {
            last.step = if attached {
                AttachStep::Done
            } else {
                AttachStep::Failed
            };
        }
        self.revision.fetch_add(1, Ordering::AcqRel);
    }
    fn stop(&self) {
        self.active.store(false, Ordering::Release);
    }
}
static RESTORE_PROGRESS: OnceLock<Arc<RestoreProgress>> = OnceLock::new();
/// Record that a terminal attach is starting. Free unless a restore watchdog is
/// running.
pub fn note_attach_started(session_id: &str, cwd: &Path) {
    if let Some(progress) = RESTORE_PROGRESS.get() {
        progress.attach_started(session_id, cwd);
    }
}
/// Record how the attach that `note_attach_started` announced ended.
pub fn note_attach_finished(session_id: &str, attached: bool) {
    if let Some(progress) = RESTORE_PROGRESS.get() {
        progress.attach_finished(session_id, attached);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WatchState {
    /// Waiting for the restore to be confirmed.
    Pending,
    /// The main thread is writing the confirmation.
    Confirming,
    /// Restoration was confirmed; the watchdog is finished.
    Confirmed,
    /// The deadline passed first; the process is being taken down.
    Fired,
}
struct WatchShared {
    state: Mutex<WatchState>,
    changed: Condvar,
}
#[derive(Clone, Copy, Debug)]
struct WatchdogConfig {
    deadline: Duration,
    /// Extra time for a confirmation that began before the deadline.
    confirm_grace: Duration,
    mirror_interval: Duration,
}
struct WatchdogContext {
    manager: RuntimeManager,
    instance_id: String,
    request_id: String,
    ticket_id: String,
    pid: u32,
    executable: PathBuf,
    progress: Arc<RestoreProgress>,
    /// Writes a diagnostic; a field so that a filesystem that stalls can be
    /// simulated.
    write: DiagnosticWriter,
}
type DiagnosticWriter = fn(&RuntimeManager, &ReloadDiagnostic) -> Result<PathBuf, String>;
impl WatchdogContext {
    fn diagnostic(&self, status: DiagnosticStatus) -> ReloadDiagnostic {
        ReloadDiagnostic {
            status,
            request_id: self.request_id.clone(),
            ticket_id: self.ticket_id.clone(),
            pid: self.pid,
            executable: self.executable.clone(),
            build: build_label(),
            elapsed: self.progress.started.elapsed(),
            attached: self.progress.attached.load(Ordering::Acquire),
            last: locked(&self.progress.last).clone(),
        }
    }
}
/// What a watchdog that fires does about the registry, off the main thread:
/// leave the diagnostic and remove the registration.
fn record_watchdog_timeout(
    manager: &RuntimeManager,
    instance_id: &str,
    diagnostic: &ReloadDiagnostic,
) -> Result<PathBuf, String> {
    let written = manager.write_diagnostic(diagnostic);
    manager.remove_registration(instance_id);
    written
}
/// Run `work` on its own thread and wait at most `limit` for it.
fn run_bounded(limit: Duration, work: impl FnOnce() + Send + 'static) -> bool {
    let (done, finished) = mpsc::channel();
    let spawned = thread::Builder::new()
        .name("riwork-restore-cleanup".to_owned())
        .spawn(move || {
            work();
            let _ = done.send(());
        });
    spawned.is_ok() && finished.recv_timeout(limit).is_ok()
}
/// A replacement always leads its own process group (the GUI that launched it
/// sees to that). Ask everything else in the group to terminate, without
/// ending this process before it has finished. A group this process does not
/// lead is a shell's or a session's and is left alone.
#[cfg(unix)]
fn terminate_own_group() {
    unsafe {
        let (pid, pgrp) = (libc::getpid(), libc::getpgrp());
        if pid > 1 && pgrp == pid {
            libc::signal(libc::SIGTERM, libc::SIG_IGN);
            libc::killpg(pgrp, libc::SIGTERM);
        }
    }
}
#[cfg(not(unix))]
fn terminate_own_group() {}
/// End the process now. `_exit` runs no atexit handlers and takes no locks: the
/// watchdog only fires when other threads may be wedged in ways that could make
/// an orderly `exit` hang too.
fn exit_now(code: i32) -> ! {
    #[cfg(unix)]
    unsafe {
        libc::_exit(code)
    }
    #[cfg(not(unix))]
    std::process::exit(code)
}
fn exit_after_restore_timeout(context: &WatchdogContext, diagnostic: ReloadDiagnostic) {
    let (manager, instance_id) = (context.manager.clone(), context.instance_id.clone());
    // Bounded: a filesystem that hangs must not keep a wedged process alive.
    run_bounded(WATCHDOG_CLEANUP_LIMIT, move || {
        let _ = record_watchdog_timeout(&manager, &instance_id, &diagnostic);
    });
    terminate_own_group();
    exit_now(RESTORE_WATCHDOG_EXIT_CODE)
}

/// Guards a restoring replacement against a main thread that never finishes.
/// GUI code cannot save it: when it is wedged the GUI that launched it may be
/// gone too, so a separate thread ends the process if restoration has not been
/// confirmed by the deadline. That timer does no file I/O of its own, so a
/// filesystem that stalls (a plausible reason for a wedged process) cannot hold
/// it up. A second thread mirrors the latest breadcrumb to disk, so whoever
/// gives up first can say where the restore hung.
pub struct RestoreWatchdog {
    shared: Arc<WatchShared>,
    progress: Arc<RestoreProgress>,
}
enum WatchStep {
    Wait(Duration),
    Fire,
    Confirmed,
    Done,
}
impl RestoreWatchdog {
    fn spawn(
        context: WatchdogContext,
        config: WatchdogConfig,
        on_timeout: impl FnOnce(&WatchdogContext, ReloadDiagnostic) + Send + 'static,
    ) -> Self {
        let shared = Arc::new(WatchShared {
            state: Mutex::new(WatchState::Pending),
            changed: Condvar::new(),
        });
        let progress = context.progress.clone();
        let context = Arc::new(context);
        let (timer_shared, timer_context) = (shared.clone(), context.clone());
        let timer = thread::Builder::new()
            .name("riwork-restore-watchdog".to_owned())
            .spawn(move || watch(&timer_shared, &timer_context, config, on_timeout));
        if let Err(error) = timer {
            eprintln!("riwork: cannot start the restore watchdog: {error}");
        }
        let (mirror_shared, mirror_context) = (shared.clone(), context);
        let mirror_thread = thread::Builder::new()
            .name("riwork-restore-breadcrumbs".to_owned())
            .spawn(move || {
                mirror(&mirror_shared, &mirror_context, config.mirror_interval);
            });
        if let Err(error) = mirror_thread {
            eprintln!("riwork: cannot record restore breadcrumbs: {error}");
        }
        Self { shared, progress }
    }
    /// The restore is about to be confirmed. Returns `false` if the watchdog
    /// has already fired, in which case the process is on its way out and must
    /// not confirm. Otherwise the watchdog holds off (for a bounded time) until
    /// `finish_confirm` or `abort_confirm`.
    pub fn begin_confirm(&self) -> bool {
        let mut state = locked(&self.shared.state);
        match *state {
            WatchState::Fired => false,
            WatchState::Pending => {
                *state = WatchState::Confirming;
                true
            }
            WatchState::Confirming | WatchState::Confirmed => true,
        }
    }
    /// The confirmation failed; the watchdog resumes its countdown.
    pub fn abort_confirm(&self) {
        let mut state = locked(&self.shared.state);
        if *state == WatchState::Confirming {
            *state = WatchState::Pending;
        }
        self.shared.changed.notify_all();
    }
    /// The restore was confirmed: retire the watchdog and its breadcrumbs.
    pub fn finish_confirm(&self) {
        {
            let mut state = locked(&self.shared.state);
            if *state == WatchState::Fired {
                return;
            }
            *state = WatchState::Confirmed;
        }
        self.progress.stop();
        self.shared.changed.notify_all();
    }
}
fn next_step(shared: &WatchShared, started: Instant, config: WatchdogConfig) -> WatchStep {
    let mut state = locked(&shared.state);
    let limit = match *state {
        WatchState::Pending => config.deadline,
        WatchState::Confirming => config.deadline.saturating_add(config.confirm_grace),
        WatchState::Confirmed => return WatchStep::Confirmed,
        WatchState::Fired => return WatchStep::Done,
    };
    let elapsed = started.elapsed();
    if elapsed >= limit {
        // Decided under the same lock `begin_confirm` takes, so exactly one of
        // "confirming" and "firing" happens.
        *state = WatchState::Fired;
        shared.changed.notify_all();
        return WatchStep::Fire;
    }
    WatchStep::Wait(limit - elapsed)
}
/// The timer: waits for the deadline and hands over to `on_timeout`. It only
/// waits on memory, so nothing on disk can delay it.
fn watch(
    shared: &WatchShared,
    context: &WatchdogContext,
    config: WatchdogConfig,
    on_timeout: impl FnOnce(&WatchdogContext, ReloadDiagnostic),
) {
    loop {
        match next_step(shared, context.progress.started, config) {
            WatchStep::Done | WatchStep::Confirmed => return,
            WatchStep::Fire => {
                context.progress.stop();
                on_timeout(
                    context,
                    context.diagnostic(DiagnosticStatus::WatchdogTimeout),
                );
                return;
            }
            WatchStep::Wait(remaining) => {
                let state = locked(&shared.state);
                if matches!(*state, WatchState::Pending | WatchState::Confirming) {
                    let _ = shared
                        .changed
                        .wait_timeout(state, remaining)
                        .unwrap_or_else(PoisonError::into_inner);
                }
            }
        }
    }
}
/// Copies breadcrumbs to disk while the restore runs, and removes the file once
/// it is confirmed. This is the only place that writes while the watchdog is
/// waiting, and it may block on a bad filesystem without harm: the timer does
/// not wait for it, and the timeout writes its own diagnostic (bounded).
fn mirror(shared: &WatchShared, context: &WatchdogContext, interval: Duration) {
    let mut mirrored = None;
    loop {
        match *locked(&shared.state) {
            WatchState::Confirmed => {
                context
                    .manager
                    .remove_diagnostic(&context.request_id, &context.ticket_id);
                return;
            }
            WatchState::Fired => return,
            WatchState::Pending | WatchState::Confirming => {}
        }
        let revision = context.progress.revision.load(Ordering::Acquire);
        // A failed write is tried again on the next round.
        if mirrored != Some(revision)
            && (context.write)(
                &context.manager,
                &context.diagnostic(DiagnosticStatus::InProgress),
            )
            .is_ok()
        {
            mirrored = Some(revision);
        }
        let state = locked(&shared.state);
        if matches!(*state, WatchState::Pending | WatchState::Confirming) {
            let _ = shared
                .changed
                .wait_timeout(state, interval)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

fn refresh_counts(report: &mut ReloadReport) {
    report.reloaded = report
        .instances
        .iter()
        .filter(|i| i.state == ReloadState::Reloaded)
        .count();
    report.pending = report
        .instances
        .iter()
        .filter(|i| i.state == ReloadState::Requested)
        .count();
    report.failed = report
        .instances
        .iter()
        .filter(|i| {
            matches!(
                i.state,
                ReloadState::Failed | ReloadState::TimedOut | ReloadState::Busy
            )
        })
        .count();
}
fn older_than(path: &Path, age: Duration) -> bool {
    fs::symlink_metadata(path)
        .ok()
        .filter(|meta| meta.is_file())
        .and_then(|meta| meta.modified().ok())
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|elapsed| elapsed > age)
}
fn warn_unreadable(count: usize) {
    if count > 0 {
        eprintln!(
            "riwork: warning: {count} running RiWork app(s) are registered in a format this build cannot read \
             (built from an incompatible version?). They are not listed and cannot be reloaded from here; \
             quit and reopen them."
        );
    }
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
fn valid_id(id: &str) -> Result<(), String> {
    if Uuid::parse_str(id).is_ok_and(|uuid| uuid.to_string() == id) {
        Ok(())
    } else {
        Err("Invalid RiWork runtime identifier".to_owned())
    }
}
fn validate_windows(windows: &[RuntimeWindow]) -> Result<(), String> {
    if windows.len() > 256 {
        return Err("Too many RiWork windows in restore snapshot".to_owned());
    }
    for window in windows {
        if let Some(bounds) = window.bounds {
            if ![bounds.x, bounds.y, bounds.width, bounds.height]
                .iter()
                .all(|v| v.is_finite())
                || bounds.width <= 0.0
                || bounds.height <= 0.0
            {
                return Err("Invalid restored window geometry".to_owned());
            }
        }
        if let Some(layout) = &window.layout {
            layout.clone().normalize()?;
        }
    }
    Ok(())
}
fn executable_path(path: &Path) -> Result<PathBuf, String> {
    let metadata = path
        .metadata()
        .map_err(|e| format!("Cannot read replacement executable {}: {e}", path.display()))?;
    if !metadata.is_file() {
        return Err("Replacement RiWork executable is not a file".to_owned());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err("Replacement RiWork file is not executable".to_owned());
        }
    }
    path.canonicalize().map_err(|e| e.to_string())
}
fn private_directory(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if path.metadata().map_err(|e| e.to_string())?.uid() != current_uid() {
            return Err("RiWork runtime directory belongs to another user".to_owned());
        }
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    }
    Ok(())
}
fn read_optional<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => read_json(path).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}
fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, String> {
    let data = read_private_file(path, MAX_BYTES)?;
    serde_json::from_slice(&data).map_err(|e| format!("Invalid RiWork runtime JSON: {e}"))
}
/// A regular file (never a symlink) that belongs to this user, at most `max` bytes.
fn read_private_file(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    let meta = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.len() > max {
        return Err("Invalid RiWork runtime file".to_owned());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.uid() != current_uid() {
            return Err("RiWork runtime file belongs to another user".to_owned());
        }
    }
    let mut data = vec![];
    File::open(path)
        .map_err(|e| e.to_string())?
        .take(max + 1)
        .read_to_end(&mut data)
        .map_err(|e| e.to_string())?;
    if data.len() as u64 > max {
        return Err("RiWork runtime file is too large".to_owned());
    }
    Ok(data)
}
/// Registrations, requests and tickets describe live processes, so a crash
/// makes them meaningless anyway. Skipping the sync keeps the periodic writes
/// from queueing a full fsync (F_FULLFSYNC on macOS) behind unrelated disk I/O.
fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    write_json_synced(path, value, false)
}
fn write_json_synced(path: &Path, value: &impl Serialize, sync: bool) -> Result<(), String> {
    let data = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    if data.len() as u64 > MAX_BYTES {
        return Err("RiWork runtime snapshot is too large".to_owned());
    }
    write_private_file(path, &data, sync)
}
/// A new private (0600) file renamed into place, so readers see the old file or
/// the whole new one, never a partial write.
fn write_private_file(path: &Path, data: &[u8], sync: bool) -> Result<(), String> {
    let temporary = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary).map_err(|e| e.to_string())?;
        file.write_all(data).map_err(|e| e.to_string())?;
        if sync {
            file.sync_all().map_err(|e| e.to_string())?;
        }
        fs::rename(&temporary, path).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

pub(crate) struct ProcessIdentity {
    pid: u32,
    uid: u32,
    started_token: String,
    pub(crate) executable: Option<PathBuf>,
}
impl ProcessIdentity {
    /// When the process started, as seconds since the epoch, where the platform
    /// reports that. Linux's start token counts ticks since boot instead.
    pub(crate) fn started_unix(&self) -> Option<f64> {
        if cfg!(target_os = "macos") {
            self.started_token.parse().ok()
        } else {
            None
        }
    }
}
fn process_matches(instance: &RuntimeInstance) -> Result<bool, String> {
    process_alive(instance.pid, instance.uid, &instance.started_token)
}
fn process_alive(pid: u32, uid: u32, started_token: &str) -> Result<bool, String> {
    if uid != current_uid() {
        return Ok(false);
    }
    Ok(process_identity(pid)?
        .is_some_and(|identity| identity.uid == uid && identity.started_token == started_token))
}
#[cfg(unix)]
fn current_uid() -> u32 {
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    unsafe { geteuid() }
}
#[cfg(not(unix))]
fn current_uid() -> u32 {
    0
}
#[cfg(target_os = "macos")]
pub(crate) fn process_identity(pid: u32) -> Result<Option<ProcessIdentity>, String> {
    use std::{ffi::c_void, os::unix::ffi::OsStringExt};
    #[repr(C)]
    struct BsdInfo {
        prefix: [u32; 12],
        comm: [u8; 16],
        name: [u8; 32],
        tail: [u32; 6],
        started_seconds: u64,
        started_microseconds: u64,
    }
    #[link(name = "proc")]
    unsafe extern "C" {
        fn proc_pidinfo(pid: i32, flavor: i32, arg: u64, buffer: *mut c_void, size: i32) -> i32;
        fn proc_pidpath(pid: i32, buffer: *mut c_void, size: u32) -> i32;
    }
    let Ok(pid_i32) = i32::try_from(pid) else {
        return Ok(None);
    };
    let mut info: BsdInfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<BsdInfo>();
    if unsafe {
        proc_pidinfo(
            pid_i32,
            3,
            0,
            (&mut info as *mut BsdInfo).cast(),
            size as i32,
        )
    } != size as i32
    {
        return Ok(None);
    }
    let mut bytes = vec![0u8; 4096];
    let executable =
        if unsafe { proc_pidpath(pid_i32, bytes.as_mut_ptr().cast(), bytes.len() as u32) } > 0 {
            bytes.truncate(
                bytes
                    .iter()
                    .position(|byte| *byte == 0)
                    .unwrap_or(bytes.len()),
            );
            Some(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
        } else {
            None
        };
    Ok(Some(ProcessIdentity {
        pid,
        uid: info.prefix[5],
        started_token: format!("{}.{:06}", info.started_seconds, info.started_microseconds),
        executable,
    }))
}
#[cfg(target_os = "linux")]
pub(crate) fn process_identity(pid: u32) -> Result<Option<ProcessIdentity>, String> {
    use std::os::unix::fs::MetadataExt;
    let root = PathBuf::from(format!("/proc/{pid}"));
    let Ok(stat) = fs::read_to_string(root.join("stat")) else {
        return Ok(None);
    };
    let Some((_, fields)) = stat.rsplit_once(')') else {
        return Ok(None);
    };
    let Some(started_token) = fields.split_whitespace().nth(19) else {
        return Ok(None);
    };
    let executable = fs::read_link(root.join("exe")).ok();
    let uid = root.metadata().map_err(|e| e.to_string())?.uid();
    Ok(Some(ProcessIdentity {
        pid,
        uid,
        started_token: started_token.to_owned(),
        executable,
    }))
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn process_identity(_pid: u32) -> Result<Option<ProcessIdentity>, String> {
    Err("RiWork GUI reloads are unsupported on this platform".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        path: PathBuf,
        manager: RuntimeManager,
    }
    impl Fixture {
        fn new() -> Self {
            let path = env::temp_dir().join(format!("riwork-runtime-test-{}", Uuid::new_v4()));
            let manager = RuntimeManager::at(path.join("registry")).unwrap();
            Self { path, manager }
        }
        fn register(&self, name: &str) -> RuntimeRegistration {
            self.manager.register(self.path.join(name)).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
    fn window(path: PathBuf) -> RuntimeWindow {
        RuntimeWindow {
            project_id: Some(Uuid::new_v4().to_string()),
            path,
            bounds: Some(WindowGeometry {
                x: 20.,
                y: 30.,
                width: 1000.,
                height: 700.,
            }),
            mode: WindowMode::Maximized,
            layout: None,
            focus_mode: true,
            focus_centered: false,
        }
    }
    #[test]
    fn installed_build_pointer_is_private_and_preserves_latest_executable() {
        let fixture = Fixture::new();
        assert_eq!(fixture.manager.installed_executable().unwrap(), None);
        let executable = env::current_exe().unwrap();
        fixture.manager.record_installed_build(&executable).unwrap();
        assert_eq!(
            fixture.manager.installed_executable().unwrap(),
            Some(executable_path(&executable).unwrap())
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = fixture
                .manager
                .directory
                .join("installed-build.json")
                .metadata()
                .unwrap();
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn removed_installed_build_falls_back_and_missing_install_is_not_recorded() {
        let fixture = Fixture::new();
        let executable = fixture.path.join("riwork");
        fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        }
        fixture.manager.record_installed_build(&executable).unwrap();
        assert!(fixture.manager.installed_executable().unwrap().is_some());
        fs::remove_file(&executable).unwrap();
        assert_eq!(fixture.manager.installed_executable().unwrap(), None);
        assert!(fixture.manager.record_installed_build(&executable).is_err());
        assert_eq!(fixture.manager.installed_executable().unwrap(), None);
    }
    #[test]
    fn all_state_homes_share_one_registry_and_drop_unregisters_only_its_instance() {
        let fixture = Fixture::new();
        let mut first = fixture.register("a");
        let mut second = fixture.register("b");
        let windows = vec![window(fixture.path.clone()), window(fixture.path.clone())];
        first.publish_windows(windows.clone()).unwrap();
        second.publish_windows(vec![]).unwrap();
        let instances = fixture.manager.instances().unwrap();
        assert_eq!(instances.len(), 2);
        assert_eq!(
            instances
                .iter()
                .find(|i| i.id == first.instance.id)
                .unwrap()
                .windows,
            windows
        );
        drop(first);
        assert_eq!(fixture.manager.instances().unwrap().len(), 1);
    }
    #[test]
    fn pid_reuse_is_stale_but_renamed_or_unlinked_executable_is_still_live() {
        let fixture = Fixture::new();
        let registration = fixture.register("state");
        let mut changed = registration.instance.clone();
        changed.executable = fixture.path.join("missing-replaced-executable");
        write_json(
            &fixture.manager.instance_path(&changed.id).unwrap(),
            &changed,
        )
        .unwrap();
        assert_eq!(fixture.manager.instances().unwrap().len(), 1);
        changed.started_token.push_str("-different-kernel-start");
        write_json(
            &fixture.manager.instance_path(&changed.id).unwrap(),
            &changed,
        )
        .unwrap();
        assert!(fixture.manager.instances().unwrap().is_empty());
    }
    #[test]
    fn queuing_and_wait_timeout_leave_old_gui_alive_and_do_not_launch_anything() {
        let fixture = Fixture::new();
        let mut registration = fixture.register("state");
        registration
            .publish_windows(vec![window(fixture.path.clone())])
            .unwrap();
        let report = fixture
            .manager
            .reload_all(&env::current_exe().unwrap())
            .unwrap();
        assert_eq!(report.requested, 1);
        assert_eq!(report.pending, 1);
        let request = registration.pending_reload().unwrap().unwrap();
        assert_eq!(request.request_id, report.request_id);
        let busy = fixture
            .manager
            .reload_all(&env::current_exe().unwrap())
            .unwrap();
        assert_eq!(busy.requested, 0);
        assert_eq!(busy.instances[0].state, ReloadState::Busy);
        let report = fixture
            .manager
            .wait_for_reload(report, Duration::ZERO)
            .unwrap();
        assert_eq!(report.failed, 1);
        assert_eq!(report.instances[0].state, ReloadState::TimedOut);
        assert!(process_matches(&registration.instance).unwrap());
        assert_eq!(fixture.manager.instances().unwrap().len(), 1);
    }
    #[test]
    fn readiness_requires_correct_child_and_matching_registered_windows() {
        let fixture = Fixture::new();
        let old = fixture.register("state");
        let mut new = fixture.register("state");
        let windows = vec![window(fixture.path.clone())];
        let report = fixture
            .manager
            .reload_all(&env::current_exe().unwrap())
            .unwrap();
        let request = old.pending_reload().unwrap().unwrap();
        let snapshot = RestoreSnapshot {
            ticket_id: request.ticket_id.clone(),
            request_id: request.request_id.clone(),
            old_instance_id: old.instance.id.clone(),
            replacement_executable: executable_path(&env::current_exe().unwrap()).unwrap(),
            state_home: new.instance.state_home.clone(),
            windows: windows.clone(),
            created_at: now(),
        };
        write_json(
            &fixture.manager.restore_path(&snapshot.ticket_id).unwrap(),
            &snapshot,
        )
        .unwrap();
        assert!(fixture.manager.mark_restore_ready(&snapshot, &new).is_err());
        new.publish_windows(windows).unwrap();
        fixture.manager.mark_restore_ready(&snapshot, &new).unwrap();
        let mut launch = ReloadLaunch {
            pid: std::process::id(),
            request: request.clone(),
            started: Instant::now(),
            exited: Arc::new(AtomicBool::new(false)),
            replacement: None,
            stop_grace: STOP_GRACE,
        };
        assert!(old.reload_ready(&launch).unwrap());
        launch.pid = 0;
        assert!(!old.reload_ready(&launch).unwrap());
        let report = fixture
            .manager
            .wait_for_reload(report, Duration::ZERO)
            .unwrap();
        assert!(report.reloaded >= 1);
        assert!(
            !fixture
                .manager
                .restore_path(&snapshot.ticket_id)
                .unwrap()
                .exists()
        );
    }
    #[test]
    fn failed_or_hung_replacement_keeps_old_alive_and_allows_retry() {
        let fixture = Fixture::new();
        let old = fixture.register("state");
        for exited in [true, false] {
            let report = fixture
                .manager
                .reload_all(&env::current_exe().unwrap())
                .unwrap();
            assert_eq!(report.requested, 1);
            let request = old.pending_reload().unwrap().unwrap();
            let launch = ReloadLaunch {
                pid: std::process::id(),
                request,
                started: Instant::now() - Duration::from_secs(26),
                exited: Arc::new(AtomicBool::new(exited)),
                replacement: None,
                stop_grace: STOP_GRACE,
            };
            assert!(old.reload_ready(&launch).is_err());
            assert!(old.pending_reload().unwrap().is_none());
            assert!(process_matches(&old.instance).unwrap());
            let report = fixture
                .manager
                .wait_for_reload(report, Duration::ZERO)
                .unwrap();
            assert_eq!(report.failed, 1);
            assert_eq!(report.instances[0].state, ReloadState::Failed);
        }
    }
    #[cfg(unix)]
    fn inode(path: &Path) -> u64 {
        use std::os::unix::fs::MetadataExt;
        path.metadata().unwrap().ino()
    }
    fn set_age(path: &Path, age: Duration) {
        OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(SystemTime::now() - age)
            .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn publishing_writes_only_changes_and_never_waits_for_the_registry_lock() {
        let fixture = Fixture::new();
        let mut registration = fixture.register("state");
        let path = fixture
            .manager
            .instance_path(&registration.instance.id)
            .unwrap();
        let windows = vec![window(fixture.path.clone())];
        assert!(registration.try_publish_windows(windows.clone()).unwrap());
        let written = inode(&path);
        assert_eq!(
            read_json::<RuntimeInstance>(&path).unwrap().windows,
            windows
        );
        // Unchanged windows leave the file alone (every write is a new inode).
        for _ in 0..3 {
            assert!(registration.try_publish_windows(windows.clone()).unwrap());
        }
        assert_eq!(inode(&path), written);
        // A busy registry is not waited for, and the change is not lost.
        let changed = vec![window(fixture.path.clone()), window(fixture.path.clone())];
        let held = fixture.manager.lock().unwrap();
        let started = Instant::now();
        assert!(!registration.try_publish_windows(changed.clone()).unwrap());
        assert!(started.elapsed() < Duration::from_millis(500));
        assert_eq!(inode(&path), written);
        drop(held);
        assert!(registration.try_publish_windows(changed.clone()).unwrap());
        assert_ne!(inode(&path), written);
        assert_eq!(
            read_json::<RuntimeInstance>(&path).unwrap().windows,
            changed
        );
        // An invalid layout is rejected without touching the last good file.
        let mut invalid = changed.clone();
        invalid[0].bounds.as_mut().unwrap().width = f32::NAN;
        assert!(registration.try_publish_windows(invalid).is_err());
        assert_eq!(registration.instance.windows, changed);
        // A removed registration is restored, and a due heartbeat refreshes it.
        fs::remove_file(&path).unwrap();
        assert!(registration.try_publish_windows(changed.clone()).unwrap());
        assert!(path.exists());
        let refreshed = inode(&path);
        registration.written_at = Instant::now() - HEARTBEAT * 2;
        assert!(registration.try_publish_windows(changed).unwrap());
        assert_ne!(inode(&path), refreshed);
    }

    #[cfg(unix)]
    #[test]
    fn a_confirmation_that_lands_before_the_timeout_write_is_never_overwritten() {
        let fixture = Fixture::new();
        let old = fixture.register("state");
        let mut new = fixture.register("state");
        let windows = vec![window(fixture.path.clone())];
        let report = fixture
            .manager
            .reload_all(&env::current_exe().unwrap())
            .unwrap();
        let request = old.pending_reload().unwrap().unwrap();
        let snapshot = RestoreSnapshot {
            ticket_id: request.ticket_id.clone(),
            request_id: request.request_id.clone(),
            old_instance_id: old.instance.id.clone(),
            replacement_executable: executable_path(&env::current_exe().unwrap()).unwrap(),
            state_home: new.instance.state_home.clone(),
            windows: windows.clone(),
            created_at: now(),
        };
        write_json(
            &fixture.manager.restore_path(&snapshot.ticket_id).unwrap(),
            &snapshot,
        )
        .unwrap();
        new.publish_windows(windows).unwrap();
        // The old GUI decided to time out (its earlier read saw nothing), then
        // the replacement confirmed before the failure was written.
        fixture.manager.mark_restore_ready(&snapshot, &new).unwrap();
        let launch = ReloadLaunch {
            pid: std::process::id(),
            request: request.clone(),
            started: Instant::now() - Duration::from_secs(26),
            exited: Arc::new(AtomicBool::new(false)),
            replacement: None,
            stop_grace: STOP_GRACE,
        };
        assert!(
            old.settle_failure(&request, "timed out", Some(&launch))
                .unwrap()
        );
        old.fail_reload(&request, "late failure").unwrap();
        let path = fixture
            .manager
            .response_path(&old.instance.id, &request.request_id)
            .unwrap();
        assert_eq!(
            read_json::<Response>(&path).unwrap().state,
            ReloadState::Reloaded
        );
        // The same decision in reload_ready reports success, not an error.
        assert!(old.reload_ready(&launch).unwrap());
        let report = fixture
            .manager
            .wait_for_reload(report, Duration::ZERO)
            .unwrap();
        assert!(report.reloaded >= 1);
        // A different process than the one launched does not count.
        let wrong = ReloadLaunch { pid: 1, ..launch };
        assert!(
            !old.settle_failure(&request, "wrong child", Some(&wrong))
                .unwrap()
        );
        assert_eq!(
            read_json::<Response>(&path).unwrap().state,
            ReloadState::Failed
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_replacement_cannot_confirm_a_reload_the_old_gui_already_abandoned() {
        let fixture = Fixture::new();
        let old = fixture.register("state");
        let mut new = fixture.register("state");
        let windows = vec![window(fixture.path.clone())];
        fixture
            .manager
            .reload_all(&env::current_exe().unwrap())
            .unwrap();
        let request = old.pending_reload().unwrap().unwrap();
        let snapshot = RestoreSnapshot {
            ticket_id: request.ticket_id.clone(),
            request_id: request.request_id.clone(),
            old_instance_id: old.instance.id.clone(),
            replacement_executable: executable_path(&env::current_exe().unwrap()).unwrap(),
            state_home: new.instance.state_home.clone(),
            windows: windows.clone(),
            created_at: now(),
        };
        let ticket = fixture.manager.restore_path(&snapshot.ticket_id).unwrap();
        write_json(&ticket, &snapshot).unwrap();
        new.publish_windows(windows).unwrap();
        old.fail_reload(&request, "gave up").unwrap();
        assert!(!ticket.exists());
        // Even if the replacement read the ticket before it was withdrawn, its
        // confirmation must not replace the failure.
        write_json(&ticket, &snapshot).unwrap();
        let error = fixture
            .manager
            .mark_restore_ready(&snapshot, &new)
            .unwrap_err();
        assert!(error.contains("gave up"), "{error}");
        let path = fixture
            .manager
            .response_path(&old.instance.id, &request.request_id)
            .unwrap();
        assert_eq!(
            read_json::<Response>(&path).unwrap().state,
            ReloadState::Failed
        );
    }

    #[test]
    fn an_unreadable_response_is_replaced_by_the_failure_record() {
        let fixture = Fixture::new();
        let old = fixture.register("state");
        fixture
            .manager
            .reload_all(&env::current_exe().unwrap())
            .unwrap();
        let request = old.pending_reload().unwrap().unwrap();
        let path = fixture
            .manager
            .response_path(&old.instance.id, &request.request_id)
            .unwrap();
        fs::write(&path, "{ not a response").unwrap();
        let launch = ReloadLaunch {
            pid: std::process::id(),
            request: request.clone(),
            started: Instant::now() - Duration::from_secs(26),
            exited: Arc::new(AtomicBool::new(false)),
            replacement: None,
            stop_grace: STOP_GRACE,
        };
        // Neither an error nor a stuck wait: the timeout is recorded properly.
        assert!(
            old.reload_ready(&launch)
                .unwrap_err()
                .contains("25 seconds")
        );
        assert_eq!(
            read_json::<Response>(&path).unwrap().state,
            ReloadState::Failed
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_request_for_a_vanished_executable_is_answered_instead_of_retried() {
        let fixture = Fixture::new();
        let old = fixture.register("state");
        let vanishing = fixture.path.join("vanishing");
        fs::write(&vanishing, "#!/bin/sh\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&vanishing, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let report = fixture.manager.reload_all(&vanishing).unwrap();
        assert_eq!(report.requested, 1);
        fs::remove_file(&vanishing).unwrap();
        assert!(old.pending_reload().unwrap().is_none());
        assert!(old.pending_reload().unwrap().is_none());
        let report = fixture
            .manager
            .wait_for_reload(report, Duration::ZERO)
            .unwrap();
        assert_eq!(report.instances[0].state, ReloadState::Failed);
        assert!(report.instances[0].message.contains("Cannot read"));
    }

    #[cfg(unix)]
    #[test]
    fn a_reload_request_can_only_launch_this_app_or_the_recorded_install() {
        let fixture = Fixture::new();
        let old = fixture.register("state");
        let marker = fixture.path.join("ran");
        let planted = fixture.path.join("planted");
        fs::write(
            &planted,
            format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&planted, fs::Permissions::from_mode(0o755)).unwrap();
        }
        fixture.manager.reload_all(&planted).unwrap();
        let request = old.pending_reload().unwrap().unwrap();
        let error = old.launch_reload(&request, vec![]).unwrap_err();
        assert!(error.contains("Refusing to launch"), "{error}");
        assert!(
            !fixture
                .manager
                .restore_path(&request.ticket_id)
                .unwrap()
                .exists()
        );
        std::thread::sleep(Duration::from_millis(200));
        assert!(!marker.exists());
        // The running executable and the recorded install are allowed.
        assert!(
            old.trusted_replacement(&env::current_exe().unwrap())
                .is_ok()
        );
        fixture.manager.record_installed_build(&planted).unwrap();
        assert_eq!(
            old.trusted_replacement(&planted).unwrap(),
            planted.canonicalize().unwrap()
        );
        // ...unless anyone could have modified it.
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&planted, fs::Permissions::from_mode(0o777)).unwrap();
        }
        assert!(old.trusted_replacement(&planted).is_err());
    }

    #[test]
    fn unreadable_registrations_are_counted_and_only_dead_or_ancient_ones_are_pruned() {
        let fixture = Fixture::new();
        let live = fixture.register("state");
        // A running GUI whose registration this build cannot parse.
        let incompatible = fixture.register("other");
        let path = fixture
            .manager
            .instance_path(&incompatible.instance.id)
            .unwrap();
        let mut value: serde_json::Value = read_json(&path).unwrap();
        value["windows"] = serde_json::json!("layout format 2");
        fs::write(&path, value.to_string()).unwrap();
        // The same, but for a process that no longer exists.
        let dead_id = Uuid::new_v4().to_string();
        let mut dead = value.clone();
        dead["id"] = dead_id.clone().into();
        dead["started_token"] = "0.000000".into();
        let dead_path = fixture.manager.instance_path(&dead_id).unwrap();
        fs::write(&dead_path, dead.to_string()).unwrap();
        // Garbage: kept while it might be a GUI mid-write, removed when old.
        let garbage = fixture
            .manager
            .instance_path(&Uuid::new_v4().to_string())
            .unwrap();
        fs::write(&garbage, "not json").unwrap();

        let (instances, unreadable) = fixture.manager.live_instances().unwrap();
        assert_eq!(
            instances.iter().map(|i| i.id.clone()).collect::<Vec<_>>(),
            vec![live.instance.id.clone()]
        );
        assert_eq!(unreadable, 2);
        assert!(!dead_path.exists());
        assert!(garbage.exists());
        let report = fixture
            .manager
            .reload_all(&env::current_exe().unwrap())
            .unwrap();
        assert_eq!(report.unreadable_registrations, 2);
        assert_eq!(report.instances.len(), 1);
        assert_eq!(
            serde_json::to_value(&report).unwrap()["unreadable_registrations"],
            2
        );
        set_age(&garbage, UNREADABLE_AGE + Duration::from_secs(1));
        let (_, unreadable) = fixture.manager.live_instances().unwrap();
        assert_eq!(unreadable, 1);
        assert!(!garbage.exists());
        assert!(path.exists());
    }

    #[test]
    fn leftover_reload_files_are_pruned_but_recent_ones_and_registrations_stay() {
        let fixture = Fixture::new();
        let live = fixture.register("state");
        let id = || Uuid::new_v4().to_string();
        let mut old_files = vec![];
        let mut fresh_files = vec![];
        for name in ["requests", "restores", "responses"] {
            let dir = fixture.manager.directory.join(name);
            let old = dir.join(format!("{}.json", id()));
            let fresh = dir.join(format!("{}.json", id()));
            fs::write(&old, "{}").unwrap();
            fs::write(&fresh, "{}").unwrap();
            set_age(&old, LEFTOVER_AGE + Duration::from_secs(60));
            old_files.push(old);
            fresh_files.push(fresh);
        }
        let stale_temporary =
            fixture
                .manager
                .directory
                .join("instances")
                .join(format!("{}.{}.tmp", id(), id()));
        fs::write(&stale_temporary, "{").unwrap();
        set_age(&stale_temporary, LEFTOVER_AGE + Duration::from_secs(60));
        // Registrations are judged by their process, however old the file is.
        let registration = fixture.manager.instance_path(&live.instance.id).unwrap();
        set_age(&registration, LEFTOVER_AGE * 3);
        fixture
            .manager
            .reload_all(&env::current_exe().unwrap())
            .unwrap();
        assert!(old_files.iter().all(|path| !path.exists()));
        assert!(!stale_temporary.exists());
        assert!(fresh_files.iter().all(|path| path.exists()));
        assert!(registration.exists());
    }

    #[test]
    fn identifiers_geometry_and_private_file_read_are_validated() {
        let fixture = Fixture::new();
        assert!(fixture.manager.restore_path("../escape").is_err());
        let mut invalid = window(fixture.path.clone());
        invalid.bounds.as_mut().unwrap().width = f32::NAN;
        assert!(validate_windows(&[invalid]).is_err());
        let old = fixture.register("state");
        let path = fixture.manager.instance_path(&old.instance.id).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::{PermissionsExt, symlink};
            assert_eq!(
                fixture
                    .manager
                    .directory
                    .metadata()
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
            let link = fixture.path.join("symlink.json");
            symlink(path, &link).unwrap();
            assert!(read_json::<RuntimeInstance>(&link).is_err());
        }
    }

    // -- Stopping a replacement that failed ---------------------------------

    fn expected_process(pid: u32) -> ReplacementProcess {
        ReplacementProcess {
            pid,
            uid: 501,
            started_token: "1700000000.000123".to_owned(),
            leads_group: true,
        }
    }

    #[test]
    fn identity_needs_the_same_uid_and_kernel_start_time() {
        let expected = expected_process(4242);
        assert_eq!(liveness(&expected, None), Liveness::Gone);
        assert_eq!(
            liveness(&expected, Some((501, "1700000000.000123"))),
            Liveness::Same
        );
        // The pid now belongs to a process started later (or by someone else).
        assert_eq!(
            liveness(&expected, Some((501, "1700000099.000001"))),
            Liveness::Reused
        );
        assert_eq!(
            liveness(&expected, Some((0, "1700000000.000123"))),
            Liveness::Reused
        );
    }

    #[test]
    fn signals_never_go_to_pid_zero_one_the_caller_or_the_callers_group() {
        let (own_pid, own_pgid) = (500, 480);
        // A replacement that leads its own group gets the whole group.
        assert_eq!(
            signal_scope(900, true, Some(900), own_pid, own_pgid),
            Some(Scope::Group(900))
        );
        // One that moved into another group is signalled alone: that group is
        // somebody else's.
        assert_eq!(
            signal_scope(900, true, Some(123), own_pid, own_pgid),
            Some(Scope::Process(900))
        );
        // Gone: only its former group can be left, and only if it led one.
        assert_eq!(
            signal_scope(900, true, None, own_pid, own_pgid),
            Some(Scope::Group(900))
        );
        assert_eq!(signal_scope(900, false, None, own_pid, own_pgid), None);
        for pid in [0, 1] {
            assert_eq!(
                signal_scope(pid, true, Some(pid as i32), own_pid, own_pgid),
                None
            );
        }
        assert_eq!(signal_scope(u32::MAX, true, None, own_pid, own_pgid), None);
        assert_eq!(
            signal_scope(own_pid, true, Some(own_pid as i32), own_pid, own_pgid),
            None
        );
        // The group of the caller, by any route.
        assert_eq!(signal_scope(480, true, Some(480), own_pid, own_pgid), None);
    }

    /// A stand-in for a replacement: a shell in its own process group that
    /// starts two `sleep` children and waits for them. Everything it starts is
    /// ours, and it is torn down when the test ends however it ends.
    #[cfg(unix)]
    struct FakeReplacement {
        pid: u32,
        process: ReplacementProcess,
        exited: Arc<AtomicBool>,
        children: Vec<u32>,
        directory: PathBuf,
        reaper: Option<thread::JoinHandle<()>>,
    }
    #[cfg(unix)]
    impl FakeReplacement {
        fn spawn(directory: &Path, script: &str) -> Self {
            use std::os::unix::process::CommandExt;
            fs::create_dir_all(directory).unwrap();
            let pids = directory.join("pids");
            let mut command = Command::new("sh");
            command
                .arg("-c")
                .arg(script)
                .arg(&pids)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0);
            let mut child = command.spawn().unwrap();
            let pid = child.id();
            let process = ReplacementProcess::of(pid, true).unwrap();
            let exited = Arc::new(AtomicBool::new(false));
            let flag = exited.clone();
            let reaper = thread::spawn(move || {
                let _ = child.wait();
                flag.store(true, Ordering::Release);
            });
            let mut children = vec![];
            let deadline = Instant::now() + Duration::from_secs(10);
            while children.len() < 2 && Instant::now() < deadline {
                children = fs::read_to_string(&pids)
                    .unwrap_or_default()
                    .lines()
                    .filter_map(|line| line.trim().parse().ok())
                    .collect();
                thread::sleep(Duration::from_millis(20));
            }
            assert_eq!(children.len(), 2, "the fake replacement did not start");
            Self {
                pid,
                process,
                exited,
                children,
                directory: directory.to_owned(),
                reaper: Some(reaper),
            }
        }
        /// Two sleeping children; SIGTERM ends all of it.
        fn well_behaved(directory: &Path) -> Self {
            Self::spawn(
                directory,
                r#"sleep 60 & echo $! >> "$0"; sleep 60 & echo $! >> "$0"; wait"#,
            )
        }
        /// Ignores SIGTERM, and so do its children (ignored signals are inherited).
        fn stubborn(directory: &Path) -> Self {
            Self::spawn(
                directory,
                r#"trap '' TERM; sleep 60 & echo $! >> "$0"; sleep 60 & echo $! >> "$0"; while :; do sleep 1; done"#,
            )
        }
        fn launch(&self, request: ReloadRequest, replacement: bool) -> ReloadLaunch {
            ReloadLaunch {
                pid: self.pid,
                request,
                started: Instant::now() - Duration::from_secs(26),
                exited: self.exited.clone(),
                replacement: replacement.then(|| self.process.clone()),
                stop_grace: Duration::from_millis(400),
            }
        }
        fn leader_alive(&self) -> bool {
            !self.exited.load(Ordering::Acquire)
        }
        fn child_alive(&self, pid: u32) -> bool {
            unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
        }
        /// Everything it started is gone (waiting a moment for init to reap).
        fn assert_all_gone(&self) {
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline
                && (self.leader_alive() || self.children.iter().any(|pid| self.child_alive(*pid)))
            {
                thread::sleep(Duration::from_millis(20));
            }
            assert!(!self.leader_alive(), "the replacement is still running");
            for pid in &self.children {
                assert!(!self.child_alive(*pid), "child {pid} is still running");
            }
        }
    }
    #[cfg(unix)]
    impl Drop for FakeReplacement {
        fn drop(&mut self) {
            // The group cannot have been reused while it has members.
            if group_exists(self.pid as i32) {
                unsafe { libc::killpg(self.pid as libc::pid_t, libc::SIGKILL) };
            }
            if let Some(reaper) = self.reaper.take() {
                let _ = reaper.join();
            }
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "slow: real process group and a timed grace period"]
    fn a_replacement_that_ignores_sigterm_is_killed_with_its_children() {
        let fixture = Fixture::new();
        let fake = FakeReplacement::stubborn(&fixture.path.join("fake"));
        let started = Instant::now();
        let outcome = stop_process(&fake.process, Duration::from_millis(400));
        assert_eq!(outcome, StopOutcome::Killed);
        // It was given the grace period before the SIGKILL.
        assert!(started.elapsed() >= Duration::from_millis(400));
        fake.assert_all_gone();
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "slow: real process group"]
    fn a_reused_pid_is_never_signalled() {
        let fixture = Fixture::new();
        let fake = FakeReplacement::well_behaved(&fixture.path.join("fake"));
        // Same pid, but the recorded process started at a different time (or
        // belongs to someone else): as if the launched one died and the pid
        // was handed out again.
        for reused in [
            ReplacementProcess {
                started_token: format!("{}-earlier", fake.process.started_token),
                ..fake.process.clone()
            },
            ReplacementProcess {
                uid: fake.process.uid + 1,
                ..fake.process.clone()
            },
        ] {
            assert_eq!(
                stop_process(&reused, Duration::from_millis(100)),
                StopOutcome::PidReused
            );
            assert!(fake.leader_alive());
            assert!(fake.children.iter().all(|pid| fake.child_alive(*pid)));
        }
        // Through the reload path too: the failure says so and nothing dies.
        let old = fixture.register("state");
        fixture
            .manager
            .reload_all(&env::current_exe().unwrap())
            .unwrap();
        let request = old.pending_reload().unwrap().unwrap();
        let mut launch = fake.launch(request, true);
        launch.replacement.as_mut().unwrap().started_token.push('x');
        let error = old.reload_ready(&launch).unwrap_err();
        assert!(
            error.contains("its pid now belongs to another process"),
            "{error}"
        );
        assert!(!error.contains("was stopped"), "{error}");
        assert!(fake.leader_alive());
        assert!(fake.children.iter().all(|pid| fake.child_alive(*pid)));
    }

    #[cfg(unix)]
    #[test]
    fn this_process_and_its_group_are_refused() {
        let me = ReplacementProcess::of(std::process::id(), true).unwrap();
        assert_eq!(
            stop_process(&me, Duration::from_millis(50)),
            StopOutcome::Refused
        );
    }

    /// The fake replacement registered as the GUI that restored its windows,
    /// then asking to confirm. `ticket` is whether the old GUI still has the
    /// restore ticket out (the replacement read it when it started).
    #[cfg(unix)]
    fn confirm_as(
        fixture: &Fixture,
        fake: &FakeReplacement,
        old: &RuntimeRegistration,
        request: &ReloadRequest,
        ticket: bool,
    ) -> Result<(), String> {
        let windows = vec![window(fixture.path.clone())];
        let executable = executable_path(&env::current_exe().unwrap()).unwrap();
        let state_home = fixture.path.join("state").canonicalize().unwrap();
        let mut new = RuntimeRegistration {
            manager: fixture.manager.clone(),
            instance: RuntimeInstance {
                id: Uuid::new_v4().to_string(),
                pid: fake.pid,
                uid: fake.process.uid,
                started_token: fake.process.started_token.clone(),
                executable: executable.clone(),
                state_home: state_home.clone(),
                windows: vec![],
                updated_at: now(),
            },
            written_at: Instant::now(),
        };
        let snapshot = RestoreSnapshot {
            ticket_id: request.ticket_id.clone(),
            request_id: request.request_id.clone(),
            old_instance_id: old.instance.id.clone(),
            replacement_executable: executable,
            state_home,
            windows: windows.clone(),
            created_at: now(),
        };
        if ticket {
            write_json(
                &fixture.manager.restore_path(&snapshot.ticket_id).unwrap(),
                &snapshot,
            )
            .unwrap();
        }
        new.publish_windows(windows).unwrap();
        let result = fixture.manager.mark_restore_ready(&snapshot, &new);
        // The fake is not this process: do not let its registration be removed
        // by a Drop that would also delete files a later assertion reads.
        std::mem::forget(new);
        result
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "slow: real process group"]
    fn a_replacement_that_confirmed_just_before_the_timeout_is_never_stopped() {
        let fixture = Fixture::new();
        let old = fixture.register("state");
        let fake = FakeReplacement::stubborn(&fixture.path.join("fake"));
        let report = fixture
            .manager
            .reload_all(&env::current_exe().unwrap())
            .unwrap();
        let request = old.pending_reload().unwrap().unwrap();
        // The old GUI's clock says it is time to give up, but the confirmation
        // landed first.
        confirm_as(&fixture, &fake, &old, &request, true).unwrap();
        let launch = fake.launch(request.clone(), true);
        assert!(old.abandon(&request, &launch).unwrap());
        assert!(old.reload_ready(&launch).unwrap());
        assert_eq!(launch.replacement.as_ref(), Some(&fake.process));
        assert!(fake.leader_alive(), "a confirmed replacement was stopped");
        assert!(fake.children.iter().all(|pid| fake.child_alive(*pid)));
        let path = fixture
            .manager
            .response_path(&old.instance.id, &request.request_id)
            .unwrap();
        assert_eq!(
            read_json::<Response>(&path).unwrap().state,
            ReloadState::Reloaded
        );
        let report = fixture
            .manager
            .wait_for_reload(report, Duration::ZERO)
            .unwrap();
        assert_eq!(report.reloaded, 1);
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "slow: real process group"]
    fn a_replacement_can_no_longer_confirm_once_it_is_being_stopped() {
        let fixture = Fixture::new();
        let old = fixture.register("state");
        let fake = FakeReplacement::well_behaved(&fixture.path.join("fake"));
        fixture
            .manager
            .reload_all(&env::current_exe().unwrap())
            .unwrap();
        let request = old.pending_reload().unwrap().unwrap();
        let launch = fake.launch(request.clone(), true);
        // Step one of giving up: nothing has confirmed, so the ticket goes.
        assert!(!old.abandon(&request, &launch).unwrap());
        // The kill is about to happen; a confirmation now must be refused, even
        // though no failure has been written yet.
        let error = confirm_as(&fixture, &fake, &old, &request, false).unwrap_err();
        assert!(!error.is_empty());
        let path = fixture
            .manager
            .response_path(&old.instance.id, &request.request_id)
            .unwrap();
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "slow: real child processes"]
    fn a_replacement_outside_its_own_group_is_signalled_alone() {
        use std::os::unix::process::CommandExt;
        // Not started as a group leader, so it shares this test's group: the
        // group must not be signalled, only the process.
        let mut child = Command::new("sleep")
            .arg("60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let process = ReplacementProcess::of(child.id(), true).unwrap();
        assert!(!process.leads_group);
        let mut alone = Command::new("sleep");
        alone.arg("60").process_group(0);
        let mut leader = alone.spawn().unwrap();
        assert!(
            ReplacementProcess::of(leader.id(), true)
                .unwrap()
                .leads_group
        );
        assert!(
            !ReplacementProcess::of(leader.id(), false)
                .unwrap()
                .leads_group
        );
        let _ = leader.kill();
        let _ = leader.wait();
        let reaper = thread::spawn(move || child.wait().unwrap());
        assert_eq!(
            stop_process(&process, Duration::from_secs(5)),
            StopOutcome::Terminated
        );
        assert!(!reaper.join().unwrap().success());
    }

    // -- Diagnostics and breadcrumbs ----------------------------------------

    fn sample_diagnostic(request: &str, ticket: &str, pid: u32) -> ReloadDiagnostic {
        ReloadDiagnostic {
            status: DiagnosticStatus::WatchdogTimeout,
            request_id: request.to_owned(),
            ticket_id: ticket.to_owned(),
            pid,
            executable: PathBuf::from("/Applications/RiWork.app/Contents/MacOS/riwork"),
            build: "0.1.0 release".to_owned(),
            elapsed: Duration::from_millis(40_250),
            attached: 6,
            last: Some(Breadcrumb {
                session_id: "0f9c3c1e-session".to_owned(),
                cwd: "/Users/me/work tree/α".to_owned(),
                step: AttachStep::Start,
            }),
        }
    }

    #[test]
    fn the_diagnostic_file_holds_the_breadcrumb_count_elapsed_time_and_build() {
        let diagnostic = sample_diagnostic(
            &Uuid::new_v4().to_string(),
            &Uuid::new_v4().to_string(),
            777,
        );
        let text = diagnostic.render();
        assert_eq!(
            text,
            format!(
                "RiWork reload diagnostic\n\
                 status: watchdog timeout\n\
                 request: {}\n\
                 ticket: {}\n\
                 replacement pid: 777\n\
                 executable: /Applications/RiWork.app/Contents/MacOS/riwork\n\
                 build: 0.1.0 release\n\
                 elapsed: 40.2s\n\
                 terminals attached: 6\n\
                 last step: attach_terminal start\n\
                 session: 0f9c3c1e-session\n\
                 cwd: /Users/me/work tree/α\n",
                diagnostic.request_id, diagnostic.ticket_id
            )
        );
        let parsed = ReloadDiagnostic::parse(&text).unwrap();
        assert_eq!(parsed.last, diagnostic.last);
        assert_eq!(parsed.attached, 6);
        assert_eq!(parsed.pid, 777);
        assert_eq!(parsed.status, DiagnosticStatus::WatchdogTimeout);
        assert_eq!(parsed.executable, diagnostic.executable);
        assert!((parsed.elapsed.as_secs_f64() - 40.2).abs() < 0.11);
        // Before any terminal was reached there is nothing to name.
        let nothing = ReloadDiagnostic {
            last: None,
            attached: 0,
            ..diagnostic
        };
        let parsed = ReloadDiagnostic::parse(&nothing.render()).unwrap();
        assert_eq!(parsed.last, None);
        assert_eq!(
            parsed.stuck_at(),
            "stuck before attaching its first terminal"
        );
        // A value cannot smuggle in a line of its own.
        let hostile = ReloadDiagnostic {
            last: Some(Breadcrumb {
                session_id: "s\nstatus: in progress".to_owned(),
                cwd: "/tmp/\r\nx".to_owned(),
                step: AttachStep::Done,
            }),
            ..nothing
        };
        let parsed = ReloadDiagnostic::parse(&hostile.render()).unwrap();
        assert_eq!(parsed.status, DiagnosticStatus::WatchdogTimeout);
        assert_eq!(hostile.render().lines().count(), 12);
        // Anything else is not a diagnostic.
        assert!(ReloadDiagnostic::parse("not a diagnostic").is_none());
        assert!(
            ReloadDiagnostic::parse("RiWork reload diagnostic\nstatus: in progress\n").is_none()
        );
    }

    #[test]
    fn diagnostics_are_private_named_by_launch_and_only_read_for_their_replacement() {
        let fixture = Fixture::new();
        let id = || Uuid::new_v4().to_string();
        let (request, ticket, other_ticket) = (id(), id(), id());
        let path = fixture
            .manager
            .write_diagnostic(&sample_diagnostic(&request, &ticket, 777))
            .unwrap();
        assert_eq!(
            path,
            fixture
                .manager
                .directory
                .join("diagnostics")
                .join(format!("reload-{request}-{ticket}.txt"))
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
            assert_eq!(
                path.parent()
                    .unwrap()
                    .metadata()
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        // Only the whole file is ever visible: no leftover temporary files.
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        let (found, diagnostic) = fixture
            .manager
            .read_diagnostic(&request, &ticket, 777)
            .unwrap();
        assert_eq!(found, path);
        assert_eq!(diagnostic.attached, 6);
        // A file from another replacement of the same launch is not this one's.
        assert!(
            fixture
                .manager
                .read_diagnostic(&request, &ticket, 778)
                .is_none()
        );
        assert!(
            fixture
                .manager
                .read_diagnostic(&id(), &ticket, 777)
                .is_none()
        );
        // Every app reloaded by one `riwork reload` shares the request id, so
        // a second app's replacement leaves a file of its own, and the first's
        // is neither replaced nor removed by it.
        let second = fixture
            .manager
            .write_diagnostic(&sample_diagnostic(&request, &other_ticket, 888))
            .unwrap();
        assert_ne!(second, path);
        assert_eq!(
            fixture
                .manager
                .read_diagnostic(&request, &other_ticket, 888)
                .unwrap()
                .0,
            second
        );
        fixture.manager.remove_diagnostic(&request, &other_ticket);
        assert!(!second.exists());
        assert!(
            fixture
                .manager
                .read_diagnostic(&request, &ticket, 777)
                .is_some()
        );
        // Paths are built only from valid ids.
        assert!(
            fixture
                .manager
                .diagnostic_path("../escape", &ticket)
                .is_err()
        );
        assert!(
            fixture
                .manager
                .diagnostic_path(&request, "../escape")
                .is_err()
        );
        assert!(
            fixture
                .manager
                .read_diagnostic("../escape", &ticket, 777)
                .is_none()
        );
        // A second write replaces the first.
        let mut newer = sample_diagnostic(&request, &ticket, 777);
        newer.attached = 7;
        fixture.manager.write_diagnostic(&newer).unwrap();
        assert_eq!(
            fixture
                .manager
                .read_diagnostic(&request, &ticket, 777)
                .unwrap()
                .1
                .attached,
            7
        );
        #[cfg(unix)]
        {
            // A symlink planted in its place is not followed.
            fs::remove_file(&path).unwrap();
            std::os::unix::fs::symlink(fixture.path.join("elsewhere"), &path).unwrap();
            assert!(
                fixture
                    .manager
                    .read_diagnostic(&request, &ticket, 777)
                    .is_none()
            );
        }
    }

    #[test]
    fn diagnostics_outlive_other_leftovers_but_not_a_week() {
        let fixture = Fixture::new();
        let dir = fixture.manager.directory.join("diagnostics");
        assert!(dir.is_dir());
        let name = |suffix: &str| dir.join(format!("reload-{}{suffix}", Uuid::new_v4()));
        let (recent, stale, temporary) = (name(".txt"), name(".txt"), name(".tmp"));
        for path in [&recent, &stale, &temporary] {
            fs::write(path, "x").unwrap();
        }
        set_age(&recent, LEFTOVER_AGE * 2);
        set_age(&stale, DIAGNOSTIC_AGE + Duration::from_secs(60));
        set_age(&temporary, DIAGNOSTIC_AGE + Duration::from_secs(60));
        fixture
            .manager
            .reload_all(&env::current_exe().unwrap())
            .unwrap();
        assert!(recent.exists());
        assert!(!stale.exists());
        assert!(!temporary.exists());
    }

    #[test]
    fn breadcrumbs_record_the_session_cwd_and_step_of_each_attach() {
        let progress = RestoreProgress::new();
        assert_eq!(*locked(&progress.last), None);
        progress.attach_started("session-1", Path::new("/work/one"));
        progress.attach_finished("session-1", true);
        progress.attach_started("session-2", Path::new("/work/two"));
        assert_eq!(
            locked(&progress.last).clone(),
            Some(Breadcrumb {
                session_id: "session-2".to_owned(),
                cwd: "/work/two".to_owned(),
                step: AttachStep::Start,
            })
        );
        assert_eq!(progress.attached.load(Ordering::Acquire), 1);
        progress.attach_finished("session-2", false);
        assert_eq!(
            locked(&progress.last).as_ref().map(|b| b.step),
            Some(AttachStep::Failed)
        );
        assert_eq!(progress.attached.load(Ordering::Acquire), 1);
        // A late report for another session does not rewrite the current one.
        progress.attach_started("session-3", Path::new("/work/three"));
        progress.attach_finished("session-2", true);
        assert_eq!(
            locked(&progress.last).as_ref().map(|b| b.step),
            Some(AttachStep::Start)
        );
        // Once the restore is over, recording stops.
        let revision = progress.revision.load(Ordering::Acquire);
        progress.stop();
        progress.attach_started("session-4", Path::new("/work/four"));
        assert_eq!(progress.revision.load(Ordering::Acquire), revision);
        assert_eq!(
            locked(&progress.last)
                .as_ref()
                .map(|b| b.session_id.as_str()),
            Some("session-3")
        );
        // Without a watchdog these are no-ops.
        note_attach_started("x", Path::new("/"));
        note_attach_finished("x", true);
    }

    #[test]
    fn the_failure_message_names_where_the_replacement_hung() {
        let request = Uuid::new_v4().to_string();
        let mut diagnostic = sample_diagnostic(&request, &Uuid::new_v4().to_string(), 777);
        let path = Path::new("/runtime/diagnostics/reload-x.txt");
        let stopped = timeout_message(777, Some(&StopOutcome::Killed), Some((path, &diagnostic)));
        assert_eq!(
            stopped,
            "Replacement RiWork did not confirm restoration within 25 seconds: \
             stuck attaching terminal for session 0f9c3c1e-session (cwd /Users/me/work tree/α); \
             see /runtime/diagnostics/reload-x.txt. \
             The unresponsive replacement (pid 777) was stopped. Existing windows remain open."
        );
        // Without a diagnostic there is nothing to point at.
        assert_eq!(
            timeout_message(777, Some(&StopOutcome::Terminated), None),
            "Replacement RiWork did not confirm restoration within 25 seconds. \
             The unresponsive replacement (pid 777) was stopped. Existing windows remain open."
        );
        // Without an outcome (the registry was busy) nothing is claimed.
        assert_eq!(
            timeout_message(777, None, None),
            "Replacement RiWork did not confirm restoration within 25 seconds. Existing windows remain open."
        );
        diagnostic.last.as_mut().unwrap().step = AttachStep::Done;
        assert!(
            diagnostic
                .stuck_at()
                .starts_with("no progress after attaching terminal")
        );
        diagnostic.last.as_mut().unwrap().step = AttachStep::Failed;
        assert!(diagnostic.stuck_at().contains("failed attach"));
        for (outcome, expected) in [
            (StopOutcome::AlreadyGone, "had already exited"),
            (StopOutcome::Unverified, "could not be identified"),
            (StopOutcome::Refused, "could not be signalled safely"),
            (StopOutcome::Survived, "did not exit after SIGKILL"),
        ] {
            assert!(
                timeout_message(9, Some(&outcome), None).contains(expected),
                "{outcome:?}"
            );
            assert!(!timeout_message(9, Some(&outcome), None).contains("was stopped"));
        }
    }

    // -- The replacement's watchdog -----------------------------------------

    struct WatchFixture {
        fixture: Fixture,
        registration: RuntimeRegistration,
        request_id: String,
        ticket_id: String,
        progress: Arc<RestoreProgress>,
        write: DiagnosticWriter,
    }
    impl WatchFixture {
        fn new() -> Self {
            let fixture = Fixture::new();
            let registration = fixture.register("replacement");
            Self {
                fixture,
                registration,
                request_id: Uuid::new_v4().to_string(),
                ticket_id: Uuid::new_v4().to_string(),
                progress: Arc::new(RestoreProgress::new()),
                write: RuntimeManager::write_diagnostic,
            }
        }
        fn context(&self) -> WatchdogContext {
            WatchdogContext {
                manager: self.fixture.manager.clone(),
                instance_id: self.registration.instance.id.clone(),
                request_id: self.request_id.clone(),
                ticket_id: self.ticket_id.clone(),
                pid: std::process::id(),
                executable: env::current_exe().unwrap(),
                progress: self.progress.clone(),
                write: self.write,
            }
        }
        fn diagnostic_file(&self) -> PathBuf {
            self.fixture
                .manager
                .diagnostic_path(&self.request_id, &self.ticket_id)
                .unwrap()
        }
        fn read_diagnostic(&self) -> Option<ReloadDiagnostic> {
            self.fixture
                .manager
                .read_diagnostic(&self.request_id, &self.ticket_id, std::process::id())
                .map(|(_, diagnostic)| diagnostic)
        }
        /// A watchdog that reports what it would have done instead of exiting.
        fn watchdog(
            &self,
            deadline: Duration,
            confirm_grace: Duration,
        ) -> (
            RestoreWatchdog,
            mpsc::Receiver<(ReloadDiagnostic, Option<PathBuf>)>,
        ) {
            let (fired, receiver) = mpsc::channel();
            let watchdog = RestoreWatchdog::spawn(
                self.context(),
                WatchdogConfig {
                    deadline,
                    confirm_grace,
                    mirror_interval: Duration::from_millis(10),
                },
                move |context, diagnostic| {
                    let written = record_watchdog_timeout(
                        &context.manager,
                        &context.instance_id,
                        &diagnostic,
                    );
                    let _ = fired.send((diagnostic, written.ok()));
                },
            );
            (watchdog, receiver)
        }
    }

    #[test]
    #[ignore = "slow: wall-clock watchdog deadline"]
    fn a_watchdog_fires_after_its_deadline_and_leaves_the_last_breadcrumb() {
        let watch = WatchFixture::new();
        let registration = watch
            .fixture
            .manager
            .instance_path(&watch.registration.instance.id)
            .unwrap();
        assert!(registration.exists());
        watch
            .progress
            .attach_started("session-1", Path::new("/work/one"));
        watch.progress.attach_finished("session-1", true);
        watch
            .progress
            .attach_started("session-2", Path::new("/work/two"));
        let started = Instant::now();
        let (_watchdog, fired) = watch.watchdog(Duration::from_millis(250), Duration::from_secs(5));
        let (diagnostic, written) = fired.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(started.elapsed() >= Duration::from_millis(250));
        assert_eq!(diagnostic.status, DiagnosticStatus::WatchdogTimeout);
        assert!(diagnostic.elapsed >= Duration::from_millis(250));
        assert_eq!(diagnostic.attached, 1);
        assert_eq!(
            diagnostic.last,
            Some(Breadcrumb {
                session_id: "session-2".to_owned(),
                cwd: "/work/two".to_owned(),
                step: AttachStep::Start,
            })
        );
        // The file is in the runtime directory and says the same.
        let written = written.unwrap();
        assert_eq!(written, watch.diagnostic_file());
        let on_disk = watch.read_diagnostic().unwrap();
        assert_eq!(on_disk.status, DiagnosticStatus::WatchdogTimeout);
        assert_eq!(on_disk.last, diagnostic.last);
        assert_eq!(on_disk.attached, 1);
        assert_eq!(on_disk.executable, env::current_exe().unwrap());
        assert!(
            fs::read_to_string(&written)
                .unwrap()
                .contains("session: session-2")
        );
        // The registration is gone, and recording stopped.
        assert!(!registration.exists());
        assert!(!watch.progress.active.load(Ordering::Acquire));
        // It fires once.
        assert!(fired.recv_timeout(Duration::from_millis(300)).is_err());
    }

    #[test]
    fn a_watchdog_mirrors_progress_to_disk_and_cleans_up_when_confirmed() {
        let watch = WatchFixture::new();
        let (watchdog, fired) = watch.watchdog(Duration::from_secs(30), Duration::from_secs(5));
        let read = || watch.read_diagnostic();
        let wait_for = |condition: &dyn Fn(&ReloadDiagnostic) -> bool| {
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                if let Some(diagnostic) = read().filter(|d| condition(d)) {
                    return diagnostic;
                }
                thread::sleep(Duration::from_millis(10));
            }
            panic!("the diagnostic never reached the expected state");
        };
        // Even before any terminal: a replacement that hangs early is describable.
        let initial = wait_for(&|_| true);
        assert_eq!(initial.status, DiagnosticStatus::InProgress);
        assert_eq!(initial.last, None);
        watch
            .progress
            .attach_started("session-9", Path::new("/work/nine"));
        let started = wait_for(&|d| d.last.is_some());
        assert_eq!(
            started.stuck_at(),
            "stuck attaching terminal for session session-9 (cwd /work/nine)"
        );
        watch.progress.attach_finished("session-9", true);
        let done = wait_for(&|d| d.attached == 1);
        assert_eq!(done.last.unwrap().step, AttachStep::Done);
        // A successful restore leaves nothing behind.
        assert!(watchdog.begin_confirm());
        watchdog.finish_confirm();
        let deadline = Instant::now() + Duration::from_secs(10);
        while watch.diagnostic_file().exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!watch.diagnostic_file().exists());
        assert!(fired.recv_timeout(Duration::from_millis(200)).is_err());
    }

    #[test]
    #[ignore = "slow: wall-clock watchdog deadlines"]
    fn a_confirmation_in_flight_holds_the_watchdog_off_for_a_bounded_time() {
        let watch = WatchFixture::new();
        // Confirmation begins in time and outlasts the deadline: not fired...
        let (watchdog, fired) =
            watch.watchdog(Duration::from_millis(150), Duration::from_millis(900));
        assert!(watchdog.begin_confirm());
        assert!(fired.recv_timeout(Duration::from_millis(500)).is_err());
        watchdog.finish_confirm();
        assert!(fired.recv_timeout(Duration::from_millis(700)).is_err());
        // ...but one that never finishes cannot hold a wedged process forever.
        let stuck = WatchFixture::new();
        let (watchdog, fired) =
            stuck.watchdog(Duration::from_millis(100), Duration::from_millis(300));
        assert!(watchdog.begin_confirm());
        let started = Instant::now();
        fired.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(started.elapsed() >= Duration::from_millis(400));
        // A failed confirmation re-arms the ordinary deadline.
        let failed = WatchFixture::new();
        let (watchdog, fired) =
            failed.watchdog(Duration::from_millis(300), Duration::from_secs(30));
        assert!(watchdog.begin_confirm());
        watchdog.abort_confirm();
        assert_eq!(*locked(&watchdog.shared.state), WatchState::Pending);
        fired.recv_timeout(Duration::from_secs(10)).unwrap();
    }

    /// A diagnostic writer that never returns, as on a filesystem that stalls.
    fn stalled_writer(_: &RuntimeManager, _: &ReloadDiagnostic) -> Result<PathBuf, String> {
        loop {
            thread::sleep(Duration::from_secs(3600));
        }
    }

    #[test]
    #[ignore = "slow: wall-clock watchdog deadline"]
    fn a_stalled_filesystem_cannot_hold_the_watchdog_up() {
        let mut watch = WatchFixture::new();
        watch.write = stalled_writer;
        // The breadcrumb thread wedges in its first write...
        watch
            .progress
            .attach_started("session-1", Path::new("/work/one"));
        let started = Instant::now();
        let (_watchdog, fired) = watch.watchdog(Duration::from_millis(300), Duration::from_secs(5));
        // ...and the timer fires on time regardless.
        let (diagnostic, written) = fired.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(started.elapsed() >= Duration::from_millis(300));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(diagnostic.status, DiagnosticStatus::WatchdogTimeout);
        assert_eq!(written.unwrap(), watch.diagnostic_file());
    }

    #[test]
    fn a_watchdog_that_fired_refuses_to_let_the_restore_confirm() {
        let watch = WatchFixture::new();
        let (watchdog, fired) = watch.watchdog(Duration::from_millis(50), Duration::from_secs(5));
        fired.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(!watchdog.begin_confirm());
        watchdog.finish_confirm();
        assert_eq!(*locked(&watchdog.shared.state), WatchState::Fired);
        watchdog.abort_confirm();
        assert_eq!(*locked(&watchdog.shared.state), WatchState::Fired);
    }

    #[test]
    #[ignore = "slow: 40 timed races against the watchdog"]
    fn confirming_and_firing_never_both_win() {
        // Many races between the two at nearly the same instant: whichever gets
        // there first, exactly one outcome is observed.
        for round in 0..40 {
            let watch = WatchFixture::new();
            let (watchdog, fired) =
                watch.watchdog(Duration::from_millis(20), Duration::from_secs(5));
            thread::sleep(Duration::from_millis(15 + round % 10));
            let confirmed = watchdog.begin_confirm();
            if confirmed {
                watchdog.finish_confirm();
                assert!(
                    fired.recv_timeout(Duration::from_millis(80)).is_err(),
                    "round {round}: fired after a confirmation began in time"
                );
            } else {
                assert!(
                    fired.recv_timeout(Duration::from_secs(10)).is_ok(),
                    "round {round}: refused to confirm but never fired"
                );
            }
        }
    }

    #[test]
    fn cleanup_that_hangs_cannot_hold_the_exit_up() {
        let started = Instant::now();
        assert!(!run_bounded(Duration::from_millis(100), || {
            thread::sleep(Duration::from_secs(3))
        }));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(run_bounded(Duration::from_secs(5), || {}));
    }

    // -- The production watchdog, in a process of its own -------------------

    const WATCHDOG_CHILD: &str = "RIWORK_TEST_WATCHDOG_CHILD";

    /// Runs only when the test below re-executes this binary with
    /// `RIWORK_TEST_WATCHDOG_CHILD` set to a runtime directory; an ordinary test
    /// run finds the variable unset and passes at once. It plays a replacement
    /// whose main thread is wedged: it registers, starts a child of its own,
    /// records a breadcrumb, arms the real watchdog and then blocks forever.
    #[cfg(unix)]
    #[test]
    // The helper is left for the watchdog's group signal to end; that it does
    // is what the test above asserts.
    #[allow(clippy::zombie_processes)]
    fn watchdog_child_entry() {
        let Some(runtime) = env::var_os(WATCHDOG_CHILD).map(PathBuf::from) else {
            return;
        };
        let manager = RuntimeManager::at(runtime.clone()).unwrap();
        let registration = manager.register(runtime.join("state")).unwrap();
        let helper = Command::new("sleep")
            .arg("60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        fs::write(runtime.join("helper.pid"), helper.id().to_string()).unwrap();
        let progress = Arc::new(RestoreProgress::new());
        progress.attach_started("wedged-session", Path::new("/wedged/cwd"));
        let _watchdog = RestoreWatchdog::spawn(
            WatchdogContext {
                manager,
                instance_id: registration.instance.id.clone(),
                request_id: env::var("RIWORK_TEST_WATCHDOG_REQUEST").unwrap(),
                ticket_id: env::var("RIWORK_TEST_WATCHDOG_TICKET").unwrap(),
                pid: std::process::id(),
                executable: env::current_exe().unwrap(),
                progress,
                write: RuntimeManager::write_diagnostic,
            },
            WatchdogConfig {
                deadline: Duration::from_millis(400),
                confirm_grace: Duration::from_secs(5),
                mirror_interval: Duration::from_millis(20),
            },
            exit_after_restore_timeout,
        );
        loop {
            thread::sleep(Duration::from_secs(3600));
        }
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "slow: re-executes the test binary as a wedged replacement"]
    fn a_wedged_replacement_records_where_it_hung_unregisters_and_exits_with_its_group() {
        use std::os::unix::process::{CommandExt, ExitStatusExt};
        let fixture = Fixture::new();
        let runtime = fixture.path.join("child-runtime");
        let request = Uuid::new_v4().to_string();
        let ticket = Uuid::new_v4().to_string();
        // Its own group, as a replacement launched by a GUI has; the watchdog
        // signals only a group it leads.
        let mut child = Command::new(env::current_exe().unwrap())
            .args([
                "--exact",
                "runtime::tests::watchdog_child_entry",
                "--test-threads=1",
            ])
            .env(WATCHDOG_CHILD, &runtime)
            .env("RIWORK_TEST_WATCHDOG_REQUEST", &request)
            .env("RIWORK_TEST_WATCHDOG_TICKET", &ticket)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                unsafe { libc::killpg(child.id() as libc::pid_t, libc::SIGKILL) };
                let _ = child.wait();
                panic!("the wedged replacement did not end itself");
            }
            thread::sleep(Duration::from_millis(50));
        };
        assert_eq!(status.signal(), None, "it must exit, not be killed");
        assert_eq!(status.code(), Some(RESTORE_WATCHDOG_EXIT_CODE));
        // Where it hung.
        let manager = RuntimeManager::at(runtime.clone()).unwrap();
        let (path, diagnostic) = manager
            .read_diagnostic(&request, &ticket, child.id())
            .unwrap();
        assert_eq!(
            path,
            manager
                .directory
                .join("diagnostics")
                .join(format!("reload-{request}-{ticket}.txt"))
        );
        assert_eq!(diagnostic.status, DiagnosticStatus::WatchdogTimeout);
        assert_eq!(diagnostic.attached, 0);
        assert_eq!(
            diagnostic.stuck_at(),
            "stuck attaching terminal for session wedged-session (cwd /wedged/cwd)"
        );
        assert!(diagnostic.elapsed >= Duration::from_millis(400));
        assert_eq!(diagnostic.executable, env::current_exe().unwrap());
        // Unregistered, so nothing lists a process with no windows.
        assert_eq!(
            fs::read_dir(manager.directory.join("instances"))
                .unwrap()
                .count(),
            0
        );
        // What it had started in its group went with it.
        let helper: i32 = fs::read_to_string(runtime.join("helper.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while unsafe { libc::kill(helper, 0) } == 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        let survived = unsafe { libc::kill(helper, 0) } == 0;
        if survived {
            unsafe { libc::kill(helper, libc::SIGKILL) };
        }
        assert!(!survived, "the replacement's helper outlived it");
    }
}
