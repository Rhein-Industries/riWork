//! Where a chat's conversation went on: the marker the host writes into a chat that was
//! handed off or forked (`handoff`), naming the chat that took the conversation.
//!
//! The new chat says where it came from in its own info (`ChatInfo::origin`); the chat it
//! came from gets this marker in its log, so that its transcript says, at the point it was
//! cut, that the conversation goes on elsewhere. It is Somtus's own record: never sent to the
//! provider, never read into a handoff document, and never the provider session's history.
//!
//! Like a worker card (`workers`), a marker is an ordinary completed `tool_call` (server
//! `somtus`, tool `lineage`) with an additive `presentation.lineage`, written as one
//! `ItemCompleted` with no turn: a reader from before it (an older desktop, the phone) draws a
//! plain tool row at the end of the transcript, and no reader has to skip an event type it
//! does not know (`ChatEvent` is closed: an unknown event stops `log::read_envelopes`, fails a
//! snapshot page and the phone's reply). The desktop shows it right after the item it names
//! (`LineageMark::at`), or where it was written.

use super::model::{
    Item, ItemBody, ItemStatus, LineageContext, LineageKind, Presentation, Provider,
};
use serde::{Deserialize, Serialize};

pub const SERVER: &str = "riwork";
pub const TOOL: &str = "lineage";
const ID_PREFIX: &str = "lineage:";
/// The longest title a marker keeps.
const TITLE_CHARS: usize = 120;

/// One chat the conversation went on in (`Presentation::lineage`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineageMark {
    pub kind: LineageKind,
    /// The chat that took the conversation.
    pub target_id: String,
    #[serde(default)]
    pub target_title: String,
    /// Its agent (`codex`, `claude`, `muse`) and model, when it was made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The item of this chat the conversation was cut at, that item included; none for the
    /// whole of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    pub context: LineageContext,
    /// When it was written, in Unix seconds.
    #[serde(default)]
    pub noted_at: u64,
}

impl LineageMark {
    /// What the marker's row says: "Handed off to Codex · gpt-5", "Forked here → “Fix it (fork)”".
    pub fn label(&self) -> String {
        self.label_in(None)
    }

    /// The label in a chat of `provider`: a fork that kept the chat's agent names only the
    /// model it was given, if any.
    pub fn label_in(&self, provider: Option<Provider>) -> String {
        let same = self.kind == LineageKind::Fork
            && provider
                .is_some_and(|provider| self.provider.as_deref() == Some(provider_word(provider)));
        let agent = if same {
            self.model
                .as_deref()
                .map(str::trim)
                .filter(|model| !model.is_empty())
                .map(str::to_owned)
        } else {
            agent_label(self.provider.as_deref(), self.model.as_deref())
        };
        match self.kind {
            LineageKind::Fork => {
                let title = quoted(&self.target_title);
                match agent {
                    Some(agent) => format!("Forked here → {title} · {agent}"),
                    None => format!("Forked here → {title}"),
                }
            }
            LineageKind::Handoff | LineageKind::Unknown => match agent {
                Some(agent) => format!("Handed off to {agent}"),
                None => format!("Handed off to {}", quoted(&self.target_title)),
            },
        }
    }
}

/// "Codex · gpt-5", "Claude", or none when nothing is known.
pub fn agent_label(provider: Option<&str>, model: Option<&str>) -> Option<String> {
    let provider = provider.map(provider_label);
    let model = model.map(str::trim).filter(|model| !model.is_empty());
    match (provider, model) {
        (Some(provider), Some(model)) => Some(format!("{provider} · {model}")),
        (Some(provider), None) => Some(provider.to_owned()),
        (None, Some(model)) => Some(model.to_owned()),
        (None, None) => None,
    }
}

/// The name a person knows an agent by, from the word Somtus keeps for it.
pub fn provider_label(word: &str) -> &str {
    match word {
        "codex" => "Codex",
        "claude" => "Claude",
        "muse" => "Muse",
        "grok" => "Grok",
        other => other,
    }
}

/// The word Somtus keeps for a chat's provider.
pub fn provider_word(provider: Provider) -> &'static str {
    match provider {
        Provider::Codex => "codex",
        Provider::Claude => "claude",
        Provider::Muse => "muse",
        Provider::Acp => "acp",
    }
}

pub fn quoted(title: &str) -> String {
    let title = title.trim();
    if title.is_empty() {
        "a new chat".to_owned()
    } else {
        format!("“{title}”")
    }
}

/// A title as a marker or an origin keeps it: one line, bounded.
pub fn bounded_title(title: &str) -> String {
    title
        .chars()
        .filter(|c| !c.is_control())
        .take(TITLE_CHARS)
        .collect::<String>()
        .trim()
        .to_owned()
}

pub fn of(item: &Item) -> Option<&LineageMark> {
    item.presentation.lineage.as_ref()
}

/// Whether the host wrote `item` as a lineage marker, readable by this build or not.
pub fn is_marker(item: &Item) -> bool {
    item.presentation.lineage.is_some()
        || matches!(&item.body, ItemBody::ToolCall { server: Some(server), tool, .. }
            if server == SERVER && tool == TOOL)
}

/// The id of the marker for the chat `target_id`: one marker for each chat the conversation
/// went on in, so writing it again replaces it.
pub fn id_for(target_id: &str) -> String {
    format!("{ID_PREFIX}{target_id}")
}

/// The marker item for `mark`.
pub fn item(mark: LineageMark) -> Item {
    Item {
        started_at: None,
        ended_at: None,
        body: ItemBody::ToolCall {
            server: Some(SERVER.to_owned()),
            tool: TOOL.to_owned(),
            input: serde_json::json!({
                "kind": mark.kind,
                "target_id": mark.target_id,
                "title": mark.target_title,
            }),
            output: None,
        },
        id: id_for(&mark.target_id),
        turn_id: None,
        status: ItemStatus::Completed,
        presentation: Presentation {
            lineage: Some(mark),
            ..Default::default()
        },
    }
}

/// Checks a marker a client asks the host to write: ids that name chats, words and titles of
/// a bounded size.
pub fn check(mark: &LineageMark) -> Result<(), String> {
    let canonical = |id: &str| uuid::Uuid::parse_str(id).is_ok_and(|uuid| uuid.to_string() == id);
    if !canonical(&mark.target_id) {
        return Err("a lineage marker names its chat by a canonical UUID".into());
    }
    if mark.kind == LineageKind::Unknown || mark.context == LineageContext::Unknown {
        return Err("unknown lineage kind or context".into());
    }
    let short = |text: &Option<String>| text.as_ref().is_none_or(|text| text.len() <= 512);
    if mark.target_title.len() > 512
        || !short(&mark.provider)
        || !short(&mark.model)
        || !short(&mark.at)
    {
        return Err("a lineage marker's words are at most 512 bytes".into());
    }
    Ok(())
}
