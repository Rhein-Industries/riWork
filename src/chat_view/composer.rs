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
/// points.
pub const BAR_INSET: f32 = 10.0;
pub const BAR_GAP: f32 = 6.0;
/// The message box card's inset from the pane's sides and bottom, its padding and the gap
/// between its pieces, in design points. The bars above it (a notice, a request, a question)
/// keep the same inset, so the bottom of the chat reads as one column of cards.
pub const CARD_INSET: f32 = 14.0;
pub const CARD_PADDING: f32 = 7.0;
pub const CARD_GAP: f32 = 4.0;
/// Inset, padding and gap in a pane narrower than `NARROW_PANE`, in pixels: these do not
/// grow with the text, so a big text size leaves the box its room.
pub const NARROW_SPACE: f32 = 4.0;
/// Below this width (in design points) the card gives up its usual insets and its controls
/// take a row of their own.
pub const NARROW_PANE: f32 = 360.0;
/// From this width (in design points) an empty box shares one row with its controls.
pub const COMPACT_PANE: f32 = 680.0;

/// How the message box card is laid out in a pane of a given width: in pixels, as drawn. The
/// same in every design; a design only chooses how the card and its buttons look.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    /// The empty box shares one row with Attach and the controls. Otherwise the box takes the
    /// card's whole width and Attach and the controls wrap in a row under it.
    pub compact: bool,
    /// A pane too narrow for the usual insets: the controls take a row of their own.
    pub narrow: bool,
    /// The card's inset from the pane (also the bars' above it), its padding, and the gap
    /// between its pieces.
    pub inset: f32,
    pub padding: f32,
    pub gap: f32,
    /// Every button's side: the round buttons of the action row (the mic, Stop, Send) share
    /// the room the card leaves them and never grow past `ROUND_BUTTON`.
    pub button: f32,
}

/// The card's layout for a pane `pane` px wide at interface scale `scale`, with an `empty`
/// draft (no text, no attachment) and `actions` round buttons in its action row (Send, and
/// the mic and Stop when shown). A pane not yet laid out (zero wide) gets the usual insets
/// and full-size buttons.
pub fn layout(pane: f32, scale: f32, empty: bool, actions: usize) -> Layout {
    let space = |base: f32| (base * scale.max(1.0)).round();
    let narrow = pane > 0.0 && pane < space(NARROW_PANE);
    let (inset, padding, gap) = if narrow {
        (NARROW_SPACE, NARROW_SPACE, NARROW_SPACE)
    } else {
        (space(CARD_INSET), space(CARD_PADDING), space(CARD_GAP))
    };
    let actions = actions.max(1);
    let available = if pane <= 0.0 {
        space(COMPACT_PANE)
    } else {
        // The card's border takes a pixel on each side.
        (pane - 2.0 * (inset + padding) - 2.0).max(1.0)
    };
    let button = space(super::widgets::ROUND_BUTTON)
        .min(((available - gap * (actions - 1) as f32) / actions as f32).max(1.0));
    Layout {
        compact: empty && pane >= space(COMPACT_PANE),
        narrow,
        inset,
        padding,
        gap,
        button,
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
    fn the_card_keeps_its_insets_and_its_buttons_share_what_the_pane_leaves() {
        let round = super::super::widgets::ROUND_BUTTON;
        // At 1.0 in a wide pane: 14 inset, 7 padding, 4 gap and full-size buttons; an empty
        // box shares the controls' row from 680 px, a draft never does.
        let wide = layout(900.0, 1.0, true, 3);
        assert_eq!(
            (wide.inset, wide.padding, wide.gap, wide.button),
            (14.0, 7.0, 4.0, round)
        );
        assert!(wide.compact && !wide.narrow);
        assert!(!layout(900.0, 1.0, false, 3).compact);
        assert!(layout(680.0, 1.0, true, 1).compact);
        assert!(!layout(679.0, 1.0, true, 1).compact);
        // Under 360 px the card gives up its insets for 4 px ones that do not grow.
        let narrow = layout(359.0, 1.0, true, 3);
        assert!(narrow.narrow && !narrow.compact);
        assert_eq!(
            (narrow.inset, narrow.padding, narrow.gap),
            (NARROW_SPACE, NARROW_SPACE, NARROW_SPACE)
        );
        assert!(!layout(360.0, 1.0, true, 3).narrow);
        // Not laid out yet: the usual insets and full-size buttons.
        let unmeasured = layout(0.0, 1.0, true, 3);
        assert!(!unmeasured.narrow && !unmeasured.compact && unmeasured.button == round);
        for scale in [0.8, 1.0, 1.5, 24.0 / ui_text::REFERENCE_SIZE] {
            for pane in [100.0, 160.0, 240.0, 300.0, 368.0, 500.0, 720.0, 1440.0] {
                for actions in 1..=3 {
                    let fit = layout(pane, scale, true, actions);
                    let what = format!("{pane} px at {scale}×, {actions} actions");
                    // The action row fits inside the card, whose insets fit in the pane.
                    let row = actions as f32 * fit.button + (actions - 1) as f32 * fit.gap;
                    let inner = pane - 2.0 * (fit.inset + fit.padding) - 2.0;
                    assert!(row <= inner + 0.01, "row {row} > {inner}: {what}");
                    assert!(fit.button >= 1.0 && fit.button <= (round * scale.max(1.0)).round());
                    // A scale grows the insets with the text, the narrow ones stay as they are.
                    if !fit.narrow {
                        assert_eq!(fit.inset, (CARD_INSET * scale.max(1.0)).round(), "{what}");
                    }
                }
            }
        }
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
