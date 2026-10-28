//! Automations UI boundary. The engine still owns the ledger and every dispatch.
//!
//! The process initializer starts one ticker, even when no panel is mounted.
//! Subscribers receive changed snapshots only; there are no window-owned polls.
//! Test-support and disposable UI fixtures never start the real dispatcher.
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock, PoisonError,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use chrono::{DateTime, Local, SecondsFormat};
use nsh_core::prelude::*;
use riwork_engine::{
    chat::{
        log,
        model::{ApprovalMode, Provider},
    },
    schedule_service::{self, ScopeInput},
    schedules::{self as engine, Schedule, ScheduleStore, Scope, Target, Timing},
    sessions::{SessionManager, ShellSession},
    store::Store,
};
use serde::{Deserialize, Serialize};

use crate::{
    hub::{Hub, Snapshot},
    service::{self, ProcessService},
};

pub const SCHEMA: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScheduleScopeDto {
    pub scope: String,
    pub project_id: Option<String>,
    pub worktree_id: Option<String>,
}
impl ScheduleScopeDto {
    fn resolve(&self) -> Result<Scope, String> {
        ScopeInput {
            scope: self.scope.clone(),
            project_id: self.project_id.clone(),
            worktree_id: self.worktree_id.clone(),
        }
        .resolve()
        .map_err(|e| format!("{}: {}", e.code, e.message))
    }
}
impl From<&Scope> for ScheduleScopeDto {
    fn from(scope: &Scope) -> Self {
        match scope {
            Scope::App => Self {
                scope: "app".into(),
                project_id: None,
                worktree_id: None,
            },
            Scope::Project { project_id } => Self {
                scope: "project".into(),
                project_id: Some(project_id.clone()),
                worktree_id: None,
            },
            Scope::Workspace {
                project_id,
                worktree_id,
            } => Self {
                scope: "workspace".into(),
                project_id: Some(project_id.clone()),
                worktree_id: Some(worktree_id.clone()),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScheduleKeyDto {
    pub id: String,
    pub revision: u64,
    pub scope: ScheduleScopeDto,
    pub target_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScheduleChatOptionsDto {
    pub provider: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub fast: bool,
    pub permission: String,
    pub account_id: Option<String>,
}
impl Default for ScheduleChatOptionsDto {
    fn default() -> Self {
        Self {
            provider: "codex".into(),
            model: None,
            effort: None,
            fast: false,
            permission: "supervised".into(),
            account_id: None,
        }
    }
}
impl ScheduleChatOptionsDto {
    fn parsed(&self) -> Result<(Provider, ApprovalMode), String> {
        schedule_service::chat_options(Some(&self.provider), Some(&self.permission))
            .map_err(|e| format!("{}: {}", e.code, e.message))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScheduleBindingDto {
    pub scope: ScheduleScopeDto,
    /// `shell` is explicit shell selection; `legacy_chat` preserves old chat schedules.
    pub kind: String,
    pub id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateScheduleDto {
    pub scope: ScheduleScopeDto,
    pub title: String,
    pub prompt: String,
    /// RFC 3339, whole seconds and explicit offset; the engine persists UTC seconds.
    pub at: String,
    pub every_minutes: Option<u64>,
    pub binding: Option<ScheduleBindingDto>,
    pub options: Option<ScheduleChatOptionsDto>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateScheduleDto {
    pub key: ScheduleKeyDto,
    pub title: String,
    pub prompt: String,
    pub at: String,
    pub every_minutes: Option<u64>,
    /// Absent means retain the exact pinned target. Present means an explicit rebind.
    pub binding: Option<ScheduleBindingDto>,
    /// Options-only editing keeps provider, account, destination id and root pinned.
    pub options: Option<ScheduleChatOptionsDto>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleRunDto {
    pub due_at: u64,
    pub observed_at: u64,
    pub due_display: String,
    pub observed_display: String,
    pub outcome: String,
    pub message: String,
    pub created_chat_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleDto {
    pub key: ScheduleKeyDto,
    pub title: String,
    pub prompt: String,
    pub destination: String,
    pub destination_label: String,
    pub options: Option<ScheduleChatOptionsDto>,
    pub account_label: String,
    pub first_at: u64,
    pub first_iso: String,
    pub every_minutes: Option<u64>,
    pub paused: bool,
    pub review_required: bool,
    pub next_run: Option<u64>,
    pub next_display: Option<String>,
    /// The unchanged engine ledger stores one last run, not a separate history log.
    pub last_run: Option<ScheduleRunDto>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleTargetDto {
    pub binding: ScheduleBindingDto,
    pub label: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleChatDto {
    pub id: String,
    pub project_id: String,
    pub title: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleWorktreeDto {
    pub id: String,
    pub project_id: String,
    pub label: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct SchedulesDto {
    pub schema: u32,
    pub now: u64,
    pub rows: Vec<ScheduleDto>,
    pub targets: Vec<ScheduleTargetDto>,
    pub chats: Vec<ScheduleChatDto>,
    pub worktrees: Vec<ScheduleWorktreeDto>,
    pub error: Option<String>,
    pub runtime_error: Option<String>,
}
impl JsValue for Snapshot<SchedulesDto> {
    fn ts_type(types: &mut nsh_core::element::TsTypes) -> String {
        types.declare("SchedulesEvent", |types| format!("export interface SchedulesEvent {{ kind: 'snapshot'; rev: number; data: {}; resync?: boolean; }}", SchedulesDto::ts_type(types)))
    }
    fn from_js(value: nsh_core::element::JsIn<'_>) -> Result<Self, nsh_core::element::PropError> {
        serde_json::from_value(nsh_core::JsonValue::from_js(value)?)
            .map_err(|e| nsh_core::element::PropError(e.to_string()))
    }
    fn to_js<'a>(
        &self,
        host: &'a dyn nsh_core::element::JsHost,
    ) -> Result<nsh_core::element::JsOwned<'a>, nsh_core::element::PropError> {
        serde_json::to_value(self)
            .map_err(|e| nsh_core::element::PropError(e.to_string()))?
            .to_js(host)
    }
}
// Passing time alone must not wake an idle window. Absolute labels are computed
// in Rust; a fresh timestamp comes with the next actual data/status change.
impl PartialEq for SchedulesDto {
    fn eq(&self, other: &Self) -> bool {
        self.schema == other.schema
            && self.rows == other.rows
            && self.targets == other.targets
            && self.chats == other.chats
            && self.worktrees == other.worktrees
            && self.error == other.error
            && self.runtime_error == other.runtime_error
    }
}

pub fn display_time(at: u64) -> String {
    i64::try_from(at)
        .ok()
        .and_then(|at| DateTime::from_timestamp(at, 0))
        .map(|at| {
            at.with_timezone(&Local)
                .format("%Y-%m-%d %H:%M:%S %:z")
                .to_string()
        })
        .unwrap_or_else(|| at.to_string())
}
fn iso_time(at: u64) -> String {
    i64::try_from(at)
        .ok()
        .and_then(|at| DateTime::from_timestamp(at, 0))
        .map(|at| {
            at.with_timezone(&Local)
                .to_rfc3339_opts(SecondsFormat::Secs, false)
        })
        .unwrap_or_default()
}
fn permission_name(mode: ApprovalMode) -> String {
    match mode {
        ApprovalMode::Supervised => "supervised",
        ApprovalMode::AutoEdit => "auto-edit",
        ApprovalMode::Full => "full",
        ApprovalMode::Plan => "plan",
    }
    .into()
}
pub fn schedule_dto(row: &Schedule) -> ScheduleDto {
    let fresh = row.target.new_chat.as_ref();
    let provider = fresh.map(|f| crate::chat::dto::wire_name(&f.provider));
    ScheduleDto {
        key: ScheduleKeyDto {
            id: row.id.clone(),
            revision: row.revision,
            scope: (&row.target.scope).into(),
            target_id: row.target.shell_id.clone(),
        },
        title: row.title.clone(),
        prompt: row.prompt.clone(),
        destination: if fresh.is_some() {
            "new_chat"
        } else if row.target.chat.is_some() {
            "legacy_chat"
        } else {
            "shell"
        }
        .into(),
        destination_label: fresh
            .map(|f| {
                format!(
                    "New {} project chat · {}",
                    provider.as_deref().unwrap_or(""),
                    f.root.display()
                )
            })
            .unwrap_or_else(|| {
                format!(
                    "{} · {} · {}",
                    row.target.scope.label(),
                    if row.target.chat.is_some() {
                        "orchestrator chat"
                    } else {
                        "AI shell"
                    },
                    row.target.shell_id
                )
            }),
        options: fresh.map(|f| ScheduleChatOptionsDto {
            provider: provider.clone().unwrap(),
            model: f.model.clone(),
            effort: f.effort.clone(),
            fast: f.fast,
            permission: permission_name(f.approval_mode),
            account_id: f.codex_account_id.clone(),
        }),
        account_label: fresh
            .map(|f| match f.provider {
                Provider::Codex => f
                    .codex_account_id
                    .clone()
                    .unwrap_or_else(|| "System default".into()),
                Provider::Claude => "Claude system login".into(),
                Provider::Muse => "Muse system login".into(),
            })
            .unwrap_or_else(|| "Pinned with target".into()),
        first_at: row.timing.first(),
        first_iso: iso_time(row.timing.first()),
        every_minutes: match row.timing {
            Timing::Once { .. } => None,
            Timing::Interval { seconds, .. } => Some(seconds / 60),
        },
        paused: row.paused,
        review_required: row.review_required,
        next_run: row.next_run,
        next_display: row.next_run.map(display_time),
        last_run: row.last_run.as_ref().map(|run| ScheduleRunDto {
            due_at: run.due_at,
            observed_at: run.observed_at,
            due_display: display_time(run.due_at),
            observed_display: display_time(run.observed_at),
            outcome: serde_json::to_value(&run.outcome)
                .unwrap()
                .as_str()
                .unwrap()
                .into(),
            message: run.message.clone(),
            created_chat_id: run.created_chat_id.clone(),
        }),
    }
}

fn invalid(message: impl Into<String>) -> String {
    format!("invalid_argument: {}", message.into())
}
fn checked_id(id: &str) -> Result<(), String> {
    service::validate_uuid(id).map_err(invalid)
}
pub fn parse_timing(at: &str, minutes: Option<u64>) -> Result<Timing, String> {
    let date = DateTime::parse_from_rfc3339(at)
        .map_err(|_| invalid("Use YYYY-MM-DDTHH:MM:SS+HH:MM with an explicit UTC offset"))?;
    if date.timestamp_subsec_nanos() != 0 {
        return Err(invalid("Date must identify a whole second"));
    }
    let at = u64::try_from(date.timestamp()).map_err(|_| invalid("Date must be after 1970"))?;
    let timing = match minutes {
        None => Timing::Once { at },
        Some(minutes) => Timing::Interval {
            first: at,
            seconds: minutes
                .checked_mul(60)
                .ok_or_else(|| invalid("Repeat interval is too large"))?,
        },
    };
    timing.validate().map_err(invalid)?;
    Ok(timing)
}
fn store_error(error: String) -> String {
    let code = if error.contains("changed") || error.contains("reopen") {
        "revision_conflict"
    } else if error.contains("reviewing this outcome") {
        "review_required"
    } else if error.contains("Edit the schedule") || error.contains("Edit the one-time") {
        "future_run_required"
    } else if error.contains("title")
        || error.contains("prompt")
        || error.contains("First run")
        || error.contains("Repeat interval")
        || error.contains("cannot be scheduled")
        || error.contains("Date")
    {
        "invalid_argument"
    } else if error.contains("deleted") {
        "not_found"
    } else {
        "store_error"
    };
    format!("{code}: {error}")
}

/// Explicit-home adapter: no environment mutation or default-home fallback.
/// ScheduleService's explicit-home constructor is engine-test-only, so these
/// calls use the same public Target/Scope/Timing/ledger operations as GPUI.
pub struct EngineSchedules {
    home: PathBuf,
}
impl EngineSchedules {
    pub fn new(home: PathBuf) -> Self {
        Self { home }
    }
    fn ledger(&self) -> Result<ScheduleStore, String> {
        ScheduleStore::at(self.home.clone()).map_err(store_error)
    }
    pub fn list(&self, scope: Option<&ScheduleScopeDto>) -> Result<Vec<ScheduleDto>, String> {
        let scope = scope.map(ScheduleScopeDto::resolve).transpose()?;
        let mut rows = self.ledger()?.list().map_err(store_error)?;
        rows.retain(|row| {
            scope
                .as_ref()
                .is_none_or(|scope| &row.target.scope == scope)
        });
        rows.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(rows.iter().map(schedule_dto).collect())
    }
    fn row(&self, id: &str) -> Result<Schedule, String> {
        checked_id(id)?;
        self.ledger()?
            .list()
            .map_err(store_error)?
            .into_iter()
            .find(|row| row.id == id)
            .ok_or_else(|| format!("not_found: Schedule {id} was not found"))
    }
    pub fn show(&self, id: &str) -> Result<ScheduleDto, String> {
        self.row(id).map(|row| schedule_dto(&row))
    }
    fn checked(&self, key: &ScheduleKeyDto) -> Result<Schedule, String> {
        checked_id(&key.target_id)?;
        if key.revision == 0 {
            return Err(invalid("revision must be a positive integer"));
        }
        let scope = key.scope.resolve()?;
        let row = self.row(&key.id)?;
        if row.target.shell_id != key.target_id || row.target.scope != scope {
            return Err(
                "target_mismatch: Scope or target ID does not match the pinned schedule target"
                    .into(),
            );
        }
        if row.revision != key.revision {
            return Err(
                "revision_conflict: Schedule or run status changed; reopen it before saving".into(),
            );
        }
        Ok(row)
    }
    fn bind(&self, binding: &ScheduleBindingDto) -> Result<Target, String> {
        checked_id(&binding.id)?;
        let scope = binding.scope.resolve()?;
        let state = Store::open(self.home.clone())?.snapshot()?;
        let target = match binding.kind.as_str() {
            "shell" => Target::bind_shell(
                scope,
                &state,
                &SessionManager::at(self.home.clone())?,
                &binding.id,
            ),
            "legacy_chat" => {
                let chat = self
                    .ledger()?
                    .chat_orchestrators()
                    .into_iter()
                    .find(|chat| chat.id == binding.id)
                    .ok_or("binding_failed: This chat target is no longer available")?;
                Target::bind_chat(scope, &state, &chat)
            }
            _ => return Err(invalid("target kind must be shell or legacy_chat")),
        };
        target.map_err(|e| format!("binding_failed: {e}"))
    }
    pub fn create(&self, request: CreateScheduleDto, fresh: bool) -> Result<ScheduleDto, String> {
        let timing = parse_timing(&request.at, request.every_minutes)?;
        let scope = request.scope.resolve()?;
        let target = if fresh {
            if request.binding.is_some() {
                return Err(invalid("New chat cannot also name an existing shell"));
            }
            let Scope::Project { project_id } = scope else {
                return Err(invalid("New chats require project scope"));
            };
            let options = request.options.unwrap_or_default();
            let (provider, permission) = options.parsed()?;
            let state = Store::open(self.home.clone())?.snapshot()?;
            Target::bind_new_chat(
                &self.home,
                &state,
                &project_id,
                provider,
                options.model,
                options.effort,
                options.fast,
                permission,
                options.account_id.as_deref(),
            )
            .map_err(|e| format!("binding_failed: {e}"))?
        } else {
            if request.options.is_some() {
                return Err(invalid("Existing shell options are pinned by its session"));
            }
            let binding = request
                .binding
                .ok_or_else(|| invalid("Choose an existing target session"))?;
            if binding.scope.resolve()? != scope {
                return Err(invalid(
                    "Selected target does not match the requested scope",
                ));
            }
            self.bind(&binding)?
        };
        self.ledger()?
            .save(
                None,
                request.title,
                request.prompt,
                target,
                timing,
                engine::now(),
            )
            .map(|row| schedule_dto(&row))
            .map_err(store_error)
    }
    pub fn update(&self, request: UpdateScheduleDto) -> Result<ScheduleDto, String> {
        let row = self.checked(&request.key)?;
        let mut target = row.target;
        if let Some(binding) = request.binding {
            if target.new_chat.is_some() {
                return Err(invalid(
                    "A new-chat destination cannot be rebound as a shell",
                ));
            }
            target = self.bind(&binding)?;
        }
        if let Some(options) = request.options {
            let (provider, permission) = options.parsed()?;
            let fresh = target
                .new_chat
                .as_ref()
                .ok_or_else(|| invalid("Existing shell options are pinned by its session"))?;
            if fresh.provider != provider || fresh.codex_account_id != options.account_id {
                return Err("target_mismatch: Provider and account stay pinned when editing; create a new automation to change them".into());
            }
            target
                .set_new_chat_options(options.model, options.effort, options.fast, permission)
                .map_err(invalid)?;
        }
        if target.new_chat.is_some() {
            target
                .validate_new_chat(&self.home, &Store::open(self.home.clone())?.snapshot()?)
                .map_err(|e| format!("binding_failed: {e}"))?;
        }
        self.ledger()?
            .save(
                Some((&request.key.id, request.key.revision)),
                request.title,
                request.prompt,
                target,
                parse_timing(&request.at, request.every_minutes)?,
                engine::now(),
            )
            .map(|row| schedule_dto(&row))
            .map_err(store_error)
    }
    pub fn pause(&self, key: &ScheduleKeyDto, paused: bool) -> Result<ScheduleDto, String> {
        self.checked(key)?;
        self.ledger()?
            .pause(&key.id, key.revision, paused, engine::now())
            .map(|row| schedule_dto(&row))
            .map_err(store_error)
    }
    pub fn delete(&self, key: &ScheduleKeyDto) -> Result<ScheduleDto, String> {
        let row = self.checked(key)?;
        self.ledger()?
            .delete(&key.id, key.revision)
            .map_err(store_error)?;
        Ok(schedule_dto(&row))
    }
    fn read(&self, runtime_error: Option<String>) -> Result<SchedulesDto, String> {
        let state = Store::open(self.home.clone())?.snapshot()?;
        let ledger = self.ledger()?;
        let rows = self.list(None)?;
        // Fixture/test builds must never inspect a real tmux server while reading UI data.
        let shells = if fixture_home(&self.home) {
            Vec::new()
        } else {
            SessionManager::at(self.home.clone())?.sample(false)?.shells
        };
        let mut scopes = vec![Scope::App];
        scopes.extend(state.projects.iter().map(|p| Scope::Project {
            project_id: p.id.clone(),
        }));
        scopes.extend(state.worktrees.iter().map(|w| Scope::Workspace {
            project_id: w.project_id.clone(),
            worktree_id: w.id.clone(),
        }));
        let mut targets = target_choices(&state, &shells, &scopes);
        for chat in ledger.chat_orchestrators() {
            for scope in &scopes {
                if scope.matches_chat(&state, &chat) {
                    targets.push(ScheduleTargetDto {
                        binding: ScheduleBindingDto {
                            scope: scope.into(),
                            kind: "legacy_chat".into(),
                            id: chat.id.clone(),
                        },
                        label: format!(
                            "{} chat · {}",
                            engine::chat_harness(chat.provider).program(),
                            chat.id
                        ),
                    });
                }
            }
        }
        let chats = log::read_infos(&self.home)
            .into_iter()
            .filter(|chat| chat.orchestrator.is_none())
            .filter_map(|chat| {
                Some(ScheduleChatDto {
                    id: chat.id,
                    project_id: chat.project_id?,
                    title: chat.title,
                })
            })
            .collect();
        Ok(SchedulesDto {
            schema: SCHEMA,
            now: engine::now(),
            rows,
            targets,
            chats,
            worktrees: state
                .worktrees
                .iter()
                .map(|w| ScheduleWorktreeDto {
                    id: w.id.clone(),
                    project_id: w.project_id.clone(),
                    label: w.branch.clone(),
                })
                .collect(),
            error: None,
            runtime_error,
        })
    }
}
pub fn target_choices(
    state: &riwork_engine::store::State,
    shells: &[ShellSession],
    scopes: &[Scope],
) -> Vec<ScheduleTargetDto> {
    shells
        .iter()
        .filter(|shell| shell.alive && shell.harness.is_some_and(|h| h.schedulable()))
        .flat_map(|shell| {
            scopes
                .iter()
                .filter(move |scope| scope.matches_explicit_shell(state, shell))
                .map(move |scope| ScheduleTargetDto {
                    binding: ScheduleBindingDto {
                        scope: scope.into(),
                        kind: "shell".into(),
                        id: shell.id.clone(),
                    },
                    label: format!("{} · {}", shell.harness.unwrap().program(), shell.id),
                })
        })
        .collect()
}

fn fixture_home(home: &Path) -> bool {
    if cfg!(any(test, feature = "test-support")) {
        return true;
    }
    #[cfg(debug_assertions)]
    {
        riwork_nsh_workspace::session_io::is_fixture(home)
    }
    #[cfg(not(debug_assertions))]
    {
        let _ = home;
        false
    }
}

pub struct ScheduleProcess {
    home: PathBuf,
    hub: Hub<SchedulesDto>,
    wake: mpsc::SyncSender<()>,
    dispatch: bool,
    observing: AtomicBool,
}
static PROCESS: OnceLock<Arc<ScheduleProcess>> = OnceLock::new();
static START: Mutex<()> = Mutex::new(());

/// Called by the root initializer, not by a panel. Never changes HOME/RIWORK_HOME.
pub fn start_process_ticker(home: PathBuf) -> Result<(), String> {
    let _start = START.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(process) = PROCESS.get() {
        return if process.home == home {
            Ok(())
        } else {
            Err("conflict: schedule process home is already fixed".into())
        };
    }
    let (tx, rx) = mpsc::sync_channel(1);
    let process = Arc::new(ScheduleProcess {
        dispatch: !fixture_home(&home),
        home,
        hub: Hub::pending(),
        wake: tx,
        observing: AtomicBool::new(false),
    });
    let worker = process.clone();
    thread::Builder::new()
        .name("riwork-schedules".into())
        .spawn(move || {
            let adapter = EngineSchedules::new(worker.home.clone());
            let mut trackers = HashMap::new();
            let mut runtime_error = None;
            let mut last: Option<SchedulesDto> = None;
            let mut next_tick = Instant::now() + Duration::from_secs(1);
            loop {
                // Watcher wakes refresh data, but must not increase dispatch cadence.
                let _ = rx.recv_timeout(next_tick.saturating_duration_since(Instant::now()));
                let tick = Instant::now() >= next_tick;
                if tick {
                    next_tick = Instant::now() + Duration::from_secs(1);
                }
                if tick && worker.dispatch {
                    runtime_error = adapter
                        .ledger()
                        .and_then(|store| store.tick_tracked(engine::now(), &mut trackers))
                        .err();
                }
                if worker.hub.has_subscribers() {
                    match adapter.read(runtime_error.clone()) {
                        Ok(data) => {
                            worker.hub.publish(data.clone());
                            last = Some(data);
                        }
                        Err(error) => {
                            let mut data = last.clone().unwrap_or(SchedulesDto {
                                schema: SCHEMA,
                                now: engine::now(),
                                rows: Vec::new(),
                                targets: Vec::new(),
                                chats: Vec::new(),
                                worktrees: Vec::new(),
                                error: None,
                                runtime_error: None,
                            });
                            data.error = Some(error);
                            data.runtime_error = runtime_error.clone();
                            worker.hub.publish(data);
                        }
                    }
                }
            }
        })
        .map_err(|e| format!("io: start schedule process: {e}"))?;
    PROCESS
        .set(process)
        .map_err(|_| "conflict: schedule process initialized twice".to_owned())
}
/// Reuses the established process watcher (schedules, sessions, settings, chats).
pub fn observe_process_changes(process: &ProcessService) {
    if let Some(schedules) = PROCESS.get()
        && !schedules.observing.swap(true, Ordering::SeqCst)
    {
        process.observe_changes(schedules.wake.clone());
    }
}
async fn process() -> Result<Arc<ScheduleProcess>, String> {
    service::blocking(|| {
        let home = service::process_home()?;
        PROCESS
            .get()
            .filter(|process| process.home == home)
            .cloned()
            .ok_or_else(|| {
                "unavailable: schedules process has not been initialized by the host".into()
            })
    })
    .await
}
/// The UI uses the frozen bridge codes, retaining the engine's precise reason
/// in the message (including revision_conflict so a draft asks to reopen).
fn bridge_error(error: String) -> String {
    let reason = error.split_once(':').map(|(code, _)| code).unwrap_or("");
    let code = match reason {
        "invalid_argument" => "invalid",
        "revision_conflict" => "conflict",
        "binding_failed" | "target_mismatch" | "review_required" | "future_run_required" => {
            "refused"
        }
        "store_error" => "io",
        "refused" | "broken" | "not_found" | "invalid" | "conflict" | "unavailable" | "io" => {
            return error;
        }
        _ => "io",
    };
    format!("{code}: {error}")
}
async fn operation<T: Send + 'static>(
    work: impl FnOnce(&EngineSchedules) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let process = process().await?;
    service::blocking(move || {
        let result = work(&EngineSchedules::new(process.home.clone())).map_err(bridge_error);
        let _ = process.wake.try_send(());
        result
    })
    .await
}

#[module]
pub struct Schedules;
#[module]
impl Schedules {
    pub async fn list(scope: Option<ScheduleScopeDto>) -> Result<Vec<ScheduleDto>, String> {
        operation(move |service| service.list(scope.as_ref())).await
    }
    pub async fn show(id: String) -> Result<ScheduleDto, String> {
        operation(move |service| service.show(&id)).await
    }
    pub async fn create(request: CreateScheduleDto) -> Result<ScheduleDto, String> {
        operation(move |service| service.create(request, false)).await
    }
    pub async fn create_chat(request: CreateScheduleDto) -> Result<ScheduleDto, String> {
        operation(move |service| service.create(request, true)).await
    }
    pub async fn update(request: UpdateScheduleDto) -> Result<ScheduleDto, String> {
        operation(move |service| service.update(request)).await
    }
    pub async fn pause(key: ScheduleKeyDto, paused: bool) -> Result<ScheduleDto, String> {
        operation(move |service| service.pause(&key, paused)).await
    }
    pub async fn delete(key: ScheduleKeyDto) -> Result<ScheduleDto, String> {
        operation(move |service| service.delete(&key)).await
    }
    pub async fn targets(scope: ScheduleScopeDto) -> Result<Vec<ScheduleTargetDto>, String> {
        operation(move |service| {
            scope.resolve()?;
            Ok(service
                .read(None)?
                .targets
                .into_iter()
                .filter(|target| target.binding.scope == scope)
                .collect())
        })
        .await
    }
    pub async fn chat_options(
        provider: String,
        project_id: String,
    ) -> Result<ScheduleChatOptionsDto, String> {
        operation(move |service| {
            checked_id(&project_id)?;
            let mut options = ScheduleChatOptionsDto {
                provider,
                ..Default::default()
            };
            let (provider, _) = options.parsed()?;
            options.account_id = riwork_engine::chat::launch::account_for(
                &service.home,
                provider,
                Some(&project_id),
                None,
            )
            .map_err(|e| format!("binding_failed: {e}"))?;
            Ok(options)
        })
        .await
    }
    pub fn subscribe(
        listener: Callback<Snapshot<SchedulesDto>>,
        cx: CallCx<'_>,
    ) -> Result<crate::Subscription, String> {
        Ok(crate::Subscription::start(cx, async move {
            if let Ok(process) = process().await {
                if let Ok(subscription) = service::blocking(move || {
                    let subscription = process.hub.subscribe();
                    let _ = process.wake.try_send(());
                    Ok(subscription)
                })
                .await
                {
                    crate::deliver(subscription, listener).await;
                }
            }
        }))
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod fresh_dispatch_tests;
