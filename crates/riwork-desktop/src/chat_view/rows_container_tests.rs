use super::*;
use crate::chat::model::{ItemStatus, LineRange, Presentation, Semantics, ToolKind};
use crate::chat_view::{ChatViewEvent, DisplayMode, ImageScope, editor_tests, state, testing};
use gpui::{AppContext, TestAppContext};
use gpui_kit::test::TestWindowExt;

fn item(id: &str, body: ItemBody) -> Item {
    Item {
        id: id.into(),
        turn_id: Some("turn".into()),
        status: ItemStatus::Completed,
        presentation: Presentation::default(),
        started_at: Some(1_000),
        ended_at: Some(2_200),
        body,
    }
}

#[gpui::test]
fn shell_container_aligns_with_title_caps_output_and_copies_full_stored_text(
    cx: &mut TestAppContext,
) {
    let old = ui_text::set_for_tests(1.0, ui_text::Face::Menlo);
    let (handle, view, _) = editor_tests::mount_selection(cx);
    let output = (0..35)
        .map(|n| {
            if n == 2 {
                "error: failed step".into()
            } else {
                format!("line {n}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let full = output.clone();
    let command = "printf first_with_a_long_argument_for_preview_headers_and_resizing_without_source_export_drift\nprintf second";
    view.update(cx, |view, cx| {
        view.model.link = state::Link::Live;
        view.model.transcript.items = vec![item(
            "cmd",
            ItemBody::Command {
                command: command.into(),
                cwd: Some("/tmp/project".into()),
                output,
                exit_code: Some(1),
            },
        )];
        view.display_mode = DisplayMode::Verbose;
        view.refresh_projection(cx);
        view.expand_transcript(true, cx);
    });
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let card = window.find("card:cmd").bounds();
        let container = window.find("output-block:cmd").bounds();
        assert_eq!(container.left(), card.left() + text_indent());
        assert_eq!(container.right(), card.right());
        assert_eq!(container.left() - card.left(), text_indent());
        assert!(window.find("transcript:command:cmd").bounds().size.height <= ui_text::space(18.0));
        assert!(window.try_find("output-footer:cmd").is_some());
        let body = window.find("output:cmd").bounds();
        assert!(body.size.height <= ui_text::space(320.0));
        assert_eq!(
            window.find("output-line:output:cmd:2").bounds().size.width,
            body.size.width
        );
        assert_eq!(window.find("copy:cmd").label(), Some("Copy output"));
        assert_eq!(
            window.find("copy:cmd").bounds().size,
            gpui::size(ui_text::space(20.0), ui_text::space(20.0))
        );
        assert!(!window.find("copy:cmd").visible(), "copy is hidden at rest");
        window.press("down", cx);
        assert!(
            window.find("copy:cmd").visible(),
            "arrow focus reveals the row action"
        );
        view.update(cx, |view, cx| {
            view.row_keyboard_focus = false;
            cx.notify();
        });
        window.render_frame(cx);
        assert!(!window.find("copy:cmd").visible());
        window.hover("output-block:cmd", cx);
        assert!(
            window.find("copy:cmd").visible(),
            "hover reveals the row action"
        );
        window.click("copy:cmd", cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|c| c.text()),
            Some(full.clone())
        );
        window.click("show-all:cmd", cx);
        assert_eq!(window.find("show-all:cmd").label(), Some("Show less"));
        assert!(window.find("output:cmd").bounds().size.height <= ui_text::space(320.0));
        window.click("show-all:cmd", cx);
        assert_eq!(window.find("show-all:cmd").label(), Some("Show all"));
        window.right_click("output-header:cmd", cx);
        window.click("copy-command:cmd", cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|c| c.text()),
            Some(command.into())
        );
        window.resize(gpui::size(px(500.0), px(800.0)));
        for _ in 0..3 {
            window.render_frame(cx);
        }
        assert!(
            window
                .find("transcript:command:cmd")
                .label()
                .unwrap()
                .contains('…')
        );
    })
    .unwrap();
    view.update(cx, |view, cx| {
        let command = &mut view.model.transcript.items[0];
        command.status = ItemStatus::InProgress;
        command.started_at = Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64
                - 7_000,
        );
        command.ended_at = None;
        if let ItemBody::Command {
            output, exit_code, ..
        } = &mut command.body
        {
            output.clear();
            *exit_code = None;
        }
        view.refresh_projection(cx);
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("output-footer:cmd").is_none());
        assert!(window.try_find("output-placeholder:output:cmd").is_some());
        assert!(window.try_find("transcript:output:cmd:0").is_none());
    })
    .unwrap();
    ui_text::set_for_tests(old.0, old.1);
}

#[gpui::test]
fn file_excerpt_strips_numbering_copies_full_excerpt_and_opens_at_first_line(
    cx: &mut TestAppContext,
) {
    let old = ui_text::set_for_tests(1.0, ui_text::Face::Menlo);
    let root = std::env::temp_dir().join(format!("riwork-excerpt-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("source.rs"), "fixture\n").unwrap();
    let (handle, view, _) = editor_tests::mount_selection(cx);
    let mut info = testing::info("chat");
    info.cwd = root.clone();
    let project_id = uuid::Uuid::new_v4().to_string();
    info.project_id = Some(project_id.clone());
    let mut store = crate::store::State::default();
    store.projects.push(
        serde_json::from_value(serde_json::json!({
            "id": project_id, "name":"Excerpt", "root":root, "created_at":1,
        }))
        .unwrap(),
    );
    let scope = ImageScope::for_chat(&info, None, &store, &root.join("home"), Some(root.clone()));
    let output = (12..32)
        .map(|n| match n {
            13 => "13\talpha".into(),
            14 => "123 words".into(),
            _ => format!("{n}→line {n}"),
        })
        .collect::<Vec<_>>()
        .join("\n");
    let copied = (12..32)
        .map(|n| match n {
            13 => "alpha".into(),
            14 => "123 words".into(),
            _ => format!("line {n}"),
        })
        .collect::<Vec<_>>()
        .join("\n");
    let preview_target = std::rc::Rc::new(std::cell::RefCell::new(None));
    let recorded = preview_target.clone();
    let _subscription = cx.update(|cx| {
        cx.subscribe(&view, move |_, event: &ChatViewEvent, _| {
            if let ChatViewEvent::OpenFile { target } = event {
                *recorded.borrow_mut() = Some(target.clone());
            }
        })
    });
    view.update(cx, |view, cx| {
        view.model.link = state::Link::Live;
        view.model.transcript.info = Some(info);
        let mut read = item(
            "read",
            ItemBody::ToolCall {
                server: None,
                tool: "Read".into(),
                input: serde_json::json!({}),
                output: Some(output),
            },
        );
        read.presentation.semantics = Some(Semantics {
            kind: ToolKind::Read,
            target: Some("source.rs".into()),
            range: Some(LineRange {
                start: 12,
                end: Some(31),
            }),
            ..Default::default()
        });
        view.model.transcript.items = vec![read];
        view.set_image_scope(scope, cx);
        view.display_mode = DisplayMode::Verbose;
        view.refresh_projection(cx);
        view.expand_transcript(true, cx);
    });
    // The first frame schedules availability; the next turn receives the off-window result.
    cx.update_window(handle.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let container = window.find("output-block:read").bounds();
        assert_eq!(
            container.left(),
            window.find("card:read").bounds().left() + text_indent()
        );
        assert_eq!(container.right(), window.find("card:read").bounds().right());
        assert_eq!(
            window.find("transcript:tool-output:read:0").label(),
            Some("line 12")
        );
        assert_eq!(
            window.find("transcript:tool-output:read:1").label(),
            Some("alpha")
        );
        assert_eq!(
            window.find("transcript:tool-output:read:2").label(),
            Some("123 words")
        );
        assert!(window.try_find("excerpt-gutter:read:0").is_some());
        assert!(window.try_find("excerpt-line:read:15").is_none());
        assert_ne!(window.find("open-excerpt:read").disabled(), Some(true));
        assert_eq!(
            window.find("open-excerpt:read").role(),
            Some(gpui::Role::Button)
        );
        window.hover("output-block:read", cx);
        window.click("copy:read", cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|c| c.text()),
            Some(copied)
        );
        window.click("open-excerpt:read", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(preview_target.borrow().as_deref(), Some("source.rs:12"));
    preview_target.borrow_mut().take();
    let requests = view.read_with(cx, |view, _| view.caches.borrow().excerpt_open_request);
    assert_eq!(
        view.read_with(cx, |view, _| view.caches.borrow().excerpt_open["read"]
            .available),
        Some(true)
    );
    // A redraw does not probe the filesystem again or change the container kind.
    std::fs::remove_file(root.join("source.rs")).unwrap();
    cx.update_window(handle.into(), |_, window, cx| {
        for _ in 0..4 {
            window.render_frame(cx);
        }
        assert!(window.try_find("excerpt-header:read").is_some());
    })
    .unwrap();
    assert_eq!(
        view.read_with(cx, |view, _| view.caches.borrow().excerpt_open_request),
        requests
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.caches.borrow().excerpt_open["read"]
            .available),
        Some(true)
    );
    view.update(cx, |view, cx| {
        view.model.transcript.items[0].status = ItemStatus::InProgress;
        view.model.transcript.items[0]
            .presentation
            .semantics
            .as_mut()
            .unwrap()
            .target = Some("missing.rs".into());
        view.refresh_projection(cx);
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("open-excerpt:read").label(),
            Some("Open in viewer")
        );
        window.hover("output-block:read", cx);
        window.click("open-excerpt:read", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        view.read_with(cx, |view, _| view.caches.borrow().excerpt_open_request),
        requests + 1
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.caches.borrow().excerpt_open["read"]
            .available),
        Some(false)
    );
    assert!(
        preview_target.borrow().is_none(),
        "an unavailable file keeps a visible, inert Open action"
    );
    // Completion can make a produced file available without changing the target.
    std::fs::write(root.join("missing.rs"), "newly produced file").unwrap();
    view.update(cx, |view, cx| {
        view.model.transcript.items[0].status = ItemStatus::Completed;
        view.refresh_projection(cx);
    });
    cx.update_window(handle.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(
        view.read_with(cx, |view, _| view.caches.borrow().excerpt_open_request),
        requests + 2
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.caches.borrow().excerpt_open["read"]
            .available),
        Some(true)
    );
    // A file-shaped target does not turn searches or listings into excerpts.
    for kind in [ToolKind::Search, ToolKind::List] {
        view.update(cx, |view, cx| {
            view.model.transcript.items[0]
                .presentation
                .semantics
                .as_mut()
                .unwrap()
                .kind = kind;
            view.refresh_projection(cx);
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("excerpt-header:read").is_none(), "{kind:?}");
            assert!(
                window.try_find("excerpt-gutter:read:0").is_none(),
                "{kind:?}"
            );
            assert!(window.try_find("open-excerpt:read").is_none(), "{kind:?}");
            assert!(
                window
                    .find("transcript:tool-output:read:run:0")
                    .label()
                    .is_some_and(|text| text.starts_with("12→line 12")),
                "{kind:?} keeps the original output"
            );
        })
        .unwrap();
    }
    view.update(cx, |view, cx| {
        view.set_image_scope(ImageScope::default(), cx);
        assert!(view.caches.borrow().excerpt_open.is_empty());
    });
    std::fs::remove_dir_all(root).unwrap();
    ui_text::set_for_tests(old.0, old.1);
}
