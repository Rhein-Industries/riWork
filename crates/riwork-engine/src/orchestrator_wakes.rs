//! Orchestrator check-ins when a worker finishes a turn. Only identifiers are
//! queued; no agent text.
//!
//! A worker is a harness shell that an orchestrator or other automation opened
//! (`ShellSession::is_worker`): never an orchestrator itself, a user-opened
//! shell or a plain shell. When one completes a turn, the same verified
//! completion that queues a desktop alert (`notifications::record_completion`)
//! queues one wake for each orchestrator scope here: the shell's project
//! orchestrator and the global one. Like an alert, a wake is opt-in per project
//! (`Project::wake_orchestrators_on_agent_done`, which governs both of its
//! scopes) and is deduplicated by the provider's event id under a cross-process
//! lock, so a replayed hook or a second window never queues it twice.
//!
//! **Delivery** runs in the desktop app, which also claims the alerts and
//! dispatches schedules: every GUI process ticks a `Runner` each second and a
//! non-blocking lock lets one of them deliver at a time. While no RiWork app
//! runs nothing is delivered, and a wake expires after `WAKE_LIFETIME`, as an
//! unclaimed alert does. A wake goes only to an orchestrator that exists (none
//! is created for it: a scope without one skips the wake) and is idle: a chat
//! the host reports `Idle`, or a Codex terminal whose rollout shows a completed
//! turn and whose screen shows its empty prompt, the evidence a schedule waits
//! for. A busy orchestrator keeps the wake until it is idle or the wake
//! expires; a chat turn is never steered and a working terminal never typed
//! into. One orchestrator gets at most one wake per tick, and the next only
//! after it finished a turn since the last. Each wake is claimed on disk before
//! the orchestrator hears of it and is never retried.

use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    activity::ActivityTracker,
    chat::{
        self,
        client::{CallError, socket_path},
        model::{ChatCommand, ChatEvent, ChatInfo, ChatState, OrchestratorScope},
    },
    notifications::{canonical_uuid, compact_label, now, reject_symlink, valid_completion},
    orchestrators::{self, ChatHost, Orchestrator},
    schedule_chat::{Proof, seen_in_log},
    sessions::{HarnessKind, SessionManager, ShellSession},
    store::{State, Store},
};

const LEDGER: &str = "orchestrator-wakes.json";
const LOCK: &str = "orchestrator-wakes.lock";
const DELIVERY_LOCK: &str = "orchestrator-wakes-delivery.lock";
const MAX_BYTES: u64 = 8 * 1024 * 1024;
const MAX_ENTRIES: usize = 4096;
const MAX_GUARDS: usize = 256;
/// As long as an unclaimed completion alert waits (`notifications`).
pub const WAKE_LIFETIME: u64 = 5 * 60;
const RECEIPT_LIFETIME: u64 = 7 * 24 * 60 * 60;

/// Which orchestrator a wake is for: its worker's project's, or the global one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeScope {
    Project,
    Global,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeState {
    /// Waiting for its orchestrator to be idle.
    Pending,
    /// Taken for delivery. One whose delivery never finished stays here.
    Claimed,
    Delivered,
    /// Sent, but not proven received. Never retried.
    Uncertain,
    /// The chat host refused it; nothing was delivered.
    Failed,
    /// The scope has no orchestrator, or the shell no longer is a worker.
    Skipped,
    /// The orchestrator stayed busy for `WAKE_LIFETIME`.
    Expired,
    /// The project's wakes were turned off first.
    Cancelled,
}

#[derive(Default, Serialize, Deserialize)]
struct Ledger {
    #[serde(default)]
    entries: Vec<Entry>,
    /// What each orchestrator showed when it was last woken.
    #[serde(default)]
    guards: Vec<Guard>,
}

#[derive(Default, Deserialize)]
struct RawLedger {
    #[serde(default)]
    entries: Vec<serde_json::Value>,
    #[serde(default)]
    guards: Vec<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Entry {
    wake_id: String,
    provider: HarnessKind,
    shell_id: String,
    project_id: String,
    scope: WakeScope,
    event_id: String,
    created_at: u64,
    state: WakeState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    orchestrator_id: Option<String>,
}

/// The next wake of `orchestrator_id` waits for a turn it finished after this:
/// for a chat `log:MARK`, its log's length before the last wake went in; for a
/// terminal its idle token then. Older than `WAKE_LIFETIME`, it is dropped: an
/// idle report that late is the agent's own.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Guard {
    orchestrator_id: String,
    token: String,
    at: u64,
}

/// Queue a wake for each orchestrator scope of a verified completion
/// (`notifications::record_completion`). Only a worker harness shell of a
/// project whose wakes are on queues any; a replayed event queues none. The
/// number of wakes queued.
pub(crate) fn record_at(
    home: &Path,
    shell_id: &str,
    event_id: &str,
    provider: HarnessKind,
    time: u64,
) -> Result<usize, String> {
    if !valid_completion(shell_id, event_id) {
        return Err("Invalid agent completion identity".into());
    }
    let shell = SessionManager::at(home.to_path_buf())?.registered_session(shell_id)?;
    if shell.harness != Some(provider) || !shell.is_worker() {
        return Ok(0);
    }
    let Some(project_id) = shell.project_id.as_deref() else {
        return Ok(0);
    };
    if !canonical_uuid(project_id) {
        return Err("Invalid agent project identity".into());
    }
    let state = Store::open(home.to_path_buf())?.snapshot()?;
    if !state
        .project(project_id)
        .is_ok_and(|project| project.wake_orchestrators_on_agent_done)
    {
        return Ok(0);
    }
    let _lock = lock(home, LOCK)?;
    let mut ledger = read(home)?;
    prune(&mut ledger, time);
    let mut queued = 0;
    for scope in [WakeScope::Project, WakeScope::Global] {
        if ledger.entries.iter().any(|entry| {
            entry.scope == scope
                && entry.provider == provider
                && entry.shell_id == shell_id
                && entry.event_id == event_id
        }) {
            continue;
        }
        ledger.entries.push(Entry {
            wake_id: Uuid::new_v4().to_string(),
            provider,
            shell_id: shell_id.into(),
            project_id: project_id.into(),
            scope,
            event_id: event_id.into(),
            created_at: time,
            state: WakeState::Pending,
            orchestrator_id: None,
        });
        queued += 1;
    }
    if queued == 0 {
        return Ok(0);
    }
    if ledger.entries.len() > MAX_ENTRIES {
        let excess = ledger.entries.len() - MAX_ENTRIES;
        ledger.entries.drain(..excess);
    }
    write(home, &ledger)?;
    Ok(queued)
}

/// Turning a project's wakes off cancels the ones still waiting, so turning
/// them on again never replays old work.
pub fn cancel_pending(home: &Path, project_id: &str) -> Result<(), String> {
    if !home.join(LEDGER).exists() {
        return Ok(());
    }
    let _lock = lock(home, LOCK)?;
    let mut ledger = read(home)?;
    let mut changed = false;
    for entry in &mut ledger.entries {
        if entry.project_id == project_id && entry.state == WakeState::Pending {
            entry.state = WakeState::Cancelled;
            changed = true;
        }
    }
    if changed {
        write(home, &ledger)?;
    }
    Ok(())
}

/// Delivers wakes for the desktop app, which ticks it every second off its
/// main thread. Codex terminal orchestrators keep their rollout readers from
/// one tick to the next.
pub struct Runner {
    home: PathBuf,
    trackers: HashMap<String, ActivityTracker>,
}

impl Runner {
    pub fn at(home: PathBuf) -> Self {
        Self {
            home,
            trackers: HashMap::new(),
        }
    }

    pub fn tick(&mut self) -> Result<(), String> {
        let host = ChatHost {
            home: &self.home,
            ensure: &running_host,
        };
        let mut terminals = TmuxTerminals {
            home: &self.home,
            trackers: &mut self.trackers,
        };
        deliver_at(&self.home, &host, Proof::DEFAULT, &mut terminals, now())
    }
}

/// The chat host's socket when one runs. A wake never starts a host: an
/// orchestrator chat without one is stopped, not idle.
fn running_host(home: &Path) -> Result<PathBuf, String> {
    let socket = socket_path(home);
    std::os::unix::net::UnixStream::connect(&socket)
        .map(|_| socket)
        .map_err(|_| "The chat host is not running".to_owned())
}

/// The terminal side of delivery: whether a terminal orchestrator runs, its
/// idle evidence, and its pane. Tests stand in for tmux here.
pub(crate) trait Terminals {
    fn live(&mut self, shell: &ShellSession) -> bool;
    /// A token for the last turn the orchestrator completed, while it is idle;
    /// `None` while it works or when that cannot be told.
    fn idle_token(&mut self, shell: &ShellSession) -> Option<String>;
    /// Types `text` and one Return into the orchestrator's pane once its screen
    /// shows the empty prompt and its idle token still is `token`. `claim` runs
    /// last, under the pane's input lock: nothing is typed unless it says yes.
    fn submit(
        &mut self,
        shell: &ShellSession,
        text: &str,
        token: &str,
        claim: &mut dyn FnMut() -> Result<bool, String>,
    ) -> Submission;
}

pub(crate) enum Submission {
    /// Nothing was typed: not idle after all, or the claim said no.
    NotSent,
    Sent,
    /// Claimed, then typing failed: some of it may be in the pane.
    Partial,
}

/// RiWork's terminal orchestrator is a Codex, idle as a schedule sees it: its
/// rollout's completed turn, the pane's Codex holding that rollout open, and
/// the empty prompt on a screen that holds still.
/// A custom orchestrator command has no harness and so no idle evidence.
struct TmuxTerminals<'a> {
    home: &'a Path,
    trackers: &'a mut HashMap<String, ActivityTracker>,
}

fn codex_idle_token(
    home: &Path,
    trackers: &mut HashMap<String, ActivityTracker>,
    shell: &ShellSession,
) -> Option<String> {
    if shell.harness != Some(HarnessKind::Codex) {
        return None;
    }
    let tracker = trackers
        .entry(shell.id.clone())
        .or_insert_with(|| ActivityTracker::at(home.to_path_buf()));
    let conversation = tracker.schedule_identity(shell)?;
    let turn = tracker.schedule_idle_token(shell, &conversation)?;
    Some(format!("{conversation}:{turn}"))
}

impl Terminals for TmuxTerminals<'_> {
    fn live(&mut self, shell: &ShellSession) -> bool {
        shell.alive
    }

    fn idle_token(&mut self, shell: &ShellSession) -> Option<String> {
        codex_idle_token(self.home, self.trackers, shell)
    }

    fn submit(
        &mut self,
        shell: &ShellSession,
        text: &str,
        token: &str,
        claim: &mut dyn FnMut() -> Result<bool, String>,
    ) -> Submission {
        let Ok(manager) = SessionManager::at(self.home.to_path_buf()) else {
            return Submission::NotSent;
        };
        let (home, trackers) = (self.home, &mut *self.trackers);
        let mut claimed = false;
        let sent = manager.send_at_settled_prompt(shell, text, || {
            if codex_idle_token(home, trackers, shell).as_deref() != Some(token) {
                return Err("The orchestrator is no longer idle".into());
            }
            // As a schedule proves it: the pane's own Codex holds that
            // conversation's rollout open, so the idle turn is this pane's.
            let conversation = token.split_once(':').map(|(conversation, _)| conversation);
            if manager.schedule_provider_identity(shell).ok().as_deref() != conversation {
                return Err("The orchestrator's pane is not in its bound conversation".into());
            }
            if !claim()? {
                return Err("The wake was taken or cancelled".into());
            }
            claimed = true;
            Ok(())
        });
        match sent {
            Ok(()) => Submission::Sent,
            Err(_) if claimed => Submission::Partial,
            Err(_) => Submission::NotSent,
        }
    }
}

/// One delivery pass at `time`. Waiting wakes that can no longer go anywhere
/// are settled first; then each scope's oldest waiting wake goes to its
/// orchestrator if that one is idle.
pub(crate) fn deliver_at(
    home: &Path,
    host: &ChatHost,
    proof: Proof,
    terminals: &mut dyn Terminals,
    time: u64,
) -> Result<(), String> {
    if !home.join(LEDGER).exists() {
        return Ok(());
    }
    let delivering = private_file(&home.join(DELIVERY_LOCK))?;
    if delivering.try_lock_exclusive().is_err() {
        // Another RiWork process is delivering now.
        return Ok(());
    }
    if !has_pending(home)? {
        return Ok(());
    }
    // A pass can wait seconds per scope (a chat's proof, a pane's lock): claims
    // are checked against the time they are taken at.
    let started = std::time::Instant::now();
    let clock = || time + started.elapsed().as_secs();
    let state = Store::open(home.to_path_buf())?.snapshot()?;
    let sessions = SessionManager::at(home.to_path_buf())?;
    let (waiting, guards) = settle_waiting(home, &state, &sessions, time)?;
    let mut scopes: Vec<(OrchestratorScope, Vec<(Entry, ShellSession)>)> = Vec::new();
    for (entry, shell) in waiting {
        let scope = orchestrators::scope_of(match entry.scope {
            WakeScope::Project => Some(entry.project_id.as_str()),
            WakeScope::Global => None,
        });
        match scopes.iter_mut().find(|(known, _)| *known == scope) {
            Some((_, wakes)) => wakes.push((entry, shell)),
            None => scopes.push((scope, vec![(entry, shell)])),
        }
    }
    let mut first_error = None;
    for (scope, wakes) in scopes {
        let orchestrator = match orchestrators::find(&sessions, host, &scope) {
            Ok(orchestrator) => orchestrator,
            Err(error) => {
                first_error.get_or_insert(error);
                continue;
            }
        };
        let (entry, shell) = &wakes[0];
        let text = message(&state, entry, shell);
        let result = match orchestrator {
            Some(Orchestrator::Chat(chat)) => {
                let guard = guards.iter().find(|guard| guard.orchestrator_id == chat.id);
                deliver_chat(home, host, proof, guard, entry, &chat, &text, &clock)
            }
            Some(Orchestrator::Terminal(orchestrator)) if terminals.live(&orchestrator) => {
                let guard = guards
                    .iter()
                    .find(|guard| guard.orchestrator_id == orchestrator.id);
                deliver_terminal(home, terminals, guard, entry, &orchestrator, &text, &clock)
            }
            // No orchestrator is created to deliver; the scope's wakes are dropped.
            _ => skip(home, wakes.iter().map(|(entry, _)| entry.wake_id.as_str())),
        };
        if let Err(error) = result {
            first_error.get_or_insert(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

#[allow(clippy::too_many_arguments)]
fn deliver_chat(
    home: &Path,
    host: &ChatHost,
    proof: Proof,
    guard: Option<&Guard>,
    entry: &Entry,
    chat: &ChatInfo,
    text: &str,
    clock: &dyn Fn() -> u64,
) -> Result<(), String> {
    // Without a running host the chat is stopped: the wake waits, and expires.
    let Ok(mut client) = host.connect() else {
        return Ok(());
    };
    // The state the lookup saw may be old: ask the host again just before sending.
    let current = client
        .list()?
        .into_iter()
        .find(|listed| listed.id == chat.id && listed.orchestrator == chat.orchestrator);
    if current.is_none_or(|chat| chat.state != ChatState::Idle) {
        return Ok(());
    }
    if guard.is_some_and(|guard| !chat_turn_since(home, &chat.id, &guard.token)) {
        return Ok(());
    }
    let mark = chat::log::mark(home, &chat.id)?;
    if !claim(
        home,
        &entry.wake_id,
        &chat.id,
        &format!("log:{mark}"),
        clock(),
    )? {
        return Ok(());
    }
    let sent = client.command_checked(
        &chat.id,
        ChatCommand::Send {
            text: text.to_owned(),
        },
    );
    let outcome = match sent {
        Ok(()) if seen_in_log(host, &chat.id, mark, text, proof) => WakeState::Delivered,
        Ok(()) | Err(CallError::Broken(_)) => WakeState::Uncertain,
        Err(CallError::Refused(_)) => WakeState::Failed,
    };
    finish(home, &entry.wake_id, outcome)
}

/// Whether the chat finished a turn after the log was `token` long. A log that
/// is shorter now was replaced, and holds nothing to wait for.
fn chat_turn_since(home: &Path, chat_id: &str, token: &str) -> bool {
    let Some(mark) = token
        .strip_prefix("log:")
        .and_then(|mark| mark.parse::<u64>().ok())
    else {
        return true;
    };
    if chat::log::mark(home, chat_id).is_ok_and(|length| length < mark) {
        return true;
    }
    chat::log::read_after(home, chat_id, mark).is_ok_and(|(events, _)| {
        events
            .iter()
            .any(|envelope| matches!(envelope.event, ChatEvent::TurnCompleted { .. }))
    })
}

fn deliver_terminal(
    home: &Path,
    terminals: &mut dyn Terminals,
    guard: Option<&Guard>,
    entry: &Entry,
    orchestrator: &ShellSession,
    text: &str,
    clock: &dyn Fn() -> u64,
) -> Result<(), String> {
    let Some(token) = terminals.idle_token(orchestrator) else {
        return Ok(());
    };
    if guard.is_some_and(|guard| guard.token == token) {
        // It has not finished a turn since the last wake was typed in.
        return Ok(());
    }
    let mut claim_error = None;
    let mut take = || {
        claim(home, &entry.wake_id, &orchestrator.id, &token, clock()).inspect_err(|error| {
            claim_error = Some(error.clone());
        })
    };
    let outcome = match terminals.submit(orchestrator, text, &token, &mut take) {
        Submission::NotSent => return claim_error.map_or(Ok(()), Err),
        Submission::Sent => WakeState::Delivered,
        Submission::Partial => WakeState::Uncertain,
    };
    finish(home, &entry.wake_id, outcome)
}

/// The check-in an orchestrator gets: one line of identifiers, and where to
/// look. Names are the ones the completion alert shows.
fn message(state: &State, entry: &Entry, shell: &ShellSession) -> String {
    let project = state
        .project(&entry.project_id)
        .map(|project| one_line(&project.name))
        .unwrap_or_default();
    let worktree = shell.worktree_id.as_ref().and_then(|id| {
        state
            .worktrees
            .iter()
            .find(|worktree| &worktree.id == id && worktree.project_id == entry.project_id)
            .filter(|worktree| canonical_uuid(&worktree.id))
    });
    let (place, tasks) = match worktree {
        Some(worktree) => (
            format!(
                ", worktree {} ({})",
                one_line(&worktree.branch),
                worktree.id
            ),
            format!("riwork worktree tasks {}", worktree.id),
        ),
        None => (
            String::new(),
            format!("riwork project tasks {}", entry.project_id),
        ),
    };
    format!(
        "RiWork check-in: worker shell {shell} ({harness}) finished a turn in project {project} ({project_id}){place}. \
         Read its output with `riwork shell output {shell} --lines 120` and its task state with `{tasks}`, then decide its next step.",
        shell = entry.shell_id,
        harness = entry.provider.program(),
        project_id = entry.project_id,
    )
}

/// A name as one physical line: no control characters or line separators.
fn one_line(value: &str) -> String {
    compact_label(value)
        .chars()
        .filter(|character| !matches!(character, '\u{2028}' | '\u{2029}'))
        .collect()
}

fn has_pending(home: &Path) -> Result<bool, String> {
    let _lock = lock(home, LOCK)?;
    Ok(read(home)?
        .entries
        .iter()
        .any(|entry| entry.state == WakeState::Pending))
}

/// Settles the waiting wakes that can no longer be delivered, and returns the
/// rest, oldest first, with their shells, and the orchestrators' guards.
fn settle_waiting(
    home: &Path,
    state: &State,
    sessions: &SessionManager,
    time: u64,
) -> Result<(Vec<(Entry, ShellSession)>, Vec<Guard>), String> {
    let shells = sessions.registered_sessions()?;
    let _lock = lock(home, LOCK)?;
    let mut ledger = read(home)?;
    let mut changed = prune(&mut ledger, time);
    let mut waiting = Vec::new();
    for entry in &mut ledger.entries {
        if entry.state != WakeState::Pending {
            continue;
        }
        let shell = shells.iter().find(|shell| {
            shell.id == entry.shell_id
                && shell.is_worker()
                && shell.harness == Some(entry.provider)
                && shell.project_id.as_deref() == Some(entry.project_id.as_str())
        });
        let settled = if time.saturating_sub(entry.created_at) > WAKE_LIFETIME {
            WakeState::Expired
        } else if !state
            .project(&entry.project_id)
            .is_ok_and(|project| project.wake_orchestrators_on_agent_done)
        {
            WakeState::Cancelled
        } else if let Some(shell) = shell {
            waiting.push((entry.clone(), shell.clone()));
            continue;
        } else {
            WakeState::Skipped
        };
        entry.state = settled;
        changed = true;
    }
    if changed {
        write(home, &ledger)?;
    }
    Ok((waiting, ledger.guards))
}

/// Takes a waiting wake for delivery to `orchestrator_id`, and notes the
/// orchestrator's `token`; false when it no longer waits. The project's
/// setting is read again here, last: a toggle that lost the race with a
/// pass's earlier look still wins over the delivery.
fn claim(
    home: &Path,
    wake_id: &str,
    orchestrator_id: &str,
    token: &str,
    time: u64,
) -> Result<bool, String> {
    let _lock = lock(home, LOCK)?;
    let mut ledger = read(home)?;
    let Some(entry) = ledger
        .entries
        .iter_mut()
        .find(|entry| entry.wake_id == wake_id && entry.state == WakeState::Pending)
    else {
        return Ok(false);
    };
    if time.saturating_sub(entry.created_at) > WAKE_LIFETIME {
        return Ok(false);
    }
    if !Store::open(home.to_path_buf())?
        .snapshot()?
        .project(&entry.project_id)
        .is_ok_and(|project| project.wake_orchestrators_on_agent_done)
    {
        entry.state = WakeState::Cancelled;
        write(home, &ledger)?;
        return Ok(false);
    }
    entry.state = WakeState::Claimed;
    entry.orchestrator_id = Some(orchestrator_id.into());
    ledger
        .guards
        .retain(|guard| guard.orchestrator_id != orchestrator_id);
    ledger.guards.push(Guard {
        orchestrator_id: orchestrator_id.into(),
        token: token.into(),
        at: time,
    });
    if ledger.guards.len() > MAX_GUARDS {
        ledger.guards.remove(0);
    }
    write(home, &ledger)?;
    Ok(true)
}

/// Records how a claimed wake's delivery ended. One the host refused put
/// nothing in, so its orchestrator has no turn to finish first.
fn finish(home: &Path, wake_id: &str, outcome: WakeState) -> Result<(), String> {
    let _lock = lock(home, LOCK)?;
    let mut ledger = read(home)?;
    let Some(entry) = ledger
        .entries
        .iter_mut()
        .find(|entry| entry.wake_id == wake_id && entry.state == WakeState::Claimed)
    else {
        return Ok(());
    };
    entry.state = outcome;
    if outcome == WakeState::Failed {
        let orchestrator = entry.orchestrator_id.clone();
        ledger
            .guards
            .retain(|guard| Some(&guard.orchestrator_id) != orchestrator.as_ref());
    }
    write(home, &ledger)
}

fn skip<'a>(home: &Path, wake_ids: impl Iterator<Item = &'a str>) -> Result<(), String> {
    let wake_ids: Vec<_> = wake_ids.collect();
    let _lock = lock(home, LOCK)?;
    let mut ledger = read(home)?;
    let mut changed = false;
    for entry in &mut ledger.entries {
        if entry.state == WakeState::Pending && wake_ids.contains(&entry.wake_id.as_str()) {
            entry.state = WakeState::Skipped;
            changed = true;
        }
    }
    if changed {
        write(home, &ledger)?;
    }
    Ok(())
}

/// Drops old receipts and stale guards; true when anything went.
fn prune(ledger: &mut Ledger, time: u64) -> bool {
    let before = (ledger.entries.len(), ledger.guards.len());
    ledger
        .entries
        .retain(|entry| time.saturating_sub(entry.created_at) <= RECEIPT_LIFETIME);
    ledger
        .guards
        .retain(|guard| time.saturating_sub(guard.at) <= WAKE_LIFETIME);
    before != (ledger.entries.len(), ledger.guards.len())
}

fn private_file(path: &Path) -> Result<File, String> {
    reject_symlink(path)?;
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|_| "Cannot open the orchestrator wake lock".to_owned())
}

fn lock(home: &Path, name: &str) -> Result<File, String> {
    let file = private_file(&home.join(name))?;
    file.lock_exclusive()
        .map_err(|_| "Cannot lock orchestrator wakes")?;
    Ok(file)
}

fn read(home: &Path) -> Result<Ledger, String> {
    let path = home.join(LEDGER);
    reject_symlink(&path)?;
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Ledger::default()),
        Err(_) => return Err("Cannot read orchestrator wakes".into()),
    };
    if file
        .metadata()
        .map_err(|_| "Cannot inspect orchestrator wakes")?
        .len()
        > MAX_BYTES
    {
        return Err("Orchestrator wake metadata is too large".into());
    }
    // As in the alert ledger, a damaged or foreign entry is dropped on its own.
    let raw: RawLedger = serde_json::from_reader(file.take(MAX_BYTES))
        .map_err(|_| "Invalid orchestrator wake metadata")?;
    let mut entries: Vec<Entry> = raw
        .entries
        .into_iter()
        .filter_map(|value| serde_json::from_value::<Entry>(value).ok())
        .filter(valid_entry)
        .collect();
    if entries.len() > MAX_ENTRIES {
        entries.drain(..entries.len() - MAX_ENTRIES);
    }
    let guards = raw
        .guards
        .into_iter()
        .filter_map(|value| serde_json::from_value::<Guard>(value).ok())
        .filter(|guard| canonical_uuid(&guard.orchestrator_id) && guard.token.len() <= 1024)
        .collect();
    Ok(Ledger { entries, guards })
}

fn valid_entry(entry: &Entry) -> bool {
    canonical_uuid(&entry.wake_id)
        && canonical_uuid(&entry.project_id)
        && valid_completion(&entry.shell_id, &entry.event_id)
        && entry.orchestrator_id.as_deref().is_none_or(canonical_uuid)
}

fn write(home: &Path, ledger: &Ledger) -> Result<(), String> {
    let path = home.join(LEDGER);
    reject_symlink(&path)?;
    let temporary = home.join(format!(".orchestrator-wakes-{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|_| "Cannot stage orchestrator wakes")?;
        serde_json::to_writer(&mut file, ledger).map_err(|_| "Cannot encode orchestrator wakes")?;
        file.write_all(b"\n")
            .and_then(|_| file.sync_all())
            .map_err(|_| "Cannot save orchestrator wakes")?;
        fs::rename(&temporary, path).map_err(|_| "Cannot install orchestrator wakes")
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result.map_err(str::to_owned)
}

/// Each wake's scope, state and orchestrator, oldest first, for tests.
#[cfg(test)]
pub(crate) fn states(home: &Path) -> Vec<(WakeScope, WakeState, Option<String>)> {
    read(home)
        .unwrap()
        .entries
        .into_iter()
        .map(|entry| (entry.scope, entry.state, entry.orchestrator_id))
        .collect()
}

#[cfg(test)]
mod tests;
