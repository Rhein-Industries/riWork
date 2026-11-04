//! Forking a chat (`somtus chat fork`, Fork chat and Fork from here in a chat tab): a new
//! chat that goes on, on its own, from a point of another chat's conversation, while the
//! original goes on as it was.
//!
//! The point is an item of the source's transcript, that item included (`ForkRequest::at`),
//! or the whole conversation. A chat at work is forked at its last finished item: what is
//! still being written is not the fork's.
//!
//! The provider's own copy of its thread is used when it reproduces exactly that point
//! (`ChatOrigin::native`): Claude copies a whole session (`--resume <session>
//! --fork-session`), so it forks an idle chat whole; Codex forks a thread through a finished
//! turn (`thread/fork` with `lastTurnId`), so it forks at the end of any finished turn of the
//! thread the chat runs now, and at a message that begins a turn by forking through the turn
//! before and sending the message again. A provider whose own fork does not start (the thread
//! is gone, an older CLI) is left, and the fork is made like any other: a handoff document
//! cut at the point (`super::run`), which is also how a fork with another provider is made.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use super::{Context, Env, Kind, Request, Source, Started, document, mark_source};
use crate::{
    chat::{
        client::Client,
        lineage,
        model::{
            ApprovalMode, ChatCommand, ChatEvent, ChatInfo, ChatOrigin, ChatState, ContributorMode,
            Item, ItemBody, ItemStatus, LineageContext, LineageKind, NativeFork, NewChat, Provider,
            Transcript,
        },
        workers,
    },
    sessions::HarnessKind,
};

pub struct ForkRequest {
    /// The chat to fork.
    pub chat_id: String,
    /// The item to fork at, that item included; none for the whole conversation.
    pub at: Option<String>,
    /// Another agent for the fork; none for the source's own. Its model and effort are the
    /// source's when the agent is the same and none is given.
    pub provider: Option<Provider>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub mode: Option<ApprovalMode>,
    /// None for "<title> (fork)".
    pub title: Option<String>,
}

pub struct ForkOutcome {
    pub chat: ChatInfo,
    /// What the fork got of the conversation: the provider's copy, or a transcript.
    pub context: LineageContext,
    /// The item it was cut at; none for the whole conversation.
    pub at: Option<String>,
    /// The handoff document it started from, when it did.
    pub document: Option<PathBuf>,
    /// Why the provider's own fork was tried and not used.
    pub fallback: Option<String>,
}

/// Forks `request.chat_id`. Blocks while the new chat starts (and while a transcript is
/// read), so call it off the UI thread.
pub fn fork(
    env: &Env<'_>,
    request: ForkRequest,
    progress: &dyn Fn(&str),
) -> Result<ForkOutcome, String> {
    let socket = (env.ensure)()?;
    let source = Client::connect(&socket)?
        .list()?
        .into_iter()
        .find(|chat| chat.id == request.chat_id)
        .ok_or_else(|| format!("No chat has the id {}", request.chat_id))?;
    if source.orchestrator.is_some() {
        return Err("An orchestrator's conversation is not forked.".into());
    }
    let shape = Shape::read(env.home, &source.id)?;
    let busy = matches!(source.state, ChatState::Running | ChatState::Waiting)
        || shape.transcript.turn_id.is_some();
    let at = shape.cut(request.at.as_deref(), busy)?;
    let provider = request.provider.unwrap_or(source.provider);
    let same = provider == source.provider;
    let model = request
        .model
        .clone()
        .filter(|model| !model.trim().is_empty())
        .or_else(|| source.model.clone().filter(|_| same));
    let effort = request
        .effort
        .clone()
        .filter(|effort| !effort.trim().is_empty())
        .or_else(|| source.effort.clone().filter(|_| same));
    let mode = request.mode.unwrap_or(source.approval_mode);
    let title = request
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{} (fork)", super::shown_title(&source).trim()));
    let title = document::clip(&title, 100);

    let mut fallback = None;
    if same && let Some((native, again)) = shape.native(&source, at.as_deref(), busy) {
        progress("Forking the conversation…");
        let mut client = Client::connect(&socket)?;
        let origin = ChatOrigin {
            native: Some(native),
            ..Source::Chat(source.clone()).lineage(
                LineageKind::Fork,
                at.clone(),
                LineageContext::Native,
            )
        };
        let chat = client.create(NewChat {
            parent_id: None,
            provider,
            project_id: source.project_id.clone(),
            worktree_id: source.worktree_id.clone(),
            cwd: source.cwd.clone(),
            // The thread is kept in the source's Codex home; the host holds the fork to it.
            codex_account_id: (provider == Provider::Codex).then(|| {
                source
                    .codex_account_id
                    .clone()
                    .unwrap_or_else(|| crate::codex_accounts::SYSTEM_DEFAULT_ID.to_owned())
            }),
            title: Some(title.clone()),
            approval_mode: mode,
            model: model.clone(),
            effort: effort.clone(),
            orchestrator: None,
            fast: source.fast,
            contributor_mode: ContributorMode::Off,
            contributor_opt_out: false,
            claude_billing: source
                .claude_billing
                .filter(|_| provider == Provider::Claude),
            origin: Some(origin),
        })?;
        // A chat host from before forks reads past the origin and makes a fresh chat that
        // knows nothing of the conversation: it goes too, and the document carries it.
        let forked = chat
            .origin
            .as_ref()
            .is_some_and(|origin| origin.native.is_some());
        match &chat.state {
            ChatState::Failed { message } => {
                // Nothing was said in it: it goes, and the fork is made from the transcript.
                let _ = client.delete(&chat.id);
                fallback = Some(format!(
                    "{}'s own fork did not start ({}), so the fork starts from the conversation as recorded.",
                    super::provider_name(provider),
                    message.trim()
                ));
            }
            _ if !forked => {
                let _ = client.delete(&chat.id);
                fallback = Some(
                    "The chat host running now is from before forks (a newer one takes over once it has been idle a while), so the fork starts from the conversation as recorded."
                        .into(),
                );
            }
            _ => {
                if let Some(text) = again {
                    client
                        .command(&chat.id, ChatCommand::Send { text })
                        .map_err(|error| {
                            format!(
                                "Chat {} was forked, but the message it was forked at was not sent again: {error}",
                                chat.id
                            )
                        })?;
                }
                mark_source(
                    &socket,
                    &source.id,
                    &chat,
                    LineageKind::Fork,
                    at.clone(),
                    LineageContext::Native,
                );
                return Ok(ForkOutcome {
                    chat,
                    context: LineageContext::Native,
                    at,
                    document: None,
                    fallback: None,
                });
            }
        }
    }

    let outcome = super::run(
        env,
        Request {
            source: Source::Chat(source.clone()),
            kind: Kind::Chat,
            provider: harness(provider)?,
            model,
            effort,
            // A Codex fork keeps the source's account, as a native one does.
            account: source
                .codex_account_id
                .clone()
                .filter(|_| same && provider == Provider::Codex),
            mode: Some(mode),
            contributor: ContributorMode::Off,
            context: Context::Transcript,
            note: None,
            at: at.clone(),
            title: Some(title),
            lineage: LineageKind::Fork,
        },
        progress,
    )?;
    let Started::Chat(chat) = outcome.target else {
        return Err("A fork is a chat.".into());
    };
    Ok(ForkOutcome {
        chat,
        context: LineageContext::Transcript,
        at,
        document: Some(outcome.document),
        fallback,
    })
}

fn harness(provider: Provider) -> Result<HarnessKind, String> {
    match provider {
        Provider::Codex => Ok(HarnessKind::Codex),
        Provider::Claude => Ok(HarnessKind::Claude),
        Provider::Muse => Ok(HarnessKind::Muse),
        Provider::Acp => Err("ACP conversation forks are not supported; create a new ACP chat explicitly".into()),
    }
}

/// What a chat's log says about where it can be forked.
struct Shape {
    transcript: Transcript,
    /// The provider and thread each turn ran on, as the chat's info said when it started.
    threads: HashMap<String, (Provider, Option<String>)>,
    /// Turns in the order they started.
    order: Vec<String>,
    /// Turns that ended.
    ended: HashSet<String>,
}

impl Shape {
    fn read(home: &std::path::Path, chat_id: &str) -> Result<Self, String> {
        let dir = crate::chat::log::chat_dir(home, chat_id).ok_or("invalid chat id")?;
        let envelopes = crate::chat::log::read_envelopes(&dir)
            .map_err(|error| format!("Cannot read the chat's event log: {error}"))?;
        let mut shape = Shape {
            transcript: Transcript::default(),
            threads: HashMap::new(),
            order: Vec::new(),
            ended: HashSet::new(),
        };
        let mut binding: Option<(Provider, Option<String>)> = None;
        for envelope in &envelopes {
            match &envelope.event {
                ChatEvent::Info { info } => {
                    binding = Some((info.provider, info.provider_thread_id.clone()));
                }
                ChatEvent::TurnStarted { turn_id, .. } => {
                    if !shape.threads.contains_key(turn_id) {
                        shape.order.push(turn_id.clone());
                    }
                    if let Some(binding) = &binding {
                        shape.threads.insert(turn_id.clone(), binding.clone());
                    }
                }
                ChatEvent::TurnCompleted { turn_id, .. } => {
                    shape.ended.insert(turn_id.clone());
                }
                _ => {}
            }
            shape
                .transcript
                .apply_sequenced(envelope.seq, &envelope.event);
        }
        Ok(shape)
    }

    /// Whether `item` is part of the conversation, not Somtus's record or a banner.
    fn said(item: &Item) -> bool {
        !lineage::is_marker(item)
            && !workers::is_marker(item)
            && !matches!(item.body, ItemBody::Notice { .. })
    }

    /// Where to cut: the item asked for, or for a chat at work its last finished item, or
    /// nowhere (the whole conversation).
    fn cut(&self, at: Option<&str>, busy: bool) -> Result<Option<String>, String> {
        let items = &self.transcript.items;
        if let Some(at) = at {
            let item = items
                .iter()
                .find(|item| item.id == at)
                .ok_or_else(|| format!("The chat has no item {at} to fork at."))?;
            if !Self::said(item) {
                return Err("Fork at a message or a step of the conversation.".into());
            }
            if item.status == ItemStatus::InProgress {
                return Err(
                    "That part of the conversation is still being written; fork at one that is finished."
                        .into(),
                );
            }
            return Ok(Some(at.to_owned()));
        }
        if !busy {
            return Ok(None);
        }
        Ok(items
            .iter()
            .rev()
            .find(|item| Self::said(item) && item.status != ItemStatus::InProgress)
            .map(|item| item.id.clone()))
    }

    /// The provider's own fork that reproduces the cut, if there is one, and the message to
    /// send again in it (a fork at a message that begins a turn).
    fn native(
        &self,
        source: &ChatInfo,
        at: Option<&str>,
        busy: bool,
    ) -> Option<(NativeFork, Option<String>)> {
        // A chat that switched provider has its earlier conversation in its instructions,
        // which a copy of the thread does not take along.
        if source.carried_over.is_some() {
            return None;
        }
        let thread = source.provider_thread_id.clone()?;
        let whole = || NativeFork {
            thread_id: thread.clone(),
            through_turn: None,
        };
        match source.provider {
            Provider::Claude => (at.is_none() && !busy).then(|| (whole(), None)),
            Provider::Muse | Provider::Acp => None,
            Provider::Codex => {
                let Some(at) = at else {
                    return (!busy).then(|| (whole(), None));
                };
                let current = (source.provider, Some(thread.clone()));
                let on_thread = |turn: &str| {
                    self.ended.contains(turn) && self.threads.get(turn) == Some(&current)
                };
                let items = &self.transcript.items;
                let item = items.iter().find(|item| item.id == at)?;
                let turn = item.turn_id.clone()?;
                let said: Vec<&Item> = items
                    .iter()
                    .filter(|item| item.turn_id.as_ref() == Some(&turn) && Self::said(item))
                    .collect();
                if said.last().is_some_and(|last| last.id == at) && on_thread(&turn) {
                    let fork = NativeFork {
                        thread_id: thread,
                        through_turn: Some(turn),
                    };
                    return Some((fork, None));
                }
                let ItemBody::UserMessage { text } = &item.body else {
                    return None;
                };
                if said.first().is_none_or(|first| first.id != at) {
                    return None;
                }
                let at_turn = self.order.iter().position(|known| *known == turn)?;
                let before = self.order[..at_turn].last()?;
                on_thread(before).then(|| {
                    let fork = NativeFork {
                        thread_id: thread.clone(),
                        through_turn: Some(before.clone()),
                    };
                    (fork, Some(text.clone()))
                })
            }
        }
    }
}
