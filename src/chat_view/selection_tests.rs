//! Source-only fixtures: actual ChatView, one Base Root/layer per headless
//! window, native pointer/key dispatch and test-platform clipboard/AX facts.
//! The Feed records deliveries and never opens a socket or starts a provider.
use super::*;
use crate::chat::model::{Item, ItemStatus, Presentation};
use gpui::{
    ClipboardItem, InputEvent as _, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    Pixels, Point, SharedString, TestAppContext, point,
};
use gpui_kit::{
    base::{TextSelection, input::Copy},
    test::TestWindowExt,
};

fn message(id: &str, text: &str) -> Item {
    Item {
        id: id.into(),
        turn_id: Some(format!("fixture-turn:{id}")),
        status: ItemStatus::Completed,
        presentation: Presentation {
            phase: Some(crate::chat::model::MessagePhase::Final),
            ..Presentation::default()
        },
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

fn select_glyph_span(
    view: &Entity<ChatView>,
    window: &mut Window,
    first: &str,
    last: &str,
    cx: &mut gpui::App,
) {
    window.render_frame(cx);
    // These are native, visible TextLayout coordinates, checked against both
    // native byte hit testing and the source-key resolver before real dispatch.
    let selection = &view.read(cx).transcript_selection;
    let from = selection.glyph_span_points(first).0;
    let to = selection.glyph_span_points(last).1;
    window.drag(from, to, cx);
}

fn copied(window: &mut Window, cx: &mut gpui::App) -> String {
    cx.write_to_clipboard(ClipboardItem::new_string("unchanged sentinel".into()));
    window.press("cmd-c", cx);
    cx.read_from_clipboard().unwrap().text().unwrap().to_owned()
}

#[gpui::test]
fn base_drag_crosses_unicode_paragraphs_and_messages_in_reading_order(cx: &mut TestAppContext) {
    let (handle, view, recording) = editor_tests::mount_selection(cx);
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
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        cx.read_from_clipboard().unwrap().text().unwrap(),
        "alpha 🦀 café\nbeta\ngamma"
    );
    assert!(recording.try_recv().is_err());
}

#[gpui::test]
fn streaming_mode_and_other_window_cannot_copy_stale_or_foreign_selection(cx: &mut TestAppContext) {
    let (first, a, _) = editor_tests::mount_selection(cx);
    let (second, b, _) = editor_tests::mount_selection(cx);
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
fn library_copy_adapter_preserves_selected_code_indentation(cx: &mut TestAppContext) {
    let (handle, view, recording) = editor_tests::mount_selection(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        install(
            &view,
            vec![message("indent", "```\n  code 🦀\n    nested\n```")],
            window,
            cx,
        );
        select_glyph_span(&view, window, "code:indent/0", "code:indent/0", cx);
        assert_eq!(copied(window, cx), "  code 🦀\n    nested");
    })
    .unwrap();
    assert!(recording.try_recv().is_err());
}

#[gpui::test]
#[ignore = "slow: ~150 wheel/drag frames in a headless window (about 4 s)"]
fn virtualized_partial_selection_exports_unmounted_endpoints_and_middle_both_directions(
    cx: &mut TestAppContext,
) {
    const FIRST_BYTE: usize = 10; // before é
    const LAST_BYTE: usize = 17; // after 🦀
    for reverse in [false, true] {
        let (handle, view, recording) = editor_tests::mount_selection(cx);
        let texts = (0..40)
            .map(|row| format!("row {row:02} café 🦀 {}TAIL", "body ".repeat(160)))
            .collect::<Vec<_>>();
        let items = texts
            .iter()
            .enumerate()
            .map(|(row, text)| message(&format!("virtual-{row}"), text))
            .collect();
        cx.update_window(handle.into(), |_, window, cx| {
            install(&view, items, window, cx);
            view.update(cx, |v, cx| {
                v.list.pause_following_tail();
                v.list.scroll_to(gpui::ListOffset {
                    item_ix: if reverse { 25 } else { 0 },
                    offset_in_item: px(0.),
                });
                cx.notify();
            });
            window.render_frame(cx);
        })
        .unwrap();
        cx.run_until_parked();
        let first_key = if reverse {
            "virtual-25/0"
        } else {
            "virtual-0/0"
        };
        let last_key = if reverse {
            "virtual-0/0"
        } else {
            "virtual-25/0"
        };
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            let at = view
                .read(cx)
                .transcript_selection
                .glyph_point(first_key, if reverse { LAST_BYTE } else { FIRST_BYTE })
                .unwrap();
            assert!(view.read(cx).list.viewport_bounds().contains(&at));
            window.dispatch_event(
                MouseMoveEvent {
                    position: at,
                    pressed_button: None,
                    modifiers: Default::default(),
                }
                .to_platform_input(),
                cx,
            );
            window.dispatch_event(
                MouseDownEvent {
                    position: at,
                    button: MouseButton::Left,
                    modifiers: Default::default(),
                    click_count: 1,
                    first_mouse: false,
                }
                .to_platform_input(),
                cx,
            );
            window.render_frame(cx);
        })
        .unwrap();
        let mut reached = false;
        for _ in 0..100 {
            reached = cx
                .update_window(handle.into(), |_, window, cx| {
                    window.render_frame(cx);
                    let viewport = view.read(cx).list.viewport_bounds();
                    if let Some(at) = view
                        .read(cx)
                        .transcript_selection
                        .glyph_point(last_key, if reverse { FIRST_BYTE } else { LAST_BYTE })
                        .filter(|at| viewport.contains(at))
                    {
                        assert!(
                            view.read(cx)
                                .transcript_selection
                                .glyph_point(first_key, FIRST_BYTE)
                                .is_none(),
                            "anchor must genuinely unmount during the held drag"
                        );
                        assert!(
                            window
                                .try_find(SharedString::from(format!("transcript:{first_key}")))
                                .is_none()
                        );
                        assert!(
                            window.try_find("transcript:virtual-10/0").is_none(),
                            "an intervening selected message must also unmount during the held drag"
                        );
                        window.dispatch_event(
                            MouseMoveEvent {
                                position: at,
                                pressed_button: Some(MouseButton::Left),
                                modifiers: Default::default(),
                            }
                            .to_platform_input(),
                            cx,
                        );
                        window.render_frame(cx);
                        window.dispatch_event(
                            MouseUpEvent {
                                position: at,
                                button: MouseButton::Left,
                                modifiers: Default::default(),
                                click_count: 1,
                            }
                            .to_platform_input(),
                            cx,
                        );
                        window.render_frame(cx);
                        true
                    } else {
                        let at = viewport.center();
                        window.dispatch_event(
                            gpui::ScrollWheelEvent {
                                position: at,
                                delta: gpui::ScrollDelta::Pixels(point(
                                    px(0.),
                                    px(if reverse { 180. } else { -180. }),
                                )),
                                ..Default::default()
                            }
                            .to_platform_input(),
                            cx,
                        );
                        window.render_frame(cx);
                        window.dispatch_event(
                            MouseMoveEvent {
                                position: at,
                                pressed_button: Some(MouseButton::Left),
                                modifiers: Default::default(),
                            }
                            .to_platform_input(),
                            cx,
                        );
                        window.render_frame(cx);
                        false
                    }
                })
                .unwrap();
            cx.run_until_parked();
            if reached {
                break;
            }
        }
        assert!(reached, "bounded real wheel sequence must reach cursor row");
        let expected = std::iter::once(&texts[0][FIRST_BYTE..])
            .chain(texts[1..25].iter().map(String::as_str))
            .chain(std::iter::once(&texts[25][..LAST_BYTE]))
            .collect::<Vec<_>>()
            .join("\n");
        cx.update_window(handle.into(), |_, window, cx| {
            assert_eq!(copied(window, cx), expected)
        })
        .unwrap();
        // Move the viewport beyond BOTH endpoints after release. Copy must
        // remain partial and include all source rows, not only painted leaves.
        for _ in 0..50 {
            cx.update_window(handle.into(), |_, window, cx| {
                let at = view.read(cx).list.viewport_bounds().center();
                window.dispatch_event(
                    gpui::ScrollWheelEvent {
                        position: at,
                        delta: gpui::ScrollDelta::Pixels(point(px(0.), px(-300.))),
                        ..Default::default()
                    }
                    .to_platform_input(),
                    cx,
                );
                window.render_frame(cx);
            })
            .unwrap();
            cx.run_until_parked();
        }
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            for row in [0, 10, 25] {
                assert!(
                    window
                        .try_find(SharedString::from(format!("transcript:virtual-{row}/0")))
                        .is_none(),
                    "endpoint/middle element must genuinely unmount, row {row}"
                );
                assert!(
                    view.read(cx)
                        .transcript_selection
                        .glyph_point(&format!("virtual-{row}/0"), 0)
                        .is_none()
                );
            }
            assert_eq!(copied(window, cx), expected);
            cx.write_to_clipboard(ClipboardItem::new_string("menu sentinel".into()));
            window.dispatch_action(Box::new(Copy), cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), expected);
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |v, cx| {
                v.model.transcript.items[39].body = ItemBody::AgentMessage {
                    text: "unrelated tail replacement".into(),
                };
                v.sync_list(
                    &Applied {
                        touched: [39].into_iter().collect(),
                        ..Default::default()
                    },
                    cx,
                );
            });
            window.render_frame(cx);
            assert_eq!(
                copied(window, cx),
                expected,
                "unrelated tail updates must preserve stable partial endpoints"
            );
            // A touched unmounted middle row invalidates before another paint.
            view.update(cx, |v, cx| {
                v.model.transcript.items[10].body = ItemBody::AgentMessage {
                    text: "different middle".into(),
                };
                v.sync_list(
                    &Applied {
                        touched: [10].into_iter().collect(),
                        ..Default::default()
                    },
                    cx,
                );
            });
            assert!(TextSelection::selected_text(window, cx).is_empty());
        })
        .unwrap();
        assert!(recording.try_recv().is_err());
    }
}

#[gpui::test]
fn chat_base_menu_choice_has_radio_semantics_and_records_one_configuration(
    cx: &mut TestAppContext,
) {
    let (handle, view, recording) = editor_tests::mount_selection(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        install(&view, vec![], window, cx);
        window.click("chat-mode", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("chat-mode").expanded(), Some(true));
        let choice = window.find("mode-Supervised");
        assert_eq!(choice.role(), Some(gpui::Role::MenuItemRadio));
        assert_eq!(choice.checked(), Some(true));
        assert_eq!(choice.label(), Some("Supervised"));
        window.click("mode-Plan", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert!(matches!(
        recording.try_recv().unwrap(),
        feed::Delivery::Command(ChatCommand::Configure {
            model: None,
            effort: None,
            approval_mode: Some(ApprovalMode::Plan),
            fast: None
        })
    ));
    assert!(
        recording.try_recv().is_err(),
        "one Base activation records one domain command"
    );
    assert!(view.read_with(cx, |v, _| v.menu.is_none()));
}
