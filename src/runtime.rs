//! Cooperative GUI reloads. Desktop windows are replaced only after a new GUI
//! confirms restoration; terminal and harness processes are never signalled.

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
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const MAX_BYTES: u64 = 32 * 1024 * 1024;
const REQUEST_LIFETIME: u64 = 300;

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
        for name in ["instances", "requests", "restores", "responses"] {
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
        write_json(&self.directory.join("installed-build.json"), &installed)
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
        write_json(&self.instance_path(&instance.id)?, &instance)?;
        Ok(RuntimeRegistration {
            manager: self.clone(),
            instance,
        })
    }
    pub fn instances(&self) -> Result<Vec<RuntimeInstance>, String> {
        let _lock = self.lock()?;
        self.live_instances()
    }
    fn live_instances(&self) -> Result<Vec<RuntimeInstance>, String> {
        let mut live = vec![];
        for entry in fs::read_dir(self.directory.join("instances")).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let Ok(instance) = read_json::<RuntimeInstance>(&path) else {
                continue;
            };
            if self.instance_path(&instance.id)? != path {
                continue;
            }
            if process_matches(&instance)? {
                live.push(instance);
            } else {
                fs::remove_file(&path)
                    .map_err(|e| format!("Cannot remove stale GUI registration: {e}"))?;
                let _ = fs::remove_file(self.request_path(&instance.id)?);
            }
        }
        live.sort_by_key(|instance| instance.pid);
        Ok(live)
    }
    pub fn reload_all(&self, replacement: &Path) -> Result<ReloadReport, String> {
        let replacement = executable_path(replacement)?;
        let _lock = self.lock()?;
        let instances = self.live_instances()?;
        let request_id = Uuid::new_v4().to_string();
        let mut report = ReloadReport {
            request_id: request_id.clone(),
            replacement_executable: replacement.clone(),
            requested: 0,
            reloaded: 0,
            pending: 0,
            failed: 0,
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
        let _lock = self.lock()?;
        write_json(
            &self.response_path(&stored.old_instance_id, &stored.request_id)?,
            &response,
        )?;
        fs::remove_file(self.restore_path(&stored.ticket_id)?)
            .map_err(|e| format!("Cannot consume restore ticket: {e}"))?;
        Ok(())
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
    fn response_path(&self, instance: &str, request: &str) -> Result<PathBuf, String> {
        valid_id(instance)?;
        valid_id(request)?;
        Ok(self
            .directory
            .join("responses")
            .join(format!("{instance}-{request}.json")))
    }
    fn lock(&self) -> Result<File, String> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.directory.join("registry.lock"))
            .map_err(|e| e.to_string())?;
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
    pub fn publish_windows(&mut self, windows: Vec<RuntimeWindow>) -> Result<(), String> {
        validate_windows(&windows)?;
        self.instance.windows = windows;
        self.instance.updated_at = now();
        let _lock = self.manager.lock()?;
        write_json(
            &self.manager.instance_path(&self.instance.id)?,
            &self.instance,
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
        executable_path(&request.replacement_executable)?;
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
        let executable = executable_path(&request.replacement_executable)?;
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
        })
    }
    pub fn reload_ready(&self, launch: &ReloadLaunch) -> Result<bool, String> {
        let response = read_optional::<Response>(
            &self
                .manager
                .response_path(&self.instance.id, &launch.request.request_id)?,
        )?;
        if let Some(response) = response {
            if response.state == ReloadState::Failed {
                return Err(response.message);
            }
            if response.request_id == launch.request.request_id
                && response.instance_id == self.instance.id
                && response.state == ReloadState::Reloaded
                && response.new_instance.as_ref().is_some_and(|new| {
                    new.pid == launch.pid && process_matches(new).unwrap_or(false)
                })
            {
                return Ok(true);
            }
        }
        let failure = if launch.exited.load(Ordering::Acquire) {
            Some(
                "Replacement RiWork exited before restoring its windows. Existing windows remain open.",
            )
        } else if launch.started.elapsed() >= Duration::from_secs(25) {
            Some(
                "Replacement RiWork did not confirm restoration within 25 seconds. Existing windows remain open.",
            )
        } else {
            None
        };
        if let Some(message) = failure {
            self.fail_reload(&launch.request, message)?;
            return Err(message.to_owned());
        }
        Ok(false)
    }
    pub fn fail_reload(&self, request: &ReloadRequest, message: &str) -> Result<(), String> {
        if request.instance_id != self.instance.id {
            return Err("Reload request belongs to another GUI".to_owned());
        }
        let response = Response {
            request_id: request.request_id.clone(),
            instance_id: self.instance.id.clone(),
            state: ReloadState::Failed,
            new_instance: None,
            message: message.to_owned(),
        };
        let _lock = self.manager.lock()?;
        write_json(
            &self
                .manager
                .response_path(&self.instance.id, &request.request_id)?,
            &response,
        )?;
        let _ = fs::remove_file(self.manager.restore_path(&request.ticket_id)?);
        Ok(())
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
    let meta = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.len() > MAX_BYTES {
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
        .take(MAX_BYTES + 1)
        .read_to_end(&mut data)
        .map_err(|e| e.to_string())?;
    if data.len() as u64 > MAX_BYTES {
        return Err("RiWork runtime file is too large".to_owned());
    }
    serde_json::from_slice(&data).map_err(|e| format!("Invalid RiWork runtime JSON: {e}"))
}
fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let data = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    if data.len() as u64 > MAX_BYTES {
        return Err("RiWork runtime snapshot is too large".to_owned());
    }
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
        file.write_all(&data).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        fs::rename(&temporary, path).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

struct ProcessIdentity {
    pid: u32,
    uid: u32,
    started_token: String,
    executable: Option<PathBuf>,
}
fn process_matches(instance: &RuntimeInstance) -> Result<bool, String> {
    if instance.uid != current_uid() {
        return Ok(false);
    }
    Ok(process_identity(instance.pid)?.is_some_and(|identity| {
        identity.uid == instance.uid && identity.started_token == instance.started_token
    }))
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
fn process_identity(pid: u32) -> Result<Option<ProcessIdentity>, String> {
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
fn process_identity(pid: u32) -> Result<Option<ProcessIdentity>, String> {
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
fn process_identity(_pid: u32) -> Result<Option<ProcessIdentity>, String> {
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
}
