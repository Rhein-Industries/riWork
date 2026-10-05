//! Starting the agent that takes a conversation over: a chat made by the chat host, or a
//! terminal started the way the New Tab menu starts one. The first message is the same
//! in both (see `Request::message`); only how it gets there differs.

use super::{Origin, Request};
use crate::{
    chat::{
        client::Client,
        model::{ChatCommand, ChatInfo, ChatState, NewChat, Provider},
    },
    codex_accounts::CodexAccountBinding,
    sessions::{HarnessChoices, HarnessKind, SessionManager, ShellSession},
};

/// The agent a chat runs, if chats run it: Grok has no chat.
pub fn chat_provider(harness: HarnessKind) -> Result<Provider, String> {
    match harness {
        HarnessKind::Codex => Ok(Provider::Codex),
        HarnessKind::Claude => Ok(Provider::Claude),
        HarnessKind::Grok => {
            Err("Chats run Codex and Claude. Grok runs in a terminal: use --to shell.".into())
        }
    }
}

/// A chat in the source's project, worktree and directory, with `message` as its first
/// message. `account` is a RiWork account id, or none for the project's own.
pub fn start_chat(
    socket: &std::path::Path,
    origin: &Origin,
    request: &Request,
    account: Option<String>,
    title: String,
    message: String,
) -> Result<ChatInfo, String> {
    let mut client = Client::connect(socket)?;
    let chat = client.create(NewChat {
        provider: chat_provider(request.provider)?,
        project_id: origin.project_id.clone(),
        worktree_id: origin.worktree_id.clone(),
        cwd: origin.cwd.clone(),
        codex_account_id: account.clone(),
        title: Some(title),
        approval_mode: request.mode.unwrap_or_default(),
        model: request.model.clone(),
        effort: request.effort.clone(),
        orchestrator: None,
        fast: false,
    })?;
    // A chat host from before the account could be asked for makes the chat under the
    // project's, which is not what was asked for: take it back before it is sent anything.
    if let Some(wanted) = &account {
        let system = wanted == crate::codex_accounts::SYSTEM_DEFAULT_ID;
        let made = chat.codex_account_id.as_deref();
        if made != Some(wanted.as_str()) && !(system && made.is_none()) {
            let _ = client.delete(&chat.id);
            return Err(
                "The chat host did not start the chat on the Codex account that was asked for; it is from an older build. The chat was removed. Try again once the host has exited and been started anew (it exits by itself after 15 minutes without work)."
                    .into(),
            );
        }
    }
    // The chat exists either way; a provider that did not start is the caller's to
    // hear of, with the id to retry on.
    if let ChatState::Failed { message } = &chat.state {
        return Err(format!(
            "Chat {} was created, but its provider did not start: {message}",
            chat.id
        ));
    }
    client
        .command(&chat.id, ChatCommand::Send { text: message })
        .map_err(|error| {
            format!(
                "Chat {} was created, but its first message failed: {error}",
                chat.id
            )
        })?;
    Ok(chat)
}

/// A terminal agent in the source's project, worktree and directory that starts with
/// `prompt`, with the model, effort, permissions and account the request chose.
pub fn start_shell(
    manager: &SessionManager,
    origin: &Origin,
    request: &Request,
    binding: Option<CodexAccountBinding>,
    prompt: String,
) -> Result<ShellSession, String> {
    let project_id = origin.project_id.clone().ok_or(
        "The source belongs to no project, and a terminal needs one to start in. Hand off to a chat instead.",
    )?;
    manager.create_harness_with(
        project_id,
        origin.worktree_id.clone(),
        origin.cwd.clone(),
        request.provider,
        HarnessChoices {
            model: request.model.clone(),
            effort: request.effort.clone(),
            mode: request.mode,
            prompt: Some(prompt),
            binding,
        },
    )
}
