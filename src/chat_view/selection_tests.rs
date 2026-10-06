//! Source-only fixtures: actual ChatView, one Base Root/layer per headless
//! window, native pointer/key dispatch and test-platform clipboard/AX facts.
//! The Feed records deliveries and never opens a socket or starts a provider.
use super::*;
use crate::chat::model::{Item, ItemStatus, Presentation, QuestionPrompt};
use gpui::{
    ClipboardItem, InputEvent as _, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, SharedString, TestAppContext, point,
};
use gpui_kit::{
    base::{TextSelection, input::Copy},
    test::TestWindowExt,
};

fn message(id: &str, text: &str) -> Item {
    Item {
        id: id.into(),
        turn_id: None,
        status: ItemStatus::Completed,
        presentation: Presentation::default(),
        body: ItemBody::AgentMessage { text: text.into() },
    }
}

fn install(view: &Entity<ChatView>, items: Vec<Item>, window: &mut Window, cx: &mut gpui::App) {
    view.update(cx, |v, cx| {
        v.model.link = Link::Live;
        v.model.transcript.items = items;
        v.refresh_projection(cx);
        v.list.remeasure();
        cx.notify();
    });
    window.render_frame(cx);
}

fn ends(window: &Window, id: &str) -> (Point<Pixels>, Point<Pixels>) {
    let bounds = window.find(SharedString::from(id.to_owned())).bounds();
    (
        point(bounds.left() + px(1.), bounds.top() + px(6.)),
        point(bounds.right() - px(1.), bounds.bottom() - px(6.)),
    )
}

fn select_between(window: &mut Window, first: &str, last: &str, cx: &mut gpui::App) {
    window.render_frame(cx);
    let from = ends(window, first).0;
    let to = ends(window, last).1;
    window.drag(from, to, cx);
}

fn copied(window: &mut Window, cx: &mut gpui::App) -> String {
    cx.write_to_clipboard(ClipboardItem::new_string("unchanged sentinel".into()));
    window.press("cmd-c", cx);
    cx.read_from_clipboard().unwrap().text().unwrap().to_owned()
}

#[gpui::test]
fn base_drag_crosses_unicode_paragraphs_and_messages_in_reading_order(cx: &mut TestAppContext) {
    let (handle, view, recording) = editor_tests::mount(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        install(
            &view,
            vec![
                message("first", "alpha 🦀 café\n\nbeta"),
                message("second", "gamma"),
            ],
            window,
            cx,
        );
        let ax = window.find("transcript:first/0");
        assert_eq!(ax.role(), Some(gpui::Role::Label));
        assert_eq!(ax.label(), Some("alpha 🦀 café"));
        select_between(window, "transcript:first/0", "transcript:second/0", cx);
        assert_eq!(copied(window, cx), "alpha 🦀 café\nbeta\ngamma");
        // Native menu/action routing uses the same Base Copy action.
        cx.write_to_clipboard(ClipboardItem::new_string("menu sentinel".into()));
        window.dispatch_action(Box::new(Copy), cx);
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            "alpha 🦀 café\nbeta\ngamma"
        );
    })
    .unwrap();
    assert!(recording.try_recv().is_err());
}

fn pointer_click(
    window: &mut Window,
    at: Point<Pixels>,
    count: usize,
    shift: bool,
    cx: &mut gpui::App,
) {
    window.dispatch_event(
        MouseMoveEvent {
            position: at,
            pressed_button: None,
            modifiers: Modifiers::default(),
        }
        .to_platform_input(),
        cx,
    );
    window.render_frame(cx);
    let modifiers = Modifiers {
        shift,
        ..Default::default()
    };
    window.dispatch_event(
        MouseDownEvent {
            position: at,
            button: MouseButton::Left,
            modifiers,
            click_count: count,
            first_mouse: false,
        }
        .to_platform_input(),
        cx,
    );
    window.render_frame(cx);
    window.dispatch_event(
        MouseUpEvent {
            position: at,
            button: MouseButton::Left,
            modifiers,
            click_count: count,
        }
        .to_platform_input(),
        cx,
    );
    window.render_frame(cx);
}

#[gpui::test]
fn base_word_line_and_shift_extension_use_real_pointer_events(cx: &mut TestAppContext) {
    let (handle, view, _) = editor_tests::mount(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        install(
            &view,
            vec![message("words", "alpha beta"), message("end", "gamma")],
            window,
            cx,
        );
        let at = ends(window, "transcript:words/0").0 + point(px(10.), px(0.));
        pointer_click(window, at, 2, false, cx);
        assert_eq!(copied(window, cx), "alpha");
        pointer_click(window, at, 3, false, cx);
        assert_eq!(copied(window, cx), "alpha beta");
        let first = ends(window, "transcript:words/0").0;
        let last = ends(window, "transcript:end/0").1;
        pointer_click(window, first, 1, false, cx);
        pointer_click(window, last, 1, true, cx);
        assert_eq!(copied(window, cx), "alpha beta\ngamma");
    })
    .unwrap();
}

#[gpui::test]
fn rich_code_table_and_tool_output_participate_in_one_library_selection(cx: &mut TestAppContext) {
    let (handle, view, recording) = editor_tests::mount(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        let mut tool = message("tool", "");
        tool.body = ItemBody::ToolCall { server: None, tool: "fixture".into(), input: serde_json::json!({}), output: Some("tool 🦀 tail".into()) };
        view.update(cx, |v, cx| { v.open.insert("tool".into()); v.apply_display_mode(DisplayMode::Verbose, cx); });
        install(&view, vec![message("rich", "**bold café**\n\n```rust\n  code 🦀\n```\n\n| H1 | H2 |\n| --- | --- |\n| C1 | C2 |"), tool], window, cx);
        select_between(window, "transcript:rich/0", "transcript:tool-output:tool", cx);
        let text = copied(window, cx);
        assert!(text.starts_with("bold café\n  code 🦀\n"), "{text:?}");
        let ordered = ["H1", "H2", "C1", "C2", "tool 🦀 tail"];
        let mut previous = 0;
        for part in ordered {
            let at = text[previous..].find(part).unwrap_or_else(|| panic!("missing ordered {part:?} in {text:?}")) + previous;
            previous = at + part.len();
        }
        assert_eq!(text.matches("tool 🦀 tail").count(), 1);
    }).unwrap();
    assert!(recording.try_recv().is_err());
}

#[gpui::test]
fn focused_composer_model_and_request_answer_keep_copy_priority(cx: &mut TestAppContext) {
    let (handle, view, recording) = editor_tests::mount(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        install(
            &view,
            vec![message("copy", "transcript secret")],
            window,
            cx,
        );
        select_between(window, "transcript:copy/0", "transcript:copy/0", cx);
        window.click("chat-composer", cx);
        window.input("draft 🦀", cx);
        window.press("cmd-a", cx);
        assert_eq!(copied(window, cx), "draft 🦀");
        window.press("right", cx);
        assert_eq!(
            copied(window, cx),
            "",
            "an empty editor selection must not copy old transcript text"
        );
        view.update(cx, |v, cx| v.toggle_menu(Menu::Model, window, cx));
        window.render_frame(cx);
        window.click("chat-model-input", cx);
        window.input("fixture-model", cx);
        window.press("cmd-a", cx);
        assert_eq!(copied(window, cx), "fixture-model");
        let question = Question {
            request_id: "copy-request".into(),
            questions: vec![QuestionPrompt {
                header: None,
                question: "Which?".into(),
                options: vec![],
                multi_select: false,
            }],
        };
        view.update(cx, |v, cx| {
            v.menu = None;
            v.model.transcript.questions = vec![question.clone()];
            v.ready_inputs(window, cx);
            v.answers[&editors::AnswerKey::of(&question, 0)]
                .state
                .read(cx)
                .focus_handle(cx)
                .focus(window, cx);
            cx.notify();
        });
        window.render_frame(cx);
        window.input("answer café", cx);
        window.press("cmd-a", cx);
        assert_eq!(copied(window, cx), "answer café");
        assert_eq!(view.read(cx).composer.read(cx).value(), "draft 🦀");
    })
    .unwrap();
    assert!(recording.try_recv().is_err());
}

#[gpui::test]
fn streaming_mode_and_other_window_cannot_copy_stale_or_foreign_selection(cx: &mut TestAppContext) {
    let (first, a, _) = editor_tests::mount(cx);
    let (second, b, _) = editor_tests::mount(cx);
    for (handle, view, text) in [(first, &a, "one 🦀"), (second, &b, "two café")] {
        cx.update_window(handle.into(), |_, window, cx| {
            install(view, vec![message("shared-id", text)], window, cx);
            select_between(
                window,
                "transcript:shared-id/0",
                "transcript:shared-id/0",
                cx,
            );
            assert_eq!(copied(window, cx), text);
        })
        .unwrap();
    }
    cx.update_window(first.into(), |_, window, cx| {
        a.update(cx, |v, cx| {
            v.model.transcript.items[0].body = ItemBody::AgentMessage {
                text: "new 🦀".into(),
            };
            v.sync_list(
                &Applied {
                    touched: [0].into_iter().collect(),
                    ..Default::default()
                },
                cx,
            );
            cx.notify();
        });
        // Invalidation precedes the next render, so even menu Copy cannot
        // retrieve a projection from the replaced stream.
        assert!(TextSelection::selected_text(window, cx).is_empty());
        window.render_frame(cx);
        select_between(
            window,
            "transcript:shared-id/0",
            "transcript:shared-id/0",
            cx,
        );
        assert_eq!(copied(window, cx), "new 🦀");
        a.update(cx, |v, cx| v.apply_display_mode(DisplayMode::Verbose, cx));
        assert!(TextSelection::selected_text(window, cx).is_empty());
    })
    .unwrap();
    cx.update_window(second.into(), |_, window, cx| {
        assert_eq!(copied(window, cx), "two café")
    })
    .unwrap();
}

#[gpui::test]
fn wrapped_unicode_drag_survives_a_wheel_repaint(cx: &mut TestAppContext) {
    let (handle, view, _) = editor_tests::mount(cx);
    let text = "🦀 café e\u{301} wrapped words "
        .repeat(150)
        .trim()
        .to_owned();
    cx.update_window(handle.into(), |_, window, cx| {
        install(
            &view,
            vec![message("wrapped", &text), message("tail", "tail")],
            window,
            cx,
        );
        view.update(cx, |v, cx| {
            v.list.pause_following_tail();
            v.list.scroll_to(gpui::ListOffset {
                item_ix: 0,
                offset_in_item: px(0.),
            });
            cx.notify();
        });
        window.render_frame(cx);
        let bounds = window.find("transcript:wrapped/0").bounds();
        assert!(bounds.size.height > px(30.), "fixture must wrap");
        window.drag(
            point(bounds.left() + px(1.), bounds.top() + px(6.)),
            point(bounds.left() + px(60.), bounds.top() + px(42.)),
            cx,
        );
        let before = copied(window, cx);
        assert!(before.contains("🦀 café e\u{301}"));
        assert!(!before.contains("tail"));
        let offset = view.read(cx).list.logical_scroll_top();
        // GPUI wheel dispatch plus reflow; no custom endpoint adjustment.
        window.dispatch_event(
            gpui::ScrollWheelEvent {
                position: bounds.origin + point(px(20.), px(60.)),
                delta: gpui::ScrollDelta::Pixels(point(px(0.), px(-12.))),
                modifiers: Default::default(),
                touch_phase: gpui::TouchPhase::Moved,
            }
            .to_platform_input(),
            cx,
        );
        window.render_frame(cx);
        let after = view.read(cx).list.logical_scroll_top();
        assert!(
            after.item_ix != offset.item_ix || after.offset_in_item != offset.offset_in_item,
            "fixture must actually scroll"
        );
        assert_eq!(copied(window, cx), before);
    })
    .unwrap();
}

#[gpui::test]
fn selecting_a_path_does_not_open_it_but_a_collapsed_click_does(cx: &mut TestAppContext) {
    use std::{cell::RefCell, rc::Rc};
    let (handle, view, recording) = editor_tests::mount(cx);
    let opened = Rc::new(RefCell::new(Vec::new()));
    let result = opened.clone();
    let _events = cx.update(|cx| {
        cx.subscribe(&view, move |_, event, _| {
            if let ChatViewEvent::OpenFile { target } = event {
                result.borrow_mut().push(target.clone());
            }
        })
    });
    cx.update_window(handle.into(), |_, window, cx| {
        install(
            &view,
            vec![message("link", "[fixture](/fixture/file.rs)")],
            window,
            cx,
        );
        let (from, to) = ends(window, "transcript:link/0");
        window.drag(from, to, cx);
        assert_eq!(copied(window, cx), "fixture");
        assert!(opened.borrow().is_empty());
        pointer_click(window, from + point(px(10.), px(0.)), 3, false, cx);
        assert!(opened.borrow().is_empty());
        pointer_click(window, from + point(px(10.), px(0.)), 1, false, cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(*opened.borrow(), vec!["/fixture/file.rs".to_owned()]);
    assert!(recording.try_recv().is_err());
}

#[gpui::test]
fn library_copy_adapter_preserves_selected_code_indentation(cx: &mut TestAppContext) {
    let (handle, view, recording) = editor_tests::mount(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        install(
            &view,
            vec![message("indent", "```\n  code 🦀\n    nested\n```")],
            window,
            cx,
        );
        select_between(
            window,
            "transcript:code:indent/0",
            "transcript:code:indent/0",
            cx,
        );
        assert_eq!(copied(window, cx), "  code 🦀\n    nested");
    })
    .unwrap();
    assert!(recording.try_recv().is_err());
}

#[gpui::test]
fn same_length_diff_replacement_changes_visible_copy_and_clears_old_selection(
    cx: &mut TestAppContext,
) {
    use crate::chat::model::{ChangeKind, FileChange};
    let (handle, view, recording) = editor_tests::mount(cx);
    let change = |text: &str| {
        let mut item = message("edit", "");
        item.body = ItemBody::FileChange {
            changes: vec![FileChange {
                path: "fixture.rs".into(),
                kind: ChangeKind::Modify,
                diff: Some(text.into()),
            }],
        };
        item
    };
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |v, cx| {
            v.open.insert("edit".into());
            v.apply_display_mode(DisplayMode::Verbose, cx);
        });
        install(&view, vec![change("+old")], window, cx);
        select_between(
            window,
            "transcript:diff:edit#0:0",
            "transcript:diff:edit#0:0",
            cx,
        );
        let old = copied(window, cx);
        assert!(old.contains("old"));
        view.update(cx, |v, cx| {
            v.model.transcript.items[0] = change("+new");
            v.sync_list(
                &Applied {
                    touched: [0].into_iter().collect(),
                    ..Default::default()
                },
                cx,
            );
            cx.notify();
        });
        assert!(TextSelection::selected_text(window, cx).is_empty());
        window.render_frame(cx);
        select_between(
            window,
            "transcript:diff:edit#0:0",
            "transcript:diff:edit#0:0",
            cx,
        );
        let new = copied(window, cx);
        assert!(new.contains("new"));
        assert!(!new.contains("old"));
    })
    .unwrap();
    assert!(recording.try_recv().is_err());
}
