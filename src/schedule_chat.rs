//! Scheduled delivery into an orchestrator that runs as a chat. This is the
//! chat counterpart of `SessionManager::send_scheduled`, which types into a tmux
//! pane; the scheduler (`schedules`) takes the same claim for both.
//!
//! The orchestrator's chat is read from the chat host: a target whose chat is
//! gone, or is another chat than the one that was pinned, fails and pauses; a
//! chat that is `Idle` or `Stopped` (a message resumes it) takes the prompt; one
//! that is starting, running, waiting for the user or failed defers, and is
//! asked again. Nothing here ever sends twice:
//!
//! 1. Before anything is sent the log's length is noted, and the scheduler's
//!    **claim** is taken. The claim is on disk before the host hears of the
//!    message, and the schedule has moved on to its next occurrence.
//! 2. The prompt goes to the host as a `Send`. A host that refuses it did
//!    nothing (the schedule fails); an exchange that breaks leaves it open (the
//!    schedule is uncertain).
//! 3. A `Send` the host acknowledged is delivered once the chat's log shows a
//!    user message with exactly the prompt's text after the noted position. That
//!    is the proof; an acknowledged `Send` without it, in the time allowed, is
//!    uncertain. A failed or uncertain schedule pauses and is never retried.

use crate::{
    chat::{
        self,
        client::CallError,
        model::{ChatCommand, ChatEvent, ChatState, ItemBody},
    },
    orchestrators::ChatHost,
    schedules::{Delivery, Target},
    store::State,
};
use std::time::{Duration, Instant};
use uuid::Uuid;

/// How long delivery waits to see the message in the log, and how often it
/// looks.
#[derive(Clone, Copy, Debug)]
pub struct Proof {
    pub wait: Duration,
    pub poll: Duration,
}

impl Proof {
    /// A message shows up in the log within moments of the host accepting it.
    /// The wait is short because the scheduler holds its lock for it.
    pub const DEFAULT: Self = Self {
        wait: Duration::from_secs(10),
        poll: Duration::from_millis(50),
    };
}

/// Delivers `prompt` to the chat `target` pins, or says why not. `claim` is the
/// scheduler's: it records the attempt before the host is told and refuses it
/// (`Ok(false)`) over the rate limit.
pub fn deliver(
    host: &ChatHost,
    proof: Proof,
    target: &Target,
    state: &State,
    prompt: &str,
    claim: &mut dyn FnMut(&str) -> Result<bool, String>,
) -> Result<Delivery, String> {
    let mut client = match host.connect() {
        Ok(client) => client,
        Err(error) => {
            return Ok(Delivery::Deferred(format!(
                "The chat host is unavailable: {error}"
            )));
        }
    };
    let chats = match client.list() {
        Ok(chats) => chats,
        Err(error) => {
            return Ok(Delivery::Deferred(format!(
                "Cannot read the target chat: {error}"
            )));
        }
    };
    let Some(chat) = chats.into_iter().find(|chat| chat.id == target.shell_id) else {
        return Ok(Delivery::Failed(
            "Target session no longer exists or has exited; select an existing target explicitly."
                .into(),
        ));
    };
    if !target.matches_chat(state, &chat) {
        return Ok(Delivery::Failed(
            "Target identity changed; edit and explicitly select the intended session.".into(),
        ));
    }
    match &chat.state {
        ChatState::Idle | ChatState::Stopped => {}
        ChatState::Starting => {
            return Ok(Delivery::Deferred(
                "The orchestrator chat is starting".into(),
            ));
        }
        ChatState::Running => {
            return Ok(Delivery::Deferred(
                "The orchestrator chat is working on a turn".into(),
            ));
        }
        ChatState::Waiting => {
            return Ok(Delivery::Deferred(
                "The orchestrator chat is waiting for an approval or an answer".into(),
            ));
        }
        ChatState::Failed { message } => {
            return Ok(Delivery::Deferred(format!(
                "The orchestrator chat failed ({message}); open it and send a message to restart it"
            )));
        }
    }
    let mark = match chat::log::mark(host.home, &chat.id) {
        Ok(mark) => mark,
        Err(error) => {
            return Ok(Delivery::Deferred(format!(
                "Cannot read the target chat's log: {error}"
            )));
        }
    };
    // Nothing about a chat goes stale the way a terminal's idle signal does: its
    // state is the host's, read just now. So a claim is never refused for the
    // state it was taken at, only for the delivery limit.
    let token = format!("chat:{}:{mark}:{}", chat.id, Uuid::new_v4());
    if !claim(&token)? {
        return Ok(Delivery::Deferred(
            "Waiting for the four-attempts-per-minute delivery limit.".into(),
        ));
    }
    let sent = client.command_checked(
        &chat.id,
        ChatCommand::Send {
            text: prompt.to_owned(),
        },
    );
    match sent {
        Ok(()) => {}
        Err(CallError::Refused(reason)) => {
            return Ok(Delivery::Failed(format!(
                "The chat host refused the message ({reason}); nothing was delivered."
            )));
        }
        Err(CallError::Broken(reason)) => {
            return Ok(Delivery::Uncertain(format!(
                "The chat host may not have received the message: {reason}. Review the target; no automatic retry."
            )));
        }
    }
    if seen_in_log(host, &chat.id, mark, prompt, proof) {
        Ok(Delivery::Submitted)
    } else {
        Ok(Delivery::Uncertain(format!(
            "The chat host accepted the message, but the chat's log did not show it within {} s. Review the target; no automatic retry.",
            proof.wait.as_secs().max(1)
        )))
    }
}

/// Whether the chat's log gets a user message that is `prompt` after `mark`
/// within the time `proof` allows.
fn seen_in_log(host: &ChatHost, chat_id: &str, mark: u64, prompt: &str, proof: Proof) -> bool {
    let deadline = Instant::now() + proof.wait;
    let mut from = mark;
    loop {
        // A log that cannot be read just now is looked at again.
        if let Ok((events, next)) = chat::log::read_after(host.home, chat_id, from) {
            from = next;
            if events
                .iter()
                .any(|envelope| is_user_message(&envelope.event, prompt))
            {
                return true;
            }
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(proof.poll);
    }
}

fn is_user_message(event: &ChatEvent, prompt: &str) -> bool {
    match event {
        ChatEvent::ItemStarted { item } | ChatEvent::ItemCompleted { item } => {
            matches!(&item.body, ItemBody::UserMessage { text } if text == prompt)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests;

/// Fresh ordinary project chat: the claim includes its preassigned ID before
/// any creation request. After any creation/send ambiguity this occurrence is
/// paused, never retried. The ID remains visible even if the host reply is lost.
pub fn deliver_new(
    host: &ChatHost,
    proof: Proof,
    target: &Target,
    state: &State,
    prompt: &str,
    claim: &mut dyn FnMut(&str) -> Result<bool, String>,
) -> Result<Delivery, String> {
    if let Err(error) = target.validate_new_chat(host.home, state) {
        return Ok(Delivery::Failed(error));
    }
    let fresh = target
        .new_chat
        .as_ref()
        .ok_or("Missing fresh chat destination")?;
    let crate::schedules::Scope::Project { project_id } = &target.scope else {
        return Ok(Delivery::Failed("New chats require a project".into()));
    };
    let mut client = match host.connect() {
        Ok(client) => client,
        Err(error) => {
            return Ok(Delivery::Deferred(format!(
                "Chat host unavailable: {error}"
            )));
        }
    };
    // Same connection as Create. The installed binary may be newer than the
    // running host; unknown operations on old hosts are non-mutating refusals.
    match client.supports_identified_create(proof.wait.min(Duration::from_secs(2))) {
        Ok(true) => {}
        Ok(false) => {
            return Ok(Delivery::Deferred(
                "Chat host does not support identified creation; waiting for a compatible host"
                    .into(),
            ));
        }
        Err(error) => {
            return Ok(Delivery::Deferred(format!(
                "Cannot confirm identified chat creation support: {error}"
            )));
        }
    }
    let chat_id = Uuid::new_v4().to_string();
    if !claim(&format!("new-chat:{chat_id}"))? {
        return Ok(Delivery::Deferred(
            "Waiting for the four-attempts-per-minute delivery limit".into(),
        ));
    }
    let chat = match client.create_identified(
        &chat_id,
        chat::model::NewChat {
            provider: fresh.provider,
            project_id: Some(project_id.clone()),
            worktree_id: None,
            cwd: fresh.root.clone(),
            codex_account_id: fresh.codex_account_id.clone().or_else(|| {
                (fresh.provider == chat::model::Provider::Codex)
                    .then(|| crate::codex_accounts::SYSTEM_DEFAULT_ID.to_owned())
            }),
            title: fresh.title.clone(),
            approval_mode: fresh.approval_mode,
            model: fresh.model.clone(),
            effort: fresh.effort.clone(),
            fast: fresh.fast,
            orchestrator: None,
        },
    ) {
        Ok(chat) => chat,
        Err(CallError::Refused(error)) => {
            return Ok(Delivery::Failed(format!(
                "Chat creation refused: {error}. No automatic retry."
            )));
        }
        Err(CallError::Broken(error)) => {
            return Ok(Delivery::Uncertain(format!(
                "Chat creation may have succeeded: {error}. Open the recorded chat; no automatic retry."
            )));
        }
    };
    if chat.id != chat_id
        || chat.project_id.as_ref() != Some(project_id)
        || chat.worktree_id.is_some()
        || chat.orchestrator.is_some()
        || chat.cwd != fresh.root
        || chat.provider != fresh.provider
        || chat.codex_account_id != fresh.codex_account_id
        || chat.approval_mode != fresh.approval_mode
        || chat.model != fresh.model
        || chat.effort != fresh.effort
        || chat.fast != fresh.fast
    {
        return Ok(Delivery::Uncertain("Created chat identity differs from the requested destination; review it. No prompt sent.".into()));
    }
    // Creation can return while initialization is still running. Observe the same
    // chat only; do not defer a claimed occurrence or create another chat.
    let deadline = Instant::now() + proof.wait;
    loop {
        let current = match client.list() {
            Ok(chats) => chats.into_iter().find(|chat| chat.id == chat_id),
            Err(error) => {
                return Ok(Delivery::Uncertain(format!(
                    "Created chat cannot be inspected: {error}. No automatic retry."
                )));
            }
        };
        match current.map(|chat| chat.state) {
            Some(ChatState::Idle) => break,
            Some(ChatState::Starting) if Instant::now() < deadline => {
                std::thread::sleep(proof.poll)
            }
            Some(ChatState::Failed { message }) => {
                return Ok(Delivery::Failed(format!(
                    "Created chat failed to start: {message}. No prompt sent."
                )));
            }
            _ => return Ok(Delivery::Uncertain(
                "Created chat is not ready for its prompt; open it for review. No automatic retry."
                    .into(),
            )),
        }
    }
    let mark = match chat::log::mark(host.home, &chat_id) {
        Ok(mark) => mark,
        Err(error) => {
            return Ok(Delivery::Uncertain(format!(
                "Cannot inspect created chat log: {error}. No prompt sent."
            )));
        }
    };
    match client.command_checked(&chat_id, ChatCommand::Send { text: prompt.into() }) {
        Ok(()) if seen_in_log(host, &chat_id, mark, prompt, proof) => Ok(Delivery::Submitted),
        Ok(()) => Ok(Delivery::Uncertain("Prompt accepted but log proof is unavailable; open the created chat. No automatic retry.".into())),
        Err(CallError::Refused(error)) => Ok(Delivery::Failed(format!("Created chat refused prompt: {error}. No automatic retry."))),
        Err(CallError::Broken(error)) => Ok(Delivery::Uncertain(format!("Prompt may have been sent: {error}. Open the created chat; no automatic retry."))),
    }
}

#[cfg(test)]
mod fresh_tests;
