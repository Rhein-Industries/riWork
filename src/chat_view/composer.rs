//! The message box: what ⏎ and ⎋ do in it, moving between its lines, and the text it sends.

#[cfg(test)]
use crate::ui_text;
use crate::{
    chat::model::Decision,
    dictation::{self, Phase},
};

use super::approval;

/// What ⏎ does in the message box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Enter {
    /// Send the message (which steers a turn that is running).
    Send,
    NewLine,
    /// A request waits and the box is empty, so ⏎ answers the request.
    Approve(Decision),
    /// An input method is composing text: ⏎ belongs to it.
    Ignore,
}

/// ⏎ sends and ⇧⏎ (or ⌥⏎) starts a new line. While an approval waits and nothing is typed,
/// ⏎ and ⇧⏎ answer it instead: whatever has been typed makes ⏎ a message again, so a key
/// pressed for a message never allows a command.
pub fn enter(shift: bool, alt: bool, composing: bool, empty: bool, offered: &[Decision]) -> Enter {
    if composing {
        return Enter::Ignore;
    }
    if empty && let Some(decision) = approval::decision_for_key("enter", shift, offered) {
        return Enter::Approve(decision);
    }
    if shift || alt {
        Enter::NewLine
    } else {
        Enter::Send
    }
}

/// ⎋ denies the request that waits, when nothing is typed to be abandoned.
pub fn escape(empty: bool, offered: &[Decision]) -> Option<Decision> {
    if empty {
        approval::decision_for_key("escape", false, offered)
    } else {
        None
    }
}

/// The line under the message box while a dictation works: what it is doing. Nothing at
/// rest, and nothing while the mic is hidden (`mic`): the box carries no helper text, its keys
/// are in the buttons' tooltips.
pub fn status(dictation: &Phase, mic: bool) -> Option<String> {
    if !mic {
        return None;
    }
    let key = dictation::SHORTCUT_LABEL;
    match dictation {
        Phase::Preparing { note: Some(note) } => Some(note.clone()),
        Phase::Preparing { note: None } => Some("Getting the microphone ready…".to_owned()),
        Phase::Listening { .. } => Some(format!(
            "Listening, recognized on this Mac · {key} or the mic stops · ⎋ cancels"
        )),
        Phase::Finishing { .. } => Some("Finishing what was heard…".to_owned()),
        Phase::Idle | Phase::Failed(_) => None,
    }
}

/// The chat header's inset from the tab's sides and the gap between its controls, in design
/// points. The message box under the list keeps the same, so its edges line up with the
/// header's, until a narrow pane makes it give some of that up (see `layout`).
pub const BAR_INSET: f32 = 10.0;
pub const BAR_GAP: f32 = 6.0;
/// The message box bar's inset and gap in a pane too narrow for the usual ones, in pixels:
/// these do not grow with the text, so a big text size leaves the box its room.
pub const COMPACT_INSET: f32 = 4.0;
pub const COMPACT_GAP: f32 = 4.0;
/// The narrowest the message box may get, in design points.
pub const MIN_FIELD: f32 = 120.0;

/// The empty Hermes field shares a row with the controls only when there is room for a
/// readable prompt. Drafts always get the full width; narrow panes put controls below.
pub fn hermes_compact(pane: f32, scale: f32, empty: bool) -> bool {
    empty && pane >= (680.0 * scale.max(1.0)).round()
}

/// How the message box bar is laid out in a pane of a given width: in pixels, as drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    /// The box takes the whole row and the buttons a row under it.
    pub stacked: bool,
    /// Whether the mic is shown.
    pub mic: bool,
    /// The bar's padding on every side, and the gap between its pieces.
    pub inset: f32,
    pub gap: f32,
    /// Every button's side. Native and the colorful themes draw the same square buttons
    /// (a symbol or a one-character mark), so one side serves both.
    pub button: f32,
    /// The box's width.
    pub field: f32,
    /// The width the buttons take in their row (the box's row when not stacked), gaps
    /// included.
    pub buttons: f32,
}

/// The bar's layout for a pane `pane` px wide at interface scale `scale`, with the mic set to
/// show (`mic`) and kept whatever the room (`keep_mic`, while it dictates), and a turn
/// running (`running`, which adds Stop). Attach and Send are always there.
///
/// With the header's inset and gap, every button goes beside the box while the box keeps
/// `MIN_FIELD`; else the mic goes first; else the box takes the whole row and the buttons a
/// row under it, where the mic comes back if there is room. When even that row is short (or
/// the box is short of its minimum on a row of its own), the bar falls to `COMPACT_INSET` and
/// `COMPACT_GAP` and the buttons shrink to share the row, so the box keeps
/// `min(MIN_FIELD, pane - 2 × COMPACT_INSET)` and the buttons never pass the pane's edge.
/// Lengths in design points grow with the text as `ui_text::space` grows them. A pane not
/// yet laid out (zero wide) gets the full row.
pub fn layout(pane: f32, scale: f32, mic: bool, keep_mic: bool, running: bool) -> Layout {
    let space = |base: f32| (base * scale.max(1.0)).round();
    let (inset, gap, side, min) = (
        space(BAR_INSET),
        space(BAR_GAP),
        space(super::widgets::ROUND_BUTTON),
        space(MIN_FIELD),
    );
    let base = 2 + usize::from(running);
    let all = base + usize::from(mic);
    let needed = if keep_mic && mic { all } else { base };
    let span = |count: usize, side: f32, gap: f32| count as f32 * (side + gap);
    let row = pane - 2.0 * inset;
    let inline = |mic: bool| {
        let count = base + usize::from(mic);
        Layout {
            stacked: false,
            mic,
            inset,
            gap,
            button: side,
            field: row - span(count, side, gap),
            buttons: span(count, side, gap),
        }
    };
    if pane <= 0.0 || row >= min + span(all, side, gap) {
        return inline(mic);
    }
    if !(keep_mic && mic) && row >= min + span(base, side, gap) {
        return inline(false);
    }
    // Under the box: Attach, a space that takes what is left, then the rest; a gap before
    // each button but the first and one before the space, so one gap per button.
    let (inset, gap) = if row >= min && row >= span(needed, side, gap) {
        (inset, gap)
    } else {
        (COMPACT_INSET, COMPACT_GAP)
    };
    let row = pane - 2.0 * inset;
    let side = side.min(
        ((row - needed as f32 * gap) / needed as f32)
            .floor()
            .max(1.0),
    );
    let mic = mic && row >= span(all, side, gap);
    let count = base + usize::from(mic);
    Layout {
        stacked: true,
        mic,
        inset,
        gap,
        button: side,
        field: row,
        buttons: span(count, side, gap),
    }
}

/// The message to send for the box's text: without the blank lines around it, and nothing
/// for a box that holds only spaces.
pub fn message(text: &str) -> Option<String> {
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// The byte offset where the line holding `cursor` starts.
pub fn line_start(text: &str, cursor: usize) -> usize {
    text[..cursor].rfind('\n').map_or(0, |at| at + 1)
}

/// The byte offset where the line holding `cursor` ends, before its line break.
pub fn line_end(text: &str, cursor: usize) -> usize {
    text[cursor..]
        .find('\n')
        .map_or(text.len(), |at| cursor + at)
}

/// `cursor` one line up or down, in the same column where the line is long enough. Lines
/// are the ones typed; a long line that wraps on screen is one line here. `None` at the
/// first or last line.
pub fn vertical(text: &str, cursor: usize, up: bool) -> Option<usize> {
    let start = line_start(text, cursor);
    let column = text[start..cursor].chars().count();
    let target = if up {
        if start == 0 {
            return None;
        }
        line_start(text, start - 1)
    } else {
        let end = line_end(text, cursor);
        if end == text.len() {
            return None;
        }
        end + 1
    };
    let end = line_end(text, target);
    let offset = text[target..end]
        .char_indices()
        .nth(column)
        .map_or(end, |(at, _)| target + at);
    Some(offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ASKED: [Decision; 3] = [
        Decision::Accept,
        Decision::AcceptForSession,
        Decision::Decline,
    ];

    #[test]
    fn return_sends_and_shift_return_starts_a_line() {
        assert_eq!(enter(false, false, false, false, &[]), Enter::Send);
        assert_eq!(enter(true, false, false, false, &[]), Enter::NewLine);
        assert_eq!(enter(false, true, false, false, &[]), Enter::NewLine);
        // Even an empty box sends (the caller drops an empty message), and ⇧⏎ still adds a line.
        assert_eq!(enter(false, false, false, true, &[]), Enter::Send);
        assert_eq!(enter(true, false, false, true, &[]), Enter::NewLine);
    }

    #[test]
    fn an_input_method_keeps_its_return() {
        assert_eq!(enter(false, false, true, false, &ASKED), Enter::Ignore);
        assert_eq!(enter(false, false, true, true, &ASKED), Enter::Ignore);
    }

    #[test]
    fn with_a_request_waiting_and_nothing_typed_return_answers_it() {
        assert_eq!(
            enter(false, false, false, true, &ASKED),
            Enter::Approve(Decision::Accept)
        );
        assert_eq!(
            enter(true, false, false, true, &ASKED),
            Enter::Approve(Decision::AcceptForSession)
        );
        assert_eq!(escape(true, &ASKED), Some(Decision::Decline));
        // Typed text is a message first.
        assert_eq!(enter(false, false, false, false, &ASKED), Enter::Send);
        assert_eq!(enter(true, false, false, false, &ASKED), Enter::NewLine);
        assert_eq!(escape(false, &ASKED), None);
        // A choice the request does not offer is not made.
        assert_eq!(
            enter(true, false, false, true, &[Decision::Accept]),
            Enter::NewLine
        );
        assert_eq!(escape(true, &[]), None);
    }

    #[test]
    fn the_box_has_no_helper_text_at_rest_and_says_what_a_dictation_does() {
        assert_eq!(status(&Phase::Idle, true), None);
        assert_eq!(status(&Phase::Idle, false), None);
        assert_eq!(
            status(&Phase::Failed(dictation::Problem::Unsupported), true),
            None
        );
        let listening = Phase::Listening { text: "hi".into() };
        assert!(status(&listening, true).unwrap().starts_with("Listening"));
        // Hidden mid-dictation (before the dictation is cancelled), nothing.
        assert_eq!(status(&listening, false), None);
        let preparing = Phase::Preparing { note: None };
        assert_eq!(
            status(&preparing, true).as_deref(),
            Some("Getting the microphone ready…")
        );
        assert_eq!(status(&preparing, false), None);
    }

    #[test]
    fn the_box_keeps_its_minimum_and_the_buttons_stay_in_the_pane() {
        let at = |pane, mic, running| layout(pane, 1.0, mic, false, running);
        // At 1.0: 10 inset, 6 gap, 26 buttons (the side every composer button is drawn at),
        // 120 box.
        assert_eq!(
            at(500.0, true, true).button,
            super::super::widgets::ROUND_BUTTON
        );
        let wide = at(500.0, true, true);
        assert!(!wide.stacked && wide.mic && wide.field == 480.0 - 4.0 * 32.0);
        assert!(at(268.0, true, true).mic);
        let short = at(267.0, true, true);
        assert!(!short.stacked && !short.mic && short.field >= 120.0);
        assert!(at(235.0, true, true).stacked);
        // Not laid out yet: the full row.
        assert!(!at(0.0, true, true).stacked && at(0.0, true, true).mic);
        // A dictating mic is never dropped: under the box instead.
        let dictating = layout(267.0, 1.0, true, true, true);
        assert!(dictating.stacked && dictating.mic);
        for scale in [0.8, 1.0, 1.5, 24.0 / ui_text::REFERENCE_SIZE] {
            for pane in [120.0, 160.0, 200.0, 240.0, 300.0, 368.0, 500.0, 900.0] {
                for (mic, running) in [(true, true), (true, false), (false, true), (false, false)] {
                    let fit = layout(pane, scale, mic, false, running);
                    let min = (MIN_FIELD * f32::max(scale, 1.0)).round();
                    let floor = min.min(pane - 2.0 * COMPACT_INSET);
                    let what = format!("{pane} px at {scale}× mic {mic} running {running}");
                    assert!(fit.field >= floor, "box {} < {floor}: {what}", fit.field);
                    let row = pane - 2.0 * fit.inset;
                    if fit.stacked {
                        assert!(
                            fit.buttons <= row,
                            "buttons {} > {row}: {what}",
                            fit.buttons
                        );
                        assert_eq!(fit.field, row);
                    } else {
                        assert_eq!(fit.field + fit.buttons, row, "{what}");
                    }
                    assert!(!fit.mic || mic, "{what}");
                }
            }
        }
        // 24 pt text in the narrowest pane: compact, the box keeps 120.
        let big = layout(160.0, 24.0 / ui_text::REFERENCE_SIZE, true, false, true);
        assert!(big.stacked && big.inset == COMPACT_INSET && big.field >= 120.0);
    }

    #[test]
    fn a_message_is_the_trimmed_text_or_nothing() {
        assert_eq!(message("  hello\n\n"), Some("hello".to_owned()));
        assert_eq!(message("a\n\nb"), Some("a\n\nb".to_owned()));
        assert_eq!(message(" \n\t "), None);
        assert_eq!(message(""), None);
    }

    #[test]
    fn lines_are_found_around_the_cursor() {
        let text = "one\ntwo\n\nfour";
        assert_eq!((line_start(text, 0), line_end(text, 0)), (0, 3));
        assert_eq!((line_start(text, 5), line_end(text, 5)), (4, 7));
        assert_eq!((line_start(text, 8), line_end(text, 8)), (8, 8));
        assert_eq!((line_start(text, 13), line_end(text, 13)), (9, 13));
        assert_eq!((line_start("", 0), line_end("", 0)), (0, 0));
    }

    #[test]
    fn up_and_down_keep_the_column_where_the_line_is_long_enough() {
        let text = "hello\nhi\n\nworld!";
        let end = text.len();
        // From the end of "world!" up to the empty line, and from there to the start of "hi".
        assert_eq!(vertical(text, end, true), Some(9));
        assert_eq!(vertical(text, 9, true), Some(6));
        assert_eq!(vertical(text, 9, false), Some(10));
        // Column 4 of "hello" has no match in the two-letter "hi": its end.
        assert_eq!(vertical(text, 4, false), Some(8));
        // Column 2 of "hi" is column 2 of "hello".
        assert_eq!(vertical(text, 8, true), Some(2));
        assert_eq!(vertical(text, 2, true), None, "the first line");
        assert_eq!(vertical(text, end, false), None, "the last line");
        assert_eq!(vertical("", 0, true), None);
    }

    #[test]
    fn columns_count_characters_not_bytes() {
        let text = "héé\nabcd";
        // Column 3 of the first line is its end; below it is column 3 of "abcd".
        assert_eq!(vertical(text, "héé".len(), false), Some("héé\nabc".len()));
        assert_eq!(vertical(text, "héé\nabc".len(), true), Some("héé".len()));
    }
}
