//! Selecting the text of a message with the mouse and copying it with ⌘C.
//!
//! GPUI has no selectable text, so each run of text that can be selected (a paragraph, a
//! heading, a table cell, a code block, one of your own messages) is a `StyledText` whose
//! layout answers which character is under the pointer. A press starts a selection in that
//! run, dragging within it extends the selection, a double click takes a word and a triple
//! click the whole run. One selection exists at a time, and it stays within its run.

use std::ops::Range;

use gpui::{
    AnyElement, Context, ElementId, HighlightStyle, InteractiveText, MouseButton, MouseDownEvent,
    MouseMoveEvent, SharedString, StyledText, TextLayout, div, prelude::*, rgb,
};

use crate::terminal_links::is_openable_url;

use super::{ChatView, widgets::Look};

/// The selected part of one run of text.
pub(super) struct Selection {
    /// Which run: the same string every time that run is drawn.
    pub key: String,
    pub anchor: usize,
    pub head: usize,
    /// What is selected, kept so that ⌘C needs nothing but this.
    pub text: String,
}

impl Selection {
    fn range(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }
}

/// `at` moved back to the start of the character it is in, and no further than the text.
fn char_start(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// The word around `at`: letters, digits and underscores, or if `at` is on something else the
/// run of that kind of character.
pub(super) fn word_at(text: &str, at: usize) -> Range<usize> {
    let at = char_start(text, at);
    let Some(here) = text[at..].chars().next() else {
        return at..at;
    };
    let kind = |c: char| {
        if c.is_alphanumeric() || c == '_' {
            0
        } else if c.is_whitespace() {
            1
        } else {
            2
        }
    };
    let wanted = kind(here);
    let start = text[..at]
        .char_indices()
        .rev()
        .take_while(|(_, c)| kind(*c) == wanted)
        .last()
        .map_or(at, |(offset, _)| offset);
    let end = text[at..]
        .char_indices()
        .find(|(_, c)| kind(*c) != wanted)
        .map_or(text.len(), |(offset, _)| at + offset);
    start..end
}

/// `highlights` (sorted, not overlapping) with `selected` laid over them: where the two meet
/// the text keeps its own style and takes the selection's background.
pub(super) fn with_selection(
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    selected: Range<usize>,
    selection: HighlightStyle,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut out = Vec::with_capacity(highlights.len() + 2);
    // Where the selection is not yet accounted for.
    let mut covered = selected.start;
    for (range, style) in highlights {
        if range.end <= selected.start || range.start >= selected.end {
            out.push((range, style));
            continue;
        }
        if range.start < selected.start {
            out.push((range.start..selected.start, style));
        }
        let inside = range.start.max(selected.start)..range.end.min(selected.end);
        if covered < inside.start {
            out.push((covered..inside.start, selection));
        }
        covered = inside.end;
        out.push((
            inside,
            HighlightStyle {
                background_color: selection.background_color,
                ..style
            },
        ));
        if range.end > selected.end {
            out.push((selected.end..range.end, style));
        }
    }
    if covered < selected.end {
        out.push((covered..selected.end, selection));
    }
    out.sort_by_key(|(range, _)| range.start);
    out
}

impl ChatView {
    /// A run of text the mouse can select in. `links` are parts of it that open an address.
    pub(super) fn selectable(
        &self,
        key: &str,
        text: String,
        mut highlights: Vec<(Range<usize>, HighlightStyle)>,
        mut links: Vec<(Range<usize>, String)>,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        for (range, target) in super::links::detect(&text, key.starts_with("code:")) {
            if links
                .iter()
                .all(|(existing, _)| existing.end <= range.start || existing.start >= range.end)
            {
                if highlights.is_empty() {
                    highlights.push((
                        range.clone(),
                        HighlightStyle {
                            color: Some(rgb(look.colors.cyan).into()),
                            ..Default::default()
                        },
                    ));
                }
                links.push((range, target));
            }
        }
        links.sort_by_key(|(range, _)| range.start);
        let selected = self
            .selection
            .as_ref()
            .filter(|selection| selection.key == key)
            .map(Selection::range)
            .filter(|range| {
                !range.is_empty()
                    && range.end <= text.len()
                    && text.is_char_boundary(range.start)
                    && text.is_char_boundary(range.end)
            });
        let highlights = match selected {
            Some(range) => with_selection(
                highlights,
                range,
                HighlightStyle {
                    background_color: Some(rgb(look.tint(look.colors.cyan, 0.4)).into()),
                    ..Default::default()
                },
            ),
            None => highlights,
        };
        let styled = StyledText::new(text.clone()).with_highlights(highlights);
        let layout = styled.layout().clone();
        let link_ranges: Vec<Range<usize>> = links.iter().map(|(range, _)| range.clone()).collect();
        let body = if links.is_empty() {
            styled.into_any_element()
        } else {
            let urls: Vec<String> = links.into_iter().map(|(_, url)| url).collect();
            let view = cx.weak_entity();
            InteractiveText::new(
                ElementId::Name(SharedString::from(format!("text:{key}"))),
                styled,
            )
            .on_click(link_ranges.clone(), move |at, _, cx| {
                if let Some(url) = urls.get(at) {
                    if is_openable_url(url) {
                        cx.open_url(url);
                    } else {
                        let _ = view.update(cx, |_, cx| {
                            cx.emit(super::ChatViewEvent::OpenFile {
                                target: url.clone(),
                            })
                        });
                    }
                }
            })
            .into_any_element()
        };
        let (down_key, down_text, down_layout) = (key.to_owned(), text.clone(), layout.clone());
        let (move_key, move_text) = (key.to_owned(), text);
        div()
            .min_w_0()
            .cursor_text()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |view, event: &MouseDownEvent, window, cx| {
                    view.focus.focus(window, cx);
                    let at = char_start(&down_text, index_at(&down_layout, event.position));
                    // A press on a link is the link's.
                    if link_ranges.iter().any(|range| range.contains(&at)) {
                        view.selection = None;
                        cx.notify();
                        return;
                    }
                    let range = match event.click_count {
                        0 | 1 => at..at,
                        2 => word_at(&down_text, at),
                        _ => 0..down_text.len(),
                    };
                    view.selection = Some(Selection {
                        key: down_key.clone(),
                        anchor: range.start,
                        head: range.end,
                        text: down_text[range].to_owned(),
                    });
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(move |view, event: &MouseMoveEvent, _, cx| {
                if !event.dragging() {
                    return;
                }
                let Some(selection) = view.selection.as_mut().filter(|s| s.key == move_key) else {
                    return;
                };
                // The run may have changed since the press, while it was streaming.
                let at = char_start(&move_text, index_at(&layout, event.position));
                let anchor = char_start(&move_text, selection.anchor);
                if selection.head != at || selection.anchor != anchor {
                    selection.anchor = anchor;
                    selection.head = at;
                    selection.text = move_text[anchor.min(at)..anchor.max(at)].to_owned();
                    cx.notify();
                }
            }))
            .child(body)
            .into_any_element()
    }

    /// Copy the selected text. Whether there was any.
    pub(super) fn copy_selection(&mut self, cx: &mut Context<Self>) -> bool {
        match self.selection.as_ref().filter(|s| !s.text.is_empty()) {
            Some(selection) => {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(selection.text.clone()));
                true
            }
            None => false,
        }
    }
}

/// The character under `position`, or the nearest one.
fn index_at(layout: &TextLayout, position: gpui::Point<gpui::Pixels>) -> usize {
    match layout.index_for_position(position) {
        Ok(at) | Err(at) => at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(color: u32) -> HighlightStyle {
        HighlightStyle {
            color: Some(rgb(color).into()),
            ..Default::default()
        }
    }

    fn marked() -> HighlightStyle {
        HighlightStyle {
            background_color: Some(rgb(0x123456).into()),
            ..Default::default()
        }
    }

    #[test]
    fn a_double_click_takes_the_word_a_symbol_run_or_a_space_run() {
        let text = "let x_1 = foo::bar(42);";
        assert_eq!(&text[word_at(text, 5)], "x_1");
        assert_eq!(&text[word_at(text, 4)], "x_1");
        assert_eq!(&text[word_at(text, 7)], " ", "the one space");
        assert_eq!(&text[word_at(text, 13)], "::");
        assert_eq!(&text[word_at(text, 8)], "=");
        assert_eq!(&text[word_at(text, 0)], "let");
        // At the end of the text, on a character in the middle of a multi-byte one, and in nothing.
        assert_eq!(word_at(text, text.len()), text.len()..text.len());
        assert_eq!(&"héllo wörld"[word_at("héllo wörld", 2)], "héllo");
        assert_eq!(word_at("", 0), 0..0);
    }

    #[test]
    fn a_selection_over_plain_text_is_one_highlight() {
        assert_eq!(
            with_selection(vec![], 3..8, marked()),
            vec![(3..8, marked())]
        );
    }

    #[test]
    fn a_selection_keeps_the_style_of_the_text_it_covers_and_takes_the_background() {
        let bold = style(1);
        let code = style(2);
        // |0..4 bold| 4..6 plain |6..10 code| 12..14 bold, selected 2..11.
        let out = with_selection(
            vec![(0..4, bold), (6..10, code), (12..14, bold)],
            2..11,
            marked(),
        );
        let marked_over = |style: HighlightStyle| HighlightStyle {
            background_color: marked().background_color,
            ..style
        };
        assert_eq!(
            out,
            vec![
                (0..2, bold),
                (2..4, marked_over(bold)),
                (4..6, marked()),
                (6..10, marked_over(code)),
                (10..11, marked()),
                (12..14, bold),
            ]
        );
        // The pieces are in order and do not overlap, as `StyledText` needs.
        for pair in out.windows(2) {
            assert!(pair[0].0.end <= pair[1].0.start, "{out:?}");
        }
    }

    #[test]
    fn a_style_that_runs_past_both_ends_of_the_selection_is_split_in_three() {
        let bold = style(1);
        let out = with_selection(vec![(0..10, bold)], 3..6, marked());
        assert_eq!(out.len(), 3);
        assert_eq!(out[0], (0..3, bold));
        assert_eq!(out[1].0, 3..6);
        assert_eq!(out[2], (6..10, bold));
    }
}
