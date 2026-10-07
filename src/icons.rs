//! Small vector controls that keep pane chrome consistent across fonts and themes.

use gpui::{
    AnyElement, App, Bounds, Corners, IntoElement, PathBuilder, canvas, div, point, prelude::*, px,
    rgb,
};

use crate::{
    chat::model::Provider,
    layouts::PanelKind,
    settings::Settings,
    symbols::{self, Weight},
    ui_text,
};

#[derive(Clone, Copy, Debug)]
pub enum Icon {
    Lock,
    Unlock,
    Focus,
    Add,
    SplitRight,
    SplitDown,
    Close,
    More,
    Bell,
    BellOff,
    /// A star: the main pane, where new tabs and opened panels go.
    Main,
    /// A tick: the menu row beside it is on.
    Check,
    /// A window with a navigation strip down its left side: the layout menu.
    Layout,
    /// The symbol a built-in panel tab shows instead of its label.
    Panel(PanelKind),
    /// The symbol a toolbar button shows instead of its label.
    Action(ActionGlyph),
    /// The mark of the agent behind a chat tab, shown instead of its name.
    Provider(Provider),
}

/// Text buttons that can be drawn as a glyph, with the label kept for the tooltip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionGlyph {
    NewFolder,
    NewProject,
    EditInVim,
    CopyPath,
    CopyContents,
    Reveal,
    OpenExternally,
}

/// Whether text buttons that have a glyph show it instead of their label. This is
/// the `panel_tab_icons` setting, which now covers toolbar buttons as well as tabs.
pub fn labels_as_icons(cx: &App) -> bool {
    cx.try_global::<Settings>()
        .is_some_and(|settings| settings.panel_tab_icons)
}

impl Icon {
    /// The SF Symbol Native draws for this icon.
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Lock => "lock",
            Self::Unlock => "lock.open",
            Self::Focus => "arrow.up.left.and.arrow.down.right",
            Self::Add => "plus",
            Self::SplitRight => "rectangle.split.2x1",
            Self::SplitDown => "rectangle.split.1x2",
            Self::Close => "xmark",
            Self::More => "ellipsis",
            Self::Bell => "bell",
            Self::BellOff => "bell.slash",
            Self::Main => "star",
            Self::Check => "checkmark",
            Self::Layout => "sidebar.left",
            Self::Panel(panel) => match panel {
                PanelKind::Projects => "rectangle.stack",
                PanelKind::Worktrees => "arrow.triangle.branch",
                PanelKind::Files => "folder",
                PanelKind::Preview => "eye",
                PanelKind::Tasks => "checklist",
                PanelKind::Shells => "terminal",
                PanelKind::Usage => "chart.bar",
                PanelKind::Settings => "gearshape",
                PanelKind::ProjectSettings => "slider.horizontal.3",
                PanelKind::Schedules => "calendar",
            },
            Self::Action(action) => match action {
                ActionGlyph::NewFolder => "folder.badge.plus",
                ActionGlyph::NewProject => "plus",
                ActionGlyph::EditInVim => "square.and.pencil",
                ActionGlyph::CopyPath => "link",
                ActionGlyph::CopyContents => "doc.on.doc",
                ActionGlyph::Reveal => "folder",
                ActionGlyph::OpenExternally => "arrow.up.forward.app",
            },
            // An agent's mark is its own logo, which SF Symbols does not have: Native
            // draws the same vector mark as the other themes.
            Self::Provider(_) => "",
        }
    }

    /// Whether Native draws this icon as an SF Symbol rather than its vector glyph.
    fn has_symbol(self) -> bool {
        !matches!(self, Self::Provider(_))
    }

    /// A menu's tick is medium, as AppKit draws it; the rest are regular, like the text.
    fn symbol_weight(self) -> Weight {
        match self {
            Self::Check => Weight::Medium,
            _ => Weight::Regular,
        }
    }

    /// The point size of its symbol in a box of `side` points. Symbols are drawn a little
    /// under the size of the list text, as macOS's own toolbars and sidebars draw them;
    /// the wide ones (the splits, the ellipsis) a step smaller so all read alike.
    fn symbol_points(self, side: f32) -> f32 {
        let points = side * 0.8;
        match self {
            Self::SplitRight | Self::SplitDown | Self::Layout | Self::Focus => points * 0.9,
            _ => points,
        }
    }
}

/// Paint an icon in a 14 px box, drawn at the interface text scale so it stays in
/// proportion with the text beside it. The caller owns its hit target and tooltip.
/// Native draws the icon's SF Symbol in the same box, tinted alike; a symbol this
/// macOS lacks falls back to the vector glyph.
pub fn icon(kind: Icon, color: u32) -> AnyElement {
    paint_icon(kind, color, None)
}

/// An icon on a bar or row of text designed at `text` px. Native draws its SF Symbol at
/// that text's point size, as AppKit pairs a symbol with the label beside it, so it stands
/// as tall as the words do, and in the text color its element has at that moment: a
/// button that brightens its text on hover brightens the symbol with it. The colorful
/// themes draw the same vector glyph in `color` as `icon` does.
pub fn text_icon(kind: Icon, text: f32, color: u32) -> AnyElement {
    paint_icon(kind, color, Some(text))
}

impl Icon {
    /// The point size and weight of its symbol beside text of `points`. A tab's close mark
    /// is a small, firmer cross, as Safari and Finder draw it; the wide symbols a step smaller.
    fn text_symbol(self, points: f32) -> (f32, Weight) {
        match self {
            Self::Close => (points * 0.8, Weight::Medium),
            Self::Focus | Self::SplitRight | Self::SplitDown | Self::Layout => {
                (points * 0.92, Weight::Regular)
            }
            _ => (points, self.symbol_weight()),
        }
    }
}

/// The color text drawn here now would have, as a 0xRRGGBB value.
fn text_color(window: &gpui::Window) -> u32 {
    let color = gpui::Rgba::from(window.text_style().color);
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u32;
    (channel(color.r) << 16) | (channel(color.g) << 8) | channel(color.b)
}

fn paint_icon(kind: Icon, color: u32, text: Option<f32>) -> AnyElement {
    let scale = ui_text::scale();
    let native = ui_text::is_native();
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            if native && kind.has_symbol() {
                let side = f32::from(bounds.size.width);
                let (points, weight, tint) = match text {
                    Some(text) => {
                        let (points, weight) = kind.text_symbol(text * scale);
                        (points, weight, text_color(window))
                    }
                    None => (kind.symbol_points(side), kind.symbol_weight(), color),
                };
                if paint_symbol(kind.symbol(), points, weight, tint, bounds, window) {
                    return;
                }
            }
            let mut path = if matches!(kind, Icon::More) {
                PathBuilder::fill()
            } else {
                PathBuilder::stroke(px(1.25))
            };
            match kind {
                Icon::Lock | Icon::Unlock => {
                    // A shared body keeps the two states from shifting in the toolbar.
                    path.move_to(point(px(3.5), px(6.5)));
                    path.line_to(point(px(10.5), px(6.5)));
                    path.curve_to(point(px(11.0), px(7.0)), point(px(11.0), px(6.5)));
                    path.line_to(point(px(11.0), px(12.0)));
                    path.curve_to(point(px(10.5), px(12.5)), point(px(11.0), px(12.5)));
                    path.line_to(point(px(3.5), px(12.5)));
                    path.curve_to(point(px(3.0), px(12.0)), point(px(3.0), px(12.5)));
                    path.line_to(point(px(3.0), px(7.0)));
                    path.curve_to(point(px(3.5), px(6.5)), point(px(3.0), px(6.5)));
                    path.close();
                    path.move_to(point(px(4.5), px(6.5)));
                    path.line_to(point(px(4.5), px(4.0)));
                    path.cubic_bezier_to(
                        point(px(9.5), px(4.0)),
                        point(px(4.5), px(0.7)),
                        point(px(9.5), px(0.7)),
                    );
                    if matches!(kind, Icon::Lock) {
                        path.line_to(point(px(9.5), px(6.5)));
                    }
                    line(&mut path, (7.0, 8.8), (7.0, 10.5));
                }
                Icon::Focus => {
                    for (x, y, inner_x, inner_y) in [
                        (2.0, 2.0, 5.0, 5.0),
                        (12.0, 2.0, 9.0, 5.0),
                        (2.0, 12.0, 5.0, 9.0),
                        (12.0, 12.0, 9.0, 9.0),
                    ] {
                        path.move_to(point(px(inner_x), px(y)));
                        path.line_to(point(px(x), px(y)));
                        path.line_to(point(px(x), px(inner_y)));
                    }
                }
                Icon::Add => {
                    line(&mut path, (7.0, 2.5), (7.0, 11.5));
                    line(&mut path, (2.5, 7.0), (11.5, 7.0));
                }
                Icon::SplitRight | Icon::SplitDown => {
                    rectangle(&mut path, 2.0, 2.0, 12.0, 12.0);
                    if matches!(kind, Icon::SplitRight) {
                        line(&mut path, (7.0, 2.0), (7.0, 12.0));
                    } else {
                        line(&mut path, (2.0, 7.0), (12.0, 7.0));
                    }
                }
                Icon::Close => {
                    line(&mut path, (3.0, 3.0), (11.0, 11.0));
                    line(&mut path, (11.0, 3.0), (3.0, 11.0));
                }
                Icon::More => {
                    for x in [3.0, 7.0, 11.0] {
                        dot(&mut path, x, 7.0);
                    }
                }
                Icon::Bell | Icon::BellOff => {
                    line(&mut path, (7.0, 1.3), (7.0, 2.5));
                    path.move_to(point(px(2.5), px(10.0)));
                    path.line_to(point(px(4.0), px(8.4)));
                    path.line_to(point(px(4.0), px(5.5)));
                    path.cubic_bezier_to(
                        point(px(7.0), px(2.5)),
                        point(px(4.0), px(3.8)),
                        point(px(5.1), px(2.5)),
                    );
                    path.cubic_bezier_to(
                        point(px(10.0), px(5.5)),
                        point(px(8.9), px(2.5)),
                        point(px(10.0), px(3.8)),
                    );
                    path.line_to(point(px(10.0), px(8.4)));
                    path.line_to(point(px(11.5), px(10.0)));
                    path.close();
                    path.move_to(point(px(5.5), px(11.3)));
                    path.cubic_bezier_to(
                        point(px(8.5), px(11.3)),
                        point(px(5.9), px(12.7)),
                        point(px(8.1), px(12.7)),
                    );
                    if matches!(kind, Icon::BellOff) {
                        line(&mut path, (1.5, 1.5), (12.5, 12.5));
                    }
                }
                Icon::Main => star_glyph(&mut path),
                Icon::Check => check_glyph(&mut path),
                Icon::Layout => layout_glyph(&mut path),
                Icon::Panel(panel) => panel_glyph(&mut path, panel),
                Icon::Action(action) => action_glyph(&mut path, action),
                Icon::Provider(provider) => provider_glyph(&mut path, provider),
            }
            path.scale(scale);
            path.translate(bounds.origin);
            if let Ok(path) = path.build() {
                window.paint_path(path, rgb(color));
            }
        },
    )
    .size(px(14.0 * scale))
    .flex_shrink_0()
    .into_any_element()
}

/// Paint SF Symbol `name` centered in the square at `bounds`' origin. False when this
/// macOS has no such symbol, so the caller can draw something else.
fn paint_symbol(
    name: &'static str,
    points: f32,
    weight: Weight,
    color: u32,
    bounds: Bounds<gpui::Pixels>,
    window: &mut gpui::Window,
) -> bool {
    let scale = window.scale_factor();
    let square = device_square(bounds, scale);
    let side = f32::from(square.size.width);
    let key = symbols::Key::new(name, points, side, weight, color, scale);
    let Some(image) = symbols::image(key) else {
        return false;
    };
    let _ = window.paint_image(square, square, Corners::default(), image, 0, false);
    true
}

/// The square a symbol's bitmap is painted over: `bounds`' width rounded to whole device
/// pixels and its origin moved onto a device pixel, both at the window's `scale`. GPUI rounds
/// an image's edges to device pixels one by one, so a box at a fractional size or position
/// would come out a pixel wider or narrower than its bitmap and be resampled, blurring it;
/// this square maps the bitmap's pixels one to one onto the screen's.
fn device_square(bounds: Bounds<gpui::Pixels>, scale: f32) -> Bounds<gpui::Pixels> {
    let pixels = (f32::from(bounds.size.width) * scale).round().max(1.0);
    let snap = |value: gpui::Pixels| px((f32::from(value) * scale).round() / scale);
    Bounds::new(
        point(snap(bounds.origin.x), snap(bounds.origin.y)),
        gpui::size(px(pixels / scale), px(pixels / scale)),
    )
}

/// SF Symbol `name` at `points` (a design size, grown with the interface text like the text
/// beside it), centered in a box a little larger than the symbol so it never clips. `color`
/// None takes the text color its element has at that moment, so a button that brightens its
/// text on hover brightens the symbol with it. Native's own controls use it; elsewhere, or on
/// a macOS without the symbol, the box stays empty.
pub fn symbol(name: &'static str, points: f32, color: Option<u32>) -> AnyElement {
    symbol_with_limit(name, points, color, None)
}

/// Fit both the bitmap and its canvas inside an adaptively sized control.
pub fn symbol_in_box(
    name: &'static str,
    points: f32,
    color: Option<u32>,
    side: gpui::Pixels,
) -> AnyElement {
    symbol_with_limit(name, points, color, Some(f32::from(side)))
}

fn fitted_symbol_size(points: f32, scale: f32, limit: Option<f32>) -> (f32, f32) {
    let normal = (points * 1.3).max(14.0) * scale;
    let side = limit.map_or(normal, |limit| normal.min(limit.max(1.0)));
    (points * scale * side / normal, side)
}

fn symbol_with_limit(
    name: &'static str,
    points: f32,
    color: Option<u32>,
    limit: Option<f32>,
) -> AnyElement {
    let scale = ui_text::scale();
    let (points, side) = fitted_symbol_size(points, scale, limit);
    let native = ui_text::is_native();
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            if native {
                let tint = color.unwrap_or_else(|| text_color(window));
                paint_symbol(name, points, Weight::Regular, tint, bounds, window);
            }
        },
    )
    .size(px(side))
    .flex_shrink_0()
    .into_any_element()
}

/// The SF Symbol Native draws for a text mark that stands for a control: a row's
/// disclosure triangle, a sort arrow, a gear, a pencil. None keeps the mark as text.
pub fn mark_symbol(mark: &str) -> Option<&'static str> {
    Some(match mark {
        "⚙" => "gearshape",
        "↗" => "arrow.up.right",
        "✎" => "pencil",
        "×" => "xmark",
        "+" => "plus",
        "▸" => "chevron.right",
        "▾" => "chevron.down",
        "▴" => "chevron.up",
        "↓" => "arrow.down",
        "↑" => "arrow.up",
        "✓" => "checkmark",
        "⌕" => "magnifyingglass",
        _ => return None,
    })
}

/// A text mark used as a control, as the colorful themes show it; under Native, its SF
/// Symbol in a box the size of the text at `size` px, so it sits in the line like the mark.
/// Disclosure chevrons are drawn smaller and bolder, as in a Finder sidebar.
pub fn mark(mark: &'static str, size: f32, color: u32) -> AnyElement {
    paint_mark(mark, size, Some(color))
}

/// A text mark used as a control, whose Native symbol takes the text color its element has
/// at that moment, as the mark itself does in the colorful themes: a control that brightens
/// its text on hover brightens the symbol with it.
pub fn text_mark(mark: &'static str, size: f32) -> AnyElement {
    paint_mark(mark, size, None)
}

fn paint_mark(mark: &'static str, size: f32, color: Option<u32>) -> AnyElement {
    let symbol = mark_symbol(mark).filter(|_| ui_text::is_native());
    let Some(name) = symbol else {
        return div().flex_none().child(mark).into_any_element();
    };
    let chevron = name.starts_with("chevron");
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let side = f32::from(bounds.size.width);
            let (points, weight) = if chevron {
                (side * 0.62, Weight::Medium)
            } else {
                (side * 0.8, Weight::Regular)
            };
            let color = color.unwrap_or_else(|| text_color(window));
            paint_symbol(name, points, weight, color, bounds, window);
        },
    )
    .size(ui_text::text(size + 2.0))
    .flex_shrink_0()
    .into_any_element()
}

/// A five-pointed star, point up.
fn star_glyph(path: &mut PathBuilder) {
    let points: Vec<(f32, f32)> = (0..10)
        .map(|index| {
            let angle = (-90.0_f32 + 36.0 * index as f32).to_radians();
            let radius = if index % 2 == 0 { 4.9 } else { 2.2 };
            (7.0 + radius * angle.cos(), 7.5 + radius * angle.sin())
        })
        .collect();
    polygon(path, &points);
}

fn check_glyph(path: &mut PathBuilder) {
    path.move_to(point(px(2.5), px(7.5)));
    path.line_to(point(px(5.5), px(11.0)));
    path.line_to(point(px(11.5), px(3.0)));
}

/// A window whose left strip holds three short rows, like a list of navigation panels, beside
/// the open area for everything else, which has a bar along its top.
fn layout_glyph(path: &mut PathBuilder) {
    rectangle(path, 1.5, 2.0, 12.5, 12.0);
    line(path, (5.8, 2.0), (5.8, 12.0));
    // A bar over the open area, which is what the strip is not.
    line(path, (5.8, 5.2), (12.5, 5.2));
    for y in [4.6, 7.0, 9.4] {
        line(path, (3.0, y), (4.3, y));
    }
}

/// Codex is a shell prompt, `>_`; Claude is a spark of four crossing strokes.
fn provider_glyph(path: &mut PathBuilder, provider: Provider) {
    match provider {
        Provider::Codex => {
            path.move_to(point(px(2.5), px(3.0)));
            path.line_to(point(px(6.8), px(6.8)));
            path.line_to(point(px(2.5), px(10.6)));
            line(path, (8.0, 11.0), (12.5, 11.0));
        }
        Provider::Claude => {
            line(path, (7.0, 1.5), (7.0, 12.5));
            line(path, (1.5, 7.0), (12.5, 7.0));
            line(path, (3.1, 3.1), (10.9, 10.9));
            line(path, (10.9, 3.1), (3.1, 10.9));
        }
    }
}

/// One distinct, simple symbol per built-in panel, in the same 14 px box.
fn panel_glyph(path: &mut PathBuilder, panel: PanelKind) {
    match panel {
        PanelKind::Projects => folder(path),
        PanelKind::Worktrees => {
            // A trunk with a branch that peels off toward the right.
            circle(path, 4.0, 3.0, 1.3);
            circle(path, 4.0, 11.0, 1.3);
            circle(path, 10.5, 4.8, 1.3);
            line(path, (4.0, 4.3), (4.0, 9.7));
            path.move_to(point(px(9.2), px(4.8)));
            path.cubic_bezier_to(
                point(px(4.0), px(8.0)),
                point(px(6.0), px(4.8)),
                point(px(4.0), px(6.0)),
            );
        }
        PanelKind::Files => {
            polygon(
                path,
                &[
                    (3.0, 1.5),
                    (8.0, 1.5),
                    (11.0, 4.5),
                    (11.0, 12.5),
                    (3.0, 12.5),
                ],
            );
            // The folded corner and two lines of text.
            path.move_to(point(px(8.0), px(1.5)));
            path.line_to(point(px(8.0), px(4.5)));
            path.line_to(point(px(11.0), px(4.5)));
            line(path, (5.0, 7.3), (9.0, 7.3));
            line(path, (5.0, 9.6), (9.0, 9.6));
        }
        PanelKind::Preview => {
            // An eye: the lids meet at the corners, and the pupil sits between them.
            path.move_to(point(px(1.2), px(7.0)));
            path.cubic_bezier_to(
                point(px(7.0), px(3.0)),
                point(px(3.0), px(4.6)),
                point(px(4.8), px(3.0)),
            );
            path.cubic_bezier_to(
                point(px(12.8), px(7.0)),
                point(px(9.2), px(3.0)),
                point(px(11.0), px(4.6)),
            );
            path.cubic_bezier_to(
                point(px(7.0), px(11.0)),
                point(px(11.0), px(9.4)),
                point(px(9.2), px(11.0)),
            );
            path.cubic_bezier_to(
                point(px(1.2), px(7.0)),
                point(px(4.8), px(11.0)),
                point(px(3.0), px(9.4)),
            );
            path.close();
            circle(path, 7.0, 7.0, 1.7);
        }
        PanelKind::Tasks => {
            rectangle(path, 2.0, 2.0, 12.0, 12.0);
            path.move_to(point(px(4.6), px(7.2)));
            path.line_to(point(px(6.4), px(9.0)));
            path.line_to(point(px(9.6), px(5.0)));
        }
        PanelKind::Shells => {
            // A prompt: `>_`.
            path.move_to(point(px(2.5), px(3.5)));
            path.line_to(point(px(6.0), px(7.0)));
            path.line_to(point(px(2.5), px(10.5)));
            line(path, (7.5, 10.8), (11.5, 10.8));
        }
        PanelKind::Usage => {
            line(path, (3.0, 12.0), (3.0, 8.0));
            line(path, (7.0, 12.0), (7.0, 3.5));
            line(path, (11.0, 12.0), (11.0, 6.0));
        }
        PanelKind::Settings => {
            // Sliders: each track is split around its knob.
            for (y, knob) in [(3.5, 9.0), (7.0, 4.8), (10.5, 8.2)] {
                line(path, (2.0, y), (knob - 1.7, y));
                line(path, (knob + 1.7, y), (12.0, y));
                circle(path, knob, y, 1.3);
            }
        }
        PanelKind::ProjectSettings => {
            // The project folder with one slider through it.
            folder(path);
            line(path, (3.6, 8.4), (5.9, 8.4));
            line(path, (8.1, 8.4), (10.4, 8.4));
            circle(path, 7.0, 8.4, 1.1);
        }
        PanelKind::Schedules => {
            circle(path, 7.0, 7.0, 5.3);
            path.move_to(point(px(7.0), px(3.8)));
            path.line_to(point(px(7.0), px(7.0)));
            path.line_to(point(px(9.4), px(8.4)));
        }
    }
}

/// One simple symbol per text button, in the same 14 px box.
fn action_glyph(path: &mut PathBuilder, action: ActionGlyph) {
    match action {
        ActionGlyph::NewFolder => {
            // The folder loses its bottom-right corner to make room for the plus.
            path.move_to(point(px(6.5), px(12.0)));
            path.line_to(point(px(1.5), px(12.0)));
            path.line_to(point(px(1.5), px(2.5)));
            path.line_to(point(px(5.3), px(2.5)));
            path.line_to(point(px(6.6), px(4.2)));
            path.line_to(point(px(12.5), px(4.2)));
            path.line_to(point(px(12.5), px(6.5)));
            plus(path, 10.5, 10.3, 2.3);
        }
        ActionGlyph::NewProject => {
            // Same corner badge as the folder, so the two read as a pair.
            path.move_to(point(px(6.5), px(12.0)));
            path.line_to(point(px(2.0), px(12.0)));
            path.line_to(point(px(2.0), px(2.0)));
            path.line_to(point(px(12.0), px(2.0)));
            path.line_to(point(px(12.0), px(6.5)));
            plus(path, 10.5, 10.3, 2.3);
        }
        ActionGlyph::EditInVim => {
            // A pencil on the diagonal, tip down-left, with its tip cone and eraser band.
            polygon(
                path,
                &[
                    (2.2, 11.8),
                    (3.44, 8.44),
                    (9.94, 1.94),
                    (12.06, 4.06),
                    (5.56, 10.56),
                ],
            );
            line(path, (3.44, 8.44), (5.56, 10.56));
            line(path, (8.7, 3.2), (10.8, 5.3));
        }
        ActionGlyph::CopyPath => {
            // Two chain links, the second interlocked with the first.
            capsule(path, (3.8, 10.2), (6.3, 7.7), 1.8);
            capsule(path, (7.7, 6.3), (10.2, 3.8), 1.8);
        }
        ActionGlyph::CopyContents => {
            // The page behind shows only where the page in front does not cover it.
            rectangle(path, 5.0, 5.0, 12.0, 12.5);
            path.move_to(point(px(5.0), px(9.0)));
            path.line_to(point(px(2.0), px(9.0)));
            path.line_to(point(px(2.0), px(1.5)));
            path.line_to(point(px(9.0), px(1.5)));
            path.line_to(point(px(9.0), px(5.0)));
        }
        ActionGlyph::Reveal => {
            // A folder with a magnifier over its corner: find it in Finder.
            path.move_to(point(px(5.0), px(12.0)));
            path.line_to(point(px(1.5), px(12.0)));
            path.line_to(point(px(1.5), px(2.5)));
            path.line_to(point(px(5.3), px(2.5)));
            path.line_to(point(px(6.6), px(4.2)));
            path.line_to(point(px(12.5), px(4.2)));
            path.line_to(point(px(12.5), px(5.0)));
            circle(path, 9.0, 9.0, 2.4);
            line(path, (10.8, 10.8), (12.9, 12.9));
        }
        ActionGlyph::OpenExternally => {
            // A box with an open corner and an arrow leaving through it.
            path.move_to(point(px(6.0), px(2.5)));
            path.line_to(point(px(2.5), px(2.5)));
            path.line_to(point(px(2.5), px(11.5)));
            path.line_to(point(px(11.5), px(11.5)));
            path.line_to(point(px(11.5), px(8.0)));
            line(path, (6.5, 7.5), (12.0, 2.0));
            path.move_to(point(px(8.3), px(2.0)));
            path.line_to(point(px(12.0), px(2.0)));
            path.line_to(point(px(12.0), px(5.7)));
        }
    }
}

fn folder(path: &mut PathBuilder) {
    polygon(
        path,
        &[
            (1.5, 11.5),
            (1.5, 2.5),
            (5.3, 2.5),
            (6.6, 4.2),
            (12.5, 4.2),
            (12.5, 11.5),
        ],
    );
}

fn line(path: &mut PathBuilder, from: (f32, f32), to: (f32, f32)) {
    path.move_to(point(px(from.0), px(from.1)));
    path.line_to(point(px(to.0), px(to.1)));
}

fn polygon(path: &mut PathBuilder, points: &[(f32, f32)]) {
    let mut points = points.iter();
    if let Some(&(x, y)) = points.next() {
        path.move_to(point(px(x), px(y)));
    }
    for &(x, y) in points {
        path.line_to(point(px(x), px(y)));
    }
    path.close();
}

fn rectangle(path: &mut PathBuilder, left: f32, top: f32, right: f32, bottom: f32) {
    path.move_to(point(px(left), px(top)));
    path.line_to(point(px(right), px(top)));
    path.line_to(point(px(right), px(bottom)));
    path.line_to(point(px(left), px(bottom)));
    path.close();
}

fn plus(path: &mut PathBuilder, x: f32, y: f32, arm: f32) {
    line(path, (x - arm, y), (x + arm, y));
    line(path, (x, y - arm), (x, y + arm));
}

/// A stadium outline: the segment `from`..`to` swollen by `radius` on both sides.
fn capsule(path: &mut PathBuilder, from: (f32, f32), to: (f32, f32), radius: f32) {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let length = dx.hypot(dy);
    // Unit vectors along the segment and to its left, scaled to the radius.
    let (along, across) = (
        (dx / length * radius, dy / length * radius),
        (-dy / length * radius, dx / length * radius),
    );
    let control = 0.552_284_8;
    let offset = |center: (f32, f32), a: (f32, f32)| point(px(center.0 + a.0), px(center.1 + a.1));
    // A quarter turn about `center` from radius vector `a` to `b`.
    let quarter = |path: &mut PathBuilder, center: (f32, f32), a: (f32, f32), b: (f32, f32)| {
        path.cubic_bezier_to(
            offset(center, b),
            offset(center, (a.0 + b.0 * control, a.1 + b.1 * control)),
            offset(center, (b.0 + a.0 * control, b.1 + a.1 * control)),
        );
    };
    let negate = |v: (f32, f32)| (-v.0, -v.1);
    path.move_to(offset(from, across));
    path.line_to(offset(to, across));
    quarter(path, to, across, along);
    quarter(path, to, along, negate(across));
    path.line_to(offset(from, negate(across)));
    quarter(path, from, negate(across), negate(along));
    quarter(path, from, negate(along), across);
    path.close();
}

fn dot(path: &mut PathBuilder, x: f32, y: f32) {
    circle(path, x, y, 0.9);
}

fn circle(path: &mut PathBuilder, x: f32, y: f32, radius: f32) {
    let control = radius * 0.552_284_8;
    path.move_to(point(px(x + radius), px(y)));
    path.cubic_bezier_to(
        point(px(x), px(y + radius)),
        point(px(x + radius), px(y + control)),
        point(px(x + control), px(y + radius)),
    );
    path.cubic_bezier_to(
        point(px(x - radius), px(y)),
        point(px(x - control), px(y + radius)),
        point(px(x - radius), px(y + control)),
    );
    path.cubic_bezier_to(
        point(px(x), px(y - radius)),
        point(px(x - radius), px(y - control)),
        point(px(x - control), px(y - radius)),
    );
    path.cubic_bezier_to(
        point(px(x + radius), px(y)),
        point(px(x + control), px(y - radius)),
        point(px(x + radius), px(y - control)),
    );
    path.close();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fitted_symbols_keep_their_canvas_and_bitmap_inside_small_controls() {
        for scale in [1.0, 1.5, 24.0 / 11.0] {
            for side in [1.0, 15.0, 19.0, 26.0, 44.0] {
                let (points, canvas) = fitted_symbol_size(11.0, scale, Some(side));
                assert!(canvas <= side);
                assert!(points * 1.3 <= canvas + 0.001);
                assert!(points <= 11.0 * scale);
            }
            let (points, side) = fitted_symbol_size(11.0, scale, None);
            assert!((points - 11.0 * scale).abs() <= 0.001);
            assert!((side - 14.3 * scale).abs() <= 0.001);
        }
    }

    const PANELS: [PanelKind; 10] = [
        PanelKind::Projects,
        PanelKind::Worktrees,
        PanelKind::Files,
        PanelKind::Preview,
        PanelKind::Tasks,
        PanelKind::Shells,
        PanelKind::Usage,
        PanelKind::Settings,
        PanelKind::ProjectSettings,
        PanelKind::Schedules,
    ];

    const ACTIONS: [ActionGlyph; 7] = [
        ActionGlyph::NewFolder,
        ActionGlyph::NewProject,
        ActionGlyph::EditInVim,
        ActionGlyph::CopyPath,
        ActionGlyph::CopyContents,
        ActionGlyph::Reveal,
        ActionGlyph::OpenExternally,
    ];

    /// Cells per pixel side for the coverage comparison below.
    const GRID: usize = 4;

    struct Shape {
        vertices: Vec<(i32, i32)>,
        /// Which cells of a 14 px box the glyph paints, `GRID` cells per pixel.
        cells: Vec<bool>,
    }

    fn shape(name: &str, draw: impl FnOnce(&mut PathBuilder)) -> Shape {
        let mut path = PathBuilder::stroke(px(1.25));
        draw(&mut path);
        let path = path.build().expect("glyph tessellates");
        // Tessellated stroke geometry includes the line width, so this is the painted extent.
        let (left, top) = (path.bounds.origin.x.as_f32(), path.bounds.origin.y.as_f32());
        let (right, bottom) = (
            left + path.bounds.size.width.as_f32(),
            top + path.bounds.size.height.as_f32(),
        );
        assert!(
            left >= 0.0 && top >= 0.0 && right <= 14.0 && bottom <= 14.0,
            "{name} leaves the 14 px box: {left},{top} to {right},{bottom}"
        );
        assert!(right - left >= 8.0 && bottom - top >= 8.0, "{name} is tiny");
        let points: Vec<(f32, f32)> = path
            .vertices
            .iter()
            .map(|vertex| (vertex.xy_position.x.as_f32(), vertex.xy_position.y.as_f32()))
            .collect();
        let side = 14 * GRID;
        let mut cells = vec![false; side * side];
        for triangle in points.chunks_exact(3) {
            let edge = |a: (f32, f32), b: (f32, f32), p: (f32, f32)| {
                (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0)
            };
            for (index, cell) in cells.iter_mut().enumerate() {
                let p = (
                    ((index % side) as f32 + 0.5) / GRID as f32,
                    ((index / side) as f32 + 0.5) / GRID as f32,
                );
                let signs = [
                    edge(triangle[0], triangle[1], p),
                    edge(triangle[1], triangle[2], p),
                    edge(triangle[2], triangle[0], p),
                ];
                if signs.iter().all(|s| *s >= 0.0) || signs.iter().all(|s| *s <= 0.0) {
                    *cell = true;
                }
            }
        }
        let mut vertices: Vec<_> = points
            .iter()
            .map(|(x, y)| ((x * 100.0).round() as i32, (y * 100.0).round() as i32))
            .collect();
        vertices.sort_unstable();
        vertices.dedup();
        Shape { vertices, cells }
    }

    /// The share of painted cells that only one of two glyphs paints.
    fn difference(a: &Shape, b: &Shape) -> f32 {
        let (mut either, mut one) = (0, 0);
        for (a, b) in a.cells.iter().zip(&b.cells) {
            either += usize::from(*a || *b);
            one += usize::from(a != b);
        }
        one as f32 / either as f32
    }

    #[test]
    fn control_marks_map_to_symbols_and_text_stays_text() {
        assert_eq!(mark_symbol("▸"), Some("chevron.right"));
        assert_eq!(mark_symbol("⚙"), Some("gearshape"));
        assert_eq!(mark_symbol("A1"), None);
        for mark in ["⚙", "↗", "✎", "×", "+", "▸", "▾", "▴", "↓", "↑", "✓", "⌕"]
        {
            let name = mark_symbol(mark).unwrap();
            let key = symbols::Key::new(name, 12.0, 14.0, Weight::Regular, 0, 2.0);
            assert!(symbols::image(key).is_some(), "{mark}: {name}");
        }
    }

    #[test]
    fn every_panel_and_action_has_its_own_symbol() {
        let panels: Vec<_> = PANELS
            .iter()
            .map(|panel| Icon::Panel(*panel).symbol())
            .collect();
        let actions: Vec<_> = ACTIONS
            .iter()
            .map(|action| Icon::Action(*action).symbol())
            .collect();
        for names in [&panels, &actions] {
            let mut unique = names.to_vec();
            unique.sort_unstable();
            unique.dedup();
            assert_eq!(unique.len(), names.len(), "{names:?}");
        }
        assert_eq!(Icon::Lock.symbol(), "lock");
        assert_eq!(Icon::Unlock.symbol(), "lock.open");
        assert_eq!(Icon::BellOff.symbol(), "bell.slash");
    }

    /// A symbol's box at a fractional size and position, as the text scale lays it out, is
    /// moved onto whole device pixels at 1x and 2x, so its bitmap is never resampled.
    #[test]
    fn symbol_boxes_sit_on_whole_device_pixels() {
        for scale in [1.0_f32, 2.0] {
            for (x, y, side) in [(10.3, 4.71, 16.52), (0.0, 0.0, 14.0), (203.49, 7.5, 11.8)] {
                let bounds = Bounds::new(point(px(x), px(y)), gpui::size(px(side), px(side)));
                let square = device_square(bounds, scale);
                let whole = |value: gpui::Pixels| {
                    let device = f32::from(value) * scale;
                    (device - device.round()).abs() < 1e-3
                };
                assert!(
                    whole(square.origin.x) && whole(square.origin.y),
                    "{x},{y} at {scale}x"
                );
                assert!(whole(square.size.width), "{side} at {scale}x");
                assert_eq!(square.size.width, square.size.height);
                assert!((f32::from(square.size.width) - side).abs() <= 0.5 / scale);
                assert!((f32::from(square.origin.x) - x).abs() <= 0.5 / scale);
                // The bitmap has exactly as many pixels as the square covers.
                let key = symbols::Key::new(
                    "lock",
                    11.0,
                    f32::from(square.size.width),
                    Weight::Regular,
                    0,
                    scale,
                );
                assert_eq!(key.pixels() as f32, f32::from(square.size.width) * scale);
            }
        }
    }

    /// Every symbol named exists on this Mac, so Native never falls back to a vector glyph.
    #[test]
    fn every_symbol_rasterizes() {
        let fixed = [
            Icon::Lock,
            Icon::Unlock,
            Icon::Focus,
            Icon::Add,
            Icon::SplitRight,
            Icon::SplitDown,
            Icon::Close,
            Icon::More,
            Icon::Bell,
            Icon::BellOff,
            Icon::Main,
            Icon::Check,
            Icon::Layout,
        ];
        let all = fixed
            .into_iter()
            .chain(PANELS.iter().map(|panel| Icon::Panel(*panel)))
            .chain(ACTIONS.iter().map(|action| Icon::Action(*action)));
        for icon in all {
            let key = symbols::Key::new(
                icon.symbol(),
                icon.symbol_points(16.0),
                16.0,
                icon.symbol_weight(),
                0,
                2.0,
            );
            assert!(symbols::image(key).is_some(), "{icon:?}: {}", icon.symbol());
        }
    }

    #[test]
    fn the_main_pane_star_and_the_tick_stay_inside_the_icon_box() {
        // `shape` fails a glyph that leaves the box or is too small to read.
        let star = shape("star", star_glyph);
        let tick = shape("tick", check_glyph);
        assert_ne!(star.vertices, tick.vertices);
        for panel in PANELS {
            let panel_shape = shape(&format!("{panel:?}"), |path| panel_glyph(path, panel));
            assert!(
                difference(&star, &panel_shape) >= 0.3,
                "the star reads as {panel:?}"
            );
        }
    }

    #[test]
    fn the_layout_glyph_stays_inside_the_icon_box_and_reads_as_none_of_the_panels() {
        let layout = shape("layout", layout_glyph);
        for panel in PANELS {
            let panel_shape = shape(&format!("{panel:?}"), |path| panel_glyph(path, panel));
            assert_ne!(layout.vertices, panel_shape.vertices);
            assert!(
                difference(&layout, &panel_shape) >= 0.3,
                "the layout glyph reads as {panel:?}"
            );
        }
        assert_ne!(layout.vertices, shape("star", star_glyph).vertices);
    }

    #[test]
    fn every_panel_has_its_own_glyph_inside_the_icon_box() {
        let shapes: Vec<_> = PANELS
            .iter()
            .map(|panel| shape(&format!("{panel:?}"), |path| panel_glyph(path, *panel)))
            .collect();
        for (index, shape) in shapes.iter().enumerate() {
            for other in &shapes[index + 1..] {
                assert_ne!(shape.vertices, other.vertices);
            }
        }
    }

    #[test]
    fn each_agent_has_its_own_mark_that_reads_as_no_panel_and_no_action() {
        let marks = [Provider::Codex, Provider::Claude].map(|provider| {
            (
                format!("{provider:?}"),
                shape("mark", |path| provider_glyph(path, provider)),
            )
        });
        assert_ne!(marks[0].1.vertices, marks[1].1.vertices);
        assert!(difference(&marks[0].1, &marks[1].1) >= 0.3);
        for (name, mark) in &marks {
            for panel in PANELS {
                let panel_shape = shape(&format!("{panel:?}"), |path| panel_glyph(path, panel));
                let differs = difference(mark, &panel_shape);
                assert!(differs >= 0.3, "{name} reads as {panel:?} ({differs:.2})");
            }
            for action in ACTIONS {
                let action_shape = shape(&format!("{action:?}"), |path| action_glyph(path, action));
                let differs = difference(mark, &action_shape);
                assert!(differs >= 0.3, "{name} reads as {action:?} ({differs:.2})");
            }
        }
    }

    #[test]
    fn every_action_glyph_stays_inside_the_icon_box_and_differs_from_the_others() {
        let panels: Vec<_> = PANELS
            .iter()
            .map(|panel| {
                (
                    format!("{panel:?}"),
                    shape("panel", |path| panel_glyph(path, *panel)),
                )
            })
            .collect();
        let actions: Vec<_> = ACTIONS
            .iter()
            .map(|action| {
                (
                    format!("{action:?}"),
                    shape(&format!("{action:?}"), |path| action_glyph(path, *action)),
                )
            })
            .collect();
        for (index, (name, glyph)) in actions.iter().enumerate() {
            // A glyph that is a few strokes away from another reads as the same
            // symbol at 14 px, so distinct vertices are not enough.
            for (other_name, other) in actions[index + 1..].iter().chain(&panels) {
                assert_ne!(
                    glyph.vertices, other.vertices,
                    "{name} repeats {other_name}"
                );
                let differs = difference(glyph, other);
                assert!(
                    differs >= 0.3,
                    "{name} and {other_name} differ in only {differs:.2} of their ink"
                );
            }
        }
    }
}
