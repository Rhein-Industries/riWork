//! RiWork's own workers in the timeline of the chat that started them.
//!
//! A worker is a terminal (`riwork shell create`) or a chat (`riwork chat new`) whose
//! `parent_id` is a chat. The chat host, the only writer of a chat's log, puts one marker
//! item into the parent chat when the worker is started or given a later assignment, and
//! replaces that same item with a bounded final snapshot when the worker finishes. What is
//! live (status, elapsed time, the screen) comes from the inventory by `session_id`, and
//! is never logged.
//!
//! A marker is an ordinary `tool_call` (server `riwork`, tool `worker`) with an additive
//! `presentation.worker`, so a reader from before draws a plain tool row. Its status is
//! always `completed`: `TurnCompleted` closes what is in progress, and a worker outlives
//! the turn that started it.
use super::model::{
    ChatEvent, ChatInfo, ChatState, Delta, Item, ItemBody, ItemStatus, MessagePhase, Presentation,
    TurnOutcome,
};
use serde::{Deserialize, Deserializer, Serialize};
use std::path::Path;

pub const SERVER: &str = "riwork";
pub const TOOL: &str = "worker";
const ID_PREFIX: &str = "worker:";
pub const TEXT_CHARS: usize = 240;
/// A marker the inventory has not seen yet reads as starting for this long.
pub const STARTING_GRACE: u64 = 30;
/// Reconciliation leaves a worker younger than this to its spawner's own note.
pub const LATE_AFTER: u64 = 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerKind {
    Shell,
    Chat,
    #[serde(other)]
    Unknown,
}

/// How a worker's assignment ended, as recorded in the parent chat.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerEnd {
    Done,
    Exited,
    Failed,
    Stopped,
    #[serde(other)]
    Unknown,
}
impl WorkerEnd {
    pub fn label(self) -> &'static str {
        match self {
            Self::Done => "Done",
            Self::Exited => "Exited",
            Self::Failed => "Failed",
            Self::Stopped => "Stopped",
            Self::Unknown => "Status not reported",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerFinal {
    pub status: WorkerEnd,
    pub ended_at: u64,
    /// The last lines the worker showed, bounded by `subshells::bounded_output`.
    #[serde(default)]
    pub tail: String,
    /// What ended it, so that the same end is recorded once: a worker chat's turn, and
    /// where that turn's end is in the worker's log (a provider may use a turn id again)...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_end: Option<u64>,
    /// ...or when a terminal's agent became done.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_since: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerRef {
    pub session_id: String,
    pub kind: WorkerKind,
    #[serde(default)]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<String>,
    /// The first line of what the parent sent, when it sent something.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignment: Option<String>,
    pub noted_at: u64,
    /// A worker chat's next log position when the card was written: the card's message
    /// is at or after it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<u64>,
    /// Written by reconciliation, after the start was missed: placed where it was seen.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub late: bool,
    /// A later assignment of a worker this chat already had.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub follow_up: bool,
    #[serde(rename = "final", default, skip_serializing_if = "Option::is_none")]
    pub end: Option<WorkerFinal>,
}

/// A marker a reader cannot understand is a plain tool row, never an unreadable item.
pub(super) fn lenient<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<WorkerRef>, D::Error> {
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|value| serde_json::from_value(value).ok()))
}

/// What a client tells the host about one of a chat's workers (`Request::NoteWorker`).
/// The host checks it against the inventory and describes the worker itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "note", rename_all = "snake_case")]
pub enum WorkerNote {
    /// The worker was just started, or (`late`) is first seen now.
    Spawn {
        session_id: String,
        kind: WorkerKind,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        late: bool,
    },
    /// The parent sent the worker `text`.
    Assign {
        session_id: String,
        kind: WorkerKind,
        text: String,
    },
    /// The worker finished an assignment. `done_since` is when a terminal's activity says
    /// it became done; a worker chat's end names its `turn`, where that turn ended in its
    /// log (`turn_end`), and the messages it `took`. Either says which cards the end is for
    /// (`apply`).
    Finish {
        session_id: String,
        kind: WorkerKind,
        end: WorkerFinal,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        done_since: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_end: Option<u64>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        took: Vec<Took>,
    },
}

/// A message a worker chat's turn took: its place in that chat's log and its first line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Took {
    pub seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<String>,
}
impl WorkerNote {
    pub fn session_id(&self) -> &str {
        match self {
            Self::Spawn { session_id, .. }
            | Self::Assign { session_id, .. }
            | Self::Finish { session_id, .. } => session_id,
        }
    }
    pub fn kind(&self) -> WorkerKind {
        match self {
            Self::Spawn { kind, .. } | Self::Assign { kind, .. } | Self::Finish { kind, .. } => {
                *kind
            }
        }
    }
}

pub fn of(item: &Item) -> Option<&WorkerRef> {
    item.presentation.worker.as_ref()
}

/// Whether the host wrote `item` as a worker marker, readable by this build or not.
pub fn is_marker(item: &Item) -> bool {
    item.presentation.worker.is_some()
        || matches!(&item.body, ItemBody::ToolCall { server: Some(server), tool, .. }
            if server == SERVER && tool == TOOL)
}

fn bounded(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(TEXT_CHARS)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// The first nonblank line of an assignment.
pub fn assignment_line(text: &str) -> Option<String> {
    text.lines().map(bounded).find(|line| !line.is_empty())
}

pub fn describe_shell(
    shell: &crate::sessions::ShellSession,
    tasks: &[crate::store::Task],
    now: u64,
) -> WorkerRef {
    let card = crate::subshells::ShellCard::of(shell, None, tasks);
    WorkerRef {
        session_id: shell.id.clone(),
        kind: WorkerKind::Shell,
        title: bounded(&card.title()),
        harness: shell.harness.map(|harness| harness.program().to_owned()),
        model: None,
        worktree_id: shell.worktree_id.clone(),
        assignment: None,
        noted_at: now,
        after: None,
        late: false,
        follow_up: false,
        end: None,
    }
}

pub fn describe_chat(chat: &ChatInfo, now: u64) -> WorkerRef {
    WorkerRef {
        session_id: chat.id.clone(),
        kind: WorkerKind::Chat,
        title: bounded(&chat.title),
        harness: Some(provider_name(chat.provider).to_owned()),
        model: chat.model.as_deref().map(bounded),
        worktree_id: chat.worktree_id.clone(),
        assignment: None,
        noted_at: now,
        after: None,
        late: false,
        follow_up: false,
        end: None,
    }
}

fn provider_name(provider: super::model::Provider) -> &'static str {
    match provider {
        super::model::Provider::Codex => "codex",
        super::model::Provider::Claude => "claude",
        super::model::Provider::Muse => "muse",
    }
}

/// The marker item for `worker` with item id `id`.
pub fn item(id: String, turn_id: Option<String>, worker: WorkerRef) -> Item {
    let output = worker
        .end
        .as_ref()
        .map(|end| end.tail.clone())
        .filter(|tail| !tail.is_empty());
    Item {
        body: ItemBody::ToolCall {
            server: Some(SERVER.to_owned()),
            tool: TOOL.to_owned(),
            input: serde_json::json!({
                "session_id": worker.session_id,
                "title": worker.title,
            }),
            output,
        },
        presentation: Presentation {
            worker: Some(worker),
            ..Default::default()
        },
        id,
        turn_id,
        status: ItemStatus::Completed,
    }
}

fn next_id(session_id: &str, markers: &[Item]) -> String {
    let first = format!("{ID_PREFIX}{session_id}");
    if markers.is_empty() {
        return first;
    }
    (markers.len() + 1..)
        .map(|n| format!("{first}:{n}"))
        .find(|id| markers.iter().all(|marker| &marker.id != id))
        .unwrap_or(first)
}

#[derive(Clone, Debug, PartialEq)]
pub enum Upsert {
    Append(Item),
    /// Cards rewritten in place, oldest first.
    Replace(Vec<Item>),
    Nothing,
}

/// What the host writes for `note`, given the session's markers in the chat (oldest
/// first), a fresh description of the worker, the chat's open turn and the time.
///
/// - A start is written once; a later start (a late sighting) of a known worker is nothing.
/// - The first assignment joins the start's card when it comes in the same turn, so a
///   `shell create` and its first `shell send` make one card; any other assignment is a
///   new card below.
/// - An end is recorded once: one said again (a restart, another window) is nothing.
/// - A worker chat's turn ends the cards of the messages it took (`took_by`), all of them
///   when messages were steered into one turn; a turn that took none of its cards' messages
///   ends nothing.
/// - A terminal's done ends the latest open card it follows (`done_by`).
/// - An exit ends the latest card, if it is still open.
pub fn apply(
    markers: &[Item],
    note: &WorkerNote,
    fresh: WorkerRef,
    turn: Option<&str>,
    now: u64,
) -> Upsert {
    let last = markers.last();
    match note {
        WorkerNote::Spawn {
            session_id, late, ..
        } => {
            if !markers.is_empty() {
                return Upsert::Nothing;
            }
            Upsert::Append(item(
                next_id(session_id, markers),
                turn.map(str::to_owned),
                WorkerRef {
                    late: *late,
                    noted_at: now,
                    ..fresh
                },
            ))
        }
        WorkerNote::Assign {
            session_id, text, ..
        } => {
            let line = assignment_line(text);
            if let Some(open) = last.filter(|marker| {
                turn.is_some()
                    && marker.turn_id.as_deref() == turn
                    && of(marker)
                        .is_some_and(|worker| worker.end.is_none() && worker.assignment.is_none())
            }) {
                let mut marker = open.clone();
                if let Some(worker) = marker.presentation.worker.as_mut() {
                    worker.assignment = line.or(worker.assignment.take());
                }
                return if &marker == open {
                    Upsert::Nothing
                } else {
                    Upsert::Replace(vec![marker])
                };
            }
            Upsert::Append(item(
                next_id(session_id, markers),
                turn.map(str::to_owned),
                WorkerRef {
                    assignment: line,
                    follow_up: !markers.is_empty(),
                    noted_at: now,
                    ..fresh
                },
            ))
        }
        WorkerNote::Finish {
            end,
            done_since,
            turn,
            turn_end,
            took,
            ..
        } => {
            let targets: Vec<&Item> = if end.status == WorkerEnd::Exited {
                last.filter(|marker| is_open(marker)).into_iter().collect()
            } else if turn.is_some() {
                took_by(markers, took, *turn_end)
            } else {
                done_by(markers, *done_since).into_iter().collect()
            };
            if targets.is_empty() {
                return Upsert::Nothing;
            }
            Upsert::Replace(
                targets
                    .into_iter()
                    .map(|open| {
                        let mut worker = of(open).cloned().expect("an open marker");
                        worker.end = Some(WorkerFinal {
                            status: end.status,
                            ended_at: end.ended_at,
                            tail: crate::subshells::bounded_output(&end.tail),
                            turn: turn.clone(),
                            turn_end: *turn_end,
                            done_since: *done_since,
                        });
                        item(open.id.clone(), open.turn_id.clone(), worker)
                    })
                    .collect(),
            )
        }
    }
}

fn is_open(marker: &Item) -> bool {
    of(marker).is_some_and(|worker| worker.end.is_none())
}

/// The open cards a worker chat's turn ended. Each message it took is the message of the
/// latest card written before it with the same first line that is open or already has this
/// end (`ended`, the end's place in the worker's log): said again, after a restart or a
/// crash between two cards, an end finds the cards it ended and writes only those still
/// open. A card that was given nothing takes the first message no card claims, while it is
/// the worker's latest card.
fn took_by<'a>(markers: &'a [Item], took: &[Took], ended: Option<u64>) -> Vec<&'a Item> {
    let ours = |worker: &WorkerRef| {
        worker
            .end
            .as_ref()
            .is_none_or(|end| ended.is_some() && end.turn_end == ended)
    };
    let mut claimed: Vec<usize> = Vec::new();
    let mut unclaimed = Vec::new();
    for message in took {
        let card = (0..markers.len()).rev().find(|at| {
            !claimed.contains(at)
                && of(&markers[*at]).is_some_and(|worker| {
                    ours(worker)
                        && worker.assignment.is_some()
                        && worker.assignment == message.line
                        && worker.after.unwrap_or(0) <= message.seq
                })
        });
        match card {
            Some(at) => claimed.push(at),
            None => unclaimed.push(message),
        }
    }
    if let (Some(message), Some(at)) = (unclaimed.first(), markers.len().checked_sub(1))
        && !claimed.contains(&at)
        && of(&markers[at]).is_some_and(|worker| {
            ours(worker) && worker.assignment.is_none() && worker.after.unwrap_or(0) <= message.seq
        })
    {
        claimed.push(at);
    }
    claimed.sort_unstable();
    claimed
        .into_iter()
        .map(|at| &markers[at])
        .filter(|card| is_open(card))
        .collect()
}

/// The open card a terminal's done ends, unless a card has this done already: the latest
/// it surely follows, else one written in its second, since both times are whole seconds.
/// A late card was written after its worker started, so it follows any done; a done with
/// no time ends the latest open card.
fn done_by(markers: &[Item], since: Option<u64>) -> Option<&Item> {
    let open = || markers.iter().rev().filter(|marker| is_open(marker));
    let Some(since) = since else {
        return open().next();
    };
    if markers
        .iter()
        .filter_map(|marker| of(marker)?.end.as_ref())
        .any(|end| end.done_since == Some(since))
    {
        return None;
    }
    let follows = |marker: &&Item, same: bool| {
        of(marker).is_some_and(|worker| {
            worker.late || since > worker.noted_at || same && since == worker.noted_at
        })
    };
    open()
        .find(|marker| follows(marker, false))
        .or_else(|| open().find(|marker| follows(marker, true)))
}

/// A worker chat's turn ended: how its card reads.
pub fn chat_end(outcome: &TurnOutcome, reply: Option<&str>, now: u64) -> WorkerFinal {
    WorkerFinal {
        status: match outcome {
            TurnOutcome::Completed => WorkerEnd::Done,
            TurnOutcome::Interrupted => WorkerEnd::Stopped,
            TurnOutcome::Failed { .. } => WorkerEnd::Failed,
        },
        ended_at: now,
        tail: reply
            .map(crate::subshells::bounded_output)
            .unwrap_or_default(),
        turn: None,
        turn_end: None,
        done_since: None,
    }
}

/// The most a worker chat's turn keeps of its messages and of its reply while it runs.
const TOOK_KEPT: usize = 64;
const REPLY_KEPT: usize = 64 * 1024;

/// What a worker chat's open turns took and said, followed through its events, for the
/// end its parent records. The host follows a running chat's events, and a stopped
/// chat's log after a restart.
#[derive(Debug, Default)]
pub struct TurnRecord {
    /// Messages of turns not over yet: the turn, the item, and the message.
    took: Vec<(String, String, Took)>,
    /// The turn's reply so far: the item and its text.
    reply: Option<(String, String)>,
    /// A reply item begun empty, whose text comes in deltas.
    streaming: Option<String>,
}

impl TurnRecord {
    /// Follows the worker chat's event at `seq`; at the end of a turn, its end.
    pub fn follow(
        &mut self,
        session_id: &str,
        seq: u64,
        event: &ChatEvent,
        now: u64,
    ) -> Option<WorkerNote> {
        match event {
            ChatEvent::TurnStarted { .. } => {
                self.reply = None;
                self.streaming = None;
            }
            ChatEvent::ItemStarted { item } | ChatEvent::ItemCompleted { item } => {
                match &item.body {
                    ItemBody::UserMessage { text } => {
                        if let Some(turn) = &item.turn_id
                            && !self.took.iter().any(|(_, id, _)| id == &item.id)
                        {
                            if self.took.len() == TOOK_KEPT {
                                self.took.remove(0);
                            }
                            self.took.push((
                                turn.clone(),
                                item.id.clone(),
                                Took {
                                    seq,
                                    line: assignment_line(text),
                                },
                            ));
                        }
                    }
                    ItemBody::AgentMessage { text }
                        if item.presentation.phase != Some(MessagePhase::Commentary) =>
                    {
                        if !text.trim().is_empty() {
                            self.reply = Some((item.id.clone(), kept(text.clone())));
                            self.streaming = None;
                        } else if matches!(event, ChatEvent::ItemStarted { .. }) {
                            self.streaming = Some(item.id.clone());
                        }
                    }
                    _ => {}
                }
            }
            ChatEvent::ItemDelta {
                item_id,
                delta: Delta::Text(more),
            } => match &mut self.reply {
                Some((id, text)) if id == item_id => {
                    text.push_str(more);
                    *text = kept(std::mem::take(text));
                }
                _ if self.streaming.as_ref() == Some(item_id) => {
                    self.reply = Some((item_id.clone(), kept(more.clone())));
                    self.streaming = None;
                }
                _ => {}
            },
            ChatEvent::TurnCompleted { turn_id, outcome } => {
                let took = self
                    .took
                    .iter()
                    .filter(|(turn, ..)| turn == turn_id)
                    .map(|(.., took)| took.clone())
                    .collect();
                self.took.retain(|(turn, ..)| turn != turn_id);
                self.streaming = None;
                let reply = self.reply.take().map(|(_, text)| text);
                return Some(WorkerNote::Finish {
                    session_id: session_id.to_owned(),
                    kind: WorkerKind::Chat,
                    end: chat_end(outcome, reply.as_deref(), now),
                    done_since: None,
                    turn: Some(turn_id.clone()),
                    turn_end: Some(seq),
                    took,
                });
            }
            _ => {}
        }
        None
    }
}

/// The end of `text`, at most `REPLY_KEPT` bytes: a card shows the last lines.
fn kept(mut text: String) -> String {
    if text.len() > REPLY_KEPT {
        let mut from = text.len() - REPLY_KEPT;
        while !text.is_char_boundary(from) {
            from += 1;
        }
        text.drain(..from);
    }
    text
}

/// The worker's state as the inventory reports it now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveStatus {
    Working,
    Waiting,
    Done,
    Exited,
    Stopped,
    Failed,
    /// Alive, but its activity is not reported.
    Unknown,
}
impl LiveStatus {
    pub fn of_shell(card: &crate::subshells::ShellCard) -> Self {
        use crate::activity::AgentActivity;
        match card.activity {
            _ if !card.alive => Self::Exited,
            AgentActivity::Working => Self::Working,
            AgentActivity::Waiting => Self::Waiting,
            AgentActivity::Done => Self::Done,
            AgentActivity::Exited => Self::Exited,
            AgentActivity::Unknown => Self::Unknown,
        }
    }
    pub fn of_chat(state: &ChatState) -> Self {
        match state {
            ChatState::Starting | ChatState::Running => Self::Working,
            ChatState::Waiting => Self::Waiting,
            ChatState::Idle => Self::Done,
            ChatState::Stopped => Self::Stopped,
            ChatState::Failed { .. } => Self::Failed,
        }
    }
}

/// What a client's inventory says about a worker now: drawn on its card, never logged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveWorker {
    pub status: LiveStatus,
    /// A live terminal that can be opened, messaged and stopped from the card.
    pub actionable: bool,
    pub model: Option<String>,
    /// The terminal's last lines (`subshells::bounded_output`), or nothing for a chat.
    pub output: String,
    pub output_error: Option<String>,
}

/// Whether `id` names a chat saved under `home`, so that a note about a worker whose
/// parent it is can be recorded there.
pub fn parent_is_chat(home: &Path, id: &str) -> bool {
    is_chat(home, id)
}

/// What a marker's card shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CardStatus {
    /// Noted, and not in the inventory yet.
    Starting,
    Live(LiveStatus),
    /// Gone from the inventory before its end was recorded.
    Ended,
    Finished(WorkerEnd),
    /// A later card of the same worker carries on below.
    Continued,
}
impl CardStatus {
    pub fn of(worker: &WorkerRef, superseded: bool, live: Option<LiveStatus>, now: u64) -> Self {
        if let Some(end) = &worker.end {
            return Self::Finished(end.status);
        }
        if superseded {
            return Self::Continued;
        }
        match live {
            Some(live) => Self::Live(live),
            None if now.saturating_sub(worker.noted_at) < STARTING_GRACE => Self::Starting,
            None => Self::Ended,
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            Self::Starting => "Starting",
            Self::Live(LiveStatus::Working) => "Working",
            Self::Live(LiveStatus::Waiting) => "Waiting for input",
            Self::Live(LiveStatus::Done) => "Done",
            Self::Live(LiveStatus::Exited) => "Exited",
            Self::Live(LiveStatus::Stopped) => "Stopped",
            Self::Live(LiveStatus::Failed) => "Failed",
            Self::Live(LiveStatus::Unknown) => "Unknown",
            Self::Ended => "Ended · details unavailable",
            Self::Finished(end) => end.label(),
            Self::Continued => "Continued below",
        }
    }
    /// Counted by the running-workers indicator, and drawn with the fixed live body.
    pub fn running(&self) -> bool {
        matches!(
            self,
            Self::Starting
                | Self::Live(LiveStatus::Working | LiveStatus::Waiting | LiveStatus::Unknown)
        )
    }
}

/// The index of each session's latest marker: only that card can be live.
pub fn latest(items: &[Item]) -> std::collections::HashMap<&str, usize> {
    items
        .iter()
        .enumerate()
        .filter_map(|(at, item)| of(item).map(|worker| (worker.session_id.as_str(), at)))
        .collect()
}

/// One line for what reads a chat as text (`text`, handoff documents).
pub fn summary(worker: &WorkerRef) -> String {
    let who = match (&worker.harness, worker.kind) {
        (Some(harness), WorkerKind::Chat) => format!("{harness} chat worker"),
        (Some(harness), _) => format!("{harness} worker"),
        (None, _) => "worker".to_owned(),
    };
    let mut line = format!(
        "{} {who} · {}",
        if worker.follow_up {
            "Follow-up for"
        } else {
            "Started"
        },
        worker.title
    );
    if let Some(assignment) = &worker.assignment {
        line.push_str(" · ");
        line.push_str(assignment);
    }
    if let Some(end) = &worker.end {
        line.push_str(" → ");
        line.push_str(end.status.label());
    }
    line
}

// ---- Telling the host -------------------------------------------------------------------------

/// Whether `id` names a saved chat (rather than a terminal or nothing).
fn is_chat(home: &Path, id: &str) -> bool {
    super::log::chat_dir(home, id).is_some_and(|dir| dir.join("info.json").is_file())
}

/// Best effort: a host that is not running, or is older than this note, changes nothing
/// the caller did. Never starts a host.
pub fn send(home: &Path, chat_id: &str, note: WorkerNote) -> Result<(), String> {
    let mut client = super::client::Client::connect(&super::client::socket_path(home))?;
    client
        .note_worker(chat_id, note)
        .map_err(|error| error.to_string())
}

/// After `riwork shell create` (CLI or MCP): the new shell's card in its parent chat.
pub fn note_spawned_shell(home: &Path, shell: &crate::sessions::ShellSession) {
    let Some(parent) = shell.parent_id.as_deref().filter(|id| is_chat(home, id)) else {
        return;
    };
    let _ = send(
        home,
        parent,
        WorkerNote::Spawn {
            session_id: shell.id.clone(),
            kind: WorkerKind::Shell,
            late: false,
        },
    );
}

/// A send to a worker: a card in its parent chat, only when that chat (the caller) sent it.
pub fn note_sent(
    home: &Path,
    parent: Option<&str>,
    session_id: &str,
    kind: WorkerKind,
    text: &str,
) {
    let caller = crate::project_tabs::caller_parent();
    if let Some((parent, note)) = sent_note(home, caller.as_deref(), parent, session_id, kind, text)
    {
        let _ = send(home, &parent, note);
    }
}

fn sent_note(
    home: &Path,
    caller: Option<&str>,
    parent: Option<&str>,
    session_id: &str,
    kind: WorkerKind,
    text: &str,
) -> Option<(String, WorkerNote)> {
    let parent = parent.filter(|id| is_chat(home, id) && caller == Some(*id))?;
    Some((
        parent.to_owned(),
        WorkerNote::Assign {
            session_id: session_id.to_owned(),
            kind,
            text: text.to_owned(),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh(session: &str) -> WorkerRef {
        WorkerRef {
            session_id: session.into(),
            kind: WorkerKind::Shell,
            title: "Fix polling".into(),
            harness: Some("claude".into()),
            model: None,
            worktree_id: Some("w".into()),
            assignment: None,
            noted_at: 0,
            after: None,
            late: false,
            follow_up: false,
            end: None,
        }
    }
    fn spawn(late: bool) -> WorkerNote {
        WorkerNote::Spawn {
            session_id: "s".into(),
            kind: WorkerKind::Shell,
            late,
        }
    }
    fn assign(text: &str) -> WorkerNote {
        WorkerNote::Assign {
            session_id: "s".into(),
            kind: WorkerKind::Shell,
            text: text.into(),
        }
    }
    fn finish(status: WorkerEnd, done_since: Option<u64>) -> WorkerNote {
        WorkerNote::Finish {
            session_id: "s".into(),
            kind: WorkerKind::Shell,
            end: WorkerFinal {
                status,
                ended_at: 500,
                tail: "line\n".repeat(20) + "last",
                turn: None,
                turn_end: None,
                done_since: None,
            },
            done_since,
            turn: None,
            turn_end: None,
            took: Vec::new(),
        }
    }
    fn appended(upsert: Upsert) -> Item {
        match upsert {
            Upsert::Append(item) => item,
            other => panic!("expected an append, got {other:?}"),
        }
    }
    fn replaced(upsert: Upsert) -> Item {
        match upsert {
            Upsert::Replace(items) if items.len() == 1 => items.into_iter().next().unwrap(),
            other => panic!("expected one replacement, got {other:?}"),
        }
    }

    #[test]
    fn a_start_is_written_once_completed_with_the_open_turn_and_a_plain_tool_body() {
        let marker = appended(apply(&[], &spawn(false), fresh("s"), Some("t1"), 100));
        assert_eq!(marker.id, "worker:s");
        assert_eq!(marker.turn_id.as_deref(), Some("t1"));
        assert_eq!(marker.status, ItemStatus::Completed);
        assert!(matches!(
            &marker.body,
            ItemBody::ToolCall { server: Some(server), tool, .. } if server == SERVER && tool == TOOL
        ));
        assert_eq!(of(&marker).unwrap().noted_at, 100);
        assert!(!of(&marker).unwrap().late);
        assert_eq!(
            apply(&[marker.clone()], &spawn(true), fresh("s"), None, 200),
            Upsert::Nothing,
            "a late sighting of a known worker writes nothing"
        );
        let late = appended(apply(&[], &spawn(true), fresh("s"), None, 200));
        assert!(of(&late).unwrap().late);
    }

    #[test]
    fn the_first_send_of_the_same_turn_joins_the_start_and_a_later_one_is_a_new_card() {
        let start = appended(apply(&[], &spawn(false), fresh("s"), Some("t1"), 100));
        let joined = replaced(apply(
            std::slice::from_ref(&start),
            &assign("\n  Fix the poller.\nDetails follow"),
            fresh("s"),
            Some("t1"),
            110,
        ));
        assert_eq!(joined.id, "worker:s");
        let worker = of(&joined).unwrap();
        assert_eq!(worker.assignment.as_deref(), Some("Fix the poller."));
        assert_eq!(worker.noted_at, 100, "the card counts from the start");
        assert!(!worker.follow_up);
        let later = appended(apply(
            std::slice::from_ref(&joined),
            &assign("Also update the docs"),
            fresh("s"),
            Some("t2"),
            900,
        ));
        assert_eq!(later.id, "worker:s:2");
        assert!(of(&later).unwrap().follow_up);
        assert_eq!(of(&later).unwrap().noted_at, 900);
        // A send while no turn is open never joins a card.
        let third = appended(apply(&[joined, later], &assign("x"), fresh("s"), None, 950));
        assert_eq!(third.id, "worker:s:3");
    }

    #[test]
    fn a_finish_completes_only_the_latest_open_card_and_not_with_an_earlier_done() {
        let start = appended(apply(&[], &spawn(false), fresh("s"), Some("t1"), 100));
        let markers = [start];
        assert_eq!(
            apply(
                &markers,
                &finish(WorkerEnd::Done, Some(99)),
                fresh("s"),
                None,
                500
            ),
            Upsert::Nothing,
            "a done from before the card is the previous assignment's"
        );
        let done = replaced(apply(
            &markers,
            &finish(WorkerEnd::Done, Some(101)),
            fresh("s"),
            None,
            500,
        ));
        let end = of(&done).unwrap().end.clone().unwrap();
        assert_eq!(end.status, WorkerEnd::Done);
        assert!(end.tail.lines().count() <= crate::subshells::OUTPUT_LINES);
        assert!(end.tail.ends_with("last"));
        assert!(
            matches!(&done.body, ItemBody::ToolCall { output: Some(tail), .. } if tail == &end.tail)
        );
        assert_eq!(
            apply(
                &[done],
                &finish(WorkerEnd::Exited, None),
                fresh("s"),
                None,
                600
            ),
            Upsert::Nothing,
            "an ended card keeps its result"
        );
        let exited = replaced(apply(
            &markers,
            &finish(WorkerEnd::Exited, Some(1)),
            fresh("s"),
            None,
            500,
        ));
        assert_eq!(
            of(&exited).unwrap().end.as_ref().unwrap().status,
            WorkerEnd::Exited
        );
        assert_eq!(
            apply(&[], &finish(WorkerEnd::Done, None), fresh("s"), None, 500),
            Upsert::Nothing
        );
    }

    #[test]
    fn markers_survive_turn_completion_and_upserts_keep_their_place() {
        use super::super::model::{ChatEvent, Transcript};
        let start = appended(apply(&[], &spawn(false), fresh("s"), Some("t1"), 100));
        let mut transcript = Transcript::default();
        transcript.apply(&ChatEvent::TurnStarted {
            turn_id: "t1".into(),
        });
        transcript.apply(&ChatEvent::ItemCompleted {
            item: start.clone(),
        });
        transcript.apply(&ChatEvent::ItemCompleted {
            item: Item {
                presentation: Default::default(),
                id: "after".into(),
                turn_id: Some("t1".into()),
                status: ItemStatus::Completed,
                body: ItemBody::AgentMessage { text: "ok".into() },
            },
        });
        transcript.apply(&ChatEvent::TurnCompleted {
            turn_id: "t1".into(),
            outcome: TurnOutcome::Interrupted,
        });
        assert_eq!(transcript.items[0].status, ItemStatus::Completed);
        let done = replaced(apply(
            &[start],
            &finish(WorkerEnd::Done, Some(200)),
            fresh("s"),
            None,
            500,
        ));
        transcript.apply(&ChatEvent::ItemCompleted { item: done });
        assert_eq!(transcript.items[0].id, "worker:s");
        assert!(of(&transcript.items[0]).unwrap().end.is_some());
        assert_eq!(transcript.items[1].id, "after");
    }

    #[test]
    fn a_second_send_in_the_same_turn_is_its_own_card_and_each_end_finds_its_card() {
        let start = appended(apply(&[], &spawn(false), fresh("s"), Some("t1"), 100));
        let first = replaced(apply(
            std::slice::from_ref(&start),
            &assign("Task one"),
            fresh("s"),
            Some("t1"),
            101,
        ));
        let second = appended(apply(
            std::slice::from_ref(&first),
            &assign("Task two"),
            fresh("s"),
            Some("t1"),
            200,
        ));
        assert_eq!(second.id, "worker:s:2");
        assert_eq!(of(&first).unwrap().assignment.as_deref(), Some("Task one"));
        let markers = [first, second];
        // Task one's done (before the second card) completes the first card, late or not.
        let one = replaced(apply(
            &markers,
            &finish(WorkerEnd::Done, Some(150)),
            fresh("s"),
            None,
            300,
        ));
        assert_eq!(one.id, "worker:s");
        let markers = [one, markers[1].clone()];
        let two = replaced(apply(
            &markers,
            &finish(WorkerEnd::Done, Some(250)),
            fresh("s"),
            None,
            300,
        ));
        assert_eq!(two.id, "worker:s:2");
        let markers = [markers[0].clone(), two];
        assert_eq!(
            apply(
                &markers,
                &finish(WorkerEnd::Done, Some(260)),
                fresh("s"),
                None,
                300
            ),
            Upsert::Nothing,
            "every card has its end"
        );
    }

    #[test]
    fn a_done_in_the_second_its_card_was_written_is_that_cards_when_nothing_older_is_open() {
        let start = appended(apply(&[], &spawn(false), fresh("s"), Some("t1"), 100));
        let first = replaced(apply(
            &[start],
            &finish(WorkerEnd::Done, Some(150)),
            fresh("s"),
            None,
            160,
        ));
        let second = appended(apply(
            std::slice::from_ref(&first),
            &assign("Next"),
            fresh("s"),
            Some("t2"),
            300,
        ));
        let ended = replaced(apply(
            &[first, second],
            &finish(WorkerEnd::Done, Some(300)),
            fresh("s"),
            None,
            301,
        ));
        assert_eq!(ended.id, "worker:s:2");
    }

    #[test]
    fn a_done_in_the_second_of_a_newer_card_goes_to_an_open_card_it_surely_follows() {
        let first = appended(apply(&[], &spawn(false), fresh("s"), Some("t1"), 100));
        let second = appended(apply(
            std::slice::from_ref(&first),
            &assign("Next"),
            fresh("s"),
            Some("t2"),
            300,
        ));
        // The first assignment ended in the second the next was sent: the first card's.
        let ended = replaced(apply(
            &[first, second],
            &finish(WorkerEnd::Done, Some(300)),
            fresh("s"),
            None,
            301,
        ));
        assert_eq!(ended.id, "worker:s");
    }

    #[test]
    fn a_late_card_takes_a_done_from_before_it_was_written() {
        let late = appended(apply(&[], &spawn(true), fresh("s"), None, 200));
        let ended = replaced(apply(
            &[late],
            &finish(WorkerEnd::Done, Some(150)),
            fresh("s"),
            None,
            201,
        ));
        assert_eq!(
            of(&ended).unwrap().end.as_ref().unwrap().status,
            WorkerEnd::Done
        );
    }

    #[test]
    fn the_same_done_said_again_completes_nothing_more() {
        let first = appended(apply(&[], &spawn(false), fresh("s"), Some("t1"), 100));
        let second = appended(apply(
            std::slice::from_ref(&first),
            &assign("Next"),
            fresh("s"),
            Some("t2"),
            200,
        ));
        let done = replaced(apply(
            &[first.clone(), second],
            &finish(WorkerEnd::Done, Some(250)),
            fresh("s"),
            None,
            260,
        ));
        assert_eq!(done.id, "worker:s:2");
        // A desktop restart, or a second window, says it again.
        assert_eq!(
            apply(
                &[first, done],
                &finish(WorkerEnd::Done, Some(250)),
                fresh("s"),
                None,
                270,
            ),
            Upsert::Nothing
        );
    }

    #[test]
    fn a_chat_turn_ends_the_cards_of_the_messages_it_took() {
        let chat = |text: Option<&str>, after: u64| WorkerRef {
            kind: WorkerKind::Chat,
            assignment: text.map(str::to_owned),
            after: Some(after),
            ..fresh("s")
        };
        let ended = |turn: &str, took: &[(u64, &str)]| WorkerNote::Finish {
            session_id: "s".into(),
            kind: WorkerKind::Chat,
            end: chat_end(&TurnOutcome::Completed, Some(turn), 500),
            done_since: None,
            turn: Some(turn.into()),
            // The turn ends right after its last message.
            turn_end: took.last().map(|(seq, _)| seq + 1),
            took: took
                .iter()
                .map(|(seq, line)| Took {
                    seq: *seq,
                    line: Some((*line).into()),
                })
                .collect(),
        };
        let ids = |upsert: Upsert| match upsert {
            Upsert::Replace(items) => items.into_iter().map(|item| item.id).collect(),
            Upsert::Nothing => Vec::new(),
            other => panic!("unexpected {other:?}"),
        };
        let spawned = item("worker:s".into(), None, chat(None, 3));
        let again = item("worker:s:2".into(), None, chat(Some("continue"), 10));
        let more = item("worker:s:3".into(), None, chat(Some("continue"), 30));
        let markers = [spawned.clone(), again.clone(), more.clone()];
        // The same words twice: each message is the latest card's written before it.
        assert_eq!(
            ids(apply(
                &markers,
                &ended("c2", &[(35, "continue")]),
                fresh("s"),
                None,
                1
            )),
            ["worker:s:3"]
        );
        assert_eq!(
            ids(apply(
                &markers,
                &ended("c1", &[(12, "continue")]),
                fresh("s"),
                None,
                1
            )),
            ["worker:s:2"]
        );
        // A turn the user started in the worker, with words no card has, ends nothing; a
        // card that was given nothing is past, since cards follow it.
        assert!(
            ids(apply(
                &markers,
                &ended("u", &[(40, "hello")]),
                fresh("s"),
                None,
                1
            ))
            .is_empty()
        );
        // While it is the latest card, the worker's first turn is its.
        assert_eq!(
            ids(apply(
                &[spawned],
                &ended("c0", &[(5, "Go")]),
                fresh("s"),
                None,
                1
            )),
            ["worker:s"]
        );
        // An end already recorded is nothing, said again.
        let done = replaced(apply(
            &markers,
            &ended("c2", &[(35, "continue")]),
            fresh("s"),
            None,
            1,
        ));
        assert_eq!(
            of(&done).unwrap().end.as_ref().unwrap().turn.as_deref(),
            Some("c2")
        );
        let markers = [markers[0].clone(), markers[1].clone(), done];
        assert_eq!(
            apply(
                &markers,
                &ended("c2", &[(35, "continue")]),
                fresh("s"),
                None,
                2
            ),
            Upsert::Nothing
        );
        // A provider that started over may name a later turn as it named that one: the
        // end's place in the log tells them apart.
        let later = item("worker:s:4".into(), None, chat(Some("continue"), 50));
        let markers = [
            markers[0].clone(),
            markers[1].clone(),
            markers[2].clone(),
            later,
        ];
        assert_eq!(
            ids(apply(
                &markers,
                &ended("c2", &[(55, "continue")]),
                fresh("s"),
                None,
                3
            )),
            ["worker:s:4"]
        );
    }

    #[test]
    fn an_end_said_again_writes_only_the_cards_it_has_not_ended_yet() {
        let card = |id: &str, text: &str, after: u64| {
            item(
                id.into(),
                None,
                WorkerRef {
                    kind: WorkerKind::Chat,
                    assignment: Some(text.into()),
                    after: Some(after),
                    ..fresh("s")
                },
            )
        };
        let end = WorkerNote::Finish {
            session_id: "s".into(),
            kind: WorkerKind::Chat,
            end: chat_end(&TurnOutcome::Completed, Some("Both done"), 500),
            done_since: None,
            turn: Some("c1".into()),
            turn_end: Some(30),
            took: vec![
                Took {
                    seq: 12,
                    line: Some("Task one".into()),
                },
                Took {
                    seq: 25,
                    line: Some("Also this".into()),
                },
            ],
        };
        let markers = [
            card("worker:s", "Task one", 10),
            card("worker:s:2", "Also this", 20),
        ];
        let Upsert::Replace(both) = apply(&markers, &end, fresh("s"), None, 1) else {
            panic!("both cards end");
        };
        assert_eq!(both.len(), 2);
        // The host stopped after writing the first: said again, only the second is written.
        let markers = [both[0].clone(), markers[1].clone()];
        assert_eq!(
            replaced(apply(&markers, &end, fresh("s"), None, 2)).id,
            "worker:s:2"
        );
        assert_eq!(
            apply(&both, &end, fresh("s"), None, 3),
            Upsert::Nothing,
            "and once both have it, nothing"
        );
    }

    #[test]
    fn a_worker_chats_turn_record_names_its_turn_its_messages_and_its_last_reply() {
        let mut record = TurnRecord::default();
        let event = |item: Item, started: bool| {
            if started {
                ChatEvent::ItemStarted { item }
            } else {
                ChatEvent::ItemCompleted { item }
            }
        };
        let body = |id: &str, body: ItemBody| Item {
            presentation: Default::default(),
            id: id.into(),
            turn_id: Some("c1".into()),
            status: ItemStatus::Completed,
            body,
        };
        let mut seq = 0;
        let mut follow = |record: &mut TurnRecord, event: ChatEvent| {
            seq += 1;
            record.follow("s", seq, &event, 9)
        };
        follow(
            &mut record,
            ChatEvent::TurnStarted {
                turn_id: "c1".into(),
            },
        );
        let user = body(
            "u1",
            ItemBody::UserMessage {
                text: "\n Fix it\nnow".into(),
            },
        );
        follow(&mut record, event(user.clone(), true));
        follow(&mut record, event(user, false));
        follow(
            &mut record,
            event(
                body(
                    "r1",
                    ItemBody::AgentMessage {
                        text: "Looking".into(),
                    },
                ),
                false,
            ),
        );
        follow(
            &mut record,
            event(
                body(
                    "r2",
                    ItemBody::AgentMessage {
                        text: String::new(),
                    },
                ),
                true,
            ),
        );
        follow(
            &mut record,
            ChatEvent::ItemDelta {
                item_id: "r2".into(),
                delta: Delta::Text("Fixed, ".into()),
            },
        );
        follow(
            &mut record,
            ChatEvent::ItemDelta {
                item_id: "r2".into(),
                delta: Delta::Text("tests pass".into()),
            },
        );
        let Some(WorkerNote::Finish {
            end, turn, took, ..
        }) = follow(
            &mut record,
            ChatEvent::TurnCompleted {
                turn_id: "c1".into(),
                outcome: TurnOutcome::Interrupted,
            },
        )
        else {
            panic!("a turn's end is a finish");
        };
        assert_eq!(turn.as_deref(), Some("c1"));
        assert_eq!(
            took,
            [Took {
                seq: 2,
                line: Some("Fix it".into())
            }]
        );
        assert_eq!(
            (end.status, end.tail.as_str()),
            (WorkerEnd::Stopped, "Fixed, tests pass")
        );
    }

    #[test]
    fn readers_tolerate_unknown_and_malformed_worker_presentations() {
        let marker = appended(apply(&[], &spawn(false), fresh("s"), None, 1));
        let mut value = serde_json::to_value(&marker).unwrap();
        value["presentation"]["worker"]["future"] = serde_json::json!({"shape": 1});
        value["presentation"]["worker"]["kind"] = "robot".into();
        value["presentation"]["worker"]["final"] =
            serde_json::json!({"status": "vanished", "ended_at": 3});
        let read: Item = serde_json::from_value(value.clone()).unwrap();
        let worker = of(&read).unwrap();
        assert_eq!(worker.kind, WorkerKind::Unknown);
        assert_eq!(worker.end.as_ref().unwrap().status, WorkerEnd::Unknown);
        assert_eq!(worker.end.as_ref().unwrap().tail, "");
        value["presentation"]["worker"] = serde_json::json!({"session_id": 5});
        let read: Item = serde_json::from_value(value).unwrap();
        assert!(
            of(&read).is_none(),
            "a malformed marker is a plain tool row"
        );
        assert!(matches!(read.body, ItemBody::ToolCall { .. }));
    }

    #[test]
    fn card_status_prefers_the_record_then_supersession_then_the_inventory() {
        let mut worker = fresh("s");
        worker.noted_at = 100;
        assert_eq!(
            CardStatus::of(&worker, false, None, 110),
            CardStatus::Starting
        );
        assert_eq!(CardStatus::of(&worker, false, None, 130), CardStatus::Ended);
        assert!(!CardStatus::Ended.running());
        let working = CardStatus::of(&worker, false, Some(LiveStatus::Working), 999);
        assert!(working.running());
        assert_eq!(working.label(), "Working");
        assert_eq!(
            CardStatus::of(&worker, true, Some(LiveStatus::Working), 999),
            CardStatus::Continued
        );
        assert!(!CardStatus::Continued.running());
        assert!(!CardStatus::Live(LiveStatus::Done).running());
        worker.end = Some(WorkerFinal {
            status: WorkerEnd::Failed,
            ended_at: 200,
            tail: String::new(),
            turn: None,
            turn_end: None,
            done_since: None,
        });
        assert_eq!(
            CardStatus::of(&worker, true, Some(LiveStatus::Working), 999),
            CardStatus::Finished(WorkerEnd::Failed)
        );
    }

    #[test]
    fn a_worker_chat_turn_reads_as_its_end_with_a_bounded_reply() {
        let end = chat_end(&TurnOutcome::Completed, Some(&"x\n".repeat(50)), 7);
        assert_eq!(end.status, WorkerEnd::Done);
        assert_eq!(end.ended_at, 7);
        assert!(end.tail.lines().count() <= crate::subshells::OUTPUT_LINES);
        assert_eq!(
            chat_end(&TurnOutcome::Interrupted, None, 7).status,
            WorkerEnd::Stopped
        );
        assert_eq!(
            chat_end(
                &TurnOutcome::Failed {
                    message: "x".into()
                },
                None,
                7
            )
            .status,
            WorkerEnd::Failed
        );
    }

    #[test]
    fn only_the_workers_own_parent_chat_makes_a_card_by_sending_to_it() {
        let home = std::env::temp_dir().join(format!("riwork-sent-{}", uuid::Uuid::new_v4()));
        let parent = uuid::Uuid::new_v4().to_string();
        let dir = super::super::log::chat_dir(&home, &parent).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("info.json"), "{}").unwrap();
        let other = uuid::Uuid::new_v4().to_string();
        let sent = |caller: Option<&str>, parent: Option<&str>| {
            sent_note(&home, caller, parent, "w", WorkerKind::Shell, "go")
        };
        assert!(matches!(
            sent(Some(&parent), Some(&parent)),
            Some((to, WorkerNote::Assign { .. })) if to == parent
        ));
        assert!(
            sent(Some(&other), Some(&parent)).is_none(),
            "another chat's send"
        );
        assert!(sent(None, Some(&parent)).is_none(), "the user's own send");
        assert!(
            sent(Some(&other), Some(&other)).is_none(),
            "a parent that is no chat"
        );
        assert!(
            sent(Some(&parent), None).is_none(),
            "a worker with no parent"
        );
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn the_text_line_says_who_what_and_how_it_ended() {
        let mut worker = fresh("s");
        worker.assignment = Some("Fix it".into());
        assert_eq!(
            summary(&worker),
            "Started claude worker · Fix polling · Fix it"
        );
        worker.follow_up = true;
        worker.end = Some(WorkerFinal {
            status: WorkerEnd::Done,
            ended_at: 1,
            tail: String::new(),
            turn: None,
            turn_end: None,
            done_since: None,
        });
        assert_eq!(
            summary(&worker),
            "Follow-up for claude worker · Fix polling · Fix it → Done"
        );
    }
}
