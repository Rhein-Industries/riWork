//! Local, bounded control mailbox shared by coding shells and running preview tabs.
//!
//! The GUI claims commands exactly once. Saved tabs never enqueue launch commands;
//! an explicit `preview open` is the only operation here that starts an app.

use crate::app_preview::{PreviewPhase, PreviewSpec, PreviewState};
use fs2::FileExt;
use riwork_core::app_preview::PreviewInput;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const MAX_JSON: u64 = 512 * 1024;
const MAX_FRAME: u64 = 16 * 1024 * 1024;
const HOST_LEASE: u64 = 15;
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
const REQUEST_LEASE: u64 = 30;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ControlError {
    pub code: String,
    pub message: String,
}
impl ControlError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}
impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for ControlError {}
type Result<T> = std::result::Result<T, ControlError>;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum PreviewAction {
    Open {
        project_id: String,
        spec: PreviewSpec,
        #[serde(default)]
        background: bool,
    },
    Close {
        preview_id: String,
    },
    Input {
        preview_id: String,
        input: PreviewInput,
    },
    /// Wake a background preview briefly so its screenshot reflects the latest state.
    Refresh {
        preview_id: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PreviewRequest {
    pub id: String,
    pub host_id: String,
    pub project_id: String,
    pub action: PreviewAction,
    pub expires_at: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PreviewReply {
    pub preview_id: String,
    pub reused: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct FrameInfo {
    pub width: u32,
    pub height: u32,
    pub sequence: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PreviewStatus {
    pub v: u32,
    #[serde(default)]
    pub revision: u64,
    pub preview_id: String,
    pub project_id: String,
    pub spec: PreviewSpec,
    pub phase: String,
    pub ready: bool,
    pub message: String,
    pub warning: Option<String>,
    pub pid: Option<u32>,
    pub window_id: Option<u64>,
    pub frame: Option<FrameInfo>,
    pub logs: Vec<String>,
    pub ios: Option<crate::app_preview::ios::IosPreviewStatus>,
    pub owner_pid: u32,
    pub owner_host_id: String,
    pub updated_at: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Host {
    id: String,
    project_id: String,
    pid: u32,
    updated_at: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Owner {
    host_id: String,
}

#[derive(Clone, Debug)]
pub struct PreviewControl {
    directory: PathBuf,
    /// Last successful lease renewal, shared by clones of one window's control.
    heartbeat: Arc<Mutex<Option<(Host, Instant)>>>,
}

impl PreviewControl {
    pub fn open(home: &Path) -> Result<Self> {
        let directory = home.join("preview-control");
        for path in [
            &directory,
            &directory.join("hosts"),
            &directory.join("owners"),
            &directory.join("requests"),
            &directory.join("claimed"),
            &directory.join("responses"),
            &directory.join("statuses"),
            &directory.join("frames"),
        ] {
            crate::paths::create_private_dir(path).map_err(io_error)?;
        }
        Ok(Self {
            directory,
            heartbeat: Arc::default(),
        })
    }

    /// Call at least every five seconds while this project window is open.
    pub fn heartbeat(&self, host_id: &str, project_id: &str) -> Result<()> {
        self.heartbeat_at(host_id, project_id, Instant::now(), now())
    }

    fn heartbeat_at(
        &self,
        host_id: &str,
        project_id: &str,
        monotonic: Instant,
        wall: u64,
    ) -> Result<()> {
        valid_id(host_id)?;
        valid_id(project_id)?;
        let mut written = self
            .heartbeat
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if written.as_ref().is_some_and(|(host, at)| {
            host.id == host_id
                && host.project_id == project_id
                && monotonic.saturating_duration_since(*at) < HEARTBEAT_INTERVAL
        }) {
            return Ok(());
        }
        let host = Host {
            id: host_id.into(),
            project_id: project_id.into(),
            pid: std::process::id(),
            updated_at: wall,
        };
        self.write(&self.path("hosts", host_id), &host)?;
        *written = Some((host, monotonic));
        Ok(())
    }

    pub fn remove_host(&self, host_id: &str) -> Result<()> {
        valid_id(host_id)?;
        let mut written = self
            .heartbeat
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *written = None;
        remove_optional(&self.path("hosts", host_id))
    }

    /// Claim before touching UI state, so another project window cannot run it twice.
    pub fn take_requests(&self, host_id: &str, project_id: &str) -> Result<Vec<PreviewRequest>> {
        self.heartbeat(host_id, project_id)?;
        // Most windows have no commands. A request arriving after this check is
        // seen on the next 250 ms poll. Claiming still re-reads under the lock,
        // so two windows cannot execute the same request.
        if self.entries("requests")?.is_empty() {
            return Ok(Vec::new());
        }
        let _lock = self.lock()?;
        let mut requests = vec![];
        for path in self.entries("requests")? {
            let request: PreviewRequest = match self.read(&path) {
                Ok(request) => request,
                Err(_) => {
                    let _ = fs::remove_file(path);
                    continue;
                }
            };
            if request.expires_at < now() {
                let _ = fs::remove_file(path);
                continue;
            }
            if request.project_id != project_id || request.host_id != host_id {
                continue;
            }
            if valid_id(&request.id).is_err() || self.path("requests", &request.id) != path {
                continue;
            }
            fs::rename(&path, self.path("claimed", &request.id)).map_err(io_error)?;
            requests.push(request);
            if requests.len() == 32 {
                break;
            }
        }
        Ok(requests)
    }

    pub fn respond(
        &self,
        request_id: &str,
        reply: std::result::Result<PreviewReply, ControlError>,
    ) -> Result<()> {
        valid_id(request_id)?;
        self.write(&self.path("responses", request_id), &reply)?;
        remove_optional(&self.path("claimed", request_id))
    }

    /// Enqueue only for an existing, responsive GUI; never launch a GUI or app itself.
    pub fn request(&self, action: PreviewAction, timeout: Duration) -> Result<PreviewReply> {
        if let PreviewAction::Input { input, .. } = &action {
            crate::app_preview::validate_input(input)
                .map_err(|message| ControlError::new("invalid_request", message))?;
        }
        let project_id = match &action {
            PreviewAction::Open { project_id, .. } => {
                valid_id(project_id)?;
                project_id.clone()
            }
            PreviewAction::Close { preview_id }
            | PreviewAction::Input { preview_id, .. }
            | PreviewAction::Refresh { preview_id } => self.status(preview_id)?.project_id,
        };
        let mut hosts = self.hosts(&project_id)?;
        hosts.sort_by(|a, b| a.id.cmp(&b.id));
        let host_id = match &action {
            PreviewAction::Open { spec, .. } => self
                .entries("statuses")?
                .into_iter()
                .filter_map(|path| self.read::<PreviewStatus>(&path).ok())
                .filter(|status| {
                    status.project_id == project_id
                        && status.spec == *spec
                        && status.phase != "closed"
                })
                .find_map(|status| {
                    hosts
                        .iter()
                        .find(|host| host.id == status.owner_host_id)
                        .map(|host| host.id.clone())
                })
                .unwrap_or_else(|| hosts[0].id.clone()),
            PreviewAction::Close { preview_id }
            | PreviewAction::Input { preview_id, .. }
            | PreviewAction::Refresh { preview_id } => {
                let status = self.status(preview_id)?;
                hosts
                    .iter()
                    .find(|host| host.id == status.owner_host_id)
                    .map(|host| host.id.clone())
                    .ok_or_else(|| {
                        ControlError::new(
                            "host_unavailable",
                            "The window that owns this preview is no longer responding.",
                        )
                    })?
            }
        };
        let request = PreviewRequest {
            id: Uuid::new_v4().to_string(),
            host_id,
            project_id,
            action,
            expires_at: now() + REQUEST_LEASE,
        };
        let bytes = serde_json::to_vec(&request).map_err(io_error)?;
        if bytes.len() as u64 > MAX_JSON {
            return Err(ControlError::new(
                "invalid_request",
                "Preview request exceeds 512 KiB.",
            ));
        }
        self.write(&self.path("requests", &request.id), &request)?;
        let started = Instant::now();
        loop {
            let response_path = self.path("responses", &request.id);
            if response_path.exists() {
                let reply: std::result::Result<PreviewReply, ControlError> =
                    self.read(&response_path)?;
                let _ = fs::remove_file(response_path);
                return reply;
            }
            if started.elapsed() >= timeout {
                // A queued command is cancellable; a claimed one may already have run.
                let queued = fs::remove_file(self.path("requests", &request.id)).is_ok();
                return Err(ControlError::new(
                    "timeout",
                    if queued {
                        "RiWork did not accept the preview command before the timeout; the queued command was cancelled."
                    } else {
                        "RiWork accepted the preview command but did not confirm it before the timeout; inspect preview status before retrying."
                    },
                ));
            }
            thread::sleep(Duration::from_millis(40));
        }
    }

    /// Start an explicit tab lifetime, including an idle tab restored from layout.
    /// Call once on creation, before scheduling asynchronous publish work.
    pub fn begin(&self, preview_id: &str, host_id: &str) -> Result<()> {
        valid_id(preview_id)?;
        valid_id(host_id)?;
        let _lock = self.lock()?;
        if let Ok(previous) = self.read::<Owner>(&self.path("owners", preview_id)) {
            if previous.host_id != host_id {
                let previous_status = self
                    .read::<PreviewStatus>(&self.path("statuses", preview_id))
                    .ok();
                let host = self
                    .read::<Host>(&self.path("hosts", &previous.host_id))
                    .ok();
                if host.is_some_and(|host| {
                    now().saturating_sub(host.updated_at) <= HOST_LEASE && alive(host.pid)
                }) && previous_status.is_none_or(|status| status.phase != "closed")
                {
                    return Err(ControlError::new(
                        "owner_conflict",
                        "Another RiWork window owns this preview ID; create a new preview ID.",
                    ));
                }
            }
        }
        self.write(
            &self.path("owners", preview_id),
            &Owner {
                host_id: host_id.into(),
            },
        )?;
        remove_optional(&self.path("statuses", preview_id))?;
        remove_optional(&self.frame_path(preview_id))
    }

    /// Invalidate old readiness before acknowledging an explicit run.
    pub fn begin_run(
        &self,
        preview_id: &str,
        host_id: &str,
        project_id: &str,
        spec: &PreviewSpec,
        revision: u64,
    ) -> Result<()> {
        valid_id(preview_id)?;
        valid_id(host_id)?;
        valid_id(project_id)?;
        let _lock = self.lock()?;
        self.require_owner(preview_id, host_id)?;
        if let Ok(previous) = self.read::<PreviewStatus>(&self.path("statuses", preview_id)) {
            if previous.phase == "closed" || previous.revision > revision {
                return Err(ControlError::new(
                    "stale_revision",
                    "The preview run changed before this launch could begin.",
                ));
            }
        }
        let status = PreviewStatus {
            v: 1,
            revision,
            preview_id: preview_id.into(),
            project_id: project_id.into(),
            spec: spec.clone(),
            phase: "starting".into(),
            ready: false,
            message: "Starting preview…".into(),
            warning: None,
            pid: None,
            window_id: None,
            frame: None,
            logs: vec![],
            ios: None,
            owner_pid: std::process::id(),
            owner_host_id: host_id.into(),
            updated_at: now(),
        };
        remove_optional(&self.frame_path(preview_id))?;
        self.write(&self.path("statuses", preview_id), &status)
    }

    pub fn publish(
        &self,
        preview_id: &str,
        host_id: &str,
        project_id: &str,
        spec: &PreviewSpec,
        revision: u64,
        state: &PreviewState,
    ) -> Result<PreviewStatus> {
        valid_id(preview_id)?;
        valid_id(host_id)?;
        valid_id(project_id)?;
        let _lock = self.lock()?;
        self.require_owner(preview_id, host_id)?;
        let previous = self
            .read::<PreviewStatus>(&self.path("statuses", preview_id))
            .ok();
        // A background publisher already queued when the tab closed cannot revive it.
        if let Some(previous) = previous
            .as_ref()
            .filter(|status| status.phase == "closed" || status.revision > revision)
        {
            return Ok(previous.clone());
        }
        let frame = state.frame.as_ref().map(|f| FrameInfo {
            width: f.width,
            height: f.height,
            sequence: f.sequence,
        });
        if let Some(frame) = &state.frame {
            if frame.png.len() as u64 > MAX_FRAME {
                return Err(ControlError::new(
                    "frame_too_large",
                    "Preview frame exceeds 16 MiB.",
                ));
            }
            if previous
                .as_ref()
                .and_then(|s| s.frame.as_ref())
                .map(|f| f.sequence)
                != Some(frame.sequence)
                || previous.as_ref().is_none_or(|s| {
                    s.owner_pid != std::process::id()
                        || s.pid != state.pid
                        || s.revision != revision
                })
            {
                self.write_bytes(&self.frame_path(preview_id), &frame.png)?;
            }
        }
        let phase = match state.phase {
            PreviewPhase::Starting => "starting",
            PreviewPhase::WaitingForWindow => "waiting_for_window",
            PreviewPhase::Streaming => "streaming",
            PreviewPhase::Paused => "paused",
            PreviewPhase::Exited => "exited",
            PreviewPhase::Stopped => "stopped",
            PreviewPhase::Error => "error",
        };
        let mut status = PreviewStatus {
            v: 1,
            revision,
            preview_id: preview_id.into(),
            project_id: project_id.into(),
            spec: spec.clone(),
            phase: phase.into(),
            ready: frame.is_some()
                && matches!(state.phase, PreviewPhase::Streaming | PreviewPhase::Paused),
            message: state.message.clone(),
            warning: state.warning.clone(),
            pid: state.pid,
            window_id: state.window_id,
            frame,
            logs: bounded_logs(state),
            ios: state.ios.clone(),
            owner_pid: std::process::id(),
            owner_host_id: host_id.into(),
            updated_at: now(),
        };
        if let Some(previous) = previous.as_ref() {
            let mut comparable = serde_json::to_value(previous).map_err(io_error)?;
            comparable["updated_at"] = serde_json::json!(status.updated_at);
            if comparable == serde_json::to_value(&status).map_err(io_error)?
                && now().saturating_sub(previous.updated_at) < 5
            {
                status.updated_at = previous.updated_at;
                return Ok(status);
            }
        }
        self.write(&self.path("statuses", preview_id), &status)?;
        Ok(status)
    }

    pub fn closed(
        &self,
        preview_id: &str,
        host_id: &str,
        project_id: &str,
        spec: &PreviewSpec,
    ) -> Result<()> {
        valid_id(preview_id)?;
        valid_id(host_id)?;
        let _lock = self.lock()?;
        self.require_owner(preview_id, host_id)?;
        let revision = self
            .read::<PreviewStatus>(&self.path("statuses", preview_id))
            .map(|status| status.revision)
            .unwrap_or_default();
        let status = PreviewStatus {
            v: 1,
            revision,
            preview_id: preview_id.into(),
            project_id: project_id.into(),
            spec: spec.clone(),
            phase: "closed".into(),
            ready: false,
            message: "Preview tab closed.".into(),
            warning: None,
            pid: None,
            window_id: None,
            frame: None,
            logs: vec![],
            ios: None,
            owner_pid: std::process::id(),
            owner_host_id: host_id.into(),
            updated_at: now(),
        };
        self.write(&self.path("statuses", preview_id), &status)?;
        remove_optional(&self.frame_path(preview_id))
    }

    pub fn status(&self, preview_id: &str) -> Result<PreviewStatus> {
        valid_id(preview_id)?;
        let status: PreviewStatus = self.read(&self.path("statuses", preview_id)).map_err(|e| {
            if e.code == "not_found" {
                ControlError::new("not_found", format!("Unknown preview {preview_id}."))
            } else {
                e
            }
        })?;
        if status.phase != "closed" {
            if !self
                .hosts(&status.project_id)?
                .iter()
                .any(|host| host.id == status.owner_host_id)
            {
                return Err(ControlError::new(
                    "host_unavailable",
                    "The window that owns this preview is no longer responding.",
                ));
            }
            if now().saturating_sub(status.updated_at) > HOST_LEASE || !alive(status.owner_pid) {
                return Err(ControlError::new(
                    "host_unavailable",
                    "The GUI that owns this preview is no longer responding.",
                ));
            }
        }
        Ok(status)
    }

    pub fn statuses(&self) -> Result<Vec<PreviewStatus>> {
        let mut items = vec![];
        for path in self.entries("statuses")? {
            if let Some(id) = path.file_stem().and_then(|id| id.to_str()) {
                if let Ok(status) = self.status(id) {
                    items.push(status);
                }
            }
        }
        items.sort_by(|a, b| a.preview_id.cmp(&b.preview_id));
        Ok(items)
    }

    pub fn screenshot(&self, preview_id: &str) -> Result<(PreviewStatus, Vec<u8>)> {
        let status = self.status(preview_id)?;
        if status.frame.is_none() {
            return Err(ControlError::new(
                "not_ready",
                "Preview has not produced a frame yet.",
            ));
        }
        let png = read_bytes(&self.frame_path(preview_id), MAX_FRAME)?;
        Ok((status, png))
    }

    pub fn wait_frame(
        &self,
        preview_id: &str,
        after: u64,
        timeout: Duration,
    ) -> Result<(PreviewStatus, Vec<u8>)> {
        let started = Instant::now();
        loop {
            let status = self.status(preview_id)?;
            if status
                .frame
                .as_ref()
                .is_some_and(|frame| frame.sequence > after)
            {
                return self.screenshot(preview_id);
            }
            if matches!(
                status.phase.as_str(),
                "error" | "exited" | "stopped" | "closed"
            ) {
                return Err(ControlError::new("preview_failed", status.message));
            }
            if started.elapsed() >= timeout {
                return Err(ControlError::new(
                    "timeout",
                    "Preview did not produce a fresh frame before the timeout.",
                ));
            }
            thread::sleep(Duration::from_millis(40));
        }
    }

    pub fn wait_ready(&self, preview_id: &str, timeout: Duration) -> Result<PreviewStatus> {
        let started = Instant::now();
        loop {
            match self.status(preview_id) {
                Ok(status) if status.ready => return Ok(status),
                Ok(status)
                    if matches!(
                        status.phase.as_str(),
                        "error" | "exited" | "stopped" | "closed"
                    ) =>
                {
                    return Err(ControlError::new(
                        "preview_failed",
                        format!("Preview {preview_id}: {}", status.message),
                    ));
                }
                Err(error) if error.code != "not_found" => return Err(error),
                _ => {}
            }
            if started.elapsed() >= timeout {
                return Err(ControlError::new(
                    "timeout",
                    format!(
                        "Preview {preview_id} did not produce a frame before the timeout; it remains open. Inspect its status and build logs."
                    ),
                ));
            }
            thread::sleep(Duration::from_millis(80));
        }
    }

    fn hosts(&self, project_id: &str) -> Result<Vec<Host>> {
        let mut hosts = vec![];
        for path in self.entries("hosts")? {
            if let Ok(host) = self.read::<Host>(&path) {
                if host.project_id == project_id
                    && now().saturating_sub(host.updated_at) <= HOST_LEASE
                    && alive(host.pid)
                {
                    hosts.push(host);
                }
            }
        }
        if hosts.is_empty() {
            Err(ControlError::new(
                "host_unavailable",
                "Open this project in an updated RiWork GUI before controlling previews.",
            ))
        } else {
            Ok(hosts)
        }
    }
    fn require_owner(&self, preview_id: &str, host_id: &str) -> Result<()> {
        match self.read::<Owner>(&self.path("owners", preview_id)) {
            Ok(owner) if owner.host_id == host_id => Ok(()),
            _ => Err(ControlError::new(
                "owner_conflict",
                "Preview ownership changed; this stale publisher cannot update it.",
            )),
        }
    }
    fn path(&self, kind: &str, id: &str) -> PathBuf {
        self.directory.join(kind).join(format!("{id}.json"))
    }
    fn frame_path(&self, id: &str) -> PathBuf {
        self.directory.join("frames").join(format!("{id}.png"))
    }
    fn entries(&self, kind: &str) -> Result<Vec<PathBuf>> {
        Ok(fs::read_dir(self.directory.join(kind))
            .map_err(io_error)?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .take(4096)
            .collect())
    }
    fn read<T: DeserializeOwned>(&self, path: &Path) -> Result<T> {
        serde_json::from_slice(&read_bytes(path, MAX_JSON)?).map_err(io_error)
    }
    fn write<T: Serialize>(&self, path: &Path, value: &T) -> Result<()> {
        self.write_bytes(path, &serde_json::to_vec(value).map_err(io_error)?)
    }
    fn write_bytes(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        let tmp = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
        let result = (|| {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&tmp).map_err(io_error)?;
            file.write_all(bytes).map_err(io_error)?;
            fs::rename(&tmp, path).map_err(io_error)
        })();
        if result.is_err() {
            let _ = fs::remove_file(tmp);
        }
        result
    }
    fn lock(&self) -> Result<File> {
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(self.directory.join("control.lock"))
            .map_err(io_error)?;
        file.lock_exclusive().map_err(io_error)?;
        Ok(file)
    }
}

fn bounded_logs(state: &PreviewState) -> Vec<String> {
    let mut size = 0;
    let mut logs = state
        .logs
        .iter()
        .rev()
        .take(128)
        .map(|entry| entry.text.chars().take(2048).collect::<String>())
        .take_while(|line| {
            size += line.len();
            size <= 64 * 1024
        })
        .collect::<Vec<_>>();
    logs.reverse();
    logs
}
fn valid_id(id: &str) -> Result<()> {
    if Uuid::parse_str(id).is_ok_and(|uuid| uuid.to_string() == id) {
        Ok(())
    } else {
        Err(ControlError::new(
            "invalid_request",
            "Preview, project, host, and request IDs must be canonical UUIDs.",
        ))
    }
}
fn read_bytes(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let file = File::open(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ControlError::new("not_found", format!("{} does not exist.", path.display()))
        } else {
            io_error(e)
        }
    })?;
    if !file.metadata().map_err(io_error)?.is_file() {
        return Err(ControlError::new(
            "invalid_data",
            "Preview control entry is not a file.",
        ));
    }
    let mut bytes = vec![];
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() as u64 > limit {
        return Err(ControlError::new(
            "invalid_data",
            "Preview control entry exceeds its size limit.",
        ));
    }
    Ok(bytes)
}
fn remove_optional(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_error(e)),
    }
}
fn io_error(error: impl std::fmt::Display) -> ControlError {
    ControlError::new("control_error", error.to_string())
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        pid > 0 && unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        pid == std::process::id()
    }
}

#[cfg(test)]
mod tests;
