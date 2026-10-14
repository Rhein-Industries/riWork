//! Chat tabs: Codex, Claude and Muse driven through their structured interfaces
//! instead of their terminal TUIs.
//!
//! - `model` is the provider-neutral vocabulary every part speaks: what a chat
//!   is, the items of its transcript, the events that build it, and the commands
//!   a user sends. `Transcript` folds events into what a tab draws.
//! - `driver` is the seam between a chat and one provider process (`codex
//!   app-server` over JSON-RPC, `claude` over stream-json, `muse serve` over
//!   MSP JSON-RPC). `codex`, `claude` and `muse` are the drivers; `child` is
//!   what they share (the process in its own group, a size-capped line reader).
//! - `client` is the blocking client a window and the CLI use to reach it.
//! - `wire` is the line protocol of the chat host (`riwork chat serve`), the
//!   background process that owns the provider processes so chats keep running
//!   while app windows reload.
//! - `host` is that process: it serves `wire`, keeps each chat's files (`log`)
//!   and starts drivers with the configuration `launch` resolves.
//! - `text` writes a transcript as plain text, for what reads a chat like a
//!   terminal (`riwork orchestrator output`).
//! - `catalog` reads the models each provider offers from what chats saved, for
//!   a chat that is not running that provider yet.
//! - `suggest` is what a message box offers while a `/` command or an `@` file is
//!   typed, for the desktop and (through `riwork chat suggestions`) the phone.

// The contract lands before the host and the tabs that use it.
#![allow(dead_code)]

mod account_identity;
#[cfg(test)]
mod attachment_driver_tests;
pub mod attachments;
pub mod billing;
pub mod catalog;
pub mod child;
pub mod claude;
pub mod client;
pub mod codex;
pub mod driver;
pub mod host;
pub mod launch;
pub mod log;
pub mod media;
pub mod model;
pub mod muse;
mod notice_dismissals;
mod rate_limits;
pub mod remote_payload;
pub mod subagents;
pub mod suggest;
#[cfg(any(test, feature = "test-support"))]
pub mod testing;
#[cfg(test)]
mod testkit;
pub mod text;
pub mod wire;
