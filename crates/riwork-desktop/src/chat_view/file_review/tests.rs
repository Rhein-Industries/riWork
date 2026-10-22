use super::*;
use crate::chat::model::{ItemStatus, MessagePhase, Presentation, TurnOutcome};
use gpui::{KeyBinding, TestAppContext};
use gpui_kit::test::TestWindowExt;

fn change(path: &str, kind: ChangeKind, patch: &str) -> FileChange {
    FileChange {
        path: path.into(),
        kind,
        diff: Some(patch.into()),
    }
}

fn items(changes: Vec<FileChange>) -> Vec<Item> {
    vec![
        Item {
            id: "review-edit".into(),
            turn_id: Some("review-turn".into()),
            status: ItemStatus::Completed,
            presentation: Presentation::default(),
            body: ItemBody::FileChange { changes },
        },
        Item {
            id: "review-answer".into(),
            turn_id: Some("review-turn".into()),
            status: ItemStatus::Completed,
            presentation: Presentation {
                phase: Some(MessagePhase::Final),
                ..Default::default()
            },
            body: ItemBody::AgentMessage {
                text: "Reported changes are ready.".into(),
            },
        },
    ]
}

#[test]
fn folder_order_keeps_actual_edit_identity_and_hunks_keep_original_coordinates() {
    let ordered = files([
        ("edit".into(), 0, "src/z.rs".into()),
        ("edit".into(), 1, "README.md".into()),
        ("edit".into(), 2, "src/a.rs".into()),
    ]);
    assert_eq!(
        ordered.iter().map(|file| file.change).collect::<Vec<_>>(),
        [1, 2, 0]
    );
    assert_eq!(ordered[1].key(), "edit#2");
    let edit = change(
        "src/a.rs",
        ChangeKind::Modify,
        "@@ -8 +10 @@\n-old\n+new\n@@ -40 +50 @@\n-before\n+after",
    );
    assert_eq!(hunks(&edit), [0..3, 3..6]);
    let lines = diff::display(edit.diff.as_deref().unwrap(), edit.kind).0;
    assert_eq!(
        diff::line_numbers(&lines, edit.kind)[4..],
        [(Some(40), None), (None, Some(50))]
    );
}

#[test]
fn rename_and_missing_patch_descriptions_do_not_invent_workspace_state() {
    assert_eq!(
        description(&change(
            "new.rs",
            ChangeKind::Rename,
            "rename from old.rs\nrename to new.rs"
        )),
        "Reported rename: old.rs → new.rs"
    );
    assert!(description(&change("new.rs", ChangeKind::Rename, "")).contains("not supplied"));
    assert!(description(&change("gone.rs", ChangeKind::Delete, "")).starts_with("Reported"));
    assert!(hunks(&change("gone.rs", ChangeKind::Delete, "")).is_empty());
}

#[gpui::test]
fn native_review_browses_folders_files_and_hunks_without_moving_the_transcript(
    cx: &mut TestAppContext,
) {
    let (handle, view, recording) = super::super::editor_tests::mount_selection(cx);
    let patch = "@@ -8 +10 @@\n-old\n+needle\n@@ -40 +50 @@\n-before\n+after";
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |view, cx| {
            view.model.link = super::super::Link::Live;
            view.model.transcript.items = items(vec![
                change("src/a.rs", ChangeKind::Modify, patch),
                change("tests/a.rs", ChangeKind::Delete, "-old"),
            ]);
            view.model.completed = vec![("review-turn".into(), TurnOutcome::Completed, 2)];
            view.refresh_projection(cx);
            view.list.set_follow_mode(gpui::FollowMode::Normal);
            cx.notify();
        });
        window.render_frame(cx);
        let before = view.read(cx).list.logical_scroll_top();
        window.click("review-answer:review-answer", cx);
        window.render_frame(cx);
        assert!(view.read(cx).file_review.is_some());
        assert!(window.try_find("review-folder:src").is_some());
        window.click("file-review-collapse-all", cx);
        window.render_frame(cx);
        assert!(window.try_find("review-file:0").is_none());
        window.click("file-review-expand-all", cx);
        window.render_frame(cx);
        assert!(window.try_find("review-file:0").is_some());
        window.click("file-review-hunk-next", cx);
        window.render_frame(cx);
        assert_eq!(view.read(cx).file_review.as_ref().unwrap().hunk, 1);
        window.click("file-review-comment", cx);
        window.render_frame(cx);
        let refs = view.read(cx).references.contents();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].excerpt, "@@ -40 +50 @@\n-before\n+after");
        match &refs[0].origin {
            crate::chat_drafts::references::ReferenceOrigin::Diff {
                item,
                change,
                first,
                last,
                old,
                new,
                ..
            } => {
                assert_eq!(item, "review-edit");
                assert_eq!((*change, *first, *last), (0, 3, 5));
                assert_eq!(old, &[(40, 40)]);
                assert_eq!(new, &[(50, 50)]);
            }
            _ => panic!("a hunk comment must preserve diff provenance"),
        }
        window.click("file-review-next", cx);
        window.render_frame(cx);
        assert_eq!(view.read(cx).file_review.as_ref().unwrap().selected, 1);
        assert!(
            window.try_find("file-review-open").is_none(),
            "deleted files cannot claim to open a live workspace file"
        );
        assert_eq!(
            window.find("transcript:path:review-edit#1").label(),
            Some("tests/a.rs")
        );
        window.click("copy-diff:review-edit#1", cx);
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().as_deref(),
            Some("-old")
        );
        window.click("file-review-back", cx);
        window.render_frame(cx);
        assert!(view.read(cx).file_review.is_none());
        let after = view.read(cx).list.logical_scroll_top();
        assert_eq!(
            (before.item_ix, before.offset_in_item),
            (after.item_ix, after.offset_in_item)
        );
        assert!(!view.read(cx).list.is_following_tail());
    })
    .unwrap();
    assert!(recording.try_recv().is_err());
}

#[gpui::test]
fn many_files_render_a_bounded_browser_and_find_uses_the_current_patch(cx: &mut TestAppContext) {
    let (handle, view, recording) = super::super::editor_tests::mount_selection(cx);
    cx.update(|cx| {
        cx.bind_keys([
            KeyBinding::new("cmd-f", super::super::FindInChat, Some("ChatView")),
            KeyBinding::new("cmd-f", super::super::FindInChat, Some("ChatView > Input")),
        ])
    });
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |view, cx| {
            view.model.link = super::super::Link::Live;
            view.model.transcript.items = items(
                (0..120)
                    .map(|at| {
                        change(
                            &format!("src/file-{at:03}.rs"),
                            ChangeKind::Add,
                            if at == 0 { "needle" } else { "other" },
                        )
                    })
                    .collect(),
            );
            view.model.completed = vec![("review-turn".into(), TurnOutcome::Completed, 2)];
            view.refresh_projection(cx);
            view.composer.update(cx, |state, cx| {
                state.set_value("Keep this draft", window, cx)
            });
            cx.notify();
        });
        window.render_frame(cx);
        assert!(
            window
                .try_find("transcript:path:answer-file:review-answer:8")
                .is_none()
        );
        window.click("review-answer:review-answer", cx);
        window.render_frame(cx);
        assert!(window.try_find("review-file:10").is_some());
        assert!(
            window.try_find("review-file:11").is_none(),
            "folder header counts toward the bounded page"
        );
        let focus = view.read(cx).focus.clone();
        focus.focus(window, cx);
        window.press("cmd-f", cx);
        window.input("needle", cx);
    })
    .unwrap();
    // Native input emits Change after the window transaction; let that event
    // reach Find before checking its search and painting the next frame.
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(view.read(cx).find.status().1, 1);
        assert!(
            window
                .find("chat-find-scope")
                .label()
                .unwrap()
                .contains("current file")
        );
        window.click("file-review-next", cx);
        window.render_frame(cx);
        assert_eq!(
            view.read(cx).find.status().1,
            0,
            "Find must not paint hits from the preceding file"
        );
        assert_eq!(view.read(cx).composer_text(cx), "Keep this draft");
    })
    .unwrap();
    assert!(recording.try_recv().is_err());
}

#[gpui::test]
fn replacing_a_patch_clamps_hunk_actions_and_back_cancels_queued_review_jumps(
    cx: &mut TestAppContext,
) {
    let (handle, view, recording) = super::super::editor_tests::mount_selection(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |view, cx| {
            view.model.link = super::super::Link::Live;
            view.model.transcript.items = items(vec![change(
                "src/a.rs",
                ChangeKind::Modify,
                "@@ -1 +1 @@\n-a\n+b\n@@ -20 +30 @@\n-old\n+new",
            )]);
            view.model.completed = vec![("review-turn".into(), TurnOutcome::Completed, 2)];
            view.refresh_projection(cx);
            view.list.set_follow_mode(gpui::FollowMode::Normal);
            view.open_file_review_at("review-edit", 0, Some(4), window, cx);
        });
        window.render_frame(cx);
        assert_eq!(view.read(cx).file_review.as_ref().unwrap().hunk, 1);
        let old_revision = crate::chat_drafts::references::revision(
            "@@ -1 +1 @@\n-a\n+b\n@@ -20 +30 @@\n-old\n+new",
        );
        view.update(cx, |view, _cx| {
            let ItemBody::FileChange { changes } = &mut view.model.transcript.items[0].body else {
                panic!("fixture edit")
            };
            changes[0].diff = Some("@@ -1 +1 @@\n-a\n+c".into());
            view.transcript_selection.changed();
        });
        // Admit the exact revision/range captured by the old frame. A native
        // helper click redraws first, so it would instead capture the new patch.
        view.update(cx, |view, cx| {
            assert!(!view.comment_review_hunk("review-edit", 0, 3..6, &old_revision, window, cx));
        });
        assert!(view.read(cx).references.is_empty());
        window.render_frame(cx);
        assert_eq!(
            view.read(cx).file_review.as_ref().unwrap().hunk,
            0,
            "patch shrink must settle the stored position before navigation"
        );
        window.click("file-review-hunk-previous", cx);
        assert_eq!(view.read(cx).file_review.as_ref().unwrap().hunk, 0);
        window.click("file-review-comment", cx);
        assert_eq!(
            view.read(cx).references.contents()[0].excerpt,
            "@@ -1 +1 @@\n-a\n+c",
            "a newly painted comment must capture the current patch"
        );
        window.click("file-review-back", cx);
        window.render_frame(cx);
        let before = view.read(cx).list.logical_scroll_top();
        window.simulate_next_frame(cx);
        window.render_frame(cx);
        let after = view.read(cx).list.logical_scroll_top();
        assert_eq!(
            (before.item_ix, before.offset_in_item),
            (after.item_ix, after.offset_in_item)
        );
        assert!(view.read(cx).file_review.is_none());
    })
    .unwrap();
    assert!(recording.try_recv().is_err());
}

#[gpui::test]
fn review_selection_survives_background_rows_but_retires_changed_patch_text(
    cx: &mut TestAppContext,
) {
    let (handle, view, recording) = super::super::editor_tests::mount_selection(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |view, cx| {
            view.model.link = super::super::Link::Live;
            let mut transcript = items(vec![change(
                "src/a.rs",
                ChangeKind::Modify,
                "@@ -1 +1 @@\n-old\n+keep this selected",
            )]);
            transcript.insert(
                0,
                Item {
                    id: "background-message".into(),
                    turn_id: Some("review-turn".into()),
                    status: ItemStatus::Completed,
                    presentation: Presentation::default(),
                    body: ItemBody::UserMessage {
                        text: "Review this change".into(),
                    },
                },
            );
            view.model.transcript.items = transcript;
            view.model.completed = vec![("review-turn".into(), TurnOutcome::Completed, 3)];
            view.refresh_projection(cx);
            view.open_file_review_at("review-edit", 0, None, window, cx);
        });
        window.render_frame(cx);
        let (from, to) = view
            .read(cx)
            .transcript_selection
            .glyph_span_points("diff:review-edit#0:2");
        window.drag(from, to, cx);
        assert_eq!(
            view.read(cx).transcript_selection.selected_text(cx),
            "+keep this selected"
        );
        view.update(cx, |view, cx| {
            view.model.transcript.items[0].body = ItemBody::UserMessage {
                text: "Unrelated replacement".into(),
            };
            view.sync_list(
                &super::super::Applied {
                    touched: [0].into_iter().collect(),
                    ..Default::default()
                },
                cx,
            );
            assert_eq!(
                view.transcript_selection.selected_text(cx),
                "+keep this selected"
            );
            let before = view.visible.clone();
            view.open
                .insert(super::super::display::activity_key("review-turn"));
            view.refresh_projection(cx);
            assert_ne!(
                view.visible, before,
                "fixture changes background row projection"
            );
            view.refresh_selection_document(Look::of(cx), cx);
            assert_eq!(
                view.transcript_selection.selected_text(cx),
                "+keep this selected"
            );
            let ItemBody::FileChange { changes } = &mut view.model.transcript.items[1].body else {
                panic!("fixture edit")
            };
            changes[0].diff = Some("@@ -1 +1 @@\n-old\n+different text".into());
            view.sync_list(
                &super::super::Applied {
                    touched: [1].into_iter().collect(),
                    ..Default::default()
                },
                cx,
            );
            assert!(
                view.transcript_selection.selected_text(cx).is_empty(),
                "changed patch cannot be copied from the previous source projection"
            );
        });
    })
    .unwrap();
    assert!(recording.try_recv().is_err());
}

#[gpui::test]
fn hunk_navigation_reveals_the_target_inside_both_review_scroll_regions(cx: &mut TestAppContext) {
    let (handle, view, _) = super::super::editor_tests::mount_selection(cx);
    let patch = format!(
        "@@ -1,150 +1,150 @@\n{}@@ -200 +300 @@\n-old\n+new",
        " unchanged\n".repeat(150)
    );
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |view, cx| {
            view.model.link = super::super::Link::Live;
            view.model.transcript.items =
                items(vec![change("src/a.rs", ChangeKind::Modify, &patch)]);
            view.model.completed = vec![("review-turn".into(), TurnOutcome::Completed, 2)];
            view.refresh_projection(cx);
            view.open_file_review_at("review-edit", 0, None, window, cx);
        });
        window.render_frame(cx);
        window.click("file-review-hunk-next", cx);
    })
    .unwrap();
    for _ in 0..12 {
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.simulate_next_frame(cx);
        })
        .unwrap();
        cx.run_until_parked();
    }
    view.read_with(cx, |view, _| {
        let viewport = view.find_list().viewport_bounds();
        let key = "diff:review-edit#0:151";
        let rect = view.transcript_selection.with_source(|source| {
            let at = source.iter().position(|leaf| leaf.key == key).unwrap();
            view.transcript_selection.painted_rect(at, 0..1).unwrap()
        });
        assert!(
            rect.top() >= viewport.top() && rect.bottom() <= viewport.bottom(),
            "hunk header {rect:?} must be inside review viewport {viewport:?}"
        );
        let scroller = view.caches.borrow().scrollers["diff:review-edit#0"].clone();
        let inner = scroller.handle().bounds();
        assert!(rect.top() >= inner.top() && rect.bottom() <= inner.bottom());
    });
}
