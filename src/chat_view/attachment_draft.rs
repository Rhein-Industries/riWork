//! Submission snapshots for the Kit composer handoff. The UI retains its editor contents
//! until a receipt arrives. A transport failure is uncertain and requires explicit review.
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
    pub snapshot: Draft,
    pub status: Status,
    dispatched: bool,
}
impl Submission {
    pub fn begin(draft: &Draft) -> Result<Self, String> {
        if draft.attachments.is_empty() {
            return Err("an attachment submission needs attachments".into());
        }
        Ok(Self {
            snapshot: draft.clone(),
            status: Status::Pending,
            dispatched: false,
        })
    }
    pub fn command(&mut self) -> Result<ChatCommand, String> {
        if self.status != Status::Pending || self.dispatched {
            return Err("submission already attempted; review the outcome before resending".into());
        }
        self.dispatched = true;
        Ok(ChatCommand::SendAttachments {
            text: self.snapshot.text.clone(),
            attachments: self.snapshot.attachments.clone(),
        })
    }
    /// The saved snapshot remains available even when newer editor contents have replaced
    /// it. Refusal/Broken never overwrite newer edits or erase the failed draft.
    pub fn settle(&mut self, current: &mut Draft, result: Result<(), CallError>) {
        if self.status != Status::Pending {
            return;
        }
        self.status = match result {
            Ok(()) => {
                if current == &self.snapshot {
                    *current = Draft::default();
                }
                Status::Submitted
            }
            Err(CallError::Refused(e)) => Status::Refused(e),
            Err(CallError::Broken(e)) => Status::Uncertain(e),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::{model::Provider, testing::TestHost};
    #[test]
    fn failed_and_uncertain_submissions_preserve_exact_draft_and_never_become_completion() {
        let host = TestHost::new();
        let chat = host.create(Provider::Codex);
        let path = host.work().join("notes");
        std::fs::write(&path, b"file contents").unwrap();
        let draft = Draft {
            text: "exact draft 🦀  \n".into(),
            attachments: vec![host.client().stage_attachment(&chat.id, &path).unwrap()],
        };
        for error in [
            CallError::Refused("unsupported host".into()),
            CallError::Broken("reply lost".into()),
        ] {
            let mut current = draft.clone();
            let mut submission = Submission::begin(&current).unwrap();
            submission.settle(&mut current, Err(error));
            assert_eq!(current, draft);
            assert_eq!(submission.snapshot, draft);
            assert!(submission.command().is_err());
            submission.settle(&mut current, Ok(()));
            assert_eq!(current, draft);
        }
        let mut current = draft.clone();
        let mut submission = Submission::begin(&current).unwrap();
        current.text.push_str("newer edit");
        let newer = current.clone();
        submission.settle(&mut current, Ok(()));
        assert_eq!(current, newer);
        assert_eq!(submission.status, Status::Submitted);
        assert_eq!(submission.snapshot, draft);
        let mut current = draft.clone();
        let mut submission = Submission::begin(&current).unwrap();
        submission.settle(&mut current, Ok(()));
        assert_eq!(current, Draft::default());
        // No TurnOutcome is inferred from either a receipt or the cleared editor.
        assert_eq!(submission.status, Status::Submitted);
    }
}
