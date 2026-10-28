//! Reading a file change's unified diff for display: which lines were added, removed or are
//! context, and how many of each.
//!
//! The hunk headers (`@@ -1,3 +1,4 @@`) say how many lines the hunk holds, which is what
//! tells a removed line that starts with `--` (a deleted SQL comment, say) from the `---`
//! header of the next file. Providers do not always send a full diff: Codex may send just
//! the contents of a new file, so a diff without any header is read by the kind of change.

use riwork_engine::chat::model::ChangeKind;

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

/// Old/new source coordinates, independent of presentation and copied patch text.
pub fn line_numbers(
    lines: &[DiffLine<'_>],
    change: ChangeKind,
) -> Vec<(Option<usize>, Option<usize>)> {
    let structured = lines.iter().any(|line| line.kind == LineKind::Hunk);
    let mut old = (!structured && change == ChangeKind::Delete).then_some(1usize);
    let mut new = (!structured && change == ChangeKind::Add).then_some(1usize);
    lines
        .iter()
        .map(|line| {
            if line.kind == LineKind::Hunk {
                let mut parts = line.text.split_whitespace().skip(1);
                let start = |part: Option<&str>, sign| {
                    part?
                        .strip_prefix(sign)?
                        .split(',')
                        .next()?
                        .parse::<usize>()
                        .ok()
                };
                old = start(parts.next(), '-');
                new = start(parts.next(), '+');
                return (None, None);
            }
            let result = match line.kind {
                LineKind::Add => (None, new),
                LineKind::Remove => (old, None),
                LineKind::Context => (old, new),
                _ => (None, None),
            };
            if matches!(line.kind, LineKind::Remove | LineKind::Context) {
                old = old.and_then(|n| n.checked_add(1));
            }
            if matches!(line.kind, LineKind::Add | LineKind::Context) {
                new = new.and_then(|n| n.checked_add(1));
            }
            result
        })
        .collect()
}

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
