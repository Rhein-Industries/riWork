//! App previews own a development process and stream its frames into a tab.
//!
//! Cooperative previews use an authenticated local socket and create no desktop
//! window. The explicit capture fallback talks only to RiWork's guarded Cua MCP
//! server and binds one window of the process group that this launcher owns.

use std::{
    collections::VecDeque,
    io::Read,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

pub use riwork_core::app_preview::{
    IosPreviewSpec, PreviewInput, PreviewMode, PreviewMouseButton, PreviewSpec,
};

mod capture;
mod cooperative;
pub mod ios;

const LOG_BYTES: usize = 64 * 1024;
const LOG_ENTRIES: usize = 256;
const INPUT_QUEUE: usize = 128;
const TICK: Duration = Duration::from_millis(50);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewPhase {
    Starting,
    WaitingForWindow,
    Streaming,
    Paused,
    Exited,
    Stopped,
    Error,
}

#[derive(Clone, Debug)]
pub struct PreviewFrame {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub sequence: u64,
}

#[derive(Clone, Debug)]
pub struct PreviewLog {
    pub text: String,
    pub stderr: bool,
}

#[derive(Clone, Debug)]
pub struct PreviewWindow {
    pub pid: u32,
    pub window_id: u64,
    pub title: String,
}

#[derive(Clone, Debug)]
pub struct PreviewState {
    pub phase: PreviewPhase,
    pub mode: PreviewMode,
    pub pid: Option<u32>,
    pub window_id: Option<u64>,
    pub windows: Vec<PreviewWindow>,
    pub message: String,
    pub warning: Option<String>,
    pub frame: Option<Arc<PreviewFrame>>,
    pub logs: Vec<PreviewLog>,
    pub ios: Option<ios::IosPreviewStatus>,
}

impl PreviewState {
    fn new(mode: PreviewMode) -> Self {
        Self {
            phase: PreviewPhase::Starting,
            mode,
            pid: None,
            window_id: None,
            windows: Vec::new(),
            message: "Starting preview…".into(),
            warning: None,
            frame: None,
            logs: Vec::new(),
            ios: None,
        }
    }
}

/// Dropping the last UI handle stops only the process group it launched.
pub struct AppPreview {
    state: Arc<Mutex<PreviewState>>,
    control: mpsc::Sender<Control>,
    input: mpsc::SyncSender<PreviewInput>,
    cancellation: Arc<AtomicU64>,
    active_revision: Arc<AtomicU64>,
}

enum Control {
    Visible(bool),
    Stop(u64),
    Restart(u64),
    SelectWindow { pid: u32, window_id: u64 },
    Shutdown(u64),
}

impl AppPreview {
    /// Launch is explicit; opening or restoring a tab must not call this method.
    pub fn launch(spec: PreviewSpec) -> Result<Self, String> {
        if spec.mode != PreviewMode::IosSimulator && spec.command.trim().is_empty() {
            return Err("Enter the command that starts your development app.".into());
        }
        if spec.mode == PreviewMode::IosSimulator && spec.ios.is_none() {
            return Err("Configure the Xcode scheme for this iOS preview.".into());
        }
        let cwd = spec.cwd.canonicalize().map_err(|error| {
            format!(
                "Cannot resolve preview folder {}: {error}",
                spec.cwd.display()
            )
        })?;
        if !cwd.is_dir() {
            return Err("The preview working folder is not a directory.".into());
        }
        let spec = PreviewSpec { cwd, ..spec };
        let state = Arc::new(Mutex::new(PreviewState::new(spec.mode)));
        let (control, controls) = mpsc::channel();
        let (input, inputs) = mpsc::sync_channel(INPUT_QUEUE);
        let worker_state = Arc::clone(&state);
        let cancellation = Arc::new(AtomicU64::new(0));
        let worker_cancellation = Arc::clone(&cancellation);
        let active_revision = Arc::new(AtomicU64::new(0));
        let worker_revision = Arc::clone(&active_revision);
        thread::Builder::new()
            .name("riwork-app-preview".into())
            .spawn(move || {
                worker(
                    spec,
                    worker_state,
                    controls,
                    inputs,
                    worker_cancellation,
                    worker_revision,
                )
            })
            .map_err(|error| format!("Cannot start preview worker: {error}"))?;
        Ok(Self {
            state,
            control,
            input,
            cancellation,
            active_revision,
        })
    }

    pub fn snapshot(&self) -> PreviewState {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut snapshot = state.clone();
        if self.active_revision.load(Ordering::SeqCst) != self.cancellation.load(Ordering::SeqCst) {
            // An old transport may finish a capture while Restart is queued.
            // Never publish that frame as readiness for the new generation.
            snapshot.phase = PreviewPhase::Starting;
            snapshot.frame = None;
            snapshot.pid = None;
            snapshot.window_id = None;
        }
        snapshot
    }

    pub fn set_visible(&self, visible: bool) {
        let _ = self.control.send(Control::Visible(visible));
    }

    pub fn input(&self, input: PreviewInput) {
        if let Err(message) = validate_input(&input) {
            self.state.lock().unwrap_or_else(|e| e.into_inner()).warning = Some(message);
            return;
        }
        // A stalled app cannot grow the event queue or stall RiWork's UI.
        if let Err(mpsc::TrySendError::Full(_)) = self.input.try_send(input) {
            self.state.lock().unwrap_or_else(|e| e.into_inner()).warning =
                Some("Preview is busy; input queue is full.".into());
        }
    }

    pub fn stop(&self) {
        let revision = self.cancellation.fetch_add(1, Ordering::SeqCst) + 1;
        self.state.lock().unwrap_or_else(|e| e.into_inner()).message = "Stopping preview…".into();
        let _ = self.control.send(Control::Stop(revision));
    }

    pub fn restart(&self) {
        let revision = self.cancellation.fetch_add(1, Ordering::SeqCst) + 1;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.phase = PreviewPhase::Starting;
        state.frame = None;
        state.message = "Restarting preview…".into();
        drop(state);
        let _ = self.control.send(Control::Restart(revision));
    }

    pub fn select_window(&self, pid: u32, window_id: u64) {
        let _ = self.control.send(Control::SelectWindow { pid, window_id });
    }
}

pub fn validate_input(input: &PreviewInput) -> Result<(), String> {
    use riwork_core::app_preview::{MAX_PREVIEW_DIMENSION, MAX_PREVIEW_INPUT_MESSAGE_BYTES};
    match input {
        PreviewInput::Text { text } if text.len() > 60 * 1024 => {
            return Err("Preview text input is limited to 60 KiB per event.".into());
        }
        PreviewInput::Key { key } | PreviewInput::KeyUp { key } if key.len() > 128 => {
            return Err("Preview key name is too long.".into());
        }
        PreviewInput::Click { x, y, .. }
        | PreviewInput::PointerDown { x, y, .. }
        | PreviewInput::PointerUp { x, y, .. }
        | PreviewInput::PointerMove { x, y }
        | PreviewInput::Scroll { x, y, .. }
            if !x.is_finite() || !y.is_finite() || *x < 0.0 || *y < 0.0 =>
        {
            return Err("Invalid preview pointer position.".into());
        }
        PreviewInput::Scroll {
            delta_x, delta_y, ..
        } if !delta_x.is_finite() || !delta_y.is_finite() => {
            return Err("Invalid preview scroll delta.".into());
        }
        PreviewInput::Resize { width, height }
            if *width == 0
                || *height == 0
                || *width > MAX_PREVIEW_DIMENSION
                || *height > MAX_PREVIEW_DIMENSION =>
        {
            return Err("Preview dimensions must be between 1 and 2048 pixels.".into());
        }
        _ => {}
    }
    let message = riwork_core::app_preview::PreviewHostMessage::Input {
        input: input.clone(),
    };
    if serde_json::to_vec(&message)
        .map_err(|error| error.to_string())?
        .len()
        + 1
        > MAX_PREVIEW_INPUT_MESSAGE_BYTES
    {
        return Err("Encoded preview input exceeds the 64 KiB message limit.".into());
    }
    Ok(())
}

impl Drop for AppPreview {
    fn drop(&mut self) {
        let revision = self.cancellation.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = self.control.send(Control::Shutdown(revision));
    }
}

struct OwnedProcess {
    child: Child,
    group: u32,
    reaped: bool,
}

impl OwnedProcess {
    fn launch(
        spec: &PreviewSpec,
        endpoint: Option<&cooperative::Endpoint>,
        state: &Arc<Mutex<PreviewState>>,
    ) -> Result<Self, String> {
        let mut command = Command::new("/bin/zsh");
        command.args(["-lc", &spec.command]).current_dir(&spec.cwd);
        Self::launch_command(command, endpoint, state)
    }

    fn launch_command(
        mut command: Command,
        endpoint: Option<&cooperative::Endpoint>,
        state: &Arc<Mutex<PreviewState>>,
    ) -> Result<Self, String> {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // A child preview must not inherit a stale endpoint from a parent app.
        command
            .env_remove("RIWORK_PREVIEW_SOCKET")
            .env_remove("RIWORK_PREVIEW_TOKEN");
        if let Some(endpoint) = endpoint {
            command
                .env("RIWORK_PREVIEW_SOCKET", &endpoint.path)
                .env("RIWORK_PREVIEW_TOKEN", &endpoint.token);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("Cannot launch preview: {error}"))?;
        let group = child.id();
        if let Some(stdout) = child.stdout.take() {
            collect_logs(stdout, false, Arc::clone(state));
        }
        if let Some(stderr) = child.stderr.take() {
            collect_logs(stderr, true, Arc::clone(state));
        }
        Ok(Self {
            child,
            group,
            reaped: false,
        })
    }

    fn owns(&mut self, pid: u32) -> bool {
        // Keep the leader unreaped while using its group as a launch identity.
        // A live leader prevents its pid/group number being recycled.
        if pid == std::process::id() || self.exited().is_some() {
            return false;
        }
        #[cfg(unix)]
        {
            // SAFETY: getpgid is a read-only process query, including for an exited pid.
            unsafe { libc::getpgid(pid as libc::pid_t) == self.group as libc::pid_t }
        }
        #[cfg(not(unix))]
        {
            pid == self.group
        }
    }

    fn exited(&mut self) -> Option<String> {
        #[cfg(unix)]
        {
            // Observe, without reaping, so the leader's pid/group number stays
            // reserved until stop() also terminates any surviving descendants.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            // SAFETY: `info` is a valid output buffer and this is our own child.
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.group as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if result == 0 {
                #[cfg(target_os = "macos")]
                let status = info.si_status;
                #[cfg(not(target_os = "macos"))]
                let status = unsafe { info.si_status() };
                return (info.si_signo != 0).then(|| {
                    if info.si_code == libc::CLD_EXITED {
                        format!("App exited with status {status}.")
                    } else {
                        format!("App stopped by signal {status}.")
                    }
                });
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ECHILD) {
                self.reaped = true;
            }
            Some(format!("Cannot inspect preview process: {error}"))
        }
        #[cfg(not(unix))]
        match self.child.try_wait() {
            Ok(Some(status)) => {
                self.reaped = true;
                Some(format!("App exited ({status})."))
            }
            Err(error) => Some(format!("Cannot inspect preview process: {error}")),
            Ok(None) => None,
        }
    }

    fn stop(&mut self) {
        // Do not signal a group after reaping its leader: that numeric group
        // could eventually have been reused by a different user's process.
        if self.reaped {
            return;
        }
        #[cfg(unix)]
        {
            // SAFETY: process_group(0) created this group for this live child.
            unsafe {
                libc::kill(-(self.group as libc::pid_t), libc::SIGTERM);
            }
            let deadline = Instant::now() + Duration::from_millis(500);
            while Instant::now() < deadline {
                // Do not reap until after the final group signal.
                thread::sleep(Duration::from_millis(20));
            }
            // The leader may be a zombie, still reserving its pid until wait.
            unsafe {
                libc::kill(-(self.group as libc::pid_t), libc::SIGKILL);
            }
        }
        #[cfg(not(unix))]
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.reaped = true;
    }
}

impl Drop for OwnedProcess {
    fn drop(&mut self) {
        self.stop();
    }
}

fn collect_logs(
    mut source: impl Read + Send + 'static,
    stderr: bool,
    state: Arc<Mutex<PreviewState>>,
) {
    thread::spawn(move || {
        let mut bytes = [0; 2048];
        loop {
            let Ok(count) = source.read(&mut bytes) else {
                break;
            };
            if count == 0 {
                break;
            }
            let text = String::from_utf8_lossy(&bytes[..count]).into_owned();
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            push_log(&mut state.logs, PreviewLog { text, stderr });
        }
    });
}

fn push_log(logs: &mut Vec<PreviewLog>, log: PreviewLog) {
    logs.push(log);
    let mut bytes: usize = logs.iter().map(|log| log.text.len()).sum();
    while logs.len() > LOG_ENTRIES || bytes > LOG_BYTES {
        bytes -= logs.remove(0).text.len();
    }
}

fn set_message(state: &Arc<Mutex<PreviewState>>, phase: PreviewPhase, message: impl Into<String>) {
    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    state.phase = phase;
    state.message = message.into();
}

enum Transport {
    Cooperative(cooperative::Endpoint),
    Capture(capture::Capture),
}

impl Transport {
    fn open(mode: PreviewMode) -> Result<Self, String> {
        match mode {
            PreviewMode::Cooperative | PreviewMode::IosSimulator => {
                cooperative::Endpoint::open().map(Self::Cooperative)
            }
            PreviewMode::WindowCapture => Ok(Self::Capture(capture::Capture::new())),
        }
    }
    fn endpoint(&self) -> Option<&cooperative::Endpoint> {
        match self {
            Self::Cooperative(endpoint) => Some(endpoint),
            _ => None,
        }
    }
    fn tick(
        &mut self,
        process: &mut OwnedProcess,
        visible: bool,
        state: &Arc<Mutex<PreviewState>>,
    ) -> Result<(), String> {
        match self {
            Self::Cooperative(endpoint) => endpoint.tick(process, visible, state),
            Self::Capture(capture) => capture.tick(process, visible, state),
        }
    }
    fn input(
        &mut self,
        input: PreviewInput,
        process: &mut OwnedProcess,
        state: &Arc<Mutex<PreviewState>>,
    ) -> Result<(), String> {
        match self {
            Self::Cooperative(endpoint) => endpoint.input(input),
            Self::Capture(capture) => capture.input(input, process, state),
        }
    }
    fn shutdown(&mut self) {
        if let Self::Cooperative(endpoint) = self {
            endpoint.shutdown();
        }
    }
}

fn worker(
    spec: PreviewSpec,
    state: Arc<Mutex<PreviewState>>,
    controls: mpsc::Receiver<Control>,
    inputs: mpsc::Receiver<PreviewInput>,
    cancellation: Arc<AtomicU64>,
    active_revision: Arc<AtomicU64>,
) {
    let mut visible = true;
    let mut launch = true;
    let mut process = None::<OwnedProcess>;
    let mut transport = None::<Transport>;
    let mut consecutive_error = None::<String>;
    let mut pending = VecDeque::<PreviewInput>::new();
    let mut priority_control = None;
    let mut launch_revision = 0;
    let mut ios_session = None;
    loop {
        if launch {
            launch = false;
            // Input belongs to a process generation; never replay it after restart.
            while inputs.try_recv().is_ok() {}
            pending.clear();
            {
                let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                state.phase = PreviewPhase::Starting;
                state.message = "Starting preview…".into();
                state.pid = None;
                state.window_id = None;
                state.windows.clear();
                state.frame = None;
                state.warning = None;
                state.ios = None;
                active_revision.store(launch_revision, Ordering::SeqCst);
            }
            match Transport::open(spec.mode).and_then(|transport| {
                if spec.mode == PreviewMode::IosSimulator {
                    let (process, session) = ios::launch(
                        &spec,
                        transport.endpoint().expect("cooperative transport"),
                        &state,
                        Arc::clone(&cancellation),
                        launch_revision,
                    )?;
                    ios_session = Some(session);
                    Ok((transport, process))
                } else {
                    OwnedProcess::launch(&spec, transport.endpoint(), &state)
                        .map(|process| (transport, process))
                }
            }) {
                Ok((new_transport, new_process)) => {
                    state.lock().unwrap_or_else(|e| e.into_inner()).pid = Some(new_process.group);
                    process = Some(new_process);
                    transport = Some(new_transport);
                    consecutive_error = None;
                    set_message(
                        &state,
                        PreviewPhase::WaitingForWindow,
                        match spec.mode {
                            PreviewMode::Cooperative => "Waiting for the app's RiWork preview SDK…",
                            PreviewMode::WindowCapture => "Waiting for the launched app's window…",
                            PreviewMode::IosSimulator => {
                                "Waiting for the iOS simulator's first frame…"
                            }
                        },
                    );
                }
                Err(error) => set_message(&state, PreviewPhase::Error, error),
            }
        }
        match priority_control
            .take()
            .map(Ok)
            .unwrap_or_else(|| controls.recv_timeout(TICK))
        {
            Ok(control) => {
                match control {
                    Control::Visible(value) => visible = value,
                    Control::SelectWindow { pid, window_id } => {
                        if let (Some(Transport::Capture(capture)), Some(process)) =
                            (&mut transport, &mut process)
                        {
                            if let Err(error) =
                                capture.select_window(pid, window_id, process, &state)
                            {
                                set_message(&state, PreviewPhase::Error, error);
                            }
                        }
                    }
                    Control::Stop(revision)
                    | Control::Restart(revision)
                    | Control::Shutdown(revision) => {
                        if let Some(transport) = &mut transport {
                            transport.shutdown();
                        }
                        drop(transport.take());
                        drop(process.take());
                        drop(ios_session.take());
                        pending.clear();
                        let latest = revision == cancellation.load(Ordering::SeqCst);
                        let restarting = matches!(control, Control::Restart(_)) || !latest;
                        {
                            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                            state.phase = if restarting {
                                PreviewPhase::Starting
                            } else {
                                PreviewPhase::Stopped
                            };
                            state.message = if restarting {
                                "Restarting preview…"
                            } else {
                                "Preview stopped."
                            }
                            .into();
                            state.frame = None;
                            state.pid = None;
                            state.window_id = None;
                            if !restarting {
                                active_revision.store(revision, Ordering::SeqCst);
                            }
                        }
                        if matches!(control, Control::Shutdown(_)) {
                            break;
                        }
                        launch = matches!(control, Control::Restart(_)) && latest;
                        launch_revision = revision;
                    }
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if let Some(message) = process.as_mut().and_then(OwnedProcess::exited) {
            if let Some(transport) = &mut transport {
                transport.shutdown();
            }
            drop(transport.take());
            drop(process.take());
            drop(ios_session.take());
            set_message(&state, PreviewPhase::Exited, message);
            while inputs.try_recv().is_ok() {}
            continue;
        }
        let (Some(process), Some(transport)) = (&mut process, &mut transport) else {
            continue;
        };
        // Coalesce high-rate pointer and resize events; preserve key/button order.
        while pending.len() < INPUT_QUEUE {
            let Ok(input) = inputs.try_recv() else {
                break;
            };
            if matches!(
                (&input, pending.back()),
                (
                    PreviewInput::PointerMove { .. },
                    Some(PreviewInput::PointerMove { .. })
                ) | (
                    PreviewInput::Resize { .. },
                    Some(PreviewInput::Resize { .. })
                )
            ) {
                pending.pop_back();
            }
            pending.push_back(input);
        }
        if !visible {
            // The UI releases held buttons and focus immediately before hiding.
            // Deliver those releases so the child's logical input cannot stick.
            pending.retain(|input| {
                matches!(
                    input,
                    PreviewInput::PointerUp { .. }
                        | PreviewInput::KeyUp { .. }
                        | PreviewInput::Focus { focused: false }
                )
            });
        }
        // Cua requests have a timeout. Return to the priority control channel
        // after one event so Stop cannot wait behind a long queue of input.
        let budget = if spec.mode == PreviewMode::WindowCapture {
            1
        } else {
            8
        };
        for _ in 0..budget {
            let Some(input) = pending.pop_front() else {
                break;
            };
            if let Err(error) = transport.input(input, process, &state) {
                state.lock().unwrap_or_else(|e| e.into_inner()).warning = Some(error);
            }
        }
        if let Ok(control) = controls.try_recv() {
            priority_control = Some(control);
            continue;
        }
        // A background CLI-created tab must produce one frame before pausing so
        // `preview open --wait` can report readiness without focusing a window.
        let effective_visible = visible
            || state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .frame
                .is_none();
        match transport.tick(process, effective_visible, &state) {
            Ok(()) => consecutive_error = None,
            Err(error) => {
                if consecutive_error.as_ref() != Some(&error) {
                    set_message(&state, PreviewPhase::Error, &error);
                    consecutive_error = Some(error);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn launch_rejects_missing_folder_or_empty_command_without_executing() {
        let spec = PreviewSpec {
            command: " ".into(),
            cwd: PathBuf::from("/"),
            mode: PreviewMode::Cooperative,
            ios: None,
        };
        assert!(AppPreview::launch(spec).is_err());
        let spec = PreviewSpec {
            command: "echo never".into(),
            cwd: PathBuf::from("/riwork-test-missing-preview-folder"),
            mode: PreviewMode::Cooperative,
            ios: None,
        };
        assert!(AppPreview::launch(spec).is_err());
    }

    #[test]
    fn logs_have_a_byte_and_entry_bound_even_without_newlines() {
        let mut logs = Vec::new();
        for _ in 0..1000 {
            push_log(
                &mut logs,
                PreviewLog {
                    text: "x".repeat(2048),
                    stderr: false,
                },
            );
        }
        assert!(logs.len() <= LOG_ENTRIES);
        assert!(logs.iter().map(|log| log.text.len()).sum::<usize>() <= LOG_BYTES);
    }

    #[test]
    fn input_limits_reject_nonfinite_positions_and_escaped_text_growth() {
        assert!(
            validate_input(&PreviewInput::PointerMove {
                x: f32::NAN,
                y: 0.0
            })
            .is_err()
        );
        assert!(
            validate_input(&PreviewInput::Scroll {
                x: 0.0,
                y: 0.0,
                delta_x: f32::INFINITY,
                delta_y: 0.0
            })
            .is_err()
        );
        assert!(
            validate_input(&PreviewInput::Resize {
                width: 0,
                height: 100
            })
            .is_err()
        );
        assert!(
            validate_input(&PreviewInput::Text {
                text: "\0".repeat(20 * 1024)
            })
            .is_err()
        );
        assert!(
            validate_input(&PreviewInput::Text {
                text: "hello 👋".into()
            })
            .is_ok()
        );
    }

    #[test]
    fn restart_never_exposes_an_old_inflight_frame_as_new_readiness() {
        let state = Arc::new(Mutex::new(PreviewState::new(PreviewMode::Cooperative)));
        let (control, controls) = mpsc::channel();
        let (input, _inputs) = mpsc::sync_channel(1);
        let preview = AppPreview {
            state: Arc::clone(&state),
            control,
            input,
            cancellation: Arc::new(AtomicU64::new(0)),
            active_revision: Arc::new(AtomicU64::new(0)),
        };
        preview.restart();
        assert!(matches!(controls.try_recv(), Ok(Control::Restart(1))));
        // Simulate a transport already in flight when restart was requested.
        {
            let mut old = state.lock().unwrap();
            old.phase = PreviewPhase::Streaming;
            old.frame = Some(Arc::new(PreviewFrame {
                png: vec![],
                width: 1,
                height: 1,
                sequence: 99,
            }));
        }
        assert_eq!(preview.snapshot().phase, PreviewPhase::Starting);
        assert!(preview.snapshot().frame.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn observing_exit_does_not_reap_group_leader_before_descendant_cleanup() {
        use std::os::unix::process::CommandExt;
        let mut child = Command::new("/bin/sh")
            .args(["-c", "sleep 60 & echo $!; exit 0"])
            .stdout(Stdio::piped())
            .process_group(0)
            .spawn()
            .unwrap();
        let mut line = String::new();
        use std::io::BufRead;
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let descendant = line.trim().parse::<libc::pid_t>().unwrap();
        let group = child.id();
        let mut owned = OwnedProcess {
            child,
            group,
            reaped: false,
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        while owned.exited().is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(owned.exited().is_some());
        assert!(
            !owned.reaped,
            "exited() must reserve this launch's process group"
        );
        // This child has no GUI and is solely a process-ownership fixture.
        assert_eq!(unsafe { libc::getpgid(descendant) }, group as libc::pid_t);
        owned.stop();
        assert!(owned.reaped);
        let deadline = Instant::now() + Duration::from_secs(2);
        while unsafe { libc::kill(descendant, 0) } == 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert_ne!(
            unsafe { libc::kill(descendant, 0) },
            0,
            "owned descendant survived stop"
        );
    }
}
