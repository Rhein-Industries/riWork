//! Desktop adapter for the shared transcript grouping v2 projection.
//!
//! The engine owns grouping, counts, disclosure defaults and anchor remapping.
//! This module only translates persisted display settings and list indices.
//! Worker/subagent cards are top-level Normal rows between activity and answer;
//! Verbose retains their transcript position. Keep these projected identities
//! when dispatching native cards and mapping targeted worker updates.
use crate::chat::model::{ItemBody, Transcript, TurnOutcome};
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

/// A plan reads as a document: its row starts open in both modes, and the plan
/// card's own preview and Expand handle its length. A manual toggle, a bulk
/// Expand/Collapse all or a legacy blanket default still decides.
fn options(
    transcript: &Transcript,
    mode: DisplayMode,
    disclosure: &DisclosureState,
    now_ms: Option<u64>,
) -> ProjectionOptions {
    let mut disclosure = disclosure.clone();
    if disclosure.bulk.is_none() {
        for item in &transcript.items {
            if matches!(item.body, ItemBody::Plan { .. }) {
                disclosure.overrides.entry(item.id.clone()).or_insert(true);
            }
        }
    }
    ProjectionOptions {
        mode: match mode {
            DisplayMode::Normal => projection::DisplayMode::Normal,
            DisplayMode::Verbose => projection::DisplayMode::Verbose,
        },
        now_ms,
        disclosure,
        ..ProjectionOptions::default()
    }
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
        &options(transcript, mode, disclosure, now_ms),
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
    projection::project_transcript(transcript, &options(transcript, mode, disclosure, now_ms))
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

/// A finished turn's activity line keeps the projection's "Worked" prefix and
/// runtime, and states its work with the factual per-turn summary: search hits
/// are not files read, and failed or declined edits are not reported as changes.
/// An interrupted or failed turn says so at the end, even without counted work;
/// a completed one needs no outcome. A running turn keeps the projected title.
pub(super) fn activity_title(
    row: &Row,
    transcript: &Transcript,
    completed: &[(String, TurnOutcome, usize)],
) -> String {
    let Some(turn) = row
        .id
        .strip_prefix("activity:")
        .filter(|_| row.kind == RowKind::Activity && !row.running)
    else {
        return row.title.clone();
    };
    let summary = super::activity_summary::for_turn(transcript, completed, turn);
    let mut title = if summary == super::activity_summary::Summary::default() {
        row.title.clone()
    } else {
        let worked = row.title.split(" · ").next().unwrap_or(&row.title);
        format!("{worked} · {}", summary.label())
    };
    match completed
        .iter()
        .find(|(id, _, _)| id == turn)
        .map(|(_, outcome, _)| outcome)
    {
        Some(TurnOutcome::Interrupted) => title.push_str(" · Interrupted"),
        Some(TurnOutcome::Failed { message }) if !message.trim().is_empty() => {
            title.push_str(&format!(" · Failed: {}", message.trim()));
        }
        Some(TurnOutcome::Failed { .. }) => title.push_str(" · Failed"),
        Some(TurnOutcome::Completed) | None => {}
    }
    title
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::model::{ChatImage, ImageSource, Item, ItemStatus, MessagePhase};

    fn item(id: &str, turn: &str, body: ItemBody) -> Item {
        Item {
            started_at: None,
            ended_at: None,
            id: id.into(),
            turn_id: Some(turn.into()),
            status: ItemStatus::Completed,
            presentation: Default::default(),
            body,
        }
    }
    fn answer(id: &str, turn: &str) -> Item {
        let mut answer = item(id, turn, ItemBody::AgentMessage { text: id.into() });
        answer.presentation.phase = Some(MessagePhase::Final);
        answer
    }
    fn failed_command(id: &str) -> Item {
        let mut command = item(
            id,
            "turn",
            ItemBody::Command {
                command: "rg absent-pattern src".into(),
                cwd: None,
                output: "No matches".into(),
                exit_code: Some(1),
            },
        );
        command.status = ItemStatus::Failed;
        command
    }
    fn ids(rows: &[ListRow]) -> Vec<&str> {
        rows.iter().map(|entry| entry.row.id.as_str()).collect()
    }

    #[test]
    fn activity_expands_to_individual_rows_without_duplicate_images_or_other_turns() {
        let mut command = failed_command("done");
        command.status = ItemStatus::Completed;
        command.presentation.images.push(ChatImage {
            label: "Screenshot".into(),
            source: ImageSource::Url {
                url: "https://example.invalid/screenshot.png".into(),
            },
        });
        let mut t = Transcript::default();
        t.items = vec![
            command,
            answer("answer", "turn"),
            item(
                "search",
                "second",
                ItemBody::WebSearch {
                    query: "query".into(),
                },
            ),
            answer("second-answer", "second"),
        ];
        let mut open = DisclosureState::default();
        open.toggle("activity:turn", false);
        let collapsed = list_rows(&rows(&t, DisplayMode::Normal, &Default::default(), None));
        let projected = rows(&t, DisplayMode::Normal, &open, None);
        let expanded = list_rows(&projected);
        assert_eq!(
            ids(&collapsed),
            [
                "activity:turn",
                "answer",
                "activity:second",
                "second-answer"
            ]
        );
        // Each disclosed call is its own virtual-list entry; the other turn stays closed.
        assert_eq!(
            ids(&expanded),
            [
                "activity:turn",
                "done",
                "answer",
                "activity:second",
                "second-answer"
            ]
        );
        assert_eq!(expanded[1].depth, 1);
        // The turn's image is offered once, by its activity line, not again by the call.
        assert_eq!(
            find(&projected, "activity:turn").unwrap().artifacts.len(),
            1
        );
        assert!(find(&projected, "done").unwrap().artifacts.is_empty());
        assert_eq!(
            ids(&list_rows(&rows(&t, DisplayMode::Verbose, &open, None))),
            ["done", "answer", "search", "second-answer"]
        );
    }

    #[test]
    fn recovered_failures_remain_reachable_from_the_turn_summary() {
        let mut refused = failed_command("refused");
        refused.status = ItemStatus::Declined;
        if let ItemBody::Command { exit_code, .. } = &mut refused.body {
            *exit_code = None;
        }
        let mut t = Transcript::default();
        t.items = vec![failed_command("failed"), refused, answer("answer", "turn")];
        let projected = rows(&t, DisplayMode::Normal, &Default::default(), None);
        let activity = find(&projected, "activity:turn").unwrap();
        assert!(!activity.open);
        assert_eq!(activity.failed_count, 1);
        assert!(find(&activity.children, "failed").is_some());
        assert!(find(&activity.children, "refused").is_some());
        let completed = [("turn".to_owned(), TurnOutcome::Completed, 3)];
        assert_eq!(
            activity_title(activity, &t, &completed),
            "Worked · Ran 1 command · 1 failed · 1 declined"
        );
        // Without its completion boundary the turn keeps the projected title.
        assert_eq!(activity_title(activity, &t, &[]), activity.title);
    }

    #[test]
    fn interrupted_and_failed_turns_state_their_outcome_on_the_activity_line() {
        let mut t = Transcript::default();
        t.items = vec![
            item("ask", "failed", ItemBody::UserMessage { text: "go".into() }),
            failed_command("stopped"),
        ];
        t.items[1].turn_id = Some("stopped".into());
        let completed = |failed, stopped| {
            vec![
                ("failed".to_owned(), failed, 1),
                ("stopped".to_owned(), stopped, 2),
            ]
        };
        let projected = rows(&t, DisplayMode::Normal, &Default::default(), None);
        let failed = find(&projected, "activity:failed").unwrap();
        let stopped = find(&projected, "activity:stopped").unwrap();
        // A failed turn without counted work still says so, with its whole message.
        let outcomes = completed(
            TurnOutcome::Failed {
                message: "Rate limited\nRetry later".into(),
            },
            TurnOutcome::Interrupted,
        );
        assert_eq!(
            activity_title(failed, &t, &outcomes),
            format!("{} · Failed: Rate limited\nRetry later", failed.title)
        );
        assert_eq!(
            activity_title(stopped, &t, &outcomes),
            "Worked · Ran 1 command · 1 failed · Interrupted"
        );
        let outcomes = completed(
            TurnOutcome::Failed {
                message: " ".into(),
            },
            TurnOutcome::Completed,
        );
        assert_eq!(
            activity_title(failed, &t, &outcomes),
            format!("{} · Failed", failed.title)
        );
        assert_eq!(
            activity_title(stopped, &t, &outcomes),
            "Worked · Ran 1 command · 1 failed"
        );
    }

    #[test]
    fn plan_rows_start_open_in_both_modes_until_toggled_or_collapsed() {
        let mut t = Transcript::default();
        t.items = vec![
            failed_command("done"),
            item(
                "plan",
                "turn",
                ItemBody::Plan {
                    explanation: Some("# Plan".into()),
                    steps: vec![],
                },
            ),
            answer("answer", "turn"),
        ];
        for mode in [DisplayMode::Normal, DisplayMode::Verbose] {
            let projected = rows(&t, mode, &Default::default(), None);
            assert!(find(&projected, "plan").unwrap().open, "{mode:?}");
            assert!(!find(&projected, "done").unwrap().open, "{mode:?}");
            let mut closed = DisclosureState::default();
            closed.toggle("plan", true);
            assert!(!find(&rows(&t, mode, &closed, None), "plan").unwrap().open);
        }
        let mut collapsed = DisclosureState::default();
        let verbose = rows(&t, DisplayMode::Verbose, &collapsed, None);
        collapsed.reset_bulk_for_rows(false, projection::DisplayMode::Verbose, &verbose);
        assert!(
            !find(&rows(&t, DisplayMode::Verbose, &collapsed, None), "plan")
                .unwrap()
                .open
        );
    }

    #[gpui::test]
    fn activity_disclosure_keeps_card_selection_and_markdown_plan_copy(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::AppContext;
        use gpui_kit::test::TestWindowExt;
        let (handle, view, recording) = super::super::editor_tests::mount_selection(cx);
        let source = "# Shipping plan\n\n- [x] Review\n\n```rust\nlet ready = true;\n```";
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |view, cx| {
                let mut command = failed_command("done");
                command.status = ItemStatus::Completed;
                view.model.link = super::super::Link::Live;
                view.model.transcript.items = vec![
                    command,
                    item(
                        "plan",
                        "turn",
                        ItemBody::Plan {
                            explanation: Some(source.into()),
                            steps: vec![],
                        },
                    ),
                    answer("answer", "turn"),
                ];
                view.model.completed = vec![("turn".into(), TurnOutcome::Completed, 3)];
                view.refresh_projection(cx);
                cx.notify();
            });
            window.render_frame(cx);
            assert!(window.try_find("card:done").is_none());
            window.click("card:activity:turn", cx);
            window.render_frame(cx);
            assert!(
                view.read(cx)
                    .list_rows
                    .iter()
                    .any(|entry| entry.row.id == "done")
            );
            window.click("card:done", cx);
            window.render_frame(cx);
            // The plan's row starts open; its card handles its own length.
            assert!(find(&view.read(cx).visible, "plan").unwrap().open);
            let leaves = super::super::selection_document::leaves(
                view.read(cx),
                super::super::widgets::Look::of(cx),
            );
            assert!(
                leaves
                    .iter()
                    .any(|leaf| leaf.key == "output:done" && leaf.text == "No matches")
            );
            assert!(
                leaves
                    .iter()
                    .any(|leaf| leaf.key == "plan:plan:document/0" && leaf.text == "Shipping plan")
            );
            window.click("copy-plan:plan", cx);
            assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), source);
            window.click("card:activity:turn", cx);
            window.render_frame(cx);
            let leaves = super::super::selection_document::leaves(
                view.read(cx),
                super::super::widgets::Look::of(cx),
            );
            assert!(
                leaves
                    .iter()
                    .all(|leaf| leaf.key != "output:done" && !leaf.key.contains("plan:plan:"))
            );
            assert_eq!(view.read(cx).list_rows.len(), 2);
        })
        .unwrap();
        assert!(recording.try_recv().is_err());
    }
}
