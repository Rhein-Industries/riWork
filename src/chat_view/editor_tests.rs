//! Deferred under incident hold: actual ChatView + Kit event/draft integration.
//! Recording Feed uses no socket, child, provider, credentials, or host startup.
use super::*;
use crate::chat::{
    client::CallError,
    model::{ChatCommand, QuestionPrompt},
};
use gpui::{
    Bounds, ClipboardEntry, ClipboardItem, ElementInputHandler, ExternalPaths, InputHandler,
    TestAppContext, WindowBounds, WindowHandle, WindowOptions, point, size,
};
use gpui_kit::test::TestWindowExt;
use std::sync::mpsc::Receiver;

pub(super) fn mount(
    cx: &mut TestAppContext,
) -> (
    WindowHandle<gpui_kit::base::Root>,
    Entity<ChatView>,
    Receiver<feed::Delivery>,
) {
    mount_config(
        cx,
        HostConfig {
            ensure: Arc::new(|| Err("fixture staging is disabled".into())),
        },
    )
}

pub(super) fn mount_selection(
    cx: &mut TestAppContext,
) -> (
    WindowHandle<gpui_kit::base::Root>,
    Entity<ChatView>,
    Receiver<feed::Delivery>,
) {
    let mounted = mount_config(
        cx,
        HostConfig {
            ensure: Arc::new(|| panic!("recording-only selection fixture reached host staging")),
        },
    );
    // Selection-only cases start in transcript focus. Cases exercising an
    // editor-to-transcript transfer click the actual editor and yield between
    // native pointer input and Copy, allowing Base's deferred focus effect.
    cx.update_window(mounted.0.into(), |_, window, cx| {
        mounted.1.read(cx).focus.clone().focus(window, cx);
    })
    .unwrap();
    cx.run_until_parked();
    mounted
}

pub(super) fn mount_config(
    cx: &mut TestAppContext,
    config: HostConfig,
) -> (
    WindowHandle<gpui_kit::base::Root>,
    Entity<ChatView>,
    Receiver<feed::Delivery>,
) {
    let (handle, view, recording) = cx.update(|cx| {
        cx.set_global(crate::settings::Settings::default());
        cx.set_global(crate::theme::Appearance {
            selected: crate::theme::ThemeChoice::RiWork,
            palette: crate::theme::Palette::RIWORK,
            terminal: None,
            ghostty: None,
            error: None,
        });
        text_input::init(cx);
        let (recording_feed, recording) = Feed::recording();
        let mut chat = None;
        let handle = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.), px(0.)),
                        size(px(700.), px(800.)),
                    ))),
                    ..Default::default()
                },
                |window, cx| {
                    let view = cx.new(|cx| {
                        let mut view = ChatView::blank(config, window, cx);
                        view.chat_id = Some("fixture-chat".into());
                        view.feed = Some(recording_feed);
                        view
                    });
                    chat = Some(view.clone());
                    cx.new(|cx| gpui_kit::base::Root::new(view, window, cx))
                },
            )
            .unwrap();
        let view = chat.unwrap();
        (handle, view, recording)
    });
    cx.update_window(handle.into(), |_, window, _| window.activate_window())
        .unwrap();
    cx.run_until_parked();
    (handle, view, recording)
}
fn next(recording: &Receiver<feed::Delivery>) -> (u64, ChatCommand) {
    match recording.try_recv().unwrap() {
        feed::Delivery::Submission { id, command } => (id, command),
        _ => panic!("composer must use a correlated single-attempt delivery"),
    }
}

#[gpui::test]
fn typed_model_enter_configures_once_and_menu_refresh_keeps_editor(cx: &mut TestAppContext) {
    let (handle, view, recording) = mount(cx);
    let identity = view.read_with(cx, |v, _| v.model_input.entity_id());
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |v, cx| v.toggle_menu(Menu::Model, window, cx))
    })
    .unwrap();
    cx.update_window(handle.into(), |_, window, cx| {
        window.click("chat-model-input", cx);
        window.input("typed-model", cx);
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert!(
        matches!(recording.try_recv().unwrap(), feed::Delivery::Command(ChatCommand::Configure { model: Some(model), .. }) if model == "typed-model")
    );
    assert!(recording.try_recv().is_err());
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |v, cx| {
            v.toggle_menu(Menu::Model, window, cx);
            assert_eq!(v.model_input.entity_id(), identity);
            assert_eq!(v.model_input.read(cx).value(), "typed-model");
        })
    })
    .unwrap();
}

#[gpui::test]
fn attachment_only_draft_is_a_send_and_never_an_empty_draft_approval(cx: &mut TestAppContext) {
    use crate::chat::{
        attachments::{Attachment, AttachmentKind, Preview},
        model::{Approval, ApprovalKind},
    };
    let (handle, view, recording) = mount(cx);
    let snapshot = Attachment {
        id: "owned-snapshot".into(),
        name: "one.txt".into(),
        path: "/private/fixture/owned".into(),
        kind: AttachmentKind::Text,
        bytes: 5,
        fingerprint: "fixture".into(),
        preview: Preview::Text {
            excerpt: "owned".into(),
        },
    };
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |v, cx| {
            v.model.transcript.approvals = vec![Approval {
                request_id: "approval".into(),
                item_id: None,
                kind: ApprovalKind::Command,
                title: "approve?".into(),
                detail: String::new(),
                choices: vec![Decision::Accept, Decision::Decline],
            }];
            v.attachments
                .push(attachment_ui::Chip::ready(snapshot.clone()));
            v.bump_generation();
            assert!(!v.draft_empty(cx));
            v.focus(window, cx);
        })
    })
    .unwrap();
    cx.update_window(handle.into(), |_, window, cx| window.press("enter", cx))
        .unwrap();
    cx.run_until_parked();
    let (_, command) = next(&recording);
    assert_eq!(
        command,
        ChatCommand::SendAttachments {
            text: String::new(),
            attachments: vec![snapshot]
        }
    );
    assert!(recording.try_recv().is_err());
    assert!(!view.read_with(cx, |v, _| v.answered.contains("approval")));
}

#[gpui::test]
fn actual_composer_action_and_newer_equal_draft_receipt(cx: &mut TestAppContext) {
    let (handle, view, recording) = mount(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.click("chat-composer", cx);
        window.input("A🦀Z", cx);
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    let (id, command) = next(&recording);
    assert_eq!(
        command,
        ChatCommand::Send {
            text: "A🦀Z".into()
        }
    );
    assert!(
        recording.try_recv().is_err(),
        "one action produces one dispatch"
    );
    let identity = view.read_with(cx, |v, _| v.composer.entity_id());
    cx.update_window(handle.into(), |_, window, cx| {
        window.press("cmd-a", cx);
        window.input("newer", cx);
        window.press("cmd-a", cx);
        window.input("A🦀Z", cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |v, cx| {
            assert_eq!(v.composer.entity_id(), identity);
            assert!(v.editor_generation > v.submissions[0].generation);
            v.submission_receipt(id + 100, command.clone(), Ok(()), window, cx);
            assert_eq!(v.pending_submission, Some(id));
            v.submission_receipt(id, command.clone(), Ok(()), window, cx);
            assert_eq!(
                v.composer_text(cx),
                "A🦀Z",
                "equal content with newer edits is retained"
            );
            assert_eq!(v.pending_submission, None);
        })
    })
    .unwrap();
}

#[gpui::test]
fn exact_success_clears_but_uncertainty_keeps_snapshot_and_explicit_resend(
    cx: &mut TestAppContext,
) {
    let (handle, view, recording) = mount(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.click("chat-composer", cx);
        window.input("exact", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |v, cx| v.send_message(window, cx))
    })
    .unwrap();
    let (id, command) = next(&recording);
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |v, cx| {
            v.submission_receipt(id, command.clone(), Ok(()), window, cx);
            assert!(v.draft_empty(cx));
        })
    })
    .unwrap();
    cx.update_window(handle.into(), |_, window, cx| {
        window.input("  exact failed 🦀\n", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |v, cx| v.send_message(window, cx))
    })
    .unwrap();
    let (id, command) = next(&recording);
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |v, cx| {
            v.submission_receipt(
                id,
                command.clone(),
                Err(CallError::Broken("receipt lost".into())),
                window,
                cx,
            );
            let exact = v.composer_text(cx);
            assert_eq!(v.submissions[0].snapshot.text, exact);
            v.submission_receipt(id, command.clone(), Ok(()), window, cx);
            v.send_message(window, cx);
            assert_eq!(v.composer_text(cx), exact);
            assert!(
                recording.try_recv().is_err(),
                "ambiguous dispatch is never blindly retried"
            );
            v.resend_snapshot(id, window, cx);
        })
    })
    .unwrap();
    let (_, retry) = next(&recording);
    assert_eq!(retry, command);
}

#[gpui::test]
fn actual_ime_enter_and_alt_newline_stay_in_kit(cx: &mut TestAppContext) {
    let (handle, view, recording) = mount(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.click("chat-composer", cx);
        window.input("A", cx);
        window.press("alt-enter", cx);
        let composer = view.read(cx).composer.clone();
        assert_eq!(composer.read(cx).value(), "A\n");
        let mut handler = ElementInputHandler::new(window.find("chat-composer").bounds(), composer);
        handler.replace_and_mark_text_in_range(None, "中", Some(1..1), window, cx);
        window.render_frame(cx);
        window.press("enter", cx);
        assert!(handler.marked_text_range(window, cx).is_some());
    })
    .unwrap();
    cx.run_until_parked();
    assert!(recording.try_recv().is_err());
}

#[gpui::test]
fn request_refresh_preserves_entities_and_failed_answers_new_request_does_not_inherit(
    cx: &mut TestAppContext,
) {
    let (handle, view, recording) = mount(cx);
    let question = Question {
        request_id: "q1".into(),
        questions: vec![QuestionPrompt {
            header: None,
            question: "Which path?".into(),
            options: vec![],
            multi_select: false,
        }],
    };
    let key = editors::AnswerKey::of(&question, 0);
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |v, cx| {
            v.model.transcript.questions = vec![question.clone()];
            v.ready_inputs(window, cx);
            v.answers[&key]
                .state
                .update(cx, |s, cx| s.replace_all("/exact/🦀", window, cx));
        })
    })
    .unwrap();
    cx.run_until_parked();
    let identity = view.read_with(cx, |v, _| v.answers[&key].state.entity_id());
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |v, cx| {
            v.ready_inputs(window, cx);
            assert_eq!(v.answers[&key].state.entity_id(), identity);
            v.answers[&key]
                .state
                .read(cx)
                .focus_handle(cx)
                .focus(window, cx);
        })
    })
    .unwrap();
    cx.update_window(handle.into(), |_, window, cx| window.press("enter", cx))
        .unwrap();
    cx.run_until_parked();
    let (id, command) = next(&recording);
    assert_eq!(
        command,
        ChatCommand::Answer {
            request_id: "q1".into(),
            answers: vec![vec!["/exact/🦀".into()]]
        }
    );
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |v, cx| {
            v.submission_receipt(
                id,
                command,
                Err(CallError::Refused("still waiting".into())),
                window,
                cx,
            );
            v.ready_inputs(window, cx);
            assert_eq!(v.typed_answers(&question, cx), vec!["/exact/🦀"]);
            let mut next = question.clone();
            next.request_id = "q2".into();
            v.model.transcript.questions = vec![next.clone()];
            v.ready_inputs(window, cx);
            assert_eq!(v.typed_answers(&next, cx), vec![""]);
            assert_ne!(
                v.answers[&editors::AnswerKey::of(&next, 0)]
                    .state
                    .entity_id(),
                identity
            );
        })
    })
    .unwrap();
}

#[gpui::test]
fn native_mixed_file_paste_consumes_once_and_preserves_every_filename(cx: &mut TestAppContext) {
    let (handle, view, _) = mount(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.click("chat-composer", cx);
        cx.write_to_clipboard(ClipboardItem::new_string("plain text".into()));
        window.press("cmd-v", cx);
        let mut entries = ClipboardItem::new_string("filename text".into()).entries;
        entries.push(ClipboardEntry::ExternalPaths(ExternalPaths(
            vec!["/fixture/one.txt".into(), "/fixture/two.png".into()].into(),
        )));
        cx.write_to_clipboard(ClipboardItem { entries });
        window.press("cmd-v", cx);
        assert_eq!(view.read(cx).composer.read(cx).value(), "plain text");
        assert_eq!(
            view.read(cx)
                .attachments
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            vec!["one.txt", "two.png"]
        );
        assert!(!view.read(cx).draft_empty(cx));
    })
    .unwrap();
    cx.run_until_parked();
    view.read_with(cx, |v, _| {
        assert_eq!(v.attachments.len(), 2);
        for chip in &v.attachments {
            assert!(
                matches!(&chip.state, attachment_ui::Stage::Failed(error) if !error.is_empty())
            );
        }
    });
}

#[gpui::test]
fn real_editor_dictation_partial_final_cancel_and_user_edit_anchor(cx: &mut TestAppContext) {
    use crate::dictation::Event;
    let (handle, view, _) = mount(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.click("chat-composer", cx);
        window.input("base ", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |v, cx| {
            v.dictation_fixture(Event::Start, window, cx);
            v.dictation_fixture(Event::Ready, window, cx);
            v.dictation_fixture(Event::Heard("partial".into()), window, cx);
        })
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |v, cx| {
            assert_eq!(v.composer_text(cx), "base partial");
            v.dictation_fixture(Event::Finished("final".into()), window, cx);
        })
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |v, cx| {
            assert_eq!(v.composer_text(cx), "base final");
            v.dictation_fixture(Event::Start, window, cx);
            v.dictation_fixture(Event::Ready, window, cx);
            v.dictation_fixture(Event::Heard("temporary".into()), window, cx);
        })
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.press("cmd-a", cx);
        window.input("user edit", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |v, cx| {
            v.dictation_fixture(Event::Heard("new words".into()), window, cx);
        })
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |v, cx| {
            v.dictation_fixture(Event::Cancel, window, cx);
            assert_eq!(v.composer_text(cx), "user edit");
        })
    })
    .unwrap();
}

/// The message box bar as drawn in a pane `width` px wide, with the mic on and a turn running:
/// the box's and every shown button's bounds, from the frame itself.
fn composer_frame(
    cx: &mut TestAppContext,
    width: f32,
    native: bool,
    scale: f32,
) -> (
    gpui::Bounds<gpui::Pixels>,
    Vec<(&'static str, gpui::Bounds<gpui::Pixels>)>,
    super::composer::Layout,
) {
    let face = if native {
        crate::ui_text::Face::System
    } else {
        crate::ui_text::Face::Menlo
    };
    let before = crate::ui_text::set_for_tests(scale, face);
    let handle = cx.update(|cx| {
        cx.set_global(crate::settings::Settings {
            dictation_mic: true,
            ..Default::default()
        });
        cx.set_global(crate::theme::Appearance {
            selected: crate::theme::ThemeChoice::RiWork,
            palette: crate::theme::Palette::RIWORK,
            terminal: None,
            ghostty: None,
            error: None,
        });
        text_input::init(cx);
        let (recording_feed, _recording) = Feed::recording();
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(width), px(600.)),
                ))),
                ..Default::default()
            },
            |window, cx| {
                cx.new(|cx| {
                    let mut view = ChatView::blank(
                        HostConfig {
                            ensure: Arc::new(|| Err("fixture staging is disabled".into())),
                        },
                        window,
                        cx,
                    );
                    view.chat_id = Some("fixture-chat".into());
                    view.feed = Some(recording_feed);
                    view.model.transcript.state = crate::chat::model::ChatState::Running;
                    view
                })
            },
        )
        .unwrap()
    });
    // The first frame measures the pane; the second lays the bar out for it.
    for _ in 0..3 {
        cx.update_window(handle.into(), |_, window, cx| window.render_frame(cx))
            .unwrap();
        cx.run_until_parked();
    }
    let mut visual = gpui::VisualTestContext::from_window(handle.into(), cx);
    let pane = visual
        .debug_bounds("composer-pane")
        .expect("the bar is drawn");
    assert_eq!(f32::from(pane.size.width), width, "the bar spans the pane");
    let field = visual
        .debug_bounds("composer-field")
        .expect("the box is drawn");
    let buttons = [
        "composer-attach",
        "composer-mic",
        "composer-stop",
        "composer-send",
    ]
    .into_iter()
    .filter_map(|name| visual.debug_bounds(name).map(|bounds| (name, bounds)))
    .collect();
    let layout = super::composer::layout(width, scale, true, false, true);
    crate::ui_text::set_for_tests(before.0, before.1);
    (field, buttons, layout)
}

#[gpui::test]
fn the_drawn_composer_keeps_its_box_and_buttons_in_any_pane(cx: &mut TestAppContext) {
    let big = 24.0 / crate::ui_text::REFERENCE_SIZE;
    for native in [true, false] {
        let mut modes = std::collections::BTreeSet::new();
        for scale in [1.0, 1.5, big] {
            for width in [160.0, 240.0, 300.0, 368.0, 600.0] {
                let what = format!("{width} px, {scale}×, native {native}");
                let (field, buttons, layout) = composer_frame(cx, width, native, scale);
                modes.insert((layout.stacked, layout.mic));
                let min = (super::composer::MIN_FIELD * scale).round();
                let floor = min.min(width - 2.0 * super::composer::COMPACT_INSET);
                let box_width = f32::from(field.size.width);
                assert!(
                    box_width >= floor - 0.5,
                    "box {box_width} < {floor}: {what}"
                );
                assert!(
                    (box_width - layout.field).abs() <= 1.0,
                    "drawn box {box_width} vs planned {}: {what}",
                    layout.field
                );
                // Attach, Stop and Send always; the mic as planned.
                let names: Vec<_> = buttons.iter().map(|(name, _)| *name).collect();
                assert!(names.contains(&"composer-attach"), "{what}");
                assert!(names.contains(&"composer-stop"), "{what}");
                assert!(names.contains(&"composer-send"), "{what}");
                assert_eq!(names.contains(&"composer-mic"), layout.mic, "{what}");
                for (name, bounds) in &buttons {
                    let (left, right) = (f32::from(bounds.left()), f32::from(bounds.right()));
                    assert!(
                        (f32::from(bounds.size.width) - layout.button).abs() <= 0.5,
                        "{name} is {} wide, planned {}: {what}",
                        bounds.size.width,
                        layout.button
                    );
                    assert!(
                        left >= -0.5 && right <= width + 0.5,
                        "{name} {left}..{right}: {what}"
                    );
                    // Stacked buttons sit under the box; inline ones beside it.
                    if layout.stacked {
                        assert!(
                            bounds.top() >= field.bottom(),
                            "{name} under the box: {what}"
                        );
                    } else {
                        assert!(
                            right <= f32::from(field.left()) + 0.5
                                || left >= f32::from(field.right()) - 0.5,
                            "{name} beside the box: {what}"
                        );
                    }
                }
            }
        }
        // Every way of laying it out was drawn: beside the box with and without the mic,
        // and under it.
        assert!(modes.contains(&(false, true)), "native {native}: {modes:?}");
        assert!(
            modes.contains(&(false, false)),
            "native {native}: {modes:?}"
        );
        assert!(
            modes.iter().any(|(stacked, _)| *stacked),
            "native {native}: {modes:?}"
        );
    }
}

#[gpui::test]
fn the_display_pill_is_kit_toggles_as_tall_as_the_header_buttons(cx: &mut TestAppContext) {
    // Not clicked here: choosing a mode remembers it for the chat in the settings file.
    let (handle, view, recording) = mount(cx);
    view.update(cx, |view, cx| {
        view.model.link = super::state::Link::Live;
        cx.notify();
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let normal = window.find("chat-display-normal");
        let verbose = window.find("chat-display-verbose");
        let picker = window.find("chat-mode");
        for (segment, name, pressed) in [(&normal, "Normal", true), (&verbose, "Verbose", false)] {
            assert!(segment.visible());
            assert_eq!(segment.role(), Some(gpui::Role::Button));
            assert_eq!(segment.label(), Some(name));
            assert_eq!(segment.checked(), Some(pressed), "{name} pressed");
            assert_eq!(
                segment.bounds().size.height,
                picker.bounds().size.height,
                "{name} is as tall as the picker beside it"
            );
        }
        // One continuous track: the segments touch.
        assert_eq!(normal.bounds().right(), verbose.bounds().left());
    })
    .unwrap();
    assert!(recording.try_recv().is_err());
}
