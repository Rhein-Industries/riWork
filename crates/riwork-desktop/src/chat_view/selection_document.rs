//! The current visible transcript's source export, independent of virtualization.
//! This contains no pointer/gesture state or character hit-testing algorithm.
use super::{
    ChatView, cards, diff,
    display::Row,
    markdown::{self, Block, Span},
    widgets::Look,
};
use crate::chat::model::{ItemBody, NoticeLevel};
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
    let mut source = leaves_for(view, look, &view.visible, &view.open, false);
    source.retain(|leaf| !view.diagram_hides_source(&leaf.key, &leaf.text, look));
    source
}

/// Search has its own source: closing a disclosure must not remove its matches,
/// and hidden text must never become part of ordinary selection or Copy.
#[derive(Default)]
pub(super) struct SearchSource {
    pub leaves: Vec<SourceLeaf>,
    pub disclosures: HashMap<String, Vec<String>>,
}

pub(super) fn search_source(view: &ChatView, look: Look) -> SearchSource {
    if let Some(leaves) = view.review_source() {
        return SearchSource {
            leaves,
            ..Default::default()
        };
    }
    let mut open = view.open.clone();
    let mut activity_for_item = HashMap::new();
    for row in super::display::rows(
        &view.model.transcript,
        &view.model.completed,
        view.display_mode,
    ) {
        if let Row::Activity { turn_id, items } = row {
            let key = super::display::activity_key(&turn_id);
            open.insert(key.clone());
            for at in items {
                activity_for_item.insert(at, key.clone());
            }
        }
    }
    let rows = super::display::rows_with_activity(
        &view.model.transcript,
        &view.model.completed,
        view.display_mode,
        &open,
    );
    let mut item_rows = HashMap::new();
    let mut group_rows = HashMap::new();
    let mut outcome_rows = HashMap::new();
    for (row, entry) in view.visible.iter().enumerate() {
        match entry {
            Row::Item(at) | Row::Details(at) => {
                item_rows.insert(*at, row);
            }
            Row::Activity { turn_id, .. } => {
                group_rows.insert(super::display::activity_key(turn_id), row);
            }
            Row::Outcome(turn_id, _) => {
                outcome_rows.insert(turn_id.as_str(), row);
            }
            _ => {}
        }
    }
    let mut result = SearchSource {
        leaves: leaves_for(view, look, &rows, &open, true),
        ..Default::default()
    };
    for leaf in &mut result.leaves {
        let mut required = Vec::new();
        if let Some(key) = leaf.key.strip_prefix("code:") {
            required.push(super::diagram::source_key(key));
        }
        match &rows[leaf.row] {
            Row::Item(at) | Row::Details(at) => {
                let item = &view.model.transcript.items[*at];
                if let Some(activity) = activity_for_item.get(at) {
                    required.push(activity.clone());
                }
                leaf.row = item_rows
                    .get(at)
                    .copied()
                    .or_else(|| {
                        activity_for_item
                            .get(at)
                            .and_then(|key| group_rows.get(key).copied())
                    })
                    .unwrap_or(leaf.row);
                if cards::head(item, None).is_some_and(|head| head.expandable)
                    || crate::chat::workers::of(item).is_some()
                    || crate::chat::subagents::in_transcript(
                        item,
                        &view.model.transcript,
                        view.info().map(|info| info.provider),
                    )
                    .is_some()
                    || (matches!(item.body, ItemBody::WebSearch { .. })
                        && view.tool_output(item).is_some())
                {
                    required.push(item.id.clone());
                }
                if leaf.key.starts_with("tool-input:") || leaf.key.starts_with("tool-output:") {
                    required.push(super::tool_views::raw_key(item));
                }
                if let Some((file, _)) = leaf
                    .key
                    .strip_prefix("diff:")
                    .and_then(|key| key.rsplit_once(':'))
                {
                    required.push(file.to_owned());
                }
                if matches!(item.body, ItemBody::Plan { .. }) {
                    required.push(super::plan_document::expansion_key(&item.id));
                }
            }
            Row::Outcome(turn, _) => {
                leaf.row = outcome_rows.get(turn.as_str()).copied().unwrap_or(leaf.row);
            }
            _ => {}
        }
        result.disclosures.insert(leaf.key.clone(), required);
    }
    result
}

fn leaves_for(
    view: &ChatView,
    look: Look,
    rows: &[Row],
    open: &HashSet<String>,
    search: bool,
) -> Vec<SourceLeaf> {
    let mut result = Vec::new();
    for (row, visible) in rows.iter().enumerate() {
        if let Row::Outcome(turn, outcome) = visible {
            result.push(SourceLeaf {
                key: format!("outcome:{turn}"),
                text: super::display::outcome_text(outcome),
                row,
            });
            continue;
        }
        let (Row::Item(at) | Row::Details(at)) = visible else {
            continue;
        };
        let item = &view.model.transcript.items[*at];
        // A worker card's live lines are never selectable; an opened result is.
        if let Some(worker) = crate::chat::workers::of(item) {
            let tail = worker
                .end
                .as_ref()
                .map(|end| end.tail.clone())
                .unwrap_or_default();
            if (search || open.contains(&item.id))
                && !tail.trim().is_empty()
                && !view.worker_status(*at, worker).running()
            {
                result.push(SourceLeaf {
                    key: format!("worker:{}", item.id),
                    text: tail,
                    row,
                });
            }
            continue;
        }
        if let Some(run) = crate::chat::subagents::in_transcript(
            item,
            &view.model.transcript,
            view.info().map(|info| info.provider),
        ) {
            let text = run.result_text();
            if (search || open.contains(&item.id)) && !text.is_empty() {
                result.push(SourceLeaf {
                    key: format!("subagent:{}", item.id),
                    text,
                    row,
                });
            }
            continue;
        }
        let expanded = (search || open.contains(&item.id))
            && (cards::head(item, None).is_some_and(|head| head.expandable)
                || (matches!(item.body, ItemBody::WebSearch { .. })
                    && view.tool_output(item).is_some()));
        if expanded && let Some(output) = view.tool_output(item) {
            result.extend(
                super::tool_views::texts(&item.id, &output)
                    .into_iter()
                    .map(|text| SourceLeaf {
                        key: text.key,
                        text: text.text,
                        row,
                    }),
            );
            if !search && !open.contains(&super::tool_views::raw_key(item)) {
                continue;
            }
        }
        let mut entries = Vec::new();
        let mut put = |key: String, text: String| entries.push(SourceLeaf { key, text, row });
        match &item.body {
            ItemBody::AgentMessage { text } => {
                blocks(&view.parsed(item, text), &item.id, row, &mut result);
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
                    if (search || open.contains(&key))
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
                        &mut result,
                    )
                }
            }
            ItemBody::Reasoning { text } if expanded => {
                put(format!("thought:{}", item.id), text.trim().into())
            }
            ItemBody::Command {
                command, output, ..
            } if expanded => {
                if command.trim().contains('\n') {
                    put(format!("command:{}", item.id), command.trim().into());
                }
                put(
                    format!("output:{}", item.id),
                    view.command_output_text(item, output),
                );
            }
            ItemBody::ToolCall { input, output, .. } if expanded => {
                let input = cards::input_pretty(input);
                if !input.is_empty() {
                    put(
                        format!("tool-input:{}", item.id),
                        cards::tail(&input, 200, 32 * 1024).0.into(),
                    );
                }
                if let Some(output) = output.as_deref().filter(|text| !text.is_empty()) {
                    put(
                        format!("tool-output:{}", item.id),
                        cards::clean_output(cards::tail(output, 200, 32 * 1024).0),
                    );
                }
            }
            ItemBody::WebSearch { query } if expanded => {
                if let Some(output) = &item.presentation.tool
                    && let crate::chat::tool_output::ToolOutput::WebSearch { raw, .. } = output
                {
                    let input = cards::input_pretty(&serde_json::json!({"query":query}));
                    put(format!("tool-input:{}", item.id), input);
                    put(
                        format!("tool-output:{}", item.id),
                        cards::clean_output(cards::tail(raw, 200, 32 * 1024).0),
                    );
                }
            }
            ItemBody::FileChange { changes } if expanded => {
                for (at, change) in changes.iter().enumerate() {
                    let key = format!("{}#{at}", item.id);
                    if !search && at >= super::turn_review::INLINE_FILES && !open.contains(&key) {
                        continue;
                    }
                    if (search || changes.len() == 1 || open.contains(&key))
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
            ItemBody::Plan { explanation, steps } => {
                if steps.is_empty()
                    && let Some(source) = explanation
                        .as_deref()
                        .filter(|text| !text.trim().is_empty())
                {
                    let expanded =
                        search || open.contains(&super::plan_document::expansion_key(&item.id));
                    let source = super::plan_document::shown_source(source, expanded);
                    blocks(
                        &view.parsed_variant(
                            item,
                            source,
                            if expanded { "full" } else { "preview" },
                        ),
                        &format!("plan:{}:document", item.id),
                        row,
                        &mut result,
                    );
                    continue;
                }
                if let Some(text) = explanation {
                    put(format!("plan:{}:explanation", item.id), text.trim().into());
                }
                for (at, step) in steps.iter().enumerate() {
                    put(format!("plan:{}:{at}", item.id), step.text.clone());
                }
            }
            ItemBody::Todo { items } => {
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
    result
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
        }
    }
}
