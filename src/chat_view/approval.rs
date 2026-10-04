//! The bar above the composer where a chat asks the user to allow something, and the
//! keyboard that answers it.

use crate::chat::model::{ApprovalKind, Decision};

/// What the button for a decision says.
pub fn decision_label(decision: Decision) -> &'static str {
    match decision {
        Decision::Accept => "Allow",
        Decision::AcceptForSession => "Allow for session",
        Decision::Decline => "Deny",
        Decision::Cancel => "Stop",
    }
}

/// The key that makes the decision, if it has one.
pub fn decision_key(decision: Decision) -> Option<&'static str> {
    match decision {
        Decision::Accept => Some("⏎"),
        Decision::AcceptForSession => Some("⇧⏎"),
        Decision::Decline => Some("⎋"),
        Decision::Cancel => None,
    }
}

/// What is being asked, as the bar's small heading.
pub fn kind_label(kind: ApprovalKind) -> &'static str {
    match kind {
        ApprovalKind::Command => "Run a command?",
        ApprovalKind::FileChange => "Change files?",
        ApprovalKind::Permissions => "Grant more access?",
        ApprovalKind::Tool => "Use a tool?",
    }
}

/// The decision a key makes among those the provider offers: ⏎ allows, ⇧⏎ allows for the
/// session and ⎋ denies (or, where denying is not offered, stops). A key whose decision is
/// not offered does nothing, so ⏎ never silently picks another choice.
pub fn decision_for_key(key: &str, shift: bool, offered: &[Decision]) -> Option<Decision> {
    let wanted = match (key, shift) {
        ("enter" | "return", false) => Decision::Accept,
        ("enter" | "return", true) => Decision::AcceptForSession,
        ("escape", _) => {
            if offered.contains(&Decision::Decline) {
                Decision::Decline
            } else {
                Decision::Cancel
            }
        }
        _ => return None,
    };
    offered.contains(&wanted).then_some(wanted)
}

/// The lines of an approval's detail to show: the first few, or all when expanded, and how
/// many more there are.
pub fn detail_lines(detail: &str, expanded: bool) -> (Vec<&str>, usize) {
    const FOLDED: usize = 3;
    let lines: Vec<&str> = detail.trim_end().lines().collect();
    if expanded || lines.len() <= FOLDED {
        (lines, 0)
    } else {
        let hidden = lines.len() - FOLDED;
        (lines[..FOLDED].to_vec(), hidden)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Decision::*;

    const ALL: [Decision; 4] = [Accept, AcceptForSession, Decline, Cancel];

    #[test]
    fn return_allows_shift_return_allows_for_the_session_and_escape_denies() {
        assert_eq!(decision_for_key("enter", false, &ALL), Some(Accept));
        assert_eq!(decision_for_key("return", false, &ALL), Some(Accept));
        assert_eq!(
            decision_for_key("enter", true, &ALL),
            Some(AcceptForSession)
        );
        assert_eq!(decision_for_key("escape", false, &ALL), Some(Decline));
        assert_eq!(decision_for_key("a", false, &ALL), None);
        assert_eq!(decision_for_key("tab", true, &ALL), None);
    }

    #[test]
    fn a_key_only_makes_a_decision_the_provider_offers() {
        // A request that cannot be allowed for the session: ⇧⏎ does nothing, not ⏎'s job.
        let narrow = [Accept, Decline];
        assert_eq!(decision_for_key("enter", true, &narrow), None);
        assert_eq!(decision_for_key("enter", false, &narrow), Some(Accept));
        // Where denying is not offered, ⎋ stops the turn if it can.
        assert_eq!(
            decision_for_key("escape", false, &[Accept, Cancel]),
            Some(Cancel)
        );
        assert_eq!(decision_for_key("escape", false, &[Accept]), None);
        assert_eq!(decision_for_key("enter", false, &[]), None);
    }

    #[test]
    fn buttons_say_what_they_do_and_the_key_that_does_it() {
        assert_eq!(
            ALL.map(decision_label),
            ["Allow", "Allow for session", "Deny", "Stop"]
        );
        assert_eq!(
            ALL.map(decision_key),
            [Some("⏎"), Some("⇧⏎"), Some("⎋"), None]
        );
        assert_eq!(kind_label(ApprovalKind::Command), "Run a command?");
    }

    #[test]
    fn a_long_detail_is_folded_to_a_few_lines_until_expanded() {
        let detail = "a\nb\nc\nd\ne\n";
        assert_eq!(detail_lines(detail, false), (vec!["a", "b", "c"], 2));
        assert_eq!(
            detail_lines(detail, true),
            (vec!["a", "b", "c", "d", "e"], 0)
        );
        assert_eq!(detail_lines("a\nb", false), (vec!["a", "b"], 0));
        assert_eq!(detail_lines("", false), (vec![], 0));
    }
}
