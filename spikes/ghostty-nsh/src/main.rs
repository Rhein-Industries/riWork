//! THROWAWAY: a native AppKit/NSH 0.3 fixture, not production RiWork code.
//! The terminal factory owns one direct libghostty NSView via a narrow C ABI.
use std::{
    ffi::{CString, c_char, c_void},
    path::PathBuf,
    rc::Rc,
};

use nsh_core::{JsonValue, prelude::*};
use nsh_platform_macos::{
    AppOptions, AppView, CacheLocation, CacheOptions, ComponentContext, NativeComponent, Root,
    objc2::{
        MainThreadMarker, MainThreadOnly, define_class, msg_send, rc::Retained,
        runtime::ProtocolObject,
    },
    objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSView, NSWindow,
        NSWindowDelegate, NSWindowStyleMask,
    },
    objc2_foundation::{NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString},
};
use serde_json::json;

unsafe extern "C" {
    fn spike_terminal_view_new(command: *const c_char, config: *const c_char) -> *mut NSView;
    fn spike_terminal_view_theme(view: *mut c_void, path: *const c_char);
    fn spike_terminal_view_detach(view: *mut c_void);
    fn spike_terminal_view_reattach(view: *mut c_void);
    fn spike_terminal_view_ime_probe(view: *mut c_void);
    fn spike_terminal_view_scale_probe(view: *mut c_void, scale: f64);
    fn spike_terminal_view_shutdown(view: *mut c_void);
}

struct Terminal {
    view: Retained<NSView>,
    context: ComponentContext,
}

impl Terminal {
    fn ptr(&self) -> *mut c_void {
        Retained::as_ptr(&self.view).cast_mut().cast()
    }
}

impl NativeComponent for Terminal {
    fn view(&self) -> Retained<NSView> {
        self.view.clone()
    }

    fn set_prop(&mut self, name: &str, value: Option<&JsonValue>) {
        eprintln!("SPIKE native prop {name}={value:?}");
        if name == "config" {
            if let Some(path) = value.and_then(JsonValue::as_str) {
                let path = CString::new(path).expect("config path has no NUL");
                unsafe { spike_terminal_view_theme(self.ptr(), path.as_ptr()) };
            }
        }
    }

    fn command(&mut self, name: &str, args: &JsonValue) {
        eprintln!("SPIKE native command {name} args={args}");
        unsafe {
            match name {
                "detach" => spike_terminal_view_detach(self.ptr()),
                "reattach" => spike_terminal_view_reattach(self.ptr()),
                "ime-probe" => spike_terminal_view_ime_probe(self.ptr()),
                "scale-probe" => {
                    spike_terminal_view_scale_probe(self.ptr(), args.as_f64().unwrap_or(2.0))
                }
                _ => eprintln!("SPIKE unknown command {name}"),
            }
        }
        self.context.emit("command", json!({"name": name}));
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        eprintln!("SPIKE NativeComponent drop");
        unsafe { spike_terminal_view_shutdown(self.ptr()) };
    }
}

fn control(label: &'static str, action: impl Fn() + 'static) -> impl IntoElement {
    view()
        .px(px(10.0))
        .py(px(7.0))
        .rounded(px(5.0))
        .bg(rgb(0x34435a))
        .accessible(true)
        .label(label)
        .on_press(move || action())
        .child(text(label).text_size(px(12.0)).text_color(rgb(0xffffff)))
}

#[component]
fn Fixture() -> impl IntoElement {
    let terminal = element_ref();
    let light = signal(false);
    let status = signal(String::from(
        "Direct libghostty • isolated fixture • no GPUI",
    ));
    let initial_config = std::env::var("SPIKE_CONFIG").expect("SPIKE_CONFIG");
    let light_config =
        std::env::var("SPIKE_LIGHT_CONFIG").unwrap_or_else(|_| initial_config.clone());
    view()
        .flex_1()
        .bg(rgb(0x162131))
        .gap(px(8.0))
        .p(px(10.0))
        .child(
            text("RiWork Ghostty / NSH 0.3 SPIKE")
                .text_size(px(17.0))
                .text_color(rgb(0xffffff)),
        )
        .child(
            view()
                .flex_row()
                .gap(px(7.0))
                .child(control("Theme", move || light.set(!light.get())))
                .child(control("Detach", move || {
                    terminal.command("detach", json!(null))
                }))
                .child(control("Reattach", move || {
                    terminal.command("reattach", json!(null))
                }))
                .child(control("IME probe", move || {
                    terminal.command("ime-probe", json!(null))
                }))
                .child(control("Scale 1×", move || {
                    terminal.command("scale-probe", json!(1.0))
                }))
                .child(control("Scale 2×", move || {
                    terminal.command("scale-probe", json!(2.0))
                })),
        )
        .child(
            native("riwork.terminal")
                .element_ref(&terminal)
                .flex_1()
                .prop("config", move || {
                    json!(if light.get() {
                        &light_config
                    } else {
                        &initial_config
                    })
                })
                .on_native_event(move |event: &NativeEvent| {
                    status.set(format!("{}: {}", event.name, event.data));
                }),
        )
        .child(
            text(move || status.get())
                .text_size(px(11.0))
                .text_color(rgb(0xaebdd0)),
        )
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "RiWorkGhosttyNshSpikeWindowDelegate"]
    struct WindowDelegate;
    unsafe impl NSObjectProtocol for WindowDelegate {}
    unsafe impl NSWindowDelegate for WindowDelegate {
        #[unsafe(method(windowShouldClose:))]
        fn window_should_close(&self, _window: &NSWindow) -> bool {
            NSApplication::sharedApplication(self.mtm()).stop(None);
            true
        }
    }
);

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let isolated_home = PathBuf::from(
        std::env::var_os("RIWORK_HOME")
            .expect("RIWORK_HOME must name the isolated fixture directory"),
    );
    assert!(isolated_home.is_absolute(), "RIWORK_HOME must be absolute");
    assert!(
        isolated_home.join("GHOSTTY_NSH_SPIKE_FIXTURE").is_file(),
        "Refusing launch without RIWORK_HOME/GHOSTTY_NSH_SPIKE_FIXTURE marker"
    );
    let command = CString::new(
        std::env::var("SPIKE_COMMAND")
            .expect("SPIKE_COMMAND must attach the isolated tmux session"),
    )
    .unwrap();
    let config = CString::new(
        std::env::var("SPIKE_CONFIG")
            .expect("SPIKE_CONFIG must be the fixture Ghostty configuration"),
    )
    .unwrap();
    let mtm = MainThreadMarker::new().expect("main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    let options = AppOptions {
        title: "RiWork Ghostty NSH SPIKE — isolated fixture".into(),
        app_id: "dev.riwork.ghostty-nsh-spike".into(),
        size: (1000, 680),
        data_dir: Some(isolated_home.join("nsh-data")),
        cache: CacheOptions::at(CacheLocation::Dir(isolated_home.join("nsh-cache"))),
        ..Default::default()
    };
    let view = AppView::new(mtm, options);
    view.register_component(
        "riwork.terminal",
        Rc::new(move |context| {
            let raw = unsafe { spike_terminal_view_new(command.as_ptr(), config.as_ptr()) };
            let view = unsafe { Retained::from_raw(raw) }.expect("libghostty native view creation");
            eprintln!("SPIKE riwork.terminal created view={raw:p}");
            Box::new(Terminal { view, context })
        }),
    );
    let frame = NSRect::new(NSPoint::new(100.0, 100.0), NSSize::new(1000.0, 680.0));
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable;
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    let delegate: Retained<WindowDelegate> = unsafe { msg_send![WindowDelegate::alloc(mtm), init] };
    window.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    window.setTitle(&NSString::from_str(
        "RiWork Ghostty NSH SPIKE — isolated fixture",
    ));
    window.setContentView(Some(&view.view()));
    view.load(
        Root::of::<Fixture>()
            .app_id("dev.riwork.ghostty-nsh-spike")
            .into(),
    );
    // Never activate or steal focus from the live RiWork/orchestrator window.
    window.orderBack(None);
    eprintln!(
        "SPIKE fixture pid={} RIWORK_HOME={}",
        std::process::id(),
        isolated_home.display()
    );
    app.run();
    view.stop();
    window.setDelegate(None);
    eprintln!("SPIKE host stopped");
}
