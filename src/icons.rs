//! Small vector controls that keep pane chrome consistent across fonts and themes.

use gpui::{AnyElement, IntoElement, PathBuilder, canvas, point, prelude::*, px, rgb};

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

fn line(path: &mut PathBuilder, from: (f32, f32), to: (f32, f32)) {
    path.move_to(point(px(from.0), px(from.1)));
    path.line_to(point(px(to.0), px(to.1)));
}

fn rectangle(path: &mut PathBuilder, left: f32, top: f32, right: f32, bottom: f32) {
    path.move_to(point(px(left), px(top)));
    path.line_to(point(px(right), px(top)));
    path.line_to(point(px(right), px(bottom)));
    path.line_to(point(px(left), px(bottom)));
    path.close();
}

fn dot(path: &mut PathBuilder, x: f32, y: f32) {
    let radius = 0.9;
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
