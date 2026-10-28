//! Real GPUI routing for the chat turn's preview sequence and native viewer zoom.
use super::*;
use crate::{
    chat::{
        attachments::{Attachment, AttachmentKind, Preview},
        model::{
            ChatImage, ChatInfo, ImageSource, Item, ItemStatus, LineRange, MessagePhase, PathRef,
            Presentation, Semantics, ToolKind,
        },
    },
    file_explorer::{ExplorerRoot, FileExplorer, FileExplorerEvent, FilePreview},
    file_preview::{PreviewEntry, PreviewSequence},
};
use gpui::TestAppContext;
use gpui_kit::test::TestWindowExt;
use std::path::{Path, PathBuf};

fn item(id: &str, turn: &str, body: ItemBody) -> Item {
    Item {
        id: id.into(),
        turn_id: Some(turn.into()),
        status: ItemStatus::Completed,
        body,
        presentation: Presentation::default(),
        started_at: None,
        ended_at: None,
    }
}
fn attachment(name: &str, image: bool) -> Attachment {
    Attachment {
        id: name.into(),
        name: name.into(),
        path: PathBuf::from(format!("/tmp/m2-fixture/{name}")),
        kind: if image {
            AttachmentKind::Image {
                mime: "image/png".into(),
            }
        } else {
            AttachmentKind::Text
        },
        bytes: 42,
        fingerprint: "fixture".into(),
        preview: if image {
            Preview::Image {
                path: PathBuf::from(format!("/tmp/m2-fixture/{name}")),
            }
        } else {
            Preview::Text {
                excerpt: "fixture".into(),
            }
        },
    }
}

#[gpui::test]
fn viewer_sequence_uses_current_turn_attachments_first_and_recorded_provenance(
    cx: &mut TestAppContext,
) {
    let (_, view, _) = editor_tests::mount_selection(cx);
    view.update(cx, |view, cx| {
        let mut user = item(
            "user",
            "current",
            ItemBody::UserMessage {
                text: "Inspect".into(),
            },
        );
        user.presentation.attachments = vec![
            attachment("screen.png", true),
            attachment("notes.md", false),
        ];
        let mut read = item(
            "read",
            "current",
            ItemBody::ToolCall {
                server: None,
                tool: "read_file".into(),
                input: serde_json::json!({"path":"src/main.rs"}),
                output: Some("source".into()),
            },
        );
        read.presentation.semantics = Some(Semantics {
            kind: ToolKind::Read,
            target: Some("src/main.rs".into()),
            range: Some(LineRange {
                start: 12,
                end: Some(20),
            }),
            ..Semantics::default()
        });
        let mut produced = item(
            "produce",
            "current",
            ItemBody::Command {
                command: "render-report".into(),
                cwd: None,
                output: "Done".into(),
                exit_code: Some(0),
            },
        );
        produced.presentation.semantics = Some(Semantics {
            kind: ToolKind::Shell,
            produced: vec![PathRef {
                path: "report.md".into(),
                line: None,
            }],
            ..Semantics::default()
        });
        produced.presentation.images.push(ChatImage {
            label: "result.png".into(),
            source: ImageSource::Local {
                path: "result.png".into(),
            },
        });
        let mut answer = item(
            "answer",
            "current",
            ItemBody::AgentMessage {
                text: "See src/main.rs:12 and report.md.".into(),
            },
        );
        answer.presentation.phase = Some(MessagePhase::Final);
        view.model.transcript.items = vec![
            item(
                "older",
                "older",
                ItemBody::AgentMessage {
                    text: "old.txt".into(),
                },
            ),
            user,
            read,
            produced,
            answer,
        ];
        view.refresh_projection(cx);
        let exact_line = view.file_preview_sequence("src/main.rs:17");
        assert!(exact_line.current().unwrap().target.ends_with(":17"));
        let sequence = view.file_preview_sequence("report.md");
        assert_eq!(
            sequence
                .entries
                .iter()
                .map(|entry| entry.title.as_str())
                .collect::<Vec<_>>(),
            [
                "screen.png",
                "notes.md",
                "main.rs",
                "report.md",
                "result.png"
            ]
        );
        assert_eq!(sequence.index, 3);
        assert_eq!(sequence.entries[0].source, "Your message");
        assert_eq!(sequence.entries[1].source, "Your message");
        assert_eq!(sequence.entries[2].target, "src/main.rs:12");
        assert!(sequence.entries[4].image.is_some());
        assert_eq!(sequence.current().unwrap().target, "report.md");
        let before = sequence.clone();
        view.apply_display_mode(DisplayMode::Verbose, cx);
        view.expand_transcript(true, cx);
        assert_eq!(
            view.file_preview_sequence("report.md"),
            before,
            "paging is independent of display mode and disclosure"
        );
    });
}

struct NativeFace((f32, ui_text::Face));
impl NativeFace {
    fn new() -> Self {
        Self(ui_text::set_for_tests(1., ui_text::Face::SystemMono))
    }
}
impl Drop for NativeFace {
    fn drop(&mut self) {
        ui_text::set_for_tests(self.0.0, self.0.1);
    }
}
struct ViewerFixture {
    explorer: Entity<FileExplorer>,
    preview: Entity<FilePreview>,
    closes: usize,
    _subscription: Subscription,
}
impl Render for ViewerFixture {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.preview.clone()
    }
}

fn scope(root: &Path) -> ImageScope {
    let project_id = uuid::Uuid::new_v4().to_string();
    let info: ChatInfo = serde_json::from_value(serde_json::json!({
        "id":uuid::Uuid::new_v4().to_string(), "provider":"codex", "project_id":project_id,
        "cwd":root, "title":"Viewer test", "created_at_unix":1,
    }))
    .unwrap();
    let mut state = crate::store::State::default();
    state.projects.push(
        serde_json::from_value(serde_json::json!({
            "id":project_id, "name":"Viewer fixture", "root":root, "created_at":1,
        }))
        .unwrap(),
    );
    ImageScope::for_chat(
        &info,
        None,
        &state,
        &root.join("test-home"),
        Some(root.to_owned()),
    )
}

#[gpui::test]
fn native_viewer_routes_zoom_paging_and_escape_without_touching_the_composer(
    cx: &mut TestAppContext,
) {
    let _face = NativeFace::new();
    let (chat_window, chat, _recording) = editor_tests::mount_selection(cx);
    cx.update(|cx| {
        cx.set_global(crate::theme::Appearance {
            selected: crate::theme::ThemeChoice::Native,
            palette: crate::theme::Palette::NATIVE_LIGHT,
            terminal: None,
            ghostty: None,
        });
    });
    chat.update(cx, |chat, cx| {
        chat.model.link = state::Link::Live;
        chat.model.transcript.info = Some(testing::info("chat"));
        cx.notify();
    });
    let chat_toolbar_geometry = cx
        .update_window(chat_window.into(), |_, window, cx| {
            for _ in 0..4 {
                window.render_frame(cx);
            }
            (
                window.find("chat-toolbar").bounds().size.height,
                window.find("chat-more").bounds().size,
                window.find("chat-mode").bounds().size.height,
            )
        })
        .unwrap();
    let root = std::env::temp_dir().join(format!("riwork-m2-viewer-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("notes.txt"), "one\ntwo\nthree\n").unwrap();
    image::RgbaImage::new(8, 4)
        .save(root.join("image.png"))
        .unwrap();
    let mount_root = root.clone();
    let (handle, fixture) = crate::form_input::test_window(cx, move |window, cx| {
        cx.set_global(crate::theme::Appearance {
            selected: crate::theme::ThemeChoice::Native,
            palette: crate::theme::Palette::NATIVE_LIGHT,
            terminal: None,
            ghostty: None,
        });
        let explorer = cx.new(|cx| FileExplorer::new(window, cx));
        explorer.update(cx, |view, cx| {
            view.set_root(
                Some(ExplorerRoot {
                    path: mount_root.clone(),
                    label: "Fixture".into(),
                    worktree_id: None,
                }),
                window,
                cx,
            );
            view.show_chat_sequence(
                PreviewSequence {
                    entries: vec![
                        PreviewEntry {
                            target: mount_root.join("notes.txt").to_string_lossy().into_owned(),
                            title: "notes.txt".into(),
                            source: "Your message".into(),
                            image: None,
                            attachment: true,
                        },
                        PreviewEntry {
                            target: mount_root.join("image.png").to_string_lossy().into_owned(),
                            title: "image.png".into(),
                            source: "Ran render".into(),
                            image: Some(ChatImage {
                                label: "image.png".into(),
                                source: ImageSource::Local {
                                    path: mount_root
                                        .join("image.png")
                                        .to_string_lossy()
                                        .into_owned(),
                                },
                            }),
                            attachment: false,
                        },
                    ],
                    index: 0,
                },
                scope(&mount_root),
                cx,
            );
        });
        let preview = cx.new(|cx| FilePreview::new(explorer.clone(), cx));
        let subscription = cx.subscribe(&explorer, |fixture: &mut ViewerFixture, _, event, _| {
            if matches!(event, FileExplorerEvent::ClosePreview) {
                fixture.closes += 1;
            }
        });
        ViewerFixture {
            explorer,
            preview,
            closes: 0,
            _subscription: subscription,
        }
    });
    cx.update(|cx| cx.bind_keys(crate::text_and_preview_zoom_bindings()));
    let explorer = fixture.read_with(cx, |fixture, _| fixture.explorer.clone());
    cx.run_until_parked();
    crate::form_input::test_turn(cx, handle, |window, app| {
        explorer.update(app, |view, cx| view.focus_preview(window, cx));
        window.render_frame(app);
        assert_eq!(
            window.find("preview-toolbar").bounds().size.height,
            chat_toolbar_geometry.0,
            "preview and chat use the same toolbar height"
        );
        assert_eq!(
            window.find("preview-close").bounds().size,
            chat_toolbar_geometry.1,
            "preview actions use the chat toolbar's round symbol-button geometry"
        );
        assert_eq!(
            window.find("preview-zoom-fit").bounds().size.height,
            chat_toolbar_geometry.2,
            "preview zoom and chat choices use the same capsule geometry and typography"
        );
        assert!(window.try_find("preview-previous-file").is_some());
        assert!(
            window.find("preview-code-line:0").bounds().size.height > px(0.0),
            "the actual text rows must occupy the preview viewport"
        );
        window.click("preview-zoom-in", app);
        assert!(!explorer.read(app).preview_state_for_test().0.fit);
        window.click("preview-zoom-fit", app);
        assert!(explorer.read(app).preview_state_for_test().0.fit);
        window.press("left", app);
        assert_eq!(
            explorer.read(app).preview_state_for_test().1.unwrap().index,
            0
        );
        window.press("cmd-=", app);
        let (zoom, _) = explorer.read(app).preview_state_for_test();
        assert!(!zoom.fit);
        assert!((zoom.scale - 1.2).abs() < 0.001);
        window.press("cmd--", app);
        assert!((explorer.read(app).preview_state_for_test().0.scale - 1.).abs() < 0.001);
        window.press("cmd-0", app);
        assert!(explorer.read(app).preview_state_for_test().0.fit);
        window.press("cmd-=", app);
        window.press("right", app);
        let (zoom, sequence) = explorer.read(app).preview_state_for_test();
        assert!(zoom.fit, "each new file starts fitted");
        assert_eq!(sequence.unwrap().index, 1);
    });
    crate::form_input::test_turn(cx, handle, |window, app| {
        window.press("right", app);
        assert_eq!(
            explorer.read(app).preview_state_for_test().1.unwrap().index,
            1
        );
        window.press("cmd-=", app);
        assert!((explorer.read(app).preview_state_for_test().0.scale - 1.2).abs() < 0.001);
        window.press("cmd-0", app);
        assert!(explorer.read(app).preview_state_for_test().0.fit);
        window.press("escape", app);
    });
    assert_eq!(fixture.read_with(cx, |fixture, _| fixture.closes), 1);
    let _ = std::fs::remove_dir_all(root);
}

#[gpui::test]
fn a_cited_file_is_a_chip_that_opens_the_turn_preview_at_that_file(cx: &mut TestAppContext) {
    let root = std::env::temp_dir().join(format!("riwork-citation-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("outputs")).unwrap();
    let report = root.join("outputs/Bericht März.docx");
    std::fs::write(&report, "fixture").unwrap();
    let (handle, view, _) = editor_tests::mount_selection(cx);
    let opened = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let observed = opened.clone();
    cx.update(|cx| {
        cx.subscribe(&view, move |_, event: &ChatViewEvent, _| {
            if let ChatViewEvent::OpenPreview { sequence } = event {
                observed.borrow_mut().push(sequence.clone());
            }
        })
        .detach()
    });
    view.update(cx, |view, cx| {
        view.model.link = state::Link::Live;
        let mut info = testing::info("chat");
        info.cwd = root.clone();
        view.model.transcript.info = Some(info);
        let mut answer = item(
            "answer",
            "turn",
            ItemBody::AgentMessage {
                text: format!(
                    "Done.\n:codex-file-citation{{path=\"{}\" artifact_kind=\"document\" purpose=\"output\"}}",
                    report.display()
                ),
            },
        );
        answer.presentation.phase = Some(MessagePhase::Final);
        view.model.transcript.items = vec![answer];
        view.refresh_projection(cx);
        cx.notify();
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        // The chip reads as the file's name; the directive itself is nowhere on screen.
        assert_eq!(
            window.find("transcript:answer/1/file/0").label(),
            Some("Bericht März.docx")
        );
        assert_eq!(window.find("transcript:answer/0").label(), Some("Done."));
        window.click("chip:answer/1/file/0".to_owned(), cx);
    })
    .unwrap();
    cx.run_until_parked();
    let opened = opened.borrow();
    assert_eq!(opened.len(), 1);
    let current = opened[0].current().unwrap();
    assert_eq!(current.title, "Bericht März.docx");
    assert_eq!(current.target, report.to_string_lossy());
    assert_eq!(current.source, "Answer");
    let _ = std::fs::remove_dir_all(root);
}
