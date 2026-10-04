//! A small Markdown reader for what agents write: headings, paragraphs, emphasis, inline and
//! fenced code, bullet and numbered lists (nested), links, block quotes, rules and tables.
//!
//! It reads CommonMark loosely, because the text arrives while it streams. An unclosed code
//! fence runs to the end of the text, a `**` that never closes is two asterisks, and a line
//! break inside a paragraph stays a line break, as the agents' own terminals show it. Nothing
//! here depends on the GUI.

/// How a run of text is set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub strike: bool,
}

/// A run of text in one style, perhaps a link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: Style,
    pub link: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Heading {
        level: u8,
        spans: Vec<Span>,
    },
    Paragraph(Vec<Span>),
    Code {
        language: Option<String>,
        text: String,
    },
    /// `start` is the first number of a numbered list.
    List {
        start: Option<u64>,
        items: Vec<Vec<Block>>,
    },
    Quote(Vec<Block>),
    Table {
        align: Vec<Align>,
        header: Vec<Vec<Span>>,
        rows: Vec<Vec<Vec<Span>>>,
    },
    Rule,
}

/// Blocks nest (a quote in a list in a quote); past this the rest is plain text.
const MAX_DEPTH: usize = 8;
/// A paragraph longer than this is set as plain text: finding the end of every `*` in a
/// pathological one would take longer than the message is worth.
const MAX_STYLED: usize = 16 * 1024;

pub fn parse(source: &str) -> Vec<Block> {
    let lines: Vec<String> = source
        .lines()
        .map(|line| line.replace('\t', "    "))
        .collect();
    blocks(&lines, 0)
}

/// The concatenated text of spans, which is what the screen shows.
pub fn plain_text(spans: &[Span]) -> String {
    spans.iter().map(|span| span.text.as_str()).collect()
}

// ---------------------------------------------------------------------------------------
// Blocks
// ---------------------------------------------------------------------------------------

fn blocks(lines: &[String], depth: usize) -> Vec<Block> {
    let mut out = Vec::new();
    let mut paragraph: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].as_str();
        if line.trim().is_empty() {
            flush(&mut paragraph, &mut out);
            i += 1;
            continue;
        }
        if depth >= MAX_DEPTH {
            paragraph.push(line);
            i += 1;
            continue;
        }
        if let Some(fence) = fence_open(line) {
            flush(&mut paragraph, &mut out);
            let mut code = Vec::new();
            i += 1;
            while i < lines.len() && !fence_closes(&lines[i], &fence) {
                code.push(unindent(&lines[i], fence.indent));
                i += 1;
            }
            // The closing fence, or the end of a message that is still streaming.
            i += 1;
            out.push(Block::Code {
                language: fence.language,
                text: code.join("\n"),
            });
            continue;
        }
        if let Some((level, text)) = heading(line) {
            flush(&mut paragraph, &mut out);
            out.push(Block::Heading {
                level,
                spans: inline(text),
            });
            i += 1;
            continue;
        }
        if is_rule(line) {
            flush(&mut paragraph, &mut out);
            out.push(Block::Rule);
            i += 1;
            continue;
        }
        if quote_content(line).is_some() {
            flush(&mut paragraph, &mut out);
            let mut inner = Vec::new();
            while i < lines.len() {
                match quote_content(&lines[i]) {
                    Some(content) => inner.push(content.to_owned()),
                    // A line of the same paragraph may skip the `>`.
                    None if !lines[i].trim().is_empty()
                        && inner.last().is_some_and(|last| !last.trim().is_empty())
                        && !starts_block(&lines[i]) =>
                    {
                        inner.push(lines[i].clone())
                    }
                    None => break,
                }
                i += 1;
            }
            out.push(Block::Quote(blocks(&inner, depth + 1)));
            continue;
        }
        if paragraph.is_empty()
            && let Some((table, next)) = table(lines, i)
        {
            out.push(table);
            i = next;
            continue;
        }
        if marker(line).is_some() {
            flush(&mut paragraph, &mut out);
            let (list, next) = list(lines, i, depth);
            out.push(list);
            i = next;
            continue;
        }
        paragraph.push(line);
        i += 1;
    }
    flush(&mut paragraph, &mut out);
    out
}

fn flush(paragraph: &mut Vec<&str>, out: &mut Vec<Block>) {
    if paragraph.is_empty() {
        return;
    }
    let text = paragraph
        .iter()
        .map(|line| line.trim())
        .collect::<Vec<_>>()
        .join("\n");
    paragraph.clear();
    out.push(Block::Paragraph(inline(&text)));
}

/// Whether `line` starts something other than a paragraph.
fn starts_block(line: &str) -> bool {
    fence_open(line).is_some()
        || heading(line).is_some()
        || is_rule(line)
        || quote_content(line).is_some()
        || marker(line).is_some()
}

fn indent_of(line: &str) -> usize {
    line.chars().take_while(|c| *c == ' ').count()
}

fn unindent(line: &str, columns: usize) -> String {
    let spaces = indent_of(line).min(columns);
    line[spaces..].to_owned()
}

struct Fence {
    indent: usize,
    mark: char,
    length: usize,
    language: Option<String>,
}

fn fence_open(line: &str) -> Option<Fence> {
    let indent = indent_of(line);
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let mark = rest.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let length = rest.chars().take_while(|c| *c == mark).count();
    if length < 3 {
        return None;
    }
    let info = rest[length..].trim();
    // An info string may not hold a backtick: that is inline code, not a fence.
    if mark == '`' && info.contains('`') {
        return None;
    }
    let language = info
        .split_whitespace()
        .next()
        .map(str::to_owned)
        .filter(|language| !language.is_empty());
    Some(Fence {
        indent,
        mark,
        length,
        language,
    })
}

fn fence_closes(line: &str, fence: &Fence) -> bool {
    if indent_of(line) > 3 {
        return false;
    }
    let rest = line.trim();
    let length = rest.chars().take_while(|c| *c == fence.mark).count();
    length >= fence.length && rest[length..].trim().is_empty()
}

fn heading(line: &str) -> Option<(u8, &str)> {
    let indent = indent_of(line);
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let level = rest.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let text = &rest[level..];
    if !text.is_empty() && !text.starts_with(' ') {
        return None;
    }
    let text = text.trim();
    // A closing run of `#` is decoration.
    let without_closing = text.trim_end_matches('#');
    let text = if without_closing.len() < text.len() && without_closing.ends_with(' ') {
        without_closing.trim_end()
    } else {
        text
    };
    Some((level as u8, text))
}

fn is_rule(line: &str) -> bool {
    if indent_of(line) > 3 {
        return false;
    }
    let compact: Vec<char> = line.chars().filter(|c| !c.is_whitespace()).collect();
    compact.len() >= 3
        && matches!(compact[0], '-' | '*' | '_')
        && compact.iter().all(|c| *c == compact[0])
}

/// What follows the `>` of a quote line.
fn quote_content(line: &str) -> Option<&str> {
    let indent = indent_of(line);
    if indent > 3 {
        return None;
    }
    let rest = line[indent..].strip_prefix('>')?;
    Some(rest.strip_prefix(' ').unwrap_or(rest))
}

/// A list marker at the start of a line.
struct Marker {
    indent: usize,
    ordered: Option<u64>,
    /// Where the item's content starts, in columns of the line.
    content: usize,
}

fn marker(line: &str) -> Option<Marker> {
    let indent = indent_of(line);
    let rest = &line[indent..];
    let (ordered, width) = match rest.chars().next()? {
        '-' | '*' | '+' => (None, 1),
        '0'..='9' => {
            let digits = rest.chars().take_while(char::is_ascii_digit).count();
            let after = rest[digits..].chars().next()?;
            if digits > 9 || !matches!(after, '.' | ')') {
                return None;
            }
            (Some(rest[..digits].parse().ok()?), digits + 1)
        }
        _ => return None,
    };
    let after = &rest[width..];
    if !after.is_empty() && !after.starts_with(' ') {
        return None;
    }
    if is_rule(line) {
        return None;
    }
    let spaces = indent_of(after);
    // Five spaces or more after the marker is code in the item, not spacing.
    let spaces = if after.trim().is_empty() || spaces > 4 {
        1
    } else {
        spaces
    };
    Some(Marker {
        indent,
        ordered,
        content: indent + width + spaces,
    })
}

/// The list that starts at `lines[start]` and the line after it.
fn list(lines: &[String], start: usize, depth: usize) -> (Block, usize) {
    let first = marker(&lines[start]).expect("a list starts at a marker");
    let ordered = first.ordered.is_some();
    let mut items = Vec::new();
    let mut i = start;
    while i < lines.len() {
        let Some(item) = marker(&lines[i]) else {
            break;
        };
        // A marker of the other kind that is indented is a list inside the previous item,
        // which agents write with fewer spaces than CommonMark asks for; at the list's own
        // indent it ends the list.
        if item.ordered.is_some() != ordered || item.indent >= first.content {
            break;
        }
        let mut body = vec![lines[i][item.content.min(lines[i].len())..].to_owned()];
        i += 1;
        while i < lines.len() {
            let line = lines[i].as_str();
            if line.trim().is_empty() {
                // Blank lines belong to the item only if more of it follows.
                let next = lines[i..].iter().position(|l| !l.trim().is_empty());
                match next.map(|n| &lines[i + n]) {
                    Some(next) if indent_of(next) >= item.content => {
                        body.push(String::new());
                        i += 1;
                    }
                    _ => break,
                }
            } else if indent_of(line) >= item.content {
                body.push(unindent(line, item.content));
                i += 1;
            } else if let Some(sibling) = marker(line) {
                // A shallower marker is the next item (or another list); a deeper one of
                // the other kind is nested in this item.
                if sibling.ordered.is_some() != ordered && sibling.indent > item.indent {
                    body.push(line[sibling.indent..].to_owned());
                    i += 1;
                } else {
                    break;
                }
            } else if starts_block(line) || body.last().is_some_and(|l| l.trim().is_empty()) {
                break;
            } else {
                // A line of the item's paragraph that skips the indent.
                body.push(line.trim_start().to_owned());
                i += 1;
            }
        }
        items.push(blocks(&body, depth + 1));
        // Blank lines between items do not end the list.
        let mut next = i;
        while next < lines.len() && lines[next].trim().is_empty() {
            next += 1;
        }
        if next < lines.len()
            && marker(&lines[next]).is_some_and(|m| {
                m.ordered.is_some() == ordered
                    && m.indent < first.content
                    && m.indent >= first.indent.saturating_sub(3)
            })
        {
            i = next;
        } else {
            break;
        }
    }
    (
        Block::List {
            start: first.ordered,
            items,
        },
        i,
    )
}

// ---------------------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------------------

fn table(lines: &[String], start: usize) -> Option<(Block, usize)> {
    let header = lines.get(start)?;
    let delimiter = lines.get(start + 1)?;
    if !header.contains('|') {
        return None;
    }
    let head = split_cells(header);
    let align = delimiter_row(delimiter)?;
    if head.len() != align.len() {
        return None;
    }
    let mut rows = Vec::new();
    let mut i = start + 2;
    while i < lines.len() && !lines[i].trim().is_empty() && lines[i].contains('|') {
        let mut cells = split_cells(&lines[i]);
        cells.resize(head.len(), String::new());
        rows.push(cells.iter().map(|cell| inline(cell)).collect());
        i += 1;
    }
    Some((
        Block::Table {
            align,
            header: head.iter().map(|cell| inline(cell)).collect(),
            rows,
        },
        i,
    ))
}

/// The cells of a table row, without the outer pipes. A `\|` and a pipe in code stay in the cell.
fn split_cells(line: &str) -> Vec<String> {
    let line = line.trim();
    let line = line.strip_prefix('|').unwrap_or(line);
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut in_code = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                chars.next();
                cell.push('|');
            }
            '`' => {
                in_code = !in_code;
                cell.push(c);
            }
            '|' if !in_code => cells.push(std::mem::take(&mut cell).trim().to_owned()),
            _ => cell.push(c),
        }
    }
    let last = cell.trim().to_owned();
    // A trailing pipe leaves nothing after it.
    if !last.is_empty() || !line.ends_with('|') {
        cells.push(last);
    }
    cells
}

fn delimiter_row(line: &str) -> Option<Vec<Align>> {
    let cells = split_cells(line);
    if cells.is_empty() {
        return None;
    }
    cells
        .iter()
        .map(|cell| {
            let left = cell.starts_with(':');
            let right = cell.ends_with(':');
            let dashes = cell.trim_matches(':');
            (!dashes.is_empty() && dashes.chars().all(|c| c == '-')).then_some(
                match (left, right) {
                    (true, true) => Align::Center,
                    (false, true) => Align::Right,
                    _ => Align::Left,
                },
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------------------
// Inline
// ---------------------------------------------------------------------------------------

/// The spans of a run of text, adjacent ones in one style merged.
pub fn inline(text: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    if text.len() > MAX_STYLED {
        push(&mut spans, text, Style::default(), None);
    } else {
        inline_into(text, Style::default(), None, &mut spans, 0);
    }
    spans
}

fn push(spans: &mut Vec<Span>, text: &str, style: Style, link: Option<&str>) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = spans.last_mut()
        && last.style == style
        && last.link.as_deref() == link
    {
        last.text.push_str(text);
        return;
    }
    spans.push(Span {
        text: text.to_owned(),
        style,
        link: link.map(str::to_owned),
    });
}

fn inline_into(text: &str, style: Style, link: Option<&str>, out: &mut Vec<Span>, depth: usize) {
    let mut plain = String::new();
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        let c = rest.chars().next().expect("not at the end");
        let width = c.len_utf8();
        let before = text[..i].chars().next_back();
        match c {
            '\\' => match rest[1..].chars().next() {
                Some(next) if next.is_ascii_punctuation() => {
                    plain.push(next);
                    i += 1 + next.len_utf8();
                }
                _ => {
                    plain.push(c);
                    i += width;
                }
            },
            '`' => {
                let run = rest.chars().take_while(|c| *c == '`').count();
                match find_run(&rest[run..], '`', run) {
                    Some(end) => {
                        push(out, &plain, style, link);
                        plain.clear();
                        let code = &rest[run..run + end];
                        // One space on each side is padding, not part of the code.
                        let padded = code.len() >= 2
                            && code.starts_with(' ')
                            && code.ends_with(' ')
                            && !code.trim().is_empty();
                        let code = if padded {
                            &code[1..code.len() - 1]
                        } else {
                            code
                        };
                        let code_style = Style {
                            code: true,
                            ..style
                        };
                        push(out, &code.replace('\n', " "), code_style, link);
                        i += run + end + run;
                    }
                    None => {
                        plain.push_str(&rest[..run]);
                        i += run;
                    }
                }
            }
            '!' if rest[1..].starts_with('[') && depth < MAX_DEPTH => {
                // An image is its description, as a link to the image.
                match bracketed_link(&rest[1..]) {
                    Some((label, destination, used)) => {
                        push(out, &plain, style, link);
                        plain.clear();
                        let label = if label.is_empty() { "image" } else { label };
                        inline_into(label, style, Some(destination), out, depth + 1);
                        i += 1 + used;
                    }
                    None => {
                        plain.push(c);
                        i += width;
                    }
                }
            }
            '[' if depth < MAX_DEPTH => match bracketed_link(rest) {
                Some((label, destination, used)) => {
                    push(out, &plain, style, link);
                    plain.clear();
                    inline_into(label, style, Some(destination), out, depth + 1);
                    i += used;
                }
                None => {
                    plain.push(c);
                    i += width;
                }
            },
            '<' => match autolink(rest) {
                Some((url, used)) => {
                    push(out, &plain, style, link);
                    plain.clear();
                    push(out, url, style, Some(url));
                    i += used;
                }
                None => {
                    plain.push(c);
                    i += width;
                }
            },
            '*' | '_' | '~' if depth < MAX_DEPTH => {
                let run = rest.chars().take_while(|d| *d == c).count();
                match emphasis(rest, c, run, before) {
                    Some((inner, marks, used)) => {
                        push(out, &plain, style, link);
                        plain.clear();
                        let mut next = style;
                        match (c, marks) {
                            ('~', _) => next.strike = true,
                            (_, 1) => next.italic = true,
                            (_, 2) => next.bold = true,
                            _ => {
                                next.bold = true;
                                next.italic = true;
                            }
                        }
                        inline_into(inner, next, link, out, depth + 1);
                        i += used;
                    }
                    None => {
                        plain.push_str(&rest[..run]);
                        i += run;
                    }
                }
            }
            'h' if link.is_none() && !before.is_some_and(char::is_alphanumeric) => {
                match bare_url(rest) {
                    Some(url) => {
                        push(out, &plain, style, link);
                        plain.clear();
                        push(out, url, style, Some(url));
                        i += url.len();
                    }
                    None => {
                        plain.push(c);
                        i += width;
                    }
                }
            }
            _ => {
                plain.push(c);
                i += width;
            }
        }
    }
    push(out, &plain, style, link);
}

/// The offset of the first run of exactly `length` `mark`s in `text`.
fn find_run(text: &str, mark: char, length: usize) -> Option<usize> {
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        if rest.starts_with(mark) {
            let run = rest.chars().take_while(|c| *c == mark).count();
            if run == length {
                return Some(i);
            }
            i += run;
        } else {
            i += rest.chars().next().map_or(1, char::len_utf8);
        }
    }
    None
}

/// An emphasis, strong or strike-through run opened at the start of `rest` by `run` marks
/// of `mark`: its inner text, how many marks open it and how much text it takes in all.
fn emphasis(
    rest: &str,
    mark: char,
    run: usize,
    before: Option<char>,
) -> Option<(&str, usize, usize)> {
    let marks = if mark == '~' {
        if run != 2 {
            return None;
        }
        2
    } else {
        run.min(3)
    };
    let after_open = &rest[marks..];
    // A run followed by a space is just characters: `2 * 3`.
    if after_open.chars().next().is_none_or(char::is_whitespace) {
        return None;
    }
    // Underscores inside a word (`snake_case`) are not emphasis.
    if mark == '_' && before.is_some_and(char::is_alphanumeric) {
        return None;
    }
    // An opener of the other length inside the text: `**bold *and italic***` closes the
    // inner emphasis with the first star of the last run and its own with the other two.
    let inner_marks = 3 - marks.min(2);
    let mut inner_open = false;
    let mut i = 0;
    while i < after_open.len() {
        let here = &after_open[i..];
        let c = here.chars().next()?;
        if c == '`' {
            // Code spans hide their asterisks.
            let ticks = here.chars().take_while(|c| *c == '`').count();
            match find_run(&here[ticks..], '`', ticks) {
                Some(end) => i += ticks + end + ticks,
                None => i += ticks,
            }
            continue;
        }
        if c == '\\' {
            i += 1 + here[1..].chars().next().map_or(0, char::len_utf8);
            continue;
        }
        if c == mark {
            let closing = here.chars().take_while(|d| *d == mark).count();
            let previous = after_open[..i].chars().next_back();
            let can_close = i > 0 && previous.is_some_and(|p| !p.is_whitespace());
            let can_open = here[closing..]
                .chars()
                .next()
                .is_some_and(|n| !n.is_whitespace());
            if mark != '~' && marks < 3 && closing == 3 && inner_open && can_close {
                return Some((&after_open[..i + inner_marks], marks, marks + i + 3));
            }
            let ends_word = |at: usize| {
                mark != '_'
                    || here[at..]
                        .chars()
                        .next()
                        .is_none_or(|n| !n.is_alphanumeric())
            };
            if closing >= marks && can_close && ends_word(marks) {
                // Take the marks that close it; extras belong to the text after.
                return Some((&after_open[..i], marks, marks + i + marks));
            }
            if mark != '~' && closing == inner_marks && can_open && !can_close {
                inner_open = true;
            }
            i += closing;
            continue;
        }
        i += c.len_utf8();
    }
    None
}

/// `[label](destination "title")` at the start of `text`: the label, the destination and
/// how much text it takes.
fn bracketed_link(text: &str) -> Option<(&str, &str, usize)> {
    let mut depth = 0usize;
    let mut close = None;
    let mut escaped = false;
    for (offset, c) in text.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' => escaped = true,
            '[' => depth += 1,
            ']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    close = Some(offset);
                    break;
                }
            }
            '\n' if depth == 0 => return None,
            _ => {}
        }
    }
    let close = close?;
    let label = &text[1..close];
    let after = text[close + 1..].strip_prefix('(')?;
    let mut parens = 1usize;
    let mut end = None;
    for (offset, c) in after.char_indices() {
        match c {
            '(' => parens += 1,
            ')' => {
                parens -= 1;
                if parens == 0 {
                    end = Some(offset);
                    break;
                }
            }
            '\n' => return None,
            _ => {}
        }
    }
    let end = end?;
    let target = after[..end].trim();
    // A title after the destination is dropped.
    let destination = target.split_whitespace().next()?;
    let destination = destination.trim_start_matches('<').trim_end_matches('>');
    if destination.is_empty() {
        return None;
    }
    Some((label, destination, close + 2 + end + 1))
}

/// `<https://example.com>` at the start of `text`.
fn autolink(text: &str) -> Option<(&str, usize)> {
    let end = text.find('>')?;
    let url = &text[1..end];
    let known = ["http://", "https://", "mailto:"]
        .iter()
        .any(|scheme| url.starts_with(scheme));
    (known && !url.contains(char::is_whitespace)).then_some((url, end + 1))
}

/// An `http://` or `https://` address at the start of `text`, without the punctuation that
/// ends a sentence after it.
fn bare_url(text: &str) -> Option<&str> {
    if !(text.starts_with("http://") || text.starts_with("https://")) {
        return None;
    }
    let end = text
        .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '`'))
        .unwrap_or(text.len());
    let mut url = &text[..end];
    loop {
        let trimmed = url.trim_end_matches(['.', ',', ';', ':', '!', '?', '\'', '*']);
        // A closing bracket belongs to the address only if it opened inside it.
        let unbalanced = |open: char, close: char| {
            trimmed.ends_with(close)
                && trimmed.matches(close).count() > trimmed.matches(open).count()
        };
        if unbalanced('(', ')') || unbalanced('[', ']') || unbalanced('{', '}') {
            url = &trimmed[..trimmed.len() - 1];
        } else if trimmed.len() != url.len() {
            url = trimmed;
        } else {
            break;
        }
    }
    (url.len() > "https://".len()).then_some(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(block: &Block) -> String {
        match block {
            Block::Paragraph(spans) | Block::Heading { spans, .. } => plain_text(spans),
            other => panic!("not text: {other:?}"),
        }
    }

    fn span(text: &str, style: Style) -> Span {
        Span {
            text: text.into(),
            style,
            link: None,
        }
    }

    const PLAIN: Style = Style {
        bold: false,
        italic: false,
        code: false,
        strike: false,
    };

    #[test]
    fn headings_paragraphs_and_rules() {
        let blocks = parse("# One\n\nSome text\nover two lines.\n\n### Three ###\n---\n");
        assert_eq!(blocks.len(), 4);
        assert!(matches!(&blocks[0], Block::Heading { level: 1, .. }));
        assert_eq!(text(&blocks[0]), "One");
        // A line break inside a paragraph is kept.
        assert_eq!(text(&blocks[1]), "Some text\nover two lines.");
        assert!(matches!(&blocks[2], Block::Heading { level: 3, .. }));
        assert_eq!(text(&blocks[2]), "Three");
        assert_eq!(blocks[3], Block::Rule);
        // Seven hashes, or none followed by a space, are not headings.
        assert!(matches!(&parse("####### x")[0], Block::Paragraph(_)));
        assert!(matches!(&parse("#hashtag")[0], Block::Paragraph(_)));
    }

    #[test]
    fn fenced_code_keeps_its_text_and_language_and_runs_to_the_end_while_streaming() {
        let blocks = parse("Before\n```rust\nfn main() {\n    # not a heading\n}\n```\nAfter");
        assert_eq!(blocks.len(), 3);
        assert_eq!(
            blocks[1],
            Block::Code {
                language: Some("rust".into()),
                text: "fn main() {\n    # not a heading\n}".into(),
            }
        );
        assert_eq!(text(&blocks[2]), "After");

        // Not closed yet: the rest is code.
        assert_eq!(
            parse("```\nlet x = 1;\nlet y"),
            [Block::Code {
                language: None,
                text: "let x = 1;\nlet y".into()
            }]
        );
        // A longer fence holds a shorter one, and ~~~ is a fence too.
        assert_eq!(
            parse("````md\n```\ninner\n```\n````"),
            [Block::Code {
                language: Some("md".into()),
                text: "```\ninner\n```".into()
            }]
        );
        assert!(matches!(&parse("~~~sh\nls\n~~~")[0], Block::Code { .. }));
        // Three backticks with a backtick after them is inline code.
        assert!(matches!(
            &parse("```not a fence```")[0],
            Block::Paragraph(_)
        ));
    }

    #[test]
    fn lists_nest_and_numbered_ones_keep_their_start() {
        let blocks = parse("- one\n- two\n  - two a\n  - two b\n- three\n\n3. x\n4. y");
        let Block::List { start: None, items } = &blocks[0] else {
            panic!("{:?}", blocks[0]);
        };
        assert_eq!(items.len(), 3);
        assert_eq!(text(&items[1][0]), "two");
        let Block::List {
            start: None,
            items: inner,
        } = &items[1][1]
        else {
            panic!("{:?}", items[1]);
        };
        assert_eq!(inner.len(), 2);
        assert_eq!(text(&inner[1][0]), "two b");
        assert_eq!(text(&items[2][0]), "three");
        let Block::List {
            start: Some(3),
            items,
        } = &blocks[1]
        else {
            panic!("{:?}", blocks[1]);
        };
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn a_list_inside_a_numbered_item_nests_even_when_indented_by_less_than_commonmark_asks() {
        let blocks = parse("1. First\n  - detail a\n  - detail b\n2. Second\n   continued\n");
        assert_eq!(blocks.len(), 1, "{blocks:#?}");
        let Block::List {
            start: Some(1),
            items,
        } = &blocks[0]
        else {
            panic!("{:?}", blocks[0]);
        };
        assert_eq!(items.len(), 2);
        assert_eq!(text(&items[0][0]), "First");
        assert!(matches!(&items[0][1], Block::List { start: None, items } if items.len() == 2));
        assert_eq!(text(&items[1][0]), "Second\ncontinued");
    }

    #[test]
    fn list_items_hold_code_and_paragraphs_and_lazy_lines() {
        let blocks = parse("- run:\n\n  ```sh\n  make\n  ```\n\n  then wait\n- next\nlazy line\n");
        let Block::List { items, .. } = &blocks[0] else {
            panic!("{:?}", blocks[0]);
        };
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].len(), 3);
        assert_eq!(
            items[0][1],
            Block::Code {
                language: Some("sh".into()),
                text: "make".into()
            }
        );
        assert_eq!(text(&items[1][0]), "next\nlazy line");
        // `* * *` is a rule and `2 * 3` is arithmetic, not lists.
        assert_eq!(parse("* * *"), [Block::Rule]);
        assert!(matches!(&parse("2 * 3 = 6")[0], Block::Paragraph(_)));
        assert!(matches!(&parse("-1 is negative")[0], Block::Paragraph(_)));
    }

    #[test]
    fn quotes_nest_blocks_and_take_lazy_lines() {
        let blocks = parse("> Quoted *text*\ncontinued\n>\n> - item\n\nAfter");
        let Block::Quote(inner) = &blocks[0] else {
            panic!("{:?}", blocks[0]);
        };
        assert_eq!(text(&inner[0]), "Quoted text\ncontinued");
        assert!(matches!(&inner[1], Block::List { .. }));
        assert_eq!(text(&blocks[1]), "After");
        // A quote inside a quote.
        let Block::Quote(outer) = &parse("> > deep")[0] else {
            panic!();
        };
        assert!(matches!(&outer[0], Block::Quote(_)));
    }

    #[test]
    fn pipe_tables_have_alignment_and_pad_short_rows() {
        let blocks =
            parse("| Name | Count |\n|:--|--:|\n| a | 1 |\n| `x|y` | 2 |\n| short |\n\nafter");
        let Block::Table {
            align,
            header,
            rows,
        } = &blocks[0]
        else {
            panic!("{:?}", blocks[0]);
        };
        assert_eq!(align, &[Align::Left, Align::Right]);
        assert_eq!(plain_text(&header[1]), "Count");
        assert_eq!(rows.len(), 3);
        assert_eq!(plain_text(&rows[1][0]), "x|y");
        assert_eq!(plain_text(&rows[2][1]), "");
        assert_eq!(text(&blocks[1]), "after");
        // Without a delimiter row it is a paragraph.
        assert!(matches!(&parse("a | b\nc | d")[0], Block::Paragraph(_)));
    }

    #[test]
    fn inline_styles_nest() {
        let spans = inline("a **bold *and italic*** `code` ~~gone~~ _it_");
        let bold = Style {
            bold: true,
            ..PLAIN
        };
        let both = Style {
            bold: true,
            italic: true,
            ..PLAIN
        };
        assert_eq!(
            spans,
            [
                span("a ", PLAIN),
                span("bold ", bold),
                span("and italic", both),
                span(" ", PLAIN),
                span(
                    "code",
                    Style {
                        code: true,
                        ..PLAIN
                    }
                ),
                span(" ", PLAIN),
                span(
                    "gone",
                    Style {
                        strike: true,
                        ..PLAIN
                    }
                ),
                span(" ", PLAIN),
                span(
                    "it",
                    Style {
                        italic: true,
                        ..PLAIN
                    }
                ),
            ]
        );
    }

    #[test]
    fn marks_that_do_not_close_or_sit_inside_words_stay_text() {
        for literal in [
            "2 * 3 * 4",
            "snake_case_name and a_b_c here",
            "an **unclosed bold",
            "a lone * star",
            "5 ~ 6",
            "`unclosed",
        ] {
            let spans = inline(literal);
            assert_eq!(plain_text(&spans), literal);
            assert!(
                spans.iter().all(|s| s.style == PLAIN),
                "{literal}: {spans:?}"
            );
        }
        // Asterisks in code are code.
        let spans = inline("`*a*` and *b*");
        assert_eq!(spans[0].text, "*a*");
        assert!(spans[0].style.code);
        assert_eq!(spans[2].text, "b");
        assert!(spans[2].style.italic);
        // Escapes.
        assert_eq!(
            plain_text(&inline(r"\*not italic\* and a\_b")),
            "*not italic* and a_b"
        );
    }

    #[test]
    fn links_are_spans_with_a_destination() {
        let spans = inline(
            "See [the *docs*](https://example.com/a_(b) \"title\") or https://example.org/x, <https://a.dev> and (https://b.dev).",
        );
        let links: Vec<_> = spans
            .iter()
            .filter_map(|s| s.link.as_deref().map(|l| (s.text.as_str(), l)))
            .collect();
        assert_eq!(
            links,
            [
                ("the ", "https://example.com/a_(b)"),
                ("docs", "https://example.com/a_(b)"),
                ("https://example.org/x", "https://example.org/x"),
                ("https://a.dev", "https://a.dev"),
                ("https://b.dev", "https://b.dev"),
            ]
        );
        assert!(spans.iter().any(|s| s.text == "docs" && s.style.italic));
        // The comma and the closing bracket after an address are not part of it.
        assert!(plain_text(&spans).ends_with("x, https://a.dev and (https://b.dev)."));
        // Square brackets without a destination are text, and code is never a link.
        assert_eq!(plain_text(&inline("[x] done")), "[x] done");
        assert!(inline("`https://a.dev`").iter().all(|s| s.link.is_none()));
        assert!(
            inline("[a](b)")
                .iter()
                .all(|s| s.link.as_deref() == Some("b"))
        );
        // An image is its description.
        assert_eq!(
            plain_text(&inline("![a chart](https://x.dev/c.png)")),
            "a chart"
        );
    }

    #[test]
    fn hostile_input_neither_panics_nor_runs_away() {
        let deep = ">".repeat(500) + " x";
        assert!(!parse(&deep).is_empty());
        let stars = "*a ".repeat(5000);
        assert_eq!(plain_text(&inline(&stars)), stars);
        let brackets = "[".repeat(3000) + &"](".repeat(3000);
        let _ = inline(&brackets);
        let lists = "- a\n".to_owned() + &"  - b\n".repeat(200);
        assert!(!parse(&lists).is_empty());
        for odd in [
            "", "\n\n", "| |\n|-|", "```", "- ", "1.", "> ", "[](", "é*é*é", "**é**",
        ] {
            let _ = parse(odd);
        }
    }
}
