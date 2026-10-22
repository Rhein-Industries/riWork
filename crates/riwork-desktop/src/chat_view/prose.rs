//! Drawing an agent's Markdown: the blocks `markdown` read, as elements.

use std::{
    hash::{Hash, Hasher},
    ops::Range,
};

use gpui::{
    AnyElement, Context, ElementId, FontStyle, FontWeight, HighlightStyle, SharedString,
    StrikethroughStyle, UnderlineStyle, div, prelude::*, px, rgb,
};
use gpui_kit::base::TestSupportExt as _;

use crate::{controls, ui_text};

use super::{
    ChatView,
    markdown::{Align, Block, CalloutKind, Span, plain_text},
    widgets::{Look, button, copy_button},
};

impl ChatView {
    /// TextView is compatible with nonvirtual ordinary prompt prose. Its
    /// parser/state and emphasis/headings are library owned. Selection here
    /// stays disabled as before: these are question instructions, not part of
    /// the transcript document. See the pinned API boundary in the handoff.
    pub(super) fn ordinary_prose(
        &self,
        key: &str,
        source: &str,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let blocks = super::markdown::parse(source);
        let ordinary = blocks.iter().all(|block| match block {
            Block::Paragraph(spans) | Block::Heading { spans, .. } => spans
                .iter()
                .all(|span| !span.image && span.link.is_none() && !span.style.code),
            _ => false,
        });
        if !ordinary {
            return div().child(source.to_owned()).into_any_element();
        }
        let style = gpui_kit::base::TextViewStyle::from_theme(&gpui_kit::base::Theme::global(cx))
            .with_foreground(rgb(look.colors.text).into())
            .with_muted_foreground(rgb(look.colors.muted).into())
            .with_link(rgb(look.colors.cyan).into())
            .with_selection(rgb(look.tint(look.colors.cyan, 0.4)).into())
            .with_paragraph_gap(gpui::rems(0.))
            .with_heading(move |level| {
                let factor = match level {
                    1 => 1.35,
                    2 => 1.2,
                    3 => 1.1,
                    _ => 1.,
                };
                let mut heading = div()
                    .text_size(ui_text::text(12. * factor))
                    .font_weight(if look.native {
                        FontWeight::SEMIBOLD
                    } else {
                        FontWeight::BOLD
                    })
                    .text_color(rgb(if level <= 2 {
                        look.colors.cyan
                    } else {
                        look.colors.text
                    }));
                heading.style().clone()
            });
        gpui_kit::base::TextView::markdown(
            SharedString::from(format!("kit-prose:{key}")),
            source.to_owned(),
        )
        .style(style)
        .selectable(false)
        .scrollable(false)
        // Prompt text had no navigation action. Never use the default URL
        // opener if a parser recognizes an unexpected link/autolink.
        .on_link_click(|_, _, _, _| {})
        .into_any_element()
    }

    /// The blocks of a message, one under the other. `key` tells this message's elements
    /// from the others'.
    pub(super) fn prose(
        &self,
        blocks: &[Block],
        key: &str,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.prose_at(blocks, key, 0, look, cx)
    }

    pub(super) fn prose_at(
        &self,
        blocks: &[Block],
        key: &str,
        start: usize,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id(ElementId::Name(SharedString::from(format!("prose:{key}"))))
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            // Every design's prose spacing: roomy lines and paragraphs. Hermes sets it in its
            // proportional face, a size up.
            .gap(ui_text::space(12.0))
            .line_height(gpui::relative(1.5))
            .when(look.hermes(), |prose| {
                prose
                    .font_family(".SystemUIFont")
                    .text_size(ui_text::text(14.0))
            })
            .children(
                blocks.iter().enumerate().map(|(at, block)| {
                    self.block(block, &format!("{key}/{}", start + at), look, cx)
                }),
            )
            .test_support()
            .into_any_element()
    }

    fn block(&self, block: &Block, key: &str, look: Look, cx: &mut Context<Self>) -> AnyElement {
        let colors = look.colors;
        let hermes = look.hermes();
        match block {
            Block::Paragraph(spans) | Block::Task { spans, .. } => {
                self.text(spans, key, look, None, cx)
            }
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
                    .text_size(ui_text::text((if hermes { 14.0 } else { 12.0 }) * size))
                    .font_weight(weight)
                    .text_color(rgb(if *level <= 2 && !hermes {
                        colors.cyan
                    } else {
                        colors.text
                    }))
                    .child(self.text(spans, key, look, Some(weight), cx))
                    .into_any_element()
            }
            Block::Code {
                language,
                title,
                text,
            } => {
                let fallback =
                    self.code_block(language.as_deref(), title.as_deref(), text, key, look, cx);
                if language
                    .as_deref()
                    .is_some_and(|language| language.eq_ignore_ascii_case("mermaid"))
                {
                    self.diagram_block(text, key, fallback, look, cx)
                } else {
                    fallback
                }
            }
            Block::List { start, items } => div()
                .w_full()
                .flex()
                .flex_col()
                .min_w_0()
                .gap(ui_text::space(8.0))
                .children(items.iter().enumerate().map(|(at, item)| {
                    let marker = match item.first() {
                        Some(Block::Task { checked, .. }) => {
                            if *checked { "☑" } else { "☐" }.to_owned()
                        }
                        _ => match start {
                            Some(first) => format!("{}.", first + at as u64),
                            None => "•".to_owned(),
                        },
                    };
                    div()
                        .w_full()
                        .flex()
                        .items_start()
                        .gap(ui_text::space(10.0))
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
                                .gap(ui_text::space(8.0))
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
                .gap(ui_text::space(12.0))
                .children(
                    inner
                        .iter()
                        .enumerate()
                        .map(|(at, block)| self.block(block, &format!("{key}/{at}"), look, cx)),
                )
                .into_any_element(),
            Block::Callout { kind, blocks } => {
                let accent = match kind {
                    CalloutKind::Warning | CalloutKind::Caution => colors.gold,
                    CalloutKind::Tip => look.diff.added,
                    CalloutKind::Important => colors.magenta,
                    CalloutKind::Note => colors.cyan,
                };
                div()
                    .w_full()
                    .pl(ui_text::space(12.0))
                    .py(ui_text::space(6.0))
                    .border_l_2()
                    .border_color(rgb(accent))
                    .flex()
                    .flex_col()
                    .gap(ui_text::space(8.0))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(accent))
                            .child(self.selectable(
                                &format!("callout:{key}"),
                                kind.label().to_owned(),
                                Vec::new(),
                                Vec::new(),
                                look,
                                cx,
                            )),
                    )
                    .children(
                        blocks
                            .iter()
                            .enumerate()
                            .map(|(at, block)| self.block(block, &format!("{key}/{at}"), look, cx)),
                    )
                    .into_any_element()
            }
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
            Block::Files(files) => self.cited_files(files, key, look, cx),
        }
    }

    fn code_block(
        &self,
        language: Option<&str>,
        title: Option<&str>,
        code: &str,
        key: &str,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = look.colors;
        let copy_key = format!("copy:{key}");
        let copied = self.copied.as_deref() == Some(copy_key.as_str());
        let owned = code.to_owned();
        let wrap_key = format!("wrap-code:{key}");
        let wrapped = self.open.contains(&wrap_key);
        let shown = code.replace('\t', "    ");
        let highlights = super::code_highlight::highlight(language, title, &shown, colors.bg);
        let caption = match (title, language) {
            (Some(title), Some(language)) => format!("{title} · {language}"),
            (Some(title), None) => title.to_owned(),
            (None, language) => language.unwrap_or("code").to_owned(),
        };
        div()
            .w_full()
            .rounded(if look.native {
                controls::radius(controls::ROW_RADIUS)
            } else {
                px(4.0)
            })
            .border_1()
            .border_color(rgb(colors.divider))
            .bg(rgb(if look.hermes() {
                colors.panel
            } else {
                colors.panel_active
            }))
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .px(ui_text::space(8.0))
                    .py(ui_text::space(2.0))
                    .border_b_1()
                    .border_color(rgb(colors.divider))
                    .text_size(ui_text::text(10.0))
                    .text_color(rgb(colors.muted))
                    .child(div().flex_1().min_w_0().truncate().child(caption))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .flex_none()
                            .items_center()
                            .gap(ui_text::space(4.0))
                            .child(self.code_actions(language, title, code, key, look, cx))
                            .child(
                                button(
                                    SharedString::from(wrap_key.clone()),
                                    if wrapped { "Unwrap" } else { "Wrap" },
                                    None,
                                    look,
                                )
                                .on_click(cx.listener(
                                    move |view, _, _, cx| view.toggle_output_layout(&wrap_key, cx),
                                )),
                            )
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
                                .on_click(cx.listener(
                                    move |view, _, _, cx| {
                                        view.copy(copy_key.clone(), owned.clone(), cx);
                                    },
                                )),
                            ),
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
                        .when(!wrapped, |block| block.overflow_x_scroll())
                        // A vertical wheel turn belongs to the transcript, not to this box.
                        .restrict_scroll_to_axis()
                        .px(ui_text::space(10.0))
                        .py(ui_text::space(8.0))
                        .text_size(ui_text::text(11.0))
                        .font_family(ui_text::code_family())
                        .child(
                            div()
                                .when(!wrapped, |block| block.flex_none().whitespace_nowrap())
                                .when(wrapped, |block| block.w_full().min_w_0())
                                .child(self.selectable(
                                    &format!("code:{key}"),
                                    shown,
                                    highlights,
                                    Vec::new(),
                                    look,
                                    cx,
                                )),
                        ),
                ),
            )
            .into_any_element()
    }

    /// Wrapping changes geometry, not the selected source text or its stable keys.
    pub(super) fn toggle_output_layout(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.open.remove(key) {
            self.open.insert(key.to_owned());
        }
        self.transcript_selection.changed();
        self.invalidate_leaf_owner(key);
        self.list.remeasure_items(0..self.list_rows.len());
        self.find_list().remeasure();
        cx.notify();
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
        let wrap_key = format!("unwrap-table:{key}");
        let wrapped = !self.open.contains(&wrap_key);
        let widths = super::output_table::widths(header, rows, wrapped);
        let total = widths.iter().sum::<f32>();
        let mut row = |cells: &[Vec<Span>], at: usize, head: bool| {
            div()
                .min_w_full()
                .flex()
                .border_b_1()
                .border_color(rgb(colors.divider))
                .when(head, |row| {
                    row.bg(rgb(colors.panel_active)).font_weight(bold)
                })
                .children(cells.iter().enumerate().map(|(column, cell)| {
                    div()
                        .flex_none()
                        .w(ui_text::space(widths.get(column).copied().unwrap_or(100.0)))
                        .px(ui_text::space(8.0))
                        .py(ui_text::space(5.0))
                        .when(!wrapped, |cell| cell.whitespace_nowrap().overflow_hidden())
                        .when(column + 1 < cells.len(), |cell| {
                            cell.border_r_1().border_color(rgb(colors.divider))
                        })
                        .when(align.get(column) == Some(&Align::Right), |cell| {
                            cell.text_right()
                        })
                        .when(align.get(column) == Some(&Align::Center), |cell| {
                            cell.text_center()
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
        let markdown_key = format!("copy-table-markdown:{key}");
        let csv_key = format!("copy-table-csv:{key}");
        let copied_markdown = self.copied.as_deref() == Some(markdown_key.as_str());
        let copied_csv = self.copied.as_deref() == Some(csv_key.as_str());
        let markdown = super::output_table::markdown(align, header, rows);
        let csv = super::output_table::csv(header, rows);
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(ui_text::space(4.0))
            .child(
                self.scroller(&format!("table:{key}")).attach(
                    div()
                        .id(ElementId::Name(SharedString::from(format!("table:{key}"))))
                        .w_full()
                        .min_w_0()
                        .overflow_x_scroll()
                        .restrict_scroll_to_axis()
                        .border_t_1()
                        .border_l_1()
                        .border_r_1()
                        .border_color(rgb(colors.divider))
                        .rounded(if look.native {
                            controls::radius(controls::ROW_RADIUS)
                        } else {
                            px(3.0)
                        })
                        .child(
                            div()
                                .w(ui_text::space(total))
                                .min_w_full()
                                .child(head)
                                .children(body),
                        ),
                ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(ui_text::space(6.0))
                    .child(
                        button(
                            SharedString::from(wrap_key.clone()),
                            if wrapped {
                                "Unwrap cells"
                            } else {
                                "Wrap cells"
                            },
                            None,
                            look,
                        )
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.toggle_output_layout(&wrap_key, cx)
                        })),
                    )
                    .child(div().flex_1())
                    .child(
                        copy_button(
                            SharedString::from(markdown_key.clone()),
                            copied_markdown,
                            "Markdown",
                            "Copy table as Markdown",
                            look,
                        )
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.copy(markdown_key.clone(), markdown.clone(), cx)
                        })),
                    )
                    .child(
                        copy_button(
                            SharedString::from(csv_key.clone()),
                            copied_csv,
                            "CSV",
                            "Copy table as CSV",
                            look,
                        )
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.copy(csv_key.clone(), csv.clone(), cx)
                        })),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::model::{Item, ItemBody, ItemStatus, MessagePhase, Presentation};
    use gpui::TestAppContext;
    use gpui_kit::test::TestWindowExt;

    fn install(
        view: &gpui::Entity<ChatView>,
        text: &str,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) {
        view.update(cx, |view, cx| {
            view.model.link = super::super::Link::Live;
            view.model.transcript.items = vec![Item {
                started_at: None,
                ended_at: None,
                id: "output".into(),
                turn_id: Some("turn".into()),
                status: ItemStatus::Completed,
                presentation: Presentation {
                    phase: Some(MessagePhase::Final),
                    ..Presentation::default()
                },
                body: ItemBody::AgentMessage { text: text.into() },
            }];
            view.refresh_projection(cx);
            view.list.remeasure();
            cx.notify();
        });
        window.render_frame(cx);
    }

    #[gpui::test]
    fn native_code_wrap_reflows_without_changing_copied_source(cx: &mut TestAppContext) {
        let (handle, view, recording) = super::super::editor_tests::mount_selection(cx);
        let code = format!("let text = \"{}\";", "hello ".repeat(35));
        cx.update_window(handle.into(), |_, window, cx| {
            install(
                &view,
                &format!("```rust title=main.rs\n{code}\n```"),
                window,
                cx,
            );
            let before = window.find("transcript:code:output/0").bounds().size.height;
            window.click("wrap-code:output/0", cx);
            window.render_frame(cx);
            let after = window.find("transcript:code:output/0").bounds().size.height;
            assert!(after > before, "wrapped code must gain lines");
            window.click("copy:output/0", cx);
            assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), code);
        })
        .unwrap();
        assert!(recording.try_recv().is_err());
    }

    #[gpui::test]
    fn native_table_copy_and_semantic_markdown_reach_the_transcript(cx: &mut TestAppContext) {
        let (handle, view, recording) = super::super::editor_tests::mount_selection(cx);
        cx.update_window(handle.into(), |_, window, cx| {
            install(
                &view,
                "| Name | Status |\n| --- | --- |\n| alpha | done |",
                window,
                cx,
            );
            window.click("copy-table-csv:output/0", cx);
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                "Name,Status\r\nalpha,done"
            );
            window.click("copy-table-markdown:output/0", cx);
            assert!(
                cx.read_from_clipboard()
                    .unwrap()
                    .text()
                    .unwrap()
                    .contains("| alpha | done |")
            );
            install(
                &view,
                "> [!NOTE]\n> A **useful** note.\n\n- [x] Ready",
                window,
                cx,
            );
            assert_eq!(
                window.find("transcript:callout:output/0").label(),
                Some("Note")
            );
            assert_eq!(
                window.find("transcript:output/0/0").label(),
                Some("A useful note.")
            );
            assert_eq!(
                window.find("transcript:output/1/0/0").label(),
                Some("Ready")
            );
        })
        .unwrap();
        assert!(recording.try_recv().is_err());
    }
}
