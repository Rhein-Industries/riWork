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
    assert_eq!(labels(&edit), ["Cut", "Copy", "Paste", "---", "Select All"]);
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
    for index in [3, 4, 5, 9, 10, 11] {
        assert!(
            !view.itemAtIndex(index).unwrap().isEnabled(),
            "deferred row {index} must stay disabled"
        );
    }
    for index in [0, 1, 6, 7, 13, 14, 15] {
        assert!(view.itemAtIndex(index).unwrap().isEnabled());
    }
    // AppKit must not silently re-enable deferred commands during validation.
    view.update();
    for index in [3, 4, 5, 9, 10, 11] {
        assert!(!view.itemAtIndex(index).unwrap().isEnabled());
    }
}

fn view_menu_text_size_items(_: MainThreadMarker, menu: &NSMenu, _: &nsh_menus::HostMenus) {
    let view = submenu(menu, 2);
    for (index, text) in [(13, "="), (14, "-"), (15, "0")] {
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
