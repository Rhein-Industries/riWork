//! Real rendered cards with recording events: no shell manager, host or provider.
use super::*;
use crate::activity::AgentActivity;
use crate::chat::model::{ChatEvent, Item, ItemStatus, Provider};
use gpui::{SharedString, TestAppContext};
use gpui_kit::test::TestWindowExt;
use riwork_engine::subshells::{ShellCard, TaskLink};
use std::sync::Mutex;

fn card(activity: AgentActivity) -> ShellCard {
    ShellCard {
        id: "worker".into(),
        harness: "Claude".into(),
        model: None,
        tasks: vec![TaskLink {
            id: "task".into(),
            title: "Fix polling".into(),
        }],
        activity,
        alive: true,
        created_at: crate::activity::unix_now().saturating_sub(90),
        ended_at: None,
        output: "First line\nLast output".into(),
        output_error: None,
    }
}

#[gpui::test]
fn worker_appears_updates_and_collapses_without_taking_composer_focus(cx: &mut TestAppContext) {
    let (handle, view, _recording) = editor_tests::mount(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("chat-composer", cx);
        window.input("Keep my draft", cx);
        view.update(cx, |view, cx| {
            view.set_subshells(
                vec![card(AgentActivity::Working)],
                "Live · last checked 0s ago · next check in 2s".into(),
                cx,
            )
        });
        window.render_frame(cx);
        assert!(window.find("worker:worker").visible());
        assert_eq!(window.find("worker-details:worker").expanded(), Some(true));
        assert!(
            window
                .find("worker-details:worker")
                .label()
                .unwrap()
                .contains("Fix polling · Working")
        );
        assert!(window.find("worker-output:worker").visible());
        view.read_with(cx, |view, cx| {
            assert!(view.composer.read(cx).focus_handle(cx).is_focused(window));
            assert_eq!(view.composer_text(cx), "Keep my draft");
        });
        let mut next = card(AgentActivity::Waiting);
        next.output = "New output".into();
        view.update(cx, |view, cx| {
            view.set_subshells(
                vec![next],
                "Backing off · last checked 4s ago · next check in 4s".into(),
                cx,
            )
        });
        window.render_frame(cx);
        assert!(
            window
                .find("worker-details:worker")
                .label()
                .unwrap()
                .contains("Waiting for input")
        );
        let mut done = card(AgentActivity::Done);
        done.ended_at = Some(done.created_at + 90);
        view.update(cx, |view, cx| {
            view.set_subshells(
                vec![done],
                "Live · last checked 0s ago · next check in 2s".into(),
                cx,
            )
        });
        window.render_frame(cx);
        assert_eq!(window.find("worker-details:worker").expanded(), Some(false));
        assert!(window.try_find("worker-output:worker").is_none());
        view.read_with(cx, |view, cx| {
            assert!(view.composer.read(cx).focus_handle(cx).is_focused(window))
        });
        window.click("worker-details:worker", cx);
        window.render_frame(cx);
        assert!(
            window.find("worker-output:worker").visible(),
            "finished output remains reviewable"
        );
    })
    .unwrap();
}

#[gpui::test]
fn worker_actions_dispatch_once_and_a_refused_message_keeps_its_text(cx: &mut TestAppContext) {
    let (handle, view, _recording) = editor_tests::mount(cx);
    let events = Arc::new(Mutex::new(Vec::new()));
    let received = events.clone();
    let _subscription = view.update(cx, |_, cx| {
        cx.subscribe(&view, move |_, _, event, cx| {
            if let ChatViewEvent::WorkerShell { shell_id, action } = event {
                received
                    .lock()
                    .unwrap()
                    .push((shell_id.clone(), action.clone()));
            }
            let _ = cx;
        })
    });
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |view, cx| {
            view.set_subshells(vec![card(AgentActivity::Working)], "Live".into(), cx)
        });
        window.render_frame(cx);
        window.click("worker-open:worker", cx);
        window.click("worker-message:worker", cx);
        window.render_frame(cx);
        window.input("Review this 🦀", cx);
        window.click("worker-send", cx);
        window.render_frame(cx);
        window.click("worker-send", cx);
        view.update(cx, |view, cx| {
            view.subshell_action_finished(
                "worker",
                Err("Harness is waiting for folder trust; input was not sent".into()),
                cx,
            )
        });
        window.render_frame(cx);
        assert!(window.find("chat-workers-error").visible());
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
                "Review this 🦀"
            )
        });
        window.click("worker-cancel-message", cx);
        window.render_frame(cx);
        window.click("worker-stop:worker", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        events.lock().unwrap().as_slice(),
        [
            ("worker".into(), ShellAction::Open),
            ("worker".into(), ShellAction::Send("Review this 🦀".into())),
            ("worker".into(), ShellAction::Stop),
        ],
        "one explicit send, with no retry after refusal"
    );
}

#[gpui::test]
fn provider_delegation_has_a_structured_header_and_expandable_result(cx: &mut TestAppContext) {
    let (handle, view, _recording) = editor_tests::mount(cx);
    view.update(cx,|view,cx| {
        let mut info = testing::info("fixture-chat"); info.provider = Provider::Claude;
        view.adopt_info_for_tests(info);
        view.model.link = Link::Live;
        view.model.transcript.apply(&ChatEvent::ItemCompleted {item:Item {id:"agent".into(),turn_id:Some("turn".into()),status:ItemStatus::Completed,
            body:ItemBody::ToolCall {server:None,tool:"Agent".into(),input:serde_json::json!({"subagent_type":"Explore","description":"Find the handler"}),output:Some("Found in router.rs".into())},presentation:Default::default() }});
        view.refresh_projection(cx);
        cx.notify();
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("card:agent").label(),
            Some("Subagent · Explore")
        );
        window.click("card:agent", cx);
        window.render_frame(cx);
        assert!(
            window
                .try_find(SharedString::from("subagent-result:agent"))
                .is_some()
        );
        let (from, to) = view
            .read(cx)
            .transcript_selection
            .glyph_span_points("subagent:agent");
        window.drag(from, to, cx);
        window.press("cmd-c", cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|c| c.text()),
            Some("Found in router.rs".into())
        );
    })
    .unwrap();
}
