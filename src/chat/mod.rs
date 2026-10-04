//! Chat tabs: Codex and Claude driven through their structured interfaces
//! instead of their terminal TUIs.
//!
//! - `model` is the provider-neutral vocabulary every part speaks: what a chat
//!   is, the items of its transcript, the events that build it, and the commands
//!   a user sends. `Transcript` folds events into what a tab draws.
//! - `driver` is the seam between a chat and one provider process (`codex
//!   app-server` over JSON-RPC, `claude` over stream-json).
//! - `client` is the blocking client a window and the CLI use to reach it.
//! - `wire` is the line protocol of the chat host (`riwork chat serve`), the
//!   background process that owns the provider processes so chats keep running
//!   while app windows reload.

// The contract lands before the host and the tabs that use it.
#![allow(dead_code)]

pub mod client;
pub mod driver;
pub mod model;
pub mod wire;
