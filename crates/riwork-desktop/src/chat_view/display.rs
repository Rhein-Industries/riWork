//! Desktop adapter for the shared transcript grouping v2 projection.
//!
//! The engine owns grouping, counts, disclosure defaults and anchor remapping.
//! This module only translates persisted display settings and list indices.
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

pub(super) fn contains(row: &Row, id: &str) -> bool {
    row.id == id
        || row.children.iter().any(|child| contains(child, id))
        || row.comments.iter().any(|comment| contains(comment, id))
        || row.artifacts.iter().any(|artifact| artifact.item_id == id)
}

/// Resolve an item anchor with the shared rules, then locate its virtual list row.
pub(super) fn remap_anchor(anchor: &Row, rows: &[Row]) -> Option<usize> {
    let id = projection::visible_anchor(rows, anchor_item(anchor))
        .or_else(|| projection::visible_anchor(rows, &anchor.id));
    id.and_then(|id| rows.iter().position(|row| contains(row, &id)))
        .or_else(|| {
            rows.iter()
                .position(|row| contains(row, anchor_item(anchor)))
        })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::model::{
        ChatEvent, Item, ItemBody, ItemStatus, MessagePhase, Presentation, TurnOutcome,
    };
    use crate::chat::wire::Envelope;
    use crate::chat_view::state::ChatModel;

    fn item(id: &str, body: ItemBody) -> Item {
        Item {
            id: id.into(),
            turn_id: Some("turn".into()),
            status: ItemStatus::Completed,
            presentation: Presentation::default(),
            body,
            started_at: None,
            ended_at: None,
        }
    }

    pub(super) fn transcript() -> Transcript {
        let mut transcript = Transcript::default();
        transcript.items = vec![
            item(
                "user",
                ItemBody::UserMessage {
                    text: "Find it".into(),
                },
            ),
            item(
                "read",
                ItemBody::Command {
                    command: "cat src/main.rs".into(),
                    cwd: None,
                    output: "fn main() {}".into(),
                    exit_code: Some(0),
                },
            ),
            item(
                "comment",
                ItemBody::AgentMessage {
                    text: "Checking another file".into(),
                },
            ),
            item(
                "search",
                ItemBody::Command {
                    command: "rg main src".into(),
                    cwd: None,
                    output: "src/main.rs:1:fn main() {}".into(),
                    exit_code: Some(0),
                },
            ),
            item(
                "build",
                ItemBody::Command {
                    command: "cargo build".into(),
                    cwd: None,
                    output: "error: failed".into(),
                    exit_code: Some(1),
                },
            ),
            item(
                "answer",
                ItemBody::AgentMessage {
                    text: "Found it".into(),
                },
            ),
        ];
        transcript.items[2].presentation.phase = Some(MessagePhase::Commentary);
        transcript.items[4].status = ItemStatus::Failed;
        transcript.items[5].presentation.phase = Some(MessagePhase::Final);
        transcript
    }

    #[test]
    fn desktop_consumes_engine_rows_and_all_disclosures_start_closed() {
        let transcript = transcript();
        for mode in [DisplayMode::Normal, DisplayMode::Verbose] {
            let desktop = rows(&transcript, mode, &DisclosureState::default(), Some(100));
            let expected = projection::project_transcript(
                &transcript,
                &ProjectionOptions {
                    mode: if mode == DisplayMode::Normal {
                        projection::DisplayMode::Normal
                    } else {
                        projection::DisplayMode::Verbose
                    },
                    now_ms: Some(100),
                    ..ProjectionOptions::default()
                },
            );
            assert_eq!(desktop, expected);
            let mut opened = Vec::new();
            open_ids(&desktop, &mut opened);
            assert!(opened.is_empty());
        }
    }

    #[test]
    fn normal_keeps_one_activity_and_hides_commentary_even_when_open() {
        let transcript = transcript();
        let mut state = DisclosureState::default();
        state.toggle("activity:turn", false);
        let rows = rows(&transcript, DisplayMode::Normal, &state, None);
        assert_eq!(
            rows.iter().map(|row| row.kind).collect::<Vec<_>>(),
            [RowKind::User, RowKind::Activity, RowKind::Final]
        );
        let activity = &rows[1];
        assert!(activity.open);
        assert_eq!(activity.failed_count, 1);
        assert!(!activity.children.is_empty());
        assert!(find(&activity.children, "comment").is_none());
        assert!(activity.children.iter().all(|row| !row.open));
    }

    #[test]
    fn anchors_follow_collapsed_groups_and_activity() {
        let transcript = transcript();
        let normal = rows(
            &transcript,
            DisplayMode::Normal,
            &DisclosureState::default(),
            None,
        );
        let verbose = rows(
            &transcript,
            DisplayMode::Verbose,
            &DisclosureState::default(),
            None,
        );
        let build = find(&verbose, "build").unwrap();
        assert_eq!(remap_anchor(build, &normal), Some(1));
        assert_eq!(focusable_rows(&normal), ["activity:turn"]);
        assert!(focusable_rows(&verbose).contains(&"build".into()));
    }

    fn apply(model: &mut ChatModel, event: ChatEvent) {
        let changed = model.apply(&[Envelope {
            chat_id: "chat".into(),
            seq: model.last_seq + 1,
            event,
        }]);
        assert!(changed.projection_changed);
    }

    #[test]
    fn activity_uses_folded_turn_wall_time_instead_of_overlapping_call_durations() {
        let mut model = ChatModel::new();
        apply(
            &mut model,
            ChatEvent::TurnStarted {
                turn_id: "turn".into(),
                started_at: Some(1_000),
            },
        );
        for (id, command, end) in [
            ("build", "cargo build", 15_000),
            ("test", "cargo test", 25_000),
        ] {
            let mut call = item(
                id,
                ItemBody::Command {
                    command: command.into(),
                    cwd: None,
                    output: String::new(),
                    exit_code: Some(0),
                },
            );
            call.started_at = Some(5_000);
            call.ended_at = Some(end);
            apply(&mut model, ChatEvent::ItemCompleted { item: call });
        }
        apply(
            &mut model,
            ChatEvent::TurnCompleted {
                turn_id: "turn".into(),
                outcome: TurnOutcome::Completed,
                ended_at: Some(73_000),
            },
        );
        let projected = rows(
            &model.transcript,
            DisplayMode::Normal,
            &DisclosureState::default(),
            Some(90_000),
        );
        let activity = find(&projected, "activity:turn").unwrap();
        assert_eq!(activity.duration_ms, Some(72_000));
        assert_eq!(activity.title, "Worked 1m 12s · 2 commands");
        assert!(!activity.running);
    }

    #[test]
    fn running_activity_includes_time_between_calls_and_keeps_spinner_out_of_text() {
        let mut model = ChatModel::new();
        apply(
            &mut model,
            ChatEvent::TurnStarted {
                turn_id: "turn".into(),
                started_at: Some(1_000),
            },
        );
        let mut call = item(
            "test",
            ItemBody::Command {
                command: "cargo test".into(),
                cwd: None,
                output: String::new(),
                exit_code: None,
            },
        );
        call.status = ItemStatus::InProgress;
        call.started_at = Some(17_000);
        apply(&mut model, ChatEvent::ItemStarted { item: call });
        let projected = rows(
            &model.transcript,
            DisplayMode::Normal,
            &DisclosureState::default(),
            Some(19_000),
        );
        let activity = find(&projected, "activity:turn").unwrap();
        assert_eq!(activity.duration_ms, Some(18_000));
        assert_eq!(activity.title, "Working 18s · running cargo test");
        assert!(activity.running);
        assert_eq!(header_text(activity, None), activity.title);
        assert!(!projection::selection_leaves(&projected)[0].1.contains('◌'));
    }
}
