//! Real rendered cards with recording events: no shell manager, host or provider. Worker
//! cards are transcript items the host wrote, with live state installed beside them.
use super::*;
use crate::chat::model::{ChatEvent, Item, ItemStatus, Provider};
use crate::chat::workers::{LiveStatus, LiveWorker, WorkerEnd, WorkerFinal, WorkerKind, WorkerRef};
use gpui::{ListOffset, SharedString, TestAppContext, WindowHandle, px};
use gpui_kit::test::TestWindowExt;
use std::collections::BTreeMap;
use std::sync::Mutex;

fn worker(session: &str, follow_up: bool, end: Option<WorkerFinal>) -> WorkerRef {
    WorkerRef {
        session_id: session.into(),
        kind: WorkerKind::Shell,
        title: "Fix polling".into(),
        harness: Some("claude".into()),
        model: None,
        worktree_id: None,
        assignment: Some("Make the poller back off".into()),
        noted_at: crate::activity::unix_now().saturating_sub(90),
        after: None,
        late: false,
        follow_up,
        end,
    }
}

fn marker(id: &str, session: &str, follow_up: bool, end: Option<WorkerFinal>) -> ChatEvent {
    ChatEvent::ItemCompleted {
        item: crate::chat::workers::item(
            id.into(),
            Some("turn".into()),
            worker(session, follow_up, end),
        ),
    }
}

fn done(tail: &str) -> Option<WorkerFinal> {
    Some(WorkerFinal {
        status: WorkerEnd::Done,
        ended_at: crate::activity::unix_now(),
        tail: tail.into(),
        turn: None,
        turn_end: None,
        done_since: None,
    })
}

/// A message from the user: a row in every display mode.
fn message(id: &str, text: &str) -> ChatEvent {
    ChatEvent::ItemCompleted {
        item: Item {
            started_at: None,
            ended_at: None,
            presentation: Default::default(),
            id: id.into(),
            turn_id: Some("turn".into()),
            status: ItemStatus::Completed,
            body: ItemBody::UserMessage { text: text.into() },
        },
    }
}

fn live(status: LiveStatus, output: &str) -> LiveWorker {
    LiveWorker {
        status,
        actionable: true,
        model: None,
        output: output.into(),
        output_error: None,
    }
}

fn only(session: &str, worker: LiveWorker) -> BTreeMap<String, LiveWorker> {
    BTreeMap::from([(session.to_owned(), worker)])
}

/// A live chat the view follows, as after its first events.
fn live_chat(cx: &mut TestAppContext, view: &Entity<ChatView>) {
    view.update(cx, |view, cx| {
        let mut info = testing::info("fixture-chat");
        info.provider = Provider::Claude;
        view.adopt_info_for_tests(info);
        view.model.link = Link::Live;
        cx.notify();
    });
}

/// What the host says next, through the feed as it arrives.
fn arrive(
    cx: &mut TestAppContext,
    handle: WindowHandle<gpui_kit::base::Root>,
    view: &Entity<ChatView>,
    events: Vec<ChatEvent>,
) {
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |view, cx| {
            thread_local!(static SEQ: std::cell::Cell<u64> = const { std::cell::Cell::new(1000) });
            let envelopes = events
                .into_iter()
                .map(|event| crate::chat::wire::Envelope {
                    chat_id: "fixture-chat".into(),
                    seq: SEQ.with(|seq| {
                        seq.set(seq.get() + 1);
                        seq.get()
                    }),
                    event,
                })
                .collect();
            view.accept(vec![feed::FeedMsg::Events(envelopes)], window, cx);
        });
        window.render_frame(cx);
    })
    .unwrap();
}

fn in_transcript(snapshot: &gpui_kit::test::ElementSnapshot) -> bool {
    snapshot
        .path()
        .iter()
        .any(|id| id.to_string().contains("transcript-row:"))
}

#[gpui::test]
fn worker_appears_updates_and_collapses_without_taking_composer_focus(cx: &mut TestAppContext) {
    let (handle, view, _recording) = editor_tests::mount(cx);
    live_chat(cx, &view);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("chat-composer", cx);
        window.input("Keep my draft", cx);
    })
    .unwrap();
    arrive(
        cx,
        handle,
        &view,
        vec![
            ChatEvent::TurnStarted {
                started_at: None,
                turn_id: "turn".into(),
            },
            message("before", "Start a worker."),
            marker("worker:w", "w", false, None),
        ],
    );
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |view, cx| {
            view.set_subshells(
                only("w", live(LiveStatus::Working, "First line\nLast output")),
                None,
                cx,
            )
        });
        window.render_frame(cx);
        let card = window.find("worker:worker:w");
        assert!(card.visible());
        assert!(in_transcript(&card), "the card is a row of the transcript");
        let shelf = window.find("chat-workers");
        assert!(shelf.visible(), "workers stay above the composer");
        assert!(
            !in_transcript(&shelf),
            "the shelf does not scroll with the chat"
        );
        let label = window
            .find("worker-details:worker:w")
            .label()
            .unwrap()
            .to_owned();
        assert!(label.contains("Worker · Fix polling · Working"), "{label}");
        assert!(window.find("worker-output:worker:w").visible());
        let height = card.bounds().size.height;
        view.read_with(cx, |view, cx| {
            assert!(view.composer.read(cx).focus_handle(cx).is_focused(window));
            assert_eq!(view.composer_text(cx), "Keep my draft");
        });
        // New state and more output than fits: same card, same height.
        view.update(cx, |view, cx| {
            view.set_subshells(
                only(
                    "w",
                    live(
                        LiveStatus::Waiting,
                        "a\nb\nc\nd\ne\na much longer line ".repeat(30).as_str(),
                    ),
                ),
                None,
                cx,
            )
        });
        window.render_frame(cx);
        assert!(
            window
                .find("worker-details:worker:w")
                .label()
                .unwrap()
                .contains("Waiting for input")
        );
        assert_eq!(window.find("worker:worker:w").bounds().size.height, height);
    })
    .unwrap();
    // The host records the end: the card collapses to its result in place.
    arrive(
        cx,
        handle,
        &view,
        vec![marker("worker:w", "w", false, done("All tests pass"))],
    );
    cx.update_window(handle.into(), |_, window, cx| {
        assert_eq!(
            window.find("worker-details:worker:w").expanded(),
            Some(false)
        );
        assert!(window.try_find("worker-output:worker:w").is_none());
        assert!(
            window
                .find("worker-details:worker:w")
                .label()
                .unwrap()
                .ends_with("Done")
        );
        view.read_with(cx, |view, cx| {
            assert!(view.composer.read(cx).focus_handle(cx).is_focused(window))
        });
        window.click("worker-details:worker:w", cx);
        window.render_frame(cx);
        assert!(
            window.find("worker-result:worker:w").visible(),
            "finished output remains reviewable"
        );
        let (from, to) = view
            .read(cx)
            .transcript_selection
            .glyph_span_points("worker:worker:w");
        window.drag(from, to, cx);
        window.press("cmd-c", cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|c| c.text()),
            Some("All tests pass".into())
        );
    })
    .unwrap();
}

#[gpui::test]
fn worker_actions_dispatch_once_and_a_refused_message_keeps_its_text(cx: &mut TestAppContext) {
    let (handle, view, _recording) = editor_tests::mount(cx);
    live_chat(cx, &view);
    let events = Arc::new(Mutex::new(Vec::new()));
    let received = events.clone();
    let _subscription = view.update(cx, |_, cx| {
        cx.subscribe(&view, move |_, _, event, cx| {
            if let ChatViewEvent::WorkerShell { shell_id, action } = event {
                received
                    .lock()
                    .unwrap()
                    .push((shell_id.clone(), action.clone()));
            }
            let _ = cx;
        })
    });
    arrive(
        cx,
        handle,
        &view,
        vec![marker("worker:w", "w", false, None)],
    );
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |view, cx| {
            view.set_subshells(only("w", live(LiveStatus::Working, "busy")), None, cx)
        });
        window.render_frame(cx);
        window.click("worker-open:w", cx);
        window.click("worker-message:w", cx);
        window.render_frame(cx);
        // The message goes from a bar above the message box, not from inside the list.
        let bar = window.find("chat-worker-bar");
        assert!(bar.visible());
        assert!(!in_transcript(&bar));
        assert_eq!(
            window.find("chat-worker-bar-label").label(),
            None,
            "a plain label"
        );
        window.input("Review this 🦀", cx);
        window.click("worker-send", cx);
        window.render_frame(cx);
        window.click("worker-send", cx);
        view.update(cx, |view, cx| {
            view.subshell_action_finished(
                "w",
                Err("Harness is waiting for folder trust; input was not sent".into()),
                cx,
            )
        });
        window.render_frame(cx);
        assert!(window.find("chat-workers-error").visible());
        view.read_with(cx, |view, cx| {
            assert_eq!(
                view.workers
                    .message
                    .as_ref()
                    .unwrap()
                    .1
                    .read(cx)
                    .value()
                    .as_ref(),
                "Review this 🦀"
            )
        });
        window.click("worker-cancel-message", cx);
        window.render_frame(cx);
        assert!(window.try_find("chat-worker-bar").is_none());
        window.click("worker-stop:w", cx);
        // A refused stop says why until dismissed.
        view.update(cx, |view, cx| {
            view.subshell_action_finished("w", Err("tmux is not answering".into()), cx)
        });
        window.render_frame(cx);
        assert!(window.find("chat-workers-error").visible());
        window.click("worker-dismiss-error", cx);
        window.render_frame(cx);
        assert!(window.try_find("chat-worker-bar").is_none());
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        events.lock().unwrap().as_slice(),
        [
            ("w".into(), ShellAction::Open),
            ("w".into(), ShellAction::Send("Review this 🦀".into())),
            ("w".into(), ShellAction::Stop),
        ],
        "one explicit send, with no retry after refusal"
    );
}

/// A card of the worker chat `session`.
fn chat_marker(id: &str, session: &str, end: Option<WorkerFinal>) -> ChatEvent {
    ChatEvent::ItemCompleted {
        item: crate::chat::workers::item(
            id.into(),
            Some("turn".into()),
            WorkerRef {
                kind: WorkerKind::Chat,
                ..worker(session, false, end)
            },
        ),
    }
}

#[gpui::test]
fn an_alive_worker_and_a_finished_worker_chat_keep_their_actions_on_the_latest_card(
    cx: &mut TestAppContext,
) {
    let (handle, view, _recording) = editor_tests::mount(cx);
    live_chat(cx, &view);
    arrive(
        cx,
        handle,
        &view,
        vec![
            marker("worker:w", "w", false, done("First result")),
            marker("worker:w:2", "w", true, None),
            chat_marker("worker:c", "c", done("Reviewed")),
            marker("worker:x", "x", false, done("Gone")),
        ],
    );
    cx.update_window(handle.into(), |_, window, cx| {
        let mut gone = live(LiveStatus::Exited, "");
        gone.actionable = false;
        let mut chat = live(LiveStatus::Done, "");
        chat.actionable = false;
        view.update(cx, |view, cx| {
            view.set_subshells(
                BTreeMap::from([
                    ("w".to_owned(), live(LiveStatus::Done, "idle prompt")),
                    ("c".to_owned(), chat),
                    ("x".to_owned(), gone),
                ]),
                None,
                cx,
            )
        });
        window.render_frame(cx);
        // Done, and still alive: it can be given more work, opened or stopped, from its
        // latest card only.
        for id in ["worker-open:w", "worker-message:w", "worker-stop:w"] {
            let button = window.find(id);
            assert!(button.visible(), "{id}");
            assert!(
                button
                    .path()
                    .iter()
                    .any(|part| part.to_string().contains("worker:worker:w:2")),
                "{id}"
            );
        }
        assert!(
            window.find("worker-open:c").visible(),
            "a finished chat opens"
        );
        assert!(
            window.try_find("worker-open:x").is_none(),
            "an exited terminal"
        );
    })
    .unwrap();
}

#[gpui::test]
fn a_message_edited_while_it_went_and_one_left_by_a_stop_are_kept(cx: &mut TestAppContext) {
    let (handle, view, _recording) = editor_tests::mount(cx);
    live_chat(cx, &view);
    arrive(
        cx,
        handle,
        &view,
        vec![marker("worker:w", "w", false, None)],
    );
    let draft = |view: &Entity<ChatView>, cx: &mut gpui::Window, app: &mut gpui::App| {
        let _ = cx;
        view.read(app)
            .workers
            .message
            .as_ref()
            .map(|(_, input, _)| input.read(app).value().to_string())
    };
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |view, cx| {
            view.set_subshells(only("w", live(LiveStatus::Working, "busy")), None, cx)
        });
        window.render_frame(cx);
        window.click("worker-message:w", cx);
        window.render_frame(cx);
        window.input("First part", cx);
        window.click("worker-send", cx);
        window.click("worker-message-input", cx);
        window.press("end", cx);
        window.input(", then more", cx);
        view.update(cx, |view, cx| {
            view.subshell_action_finished("w", Ok(()), cx)
        });
        window.render_frame(cx);
        assert_eq!(
            draft(&view, window, cx).as_deref(),
            Some("First part, then more"),
            "what was typed after the send is still there"
        );
        window.click("worker-stop:w", cx);
        view.update(cx, |view, cx| {
            view.subshell_action_finished("w", Ok(()), cx)
        });
        window.render_frame(cx);
        assert_eq!(
            draft(&view, window, cx).as_deref(),
            Some("First part, then more"),
            "a stop leaves an unsent message"
        );
        // A message that went as it reads closes the bar.
        window.click("worker-send", cx);
        view.update(cx, |view, cx| {
            view.subshell_action_finished("w", Ok(()), cx)
        });
        window.render_frame(cx);
        assert!(window.try_find("chat-worker-bar").is_none());
    })
    .unwrap();
}

#[gpui::test]
fn an_open_that_fails_says_why(cx: &mut TestAppContext) {
    let (handle, view, _recording) = editor_tests::mount(cx);
    live_chat(cx, &view);
    arrive(
        cx,
        handle,
        &view,
        vec![marker("worker:w", "w", false, None)],
    );
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |view, cx| {
            view.set_subshells(only("w", live(LiveStatus::Working, "busy")), None, cx)
        });
        window.render_frame(cx);
        window.click("worker-open:w", cx);
        view.update(cx, |view, cx| {
            view.subshell_action_finished(
                "w",
                Err("This worker is no longer available in this chat's project.".into()),
                cx,
            )
        });
        window.render_frame(cx);
        assert!(window.find("chat-workers-error").visible());
    })
    .unwrap();
}

#[gpui::test]
fn a_running_card_that_arrives_below_a_reader_is_counted_before_it_is_drawn(
    cx: &mut TestAppContext,
) {
    let (handle, view, _recording) = editor_tests::mount(cx);
    live_chat(cx, &view);
    let events = (0..40)
        .map(|n| message(&format!("m{n}"), &format!("Paragraph {n}\n\nmore")))
        .collect();
    arrive(cx, handle, &view, events);
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |view, cx| {
            view.list.pause_following_tail();
            view.list.scroll_to(ListOffset {
                item_ix: 2,
                offset_in_item: px(0.),
            });
            cx.notify();
        });
        window.render_frame(cx);
    })
    .unwrap();
    arrive(
        cx,
        handle,
        &view,
        vec![marker("worker:w", "w", false, None)],
    );
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |view, cx| {
            view.set_subshells(only("w", live(LiveStatus::Working, "busy")), None, cx)
        });
        window.render_frame(cx);
        assert_eq!(
            window.find("chat-workers-running").label(),
            Some("1 worker running ↓")
        );
    })
    .unwrap();
}

#[gpui::test]
fn the_same_inventory_again_still_moves_running_cards_on(cx: &mut TestAppContext) {
    let (handle, view, _recording) = editor_tests::mount(cx);
    live_chat(cx, &view);
    let now = crate::activity::unix_now();
    let starting = ChatEvent::ItemCompleted {
        item: crate::chat::workers::item(
            "worker:s".into(),
            Some("turn".into()),
            WorkerRef {
                noted_at: now - (crate::chat::workers::STARTING_GRACE - 1),
                ..worker("s", false, None)
            },
        ),
    };
    arrive(cx, handle, &view, vec![starting]);
    let notified = std::rc::Rc::new(std::cell::Cell::new(0));
    let count = notified.clone();
    let _observe = cx.update(|cx| cx.observe(&view, move |_, _| count.set(count.get() + 1)));
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |view, cx| view.set_subshells(BTreeMap::new(), None, cx));
        window.render_frame(cx);
        assert!(
            window
                .find("worker-details:worker:s")
                .label()
                .unwrap()
                .ends_with("Starting")
        );
    })
    .unwrap();
    // Nothing new from the inventory, but its time is up: it reads as ended.
    while crate::activity::unix_now() == now {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    notified.set(0);
    cx.update(|cx| view.update(cx, |view, cx| view.set_subshells(BTreeMap::new(), None, cx)));
    assert!(notified.get() > 0, "the cards are drawn again");
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window
                .find("worker-details:worker:s")
                .label()
                .unwrap()
                .ends_with("Ended · details unavailable")
        );
        assert!(window.try_find("worker-output:worker:s").is_none());
    })
    .unwrap();
}

#[gpui::test]
fn a_card_arriving_or_updating_keeps_a_selection_elsewhere(cx: &mut TestAppContext) {
    let (handle, view, _recording) = editor_tests::mount_selection(cx);
    live_chat(cx, &view);
    arrive(cx, handle, &view, vec![message("m", "Keep this selected")]);
    cx.update_window(handle.into(), |_, window, cx| {
        let (from, to) = view
            .read(cx)
            .transcript_selection
            .glyph_span_points("user:m/0");
        window.drag(from, to, cx);
    })
    .unwrap();
    arrive(
        cx,
        handle,
        &view,
        vec![marker("worker:w", "w", false, None)],
    );
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |view, cx| {
            view.set_subshells(only("w", live(LiveStatus::Working, "one")), None, cx)
        });
        window.render_frame(cx);
        view.update(cx, |view, cx| {
            view.set_subshells(only("w", live(LiveStatus::Working, "one\ntwo")), None, cx)
        });
        window.render_frame(cx);
        cx.write_to_clipboard(gpui::ClipboardItem::new_string("unchanged".into()));
        window.press("cmd-c", cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|c| c.text()),
            Some("Keep this selected".into())
        );
    })
    .unwrap();
}

#[gpui::test]
fn the_running_indicator_jumps_to_a_card_out_of_view_and_only_the_latest_card_is_live(
    cx: &mut TestAppContext,
) {
    let (handle, view, _recording) = editor_tests::mount(cx);
    live_chat(cx, &view);
    // These geometry checks retain transcript order; Normal's turn layout
    // intentionally places the worker after its same-turn user rows.
    view.update(cx, |view, cx| {
        view.apply_display_mode(DisplayMode::Verbose, cx)
    });
    let mut events = vec![marker("worker:w", "w", false, None)];
    events.extend((0..40).map(|n| message(&format!("m{n}"), &format!("Paragraph {n}\n\nmore"))));
    arrive(cx, handle, &view, events);
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |view, cx| {
            view.set_subshells(only("w", live(LiveStatus::Working, "busy")), None, cx)
        });
        window.render_frame(cx);
        window.render_frame(cx);
        let chip = window.find("chat-workers-running");
        assert_eq!(chip.label(), Some("1 worker running ↑"));
        window.click("chat-workers-running", cx);
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(window.find("worker:worker:w").visible());
        assert!(window.try_find("chat-workers-running").is_none());
    })
    .unwrap();
    // A follow-up below: the first card says so and is no longer live or counted.
    arrive(
        cx,
        handle,
        &view,
        (40..80)
            .map(|n| message(&format!("m{n}"), &format!("Paragraph {n}\n\nmore")))
            .chain([marker("worker:w:2", "w", true, None)])
            .collect(),
    );
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |view, _| {
            view.list.set_follow_mode(gpui::FollowMode::Tail)
        });
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(
            window.try_find("chat-workers-running").is_none(),
            "the live card is in view"
        );
        let later = window
            .find("worker-details:worker:w:2")
            .label()
            .unwrap()
            .to_owned();
        assert!(
            later.starts_with("Follow-up · Worker · Fix polling"),
            "{later}"
        );
        view.update(cx, |view, cx| {
            view.list.scroll_to(ListOffset {
                item_ix: 0,
                offset_in_item: px(0.),
            });
            cx.notify();
        });
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(
            window
                .find("worker-details:worker:w")
                .label()
                .unwrap()
                .ends_with("Continued below")
        );
        assert!(window.try_find("worker-output:worker:w").is_none());
        assert_eq!(
            window.find("chat-workers-running").label(),
            Some("1 worker running ↓")
        );
        // New output marks the card for measuring again; it still counts below.
        view.update(cx, |view, cx| {
            view.set_subshells(only("w", live(LiveStatus::Working, "more")), None, cx)
        });
        window.render_frame(cx);
        assert_eq!(
            window.find("chat-workers-running").label(),
            Some("1 worker running ↓")
        );
    })
    .unwrap();
}

#[gpui::test]
fn a_card_that_ends_below_keeps_a_reader_where_they_are(cx: &mut TestAppContext) {
    let (handle, view, _recording) = editor_tests::mount(cx);
    live_chat(cx, &view);
    let mut events: Vec<_> = (0..30)
        .map(|n| message(&format!("m{n}"), &format!("Paragraph {n}\n\nmore")))
        .collect();
    events.push(marker("worker:w", "w", false, None));
    events.extend((30..40).map(|n| message(&format!("m{n}"), &format!("Paragraph {n}"))));
    arrive(cx, handle, &view, events);
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |view, cx| {
            view.set_subshells(only("w", live(LiveStatus::Working, "busy")), None, cx);
            view.list.scroll_to(ListOffset {
                item_ix: 3,
                offset_in_item: px(5.),
            });
            cx.notify();
        });
        window.render_frame(cx);
    })
    .unwrap();
    let at = |view: &ChatView| {
        let top = view.list.logical_scroll_top();
        (top.item_ix, top.offset_in_item)
    };
    let reading = view.read_with(cx, |view, _| at(view));
    arrive(
        cx,
        handle,
        &view,
        vec![marker("worker:w", "w", false, done("ok"))],
    );
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |view, cx| view.set_subshells(BTreeMap::new(), None, cx));
        window.render_frame(cx);
    })
    .unwrap();
    view.read_with(cx, |view, _| {
        assert_eq!(at(view), reading);
        assert!(!view.list.is_following_tail());
    });
}

#[gpui::test]
fn a_card_at_the_top_of_the_view_that_ends_keeps_the_view_there(cx: &mut TestAppContext) {
    let (handle, view, _recording) = editor_tests::mount(cx);
    live_chat(cx, &view);
    // These geometry checks retain transcript order; Normal's turn layout
    // intentionally places the worker after its same-turn user rows.
    view.update(cx, |view, cx| {
        view.apply_display_mode(DisplayMode::Verbose, cx)
    });
    let mut events: Vec<_> = (0..10)
        .map(|n| message(&format!("m{n}"), &format!("Paragraph {n}\n\nmore")))
        .collect();
    events.push(marker("worker:w", "w", false, None));
    events.extend((10..40).map(|n| message(&format!("m{n}"), &format!("Paragraph {n}"))));
    arrive(cx, handle, &view, events);
    let card_row = view.read_with(cx, |view, _| {
        view.list_rows
            .iter()
            .position(|row| row.row.id == "worker:w")
            .unwrap()
    });
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |view, cx| {
            view.set_subshells(only("w", live(LiveStatus::Working, "busy")), None, cx);
            view.list.scroll_to(ListOffset {
                item_ix: card_row,
                offset_in_item: px(0.),
            });
            cx.notify();
        });
        window.render_frame(cx);
    })
    .unwrap();
    arrive(
        cx,
        handle,
        &view,
        vec![marker("worker:w", "w", false, done("ok"))],
    );
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("worker:worker:w").visible());
    })
    .unwrap();
    view.read_with(cx, |view, _| {
        let top = view.list.logical_scroll_top();
        assert_eq!((top.item_ix, top.offset_in_item), (card_row, px(0.)));
    });
}

#[gpui::test]
fn markers_are_rows_in_every_display_mode_and_live_state_without_a_card_draws_nothing(
    cx: &mut TestAppContext,
) {
    let (handle, view, _recording) = editor_tests::mount(cx);
    live_chat(cx, &view);
    arrive(
        cx,
        handle,
        &view,
        vec![marker("worker:w", "w", false, done("x"))],
    );
    view.read_with(cx, |view, _| {
        for mode in [DisplayMode::Verbose, DisplayMode::default()] {
            let rows = display::rows(&view.model.transcript, mode, &Default::default(), None);
            assert!(display::find(&rows, "worker:w").is_some(), "{mode:?}");
        }
    });
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |view, cx| {
            view.set_subshells(
                only("someone-else", live(LiveStatus::Working, "x")),
                None,
                cx,
            )
        });
        window.render_frame(cx);
        assert!(window.try_find("worker:worker:someone-else").is_none());
        assert!(window.try_find("worker-output:worker:w").is_none());
    })
    .unwrap();
}

#[gpui::test]
fn provider_delegation_has_a_structured_header_and_expandable_result(cx: &mut TestAppContext) {
    let (handle, view, _recording) = editor_tests::mount(cx);
    view.update(cx,|view,cx| {
        let mut info = testing::info("fixture-chat"); info.provider = Provider::Claude;
        view.adopt_info_for_tests(info);
        view.model.link = Link::Live;
        view.model.transcript.apply(&ChatEvent::ItemCompleted {item:Item { started_at: None, ended_at: None,id:"agent".into(),turn_id:Some("turn".into()),status:ItemStatus::Completed,
            body:ItemBody::ToolCall {server:None,tool:"Agent".into(),input:serde_json::json!({"subagent_type":"Explore","description":"Find the handler"}),output:Some("Found in router.rs".into())},presentation:Default::default() }});
        view.refresh_projection(cx);
        cx.notify();
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("card:agent").label(),
            Some("Subagent · Explore")
        );
        window.click("card:agent", cx);
        window.render_frame(cx);
        assert!(
            window
                .try_find(SharedString::from("subagent-result:agent"))
                .is_some()
        );
        let (from, to) = view
            .read(cx)
            .transcript_selection
            .glyph_span_points("subagent:agent");
        window.drag(from, to, cx);
        window.press("cmd-c", cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|c| c.text()),
            Some("Found in router.rs".into())
        );
    })
    .unwrap();
}
