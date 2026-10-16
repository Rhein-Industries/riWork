//! The current visible transcript's source export, independent of virtualization.
//! This contains no pointer/gesture state or character hit-testing algorithm.
use super::{
    ChatView, cards, diff,
    display::{self, RowKind},
    markdown::{self, Block, Span},
    widgets::Look,
};
use crate::chat::model::{ItemBody, NoticeLevel};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SourceLeaf {
    pub key: String,
    pub text: String,
    pub row: usize,
}

pub(super) fn leaves(view: &ChatView, look: Look) -> Vec<SourceLeaf> {
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
pub(super) fn mounted_leaves(view: &ChatView) -> Vec<SourceLeaf> {
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
        let visible: std::collections::HashSet<_> =
            riwork_engine::chat::projection::selection_leaves(std::slice::from_ref(top))
                .into_iter()
                .map(|(id, _)| id)
                .collect();
        append_row(view, top, row, look, &visible, &mut result);
    }
    result
}

fn append_row(
    view: &ChatView,
    projected: &display::Row,
    row: usize,
    look: Look,
    visible: &std::collections::HashSet<String>,
    result: &mut Vec<SourceLeaf>,
) {
    if !visible.contains(&projected.id) {
        return;
    }
    if let Some((_, item)) = view.projected_item(&projected.id) {
        let expanded = projected.open;
        let mut entries = Vec::new();
        let mut put = |key: String, text: String| entries.push(SourceLeaf { key, text, row });
        match &item.body {
            ItemBody::AgentMessage { text } if projected.kind == RowKind::Commentary => {
                put(item.id.clone(), text.clone());
            }
            ItemBody::AgentMessage { text } => {
                blocks(&view.parsed(item, text), &item.id, row, result)
            }
            ItemBody::UserMessage { text } => blocks(
                &view.parsed(item, text),
                &format!("user:{}", item.id),
                row,
                result,
            ),
            ItemBody::Reasoning { text } if expanded => {
                put(format!("thought:{}", item.id), text.trim().into())
            }
            ItemBody::Command {
                command, output, ..
            } if expanded => {
                put(
                    format!("command:{}", item.id),
                    view.command_container_label(command),
                );
                let preview = cards::output_preview(
                    output,
                    projected.running,
                    view.open.contains(&format!("output-all:{}", item.id)),
                );
                for run in cards::output_runs(&preview) {
                    if let Some(key) = run.selection_key(&format!("output:{}", item.id)) {
                        put(key, run.text);
                    }
                }
            }
            ItemBody::ToolCall { input, output, .. } if expanded => {
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
                    .as_deref()
                    .filter(|text| !text.is_empty())
                    .or_else(|| semantic.and_then(|s| s.excerpt.as_deref()))
                    .map(|text| {
                        cards::inline_image_output(text, !item.presentation.images.is_empty())
                    })
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
            ItemBody::FileChange { changes } if expanded => {
                for (at, change) in changes.iter().enumerate() {
                    let key = format!("{}#{at}", item.id);
                    if (changes.len() == 1 || view.open.contains(&key))
                        && let Some(text) = change
                            .diff
                            .as_deref()
                            .filter(|text| !text.trim().is_empty())
                    {
                        for (line, source) in diff::display(text, change.kind).0.iter().enumerate()
                        {
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
                if let Some(text) = explanation {
                    put(format!("plan:{}:explanation", item.id), text.trim().into());
                }
                for (at, step) in steps.iter().enumerate() {
                    put(format!("plan:{}:{at}", item.id), step.text.clone());
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

    if projected.kind == RowKind::User {
        if let Some((_, item)) = view.projected_item(&projected.id) {
            for (index, attachment) in item.presentation.attachments.iter().enumerate() {
                if matches!(
                    attachment.kind,
                    crate::chat::attachments::AttachmentKind::Text
                ) {
                    result.push(SourceLeaf {
                        key: format!("attachment:{}:{index}", item.id),
                        text: format!(
                            "{} {}",
                            attachment.name,
                            crate::file_preview::format_size(attachment.bytes)
                        ),
                        row,
                    });
                }
            }
        }
    }
    if projected.kind == RowKind::Activity {
        artifacts(view, &projected.artifacts, row, result);
    }
    for child in &projected.children {
        append_row(view, child, row, look, visible, result);
    }
    for comment in &projected.comments {
        append_row(view, comment, row, look, visible, result);
    }
    if !matches!(projected.kind, RowKind::Activity | RowKind::User) {
        artifacts(view, &projected.artifacts, row, result);
    }
}

fn artifacts(
    view: &ChatView,
    artifacts: &[riwork_engine::chat::projection::ArtifactRef],
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
            Block::Paragraph(spans) | Block::Heading { spans, .. } => inline(spans, &key, row, out),
            Block::Code { text, .. } => out.push(SourceLeaf {
                key: format!("code:{key}"),
                text: text.replace('\t', "    "),
                row,
            }),
            Block::Quote(inner) => blocks(inner, &key, row, out),
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
        }
    }
}
