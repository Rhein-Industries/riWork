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
        started_at: None,
        ended_at: None,
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
        // The activity header is crossed visually, but decorative headers are
        // excluded from the text that the drag copies.
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
fn unrelated_activity_ticks_and_output_deltas_preserve_selected_answer(cx: &mut TestAppContext) {
    let (handle, view, recording) = editor_tests::mount_selection(cx);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    cx.update_window(handle.into(), |_, window, cx| {
        let mut running = Item {
            id: "running-call".into(),
            turn_id: Some("running-turn".into()),
            status: ItemStatus::InProgress,
            started_at: Some(now),
            ended_at: None,
            presentation: Presentation::default(),
            body: ItemBody::Command {
                command: "cargo test".into(),
                cwd: None,
                output: "Starting tests\n".into(),
                exit_code: None,
            },
        };
        // Keep a live header separate from the completed answer being selected.
        running.presentation.semantics = Some(crate::chat::model::Semantics {
            kind: crate::chat::model::ToolKind::Shell,
            ..Default::default()
        });
        install(
            &view,
            vec![message("stable-answer", "A completed answer 🦀"), running],
            window,
            cx,
        );
        view.update(cx, |view, cx| {
            view.model.transcript.turn_id = Some("running-turn".into());
            view.model.transcript.turn_timing.insert(
                "running-turn".into(),
                crate::chat::model::TurnTiming {
                    started_at: Some(now),
                    ended_at: None,
                },
            );
            view.refresh_projection(cx);
        });
        window.render_frame(cx);
        select_between(
            window,
            "transcript:stable-answer/0",
            "transcript:stable-answer/0",
            cx,
        );
        assert_eq!(copied(window, cx), "A completed answer 🦀");
        let previous_title = view
            .read(cx)
            .visible
            .iter()
            .find(|row| row.id == "activity:running-turn")
            .unwrap()
            .title
            .clone();
        view.update(cx, |view, cx| view.tick_activity(Some(now + 18_000), cx));
        let elapsed_title = &view
            .read(cx)
            .visible
            .iter()
            .find(|row| row.id == "activity:running-turn")
            .unwrap()
            .title;
        assert_ne!(
            &previous_title, elapsed_title,
            "the tick must really update the unrelated activity"
        );
        assert!(elapsed_title.contains("18s"));
        assert_eq!(
            copied(window, cx),
            "A completed answer 🦀",
            "timer ticks must preserve the selected answer"
        );
        view.update(cx, |view, cx| {
            if let ItemBody::Command { output, .. } = &mut view.model.transcript.items[1].body {
                output.push_str("test first_case ... ok\n");
            }
            view.sync_list(
                &Applied {
                    touched: [1].into_iter().collect(),
                    projection_changed: false,
                    ..Default::default()
                },
                cx,
            );
        });
        window.render_frame(cx);
        assert_eq!(
            copied(window, cx),
            "A completed answer 🦀",
            "unrelated streaming output must preserve the selected answer"
        );
    })
    .unwrap();
    assert!(recording.try_recv().is_err());
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
            // This fixture measures message virtualization, with one row per message.
            // Normal's per-turn activity headers have their own export test above.
            view.update(cx, |v, cx| {
                v.pending_display = Some(DisplayMode::Verbose);
                v.apply_display_mode(DisplayMode::Verbose, cx);
            });
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
            fast: None,
            contributor_mode: None,
            contributor_opt_out: None,
            claude_billing: None,
        })
    ));
    assert!(
        recording.try_recv().is_err(),
        "one Base activation records one domain command"
    );
    assert!(view.read_with(cx, |v, _| v.menu.is_none()));
}

#[gpui::test]
fn output_drag_copy_excludes_synthetic_omissions_and_waiting_placeholders(cx: &mut TestAppContext) {
    let (handle, view, _) = editor_tests::mount_selection(cx);
    let make = |id: &str, status, body| Item {
        id: id.into(),
        turn_id: Some(format!("turn:{id}")),
        status,
        started_at: None,
        ended_at: None,
        presentation: Presentation::default(),
        body,
    };
    let output = (1..=16)
        .map(|line| {
            if line == 2 {
                "… 1 line omitted\n".into()
            } else {
                format!("line {line}\n")
            }
        })
        .collect::<String>();
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |view, cx| {
            view.pending_display = Some(DisplayMode::Verbose);
            view.apply_display_mode(DisplayMode::Verbose, cx);
            for id in ["cut", "waiting-tool", "empty-tool", "literal"] {
                view.disclosure.overrides.insert(id.into(), true);
            }
        });
        install(
            &view,
            vec![
                make(
                    "cut",
                    ItemStatus::Completed,
                    ItemBody::Command {
                        command: "printf fixture".into(),
                        cwd: None,
                        output: output.clone(),
                        exit_code: Some(0),
                    },
                ),
                make(
                    "waiting-tool",
                    ItemStatus::InProgress,
                    ItemBody::ToolCall {
                        server: None,
                        tool: "fixture_wait".into(),
                        input: serde_json::Value::Null,
                        output: None,
                    },
                ),
                make(
                    "empty-tool",
                    ItemStatus::Completed,
                    ItemBody::ToolCall {
                        server: None,
                        tool: "fixture_empty".into(),
                        input: serde_json::Value::Null,
                        output: Some(String::new()),
                    },
                ),
                make(
                    "literal",
                    ItemStatus::Completed,
                    ItemBody::Command {
                        command: "printf literal".into(),
                        cwd: None,
                        output: "Waiting for output…".into(),
                        exit_code: Some(0),
                    },
                ),
            ],
            window,
            cx,
        );
        assert!(window.find("output-line:output:cut:3").visible());
        assert!(window.try_find("transcript:output:cut:3").is_none());
        assert!(
            window
                .find("output-placeholder:tool-output:waiting-tool")
                .visible()
        );
        assert!(
            window
                .try_find("transcript:tool-output:waiting-tool:0")
                .is_none()
        );
        assert!(
            window
                .try_find("transcript:tool-output:empty-tool:0")
                .is_none()
        );
        assert_eq!(
            window.find("transcript:output:literal:run:0").label(),
            Some("Waiting for output…")
        );
        view.read_with(cx, |view, cx| {
            let leaves = super::selection_document::leaves(view, Look::of(cx));
            assert!(!leaves.iter().any(|leaf| leaf.key == "output:cut:3"));
            assert!(
                !leaves
                    .iter()
                    .any(|leaf| leaf.key.starts_with("tool-output:waiting-tool:"))
            );
            assert!(
                !leaves
                    .iter()
                    .any(|leaf| leaf.key.starts_with("tool-output:empty-tool:"))
            );
        });
        select_glyph_span(&view, window, "output:cut:run:0", "output:cut:run:4", cx);
        let expected = ["line 1", "… 1 line omitted", "line 3"]
            .into_iter()
            .map(str::to_owned)
            .chain((5..=16).map(|line| format!("line {line}")))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(copied(window, cx), expected);
    })
    .unwrap();
}

#[gpui::test]
fn row_headers_toggle_without_selecting_and_open_container_text_remains_selectable(
    cx: &mut TestAppContext,
) {
    struct RestoreFace((f32, ui_text::Face));
    impl Drop for RestoreFace {
        fn drop(&mut self) {
            ui_text::set_for_tests(self.0.0, self.0.1);
        }
    }
    let _restore = RestoreFace(ui_text::set_for_tests(1., ui_text::Face::System));
    let (handle, view, recording) = editor_tests::mount_selection(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |view, _| view.display_mode = DisplayMode::Verbose);
        install(
            &view,
            vec![Item {
                id: "header-interaction".into(),
                turn_id: Some("interaction-turn".into()),
                status: ItemStatus::Completed,
                started_at: None,
                ended_at: None,
                presentation: Presentation::default(),
                body: ItemBody::Command {
                    command: "printf interaction alpha beta gamma".into(),
                    cwd: None,
                    output: "alpha output\nbeta output\nlast output".into(),
                    exit_code: Some(0),
                },
            }],
            window,
            cx,
        );
        let is_open = |cx: &gpui::App| {
            display::find(&view.read(cx).visible, "header-interaction")
                .unwrap()
                .open
        };
        assert!(!is_open(cx));
        let bounds = window.find("card:header-interaction").bounds();
        let title = point(rows::text_indent() + px(3.), bounds.size.height / 2.);
        let whitespace = point(bounds.size.width - px(12.), bounds.size.height / 2.);
        for (area, offset) in [("title", title), ("whitespace", whitespace)] {
            for expected_open in [true, false] {
                window.click_at("card:header-interaction", offset, cx);
                assert_eq!(
                    is_open(cx),
                    expected_open,
                    "clicking {area} toggles immediately"
                );
            }
        }
        window.drag(
            point(bounds.left() + px(62.), bounds.center().y),
            point(
                (bounds.left() + px(240.)).min(bounds.right() - px(12.)),
                bounds.center().y,
            ),
            cx,
        );
        assert!(TextSelection::selected_text(window, cx).is_empty());
        assert_eq!(
            window.find("card:header-interaction").role(),
            Some(gpui::Role::Button)
        );
        assert!(
            window
                .find("card:header-interaction")
                .label()
                .is_some_and(|name| name.contains("printf interaction"))
        );
        if !is_open(cx) {
            window.click("card:header-interaction", cx);
        }
        for target in [
            "output-header:header-interaction",
            "transcript:output:header-interaction:run:0",
            "output-block:header-interaction",
        ] {
            window.click(target, cx);
            assert!(is_open(cx), "clicking {target} cannot close the row");
        }
        select_glyph_span(
            &view,
            window,
            "output:header-interaction:run:0",
            "output:header-interaction:run:0",
            cx,
        );
        assert!(is_open(cx));
        assert_eq!(copied(window, cx), "alpha output\nbeta output\nlast output");
    })
    .unwrap();
    assert!(recording.try_recv().is_err());
}
