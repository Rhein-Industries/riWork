//! A projection of the complete transcript. Item completion alone never means final.
use crate::chat::model::{ItemBody, ItemStatus, MessagePhase, Transcript, TurnOutcome};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayMode {
    #[default]
    Normal,
    Verbose,
}
impl DisplayMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::Verbose => "Verbose",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Row {
    Item(usize),
    /// An actionable technical item whose images are in the turn disclosure.
    Details(usize),
    Artifacts {
        turn_id: String,
        items: Vec<usize>,
    },
    Outcome(String, TurnOutcome),
}

pub(super) fn outcome_text(outcome: &TurnOutcome) -> String {
    match outcome {
        TurnOutcome::Completed => "Turn completed".into(),
        TurnOutcome::Interrupted => "Turn interrupted".into(),
        TurnOutcome::Failed { message } => format!("Turn failed: {message}"),
    }
}

fn technical(item: &crate::chat::model::Item) -> bool {
    matches!(
        item.body,
        ItemBody::Command { .. }
            | ItemBody::ToolCall { .. }
            | ItemBody::FileChange { .. }
            | ItemBody::WebSearch { .. }
    )
}

fn usable_answer(item: &crate::chat::model::Item) -> bool {
    matches!(&item.body, ItemBody::AgentMessage { text } if !text.trim().is_empty()
    || item.presentation.images.iter().any(|image| match &image.source {
        crate::chat::model::ImageSource::Local { path } => !path.trim().is_empty(),
        crate::chat::model::ImageSource::Url { url } => !url.trim().is_empty(),
        crate::chat::model::ImageSource::Data { base64, .. } => !base64.is_empty(),
        crate::chat::model::ImageSource::Unavailable { .. } => false,
    })) && !matches!(
        item.status,
        ItemStatus::Failed | ItemStatus::Declined | ItemStatus::Interrupted
    )
}

/// Keep a reader on the same item (or its collapsed turn disclosure) across modes.
pub(super) fn remap_anchor(
    anchor: &Row,
    rows: &[Row],
    completed: &[(String, TurnOutcome, usize)],
) -> Option<usize> {
    if let Some(at) = rows.iter().position(|row| row == anchor) {
        return Some(at);
    }
    let index = match anchor {
        Row::Item(at) | Row::Details(at) => *at,
        Row::Artifacts { items, .. } => *items.first()?,
        // Verbose has no synthetic outcome row. Stay at the end of that turn,
        // rather than retaining a row number that now belongs to another turn.
        Row::Outcome(turn, _) => completed
            .iter()
            .find(|(id, _, _)| id == turn)?
            .2
            .saturating_sub(1),
    };
    if let Some(at) = rows.iter().position(|row| match row {
        Row::Item(at) | Row::Details(at) => *at == index,
        Row::Artifacts { items, .. } => items.contains(&index),
        _ => false,
    }) {
        return Some(at);
    }
    // The turn's last hidden detail maps back to its outcome, not the next
    // turn's user message, when switching from Verbose to Normal.
    if let Some((turn, _, _)) = completed.iter().find(|(_, _, end)| *end > index)
        && let Some(at) = rows
            .iter()
            .position(|row| matches!(row, Row::Outcome(id, _) if id == turn))
    {
        return Some(at);
    }
    let positions = rows
        .iter()
        .enumerate()
        .filter_map(|(row, entry)| match entry {
            Row::Item(at) | Row::Details(at) => Some((row, *at)),
            Row::Artifacts { items, .. } => items.first().map(|at| (row, *at)),
            _ => None,
        })
        .collect::<Vec<_>>();
    positions
        .iter()
        .find(|(_, at)| *at >= index)
        .or(positions.last())
        .map(|(row, _)| *row)
}

pub(super) fn rows(
    transcript: &Transcript,
    completed: &[(String, TurnOutcome, usize)],
    mode: DisplayMode,
) -> Vec<Row> {
    if mode == DisplayMode::Verbose {
        return (0..transcript.items.len()).map(Row::Item).collect();
    }
    let waiting_message = (transcript.state == crate::chat::model::ChatState::Waiting)
        .then(|| {
            transcript
                .items
                .iter()
                .rposition(|item| matches!(item.body, ItemBody::AgentMessage { .. }))
        })
        .flatten();
    let mut explicit = HashSet::new();
    let mut usable_explicit = HashSet::new();
    let mut last = HashMap::new();
    // A legacy message is a candidate only while no later tool/reasoning/commentary follows it.
    for (at, item) in transcript.items.iter().enumerate() {
        if let Some(turn) = &item.turn_id {
            if item.presentation.phase == Some(MessagePhase::Final) {
                explicit.insert(turn.as_str());
                if usable_answer(item) {
                    usable_explicit.insert(turn.as_str());
                }
            }
            match &item.body {
                ItemBody::AgentMessage { .. }
                    if item.presentation.phase != Some(MessagePhase::Commentary) =>
                {
                    last.insert(turn.as_str(), at);
                }
                ItemBody::Notice { .. } | ItemBody::UserMessage { .. } => {}
                _ => {
                    last.remove(turn.as_str());
                }
            }
        }
    }
    let finished: HashSet<&str> = completed.iter().map(|(id, _, _)| id.as_str()).collect();
    let done: HashSet<&str> = completed
        .iter()
        .filter(|(_, outcome, _)| *outcome == TurnOutcome::Completed)
        .map(|(id, _, _)| id.as_str())
        .collect();
    let finals: HashSet<usize> = last
        .into_iter()
        .filter(|(id, at)| {
            done.contains(id) && !explicit.contains(id) && usable_answer(&transcript.items[*at])
        })
        .map(|(_, at)| at)
        .collect();
    let final_turns: HashSet<&str> = finals
        .iter()
        .filter_map(|ix| transcript.items[*ix].turn_id.as_deref())
        .chain(usable_explicit)
        .collect();
    let visible = |at: usize, item: &crate::chat::model::Item| {
        // Notices are the banners above the message box (docs/chat-notices.md); Verbose
        // keeps them in place as the whole record.
        if matches!(item.body, ItemBody::Notice { .. }) {
            return false;
        }
        let recovered = technical(item)
            && item
                .turn_id
                .as_deref()
                .is_some_and(|turn| done.contains(turn) && final_turns.contains(turn));
        waiting_message == Some(at)
            || matches!(item.body, ItemBody::UserMessage { .. })
            || (!recovered
                && matches!(
                    item.status,
                    ItemStatus::Failed | ItemStatus::Declined | ItemStatus::Interrupted
                ))
            || item.presentation.phase == Some(MessagePhase::Final)
            || finals.contains(&at)
    };
    let mut groups: HashMap<&str, Vec<usize>> = HashMap::new();
    let mut grouped = HashSet::new();
    for (at, item) in transcript.items.iter().enumerate() {
        if !item.presentation.images.is_empty()
            && !matches!(item.body, ItemBody::UserMessage { .. })
            && item.presentation.phase != Some(MessagePhase::Final)
            && !finals.contains(&at)
            && waiting_message != Some(at)
            && (technical(item) || !visible(at, item))
            && let Some(turn) = item
                .turn_id
                .as_deref()
                .filter(|turn| finished.contains(turn))
        {
            groups.entry(turn).or_default().push(at);
            grouped.insert(at);
        }
    }
    let mut out = Vec::new();
    let mut outcomes = completed.iter().peekable();
    for at in 0..=transcript.items.len() {
        while let Some((turn, outcome, end)) = outcomes.peek() {
            if *end > at {
                break;
            }
            if let Some(items) = groups.remove(turn.as_str()) {
                out.push(Row::Artifacts {
                    turn_id: turn.clone(),
                    items,
                });
            }
            let has_final = final_turns.contains(turn.as_str());
            if !has_final || *outcome != TurnOutcome::Completed {
                out.push(Row::Outcome(turn.clone(), outcome.clone()));
            }
            outcomes.next();
        }
        let Some(item) = transcript.items.get(at) else {
            break;
        };
        if visible(at, item) {
            out.push(if grouped.contains(&at) {
                Row::Details(at)
            } else {
                Row::Item(at)
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::model::{
        ChatEvent, ChatImage, ChatState, ImageSource, Item, NoticeLevel, Presentation,
    };
    use crate::chat::wire::Envelope;
    use crate::chat_view::state::ChatModel;
    fn item(id: &str, phase: Option<MessagePhase>, body: ItemBody) -> Item {
        Item {
            id: id.into(),
            turn_id: Some("turn".into()),
            status: ItemStatus::Completed,
            presentation: Presentation {
                phase,
                images: vec![],
                ..Default::default()
            },
            body,
        }
    }
    fn text(id: &str, phase: Option<MessagePhase>) -> Item {
        item(id, phase, ItemBody::AgentMessage { text: id.into() })
    }
    fn with_image(mut item: Item) -> Item {
        item.presentation.images.push(ChatImage {
            label: "Screenshot".into(),
            source: ImageSource::Url {
                url: "https://example.invalid/screenshot.png".into(),
            },
        });
        item
    }
    fn apply(model: &mut ChatModel, seq: &mut u64, event: ChatEvent) {
        *seq += 1;
        model.apply(&[Envelope {
            chat_id: "chat".into(),
            seq: *seq,
            event,
        }]);
    }

    fn failed_command(id: &str) -> Item {
        let mut command = item(
            id,
            None,
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

    #[test]
    fn successful_verdict_recovers_four_errors_and_groups_nine_images_without_losing_history() {
        let mut model = ChatModel::new();
        let mut seq = 0;
        apply(
            &mut model,
            &mut seq,
            ChatEvent::TurnStarted {
                turn_id: "turn".into(),
            },
        );
        apply(
            &mut model,
            &mut seq,
            ChatEvent::State {
                state: ChatState::Running,
            },
        );
        let user = with_image(item(
            "user",
            None,
            ItemBody::UserMessage {
                text: "Inspect".into(),
            },
        ));
        apply(
            &mut model,
            &mut seq,
            ChatEvent::ItemCompleted { item: user },
        );
        for n in 0..4 {
            apply(
                &mut model,
                &mut seq,
                ChatEvent::ItemCompleted {
                    item: failed_command(&format!("rg-{n}")),
                },
            );
        }
        for n in 0..9 {
            apply(
                &mut model,
                &mut seq,
                ChatEvent::ItemCompleted {
                    item: with_image(item(
                        &format!("tool-{n}"),
                        None,
                        ItemBody::ToolCall {
                            server: Some("cua".into()),
                            tool: "screenshot".into(),
                            input: serde_json::json!({"window": 42}),
                            output: Some("Capture complete".into()),
                        },
                    )),
                },
            );
        }
        let answer = with_image(text("Usable result", Some(MessagePhase::Final)));
        apply(
            &mut model,
            &mut seq,
            ChatEvent::ItemCompleted { item: answer },
        );
        assert_eq!(
            rows(&model.transcript, &model.completed, DisplayMode::Normal),
            vec![
                Row::Item(0),
                Row::Item(1),
                Row::Item(2),
                Row::Item(3),
                Row::Item(4),
                Row::Item(14)
            ]
        );
        apply(
            &mut model,
            &mut seq,
            ChatEvent::TurnCompleted {
                turn_id: "turn".into(),
                outcome: TurnOutcome::Completed,
            },
        );
        let before = model.transcript.clone();
        let normal = vec![
            Row::Item(0),
            Row::Item(14),
            Row::Artifacts {
                turn_id: "turn".into(),
                items: (5..14).collect(),
            },
        ];
        assert_eq!(
            rows(&model.transcript, &model.completed, DisplayMode::Normal),
            normal
        );
        assert_eq!(
            rows(&model.transcript, &model.completed, DisplayMode::Verbose),
            (0..15).map(Row::Item).collect::<Vec<_>>()
        );
        assert_eq!(
            rows(&model.transcript, &model.completed, DisplayMode::Normal),
            normal
        );
        assert_eq!(model.transcript, before);
        assert!(matches!(
            model.transcript.items[1].body,
            ItemBody::Command {
                exit_code: Some(1),
                ..
            }
        ));
    }

    #[test]
    fn only_technical_failures_in_successful_turns_with_usable_answers_are_recovered() {
        for outcome in [
            TurnOutcome::Completed,
            TurnOutcome::Interrupted,
            TurnOutcome::Failed {
                message: "Failed".into(),
            },
        ] {
            for answer in [
                None,
                Some(text(" ", Some(MessagePhase::Final))),
                Some(text("Answer", Some(MessagePhase::Final))),
            ] {
                let recover =
                    outcome == TurnOutcome::Completed && answer.as_ref().is_some_and(usable_answer);
                for status in [
                    ItemStatus::Failed,
                    ItemStatus::Declined,
                    ItemStatus::Interrupted,
                ] {
                    let mut t = Transcript::default();
                    let mut command = with_image(failed_command("rg"));
                    command.status = status;
                    t.items.push(command);
                    if let Some(answer) = answer.clone() {
                        t.items.push(answer);
                    }
                    let completed = vec![("turn".into(), outcome.clone(), t.items.len())];
                    let projected = rows(&t, &completed, DisplayMode::Normal);
                    assert_eq!(projected.contains(&Row::Details(0)), !recover);
                    assert!(projected.contains(&Row::Artifacts {
                        turn_id: "turn".into(),
                        items: vec![0]
                    }));
                    assert_eq!(
                        rows(&t, &completed, DisplayMode::Verbose),
                        (0..t.items.len()).map(Row::Item).collect::<Vec<_>>()
                    );
                }
            }
        }
        let mut t = Transcript::default();
        t.items = vec![
            failed_command("rg"),
            text("Answer", Some(MessagePhase::Final)),
            item(
                "warning",
                None,
                ItemBody::notice(NoticeLevel::Warning, "Review permissions", None),
            ),
            item(
                "error",
                None,
                ItemBody::notice(NoticeLevel::Error, "Action required", None),
            ),
        ];
        t.items[2].status = ItemStatus::Failed;
        assert_eq!(
            rows(
                &t,
                &[("turn".into(), TurnOutcome::Completed, 4)],
                DisplayMode::Normal
            ),
            // The notices are banners, failed or not.
            vec![Row::Item(1)]
        );
        // A final whose only image is unavailable is not a usable answer.
        t.items[1] = with_image(text("", Some(MessagePhase::Final)));
        t.items[1].presentation.images[0].source = ImageSource::Unavailable {
            reason: "Too large".into(),
        };
        assert!(
            rows(
                &t,
                &[("turn".into(), TurnOutcome::Completed, 4)],
                DisplayMode::Normal
            )
            .contains(&Row::Item(0))
        );
    }

    #[test]
    fn disclosure_anchor_and_grouping_are_per_turn_and_keep_deliverables_separate() {
        let mut t = Transcript::default();
        t.items = vec![
            with_image(failed_command("first-tool")),
            with_image(text("First result", Some(MessagePhase::Final))),
            with_image(failed_command("second-tool")),
            with_image(text("Second result", None)),
        ];
        for item in &mut t.items[2..] {
            item.turn_id = Some("second".into());
        }
        let completed = vec![
            ("turn".into(), TurnOutcome::Completed, 2),
            ("second".into(), TurnOutcome::Completed, 4),
        ];
        let normal = rows(&t, &completed, DisplayMode::Normal);
        assert_eq!(
            normal,
            vec![
                Row::Item(1),
                Row::Artifacts {
                    turn_id: "turn".into(),
                    items: vec![0]
                },
                Row::Item(3),
                Row::Artifacts {
                    turn_id: "second".into(),
                    items: vec![2]
                }
            ]
        );
        let verbose = rows(&t, &completed, DisplayMode::Verbose);
        assert_eq!(remap_anchor(&Row::Item(1), &verbose, &completed), Some(1));
        assert_eq!(remap_anchor(&Row::Item(0), &normal, &completed), Some(1));
        assert_eq!(remap_anchor(&normal[3], &verbose, &completed), Some(2));
        assert_eq!(remap_anchor(&Row::Item(2), &normal, &completed), Some(3));
        assert_eq!(remap_anchor(&Row::Item(3), &normal, &completed), Some(2));
    }

    #[test]
    fn outcome_reader_anchor_stays_at_its_turn_across_modes() {
        for outcome in [
            TurnOutcome::Completed,
            TurnOutcome::Interrupted,
            TurnOutcome::Failed {
                message: "Unresolved error".into(),
            },
        ] {
            let mut t = Transcript::default();
            t.items.push(item(
                "user",
                None,
                ItemBody::UserMessage {
                    text: "Request".into(),
                },
            ));
            // Enough hidden details to make keeping the old visible row number
            // jump near the start of the turn instead of its completion.
            for n in 0..40 {
                t.items
                    .push(text(&format!("detail-{n}"), Some(MessagePhase::Commentary)));
            }
            let boundary = t.items.len();
            let mut next_user = item(
                "next-user",
                None,
                ItemBody::UserMessage {
                    text: "Next request".into(),
                },
            );
            next_user.turn_id = Some("next".into());
            t.items.push(next_user);
            let completed = vec![("turn".into(), outcome.clone(), boundary)];
            let normal = rows(&t, &completed, DisplayMode::Normal);
            let verbose = rows(&t, &completed, DisplayMode::Verbose);
            let anchor = Row::Outcome("turn".into(), outcome);
            let normal_at = normal.iter().position(|row| row == &anchor).unwrap();
            assert_eq!(remap_anchor(&anchor, &normal, &completed), Some(normal_at));
            let verbose_at = remap_anchor(&anchor, &verbose, &completed).unwrap();
            assert_eq!(verbose[verbose_at], Row::Item(boundary - 1));
            assert_eq!(
                remap_anchor(&verbose[verbose_at], &normal, &completed),
                Some(normal_at)
            );
            assert_eq!(t.items.len(), boundary + 1);
        }
    }

    #[test]
    fn tool_images_wait_for_turn_completion_and_verbose_retains_the_whole_history() {
        let mut model = ChatModel::new();
        let mut seq = 0;
        apply(
            &mut model,
            &mut seq,
            ChatEvent::TurnStarted {
                turn_id: "turn".into(),
            },
        );
        apply(
            &mut model,
            &mut seq,
            ChatEvent::State {
                state: ChatState::Running,
            },
        );
        let user = with_image(item(
            "user",
            None,
            ItemBody::UserMessage {
                text: "Inspect this image".into(),
            },
        ));
        apply(
            &mut model,
            &mut seq,
            ChatEvent::ItemCompleted { item: user.clone() },
        );
        let mut tool = with_image(item(
            "tool",
            None,
            ItemBody::ToolCall {
                server: Some("cua".into()),
                tool: "screenshot".into(),
                input: serde_json::json!({"window": 42}),
                output: Some("Routine tool details".into()),
            },
        ));
        tool.status = ItemStatus::InProgress;
        apply(
            &mut model,
            &mut seq,
            ChatEvent::ItemStarted { item: tool.clone() },
        );
        assert_eq!(
            rows(&model.transcript, &model.completed, DisplayMode::Normal),
            vec![Row::Item(0)]
        );
        assert_eq!(
            rows(&model.transcript, &model.completed, DisplayMode::Verbose),
            vec![Row::Item(0), Row::Item(1)]
        );

        // Finishing the tool is not finishing the assistant's turn.
        tool.status = ItemStatus::Completed;
        apply(
            &mut model,
            &mut seq,
            ChatEvent::ItemCompleted { item: tool.clone() },
        );
        assert_eq!(
            rows(&model.transcript, &model.completed, DisplayMode::Normal),
            vec![Row::Item(0)]
        );
        assert_eq!(model.transcript.items[1], tool);
        let mut answer = with_image(text(
            "Final answer with an image",
            Some(MessagePhase::Final),
        ));
        answer.status = ItemStatus::InProgress;
        apply(
            &mut model,
            &mut seq,
            ChatEvent::ItemStarted { item: answer },
        );
        assert_eq!(
            rows(&model.transcript, &model.completed, DisplayMode::Normal),
            vec![Row::Item(0), Row::Item(2)]
        );
        assert_eq!(model.transcript.items[0], user);

        apply(
            &mut model,
            &mut seq,
            ChatEvent::TurnCompleted {
                turn_id: "turn".into(),
                outcome: TurnOutcome::Completed,
            },
        );
        let history = model.transcript.clone();
        // Artifact is a metadata-free rendering row, not the detailed tool item.
        let normal = vec![
            Row::Item(0),
            Row::Item(2),
            Row::Artifacts {
                turn_id: "turn".into(),
                items: vec![1],
            },
        ];
        assert_eq!(
            rows(&model.transcript, &model.completed, DisplayMode::Normal),
            normal
        );
        assert_eq!(
            rows(&model.transcript, &model.completed, DisplayMode::Verbose),
            vec![Row::Item(0), Row::Item(1), Row::Item(2)]
        );
        assert_eq!(model.transcript.items[1], tool);
        assert_eq!(
            rows(&model.transcript, &model.completed, DisplayMode::Normal),
            normal
        );
        assert_eq!(model.transcript, history);
    }

    #[test]
    fn image_failures_remain_visible_while_commentary_images_wait_and_lose_metadata() {
        for status in [
            ItemStatus::Failed,
            ItemStatus::Declined,
            ItemStatus::Interrupted,
        ] {
            let mut transcript = Transcript::default();
            let mut failed = with_image(item(
                "tool",
                None,
                ItemBody::ToolCall {
                    server: None,
                    tool: "screenshot".into(),
                    input: serde_json::Value::Null,
                    output: Some("Actionable failure".into()),
                },
            ));
            failed.status = status;
            transcript.items.push(failed);
            assert_eq!(
                rows(&transcript, &[], DisplayMode::Normal),
                vec![Row::Item(0)]
            );
        }
        let mut transcript = Transcript::default();
        transcript.items.push(with_image(text(
            "commentary",
            Some(MessagePhase::Commentary),
        )));
        assert!(rows(&transcript, &[], DisplayMode::Normal).is_empty());
        let completed = vec![("turn".into(), TurnOutcome::Completed, 1)];
        assert_eq!(
            rows(&transcript, &completed, DisplayMode::Normal),
            vec![
                Row::Artifacts {
                    turn_id: "turn".into(),
                    items: vec![0]
                },
                Row::Outcome("turn".into(), TurnOutcome::Completed)
            ]
        );
        assert_eq!(
            rows(&transcript, &completed, DisplayMode::Verbose),
            vec![Row::Item(0)]
        );
    }
    #[test]
    fn commentary_is_never_a_verdict_even_after_completion() {
        let mut t = Transcript::default();
        t.items = vec![text("comment", Some(MessagePhase::Commentary))];
        assert!(rows(&t, &[], DisplayMode::Normal).is_empty());
        assert_eq!(
            rows(
                &t,
                &[("turn".into(), TurnOutcome::Completed, 1)],
                DisplayMode::Normal
            ),
            vec![Row::Outcome("turn".into(), TurnOutcome::Completed)]
        );
        assert_eq!(rows(&t, &[], DisplayMode::Verbose), vec![Row::Item(0)]);
    }
    #[test]
    fn final_streams_but_legacy_text_waits_for_completed_turn_and_failures_survive() {
        let mut t = Transcript::default();
        t.items = vec![
            text("comment", None),
            text("final", Some(MessagePhase::Final)),
        ];
        assert_eq!(rows(&t, &[], DisplayMode::Normal), vec![Row::Item(1)]);
        t.items[1].presentation.phase = None;
        assert!(rows(&t, &[], DisplayMode::Normal).is_empty());
        let completed = vec![("turn".into(), TurnOutcome::Completed, 2)];
        assert_eq!(
            rows(&t, &completed, DisplayMode::Normal),
            vec![Row::Item(1)]
        );
        t.items.push(item(
            "tool",
            None,
            ItemBody::ToolCall {
                server: None,
                tool: "test".into(),
                input: serde_json::Value::Null,
                output: None,
            },
        ));
        assert_eq!(
            rows(
                &t,
                &[("turn".into(), TurnOutcome::Completed, 3)],
                DisplayMode::Normal
            ),
            vec![Row::Outcome("turn".into(), TurnOutcome::Completed)]
        );
        t.items[2].status = ItemStatus::Failed;
        assert!(rows(&t, &completed, DisplayMode::Normal).contains(&Row::Item(2)));
    }
    #[test]
    fn changing_mode_preserves_replayed_events_requests_and_full_text() {
        let mut t = Transcript::default();
        t.apply(&ChatEvent::ItemStarted {
            item: text("intermediate", Some(MessagePhase::Commentary)),
        });
        t.apply(&ChatEvent::State {
            state: ChatState::Waiting,
        });
        t.apply(&ChatEvent::ApprovalRequested {
            approval: crate::chat::model::Approval {
                request_id: "approve".into(),
                item_id: None,
                kind: crate::chat::model::ApprovalKind::Command,
                title: "Approve operation".into(),
                detail: "Action required".into(),
                choices: vec![],
            },
        });
        t.apply(&ChatEvent::QuestionRequested {
            question: crate::chat::model::Question {
                request_id: "question".into(),
                questions: vec![crate::chat::model::QuestionPrompt {
                    header: None,
                    question: "Choose a target".into(),
                    options: vec![],
                    multi_select: false,
                }],
            },
        });
        let before = t.clone();
        rows(&t, &[], DisplayMode::Normal);
        rows(&t, &[], DisplayMode::Verbose);
        assert_eq!(before, t);
        assert_eq!(rows(&t, &[], DisplayMode::Normal), vec![Row::Item(0)]);
        assert_eq!(t.approvals.len(), 1);
        assert_eq!(t.questions.len(), 1);
    }
    #[test]
    fn large_history_projection_is_linear_and_keeps_one_result_per_turn() {
        let mut t = Transcript::default();
        let mut completed = vec![];
        for n in 0..10_000 {
            let mut entry = text("result", None);
            entry.id = format!("item-{n}");
            entry.turn_id = Some(format!("turn-{n}"));
            t.items.push(entry);
            completed.push((format!("turn-{n}"), TurnOutcome::Completed, n + 1));
        }
        assert_eq!(rows(&t, &completed, DisplayMode::Normal).len(), 10_000);
    }
}
