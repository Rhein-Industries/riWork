//! The message box: what ⏎ and ⎋ do in it, moving between its lines, and the text it sends.

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

/// The narrowest the message box may get beside its buttons, in design points.
pub const MIN_FIELD: f32 = 120.0;

/// Where the message box's buttons go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fit {
    /// In one row with the box; `mic` is whether the mic still fits in it.
    Inline { mic: bool },
    /// The box takes the whole width, its buttons a row under it; `mic` as for `Inline`.
    Stacked { mic: bool },
}

/// The buttons' place for a row `width` px wide (the bar inside its padding) at interface
/// scale `scale`, with the mic shown (`mic`) and a turn running (`running`, which adds Stop).
/// The box keeps at least `MIN_FIELD`: the mic goes first, then every button moves to a row
/// under the box, where the mic comes back if that row has room for it. Lengths are design
/// points, grown with the text the way `ui_text::space` grows them. A width not yet laid
/// out (zero) keeps everything in the row.
pub fn fit(width: f32, scale: f32, mic: bool, running: bool, button: f32, gap: f32) -> Fit {
    let space = |base: f32| (base * scale.max(1.0)).round();
    // Each button with the gap before it (under the box, the gap before the space that
    // pushes the rest to the end stands in for the box's).
    let buttons = |count: usize| count as f32 * (space(button) + space(gap));
    // Attach and Send are always there.
    let base = 2 + usize::from(running);
    let all = base + usize::from(mic);
    if width <= 0.0 || width >= space(MIN_FIELD) + buttons(all) {
        Fit::Inline { mic }
    } else if width >= space(MIN_FIELD) + buttons(base) {
        Fit::Inline { mic: false }
    } else {
        Fit::Stacked {
            mic: mic && width >= buttons(all),
        }
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
    fn the_box_keeps_its_minimum_width_dropping_the_mic_then_stacking_the_buttons() {
        let fit = |width, scale, mic, running| fit(width, scale, mic, running, 26.0, 6.0);
        // 120 for the box and 32 per button.
        assert_eq!(fit(500.0, 1.0, true, true), Fit::Inline { mic: true });
        assert_eq!(fit(248.0, 1.0, true, true), Fit::Inline { mic: true });
        assert_eq!(fit(247.0, 1.0, true, true), Fit::Inline { mic: false });
        assert_eq!(fit(216.0, 1.0, true, true), Fit::Inline { mic: false });
        assert_eq!(fit(215.0, 1.0, true, true), Fit::Stacked { mic: true });
        // The narrowest pane (160 pt, 140 inside the bar) stacks, running or not, and keeps
        // the mic under the box while its row has room.
        assert_eq!(fit(140.0, 1.0, true, true), Fit::Stacked { mic: true });
        assert_eq!(fit(140.0, 1.0, false, false), Fit::Stacked { mic: false });
        // Without Stop, more room; without the mic, nothing to drop first.
        assert_eq!(fit(184.0, 1.0, false, false), Fit::Inline { mic: false });
        assert_eq!(fit(216.0, 1.0, true, false), Fit::Inline { mic: true });
        // Bigger text grows the box's minimum and the buttons with it: at 1.15 the 160 pt
        // pane's row has no room for the mic even under the box.
        assert_eq!(fit(248.0, 1.5, true, true), Fit::Stacked { mic: true });
        assert_eq!(fit(372.0, 1.5, true, true), Fit::Inline { mic: true });
        assert_eq!(fit(140.0, 1.15, true, true), Fit::Stacked { mic: false });
        // Smaller text never shrinks them below their design size.
        assert_eq!(fit(247.0, 0.8, true, true), Fit::Inline { mic: false });
        // Not laid out yet: everything in the row.
        assert_eq!(fit(0.0, 1.0, true, true), Fit::Inline { mic: true });
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
