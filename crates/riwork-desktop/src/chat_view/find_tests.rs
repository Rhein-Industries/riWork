//! Find in the chat, in a real ChatView window: ⌘F, typing, stepping, scrolling the current
//! match into view, closing, seeding from the selection, streaming content and one bar per
//! chat. Keys go through the same bindings `main` makes and Kit's own, by native dispatch;
//! frames that a reveal waits for are delivered with `simulate_next_frame`.
use super::*;
use crate::chat::model::{Item, ItemStatus, Presentation};
use gpui::{App, ClipboardItem, KeyBinding, TestAppContext, WindowHandle, point};
use gpui_kit::{base::Root, test::TestWindowExt};
use std::ops::Range;

actions!(find_tests, [WindowSearch, WindowGrok]);

fn message(id: &str, text: &str) -> Item {
    Item {
        id: id.into(),
        turn_id: Some(format!("fixture-turn:{id}")),
        status: ItemStatus::Completed,
        presentation: Presentation {
            phase: Some(crate::chat::model::MessagePhase::Final),
            ..Presentation::default()
        },
        body: ItemBody::AgentMessage { text: text.into() },
    }
}

/// What the window above a chat does with ⌘F and ⌘⇧G when the chat leaves them alone.
#[derive(Default)]
struct WindowKeys {
    search: usize,
    grok: usize,
}
impl gpui::Global for WindowKeys {}

/// The bindings `main` makes for find in a chat, after the window's own ⌘F and ⌘⇧G, which
/// have no context and so rank with the deepest one: in a field, Kit's `Input`.
fn bind(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.bind_keys([
            KeyBinding::new("cmd-f", WindowSearch, None),
            KeyBinding::new("cmd-shift-g", WindowGrok, None),
            KeyBinding::new("cmd-f", FindInChat, Some("ChatView")),
            KeyBinding::new("cmd-f", FindInChat, Some("ChatView > Input")),
            KeyBinding::new("cmd-g", FindNext, Some("ChatView")),
            KeyBinding::new("cmd-g", FindNext, Some("ChatView > Input")),
            KeyBinding::new("cmd-shift-g", FindPrevious, Some("ChatView")),
            KeyBinding::new("cmd-shift-g", FindPrevious, Some("ChatView > Input")),
        ]);
        cx.set_global(WindowKeys::default());
        cx.on_action(|_: &WindowSearch, cx| cx.global_mut::<WindowKeys>().search += 1);
        cx.on_action(|_: &WindowGrok, cx| cx.global_mut::<WindowKeys>().grok += 1);
    });
}

fn window_keys(cx: &mut TestAppContext) -> (usize, usize) {
    cx.read(|cx| {
        let keys = cx.global::<WindowKeys>();
        (keys.search, keys.grok)
    })
}

type Handle = WindowHandle<Root>;

/// A chat in a window with the transcript focused, showing `items`.
fn mount(cx: &mut TestAppContext, items: Vec<Item>) -> (Handle, Entity<ChatView>) {
    let (handle, view, _) = editor_tests::mount_selection(cx);
    bind(cx);
    install(cx, handle, &view, items);
    (handle, view)
}

fn install(cx: &mut TestAppContext, handle: Handle, view: &Entity<ChatView>, items: Vec<Item>) {
    act(cx, handle, |window, cx| {
        view.update(cx, |v, cx| {
            v.model.link = Link::Live;
            v.model.transcript.items = items;
            v.refresh_projection(cx);
            v.list.remeasure();
            cx.notify();
        });
        window.render_frame(cx);
    });
}

/// Run `f` in the window, then let its effects and the frames they ask for happen.
fn act(cx: &mut TestAppContext, handle: Handle, f: impl FnOnce(&mut Window, &mut App)) {
    cx.update_window(handle.into(), |_, window, cx| f(window, cx))
        .unwrap();
    cx.run_until_parked();
    settle(cx, handle);
}

/// Draw frames, delivering next-frame callbacks, until nothing more is asked for.
fn settle(cx: &mut TestAppContext, handle: Handle) {
    for _ in 0..60 {
        let ran = cx
            .update_window(handle.into(), |_, window, cx| {
                window.render_frame(cx);
                window.simulate_next_frame(cx)
            })
            .unwrap();
        cx.run_until_parked();
        if ran == 0 {
            break;
        }
    }
    cx.update_window(handle.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
}

fn read<R>(
    cx: &mut TestAppContext,
    handle: Handle,
    f: impl FnOnce(&mut Window, &mut App) -> R,
) -> R {
    cx.update_window(handle.into(), |_, window, cx| f(window, cx))
        .unwrap()
}

fn press(cx: &mut TestAppContext, handle: Handle, key: &str) {
    act(cx, handle, |window, cx| window.press(key, cx));
}

fn type_text(cx: &mut TestAppContext, handle: Handle, text: &str) {
    act(cx, handle, |window, cx| window.input(text, cx));
}

/// The current match's place (counted from one) and the number of matches.
fn status(cx: &mut TestAppContext, view: &Entity<ChatView>) -> (Option<usize>, usize) {
    cx.read(|cx| {
        let (current, count) = view.read(cx).find.status();
        (current.map(|at| at + 1), count)
    })
}

/// What the bar says beside its field.
fn count_label(cx: &mut TestAppContext, handle: Handle) -> String {
    read(cx, handle, |window, _| {
        window
            .find("chat-find-count")
            .label()
            .unwrap_or_default()
            .to_owned()
    })
}

fn query(cx: &mut TestAppContext, view: &Entity<ChatView>) -> String {
    cx.read(|cx| view.read(cx).find.input.read(cx).value().to_string())
}

fn field_focused(cx: &mut TestAppContext, handle: Handle, view: &Entity<ChatView>) -> bool {
    read(cx, handle, |window, cx| {
        view.read(cx).find.field_focused(window, cx)
    })
}

/// The leaf and text of the current match.
fn current_text(cx: &mut TestAppContext, view: &Entity<ChatView>) -> (String, String) {
    cx.read(|cx| {
        let view = view.read(cx);
        let hit = view.find.current_hit().expect("a current match");
        view.transcript_selection.with_source(|source| {
            let leaf = &source[hit.leaf];
            (leaf.key.clone(), leaf.text[hit.range].to_owned())
        })
    })
}

/// Whether the current match was drawn inside the list's viewport in the last frame.
fn current_in_view(cx: &mut TestAppContext, view: &Entity<ChatView>) -> bool {
    cx.read(|cx| {
        let view = view.read(cx);
        let hit = view.find.current_hit().expect("a current match");
        let viewport = view.list.viewport_bounds();
        view.transcript_selection
            .painted_rect(hit.leaf, hit.range)
            .is_some_and(|rect| rect.top() >= viewport.top() && rect.bottom() <= viewport.bottom())
    })
}

/// The matches the last frame painted: leaf key, bytes, current.
fn painted(cx: &mut TestAppContext, view: &Entity<ChatView>) -> Vec<(String, Range<usize>, bool)> {
    cx.read(|cx| view.read(cx).transcript_selection.painted_find())
}

/// The list's top row and how far into it the view is scrolled.
fn scroll_top(cx: &mut TestAppContext, view: &Entity<ChatView>) -> (usize, gpui::Pixels) {
    cx.read(|cx| {
        let top = view.read(cx).list.logical_scroll_top();
        (top.item_ix, top.offset_in_item)
    })
}

fn copied(cx: &mut TestAppContext, handle: Handle) -> String {
    act(cx, handle, |window, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string("unchanged sentinel".into()));
        window.press("cmd-c", cx);
    });
    cx.read(|cx| cx.read_from_clipboard().unwrap().text().unwrap().to_owned())
}

/// Drag across bytes `from..to` of the drawn leaf `key`.
fn select(
    cx: &mut TestAppContext,
    handle: Handle,
    view: &Entity<ChatView>,
    key: &str,
    from: usize,
    to: usize,
) {
    act(cx, handle, |window, cx| {
        window.render_frame(cx);
        let selection = &view.read(cx).transcript_selection;
        let start = selection.glyph_point(key, from).unwrap() + point(px(1.), px(0.));
        let end = selection.glyph_point(key, to).unwrap() + point(px(1.), px(0.));
        window.drag(start, end, cx);
    });
}

fn three_messages() -> Vec<Item> {
    vec![
        message("one", "Alphabet and beta"),
        message("two", "gamma ALPHA delta\n\n```\nlet alpha = 1;\n```"),
        message("three", "omega alpha"),
    ]
}

#[gpui::test]
fn cmd_f_opens_the_bar_focused_and_typing_counts_matches_in_reading_order(cx: &mut TestAppContext) {
    let (handle, view) = mount(cx, three_messages());
    press(cx, handle, "cmd-f");
    assert!(cx.read(|cx| view.read(cx).find.open));
    assert!(field_focused(cx, handle, &view));
    assert!(read(cx, handle, |window, _| window
        .try_find("chat-find")
        .is_some()));
    assert_eq!(count_label(cx, handle), "");
    assert_eq!(window_keys(cx), (0, 0), "the chat's ⌘F is not the window's");

    type_text(cx, handle, "alpha");
    // Messages, a code block and either case, as the transcript shows them.
    assert_eq!(status(cx, &view), (Some(1), 4));
    assert_eq!(count_label(cx, handle), "1 of 4");
    assert_eq!(
        current_text(cx, &view),
        ("one/0".to_owned(), "Alpha".to_owned())
    );
    let mut shown = painted(cx, &view);
    shown.sort_by(|a, b| (&a.0, a.1.start).cmp(&(&b.0, b.1.start)));
    assert_eq!(
        shown,
        vec![
            ("code:two/1".to_owned(), 4..9, false),
            ("one/0".to_owned(), 0..5, true),
            ("three/0".to_owned(), 6..11, false),
            ("two/0".to_owned(), 6..11, false),
        ]
    );
    // Live: another letter narrows the matches at once.
    type_text(cx, handle, "b");
    assert_eq!(status(cx, &view), (Some(1), 1));
    assert_eq!(count_label(cx, handle), "1 of 1");
    assert_eq!(
        current_text(cx, &view),
        ("one/0".to_owned(), "Alphab".to_owned())
    );
}

#[gpui::test]
fn cmd_f_from_the_message_box_opens_the_bar_and_cmd_f_again_selects_the_words(
    cx: &mut TestAppContext,
) {
    let (handle, view) = mount(cx, three_messages());
    act(cx, handle, |window, cx| {
        view.update(cx, |v, cx| v.focus(window, cx))
    });
    // Kit's own ⌘F in the message box is not taken, so the chat's binding gets it.
    press(cx, handle, "cmd-f");
    assert!(field_focused(cx, handle, &view));
    type_text(cx, handle, "beta");
    assert_eq!(status(cx, &view), (Some(1), 1));
    // Back in the message box with the bar open: ⌘F returns to the field with its words
    // selected, and keeps the match.
    act(cx, handle, |window, cx| {
        let composer = view.read(cx).composer.read(cx).focus_handle(cx);
        composer.focus(window, cx);
    });
    assert!(!field_focused(cx, handle, &view));
    press(cx, handle, "cmd-f");
    assert!(field_focused(cx, handle, &view));
    assert_eq!(
        cx.read(|cx| view.read(cx).find.input.read(cx).selected_range()),
        0..4
    );
    assert_eq!(status(cx, &view), (Some(1), 1));
    assert_eq!(window_keys(cx), (0, 0));
}

#[gpui::test]
fn cmd_f_outside_the_chat_is_the_windows_and_cmd_shift_g_is_too_while_the_bar_is_closed(
    cx: &mut TestAppContext,
) {
    let (handle, view) = mount(cx, three_messages());
    // Nothing of the chat has the keys (another pane, the tab strip): no ChatView context.
    act(cx, handle, |window, cx| window.blur(cx));
    press(cx, handle, "cmd-f");
    assert!(!cx.read(|cx| view.read(cx).find.open));
    assert_eq!(window_keys(cx), (1, 0));
    // In the chat with the bar closed, ⌘⇧G and ⌘G are not find's.
    act(cx, handle, |window, cx| {
        view.read(cx).focus.clone().focus(window, cx)
    });
    press(cx, handle, "cmd-shift-g");
    press(cx, handle, "cmd-g");
    assert_eq!(window_keys(cx), (1, 1));
    assert!(!cx.read(|cx| view.read(cx).find.open));
    // With it open, ⌘⇧G steps instead.
    press(cx, handle, "cmd-f");
    type_text(cx, handle, "alpha");
    press(cx, handle, "cmd-shift-g");
    assert_eq!(status(cx, &view), (Some(4), 4));
    assert_eq!(window_keys(cx), (1, 1));
}

#[gpui::test]
fn enter_shift_enter_cmd_g_and_the_arrows_step_round_the_matches(cx: &mut TestAppContext) {
    let (handle, view) = mount(cx, three_messages());
    press(cx, handle, "cmd-f");
    type_text(cx, handle, "alpha");
    assert_eq!(status(cx, &view), (Some(1), 4));
    for expected in [2, 3, 4, 1] {
        press(cx, handle, "enter");
        assert_eq!(status(cx, &view), (Some(expected), 4));
    }
    press(cx, handle, "shift-enter");
    assert_eq!(status(cx, &view), (Some(4), 4));
    assert_eq!(
        current_text(cx, &view),
        ("three/0".to_owned(), "alpha".to_owned())
    );
    press(cx, handle, "cmd-g");
    assert_eq!(status(cx, &view), (Some(1), 4));
    press(cx, handle, "cmd-shift-g");
    press(cx, handle, "cmd-shift-g");
    assert_eq!(status(cx, &view), (Some(3), 4));
    assert_eq!(count_label(cx, handle), "3 of 4");
    // The arrows step too, and leave the keys in the field for the next ⏎.
    act(cx, handle, |window, cx| window.click("chat-find-next", cx));
    assert_eq!(status(cx, &view), (Some(4), 4));
    act(cx, handle, |window, cx| {
        window.click("chat-find-previous", cx)
    });
    act(cx, handle, |window, cx| {
        window.click("chat-find-previous", cx)
    });
    assert_eq!(status(cx, &view), (Some(2), 4));
    assert!(field_focused(cx, handle, &view));
    press(cx, handle, "enter");
    assert_eq!(status(cx, &view), (Some(3), 4));
    // The painted current match follows.
    assert!(
        painted(cx, &view).contains(&("code:two/1".to_owned(), 4..9, true)),
        "{:?}",
        painted(cx, &view)
    );
}

#[gpui::test]
fn no_results_says_so_and_the_arrows_do_nothing(cx: &mut TestAppContext) {
    let (handle, view) = mount(cx, three_messages());
    press(cx, handle, "cmd-f");
    type_text(cx, handle, "zebra");
    assert_eq!(status(cx, &view), (None, 0));
    assert_eq!(count_label(cx, handle), "No results");
    assert!(painted(cx, &view).is_empty());
    press(cx, handle, "enter");
    press(cx, handle, "cmd-g");
    assert_eq!(status(cx, &view), (None, 0));
    // The draft is not searched, only what the transcript shows: closed and opened again,
    // the words are found again and there are still none.
    act(cx, handle, |window, cx| {
        view.update(cx, |v, cx| {
            v.composer.update(cx, |state, cx| {
                state.set_value("zebra in the draft", window, cx)
            })
        })
    });
    press(cx, handle, "escape");
    press(cx, handle, "cmd-f");
    assert_eq!(query(cx, &view), "zebra");
    assert_eq!(status(cx, &view), (None, 0));
    assert_eq!(count_label(cx, handle), "No results");
}

#[gpui::test]
fn escape_closes_clears_the_matches_and_gives_the_keys_back(cx: &mut TestAppContext) {
    let (handle, view) = mount(cx, three_messages());
    // From the message box: the keys go back to it.
    act(cx, handle, |window, cx| {
        view.update(cx, |v, cx| v.focus(window, cx))
    });
    press(cx, handle, "cmd-f");
    type_text(cx, handle, "alpha");
    assert_eq!(painted(cx, &view).len(), 4);
    press(cx, handle, "escape");
    assert!(!cx.read(|cx| view.read(cx).find.open));
    assert!(read(cx, handle, |window, _| window
        .try_find("chat-find")
        .is_none()));
    assert!(painted(cx, &view).is_empty());
    assert!(read(cx, handle, |window, cx| {
        view.read(cx)
            .composer
            .read(cx)
            .focus_handle(cx)
            .is_focused(window)
    }));
    // The message box's ⎋ is its own again once the bar is gone: nothing reopens.
    press(cx, handle, "escape");
    assert!(!cx.read(|cx| view.read(cx).find.open));

    // From the transcript, by the close button: the transcript has the keys again. The words
    // are kept for the next ⌘F, which finds them again.
    act(cx, handle, |window, cx| {
        view.read(cx).focus.clone().focus(window, cx)
    });
    press(cx, handle, "cmd-f");
    assert_eq!(query(cx, &view), "alpha");
    assert_eq!(status(cx, &view), (Some(1), 4));
    act(cx, handle, |window, cx| window.click("chat-find-close", cx));
    assert!(!cx.read(|cx| view.read(cx).find.open));
    assert!(painted(cx, &view).is_empty());
    assert!(read(cx, handle, |window, cx| view
        .read(cx)
        .focus
        .is_focused(window)));
}

#[gpui::test]
fn one_line_of_selected_transcript_text_becomes_the_words(cx: &mut TestAppContext) {
    let (handle, view) = mount(cx, three_messages());
    select(cx, handle, &view, "one/0", 13, 17);
    assert_eq!(copied(cx, handle), "beta");
    press(cx, handle, "cmd-f");
    assert_eq!(query(cx, &view), "beta");
    assert_eq!(
        cx.read(|cx| view.read(cx).find.input.read(cx).selected_range()),
        0..4
    );
    assert_eq!(status(cx, &view), (Some(1), 1));
    assert_eq!(
        current_text(cx, &view),
        ("one/0".to_owned(), "beta".to_owned())
    );

    // Selection and copy still work with matches painted, and a selection across lines does
    // not replace the words.
    act(cx, handle, |window, cx| {
        view.read(cx).focus.clone().focus(window, cx)
    });
    select(cx, handle, &view, "one/0", 0, 5);
    assert_eq!(copied(cx, handle), "Alpha");
    act(cx, handle, |window, cx| {
        window.render_frame(cx);
        let selection = &view.read(cx).transcript_selection;
        let (from, _) = selection.glyph_span_points("one/0");
        let (_, to) = selection.glyph_span_points("three/0");
        window.drag(from, to, cx);
    });
    press(cx, handle, "cmd-f");
    assert_eq!(query(cx, &view), "beta");
}

/// Forty long messages, one of which says `needle` in its last line.
fn long_chat(needle_at: usize) -> Vec<Item> {
    (0..40)
        .map(|row| {
            let mut text = format!("row{row:02} ");
            text.push_str(&"filler words ".repeat(120));
            if row == needle_at {
                text.push_str("\n\nthe needle is here");
            }
            message(&format!("long-{row}"), &text)
        })
        .collect()
}

fn scroll_to_top(cx: &mut TestAppContext, handle: Handle, view: &Entity<ChatView>) {
    act(cx, handle, |_, cx| {
        view.update(cx, |v, cx| {
            v.list.scroll_to(gpui::ListOffset {
                item_ix: 0,
                offset_in_item: px(0.),
            });
            cx.notify();
        })
    });
}

#[gpui::test]
fn a_match_in_a_row_the_list_has_not_drawn_is_scrolled_into_view(cx: &mut TestAppContext) {
    let (handle, view) = mount(cx, long_chat(31));
    scroll_to_top(cx, handle, &view);
    assert_eq!(scroll_top(cx, &view), (0, px(0.)));
    assert!(read(cx, handle, |window, _| window
        .try_find("transcript:long-31/1")
        .is_none()));
    press(cx, handle, "cmd-f");
    type_text(cx, handle, "needle");
    assert_eq!(status(cx, &view), (Some(1), 1));
    // Not drawn when found; drawn, in view, and painted as the current one once revealed.
    assert!(current_in_view(cx, &view));
    assert!(!cx.read(|cx| view.read(cx).find.revealing()));
    assert!(painted(cx, &view).contains(&("long-31/1".to_owned(), 4..10, true)));
    assert!(!cx.read(|cx| view.read(cx).list.is_following_tail()));
    // Back to the top with the bar open, ⏎ brings it into view again.
    scroll_to_top(cx, handle, &view);
    assert!(!current_in_view(cx, &view));
    press(cx, handle, "enter");
    assert!(current_in_view(cx, &view));
}

#[gpui::test]
fn a_match_already_in_view_does_not_scroll_and_a_new_query_starts_from_the_view(
    cx: &mut TestAppContext,
) {
    let (handle, view) = mount(cx, long_chat(31));
    act(cx, handle, |_, cx| {
        view.update(cx, |v, cx| {
            v.list.scroll_to(gpui::ListOffset {
                item_ix: 20,
                offset_in_item: px(0.),
            });
            cx.notify();
        })
    });
    let before = scroll_top(cx, &view);
    press(cx, handle, "cmd-f");
    // Every row says "rowNN"; the first at or below the top of the view is row 20's.
    type_text(cx, handle, "row");
    assert_eq!(status(cx, &view), (Some(21), 40));
    assert_eq!(
        current_text(cx, &view),
        ("long-20/0".to_owned(), "row".to_owned())
    );
    assert_eq!(
        scroll_top(cx, &view),
        before,
        "a match in view does not move the view"
    );
}

#[gpui::test]
fn a_match_far_along_a_code_line_scrolls_its_box(cx: &mut TestAppContext) {
    let line = format!("{}needle()", "x".repeat(600));
    let (handle, view) = mount(cx, vec![message("code", &format!("```\n{line}\n```"))]);
    press(cx, handle, "cmd-f");
    type_text(cx, handle, "needle");
    assert_eq!(status(cx, &view), (Some(1), 1));
    let inside = cx.read(|cx| {
        let view = view.read(cx);
        let hit = view.find.current_hit().unwrap();
        let rect = view
            .transcript_selection
            .painted_rect(hit.leaf, hit.range)
            .unwrap();
        let scroller = view.caches.borrow().scrollers["code:code/0"].clone();
        let bounds = scroller.handle().bounds();
        (scroller.handle().offset().x < px(0.))
            && rect.left() >= bounds.left()
            && rect.right() <= bounds.right()
    });
    assert!(inside, "the code box scrolled sideways to the match");
}

#[gpui::test]
fn a_match_in_a_deep_rich_tool_result_scrolls_its_inner_box(cx: &mut TestAppContext) {
    use crate::chat::tool_output::{FileHit, ToolOutput};
    let mut item = message("result:part", "");
    item.presentation.phase = None;
    item.body = ItemBody::ToolCall {
        server: None,
        tool: "Grep".into(),
        input: serde_json::json!({"pattern":"value"}),
        output: None,
    };
    item.presentation.tool = Some(ToolOutput::FileSearch {
        query: "value".into(),
        hits: (0..30)
            .map(|at| FileHit {
                path: format!("src/file-{at}.rs"),
                line: Some(at + 1),
                preview: if at == 29 {
                    "needle".into()
                } else {
                    "matched value".into()
                },
            })
            .collect(),
        omitted: 0,
    });
    let (handle, view) = mount(cx, vec![item]);
    act(cx, handle, |_, cx| {
        view.update(cx, |view, cx| {
            view.display_mode = DisplayMode::Verbose;
            view.open.insert("result:part".into());
            view.refresh_projection(cx);
            cx.notify();
        });
    });
    press(cx, handle, "cmd-f");
    type_text(cx, handle, "needle");
    assert_eq!(status(cx, &view), (Some(1), 1));
    let inside = cx.read(|cx| {
        let view = view.read(cx);
        let hit = view.find.current_hit().unwrap();
        let rect = view
            .transcript_selection
            .painted_rect(hit.leaf, hit.range)
            .unwrap();
        let scroller = view.caches.borrow().scrollers["tool-rich:result:part"].clone();
        let bounds = scroller.handle().bounds();
        scroller.handle().offset().y < px(0.)
            && rect.top() >= bounds.top()
            && rect.bottom() <= bounds.bottom()
    });
    assert!(inside, "the result list scrolled down to the matched row");
    assert!(current_in_view(cx, &view));
}

#[gpui::test]
fn a_match_in_a_wide_table_scrolls_to_its_column(cx: &mut TestAppContext) {
    let header = (0..8)
        .map(|at| format!("Column {at}"))
        .collect::<Vec<_>>()
        .join(" | ");
    let cells = (0..8)
        .map(|at| {
            if at == 7 {
                "needle".into()
            } else {
                "long descriptive cell contents".into()
            }
        })
        .collect::<Vec<String>>()
        .join(" | ");
    let source = format!(
        "| {header} |\n| {} |\n| {cells} |",
        vec!["---"; 8].join(" | ")
    );
    let (handle, view) = mount(cx, vec![message("table", &source)]);
    press(cx, handle, "cmd-f");
    type_text(cx, handle, "needle");
    assert_eq!(status(cx, &view), (Some(1), 1));
    let inside = cx.read(|cx| {
        let view = view.read(cx);
        let hit = view.find.current_hit().unwrap();
        let rect = view
            .transcript_selection
            .painted_rect(hit.leaf, hit.range)
            .unwrap();
        let scroller = view.caches.borrow().scrollers["table:table/0"].clone();
        let bounds = scroller.handle().bounds();
        scroller.handle().offset().x < px(0.)
            && rect.left() >= bounds.left()
            && rect.right() <= bounds.right()
    });
    assert!(inside, "the table scrolled sideways to the matched cell");
}

#[gpui::test]
fn streaming_content_updates_the_matches_without_moving_the_view(cx: &mut TestAppContext) {
    let (handle, view) = mount(cx, long_chat(31));
    scroll_to_top(cx, handle, &view);
    press(cx, handle, "cmd-f");
    type_text(cx, handle, "row0");
    // Rows 00 to 09; step to row 03's.
    assert_eq!(status(cx, &view), (Some(1), 10));
    for _ in 0..3 {
        press(cx, handle, "enter");
    }
    assert_eq!(current_text(cx, &view).0, "long-3/0");
    let before = scroll_top(cx, &view);
    // A turn streams: a new message arrives, and one above the view changes.
    assert!(before.0 > 0, "{before:?}");
    act(cx, handle, |window, cx| {
        view.update(cx, |v, cx| {
            let items = &mut v.model.transcript.items;
            items.push(message("late", "row0 again"));
            items[0].body = ItemBody::AgentMessage {
                text: "row00 then row0 twice".into(),
            };
            v.sync_list(
                &Applied {
                    appended: 1,
                    touched: [0].into_iter().collect(),
                    projection_changed: true,
                    ..Default::default()
                },
                cx,
            );
            cx.notify();
        });
        window.render_frame(cx);
    });
    // Two more matches; the current one is still row 03's, now fifth; nothing scrolled.
    assert_eq!(status(cx, &view), (Some(5), 12));
    assert_eq!(current_text(cx, &view).0, "long-3/0");
    assert_eq!(count_label(cx, handle), "5 of 12");
    assert_eq!(scroll_top(cx, &view), before);
    assert!(!cx.read(|cx| view.read(cx).find.revealing()));
}

/// Two chat tabs in one window, one shown at a time.
struct Tabs {
    chats: [Entity<ChatView>; 2],
    shown: usize,
}
impl Render for Tabs {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.chats[self.shown].clone())
    }
}

#[gpui::test]
fn each_chat_keeps_its_own_bar_and_matches(cx: &mut TestAppContext) {
    bind(cx);
    let (handle, tabs) = cx.update(|cx| {
        cx.set_global(crate::settings::Settings::default());
        cx.set_global(crate::theme::Appearance {
            selected: crate::theme::ThemeChoice::RiWork,
            palette: crate::theme::Palette::RIWORK,
            terminal: None,
            ghostty: None,
            error: None,
        });
        text_input::init(cx);
        let mut tabs = None;
        let handle = cx
            .open_window(Default::default(), |window, cx| {
                let chat = |id: &str, window: &mut Window, cx: &mut App| {
                    let (feed, _) = Feed::recording();
                    let id = id.to_owned();
                    cx.new(|cx| {
                        let mut view = ChatView::blank(
                            HostConfig {
                                ensure: Arc::new(|| Err("fixture staging is disabled".into())),
                            },
                            window,
                            cx,
                        );
                        view.chat_id = Some(id);
                        view.feed = Some(feed);
                        view
                    })
                };
                let chats = [chat("a", window, cx), chat("b", window, cx)];
                let view = cx.new(|_| Tabs { chats, shown: 0 });
                tabs = Some(view.clone());
                cx.new(|cx| Root::new(view, window, cx))
            })
            .unwrap();
        (handle, tabs.unwrap())
    });
    let [a, b] = cx.read(|cx| tabs.read(cx).chats.clone());
    install(cx, handle, &a, vec![message("a1", "shared word in a")]);
    act(cx, handle, |_, cx| {
        tabs.update(cx, |t, cx| {
            t.shown = 1;
            cx.notify()
        })
    });
    install(
        cx,
        handle,
        &b,
        vec![message("b1", "shared word, shared again in b")],
    );
    act(cx, handle, |_, cx| {
        tabs.update(cx, |t, cx| {
            t.shown = 0;
            cx.notify()
        })
    });

    act(cx, handle, |window, cx| {
        a.read(cx).focus.clone().focus(window, cx)
    });
    press(cx, handle, "cmd-f");
    type_text(cx, handle, "shared");
    assert_eq!(status(cx, &a), (Some(1), 1));

    // The other tab: no bar, no words, no matches of the first chat.
    act(cx, handle, |window, cx| {
        tabs.update(cx, |t, cx| {
            t.shown = 1;
            cx.notify()
        });
        b.read(cx).focus.clone().focus(window, cx);
    });
    assert!(!cx.read(|cx| b.read(cx).find.open));
    assert!(read(cx, handle, |window, _| window
        .try_find("chat-find")
        .is_none()));
    assert!(painted(cx, &b).is_empty());
    press(cx, handle, "cmd-f");
    assert_eq!(query(cx, &b), "");
    type_text(cx, handle, "again");
    assert_eq!(status(cx, &b), (Some(1), 1));
    press(cx, handle, "escape");
    assert!(!cx.read(|cx| b.read(cx).find.open));

    // Back: the first chat's bar is as it was left.
    act(cx, handle, |_, cx| {
        tabs.update(cx, |t, cx| {
            t.shown = 0;
            cx.notify()
        })
    });
    assert!(cx.read(|cx| a.read(cx).find.open));
    assert_eq!(query(cx, &a), "shared");
    assert_eq!(status(cx, &a), (Some(1), 1));
    assert_eq!(painted(cx, &a), vec![("a1/0".to_owned(), 0..6, true)]);
}

#[gpui::test]
fn reopening_judges_the_view_the_bar_leaves_the_transcript(cx: &mut TestAppContext) {
    let (handle, view) = mount(cx, long_chat(31));
    press(cx, handle, "cmd-f");
    type_text(cx, handle, "needle");
    press(cx, handle, "escape");
    // With the bar closed, the match sits just above the bottom of the view: in the strip
    // the bar takes from the list when it opens again.
    act(cx, handle, |_, cx| {
        view.update(cx, |v, cx| {
            let at = v.transcript_selection.glyph_point("long-31/1", 4).unwrap();
            let bottom = v.list.viewport_bounds().bottom();
            v.list.pause_following_tail();
            v.list.scroll_by(at.y - (bottom - px(16.)));
            cx.notify();
        })
    });
    let shown = cx.read(|cx| {
        let v = view.read(cx);
        let at = v.transcript_selection.glyph_point("long-31/1", 4).unwrap();
        let viewport = v.list.viewport_bounds();
        viewport.contains(&at) && at.y > viewport.bottom() - px(30.)
    });
    assert!(shown, "the match is in view, near its bottom edge");
    press(cx, handle, "cmd-f");
    assert_eq!(query(cx, &view), "needle");
    assert_eq!(status(cx, &view), (Some(1), 1));
    assert!(current_in_view(cx, &view));
}

#[gpui::test]
fn a_match_wider_than_its_code_box_shows_its_start_and_settles(cx: &mut TestAppContext) {
    let (handle, view, _) = editor_tests::mount_sized(
        cx,
        HostConfig {
            ensure: Arc::new(|| Err("fixture staging is disabled".into())),
        },
        320.,
    );
    bind(cx);
    let word = format!("needle{}", "q".repeat(60));
    let line = format!("{} {word} tail", "y".repeat(200));
    install(
        cx,
        handle,
        &view,
        vec![message("code", &format!("```\n{line}\n```"))],
    );
    act(cx, handle, |window, cx| {
        view.read(cx).focus.clone().focus(window, cx)
    });
    press(cx, handle, "cmd-f");
    type_text(cx, handle, &word);
    assert_eq!(status(cx, &view), (Some(1), 1));
    let place = |cx: &mut TestAppContext| {
        cx.read(|cx| {
            let view = view.read(cx);
            let hit = view.find.current_hit().unwrap();
            let rect = view
                .transcript_selection
                .painted_rect(hit.leaf, hit.range)
                .unwrap();
            let scroller = view.caches.borrow().scrollers["code:code/0"].clone();
            let bounds = scroller.handle().bounds();
            assert!(
                rect.size.width > bounds.size.width,
                "the match is wider than its box"
            );
            assert!(!view.find.revealing());
            (
                rect.left() >= bounds.left() && rect.left() < bounds.right(),
                scroller.handle().offset(),
            )
        })
    };
    let (start_shown, offset) = place(cx);
    assert!(start_shown, "the box shows where the match starts");
    assert!(offset.x < px(0.));
    // Nothing moves once it has settled.
    press(cx, handle, "cmd-g");
    assert_eq!(place(cx), (true, offset));
}

#[gpui::test]
fn escape_closing_a_menu_from_the_find_field_leaves_the_keys_there(cx: &mut TestAppContext) {
    let (handle, view) = mount(cx, three_messages());
    press(cx, handle, "cmd-f");
    type_text(cx, handle, "alpha");
    act(cx, handle, |_, cx| {
        view.update(cx, |v, cx| {
            v.open.insert(panels::ATTACHMENT_MENU_KEY.to_owned());
            cx.notify();
        })
    });
    // The first ⎋ closes the menu; the keys stay in the field, so the next closes the bar.
    press(cx, handle, "escape");
    assert!(!cx.read(|cx| view.read(cx).open.contains(panels::ATTACHMENT_MENU_KEY)));
    assert!(cx.read(|cx| view.read(cx).find.open));
    assert!(field_focused(cx, handle, &view));
    press(cx, handle, "escape");
    assert!(!cx.read(|cx| view.read(cx).find.open));
}

fn command(id: &str, turn: &str, output: &str) -> Item {
    Item {
        id: id.into(),
        turn_id: Some(turn.into()),
        status: ItemStatus::Completed,
        presentation: Presentation::default(),
        body: ItemBody::Command {
            command: "fixture command".into(),
            cwd: None,
            output: output.into(),
            exit_code: Some(0),
        },
    }
}

#[gpui::test]
fn hidden_activity_matches_open_only_the_selected_detail_and_keep_stable_identity(
    cx: &mut TestAppContext,
) {
    use crate::chat::model::TurnOutcome;
    let mut first = message("answer-one", "First answer");
    first.turn_id = Some("one".into());
    let mut second = message("answer-two", "Second answer");
    second.turn_id = Some("two".into());
    let third = message("answer-three", "Preserve this selection");
    let (handle, view) = mount(
        cx,
        vec![
            command("command-one", "one", "First needle 🦀"),
            command("irrelevant", "one", "Other output"),
            first,
            command("command-two", "two", "Second needle 🦀"),
            second,
            third,
        ],
    );
    act(cx, handle, |window, cx| {
        view.update(cx, |view, cx| {
            view.model.completed = vec![
                ("one".into(), TurnOutcome::Completed, 3),
                ("two".into(), TurnOutcome::Completed, 5),
                (
                    "fixture-turn:answer-three".into(),
                    TurnOutcome::Completed,
                    6,
                ),
            ];
            view.refresh_projection(cx);
            view.composer
                .update(cx, |input, cx| input.set_value("Keep my draft", window, cx));
            cx.notify();
        })
    });
    scroll_to_top(cx, handle, &view);
    assert!(cx.read(|cx| {
        view.read(cx)
            .transcript_selection
            .with_source(|source| !source.iter().any(|leaf| leaf.key.starts_with("output:")))
    }));
    press(cx, handle, "cmd-f");
    type_text(cx, handle, "needle");
    assert_eq!(status(cx, &view), (Some(1), 2));
    assert_eq!(
        current_text(cx, &view),
        ("output:command-one".into(), "needle".into())
    );
    assert!(current_in_view(cx, &view));
    cx.read(|cx| {
        let view = view.read(cx);
        assert!(view.open.contains("activity:one"));
        assert!(view.open.contains("command-one"));
        assert!(!view.open.contains("irrelevant"));
        assert!(!view.open.contains("activity:two"));
        assert!(!view.open.contains("command-two"));
        assert_eq!(view.composer_text(cx), "Keep my draft");
        assert!(
            !view
                .transcript_selection
                .with_source(|source| source.iter().any(|leaf| leaf.key == "output:command-two"))
        );
    });
    // Select a later answer before the next hidden group inserts rows above it.
    select(
        cx,
        handle,
        &view,
        "answer-three/0",
        0,
        "Preserve this selection".len(),
    );
    assert_eq!(copied(cx, handle), "Preserve this selection");
    press(cx, handle, "cmd-g");
    assert_eq!(status(cx, &view), (Some(2), 2));
    assert_eq!(
        current_text(cx, &view),
        ("output:command-two".into(), "needle".into())
    );
    assert_eq!(
        copied(cx, handle),
        "Preserve this selection",
        "revealing another row must preserve unchanged selected text"
    );
    assert!(cx.read(|cx| view.read(cx).open.contains("activity:two")));
    act(cx, handle, |_, cx| {
        view.update(cx, |view, cx| {
            view.apply_display_mode(DisplayMode::Verbose, cx)
        })
    });
    assert_eq!(status(cx, &view), (Some(2), 2));
    assert_eq!(current_text(cx, &view).0, "output:command-two");
    press(cx, handle, "cmd-shift-g");
    assert_eq!(current_text(cx, &view).0, "output:command-one");
    assert_eq!(
        cx.read(|cx| view.read(cx).composer_text(cx)),
        "Keep my draft"
    );
}

#[gpui::test]
fn find_opens_only_the_matching_nested_diff_and_preserves_preview_limits(cx: &mut TestAppContext) {
    use crate::chat::model::{ChangeKind, FileChange};
    let mut item = message("edits", "");
    item.presentation.phase = None;
    item.body = ItemBody::FileChange {
        changes: vec![
            FileChange {
                path: "first.txt".into(),
                kind: ChangeKind::Modify,
                diff: Some("@@ -1 +1 @@\n-before\n+needle café".into()),
            },
            FileChange {
                path: "other.txt".into(),
                kind: ChangeKind::Modify,
                diff: Some(format!("{}notretained", " ordinary\n".repeat(450))),
            },
        ],
    };
    let (handle, view) = mount(cx, vec![item]);
    act(cx, handle, |_, cx| {
        view.update(cx, |view, cx| {
            view.apply_display_mode(DisplayMode::Verbose, cx)
        })
    });
    press(cx, handle, "cmd-f");
    type_text(cx, handle, "needle");
    assert_eq!(status(cx, &view), (Some(1), 1));
    assert_eq!(
        current_text(cx, &view),
        ("diff:edits#0:2".into(), "needle".into())
    );
    cx.read(|cx| {
        let view = view.read(cx);
        assert!(view.open.contains("edits"));
        assert!(view.open.contains("edits#0"));
        assert!(!view.open.contains("edits#1"));
    });
    press(cx, handle, "cmd-a");
    type_text(cx, handle, "notretained");
    assert_eq!(status(cx, &view), (None, 0));
    assert!(!cx.read(|cx| view.read(cx).open.contains("edits#1")));
    assert_eq!(
        read(cx, handle, |window, _| window
            .find("chat-find-scope")
            .label()
            .unwrap()
            .to_owned()),
        "Search includes closed details. Command logs use the current page; tool output and diffs use retained previews."
    );
}

#[gpui::test]
fn find_expands_a_long_plan_before_mapping_its_hidden_match(cx: &mut TestAppContext) {
    let mut item = message("long-plan", "");
    item.presentation.phase = None;
    let source = format!(
        "# Plan\n\n{}\n\nFinal needle café",
        "A bounded preview line.\n".repeat(35)
    );
    item.body = ItemBody::Plan {
        explanation: Some(source),
        steps: Vec::new(),
    };
    let (handle, view) = mount(cx, vec![item]);
    act(cx, handle, |_, cx| {
        view.update(cx, |view, cx| {
            view.apply_display_mode(DisplayMode::Verbose, cx)
        })
    });
    assert!(!cx.read(|cx| {
        view.read(cx)
            .transcript_selection
            .with_source(|source| source.iter().any(|leaf| leaf.text.contains("needle")))
    }));
    press(cx, handle, "cmd-f");
    type_text(cx, handle, "needle");
    assert_eq!(status(cx, &view), (Some(1), 1));
    assert!(cx.read(|cx| {
        view.read(cx)
            .open
            .contains(&plan_document::expansion_key("long-plan"))
    }));
    assert_eq!(current_text(cx, &view).1, "needle");
    assert!(current_in_view(cx, &view));
}
