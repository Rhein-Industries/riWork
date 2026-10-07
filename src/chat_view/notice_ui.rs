//! The notice banners inside the message box's bar, and the history the ⋯ menu opens (see
//! `notices` for which ones show).

use gpui::{AnyElement, Context, KeyDownEvent, Window, div, prelude::*, px, rgb};
use gpui_kit::base::TestSupportExt as _;

use crate::{chat::model::NoticeLevel, icons, ui_text};

use super::{
    ChatView, ChatViewEvent, cards,
    notices::{self, Banner},
    widgets::{self, Look, button},
};

/// The SF Symbol Native leads a banner with.
fn symbol(level: NoticeLevel) -> &'static str {
    match level {
        NoticeLevel::Info => "info.circle",
        NoticeLevel::Warning => "exclamationmark.triangle",
        NoticeLevel::Error => "xmark.octagon",
    }
}

fn escape(event: &KeyDownEvent) -> bool {
    event.keystroke.key == "escape"
}

impl ChatView {
    fn dismiss_notice(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.notices.dismiss(id);
        // The keys go back to the message box, not to nothing.
        self.focus_composer = true;
        window.refresh();
        cx.notify();
    }

    pub(super) fn toggle_notice_history(&mut self, cx: &mut Context<Self>) {
        self.notices.history = !self.notices.history;
        self.menu = None;
        cx.notify();
    }

    /// The close button of a banner or the history: a focusable Kit button that Esc also
    /// presses while it has the focus.
    fn close_button(
        &self,
        id: String,
        name: &'static str,
        look: Look,
        on_close: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + Clone + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let close = if look.native {
            widgets::symbol_button_sized(id, "xmark", name, look, ui_text::space(18.0))
        } else {
            button(id, "×", None, look).accessibility_label(name)
        };
        let on_key = on_close.clone();
        close
            .on_click(cx.listener(move |view, _, window, cx| on_close(view, window, cx)))
            .on_key_down(cx.listener(move |view, event: &KeyDownEvent, window, cx| {
                if escape(event) {
                    cx.stop_propagation();
                    on_key(view, window, cx);
                }
            }))
            .into_any_element()
    }

    /// One banner: its level's symbol and color, the text, a usage limit's reset time and
    /// the way to the Usage panel, and ×.
    fn notice_banner(&self, notice: &Banner, look: Look, cx: &mut Context<Self>) -> AnyElement {
        let colors = look.colors;
        let tone = look.tone(cards::notice_tone(notice.level));
        let ink = if notice.level == NoticeLevel::Info {
            colors.text
        } else {
            tone
        };
        let reset = notice
            .resets_at
            .and_then(|at| notices::reset_text(at, notices::now_unix()));
        let id = notice.id.clone();
        div()
            .id(gpui::SharedString::from(format!(
                "chat-notice:{}",
                notice.id
            )))
            .w_full()
            .flex()
            .items_start()
            .gap(ui_text::space(8.0))
            .px(ui_text::space(10.0))
            .py(ui_text::space(5.0))
            .rounded(px(if look.native { 8.0 } else { 3.0 }))
            .border_1()
            // An error weighs more than a warning, also where both share Native's one
            // signal color.
            .border_color(rgb(if notice.level == NoticeLevel::Error {
                tone
            } else {
                look.tint(tone, 0.35)
            }))
            .bg(rgb(look.tint(
                tone,
                if notice.level == NoticeLevel::Error {
                    0.18
                } else {
                    0.10
                },
            )))
            .text_size(ui_text::text(11.0))
            .text_color(rgb(ink))
            .role(gpui::Role::Status)
            .aria_label(notice.text.clone())
            .children(look.native.then(|| {
                div()
                    .flex_none()
                    .pt(ui_text::space(2.0))
                    .child(icons::symbol(symbol(notice.level), 11.0, Some(tone)))
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(ui_text::space(2.0))
                    .child(
                        // A long message scrolls here and leaves × in reach.
                        div()
                            .id(gpui::SharedString::from(format!(
                                "chat-notice-text:{}",
                                notice.id
                            )))
                            .max_h(ui_text::space(90.0))
                            .overflow_y_scroll()
                            .child(notice.text.clone()),
                    )
                    .children((notice.usage_limit()).then(|| {
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(ui_text::space(8.0))
                            .text_size(ui_text::text(10.0))
                            .text_color(rgb(colors.muted))
                            .children(
                                reset.map(|reset| div().child(widgets::sentence(&reset, look))),
                            )
                            .child(
                                button(
                                    format!("chat-notice-usage:{}", notice.id),
                                    "Show usage",
                                    None,
                                    look,
                                )
                                .on_click(
                                    cx.listener(|_, _, _, cx| cx.emit(ChatViewEvent::ShowUsage)),
                                ),
                            )
                    })),
            )
            .test_support()
            .child(self.close_button(
                format!("chat-notice-close:{}", notice.id),
                "Dismiss",
                look,
                move |view, window, cx| view.dismiss_notice(&id, window, cx),
                cx,
            ))
            .into_any_element()
    }

    /// The banners, newest on top: two, then "n more", or all of them once it is pressed;
    /// the history in their place while it is open.
    pub(super) fn notice_stack(&self, look: Look, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.notices.history {
            return Some(self.notice_history(look, cx));
        }
        let banners = self
            .notices
            .banners(&self.model.transcript, notices::now_unix());
        if banners.is_empty() {
            return None;
        }
        let hidden = banners.len().saturating_sub(notices::VISIBLE);
        let shown = if self.notices.expanded {
            banners.len()
        } else {
            notices::VISIBLE
        };
        let colors = look.colors;
        Some(
            div()
                .id("chat-notices")
                .w_full()
                .flex()
                .flex_col()
                .gap(ui_text::space(4.0))
                .role(gpui::Role::Group)
                .aria_label("Notices")
                .children(
                    banners
                        .iter()
                        .take(shown)
                        .map(|notice| self.notice_banner(notice, look, cx)),
                )
                .children((hidden > 0).then(|| {
                    let label = if self.notices.expanded {
                        "Show fewer".to_owned()
                    } else {
                        format!("{hidden} more")
                    };
                    div()
                        .flex()
                        .justify_end()
                        .text_color(rgb(colors.muted))
                        .child(
                            button("chat-notices-more", label, None, look)
                                .aria_expanded(self.notices.expanded)
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.notices.expanded = !view.notices.expanded;
                                    cx.notify();
                                })),
                        )
                }))
                .into_any_element(),
        )
    }

    /// Every notice of the chat, newest first, with whether it was resolved.
    fn notice_history(&self, look: Look, cx: &mut Context<Self>) -> AnyElement {
        let colors = look.colors;
        let all = notices::history(&self.model.transcript);
        let close = |view: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
            view.notices.history = false;
            view.focus_composer = true;
            window.refresh();
            cx.notify();
        };
        div()
            .id("chat-notice-history")
            .w_full()
            .flex()
            .flex_col()
            .rounded(px(if look.native { 8.0 } else { 3.0 }))
            .border_1()
            .border_color(rgb(colors.divider))
            .bg(rgb(colors.panel_active))
            .role(gpui::Role::Group)
            .aria_label("Notice history")
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(ui_text::space(8.0))
                    .px(ui_text::space(10.0))
                    .py(ui_text::space(4.0))
                    .border_b_1()
                    .border_color(rgb(colors.divider))
                    .text_size(ui_text::text(11.0))
                    .text_color(rgb(colors.text))
                    .when(look.native, |head| {
                        head.font_weight(gpui::FontWeight::SEMIBOLD)
                    })
                    .child(div().flex_1().child(format!("Notices ({})", all.len())))
                    .child(self.close_button(
                        "chat-notice-history-close".into(),
                        "Close notices",
                        look,
                        close,
                        cx,
                    )),
            )
            .child(
                div()
                    .id("chat-notice-history-list")
                    .max_h(ui_text::space(180.0))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .children(all.is_empty().then(|| {
                        div()
                            .px(ui_text::space(10.0))
                            .py(ui_text::space(6.0))
                            .text_size(ui_text::text(11.0))
                            .text_color(rgb(colors.muted))
                            .child("No notices in this chat.")
                    }))
                    .test_support()
                    .children(all.iter().map(|notice| {
                        let tone = look.tone(cards::notice_tone(notice.level));
                        div()
                            .flex()
                            .items_start()
                            .gap(ui_text::space(8.0))
                            .px(ui_text::space(10.0))
                            .py(ui_text::space(4.0))
                            .text_size(ui_text::text(11.0))
                            .text_color(rgb(if notice.resolved { colors.muted } else { tone }))
                            .children(look.native.then(|| {
                                div()
                                    .flex_none()
                                    .pt(ui_text::space(2.0))
                                    .child(icons::symbol(symbol(notice.level), 10.0, Some(tone)))
                            }))
                            .child(div().flex_1().min_w_0().child(notice.text.clone()))
                            .children(notice.resolved.then(|| {
                                div()
                                    .flex_none()
                                    .text_size(ui_text::text(10.0))
                                    .text_color(rgb(colors.muted))
                                    .child(widgets::sentence("resolved", look))
                            }))
                    })),
            )
            .into_any_element()
    }
}
