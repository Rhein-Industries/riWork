//! Bounded syntax coloring for selectable native transcript text.
//! The cache owns exact source strings, so same-length edits never reuse stale tokens.

use std::{cell::RefCell, collections::VecDeque, ops::Range, sync::LazyLock};

use gpui::{FontStyle, FontWeight, HighlightStyle, rgb};
use syntect::{
    easy::HighlightLines,
    highlighting::{FontStyle as SyntaxFontStyle, HighlightState, ThemeSet},
    parsing::{ParseState, SyntaxSet},
    util::LinesWithEndings,
};

const MAX_BYTES: usize = 128 * 1024;
const MAX_LINE: usize = 16 * 1024;
const MAX_SPANS: usize = 16 * 1024;
const CACHE_ENTRIES: usize = 32;
const CACHE_BYTES: usize = 8 * 1024 * 1024;

static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
static THEMES: LazyLock<ThemeSet> = LazyLock::new(ThemeSet::load_defaults);

struct Entry {
    source: String,
    language: String,
    light: bool,
    spans: Vec<(Range<usize>, HighlightStyle)>,
    // State immediately after the last complete line; never retain a partial line's state.
    prefix: usize,
    prefix_spans: usize,
    state: Option<(HighlightState, ParseState)>,
}

impl Entry {
    fn bytes(&self) -> usize {
        self.source.len()
            + self.language.len()
            + self.spans.len() * std::mem::size_of::<(Range<usize>, HighlightStyle)>()
    }
}

thread_local! {
    static CACHE: RefCell<VecDeque<Entry>> = const { RefCell::new(VecDeque::new()) };
}

fn syntax_token<'a>(language: Option<&'a str>, title: Option<&'a str>) -> &'a str {
    let token = language.filter(|s| !s.is_empty()).unwrap_or_else(|| {
        title
            .and_then(|s| s.rsplit_once('.').map(|(_, ext)| ext))
            .unwrap_or("")
    });
    match token {
        "javascript" | "jsx" | "typescript" | "tsx" | "ts" => "js",
        "shell" | "shellscript" | "bash" | "zsh" | "console" => "sh",
        "python" => "py",
        "csharp" | "c#" => "cs",
        "yml" => "yaml",
        _ => token,
    }
}

pub(super) fn highlight(
    language: Option<&str>,
    title: Option<&str>,
    source: &str,
    background: u32,
) -> Vec<(Range<usize>, HighlightStyle)> {
    if source.len() > MAX_BYTES || source.lines().any(|line| line.len() > MAX_LINE) {
        return Vec::new();
    }
    let language = syntax_token(language, title).to_ascii_lowercase();
    if language.is_empty() || matches!(language.as_str(), "text" | "plain" | "plaintext" | "txt") {
        return Vec::new();
    }
    let light = ((background >> 16) & 255) * 299
        + ((background >> 8) & 255) * 587
        + (background & 255) * 114
        > 128_000;
    CACHE.with_borrow_mut(|cache| {
        if let Some(at) = cache.iter().position(|entry| {
            entry.source == source && entry.language == language && entry.light == light
        }) {
            let entry = cache.remove(at).expect("a cached entry");
            let spans = entry.spans.clone();
            cache.push_back(entry);
            return spans;
        }
        let previous = cache
            .iter()
            .rposition(|entry| {
                entry.language == language
                    && entry.light == light
                    && entry.prefix > 0
                    && source.starts_with(&entry.source)
                    && !source.contains('\r')
            })
            .and_then(|at| cache.remove(at));
        let entry = tokenize(&language, source, light, previous.as_ref());
        let spans = entry.spans.clone();
        let bytes = entry.bytes();
        while !cache.is_empty()
            && (cache.len() >= CACHE_ENTRIES
                || cache.iter().map(Entry::bytes).sum::<usize>() + bytes > CACHE_BYTES)
        {
            cache.pop_front();
        }
        if bytes <= CACHE_BYTES {
            cache.push_back(entry);
        }
        spans
    })
}

fn tokenize(language: &str, source: &str, light: bool, previous: Option<&Entry>) -> Entry {
    let mut entry = Entry {
        source: source.into(),
        language: language.into(),
        light,
        spans: Vec::new(),
        prefix: 0,
        prefix_spans: 0,
        state: None,
    };
    let Some(syntax) = SYNTAXES.find_syntax_by_token(language) else {
        return entry;
    };
    let theme_name = if light {
        "InspiredGitHub"
    } else {
        "base16-ocean.dark"
    };
    let Some(theme) = THEMES.themes.get(theme_name) else {
        return entry;
    };
    let mut highlighter = match previous.and_then(|entry| entry.state.as_ref()) {
        Some((highlight, parse)) => {
            HighlightLines::from_state(theme, highlight.clone(), parse.clone())
        }
        None => HighlightLines::new(syntax, theme),
    };
    let mut offset = 0;
    if let Some(previous) = previous.filter(|entry| entry.state.is_some()) {
        offset = previous.prefix;
        entry
            .spans
            .extend_from_slice(&previous.spans[..previous.prefix_spans]);
    }
    let completed = source.rfind('\n').map_or(0, |at| at + 1);
    let mut append =
        |highlighter: &mut HighlightLines<'_>, text: &str, offset: &mut usize| -> Option<()> {
            for line in LinesWithEndings::from(text) {
                for (style, text) in highlighter.highlight_line(line, &SYNTAXES).ok()? {
                    let end = *offset + text.len();
                    if !text.is_empty() && !text.chars().all(char::is_whitespace) {
                        let color = style.foreground;
                        let mut highlight = HighlightStyle {
                            color: Some(
                                rgb((u32::from(color.r) << 16)
                                    | (u32::from(color.g) << 8)
                                    | u32::from(color.b))
                                .into(),
                            ),
                            ..HighlightStyle::default()
                        };
                        if style.font_style.contains(SyntaxFontStyle::BOLD) {
                            highlight.font_weight = Some(FontWeight::BOLD);
                        }
                        if style.font_style.contains(SyntaxFontStyle::ITALIC) {
                            highlight.font_style = Some(FontStyle::Italic);
                        }
                        entry.spans.push((*offset..end, highlight));
                        if entry.spans.len() > MAX_SPANS {
                            return None;
                        }
                    }
                    *offset = end;
                }
            }
            Some(())
        };
    if append(&mut highlighter, &source[offset..completed], &mut offset).is_none() {
        entry.spans.clear();
        return entry;
    }
    let state = highlighter.state();
    let prefix_spans = entry.spans.len();
    // The unfinished final line is rendered from a clone, leaving the completed prefix valid
    // when a quote/comment/escape at the end of that line arrives in a later delta.
    let mut tail = HighlightLines::from_state(theme, state.0.clone(), state.1.clone());
    // End the first closure's mutable borrow before inspecting/copying the span count.
    for line in LinesWithEndings::from(&source[completed..]) {
        let Ok(tokens) = tail.highlight_line(line, &SYNTAXES) else {
            entry.spans.clear();
            return entry;
        };
        for (style, text) in tokens {
            let end = offset + text.len();
            if !text.is_empty() && !text.chars().all(char::is_whitespace) {
                let color = style.foreground;
                let mut highlight = HighlightStyle {
                    color: Some(
                        rgb((u32::from(color.r) << 16)
                            | (u32::from(color.g) << 8)
                            | u32::from(color.b))
                        .into(),
                    ),
                    ..HighlightStyle::default()
                };
                if style.font_style.contains(SyntaxFontStyle::BOLD) {
                    highlight.font_weight = Some(FontWeight::BOLD);
                }
                if style.font_style.contains(SyntaxFontStyle::ITALIC) {
                    highlight.font_style = Some(FontStyle::Italic);
                }
                entry.spans.push((offset..end, highlight));
                if entry.spans.len() > MAX_SPANS {
                    entry.spans.clear();
                    return entry;
                }
            }
            offset = end;
        }
    }
    entry.prefix = completed;
    entry.prefix_spans = prefix_spans;
    entry.state = Some(state);
    entry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_tokens_have_distinct_colors_and_unicode_boundaries() {
        let source = "fn main() { let café = \"こんにちは\"; // comment\n}";
        let spans = highlight(Some("rust"), None, source, 0x101010);
        assert!(spans.len() > 3);
        assert!(
            spans
                .windows(2)
                .any(|pair| pair[0].1.color != pair[1].1.color)
        );
        for (range, _) in spans {
            assert!(source.get(range).is_some());
        }
    }

    #[test]
    fn exact_edits_theme_changes_and_filename_fallback_are_not_stale() {
        let first = "let a = \"x\";";
        let second = "//t a = \"x\";";
        assert_eq!(first.len(), second.len());
        let a = highlight(Some("rust"), None, first, 0x101010);
        assert_ne!(a, highlight(Some("rust"), None, second, 0x101010));
        assert_ne!(a, highlight(Some("rust"), None, first, 0xffffff));
        assert_eq!(a, highlight(None, Some("src/main.rs"), first, 0x101010));
    }

    #[test]
    fn unsupported_and_oversized_inputs_keep_plain_text() {
        assert!(highlight(Some("invented-language"), None, "hello", 0).is_empty());
        assert!(highlight(Some("rust"), None, &"x".repeat(MAX_LINE + 1), 0).is_empty());
        assert!(highlight(Some("rust"), None, &"x\n".repeat(MAX_BYTES), 0).is_empty());
    }

    #[test]
    fn every_streamed_prefix_matches_a_fresh_parse_across_multiline_syntax() {
        let source = "fn main() {\n/* a comment\ncontinues */ let text = r#\"héllo\nworld\"#;\n}\n";
        let mut previous = None;
        for (end, _) in source
            .char_indices()
            .skip(1)
            .chain(std::iter::once((source.len(), ' ')))
        {
            let streamed = tokenize("rust", &source[..end], false, previous.as_ref());
            let fresh = tokenize("rust", &source[..end], false, None);
            assert_eq!(streamed.spans, fresh.spans, "prefix ending at {end}");
            previous = Some(streamed);
        }
    }
}
