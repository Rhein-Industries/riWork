//! Provider identity shared by persisted settings and the chat protocol.
//! Serialized names are part of the stored format and must remain compatible.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Codex,
    Claude,
}
