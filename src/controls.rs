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

/// A leading mark (a radio, a check box) beside a block of text: centered on the text's
/// first line, whose line box the caller sets to `line` design points, so the mark lines up
/// with the title however many lines follow it.
pub fn on_first_line(mark: impl IntoElement, line: f32) -> Div {
    div()
        .flex_none()
        .h(ui_text::space(line))
        .flex()
        .items_center()
        .child(mark)
}

/// A card: a soft rounded surface without a border, set on a panel's grey as System
/// Settings sets its groups: white in light mode, black in dark mode, so the grey controls
/// inside keep their contrast in both.
pub fn card<E: Styled>(element: E, colors: Palette) -> E {
    element
        .rounded(radius(CARD_RADIUS))
        .border_0()
        .bg(rgb(colors.bg))
}

// Navigation panels.
//
// Every panel that can stand in the navigation pane (Projects, Files, Worktrees, Tasks,
// Shells, Usage, Schedules, Project settings, Preview and Settings) is laid out alike under
// Native: a `panel` page with a `panel_header` (title, a muted meta line, bare symbol
// buttons), an optional `search_field`, then its content: `list_row`s inset from the
// edges, `section_heading`s over groups, `card`s for forms, an `empty_state` when there is
// nothing to show, and `footnote`s. Text edges line up at `PANEL_INSET` from the panel's
// edge throughout: a row sits `LIST_MARGIN` in and pads its text by the rest.

/// The distance from a panel's edge to its text: header, headings, notes and row labels.
pub const PANEL_INSET: f32 = 12.0;
/// The distance from a panel's edge to its rows' fill and to its search field.
pub const LIST_MARGIN: f32 = 6.0;
/// A list row's corner radius, as a Finder or Xcode sidebar rounds its selection.
pub const LIST_ROW_RADIUS: f32 = 7.0;
/// A panel header's height: a title over a meta line.
pub const HEADER_HEIGHT: f32 = 46.0;
/// A panel title's design size; its meta line and section headings are `META_TEXT`.
pub const TITLE_TEXT: f32 = 13.0;
pub const META_TEXT: f32 = 10.0;
/// A header button's round hit target, and its symbol's design size.
const TOOLBAR_BUTTON: f32 = 24.0;
const TOOLBAR_SYMBOL: f32 = 11.0;

/// A navigation panel's page: the sidebar grey, body text in the interface face.
pub fn panel(colors: Palette) -> Div {
    div()
        .size_full()
        .min_w_0()
        .min_h_0()
        .flex()
        .flex_col()
        .bg(rgb(colors.panel))
        .text_color(rgb(colors.text))
        .font_family(ui_text::ui_family())
        .text_size(ui_text::text(11.0))
}

/// A panel's header: its title in semibold over a muted meta line (a count, a path, a
/// state), and its actions as bare symbol buttons on the right. The header keeps its height
/// whether or not there is a meta line, so switching panels never moves the content.
pub fn panel_header(
    title: impl Into<gpui::SharedString>,
    meta: Option<gpui::SharedString>,
    actions: impl IntoIterator<Item = AnyElement>,
    colors: Palette,
) -> Div {
    let line = |element: Div| {
        element
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
    };
    div()
        .flex_none()
        .h(ui_text::space(HEADER_HEIGHT))
        .pl(ui_text::space(PANEL_INSET))
        .pr(ui_text::space(PANEL_INSET - 4.0))
        .flex()
        .items_center()
        .gap(ui_text::space(8.0))
        .font_family(ui_text::ui_family())
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(ui_text::space(1.0))
                .child(
                    line(div())
                        .text_size(ui_text::text(TITLE_TEXT))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(rgb(colors.text))
                        .child(title.into()),
                )
                .children(meta.map(|meta| {
                    line(div())
                        .text_size(ui_text::text(META_TEXT))
                        .text_color(rgb(colors.muted))
                        .child(meta)
                })),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(ui_text::space(2.0))
                .children(actions),
        )
}

/// A header button: a bare SF Symbol, muted, in a round hover that brings it to the text
/// color, with its name in a tooltip. A disabled one stays muted and says why in the
/// tooltip. The caller adds the click.
pub fn toolbar_button(
    id: impl Into<gpui::ElementId>,
    symbol: &'static str,
    tooltip: impl Into<gpui::SharedString>,
    enabled: bool,
    colors: Palette,
) -> gpui::Stateful<Div> {
    let rest = if enabled {
        colors.muted
    } else {
        theme::mix(colors.muted, colors.panel, 0.45)
    };
    div()
        .id(id)
        .flex_none()
        .size(ui_text::space(TOOLBAR_BUTTON))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .text_color(rgb(rest))
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .hover(move |style| style.bg(rgb(colors.divider)).text_color(rgb(colors.text)))
        })
        .child(crate::icons::symbol(symbol, TOOLBAR_SYMBOL, None))
        .child(crate::tooltip::anchor(
            tooltip.into(),
            crate::tooltip::Look::Control,
        ))
}

/// A toolbar button that is on, such as a filter in use: drawn in the text color on a
/// quiet fill, as a toggled toolbar item is.
pub fn toolbar_button_on(button: gpui::Stateful<Div>, colors: Palette) -> gpui::Stateful<Div> {
    button
        .bg(rgb(colors.panel_active))
        .text_color(rgb(colors.text))
}

/// A search or filter field: a grey capsule with a magnifier, inset like the rows below.
/// The caller puts the text in it.
pub fn search_field<E: Styled>(element: E, focused: bool, colors: Palette) -> E {
    element
        .flex_none()
        .h(ui_text::space(24.0))
        .mx(ui_text::space(LIST_MARGIN))
        .mb(ui_text::space(6.0))
        .px(ui_text::space(8.0))
        .flex()
        .items_center()
        .gap(ui_text::space(5.0))
        .rounded_full()
        .border_1()
        .border_color(if focused {
            rgb(colors.focus).into()
        } else {
            transparent_black()
        })
        .bg(rgb(colors.panel_active))
        .text_color(rgb(if focused { colors.text } else { colors.muted }))
        .font_family(ui_text::ui_family())
        .text_size(ui_text::text(11.0))
}

/// A group's heading inside a panel: small, semibold and muted, as a Finder sidebar heads
/// its sections, inset like the header's title.
pub fn section_heading(label: impl Into<gpui::SharedString>, colors: Palette) -> Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(ui_text::space(6.0))
        .px(ui_text::space(PANEL_INSET))
        .pt(ui_text::space(10.0))
        .pb(ui_text::space(4.0))
        .text_size(ui_text::text(META_TEXT))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(rgb(colors.muted))
        .child(label.into())
}

/// A navigation list's row: rounded, filled when selected and clear otherwise, its text
/// padded so it lines up with the header's. Put it in `list_inset` to keep it off the edges.
pub fn list_row<E: Styled + InteractiveElement>(element: E, selected: bool, colors: Palette) -> E {
    row(element, selected, colors)
        .rounded(radius(LIST_ROW_RADIUS))
        .px(ui_text::space(PANEL_INSET - LIST_MARGIN))
}

/// The box that insets a `list_row` from the panel's edges. A margin on a full-width row
/// would push it past the right edge, so a padded box holds it instead.
pub fn list_inset(row: impl IntoElement) -> Div {
    div()
        .w_full()
        .min_w_0()
        .px(ui_text::space(LIST_MARGIN))
        .child(row)
}

/// A list row's title line and its muted detail lines, the sizes every navigation list
/// uses.
pub const ROW_TITLE_TEXT: f32 = 11.0;
pub const ROW_DETAIL_TEXT: f32 = 10.0;

/// What a panel shows when there is nothing in it: a muted symbol over a short line,
/// centered in the room left.
pub fn empty_state(
    symbol: &'static str,
    text: impl Into<gpui::SharedString>,
    colors: Palette,
) -> Div {
    div()
        .flex_1()
        .min_h(ui_text::space(120.0))
        .w_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(ui_text::space(8.0))
        .px(ui_text::space(PANEL_INSET * 2.0))
        .py(ui_text::space(24.0))
        .text_color(rgb(colors.muted))
        .child(crate::icons::symbol(
            symbol,
            22.0,
            Some(theme::mix(colors.muted, colors.panel, 0.25)),
        ))
        .child(
            div()
                .max_w(ui_text::space(240.0))
                .text_center()
                .text_size(ui_text::text(11.0))
                .child(text.into()),
        )
}

/// A panel's fine print: a muted footnote inset like the rest of its text.
pub fn footnote(text: impl Into<gpui::SharedString>, colors: Palette) -> Div {
    div()
        .flex_none()
        .px(ui_text::space(PANEL_INSET))
        .py(ui_text::space(8.0))
        .text_size(ui_text::text(9.0))
        .text_color(rgb(colors.muted))
        .child(text.into())
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

/// A menu's corner radius, and its rows', concentric with it at the menu's padding.
pub const MENU_RADIUS: f32 = 10.0;
pub const MENU_ROW_RADIUS: f32 = 6.0;
const MENU_PADDING: f32 = 4.0;

/// A popover menu, as macOS draws one: a raised rounded panel with a hairline edge and a
/// soft shadow. Its rows are styled with `menu_row`.
pub fn menu<E: Styled>(element: E, colors: Palette) -> E {
    element
        .rounded(radius(MENU_RADIUS))
        .border_1()
        .border_color(rgb(colors.divider))
        .bg(rgb(raised(colors)))
        .shadow_lg()
        .p(ui_text::space(MENU_PADDING))
}

/// A menu row: body text, inset from the menu's edge, highlighted by a rounded fill.
pub fn menu_row<E: Styled>(element: E, colors: Palette) -> E {
    element
        .rounded(radius(MENU_ROW_RADIUS))
        .min_h(ui_text::space(22.0))
        .px(ui_text::space(8.0))
        .py(ui_text::space(3.0))
        .text_size(ui_text::text(11.0))
        .text_color(rgb(colors.text))
}

/// The fill of a menu row under the pointer, for `hovered`: a step from the menu's surface.
pub fn menu_row_hover(colors: Palette) -> u32 {
    theme::mix(raised(colors), colors.text, 0.09)
}

/// A hairline between a menu's groups, inset like its rows' text.
pub fn menu_separator(colors: Palette) -> AnyElement {
    div()
        .h(gpui::px(1.0))
        .mx(ui_text::space(8.0))
        .my(ui_text::space(4.0))
        .bg(rgb(colors.divider))
        .into_any_element()
}

/// A group's heading in a menu: small, muted and semibold, over its rows.
pub fn menu_heading(label: impl Into<gpui::SharedString>, colors: Palette) -> AnyElement {
    div()
        .px(ui_text::space(8.0))
        .pt(ui_text::space(3.0))
        .pb(ui_text::space(2.0))
        .text_size(ui_text::text(9.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(rgb(colors.muted))
        .child(label.into())
        .into_any_element()
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
