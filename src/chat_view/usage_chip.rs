//! The context meter in the chat's toolbar: a ring filling by the share of the context in use,
//! the counts and the cost. The account's limit windows are in its hover hint and the Usage
//! panel it opens; nearing a limit is no banner and no chip, only a blocking limit is a banner
//! (see `notices`). docs/chat-notices.md has the windows' wire shape.

use gpui::{
    AnyElement, Context, PathBuilder, Pixels, SharedString, TextRun, Window, canvas, div, point,
    prelude::*, px, rgb,
};
use gpui_kit::base::TestSupportExt as _;

use crate::{
    chat::model::RateWindow,
    tooltip::{self, Look as TipLook},
    ui_text,
};

use super::{ChatView, ChatViewEvent, notices, toolbar, widgets::Look};

/// What the context meter leaves out to fit its row: the cost.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Detail {
    Full,
    NoCost,
}

impl Detail {
    const ALL: [Self; 2] = [Self::Full, Self::NoCost];
}

/// The most the meter can show in `available` px, given what each step needs.
pub(super) fn fit(available: f32, need: impl Fn(Detail) -> f32) -> Detail {
    Detail::ALL
        .into_iter()
        .find(|detail| need(*detail) <= available)
        .unwrap_or(Detail::NoCost)
}

/// What else shares the meter's line with it at the least: the ⋯ button, the row's padding.
const RESERVE: f32 = 64.0;
/// The gaps, and the meter's ring.
const METER_GAP: f32 = 6.0;
const RING: f32 = 12.0;
/// The ring's line, at the row's scale.
const RING_LINE: f32 = 2.0;

/// The window nearest its limit: of the ones at or past their warning, the most used, while
/// its reset has not passed (the meter's hint is redrawn when it resets).
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

/// Every window, for the meter's hover hint (one line: a hint does not wrap).
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
    /// The context meter, leaving out its cost (`Detail`) until it fits the chat's width, and
    /// never wider than its row.
    pub(super) fn usage_group(
        &self,
        look: Look,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let usage = self.model.transcript.usage.as_ref()?;
        let measured = self.composer_width.get();
        let detail = if measured <= 0.0 {
            Detail::Full
        } else {
            let space = |base: f32| f32::from(ui_text::space(base));
            let mono = |text: &str| {
                text_width(
                    text,
                    ui_text::mono_family(),
                    ui_text::text(10.0),
                    false,
                    window,
                )
            };
            let ring = toolbar::percent_text(usage).map_or(0.0, |percent| {
                space(RING) + space(METER_GAP / 2.0) + mono(&percent) + space(METER_GAP)
            });
            let counts = ring + mono(&toolbar::usage_text(usage));
            let cost = usage.cost_usd.map_or(0.0, |cost| {
                space(METER_GAP) + mono(&toolbar::cost_text(cost))
            });
            fit(measured - space(RESERVE), |detail| match detail {
                Detail::Full => counts + cost,
                Detail::NoCost => counts,
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
                .child(self.context_meter(usage, detail, look, cx))
                .test_support()
                .into_any_element(),
        )
    }

    /// How full the context window is: a ring filling by the share in use with the percent
    /// beside it, then the counts, and the cost while it fits.
    fn context_meter(
        &self,
        usage: &crate::chat::model::Usage,
        detail: Detail,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = look.colors;
        let fraction = toolbar::context_fraction(usage);
        let now = notices::now_unix();
        let windows = &self.model.transcript.rate_limits;
        let hint = if windows.is_empty() {
            toolbar::usage_details(usage)
        } else {
            format!(
                "{}; {}",
                toolbar::usage_details(usage),
                chip_details(windows, now)
            )
        };
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
            .cursor_pointer()
            .role(gpui::Role::Button)
            .aria_label(match toolbar::percent_text(usage) {
                Some(percent) => format!("Context {percent} used, open Usage"),
                None => "Open Usage".to_owned(),
            })
            .on_click(cx.listener(|_, _, _, cx| cx.emit(ChatViewEvent::ShowUsage)))
            .children(fraction.map(|fraction| {
                let fill = if fraction >= 0.9 {
                    look.error()
                } else if fraction >= 0.7 {
                    colors.gold
                } else {
                    colors.cyan
                };
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(ui_text::space(METER_GAP / 2.0))
                    .child(ring(fraction, colors.divider, fill))
                    .children(toolbar::percent_text(usage))
            }))
            .child(div().min_w_0().truncate().child(toolbar::usage_text(usage)))
            .children(
                usage
                    .cost_usd
                    .filter(|_| detail == Detail::Full)
                    .map(|cost| div().flex_none().child(toolbar::cost_text(cost))),
            )
            .child(tooltip::anchor(hint, TipLook::Control))
            .test_support()
            .into_any_element()
    }
}

/// A circle outline in `track`, its share in use drawn over it in `fill` from the top,
/// clockwise, as the iPhone's meter draws it.
fn ring(fraction: f32, track: u32, fill: u32) -> AnyElement {
    let line = f32::from(ui_text::space(RING_LINE));
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let side = f32::from(bounds.size.width.min(bounds.size.height));
            let radius = (side - line) / 2.0;
            let center = (side / 2.0, side / 2.0);
            let stroke = |points: Vec<(f32, f32)>, closed: bool| {
                let mut path = PathBuilder::stroke(px(line));
                let points: Vec<_> = points
                    .into_iter()
                    .map(|(x, y)| point(px(x), px(y)))
                    .collect();
                path.add_polygon(&points, closed);
                path.translate(bounds.origin);
                path.build().ok()
            };
            let mut track_points = toolbar::ring_arc(1.0, center, radius);
            track_points.pop();
            if let Some(path) = stroke(track_points, true) {
                window.paint_path(path, rgb(track));
            }
            let arc = toolbar::ring_arc(fraction, center, radius);
            // Round ends: a dot of the line's width on each end of an open arc.
            let caps: Vec<(f32, f32)> = match (arc.first(), arc.last()) {
                (Some(&start), Some(&end)) if fraction < 1.0 => vec![start, end],
                _ => Vec::new(),
            };
            if arc.len() >= 2
                && let Some(path) = stroke(arc, fraction >= 1.0)
            {
                window.paint_path(path, rgb(fill));
            }
            for (x, y) in caps {
                let mut dot = PathBuilder::fill();
                let rim: Vec<_> = toolbar::ring_arc(1.0, (x, y), line / 2.0)
                    .into_iter()
                    .map(|(x, y)| point(px(x), px(y)))
                    .collect();
                dot.add_polygon(&rim, true);
                dot.translate(bounds.origin);
                if let Ok(dot) = dot.build() {
                    window.paint_path(dot, rgb(fill));
                }
            }
        },
    )
    .flex_none()
    .size(ui_text::space(RING))
    .into_any_element()
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
    fn the_meter_leaves_out_the_cost_to_fit() {
        let need = |detail: Detail| match detail {
            Detail::Full => 300.0,
            Detail::NoCost => 200.0,
        };
        assert_eq!(fit(400.0, need), Detail::Full);
        assert_eq!(fit(299.0, need), Detail::NoCost);
        // Narrower still, the least there is (and the row clips it).
        assert_eq!(fit(50.0, need), Detail::NoCost);
    }

    #[test]
    fn the_hint_lists_each_window_with_its_reset() {
        use chrono::{Local, TimeZone};
        let now = Local.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap();
        let saturday = Local.with_ymd_and_hms(2026, 10, 10, 14, 0, 0).unwrap();
        let w = window(
            "seven_day",
            "weekly",
            86.6,
            70.0,
            Some(saturday.timestamp() as u64),
        );
        let five = window("five_hour", "5h", 40.0, 70.0, None);
        assert_eq!(
            chip_details(&[w, five], now.timestamp() as u64),
            "weekly 87% used · resets Sat 14:00; 5h 40% used"
        );
    }
}
