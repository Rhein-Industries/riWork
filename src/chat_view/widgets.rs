//! Small pieces every part of the chat tab draws with: the colors of a theme, buttons, badges.

use std::{cell::Cell, rc::Rc, time::Duration};

use gpui::{
    Animation, AnimationExt, AnyElement, App, Div, ElementId, Pixels, Point, ScrollHandle,
    SharedString, Stateful, div, prelude::*, pulsating_between, px, rgb,
};

use crate::{
    theme::{self, DiffColors, Palette},
    ui_text,
};

use super::cards::{Badge, Tone};

/// The colors of the theme on screen: its palette, and the terminal's green and red for what
/// a change added, what it removed and what went wrong.
#[derive(Clone, Copy)]
pub(super) struct Look {
    pub colors: Palette,
    pub diff: DiffColors,
}

impl Look {
    pub fn of(cx: &App) -> Self {
        Self {
            colors: theme::palette(cx),
            diff: theme::diff_colors(cx),
        }
    }

    pub fn tone(self, tone: Tone) -> u32 {
        match tone {
            Tone::Muted => self.colors.muted,
            Tone::Accent => self.colors.cyan,
            Tone::Warning => self.colors.gold,
            Tone::Error => self.diff.removed,
        }
    }

    /// `color` laid thinly over the panel highlight, for the background of a line or a chip.
    pub fn tint(self, color: u32, amount: f64) -> u32 {
        theme::mix(self.colors.panel_active, color, amount)
    }
}

/// A small bordered button. The caller adds `.on_click(..)`; `accent` is the color of the
/// primary choice, `None` for an ordinary one.
pub(super) fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    accent: Option<u32>,
    look: Look,
) -> Stateful<Div> {
    let colors = look.colors;
    let ink = accent.unwrap_or(colors.text);
    div()
        .id(id)
        .flex_none()
        .px(ui_text::space(8.0))
        .py(ui_text::space(3.0))
        .border_1()
        .border_color(rgb(accent.unwrap_or(colors.divider)))
        .rounded(px(3.0))
        .bg(rgb(colors.panel))
        .text_size(ui_text::text(10.0))
        .text_color(rgb(ink))
        .cursor_pointer()
        .hover(|style| style.bg(rgb(colors.panel_active)))
        .child(label.into())
}

/// A button that cannot be pressed now.
pub(super) fn dimmed(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    look: Look,
) -> Stateful<Div> {
    let colors = look.colors;
    div()
        .id(id)
        .flex_none()
        .px(ui_text::space(8.0))
        .py(ui_text::space(3.0))
        .border_1()
        .border_color(rgb(colors.divider))
        .rounded(px(3.0))
        .text_size(ui_text::text(10.0))
        .text_color(rgb(colors.muted))
        .child(label.into())
}

/// A status badge; one that stands for something still going on pulses.
pub(super) fn badge(id: impl Into<ElementId>, badge: &Badge, look: Look) -> AnyElement {
    let color = look.tone(badge.tone);
    let chip = div()
        .flex_none()
        .px(ui_text::space(6.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(rgb(color))
        .text_size(ui_text::text(9.0))
        .text_color(rgb(color))
        .child(badge.label.clone());
    if badge.live {
        chip.with_animation(
            id,
            Animation::new(Duration::from_millis(1400))
                .repeat()
                .with_easing(pulsating_between(0.4, 1.0))
                .with_max_fps(20.0),
            |chip, level| chip.opacity(level),
        )
        .into_any_element()
    } else {
        chip.into_any_element()
    }
}

/// A dot that pulses while something is going on.
pub(super) fn pulse(id: impl Into<ElementId>, color: u32) -> AnyElement {
    div()
        .flex_none()
        .size(ui_text::space(7.0))
        .rounded(px(4.0))
        .bg(rgb(color))
        .with_animation(
            id,
            Animation::new(Duration::from_millis(1200))
                .repeat()
                .with_easing(pulsating_between(0.25, 1.0))
                .with_max_fps(20.0),
            |dot, level| dot.opacity(level),
        )
        .into_any_element()
}

/// The arrow at the start of a card that opens and closes.
pub(super) fn chevron(open: bool, expandable: bool, color: u32) -> AnyElement {
    div()
        .flex_none()
        .w(ui_text::space(10.0))
        .text_size(ui_text::text(9.0))
        .text_color(rgb(color))
        .child(match (expandable, open) {
            (false, _) => " ",
            (true, false) => "▸",
            (true, true) => "▾",
        })
        .into_any_element()
}

/// A box that scrolls inside the transcript. GPUI lets a wheel turn scroll every box under
/// the pointer, so the transcript would move with the box; here the box keeps the turn that
/// moved it, and the transcript gets the ones that find the box at its end.
#[derive(Clone)]
pub(super) struct Scroller {
    handle: ScrollHandle,
    /// Where the box was after the last wheel turn it saw, or when it was drawn.
    seen: Rc<Cell<Point<Pixels>>>,
}

impl Scroller {
    pub fn new() -> Self {
        Self {
            handle: ScrollHandle::new(),
            seen: Rc::new(Cell::new(Point::default())),
        }
    }

    /// `element` scrolls with this handle and stops a wheel turn that moved it.
    pub fn attach<E: StatefulInteractiveElement>(&self, element: E) -> E {
        self.seen.set(self.handle.offset());
        let (handle, seen) = (self.handle.clone(), self.seen.clone());
        element
            .track_scroll(&self.handle)
            // This runs after the box's own handling of the same turn.
            .on_scroll_wheel(move |_, _, cx| {
                let now = handle.offset();
                if seen.replace(now) != now {
                    cx.stop_propagation();
                }
            })
    }
}
