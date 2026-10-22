//! Virtual-list sections for long answers; projection and provider identities stay intact.
use super::{
    ChatView,
    display::{self, ListRow, MessagePart, RowKind},
};
use crate::chat::model::ItemBody;
use std::collections::{HashMap, HashSet};

const BLOCKS_PER_PART: usize = 4;
const SPLIT_AFTER: usize = 8;

impl ChatView {
    fn split_message(&self, entry: ListRow, previous: Option<&ListRow>) -> Vec<ListRow> {
        if entry.row.kind != RowKind::Final {
            return vec![entry];
        }
        let Some((_, item)) = self.projected_item(&entry.row.id) else {
            return vec![entry];
        };
        let ItemBody::AgentMessage { text } = &item.body else {
            return vec![entry];
        };
        let established = previous.and_then(|entry| entry.message.as_ref());
        // Fewer than nine lines cannot contain more than eight top-level blocks.
        // Keep ordinary short answers on the renderer's existing lazy parse path.
        if established.is_none() && text.lines().take(SPLIT_AFTER + 1).count() <= SPLIT_AFTER {
            return vec![entry];
        }
        // Worker results have their own native card and disclosure contract.
        if crate::chat::workers::of(item).is_some()
            || crate::chat::subagents::in_transcript(
                item,
                &self.model.transcript,
                self.info().map(|info| info.provider),
            )
            .is_some()
        {
            return vec![entry];
        }
        let blocks = self.parsed(item, text);
        if blocks.is_empty() || (established.is_none() && blocks.len() <= SPLIT_AFTER) {
            return vec![entry];
        }
        let count = blocks.len();
        // A scroll offset is local to its section: never merge an established
        // boundary while its block survives. When a previously unsplit answer
        // grows, keep its (at most eight-block) prefix in the original list row.
        let first_capacity = established.map_or_else(
            || {
                if previous.is_some() {
                    SPLIT_AFTER
                } else {
                    BLOCKS_PER_PART
                }
            },
            |part| part.block_capacity,
        );
        std::iter::once(0)
            .chain((first_capacity..count).step_by(BLOCKS_PER_PART))
            .map(|start| {
                let block_capacity = if start == 0 {
                    first_capacity
                } else {
                    BLOCKS_PER_PART
                };
                let end = (start + block_capacity).min(count);
                ListRow {
                    row: entry.row.clone(),
                    depth: entry.depth,
                    message: Some(MessagePart {
                        start,
                        block_capacity,
                        blocks: blocks[start..end].to_vec(),
                        last: end == count,
                    }),
                }
            })
            .collect()
    }

    pub(super) fn message_list_rows(&self, rows: &[display::Row]) -> Vec<ListRow> {
        // One index for the whole projection; do not scan history per message.
        let previous: HashMap<_, _> = self
            .list_rows
            .iter()
            .filter(|entry| entry.row.kind == RowKind::Final && entry.block_start() == 0)
            .map(|entry| (entry.row.id.as_str(), entry))
            .collect();
        display::list_rows(rows)
            .into_iter()
            .flat_map(|entry| {
                let previous = previous.get(entry.row.id.as_str()).copied();
                self.split_message(entry, previous)
            })
            .collect()
    }

    /// Deltas do not regroup historical turns. Only their owners are sectioned
    /// again; equal sections retain their measurements and selection source.
    pub(super) fn refresh_message_parts(&mut self, touched: &HashSet<String>) -> HashSet<usize> {
        if touched.is_empty() {
            return HashSet::new();
        }
        let mut changes = Vec::new();
        let mut at = 0;
        while at < self.list_rows.len() {
            let entry = &self.list_rows[at];
            if !touched.contains(&entry.row.id) || entry.row.kind != RowKind::Final {
                at += 1;
                continue;
            }
            let start = at;
            while at < self.list_rows.len() && self.list_rows[at].row.id == entry.row.id {
                at += 1;
            }
            let mut whole = entry.clone();
            whole.message = None;
            let parts = self.split_message(whole, Some(entry));
            if self.list_rows[start..at] != parts {
                changes.push((start..at, parts));
            }
        }
        if changes.is_empty() {
            return HashSet::new();
        }
        let tail = self.list.is_following_tail();
        let offset = self.list.logical_scroll_top();
        let anchor = self.list_rows.get(offset.item_ix).map(ListRow::source_key);
        let mut changed = HashSet::new();
        for (range, parts) in changes.into_iter().rev() {
            let old = &self.list_rows[range.clone()];
            let prefix = parts.iter().zip(old).take_while(|(a, b)| a == b).count();
            let suffix = parts[prefix..]
                .iter()
                .rev()
                .zip(old[prefix..].iter().rev())
                .take_while(|(a, b)| a == b)
                .count();
            changed.extend(
                parts[prefix..parts.len() - suffix]
                    .iter()
                    .map(ListRow::source_key),
            );
            if parts
                .iter()
                .map(ListRow::source_key)
                .ne(old.iter().map(ListRow::source_key))
            {
                self.transcript_selection.changed();
                self.turn_navigation.invalidate();
            }
            self.list.splice(
                range.start + prefix..range.end - suffix,
                parts.len() - prefix - suffix,
            );
            self.list_rows.splice(range, parts);
        }
        self.items_in_list = self.list_rows.len();
        if !tail
            && let Some((id, block)) = anchor
            && let Some(at) = self.list_rows.iter().rposition(|entry| {
                entry.row.id == id && entry.message.as_ref().map_or(0, |part| part.start) <= block
            })
        {
            self.list.scroll_to(gpui::ListOffset {
                item_ix: at,
                offset_in_item: offset.offset_in_item,
            });
        }
        self.list_rows
            .iter()
            .enumerate()
            .filter(|(_, entry)| changed.contains(&entry.source_key()))
            .map(|(at, _)| at)
            .collect()
    }

    pub(super) fn replace_list_rows(&mut self, entries: Vec<ListRow>) {
        let prefix = entries
            .iter()
            .zip(&self.list_rows)
            .take_while(|(a, b)| a == b)
            .count();
        let suffix = entries[prefix..]
            .iter()
            .rev()
            .zip(self.list_rows[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        self.list.splice(
            prefix..self.list_rows.len() - suffix,
            entries.len() - prefix - suffix,
        );
        self.items_in_list = entries.len();
        self.list_rows = entries;
    }
}
