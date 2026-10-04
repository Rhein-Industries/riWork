//! The seam between a chat and one provider process.
//!
//! A driver starts `codex app-server` or `claude` (stream-json), translates the
//! provider's messages into `ChatEvent`s on `events`, and turns `ChatCommand`s
//! into provider requests. It owns the child process and its reader threads; the
//! chat host owns the driver, stores the events and fans them out.

use super::model::{ApprovalMode, ChatCommand, ChatEvent, Provider};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::mpsc::Sender;

/// How to start a provider process for one chat.
#[derive(Clone, Debug)]
pub struct DriverConfig {
    pub provider: Provider,
    /// The `codex` or `claude` executable.
    pub program: PathBuf,
    pub cwd: PathBuf,
    pub approval_mode: ApprovalMode,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Resume this Codex thread or Claude session instead of starting one.
    pub resume: Option<String>,
    /// Extra arguments before the driver's own, e.g. Cua MCP configuration.
    pub extra_args: Vec<String>,
    /// Environment to set and to remove (e.g. `CODEX_HOME`; `ANTHROPIC_API_KEY`
    /// is always removed for Claude so a stray key cannot switch billing).
    pub env: Vec<(OsString, OsString)>,
    pub env_remove: Vec<OsString>,
}

/// A running provider process for one chat.
pub trait Driver: Send {
    /// Act on a user command. Errors are for commands the driver cannot carry
    /// out at all; provider failures arrive as events.
    fn command(&mut self, command: ChatCommand) -> Result<(), String>;
    /// The Codex thread id or Claude session id, once known.
    fn provider_thread_id(&self) -> Option<String>;
    /// Stop the process (gracefully, then by force) and wait for it.
    fn shutdown(&mut self);
}

/// Start the provider process for `config`, sending its events to `events`
/// until it exits. The first events are `State { Starting }` then, once the
/// provider is ready, `Info` (with the thread id) and `State { Idle }`.
pub type StartDriver = fn(DriverConfig, Sender<ChatEvent>) -> Result<Box<dyn Driver>, String>;
