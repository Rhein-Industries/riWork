//! Small vector controls that keep pane chrome consistent across fonts and themes.

use gpui::{AnyElement, App, IntoElement, PathBuilder, canvas, point, prelude::*, px, rgb};

use crate::{chat::model::Provider, layouts::PanelKind, settings::Settings, ui_text};

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

/// Paint an icon in a 14 px box, drawn at the interface text scale so it stays in
/// proportion with the text beside it. The caller owns its hit target and tooltip.
pub fn icon(kind: Icon, color: u32) -> AnyElement {
    let scale = ui_text::scale();
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
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
