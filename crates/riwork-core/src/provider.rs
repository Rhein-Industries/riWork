//! Provider identity shared by persisted settings and the chat protocol.
//! Serialized names are part of the stored format and must remain compatible.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Codex,
    Claude,
    Muse,
    /// A user-configured Agent Client Protocol executable.
    Acp,
}

/// Muse remains decodable so existing conversations and settings are preserved.
/// It cannot launch work in this build.
pub const MUSE_DISABLED: &str = "Muse is disabled in Somtus. Choose Codex or Claude.";

impl Provider {
    pub fn ensure_enabled(self) -> Result<(), String> {
        if self == Self::Muse { Err(MUSE_DISABLED.into()) } else { Ok(()) }
    }
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

#[cfg(test)]
mod availability_tests {
    use super::*;
    #[test]
    fn muse_is_readable_but_cannot_launch() {
        assert_eq!(serde_json::from_str::<Provider>("\"muse\"").unwrap(), Provider::Muse);
        assert_eq!(Provider::Muse.ensure_enabled().unwrap_err(), MUSE_DISABLED);
        assert!(Provider::Codex.ensure_enabled().is_ok());
        assert!(Provider::Claude.ensure_enabled().is_ok());
        assert!(crate::settings::AUTOMATIC_ORCHESTRATORS.iter().all(|(provider, _)| provider.ensure_enabled().is_ok()));
    }
}
