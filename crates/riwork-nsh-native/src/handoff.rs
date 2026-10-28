//! The desktop's explicit conversation handoff, using the same engine as the CLI.
//! Reading the source starts nothing. Transcript/summary collection and target
//! creation run on the blocking pool only after the user confirms the dialog.
use nsh_core::prelude::*;
use riwork_engine::{
    chat::model::{ApprovalMode, ContributorMode, Provider},
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
    #[prop(skip_none)]
    pub project_id: Option<String>,
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
    #[prop(skip_none)]
    pub model: Option<String>,
    #[prop(skip_none)]
    pub effort: Option<String>,
    #[prop(skip_none)]
    pub account: Option<String>,
    #[prop(skip_none)]
    pub mode: Option<String>,
    pub context: String,
    #[prop(skip_none)]
    pub note: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct HandoffResultDto {
    pub id: String,
    pub kind: String,
    #[prop(skip_none)]
    pub project_id: Option<String>,
    #[prop(skip_none)]
    pub worktree_id: Option<String>,
    pub document: String,
    pub context: String,
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
            let source = handoff::resolve_source(&home, &request.source_id)?;
            source_dto(&source)?;
            let kind = match request.kind.as_str() {
                "shell" => Kind::Shell, "chat" => Kind::Chat,
                _ => return Err("Choose a shell or chat handoff destination.".into()),
            };
            let provider = match request.provider.as_str() {
                "codex" => HarnessKind::Codex, "claude" => HarnessKind::Claude,
                "grok" => HarnessKind::Grok, "muse" => HarnessKind::Muse,
                _ => return Err("Choose Codex, Claude, Grok or Muse.".into()),
            };
            let mode = match request.mode.as_deref() {
                None | Some("") | Some("default") => None,
                Some("supervised") => Some(ApprovalMode::Supervised),
                Some("auto_edit") => Some(ApprovalMode::AutoEdit),
                Some("full") => Some(ApprovalMode::Full),
                Some("plan") => Some(ApprovalMode::Plan),
                _ => return Err("Unknown handoff permission mode.".into()),
            };
            let context = match request.context.as_str() {
                "transcript" => Context::Transcript, "summary" => Context::Summary,
                _ => return Err("Choose transcript or summary context.".into()),
            };
            let ensure = || backend.ensure();
            let env = handoff::Env {
                home: &home, ensure: &ensure, account: &codex_accounts::resolve_account,
                timing: handoff::summary::Timing::DEFAULT,
            };
            let result = handoff::run(&env, handoff::Request {
                source, kind, provider, model: text(request.model, "model", 100)?,
                effort: text(request.effort, "effort", 100)?, account: text(request.account, "account", 200)?,
                mode, contributor: ContributorMode::Off, context, note: text(request.note, "note", 2000)?,
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
