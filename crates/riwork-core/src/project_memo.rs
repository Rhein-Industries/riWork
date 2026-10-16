//! One free-text Markdown memo per project: the person's own notes for it.
//!
//! The memo is kept in `state.json` beside the projects (`State::project_memos`, keyed by
//! project id), not inside `Project`, because project records are handed to agents
//! (`riwork_project_list` over MCP, `project list --json`) and the memo is not agent context:
//! it leaves the store only when the person reads it, edits it, or inserts it into a chat
//! draft with `/memo`. Saves are whole-text replacements and the last one to reach the store
//! wins; there is no merge.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

/// The most a memo may hold, in UTF-8 bytes after `normalize`: 32 KiB, about 5,000 words of
/// notes. A phone saves the whole memo in one request, whose JSON may double it (a `"`, `\`
/// or `/` is escaped as two bytes; controls other than tab and newline are removed by
/// `normalize`, so none becomes a six-byte `\u00XX`), and the request still fits the remote
/// link's 128 KiB plaintext frame with room for its envelope. Every window re-reads
/// `state.json` when it changes, which keeps that file small as well.
pub const MAX_BYTES: usize = 32 * 1024;

/// The chat composer command that previews this project's memo.
pub const COMMAND: &str = "/memo";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectMemo {
    /// Markdown, as the person wrote it (after `normalize`). Empty is the same as no memo.
    #[serde(default)]
    pub text: String,
    /// Unix seconds of the save that last changed the text; 0 when it never was. Each such save
    /// moves it at least one past the one before, even within one second or with the clock set
    /// back, so equal times mean the same revision of the memo (a phone saves its kept text only
    /// over the revision it was written on).
    #[serde(default)]
    pub updated_at: u64,
}

impl ProjectMemo {
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }
}

/// The text a memo stores for `text`: line breaks as `\n` (a `\r\n` or lone `\r` becomes one
/// `\n`), and no control character other than tab and newline (a pasted escape sequence or NUL
/// is dropped). Nothing else changes, so typing never moves under the cursor. Idempotent.
pub fn normalize(text: &str) -> Cow<'_, str> {
    if !text
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            '\n' | '\t' => out.push(c),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

/// How many bytes `text` (already normalized) is over `MAX_BYTES`, if it is.
pub fn bytes_over(text: &str) -> Option<usize> {
    text.len().checked_sub(MAX_BYTES).filter(|over| *over > 0)
}

/// Why `text` (already normalized) cannot be saved, in words for the person, or `None`.
pub fn too_large(text: &str) -> Option<String> {
    bytes_over(text).map(|_| {
        format!(
            "The memo is {} bytes; a memo holds at most {MAX_BYTES} bytes (32 KB). Shorten it to save.",
            text.len()
        )
    })
}

/// What is left of `draft` when it ends in the `/memo` command, or `None` when it does not.
/// The command is the draft's last word: `/memo` alone, or after whitespace (`notes /memo`),
/// with only whitespace after it. The rest keeps its own text with trailing whitespace removed.
pub fn command_rest(draft: &str) -> Option<&str> {
    let trimmed = draft.trim_end();
    let rest = trimmed.strip_suffix(COMMAND)?;
    if rest.is_empty() || rest.ends_with(char::is_whitespace) {
        Some(rest.trim_end())
    } else {
        None
    }
}

/// `draft` with `memo` appended, as "Insert into composer" leaves it: the memo after a blank
/// line, or alone when the draft is empty. The memo's trailing line breaks are dropped so the
/// cursor ends on its last line; its text is otherwise unchanged. Nothing is sent.
pub fn append_to_draft(draft: &str, memo: &str) -> String {
    let memo = memo.trim_end_matches(['\n', '\r']);
    let head = draft.trim_end();
    if head.is_empty() {
        memo.to_owned()
    } else {
        format!("{head}\n\n{memo}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_unifies_line_breaks_and_drops_controls_only() {
        assert!(matches!(normalize("# Notes\n\n- a\tb"), Cow::Borrowed(_)));
        assert_eq!(normalize("a\r\nb\rc\n"), "a\nb\nc\n");
        assert_eq!(normalize("x\u{1b}[31my\u{0}z\u{7f}"), "x[31myz");
        assert_eq!(normalize("ünï 😀 \u{2028}"), "ünï 😀 \u{2028}");
        let once = normalize("a\r\r\nb\u{85}").into_owned();
        assert_eq!(once, "a\n\nb");
        assert_eq!(normalize(&once), once);
    }

    #[test]
    fn the_cap_counts_utf8_bytes() {
        let full = "x".repeat(MAX_BYTES);
        assert_eq!(bytes_over(&full), None);
        assert_eq!(too_large(&full), None);
        assert_eq!(bytes_over(&format!("{full}y")), Some(1));
        // 10,923 three-byte characters are 32,769 bytes: fewer characters than the cap.
        let heavy = "\u{65e5}".repeat(MAX_BYTES / 3 + 1);
        assert!(heavy.chars().count() < MAX_BYTES);
        assert_eq!(bytes_over(&heavy), Some(1));
        assert!(too_large(&heavy).unwrap().contains("32768 bytes"));
    }

    #[test]
    fn the_command_is_the_drafts_last_word() {
        assert_eq!(command_rest("/memo"), Some(""));
        assert_eq!(command_rest("  /memo \n"), Some(""));
        assert_eq!(command_rest("Review this:\n/memo"), Some("Review this:"));
        assert_eq!(command_rest("see /memo"), Some("see"));
        for draft in ["", "/memos", "x/memo", "/memo please", "/Memo", "memo"] {
            assert_eq!(command_rest(draft), None, "{draft:?}");
        }
    }

    #[test]
    fn insert_appends_after_a_blank_line_and_never_replaces() {
        assert_eq!(append_to_draft("", "# Plan\n- ship\n\n"), "# Plan\n- ship");
        assert_eq!(append_to_draft("  \n", "memo"), "memo");
        assert_eq!(
            append_to_draft("Use these notes:\n", "- a\n- b"),
            "Use these notes:\n\n- a\n- b"
        );
        assert_eq!(
            append_to_draft("draft", "  indented"),
            "draft\n\n  indented"
        );
    }

    #[test]
    fn a_memo_from_json_without_fields_is_empty() {
        let memo: ProjectMemo = serde_json::from_str("{}").unwrap();
        assert!(memo.is_empty());
        assert_eq!(memo.updated_at, 0);
        assert!(
            ProjectMemo {
                text: " \n".into(),
                updated_at: 1
            }
            .is_empty()
        );
    }
}
