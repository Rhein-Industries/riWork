//! The one-line text of a call header. GPUI's `truncate()` measures a line with the
//! element's base font, so a command or path drawn in the code font overflowed its
//! box and was clipped mid-glyph, and a cut just after a `·` separator left `·…`.
//! This element shapes every run with its own font and cuts at a drawn glyph.
//! It is as wide as its text and gives way, by `shrink`, only when the row is full.
use std::ops::Range;

use gpui::{
    App, Bounds, Element, ElementId, FontWeight, GlobalElementId, Hsla, InspectorElementId,
    IntoElement, LayoutId, Length, Pixels, ShapedLine, SharedString, Style, TextAlign, TextRun,
    Window, px,
};

const ELLIPSIS: &str = "…";

/// A styled byte range of the header text.
#[derive(Clone, Debug, Default)]
pub(super) struct Span {
    pub range: Range<usize>,
    pub code: bool,
    pub color: Option<Hsla>,
    pub weight: Option<FontWeight>,
}

/// Draws `text`, or as much of it as its box keeps, then `…`.
pub(super) struct HeaderLine {
    text: SharedString,
    spans: Vec<Span>,
    shrink: f32,
}

/// A header text; a larger `shrink` gives way first.
pub(super) fn header_line(
    text: impl Into<SharedString>,
    spans: Vec<Span>,
    shrink: f32,
) -> HeaderLine {
    HeaderLine {
        text: text.into(),
        spans,
        shrink,
    }
}

/// What a header shows when only `text[..fit]` fits beside the ellipsis: a cut never
/// ends in the space or `·` of a separator, so the line reads `title…`, never `title ·…`.
fn truncated(text: &str, fit: usize) -> String {
    let kept = text[..fit].trim_end_matches(|c: char| c.is_whitespace() || c == '·');
    format!("{kept}{ELLIPSIS}")
}

/// Text runs for the first `len` bytes; an appended ellipsis continues the last run.
fn runs(base: &TextRun, spans: &[Span], len: usize, total: usize) -> Vec<TextRun> {
    let mut edges = vec![0, len];
    for span in spans {
        edges.extend([span.range.start.min(len), span.range.end.min(len)]);
    }
    edges.sort_unstable();
    edges.dedup();
    let mut runs: Vec<TextRun> = edges
        .windows(2)
        .filter(|edge| edge[1] > edge[0])
        .map(|edge| {
            let mut run = base.clone();
            run.len = edge[1] - edge[0];
            for span in spans.iter().filter(|span| span.range.contains(&edge[0])) {
                if span.code {
                    run.font.family = crate::ui_text::code_family();
                }
                if let Some(color) = span.color {
                    run.color = color;
                }
                if let Some(weight) = span.weight {
                    run.font.weight = weight;
                }
            }
            run
        })
        .collect();
    match runs.last_mut() {
        Some(last) => last.len += total - len,
        None => runs.push(TextRun {
            len: total,
            ..base.clone()
        }),
    }
    runs
}

impl HeaderLine {
    /// The whole line, unclipped.
    fn full(&self, window: &mut Window) -> (ShapedLine, TextRun, Pixels) {
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let base = style.to_run(self.text.len());
        let len = self.text.len();
        let full = window.text_system().shape_line(
            self.text.clone(),
            font_size,
            &runs(&base, &self.spans, len, len),
            None,
        );
        (full, base, font_size)
    }

    fn shape(&self, width: Pixels, window: &mut Window) -> Option<(ShapedLine, Pixels)> {
        let line_height = window.text_style().line_height_in_pixels(window.rem_size());
        let (full, base, font_size) = self.full(window);
        let len = self.text.len();
        // Half a pixel absorbs the layout's rounding of the measured width.
        if full.width <= width + px(0.5) {
            return Some((full, line_height));
        }
        let ellipsis = window
            .text_system()
            .shape_line(
                ELLIPSIS.into(),
                font_size,
                &[TextRun {
                    len: ELLIPSIS.len(),
                    ..base.clone()
                }],
                None,
            )
            .width;
        if ellipsis > width {
            // Too narrow for even `…`: a clipped glyph would read as a stray mark.
            return None;
        }
        let mut fit = full
            .index_for_x((width - ellipsis).max(px(0.)))
            .unwrap_or(len)
            .min(len);
        loop {
            let text = truncated(&self.text, fit);
            let kept = text.len() - ELLIPSIS.len();
            let line = window.text_system().shape_line(
                text.clone().into(),
                font_size,
                &runs(&base, &self.spans, kept, text.len()),
                None,
            );
            if line.width <= width || fit == 0 {
                return Some((line, line_height));
            }
            fit = self.text.floor_char_boundary(fit - 1);
        }
    }
}

impl IntoElement for HeaderLine {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for HeaderLine {
    type RequestLayoutState = ();
    type PrepaintState = Option<(ShapedLine, Pixels)>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        _: &mut App,
    ) -> (LayoutId, ()) {
        // As wide as the whole text, so a short header reads exactly as plain text;
        // a full row shrinks it to nothing at the least.
        let line_height = window.text_style().line_height_in_pixels(window.rem_size());
        let style = Style {
            flex_grow: 0.,
            flex_shrink: self.shrink,
            min_size: gpui::Size {
                width: Length::Definite(px(0.).into()),
                height: Length::Auto,
            },
            ..Style::default()
        };
        let (full, _, _) = self.full(window);
        let width = full.width.ceil();
        // Its least width is nothing, so whatever follows it on the row (the runtime)
        // keeps its own width when the row is full.
        let layout = window.request_measured_layout(style, move |_, available, _, _| gpui::Size {
            width: match available.width {
                gpui::AvailableSpace::MinContent => px(0.),
                _ => width,
            },
            height: line_height,
        });
        (layout, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        _: &mut App,
    ) -> Self::PrepaintState {
        self.shape(bounds.size.width, window)
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        shaped: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some((line, line_height)) = shaped else {
            return;
        };
        let origin = bounds.origin
            + gpui::point(
                px(0.),
                ((bounds.size.height - *line_height) / 2.).max(px(0.)),
            );
        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            line.paint(origin, *line_height, TextAlign::Left, None, window, cx)
                .ok();
        });
    }
}
