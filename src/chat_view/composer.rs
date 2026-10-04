//! The message box: what ⏎ and ⎋ do in it, moving between its lines, and the text it sends.

use crate::chat::model::Decision;

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
