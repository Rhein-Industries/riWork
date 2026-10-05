//! The chat host's line protocol on `RIWORK_HOME/run/chat.sock` (mode 600).
//!
//! Every line is one JSON object. A client sends `Request`s and gets one
//! `Response` each, matched by `id`. After a successful `subscribe` the
//! connection carries only `Envelope`s for that chat, oldest first from the
//! requested sequence number, then live ones; a client subscribes on a
//! connection of its own.

use super::model::{ChatCommand, ChatEvent, ChatInfo, NewChat};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Start a chat. The result is its `ChatInfo`. A chat that makes itself an
    /// orchestrator of a scope that has one is refused (`ORCHESTRATOR_EXISTS`).
    Create { id: String, chat: NewChat },
    /// All chats the host knows, running or not. The result is `[ChatInfo]`.
    List { id: String },
    /// Act on a chat. A stopped chat is resumed first.
    Command {
        id: String,
        chat_id: String,
        command: ChatCommand,
    },
    /// Replay the chat's events from `since` (0 for all), then stream new ones.
    Subscribe {
        id: String,
        chat_id: String,
        #[serde(default)]
        since: u64,
    },
    /// Stop the chat's process and keep its history.
    Close { id: String, chat_id: String },
    /// Stop the chat and delete its history.
    Delete { id: String, chat_id: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub id: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One event of a chat with its position in the chat's log.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub chat_id: String,
    /// 1-based and gapless per chat; a client resumes with `since = seq`.
    pub seq: u64,
    pub event: ChatEvent,
}

/// The `result` of `List`.
pub type ChatList = Vec<ChatInfo>;
