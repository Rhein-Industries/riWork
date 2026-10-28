//! Project forms use the same core choices as launch resolution. No form read
//! discovers accounts, reads credentials, starts an agent, or changes a binding.
use crate::{
    dto::{FolderDto, ProjectCodexAccountDto, ProjectDto, StoreState},
    service::{self, blocking},
    settings::{
        SettingsDto,
        forms::{self, DelegationDto, ModelChoiceDto, RouteDto},
    },
    usage::AccountsDto,
};
use nsh_core::{JsonValue, prelude::*};
use riwork_core::{
    delegation::DelegationPolicy,
    settings::{OrchestratorRuns, Settings as CoreSettings},
    store::{ProjectCodexAccount, ProjectOrchestrator},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct OrchestratorRunsDto {
    pub mode: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub description: String,
}
impl From<&OrchestratorRuns> for OrchestratorRunsDto {
    fn from(runs: &OrchestratorRuns) -> Self {
        let (provider, model) = match runs {
            OrchestratorRuns::Chat { provider, model } => {
                (Some(forms::provider_word(*provider).into()), model.clone())
            }
            _ => (None, None),
        };
        Self {
            mode: serde_json::to_value(runs.mode())
                .unwrap()
                .as_str()
                .unwrap()
                .into(),
            provider,
            model,
            description: runs.describe(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct ProjectAccountChoiceDto {
    pub id: String,
    pub source: String,
    pub account_id: Option<String>,
    pub label: String,
    pub available: bool,
    pub reason: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct ProjectOrchestratorDto {
    pub selected_mode: Option<String>,
    pub provider: String,
    pub model: Option<String>,
    pub models: Vec<ModelChoiceDto>,
    pub effective: OrchestratorRunsDto,
    pub unreadable_note: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDelegationDto {
    pub selected_spread_work: Option<bool>,
    pub selected_parallel_shells: Option<u32>,
    pub selected_design: Option<RouteDto>,
    pub selected_workhorse: Option<RouteDto>,
    pub effective: DelegationDto,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct ProjectFolderDto {
    pub id: String,
    pub name: String,
    pub parent_id: Option<String>,
    pub path: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSettingsDto {
    pub schema: u32,
    pub project: ProjectDto,
    pub folders: Vec<ProjectFolderDto>,
    pub settings: SettingsDto,
    pub accounts: Vec<ProjectAccountChoiceDto>,
    pub selected_account_unavailable: bool,
    pub accounts_listed: bool,
    pub accounts_refreshing: bool,
    pub accounts_error: Option<String>,
    pub orchestrator: ProjectOrchestratorDto,
    pub delegation: ProjectDelegationDto,
}

pub(crate) fn account_choice(
    choice: &ProjectCodexAccountDto,
) -> Result<ProjectCodexAccount, String> {
    match (choice.source.as_str(), choice.account_id.as_deref()) {
        ("inherit", None) => Ok(ProjectCodexAccount::Inherit),
        ("system_default", None) => Ok(ProjectCodexAccount::SystemDefault),
        ("saved", Some(id)) if !id.is_empty() => Ok(ProjectCodexAccount::Saved(id.into())),
        _ => Err("invalid: choose Inherit app default, System default, or a saved account".into()),
    }
}
pub(crate) fn orchestrator_choice(mut value: JsonValue) -> Result<ProjectOrchestrator, String> {
    if let Some(model) = value.get_mut("model") {
        if let Some(text) = model.as_str() {
            *model = serde_json::Value::String(forms::checked_model(text)?);
        }
    }
    let choice: ProjectOrchestrator =
        serde_json::from_value(value).map_err(|e| format!("invalid: {e}"))?;
    if matches!(choice, ProjectOrchestrator::Unreadable(_)) {
        return Err(
            "invalid: choose Inherit, Automatic, Terminal, or a supported chat provider and model"
                .into(),
        );
    }
    Ok(choice)
}
fn policy(value: &JsonValue) -> DelegationPolicy {
    let mut value = value.clone();
    if let Some(object) = value.as_object_mut() {
        for (bridge, stored) in [
            ("spreadWork", "spread_work"),
            ("parallelShells", "parallel_shells"),
        ] {
            if let Some(value) = object.remove(bridge) {
                object.insert(stored.into(), value);
            }
        }
    }
    serde_json::from_value(value).expect("core delegation reader is lenient")
}
fn folder_path(folder: &FolderDto, folders: &[FolderDto]) -> String {
    let mut names = vec![folder.name.clone()];
    let mut next = folder.parent_id.as_deref();
    let mut seen = std::collections::HashSet::from([folder.id.as_str()]);
    while let Some(id) = next {
        if !seen.insert(id) {
            break;
        }
        let Some(parent) = folders.iter().find(|f| f.id == id) else {
            break;
        };
        names.push(parent.name.clone());
        next = parent.parent_id.as_deref();
    }
    names.reverse();
    names.join("/")
}
/// Pure projection, used by private-home tests without touching process globals.
pub fn project_form(
    state: &StoreState,
    project_id: &str,
    settings: &CoreSettings,
    settings_error: Option<String>,
    accounts: &AccountsDto,
) -> Result<ProjectSettingsDto, String> {
    let project = state
        .projects
        .iter()
        .find(|p| p.id == project_id)
        .cloned()
        .ok_or("not_found: project")?;
    let choice: ProjectOrchestrator = serde_json::from_value(project.orchestrator.clone())
        .expect("lenient project orchestrator reader");
    let effective = choice
        .choice()
        .cloned()
        .unwrap_or_else(|| settings.orchestrator_runs());
    let selected_mode = match &choice {
        ProjectOrchestrator::Inherit => Some("inherit".into()),
        ProjectOrchestrator::Choose(runs) => Some(
            serde_json::to_value(runs.mode())
                .unwrap()
                .as_str()
                .unwrap()
                .into(),
        ),
        ProjectOrchestrator::Unreadable(_) => None,
    };
    let (provider, model) = match choice.choice() {
        Some(OrchestratorRuns::Chat { provider, model }) => (*provider, model.clone()),
        _ => (
            settings.orchestrator_chat_provider,
            settings.orchestrator_chat_model.clone(),
        ),
    };
    let project_policy = policy(&project.delegation_policy);
    let mut options = vec![
        ProjectAccountChoiceDto {
            id: "inherit".into(),
            source: "inherit".into(),
            account_id: None,
            label: "Inherit app default".into(),
            available: true,
            reason: None,
        },
        ProjectAccountChoiceDto {
            id: "system-default".into(),
            source: "system_default".into(),
            account_id: None,
            label: "System default".into(),
            available: true,
            reason: None,
        },
    ];
    options.extend(accounts.accounts.iter().filter(|a| !a.system).map(|a| {
        ProjectAccountChoiceDto {
            id: a.id.clone(),
            source: "saved".into(),
            account_id: Some(a.id.clone()),
            label: a.label.clone(),
            available: a.available,
            reason: a.unavailable_reason.clone(),
        }
    }));
    let selected_account_unavailable = if project.codex_account.source == "saved" {
        let id = project.codex_account.account_id.as_deref().unwrap_or("");
        let available = options
            .iter()
            .any(|a| a.source == "saved" && a.account_id.as_deref() == Some(id) && a.available);
        if !options
            .iter()
            .any(|a| a.source == "saved" && a.account_id.as_deref() == Some(id))
        {
            options.push(ProjectAccountChoiceDto {
                id: id.into(),
                source: "saved".into(),
                account_id: Some(id.into()),
                label: format!(
                    "Saved account unavailable · {}",
                    id.chars().take(8).collect::<String>()
                ),
                available: false,
                reason: Some(
                    "Selected account unavailable. Choose another before starting Codex.".into(),
                ),
            });
        }
        !available
    } else {
        project.codex_account.source == "inherit" && accounts.selected_unavailable
    };
    let mut folders = state
        .project_folders
        .iter()
        .map(|f| ProjectFolderDto {
            id: f.id.clone(),
            name: f.name.clone(),
            parent_id: f.parent_id.clone(),
            path: folder_path(f, &state.project_folders),
        })
        .collect::<Vec<_>>();
    folders.sort_by(|a, b| {
        a.path
            .to_lowercase()
            .cmp(&b.path.to_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(ProjectSettingsDto {
        schema: 1,
        project,
        folders,
        settings: SettingsDto::from_settings(settings, settings_error),
        accounts: options,
        selected_account_unavailable,
        accounts_listed: accounts.listed,
        accounts_refreshing: accounts.refreshing,
        accounts_error: accounts.error.clone(),
        orchestrator: ProjectOrchestratorDto {
            selected_mode,
            provider: forms::provider_word(provider).into(),
            model: model.clone(),
            models: forms::model_choices(provider, model.as_deref()),
            effective: (&effective).into(),
            unreadable_note: choice.unreadable_note(),
        },
        delegation: ProjectDelegationDto {
            selected_spread_work: project_policy.spread_work,
            selected_parallel_shells: project_policy.parallel_shells.map(u32::from),
            selected_design: project_policy.design.as_ref().map(Into::into),
            selected_workhorse: project_policy.workhorse.as_ref().map(Into::into),
            effective: DelegationDto::new(&settings.delegation_policy, Some(&project_policy)),
        },
    })
}

#[module]
pub struct ProjectSettings;
#[module]
impl ProjectSettings {
    pub async fn get(project_id: String) -> Result<ProjectSettingsDto, String> {
        service::validate_uuid(&project_id)?;
        let accounts = crate::usage::Accounts::list().await?;
        let process = service::initialize().await?;
        blocking(move || {
            let state = process.store.snapshot().map_err(service::backend_error)?;
            let (settings, error) = process.preferences.core();
            project_form(&state, &project_id, &settings, error, &accounts)
        })
        .await
    }
}
