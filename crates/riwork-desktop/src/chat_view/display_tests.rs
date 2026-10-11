//! A chat's Normal/Verbose choice across a tab switch, closing and reopening the tab, and a
//! restart. Tabs are made with `ChatView::open_deferred`, as a restored window, a project
//! switch and a phone-opened chat make them, and the choice is saved in a settings file
//! under a temporary home instead of the user's.
use super::*;
use crate::settings::{Settings, SettingsStore};
use DisplayMode::{Normal, Verbose};
use gpui::{AnyWindowHandle, Bounds, TestAppContext, WindowBounds, WindowOptions, point, size};
use std::path::{Path, PathBuf};

fn home() -> PathBuf {
    let path = std::env::temp_dir().join(format!("riwork-display-view-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn chat_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// The app as it starts: the settings read from `home`, and the other globals a chat needs.
fn start(cx: &mut TestAppContext, home: &Path) {
    let settings = SettingsStore::open(home).unwrap().load().unwrap();
    cx.update(|cx| {
        cx.set_global(settings);
        cx.set_global(crate::theme::Appearance {
            selected: crate::theme::ThemeChoice::RiWork,
            palette: crate::theme::Palette::RIWORK,
            terminal: None,
            ghostty: None,
            error: None,
        });
        text_input::init(cx);
        crate::chat_drafts::init(home, cx);
    });
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
) -> (AnyWindowHandle, Entity<Pane>, Vec<Entity<ChatView>>) {
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
                    let tabs: Vec<_> = ids
                        .iter()
                        .map(|id| {
                            cx.new(|cx| {
                                ChatView::open_deferred(
                                    (*id).to_owned(),
                                    None,
                                    HostConfig {
                                        ensure: Arc::new(|| {
                                            Err("fixture staging is disabled".into())
                                        }),
                                    },
                                    window,
                                    cx,
                                )
                            })
                        })
                        .collect();
                    let pane = cx.new(|_| Pane {
                        tabs: tabs.clone(),
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
    cx.run_until_parked();
    (handle, pane, tabs)
}

/// What the display menu does when `mode` is picked in `view`.
fn choose(cx: &mut TestAppContext, home: &Path, view: &Entity<ChatView>, mode: DisplayMode) {
    view.update(cx, |view, cx| {
        view.choose_display_in(|| SettingsStore::open(home), mode, cx)
    });
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

fn mode(cx: &mut TestAppContext, view: &Entity<ChatView>) -> DisplayMode {
    view.read_with(cx, |view, _| view.display_mode)
}

#[gpui::test]
fn verbose_stays_after_a_tab_switch_and_closing_and_reopening_the_tab(cx: &mut TestAppContext) {
    let home = home();
    start(cx, &home);
    let (a, b) = (chat_id(), chat_id());
    let (handle, pane, tabs) = mount(cx, &[&a, &b]);
    assert_eq!(mode(cx, &tabs[0]), Normal);
    choose(cx, &home, &tabs[0], Verbose);
    show(cx, &pane, 1);
    show(cx, &pane, 0);
    assert_eq!(mode(cx, &tabs[0]), Verbose);
    assert_eq!(mode(cx, &tabs[1]), Normal);
    drop(tabs);
    close(cx, handle);
    let (_, _, tabs) = mount(cx, &[&a]);
    assert_eq!(mode(cx, &tabs[0]), Verbose);
    let _ = std::fs::remove_dir_all(home);
}

#[gpui::test]
fn the_choice_survives_a_restart(cx: &mut TestAppContext) {
    let home = home();
    start(cx, &home);
    let a = chat_id();
    let (handle, _, tabs) = mount(cx, &[&a]);
    choose(cx, &home, &tabs[0], Verbose);
    drop(tabs);
    close(cx, handle);
    // A new process reads only what is on disk.
    cx.update(|cx| cx.set_global(Settings::default()));
    start(cx, &home);
    let (_, _, tabs) = mount(cx, &[&a]);
    assert_eq!(mode(cx, &tabs[0]), Verbose);
    let _ = std::fs::remove_dir_all(home);
}

#[gpui::test]
fn a_new_chat_takes_the_setting_and_two_chats_keep_different_modes(cx: &mut TestAppContext) {
    let home = home();
    SettingsStore::open(&home)
        .unwrap()
        .update(|settings| settings.chat_display = Verbose)
        .unwrap();
    start(cx, &home);
    let (a, b, fresh) = (chat_id(), chat_id(), chat_id());
    let (handle, _, tabs) = mount(cx, &[&a, &b]);
    assert_eq!(mode(cx, &tabs[0]), Verbose);
    assert_eq!(mode(cx, &tabs[1]), Verbose);
    choose(cx, &home, &tabs[0], Normal);
    // Toggling one chat changes only that chat, and not the default.
    assert_eq!(mode(cx, &tabs[0]), Normal);
    assert_eq!(mode(cx, &tabs[1]), Verbose);
    assert_eq!(
        cx.update(|cx| cx.global::<Settings>().chat_display),
        Verbose
    );
    drop(tabs);
    close(cx, handle);
    let (_, _, tabs) = mount(cx, &[&a, &b, &fresh]);
    assert_eq!(mode(cx, &tabs[0]), Normal);
    assert_eq!(mode(cx, &tabs[1]), Verbose);
    assert_eq!(mode(cx, &tabs[2]), Verbose);
    let _ = std::fs::remove_dir_all(home);
}
