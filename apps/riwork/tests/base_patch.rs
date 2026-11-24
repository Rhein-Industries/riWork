//! The vendored `nsh-base` patch (`apps/riwork/vendor/nsh-base/RIWORK.md`) as the app's TSX
//! uses it, on the real headless engine: what `DockArea.ts` and `SettingsFocus.tsx` widen the
//! stock declarations with is declared by the patched registry in these shapes, a tab drag
//! reports `onDragChange` around its layout edit, and Button and Switch report the keyboard
//! focus. No window, home or session is opened.
use std::sync::Mutex;

use nsh_base::{
    button,
    dock::{DockAreaLayout, DockLayout, DockPanel, DockTab, dock_area},
    switch,
};
use nsh_core::{
    Size,
    element::Root,
    geometry::point,
    platform::{Idiom, Os},
    prelude::*,
};
use nsh_engine::EngineConfig;
use nsh_testing::Headless;

/// `C` on a Mac's headless surface: Tab stops at views that take presses.
fn start<C: Component<Props = ()>>(shown: &str) -> Headless {
    let mut config = EngineConfig {
        viewport: Size::new(px(800.0), px(600.0)),
        ..EngineConfig::new(Root::of::<C>())
    };
    config.environment.os = Os::MacOs;
    config.environment.idiom = Idiom::Desktop;
    let mut app = Headless::start(config).expect("the engine starts");
    app.wait_for(shown).unwrap();
    app
}

/// The declarations of one export of `nsh/base` (`export interface NameProps { … }`).
fn interface<'a>(declarations: &'a str, name: &str) -> &'a str {
    let start = declarations
        .find(&format!("export interface {name} "))
        .unwrap_or_else(|| panic!("nsh/base declares {name}"));
    let end = declarations[start..].find("\n}\n").expect("the interface ends");
    &declarations[start..start + end]
}

#[test]
fn the_patched_registry_declares_what_the_app_s_adapters_widen() {
    let declarations = nsh_core::element::declarations(&nsh_base::registry(), &[]);
    let dock = interface(&declarations, "DockAreaProps");
    for member in [
        "  dividerSize?: number;",
        "  fixedPanels?: (string)[];",
        "  draggableTabBars?: (number)[];",
        "  renderTabBarPrefix?: (value: number) => JSX.Element;",
        "  renderTabBarSuffix?: (value: number) => JSX.Element;",
        // One object, as every NSH 0.4+ event: `#[args(dragging)]` names its field.
        "  onDragChange?: (event: { dragging: boolean }) => void;",
    ] {
        assert!(dock.contains(member), "DockAreaProps has `{member}`:\n{dock}");
    }
    for name in ["ButtonProps", "SwitchProps"] {
        let props = interface(&declarations, name);
        for member in ["  onFocus?: () => void;", "  onBlur?: () => void;"] {
            assert!(props.contains(member), "{name} has `{member}`:\n{props}");
        }
    }
}

static DRAG: Mutex<Vec<String>> = Mutex::new(Vec::new());

#[component]
fn Dock() -> impl IntoElement {
    let layout = DockAreaLayout::new(
        DockLayout::h_split()
            .child(DockLayout::tabs().panel("a").panel("b"), Some(px(400.0)))
            .child(DockLayout::tabs().panel("c"), None),
    );
    view().w(px(800.0)).h(px(600.0)).child(
        dock_area()
            .default_layout(layout)
            .compact(false)
            .divider_size(px(5.0))
            .tab_bar_class("flex-row h-6")
            .render_tab(|tab: &DockTab| {
                view()
                    .test_id(&format!("tab-{}", tab.panel))
                    .w(px(80.0))
                    .h(px(24.0))
                    .child(text(format!("Tab {}", tab.panel)))
            })
            .render_panel(|panel: &DockPanel| {
                view().flex_1().child(text(format!("Panel {}", panel.id)))
            })
            .on_drag_change(|dragging: &bool| {
                DRAG.lock().unwrap().push(format!("drag {dragging}"))
            })
            .on_layout_change(|_: &DockAreaLayout| DRAG.lock().unwrap().push("layout".into()))
            .size_full(),
    )
}

#[test]
fn a_tab_drag_reports_its_start_then_the_layout_then_its_end() {
    let mut app = start::<Dock>("Tab a");
    assert!(DRAG.lock().unwrap().is_empty());
    // From the middle of tab a into the middle of c's group: it merges there.
    app.drag("id:tab-a", point(px(560.0), px(288.0))).unwrap();
    assert_eq!(*DRAG.lock().unwrap(), ["drag true", "layout", "drag false"]);
}

static FOCUS: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

fn focus(event: &'static str) -> impl Fn() + 'static {
    move || FOCUS.lock().unwrap().push(event)
}

#[component]
fn Controls() -> impl IntoElement {
    view()
        .child(
            button()
                .on_focus(focus("button focus"))
                .on_blur(focus("button blur"))
                .child(text("Button")),
        )
        .child(
            switch()
                .on_focus(focus("switch focus"))
                .on_blur(focus("switch blur"))
                .child(text("Switch")),
        )
        .child(button().child(text("Last")))
}

#[test]
fn button_and_switch_report_the_keyboard_focus_coming_and_going() {
    let mut app = start::<Controls>("Last");
    assert!(FOCUS.lock().unwrap().is_empty());
    app.press_key("tab").unwrap();
    assert_eq!(*FOCUS.lock().unwrap(), ["button focus"]);
    app.press_key("tab").unwrap();
    assert_eq!(*FOCUS.lock().unwrap(), ["button focus", "button blur", "switch focus"]);
    app.press_key("tab").unwrap();
    assert_eq!(
        *FOCUS.lock().unwrap(),
        ["button focus", "button blur", "switch focus", "switch blur"]
    );
}
