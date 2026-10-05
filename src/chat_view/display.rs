//! A projection of the complete transcript. Item completion alone never means final.
use crate::chat::model::{
    ItemBody, ItemStatus, MessagePhase, NoticeLevel, Transcript, TurnOutcome,
};
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
    Outcome(String, TurnOutcome),
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
    let mut last = HashMap::new();
    // A legacy message is a candidate only while no later tool/reasoning/commentary follows it.
    for (at, item) in transcript.items.iter().enumerate() {
        if let Some(turn) = &item.turn_id {
            if item.presentation.phase == Some(MessagePhase::Final) {
                explicit.insert(turn.as_str());
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
    let done: HashSet<&str> = completed
        .iter()
        .filter(|(_, outcome, _)| *outcome == TurnOutcome::Completed)
        .map(|(id, _, _)| id.as_str())
        .collect();
    let finals: HashSet<usize> = last
        .into_iter()
        .filter(|(id, _)| done.contains(id) && !explicit.contains(id))
        .map(|(_, at)| at)
        .collect();
    let final_turns: HashSet<&str> = finals
        .iter()
        .filter_map(|ix| transcript.items[*ix].turn_id.as_deref())
        .collect();
    let mut out = Vec::new();
    let mut outcomes = completed.iter().peekable();
    for at in 0..=transcript.items.len() {
        while let Some((turn, outcome, end)) = outcomes.peek() {
            if *end > at {
                break;
            }
            let has_final = explicit.contains(turn.as_str()) || final_turns.contains(turn.as_str());
            if !has_final || *outcome != TurnOutcome::Completed {
                out.push(Row::Outcome(turn.clone(), outcome.clone()));
            }
            outcomes.next();
        }
        let Some(item) = transcript.items.get(at) else {
            break;
        };
        let visible = waiting_message == Some(at)
            || matches!(item.body, ItemBody::UserMessage { .. })
            || matches!(
                item.body,
                ItemBody::Notice {
                    level: NoticeLevel::Warning | NoticeLevel::Error,
                    ..
                }
            )
            || matches!(
                item.status,
                ItemStatus::Failed | ItemStatus::Declined | ItemStatus::Interrupted
            )
            || !item.presentation.images.is_empty()
            || item.presentation.phase == Some(MessagePhase::Final)
            || finals.contains(&at);
        if visible {
            out.push(Row::Item(at));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::model::{ChatEvent, ChatState, Item, Presentation};
    fn item(id: &str, phase: Option<MessagePhase>, body: ItemBody) -> Item {
        Item {
            id: id.into(),
            turn_id: Some("turn".into()),
            status: ItemStatus::Completed,
            presentation: Presentation {
                phase,
                images: vec![],
            },
            body,
        }
    }
    fn text(id: &str, phase: Option<MessagePhase>) -> Item {
        item(id, phase, ItemBody::AgentMessage { text: id.into() })
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
        let before = t.clone();
        rows(&t, &[], DisplayMode::Normal);
        rows(&t, &[], DisplayMode::Verbose);
        assert_eq!(before, t);
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
