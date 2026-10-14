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
        assert_eq!(
            notices::history(&view.model.transcript, usize::MAX).len(),
            3
        );
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
fn a_refused_contributor_change_says_the_hosts_reason_among_the_settings(cx: &mut TestAppContext) {
    let (_handle, view, _recording) = mount(cx);
    let contributor = |mode| ChatCommand::Configure {
        model: None,
        effort: None,
        approval_mode: None,
        fast: None,
        contributor_mode: Some(mode),
        contributor_opt_out: None,
        claude_billing: None,
    };
    let busy = "Wait for the turn to finish, or interrupt it, before changing contributor mode.";
    view.update(cx, |view, _| {
        view.command_failed(contributor(ContributorMode::On), busy.into())
    });
    view.read_with(cx, |view, _| {
        assert_eq!(view.notices.local(notices::LocalKey::Settings), Some(busy));
        assert_eq!(view.notices.local(notices::LocalKey::Link), None);
    });
    view.update(cx, |view, _| {
        view.command_failed(
            contributor(ContributorMode::Off),
            "this chat cannot be configured".into(),
        )
    });
    view.read_with(cx, |view, _| {
        assert_eq!(
            view.notices.local(notices::LocalKey::Settings),
            Some("Could not change contributor mode: this chat cannot be configured")
        );
    });
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
fn a_window_near_its_limit_is_no_chip_and_the_meter_opens_usage(cx: &mut TestAppContext) {
    let start = 1_800_000_000;
    notices::TEST_NOW.with(|now| now.set(Some(start)));
    let (handle, view, _recording) = mount(cx);
    view.update(cx, |view, cx| {
        view.model.link = state::Link::Live;
        view.model.transcript.apply(&ChatEvent::RateLimits {
            windows: vec![crate::chat::model::RateWindow {
                id: "seven_day".into(),
                label: "weekly".into(),
                used_percent: 91.0,
                resets_at: Some(start + 30),
                warn_at: 70.0,
            }],
        });
        view.model.transcript.apply(&ChatEvent::Usage {
            usage: crate::chat::model::Usage {
                input_tokens: 600_000,
                output_tokens: 20_000,
                cached_input_tokens: 0,
                context_window: Some(1_000_000),
                context_used: Some(563_000),
                cost_usd: Some(344.31),
            },
        });
        cx.notify();
    });
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
        assert!(window.try_find("chat-usage-chip").is_none(), "no chip");
        // No banner either: nearing a limit is the Usage panel's.
        assert!(window.try_find("chat-notices").is_none());
        let meter = window.find("chat-context-meter");
        assert_eq!(meter.role(), Some(gpui::Role::Button));
        assert_eq!(meter.label(), Some("Context 56% used, open Usage"));
        window.click("chat-context-meter", cx);
    })
    .unwrap();
    assert!(opened.get(), "the meter opens the Usage panel");
    notices::TEST_NOW.with(|now| now.set(None));
}

#[gpui::test]
fn the_meter_fits_a_narrow_chat_by_leaving_out_its_cost(cx: &mut TestAppContext) {
    let start = 1_800_000_000;
    notices::TEST_NOW.with(|now| now.set(Some(start)));
    let mut widths = Vec::new();
    // The usage has its own row in a narrow chat, beside no session id here.
    for width in [900.0_f32, 360.0, 170.0, 120.0] {
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
            assert!(window.try_find("chat-usage-chip").is_none());
            let meter = window.find("chat-context-meter").bounds();
            let what = format!("{width} px: group {group:?}");
            assert!(
                group.left() >= px(0.) && group.right() <= px(width),
                "{what}"
            );
            for part in [meter] {
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

/// The header at `width`: what row 1 shows, what ⋯ holds, and the pieces' bounds.
struct Header {
    shown: Vec<&'static str>,
    folded: Vec<&'static str>,
    first: gpui::Bounds<gpui::Pixels>,
    more: gpui::Bounds<gpui::Pixels>,
    usage: gpui::Bounds<gpui::Pixels>,
    thread: gpui::Bounds<gpui::Pixels>,
    billing: Option<gpui::Bounds<gpui::Pixels>>,
    controls: Vec<gpui::Bounds<gpui::Pixels>>,
}

fn header_at(cx: &mut TestAppContext, width: f32) -> Header {
    header_with(cx, width, crate::theme::ThemeChoice::RiWork, false)
}

fn header_with(
    cx: &mut TestAppContext,
    width: f32,
    theme: crate::theme::ThemeChoice,
    billing: bool,
) -> Header {
    let (handle, view, _recording) = super::editor_tests::mount_sized(
        cx,
        HostConfig {
            ensure: std::sync::Arc::new(|| Err("fixture staging is disabled".into())),
        },
        width,
    );
    cx.update(|cx| {
        cx.global_mut::<crate::theme::Appearance>().selected = theme;
    });
    view.update(cx, |view, cx| {
        view.model.link = state::Link::Live;
        let mut info = super::testing::info("chat");
        info.provider_thread_id = Some("cc2091da-0000-4000-8000-feedfacecafe".into());
        info.model = Some("sol".into());
        view.model.transcript.apply(&ChatEvent::Info { info });
        view.model.transcript.apply(&ChatEvent::Models {
            models: vec![
                serde_json::from_value(serde_json::json!({
                    "id": "sol", "name": "gpt-5.5", "efforts": ["low", "high"],
                    "default_effort": "high", "supports_fast": true
                }))
                .unwrap(),
            ],
        });
        view.model.transcript.apply(&ChatEvent::Usage {
            usage: crate::chat::model::Usage {
                input_tokens: 150_000,
                output_tokens: 20_000,
                cached_input_tokens: 0,
                context_window: Some(200_000),
                context_used: Some(72_000),
                cost_usd: Some(1.25),
            },
        });
        if billing {
            view.model.transcript.apply(&ChatEvent::Account {
                account: crate::chat::model::Account {
                    api_key_source: Some("ANTHROPIC_API_KEY".into()),
                    plan: None,
                },
            });
        }
        cx.notify();
    });
    let controls = [("chat-mode", "more-mode"), ("chat-compact", "more-compact")];
    let header = cx
        .update_window(handle.into(), |_, window, cx| {
            for _ in 0..4 {
                window.render_frame(cx);
            }
            let shown = controls
                .iter()
                .filter(|(id, _)| window.try_find(*id).is_some())
                .map(|(id, _)| *id)
                .collect::<Vec<_>>();
            Header {
                shown,
                folded: Vec::new(),
                first: window.find("chat-display-normal").bounds(),
                more: window.find("chat-more").bounds(),
                usage: window.find("chat-usage-group").bounds(),
                thread: window.find("chat-thread").bounds(),
                billing: window.try_find("chat-api-key").map(|badge| badge.bounds()),
                controls: controls
                    .iter()
                    .filter_map(|(id, _)| window.try_find(*id).map(|control| control.bounds()))
                    .collect(),
            }
        })
        .unwrap();
    // The ones row 1 has no room for are in ⋯, with their values.
    cx.update_window(handle.into(), |_, window, cx| {
        window.click("chat-more", cx);
        window.render_frame(cx);
    })
    .unwrap();
    let folded = cx
        .update_window(handle.into(), |_, window, _| {
            // The menu stays inside the chat, however narrow.
            let menu = window.find("chat-choices-popover").bounds();
            assert!(
                menu.left() >= px(0.) && menu.right() <= px(width),
                "{width}: {menu:?}"
            );
            controls
                .iter()
                .filter(|(_, row)| window.try_find(*row).is_some())
                .map(|(_, row)| *row)
                .collect::<Vec<_>>()
        })
        .unwrap();
    // A folded picker opens its menu from ⋯, and pressing ⋯ again closes it.
    if folded.contains(&"more-mode") {
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("more-mode", cx);
            window.render_frame(cx);
        })
        .unwrap();
        view.read_with(cx, |view, _| assert_eq!(view.menu, Some(Menu::Mode)));
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("chat-more", cx);
            window.render_frame(cx);
        })
        .unwrap();
        view.read_with(cx, |view, _| assert_eq!(view.menu, None));
    }
    Header { folded, ..header }
}

#[gpui::test]
fn the_shared_header_keeps_controls_and_more_on_row_one_and_wraps_usage_below(
    cx: &mut TestAppContext,
) {
    let middle = |b: gpui::Bounds<gpui::Pixels>| b.top() + b.size.height / 2.;
    let order = ["mode", "compact"];
    let mut folds = Vec::new();
    for width in [900.0_f32, 760.0, 520.0, 330.0, 230.0] {
        let header = header_at(cx, width);
        let Header {
            first,
            more,
            usage,
            thread,
            ..
        } = header;
        let what = format!(
            "{width} px: shown {:?}, folded {:?}, more {more:?}, usage {usage:?}",
            header.shown, header.folded
        );
        // Each control is either on row 1 or in ⋯, and the folded ones are the row's end:
        // Compact first, then Mode. The model controls live in the composer.
        assert_eq!(
            header.shown.len() + header.folded.len(),
            order.len(),
            "{what}"
        );
        let kept = header.shown.len();
        assert!(
            header
                .shown
                .iter()
                .zip(order)
                .all(|(id, name)| id.ends_with(name)),
            "{what}"
        );
        assert!(
            header
                .folded
                .iter()
                .zip(&order[kept..])
                .all(|(id, name)| id.ends_with(name)),
            "{what}"
        );
        for control in &header.controls {
            assert!(
                (middle(*control) - middle(first)).abs() <= px(1.),
                "{what}: control left row one"
            );
            assert!(
                control.right() + ui_text::space(composer::BAR_GAP) <= more.left() + px(1.),
                "{what}: control overlaps More"
            );
        }
        // ⋯ ends row 1, level with its first control, at the trailing inset.
        assert!((middle(more) - middle(first)).abs() <= px(1.), "{what}");
        assert!(
            (more.right() - (px(width) - ui_text::space(composer::BAR_INSET))).abs() <= px(1.),
            "{what}"
        );
        // At most two rows: the usage shares row 1 or is the one row below it, the session
        // id flush under ⋯.
        let two_rows = usage.top() > first.bottom();
        if two_rows {
            let gap = ui_text::space(composer::BAR_GAP);
            assert!(
                (thread.top() - more.bottom() - gap).abs() <= px(1.),
                "{what}: row two follows row one directly"
            );
            assert!((middle(usage) - middle(thread)).abs() <= px(1.), "{what}");
            assert!((usage.left() - first.left()).abs() <= px(1.), "{what}");
            assert!((thread.right() - more.right()).abs() <= px(1.), "{what}");
        } else {
            // Right-aligned before ⋯: … Compact, the usage, the session id, ⋯.
            assert!(header.folded.is_empty(), "{what}");
            assert!((middle(usage) - middle(first)).abs() <= px(1.), "{what}");
            assert!((middle(thread) - middle(first)).abs() <= px(1.), "{what}");
            let gap = ui_text::space(composer::BAR_GAP) + px(1.);
            assert!(usage.right() <= thread.left(), "{what}");
            assert!(thread.left() - usage.right() <= gap, "{what}");
            assert!(more.left() - thread.right() <= gap, "{what}");
        }
        folds.push((two_rows, header.folded.len(), usage.size.width));
    }
    // Wide: nothing folded. Medium: usage and ID below. Narrow: Compact then Mode fold.
    let counts = folds
        .iter()
        .map(|(two, folded, _)| (*two, *folded))
        .collect::<Vec<_>>();
    assert_eq!(
        counts,
        [(false, 0), (false, 0), (true, 0), (true, 1), (true, 2)],
        "{folds:?}"
    );
}

#[gpui::test]
fn the_billing_badge_is_measured_and_the_two_row_contract_holds_in_every_design(
    cx: &mut TestAppContext,
) {
    let middle = |b: gpui::Bounds<gpui::Pixels>| b.top() + b.size.height / 2.;
    for theme in crate::theme::ThemeChoice::ALL {
        let face = match theme {
            crate::theme::ThemeChoice::Native => ui_text::Face::SystemMono,
            crate::theme::ThemeChoice::Hermes => ui_text::Face::Hermes,
            _ => ui_text::Face::Menlo,
        };
        let before = ui_text::set_for_tests(1., face);
        for width in [900., 520., 330.] {
            let h = header_with(cx, width, theme, true);
            let badge = h.billing.expect("API billing badge is visible");
            let what = format!(
                "{theme:?} at {width}: badge {badge:?}, ring {:?}, more {:?}",
                h.usage, h.more
            );
            assert!((middle(h.more) - middle(h.first)).abs() <= px(1.), "{what}");
            for control in h.controls {
                assert!(
                    (middle(control) - middle(h.first)).abs() <= px(1.),
                    "{what}"
                );
            }
            assert!(badge.size.width > px(0.), "{what}");
            assert!(
                badge.right() + ui_text::space(composer::BAR_GAP) <= h.usage.left() + px(1.),
                "{what}"
            );
            assert!(h.usage.right() <= h.thread.left(), "{what}");
            assert!((middle(badge) - middle(h.usage)).abs() <= px(1.), "{what}");
            assert!(
                (middle(h.usage) - middle(h.thread)).abs() <= px(1.),
                "{what}"
            );
            assert!(
                (h.more.right() - (px(width) - ui_text::space(composer::BAR_INSET))).abs()
                    <= px(1.),
                "{what}"
            );
            if h.thread.top() > h.more.bottom() {
                assert!(
                    (h.thread.top() - h.more.bottom() - ui_text::space(composer::BAR_GAP)).abs()
                        <= px(1.),
                    "{what}: no third row"
                );
            } else {
                assert!(h.folded.is_empty(), "{what}");
                assert!(
                    (middle(h.usage) - middle(h.first)).abs() <= px(1.),
                    "{what}"
                );
                assert!(h.thread.right() <= h.more.left(), "{what}");
            }
        }
        ui_text::set_for_tests(before.0, before.1);
    }
}

#[gpui::test]
fn a_provider_switch_clears_the_old_limit_banner_and_meter_windows(cx: &mut TestAppContext) {
    let (handle, view, _) = mount(cx);
    let old = super::testing::info("chat");
    let mut resolved = ItemBody::notice(
        NoticeLevel::Error,
        "Old weekly limit reached",
        Some("rate_limit:weekly"),
    );
    view.update(cx, |view, _| {
        view.model
            .transcript
            .apply(&ChatEvent::Info { info: old.clone() });
        view.model.transcript.apply(&ChatEvent::RateLimits {
            windows: vec![crate::chat::model::RateWindow {
                id: "weekly".into(),
                label: "Old weekly".into(),
                used_percent: 100.,
                resets_at: Some(u64::MAX),
                warn_at: 70.,
            }],
        });
    });
    push(&view, cx, "old-limit", resolved.clone());
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(shown(window, "old-limit"));
    })
    .unwrap();
    if let ItemBody::Notice { resolved, .. } = &mut resolved {
        *resolved = true;
    }
    push(&view, cx, "old-limit", resolved);
    view.update(cx, |view, cx| {
        let mut next = old;
        next.provider = match next.provider {
            Provider::Claude => Provider::Codex,
            Provider::Codex | Provider::Muse => Provider::Claude,
        };
        view.model.transcript.apply(&ChatEvent::Info { info: next });
        view.model.transcript.apply(&ChatEvent::Usage {
            usage: crate::chat::model::Usage {
                context_used: Some(10),
                context_window: Some(100),
                ..Default::default()
            },
        });
        assert!(view.model.transcript.rate_limits.is_empty());
        assert!(
            usage_chip::chip_details(&view.model.transcript.rate_limits, 0).is_empty(),
            "no old windows in the meter tooltip"
        );
        cx.notify();
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(!shown(window, "old-limit"));
        assert!(
            window.try_find("chat-context-meter").is_some(),
            "new provider has its own meter"
        );
    })
    .unwrap();
}

#[gpui::test]
fn the_session_id_card_stays_in_the_row_and_copies_the_whole_id(cx: &mut TestAppContext) {
    let (handle, view, _recording) = mount(cx);
    view.update(cx, |view, cx| {
        view.model.link = state::Link::Live;
        let mut info = super::testing::info("chat");
        info.provider_thread_id = Some("cc2091da-0000-4000-8000-feedfacecafe".into());
        view.model.transcript.apply(&ChatEvent::Info { info });
        cx.notify();
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("chat-thread", cx);
        assert_eq!(
            cx.read_from_clipboard()
                .and_then(|item| item.text())
                .as_deref(),
            Some("cc2091da-0000-4000-8000-feedfacecafe")
        );
    })
    .unwrap();
}

/// Times the ⋯ menu with a chat that has many notices. Run with
/// `cargo test --bin riwork profile_more_menu -- --ignored --nocapture`.
#[gpui::test]
#[ignore]
fn profile_more_menu(cx: &mut TestAppContext) {
    use std::time::Instant;
    let (handle, view, _recording) = mount(cx);
    view.update(cx, |view, cx| {
        view.model.link = state::Link::Live;
        for at in 0..3_000 {
            if at % 3 == 0 {
                view.model.transcript.apply(&ChatEvent::ItemCompleted {
                    item: Item {
                        id: format!("u{at}"),
                        turn_id: Some(format!("turn{at}")),
                        status: ItemStatus::Completed,
                        body: ItemBody::UserMessage {
                            text: format!("message {at}"),
                        },
                        presentation: Default::default(),
                    },
                });
            }
            view.model.transcript.apply(&ChatEvent::ItemCompleted {
                item: Item {
                    id: format!("n{at}"),
                    turn_id: Some(format!("turn{at}")),
                    status: ItemStatus::Completed,
                    body: ItemBody::notice(
                        NoticeLevel::Warning,
                        format!("Notice number {at} about something that happened"),
                        Some(if at % 2 == 0 {
                            "rate_limit:five_hour"
                        } else {
                            "turn_failed"
                        }),
                    ),
                    presentation: Default::default(),
                },
            });
        }
        cx.notify();
    });
    let frames = 10;
    let report =
        |label: &str, cx: &mut TestAppContext, step: &dyn Fn(&mut Window, &mut gpui::App)| {
            let (mut act, mut frame) = (std::time::Duration::ZERO, std::time::Duration::ZERO);
            for _ in 0..frames {
                cx.update_window(handle.into(), |_, window, cx| {
                    view.update(cx, |view, cx| {
                        view.menu = None;
                        cx.notify();
                    });
                    window.render_frame(cx);
                    let start = Instant::now();
                    step(window, cx);
                    act += start.elapsed();
                    let start = Instant::now();
                    window.render_frame(cx);
                    frame += start.elapsed();
                })
                .unwrap();
            }
            eprintln!(
                "{label}: action {:?}, next frame {:?}",
                act / frames,
                frame / frames
            );
        };
    report("re-render, menu closed", cx, &|_, cx| {
        view.update(cx, |_, cx| cx.notify())
    });
    report("menu = More, notify", cx, &|_, cx| {
        view.update(cx, |view, cx| {
            view.menu = Some(Menu::More);
            cx.notify();
        })
    });
    report("click ⋯", cx, &|window, cx| window.click("chat-more", cx));
    report("toggle_menu(More) directly", cx, &|window, cx| {
        view.update(cx, |view, cx| view.toggle_menu(Menu::More, window, cx))
    });
    report("focus the view", cx, &|window, cx| {
        view.update(cx, |view, cx| view.focus(window, cx))
    });
    report("click Compact (another button)", cx, &|window, cx| {
        window.click("chat-compact", cx)
    });
    cx.update_window(handle.into(), |_, window, cx| {
        let start = Instant::now();
        for _ in 0..frames {
            view.update(cx, |view, cx| {
                let _ = view.menu_popover_for_profile(window, cx);
            });
        }
        eprintln!("menu build alone: {:?}", start.elapsed() / frames);
    })
    .unwrap();
    view.read_with(cx, |view, _| {
        let t = &view.model.transcript;
        let start = Instant::now();
        for _ in 0..frames {
            std::hint::black_box(view.notices.count(t));
        }
        eprintln!("count (cached): {:?}", start.elapsed() / frames);
        let start = Instant::now();
        for _ in 0..frames {
            std::hint::black_box(notices::history(t, notices::HISTORY_SHOWN));
        }
        eprintln!(
            "history (newest {}): {:?}",
            notices::HISTORY_SHOWN,
            start.elapsed() / frames
        );
        let start = Instant::now();
        for _ in 0..frames {
            std::hint::black_box(view.notices.banners(t, notices::now_unix()));
        }
        eprintln!("banners (cached): {:?}", start.elapsed() / frames);
        eprintln!("items: {}", t.items.len());
    });
}

#[gpui::test]
fn a_long_history_draws_only_the_newest_and_says_how_many_there_are(cx: &mut TestAppContext) {
    let (handle, view, _recording) = mount(cx);
    let total = notices::HISTORY_SHOWN + 50;
    view.update(cx, |view, cx| {
        view.model.link = state::Link::Live;
        for at in 0..total {
            view.model.transcript.apply(&ChatEvent::ItemCompleted {
                item: Item {
                    id: format!("n{at}"),
                    turn_id: Some("turn".into()),
                    status: ItemStatus::Completed,
                    body: ItemBody::notice(NoticeLevel::Info, format!("notice {at}"), None),
                    presentation: Default::default(),
                },
            });
        }
        cx.notify();
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("chat-more", cx);
        window.render_frame(cx);
        assert_eq!(
            window.find("chat-notice-history").label(),
            Some(format!("Notices ({total})").as_str())
        );
        window.click("chat-notice-history", cx);
        window.render_frame(cx);
        assert!(window.try_find("chat-notice-history-older").is_some());
    })
    .unwrap();
    view.read_with(cx, |view, _| {
        let shown = notices::history(&view.model.transcript, notices::HISTORY_SHOWN);
        assert_eq!(shown.len(), notices::HISTORY_SHOWN);
        assert_eq!(
            shown[0].text,
            format!("notice {}", total - 1),
            "newest first"
        );
    });
}

/// A Claude chat billed as `own` that said it runs on its subscription, with the key `saved`
/// kept in memory, never the Keychain.
fn claude_chat(cx: &mut TestAppContext, view: &Entity<ChatView>, own: ClaudeBilling, saved: bool) {
    use riwork_engine::anthropic_key::MemoryStore;
    cx.update(|app| {
        let keys = if saved {
            MemoryStore::with_key("sk-ant-dummy-banner")
        } else {
            MemoryStore::default()
        };
        app.set_global(crate::api_key::ApiKeyState::with_store(
            std::sync::Arc::new(keys),
        ))
    });
    view.update(cx, |view, cx| {
        let mut info = super::testing::info("fixture-chat");
        info.provider = Provider::Claude;
        info.claude_billing = Some(own);
        view.model.transcript.apply(&ChatEvent::Info { info });
        view.model.transcript.apply(&ChatEvent::Account {
            account: crate::chat::model::Account {
                api_key_source: None,
                plan: Some("Claude Max".into()),
            },
        });
        cx.notify();
    });
}

fn limit(kind: &str, resets_at: u64) -> ItemBody {
    let mut body = ItemBody::notice(
        NoticeLevel::Error,
        "This account has reached the five-hour usage limit.",
        Some(kind),
    );
    if let ItemBody::Notice { resets_at: at, .. } = &mut body {
        *at = Some(resets_at);
    }
    body
}

/// What the host says next, through the feed as it arrives.
fn arrive(
    cx: &mut TestAppContext,
    handle: gpui::WindowHandle<gpui_kit::base::Root>,
    view: &Entity<ChatView>,
    events: Vec<ChatEvent>,
) {
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |view, cx| {
            // Each event is new to the tab: sequence numbers only grow.
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
        })
    })
    .unwrap();
}

fn billed(own: ClaudeBilling, state: ChatState) -> ChatEvent {
    let mut info = super::testing::info("fixture-chat");
    info.provider = Provider::Claude;
    info.claude_billing = Some(own);
    info.state = state;
    ChatEvent::Info { info }
}

fn commands(recording: &std::sync::mpsc::Receiver<feed::Delivery>) -> Vec<ChatCommand> {
    recording
        .try_iter()
        .filter_map(|delivery| match delivery {
            feed::Delivery::Command(command) => Some(command),
            _ => None,
        })
        .collect()
}

fn to_api_key() -> ChatCommand {
    ChatCommand::Configure {
        model: None,
        effort: None,
        approval_mode: None,
        fast: None,
        contributor_mode: None,
        contributor_opt_out: None,
        claude_billing: Some(ClaudeBilling::ApiKey),
    }
}

#[gpui::test]
fn a_subscription_limit_switches_to_the_saved_key_and_sends_the_rejected_message_once_it_holds(
    cx: &mut TestAppContext,
) {
    let start = 1_800_000_000;
    notices::TEST_NOW.with(|now| now.set(Some(start)));
    let (handle, view, recording) = mount(cx);
    claude_chat(cx, &view, ClaudeBilling::Subscription, true);
    push(
        &view,
        cx,
        "u",
        ItemBody::UserMessage {
            text: "fix the build".into(),
        },
    );
    push(
        &view,
        cx,
        "limit",
        limit("rate_limit:five_hour", start + 3600),
    );
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(shown(window, "limit"));
        assert!(window.try_find("chat-notice-enter-api-key:limit").is_none());
        assert!(window.try_find("chat-notice-usage:limit").is_some());
        window.click("chat-notice-use-api-key:limit", cx);
    })
    .unwrap();
    // The switch first; the message only once it holds on a Claude ready again.
    assert_eq!(commands(&recording), [to_api_key()]);
    arrive(
        cx,
        handle,
        &view,
        vec![billed(ClaudeBilling::ApiKey, ChatState::Stopped)],
    );
    arrive(
        cx,
        handle,
        &view,
        vec![ChatEvent::State {
            state: ChatState::Starting,
        }],
    );
    assert_eq!(commands(&recording), []);
    arrive(
        cx,
        handle,
        &view,
        vec![
            ChatEvent::State {
                state: ChatState::Idle,
            },
            ChatEvent::Account {
                account: crate::chat::model::Account {
                    api_key_source: Some("ANTHROPIC_API_KEY".into()),
                    plan: None,
                },
            },
        ],
    );
    assert_eq!(
        commands(&recording),
        [ChatCommand::Send {
            text: "fix the build".into()
        }]
    );
    // On the key, the subscription's limit is no banner; the switch says what it did.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(!shown(window, "limit"));
        assert!(shown(window, "local:settings"));
    })
    .unwrap();
    view.read_with(cx, |view, _| {
        assert!(
            view.notices
                .local(notices::LocalKey::Settings)
                .is_some_and(|said| said.contains("sent again"))
        )
    });
    // Later changes of state send nothing more.
    arrive(
        cx,
        handle,
        &view,
        vec![billed(ClaudeBilling::ApiKey, ChatState::Idle)],
    );
    assert_eq!(commands(&recording), []);
    notices::TEST_NOW.with(|now| now.set(None));
}

#[gpui::test]
fn a_turn_that_had_begun_is_not_sent_again_and_a_refused_switch_sends_nothing(
    cx: &mut TestAppContext,
) {
    let start = 1_800_000_000;
    notices::TEST_NOW.with(|now| now.set(Some(start)));
    // The turn had produced an item before the limit: the switch alone, and it says so.
    let (handle, view, recording) = mount(cx);
    claude_chat(cx, &view, ClaudeBilling::Subscription, true);
    push(&view, cx, "u", ItemBody::UserMessage { text: "go".into() });
    push(
        &view,
        cx,
        "a",
        ItemBody::AgentMessage {
            text: "on it".into(),
        },
    );
    push(
        &view,
        cx,
        "limit",
        limit("rate_limit:seven_day", start + 3600),
    );
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("chat-notice-use-api-key:limit", cx);
    })
    .unwrap();
    assert_eq!(commands(&recording), [to_api_key()]);
    arrive(
        cx,
        handle,
        &view,
        vec![billed(ClaudeBilling::ApiKey, ChatState::Idle)],
    );
    assert_eq!(commands(&recording), []);
    view.read_with(cx, |view, _| {
        let said = view.notices.local(notices::LocalKey::Settings).unwrap();
        assert!(said.contains("Nothing was sent again"), "{said}");
    });

    // The host refuses the switch: no message goes, and the refusal is the banner.
    let (handle, view, recording) = mount(cx);
    claude_chat(cx, &view, ClaudeBilling::Subscription, true);
    push(&view, cx, "u", ItemBody::UserMessage { text: "go".into() });
    push(
        &view,
        cx,
        "limit",
        limit("rate_limit:five_hour", start + 3600),
    );
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("chat-notice-use-api-key:limit", cx);
        view.update(cx, |view, cx| {
            view.accept(
                vec![feed::FeedMsg::CommandFailed {
                    command: to_api_key(),
                    error: "Wait for the turn to finish, or interrupt it, before changing who this chat bills.".into(),
                }],
                window,
                cx,
            )
        });
    })
    .unwrap();
    assert_eq!(commands(&recording), [to_api_key()]);
    arrive(
        cx,
        handle,
        &view,
        vec![billed(ClaudeBilling::ApiKey, ChatState::Idle)],
    );
    assert_eq!(commands(&recording), [], "nothing after a refusal");
    view.read_with(cx, |view, _| {
        let said = view.notices.local(notices::LocalKey::Settings).unwrap();
        assert!(said.contains("changing who this chat bills"), "{said}");
    });

    // Set to API key already, but this Claude started without one: it is stopped, and the
    // message goes with its next start.
    let (handle, view, recording) = mount(cx);
    claude_chat(cx, &view, ClaudeBilling::ApiKey, true);
    push(&view, cx, "u", ItemBody::UserMessage { text: "go".into() });
    push(
        &view,
        cx,
        "limit",
        limit("rate_limit:five_hour", start + 3600),
    );
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("chat-notice-use-api-key:limit", cx);
    })
    .unwrap();
    assert_eq!(
        commands(&recording),
        [ChatCommand::Stop, ChatCommand::Send { text: "go".into() }]
    );
    notices::TEST_NOW.with(|now| now.set(None));
}

#[gpui::test]
fn with_no_key_saved_the_limit_opens_settings_and_a_dismissal_lasts_for_that_limit_and_reset(
    cx: &mut TestAppContext,
) {
    let start = 1_800_000_000;
    notices::TEST_NOW.with(|now| now.set(Some(start)));
    let (handle, view, recording) = mount(cx);
    claude_chat(cx, &view, ClaudeBilling::Subscription, false);
    let opened = std::rc::Rc::new(std::cell::Cell::new(false));
    let seen = opened.clone();
    cx.update(|cx| {
        cx.subscribe(&view, move |_, event: &ChatViewEvent, _| {
            if matches!(event, ChatViewEvent::OpenApiKeySettings) {
                seen.set(true);
            }
        })
        .detach()
    });
    push(&view, cx, "u", ItemBody::UserMessage { text: "go".into() });
    push(
        &view,
        cx,
        "limit",
        limit("rate_limit:five_hour", start + 3600),
    );
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("chat-notice-use-api-key:limit").is_none());
        window.click("chat-notice-enter-api-key:limit", cx);
    })
    .unwrap();
    assert!(opened.get(), "Enter API key opens Settings at the key");
    assert_eq!(commands(&recording), [], "and changes nothing in the chat");

    // Dismissed, this limit stays dismissed, and the host is told.
    cx.update_window(handle.into(), |_, window, cx| {
        window.click("chat-notice-close:limit", cx);
        window.render_frame(cx);
        assert!(!shown(window, "limit"));
    })
    .unwrap();
    assert_eq!(
        commands(&recording),
        [ChatCommand::DismissNotice {
            item_id: "limit".into()
        }]
    );
    // The next limit, with its own reset time, is a banner again, with its offer.
    push(
        &view,
        cx,
        "u2",
        ItemBody::UserMessage {
            text: "go on".into(),
        },
    );
    push(
        &view,
        cx,
        "limit-2",
        limit("rate_limit:five_hour", start + 7200),
    );
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(shown(window, "limit-2"));
        assert!(
            window
                .try_find("chat-notice-enter-api-key:limit-2")
                .is_some()
        );
    })
    .unwrap();

    // Extra usage, a model's weekly window and another provider's limit keep the plain banner.
    for (provider, kind) in [
        (Provider::Claude, "rate_limit:overage"),
        (Provider::Claude, "rate_limit:seven_day_opus"),
        (Provider::Codex, "rate_limit:five_hour"),
    ] {
        let (handle, view, _recording) = mount(cx);
        claude_chat(cx, &view, ClaudeBilling::Subscription, true);
        view.update(cx, |view, _| {
            view.model.transcript.info.as_mut().unwrap().provider = provider;
        });
        push(&view, cx, "u", ItemBody::UserMessage { text: "go".into() });
        push(&view, cx, "limit", limit(kind, start + 3600));
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(shown(window, "limit"), "{provider:?} {kind}");
            assert!(window.try_find("chat-notice-use-api-key:limit").is_none());
            assert!(window.try_find("chat-notice-enter-api-key:limit").is_none());
        })
        .unwrap();
    }
    notices::TEST_NOW.with(|now| now.set(None));
}

#[gpui::test]
fn a_message_the_user_sends_during_the_switch_is_not_followed_by_the_old_one(
    cx: &mut TestAppContext,
) {
    let start = 1_800_000_000;
    notices::TEST_NOW.with(|now| now.set(Some(start)));
    // Sent from the message box while Claude restarts: the banner's switch sends nothing after.
    let (handle, view, recording) = mount(cx);
    claude_chat(cx, &view, ClaudeBilling::Subscription, true);
    push(
        &view,
        cx,
        "u",
        ItemBody::UserMessage {
            text: "fix the build".into(),
        },
    );
    push(
        &view,
        cx,
        "limit",
        limit("rate_limit:five_hour", start + 3600),
    );
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("chat-notice-use-api-key:limit", cx);
        window.click("chat-composer", cx);
        window.input("fix the build please", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| window.press("enter", cx))
        .unwrap();
    cx.run_until_parked();
    let deliveries: Vec<_> = recording.try_iter().collect();
    assert!(
        deliveries.iter().any(|delivery| matches!(
            delivery,
            feed::Delivery::Submission { command: ChatCommand::Send { text }, .. } if text == "fix the build please"
        )),
        "the user's own message went"
    );
    arrive(
        cx,
        handle,
        &view,
        vec![billed(ClaudeBilling::ApiKey, ChatState::Idle)],
    );
    assert_eq!(commands(&recording), [], "nothing after it");

    // The transcript moved on before the switch held: the old message is not sent either.
    let (handle, view, recording) = mount(cx);
    claude_chat(cx, &view, ClaudeBilling::Subscription, true);
    push(&view, cx, "u", ItemBody::UserMessage { text: "go".into() });
    push(
        &view,
        cx,
        "limit",
        limit("rate_limit:five_hour", start + 3600),
    );
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("chat-notice-use-api-key:limit", cx);
    })
    .unwrap();
    assert_eq!(commands(&recording), [to_api_key()]);
    push(
        &view,
        cx,
        "u2",
        ItemBody::UserMessage {
            text: "from the phone".into(),
        },
    );
    arrive(
        cx,
        handle,
        &view,
        vec![billed(ClaudeBilling::ApiKey, ChatState::Idle)],
    );
    assert_eq!(commands(&recording), []);
    notices::TEST_NOW.with(|now| now.set(None));
}

#[gpui::test]
fn with_no_key_saved_the_composer_switch_opens_settings_and_shows_the_subscription_claude_bills(
    cx: &mut TestAppContext,
) {
    let (handle, view, recording) = mount(cx);
    // Set to API key, the default, with no key saved: Claude says it bills the subscription.
    claude_chat(cx, &view, ClaudeBilling::ApiKey, false);
    let opened = std::rc::Rc::new(std::cell::Cell::new(false));
    let seen = opened.clone();
    cx.update(|cx| {
        cx.subscribe(&view, move |_, event: &ChatViewEvent, _| {
            if matches!(event, ChatViewEvent::OpenApiKeySettings) {
                seen.set(true);
            }
        })
        .detach()
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let toggle = window.find("chat-billing");
        assert_eq!(toggle.label(), Some("Billing: Subscription"));
        assert_eq!(toggle.checked(), Some(false));
        window.click("chat-billing", cx);
    })
    .unwrap();
    assert!(opened.get(), "a key is entered in Settings first");
    assert_eq!(commands(&recording), [], "and the chat is left as it is");
}
