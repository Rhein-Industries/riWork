//! Small pieces every part of the chat tab draws with: the colors of a theme, buttons, badges.

use std::{cell::Cell, rc::Rc, time::Duration};

use gpui::{
    Animation, AnimationExt, AnyElement, App, Div, ElementId, Pixels, Point, ScrollHandle,
    SharedString, div, prelude::*, pulsating_between, px, rgb,
};

use crate::{
    behavior_controls as behavior,
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
    /// Whether the chat is drawn in the Hermes design's style: proportional text, flat panel
    /// cards with small corners, a filled send disc. It chooses how things look only; every
    /// design lays the chat out the same.
    pub fn hermes(self) -> bool {
        self.colors == Palette::HERMES
    }

    /// The corners of the cards at the bottom of the chat (the message box and the bars above
    /// it): Hermes's small ones, the continuous-corner message field's in every other design.
    pub fn card_radius(self) -> Pixels {
        ui_text::space(if self.hermes() { 5.0 } else { 16.0 })
    }

    pub fn chat_family(self) -> SharedString {
        if self.hermes() {
            ".SystemUIFont".into()
        } else {
            ui_text::ui_family()
        }
    }

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
) -> behavior::Button {
    let label = label.into();
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
    behavior::button_content(id, label.clone(), label)
        .line_height(gpui::relative(1.618_034))
        .focus_visible(move |style| style.border_color(rgb(colors.focus)))
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
}

/// A button that cannot be pressed now.
pub(super) fn dimmed(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    look: Look,
) -> behavior::Button {
    let label = label.into();
    let colors = look.colors;
    if look.native {
        return capsule(id, label, Button::Disabled, look);
    }
    behavior::button_content(id, label.clone(), label)
        .line_height(gpui::relative(1.618_034))
        .disabled(true)
        .flex_none()
        .px(ui_text::space(8.0))
        .py(ui_text::space(CAPSULE_PAD_Y))
        .border_1()
        .border_color(rgb(colors.divider))
        .rounded(px(3.0))
        .text_size(ui_text::text(10.0))
        .text_color(rgb(colors.muted))
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
) -> behavior::Button {
    let label = label.into();
    controls::button(
        behavior::button_content(id, label.clone(), label)
            .line_height(gpui::relative(1.618_034))
            .disabled(kind == Button::Disabled)
            .flex_none()
            .flex()
            .items_center()
            .gap(ui_text::space(4.0))
            .py(ui_text::space(CAPSULE_PAD_Y))
            .text_size(ui_text::text(10.0)),
        kind,
        look.colors,
    )
}

/// A segmented control's track: one continuous pill in the toolbar's grey control fill,
/// without an edge, that holds its `segment`s side by side with no gap. It adds nothing
/// around them, so it is exactly as tall as a `capsule` beside it. It is the Kit's toggle
/// group, which names the set for VoiceOver as `name`.
pub(super) fn segments(
    id: impl Into<ElementId>,
    name: impl Into<SharedString>,
    look: Look,
) -> behavior::Segments {
    behavior::Segments::new(id)
        .aria_label(name.into())
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
                ink: if theme::contrast(colors.muted, colors.panel_active) >= 4.5 {
                    colors.muted
                } else {
                    colors.text
                },
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
/// whole segment, margin included, is the Kit's toggle, pressed while `selected`: it takes
/// the click and the keyboard, which the caller handles with `on_change`, and its margin's
/// outer point is the focus ring, as a capsule's edge is.
pub(super) fn segment(
    id: &'static str,
    label: impl Into<SharedString>,
    selected: bool,
    look: Look,
) -> behavior::Toggle {
    let SegmentColors {
        fill,
        ink,
        hover,
        hover_ink,
    } = SegmentColors::of(selected, look.colors);
    let colors = look.colors;
    let label: SharedString = label.into();
    let margin = px(2.0);
    let pill = div()
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
                return pill.child(label.clone());
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
                    .child(label.clone()),
            )
        });
    behavior::toggle_content(id, label.clone(), pill, selected)
        .group(id)
        .line_height(gpui::relative(1.618_034))
        .flex_none()
        .rounded_full()
        // The margin: a hairline that shows only as the keyboard focus ring, and the rest.
        .border_1()
        .border_color(gpui::transparent_black())
        .focus_visible(move |style| style.border_color(rgb(colors.focus)))
        .p(margin - px(1.0))
        .cursor_pointer()
        .text_size(ui_text::text(10.0))
        .text_color(rgb(ink))
}

/// A bare SF Symbol button with its name in a tooltip, as Native's panel headers have one,
/// for the chat's small actions: copy, the toolbar's menu. The caller adds the click.
pub(super) fn symbol_button(
    id: impl Into<ElementId>,
    symbol: &'static str,
    tooltip: impl Into<SharedString>,
    look: Look,
) -> behavior::Button {
    symbol_button_sized(
        id,
        symbol,
        tooltip,
        look,
        ui_text::space(controls::TOOLBAR_BUTTON),
    )
}

pub(super) fn symbol_button_sized(
    id: impl Into<ElementId>,
    symbol: &'static str,
    tooltip: impl Into<SharedString>,
    look: Look,
    side: Pixels,
) -> behavior::Button {
    let name = tooltip.into();
    bare_symbol(
        behavior::button_content(
            id,
            name.clone(),
            icons::symbol_in_box(
                symbol,
                controls::TOOLBAR_SYMBOL,
                None,
                (side - px(4.0)).max(px(1.0)),
            ),
        ),
        name,
        look,
    )
    .size(side)
}

/// A `symbol_button` that stays pressed while `pressed`, such as the message box's mic.
/// The caller adds `on_change`.
pub(super) fn symbol_toggle(
    id: impl Into<ElementId>,
    symbol: &'static str,
    tooltip: impl Into<SharedString>,
    pressed: bool,
    look: Look,
) -> behavior::Toggle {
    symbol_toggle_sized(
        id,
        symbol,
        tooltip,
        pressed,
        look,
        ui_text::space(controls::TOOLBAR_BUTTON),
    )
}

pub(super) fn symbol_toggle_sized(
    id: impl Into<ElementId>,
    symbol: &'static str,
    tooltip: impl Into<SharedString>,
    pressed: bool,
    look: Look,
    side: Pixels,
) -> behavior::Toggle {
    let name = tooltip.into();
    bare_symbol(
        behavior::toggle_content(
            id,
            name.clone(),
            icons::symbol_in_box(
                symbol,
                controls::TOOLBAR_SYMBOL,
                None,
                (side - px(4.0)).max(px(1.0)),
            ),
            pressed,
        ),
        name,
        look,
    )
    .size(side)
}

/// A `symbol_button`'s look: a bare symbol in a round hit area that fills under the pointer
/// and with keyboard focus, its name in a tooltip.
fn bare_symbol<E: Styled + InteractiveElement + ParentElement>(
    control: E,
    name: SharedString,
    look: Look,
) -> E {
    let colors = look.colors;
    control
        .line_height(gpui::relative(1.618_034))
        .flex_none()
        .size(ui_text::space(controls::TOOLBAR_BUTTON))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .text_color(rgb(colors.muted))
        .cursor_pointer()
        .hover(move |style| style.bg(rgb(colors.divider)).text_color(rgb(colors.text)))
        .focus_visible(move |style| style.bg(rgb(colors.divider)).text_color(rgb(colors.focus)))
        .child(crate::tooltip::anchor(name, crate::tooltip::Look::Control))
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
) -> behavior::Button {
    round_button_sized(
        id,
        symbol,
        tooltip,
        kind,
        look,
        ui_text::space(ROUND_BUTTON),
    )
}

pub(super) fn round_button_sized(
    id: impl Into<ElementId>,
    symbol: &'static str,
    tooltip: impl Into<SharedString>,
    kind: Button,
    look: Look,
    side: Pixels,
) -> behavior::Button {
    let colors = look.colors;
    let name = tooltip.into();
    controls::button(
        behavior::button_content(
            id,
            name.clone(),
            icons::symbol_in_box(
                symbol,
                controls::TOOLBAR_SYMBOL,
                None,
                (side - px(4.0)).max(px(1.0)),
            ),
        )
        .line_height(gpui::relative(1.618_034))
        .disabled(kind == Button::Disabled)
        .flex_none()
        .size(side)
        .flex()
        .items_center()
        .justify_center()
        .child(crate::tooltip::anchor(name, crate::tooltip::Look::Control)),
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

/// The design size of the text in an input box.
pub(super) const FIELD_TEXT: f32 = 12.0;

/// The line box of an input box's text (GPUI's default line height, φ times the text size,
/// rounded as GPUI rounds it), which the box sets explicitly.
pub(super) fn field_line() -> Pixels {
    px((f32::from(ui_text::text(FIELD_TEXT)) * 1.618_034).round())
}

/// A button that copies something: `word` ("copy"), then "copied" once it has, in the
/// colorful themes; Native's copy symbol, then a tick, named in its tooltip.
pub(super) fn copy_button(
    id: impl Into<ElementId>,
    copied: bool,
    word: &'static str,
    tooltip: &'static str,
    look: Look,
) -> behavior::Button {
    if look.native {
        let (symbol, tooltip) = if copied {
            ("checkmark", "Copied")
        } else {
            ("doc.on.doc", tooltip)
        };
        return symbol_button(id, symbol, tooltip, look);
    }
    button(id, if copied { "copied" } else { word }, None, look).accessibility_label(if copied {
        "Copied"
    } else {
        tooltip
    })
}

/// Controlled Base toggle with the chat's existing button treatment.
pub(super) fn toggle_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    accent: Option<u32>,
    pressed: bool,
    look: Look,
) -> behavior::Toggle {
    toggle_button_with_disabled(id, label, accent, pressed, false, look)
}

pub(super) fn toggle_button_with_disabled(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    accent: Option<u32>,
    pressed: bool,
    disabled: bool,
    look: Look,
) -> behavior::Toggle {
    let label = label.into();
    let colors = look.colors;
    let toggle = behavior::toggle_content(id, label.clone(), label, pressed)
        .disabled(disabled)
        .line_height(gpui::relative(1.618_034))
        .flex_none()
        .text_size(ui_text::text(10.));
    if look.native {
        let kind = if accent == Some(colors.cyan) {
            Button::Primary
        } else {
            Button::Secondary
        };
        controls::button(
            toggle
                .flex()
                .items_center()
                .gap(ui_text::space(4.))
                .py(ui_text::space(CAPSULE_PAD_Y)),
            kind,
            colors,
        )
        .when(!disabled, |toggle| {
            toggle
                .cursor_pointer()
                .hover(move |style| style.bg(rgb(kind.hover(colors))))
        })
    } else {
        toggle
            .focus_visible(move |style| style.border_color(rgb(colors.focus)))
            .px(ui_text::space(8.))
            .py(ui_text::space(CAPSULE_PAD_Y))
            .border_1()
            .border_color(rgb(accent.unwrap_or(colors.divider)))
            .rounded(px(3.))
            .bg(rgb(colors.panel))
            .text_color(rgb(accent.unwrap_or(colors.text)))
            .when(!disabled, |toggle| {
                toggle
                    .cursor_pointer()
                    .hover(move |style| style.bg(rgb(colors.panel_active)))
            })
    }
}

/// Composite content retains its exact layout; the shared Base primitive is
/// the sole activation/focus/AX owner and adds no visible label or glyph.
pub(super) fn content_button(
    id: impl Into<ElementId>,
    name: impl Into<SharedString>,
    content: impl IntoElement,
    look: Look,
) -> behavior::Button {
    let colors = look.colors;
    behavior::button_content(id, name, content)
        .w_full()
        .gap_0()
        .p_0()
        .items_stretch()
        .line_height(gpui::relative(1.618_034))
        .focus_visible(move |style| style.bg(rgb(colors.divider)))
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
