use super::*;
use crate::chat::{
    attachments::{Attachment, AttachmentKind, Preview},
    model::{ChangeKind, ChatCommand, FileChange, Item, ItemStatus},
};
use crate::chat_view::{Link, editor_tests, feed};
use gpui::{App, TestAppContext, WindowHandle};
use gpui_kit::{base::Root, test::TestWindowExt};

type Handle = WindowHandle<Root>;
const PATCH: &str = "@@ -1,2 +1,2 @@\n keep\n-old\n+new";
fn item(patch: &str) -> Item {
    Item {
        id: "edit".into(),
        turn_id: Some("turn".into()),
        status: ItemStatus::Completed,
        presentation: Default::default(),
        body: ItemBody::FileChange {
            changes: vec![FileChange {
                path: "src/main.rs".into(),
                kind: ChangeKind::Modify,
                diff: Some(patch.into()),
            }],
        },
    }
}
fn act(cx: &mut TestAppContext, handle: Handle, f: impl FnOnce(&mut Window, &mut App)) {
    cx.update_window(handle.into(), |_, window, cx| f(window, cx))
        .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
}
fn add(cx: &mut TestAppContext, handle: Handle, view: &Entity<ChatView>) -> String {
    act(cx, handle, |window, cx| {
        view.update(cx, |view, cx| {
            assert!(view.add_review_reference("edit", 0, 1, 3, window, cx));
        })
    });
    cx.read(|cx| {
        view.read(cx)
            .references
            .contents()
            .last()
            .unwrap()
            .id
            .clone()
    })
}
fn install(cx: &mut TestAppContext, handle: Handle, view: &Entity<ChatView>) {
    act(cx, handle, |window, cx| {
        view.update(cx, |view, cx| {
            view.model.link = Link::Live;
            view.model.transcript.items = vec![item(PATCH)];
            view.composer.update(cx, |state, cx| {
                state.set_value("Existing writing 🦀  ", window, cx)
            });
            view.refresh_projection(cx);
            view.list.remeasure();
            cx.notify();
        })
    });
}

#[gpui::test]
fn native_reference_edits_remove_only_one_and_never_send_until_send(cx: &mut TestAppContext) {
    let (handle, view, recording) = editor_tests::mount_selection(cx);
    install(cx, handle, &view);
    let attachment = Attachment {
        id: "kept".into(),
        name: "notes.txt".into(),
        path: "/fixture/owned".into(),
        kind: AttachmentKind::Text,
        bytes: 4,
        fingerprint: "fixture".into(),
        preview: Preview::Text {
            excerpt: "kept".into(),
        },
    };
    view.update(cx, |view, _| {
        view.attachments
            .push(super::super::attachment_ui::Chip::ready(attachment.clone()))
    });
    let first = add(cx, handle, &view);
    act(cx, handle, |window, cx| {
        window.click(format!("reference-comment:{first}"), cx);
        window.input("Please simplify", cx);
        window.press("enter", cx);
        window.input("this branch", cx);
    });
    assert!(recording.try_recv().is_err());
    let second = add(cx, handle, &view);
    act(cx, handle, |window, cx| {
        window.click(format!("reference-quote:{second}"), cx);
        window.press("cmd-a", cx);
        window.input("edited excerpt", cx);
    });
    let values = cx.read(|cx| view.read(cx).references.contents());
    assert_eq!(values[0].comment, "Please simplify\nthis branch");
    assert_eq!(values[1].quote, "edited excerpt");
    assert_eq!(values[1].excerpt, " keep\n-old\n+new");
    assert_eq!(
        cx.read(|cx| view.read(cx).composer_text(cx)),
        "Existing writing 🦀  "
    );
    act(cx, handle, |window, cx| {
        window.click(format!("reference-remove:{first}"), cx)
    });
    assert_eq!(cx.read(|cx| view.read(cx).references.contents().len()), 1);
    assert_eq!(
        cx.read(|cx| view.read(cx).references.contents()[0].id.clone()),
        second
    );
    assert!(recording.try_recv().is_err());
    act(cx, handle, |window, cx| window.click("chat-send", cx));
    let feed::Delivery::Submission { id, command } = recording.try_recv().expect("explicit Send")
    else {
        panic!("submission")
    };
    let ChatCommand::SendAttachments { text, attachments } = &command else {
        panic!("attachments preserved")
    };
    assert_eq!(attachments, &vec![attachment]);
    assert!(text.starts_with("Existing writing 🦀  \n\nReview comment on \"src/main.rs\""));
    assert!(text.contains("> edited excerpt"));
    assert!(!text.contains("Please simplify"));
    act(cx, handle, |window, cx| {
        view.update(cx, |view, cx| {
            view.submission_receipt(id, command, Ok(()), window, cx)
        })
    });
    assert!(cx.read(|cx| view.read(cx).references.is_empty()));
}

#[gpui::test]
fn stale_reference_is_explicit_and_reanchors_unique_unchanged_lines(cx: &mut TestAppContext) {
    let (handle, view, recording) = editor_tests::mount_selection(cx);
    install(cx, handle, &view);
    let id = add(cx, handle, &view);
    let original = cx.read(|cx| view.read(cx).references.contents()[0].origin.clone());
    let changed = "@@ -20,2 +30,2 @@\n keep\n-old\n+new";
    act(cx, handle, |_, cx| {
        view.update(cx, |view, cx| {
            view.model.transcript.items[0] = item(changed);
            view.refresh_reference_states(cx);
            cx.notify();
        })
    });
    assert_eq!(
        cx.read(|cx| view.read(cx).references.contents()[0].state),
        ReferenceState::Changed
    );
    act(cx, handle, |window, cx| {
        window.click(format!("reference-reanchor:{id}"), cx)
    });
    let value = cx.read(|cx| view.read(cx).references.contents()[0].clone());
    assert_eq!(value.state, ReferenceState::Current);
    assert_eq!(value.original_origin, Some(original));
    let ReferenceOrigin::Diff {
        old, new, revision, ..
    } = value.origin
    else {
        panic!("diff")
    };
    assert_eq!(old, vec![(20, 21)]);
    assert_eq!(new, vec![(30, 31)]);
    assert_eq!(revision, data::revision(changed));
    act(cx, handle, |window, cx| {
        window.click(format!("reference-go:{id}"), cx)
    });
    act(cx, handle, |window, _| {
        assert!(window.try_find("chat-file-review").is_some())
    });
    assert!(recording.try_recv().is_err());
}

#[gpui::test]
fn ambiguous_or_missing_source_never_rewrites_saved_reference(cx: &mut TestAppContext) {
    let (handle, view, recording) = editor_tests::mount_selection(cx);
    install(cx, handle, &view);
    let id = add(cx, handle, &view);
    let original = cx.read(|cx| view.read(cx).references.contents()[0].origin.clone());
    act(cx, handle, |_, cx| {
        view.update(cx, |view, cx| {
            view.model.transcript.items[0] = item(&format!("{PATCH}\n{PATCH}"));
            view.refresh_reference_states(cx);
            cx.notify();
        })
    });
    act(cx, handle, |window, cx| {
        window.click(format!("reference-reanchor:{id}"), cx)
    });
    assert_eq!(
        cx.read(|cx| view.read(cx).references.contents()[0].origin.clone()),
        original
    );
    assert_eq!(
        cx.read(|cx| view.read(cx).references.contents()[0].state),
        ReferenceState::Changed
    );
    act(cx, handle, |_, cx| {
        view.update(cx, |view, cx| {
            view.model.transcript.items.clear();
            view.refresh_reference_states(cx);
            cx.notify();
        })
    });
    assert_eq!(
        cx.read(|cx| view.read(cx).references.contents()[0].state),
        ReferenceState::Missing
    );
    act(cx, handle, |window, cx| {
        window.click(format!("reference-go:{id}"), cx)
    });
    assert_eq!(
        cx.read(|cx| view.read(cx).references.contents()[0].origin.clone()),
        original
    );
    assert!(recording.try_recv().is_err());
}

#[gpui::test]
fn reference_native_copy_and_ime_escape_never_reach_transcript_or_send(cx: &mut TestAppContext) {
    use gpui::{ElementInputHandler, InputHandler as _};
    let (handle, view, recording) = editor_tests::mount_selection(cx);
    install(cx, handle, &view);
    let id = add(cx, handle, &view);
    act(cx, handle, |window, cx| {
        window.click(format!("reference-comment:{id}"), cx);
        let editor = view.read(cx).references.entries[0].comment.clone();
        let mut handler = ElementInputHandler::new(
            window.find(format!("reference-comment:{id}")).bounds(),
            editor,
        );
        handler.replace_and_mark_text_in_range(None, "日本", Some(2..2), window, cx);
        view.update(cx, |view, cx| {
            view.escape_action(&gpui_kit::base::input::Escape, window, cx)
        });
        assert_eq!(
            view.read(cx).references.editing.as_deref(),
            Some(id.as_str())
        );
        window.press("escape", cx);
    });
    assert_eq!(
        cx.read(|cx| view.read(cx).references.editing.clone()),
        Some(id)
    );
    act(cx, handle, |window, cx| {
        window.input("Exact comment 🦀", cx);
        window.press("cmd-a", cx);
        window.press("cmd-c", cx);
    });
    let current = cx.read(|cx| {
        view.read(cx).references.entries[0]
            .comment
            .read(cx)
            .value()
            .to_string()
    });
    assert_eq!(
        cx.read(|cx| cx.read_from_clipboard().unwrap().text()),
        Some(current)
    );
    assert_eq!(
        cx.read(|cx| view.read(cx).composer_text(cx)),
        "Existing writing 🦀  "
    );
    assert!(recording.try_recv().is_err());
}
