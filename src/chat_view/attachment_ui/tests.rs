//! Headless ChatView events and actual private, locally staged thumbnails.
//! Recording Feed only; no socket, provider, host startup or native GUI.
use super::*;
use crate::{
    chat::{attachments, client::CallError, model::ChatCommand},
    chat_view::{HostConfig, attachment_draft, editor_tests, feed, state::Link},
};
use gpui::{SharedString, TestAppContext, px};
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
        let card = window.find(id("attachment-card", &staged[0].id));
        assert_eq!(card.role(), Some(gpui::Role::Button));
        let label = format!("Preview {}", staged[0].name);
        assert_eq!(card.label(), Some(label.as_str()));
        assert_eq!(card.expanded(), Some(false));
        // No staged copy yet: the card opens the inline preview instead of Quick Look.
        window.click(id("attachment-card", &staged[0].id), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find(id("attachment-card", &staged[0].id)).expanded(),
            Some(true)
        );
        let expanded = window.find(id("attachment-expanded", &staged[0].id));
        assert!(expanded.visible());
        assert!(expanded.bounds().size.width <= ui_text::space(256.));
        assert!(expanded.bounds().size.height <= ui_text::space(256.));
        assert!(QUICK_LOOKS.with(|q| q.borrow().is_empty()));
        // Actual Base keyboard activation collapses the focused card once.
        window.press("space", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find(id("attachment-card", &staged[0].id)).expanded(),
            Some(false)
        );
        assert!(
            window
                .try_find(id("attachment-expanded", &staged[0].id))
                .is_none()
        );
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
            assert!(thumb.bounds().size.width <= ui_text::space(64.));
            assert!(thumb.bounds().size.height <= ui_text::space(64.));
            let card = window.find(id("attachment-card", &attachment.id));
            assert_eq!(card.role(), Some(gpui::Role::Button));
            assert!(thumb.bounds().top() >= card.bounds().top());
            assert!(thumb.bounds().bottom() <= card.bounds().bottom());
            // Ready opens Quick Look, so it is no disclosure.
            assert_eq!(card.expanded(), None);
            let remove = window.find(id("attachment-remove", &attachment.id));
            assert_eq!(remove.role(), Some(gpui::Role::Button));
            let label = format!("Remove {}", attachment.name);
            assert_eq!(remove.label(), Some(label.as_str()));
            assert!(card.bounds().contains(&remove.bounds().center()));
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
        // The text file is a file card with its size, not a thumbnail.
        assert_eq!(
            window.find(id("attachment-status", &staged[2].id)).label(),
            Some(size_text(staged[2].bytes).as_str())
        );
        window.click(id("attachment-card", &staged[0].id), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            QUICK_LOOKS.with(|q| q.take()),
            vec![(staged[0].path.clone(), "public.png")],
            "Ready opens Quick Look on the staged copy, once"
        );
        assert!(
            window
                .try_find(id("attachment-expanded", &staged[0].id))
                .is_none()
        );
        assert!(!view.read(cx).attachments[0].open);
        assert!(
            window
                .find(id("attachment-thumbnail", &staged[0].id))
                .visible()
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
                assert!(thumb.bounds().size.width <= ui_text::space(64.));
                assert!(thumb.bounds().size.height <= ui_text::space(64.));
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
            let retry = window.find(id("attachment-card", &keys[0]));
            assert_eq!(retry.role(), Some(gpui::Role::Button));
            let label = format!("Retry staging {}", view.read(cx).attachments[0].name);
            assert_eq!(retry.label(), Some(label.as_str()));
            window.click(id("attachment-card", &keys[0]), cx);
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
        window.click(id("attachment-card", &keys[1]), cx);
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

#[test]
fn a_cut_keeps_the_start_and_the_end_as_wide_as_the_room() {
    // One unit per character, a stand-in for a measured width.
    let chars = |text: &str| text.chars().count() as f32;
    assert_eq!(fit_middle("notes", 5., chars), "notes");
    assert_eq!(fit_middle("abcdefghijklmnop", 9., chars), "abcd…mnop");
    assert_eq!(fit_middle("abcdefghijklmnop", 10., chars), "abcde…mnop");
    let crab = "🦀".repeat(30);
    let cut = fit_middle(&crab, 12., chars);
    assert_eq!(cut.chars().count(), 12);
    assert!(cut.contains('…'));
    // No room: the ellipsis alone, never more than the room allows otherwise.
    assert_eq!(fit_middle("abc", 1., chars), "…");
    assert_eq!(fit_middle("abc", 0., chars), "…");
    // Wide glyphs leave fewer characters than narrow ones in the same room.
    let wide = |text: &str| {
        text.chars()
            .map(|c| if c == 'W' { 2. } else { 1. })
            .sum::<f32>()
    };
    assert_eq!(fit_middle("WWWWWWWWWW", 9., wide), "WW…WW");
}

#[test]
fn size_text_scales_to_b_kb_mb() {
    assert_eq!(size_text(0), "0 B");
    assert_eq!(size_text(512), "512 B");
    assert_eq!(size_text(1023), "1023 B");
    assert_eq!(size_text(1024), "1 KB");
    assert_eq!(size_text(4608), "4.5 KB");
    assert_eq!(size_text(36734), "36 KB");
    assert_eq!(size_text(1023 * 1024), "1023 KB");
    assert_eq!(size_text(1024 * 1024 - 1), "1 MB");
    assert_eq!(size_text(1_258_291), "1.2 MB");
    assert_eq!(size_text(FILE_BYTES), "4 MB");
    assert_eq!(size_text(15 * 1024 * 1024), "15 MB");
    assert_eq!(size_text(3 << 30), "3 GB");
}

#[test]
fn file_kind_icon_by_extension() {
    for (name, kind, symbol) in [
        ("photo.PNG", FileKind::Image, "photo"),
        ("report.pdf", FileKind::Pdf, "doc.richtext"),
        (
            "main.rs",
            FileKind::Code,
            "chevron.left.forwardslash.chevron.right",
        ),
        (
            "config.yaml",
            FileKind::Code,
            "chevron.left.forwardslash.chevron.right",
        ),
        ("logs.tar.gz", FileKind::Archive, "doc.zipper"),
        ("notes.md", FileKind::Document, "doc.text"),
        ("Makefile", FileKind::Other, "doc"),
        (".env", FileKind::Other, "doc"),
        ("weird.ext with space", FileKind::Other, "doc"),
    ] {
        assert_eq!(FileKind::of(name), kind, "{name}");
        assert_eq!(kind.symbol(), symbol);
    }
    assert_eq!(extension("logs.tar.GZ").as_deref(), Some("gz"));
    assert_eq!(extension(".env"), None);
    assert_eq!(extension("trailing."), None);
    assert_eq!(extension_label("main.rs"), "RS");
    assert_eq!(extension_label("archive.tgz"), "TGZ");
    assert_eq!(extension_label("diagram.drawio"), "DRAW");
    assert_eq!(extension_label("README"), "FILE");
}

#[test]
fn quick_look_reads_the_staged_kind_under_a_safe_name() {
    assert_eq!(
        content_type(&AttachmentKind::Image {
            mime: "image/jpeg".into()
        }),
        "public.jpeg"
    );
    assert_eq!(
        content_type(&AttachmentKind::Image {
            mime: "image/png".into()
        }),
        "public.png"
    );
    assert_eq!(content_type(&AttachmentKind::Text), "public.plain-text");
    assert_eq!(quick_look_name("photo.png"), "photo.png");
    assert_eq!(quick_look_name("../../etc/passwd"), "passwd");
    assert_eq!(quick_look_name(".."), "attachment");
    assert_eq!(quick_look_name(""), "attachment");
}

#[test]
fn preview_bytes_only_for_bounded_regular_files() {
    let root = std::env::temp_dir().join(format!("riwork-preview-bytes-{}", Uuid::new_v4()));
    fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
    let small = root.join("small.png");
    fs::write(&small, b"bytes").unwrap();
    assert_eq!(read_preview_bytes(&small).as_deref(), Some(&b"bytes"[..]));
    let big = root.join("big.png");
    fs::write(&big, vec![0; FILE_BYTES as usize + 1]).unwrap();
    assert!(read_preview_bytes(&big).is_none());
    assert!(read_preview_bytes(&root).is_none());
    assert!(read_preview_bytes(&root.join("missing.png")).is_none());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn a_name_splits_into_stem_and_extension_as_written() {
    assert_eq!(
        split_name("Report.Final.PDF"),
        ("Report.Final".into(), Some(".PDF".into()))
    );
    assert_eq!(
        split_name("backup.tar.gz"),
        ("backup.tar".into(), Some(".gz".into()))
    );
    // Whatever follows the last dot, any characters, up to its own cap.
    assert_eq!(
        split_name("a.not-an-ext"),
        ("a".into(), Some(".not-an-ext".into()))
    );
    assert_eq!(
        split_name("notes.abcdefghijkl"),
        ("notes".into(), Some(".abcdefghijkl".into()))
    );
    assert_eq!(
        split_name("notes.abcdefghijklmnop"),
        ("notes".into(), Some(".abcdefghijk…".into()))
    );
    assert_eq!(split_name("Makefile"), ("Makefile".into(), None));
    assert_eq!(split_name(".env"), (".env".into(), None));
    assert_eq!(split_name("trailing."), ("trailing.".into(), None));
}

#[gpui::test]
fn a_wide_name_keeps_its_extension_whole_in_cards_of_120_160_and_200(cx: &mut TestAppContext) {
    let root = std::env::temp_dir().join(format!("riwork-wide-name-{}", Uuid::new_v4()));
    fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
    let chat = root.join("chat");
    fs::DirBuilder::new().mode(0o700).create(&chat).unwrap();
    let staged: Vec<Attachment> = [
        format!("{}.txt", "W".repeat(18)),
        "project-backup-2026-10-09-nightly.tar.gz".to_owned(),
        "quarterly-figures.abcdefghijkl".to_owned(),
    ]
    .iter()
    .map(|name| {
        // Staging takes text, PNG and JPEG; a card shows any name, so the staged text file
        // stands in under each one.
        let path = root.join(format!("{}.txt", Uuid::new_v4()));
        fs::write(&path, "wide\n").unwrap();
        let mut staged = attachments::stage(&chat, &path).unwrap();
        staged.name = name.clone();
        staged
    })
    .collect();
    let config = || HostConfig {
        ensure: Arc::new(|| Err("fixture staging is disabled".into())),
    };
    // The window width that gives a card of `target`: the row is the window less the
    // composer's insets, found from a first try.
    let card_at = |cx: &mut TestAppContext, window_width: f32| {
        let (handle, view, _recording) = editor_tests::mount_sized(cx, config(), window_width);
        view.update(cx, |v, cx| {
            v.model.link = Link::Live;
            v.attachments = staged.iter().cloned().map(Chip::ready).collect();
            v.bump_generation();
            cx.notify();
        });
        (handle, view)
    };
    for target in [120.0_f32, 160., 200.] {
        let mut window_width = target + 60.;
        for _ in 0..3 {
            let (handle, _view) = card_at(cx, window_width);
            let mut width = 0.;
            cx.update_window(handle.into(), |_, window, cx| {
                // The first frame measures the composer; the next ones fit the cards to it.
                for _ in 0..3 {
                    window.render_frame(cx);
                }
                width = f32::from(
                    window
                        .find(id("attachment-card", &staged[0].id))
                        .bounds()
                        .size
                        .width,
                );
                if (width - target).abs() > 0.5 {
                    return;
                }
                for attachment in &staged {
                    let what = format!("{} at {target}", attachment.name);
                    let card = window.find(id("attachment-card", &attachment.id)).bounds();
                    let stem = window.find(id("attachment-stem", &attachment.id)).bounds();
                    let ext = window.find(id("attachment-ext", &attachment.id));
                    assert!(ext.visible(), "{what}");
                    let ext = ext.bounds();
                    assert!(ext.size.width > px(0.), "{what}");
                    assert!(ext.left() >= card.left(), "{what}: {ext:?} in {card:?}");
                    // Clear of the × badge's corner, and the stem never runs under it.
                    assert!(
                        ext.right() <= card.right() - ui_text::space(22.) + px(0.5),
                        "{what}: {ext:?} in {card:?}"
                    );
                    assert!(
                        stem.right() <= ext.left() + px(0.5),
                        "{what}: {stem:?} {ext:?}"
                    );
                    assert!(stem.size.width > px(0.), "{what}: {stem:?}");
                }
            })
            .unwrap();
            if (width - target).abs() <= 0.5 {
                window_width = -1.;
                break;
            }
            window_width += target - width;
        }
        assert_eq!(window_width, -1., "no window gave a {target} px card");
    }
    let _ = fs::remove_dir_all(root);
}

#[test]
fn clipboard_staging_owns_normalized_tiff_and_exact_png_jpeg_bytes() {
    use crate::chat::wire::{Request, Response};
    use std::{
        io::{BufRead, BufReader},
        os::unix::net::UnixListener,
        thread,
    };
    let root = crate::chat::testing::private_socket_fixture_home();
    let chat = root.join("chat");
    fs::create_dir(&chat).unwrap();
    let socket = root.join("stage.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let served_chat = chat.clone();
    let server = thread::spawn(move || {
        let mut content = Vec::new();
        for _ in 0..3 {
            let mut stream = crate::chat::testing::bounded_fixture_accept(&listener);
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let Request::StageAttachment { id, chat_id, path } =
                serde_json::from_str(&line).unwrap()
            else {
                panic!("only stage_attachment is allowed")
            };
            assert_eq!(chat_id, "fixture-chat");
            let source_bytes = fs::read(&path).unwrap();
            let attachment = attachments::stage(&served_chat, &path).unwrap();
            assert_eq!(fs::read(&attachment.path).unwrap(), source_bytes);
            let Preview::Image { path: preview } = &attachment.preview else {
                panic!("image preview missing")
            };
            assert_eq!(
                fs::read(preview).unwrap(),
                image_thumbnail(&source_bytes).unwrap()
            );
            writeln!(
                stream,
                "{}",
                serde_json::to_string(&Response {
                    id,
                    ok: true,
                    result: Some(serde_json::to_value(&attachment).unwrap()),
                    error: None
                })
                .unwrap()
            )
            .unwrap();
            content.push((path, source_bytes));
        }
        content
    });
    let ensure: super::super::feed::Ensure = Arc::new(move || Ok(socket.clone()));
    for (format, gpui_format) in [
        (image::ImageFormat::Png, ImageFormat::Png),
        (image::ImageFormat::Jpeg, ImageFormat::Jpeg),
        (image::ImageFormat::Tiff, ImageFormat::Tiff),
    ] {
        let image = Arc::new(Image::from_bytes(gpui_format, image_bytes(format)));
        let original = image.bytes().to_vec();
        let source = Source::Image {
            name: format!("private-clipboard-fixture.{gpui_format:?}"),
            image: image.clone(),
        };
        let staged = stage_source(&ensure, "fixture-chat", &source).unwrap();
        let owned = fs::read(&staged.path).unwrap();
        if format == image::ImageFormat::Tiff {
            assert_eq!(
                image::guess_format(&owned).unwrap(),
                image::ImageFormat::Png
            );
            assert_eq!(
                image::load_from_memory(&owned).unwrap(),
                image::load_from_memory(&original).unwrap()
            );
        } else {
            assert_eq!(owned, original);
        }
        assert_eq!(
            image.bytes(),
            original,
            "source identity and bytes survive normalization"
        );
        // Build actual provider inputs from the owned normalized snapshot,
        // without starting a host or either provider.
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        let mime = if format == image::ImageFormat::Jpeg {
            "image/jpeg"
        } else {
            "image/png"
        };
        let text = "  exact attachment text 🦀  ";
        let codex = attachments::inputs(text, std::slice::from_ref(&staged), false).unwrap();
        let claude = attachments::inputs(text, std::slice::from_ref(&staged), true).unwrap();
        assert_eq!(codex[0]["text"], text);
        assert_eq!(claude[0]["text"], text);
        assert_eq!(codex[1]["text"], format!("Attached image: {}", staged.name));
        let prefix = format!("data:{mime};base64,");
        assert_eq!(
            STANDARD
                .decode(
                    codex[2]["url"]
                        .as_str()
                        .unwrap()
                        .strip_prefix(&prefix)
                        .unwrap()
                )
                .unwrap(),
            owned
        );
        assert_eq!(claude[2]["source"]["media_type"], mime);
        assert_eq!(
            STANDARD
                .decode(claude[2]["source"]["data"].as_str().unwrap())
                .unwrap(),
            owned
        );
        let mut stale = staged.clone();
        stale.fingerprint = "changed fixture descriptor".into();
        assert!(attachments::inputs(text, &[stale], false).is_err());
    }
    for (scratch, _) in server.join().unwrap() {
        assert!(
            !scratch.exists(),
            "only private scratch source is removed after staging"
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[gpui::test]
fn attachment_send_refusal_retains_exact_draft_and_requires_explicit_resend(
    cx: &mut TestAppContext,
) {
    let (handle, view, recording) = editor_tests::mount_selection(cx);
    let attachment = locally_staged().remove(0);
    let text = "  exact refused draft 🦀  ";
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |v, cx| {
            v.attachments.push(Chip::ready(attachment.clone()));
            v.bump_generation();
        });
        window.click("chat-composer", cx);
        window.input(text, cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |v, cx| v.send_message(window, cx));
    })
    .unwrap();
    let feed::Delivery::Submission { id, command } = recording.try_recv().unwrap() else {
        panic!("attachment submission missing")
    };
    assert_eq!(
        command,
        ChatCommand::SendAttachments {
            text: text.into(),
            attachments: vec![attachment.clone()]
        }
    );
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |v, cx| {
            v.submission_receipt(
                id,
                command.clone(),
                Err(CallError::Refused(
                    "running legacy backend does not support attachments; refresh then retry"
                        .into(),
                )),
                window,
                cx,
            );
            assert_eq!(v.composer_text(cx), text);
            assert_eq!(v.attachments[0].attachment(), Some(&attachment));
            assert_eq!(v.submissions[0].snapshot.text, text);
            assert_eq!(
                v.submissions[0].snapshot.attachments,
                vec![attachment.clone()]
            );
            assert!(matches!(v.submissions[0].status, Status::Refused(_)));
            assert!(
                recording.try_recv().is_err(),
                "no automatic retry or text fallback"
            );
            v.resend_snapshot(id, window, cx);
        });
    })
    .unwrap();
    let feed::Delivery::Submission {
        id: retry,
        command: retried,
    } = recording.try_recv().unwrap()
    else {
        panic!("explicit resend missing")
    };
    assert_ne!(retry, id);
    assert_eq!(retried, command);
    assert!(recording.try_recv().is_err());
}

#[test]
fn retained_raw_image_budget_bounds_aggregate_bytes_without_large_allocations() {
    assert!(raw_image_admission([], RAW_TIFF_BYTES).is_ok());
    assert!(
        raw_image_admission([], RAW_TIFF_BYTES + 1)
            .unwrap_err()
            .contains("remove an existing image")
    );
    assert!(raw_image_admission([FILE_BYTES, RAW_TIFF_BYTES - FILE_BYTES - 1], 1).is_ok());
    assert!(
        raw_image_admission([FILE_BYTES, RAW_TIFF_BYTES - FILE_BYTES - 1], 2)
            .unwrap_err()
            .contains("64 MiB")
    );
    assert!(raw_image_admission([RAW_TIFF_BYTES], 1).is_err());
    assert!(
        raw_image_admission([u64::MAX, 1], 1).is_err(),
        "overflow must refuse"
    );
    assert!(raw_image_admission([1; SEND_COUNT - 1], 1).is_ok());
    assert!(
        raw_image_admission([1; SEND_COUNT], 1)
            .unwrap_err()
            .contains("remove an existing image")
    );
}

#[gpui::test]
fn repeated_failed_image_pastes_bound_raw_queue_without_changing_retained_drafts(
    cx: &mut TestAppContext,
) {
    let attempts = Arc::new(AtomicUsize::new(0));
    let count = attempts.clone();
    let (handle, view, recording) = editor_tests::mount_config(
        cx,
        HostConfig {
            ensure: Arc::new(move || {
                count.fetch_add(1, Ordering::SeqCst);
                Err("private refusing Ensure fixture".into())
            }),
        },
    );
    let images = [
        Image::from_bytes(ImageFormat::Png, image_bytes(image::ImageFormat::Png)),
        Image::from_bytes(ImageFormat::Tiff, image_bytes(image::ImageFormat::Tiff)),
    ];
    let mixed = |image: &Image| {
        let mut item = ClipboardItem::new_string("must not replace the draft".into());
        item.entries.push(ClipboardEntry::Image(image.clone()));
        item
    };
    cx.update_window(handle.into(), |_, window, cx| {
        window.click("chat-composer", cx);
        window.input("exact retained draft 🦀  ", cx);
    })
    .unwrap();
    cx.run_until_parked();
    for i in 0..SEND_COUNT {
        cx.update_window(handle.into(), |_, window, cx| {
            cx.write_to_clipboard(mixed(&images[i % 2]));
            window.press("cmd-v", cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(attempts.load(Ordering::SeqCst), i + 1);
    }
    let (keys, previews, generation, tasks) = view.read_with(cx, |v, cx| {
        assert_eq!(v.attachments.len(), SEND_COUNT);
        assert_eq!(v.composer_text(cx), "exact retained draft 🦀  ");
        for (i, chip) in v.attachments.iter().enumerate() {
            assert!(matches!(chip.state, Stage::Failed(_)));
            let Some(Source::Image { image, .. }) = &chip.source else {
                panic!("failed retry source lost")
            };
            assert_eq!(image.as_ref(), &images[i % 2]);
        }
        (
            v.attachments
                .iter()
                .map(|c| c.id.clone())
                .collect::<Vec<_>>(),
            v.attachments
                .iter()
                .map(|c| c.local_preview.clone().unwrap())
                .collect::<Vec<_>>(),
            v.editor_generation,
            v.attachment_tasks.len(),
        )
    });
    for image in &images {
        cx.update_window(handle.into(), |_, window, cx| {
            cx.write_to_clipboard(mixed(image));
            window.press("cmd-v", cx);
        })
        .unwrap();
        cx.run_until_parked();
        view.read_with(cx, |v, cx| {
            assert_eq!(
                v.attachments.len(),
                SEND_COUNT,
                "no over-budget Failed source may be added"
            );
            assert_eq!(v.editor_generation, generation);
            assert_eq!(
                v.attachment_tasks.len(),
                tasks,
                "refusal starts no preview/staging task"
            );
            assert!(
                v.notices.local(super::super::notices::LocalKey::Attachment)
                    .unwrap()
                    .contains("remove an existing image"),
                "mixed text must not hide admission notice"
            );
            assert_eq!(v.composer_text(cx), "exact retained draft 🦀  ");
            for (i, chip) in v.attachments.iter().enumerate() {
                assert_eq!(chip.id, keys[i]);
                let Some(Source::Image { image, .. }) = &chip.source else {
                    panic!("existing raw source changed")
                };
                assert_eq!(image.as_ref(), &images[i % 2]);
                assert!(Arc::ptr_eq(
                    chip.local_preview.as_ref().unwrap(),
                    &previews[i]
                ));
            }
        });
        assert_eq!(attempts.load(Ordering::SeqCst), SEND_COUNT);
    }
    // Retry is a replacement of the same retained raw source, not an admission.
    let retried = cx
        .update_window(handle.into(), |_, window, cx| {
            view.update(cx, |v, cx| {
                v.retry_staging(&keys[0], window, cx);
                let retried = v.attachments[0].id.clone();
                assert_ne!(retried, keys[0]);
                assert!(matches!(v.attachments[0].state, Stage::Pending));
                let generation = v.editor_generation;
                let tasks = v.attachment_tasks.len();
                assert!(v.paste_attachments(&mixed(&images[0]), window, cx));
                assert_eq!(
                    v.editor_generation, generation,
                    "Pending sources also consume budget"
                );
                assert_eq!(v.attachment_tasks.len(), tasks);
                assert_eq!(v.attachments.len(), SEND_COUNT);
                v.staging_finished(&keys[0], Err("stale attempt".into()), cx);
                assert!(matches!(v.attachments[0].state, Stage::Pending));
                retried
            })
        })
        .unwrap();
    cx.run_until_parked();
    assert_eq!(attempts.load(Ordering::SeqCst), SEND_COUNT + 1);
    let ready = locally_staged().remove(0);
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |v, cx| {
            v.remove_attachment(&retried, cx);
            v.staging_finished(&retried, Ok(ready.clone()), cx);
            v.local_preview_finished(&retried, Some(previews[0].clone()), cx);
            assert_eq!(
                v.attachments.len(),
                SEND_COUNT - 1,
                "late removed attempt cannot return"
            );
            assert_eq!(v.attachments[0].id, keys[1]);
            assert!(Arc::ptr_eq(
                v.attachments[0].local_preview.as_ref().unwrap(),
                &previews[1]
            ));
            assert!(v.paste_attachments(&mixed(&images[0]), window, cx));
            assert_eq!(
                v.attachments.len(),
                SEND_COUNT,
                "removal restores admission capacity"
            );
        });
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(attempts.load(Ordering::SeqCst), SEND_COUNT + 2);
    // Source::File paths keep their existing admission and filename semantics.
    let files = [
        PathBuf::from("/private/fixture/first.txt"),
        PathBuf::from("/private/fixture/second.txt"),
    ];
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |v, cx| {
            v.stage_sources(
                files.iter().cloned().map(Source::File).collect(),
                window,
                cx,
            )
        });
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(attempts.load(Ordering::SeqCst), SEND_COUNT + 4);
    view.read_with(cx, |v, cx| {
        assert_eq!(v.attachments.len(), SEND_COUNT + files.len());
        for (chip, path) in v.attachments[SEND_COUNT..].iter().zip(&files) {
            assert_eq!(chip.name, path.file_name().unwrap().to_str().unwrap());
            assert!(matches!(&chip.source, Some(Source::File(kept)) if kept == path));
        }
        assert_eq!(v.composer_text(cx), "exact retained draft 🦀  ");
    });
    // A successful staging receipt retires its raw source. Ready snapshots and
    // File paths must not consume the newly available raw-image admission slot.
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |v, cx| {
            v.retry_staging(&keys[1], window, cx);
            let staged_attempt = v.attachments[0].id.clone();
            v.staging_finished(&staged_attempt, Ok(ready.clone()), cx);
            assert!(v.attachments[0].source.is_none());
            assert!(v.paste_attachments(&mixed(&images[1]), window, cx));
            assert_eq!(v.attachments.len(), SEND_COUNT + files.len() + 1);
        });
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(attempts.load(Ordering::SeqCst), SEND_COUNT + 6);
    view.read_with(cx, |v, cx| {
        assert!(
            matches!(v.attachments[0].state, Stage::Ready(_)),
            "late failed retry cannot overwrite Ready"
        );
        assert_eq!(
            v.attachments
                .iter()
                .filter(|chip| matches!(&chip.source, Some(Source::Image { .. })))
                .count(),
            SEND_COUNT
        );
        assert_eq!(v.composer_text(cx), "exact retained draft 🦀  ");
    });
    assert!(
        recording.try_recv().is_err(),
        "refused raw sources never dispatch a message"
    );
}
