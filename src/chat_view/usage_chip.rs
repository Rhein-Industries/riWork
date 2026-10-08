//! The usage chip beside the context meter: the account's most used limit window once it
//! nears its limit ("⚠ weekly 87% · resets Thu 14:00"). A warning is no banner; only a
//! blocking limit is (see `notices`). docs/chat-notices.md has the windows' wire shape.

use gpui::{
    AnyElement, Context, Pixels, SharedString, TextRun, Window, div, prelude::*, px, relative, rgb,
};
use gpui_kit::base::TestSupportExt as _;

use crate::{
    chat::model::RateWindow,
    tooltip::{self, Look as TipLook},
    ui_text,
};

use super::{ChatView, ChatViewEvent, notices, toolbar, widgets::Look};

/// From this use on the chip's words are bold.
pub(super) const URGENT: f64 = 90.0;

/// What the chip and the context meter leave out to fit their row, in that order: the
/// meter's cost, the chip's reset time, the chip's window name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Detail {
    Full,
    NoCost,
    NoReset,
    NoName,
}

impl Detail {
    const ALL: [Self; 4] = [Self::Full, Self::NoCost, Self::NoReset, Self::NoName];
}

/// The most the group can show in `available` px, given what each step needs.
pub(super) fn fit(available: f32, need: impl Fn(Detail) -> f32) -> Detail {
    Detail::ALL
        .into_iter()
        .find(|detail| need(*detail) <= available)
        .unwrap_or(Detail::NoName)
}

/// What else shares the group's line with it at the least: the ⋯ button, the row's padding.
const RESERVE: f32 = 64.0;
/// The chip's padding, the gaps, and the meter's bar.
const CHIP_PAD: f32 = 12.0;
const GAP: f32 = 8.0;
const METER_GAP: f32 = 6.0;
const BAR: f32 = 44.0;

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

/// The chip's words: "⚠ weekly 87% · resets Thu 14:00", then without the reset time,
/// then without the window's name ("⚠ 87%").
pub(super) fn chip_text(window: &RateWindow, now: u64, detail: Detail) -> String {
    let percent = window.used_percent.round() as u32;
    if detail >= Detail::NoName {
        return format!("⚠ {percent}%");
    }
    let used = format!("⚠ {} {percent}%", window.label);
    match reset(window, now) {
        Some(reset) if detail < Detail::NoReset => format!("{used} · {reset}"),
        _ => used,
    }
}

fn text_width(text: &str, family: SharedString, size: Pixels, bold: bool, window: &Window) -> f32 {
    let mut font = gpui::font(family);
    if bold {
        font.weight = gpui::FontWeight::BOLD;
    }
    let run = TextRun {
        len: text.len(),
        font,
        color: gpui::black(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line =
        window
            .text_system()
            .shape_line(SharedString::from(text.to_owned()), size, &[run], None);
    f32::from(line.width)
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
    /// The usage chip and the context meter, together so they wrap as one, leaving out
    /// detail (`Detail`) until they fit the chat's width, and never wider than their row.
    pub(super) fn usage_group(
        &self,
        look: Look,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let now = notices::now_unix();
        let windows = &self.model.transcript.rate_limits;
        let warned = warned(windows, now);
        let usage = self.model.transcript.usage.as_ref();
        if warned.is_none() && usage.is_none() {
            return None;
        }
        let measured = self.composer_width.get();
        let detail = if measured <= 0.0 {
            Detail::Full
        } else {
            let space = |base: f32| f32::from(ui_text::space(base));
            let chip = |detail| {
                warned.map_or(0.0, |w| {
                    text_width(
                        &chip_text(w, now, detail),
                        ui_text::ui_family(),
                        ui_text::text(11.0),
                        w.used_percent >= URGENT,
                        window,
                    ) + space(CHIP_PAD)
                        + space(GAP)
                })
            };
            let meter = |detail| {
                usage.map_or(0.0, |usage| {
                    let mono = |text: &str| {
                        text_width(
                            text,
                            ui_text::mono_family(),
                            ui_text::text(10.0),
                            false,
                            window,
                        )
                    };
                    let cost = usage
                        .cost_usd
                        .filter(|_| detail == Detail::Full)
                        .map_or(0.0, |cost| {
                            mono(&toolbar::cost_text(cost)) + space(METER_GAP)
                        });
                    space(BAR) + space(METER_GAP) + mono(&toolbar::usage_text(usage)) + cost
                })
            };
            fit(measured - space(RESERVE), |detail| {
                chip(detail) + meter(detail)
            })
        };
        Some(
            div()
                .id("chat-usage-group")
                .min_w_0()
                .max_w_full()
                .overflow_hidden()
                .flex()
                .items_center()
                .gap(ui_text::space(GAP))
                .children(warned.map(|w| self.usage_chip(w, detail, look, cx)))
                .children(usage.map(|usage| self.context_meter(usage, detail, look)))
                .test_support()
                .into_any_element(),
        )
    }

    fn usage_chip(
        &self,
        window: &RateWindow,
        detail: Detail,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let now = notices::now_unix();
        let windows = &self.model.transcript.rate_limits;
        let tone = look.tone(super::cards::notice_tone(
            crate::chat::model::NoticeLevel::Warning,
        ));
        div()
            .id("chat-usage-chip")
            .relative()
            .min_w_0()
            .flex()
            .items_center()
            .px(ui_text::space(CHIP_PAD / 2.0))
            .rounded(px(if look.native { 9.0 } else { 3.0 }))
            .bg(rgb(look.tint(tone, 0.10)))
            .font_family(ui_text::ui_family())
            .text_size(ui_text::text(11.0))
            .text_color(rgb(tone))
            .when(window.used_percent >= URGENT, |chip| {
                chip.font_weight(gpui::FontWeight::BOLD)
            })
            .cursor_pointer()
            .role(gpui::Role::Button)
            .aria_label(chip_text(window, now, Detail::Full))
            .on_click(cx.listener(|_, _, _, cx| cx.emit(ChatViewEvent::ShowUsage)))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .child(chip_text(window, now, detail)),
            )
            .child(tooltip::anchor(
                chip_details(windows, now),
                TipLook::Control,
            ))
            .test_support()
            .into_any_element()
    }

    /// How full the context window is: a bar and the counts, and the cost while it fits.
    fn context_meter(
        &self,
        usage: &crate::chat::model::Usage,
        detail: Detail,
        look: Look,
    ) -> AnyElement {
        let colors = look.colors;
        let fraction = toolbar::context_fraction(usage);
        div()
            .id("chat-context-meter")
            .relative()
            .min_w_0()
            .flex()
            .items_center()
            .gap(ui_text::space(METER_GAP))
            .text_size(ui_text::text(10.0))
            .text_color(rgb(colors.muted))
            // Counts, in the monospace accent where Native keeps one.
            .font_family(ui_text::mono_family())
            .whitespace_nowrap()
            .children(fraction.map(|fraction| {
                div()
                    .flex_none()
                    .w(ui_text::space(BAR))
                    .h(ui_text::space(5.0))
                    .rounded(px(3.0))
                    .when(look.native, |track| track.rounded_full())
                    .bg(rgb(colors.divider))
                    .child(
                        div()
                            .h_full()
                            .rounded(px(3.0))
                            .when(look.native, |fill| fill.rounded_full())
                            .w(relative(fraction))
                            .bg(rgb(if fraction >= 0.9 {
                                look.error()
                            } else if fraction >= 0.7 {
                                colors.gold
                            } else {
                                colors.cyan
                            })),
                    )
            }))
            .child(div().min_w_0().truncate().child(toolbar::usage_text(usage)))
            .children(
                usage
                    .cost_usd
                    .filter(|_| detail == Detail::Full)
                    .map(|cost| div().flex_none().child(toolbar::cost_text(cost))),
            )
            .child(tooltip::anchor(
                toolbar::usage_details(usage),
                TipLook::Control,
            ))
            .test_support()
            .into_any_element()
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
    fn the_group_leaves_out_the_cost_then_the_reset_then_the_name_to_fit() {
        let need = |detail: Detail| match detail {
            Detail::Full => 400.0,
            Detail::NoCost => 300.0,
            Detail::NoReset => 200.0,
            Detail::NoName => 120.0,
        };
        assert_eq!(fit(500.0, need), Detail::Full);
        assert_eq!(fit(399.0, need), Detail::NoCost);
        assert_eq!(fit(250.0, need), Detail::NoReset);
        assert_eq!(fit(150.0, need), Detail::NoName);
        // Narrower still, the least there is (and the row clips it).
        assert_eq!(fit(50.0, need), Detail::NoName);
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
        assert_eq!(
            chip_text(&w, now, Detail::Full),
            "⚠ weekly 87% · resets Sat 14:00"
        );
        assert_eq!(
            chip_text(&w, now, Detail::NoCost),
            chip_text(&w, now, Detail::Full)
        );
        assert_eq!(chip_text(&w, now, Detail::NoReset), "⚠ weekly 87%");
        assert_eq!(chip_text(&w, now, Detail::NoName), "⚠ 87%");
        let five = window("five_hour", "5h", 40.0, 70.0, None);
        assert_eq!(
            chip_details(&[w, five], now),
            "weekly 87% used · resets Sat 14:00; 5h 40% used"
        );
    }
}
