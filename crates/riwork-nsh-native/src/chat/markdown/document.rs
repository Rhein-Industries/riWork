//! RiWork's Markdown blocks as `nsh/base` `TextView` document blocks: the objects TSX
//! passes as `document={{ blocks }}` (`TextViewBlock`, `TextViewInline` in
//! `nsh/base.d.ts`). The blocks are built as `nsh_base::text_view` nodes and cross as
//! their JSON, so the shape is the one TextView reads by construction.
//!
//! A line break inside a paragraph is a `break` (RiWork keeps single newlines; NSH's own
//! Markdown would make them spaces). A run of cited files is a custom block named
//! [`FILES`] for TSX's `renderCustom`. Images are out of scope here: an image span is a
//! link to its target.
//!
//! [`StreamingDocument`] keeps the blocks of a message that is still being written and
//! answers each new text with a [`Splice`] of the blocks that changed: earlier blocks are
//! neither converted nor sent again.

use std::sync::Arc;

use nsh_base::text_view::{
    Alert, Block as ViewBlock, CodeBlock, ColumnAlign, CustomNode, Inline, ListItem, Table,
    TableCell,
};
use riwork_engine::chat::citation::FileCitation;
use serde::Serialize;
use serde_json::{Value, json};

use super::{Align, Block, CalloutKind, ParseCache, Span, is_local_image};

/// The name of the custom block that files cited on lines of their own become. Its data
/// is `{ files: [{ path, name, image, artifactKind?, purpose? }] }` and its text their
/// names, one per line (what copying the block gives).
pub const FILES: &str = "files";

/// `blocks` as `TextViewBlock` objects.
pub fn document_blocks(blocks: &[Block]) -> Vec<Value> {
    blocks.iter().map(block_json).collect()
}

/// `block` as a `TextViewBlock` object.
pub fn block_json(block: &Block) -> Value {
    serde_json::to_value(view_block(block)).expect("TextView nodes serialize to JSON")
}

/// `block` as a TextView node.
pub fn view_block(block: &Block) -> ViewBlock {
    match block {
        Block::Heading { level, spans } => ViewBlock::Heading {
            level: (*level).clamp(1, 6),
            content: inlines(spans),
            anchor: None,
        },
        // A task is the first block of a list item (`list_item`); elsewhere it is its text.
        Block::Paragraph(spans) | Block::Task { spans, .. } => ViewBlock::Paragraph {
            content: inlines(spans),
        },
        Block::Code {
            language,
            title,
            text,
        } => {
            let mut code = CodeBlock::new(language.clone(), text.clone());
            code.meta = title.clone();
            ViewBlock::CodeBlock(code)
        }
        Block::List { start, items } => ViewBlock::List {
            ordered: start.is_some(),
            // At most nine digits: always a u32.
            start: start.map_or(1, |start| u32::try_from(start).unwrap_or(u32::MAX)),
            loose: false,
            items: items.iter().map(|item| list_item(item)).collect(),
        },
        Block::Quote(blocks) => ViewBlock::Blockquote {
            children: view_blocks(blocks),
            alert: None,
        },
        Block::Callout { kind, blocks } => ViewBlock::Blockquote {
            children: view_blocks(blocks),
            alert: Some(alert(*kind)),
        },
        Block::Table {
            align,
            header,
            rows,
        } => {
            let cell = |spans: &Vec<Span>| TableCell::new(inlines(spans));
            let mut table = Table::default();
            table.align = align.iter().map(|align| column_align(*align)).collect();
            table.head = header.iter().map(cell).collect();
            table.rows = rows
                .iter()
                .map(|row| row.iter().map(cell).collect())
                .collect();
            ViewBlock::Table(table)
        }
        Block::Rule => ViewBlock::Rule,
        Block::Files(files) => ViewBlock::Custom(files_node(files)),
    }
}

fn view_blocks(blocks: &[Block]) -> Vec<ViewBlock> {
    blocks.iter().map(view_block).collect()
}

/// A task item's first block carries its check; a task with no text of its own (`- [x]`
/// alone, or before a code fence) leaves no empty paragraph behind.
fn list_item(blocks: &[Block]) -> ListItem {
    match blocks.split_first() {
        Some((Block::Task { checked, spans }, rest)) => {
            let mut children = Vec::with_capacity(blocks.len());
            if !spans.is_empty() {
                children.push(ViewBlock::Paragraph {
                    content: inlines(spans),
                });
            }
            children.extend(rest.iter().map(view_block));
            let mut item = ListItem::new(children);
            item.checked = Some(*checked);
            item
        }
        _ => ListItem::new(view_blocks(blocks)),
    }
}

fn alert(kind: CalloutKind) -> Alert {
    match kind {
        CalloutKind::Note => Alert::Note,
        CalloutKind::Tip => Alert::Tip,
        CalloutKind::Important => Alert::Important,
        CalloutKind::Warning => Alert::Warning,
        CalloutKind::Caution => Alert::Caution,
    }
}

fn column_align(align: Align) -> ColumnAlign {
    match align {
        Align::Left => ColumnAlign::Left,
        Align::Center => ColumnAlign::Center,
        Align::Right => ColumnAlign::Right,
    }
}

fn files_node(files: &[FileCitation]) -> CustomNode {
    let names = files
        .iter()
        .map(FileCitation::name)
        .collect::<Vec<_>>()
        .join("\n");
    let files = files
        .iter()
        .map(|file| {
            let mut entry = json!({
                "path": file.path,
                "name": file.name(),
                "image": is_local_image(&file.path),
            });
            if let Some(kind) = &file.artifact_kind {
                entry["artifactKind"] = kind.as_str().into();
            }
            if let Some(purpose) = &file.purpose {
                entry["purpose"] = purpose.as_str().into();
            }
            entry
        })
        .collect::<Vec<_>>();
    CustomNode::new(FILES)
        .with_text(names)
        .with_data(json!({ "files": files }))
}

// ---------------------------------------------------------------------------------------
// Inline
// ---------------------------------------------------------------------------------------

/// Spans as inline nodes, nested outside in: link, strong, emphasis, strikethrough, then
/// code or text. Neighbouring spans that share a link or a style share its node.
pub fn inlines(spans: &[Span]) -> Vec<Inline> {
    let mut out = Vec::with_capacity(spans.len());
    links(spans, &mut out);
    out
}

/// Spans to one target are one link. An image (out of scope here) is a link of its own to
/// its target, named by its label, else by the target.
fn links(spans: &[Span], out: &mut Vec<Inline>) {
    let mut i = 0;
    while i < spans.len() {
        let Some(url) = &spans[i].link else {
            let end = run(spans, i, |span| span.link.is_none());
            styled(&spans[i..end], Layer::Strong, out);
            i = end;
            continue;
        };
        let mut content = Vec::new();
        if spans[i].image {
            if spans[i].text.is_empty() {
                let named = Span {
                    text: url.clone(),
                    ..spans[i].clone()
                };
                styled(std::slice::from_ref(&named), Layer::Strong, &mut content);
            } else {
                styled(&spans[i..=i], Layer::Strong, &mut content);
            }
            i += 1;
        } else {
            let end = run(spans, i, |span| {
                !span.image && span.link.as_deref() == Some(url.as_str())
            });
            styled(&spans[i..end], Layer::Strong, &mut content);
            i = end;
        }
        out.push(Inline::Link {
            url: url.clone(),
            title: None,
            content,
        });
    }
}

#[derive(Clone, Copy)]
enum Layer {
    Strong,
    Emphasis,
    Strikethrough,
    Leaf,
}

fn styled(spans: &[Span], layer: Layer, out: &mut Vec<Inline>) {
    let (on, next): (fn(&Span) -> bool, Layer) = match layer {
        Layer::Strong => (|span| span.style.bold, Layer::Emphasis),
        Layer::Emphasis => (|span| span.style.italic, Layer::Strikethrough),
        Layer::Strikethrough => (|span| span.style.strike, Layer::Leaf),
        Layer::Leaf => {
            spans.iter().for_each(|span| leaf(span, out));
            return;
        }
    };
    let mut i = 0;
    while i < spans.len() {
        let set = on(&spans[i]);
        let end = run(spans, i, |span| on(span) == set);
        if set {
            let mut content = Vec::new();
            styled(&spans[i..end], next, &mut content);
            out.push(match layer {
                Layer::Strong => Inline::Strong { content },
                Layer::Emphasis => Inline::Emphasis { content },
                _ => Inline::Strikethrough { content },
            });
        } else {
            styled(&spans[i..end], next, out);
        }
        i = end;
    }
}

/// Code as code; text with each `\n` a visible line break.
fn leaf(span: &Span, out: &mut Vec<Inline>) {
    if span.style.code {
        out.push(Inline::Code {
            code: span.text.clone(),
        });
        return;
    }
    for (at, line) in span.text.split('\n').enumerate() {
        if at > 0 {
            out.push(Inline::Break);
        }
        if !line.is_empty() {
            out.push(Inline::text(line));
        }
    }
}

/// The end of the run of spans from `start` (which belongs to it) that `belongs` accepts.
fn run(spans: &[Span], start: usize, belongs: impl Fn(&Span) -> bool) -> usize {
    spans[start + 1..]
        .iter()
        .position(|span| !belongs(span))
        .map_or(spans.len(), |at| start + 1 + at)
}

// ---------------------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------------------

/// What changed in a document's blocks: replace the blocks from `from` on with `blocks`;
/// the document then has `len` blocks (`from + blocks.len()`).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Splice {
    pub from: usize,
    pub blocks: Vec<Value>,
    pub len: usize,
}

/// The TextView blocks of a message as it streams. Each [`update`](Self::update) parses
/// the new text through a [`ParseCache`] (only what follows the sealed prefix is read
/// again) and converts only the blocks from the first one that changed, so a delta costs
/// the tail, not the message.
#[derive(Default)]
pub struct StreamingDocument {
    cache: ParseCache,
    parsed: Arc<Vec<Block>>,
    blocks: Vec<Value>,
}

impl StreamingDocument {
    pub fn new() -> Self {
        Self::default()
    }

    /// The blocks of the text last given, as `TextViewBlock` objects.
    pub fn blocks(&self) -> &[Value] {
        &self.blocks
    }

    /// The parsed blocks of the text last given.
    pub fn parsed(&self) -> &Arc<Vec<Block>> {
        &self.parsed
    }

    /// Takes the whole text so far (or a replacement of it) and returns how the blocks
    /// changed: `from` is the first block whose JSON differs from the last emission.
    pub fn update(&mut self, text: &str) -> Splice {
        let parsed = self.cache.parse(text);
        if Arc::ptr_eq(&parsed, &self.parsed) {
            return Splice {
                from: self.blocks.len(),
                blocks: Vec::new(),
                len: self.blocks.len(),
            };
        }
        // The blocks the cache took over are the ones converted last time; past them, a
        // block that parses the same converts the same.
        let mut from = self.cache.reused.min(self.parsed.len()).min(parsed.len());
        let shared = parsed.len().min(self.parsed.len());
        while from < shared && parsed[from] == self.parsed[from] {
            from += 1;
        }
        let previous = self.blocks.split_off(from);
        self.blocks.extend(parsed[from..].iter().map(block_json));
        // A block can parse differently and still show the same.
        from += self.blocks[from..]
            .iter()
            .zip(&previous)
            .take_while(|(new, old)| new == old)
            .count();
        self.parsed = parsed;
        Splice {
            from,
            blocks: self.blocks[from..].to_vec(),
            len: self.blocks.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::markdown::parse;

    fn doc(source: &str) -> Vec<Value> {
        document_blocks(&parse(source))
    }

    /// TextView reads `document` through serde (`nsh_base::text_view::Block`): every block
    /// emitted must read back as the same JSON.
    fn read_back(blocks: &[Value]) {
        for block in blocks {
            let node: ViewBlock = serde_json::from_value(block.clone())
                .unwrap_or_else(|error| panic!("{error}: {block}"));
            assert_eq!(&serde_json::to_value(node).unwrap(), block);
        }
    }

    fn text(text: &str) -> Value {
        json!({ "type": "text", "text": text })
    }

    #[test]
    fn headings_paragraphs_rules_and_code_blocks() {
        let blocks = doc(
            "# One\n\nSome text\n\n---\n\n```rust title=\"src/main.rs\"\nfn main() {}\n```\n\n```\nplain\n```",
        );
        read_back(&blocks);
        assert_eq!(
            blocks,
            [
                json!({ "type": "heading", "level": 1, "content": [text("One")] }),
                json!({ "type": "paragraph", "content": [text("Some text")] }),
                json!({ "type": "rule" }),
                json!({ "type": "codeBlock", "language": "rust", "meta": "src/main.rs", "code": "fn main() {}" }),
                json!({ "type": "codeBlock", "code": "plain" }),
            ]
        );
    }

    #[test]
    fn a_single_newline_in_a_paragraph_is_a_visible_break() {
        let blocks = doc("Line one\nline **two\nthree**\n\n\nnext");
        read_back(&blocks);
        assert_eq!(
            blocks,
            [
                json!({ "type": "paragraph", "content": [
                    text("Line one"),
                    { "type": "break" },
                    text("line "),
                    { "type": "strong", "content": [text("two"), { "type": "break" }, text("three")] },
                ] }),
                json!({ "type": "paragraph", "content": [text("next")] }),
            ]
        );
        // A table cell or heading has no line of its own to break.
        assert!(
            !json!(doc("# a\n| x |\n|---|\n| y |"))
                .to_string()
                .contains("break")
        );
    }

    #[test]
    fn inline_styles_nest_and_links_hold_their_styled_label() {
        let blocks =
            doc("*i* **b** ***bi*** ~~s~~ `c` [**see** it](https://x.dev) <https://y.dev>");
        read_back(&blocks);
        assert_eq!(
            blocks[0]["content"],
            json!([
                { "type": "emphasis", "content": [text("i")] },
                text(" "),
                { "type": "strong", "content": [text("b")] },
                text(" "),
                { "type": "strong", "content": [{ "type": "emphasis", "content": [text("bi")] }] },
                text(" "),
                { "type": "strikethrough", "content": [text("s")] },
                text(" "),
                { "type": "code", "code": "c" },
                text(" "),
                { "type": "link", "url": "https://x.dev", "content": [
                    { "type": "strong", "content": [text("see")] },
                    text(" it"),
                ] },
                text(" "),
                { "type": "link", "url": "https://y.dev", "content": [text("https://y.dev")] },
            ])
        );
        // Neighbouring spans in one style share its node: code inside bold stays inside.
        assert_eq!(
            doc("**a `b` ~~c~~**")[0]["content"],
            json!([{ "type": "strong", "content": [
                text("a "),
                { "type": "code", "code": "b" },
                text(" "),
                { "type": "strikethrough", "content": [text("c")] },
            ] }])
        );
    }

    #[test]
    fn images_and_inline_citations_are_links_to_their_target() {
        let blocks = doc(
            "![Chart](https://e.com/c.png)[Source](https://e.com/c.png) ![](/tmp/a.png) see /tmp/b.png and :codex-file-citation{path=\"/out/Bericht März.docx\"}",
        );
        read_back(&blocks);
        let link = |url: &str, label: &str| json!({ "type": "link", "url": url, "content": [text(label)] });
        assert_eq!(
            blocks[0]["content"],
            json!([
                link("https://e.com/c.png", "Chart"),
                link("https://e.com/c.png", "Source"),
                text(" "),
                link("/tmp/a.png", "/tmp/a.png"),
                text(" see "),
                link("/tmp/b.png", "/tmp/b.png"),
                text(" and "),
                link("/out/Bericht März.docx", "Bericht März.docx"),
            ])
        );
    }

    #[test]
    fn lists_tasks_quotes_callouts_tables_and_files() {
        let source = format!(
            "3. three\n4. four\n   - nested\n\n- [x] **done**\n- [ ] todo\n- [x]\n  ```\n  code\n  ```\n\n> quoted\n\n> [!WARNING]\n> careful\n\n| L | C | R |\n|---|:-:|--:|\n| a | b |\n\n{}\n{}",
            ":codex-file-citation{path=\"/out/r.docx\" artifact_kind=\"document\" purpose=\"output\"}",
            ":codex-file-citation{path=\"/out/shot.png\"}",
        );
        let blocks = doc(&source);
        read_back(&blocks);
        let paragraph = |label: &str| json!({ "type": "paragraph", "content": [text(label)] });
        assert_eq!(
            blocks,
            [
                json!({ "type": "list", "ordered": true, "start": 3, "loose": false, "items": [
                    { "children": [paragraph("three")] },
                    { "children": [
                        paragraph("four"),
                        { "type": "list", "ordered": false, "start": 1, "loose": false, "items": [
                            { "children": [paragraph("nested")] },
                        ] },
                    ] },
                ] }),
                json!({ "type": "list", "ordered": false, "start": 1, "loose": false, "items": [
                    { "checked": true, "children": [
                        { "type": "paragraph", "content": [{ "type": "strong", "content": [text("done")] }] },
                    ] },
                    { "checked": false, "children": [paragraph("todo")] },
                    { "checked": true, "children": [{ "type": "codeBlock", "code": "code" }] },
                ] }),
                json!({ "type": "blockquote", "children": [paragraph("quoted")] }),
                json!({ "type": "blockquote", "alert": "warning", "children": [paragraph("careful")] }),
                json!({ "type": "table", "align": ["left", "center", "right"],
                    "head": [{ "content": [text("L")] }, { "content": [text("C")] }, { "content": [text("R")] }],
                    "rows": [[{ "content": [text("a")] }, { "content": [text("b")] }, { "content": [] }]],
                }),
                json!({ "type": "custom", "name": "files", "text": "r.docx\nshot.png", "data": { "files": [
                    { "path": "/out/r.docx", "name": "r.docx", "image": false, "artifactKind": "document", "purpose": "output" },
                    { "path": "/out/shot.png", "name": "shot.png", "image": true },
                ] } }),
            ]
        );
        for (kind, alert) in [
            ("NOTE", "note"),
            ("TIP", "tip"),
            ("IMPORTANT", "important"),
            ("CAUTION", "caution"),
        ] {
            assert_eq!(doc(&format!("> [!{kind}]\n> x"))[0]["alert"], alert);
        }
    }

    #[test]
    fn the_whole_document_is_a_text_view_document() {
        let blocks = doc("# T\n\n- a\n\n> [!TIP]\n> b\n\n| x |\n|---|\n| y |");
        let document: nsh_base::text_view::Document =
            serde_json::from_value(json!({ "blocks": blocks })).unwrap();
        assert_eq!(document.plain_text(), "T\n\na\n\nb\n\nx\ny");
    }

    const STREAMED: &[&str] = &[
        "# Plan 🦀\n\nFirst **bold** line\nsecond line with 中文 and é.\n\n```rust src/main.rs\nfn main() {\n    println!(\"🦀\");\n}\n```\n\n- one\n- [x] two\n  - nested 日本\n\n> [!NOTE]\n> careful 👩‍👩‍👧\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n:codex-file-citation{path=\"/out/Bericht März.docx\"}\n\nTail ~~done~~ 🎉",
        "Intro\n\n1. a\n\n2. b\n\n   more\n\nAfter the list\n\n```\nopen fence 中\n\nstill code",
        "```\r\nwindows\r\n```\r\n\r\nTail\r\n",
        "\u{feff}# BOM\n\ntext",
    ];

    #[test]
    fn streaming_char_by_char_matches_a_full_parse_and_never_resends_sealed_blocks() {
        for source in STREAMED {
            let mut document = StreamingDocument::new();
            let mut shown: Vec<Value> = Vec::new();
            let ends = source.char_indices().map(|(at, _)| at).skip(1);
            for end in ends.chain([source.len()]) {
                let chunk = &source[..end];
                let sealed = document.cache.stable.map_or(0, |(_, blocks)| blocks);
                let reusable = !chunk.contains(['\r', '\u{feff}']);
                let splice = document.update(chunk);
                let full = doc(chunk);
                if reusable {
                    assert!(splice.from >= sealed, "{chunk:?}: {splice:?} < {sealed}");
                }
                // `from` is exactly the first block whose JSON differs.
                let unchanged = shown.iter().zip(&full).take_while(|(a, b)| a == b).count();
                assert_eq!(splice.from, unchanged, "{chunk:?}");
                assert_eq!(splice.len, splice.from + splice.blocks.len());
                // Applying the splice gives what a full conversion gives.
                shown.truncate(splice.from);
                shown.extend(splice.blocks);
                assert_eq!(shown, full, "{chunk:?}");
                assert_eq!(document.blocks(), full.as_slice());
                assert_eq!(**document.parsed(), parse(chunk));
            }
            read_back(&shown);
        }
    }

    #[test]
    fn a_splice_starts_at_the_first_changed_block_and_carries_only_the_tail() {
        let mut document = StreamingDocument::new();
        let head = "# A\n\nfirst\n\n```\ncode\n```\n\n";
        let splice = document.update(head);
        assert_eq!((splice.from, splice.blocks.len(), splice.len), (0, 3, 3));
        // Appending to the open tail resends only the tail.
        let splice = document.update(&format!("{head}Tail"));
        assert_eq!((splice.from, splice.blocks.len(), splice.len), (3, 1, 4));
        let splice = document.update(&format!("{head}Tail **grows**"));
        assert_eq!((splice.from, splice.blocks.len(), splice.len), (3, 1, 4));
        // The same text is no change.
        let splice = document.update(&format!("{head}Tail **grows**"));
        assert_eq!((splice.from, splice.blocks.len(), splice.len), (4, 0, 4));
        // Text that leaves the blocks as they were is no change either.
        let splice = document.update(&format!("{head}Tail **grows**\n"));
        assert_eq!((splice.from, splice.blocks.len(), splice.len), (4, 0, 4));
        // A shorter replacement truncates; an edit in the middle starts there.
        let splice = document.update("# A\n\nfirst");
        assert_eq!((splice.from, splice.blocks.len(), splice.len), (2, 0, 2));
        let splice = document.update("# A\n\nchanged\n\nmore");
        assert_eq!((splice.from, splice.len), (1, 3));
        assert_eq!(splice.blocks, doc("# A\n\nchanged\n\nmore")[1..]);
        let splice = document.update("");
        assert_eq!((splice.from, splice.blocks.len(), splice.len), (0, 0, 0));
    }

    #[test]
    fn a_long_streamed_answer_converts_only_its_tail() {
        let mut source = String::new();
        let mut document = StreamingDocument::new();
        for section in 0..200 {
            source.push_str(&format!(
                "## Section {section}\n\nSome **text** {section}.\n\n"
            ));
            let splice = document.update(&source);
            // Each chunk adds a heading and a paragraph after the sealed blocks.
            assert_eq!(splice.from, section * 2, "{splice:?}");
            assert_eq!(splice.blocks.len(), 2);
        }
        assert_eq!(document.blocks(), doc(&source).as_slice());
    }

    #[test]
    fn hostile_input_neither_panics_nor_runs_away() {
        let deep = ">".repeat(500) + " x";
        let stars = "*a ".repeat(5000);
        let brackets = "[".repeat(3000) + &"](".repeat(3000);
        let lists = "- a\n".to_owned() + &"  - b\n".repeat(200);
        let citations = ":codex-file-citation{path=\"".repeat(500);
        let nested = "- ".repeat(300) + "x";
        let long = "**x ".repeat(20_000);
        for source in [
            deep.as_str(),
            &stars,
            &brackets,
            &lists,
            &citations,
            &nested,
            &long,
            "",
            "\n\n",
            "| |\n|-|",
            "```",
            "- ",
            "- [x]",
            "1.",
            "> ",
            "> [!NOTE]",
            "[](",
            "![](",
            "![]()",
            "é*é*é",
            "**é**",
            "\u{0}\u{feff}\r\r\n",
        ] {
            let blocks = doc(source);
            read_back(&blocks);
            // A few uneven chunks: the pathological ones are slow to parse at all.
            let step = 997.max(source.len() / 3);
            let mut document = StreamingDocument::new();
            let mut end = 0;
            while end < source.len() {
                end = (end + step).min(source.len());
                while !source.is_char_boundary(end) {
                    end += 1;
                }
                document.update(&source[..end]);
            }
            assert_eq!(document.blocks(), blocks.as_slice());
        }
    }
}
