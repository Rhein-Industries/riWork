//! A chat's Normal/Verbose choice across a tab switch, closing and reopening the tab, and a
//! restart. Tabs use the deferred constructor with an inert transport, as a restored
//! window or project switch does, and the choice is saved in a settings file
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
                                ChatView::open_inert_for_tests(
                                    (*id).to_owned(),
                                    HostConfig {
                                        ensure: Arc::new(|| {
                                            panic!("display fixture must never start a host")
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

/// A long text-and-tools chat, the shape a day of agent work leaves: hundreds of
/// completed turns, each with commands, an edit and a Markdown answer, then one
/// live turn that streams. Timings print; run with `--ignored --nocapture`.
#[gpui::test]
#[ignore]
fn long_chat_streaming_costs(cx: &mut TestAppContext) {
    use crate::chat::{
        model::{
            ChangeKind, ChatEvent, Delta, FileChange, Item, ItemStatus, Presentation, TurnOutcome,
        },
        wire::Envelope,
    };
    use gpui_kit::test::TestWindowExt;
    const TURNS: usize = 400;
    // `LONG_CHAT_REPEAT=10` lengthens every phase for a sampling profiler.
    let repeat: usize = std::env::var("LONG_CHAT_REPEAT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1);
    let home = home();
    start(cx, &home);
    let id = chat_id();
    let (handle, _, tabs) = mount(cx, &[&id]);
    let view = tabs[0].clone();
    let seq = std::cell::Cell::new(0u64);
    let envelope = |event| {
        seq.set(seq.get() + 1);
        Envelope {
            chat_id: id.clone(),
            seq: seq.get(),
            event,
        }
    };
    let item = |id: String, turn: &str, status, body| Item {
        id,
        turn_id: Some(turn.into()),
        status,
        presentation: Presentation::default(),
        started_at: None,
        ended_at: None,
        body,
    };
    let answer = |n: usize| {
        format!(
            "## Result {n}\n\nI changed `src/lib.rs` and [the parser](src/parse.rs:{n}) so the \
             fold keeps its order. See https://example.com/issue/{n} for context.\n\n\
             - first point about **turn {n}** with `code`\n- second point\n  - nested detail\n\n\
             ```rust\nfn turn_{n}() -> usize {{\n    let x = {n};\n    x * 2\n}}\n```\n\n\
             | file | lines |\n|---|---|\n| src/lib.rs | {n} |\n| src/parse.rs | 12 |\n\n\
             That should be all for this step; the tests pass.\n"
        )
    };
    let mut events = Vec::new();
    for turn in 0..TURNS {
        let t = format!("turn-{turn}");
        events.push(envelope(ChatEvent::TurnStarted {
            turn_id: t.clone(),
            started_at: None,
        }));
        let mut push = |item: Item| {
            events.push(envelope(ChatEvent::ItemStarted { item: item.clone() }));
            events.push(envelope(ChatEvent::ItemCompleted { item }));
        };
        push(item(
            format!("u-{turn}"),
            &t,
            ItemStatus::Completed,
            ItemBody::UserMessage {
                text: format!("Please fix issue {turn} in the parser."),
            },
        ));
        for c in 0..5 {
            push(item(
                format!("c-{turn}-{c}"),
                &t,
                ItemStatus::Completed,
                ItemBody::Command {
                    command: format!("rg -n pattern_{c} src"),
                    cwd: None,
                    output: "src/lib.rs:12: let pattern = 1;\n".repeat(40),
                    exit_code: Some(0),
                },
            ));
        }
        push(item(
            format!("f-{turn}"),
            &t,
            ItemStatus::Completed,
            ItemBody::FileChange {
                changes: vec![FileChange {
                    path: "src/lib.rs".into(),
                    kind: ChangeKind::Modify,
                    diff: Some("@@ -1,3 +1,3 @@\n-old\n+new\n context\n".into()),
                }],
            },
        ));
        push(item(
            format!("a-{turn}"),
            &t,
            ItemStatus::Completed,
            ItemBody::AgentMessage { text: answer(turn) },
        ));
        events.push(envelope(ChatEvent::TurnCompleted {
            turn_id: t,
            outcome: TurnOutcome::Completed,
            ended_at: None,
        }));
    }
    let live = "turn-live".to_owned();
    events.push(envelope(ChatEvent::TurnStarted {
        turn_id: live.clone(),
        started_at: None,
    }));
    events.push(envelope(ChatEvent::ItemStarted {
        item: item(
            "u-live".into(),
            &live,
            ItemStatus::Completed,
            ItemBody::UserMessage {
                text: "One more thing.".into(),
            },
        ),
    }));
    // Only the fold and list bookkeeping: the test window draws once the update
    // returns, which `frame` measures separately.
    let accept = |cx: &mut TestAppContext, events: Vec<Envelope>| {
        cx.update_window(handle, |_, window, cx| {
            view.update(cx, |view, cx| {
                let started = Instant::now();
                view.accept(vec![feed::FeedMsg::Events(events)], window, cx);
                started.elapsed().as_secs_f64() * 1000.
            })
        })
        .unwrap()
    };
    let frame = |cx: &mut TestAppContext| {
        let started = Instant::now();
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .unwrap();
        started.elapsed().as_secs_f64() * 1000.
    };
    let median = |mut samples: Vec<f64>| {
        samples.sort_by(f64::total_cmp);
        samples[samples.len() / 2]
    };
    let load_ms = accept(cx, events);
    frame(cx);
    // A tool call starting and completing in the live turn: projection work.
    let mut item_ms = Vec::new();
    let mut item_frame_ms = Vec::new();
    for c in 0..30 * repeat {
        let call = item(
            format!("c-live-{c}"),
            &live,
            ItemStatus::InProgress,
            ItemBody::Command {
                command: "cargo test".into(),
                cwd: None,
                output: String::new(),
                exit_code: None,
            },
        );
        let mut done = call.clone();
        done.status = ItemStatus::Completed;
        item_ms.push(accept(
            cx,
            vec![envelope(ChatEvent::ItemStarted { item: call })],
        ));
        item_frame_ms.push(frame(cx));
        item_ms.push(accept(
            cx,
            vec![envelope(ChatEvent::ItemCompleted { item: done })],
        ));
        item_frame_ms.push(frame(cx));
    }
    // Text streaming into the live answer.
    let mut delta_ms = Vec::new();
    let mut delta_frame_ms = Vec::new();
    for n in 0..200 * repeat {
        let event = if n == 0 {
            ChatEvent::ItemStarted {
                item: item(
                    "a-live".into(),
                    &live,
                    ItemStatus::InProgress,
                    ItemBody::AgentMessage {
                        text: String::new(),
                    },
                ),
            }
        } else {
            ChatEvent::ItemDelta {
                item_id: "a-live".into(),
                delta: Delta::Text(if n % 9 == 0 {
                    "\n\n- a list entry with `code`\n".into()
                } else {
                    format!("word{n} and some more text ")
                }),
            }
        };
        delta_ms.push(accept(cx, vec![envelope(event)]));
        delta_frame_ms.push(frame(cx));
    }
    // Idle frames scrolled into history, where completed answers are drawn.
    view.update(cx, |view, cx| {
        view.list.scroll_to(gpui::ListOffset {
            item_ix: view.items_in_list / 2,
            offset_in_item: px(0.),
        });
        cx.notify();
    });
    let mut idle_frame_ms = Vec::new();
    for _ in 0..30 * repeat {
        view.update(cx, |_, cx| cx.notify());
        idle_frame_ms.push(frame(cx));
    }
    let completed_ms = accept(
        cx,
        vec![envelope(ChatEvent::TurnCompleted {
            turn_id: live.clone(),
            outcome: TurnOutcome::Completed,
            ended_at: None,
        })],
    );
    let rows = view.read_with(cx, |view, _| view.items_in_list);
    eprintln!(
        "long chat ({TURNS} turns, {rows} list rows): load={load_ms:.1} ms; \
         item event median={:.3} ms; item frame median={:.3} ms; \
         text delta median={:.3} ms; delta frame median={:.3} ms; \
         idle history frame median={:.3} ms; turn completed={completed_ms:.3} ms",
        median(item_ms),
        median(item_frame_ms),
        median(delta_ms),
        median(delta_frame_ms),
        median(idle_frame_ms),
    );
    let _ = std::fs::remove_dir_all(home);
}
