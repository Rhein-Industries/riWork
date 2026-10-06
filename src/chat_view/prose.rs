//! Drawing an agent's Markdown: the blocks `markdown` read, as elements.

use std::{
    hash::{Hash, Hasher},
    ops::Range,
};

use gpui::{
    AnyElement, Context, ElementId, FontStyle, FontWeight, HighlightStyle, SharedString,
    StrikethroughStyle, UnderlineStyle, div, prelude::*, px, rgb,
};

use crate::{controls, ui_text};

use super::{
    ChatView,
    markdown::{Align, Block, Span, plain_text},
    widgets::{Look, copy_button},
};

impl ChatView {
    /// The blocks of a message, one under the other. `key` tells this message's elements
    /// from the others'.
    pub(super) fn prose(
        &self,
        blocks: &[Block],
        key: &str,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(ui_text::space(8.0))
            .children(
                blocks
                    .iter()
                    .enumerate()
                    .map(|(at, block)| self.block(block, &format!("{key}/{at}"), look, cx)),
            )
            .into_any_element()
    }

    fn block(&self, block: &Block, key: &str, look: Look, cx: &mut Context<Self>) -> AnyElement {
        let colors = look.colors;
        match block {
            Block::Paragraph(spans) => self.text(spans, key, look, None, cx),
            Block::Heading { level, spans } => {
                let size = match level {
                    1 => 1.35,
                    2 => 1.2,
                    3 => 1.1,
                    _ => 1.0,
                };
                // Native heads a section in semibold, as its panels do.
                let weight = if look.native {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::BOLD
                };
                div()
                    .w_full()
                    .pt(ui_text::space(4.0))
                    .text_size(ui_text::text(12.0 * size))
                    .font_weight(weight)
                    .text_color(rgb(if *level <= 2 {
                        colors.cyan
                    } else {
                        colors.text
                    }))
                    .child(self.text(spans, key, look, Some(weight), cx))
                    .into_any_element()
            }
            Block::Code { language, text } => {
                self.code_block(language.as_deref(), text, key, look, cx)
            }
            Block::List { start, items } => div()
                .w_full()
                .flex()
                .flex_col()
                .gap(ui_text::space(3.0))
                .children(items.iter().enumerate().map(|(at, item)| {
                    let marker = match start {
                        Some(first) => format!("{}.", first + at as u64),
                        None => "•".to_owned(),
                    };
                    div()
                        .w_full()
                        .flex()
                        .items_start()
                        .gap(ui_text::space(6.0))
                        .child(
                            div()
                                .flex_none()
                                .min_w(ui_text::space(18.0))
                                .flex()
                                .justify_end()
                                .text_color(rgb(colors.muted))
                                .child(marker),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(ui_text::space(4.0))
                                .children(item.iter().enumerate().map(|(inner, block)| {
                                    self.block(block, &format!("{key}/{at}/{inner}"), look, cx)
                                })),
                        )
                }))
                .into_any_element(),
            Block::Quote(inner) => div()
                .w_full()
                .pl(ui_text::space(10.0))
                .border_l_2()
                .border_color(rgb(colors.divider))
                .text_color(rgb(colors.muted))
                .flex()
                .flex_col()
                .gap(ui_text::space(6.0))
                .children(
                    inner
                        .iter()
                        .enumerate()
                        .map(|(at, block)| self.block(block, &format!("{key}/{at}"), look, cx)),
                )
                .into_any_element(),
            Block::Table {
                align,
                header,
                rows,
            } => self.table(align, header, rows, key, look, cx),
            Block::Rule => div()
                .w_full()
                .my(ui_text::space(4.0))
                .h(px(1.0))
                .bg(rgb(colors.divider))
                .into_any_element(),
        }
    }

    fn code_block(
        &self,
        language: Option<&str>,
        code: &str,
        key: &str,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = look.colors;
        let copy_key = format!("copy:{key}");
        let copied = self.copied.as_deref() == Some(copy_key.as_str());
        let owned = code.to_owned();
        div()
            .w_full()
            .rounded(if look.native {
                controls::radius(controls::ROW_RADIUS)
            } else {
                px(4.0)
            })
            .border_1()
            .border_color(rgb(colors.divider))
            .bg(rgb(colors.panel_active))
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(ui_text::space(8.0))
                    .py(ui_text::space(2.0))
                    .border_b_1()
                    .border_color(rgb(colors.divider))
                    .text_size(ui_text::text(10.0))
                    .text_color(rgb(colors.muted))
                    .child(language.unwrap_or("code").to_owned())
                    .child(
                        copy_button(
                            SharedString::from(copy_key.clone()),
                            copied,
                            "copy",
                            "Copy code",
                            look,
                        )
                        .when(!look.native, |button| {
                            button
                                .border_color(rgb(colors.panel_active))
                                .bg(rgb(colors.panel_active))
                        })
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.copy(copy_key.clone(), owned.clone(), cx);
                        })),
                    ),
            )
            .child(
                self.scroller(&format!("code:{key}")).attach(
                    div()
                        .id(ElementId::Name(SharedString::from(format!("code:{key}"))))
                        .w_full()
                        // A row whose child does not shrink to it: a long line scrolls sideways.
                        .flex()
                        .items_start()
                        .overflow_x_scroll()
                        // A vertical wheel turn belongs to the transcript, not to this box.
                        .restrict_scroll_to_axis()
                        .px(ui_text::space(10.0))
                        .py(ui_text::space(8.0))
                        .text_size(ui_text::text(11.0))
                        .font_family(ui_text::code_family())
                        .child(div().flex_none().whitespace_nowrap().child(self.selectable(
                            &format!("code:{key}"),
                            code.replace('\t', "    "),
                            Vec::new(),
                            Vec::new(),
                            look,
                            cx,
                        ))),
                ),
            )
            .into_any_element()
    }

    /// A run of styled text; links in it open in the browser, and it can be selected.
    fn text(
        &self,
        spans: &[Span],
        key: &str,
        look: Look,
        base: Option<FontWeight>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if spans.iter().any(|span| span.image) {
            return div()
                .w_full()
                .flex()
                .flex_col()
                .gap(ui_text::space(6.0))
                .children(spans.iter().enumerate().map(|(at, span)| {
                    if span.image {
                        let target = span.link.as_deref().unwrap_or_default();
                        let mut hash = std::hash::DefaultHasher::new();
                        target.hash(&mut hash);
                        let ordinal = spans[..at]
                            .iter()
                            .filter(|other| other.image && other.link == span.link)
                            .count();
                        self.image_card(
                            &crate::chat::media::reference(&span.text, target),
                            &format!("{key}/image/{:x}/{ordinal}", hash.finish()),
                            None,
                            None,
                            look,
                            cx,
                        )
                    } else {
                        self.text(
                            std::slice::from_ref(span),
                            &format!("{key}/{at}"),
                            look,
                            base,
                            cx,
                        )
                    }
                }))
                .into_any_element();
        }
        let colors = look.colors;
        let mut highlights: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
        let mut links: Vec<(Range<usize>, String)> = Vec::new();
        let mut at = 0;
        for span in spans {
            let range = at..at + span.text.len();
            at = range.end;
            let mut style = HighlightStyle::default();
            if span.style.bold && base.is_none() {
                style.font_weight = Some(FontWeight::BOLD);
            }
            if span.style.italic {
                style.font_style = Some(FontStyle::Italic);
            }
            if span.style.code {
                style.background_color = Some(rgb(look.tint(colors.divider, 0.5)).into());
                // Native keeps its signal color for state; code reads in the secondary color.
                // A highlight cannot change the face, so it stays in the run's own.
                style.color = Some(
                    rgb(if look.native {
                        colors.magenta
                    } else {
                        colors.gold
                    })
                    .into(),
                );
            }
            if span.style.strike {
                style.strikethrough = Some(StrikethroughStyle {
                    thickness: px(1.0),
                    color: None,
                });
            }
            let detected = span
                .style
                .code
                .then(|| super::links::detect(&span.text, true))
                .unwrap_or_default();
            if span.link.is_none() {
                links.extend(detected.into_iter().map(|(mut range, target)| {
                    range.start += at - span.text.len();
                    range.end += at - span.text.len();
                    (range, target)
                }));
            }
            if let Some(url) = &span.link {
                style.color = Some(rgb(colors.cyan).into());
                style.underline = Some(UnderlineStyle {
                    thickness: px(1.0),
                    color: Some(rgb(colors.cyan).into()),
                    wavy: false,
                });
                links.push((range.clone(), url.clone()));
            }
            if style != HighlightStyle::default() {
                highlights.push((range, style));
            }
        }
        self.selectable(key, plain_text(spans), highlights, links, look, cx)
    }

    fn table(
        &self,
        align: &[Align],
        header: &[Vec<Span>],
        rows: &[Vec<Vec<Span>>],
        key: &str,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = look.colors;
        let bold = if look.native {
            FontWeight::SEMIBOLD
        } else {
            FontWeight::BOLD
        };
        let mut row = |cells: &[Vec<Span>], at: usize, head: bool| {
            div()
                .w_full()
                .flex()
                .border_b_1()
                .border_color(rgb(colors.divider))
                .when(head, |row| {
                    row.bg(rgb(colors.panel_active)).font_weight(bold)
                })
                .children(cells.iter().enumerate().map(|(column, cell)| {
                    div()
                        .flex_1()
                        .min_w_0()
                        .px(ui_text::space(8.0))
                        .py(ui_text::space(4.0))
                        .when(column + 1 < cells.len(), |cell| {
                            cell.border_r_1().border_color(rgb(colors.divider))
                        })
                        .when(align.get(column) == Some(&Align::Right), |cell| {
                            cell.flex().justify_end()
                        })
                        .when(align.get(column) == Some(&Align::Center), |cell| {
                            cell.flex().justify_center()
                        })
                        .child(self.text(
                            cell,
                            &format!("{key}/{at}/{column}"),
                            look,
                            head.then_some(bold),
                            cx,
                        ))
                }))
        };
        let head = row(header, 0, true);
        let body: Vec<_> = rows
            .iter()
            .enumerate()
            .map(|(at, cells)| row(cells, at + 1, false))
            .collect();
        div()
            .w_full()
            .border_t_1()
            .border_l_1()
            .border_r_1()
            .border_color(rgb(colors.divider))
            .rounded(if look.native {
                controls::radius(controls::ROW_RADIUS)
            } else {
                px(3.0)
            })
            .overflow_hidden()
            .child(head)
            .children(body)
            .into_any_element()
    }
}
