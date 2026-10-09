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
    fn a_message_is_the_trimmed_text_or_nothing() {
        assert_eq!(message("  hello\n\n"), Some("hello".to_owned()));
        assert_eq!(message("a\n\nb"), Some("a\n\nb".to_owned()));
        assert_eq!(message(" \n\t "), None);
        assert_eq!(message(""), None);
    }

    #[test]
    fn columns_count_characters_not_bytes() {
        let text = "héé\nabcd";
        // Column 3 of the first line is its end; below it is column 3 of "abcd".
        assert_eq!(vertical(text, "héé".len(), false), Some("héé\nabc".len()));
        assert_eq!(vertical(text, "héé\nabc".len(), true), Some("héé".len()));
    }
}
