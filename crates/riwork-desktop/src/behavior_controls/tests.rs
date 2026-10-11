//! Headless behavior/selection tests only: no Workspace, terminal, service or provider.
use super::*;
use gpui::InputEvent as _;
use gpui::{
    Context, Entity, FocusHandle, Focusable, Render, TestAppContext, WindowHandle, WindowOptions,
    canvas, point, px,
};
use gpui_kit::base::{
    Root, TestSupportExt, TextSelection, TextSelectionHandle, TextSelectionRegistration,
    TextSelectionScopeId,
};
use gpui_kit::test::TestWindowExt;
use std::{cell::RefCell, rc::Rc};

#[derive(Clone)]
struct Fact {
    node: Option<accesskit::Node>,
    bounds: Bounds<Pixels>,
    text: gpui::TextStyle,
}
type Facts = Rc<RefCell<HashMap<&'static str, Fact>>>;

// Transparent test observation of the actual library element, in its drawing lifecycle.
// No substitute hitbox, focus, role, state or event handler is installed.
struct Observed<E: Element> {
    inner: E,
    key: &'static str,
    facts: Facts,
}
impl<E: Element> IntoElement for Observed<E> {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl<E: Element> Element for Observed<E> {
    type RequestLayoutState = E::RequestLayoutState;
    type PrepaintState = E::PrepaintState;
    fn id(&self) -> Option<ElementId> {
        self.inner.id()
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        self.inner.source_location()
    }
    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        self.inner.request_layout(id, inspector, window, cx)
    }
    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let state = self
            .inner
            .prepaint(id, inspector, bounds, layout, window, cx);
        let node = self.inner.a11y_role().map(|role| {
            let mut node = accesskit::Node::new(role);
            self.inner.write_a11y_info(&mut node);
            node
        });
        self.facts.borrow_mut().insert(
            self.key,
            Fact {
                node,
                bounds,
                text: window.text_style(),
            },
        );
        state
    }
    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut Self::RequestLayoutState,
        state: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.inner
            .paint(id, inspector, bounds, layout, state, window, cx)
    }
    fn a11y_role(&self) -> Option<gpui::Role> {
        self.inner.a11y_role()
    }
    fn write_a11y_info(&self, node: &mut accesskit::Node) {
        self.inner.write_a11y_info(node);
    }
    fn a11y_synthetic_children(
        &mut self,
        state: &mut Self::PrepaintState,
        builder: &mut A11ySubtreeBuilder,
    ) {
        self.inner.a11y_synthetic_children(state, builder);
    }
}
#[derive(IntoElement)]
struct ObservedControl<B: Primitive> {
    control: Control<B>,
    key: &'static str,
    facts: Facts,
}
impl<B: Primitive> RenderOnce for ObservedControl<B> {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        // Executed by GPUI under the mounted view, never from update_window.
        Observed {
            inner: self.control.render(window, cx).into_element(),
            key: self.key,
            facts: self.facts,
        }
    }
}
fn observe_control<B: Primitive>(
    control: Control<B>,
    key: &'static str,
    facts: &Facts,
) -> ObservedControl<B> {
    ObservedControl {
        control,
        key,
        facts: facts.clone(),
    }
}
struct Harness {
    focus: FocusHandle,
    button_focus: FocusHandle,
    terminal_focus: FocusHandle,
    chat_focus: FocusHandle,
    link_focus: Option<FocusHandle>,
    facts: Facts,
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
    terminal_paste_actions: usize,
    raw_tabs: usize,
    transcript_copies: usize,
    cancels: usize,
    modal_return: FocusReturn,
    legacy_scope_capture: bool,
    scope_capture_enabled: bool,
    editor: Entity<crate::text_input::InputState>,
    next_editor: Entity<crate::text_input::InputState>,
    notes: Entity<crate::text_input::TextareaState>,
    // Only the mode-policy fixture mounts this explicitly indentable Base textarea.
    indent_notes: Option<Entity<crate::text_input::TextareaState>>,
    selection: TextSelectionHandle,
    chat_scope: TextSelectionScopeId,
    other_selection: TextSelectionHandle,
    other_scope: TextSelectionScopeId,
    modal_selection: TextSelectionHandle,
    modal_scope: TextSelectionScopeId,
    modal_focus: FocusHandle,
    modal: bool,
}

fn participant(
    id: &'static str,
    selection: TextSelectionHandle,
) -> impl IntoElement + ParentElement {
    div().id(id).test_support().w(px(160.)).h(px(24.)).child(
        canvas(
            move |bounds, window, cx| {
                let hitbox = window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal);
                selection.register(
                    TextSelectionRegistration::new(hitbox, bounds).with_text_bounds(vec![bounds]),
                    window,
                    cx,
                );
            },
            |_, _, _, _| {},
        )
        .size_full(),
    )
}

impl Render for Harness {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        sync_modal_scope(self.modal.then_some(self.modal_scope), window, cx);
        let colors = Palette::RIWORK;
        let toggle_owner = cx.entity().downgrade();
        let switch_owner = cx.entity().downgrade();
        let mut observed_link = link("link", "Read details", colors)
            .disabled(self.disabled)
            .w(px(160.))
            .h(px(24.))
            .on_activate(cx.listener(|owner, _, _, _| owner.link_calls += 1));
        // Link owns a private keyed focus handle and replaces track_focus in
        // its render. Observe the actual handle after its native mouse focus,
        // without installing a substitute focus handle or focus operation.
        observed_link.base = observed_link.base.on_mouse_up(
            gpui::MouseButton::Left,
            cx.listener(|owner, _, window, cx| owner.link_focus = window.focused(cx)),
        );
        // The click-count probe is not focusable. The domain container retains
        // ordinary focus ancestry without turning observation into a keyboard button.
        div()
            .id("behavior-harness")
            .size_full()
            .on_click(cx.listener(|owner, _, _, _| owner.parent_calls += 1))
            .child(
                div()
                    .id("behavior-content")
                    .track_focus(&self.focus)
                    .size_full()
                    .flex()
                    .flex_col()
                    .on_key_down(
                        cx.listener(|owner, event: &gpui::KeyDownEvent, window, cx| {
                            if protect_terminal_edit_menu_fallback(event, window, cx) {
                                owner.terminal_menu_guards += 1;
                            }
                        }),
                    )
                    .on_action(cx.listener(|owner, _: &base::input::Copy, window, cx| {
                        let text = TextSelection::selected_text(window, cx);
                        if text.is_empty() {
                            cx.propagate();
                            return;
                        }
                        owner.transcript_copies += 1;
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                    }))
                    .on_action(cx.listener(|owner, _: &crate::PasteInTerminal, _, cx| {
                        // Pure observation of main's real Terminal binding. Text/no-file
                        // paste propagates to Ghostty's raw handler; no native surface
                        // or terminal_drop/provider work is constructed by this fixture.
                        owner.terminal_paste_actions += 1;
                        cx.propagate();
                    }))
                    .capture_key_down(cx.listener(
                        |owner, event: &gpui::KeyDownEvent, window, cx| {
                            if event.keystroke.key == "escape" && owner.modal {
                                owner.modal = false;
                                owner.cancels += 1;
                                owner.modal_return.restore_within(&owner.focus, window, cx);
                                cx.stop_propagation();
                                cx.notify();
                                return;
                            }
                            let first = owner.editor.read(cx).focus_handle(cx).is_focused(window);
                            let next = owner
                                .next_editor
                                .read(cx)
                                .focus_handle(cx)
                                .is_focused(window);
                            if event.keystroke.key == "tab" && (first || next) {
                                let input = if first {
                                    owner.editor.clone()
                                } else {
                                    owner.next_editor.clone()
                                };
                                let composing = input.update(cx, |state, cx| {
                                    gpui::EntityInputHandler::marked_text_range(state, window, cx)
                                        .is_some()
                                });
                                if !composing {
                                    owner.raw_tabs += 1;
                                    let target = if event.keystroke.modifiers.shift {
                                        &owner.editor
                                    } else {
                                        &owner.next_editor
                                    };
                                    target.read(cx).focus_handle(cx).focus(window, cx);
                                }
                                cx.stop_propagation();
                            }
                        },
                    ))
                    .child(
                        div()
                            .key_context("ChatView")
                            .track_focus(&self.chat_focus.clone().tab_stop(false))
                            .capture_any_mouse_down(cx.listener(
                                |owner, event: &gpui::MouseDownEvent, window, cx| {
                                    if owner.scope_capture_enabled
                                        && event.button == gpui::MouseButton::Left
                                    {
                                        if owner.legacy_scope_capture {
                                            TextSelection::activate_scope(
                                                owner.chat_scope,
                                                window,
                                                cx,
                                            );
                                        } else {
                                            activate_content_scope(owner.chat_scope, window, cx);
                                        }
                                    }
                                },
                            ))
                            .child(selection_scope(
                                participant("transcript", self.selection.clone()),
                                self.chat_scope,
                            )),
                    )
                    .child(
                        div()
                            .capture_any_mouse_down(cx.listener(
                                |owner, event: &gpui::MouseDownEvent, window, cx| {
                                    if owner.scope_capture_enabled
                                        && event.button == gpui::MouseButton::Left
                                    {
                                        activate_content_scope(owner.other_scope, window, cx);
                                    }
                                },
                            ))
                            .child(selection_scope(
                                participant("other-transcript", self.other_selection.clone()),
                                self.other_scope,
                            )),
                    )
                    .children(self.modal.then(|| {
                        modal_scope(
                            div()
                                .flex()
                                .flex_col()
                                .child(participant("modal-text", self.modal_selection.clone()))
                                .child(
                                    button(
                                        "modal-first",
                                        "Back",
                                        controls::Button::Secondary,
                                        colors,
                                    )
                                    .on_click(|_, _, _| {}),
                                )
                                .child(
                                    button(
                                        "modal-last",
                                        "Confirm",
                                        controls::Button::Secondary,
                                        colors,
                                    )
                                    .on_click(|_, _, _| {}),
                                ),
                            self.modal_scope,
                            "modal-scope",
                            &self.modal_focus,
                        )
                    }))
                    .child(
                        button("button", "Apply", controls::Button::Secondary, colors)
                            .track_focus(&self.button_focus)
                            .disabled(self.disabled)
                            .on_click(cx.listener(|owner, _, _, _| owner.button_calls += 1)),
                    )
                    .child(
                        toggle("toggle", "Pin", self.pressed, colors)
                            .disabled(self.disabled)
                            .on_change(move |next, _, _, cx| {
                                let _ = toggle_owner.update(cx, |owner, cx| {
                                    owner.pressed = next;
                                    owner.toggle_calls += 1;
                                    cx.notify();
                                });
                            }),
                    )
                    .child(
                        switch("switch", "Show details", self.checked, colors)
                            .disabled(self.disabled)
                            .on_change(move |next, _, _, cx| {
                                let _ = switch_owner.update(cx, |owner, cx| {
                                    owner.checked = next;
                                    owner.switch_calls += 1;
                                    cx.notify();
                                });
                            }),
                    )
                    .child(observe_control(observed_link, "link", &self.facts))
                    .child(crate::text_input::input("editor", &self.editor, window, cx))
                    .child(crate::text_input::input(
                        "next-editor",
                        &self.next_editor,
                        window,
                        cx,
                    ))
                    // An input nested below a terminal context still owns its Copy action.
                    .child(
                        div()
                            .key_context("Terminal")
                            .child(crate::text_input::textarea(
                                "notes",
                                &self.notes,
                                window,
                                cx,
                            ))
                            .children(self.indent_notes.as_ref().map(|state| {
                                crate::text_input::textarea("indent-notes", state, window, cx)
                            })),
                    )
                    // This proxy proves key routing without constructing Ghostty or a process.
                    .child(
                        div()
                            .id("terminal-proxy")
                            .key_context("Terminal")
                            .track_focus(&self.terminal_focus)
                            .h(px(24.))
                            .on_key_down(cx.listener(|owner, event: &gpui::KeyDownEvent, _, _| {
                                owner.terminal_keys.push(event.keystroke.key.clone());
                            })),
                    ),
            )
    }
}

fn setup(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.set_global(crate::settings::Settings::default());
        cx.set_global(crate::theme::Appearance {
            selected: crate::theme::ThemeChoice::RiWork,
            palette: Palette::RIWORK,
            terminal: None,
            ghostty: None,
            error: None,
        });
        crate::text_input::init(cx);
        init(cx);
        // Same action/key/context and initialization order as main. This is a
        // binding-only fixture: never call terminal_paste or construct Ghostty.
        cx.bind_keys([gpui::KeyBinding::new(
            "cmd-v",
            crate::PasteInTerminal,
            Some("Terminal"),
        )]);
    });
}
fn mount(cx: &mut TestAppContext) -> (WindowHandle<Root>, Entity<Harness>) {
    let (handle, content) = cx.update(|cx| {
        let mut content = None;
        let handle = cx
            .open_window(WindowOptions::default(), |window, cx| {
                let view = cx.new(|cx| {
                    let chat_focus = cx.focus_handle();
                    let focus_selection = chat_focus.clone();
                    let selection = TextSelectionHandle::new("transcript", cx);
                    selection.focus_with(move |window, cx| focus_selection.focus(window, cx), cx);
                    Harness {
                        focus: cx.focus_handle(),
                        button_focus: cx.focus_handle(),
                        terminal_focus: cx.focus_handle(),
                        chat_focus,
                        link_focus: None,
                        facts: Default::default(),
                        button_calls: 0,
                        parent_calls: 0,
                        toggle_calls: 0,
                        switch_calls: 0,
                        link_calls: 0,
                        pressed: false,
                        checked: false,
                        disabled: false,
                        terminal_keys: Vec::new(),
                        terminal_menu_guards: 0,
                        terminal_paste_actions: 0,
                        raw_tabs: 0,
                        transcript_copies: 0,
                        cancels: 0,
                        modal_return: Default::default(),
                        legacy_scope_capture: false,
                        scope_capture_enabled: true,
                        editor: crate::text_input::single_line("", "Editor", window, cx),
                        next_editor: crate::text_input::single_line("", "Next editor", window, cx),
                        notes: crate::text_input::multiline(
                            "",
                            "Notes",
                            1,
                            3,
                            crate::text_input::EnterBehavior::Newline,
                            window,
                            cx,
                        ),
                        indent_notes: None,
                        selection,
                        chat_scope: TextSelectionScopeId::new(),
                        other_selection: TextSelectionHandle::new("other transcript", cx),
                        other_scope: TextSelectionScopeId::new(),
                        modal_selection: TextSelectionHandle::new("modal", cx),
                        modal_scope: TextSelectionScopeId::new(),
                        modal_focus: cx.focus_handle(),
                        modal: false,
                    }
                });
                content = Some(view.clone());
                cx.new(|cx| Root::new(view, window, cx))
            })
            .unwrap();
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
fn turn(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    f: impl FnOnce(&mut Window, &mut App),
) {
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        f(window, cx);
    })
    .unwrap();
    cx.run_until_parked();
}
fn assert_editor_focus(content: &Entity<Harness>, window: &Window, cx: &App, phase: &str) {
    let editor = content.read(cx).editor.clone();
    assert!(
        editor.read(cx).focus_handle(cx).is_focused(window),
        "editor focus lost at {phase}; contexts={:?}",
        window.context_stack()
    );
}

#[gpui::test]
fn base_button_pointer_keyboard_activate_once_and_disabled_does_not_bubble(
    cx: &mut TestAppContext,
) {
    setup(cx);
    let (handle, content) = mount(cx);
    for key in [None, Some("enter"), Some("space")] {
        turn(cx, handle, |window, cx| {
            if let Some(key) = key {
                window.press(key, cx);
            } else {
                window.click("button", cx);
            }
        });
    }
    assert_eq!(content.read_with(cx, |owner, _| owner.button_calls), 3);
    content.update(cx, |owner, cx| {
        owner.disabled = true;
        owner.parent_calls = 0;
        cx.notify();
    });
    turn(cx, handle, |window, cx| {
        window.click("button", cx);
        window.press("enter", cx);
        window.press("space", cx);
    });
    assert_eq!(
        content.read_with(cx, |owner, _| (owner.button_calls, owner.parent_calls)),
        (3, 0)
    );
}

fn drag_transcript(cx: &mut TestAppContext, handle: WindowHandle<Root>, id: &'static str) {
    turn(cx, handle, |window, cx| {
        let bounds = window.find(id).bounds();
        window.drag(
            bounds.origin + point(px(2.), px(10.)),
            bounds.origin + point(px(80.), px(10.)),
            cx,
        );
    });
}
fn menu_action(label: &str) -> Box<dyn gpui::Action> {
    edit_menu()
        .items
        .into_iter()
        .find_map(|item| match item {
            gpui::MenuItem::Action {
                name,
                action,
                os_action: Some(_),
                ..
            } if &*name == label => Some(action),
            _ => None,
        })
        .expect("native Edit action")
}

#[gpui::test]
fn parent_rerender_preserves_nondefault_transcript_scope_after_pointer_selection(
    cx: &mut TestAppContext,
) {
    setup(cx);
    // The legacy direct-activation path is Base behavior; one owner path proves the rerender.
    for legacy in [false] {
        let (handle, content) = mount(cx);
        content.update(cx, |owner, cx| {
            owner.legacy_scope_capture = legacy;
            cx.notify();
        });
        drag_transcript(cx, handle, "transcript");
        // Force the actual parent render path after the pointer-up/effect turn.
        content.update(cx, |_, cx| cx.notify());
        turn(cx, handle, |window, cx| {
            assert_eq!(TextSelection::selected_text(window, cx), "transcript")
        });
        turn(cx, handle, |window, cx| window.press("cmd-c", cx));
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            "transcript"
        );
        turn(cx, handle, |window, cx| {
            window.dispatch_action(menu_action("Copy"), cx)
        });
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            "transcript"
        );
        assert_eq!(content.read_with(cx, |owner, _| owner.transcript_copies), 2);
    }
}

#[gpui::test]
fn modal_transition_restores_content_scope_and_retired_scope_stays_inactive(
    cx: &mut TestAppContext,
) {
    setup(cx);
    let (handle, content) = mount(cx);
    drag_transcript(cx, handle, "transcript");
    content.update(cx, |owner, cx| {
        owner.modal = true;
        cx.notify();
    });
    drag_transcript(cx, handle, "modal-text");
    turn(cx, handle, |window, cx| {
        // A background scope request cannot displace the active modal.
        activate_content_scope(content.read(cx).other_scope, window, cx);
        assert_eq!(TextSelection::selected_text(window, cx), "modal");
    });
    content.update(cx, |owner, cx| {
        owner.modal = false;
        owner.scope_capture_enabled = false;
        cx.notify();
    });
    // No child capture may activate scope now: this proves modal-close restoration.
    drag_transcript(cx, handle, "transcript");
    turn(cx, handle, |window, cx| {
        assert_eq!(TextSelection::selected_text(window, cx), "transcript")
    });
    content.update(cx, |owner, cx| {
        owner.modal = true;
        cx.notify();
    });
    turn(cx, handle, |window, cx| {
        retire_content_scope(content.read(cx).chat_scope, window, cx)
    });
    content.update(cx, |owner, cx| {
        owner.modal = false;
        cx.notify();
    });
    drag_transcript(cx, handle, "transcript");
    turn(cx, handle, |window, cx| {
        assert!(!TextSelection::has_selection(window, cx))
    });
}

#[gpui::test]
fn single_line_owner_tab_and_shift_tab_preserve_ime_and_navigate_once(cx: &mut TestAppContext) {
    setup(cx);
    let (handle, content) = mount(cx);
    turn(cx, handle, |window, cx| {
        window.click("editor", cx);
        assert_editor_focus(&content, window, cx, "single-line after click");
        let editor = content.read(cx).editor.clone();
        let mut handler = gpui::ElementInputHandler::new(window.find("editor").bounds(), editor);
        gpui::InputHandler::replace_and_mark_text_in_range(
            &mut handler,
            None,
            "日本",
            Some(2..2),
            window,
            cx,
        );
        assert_editor_focus(&content, window, cx, "single-line after mark");
        window.render_frame(cx);
        assert_editor_focus(&content, window, cx, "single-line after marked frame");
    });
    for key in ["tab", "shift-tab"] {
        turn(cx, handle, |window, cx| {
            assert_editor_focus(&content, window, cx, "single-line before marked navigation");
            window.press(key, cx);
            assert_editor_focus(&content, window, cx, "single-line after marked navigation");
        });
        turn(cx, handle, |window, cx| {
            assert_editor_focus(
                &content,
                window,
                cx,
                "single-line marked navigation after effect flush",
            );
            content.read(cx).editor.clone().update(cx, |state, cx| {
                assert_eq!(state.value(), "日本");
                assert_eq!(
                    gpui::EntityInputHandler::marked_text_range(state, window, cx),
                    Some(0..2)
                );
            });
        });
    }
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
    turn(cx, handle, |window, cx| {
        assert!(
            content
                .read(cx)
                .next_editor
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
        )
    });
    turn(cx, handle, |window, cx| window.press("shift-tab", cx));
    assert_eq!(content.read_with(cx, |owner, _| owner.raw_tabs), 2);
    turn(cx, handle, |window, cx| {
        assert!(
            content
                .read(cx)
                .editor
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
        )
    });
}

#[gpui::test]
fn native_edit_menu_uses_base_actions_and_preserves_editor_priority(cx: &mut TestAppContext) {
    setup(cx);
    let (handle, content) = mount(cx);
    assert!(menu_action("Copy").partial_eq(&base::input::Copy));
    assert!(menu_action("Cut").partial_eq(&base::input::Cut));
    assert!(menu_action("Paste").partial_eq(&base::input::Paste));
    assert!(menu_action("Select All").partial_eq(&base::input::SelectAll));
    content.update(cx, |owner, cx| {
        owner.selection.set_fallback_copy_text("  transcript  ", cx)
    });
    drag_transcript(cx, handle, "transcript");
    turn(cx, handle, |window, cx| {
        window.dispatch_action(menu_action("Copy"), cx)
    });
    assert_eq!(
        cx.read_from_clipboard().unwrap().text().unwrap(),
        "  transcript  "
    );
    turn(cx, handle, |window, cx| window.press("cmd-c", cx));
    assert_eq!(
        cx.read_from_clipboard().unwrap().text().unwrap(),
        "  transcript  "
    );
    turn(cx, handle, |window, cx| {
        let editor = content.read(cx).editor.clone();
        editor.update(cx, |state, cx| state.set_value("editor bytes", window, cx));
        editor.read(cx).focus_handle(cx).focus(window, cx);
    });
    turn(cx, handle, |window, cx| {
        window.dispatch_action(menu_action("Select All"), cx)
    });
    turn(cx, handle, |window, cx| {
        window.dispatch_action(menu_action("Copy"), cx)
    });
    assert_eq!(
        cx.read_from_clipboard().unwrap().text().unwrap(),
        "editor bytes"
    );
    assert_eq!(content.read_with(cx, |owner, _| owner.transcript_copies), 2);
    turn(cx, handle, |window, cx| {
        window.dispatch_action(menu_action("Cut"), cx)
    });
    assert_eq!(
        content.read_with(cx, |owner, cx| owner.editor.read(cx).value()),
        ""
    );
    turn(cx, handle, |window, cx| {
        window.dispatch_action(menu_action("Paste"), cx)
    });
    assert_eq!(
        content.read_with(cx, |owner, cx| owner.editor.read(cx).value()),
        "editor bytes"
    );
    assert_eq!(
        content.read_with(cx, |owner, _| owner.terminal_menu_guards),
        0
    );
    // An Input nested under Terminal outranks both Root Copy and main's native
    // paste binding. Its actual Base handler remains the only editor owner.
    turn(cx, handle, |window, cx| {
        let notes = content.read(cx).notes.clone();
        notes.update(cx, |state, cx| {
            state.set_value("nested editor bytes", window, cx)
        });
        notes.read(cx).focus_handle(cx).focus(window, cx);
    });
    turn(cx, handle, |window, cx| window.press("cmd-a", cx));
    turn(cx, handle, |window, cx| window.press("cmd-c", cx));
    assert_eq!(
        cx.read_from_clipboard().unwrap().text().unwrap(),
        "nested editor bytes"
    );
    turn(cx, handle, |window, cx| window.press("cmd-v", cx));
    assert_eq!(
        content.read_with(cx, |owner, cx| owner.notes.read(cx).value()),
        "nested editor bytes"
    );
    assert_eq!(
        content.read_with(cx, |owner, _| (
            owner.terminal_menu_guards,
            owner.terminal_paste_actions,
            owner.transcript_copies
        )),
        (0, 0, 2)
    );
    // The Terminal binding propagates text paste; Root Copy/Tab are scoped-unbound.
    turn(cx, handle, |window, cx| {
        let focus = content.read(cx).terminal_focus.clone();
        focus.focus(window, cx);
        window.render_frame(cx);
        for key in ["cmd-x", "cmd-c", "cmd-v", "cmd-a", "tab", "shift-tab"] {
            window.press(key, cx);
        }
    });
    assert_eq!(
        content.read_with(cx, |owner, _| owner.terminal_keys.clone()),
        ["x", "c", "v", "a", "tab", "tab"]
    );
    assert_eq!(
        content.read_with(cx, |owner, _| owner.terminal_menu_guards),
        4
    );
    assert_eq!(
        content.read_with(cx, |owner, _| owner.terminal_paste_actions),
        1
    );
    turn(cx, handle, |window, cx| window.press("cmd-shift-c", cx));
    assert_eq!(
        content.read_with(cx, |owner, _| owner.terminal_menu_guards),
        4
    );
}

struct ContentControls {
    calls: Vec<&'static str>,
    choice: usize,
    pressed: bool,
    checked: bool,
    disabled: bool,
    drag_calls: usize,
}
#[derive(Clone)]
struct ContentRowDrag;
impl Render for ContentRowDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().child("drag")
    }
}
impl Render for ContentControls {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let toggle_owner = cx.entity().downgrade();
        let switch_owner = cx.entity().downgrade();
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                button_content("content-row", "Choose item", "Visible row")
                    .group("content-tab")
                    .w(px(180.))
                    .h(px(30.))
                    .hover(|style| style.bg(rgb(0x334455)))
                    .focus_visible(|style| style.border_1().border_color(rgb(0xffffff)))
                    .on_click(cx.listener(|owner, _, _, _| owner.calls.push("row")))
                    .on_drag(ContentRowDrag, {
                        let owner = cx.entity().downgrade();
                        move |drag, _, _, cx| {
                            let _ = owner.update(cx, |owner, _| owner.drag_calls += 1);
                            cx.new(|_| drag.clone())
                        }
                    })
                    .child(tab_close_boundary(
                        "content-close-boundary",
                        action("content-close", "Close item", Palette::RIWORK)
                            .child("×")
                            .size(px(16.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .text_color(rgb(Palette::RIWORK.muted))
                            .disabled(self.disabled)
                            .hover(|style| {
                                style
                                    .bg(rgb(Palette::RIWORK.divider))
                                    .text_color(rgb(Palette::RIWORK.text))
                            })
                            .map(|close| tab_close_reveal(close, "content-tab"))
                            // Same owner isolation as real nested tab/project controls.
                            .on_click(cx.listener(|owner, _, _, cx| {
                                cx.stop_propagation();
                                owner.calls.push("close");
                            })),
                    ))
                    .child(
                        radio_content(
                            "nested-disabled-radio",
                            "Unavailable destination",
                            "Unavailable",
                            false,
                        )
                        .disabled(true)
                        .w(px(60.))
                        .h(px(24.))
                        .on_change(|_, _, _, _| panic!("disabled radio activated")),
                    ),
            )
            .child(
                toggle_content("content-toggle", "Pin item", "Owner pin mark", self.pressed)
                    .w(px(180.))
                    .h(px(30.))
                    .disabled(self.disabled)
                    .hover(|style| style.bg(rgb(0x334455)))
                    .on_change(move |next, _, _, cx| {
                        let _ = toggle_owner.update(cx, |owner, cx| {
                            owner.pressed = next;
                            owner.calls.push("toggle");
                            cx.notify();
                        });
                    }),
            )
            .child(
                switch_content(
                    "content-switch",
                    "Show item",
                    "Owner switch mark",
                    self.checked,
                )
                .w(px(180.))
                .h(px(30.))
                .disabled(self.disabled)
                .hover(|style| style.bg(rgb(0x334455)))
                .on_change(move |next, _, _, cx| {
                    let _ = switch_owner.update(cx, |owner, cx| {
                        owner.checked = next;
                        owner.calls.push("switch");
                        cx.notify();
                    });
                }),
            )
            .child(
                base::RadioGroup::new("content-choices")
                    .aria_label("Destination")
                    .children((0..2).map(|choice| {
                        let owner = cx.entity().downgrade();
                        radio_content(
                            format!("choice-{choice}"),
                            format!("Destination {choice}"),
                            format!("Visible {choice}"),
                            self.choice == choice,
                        )
                        .w(px(180.))
                        .h(px(30.))
                        .disabled(self.disabled)
                        .set_position(choice + 1, 2)
                        .hover(|style| style.bg(rgb(0x334455)))
                        .on_change(move |next, _, _, cx| {
                            if next {
                                let _ = owner.update(cx, |owner, cx| {
                                    owner.choice = choice;
                                    owner.calls.push("radio");
                                    cx.notify();
                                });
                            }
                        })
                    })),
            )
    }
}
#[gpui::test]
fn content_controls_keep_caller_hover_nested_isolation_and_radio_exclusivity(
    cx: &mut TestAppContext,
) {
    setup(cx);
    let (handle, content) = cx.update(|cx| {
        let mut content = None;
        let handle = cx
            .open_window(WindowOptions::default(), |window, cx| {
                let view = cx.new(|_| ContentControls {
                    calls: Vec::new(),
                    choice: 0,
                    pressed: false,
                    checked: false,
                    disabled: false,
                    drag_calls: 0,
                });
                content = Some(view.clone());
                cx.new(|cx| Root::new(view, window, cx))
            })
            .unwrap();
        (handle, content.unwrap())
    });
    turn(cx, handle, |window, _| window.activate_window());
    // Establish real Base row focus without assuming a root-level key context
    // exists before any control is focused. Observe the setup activation too.
    turn(cx, handle, |window, cx| window.click("content-row", cx));
    assert_eq!(
        content.read_with(cx, |owner, _| owner.calls.clone()),
        ["row"]
    );
    content.update(cx, |owner, _| owner.calls.clear());
    turn(cx, handle, |window, cx| {
        window.dispatch_event(
            gpui::MouseMoveEvent {
                position: point(px(300.), px(200.)),
                pressed_button: None,
                modifiers: Default::default(),
            }
            .to_platform_input(),
            cx,
        );
        window.render_frame(cx);
        assert!(
            !window.find("content-close").visible(),
            "inactive close stays visually quiet away from its tab"
        );
        window.press("tab", cx);
    });
    turn(cx, handle, |window, _| {
        assert_eq!(window.find("content-close").focused(), Some(true));
        assert!(
            window.find("content-close").visible(),
            "Tab reaches and reveals the actual close control without pointer hover"
        );
    });
    turn(cx, handle, |window, cx| window.press("shift-tab", cx));
    turn(cx, handle, |window, _| {
        assert!(!window.find("content-close").visible())
    });
    turn(cx, handle, |window, cx| window.hover("content-row", cx));
    turn(cx, handle, |window, _| {
        assert!(
            window.find("content-close").visible(),
            "existing tab hover still reveals close"
        )
    });
    turn(cx, handle, |window, cx| window.click("content-close", cx));
    turn(cx, handle, |window, _| {
        assert_eq!(
            window.find("content-close").focused(),
            Some(true),
            "the boundary runs after Base pointer focus"
        );
        assert!(window.find("content-close").visible());
    });
    turn(cx, handle, |window, cx| window.press("enter", cx));
    assert_eq!(
        content.read_with(cx, |owner, _| owner.calls.clone()),
        ["close", "close"]
    );
    turn(cx, handle, |window, cx| {
        let bounds = window.find("content-close").bounds();
        window.drag(
            bounds.center(),
            bounds.center() + point(px(100.), px(80.)),
            cx,
        );
    });
    assert_eq!(
        content.read_with(cx, |owner, _| owner.drag_calls),
        0,
        "close presses never arm a tab drag"
    );
    assert_eq!(
        content.read_with(cx, |owner, _| owner.calls.clone()),
        ["close", "close"]
    );
    turn(cx, handle, |window, cx| {
        let bounds = window.find("content-row").bounds();
        window.drag(
            bounds.origin + point(px(3.), px(3.)),
            bounds.origin + point(px(103.), px(83.)),
            cx,
        );
    });
    assert_eq!(
        content.read_with(cx, |owner, _| owner.drag_calls),
        1,
        "ordinary tab-row dragging is retained"
    );
    turn(cx, handle, |window, cx| {
        window.click("nested-disabled-radio", cx)
    });
    assert_eq!(
        content.read_with(cx, |owner, _| owner.calls.clone()),
        ["close", "close"]
    );
    for id in ["content-toggle", "content-switch"] {
        turn(cx, handle, |window, cx| window.click(id, cx));
        turn(cx, handle, |window, cx| window.press("space", cx));
    }
    assert_eq!(
        content.read_with(cx, |owner, _| (owner.pressed, owner.checked)),
        (false, false)
    );
    turn(cx, handle, |window, cx| window.click("choice-1", cx));
    turn(cx, handle, |window, cx| window.press("enter", cx));
    turn(cx, handle, |window, cx| window.press("space", cx));
    assert_eq!(content.read_with(cx, |owner, _| owner.choice), 1);
    assert_eq!(
        content.read_with(cx, |owner, _| owner
            .calls
            .iter()
            .filter(|call| **call == "radio")
            .count()),
        1
    );
    let before = content.read_with(cx, |owner, _| owner.calls.clone());
    content.update(cx, |owner, cx| {
        owner.disabled = true;
        cx.notify();
    });
    for id in [
        "content-close",
        "content-toggle",
        "content-switch",
        "choice-0",
    ] {
        turn(cx, handle, |window, cx| window.click(id, cx));
    }
    assert_eq!(
        content.read_with(cx, |owner, _| owner.calls.clone()),
        before
    );
}
