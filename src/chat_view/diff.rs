//! Reading a file change's unified diff for display: which lines were added, removed or are
//! context, and how many of each.
//!
//! The hunk headers (`@@ -1,3 +1,4 @@`) say how many lines the hunk holds, which is what
//! tells a removed line that starts with `--` (a deleted SQL comment, say) from the `---`
//! header of the next file. Providers do not always send a full diff: Codex may send just
//! the contents of a new file, so a diff without any header is read by the kind of change.

use crate::chat::model::ChangeKind;

/// What a line of a diff is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    /// `diff --git`, `index`, `---`, `+++` and the like.
    Header,
    /// `@@ -a,b +c,d @@`
    Hunk,
    Add,
    Remove,
    Context,
    /// `\ No newline at end of file`
    Note,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiffLine<'a> {
    pub kind: LineKind,
    /// The line as it is in the diff, with its `+`, `-` or space.
    pub text: &'a str,
}

/// Lines drawn at most for one file; the rest is counted, not drawn.
pub const MAX_LINES: usize = 400;

pub fn classify(diff: &str, change: ChangeKind) -> Vec<DiffLine<'_>> {
    let has_structure = diff.lines().any(|line| {
        line.starts_with("@@") || line.starts_with("diff ") || line.starts_with("--- ")
    });
    if !has_structure {
        return unstructured(diff, change);
    }
    let mut lines = Vec::new();
    // Lines of the current hunk still to come, old side and new side.
    let (mut old_left, mut new_left) = (0usize, 0usize);
    for text in diff.lines() {
        let in_hunk = old_left > 0 || new_left > 0;
        let first = text.chars().next();
        let kind = match first {
            _ if in_hunk && first == Some('+') => {
                new_left = new_left.saturating_sub(1);
                LineKind::Add
            }
            _ if in_hunk && first == Some('-') => {
                old_left = old_left.saturating_sub(1);
                LineKind::Remove
            }
            _ if in_hunk && matches!(first, Some(' ') | None) => {
                old_left = old_left.saturating_sub(1);
                new_left = new_left.saturating_sub(1);
                LineKind::Context
            }
            _ if in_hunk && first == Some('\\') => LineKind::Note,
            _ if text.starts_with("@@") => {
                let (old, new) = hunk_lengths(text).unwrap_or((0, 0));
                old_left = old;
                new_left = new;
                LineKind::Hunk
            }
            // Outside a hunk, or a hunk that ended early: the counts were wrong, trust the
            // line.
            _ => {
                old_left = 0;
                new_left = 0;
                by_prefix(text)
            }
        };
        lines.push(DiffLine { kind, text });
    }
    lines
}

/// The line counts of a hunk header: `@@ -a[,b] +c[,d] @@`.
fn hunk_lengths(header: &str) -> Option<(usize, usize)> {
    let mut parts = header.split_whitespace();
    parts.next()?;
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    let length = |range: &str| match range.split_once(',') {
        Some((_, count)) => count.parse().ok(),
        None => Some(1),
    };
    Some((length(old)?, length(new)?))
}

/// A line outside any hunk, by how it starts.
fn by_prefix(text: &str) -> LineKind {
    const HEADERS: [&str; 12] = [
        "diff ",
        "index ",
        "--- ",
        "+++ ",
        "new file",
        "deleted file",
        "old mode",
        "new mode",
        "similarity ",
        "rename ",
        "copy ",
        "Binary files",
    ];
    if HEADERS.iter().any(|header| text.starts_with(header)) {
        return LineKind::Header;
    }
    match text.chars().next() {
        Some('+') => LineKind::Add,
        Some('-') => LineKind::Remove,
        Some('\\') => LineKind::Note,
        _ => LineKind::Context,
    }
}

/// A diff with no headers: the contents of a file that was added or deleted, or lines that
/// carry their own `+` and `-`.
fn unstructured(diff: &str, change: ChangeKind) -> Vec<DiffLine<'_>> {
    let prefixed = diff
        .lines()
        .any(|line| line.starts_with('+') || line.starts_with('-'));
    diff.lines()
        .map(|text| DiffLine {
            kind: match change {
                ChangeKind::Add if !prefixed => LineKind::Add,
                ChangeKind::Delete if !prefixed => LineKind::Remove,
                _ => by_prefix(text),
            },
            text,
        })
        .collect()
}

/// Lines added and lines removed.
pub fn stats(lines: &[DiffLine<'_>]) -> (usize, usize) {
    lines
        .iter()
        .fold((0, 0), |(added, removed), line| match line.kind {
            LineKind::Add => (added + 1, removed),
            LineKind::Remove => (added, removed + 1),
            _ => (added, removed),
        })
}

/// The first `MAX_LINES` lines of `diff` classified, and how many lines there are beyond them.
/// Only the lines drawn are read, so a diff of any size costs the same to draw.
pub fn display(diff: &str, change: ChangeKind) -> (Vec<DiffLine<'_>>, usize) {
    let cut = diff
        .match_indices('\n')
        .nth(MAX_LINES - 1)
        .map_or(diff.len(), |(at, _)| at);
    let hidden = if cut < diff.len() {
        diff[cut..].lines().count().saturating_sub(1)
    } else {
        0
    };
    (classify(&diff[..cut], change), hidden)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(diff: &str, change: ChangeKind) -> Vec<LineKind> {
        classify(diff, change)
            .iter()
            .map(|line| line.kind)
            .collect()
    }

    use LineKind::*;

    #[test]
    fn a_unified_diff_is_split_into_headers_hunks_and_changes() {
        let diff = "diff --git a/a.rs b/a.rs\nindex 1..2 100644\n--- a/a.rs\n+++ b/a.rs\n@@ -1,3 +1,3 @@\n fn a() {\n-    1\n+    2\n }\n";
        assert_eq!(
            kinds(diff, ChangeKind::Modify),
            [
                Header, Header, Header, Header, Hunk, Context, Remove, Add, Context
            ]
        );
        let lines = classify(diff, ChangeKind::Modify);
        assert_eq!(stats(&lines), (1, 1));
        assert_eq!(lines[6].text, "-    1");
    }

    #[test]
    fn hunk_counts_tell_a_removed_comment_from_a_file_header() {
        // `-- comment` removed shows as `--- comment`; `++ x` added shows as `+++ x`.
        let diff = "--- a/q.sql\n+++ b/q.sql\n@@ -1,2 +1,2 @@\n--- comment\n+++ note\n select 1;\n";
        assert_eq!(
            kinds(diff, ChangeKind::Modify),
            [Header, Header, Hunk, Remove, Add, Context]
        );
    }

    #[test]
    fn an_empty_context_line_and_the_no_newline_note_stay_in_the_hunk() {
        let diff = "@@ -1,3 +1,3 @@\n a\n\n-b\n\\ No newline at end of file\n+c\n\\ No newline at end of file\n";
        assert_eq!(
            kinds(diff, ChangeKind::Modify),
            [Hunk, Context, Context, Remove, Note, Add, Note]
        );
    }

    #[test]
    fn a_hunk_shorter_than_it_says_does_not_swallow_what_follows() {
        let diff = "@@ -1,9 +1,9 @@\n-a\n+b\ndiff --git a/x b/x\n--- a/x\n";
        assert_eq!(
            kinds(diff, ChangeKind::Modify),
            [Hunk, Remove, Add, Header, Header]
        );
    }

    #[test]
    fn a_diff_without_headers_is_read_by_the_kind_of_change() {
        // The contents of a new file.
        assert_eq!(kinds("one\ntwo\n", ChangeKind::Add), [Add, Add]);
        // The contents of a deleted one.
        assert_eq!(kinds("one\ntwo", ChangeKind::Delete), [Remove, Remove]);
        // Lines that carry their own marks are read as they are.
        assert_eq!(
            kinds("+one\n-two\n three", ChangeKind::Modify),
            [Add, Remove, Context]
        );
        assert_eq!(kinds("+one\ntwo", ChangeKind::Add), [Add, Context]);
        assert!(classify("", ChangeKind::Modify).is_empty());
    }
}
