//! An AppView owner for the unchanged preview engine. Constructing/restoring a
//! handle does no I/O and never starts a process. Only an explicit Run or
//! control-start request may launch; restoration never does.
use crate::service;
use nsh_core::{element::Cleanup, prelude::*};
use riwork_core::app_preview::{PreviewInput, PreviewMode, PreviewSpec};
use riwork_engine::app_preview::{AppPreview, PreviewPhase, PreviewState, validate_input};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub(crate) trait Session: Send + Sync {
    fn snapshot(&self) -> PreviewState;
    fn visible(&self, visible: bool);
    fn input(&self, input: PreviewInput);
    fn stop(&self);
    fn restart(&self);
    fn select_window(&self, pid: u32, window: u64);
}
impl Session for AppPreview {
    fn snapshot(&self) -> PreviewState {
        self.snapshot()
    }
    fn visible(&self, visible: bool) {
        self.set_visible(visible);
    }
    fn input(&self, input: PreviewInput) {
        self.input(input);
    }
    fn stop(&self) {
        self.stop();
    }
    fn restart(&self) {
        self.restart();
    }
    fn select_window(&self, pid: u32, window: u64) {
        self.select_window(pid, window);
    }
}
type Launch = Arc<dyn Fn(PreviewSpec) -> Result<Box<dyn Session>, String> + Send + Sync>;
struct Controller {
    session: Option<Arc<dyn Session>>,
    launched: Option<PreviewSpec>,
    surface: Arc<crate::preview_surface::Surface>,
    launch: Launch,
}
impl Controller {
    fn new(launch: Launch) -> Self {
        Self {
            session: None,
            launched: None,
            surface: crate::preview_surface::Surface::new(),
            launch,
        }
    }
    fn run(&mut self, spec: PreviewSpec, closed: &AtomicBool) -> Result<(), String> {
        if closed.load(Ordering::Acquire) {
            return Err("unavailable: preview is closed".into());
        }
        if spec.mode != PreviewMode::IosSimulator && spec.command.trim().is_empty() {
            return Err("Enter the command that builds and starts your app.".into());
        }
        if spec.mode == PreviewMode::IosSimulator
            && spec
                .ios
                .as_ref()
                .is_none_or(|ios| ios.scheme.trim().is_empty())
        {
            return Err("Configure the Xcode scheme for this iOS preview.".into());
        }
        self.surface.reset();
        if let Some(session) = &self.session {
            if self.launched.as_ref() == Some(&spec) {
                session.restart();
                self.surface.attach(session.clone(), spec.mode);
                return Ok(());
            }
            // An edited spec must not leave two process groups alive. Error is
            // not terminal: an errored capture can still own a running child.
            session.stop();
            let until = Instant::now() + Duration::from_secs(30);
            while !matches!(
                session.snapshot().phase,
                PreviewPhase::Stopped | PreviewPhase::Exited
            ) {
                if closed.load(Ordering::Acquire) {
                    self.session = None;
                    return Err("unavailable: preview is closed".into());
                }
                if Instant::now() >= until {
                    return Err(
                        "Preview is still stopping. Try Run again when it has stopped.".into(),
                    );
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        self.session = None;
        if closed.load(Ordering::Acquire) {
            return Err("unavailable: preview is closed".into());
        }
        let session: Arc<dyn Session> = Arc::from((self.launch)(spec.clone())?);
        if closed.load(Ordering::Acquire) {
            session.stop();
            return Err("unavailable: preview is closed".into());
        }
        self.surface.attach(session.clone(), spec.mode);
        self.session = Some(session);
        self.launched = Some(spec);
        Ok(())
    }
    fn snapshot(&mut self) -> Value {
        let Some(session) = &self.session else {
            return json!({"phase":"idle","message":"Enter a command, then select Run.","logs":[],"windows":[],"frame":null,"pid":null});
        };
        let state = session.snapshot();
        json!({"phase":format!("{:?}",state.phase).to_lowercase(), "message":state.message,"warning":self.surface.error().or(state.warning),"pid":state.pid,"windowId":state.window_id,
            "frame":state.frame.as_ref().map(|frame| json!({"width":frame.width,"height":frame.height})),
            "windows":state.windows.iter().map(|w| json!({"pid":w.pid,"id":w.window_id,"title":w.title})).collect::<Vec<_>>(),
            "logs":state.logs.iter().rev().take(160).collect::<Vec<_>>().into_iter().rev().map(|log| json!({"text":log.text.chars().filter(|c| !c.is_control() || *c == '\n' || *c == '\t').take(2000).collect::<String>(),"stderr":log.stderr})).collect::<Vec<_>>()})
    }
    fn start_controlled(
        &mut self,
        spec: PreviewSpec,
        closed: &AtomicBool,
        now: Instant,
    ) -> Result<(), String> {
        if closed.load(Ordering::Acquire) {
            return Err("unavailable: preview is closed".into());
        }
        let running = self.session.as_ref().is_some_and(|session| {
            matches!(
                session.snapshot().phase,
                PreviewPhase::Starting
                    | PreviewPhase::WaitingForWindow
                    | PreviewPhase::Streaming
                    | PreviewPhase::Paused
            )
        });
        if !running {
            self.run(spec, closed)?;
        }
        self.surface.control_start(now);
        Ok(())
    }
}
struct Owner {
    state: Mutex<Controller>,
    closed: AtomicBool,
    surface: Arc<crate::preview_surface::Surface>,
}
impl Owner {
    fn new(controller: Controller) -> Arc<Self> {
        Arc::new(Self {
            surface: controller.surface.clone(),
            state: Mutex::new(controller),
            closed: AtomicBool::new(false),
        })
    }
    fn close(self: &Arc<Self>) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.surface.close();
        if let Ok(mut state) = self.state.try_lock() {
            state.session = None;
            return;
        }
        let owner = self.clone();
        // A Run may be waiting for the old child to stop. Never block AppView
        // teardown; the worker observes closed before it can publish a launch.
        let _ = std::thread::Builder::new()
            .name("nsh-preview-close".into())
            .spawn(move || {
                owner.state.lock().unwrap().session = None;
            });
    }
    fn check(&self) -> Result<(), String> {
        if self.closed.load(Ordering::Acquire) {
            Err("unavailable: preview is closed".into())
        } else {
            Ok(())
        }
    }
}
#[class]
pub struct PreviewHandle {
    owner: Arc<Owner>,
    cleanup: Option<Cleanup>,
}
impl Drop for PreviewHandle {
    fn drop(&mut self) {
        self.owner.close();
    }
}
#[class]
impl PreviewHandle {
    /// Opaque process-local native surface binding; contains no frame bytes.
    pub fn surface_id(&self) -> String {
        self.owner.surface.id().to_owned()
    }
    pub async fn snapshot(this: Instance<Self>) -> Result<Value, String> {
        let owner = this.with(|handle| handle.owner.clone());
        service::blocking(move || {
            owner.check()?;
            Ok(owner.state.lock().unwrap().snapshot())
        })
        .await
    }
    pub async fn run(this: Instance<Self>, spec: Value) -> Result<(), String> {
        let spec = serde_json::from_value(spec).map_err(|e| format!("invalid: {e}"))?;
        let owner = this.with(|handle| handle.owner.clone());
        service::blocking(move || owner.state.lock().unwrap().run(spec, &owner.closed)).await
    }
    /// Explicit control-window launch, with a five-second capture visibility lease.
    pub async fn start_controlled(this: Instance<Self>, spec: Value) -> Result<(), String> {
        let spec: PreviewSpec =
            serde_json::from_value(spec).map_err(|e| format!("invalid: {e}"))?;
        let owner = this.with(|handle| handle.owner.clone());
        service::blocking(move || {
            owner.check()?;
            owner
                .state
                .lock()
                .unwrap()
                .start_controlled(spec, &owner.closed, Instant::now())
        })
        .await
    }
    /// Explicit control capture never admits local input to an invisible panel.
    pub async fn refresh_controlled(this: Instance<Self>) -> Result<(), String> {
        let owner = this.with(|handle| handle.owner.clone());
        service::blocking(move || {
            owner.check()?;
            owner.surface.control_refresh(Instant::now())
        })
        .await
    }
    pub async fn stop(this: Instance<Self>) -> Result<(), String> {
        let owner = this.with(|handle| handle.owner.clone());
        service::blocking(move || {
            owner.check()?;
            owner.surface.reset();
            if let Some(session) = &owner.state.lock().unwrap().session {
                session.stop();
            }
            Ok(())
        })
        .await
    }
    pub async fn set_visible(this: Instance<Self>, visible: bool) -> Result<(), String> {
        let owner = this.with(|handle| handle.owner.clone());
        service::blocking(move || {
            owner.check()?;
            owner.surface.set_visible(visible);
            Ok(())
        })
        .await
    }
    pub async fn input(this: Instance<Self>, input: Value) -> Result<(), String> {
        let input: PreviewInput =
            serde_json::from_value(input).map_err(|e| format!("invalid: {e}"))?;
        validate_input(&input)?;
        let owner = this.with(|handle| handle.owner.clone());
        service::blocking(move || {
            owner.check()?;
            match input {
                PreviewInput::Refresh => owner.surface.control_refresh(Instant::now())?,
                PreviewInput::ShowWindow => {
                    if let Some(session) = &owner.state.lock().unwrap().session {
                        session.input(input);
                    }
                }
                _ => {
                    return Err(
                        "invalid: interactive input belongs to the native preview surface".into(),
                    );
                }
            }
            Ok(())
        })
        .await
    }
    pub async fn select_window(this: Instance<Self>, pid: u32, window: u64) -> Result<(), String> {
        let owner = this.with(|handle| handle.owner.clone());
        service::blocking(move || {
            owner.check()?;
            let state = owner.state.lock().unwrap();
            if let Some(session) = &state.session {
                owner.surface.select(
                    session.clone(),
                    state
                        .launched
                        .as_ref()
                        .map_or(PreviewMode::Cooperative, |s| s.mode),
                    pid,
                    window,
                );
            }
            Ok(())
        })
        .await
    }
    pub fn close(&mut self) {
        self.owner.close();
        if let Some(cleanup) = self.cleanup.take() {
            cleanup.release();
        }
    }
}
#[module]
pub struct Previews;
#[module]
impl Previews {
    pub fn open(cx: CallCx<'_>) -> Result<PreviewHandle, String> {
        let home = service::process_home()?;
        let launch: Launch = if crate::cua::fixture_home(&home) {
            Arc::new(|spec| Ok(Box::new(FakeSession::new(spec))))
        } else {
            Arc::new(|spec| {
                AppPreview::launch(spec).map(|session| Box::new(session) as Box<dyn Session>)
            })
        };
        let owner = Owner::new(Controller::new(launch));
        let cleanup_owner = owner.clone();
        let cleanup = cx.on_cleanup(move || cleanup_owner.close());
        Ok(PreviewHandle {
            owner,
            cleanup: Some(cleanup),
        })
    }
}

// No subprocess, capture, network or simulator in a fixture home.
struct FakeSession(Mutex<PreviewState>);
impl FakeSession {
    fn new(spec: PreviewSpec) -> Self {
        Self(Mutex::new(PreviewState {
            phase: PreviewPhase::Streaming,
            mode: spec.mode,
            pid: Some(424242),
            window_id: None,
            windows: vec![],
            message: "Fixture preview running".into(),
            warning: None,
            frame: None,
            logs: vec![],
            ios: None,
        }))
    }
}
impl Session for FakeSession {
    fn snapshot(&self) -> PreviewState {
        self.0.lock().unwrap().clone()
    }
    fn visible(&self, visible: bool) {
        let mut state = self.0.lock().unwrap();
        if matches!(state.phase, PreviewPhase::Streaming | PreviewPhase::Paused) {
            state.phase = if visible {
                PreviewPhase::Streaming
            } else {
                PreviewPhase::Paused
            };
        }
    }
    fn input(&self, _: PreviewInput) {}
    fn stop(&self) {
        let mut state = self.0.lock().unwrap();
        state.phase = PreviewPhase::Stopped;
        state.pid = None;
        state.message = "Preview stopped".into();
    }
    fn restart(&self) {
        let mut state = self.0.lock().unwrap();
        state.phase = PreviewPhase::Streaming;
        state.pid = Some(424242);
        state.message = "Fixture preview restarted".into();
    }
    fn select_window(&self, _: u32, _: u64) {}
}

#[cfg(test)]
mod phase6c_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    fn spec(command: &str) -> PreviewSpec {
        PreviewSpec {
            command: command.into(),
            cwd: "/fixture/project".into(),
            mode: Default::default(),
            ios: None,
        }
    }
    #[test]
    fn phase6c_preview_run_only_restart_edited_spec_stop_visibility_and_dispose() {
        let launches = Arc::new(AtomicUsize::new(0));
        let launched = launches.clone();
        let mut controller = Controller::new(Arc::new(move |spec| {
            launched.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(FakeSession::new(spec)))
        }));
        let closed = AtomicBool::new(false);
        assert_eq!(controller.snapshot()["phase"], "idle");
        assert_eq!(launches.load(Ordering::SeqCst), 0);
        assert!(controller.run(spec(""), &closed).is_err());
        assert_eq!(launches.load(Ordering::SeqCst), 0);
        controller.run(spec("first"), &closed).unwrap();
        assert_eq!(controller.snapshot()["phase"], "paused");
        controller.session.as_ref().unwrap().visible(true);
        assert_eq!(controller.snapshot()["phase"], "streaming");
        controller.run(spec("first"), &closed).unwrap();
        assert_eq!(
            launches.load(Ordering::SeqCst),
            1,
            "same spec uses engine restart"
        );
        controller.run(spec("edited"), &closed).unwrap();
        assert_eq!(launches.load(Ordering::SeqCst), 2);
        controller.session.as_ref().unwrap().stop();
        assert_eq!(controller.snapshot()["phase"], "stopped");
        let owner = Owner::new(controller);
        owner.close();
        assert!(owner.state.lock().unwrap().session.is_none());
        assert!(
            owner
                .state
                .lock()
                .unwrap()
                .run(spec("resurrection"), &owner.closed)
                .is_err()
        );
        assert_eq!(launches.load(Ordering::SeqCst), 2);
    }
    #[test]
    fn phase6c_owner_teardown_during_delayed_launch_cannot_publish_orphan() {
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (finish_tx, finish_rx) = std::sync::mpsc::channel();
        let finish = Mutex::new(finish_rx);
        let owner = Owner::new(Controller::new(Arc::new(move |spec| {
            started_tx.send(()).unwrap();
            finish.lock().unwrap().recv().unwrap();
            Ok(Box::new(FakeSession::new(spec)))
        })));
        let worker = owner.clone();
        let join = std::thread::spawn(move || {
            worker
                .state
                .lock()
                .unwrap()
                .run(spec("delayed"), &worker.closed)
        });
        started_rx.recv().unwrap();
        owner.close();
        finish_tx.send(()).unwrap();
        assert!(join.join().unwrap().is_err());
        assert!(owner.state.lock().unwrap().session.is_none());
    }

    #[test]
    fn preview_json_snapshot_has_metadata_only_and_bounds_display_logs() {
        use riwork_engine::app_preview::{PreviewFrame, PreviewLog};
        let session = Arc::new(FakeSession::new(spec("fake")));
        {
            let mut state = session.0.lock().unwrap();
            // A large arbitrary source payload must never be encoded/copied
            // into the JSON result. Decoding belongs to the surface worker.
            state.frame = Some(Arc::new(PreviewFrame {
                png: vec![0xab; 16 * 1024 * 1024],
                width: 2048,
                height: 2048,
                sequence: 42,
            }));
            state.logs = (0..200)
                .map(|i| PreviewLog {
                    text: format!("{i}:\0\u{1b}\t\n{}", "é".repeat(2400)),
                    stderr: i % 2 == 0,
                })
                .collect();
        }
        let mut controller = Controller::new(Arc::new(|_| panic!("snapshot must not launch")));
        controller.session = Some(session.clone());
        let snapshot = controller.snapshot();
        assert_eq!(snapshot["frame"], json!({"width":2048,"height":2048}));
        let logs = snapshot["logs"].as_array().unwrap();
        assert_eq!(logs.len(), 160);
        assert!(logs[0]["text"].as_str().unwrap().starts_with("40:\t\n"));
        assert_eq!(logs[0]["stderr"], true);
        for log in logs {
            let text = log["text"].as_str().unwrap();
            assert_eq!(text.chars().count(), 2000);
            assert!(!text.contains(['\0', '\u{1b}']));
        }
        let encoded = snapshot.to_string();
        assert!(
            !encoded.contains("data:image")
                && !encoded.contains("base64")
                && !encoded.contains("png")
                && !encoded.contains("src")
        );
        assert!(encoded.len() < 650_000);
        session.0.lock().unwrap().logs.clear();
        assert!(
            controller.snapshot().to_string().len() < 512,
            "16 MiB source yields <512-byte metadata snapshot"
        );
        eprintln!(
            "preview transport: 16777216 source bytes -> {} JSON bytes, dimensions only; logs last160 x2000 Unicode chars",
            controller.snapshot().to_string().len()
        );
    }

    #[test]
    fn preview_control_start_reuses_running_session_but_restarts_error_even_with_pid() {
        struct Shared(Arc<FakeSession>, Arc<AtomicUsize>);
        impl Session for Shared {
            fn snapshot(&self) -> PreviewState {
                self.0.snapshot()
            }
            fn visible(&self, visible: bool) {
                self.0.visible(visible);
            }
            fn input(&self, input: PreviewInput) {
                self.0.input(input);
            }
            fn stop(&self) {
                self.0.stop();
            }
            fn restart(&self) {
                self.1.fetch_add(1, Ordering::SeqCst);
                self.0.restart();
            }
            fn select_window(&self, pid: u32, window: u64) {
                self.0.select_window(pid, window);
            }
        }
        let session = Arc::new(FakeSession::new(spec("fake")));
        let restarts = Arc::new(AtomicUsize::new(0));
        let launches = Arc::new(AtomicUsize::new(0));
        let mut controller = Controller::new({
            let (session, restarts, launches) =
                (session.clone(), restarts.clone(), launches.clone());
            Arc::new(move |_| {
                launches.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(Shared(session.clone(), restarts.clone())))
            })
        });
        let closed = AtomicBool::new(false);
        assert!(controller.session.is_none());
        controller
            .start_controlled(spec("fake"), &closed, Instant::now())
            .unwrap();
        assert_eq!(launches.load(Ordering::SeqCst), 1);
        controller
            .start_controlled(spec("fake"), &closed, Instant::now())
            .unwrap();
        assert_eq!(restarts.load(Ordering::SeqCst), 0);
        session.0.lock().unwrap().phase = PreviewPhase::Error;
        assert!(session.snapshot().pid.is_some());
        controller
            .start_controlled(spec("fake"), &closed, Instant::now())
            .unwrap();
        assert_eq!(restarts.load(Ordering::SeqCst), 1);
        let owner = Owner::new(controller);
        owner.close();
        assert!(
            owner
                .state
                .lock()
                .unwrap()
                .start_controlled(spec("fake"), &owner.closed, Instant::now())
                .is_err()
        );
        assert_eq!(launches.load(Ordering::SeqCst), 1);
        assert_eq!(restarts.load(Ordering::SeqCst), 1);
    }
}
