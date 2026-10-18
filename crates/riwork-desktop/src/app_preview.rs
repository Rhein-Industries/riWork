//! A separately owned development process, presented as an interactive tab.
//!
//! Restoring a tab only restores its launch configuration. A command runs only
//! after Run, and all desktop input stays behind the backend's owned target.

use std::{
    cell::Cell,
    io::Cursor,
    ops::Range,
    path::PathBuf,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{
    App, Bounds, Context, ElementInputHandler, Entity, EntityInputHandler, EventEmitter,
    FocusHandle, Focusable, IntoElement, KeyDownEvent, KeyUpEvent, MouseButton, ObjectFit, Pixels,
    Point, Render, RenderImage, Subscription, Task, UTF16Selection, Window, canvas, div, img,
    prelude::*, px, rgb,
};
use image::{Frame, ImageReader};
use riwork_core::app_preview::IosPreviewSpec;
use riwork_engine::app_preview::{
    AppPreview, PreviewFrame, PreviewInput, PreviewMode, PreviewMouseButton, PreviewPhase,
    PreviewSpec, PreviewState,
};
use riwork_engine::preview_control::PreviewControl;

use crate::{
    behavior_controls as behavior,
    controls::Button,
    text_input::{self, InputEvent, InputState},
    theme, ui_text,
};

const POLL_INTERVAL: Duration = Duration::from_millis(100);
const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
const MAX_FRAME_EDGE: u32 = 2048;
const MAX_LOG_LINES: usize = 160;

pub enum AppPreviewEvent {
    /// Commit a launch configuration for restoring the tab, without running it.
    Configured(PreviewSpec),
}

/// Kept narrow so headless UI tests can exercise lifecycle and routing without
/// starting a process or touching the desktop.
trait PreviewSession {
    fn snapshot(&self) -> PreviewState;
    fn set_visible(&self, visible: bool);
    fn input(&self, input: PreviewInput);
    fn stop(&self);
    fn restart(&self);
    fn select_window(&self, pid: u32, window_id: u64);
}

impl PreviewSession for AppPreview {
    fn snapshot(&self) -> PreviewState {
        self.snapshot()
    }
    fn set_visible(&self, visible: bool) {
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
    fn select_window(&self, pid: u32, window_id: u64) {
        self.select_window(pid, window_id);
    }
}

type Launch = Box<dyn Fn(PreviewSpec) -> Result<Box<dyn PreviewSession>, String>>;

#[derive(Clone, Copy)]
enum IosField {
    Scheme,
    Project,
    Device,
    Runtime,
}

#[derive(Clone)]
struct ControlBinding {
    store: PreviewControl,
    preview_id: String,
    host_id: String,
    project_id: String,
}

pub struct AppPreviewView {
    spec: PreviewSpec,
    command: Entity<InputState>,
    cwd: Entity<InputState>,
    ios_scheme: Entity<InputState>,
    ios_project: Entity<InputState>,
    ios_device: Entity<InputState>,
    ios_runtime: Entity<InputState>,
    focus: FocusHandle,
    session: Option<Box<dyn PreviewSession>>,
    launched_spec: Option<PreviewSpec>,
    pending_launch: bool,
    launch: Launch,
    state: Option<PreviewState>,
    error: Option<String>,
    visible: bool,
    capture_visible: bool,
    control_until: Option<Instant>,
    control_binding: Option<ControlBinding>,
    publishing: bool,
    image: Option<Arc<RenderImage>>,
    image_sequence: Option<u64>,
    source_frame: Option<Arc<PreviewFrame>>,
    generation: u64,
    frame_size: Option<(u32, u32)>,
    viewport: Rc<Cell<Bounds<Pixels>>>,
    last_resize: Option<(u32, u32)>,
    last_pointer: Option<Point<Pixels>>,
    pressed: Option<PreviewMouseButton>,
    focused: bool,
    marked: String,
    show_logs: bool,
    _subscriptions: Vec<Subscription>,
    _poll: Option<Task<()>>,
    decode: Option<Task<()>>,
}

impl EventEmitter<AppPreviewEvent> for AppPreviewView {}

impl AppPreviewView {
    pub fn new(spec: PreviewSpec, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::with_launcher(
            spec,
            window,
            cx,
            Box::new(|spec| {
                AppPreview::launch(spec).map(|session| Box::new(session) as Box<dyn PreviewSession>)
            }),
        )
    }

    fn with_launcher(
        spec: PreviewSpec,
        window: &mut Window,
        cx: &mut Context<Self>,
        launch: Launch,
    ) -> Self {
        let command =
            text_input::single_line(spec.command.clone(), "cargo run -p your-app", window, cx);
        let cwd = text_input::single_line(
            spec.cwd.to_string_lossy().into_owned(),
            "Working directory",
            window,
            cx,
        );
        let ios = spec.ios.clone().unwrap_or_default();
        let ios_scheme = text_input::single_line(
            ios.scheme,
            "Scheme (automatic when unambiguous)",
            window,
            cx,
        );
        let ios_project = text_input::single_line(
            ios.project
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default(),
            "Project or workspace (automatic)",
            window,
            cx,
        );
        let ios_device = text_input::single_line(
            ios.device_type.unwrap_or_default(),
            "iPhone model (automatic)",
            window,
            cx,
        );
        let ios_runtime = text_input::single_line(
            ios.runtime.unwrap_or_default(),
            "iOS version (latest installed)",
            window,
            cx,
        );
        let focus = cx.focus_handle();
        let mut subscriptions = vec![
            cx.subscribe_in(&command, window, |view, state, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    view.spec.command = state.read(cx).value().to_string();
                }
                if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                    view.configured(cx);
                }
            }),
            cx.subscribe_in(&cwd, window, |view, state, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    view.spec.cwd = PathBuf::from(state.read(cx).value().to_string());
                }
                if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                    view.configured(cx);
                }
            }),
            cx.observe_global::<theme::Appearance>(|_, cx| cx.notify()),
        ];
        for (input, field) in [
            (&ios_scheme, IosField::Scheme),
            (&ios_project, IosField::Project),
            (&ios_device, IosField::Device),
            (&ios_runtime, IosField::Runtime),
        ] {
            subscriptions.push(
                cx.subscribe_in(input, window, move |view, input, event, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        let value = input.read(cx).value().trim().to_owned();
                        let ios = view.spec.ios.get_or_insert_with(IosPreviewSpec::default);
                        match field {
                            IosField::Scheme => ios.scheme = value,
                            IosField::Project => {
                                ios.project = (!value.is_empty()).then(|| PathBuf::from(value))
                            }
                            IosField::Device => {
                                ios.device_type = (!value.is_empty()).then_some(value)
                            }
                            IosField::Runtime => ios.runtime = (!value.is_empty()).then_some(value),
                        }
                    }
                    if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                        view.configured(cx);
                    }
                }),
            );
        }
        subscriptions.push(cx.on_focus_out(&focus, window, |view, _, _, _| {
            view.release_input();
        }));
        subscriptions.push(cx.on_focus(&focus, window, |view, window, _| {
            if view.accepts_input(window) && !view.focused {
                view.send(PreviewInput::Focus { focused: true });
                view.focused = true;
            }
        }));
        cx.on_release(|view, cx| {
            view.release_input();
            if let Some(session) = view.session.take() {
                session.stop();
            }
            view.clear_image(cx);
            if let Some(binding) = view.control_binding.take() {
                let spec = view.spec.clone();
                cx.background_executor()
                    .spawn(async move {
                        let _ = binding.store.closed(
                            &binding.preview_id,
                            &binding.host_id,
                            &binding.project_id,
                            &spec,
                        );
                    })
                    .detach();
            }
        })
        .detach();
        let poll = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                if this
                    .update_in(cx, |view, window, cx| view.poll(window, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            spec,
            command,
            cwd,
            ios_scheme,
            ios_project,
            ios_device,
            ios_runtime,
            focus,
            session: None,
            launched_spec: None,
            pending_launch: false,
            launch,
            state: None,
            error: None,
            visible: false,
            capture_visible: false,
            control_until: None,
            control_binding: None,
            publishing: false,
            image: None,
            image_sequence: None,
            source_frame: None,
            generation: 0,
            frame_size: None,
            viewport: Rc::new(Cell::new(Bounds::default())),
            last_resize: None,
            last_pointer: None,
            pressed: None,
            focused: false,
            marked: String::new(),
            show_logs: false,
            _subscriptions: subscriptions,
            _poll: Some(poll),
            decode: None,
        }
    }

    pub fn spec(&self) -> &PreviewSpec {
        &self.spec
    }

    pub fn bind_control(
        &mut self,
        store: PreviewControl,
        preview_id: String,
        host_id: String,
        project_id: String,
    ) {
        self.control_binding = Some(ControlBinding {
            store,
            preview_id,
            host_id,
            project_id,
        });
    }

    fn publish_control(&mut self, state: PreviewState, cx: &mut Context<Self>) {
        if self.publishing {
            return;
        }
        let Some(binding) = self.control_binding.clone() else {
            return;
        };
        self.publishing = true;
        let spec = self.spec.clone();
        let revision = self.generation;
        let work = cx.background_executor().spawn(async move {
            binding.store.publish(
                &binding.preview_id,
                &binding.host_id,
                &binding.project_id,
                &spec,
                revision,
                &state,
            )
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |view, cx| {
                view.publishing = false;
                if let Err(error) = result {
                    view.error = Some(format!("Cannot publish preview status: {error}"));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub fn title(&self) -> String {
        self.spec
            .cwd
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .map(|name| format!("Preview · {name}"))
            .unwrap_or_else(|| "App Preview".into())
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.session.is_some() {
            self.focus.focus(window, cx);
        } else if self.spec.mode == PreviewMode::IosSimulator {
            self.ios_scheme.read(cx).focus_handle(cx).focus(window, cx);
        } else {
            self.command.read(cx).focus_handle(cx).focus(window, cx);
        }
    }

    /// Selection is supplied by the pane owner. Window hiding and deactivation
    /// are checked separately, so an invisible preview cannot accept input.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if !visible {
            self.release_input();
            if !self
                .control_until
                .is_some_and(|until| until > Instant::now())
            {
                self.capture_visible = false;
                if let Some(session) = &self.session {
                    session.set_visible(false);
                }
            }
        }
        cx.notify();
    }

    pub fn stop(&mut self, cx: &mut Context<Self>) {
        self.control_until = None;
        self.pending_launch = false;
        self.release_input();
        if let Some(session) = &self.session {
            session.stop();
        }
        self.set_fields_enabled(true, cx);
        cx.notify();
    }

    /// A shell explicitly requested a launch; restoring a layout never calls this.
    pub fn start_controlled(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        self.control_until = Some(Instant::now() + Duration::from_secs(5));
        if self.session.is_none() || !self.is_running() {
            self.run(cx);
        }
        if self.session.is_none() {
            return Err(self
                .error
                .clone()
                .unwrap_or_else(|| "Cannot start preview.".into()));
        }
        self.capture_visible = true;
        if let Some(session) = &self.session {
            session.set_visible(true);
        }
        Ok(())
    }

    pub fn refresh_controlled(&mut self) -> Result<(), String> {
        let session = self
            .session
            .as_ref()
            .ok_or("This preview has not been started.")?;
        let state = session.snapshot();
        if state.frame.is_none()
            || matches!(
                state.phase,
                PreviewPhase::Stopped | PreviewPhase::Exited | PreviewPhase::Error
            )
        {
            return Err(
                "Wait for a running preview to produce a frame before capturing it.".into(),
            );
        }
        self.control_until = Some(Instant::now() + Duration::from_secs(5));
        self.capture_visible = true;
        session.set_visible(true);
        session.input(PreviewInput::Refresh);
        Ok(())
    }

    pub fn input_controlled(&mut self, input: PreviewInput) -> Result<(), String> {
        if !self.is_running()
            || self
                .session
                .as_ref()
                .is_none_or(|session| session.snapshot().frame.is_none())
        {
            return Err("Wait for the preview to produce a frame before sending input.".into());
        }
        self.refresh_controlled()?;
        self.send(input);
        Ok(())
    }

    fn configured(&self, cx: &mut Context<Self>) {
        cx.emit(AppPreviewEvent::Configured(self.spec.clone()));
    }

    fn is_running(&self) -> bool {
        self.pending_launch
            || self.session.is_some()
                && self.state.as_ref().is_none_or(|state| {
                    !matches!(
                        state.phase,
                        PreviewPhase::Exited | PreviewPhase::Stopped | PreviewPhase::Error
                    )
                })
    }

    fn set_fields_enabled(&self, enabled: bool, cx: &mut Context<Self>) {
        self.command
            .update(cx, |state, cx| state.set_disabled(!enabled, cx));
        self.cwd
            .update(cx, |state, cx| state.set_disabled(!enabled, cx));
        for field in [
            &self.ios_scheme,
            &self.ios_project,
            &self.ios_device,
            &self.ios_runtime,
        ] {
            field.update(cx, |state, cx| state.set_disabled(!enabled, cx));
        }
    }

    fn can_stop(&self) -> bool {
        self.session.is_some()
            && self.state.as_ref().is_none_or(|state| {
                !matches!(state.phase, PreviewPhase::Stopped | PreviewPhase::Exited)
            })
    }

    fn run(&mut self, cx: &mut Context<Self>) {
        self.configured(cx);
        if self.spec.mode != PreviewMode::IosSimulator && self.spec.command.trim().is_empty() {
            self.error = Some("Enter the command that builds and starts your app.".into());
            cx.notify();
            return;
        }
        self.release_input();
        if let Some(session) = &self.session {
            if self.launched_spec.as_ref() == Some(&self.spec) {
                // The worker stops and reaps its previous process before it
                // launches again, so ports and build locks are released first.
                session.restart();
                self.reset_frame(cx);
                self.set_fields_enabled(false, cx);
            } else {
                // An edited command needs a new worker. Wait for an explicit
                // stopped acknowledgement before creating it.
                session.stop();
                self.reset_frame(cx);
                self.pending_launch = true;
                self.set_fields_enabled(false, cx);
            }
            cx.notify();
            return;
        }
        self.reset_frame(cx);
        match (self.launch)(self.spec.clone()) {
            Ok(session) => {
                session.set_visible(self.visible);
                self.capture_visible = self.visible;
                self.launched_spec = Some(self.spec.clone());
                self.session = Some(session);
                self.set_fields_enabled(false, cx);
            }
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    fn reset_frame(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        self.generation = self.generation.wrapping_add(1);
        if let Some(binding) = &self.control_binding {
            if let Err(error) = binding.store.begin_run(
                &binding.preview_id,
                &binding.host_id,
                &binding.project_id,
                &self.spec,
                self.generation,
            ) {
                self.error = Some(format!("Cannot start preview status: {error}"));
            }
        }
        self.decode = None;
        self.clear_image(cx);
        self.state = None;
        self.last_resize = None;
    }

    fn restart(&mut self, cx: &mut Context<Self>) {
        self.run(cx);
    }

    fn release_input(&mut self) {
        if let Some(session) = &self.session {
            if let (Some(button), Some(position)) = (self.pressed.take(), self.last_pointer) {
                if let Some((x, y)) = self.map_position(position, true) {
                    session.input(PreviewInput::PointerUp { x, y, button });
                }
            }
            if self.focused {
                session.input(PreviewInput::Focus { focused: false });
            }
        }
        self.focused = false;
        self.marked.clear();
        self.pressed = None;
    }

    fn poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let visible = (self.visible && window.is_visible() && window.is_window_active())
            || self
                .control_until
                .is_some_and(|until| until > Instant::now());
        if self.capture_visible != visible {
            self.capture_visible = visible;
            if !visible {
                self.release_input();
            }
            if let Some(session) = &self.session {
                session.set_visible(visible);
            }
        }
        let Some(session) = &self.session else {
            self.publish_control(
                PreviewState {
                    phase: if self.error.is_some() {
                        PreviewPhase::Error
                    } else {
                        PreviewPhase::Stopped
                    },
                    mode: self.spec.mode,
                    pid: None,
                    window_id: None,
                    windows: Vec::new(),
                    message: self
                        .error
                        .clone()
                        .unwrap_or_else(|| "Preview stopped; choose Run to start it.".into()),
                    warning: None,
                    frame: None,
                    ios: None,
                    logs: Vec::new(),
                },
                cx,
            );
            return;
        };
        let state = session.snapshot();
        self.publish_control(state.clone(), cx);
        if self.pending_launch
            && matches!(state.phase, PreviewPhase::Stopped | PreviewPhase::Exited)
        {
            self.session.take();
            self.pending_launch = false;
            self.run(cx);
            return;
        }
        let changed = self.state.as_ref().is_none_or(|old| {
            old.phase != state.phase
                || old.message != state.message
                || old.warning != state.warning
                || old.logs.len() != state.logs.len()
                || old.logs.last().map(|log| &log.text) != state.logs.last().map(|log| &log.text)
                || old
                    .windows
                    .iter()
                    .map(|window| (window.pid, window.window_id, &window.title))
                    .ne(state
                        .windows
                        .iter()
                        .map(|window| (window.pid, window.window_id, &window.title)))
        });
        let frame = if visible { state.frame.clone() } else { None };
        let running = !matches!(
            state.phase,
            PreviewPhase::Exited | PreviewPhase::Stopped | PreviewPhase::Error
        );
        self.state = Some(state);
        if !running && !self.pending_launch {
            self.set_fields_enabled(true, cx);
        }
        if let Some(frame) = frame {
            // A restarted transport may begin sequence numbering at one again.
            // The backend retains the same Arc until a genuinely new frame.
            if self
                .source_frame
                .as_ref()
                .is_none_or(|old| !Arc::ptr_eq(old, &frame))
                && self.decode.is_none()
            {
                self.decode_frame(frame, cx);
            }
        }
        if visible && self.spec.mode == PreviewMode::Cooperative {
            let bounds = self.viewport.get();
            if let Some(size) =
                bounded_viewport(bounds.size.width.as_f32(), bounds.size.height.as_f32())
            {
                if self.last_resize != Some(size) {
                    self.last_resize = Some(size);
                    if let Some(session) = &self.session {
                        session.input(PreviewInput::Resize {
                            width: size.0,
                            height: size.1,
                        });
                    }
                }
            }
        }
        if changed {
            cx.notify();
        }
    }

    fn clear_image(&mut self, cx: &mut App) {
        if let Some(image) = self.image.take() {
            release_image(image, cx);
        }
        self.image_sequence = None;
        self.source_frame = None;
        self.frame_size = None;
    }

    fn decode_frame(&mut self, frame: Arc<PreviewFrame>, cx: &mut Context<Self>) {
        let generation = self.generation;
        let sequence = frame.sequence;
        let dimensions = (frame.width, frame.height);
        let source = frame.clone();
        let work = cx
            .background_executor()
            .spawn(async move { decode_png(&frame) });
        self.decode = Some(cx.spawn(async move |this, cx| {
            let decoded = work.await;
            let _ = this.update(cx, |view, cx| {
                view.decode = None;
                if view.generation != generation {
                    return;
                }
                match decoded {
                    Ok(image) => {
                        view.clear_image(cx);
                        view.error = None;
                        view.image = Some(image);
                        view.image_sequence = Some(sequence);
                        view.frame_size = Some(dimensions);
                    }
                    Err(error) => {
                        view.error = Some(error);
                        // A malformed frame is tried only once.
                        view.image_sequence = Some(sequence);
                    }
                }
                view.source_frame = Some(source);
                cx.notify();
            });
        }));
    }

    fn map_position(&self, point: Point<Pixels>, clamp: bool) -> Option<(f32, f32)> {
        let (width, height) = self.frame_size?;
        let bounds = self.viewport.get();
        map_point(
            (
                bounds.origin.x.as_f32(),
                bounds.origin.y.as_f32(),
                bounds.size.width.as_f32(),
                bounds.size.height.as_f32(),
            ),
            (width, height),
            (point.x.as_f32(), point.y.as_f32()),
            clamp,
        )
    }

    fn accepts_input(&self, window: &Window) -> bool {
        self.visible
            && self.capture_visible
            && window.is_visible()
            && window.is_window_active()
            && self.is_running()
            && self.frame_size.is_some()
    }

    fn send(&self, input: PreviewInput) {
        if let Some(session) = &self.session {
            session.input(input);
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.accepts_input(window) || !self.focus.is_focused(window) {
            return;
        }
        // The cooperative app has an in-memory platform clipboard. Plain text
        // paste comes from RiWork's native clipboard instead.
        if self.spec.mode != PreviewMode::WindowCapture
            && event.keystroke.key == "v"
            && !event.keystroke.modifiers.alt
            && !event.keystroke.modifiers.shift
            && if cfg!(target_os = "macos") {
                event.keystroke.modifiers.platform
            } else {
                event.keystroke.modifiers.control
            }
        {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                self.send(PreviewInput::Text { text });
                cx.stop_propagation();
                window.prevent_default();
                return;
            }
        }
        // Printable text is committed through the native input handler, which
        // also handles alternate keyboard layouts and composed Unicode.
        if is_text_key(&event.keystroke) {
            return;
        }
        self.send(PreviewInput::Key {
            key: event.keystroke.unparse(),
        });
        cx.stop_propagation();
        window.prevent_default();
    }

    fn key_up(&mut self, event: &KeyUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.accepts_input(window)
            || !self.focus.is_focused(window)
            || is_text_key(&event.keystroke)
        {
            return;
        }
        if self.spec.mode != PreviewMode::WindowCapture {
            self.send(PreviewInput::KeyUp {
                key: event.keystroke.unparse(),
            });
        }
        cx.stop_propagation();
    }

    fn surface(&self, window: &Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let viewport = self.viewport.clone();
        let input_view = cx.entity();
        let focus = self.focus.clone();
        let colors = theme::palette(cx);
        let mut surface = div()
            .id("app-preview-surface")
            .track_focus(&self.focus)
            .key_context("AppPreview")
            .relative()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .bg(rgb(colors.bg))
            .border_1()
            .border_color(rgb(if self.focus.is_focused(window) {
                colors.focus
            } else {
                colors.divider
            }))
            .on_key_down(cx.listener(Self::key_down))
            .on_key_up(cx.listener(Self::key_up));
        for button in [MouseButton::Left, MouseButton::Right, MouseButton::Middle] {
            surface = surface
                .on_mouse_down(
                    button,
                    cx.listener(move |view, event: &gpui::MouseDownEvent, window, cx| {
                        if !view.accepts_input(window) {
                            return;
                        }
                        let Some((x, y)) = view.map_position(event.position, false) else {
                            return;
                        };
                        let button = preview_button(button);
                        view.focus.focus(window, cx);
                        if !view.focused {
                            view.send(PreviewInput::Focus { focused: true });
                            view.focused = true;
                        }
                        view.last_pointer = Some(event.position);
                        if view.spec.mode != PreviewMode::WindowCapture {
                            view.pressed = Some(button);
                            view.send(PreviewInput::PointerDown { x, y, button });
                        } else {
                            view.send(PreviewInput::Click { x, y, button });
                        }
                        cx.stop_propagation();
                        window.prevent_default();
                    }),
                )
                .on_mouse_up(
                    button,
                    cx.listener(move |view, event: &gpui::MouseUpEvent, window, cx| {
                        if !view.accepts_input(window)
                            || view.spec.mode == PreviewMode::WindowCapture
                        {
                            return;
                        }
                        if view.pressed.take().is_some() {
                            if let Some((x, y)) = view.map_position(event.position, true) {
                                view.send(PreviewInput::PointerUp {
                                    x,
                                    y,
                                    button: preview_button(button),
                                });
                            }
                            cx.stop_propagation();
                        }
                    }),
                )
                .on_mouse_up_out(
                    button,
                    cx.listener(move |view, event: &gpui::MouseUpEvent, _, cx| {
                        if view.pressed.take().is_some() {
                            if let Some((x, y)) = view.map_position(event.position, true) {
                                view.send(PreviewInput::PointerUp {
                                    x,
                                    y,
                                    button: preview_button(button),
                                });
                            }
                            cx.stop_propagation();
                        }
                    }),
                );
        }
        surface
            .on_mouse_move(
                cx.listener(|view, event: &gpui::MouseMoveEvent, window, _| {
                    if !view.accepts_input(window) || view.spec.mode == PreviewMode::WindowCapture {
                        return;
                    }
                    if let Some((x, y)) = view.map_position(event.position, view.pressed.is_some())
                    {
                        view.last_pointer = Some(event.position);
                        view.send(PreviewInput::PointerMove { x, y });
                    }
                }),
            )
            .on_scroll_wheel(
                cx.listener(|view, event: &gpui::ScrollWheelEvent, window, cx| {
                    if !view.accepts_input(window) {
                        return;
                    }
                    if let Some((x, y)) = view.map_position(event.position, false) {
                        let delta = event.delta.pixel_delta(px(20.));
                        view.send(PreviewInput::Scroll {
                            x,
                            y,
                            delta_x: delta.x.as_f32(),
                            delta_y: delta.y.as_f32(),
                        });
                        cx.stop_propagation();
                    }
                }),
            )
            .when_some(self.image.clone(), |surface, image| {
                surface.child(
                    img(image)
                        .absolute()
                        .size_full()
                        .object_fit(ObjectFit::Contain),
                )
            })
            .when(self.image.is_none(), |surface| {
                surface.child(
                    div()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .p(ui_text::space(20.))
                        .text_color(rgb(colors.muted))
                        .child(if self.session.is_some() {
                            "Waiting for the app’s first frame…"
                        } else {
                            "Configure your app, then choose Run."
                        }),
                )
            })
            .child(
                canvas(
                    move |bounds, _, _| viewport.set(bounds),
                    move |bounds, _, window, cx| {
                        window.handle_input(
                            &focus,
                            ElementInputHandler::new(bounds, input_view),
                            cx,
                        );
                    },
                )
                .absolute()
                .size_full(),
            )
            .into_any_element()
    }
}

impl Render for AppPreviewView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme::palette(cx);
        let running = self.is_running();
        let status = self.pending_launch.then(|| "Stopping the previous app before launching the new command…".into())
            .or_else(|| self.error.clone()).or_else(|| self.state.as_ref().map(|state| state.message.clone()))
            .unwrap_or_else(|| match self.spec.mode {
                PreviewMode::Cooperative => "Hidden GPUI app. Add riwork-gpui-preview to your app and start it in preview mode.".into(),
                PreviewMode::WindowCapture => "Cua window capture. A normal app window is required; hiding its Dock entry is app-specific.".into(),
                PreviewMode::IosSimulator => "Build and run an iOS app in a dedicated simulator. Stream the device here without opening Simulator.app.".into(),
            });
        let mode = self.spec.mode;
        let logs = self.state.as_ref().map(|state| &state.logs);
        let log_count = logs.map_or(0, Vec::len);
        let warning = self.state.as_ref().and_then(|state| state.warning.clone());
        let windows = self
            .state
            .as_ref()
            .filter(|state| {
                state.phase == PreviewPhase::WaitingForWindow && state.windows.len() > 1
            })
            .map(|state| state.windows.clone())
            .unwrap_or_default();
        div().id("app-preview-tab").size_full().min_w_0().min_h_0()
            .flex().flex_col().bg(rgb(colors.bg)).text_color(rgb(colors.text))
            .font_family(ui_text::ui_family()).text_size(ui_text::text(11.))
            .child(div().flex_none().p(ui_text::space(10.)).flex().flex_col().gap(ui_text::space(6.))
                .border_b_1().border_color(rgb(colors.divider))
                .child(div().flex().flex_wrap().items_center().gap(ui_text::space(6.))
                    .child(div().flex_1().min_w(ui_text::space(90.)).font_weight(gpui::FontWeight::SEMIBOLD).child("App Preview"))
                    .children([(PreviewMode::Cooperative, "Hidden GPUI"), (PreviewMode::IosSimulator, "iOS Simulator"), (PreviewMode::WindowCapture, "Cua window")].into_iter().enumerate().map(|(index, (candidate, label))| {
                        behavior::button(("app-preview-mode", index), label, if mode == candidate { Button::Primary } else { Button::Secondary }, colors)
                            .disabled(running).on_click(cx.listener(move |view, _, _, cx| {
                                if view.is_running() { return; }
                                view.spec.mode = candidate;
                                if candidate == PreviewMode::IosSimulator { view.spec.ios.get_or_insert_with(IosPreviewSpec::default); }
                                view.configured(cx);
                                cx.notify();
                            }))
                    }))
                    .child(behavior::button("app-preview-run", "Run", Button::Primary, colors).disabled(running)
                        .on_click(cx.listener(|view, _, _, cx| { if !view.is_running() { view.run(cx); } })))
                    .child(behavior::button("app-preview-restart", "Restart", Button::Secondary, colors).disabled(self.session.is_none())
                        .on_click(cx.listener(|view, _, _, cx| view.restart(cx))))
                    .child(behavior::button("app-preview-stop", "Stop", Button::Secondary, colors).disabled(!self.can_stop())
                        .on_click(cx.listener(|view, _, _, cx| view.stop(cx))))
                    .when(mode == PreviewMode::WindowCapture && self.session.is_some(), |row| row.child(
                        behavior::button("app-preview-show", "Show window", Button::Secondary, colors)
                            .on_click(cx.listener(|view, _, _, _| view.send(PreviewInput::ShowWindow))),
                    )))
                .when(mode != PreviewMode::IosSimulator, |header| header.child(div().flex().flex_wrap().gap(ui_text::space(6.))
                    .child(div().flex_1().min_w(ui_text::space(160.)).child(text_input::input("app-preview-command", &self.command, window, cx)))
                    .child(div().flex_1().min_w(ui_text::space(160.)).child(text_input::input("app-preview-cwd", &self.cwd, window, cx)))))
                .when(mode == PreviewMode::IosSimulator, |header| header
                    .child(div().child(text_input::input("app-preview-ios-cwd", &self.cwd, window, cx)))
                    .child(div().flex().flex_wrap().gap(ui_text::space(6.)).children([
                        ("app-preview-ios-scheme", "Scheme", &self.ios_scheme),
                        ("app-preview-ios-project", "Project / workspace", &self.ios_project),
                    ].into_iter().map(|(id,label,input)| div().flex_1().min_w(ui_text::space(170.)).flex().flex_col().gap(ui_text::space(3.)).child(label).child(text_input::input(id,input,window,cx)))))
                    .child(div().flex().flex_wrap().gap(ui_text::space(6.)).children([
                        ("app-preview-ios-device", "Device", &self.ios_device),
                        ("app-preview-ios-runtime", "iOS version", &self.ios_runtime),
                    ].into_iter().map(|(id,label,input)| div().flex_1().min_w(ui_text::space(170.)).flex().flex_col().gap(ui_text::space(3.)).child(label).child(text_input::input(id,input,window,cx))))))
                .child(div().text_size(ui_text::text(10.)).text_color(rgb(if self.error.is_some() { colors.gold } else { colors.muted })).child(status))
                .when_some(warning, |header, warning| header.child(div().text_size(ui_text::text(10.)).text_color(rgb(colors.gold)).child(warning))))
            .when(!windows.is_empty(), |view| view.child(div().flex_none().flex().flex_wrap().gap(ui_text::space(6.)).p(ui_text::space(8.))
                .child("Choose an app window:")
                .children(windows.into_iter().enumerate().map(|(index, window)| {
                    behavior::button(("app-preview-window", index), if window.title.is_empty() { format!("Window {}", window.window_id) } else { window.title }, Button::Secondary, colors)
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.clear_image(cx);
                            if let Some(session) = &view.session { session.select_window(window.pid, window.window_id); }
                        }))
                }))))
            .child(self.surface(window, cx))
            .child(div().flex_none().border_t_1().border_color(rgb(colors.divider)).px(ui_text::space(10.)).py(ui_text::space(4.))
                .flex().flex_wrap().items_center().gap(ui_text::space(8.))
                .child(behavior::button("app-preview-logs", format!("{} Logs ({log_count})", if self.show_logs { "▾" } else { "▸" }), Button::Secondary, colors)
                    .on_click(cx.listener(|view, _, _, cx| { view.show_logs = !view.show_logs; cx.notify(); })))
                .child(div().flex_1().min_w(ui_text::space(180.)).text_size(ui_text::text(10.)).text_color(rgb(colors.muted)).child(
                    if mode == PreviewMode::IosSimulator { "Tap and drag on the device to interact. The simulator belongs to this preview." }
                    else if mode == PreviewMode::Cooperative { "Click the preview to type. Capture pauses while the tab or window is inactive." }
                    else { "Input targets only the app launched here. RiWork Cua setup supplies capture and input permissions." },
                )))
            .when(self.show_logs, |view| view.child(div().id("app-preview-log-output")
                .flex_none().h(ui_text::space(160.)).max_h(ui_text::space(180.)).min_h_0().overflow_y_scroll()
                .p(ui_text::space(8.)).font_family(ui_text::mono_family()).text_size(ui_text::text(10.))
                .children(logs.into_iter().flat_map(|logs| logs.iter().rev().take(MAX_LOG_LINES).rev()).enumerate().map(|(index, log)| {
                    div().id(("app-preview-log", index)).text_color(rgb(if log.stderr { colors.gold } else { colors.muted }))
                        .child(clean_log(&log.text))
                }))))
    }
}

impl Focusable for AppPreviewView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// A stream has no locally editable document. Only committed text is forwarded;
/// native IME composition remains local until the platform commits it.
impl EntityInputHandler for AppPreviewView {
    fn text_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        Some(self.marked.clone())
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        window: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        self.accepts_input(window).then_some(UTF16Selection {
            range: self.marked.encode_utf16().count()..self.marked.encode_utf16().count(),
            reversed: false,
        })
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        (!self.marked.is_empty()).then(|| 0..self.marked.encode_utf16().count())
    }
    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked.clear();
    }
    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.accepts_input(window) && self.focus.is_focused(window) && !text.is_empty() {
            self.send(PreviewInput::Text { text: text.into() });
            cx.stop_propagation();
        }
        self.marked.clear();
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        if self.accepts_input(window) {
            self.marked = text.into();
        }
    }
    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(bounds)
    }
    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(0)
    }
    fn accepts_text_input(&self, window: &mut Window, _: &mut Context<Self>) -> bool {
        self.accepts_input(window) && self.focus.is_focused(window)
    }
}

fn release_image(image: Arc<RenderImage>, cx: &mut App) {
    cx.defer(move |cx| cx.drop_image(image, None));
}

fn preview_button(button: MouseButton) -> PreviewMouseButton {
    match button {
        MouseButton::Right => PreviewMouseButton::Right,
        MouseButton::Middle => PreviewMouseButton::Middle,
        _ => PreviewMouseButton::Left,
    }
}

fn is_text_key(key: &gpui::Keystroke) -> bool {
    key.key_char
        .as_ref()
        .is_some_and(|text| !text.is_empty() && !text.chars().any(char::is_control))
        && !key.modifiers.control
        && !key.modifiers.platform
}

fn bounded_viewport(width: f32, height: f32) -> Option<(u32, u32)> {
    if !width.is_finite() || !height.is_finite() || width < 32. || height < 32. {
        return None;
    }
    let scale = (1920. / width).min(1200. / height).min(1.);
    Some((
        (width * scale).round().max(32.) as u32,
        (height * scale).round().max(32.) as u32,
    ))
}

/// Match `ObjectFit::Contain`, rejecting letterbox clicks; a captured drag can
/// clamp outside points so releasing outside the frame cannot leave a button held.
fn map_point(
    bounds: (f32, f32, f32, f32),
    frame: (u32, u32),
    point: (f32, f32),
    clamp: bool,
) -> Option<(f32, f32)> {
    let (left, top, width, height) = bounds;
    if frame.0 == 0
        || frame.1 == 0
        || width <= 0.
        || height <= 0.
        || [left, top, width, height, point.0, point.1]
            .iter()
            .any(|value| !value.is_finite())
    {
        return None;
    }
    let scale = (width / frame.0 as f32).min(height / frame.1 as f32);
    let origin_x = left + (width - frame.0 as f32 * scale) / 2.;
    let origin_y = top + (height - frame.1 as f32 * scale) / 2.;
    let x = (point.0 - origin_x) / scale;
    let y = (point.1 - origin_y) / scale;
    if !clamp && (x < 0. || y < 0. || x >= frame.0 as f32 || y >= frame.1 as f32) {
        return None;
    }
    Some((
        x.clamp(0., frame.0.saturating_sub(1) as f32),
        y.clamp(0., frame.1.saturating_sub(1) as f32),
    ))
}

fn decode_png(frame: &PreviewFrame) -> Result<Arc<RenderImage>, String> {
    if frame.png.len() > MAX_FRAME_BYTES
        || frame.width == 0
        || frame.height == 0
        || frame.width > MAX_FRAME_EDGE
        || frame.height > MAX_FRAME_EDGE
    {
        return Err("The app sent a frame larger than the preview limit.".into());
    }
    let mut reader = ImageReader::with_format(Cursor::new(&frame.png), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_FRAME_EDGE);
    limits.max_image_height = Some(MAX_FRAME_EDGE);
    limits.max_alloc = Some(32 * 1024 * 1024);
    reader.limits(limits);
    let mut pixels = reader
        .decode()
        .map_err(|error| format!("Cannot decode the app frame: {error}"))?
        .into_rgba8();
    if pixels.dimensions() != (frame.width, frame.height) {
        return Err("The app frame dimensions do not match its metadata.".into());
    }
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Ok(Arc::new(RenderImage::new([Frame::new(pixels)])))
}

fn clean_log(text: &str) -> String {
    text.chars()
        .filter(|ch| !ch.is_control() || matches!(ch, '\t' | '\n'))
        .take(2000)
        .collect()
}

#[cfg(test)]
mod tests;

#[cfg(feature = "app-preview-fixture")]
mod fixture;
#[cfg(feature = "app-preview-fixture")]
pub use fixture::render_fixture_if_requested;
