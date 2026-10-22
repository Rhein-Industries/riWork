use super::*;
use crate::chat_view::{DisplayMode, HostConfig, Link, editor_tests};
use gpui::{App, Entity, TestAppContext, WindowHandle, px};
use gpui_kit::{base::Root, test::TestWindowExt};
use std::sync::Arc;

struct TempDir(PathBuf);
impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("riwork-plan-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn preview_is_an_exact_bounded_unicode_prefix_and_short_plans_stay_complete() {
    let short = "# Small\r\n\r\n- Keep **this** exactly.\r\n";
    assert_eq!(shown_source(short, false), short);
    let long = format!("# Plan\n{}", "😀é日".repeat(500));
    let preview = shown_source(&long, false);
    assert!(long.starts_with(preview));
    assert_eq!(preview.chars().count(), PREVIEW_CHARS);
    assert_eq!(shown_source(&long, true), long);
    let lines = "line\r\n".repeat(30);
    assert_eq!(
        shown_source(&lines, false),
        "line\r\n".repeat(PREVIEW_LINES)
    );
    assert_eq!(filename("# **My** / .. Plan: 😀\n"), "my-plan.md");
    assert_eq!(filename("no heading"), "plan.md");
}

#[test]
fn export_preserves_exact_bytes_and_refuses_unconfirmed_or_changed_destinations() {
    let home = TempDir::new();
    let path = home.path().join("plan.md");
    let source = "# Café 🦀\r\n\r\n```rs\r\nlet x = 1;\r\n```\r\n";
    let prepared = destination(path.clone(), Some(home.path())).unwrap();
    write(&prepared, source, false).unwrap();
    assert_eq!(fs::read(&path).unwrap(), source.as_bytes());
    let existing = destination(path.clone(), None).unwrap();
    assert!(write(&existing, "replacement", false).is_err());
    assert_eq!(fs::read(&path).unwrap(), source.as_bytes());
    fs::write(&path, "changed externally").unwrap();
    assert!(write(&existing, "replacement", true).is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "changed externally");
    let existing = destination(path.clone(), None).unwrap();
    write(&existing, source, true).unwrap();
    assert_eq!(fs::read(&path).unwrap(), source.as_bytes());
    assert_eq!(fs::read_dir(home.path()).unwrap().count(), 1);
}

#[test]
fn workspace_export_rejects_escape_symlinks_and_new_file_collisions() {
    let home = TempDir::new();
    let outside = TempDir::new();
    assert!(destination(outside.path().join("escape.md"), Some(home.path())).is_err());
    std::os::unix::fs::symlink(outside.path(), home.path().join("linked")).unwrap();
    assert!(destination(home.path().join("linked/escape.md"), Some(home.path())).is_err());
    let path = home.path().join("plan.md");
    let prepared = destination(path.clone(), None).unwrap();
    fs::write(&path, "another writer").unwrap();
    assert!(write(&prepared, "my plan", false).is_err());
    assert_eq!(fs::read_to_string(path).unwrap(), "another writer");
    std::os::unix::fs::symlink(home.path().join("plan.md"), home.path().join("alias.md")).unwrap();
    assert!(destination(home.path().join("alias.md"), None).is_err());
}

type Handle = WindowHandle<Root>;

fn act(cx: &mut TestAppContext, handle: Handle, f: impl FnOnce(&mut Window, &mut App)) {
    cx.update_window(handle.into(), |_, window, cx| f(window, cx))
        .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
}

fn mount(
    cx: &mut TestAppContext,
    source: &str,
    workspace: &Path,
    width: f32,
) -> (Handle, Entity<ChatView>) {
    let (handle, view, _) = editor_tests::mount_sized(
        cx,
        HostConfig {
            ensure: Arc::new(|| panic!("plan fixture must never start a host")),
        },
        width,
    );
    act(cx, handle, |window, cx| {
        view.update(cx, |view, cx| {
            view.model.link = Link::Live;
            view.display_mode = DisplayMode::Verbose;
            view.model.transcript.info = Some(
                serde_json::from_value(serde_json::json!({
                    "id": "fixture-chat", "provider": "claude", "cwd": workspace,
                    "title": "Fixture", "created_at_unix": 1, "state": {"state": "idle"}
                }))
                .unwrap(),
            );
            view.model.transcript.items = vec![Item {
                started_at: None,
                ended_at: None,
                id: "plan".into(),
                turn_id: Some("turn".into()),
                status: ItemStatus::Completed,
                presentation: Default::default(),
                body: ItemBody::Plan {
                    explanation: Some(source.into()),
                    steps: vec![],
                },
            }];
            view.composer
                .update(cx, |state, cx| state.set_value("keep my draft", window, cx));
            // A plan is a projected call; its document is drawn while the call is open.
            view.disclosure.overrides.insert("plan".into(), true);
            view.refresh_projection(cx);
            view.list.remeasure();
            cx.notify();
        });
    });
    (handle, view)
}

fn click(cx: &mut TestAppContext, handle: Handle, id: &str) {
    act(cx, handle, |window, cx| window.click(id.to_owned(), cx));
}

#[gpui::test]
fn native_save_cancel_error_and_replace_preserve_draft_and_exact_source(cx: &mut TestAppContext) {
    let home = TempDir::new();
    let source = "# Plan é\r\n\r\n```rust\r\nlet a = 1;\r\n```\r\n";
    let (handle, view) = mount(cx, source, home.path(), 700.);
    click(cx, handle, "save-plan:plan");
    assert!(cx.did_prompt_for_new_path());
    cx.simulate_new_path_selection(|_| None);
    cx.run_until_parked();
    assert_eq!(fs::read_dir(home.path()).unwrap().count(), 0);
    assert!(cx.read(|cx| view.read(cx).notices.local(LocalKey::Plan).is_none()));

    click(cx, handle, "save-plan-workspace:plan");
    cx.simulate_new_path_selection(|directory| {
        assert_eq!(directory, home.path());
        Some(home.path().join("missing/fail.md"))
    });
    cx.run_until_parked();
    assert!(cx.read(|cx| {
        view.read(cx)
            .notices
            .local(LocalKey::Plan)
            .unwrap()
            .starts_with("Could not save plan:")
    }));
    let path = home.path().join("saved.md");
    fs::write(&path, "original").unwrap();
    click(cx, handle, "save-plan:plan");
    cx.simulate_new_path_selection(|_| Some(path.clone()));
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    assert_eq!(fs::read_to_string(&path).unwrap(), "original");
    cx.simulate_prompt_answer("Cancel");
    cx.run_until_parked();
    assert_eq!(fs::read_to_string(&path).unwrap(), "original");
    click(cx, handle, "save-plan:plan");
    cx.simulate_new_path_selection(|_| Some(path.clone()));
    cx.run_until_parked();
    cx.simulate_prompt_answer("Replace");
    cx.run_until_parked();
    assert_eq!(fs::read(&path).unwrap(), source.as_bytes());
    assert_eq!(
        cx.read(|cx| view.read(cx).composer_text(cx)),
        "keep my draft"
    );
    assert!(cx.read(|cx| view.read(cx).pending_submission.is_none()));
}

#[gpui::test]
fn long_plan_expands_and_collapses_and_copies_full_source_at_narrow_width(cx: &mut TestAppContext) {
    let home = TempDir::new();
    let source = format!("# Long plan\n\n{}", "- Étape 😀 with details\n".repeat(45));
    let (handle, view) = mount(cx, &source, home.path(), 380.);
    act(cx, handle, |window, _| {
        assert_eq!(
            window.find("plan-document:plan").label(),
            Some("Expand plan")
        );
        let card = window.find("plan-card:plan").bounds();
        for id in ["save-plan:plan", "save-plan-workspace:plan"] {
            let bounds = window.find(id).bounds();
            assert!(bounds.left() >= card.left() && bounds.right() <= card.right());
        }
    });
    click(cx, handle, "copy-plan:plan");
    assert_eq!(
        cx.read(|cx| cx.read_from_clipboard().unwrap().text()),
        Some(source.clone())
    );
    let header_top = cx
        .update_window(handle.into(), |_, window, _| {
            window.find("plan-document:plan").bounds().top()
        })
        .unwrap();
    click(cx, handle, "plan-document:plan");
    assert!(cx.read(|cx| view.read(cx).open.contains(&expansion_key("plan"))));
    assert!(!cx.read(|cx| view.read(cx).list.is_following_tail()));
    act(cx, handle, |window, _| {
        assert_eq!(
            window.find("plan-document:plan").label(),
            Some("Collapse plan")
        );
        let after = window.find("plan-document:plan").bounds().top();
        assert!((after - header_top).abs() <= px(1.));
    });
    // The same focused control remains usable from the keyboard after relayout.
    act(cx, handle, |window, cx| window.press("enter", cx));
    assert!(!cx.read(|cx| view.read(cx).open.contains(&expansion_key("plan"))));
}

#[gpui::test]
fn streaming_plan_cannot_save_and_keeps_expansion_when_source_grows(cx: &mut TestAppContext) {
    let home = TempDir::new();
    let source = "plan paragraph\n\n".repeat(30);
    let (handle, view) = mount(cx, &source, home.path(), 700.);
    click(cx, handle, "plan-document:plan");
    act(cx, handle, |window, cx| {
        view.update(cx, |view, cx| {
            view.model.transcript.items[0].status = ItemStatus::InProgress;
            if let ItemBody::Plan { explanation, .. } = &mut view.model.transcript.items[0].body {
                explanation.as_mut().unwrap().push_str("new content\n");
            }
            view.save_plan("plan", false, window, cx);
            view.refresh_projection(cx);
            view.list.remeasure();
            cx.notify();
        });
    });
    assert!(!cx.did_prompt_for_new_path());
    assert!(cx.read(|cx| view.read(cx).open.contains(&expansion_key("plan"))));
}
