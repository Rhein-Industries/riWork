//! A small Markdown reader for what agents write: headings, paragraphs, emphasis, inline and
//! fenced code, bullet and numbered lists (nested), links, block quotes, rules and tables,
//! and Codex's file citations (`:codex-file-citation{path="…"}`).
//!
//! It reads CommonMark loosely, because the text arrives while it streams. An unclosed code
//! fence runs to the end of the text, a `**` that never closes is two asterisks, a citation
//! whose brace has not closed yet is text, and a line break inside a paragraph stays a line
//! break, as the agents' own terminals show it. Nothing here depends on the GUI.
//!
//! Ported unchanged from the GPUI parity reference
//! `crates/riwork-desktop/src/chat_view/markdown.rs`; [`document`] turns its blocks into
//! `nsh/base` `TextView` document blocks.

use std::{borrow::Cow, ops::Range, sync::Arc};

use riwork_engine::chat::citation::{self, FileCitation};

pub mod document;
pub use document::{Splice, StreamingDocument, document_blocks};

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
    pub image: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CalloutKind {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
}

impl CalloutKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Note => "Note",
            Self::Tip => "Tip",
            Self::Important => "Important",
            Self::Warning => "Warning",
            Self::Caution => "Caution",
        }
    }
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
        title: Option<String>,
        text: String,
    },
    /// The first paragraph of a task-list item; the list owns its read-only marker.
    Task {
        checked: bool,
        spans: Vec<Span>,
    },
    /// `start` is the first number of a numbered list.
    List {
        start: Option<u64>,
        items: Vec<Vec<Block>>,
    },
    Quote(Vec<Block>),
    Callout {
        kind: CalloutKind,
        blocks: Vec<Block>,
    },
    Table {
        align: Vec<Align>,
        header: Vec<Vec<Span>>,
        rows: Vec<Vec<Vec<Span>>>,
    },
    Rule,
    /// Files cited on lines of their own, one after the other. A citation inside a sentence
    /// is a link span instead, named for its file.
    Files(Vec<FileCitation>),
}

/// Blocks nest (a quote in a list in a quote); past this the rest is plain text.
const MAX_DEPTH: usize = 8;
/// A paragraph longer than this is set as plain text: finding the end of every `*` in a
/// pathological one would take longer than the message is worth.
const MAX_STYLED: usize = 16 * 1024;

pub fn parse(source: &str) -> Vec<Block> {
    parse_with_boundary(source).0
}

/// Cache only through a complete, top-level blank line that no list may still span.
/// A blank line ends a paragraph, quote, table or run of citations, and a heading,
/// rule or closed fence before it is already whole; only a list continues past one,
/// when a later marker or indented line follows. Appended text cannot change what
/// came before such a line. The normal block parser finds the boundary, so nested
/// fences and lists cannot fool it.
#[derive(Default)]
pub struct ParseCache {
    source: String,
    blocks: Arc<Vec<Block>>,
    stable: Option<(usize, usize)>,
    /// How many leading blocks the last parse took over from the one before it, as they
    /// were: what a streaming document need not compare or convert again.
    reused: usize,
    #[cfg(test)]
    reparsed_bytes: usize,
}

impl ParseCache {
    pub fn parse(&mut self, source: &str) -> Arc<Vec<Block>> {
        if self.source == source {
            self.reused = self.blocks.len();
            return self.blocks.clone();
        }
        // Keep unusual line endings on the full parser path. Byte offsets below
        // refer to complete LF lines, never to normalized or partially read text.
        let reusable = !source.contains(['\r', '\u{feff}']);
        let (offset, count) = self
            .stable
            .filter(|(end, _)| reusable && source.get(..*end) == self.source.get(..*end))
            .unwrap_or_default();
        let (tail, boundary) = parse_with_boundary(&source[offset..]);
        let mut blocks = self.blocks[..count].to_vec();
        blocks.extend(tail);
        self.reused = count;
        self.stable = if reusable {
            boundary
                .map(|(end, blocks)| (offset + end, count + blocks))
                .or_else(|| (offset > 0).then_some((offset, count)))
        } else {
            None
        };
        #[cfg(test)]
        {
            self.reparsed_bytes = source.len() - offset;
        }
        self.source.clear();
        self.source.push_str(source);
        self.blocks = Arc::new(blocks);
        self.blocks.clone()
    }
}

fn parse_with_boundary(source: &str) -> (Vec<Block>, Option<(usize, usize)>) {
    let lines: Vec<String> = source
        .lines()
        .map(|line| line.replace('\t', "    "))
        .collect();
    let mut boundary = None;
    let blocks = blocks_with_boundary(&lines, 0, &mut boundary);
    let boundary = boundary.and_then(|(lines, blocks)| {
        let end = source.split_inclusive('\n').take(lines).map(str::len).sum();
        // A whitespace-only trailing fragment is still writable by the next chunk.
        (source.as_bytes().get(end - 1) == Some(&b'\n')).then_some((end, blocks))
    });
    (blocks, boundary)
}

#[cfg(test)]
mod incremental_tests {
    use super::*;

    #[test]
    fn every_unicode_chunk_matches_full_parse_across_block_types() {
        let sources = [
            "# Heading\n\nText **bold** é中🦀.\n\n```rust src/main.rs\nlet x = 1;\n```\n\n## After\n\n- one\n  - nested\n- [x] done\n\n> [!NOTE]\n> a quote\n\n| A | B |\n|---|---|\n| x | y |\n\n~~~text\nlast\n~~~\n\nTail",
            "- list\n  ```rust\n  code\n  ```\n\n  continued\n\nOutside\n\n```\nopen",
            "> ```\n> nested\n> ```\n\n> continued\n\n```\nroot\n```\n\n| table |\n|---|\n| row |",
            "```\ncode\n```\n \t \n- x\n  lazy continuation\n\n    indented\n\n---\n",
            "```\r\nwindows\r\n```\r\n\r\nTail\r\n",
            "\u{feff}# Heading\n\n```\ncode\n```\n\nTail",
            "Text\n```\nunfinished\n\nmore",
            "```\nx\n```\n\n:codex-file-citation{path=\"/out/Bericht März.docx\" purpose=\"output\"}\nQbit: see :codex-file-citation{path=\"/out/b.pdf\"}.\n:codex-file-citation{path=\"/out/c.png\"}\n\n- item\n  :codex-file-citation{path=\"/out/d.txt\"}",
            // Lists continue across blank lines; everything else ends at one.
            "- a\n\n- b\n\n  continued\n\n\n- c\n\n1. one\n\n2. two\n\nAfter\n\n- d\n\n  * nested\n\nend",
            "Intro **bold\nstill** here.\n\n> q\n\n> r\nlazy\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n| c |\n|---|\n\nText\n# H\n---\n\n***\n\n> [!TIP]\n> tip\n\nfin",
            "para\n\n\n\n:codex-file-citation{path=\"/a.txt\"}\n\n- [x] done\n\n- [ ] todo\n\n  nested para\n\n12. twelve\n\n13) thirteen\n\n- x\n1. y\n\nz",
        ];
        for source in sources {
            let mut cache = ParseCache::default();
            for end in source.char_indices().map(|(i, _)| i).chain([source.len()]) {
                let chunk = &source[..end];
                assert_eq!(*cache.parse(chunk), parse(chunk), "prefix: {chunk:?}");
            }
        }
    }

    #[test]
    fn replacements_truncations_and_repeated_reads_keep_exact_source_identity() {
        let mut cache = ParseCache::default();
        for source in [
            "```rs\nold\n```\n\nTail",
            "```rs\nold\n```\n\nEdit",
            "```rs\nnew\n```\n\nEdit",
            "```rs\nnew",
            "",
            "# New document\n\n```\nchanged\n```\n\n끝",
        ] {
            let parsed = cache.parse(source);
            assert_eq!(*parsed, parse(source));
            assert!(Arc::ptr_eq(&parsed, &cache.parse(source)));
        }
    }

    #[test]
    fn complete_top_level_blank_lines_outside_lists_seal_a_prefix() {
        let mut cache = ParseCache::default();
        let prefix = "# Start\n\n```rs\nlet x = 1;\n```\n\n";
        cache.parse(prefix);
        cache.parse(&format!("{prefix}Tail"));
        assert_eq!(cache.reparsed_bytes, 4);
        // A streaming answer reparses only the paragraph being written.
        let prose = "Intro **bold**.\n\n> ```\n> code\n> ```\n\n| a |\n|---|\n| b |\n\n";
        let mut cache = ParseCache::default();
        cache.parse(prose);
        cache.parse(&format!("{prose}Next sentence"));
        assert_eq!(cache.reparsed_bytes, "Next sentence".len());
        for incomplete in [
            "```\ncode\n```\n ",
            "```\ncode\n```\n",
            "Paragraph\n \t",
            "- list\n  ```\n  code\n  ```\n\n",
            "- item\n\n",
            "- item\n\n\n",
            "- item\n\nmore",
            "```\nopen\n\nstill code\n\n",
        ] {
            let mut cache = ParseCache::default();
            cache.parse(incomplete);
            assert!(cache.stable.is_none(), "{incomplete:?}");
        }
    }

    /// Run explicitly in an optimized test binary; timings are evidence, not a
    /// platform-sensitive assertion in the normal suite.
    #[test]
    #[ignore = "manual optimized streaming benchmark"]
    fn streaming_benchmark() {
        use std::{hint::black_box, time::Instant};
        let mut source = String::new();
        for section in 0..128 {
            source.push_str(&format!("## Section {section}\n\nSome **explanation** with [a link](https://example.com).\n\n```rust\n"));
            for line in 0..40 {
                source.push_str(&format!("let value_{line} = process({section}, {line});\n"));
            }
            source.push_str("```\n\n");
        }
        let ends: Vec<_> = (512..source.len())
            .step_by(512)
            .chain([source.len()])
            .collect();
        let mut full_times = Vec::new();
        let mut cached_times = Vec::new();
        for _ in 0..5 {
            let start = Instant::now();
            for &end in &ends {
                black_box(parse(black_box(&source[..end])));
            }
            full_times.push(start.elapsed());
            let mut cache = ParseCache::default();
            let start = Instant::now();
            for &end in &ends {
                black_box(cache.parse(black_box(&source[..end])));
            }
            cached_times.push(start.elapsed());
            assert_eq!(*cache.parse(&source), parse(&source));
        }
        full_times.sort();
        cached_times.sort();
        eprintln!(
            "{} bytes, {} chunks; median of 5: full {:?}, incremental {:?}, {:.2}x faster",
            source.len(),
            ends.len(),
            full_times[2],
            cached_times[2],
            full_times[2].as_secs_f64() / cached_times[2].as_secs_f64()
        );
    }
}

/// The concatenated text of spans, which is what the screen shows.
pub fn plain_text(spans: &[Span]) -> String {
    spans.iter().map(|span| span.text.as_str()).collect()
}

/// Text a row shows without Markdown (commentary): each citation reads as its file's name,
/// linked to the file. The row draws this text and selecting it copies the same.
pub fn cited_text(text: &str) -> (Cow<'_, str>, Vec<(Range<usize>, String)>) {
    let found = citation::find(text);
    if found.is_empty() {
        return (Cow::Borrowed(text), Vec::new());
    }
    let mut shown = String::with_capacity(text.len());
    let mut links = Vec::new();
    let mut at = 0;
    for (range, file) in found {
        shown.push_str(&text[at..range.start]);
        links.push((
            shown.len()..shown.len() + file.name().len(),
            file.path.clone(),
        ));
        shown.push_str(file.name());
        at = range.end;
    }
    shown.push_str(&text[at..]);
    (Cow::Owned(shown), links)
}

// ---------------------------------------------------------------------------------------
// Blocks
// ---------------------------------------------------------------------------------------

fn blocks(lines: &[String], depth: usize) -> Vec<Block> {
    blocks_with_boundary(lines, depth, &mut None)
}

fn blocks_with_boundary(
    lines: &[String],
    depth: usize,
    boundary: &mut Option<(usize, usize)>,
) -> Vec<Block> {
    let mut out = Vec::new();
    let mut paragraph: Vec<&str> = Vec::new();
    // A list followed only by blank lines so far: a later item may still join it.
    let mut list_open = false;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].as_str();
        if line.trim().is_empty() {
            flush(&mut paragraph, &mut out);
            i += 1;
            if depth == 0 && !list_open {
                *boundary = Some((i, out.len()));
            }
            continue;
        }
        list_open = false;
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
                title: fence.title,
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
            let callout = inner.first().and_then(|line| match line.trim() {
                "[!NOTE]" => Some(CalloutKind::Note),
                "[!TIP]" => Some(CalloutKind::Tip),
                "[!IMPORTANT]" => Some(CalloutKind::Important),
                "[!WARNING]" => Some(CalloutKind::Warning),
                "[!CAUTION]" => Some(CalloutKind::Caution),
                _ => None,
            });
            out.push(match callout {
                Some(kind) => Block::Callout {
                    kind,
                    blocks: blocks(&inner[1..], depth + 1),
                },
                None => Block::Quote(blocks(&inner, depth + 1)),
            });
            continue;
        }
        if let Some(mut files) = citation::line(line) {
            flush(&mut paragraph, &mut out);
            i += 1;
            while let Some(more) = lines.get(i).and_then(|line| citation::line(line)) {
                files.extend(more);
                i += 1;
            }
            out.push(Block::Files(files));
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
            list_open = true;
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
    title: Option<String>,
}

/// Only the small, conventional fence header is metadata; never interpret arbitrary HTML.
fn fence_info(info: &str) -> (Option<String>, Option<String>) {
    let mut tokens = Vec::new();
    let mut rest = info;
    while !rest.is_empty() && tokens.len() < 16 {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        let mut quoted = None;
        let end = rest
            .char_indices()
            .find_map(|(at, c)| {
                match (quoted, c) {
                    (Some(quote), c) if quote == c => quoted = None,
                    (None, '\'' | '"') => quoted = Some(c),
                    (None, c) if c.is_whitespace() => return Some(at),
                    _ => {}
                }
                None
            })
            .unwrap_or(rest.len());
        tokens.push(&rest[..end]);
        rest = &rest[end..];
    }
    let language = tokens
        .first()
        .filter(|token| !token.contains('='))
        .map(|token| token.to_string());
    let title = tokens.iter().find_map(|token| {
        let value = if let Some((key, value)) = token.split_once('=') {
            matches!(key, "title" | "file" | "filename").then_some(value)?
        } else if Some(*token) != tokens.first().copied() && token.contains('.') {
            token
        } else {
            return None;
        };
        let value = value.trim_matches(['\'', '"']);
        (!value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control))
            .then(|| value.to_owned())
    });
    (language, title)
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
    let (language, title) = fence_info(info);
    Some(Fence {
        indent,
        mark,
        length,
        language,
        title,
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
        let checked = body.first().and_then(|line| {
            let checked = match line.get(..3) {
                Some("[ ]") => false,
                Some("[x]" | "[X]") => true,
                _ => return None,
            };
            (line.len() == 3 || line[3..].starts_with(char::is_whitespace)).then_some(checked)
        });
        if checked.is_some() {
            body[0] = body[0][3..].trim_start().to_owned();
        }
        let mut parsed = blocks(&body, depth + 1);
        if let Some(checked) = checked {
            if let Some(Block::Paragraph(spans)) = parsed.first_mut() {
                let spans = std::mem::take(spans);
                parsed[0] = Block::Task { checked, spans };
            } else {
                parsed.insert(
                    0,
                    Block::Task {
                        checked,
                        spans: Vec::new(),
                    },
                );
            }
        }
        items.push(parsed);
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
    local_images(spans)
}

/// Whether `target` is a local image, by its extension.
pub fn is_local_image(target: &str) -> bool {
    super::links::local_target(target)
        .ok()
        .is_some_and(|(path, _)| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| {
                    matches!(
                        ext.to_ascii_lowercase().as_str(),
                        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "ico"
                    )
                })
        })
}

fn local_images(spans: Vec<Span>) -> Vec<Span> {
    let is_image = is_local_image;
    let mut out = Vec::new();
    for span in spans {
        if span.image {
            out.push(span);
            continue;
        }
        if let Some(target) = &span.link {
            let mut span = span.clone();
            span.image = is_image(target);
            out.push(span);
            continue;
        }
        let paths = super::links::detect(&span.text, span.style.code);
        let mut at = 0;
        for (range, target) in paths.into_iter().filter(|(_, target)| is_image(target)) {
            if at < range.start {
                out.push(Span {
                    text: span.text[at..range.start].into(),
                    ..span.clone()
                });
            }
            out.push(Span {
                text: target.clone(),
                link: Some(target),
                image: true,
                style: span.style,
            });
            at = range.end;
        }
        if at < span.text.len() {
            out.push(Span {
                text: span.text[at..].into(),
                ..span
            });
        }
    }
    out
}

fn push(spans: &mut Vec<Span>, text: &str, style: Style, link: Option<&str>) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = spans.last_mut()
        && !last.image
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
        image: false,
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
            '!' if rest.starts_with("![") && depth < MAX_DEPTH => {
                if let Some((label, target, used)) = bracketed_link(&rest[1..]) {
                    push(out, &plain, style, link);
                    plain.clear();
                    out.push(Span {
                        text: label.into(),
                        style,
                        link: Some(target.into()),
                        image: true,
                    });
                    i += used + 1;
                } else {
                    plain.push(c);
                    i += width;
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
            // A citation inside a sentence reads as its file's name, linked to the file.
            ':' => match citation::parse(rest) {
                Some((file, used)) => {
                    push(out, &plain, style, link);
                    plain.clear();
                    out.push(Span {
                        text: file.name().to_owned(),
                        style,
                        link: Some(file.path),
                        image: false,
                    });
                    i += used;
                }
                None => {
                    plain.push(c);
                    i += width;
                }
            },
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
    let destination = if let Some(angled) = target.strip_prefix('<') {
        angled.split_once('>')?.0
    } else {
        target
            .split_once(" \"")
            .map_or(target, |(destination, _)| destination)
    };
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

    const PLAIN: Style = Style {
        bold: false,
        italic: false,
        code: false,
        strike: false,
    };

    #[test]
    fn explicit_images_keep_image_semantics_for_remote_data_and_local_targets() {
        for target in [
            "https://example.com/chart.png",
            "https://example.com/image?id=1",
            "data:image/png;base64,AAAA",
            "/tmp/chart.png",
        ] {
            let spans = inline(&format!("Before ![A chart]({target}) after"));
            assert_eq!(spans.len(), 3, "{target}: {spans:?}");
            assert_eq!(spans[1].text, "A chart");
            assert_eq!(spans[1].link.as_deref(), Some(target));
            assert!(spans[1].image, "{target}");
            assert!(!spans[0].image && !spans[2].image);
        }
    }

    #[test]
    fn an_image_does_not_absorb_an_adjacent_link_to_the_same_target() {
        let spans =
            inline("![Chart](https://example.com/chart)[Source](https://example.com/chart)");
        assert_eq!(spans.len(), 2);
        assert!(spans[0].image);
        assert_eq!(spans[0].text, "Chart");
        assert!(!spans[1].image);
        assert_eq!(spans[1].text, "Source");
        assert_eq!(spans[0].link, spans[1].link);
    }

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
                title: None,
                text: "fn main() {\n    # not a heading\n}".into(),
            }
        );
        assert_eq!(text(&blocks[2]), "After");

        // Not closed yet: the rest is code.
        assert_eq!(
            parse("```\nlet x = 1;\nlet y"),
            [Block::Code {
                language: None,
                title: None,
                text: "let x = 1;\nlet y".into()
            }]
        );
        // A longer fence holds a shorter one, and ~~~ is a fence too.
        assert_eq!(
            parse("````md\n```\ninner\n```\n````"),
            [Block::Code {
                language: Some("md".into()),
                title: None,
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
    fn fences_keep_quoted_filename_metadata_separate_from_code() {
        for info in [
            "rust title=\"src/my file.rs\"",
            "rust filename='src/my file.rs'",
        ] {
            let source = format!("```{info}\nfn main() {{}}\n```");
            let Block::Code {
                language,
                title,
                text,
            } = parse(&source).remove(0)
            else {
                panic!()
            };
            assert_eq!(language.as_deref(), Some("rust"));
            assert_eq!(title.as_deref(), Some("src/my file.rs"));
            assert_eq!(text, "fn main() {}");
        }
        assert_eq!(fence_info("title=main.rs"), (None, Some("main.rs".into())));
        assert_eq!(
            fence_info("rust src/main.rs"),
            (Some("rust".into()), Some("src/main.rs".into()))
        );
        assert!(
            fence_info(&format!("rust title={}", "x".repeat(257)))
                .1
                .is_none()
        );
    }

    #[test]
    fn task_lists_and_callouts_preserve_nested_content() {
        let Block::List { items, .. } =
            parse("- [x] **Done**\n- [ ] Next\n  - nested\n- [x]word").remove(0)
        else {
            panic!()
        };
        assert!(
            matches!(&items[0][0], Block::Task { checked: true, spans } if spans[0].style.bold)
        );
        assert!(matches!(&items[1][0], Block::Task { checked: false, .. }));
        assert!(matches!(&items[1][1], Block::List { .. }));
        assert!(matches!(&items[2][0], Block::Paragraph(_)));
        let Block::Callout { kind, blocks } =
            parse("> [!WARNING]\n> Keep **this**.\n>\n> - Detail").remove(0)
        else {
            panic!()
        };
        assert_eq!(kind, CalloutKind::Warning);
        assert_eq!(text(&blocks[0]), "Keep this.");
        assert!(matches!(&blocks[1], Block::List { .. }));
        assert!(matches!(
            &parse("> [!UNKNOWN]\n> ordinary quote")[0],
            Block::Quote(_)
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

    const REPORT: &str = "/Users/me/projects/Baumer_featuring_UBS/outputs/ocra-ky3p/Baumer_Organisation_Fragen_und_Nachweise_20261021.docx";

    fn cite(path: &str) -> String {
        format!(
            ":codex-file-citation{{path=\"{path}\" artifact_kind=\"document\" purpose=\"output\"}}"
        )
    }

    #[test]
    fn citations_on_lines_of_their_own_are_files_and_in_a_sentence_linked_names() {
        let source = format!(
            "{}\nQbit: 114 eigene Einträge plus Übersicht.\n{}\n  {} {}\n\nSee {} for the rest.",
            cite(REPORT),
            cite("/out/Qbit Fragen.docx"),
            cite("/out/b.pdf"),
            cite("/out/c.txt"),
            cite("/out/Zuarbeit März.xlsx"),
        );
        let blocks = parse(&source);
        assert_eq!(blocks.len(), 4, "{blocks:#?}");
        let Block::Files(files) = &blocks[0] else {
            panic!("{:?}", blocks[0]);
        };
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, REPORT);
        assert_eq!(files[0].artifact_kind.as_deref(), Some("document"));
        assert_eq!(files[0].purpose.as_deref(), Some("output"));
        // The line between them stays its own paragraph, and the next lines of citations
        // are one row of files.
        assert_eq!(
            text(&blocks[1]),
            "Qbit: 114 eigene Einträge plus Übersicht."
        );
        let Block::Files(files) = &blocks[2] else {
            panic!("{:?}", blocks[2]);
        };
        assert_eq!(
            files.iter().map(FileCitation::name).collect::<Vec<_>>(),
            ["Qbit Fragen.docx", "b.pdf", "c.txt"]
        );
        // Inside a sentence, the file's name links to its path; selecting it copies the name.
        let Block::Paragraph(spans) = &blocks[3] else {
            panic!("{:?}", blocks[3]);
        };
        assert_eq!(plain_text(spans), "See Zuarbeit März.xlsx for the rest.");
        assert_eq!(spans[1].link.as_deref(), Some("/out/Zuarbeit März.xlsx"));
        assert!(!spans[1].image);
        // Inside a list item and a quote as well.
        let Block::List { items, .. } = parse(&format!("- One\n  {}", cite("/out/d.md"))).remove(0)
        else {
            panic!()
        };
        assert!(matches!(&items[0][1], Block::Files(files) if files[0].path == "/out/d.md"));
        let Block::Quote(inner) = parse(&format!("> {}", cite("/out/e.md"))).remove(0) else {
            panic!()
        };
        assert!(matches!(&inner[0], Block::Files(_)));
    }

    #[test]
    fn malformed_unfinished_and_quoted_citations_stay_text() {
        for literal in [
            ":codex-file-citation{purpose=\"output\"}",
            ":codex-file-citation{path=\"/out/a.docx\"",
            ":codex-file-citation{path=\"/out/a.docx\" purpose=\"output\"",
            ":codex-file-citation{path=\"/out/a.do",
            ":codex-file-citation{path=/out/a.docx}",
            "a :codex-file-citation{path=\"\"} b",
        ] {
            let blocks = parse(literal);
            assert_eq!(blocks.len(), 1, "{literal}: {blocks:?}");
            assert_eq!(text(&blocks[0]), literal);
        }
        // Code shows the directive itself.
        let spans = inline(&format!("`{}`", cite("/out/a.docx")));
        assert_eq!(plain_text(&spans), cite("/out/a.docx"));
        assert!(spans[0].style.code && spans[0].link.is_none());
        assert!(matches!(
            &parse(&format!("```\n{}\n```", cite("/out/a.docx")))[0],
            Block::Code { .. }
        ));
        // While a message streams, a directive is text until its brace closes, then a file.
        let full = format!("Done.\n{}", cite(REPORT));
        for end in full.char_indices().map(|(at, _)| at).chain([full.len()]) {
            let blocks = parse(&full[..end]);
            if end == full.len() {
                assert!(matches!(&blocks[1], Block::Files(_)), "{blocks:?}");
            } else if end > "Done.\n".len() {
                assert_eq!(text(&blocks[0]), full[..end].trim_end());
            }
        }
    }

    #[test]
    fn plain_rows_name_cited_files_and_link_them() {
        let source = format!("Wrote {} and {}.", cite(REPORT), cite("/b.pdf"));
        let (shown, links) = cited_text(&source);
        assert_eq!(
            shown,
            "Wrote Baumer_Organisation_Fragen_und_Nachweise_20261021.docx and b.pdf."
        );
        assert_eq!(links.len(), 2);
        assert_eq!(
            &shown[links[0].0.clone()],
            "Baumer_Organisation_Fragen_und_Nachweise_20261021.docx"
        );
        assert_eq!(links[0].1, REPORT);
        assert_eq!(&shown[links[1].0.clone()], "b.pdf");
        assert!(
            matches!(cited_text("No files."), (Cow::Borrowed("No files."), links) if links.is_empty())
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
        let citations = ":codex-file-citation{path=\"".repeat(500);
        assert_eq!(plain_text(&inline(&citations)), citations);
        for odd in [
            "", "\n\n", "| |\n|-|", "```", "- ", "1.", "> ", "[](", "é*é*é", "**é**",
        ] {
            let _ = parse(odd);
        }
    }
}
