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
    let local = Arc::new(Image::from_bytes(
        ImageFormat::Png,
        image_thumbnail(&image_bytes(image::ImageFormat::Png)).unwrap(),
    ));
    let editor = view.read_with(cx, |v, _| v.composer.entity_id());
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |v, cx| {
            v.model.link = Link::Live;
            v.attachments = staged.iter().cloned().map(Chip::ready).collect();
            // Exercise the same completion seam before/after host readiness.
            v.attachments[0].state = Stage::Pending;
            v.local_preview_finished(&staged[0].id, Some(local.clone()), cx);
            assert!(v.attachments[0].preview_source().is_some());
            assert!(
                v.attachments[0].attachment().is_none(),
                "local preview is not Ready"
            );
            v.bump_generation();
            cx.notify();
        });
        window.click("chat-composer", cx);
        window.input("exact draft 🦀  ", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window
                .find(id("attachment-thumbnail", &staged[0].id))
                .visible(),
            "Pending local preview must render"
        );
        assert_eq!(
            window.find(id("attachment-status", &staged[0].id)).label(),
            Some("Staging…")
        );
        assert!(gpui::ImageSource::Image(local.clone()).is_asset_cached(cx));
        view.update(cx, |v, cx| {
            v.staging_finished(&staged[0].id, Ok(staged[0].clone()), cx);
            assert!(v.attachments[0].local_preview.is_none());
            v.local_preview_finished(&staged[0].id, Some(local.clone()), cx);
            assert!(
                v.attachments[0].local_preview.is_none(),
                "late local completion cannot replace Ready"
            );
        });
    })
    .unwrap();
    cx.run_until_parked();
    assert!(!cx.update(|cx| gpui::ImageSource::Image(local.clone()).is_asset_cached(cx)));
    let generation = view.read_with(cx, |v, _| v.editor_generation);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let queue = window.find("chat-attachment-queue");
        assert_eq!(queue.role(), Some(gpui::Role::Group));
        assert_eq!(queue.label(), Some("Draft attachments"));
        assert!(queue.bounds().bottom() <= window.find("chat-composer").bounds().top());
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
fn mixed_image_paste_preview_survives_refusal_and_retry_guards_attempt_identity(
    cx: &mut TestAppContext,
) {
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
    let staged = locally_staged();
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
    let previews = view.read_with(cx, |v, _| {
        v.attachments
            .iter()
            .map(|chip| chip.local_preview.clone().unwrap())
            .collect::<Vec<_>>()
    });
    assert_ne!(
        previews[0].id(),
        previews[1].id(),
        "each local asset has private cache identity"
    );
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    let retried = cx
        .update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window.find("chat-attachment-queue").bounds().bottom()
                    <= window.find("chat-composer").bounds().top()
            );
            for (chip, original) in view.read(cx).attachments.iter().zip(&images) {
                let Stage::Failed(error) = &chip.state else {
                    panic!("expected filename-specific refusal")
                };
                assert!(error.contains(&chip.name));
                assert_eq!(
                    window.find(id("attachment-status", &chip.id)).label(),
                    Some(error.as_str())
                );
                let thumb = window.find(id("attachment-thumbnail", &chip.id));
                assert_eq!(thumb.role(), Some(gpui::Role::Image));
                assert!(thumb.visible());
                assert!(thumb.bounds().size.width <= ui_text::space(56.));
                assert!(thumb.bounds().size.height <= ui_text::space(56.));
                let local = chip
                    .local_preview
                    .as_ref()
                    .expect("refused staging still has a local preview");
                assert_eq!(
                    image::ImageReader::new(Cursor::new(local.bytes()))
                        .with_guessed_format()
                        .unwrap()
                        .into_dimensions()
                        .unwrap(),
                    (256, 192)
                );
                assert!(
                    chip.attachment().is_none(),
                    "preview does not grant Send readiness"
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
            view.update(cx, |v, cx| {
                let next = v.attachments[0].id.clone();
                assert_ne!(next, keys[0], "retry must mint a fresh attempt UUID");
                assert_eq!(
                    v.attachments[1].id, keys[1],
                    "neighbor must remain untouched"
                );
                assert!(matches!(v.attachments[0].state, Stage::Pending));
                let kept = v.attachments[0].local_preview.clone().unwrap();
                // Real completion methods must ignore superseded attempt IDs.
                v.staging_finished(&keys[0], Ok(staged[0].clone()), cx);
                v.local_preview_finished(&keys[0], None, cx);
                assert!(matches!(v.attachments[0].state, Stage::Pending));
                assert!(Arc::ptr_eq(
                    v.attachments[0].local_preview.as_ref().unwrap(),
                    &kept
                ));
                assert!(v.attachments[0].attachment().is_none());
                v.send_message(window, cx);
                assert!(
                    v.pending_submission.is_none(),
                    "Pending preview cannot send"
                );
                next
            })
        })
        .unwrap();
    cx.run_until_parked();
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    view.read_with(cx, |v, cx| {
        assert_eq!(v.composer_text(cx), "plain text paste");
        assert_eq!(v.attachments[0].id, retried);
        assert_eq!(v.attachments[1].id, keys[1]);
        for (chip, original) in v.attachments.iter().zip(&images) {
            let Some(Source::Image { image, .. }) = &chip.source else {
                panic!("source changed on retry")
            };
            assert_eq!(image.as_ref(), original);
            assert!(matches!(chip.state, Stage::Failed(_)));
        }
    });
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |v, cx| {
            v.send_message(window, cx);
            assert!(v.pending_submission.is_none(), "Failed preview cannot send");
        });
        window.click(id("attachment-remove", &retried), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        assert!(!gpui::ImageSource::Image(previews[0].clone()).is_asset_cached(cx));
        assert!(
            gpui::ImageSource::Image(previews[1].clone()).is_asset_cached(cx),
            "neighbor cache must survive removal"
        );
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(view.read(cx).attachments.len(), 1);
        assert_eq!(view.read(cx).attachments[0].id, keys[1]);
        // Removal while its one-shot retry is in flight must not resurrect it.
        window.click(id("attachment-retry", &keys[1]), cx);
        assert!(matches!(view.read(cx).attachments[0].state, Stage::Pending));
        let last = view.read(cx).attachments[0].id.clone();
        assert_ne!(last, keys[1]);
        let local = view.read(cx).attachments[0].local_preview.clone();
        window.click(id("attachment-remove", &last), cx);
        view.update(cx, |v, cx| {
            v.local_preview_finished(&last, local, cx);
            v.staging_finished(&last, Ok(staged[1].clone()), cx);
            assert!(
                v.attachments.is_empty(),
                "removed completion cannot resurrect a chip"
            );
        });
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(attempts.load(Ordering::SeqCst), 4);
    assert!(!cx.update(|cx| gpui::ImageSource::Image(previews[1].clone()).is_asset_cached(cx)));
    view.read_with(cx, |v, cx| {
        assert!(v.attachments.is_empty());
        assert_eq!(v.composer_text(cx), "plain text paste");
    });
    // Actual native mock clipboard path: bad image bytes never acquire a preview.
    let invalid = [
        Image::from_bytes(ImageFormat::Png, b"corrupt PNG".to_vec()),
        Image::from_bytes(ImageFormat::Png, b"GIF89a unsupported".to_vec()),
        Image::from_bytes(ImageFormat::Png, vec![0; FILE_BYTES as usize + 1]),
        Image::from_bytes(ImageFormat::Gif, images[0].bytes().to_vec()),
    ];
    cx.update_window(handle.into(), |_, window, cx| {
        window.click("chat-composer", cx);
        cx.write_to_clipboard(ClipboardItem {
            entries: invalid.into_iter().map(ClipboardEntry::Image).collect(),
        });
        window.press("cmd-v", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(view.read(cx).attachments.len(), 4);
        for chip in &view.read(cx).attachments {
            assert!(matches!(chip.state, Stage::Failed(_)));
            assert!(chip.local_preview.is_none());
            assert!(chip.preview_source().is_none());
            assert!(
                window
                    .try_find(id("attachment-thumbnail", &chip.id))
                    .is_none()
            );
        }
        assert_eq!(view.read(cx).composer_text(cx), "plain text paste");
    })
    .unwrap();
    assert!(recording.try_recv().is_err());
}

#[test]
fn pure_clipboard_thumbnail_keeps_shared_static_image_admission() {
    for format in [image::ImageFormat::Png, image::ImageFormat::Jpeg] {
        let bytes = image_thumbnail(&image_bytes(format)).unwrap();
        assert_eq!(
            image::guess_format(&bytes).unwrap(),
            image::ImageFormat::Png
        );
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (256, 192));
    }
    assert!(image_thumbnail(b"corrupt PNG").is_err());
    assert!(
        image_thumbnail(b"GIF89a unsupported")
            .unwrap_err()
            .contains("unsupported")
    );
    assert!(
        image_thumbnail(&vec![0; FILE_BYTES as usize + 1])
            .unwrap_err()
            .contains("4 MiB")
    );
    // Real, small encoded PNG with an over-limit axis; no forged dimensions.
    let mut wide = Vec::new();
    image::DynamicImage::new_rgb8(8193, 1)
        .write_to(&mut Cursor::new(&mut wide), image::ImageFormat::Png)
        .unwrap();
    assert!(image_thumbnail(&wide).is_err());
}
