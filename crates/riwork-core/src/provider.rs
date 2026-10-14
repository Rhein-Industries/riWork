//! Provider identity shared by persisted settings and the chat protocol.
//! Serialized names are part of the stored format and must remain compatible.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Codex,
    Claude,
    Muse,
}

/// Who pays for a Claude chat. `Subscription` starts the chat without
/// `ANTHROPIC_API_KEY`, so it runs on the user's Claude sign-in. `ApiKey` gives it the
/// Anthropic API key saved in the Keychain, or, with none saved, leaves the environment as
/// it is: a key exported in the shell is used, and with no key anywhere Claude's own
/// sign-in decides. `ApiKey` is the default: it is what a Claude chat always did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeBilling {
    Subscription,
    #[default]
    ApiKey,
}

impl ClaudeBilling {
    pub const ALL: [Self; 2] = [Self::Subscription, Self::ApiKey];

    pub fn label(self) -> &'static str {
        match self {
            Self::Subscription => "Subscription",
            Self::ApiKey => "API key",
        }
    }

    /// The choice a one-click switch moves to.
    pub fn other(self) -> Self {
        match self {
            Self::Subscription => Self::ApiKey,
            Self::ApiKey => Self::Subscription,
        }
    }

    /// The `claude_chat_ignores_api_key` switch this choice grew from: true is
    /// `Subscription`, false `ApiKey` (`Settings::claude_chat_billing`).
    pub fn ignores_api_key(self) -> bool {
        self == Self::Subscription
    }

    pub fn from_ignores_api_key(ignores: bool) -> Self {
        if ignores {
            Self::Subscription
        } else {
            Self::ApiKey
        }
    }
}
