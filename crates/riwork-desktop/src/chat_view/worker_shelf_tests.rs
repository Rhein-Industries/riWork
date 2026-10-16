use super::*;
use crate::chat::{
    model::{ChatEvent, Item, ItemStatus, Provider},
    workers::{self, LiveStatus, LiveWorker, WorkerKind, WorkerRef},
};
use gpui::{ListOffset, TestAppContext};
use gpui_kit::test::TestWindowExt;
use serde_json::json;
use std::collections::BTreeMap;

fn worker(id: &str) -> Item {
    workers::item(
        format!("marker:{id}"),
        Some("current".into()),
        WorkerRef {
            session_id: id.into(),
            kind: WorkerKind::Shell,
            title: format!("Review {id}"),
            harness: Some("codex".into()),
            model: None,
            worktree_id: None,
            assignment: Some("Inspect the changes and report useful findings".into()),
            noted_at: crate::activity::unix_now(),
            after: None,
            late: false,
            follow_up: false,
            end: None,
        },
    )
}

fn live(status: LiveStatus, output: &str) -> LiveWorker {
    LiveWorker {
        status,
        actionable: true,
        model: None,
        output: output.into(),
        output_error: None,
    }
}

fn install(view: &mut ChatView, items: Vec<Item>, cx: &mut Context<ChatView>) {
    let mut info = testing::info("fixture-chat");
    info.provider = Provider::Codex;
    view.adopt_info_for_tests(info);
    view.model.link = Link::Live;
    for item in items {
        view.model
            .transcript
            .apply(&ChatEvent::ItemCompleted { item });
    }
    view.refresh_projection(cx);
    cx.notify();
}

fn delegation(id: &str, operation: &str, states: serde_json::Value, purpose: Option<&str>) -> Item {
    Item {
        id: id.into(),
        turn_id: Some("current".into()),
        status: ItemStatus::Completed,
        body: ItemBody::ToolCall {
            server: None,
            tool: "collabAgentToolCall".into(),
            input: json!({"tool":operation,"prompt":purpose,"agentsStates":states}),
            output: None,
        },
        presentation: Default::default(),
    }
}

#[gpui::test]
fn shelf_joins_provider_reports_by_child_and_replacement_order(cx: &mut TestAppContext) {
    let (_handle, view, _recording) = editor_tests::mount(cx);
    view.update(cx, |view, cx| {
        install(
            view,
            vec![
                delegation(
                    "spawn",
                    "spawnAgent",
                    json!({"child":{"status":"running"}}),
                    Some("Audit polling"),
                ),
                delegation(
                    "wait",
                    "wait",
                    json!({"child":{"status":"completed","message":"Polling has a race"}}),
                    None,
                ),
            ],
            cx,
        );
        let rows = view.shelf_workers();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "subagent:child");
        assert_eq!(rows[0].title, "Audit polling");
        assert_eq!(rows[0].label, "Done");
        assert_eq!(rows[0].output, "Polling has a race");
        let source = view.worker_summary_sources();
        assert_eq!(source.len(), 1);
        assert_eq!(source[0].id, "subagent:child");
        assert_eq!(source[0].assignment.as_deref(), Some("Audit polling"));
        // A previously placed operation is replaced after the later row. Its newer
        // lifecycle wins; an earlier assignment's result cannot explain its new work.
        view.model.transcript.apply(&ChatEvent::ItemCompleted {
            item: delegation(
                "spawn",
                "sendInput",
                json!({"child":{"status":"running"}}),
                Some("Audit polling"),
            ),
        });
        view.refresh_projection(cx);
        let rows = view.shelf_workers();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "Working");
        assert!(rows[0].output.is_empty());
        assert!(view.worker_summary_sources().is_empty());
    });
}

#[gpui::test]
fn shelf_stays_above_composer_and_bounds_overflow_at_narrow_and_wide_widths(
    cx: &mut TestAppContext,
) {
    for width in [320., 900.] {
        let (handle, view, _recording) = editor_tests::mount_sized(
            cx,
            HostConfig {
                ensure: Arc::new(|| Err("fixture staging is disabled".into())),
            },
            width,
        );
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |view, cx| {
                install(view, (0..8).map(|i| worker(&i.to_string())).collect(), cx);
                view.set_subshells(
                    (0..8)
                        .map(|i| {
                            (
                                i.to_string(),
                                live(
                                    if i == 0 {
                                        LiveStatus::Waiting
                                    } else {
                                        LiveStatus::Working
                                    },
                                    "Inspecting the implementation and its callers",
                                ),
                            )
                        })
                        .collect(),
                    None,
                    cx,
                );
                view.workers.summaries.insert(
                    "worker:0".into(),
                    "Found a polling race; preparing the fix".into(),
                );
                cx.notify();
            });
            window.render_frame(cx);
            window.click("chat-composer", cx);
            window.input("Keep this draft", cx);
            window.render_frame(cx);
            let shelf = window.find("chat-workers");
            assert!(shelf.visible());
            assert!(
                !shelf
                    .path()
                    .iter()
                    .any(|id| id.to_string().contains("transcript-row:"))
            );
            assert!(shelf.bounds().size.width <= gpui::px(width));
            assert!(shelf.bounds().size.height <= ui_text::space(130.));
            assert!(shelf.bounds().bottom() <= window.find("chat-composer").bounds().top());
            assert!(
                window
                    .find("shelf-worker:worker:0")
                    .label()
                    .unwrap()
                    .contains("Found a polling race")
            );
            assert!(
                window.try_find("shelf-worker:worker:6").is_none(),
                "two rows stay compact"
            );
            let offset = view.read(cx).list.logical_scroll_top();
            view.update(cx, |view, cx| {
                view.set_subshells(
                    (0..8)
                        .map(|i| {
                            (
                                i.to_string(),
                                live(
                                    if i == 0 {
                                        LiveStatus::Waiting
                                    } else {
                                        LiveStatus::Working
                                    },
                                    &"Long new output 🦀\n".repeat(30),
                                ),
                            )
                        })
                        .collect(),
                    None,
                    cx,
                );
            });
            window.render_frame(cx);
            assert_eq!(
                window.find("chat-workers").bounds().size.height,
                shelf.bounds().size.height
            );
            view.read_with(cx, |view, cx| {
                assert_eq!(view.composer_text(cx), "Keep this draft");
                assert!(view.composer.read(cx).focus_handle(cx).is_focused(window));
                let after = view.list.logical_scroll_top();
                assert_eq!(after.item_ix, offset.item_ix);
                assert_eq!(after.offset_in_item, offset.offset_in_item);
            });
            window.click("chat-worker-shelf-expand", cx);
            window.render_frame(cx);
            assert!(window.find("shelf-worker:worker:6").bounds().size.height > gpui::px(0.));
            assert!(window.find("chat-workers").bounds().size.height <= ui_text::space(250.));
            window.click("chat-worker-shelf-expand", cx);
            window.render_frame(cx);
            assert!(window.try_find("shelf-worker:worker:6").is_none());
        })
        .unwrap();
    }
}

#[gpui::test]
fn shelf_uses_live_state_once_per_session_and_prioritizes_attention(cx: &mut TestAppContext) {
    let (_handle, view, _recording) = editor_tests::mount(cx);
    view.update(cx, |view, cx| {
        let mut first = worker("same");
        first.id = "older-assignment".into();
        let mut historical = delegation(
            "historical",
            "wait",
            json!({"past":{"status":"completed","message":"Old result"}}),
            None,
        );
        historical.turn_id = Some("past-turn".into());
        let mut ended = worker("ended");
        ended.turn_id = Some("past-turn".into());
        ended.presentation.worker.as_mut().unwrap().noted_at = 1;
        install(
            view,
            vec![
                historical,
                ended,
                first,
                worker("same"),
                worker("waiting"),
                worker("failed"),
            ],
            cx,
        );
        view.set_subshells(
            BTreeMap::from([
                ("same".into(), live(LiveStatus::Working, "")),
                (
                    "waiting".into(),
                    live(LiveStatus::Waiting, "Need an answer"),
                ),
                ("failed".into(), live(LiveStatus::Failed, "Build failed")),
            ]),
            None,
            cx,
        );
        let rows = view.shelf_workers();
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            ["worker:failed", "worker:waiting", "worker:same"]
        );
        assert_eq!(
            view.worker_summary_sources().len(),
            2,
            "no output means no summary call"
        );
    });
}

#[gpui::test]
fn unknown_background_delegation_is_not_active_and_retires_with_its_turn(cx: &mut TestAppContext) {
    let (handle, view, _recording) = editor_tests::mount(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |view, cx| {
            let mut info = testing::info("fixture-chat");
            info.provider = Provider::Claude;
            view.adopt_info_for_tests(info);
            view.model.link = Link::Live;
            view.model.transcript.apply(&ChatEvent::ItemCompleted {
                item: Item {
                    id: "background-agent".into(),
                    turn_id: Some("previous-turn".into()),
                    status: ItemStatus::Completed,
                    body: ItemBody::ToolCall {
                        server: None,
                        tool: "Agent".into(),
                        input: json!({
                            "subagent_type":"Explore",
                            "description":"Inspect the poller",
                            "run_in_background":true,
                        }),
                        output: Some("Started in background".into()),
                    },
                    presentation: Default::default(),
                },
            });
            view.refresh_projection(cx);
            let rows = view.shelf_workers();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].label, "Unreported");
            assert!(
                !rows[0].active,
                "operation completion does not establish child activity"
            );
            cx.notify();
        });
        window.render_frame(cx);
        assert!(
            window
                .find("shelf-worker:subagent:item:background-agent")
                .label()
                .unwrap()
                .contains("Unreported")
        );
        view.update(cx, |view, cx| {
            view.model.transcript.apply(&ChatEvent::TurnStarted {
                turn_id: "new-turn".into(),
            });
            view.refresh_projection(cx);
            assert!(view.shelf_workers().is_empty());
            assert!(view.worker_summary_sources().is_empty());
            cx.notify();
        });
        window.render_frame(cx);
        assert!(
            window.try_find("chat-workers").is_none(),
            "unreported history does not stay pinned"
        );
    })
    .unwrap();
}

#[gpui::test]
fn shelf_details_preserve_transcript_actions_and_message_draft(cx: &mut TestAppContext) {
    let (handle, view, _recording) = editor_tests::mount(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |view, cx| {
            install(view, vec![worker("w")], cx);
            view.set_subshells(
                BTreeMap::from([("w".into(), live(LiveStatus::Working, "Reviewing"))]),
                None,
                cx,
            );
            view.list.pause_following_tail();
            view.list.scroll_to(ListOffset {
                item_ix: 0,
                offset_in_item: gpui::px(0.),
            });
        });
        window.render_frame(cx);
        window.click("shelf-worker:worker:w", cx);
        window.render_frame(cx);
        assert!(window.find("shelf-detail:worker:w").visible());
        assert!(
            window.find("worker-open:w").visible(),
            "the timeline keeps its action"
        );
        window.click("shelf-message:w", cx);
        window.render_frame(cx);
        window.input("Review cancellation too", cx);
        window.click("shelf-worker:worker:w", cx);
        window.render_frame(cx);
        assert!(window.try_find("shelf-detail:worker:w").is_none());
        assert!(window.find("chat-worker-bar").visible());
        view.read_with(cx, |view, cx| {
            assert_eq!(
                view.workers
                    .message
                    .as_ref()
                    .unwrap()
                    .1
                    .read(cx)
                    .value()
                    .as_ref(),
                "Review cancellation too",
            )
        });
    })
    .unwrap();
}
