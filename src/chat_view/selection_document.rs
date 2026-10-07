//! The current visible transcript's source export, independent of virtualization.
//! This contains no pointer/gesture state or character hit-testing algorithm.
use super::{
    ChatView, cards, diff,
    display::Row,
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
    for (row, visible) in view.visible.iter().enumerate() {
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
        let expanded = view.open.contains(&item.id)
            && cards::head(item, None).is_some_and(|head| head.expandable);
        let mut entries = Vec::new();
        let mut put = |key: String, text: String| entries.push(SourceLeaf { key, text, row });
        match &item.body {
            ItemBody::AgentMessage { text } => {
                blocks(&view.parsed(item, text), &item.id, row, &mut result)
            }
            ItemBody::UserMessage { text } => blocks(
                &view.parsed(item, text),
                &format!("user:{}", item.id),
                row,
                &mut result,
            ),
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
                    cards::clean_output(cards::tail(output, 200, 32 * 1024).0),
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
            ItemBody::Plan { explanation, steps } => {
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
