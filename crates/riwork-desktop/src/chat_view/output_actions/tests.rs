use super::*;
use crate::chat::model::{Item, ItemBody, MessagePhase, Presentation};
use crate::chat_view::{Link, editor_tests};
use gpui::{App, ClipboardItem, TestAppContext, WindowHandle};
use gpui_kit::{base::Root, test::TestWindowExt};
use std::{cell::RefCell, rc::Rc};

type Handle = WindowHandle<Root>;
struct TempDir(PathBuf);
impl TempDir {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("riwork-snippet-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn item(id: &str, text: &str, status: ItemStatus) -> Item {
    Item {
        started_at: None,
        ended_at: None,
        id: id.into(),
        turn_id: Some("turn".into()),
        status,
        presentation: Presentation {
            phase: Some(MessagePhase::Final),
            ..Default::default()
        },
        body: ItemBody::AgentMessage { text: text.into() },
    }
}
fn act(cx: &mut TestAppContext, handle: Handle, f: impl FnOnce(&mut Window, &mut App)) {
    cx.update_window(handle.into(), |_, window, cx| f(window, cx))
        .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
}
fn install(
    cx: &mut TestAppContext,
    handle: Handle,
    view: &Entity<ChatView>,
    text: &str,
    status: ItemStatus,
) {
    act(cx, handle, |window, cx| {
        view.update(cx, |view, cx| {
            view.model.link = Link::Live;
            view.model.transcript.items = vec![item("answer", text, status)];
            view.composer.update(cx, |state, cx| {
                state.set_value("Keep my chat draft", window, cx)
            });
            view.refresh_projection(cx);
            view.list.remeasure();
            cx.notify();
        })
    });
}

#[gpui::test]
fn reader_scratch_edits_copy_and_cancelled_close_preserve_chat_and_source(cx: &mut TestAppContext) {
    let (handle, view, recording) = editor_tests::mount_selection(cx);
    let original = "```rust\nlet café = \"🦀\";\n```";
    install(cx, handle, &view, original, ItemStatus::Completed);
    act(cx, handle, |window, cx| {
        window.click("read-code:answer/0", cx)
    });
    act(cx, handle, |window, cx| {
        window.click("code-reader-editor", cx);
        window.press("cmd-a", cx);
        window.input("  edited 🦀\nsecond line  ", cx);
    });
    let edited = cx.read(|cx| {
        view.read(cx)
            .output_overlay
            .as_ref()
            .unwrap()
            .editor
            .read(cx)
            .value()
            .to_string()
    });
    act(cx, handle, |window, cx| {
        window.click("copy-code-reader", cx)
    });
    assert_eq!(
        cx.read(|cx| cx.read_from_clipboard().unwrap().text()),
        Some(edited.clone())
    );
    act(cx, handle, |window, _| {
        assert_eq!(window.find("copy-code-reader").label(), Some("Copied"))
    });
    act(cx, handle, |window, cx| {
        window.click("close-code-reader", cx)
    });
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Keep editing");
    cx.run_until_parked();
    assert_eq!(
        cx.read(|cx| view
            .read(cx)
            .output_overlay
            .as_ref()
            .unwrap()
            .editor
            .read(cx)
            .value()
            .to_string()),
        edited
    );
    assert_eq!(
        cx.read(|cx| view.read(cx).composer_text(cx)),
        "Keep my chat draft"
    );
    assert_eq!(
        cx.read(|cx| view.read(cx).model.transcript.items[0].body.clone()),
        ItemBody::AgentMessage {
            text: original.into()
        }
    );
    act(cx, handle, |window, cx| {
        window.click("close-code-reader", cx)
    });
    cx.simulate_prompt_answer("Close");
    cx.run_until_parked();
    assert!(cx.read(|cx| view.read(cx).output_overlay.is_none()));
    assert!(recording.try_recv().is_err());
}

#[gpui::test]
fn native_export_cancel_errors_and_exact_saved_scratch_leave_reader_open(cx: &mut TestAppContext) {
    let home = TempDir::new();
    let (handle, view, recording) = editor_tests::mount_selection(cx);
    install(
        cx,
        handle,
        &view,
        "```python\nprint('source')\n```",
        ItemStatus::Completed,
    );
    act(cx, handle, |window, cx| {
        window.click("read-code:answer/0", cx)
    });
    act(cx, handle, |window, cx| {
        window.click("code-reader-editor", cx);
        window.press("cmd-a", cx);
        window.input("# edited 🦀\nprint('scratch')  ", cx);
    });
    let expected = cx.read(|cx| {
        view.read(cx)
            .output_overlay
            .as_ref()
            .unwrap()
            .editor
            .read(cx)
            .value()
            .to_string()
    });
    act(cx, handle, |window, cx| {
        window.click("save-code-reader", cx)
    });
    assert!(cx.did_prompt_for_new_path());
    cx.simulate_new_path_selection(|_| None);
    cx.run_until_parked();
    assert_eq!(std::fs::read_dir(&home.0).unwrap().count(), 0);
    assert!(cx.read(|cx| view.read(cx).notices.local(LocalKey::Export).is_none()));
    act(cx, handle, |window, cx| {
        window.click("save-code-reader", cx)
    });
    cx.simulate_new_path_selection(|_| Some(home.0.join("missing/file.py")));
    cx.run_until_parked();
    act(cx, handle, |window, _| {
        assert!(window.try_find("code-reader-export-notice").is_some())
    });
    assert!(cx.read(|cx| view.read(cx).output_overlay.is_some()));
    let path = home.0.join("scratch.py");
    act(cx, handle, |window, cx| {
        window.click("save-code-reader", cx)
    });
    cx.simulate_new_path_selection(|_| Some(path.clone()));
    cx.run_until_parked();
    assert_eq!(std::fs::read(path).unwrap(), expected.as_bytes());
    assert!(cx.read(|cx| view.read(cx).output_overlay.is_some()));
    assert_eq!(
        cx.read(|cx| view.read(cx).composer_text(cx)),
        "Keep my chat draft"
    );
    assert!(recording.try_recv().is_err());
}

#[gpui::test]
fn streaming_shell_actions_are_disabled_and_handoff_requires_explicit_target(
    cx: &mut TestAppContext,
) {
    let (handle, view, recording) = editor_tests::mount_selection(cx);
    let source = "printf 'hello 🦀'\nprintf '%s' done";
    install(
        cx,
        handle,
        &view,
        &format!("```bash\n{source}\n```"),
        ItemStatus::InProgress,
    );
    let delivered = Rc::new(RefCell::new(Vec::<(String, String)>::new()));
    let seen = delivered.clone();
    cx.update(|cx| {
        cx.write_to_clipboard(ClipboardItem::new_string("unchanged".into()));
        cx.subscribe(&view, move |view, event: &ChatViewEvent, cx| match event {
            ChatViewEvent::CodeToTerminal { text } => {
                seen.borrow_mut().push(("picker".into(), text.clone()));
                cx.update_window(handle.into(), |_, window, cx| {
                    view.update(cx, |view, cx| {
                        view.choose_snippet_terminal(
                            text.clone(),
                            vec![("terminal-1".into(), "Fixture terminal".into())],
                            window,
                            cx,
                        )
                    });
                })
                .unwrap();
            }
            ChatViewEvent::FocusCodeTerminal { shell_id, text } => {
                seen.borrow_mut().push((shell_id.clone(), text.clone()));
                // The project window's authorized handoff only copies and focuses;
                // it never sends bytes to a terminal or adds an Enter keystroke.
                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
            }
            _ => {}
        })
        .detach();
    });
    act(cx, handle, |window, cx| {
        window.click("terminal-code:answer/0", cx);
        window.click("read-code:answer/0", cx);
        window.click("save-code:answer/0", cx);
    });
    assert!(delivered.borrow().is_empty());
    assert!(!cx.did_prompt_for_new_path());
    assert!(cx.read(|cx| view.read(cx).output_overlay.is_none()));
    act(cx, handle, |_, cx| {
        view.update(cx, |view, cx| {
            view.model.transcript.items[0].status = ItemStatus::Completed;
            cx.notify();
        })
    });
    act(cx, handle, |window, cx| {
        window.click("terminal-code:answer/0", cx)
    });
    assert_eq!(
        &*delivered.borrow(),
        &vec![("picker".into(), source.into())]
    );
    act(cx, handle, |window, cx| {
        window.input("must not enter the composer", cx);
        window.press("enter", cx);
    });
    assert_eq!(
        cx.read(|cx| view.read(cx).composer_text(cx)),
        "Keep my chat draft"
    );
    assert!(recording.try_recv().is_err());
    assert_eq!(
        cx.read(|cx| cx.read_from_clipboard().unwrap().text()),
        Some("unchanged".into())
    );
    act(cx, handle, |window, cx| {
        window.click("cancel-snippet-terminal", cx)
    });
    assert_eq!(
        cx.read(|cx| cx.read_from_clipboard().unwrap().text()),
        Some("unchanged".into())
    );
    act(cx, handle, |window, cx| {
        window.click("terminal-code:answer/0", cx)
    });
    act(cx, handle, |window, cx| {
        window.click("snippet-terminal:0", cx)
    });
    assert_eq!(
        delivered.borrow().last(),
        Some(&("terminal-1".into(), source.into()))
    );
    assert_eq!(
        cx.read(|cx| cx.read_from_clipboard().unwrap().text()),
        Some(source.into())
    );
    assert!(recording.try_recv().is_err());
}

#[gpui::test]
fn completed_prefix_item_cannot_enable_a_streaming_child_source(cx: &mut TestAppContext) {
    let (_, view, _) = editor_tests::mount_selection(cx);
    view.update(cx, |view, _| {
        view.model.transcript.items = vec![
            item("answer", "done", ItemStatus::Completed),
            item("answer/child", "streaming", ItemStatus::InProgress),
        ];
        assert!(view.completed_code("answer/0"));
        assert!(!view.completed_code("answer/child/0"));
        assert!(!view.completed_code("user:answer/child/0"));
        assert!(!view.completed_code("plan:answer/child:document/0"));
        assert!(!view.completed_code("unknown/0"));
        assert!(view.completed_code("plan:answer:document/0"));
        assert!(view.completed_code("user:answer/0"));
        assert!(!view.completed_code("answer"));
        // `user:x/0` is as specific for an item `user:x` as for an item `x`:
        // the later item owns it.
        for (first, second) in [("user:x", "x"), ("x", "user:x")] {
            view.model.transcript.items = vec![
                item(first, "one", ItemStatus::InProgress),
                item(second, "two", ItemStatus::Completed),
            ];
            assert!(view.completed_code("user:x/0"));
            view.model.transcript.items.reverse();
            assert!(!view.completed_code("user:x/0"));
        }
    });
}

/// The same answers from the model's index, as a folded chat looks items up.
#[gpui::test]
fn code_sources_are_found_in_a_folded_chat(cx: &mut TestAppContext) {
    use crate::chat::{model::ChatEvent, wire::Envelope};
    let (_, view, _) = editor_tests::mount_selection(cx);
    view.update(cx, |view, _| {
        let events: Vec<_> = [
            item("answer", "done", ItemStatus::Completed),
            item("answer/child", "streaming", ItemStatus::InProgress),
            item("x", "x", ItemStatus::Completed),
            item("user:x", "user x", ItemStatus::InProgress),
        ]
        .into_iter()
        .enumerate()
        .map(|(seq, item)| Envelope {
            chat_id: "chat".into(),
            seq: seq as u64 + 1,
            event: ChatEvent::ItemCompleted { item },
        })
        .collect();
        view.model.apply(&events);
        assert!(view.completed_code("answer/0"));
        assert!(view.completed_code("answer/2/0"));
        assert!(!view.completed_code("answer/child/0"));
        assert!(!view.completed_code("user:answer/child/0"));
        assert!(view.completed_code("plan:answer:document/0"));
        assert!(!view.completed_code("plan:answer/child:document/0"));
        assert!(!view.completed_code("user:x/0"));
        assert!(!view.completed_code("unknown/0"));
    });
}

#[gpui::test]
fn reader_native_copy_and_ime_escape_stay_in_the_scratch_editor(cx: &mut TestAppContext) {
    use gpui::{ElementInputHandler, InputHandler as _};
    let (handle, view, recording) = editor_tests::mount_selection(cx);
    install(
        cx,
        handle,
        &view,
        "```rust\nlet source = 1;\n```",
        ItemStatus::Completed,
    );
    act(cx, handle, |window, cx| {
        window.click("read-code:answer/0", cx)
    });
    act(cx, handle, |window, cx| {
        window.click("code-reader-editor", cx);
        let editor = view
            .read(cx)
            .output_overlay
            .as_ref()
            .unwrap()
            .editor
            .clone();
        let mut handler =
            ElementInputHandler::new(window.find("code-reader-editor").bounds(), editor);
        handler.replace_and_mark_text_in_range(None, "日本", Some(2..2), window, cx);
        view.update(cx, |view, cx| {
            view.escape_action(&gpui_kit::base::input::Escape, window, cx)
        });
        assert!(!view.read(cx).output_overlay.as_ref().unwrap().closing);
        window.press("escape", cx);
    });
    assert!(!cx.has_pending_prompt());
    assert!(cx.read(|cx| view.read(cx).output_overlay.is_some()));
    act(cx, handle, |window, cx| {
        window.press("cmd-a", cx);
        window.press("cmd-c", cx);
    });
    let current = cx.read(|cx| {
        view.read(cx)
            .output_overlay
            .as_ref()
            .unwrap()
            .editor
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
        "Keep my chat draft"
    );
    assert!(recording.try_recv().is_err());
}

#[gpui::test]
fn reader_close_prompt_settles_after_its_original_window_disappears(cx: &mut TestAppContext) {
    let (handle, view, recording) = editor_tests::mount_selection(cx);
    install(
        cx,
        handle,
        &view,
        "```rust\nlet source = 1;\n```",
        ItemStatus::Completed,
    );
    act(cx, handle, |window, cx| {
        window.click("read-code:answer/0", cx)
    });
    act(cx, handle, |window, cx| {
        window.click("code-reader-editor", cx);
        window.input("edited", cx);
    });
    act(cx, handle, |window, cx| {
        window.click("close-code-reader", cx)
    });
    assert!(cx.has_pending_prompt());
    assert!(cx.read(|cx| view.read(cx).output_overlay.as_ref().unwrap().closing));
    // Keep the entity alive as a moved tab would, but remove the prompt's window.
    cx.update_window(handle.into(), |_, window, _| window.remove_window())
        .unwrap();
    cx.simulate_prompt_answer("Keep editing");
    cx.run_until_parked();
    assert!(!cx.read(|cx| view.read(cx).output_overlay.as_ref().unwrap().closing));
    assert_eq!(
        cx.read(|cx| view.read(cx).composer_text(cx)),
        "Keep my chat draft"
    );
    assert!(recording.try_recv().is_err());
}

#[test]
fn snippet_names_strip_paths_and_shell_actions_require_shell_language() {
    assert_eq!(
        snippet_name(Some("../src/main.rs"), Some("rust")),
        "main.rs"
    );
    assert_eq!(snippet_name(Some(".."), Some("python")), "snippet.py");
    assert_eq!(snippet_name(None, Some("powershell")), "snippet.ps1");
    assert!(shell(Some("BaSh")));
    assert!(!shell(Some("rust")));
    assert!(!shell(None));
}
