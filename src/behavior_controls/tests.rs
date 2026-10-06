//! Headless behavior/selection tests only: no Workspace, terminal, service or provider.
use super::*;
use gpui::{Context, Entity, FocusHandle, Focusable, Render, TestAppContext, WindowHandle, WindowOptions, canvas, point, px};
use gpui_kit::base::{Root, TextSelection, TextSelectionHandle, TextSelectionRegistration, TextSelectionScopeId, TestSupportExt};
use gpui_kit::test::TestWindowExt;

struct Harness {
    focus: FocusHandle,
    button_focus: FocusHandle,
    terminal_focus: FocusHandle,
    chat_focus: FocusHandle,
    button_calls: usize,
    parent_calls: usize,
    toggle_calls: usize,
    switch_calls: usize,
    link_calls: usize,
    pressed: bool,
    checked: bool,
    disabled: bool,
    terminal_keys: Vec<String>,
    terminal_menu_guards: usize,
    raw_tabs: usize,
    transcript_copies: usize,
    cancels: usize,
    modal_return: FocusReturn,
    legacy_scope_capture: bool,
    scope_capture_enabled: bool,
    editor: Entity<crate::text_input::InputState>,
    next_editor: Entity<crate::text_input::InputState>,
    notes: Entity<crate::text_input::TextareaState>,
    selection: TextSelectionHandle,
    chat_scope: TextSelectionScopeId,
    other_selection: TextSelectionHandle,
    other_scope: TextSelectionScopeId,
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
        sync_modal_scope(self.modal.then_some(self.modal_scope), window, cx);
        let colors = Palette::RIWORK;
        let toggle_owner = cx.entity().downgrade();
        let switch_owner = cx.entity().downgrade();
        div().id("behavior-harness").track_focus(&self.focus).size_full().flex().flex_col()
            .on_click(cx.listener(|owner, _, _, _| owner.parent_calls += 1))
            .on_key_down(cx.listener(|owner, event: &gpui::KeyDownEvent, window, cx| {
                if protect_terminal_edit_menu_fallback(event, window, cx) { owner.terminal_menu_guards += 1; }
            }))
            .on_action(cx.listener(|owner, _: &base::input::Copy, window, cx| {
                let text = TextSelection::selected_text(window, cx);
                if text.is_empty() { cx.propagate(); return; }
                owner.transcript_copies += 1;
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
            }))
            .capture_key_down(cx.listener(|owner, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" && owner.modal {
                    owner.modal = false; owner.cancels += 1;
                    owner.modal_return.restore_within(&owner.focus, window, cx);
                    cx.stop_propagation(); cx.notify(); return;
                }
                let first = owner.editor.read(cx).focus_handle(cx).is_focused(window);
                let next = owner.next_editor.read(cx).focus_handle(cx).is_focused(window);
                if event.keystroke.key == "tab" && (first || next) {
                    let input = if first { owner.editor.clone() } else { owner.next_editor.clone() };
                    let composing = input.update(cx, |state, cx| {
                        gpui::EntityInputHandler::marked_text_range(state, window, cx).is_some()
                    });
                    if !composing {
                        owner.raw_tabs += 1;
                        let target = if event.keystroke.modifiers.shift { &owner.editor } else { &owner.next_editor };
                        target.read(cx).focus_handle(cx).focus(window, cx);
                    }
                    cx.stop_propagation();
                }
            }))
            .child(div().key_context("ChatView").track_focus(&self.chat_focus.clone().tab_stop(false))
                .capture_any_mouse_down(cx.listener(|owner, event: &gpui::MouseDownEvent, window, cx| {
                    if owner.scope_capture_enabled && event.button == gpui::MouseButton::Left {
                        if owner.legacy_scope_capture { TextSelection::activate_scope(owner.chat_scope, window, cx); }
                        else { activate_content_scope(owner.chat_scope, window, cx); }
                    }
                }))
                .child(selection_scope(participant("transcript", self.selection.clone()), self.chat_scope)))
            .child(div().capture_any_mouse_down(cx.listener(|owner, event: &gpui::MouseDownEvent, window, cx| {
                if owner.scope_capture_enabled && event.button == gpui::MouseButton::Left { activate_content_scope(owner.other_scope, window, cx); }
            })).child(selection_scope(participant("other-transcript", self.other_selection.clone()), self.other_scope)))
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
            .child(crate::text_input::input("next-editor", &self.next_editor, window, cx))
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
            let view = cx.new(|cx| {
                let chat_focus = cx.focus_handle();
                let focus_selection = chat_focus.clone();
                let selection = TextSelectionHandle::new("transcript", cx);
                selection.focus_with(move |window, cx| focus_selection.focus(window, cx), cx);
                Harness {
                focus: cx.focus_handle(), button_focus: cx.focus_handle(), terminal_focus: cx.focus_handle(),
                chat_focus,
                button_calls: 0, parent_calls: 0, toggle_calls: 0, switch_calls: 0, link_calls: 0,
                pressed: false, checked: false, disabled: false, terminal_keys: Vec::new(), terminal_menu_guards: 0, raw_tabs: 0,
                transcript_copies: 0, cancels: 0, modal_return: Default::default(), legacy_scope_capture: false, scope_capture_enabled: true,
                editor: crate::text_input::single_line("", "Editor", window, cx),
                next_editor: crate::text_input::single_line("", "Next editor", window, cx),
                notes: crate::text_input::multiline("", "Notes", 1, 3, crate::text_input::EnterBehavior::Newline, window, cx),
                selection, chat_scope: TextSelectionScopeId::new(),
                other_selection: TextSelectionHandle::new("other transcript", cx), other_scope: TextSelectionScopeId::new(),
                modal_selection: TextSelectionHandle::new("modal", cx),
                modal_scope: TextSelectionScopeId::new(), modal_focus: cx.focus_handle(), modal: false,
                }
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
        let focus = content.read(cx).focus.clone();
        window.focus(&focus, cx);
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
    turn(cx, handle, |window, cx| {
        let focus = content.read(cx).button_focus.clone();
        window.focus(&focus, cx);
        content.update(cx, |owner, cx| owner.modal_return.capture(window, cx));
    });
    content.update(cx, |owner, cx| { owner.modal = true; cx.notify(); });
    turn(cx, handle, |window, cx| window.click("modal-first", cx));
    for (key, target) in [("tab", "modal-last"), ("tab", "modal-first"), ("shift-tab", "modal-last")] {
        turn(cx, handle, |window, cx| window.press(key, cx));
        turn(cx, handle, |window, _| assert_eq!(window.find(target).focused(), Some(true)));
    }
    assert_eq!(content.read_with(cx, |owner, _| (owner.button_calls, owner.toggle_calls, owner.switch_calls, owner.link_calls)), (0, 0, 0, 0));
    turn(cx, handle, |window, cx| window.press("escape", cx));
    assert_eq!(content.read_with(cx, |owner, _| (owner.modal, owner.cancels)), (false, 1));
    turn(cx, handle, |window, cx| assert!(content.read(cx).button_focus.is_focused(window)));
    turn(cx, handle, |window, cx| content.update(cx, |owner, cx| owner.modal_return.capture(window, cx)));
    content.update(cx, |owner, cx| { owner.disabled = true; cx.notify(); });
    turn(cx, handle, |window, cx| {
        // The invoker's entity/handle is alive, but disabling it removed its focus node.
        content.update(cx, |owner, cx| {
            assert!(!owner.modal_return.restore_within(&owner.focus, window, cx));
            owner.focus.focus(window, cx);
        });
    });
    turn(cx, handle, |window, cx| assert!(content.read(cx).focus.is_focused(window)));
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
    assert_eq!(content.read_with(cx, |owner, _| owner.terminal_menu_guards), 0);
    turn(cx, handle, |window, cx| {
        let terminal_focus = content.read(cx).terminal_focus.clone();
        window.focus(&terminal_focus, cx); window.render_frame(cx);
        for key in ["tab", "shift-tab", "cmd-c"] { window.press(key, cx); }
        assert!(terminal_focus.is_focused(window));
    });
    assert_eq!(content.read_with(cx, |owner, _| owner.terminal_keys.clone()), ["tab", "tab", "c"]);
}

fn drag_transcript(cx: &mut TestAppContext, handle: WindowHandle<Root>, id: &'static str) {
    turn(cx, handle, |window, cx| {
        let bounds = window.find(id).bounds();
        window.drag(bounds.origin + point(px(2.), px(10.)), bounds.origin + point(px(80.), px(10.)), cx);
    });
}
fn menu_action(label: &str) -> Box<dyn gpui::Action> {
    edit_menu().items.into_iter().find_map(|item| match item {
        gpui::MenuItem::Action { name, action, os_action: Some(_), .. } if &*name == label => Some(action),
        _ => None,
    }).expect("native Edit action")
}

#[gpui::test]
fn parent_rerender_preserves_nondefault_transcript_scope_after_pointer_selection(cx: &mut TestAppContext) {
    setup(cx);
    for legacy in [false, true] {
        let (handle, content) = mount(cx);
        content.update(cx, |owner, cx| { owner.legacy_scope_capture = legacy; cx.notify(); });
        drag_transcript(cx, handle, "transcript");
        // Force the actual parent render path after the pointer-up/effect turn.
        content.update(cx, |_, cx| cx.notify());
        turn(cx, handle, |window, cx| assert_eq!(TextSelection::selected_text(window, cx), "transcript"));
        turn(cx, handle, |window, cx| window.press("cmd-c", cx));
        assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), "transcript");
        turn(cx, handle, |window, cx| window.dispatch_action(menu_action("Copy"), cx));
        assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), "transcript");
        assert_eq!(content.read_with(cx, |owner, _| owner.transcript_copies), 2);
    }
}

#[gpui::test]
fn two_visible_transcript_scopes_switch_on_first_pointer_gesture(cx: &mut TestAppContext) {
    setup(cx); let (handle, content) = mount(cx);
    for (id, expected) in [("transcript", "transcript"), ("other-transcript", "other transcript"), ("transcript", "transcript")] {
        drag_transcript(cx, handle, id);
        content.update(cx, |_, cx| cx.notify());
        turn(cx, handle, |window, cx| assert_eq!(TextSelection::selected_text(window, cx), expected));
        turn(cx, handle, |window, cx| window.dispatch_action(menu_action("Copy"), cx));
        assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), expected);
    }
}

#[gpui::test]
fn modal_transition_restores_content_scope_and_retired_scope_stays_inactive(cx: &mut TestAppContext) {
    setup(cx); let (handle, content) = mount(cx);
    drag_transcript(cx, handle, "transcript");
    content.update(cx, |owner, cx| { owner.modal = true; cx.notify(); });
    drag_transcript(cx, handle, "modal-text");
    turn(cx, handle, |window, cx| {
        // A background scope request cannot displace the active modal.
        activate_content_scope(content.read(cx).other_scope, window, cx);
        assert_eq!(TextSelection::selected_text(window, cx), "modal");
    });
    content.update(cx, |owner, cx| { owner.modal = false; owner.scope_capture_enabled = false; cx.notify(); });
    // No child capture may activate scope now: this proves modal-close restoration.
    drag_transcript(cx, handle, "transcript");
    turn(cx, handle, |window, cx| assert_eq!(TextSelection::selected_text(window, cx), "transcript"));
    content.update(cx, |owner, cx| { owner.modal = true; cx.notify(); });
    turn(cx, handle, |window, cx| retire_content_scope(content.read(cx).chat_scope, window, cx));
    content.update(cx, |owner, cx| { owner.modal = false; cx.notify(); });
    drag_transcript(cx, handle, "transcript");
    turn(cx, handle, |window, cx| assert!(!TextSelection::has_selection(window, cx)));
}

#[gpui::test]
fn single_line_owner_tab_and_shift_tab_preserve_ime_and_navigate_once(cx: &mut TestAppContext) {
    setup(cx); let (handle, content) = mount(cx);
    turn(cx, handle, |window, cx| {
        window.click("editor", cx);
        let editor = content.read(cx).editor.clone();
        let mut handler = gpui::ElementInputHandler::new(window.find("editor").bounds(), editor);
        gpui::InputHandler::replace_and_mark_text_in_range(&mut handler, None, "日本", Some(2..2), window, cx);
    });
    for key in ["tab", "shift-tab"] { turn(cx, handle, |window, cx| window.press(key, cx)); }
    assert_eq!(content.read_with(cx, |owner, _| owner.raw_tabs), 0);
    turn(cx, handle, |window, cx| {
        let editor = content.read(cx).editor.clone();
        assert!(editor.read(cx).focus_handle(cx).is_focused(window));
        editor.update(cx, |state, cx| {
            assert!(gpui::EntityInputHandler::marked_text_range(state, window, cx).is_some());
            gpui::EntityInputHandler::unmark_text(state, window, cx);
        });
    });
    turn(cx, handle, |window, cx| window.press("tab", cx));
    assert_eq!(content.read_with(cx, |owner, _| owner.raw_tabs), 1);
    turn(cx, handle, |window, cx| assert!(content.read(cx).next_editor.read(cx).focus_handle(cx).is_focused(window)));
    turn(cx, handle, |window, cx| window.press("shift-tab", cx));
    assert_eq!(content.read_with(cx, |owner, _| owner.raw_tabs), 2);
    turn(cx, handle, |window, cx| assert!(content.read(cx).editor.read(cx).focus_handle(cx).is_focused(window)));
}

#[gpui::test]
fn native_edit_menu_uses_base_actions_and_preserves_editor_priority(cx: &mut TestAppContext) {
    setup(cx); let (handle, content) = mount(cx);
    assert!(menu_action("Copy").partial_eq(&base::input::Copy));
    assert!(menu_action("Cut").partial_eq(&base::input::Cut));
    assert!(menu_action("Paste").partial_eq(&base::input::Paste));
    assert!(menu_action("Select All").partial_eq(&base::input::SelectAll));
    content.update(cx, |owner, cx| owner.selection.set_fallback_copy_text("  transcript  ", cx));
    drag_transcript(cx, handle, "transcript");
    turn(cx, handle, |window, cx| window.dispatch_action(menu_action("Copy"), cx));
    assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), "  transcript  ");
    turn(cx, handle, |window, cx| window.press("cmd-c", cx));
    assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), "  transcript  ");
    turn(cx, handle, |window, cx| {
        let editor = content.read(cx).editor.clone();
        editor.update(cx, |state, cx| state.set_value("editor bytes", window, cx));
        editor.read(cx).focus_handle(cx).focus(window, cx);
    });
    turn(cx, handle, |window, cx| window.dispatch_action(menu_action("Select All"), cx));
    turn(cx, handle, |window, cx| window.dispatch_action(menu_action("Copy"), cx));
    assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), "editor bytes");
    assert_eq!(content.read_with(cx, |owner, _| owner.transcript_copies), 2);
    turn(cx, handle, |window, cx| window.dispatch_action(menu_action("Cut"), cx));
    assert_eq!(content.read_with(cx, |owner, cx| owner.editor.read(cx).value()), "");
    turn(cx, handle, |window, cx| window.dispatch_action(menu_action("Paste"), cx));
    assert_eq!(content.read_with(cx, |owner, cx| owner.editor.read(cx).value()), "editor bytes");
    assert_eq!(content.read_with(cx, |owner, _| owner.terminal_menu_guards), 0);
    // Editor-only bindings remain absent in a terminal context; Copy is scoped-unbound.
    turn(cx, handle, |window, cx| {
        let focus = content.read(cx).terminal_focus.clone();
        focus.focus(window, cx); window.render_frame(cx);
        for key in ["cmd-x", "cmd-c", "cmd-v", "cmd-a", "tab", "shift-tab"] { window.press(key, cx); }
    });
    assert_eq!(content.read_with(cx, |owner, _| owner.terminal_keys.clone()), ["x", "c", "v", "a", "tab", "tab"]);
    assert_eq!(content.read_with(cx, |owner, _| owner.terminal_menu_guards), 4);
    turn(cx, handle, |window, cx| window.press("cmd-shift-c", cx));
    assert_eq!(content.read_with(cx, |owner, _| owner.terminal_menu_guards), 4);
}

struct ContentControls {
    calls: Vec<&'static str>, choice: usize, pressed: bool, checked: bool, disabled: bool,
}
impl Render for ContentControls {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let toggle_owner = cx.entity().downgrade();
        let switch_owner = cx.entity().downgrade();
        div().size_full().flex().flex_col()
            .child(button_content("content-row", "Choose item", "Visible row")
                .w(px(180.)).h(px(30.)).hover(|style| style.bg(rgb(0x334455)))
                .focus_visible(|style| style.border_1().border_color(rgb(0xffffff)))
                .on_click(cx.listener(|owner, _, _, _| owner.calls.push("row")))
                .child(button_content("content-close", "Close item", "×")
                    .size(px(24.)).disabled(self.disabled)
                    .hover(|style| style.bg(rgb(0x445566)))
                    .on_click(cx.listener(|owner, _, _, _| owner.calls.push("close"))))
                .child(radio_content("nested-disabled-radio", "Unavailable destination", "Unavailable", false)
                    .disabled(true).w(px(60.)).h(px(24.)).on_change(|_, _, _, _| panic!("disabled radio activated"))))
            .child(toggle_content("content-toggle", "Pin item", "Owner pin mark", self.pressed)
                .w(px(180.)).h(px(30.)).disabled(self.disabled)
                .hover(|style| style.bg(rgb(0x334455)))
                .on_change(move |next, _, _, cx| { let _ = toggle_owner.update(cx, |owner, cx| {
                    owner.pressed = next; owner.calls.push("toggle"); cx.notify();
                }); }))
            .child(switch_content("content-switch", "Show item", "Owner switch mark", self.checked)
                .w(px(180.)).h(px(30.)).disabled(self.disabled)
                .hover(|style| style.bg(rgb(0x334455)))
                .on_change(move |next, _, _, cx| { let _ = switch_owner.update(cx, |owner, cx| {
                    owner.checked = next; owner.calls.push("switch"); cx.notify();
                }); }))
            .child(base::RadioGroup::new("content-choices").aria_label("Destination").children((0..2).map(|choice| {
                let owner = cx.entity().downgrade();
                radio_content(format!("choice-{choice}"), format!("Destination {choice}"), format!("Visible {choice}"), self.choice == choice)
                    .w(px(180.)).h(px(30.)).disabled(self.disabled).set_position(choice + 1, 2)
                    .hover(|style| style.bg(rgb(0x334455)))
                    .on_change(move |next, _, _, cx| { if next { let _ = owner.update(cx, |owner, cx| {
                        owner.choice = choice; owner.calls.push("radio"); cx.notify();
                    }); } })
            })))
    }
}
#[gpui::test]
fn content_controls_keep_caller_hover_nested_isolation_and_radio_exclusivity(cx: &mut TestAppContext) {
    setup(cx);
    let (handle, content) = cx.update(|cx| {
        let mut content = None;
        let handle = cx.open_window(WindowOptions::default(), |window, cx| {
            let view = cx.new(|_| ContentControls { calls: Vec::new(), choice: 0, pressed: false, checked: false, disabled: false });
            content = Some(view.clone()); cx.new(|cx| Root::new(view, window, cx))
        }).unwrap();
        (handle, content.unwrap())
    });
    turn(cx, handle, |window, _| window.activate_window());
    turn(cx, handle, |window, cx| window.click("content-close", cx));
    turn(cx, handle, |window, cx| window.press("enter", cx));
    assert_eq!(content.read_with(cx, |owner, _| owner.calls.clone()), ["close", "close"]);
    turn(cx, handle, |window, cx| window.click("nested-disabled-radio", cx));
    assert_eq!(content.read_with(cx, |owner, _| owner.calls.clone()), ["close", "close"]);
    for id in ["content-toggle", "content-switch"] {
        turn(cx, handle, |window, cx| window.click(id, cx));
        turn(cx, handle, |window, cx| window.press("space", cx));
    }
    assert_eq!(content.read_with(cx, |owner, _| (owner.pressed, owner.checked)), (false, false));
    turn(cx, handle, |window, cx| window.click("choice-1", cx));
    turn(cx, handle, |window, cx| window.press("enter", cx));
    turn(cx, handle, |window, cx| window.press("space", cx));
    assert_eq!(content.read_with(cx, |owner, _| owner.choice), 1);
    assert_eq!(content.read_with(cx, |owner, _| owner.calls.iter().filter(|call| **call == "radio").count()), 1);
    let before = content.read_with(cx, |owner, _| owner.calls.clone());
    content.update(cx, |owner, cx| { owner.disabled = true; cx.notify(); });
    for id in ["content-close", "content-toggle", "content-switch", "choice-0"] {
        turn(cx, handle, |window, cx| window.click(id, cx));
    }
    assert_eq!(content.read_with(cx, |owner, _| owner.calls.clone()), before);
}

#[gpui::test]
fn content_adapter_ax_names_disabled_radio_and_focus_scope_metadata(cx: &mut TestAppContext) {
    setup(cx); let (handle, content) = mount(cx);
    turn(cx, handle, |window, cx| {
        for disabled in [false, true] {
            for checked in [false, true] {
                let r = node(radio_content("node-radio", "Destination", "Different visible label", checked).disabled(disabled).on_change(|_, _, _, _| {}), window, cx);
                assert_eq!(r.role(), gpui::Role::RadioButton); assert_eq!(r.label(), Some("Destination"));
                assert_eq!(r.toggled(), Some(if checked { accesskit::Toggled::True } else { accesskit::Toggled::False }));
                assert_eq!(r.is_selected(), Some(checked)); assert_eq!(r.is_disabled(), disabled);
                assert_eq!(r.supports_action(accesskit::Action::Click), !disabled && !checked);
            }
            let b = node(button_content("node-menu-item", "Refresh", "↻").role(gpui::Role::MenuItem)
                .disabled(disabled).on_click(|_, _, _| {}), window, cx);
            assert_eq!(b.role(), gpui::Role::MenuItem); assert_eq!(b.label(), Some("Refresh"));
            assert_eq!(b.is_disabled(), disabled); assert_eq!(b.supports_action(accesskit::Action::Click), !disabled);
        }
        let focus = content.read(cx).modal_focus.clone();
        let element = focus_scope(div().role(gpui::Role::Menu).aria_label("Choices"), "menu-scope", &focus).into_element();
        assert_eq!(element.a11y_role(), Some(gpui::Role::Menu));
        let mut metadata = accesskit::Node::new(gpui::Role::Menu); element.write_a11y_info(&mut metadata);
        assert_eq!(metadata.label(), Some("Choices")); assert!(metadata.supports_action(accesskit::Action::Focus));
    });
}
