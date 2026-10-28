//! Provider-reported file changes retain their own patches and source coordinates.
use std::collections::{BTreeMap, HashMap};
use nsh_core::prelude::*;
use serde::{Serialize, Deserialize};
use riwork_engine::chat::model::{FileChange, Item, ItemBody, ItemStatus, Transcript};
use super::{diff, dto::wire_name};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct ChatChangedFile {
    pub id: String, pub item_id: String, pub path: String, pub kind: String,
    pub has_diff: bool, pub added: u32, pub removed: u32,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct ChatDiffLine {
    pub kind: String, pub text: String, pub old_line: Option<u32>, pub new_line: Option<u32>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct ChatFileReview {
    pub file: ChatChangedFile, pub lines: Vec<ChatDiffLine>, pub patch: String,
}
fn file(item: &str, at: usize, change: &FileChange) -> ChatChangedFile {
    let (added, removed) = change.diff.as_ref().map(|text| diff::stats(&diff::classify(text, change.kind))).unwrap_or_default();
    ChatChangedFile { id: format!("{item}#{at}"), item_id: item.into(), path: change.path.clone(), kind: wire_name(&change.kind),
        has_diff: change.diff.as_ref().is_some_and(|text| !text.is_empty()), added: added as u32, removed: removed as u32 }
}
pub fn files(transcript: &Transcript, scope: Option<&str>) -> Vec<ChatChangedFile> {
    let selected = scope.and_then(|id| transcript.items.iter().find(|item| item.id == id));
    let turn = selected.filter(|item| matches!(item.body, ItemBody::AgentMessage { .. })).and_then(|item| item.turn_id.as_deref());
    let mut rows = BTreeMap::new();
    for item in &transcript.items {
        if let Some(id) = scope {
            if let Some(turn) = turn {
                if item.turn_id.as_deref() != Some(turn) || item.status != ItemStatus::Completed { continue; }
            } else if item.id != id { continue; }
        }
        let ItemBody::FileChange { changes } = &item.body else { continue; };
        for (at, change) in changes.iter().enumerate() {
            let row = file(&item.id, at, change);
            // An answer shows the latest reported patch per path, never sums repeated edits.
            rows.insert(if turn.is_some() { change.path.clone() } else { format!("{}:{}", change.path, row.id) }, row);
        }
    }
    rows.into_values().collect()
}
pub fn read(transcript: &Transcript, key: &str) -> Result<ChatFileReview, String> {
    let (id, index) = key.rsplit_once('#').ok_or("invalid: file review identity")?;
    let index = index.parse::<usize>().map_err(|_| "invalid: file review index")?;
    let item = transcript.items.iter().find(|item| item.id == id).ok_or("The reported change is no longer available")?;
    let ItemBody::FileChange { changes } = &item.body else { return Err("This item has no file changes".into()); };
    let change = changes.get(index).ok_or("The reported change is no longer available")?;
    let patch = change.diff.clone().unwrap_or_default();
    // The native List virtualizes rendering; every recorded line stays available.
    let shown = diff::classify(&patch, change.kind);
    let numbers = diff::line_numbers(&shown, change.kind);
    let lines = shown.iter().zip(numbers).map(|(line, (old, new))| ChatDiffLine {
        kind: match line.kind { diff::LineKind::Add => "add", diff::LineKind::Remove => "remove", diff::LineKind::Hunk => "hunk", diff::LineKind::Header => "header", diff::LineKind::Note => "note", _ => "context" }.into(),
        text: line.text.into(), old_line: old.map(|n| n as u32), new_line: new.map(|n| n as u32),
    }).collect();
    Ok(ChatFileReview { file: file(id, index, change), lines, patch })
}

/// Counts are maintained at whole-item replacement boundaries, never per row.
/// Path reference counts retain a path while any completed edit in its turn
/// still reports it, including when another edit is replaced or removed.
#[derive(Default)]
pub struct FileCounts {
    items: HashMap<String, FileContribution>,
    turns: HashMap<String, HashMap<String, usize>>,
    total: usize,
}

struct FileContribution {
    total: usize,
    turn: Option<String>,
    paths: Vec<String>,
}

impl FileCounts {
    pub fn replace(&mut self, id: &str, item: Option<&Item>) {
        if let Some(previous) = self.items.remove(id) {
            self.total -= previous.total;
            if let Some(turn) = previous.turn {
                let empty = if let Some(paths) = self.turns.get_mut(&turn) {
                    for path in previous.paths {
                        if let Some(count) = paths.get_mut(&path) {
                            *count -= 1;
                            if *count == 0 { paths.remove(&path); }
                        }
                    }
                    paths.is_empty()
                } else { false };
                if empty { self.turns.remove(&turn); }
            }
        }
        let Some(item) = item else { return; };
        let ItemBody::FileChange { changes } = &item.body else { return; };
        if changes.is_empty() { return; }
        let turn = (item.status == ItemStatus::Completed).then(|| item.turn_id.clone()).flatten();
        let paths = if let Some(turn) = &turn {
            let counts = self.turns.entry(turn.clone()).or_default();
            changes.iter().map(|change| {
                *counts.entry(change.path.clone()).or_default() += 1;
                change.path.clone()
            }).collect()
        } else { Vec::new() };
        self.total += changes.len();
        self.items.insert(id.into(), FileContribution { total: changes.len(), turn, paths });
    }

    pub fn total(&self) -> usize { self.total }

    pub fn count_item(&self, item: &Item) -> usize {
        match &item.body {
            ItemBody::FileChange { changes } => changes.len(),
            ItemBody::AgentMessage { .. } => item.turn_id.as_ref()
                .and_then(|turn| self.turns.get(turn)).map_or(0, HashMap::len),
            _ => 0,
        }
    }
}
