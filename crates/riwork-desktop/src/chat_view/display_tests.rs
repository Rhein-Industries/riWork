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
                                let mut view = ChatView::open_deferred(
                                    (*id).to_owned(),
                                    None,
                                    HostConfig {
                                        ensure: Arc::new(|| {
                                            Err("fixture staging is disabled".into())
                                        }),
                                    },
                                    window,
                                    cx,
                                );
                                // These fixtures project local state; real socket threads can
                                // wake GPUI's deterministic executor from a foreign thread.
                                view.feed_started = true;
                                view
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

pub(super) fn tool_transcript() -> crate::chat::model::Transcript {
    use crate::chat::model::{Item, ItemStatus, MessagePhase, Presentation, Transcript};
    let make = |id: &str, body| Item {
        id: id.into(),
        turn_id: Some("turn".into()),
        status: ItemStatus::Completed,
        presentation: Presentation::default(),
        body,
        started_at: None,
        ended_at: None,
    };
    let mut transcript = Transcript::default();
    transcript.items = vec![
        make(
            "user",
            ItemBody::UserMessage {
                text: "Inspect the project".into(),
            },
        ),
        make(
            "read",
            ItemBody::Command {
                command: "cat src/main.rs".into(),
                cwd: None,
                output: "fn main() {}".into(),
                exit_code: Some(0),
            },
        ),
        make(
            "search",
            ItemBody::Command {
                command: "rg main src".into(),
                cwd: None,
                output: "src/main.rs:1:fn main() {}".into(),
                exit_code: Some(0),
            },
        ),
        make(
            "build",
            ItemBody::Command {
                command: "cargo build".into(),
                cwd: None,
                output: "error: fixture".into(),
                exit_code: Some(1),
            },
        ),
        make(
            "answer",
            ItemBody::AgentMessage {
                text: "The build needs a fix.".into(),
            },
        ),
    ];
    transcript.items[3].status = ItemStatus::Failed;
    transcript.items[4].presentation.phase = Some(MessagePhase::Final);
    transcript
}

fn install_tools(cx: &mut TestAppContext, view: &Entity<ChatView>) {
    view.update(cx, |view, cx| {
        view.model.transcript = tool_transcript();
        view.model.link = Link::Live;
        view.refresh_projection(cx);
        cx.notify();
    });
    cx.run_until_parked();
}

#[gpui::test]
fn shared_projection_starts_closed_and_manual_children_survive_activity_reopening(
    cx: &mut TestAppContext,
) {
    let home = home();
    start(cx, &home);
    let id = chat_id();
    let (_, _, tabs) = mount(cx, &[&id]);
    let view = &tabs[0];
    install_tools(cx, view);
    view.update(cx, |view, cx| {
        assert_eq!(
            view.visible.iter().map(|row| row.kind).collect::<Vec<_>>(),
            [
                display::RowKind::User,
                display::RowKind::Activity,
                display::RowKind::Final
            ]
        );
        assert!(view.open.is_empty(), "failed rows never open themselves");
        view.list.scroll_to(gpui::ListOffset {
            item_ix: 1,
            offset_in_item: px(3.),
        });
        let anchor = view.list.logical_scroll_top();
        view.toggle_projected("activity:turn", cx);
        assert!(!view.list.is_following_tail());
        assert_eq!(view.list.logical_scroll_top().item_ix, anchor.item_ix);
        assert_eq!(
            view.list.logical_scroll_top().offset_in_item,
            anchor.offset_in_item
        );
        assert!(!display::find(&view.visible, "g:turn:read").unwrap().open);
        view.toggle_projected("g:turn:read", cx);
        view.toggle_projected("read", cx);
        view.toggle_projected("activity:turn", cx);
        assert!(!view.visible[1].open);
        view.toggle_projected("activity:turn", cx);
        assert!(display::find(&view.visible, "read").unwrap().open);
        // Streaming reprojects without replacing a deliberate disclosure.
        view.model.transcript.items[1].status = crate::chat::model::ItemStatus::InProgress;
        view.refresh_projection(cx);
        assert!(display::find(&view.visible, "read").unwrap().open);
        assert!(!display::find(&view.visible, "build").unwrap().open);
    });
    let _ = std::fs::remove_dir_all(home);
}

#[gpui::test]
fn mode_and_bulk_resets_obey_the_shared_normal_and_verbose_scopes(cx: &mut TestAppContext) {
    let home = home();
    start(cx, &home);
    let id = chat_id();
    let (_, _, tabs) = mount(cx, &[&id]);
    let view = &tabs[0];
    install_tools(cx, view);
    view.update(cx, |view, cx| {
        view.toggle_projected("activity:turn", cx);
        view.toggle_projected("build", cx);
        view.expand_transcript(true, cx);
        assert!(view.visible[1].open);
        assert!(!display::find(&view.visible, "build").unwrap().open);
        assert_eq!(view.disclosure.overrides.get("activity:turn"), Some(&true));
        view.apply_display_mode(Verbose, cx);
        assert!(view.open.is_empty());
        view.expand_transcript(true, cx);
        assert!(display::find(&view.visible, "read").unwrap().open);
        assert!(display::find(&view.visible, "build").unwrap().open);
        view.toggle_projected("build", cx);
        view.refresh_projection(cx);
        assert!(!display::find(&view.visible, "build").unwrap().open);
        view.expand_transcript(false, cx);
        assert!(view.open.is_empty());
        view.apply_display_mode(Normal, cx);
        assert!(!view.visible[1].open);
        assert!(view.disclosure.overrides.is_empty());
        assert!(view.disclosure.bulk.is_none());
    });
    let _ = std::fs::remove_dir_all(home);
}

#[gpui::test]
fn transcript_starts_below_the_toolbar_without_a_bulk_disclosure_row(cx: &mut TestAppContext) {
    use gpui_kit::test::TestWindowExt;
    let home = home();
    start(cx, &home);
    let id = chat_id();
    let (handle, _, tabs) = mount(cx, &[&id]);
    let view = &tabs[0];
    install_tools(cx, view);
    for mode in [Normal, Verbose] {
        view.update(cx, |view, cx| view.apply_display_mode(mode, cx));
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("transcript-expand-all").is_none());
            assert!(window.try_find("transcript-collapse-all").is_none());
            let toolbar_bottom = window.find("chat-toolbar").bounds().bottom();
            view.read_with(cx, |view, _| {
                assert_eq!(
                    view.list.viewport_bounds().top(),
                    toolbar_bottom,
                    "{mode:?} must not reserve a row for bulk actions or shortcut hints"
                );
            });
        })
        .unwrap();
    }
    let _ = std::fs::remove_dir_all(home);
}

#[gpui::test]
fn transcript_actions_and_arrow_space_keys_are_scoped_away_from_editors(cx: &mut TestAppContext) {
    use gpui::{Focusable, KeyBinding};
    use gpui_kit::test::TestWindowExt;
    let home = home();
    start(cx, &home);
    cx.update(|cx| {
        cx.bind_keys([
            KeyBinding::new("cmd-shift-v", ToggleTranscriptMode, Some("ChatView")),
            KeyBinding::new("cmd-alt-right", ExpandTranscript, Some("ChatView")),
            KeyBinding::new("cmd-alt-left", CollapseTranscript, Some("ChatView")),
        ])
    });
    let id = chat_id();
    let (handle, _, tabs) = mount(cx, &[&id]);
    let view = &tabs[0];
    install_tools(cx, view);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| view.focus.focus(window, cx));
        window.press("cmd-alt-right", cx);
        view.read_with(cx, |view, _| assert!(view.visible[1].open));
        window.press("cmd-alt-left", cx);
        window.press("down", cx);
        view.read_with(cx, |view, _| {
            assert_eq!(view.focused_row.as_deref(), Some("activity:turn"))
        });
        window.press("space", cx);
        view.read_with(cx, |view, _| assert!(view.visible[1].open));
        window.press("down", cx);
        view.read_with(cx, |view, _| {
            assert_eq!(view.focused_row.as_deref(), Some("g:turn:read"))
        });
        view.update(cx, |view, cx| {
            view.composer.read(cx).focus_handle(cx).focus(window, cx)
        });
        window.press("down", cx);
        window.press("space", cx);
        view.read_with(cx, |view, cx| {
            assert_eq!(view.focused_row.as_deref(), Some("g:turn:read"));
            assert_eq!(view.composer.read(cx).value(), " ");
        });
        // Anonymous chats retain their pending mode without opening a settings store.
        view.update(cx, |view, cx| {
            view.chat_id = None;
            view.focus.focus(window, cx);
        });
        window.press("cmd-shift-v", cx);
        view.read_with(cx, |view, _| {
            assert_eq!(view.display_mode, Verbose);
            assert!(view.open.is_empty());
            assert!(view.focused_row.is_none());
        });
        window.press("cmd-alt-right", cx);
        view.read_with(cx, |view, _| {
            assert!(display::find(&view.visible, "read").unwrap().open);
            assert!(display::find(&view.visible, "build").unwrap().open);
        });
        window.press("cmd-alt-left", cx);
        view.read_with(cx, |view, _| {
            assert!(!display::find(&view.visible, "read").unwrap().open);
            assert!(!display::find(&view.visible, "build").unwrap().open);
        });
    })
    .unwrap();
    let _ = std::fs::remove_dir_all(home);
}

#[gpui::test]
fn hover_and_keyboard_focus_use_themed_chrome_without_persistent_pointer_selection(
    cx: &mut TestAppContext,
) {
    use gpui_kit::test::TestWindowExt;
    let home = home();
    start(cx, &home);
    // Verify the hover animation's endpoints deterministically. The production
    // transition is animated; reduced motion paints each endpoint without a
    // wall-clock wait or a dependency on how many frames the executor draws.
    cx.update(|cx| cx.set_reduce_motion(true));
    let id = chat_id();
    let (handle, _, tabs) = mount(cx, &[&id]);
    let view = &tabs[0];
    install_tools(cx, view);
    let painted_border = |window: &Window| {
        let bounds = window
            .find("card:activity:turn")
            .bounds()
            .scale(window.scale_factor());
        window.painted_quads().into_iter().find(|quad| {
            quad.bounds == bounds && quad.border_widths.top.0 > 0.0 && quad.border_color.a > 0.0
        })
    };
    let painted_fill = |window: &Window| {
        let bounds = window
            .find("card:activity:turn")
            .bounds()
            .scale(window.scale_factor());
        window.painted_quads().into_iter().find_map(|quad| {
            (quad.bounds.origin.x >= bounds.origin.x
                && quad.bounds.origin.y >= bounds.origin.y
                && quad.bounds.right() <= bounds.right()
                && quad.bounds.bottom() <= bounds.bottom()
                && quad.bounds.size.width.0 >= bounds.size.width.0 - 4.0
                && quad.bounds.size.height.0 >= bounds.size.height.0 - 4.0)
                .then(|| quad.background.as_solid())
                .flatten()
                .filter(|color| !color.is_transparent())
        })
    };
    let no_pointer_border = |window: &Window| {
        assert!(
            painted_border(window).is_none(),
            "pointer hover/click must never paint a row border"
        );
    };
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.hover("card:activity:turn", cx);
        no_pointer_border(window);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            painted_fill(window),
            Some(gpui::Hsla::from(rgb(Look::of(cx).colors.divider))),
            "hover fades to the theme's divider tint"
        );
        no_pointer_border(window);
        window.hover("chat-composer-shell", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let fill = painted_fill(window);
        assert!(
            fill.is_none(),
            "leaving the row clears hover tint: {fill:?}"
        );
        no_pointer_border(window);
        view.update(cx, |view, cx| view.focus.focus(window, cx));
        for _ in 0..40 {
            window.press("tab", cx);
            if window.find("card:activity:turn").focused() == Some(true) {
                break;
            }
        }
        assert_eq!(window.find("card:activity:turn").focused(), Some(true));
        let ring = painted_border(window).expect("Tab focus needs a visible row border");
        assert!(
            ring.border_color.a <= 0.5,
            "the keyboard ring must stay subtle: {ring:?}"
        );
        assert_eq!(
            window.find("card:activity:turn").bounds().size.height,
            px(24.0)
        );
        window.hover("card:activity:turn", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("card:activity:turn").focused(),
            Some(true),
            "entering a keyboard-focused header must preserve its focus identity"
        );
        window.click_at("card:activity:turn", point(px(8.), px(12.)), cx);
        no_pointer_border(window);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        no_pointer_border(window);
        window.press("space", cx);
        let ring =
            painted_border(window).expect("Space after a pointer click restores keyboard focus");
        assert!(ring.border_color.a <= 0.5);
        window.click_at("card:activity:turn", point(px(8.), px(12.)), cx);
        no_pointer_border(window);
        view.update(cx, |view, _| view.focused_row = None);
        window.press("down", cx);
        let ring = painted_border(window).expect("arrow focus needs a visible row border");
        assert!(ring.border_color.a <= 0.5);
        window.click_at("card:activity:turn", point(px(8.), px(12.)), cx);
        no_pointer_border(window);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        no_pointer_border(window);
        window.hover("chat-composer-shell", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            painted_fill(window).is_none(),
            "a pointer click cannot leave a selection background after hover ends"
        );
        no_pointer_border(window);
    })
    .unwrap();
    let _ = std::fs::remove_dir_all(home);
}

#[gpui::test]
fn hover_tint_stays_continuous_across_every_header_child_region(cx: &mut TestAppContext) {
    use crate::chat::model::{ChatImage, ImageSource, ItemStatus, Semantics, ToolKind};
    use gpui::{InputEvent, MouseMoveEvent};
    use gpui_kit::test::TestWindowExt;
    let home = home();
    start(cx, &home);
    cx.update(|cx| cx.set_reduce_motion(false));
    let id = chat_id();
    let (handle, _, tabs) = mount(cx, &[&id]);
    let view = &tabs[0];
    install_tools(cx, view);
    view.update(cx, |view, cx| {
        let template = view.model.transcript.items[1].clone();
        let mut thought = template.clone();
        thought.id = "thought".into();
        thought.body = ItemBody::Reasoning {
            text: "Check how the child regions share their header.".into(),
        };
        let mut subagent = template.clone();
        subagent.id = "subagent".into();
        subagent.body = ItemBody::ToolCall {
            server: None,
            tool: "Task".into(),
            input: serde_json::json!({ "description": "Review header hover" }),
            output: Some("Reviewed".into()),
        };
        subagent.presentation.semantics = Some(Semantics {
            kind: ToolKind::Subagent,
            target: Some("reviewer".into()),
            ..Default::default()
        });
        let mut image = template.clone();
        image.id = "shot".into();
        image.body = ItemBody::ToolCall {
            server: None,
            tool: "Read".into(),
            input: serde_json::json!({ "file_path": "/tmp/header-hover.png" }),
            output: Some("[image]".into()),
        };
        image.presentation.semantics = Some(Semantics {
            kind: ToolKind::Read,
            target: Some("/tmp/header-hover.png".into()),
            ..Default::default()
        });
        // A thumbnail placeholder exercises the same child layout without disk
        // reads or a decoded-image worker changing the fixture during the sweep.
        image.presentation.images = vec![ChatImage {
            label: "header-hover.png".into(),
            source: ImageSource::Unavailable {
                reason: "hover fixture".into(),
            },
        }];
        let mut running = template;
        running.id = "running".into();
        running.status = ItemStatus::InProgress;
        running.body = ItemBody::Command {
            command: "cargo test --workspace".into(),
            cwd: Some("/tmp/header-hover".into()),
            output: "Checking the workspace".into(),
            exit_code: None,
        };
        let answer = view.model.transcript.items.pop().unwrap();
        view.model
            .transcript
            .items
            .extend([thought, subagent, image, running, answer]);
        for item in &mut view.model.transcript.items {
            item.started_at = Some(1_000);
            item.ended_at = (item.status != ItemStatus::InProgress).then_some(2_000);
        }
        view.refresh_projection(cx);
        cx.notify();
    });
    cx.run_until_parked();

    for (mode, rows) in [
        (Normal, &["activity:turn"][..]),
        (
            Verbose,
            &[
                "g:turn:read",
                "build",
                "thought",
                "subagent",
                "shot",
                "running",
            ][..],
        ),
    ] {
        view.update(cx, |view, cx| view.apply_display_mode(mode, cx));
        cx.run_until_parked();
        for row in rows {
            let (bounds, was_open) = cx
                .update_window(handle, |_, window, cx| {
                    window.hover("chat-composer-shell", cx);
                    window.render_frame(cx);
                    let bounds = window.find(format!("card:{row}")).bounds();
                    let open = display::find(&view.read(cx).visible, row).unwrap().open;
                    assert_eq!(bounds.size.height, px(24.0), "{row}");
                    window.hover(format!("card:{row}"), cx);
                    (bounds, open)
                })
                .unwrap();
            // GPUI's non-synced animation uses wall time. Complete only the
            // initial fade, then check every move in its immediate frame so
            // leaving/re-entering a child cannot hide a restarted animation.
            std::thread::sleep(std::time::Duration::from_millis(120));
            // Sample the chevron, icon, text, metadata, trailing actions and
            // gaps at the top, center and bottom of the complete header.
            let width = f32::from(bounds.size.width);
            let height = f32::from(bounds.size.height);
            let mut samples = (3..width as usize - 2)
                .step_by(32)
                .map(|x| x as f32)
                .chain([12.0, 24.0, 34.0, 46.0, 60.0])
                .chain([width - 12.0, width - 36.0, width - 60.0, width - 84.0])
                .filter(|x| *x > 0.0 && *x < width)
                .collect::<Vec<_>>();
            samples.sort_by(f32::total_cmp);
            samples.dedup();
            for y in [0.5, height / 2.0, height - 0.5] {
                for &x in &samples {
                    let position = bounds.origin + point(px(x), px(y));
                    cx.update_window(handle, |_, window, cx| {
                        window.dispatch_event(
                            MouseMoveEvent {
                                position,
                                pressed_button: None,
                                modifiers: Default::default(),
                            }
                            .to_platform_input(),
                            cx,
                        );
                        window.render_frame(cx);
                        let scaled = bounds.scale(window.scale_factor());
                        let tint = gpui::Hsla::from(rgb(Look::of(cx).colors.divider));
                        assert!(
                            window.painted_quads().iter().any(|quad| {
                                quad.bounds.origin.x >= scaled.origin.x
                                    && quad.bounds.origin.y >= scaled.origin.y
                                    && quad.bounds.right() <= scaled.right()
                                    && quad.bounds.bottom() <= scaled.bottom()
                                    && quad.bounds.size.width.0 >= scaled.size.width.0 - 4.0
                                    && quad.bounds.size.height.0 >= scaled.size.height.0 - 4.0
                                    && quad.background.as_solid() == Some(tint)
                            }),
                            "{row} lost its full-header hover tint at ({x}, {y})"
                        );
                        view.read_with(cx, |view, _| {
                            assert_eq!(
                                display::find(&view.visible, row).unwrap().open,
                                was_open,
                                "hovering {row} must not activate its disclosure"
                            );
                        });
                    })
                    .unwrap();
                }
            }
            cx.update_window(handle, |_, window, cx| {
                window.hover("chat-composer-shell", cx);
            })
            .unwrap();
            cx.run_until_parked();
        }
    }
    let _ = std::fs::remove_dir_all(home);
}

#[gpui::test]
fn backfill_remaps_manual_group_disclosure_and_keyboard_focus_by_item(cx: &mut TestAppContext) {
    let home = home();
    start(cx, &home);
    let id = chat_id();
    let (_, _, tabs) = mount(cx, &[&id]);
    let view = &tabs[0];
    install_tools(cx, view);
    view.update(cx, |view, cx| {
        view.apply_display_mode(Verbose, cx);
        view.toggle_projected("g:turn:read", cx);
        view.toggle_projected("read", cx);
        view.focused_row = Some("g:turn:read".into());
        let mut backfilled = view.model.transcript.items[1].clone();
        backfilled.id = "backfill".into();
        view.model.transcript.items.insert(1, backfilled);
        view.refresh_projection(cx);
        assert!(
            display::find(&view.visible, "g:turn:backfill")
                .unwrap()
                .open
        );
        assert!(display::find(&view.visible, "read").unwrap().open);
        assert_eq!(view.focused_row.as_deref(), Some("g:turn:backfill"));
        assert!(!view.disclosure.overrides.contains_key("g:turn:read"));
    });
    let _ = std::fs::remove_dir_all(home);
}

#[gpui::test]
fn expand_all_does_not_open_later_calls_and_streaming_regroup_preserves_manual_open(
    cx: &mut TestAppContext,
) {
    let home = home();
    start(cx, &home);
    let id = chat_id();
    let (_, _, tabs) = mount(cx, &[&id]);
    let view = &tabs[0];
    install_tools(cx, view);
    view.update(cx, |view, cx| {
        view.apply_display_mode(Verbose, cx);
        view.expand_transcript(true, cx);
        let mut later = view.model.transcript.items[3].clone();
        later.id = "later-build".into();
        view.model.transcript.items.insert(4, later);
        view.refresh_projection(cx);
        assert!(display::find(&view.visible, "build").unwrap().open);
        assert!(!display::find(&view.visible, "later-build").unwrap().open);
        view.apply_display_mode(Normal, cx);
        view.expand_transcript(true, cx);
        let mut next_turn = view.model.transcript.items[3].clone();
        next_turn.id = "next-turn-build".into();
        next_turn.turn_id = Some("next-turn".into());
        view.model.transcript.items.push(next_turn);
        view.refresh_projection(cx);
        assert!(
            !display::find(&view.visible, "activity:next-turn")
                .unwrap()
                .open
        );

        view.apply_display_mode(Verbose, cx);
        view.model.transcript.items = tool_transcript().items;
        view.model.transcript.items.remove(2);
        view.refresh_projection(cx);
        view.toggle_projected("read", cx);
        let mut later_read = view.model.transcript.items[1].clone();
        later_read.id = "later-read".into();
        view.model.transcript.items.insert(2, later_read);
        view.refresh_projection(cx);
        assert!(display::find(&view.visible, "g:turn:read").unwrap().open);
        assert!(display::find(&view.visible, "read").unwrap().open);
        assert!(!display::find(&view.visible, "later-read").unwrap().open);
    });
    let _ = std::fs::remove_dir_all(home);
}

#[gpui::test]
fn keyboard_focus_reveals_nested_header_inside_a_tall_activity(cx: &mut TestAppContext) {
    use gpui_kit::test::TestWindowExt;
    let home = home();
    start(cx, &home);
    let id = chat_id();
    let (handle, _, tabs) = mount(cx, &[&id]);
    let view = &tabs[0];
    install_tools(cx, view);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.toggle_projected("activity:turn", cx);
            view.toggle_projected("g:turn:read", cx);
            view.toggle_projected("read", cx);
            if let ItemBody::Command { output, .. } = &mut view.model.transcript.items[1].body {
                *output = (0..100).map(|n| format!("line {n}\n")).collect();
            }
            view.open.insert("output-all:read".into());
            view.list.remeasure();
            view.list.scroll_to(gpui::ListOffset {
                item_ix: 1,
                offset_in_item: px(0.),
            });
            view.focused_row = Some("read".into());
        });
        window.render_frame(cx);
        view.update(cx, |view, cx| view.focus_transcript_row(1, cx));
        window.render_frame(cx);
        view.read_with(cx, |view, _| {
            assert_eq!(view.focused_row.as_deref(), Some("search"));
            let geometry = view.header_bounds.borrow();
            let header = geometry.get("search").expect("nested header was laid out");
            let viewport = view.list.viewport_bounds();
            assert!(header.top() >= viewport.top(), "{header:?} / {viewport:?}");
            assert!(
                header.bottom() <= viewport.bottom(),
                "{header:?} / {viewport:?}"
            );
        });
    })
    .unwrap();
    let _ = std::fs::remove_dir_all(home);
}

#[gpui::test]
fn streaming_output_and_clock_reuse_cached_rows_and_completion_stays_closed(
    cx: &mut TestAppContext,
) {
    use crate::chat::{
        model::{ChatEvent, Delta, Item, ItemStatus, Presentation, TurnOutcome},
        wire::Envelope,
    };
    let home = home();
    start(cx, &home);
    let id = chat_id();
    let (_, _, tabs) = mount(cx, &[&id]);
    tabs[0].update(cx, |view, cx| {
        let call = Item {
            id: "stream".into(),
            turn_id: Some("live".into()),
            status: ItemStatus::InProgress,
            started_at: Some(2_000),
            ended_at: None,
            presentation: Presentation::default(),
            body: ItemBody::Command {
                command: "cargo test".into(),
                cwd: None,
                output: String::new(),
                exit_code: None,
            },
        };
        let applied = view.model.apply(&[
            Envelope {
                chat_id: id.clone(),
                seq: 1,
                event: ChatEvent::TurnStarted {
                    turn_id: "live".into(),
                    started_at: Some(1_000),
                },
            },
            Envelope {
                chat_id: id.clone(),
                seq: 2,
                event: ChatEvent::ItemStarted { item: call },
            },
        ]);
        view.sync_list(&applied, cx);
        let runs = view.projection_runs;
        let source_revision = view.model.transcript.revision();
        for seq in 3..103 {
            let applied = view.model.apply(&[Envelope {
                chat_id: id.clone(),
                seq,
                event: ChatEvent::ItemDelta {
                    item_id: "stream".into(),
                    delta: Delta::Output("another line\n".into()),
                },
            }]);
            assert!(!applied.projection_changed);
            view.sync_list(&applied, cx);
        }
        assert!(view.model.transcript.revision() > source_revision);
        view.tick_activity(Some(19_000), cx);
        assert_eq!(
            view.projection_runs, runs,
            "body growth and ticks never regroup"
        );
        assert_eq!(view.visible[0].title, "Working 18s · running cargo test");
        assert!(!view.visible[0].open);
        let applied = view.model.apply(&[Envelope {
            chat_id: id.clone(),
            seq: 103,
            event: ChatEvent::TurnCompleted {
                turn_id: "live".into(),
                outcome: TurnOutcome::Completed,
                ended_at: Some(20_000),
            },
        }]);
        view.sync_list(&applied, cx);
        assert_eq!(view.projection_runs, runs + 1);
        assert_eq!(view.visible[0].title, "Worked 19s · 1 command");
        assert!(!view.visible[0].open);
        assert!(!view.visible[0].children[0].open);
    });
    let _ = std::fs::remove_dir_all(home);
}

#[gpui::test]
fn paced_streaming_headers_and_open_bodies_stay_inside_their_allocated_rows(
    cx: &mut TestAppContext,
) {
    use crate::chat::{
        model::{
            ChatEvent, Delta, Item, ItemStatus, Presentation, Semantics, ToolKind, TurnOutcome,
        },
        wire::Envelope,
    };
    use gpui::Pixels;
    use gpui_kit::test::TestWindowExt;
    let home = home();
    start(cx, &home);
    cx.update(|cx| cx.set_reduce_motion(true));
    for mode in [Normal, Verbose] {
        let id = chat_id();
        let (handle, _, tabs) = mount(cx, &[&id]);
        let view = &tabs[0];
        let make = |id: &str, status, body| Item {
            id: id.into(),
            turn_id: Some("live".into()),
            status,
            presentation: Presentation::default(),
            body,
            started_at: Some(2_000),
            ended_at: (status != ItemStatus::InProgress).then_some(3_000),
        };
        let mut stream = make(
            "stream",
            ItemStatus::InProgress,
            ItemBody::Command {
                command: "cargo test".into(),
                cwd: Some("/tmp/stream-layout".into()),
                output: String::new(),
                exit_code: None,
            },
        );
        let mut seq = 0;
        let mut apply = |events: Vec<ChatEvent>, cx: &mut TestAppContext| {
            view.update(cx, |view, cx| {
                let events: Vec<_> = events
                    .into_iter()
                    .map(|event| {
                        seq += 1;
                        Envelope {
                            chat_id: id.clone(),
                            seq,
                            event,
                        }
                    })
                    .collect();
                let applied = view.model.apply(&events);
                view.sync_list(&applied, cx);
                cx.notify();
            });
            cx.run_until_parked();
        };
        let expect_open_body = std::cell::Cell::new(false);
        let assert_layout = |stage: &str, cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, cx| {
                view.update(cx, |view, _| view.header_bounds.borrow_mut().clear());
                window.render_frame(cx);
                view.read_with(cx, |view, _| {
                    let contains = |outer: Bounds<Pixels>, inner: Bounds<Pixels>| {
                        inner.left() >= outer.left() - px(0.5)
                            && inner.right() <= outer.right() + px(0.5)
                            && inner.top() >= outer.top() - px(0.5)
                            && inner.bottom() <= outer.bottom() + px(0.5)
                    };
                    let geometry = view.header_bounds.borrow();
                    let mut previous: Option<(usize, Bounds<Pixels>)> = None;
                    let mut headers = 0;
                    for (ix, entry) in view.list_rows.iter().enumerate() {
                        let Some(actual) = window.try_find(format!("transcript-row:{ix}")) else {
                            continue;
                        };
                        let actual = actual.bounds();
                        let allocated = view
                            .list
                            .bounds_for_item(ix)
                            .expect("a painted transcript row must have a measured allocation");
                        assert!(
                            contains(allocated, actual),
                            "{mode:?}/{stage}: row {ix} {} painted {actual:?} outside cached allocation {allocated:?}",
                            entry.row.id
                        );
                        if let Some((before, previous)) = previous {
                            assert!(
                                previous.bottom() <= actual.top() + px(0.5),
                                "{mode:?}/{stage}: row {before} {previous:?} overlaps row {ix} {actual:?}"
                            );
                        }
                        previous = Some((ix, actual));
                        if let Some(header) = window.try_find(format!("card:{}", entry.row.id)) {
                            headers += 1;
                            let header = header.bounds();
                            assert!(
                                contains(actual, header),
                                "{mode:?}/{stage}: header {} {header:?} escapes row {actual:?}",
                                entry.row.id
                            );
                            let label = geometry
                                .get(&format!("label:{}", entry.row.id))
                                .expect("every painted header records its actual text bounds");
                            assert!(
                                contains(header, *label),
                                "{mode:?}/{stage}: label {} {label:?} overflows its 24px header {header:?}",
                                entry.row.id
                            );
                        }
                        if let Some(body) = window.try_find(format!("output-block:{}", entry.row.id)) {
                            let body = body.bounds();
                            assert!(
                                contains(actual, body),
                                "{mode:?}/{stage}: open body {} {body:?} escapes row {actual:?}",
                                entry.row.id
                            );
                            let painted = body.scale(window.scale_factor());
                            assert!(
                                window.painted_quads().iter().any(|quad| {
                                    (quad.bounds.origin.x.0 - painted.origin.x.0).abs() <= 1.0
                                        && (quad.bounds.origin.y.0 - painted.origin.y.0).abs() <= 1.0
                                        && (quad.bounds.size.width.0 - painted.size.width.0).abs()
                                            <= 1.0
                                        && (quad.bounds.size.height.0 - painted.size.height.0).abs()
                                            <= 1.0
                                }),
                                "{mode:?}/{stage}: open body {} must be painted at its measured bounds",
                                entry.row.id
                            );
                        }
                    }
                    assert!(headers > 0, "{mode:?}/{stage}: fixture painted no headers");
                    if expect_open_body.get() {
                        assert!(
                            display::find(&view.visible, "stream").unwrap().open,
                            "{mode:?}/{stage}: streamed replacements must preserve deliberate disclosure"
                        );
                        assert!(
                            window.try_find("output-block:stream").is_some(),
                            "{mode:?}/{stage}: the open streamed body must be mounted"
                        );
                    }
                });
            })
            .unwrap();
        };
        view.update(cx, |view, cx| {
            view.model.link = Link::Live;
            view.apply_display_mode(mode, cx);
        });
        apply(
            vec![
                ChatEvent::TurnStarted {
                    turn_id: "live".into(),
                    started_at: Some(1_000),
                },
                ChatEvent::ItemStarted {
                    item: make(
                        "user",
                        ItemStatus::Completed,
                        ItemBody::UserMessage {
                            text: "Stream the command while keeping adjacent rows separate.".into(),
                        },
                    ),
                },
                ChatEvent::ItemStarted {
                    item: make(
                        "before",
                        ItemStatus::Completed,
                        ItemBody::Command {
                            command: "cargo metadata --no-deps".into(),
                            cwd: None,
                            output: "Neighbour before".into(),
                            exit_code: Some(0),
                        },
                    ),
                },
                ChatEvent::ItemStarted {
                    item: stream.clone(),
                },
                ChatEvent::ItemStarted {
                    item: make(
                        "after",
                        ItemStatus::Completed,
                        ItemBody::Command {
                            command: "cargo check".into(),
                            cwd: None,
                            output: "Neighbour after".into(),
                            exit_code: Some(0),
                        },
                    ),
                },
                ChatEvent::ItemStarted {
                    item: make(
                        "answer",
                        ItemStatus::Completed,
                        ItemBody::AgentMessage {
                            text: "The next visible message stays below the live activity.".into(),
                        },
                    ),
                },
            ],
            cx,
        );
        assert_layout("short running label", cx);
        if let ItemBody::Command { command, .. } = &mut stream.body {
            *command = "cargo test\nsecond command line\nthird command line".into();
            stream.presentation.semantics = Some(Semantics {
                kind: ToolKind::Shell,
                target: Some(command.clone()),
                ..Default::default()
            });
        }
        apply(
            vec![ChatEvent::ItemStarted {
                item: stream.clone(),
            }],
            cx,
        );
        assert_layout("short multiline running replacement", cx);
        if let ItemBody::Command { command, cwd, .. } = &mut stream.body {
            *command = format!(
                "cargo test --package riwork-desktop {}\nsecond command line\nthird command line",
                "--test-argument-that-must-truncate ".repeat(8)
            );
            *cwd = Some(format!("/tmp/{}", "long-streaming-path/".repeat(12)));
            stream.presentation.semantics = Some(Semantics {
                kind: ToolKind::Shell,
                target: Some(command.clone()),
                ..Default::default()
            });
        }
        apply(
            vec![ChatEvent::ItemStarted {
                item: stream.clone(),
            }],
            cx,
        );
        assert_layout("multiline long running replacement", cx);
        for now in [59_000, 62_000, 3_662_000] {
            view.update(cx, |view, cx| view.tick_activity(Some(now), cx));
            cx.run_until_parked();
            view.read_with(cx, |view, _| {
                assert_eq!(
                    display::find(&view.visible, "stream").unwrap().duration_ms,
                    Some(now - 2_000),
                    "the timer must actually update the running command before its layout is checked"
                );
            });
            assert_layout(&format!("clock tick {now}"), cx);
        }
        view.update(cx, |view, cx| {
            if mode == Normal {
                view.toggle_projected("activity:live", cx);
            }
            view.toggle_projected("stream", cx);
        });
        cx.run_until_parked();
        expect_open_body.set(true);
        assert_layout("open waiting body", cx);
        for output in [
            "first line\n".to_owned(),
            "second line\nthird line\n".to_owned(),
            "tail line\n".repeat(12),
        ] {
            apply(
                vec![ChatEvent::ItemDelta {
                    item_id: "stream".into(),
                    delta: Delta::Output(output),
                }],
                cx,
            );
            assert_layout("paced output delta in open body", cx);
        }
        stream.status = ItemStatus::Completed;
        stream.ended_at = Some(3_663_000);
        if let ItemBody::Command {
            output, exit_code, ..
        } = &mut stream.body
        {
            *output = (1..=24)
                .map(|line| format!("final replacement line {line}\n"))
                .collect();
            *exit_code = Some(0);
        }
        apply(
            vec![ChatEvent::ItemCompleted {
                item: stream.clone(),
            }],
            cx,
        );
        assert_layout("completed item replaces streamed body", cx);
        if let ItemBody::Command { output, .. } = &mut stream.body {
            *output = "Final concise output\n".into();
        }
        apply(vec![ChatEvent::ItemCompleted { item: stream }], cx);
        assert_layout("completed whole-item correction shrinks open body", cx);
        apply(
            vec![ChatEvent::TurnCompleted {
                turn_id: "live".into(),
                outcome: TurnOutcome::Completed,
                ended_at: Some(3_664_000),
            }],
            cx,
        );
        assert_layout("completed turn summary", cx);
        close(cx, handle);
    }
    let _ = std::fs::remove_dir_all(home);
}

/// Exercise native layout, selection export and token updates together. A
/// virtual top-level group alone does not protect us from 2,200 eager children.
#[gpui::test]
fn large_disclosed_turn_virtualizes_children_and_streams_within_budget(cx: &mut TestAppContext) {
    use crate::chat::{
        model::{ChatEvent, Delta, Item, ItemStatus, Presentation},
        wire::Envelope,
    };
    let home = home();
    start(cx, &home);
    let id = chat_id();
    use gpui_kit::test::TestWindowExt;
    let (handle, _, tabs) = mount(cx, &[&id]);
    let view = &tabs[0];
    let mut bulk_ms = [0.; 3];
    view.update(cx, |view, cx| {
        let events: Vec<_> = (0..2200)
            .map(|n| Envelope {
                chat_id: id.clone(),
                seq: n + 1,
                event: ChatEvent::ItemStarted {
                    item: Item {
                        id: format!("perf-{n}"),
                        turn_id: Some("perf".into()),
                        status: if n == 2199 {
                            ItemStatus::InProgress
                        } else {
                            ItemStatus::Completed
                        },
                        presentation: Presentation::default(),
                        started_at: None,
                        ended_at: None,
                        body: ItemBody::Command {
                            command: "cat src/main.rs".into(),
                            cwd: None,
                            output: "bounded output line\n".repeat(3500),
                            exit_code: Some(0),
                        },
                    },
                },
            })
            .collect();
        let applied = view.model.apply(&events);
        view.sync_list(&applied, cx);
        view.apply_display_mode(Verbose, cx);
        for duration in &mut bulk_ms {
            let toggle = Instant::now();
            view.expand_transcript(true, cx);
            *duration = toggle.elapsed().as_secs_f64() * 1000.;
            view.expand_transcript(false, cx);
        }
        view.expand_transcript(true, cx);
        assert!(view.items_in_list >= 2200);
        view.list.scroll_to(gpui::ListOffset {
            item_ix: 1000,
            offset_in_item: px(0.),
        });
        view.rendered_rows.set(0);
        cx.notify();
    });
    let mut frames_ms = [0.; 3];
    for duration in &mut frames_ms {
        view.update(cx, |view, _| view.rendered_rows.set(0));
        let frame = Instant::now();
        // TestWindowExt refreshes and draws the native frame on every call.
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .unwrap();
        *duration = frame.elapsed().as_secs_f64() * 1000.;
        view.read_with(cx, |view, _| {
            assert!(
                view.rendered_rows.get() > 0 && view.rendered_rows.get() < 100,
                "offscreen children rendered: {}",
                view.rendered_rows.get()
            );
        });
    }
    frames_ms.sort_by(f64::total_cmp);
    let frame_ms = frames_ms[1];
    assert!(
        frame_ms < 25.,
        "median large-fixture frame {frame_ms:.1} ms exceeded 25 ms"
    );
    bulk_ms.sort_by(f64::total_cmp);
    let expand_ms = bulk_ms[1];
    assert!(
        expand_ms < 60.,
        "median Expand all {expand_ms:.1} ms exceeded 60 ms"
    );
    let mut toggles_ms = [0.; 3];
    view.update(cx, |view, cx| {
        let builds = view.transcript_selection.source_builds.get();
        assert!(builds < 100, "offscreen selection bodies built: {builds}");
        for duration in &mut toggles_ms {
            let toggles = Instant::now();
            for _ in 0..4 {
                view.toggle_projected("g:perf:perf-0", cx);
                view.refresh_selection_document(Look::of(cx), cx);
            }
            *duration = toggles.elapsed().as_secs_f64() * 1000. / 4.;
        }
        assert!(
            view.transcript_selection.source_builds.get() - builds <= 12,
            "disclosure rebuilt unchanged historical selection bodies"
        );
    });
    toggles_ms.sort_by(f64::total_cmp);
    let toggle_ms = toggles_ms[1];
    assert!(
        toggle_ms < 60.,
        "median row toggle {toggle_ms:.1} ms exceeded 60 ms"
    );
    let started = Instant::now();
    view.update(cx, |view, cx| {
        let runs = view.projection_runs;
        for seq in 2201..2321 {
            let applied = view.model.apply(&[Envelope {
                chat_id: id.clone(),
                seq,
                event: ChatEvent::ItemDelta {
                    item_id: "perf-2199".into(),
                    delta: Delta::Output("token ".into()),
                },
            }]);
            view.sync_list(&applied, cx);
        }
        assert_eq!(
            view.projection_runs, runs,
            "tokens reprojected historical calls"
        );
    });
    let event_ms = started.elapsed().as_secs_f64() * 1000. / 120.;
    eprintln!(
        "desktop perf: native frame median={frame_ms:.3} ms samples={frames_ms:?}; event={event_ms:.3} ms; row toggle median={toggle_ms:.3} ms; Expand all median={expand_ms:.3} ms"
    );
    assert!(
        event_ms < 1.5,
        "stream event {event_ms:.3} ms exceeded 1.5 ms"
    );
    let _ = std::fs::remove_dir_all(home);
}
