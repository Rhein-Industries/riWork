//! What a user message sent with it, in the transcript: real snapshots staged as the chat host
//! stages them, logged as Claude's and Muse's chats log the message (`attachments::summary`
//! as its text, the snapshots under `presentation`), drawn by a mounted chat.
use super::*;
use crate::chat::{
    attachments::{self, stage},
    model::{ChatEvent, ChatImage, ImageSource, Item, ItemBody, ItemStatus, Presentation},
    wire::Envelope,
};
use crate::chat_view::{
    ImageScope,
    editor_tests::mount,
    feed::FeedMsg,
    image_scope::tests::{Fixture, png},
    notices::LocalKey,
    selection_document,
    widgets::Look,
};
use gpui::{Entity, TestAppContext, WindowHandle};
use gpui_kit::test::TestWindowExt;
use std::path::Path;

const ALPHA: &str = "aaaaaaaa-0000-4000-8000-0000000000a1";
/// The fixture's chat: its snapshots are the only ones under `chats` its scope reaches.
const CHAT: &str = "11111111-2222-4333-8444-555555555555";

fn snapshot(fixture: &Fixture, source: &Path) -> Attachment {
    let chat = crate::chat::log::chat_dir(&fixture.home, CHAT).unwrap();
    fs::create_dir_all(&chat).unwrap();
    stage(&chat, source).unwrap()
}

/// The user message as the host logs it: the summary as its text, each image also as a
/// local image, every snapshot under `attachments`.
fn sent(id: &str, text: &str, sent: &[Attachment]) -> Item {
    Item {
        presentation: Presentation {
            images: sent
                .iter()
                .filter(|a| matches!(a.kind, AttachmentKind::Image { .. }))
                .map(|a| ChatImage {
                    label: a.name.clone(),
                    source: ImageSource::Local {
                        path: a.path.display().to_string(),
                    },
                })
                .collect(),
            attachments: sent.to_vec(),
            ..Presentation::default()
        },
        id: id.into(),
        turn_id: Some("turn-1".into()),
        status: ItemStatus::Completed,
        body: ItemBody::UserMessage {
            text: attachments::summary(text, sent),
        },
    }
}

type Mounted = (
    WindowHandle<gpui_kit::base::Root>,
    Entity<ChatView>,
    Fixture,
);

/// A chat of project alpha showing `items`, its scope given as the window gives it.
fn show(cx: &mut TestAppContext, fixture: Fixture, items: Vec<serde_json::Value>) -> Mounted {
    let (handle, view, _feed) = mount(cx);
    let info = fixture.chat(Some(ALPHA), Some("alpha-main"), &fixture.alpha, None);
    let mut events = vec![serde_json::to_value(ChatEvent::Info { info }).unwrap()];
    events.extend(items);
    let envelopes = events
        .into_iter()
        .enumerate()
        .map(|(at, event)| {
            serde_json::from_value::<Envelope>(serde_json::json!({
                "chat_id": "fixture-chat",
                "seq": at as u64 + 1,
                "event": event,
            }))
            .unwrap()
        })
        .collect();
    let scope = fixture.alpha_chat();
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |view, cx| {
            view.accept(vec![FeedMsg::Events(envelopes)], window, cx);
            view.set_image_scope(scope, cx);
        });
    })
    .unwrap();
    (handle, view, fixture)
}

fn completed(item: &Item) -> serde_json::Value {
    serde_json::to_value(ChatEvent::ItemCompleted { item: item.clone() }).unwrap()
}

/// A frame drawn after every read the last one started has come back.
fn settle(cx: &mut TestAppContext, handle: WindowHandle<gpui_kit::base::Root>) {
    for _ in 0..2 {
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| window.render_frame(cx))
            .unwrap();
    }
}

/// The transcript's text as Copy reads it.
fn transcript(cx: &mut TestAppContext, view: &Entity<ChatView>) -> Vec<String> {
    view.read_with(cx, |view, cx| {
        selection_document::leaves(view, Look::of(cx))
            .into_iter()
            .map(|leaf| leaf.text)
            .collect()
    })
}

fn key(item: &str, a: &Attachment) -> String {
    format!("{item}:sent:{}", a.id)
}

#[gpui::test]
fn a_sent_image_shows_as_its_thumbnail_in_place_of_the_attached_text(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    // A pasted image, as the composer names it, and a second one: they wrap in one row.
    let pasted = fixture
        .alpha
        .join("Clipboard image 6f0e1d2c-0000-4000-8000-000000000001.png");
    png(&pasted);
    let images = [
        snapshot(&fixture, &pasted),
        snapshot(&fixture, &fixture.alpha.join("shot.png")),
    ];
    let item = sent("user-1", "Why is this red?", &images);
    let (handle, view, _fixture) = show(cx, fixture, vec![completed(&item)]);
    let keys = images.iter().map(|a| key("user-1", a)).collect::<Vec<_>>();

    // Before any thumbnail is read, each card already has its size: nothing moves when it is.
    let mut before = Vec::new();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        for key in &keys {
            let card = window.find(format!("sent-attachment:{key}"));
            assert!(window.try_find(format!("sent-thumbnail:{key}")).is_none());
            before.push(card.bounds());
        }
        before.push(window.find("user-message:user-1").bounds());
    })
    .unwrap();
    settle(cx, handle);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let mut after = Vec::new();
        for (key, a) in keys.iter().zip(&images) {
            let card = window.find(format!("sent-attachment:{key}"));
            assert_eq!(card.label(), Some(format!("Preview {}", a.name).as_str()));
            let thumbnail = window.find(format!("sent-thumbnail:{key}"));
            assert_eq!(thumbnail.role(), Some(gpui::Role::Image));
            assert_eq!(
                thumbnail.label(),
                Some(format!("Sent image: {}", a.name).as_str())
            );
            assert!(window.try_find(format!("sent-file:{key}")).is_none());
            // A square of the same side for every image, side by side.
            let bounds = card.bounds();
            assert_eq!(bounds.size.width, bounds.size.height);
            after.push(bounds);
        }
        after.push(window.find("user-message:user-1").bounds());
        assert_eq!(after, before);
        assert_eq!(after[0].size, after[1].size);
        assert_eq!(after[0].top(), after[1].top());
        assert!(after[1].left() > after[0].right());
        // The images are not also offered as folded image cards below the message.
        for n in 0..images.len() {
            assert!(window.try_find(format!("fold:user-1:image:{n}")).is_none());
        }
    })
    .unwrap();
    // What the user typed is the message's text; the attached lines are gone.
    assert_eq!(transcript(cx, &view), vec!["Why is this red?".to_owned()]);
}

#[gpui::test]
fn an_image_sent_alone_shows_only_its_thumbnail(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let image = snapshot(&fixture, &fixture.alpha.join("shot.png"));
    let item = sent("user-1", "", std::slice::from_ref(&image));
    let (handle, view, _fixture) = show(cx, fixture, vec![completed(&item)]);
    settle(cx, handle);
    cx.update_window(handle.into(), |_, window, _| {
        assert!(
            window
                .try_find(format!("sent-thumbnail:{}", key("user-1", &image)))
                .is_some()
        );
    })
    .unwrap();
    assert!(transcript(cx, &view).is_empty());
}

#[gpui::test]
fn a_sent_image_whose_file_is_gone_is_a_card_naming_it(cx: &mut TestAppContext) {
    QUICK_LOOKS.with(|q| q.borrow_mut().clear());
    let fixture = Fixture::new();
    let gone = snapshot(&fixture, &fixture.alpha.join("shot.png"));
    let Preview::Image { path: preview } = &gone.preview else {
        panic!("an image has a preview");
    };
    fs::remove_file(preview).unwrap();
    fs::remove_file(&gone.path).unwrap();
    let item = sent("user-1", "see", std::slice::from_ref(&gone));
    let (handle, view, _fixture) = show(cx, fixture, vec![completed(&item)]);
    settle(cx, handle);
    let key = key("user-1", &gone);
    cx.update_window(handle.into(), |_, window, cx| {
        assert!(window.try_find(format!("sent-thumbnail:{key}")).is_none());
        assert!(window.try_find(format!("sent-file:{key}")).is_some());
        assert_eq!(
            window.find(format!("sent-attachment:{key}")).label(),
            Some("Preview shot.png")
        );
        assert_eq!(
            window.find(format!("sent-status:{key}")).label(),
            Some("Image unavailable")
        );
        // A click finds nothing to show and changes nothing.
        window.click(format!("sent-attachment:{key}"), cx);
    })
    .unwrap();
    assert!(QUICK_LOOKS.with(|q| q.borrow().is_empty()));
    assert_eq!(
        view.read_with(cx, |view, _| view
            .notices
            .local(LocalKey::Media)
            .map(str::to_owned)),
        Some(format!("{} is no longer available to preview.", "shot.png"))
    );
    let text = transcript(cx, &view);
    assert_eq!(text, vec!["see".to_owned()]);
    assert!(!text.concat().contains("bytes"));
}

#[gpui::test]
fn a_sent_text_file_is_a_card_with_its_name_and_size(cx: &mut TestAppContext) {
    QUICK_LOOKS.with(|q| q.borrow_mut().clear());
    let fixture = Fixture::new();
    let notes = snapshot(&fixture, &fixture.alpha.join("notes.txt"));
    let image = snapshot(&fixture, &fixture.alpha.join("shot.png"));
    let item = sent("user-1", "both", &[notes.clone(), image.clone()]);
    let (handle, view, _fixture) = show(cx, fixture, vec![completed(&item)]);
    settle(cx, handle);
    cx.update_window(handle.into(), |_, window, cx| {
        let file = key("user-1", &notes);
        assert!(window.try_find(format!("sent-file:{file}")).is_some());
        assert!(window.try_find(format!("sent-thumbnail:{file}")).is_none());
        assert_eq!(
            window.find(format!("sent-status:{file}")).label(),
            Some(size_text(notes.bytes).as_str())
        );
        assert!(
            window
                .try_find(format!("sent-thumbnail:{}", key("user-1", &image)))
                .is_some()
        );
        // A card's click shows the snapshot itself in Quick Look, as the composer's does.
        window.click(format!("sent-attachment:{file}"), cx);
        window.click(format!("sent-attachment:{}", key("user-1", &image)), cx);
    })
    .unwrap();
    assert_eq!(
        QUICK_LOOKS.with(|q| q.borrow().clone()),
        vec![
            (notes.path.clone(), "public.plain-text"),
            (image.path.clone(), "public.png")
        ]
    );
    assert_eq!(transcript(cx, &view), vec!["both".to_owned()]);
}

#[gpui::test]
fn a_logged_snapshot_outside_this_chats_folder_is_neither_read_nor_shown(cx: &mut TestAppContext) {
    QUICK_LOOKS.with(|q| q.borrow_mut().clear());
    let fixture = Fixture::new();
    // Another chat's snapshot, named in this chat's log.
    let chat =
        crate::chat::log::chat_dir(&fixture.home, "99999999-2222-4333-8444-555555555555").unwrap();
    fs::create_dir_all(&chat).unwrap();
    let other = stage(&chat, &fixture.alpha.join("shot.png")).unwrap();
    let item = sent("user-1", "", std::slice::from_ref(&other));
    let (handle, view, _fixture) = show(cx, fixture, vec![completed(&item)]);
    settle(cx, handle);
    let key = key("user-1", &other);
    cx.update_window(handle.into(), |_, window, cx| {
        assert!(window.try_find(format!("sent-thumbnail:{key}")).is_none());
        assert_eq!(
            window.find(format!("sent-status:{key}")).label(),
            Some("Image unavailable")
        );
        window.click(format!("sent-attachment:{key}"), cx);
    })
    .unwrap();
    assert!(QUICK_LOOKS.with(|q| q.borrow().is_empty()));
    assert_eq!(
        view.read_with(cx, |view, _| view
            .notices
            .local(LocalKey::Media)
            .map(str::to_owned)),
        Some(format!("{} is no longer available to preview.", "shot.png"))
    );
}

#[gpui::test]
fn messages_logged_before_and_beside_this_still_show_as_they_did(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let image = snapshot(&fixture, &fixture.alpha.join("shot.png"));
    let summary = attachments::summary("older", std::slice::from_ref(&image));
    // As a host wrote it before it kept the snapshots: the summary and the image only.
    let older = serde_json::json!({
        "event": "item_completed",
        "item": {
            "presentation": {"images": [{"label": image.name, "source": {"kind": "local", "path": image.path}}]},
            "id": "user-old",
            "turn_id": "turn-1",
            "status": "completed",
            "body": {"type": "user_message", "text": summary},
        },
    });
    // As hosts write it now, read back from the log after a restart.
    let logged = serde_json::json!({
        "event": "item_completed",
        "item": {
            "presentation": {
                "images": [{"label": image.name, "source": {"kind": "local", "path": image.path}}],
                "attachments": [serde_json::to_value(&image).unwrap()],
            },
            "id": "user-now",
            "turn_id": "turn-2",
            "status": "completed",
            "body": {"type": "user_message", "text": attachments::summary("newer", std::slice::from_ref(&image))},
        },
    });
    // A text that is not the summary of its attachments is shown whole, beside its cards.
    let mut edited = sent("user-edited", "edited", std::slice::from_ref(&image));
    edited.body = ItemBody::UserMessage {
        text: "edited, not by the host".into(),
    };
    let plain = sent("user-plain", "no attachments", &[]);
    let (handle, view, _fixture) = show(
        cx,
        fixture,
        vec![older, logged, completed(&edited), completed(&plain)],
    );
    settle(cx, handle);
    cx.update_window(handle.into(), |_, window, _| {
        // The older message keeps its folded image card and draws no snapshot cards.
        assert!(window.try_find("fold:user-old:image:0").is_some());
        assert!(
            window
                .try_find(format!("sent-attachment:{}", key("user-old", &image)))
                .is_none()
        );
        for item in ["user-now", "user-edited"] {
            assert!(
                window
                    .try_find(format!("sent-thumbnail:{}", key(item, &image)))
                    .is_some(),
                "{item}"
            );
            assert!(window.try_find(format!("fold:{item}:image:0")).is_none());
        }
        assert!(window.try_find("user-message:user-plain").is_some());
    })
    .unwrap();
    // The older message's text is as it was logged, the attached line and all (its file name
    // is a reference the prose draws, as before).
    let text = transcript(cx, &view);
    let (older, rest) = text.split_at(text.len() - 3);
    let older = older.concat();
    assert!(summary.starts_with("older\n[Attached: shot.png ("));
    assert!(
        older.starts_with("older") && older.contains("[Attached: "),
        "{older:?}"
    );
    assert_eq!(
        rest,
        ["newer", "edited, not by the host", "no attachments"].map(str::to_owned)
    );
}

#[gpui::test]
fn thumbnails_are_read_a_few_at_a_time_once_each_and_again_under_a_new_scope(
    cx: &mut TestAppContext,
) {
    let fixture = Fixture::new();
    let mut sent_files = Vec::new();
    for n in 0..6 {
        let source = fixture.alpha.join(format!("shot {n}.png"));
        png(&source);
        sent_files.push(snapshot(&fixture, &source));
    }
    // One whose preview is gone: tried once, then left alone.
    let gone = sent_files[5].clone();
    let Preview::Image { path: preview } = &gone.preview else {
        panic!("an image has a preview");
    };
    fs::remove_file(preview).unwrap();
    let item = sent("user-1", "six", &sent_files);
    let (handle, view, fixture) = show(cx, fixture, vec![completed(&item)]);
    let reads = |cx: &mut TestAppContext| view.read_with(cx, |view, _| view.thumbnail_reads());
    let shown = |cx: &mut TestAppContext| {
        cx.update_window(handle.into(), |_, window, _| {
            sent_files
                .iter()
                .filter(|a| {
                    window
                        .try_find(format!("sent-thumbnail:{}", key("user-1", a)))
                        .is_some()
                })
                .count()
        })
        .unwrap()
    };

    cx.update_window(handle.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
    let (reading, asked) = reads(cx);
    assert_eq!((reading, asked.len()), (4, 4), "four at a time");
    settle(cx, handle);
    let (reading, asked) = reads(cx);
    assert_eq!((reading, asked.len()), (0, 6));
    assert_eq!(shown(cx), 5);
    // More frames ask for nothing again, the failed one included.
    for _ in 0..3 {
        settle(cx, handle);
    }
    assert_eq!(reads(cx), (0, asked.clone()));
    cx.update_window(handle.into(), |_, window, _| {
        assert_eq!(
            window
                .find(format!("sent-status:{}", key("user-1", &gone)))
                .label(),
            Some("Image unavailable")
        );
    })
    .unwrap();

    // A scope without the chat's identity forgets them at once and reads nothing.
    view.update(cx, |view, cx| {
        view.set_image_scope(ImageScope::default(), cx)
    });
    assert_eq!(reads(cx), (0, Default::default()));
    settle(cx, handle);
    assert_eq!(reads(cx), (0, Default::default()));
    assert_eq!(shown(cx), 0);
    // The chat's scope again: they are read again.
    let scope = fixture.alpha_chat();
    view.update(cx, |view, cx| view.set_image_scope(scope, cx));
    for _ in 0..2 {
        settle(cx, handle);
    }
    assert_eq!(shown(cx), 5);
}

#[gpui::test]
fn a_thumbnail_read_under_a_scope_that_has_since_gone_is_dropped(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let image = snapshot(&fixture, &fixture.alpha.join("shot.png"));
    let item = sent("user-1", "", std::slice::from_ref(&image));
    let (handle, view, _fixture) = show(cx, fixture, vec![completed(&item)]);
    // The read starts, and the scope goes before it comes back.
    cx.update_window(handle.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
    assert_eq!(view.read_with(cx, |view, _| view.thumbnail_reads().0), 1);
    view.update(cx, |view, cx| {
        view.set_image_scope(ImageScope::default(), cx)
    });
    cx.run_until_parked();
    assert_eq!(
        view.read_with(cx, |view, _| view.thumbnail_reads()),
        (0, Default::default())
    );
    settle(cx, handle);
    cx.update_window(handle.into(), |_, window, _| {
        let key = key("user-1", &image);
        assert!(window.try_find(format!("sent-thumbnail:{key}")).is_none());
        assert_eq!(
            window.find(format!("sent-status:{key}")).label(),
            Some("Image unavailable")
        );
    })
    .unwrap();
}

#[gpui::test]
fn a_preview_or_snapshot_the_log_names_elsewhere_is_never_read(cx: &mut TestAppContext) {
    QUICK_LOOKS.with(|q| q.borrow_mut().clear());
    let fixture = Fixture::new();
    let staged = snapshot(&fixture, &fixture.alpha.join("shot.png"));
    // A preview named in a project folder the scope reaches, beside a real snapshot.
    let mut elsewhere = staged.clone();
    elsewhere.preview = Preview::Image {
        path: fixture.alpha.join("shot.png"),
    };
    // A snapshot under an id that is not its own folder's.
    let mut renamed = staged.clone();
    renamed.id = uuid::Uuid::new_v4().to_string();
    let item = sent("user-1", "", &[elsewhere.clone(), renamed.clone()]);
    let (handle, view, _fixture) = show(cx, fixture, vec![completed(&item)]);
    settle(cx, handle);
    assert_eq!(
        view.read_with(cx, |view, _| view.thumbnail_reads()),
        (0, Default::default())
    );
    cx.update_window(handle.into(), |_, window, cx| {
        for a in [&elsewhere, &renamed] {
            let key = key("user-1", a);
            assert_eq!(
                window.find(format!("sent-status:{key}")).label(),
                Some("Image unavailable")
            );
        }
        // The renamed one names a real snapshot, but not as its own: Quick Look is refused.
        window.click(format!("sent-attachment:{}", key("user-1", &renamed)), cx);
        // The other's snapshot is its own: shown, though its preview is refused.
        window.click(format!("sent-attachment:{}", key("user-1", &elsewhere)), cx);
    })
    .unwrap();
    assert_eq!(
        QUICK_LOOKS.with(|q| q.borrow().clone()),
        vec![(staged.path.clone(), "public.png")]
    );
}
