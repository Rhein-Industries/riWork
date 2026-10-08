//! The usage chip beside the context meter: the account's most used limit window once it
//! nears its limit ("⚠ weekly 87% · resets Thu 14:00"). A warning is no banner; only a
//! blocking limit is (see `notices`). docs/chat-notices.md has the windows' wire shape.

use gpui::{AnyElement, Context, div, prelude::*, rgb};
use gpui_kit::base::TestSupportExt as _;

use crate::{
    chat::model::RateWindow,
    tooltip::{self, Look as TipLook},
    ui_text,
};

use super::{ChatView, ChatViewEvent, notices, widgets::Look};

/// From this use on the chip's words are bold.
pub(super) const URGENT: f64 = 90.0;

/// Below this message box width the chip leaves out the reset time.
pub(super) const TIGHT: f32 = 520.0;

/// The window the chip shows: of the ones at or past their warning, the most used, while
/// its reset has not passed.
pub(super) fn warned(windows: &[RateWindow], now: u64) -> Option<&RateWindow> {
    windows
        .iter()
        .filter(|window| window.used_percent >= window.warn_at)
        .filter(|window| window.resets_at.is_none_or(|at| at > now))
        .max_by(|a, b| a.used_percent.total_cmp(&b.used_percent))
}

/// "resets Thu 14:00", "resets today 14:00".
fn reset(window: &RateWindow, now: u64) -> Option<String> {
    window
        .resets_at
        .and_then(|at| notices::reset_text(at, now))
        .map(|text| text.replacen(" at ", " ", 1))
}

/// The chip's words: "⚠ weekly 87% · resets Thu 14:00", or "⚠ weekly 87%" when `tight`.
pub(super) fn chip_text(window: &RateWindow, now: u64, tight: bool) -> String {
    let used = format!("⚠ {} {}%", window.label, window.used_percent.round() as u32);
    match reset(window, now) {
        Some(reset) if !tight => format!("{used} · {reset}"),
        _ => used,
    }
}

/// Every window, for the chip's hover hint (one line: a hint does not wrap).
pub(super) fn chip_details(windows: &[RateWindow], now: u64) -> String {
    windows
        .iter()
        .map(|window| {
            let used = format!(
                "{} {}% used",
                window.label,
                window.used_percent.round() as u32
            );
            match reset(window, now) {
                Some(reset) => format!("{used} · {reset}"),
                None => used,
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

impl ChatView {
    pub(super) fn usage_chip(&self, look: Look, cx: &mut Context<Self>) -> Option<AnyElement> {
        let now = notices::now_unix();
        let windows = &self.model.transcript.rate_limits;
        let window = warned(windows, now)?;
        let tight = self.composer_width.get() > 0.0 && self.composer_width.get() < TIGHT;
        let tone = look.tone(super::cards::notice_tone(
            crate::chat::model::NoticeLevel::Warning,
        ));
        Some(
            div()
                .id("chat-usage-chip")
                .relative()
                .flex_none()
                .flex()
                .items_center()
                .px(ui_text::space(6.0))
                .rounded(gpui::px(if look.native { 9.0 } else { 3.0 }))
                .bg(rgb(look.tint(tone, 0.10)))
                .text_size(ui_text::text(11.0))
                .text_color(rgb(tone))
                .when(window.used_percent >= URGENT, |chip| {
                    chip.font_weight(gpui::FontWeight::BOLD)
                })
                .whitespace_nowrap()
                .cursor_pointer()
                .role(gpui::Role::Button)
                .aria_label(chip_text(window, now, false))
                .on_click(cx.listener(|_, _, _, cx| cx.emit(ChatViewEvent::ShowUsage)))
                .child(chip_text(window, now, tight))
                .child(tooltip::anchor(
                    chip_details(windows, now),
                    TipLook::Control,
                ))
                .test_support()
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(
        id: &str,
        label: &str,
        used: f64,
        warn_at: f64,
        resets_at: Option<u64>,
    ) -> RateWindow {
        RateWindow {
            id: id.into(),
            label: label.into(),
            used_percent: used,
            resets_at,
            warn_at,
        }
    }

    #[test]
    fn the_chip_shows_the_most_used_window_past_its_warning_until_it_resets() {
        let windows = [
            window("five_hour", "5h", 69.0, 70.0, Some(200)),
            window("seven_day", "weekly", 72.0, 70.0, Some(300)),
            window("seven_day_opus", "weekly Opus", 80.0, 70.0, Some(100)),
        ];
        assert_eq!(warned(&windows, 50).unwrap().id, "seven_day_opus");
        // Opus reset: the weekly one; 5h stays under its threshold.
        assert_eq!(warned(&windows, 100).unwrap().id, "seven_day");
        assert!(warned(&windows, 300).is_none());
        // Codex's 5h window on Plus warns from 50, the weekly one from 75.
        let codex = [
            window("primary", "5h", 55.0, 50.0, None),
            window("secondary", "weekly", 74.0, 75.0, None),
        ];
        assert_eq!(warned(&codex, 0).unwrap().id, "primary");
        assert!(warned(&[window("secondary", "weekly", 49.0, 50.0, None)], 0).is_none());
    }

    #[test]
    fn the_chip_reads_as_window_percent_and_reset_and_shortens_when_tight() {
        use chrono::{Local, TimeZone};
        let now = Local.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap();
        let thursday = Local.with_ymd_and_hms(2026, 10, 10, 14, 0, 0).unwrap();
        let w = window(
            "seven_day",
            "weekly",
            86.6,
            70.0,
            Some(thursday.timestamp() as u64),
        );
        let now = now.timestamp() as u64;
        assert_eq!(chip_text(&w, now, false), "⚠ weekly 87% · resets Sat 14:00");
        assert_eq!(chip_text(&w, now, true), "⚠ weekly 87%");
        let five = window("five_hour", "5h", 40.0, 70.0, None);
        assert_eq!(
            chip_details(&[w, five], now),
            "weekly 87% used · resets Sat 14:00; 5h 40% used"
        );
    }
}
