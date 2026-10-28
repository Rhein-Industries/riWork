//! The reserved `riwork.preview` AppKit component. A private Rust owner supplies
//! decoded pixels directly; props/events never carry an encoded frame.
use crate::preview_surface::{DecodedFrame, Surface, ViewSnapshot};
use nsh_core::JsonValue;
use nsh_platform_macos::{
    ComponentContext, Components, NativeComponent, objc2::rc::Retained, objc2_app_kit::NSView,
};
use riwork_core::app_preview::PreviewMode;
use std::{
    ffi::{CStr, c_char, c_void},
    rc::Rc,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

unsafe extern "C" {
    fn rw_preview_new(
        context: *mut c_void,
        event: extern "C" fn(*mut c_void, *const c_char) -> bool,
        release: extern "C" fn(*mut c_void),
    ) -> *mut NSView;
    fn rw_preview_visible(view: *mut c_void, visible: bool);
    fn rw_preview_state(view: *mut c_void, running: bool, enabled: bool, capture: bool);
    fn rw_preview_frame(
        view: *mut c_void,
        bytes: *const u8,
        length: usize,
        width: u32,
        height: u32,
        owner: *mut c_void,
        release: Option<extern "C" fn(*mut c_void, *const c_void, usize)>,
    );
    fn rw_preview_shutdown(view: *mut c_void);
}

struct State {
    view: AtomicUsize,
    alive: AtomicBool,
    generation: AtomicU64,
    visible: AtomicBool,
    focused: AtomicBool,
    applied_revision: AtomicU64,
    queued: AtomicBool,
    pending: Mutex<Option<Publication>>,
    owner: Mutex<Option<Arc<Surface>>>,
    context: ComponentContext,
}
struct Publication {
    generation: u64,
    owner: Arc<Surface>,
    snapshot: ViewSnapshot,
}
impl State {
    fn surface(&self) -> Option<Arc<Surface>> {
        self.owner.lock().unwrap().clone()
    }
    fn ptr(&self) -> *mut c_void {
        self.view.load(Ordering::Acquire) as *mut c_void
    }
}

/// Register before constructing the first AppView, alongside riwork.terminal.
pub fn register(components: &Components) {
    components.register(
        "riwork.preview",
        Rc::new(move |context| {
            let state = Arc::new(State {
                view: AtomicUsize::new(0),
                alive: AtomicBool::new(true),
                generation: AtomicU64::new(0),
                visible: AtomicBool::new(false),
                focused: AtomicBool::new(false),
                applied_revision: AtomicU64::new(u64::MAX),
                queued: AtomicBool::new(false),
                pending: Mutex::new(None),
                owner: Mutex::new(None),
                context,
            });
            // The NSView may outlive its Rust component during removal. A weak
            // callback keeps it inert then, without extending the owner lifetime.
            let context = Box::into_raw(Box::new(Arc::downgrade(&state))).cast();
            let raw = unsafe { rw_preview_new(context, on_event, release_context) };
            let view = unsafe { Retained::from_raw(raw) }.expect("create preview NSView");
            state.view.store(raw as usize, Ordering::Release);
            start_polling(&state);
            Box::new(Preview { view, state })
        }),
    );
}

struct Preview {
    view: Retained<NSView>,
    state: Arc<State>,
}
impl NativeComponent for Preview {
    fn view(&self) -> Retained<NSView> {
        self.view.clone()
    }

    fn set_prop(&mut self, name: &str, value: Option<&JsonValue>) {
        match name {
            "owner" => {
                let next = value.and_then(JsonValue::as_str).and_then(Surface::lookup);
                let mut current = self.state.owner.lock().unwrap();
                if current
                    .as_ref()
                    .zip(next.as_ref())
                    .is_some_and(|(a, b)| Arc::ptr_eq(a, b))
                {
                    return;
                }
                if let Some(previous) = current.take() {
                    previous.release();
                }
                self.state.generation.fetch_add(1, Ordering::AcqRel);
                self.state
                    .applied_revision
                    .store(u64::MAX, Ordering::Release);
                self.state.pending.lock().unwrap().take();
                if let Some(owner) = &next {
                    owner.set_visible(self.state.visible.load(Ordering::Acquire));
                }
                *current = next;
                drop(current);
                // Clear synchronously; an old worker completion cannot revive it.
                unsafe {
                    rw_preview_state(self.state.ptr(), false, false, false);
                    rw_preview_frame(
                        self.state.ptr(),
                        std::ptr::null(),
                        0,
                        0,
                        0,
                        std::ptr::null_mut(),
                        None,
                    );
                    rw_preview_visible(
                        self.state.ptr(),
                        self.state.visible.load(Ordering::Acquire),
                    );
                }
            }
            "visible" => {
                let visible = value.and_then(JsonValue::as_bool).unwrap_or(false);
                self.state.visible.store(visible, Ordering::Release);
                if let Some(owner) = self.state.surface() {
                    owner.set_visible(visible);
                }
                unsafe { rw_preview_visible(self.state.ptr(), visible) };
            }
            _ => {}
        }
    }
}
impl Drop for Preview {
    fn drop(&mut self) {
        self.state.alive.store(false, Ordering::Release);
        self.state.generation.fetch_add(1, Ordering::AcqRel);
        self.state.pending.lock().unwrap().take();
        if let Some(owner) = self.state.owner.lock().unwrap().take() {
            owner.release();
        }
        unsafe { rw_preview_shutdown(self.state.ptr()) };
    }
}

fn start_polling(state: &Arc<State>) {
    let weak = Arc::downgrade(state);
    thread::Builder::new()
        .name("nsh-preview-view".into())
        .spawn(move || {
            loop {
                let Some(state) = weak.upgrade() else { break };
                if !state.alive.load(Ordering::Acquire) {
                    break;
                }
                let generation = state.generation.load(Ordering::Acquire);
                if let Some(owner) = state.surface() {
                    // PNG decode and engine polling happen on this worker.
                    // Publication looks up current pixels under the install
                    // guard; a queued callback retains no obsolete image.
                    let mut snapshot = owner.poll();
                    drop(snapshot.frame.take());
                    // Only one pending publication exists even if the main thread
                    // is busy. Newer frames replace it; callbacks never accumulate
                    // an unbounded collection of stale decoded frame allocations.
                    let schedule = {
                        let mut pending = state.pending.lock().unwrap();
                        *pending = Some(Publication {
                            generation,
                            owner,
                            snapshot,
                        });
                        !state.queued.swap(true, Ordering::AcqRel)
                    };
                    if schedule {
                        let install = state.clone();
                        dispatch2::DispatchQueue::main().exec_async(move || {
                            let publication = {
                                let mut pending = install.pending.lock().unwrap();
                                install.queued.store(false, Ordering::Release);
                                pending.take()
                            };
                            if let Some(Publication {
                                generation,
                                owner,
                                snapshot,
                            }) = publication
                                && install.alive.load(Ordering::Acquire)
                                && install.generation.load(Ordering::Acquire) == generation
                            {
                                apply(&install, &owner, snapshot);
                            }
                        });
                    }
                }
                drop(state);
                thread::sleep(Duration::from_millis(100));
            }
        })
        .expect("start preview view polling");
}

fn apply(state: &State, surface: &Surface, snapshot: ViewSnapshot) {
    // This can synchronously emit native focus/release events, so it must not
    // run under the Surface frame-install lock.
    unsafe {
        rw_preview_state(
            state.ptr(),
            snapshot.running,
            snapshot.accepts_input,
            snapshot.mode == PreviewMode::WindowCapture,
        );
    }
    surface.with_current_frame(snapshot.revision, |frame| {
        if state
            .applied_revision
            .swap(snapshot.revision, Ordering::AcqRel)
            == snapshot.revision
        {
            return;
        }
        if let Some(frame) = frame {
            // CGDataProvider owns this Arc until its image has left the layer. No
            // copy, temporary file, JSON encoding or JS allocation is involved.
            let bytes = frame.bgra.as_ptr();
            let length = frame.bgra.len();
            let (width, height) = (frame.width, frame.height);
            let owner = Box::into_raw(Box::new(frame)).cast();
            unsafe {
                rw_preview_frame(
                    state.ptr(),
                    bytes,
                    length,
                    width,
                    height,
                    owner,
                    Some(release_frame),
                );
            }
        } else {
            unsafe {
                rw_preview_frame(
                    state.ptr(),
                    std::ptr::null(),
                    0,
                    0,
                    0,
                    std::ptr::null_mut(),
                    None,
                );
            }
        }
    });
}

extern "C" fn on_event(context: *mut c_void, json: *const c_char) -> bool {
    if context.is_null() || json.is_null() {
        return false;
    }
    let weak = unsafe { &*context.cast::<Weak<State>>() };
    let Some(state) = weak.upgrade() else {
        return false;
    };
    if !state.alive.load(Ordering::Acquire) {
        return false;
    }
    let Ok(value) = serde_json::from_slice::<JsonValue>(unsafe { CStr::from_ptr(json) }.to_bytes())
    else {
        return false;
    };
    let focus_event = value.get("kind").and_then(JsonValue::as_str) == Some("focus");
    let focused = focus_event && value.get("focused").and_then(JsonValue::as_bool) == Some(true);
    let release = value.get("kind").and_then(JsonValue::as_str) == Some("release");
    if (focus_event || release) && state.focused.swap(focused, Ordering::AcqRel) != focused {
        state
            .context
            .emit("focusChange", serde_json::json!({"focused":focused}));
    }
    state
        .surface()
        .is_some_and(|owner| owner.native_event(value))
}

extern "C" fn release_context(context: *mut c_void) {
    if !context.is_null() {
        drop(unsafe { Box::from_raw(context.cast::<Weak<State>>()) });
    }
}
extern "C" fn release_frame(owner: *mut c_void, _: *const c_void, _: usize) {
    if !owner.is_null() {
        drop(unsafe { Box::from_raw(owner.cast::<Arc<DecodedFrame>>()) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preview_input::SurfaceEvent;
    use serde_json::{Value, json};
    use std::ffi::CString;

    unsafe extern "C" {
        fn rw_preview_fixture(input: *const c_char) -> *mut c_char;
        fn rw_preview_fixture_free(value: *mut c_char);
    }

    /// Foundation + CGDataProvider only: the fixture never constructs AppKit
    /// application/window/view/event objects or reads the native clipboard.
    fn fixture(value: Value) -> Value {
        let request = CString::new(value.to_string()).unwrap();
        let raw = unsafe { rw_preview_fixture(request.as_ptr()) };
        assert!(!raw.is_null());
        let result = serde_json::from_slice(unsafe { CStr::from_ptr(raw) }.to_bytes());
        unsafe { rw_preview_fixture_free(raw) };
        let result = result.unwrap();
        eprintln!("preview native fixture: {value} -> {result}");
        result
    }

    #[test]
    fn native_key_formatter_matches_gpui_names_without_appkit_events() {
        for (characters, modifiers, expected) in [
            ("a", vec!["cmd"], "cmd-a"),
            ("\t", vec!["shift"], "shift-tab"),
            ("\r", vec![], "enter"),
            ("\u{7f}", vec![], "backspace"),
            ("\u{1b}", vec![], "escape"),
            ("\u{f700}", vec!["fn"], "up"),
            ("\u{f703}", vec!["shift"], "shift-right"),
            ("\u{f728}", vec![], "delete"),
            ("\u{f746}", vec![], "insert"),
            ("\u{f704}", vec!["fn"], "f1"),
            (
                "a",
                vec!["fn", "ctrl", "alt", "cmd", "shift"],
                "fn-ctrl-alt-cmd-shift-a",
            ),
        ] {
            let actual = fixture(
                json!({"operation":"key", "characters":characters, "typed":characters,
                "modifiers":modifiers}),
            );
            assert_eq!(actual["key"], expected);
            assert_eq!(actual["text_key"], false);
        }
        for typed in ["é", "日本", "😀", " ", ""] {
            let actual = fixture(json!({"operation":"key", "characters":typed, "typed":typed,
                "modifiers":[]}));
            assert_eq!(actual["text_key"], true, "Unicode/dead keys belong to IME");
        }
        // Shift is retained for ASCII letters but absorbed into punctuation;
        // alternate command layouts use their command translation.
        assert_eq!(
            fixture(
                json!({"operation":"key", "characters":"?", "typed":"?", "plain":"/",
            "shifted":"?", "modifiers":["shift"]})
            )["key"],
            "?"
        );
        assert_eq!(
            fixture(
                json!({"operation":"key", "characters":"ս", "typed":"ս", "plain":"ս",
            "cmd":"s", "both":"S", "modifiers":["cmd","shift"]})
            )["key"],
            "cmd-shift-s"
        );
    }

    #[test]
    fn native_mouse_records_keep_all_buttons_outside_coordinates_and_wheel_units() {
        for (button, name) in [(0, "left"), (1, "right"), (2, "middle")] {
            for down in [true, false] {
                let record = fixture(json!({"operation":"pointer", "button":button, "down":down,
                    "x":-40.5, "y":9000.0}));
                assert_eq!(record["button"], name);
                assert_eq!(
                    record["kind"],
                    if down { "pointer_down" } else { "pointer_up" }
                );
                assert_eq!(record["x"], -40.5);
                assert_eq!(record["y"], 9000.0);
                serde_json::from_value::<SurfaceEvent>(record).unwrap();
            }
        }
        assert!(fixture(json!({"operation":"pointer", "button":3})).is_null());
        for precise in [true, false] {
            let record = fixture(json!({"operation":"scroll", "x":12, "y":14, "dx":1.25,
                "dy":-3.5, "precise":precise}));
            assert_eq!(record["delta_x"], 1.25);
            assert_eq!(record["delta_y"], -3.5);
            assert_eq!(record["precise"], precise);
            serde_json::from_value::<SurfaceEvent>(record).unwrap();
        }
    }

    #[test]
    fn native_key_up_preserves_down_spelling_after_modifiers_change() {
        let actual = fixture(json!({"operation":"keyup"}));
        assert_eq!(
            actual,
            json!({"record":{"kind":"key_up", "key":"cmd-shift-a", "text_key":false}, "held":0})
        );
        serde_json::from_value::<SurfaceEvent>(actual["record"].clone()).unwrap();
        assert_eq!(
            fixture(json!({"operation":"repeat"})),
            json!({"unchanged":null,
            "changed":{"kind":"key_up","key":"cmd-shift-a","text_key":false},"held":0})
        );
    }

    #[test]
    fn native_key_up_records_own_dynamic_keys_after_the_key_down_pool_drains() {
        assert_eq!(
            fixture(json!({"operation":"keyup_dynamic"})),
            json!({"up_owned":true, "repeat_owned":true, "released":true, "held":0})
        );
    }

    #[test]
    fn native_ime_falls_back_only_for_unhandled_navigation_once() {
        for (text_key, consumed) in [(false, false), (false, true), (true, false)] {
            let actual = fixture(
                json!({"operation":"ime_command", "key":"left", "text_key":text_key, "consumed":consumed}),
            );
            let expected = if text_key || consumed {
                json!([])
            } else {
                json!([{"kind":"key","key":"left","text_key":false}])
            };
            assert_eq!(actual, json!({"events":expected, "after_return":null}));
        }
    }

    #[test]
    fn native_key_up_monitor_admits_only_the_exact_focused_owned_release() {
        let valid = json!({"operation":"key_up_monitor", "stopped":false, "same_window":true, "first_responder":true, "held":true});
        assert_eq!(fixture(valid.clone()), json!({"consume":true}));
        for (field, value) in [
            ("stopped", true),
            ("same_window", false),
            ("first_responder", false),
            ("held", false),
        ] {
            let mut invalid = valid.clone();
            invalid[field] = json!(value);
            assert_eq!(fixture(invalid), json!({"consume":false}), "{field}");
        }
    }

    #[test]
    fn native_ime_keeps_utf16_marked_text_local_and_commits_unicode() {
        let actual = fixture(json!({"operation":"ime"}));
        assert_eq!(
            actual,
            json!({"marked_utf16":5, "selection_start":5, "selection_length":0,
            "commit":{"kind":"text", "text":"日本😀"}, "marked_after":0})
        );
        serde_json::from_value::<SurfaceEvent>(actual["commit"].clone()).unwrap();
    }

    #[test]
    fn native_focus_reactivation_reconciles_the_same_first_responder_once() {
        use crate::preview_input::Policy;
        use riwork_core::app_preview::PreviewInput;
        let mut policy = Policy::new(PreviewMode::Cooperative);
        policy.configure(PreviewMode::Cooperative, Some((10, 10)), true, true, true);
        policy.handle(SurfaceEvent::Window {
            visible: true,
            active: true,
        });
        let active =
            fixture(json!({"operation":"focus", "can_input":true, "first_responder":true}));
        let inactive =
            fixture(json!({"operation":"focus", "can_input":false, "first_responder":true}));
        let other =
            fixture(json!({"operation":"focus", "can_input":true, "first_responder":false}));
        assert_eq!(active, json!({"kind":"focus", "focused":true}));
        assert_eq!(inactive, json!({"kind":"release"}));
        assert_eq!(other, json!({"kind":"release"}));
        assert_eq!(
            policy
                .handle(serde_json::from_value(active.clone()).unwrap())
                .inputs,
            vec![PreviewInput::Focus { focused: true }]
        );
        assert_eq!(
            policy
                .handle(SurfaceEvent::Window {
                    visible: true,
                    active: false
                })
                .inputs,
            vec![PreviewInput::Focus { focused: false }]
        );
        assert!(
            policy
                .handle(serde_json::from_value(inactive).unwrap())
                .inputs
                .is_empty()
        );
        assert!(
            policy
                .handle(SurfaceEvent::Window {
                    visible: true,
                    active: true
                })
                .inputs
                .is_empty()
        );
        assert_eq!(
            policy
                .handle(serde_json::from_value(active.clone()).unwrap())
                .inputs,
            vec![PreviewInput::Focus { focused: true }]
        );
        assert!(
            policy
                .handle(serde_json::from_value(active).unwrap())
                .inputs
                .is_empty()
        );
    }

    #[test]
    fn cgimage_keeps_bgra_owner_until_release_and_rejects_mismatched_length() {
        let actual = fixture(json!({"operation":"image"}));
        assert_eq!(
            actual,
            json!({"valid":true, "identical_bgra":true, "release_before":0,
            "release_after":1, "invalid_releases":1})
        );
        let frame = Arc::new(DecodedFrame {
            width: 1,
            height: 1,
            bgra: vec![0, 0, 0, 255],
        });
        let provider_owner = Box::into_raw(Box::new(frame.clone())).cast();
        assert_eq!(Arc::strong_count(&frame), 2);
        release_frame(provider_owner, frame.bgra.as_ptr().cast(), frame.bgra.len());
        assert_eq!(Arc::strong_count(&frame), 1);
    }
}
