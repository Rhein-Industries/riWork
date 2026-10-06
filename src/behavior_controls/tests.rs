//! Headless behavior/selection tests only: no Workspace, terminal, service or provider.
use super::*;
use gpui::{Context, Entity, FocusHandle, Focusable, Render, TestAppContext, WindowHandle, WindowOptions, canvas, point, px};
use gpui_kit::base::{Root, TextSelection, TextSelectionHandle, TextSelectionRegistration, TextSelectionScopeId, TestSupportExt};
use gpui_kit::test::TestWindowExt;

struct Harness {
    focus: FocusHandle,
    button_focus: FocusHandle,
    terminal_focus: FocusHandle,
    button_calls: usize,
    parent_calls: usize,
    toggle_calls: usize,
    switch_calls: usize,
    link_calls: usize,
    pressed: bool,
    checked: bool,
    disabled: bool,
    terminal_keys: Vec<String>,
    raw_tabs: usize,
    editor: Entity<crate::text_input::InputState>,
    notes: Entity<crate::text_input::TextareaState>,
    selection: TextSelectionHandle,
    modal_selection: TextSelectionHandle,
    modal_scope: TextSelectionScopeId,
    modal_focus: FocusHandle,
    modal: bool,
}

fn participant(id: &'static str, selection: TextSelectionHandle) -> impl IntoElement + ParentElement {
    div().id(id).test_support().w(px(160.)).h(px(24.)).child(
        canvas(move |bounds, window, cx| {
            let hitbox = window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal);
            selection.register(
                TextSelectionRegistration::new(hitbox, bounds).with_text_bounds(vec![bounds]),
                window, cx,
            );
        }, |_, _, _, _| {}).size_full(),
    )
}

impl Render for Harness {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        TextSelection::activate_scope(if self.modal { self.modal_scope } else { Default::default() }, window, cx);
        let colors = Palette::RIWORK;
        let toggle_owner = cx.entity().downgrade();
        let switch_owner = cx.entity().downgrade();
        div().track_focus(&self.focus).size_full().flex().flex_col()
            .on_click(cx.listener(|owner, _, _, _| owner.parent_calls += 1))
            .capture_key_down(cx.listener(|owner, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key == "tab" && owner.editor.read(cx).focus_handle(cx).is_focused(window) {
                    let composing = owner.editor.update(cx, |state, cx| {
                        gpui::EntityInputHandler::marked_text_range(state, window, cx).is_some()
                    });
                    if !composing { owner.raw_tabs += 1; }
                    cx.stop_propagation();
                }
            }))
            .child(participant("transcript", self.selection.clone()))
            .children(self.modal.then(|| modal_scope(
                div().flex().flex_col()
                    .child(participant("modal-text", self.modal_selection.clone()))
                    .child(button("modal-first", "Back", controls::Button::Secondary, colors).on_click(|_, _, _| {}))
                    .child(button("modal-last", "Confirm", controls::Button::Secondary, colors).on_click(|_, _, _| {})),
                self.modal_scope, "modal-scope", &self.modal_focus,
            )))
            .child(button("button", "Apply", controls::Button::Secondary, colors)
                .track_focus(&self.button_focus).disabled(self.disabled)
                .on_click(cx.listener(|owner, _, _, _| owner.button_calls += 1)))
            .child(toggle("toggle", "Pin", self.pressed, colors).disabled(self.disabled)
                .on_change(move |next, _, _, cx| {
                    let _ = toggle_owner.update(cx, |owner, cx| {
                        owner.pressed = next; owner.toggle_calls += 1; cx.notify();
                    });
                }))
            .child(switch("switch", "Show details", self.checked, colors).disabled(self.disabled)
                .on_change(move |next, _, _, cx| {
                    let _ = switch_owner.update(cx, |owner, cx| {
                        owner.checked = next; owner.switch_calls += 1; cx.notify();
                    });
                }))
            .child(link("link", "Read details", colors).disabled(self.disabled)
                .on_activate(cx.listener(|owner, _, _, _| owner.link_calls += 1)))
            .child(crate::text_input::input("editor", &self.editor, window, cx))
            // An input nested below a terminal context still owns its Copy action.
            .child(div().key_context("Terminal")
                .child(crate::text_input::textarea("notes", &self.notes, window, cx)))
            // This proxy proves key routing without constructing Ghostty or a process.
            .child(div().id("terminal-proxy").key_context("Terminal")
                .track_focus(&self.terminal_focus).h(px(24.))
                .on_key_down(cx.listener(|owner, event: &gpui::KeyDownEvent, _, _| {
                    owner.terminal_keys.push(event.keystroke.key.clone());
                })))
    }
}

fn setup(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.set_global(crate::settings::Settings::default());
        cx.set_global(crate::theme::Appearance {
            selected: crate::theme::ThemeChoice::RiWork, palette: Palette::RIWORK,
            terminal: None, ghostty: None, error: None,
        });
        crate::text_input::init(cx);
        init(cx);
    });
}
fn mount(cx: &mut TestAppContext) -> (WindowHandle<Root>, Entity<Harness>) {
    let (handle, content) = cx.update(|cx| {
        let mut content = None;
        let handle = cx.open_window(WindowOptions::default(), |window, cx| {
            let view = cx.new(|cx| Harness {
                focus: cx.focus_handle(), button_focus: cx.focus_handle(), terminal_focus: cx.focus_handle(),
                button_calls: 0, parent_calls: 0, toggle_calls: 0, switch_calls: 0, link_calls: 0,
                pressed: false, checked: false, disabled: false, terminal_keys: Vec::new(), raw_tabs: 0,
                editor: crate::text_input::single_line("", "Editor", window, cx),
                notes: crate::text_input::multiline("", "Notes", 1, 3, crate::text_input::EnterBehavior::Newline, window, cx),
                selection: TextSelectionHandle::new("transcript", cx),
                modal_selection: TextSelectionHandle::new("modal", cx),
                modal_scope: TextSelectionScopeId::new(), modal_focus: cx.focus_handle(), modal: false,
            });
            content = Some(view.clone());
            cx.new(|cx| Root::new(view, window, cx))
        }).unwrap();
        (handle, content.unwrap())
    });
    turn(cx, handle, |window, cx| {
        window.activate_window();
        let focus = content.read(cx).focus.clone();
        window.focus(&focus, cx);
        window.render_frame(cx);
    });
    (handle, content)
}
fn turn(cx: &mut TestAppContext, handle: WindowHandle<Root>, f: impl FnOnce(&mut Window, &mut App)) {
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        f(window, cx);
    }).unwrap();
    cx.run_until_parked();
}

#[gpui::test]
fn base_button_pointer_keyboard_activate_once_and_disabled_does_not_bubble(cx: &mut TestAppContext) {
    setup(cx); let (handle, content) = mount(cx);
    for key in [None, Some("enter"), Some("space")] {
        turn(cx, handle, |window, cx| {
            if let Some(key) = key { window.press(key, cx); } else { window.click("button", cx); }
        });
    }
    assert_eq!(content.read_with(cx, |owner, _| owner.button_calls), 3);
    content.update(cx, |owner, cx| { owner.disabled = true; owner.parent_calls = 0; cx.notify(); });
    turn(cx, handle, |window, cx| { window.click("button", cx); window.press("enter", cx); window.press("space", cx); });
    assert_eq!(content.read_with(cx, |owner, _| (owner.button_calls, owner.parent_calls)), (3, 0));
}

#[gpui::test]
fn root_tab_traversal_uses_base_focus_and_skips_disabled_controls(cx: &mut TestAppContext) {
    setup(cx); let (handle, content) = mount(cx);
    turn(cx, handle, |window, cx| window.click("button", cx));
    for (key, target) in [("tab", "toggle"), ("tab", "switch"), ("shift-tab", "toggle")] {
        turn(cx, handle, |window, cx| window.press(key, cx));
        turn(cx, handle, |window, _| assert_eq!(window.find(target).focused(), Some(true)));
    }
    content.update(cx, |owner, cx| { owner.disabled = true; cx.notify(); });
    turn(cx, handle, |window, cx| {
        window.focus(&content.read(cx).focus, cx);
        window.render_frame(cx);
        window.press("tab", cx);
    });
    turn(cx, handle, |window, cx| {
        assert!(content.read(cx).editor.read(cx).focus_handle(cx).is_focused(window));
    });
    assert_eq!(content.read_with(cx, |owner, _| (owner.button_calls, owner.toggle_calls, owner.switch_calls, owner.link_calls)), (1, 0, 0, 0));
}

#[gpui::test]
fn root_modal_tab_controls_stay_within_base_focus_trap(cx: &mut TestAppContext) {
    setup(cx); let (handle, content) = mount(cx);
    content.update(cx, |owner, cx| { owner.modal = true; cx.notify(); });
    turn(cx, handle, |window, cx| window.click("modal-first", cx));
    for (key, target) in [("tab", "modal-last"), ("tab", "modal-first"), ("shift-tab", "modal-last")] {
        turn(cx, handle, |window, cx| window.press(key, cx));
        turn(cx, handle, |window, _| assert_eq!(window.find(target).focused(), Some(true)));
    }
    assert_eq!(content.read_with(cx, |owner, _| (owner.button_calls, owner.toggle_calls, owner.switch_calls, owner.link_calls)), (0, 0, 0, 0));
}

#[gpui::test]
fn base_toggle_switch_link_share_pointer_and_keyboard_owner_callbacks(cx: &mut TestAppContext) {
    setup(cx); let (handle, content) = mount(cx);
    for id in ["toggle", "switch", "link"] {
        turn(cx, handle, |window, cx| window.click(id, cx));
        turn(cx, handle, |window, cx| window.press("enter", cx));
        turn(cx, handle, |window, cx| window.press("space", cx));
    }
    assert_eq!(content.read_with(cx, |owner, _| (owner.toggle_calls, owner.switch_calls, owner.link_calls, owner.pressed, owner.checked)), (3, 3, 3, true, true));
    content.update(cx, |owner, cx| { owner.disabled = true; cx.notify(); });
    for id in ["toggle", "switch", "link"] { turn(cx, handle, |window, cx| window.click(id, cx)); }
    assert_eq!(content.read_with(cx, |owner, _| (owner.toggle_calls, owner.switch_calls, owner.link_calls)), (3, 3, 3));
}

fn node<B: Primitive>(control: Control<B>, window: &mut Window, cx: &mut App) -> accesskit::Node {
    let element = control.render(window, cx).into_element();
    let mut node = accesskit::Node::new(element.a11y_role().expect("semantic role"));
    element.write_a11y_info(&mut node); node
}
#[gpui::test]
fn accessible_nodes_keep_names_states_and_only_enabled_click_actions(cx: &mut TestAppContext) {
    setup(cx); let (handle, _) = mount(cx);
    turn(cx, handle, |window, cx| {
        let colors = Palette::RIWORK;
        for disabled in [false, true] {
            let b = node(button("node-button", "Apply", controls::Button::Secondary, colors).disabled(disabled).on_click(|_, _, _| {}), window, cx);
            assert_eq!(b.role(), gpui::Role::Button); assert_eq!(b.label(), Some("Apply"));
            assert_eq!(b.is_disabled(), disabled); assert_eq!(b.supports_action(accesskit::Action::Click), !disabled);
            let t = node(toggle("node-toggle", "Pin", true, colors).disabled(disabled).on_change(|_, _, _, _| {}), window, cx);
            assert_eq!(t.role(), gpui::Role::Button); assert_eq!(t.label(), Some("Pin"));
            assert_eq!(t.toggled(), Some(accesskit::Toggled::True)); assert_eq!(t.is_disabled(), disabled);
            assert_eq!(t.supports_action(accesskit::Action::Click), !disabled);
            let s = node(switch("node-switch", "Show details", false, colors).disabled(disabled).on_change(|_, _, _, _| {}), window, cx);
            assert_eq!(s.role(), gpui::Role::Switch); assert_eq!(s.label(), Some("Show details"));
            assert_eq!(s.toggled(), Some(accesskit::Toggled::False)); assert_eq!(s.is_disabled(), disabled);
            assert_eq!(s.supports_action(accesskit::Action::Click), !disabled);
            let l = node(link("node-link", "Read details", colors).disabled(disabled).on_activate(|_, _, _| {}), window, cx);
            assert_eq!(l.role(), gpui::Role::Link); assert_eq!(l.label(), Some("Read details"));
            assert_eq!(l.is_disabled(), disabled); assert_eq!(l.supports_action(accesskit::Action::Click), !disabled);
        }
    });
}

#[gpui::test]
fn root_preserves_content_identity_and_selection_is_window_local(cx: &mut TestAppContext) {
    setup(cx); let (first, first_content) = mount(cx); let (second, _) = mount(cx);
    cx.update_window(first.into(), |view, _, cx| {
        assert_eq!(content_view::<Harness>(view, cx).unwrap().entity_id(), first_content.entity_id());
    }).unwrap();
    turn(cx, first, |window, cx| {
        let bounds = window.find("transcript").bounds();
        window.drag(bounds.origin + point(px(2.), px(10.)), bounds.origin + point(px(80.), px(10.)), cx);
    });
    turn(cx, first, |window, cx| {
        assert_eq!(TextSelection::selected_text(window, cx), "transcript");
        window.press("cmd-c", cx);
    });
    assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), "transcript");
    turn(cx, second, |window, cx| {
        assert!(!TextSelection::has_selection(window, cx)); window.press("cmd-c", cx);
    });
    assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), "transcript");
}

#[gpui::test]
fn root_modal_scope_and_input_terminal_keys_do_not_cross_owner_boundaries(cx: &mut TestAppContext) {
    setup(cx); let (handle, content) = mount(cx);
    turn(cx, handle, |window, cx| {
        let bounds = window.find("transcript").bounds();
        window.drag(bounds.origin + point(px(2.), px(10.)), bounds.origin + point(px(80.), px(10.)), cx);
    });
    content.update(cx, |owner, cx| { owner.modal = true; cx.notify(); });
    turn(cx, handle, |window, cx| {
        window.render_frame(cx); assert!(!TextSelection::has_selection(window, cx));
        let bounds = window.find("modal-text").bounds();
        window.drag(bounds.origin + point(px(2.), px(10.)), bounds.origin + point(px(80.), px(10.)), cx);
    });
    turn(cx, handle, |window, cx| assert_eq!(TextSelection::selected_text(window, cx), "modal"));
    content.update(cx, |owner, cx| { owner.modal = false; cx.notify(); });
    turn(cx, handle, |window, cx| {
        window.render_frame(cx); assert!(!TextSelection::has_selection(window, cx));
        window.click("editor", cx);
        let editor = content.read(cx).editor.clone();
        let mut handler = gpui::ElementInputHandler::new(window.find("editor").bounds(), editor);
        gpui::InputHandler::replace_and_mark_text_in_range(&mut handler, None, "日本", Some(2..2), window, cx);
        window.render_frame(cx); window.press("tab", cx);
    });
    assert_eq!(content.read_with(cx, |owner, _| owner.raw_tabs), 0);
    turn(cx, handle, |window, cx| {
        let editor = content.read(cx).editor.clone();
        assert!(editor.read(cx).focus_handle(cx).is_focused(window));
        editor.update(cx, |state, cx| gpui::EntityInputHandler::unmark_text(state, window, cx));
        window.press("tab", cx);
    });
    assert_eq!(content.read_with(cx, |owner, _| owner.raw_tabs), 1);
    turn(cx, handle, |window, cx| {
        window.click("notes", cx); window.press("tab", cx);
    });
    turn(cx, handle, |window, cx| {
        // Base's default textarea tab policy is two soft spaces, not a hard tab.
        assert_eq!(content.read(cx).notes.read(cx).value(), "  ");
        window.press("cmd-a", cx);
        window.press("cmd-c", cx);
    });
    assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), "  ");
    turn(cx, handle, |window, cx| {
        let terminal_focus = content.read(cx).terminal_focus.clone();
        window.focus(&terminal_focus, cx); window.render_frame(cx);
        for key in ["tab", "shift-tab", "cmd-c"] { window.press(key, cx); }
        assert!(terminal_focus.is_focused(window));
    });
    assert_eq!(content.read_with(cx, |owner, _| owner.terminal_keys.clone()), ["tab", "tab", "c"]);
}
