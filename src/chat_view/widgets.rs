//! Small pieces every part of the chat tab draws with: the colors of a theme, buttons, badges.

use std::{cell::Cell, rc::Rc, time::Duration};

use gpui::{
    Animation, AnimationExt, AnyElement, App, Div, ElementId, Pixels, Point, ScrollHandle,
    SharedString, Stateful, div, prelude::*, pulsating_between, px, rgb,
};

use crate::{
    controls::{self, Button},
    icons,
    theme::{self, DiffColors, Palette},
    ui_text,
};

use super::cards::{Badge, Tone};

/// The colors of the theme on screen: its palette, and the terminal's green and red for what
/// a change added, what it removed and what went wrong. `native` is whether the chat is drawn
/// the Native way: capsule controls, SF Symbols and sentence case (see `controls`).
#[derive(Clone, Copy)]
pub(super) struct Look {
    pub colors: Palette,
    pub diff: DiffColors,
    pub native: bool,
}

impl Look {
    pub fn of(cx: &App) -> Self {
        Self {
            colors: theme::palette(cx),
            diff: theme::diff_colors(cx),
            native: ui_text::is_native(),
        }
    }

    /// Something going on speaks in the theme's working color; what went wrong in the
    /// terminal's red, or in Native's one signal color, which it keeps for errors.
    pub fn tone(self, tone: Tone) -> u32 {
        match tone {
            Tone::Muted => self.colors.muted,
            Tone::Accent => self.colors.working,
            Tone::Warning => self.colors.gold,
            Tone::Error if self.native => self.colors.gold,
            Tone::Error => self.diff.removed,
        }
    }

    /// The color of a failure the user should see or an action that stops something.
    pub fn error(self) -> u32 {
        self.tone(Tone::Error)
    }

    /// `color` laid thinly over the panel highlight, for the background of a line or a chip.
    pub fn tint(self, color: u32, amount: f64) -> u32 {
        theme::mix(self.colors.panel_active, color, amount)
    }
}

/// A short label written in lowercase, such as "copy" or "running", as the theme shows it:
/// as it is in the colorful themes, starting with a capital in Native's sentence case.
pub(super) fn sentence(label: &str, look: Look) -> String {
    let mut chars = label.chars();
    match chars.next() {
        Some(first) if look.native => first.to_uppercase().chain(chars).collect(),
        _ => label.to_owned(),
    }
}

/// A small bordered button. The caller adds `.on_click(..)`; `accent` is the color of the
/// primary choice, `None` for an ordinary one. Native draws a capsule instead: filled with
/// the primary color for the primary choice (an `accent` of the palette's `cyan`), grey for
/// every other.
pub(super) fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    accent: Option<u32>,
    look: Look,
) -> Stateful<Div> {
    let colors = look.colors;
    if look.native {
        let kind = if accent == Some(colors.cyan) {
            Button::Primary
        } else {
            Button::Secondary
        };
        return capsule(id, label, kind, look)
            .cursor_pointer()
            .hover(move |style| style.bg(rgb(kind.hover(colors))));
    }
    let ink = accent.unwrap_or(colors.text);
    div()
        .id(id)
        .flex_none()
        .px(ui_text::space(8.0))
        .py(ui_text::space(CAPSULE_PAD_Y))
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
    if look.native {
        return capsule(id, label, Button::Disabled, look);
    }
    div()
        .id(id)
        .flex_none()
        .px(ui_text::space(8.0))
        .py(ui_text::space(CAPSULE_PAD_Y))
        .border_1()
        .border_color(rgb(colors.divider))
        .rounded(px(3.0))
        .text_size(ui_text::text(10.0))
        .text_color(rgb(colors.muted))
        .child(label.into())
}

/// The vertical padding of a `capsule` (and of the colorful themes' `button`) inside its
/// hairline edge, which sets the height of the toolbar's controls; a `segments` track is
/// built to the same height from it.
pub(super) const CAPSULE_PAD_Y: f32 = 3.0;

/// Native's push button at the chat's size, without its hover. Its children line up in a
/// row, so a caller can add a symbol after the label.
pub(super) fn capsule(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    kind: Button,
    look: Look,
) -> Stateful<Div> {
    controls::button(
        div()
            .id(id)
            .flex_none()
            .flex()
            .items_center()
            .gap(ui_text::space(4.0))
            .py(ui_text::space(CAPSULE_PAD_Y))
            .text_size(ui_text::text(10.0))
            .child(label.into()),
        kind,
        look.colors,
    )
}

/// A segmented control's track: one continuous pill in the toolbar's grey control fill,
/// without an edge, that holds its `segment`s side by side with no gap. It adds nothing
/// around them, so it is exactly as tall as a `capsule` beside it.
pub(super) fn segments(look: Look) -> Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .rounded_full()
        .bg(rgb(look.colors.panel_active))
}

/// The colors of a `segment`: its pill's fill (none for a bare label) and label, at rest
/// and under the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SegmentColors {
    fill: Option<u32>,
    ink: u32,
    hover: u32,
    hover_ink: u32,
}

impl SegmentColors {
    fn of(selected: bool, colors: Palette) -> Self {
        if selected {
            Self {
                fill: Some(colors.cyan),
                ink: colors.bg,
                hover: Button::Primary.hover(colors),
                hover_ink: colors.bg,
            }
        } else {
            Self {
                fill: None,
                ink: colors.muted,
                hover: Button::Secondary.hover(colors),
                hover_ink: colors.text,
            }
        }
    }
}

/// One option of a `segments` track, exactly a `capsule`'s height: a clear margin where a
/// capsule has its hairline edge and a point more, and a pill inside it a point less
/// padded. The selected one's pill is filled in the primary color a `button` gives the
/// primary choice; the others are bare labels in the secondary color whose pill fills
/// under the pointer, their label then in the text color as an ordinary button's is. The
/// whole segment, margin included, takes the click, which the caller adds.
pub(super) fn segment(
    id: &'static str,
    label: impl Into<SharedString>,
    selected: bool,
    look: Look,
) -> Stateful<Div> {
    let SegmentColors {
        fill,
        ink,
        hover,
        hover_ink,
    } = SegmentColors::of(selected, look.colors);
    let label: SharedString = label.into();
    let margin = px(2.0);
    div()
        .id(id)
        .group(id)
        .flex_none()
        .p(margin)
        .cursor_pointer()
        .text_size(ui_text::text(10.0))
        .text_color(rgb(ink))
        .child(
            div()
                .relative()
                .flex()
                .items_center()
                .px(ui_text::space(10.0))
                .py(ui_text::space(CAPSULE_PAD_Y) + px(1.0) - margin)
                .rounded_full()
                .when_some(fill, |pill, fill| pill.bg(rgb(fill)))
                .group_hover(id, move |style| style.bg(rgb(hover)))
                // A text's color is fixed when it is laid out, before the pointer is known,
                // so a label whose color changes under the pointer is drawn twice, in both
                // colors, and the pointer only chooses which one shows.
                .map(|pill| {
                    if hover_ink == ink {
                        return pill.child(label);
                    }
                    pill.child(
                        div()
                            .group_hover(id, |style| style.opacity(0.0))
                            .child(label.clone()),
                    )
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(rgb(hover_ink))
                            .opacity(0.0)
                            .group_hover(id, |style| style.opacity(1.0))
                            .child(label),
                    )
                }),
        )
}

/// A bare SF Symbol button with its name in a tooltip, as Native's panel headers have one,
/// for the chat's small actions: copy, the toolbar's menu. The caller adds the click.
pub(super) fn symbol_button(
    id: impl Into<ElementId>,
    symbol: &'static str,
    tooltip: impl Into<SharedString>,
    look: Look,
) -> Stateful<Div> {
    controls::toolbar_button(id, symbol, tooltip, true, look.colors).cursor_pointer()
}

/// Native's round button with an SF Symbol and its name in a tooltip, as a message field's
/// send button is drawn: filled with the primary color for the primary action. A disabled
/// one keeps its place and takes no hover. The caller adds the click.
pub(super) fn round_button(
    id: impl Into<ElementId>,
    symbol: &'static str,
    tooltip: impl Into<SharedString>,
    kind: Button,
    look: Look,
) -> Stateful<Div> {
    let colors = look.colors;
    controls::button(
        div()
            .id(id)
            .flex_none()
            .size(ui_text::space(ROUND_BUTTON))
            .flex()
            .items_center()
            .justify_center()
            .child(icons::symbol(symbol, 11.0, None))
            .child(crate::tooltip::anchor(
                tooltip.into(),
                crate::tooltip::Look::Control,
            )),
        kind,
        colors,
    )
    .px(px(0.0))
    .when(kind != Button::Disabled, |button| {
        button
            .cursor_pointer()
            .hover(move |style| style.bg(rgb(kind.hover(colors))))
    })
}

/// The line box of the chat's 11 px design text at GPUI's default line height (φ), for
/// `controls::on_first_line`: a mark centred on it sits on the text's first line.
pub(super) const BODY_LINE: f32 = 17.8;

/// A round button's side.
pub(super) const ROUND_BUTTON: f32 = 26.0;

/// The design size of the text in an input box, and the space above and below it.
pub(super) const FIELD_TEXT: f32 = 12.0;
pub(super) const FIELD_PAD_Y: f32 = 6.0;

/// The height of an input box of one line: its text's line box (GPUI's default line height,
/// φ times the text size, rounded as GPUI rounds it), its padding and its 1 px border.
pub(super) fn field_line_height() -> Pixels {
    let line = (f32::from(ui_text::text(FIELD_TEXT)) * 1.618_034).round();
    px(line + 2.0 * ui_text::space_f32(FIELD_PAD_Y) + 2.0)
}

/// A button beside the message box, centered on the box's line: on the box's own center while
/// it has one line, and on its last line once it has more, as the row keeps its buttons at the
/// bottom. Every button of the row sits in one of these, so they share one center line whatever
/// their height.
pub(super) fn beside_field(button: impl IntoElement) -> Div {
    div()
        .flex_none()
        .h(field_line_height())
        .flex()
        .items_center()
        .child(button)
}

/// A button that copies something: `word` ("copy"), then "copied" once it has, in the
/// colorful themes; Native's copy symbol, then a tick, named in its tooltip.
pub(super) fn copy_button(
    id: impl Into<ElementId>,
    copied: bool,
    word: &'static str,
    tooltip: &'static str,
    look: Look,
) -> Stateful<Div> {
    if look.native {
        let (symbol, tooltip) = if copied {
            ("checkmark", "Copied")
        } else {
            ("doc.on.doc", tooltip)
        };
        return symbol_button(id, symbol, tooltip, look);
    }
    button(id, if copied { "copied" } else { word }, None, look)
}

/// A status badge; one that stands for something still going on pulses. Native draws it as
/// a grey capsule, as it draws its other read-only states (`controls::chip`).
pub(super) fn badge(id: impl Into<ElementId>, badge: &Badge, look: Look) -> AnyElement {
    let color = look.tone(badge.tone);
    let chip = if look.native {
        div()
            .flex_none()
            .px(ui_text::space(7.0))
            .rounded_full()
            .bg(rgb(look.colors.panel_active))
            .text_size(ui_text::text(9.0))
            .text_color(rgb(color))
            .child(sentence(&badge.label, look))
    } else {
        div()
            .flex_none()
            .px(ui_text::space(6.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(rgb(color))
            .text_size(ui_text::text(9.0))
            .text_color(rgb(color))
            .child(badge.label.clone())
    };
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

/// The arrow at the start of a card that opens and closes; Native's is its chevron symbol.
pub(super) fn chevron(open: bool, expandable: bool, color: u32) -> AnyElement {
    if ui_text::is_native() {
        return div()
            .flex_none()
            .w(ui_text::space(12.0))
            .flex()
            .justify_center()
            .when(expandable, |slot| {
                slot.child(icons::mark(if open { "▾" } else { "▸" }, 9.0, color))
            })
            .into_any_element();
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn look(native: bool) -> Look {
        Look {
            colors: Palette::native(true),
            diff: DiffColors {
                added: 0x00ff00,
                removed: 0xff0000,
            },
            native,
        }
    }

    #[test]
    fn segment_labels_stay_readable_under_the_pointer() {
        let mut palettes = vec![
            ("Native light", Palette::native(false)),
            ("Native dark", Palette::native(true)),
        ];
        for choice in [
            theme::ThemeChoice::RiWork,
            theme::ThemeChoice::Catppuccin,
            theme::ThemeChoice::TokyoNight,
            theme::ThemeChoice::GruvboxLight,
        ] {
            palettes.push((
                choice.label(),
                theme::Appearance::resolve(choice, false).palette,
            ));
        }
        for (name, colors) in palettes {
            let track = colors.panel_active;
            for selected in [false, true] {
                let segment = SegmentColors::of(selected, colors);
                let rest = theme::contrast(segment.ink, segment.fill.unwrap_or(track));
                let hover = theme::contrast(segment.hover_ink, segment.hover);
                eprintln!("{name} selected={selected}: rest {rest:.2}:1, hover {hover:.2}:1");
                assert!(
                    hover >= 4.5,
                    "{name} selected={selected} hovered: {hover:.2}"
                );
            }
        }
    }

    #[test]
    fn lowercase_labels_start_with_a_capital_only_in_native() {
        assert_eq!(sentence("copy output", look(true)), "Copy output");
        assert_eq!(sentence("exit 0", look(true)), "Exit 0");
        assert_eq!(sentence("copy output", look(false)), "copy output");
        assert_eq!(sentence("", look(true)), "");
    }

    #[test]
    fn native_keeps_its_signal_color_for_errors_and_work() {
        let native = look(true);
        assert_eq!(native.tone(Tone::Error), native.colors.gold);
        assert_eq!(native.tone(Tone::Accent), native.colors.working);
        // The colorful themes keep the terminal's red, and their working color is `cyan`.
        let colorful = Look {
            colors: Palette::RIWORK,
            ..look(false)
        };
        assert_eq!(colorful.tone(Tone::Error), 0xff0000);
        assert_eq!(colorful.tone(Tone::Accent), Palette::RIWORK.cyan);
    }
}
