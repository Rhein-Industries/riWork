//! Chat tabs: Codex and Claude driven through their structured interfaces
//! instead of their terminal TUIs.
//!
//! - `model` is the provider-neutral vocabulary every part speaks: what a chat
//!   is, the items of its transcript, the events that build it, and the commands
//!   a user sends. `Transcript` folds events into what a tab draws.
//! - `driver` is the seam between a chat and one provider process (`codex
//!   app-server` over JSON-RPC, `claude` over stream-json). `codex` and
//!   `claude` are the two drivers; `child` is what they share (the process in
//!   its own group, a size-capped line reader).
//! - `client` is the blocking client a window and the CLI use to reach it.
//! - `wire` is the line protocol of the chat host (`riwork chat serve`), the
//!   background process that owns the provider processes so chats keep running
//!   while app windows reload.
//! - `host` is that process: it serves `wire`, keeps each chat's files (`log`)
//!   and starts drivers with the configuration `launch` resolves.

// The contract lands before the host and the tabs that use it.
#![allow(dead_code)]

pub mod child;
pub mod claude;
pub mod client;
pub mod codex;
pub mod driver;
pub mod host;
mod launch;
pub(crate) mod log;
pub mod model;
#[cfg(test)]
pub(crate) mod testing;
#[cfg(test)]
mod testkit;
pub mod wire;
