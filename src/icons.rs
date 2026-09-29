//! Small vector controls that keep pane chrome consistent across fonts and themes.

use gpui::{AnyElement, IntoElement, PathBuilder, canvas, point, prelude::*, px, rgb};

use crate::layouts::PanelKind;

#[derive(Clone, Copy)]
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
    /// The symbol a built-in panel tab shows instead of its label.
    Panel(PanelKind),
}

/// Paint an icon in a fixed 14 px box. The caller owns its hit target and tooltip.
pub fn icon(kind: Icon, color: u32) -> AnyElement {
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
                Icon::Panel(panel) => panel_glyph(&mut path, panel),
            }
            path.translate(bounds.origin);
            if let Ok(path) = path.build() {
                window.paint_path(path, rgb(color));
            }
        },
    )
    .size(px(14.0))
    .flex_shrink_0()
    .into_any_element()
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

    const PANELS: [PanelKind; 9] = [
        PanelKind::Projects,
        PanelKind::Worktrees,
        PanelKind::Files,
        PanelKind::Tasks,
        PanelKind::Shells,
        PanelKind::Usage,
        PanelKind::Settings,
        PanelKind::ProjectSettings,
        PanelKind::Schedules,
    ];

    fn shape(panel: PanelKind) -> Vec<(i32, i32)> {
        let mut path = PathBuilder::stroke(px(1.25));
        panel_glyph(&mut path, panel);
        let path = path.build().expect("panel glyph tessellates");
        // Tessellated stroke geometry includes the line width, so this is the painted extent.
        let (left, top) = (path.bounds.origin.x.as_f32(), path.bounds.origin.y.as_f32());
        let (right, bottom) = (
            left + path.bounds.size.width.as_f32(),
            top + path.bounds.size.height.as_f32(),
        );
        assert!(
            left >= 0.0 && top >= 0.0 && right <= 14.0 && bottom <= 14.0,
            "{panel:?} leaves the 14 px box: {left},{top} to {right},{bottom}"
        );
        assert!(
            right - left >= 8.0 && bottom - top >= 8.0,
            "{panel:?} is tiny"
        );
        let mut vertices: Vec<_> = path
            .vertices
            .iter()
            .map(|vertex| {
                (
                    (vertex.xy_position.x.as_f32() * 100.0).round() as i32,
                    (vertex.xy_position.y.as_f32() * 100.0).round() as i32,
                )
            })
            .collect();
        vertices.sort_unstable();
        vertices.dedup();
        vertices
    }

    #[test]
    fn every_panel_has_its_own_glyph_inside_the_icon_box() {
        let shapes: Vec<_> = PANELS.iter().map(|panel| shape(*panel)).collect();
        for (index, shape) in shapes.iter().enumerate() {
            for other in &shapes[index + 1..] {
                assert_ne!(shape, other);
            }
        }
    }
}
