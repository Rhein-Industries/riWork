//! The current visible transcript's source export, independent of virtualization.
//! This contains no pointer/gesture state or character hit-testing algorithm.
use super::{
    ChatView, cards, diff,
    display::{self, RowKind},
    markdown::{self, Block, Span},
    widgets::Look,
};
use crate::chat::model::{Item, ItemBody, NoticeLevel};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SourceLeaf {
    pub key: String,
    pub text: String,
    pub row: usize,
}

pub(super) fn leaves(view: &ChatView, look: Look) -> Vec<SourceLeaf> {
    if let Some(source) = view.review_source() {
        return source;
    }
    let mut result = Vec::new();
    for (row, entry) in view.list_rows.iter().enumerate() {
        result.extend(
            view.transcript_selection
                .cached_source(&entry.row, row)
                .unwrap_or_else(|| uncached_row_leaves(view, row, look)),
        );
    }
    result
}

/// Bodies already visited by the virtual list. Offscreen output cleanup/markdown
/// parsing is deferred until that row is drawn or copied; headers are controls.
/// A focused file review is its own, complete document.
pub(super) fn mounted_leaves(view: &ChatView) -> Vec<SourceLeaf> {
    if let Some(source) = view.review_source() {
        return source;
    }
    let mut result = Vec::new();
    for (at, entry) in view.list_rows.iter().enumerate() {
        if let Some(source) = view.transcript_selection.cached_source(&entry.row, at) {
            result.extend(source);
        }
    }
    result
}

/// Refresh a streaming body without rebuilding the historical selection document.
pub(super) fn row_leaves(view: &ChatView, row: usize, look: Look) -> Vec<SourceLeaf> {
    if let Some(entry) = view.list_rows.get(row) {
        let top = &entry.row;
        if let Some(source) = view.transcript_selection.cached_source(top, row) {
            return source;
        }
        let result = uncached_row_leaves(view, row, look);
        view.transcript_selection.cache_source(top, &result);
        return result;
    }
    Vec::new()
}

/// Copy may traverse unmounted history without retaining a second transcript's
/// worth of cleaned output. Only rows visited by the renderer enter the cache.
fn uncached_row_leaves(view: &ChatView, row: usize, look: Look) -> Vec<SourceLeaf> {
    let mut result = Vec::new();
    if let Some(entry) = view.list_rows.get(row) {
        let top = &entry.row;
        // Shared selection rules decide which headers/children/comments are visible.
        let visible: HashSet<_> =
            riwork_engine::chat::projection::selection_leaves(std::slice::from_ref(top))
                .into_iter()
                .map(|(id, _)| id)
                .collect();
        append_row(view, top, row, look, &visible, &mut result);
    }
    // A drawn diagram replaces its code block; the hidden source is not selectable.
    result.retain(|leaf| !view.diagram_hides_source(&leaf.key, &leaf.text, look));
    result
}

/// Search has its own source: closing a disclosure must not remove its matches,
/// and hidden text must never become part of ordinary selection or Copy.
#[derive(Default)]
pub(super) struct SearchSource {
    pub leaves: Vec<SourceLeaf>,
    /// What each leaf needs opened to be drawn: projected rows (disclosure
    /// overrides) and the body helpers' own keys (`ChatView::open`).
    pub disclosures: HashMap<String, Vec<String>>,
    /// The projected row each leaf is drawn in, whose cached source a detail invalidates.
    pub owners: HashMap<String, String>,
}

/// Every projected row, open or not, in list order. A hidden leaf's `row` is the
/// list row of its nearest drawn ancestor, so matches keep their reading order.
pub(super) fn search_source(view: &ChatView, look: Look) -> SearchSource {
    if let Some(leaves) = view.review_source() {
        return SearchSource {
            leaves,
            ..Default::default()
        };
    }
    let listed: HashMap<&str, usize> = view
        .list_rows
        .iter()
        .enumerate()
        .map(|(at, entry)| (entry.row.id.as_str(), at))
        .collect();
    let mut result = SearchSource::default();
    let mut closed = Vec::new();
    search_rows(
        view,
        &view.visible,
        0,
        look,
        &listed,
        &mut closed,
        &mut result,
    );
    result
}

fn search_rows(
    view: &ChatView,
    rows: &[display::Row],
    parent_row: usize,
    look: Look,
    listed: &HashMap<&str, usize>,
    closed: &mut Vec<String>,
    result: &mut SearchSource,
) {
    for projected in rows {
        let row = listed
            .get(projected.id.as_str())
            .copied()
            .unwrap_or(parent_row);
        let start = result.leaves.len();
        append_own(view, projected, row, look, true, &mut result.leaves);
        let item = view.projected_item(&projected.id).map(|(_, item)| item);
        // A closed call's body opens with the call; its artifact chips do not need it.
        let own_closed = !projected.open && display::disclosure(projected);
        for leaf in &result.leaves[start..] {
            let mut required = closed.clone();
            if own_closed && !leaf.key.starts_with("artifact:") {
                required.push(projected.id.clone());
            }
            if let Some(item) = item {
                required.extend(body_disclosures(view, item, &leaf.key));
            }
            result.disclosures.insert(leaf.key.clone(), required);
            result.owners.insert(leaf.key.clone(), projected.id.clone());
        }
        let pushed = own_closed;
        if pushed {
            closed.push(projected.id.clone());
        }
        search_rows(view, &projected.children, row, look, listed, closed, result);
        if pushed {
            closed.pop();
        }
        // Comments sit beside a disclosure, so they do not need it open.
        search_rows(view, &projected.comments, row, look, listed, closed, result);
    }
}

/// The body helpers' keys a hidden leaf of `item` needs, beyond its projected rows.
fn body_disclosures(view: &ChatView, item: &Item, key: &str) -> Vec<String> {
    let mut required = Vec::new();
    if let Some(code) = key.strip_prefix("code:") {
        required.push(super::diagram::source_key(code));
    }
    if (key.starts_with("tool-input:") || key.starts_with("tool-output:"))
        && view.tool_output(item).is_some()
    {
        required.push(super::tool_views::raw_key(item));
    }
    if let Some((file, _)) = key
        .strip_prefix("diff:")
        .and_then(|key| key.rsplit_once(':'))
    {
        required.push(file.to_owned());
    }
    if matches!(item.body, ItemBody::Plan { .. }) {
        required.push(super::plan_document::expansion_key(&item.id));
    }
    required
}

fn append_row(
    view: &ChatView,
    projected: &display::Row,
    row: usize,
    look: Look,
    visible: &HashSet<String>,
    result: &mut Vec<SourceLeaf>,
) {
    if !visible.contains(&projected.id) {
        return;
    }
    append_own(view, projected, row, look, false, result);
    for child in &projected.children {
        append_row(view, child, row, look, visible, result);
    }
    for comment in &projected.comments {
        append_row(view, comment, row, look, visible, result);
    }
}

/// One projected row's own leaves, as `rows` draws them: its item's body and its
/// artifact chips. `search` exports a closed body as if it were open.
fn append_own(
    view: &ChatView,
    projected: &display::Row,
    row: usize,
    look: Look,
    search: bool,
    result: &mut Vec<SourceLeaf>,
) {
    let item = view.projected_item(&projected.id);
    if let Some((at, item)) = item {
        append_item(view, projected, at, item, row, look, search, result);
    }
    if projected.kind == RowKind::Activity {
        artifacts(view, projected.artifacts.iter(), row, result);
    } else if projected.kind != RowKind::User {
        let item = item.map(|(_, item)| item);
        artifacts(
            view,
            projected.artifacts.iter().filter(|artifact| {
                super::media::artifact_chip_shown(
                    view.display_mode,
                    projected.kind,
                    artifact,
                    item,
                    projected.open,
                )
            }),
            row,
            result,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn append_item(
    view: &ChatView,
    projected: &display::Row,
    at: usize,
    item: &Item,
    row: usize,
    look: Look,
    search: bool,
    result: &mut Vec<SourceLeaf>,
) {
    // The native cards own these result bodies. Their live tails are status
    // displays, never selectable or searchable transcript text.
    if let Some(worker) = crate::chat::workers::of(item) {
        let tail = worker
            .end
            .as_ref()
            .map(|end| end.tail.clone())
            .unwrap_or_default();
        if (search || view.open.contains(&item.id))
            && !tail.trim().is_empty()
            && !view.worker_status(at, worker).running()
        {
            result.push(SourceLeaf {
                key: format!("worker:{}", item.id),
                text: tail,
                row,
            });
        }
        return;
    }
    if let Some(run) = crate::chat::subagents::in_transcript(
        item,
        &view.model.transcript,
        view.info().map(|info| info.provider),
    ) {
        let text = run.result_text();
        if (search || view.open.contains(&item.id)) && !text.is_empty() {
            result.push(SourceLeaf {
                key: format!("subagent:{}", item.id),
                text,
                row,
            });
        }
        return;
    }
    let expanded = search || projected.open;
    let mut entries = Vec::new();
    let mut put = |key: String, text: String| entries.push(SourceLeaf { key, text, row });
    match &item.body {
        ItemBody::AgentMessage { text } if projected.kind == RowKind::Commentary => {
            put(item.id.clone(), markdown::cited_text(text).0.into_owned());
        }
        ItemBody::AgentMessage { text } => {
            blocks(&view.parsed(item, text), &item.id, row, result);
            for (at, change) in view.answer_files(item).iter().enumerate() {
                let key = super::turn_review::file_key(&item.id, at);
                if !search && !view.answer_file_visible(&item.id, at) {
                    continue;
                }
                result.push(SourceLeaf {
                    key: format!("path:{key}"),
                    text: change.path.clone(),
                    row,
                });
                if (search || view.open.contains(&key))
                    && let Some(text) = &change.diff
                {
                    for (at, line) in diff::display(text, change.kind).0.iter().enumerate() {
                        let text = line.text.replace('\t', "    ");
                        result.push(SourceLeaf {
                            key: format!("diff:{key}:{at}"),
                            text: if text.is_empty() { " ".into() } else { text },
                            row,
                        });
                    }
                }
            }
        }
        // As the message shows it: the lines logged for its files are its cards.
        ItemBody::UserMessage { text } => {
            if let Some(text) = super::attachment_ui::shown_text(item, text) {
                blocks(
                    &view.parsed(item, text),
                    &format!("user:{}", item.id),
                    row,
                    result,
                );
            }
        }
        ItemBody::Reasoning { text } if expanded => {
            put(format!("thought:{}", item.id), text.trim().into())
        }
        // The container header owns the command; the live log is one retained page.
        ItemBody::Command {
            command, output, ..
        } if expanded => {
            put(
                format!("command:{}", item.id),
                view.command_container_label(command),
            );
            put(
                format!("output:{}", item.id),
                view.command_output_text(item, output),
            );
        }
        ItemBody::ToolCall { input, output, .. } if expanded => {
            if rich_tool(view, item, row, search, result) {
                tool_leaves(view, projected, item, input, output.as_deref(), &mut put);
            }
        }
        ItemBody::WebSearch { query } if expanded => {
            if let Some(output) = view.tool_output(item) {
                let raw = match output.as_ref() {
                    crate::chat::tool_output::ToolOutput::WebSearch { raw, .. } => {
                        Some(raw.as_str())
                    }
                    _ => None,
                };
                if rich_tool(view, item, row, search, result) {
                    tool_leaves(
                        view,
                        projected,
                        item,
                        &serde_json::json!({"query":query}),
                        raw,
                        &mut put,
                    );
                }
            }
        }
        ItemBody::FileChange { changes } if expanded => {
            for (at, change) in changes.iter().enumerate() {
                let key = format!("{}#{at}", item.id);
                if !search && at >= super::turn_review::INLINE_FILES && !view.open.contains(&key) {
                    continue;
                }
                if (search || changes.len() == 1 || view.open.contains(&key))
                    && let Some(text) = change
                        .diff
                        .as_deref()
                        .filter(|text| !text.trim().is_empty())
                {
                    for (line, source) in diff::display(text, change.kind).0.iter().enumerate() {
                        let text = source.text.replace('\t', "    ");
                        put(
                            format!("diff:{key}:{line}"),
                            if text.is_empty() { " ".into() } else { text },
                        );
                    }
                }
            }
        }
        ItemBody::Plan { explanation, steps } if expanded => {
            if steps.is_empty()
                && let Some(source) = explanation
                    .as_deref()
                    .filter(|text| !text.trim().is_empty())
            {
                let expanded = search
                    || view
                        .open
                        .contains(&super::plan_document::expansion_key(&item.id));
                let source = super::plan_document::shown_source(source, expanded);
                blocks(
                    &view.parsed_variant(item, source, if expanded { "full" } else { "preview" }),
                    &format!("plan:{}:document", item.id),
                    row,
                    result,
                );
            } else {
                if let Some(text) = explanation {
                    put(format!("plan:{}:explanation", item.id), text.trim().into());
                }
                for (at, step) in steps.iter().enumerate() {
                    put(format!("plan:{}:{at}", item.id), step.text.clone());
                }
            }
        }
        ItemBody::Todo { items } if expanded => {
            for (at, step) in items.iter().enumerate() {
                put(format!("plan:{}:{at}", item.id), step.text.clone());
            }
        }
        ItemBody::Compaction => put(
            format!("compaction:{}", item.id),
            super::widgets::sentence("context compacted", look),
        ),
        ItemBody::Notice { level, text, .. } => {
            let prefix = if look.native {
                ""
            } else {
                match level {
                    NoticeLevel::Info => "",
                    NoticeLevel::Warning => "! ",
                    NoticeLevel::Error => "✕ ",
                }
            };
            put(format!("notice:{}", item.id), format!("{prefix}{text}"));
        }
        _ => {}
    }
    result.extend(entries);
}

/// A structured result's rows. Whether its raw input/output is drawn too.
fn rich_tool(
    view: &ChatView,
    item: &Item,
    row: usize,
    search: bool,
    result: &mut Vec<SourceLeaf>,
) -> bool {
    let Some(output) = view.tool_output(item) else {
        return true;
    };
    result.extend(
        super::tool_views::texts(&item.id, &output)
            .into_iter()
            .map(|text| SourceLeaf {
                key: text.key,
                text: text.text,
                row,
            }),
    );
    search || view.open.contains(&super::tool_views::raw_key(item))
}

/// A call's raw input and output, as its container draws them.
fn tool_leaves(
    view: &ChatView,
    projected: &display::Row,
    item: &Item,
    input: &serde_json::Value,
    output: Option<&str>,
    put: &mut impl FnMut(String, String),
) {
    let semantic = item.presentation.semantics.as_ref();
    let show_input = semantic.is_none_or(|s| {
        matches!(
            s.kind,
            crate::chat::model::ToolKind::Mcp
                | crate::chat::model::ToolKind::Other
                | crate::chat::model::ToolKind::Subagent
        )
    });
    if show_input {
        let input = cards::input_pretty(input);
        if !input.is_empty() {
            put(
                format!("tool-input:{}", item.id),
                cards::clean_output(cards::tail(&input, 200, 32 * 1024).0),
            );
        }
    }
    let output = output
        .filter(|text| !text.is_empty())
        .or_else(|| semantic.and_then(|s| s.excerpt.as_deref()))
        .map(|text| cards::inline_image_output(text, !item.presentation.images.is_empty()))
        .filter(|text| !text.trim().is_empty());
    if super::rows::is_file_excerpt(item)
        && (output.is_some() || item.presentation.images.is_empty())
    {
        let target = semantic
            .and_then(|s| s.target.as_deref())
            .unwrap_or_default();
        put(
            format!("file-header:{}", item.id),
            view.file_container_label(target),
        );
        if !projected.running {
            let preview = cards::file_output_preview(
                output.as_deref().unwrap_or_default(),
                view.open.contains(&format!("output-all:{}", item.id)),
            );
            for (index, line) in preview.selectable_lines() {
                put(format!("tool-output:{}:{index}", item.id), line.to_owned());
            }
        }
    } else if let Some(output) = output {
        let preview = cards::output_preview(
            &output,
            projected.running,
            view.open.contains(&format!("output-all:{}", item.id)),
        );
        for run in cards::output_runs(&preview) {
            if let Some(key) = run.selection_key(&format!("tool-output:{}", item.id)) {
                put(key, run.text);
            }
        }
    }
}

fn artifacts<'a>(
    view: &ChatView,
    artifacts: impl IntoIterator<Item = &'a riwork_engine::chat::projection::ArtifactRef>,
    row: usize,
    result: &mut Vec<SourceLeaf>,
) {
    for artifact in artifacts {
        let Some((_, item)) = view.projected_item(&artifact.item_id) else {
            continue;
        };
        if let Some(path) = &artifact.path {
            result.push(SourceLeaf {
                key: format!("artifact:{}:path:{}", item.id, path.path),
                text: std::path::Path::new(&path.path)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.path.clone()),
                row,
            });
        } else if let Some(index) = artifact.image_index
            && let Some(image) = item.presentation.images.get(index)
        {
            result.push(SourceLeaf {
                key: format!("artifact:{}:image:{index}", item.id),
                text: super::media::image_label(image, Some(item)),
                row,
            });
        }
    }
}

fn inline(spans: &[Span], key: &str, row: usize, out: &mut Vec<SourceLeaf>) {
    if spans.iter().any(|span| span.image) {
        for (at, span) in spans.iter().enumerate().filter(|(_, span)| !span.image) {
            inline(std::slice::from_ref(span), &format!("{key}/{at}"), row, out);
        }
    } else {
        out.push(SourceLeaf {
            key: key.into(),
            text: markdown::plain_text(spans),
            row,
        });
    }
}

fn blocks(source: &[Block], key: &str, row: usize, out: &mut Vec<SourceLeaf>) {
    for (at, block) in source.iter().enumerate() {
        let key = format!("{key}/{at}");
        match block {
            Block::Paragraph(spans) | Block::Heading { spans, .. } | Block::Task { spans, .. } => {
                inline(spans, &key, row, out)
            }
            Block::Code { text, .. } => out.push(SourceLeaf {
                key: format!("code:{key}"),
                text: text.replace('\t', "    "),
                row,
            }),
            Block::Quote(inner) => blocks(inner, &key, row, out),
            Block::Callout {
                kind,
                blocks: inner,
            } => {
                out.push(SourceLeaf {
                    key: format!("callout:{key}"),
                    text: kind.label().to_owned(),
                    row,
                });
                blocks(inner, &key, row, out);
            }
            Block::List { items, .. } => {
                for (at, item) in items.iter().enumerate() {
                    blocks(item, &format!("{key}/{at}"), row, out);
                }
            }
            Block::Table { header, rows, .. } => {
                for (at, cells) in std::iter::once(header).chain(rows.iter()).enumerate() {
                    for (cell, spans) in cells.iter().enumerate() {
                        inline(spans, &format!("{key}/{at}/{cell}"), row, out);
                    }
                }
            }
            Block::Rule => {}
            // A chip's text is its file's name.
            Block::Files(files) => {
                for (at, file) in files.iter().enumerate() {
                    out.push(SourceLeaf {
                        key: super::media::cited_file_key(&key, at),
                        text: file.name().to_owned(),
                        row,
                    });
                }
            }
        }
    }
}
