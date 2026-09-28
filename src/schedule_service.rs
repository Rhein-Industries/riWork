//! Validated schedule operations shared by the CLI and MCP bridge.
//! Dispatch remains owned by the desktop scheduler in `schedules`.

use chrono::DateTime;
use serde::Serialize;
#[cfg(test)]
use std::path::PathBuf;
use uuid::Uuid;

use crate::{
    schedules::{self, Schedule, ScheduleStore, Scope, Target, Timing},
    sessions::SessionManager,
    store::Store,
};

#[derive(Clone, Debug)]
pub struct ScopeInput {
    pub scope: String,
    pub project_id: Option<String>,
    pub worktree_id: Option<String>,
}

impl ScopeInput {
    pub fn resolve(&self) -> Result<Scope, ScheduleError> {
        match self.scope.as_str() {
            "app" if self.project_id.is_none() && self.worktree_id.is_none() => Ok(Scope::App),
            "project" if self.worktree_id.is_none() => Ok(Scope::Project {
                project_id: full_id(self.project_id.as_deref(), "project_id")?.to_owned(),
            }),
            "workspace" => Ok(Scope::Workspace {
                project_id: full_id(self.project_id.as_deref(), "project_id")?.to_owned(),
                worktree_id: full_id(self.worktree_id.as_deref(), "worktree_id")?.to_owned(),
            }),
            "app" | "project" => Err(ScheduleError::invalid(
                "Scope has unexpected project/worktree identity",
            )),
            _ => Err(ScheduleError::invalid(
                "scope must be app, project, or workspace",
            )),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ScheduleKey {
    pub id: String,
    pub revision: u64,
    pub scope: ScopeInput,
    pub shell_id: String,
}

#[derive(Clone, Debug)]
pub struct CreateRequest {
    pub scope: ScopeInput,
    pub shell_id: String,
    pub title: String,
    pub prompt: String,
    pub at: String,
    pub every_minutes: Option<u64>,
}

#[derive(Clone, Debug)]
pub enum RepeatChange {
    Keep,
    Once,
    EveryMinutes(u64),
}

#[derive(Clone, Debug)]
pub struct UpdateRequest {
    pub key: ScheduleKey,
    pub title: Option<String>,
    pub prompt: Option<String>,
    pub at: String,
    pub repeat: RepeatChange,
}

#[derive(Clone, Debug, Serialize)]
pub struct ScheduleError {
    pub code: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<Schedule>,
}

impl ScheduleError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            current: None,
        }
    }
    fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid_argument", message)
    }
    fn store(message: impl Into<String>) -> Self {
        Self::new("store_error", message)
    }
    fn conflict(message: impl Into<String>, current: Schedule) -> Self {
        Self {
            code: "revision_conflict",
            message: message.into(),
            current: Some(current),
        }
    }
}

pub struct ScheduleService {
    workspace: Store,
    schedules: ScheduleStore,
    sessions: SessionManager,
}

impl ScheduleService {
    pub fn open_default() -> Result<Self, ScheduleError> {
        let sessions = SessionManager::open_default().map_err(ScheduleError::store)?;
        let home = sessions.state_home().to_path_buf();
        Ok(Self {
            workspace: Store::open(home.clone()).map_err(ScheduleError::store)?,
            schedules: ScheduleStore::at(home).map_err(ScheduleError::store)?,
            sessions,
        })
    }

    #[cfg(test)]
    pub fn at(home: PathBuf) -> Result<Self, ScheduleError> {
        Ok(Self {
            workspace: Store::open(home.clone()).map_err(ScheduleError::store)?,
            schedules: ScheduleStore::at(home.clone()).map_err(ScheduleError::store)?,
            sessions: SessionManager::at(home).map_err(ScheduleError::store)?,
        })
    }

    pub fn list(&self, scope: Option<&ScopeInput>) -> Result<Vec<Schedule>, ScheduleError> {
        let scope = scope.map(ScopeInput::resolve).transpose()?;
        let mut items = self.schedules.list().map_err(ScheduleError::store)?;
        if let Some(scope) = scope {
            items.retain(|item| item.target.scope == scope);
        }
        items.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(items)
    }

    pub fn show(&self, id: &str) -> Result<Schedule, ScheduleError> {
        full_id(Some(id), "schedule_id")?;
        self.schedules
            .list()
            .map_err(ScheduleError::store)?
            .into_iter()
            .find(|item| item.id == id)
            .ok_or_else(|| ScheduleError::new("not_found", format!("Schedule {id} was not found")))
    }

    pub fn create(&self, request: CreateRequest) -> Result<Schedule, ScheduleError> {
        let scope = request.scope.resolve()?;
        full_id(Some(&request.shell_id), "shell_id")?;
        let timing = timing(&request.at, request.every_minutes)?;
        let state = self.workspace.snapshot().map_err(ScheduleError::store)?;
        // `bind` verifies a live, scoped, known provider session and pins its identity.
        let target = Target::bind(scope, &state, &self.sessions, &request.shell_id)
            .map_err(|error| ScheduleError::new("binding_failed", error))?;
        self.schedules
            .save(
                None,
                request.title,
                request.prompt,
                target,
                timing,
                schedules::now(),
            )
            .map_err(classify_save_error)
    }

    pub fn update(&self, request: UpdateRequest) -> Result<Schedule, ScheduleError> {
        let current = self.checked(&request.key)?;
        let at = parse_at(&request.at)?;
        let every_minutes = match request.repeat {
            RepeatChange::Keep => match current.timing {
                Timing::Once { .. } => None,
                Timing::Interval { seconds, .. } => Some(seconds / 60),
            },
            RepeatChange::Once => None,
            RepeatChange::EveryMinutes(minutes) => Some(minutes),
        };
        let timing = timing_at(at, every_minutes)?;
        // Reuse the stored target exactly. An edit cannot silently retarget or
        // acknowledge a failed/uncertain outcome on a different provider.
        self.schedules
            .save(
                Some((&current.id, request.key.revision)),
                request.title.unwrap_or(current.title),
                request.prompt.unwrap_or(current.prompt),
                current.target,
                timing,
                schedules::now(),
            )
            .map_err(|error| self.classify_mutation_error(&request.key.id, error))
    }

    pub fn pause(&self, key: &ScheduleKey, paused: bool) -> Result<Schedule, ScheduleError> {
        self.checked(key)?;
        self.schedules
            .pause(&key.id, key.revision, paused, schedules::now())
            .map_err(|error| self.classify_mutation_error(&key.id, error))
    }

    pub fn delete(&self, key: &ScheduleKey) -> Result<Schedule, ScheduleError> {
        let current = self.checked(key)?;
        self.schedules
            .delete(&key.id, key.revision)
            .map_err(|error| self.classify_mutation_error(&key.id, error))?;
        Ok(current)
    }

    fn checked(&self, key: &ScheduleKey) -> Result<Schedule, ScheduleError> {
        let scope = key.scope.resolve()?;
        full_id(Some(&key.shell_id), "shell_id")?;
        if key.revision == 0 {
            return Err(ScheduleError::invalid(
                "revision must be a positive integer",
            ));
        }
        let current = self.show(&key.id)?;
        if current.target.scope != scope || current.target.shell_id != key.shell_id {
            return Err(ScheduleError {
                code: "target_mismatch",
                message: "Scope or shell_id does not match the pinned schedule target".into(),
                current: Some(current),
            });
        }
        if current.revision != key.revision {
            return Err(ScheduleError::conflict(
                "Schedule or run status changed; read the current revision before retrying",
                current,
            ));
        }
        Ok(current)
    }

    fn classify_mutation_error(&self, id: &str, error: String) -> ScheduleError {
        match self.show(id) {
            Ok(current) if error.contains("changed") => ScheduleError::conflict(error, current),
            Err(found) if found.code == "not_found" => found,
            _ => classify_save_error(error),
        }
    }
}

fn classify_save_error(error: String) -> ScheduleError {
    let code = if error.contains("reviewing this outcome") {
        "review_required"
    } else if error.contains("Edit the schedule") || error.contains("Edit the one-time") {
        "future_run_required"
    } else if error.contains("future run")
        || error.contains("First run")
        || error.contains("title")
        || error.contains("prompt")
        || error.contains("Repeat interval")
        || error.contains("Date")
        || error.contains("cannot be scheduled")
    {
        "invalid_argument"
    } else {
        "store_error"
    };
    ScheduleError::new(code, error)
}

fn full_id<'a>(value: Option<&'a str>, name: &str) -> Result<&'a str, ScheduleError> {
    let value = value.ok_or_else(|| ScheduleError::invalid(format!("{name} is required")))?;
    if Uuid::parse_str(value).is_ok_and(|id| id.to_string() == value) {
        Ok(value)
    } else {
        Err(ScheduleError::invalid(format!(
            "{name} must be a full canonical UUID"
        )))
    }
}

fn parse_at(value: &str) -> Result<u64, ScheduleError> {
    let date = DateTime::parse_from_rfc3339(value).map_err(|_| {
        ScheduleError::invalid("at must be RFC 3339 with seconds and an explicit timezone, e.g. 2026-10-05T09:00:00+02:00")
    })?;
    if date.timestamp_subsec_nanos() != 0 {
        return Err(ScheduleError::invalid("at must identify a whole second"));
    }
    u64::try_from(date.timestamp()).map_err(|_| ScheduleError::invalid("at must be after 1970"))
}

fn timing(value: &str, every_minutes: Option<u64>) -> Result<Timing, ScheduleError> {
    timing_at(parse_at(value)?, every_minutes)
}

fn timing_at(at: u64, every_minutes: Option<u64>) -> Result<Timing, ScheduleError> {
    let timing = match every_minutes {
        Some(minutes) => Timing::Interval {
            first: at,
            seconds: minutes
                .checked_mul(60)
                .ok_or_else(|| ScheduleError::invalid("every_minutes is too large"))?,
        },
        None => Timing::Once { at },
    };
    timing.validate().map_err(ScheduleError::invalid)?;
    Ok(timing)
}
