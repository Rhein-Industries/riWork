//! The desktop's explicit conversation handoff and fork, using the same engine as the CLI.
//! Reading the source starts nothing. Transcript/summary collection and target
//! creation run on the blocking pool only after the user confirms the dialog (or the
//! chat tab's inline confirmation, which hands off in place: the new chat takes the tab).
use nsh_core::prelude::*;
use riwork_engine::{
    chat::{
        client::Client,
        model::{ApprovalMode, ChatState, ContributorMode, LineageContext, LineageKind, Provider},
    },
    codex_accounts,
    handoff::{self, Context, Kind, Source, Started},
    sessions::{HarnessKind, ShellKind},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct HandoffSourceDto {
    pub id: String,
    pub label: String,
    pub askable: bool,
    pub kind: String,
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[prop(skip_none)]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[prop(skip_none)]
    pub worktree_id: Option<String>,
    pub cwd: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct HandoffRequestDto {
    pub source_id: String,
    pub kind: String,
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[prop(skip_none)]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[prop(skip_none)]
    pub effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[prop(skip_none)]
    pub account: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[prop(skip_none)]
    pub mode: Option<String>,
    pub context: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[prop(skip_none)]
    pub note: Option<String>,
    /// The item of a chat source to cut its conversation at, that item included.
    #[prop(skip_none)]
    pub at: Option<String>,
    /// The new chat takes the source's tab: it keeps the name the tab showed and the
    /// source's permission mode, and the source must not be at work.
    #[prop(skip_none)]
    pub in_place: Option<bool>,
}

/// A fork of a chat (`riwork chat fork`): the whole conversation or up to an item, with the
/// source's agent or another.
#[derive(Clone, Debug, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct ForkRequestDto {
    pub chat_id: String,
    #[prop(skip_none)]
    pub at: Option<String>,
    #[prop(skip_none)]
    pub provider: Option<String>,
    #[prop(skip_none)]
    pub model: Option<String>,
    #[prop(skip_none)]
    pub effort: Option<String>,
    #[prop(skip_none)]
    pub mode: Option<String>,
    #[prop(skip_none)]
    pub title: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct ForkResultDto {
    pub id: String,
    pub title: String,
    pub provider: String,
    #[prop(skip_none)]
    pub project_id: Option<String>,
    #[prop(skip_none)]
    pub worktree_id: Option<String>,
    /// `native` (the provider's own copy of its thread) or `transcript`.
    pub context: String,
    #[prop(skip_none)]
    pub at: Option<String>,
    #[prop(skip_none)]
    pub document: Option<String>,
    #[prop(skip_none)]
    pub fallback: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct HandoffResultDto {
    pub id: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[prop(skip_none)]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[prop(skip_none)]
    pub worktree_id: Option<String>,
    pub document: String,
    pub context: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[prop(skip_none)]
    pub fallback: Option<String>,
}

fn source_dto(source: &Source) -> Result<HandoffSourceDto, String> {
    let (kind, provider, project_id, worktree_id, cwd) = match source {
        Source::Shell(shell) => {
            if shell.kind == ShellKind::Orchestrator {
                return Err("An orchestrator's conversation is not handed off.".into());
            }
            ("shell", shell.harness.unwrap_or(HarnessKind::Codex).program(), shell.project_id.clone(), shell.worktree_id.clone(), shell.cwd.clone())
        }
        Source::Chat(chat) => {
            if chat.orchestrator.is_some() {
                return Err("An orchestrator's conversation is not handed off.".into());
            }
            let provider = match chat.provider { Provider::Codex => "codex", Provider::Claude => "claude", Provider::Muse => "muse" };
            ("chat", provider, chat.project_id.clone(), chat.worktree_id.clone(), chat.cwd.clone())
        }
    };
    Ok(HandoffSourceDto {
        id: source.id().to_owned(), label: source.label(), askable: source.is_askable(),
        kind: kind.into(), provider: provider.into(), project_id, worktree_id,
        cwd: cwd.to_string_lossy().into_owned(),
    })
}

fn mode_of(mode: Option<&str>) -> Result<Option<ApprovalMode>, String> {
    Ok(match mode {
        None | Some("") | Some("default") => None,
        Some("supervised") => Some(ApprovalMode::Supervised),
        Some("auto_edit") => Some(ApprovalMode::AutoEdit),
        Some("full") => Some(ApprovalMode::Full),
        Some("plan") => Some(ApprovalMode::Plan),
        _ => return Err("Unknown handoff permission mode.".into()),
    })
}

fn provider_word(provider: Provider) -> &'static str {
    match provider { Provider::Codex => "codex", Provider::Claude => "claude", Provider::Muse => "muse" }
}

/// Forks a chat on the blocking pool (`handoff::fork`).
pub async fn fork(request: ForkRequestDto) -> Result<ForkResultDto, String> {
    crate::service::validate_uuid(&request.chat_id)?;
    crate::service::blocking(move || {
        let backend = crate::chat::service::backend();
        let home = backend.home()?;
        let provider = match request.provider.as_deref() {
            None | Some("") => None,
            Some("codex") => Some(Provider::Codex),
            Some("claude") => Some(Provider::Claude),
            Some("muse") => Some(Provider::Muse),
            Some(_) => return Err("Choose Codex, Claude or Muse.".into()),
        };
        let ensure = || backend.ensure();
        let env = handoff::Env {
            home: &home, ensure: &ensure, account: &codex_accounts::resolve_account,
            timing: handoff::summary::Timing::DEFAULT,
        };
        let outcome = handoff::fork::fork(&env, handoff::fork::ForkRequest {
            chat_id: request.chat_id,
            at: text(request.at, "item", 512)?,
            provider,
            model: text(request.model, "model", 100)?,
            effort: text(request.effort, "effort", 100)?,
            mode: mode_of(request.mode.as_deref())?,
            title: text(request.title, "title", 200)?,
        }, &|_| {})?;
        Ok(ForkResultDto {
            title: handoff::shown_title(&outcome.chat),
            id: outcome.chat.id,
            provider: provider_word(outcome.chat.provider).into(),
            project_id: outcome.chat.project_id,
            worktree_id: outcome.chat.worktree_id,
            context: if outcome.context == LineageContext::Native { "native" } else { "transcript" }.into(),
            at: outcome.at,
            document: outcome.document.map(|path| path.to_string_lossy().into_owned()),
            fallback: outcome.fallback,
        })
    }).await
}

fn text(value: Option<String>, label: &str, limit: usize) -> Result<Option<String>, String> {
    let value = value.map(|value| value.trim().to_owned()).filter(|value| !value.is_empty());
    if let Some(value) = &value {
        if value.chars().count() > limit || value.chars().any(|c| c.is_control() && !(label == "note" && (c == '\n' || c == '\t'))) {
            return Err(format!("The {label} must have at most {limit} characters and no control characters."));
        }
    }
    Ok(value)
}

#[module]
pub struct Handoff;

#[module]
impl Handoff {
    pub async fn source(id: String) -> Result<HandoffSourceDto, String> {
        crate::service::blocking(move || {
            let home = crate::service::process_home()?;
            source_dto(&handoff::resolve_source(&home, &id)?)
        }).await
    }

    pub async fn run(request: HandoffRequestDto) -> Result<HandoffResultDto, String> {
        crate::service::blocking(move || {
            let backend = crate::chat::service::backend();
            let home = backend.home()?;
            let mut source = handoff::resolve_source(&home, &request.source_id)?;
            source_dto(&source)?;
            let in_place = request.in_place == Some(true) && request.kind == "chat" && matches!(source, Source::Chat(_));
            let kind = match request.kind.as_str() {
                "shell" => Kind::Shell, "chat" => Kind::Chat,
                _ => return Err("Choose a shell or chat handoff destination.".into()),
            };
            let provider = match request.provider.as_str() {
                "codex" => HarnessKind::Codex, "claude" => HarnessKind::Claude,
                "grok" => HarnessKind::Grok, "muse" => HarnessKind::Muse,
                _ => return Err("Choose Codex, Claude, Grok or Muse.".into()),
            };
            let mut mode = mode_of(request.mode.as_deref())?;
            let context = match request.context.as_str() {
                "transcript" => Context::Transcript, "summary" => Context::Summary,
                _ => return Err("Choose transcript or summary context.".into()),
            };
            let ensure = || backend.ensure();
            let env = handoff::Env {
                home: &home, ensure: &ensure, account: &codex_accounts::resolve_account,
                timing: handoff::summary::Timing::DEFAULT,
            };
            let mut title = None;
            if let (true, Source::Chat(chat)) = (in_place, &mut source) {
                // The host's own record: whether a turn runs, and the name the tab shows now.
                // A chat at work keeps its tab: what it still does would be out of sight.
                let socket = backend.ensure()?;
                if let Some(live) = Client::connect(&socket)?.list()?.into_iter().find(|live| live.id == chat.id) {
                    if matches!(live.state, ChatState::Running | ChatState::Waiting) {
                        return Err(format!(
                            "The agent is still at work. Wait for the turn to end, or stop it, then go on with {}.",
                            handoff::agent_name(provider)
                        ));
                    }
                    *chat = live;
                }
                // The new chat takes the tab, so it keeps the tab's name, and the chat's
                // permissions unless others were chosen. A chat named only for its agent
                // ("Codex chat") is named for the new one.
                let shown = handoff::shown_title(chat);
                title = Some(if handoff::is_default_title(&shown) {
                    format!("{} chat", handoff::agent_name(provider))
                } else {
                    shown
                });
                mode = mode.or(Some(chat.approval_mode));
            }
            let result = handoff::run(&env, handoff::Request {
                source, kind, provider, model: text(request.model, "model", 100)?,
                effort: text(request.effort, "effort", 100)?, account: text(request.account, "account", 200)?,
                mode, contributor: ContributorMode::Off, context, note: text(request.note, "note", 2000)?,
                at: text(request.at, "item", 512)?, title, lineage: LineageKind::Handoff,
            }, &|_| {})?;
            let (id, kind, project_id, worktree_id) = match result.target {
                Started::Shell(shell) => (shell.id, "shell", shell.project_id, shell.worktree_id),
                Started::Chat(chat) => (chat.id, "chat", chat.project_id, chat.worktree_id),
            };
            Ok(HandoffResultDto {
                id, kind: kind.into(), project_id, worktree_id,
                document: result.document.to_string_lossy().into_owned(),
                context: if result.context == Context::Summary { "summary" } else { "transcript" }.into(),
                fallback: result.fallback,
            })
        }).await
    }
}
