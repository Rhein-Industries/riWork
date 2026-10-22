use super::*;
use crate::chat_view::{editor_tests, image_scope::tests::Fixture};
use gpui_kit::test::TestWindowExt;

fn pixels(color: [u8; 4]) -> Arc<RenderImage> {
    let rgba = image::RgbaImage::from_pixel(3, 2, image::Rgba(color));
    let mut bytes = std::io::Cursor::new(Vec::new());
    rgba.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    match file_preview::decode_image(bytes.get_ref()).unwrap() {
        PreviewContent::Image { image, .. } => image,
        _ => panic!("expected image"),
    }
}

#[test]
fn exported_png_preserves_preview_colors_alpha_and_dimensions() {
    let png = png_snapshot(&pixels([230, 40, 10, 128])).unwrap();
    let decoded = image::load_from_memory(&png).unwrap().to_rgba8();
    assert_eq!(decoded.dimensions(), (3, 2));
    assert!(decoded.pixels().all(|pixel| pixel.0 == [230, 40, 10, 128]));
    assert_eq!(
        export_name(&ImageSource::Url {
            url: "https://example.invalid/my%20photo.jpg?token=secret".into()
        }),
        "my photo-preview.png"
    );
}

#[test]
fn originals_reject_changed_missing_symlink_and_outside_files() {
    let fixture = Fixture::new();
    let scope = fixture.alpha_chat();
    let shown = pixels([200, 40, 10, 255]);
    assert!(local_original(&scope, "shot.png", &shown).is_ok());
    assert!(local_original(&scope, "link.png", &shown).is_err());
    assert!(local_original(&scope, fixture.beta.join("b.png").to_str().unwrap(), &shown).is_err());
    assert!(local_original(&scope, "missing.png", &shown).is_err());
    image::RgbaImage::from_pixel(3, 2, image::Rgba([1u8, 2, 3, 255]))
        .save(fixture.alpha.join("shot.png"))
        .unwrap();
    assert!(
        local_original(&scope, "shot.png", &shown)
            .unwrap_err()
            .contains("changed")
    );
}

#[gpui::test]
fn image_copy_and_cancelled_save_use_displayed_snapshot_without_submission(
    cx: &mut gpui::TestAppContext,
) {
    let fixture = Fixture::new();
    let (handle, view, recording) = editor_tests::mount_selection(cx);
    let expected = pixels([230, 40, 10, 128]);
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |view, cx| {
            view.media.slots.insert(
                "shot".into(),
                Slot {
                    generation: 42,
                    source: ImageSource::Data {
                        mime: "image/png".into(),
                        base64: String::new(),
                    },
                    content: Some(Ok(PreviewContent::Image {
                        image: expected.clone(),
                        description: "snapshot".into(),
                    })),
                    in_files: false,
                },
            );
            view.media.show("shot".into());
            view.composer
                .update(cx, |state, cx| state.set_value("keep my draft", window, cx));
            cx.notify();
        });
        window.render_frame(cx);
        window.click("image-copy:viewer:shot", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.read(|cx| {
        let clipboard = cx.read_from_clipboard().unwrap();
        let gpui::ClipboardEntry::Image(image) = &clipboard.entries()[0] else {
            panic!("expected image clipboard");
        };
        let decoded = image::load_from_memory(image.bytes()).unwrap().to_rgba8();
        assert_eq!(decoded.get_pixel(0, 0).0, [230, 40, 10, 128]);
        assert_eq!(view.read(cx).composer_text(cx), "keep my draft");
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("image-save:viewer:shot", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert!(cx.did_prompt_for_new_path());
    cx.simulate_new_path_selection(|_| None);
    cx.run_until_parked();
    let path = fixture.base.join("export.png");
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("image-save:viewer:shot", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.simulate_new_path_selection(|_| Some(path.clone()));
    cx.run_until_parked();
    assert_eq!(
        std::fs::read(path).unwrap(),
        png_snapshot(&expected).unwrap()
    );
    assert!(recording.try_recv().is_err());
}

#[gpui::test]
fn late_image_action_cannot_replace_clipboard_after_rescope(cx: &mut gpui::TestAppContext) {
    let (handle, view, _) = editor_tests::mount_selection(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string("keep clipboard".into()));
        view.update(cx, |view, cx| {
            view.media.slots.insert(
                "shot".into(),
                Slot {
                    generation: 1,
                    source: ImageSource::Local {
                        path: "shot.png".into(),
                    },
                    content: Some(Ok(PreviewContent::Image {
                        image: pixels([20, 30, 40, 255]),
                        description: "shot".into(),
                    })),
                    in_files: true,
                },
            );
            view.image_action("shot", 1, Action::Copy, window, cx);
            view.media.slots.clear();
        });
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        cx.read(|cx| cx.read_from_clipboard().unwrap().text()),
        Some("keep clipboard".into())
    );
    assert!(!cx.read(|cx| view.read(cx).media.action_pending));
}

#[gpui::test]
fn closing_original_window_releases_pending_image_action(cx: &mut gpui::TestAppContext) {
    let (handle, view, _) = editor_tests::mount_selection(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |view, cx| {
            view.media.slots.insert(
                "shot".into(),
                Slot {
                    generation: 1,
                    source: ImageSource::Data {
                        mime: "image/png".into(),
                        base64: String::new(),
                    },
                    content: Some(Ok(PreviewContent::Image {
                        image: pixels([20, 30, 40, 255]),
                        description: "shot".into(),
                    })),
                    in_files: false,
                },
            );
            view.image_action("shot", 1, Action::Save, window, cx);
        });
        window.remove_window();
    })
    .unwrap();
    cx.run_until_parked();
    assert!(!cx.read(|cx| view.read(cx).media.action_pending));
    assert!(!cx.did_prompt_for_new_path());
}
