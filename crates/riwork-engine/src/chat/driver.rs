//! The seam between a chat and one provider process.
//!
//! A driver starts `codex app-server` or `claude` (stream-json), translates the
//! provider's messages into `ChatEvent`s on `events`, and turns `ChatCommand`s
//! into provider requests. It owns the child process and its reader threads; the
//! chat host owns the driver, stores the events and fans them out.

use super::model::{ApprovalMode, ChatCommand, ChatEvent, ContributorMode, Item, Provider};
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Sender;

/// A provider's accepted settings or observed state. This is internal to the
/// driver/host seam, never a wire event. Omitted fields leave the host alone;
/// `Some(None)` clears a model/effort back to the provider's default.
///
/// A deferred selection (Codex's next-turn model, for example) may be reported
/// once the driver retains it. A later refusal must report the retained value,
/// rather than the rejected request. `fast` is the accepted user preference,
/// not a temporary provider availability decision.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DriverReport {
    pub provider_thread_id: Option<String>,
    pub model: Option<Option<String>>,
    pub effort: Option<Option<String>>,
    pub approval_mode: Option<ApprovalMode>,
    pub fast: Option<bool>,
}

/// Reports to one host run. The host supplies a fresh, request-scoped reporter
/// to `Driver::configure`; capture that reporter in asynchronous replies.
/// Startup/unsolicited observations use `DriverConfig::reporter`. Callbacks
/// acquire only the host's metadata lock, never its driver or lifecycle locks.
/// The default is a no-op for standalone drivers and fixtures.
#[derive(Clone)]
pub struct DriverReporter(Arc<dyn Fn(DriverReport) + Send + Sync>);

impl DriverReporter {
    pub(crate) fn new(report: impl Fn(DriverReport) + Send + Sync + 'static) -> Self {
        Self(Arc::new(report))
    }

    pub fn report(&self, report: DriverReport) {
        (self.0)(report);
    }
}

impl Default for DriverReporter {
    fn default() -> Self {
        Self::new(|_| {})
    }
}

impl std::fmt::Debug for DriverReporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DriverReporter")
    }
}

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
    /// Muse: start `muse serve --contributor-mode off|on` with it, a per-process override Muse
    /// never saves (`ChatInfo::contributor_mode`). It is always passed, so the user's Muse
    /// settings never decide. Other providers ignore it.
    pub contributor_mode: ContributorMode,
    /// Resume this Codex thread or Claude session instead of starting one.
    pub resume: Option<String>,
    /// The current provider session has history (or its emptiness is unknown).
    /// When `resume` is missing from the provider store, fail rather than start
    /// a replacement. False preserves fresh fallback for never-used sessions.
    pub resume_has_history: bool,
    /// Startup and unsolicited provider observations, scoped to this host run.
    pub reporter: DriverReporter,
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
    /// Configure using a reporter pinned to this request. `Ok` only accepts
    /// delivery: the host persists fields explicitly reported by the driver.
    /// Report deltas after acceptance/refusal, including deferred selections
    /// the driver actually retained. Never use the startup reporter for a
    /// configuration acknowledgement. The default provides no confirmation.
    fn configure(&mut self, command: ChatCommand, _reporter: DriverReporter) -> Result<(), String> {
        self.command(command)
    }
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
/// `provider_thread_id()`. A missing resume may start fresh only when
/// `config.resume_has_history` is false. An error leaves no process behind,
/// and the events end with `State { Failed }` carrying the same message.
pub type StartDriver = fn(DriverConfig, Sender<ChatEvent>) -> Result<Box<dyn Driver>, String>;
