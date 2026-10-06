//! Headless macOS-linked smoke: real Base states, layout, and event dispatch.
use super::*;
use gpui::{
    Bounds, ClipboardEntry, Context, ElementInputHandler, ExternalPaths, InputHandler, IntoElement,
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
    propagated_enters: usize,
    raw_base: bool,
    composer_enter: EnterBehavior,
    focus_events: usize,
    attachments: Rc<RefCell<Vec<std::path::PathBuf>>>,
    subscriptions: Vec<Subscription>,
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
            .size_full()
            .flex()
            .flex_col()
            .gap(px(8.))
            .on_action(cx.listener(|this, _: &Enter, _, _| this.propagated_enters += 1))
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
                                _ => {}
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
                            propagated_enters: 0,
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
    cx.update_window(handle.into(), |_, window, cx| {
        window.click("composer", cx);
        let state = fields.read(cx).composer.clone();
        let mut handler = ElementInputHandler::new(window.find("composer").bounds(), state.clone());
        handler.replace_and_mark_text_in_range(None, "日本", Some(2..2), window, cx);
        window.render_frame(cx);
        for key in ["enter", "shift-enter", "alt-enter"] {
            window.press(key, cx);
            assert_eq!(state.read(cx).value(), "日本");
            assert_eq!(handler.marked_text_range(window, cx), Some(0..2));
        }
    })
    .unwrap();
    assert_eq!(fields.read_with(cx, |fields, _| fields.submissions), 0);
    assert_eq!(
        fields.read_with(cx, |fields, _| fields.propagated_enters),
        0
    );
    cx.update_window(handle.into(), |_, window, cx| {
        let state = fields.read(cx).composer.clone();
        let mut handler = ElementInputHandler::new(window.find("composer").bounds(), state.clone());
        // Native IME commits through its input handler, not an app submit path.
        handler.replace_text_in_range(None, "日本語", window, cx);
        window.render_frame(cx);
        assert_eq!(handler.marked_text_range(window, cx), None);
        window.press("enter", cx);
        assert_eq!(state.read(cx).value(), "日本語");
    })
    .unwrap();
    assert_eq!(fields.read_with(cx, |fields, _| fields.submissions), 1);
    assert_eq!(
        fields.read_with(cx, |fields, _| fields.propagated_enters),
        0
    );
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
