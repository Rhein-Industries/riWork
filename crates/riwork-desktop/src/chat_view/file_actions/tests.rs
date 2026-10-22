use super::*;
use crate::chat_view::{HostConfig, Link, editor_tests, image_scope::tests::Fixture};
use gpui::{App, Entity, Focusable, TestAppContext, WindowHandle};
use gpui_kit::{base::Root, test::TestWindowExt};
use std::{cell::RefCell, rc::Rc, sync::Arc};

#[test]
fn relative_paths_use_canonical_cwd_and_preserve_unicode() {
    assert_eq!(
        relative_path(
            Path::new("/project/work"),
            Path::new("/project/work/a/é.rs")
        ),
        PathBuf::from("a/é.rs")
    );
    assert_eq!(
        relative_path(Path::new("/project/work"), Path::new("/project/other/a.rs")),
        PathBuf::from("../other/a.rs")
    );
    assert_eq!(
        relative_path(Path::new("/project/work"), Path::new("/project/work")),
        PathBuf::from(".")
    );
    let fixture = Fixture::new();
    let scope = fixture.alpha_chat();
    let path = fixture.alpha.join("café notes.rs");
    std::fs::write(&path, "fn main() {}\n").unwrap();
    assert_eq!(
        validated(
            &scope,
            Some(&fixture.alpha),
            "café%20notes.rs:7",
            Action::Relative
        )
        .unwrap(),
        PathBuf::from("café notes.rs")
    );
    assert_eq!(
        validated(
            &scope,
            Some(&fixture.alpha),
            "café%20notes.rs:7",
            Action::Full
        )
        .unwrap(),
        path.canonicalize().unwrap()
    );
    assert_eq!(
        validated(
            &scope,
            Some(&fixture.alpha),
            "../alpha-feature/my%20shot%20ü.png",
            Action::Relative
        )
        .unwrap(),
        PathBuf::from("../alpha-feature/my shot ü.png")
    );
    assert!(
        validated(
            &scope,
            Some(&fixture.alpha),
            "../alpha-feature/my%20shot%20ü.png",
            Action::Preview
        )
        .is_err()
    );
}

#[test]
fn every_file_action_rechecks_scope_regular_files_and_symlinks() {
    let fixture = Fixture::new();
    let scope = fixture.alpha_chat();
    for action in [
        Action::Relative,
        Action::Full,
        Action::Reveal,
        Action::Editor,
        Action::Preview,
    ] {
        for target in [
            "missing.rs",
            "link.png",
            "../alpha-feature/linkdir/b.png",
            "../beta/b.png",
            "../alpha-feature/sub",
        ] {
            assert!(
                validated(&scope, Some(&fixture.alpha), target, action).is_err(),
                "unexpectedly allowed {target}"
            );
        }
    }
    let path = fixture.alpha.join("old.rs");
    std::fs::write(&path, "source").unwrap();
    assert!(validated(&scope, Some(&fixture.alpha), "old.rs", Action::Editor).is_ok());
    std::fs::remove_file(&path).unwrap();
    assert!(validated(&scope, Some(&fixture.alpha), "old.rs", Action::Full).is_err());
    std::fs::write(fixture.alpha.join("run.command"), "echo no").unwrap();
    assert!(
        validated(&scope, Some(&fixture.alpha), "run.command", Action::Editor)
            .unwrap_err()
            .contains("Reveal in Finder")
    );
    assert!(
        validated(
            &ImageScope::default(),
            Some(&fixture.alpha),
            "notes.txt",
            Action::Full
        )
        .is_err()
    );
}

#[test]
fn paging_bounds_each_panel_without_losing_any_targets() {
    for count in [0usize, 1, 20, 21, 1999] {
        let pages = count.max(1).div_ceil(PAGE_SIZE);
        let all = (0..pages)
            .flat_map(|page| page_range(count, page))
            .collect::<Vec<_>>();
        assert_eq!(all, (0..count).collect::<Vec<_>>());
        assert!(page_range(count, usize::MAX).len() <= PAGE_SIZE);
    }
}

type Handle = WindowHandle<Root>;
fn act(cx: &mut TestAppContext, handle: Handle, f: impl FnOnce(&mut Window, &mut App)) {
    cx.update_window(handle.into(), |_, window, cx| f(window, cx))
        .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
}
fn click(cx: &mut TestAppContext, handle: Handle, id: &str) {
    act(cx, handle, |window, cx| window.click(id.to_owned(), cx));
}
fn mount(
    cx: &mut TestAppContext,
    fixture: &Fixture,
    targets: Vec<String>,
) -> (Handle, Entity<ChatView>) {
    cx.update(|cx| {
        cx.bind_keys([gpui::KeyBinding::new(
            "escape",
            gpui_kit::base::input::Escape,
            Some("ChatView"),
        )])
    });
    let (handle, view, _) = editor_tests::mount_sized(
        cx,
        HostConfig {
            ensure: Arc::new(|| panic!("file action fixture must not start a provider")),
        },
        320.,
    );
    act(cx, handle, |window, cx| {
        view.update(cx, |view, cx| {
            view.model.link = Link::Live;
            view.model.transcript.info = Some(fixture.chat(
                Some("aaaaaaaa-0000-4000-8000-0000000000a1"),
                Some("alpha-main"),
                &fixture.alpha,
                None,
            ));
            view.image_scope = fixture.alpha_chat();
            view.composer
                .update(cx, |state, cx| state.set_value("keep draft é", window, cx));
            view.open_file_actions(targets, window, cx);
        });
    });
    (handle, view)
}

#[gpui::test]
fn native_pages_keep_all_actions_within_narrow_panel_and_restore_focus(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let mut targets = (0..44)
        .map(|n| format!("src/file-{n:02}.rs"))
        .collect::<Vec<_>>();
    targets.extend([
        format!("00-{}café.rs", "directory/".repeat(30)),
        "src/file-00.rs".into(),
        "https://example.com".into(),
    ]);
    let (handle, view) = mount(cx, &fixture, targets);
    act(cx, handle, |window, cx| {
        assert_eq!(view.read(cx).file_action_targets.len(), 45);
        assert!(
            view.read(cx)
                .file_action_focus
                .as_ref()
                .unwrap()
                .is_focused(window)
        );
        assert_eq!(window.find("file-actions-page").label(), Some("1–20 of 45"));
        let panel = window.find("chat-file-actions").bounds();
        for key in ["relative", "full", "reveal", "editor", "preview"] {
            let bounds = window.find(format!("file-action-{key}:0")).bounds();
            assert!(
                bounds.left() >= panel.left() && bounds.right() <= panel.right(),
                "overflow in {key}"
            );
        }
        let path = window.find("file-action-path:0").bounds();
        assert!(path.right() <= panel.right());
        assert_eq!(
            window.find("file-action-editor:0").label(),
            Some("Open in default app")
        );
    });
    click(cx, handle, "file-actions-next");
    assert_eq!(cx.read(|cx| view.read(cx).file_action_page), 1);
    act(cx, handle, |window, _| {
        assert_eq!(
            window.find("file-actions-page").label(),
            Some("21–40 of 45")
        )
    });
    click(cx, handle, "file-actions-next");
    act(cx, handle, |window, _| {
        assert_eq!(
            window.find("file-actions-page").label(),
            Some("41–45 of 45")
        )
    });
    click(cx, handle, "file-actions-previous");
    act(cx, handle, |window, cx| window.press("escape", cx));
    assert!(cx.read(|cx| view.read(cx).file_action_targets.is_empty()));
    assert_eq!(cx.read(|cx| view.read(cx).file_action_page), 0);
    assert_eq!(
        cx.read(|cx| view.read(cx).composer_text(cx)),
        "keep draft é"
    );
    act(cx, handle, |window, cx| {
        assert!(
            view.read(cx)
                .composer
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
        )
    });
}

#[gpui::test]
fn native_copy_preview_and_stale_errors_use_exact_scoped_paths(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let path = fixture.alpha.join("café notes.rs");
    std::fs::write(&path, "source").unwrap();
    let target = "café%20notes.rs:7";
    let (handle, view) = mount(cx, &fixture, vec![target.into()]);
    click(cx, handle, "file-action-relative:0");
    assert_eq!(
        cx.read(|cx| cx.read_from_clipboard().unwrap().text()),
        Some("café notes.rs".into())
    );
    click(cx, handle, "file-action-full:0");
    assert_eq!(
        cx.read(|cx| cx.read_from_clipboard().unwrap().text()),
        Some(path.canonicalize().unwrap().display().to_string())
    );
    let seen = Rc::new(RefCell::new(Vec::new()));
    let observed = seen.clone();
    cx.update(|cx| {
        cx.subscribe(&view, move |_, event: &ChatViewEvent, _| {
            if let ChatViewEvent::OpenFile { target } = event {
                observed.borrow_mut().push(target.clone());
            }
        })
        .detach()
    });
    click(cx, handle, "file-action-preview:0");
    assert_eq!(*seen.borrow(), vec![target]);
    assert!(cx.read(|cx| view.read(cx).file_action_targets.is_empty()));
    act(cx, handle, |window, cx| {
        view.update(cx, |view, cx| {
            view.open_file_actions(vec![target.into()], window, cx)
        })
    });
    std::fs::remove_file(path).unwrap();
    let before = cx.read(|cx| cx.read_from_clipboard().unwrap().text());
    click(cx, handle, "file-action-full:0");
    assert_eq!(
        cx.read(|cx| cx.read_from_clipboard().unwrap().text()),
        before
    );
    assert!(cx.read(|cx| {
        view.read(cx)
            .notices
            .local(LocalKey::Export)
            .unwrap()
            .starts_with("Cannot use this file:")
    }));
    click(cx, handle, "file-action-editor:0");
    assert_eq!(seen.borrow().len(), 1);
    assert_eq!(
        cx.read(|cx| view.read(cx).composer_text(cx)),
        "keep draft é"
    );
}

#[gpui::test]
fn closing_panel_or_changing_scope_retires_pending_copy(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let (handle, view) = mount(cx, &fixture, vec!["notes.txt".into()]);
    act(cx, handle, |window, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string("untouched".into()));
        view.update(cx, |view, cx| {
            view.file_action("notes.txt".into(), Action::Full, window, cx);
            view.close_output_overlay(window, cx);
            // A new panel may have exactly the same targets and scope. It must
            // not revive the previous panel's queued clipboard action.
            view.open_file_actions(vec!["notes.txt".into()], window, cx);
        });
    });
    assert_eq!(
        cx.read(|cx| cx.read_from_clipboard().unwrap().text()),
        Some("untouched".into())
    );
    act(cx, handle, |window, cx| {
        view.update(cx, |view, cx| {
            view.open_file_actions(vec!["notes.txt".into()], window, cx);
            view.file_action("notes.txt".into(), Action::Full, window, cx);
            view.image_scope = ImageScope::default();
        })
    });
    assert_eq!(
        cx.read(|cx| cx.read_from_clipboard().unwrap().text()),
        Some("untouched".into())
    );
}
