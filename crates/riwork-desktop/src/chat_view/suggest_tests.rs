//! The `/` and `@` list over a real message box: it opens on the trigger and filters as the
//! token is typed, ↑ ↓ ⏎ ⇥ ⎋ and the pointer work it, its preview says what accepting puts
//! in, it stands clear of the card in wide and narrow panes, and accepting never sends.
use super::editor_tests::mount_sized;
use super::suggest_ui::Item;
use super::*;
use crate::chat::model::ChatCommand;
use gpui::{AnyWindowHandle, ElementId, TestAppContext, WindowHandle};
use gpui_kit::test::TestWindowExt;
use std::{path::PathBuf, sync::mpsc::Receiver};

type Mounted = (
    WindowHandle<gpui_kit::base::Root>,
    Entity<ChatView>,
    Receiver<feed::Delivery>,
);

/// A chat folder with a few files.
fn folder() -> PathBuf {
    let root = std::env::temp_dir().join(format!("riwork-suggest-ui-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("README.md"), "# Project\n\nRead me first.\n").unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() {\n    run();\n}\n").unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn run() {}\n").unwrap();
    root
}

/// A tab of a `provider` chat working in `root`, in a window `width` px wide, its message box
/// focused.
fn mount(
    cx: &mut TestAppContext,
    provider: Provider,
    root: &std::path::Path,
    width: f32,
) -> Mounted {
    let mounted = mount_sized(
        cx,
        HostConfig {
            ensure: Arc::new(|| Err("fixture staging is disabled".into())),
        },
        width,
    );
    let mut info = testing::info("fixture-chat");
    info.provider = provider;
    info.cwd = root.to_path_buf();
    mounted.1.update(cx, |view, cx| {
        view.adopt_info_for_tests(info);
        cx.notify();
    });
    cx.update_window(mounted.0.into(), |_, window, cx| {
        window.click("chat-composer", cx)
    })
    .unwrap();
    cx.run_until_parked();
    mounted
}

fn handle(mounted: &Mounted) -> AnyWindowHandle {
    mounted.0.into()
}

fn type_text(cx: &mut TestAppContext, mounted: &Mounted, text: &str) {
    cx.update_window(handle(mounted), |_, window, cx| window.input(text, cx))
        .unwrap();
    cx.run_until_parked();
}

fn press(cx: &mut TestAppContext, mounted: &Mounted, key: &str) {
    cx.update_window(handle(mounted), |_, window, cx| window.press(key, cx))
        .unwrap();
    cx.run_until_parked();
}

fn draft(cx: &mut TestAppContext, mounted: &Mounted) -> (String, usize) {
    mounted.1.read_with(cx, |view, cx| {
        let state = view.composer.read(cx);
        (state.value().to_string(), state.cursor())
    })
}

/// The rows as their text, and which one is highlighted; none while the list is closed.
fn rows(cx: &mut TestAppContext, mounted: &Mounted) -> Option<(Vec<String>, usize)> {
    mounted.1.read_with(cx, |view, _| {
        view.suggestions.open.as_ref().map(|open| {
            (
                open.items.iter().map(Item::insertion).collect(),
                open.highlight,
            )
        })
    })
}

fn sent(recording: &Receiver<feed::Delivery>) -> Vec<ChatCommand> {
    recording
        .try_iter()
        .filter_map(|delivery| match delivery {
            feed::Delivery::Submission { command, .. } => Some(command),
            _ => None,
        })
        .collect()
}

fn row(n: u64) -> ElementId {
    ElementId::NamedInteger("composer-suggestion".into(), n)
}

/// Draw a frame and read what is on it.
fn frame<R>(
    cx: &mut TestAppContext,
    mounted: &Mounted,
    read: impl FnOnce(&mut Window, &mut gpui::App) -> R,
) -> R {
    cx.update_window(handle(mounted), |_, window, cx| {
        window.render_frame(cx);
        read(window, cx)
    })
    .unwrap()
}

#[gpui::test]
fn slash_opens_claudes_commands_and_return_inserts_without_sending(cx: &mut TestAppContext) {
    let root = folder();
    let mounted = mount(cx, Provider::Claude, &root, 700.);
    type_text(cx, &mounted, "/");
    assert_eq!(
        rows(cx, &mounted),
        Some((vec!["/compact".into(), "/memo".into()], 0))
    );
    frame(cx, &mounted, |window, _| {
        assert!(window.find("composer-suggestions").visible());
        let first = window.find(row(0));
        assert_eq!(first.role(), Some(gpui::Role::ListBoxOption));
        assert_eq!(first.selected(), Some(true));
        assert_eq!(first.label(), Some("/compact"));
        assert_eq!(
            window.find("composer-suggestion-preview").label(),
            Some("Preview of /compact")
        );
    });
    type_text(cx, &mounted, "com");
    assert_eq!(rows(cx, &mounted), Some((vec!["/compact".into()], 0)));
    press(cx, &mounted, "enter");
    assert_eq!(draft(cx, &mounted), ("/compact ".into(), 9));
    assert_eq!(rows(cx, &mounted), None, "accepting closes the list");
    assert!(sent(&mounted.2).is_empty(), "accepting never sends");
    frame(cx, &mounted, |window, _| {
        assert!(window.try_find("composer-suggestions").is_none());
    });
    // Sending stays the person's own ⏎, and sends what the box holds.
    press(cx, &mounted, "enter");
    assert_eq!(
        sent(&mounted.2),
        [ChatCommand::Send {
            text: "/compact".into()
        }]
    );
    let _ = std::fs::remove_dir_all(root);
}

#[gpui::test]
fn only_a_trigger_at_a_word_start_opens_the_list(cx: &mut TestAppContext) {
    let root = folder();
    let mounted = mount(cx, Provider::Claude, &root, 700.);
    type_text(cx, &mounted, "a/b me@host");
    assert_eq!(rows(cx, &mounted), None, "a sigil inside a word is text");
    type_text(cx, &mounted, " /");
    assert_eq!(rows(cx, &mounted), None, "a command only starts a message");
    type_text(cx, &mounted, " @");
    assert!(rows(cx, &mounted).is_some(), "@ after a space lists files");
    // A command that matches nothing closes the list.
    let codex = mount(cx, Provider::Codex, &root, 700.);
    type_text(cx, &codex, "/");
    assert_eq!(
        rows(cx, &codex),
        Some((vec!["/memo".into()], 0)),
        "Codex chats offer RiWork's /memo, not Claude's /compact"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[gpui::test]
fn at_lists_the_chat_folder_and_the_keys_move_and_accept(cx: &mut TestAppContext) {
    let root = folder();
    let mounted = mount(cx, Provider::Codex, &root, 700.);
    type_text(cx, &mounted, "see @");
    assert_eq!(
        rows(cx, &mounted),
        Some((
            vec![
                "src/".into(),
                "README.md".into(),
                "src/lib.rs".into(),
                "src/main.rs".into()
            ],
            0
        ))
    );
    press(cx, &mounted, "down");
    press(cx, &mounted, "down");
    assert_eq!(rows(cx, &mounted).unwrap().1, 2);
    press(cx, &mounted, "up");
    press(cx, &mounted, "up");
    press(cx, &mounted, "up");
    assert_eq!(
        rows(cx, &mounted).unwrap().1,
        3,
        "↑ from the first row goes round"
    );
    assert_eq!(
        draft(cx, &mounted).0,
        "see @",
        "the arrows move the highlight, not the caret"
    );
    type_text(cx, &mounted, "ma");
    assert_eq!(rows(cx, &mounted), Some((vec!["src/main.rs".into()], 0)));
    press(cx, &mounted, "tab");
    assert_eq!(draft(cx, &mounted), ("see src/main.rs ".into(), 16));
    assert!(sent(&mounted.2).is_empty());
    let _ = std::fs::remove_dir_all(root);
}

#[gpui::test]
fn the_preview_shows_a_files_first_lines_and_what_is_inserted(cx: &mut TestAppContext) {
    let root = folder();
    let mounted = mount(cx, Provider::Claude, &root, 700.);
    type_text(cx, &mounted, "@main");
    let peek = mounted.1.read_with(cx, |view, _| {
        let open = view.suggestions.open.as_ref().unwrap();
        let Item::File(entry) = &open.items[open.highlight] else {
            panic!("a file row");
        };
        let root = view.suggestion_root_for_tests().unwrap();
        view.suggestion_peek_for_tests(&root, &entry.path)
    });
    assert_eq!(
        peek,
        Some(crate::chat::suggest::Peek::Text(vec![
            "fn main() {".into(),
            "    run();".into(),
            "}".into()
        ]))
    );
    frame(cx, &mounted, |window, _| {
        assert_eq!(
            window.find("composer-suggestion-preview").label(),
            Some("Preview of src/main.rs")
        );
    });
    let _ = std::fs::remove_dir_all(root);
}

#[gpui::test]
fn escape_closes_the_list_until_its_token_is_typed_again(cx: &mut TestAppContext) {
    let root = folder();
    let mounted = mount(cx, Provider::Claude, &root, 700.);
    type_text(cx, &mounted, "@");
    assert!(rows(cx, &mounted).is_some());
    press(cx, &mounted, "escape");
    assert_eq!(rows(cx, &mounted), None);
    assert_eq!(draft(cx, &mounted).0, "@", "⎋ leaves the draft alone");
    type_text(cx, &mounted, "src");
    assert_eq!(rows(cx, &mounted), None, "the token ⎋ closed stays closed");
    for _ in 0..4 {
        press(cx, &mounted, "backspace");
    }
    type_text(cx, &mounted, "@s");
    assert!(rows(cx, &mounted).is_some(), "a new token opens it again");
    let _ = std::fs::remove_dir_all(root);
}

#[gpui::test]
fn the_pointer_highlights_and_a_click_accepts_into_the_focused_box(cx: &mut TestAppContext) {
    let root = folder();
    let mounted = mount(cx, Provider::Claude, &root, 700.);
    type_text(cx, &mounted, "@");
    cx.update_window(handle(&mounted), |_, window, cx| {
        window.render_frame(cx);
        window.hover(row(1), cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(rows(cx, &mounted).unwrap().1, 1);
    cx.update_window(handle(&mounted), |_, window, cx| {
        window.render_frame(cx);
        window.click(row(2), cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(draft(cx, &mounted), ("src/lib.rs ".into(), 11));
    assert!(sent(&mounted.2).is_empty());
    let focused = frame(cx, &mounted, |window, cx| {
        mounted
            .1
            .read(cx)
            .composer
            .read(cx)
            .focus_handle(cx)
            .is_focused(window)
    });
    assert!(focused, "the click left the focus in the message box");
    let _ = std::fs::remove_dir_all(root);
}

#[gpui::test]
fn shift_return_is_still_a_new_line_and_a_held_return_never_sends(cx: &mut TestAppContext) {
    let root = folder();
    let mounted = mount(cx, Provider::Claude, &root, 700.);
    type_text(cx, &mounted, "/");
    press(cx, &mounted, "shift-enter");
    assert_eq!(draft(cx, &mounted).0, "/\n");
    assert_eq!(rows(cx, &mounted), None, "the caret left the token");
    assert!(sent(&mounted.2).is_empty());
    // ⏎ held down: the press accepts, its repeats (no key-up between) send nothing.
    press(cx, &mounted, "backspace");
    assert!(rows(cx, &mounted).is_some());
    cx.update_window(handle(&mounted), |_, window, cx| {
        for _ in 0..3 {
            window.dispatch_action(
                Box::new(gpui_kit::base::input::Enter {
                    secondary: false,
                    shift: false,
                }),
                cx,
            );
        }
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(draft(cx, &mounted).0, "/compact ");
    assert!(sent(&mounted.2).is_empty(), "a held ⏎ does not send");
    let _ = std::fs::remove_dir_all(root);
}

/// The list and its preview stand above the card, clear of Send, beside each other in a wide
/// pane and stacked in a narrow one.
#[gpui::test]
fn the_list_stands_clear_of_the_controls_in_wide_and_narrow_panes(cx: &mut TestAppContext) {
    let root = folder();
    for (width, beside) in [(900., true), (300., false)] {
        let mounted = mount(cx, Provider::Claude, &root, width);
        type_text(cx, &mounted, "@");
        let card = mounted
            .1
            .read_with(cx, |view, _| view.suggestions.card.get());
        frame(cx, &mounted, |window, _| {
            let popup = window.find("composer-suggestions").bounds();
            let list = window.find("composer-suggestion-list").bounds();
            let preview = window.find("composer-suggestion-preview").bounds();
            let send = window.find("chat-send").bounds();
            assert!(popup.bottom() <= card.top(), "{width}: above the card");
            assert!(popup.bottom() <= send.top(), "{width}: Send stays clear");
            assert!(
                popup.left() >= px(0.) && popup.right() <= px(width),
                "{width}"
            );
            if beside {
                assert!(
                    preview.left() >= list.right(),
                    "a wide pane puts them side by side"
                );
            } else {
                assert!(
                    preview.bottom() <= list.top(),
                    "a narrow pane stacks the preview"
                );
                assert!(list.size.width <= card.size.width);
            }
        });
    }
    let _ = std::fs::remove_dir_all(root);
}
