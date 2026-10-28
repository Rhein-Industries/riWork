//! Deferred under incident hold: actual ChatView + Kit event/draft integration.
//! Recording Feed uses no socket, child, provider, credentials, or host startup.
use super::*;
use crate::chat::{
    client::CallError,
    model::{ChatCommand, QuestionPrompt},
};
use gpui::{Bounds, TestAppContext, WindowBounds, WindowHandle, WindowOptions, point, size};
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
    mount_sized(cx, config, 700.)
}

/// `mount_config` in a window `width` px wide.
pub(super) fn mount_sized(
    cx: &mut TestAppContext,
    config: HostConfig,
    width: f32,
) -> (
    WindowHandle<gpui_kit::base::Root>,
    Entity<ChatView>,
    Receiver<feed::Delivery>,
) {
    let (handle, view, recording) = cx.update(|cx| {
        cx.set_global(crate::settings::Settings::default());
        cx.set_global(crate::theme::Appearance {
            selected: crate::theme::ThemeChoice::Native,
            palette: crate::theme::Palette::NATIVE_LIGHT,
            terminal: None,
            ghostty: None,
        });
        text_input::init(cx);
        let (recording_feed, recording) = Feed::recording();
        let mut chat = None;
        let handle = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.), px(0.)),
                        size(px(width), px(800.)),
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
fn request_refresh_preserves_entities_and_failed_answers_new_request_does_not_inherit(
    cx: &mut TestAppContext,
) {
    let (handle, view, recording) = mount(cx);
    let question = Question {
        item_id: None,
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
            v.dictation_fixture(Event::Heard("final".into()), window, cx);
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

// These are GPUI's in-memory test windows and virtual clock, never native desktop input.
fn speech_event(
    cx: &mut TestAppContext,
    handle: WindowHandle<gpui_kit::base::Root>,
    view: &Entity<ChatView>,
    event: crate::dictation::Event,
) {
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |v, cx| {
            // Both real engines report their latest hypothesis before the final event.
            if let crate::dictation::Event::Finished(text) = &event {
                v.dictation_fixture(crate::dictation::Event::Heard(text.clone()), window, cx);
            }
            v.dictation_fixture(event, window, cx);
        });
    })
    .unwrap();
    cx.run_until_parked();
}

fn composer_gesture(
    cx: &mut TestAppContext,
    handle: WindowHandle<gpui_kit::base::Root>,
    gesture: &str,
) {
    cx.update_window(handle.into(), |_, window, cx| {
        if gesture == "send" {
            window.click("chat-send", cx);
        } else {
            window.press(gesture, cx);
        }
    })
    .unwrap();
    cx.run_until_parked();
}

fn start_speech(
    cx: &mut TestAppContext,
    handle: WindowHandle<gpui_kit::base::Root>,
    view: &Entity<ChatView>,
    typed: &str,
    heard: &str,
) {
    use crate::dictation::Event;
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |v, cx| v.focus(window, cx));
        window.input(typed, cx);
    })
    .unwrap();
    cx.run_until_parked();
    speech_event(cx, handle, view, Event::Start);
    speech_event(cx, handle, view, Event::Ready);
    speech_event(cx, handle, view, Event::Heard(heard.into()));
}

#[gpui::test]
fn dictation_send_awaits_early_late_or_missing_final_and_sends_once(cx: &mut TestAppContext) {
    use crate::dictation::{Event, STOPPING_DEADLINE};
    for gesture in ["enter", "cmd-enter", "send"] {
        for timing in ["early", "late", "never"] {
            let (handle, view, recording) = mount(cx);
            start_speech(cx, handle, &view, "typed ", "first words");
            if timing == "early" {
                speech_event(
                    cx,
                    handle,
                    &view,
                    Event::Finished("first words last word".into()),
                );
            }
            composer_gesture(cx, handle, gesture);
            // Double Return, or Send+Return, is still one intent.
            composer_gesture(cx, handle, "enter");
            if timing != "early" {
                assert!(
                    recording.try_recv().is_err(),
                    "{gesture} {timing}: sent before final"
                );
                cx.executor()
                    .advance_clock(STOPPING_DEADLINE - Duration::from_millis(1));
                cx.run_until_parked();
                assert!(recording.try_recv().is_err());
                speech_event(
                    cx,
                    handle,
                    &view,
                    Event::Heard(
                        if timing == "late" {
                            "first words last"
                        } else {
                            "first words last word"
                        }
                        .into(),
                    ),
                );
                if timing == "late" {
                    speech_event(
                        cx,
                        handle,
                        &view,
                        Event::Finished("first words last word".into()),
                    );
                } else {
                    cx.executor().advance_clock(Duration::from_millis(1));
                    cx.run_until_parked();
                }
            }
            let (id, command) = next(&recording);
            assert_eq!(
                command,
                ChatCommand::Send {
                    text: "typed first words last word".into()
                },
                "{gesture} {timing}"
            );
            assert!(recording.try_recv().is_err());
            // A receipt clears the final draft. Late engine output cannot restore or resend it.
            cx.update_window(handle.into(), |_, window, cx| {
                view.update(cx, |v, cx| {
                    v.submission_receipt(id, command, Ok(()), window, cx)
                });
            })
            .unwrap();
            cx.run_until_parked();
            speech_event(cx, handle, &view, Event::Finished("late duplicate".into()));
            cx.executor().advance_clock(STOPPING_DEADLINE);
            cx.run_until_parked();
            assert_eq!(
                view.read_with(cx, |v, cx| v.composer_text(cx)),
                "",
                "{gesture} {timing}: final draft not cleared"
            );
            composer_gesture(cx, handle, "enter");
            assert!(
                recording.try_recv().is_err(),
                "{gesture} {timing}: duplicate send"
            );
        }
    }
}

#[gpui::test]
fn dictation_escape_keeps_typed_and_volatile_words_without_sending(cx: &mut TestAppContext) {
    use crate::dictation::{Event, Phase, STOPPING_DEADLINE};
    for timing in ["early", "late", "never", "empty-final"] {
        let (handle, view, recording) = mount(cx);
        start_speech(cx, handle, &view, "typed ", "volatile words");
        cx.update_window(handle.into(), |_, window, cx| {
            window.input(" plus typing", cx)
        })
        .unwrap();
        cx.run_until_parked();
        if timing == "early" {
            speech_event(cx, handle, &view, Event::Finished("volatile words".into()));
        }
        composer_gesture(cx, handle, "escape");
        if timing != "early" {
            assert!(view.read_with(cx, |v, _| matches!(
                v.dictation.phase(),
                Phase::Finishing { .. }
            )));
        }
        assert_eq!(
            view.read_with(cx, |v, cx| v.composer_text(cx)),
            "typed volatile words plus typing"
        );
        // Repeated Escape never discards the draft or restarts the deadline.
        composer_gesture(cx, handle, "escape");
        if timing == "never" {
            cx.executor().advance_clock(STOPPING_DEADLINE);
            cx.run_until_parked();
        } else if timing != "early" {
            speech_event(
                cx,
                handle,
                &view,
                Event::Finished(
                    if timing == "empty-final" {
                        ""
                    } else {
                        "volatile words."
                    }
                    .into(),
                ),
            );
        }
        assert_eq!(
            view.read_with(cx, |v, cx| v.composer_text(cx)),
            if timing == "late" {
                "typed volatile words. plus typing"
            } else {
                "typed volatile words plus typing"
            }
        );
        assert_eq!(
            view.read_with(cx, |v, _| v.dictation.phase().clone()),
            Phase::Idle
        );
        assert!(recording.try_recv().is_err());
    }
}

#[gpui::test]
fn dictation_empty_send_only_stops_and_escape_without_recording_is_unchanged(
    cx: &mut TestAppContext,
) {
    use crate::{
        chat::model::{Approval, ApprovalKind},
        dictation::{Event, Phase, STOPPING_DEADLINE},
    };
    for gesture in ["enter", "cmd-enter", "send"] {
        for final_arrives in [true, false] {
            let (handle, view, recording) = mount(cx);
            start_speech(cx, handle, &view, "", "");
            composer_gesture(cx, handle, gesture);
            if final_arrives {
                speech_event(cx, handle, &view, Event::Finished("".into()));
            } else {
                cx.executor().advance_clock(STOPPING_DEADLINE);
                cx.run_until_parked();
            }
            assert_eq!(
                view.read_with(cx, |v, _| v.dictation.phase().clone()),
                Phase::Idle
            );
            assert!(recording.try_recv().is_err());
        }
    }
    let (handle, view, recording) = mount(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |v, cx| {
            v.model.transcript.approvals = vec![Approval {
                request_id: "a".into(),
                item_id: None,
                kind: ApprovalKind::Command,
                title: "approve?".into(),
                detail: String::new(),
                choices: vec![Decision::Accept, Decision::Decline],
            }];
            v.focus(window, cx);
        });
        window.input("keep typed draft", cx);
    })
    .unwrap();
    cx.run_until_parked();
    composer_gesture(cx, handle, "escape");
    assert_eq!(
        view.read_with(cx, |v, cx| v.composer_text(cx)),
        "keep typed draft"
    );
    assert!(recording.try_recv().is_err());
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |v, cx| {
            v.composer
                .update(cx, |state, cx| state.set_value("", window, cx))
        });
    })
    .unwrap();
    cx.run_until_parked();
    composer_gesture(cx, handle, "escape");
    assert!(matches!(
        recording.try_recv().unwrap(),
        feed::Delivery::Command(ChatCommand::Approve {
            decision: Decision::Decline,
            ..
        })
    ));
}

fn composer_turn(
    cx: &mut TestAppContext,
    handle: WindowHandle<gpui_kit::base::Root>,
    view: &Entity<ChatView>,
    action: impl FnOnce(&mut ChatView, &mut Window, &mut Context<ChatView>),
) {
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |v, cx| action(v, window, cx));
    })
    .unwrap();
    cx.run_until_parked();
}

#[gpui::test]
fn dictation_typing_then_more_speech_replaces_the_span_during_recording_and_wait(
    cx: &mut TestAppContext,
) {
    use crate::dictation::Event;
    for finishing in [false, true] {
        let (handle, view, recording) = mount(cx);
        start_speech(cx, handle, &view, "typed ", "héllo");
        if finishing {
            composer_gesture(cx, handle, "enter");
        }
        cx.update_window(handle.into(), |_, window, cx| {
            window.input(" plus typing", cx)
        })
        .unwrap();
        cx.run_until_parked();
        speech_event(cx, handle, &view, Event::Heard("héllo world".into()));
        assert_eq!(
            view.read_with(cx, |v, cx| v.composer_text(cx)),
            "typed héllo world plus typing"
        );
        speech_event(
            cx,
            handle,
            &view,
            Event::Finished("héllo world again".into()),
        );
        assert_eq!(
            view.read_with(cx, |v, cx| v.composer_text(cx)),
            "typed héllo world again plus typing"
        );
        if finishing {
            assert_eq!(
                next(&recording).1,
                ChatCommand::Send {
                    text: "typed héllo world again plus typing".into()
                }
            );
        }
        assert!(recording.try_recv().is_err());
    }
}

#[gpui::test]
fn dictation_escape_cancels_a_waiting_send_and_keeps_typing_and_speech(cx: &mut TestAppContext) {
    use crate::dictation::{Event, STOPPING_DEADLINE};
    let (handle, view, recording) = mount(cx);
    start_speech(cx, handle, &view, "typed ", "first");
    composer_gesture(cx, handle, "enter");
    cx.executor().advance_clock(Duration::from_secs(5));
    cx.run_until_parked();
    assert!(
        recording.try_recv().is_err(),
        "the app must leave the helper's full five seconds to finish"
    );
    cx.update_window(handle.into(), |_, window, cx| {
        window.input(" typed during wait", cx)
    })
    .unwrap();
    cx.run_until_parked();
    composer_gesture(cx, handle, "escape");
    speech_event(cx, handle, &view, Event::Finished("first and last".into()));
    cx.executor().advance_clock(STOPPING_DEADLINE);
    cx.run_until_parked();
    assert_eq!(
        view.read_with(cx, |v, cx| v.composer_text(cx)),
        "typed first and last typed during wait"
    );
    assert!(recording.try_recv().is_err());
}

#[gpui::test]
fn dictation_cancel_hidden_mic_and_failure_cancel_waiting_sends(cx: &mut TestAppContext) {
    use crate::dictation::{Event, Phase, Problem, STOPPING_DEADLINE};
    for end in ["cancel", "hidden", "failed"] {
        let (handle, view, recording) = mount(cx);
        start_speech(cx, handle, &view, "typed ", "keep words");
        composer_gesture(cx, handle, "send");
        composer_turn(cx, handle, &view, |v, window, cx| match end {
            "cancel" => v.cancel_dictation(window, cx),
            "hidden" => {
                cx.global_mut::<crate::settings::Settings>().dictation_mic = false;
                v.follow_mic_setting(window, cx);
            }
            _ => v.dictation_fixture(Event::Failed(Problem::Interrupted), window, cx),
        });
        assert!(view.read_with(cx, |v, _| !v.dictation.phase().is_active()));
        if end == "failed" {
            assert_eq!(
                view.read_with(cx, |v, cx| v.composer_text(cx)),
                "typed keep words"
            );
            assert!(view.read_with(cx, |v, _| matches!(v.dictation.phase(), Phase::Failed(_))));
        } else {
            assert_eq!(view.read_with(cx, |v, cx| v.composer_text(cx)), "typed ");
        }
        speech_event(cx, handle, &view, Event::Finished("late words".into()));
        cx.executor().advance_clock(STOPPING_DEADLINE);
        cx.run_until_parked();
        assert!(
            recording.try_recv().is_err(),
            "{end}: a terminated dictation must not auto-send"
        );
    }
}

#[gpui::test]
fn dictation_previous_pending_submission_refuses_another_enter(cx: &mut TestAppContext) {
    use crate::dictation::Event;
    let (handle, view, recording) = mount(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |v, cx| v.focus(window, cx));
        window.input("previous draft", cx);
    })
    .unwrap();
    cx.run_until_parked();
    composer_gesture(cx, handle, "enter");
    let (id, _) = next(&recording);
    start_speech(cx, handle, &view, "", "new words");
    composer_gesture(cx, handle, "enter");
    assert_eq!(view.read_with(cx, |v, _| v.pending_submission), Some(id));
    speech_event(cx, handle, &view, Event::Finished("new words later".into()));
    assert!(recording.try_recv().is_err());
}

#[gpui::test]
fn dictation_ime_owns_enter_and_escape_while_recording(cx: &mut TestAppContext) {
    use crate::dictation::Phase;
    use gpui::{ElementInputHandler, InputHandler};
    let (handle, view, recording) = mount(cx);
    start_speech(cx, handle, &view, "typed ", "words");
    cx.update_window(handle.into(), |_, window, cx| {
        let state = view.read(cx).composer.clone();
        let mut handler = ElementInputHandler::new(window.find("chat-composer").bounds(), state);
        handler.replace_and_mark_text_in_range(None, "日本", Some(2..2), window, cx);
    })
    .unwrap();
    cx.run_until_parked();
    let before = view.read_with(cx, |v, cx| v.composer_text(cx));
    for key in ["enter", "cmd-enter", "escape"] {
        composer_gesture(cx, handle, key);
        assert!(view.read_with(cx, |v, _| matches!(
            v.dictation.phase(),
            Phase::Listening { .. }
        )));
        assert_eq!(view.read_with(cx, |v, cx| v.composer_text(cx)), before);
        assert!(recording.try_recv().is_err());
    }
}

#[gpui::test]
fn dictation_pump_rejects_real_late_events_after_timeout_and_send(cx: &mut TestAppContext) {
    use crate::dictation::{EngineEvent, Event, STOPPING_DEADLINE};
    for final_arrives in [false, true] {
        let (handle, view, recording) = mount(cx);
        start_speech(cx, handle, &view, "typed ", "words");
        let (sender, events) = async_channel::unbounded();
        composer_turn(cx, handle, &view, |v, window, cx| {
            v.dictation_pump_fixture(events, window, cx)
        });
        composer_gesture(cx, handle, "enter");
        sender
            .send_blocking(EngineEvent::Heard("words last".into()))
            .unwrap();
        cx.run_until_parked();
        if final_arrives {
            sender
                .send_blocking(EngineEvent::Heard("words last final".into()))
                .unwrap();
            sender
                .send_blocking(EngineEvent::Finished("words last final".into()))
                .unwrap();
        } else {
            cx.executor().advance_clock(STOPPING_DEADLINE);
        }
        cx.run_until_parked();
        let (id, command) = next(&recording);
        composer_turn(cx, handle, &view, |v, window, cx| {
            v.submission_receipt(id, command, Ok(()), window, cx)
        });
        assert!(
            sender
                .try_send(EngineEvent::Heard("real late final".into()))
                .is_err()
        );
        assert!(
            sender
                .try_send(EngineEvent::Finished("real late final".into()))
                .is_err()
        );
        // A new pump/session cannot be changed by the retired sender either.
        speech_event(cx, handle, &view, Event::Start);
        speech_event(cx, handle, &view, Event::Ready);
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |v, cx| v.composer_text(cx)), "");
        assert!(recording.try_recv().is_err());
    }
}

#[gpui::test]
fn dictation_find_answer_menu_suggestions_and_media_keep_their_keys(cx: &mut TestAppContext) {
    use crate::dictation::Event;
    for finishing in [false, true] {
        for owner in ["find", "answer", "menu", "suggestions", "media"] {
            let (handle, view, recording) = mount(cx);
            start_speech(cx, handle, &view, "typed ", "words");
            if finishing {
                speech_event(cx, handle, &view, Event::Stop);
            }
            let phase = view.read_with(cx, |v, _| v.dictation.phase().clone());
            composer_turn(cx, handle, &view, |v, window, cx| match owner {
                "find" => v.open_find(window, cx),
                "answer" => {
                    let question = Question {
                        item_id: None,
                        request_id: "question".into(),
                        questions: vec![QuestionPrompt {
                            header: None,
                            question: "Which?".into(),
                            options: vec![],
                            multi_select: false,
                        }],
                    };
                    let key = editors::AnswerKey::of(&question, 0);
                    v.model.transcript.questions = vec![question];
                    v.ready_inputs(window, cx);
                    let state = v.answers[&key].state.clone();
                    state.update(cx, |s, cx| s.set_value("answer", window, cx));
                    state.read(cx).focus_handle(cx).focus(window, cx);
                }
                "menu" => {
                    v.model.transcript.info = Some(testing::info("fixture-chat"));
                    v.model.transcript.models = vec![crate::chat::model::ModelOption {
                        id: "chosen-model".into(),
                        name: "Chosen model".into(),
                        ..Default::default()
                    }];
                    v.menu = Some(Menu::Model);
                    assert!(v.step_menu(true, cx));
                    v.menu_armed = true;
                    v.focus(window, cx);
                }
                "suggestions" => {
                    let mut info = testing::info("fixture-chat");
                    info.provider = Provider::Claude;
                    v.model.transcript.info = Some(info);
                    v.set_composer_value("/", window, cx);
                    v.composer
                        .update(cx, |s, cx| s.set_selected_range(1..1, cx));
                    v.focus(window, cx);
                    v.sync_suggestions(window, cx);
                    assert!(v.suggestions.open.is_some());
                }
                _ => v.media.viewer = Some("fixture-image".into()),
            });
            composer_gesture(cx, handle, "enter");
            assert_eq!(
                view.read_with(cx, |v, _| v.dictation.phase().clone()),
                phase,
                "{owner} {finishing}: Enter touched dictation"
            );
            match owner {
                "answer" => assert!(matches!(next(&recording).1, ChatCommand::Answer { .. })),
                "menu" => assert!(matches!(
                    recording.try_recv().unwrap(),
                    feed::Delivery::Command(ChatCommand::Configure { .. })
                )),
                "suggestions" => {
                    assert!(view.read_with(cx, |v, cx| v.composer_text(cx).starts_with("/compact")))
                }
                _ => {}
            }
            // Reopen contexts closed by Enter so Escape is tested as a separate owner gesture.
            composer_turn(cx, handle, &view, |v, window, cx| {
                if owner == "menu" {
                    v.menu = Some(Menu::More);
                }
                if owner == "suggestions" {
                    v.set_composer_value("/", window, cx);
                    v.composer
                        .update(cx, |s, cx| s.set_selected_range(1..1, cx));
                    v.sync_suggestions(window, cx);
                }
            });
            composer_gesture(cx, handle, "escape");
            assert_eq!(
                view.read_with(cx, |v, _| v.dictation.phase().clone()),
                phase,
                "{owner} {finishing}: Escape touched dictation"
            );
            assert!(view.read_with(cx, |v, _| match owner {
                "find" => !v.find.open,
                "menu" => v.menu.is_none(),
                "suggestions" => v.suggestions.open.is_none(),
                "media" => v.media.viewer.is_none(),
                _ => true,
            }));
            assert!(
                recording.try_recv().is_err(),
                "{owner}: context keys submitted a message"
            );
        }
    }
}

#[gpui::test]
fn dictation_refused_enter_shows_notice_and_command_period_still_interrupts(
    cx: &mut TestAppContext,
) {
    use crate::dictation::{Event, Phase};
    let (handle, view, recording) = mount(cx);
    start_speech(cx, handle, &view, "typed ", "words");
    // A queued editor submission can reach the application after the chat disappears,
    // before the old focus/field is retired. Inject that event without redrawing first.
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |v, cx| {
            v.model.link = Link::Deleted;
            let composer = v.composer.clone();
            v.composer_event(
                &composer,
                &crate::text_input::InputEvent::PressEnter {
                    secondary: false,
                    shift: false,
                },
                window,
                cx,
            );
            assert_eq!(
                v.notices.local(notices::LocalKey::Send),
                Some("Finish dictation before sending the draft.")
            );
            assert!(matches!(v.dictation.phase(), Phase::Listening { .. }));
        });
    })
    .unwrap();
    cx.run_until_parked();
    assert!(recording.try_recv().is_err());
    composer_turn(cx, handle, &view, |v, window, cx| {
        assert!(matches!(v.dictation.phase(), Phase::Listening { .. }));
        v.model.link = Link::Live;
        v.focus(window, cx);
    });
    cx.update(|cx| {
        cx.bind_keys([gpui::KeyBinding::new(
            "cmd-.",
            InterruptChat,
            Some("ChatView"),
        )])
    });
    for finishing in [false, true] {
        composer_turn(cx, handle, &view, |v, _, _| {
            v.model.transcript.state = crate::chat::model::ChatState::Running
        });
        if finishing {
            speech_event(cx, handle, &view, Event::Stop);
        }
        let phase = view.read_with(cx, |v, _| v.dictation.phase().clone());
        composer_gesture(cx, handle, "cmd-.");
        assert!(matches!(
            recording.try_recv().unwrap(),
            feed::Delivery::Command(ChatCommand::Interrupt)
        ));
        assert_eq!(
            view.read_with(cx, |v, _| v.dictation.phase().clone()),
            phase
        );
    }
}
