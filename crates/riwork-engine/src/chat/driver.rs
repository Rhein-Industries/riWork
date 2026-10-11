//! The seam between a chat and one provider process.
//!
//! A driver starts `codex app-server` or `claude` (stream-json), translates the
//! provider's messages into `ChatEvent`s on `events`, and turns `ChatCommand`s
//! into provider requests. It owns the child process and its reader threads; the
//! chat host owns the driver, stores the events and fans them out.

use super::model::{ApprovalMode, ChatCommand, ChatEvent, Item, Provider};
use std::collections::HashMap;
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
    /// The provider's fast mode, when the model has one.
    pub fast: bool,
    /// Resume this Codex thread or Claude session instead of starting one.
    pub resume: Option<String>,
    /// Outstanding sticky notices from earlier driver sessions.
    pub outstanding_notices: HashMap<String, Item>,
    /// Extra arguments before the driver's own, e.g. Cua MCP configuration.
    pub extra_args: Vec<String>,
    /// What the agent is told beside the provider's own instructions: the conversation a
    /// chat had before it switched provider (`ChatInfo::carried_over`). Claude gets it
    /// appended to its system prompt, Codex as the thread's developer instructions, at
    /// every start and resume.
    pub instructions: Option<String>,
    /// Environment to set and to remove (e.g. `CODEX_HOME`; `ANTHROPIC_API_KEY`
    /// is always removed for Claude so a stray key cannot switch billing).
    pub env: Vec<(OsString, OsString)>,
    pub env_remove: Vec<OsString>,
}

/// A running provider process for one chat.
pub trait Driver: Send {
    /// Cancel owned-child I/O without acquiring the host's command mutex.
    fn cancel_io(&self) -> Option<std::sync::Arc<dyn Fn() + Send + Sync>> {
        None
    }
    /// Act on a user command. Errors are for commands the driver cannot carry
    /// out at all; provider failures arrive as events.
    fn command(&mut self, command: ChatCommand) -> Result<(), String>;
    /// The Codex thread id or Claude session id, once known.
    fn provider_thread_id(&self) -> Option<String>;
    /// Stop the process (gracefully, then by force) and wait for it. Ends the
    /// events with `State { Stopped }` (a chat that already failed stays
    /// failed); a second call does nothing.
    fn shutdown(&mut self);
}

/// Start the provider process for `config`, sending its events to `events`
/// until it exits. Blocks until the provider is ready (call it off the UI
/// thread): the events so far are `State { Starting }` and `State { Idle }`,
/// and `provider_thread_id()` is known. Drivers do not emit `Info`, since they
/// know neither the chat's id nor its title; the host builds it from
/// `provider_thread_id()`, which differs from `config.resume` when the
/// provider had no such thread to resume. An error leaves no process behind,
/// and the events end with `State { Failed }` carrying the same message.
pub type StartDriver = fn(DriverConfig, Sender<ChatEvent>) -> Result<Box<dyn Driver>, String>;
