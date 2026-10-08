//! The notice banners inside the message box's bar, and the history the ⋯ menu opens (see
//! `notices` for which ones show).

use gpui::{AnyElement, Context, KeyDownEvent, Pixels, Window, div, prelude::*, px, rgb};
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

/// The height of one line of a banner's text, and of what sits beside it.
pub(super) const LINE: f32 = 16.0;

/// A banner's padding above and below its text.
pub(super) const PAD_Y: f32 = 5.0;

/// The tallest the banners get together before they scroll.
pub(super) const MAX_STACK: f32 = 200.0;

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

    /// A limit's banner goes at its reset time, also in a chat where nothing else happens:
    /// redraw then, and look for the next one.
    pub(super) fn schedule_notice_expiry(&mut self, cx: &mut Context<Self>) {
        let now = notices::now_unix();
        let Some(at) = self.notices.next_expiry(&self.model.transcript, now) else {
            self.notice_expiry = None;
            return;
        };
        if self
            .notice_expiry
            .as_ref()
            .is_some_and(|(due, _)| *due == at)
        {
            return;
        }
        let wait = std::time::Duration::from_secs(at.saturating_sub(now));
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |view, cx| {
                view.notice_expiry = None;
                view.schedule_notice_expiry(cx);
                cx.notify();
            });
        });
        self.notice_expiry = Some((at, task));
    }

    pub(super) fn toggle_notice_history(&mut self, cx: &mut Context<Self>) {
        self.notices.history = !self.notices.history;
        self.menu = None;
        cx.notify();
    }

    /// The close button of a banner or the history: a focusable Kit button that Esc also
    /// presses while it has the focus. In a banner (`line`) it is one text line tall and
    /// has no edge, so it adds nothing to the banner's height.
    fn close_button(
        &self,
        id: String,
        name: &'static str,
        look: Look,
        line: Option<Pixels>,
        on_close: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + Clone + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let close = match (look.native, line) {
            (true, line) => widgets::symbol_button_sized(
                id,
                "xmark",
                name,
                look,
                line.unwrap_or(ui_text::space(18.0)),
            ),
            (false, None) => button(id, "×", None, look).accessibility_label(name),
            (false, Some(line)) => button(id, "×", None, look)
                .accessibility_label(name)
                // The edge only shows the keyboard focus.
                .border_color(gpui::transparent_black())
                .bg(gpui::transparent_black())
                .px(ui_text::space(4.0))
                .py(px(0.0))
                .line_height(line)
                .text_size(ui_text::text(12.0)),
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

    /// One banner, a single row: its level's symbol, the text (wrapping, with a usage
    /// limit's reset time under it), the way to the Usage panel, and ×. The symbol and the
    /// buttons sit beside the text's first line, each in a box one line tall, so the banner
    /// is as tall as its text and the same padding goes all around it.
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
        let line = ui_text::text(LINE);
        let beside_first_line =
            |child: AnyElement| div().flex_none().h(line).flex().items_center().child(child);
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
            .py(ui_text::space(PAD_Y))
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
            .line_height(line)
            .text_color(rgb(ink))
            .role(gpui::Role::Status)
            .aria_label(notice.text.clone())
            .children(look.native.then(|| {
                beside_first_line(
                    icons::symbol(symbol(notice.level), 11.0, Some(tone)).into_any_element(),
                )
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        // A long message scrolls here and leaves × in reach.
                        div()
                            .id(gpui::SharedString::from(format!(
                                "chat-notice-text:{}",
                                notice.id
                            )))
                            .max_h(ui_text::space(90.0))
                            .overflow_y_scroll()
                            .child(notice.text.clone())
                            .test_support(),
                    )
                    .children(reset.map(|reset| {
                        div()
                            .text_size(ui_text::text(10.0))
                            .text_color(rgb(colors.muted))
                            .child(widgets::sentence(&reset, look))
                    })),
            )
            .children(notice.usage_limit().then(|| {
                beside_first_line(
                    button(
                        format!("chat-notice-usage:{}", notice.id),
                        "Show usage",
                        None,
                        look,
                    )
                    .py(px(0.0))
                    .line_height(line)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ChatViewEvent::ShowUsage)))
                    .into_any_element(),
                )
            }))
            .test_support()
            .child(beside_first_line(self.close_button(
                format!("chat-notice-close:{}", notice.id),
                "Dismiss",
                look,
                Some(line),
                move |view, window, cx| view.dismiss_notice(&id, window, cx),
                cx,
            )))
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
                .child(
                    // However many there are, the stack scrolls within a bound, so the
                    // message box and "Show fewer" stay in reach.
                    div()
                        .id("chat-notices-list")
                        .w_full()
                        .max_h(ui_text::space(MAX_STACK))
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .gap(ui_text::space(4.0))
                        .children(
                            banners
                                .iter()
                                .take(shown)
                                .map(|notice| self.notice_banner(notice, look, cx)),
                        )
                        .test_support(),
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
                        None,
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
