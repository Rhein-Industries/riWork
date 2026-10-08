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
