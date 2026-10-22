use super::*;
use crate::chat::model::{Item, ItemBody, ItemStatus};
use gpui::{TestAppContext, px};
use gpui_kit::test::TestWindowExt;

fn png(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(width, height, image::Rgba([20, 80, 160, 255]));
    let mut bytes = std::io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    bytes.into_inner()
}
fn rendered() -> Rendered {
    decode(&serde_json::to_vec(&serde_json::json!({"png":base64::engine::general_purpose::STANDARD.encode(png(80,40))})).unwrap()).unwrap()
}

#[test]
fn response_bounds_and_source_limits_fail_without_losing_source() {
    assert!(
        render(&"x".repeat(MAX_SOURCE + 1), false, &AtomicBool::new(false))
            .err()
            .unwrap()
            .contains("32 KiB")
    );
    assert!(
        decode(br#"{"error":"bad\u001b syntax\nline two"}"#)
            .err()
            .unwrap()
            .contains("bad syntax\nline two")
    );
    assert!(decode(b"not JSON").is_err());
    assert!(decode(br#"{"png":"not base64"}"#).is_err());
    assert!(decode(&serde_json::to_vec(&serde_json::json!({"png":base64::engine::general_purpose::STANDARD.encode(png(2049,1))})).unwrap()).err().unwrap().contains("size limit"));
    let valid = rendered();
    assert_eq!(valid.image.size(0).width.0, 80);
    assert_eq!(valid.image.size(0).height.0, 40);
}

#[cfg(unix)]
#[test]
fn cancelled_and_timed_out_workers_are_reaped_even_when_stdin_is_blocked() {
    use std::os::unix::fs::PermissionsExt;
    let path =
        std::env::temp_dir().join(format!("riwork-diagram-timeout-{}", uuid::Uuid::new_v4()));
    std::fs::write(&path, "#!/bin/sh\nsleep 30\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let start = Instant::now();
    let error = render_with(
        &path,
        "flowchart LR; A-->B",
        false,
        &AtomicBool::new(false),
        Duration::from_millis(80),
    )
    .err()
    .unwrap();
    assert!(error.contains("too long"));
    assert!(start.elapsed() < Duration::from_secs(3));
    let error = render_with(
        &path,
        "flowchart LR; A-->B",
        false,
        &AtomicBool::new(true),
        Duration::from_secs(1),
    )
    .err()
    .unwrap();
    assert!(error.contains("cancelled"));
    std::fs::remove_file(path).unwrap();
}

#[gpui::test]
fn native_source_switch_copy_enlarge_and_streaming_preserve_original_source(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| {
        cx.bind_keys([gpui::KeyBinding::new(
            "escape",
            gpui_kit::base::input::Escape,
            Some("ChatView"),
        )])
    });
    let (handle, view, deliveries) = super::super::editor_tests::mount_selection(cx);
    let source = "flowchart LR\n A[Start é] --> B[Done]";
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |view, cx| {
            view.model.link = super::super::Link::Live;
            view.model.transcript.items = vec![Item {
                started_at: None,
                ended_at: None,
                id: "diagram-answer".into(),
                turn_id: Some("turn".into()),
                status: ItemStatus::Completed,
                presentation: crate::chat::model::Presentation {
                    phase: Some(crate::chat::model::MessagePhase::Final),
                    ..Default::default()
                },
                body: ItemBody::AgentMessage {
                    text: format!("```mermaid\n{source}\n```"),
                },
            }];
            let look = Look::of(cx);
            view.diagrams.borrow_mut().entries.push_back(Entry {
                key: "diagram-answer/0".into(),
                source: source.into(),
                dark: crate::theme::is_dark(look.colors.bg),
                revision: view.diagram_revision("diagram-answer/0").unwrap(),
                state: State::Ready(rendered()),
            });
            view.refresh_projection(cx);
            view.list.remeasure();
            cx.notify();
        });
        window.render_frame(cx);
        assert!(
            view.read(cx)
                .diagram_hides_source("code:diagram-answer/0", source, Look::of(cx))
        );
        let look = Look::of(cx);
        assert!(
            !super::super::selection_document::leaves(view.read(cx), look)
                .iter()
                .any(|leaf| leaf.key == "code:diagram-answer/0")
        );
        let hidden = super::super::selection_document::search_source(view.read(cx), look);
        assert!(
            hidden
                .leaves
                .iter()
                .any(|leaf| leaf.key == "code:diagram-answer/0" && leaf.text == source)
        );
        assert!(
            hidden.disclosures["code:diagram-answer/0"].contains(&source_key("diagram-answer/0"))
        );
        window.click("diagram-copy:diagram-answer/0", cx);
        assert_eq!(
            cx.read_from_clipboard().unwrap().text(),
            Some(source.to_owned())
        );
        window.click("diagram-enlarge:diagram-answer/0", cx);
        window.render_frame(cx);
        assert_eq!(
            view.read(cx).media.viewer.as_deref(),
            Some("diagram-answer/0/image/mermaid")
        );
        window.press("escape", cx);
        window.render_frame(cx);
        window.click("diagram-source:diagram-answer/0", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            !view
                .read(cx)
                .diagram_hides_source("code:diagram-answer/0", source, Look::of(cx))
        );
        assert_eq!(
            window.find("transcript:code:diagram-answer/0").label(),
            Some(source)
        );
        window.click("diagram-show:diagram-answer/0", cx);
        window.render_frame(cx);
        assert!(
            view.read(cx)
                .diagram_hides_source("code:diagram-answer/0", source, Look::of(cx))
        );
        view.update(cx, |view, cx| {
            let look = Look::of(cx);
            let original = view.model.transcript.items[0].body.clone();
            // Offscreen rows must not rely on diagram_block running before export.
            view.model.transcript.items[0].body = ItemBody::AgentMessage {
                text: format!("```rust\n{source}\n```"),
            };
            assert!(!view.diagram_hides_source("code:diagram-answer/0", source, look));
            view.model.transcript.items[0].body = original;
            assert!(!view.diagram_hides_source("code:diagram-answer/0", "changed source", look));
            let mut other_theme = look;
            other_theme.colors.bg = if crate::theme::is_dark(look.colors.bg) {
                0xffffff
            } else {
                0
            };
            assert!(!view.diagram_hides_source("code:diagram-answer/0", source, other_theme));
        });
        view.update(cx, |view, cx| {
            view.model.transcript.items[0].status = ItemStatus::InProgress;
            view.transcript_selection.changed();
            view.list.remeasure();
            cx.notify();
        });
        window.render_frame(cx);
        assert!(
            !view
                .read(cx)
                .diagram_hides_source("code:diagram-answer/0", source, Look::of(cx))
        );
        assert!(view.read(cx).diagrams.borrow().entries.is_empty());
        assert!(
            window
                .find("transcript:code:diagram-answer/0")
                .bounds()
                .size
                .height
                > px(0.)
        );
    })
    .unwrap();
    assert!(deliveries.try_recv().is_err());
}

#[gpui::test]
fn completion_uses_the_longest_item_identity_even_with_slashes(cx: &mut TestAppContext) {
    let (_, view, _) = super::super::editor_tests::mount_selection(cx);
    view.update(cx, |view, _| {
        let item = |id: &str, status| Item {
            started_at: None,
            ended_at: None,
            id: id.into(),
            turn_id: None,
            status,
            presentation: Default::default(),
            body: ItemBody::AgentMessage {
                text: String::new(),
            },
        };
        view.model.transcript.items = vec![
            item("answer", ItemStatus::Completed),
            item("answer/nested", ItemStatus::InProgress),
        ];
        assert!(view.diagram_completed("answer/0"));
        assert!(!view.diagram_completed("answer/nested/0"));
        assert!(!view.diagram_completed("plan:answer/nested:document/0"));
        view.model.transcript.items[1].status = ItemStatus::Completed;
        assert!(view.diagram_completed("answer/nested/0"));
        assert!(view.diagram_completed("user:answer/nested/0"));
    });
}
