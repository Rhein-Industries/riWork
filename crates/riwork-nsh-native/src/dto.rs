//! Bridge-owned camelCase payloads. Core persistence and engine wire formats
//! remain unchanged; only these DTOs cross `riwork/native`.

use std::{collections::BTreeMap, path::Path};

use anyhow::Result;
use nsh_core::{
    JsonValue,
    element::{JsHost, JsIn, JsObject, JsOwned, JsValue, PropError, TsTypes},
};
use riwork_core::store as core;
use riwork_engine::sessions as engine;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

/// A typed string-keyed object. Keys are identifiers or caller data and are
/// deliberately never camelized; only DTO field names are translated.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StringMap<T>(pub BTreeMap<String, T>);

impl<T: JsValue + Serialize + DeserializeOwned> JsValue for StringMap<T> {
    fn ts_type(types: &mut TsTypes) -> String {
        format!("Record<string, {}>", T::ts_type(types))
    }

    fn from_js(value: JsIn<'_>) -> std::result::Result<Self, PropError> {
        serde_json::from_value(JsonValue::from_js(value)?)
            .map_err(|error| PropError(error.to_string()))
    }

    fn to_js<'a>(&self, host: &'a dyn JsHost) -> std::result::Result<JsOwned<'a>, PropError> {
        serde_json::to_value(self)
            .map_err(|error| PropError(error.to_string()))?
            .to_js(host)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct StoreState {
    pub schema_version: u32,
    pub active_project_id: Option<String>,
    pub projects: Vec<ProjectDto>,
    pub project_folders: Vec<FolderDto>,
    pub worktrees: Vec<WorktreeDto>,
    pub tasks: Vec<TaskDto>,
    pub orca_import: Option<ImportReceiptDto>,
    pub project_memos: StringMap<ProjectMemoDto>,
}

impl Default for StoreState {
    fn default() -> Self {
        Self {
            schema_version: 1,
            active_project_id: None,
            projects: Vec::new(),
            project_folders: Vec::new(),
            worktrees: Vec::new(),
            tasks: Vec::new(),
            orca_import: None,
            project_memos: StringMap::default(),
        }
    }
}

impl TryFrom<core::State> for StoreState {
    type Error = anyhow::Error;

    fn try_from(value: core::State) -> Result<Self> {
        Ok(Self {
            schema_version: value.schema_version,
            active_project_id: value.active_project_id,
            projects: value
                .projects
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<_>>()?,
            project_folders: value.project_folders.into_iter().map(Into::into).collect(),
            worktrees: value.worktrees.into_iter().map(Into::into).collect(),
            tasks: value.tasks.into_iter().map(Into::into).collect(),
            orca_import: value.orca_import.map(Into::into),
            project_memos: StringMap(
                value
                    .project_memos
                    .into_iter()
                    .map(|(id, memo)| (id, memo.into()))
                    .collect(),
            ),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDto {
    pub id: String,
    pub name: String,
    pub root: String,
    pub repository_roots: Vec<String>,
    pub folder_id: Option<String>,
    pub notify_on_agent_done: bool,
    pub wake_orchestrators_on_agent_done: bool,
    pub codex_account: ProjectCodexAccountDto,
    /// The engine retains unreadable future choices verbatim. Their unknown
    /// JSON keys are data, not bridge fields.
    pub orchestrator: JsonValue,
    /// Known policy fields are camelCase; unreadable future keys and values
    /// remain verbatim, including a policy that was not an object.
    pub delegation_policy: JsonValue,
    pub created_at: u64,
}

impl TryFrom<core::Project> for ProjectDto {
    type Error = anyhow::Error;

    fn try_from(value: core::Project) -> Result<Self> {
        let mut delegation_policy = serde_json::to_value(&value.delegation_policy)?;
        if let Some(object) = delegation_policy.as_object_mut() {
            for (disk, bridge) in [
                ("spread_work", "spreadWork"),
                ("parallel_shells", "parallelShells"),
            ] {
                if let Some(value) = object.remove(disk) {
                    object.insert(bridge.to_owned(), value);
                }
            }
        }
        Ok(Self {
            id: value.id,
            name: value.name,
            root: path_string(&value.root),
            repository_roots: value
                .repository_roots
                .iter()
                .map(|root| path_string(root))
                .collect(),
            folder_id: value.folder_id,
            notify_on_agent_done: value.notify_on_agent_done,
            wake_orchestrators_on_agent_done: value.wake_orchestrators_on_agent_done,
            codex_account: value.codex_account.into(),
            orchestrator: serde_json::to_value(value.orchestrator)?,
            delegation_policy,
            created_at: value.created_at,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCodexAccountDto {
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[prop(skip_none)]
    pub account_id: Option<String>,
}

impl From<core::ProjectCodexAccount> for ProjectCodexAccountDto {
    fn from(value: core::ProjectCodexAccount) -> Self {
        let (source, account_id) = match value {
            core::ProjectCodexAccount::Inherit => ("inherit", None),
            core::ProjectCodexAccount::SystemDefault => ("system_default", None),
            core::ProjectCodexAccount::Saved(id) => ("saved", Some(id)),
        };
        Self {
            source: source.to_owned(),
            account_id,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct FolderDto {
    pub id: String,
    pub name: String,
    pub parent_id: Option<String>,
    pub created_at: u64,
}

impl From<core::ProjectFolder> for FolderDto {
    fn from(value: core::ProjectFolder) -> Self {
        Self {
            id: value.id,
            name: value.name,
            parent_id: value.parent_id,
            created_at: value.created_at,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeDto {
    pub id: String,
    pub project_id: String,
    pub branch: String,
    pub path: String,
    pub is_primary: bool,
    pub repository_root: Option<String>,
    pub created_at: u64,
}

impl From<core::Worktree> for WorktreeDto {
    fn from(value: core::Worktree) -> Self {
        Self {
            id: value.id,
            project_id: value.project_id,
            branch: value.branch,
            path: path_string(&value.path),
            is_primary: value.is_primary,
            repository_root: value.repository_root.as_deref().map(path_string),
            created_at: value.created_at,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct TaskDto {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub details: String,
    /// Existing task status spelling (`todo`, `in_progress`, `done`).
    pub status: String,
    pub worktree_id: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

impl From<core::Task> for TaskDto {
    fn from(value: core::Task) -> Self {
        Self {
            id: value.id,
            project_id: value.project_id,
            title: value.title,
            details: value.details,
            status: value.status.as_str().to_owned(),
            worktree_id: value.worktree_id,
            created_at: value.created_at,
            updated_at: value.updated_at,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "item", rename_all = "camelCase")]
pub enum SearchHitDto {
    Project(ProjectDto),
    Worktree(WorktreeDto),
    Task(TaskDto),
}

impl TryFrom<core::SearchHit> for SearchHitDto {
    type Error = anyhow::Error;

    fn try_from(value: core::SearchHit) -> Result<Self> {
        Ok(match value {
            core::SearchHit::Project(project) => Self::Project(project.try_into()?),
            core::SearchHit::Worktree(worktree) => Self::Worktree(worktree.into()),
            core::SearchHit::Task(task) => Self::Task(task.into()),
        })
    }
}

impl JsValue for SearchHitDto {
    fn ts_type(types: &mut TsTypes) -> String {
        types.declare("SearchHitDto", |types| format!(
            "export type SearchHitDto = {{ kind: 'project'; item: {} }} | {{ kind: 'worktree'; item: {} }} | {{ kind: 'task'; item: {} }};",
            ProjectDto::ts_type(types), WorktreeDto::ts_type(types), TaskDto::ts_type(types),
        ))
    }

    fn from_js(value: JsIn<'_>) -> std::result::Result<Self, PropError> {
        serde_json::from_value(JsonValue::from_js(value)?)
            .map_err(|error| PropError(error.to_string()))
    }

    fn to_js<'a>(&self, host: &'a dyn JsHost) -> std::result::Result<JsOwned<'a>, PropError> {
        serde_json::to_value(self)
            .map_err(|error| PropError(error.to_string()))?
            .to_js(host)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct ProjectInspectionDto {
    pub root: String,
    pub exists: bool,
    pub repository_roots: Vec<String>,
    pub repository_count: usize,
    pub can_init_git: bool,
    pub discovery_complete: bool,
    pub warning: Option<String>,
}

impl From<core::ProjectInspection> for ProjectInspectionDto {
    fn from(value: core::ProjectInspection) -> Self {
        Self {
            root: path_string(&value.root),
            exists: value.exists,
            repository_roots: value
                .repository_roots
                .iter()
                .map(|root| path_string(root))
                .collect(),
            repository_count: value.repository_count,
            can_init_git: value.can_init_git,
            discovery_complete: value.discovery_complete,
            warning: value.warning,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct ProjectMemoDto {
    pub text: String,
    pub updated_at: u64,
}

impl From<riwork_core::project_memo::ProjectMemo> for ProjectMemoDto {
    fn from(value: riwork_core::project_memo::ProjectMemo) -> Self {
        Self {
            text: value.text,
            updated_at: value.updated_at,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct ImportReceiptDto {
    pub source: String,
    pub project_count: usize,
    pub folder_count: usize,
    pub worktree_count: usize,
    pub completed_at: u64,
}

impl From<riwork_core::orca_import::ImportReceipt> for ImportReceiptDto {
    fn from(value: riwork_core::orca_import::ImportReceipt) -> Self {
        Self {
            source: path_string(&value.source),
            project_count: value.project_count,
            folder_count: value.folder_count,
            worktree_count: value.worktree_count,
            completed_at: value.completed_at,
        }
    }
}

/// Both explicit reads and process samples use this shape. Activity timestamps
/// are intentionally not exposed in phase 1a: the engine sample has no activity
/// map, and mixing an absent map with `list_with_activity` would create spurious
/// differences in the subscription stream.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct SessionsSnapshot {
    pub sessions: Vec<ShellSessionDto>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct ShellSessionDto {
    pub user_opened: bool,
    pub parent_id: Option<String>,
    pub id: String,
    pub project_id: Option<String>,
    pub worktree_id: Option<String>,
    pub kind: String,
    pub cwd: String,
    pub command: Option<String>,
    pub editor_path: Option<String>,
    pub harness: Option<String>,
    pub unrestricted: bool,
    pub codex_account_id: Option<String>,
    pub codex_account_label: Option<String>,
    pub codex_account_email: Option<String>,
    pub codex_home: Option<String>,
    pub orchestrator_skill_loaded: bool,
    pub orchestrator_skill_version: Option<String>,
    pub orchestrator_delegation_version: Option<String>,
    pub orchestrator_project_root: Option<String>,
    pub created_at_unix: u64,
    pub alive: bool,
}

impl From<engine::ShellSession> for ShellSessionDto {
    fn from(value: engine::ShellSession) -> Self {
        Self {
            user_opened: value.user_opened,
            parent_id: value.parent_id,
            id: value.id,
            project_id: value.project_id,
            worktree_id: value.worktree_id,
            kind: match value.kind {
                engine::ShellKind::Project => "project",
                engine::ShellKind::Orchestrator => "orchestrator",
            }
            .to_owned(),
            cwd: path_string(&value.cwd),
            command: value.command,
            editor_path: value.editor_path.as_deref().map(path_string),
            harness: value.harness.map(|kind| kind.program().to_owned()),
            unrestricted: value.unrestricted,
            codex_account_id: value.codex_account_id,
            codex_account_label: value.codex_account_label,
            codex_account_email: value.codex_account_email,
            codex_home: value.codex_home.as_deref().map(path_string),
            orchestrator_skill_loaded: value.orchestrator_skill_loaded,
            orchestrator_skill_version: value.orchestrator_skill_version,
            orchestrator_delegation_version: value.orchestrator_delegation_version,
            orchestrator_project_root: value.orchestrator_project_root.as_deref().map(path_string),
            created_at_unix: value.created_at_unix,
            alive: value.alive,
        }
    }
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

macro_rules! snapshot_js_value {
    ($data:ty, $name:literal) => {
        impl JsValue for crate::hub::Snapshot<$data> {
            fn ts_type(types: &mut TsTypes) -> String {
                types.declare($name, |types| format!(
                    "export interface {} {{ kind: 'snapshot'; rev: number; data: {}; resync?: boolean; }}",
                    $name, <$data>::ts_type(types),
                ))
            }

            fn from_js(value: JsIn<'_>) -> std::result::Result<Self, PropError> {
                serde_json::from_value(JsonValue::from_js(value)?)
                    .map_err(|error| PropError(error.to_string()))
            }

            fn to_js<'a>(&self, host: &'a dyn JsHost) -> std::result::Result<JsOwned<'a>, PropError> {
                serde_json::to_value(self)
                    .map_err(|error| PropError(error.to_string()))?
                    .to_js(host)
            }
        }
    };
}

snapshot_js_value!(StoreState, "StoreEvent");
snapshot_js_value!(SessionsSnapshot, "SessionsEvent");

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bridge_camelizes_domain_fields_without_rewriting_persistence_or_opaque_data() {
        let persisted = json!({
            "schema_version": 1,
            "active_project_id": "project_key",
            "projects": [{
                "id": "project_key", "name": "Native bridge", "root": "/isolated/project",
                "created_at": 1,
                "codex_account": { "source": "saved", "account_id": "account_key" },
                "delegation_policy": {
                    "spread_work": true, "parallel_shells": 4,
                    "future_key": { "opaque_field": "keep_this" }
                },
                "orchestrator": { "future_mode": { "opaque_field": 1 } }
            }],
            "project_memos": { "project_key": { "text": "Memo", "updated_at": 2 } }
        });
        let core: core::State = serde_json::from_value(persisted).unwrap();
        let disk_before = serde_json::to_value(&core).unwrap();
        let bridge = serde_json::to_value(StoreState::try_from(core.clone()).unwrap()).unwrap();
        assert_eq!(bridge["schemaVersion"], 1);
        assert_eq!(bridge["activeProjectId"], "project_key");
        assert_eq!(
            bridge["projects"][0]["codexAccount"]["accountId"],
            "account_key"
        );
        assert_eq!(
            bridge["projects"][0]["delegationPolicy"]["spreadWork"],
            true
        );
        assert_eq!(
            bridge["projects"][0]["delegationPolicy"]["parallelShells"],
            4
        );
        assert_eq!(
            bridge["projects"][0]["delegationPolicy"]["future_key"],
            json!({ "opaque_field": "keep_this" })
        );
        assert_eq!(
            bridge["projects"][0]["orchestrator"],
            json!({ "future_mode": { "opaque_field": 1 } })
        );
        assert_eq!(bridge["projectMemos"]["project_key"]["updatedAt"], 2);
        assert!(bridge.get("schema_version").is_none());
        assert_eq!(serde_json::to_value(&core).unwrap(), disk_before);
        assert!(disk_before.get("schema_version").is_some());
    }

    #[test]
    fn session_dto_preserves_saved_identity_and_live_sample_fields() {
        let session: engine::ShellSession = serde_json::from_value(json!({
            "id": "00000000-0000-4000-8000-000000000001",
            "project_id": null, "worktree_id": null,
            "kind": "project", "cwd": "/isolated/worktree", "command": null,
            "created_at_unix": 5, "harness": "codex",
            "codex_account_id": "account_key", "user_opened": true
        }))
        .unwrap();
        let mut dto = ShellSessionDto::from(session);
        dto.alive = true;
        let value = serde_json::to_value(SessionsSnapshot {
            sessions: vec![dto],
        })
        .unwrap();
        assert_eq!(value["sessions"][0]["codexAccountId"], "account_key");
        assert_eq!(value["sessions"][0]["createdAtUnix"], 5);
        assert_eq!(value["sessions"][0]["alive"], true);
        assert_eq!(value["sessions"][0]["harness"], "codex");
        assert!(value["sessions"][0].get("codex_account_id").is_none());
    }
}
