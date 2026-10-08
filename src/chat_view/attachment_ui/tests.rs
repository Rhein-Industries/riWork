//! Headless ChatView events and actual private, locally staged thumbnails.
//! Recording Feed and explicitly private socket stand-ins; no provider, host startup or GUI.
use super::*;
use crate::{
    chat::{
        attachments::{self, FILE_BYTES},
        client::CallError,
        model::ChatCommand,
    },
    chat_view::{HostConfig, editor_tests, feed},
};
use gpui::TestAppContext;
use gpui_kit::test::TestWindowExt;
use std::{
    io::Cursor,
    sync::atomic::{AtomicUsize, Ordering},
};

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
                v.notice
                    .as_ref()
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
