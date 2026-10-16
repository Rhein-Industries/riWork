use super::*;
use crate::form_input::{test_turn, test_window};
use gpui::TestAppContext;
use gpui_kit::test::TestWindowExt as _;
use riwork_engine::app_preview::PreviewLog;
use std::cell::RefCell;

#[derive(Default)]
struct Calls {
    launches: Vec<PreviewSpec>,
    visibility: Vec<bool>,
    inputs: Vec<PreviewInput>,
    stops: usize,
    restarts: usize,
    stopped: bool,
    phase: Option<PreviewPhase>,
    frame: Option<Arc<PreviewFrame>>,
}

struct FakeSession(Rc<RefCell<Calls>>);

impl PreviewSession for FakeSession {
    fn snapshot(&self) -> PreviewState {
        PreviewState {
            phase: if self.0.borrow().stopped {
                PreviewPhase::Stopped
            } else {
                self.0.borrow().phase.unwrap_or(PreviewPhase::Streaming)
            },
            mode: PreviewMode::Cooperative,
            pid: Some(99),
            window_id: None,
            windows: Vec::new(),
            ios: None,
            message: "Streaming hidden GPUI app".into(),
            warning: None,
            frame: self.0.borrow().frame.clone(),
            logs: Vec::<PreviewLog>::new(),
        }
    }
    fn set_visible(&self, visible: bool) {
        self.0.borrow_mut().visibility.push(visible);
    }
    fn input(&self, input: PreviewInput) {
        self.0.borrow_mut().inputs.push(input);
    }
    fn stop(&self) {
        self.0.borrow_mut().stops += 1;
    }
    fn restart(&self) {
        self.0.borrow_mut().restarts += 1;
    }
    fn select_window(&self, _: u32, _: u64) {}
}

fn spec() -> PreviewSpec {
    PreviewSpec {
        command: "cargo run -p demo".into(),
        cwd: PathBuf::from("/tmp/demo"),
        mode: PreviewMode::Cooperative,
        ios: None,
    }
}

fn open(
    cx: &mut TestAppContext,
) -> (
    gpui::AnyWindowHandle,
    Entity<AppPreviewView>,
    Rc<RefCell<Calls>>,
) {
    let calls = Rc::new(RefCell::new(Calls::default()));
    let launch_calls = calls.clone();
    let (handle, view) = test_window(cx, move |window, cx| {
        AppPreviewView::with_launcher(
            spec(),
            window,
            cx,
            Box::new(move |spec| {
                launch_calls.borrow_mut().launches.push(spec);
                Ok(Box::new(FakeSession(launch_calls.clone())))
            }),
        )
    });
    (handle, view, calls)
}

#[gpui::test]
fn ios_mode_runs_a_scheme_without_a_shell_command(cx: &mut TestAppContext) {
    let (handle, view, calls) = open(cx);
    test_turn(cx, handle, |window, cx| {
        view.update(cx, |view, cx| {
            view.spec.mode = PreviewMode::IosSimulator;
            view.spec.command.clear();
            view.spec.ios = Some(IosPreviewSpec {
                scheme: "MyApp".into(),
                ..Default::default()
            });
            view.focus(window, cx);
            assert!(view.ios_scheme.read(cx).focus_handle(cx).is_focused(window));
            view.run(cx);
        });
    });
    assert_eq!(calls.borrow().launches.len(), 1);
    assert_eq!(
        calls.borrow().launches[0].ios.as_ref().unwrap().scheme,
        "MyApp"
    );
    assert!(calls.borrow().launches[0].command.is_empty());
}

#[gpui::test]
fn shell_control_wakes_background_stream_and_requests_fresh_frames(cx: &mut TestAppContext) {
    let (handle, view, calls) = open(cx);
    test_turn(cx, handle, |_, cx| {
        view.update(cx, |view, cx| {
            assert!(view.refresh_controlled().is_err());
            assert!(
                view.input_controlled(PreviewInput::Key {
                    key: "enter".into()
                })
                .is_err()
            );
            view.start_controlled(cx).unwrap();
            calls.borrow_mut().frame = Some(Arc::new(PreviewFrame {
                png: Vec::new(),
                width: 400,
                height: 800,
                sequence: 1,
            }));
            view.input_controlled(PreviewInput::PointerDown {
                x: 50.,
                y: 60.,
                button: PreviewMouseButton::Left,
            })
            .unwrap();
            assert!(
                !view.visible,
                "shell testing does not require selecting the tab"
            );
            assert!(
                view.control_until
                    .is_some_and(|until| until > Instant::now())
            );
        });
    });
    assert!(calls.borrow().visibility.contains(&true));
    assert!(
        calls
            .borrow()
            .inputs
            .iter()
            .any(|input| matches!(input, PreviewInput::Refresh))
    );
    assert!(calls.borrow().inputs.iter().any(
        |input| matches!(input, PreviewInput::PointerDown { x, y, .. } if *x == 50. && *y == 60.)
    ));
}

#[test]
fn aspect_fit_maps_real_content_and_rejects_letterboxes() {
    let bounds = (10., 20., 800., 600.);
    let frame = (1600, 900);
    assert_eq!(
        map_point(bounds, frame, (410., 320.), false),
        Some((800., 450.))
    );
    assert_eq!(map_point(bounds, frame, (10., 20.), false), None);
    assert_eq!(map_point(bounds, frame, (810., 320.), false), None);
    assert_eq!(
        map_point(bounds, frame, (850., 640.), true),
        Some((1599., 899.))
    );
    assert_eq!(map_point((0., 0., 0., 100.), frame, (0., 1.), false), None);
    assert_eq!(map_point(bounds, frame, (f32::NAN, 1.), true), None);
}

#[test]
fn resize_is_bounded_without_distorting_aspect_ratio() {
    assert_eq!(bounded_viewport(960., 600.), Some((960, 600)));
    assert_eq!(bounded_viewport(3840., 2400.), Some((1920, 1200)));
    assert_eq!(bounded_viewport(3000., 600.), Some((1920, 384)));
    assert_eq!(bounded_viewport(20., 20.), None);
    assert_eq!(bounded_viewport(f32::INFINITY, 600.), None);
}

#[test]
fn frames_reject_oversize_dimensions_and_metadata_mismatch() {
    let mut png = Cursor::new(Vec::new());
    image::RgbaImage::new(2, 2)
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let mut frame = PreviewFrame {
        png: png.into_inner(),
        width: 2,
        height: 2,
        sequence: 1,
    };
    assert!(decode_png(&frame).is_ok());
    frame.width = 3;
    assert!(decode_png(&frame).unwrap_err().contains("metadata"));
    frame.width = 4096;
    assert!(decode_png(&frame).unwrap_err().contains("limit"));
}

#[gpui::test]
fn restoring_a_configured_tab_waits_for_explicit_run(cx: &mut TestAppContext) {
    let (handle, view, calls) = open(cx);
    assert!(calls.borrow().launches.is_empty());
    view.read_with(cx, |view, _| {
        assert_eq!(view.spec(), &spec());
        assert!(view.session.is_none());
    });
    test_turn(cx, handle, |window, app| {
        window.click("app-preview-run", app)
    });
    assert_eq!(calls.borrow().launches, vec![spec()]);
    test_turn(cx, handle, |window, app| {
        window.click("app-preview-restart", app)
    });
    assert_eq!(calls.borrow().launches.len(), 1);
    assert_eq!(calls.borrow().restarts, 1);
    assert_eq!(calls.borrow().stops, 0);
}

#[gpui::test]
fn an_edited_launch_waits_for_the_previous_process_to_stop(cx: &mut TestAppContext) {
    let (handle, view, calls) = open(cx);
    let routed = view.clone();
    test_turn(cx, handle, move |_, app| {
        routed.update(app, |view, cx| {
            view.run(cx);
            view.spec.command = "cargo run -p other-demo".into();
            view.run(cx);
        });
    });
    assert_eq!(calls.borrow().launches.len(), 1);
    assert_eq!(calls.borrow().stops, 1);
    calls.borrow_mut().stopped = true;
    test_turn(cx, handle, move |window, app| {
        view.update(app, |view, cx| view.poll(window, cx));
    });
    assert_eq!(calls.borrow().launches.len(), 2);
    assert_eq!(
        calls.borrow().launches[1].command,
        "cargo run -p other-demo"
    );
}

#[gpui::test]
fn stop_stays_available_when_a_live_app_reports_an_error(cx: &mut TestAppContext) {
    let (handle, view, calls) = open(cx);
    let routed = view.clone();
    test_turn(cx, handle, move |_, app| {
        routed.update(app, |view, cx| view.run(cx))
    });
    calls.borrow_mut().phase = Some(PreviewPhase::Error);
    test_turn(cx, handle, move |window, app| {
        view.update(app, |view, cx| view.poll(window, cx))
    });
    test_turn(cx, handle, |window, app| {
        window.click("app-preview-stop", app)
    });
    assert_eq!(calls.borrow().stops, 1);
}

#[gpui::test]
fn invisible_tabs_release_drag_and_reject_committed_text(cx: &mut TestAppContext) {
    let (handle, view, calls) = open(cx);
    let routed = view.clone();
    test_turn(cx, handle, move |window, app| {
        routed.update(app, |view, cx| {
            view.set_visible(true, cx);
            view.run(cx);
            view.capture_visible = true;
            view.frame_size = Some((800, 600));
            view.viewport.set(Bounds::new(
                gpui::point(px(0.), px(0.)),
                gpui::size(px(800.), px(600.)),
            ));
            view.focus.focus(window, cx);
            view.replace_text_in_range(None, "hello 👋", window, cx);
            view.pressed = Some(PreviewMouseButton::Left);
            view.last_pointer = Some(gpui::point(px(900.), px(700.)));
            view.focused = true;
            view.set_visible(false, cx);
            view.replace_text_in_range(None, "must stay local", window, cx);
        });
    });
    let calls = calls.borrow();
    assert!(calls.inputs.contains(&PreviewInput::Text {
        text: "hello 👋".into()
    }));
    assert!(!calls.inputs.contains(&PreviewInput::Text {
        text: "must stay local".into()
    }));
    assert!(calls.inputs.contains(&PreviewInput::PointerUp {
        x: 799.,
        y: 599.,
        button: PreviewMouseButton::Left
    }));
    assert!(
        calls
            .inputs
            .contains(&PreviewInput::Focus { focused: false })
    );
    assert_eq!(calls.visibility.last(), Some(&false));
}

#[gpui::test]
fn focused_preview_receives_native_text_and_navigation_shortcuts(cx: &mut TestAppContext) {
    let (handle, view, calls) = open(cx);
    let routed = view.clone();
    test_turn(cx, handle, move |window, app| {
        routed.update(app, |view, cx| {
            view.set_visible(true, cx);
            view.run(cx);
            view.capture_visible = true;
            view.frame_size = Some((800, 600));
            view.focus.focus(window, cx);
        });
    });
    for key in ["tab", "shift-tab", "cmd-c", "cmd-a"] {
        test_turn(cx, handle, move |window, app| window.press(key, app));
        assert!(
            calls
                .borrow()
                .inputs
                .contains(&PreviewInput::Key { key: key.into() }),
            "preview did not receive {key}"
        );
        let focused = view.clone();
        test_turn(cx, handle, move |window, app| {
            focused.read_with(app, |view, _| assert!(view.focus.is_focused(window)));
        });
    }
    test_turn(cx, handle, |window, app| window.input("héllo 👋", app));
    let text: String = calls
        .borrow()
        .inputs
        .iter()
        .filter_map(|input| match input {
            PreviewInput::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "héllo 👋");
    calls.borrow_mut().inputs.clear();
    test_turn(cx, handle, |window, app| {
        app.write_to_clipboard(gpui::ClipboardItem::new_string("host clipboard 👋".into()));
        window.press(
            if cfg!(target_os = "macos") {
                "cmd-v"
            } else {
                "ctrl-v"
            },
            app,
        );
    });
    assert!(calls.borrow().inputs.contains(&PreviewInput::Text {
        text: "host clipboard 👋".into()
    }));
    assert!(!calls.borrow().inputs.contains(&PreviewInput::Key {
        key: "cmd-v".into()
    }));
}

#[test]
fn log_display_bounds_output_and_removes_control_codes() {
    assert_eq!(clean_log("hello\u{0}\u{7}\r\nworld\t!"), "hello\nworld\t!");
    assert_eq!(clean_log(&"x".repeat(10_000)).len(), 2000);
    assert!(is_text_key(&gpui::Keystroke::parse("a->a").unwrap()));
    assert!(!is_text_key(&gpui::Keystroke::parse("cmd-a").unwrap()));
    assert!(!is_text_key(&gpui::Keystroke::parse("enter").unwrap()));
}
