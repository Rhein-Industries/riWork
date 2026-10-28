//! Real AppKit objects, main thread, no ordered or activated windows.
#![cfg(target_os = "macos")]
#[allow(dead_code)]
#[path = "../../src/nsh_chrome.rs"]
mod nsh_chrome;
#[allow(dead_code)]
#[path = "../../src/nsh_menus.rs"]
mod nsh_menus;

use nsh_platform_macos::{
    objc2::{MainThreadMarker, rc::Retained, sel},
    objc2_app_kit::{
        NSAccessibility, NSApplication, NSApplicationActivationPolicy, NSEventModifierFlags,
        NSMenu, NSMenuItem, NSWindowButton, NSWindowStyleMask,
    },
    objc2_foundation::{NSPoint, NSRect, NSSize},
};
use std::{panic::AssertUnwindSafe, sync::Arc};

fn submenu(menu: &NSMenu, index: isize) -> Retained<NSMenu> {
    menu.itemAtIndex(index).unwrap().submenu().unwrap()
}

fn labels(menu: &NSMenu) -> Vec<String> {
    (0..menu.numberOfItems())
        .map(|index| {
            let item = menu.itemAtIndex(index).unwrap();
            if item.isSeparatorItem() {
                "---".into()
            } else {
                item.title().to_string()
            }
        })
        .collect()
}

fn key(item: &NSMenuItem, text: &str, flags: NSEventModifierFlags) {
    assert_eq!(item.keyEquivalent().to_string(), text);
    assert_eq!(item.keyEquivalentModifierMask(), flags);
}

fn window_chrome(mtm: MainThreadMarker, _: &NSMenu, _: &nsh_menus::HostMenus) {
    let window = nsh_chrome::create_window(
        mtm,
        NSRect::new(NSPoint::new(100., 100.), NSSize::new(1000., 700.)),
    );
    nsh_chrome::position_controls(&window);
    assert!(
        window
            .styleMask()
            .contains(NSWindowStyleMask::FullSizeContentView)
    );
    assert!(window.titlebarAppearsTransparent());
    assert_eq!(window.contentMinSize(), NSSize::new(640., 400.));
    assert!(!window.isVisible());
    let close = window
        .standardWindowButton(NSWindowButton::CloseButton)
        .unwrap();
    let frame = close.convertRect_toView(close.bounds(), None);
    println!(
        "chrome native_window={:?} button={:?} converted={:?} accessibility={:?}",
        window.frame(),
        close.frame(),
        frame,
        close.accessibilityFrame()
    );
    assert_eq!(frame.origin.x, 10.);
    let top = window.frame().size.height - frame.origin.y - frame.size.height;
    assert_eq!(top, 8.);
    window.close();
}

fn app_menu_matches_gpui(_: MainThreadMarker, menu: &NSMenu, _: &nsh_menus::HostMenus) {
    assert_eq!(labels(menu), ["RiWork", "Edit", "View"]);
    let app = submenu(menu, 0);
    assert_eq!(
        labels(&app),
        [
            "Settings…",
            "Project Settings…",
            "Automations",
            "---",
            "Quit RiWork"
        ]
    );
    let cmd = NSEventModifierFlags::Command;
    for (index, text, mask, tag) in [
        (0, ",", cmd, 1),
        (1, "a", cmd | NSEventModifierFlags::Option, 2),
        (2, "s", cmd | NSEventModifierFlags::Shift, 3),
        (4, "q", cmd, 4),
    ] {
        let item = app.itemAtIndex(index).unwrap();
        key(&item, text, mask);
        assert_eq!(item.tag(), tag);
        assert!(item.isEnabled());
    }
}

fn edit_menu_uses_standard_selectors(_: MainThreadMarker, menu: &NSMenu, _: &nsh_menus::HostMenus) {
    let edit = submenu(menu, 1);
    assert_eq!(
        labels(&edit),
        [
            "Cut",
            "Copy",
            "Paste",
            "---",
            "Select All",
            "---",
            "Find…",
            "Find Next",
            "Find Previous"
        ]
    );
    for (index, selector, text) in [
        (0, sel!(cut:), "x"),
        (1, sel!(copy:), "c"),
        (2, sel!(paste:), "v"),
        (4, sel!(selectAll:), "a"),
    ] {
        let item = edit.itemAtIndex(index).unwrap();
        assert_eq!(item.action(), Some(selector));
        key(&item, text, NSEventModifierFlags::Command);
    }
    key(
        &edit.itemAtIndex(6).unwrap(),
        "f",
        NSEventModifierFlags::Command,
    );
    key(
        &edit.itemAtIndex(7).unwrap(),
        "g",
        NSEventModifierFlags::Command,
    );
    key(
        &edit.itemAtIndex(8).unwrap(),
        "g",
        NSEventModifierFlags::Command | NSEventModifierFlags::Shift,
    );
}

fn view_menu_matches_gpui(_: MainThreadMarker, menu: &NSMenu, _: &nsh_menus::HostMenus) {
    let view = submenu(menu, 2);
    assert_eq!(
        labels(&view),
        [
            "Global Dashboard",
            "Global Chat",
            "---",
            "New chat…",
            "App preview…",
            "Sessions",
            "New Subshell",
            "Open File in Vim…",
            "Default Layout",
            "Gather Tabs into Main Pane",
            "---",
            "Toggle Normal / Verbose",
            "Expand All Activity",
            "Collapse All Activity",
            "---",
            "Bigger Text",
            "Smaller Text",
            "Actual Size"
        ]
    );
    for index in [3, 5, 11, 12, 13] {
        assert!(
            !view.itemAtIndex(index).unwrap().isEnabled(),
            "deferred row {index} must stay disabled"
        );
    }
    for index in [0, 1, 4, 6, 7, 8, 9, 15, 16, 17] {
        assert!(view.itemAtIndex(index).unwrap().isEnabled());
    }
    // AppKit must not silently re-enable deferred commands during validation.
    view.update();
    for index in [3, 5, 11, 12, 13] {
        assert!(!view.itemAtIndex(index).unwrap().isEnabled());
    }
    assert!(
        view.itemAtIndex(4).unwrap().isEnabled(),
        "Phase 6c App preview is implemented"
    );
}

fn view_menu_text_size_items(_: MainThreadMarker, menu: &NSMenu, _: &nsh_menus::HostMenus) {
    let view = submenu(menu, 2);
    for (index, text) in [(15, "="), (16, "-"), (17, "0")] {
        key(
            &view.itemAtIndex(index).unwrap(),
            text,
            NSEventModifierFlags::Command,
        );
    }
}

fn app_level_key_equivalents(_: MainThreadMarker, menu: &NSMenu, _: &nsh_menus::HostMenus) {
    let view = submenu(menu, 2);
    key(
        &view.itemAtIndex(0).unwrap(),
        "g",
        NSEventModifierFlags::Command | NSEventModifierFlags::Option,
    );
    key(
        &view.itemAtIndex(1).unwrap(),
        "o",
        NSEventModifierFlags::Command | NSEventModifierFlags::Shift,
    );
    key(
        &submenu(menu, 0).itemAtIndex(4).unwrap(),
        "q",
        NSEventModifierFlags::Command,
    );
}

fn text_size_key_equivalents_dispatch_without_a_window(
    mtm: MainThreadMarker,
    original: &NSMenu,
    _: &nsh_menus::HostMenus,
) {
    use nsh_platform_macos::{
        objc2_app_kit::{NSEvent, NSEventType},
        objc2_foundation::NSString,
    };
    use std::sync::Mutex;
    let app = NSApplication::sharedApplication(mtm);
    assert!(app.keyWindow().is_none());
    let captured = Arc::new(Mutex::new(Vec::new()));
    let output = captured.clone();
    let menus = nsh_menus::HostMenus::install(
        mtm,
        Arc::new(move |action| output.lock().unwrap().push(action)),
    );
    let menu = app.mainMenu().unwrap();
    for (key, command) in [
        ("=", "biggerText"),
        ("-", "smallerText"),
        ("0", "actualSize"),
    ] {
        let text = NSString::from_str(key);
        let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            NSEventType::KeyDown, NSPoint::new(0., 0.), NSEventModifierFlags::Command,
            0., 0, None, &text, &text, false, 0,
        ).unwrap();
        assert!(menu.performKeyEquivalent(&event));
        assert_eq!(
            captured.lock().unwrap().last(),
            Some(&nsh_menus::MenuAction {
                window_number: None,
                command
            })
        );
    }
    assert_eq!(captured.lock().unwrap().len(), 3);
    drop(menus);
    app.setMainMenu(Some(original));
}

fn dock_menu_from_a_fake_registry(_: MainThreadMarker, _: &NSMenu, menus: &nsh_menus::HostMenus) {
    let windows = vec![
        nsh_menus::DockWindow {
            window_number: 27,
            id: "global".into(),
            project: "Global".into(),
            branch: None,
        },
        nsh_menus::DockWindow {
            window_number: 24,
            id: "alpha".into(),
            project: "Alpha".into(),
            branch: Some("main".into()),
        },
    ];
    let dock = menus.dock_menu(&windows, Some(24));
    assert_eq!(labels(&dock), ["Alpha", "Global", "---", "New Window"]);
    assert_eq!(dock.itemAtIndex(0).unwrap().state(), 1);
    assert_eq!(dock.itemAtIndex(1).unwrap().state(), 0);
    assert_eq!(dock.itemAtIndex(0).unwrap().tag(), 24);
    assert_eq!(
        dock.itemAtIndex(0).unwrap().action(),
        Some(sel!(riworkDockWindowChosen:))
    );
    assert_eq!(
        dock.itemAtIndex(3).unwrap().action(),
        Some(sel!(riworkNewWindow:))
    );
    let cached = menus.dock_menu(&windows, Some(24));
    assert!(std::ptr::eq(&*dock, &*cached));
    let changed = menus.dock_menu(&windows, Some(27));
    assert!(!std::ptr::eq(&*dock, &*changed));
}

pub fn run() {
    let mtm = MainThreadMarker::new().expect("AppKit acceptance runs on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Prohibited);
    let menus = nsh_menus::HostMenus::install(mtm, Arc::new(|_| {}));
    let menu = app.mainMenu().unwrap();
    type Check = fn(MainThreadMarker, &NSMenu, &nsh_menus::HostMenus);
    let cases: &[(&str, Check)] = &[
        ("window_chrome", window_chrome),
        (
            "control_tab_reaches_the_nsh_keymap_once",
            control_tab_reaches_the_nsh_keymap_once,
        ),
        (
            "titlebar_double_click_reads_the_system_preference",
            titlebar_double_click_reads_the_system_preference,
        ),
        ("tooltips_create_no_nswindow", tooltips_create_no_nswindow),
        (
            "text_size_key_equivalents_dispatch_without_a_window",
            text_size_key_equivalents_dispatch_without_a_window,
        ),
        ("app_menu_matches_gpui", app_menu_matches_gpui),
        (
            "edit_menu_uses_standard_selectors",
            edit_menu_uses_standard_selectors,
        ),
        ("view_menu_matches_gpui", view_menu_matches_gpui),
        ("view_menu_text_size_items", view_menu_text_size_items),
        ("app_level_key_equivalents", app_level_key_equivalents),
        (
            "dock_menu_from_a_fake_registry",
            dock_menu_from_a_fake_registry,
        ),
    ];
    let filter = std::env::args().skip(1).find(|arg| !arg.starts_with('-'));
    let mut passed = 0;
    let mut failed = 0;
    for (name, check) in cases {
        if filter.as_ref().is_some_and(|filter| !name.contains(filter)) {
            continue;
        }
        if std::panic::catch_unwind(AssertUnwindSafe(|| check(mtm, &menu, &menus))).is_ok() {
            passed += 1;
            println!("test appkit_shell::{name} ... ok");
        } else {
            failed += 1;
            println!("test appkit_shell::{name} ... FAILED");
        }
    }
    assert!(
        passed + failed > 0,
        "no AppKit check matched the requested route"
    );
    println!(
        "test result: {}. {passed} passed; {failed} failed; 0 ignored",
        if failed == 0 { "ok" } else { "FAILED" }
    );
    assert_eq!(failed, 0);
    assert!(app.windows().iter().all(|window| !window.isVisible()));
}

// Exercise NSH's actual in-content windowDrag handler without ordering a
// window or sending input to any other application.
mod chrome_probe {
    use nsh_base::tooltip;
    use nsh_core::{element::Root, prelude::*};
    use nsh_platform_macos::{
        App, AppOptions, AppView,
        objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, rc::Retained},
        objc2_app_kit::{
            NSBackingStoreType, NSEvent, NSEventModifierFlags, NSEventType, NSView, NSWindow,
            NSWindowStyleMask,
        },
        objc2_foundation::{NSDate, NSPoint, NSRect, NSRunLoop, NSSize},
    };
    use std::{cell::Cell, rc::Rc};
    #[component]
    fn ChromeProbe() -> impl IntoElement {
        view()
            .w(px(500.))
            .h(px(300.))
            .window_drag(true)
            .tooltip(tooltip().open(Some(true)).child(text("Fixture tooltip")))
            .child(text("Fixture drag area"))
    }
    #[derive(Default)]
    pub struct Actions {
        pub zoom: Cell<u32>,
        pub minimize: Cell<u32>,
    }
    define_class!(
        #[unsafe(super(NSWindow))]
        #[thread_kind = MainThreadOnly]
        #[ivars = Actions]
        #[name = "RiWorkNshChromeProbeWindow"]
        pub struct ProbeWindow;
        impl ProbeWindow {
            #[unsafe(method(zoom:))]
            fn zoom(&self, _: Option<&nsh_platform_macos::objc2_foundation::NSObject>) { self.ivars().zoom.set(self.ivars().zoom.get()+1); }
            #[unsafe(method(miniaturize:))]
            fn miniaturize(&self, _: Option<&nsh_platform_macos::objc2_foundation::NSObject>) { self.ivars().minimize.set(self.ivars().minimize.get()+1); }
        }
    );
    pub fn pump() {
        NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.025));
    }
    pub fn open(mtm: MainThreadMarker) -> (Retained<ProbeWindow>, Rc<AppView>) {
        let allocated = ProbeWindow::alloc(mtm).set_ivars(Actions::default());
        let window: Retained<ProbeWindow> = unsafe {
            nsh_platform_macos::objc2::msg_send![
                super(allocated), initWithContentRect: NSRect::new(NSPoint::new(100.,100.),NSSize::new(800.,600.)),
                styleMask: NSWindowStyleMask::Titled | NSWindowStyleMask::Resizable,
                backing: NSBackingStoreType::Buffered, defer: false
            ]
        };
        unsafe {
            window.setReleasedWhenClosed(false);
        }
        let app_view = AppView::new(
            mtm,
            AppOptions {
                app_id: "dev.riwork.acceptance.chrome".into(),
                size: (800, 600),
                data_dir: Some(
                    std::env::temp_dir()
                        .join(format!("riwork-chrome-probe-{}", std::process::id())),
                ),
                ..Default::default()
            },
        );
        window.setContentView(Some(&app_view.view()));
        app_view.load(App::Rust(Root::new::<ChromeProbe>(|| ())));
        for _ in 0..40 {
            pump();
            if descendants(&app_view.view()) >= 4 {
                break;
            }
        }
        assert!(
            descendants(&app_view.view()) >= 4,
            "tooltip and drag target must both mount"
        );
        (window, app_view)
    }
    pub fn descendants(view: &NSView) -> usize {
        1 + view
            .subviews()
            .iter()
            .map(|child| descendants(&child))
            .sum::<usize>()
    }
    pub fn double_click(window: &ProbeWindow, app_view: &AppView) {
        let event=NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            NSEventType::LeftMouseDown,NSPoint::new(100.,500.),NSEventModifierFlags::empty(),0.,window.windowNumber(),None,1,2,1.
        ).unwrap();
        let container = app_view.view();
        let target = container
            .hitTest(NSPoint::new(100., 100.))
            .expect("drag target");
        unsafe { nsh_platform_macos::objc2::msg_send![&*target, mouseDown: &*event] }
        pump();
    }
}

fn titlebar_double_click_reads_the_system_preference(
    mtm: MainThreadMarker,
    _: &NSMenu,
    _: &nsh_menus::HostMenus,
) {
    use nsh_platform_macos::{
        objc2::{DefinedClass, runtime::AnyObject},
        objc2_foundation::{NSDictionary, NSString, NSUserDefaults},
    };
    let defaults = NSUserDefaults::standardUserDefaults();
    let key = NSString::from_str("AppleActionOnDoubleClick");
    // The argument domain is volatile and process-local: never change the
    // user's persisted preference while exercising the real AppKit reader.
    let domain = NSString::from_str("NSArgumentDomain");
    let original = defaults.volatileDomainForName(&domain);
    let (window, view) = chrome_probe::open(mtm);
    for (action, expected) in [("None", (0, 0)), ("Minimize", (0, 1)), ("Maximize", (1, 1))] {
        let value = NSString::from_str(action);
        let values = NSDictionary::<NSString, AnyObject>::from_slices(&[&*key], &[&*value]);
        unsafe {
            defaults.setVolatileDomain_forName(&values, &domain);
        }
        chrome_probe::double_click(&window, &view);
        assert_eq!(
            (window.ivars().zoom.get(), window.ivars().minimize.get()),
            expected,
            "{action}"
        );
    }
    unsafe {
        defaults.setVolatileDomain_forName(&original, &domain);
    }
    view.stop();
    window.close();
}

fn tooltips_create_no_nswindow(mtm: MainThreadMarker, _: &NSMenu, _: &nsh_menus::HostMenus) {
    let app = NSApplication::sharedApplication(mtm);
    let before = app.windows().count();
    let (window, view) = chrome_probe::open(mtm);
    assert_eq!(
        app.windows().count(),
        before + 1,
        "the tooltip is an overlay in the fixture's one window"
    );
    assert!(!window.isVisible());
    view.stop();
    window.close();
}

fn control_tab_reaches_the_nsh_keymap_once(
    mtm: MainThreadMarker,
    _: &NSMenu,
    _: &nsh_menus::HostMenus,
) {
    use nsh_platform_macos::{
        App, Bundle,
        objc2_app_kit::{NSEvent, NSEventType},
        objc2_foundation::NSString,
    };
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };

    let (window, view) = chrome_probe::open(mtm);
    let events = Arc::new(Mutex::new(Vec::<String>::new()));
    let received = events.clone();
    view.host().handle("app.fixture.key", move |call| {
        received
            .lock()
            .unwrap()
            .push(call.data()["direction"].as_str().unwrap().to_owned());
        call.resolve(serde_json::json!({}));
    });
    let ready = Arc::new(AtomicBool::new(false));
    let mounted = ready.clone();
    view.host().handle("app.fixture.ready", move |call| {
        mounted.store(true, Ordering::Release);
        call.resolve(serde_json::json!({}));
    });
    let bundle = Bundle::source(r#"
        import { mount, keymap, host } from "nsh";
        import { element, prop } from "nsh:rt";
        mount(() => {
            const root = element("view");
            prop(root, "style", { width: 500, height: 300 });
            keymap.bind("ctrl+tab", () => host.post("app.fixture.key", { direction: "next" }));
            keymap.bind("ctrl+shift+tab", () => host.post("app.fixture.key", { direction: "previous" }));
            host.post("app.fixture.ready", {});
            return root;
        });
    "#.to_owned()).with_capabilities_json(Some(r#"{"host":["app.*"]}"#)).unwrap();
    view.load(App::Js(bundle));
    for _ in 0..40 {
        chrome_probe::pump();
        if ready.load(Ordering::Acquire) {
            break;
        }
    }
    assert!(
        ready.load(Ordering::Acquire),
        "the real JS keymap fixture must mount"
    );
    // The callback is produced on the engine thread. Drain its corresponding
    // native mount batch before sending exactly one event for each chord.
    for _ in 0..3 {
        chrome_probe::pump();
    }
    assert!(window.makeFirstResponder(None));
    let app = NSApplication::sharedApplication(mtm);
    for (index, flags) in [
        NSEventModifierFlags::Control,
        NSEventModifierFlags::Control | NSEventModifierFlags::Shift,
    ]
    .into_iter()
    .enumerate()
    {
        let tab = NSString::from_str("\t");
        let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            NSEventType::KeyDown, NSPoint::new(0., 0.), flags, 0., window.windowNumber(), None,
            &tab, &tab, false, 48,
        ).unwrap();
        // AppKit's local monitor runs before its responder/key-binding loop.
        // This dispatch is confined to this prohibited-activation test process
        // and explicitly names its one unshown fixture window.
        app.sendEvent(&event);
        for _ in 0..40 {
            chrome_probe::pump();
            if events.lock().unwrap().len() > index {
                break;
            }
        }
        assert_eq!(
            events.lock().unwrap().len(),
            index + 1,
            "each physical Ctrl-Tab chord must dispatch exactly once"
        );
    }
    for _ in 0..3 {
        chrome_probe::pump();
    }
    assert_eq!(*events.lock().unwrap(), ["next", "previous"]);
    assert!(!window.isVisible());
    view.stop();
    window.close();
}
