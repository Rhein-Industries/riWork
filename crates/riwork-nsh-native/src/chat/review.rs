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
    file_with_stats(item, at, change, (added, removed))
}
fn file_with_stats(item: &str, at: usize, change: &FileChange, (added, removed): (usize, usize)) -> ChatChangedFile {
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
            // An answer shows the latest reported patch per path, never sums repeated edits.
            // Select before classifying patches that a later report may replace.
            rows.insert(if turn.is_some() { change.path.clone() } else { format!("{}:{}#{at}", change.path, item.id) }, (&item.id, at, change));
        }
    }
    rows.into_values().map(|(item, at, change)| file(item, at, change)).collect()
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
    Ok(ChatFileReview { file: file_with_stats(id, index, change, diff::stats(&shown)), lines, patch })
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

#[cfg(test)]
mod tests {
    use super::*;
    use riwork_engine::chat::model::ChangeKind;

    fn item(id: &str, turn: Option<&str>, status: ItemStatus, body: ItemBody) -> Item {
        Item { id: id.into(), turn_id: turn.map(str::to_owned), status, body,
            started_at: None, ended_at: None, presentation: Default::default() }
    }
    fn change(path: &str, kind: ChangeKind, patch: Option<&str>) -> FileChange {
        FileChange { path: path.into(), kind, diff: patch.map(str::to_owned) }
    }
    fn transcript(items: Vec<Item>) -> Transcript {
        let mut transcript = Transcript::default();
        transcript.items = items;
        transcript
    }

    #[test]
    fn answer_scope_uses_latest_completed_patch_and_observes_replacements() {
        let mut transcript = transcript(vec![
            item("answer", Some("turn"), ItemStatus::InProgress, ItemBody::AgentMessage { text: "answer".into() }),
            item("first", Some("turn"), ItemStatus::Completed, ItemBody::FileChange { changes: vec![change("same", ChangeKind::Add, Some("one\ntwo"))] }),
            item("last", Some("turn"), ItemStatus::Completed, ItemBody::FileChange { changes: vec![change("same", ChangeKind::Modify, Some("+old")), change("same", ChangeKind::Delete, Some("latest"))] }),
            item("failed", Some("turn"), ItemStatus::Failed, ItemBody::FileChange { changes: vec![change("same", ChangeKind::Add, Some("wrong"))] }),
            item("other", Some("other"), ItemStatus::Completed, ItemBody::FileChange { changes: vec![change("same", ChangeKind::Add, Some("wrong"))] }),
        ]);
        let rows = files(&transcript, Some("answer"));
        assert_eq!(rows.len(), 1);
        assert_eq!((&*rows[0].id, &*rows[0].kind, rows[0].added, rows[0].removed), ("last#1", "delete", 0, 1));
        assert_eq!(files(&transcript, Some("last")).len(), 2);
        assert_eq!(files(&transcript, Some("failed"))[0].id, "failed#0");
        assert!(files(&transcript, Some("missing")).is_empty());

        transcript.items[2].status = ItemStatus::InProgress;
        assert_eq!(files(&transcript, Some("answer"))[0].id, "first#0");
        transcript.items[1].body = ItemBody::FileChange { changes: vec![] };
        assert!(files(&transcript, Some("answer")).is_empty());
        transcript.items[0].turn_id = None;
        assert!(files(&transcript, Some("answer")).is_empty());
    }

    #[test]
    fn formatted_key_collisions_and_lexical_index_order_are_preserved() {
        let transcript = transcript(vec![
            item("c", None, ItemStatus::Completed, ItemBody::FileChange { changes: vec![change("a:b", ChangeKind::Add, Some("first"))] }),
            item("b:c", None, ItemStatus::Completed, ItemBody::FileChange { changes: vec![change("a", ChangeKind::Delete, Some("last"))] }),
            item("id#part", None, ItemStatus::InProgress, ItemBody::FileChange { changes: (0..12).map(|_| change("z:#", ChangeKind::Modify, None)).collect() }),
        ]);
        let rows = files(&transcript, None);
        assert_eq!(rows.len(), 13);
        assert_eq!(rows[0].id, "b:c#0");
        assert_eq!(rows[0].path, "a");
        assert_eq!(rows.iter().skip(1).map(|row| row.id.as_str()).collect::<Vec<_>>(),
            ["id#part#0", "id#part#1", "id#part#10", "id#part#11", "id#part#2", "id#part#3", "id#part#4", "id#part#5", "id#part#6", "id#part#7", "id#part#8", "id#part#9"]);
        assert_eq!(read(&transcript, "c#0").unwrap().patch, "first");
        assert_eq!(read(&transcript, "id#part#00").unwrap().file.id, "id#part#0");
    }

    #[test]
    fn review_stats_match_shown_lines_for_missing_and_malformed_patches() {
        for kind in [ChangeKind::Add, ChangeKind::Modify, ChangeKind::Delete, ChangeKind::Rename] {
            for patch in [None, Some(""), Some("one\r\ntwo\r\n"), Some("@@ bad @@\r\n+add\r\n-remove\r\n\\ No newline at end of file"), Some("@@ -1,3 +1,3 @@\n--- SQL comment\n+++ plus text\n context\n")] {
                let transcript = transcript(vec![item("edit#part", None, ItemStatus::Completed, ItemBody::FileChange { changes: vec![change("file", kind, patch)] })]);
                let review = read(&transcript, "edit#part#0").unwrap();
                assert_eq!(review.file, files(&transcript, Some("edit#part"))[0]);
                assert_eq!(review.patch, patch.unwrap_or_default());
                assert_eq!(review.file.added, review.lines.iter().filter(|line| line.kind == "add").count() as u32);
                assert_eq!(review.file.removed, review.lines.iter().filter(|line| line.kind == "remove").count() as u32);
                assert_eq!(review.file.has_diff, patch.is_some_and(|patch| !patch.is_empty()));
            }
        }
        let transcript = transcript(vec![item("answer", None, ItemStatus::Completed, ItemBody::AgentMessage { text: String::new() })]);
        for (key, message) in [("answer", "invalid: file review identity"), ("answer#bad", "invalid: file review index"), ("missing#0", "The reported change is no longer available"), ("answer#0", "This item has no file changes")] {
            assert_eq!(read(&transcript, key).unwrap_err(), message);
        }
    }
}
