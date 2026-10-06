//! Immutable dispatch snapshots. Only a matching editor identity AND generation may clear.
use crate::chat::{attachments::Attachment, client::CallError, model::ChatCommand};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Draft {
    pub text: String,
    pub attachments: Vec<Attachment>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Status {
    Pending,
    Submitted,
    Refused(String),
    Uncertain(String),
}
#[derive(Clone, Debug)]
pub(super) struct Submission {
    pub id: u64,
    pub editor: u64,
    pub generation: u64,
    pub snapshot: Draft,
    pub status: Status,
    dispatched: bool,
}
impl Submission {
    pub fn begin(id: u64, editor: u64, generation: u64, draft: Draft) -> Result<Self, String> {
        if draft.attachments.is_empty() && draft.text.trim().is_empty() {
            return Err("the draft is empty".into());
        }
        Ok(Self {
            id,
            editor,
            generation,
            snapshot: draft,
            status: Status::Pending,
            dispatched: false,
        })
    }
    pub fn command(&mut self) -> Result<ChatCommand, String> {
        if self.status != Status::Pending || self.dispatched {
            return Err(
                "submission already attempted; inspect the outcome before explicitly resending"
                    .into(),
            );
        }
        self.dispatched = true;
        Ok(self.expected_command())
    }
    pub fn expected_command(&self) -> ChatCommand {
        if self.snapshot.attachments.is_empty() {
            // The wire shape and trimming policy of legacy Send are unchanged.
            ChatCommand::Send {
                text: self.snapshot.text.trim().to_owned(),
            }
        } else {
            ChatCommand::SendAttachments {
                text: self.snapshot.text.clone(),
                attachments: self.snapshot.attachments.clone(),
            }
        }
    }
    /// Duplicate/stale receipts cannot change a settled status. Success acknowledges
    /// submission only; it does not establish a TurnOutcome.
    pub fn settle(&mut self, editor: u64, generation: u64, result: Result<(), CallError>) -> bool {
        if self.status != Status::Pending {
            return false;
        }
        let clear = result.is_ok()
            && self.dispatched
            && self.editor == editor
            && self.generation == generation;
        self.status = match result {
            Ok(()) => Status::Submitted,
            Err(CallError::Refused(error)) => Status::Refused(error),
            Err(CallError::Broken(error)) => Status::Uncertain(error),
        };
        clear
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn returning_to_equal_text_does_not_make_a_newer_generation_clearable() {
        let draft = Draft {
            text: "exact draft 🦀  \n".into(),
            ..Default::default()
        };
        let mut submission = Submission::begin(1, 4, 8, draft.clone()).unwrap();
        assert_eq!(
            submission.command().unwrap(),
            ChatCommand::Send {
                text: "exact draft 🦀".into()
            }
        );
        assert!(!submission.settle(4, 10, Ok(())));
        assert_eq!(submission.snapshot, draft);
        assert_eq!(submission.status, Status::Submitted);
        assert!(!submission.settle(4, 8, Ok(())));
        let mut other = Submission::begin(2, 4, 8, draft.clone()).unwrap();
        other.command().unwrap();
        assert!(
            !other.settle(5, 8, Ok(())),
            "a replaced editor must not clear"
        );
        let mut exact = Submission::begin(3, 4, 8, draft).unwrap();
        exact.command().unwrap();
        assert!(exact.settle(4, 8, Ok(())));
    }
    #[test]
    fn failed_and_uncertain_submissions_keep_exact_snapshots_without_retries() {
        for error in [
            CallError::Refused("no".into()),
            CallError::Broken("reply lost".into()),
        ] {
            let draft = Draft {
                text: "  keep the exact whitespace 🦀\n".into(),
                ..Default::default()
            };
            let mut submission = Submission::begin(1, 2, 3, draft.clone()).unwrap();
            submission.command().unwrap();
            assert!(!submission.settle(2, 3, Err(error)));
            assert_eq!(submission.snapshot, draft);
            assert!(submission.command().is_err());
            assert!(!submission.settle(2, 3, Ok(())));
            assert!(!matches!(submission.status, Status::Submitted));
        }
    }
}
