//! An AppView owner for the unchanged preview engine. Constructing/restoring a
//! handle does no I/O and never starts a process. Run is the only launch path.
use crate::service;
use base64::Engine as _;
use nsh_core::{element::Cleanup, prelude::*};
use riwork_core::app_preview::{
    MAX_PREVIEW_DIMENSION, MAX_PREVIEW_MESSAGE_BYTES, PreviewInput, PreviewMode, PreviewSpec,
};
use riwork_engine::app_preview::{
    AppPreview, PreviewFrame, PreviewPhase, PreviewState, validate_input,
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

trait Session: Send {
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
    session: Option<Box<dyn Session>>,
    launched: Option<PreviewSpec>,
    visible: bool,
    frame: Option<Arc<PreviewFrame>>,
    image: Option<String>,
    launch: Launch,
}
impl Controller {
    fn new(launch: Launch) -> Self {
        Self {
            session: None,
            launched: None,
            visible: false,
            frame: None,
            image: None,
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
        if let Some(session) = &self.session {
            if self.launched.as_ref() == Some(&spec) {
                session.restart();
                self.frame = None;
                self.image = None;
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
        self.frame = None;
        self.image = None;
        if closed.load(Ordering::Acquire) {
            return Err("unavailable: preview is closed".into());
        }
        let session = (self.launch)(spec.clone())?;
        if closed.load(Ordering::Acquire) {
            session.stop();
            return Err("unavailable: preview is closed".into());
        }
        session.visible(self.visible);
        self.session = Some(session);
        self.launched = Some(spec);
        Ok(())
    }
    fn snapshot(&mut self) -> Value {
        let Some(session) = &self.session else {
            return json!({"phase":"idle","message":"Enter a command, then select Run.","logs":[],"windows":[],"frame":null,"pid":null});
        };
        let state = session.snapshot();
        if let Some(frame) = &state.frame {
            if frame.png.len() <= MAX_PREVIEW_MESSAGE_BYTES
                && frame.width > 0
                && frame.height > 0
                && frame.width <= MAX_PREVIEW_DIMENSION
                && frame.height <= MAX_PREVIEW_DIMENSION
                && !self
                    .frame
                    .as_ref()
                    .is_some_and(|old| Arc::ptr_eq(old, frame))
            {
                self.image = Some(format!(
                    "data:image/png;base64,{}",
                    base64::engine::general_purpose::STANDARD.encode(&frame.png)
                ));
                self.frame = Some(frame.clone());
            }
        } else {
            self.frame = None;
            self.image = None;
        }
        json!({"phase":format!("{:?}",state.phase).to_lowercase(), "message":state.message,"warning":state.warning,"pid":state.pid,"windowId":state.window_id,
            "frame":self.frame.as_ref().map(|frame| json!({"src":self.image,"width":frame.width,"height":frame.height})),
            "windows":state.windows.iter().map(|w| json!({"pid":w.pid,"id":w.window_id,"title":w.title})).collect::<Vec<_>>(),
            "logs":state.logs.iter().rev().take(160).collect::<Vec<_>>().into_iter().rev().map(|log| json!({"text":log.text.chars().filter(|c| !c.is_control() || *c == '\n' || *c == '\t').take(2000).collect::<String>(),"stderr":log.stderr})).collect::<Vec<_>>()})
    }
}
struct Owner {
    state: Mutex<Controller>,
    closed: AtomicBool,
}
impl Owner {
    fn close(self: &Arc<Self>) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
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
    pub async fn stop(this: Instance<Self>) -> Result<(), String> {
        let owner = this.with(|handle| handle.owner.clone());
        service::blocking(move || {
            owner.check()?;
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
            let mut state = owner.state.lock().unwrap();
            state.visible = visible;
            if let Some(session) = &state.session {
                session.visible(visible);
            }
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
            if let Some(session) = &owner.state.lock().unwrap().session {
                session.input(input);
            }
            Ok(())
        })
        .await
    }
    pub async fn select_window(this: Instance<Self>, pid: u32, window: u64) -> Result<(), String> {
        let owner = this.with(|handle| handle.owner.clone());
        service::blocking(move || {
            owner.check()?;
            if let Some(session) = &owner.state.lock().unwrap().session {
                session.select_window(pid, window);
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
        let owner = Arc::new(Owner {
            state: Mutex::new(Controller::new(launch)),
            closed: AtomicBool::new(false),
        });
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
        let owner = Arc::new(Owner {
            state: Mutex::new(controller),
            closed,
        });
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
        let owner = Arc::new(Owner {
            state: Mutex::new(Controller::new(Arc::new(move |spec| {
                started_tx.send(()).unwrap();
                finish.lock().unwrap().recv().unwrap();
                Ok(Box::new(FakeSession::new(spec)))
            }))),
            closed: AtomicBool::new(false),
        });
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
}
