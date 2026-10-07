//! A chat's unsent draft across a tab switch, closing and reopening the tab, and a restart.
//! A view is made as `ChatView::open` makes it, with a recording feed instead of a host.
use super::*;
use crate::chat_drafts::{self, ChatDrafts, Drafts};
use gpui::{AnyWindowHandle, Bounds, TestAppContext, WindowBounds, WindowOptions, point, size};
use gpui_kit::test::TestWindowExt;
use std::{path::PathBuf, sync::mpsc::Receiver, time::SystemTime};

fn home() -> PathBuf {
    let path = std::env::temp_dir().join(format!("riwork-draft-view-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn chat_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// The app as it starts: its globals, and the drafts read from `home`.
fn start(cx: &mut TestAppContext, home: &std::path::Path) {
    cx.update(|cx| {
        cx.set_global(crate::settings::Settings::default());
        cx.set_global(crate::theme::Appearance {
            selected: crate::theme::ThemeChoice::RiWork,
            palette: crate::theme::Palette::RIWORK,
            terminal: None,
            ghostty: None,
            error: None,
        });
        text_input::init(cx);
        chat_drafts::init(home, cx);
    });
}

/// The tab of chat `id`, opened as `ChatView::open` does.
fn open_view(
    id: &str,
    window: &mut Window,
    cx: &mut gpui::App,
) -> (Entity<ChatView>, Receiver<feed::Delivery>) {
    let (recording_feed, recording) = Feed::recording();
    let view = cx.new(|cx| {
        let mut view = ChatView::blank(
            HostConfig {
                ensure: Arc::new(|| Err("fixture staging is disabled".into())),
            },
            window,
            cx,
        );
        view.chat_id = Some(id.to_owned());
        view.restore_draft(window, cx);
        view.feed = Some(recording_feed);
        view
    });
    (view, recording)
}

/// A pane: the tabs' views, of which the active one is drawn.
struct Pane {
    tabs: Vec<Entity<ChatView>>,
    active: usize,
}
impl Render for Pane {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.tabs[self.active].clone())
    }
}

/// A window with a pane of one tab per chat in `ids`, the first one shown.
fn mount(
    cx: &mut TestAppContext,
    ids: &[&str],
) -> (
    AnyWindowHandle,
    Entity<Pane>,
    Vec<(Entity<ChatView>, Receiver<feed::Delivery>)>,
) {
    let (handle, pane, tabs) = cx.update(|cx| {
        let mut made = None;
        let handle = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.), px(0.)),
                        size(px(700.), px(800.)),
                    ))),
                    ..Default::default()
                },
                |window, cx| {
                    let tabs: Vec<_> = ids.iter().map(|id| open_view(id, window, cx)).collect();
                    let pane = cx.new(|_| Pane {
                        tabs: tabs.iter().map(|(view, _)| view.clone()).collect(),
                        active: 0,
                    });
                    made = Some((pane.clone(), tabs));
                    cx.new(|cx| gpui_kit::base::Root::new(pane, window, cx))
                },
            )
            .unwrap();
        let (pane, tabs) = made.unwrap();
        (handle.into(), pane, tabs)
    });
    cx.update_window(handle, |_, window, _| window.activate_window())
        .unwrap();
    cx.run_until_parked();
    (handle, pane, tabs)
}

fn type_into(cx: &mut TestAppContext, handle: AnyWindowHandle, text: &str) {
    cx.update_window(handle, |_, window, cx| {
        window.click("chat-composer", cx);
        window.input(text, cx);
    })
    .unwrap();
    cx.run_until_parked();
}

fn show(cx: &mut TestAppContext, pane: &Entity<Pane>, tab: usize) {
    pane.update(cx, |pane, cx| {
        pane.active = tab;
        cx.notify();
    });
    cx.run_until_parked();
}

/// Close the window and with it every tab's view, as closing a tab drops its view.
fn close(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    cx.update_window(handle, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
}

fn text(cx: &mut TestAppContext, view: &Entity<ChatView>) -> String {
    view.read_with(cx, |v, cx| v.composer_text(cx))
}

#[gpui::test]
fn each_chat_keeps_its_draft_while_another_tab_is_shown(cx: &mut TestAppContext) {
    let home = home();
    start(cx, &home);
    let (a, b) = (chat_id(), chat_id());
    let (handle, pane, tabs) = mount(cx, &[&a, &b]);
    type_into(cx, handle, "draft of A 🦀");
    show(cx, &pane, 1);
    type_into(cx, handle, "draft of B");
    show(cx, &pane, 0);
    assert_eq!(text(cx, &tabs[0].0), "draft of A 🦀");
    assert_eq!(text(cx, &tabs[1].0), "draft of B");
    // Typing goes on where it stopped: the hidden tab's editor is the same one.
    type_into(cx, handle, "!");
    assert_eq!(text(cx, &tabs[0].0), "draft of A 🦀!");
    assert!(
        tabs.iter()
            .all(|(_, recording)| recording.try_recv().is_err())
    );
    let _ = std::fs::remove_dir_all(home);
}

#[gpui::test]
fn a_closed_tab_comes_back_with_its_draft_and_another_chat_without(cx: &mut TestAppContext) {
    let home = home();
    start(cx, &home);
    let (a, b) = (chat_id(), chat_id());
    let (handle, _, tabs) = mount(cx, &[&a]);
    type_into(cx, handle, "half a thought\n  indented");
    drop(tabs);
    close(cx, handle);
    let (_, _, tabs) = mount(cx, &[&a, &b]);
    assert_eq!(text(cx, &tabs[0].0), "half a thought\n  indented");
    assert_eq!(text(cx, &tabs[1].0), "");
    let _ = std::fs::remove_dir_all(home);
}

#[gpui::test]
fn a_draft_survives_a_restart_until_it_is_sent(cx: &mut TestAppContext) {
    let home = home();
    start(cx, &home);
    let a = chat_id();
    let (handle, _, tabs) = mount(cx, &[&a]);
    type_into(cx, handle, "before the restart");
    drop(tabs);
    close(cx, handle);
    // A new process reads only what is on disk.
    cx.update(|cx| cx.set_global(ChatDrafts(Drafts::load(&home, SystemTime::now()))));
    let (handle, _, tabs) = mount(cx, &[&a]);
    let (view, recording) = &tabs[0];
    assert_eq!(text(cx, view), "before the restart");
    // Restored text is a draft like any other: sending it clears it everywhere.
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |v, cx| v.send_message(window, cx))
    })
    .unwrap();
    let (id, command) = match recording.try_recv().unwrap() {
        feed::Delivery::Submission { id, command } => (id, command),
        _ => panic!("the composer sends one correlated submission"),
    };
    assert_eq!(
        command,
        ChatCommand::Send {
            text: "before the restart".into()
        }
    );
    // Until the host takes it, the draft stays, also on disk.
    assert_eq!(
        Drafts::load(&home, SystemTime::now()).get(&a),
        Some("before the restart")
    );
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |v, cx| {
            v.submission_receipt(id, command, Ok(()), window, cx)
        })
    })
    .unwrap();
    assert_eq!(text(cx, view), "");
    assert_eq!(cx.update(|cx| chat_drafts::draft(&a, cx)), None);
    assert_eq!(Drafts::load(&home, SystemTime::now()).get(&a), None);
    let _ = std::fs::remove_dir_all(home);
}
