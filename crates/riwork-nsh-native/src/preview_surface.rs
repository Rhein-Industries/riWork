//! Native preview transport. PNG and BGRA never enter the JSON/JS bridge.
//! One bounded decode is in flight per surface; restart/close invalidate both
//! worker completion and queued main-thread publication. The view owns at most
//! one decoded image; CGDataProvider retains that same allocation.
use crate::{
    app_preview::Session,
    preview_input::{Outcome, Policy, SurfaceEvent},
};
use image::ImageReader;
use riwork_core::app_preview::{
    MAX_PREVIEW_DIMENSION, MAX_PREVIEW_MESSAGE_BYTES, PreviewInput, PreviewMode,
};
use riwork_engine::app_preview::{PreviewFrame, PreviewPhase, validate_input};
use std::{
    collections::HashMap,
    io::Cursor,
    sync::{Arc, Mutex, OnceLock, Weak},
    time::{Duration, Instant},
};

pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}
#[derive(Clone)]
pub struct ViewSnapshot {
    pub revision: u64,
    pub frame: Option<Arc<DecodedFrame>>,
    pub running: bool,
    pub accepts_input: bool,
    pub focused: bool,
    pub mode: PreviewMode,
}
struct State {
    session: Option<Arc<dyn Session>>,
    mode: PreviewMode,
    generation: u64,
    revision: u64,
    closed: bool,
    visible: bool,
    window_visible: bool,
    window_active: bool,
    capture_visible: bool,
    control_until: Option<Instant>,
    running: bool,
    attempted: Option<Arc<PreviewFrame>>,
    decoding: bool,
    frame: Option<Arc<DecodedFrame>>,
    error: Option<String>,
    policy: Policy,
}
impl State {
    fn send(&self, outcome: Outcome) -> bool {
        if let Some(session) = &self.session {
            for input in outcome.inputs {
                if validate_input(&input).is_ok() {
                    session.input(input);
                }
            }
        }
        outcome.request_focus
    }
    fn configure(&mut self, now: Instant) {
        let capture = !self.closed
            && ((self.visible && self.window_visible && self.window_active)
                || self.control_until.is_some_and(|until| until > now));
        let changed = self.capture_visible != capture;
        self.capture_visible = capture;
        if changed && capture {
            if let Some(session) = &self.session {
                session.visible(true);
            }
        }
        let outcome = self.policy.configure(
            self.mode,
            self.frame.as_ref().map(|f| (f.width, f.height)),
            self.visible,
            capture,
            self.running && !self.closed,
        );
        self.send(outcome);
        if changed && !capture {
            if let Some(session) = &self.session {
                session.visible(false);
            }
        }
    }
    fn reset(&mut self) {
        let outcome = self.policy.reset();
        self.send(outcome);
        self.generation = self.generation.wrapping_add(1);
        self.revision = self.revision.wrapping_add(1);
        self.session = None;
        self.control_until = None;
        self.running = false;
        self.attempted = None;
        self.frame = None;
        self.error = None;
        self.configure(Instant::now());
    }
    fn snapshot(&self) -> ViewSnapshot {
        ViewSnapshot {
            revision: self.revision,
            frame: self.frame.clone(),
            running: self.running,
            accepts_input: self.policy.accepts_input(),
            focused: self.policy.focused(),
            mode: self.mode,
        }
    }
}
static SURFACES: OnceLock<Mutex<HashMap<String, Weak<Surface>>>> = OnceLock::new();
fn surfaces() -> &'static Mutex<HashMap<String, Weak<Surface>>> {
    SURFACES.get_or_init(Default::default)
}

pub struct Surface {
    id: String,
    state: Mutex<State>,
}
impl Surface {
    pub(crate) fn new() -> Arc<Self> {
        let surface = Arc::new(Self {
            id: uuid::Uuid::new_v4().to_string(),
            state: Mutex::new(State {
                session: None,
                mode: PreviewMode::Cooperative,
                generation: 0,
                revision: 0,
                closed: false,
                visible: false,
                window_visible: true,
                window_active: true,
                capture_visible: false,
                control_until: None,
                running: false,
                attempted: None,
                decoding: false,
                frame: None,
                error: None,
                policy: Policy::new(PreviewMode::Cooperative),
            }),
        });
        surfaces()
            .lock()
            .unwrap()
            .insert(surface.id.clone(), Arc::downgrade(&surface));
        surface
    }
    pub fn lookup(id: &str) -> Option<Arc<Self>> {
        let surface = surfaces().lock().unwrap().get(id)?.upgrade()?;
        if surface.state.lock().unwrap().closed {
            None
        } else {
            Some(surface)
        }
    }
    pub(crate) fn id(&self) -> &str {
        &self.id
    }
    pub(crate) fn attach(&self, session: Arc<dyn Session>, mode: PreviewMode) {
        let mut state = self.state.lock().unwrap();
        if state.closed {
            session.stop();
            return;
        }
        state.reset();
        state.mode = mode;
        session.visible(state.capture_visible);
        state.session = Some(session);
    }
    pub(crate) fn reset(&self) {
        self.state.lock().unwrap().reset();
    }
    pub(crate) fn close(&self) {
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return;
        }
        state.closed = true;
        if let Some(session) = state.session.clone() {
            let outcome = state.policy.release();
            state.send(outcome);
            session.stop();
        }
        state.reset();
        surfaces().lock().unwrap().remove(&self.id);
    }
    #[cfg(test)]
    fn is_current(&self, revision: u64) -> bool {
        let state = self.state.lock().unwrap();
        !state.closed && state.revision == revision
    }
    /// Linearize native image installation with reset/close. The callback gets
    /// the current allocation, never pixels captured by an earlier UI task.
    /// A closed surface may publish its current empty frame to clear an image
    /// that was installed before close. The callback must not emit input or
    /// otherwise re-enter this surface; AppKit state/focus updates run outside.
    pub fn with_current_frame(
        &self,
        revision: u64,
        install: impl FnOnce(Option<Arc<DecodedFrame>>),
    ) -> bool {
        let state = self.state.lock().unwrap();
        if state.revision != revision {
            return false;
        }
        install(if state.closed {
            None
        } else {
            state.frame.clone()
        });
        true
    }
    pub fn set_visible(&self, visible: bool) {
        let mut state = self.state.lock().unwrap();
        state.visible = visible;
        state.configure(Instant::now());
    }
    pub fn release(&self) {
        let mut state = self.state.lock().unwrap();
        let outcome = state.policy.release();
        state.send(outcome);
    }
    /// Only small platform events enter this path; native frames never do.
    pub fn native_event(&self, event: serde_json::Value) -> bool {
        let Ok(event) = serde_json::from_value::<SurfaceEvent>(event) else {
            return false;
        };
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return false;
        }
        if let SurfaceEvent::Window { visible, active } = &event {
            state.window_visible = *visible;
            state.window_active = *active;
        }
        let outcome = state.policy.handle(event);
        let focus = state.send(outcome);
        state.configure(Instant::now());
        focus
    }
    pub(crate) fn error(&self) -> Option<String> {
        self.state.lock().unwrap().error.clone()
    }
    /// A control request keeps capture alive for five seconds; local input
    /// still requires a shown/active native surface. Stop/restart clears it.
    pub(crate) fn control_refresh(&self, now: Instant) -> Result<(), String> {
        let (session, generation) = {
            let state = self.state.lock().unwrap();
            (
                state
                    .session
                    .clone()
                    .filter(|_| !state.closed)
                    .ok_or("This preview has not been started.")?,
                state.generation,
            )
        };
        let snapshot = session.snapshot();
        if snapshot.frame.is_none()
            || matches!(
                snapshot.phase,
                PreviewPhase::Stopped | PreviewPhase::Exited | PreviewPhase::Error
            )
        {
            return Err(
                "Wait for a running preview to produce a frame before capturing it.".into(),
            );
        }
        let mut state = self.state.lock().unwrap();
        if state.closed || state.generation != generation {
            return Err("unavailable: preview changed during capture request".into());
        }
        state.control_until = Some(now + Duration::from_secs(5));
        state.configure(now);
        session.input(PreviewInput::Refresh);
        Ok(())
    }
    pub(crate) fn control_start(&self, now: Instant) {
        let mut state = self.state.lock().unwrap();
        if !state.closed {
            state.control_until = Some(now + Duration::from_secs(5));
            state.configure(now);
        }
    }
    /// Worker-only. Never hold the view's lock during engine snapshot or PNG
    /// decoding; input and teardown cannot wait behind a decoder or a launch.
    pub fn poll(&self) -> ViewSnapshot {
        self.poll_with(decode_png)
    }
    fn poll_with(
        &self,
        decode: impl FnOnce(&PreviewFrame) -> Result<DecodedFrame, String>,
    ) -> ViewSnapshot {
        let (session, generation) = {
            let mut state = self.state.lock().unwrap();
            state.configure(Instant::now());
            let Some(session) = state.session.clone().filter(|_| !state.closed) else {
                return state.snapshot();
            };
            (session, state.generation)
        };
        let snapshot = session.snapshot();
        let work = {
            let mut state = self.state.lock().unwrap();
            if state.closed || state.generation != generation {
                return state.snapshot();
            }
            state.running = matches!(
                snapshot.phase,
                PreviewPhase::Starting
                    | PreviewPhase::WaitingForWindow
                    | PreviewPhase::Streaming
                    | PreviewPhase::Paused
            );
            if snapshot.frame.is_none() && state.frame.is_some() {
                let outcome = state.policy.release();
                state.send(outcome);
                state.frame = None;
                state.attempted = None;
                state.revision = state.revision.wrapping_add(1);
            }
            state.configure(Instant::now());
            if state.decoding {
                return state.snapshot();
            }
            let Some(frame) = snapshot.frame else {
                return state.snapshot();
            };
            if state
                .attempted
                .as_ref()
                .is_some_and(|old| Arc::ptr_eq(old, &frame))
            {
                return state.snapshot();
            }
            state.attempted = Some(frame.clone());
            state.decoding = true;
            frame
        };
        let decoded = decode(&work);
        let mut state = self.state.lock().unwrap();
        // Keep the single in-flight slot occupied across generation resets.
        state.decoding = false;
        if !state.closed && state.generation == generation {
            match decoded {
                Ok(frame) => {
                    state.frame = Some(Arc::new(frame));
                    state.error = None;
                }
                Err(error) => {
                    state.error = Some(error);
                }
            }
            state.revision = state.revision.wrapping_add(1);
            state.configure(Instant::now());
        }
        state.snapshot()
    }
}
impl Drop for Surface {
    fn drop(&mut self) {
        if let Ok(state) = self.state.get_mut() {
            let outcome = state.policy.release();
            state.send(outcome);
        }
        surfaces().lock().unwrap().remove(&self.id);
    }
}

fn decode_png(frame: &PreviewFrame) -> Result<DecodedFrame, String> {
    if frame.png.len() > MAX_PREVIEW_MESSAGE_BYTES
        || frame.width == 0
        || frame.height == 0
        || frame.width > MAX_PREVIEW_DIMENSION
        || frame.height > MAX_PREVIEW_DIMENSION
    {
        return Err("The app sent a frame larger than the preview limit.".into());
    }
    let mut reader = ImageReader::with_format(Cursor::new(&frame.png), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_PREVIEW_DIMENSION);
    limits.max_image_height = Some(MAX_PREVIEW_DIMENSION);
    limits.max_alloc = Some(32 * 1024 * 1024);
    reader.limits(limits);
    let mut pixels = reader
        .decode()
        .map_err(|e| format!("Cannot decode the app frame: {e}"))?
        .into_rgba8();
    if pixels.dimensions() != (frame.width, frame.height) {
        return Err("The app frame dimensions do not match its metadata.".into());
    }
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    Ok(DecodedFrame {
        width: frame.width,
        height: frame.height,
        bgra: pixels.into_raw(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{
        ExtendedColorType, ImageEncoder,
        codecs::png::{CompressionType, FilterType, PngEncoder},
    };
    use riwork_core::app_preview::PreviewMouseButton;
    use riwork_engine::app_preview::PreviewState;
    use serde_json::json;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };

    #[derive(Clone, Debug, PartialEq)]
    enum Call {
        Visible(bool),
        Input(PreviewInput),
        Stop,
        Restart,
        Select(u32, u64),
    }

    struct FakeSession {
        state: Mutex<PreviewState>,
        calls: Mutex<Vec<Call>>,
        snapshot_gate: Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>>,
    }
    impl FakeSession {
        fn new(frame: Option<Arc<PreviewFrame>>) -> Arc<Self> {
            Arc::new(Self {
                state: Mutex::new(PreviewState {
                    phase: PreviewPhase::Streaming,
                    mode: PreviewMode::Cooperative,
                    pid: Some(424242),
                    window_id: None,
                    windows: vec![],
                    message: "private in-memory fixture".into(),
                    warning: None,
                    frame,
                    logs: vec![],
                    ios: None,
                }),
                calls: Mutex::new(Vec::new()),
                snapshot_gate: Mutex::new(None),
            })
        }
        fn take_calls(&self) -> Vec<Call> {
            std::mem::take(&mut *self.calls.lock().unwrap())
        }
        fn gate_next_snapshot(&self) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
            let (entered_tx, entered_rx) = mpsc::channel();
            let (resume_tx, resume_rx) = mpsc::channel();
            *self.snapshot_gate.lock().unwrap() = Some((entered_tx, resume_rx));
            (entered_rx, resume_tx)
        }
    }
    impl Session for FakeSession {
        fn snapshot(&self) -> PreviewState {
            let snapshot = self.state.lock().unwrap().clone();
            let gate = self.snapshot_gate.lock().unwrap().take();
            if let Some((entered, resume)) = gate {
                entered.send(()).unwrap();
                resume.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            snapshot
        }
        fn visible(&self, visible: bool) {
            self.calls.lock().unwrap().push(Call::Visible(visible));
        }
        fn input(&self, input: PreviewInput) {
            self.calls.lock().unwrap().push(Call::Input(input));
        }
        fn stop(&self) {
            self.calls.lock().unwrap().push(Call::Stop);
        }
        fn restart(&self) {
            self.calls.lock().unwrap().push(Call::Restart);
        }
        fn select_window(&self, pid: u32, window: u64) {
            self.calls.lock().unwrap().push(Call::Select(pid, window));
        }
    }

    fn png(width: u32, height: u32, rgba: &[u8], sequence: u64) -> Arc<PreviewFrame> {
        let mut bytes = Vec::new();
        PngEncoder::new_with_quality(&mut bytes, CompressionType::Default, FilterType::NoFilter)
            .write_image(rgba, width, height, ExtendedColorType::Rgba8)
            .unwrap();
        Arc::new(PreviewFrame {
            png: bytes,
            width,
            height,
            sequence,
        })
    }

    fn pixel(red: u8, sequence: u64) -> Arc<PreviewFrame> {
        png(1, 1, &[red, 20, 30, 255], sequence)
    }

    fn attached(frame: Option<Arc<PreviewFrame>>) -> (Arc<Surface>, Arc<FakeSession>) {
        let session = FakeSession::new(frame);
        let surface = Surface::new();
        surface.attach(session.clone(), PreviewMode::Cooperative);
        (surface, session)
    }

    fn shown() -> (Arc<Surface>, Arc<FakeSession>) {
        let (surface, session) = attached(Some(pixel(10, 1)));
        surface.native_event(json!({"kind":"layout","width":100,"height":100}));
        surface.native_event(json!({"kind":"window","visible":true,"active":true}));
        surface.set_visible(true);
        let snapshot = surface.poll();
        assert!(snapshot.accepts_input);
        session.take_calls();
        (surface, session)
    }

    fn hold(surface: &Surface) {
        assert!(
            surface.native_event(json!({"kind":"pointer_down","x":50,"y":50,"button":"middle"}))
        );
        surface.native_event(json!({"kind":"key","key":"shift-tab","text_key":false}));
    }

    fn releases() -> Vec<Call> {
        vec![
            Call::Input(PreviewInput::PointerUp {
                x: 0.,
                y: 0.,
                button: PreviewMouseButton::Middle,
            }),
            Call::Input(PreviewInput::KeyUp {
                key: "shift-tab".into(),
            }),
            Call::Input(PreviewInput::Focus { focused: false }),
        ]
    }

    #[test]
    fn decoder_changes_rgba_to_bgra_and_preserves_alpha_without_json_transport() {
        let frame = png(2, 1, &[10, 20, 30, 255, 40, 50, 60, 128], 1);
        let decoded = decode_png(&frame).unwrap();
        assert_eq!((decoded.width, decoded.height), (2, 1));
        assert_eq!(decoded.bgra, [30, 20, 10, 255, 60, 50, 40, 128]);
        let (surface, _) = attached(Some(frame));
        let first = surface.poll().frame.unwrap();
        let next = surface.poll().frame.unwrap();
        assert!(
            Arc::ptr_eq(&first, &next),
            "unchanged frames retain the native allocation"
        );
        assert_eq!(first.bgra.as_ptr(), next.bgra.as_ptr());
    }

    #[test]
    fn decoder_enforces_compressed_byte_edge_and_metadata_limits() {
        let valid = pixel(10, 1);
        for (width, height) in [
            (0, 1),
            (1, 0),
            (MAX_PREVIEW_DIMENSION + 1, 1),
            (1, MAX_PREVIEW_DIMENSION + 1),
        ] {
            let frame = PreviewFrame {
                png: valid.png.clone(),
                width,
                height,
                sequence: 1,
            };
            assert!(
                decode_png(&frame)
                    .err()
                    .unwrap()
                    .contains("larger than the preview limit")
            );
        }
        let frame = PreviewFrame {
            png: vec![0; MAX_PREVIEW_MESSAGE_BYTES + 1],
            width: 1,
            height: 1,
            sequence: 1,
        };
        assert!(
            decode_png(&frame)
                .err()
                .unwrap()
                .contains("larger than the preview limit")
        );
        let mut exact_limit = valid.as_ref().clone();
        exact_limit.png.resize(MAX_PREVIEW_MESSAGE_BYTES, 0);
        assert_eq!(decode_png(&exact_limit).unwrap().bgra, [30, 20, 10, 255]);
        let mut mismatch = valid.as_ref().clone();
        mismatch.width = 2;
        assert!(
            decode_png(&mismatch)
                .err()
                .unwrap()
                .contains("dimensions do not match")
        );
        // The dimensions in the encoded PNG must be bounded too; trusting only
        // the transport metadata would allocate based on the malicious header.
        let too_wide = png(
            MAX_PREVIEW_DIMENSION + 1,
            1,
            &vec![0; (MAX_PREVIEW_DIMENSION as usize + 1) * 4],
            1,
        );
        let hidden_dimensions = PreviewFrame {
            png: too_wide.png.clone(),
            width: 1,
            height: 1,
            sequence: 1,
        };
        assert!(
            decode_png(&hidden_dimensions)
                .err()
                .unwrap()
                .contains("Cannot decode")
        );
    }

    #[test]
    fn maximum_edge_frame_has_a_bounded_native_bgra_allocation() {
        let edge = MAX_PREVIEW_DIMENSION;
        let rgba = vec![127; edge as usize * edge as usize * 4];
        let frame = png(edge, edge, &rgba, 1);
        assert!(frame.png.len() <= MAX_PREVIEW_MESSAGE_BYTES);
        drop(rgba);
        let decoded = decode_png(&frame).unwrap();
        assert_eq!(decoded.bgra.len(), 16 * 1024 * 1024);
        assert!(decoded.bgra.len() <= 32 * 1024 * 1024);
        // This bounds the final native pixels. The production ImageReader has
        // a separate 32 MiB decoder allocation budget; it is not a claim about
        // aggregate process memory while old/new frames overlap.
    }

    #[test]
    fn a_malformed_frame_is_attempted_once_and_keeps_the_last_good_native_image() {
        let (surface, session) = attached(Some(pixel(10, 1)));
        let good = surface.poll().frame.unwrap();
        let bad = Arc::new(PreviewFrame {
            png: b"malformed PNG".to_vec(),
            width: 1,
            height: 1,
            sequence: 2,
        });
        session.state.lock().unwrap().frame = Some(bad.clone());
        let attempts = AtomicUsize::new(0);
        for _ in 0..3 {
            let snapshot = surface.poll_with(|frame| {
                attempts.fetch_add(1, Ordering::SeqCst);
                decode_png(frame)
            });
            assert!(Arc::ptr_eq(snapshot.frame.as_ref().unwrap(), &good));
        }
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert!(surface.error().unwrap().contains("Cannot decode"));
        // A restarted transport may reuse the sequence. Source Arc identity,
        // not the sequence alone, determines whether a new frame gets tried.
        session.state.lock().unwrap().frame = Some(pixel(99, 2));
        assert_eq!(surface.poll().frame.unwrap().bgra, [30, 20, 99, 255]);
        assert!(surface.error().is_none());
    }

    #[test]
    fn delayed_decode_cannot_publish_after_reset_or_free_its_single_flight_slot_early() {
        let (surface, old_session) = shown();
        hold(&surface);
        old_session.take_calls();
        old_session.state.lock().unwrap().frame = Some(pixel(11, 2));
        let old_revision = surface.state.lock().unwrap().revision;
        let (entered_tx, entered_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let worker_surface = surface.clone();
        let worker = std::thread::spawn(move || {
            worker_surface.poll_with(|frame| {
                entered_tx.send(()).unwrap();
                resume_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                decode_png(frame)
            })
        });
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        surface.reset();
        assert_eq!(old_session.take_calls(), releases());
        let next_session = FakeSession::new(Some(pixel(99, 1)));
        surface.attach(next_session.clone(), PreviewMode::Cooperative);
        assert!(!surface.is_current(old_revision));
        let waiting =
            surface.poll_with(|_| panic!("second decoder entered while old generation is blocked"));
        assert!(waiting.frame.is_none());
        assert!(surface.state.lock().unwrap().decoding);
        resume_tx.send(()).unwrap();
        let stale_completion = worker.join().unwrap();
        assert!(
            stale_completion.frame.is_none(),
            "old pixels must not resurrect"
        );
        assert!(!surface.state.lock().unwrap().decoding);
        let current = surface.poll();
        assert_eq!(current.frame.unwrap().bgra, [30, 20, 99, 255]);
        assert!(surface.is_current(current.revision));
        assert!(
            !current.focused,
            "a new generation cannot restore old focus"
        );
    }

    #[test]
    fn delayed_decode_cannot_publish_or_send_input_after_close() {
        let (surface, session) = shown();
        hold(&surface);
        session.take_calls();
        session.state.lock().unwrap().frame = Some(pixel(11, 2));
        let old_revision = surface.state.lock().unwrap().revision;
        let id = surface.id().to_string();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let worker_surface = surface.clone();
        let worker = std::thread::spawn(move || {
            worker_surface.poll_with(|frame| {
                entered_tx.send(()).unwrap();
                resume_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                decode_png(frame)
            })
        });
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        surface.close();
        let mut expected = releases();
        expected.push(Call::Stop);
        assert_eq!(session.take_calls(), expected);
        assert!(Surface::lookup(&id).is_none());
        assert!(!surface.is_current(old_revision));
        assert!(
            !surface.native_event(json!({"kind":"pointer_down","x":50,"y":50,"button":"left"}))
        );
        surface.close();
        resume_tx.send(()).unwrap();
        let snapshot = worker.join().unwrap();
        assert!(snapshot.frame.is_none());
        assert!(!snapshot.accepts_input);
        assert!(session.take_calls().is_empty());
    }

    #[test]
    fn hiding_releases_all_held_input_before_notifying_the_engine_of_visibility_false() {
        for event in ["tab", "window", "active"] {
            let (surface, session) = shown();
            hold(&surface);
            session.take_calls();
            match event {
                "tab" => surface.set_visible(false),
                "window" => {
                    surface.native_event(json!({"kind":"window","visible":false,"active":true}));
                }
                _ => {
                    surface.native_event(json!({"kind":"window","visible":true,"active":false}));
                }
            }
            let mut expected = releases();
            expected.push(Call::Visible(false));
            assert_eq!(session.take_calls(), expected, "{event}");
            assert!(
                !surface.native_event(json!({"kind":"pointer_down","x":50,"y":50,"button":"left"}))
            );
            surface.native_event(json!({"kind":"text","text":"must stay local"}));
            assert!(session.take_calls().is_empty(), "{event}");
        }
    }

    #[test]
    fn release_and_drop_send_each_release_once_and_never_stop_a_live_shared_session() {
        let (surface, session) = shown();
        hold(&surface);
        session.take_calls();
        surface.release();
        surface.release();
        assert_eq!(session.take_calls(), releases());
        hold(&surface);
        session.take_calls();
        let id = surface.id().to_string();
        drop(surface);
        assert_eq!(session.take_calls(), releases());
        assert!(Surface::lookup(&id).is_none());
    }

    #[test]
    fn a_five_second_control_lease_keeps_capture_visible_but_never_admits_hidden_input() {
        let (surface, session) = attached(Some(pixel(10, 1)));
        surface.native_event(json!({"kind":"layout","width":100,"height":100}));
        surface.native_event(json!({"kind":"window","visible":false,"active":false}));
        surface.poll();
        session.take_calls();
        let now = Instant::now();
        surface.control_start(now);
        assert_eq!(session.take_calls(), [Call::Visible(true)]);
        assert!(!surface.state.lock().unwrap().snapshot().accepts_input);
        assert!(
            !surface.native_event(json!({"kind":"pointer_down","x":50,"y":50,"button":"left"}))
        );
        surface.native_event(json!({"kind":"focus","focused":true}));
        surface.native_event(json!({"kind":"text","text":"hidden"}));
        assert!(session.take_calls().is_empty());
        surface
            .state
            .lock()
            .unwrap()
            .configure(now + Duration::from_millis(4999));
        assert!(session.take_calls().is_empty());
        surface
            .state
            .lock()
            .unwrap()
            .configure(now + Duration::from_secs(5));
        assert_eq!(session.take_calls(), [Call::Visible(false)]);
        surface
            .state
            .lock()
            .unwrap()
            .configure(now + Duration::from_secs(6));
        assert!(session.take_calls().is_empty());
    }

    #[test]
    fn controlled_refresh_uses_current_source_frame_without_waiting_for_native_decode() {
        let surface = Surface::new();
        assert_eq!(
            surface.control_refresh(Instant::now()).unwrap_err(),
            "This preview has not been started."
        );
        let session = FakeSession::new(None);
        surface.attach(session.clone(), PreviewMode::Cooperative);
        assert!(
            surface
                .control_refresh(Instant::now())
                .unwrap_err()
                .contains("Wait for a running preview")
        );
        session.take_calls();
        session.state.lock().unwrap().frame = Some(pixel(42, 1));
        assert!(
            surface.state.lock().unwrap().frame.is_none(),
            "fixture deliberately has not decoded yet"
        );
        let now = Instant::now();
        surface.control_refresh(now).unwrap();
        assert_eq!(
            session.take_calls(),
            [Call::Visible(true), Call::Input(PreviewInput::Refresh)]
        );
        assert_eq!(
            surface.state.lock().unwrap().control_until,
            Some(now + Duration::from_secs(5))
        );
        surface.poll();
        assert!(surface.state.lock().unwrap().frame.is_some());
        session.state.lock().unwrap().frame = None;
        session.take_calls();
        assert!(
            surface.control_refresh(now).is_err(),
            "cached decoded image cannot establish readiness"
        );
        assert!(session.take_calls().is_empty());
    }

    #[test]
    fn error_stopped_and_exited_with_retained_pid_and_frame_refuse_input_and_control_refresh() {
        for phase in [
            PreviewPhase::Error,
            PreviewPhase::Stopped,
            PreviewPhase::Exited,
        ] {
            let (surface, session) = shown();
            hold(&surface);
            session.take_calls();
            session.state.lock().unwrap().phase = phase;
            assert!(session.state.lock().unwrap().pid.is_some());
            let snapshot = surface.poll();
            assert!(
                !snapshot.running,
                "terminal phase is authoritative even with a retained pid"
            );
            assert!(!snapshot.accepts_input);
            assert_eq!(session.take_calls(), releases());
            assert!(surface.control_refresh(Instant::now()).is_err());
            assert!(
                !surface.native_event(json!({"kind":"pointer_down","x":50,"y":50,"button":"left"}))
            );
            surface.native_event(json!({"kind":"text","text":"ended"}));
            assert!(session.take_calls().is_empty());
        }
    }

    #[test]
    fn reset_invalidates_an_in_flight_control_refresh_without_visible_or_refresh_side_effects() {
        let (surface, session) = attached(Some(pixel(10, 1)));
        session.take_calls();
        let (entered, resume) = session.gate_next_snapshot();
        let worker_surface = surface.clone();
        let worker = std::thread::spawn(move || worker_surface.control_refresh(Instant::now()));
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        surface.reset();
        resume.send(()).unwrap();
        assert!(
            worker
                .join()
                .unwrap()
                .unwrap_err()
                .contains("changed during capture request")
        );
        assert!(session.take_calls().is_empty());
        assert!(surface.state.lock().unwrap().control_until.is_none());
    }

    #[test]
    fn resetting_a_controlled_session_clears_the_capture_lease() {
        let (surface, session) = attached(Some(pixel(10, 1)));
        surface.control_start(Instant::now());
        assert!(surface.state.lock().unwrap().capture_visible);
        session.take_calls();
        surface.reset();
        let state = surface.state.lock().unwrap();
        assert!(state.control_until.is_none());
        assert!(!state.capture_visible);
        assert!(!state.snapshot().accepts_input);
        assert!(state.frame.is_none());
    }

    #[test]
    fn queued_native_install_rejects_old_revisions_and_accepts_current_empty_close() {
        for teardown in ["reset", "close"] {
            let (surface, _) = shown();
            let snapshot = surface.poll();
            let mut installed = None;
            assert!(surface.with_current_frame(snapshot.revision, |frame| installed = frame));
            assert!(Arc::ptr_eq(
                installed.as_ref().unwrap(),
                snapshot.frame.as_ref().unwrap()
            ));
            if teardown == "reset" {
                surface.reset();
            } else {
                surface.close();
            }
            let stale_called = AtomicUsize::new(0);
            assert!(!surface.with_current_frame(snapshot.revision, |_| {
                stale_called.fetch_add(1, Ordering::SeqCst);
            }));
            assert_eq!(stale_called.load(Ordering::SeqCst), 0);
            let cleared = surface.poll();
            assert!(cleared.frame.is_none());
            assert!(
                surface.with_current_frame(cleared.revision, |frame| installed = frame),
                "{teardown} must allow clearing an already installed image"
            );
            assert!(installed.is_none());
        }
    }

    #[test]
    fn native_frame_install_and_close_have_one_atomic_order_without_holding_a_decode_lock() {
        let (surface, session) = shown();
        let revision = surface.poll().revision;
        let order = Arc::new(Mutex::new(Vec::new()));
        let installed = Arc::new(Mutex::new(None));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let installing = surface.clone();
        let install_order = order.clone();
        let install_target = installed.clone();
        let installer = std::thread::spawn(move || {
            installing.with_current_frame(revision, |frame| {
                assert!(frame.is_some());
                // The proof exercises the production guard without constructing
                // an AppKit view. Only the short image-publication callback holds
                // the lock; the delayed decode tests prove decode does not.
                assert!(installing.state.try_lock().is_err());
                install_order.lock().unwrap().push("install entered");
                entered_tx.send(()).unwrap();
                resume_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                *install_target.lock().unwrap() = frame;
                install_order.lock().unwrap().push("install finished");
            })
        });
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let closing = surface.clone();
        let close_order = order.clone();
        let (closing_tx, closing_rx) = mpsc::channel();
        let (closed_tx, closed_rx) = mpsc::channel();
        let closer = std::thread::spawn(move || {
            closing_tx.send(()).unwrap();
            closing.close();
            close_order.lock().unwrap().push("close finished");
            closed_tx.send(()).unwrap();
        });
        closing_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            surface.state.try_lock().is_err(),
            "install owns the publication lock"
        );
        assert!(
            closed_rx.try_recv().is_err(),
            "close cannot pass the live install callback"
        );
        resume_tx.send(()).unwrap();
        assert!(installer.join().unwrap());
        closer.join().unwrap();
        assert_eq!(
            *order.lock().unwrap(),
            ["install entered", "install finished", "close finished"]
        );
        assert_eq!(session.take_calls(), [Call::Stop]);
        assert!(
            !surface
                .with_current_frame(revision, |_| panic!("old install resurrected after close"))
        );
        let current = surface.poll();
        assert!(
            surface
                .with_current_frame(current.revision, |frame| *installed.lock().unwrap() = frame)
        );
        assert!(installed.lock().unwrap().is_none());
    }
}
