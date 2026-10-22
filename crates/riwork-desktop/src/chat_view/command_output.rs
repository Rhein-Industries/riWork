//! Bounded terminal-style command logs. Rendered text, selection, and page copy share
//! one projection; escape sequences never become selectable content.
use super::{
    ChatView,
    widgets::{self, Look},
};
use crate::{chat::model::Item, ui_text};
use gpui::{
    AnyElement, Context, FontWeight, HighlightStyle, SharedString, div, point, prelude::*, px, rgb,
};
use gpui_kit::base::TestSupportExt as _;
use std::{
    collections::VecDeque,
    hash::{Hash, Hasher},
    ops::Range,
    sync::Arc,
};

const RAW_BYTES: usize = 256 * 1024;
const RETAINED_BYTES: usize = 128 * 1024;
const RETAINED_LINES: usize = 4_000;
const PAGE_BYTES: usize = 32 * 1024;
const PAGE_LINES: usize = 200;
const CACHE_ENTRIES: usize = 8;
const PAUSED_PANELS: usize = 64;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Style {
    fg: Option<u32>,
    bg: Option<u32>,
    bold: bool,
    reverse: bool,
}

#[derive(Clone, Copy)]
struct Glyph {
    ch: char,
    style: Style,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Anchor {
    raw_line: usize,
    byte: usize,
}

#[derive(Clone)]
struct Page {
    text: String,
    spans: Vec<(Range<usize>, Style)>,
    start: Anchor,
    end: Anchor,
    earlier: bool,
    newer: bool,
    limited: bool,
}

struct Log {
    text: String,
    spans: Vec<(Range<usize>, Style)>,
    /// Raw line starts are stable across appends, including repeated identical lines.
    lines: Vec<(usize, usize)>,
    first_line_offset: usize,
    limited: bool,
}

fn boundary(text: &str, mut at: usize) -> usize {
    while at < text.len() && !text.is_char_boundary(at) {
        at += 1;
    }
    at
}

fn ansi_color(index: u32) -> u32 {
    const BASE: [u32; 16] = [
        0x000000, 0xcd3131, 0x0dbc79, 0xe5e510, 0x2472c8, 0xbc3fbc, 0x11a8cd, 0xe5e5e5, 0x666666,
        0xf14c4c, 0x23d18b, 0xf5f543, 0x3b8eea, 0xd670d6, 0x29b8db, 0xffffff,
    ];
    match index {
        0..=15 => BASE[index as usize],
        16..=231 => {
            let n = index - 16;
            let value = |v| if v == 0 { 0 } else { 55 + v * 40 };
            (value(n / 36) << 16) | (value(n / 6 % 6) << 8) | value(n % 6)
        }
        _ => {
            let v = 8 + index.min(255).saturating_sub(232) * 10;
            v * 0x010101
        }
    }
}

fn sgr(parameters: &str, style: &mut Style) {
    let values: Vec<Option<u32>> = parameters
        .split(';')
        .take(32)
        .map(|value| {
            if value.is_empty() {
                Some(0)
            } else {
                value.parse().ok()
            }
        })
        .collect();
    let mut at = 0;
    while at < values.len() {
        match values[at] {
            Some(0) => *style = Style::default(),
            Some(1) => style.bold = true,
            Some(22) => style.bold = false,
            Some(7) => style.reverse = true,
            Some(27) => style.reverse = false,
            Some(30..=37) => style.fg = values[at].map(|n| ansi_color(n - 30)),
            Some(90..=97) => style.fg = values[at].map(|n| ansi_color(n - 90 + 8)),
            Some(40..=47) => style.bg = values[at].map(|n| ansi_color(n - 40)),
            Some(100..=107) => style.bg = values[at].map(|n| ansi_color(n - 100 + 8)),
            Some(39) => style.fg = None,
            Some(49) => style.bg = None,
            Some(code @ (38 | 48)) => {
                let (color, used) = match values.get(at + 1).copied().flatten() {
                    Some(5) => (
                        values
                            .get(at + 2)
                            .copied()
                            .flatten()
                            .filter(|n| *n <= 255)
                            .map(ansi_color),
                        2,
                    ),
                    Some(2) => {
                        let color = values.get(at + 2..at + 5).and_then(|rgb| {
                            let r = rgb[0].filter(|v| *v <= 255)?;
                            let g = rgb[1].filter(|v| *v <= 255)?;
                            let b = rgb[2].filter(|v| *v <= 255)?;
                            Some((r << 16) | (g << 8) | b)
                        });
                        (color, 4)
                    }
                    _ => (None, 1),
                };
                if let Some(color) = color {
                    if code == 38 {
                        style.fg = Some(color);
                    } else {
                        style.bg = Some(color);
                    }
                }
                at += used;
            }
            _ => {}
        }
        at += 1;
    }
}

/// This is a log reader, not a terminal emulator: supports SGR, CR, backspace,
/// tabs and erase-line. Other terminal control strings are consumed, never shown.
fn parse(raw: &str, raw_start: usize) -> Log {
    let mut lines: Vec<(usize, Vec<Glyph>)> = vec![(raw_start, Vec::new())];
    let mut style = Style::default();
    let mut cursor = 0usize;
    let mut chars = raw.char_indices().peekable();
    while let Some((at, ch)) = chars.next() {
        let line = &mut lines.last_mut().unwrap().1;
        match ch {
            '\u{1b}' => match chars.next() {
                Some((begin, '[')) => {
                    let mut end = None;
                    for (at, ch) in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&ch) {
                            end = Some((at, ch));
                            break;
                        }
                    }
                    if let Some((end, final_byte)) = end {
                        let parameters = &raw[begin + 1..end];
                        if parameters.len() <= 256 && final_byte == 'm' {
                            sgr(parameters, &mut style);
                        }
                        if final_byte == 'K' {
                            match parameters {
                                "" | "0" => line.truncate(cursor.min(line.len())),
                                "1" => {
                                    for glyph in line.iter_mut().take(cursor.saturating_add(1)) {
                                        *glyph = Glyph { ch: ' ', style };
                                    }
                                }
                                "2" => {
                                    line.clear();
                                }
                                _ => {}
                            }
                        }
                    }
                }
                Some((_, ']' | 'P' | '^' | '_' | 'X')) => {
                    while let Some((_, ch)) = chars.next() {
                        if ch == '\u{7}' {
                            break;
                        }
                        if ch == '\u{1b}' && chars.peek().is_some_and(|(_, ch)| *ch == '\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                Some((_, '(' | ')' | '*' | '+')) => {
                    chars.next();
                }
                _ => {}
            },
            '\n' => {
                lines.push((raw_start + at + 1, Vec::new()));
                cursor = 0;
            }
            '\r' => cursor = 0,
            '\u{8}' => cursor = cursor.saturating_sub(1),
            ch if ch.is_control() && ch != '\t' => {}
            ch => {
                let count = if ch == '\t' { 4 - cursor % 4 } else { 1 };
                for _ in 0..count {
                    let glyph = Glyph {
                        ch: if ch == '\t' { ' ' } else { ch },
                        style,
                    };
                    while line.len() < cursor {
                        line.push(Glyph { ch: ' ', style });
                    }
                    if let Some(existing) = line.get_mut(cursor) {
                        *existing = glyph;
                    } else {
                        line.push(glyph);
                    }
                    cursor += 1;
                }
            }
        }
    }
    while lines.len() > 1 && lines.last().unwrap().1.is_empty() {
        lines.pop();
    }
    let mut text = String::new();
    let mut spans: Vec<(Range<usize>, Style)> = Vec::new();
    let mut starts = Vec::new();
    for (at, (raw_line, glyphs)) in lines.into_iter().enumerate() {
        if at > 0 {
            text.push('\n');
        }
        starts.push((raw_line, text.len()));
        for glyph in glyphs {
            let start = text.len();
            text.push(glyph.ch);
            if glyph.style != Style::default() {
                if let Some((range, previous)) = spans.last_mut()
                    && *previous == glyph.style
                    && range.end == start
                {
                    range.end = text.len();
                } else {
                    spans.push((start..text.len(), glyph.style));
                }
            }
        }
    }
    let byte_cut = boundary(&text, text.len().saturating_sub(RETAINED_BYTES));
    let line_cut = starts.len().saturating_sub(RETAINED_LINES);
    let cut = byte_cut.max(starts.get(line_cut).map_or(0, |(_, at)| *at));
    let first_line = starts
        .partition_point(|(_, at)| *at <= cut)
        .saturating_sub(1);
    // Keep the original raw line identity even if its extremely long beginning was cut.
    let mut retained_starts: Vec<_> = starts[first_line..]
        .iter()
        .map(|(raw, at)| (*raw, at.saturating_sub(cut)))
        .collect();
    if retained_starts.is_empty() {
        retained_starts.push((raw_start, 0));
    }
    let spans = slice_spans(&spans, cut..text.len());
    Log {
        text: text[cut..].into(),
        spans,
        lines: retained_starts,
        first_line_offset: cut - starts[first_line].1,
        limited: raw_start > 0 || cut > 0,
    }
}

fn slice_spans(spans: &[(Range<usize>, Style)], range: Range<usize>) -> Vec<(Range<usize>, Style)> {
    spans
        .iter()
        .filter_map(|(span, style)| {
            let start = span.start.max(range.start);
            let end = span.end.min(range.end);
            (start < end).then_some((
                start.saturating_sub(range.start)..end.saturating_sub(range.start),
                *style,
            ))
        })
        .collect()
}

impl Log {
    fn anchor(&self, byte: usize) -> Anchor {
        let at = self
            .lines
            .partition_point(|(_, start)| *start <= byte)
            .saturating_sub(1);
        let (raw_line, start) = self.lines[at];
        Anchor {
            raw_line,
            byte: byte - start + if at == 0 { self.first_line_offset } else { 0 },
        }
    }
    fn locate(&self, anchor: Anchor) -> usize {
        self.lines
            .iter()
            .position(|(raw, _)| *raw == anchor.raw_line)
            .map_or(0, |at| {
                boundary(
                    &self.text,
                    (self.lines[at].1
                        + anchor.byte.saturating_sub(if at == 0 {
                            self.first_line_offset
                        } else {
                            0
                        }))
                    .min(
                        self.lines
                            .get(at + 1)
                            .map_or(self.text.len(), |(_, start)| start.saturating_sub(1)),
                    ),
                )
            })
    }
    fn page(&self, range: Range<usize>) -> Arc<Page> {
        Arc::new(Page {
            text: self.text[range.clone()].into(),
            spans: slice_spans(&self.spans, range.clone()),
            start: self.anchor(range.start),
            end: self.anchor(range.end),
            earlier: range.start > 0,
            newer: range.end < self.text.len(),
            limited: self.limited,
        })
    }
    fn ending(&self, end: usize) -> Arc<Page> {
        let end = end.min(self.text.len());
        let bytes = boundary(&self.text, end.saturating_sub(PAGE_BYTES));
        let lines = self.text[..end]
            .rmatch_indices('\n')
            .nth(PAGE_LINES - 1)
            .map_or(0, |(at, _)| at + 1);
        self.page(bytes.max(lines)..end)
    }
    fn starting(&self, start: usize) -> Arc<Page> {
        let start = start.min(self.text.len());
        let mut end = (start + PAGE_BYTES).min(self.text.len());
        while !self.text.is_char_boundary(end) {
            end -= 1;
        }
        let lines = self.text[start..]
            .match_indices('\n')
            .nth(PAGE_LINES - 1)
            .map_or(self.text.len(), |(at, _)| start + at);
        self.page(start..end.min(lines))
    }
}

struct Panel {
    id: String,
    source: String,
    source_start: usize,
    total: usize,
    log: Log,
    frozen: Option<Arc<Page>>,
    follow: bool,
}

impl Panel {
    fn page(&self) -> Arc<Page> {
        self.frozen
            .clone()
            .unwrap_or_else(|| self.log.ending(self.log.text.len()))
    }
    fn pause(&mut self) {
        if self.frozen.is_none() {
            self.frozen = Some(self.page());
        }
        self.follow = false;
    }
}

#[derive(Default)]
pub(super) struct Panels {
    entries: VecDeque<Panel>,
    paused: VecDeque<SavedPanel>,
    /// The last page of each folded command as Find last read it, by item id. Kept
    /// while the bar is open: Find reads every command again whenever the
    /// transcript changes, so on every streamed delta.
    folded: std::collections::HashMap<String, FoldedText>,
}

/// A folded command's text and which output it was read from: a transcript item's
/// output only changes by being replaced, which moves `replaced_at`, or by growing.
struct FoldedText {
    replaced_at: u64,
    total: usize,
    text: String,
}

struct SavedPanel {
    id: String,
    source_start: usize,
    total: usize,
    fingerprint: u64,
    page: Arc<Page>,
}

fn fingerprint(text: &str) -> u64 {
    let mut state = std::hash::DefaultHasher::new();
    text.hash(&mut state);
    state.finish()
}

impl Panels {
    fn panel(&mut self, id: &str, output: &str) -> &mut Panel {
        let (start, source) = retained_source(output);
        let previous = self
            .entries
            .iter()
            .position(|entry| entry.id == id)
            .and_then(|at| self.entries.remove(at));
        let mut panel = if let Some(mut panel) = previous {
            if panel.source_start != start || panel.source != source {
                let append = output.len() >= panel.total
                    && output.get(panel.source_start..panel.total) == Some(panel.source.as_str());
                if !append {
                    panel.frozen = None;
                    panel.follow = true;
                }
                panel.source = source.into();
                panel.source_start = start;
                panel.total = output.len();
                panel.log = parse(source, start);
            }
            panel
        } else {
            Panel {
                id: id.into(),
                source: source.into(),
                source_start: start,
                total: output.len(),
                log: parse(source, start),
                frozen: None,
                follow: true,
            }
        };
        if let Some(at) = self.paused.iter().position(|saved| saved.id == id) {
            let saved = self.paused.remove(at).unwrap();
            if output
                .get(saved.source_start..saved.total)
                .is_some_and(|text| fingerprint(text) == saved.fingerprint)
            {
                panel.frozen = Some(saved.page);
                panel.follow = false;
            }
        }
        while self.entries.len() >= CACHE_ENTRIES {
            if let Some(old) = self.entries.pop_front()
                && let Some(page) = old.frozen
            {
                while self.paused.len() >= PAUSED_PANELS {
                    self.paused.pop_front();
                }
                self.paused.push_back(SavedPanel {
                    id: old.id,
                    source_start: old.source_start,
                    total: old.total,
                    fingerprint: fingerprint(&old.source),
                    page,
                });
            }
        }
        self.entries.push_back(panel);
        self.entries.back_mut().unwrap()
    }
    fn existing(&mut self, id: &str) -> Option<&mut Panel> {
        self.entries.iter_mut().find(|panel| panel.id == id)
    }
}

fn luminance(color: u32) -> f64 {
    let channel = |shift: u32| {
        let c = ((color >> shift) & 255u32) as f64 / 255.;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
}

fn readable(color: u32, background: u32) -> u32 {
    let contrast = |color| {
        let (a, b) = (luminance(color), luminance(background));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    };
    if contrast(color) >= 4.5 {
        return color;
    }
    let destination = if luminance(background) > 0.179 {
        0
    } else {
        255
    };
    for step in 1..=16 {
        let channel = |shift: u32| {
            let old = (color >> shift) & 255u32;
            (old * (16 - step) + destination * step) / 16
        };
        let adjusted = (channel(16) << 16) | (channel(8) << 8) | channel(0);
        if contrast(adjusted) >= 4.5 {
            return adjusted;
        }
    }
    destination * 0x010101
}

fn highlights(page: &Page, look: Look) -> Vec<(Range<usize>, HighlightStyle)> {
    page.spans
        .iter()
        .map(|(range, style)| {
            let (fg, bg) = if style.reverse {
                (
                    style.bg.or(Some(look.colors.bg)),
                    style.fg.or(Some(look.colors.text)),
                )
            } else {
                (style.fg, style.bg)
            };
            let background = bg.unwrap_or(look.colors.bg);
            (
                range.clone(),
                HighlightStyle {
                    color: fg
                        .or(bg.map(|_| look.colors.text))
                        .map(|color| rgb(readable(color, background)).into()),
                    background_color: bg.map(|color| rgb(color).into()),
                    font_weight: style.bold.then_some(FontWeight::BOLD),
                    ..Default::default()
                },
            )
        })
        .collect()
}

fn retained_source(output: &str) -> (usize, &str) {
    let start = boundary(output, output.len().saturating_sub(RAW_BYTES));
    let start = if start > 0 {
        output[start..]
            .find('\n')
            .filter(|at| start + at + 1 < output.len())
            .map_or(start, |at| start + at + 1)
    } else {
        0
    };
    (start, &output[start..])
}

pub(super) fn retained_text(output: &str) -> String {
    let (start, source) = retained_source(output);
    parse(source, start).text
}

impl ChatView {
    /// Also used by visible selection and hidden Find; never return a different
    /// page merely because the command card is currently folded.
    pub(super) fn command_output_text(&self, item: &Item, output: &str) -> String {
        let mut panels = self.command_outputs.borrow_mut();
        if panels.entries.iter().any(|entry| entry.id == item.id)
            || panels.paused.iter().any(|entry| entry.id == item.id)
        {
            return panels.panel(&item.id, output).page().text.clone();
        }
        // Searching folded commands must not evict the user's live panel state.
        // Only the transcript's own output is remembered, and only as its events
        // left it; other text is read afresh.
        let replaced_at = self.model.transcript.replaced_at(&item.id);
        let current = replaced_at != 0
            && self.model.item(&item.id).is_some_and(|(_, current)| {
                matches!(&current.body, crate::chat::model::ItemBody::Command { output: own, .. }
                    if std::ptr::eq(own.as_str(), output))
            });
        if current
            && let Some(known) = panels
                .folded
                .get(&item.id)
                .filter(|known| known.replaced_at == replaced_at && known.total == output.len())
        {
            return known.text.clone();
        }
        let (start, source) = retained_source(output);
        let log = parse(source, start);
        let text = log.ending(log.text.len()).text.clone();
        if current && self.find.open {
            panels.folded.insert(
                item.id.clone(),
                FoldedText {
                    replaced_at,
                    total: output.len(),
                    text: text.clone(),
                },
            );
        }
        text
    }

    /// The bar closed: nothing reads every folded command until it opens again.
    pub(super) fn forget_folded_command_text(&self) {
        self.command_outputs.borrow_mut().folded = Default::default();
    }

    /// Find takes authority over the nested scroll position before revealing a hit.
    pub(super) fn pause_command_output(&self, id: &str) {
        if let Some(item) = self
            .model
            .transcript
            .items
            .iter()
            .find(|item| item.id == id)
            && let crate::chat::model::ItemBody::Command { output, .. } = &item.body
        {
            self.command_outputs.borrow_mut().panel(id, output).pause();
        }
    }

    fn command_output_page(&mut self, id: &str, direction: i8, cx: &mut Context<Self>) {
        let mut panels = self.command_outputs.borrow_mut();
        let Some(panel) = panels.existing(id) else {
            return;
        };
        let current = panel.page();
        if direction == 0 {
            panel.frozen = None;
            panel.follow = true;
        } else if direction < 0 {
            let end = panel.log.locate(current.start);
            let end = if end > 0 && panel.log.text.as_bytes()[end - 1] == b'\n' {
                end - 1
            } else {
                end
            };
            panel.frozen = Some(panel.log.ending(end));
            panel.follow = false;
        } else {
            let start = panel.log.locate(current.end);
            let start = if panel.log.text.as_bytes().get(start) == Some(&b'\n') {
                start + 1
            } else {
                start
            };
            panel.frozen = Some(panel.log.starting(start));
            panel.follow = false;
        }
        drop(panels);
        self.scroller(&format!("output:{id}"))
            .handle()
            .set_offset(point(px(0.), px(0.)));
        self.transcript_selection.changed();
        self.invalidate_leaf_owner(id);
        self.list.remeasure();
        cx.notify();
    }

    pub(super) fn command_output_panel(
        &self,
        item: &Item,
        command: &str,
        output: &str,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (page, following, newer) = {
            let mut panels = self.command_outputs.borrow_mut();
            let panel = panels.panel(&item.id, output);
            let page = panel.page();
            let newer = page.newer || panel.log.locate(page.end) < panel.log.text.len();
            (page, panel.follow, newer)
        };
        let id = item.id.clone();
        let handle = self.scroller(&format!("output:{id}")).handle().clone();
        if following {
            handle.scroll_to_bottom();
        }
        let wheel_handle = handle.clone();
        let wheel_id = id.clone();
        let page_is_latest = !newer;
        let action = |name: &'static str,
                      label: &'static str,
                      direction: i8,
                      disabled: bool,
                      cx: &mut Context<Self>| {
            let id = id.clone();
            widgets::button(
                SharedString::from(format!("command-{name}:{id}")),
                label,
                None,
                look,
            )
            .disabled(disabled)
            .on_click(
                cx.listener(move |view, _, _, cx| view.command_output_page(&id, direction, cx)),
            )
        };
        let copy_key = format!("copy-command-page:{id}");
        let retained_key = format!("copy-command-retained:{id}");
        let page_copy = page.clone();
        let retained_id = id.clone();
        let ranges = highlights(&page, look);
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .when(command.trim().contains('\n'), |body| {
                body.child(
                    div()
                        .px(ui_text::space(10.))
                        .py(ui_text::space(6.))
                        .text_size(ui_text::text(11.))
                        .font_family(ui_text::code_family())
                        .text_color(rgb(look.colors.cyan))
                        .border_b_1()
                        .border_color(rgb(look.colors.divider))
                        .child(self.selectable(
                            &format!("command:{id}"),
                            command.trim().into(),
                            Vec::new(),
                            Vec::new(),
                            look,
                            cx,
                        )),
                )
            })
            .child(
                div()
                    .id(SharedString::from(format!("command-status:{id}")))
                    .px(ui_text::space(10.))
                    .pt(ui_text::space(4.))
                    .text_size(ui_text::text(10.))
                    .text_color(rgb(look.colors.muted))
                    .child(format!(
                        "{} · {} lines in this page",
                        if following {
                            "Following output"
                        } else {
                            "Paused"
                        },
                        page.text.lines().count()
                    ))
                    .test_support(),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(ui_text::space(5.))
                    .px(ui_text::space(10.))
                    .py(ui_text::space(4.))
                    .child(action("earlier", "Earlier", -1, !page.earlier, cx))
                    .child(action("newer", "Newer", 1, !newer, cx))
                    .child(action("latest", "Latest", 0, following && !newer, cx))
                    .child(
                        widgets::copy_button(
                            SharedString::from(copy_key.clone()),
                            self.copied.as_deref() == Some(copy_key.as_str()),
                            "Copy page",
                            "Copy displayed output page",
                            look,
                        )
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.copy(copy_key.clone(), page_copy.text.clone(), cx)
                        })),
                    )
                    .child(
                        widgets::copy_button(
                            SharedString::from(retained_key.clone()),
                            self.copied.as_deref() == Some(retained_key.as_str()),
                            "Copy retained",
                            "Copy retained output (up to 128 KiB)",
                            look,
                        )
                        .on_click(cx.listener(move |view, _, _, cx| {
                            let text = view
                                .command_outputs
                                .borrow_mut()
                                .existing(&retained_id)
                                .map(|panel| panel.log.text.clone());
                            if let Some(text) = text {
                                view.copy(retained_key.clone(), text, cx);
                            }
                        })),
                    ),
            )
            .children(page.limited.then(|| {
                div()
                    .px(ui_text::space(10.))
                    .pb(ui_text::space(4.))
                    .text_size(ui_text::text(10.))
                    .text_color(rgb(look.colors.muted))
                    .child("Older output is outside the retained history (128 KiB / 4,000 lines).")
            }))
            .child(
                div()
                    .id(SharedString::from(format!("output:{id}")))
                    .w_full()
                    .max_h(ui_text::space(280.))
                    .flex()
                    .items_start()
                    .overflow_scroll()
                    .track_scroll(&handle)
                    .on_scroll_wheel(cx.listener(move |view, _, _, cx| {
                        let at_end =
                            wheel_handle.max_offset().y + wheel_handle.offset().y <= px(8.);
                        if let Some(panel) = view.command_outputs.borrow_mut().existing(&wheel_id) {
                            if page_is_latest && at_end {
                                panel.follow = true;
                                panel.frozen = None;
                            } else {
                                panel.pause();
                            }
                        }
                        // A command's wheel never moves the containing transcript.
                        cx.stop_propagation();
                        cx.notify();
                    }))
                    .px(ui_text::space(10.))
                    .py(ui_text::space(6.))
                    .bg(rgb(look.colors.bg))
                    .text_size(ui_text::text(11.))
                    .font_family(ui_text::code_family())
                    .child(div().flex_none().min_w_full().whitespace_nowrap().child(
                        self.selectable(
                            &format!("output:{id}"),
                            page.text.clone(),
                            ranges,
                            Vec::new(),
                            look,
                            cx,
                        ),
                    ))
                    .test_support(),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::model::{ItemBody, ItemStatus, Presentation};
    use gpui::{ScrollDelta, TestAppContext};
    use gpui_kit::test::TestWindowExt;

    fn command(id: &str, output: &str) -> Item {
        Item {
            started_at: None,
            ended_at: None,
            id: id.into(),
            turn_id: Some("turn".into()),
            status: ItemStatus::InProgress,
            presentation: Presentation::default(),
            body: ItemBody::Command {
                command: "cargo test".into(),
                cwd: None,
                output: output.into(),
                exit_code: None,
            },
        }
    }

    fn style_at(log: &Log, needle: &str) -> Style {
        let at = log.text.find(needle).unwrap();
        log.spans
            .iter()
            .find(|(range, _)| range.contains(&at))
            .map_or(Style::default(), |(_, style)| *style)
    }

    #[test]
    fn terminal_styles_reset_and_progress_controls_preserve_copy_text() {
        let log = parse(
            "\x1b[1;31mRED\x1b[22m still red\x1b[0m normal\nDownloading 100%\rDone\x1b[K\n\x1b[38;2;12;34;56m🦀 café\x1b[39m\n\x1b]0;hidden title\x07a\tb\nabcdef\rxy",
            0,
        );
        assert_eq!(
            log.text,
            "RED still red normal\nDone\n🦀 café\na   b\nxycdef"
        );
        assert_eq!(
            style_at(&log, "RED"),
            Style {
                fg: Some(ansi_color(1)),
                bold: true,
                ..Default::default()
            }
        );
        assert_eq!(
            style_at(&log, "still"),
            Style {
                fg: Some(ansi_color(1)),
                ..Default::default()
            }
        );
        assert_eq!(style_at(&log, "normal"), Style::default());
        assert_eq!(
            style_at(&log, "🦀"),
            Style {
                fg: Some(0x0c2238),
                ..Default::default()
            }
        );
        assert_eq!(parse("10%\r50%\r100%\r\ndone", 0).text, "100%\ndone");
        assert_eq!(parse("wrong\x08\x08ht", 0).text, "wroht");
    }

    #[test]
    fn incomplete_control_sequences_never_leak_and_snapshot_replacements_reset() {
        let input = "start \x1b[1;38;5;196mred 🦀\x1b[0m\x1b]8;;https://example.test\x1b\\link\x1b]8;;\x1b\\ end";
        let mut panels = Panels::default();
        for end in input.char_indices().map(|(at, _)| at).chain([input.len()]) {
            let text = panels.panel("command", &input[..end]).page().text.clone();
            assert!(!text.contains('\x1b'));
            assert!(!text.contains("example.test"));
        }
        assert_eq!(
            panels.panel("command", input).page().text,
            "start red 🦀link end"
        );
        panels.existing("command").unwrap().pause();
        let panel = panels.panel("command", "replacement");
        assert!(panel.follow);
        assert!(panel.frozen.is_none());
        assert_eq!(panel.page().text, "replacement");
    }

    #[test]
    fn retention_and_pages_bound_work_without_losing_available_earlier_lines() {
        let input = (0..450)
            .map(|n| format!("line {n:03} café 🦀"))
            .collect::<Vec<_>>()
            .join("\n");
        let log = parse(&input, 0);
        let last = log.ending(log.text.len());
        assert_eq!(last.text.lines().count(), 200);
        assert!(last.text.starts_with("line 250"));
        let previous = log.ending(log.locate(last.start) - 1);
        let first = log.ending(log.locate(previous.start) - 1);
        assert_eq!(
            [
                first.text.as_str(),
                previous.text.as_str(),
                last.text.as_str()
            ]
            .join("\n"),
            input
        );
        let long = format!("{}\n", "🦀".repeat(RAW_BYTES));
        let mut panels = Panels::default();
        let panel = panels.panel("long", &long);
        assert!(panel.source.len() <= RAW_BYTES);
        assert!(panel.log.text.len() <= RETAINED_BYTES);
        assert!(!panel.log.text.is_empty());
        let page = panel.page();
        assert!(page.text.len() <= PAGE_BYTES && page.limited);
        assert!(page.text.chars().all(|ch| ch == '🦀'));
        let many_lines = "x\n".repeat(RETAINED_LINES * 3);
        assert!(parse(&many_lines, 0).text.lines().count() <= RETAINED_LINES);
    }

    #[test]
    fn terminal_colors_remain_readable_on_light_dark_and_colored_backgrounds() {
        for background in [0xffffff, 0x000000, 0x162564, 0xeee9df] {
            for index in 0..256 {
                let color = readable(ansi_color(index), background);
                let (a, b) = (luminance(color), luminance(background));
                assert!((a.max(b) + 0.05) / (a.min(b) + 0.05) >= 4.5);
            }
        }
    }

    #[gpui::test]
    fn panel_follow_pause_pages_and_copy_are_independent_of_transcript_scroll(
        cx: &mut TestAppContext,
    ) {
        let (handle, view, recording) = super::super::editor_tests::mount_selection(cx);
        let raw = (0..300)
            .map(|n| format!("\x1b[32mline {n:03}\x1b[0m café 🦀"))
            .collect::<Vec<_>>()
            .join("\n");
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |view, cx| {
                view.model.link = super::super::Link::Live;
                view.display_mode = super::super::DisplayMode::Verbose;
                view.model.transcript.items = vec![command("build", &raw)];
                // The call row opens through its projected disclosure.
                view.disclosure.overrides.insert("build".into(), true);
                view.refresh_projection(cx);
                view.list.pause_following_tail();
                view.list.scroll_to(gpui::ListOffset {
                    item_ix: 0,
                    offset_in_item: px(0.),
                });
                cx.notify();
            });
            window.render_frame(cx);
            let scroll = view.read(cx).scroller("output:build").handle().clone();
            assert!(scroll.max_offset().y > px(0.));
            assert!(scroll.max_offset().y + scroll.offset().y <= px(1.));
            let outer = view.read(cx).list.logical_scroll_top();
            window.scroll(
                "output:build",
                ScrollDelta::Pixels(point(px(0.), px(80.))),
                cx,
            );
            assert!(
                !view
                    .read(cx)
                    .command_outputs
                    .borrow_mut()
                    .existing("build")
                    .unwrap()
                    .follow
            );
            let offset = scroll.offset();
            let frozen = view
                .read(cx)
                .command_outputs
                .borrow_mut()
                .existing("build")
                .unwrap()
                .page()
                .text
                .clone();
            view.update(cx, |view, cx| {
                let ItemBody::Command { output, .. } = &mut view.model.transcript.items[0].body
                else {
                    unreachable!()
                };
                output.push_str("\n\x1b[1;31mnew failure\x1b[0m");
                view.sync_list(
                    &super::super::Applied {
                        touched: [0].into_iter().collect(),
                        ..Default::default()
                    },
                    cx,
                );
                cx.notify();
            });
            window.render_frame(cx);
            assert_eq!(scroll.offset(), offset);
            let outer_after = view.read(cx).list.logical_scroll_top();
            assert_eq!(
                (outer_after.item_ix, outer_after.offset_in_item),
                (outer.item_ix, outer.offset_in_item)
            );
            assert_eq!(
                view.read(cx)
                    .transcript_selection
                    .with_selection_source(|source| source
                        .iter()
                        .find(|leaf| leaf.key == "output:build")
                        .unwrap()
                        .text
                        .clone()),
                frozen
            );
            window.click("copy-command-page:build", cx);
            assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), frozen);
            window.click("command-earlier:build", cx);
            window.render_frame(cx);
            window.click("copy-command-page:build", cx);
            let earlier = cx.read_from_clipboard().unwrap().text().unwrap();
            assert!(earlier.starts_with("line 000"));
            assert!(earlier.ends_with("line 099 café 🦀"));
            window.click("command-latest:build", cx);
            window.render_frame(cx);
            assert!(
                view.read(cx)
                    .command_outputs
                    .borrow_mut()
                    .existing("build")
                    .unwrap()
                    .follow
            );
            assert!(scroll.max_offset().y + scroll.offset().y <= px(1.));
            window.click("copy-command-retained:build", cx);
            let retained = cx.read_from_clipboard().unwrap().text().unwrap();
            assert!(retained.starts_with("line 000") && retained.ends_with("new failure"));
            assert!(!retained.contains('\x1b'));
        })
        .unwrap();
        assert!(recording.try_recv().is_err());
    }

    #[gpui::test]
    fn hidden_search_and_parse_cache_rotation_preserve_a_paused_page(cx: &mut TestAppContext) {
        let (handle, view, _) = super::super::editor_tests::mount_selection(cx);
        cx.update_window(handle.into(), |_, _, cx| {
            let output = (0..400).map(|n| format!("line {n}\n")).collect::<String>();
            view.update(cx, |view, _| {
                let mut panels = view.command_outputs.borrow_mut();
                panels.panel("reader", &output).pause();
            });
            let item = command("reader", &output);
            let expected = view.read(cx).command_output_text(&item, &output);
            for n in 0..CACHE_ENTRIES * 4 {
                let hidden = command(&format!("hidden-{n}"), "hidden text");
                assert_eq!(
                    view.read(cx).command_output_text(&hidden, "hidden text"),
                    "hidden text"
                );
            }
            assert_eq!(view.read(cx).command_outputs.borrow().entries.len(), 1);
            for n in 0..CACHE_ENTRIES * 2 {
                view.read(cx)
                    .command_outputs
                    .borrow_mut()
                    .panel(&format!("rendered-{n}"), "another log");
            }
            assert_eq!(
                view.read(cx).command_outputs.borrow().entries.len(),
                CACHE_ENTRIES
            );
            assert_eq!(
                view.read(cx)
                    .command_output_text(&item, &format!("{output}appended")),
                expected
            );
            assert!(
                !view
                    .read(cx)
                    .command_outputs
                    .borrow_mut()
                    .existing("reader")
                    .unwrap()
                    .follow
            );
        })
        .unwrap();
    }

    /// While Find is open a folded command is read once, and read again as soon as its
    /// output grows or is replaced, even by output of the same length.
    #[gpui::test]
    fn find_reads_a_folded_command_again_only_when_its_output_changes(cx: &mut TestAppContext) {
        use crate::chat::{
            model::{ChatEvent, Delta},
            wire::Envelope,
        };
        let (_, view, _) = super::super::editor_tests::mount_selection(cx);
        view.update(cx, |view, _| {
            let mut seq = 0;
            let mut fold = |view: &mut ChatView, event| {
                seq += 1;
                view.model.apply(&[Envelope {
                    chat_id: "chat".into(),
                    seq,
                    event,
                }]);
            };
            let read = |view: &ChatView| {
                let (_, item) = view.model.item("build").unwrap();
                let ItemBody::Command { output, .. } = &item.body else {
                    unreachable!()
                };
                view.command_output_text(item, output)
            };
            view.find.open = true;
            fold(
                view,
                ChatEvent::ItemStarted {
                    item: command("build", "one\n"),
                },
            );
            assert_eq!(read(view), "one");
            assert_eq!(view.command_outputs.borrow().folded.len(), 1);
            fold(
                view,
                ChatEvent::ItemDelta {
                    item_id: "build".into(),
                    delta: Delta::Output("two\n".into()),
                },
            );
            assert_eq!(read(view), "one\ntwo");
            fold(
                view,
                ChatEvent::ItemCompleted {
                    item: command("build", "six\nten\n"),
                },
            );
            assert_eq!(read(view), "six\nten");
            // Remembered: the same answer without reading the output again.
            view.command_outputs
                .borrow_mut()
                .folded
                .get_mut("build")
                .unwrap()
                .text = "remembered".into();
            assert_eq!(read(view), "remembered");
            // Text that is not the item's own output is never answered from memory.
            let (_, item) = view.model.item("build").unwrap();
            assert_eq!(view.command_output_text(item, "six\nten\n"), "six\nten");
            // Nothing is kept once the bar closes.
            view.find.open = false;
            view.forget_folded_command_text();
            assert!(view.command_outputs.borrow().folded.is_empty());
            assert_eq!(read(view), "six\nten");
            assert!(view.command_outputs.borrow().folded.is_empty());
        });
    }
}
