//! The Native theme's controls, shaped like the current macOS ones: capsule buttons,
//! switches and segmented controls, rounded cards whose rows round concentrically inside
//! them, and selection drawn as a fill instead of an outline.
//!
//! The colorful themes keep RiWork's square, outlined controls, so these are applied over
//! an element's existing style only under Native: `native(el, |el| controls::button(…))`.
//! Each helper replaces the corners, fill, border, text color and keyboard focus styles,
//! which are the parts of a style that a later call overrides; layout is the caller's.
//! A hover style can be set only once on an element, so the caller's own `hover` asks
//! `hovered` for Native's fill instead. Colors are Native's palette tokens: the primary
//! (`cyan`) is black or white, and nothing here uses the signal color.

use gpui::{
    AnyElement, Div, InteractiveElement, IntoElement, Pixels, StyleRefinement, Styled, div,
    prelude::*, rgb, transparent_black,
};

use crate::{
    theme::{self, Palette},
    ui_text,
};

/// A card's corner radius: Settings sections, dialogs and menus.
pub const CARD_RADIUS: f32 = 14.0;
/// A row inside a card, concentric with the card at its padding.
pub const ROW_RADIUS: f32 = 10.0;
/// A small field or swatch.
pub const FIELD_RADIUS: f32 = 7.0;

/// The radius for a design length, grown with the text like the spacing around it.
pub fn radius(base: f32) -> Pixels {
    ui_text::space(base)
}

/// Apply `native` to `element` when the interface is drawn in the Native theme.
pub fn native<E>(element: E, native: impl FnOnce(E) -> E) -> E {
    if ui_text::is_native() {
        native(element)
    } else {
        element
    }
}

/// A surface raised above a control's track: a segmented control's selected segment and a
/// switch's knob. White in light mode; in dark mode a grey a step lighter than the track.
pub fn raised(colors: Palette) -> u32 {
    if theme::is_dark(colors.bg) {
        theme::mix(colors.panel_active, colors.text, 0.16)
    } else {
        colors.bg
    }
}

/// A hover style: Native's `fill` under Native, the colorful themes' own otherwise.
pub fn hovered(
    style: StyleRefinement,
    fill: u32,
    legacy: impl FnOnce(StyleRefinement) -> StyleRefinement,
) -> StyleRefinement {
    if ui_text::is_native() {
        style.bg(rgb(fill))
    } else {
        legacy(style)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    /// The one action a group leads with: filled with the primary color.
    Primary,
    /// Every other action: a quiet grey capsule.
    Secondary,
    /// An action that cannot run now.
    Disabled,
}

impl Button {
    /// Fill, text and hover fill.
    fn colors(self, colors: Palette) -> (u32, u32, u32) {
        match self {
            Self::Primary => (colors.cyan, colors.bg, colors.magenta),
            Self::Secondary => (colors.panel_active, colors.text, colors.divider),
            Self::Disabled => (colors.panel_active, colors.muted, colors.panel_active),
        }
    }

    /// The fill under the pointer, for `hovered`.
    pub fn hover(self, colors: Palette) -> u32 {
        self.colors(colors).2
    }
}

/// A capsule push button.
pub fn button<E: Styled + InteractiveElement>(element: E, kind: Button, colors: Palette) -> E {
    let (fill, text, _) = kind.colors(colors);
    focus_ring(
        element
            .rounded_full()
            .px(ui_text::space(12.0))
            .bg(rgb(fill))
            .text_color(rgb(text)),
        colors,
    )
}

/// A hairline that is invisible until the element has keyboard focus.
fn focus_ring<E: Styled + InteractiveElement>(element: E, colors: Palette) -> E {
    element
        .border_1()
        .border_color(transparent_black())
        .focus_visible(move |style| style.border_color(rgb(colors.focus)))
}

/// A small read-only status, such as "Connected" or "Active": a grey capsule. Its text is
/// muted, or the signal color for a state that must stand out (`warning`).
pub fn chip(label: impl Into<gpui::SharedString>, warning: bool, colors: Palette) -> AnyElement {
    div()
        .flex_none()
        .px(ui_text::space(8.0))
        .py(ui_text::space(2.0))
        .rounded_full()
        .bg(rgb(colors.panel_active))
        .text_size(ui_text::text(9.0))
        .text_color(rgb(if warning { colors.gold } else { colors.muted }))
        .child(label.into())
        .into_any_element()
}

/// An on/off switch: a capsule track, black (white in dark mode) when on, with a round knob.
pub fn switch(on: bool, colors: Palette) -> AnyElement {
    let width = ui_text::space(30.0);
    let height = ui_text::space(17.0);
    let inset = ui_text::space(2.0);
    let knob = height - inset * 2.0;
    div()
        .flex_none()
        .w(width)
        .h(height)
        .rounded_full()
        .bg(rgb(if on { colors.cyan } else { colors.divider }))
        .flex()
        .items_center()
        .px(inset)
        .when(on, |track| track.justify_end())
        .child(
            div()
                .size(knob)
                .rounded_full()
                .bg(rgb(if on { colors.bg } else { raised(colors) })),
        )
        .into_any_element()
}

/// A segmented control's track. Its segments are styled with `segment`.
pub fn segments(colors: Palette) -> Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .p(ui_text::space(2.0))
        .gap(ui_text::space(2.0))
        .rounded_full()
        .bg(rgb(colors.panel_active))
}

/// The fill of a segment under the pointer, for `hovered`.
pub fn segment_hover(selected: bool, colors: Palette) -> u32 {
    if selected {
        raised(colors)
    } else {
        colors.divider
    }
}

/// One segment: the selected one is raised out of the track.
pub fn segment<E: Styled + InteractiveElement>(element: E, selected: bool, colors: Palette) -> E {
    let fill = if selected {
        raised(colors)
    } else {
        colors.panel_active
    };
    let segment = element
        .rounded_full()
        .px(ui_text::space(10.0))
        .py(ui_text::space(3.0))
        .border_0()
        .bg(rgb(fill))
        .text_color(rgb(if selected { colors.text } else { colors.muted }));
    if selected {
        segment.shadow_sm()
    } else {
        segment
    }
}

/// A card: a soft rounded surface without a border.
pub fn card<E: Styled>(element: E, colors: Palette) -> E {
    element
        .rounded(radius(CARD_RADIUS))
        .border_0()
        .bg(rgb(colors.panel))
}

/// A list row in a card or sidebar. A selected row is filled; others are clear until hovered.
pub fn row<E: Styled + InteractiveElement>(element: E, selected: bool, colors: Palette) -> E {
    let fill = if selected {
        rgb(colors.panel_active).into()
    } else {
        transparent_black()
    };
    focus_ring(element.rounded(radius(ROW_RADIUS)).bg(fill), colors)
}

/// The fill of a row under the pointer, for `hovered`.
pub fn row_hover(selected: bool, colors: Palette) -> u32 {
    if selected {
        colors.panel_active
    } else {
        theme::mix(colors.panel, colors.panel_active, 0.6)
    }
}

/// A text field: a rounded well with a hairline.
pub fn field<E: Styled>(element: E, colors: Palette) -> E {
    element
        .rounded(radius(FIELD_RADIUS))
        .border_1()
        .border_color(rgb(colors.divider))
        .bg(rgb(colors.bg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raised_surfaces_stand_out_from_their_track_in_both_modes() {
        for dark in [false, true] {
            let colors = Palette::native(dark);
            let raised = raised(colors);
            assert_ne!(raised, colors.panel_active);
            if dark {
                assert!(theme::luminance(raised) > theme::luminance(colors.panel_active));
            } else {
                assert_eq!(raised, colors.bg);
            }
        }
    }

    #[test]
    fn controls_apply_only_under_native() {
        // Off the UI thread the face is Menlo, as every colorful theme draws.
        assert_eq!(native(1, |value| value + 1), 1);
    }
}
