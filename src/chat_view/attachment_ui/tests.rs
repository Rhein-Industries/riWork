//! Headless ChatView events and actual private, locally staged thumbnails.
//! Recording Feed only; no socket, provider, host startup or native GUI.
use super::*;
use crate::{
    chat::{attachments, client::CallError, model::ChatCommand},
    chat_view::{HostConfig, attachment_draft, editor_tests, feed, state::Link},
};
use gpui::{SharedString, TestAppContext};
use gpui_kit::test::TestWindowExt;
use std::{
    io::Cursor,
    sync::atomic::{AtomicUsize, Ordering},
};

fn id(prefix: &str, key: &str) -> SharedString {
    format!("{prefix}-{key}").into()
}

fn image_bytes(format: image::ImageFormat) -> Vec<u8> {
    let mut bytes = Vec::new();
    image::DynamicImage::new_rgb8(512, 384)
        .write_to(&mut Cursor::new(&mut bytes), format)
        .unwrap();
    bytes
}

fn locally_staged() -> Vec<Attachment> {
    // Only the reviewer's inherited private TMPDIR; no home/settings/host.
    // Leave these owned fixture artifacts for reviewer inspection.
    let root = std::env::temp_dir().join(format!("riwork-draft-preview-{}", Uuid::new_v4()));
    fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
    let chat = root.join("chat");
    fs::DirBuilder::new().mode(0o700).create(&chat).unwrap();
    [
        ("photo.png", image_bytes(image::ImageFormat::Png)),
        ("photo.jpg", image_bytes(image::ImageFormat::Jpeg)),
        ("notes.txt", b"exact staged notes\n".to_vec()),
    ]
    .into_iter()
    .map(|(name, bytes)| {
        let path = root.join(name);
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap()
            .write_all(&bytes)
            .unwrap();
        let attachment = attachments::stage(&chat, &path).unwrap();
        if let Preview::Image { path } = &attachment.preview {
            assert_ne!(path, &attachment.path);
            assert_eq!(image::image_dimensions(path).unwrap(), (256, 192));
        }
        attachment
    })
    .collect()
}

#[gpui::test]
fn staged_png_jpeg_thumbnails_render_above_composer_and_keep_exact_draft(cx: &mut TestAppContext) {
    let (handle, view, recording) = editor_tests::mount_selection(cx);
    let staged = locally_staged();
    let editor = view.read_with(cx, |v, _| v.composer.entity_id());
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |v, cx| {
            v.model.link = Link::Live;
            v.attachments = staged.iter().cloned().map(Chip::ready).collect();
            v.bump_generation();
            cx.notify();
        });
        window.click("chat-composer", cx);
        window.input("exact draft 🦀  ", cx);
    })
    .unwrap();
    cx.run_until_parked();
    let generation = view.read_with(cx, |v, _| v.editor_generation);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let queue = window.find("chat-attachment-queue");
        assert_eq!(queue.role(), Some(gpui::Role::Group));
        assert_eq!(queue.label(), Some("Draft attachments"));
        assert!(queue.bounds().bottom() <= window.find("chat-composer-shell").bounds().top());
        for attachment in &staged[..2] {
            let thumb = window.find(id("attachment-thumbnail", &attachment.id));
            assert_eq!(thumb.role(), Some(gpui::Role::Image));
            let label = format!("Image preview: {}", attachment.name);
            assert_eq!(thumb.label(), Some(label.as_str()));
            assert!(thumb.visible());
            assert!(thumb.bounds().size.width > gpui::px(0.));
            assert!(thumb.bounds().size.width <= ui_text::space(56.));
            assert!(thumb.bounds().size.height <= ui_text::space(56.));
            assert_eq!(
                window
                    .find(id("attachment-preview", &attachment.id))
                    .expanded(),
                Some(false)
            );
            assert!(
                window
                    .try_find(id("attachment-expanded", &attachment.id))
                    .is_none()
            );
            view.read(cx)
                .attachments
                .iter()
                .find(|c| c.id == attachment.id)
                .map(|c| {
                    assert_eq!(c.attachment(), Some(attachment));
                    let Preview::Image { path } = &attachment.preview else {
                        unreachable!()
                    };
                    assert_eq!(c.image_preview(), Some(path));
                })
                .expect("exact staged chip retained");
        }
        assert!(
            window
                .try_find(id("attachment-thumbnail", &staged[2].id))
                .is_none()
        );
        window.click(id("attachment-preview", &staged[0].id), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window
                .find(id("attachment-preview", &staged[0].id))
                .expanded(),
            Some(true)
        );
        let expanded = window.find(id("attachment-expanded", &staged[0].id));
        assert!(expanded.visible());
        assert!(expanded.bounds().size.width <= ui_text::space(256.));
        assert!(expanded.bounds().size.height <= ui_text::space(256.));
        assert!(!view.read(cx).attachments[1].open);
        // Actual Base keyboard activation collapses the focused disclosure once.
        window.press("space", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window
                .find(id("attachment-preview", &staged[0].id))
                .expanded(),
            Some(false)
        );
        assert!(
            window
                .find(id("attachment-thumbnail", &staged[0].id))
                .visible()
        );
        assert!(
            window
                .try_find(id("attachment-expanded", &staged[0].id))
                .is_none()
        );
        assert_eq!(view.read(cx).composer.entity_id(), editor);
        assert_eq!(
            view.read(cx).editor_generation,
            generation,
            "preview is presentation only"
        );
        assert_eq!(view.read(cx).composer_text(cx), "exact draft 🦀  ");
        window.click(id("attachment-remove", &staged[0].id), cx);
    })
    .unwrap();
    cx.run_until_parked();
    let draft = attachment_draft::Draft {
        text: "exact draft 🦀  ".into(),
        attachments: staged[1..].to_vec(),
    };
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window
                .try_find(id("attachment-thumbnail", &staged[0].id))
                .is_none()
        );
        assert!(
            window
                .find(id("attachment-thumbnail", &staged[1].id))
                .visible()
        );
        assert_eq!(view.read(cx).composer_text(cx), draft.text);
        assert_eq!(
            view.read(cx)
                .attachments
                .iter()
                .filter_map(|chip| chip.attachment().cloned())
                .collect::<Vec<_>>(),
            draft.attachments,
        );
        window.click("chat-send", cx);
    })
    .unwrap();
    cx.run_until_parked();
    let feed::Delivery::Submission {
        id: submission,
        command,
    } = recording.try_recv().unwrap()
    else {
        panic!("exact draft must dispatch once with a receipt identity")
    };
    assert_eq!(
        command,
        ChatCommand::SendAttachments {
            text: draft.text.clone(),
            attachments: draft.attachments.clone()
        }
    );
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |v, cx| {
            v.submission_receipt(
                submission,
                command,
                Err(CallError::Broken("fixture lost receipt".into())),
                window,
                cx,
            );
            assert_eq!(v.composer_text(cx), draft.text);
            assert_eq!(
                v.attachments
                    .iter()
                    .filter_map(|chip| chip.attachment().cloned())
                    .collect::<Vec<_>>(),
                draft.attachments
            );
            assert_eq!(v.submissions[0].snapshot, draft);
        });
        window.render_frame(cx);
        assert!(
            window
                .find(id("attachment-thumbnail", &staged[1].id))
                .visible()
        );
    })
    .unwrap();
    cx.run_until_parked();
    assert!(
        recording.try_recv().is_err(),
        "ambiguous submission must not retry"
    );
}

#[gpui::test]
fn mixed_image_paste_retry_remove_preserve_chip_source_identity(cx: &mut TestAppContext) {
    let attempts = Arc::new(AtomicUsize::new(0));
    let count = attempts.clone();
    let (handle, view, recording) = editor_tests::mount_config(
        cx,
        HostConfig {
            ensure: Arc::new(move || {
                let attempt = count.fetch_add(1, Ordering::SeqCst) + 1;
                Err(format!("private fixture refuses staging attempt {attempt}"))
            }),
        },
    );
    let images = [
        Image::from_bytes(ImageFormat::Png, image_bytes(image::ImageFormat::Png)),
        Image::from_bytes(ImageFormat::Jpeg, image_bytes(image::ImageFormat::Jpeg)),
    ];
    cx.update_window(handle.into(), |_, window, cx| {
        window.click("chat-composer", cx);
        cx.write_to_clipboard(ClipboardItem::new_string("plain text paste".into()));
        window.press("cmd-v", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        let mut item = ClipboardItem::new_string("must not be inserted".into());
        item.entries
            .extend(images.iter().cloned().map(ClipboardEntry::Image));
        cx.write_to_clipboard(item);
        window.press("cmd-v", cx);
    })
    .unwrap();
    cx.run_until_parked();
    let keys = view.read_with(cx, |v, cx| {
        assert_eq!(v.composer_text(cx), "plain text paste");
        assert_eq!(v.attachments.len(), 2);
        v.attachments
            .iter()
            .map(|c| c.id.clone())
            .collect::<Vec<_>>()
    });
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        for (chip, original) in view.read(cx).attachments.iter().zip(&images) {
            let Stage::Failed(error) = &chip.state else {
                panic!("expected filename-specific refusal")
            };
            assert!(error.contains(&chip.name));
            assert_eq!(
                window.find(id("attachment-status", &chip.id)).label(),
                Some(error.as_str())
            );
            assert!(
                window
                    .try_find(id("attachment-thumbnail", &chip.id))
                    .is_none()
            );
            let Some(Source::Image { image, .. }) = &chip.source else {
                panic!("retry source lost")
            };
            assert_eq!(image.as_ref(), original);
        }
        let retry = window.find(id("attachment-retry", &keys[0]));
        assert_eq!(retry.role(), Some(gpui::Role::Button));
        let label = format!("Retry staging {}", view.read(cx).attachments[0].name);
        assert_eq!(retry.label(), Some(label.as_str()));
        window.click(id("attachment-retry", &keys[0]), cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    view.read_with(cx, |v, cx| {
        assert_eq!(v.composer_text(cx), "plain text paste");
        assert_eq!(
            v.attachments
                .iter()
                .map(|c| c.id.clone())
                .collect::<Vec<_>>(),
            keys
        );
        for (chip, original) in v.attachments.iter().zip(&images) {
            let Some(Source::Image { image, .. }) = &chip.source else {
                panic!("source changed on retry")
            };
            assert_eq!(image.as_ref(), original);
            assert!(matches!(chip.state, Stage::Failed(_)));
        }
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.click(id("attachment-remove", &keys[0]), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(view.read(cx).attachments.len(), 1);
        assert_eq!(view.read(cx).attachments[0].id, keys[1]);
        // Removal while its one-shot retry is in flight must not resurrect it.
        window.click(id("attachment-retry", &keys[1]), cx);
        assert!(matches!(view.read(cx).attachments[0].state, Stage::Pending));
        window.click(id("attachment-remove", &keys[1]), cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(attempts.load(Ordering::SeqCst), 4);
    view.read_with(cx, |v, cx| {
        assert!(v.attachments.is_empty());
        assert_eq!(v.composer_text(cx), "plain text paste");
    });
    assert!(recording.try_recv().is_err());
}
