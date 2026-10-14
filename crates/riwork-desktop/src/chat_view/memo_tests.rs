//! `/memo` in a real ChatView with a recording feed: it never sends, Insert appends the memo to
//! the draft, an empty memo offers Edit only, a chat of no project says there is no memo, and
//! Edit asks the window for the project's memo.
use super::editor_tests::mount;
use super::memo::Preview;
use super::*;
use crate::store::Store;
use gpui::TestAppContext;
use gpui_kit::test::TestWindowExt;
use std::{cell::RefCell, rc::Rc};

struct Projects {
    dir: std::path::PathBuf,
    store: Store,
    id: String,
}

impl Projects {
    fn new(memo: &str) -> Self {
        let dir = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("riwork-chat-memo-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("project")).unwrap();
        let store = Store::open(dir.join("state")).unwrap();
        let id = store
            .add_project(dir.join("project"), Some("Memo"))
            .unwrap()
            .id;
        store.set_project_memo(&id, memo).unwrap();
        Self { dir, store, id }
    }
}

impl Drop for Projects {
    fn drop(&mut self) {
        // Made by this test for itself.
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A chat of `project` (none: the global orchestrator) whose draft is `draft`, then Return.
fn submit(
    cx: &mut TestAppContext,
    project: Option<&Projects>,
    draft: &'static str,
) -> (
    gpui::WindowHandle<gpui_kit::base::Root>,
    Entity<ChatView>,
    std::sync::mpsc::Receiver<feed::Delivery>,
    Rc<RefCell<Vec<String>>>,
) {
    let (handle, view, recording) = mount(cx);
    let edits = Rc::new(RefCell::new(Vec::new()));
    let seen = edits.clone();
    cx.update(|cx| {
        cx.subscribe(&view, move |_, event: &ChatViewEvent, _| {
            if let ChatViewEvent::EditProjectMemo { project_id } = event {
                seen.borrow_mut().push(project_id.clone());
            }
        })
        .detach()
    });
    let mut info = super::testing::info("fixture-chat");
    info.project_id = project.map(|p| p.id.clone());
    let store = project.map(|p| p.store.clone());
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |v, cx| {
            v.adopt_info_for_tests(info);
            if let Some(store) = store {
                v.set_memo_store(store);
            }
            v.composer
                .update(cx, |state, cx| state.set_value(draft, window, cx));
            v.bump_generation();
            v.focus(window, cx);
        })
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| window.press("enter", cx))
        .unwrap();
    cx.run_until_parked();
    (handle, view, recording, edits)
}

fn preview(cx: &mut TestAppContext, view: &Entity<ChatView>) -> Option<Preview> {
    view.read_with(cx, |v, _| v.memo_preview.clone())
}

fn draft(cx: &mut TestAppContext, view: &Entity<ChatView>) -> String {
    view.read_with(cx, |v, cx| v.composer_text(cx))
}

fn shown(
    cx: &mut TestAppContext,
    handle: gpui::WindowHandle<gpui_kit::base::Root>,
    id: &'static str,
) -> bool {
    cx.update_window(handle.into(), move |_, window, cx| {
        window.render_frame(cx);
        window.try_find(id).is_some()
    })
    .unwrap()
}

fn click(
    cx: &mut TestAppContext,
    handle: gpui::WindowHandle<gpui_kit::base::Root>,
    id: &'static str,
) {
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(id, cx);
    })
    .unwrap();
    cx.run_until_parked();
}

#[gpui::test]
fn memo_previews_without_sending_and_insert_appends_to_the_draft(cx: &mut TestAppContext) {
    let projects = Projects::new("# Plan\n- ship the memo\n");
    let (handle, view, recording, _) = submit(cx, Some(&projects), "Use these notes:\n/memo");

    assert!(recording.try_recv().is_err(), "/memo is never sent");
    assert_eq!(
        draft(cx, &view),
        "Use these notes:",
        "the command left the draft"
    );
    assert!(matches!(
        preview(cx, &view),
        Some(Preview::Memo { ref project_id, ref memo }) if *project_id == projects.id && memo.text == "# Plan\n- ship the memo\n"
    ));
    assert!(shown(cx, handle, "chat-memo-insert") && shown(cx, handle, "chat-memo-edit"));

    click(cx, handle, "chat-memo-insert");
    assert_eq!(
        draft(cx, &view),
        "Use these notes:\n\n# Plan\n- ship the memo"
    );
    assert_eq!(preview(cx, &view), None);
    assert!(recording.try_recv().is_err(), "inserting sends nothing");
    // The memo is the user's: the store still has it as it was.
    assert_eq!(
        projects
            .store
            .snapshot()
            .unwrap()
            .project_memo(&projects.id)
            .text,
        "# Plan\n- ship the memo\n"
    );
}

#[gpui::test]
fn an_empty_memo_offers_edit_and_edit_asks_the_window(cx: &mut TestAppContext) {
    let projects = Projects::new("");
    let (handle, view, recording, edits) = submit(cx, Some(&projects), "/memo");
    assert!(recording.try_recv().is_err());
    assert_eq!(draft(cx, &view), "");
    assert!(!shown(cx, handle, "chat-memo-insert"), "nothing to insert");
    assert!(shown(cx, handle, "chat-memo-edit"));

    click(cx, handle, "chat-memo-edit");
    assert_eq!(*edits.borrow(), vec![projects.id.clone()]);
    assert_eq!(preview(cx, &view), None);
}

#[gpui::test]
fn a_chat_of_no_project_says_there_is_no_memo_here(cx: &mut TestAppContext) {
    let (handle, view, recording, edits) = submit(cx, None, "/memo");
    assert!(recording.try_recv().is_err());
    assert_eq!(preview(cx, &view), Some(Preview::NoProject));
    assert!(!shown(cx, handle, "chat-memo-insert"));
    assert!(!shown(cx, handle, "chat-memo-edit"));
    assert!(shown(cx, handle, "chat-memo-close"));

    // Escape closes it, and nothing was asked of the window.
    cx.update_window(handle.into(), |_, window, cx| window.press("escape", cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(preview(cx, &view), None);
    assert!(edits.borrow().is_empty());
}

#[gpui::test]
fn only_a_trailing_memo_command_is_one(cx: &mut TestAppContext) {
    let projects = Projects::new("notes");
    let (_, view, recording, _) = submit(cx, Some(&projects), "/memo please");
    assert_eq!(preview(cx, &view), None);
    match recording.try_recv() {
        Ok(feed::Delivery::Submission { command, .. }) => assert_eq!(
            command,
            crate::chat::model::ChatCommand::Send {
                text: "/memo please".into()
            }
        ),
        _ => panic!("an ordinary message is sent as it is"),
    }
}
