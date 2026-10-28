//! A chat's unsent draft across a tab switch, closing and reopening the tab, and a restart.
//! A view is made as `ChatView::open` makes it, with a recording feed instead of a host.
use super::*;
use crate::chat::client::CallError;
use crate::chat_drafts::{self, ChatDrafts, Drafts};
use gpui::{AnyWindowHandle, Bounds, TestAppContext, WindowBounds, WindowOptions, point, size};
use gpui_kit::test::TestWindowExt;
use std::{path::PathBuf, sync::mpsc::Receiver, time::SystemTime};

fn home() -> PathBuf {
    let path = std::env::temp_dir().join(format!("riwork-draft-view-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn chat_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// The app as it starts: its globals, and the drafts read from `home`.
fn start(cx: &mut TestAppContext, home: &std::path::Path) {
    cx.update(|cx| {
        cx.set_global(crate::settings::Settings::default());
        cx.set_global(crate::theme::Appearance {
            selected: crate::theme::ThemeChoice::Native,
            palette: crate::theme::Palette::NATIVE_LIGHT,
            terminal: None,
            ghostty: None,
        });
        text_input::init(cx);
        chat_drafts::init(home, cx);
    });
}

/// The tab of chat `id`, opened as `ChatView::open` does.
fn open_view(
    id: &str,
    window: &mut Window,
    cx: &mut gpui::App,
) -> (Entity<ChatView>, Receiver<feed::Delivery>) {
    let (recording_feed, recording) = Feed::recording();
    let view = cx.new(|cx| {
        let mut view = ChatView::blank(
            HostConfig {
                ensure: Arc::new(|| Err("fixture staging is disabled".into())),
            },
            window,
            cx,
        );
        view.chat_id = Some(id.to_owned());
        view.restore_draft(window, cx);
        view.feed = Some(recording_feed);
        view
    });
    (view, recording)
}

/// A pane: the tabs' views, of which the active one is drawn.
struct Pane {
    tabs: Vec<Entity<ChatView>>,
    active: usize,
}
impl Render for Pane {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.tabs[self.active].clone())
    }
}

/// A window with a pane of one tab per chat in `ids`, the first one shown.
fn mount(
    cx: &mut TestAppContext,
    ids: &[&str],
) -> (
    AnyWindowHandle,
    Entity<Pane>,
    Vec<(Entity<ChatView>, Receiver<feed::Delivery>)>,
) {
    let (handle, pane, tabs) = cx.update(|cx| {
        let mut made = None;
        let handle = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.), px(0.)),
                        size(px(700.), px(800.)),
                    ))),
                    ..Default::default()
                },
                |window, cx| {
                    let tabs: Vec<_> = ids.iter().map(|id| open_view(id, window, cx)).collect();
                    let pane = cx.new(|_| Pane {
                        tabs: tabs.iter().map(|(view, _)| view.clone()).collect(),
                        active: 0,
                    });
                    made = Some((pane.clone(), tabs));
                    cx.new(|cx| gpui_kit::base::Root::new(pane, window, cx))
                },
            )
            .unwrap();
        let (pane, tabs) = made.unwrap();
        (handle.into(), pane, tabs)
    });
    cx.update_window(handle, |_, window, _| window.activate_window())
        .unwrap();
    cx.run_until_parked();
    (handle, pane, tabs)
}

fn type_into(cx: &mut TestAppContext, handle: AnyWindowHandle, text: &str) {
    cx.update_window(handle, |_, window, cx| {
        window.click("chat-composer", cx);
        window.input(text, cx);
    })
    .unwrap();
    cx.run_until_parked();
}

fn show(cx: &mut TestAppContext, pane: &Entity<Pane>, tab: usize) {
    pane.update(cx, |pane, cx| {
        pane.active = tab;
        cx.notify();
    });
    cx.run_until_parked();
}

/// Close the window and with it every tab's view, as closing a tab drops its view.
fn close(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    cx.update_window(handle, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
}

fn delivery(cx: &mut TestAppContext, recording: &Receiver<feed::Delivery>) -> feed::Delivery {
    // Wait for the real writer, then let the background durability continuation run.
    // No receipt is synthesized: the recording feed must observe the actual dispatch.
    cx.update(|cx| cx.global::<ChatDrafts>().0.flush());
    cx.run_until_parked();
    recording
        .try_recv()
        .expect("submission after durable recovery write")
}

fn text(cx: &mut TestAppContext, view: &Entity<ChatView>) -> String {
    view.read_with(cx, |v, cx| v.composer_text(cx))
}

#[gpui::test]
fn each_chat_keeps_its_draft_while_another_tab_is_shown(cx: &mut TestAppContext) {
    let home = home();
    start(cx, &home);
    let (a, b) = (chat_id(), chat_id());
    let (handle, pane, tabs) = mount(cx, &[&a, &b]);
    type_into(cx, handle, "draft of A 🦀");
    show(cx, &pane, 1);
    type_into(cx, handle, "draft of B");
    show(cx, &pane, 0);
    assert_eq!(text(cx, &tabs[0].0), "draft of A 🦀");
    assert_eq!(text(cx, &tabs[1].0), "draft of B");
    // Typing goes on where it stopped: the hidden tab's editor is the same one.
    type_into(cx, handle, "!");
    assert_eq!(text(cx, &tabs[0].0), "draft of A 🦀!");
    assert!(
        tabs.iter()
            .all(|(_, recording)| recording.try_recv().is_err())
    );
    cx.update(|cx| cx.global::<ChatDrafts>().0.flush());
    let _ = std::fs::remove_dir_all(home);
}

#[gpui::test]
fn a_closed_tab_comes_back_with_its_draft_and_another_chat_without(cx: &mut TestAppContext) {
    let home = home();
    start(cx, &home);
    let (a, b) = (chat_id(), chat_id());
    let (handle, _, tabs) = mount(cx, &[&a]);
    type_into(cx, handle, "half a thought\n  indented");
    drop(tabs);
    close(cx, handle);
    let (_, _, tabs) = mount(cx, &[&a, &b]);
    assert_eq!(text(cx, &tabs[0].0), "half a thought\n  indented");
    assert_eq!(text(cx, &tabs[1].0), "");
    cx.update(|cx| cx.global::<ChatDrafts>().0.flush());
    let _ = std::fs::remove_dir_all(home);
}

#[gpui::test]
fn a_draft_survives_a_restart_until_it_is_sent(cx: &mut TestAppContext) {
    let home = home();
    start(cx, &home);
    let a = chat_id();
    let (handle, _, tabs) = mount(cx, &[&a]);
    type_into(cx, handle, "before the restart");
    drop(tabs);
    close(cx, handle);
    cx.update(|cx| cx.global::<ChatDrafts>().0.flush());
    // A new process reads only what is on disk.
    cx.update(|cx| cx.set_global(ChatDrafts(Drafts::load(&home, SystemTime::now()))));
    let (handle, _, tabs) = mount(cx, &[&a]);
    let (view, recording) = &tabs[0];
    assert_eq!(text(cx, view), "before the restart");
    // Restored text is a draft like any other: sending it clears it everywhere.
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |v, cx| v.send_message(window, cx))
    })
    .unwrap();
    let (id, command) = match delivery(cx, recording) {
        feed::Delivery::Submission { id, command } => (id, command),
        _ => panic!("the composer sends one correlated submission"),
    };
    assert_eq!(
        command,
        ChatCommand::Send {
            text: "before the restart".into()
        }
    );
    assert_eq!(cx.update(|cx| chat_drafts::draft(&a, cx)), None);
    cx.update(|cx| cx.global::<ChatDrafts>().0.flush());
    assert_eq!(Drafts::load(&home, SystemTime::now()).get(&a), None);
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |v, cx| {
            v.submission_receipt(id, command, Ok(()), window, cx)
        })
    })
    .unwrap();
    assert_eq!(text(cx, view), "");
    assert_eq!(cx.update(|cx| chat_drafts::draft(&a, cx)), None);
    cx.update(|cx| cx.global::<ChatDrafts>().0.flush());
    assert_eq!(Drafts::load(&home, SystemTime::now()).get(&a), None);
    let _ = std::fs::remove_dir_all(home);
}

#[gpui::test]
fn closing_a_tab_after_dispatch_does_not_restore_the_sent_draft(cx: &mut TestAppContext) {
    let home = home();
    start(cx, &home);
    let a = chat_id();
    let (handle, pane, tabs) = mount(cx, &[&a]);
    type_into(cx, handle, "send and close");
    cx.update_window(handle, |_, window, cx| {
        tabs[0].0.update(cx, |v, cx| v.send_message(window, cx));
    })
    .unwrap();
    assert!(matches!(
        delivery(cx, &tabs[0].1),
        feed::Delivery::Submission { .. }
    ));
    drop(tabs);
    drop(pane);
    close(cx, handle);
    cx.update(|cx| cx.global::<ChatDrafts>().0.flush());
    assert_eq!(Drafts::load(&home, SystemTime::now()).get(&a), None);
    let (_, _, tabs) = mount(cx, &[&a]);
    assert_eq!(text(cx, &tabs[0].0), "");
    tabs[0].0.read_with(cx, |view, _| {
        assert_eq!(view.submissions.len(), 1);
        assert_eq!(view.submissions[0].snapshot.text, "send and close");
        assert!(matches!(
            view.submissions[0].status,
            attachment_draft::Status::Uncertain(_)
        ));
        assert!(view.pending_submission.is_none());
    });
    assert!(tabs[0].1.try_recv().is_err(), "reopening never resends");
    cx.update(|cx| cx.global::<ChatDrafts>().0.flush());
    let _ = std::fs::remove_dir_all(home);
}

#[gpui::test]
fn refused_and_uncertain_drafts_are_persisted_again(cx: &mut TestAppContext) {
    let home = home();
    start(cx, &home);
    for error in [
        CallError::Refused("no".into()),
        CallError::Broken("reply lost".into()),
    ] {
        let a = chat_id();
        let (handle, _, tabs) = mount(cx, &[&a]);
        type_into(cx, handle, "  keep this draft 🦀");
        cx.update_window(handle, |_, window, cx| {
            tabs[0].0.update(cx, |v, cx| v.send_message(window, cx));
        })
        .unwrap();
        let feed::Delivery::Submission { id, command } = delivery(cx, &tabs[0].1) else {
            panic!("a submission");
        };
        assert_eq!(cx.update(|cx| chat_drafts::draft(&a, cx)), None);
        cx.update_window(handle, |_, window, cx| {
            tabs[0].0.update(cx, |v, cx| {
                v.submission_receipt(id, command, Err(error), window, cx)
            });
        })
        .unwrap();
        cx.update(|cx| cx.global::<ChatDrafts>().0.flush());
        assert_eq!(
            Drafts::load(&home, SystemTime::now()).get(&a),
            Some("  keep this draft 🦀")
        );
        type_into(cx, handle, " newer");
        cx.update_window(handle, |_, window, cx| {
            tabs[0]
                .0
                .update(cx, |v, cx| v.restore_snapshot(id, window, cx));
        })
        .unwrap();
        cx.update(|cx| cx.global::<ChatDrafts>().0.flush());
        assert_eq!(text(cx, &tabs[0].0), "  keep this draft 🦀");
        assert_eq!(
            Drafts::load(&home, SystemTime::now()).get(&a),
            Some("  keep this draft 🦀")
        );
        drop(tabs);
        close(cx, handle);
    }
    cx.update(|cx| cx.global::<ChatDrafts>().0.flush());
    let _ = std::fs::remove_dir_all(home);
}

#[gpui::test]
fn a_deleted_link_forgets_the_chat_draft(cx: &mut TestAppContext) {
    let home = home();
    start(cx, &home);
    let a = chat_id();
    let (handle, _, tabs) = mount(cx, &[&a]);
    type_into(cx, handle, "delete this draft");
    cx.update(|cx| cx.global::<ChatDrafts>().0.flush());
    cx.update_window(handle, |_, window, cx| {
        tabs[0].0.update(cx, |v, cx| {
            v.accept(vec![FeedMsg::Link(Link::Deleted)], window, cx)
        });
    })
    .unwrap();
    assert_eq!(cx.update(|cx| chat_drafts::draft(&a, cx)), None);
    cx.update(|cx| cx.global::<ChatDrafts>().0.flush());
    assert_eq!(Drafts::load(&home, SystemTime::now()).get(&a), None);
    let _ = std::fs::remove_dir_all(home);
}

#[gpui::test]
fn deleting_a_chat_forgets_its_draft_only_on_success(cx: &mut TestAppContext) {
    let home = home();
    start(cx, &home);
    let a = chat_id();
    let (handle, _, tabs) = mount(cx, &[&a]);
    type_into(cx, handle, "delete this draft");
    for result in [Err("refused".into()), Ok(())] {
        let expected = if result.is_ok() {
            None
        } else {
            Some("delete this draft")
        };
        cx.update_window(handle, |_, _, cx| {
            tabs[0].0.update(cx, |v, cx| v.delete_result(result, cx));
        })
        .unwrap();
        cx.update(|cx| cx.global::<ChatDrafts>().0.flush());
        assert_eq!(
            cx.update(|cx| chat_drafts::draft(&a, cx)).as_deref(),
            expected
        );
        assert_eq!(Drafts::load(&home, SystemTime::now()).get(&a), expected);
    }
    let _ = std::fs::remove_dir_all(home);
}

#[gpui::test]
fn a_send_in_one_window_keeps_the_newer_draft_of_another(cx: &mut TestAppContext) {
    let home = home();
    start(cx, &home);
    let a = chat_id();
    let (first, _, first_tabs) = mount(cx, &[&a]);
    let (second, _, second_tabs) = mount(cx, &[&a]);
    type_into(cx, first, "sent from the first window");
    cx.update_window(first, |_, window, cx| {
        first_tabs[0]
            .0
            .update(cx, |v, cx| v.send_message(window, cx));
    })
    .unwrap();
    let feed::Delivery::Submission { id, command } = delivery(cx, &first_tabs[0].1) else {
        panic!("a submission");
    };
    // The second window saves newer text before the first hears its send went through.
    type_into(cx, second, "newer in the second window");
    cx.update_window(first, |_, window, cx| {
        first_tabs[0].0.update(cx, |v, cx| {
            v.submission_receipt(id, command, Ok(()), window, cx)
        });
    })
    .unwrap();
    assert_eq!(text(cx, &first_tabs[0].0), "");
    assert_eq!(
        cx.update(|cx| chat_drafts::draft(&a, cx)).as_deref(),
        Some("newer in the second window")
    );
    cx.update(|cx| cx.global::<ChatDrafts>().0.flush());
    assert_eq!(
        Drafts::load(&home, SystemTime::now()).get(&a),
        Some("newer in the second window")
    );
    drop((first_tabs, second_tabs));
    let _ = std::fs::remove_dir_all(home);
}

#[gpui::test]
fn a_failed_send_after_the_chat_was_deleted_does_not_bring_its_draft_back(cx: &mut TestAppContext) {
    let home = home();
    start(cx, &home);
    let a = chat_id();
    let (handle, _, tabs) = mount(cx, &[&a]);
    type_into(cx, handle, "lost with the chat");
    cx.update_window(handle, |_, window, cx| {
        tabs[0].0.update(cx, |v, cx| v.send_message(window, cx));
    })
    .unwrap();
    let feed::Delivery::Submission { id, command } = delivery(cx, &tabs[0].1) else {
        panic!("a submission");
    };
    // The deletion is heard before the send's failure.
    cx.update_window(handle, |_, window, cx| {
        tabs[0].0.update(cx, |v, cx| {
            v.accept(vec![FeedMsg::Link(Link::Deleted)], window, cx);
            v.submission_receipt(
                id,
                command,
                Err(CallError::Refused("gone".into())),
                window,
                cx,
            );
        });
    })
    .unwrap();
    // Nor does a late save of what the view still holds.
    tabs[0].0.update(cx, |v, cx| v.remember_draft(cx));
    assert_eq!(cx.update(|cx| chat_drafts::draft(&a, cx)), None);
    cx.update(|cx| cx.global::<ChatDrafts>().0.flush());
    assert_eq!(Drafts::load(&home, SystemTime::now()).get(&a), None);
    drop(tabs);
    let _ = std::fs::remove_dir_all(home);
}

#[gpui::test]
fn ready_attachment_draft_survives_restart_and_acknowledgment_clears_only_recovery(
    cx: &mut TestAppContext,
) {
    let home = home();
    let chat = chat_id();
    let chat_dir = home.join("chats").join(&chat);
    std::fs::create_dir_all(&chat_dir).unwrap();
    let source = home.join("notes.txt");
    std::fs::write(&source, "review this exact file").unwrap();
    let attachment = crate::chat::attachments::stage(&chat_dir, &source).unwrap();
    start(cx, &home);
    let (handle, pane, tabs) = mount(cx, &[&chat]);
    type_into(cx, handle, "with attachment");
    tabs[0].0.update(cx, |view, cx| {
        view.attachments
            .push(attachment_ui::Chip::ready(attachment.clone()));
        view.bump_generation();
        view.remember_draft(cx);
    });
    cx.update(|cx| cx.global::<ChatDrafts>().0.flush());
    drop((pane, tabs));
    close(cx, handle);
    cx.update(|cx| cx.set_global(ChatDrafts(Drafts::load(&home, SystemTime::now()))));
    let (handle, _, tabs) = mount(cx, &[&chat]);
    assert_eq!(text(cx, &tabs[0].0), "with attachment");
    tabs[0].0.read_with(cx, |view, _| {
        assert_eq!(view.attachments.len(), 1);
        assert_eq!(view.attachments[0].attachment(), Some(&attachment));
    });
    assert!(tabs[0].1.try_recv().is_err());
    cx.update_window(handle, |_, window, cx| {
        tabs[0]
            .0
            .update(cx, |view, cx| view.send_message(window, cx));
    })
    .unwrap();
    let feed::Delivery::Submission { id, command } = delivery(cx, &tabs[0].1) else {
        panic!("a submission")
    };
    assert!(
        matches!(&command, ChatCommand::SendAttachments { attachments, .. } if attachments == &vec![attachment.clone()])
    );
    cx.update_window(handle, |_, window, cx| {
        tabs[0].0.update(cx, |view, cx| {
            view.submission_receipt(id, command, Ok(()), window, cx)
        });
    })
    .unwrap();
    cx.update(|cx| cx.global::<ChatDrafts>().0.flush());
    assert!(
        Drafts::load(&home, SystemTime::now())
            .recover(&chat)
            .is_none()
    );
    assert!(
        attachment.path.is_file(),
        "the host retains submitted attachment bytes"
    );
    let _ = std::fs::remove_dir_all(home);
}

fn saved_attachment(home: &std::path::Path, chat: &str) -> crate::chat::attachments::Attachment {
    let directory = home.join("chats").join(chat);
    std::fs::create_dir_all(&directory).unwrap();
    let source = home.join("recover.txt");
    std::fs::write(&source, "saved attachment content").unwrap();
    crate::chat::attachments::stage(&directory, &source).unwrap()
}

#[gpui::test]
fn recovered_checks_allow_text_edits_block_send_and_remain_durable_before_completion(
    cx: &mut TestAppContext,
) {
    let home = home();
    let chat = chat_id();
    let attachment = saved_attachment(&home, &chat);
    start(cx, &home);
    let (handle, _, tabs) = mount(cx, &[&chat]);
    cx.update_window(handle, |_, window, cx| {
        cx.global_mut::<ChatDrafts>()
            .0
            .set_draft(
                &chat,
                chat_drafts::Contents {
                    text: "original".into(),
                    attachments: vec![attachment.clone()],
                    references: Vec::new(),
                },
                None,
            )
            .unwrap();
        tabs[0].0.update(cx, |view, cx| {
            view.restore_draft(window, cx);
            assert!(matches!(
                view.attachments[0].state,
                attachment_ui::Stage::Checking { .. }
            ));
            assert!(view.attachments[0].attachment().is_none());
            view.send_message(window, cx);
            assert!(view.pending_submission.is_none());
            view.composer.update(cx, |state, cx| {
                state.set_value("typed while checking", window, cx)
            });
            view.bump_generation();
            view.remember_draft(cx);
        });
        cx.global::<ChatDrafts>().0.flush();
        let restarted = Drafts::load(&home, SystemTime::now());
        let (record, warnings) = restarted.recover(&chat).unwrap();
        assert_eq!(record.draft.text, "typed while checking");
        assert_eq!(record.draft.attachments, vec![attachment.clone()]);
        assert_eq!(record.unready_attachments, 0);
        assert!(warnings.is_empty());
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(text(cx, &tabs[0].0), "typed while checking");
    assert!(tabs[0].1.try_recv().is_err());
    tabs[0].0.read_with(cx, |view, _| {
        assert_eq!(view.attachments[0].attachment(), Some(&attachment))
    });
    close(cx, handle);
    let _ = std::fs::remove_dir_all(home);
}

#[gpui::test]
fn checking_completion_never_resurrects_removed_chips_or_overwrites_another_editor(
    cx: &mut TestAppContext,
) {
    let home = home();
    let chat = chat_id();
    let attachment = saved_attachment(&home, &chat);
    start(cx, &home);
    let (handle, _, tabs) = mount(cx, &[&chat]);
    cx.update_window(handle, |_, window, cx| {
        tabs[0].0.update(cx, |view, cx| {
            view.restore_attachment_chips(vec![attachment.clone()], window, cx);
            view.bump_generation();
            view.remember_draft(cx);
            view.attachments.clear();
            view.bump_generation();
            view.remember_draft(cx);
        });
    })
    .unwrap();
    cx.run_until_parked();
    assert!(
        tabs[0]
            .0
            .read_with(cx, |view, _| view.attachments.is_empty())
    );
    cx.update_window(handle, |_, window, cx| {
        tabs[0].0.update(cx, |view, cx| {
            view.restore_attachment_chips(vec![attachment.clone()], window, cx);
            view.bump_generation();
            view.remember_draft(cx);
        });
        // Another live editor takes ownership before the validation continuation.
        cx.global_mut::<ChatDrafts>()
            .0
            .set_draft(
                &chat,
                chat_drafts::Contents {
                    text: "newer owner".into(),
                    attachments: Vec::new(),
                    references: Vec::new(),
                },
                Some((u64::MAX, 1)),
            )
            .unwrap();
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        cx.read(|cx| chat_drafts::draft(&chat, cx)),
        Some("newer owner".into())
    );
    assert!(
        tabs[0]
            .0
            .read_with(cx, |view, _| view.attachments[0].attachment().is_none())
    );
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(format!("attachment-card-{}", attachment.id), cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        cx.read(|cx| chat_drafts::draft(&chat, cx)),
        Some("newer owner".into()),
        "Check again must not replace another window's newer draft"
    );
    assert!(tabs[0].1.try_recv().is_err());
    close(cx, handle);
    let _ = std::fs::remove_dir_all(home);
}

#[gpui::test]
fn corrupt_recovered_snapshot_stays_unavailable_and_preserves_metadata_for_review(
    cx: &mut TestAppContext,
) {
    let home = home();
    let chat = chat_id();
    let attachment = saved_attachment(&home, &chat);
    std::fs::write(&attachment.path, "corrupt").unwrap();
    start(cx, &home);
    let (handle, _, tabs) = mount(cx, &[&chat]);
    cx.update_window(handle, |_, window, cx| {
        tabs[0].0.update(cx, |view, cx| {
            view.submissions.push(attachment_draft::Submission::recover(
                1,
                chat_drafts::SavedSubmission {
                    key: chat_id(),
                    status: chat_drafts::SubmissionStatus::Uncertain("unknown delivery".into()),
                    snapshot: chat_drafts::Contents {
                        text: "exact saved text".into(),
                        attachments: vec![attachment.clone()],
                        references: Vec::new(),
                    },
                },
            ));
            view.restore_snapshot(1, window, cx);
            assert!(matches!(
                view.attachments[0].state,
                attachment_ui::Stage::Checking { .. }
            ));
            view.send_message(window, cx);
            assert!(view.pending_submission.is_none());
        });
    })
    .unwrap();
    cx.run_until_parked();
    tabs[0].0.read_with(cx, |view, _| {
        assert!(matches!(
            view.attachments[0].state,
            attachment_ui::Stage::Unavailable { .. }
        ));
        assert!(view.attachments[0].attachment().is_none());
        assert_eq!(
            view.attachments[0].recoverable_attachment(),
            Some(&attachment)
        );
    });
    assert_eq!(text(cx, &tabs[0].0), "exact saved text");
    assert!(tabs[0].1.try_recv().is_err());
    cx.read(|cx| {
        let (record, _) = cx.global::<ChatDrafts>().0.recover(&chat).unwrap();
        assert_eq!(record.draft.attachments, vec![attachment.clone()]);
    });
    std::fs::write(&attachment.path, "saved attachment content").unwrap();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(format!("attachment-card-{}", attachment.id), cx);
    })
    .unwrap();
    cx.run_until_parked();
    tabs[0].0.read_with(cx, |view, _| {
        assert_eq!(view.attachments[0].attachment(), Some(&attachment));
        assert!(
            view.notices.local(notices::LocalKey::Attachment).is_none(),
            "a successful Check again clears only its own recovery warning"
        );
    });
    close(cx, handle);
    let _ = std::fs::remove_dir_all(home);
}

#[gpui::test]
fn closing_during_attachment_check_keeps_metadata_for_the_next_restore(cx: &mut TestAppContext) {
    let home = home();
    let chat = chat_id();
    let attachment = saved_attachment(&home, &chat);
    start(cx, &home);
    let (handle, pane, tabs) = mount(cx, &[&chat]);
    cx.update_window(handle, |_, window, cx| {
        cx.global_mut::<ChatDrafts>()
            .0
            .set_draft(
                &chat,
                chat_drafts::Contents {
                    text: "close during checking".into(),
                    attachments: vec![attachment.clone()],
                    references: Vec::new(),
                },
                None,
            )
            .unwrap();
        tabs[0].0.update(cx, |view, cx| {
            view.restore_draft(window, cx);
            assert!(matches!(
                view.attachments[0].state,
                attachment_ui::Stage::Checking { .. }
            ));
        });
        cx.global::<ChatDrafts>().0.flush();
        window.remove_window();
    })
    .unwrap();
    drop((pane, tabs));
    cx.run_until_parked();
    cx.update(|cx| cx.set_global(ChatDrafts(Drafts::load(&home, SystemTime::now()))));
    let (handle, _, tabs) = mount(cx, &[&chat]);
    assert_eq!(text(cx, &tabs[0].0), "close during checking");
    tabs[0].0.read_with(cx, |view, _| {
        assert_eq!(view.attachments[0].attachment(), Some(&attachment));
    });
    assert!(tabs[0].1.try_recv().is_err());
    close(cx, handle);
    let _ = std::fs::remove_dir_all(home);
}

#[gpui::test]
fn missing_recovery_store_never_marks_unchecked_attachment_ready(cx: &mut TestAppContext) {
    let home = home();
    let chat = chat_id();
    let attachment = saved_attachment(&home, &chat);
    let (handle, view, recording) = editor_tests::mount_selection(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        view.update(cx, |view, cx| {
            view.chat_id = Some(chat.clone());
            view.restore_attachment_chips(vec![attachment.clone()], window, cx);
            view.bump_generation();
            view.remember_draft(cx);
        });
    })
    .unwrap();
    cx.run_until_parked();
    view.read_with(cx, |view, _| {
        let attachment_ui::Stage::Unavailable { error, .. } = &view.attachments[0].state else {
            panic!("missing store must fail closed")
        };
        assert!(error.contains("recovery store is unavailable"));
        assert!(view.attachments[0].attachment().is_none());
    });
    assert!(recording.try_recv().is_err());
    close(cx, handle.into());
    let _ = std::fs::remove_dir_all(home);
}

#[gpui::test]
fn structured_references_survive_restart_and_a_receipt_keeps_newer_comments(
    cx: &mut TestAppContext,
) {
    use crate::chat_drafts::references::{
        DraftReference, ReferenceOrigin, ReferenceState, revision,
    };
    let home = home();
    let chat = chat_id();
    let attachment = saved_attachment(&home, &chat);
    start(cx, &home);
    let (handle, pane, tabs) = mount(cx, &[&chat]);
    let reference_id = uuid::Uuid::new_v4().to_string();
    let reference = DraftReference {
        id: reference_id.clone(),
        chat: Some(chat.clone()),
        origin: ReferenceOrigin::Message {
            item: "answer".into(),
            turn: Some("turn".into()),
            revision: revision("Original answer"),
        },
        original_origin: None,
        excerpt: "Original answer".into(),
        quote: "Original answer".into(),
        comment: String::new(),
        fragments: Vec::new(),
        state: ReferenceState::Current,
    };
    cx.update_window(handle, |_, window, cx| {
        tabs[0].0.update(cx, |view, cx| {
            view.composer.update(cx, |state, cx| {
                state.set_value("Existing draft", window, cx)
            });
            view.attachments
                .push(attachment_ui::Chip::ready(attachment.clone()));
            assert!(view.add_reference(reference.clone(), window, cx));
        })
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(format!("reference-comment:{reference_id}"), cx);
        window.input("Durable reply 🦀", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| cx.global::<ChatDrafts>().0.flush());
    drop((pane, tabs));
    close(cx, handle);
    cx.update(|cx| cx.set_global(ChatDrafts(Drafts::load(&home, SystemTime::now()))));
    let (handle, _, tabs) = mount(cx, &[&chat]);
    assert_eq!(text(cx, &tabs[0].0), "Existing draft");
    let recovered = tabs[0]
        .0
        .read_with(cx, |view, _| view.references.contents());
    assert_eq!(recovered[0].origin, reference.origin);
    assert_eq!(recovered[0].comment, "Durable reply 🦀");
    assert_eq!(recovered[0].state, ReferenceState::Unloaded);
    assert!(tabs[0].1.try_recv().is_err());
    cx.update_window(handle, |_, window, cx| {
        tabs[0]
            .0
            .update(cx, |view, cx| view.send_message(window, cx))
    })
    .unwrap();
    let feed::Delivery::Submission { id, command } = delivery(cx, &tabs[0].1) else {
        panic!("submission")
    };
    let ChatCommand::SendAttachments { text, attachments } = &command else {
        panic!("rich send")
    };
    assert_eq!(attachments, &vec![attachment]);
    assert!(text.contains("Durable reply 🦀"));
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(format!("reference-edit:{reference_id}"), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(format!("reference-comment:{reference_id}"), cx);
        window.press("cmd-a", cx);
        window.input("A newer comment", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        tabs[0].0.update(cx, |view, cx| {
            view.submission_receipt(id, command, Ok(()), window, cx)
        })
    })
    .unwrap();
    assert_eq!(
        tabs[0]
            .0
            .read_with(cx, |view, _| view.references.contents()[0].comment.clone()),
        "A newer comment"
    );
    cx.update(|cx| cx.global::<ChatDrafts>().0.flush());
    let restored = Drafts::load(&home, SystemTime::now())
        .recover(&chat)
        .unwrap()
        .0;
    assert_eq!(restored.draft.references[0].comment, "A newer comment");
    assert!(restored.submissions.is_empty());
    close(cx, handle);
    let _ = std::fs::remove_dir_all(home);
}

#[gpui::test]
#[ignore = "isolated 8 MiB recovery benchmark; run explicitly with --ignored --nocapture"]
fn attachment_recovery_ui_scheduling_benchmark(cx: &mut TestAppContext) {
    let home = home();
    let chat = chat_id();
    let directory = home.join("chats").join(&chat);
    std::fs::create_dir_all(&directory).unwrap();
    let source = home.join("large.txt");
    std::fs::write(&source, vec![b'a'; crate::chat::attachments::TEXT_BYTES]).unwrap();
    let attachments: Vec<_> = (0..crate::chat::attachments::SEND_COUNT)
        .map(|_| crate::chat::attachments::stage(&directory, &source).unwrap())
        .collect();
    let synchronous = std::time::Instant::now();
    for attachment in &attachments {
        chat_drafts::validate_attachment(&directory, attachment).unwrap();
    }
    let synchronous = synchronous.elapsed();
    start(cx, &home);
    let (handle, _, tabs) = mount(cx, &[&chat]);
    cx.update_window(handle, |_, window, cx| {
        cx.global_mut::<ChatDrafts>().0.set_draft(&chat, chat_drafts::Contents {
            text: "large recovered draft".into(), attachments,
        references: Vec::new(),
}, None).unwrap();
        let start = std::time::Instant::now();
        tabs[0].0.update(cx, |view, cx| {
            view.restore_draft(window, cx);
            assert!(view.attachments.iter().all(|chip| matches!(chip.state, attachment_ui::Stage::Checking { .. })));
        });
        eprintln!("8 attachments / 8 MiB: prior synchronous validation {synchronous:?}; UI restore and scheduling {:?}", start.elapsed());
    }).unwrap();
    cx.run_until_parked();
    assert!(tabs[0].0.read_with(cx, |view, _| {
        view.attachments
            .iter()
            .all(|chip| chip.attachment().is_some())
    }));
    close(cx, handle);
    let _ = std::fs::remove_dir_all(home);
}

#[gpui::test]
fn admission_refusal_in_stale_window_preserves_the_newer_windows_draft(cx: &mut TestAppContext) {
    let home = home();
    start(cx, &home);
    let chat = chat_id();
    let (first, _, first_tabs) = mount(cx, &[&chat]);
    let (second, _, _) = mount(cx, &[&chat]);
    type_into(cx, first, "stale first window");
    type_into(cx, second, "newer second window");
    cx.update(|cx| {
        let store = &mut cx.global_mut::<ChatDrafts>().0;
        for _ in 0..chat_drafts::MAX_SUBMISSIONS {
            store
                .begin_submission(
                    &chat,
                    (u64::MAX, 1),
                    chat_drafts::SavedSubmission {
                        key: chat_id(),
                        snapshot: chat_drafts::Contents {
                            text: "existing recovery".into(),
                            attachments: Vec::new(),
                            references: Vec::new(),
                        },
                        status: chat_drafts::SubmissionStatus::Pending,
                    },
                )
                .unwrap();
        }
    });
    cx.update_window(first, |_, window, cx| {
        first_tabs[0]
            .0
            .update(cx, |view, cx| view.send_message(window, cx));
    })
    .unwrap();
    cx.update(|cx| cx.global::<ChatDrafts>().0.flush());
    cx.run_until_parked();
    assert!(first_tabs[0].1.try_recv().is_err());
    assert_eq!(text(cx, &first_tabs[0].0), "stale first window");
    assert_eq!(
        cx.update(|cx| chat_drafts::draft(&chat, cx)).as_deref(),
        Some("newer second window")
    );
    assert_eq!(
        Drafts::load(&home, SystemTime::now()).get(&chat),
        Some("newer second window")
    );
    let _ = std::fs::remove_dir_all(home);
}

#[gpui::test]
fn dismissing_snapshot_before_durability_continuation_prevents_dispatch(cx: &mut TestAppContext) {
    let home = home();
    start(cx, &home);
    let chat = chat_id();
    let (handle, _, tabs) = mount(cx, &[&chat]);
    type_into(cx, handle, "not sent after dismissal");
    cx.update_window(handle, |_, window, cx| {
        tabs[0].0.update(cx, |view, cx| {
            view.send_message(window, cx);
            let key = view.submissions[0].recovery_key.clone();
            // Another window can dismiss the recovered card while this send awaits disk.
            chat_drafts::dismiss_submission(&chat, &key, cx);
        });
    })
    .unwrap();
    cx.update(|cx| cx.global::<ChatDrafts>().0.flush());
    cx.run_until_parked();
    assert!(tabs[0].1.try_recv().is_err());
    assert_eq!(text(cx, &tabs[0].0), "not sent after dismissal");
    tabs[0].0.read_with(cx, |view, _| {
        assert!(view.pending_submission.is_none());
        assert!(matches!(
            view.submissions[0].status,
            attachment_draft::Status::Refused(_)
        ));
    });
    cx.update(|cx| cx.global::<ChatDrafts>().0.flush());
    let _ = std::fs::remove_dir_all(home);
}

#[gpui::test]
fn disk_failure_keeps_editor_and_refuses_dispatch(cx: &mut TestAppContext) {
    let home = home();
    std::fs::write(home.join(chat_drafts::DIR_NAME), "blocks storage").unwrap();
    start(cx, &home);
    let chat = chat_id();
    let (handle, _, tabs) = mount(cx, &[&chat]);
    type_into(cx, handle, "not sent without recovery");
    cx.update_window(handle, |_, window, cx| {
        tabs[0]
            .0
            .update(cx, |view, cx| view.send_message(window, cx));
    })
    .unwrap();
    cx.update(|cx| cx.global::<ChatDrafts>().0.flush());
    cx.run_until_parked();
    assert!(tabs[0].1.try_recv().is_err());
    assert_eq!(text(cx, &tabs[0].0), "not sent without recovery");
    tabs[0].0.read_with(cx, |view, _| {
        assert!(view.pending_submission.is_none());
        assert!(matches!(
            view.submissions[0].status,
            attachment_draft::Status::Refused(_)
        ));
    });
    std::fs::remove_file(home.join(chat_drafts::DIR_NAME)).unwrap();
    cx.update(|cx| cx.global::<ChatDrafts>().0.flush());
    let reloaded = Drafts::load(&home, SystemTime::now());
    assert_eq!(reloaded.get(&chat), Some("not sent without recovery"));
    assert!(matches!(
        reloaded.recover(&chat).unwrap().0.submissions[0].status,
        chat_drafts::SubmissionStatus::Refused(_)
    ));
    drop(reloaded);
    let _ = std::fs::remove_dir_all(home);
}
