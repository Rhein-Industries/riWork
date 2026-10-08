//! The notice banners as drawn: the stack, "n more", ×, Esc on a focused ×, the history.
use super::editor_tests::mount;
use super::*;
use crate::chat::model::{ChatEvent, Item, ItemStatus};
use gpui::{SharedString, TestAppContext};
use gpui_kit::test::TestWindowExt;

fn push(view: &Entity<ChatView>, cx: &mut TestAppContext, id: &str, body: ItemBody) {
    view.update(cx, |view, cx| {
        view.model.link = state::Link::Live;
        view.model.transcript.apply(&ChatEvent::ItemCompleted {
            item: Item {
                id: id.into(),
                turn_id: Some("turn".into()),
                status: ItemStatus::Completed,
                body,
                presentation: Default::default(),
            },
        });
        cx.notify();
    });
}

fn shown(window: &Window, id: &str) -> bool {
    window
        .try_find(SharedString::from(format!("chat-notice:{id}")))
        .is_some_and(|banner| banner.visible())
}

#[gpui::test]
fn two_banners_show_newest_first_the_rest_fold_and_each_closes(cx: &mut TestAppContext) {
    let (handle, view, _recording) = mount(cx);
    push(&view, cx, "u", ItemBody::UserMessage { text: "hi".into() });
    push(
        &view,
        cx,
        "a",
        ItemBody::notice(NoticeLevel::Info, "Fast mode is off", Some("fast_mode")),
    );
    push(
        &view,
        cx,
        "b",
        ItemBody::notice(
            NoticeLevel::Error,
            "This account is close to the weekly usage limit.",
            Some("rate_limit:seven_day"),
        ),
    );
    push(
        &view,
        cx,
        "c",
        ItemBody::notice(NoticeLevel::Error, "The turn failed", Some("turn_failed")),
    );
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(shown(window, "c") && shown(window, "b"));
        assert!(!shown(window, "a"), "the third folds away");
        let more = window.find("chat-notices-more");
        assert_eq!(more.label(), Some("1 more"));
        assert_eq!(more.expanded(), Some(false));
        // Newest on top.
        assert!(
            window.find("chat-notice:c").bounds().top()
                < window.find("chat-notice:b").bounds().top()
        );
        // Inside the message box's bar, above the box, at its width.
        let field = window.find("chat-composer");
        assert!(window.find("chat-notice:c").bounds().bottom() <= field.bounds().top());
        // The usage limit offers the Usage panel.
        assert!(window.try_find("chat-notice-usage:b").is_some());
        assert!(window.try_find("chat-notice-usage:c").is_none());

        window.click("chat-notices-more", cx);
        window.render_frame(cx);
        assert!(shown(window, "a"));
        assert_eq!(window.find("chat-notices-more").label(), Some("Show fewer"));

        window.click("chat-notice-close:c", cx);
        window.render_frame(cx);
        assert!(!shown(window, "c"));
        assert!(shown(window, "b") && shown(window, "a"));
        assert!(window.try_find("chat-notices-more").is_none());
    })
    .unwrap();

    // Esc presses a × that has the keyboard focus.
    cx.update_window(handle.into(), |_, window, cx| {
        window.click("chat-composer", cx);
        let close = SharedString::from("chat-notice-close:b");
        for _ in 0..12 {
            window.render_frame(cx);
            if window.find(close.clone()).focused() == Some(true) {
                break;
            }
            window.press("shift-tab", cx);
        }
        assert_eq!(
            window.find(close).focused(),
            Some(true),
            "× takes the focus"
        );
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(!shown(window, "b"));
        assert!(shown(window, "a"));
    })
    .unwrap();

    // The history keeps every notice, the dismissed ones too.
    cx.update_window(handle.into(), |_, window, cx| {
        window.click("chat-more", cx);
        window.render_frame(cx);
        assert_eq!(
            window.find("chat-notice-history").role(),
            Some(gpui::Role::MenuItem)
        );
        window.click("chat-notice-history", cx);
        window.render_frame(cx);
        assert!(window.try_find("chat-notice-history-list").is_some());
        assert!(!shown(window, "a"), "the history takes the banners' place");
        window.click("chat-notice-history-close", cx);
        window.render_frame(cx);
        assert!(window.try_find("chat-notice-history-list").is_none());
        assert!(shown(window, "a"));
    })
    .unwrap();
    view.read_with(cx, |view, _| {
        assert_eq!(notices::history(&view.model.transcript).len(), 3);
    });
}

#[gpui::test]
fn a_tab_error_updates_in_place_and_clears_when_the_host_answers(cx: &mut TestAppContext) {
    let (handle, view, _recording) = mount(cx);
    view.update(cx, |view, cx| {
        view.model.link = state::Link::Live;
        view.command_failed(ChatCommand::Interrupt, "socket closed".into());
        view.command_failed(ChatCommand::Interrupt, "connection refused".into());
        cx.notify();
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(shown(window, "local:link"));
        assert!(window.try_find("chat-notices-more").is_none(), "one banner");
    })
    .unwrap();
    view.read_with(cx, |view, _| {
        assert_eq!(
            view.notices.local(notices::LocalKey::Link),
            Some("Could not reach the chat: connection refused")
        );
    });
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |view, cx| {
            view.accept(vec![feed::FeedMsg::Link(state::Link::Live)], window, cx)
        });
    })
    .unwrap();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(!shown(window, "local:link"));
    })
    .unwrap();
}

#[gpui::test]
fn a_limit_banner_goes_by_itself_at_its_reset_time(cx: &mut TestAppContext) {
    let start = 1_800_000_000;
    notices::TEST_NOW.with(|now| now.set(Some(start)));
    let (handle, view, _recording) = mount(cx);
    push(&view, cx, "u", ItemBody::UserMessage { text: "hi".into() });
    let mut body = ItemBody::notice(
        NoticeLevel::Error,
        "This account is close to the weekly usage limit.",
        Some("rate_limit:seven_day"),
    );
    if let ItemBody::Notice { resets_at, .. } = &mut body {
        *resets_at = Some(start + 30);
    }
    push(&view, cx, "limit", body);
    view.update(cx, |view, cx| view.schedule_notice_expiry(cx));
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(shown(window, "limit"));
    })
    .unwrap();
    let redraws = std::rc::Rc::new(std::cell::Cell::new(0));
    let counted = redraws.clone();
    cx.update(|cx| {
        cx.observe(&view, move |_, _| counted.set(counted.get() + 1))
            .detach()
    });
    // Nothing happens in the chat; only the clock moves past the reset.
    notices::TEST_NOW.with(|now| now.set(Some(start + 31)));
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(31));
    cx.run_until_parked();
    assert!(redraws.get() > 0, "the reset time redraws the tab");
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(!shown(window, "limit"));
    })
    .unwrap();
    view.read_with(cx, |view, _| assert!(view.notice_expiry.is_none()));
    notices::TEST_NOW.with(|now| now.set(None));
}

#[gpui::test]
fn many_expanded_banners_scroll_and_leave_the_message_box_in_reach(cx: &mut TestAppContext) {
    let (handle, view, _recording) = mount(cx);
    push(&view, cx, "u", ItemBody::UserMessage { text: "hi".into() });
    for n in 0..12 {
        push(
            &view,
            cx,
            &format!("n{n}"),
            ItemBody::notice(NoticeLevel::Warning, format!("Warning number {n}"), None),
        );
    }
    view.update(cx, |view, cx| {
        view.notices.expanded = true;
        cx.notify();
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let list = window.find("chat-notices-list");
        assert!(
            list.bounds().size.height <= ui_text::space(notice_ui::MAX_STACK) + px(1.),
            "the stack is bounded: {:?}",
            list.bounds()
        );
        let fewer = window.find("chat-notices-more");
        assert!(fewer.visible());
        assert_eq!(fewer.label(), Some("Show fewer"));
        let field = window.find("chat-composer");
        assert!(field.visible());
        assert!(fewer.bounds().bottom() <= field.bounds().top());
        assert!(field.bounds().bottom() <= px(800.));
    })
    .unwrap();
}

#[gpui::test]
fn a_banner_is_one_line_as_tall_as_the_buttons_and_a_long_one_opens(cx: &mut TestAppContext) {
    let long = "The API failed with an overloaded error and Claude is retrying the request; \
                this can take a while when many people are using it at the same time, so the \
                turn keeps running and answers once the API is back.";
    // (level, text, kind, opens by itself)
    let cases = [
        (
            NoticeLevel::Info,
            "Fast mode is off",
            Some("fast_mode"),
            false,
        ),
        (NoticeLevel::Warning, long, Some("provider_warning"), false),
        (
            NoticeLevel::Error,
            "The turn failed",
            Some("turn_failed"),
            false,
        ),
        (
            NoticeLevel::Error,
            "Close to the weekly limit.",
            Some("rate_limit:seven_day"),
            false,
        ),
        (NoticeLevel::Error, long, Some("rate_limit:five_hour"), true),
    ];
    let start = 1_800_000_000;
    notices::TEST_NOW.with(|now| now.set(Some(start)));
    for native in [false, true] {
        let (handle, view, _recording) = mount(cx);
        cx.update(|cx| {
            cx.set_global(crate::theme::Appearance {
                selected: crate::theme::ThemeChoice::RiWork,
                palette: if native {
                    crate::theme::Palette::native(true)
                } else {
                    crate::theme::Palette::RIWORK
                },
                terminal: None,
                ghostty: None,
                error: None,
            })
        });
        push(&view, cx, "u", ItemBody::UserMessage { text: "hi".into() });
        for (n, (level, text, kind, _)) in cases.iter().enumerate() {
            let mut body = ItemBody::notice(*level, *text, *kind);
            if let ItemBody::Notice { resets_at, .. } = &mut body {
                *resets_at = kind
                    .is_some_and(|kind| kind.starts_with("rate_limit:"))
                    .then_some(start + 3600);
            }
            push(&view, cx, &format!("n{n}"), body);
        }
        view.update(cx, |view, cx| {
            view.notices.expanded = true;
            cx.notify();
        });
        let near = |a: gpui::Pixels, b: gpui::Pixels| (a - b).abs() <= px(1.0);
        let find = |window: &Window, what: &str, n: usize| {
            window
                .try_find(SharedString::from(format!("chat-notice{what}:n{n}")))
                .map(|node| node.bounds())
        };
        // Pressing the chevron opens the long warning.
        for pressed in [false, true] {
            cx.update_window(handle.into(), |_, window, cx| {
                if pressed {
                    window.click("chat-notice-more:n1", cx);
                }
                window.render_frame(cx);
                window.render_frame(cx);
                let line = ui_text::text(notice_ui::LINE);
                for (n, (_, text, kind, opens)) in cases.iter().enumerate() {
                    let what = format!("native {native}, pressed {pressed}, {kind:?}");
                    let open = *opens || (pressed && n == 1);
                    let banner = find(window, "", n).unwrap();
                    let body = find(window, "-text", n).unwrap();
                    let close = find(window, "-close", n).unwrap();
                    let chevron = find(window, "-more", n);
                    assert_eq!(chevron.is_some(), *text == long, "{what}: chevron");
                    let mut controls = vec![close];
                    controls.extend(chevron);
                    controls.extend(find(window, "-usage", n));
                    if kind.is_some_and(|kind| kind.starts_with("rate_limit:")) {
                        assert!(find(window, "-usage", n).is_some(), "{what}: Show usage");
                    }
                    for control in &controls {
                        assert!(control.left() >= body.right(), "{what}: after the text");
                        assert!(
                            control.top() >= banner.top() && control.bottom() <= banner.bottom(),
                            "{what}: {control:?} inside {banner:?}"
                        );
                    }
                    if open {
                        // The whole text, the same inset above and below; the buttons beside
                        // its first line.
                        assert!((body.size.height / line).round() >= 2.0, "{what}: wraps");
                        let above = body.top() - banner.top();
                        let below = banner.bottom() - body.bottom();
                        assert!(
                            near(above, below),
                            "{what}: {above:?} above, {below:?} below"
                        );
                        for control in &controls {
                            assert!(near(control.center().y, body.top() + line / 2.0), "{what}");
                        }
                    } else {
                        // One line, as tall as the message box's buttons, all centered.
                        assert!(
                            near(banner.size.height, ui_text::space(notice_ui::ROW)),
                            "{what}: {banner:?}"
                        );
                        assert!(near(body.size.height, line), "{what}: one line {body:?}");
                        for control in controls.iter().chain([&body]) {
                            assert!(near(control.center().y, banner.center().y), "{what}");
                        }
                    }
                    if let Some(chevron) = chevron {
                        let node =
                            window.find(SharedString::from(format!("chat-notice-more:n{n}")));
                        assert_eq!(node.expanded(), Some(open), "{what}: {chevron:?}");
                    }
                }
            })
            .unwrap();
        }
    }
    notices::TEST_NOW.with(|now| now.set(None));
}

#[gpui::test]
fn host_dismissed_notice_has_no_banner_and_close_sends_the_host_command(cx: &mut TestAppContext) {
    let (handle, view, recording) = mount(cx);
    let mut body = ItemBody::notice(
        NoticeLevel::Error,
        "weekly limit",
        Some("rate_limit:seven_day"),
    );
    if let ItemBody::Notice { dismissed, .. } = &mut body {
        *dismissed = true;
    }
    push(&view, cx, "host-dismissed", body);
    push(
        &view,
        cx,
        "auth",
        ItemBody::notice(NoticeLevel::Error, "sign in", Some("auth_required")),
    );
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(!shown(window, "host-dismissed"));
        assert!(shown(window, "auth"));
        window.click("chat-notice-close:auth", cx);
        window.render_frame(cx);
        assert!(!shown(window, "auth"));
    })
    .unwrap();
    assert!(
        matches!(recording.try_recv().unwrap(), feed::Delivery::Command(ChatCommand::DismissNotice { item_id }) if item_id == "auth")
    );
}

#[gpui::test]
fn the_usage_chip_shows_the_most_used_window_past_its_warning_and_opens_usage(
    cx: &mut TestAppContext,
) {
    let start = 1_800_000_000;
    notices::TEST_NOW.with(|now| now.set(Some(start)));
    let (handle, view, _recording) = mount(cx);
    let window = |id: &str, label: &str, used: f64| crate::chat::model::RateWindow {
        id: id.into(),
        label: label.into(),
        used_percent: used,
        resets_at: Some(start + 30),
        warn_at: 70.0,
    };
    let set = |view: &Entity<ChatView>, cx: &mut TestAppContext, windows| {
        view.update(cx, |view, cx| {
            view.model.link = state::Link::Live;
            view.model
                .transcript
                .apply(&ChatEvent::RateLimits { windows });
            view.schedule_notice_expiry(cx);
            cx.notify();
        })
    };
    set(&view, cx, vec![window("five_hour", "5h", 40.0)]);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("chat-usage-chip").is_none(), "under 70%");
    })
    .unwrap();
    set(
        &view,
        cx,
        vec![
            window("five_hour", "5h", 40.0),
            window("seven_day", "weekly", 91.0),
        ],
    );
    let opened = std::rc::Rc::new(std::cell::Cell::new(false));
    let seen = opened.clone();
    cx.update(|cx| {
        cx.subscribe(&view, move |_, event: &ChatViewEvent, _| {
            if matches!(event, ChatViewEvent::ShowUsage) {
                seen.set(true);
            }
        })
        .detach()
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let chip = window.find("chat-usage-chip");
        assert!(chip.visible());
        assert!(
            chip.label()
                .is_some_and(|label| label.starts_with("⚠ weekly 91% · resets")),
            "{:?}",
            chip.label()
        );
        // No banner: nearing a limit is the chip's.
        assert!(window.try_find("chat-notices").is_none());
        window.click("chat-usage-chip", cx);
    })
    .unwrap();
    assert!(opened.get(), "the chip opens the Usage panel");
    // The reset passes with nothing else happening: the chip goes.
    notices::TEST_NOW.with(|now| now.set(Some(start + 31)));
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(31));
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("chat-usage-chip").is_none());
    })
    .unwrap();
    notices::TEST_NOW.with(|now| now.set(None));
}

#[gpui::test]
fn the_usage_chip_and_meter_fit_a_narrow_chat_by_leaving_out_detail(cx: &mut TestAppContext) {
    let start = 1_800_000_000;
    notices::TEST_NOW.with(|now| now.set(Some(start)));
    let mut widths = Vec::new();
    for width in [900.0_f32, 520.0, 360.0, 260.0] {
        let (handle, view, _recording) = super::editor_tests::mount_sized(
            cx,
            HostConfig {
                ensure: std::sync::Arc::new(|| Err("fixture staging is disabled".into())),
            },
            width,
        );
        view.update(cx, |view, cx| {
            view.model.link = state::Link::Live;
            view.model.transcript.apply(&ChatEvent::RateLimits {
                windows: vec![crate::chat::model::RateWindow {
                    id: "seven_day_sonnet".into(),
                    label: "weekly Sonnet".into(),
                    used_percent: 95.0,
                    resets_at: Some(start + 3 * 86_400),
                    warn_at: 70.0,
                }],
            });
            view.model.transcript.apply(&ChatEvent::Usage {
                usage: crate::chat::model::Usage {
                    input_tokens: 150_000,
                    output_tokens: 20_000,
                    cached_input_tokens: 0,
                    context_window: Some(200_000),
                    context_used: Some(172_000),
                    cost_usd: Some(12.345),
                },
            });
            cx.notify();
        });
        cx.update_window(handle.into(), |_, window, cx| {
            // The first frame measures the chat; the next ones fit the group to it.
            for _ in 0..3 {
                window.render_frame(cx);
            }
            let group = window.find("chat-usage-group").bounds();
            let chip = window.find("chat-usage-chip").bounds();
            let meter = window.find("chat-context-meter").bounds();
            let what = format!("{width} px: group {group:?}");
            assert!(
                group.left() >= px(0.) && group.right() <= px(width),
                "{what}"
            );
            for part in [chip, meter] {
                assert!(part.right() <= group.right() + px(0.5), "{what}: {part:?}");
                assert!(part.size.width > px(0.), "{what}: {part:?} shown");
            }
            widths.push(group.size.width);
        })
        .unwrap();
    }
    // Narrower chats leave out more: the group never grows as the chat shrinks.
    assert!(
        widths.windows(2).all(|pair| pair[1] <= pair[0]),
        "{widths:?}"
    );
    assert!(widths[3] < widths[0], "{widths:?}");
    notices::TEST_NOW.with(|now| now.set(None));
}
