//! Upstream transcript presentation contracts, expressed through projected rows.

use super::*;
use crate::chat::model::{ChatEvent, NoticeLevel, Presentation, TurnOutcome};

fn item(id: &str, phase: Option<MessagePhase>, body: ItemBody) -> Item {
    Item {
        id: id.into(),
        turn_id: Some("turn".into()),
        status: ItemStatus::Completed,
        started_at: None,
        ended_at: None,
        presentation: Presentation {
            phase,
            ..Presentation::default()
        },
        body,
    }
}

fn text(id: &str, phase: Option<MessagePhase>) -> Item {
    item(id, phase, ItemBody::AgentMessage { text: id.into() })
}

fn rows(transcript: &Transcript, mode: DisplayMode) -> Vec<Row> {
    project_transcript(
        transcript,
        &ProjectionOptions {
            mode,
            ..ProjectionOptions::default()
        },
    )
}

fn finish(transcript: &mut Transcript) {
    transcript.apply(&ChatEvent::TurnCompleted {
        turn_id: "turn".into(),
        outcome: TurnOutcome::Completed,
        ended_at: Some(2_000),
    });
}

#[test]
fn a_notice_after_the_answer_keeps_its_turn_when_it_is_hidden() {
    let mut transcript = Transcript::default();
    transcript.items = vec![
        item("ask", None, ItemBody::UserMessage { text: "ask".into() }),
        text("answer", Some(MessagePhase::Final)),
        item(
            "warning",
            None,
            ItemBody::notice(NoticeLevel::Warning, "close to a limit", None),
        ),
        item(
            "next",
            None,
            ItemBody::UserMessage {
                text: "next".into(),
            },
        ),
    ];
    transcript.items[3].turn_id = Some("turn-2".into());
    finish(&mut transcript);

    let normal = rows(&transcript, DisplayMode::Normal);
    assert_eq!(
        normal.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        ["ask", "activity:turn", "answer", "next", "activity:turn-2"]
    );
    assert_eq!(visible_anchor(&normal, "warning"), None);
    assert_eq!(
        transcript_anchor(&transcript, &normal, "warning").as_deref(),
        Some("answer"),
        "the answer above it, not the next turn's user or activity line"
    );

    let verbose = rows(&transcript, DisplayMode::Verbose);
    assert_eq!(
        transcript_anchor(&transcript, &verbose, "warning").as_deref(),
        Some("warning")
    );
    assert_eq!(verbose[2].kind, RowKind::Notice);
    assert_eq!(transcript.items[2].id, "warning");
}

#[test]
fn commentary_is_never_a_verdict_even_after_completion() {
    let mut transcript = Transcript::default();
    transcript.items = vec![text("comment", Some(MessagePhase::Commentary))];

    // Grouping v2 represents a named turn with an activity line instead of the
    // former synthetic Outcome row. Completion must still never promote its
    // commentary to an answer, or reveal it outside Normal's disclosure.
    let before = rows(&transcript, DisplayMode::Normal);
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].kind, RowKind::Activity);
    assert!(before[0].children.is_empty());
    assert_eq!(visible_anchor(&before, "comment"), None);
    let verbose_before = rows(&transcript, DisplayMode::Verbose);
    assert_eq!(verbose_before.len(), 1);
    assert_eq!(verbose_before[0].id, "comment");
    assert_eq!(verbose_before[0].kind, RowKind::Commentary);

    finish(&mut transcript);
    let after = rows(&transcript, DisplayMode::Normal);
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].kind, RowKind::Activity);
    assert!(!after[0].running);
    assert!(after[0].children.is_empty());
    assert_eq!(visible_anchor(&after, "comment"), None);

    for projected in [before, after] {
        assert!(projected.iter().all(|row| row.kind != RowKind::Final));
    }
    let verbose = rows(&transcript, DisplayMode::Verbose);
    assert_eq!(verbose.len(), 1);
    assert_eq!(verbose[0].id, "comment");
    assert_eq!(verbose[0].kind, RowKind::Commentary);
    assert_eq!(
        transcript.items[0],
        text("comment", Some(MessagePhase::Commentary))
    );
}

#[test]
fn final_streams_but_legacy_text_waits_for_completed_turn_and_failures_survive() {
    let mut transcript = Transcript::default();
    transcript.items = vec![
        text("comment", None),
        text("final", Some(MessagePhase::Final)),
    ];
    transcript.items[1].status = ItemStatus::InProgress;
    let normal = rows(&transcript, DisplayMode::Normal);
    assert_eq!(normal.last().unwrap().id, "final");
    assert_eq!(normal.last().unwrap().kind, RowKind::Final);
    assert_eq!(transcript.items[1].status, ItemStatus::InProgress);

    // Grouping v2 deliberately classifies missing-phase assistant messages as
    // Final (spec/transcript-grouping/v2/README.md, Input and identity). Their
    // history is therefore visible immediately, and a later tool does not
    // retract it. Explicit finals still stream, and failures must survive.
    transcript.items[1].presentation.phase = None;
    let final_ids = |projected: &[Row]| {
        projected
            .iter()
            .filter(|row| row.kind == RowKind::Final)
            .map(|row| row.id.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        final_ids(&rows(&transcript, DisplayMode::Normal)),
        ["comment", "final"]
    );
    finish(&mut transcript);
    assert_eq!(
        final_ids(&rows(&transcript, DisplayMode::Normal)),
        ["comment", "final"]
    );

    transcript.items.push(item(
        "tool",
        None,
        ItemBody::ToolCall {
            server: None,
            tool: "test".into(),
            input: serde_json::Value::Null,
            output: None,
        },
    ));
    let normal = rows(&transcript, DisplayMode::Normal);
    assert_eq!(final_ids(&normal), ["comment", "final"]);
    assert_eq!(normal[0].kind, RowKind::Activity);
    assert_eq!(normal[0].failed_count, 0);
    assert_eq!(normal[0].children[0].id, "tool");

    transcript.items[2].status = ItemStatus::Failed;
    let normal = rows(&transcript, DisplayMode::Normal);
    assert_eq!(final_ids(&normal), ["comment", "final"]);
    assert_eq!(normal[0].failed_count, 1);
    assert_eq!(normal[0].children[0].id, "tool");
    assert_eq!(normal[0].children[0].failed_count, 1);
    assert_eq!(
        visible_anchor(&normal, "tool").as_deref(),
        Some("activity:turn")
    );

    let verbose = rows(&transcript, DisplayMode::Verbose);
    assert_eq!(
        verbose
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        ["comment", "final", "tool"]
    );
    assert_eq!(verbose[2].failed_count, 1);
    assert_eq!(transcript.items[2].status, ItemStatus::Failed);
}
