//! Desktop adapter for the shared transcript grouping v2 projection.
//!
//! The engine owns grouping, counts, disclosure defaults and anchor remapping.
//! This module only translates persisted display settings and list indices.
//! Worker/subagent cards are top-level Normal rows between activity and answer;
//! Verbose retains their transcript position. Keep these projected identities
//! when dispatching native cards and mapping targeted worker updates.
use crate::chat::model::Transcript;
use riwork_engine::chat::projection::{self, DisclosureState, ProjectionOptions};

pub use riwork_core::settings::DisplayMode;
pub(super) use riwork_engine::chat::projection::{Row, RowKind};

/// Every disclosed header/body is its own virtual-list item. Keeping a whole
/// expanded turn in one list item causes GPUI to lay out its offscreen calls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ListRow {
    pub row: Row,
    pub depth: usize,
}

pub(super) fn list_rows(rows: &[Row]) -> Vec<ListRow> {
    fn append(rows: &[Row], depth: usize, out: &mut Vec<ListRow>) {
        for row in rows {
            let mut leaf = row.clone();
            leaf.children.clear();
            leaf.comments.clear();
            out.push(ListRow { row: leaf, depth });
            if row.open {
                append(&row.children, depth + 1, out);
            }
            append(&row.comments, depth, out);
        }
    }
    let mut out = Vec::new();
    append(rows, 0, &mut out);
    out
}

pub(super) fn cached_rows(
    cache: &mut projection::ProjectionCache,
    transcript: &Transcript,
    mode: DisplayMode,
    disclosure: &DisclosureState,
    now_ms: Option<u64>,
    invalidated: &std::collections::HashSet<String>,
) -> Vec<Row> {
    cache.project(
        transcript,
        &ProjectionOptions {
            mode: match mode {
                DisplayMode::Normal => projection::DisplayMode::Normal,
                DisplayMode::Verbose => projection::DisplayMode::Verbose,
            },
            now_ms,
            disclosure: disclosure.clone(),
            ..ProjectionOptions::default()
        },
        invalidated,
    )
}

#[cfg(test)]
pub(super) fn rows(
    transcript: &Transcript,
    mode: DisplayMode,
    disclosure: &DisclosureState,
    now_ms: Option<u64>,
) -> Vec<Row> {
    projection::project_transcript(
        transcript,
        &ProjectionOptions {
            mode: match mode {
                DisplayMode::Normal => projection::DisplayMode::Normal,
                DisplayMode::Verbose => projection::DisplayMode::Verbose,
            },
            now_ms,
            disclosure: disclosure.clone(),
            ..ProjectionOptions::default()
        },
    )
}

fn header_title(row: &Row, item: Option<&crate::chat::model::Item>) -> String {
    let title = if row.kind == RowKind::Mcp || (row.kind == RowKind::Edit && row.target.is_none()) {
        item.and_then(|item| super::cards::head(item, None))
            .map(|head| head.title)
            .unwrap_or_else(|| row.title.clone())
    } else {
        row.title.clone()
    };
    // Dynamic activity labels can contain a whole multiline command. A header
    // always occupies one line, independently of its expanded body.
    super::cards::first_line(&title)
}

pub(super) fn header_target(row: &Row, item: Option<&crate::chat::model::Item>) -> Option<String> {
    row.target
        .as_deref()
        .filter(|target| !target.trim().is_empty())
        .map(super::cards::first_line)
        .filter(|target| target.trim() != header_title(row, item).trim())
}

/// The plain text part of the native header. Metadata keeps its separate grey style.
pub(super) fn header_text(row: &Row, item: Option<&crate::chat::model::Item>) -> String {
    let title = header_title(row, item);
    match header_target(row, item) {
        Some(target) => format!("{title} {target}"),
        None => title,
    }
}

pub(super) fn find<'a>(rows: &'a [Row], id: &str) -> Option<&'a Row> {
    rows.iter().find_map(|row| {
        if row.id == id {
            Some(row)
        } else {
            find(&row.children, id).or_else(|| find(&row.comments, id))
        }
    })
}

/// The stable item behind a list anchor, even when its group ID changes.
pub(super) fn anchor_item(row: &Row) -> &str {
    row.children.first().map_or(row.id.as_str(), anchor_item)
}

pub(super) fn disclosure(row: &Row) -> bool {
    !matches!(
        row.kind,
        RowKind::User
            | RowKind::Final
            | RowKind::Commentary
            | RowKind::Compaction
            | RowKind::Notice
            | RowKind::Request
    )
}

/// Keyboard order includes only visible disclosure headers, in transcript order.
pub(super) fn focusable_rows(rows: &[Row]) -> Vec<String> {
    let mut result = Vec::new();
    for row in rows {
        if disclosure(row) {
            result.push(row.id.clone());
        }
        if row.open {
            result.extend(focusable_rows(&row.children));
        }
        result.extend(focusable_rows(&row.comments));
    }
    result
}

pub(super) fn open_ids(rows: &[Row], result: &mut Vec<String>) {
    for row in rows {
        if row.open {
            result.push(row.id.clone());
        }
        open_ids(&row.children, result);
        open_ids(&row.comments, result);
    }
}
