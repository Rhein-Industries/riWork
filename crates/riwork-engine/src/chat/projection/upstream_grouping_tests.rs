//! Upstream transcript contracts expressed through the shared projection.

use super::*;
use crate::chat::model::{
    ChatEvent, ChatImage, ChatState, ImageSource, NoticeLevel, Presentation, TurnOutcome,
};

fn item(id: &str, phase: Option<MessagePhase>, body: ItemBody) -> Item {
    Item {
        id: id.into(),
        turn_id: Some("turn".into()),
        status: ItemStatus::Completed,
        started_at: None,
        ended_at: None,
        presentation: Presentation {
            phase,
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

fn projected(transcript: &Transcript, mode: DisplayMode) -> Vec<Row> {
    project_transcript(
        transcript,
        &ProjectionOptions {
            mode,
            ..Default::default()
        },
    )
}

fn complete(transcript: &mut Transcript, turn: &str, outcome: TurnOutcome) {
    transcript.apply(&ChatEvent::TurnCompleted {
        turn_id: turn.into(),
        outcome,
        ended_at: Some(2_000),
    });
}

fn image_ref(id: &str) -> ArtifactRef {
    ArtifactRef {
        item_id: id.into(),
        image_index: Some(0),
        path: None,
    }
}

#[test]
fn only_technical_failures_in_successful_turns_with_usable_answers_are_recovered() {
    // Grouping v2 deliberately folds every technical detail into the activity,
    // including unrecovered failures. Unlike the old Details row, its badge and
    // disclosed child retain the failure even after a successful usable answer;
    // a change of outcome/answer must never discard that record or its image.
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
            for status in [
                ItemStatus::Failed,
                ItemStatus::Declined,
                ItemStatus::Interrupted,
            ] {
                let mut transcript = Transcript::default();
                let mut command = with_image(failed_command("rg"));
                command.status = status;
                transcript.items.push(command.clone());
                if let Some(answer) = answer.clone() {
                    transcript.items.push(answer);
                }
                complete(&mut transcript, "turn", outcome.clone());
                let history = transcript.clone();
                let normal = projected(&transcript, DisplayMode::Normal);
                let activity = &normal[0];
                assert_eq!(activity.id, "activity:turn");
                assert_eq!(activity.failed_count, 1);
                assert!(!activity.open);
                assert_eq!(activity.artifacts, vec![image_ref("rg")]);
                assert_eq!(activity.children.len(), 1);
                assert_eq!(activity.children[0].id, "rg");
                assert_eq!(activity.children[0].failed_count, 1);
                assert!(activity.children[0].artifacts.is_empty());
                assert_eq!(
                    visible_anchor(&normal, "rg").as_deref(),
                    Some("activity:turn")
                );

                let verbose = projected(&transcript, DisplayMode::Verbose);
                assert_eq!(
                    verbose
                        .iter()
                        .map(|row| row.id.as_str())
                        .collect::<Vec<_>>(),
                    transcript
                        .items
                        .iter()
                        .map(|item| item.id.as_str())
                        .collect::<Vec<_>>()
                );
                assert_eq!(verbose[0].id, "rg");
                assert_eq!(verbose[0].failed_count, 1);
                assert_eq!(verbose[0].artifacts, vec![image_ref("rg")]);
                assert_eq!(transcript.items[0], command);
                assert_eq!(transcript, history);
            }
        }
    }

    let mut transcript = Transcript::default();
    transcript.items = vec![
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
    transcript.items[2].status = ItemStatus::Failed;
    complete(&mut transcript, "turn", TurnOutcome::Completed);
    let normal = projected(&transcript, DisplayMode::Normal);
    assert_eq!(
        normal.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        ["activity:turn", "Answer"]
    );
    assert_eq!(normal[0].children.len(), 1, "notices remain banners");
    assert_eq!(
        normal[0].failed_count, 1,
        "a failed notice is not failed work"
    );

    // An unavailable image alone must never erase an unresolved technical error.
    transcript.items[1] = with_image(text("", Some(MessagePhase::Final)));
    transcript.items[1].presentation.images[0].source = ImageSource::Unavailable {
        reason: "Too large".into(),
    };
    let normal = projected(&transcript, DisplayMode::Normal);
    assert_eq!(normal[0].failed_count, 1);
    assert_eq!(normal[0].children[0].id, "rg");
}

#[test]
fn disclosure_anchor_and_grouping_are_per_turn_and_keep_deliverables_separate() {
    // Grouping v2 puts each turn's tool images on its activity strip, replacing
    // the old trailing Artifacts row. Answers keep their own images, and hidden
    // tool anchors map to that same turn's activity rather than another turn.
    let mut transcript = Transcript::default();
    transcript.items = vec![
        with_image(failed_command("first-tool")),
        with_image(text("First result", Some(MessagePhase::Final))),
        with_image(failed_command("second-tool")),
        with_image(text("Second result", None)),
    ];
    for item in &mut transcript.items[2..] {
        item.turn_id = Some("second".into());
    }
    complete(&mut transcript, "turn", TurnOutcome::Completed);
    complete(&mut transcript, "second", TurnOutcome::Completed);
    let history = transcript.clone();
    let normal = projected(&transcript, DisplayMode::Normal);
    assert_eq!(
        normal.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        [
            "activity:turn",
            "First result",
            "activity:second",
            "Second result"
        ]
    );
    assert_eq!(normal[0].artifacts, vec![image_ref("first-tool")]);
    assert_eq!(normal[1].artifacts, vec![image_ref("First result")]);
    assert_eq!(normal[2].artifacts, vec![image_ref("second-tool")]);
    assert_eq!(normal[3].artifacts, vec![image_ref("Second result")]);
    assert_eq!(normal[0].children[0].id, "first-tool");
    assert_eq!(normal[2].children[0].id, "second-tool");
    assert!(normal[0].children[0].artifacts.is_empty());
    assert!(normal[2].children[0].artifacts.is_empty());

    let verbose = projected(&transcript, DisplayMode::Verbose);
    assert_eq!(
        verbose
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        ["first-tool", "First result", "second-tool", "Second result"]
    );
    for (id, normal_anchor) in [
        ("first-tool", "activity:turn"),
        ("First result", "First result"),
        ("second-tool", "activity:second"),
        ("Second result", "Second result"),
    ] {
        assert_eq!(visible_anchor(&normal, id).as_deref(), Some(normal_anchor));
        assert_eq!(visible_anchor(&verbose, id).as_deref(), Some(id));
    }

    let mut options = ProjectionOptions::default();
    options
        .disclosure
        .overrides
        .insert("activity:turn".into(), true);
    let expanded = project_transcript(&transcript, &options);
    assert_eq!(
        visible_anchor(&expanded, "first-tool").as_deref(),
        Some("first-tool")
    );
    assert_eq!(
        visible_anchor(&expanded, "second-tool").as_deref(),
        Some("activity:second")
    );
    assert_eq!(transcript, history);
}

#[test]
fn tool_images_wait_for_turn_completion_and_verbose_retains_the_whole_history() {
    // Grouping v2 deliberately lifts produced refs to the live activity strip,
    // independently of turn completion, for immediate preview navigation rather
    // than waiting for the old end-of-turn Artifacts row. The tool stays closed
    // and finishing it still does not finish the turn. Verbose preserves each
    // original item/image; user and final deliverables stay separate.
    let mut transcript = Transcript::default();
    transcript.apply(&ChatEvent::TurnStarted {
        turn_id: "turn".into(),
        started_at: Some(1_000),
    });
    transcript.apply(&ChatEvent::State {
        state: ChatState::Running,
    });
    let user = with_image(item(
        "user",
        None,
        ItemBody::UserMessage {
            text: "Inspect this image".into(),
        },
    ));
    transcript.apply(&ChatEvent::ItemCompleted { item: user.clone() });
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
    transcript.apply(&ChatEvent::ItemStarted { item: tool.clone() });
    // Folding enriches legacy tool metadata; projection must preserve that
    // normalized source record along with its output, image and lifecycle.
    tool = transcript.items[1].clone();
    let normal = projected(&transcript, DisplayMode::Normal);
    assert_eq!(
        normal.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        ["user", "activity:turn"]
    );
    assert_eq!(normal[0].artifacts, vec![image_ref("user")]);
    assert_eq!(normal[1].artifacts, vec![image_ref("tool")]);
    assert!(normal[1].running);
    assert!(!normal[1].open);
    assert_eq!(normal[1].children[0].id, "tool");
    let verbose = projected(&transcript, DisplayMode::Verbose);
    assert_eq!(
        verbose
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        ["user", "tool"]
    );
    assert_eq!(verbose[1].artifacts, vec![image_ref("tool")]);

    // Finishing the tool is not finishing the assistant's turn.
    tool.status = ItemStatus::Completed;
    transcript.apply(&ChatEvent::ItemCompleted { item: tool.clone() });
    let normal = projected(&transcript, DisplayMode::Normal);
    assert_eq!(
        normal.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        ["user", "activity:turn"]
    );
    assert!(normal[1].running);
    assert_eq!(normal[1].artifacts, vec![image_ref("tool")]);
    assert_eq!(transcript.items[1], tool);

    let mut answer = with_image(text(
        "Final answer with an image",
        Some(MessagePhase::Final),
    ));
    answer.status = ItemStatus::InProgress;
    transcript.apply(&ChatEvent::ItemStarted { item: answer });
    let normal = projected(&transcript, DisplayMode::Normal);
    assert_eq!(
        normal.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        ["user", "activity:turn", "Final answer with an image"]
    );
    assert_eq!(
        normal[2].artifacts,
        vec![image_ref("Final answer with an image")]
    );
    assert_eq!(normal[1].artifacts, vec![image_ref("tool")]);
    assert_eq!(transcript.items[0], user);

    complete(&mut transcript, "turn", TurnOutcome::Completed);
    let history = transcript.clone();
    let normal = projected(&transcript, DisplayMode::Normal);
    assert!(!normal[1].running);
    assert_eq!(
        normal.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        ["user", "activity:turn", "Final answer with an image"]
    );
    assert_eq!(normal[1].artifacts, vec![image_ref("tool")]);
    assert!(normal[1].children[0].artifacts.is_empty());
    assert_eq!(
        normal[2].artifacts,
        vec![image_ref("Final answer with an image")]
    );
    let verbose = projected(&transcript, DisplayMode::Verbose);
    assert_eq!(
        verbose
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        ["user", "tool", "Final answer with an image"]
    );
    assert_eq!(verbose[0].artifacts, vec![image_ref("user")]);
    assert_eq!(verbose[1].artifacts, vec![image_ref("tool")]);
    assert_eq!(
        verbose[2].artifacts,
        vec![image_ref("Final answer with an image")]
    );
    assert_eq!(transcript.items[1], tool);
    assert_eq!(projected(&transcript, DisplayMode::Normal), normal);
    assert_eq!(transcript, history);
}
