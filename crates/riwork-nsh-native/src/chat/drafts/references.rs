//! Durable source references. The captured source/excerpt and the user's editable
//! quote/comment are separate, so editing a reply never rewrites its provenance.
use riwork_engine::chat::model::ChangeKind;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MAX_REFERENCES: usize = 16;
pub const MAX_REFERENCE_TEXT: usize = 8 * 1024;
const MAX_REFERENCE_BYTES: usize = 192 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReferenceOrigin {
    Message {
        item: String,
        turn: Option<String>,
        revision: String,
    },
    Diff {
        item: String,
        turn: Option<String>,
        change: usize,
        path: String,
        kind: ChangeKind,
        revision: String,
        first: usize,
        last: usize,
        old: Vec<(usize, usize)>,
        new: Vec<(usize, usize)>,
    },
}
impl ReferenceOrigin {
    pub fn item(&self) -> &str {
        match self {
            Self::Message { item, .. } | Self::Diff { item, .. } => item,
        }
    }
    pub fn revision(&self) -> &str {
        match self {
            Self::Message { revision, .. } | Self::Diff { revision, .. } => revision,
        }
    }
    pub fn label(&self) -> String {
        match self {
            Self::Message { .. } => "Reply to answer".into(),
            Self::Diff { path, .. } => format!("Review {path}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceFragment {
    pub key: String,
    pub start: usize,
    pub end: usize,
    pub text: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReferenceState {
    #[default]
    Current,
    Changed,
    Missing,
    Unloaded,
}
impl ReferenceState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Current => "Source matches saved revision",
            Self::Changed => "Source changed · saved excerpt retained",
            Self::Missing => "Source unavailable · saved excerpt retained",
            Self::Unloaded => "Waiting for source · saved excerpt retained",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftReference {
    pub id: String,
    pub chat: Option<String>,
    pub origin: ReferenceOrigin,
    #[serde(default)]
    pub original_origin: Option<ReferenceOrigin>,
    pub excerpt: String,
    pub quote: String,
    pub comment: String,
    #[serde(default)]
    pub fragments: Vec<ReferenceFragment>,
    #[serde(default)]
    pub state: ReferenceState,
}

pub fn revision(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

pub fn ranges(ranges: &[(usize, usize)]) -> String {
    if ranges.is_empty() {
        return "none".into();
    }
    ranges
        .iter()
        .map(|(first, last)| {
            if first == last {
                first.to_string()
            } else {
                format!("{first}–{last}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn valid(references: &[DraftReference]) -> bool {
    let origin_valid = |origin: &ReferenceOrigin| {
        let (item, turn, revision) = match origin {
            ReferenceOrigin::Message {
                item,
                turn,
                revision,
            } => (item, turn, revision),
            ReferenceOrigin::Diff {
                item,
                turn,
                revision,
                path,
                first,
                last,
                old,
                new,
                ..
            } => {
                if path.len() > 4096
                    || last.checked_sub(*first).is_none_or(|count| count >= 80)
                    || old.len() > 80
                    || new.len() > 80
                    || old.iter().chain(new).any(|(first, last)| first > last)
                {
                    return false;
                }
                (item, turn, revision)
            }
        };
        !item.is_empty()
            && item.len() <= 4096
            && turn.as_ref().is_none_or(|turn| turn.len() <= 4096)
            && revision.len() == 64
            && revision.bytes().all(|byte| byte.is_ascii_hexdigit())
    };
    let mut ids = std::collections::HashSet::new();
    references.len() <= MAX_REFERENCES
        && references.iter().all(|reference| {
            uuid::Uuid::parse_str(&reference.id).is_ok()
                && ids.insert(&reference.id)
                && reference.chat.as_ref().is_none_or(|chat| chat.len() <= 256)
                && origin_valid(&reference.origin)
                && reference.original_origin.as_ref().is_none_or(origin_valid)
                && reference.excerpt.len() <= MAX_REFERENCE_TEXT
                && reference.quote.len() <= MAX_REFERENCE_TEXT
                && reference.comment.len() <= MAX_REFERENCE_TEXT
                && reference.fragments.len() <= 128
                && reference.fragments.iter().all(|fragment| {
                    fragment.start <= fragment.end
                        && fragment.end - fragment.start == fragment.text.len()
                        && fragment.text.len() <= MAX_REFERENCE_TEXT
                        && fragment.key.len() <= 4096
                })
        })
        && serde_json::to_vec(references).is_ok_and(|encoded| encoded.len() <= MAX_REFERENCE_BYTES)
}

/// The provider receives readable quoted context. Source revisions are explicit,
/// including stale captures, while user writing and attachment bytes stay unchanged.
pub fn provider_text(text: &str, references: &[DraftReference]) -> String {
    if references.is_empty() {
        return text.to_owned();
    }
    let mut output = text.to_owned();
    for reference in references {
        if !output.is_empty() {
            output.push_str("\n\n");
        }
        match &reference.origin {
            ReferenceOrigin::Message { .. } => output.push_str("Regarding this answer:\n"),
            ReferenceOrigin::Diff { path, old, new, .. } => {
                output.push_str(&format!(
                    "Review comment on {}:\nOld lines {}; new lines {}.\n",
                    serde_json::to_string(path).expect("string"),
                    ranges(old),
                    ranges(new)
                ));
            }
        }
        output.push_str(&format!(
            "Source revision: {}. {}.\n",
            reference.origin.revision(),
            reference.state.label()
        ));
        if let Some(original) = &reference.original_origin {
            output.push_str(&format!(
                "Re-anchored from revision {}.\n",
                original.revision()
            ));
        }
        if reference.quote != reference.excerpt {
            output.push_str("The quote below was edited in the draft.\n");
        }
        for line in reference.quote.split('\n') {
            output.push_str("> ");
            output.push_str(line);
            output.push('\n');
        }
        if !reference.comment.is_empty() {
            output.push_str(match reference.origin {
                ReferenceOrigin::Message { .. } => "Reply: ",
                ReferenceOrigin::Diff { .. } => "Comment: ",
            });
            output.push_str(&reference.comment);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    fn reference() -> DraftReference {
        DraftReference {
            id: uuid::Uuid::new_v4().to_string(),
            chat: Some(uuid::Uuid::new_v4().to_string()),
            origin: ReferenceOrigin::Diff {
                item: "edit".into(),
                turn: Some("turn".into()),
                change: 0,
                path: "src/main.rs".into(),
                kind: ChangeKind::Modify,
                revision: revision("patch"),
                first: 1,
                last: 3,
                old: vec![(1, 2), (500, 500)],
                new: vec![(1, 2), (600, 600)],
            },
            original_origin: None,
            excerpt: "-old\n+new".into(),
            quote: "+new".into(),
            comment: "Please simplify 🦀\nKeep this exact wording.".into(),
            fragments: Vec::new(),
            state: ReferenceState::Changed,
        }
    }
    #[test]
    fn revision_is_stable_and_provider_serialization_preserves_writing_and_coordinates() {
        assert_eq!(
            revision("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let reference = reference();
        let text = provider_text("Existing note  \n", std::slice::from_ref(&reference));
        assert!(text.starts_with("Existing note  \n\n\nReview comment on \"src/main.rs\":"));
        assert!(text.contains("Old lines 1–2, 500; new lines 1–2, 600."));
        assert!(text.contains("Source changed"));
        assert!(text.contains("> +new\nComment: Please simplify 🦀\nKeep this exact wording."));
        assert!(!text.contains("> -old"));
        assert_eq!(provider_text("  legacy \n", &[]), "  legacy \n");
    }
    #[test]
    fn legacy_records_default_to_no_references_and_new_records_keep_all_provenance() {
        let old: super::super::Contents =
            serde_json::from_str(r#"{"text":"old draft","attachments":[]}"#).unwrap();
        assert!(old.references.is_empty());
        let mut reference = reference();
        reference.original_origin = Some(reference.origin.clone());
        let record = super::super::Record {
            draft: super::super::Contents {
                text: "kept".into(),
                attachments: Vec::new(),
                references: vec![reference],
            },
            ..Default::default()
        };
        assert_eq!(
            super::super::Record::decode(record.encode().unwrap()),
            Some(record)
        );
    }
    #[test]
    fn malformed_or_oversized_references_are_not_admitted() {
        let mut reference = reference();
        assert!(valid(std::slice::from_ref(&reference)));
        reference.comment = "a".repeat(MAX_REFERENCE_TEXT + 1);
        assert!(!valid(std::slice::from_ref(&reference)));
        reference.comment.clear();
        let mut duplicates = vec![reference.clone(), reference.clone()];
        assert!(!valid(&duplicates));
        duplicates[1].id = uuid::Uuid::new_v4().to_string();
        assert!(valid(&duplicates));
        if let ReferenceOrigin::Diff { revision, .. } = &mut reference.origin {
            revision.clear();
        }
        assert!(!valid(&[reference]));
    }
}
