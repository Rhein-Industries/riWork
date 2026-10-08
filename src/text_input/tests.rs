//! Headless macOS-linked smoke: real Base states, layout, and event dispatch.
use super::*;
use gpui::{
    Bounds, ClipboardEntry, Context, ElementInputHandler, ExternalPaths, FocusHandle,
    InputEvent as _, InputHandler, IntoElement, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    Render, Subscription, TestAppContext, WindowBounds, WindowHandle, WindowOptions, div, point,
    px, size,
};
use gpui_kit::test::TestWindowExt;
use std::{cell::RefCell, rc::Rc};

struct Fields {
    first: Entity<InputState>,
    composer: Entity<TextareaState>,
    notes: Entity<TextareaState>,
    changes: usize,
    submissions: usize,
    first_submissions: usize,
    propagated_enters: usize,
    raw_enters: usize,
    escape_actions: usize,
    raw_escapes: usize,
    cancels: usize,
    raw_tabs: usize,
    navigation: Vec<bool>,
    escape_owner_is_action: bool,
    parent_focus: FocusHandle,
    raw_base: bool,
    composer_enter: EnterBehavior,
    focus_events: usize,
    attachments: Rc<RefCell<Vec<std::path::PathBuf>>>,
    subscriptions: Vec<Subscription>,
}

impl Fields {
    fn focused_composing(&self, window: &mut Window, cx: &mut App) -> bool {
        if self.first.read(cx).focus_handle(cx).is_focused(window) {
            return self.first.update(cx, |state, cx| {
                state.marked_text_range(window, cx).is_some()
            });
        }
        if self.composer.read(cx).focus_handle(cx).is_focused(window) {
            return self.composer.update(cx, |state, cx| {
                state.marked_text_range(window, cx).is_some()
            });
        }
        if self.notes.read(cx).focus_handle(cx).is_focused(window) {
            return self.notes.update(cx, |state, cx| {
                state.marked_text_range(window, cx).is_some()
            });
        }
        false
    }
}

impl Render for Fields {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let attachments = self.attachments.clone();
        let composer = if self.raw_base {
            InputBase::new("composer").child(Textarea::new(&self.composer))
        } else {
            textarea("composer", &self.composer, window, cx)
        };
        div()
            .track_focus(&self.parent_focus)
            .size_full()
            .flex()
            .flex_col()
            .gap(px(8.))
            .on_action(cx.listener(|this, _: &Enter, _, _| this.propagated_enters += 1))
            .on_action(cx.listener(|this, _: &Escape, _, cx| {
                this.escape_actions += 1;
                if this.escape_owner_is_action {
                    this.cancels += 1;
                } else {
                    // An owner using a raw Escape policy leaves the action
                    // available to GPUI's later raw-key fallback.
                    cx.propagate();
                }
            }))
            .capture_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "enter" => this.raw_enters += 1,
                    "escape" => {
                        this.raw_escapes += 1;
                        if !this.focused_composing(window, cx) {
                            this.cancels += 1;
                            cx.stop_propagation();
                        }
                    }
                    "tab" => {
                        this.raw_tabs += 1;
                        if !this.focused_composing(window, cx) {
                            let shift = event.keystroke.modifiers.shift;
                            this.navigation.push(shift);
                            let focus = if shift {
                                this.parent_focus.clone()
                            } else {
                                this.composer.read(cx).focus_handle(cx)
                            };
                            window.focus(&focus, cx);
                            cx.stop_propagation();
                        }
                    }
                    _ => {}
                }
            }))
            .child(input("first", &self.first, window, cx).w(px(220.)))
            .child(
                on_paste(composer, &self.composer, move |item, _, _| {
                    let files: Vec<_> = item
                        .entries
                        .iter()
                        .filter_map(|entry| match entry {
                            ClipboardEntry::ExternalPaths(paths) => Some(paths.0.clone()),
                            _ => None,
                        })
                        .flatten()
                        .collect();
                    if files.is_empty() {
                        return false;
                    }
                    attachments.borrow_mut().extend(files);
                    true
                })
                .w(px(220.)),
            )
            .child(textarea("notes", &self.notes, window, cx).w(px(220.)))
    }
}

fn mount(cx: &mut TestAppContext) -> (WindowHandle<Fields>, Entity<Fields>) {
    let (window, fields) = cx.update(|cx| {
        cx.set_global(Settings::default());
        cx.set_global(theme::Appearance {
            selected: theme::ThemeChoice::RiWork,
            palette: theme::Palette::RIWORK,
            terminal: None,
            ghostty: None,
            error: None,
        });
        init(cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.), px(0.)),
                        size(px(480.), px(400.)),
                    ))),
                    ..Default::default()
                },
                |window, cx| {
                    cx.new(|cx| {
                        let first = single_line("", "Project name", window, cx);
                        let composer =
                            multiline("", "Message", 1, 4, EnterBehavior::Submit, window, cx);
                        let notes =
                            multiline("", "Notes", 2, 4, EnterBehavior::Newline, window, cx);
                        let subscriptions = vec![
                            cx.subscribe(&first, |this: &mut Fields, _, event, _| match event {
                                InputEvent::Change => this.changes += 1,
                                InputEvent::Focus | InputEvent::Blur => this.focus_events += 1,
                                InputEvent::PressEnter { .. } => {
                                    if is_submit(event, EnterBehavior::Submit) {
                                        this.first_submissions += 1;
                                    }
                                }
                            }),
                            cx.subscribe(&composer, |this: &mut Fields, _, event, _| {
                                if is_submit(event, this.composer_enter) {
                                    this.submissions += 1;
                                }
                            }),
                        ];
                        Fields {
                            first,
                            composer,
                            notes,
                            changes: 0,
                            submissions: 0,
                            first_submissions: 0,
                            propagated_enters: 0,
                            raw_enters: 0,
                            escape_actions: 0,
                            raw_escapes: 0,
                            cancels: 0,
                            raw_tabs: 0,
                            navigation: Vec::new(),
                            escape_owner_is_action: true,
                            parent_focus: cx.focus_handle(),
                            raw_base: false,
                            composer_enter: EnterBehavior::Submit,
                            focus_events: 0,
                            attachments: Default::default(),
                            subscriptions,
                        }
                    })
                },
            )
            .unwrap();
        let fields = window.update(cx, |_, _, cx| cx.entity()).unwrap();
        (window, fields)
    });
    cx.update_window(window.into(), |_, window, _| window.activate_window())
        .unwrap();
    cx.run_until_parked();
    (window, fields)
}

/// Finish the outer App update and deliver callback effects before assertions
/// or the next gesture. Rendering inside that update does not flush effects.
fn action_turn(
    cx: &mut TestAppContext,
    handle: WindowHandle<Fields>,
    action: impl FnOnce(&mut Window, &mut App),
) {
    cx.update_window(handle.into(), |_, window, cx| action(window, cx))
        .unwrap();
    cx.run_until_parked();
}

fn mark<M: InputModeKind>(
    id: &'static str,
    state: Entity<InputBaseState<M>>,
    window: &mut Window,
    cx: &mut App,
) {
    state.update(cx, |state, cx| {
        state.set_value("d", window, cx);
        state.set_selected_range(1..1, cx);
    });
    let mut handler = ElementInputHandler::new(window.find(id).bounds(), state);
    handler.replace_and_mark_text_in_range(None, "日本", Some(2..2), window, cx);
    window.render_frame(cx);
}

fn mark_field(id: &'static str, fields: &Entity<Fields>, window: &mut Window, cx: &mut App) {
    window.click(id, cx);
    match id {
        "first" => mark(id, fields.read(cx).first.clone(), window, cx),
        "composer" => mark(id, fields.read(cx).composer.clone(), window, cx),
        _ => unreachable!(),
    }
}

fn assert_marked<M: InputModeKind>(
    state: &Entity<InputBaseState<M>>,
    marked: bool,
    window: &mut Window,
    cx: &mut App,
) {
    assert_eq!(state.read(cx).value(), "d日本");
    assert_eq!(
        state.update(cx, |state, cx| state.marked_text_range(window, cx)),
        marked.then_some(1..3),
    );
}

#[gpui::test]
fn composing_escape_cancels_only_composition_in_forms_and_chat(cx: &mut TestAppContext) {
    let (handle, fields) = mount(cx);
    for id in ["first", "composer"] {
        for direct in [false, true] {
            action_turn(cx, handle, |window, cx| mark_field(id, &fields, window, cx));
            let changes = fields.read_with(cx, |fields, _| fields.changes);
            action_turn(cx, handle, |window, cx| {
                if direct {
                    window.dispatch_action(Box::new(Escape), cx);
                } else {
                    window.press("escape", cx);
                }
            });
            cx.update_window(handle.into(), |_, window, cx| {
                if id == "first" {
                    let state = fields.read(cx).first.clone();
                    assert_marked(&state, false, window, cx);
                } else {
                    let state = fields.read(cx).composer.clone();
                    assert_marked(&state, false, window, cx);
                }
                let owner = fields.read(cx);
                assert_eq!(
                    (owner.escape_actions, owner.raw_escapes, owner.cancels),
                    (0, 0, 0)
                );
                assert_eq!((owner.submissions, owner.first_submissions), (0, 0));
                assert_eq!(owner.changes, changes);
            })
            .unwrap();
            // Unmark closes Base's existing composition undo transaction.
            action_turn(cx, handle, |window, cx| window.press("cmd-z", cx));
            cx.update_window(handle.into(), |_, _, cx| {
                let owner = fields.read(cx);
                let value = if id == "first" {
                    owner.first.read(cx).value()
                } else {
                    owner.composer.read(cx).value()
                };
                assert_eq!(value, "d");
            })
            .unwrap();
        }
    }
}

#[gpui::test]
fn plain_escape_reaches_one_parent_cancel_through_action_or_raw_policy(cx: &mut TestAppContext) {
    let (handle, fields) = mount(cx);
    for action_owner in [true, false] {
        action_turn(cx, handle, |window, cx| {
            fields.update(cx, |fields, cx| {
                fields.escape_owner_is_action = action_owner;
                cx.notify();
            });
            window.click("first", cx);
            let first = fields.read(cx).first.clone();
            first.update(cx, |state, cx| state.set_value("draft", window, cx));
            window.render_frame(cx);
        });
        let before = fields.read_with(cx, |owner, _| {
            (owner.escape_actions, owner.raw_escapes, owner.cancels)
        });
        action_turn(cx, handle, |window, cx| window.press("escape", cx));
        cx.update_window(handle.into(), |_, _, cx| {
            let owner = fields.read(cx);
            assert_eq!(owner.escape_actions, before.0 + 1);
            assert_eq!(owner.raw_escapes, before.1 + usize::from(!action_owner));
            assert_eq!(owner.cancels, before.2 + 1);
            assert_eq!(owner.first.read(cx).value(), "draft");
        })
        .unwrap();
    }
}

#[gpui::test]
fn composing_escape_defers_base_clean_on_escape_until_plain_escape(cx: &mut TestAppContext) {
    let (handle, fields) = mount(cx);
    action_turn(cx, handle, |window, cx| {
        mark_field("first", &fields, window, cx);
        let first = fields.read(cx).first.clone();
        first.update(cx, |state, _| state.set_clean_on_escape(true));
    });
    action_turn(cx, handle, |window, cx| window.press("escape", cx));
    cx.update_window(handle.into(), |_, window, cx| {
        let state = fields.read(cx).first.clone();
        assert_marked(&state, false, window, cx);
        assert_eq!(fields.read(cx).cancels, 0);
    })
    .unwrap();
    let changes = fields.read_with(cx, |owner, _| owner.changes);
    action_turn(cx, handle, |window, cx| window.press("escape", cx));
    cx.update_window(handle.into(), |_, _, cx| {
        let owner = fields.read(cx);
        assert_eq!(owner.first.read(cx).value(), "");
        assert_eq!(owner.changes, changes + 1);
        assert_eq!(
            (owner.escape_actions, owner.raw_escapes, owner.cancels),
            (0, 0, 0)
        );
    })
    .unwrap();
}

#[gpui::test]
fn raw_escape_fallback_retains_owner_composition_guard_and_key_configuration(
    cx: &mut TestAppContext,
) {
    let (handle, fields) = mount(cx);
    cx.update(|cx| cx.bind_keys([KeyBinding::new("escape", gpui::NoAction, Some("Input"))]));
    action_turn(cx, handle, |window, cx| {
        mark_field("first", &fields, window, cx)
    });
    action_turn(cx, handle, |window, cx| window.press("escape", cx));
    cx.update_window(handle.into(), |_, window, cx| {
        let state = fields.read(cx).first.clone();
        assert_marked(&state, true, window, cx);
        let owner = fields.read(cx);
        assert_eq!(
            (owner.escape_actions, owner.raw_escapes, owner.cancels),
            (0, 1, 0)
        );
    })
    .unwrap();
    action_turn(cx, handle, |window, cx| {
        let first = fields.read(cx).first.clone();
        first.update(cx, |state, cx| state.unmark_text(window, cx));
    });
    action_turn(cx, handle, |window, cx| window.press("escape", cx));
    cx.update_window(handle.into(), |_, _, cx| {
        let owner = fields.read(cx);
        assert_eq!(
            (owner.escape_actions, owner.raw_escapes, owner.cancels),
            (0, 2, 1)
        );
        assert_eq!(owner.first.read(cx).value(), "d日本");
    })
    .unwrap();
}

#[gpui::test]
fn marked_tab_and_shift_tab_keep_child_focus_until_owner_navigation_is_safe(
    cx: &mut TestAppContext,
) {
    let (handle, fields) = mount(cx);
    action_turn(cx, handle, |window, cx| {
        mark_field("first", &fields, window, cx)
    });
    for key in ["tab", "shift-tab"] {
        let before = fields.read_with(cx, |owner, _| owner.raw_tabs);
        action_turn(cx, handle, |window, cx| window.press(key, cx));
        cx.update_window(handle.into(), |_, window, cx| {
            let state = fields.read(cx).first.clone();
            assert_marked(&state, true, window, cx);
            let owner = fields.read(cx);
            assert_eq!(owner.raw_tabs, before + 1);
            assert!(owner.navigation.is_empty());
            assert!(owner.first.read(cx).focus_handle(cx).is_focused(window));
        })
        .unwrap();
    }
    action_turn(cx, handle, |window, cx| {
        let first = fields.read(cx).first.clone();
        first.update(cx, |state, cx| state.unmark_text(window, cx));
    });
    for (key, shift) in [("tab", false), ("shift-tab", true)] {
        action_turn(cx, handle, |window, cx| {
            let focus = fields.read(cx).first.read(cx).focus_handle(cx);
            window.focus(&focus, cx);
            window.render_frame(cx);
        });
        let before = fields.read_with(cx, |owner, _| owner.navigation.len());
        action_turn(cx, handle, |window, cx| window.press(key, cx));
        cx.update_window(handle.into(), |_, window, cx| {
            let state = fields.read(cx).first.clone();
            assert_marked(&state, false, window, cx);
            let owner = fields.read(cx);
            assert_eq!(owner.navigation.len(), before + 1);
            assert_eq!(owner.navigation.last(), Some(&shift));
            let focused = if shift {
                owner.parent_focus.clone()
            } else {
                owner.composer.read(cx).focus_handle(cx)
            };
            assert!(focused.is_focused(window));
        })
        .unwrap();
    }
}

#[gpui::test]
fn focused_child_and_parent_ignore_an_unfocused_siblings_marked_range(cx: &mut TestAppContext) {
    let (handle, fields) = mount(cx);
    action_turn(cx, handle, |window, cx| {
        mark_field("composer", &fields, window, cx)
    });
    action_turn(cx, handle, |window, cx| {
        window.click("first", cx);
        window.input("name", cx);
    });
    action_turn(cx, handle, |window, cx| window.press("enter", cx));
    cx.update_window(handle.into(), |_, window, cx| {
        let state = fields.read(cx).composer.clone();
        assert_marked(&state, true, window, cx);
        let owner = fields.read(cx);
        assert_eq!((owner.first_submissions, owner.submissions), (1, 0));
        assert_eq!((owner.propagated_enters, owner.raw_enters), (0, 0));
    })
    .unwrap();
    action_turn(cx, handle, |window, cx| window.press("escape", cx));
    cx.update_window(handle.into(), |_, window, cx| {
        let state = fields.read(cx).composer.clone();
        assert_marked(&state, true, window, cx);
        let owner = fields.read(cx);
        assert_eq!((owner.escape_actions, owner.cancels), (1, 1));
    })
    .unwrap();
    action_turn(cx, handle, |window, cx| {
        let focus = fields.read(cx).parent_focus.clone();
        window.focus(&focus, cx);
        window.render_frame(cx);
    });
    action_turn(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(Escape), cx)
    });
    action_turn(cx, handle, |window, cx| {
        window.dispatch_action(
            Box::new(Enter {
                secondary: false,
                shift: false,
            }),
            cx,
        );
    });
    cx.update_window(handle.into(), |_, window, cx| {
        let state = fields.read(cx).composer.clone();
        assert_marked(&state, true, window, cx);
        let owner = fields.read(cx);
        assert_eq!((owner.escape_actions, owner.cancels), (2, 2));
        assert_eq!((owner.first_submissions, owner.submissions), (1, 0));
        assert_eq!(owner.propagated_enters, 1);
        assert!(owner.parent_focus.is_focused(window));
    })
    .unwrap();
}

#[gpui::test]
fn composing_escape_preserves_masking_and_disabled_escape_behavior(cx: &mut TestAppContext) {
    let (handle, fields) = mount(cx);
    action_turn(cx, handle, |window, cx| {
        mark_field("first", &fields, window, cx);
        let first = fields.read(cx).first.clone();
        first.update(cx, |state, cx| state.set_masked(true, window, cx));
    });
    action_turn(cx, handle, |window, cx| window.press("escape", cx));
    cx.update_window(handle.into(), |_, window, cx| {
        let state = fields.read(cx).first.clone();
        assert_marked(&state, false, window, cx);
        let owner = fields.read(cx);
        assert!(owner.first.read(cx).presentation().is_masked());
        assert_eq!(owner.cancels, 0);
    })
    .unwrap();
    action_turn(cx, handle, |window, cx| {
        mark_field("first", &fields, window, cx);
        let first = fields.read(cx).first.clone();
        first.update(cx, |state, cx| state.set_disabled(true, cx));
        window.render_frame(cx);
    });
    action_turn(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(Escape), cx)
    });
    cx.update_window(handle.into(), |_, window, cx| {
        let state = fields.read(cx).first.clone();
        assert_marked(&state, true, window, cx);
        let owner = fields.read(cx);
        assert!(owner.first.read(cx).presentation().is_masked());
        assert!(owner.first.read(cx).presentation().is_disabled());
        assert_eq!(owner.cancels, 1); // Original disabled state has no Base Escape handler.
    })
    .unwrap();
}

#[gpui::test]
fn persistent_unicode_editing_focus_and_history(cx: &mut TestAppContext) {
    let (handle, fields) = mount(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.click("first", cx);
        window.input("A🦀Z", cx);
        let state = fields.read(cx).first.clone();
        let identity = state.entity_id();
        assert_eq!(state.read(cx).value(), "A🦀Z");
        window.press("shift-left", cx);
        assert_eq!(state.read(cx).selected_range(), 5..6);
        fields.update(cx, |_, cx| cx.notify());
        window.render_frame(cx);
        assert_eq!(fields.read(cx).first.entity_id(), identity);
        assert_eq!(state.read(cx).selected_range(), 5..6);
        window.input("中", cx);
        assert_eq!(state.read(cx).value(), "A🦀中");
        window.press("cmd-z", cx);
        assert_eq!(state.read(cx).value(), "A🦀Z");
        window.press("cmd-shift-z", cx);
        assert_eq!(state.read(cx).value(), "A🦀中");
    })
    .unwrap();
    let before = fields.read_with(cx, |fields, _| fields.changes);
    cx.update_window(handle.into(), |_, window, cx| {
        let state = fields.read(cx).first.clone();
        state.update(cx, |state, cx| state.set_value("model", window, cx));
        window.render_frame(cx);
    })
    .unwrap();
    assert_eq!(
        fields.read_with(cx, |fields, _| fields.changes),
        before,
        "model loading is silent"
    );
    cx.update_window(handle.into(), |_, window, cx| {
        let state = fields.read(cx).first.clone();
        state.update(cx, |state, cx| state.replace_all("formatted", window, cx));
        window.render_frame(cx);
    })
    .unwrap();
    assert!(fields.read_with(cx, |fields, _| fields.changes) > before);
    cx.update_window(handle.into(), |_, window, cx| {
        let state = fields.read(cx).first.clone();
        window.press("cmd-z", cx);
        assert_eq!(state.read(cx).value(), "model");
        window.click("composer", cx);
        window.input("elsewhere", cx);
        assert_eq!(state.read(cx).value(), "model");
        assert_eq!(fields.read(cx).subscriptions.len(), 2);
    })
    .unwrap();
    cx.run_until_parked();
    assert!(fields.read_with(cx, |fields, _| fields.focus_events) >= 2);
}

#[gpui::test]
fn enter_submission_and_alt_shift_newlines_are_distinct(cx: &mut TestAppContext) {
    let (handle, fields) = mount(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.click("composer", cx);
        window.input("send", cx);
        window.press("enter", cx);
        assert_eq!(fields.read(cx).composer.read(cx).value(), "send");
    })
    .unwrap();
    assert_eq!(fields.read_with(cx, |fields, _| fields.submissions), 1);
    cx.update_window(handle.into(), |_, window, cx| {
        window.press("shift-enter", cx);
        assert_eq!(fields.read(cx).composer.read(cx).value(), "send\n");
    })
    .unwrap();
    assert_eq!(fields.read_with(cx, |fields, _| fields.submissions), 1);
    cx.update_window(handle.into(), |_, window, cx| {
        window.press("alt-enter", cx);
        window.press("alt-shift-enter", cx);
        assert_eq!(fields.read(cx).composer.read(cx).value(), "send\n\n\n");
        window.press("cmd-z", cx);
        assert_eq!(fields.read(cx).composer.read(cx).value(), "send\n\n");
        window.press("cmd-shift-z", cx);
        assert_eq!(fields.read(cx).composer.read(cx).value(), "send\n\n\n");
    })
    .unwrap();
    assert_eq!(fields.read_with(cx, |fields, _| fields.submissions), 1);
    assert_eq!(
        fields.read_with(cx, |fields, _| fields.propagated_enters),
        0
    );
    cx.update_window(handle.into(), |_, window, cx| {
        window.click("notes", cx);
        window.input("line", cx);
        window.press("enter", cx);
        window.input("two", cx);
        assert_eq!(fields.read(cx).notes.read(cx).value(), "line\ntwo");
        let state = fields.read(cx).composer.clone();
        state.update(cx, |state, cx| state.set_submit_on_enter(false, cx));
        fields.update(cx, |this, _| this.composer_enter = EnterBehavior::Newline);
        window.click("composer", cx);
        window.press("enter", cx);
        assert_eq!(state.read(cx).value(), "send\n\n\n\n");
        window.click("first", cx);
        window.input("name", cx);
        window.press("enter", cx);
        assert_eq!(fields.read(cx).first.read(cx).value(), "name");
    })
    .unwrap();
    assert_eq!(fields.read_with(cx, |fields, _| fields.submissions), 1);
    assert_eq!(
        fields.read_with(cx, |fields, _| fields.propagated_enters),
        0
    );
}

#[gpui::test]
fn composition_return_cannot_submit_edit_or_bubble(cx: &mut TestAppContext) {
    let (handle, fields) = mount(cx);
    action_turn(cx, handle, |window, cx| {
        window.click("composer", cx);
        let state = fields.read(cx).composer.clone();
        let mut handler = ElementInputHandler::new(window.find("composer").bounds(), state.clone());
        handler.replace_and_mark_text_in_range(None, "日本", Some(2..2), window, cx);
        window.render_frame(cx);
    });
    for key in ["enter", "shift-enter", "alt-enter", "alt-shift-enter"] {
        action_turn(cx, handle, |window, cx| window.press(key, cx));
        cx.update_window(handle.into(), |_, window, cx| {
            let state = fields.read(cx).composer.clone();
            let mut handler =
                ElementInputHandler::new(window.find("composer").bounds(), state.clone());
            assert_eq!(state.read(cx).value(), "日本");
            assert_eq!(handler.marked_text_range(window, cx), Some(0..2));
            let owner = fields.read(cx);
            assert_eq!(owner.submissions, 0);
            assert_eq!((owner.propagated_enters, owner.raw_enters), (0, 0));
        })
        .unwrap();
    }
    action_turn(cx, handle, |window, cx| {
        let state = fields.read(cx).composer.clone();
        let mut handler = ElementInputHandler::new(window.find("composer").bounds(), state.clone());
        // Native IME commits through its input handler, not an app submit path.
        handler.replace_text_in_range(None, "日本語", window, cx);
        window.render_frame(cx);
    });
    cx.update_window(handle.into(), |_, window, cx| {
        let state = fields.read(cx).composer.clone();
        let mut handler = ElementInputHandler::new(window.find("composer").bounds(), state.clone());
        assert_eq!(handler.marked_text_range(window, cx), None);
        assert_eq!(
            fields.read(cx).submissions,
            0,
            "commit alone does not submit"
        );
    })
    .unwrap();
    action_turn(cx, handle, |window, cx| window.press("enter", cx));
    cx.update_window(handle.into(), |_, _, cx| {
        let state = fields.read(cx).composer.clone();
        assert_eq!(state.read(cx).value(), "日本語");
        let owner = fields.read(cx);
        assert_eq!(owner.submissions, 1);
        assert_eq!((owner.propagated_enters, owner.raw_enters), (0, 0));
    })
    .unwrap();
    action_turn(cx, handle, |window, cx| window.render_frame(cx));
    assert_eq!(fields.read_with(cx, |owner, _| owner.submissions), 1);
}

#[gpui::test]
fn released_base_enter_emits_and_propagates_without_composition_guard(cx: &mut TestAppContext) {
    let (handle, fields) = mount(cx);
    fields.update(cx, |fields, cx| {
        fields.raw_base = true;
        cx.notify();
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("composer", cx);
        window.input("send", cx);
        window.press("enter", cx);
        assert_eq!(fields.read(cx).composer.read(cx).value(), "send");
    })
    .unwrap();
    assert_eq!(fields.read_with(cx, |fields, _| fields.submissions), 1);
    assert_eq!(
        fields.read_with(cx, |fields, _| fields.propagated_enters),
        1
    );
    cx.update_window(handle.into(), |_, window, cx| {
        window.press("shift-enter", cx);
        assert_eq!(fields.read(cx).composer.read(cx).value(), "send\n");
        let state = fields.read(cx).composer.clone();
        let mut handler = ElementInputHandler::new(window.find("composer").bounds(), state.clone());
        handler.replace_and_mark_text_in_range(None, "中", Some(1..1), window, cx);
        window.render_frame(cx);
        window.press("enter", cx);
        assert_eq!(handler.marked_text_range(window, cx), Some(5..6));
    })
    .unwrap();
    assert_eq!(fields.read_with(cx, |fields, _| fields.submissions), 2);
    assert_eq!(
        fields.read_with(cx, |fields, _| fields.propagated_enters),
        2
    );
}

#[gpui::test]
fn native_clipboard_text_and_app_owned_files_respect_editability(cx: &mut TestAppContext) {
    let (handle, fields) = mount(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.click("first", cx);
        cx.write_to_clipboard(ClipboardItem::new_string("A\n🦀".into()));
        window.press("cmd-v", cx);
        assert_eq!(fields.read(cx).first.read(cx).value(), "A🦀");
        window.press("cmd-a", cx);
        window.press("cmd-c", cx);
        assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), "A🦀");
        window.click("composer", cx);
        cx.write_to_clipboard(ClipboardItem::new_string("one\ntwo".into()));
        window.press("cmd-v", cx);
        let composer = fields.read(cx).composer.clone();
        assert_eq!(composer.read(cx).value(), "one\ntwo");
        let path = std::path::PathBuf::from("/private/fixture/attachment.png");
        cx.write_to_clipboard(ClipboardItem {
            entries: vec![ClipboardEntry::ExternalPaths(ExternalPaths(
                vec![path.clone()].into(),
            ))],
        });
        window.press("cmd-v", cx);
        assert_eq!(fields.read(cx).attachments.borrow().as_slice(), &[path]);
        assert_eq!(composer.read(cx).value(), "one\ntwo");
        composer.update(cx, |state, cx| state.set_readonly(true, cx));
        window.press("cmd-v", cx);
        window.input("ignored", cx);
        assert_eq!(fields.read(cx).attachments.borrow().len(), 1);
        assert_eq!(composer.read(cx).value(), "one\ntwo");
        composer.update(cx, |state, cx| {
            state.set_readonly(false, cx);
            state.set_disabled(true, cx);
        });
        window.press("cmd-v", cx);
        assert_eq!(fields.read(cx).attachments.borrow().len(), 1);
    })
    .unwrap();
}

#[gpui::test]
fn ime_utf16_composition_preserves_the_persistent_engine(cx: &mut TestAppContext) {
    let (handle, fields) = mount(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.click("first", cx);
        window.input("A🦀Z", cx);
        let state = fields.read(cx).first.clone();
        let mut handler = ElementInputHandler::new(window.find("first").bounds(), state.clone());
        handler.replace_and_mark_text_in_range(Some(1..3), "に", Some(1..1), window, cx);
        assert_eq!(handler.marked_text_range(window, cx), Some(1..2));
        handler.replace_and_mark_text_in_range(None, "日本", Some(2..2), window, cx);
        handler.replace_text_in_range(None, "日本語", window, cx);
        window.render_frame(cx);
        assert_eq!(state.read(cx).value(), "A日本語Z");
        assert_eq!(handler.marked_text_range(window, cx), None);
        assert_eq!(state.read(cx).cursor(), 10);
        window.press("cmd-z", cx);
        assert_eq!(state.read(cx).value(), "A🦀Z");
        window.click("notes", cx);
        let notes = fields.read(cx).notes.clone();
        let mut handler = ElementInputHandler::new(window.find("notes").bounds(), notes.clone());
        handler.replace_and_mark_text_in_range(None, "中🦀", Some(3..3), window, cx);
        handler.unmark_text(window, cx);
        window.render_frame(cx);
        assert_eq!(notes.read(cx).value(), "中🦀");
        assert_eq!(handler.marked_text_range(window, cx), None);
    })
    .unwrap();
}

#[gpui::test]
fn glyph_hit_testing_drag_selection_and_compact_wrapped_layout(cx: &mut TestAppContext) {
    let (handle, fields) = mount(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.click("first", cx);
        window.input("abcdef", cx);
        let state = fields.read(cx).first.clone();
        let from = state.read(cx).range_to_bounds(&(1..1)).unwrap().center();
        let to = state.read(cx).range_to_bounds(&(4..4)).unwrap().center();
        let bounds = window.find("first").bounds();
        window.click_at("first", to - bounds.origin, cx);
        assert_eq!(state.read(cx).cursor(), 4, "glyph click places the caret");
        window.drag(from, to, cx);
        assert_eq!(state.read(cx).selected_range(), 1..4);
        window.input("X", cx);
        assert_eq!(state.read(cx).value(), "aXef");
        let single = window.find("first").bounds();
        assert_eq!(single.size.width, px(220.));
        assert!(
            single.size.height >= px(24.) && single.size.height <= px(30.),
            "{single:?}"
        );
        window.click("composer", cx);
        window.click_at("first", point(px(2.), px(2.)), cx);
        assert!(
            state.read(cx).focus_handle(cx).is_focused(window),
            "padding focuses"
        );
        let composer = fields.read(cx).composer.clone();
        composer.update(cx, |state, cx| {
            state.set_value("a long wrapped message ".repeat(30), window, cx)
        });
        window.render_frame(cx);
        let multi = window.find("composer").bounds();
        assert_eq!(multi.size.width, single.size.width);
        assert!(
            multi.size.height > single.size.height && multi.size.height <= px(80.),
            "{multi:?}"
        );
        let start = composer.read(cx).range_to_bounds(&(0..0)).unwrap();
        let wrapped = composer.read(cx).range_to_bounds(&(50..50)).unwrap();
        assert!(
            wrapped.origin.y > start.origin.y,
            "text must wrap to another visual row"
        );
        assert!(window.find("notes").bounds().origin.y >= multi.bottom());
    })
    .unwrap();
}

#[gpui::test]
fn frame_padding_keeps_focus_and_selection_after_pointer_down_and_up(cx: &mut TestAppContext) {
    let (handle, fields) = mount(cx);
    for id in ["first", "composer"] {
        for (readonly, disabled) in [(false, false), (true, false), (false, true)] {
            for corner in 0..4 {
                let mut position = point(px(0.), px(0.));
                action_turn(cx, handle, |window, cx| {
                    let (first, composer, parent_focus) = {
                        let owner = fields.read(cx);
                        (
                            owner.first.clone(),
                            owner.composer.clone(),
                            owner.parent_focus.clone(),
                        )
                    };
                    if id == "first" {
                        first.update(cx, |state, cx| {
                            state.set_value("abcdef", window, cx);
                            state.set_selected_range(1..4, cx);
                            state.set_readonly(readonly, cx);
                            state.set_disabled(disabled, cx);
                        });
                    } else {
                        composer.update(cx, |state, cx| {
                            state.set_value("abcdef", window, cx);
                            state.set_selected_range(1..4, cx);
                            state.set_readonly(readonly, cx);
                            state.set_disabled(disabled, cx);
                        });
                    }
                    window.focus(&parent_focus, cx);
                    window.render_frame(cx);
                    let bounds = window.find(id).bounds();
                    // All four points are inside the frame but outside Base's
                    // text child: exercise the surrounding padding hitbox.
                    position = bounds.origin
                        + point(
                            if corner % 2 == 0 {
                                px(2.)
                            } else {
                                bounds.size.width - px(2.)
                            },
                            if corner < 2 {
                                px(2.)
                            } else {
                                bounds.size.height - px(2.)
                            },
                        );
                    window.dispatch_event(
                        MouseMoveEvent {
                            position,
                            pressed_button: None,
                            modifiers: Default::default(),
                        }
                        .to_platform_input(),
                        cx,
                    );
                    window.render_frame(cx);
                });
                for down in [true, false] {
                    action_turn(cx, handle, |window, cx| {
                        if down {
                            window.dispatch_event(
                                MouseDownEvent {
                                    button: MouseButton::Left,
                                    position,
                                    modifiers: Default::default(),
                                    click_count: 1,
                                    first_mouse: false,
                                }
                                .to_platform_input(),
                                cx,
                            );
                        } else {
                            window.dispatch_event(
                                MouseUpEvent {
                                    button: MouseButton::Left,
                                    position,
                                    modifiers: Default::default(),
                                    click_count: 1,
                                }
                                .to_platform_input(),
                                cx,
                            );
                        }
                        window.render_frame(cx);
                    });
                    cx.update_window(handle.into(), |_, window, cx| {
                        let owner = fields.read(cx);
                        let (value, selection, focus) = if id == "first" {
                            let state = owner.first.read(cx);
                            (
                                state.value(),
                                state.selected_range(),
                                state.focus_handle(cx),
                            )
                        } else {
                            let state = owner.composer.read(cx);
                            (
                                state.value(),
                                state.selected_range(),
                                state.focus_handle(cx),
                            )
                        };
                        assert_eq!(value, "abcdef");
                        assert_eq!(selection, 1..4, "padding must not move the caret");
                        assert_eq!(
                            focus.is_focused(window),
                            !disabled,
                            "{id}, corner {corner}, down={down}"
                        );
                        assert_eq!(owner.parent_focus.is_focused(window), disabled);
                    })
                    .unwrap();
                }
            }
        }
    }
}

#[gpui::test]
fn palette_updates_keep_values_selection_and_native_ghostty_binding(cx: &mut TestAppContext) {
    let (handle, fields) = mount(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        let first = fields.read(cx).first.clone();
        first.update(cx, |state, cx| {
            state.set_value("retained", window, cx);
            state.set_selected_range(1..4, cx);
        });
    })
    .unwrap();
    for palette in [
        theme::Palette::NATIVE_LIGHT,
        theme::Palette::NATIVE_DARK,
        theme::Palette::RIWORK,
    ] {
        cx.update(|cx| {
            cx.set_global(theme::Appearance {
                selected: theme::ThemeChoice::Native,
                palette,
                terminal: None,
                ghostty: None,
                error: None,
            });
        });
        cx.update_window(handle.into(), |_, window, cx| {
            let first = fields.read(cx).first.clone();
            window.render_frame(cx);
            let kit = Theme::global(cx);
            assert_eq!(kit.tokens.colors.foreground, Hsla::from(rgb(palette.text)));
            assert_eq!(kit.tokens.colors.ring, Hsla::from(rgb(palette.focus)));
            assert_eq!(first.read(cx).value(), "retained");
            assert_eq!(first.read(cx).selected_range(), 1..4);
        })
        .unwrap();
    }
    cx.update_window(handle.into(), |_, window, cx| {
        // Headless GPUI deliberately exposes no native window handle. Prove
        // the real Ghostty macro's entry point rejects it without a process.
        fields.update(cx, |_, cx| {
            let options =
                gpui_libghostty::TerminalOptions::new("/usr/bin/true", std::env::temp_dir());
            let error = crate::Terminal::spawn(options, window, cx)
                .err()
                .expect("headless has no native handle");
            assert!(error.contains("window handle"), "{error}");
        });
        assert!(!crate::metal_layer::limit_drawables(window));
    })
    .unwrap();
}
