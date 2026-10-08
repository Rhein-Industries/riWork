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
            NoticeLevel::Warning,
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
        NoticeLevel::Warning,
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
